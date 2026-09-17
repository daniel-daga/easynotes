//! The API the UI talks to: one object that owns the keys, the CRDT and the log,
//! and hands out a stream of [`NoteView`]s.
//!
//! The shape is the one the Slice 1 plan names:
//! `create_note`, `open_note`, `apply_local`, `apply_remote`, `export_share_blob`,
//! `import_share_blob`, `watch`.
//!
//! The rule the whole app follows, established here: **the UI never mutates state
//! it then renders.** An edit goes in, an update comes out, and the new view
//! arrives on the watch channel. Local and remote edits therefore travel exactly
//! the same path, which is why the two-device case cannot drift from the one-device
//! case.

use crate::crdt::{Edit, NoteDoc, NoteView};
use crate::crypto::{AuthorKey, DeviceIdentity, DeviceSeed, Envelope, NoteKey};
use crate::error::{CoreError, Result};
use crate::ids::{ItemId, NoteId};
use crate::order;
use crate::store::{Keyring, OpStore};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use tokio::sync::watch;

/// Where the sync cursor is kept in the store's per-note metadata.
///
/// Public because moving a device from one store to another has to carry it:
/// see [`crate::store::copy_store`].
pub const SYNC_CURSOR: &str = "sync";

/// What this device has sent to, and received from, the relay for one note.
///
/// Purely local bookkeeping: it never travels, and another device's copy would be
/// meaningless. It buys two things. `last_seq` makes a relaunch cheap — the client
/// resubscribes from where it left off instead of replaying the note's whole relay
/// log. `sent_upto` is what stops an edit made offline from being re-sent on every
/// launch for ever, which without it is the price of not losing it at all.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncCursor {
    /// The highest relay sequence number applied to this note.
    pub last_seq: i64,
    /// The highest counter of our own envelopes the relay has echoed back.
    /// `None` means the relay has acknowledged nothing from this device.
    pub sent_upto: Option<u64>,
}

/// How long a note's log may get before opening it compacts the log away.
///
/// A number, not a science: 256 envelopes is a few weeks of a household's
/// shopping list and replays in well under a millisecond on the Yrs backend
/// ([ADR 0001](../../docs/adr/0001-crdt.md) measures 1 000 inserts at 10.7 ms).
/// What makes it safe to pick roughly is that the trade is not close. A 20-item
/// list with 256 toggles is a 55 KiB log against a 5 KiB snapshot on Yrs, and
/// opening it costs 18 ms from the log against 0.3 ms from the snapshot —
/// because replay verifies a signature per envelope, at 68 µs each, whichever
/// backend is underneath. `just bench-compaction` measures it, and
/// [ADR 0006](../../docs/adr/0006-compaction.md) reads it.
pub const COMPACT_AFTER: usize = 256;

/// Slice 1 builds checklists. `Text` is here because the design doc's data model
/// has both and the share blob format should not change when text notes land.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoteKind {
    Checklist,
    Text,
}

struct OpenNote<D: NoteDoc> {
    doc: D,
    key: NoteKey,
    /// Our own next counter. Recovered from the log on open, so a restart cannot
    /// reuse one.
    next_counter: u64,
    /// (author, counter) pairs already applied. This is what makes a replayed
    /// envelope a no-op at the core level, independently of the CRDT's own
    /// idempotence.
    seen: HashSet<(AuthorKey, u64)>,
    watch: watch::Sender<NoteView>,
}

pub struct NoteCore<D: NoteDoc, S: OpStore, K: Keyring> {
    identity: DeviceIdentity,
    actor: [u8; 16],
    store: S,
    keyring: K,
    open: HashMap<NoteId, OpenNote<D>>,
}

impl<D: NoteDoc, S: OpStore, K: Keyring> NoteCore<D, S, K> {
    /// Load the device identity from the keyring, generating one on first run.
    pub fn open(store: S, mut keyring: K) -> Result<Self> {
        let seed = match keyring.device_seed()? {
            Some(seed) => seed,
            None => {
                let mut seed = DeviceSeed([0u8; 64]);
                crate::crypto::random_bytes(&mut seed.0);
                keyring.put_device_seed(&seed)?;
                seed
            }
        };
        let identity = DeviceIdentity::from_seed(&seed);
        let mut actor = [0u8; 16];
        actor.copy_from_slice(&identity.public_key().0[..16]);
        Ok(Self {
            identity,
            actor,
            store,
            keyring,
            open: HashMap::new(),
        })
    }

    pub fn device_key(&self) -> AuthorKey {
        self.identity.public_key()
    }

    pub fn identity(&self) -> &DeviceIdentity {
        &self.identity
    }

    // -- lifecycle ----------------------------------------------------------

