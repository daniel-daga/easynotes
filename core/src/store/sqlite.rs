//! The real local store: SQLite, with everything that is not already an envelope
//! sealed under the master key.
//!
//! What is in the file, and what that means for someone holding it:
//!
//! | Column | What it is |
//! | --- | --- |
//! | `ops.envelope`, `snapshots.envelope` | Ciphertext already, sealed under the note key by the core before it ever reaches a store |
//! | `meta.value` | Sealed under the master key, bound to the note and the key name |
//! | `keys.secret` (the keyring) | The device seed and the note keys, sealed under the master key |
//! | `ops.note`, the sizes, the ordering | Opaque, and not encrypted — see [ADR 0005](../../../docs/adr/0005-local-store.md) |
//!
//! So a stolen database without the master key yields no note content, no keys
//! and no identity: only how many notes this device has, how big they are, and
//! in what order they were written. That is the same class of metadata the relay
//! already sees, and [ADR 0005](../../../docs/adr/0005-local-store.md) says what
//! it would take to hide it too.
//!
//! The log and the keys are two files, `notes.db` and `keys.db`, for the reason
//! the filesystem store has `log/` and `keys/`: key material does not go in the
//! file the log is in, whatever the encryption says.

use super::{Keyring, OpStore};
use crate::crypto::{DeviceSeed, MasterKey, NoteKey};
use crate::error::{CoreError, Result};
use crate::ids::NoteId;
use rusqlite::{Connection, OptionalExtension};
use std::path::Path;

/// Bumped when the schema changes in a way a migration has to handle. Stored in
/// SQLite's own `user_version`, so it costs no table.
const SCHEMA_VERSION: i32 = 1;

fn db(e: rusqlite::Error) -> CoreError {
    CoreError::Storage(e.to_string())
}

/// The pragmas every connection needs, whichever table it is for.
///
/// `synchronous = FULL` is the durability policy the filesystem store deferred:
/// an append is on the disk when it returns, so a kill costs no edit rather than
/// the last one. It is one fsync per edit, which is well within the budget for a
/// person typing a shopping list, and the thing it buys — an edit the user
/// watched appear and the app then forgets — is exactly the bug an offline-first
/// app cannot have.
fn prepare(conn: &Connection) -> Result<()> {
    // WAL survives the process dying mid-write and lets a reader run while a
    // writer commits. `journal_mode` is persistent; the rest are per connection.
    conn.pragma_update(None, "journal_mode", "WAL")
        .map_err(db)?;
    conn.pragma_update(None, "synchronous", "FULL")
        .map_err(db)?;
    // Two windows of the app, or a test and the app, are two writers. SQLite
    // serialises them; without a timeout the loser gets an error instead.
    conn.busy_timeout(std::time::Duration::from_secs(5))
        .map_err(db)?;
    Ok(())
}

fn migrate(conn: &mut Connection, schema: &str) -> Result<()> {
    // IMMEDIATE so that two processes opening the database at the same moment do
    // not both decide it is empty. Slice 1 learned this against Postgres, where a
    // racing `CREATE TABLE IF NOT EXISTS` deadlocked on the system catalog.
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(db)?;
    let version: i32 = tx
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .map_err(db)?;
    if version == 0 {
        tx.execute_batch(schema).map_err(db)?;
        tx.pragma_update(None, "user_version", SCHEMA_VERSION)
            .map_err(db)?;
    } else if version > SCHEMA_VERSION {
        return Err(CoreError::Storage(format!(
            "this database was written by a newer version of the app (schema {version}, this build understands {SCHEMA_VERSION})"
        )));
    }
    tx.commit().map_err(db)
}

// ---------------------------------------------------------------------------
// The log
// ---------------------------------------------------------------------------

const STORE_SCHEMA: &str = "
    CREATE TABLE ops (
        note     BLOB NOT NULL,
        seq      INTEGER NOT NULL,
        envelope BLOB NOT NULL,
        PRIMARY KEY (note, seq)
    ) WITHOUT ROWID;

    CREATE TABLE snapshots (
        note     BLOB NOT NULL PRIMARY KEY,
        envelope BLOB NOT NULL
    ) WITHOUT ROWID;

    CREATE TABLE meta (
        note  BLOB NOT NULL,
        key   TEXT NOT NULL,
        value BLOB NOT NULL,
        PRIMARY KEY (note, key)
    ) WITHOUT ROWID;
