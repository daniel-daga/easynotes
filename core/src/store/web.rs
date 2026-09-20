//! The store a browser gets: reads from memory, writes to a key-value store it
//! is not allowed to wait for.
//!
//! [`OpStore`] is synchronous, and every asynchronous thing a browser offers is
//! not. IndexedDB has no synchronous API at all, `Atomics.wait` is forbidden on
//! the thread the bridge's synchronous calls run on, and making the trait `async`
//! would put `.await` through the CRDT, the sync engine and the bridge on every
//! platform to pay for one. So this store does what a browser leaves possible:
//!
//! * **Reads never leave memory.** Everything the database held is loaded once,
//!   at open, into the same maps [`MemoryStore`](super::MemoryStore) keeps — and
//!   that is the whole of the read path afterwards.
//! * **Writes are told, not awaited.** Every mutation changes those maps and
//!   hands a batch of key-value [`Change`]s to a [`ChangeSink`], which in a
//!   browser turns each batch into one IndexedDB transaction and returns before
//!   it commits.
//!
//! What that costs is stated in
//! [ADR 0011](../../../docs/adr/0011-the-web-store.md) and is worth repeating
//! here, because it is the only promise in this file that is weaker than the
//! native store's: an edit is in the database *shortly* after `append` returns
//! rather than by the time it does, so a tab closed inside that window loses it.
//! The native store fsyncs per append and does not. There is no browser API that
//! would close the gap, and the batches are issued one transaction per call
//! rather than coalesced precisely so the window stays as small as a browser
//! allows.
//!
//! ## Why the split is here and not at the browser's edge
//!
//! Everything in this file is ordinary Rust that `cargo test` runs: the key
//! layout, the sealing, what a batch contains, and what a database has to be
//! reloaded into to give back what was put in. `core/tests/store.rs` runs the
//! conformance suite over it like any other store and then drains a store's
//! changes into a map and reloads a second store from that map, which is the
//! whole of "it persists" without a browser in the loop.
//!
//! What is left for the browser is applying a batch and reading every row back —
//! two operations, no rules — and that lives in the bridge crate beside the
//! other place a tab differs from a phone.

use super::{key_context, meta_context, Keyring, OpStore};
use crate::crypto::{DeviceSeed, MasterKey, NoteKey};
use crate::error::{CoreError, Result};
use crate::ids::NoteId;
use std::collections::BTreeMap;

/// Bumped when the key layout below changes in a way a reader has to know about.
/// Kept in a row of its own, for the reason SQLite keeps `user_version`: a
/// database a newer build wrote is refused rather than half-understood, and in a
/// browser that matters more than on a phone, because an old tab and a new one
/// can be open at the same moment.
pub const SCHEMA_VERSION: u8 = 1;

/// The row that holds it.
pub const SCHEMA_KEY: &str = "schema";

/// One change to a browser's key-value store.
///
/// Keys are strings because that is what an object store indexes on, and the
/// layout is [`mod@self`]'s alone — nothing outside this file builds or parses
/// one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    Put {
        key: String,
        value: Vec<u8>,
    },
    Delete {
        key: String,
    },
    /// Every key that begins with `prefix`.
    ///
    /// Here for the same reason [`OpStore::forget`] is one call: the rows a
    /// note has are not a list this file should be keeping, and a delete that
    /// named them would leave behind whatever was added last and forgotten
    /// about. A range delete is one operation in IndexedDB, so this costs
    /// nothing to honour.
    DeletePrefix {
        prefix: String,
    },
}

/// Somewhere the changes go.
///
/// **One call is one transaction.** A batch is the unit that must not be applied
/// by halves — the two steps of a compaction, or everything a forgotten note
/// had — so an implementation that split one across two transactions would
/// reintroduce exactly the crash windows [`OpStore`] is written to avoid.
///
/// It returns nothing, which is the honest shape rather than a missing one: a
/// browser cannot tell the caller whether the write succeeded without making the
/// caller wait for it, and the point of this store is that it does not wait.
/// Whether the writes are landing is a question about the sink, and the sink is
/// what answers it — see `Library::storageStatus` in the bridge.
pub trait ChangeSink: Send {
    fn changed(&self, changes: &[Change]);
}

/// A sink that drops everything, for a browser with no database to offer and for
/// tests that only care about the reads.
pub struct Discard;

impl ChangeSink for Discard {
    fn changed(&self, _changes: &[Change]) {}
}

