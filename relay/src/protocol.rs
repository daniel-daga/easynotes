//! The relay's wire protocol.
//!
//! Everything here is deliberately small. The relay stores and forwards opaque
//! blobs; it has no idea what a note, an item or a checkbox is, and it must never
//! learn. The only fields it reads out of an envelope are the ones it needs to
//! route and to refuse abuse.

use serde::{Deserialize, Serialize};

/// Mirrors `easynotes_core::crypto::MAX_ENVELOPE_BYTES`; `caps_match_the_core`
/// in `tests/protocol.rs` fails if the two ever drift.
pub const MAX_ENVELOPE_BYTES: usize = 1 << 20;

/// The protocol versions this relay speaks ([ADR
/// 0029](../../docs/adr/0029-compatibility-across-releases.md) §4). A client
/// names its own in `?p=` on every request; one outside this range is refused
/// with `426` before anything else runs.
///
/// The order of a breaking change is fixed: a relay that raises `MAX` ships
/// first, clients that speak the new version follow, and `MIN` rises only when
/// no client released in the last twelve months still speaks the old one.
/// `easynotes_relay_protocol_requests_total` says who still does.
pub const PROTOCOL_MIN: u32 = 2;
pub const PROTOCOL_MAX: u32 = 2;

/// The body of a `426`: what the client needs to tell "update the app" from
/// "the relay is too old", which it works out from the range.
#[derive(Debug, Serialize, Deserialize)]
pub struct Outdated {
    pub code: String,
    pub protocol: ProtocolRange,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProtocolRange {
    pub min: u32,
    pub max: u32,
}

impl ProtocolRange {
    pub const SPOKEN: Self = Self {
        min: PROTOCOL_MIN,
        max: PROTOCOL_MAX,
    };
}

/// How long a challenge nonce stays redeemable.
pub const CHALLENGE_TTL_SECS: u64 = 60;

/// How long an issued token is good for.
pub const TOKEN_TTL_SECS: u64 = 15 * 60;

#[derive(Debug, Deserialize)]
pub struct ChallengeRequest {
    /// The device's Ed25519 public key, base64url, no padding.
    pub device: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ChallengeResponse {
    /// 32 random bytes, base64url.
    pub nonce: String,
    pub expires_in: u64,
}

#[derive(Debug, Deserialize)]
pub struct TokenRequest {
    pub device: String,
    pub nonce: String,
    /// Ed25519 signature over the fixed-size relay-auth transcript, base64url.
    pub signature: String,
    /// `easynotes_core::relay_access`'s token proof, base64url. Required on a
    /// gated relay, ignored on an open one.
    #[serde(default)]
    pub access: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TokenResponse {
    pub token: String,
    pub expires_in: u64,
}

// ---------------------------------------------------------------------------
// The device socket
// ---------------------------------------------------------------------------
//
// `/v1/sync` carries every note a device syncs over one socket, authenticated
// once, so every message names its note. A note can be subscribed at any point
// in the socket's life. Sequence numbers are per note and start at 1, so
// `since: 0` means from the beginning.

/// What an append is refused with when the note's live log is at its cap.
pub const NOTE_FULL: &str = "note_full";

/// What an append that would start a new note is refused with when the relay
/// is at its ceiling.
pub const RELAY_FULL: &str = "relay_full";

/// What an append, a compaction or a subscribe is refused with once the topic
/// has been purged, and what every subscriber is told at the moment it is
/// ([ADR 0026](../../docs/adr/0026-deleting-an-account.md)).
pub const GONE: &str = "gone";

/// What an append is refused with past the address's write budget ([ADR
/// 0028](../../docs/adr/0028-who-may-write-to-a-relay.md) §3). The relay closes
/// the socket after saying so, and the client backs off before it reconnects.
pub const RATE_LIMITED: &str = "rate_limited";

/// How many notes one socket may be subscribed to at once. A device's notes,
/// not an enumeration of the relay.
pub const MAX_SUBSCRIPTIONS: usize = 4096;

/// How many stored envelopes of one note's backlog go out before the relay
/// looks at anything else on the socket. Small enough that a guest joining a
/// list with months of history does not hold up live edits to every other note
/// behind it; large enough that catching up is not a round trip per envelope.
pub const BACKLOG_CHUNK: usize = 64;

/// Client to server, over `/v1/sync`.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SyncClientMessage {
    /// Start sending this note: everything after `since`, then live.
    Subscribe { note: String, since: i64 },
    /// Stop sending this note.
    Unsubscribe { note: String },
    /// An envelope to append to a note this socket is subscribed to.
    Append { note: String, envelope: String },
    /// Compact a subscribed note to a base: see [`crate::quota`] and
    /// [ADR 0024](../../docs/adr/0024-relay-storage-quota.md). All base64url.
    Compact {
        note: String,
        /// Everything at or below this sequence number is in the base.
        covers: i64,
        /// The base: an ordinary sealed update holding the whole document.
        base: String,
        /// The attachment objects the base names, as object IDs: the topic's
        /// other committed objects are doomed (ADR 0027 §5). Absent, nothing
        /// is; a client from before clean-up sends none.
        #[serde(default)]
        media: Option<Vec<String>>,
        /// The note's compaction key, 32 bytes.
        key: String,
        /// Its signature over `easynotes_core::compaction::transcript`, 64 bytes.
        signature: String,
    },
    /// Snapshot the current heads of this socket's subscriptions and mark each
    /// one complete after ordered delivery reaches that head.
    Refresh { id: String },
    /// A message type this relay does not know: something a newer client can
    /// do without. Skipped rather than refused, because a refusal names no
    /// note and so reaches every note on the socket as an error.
    #[serde(other)]
    Unknown,
}

/// Server to client, over `/v1/sync`.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SyncServerMessage {
    /// One stored envelope. Sent for the backlog on subscribe and for every
    /// later append, the subscriber's own included: clients deduplicate by
    /// `(author, counter)` regardless, and it keeps `seq` tracking uniform.
    Update {
        note: String,
        seq: i64,
        envelope: String,
    },
    /// This note's backlog is drained. Per note, so a client can tell which
    /// notes are live while another is still catching up.
    Caught { note: String, seq: i64 },
    /// This note's cursor is beyond the relay's current log, which happens when
    /// the relay has been restored from an earlier backup. The client must
    /// reset its local cursor and resubscribe from the beginning.
    Reset { note: String },
    /// This note's log has grown enough to be worth compacting. Said to the
    /// device whose append crossed the line, and to nobody else, so that one
    /// member does it rather than all of them at once.
    Crowded { note: String },
    /// A compaction was refused. The device that asked stops asking until it
    /// reconnects, rather than sending a snapshot with every edit.
    CompactRefused { note: String, reason: String },
    /// Something was refused. `note` is absent when the message it answers
    /// could not be read far enough to say which note it was about.
    Error {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
        message: String,
    },
    /// Ordered backlog delivery has reached this refresh's captured head.
    RefreshComplete { id: String, note: String, seq: i64 },
}

// ---------------------------------------------------------------------------
// Invites
// ---------------------------------------------------------------------------
//
// An invite is a mailbox with a lifetime and room for one letter. The owner
// opens one, the first guest to present the link leaves a sealed join request
// in it, and the owner's device answers it with a sealed grant. The relay stores all of it and can read none of it: the requests are
// sealed under a key derived from the link's secret, the grant under a key only
// the guest who asked can derive, and neither side authenticates with a device
// key — so the relay learns no new edge between two devices either
// (ADR 0015).
//
// Every call is a JSON `POST`, the reads included, so that no secret is ever in
// a URL, where proxies and access logs would keep it.

/// How long an invite lasts when the owner does not say.
pub const INVITE_DEFAULT_TTL_SECS: u64 = 7 * 24 * 60 * 60;

/// The longest an invite may last. A link is only as safe as the chat it was
/// pasted into, and a month is already generous for "come and join our list".
pub const INVITE_MAX_TTL_SECS: u64 = 30 * 24 * 60 * 60;

/// How long an expired invite is kept so that a guest who opens a stale link
/// is told it expired, rather than that it never existed. Past this it is swept.
pub const INVITE_GRACE_SECS: i64 = 24 * 60 * 60;

/// A sealed join request: an X25519 key, a short name, and AEAD overhead.
pub const MAX_INVITE_PAYLOAD_BYTES: usize = 1024;

/// A sealed grant: an ephemeral key, a note ID and a note key, and overhead.
pub const MAX_INVITE_GRANT_BYTES: usize = 1024;

/// The pre-grant: a note ID, an owner key and a note key, sealed under the
/// link's secret, and overhead ([ADR 0030](../../docs/adr/0030-a-link-lets-its-first-person-in.md)).
pub const MAX_INVITE_PRE_GRANT_BYTES: usize = 256;

#[derive(Debug, Serialize, Deserialize)]
pub struct CreateInvite {
    /// Sixteen bytes derived from the link's secret, base64url — the same shape
    /// as a note ID and, like one, meaningless to the relay.
    pub invite: String,
    /// SHA-256 of the join token, base64url. The relay keeps the hash, so a copy
    /// of its database cannot be used to join.
    pub token_hash: String,
    /// SHA-256 of the owner's secret, base64url. The owner's secret is random
    /// and never in the link, so a guest cannot answer their own request.
    pub owner_hash: String,
    /// Seconds from now. Clamped to [`INVITE_MAX_TTL_SECS`].
    pub expires_in: u64,
    /// `easynotes_core::relay_access`'s invite proof, base64url. Required on
    /// a gated relay, ignored on an open one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access: Option<String>,
    /// The note key sealed for whoever claims the link, base64url. Optional:
    /// an older client sends none, and the join then waits for the owner's
    /// device as it always did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pre_grant: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CreatedInvite {
    /// What the relay actually granted, after clamping.
    pub expires_in: u64,
}

/// A guest asking to join. Proves it holds the link by presenting the token.
#[derive(Debug, Serialize, Deserialize)]
pub struct JoinInvite {
    pub token: String,
    /// The request's name: sixteen bytes, base64url, which the guest derives
    /// from the link and its account. The same person asking again sends the
    /// same name, and is given back the request they already made; anyone else
    /// finds the link used. The relay cannot tell whose it is.
    pub request: String,
    /// The sealed request, base64url.
    pub payload: String,
    /// `easynotes_core::relay_access`'s invite proof, base64url. Required on
    /// a gated relay, ignored on an open one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct JoinedInvite {
    /// The request's name, for polling it and for answering it.
    pub request: String,
    /// The sealed pre-grant, base64url, when the invite has one. The guest can
    /// read the note with it before the owner's device has seen the request.
    pub pre_grant: Option<String>,
}

/// A guest asking how its request went.
#[derive(Debug, Serialize, Deserialize)]
pub struct PollInvite {
    pub token: String,
    /// `easynotes_core::relay_access`'s invite proof, base64url. Required on
    /// a gated relay, ignored on an open one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestState {
    Pending,
    Approved,
    Declined,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PolledRequest {
    pub state: RequestState,
    /// The sealed grant, base64url, once the owner has approved.
    pub grant: Option<String>,
    /// The sealed pre-grant, base64url, while the request is pending. Sent
    /// again on every poll so a second device of the same account, or a retry,
    /// finds it too.
    pub pre_grant: Option<String>,
    /// Seconds until the invite expires; 0 once it has.
    pub expires_in: u64,
}

/// The owner sealing, or re-sealing, the note key for whoever claims the link.
#[derive(Debug, Serialize, Deserialize)]
pub struct SetPreGrant {
    pub owner: String,
    /// `easynotes_core::relay_access`'s invite proof, base64url. Required on
    /// a gated relay, ignored on an open one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access: Option<String>,
    /// The sealed pre-grant, base64url.
    pub pre_grant: String,
}

/// The owner proving it is the owner.
#[derive(Debug, Serialize, Deserialize)]
pub struct OwnInvite {
    pub owner: String,
    /// `easynotes_core::relay_access`'s invite proof, base64url. Required on
    /// a gated relay, ignored on an open one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct InviteListing {
    /// False once a request has been approved or the invite has expired.
    pub open: bool,
    pub expires_in: u64,
    pub requests: Vec<ListedRequest>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ListedRequest {
    pub request: String,
    /// The sealed request, base64url.
    pub payload: String,
    pub state: RequestState,
}

/// The owner's answer. A grant approves; no grant declines.
#[derive(Debug, Serialize, Deserialize)]
pub struct AnswerInvite {
    pub owner: String,
    pub grant: Option<String>,
    /// `easynotes_core::relay_access`'s invite proof, base64url. Required on
    /// a gated relay, ignored on an open one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access: Option<String>,
}

// ---------------------------------------------------------------------------
// Purge
// ---------------------------------------------------------------------------

/// How many topics one `POST /v1/purge` may name. A person deleting their
/// account purges every list they own at once; this is many households' worth,
/// and a longer list goes in several requests.
pub const MAX_PURGE_TOPICS: usize = 512;

/// `POST /v1/purge`: forget these topics for good
/// ([ADR 0026](../../docs/adr/0026-deleting-an-account.md)).
#[derive(Debug, Serialize, Deserialize)]
pub struct PurgeRequest {
    pub topics: Vec<PurgeTopic>,
}

/// One topic to purge. All base64url. The signature is `easynotes_core::purge`'s,
/// by the key [`crate::quota::authorise_purge`] accepts for the topic.
#[derive(Debug, Serialize, Deserialize)]
pub struct PurgeTopic {
    pub note: String,
    pub key: String,
    pub signature: String,
}

/// Which of them were not purged, and why. Everything not listed is gone,
/// including topics that already were.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct PurgeResponse {
    pub refused: Vec<PurgeRefused>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PurgeRefused {
    pub note: String,
    pub reason: String,
}