";

pub struct SqliteStore {
    conn: Connection,
    master: MasterKey,
}

impl SqliteStore {
    /// Open, or create, the op log at `path`. The master key seals the rows that
    /// are not already envelopes; it is never written to the file.
    pub fn open(path: impl AsRef<Path>, master: MasterKey) -> Result<Self> {
        if let Some(parent) = path.as_ref().parent() {
            std::fs::create_dir_all(parent).map_err(|e| CoreError::Storage(e.to_string()))?;
        }
        let conn = Connection::open(path).map_err(db)?;
        Self::from_connection(conn, master)
    }

    /// A database that exists only for as long as the handle does. Tests use it;
    /// the app does not.
    pub fn in_memory(master: MasterKey) -> Result<Self> {
        Self::from_connection(Connection::open_in_memory().map_err(db)?, master)
    }

    fn from_connection(mut conn: Connection, master: MasterKey) -> Result<Self> {
        prepare(&conn)?;
        migrate(&mut conn, STORE_SCHEMA)?;
        Ok(Self { conn, master })
    }

    /// The sync cursor and anything else local is sealed against the note it
    /// belongs to and the name it is stored under, so a row cannot be moved to
    /// another note or another key and still open.
    fn meta_context<'a>(note: &'a NoteId, key: &'a str) -> [&'a [u8]; 3] {
        [b"meta", note.as_bytes(), key.as_bytes()]
    }
}

impl OpStore for SqliteStore {
    fn append(&mut self, note: NoteId, envelope: &[u8]) -> Result<()> {
        // One statement, so the next sequence number is chosen under the write
        // lock that inserts it. The same `MAX(seq) + 1` in two statements is what
        // collapsed under concurrency in the relay's Postgres store in Slice 1.
        self.conn
            .execute(
                "INSERT INTO ops (note, seq, envelope)
                 VALUES (?1, (SELECT COALESCE(MAX(seq), 0) + 1 FROM ops WHERE note = ?1), ?2)",
                rusqlite::params![note.as_bytes().as_slice(), envelope],
            )
            .map_err(db)?;
        Ok(())
    }

