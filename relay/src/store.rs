//! Where ciphertext goes.
//!
//! Two implementations: Postgres for production, in-memory for tests and for
//! `just run-relay`. Both are append-only logs keyed by note ID, with a
//! per-note monotonic sequence number.
//!
//! Note IDs are stored as the opaque 22-character base64url strings the client
//! sends. The relay never decodes them into anything meaningful, because there is
//! nothing meaningful in them.

use crate::error::{RelayError, Result};
use crate::protocol::{RequestState, MAX_INVITE_REQUESTS};
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// One stored envelope.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stored {
    pub seq: i64,
    pub envelope: Vec<u8>,
}

#[async_trait]
pub trait RelayStore: Send + Sync + 'static {
    /// Append and return the sequence number assigned. Sequence numbers start at
    /// 1 and increase by one per note, with no gaps.
    async fn append(&self, note: &str, envelope: &[u8]) -> Result<i64>;

    /// Everything with `seq > since`, oldest first.
    async fn since(&self, note: &str, since: i64) -> Result<Vec<Stored>>;

    /// The highest sequence number for a note, or 0 if it has none.
    async fn head(&self, note: &str) -> Result<i64>;

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

    /// Leave a request on an invite, if it is open, unexpired and not full.
    async fn add_invite_request(
        &self,
        id: &str,
        request: &str,
        payload: &[u8],
        now: i64,
    ) -> Result<AddRequest>;

    /// Approve a pending request with `grant`, or decline it with `None`.
    ///
    /// Approving closes the invite and declines every other request still
    /// pending: an invite admits one person, and a second approval would hand
    /// the key to whoever the first one was not.
    async fn answer_invite_request(
        &self,
        id: &str,
        request: &str,
        grant: Option<&[u8]>,
        now: i64,
    ) -> Result<Answered>;

    /// Remove an invite and everything on it. Not an error if it is not there.
    async fn withdraw_invite(&self, id: &str) -> Result<()>;
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
    /// No such invite.
    Missing,
    /// Expired, or a request on it has already been approved.
    Closed,
    /// [`MAX_INVITE_REQUESTS`] already.
    Full,
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

#[derive(Default, Clone)]
pub struct MemoryStore {
    notes: Arc<Mutex<HashMap<String, Vec<Stored>>>>,
    invites: Arc<Mutex<HashMap<String, Invite>>>,
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
            .flat_map(|v| v.iter().map(|s| s.envelope.clone()))
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
    async fn append(&self, note: &str, envelope: &[u8]) -> Result<i64> {
        let mut notes = self.notes.lock().unwrap();
        let log = notes.entry(note.to_string()).or_default();
        let seq = log.len() as i64 + 1;
        log.push(Stored {
            seq,
            envelope: envelope.to_vec(),
        });
        Ok(seq)
    }

    async fn since(&self, note: &str, since: i64) -> Result<Vec<Stored>> {
        let notes = self.notes.lock().unwrap();
        Ok(notes
            .get(note)
            .map(|log| log.iter().filter(|s| s.seq > since).cloned().collect())
            .unwrap_or_default())
    }