// ---------------------------------------------------------------------------
// The key layout
// ---------------------------------------------------------------------------
//
// Everything a note has is under one prefix, so that forgetting it is one range
// delete and cannot miss a row added later:
//
//     n/{note}/o/{seq}   one envelope of the log, seq in zero-padded hex so that
//                        the store's own key order is the log's order
//     n/{note}/s         the snapshot
//     n/{note}/m/{name}  one metadata row, sealed under the master key
//
// and the keyring, which is a different object store:
//
//     device-seed        sealed under the master key
//     k/{note}           one note key, likewise
//
// A note ID is exactly 22 base64url characters and neither `/` nor any character
// of one, so `n/{note}/` is a prefix of that note's rows and of nothing else's —
// the same argument the filesystem store's `forget` makes about the dot after an
// ID.

fn note_prefix(note: NoteId) -> String {
    format!("n/{}/", note.encode())
}

fn op_key(note: NoteId, seq: usize) -> String {
    format!("n/{}/o/{seq:08x}", note.encode())
}

fn snapshot_key(note: NoteId) -> String {
    format!("n/{}/s", note.encode())
}

fn meta_key(note: NoteId, name: &str) -> String {
    format!("n/{}/m/{name}", note.encode())
}

const DEVICE_SEED_KEY: &str = "device-seed";
const ACCOUNT_ROOT_KEY: &str = "account-root";

fn note_key_key(note: NoteId) -> String {
    format!("k/{}", note.encode())
}

fn malformed(key: &str) -> CoreError {
    CoreError::Storage(format!(
        "the local database holds a row named `{key}`, which this build does not understand"
    ))
}

/// The note and the rest of the key, for a row under `n/`.
fn split_note_row(key: &str) -> Option<(NoteId, &str)> {
    let rest = key.strip_prefix("n/")?;
    let (id, rest) = rest.split_once('/')?;
    Some((NoteId::decode(id).ok()?, rest))
}

// ---------------------------------------------------------------------------
// The log
// ---------------------------------------------------------------------------

/// The op log, the snapshots and the metadata a browser holds.
///
/// Metadata is kept **sealed** in memory and opened on each read, which is what
/// the SQLite store does with the same rows and for the same two reasons: the
/// plaintext of a sync cursor or a search index entry is resident for as long as
/// a caller is holding it and no longer, and a row that was written under a
/// different master key fails when it is read rather than at some distance from
/// the key that was wrong.
pub struct WebStore {
    logs: BTreeMap<NoteId, Vec<Vec<u8>>>,
    snapshots: BTreeMap<NoteId, Vec<u8>>,
    /// Sealed, by note and row name.
    meta: BTreeMap<(NoteId, String), Vec<u8>>,
    master: MasterKey,
    sink: Box<dyn ChangeSink>,
}

impl WebStore {
    /// A store for a browser that has never held a note. It writes the schema
    /// row, so the next launch can tell an empty database from one this build
    /// is too old for.
    pub fn new(master: MasterKey, sink: Box<dyn ChangeSink>) -> Self {
        let store = Self {
            logs: BTreeMap::new(),
            snapshots: BTreeMap::new(),
            meta: BTreeMap::new(),
            master,
            sink,
        };
        store.sink.changed(&[Change::Put {
            key: SCHEMA_KEY.to_string(),
            value: vec![SCHEMA_VERSION],
        }]);
        store
    }

    /// A store holding what a browser's object store had: every key and value in
    /// it, in any order.
    ///
    /// This is the only asynchronous moment in the web build's storage, and it
    /// is at open, where the app is already waiting.
    pub fn from_rows(
        master: MasterKey,
        sink: Box<dyn ChangeSink>,
        rows: impl IntoIterator<Item = (String, Vec<u8>)>,
    ) -> Result<Self> {
        // Collected first so the schema row is read before anything is believed,
        // whatever order the rows arrive in.
        let rows: Vec<(String, Vec<u8>)> = rows.into_iter().collect();
        let mut fresh = true;
        for (key, value) in &rows {
            if key == SCHEMA_KEY {
                fresh = false;
                match value.first() {
                    Some(&v) if v <= SCHEMA_VERSION => {}
                    Some(&v) => {
                        return Err(CoreError::Storage(format!(
                            "these notes were written by a newer version of the app (schema {v}, this build understands {SCHEMA_VERSION})"
                        )))
                    }
                    None => return Err(malformed(SCHEMA_KEY)),
                }
            }
        }
        if fresh && !rows.is_empty() {
            // Rows but no schema row: not a database this file wrote. Refusing
            // is the same call as the version check above — the alternative is
            // writing into it and making the mess permanent.
            return Err(CoreError::Storage(
                "the local database has no schema row, so it was not written by this app".into(),
            ));
        }

        // Keyed by the row name rather than by sequence, because the sequence
        // numbers in the keys are the *stored* order and need not be dense: a
        // compaction rewrites them from 0, but an interrupted one need not have.
        // Sorting the keys is what recovers the log's order, and the zero-padded
        // hex is what makes sorting them the same as sorting the sequences.
        let mut logs: BTreeMap<NoteId, BTreeMap<String, Vec<u8>>> = BTreeMap::new();
        let mut snapshots = BTreeMap::new();
        let mut meta = BTreeMap::new();

        for (key, value) in rows {
            if key == SCHEMA_KEY {
                continue;
            }
            let (note, rest) = split_note_row(&key).ok_or_else(|| malformed(&key))?;
            if rest == "s" {
                snapshots.insert(note, value);
            } else if let Some(seq) = rest.strip_prefix("o/") {
                logs.entry(note).or_default().insert(seq.to_string(), value);
            } else if let Some(name) = rest.strip_prefix("m/") {
                meta.insert((note, name.to_string()), value);
            } else {
                return Err(malformed(&key));
            }
        }

        let store = Self {
            logs: logs
                .into_iter()
                .map(|(note, ops)| (note, ops.into_values().collect()))
                .collect(),
            snapshots,
            meta,
            master,
            sink,
        };
        if fresh {
            store.sink.changed(&[Change::Put {
                key: SCHEMA_KEY.to_string(),
                value: vec![SCHEMA_VERSION],
            }]);
        }
        Ok(store)
    }
}