    fn load(&self, note: NoteId) -> Result<Vec<Vec<u8>>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT envelope FROM ops WHERE note = ?1 ORDER BY seq")
            .map_err(db)?;
        let rows = stmt
            .query_map([note.as_bytes().as_slice()], |r| r.get::<_, Vec<u8>>(0))
            .map_err(db)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(db)?);
        }
        Ok(out)
    }

    fn put_snapshot(&mut self, note: NoteId, snapshot: &[u8]) -> Result<()> {
        self.conn
            .execute(
                "INSERT INTO snapshots (note, envelope) VALUES (?1, ?2)
                 ON CONFLICT (note) DO UPDATE SET envelope = excluded.envelope",
                rusqlite::params![note.as_bytes().as_slice(), snapshot],
            )
            .map_err(db)?;
        Ok(())
    }

    fn replace_log(&mut self, note: NoteId, envelopes: &[Vec<u8>]) -> Result<()> {
        // In one transaction: a compaction that deleted the log and then failed
        // to write back what the relay has not acknowledged would lose exactly
        // the edits nothing else has a copy of.
        let tx = self.conn.transaction().map_err(db)?;
        tx.execute(
            "DELETE FROM ops WHERE note = ?1",
            [note.as_bytes().as_slice()],
        )
        .map_err(db)?;
        {
            let mut insert = tx
                .prepare("INSERT INTO ops (note, seq, envelope) VALUES (?1, ?2, ?3)")
                .map_err(db)?;
            for (seq, envelope) in envelopes.iter().enumerate() {
                insert
                    .execute(rusqlite::params![
                        note.as_bytes().as_slice(),
                        seq as i64 + 1,
                        envelope
                    ])
                    .map_err(db)?;
            }
        }
        tx.commit().map_err(db)
    }

    fn log_len(&self, note: NoteId) -> Result<usize> {
        // The one question a database can answer without handing back the answer
        // to a different one: deciding whether to compact should not cost a read
        // of everything compaction would remove.
        let count: i64 = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM ops WHERE note = ?1",
                [note.as_bytes().as_slice()],
                |r| r.get(0),
            )
            .map_err(db)?;
        Ok(count as usize)
    }

    fn get_snapshot(&self, note: NoteId) -> Result<Option<Vec<u8>>> {
        self.conn
            .query_row(
                "SELECT envelope FROM snapshots WHERE note = ?1",
                [note.as_bytes().as_slice()],
                |r| r.get::<_, Vec<u8>>(0),
            )
            .optional()
            .map_err(db)
    }

    fn put_meta(&mut self, note: NoteId, key: &str, value: &[u8]) -> Result<()> {
        let sealed = self.master.seal(&Self::meta_context(&note, key), value)?;
        self.conn
            .execute(
                "INSERT INTO meta (note, key, value) VALUES (?1, ?2, ?3)
                 ON CONFLICT (note, key) DO UPDATE SET value = excluded.value",
                rusqlite::params![note.as_bytes().as_slice(), key, sealed],
            )
            .map_err(db)?;
        Ok(())
    }

    fn get_meta(&self, note: NoteId, key: &str) -> Result<Option<Vec<u8>>> {
        let sealed: Option<Vec<u8>> = self
            .conn
            .query_row(
                "SELECT value FROM meta WHERE note = ?1 AND key = ?2",
                rusqlite::params![note.as_bytes().as_slice(), key],
                |r| r.get(0),
            )
            .optional()
            .map_err(db)?;
        match sealed {
            Some(sealed) => Ok(Some(
                self.master.open(&Self::meta_context(&note, key), &sealed)?,
            )),
            None => Ok(None),
        }
    }

    fn notes(&self) -> Result<Vec<NoteId>> {
        let mut stmt = self
            .conn
            .prepare_cached(
                "SELECT note FROM ops \
                 UNION SELECT note FROM snapshots \
                 UNION SELECT note FROM meta \
                 ORDER BY note",
            )
            .map_err(db)?;
        let rows = stmt.query_map([], |r| r.get::<_, Vec<u8>>(0)).map_err(db)?;
        let mut out = Vec::new();
        for row in rows {
            let raw = row.map_err(db)?;
            let id: [u8; 16] = raw.try_into().map_err(|_| {
                CoreError::Storage("a note ID in the database is not 16 bytes".into())
            })?;
            out.push(NoteId::from_bytes(id));
        }
        out.sort();
        Ok(out)
    }
}

// ---------------------------------------------------------------------------
// The keyring
// ---------------------------------------------------------------------------

const KEYRING_SCHEMA: &str = "
    CREATE TABLE keys (
        kind   TEXT NOT NULL,
        id     BLOB NOT NULL,
        secret BLOB NOT NULL,
        PRIMARY KEY (kind, id)
    ) WITHOUT ROWID;
";

/// The device seed and the note keys, each sealed under the master key.
///
/// This is not yet the OS keychain the design doc calls for — that needs the
/// platform, and the platform is Dart's. It is the step that makes the keychain a
/// change of *where the master key comes from* rather than a rewrite: everything
/// here is already wrapped, and moving the master key into Keystore or Keychain
/// leaves this file untouched.
pub struct SqliteKeyring {
    conn: Connection,
    master: MasterKey,
}

/// The one row that is not per note.
const DEVICE_SEED_ID: &[u8] = b"";

impl SqliteKeyring {
    pub fn open(path: impl AsRef<Path>, master: MasterKey) -> Result<Self> {
        if let Some(parent) = path.as_ref().parent() {
            std::fs::create_dir_all(parent).map_err(|e| CoreError::Storage(e.to_string()))?;
        }
        let conn = Connection::open(path).map_err(db)?;
        Self::from_connection(conn, master)
    }

    pub fn in_memory(master: MasterKey) -> Result<Self> {
        Self::from_connection(Connection::open_in_memory().map_err(db)?, master)
    }

    fn from_connection(mut conn: Connection, master: MasterKey) -> Result<Self> {
        prepare(&conn)?;
        migrate(&mut conn, KEYRING_SCHEMA)?;
        Ok(Self { conn, master })
    }