    pub fn create_note(&mut self, kind: NoteKind) -> Result<NoteId> {
        let id = NoteId::random();
        let key = NoteKey::random();
        self.keyring.put_note_key(id, &key)?;
        let doc = D::create(&self.actor);
        let (tx, _) = watch::channel(doc.view());
        self.open.insert(
            id,
            OpenNote {
                doc,
                key,
                next_counter: 0,
                seen: HashSet::new(),
                watch: tx,
            },
        );
        // `kind` does not change the document shape in Slice 1 (a text note is a
        // checklist with no items); it is recorded so the UI can branch later.
        let _ = kind;
        Ok(id)
    }

    /// Open a note that is already in the store, replaying its snapshot and log.
    /// This is what makes demo step 6 — relaunch with no network — work.
    pub fn open_note(&mut self, id: NoteId) -> Result<NoteView> {
        if let Some(note) = self.open.get(&id) {
            return Ok(note.doc.view());
        }
        let key = self
            .keyring
            .note_key(id)?
            .ok_or_else(|| CoreError::UnknownNote(id.encode()))?;

        let me = self.identity.public_key();
        let mut next_counter = 0u64;

        let mut doc = match self.store.get_snapshot(id)? {
            Some(blob) => {
                let env = Envelope::decode(&blob)?;
                let plain = env.open(&key)?;
                // A snapshot is sealed with a counter from this device's own
                // sequence, which is what lets the counter survive a compaction
                // that removed every envelope it could otherwise be read from.
                if env.author == me {
                    next_counter = env.counter + 1;
                }
                D::load(&self.actor, &plain)?
            }
            None => D::create(&self.actor),
        };

        let mut seen = HashSet::new();
        let mut length = 0usize;
        for raw in self.store.load(id)? {
            length += 1;
            let env = Envelope::decode(&raw)?;
            if env.note_id != id {
                return Err(CoreError::WrongNote {
                    expected: id.encode(),
                    got: env.note_id.encode(),
                });
            }
            let plain = env.open(&key)?;
            if seen.insert((env.author, env.counter)) {
                doc.apply_remote(&plain)?;
            }
            if env.author == me {
                next_counter = next_counter.max(env.counter + 1);
            }
        }

        let view = doc.view();
        let (tx, _) = watch::channel(view.clone());
        self.open.insert(
            id,
            OpenNote {
                doc,
                key,
                next_counter,
                seen,
                watch: tx,
            },
        );

        // The one place where opening a note writes to the store, and the right
        // one: the document has just been rebuilt, so the snapshot costs nothing
        // to produce, and the replay that made compaction worth doing has just
        // been paid for the last time.
        if length > COMPACT_AFTER {
            self.compact(id)?;
            return self.view(id);
        }
        Ok(view)
    }

    pub fn view(&self, id: NoteId) -> Result<NoteView> {
        Ok(self.note(id)?.doc.view())
    }

    /// A receiver that yields the current view and then every subsequent one.
    /// The UI renders from this and from nothing else.
    pub fn watch(&self, id: NoteId) -> Result<watch::Receiver<NoteView>> {
        Ok(self.note(id)?.watch.subscribe())
    }

    fn note(&self, id: NoteId) -> Result<&OpenNote<D>> {
        self.open
            .get(&id)
            .ok_or_else(|| CoreError::UnknownNote(id.encode()))
    }

    // -- editing ------------------------------------------------------------

    /// Apply a local edit. Returns the sealed envelope to hand to the relay.
    pub fn apply_local(&mut self, id: NoteId, edit: Edit) -> Result<Vec<u8>> {
        let identity_ref = &self.identity;
        let note = self
            .open
            .get_mut(&id)
            .ok_or_else(|| CoreError::UnknownNote(id.encode()))?;

        let update = note.doc.apply_local(&edit)?;
        let counter = note.next_counter;
        note.next_counter += 1;

        let envelope = Envelope::seal(identity_ref, id, &note.key, counter, &update)?;
        note.seen.insert((envelope.author, counter));
        let encoded = envelope.encode();

        self.store.append(id, &encoded)?;
        let note = self.open.get_mut(&id).expect("just borrowed");
        let _ = note.watch.send(note.doc.view());
        Ok(encoded)
    }

    /// Apply an envelope that arrived from the relay.
    ///
    /// Unknown, tampered, replayed and misrouted envelopes are all rejected here,
    /// before the CRDT sees anything.
    pub fn apply_remote(&mut self, id: NoteId, envelope: &[u8]) -> Result<NoteView> {
        let note = self
            .open
            .get_mut(&id)
            .ok_or_else(|| CoreError::UnknownNote(id.encode()))?;

        let env = Envelope::decode(envelope)?;
        if env.note_id != id {
            return Err(CoreError::WrongNote {
                expected: id.encode(),
                got: env.note_id.encode(),
            });
        }
        // `open` verifies the signature before it decrypts.
        let plain = env.open(&note.key)?;

        if !note.seen.insert((env.author, env.counter)) {
            // Already applied. Persisting it again would grow the log for nothing.
            return Ok(note.doc.view());
        }
        note.doc.apply_remote(&plain)?;
        self.store.append(id, envelope)?;

        let note = self.open.get_mut(&id).expect("just borrowed");
        let view = note.doc.view();
        let _ = note.watch.send(view.clone());
        Ok(view)
    }

