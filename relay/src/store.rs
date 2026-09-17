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
}

// ---------------------------------------------------------------------------
// Memory
// ---------------------------------------------------------------------------

#[derive(Default, Clone)]
pub struct MemoryStore {
    notes: Arc<Mutex<HashMap<String, Vec<Stored>>>>,
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
}