    fn get(&self, kind: &str, id: &[u8]) -> Result<Option<Vec<u8>>> {
        let sealed: Option<Vec<u8>> = self
            .conn
            .query_row(
                "SELECT secret FROM keys WHERE kind = ?1 AND id = ?2",
                rusqlite::params![kind, id],
                |r| r.get(0),
            )
            .optional()
            .map_err(db)?;
        match sealed {
            // The kind is in the context, so a note key cannot be handed back as
            // the device seed by rewriting one column.
            Some(sealed) => Ok(Some(self.master.open(&[kind.as_bytes(), id], &sealed)?)),
            None => Ok(None),
        }
    }

    fn put(&mut self, kind: &str, id: &[u8], secret: &[u8]) -> Result<()> {
        let sealed = self.master.seal(&[kind.as_bytes(), id], secret)?;
        self.conn
            .execute(
                "INSERT INTO keys (kind, id, secret) VALUES (?1, ?2, ?3)
                 ON CONFLICT (kind, id) DO UPDATE SET secret = excluded.secret",
                rusqlite::params![kind, id, sealed],
            )
            .map_err(db)?;
        Ok(())
    }
}

impl Keyring for SqliteKeyring {
    fn device_seed(&self) -> Result<Option<DeviceSeed>> {
        match self.get("device-seed", DEVICE_SEED_ID)? {
            Some(raw) => {
                let b: [u8; 64] = raw
                    .try_into()
                    .map_err(|_| CoreError::Storage("device seed is not 64 bytes".into()))?;
                Ok(Some(DeviceSeed(b)))
            }
            None => Ok(None),
        }
    }

    fn put_device_seed(&mut self, seed: &DeviceSeed) -> Result<()> {
        self.put("device-seed", DEVICE_SEED_ID, &seed.0)
    }

    fn note_key(&self, note: NoteId) -> Result<Option<NoteKey>> {
        match self.get("note-key", note.as_bytes())? {
            Some(raw) => {
                let b: [u8; 32] = raw
                    .try_into()
                    .map_err(|_| CoreError::Storage("note key is not 32 bytes".into()))?;
                Ok(Some(NoteKey(b)))
            }
            None => Ok(None),
        }
    }

    fn put_note_key(&mut self, note: NoteId, key: &NoteKey) -> Result<()> {
        self.put("note-key", note.as_bytes(), &key.0)
    }
}

// ---------------------------------------------------------------------------
// Opening what a device has
// ---------------------------------------------------------------------------

/// Open the local store a device keeps in `root`, whatever it had there before.
///
/// This is the whole of "where does this app keep things", in one place the
/// tests can reach without a Flutter toolchain: the database, the keyring, and
/// the one-time carry-over from the directory of files Slice 1 wrote. A user
/// who updates the app has not agreed to lose their list.
///
/// `master` comes from the caller and is not read from anywhere near here. On a
/// device that is the OS keychain, reached by Dart; in a test it is whatever the
/// test made. The core does not choose where a key lives, which is what lets the
/// same code run under `cargo test` and on a phone.
///
/// The old files are left where they are. Deleting the only copy of the data on
/// the same launch that first writes the new one is how a migration turns a bug
/// into a loss; they can go in a release that has been out for a while.
pub fn open_local(
    root: impl AsRef<Path>,
    master: MasterKey,
) -> Result<(SqliteStore, SqliteKeyring)> {
    let root = root.as_ref();
    let database = root.join("notes.db");
    // Decided before the database is created, or it would never be true.
    let carry_over = !database.exists() && root.join("log").is_dir();

    std::fs::create_dir_all(root).map_err(|e| CoreError::Storage(e.to_string()))?;
    let mut store = SqliteStore::open(&database, master.clone())?;
    let mut keyring = SqliteKeyring::open(root.join("keys.db"), master)?;

    if carry_over {
        let old_store = super::FileStore::open(root.join("log"))?;
        let old_keys = super::FileKeyring::open(root.join("keys"))?;
        super::copy_store(
            &old_store,
            &old_keys,
            &mut store,
            &mut keyring,
            &[crate::note::SYNC_CURSOR, crate::note::ADDED],
        )?;
    }

    Ok((store, keyring))
}
