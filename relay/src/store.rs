//! Where ciphertext goes.
//!
//! Two implementations: Postgres for production, in-memory for tests and for
//! `just run-relay`. Both are logs keyed by note ID, with a per-note monotonic
//! sequence number. They only grow, except where a member compacts one to a
//! base ([ADR 0024](../../docs/adr/0024-relay-storage-quota.md)), which drops
//! what the base makes redundant and leaves gaps behind it: a `since` cursor
//! means "after this", never "exactly this many".
//!
//! Note IDs are stored as the opaque 22-character base64url strings the client
//! sends. The relay never decodes them into anything meaningful, because there is
//! nothing meaningful in them.

use crate::error::{RelayError, Result};
use crate::media::{Geometry, MediaLimits, MediaRow, Reserved};
use crate::protocol::RequestState;
use crate::quota::{self, Limits, Proposal, Refusal, Topic};
use async_trait::async_trait;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

/// One stored envelope.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stored {
    pub seq: i64,
    pub envelope: Vec<u8>,
}

/// What became of an append.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Appended {
    /// Stored at `seq`. `crowded` when the note's log has grown enough that
    /// whoever appended should compact it.
    Stored { seq: i64, crowded: bool },
    /// The note's live log is at its cap. Nothing was stored.
    NoteFull,
    /// The relay is at its ceiling and this would have started a new note.
    RelayFull,
    /// The topic was purged. Nothing was stored, and nothing will be.
    Gone,
}

/// No limits at all: what [`RelayStore::append`] applies.
const UNLIMITED: Limits = Limits {
    note_bytes: u64::MAX,
    crowded_envelopes: u64::MAX,
    crowded_bytes: u64::MAX,
    ceiling: None,
};

#[async_trait]
pub trait RelayStore: Send + Sync + 'static {
    /// Append under `limits` ([`crate::quota`]). Sequence numbers start at 1
    /// and increase by one per note.
    async fn append_within(&self, note: &str, envelope: &[u8], limits: &Limits)
        -> Result<Appended>;

    /// Append with nothing limited, and return the sequence number assigned.
    async fn append(&self, note: &str, envelope: &[u8]) -> Result<i64> {
        match self.append_within(note, envelope, &UNLIMITED).await? {
            Appended::Stored { seq, .. } => Ok(seq),
            other => Err(RelayError::Storage(format!(
                "unlimited append refused: {other:?}"
            ))),
        }
    }

    /// Compact a note to a base, if [`quota::authorise`] allows it: drop every
    /// envelope at or below `covers` that is not a kept package, and store the
    /// base as a new envelope at the head. Returns the base's sequence number.
    ///
    /// With a live list, in the same transaction, the topic's committed
    /// objects are sorted by it (ADR 0027 §5): a listed one is no longer
    /// doomed; an unlisted one committed or asked about within
    /// `media.fresh_secs` of `now` is left alone; any other unlisted one is
    /// doomed at `now`, if it is not already.
    async fn compact(
        &self,
        note: &str,
        proposal: &Proposal<'_>,
        media: &MediaLimits,
        now: i64,
    ) -> Result<std::result::Result<i64, Refusal>>;

    /// A note's bookkeeping. The default for a note the store has never seen.
    async fn topic(&self, note: &str) -> Result<Topic>;

    /// Everything with `seq > since`, oldest first.
    async fn since(&self, note: &str, since: i64) -> Result<Vec<Stored>>;

    /// At most `limit` of what [`since`](Self::since) would return, oldest
    /// first. How a backlog is sent a piece at a time, so that one note's long
    /// history does not hold up every other note on the same socket.
    async fn page(&self, note: &str, since: i64, limit: usize) -> Result<Vec<Stored>>;

    /// The highest sequence number for a note, or 0 if it has none.
    async fn head(&self, note: &str) -> Result<i64>;

    /// Save a one-use auth nonce so any relay sharing this store can redeem it.
    async fn save_auth_challenge(
        &self,
        nonce: &[u8],
        device: &[u8; 32],
        expires_at: i64,
    ) -> Result<()>;

    /// Atomically consume an unexpired challenge. A replay or an expired nonce
    /// returns `None`.
    async fn consume_auth_challenge(&self, nonce: &[u8], now: i64) -> Result<Option<[u8; 32]>>;

    // -- invites ------------------------------------------------------------
    //
    // The store keeps invites and answers questions about them atomically; it
    // does not decide who may ask. Checking a token or an owner secret is
    // `crate::invite`'s job, done against the hashes `invite` hands back.
    // Times are Unix seconds, passed in rather than read here, so that "now" is
    // one value across a whole request.

    /// Open an invite. `false` if one with this ID already exists — sixteen
    /// random bytes colliding is a client bug or a replay, not bad luck. Sweeps
    /// invites long past expiry on the way, which is the only place anything
    /// does.
    async fn create_invite(&self, invite: NewInvite<'_>, now: i64) -> Result<bool>;

    /// An invite and every request on it, expired or not, until it is swept.
    async fn invite(&self, id: &str) -> Result<Option<Invite>>;

    /// Leave a request on an invite, if it is open, unexpired and nobody has
    /// asked yet. An invite admits one person, so it takes one request; the
    /// same request again is [`AddRequest::Again`], whatever has happened to
    /// the invite since, so that a guest whose first answer was lost can still
    /// find out how it went.
    async fn add_invite_request(
        &self,
        id: &str,
        request: &str,
        payload: &[u8],
        now: i64,
    ) -> Result<AddRequest>;

    /// Approve a pending request with `grant`, or decline it with `None`.
    ///
    /// Approving closes the invite. An invite holds one request, so there is
    /// nobody else to decline; were there, a second approval would hand the
    /// key to whoever the first one was not.
    async fn answer_invite_request(
        &self,
        id: &str,
        request: &str,
        grant: Option<&[u8]>,
        now: i64,
    ) -> Result<Answered>;

    /// Remove an invite and everything on it. Not an error if it is not there.
    async fn withdraw_invite(&self, id: &str) -> Result<()>;

    /// Delete every envelope of a topic and leave it gone, if `key` and
    /// `signature` are what [`quota::authorise_purge`] accepts. A topic the
    /// relay has never seen is left gone too: an owner's offline device must
    /// not be able to bring back a note that was deleted before it synced.
    async fn purge(
        &self,
        note: &str,
        key: [u8; 32],
        signature: [u8; easynotes_core::crypto::SIG_LEN],
    ) -> Result<std::result::Result<(), Refusal>>;

    // -- media (ADR 0027 §4) ------------------------------------------------
    //
    // The metadata of attachment objects; the bytes are `crate::media::Blobs`'.
    // Times are Unix seconds, passed in as for invites.

    /// Reserve an object's bytes against its topic's budget and the relay's,
    /// both counting reserved and committed objects, checked atomically with
    /// the insert.
    async fn reserve_media(
        &self,
        note: &str,
        object: &str,
        geometry: Geometry,
        limits: &MediaLimits,
        now: i64,
    ) -> Result<Reserved>;

    async fn media(&self, note: &str, object: &str) -> Result<Option<MediaRow>>;

    /// Something wants this object: a chunk arrived, its status was asked
    /// for, it was committed, or a doomed one was fetched. An upload is not
    /// abandoned, a committed object is fresh again, and a doomed one is not
    /// doomed any more.
    async fn touch_media(&self, note: &str, object: &str, now: i64) -> Result<()>;

    /// Record an object as committed under `digest`.
    async fn commit_media(&self, note: &str, object: &str, digest: [u8; 32]) -> Result<()>;

    /// Delete every uncommitted reservation last touched before `before`, and
    /// say which, so their bytes can go too.
    async fn expire_media(&self, before: i64) -> Result<Vec<(String, String)>>;

    /// Delete every object doomed before `before`, and say which, so their
    /// bytes can go too (ADR 0027 §5).
    async fn sweep_media(&self, before: i64) -> Result<Vec<(String, String)>>;

    /// How much the relay holds, for monitoring ([`crate::metrics`]). The
    /// same sums the ceilings are enforced on, so an alert on them fires
    /// before a refusal does.
    async fn usage(&self) -> Result<Usage>;

    /// Whether the store can answer a query right now. What `/v1/ready`
    /// reports: a relay whose database has gone away still accepts
    /// connections, and only this says it cannot keep what they send.
    async fn check(&self) -> Result<()> {
        Ok(())
    }
}