    // -- index-level helpers ------------------------------------------------
    //
    // The UI thinks in positions; the CRDT thinks in order keys. The translation
    // lives here so no other layer has to know about fractional indices.

    pub fn set_title(&mut self, id: NoteId, title: impl Into<String>) -> Result<Vec<u8>> {
        self.apply_local(id, Edit::SetTitle(title.into()))
    }

    /// Append an item and return `(item id, envelope)`.
    pub fn add_item(&mut self, id: NoteId, text: impl Into<String>) -> Result<(ItemId, Vec<u8>)> {
        let keys = self.view(id)?.order_keys();
        self.insert_item_at(id, text, keys.len())
    }

    pub fn insert_item_at(
        &mut self,
        id: NoteId,
        text: impl Into<String>,
        index: usize,
    ) -> Result<(ItemId, Vec<u8>)> {
        let keys = self.view(id)?.order_keys();
        let item = ItemId::random();
        let order = order::key_for_index(&keys, index.min(keys.len()));
        let env = self.apply_local(
            id,
            Edit::AddItem {
                id: item,
                text: text.into(),
                order,
            },
        )?;
        Ok((item, env))
    }

    pub fn set_item_text(
        &mut self,
        id: NoteId,
        item: ItemId,
        text: impl Into<String>,
    ) -> Result<Vec<u8>> {
        self.require_item(id, item)?;
        self.apply_local(
            id,
            Edit::SetItemText {
                id: item,
                text: text.into(),
            },
        )
    }

    pub fn set_checked(&mut self, id: NoteId, item: ItemId, checked: bool) -> Result<Vec<u8>> {
        self.require_item(id, item)?;
        self.apply_local(id, Edit::SetChecked { id: item, checked })
    }

    pub fn remove_item(&mut self, id: NoteId, item: ItemId) -> Result<Vec<u8>> {
        self.require_item(id, item)?;
        self.apply_local(id, Edit::RemoveItem { id: item })
    }

    /// Move `item` so that it lands at `to_index` in the current display order.
    pub fn move_item(&mut self, id: NoteId, item: ItemId, to_index: usize) -> Result<Vec<u8>> {
        let view = self.view(id)?;
        let from = view
            .items
            .iter()
            .position(|i| i.id == item)
            .ok_or_else(|| CoreError::UnknownItem(item.encode()))?;

        // Compute the target slot against the list with the item taken out, so
        // "move to index 3" means the same thing whether it moves up or down.
        let mut keys = view.order_keys();
        keys.remove(from);
        let order = order::key_for_index(&keys, to_index.min(keys.len()));
        self.apply_local(id, Edit::MoveItem { id: item, order })
    }

    fn require_item(&self, id: NoteId, item: ItemId) -> Result<()> {
        if self.view(id)?.item(item).is_none() {
            return Err(CoreError::UnknownItem(item.encode()));
        }
        Ok(())
    }

    // -- sharing ------------------------------------------------------------

    /// The blob device B scans or pastes. Slice 3 moves this into an invite link's
    /// fragment without changing the bytes.
    pub fn export_share_blob(&self, id: NoteId) -> Result<String> {
        let note = self.note(id)?;
        Ok(crate::crypto::export_share_blob(&id, &note.key))
    }

    /// Accept a share blob and open an empty document for it. The content arrives
    /// as updates from the relay.
    pub fn import_share_blob(&mut self, blob: &str) -> Result<NoteId> {
        let (id, key) = crate::crypto::import_share_blob(blob)?;
        if self.open.contains_key(&id) {
            return Ok(id);
        }
        self.keyring.put_note_key(id, &key)?;
        let doc = D::create(&self.actor);
        let (tx, _) = watch::channel(doc.view());
        self.open.insert(
            id,
            OpenNote {
                doc,
                key,
                next_counter: 0,
                seen: HashSet::new(),
                watch: tx,
            },
        );
        Ok(id)
    }

    // -- persistence --------------------------------------------------------