impl OpStore for WebStore {
    fn append(&mut self, note: NoteId, envelope: &[u8]) -> Result<()> {
        let log = self.logs.entry(note).or_default();
        let seq = log.len();
        log.push(envelope.to_vec());
        self.sink.changed(&[Change::Put {
            key: op_key(note, seq),
            value: envelope.to_vec(),
        }]);
        Ok(())
    }

    fn load(&self, note: NoteId) -> Result<Vec<Vec<u8>>> {
        Ok(self.logs.get(&note).cloned().unwrap_or_default())
    }

    fn put_snapshot(&mut self, note: NoteId, snapshot: &[u8]) -> Result<()> {
        self.snapshots.insert(note, snapshot.to_vec());
        self.sink.changed(&[Change::Put {
            key: snapshot_key(note),
            value: snapshot.to_vec(),
        }]);
        Ok(())
    }

    fn get_snapshot(&self, note: NoteId) -> Result<Option<Vec<u8>>> {
        Ok(self.snapshots.get(&note).cloned())
    }

    fn replace_log(&mut self, note: NoteId, envelopes: &[Vec<u8>]) -> Result<()> {
        self.logs.insert(note, envelopes.to_vec());
        // One batch, so one transaction: a compaction that dropped the log and
        // then failed to write back what the relay has not acknowledged would
        // lose the only copy of those edits.
        let mut batch = vec![Change::DeletePrefix {
            prefix: format!("{}o/", note_prefix(note)),
        }];
        batch.extend(
            envelopes
                .iter()
                .enumerate()
                .map(|(seq, envelope)| Change::Put {
                    key: op_key(note, seq),
                    value: envelope.clone(),
                }),
        );
        self.sink.changed(&batch);
        Ok(())
    }

    fn log_len(&self, note: NoteId) -> Result<usize> {
        // Asked once per edit by the index, and answering it by cloning the log
        // would make every keystroke quadratic in the log behind it.
        Ok(self.logs.get(&note).map_or(0, Vec::len))
    }

    fn put_meta(&mut self, note: NoteId, key: &str, value: &[u8]) -> Result<()> {
        let sealed = self.master.seal(&meta_context(&note, key), value)?;
        self.sink.changed(&[Change::Put {
            key: meta_key(note, key),
            value: sealed.clone(),
        }]);
        self.meta.insert((note, key.to_string()), sealed);
        Ok(())
    }

    fn get_meta(&self, note: NoteId, key: &str) -> Result<Option<Vec<u8>>> {
        match self.meta.get(&(note, key.to_string())) {
            Some(sealed) => Ok(Some(self.master.open(&meta_context(&note, key), sealed)?)),
            None => Ok(None),
        }
    }

    fn forget(&mut self, note: NoteId) -> Result<()> {
        self.logs.remove(&note);
        self.snapshots.remove(&note);
        self.meta.retain(|(id, _), _| *id != note);
        // The prefix rather than the rows this store knows it has: the index
        // entry holds the note's whole text, so the row a future version adds
        // and this function forgets to name is the one left readable.
        self.sink.changed(&[Change::DeletePrefix {
            prefix: note_prefix(note),
        }]);
        Ok(())
    }