    async fn head(&self, note: &str) -> Result<i64> {
        let notes = self.notes.lock().unwrap();
        Ok(notes.get(note).and_then(|l| l.last()).map_or(0, |s| s.seq))
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
        if !invite.open || invite.expires_at <= now {
            return Ok(AddRequest::Closed);
        }
        if invite.requests.len() >= MAX_INVITE_REQUESTS {
            return Ok(AddRequest::Full);
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

    /// Create the schema if it is not already there.
    ///
    /// Held under an advisory lock, because `CREATE TABLE IF NOT EXISTS` is *not*
    /// safe to run concurrently in Postgres: two sessions racing it both pass the
    /// existence check and then collide on the system catalog with
    /// `duplicate key value violates unique constraint "pg_type_typname_nsp_index"`.
    /// Every relay instance runs this at startup, so "several at once" is the
    /// normal case on a deploy, not an edge case.
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

        // Two tables, because the sequence number has to be allocated atomically.
        //
        // `note_head` holds one row per note carrying its highest `seq`; an
        // `ON CONFLICT DO UPDATE ... RETURNING` against it takes a row lock and
        // hands back the next number in a single statement, so concurrent
        // appenders queue rather than collide. `note_log` holds the envelopes.
        //
        // The obvious `SELECT MAX(seq) + 1` does not work here: readers do not
        // block each other, so sixteen concurrent appends all compute the same
        // number and fifteen of them lose on the primary key. Retrying just moves
        // the collision to the retry.
        //
        // `seq` is per note rather than global so that a client's `since` cursor
        // is dense and means what it looks like it means.
        let created = async {
            sqlx::query(
                r#"
                CREATE TABLE IF NOT EXISTS note_log (
                    note_id     TEXT   NOT NULL,
                    seq         BIGINT NOT NULL,
                    envelope    BYTEA  NOT NULL,
                    received_at TIMESTAMPTZ NOT NULL DEFAULT now(),
                    PRIMARY KEY (note_id, seq)
                )
                "#,
            )
            .execute(&mut *conn)
            .await?;
            sqlx::query(
                r#"
                CREATE TABLE IF NOT EXISTS note_head (
                    note_id TEXT   PRIMARY KEY,
                    seq     BIGINT NOT NULL
                )
                "#,
            )
            .execute(&mut *conn)
            .await?;
            // Invites. `expires_at` is Unix seconds, the same number the memory
            // store keeps, so the two cannot round differently. `grant_blob`
            // because `grant` is a keyword.
            sqlx::query(
                r#"
                CREATE TABLE IF NOT EXISTS invites (
                    invite_id  TEXT    PRIMARY KEY,
                    token_hash BYTEA   NOT NULL,
                    owner_hash BYTEA   NOT NULL,
                    expires_at BIGINT  NOT NULL,
                    open       BOOLEAN NOT NULL DEFAULT TRUE
                )
                "#,
            )
            .execute(&mut *conn)
            .await?;
            sqlx::query(
                r#"
                CREATE TABLE IF NOT EXISTS invite_requests (
                    invite_id  TEXT     NOT NULL REFERENCES invites (invite_id) ON DELETE CASCADE,
                    request_id TEXT     NOT NULL,
                    payload    BYTEA    NOT NULL,
                    state      SMALLINT NOT NULL DEFAULT 0,
                    grant_blob BYTEA,
                    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
                    PRIMARY KEY (invite_id, request_id)
                )
                "#,
            )
            .execute(&mut *conn)
            .await
        }
        .await;

        // Unlock before propagating any failure: an advisory lock is
        // session-scoped, and this connection is about to go back into the pool.
        let unlocked = sqlx::query("SELECT pg_advisory_unlock($1)")
            .bind(MIGRATION_LOCK)
            .execute(&mut *conn)
            .await;

        created.map_err(|e: sqlx::Error| RelayError::Storage(e.to_string()))?;
        unlocked.map_err(|e| RelayError::Storage(e.to_string()))?;
        Ok(())
    }
}

#[async_trait]
impl RelayStore for PostgresStore {
    async fn append(&self, note: &str, envelope: &[u8]) -> Result<i64> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| RelayError::Storage(e.to_string()))?;

        // Allocating the number and writing the row are one transaction, so a
        // crash between them cannot burn a sequence number and leave a hole that
        // a client's `since` cursor would wait on forever.
        let (seq,): (i64,) = sqlx::query_as(
            r#"
            INSERT INTO note_head (note_id, seq) VALUES ($1, 1)
            ON CONFLICT (note_id) DO UPDATE SET seq = note_head.seq + 1
            RETURNING seq
            "#,
        )
        .bind(note)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| RelayError::Storage(e.to_string()))?;

        sqlx::query("INSERT INTO note_log (note_id, seq, envelope) VALUES ($1, $2, $3)")
            .bind(note)
            .bind(seq)
            .bind(envelope)
            .execute(&mut *tx)
            .await
            .map_err(|e| RelayError::Storage(e.to_string()))?;

        tx.commit()
            .await
            .map_err(|e| RelayError::Storage(e.to_string()))?;
        Ok(seq)
    }

    async fn since(&self, note: &str, since: i64) -> Result<Vec<Stored>> {
        let rows: Vec<(i64, Vec<u8>)> = sqlx::query_as(
            "SELECT seq, envelope FROM note_log WHERE note_id = $1 AND seq > $2 ORDER BY seq",
        )
        .bind(note)
        .bind(since)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| RelayError::Storage(e.to_string()))?;
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
            .map_err(|e| RelayError::Storage(e.to_string()))?;
        Ok(row.map_or(0, |r| r.0))
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
        // The row lock is what makes the cap a cap: without it, sixteen guests
        // arriving at once all count fifteen and all get in.
        let row: Option<(bool, i64)> =
            sqlx::query_as("SELECT open, expires_at FROM invites WHERE invite_id = $1 FOR UPDATE")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(pg)?;
        let Some((open, expires_at)) = row else {
            return Ok(AddRequest::Missing);
        };
        if !open || expires_at <= now {
            return Ok(AddRequest::Closed);
        }
        let (count,): (i64,) =
            sqlx::query_as("SELECT count(*) FROM invite_requests WHERE invite_id = $1")
                .bind(id)
                .fetch_one(&mut *tx)
                .await
                .map_err(pg)?;
        if count as usize >= MAX_INVITE_REQUESTS {
            return Ok(AddRequest::Full);
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

    async fn withdraw_invite(&self, id: &str) -> Result<()> {
        sqlx::query("DELETE FROM invites WHERE invite_id = $1")
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(pg)?;
        Ok(())
    }
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