    /// Write an encrypted snapshot. Slice 2 uses this to compact the log; Slice 1
    /// writes it so that `open_note` exercises the snapshot path.
    pub fn save_snapshot(&mut self, id: NoteId) -> Result<()> {
        let identity_ref = &self.identity;
        let note = self
            .open
            .get_mut(&id)
            .ok_or_else(|| CoreError::UnknownNote(id.encode()))?;
        let blob = note.doc.snapshot();
        let counter = note.next_counter;
        note.next_counter += 1;
        let env = Envelope::seal(identity_ref, id, &note.key, counter, &blob)?;
        self.store.put_snapshot(id, &env.encode())
    }

    /// Replace a note's log with a snapshot plus whatever the relay has not
    /// acknowledged. Returns how many envelopes were dropped.
    ///
    /// **What compaction may not drop** is the outbox. [ADR 0004](../../docs/adr/0004-sync-engine.md)
    /// derives it from the log rather than keeping it beside one, so this
    /// device's own envelopes are the only copy of any edit the relay has not
    /// confirmed. Two cases follow from that, and both are the same rule:
    ///
    /// * A note that syncs loses its peers' envelopes and its own acknowledged
    ///   ones, which is nearly all of them.
    /// * A note that has never reached a relay keeps its whole log, because that
    ///   log *is* how its content would reach one — and how a guest joining later
    ///   would ever see it. There is no path that sends a snapshot instead.
    ///
    /// The snapshot is written before the log is shortened. A crash in between
    /// leaves a note that replays a log it already has a snapshot of, which costs
    /// milliseconds and changes nothing; the other order would lose edits.
    pub fn compact(&mut self, id: NoteId) -> Result<usize> {
        // Up front, so that compacting a note that is not open fails the same way
        // whether or not there turns out to be anything to drop.
        self.note(id)?;

        let sent_upto = self.sync_cursor(id)?.sent_upto;
        let me = self.identity.public_key();

        let log = self.store.load(id)?;
        let before = log.len();
        let mut keep = Vec::with_capacity(before);
        for raw in log {
            let env = Envelope::decode(&raw)?;
            if env.author == me && sent_upto.is_none_or(|sent| env.counter > sent) {
                keep.push(raw);
            }
        }

        let dropped = before - keep.len();
        if dropped == 0 {
            // Nothing to gain, and a snapshot of a document that is already
            // wholly in its own log is work for its own sake.
            return Ok(0);
        }

        self.save_snapshot(id)?;
        self.store.replace_log(id, &keep)?;
        Ok(dropped)
    }

    /// Re-send the current view to everyone watching, without changing anything.
    ///
    /// This exists so a watcher blocked on the channel can be woken and told to
    /// stop. Without it a watcher parks until the next edit, which may never
    /// come — and anything waiting for it to finish parks too.
    pub fn touch(&self, id: NoteId) -> Result<()> {
        let note = self.note(id)?;
        let _ = note.watch.send(note.doc.view());
        Ok(())
    }

    // -- sync bookkeeping ---------------------------------------------------
    //
    // The sync engine drives these; they are here rather than in the engine
    // because the store is here, and because they have to work for a note that
    // is not open — a relaunch reads the cursor before it decides what to ask
    // the relay for.

    pub fn sync_cursor(&self, id: NoteId) -> Result<SyncCursor> {
        match self.store.get_meta(id, SYNC_CURSOR)? {
            Some(raw) => serde_json::from_slice(&raw)
                .map_err(|e| CoreError::Storage(format!("unreadable sync cursor: {e}"))),
            None => Ok(SyncCursor::default()),
        }
    }

    pub fn set_sync_cursor(&mut self, id: NoteId, cursor: SyncCursor) -> Result<()> {
        let raw = serde_json::to_vec(&cursor)
            .map_err(|e| CoreError::Storage(format!("unwritable sync cursor: {e}")))?;
        self.store.put_meta(id, SYNC_CURSOR, &raw)
    }

    /// Our own envelopes, oldest first, that the relay has not acknowledged.
    ///
    /// This is the outbox, and it is derived rather than kept: the log already
    /// holds every edit this device made, so an app that is killed between an
    /// offline edit and the next connection has lost nothing.
    pub fn unsent(&self, id: NoteId, after: Option<u64>) -> Result<Vec<(u64, Vec<u8>)>> {
        let me = self.identity.public_key();
        let mut out = Vec::new();
        for raw in self.store.load(id)? {
            let env = Envelope::decode(&raw)?;
            if env.author != me {
                continue;
            }
            if after.is_some_and(|sent| env.counter <= sent) {
                continue;
            }
            out.push((env.counter, raw));
        }
        out.sort_by_key(|(counter, _)| *counter);
        Ok(out)
    }

    /// Forget every in-memory document, keeping the store and keyring. Used by
    /// tests to simulate the app being killed.
    pub fn close_all(&mut self) {
        self.open.clear();
    }

    pub fn store(&self) -> &S {
        &self.store
    }
}