    fn notes(&self) -> Result<Vec<NoteId>> {
        let mut ids: Vec<NoteId> = self
            .logs
            .keys()
            .chain(self.snapshots.keys())
            .chain(self.meta.keys().map(|(id, _)| id))
            .copied()
            .collect();
        ids.sort();
        ids.dedup();
        Ok(ids)
    }
}

// ---------------------------------------------------------------------------
// The keyring
// ---------------------------------------------------------------------------

/// The device seed and the note keys a browser holds, each sealed under the
/// master key.
///
/// A separate object store from the log, which is as far as the separation the
/// two SQLite files have can be carried into a browser: there is one origin and
/// anything running on it reaches both. [ADR
/// 0011](../../../docs/adr/0011-the-web-store.md) says what that means for the
/// master key on the web, which is the honest half of this.
pub struct WebKeyring {
    seed: Option<Vec<u8>>,
    root: Option<Vec<u8>>,
    keys: BTreeMap<NoteId, Vec<u8>>,
    master: MasterKey,
    sink: Box<dyn ChangeSink>,
}

impl WebKeyring {
    pub fn new(master: MasterKey, sink: Box<dyn ChangeSink>) -> Self {
        Self {
            seed: None,
            root: None,
            keys: BTreeMap::new(),
            master,
            sink,
        }
    }

    pub fn from_rows(
        master: MasterKey,
        sink: Box<dyn ChangeSink>,
        rows: impl IntoIterator<Item = (String, Vec<u8>)>,
    ) -> Result<Self> {
        let mut keyring = Self::new(master, sink);
        for (key, value) in rows {
            if key == DEVICE_SEED_KEY {
                keyring.seed = Some(value);
            } else if key == ACCOUNT_ROOT_KEY {
                keyring.root = Some(value);
            } else if let Some(id) = key.strip_prefix("k/") {
                keyring
                    .keys
                    .insert(NoteId::decode(id).map_err(|_| malformed(&key))?, value);
            } else {
                return Err(malformed(&key));
            }
        }
        Ok(keyring)
    }
}

impl Keyring for WebKeyring {
    fn device_seed(&self) -> Result<Option<DeviceSeed>> {
        let Some(sealed) = &self.seed else {
            return Ok(None);
        };
        let raw = self.master.open(&key_context("device-seed", &[]), sealed)?;
        let b: [u8; 64] = raw
            .try_into()
            .map_err(|_| CoreError::Storage("device seed is not 64 bytes".into()))?;
        Ok(Some(DeviceSeed(b)))
    }

    fn put_device_seed(&mut self, seed: &DeviceSeed) -> Result<()> {
        let sealed = self
            .master
            .seal(&key_context("device-seed", &[]), &seed.0)?;
        self.sink.changed(&[Change::Put {
            key: DEVICE_SEED_KEY.to_string(),
            value: sealed.clone(),
        }]);
        self.seed = Some(sealed);
        Ok(())
    }

    fn account_root(&self) -> Result<Option<crate::account::AccountRoot>> {
        let Some(sealed) = &self.root else {
            return Ok(None);
        };
        let raw = self
            .master
            .open(&key_context("account-root", &[]), sealed)?;
        Ok(Some(crate::account::AccountRoot::from_slice(&raw)?))
    }

    fn put_account_root(&mut self, root: &crate::account::AccountRoot) -> Result<()> {
        let sealed = self
            .master
            .seal(&key_context("account-root", &[]), root.as_bytes())?;
        self.sink.changed(&[Change::Put {
            key: ACCOUNT_ROOT_KEY.to_string(),
            value: sealed.clone(),
        }]);
        self.root = Some(sealed);
        Ok(())
    }

    fn note_key(&self, note: NoteId) -> Result<Option<NoteKey>> {
        let Some(sealed) = self.keys.get(&note) else {
            return Ok(None);
        };
        let raw = self
            .master
            .open(&key_context("note-key", note.as_bytes()), sealed)?;
        let b: [u8; 32] = raw
            .try_into()
            .map_err(|_| CoreError::Storage("note key is not 32 bytes".into()))?;
        Ok(Some(NoteKey(b)))
    }

    fn put_note_key(&mut self, note: NoteId, key: &NoteKey) -> Result<()> {
        let sealed = self
            .master
            .seal(&key_context("note-key", note.as_bytes()), &key.0)?;
        self.sink.changed(&[Change::Put {
            key: note_key_key(note),
            value: sealed.clone(),
        }]);
        self.keys.insert(note, sealed);
        Ok(())
    }

    fn forget_note_key(&mut self, note: NoteId) -> Result<()> {
        self.keys.remove(&note);
        self.sink.changed(&[Change::Delete {
            key: note_key_key(note),
        }]);
        Ok(())
    }
}