/// Totals across every note. See [`RelayStore::usage`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    /// Notes with a log, purged ones excluded.
    pub notes: u64,
    /// Live log bytes: what the storage ceiling is measured against.
    pub log_bytes: u64,
    /// Committed attachment objects, doomed ones included, and the sealed
    /// bytes of those that are not doomed.
    pub media_objects: u64,
    pub media_committed_bytes: u64,
    /// Bytes of committed objects no compaction lists any more, waiting to be
    /// deleted (ADR 0027 §5). Not counted against their note; counted against
    /// the media ceiling until they go.
    pub media_doomed_bytes: u64,
    /// Bytes reserved by uploads not yet committed. They count against the
    /// media ceiling as committed bytes do.
    pub media_reserved_bytes: u64,
}

pub struct NewInvite<'a> {
    pub id: &'a str,
    pub token_hash: &'a [u8],
    pub owner_hash: &'a [u8],
    pub expires_at: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Invite {
    pub token_hash: Vec<u8>,
    pub owner_hash: Vec<u8>,
    pub expires_at: i64,
    /// False once a request has been approved.
    pub open: bool,
    /// Oldest first.
    pub requests: Vec<InviteRequest>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InviteRequest {
    pub id: String,
    pub payload: Vec<u8>,
    pub state: RequestState,
    pub grant: Option<Vec<u8>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AddRequest {
    Added,
    /// This request is there already.
    Again,
    /// No such invite.
    Missing,
    /// Expired, answered, or somebody else has asked already.
    Closed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Answered {
    Done,
    /// No such invite or no such request on it.
    Missing,
    /// The request was answered already.
    NotPending,
    /// An approval on an invite that has expired or admitted someone already.
    Closed,
}

/// The one rule both stores apply, so that they cannot disagree about it.
fn swept(expires_at: i64, now: i64) -> bool {
    expires_at + crate::protocol::INVITE_GRACE_SECS < now
}

// ---------------------------------------------------------------------------
// Memory
// ---------------------------------------------------------------------------

/// Outstanding handshake nonces: the device each was issued to, and when it
/// stops being accepted.
type Challenges = HashMap<Vec<u8>, ([u8; 32], i64)>;

/// One stored envelope, and whether compaction must leave it.
#[derive(Clone)]
struct Row {
    stored: Stored,
    keep: bool,
}

#[derive(Default)]
struct Log {
    rows: Vec<Row>,
    topic: Topic,
}

/// One object's metadata, and when it last heard from its uploader.
#[derive(Clone)]
struct MediaMeta {
    row: MediaRow,
    touched: i64,
    /// When no compaction listed it any more; `row.doomed` is read off this.
    doomed_at: Option<i64>,
}

#[derive(Default, Clone)]
pub struct MemoryStore {
    notes: Arc<Mutex<HashMap<String, Log>>>,
    media: Arc<Mutex<HashMap<(String, String), MediaMeta>>>,
    invites: Arc<Mutex<HashMap<String, Invite>>>,
    auth_challenges: Arc<Mutex<Challenges>>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Everything the store holds, for the no-plaintext test to rummage through.
    pub fn all_envelopes(&self) -> Vec<Vec<u8>> {
        self.notes
            .lock()
            .unwrap()
            .values()
            .flat_map(|log| log.rows.iter().map(|r| r.stored.envelope.clone()))
            .collect()
    }

    pub fn note_ids(&self) -> Vec<String> {
        self.notes.lock().unwrap().keys().cloned().collect()
    }

    /// Every byte the store holds about invites — IDs, hashes, requests and
    /// grants — for the tests that check none of it names anybody.
    pub fn all_invite_bytes(&self) -> Vec<Vec<u8>> {
        let invites = self.invites.lock().unwrap();
        let mut out = Vec::new();
        for (id, invite) in invites.iter() {
            out.push(id.as_bytes().to_vec());
            out.push(invite.token_hash.clone());
            out.push(invite.owner_hash.clone());
            for request in &invite.requests {
                out.push(request.id.as_bytes().to_vec());
                out.push(request.payload.clone());
                out.extend(request.grant.clone());
            }
        }
        out
    }
}

#[async_trait]
impl RelayStore for MemoryStore {
    async fn append_within(
        &self,
        note: &str,
        envelope: &[u8],
        limits: &Limits,
    ) -> Result<Appended> {
        let mut notes = self.notes.lock().unwrap();
        if !notes.contains_key(note) {
            if let Some(ceiling) = limits.ceiling {
                let total: u64 = notes.values().map(|log| log.topic.live_bytes).sum();
                if total >= ceiling {
                    return Ok(Appended::RelayFull);
                }
            }
        }
        let log = notes.entry(note.to_string()).or_default();
        if log.topic.gone {
            return Ok(Appended::Gone);
        }
        let class = quota::classify(&log.topic, note, envelope);
        let bytes = envelope.len() as u64;
        // An owner's package goes whatever the cap says: it is how a person is
        // removed, and removing somebody must not wait on a full note.
        if !class.keep && log.topic.live_bytes + bytes > limits.note_bytes {
            return Ok(Appended::NoteFull);
        }
        let topic = &mut log.topic;
        topic.head += 1;
        topic.live_envelopes += 1;
        topic.live_bytes += bytes;
        if !class.keep {
            topic.loose_envelopes += 1;
            topic.loose_bytes += bytes;
        }
        if let Some(owner) = class.owner {
            topic.owner = Some(owner);
        }
        if let Some(current) = class.current {
            topic.current = Some(current);
        }
        let seq = topic.head;
        let crowded = topic.crowded(limits);
        log.rows.push(Row {
            stored: Stored {
                seq,
                envelope: envelope.to_vec(),
            },
            keep: class.keep,
        });
        Ok(Appended::Stored { seq, crowded })
    }

    async fn compact(
        &self,
        note: &str,
        proposal: &Proposal<'_>,
        limits: &MediaLimits,
        now: i64,
    ) -> Result<std::result::Result<i64, Refusal>> {
        let mut notes = self.notes.lock().unwrap();
        let Some(log) = notes.get_mut(note) else {
            return Ok(Err(Refusal::Stale));
        };
        let register = match quota::authorise(&log.topic, note, proposal) {
            Ok(register) => register,
            Err(refusal) => return Ok(Err(refusal)),
        };
        log.rows
            .retain(|row| row.keep || row.stored.seq > proposal.covers);
        let topic = &mut log.topic;
        topic.head += 1;
        topic.covers = proposal.covers;
        if register {
            topic.registered = Some(proposal.key);
        }
        let seq = topic.head;
        log.rows.push(Row {
            stored: Stored {
                seq,
                envelope: proposal.base.to_vec(),
            },
            keep: false,
        });
        let topic = &mut log.topic;
        topic.live_envelopes = log.rows.len() as u64;
        topic.live_bytes = log
            .rows
            .iter()
            .map(|r| r.stored.envelope.len() as u64)
            .sum();
        let loose = log.rows.iter().filter(|r| !r.keep);
        topic.loose_envelopes = loose.clone().count() as u64;
        topic.loose_bytes = loose.map(|r| r.stored.envelope.len() as u64).sum();
        // Under the notes lock still, which a reservation takes first too.
        if let Some(live) = proposal.media {
            let live: HashSet<String> = live.iter().map(|o| o.encode()).collect();
            let fresh = now - limits.fresh_secs;
            for ((n, object), m) in self.media.lock().unwrap().iter_mut() {
                if n != note || m.row.committed.is_none() {
                    continue;
                }
                if live.contains(object) {
                    m.doomed_at = None;
                } else if m.touched < fresh {
                    m.doomed_at.get_or_insert(now);
                }
            }
        }
        Ok(Ok(seq))
    }

    async fn purge(
        &self,
        note: &str,
        key: [u8; 32],
        signature: [u8; easynotes_core::crypto::SIG_LEN],
    ) -> Result<std::result::Result<(), Refusal>> {
        let mut notes = self.notes.lock().unwrap();
        let log = notes.entry(note.to_string()).or_default();
        if let Err(refusal) = quota::authorise_purge(&log.topic, note, key, signature) {
            return Ok(Err(refusal));
        }
        *log = Log {
            rows: Vec::new(),
            topic: gone(log.topic.head),
        };
        // Under the notes lock, which a reservation takes first too, so none
        // lands between the flag and this.
        self.media.lock().unwrap().retain(|(n, _), _| n != note);
        Ok(Ok(()))
    }

    async fn reserve_media(
        &self,
        note: &str,
        object: &str,
        geometry: Geometry,
        limits: &MediaLimits,
        now: i64,
    ) -> Result<Reserved> {
        let notes = self.notes.lock().unwrap();
        if notes.get(note).is_some_and(|log| log.topic.gone) {
            return Ok(Reserved::Gone);
        }
        let mut media = self.media.lock().unwrap();
        if let Some(existing) = media.get(&(note.to_string(), object.to_string())) {
            return Ok(if existing.row.geometry == geometry {
                Reserved::Same
            } else {
                Reserved::Conflict
            });
        }
        let in_note: u64 = media
            .iter()
            .filter(|((n, _), m)| n == note && m.doomed_at.is_none())
            .map(|(_, m)| m.row.geometry.sealed_len)
            .sum();
        if in_note + geometry.sealed_len > limits.note_bytes {
            return Ok(Reserved::NoteFull);
        }
        if let Some(ceiling) = limits.ceiling {
            let total: u64 = media.values().map(|m| m.row.geometry.sealed_len).sum();
            if total + geometry.sealed_len > ceiling {
                return Ok(Reserved::RelayFull);
            }
        }
        media.insert(
            (note.to_string(), object.to_string()),
            MediaMeta {
                row: MediaRow {
                    geometry,
                    committed: None,
                    doomed: false,
                },
                touched: now,
                doomed_at: None,
            },
        );
        Ok(Reserved::New)
    }

    async fn media(&self, note: &str, object: &str) -> Result<Option<MediaRow>> {
        let media = self.media.lock().unwrap();
        Ok(media
            .get(&(note.to_string(), object.to_string()))
            .map(|m| MediaRow {
                doomed: m.doomed_at.is_some(),
                ..m.row.clone()
            }))
    }

    async fn touch_media(&self, note: &str, object: &str, now: i64) -> Result<()> {
        if let Some(m) = self
            .media
            .lock()
            .unwrap()
            .get_mut(&(note.to_string(), object.to_string()))
        {
            m.touched = now;
            m.doomed_at = None;
        }
        Ok(())
    }

    async fn commit_media(&self, note: &str, object: &str, digest: [u8; 32]) -> Result<()> {
        if let Some(m) = self
            .media
            .lock()
            .unwrap()
            .get_mut(&(note.to_string(), object.to_string()))
        {
            m.row.committed.get_or_insert(digest);
        }
        Ok(())
    }

    async fn expire_media(&self, before: i64) -> Result<Vec<(String, String)>> {
        let mut media = self.media.lock().unwrap();
        let expired: Vec<(String, String)> = media
            .iter()
            .filter(|(_, m)| m.row.committed.is_none() && m.touched < before)
            .map(|(k, _)| k.clone())
            .collect();
        for key in &expired {
            media.remove(key);
        }
        Ok(expired)
    }

    async fn sweep_media(&self, before: i64) -> Result<Vec<(String, String)>> {
        let mut media = self.media.lock().unwrap();
        let swept: Vec<(String, String)> = media
            .iter()
            .filter(|(_, m)| m.doomed_at.is_some_and(|at| at < before))
            .map(|(k, _)| k.clone())
            .collect();
        for key in &swept {
            media.remove(key);
        }
        Ok(swept)
    }

    async fn usage(&self) -> Result<Usage> {
        let mut usage = Usage::default();
        for log in self.notes.lock().unwrap().values() {
            if !log.topic.gone {
                usage.notes += 1;
            }
            usage.log_bytes += log.topic.live_bytes;
        }
        for m in self.media.lock().unwrap().values() {
            if m.row.committed.is_some() {
                usage.media_objects += 1;
                if m.doomed_at.is_some() {
                    usage.media_doomed_bytes += m.row.geometry.sealed_len;
                } else {
                    usage.media_committed_bytes += m.row.geometry.sealed_len;
                }
            } else {
                usage.media_reserved_bytes += m.row.geometry.sealed_len;
            }
        }
        Ok(usage)
    }

    async fn topic(&self, note: &str) -> Result<Topic> {
        let notes = self.notes.lock().unwrap();
        Ok(notes
            .get(note)
            .map(|log| log.topic.clone())
            .unwrap_or_default())
    }

    async fn since(&self, note: &str, since: i64) -> Result<Vec<Stored>> {
        self.page(note, since, usize::MAX).await
    }

    async fn page(&self, note: &str, since: i64, limit: usize) -> Result<Vec<Stored>> {
        let notes = self.notes.lock().unwrap();
        Ok(notes
            .get(note)
            .map(|log| {
                log.rows
                    .iter()
                    .filter(|r| r.stored.seq > since)
                    .take(limit)
                    .map(|r| r.stored.clone())
                    .collect()
            })
            .unwrap_or_default())
    }

    async fn head(&self, note: &str) -> Result<i64> {
        let notes = self.notes.lock().unwrap();
        Ok(notes.get(note).map_or(0, |log| log.topic.head))
    }

    async fn save_auth_challenge(
        &self,
        nonce: &[u8],
        device: &[u8; 32],
        expires_at: i64,
    ) -> Result<()> {
        let mut challenges = self.auth_challenges.lock().unwrap();
        challenges.retain(|_, (_, expiry)| *expiry > crate::auth::unix_now() as i64);
        challenges.insert(nonce.to_vec(), (*device, expires_at));
        Ok(())
    }

    async fn consume_auth_challenge(&self, nonce: &[u8], now: i64) -> Result<Option<[u8; 32]>> {
        let mut challenges = self.auth_challenges.lock().unwrap();
        Ok(challenges
            .remove(nonce)
            .and_then(|(device, expires_at)| (expires_at > now).then_some(device)))
    }

    async fn create_invite(&self, invite: NewInvite<'_>, now: i64) -> Result<bool> {
        let mut invites = self.invites.lock().unwrap();
        invites.retain(|_, i| !swept(i.expires_at, now));
        if invites.contains_key(invite.id) {
            return Ok(false);
        }
        invites.insert(
            invite.id.to_string(),
            Invite {
                token_hash: invite.token_hash.to_vec(),
                owner_hash: invite.owner_hash.to_vec(),
                expires_at: invite.expires_at,
                open: true,
                requests: Vec::new(),
            },
        );
        Ok(true)
    }

    async fn invite(&self, id: &str) -> Result<Option<Invite>> {
        Ok(self.invites.lock().unwrap().get(id).cloned())
    }

    async fn add_invite_request(
        &self,
        id: &str,
        request: &str,
        payload: &[u8],
        now: i64,
    ) -> Result<AddRequest> {
        let mut invites = self.invites.lock().unwrap();
        let Some(invite) = invites.get_mut(id) else {
            return Ok(AddRequest::Missing);
        };
        if invite.requests.iter().any(|r| r.id == request) {
            return Ok(AddRequest::Again);
        }
        if !invite.open || invite.expires_at <= now || !invite.requests.is_empty() {
            return Ok(AddRequest::Closed);
        }
        invite.requests.push(InviteRequest {
            id: request.to_string(),
            payload: payload.to_vec(),
            state: RequestState::Pending,
            grant: None,
        });
        Ok(AddRequest::Added)
    }

    async fn answer_invite_request(
        &self,
        id: &str,
        request: &str,
        grant: Option<&[u8]>,
        now: i64,
    ) -> Result<Answered> {
        let mut invites = self.invites.lock().unwrap();
        let Some(invite) = invites.get_mut(id) else {
            return Ok(Answered::Missing);
        };
        let Some(at) = invite.requests.iter().position(|r| r.id == request) else {
            return Ok(Answered::Missing);
        };
        if invite.requests[at].state != RequestState::Pending {
            return Ok(Answered::NotPending);
        }
        match grant {
            None => invite.requests[at].state = RequestState::Declined,
            Some(grant) => {
                if !invite.open || invite.expires_at <= now {
                    return Ok(Answered::Closed);
                }
                invite.open = false;
                for (i, r) in invite.requests.iter_mut().enumerate() {
                    if i == at {
                        r.state = RequestState::Approved;
                        r.grant = Some(grant.to_vec());
                    } else if r.state == RequestState::Pending {
                        r.state = RequestState::Declined;
                    }
                }
            }
        }
        Ok(Answered::Done)
    }

    async fn withdraw_invite(&self, id: &str) -> Result<()> {
        self.invites.lock().unwrap().remove(id);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Postgres
// ---------------------------------------------------------------------------

pub struct PostgresStore {
    pool: sqlx::PgPool,
}

/// Queries are written out rather than using `sqlx::query!`, so the relay builds
/// without a live database to check macros against.
impl PostgresStore {
    pub async fn connect(url: &str) -> Result<Self> {
        let pool = sqlx::PgPool::connect(url)
            .await
            .map_err(|e| RelayError::Storage(e.to_string()))?;
        let store = Self { pool };
        store.migrate().await?;
        Ok(store)
    }

    /// Bring the schema up to this build's: every migration in
    /// [`MIGRATIONS`] that the database has not recorded, in order.
    ///
    /// Held under an advisory lock, because every relay instance runs this at
    /// startup, so "several at once" is the normal case on a deploy. (It was
    /// also needed before there were numbered migrations: `CREATE TABLE IF NOT
    /// EXISTS` is *not* safe to run concurrently in Postgres, and two sessions
    /// racing it collide on the system catalog.) Each migration runs in its own
    /// transaction with the row that records it, so one that fails leaves the
    /// database as the previous one left it.
    ///
    /// A database that has recorded migrations this build does not know is
    /// from a newer release. It is used as it is, not refused: migrations are
    /// expand-then-contract ([ADR
    /// 0029](../../docs/adr/0029-compatibility-across-releases.md) §5), so the
    /// release before a change runs on the schema after it, which is what a
    /// rolling deploy and a rollback both need.
    pub async fn migrate(&self) -> Result<()> {
        // An arbitrary constant that only this migration uses.
        const MIGRATION_LOCK: i64 = 0x0000_EA51_0DE5;

        let mut conn = self
            .pool
            .acquire()
            .await
            .map_err(|e| RelayError::Storage(e.to_string()))?;

        sqlx::query("SELECT pg_advisory_lock($1)")
            .bind(MIGRATION_LOCK)
            .execute(&mut *conn)
            .await
            .map_err(|e| RelayError::Storage(e.to_string()))?;

        let migrated = self.apply_migrations(&mut conn).await;

        // Unlock before propagating any failure: an advisory lock is
        // session-scoped, and this connection is about to go back into the pool.
        let unlocked = sqlx::query("SELECT pg_advisory_unlock($1)")
            .bind(MIGRATION_LOCK)
            .execute(&mut *conn)
            .await;

        migrated.map_err(|e| RelayError::Storage(e.to_string()))?;
        unlocked.map_err(|e| RelayError::Storage(e.to_string()))?;
        Ok(())
    }

    /// The body of [`Self::migrate`], under its lock.
    async fn apply_migrations(&self, conn: &mut sqlx::PgConnection) -> sqlx::Result<()> {
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS schema_migrations (
                version    INTEGER PRIMARY KEY,
                name       TEXT    NOT NULL,
                applied_at TIMESTAMPTZ NOT NULL DEFAULT now()
            )
            "#,
        )
        .execute(&mut *conn)
        .await?;
        let applied: Option<i32> = sqlx::query_scalar("SELECT max(version) FROM schema_migrations")
            .fetch_one(&mut *conn)
            .await?;
        let applied = applied.unwrap_or(0);
        let known = MIGRATIONS.last().map_or(0, |m| m.version);
        if applied > known {
            tracing::warn!(
                applied,
                known,
                "the database has migrations from a newer relay; running on it as it is"
            );
        }
        for migration in MIGRATIONS.iter().filter(|m| m.version > applied) {
            // Its own connection, so the lock's session is not also
            // the one a failed migration aborts. The lock still orders
            // every instance: they are all waiting on it, not on this.
            let mut tx = self.pool.begin().await?;
            sqlx::raw_sql(migration.sql).execute(&mut *tx).await?;
            sqlx::query("INSERT INTO schema_migrations (version, name) VALUES ($1, $2)")
                .bind(migration.version)
                .bind(migration.name)
                .execute(&mut *tx)
                .await?;
            tx.commit().await?;
            tracing::info!(
                version = migration.version,
                name = migration.name,
                "applied a schema migration"
            );
        }
        Ok(())
    }

    /// The highest migration this database has recorded.
    pub async fn schema_version(&self) -> Result<i32> {
        let version: Option<i32> = sqlx::query_scalar("SELECT max(version) FROM schema_migrations")
            .fetch_one(&self.pool)
            .await
            .map_err(pg)?;
        Ok(version.unwrap_or(0))
    }
}

/// One step of the relay's schema, from `relay/migrations/`.
pub struct Migration {
    pub version: i32,
    pub name: &'static str,
    pub sql: &'static str,
}

/// Every migration, in order, numbered from 1 with no gaps
/// (`migrations_are_numbered_densely` checks). Forward only, and never edited
/// once a release has shipped it: a change is a new file here and a new entry
/// below.
///
/// **Expand, then contract.** Several relay instances can run at once (ADR
/// 0021), and the previous release must keep working on this schema until the
/// last of its instances stops. So a migration may add a table, a column with
/// a default, or an index; only a later release, once no instance of the one
/// that used it can still be running, may drop or repurpose one.
pub const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "initial",
        sql: include_str!("../migrations/0001_initial.sql"),
    },
    Migration {
        version: 2,
        name: "media_doomed",
        sql: include_str!("../migrations/0002_media_doomed.sql"),
    },
];

#[async_trait]
impl RelayStore for PostgresStore {
    async fn check(&self) -> Result<()> {
        sqlx::query("SELECT 1")
            .execute(&self.pool)
            .await
            .map(|_| ())
            .map_err(pg)
    }

    async fn append_within(
        &self,
        note: &str,
        envelope: &[u8],
        limits: &Limits,
    ) -> Result<Appended> {
        let mut tx = self.pool.begin().await.map_err(pg)?;

        // The topic's row, locked. Allocating the number and writing the row
        // are one transaction under that lock, so concurrent appenders queue
        // rather than collide, and a crash between the two cannot burn a
        // sequence number.
        //
        // The obvious `SELECT MAX(seq) + 1` does not work here: readers do not
        // block each other, so sixteen concurrent appends all compute the same
        // number and fifteen of them lose on the primary key.
        let mut topic = match lock_topic(&mut tx, note).await? {
            Some(topic) => topic,
            None => {
                if let Some(ceiling) = limits.ceiling {
                    let (total,): (i64,) = sqlx::query_as(
                        "SELECT COALESCE(sum(live_bytes), 0)::BIGINT FROM note_head",
                    )
                    .fetch_one(&mut *tx)
                    .await
                    .map_err(pg)?;
                    if total as u64 >= ceiling {
                        return Ok(Appended::RelayFull);
                    }
                }
                sqlx::query(
                    "INSERT INTO note_head (note_id, seq) VALUES ($1, 0) ON CONFLICT (note_id) DO NOTHING",
                )
                .bind(note)
                .execute(&mut *tx)
                .await
                .map_err(pg)?;
                lock_topic(&mut tx, note)
                    .await?
                    .ok_or_else(|| RelayError::Storage("topic row vanished".into()))?
            }
        };

        if topic.gone {
            return Ok(Appended::Gone);
        }
        let class = quota::classify(&topic, note, envelope);
        let bytes = envelope.len() as u64;
        if !class.keep && topic.live_bytes + bytes > limits.note_bytes {
            return Ok(Appended::NoteFull);
        }
        topic.head += 1;
        topic.live_envelopes += 1;
        topic.live_bytes += bytes;
        if !class.keep {
            topic.loose_envelopes += 1;
            topic.loose_bytes += bytes;
        }
        if let Some(owner) = class.owner {
            topic.owner = Some(owner);
        }
        if let Some(current) = class.current {
            topic.current = Some(current);
        }
        save_topic(&mut tx, note, &topic).await?;

        sqlx::query("INSERT INTO note_log (note_id, seq, envelope, keep) VALUES ($1, $2, $3, $4)")
            .bind(note)
            .bind(topic.head)
            .bind(envelope)
            .bind(class.keep)
            .execute(&mut *tx)
            .await
            .map_err(pg)?;

        tx.commit().await.map_err(pg)?;
        Ok(Appended::Stored {
            seq: topic.head,
            crowded: topic.crowded(limits),
        })
    }

    async fn compact(
        &self,
        note: &str,
        proposal: &Proposal<'_>,
        limits: &MediaLimits,
        now: i64,
    ) -> Result<std::result::Result<i64, Refusal>> {
        let mut tx = self.pool.begin().await.map_err(pg)?;
        let Some(mut topic) = lock_topic(&mut tx, note).await? else {
            return Ok(Err(Refusal::Stale));
        };
        let register = match quota::authorise(&topic, note, proposal) {
            Ok(register) => register,
            Err(refusal) => return Ok(Err(refusal)),
        };
        sqlx::query("DELETE FROM note_log WHERE note_id = $1 AND seq <= $2 AND NOT keep")
            .bind(note)
            .bind(proposal.covers)
            .execute(&mut *tx)
            .await
            .map_err(pg)?;
        topic.head += 1;
        topic.covers = proposal.covers;
        if register {
            topic.registered = Some(proposal.key);
        }
        sqlx::query(
            "INSERT INTO note_log (note_id, seq, envelope, keep) VALUES ($1, $2, $3, FALSE)",
        )
        .bind(note)
        .bind(topic.head)
        .bind(proposal.base)
        .execute(&mut *tx)
        .await
        .map_err(pg)?;
        let (live, live_bytes, loose, loose_bytes): (i64, i64, i64, i64) = sqlx::query_as(
            r#"
            SELECT count(*),
                   COALESCE(sum(length(envelope)), 0)::BIGINT,
                   count(*) FILTER (WHERE NOT keep),
                   COALESCE(sum(length(envelope)) FILTER (WHERE NOT keep), 0)::BIGINT
            FROM note_log WHERE note_id = $1
            "#,
        )
        .bind(note)
        .fetch_one(&mut *tx)
        .await
        .map_err(pg)?;
        topic.live_envelopes = live as u64;
        topic.live_bytes = live_bytes as u64;
        topic.loose_envelopes = loose as u64;
        topic.loose_bytes = loose_bytes as u64;
        save_topic(&mut tx, note, &topic).await?;
        if let Some(live) = proposal.media {
            let live: Vec<String> = live.iter().map(|o| o.encode()).collect();
            sqlx::query(
                "UPDATE media_objects SET doomed_at = NULL \
                 WHERE note_id = $1 AND object_id = ANY($2) AND doomed_at IS NOT NULL",
            )
            .bind(note)
            .bind(&live)
            .execute(&mut *tx)
            .await
            .map_err(pg)?;
            sqlx::query(
                "UPDATE media_objects SET doomed_at = $3 \
                 WHERE note_id = $1 AND NOT (object_id = ANY($2)) \
                   AND digest IS NOT NULL AND doomed_at IS NULL AND touched_at < $4",
            )
            .bind(note)
            .bind(&live)
            .bind(now)
            .bind(now - limits.fresh_secs)
            .execute(&mut *tx)
            .await
            .map_err(pg)?;
        }
        tx.commit().await.map_err(pg)?;
        Ok(Ok(topic.head))
    }

    async fn purge(
        &self,
        note: &str,
        key: [u8; 32],
        signature: [u8; easynotes_core::crypto::SIG_LEN],
    ) -> Result<std::result::Result<(), Refusal>> {
        let mut tx = self.pool.begin().await.map_err(pg)?;
        // The row first, so the purge holds the same lock an append does: an
        // append queued behind it finds the topic gone rather than landing in
        // the gap between the delete and the flag.
        sqlx::query(
            "INSERT INTO note_head (note_id, seq) VALUES ($1, 0) ON CONFLICT (note_id) DO NOTHING",
        )
        .bind(note)
        .execute(&mut *tx)
        .await
        .map_err(pg)?;
        let topic = lock_topic(&mut tx, note)
            .await?
            .ok_or_else(|| RelayError::Storage("topic row vanished".into()))?;
        if let Err(refusal) = quota::authorise_purge(&topic, note, key, signature) {
            return Ok(Err(refusal));
        }
        sqlx::query("DELETE FROM note_log WHERE note_id = $1")
            .bind(note)
            .execute(&mut *tx)
            .await
            .map_err(pg)?;
        sqlx::query("DELETE FROM media_objects WHERE note_id = $1")
            .bind(note)
            .execute(&mut *tx)
            .await
            .map_err(pg)?;
        save_topic(&mut tx, note, &gone(topic.head)).await?;
        tx.commit().await.map_err(pg)?;
        Ok(Ok(()))
    }

    async fn usage(&self) -> Result<Usage> {
        // One statement, so the numbers are from one snapshot.
        let (notes, log_bytes, objects, committed, doomed, reserved): (i64, i64, i64, i64, i64, i64) =
            sqlx::query_as(
                "SELECT \
                   (SELECT count(*) FROM note_head WHERE NOT gone), \
                   (SELECT COALESCE(sum(live_bytes), 0)::BIGINT FROM note_head), \
                   (SELECT count(*) FROM media_objects WHERE digest IS NOT NULL), \
                   (SELECT COALESCE(sum(sealed_len), 0)::BIGINT FROM media_objects \
                      WHERE digest IS NOT NULL AND doomed_at IS NULL), \
                   (SELECT COALESCE(sum(sealed_len), 0)::BIGINT FROM media_objects \
                      WHERE doomed_at IS NOT NULL), \
                   (SELECT COALESCE(sum(sealed_len), 0)::BIGINT FROM media_objects WHERE digest IS NULL)",
            )
            .fetch_one(&self.pool)
            .await
            .map_err(pg)?;
        Ok(Usage {
            notes: notes as u64,
            log_bytes: log_bytes as u64,
            media_objects: objects as u64,
            media_committed_bytes: committed as u64,
            media_doomed_bytes: doomed as u64,
            media_reserved_bytes: reserved as u64,
        })
    }

    async fn topic(&self, note: &str) -> Result<Topic> {
        let mut tx = self.pool.begin().await.map_err(pg)?;
        let topic = read_topic(&mut tx, note, false).await?;
        tx.commit().await.map_err(pg)?;
        Ok(topic.unwrap_or_default())
    }

    async fn since(&self, note: &str, since: i64) -> Result<Vec<Stored>> {
        let rows: Vec<(i64, Vec<u8>)> = sqlx::query_as(
            "SELECT seq, envelope FROM note_log WHERE note_id = $1 AND seq > $2 ORDER BY seq",
        )
        .bind(note)
        .bind(since)
        .fetch_all(&self.pool)
        .await
        .map_err(pg)?;
        Ok(rows
            .into_iter()
            .map(|(seq, envelope)| Stored { seq, envelope })
            .collect())
    }

    async fn page(&self, note: &str, since: i64, limit: usize) -> Result<Vec<Stored>> {
        let rows: Vec<(i64, Vec<u8>)> = sqlx::query_as(
            "SELECT seq, envelope FROM note_log WHERE note_id = $1 AND seq > $2 ORDER BY seq LIMIT $3",
        )
        .bind(note)
        .bind(since)
        .bind(i64::try_from(limit).unwrap_or(i64::MAX))
        .fetch_all(&self.pool)
        .await
        .map_err(pg)?;
        Ok(rows
            .into_iter()
            .map(|(seq, envelope)| Stored { seq, envelope })
            .collect())
    }

    async fn head(&self, note: &str) -> Result<i64> {
        let row: Option<(i64,)> = sqlx::query_as("SELECT seq FROM note_head WHERE note_id = $1")
            .bind(note)
            .fetch_optional(&self.pool)
            .await
            .map_err(pg)?;
        Ok(row.map_or(0, |r| r.0))
    }

    async fn save_auth_challenge(
        &self,
        nonce: &[u8],
        device: &[u8; 32],
        expires_at: i64,
    ) -> Result<()> {
        sqlx::query("DELETE FROM auth_challenges WHERE expires_at <= $1")
            .bind(crate::auth::unix_now() as i64)
            .execute(&self.pool)
            .await
            .map_err(|e| RelayError::Storage(e.to_string()))?;
        sqlx::query("INSERT INTO auth_challenges (nonce, device, expires_at) VALUES ($1, $2, $3)")
            .bind(nonce)
            .bind(device.as_slice())
            .bind(expires_at)
            .execute(&self.pool)
            .await
            .map_err(|e| RelayError::Storage(e.to_string()))?;
        Ok(())
    }

    async fn consume_auth_challenge(&self, nonce: &[u8], now: i64) -> Result<Option<[u8; 32]>> {
        let device: Option<Vec<u8>> = sqlx::query_scalar(
            "DELETE FROM auth_challenges WHERE nonce = $1 AND expires_at > $2 RETURNING device",
        )
        .bind(nonce)
        .bind(now)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| RelayError::Storage(e.to_string()))?;
        device
            .map(|bytes| {
                bytes.try_into().map_err(|_| {
                    RelayError::Storage("stored auth device key is not 32 bytes".into())
                })
            })
            .transpose()
    }

    async fn create_invite(&self, invite: NewInvite<'_>, now: i64) -> Result<bool> {
        sqlx::query("DELETE FROM invites WHERE expires_at + $1 < $2")
            .bind(crate::protocol::INVITE_GRACE_SECS)
            .bind(now)
            .execute(&self.pool)
            .await
            .map_err(pg)?;
        let inserted = sqlx::query(
            "INSERT INTO invites (invite_id, token_hash, owner_hash, expires_at) \
             VALUES ($1, $2, $3, $4) ON CONFLICT (invite_id) DO NOTHING",
        )
        .bind(invite.id)
        .bind(invite.token_hash)
        .bind(invite.owner_hash)
        .bind(invite.expires_at)
        .execute(&self.pool)
        .await
        .map_err(pg)?;
        Ok(inserted.rows_affected() == 1)
    }

    async fn invite(&self, id: &str) -> Result<Option<Invite>> {
        let row: Option<(Vec<u8>, Vec<u8>, i64, bool)> = sqlx::query_as(
            "SELECT token_hash, owner_hash, expires_at, open FROM invites WHERE invite_id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(pg)?;
        let Some((token_hash, owner_hash, expires_at, open)) = row else {
            return Ok(None);
        };
        let rows: Vec<(String, Vec<u8>, i16, Option<Vec<u8>>)> = sqlx::query_as(
            "SELECT request_id, payload, state, grant_blob FROM invite_requests \
             WHERE invite_id = $1 ORDER BY created_at, request_id",
        )
        .bind(id)
        .fetch_all(&self.pool)
        .await
        .map_err(pg)?;
        let requests = rows
            .into_iter()
            .map(|(id, payload, state, grant)| {
                Ok(InviteRequest {
                    id,
                    payload,
                    state: state_from_db(state)?,
                    grant,
                })
            })
            .collect::<Result<_>>()?;
        Ok(Some(Invite {
            token_hash,
            owner_hash,
            expires_at,
            open,
            requests,
        }))
    }

    async fn add_invite_request(
        &self,
        id: &str,
        request: &str,
        payload: &[u8],
        now: i64,
    ) -> Result<AddRequest> {
        let mut tx = self.pool.begin().await.map_err(pg)?;
        // The row lock is what makes one request one: without it, two guests
        // arriving at once both find the invite empty and both get in.
        let row: Option<(bool, i64)> =
            sqlx::query_as("SELECT open, expires_at FROM invites WHERE invite_id = $1 FOR UPDATE")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(pg)?;
        let Some((open, expires_at)) = row else {
            return Ok(AddRequest::Missing);
        };
        let asked: Vec<(String,)> =
            sqlx::query_as("SELECT request_id FROM invite_requests WHERE invite_id = $1")
                .bind(id)
                .fetch_all(&mut *tx)
                .await
                .map_err(pg)?;
        if asked.iter().any(|(r,)| r == request) {
            return Ok(AddRequest::Again);
        }
        if !open || expires_at <= now || !asked.is_empty() {
            return Ok(AddRequest::Closed);
        }
        sqlx::query(
            "INSERT INTO invite_requests (invite_id, request_id, payload) VALUES ($1, $2, $3)",
        )
        .bind(id)
        .bind(request)
        .bind(payload)
        .execute(&mut *tx)
        .await
        .map_err(pg)?;
        tx.commit().await.map_err(pg)?;
        Ok(AddRequest::Added)
    }

    async fn answer_invite_request(
        &self,
        id: &str,
        request: &str,
        grant: Option<&[u8]>,
        now: i64,
    ) -> Result<Answered> {
        let mut tx = self.pool.begin().await.map_err(pg)?;
        // Locking the invite serialises every answer to it, which is what keeps
        // two approvals from both finding the invite open.
        let row: Option<(bool, i64)> =
            sqlx::query_as("SELECT open, expires_at FROM invites WHERE invite_id = $1 FOR UPDATE")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(pg)?;
        let Some((open, expires_at)) = row else {
            return Ok(Answered::Missing);
        };
        let state: Option<(i16,)> = sqlx::query_as(
            "SELECT state FROM invite_requests WHERE invite_id = $1 AND request_id = $2",
        )
        .bind(id)
        .bind(request)
        .fetch_optional(&mut *tx)
        .await
        .map_err(pg)?;
        let Some((state,)) = state else {
            return Ok(Answered::Missing);
        };
        if state_from_db(state)? != RequestState::Pending {
            return Ok(Answered::NotPending);
        }
        match grant {
            None => {
                sqlx::query(
                    "UPDATE invite_requests SET state = $3 WHERE invite_id = $1 AND request_id = $2",
                )
                .bind(id)
                .bind(request)
                .bind(state_to_db(RequestState::Declined))
                .execute(&mut *tx)
                .await
                .map_err(pg)?;
            }
            Some(grant) => {
                if !open || expires_at <= now {
                    return Ok(Answered::Closed);
                }
                sqlx::query("UPDATE invites SET open = FALSE WHERE invite_id = $1")
                    .bind(id)
                    .execute(&mut *tx)
                    .await
                    .map_err(pg)?;
                sqlx::query(
                    "UPDATE invite_requests SET state = $2 WHERE invite_id = $1 AND state = $3",
                )
                .bind(id)
                .bind(state_to_db(RequestState::Declined))
                .bind(state_to_db(RequestState::Pending))
                .execute(&mut *tx)
                .await
                .map_err(pg)?;
                sqlx::query(
                    "UPDATE invite_requests SET state = $3, grant_blob = $4 \
                     WHERE invite_id = $1 AND request_id = $2",
                )
                .bind(id)
                .bind(request)
                .bind(state_to_db(RequestState::Approved))
                .bind(grant)
                .execute(&mut *tx)
                .await
                .map_err(pg)?;
            }
        }
        tx.commit().await.map_err(pg)?;
        Ok(Answered::Done)
    }

    async fn reserve_media(
        &self,
        note: &str,
        object: &str,
        geometry: Geometry,
        limits: &MediaLimits,
        now: i64,
    ) -> Result<Reserved> {
        // One reservation at a time, relay-wide: the budgets are sums over
        // rows, and two reservations that each read the sum before the other
        // wrote would both fit. A reservation is one per attached file, so
        // this queue is short.
        const MEDIA_LOCK: i64 = 0x0000_EA51_0ED1;
        let mut tx = self.pool.begin().await.map_err(pg)?;
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(MEDIA_LOCK)
            .execute(&mut *tx)
            .await
            .map_err(pg)?;
        // Shared-locked, so a purge holding the row waits for this or this
        // waits for it, and a reservation cannot land on a topic being purged.
        let gone: Option<(bool,)> =
            sqlx::query_as("SELECT gone FROM note_head WHERE note_id = $1 FOR SHARE")
                .bind(note)
                .fetch_optional(&mut *tx)
                .await
                .map_err(pg)?;
        if gone.is_some_and(|(gone,)| gone) {
            return Ok(Reserved::Gone);
        }
        let existing: Option<(i64, i32)> = sqlx::query_as(
            "SELECT sealed_len, chunk_size FROM media_objects WHERE note_id = $1 AND object_id = $2",
        )
        .bind(note)
        .bind(object)
        .fetch_optional(&mut *tx)
        .await
        .map_err(pg)?;
        if let Some((sealed_len, chunk_size)) = existing {
            let same = sealed_len as u64 == geometry.sealed_len
                && chunk_size as u32 == geometry.chunk_size;
            return Ok(if same {
                Reserved::Same
            } else {
                Reserved::Conflict
            });
        }
        let (in_note,): (i64,) = sqlx::query_as(
            "SELECT COALESCE(sum(sealed_len), 0)::BIGINT FROM media_objects \
             WHERE note_id = $1 AND doomed_at IS NULL",
        )
        .bind(note)
        .fetch_one(&mut *tx)
        .await
        .map_err(pg)?;
        if in_note as u64 + geometry.sealed_len > limits.note_bytes {
            return Ok(Reserved::NoteFull);
        }
        if let Some(ceiling) = limits.ceiling {
            let (total,): (i64,) =
                sqlx::query_as("SELECT COALESCE(sum(sealed_len), 0)::BIGINT FROM media_objects")
                    .fetch_one(&mut *tx)
                    .await
                    .map_err(pg)?;
            if total as u64 + geometry.sealed_len > ceiling {
                return Ok(Reserved::RelayFull);
            }
        }
        sqlx::query(
            "INSERT INTO media_objects (note_id, object_id, sealed_len, chunk_size, touched_at) \
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(note)
        .bind(object)
        .bind(geometry.sealed_len as i64)
        .bind(geometry.chunk_size as i32)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(pg)?;
        tx.commit().await.map_err(pg)?;
        Ok(Reserved::New)
    }

    async fn media(&self, note: &str, object: &str) -> Result<Option<MediaRow>> {
        let row: Option<(i64, i32, Option<Vec<u8>>, bool)> = sqlx::query_as(
            "SELECT sealed_len, chunk_size, digest, doomed_at IS NOT NULL \
             FROM media_objects WHERE note_id = $1 AND object_id = $2",
        )
        .bind(note)
        .bind(object)
        .fetch_optional(&self.pool)
        .await
        .map_err(pg)?;
        row.map(|(sealed_len, chunk_size, digest, doomed)| {
            Ok(MediaRow {
                geometry: Geometry {
                    sealed_len: sealed_len as u64,
                    chunk_size: chunk_size as u32,
                },
                committed: digest.map(key32).transpose()?,
                doomed,
            })
        })
        .transpose()
    }

    async fn touch_media(&self, note: &str, object: &str, now: i64) -> Result<()> {
        sqlx::query(
            "UPDATE media_objects SET touched_at = $3, doomed_at = NULL \
             WHERE note_id = $1 AND object_id = $2",
        )
        .bind(note)
        .bind(object)
        .bind(now)
        .execute(&self.pool)
        .await
        .map_err(pg)?;
        Ok(())
    }

    async fn commit_media(&self, note: &str, object: &str, digest: [u8; 32]) -> Result<()> {
        sqlx::query(
            "UPDATE media_objects SET digest = $3 \
             WHERE note_id = $1 AND object_id = $2 AND digest IS NULL",
        )
        .bind(note)
        .bind(object)
        .bind(digest.to_vec())
        .execute(&self.pool)
        .await
        .map_err(pg)?;
        Ok(())
    }

    async fn expire_media(&self, before: i64) -> Result<Vec<(String, String)>> {
        sqlx::query_as(
            "DELETE FROM media_objects WHERE digest IS NULL AND touched_at < $1 \
             RETURNING note_id, object_id",
        )
        .bind(before)
        .fetch_all(&self.pool)
        .await
        .map_err(pg)
    }

    async fn sweep_media(&self, before: i64) -> Result<Vec<(String, String)>> {
        sqlx::query_as(
            "DELETE FROM media_objects WHERE doomed_at < $1 RETURNING note_id, object_id",
        )
        .bind(before)
        .fetch_all(&self.pool)
        .await
        .map_err(pg)
    }

    async fn withdraw_invite(&self, id: &str) -> Result<()> {
        sqlx::query("DELETE FROM invites WHERE invite_id = $1")
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(pg)?;
        Ok(())
    }
}

type TopicRow = (
    i64,
    i64,
    i64,
    i64,
    i64,
    Option<Vec<u8>>,
    Option<i64>,
    Option<Vec<u8>>,
    Option<Vec<u8>>,
    Option<Vec<u8>>,
    i64,
    bool,
);

/// A topic's row, locked for the rest of the transaction.
async fn lock_topic(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    note: &str,
) -> Result<Option<Topic>> {
    read_topic(tx, note, true).await
}

async fn read_topic(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    note: &str,
    lock: bool,
) -> Result<Option<Topic>> {
    let sql = format!(
        "SELECT seq, live_envelopes, live_bytes, loose_envelopes, loose_bytes, owner, \
         current_revision, current_digest, current_compaction, registered, covers, gone \
         FROM note_head WHERE note_id = $1{}",
        if lock { " FOR UPDATE" } else { "" }
    );
    let row: Option<TopicRow> = sqlx::query_as(&sql)
        .bind(note)
        .fetch_optional(&mut **tx)
        .await
        .map_err(pg)?;
    let Some((
        head,
        live,
        live_bytes,
        loose,
        loose_bytes,
        owner,
        revision,
        digest,
        compaction,
        registered,
        covers,
        gone,
    )) = row
    else {
        return Ok(None);
    };
    let current = match (revision, digest, compaction) {
        (Some(revision), Some(digest), Some(compaction)) => Some(quota::Current {
            revision: revision as u64,
            digest: key32(digest)?,
            compaction: key32(compaction)?,
        }),
        _ => None,
    };
    Ok(Some(Topic {
        head,
        live_envelopes: live as u64,
        live_bytes: live_bytes as u64,
        loose_envelopes: loose as u64,
        loose_bytes: loose_bytes as u64,
        owner: owner.map(key32).transpose()?,
        current,
        registered: registered.map(key32).transpose()?,
        covers,
        gone,
    }))
}

async fn save_topic(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    note: &str,
    topic: &Topic,
) -> Result<()> {
    sqlx::query(
        "UPDATE note_head SET seq = $2, live_envelopes = $3, live_bytes = $4, \
         loose_envelopes = $5, loose_bytes = $6, owner = $7, current_revision = $8, \
         current_digest = $9, current_compaction = $10, registered = $11, covers = $12, \
         gone = $13 WHERE note_id = $1",
    )
    .bind(note)
    .bind(topic.head)
    .bind(topic.live_envelopes as i64)
    .bind(topic.live_bytes as i64)
    .bind(topic.loose_envelopes as i64)
    .bind(topic.loose_bytes as i64)
    .bind(topic.owner.map(|k| k.to_vec()))
    .bind(topic.current.map(|c| c.revision as i64))
    .bind(topic.current.map(|c| c.digest.to_vec()))
    .bind(topic.current.map(|c| c.compaction.to_vec()))
    .bind(topic.registered.map(|k| k.to_vec()))
    .bind(topic.covers)
    .bind(topic.gone)
    .execute(&mut **tx)
    .await
    .map_err(pg)?;
    Ok(())
}

/// What a purged topic keeps: its head, so that a cursor on a device that
/// still holds it reads as behind rather than ahead, and the flag.
fn gone(head: i64) -> Topic {
    Topic {
        head,
        gone: true,
        ..Topic::default()
    }
}

fn key32(raw: Vec<u8>) -> Result<[u8; 32]> {
    raw.try_into()
        .map_err(|_| RelayError::Storage("a stored key is not 32 bytes".into()))
}

fn pg(e: sqlx::Error) -> RelayError {
    RelayError::Storage(e.to_string())
}

fn state_to_db(state: RequestState) -> i16 {
    match state {
        RequestState::Pending => 0,
        RequestState::Approved => 1,
        RequestState::Declined => 2,
    }
}

fn state_from_db(raw: i16) -> Result<RequestState> {
    match raw {
        0 => Ok(RequestState::Pending),
        1 => Ok(RequestState::Approved),
        2 => Ok(RequestState::Declined),
        other => Err(RelayError::Storage(format!(
            "unknown request state {other}"
        ))),
    }
}

#[cfg(test)]
mod migration_tests {
    use super::MIGRATIONS;

    /// A gap or a repeat would make "applied up to N" mean something other
    /// than "applied 1 to N".
    #[test]
    fn migrations_are_numbered_densely() {
        for (i, migration) in MIGRATIONS.iter().enumerate() {
            assert_eq!(migration.version, i as i32 + 1, "{}", migration.name);
        }
    }

    /// Every file in `relay/migrations/` is listed, under its own number, so a
    /// migration cannot be written and then left out of the build.
    #[test]
    fn every_migration_file_is_listed() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/migrations");
        let mut files: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .filter(|name| name.ends_with(".sql"))
            .collect();
        files.sort();
        let listed: Vec<String> = MIGRATIONS
            .iter()
            .map(|m| format!("{:04}_{}.sql", m.version, m.name))
            .collect();
        assert_eq!(files, listed);
    }
}
