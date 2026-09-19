//! The Flutter-facing API.
//!
//! This is a thin shell over `easynotes-core` and should stay that way: no
//! crypto, no CRDT logic, no policy. Anything that belongs in the core belongs
//! in the core, where `cargo test` can reach it without a Flutter toolchain.
//!
//! Types are mirrored rather than re-exported because flutter_rust_bridge needs
//! to see plain structs it can generate Dart for.
//!
//! Two objects, and the split between them is the shape of the app. A
//! [`Library`] is the device: every note it holds, the relay it syncs them with,
//! and the connections that do it. A [`Note`] is one note open on one screen,
//! and there may be several or none. They share one core behind one lock, so a
//! note edited on a screen and the same note in the grid behind it cannot
//! disagree.

use crate::frb_generated::StreamSink;
use crate::relay::Connection;
use crate::storage::{Pending, Queue};
use easynotes_core::crdt::{Colour, DefaultNoteDoc, Layout};
use easynotes_core::ids::{ItemId, LabelId, NoteId};
use easynotes_core::note::{NoteCore, NoteKind};
use easynotes_core::store::{Keyring, MemoryKeyring, MemoryStore, OpStore};
use flutter_rust_bridge::frb;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::watch;

/// The store and keyring are boxed so one `Library` type serves every platform:
/// SQLite on desktop and mobile, IndexedDB in a browser, and memory for a test
/// or a tab that has neither, without this file changing.
type Core = NoteCore<DefaultNoteDoc, Box<dyn OpStore + Send>, Box<dyn Keyring + Send>>;

/// What a platform offers a core to write into: a log and the keys for it.
type Backing = (Box<dyn OpStore + Send>, Box<dyn Keyring + Send>);

#[frb(init)]
pub fn init_app() {
    flutter_rust_bridge::setup_default_user_utils();
}

// ---------------------------------------------------------------------------
// What Dart sees
// ---------------------------------------------------------------------------

/// One checklist item, as Dart sees it.
pub struct ItemView {
    pub id: String,
    pub text: String,
    pub checked: bool,
    pub order: String,
}

/// One note, as Dart sees it. Items arrive already in display order.
pub struct NoteView {
    pub title: String,
    pub items: Vec<ItemView>,
}

impl From<easynotes_core::crdt::NoteView> for NoteView {
    fn from(v: easynotes_core::crdt::NoteView) -> Self {
        Self {
            title: v.title,
            items: v
                .items
                .into_iter()
                .map(|i| ItemView {
                    id: i.id.encode(),
                    text: i.text,
                    checked: i.checked,
                    order: i.order,
                })
                .collect(),
        }
    }
}

/// One card in the grid: enough to draw a note without opening it.
pub struct NoteSummaryView {
    pub id: String,
    pub title: String,
    pub items: u32,
    pub checked: u32,
    /// The first few items, in display order. A card shows these when the note
    /// has no title, which most shopping lists do not.
    pub preview: Vec<String>,
    /// What this person has done with the note. From the account document, so
    /// none of it is shared with whoever else is on the note.
    ///
    /// Archived and trashed notes are in the list: which of them a screen draws
    /// is the screen's question, and the grid, the archive and the trash are
    /// three answers to it.
    pub colour: Option<NoteColour>,
    pub pinned: bool,
    pub archived: bool,
    pub trashed: bool,
    /// The labels this person has put on the note, sorted. A `Vec` rather than a
    /// set because that is what crosses the bridge; it holds each label once,
    /// which is the document's doing and not this type's.
    ///
    /// Each carries an ID as well as a name, because the name is the thing that
    /// can change: a screen draws the name and acts on the ID, and a rename
    /// landing between the two is then a redraw rather than a lost label.
    pub tags: Vec<LabelView>,
}

/// One label on a note: what it is, and what to call it.
pub struct LabelView {
    pub id: String,
    pub name: String,
}

impl From<easynotes_core::note::NoteSummary> for NoteSummaryView {
    fn from(s: easynotes_core::note::NoteSummary) -> Self {
        Self {
            id: s.id.encode(),
            title: s.title,
            items: s.items as u32,
            checked: s.checked as u32,
            preview: s.preview,
            colour: s.placement.colour.map(Into::into),
            pinned: s.placement.pinned,
            archived: s.placement.archived,
            trashed: s.placement.trashed,
            tags: s
                .labels
                .into_iter()
                .map(|(id, name)| LabelView {
                    id: id.encode(),
                    name,
                })
                .collect(),
        }
    }
}

/// One label, and how many notes carry it.
///
/// The count is what a filter menu shows beside the name. It can be **zero**: a
/// label exists in its own right rather than only through the notes carrying it,
/// so taking a label off the last note leaves the label. That is what an `id`
/// separate from the `name` buys, and it is what makes renaming one possible.
pub struct TagView {
    pub id: String,
    pub name: String,
    pub notes: u32,
}

/// One note a search found, and the line it was found on.
///
/// The line is carried because a card shows four items and a shopping list has
/// twenty: a result with nothing on it to say why it is a result reads as a bug.
pub struct SearchHitView {
    pub note: NoteSummaryView,
    /// The title, or the item, that the query matched.
    pub matched: String,
}

impl From<easynotes_core::note::SearchHit> for SearchHitView {
    fn from(hit: easynotes_core::note::SearchHit) -> Self {
        Self {
            note: hit.summary.into(),
            matched: hit.matched,
        }
    }
}

/// A note's colour, as Dart sees it. A fixed palette rather than a number,
/// because what a colour has to look like is a question for the theme on the
/// device drawing it — the same name is one thing in daylight and another in
/// dark mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoteColour {
    Red,
    Orange,
    Yellow,
    Green,
    Teal,
    Blue,
    Purple,
    Pink,
    Brown,
    Grey,
}

impl From<Colour> for NoteColour {
    fn from(c: Colour) -> Self {
        match c {
            Colour::Red => NoteColour::Red,
            Colour::Orange => NoteColour::Orange,
            Colour::Yellow => NoteColour::Yellow,
            Colour::Green => NoteColour::Green,
            Colour::Teal => NoteColour::Teal,
            Colour::Blue => NoteColour::Blue,
            Colour::Purple => NoteColour::Purple,
            Colour::Pink => NoteColour::Pink,
            Colour::Brown => NoteColour::Brown,
            Colour::Grey => NoteColour::Grey,
        }
    }
}

impl From<NoteColour> for Colour {
    fn from(c: NoteColour) -> Self {
        match c {
            NoteColour::Red => Colour::Red,
            NoteColour::Orange => Colour::Orange,
            NoteColour::Yellow => Colour::Yellow,
            NoteColour::Green => Colour::Green,
            NoteColour::Teal => Colour::Teal,
            NoteColour::Blue => Colour::Blue,
            NoteColour::Purple => Colour::Purple,
            NoteColour::Pink => Colour::Pink,
            NoteColour::Brown => Colour::Brown,
            NoteColour::Grey => Colour::Grey,
        }
    }
}

/// Whether the note list is a masonry grid or one column. A view setting, and
/// so a personal one: it lives in the account document with the placements.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoteLayout {
    Grid,
    List,
}

impl From<Layout> for NoteLayout {
    fn from(l: Layout) -> Self {
        match l {
            Layout::Grid => NoteLayout::Grid,
            Layout::List => NoteLayout::List,
        }
    }
}

impl From<NoteLayout> for Layout {
    fn from(l: NoteLayout) -> Self {
        match l {
            NoteLayout::Grid => Layout::Grid,
            NoteLayout::List => Layout::List,
        }
    }
}

/// Where this note's sync stands, as Dart sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum SyncState {
    /// Not connected. Everything still works; the relay just has not heard it yet.
    Offline,
    Connecting,
    /// Connected, still catching up on what arrived while we were away.
    Online,
    /// Connected and caught up.
    Synced,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyncStatus {
    pub state: SyncState,
    /// Our own edits the relay has not acknowledged yet.
    pub pending: u32,
    pub last_seq: i64,
    /// Why the last attempt failed, if it did. Shown to the user verbatim, so it
    /// is the relay's or the network's words, not ours.
    pub last_error: Option<String>,
}

impl SyncStatus {
    /// What to show before anything has been connected.
    pub(crate) fn offline() -> Self {
        Self {
            state: SyncState::Offline,
            pending: 0,
            last_seq: 0,
            last_error: None,
        }
    }

    /// One status for a device with several notes.
    ///
    /// The state is the *worst* of them, because the question the status button
    /// answers is "is everything I typed somewhere other than this device", and
    /// one note that has not made it is a no. Pending edits add up for the same
    /// reason, and the first error is shown because a device with no network has
    /// the same error for every note and repeating it says nothing.
    fn combined<'a>(each: impl Iterator<Item = &'a SyncStatus>) -> Self {
        let mut seen = false;
        let mut worst = SyncState::Synced;
        let mut pending = 0u32;
        let mut last_seq = 0i64;
        let mut last_error = None;
        for status in each {
            seen = true;
            worst = worst.min(status.state);
            pending = pending.saturating_add(status.pending);
            last_seq = last_seq.max(status.last_seq);
            if last_error.is_none() {
                last_error.clone_from(&status.last_error);
            }
        }
        if !seen {
            return Self::offline();
        }
        Self {
            state: worst,
            pending,
            last_seq,
            last_error,
        }
    }
}

impl From<easynotes_core::sync::SyncStatus> for SyncStatus {
    fn from(s: easynotes_core::sync::SyncStatus) -> Self {
        use easynotes_core::sync::SyncState as Core;
        Self {
            state: match s.state {
                Core::Offline => SyncState::Offline,
                Core::Connecting => SyncState::Connecting,
                Core::Online => SyncState::Online,
                Core::Synced => SyncState::Synced,
            },
            pending: s.pending as u32,
            last_seq: s.last_seq,
            last_error: s.last_error,
        }
    }
}

// ---------------------------------------------------------------------------
// Where sync status goes
// ---------------------------------------------------------------------------

/// One note's status channel, and which connection is allowed to write to it.
struct NoteStatus {
    tx: Arc<watch::Sender<SyncStatus>>,
    /// Bumped every time a connection is started or stopped for this note. A
    /// report from an older one is dropped.
    ///
    /// Without this, replacing a connection is a race the stale engine can win:
    /// it says `Offline` with no error as its task ends, *after* the engine that
    /// replaced it has already said why it could not connect, and the button
    /// then shows a device that is quietly offline for no reason. The epoch is
    /// checked and the value written under one lock, so there is no window.
    epoch: u64,
}

/// Every note's sync status, and the one-line answer for all of them.
///
/// Separate from [`Shared`] on purpose: an engine's forwarder task holds this,
/// and if it held the connections too, a connection would keep alive the task
/// that keeps alive the connection, and a closed library would never let go.
pub(crate) struct Statuses {
    per_note: Mutex<HashMap<NoteId, NoteStatus>>,
    overall: Arc<watch::Sender<SyncStatus>>,
}

impl Statuses {
    fn new() -> Self {
        Self {
            per_note: Mutex::new(HashMap::new()),
            overall: Arc::new(watch::channel(SyncStatus::offline()).0),
        }
    }

    fn entry<T>(&self, note: NoteId, f: impl FnOnce(&mut NoteStatus) -> T) -> T {
        let mut per_note = self.per_note.lock().expect("status lock poisoned");
        f(per_note.entry(note).or_insert_with(|| NoteStatus {
            tx: Arc::new(watch::channel(SyncStatus::offline()).0),
            epoch: 0,
        }))
    }

    /// The same, for a note that must already have a channel. `None` means it
    /// does not — which, since [`Statuses::forget`], means the note is gone.
    fn existing<T>(&self, note: NoteId, f: impl FnOnce(&mut NoteStatus) -> T) -> Option<T> {
        let mut per_note = self.per_note.lock().expect("status lock poisoned");
        per_note.get_mut(&note).map(f)
    }

    /// The channel for one note, created on first use.
    ///
    /// It belongs to the note rather than to a connection, so the UI subscribes
    /// once and keeps seeing the truth across a disconnect, a reconnect, or a
    /// move to a different relay.
    fn channel(&self, note: NoteId) -> Arc<watch::Sender<SyncStatus>> {
        self.entry(note, |status| status.tx.clone())
    }

    /// Start an epoch for a note: the connection about to be made owns its
    /// status, and whatever was there before no longer does.
    fn begin(&self, note: NoteId) -> u64 {
        self.entry(note, |status| {
            status.epoch += 1;
            status.epoch
        })
    }

    /// An engine said something. Tell that note's watchers, then everyone
    /// watching the device as a whole.
    ///
    pub(crate) fn report(&self, note: NoteId, epoch: u64, status: SyncStatus) {
        // `existing` rather than `entry`: a report can arrive for a note that has
        // just been forgotten, from an engine whose task had not yet noticed, and
        // creating a channel for a note that no longer exists would leave an
        // `Offline` in the device's combined status for ever — one deleted note
        // would make the whole device look disconnected.
        let current = self.existing(note, |note| {
            if note.epoch == epoch {
                note.tx.send_replace(status);
            }
            note.epoch
        });
        if current == Some(epoch) {
            self.refresh();
        }
    }

    /// A note is gone for good. Its epoch ends, so anything its engine says from
    /// here is dropped, and its channel goes with it so that the device's status
    /// stops counting a note that does not exist toward what is pending.
    fn forget(&self, note: NoteId) {
        let mut per_note = self.per_note.lock().expect("status lock poisoned");
        per_note.remove(&note);
        drop(per_note);
        self.refresh();
    }

    fn refresh(&self) {
        let per_note = self.per_note.lock().expect("status lock poisoned");
        let each: Vec<SyncStatus> = per_note
            .values()
            .map(|status| status.tx.borrow().clone())
            .collect();
        drop(per_note);
        self.overall.send_replace(SyncStatus::combined(each.iter()));
    }

    /// Nothing is connected any more. Said here rather than left to the engines,
    /// whose last word may not arrive before their tasks are gone — and whose
    /// last word is now ignored, because ending an epoch is what this is.
    fn all_offline(&self) {
        let mut per_note = self.per_note.lock().expect("status lock poisoned");
        for status in per_note.values_mut() {
            status.epoch += 1;
            status.tx.send_replace(SyncStatus::offline());
        }
        drop(per_note);
        self.refresh();
    }
}

// ---------------------------------------------------------------------------
// The device
// ---------------------------------------------------------------------------

/// What every note on this device shares.
pub(crate) struct Shared {
    core: Arc<Mutex<Core>>,
    /// The relay to sync with, once [`Library::connect`] has named one. Kept so
    /// that a note created or joined later can be connected on the spot.
    relay: Mutex<Option<String>>,
    connections: Mutex<HashMap<NoteId, Connection>>,
    statuses: Arc<Statuses>,
}

impl Shared {
    fn locked(&self) -> Result<std::sync::MutexGuard<'_, Core>, String> {
        self.core
            .lock()
            .map_err(|_| "core lock poisoned".to_string())
    }

    /// Start syncing one note, replacing whatever connection it had.
    ///
    /// **One socket per note**, which is what the relay's `/v1/notes/{id}` path
    /// means, and the thing the Slice 2 plan said to reconsider when the note
    /// list landed. It is kept, for now, with its cost written down: a device
    /// with twenty notes opens twenty sockets and does twenty token handshakes
    /// to reach one relay. That is fine for a household and wrong for a power
    /// user, and fixing it is a relay protocol change — a subscription message
    /// carrying the note ID, on one connection — not a client refactor. Slice 3
    /// touches the protocol for invite tokens anyway.
    fn connect_note(&self, note: NoteId) -> Result<(), String> {
        let relay = {
            let relay = self.relay.lock().map_err(|_| "relay lock poisoned")?;
            match relay.as_ref() {
                Some(url) => url.clone(),
                None => return Ok(()),
            }
        };
        let epoch = self.statuses.begin(note);
        let connection = Connection::open(
            self.core.clone(),
            note,
            &relay,
            self.statuses.clone(),
            epoch,
        )?;
        let previous = self
            .connections
            .lock()
            .map_err(|_| "sync lock poisoned".to_string())?
            .insert(note, connection);
        if let Some(previous) = previous {
            previous.shutdown();
        }
        Ok(())
    }

    /// Stop syncing one note because it is about to stop existing.
    ///
    /// Not the same as replacing a connection, which leaves the note's status
    /// channel in place because the note is still there and a screen may still be
    /// watching it. Here there will be no note, so the channel goes too.
    fn disconnect_note(&self, note: NoteId) -> Result<(), String> {
        let connection = self
            .connections
            .lock()
            .map_err(|_| "sync lock poisoned".to_string())?
            .remove(&note);
        if let Some(connection) = connection {
            connection.shutdown();
        }
        self.statuses.forget(note);
        Ok(())
    }

    /// Hand a sealed envelope to the relay, if there is a connection to hand it
    /// to. There being none is not an error: the edit is already in the log, and
    /// the next connection finds it there.
    fn publish(&self, note: NoteId, envelope: Vec<u8>) {
        if let Ok(connections) = self.connections.lock() {
            if let Some(connection) = connections.get(&note) {
                connection.publish(envelope);
            }
        }
    }
}

fn boxed_store(s: impl OpStore + Send + 'static) -> Box<dyn OpStore + Send> {
    Box::new(s)
}

fn boxed_keyring(k: impl Keyring + Send + 'static) -> Box<dyn Keyring + Send> {
    Box::new(k)
}

/// The store a device with a disk gets: SQLite for the log, SQLite for the
/// wrapped keys, and a master key that seals the second one.
///
/// All of it is `easynotes_core::store::open_local`, including the one-time
/// carry-over from the directory of files Slice 1 wrote, so that a `cargo test`
/// with no Flutter in the loop can cover what an app update does to a device
/// that already has notes on it.
#[cfg(not(target_arch = "wasm32"))]
fn on_disk(root: &std::path::Path, master: Vec<u8>) -> Result<Backing, String> {
    let master =
        easynotes_core::crypto::MasterKey::from_slice(&master).map_err(|e| e.to_string())?;
    let (store, keyring) =
        easynotes_core::store::open_local(root, master).map_err(|e| e.to_string())?;
    Ok((boxed_store(store), boxed_keyring(keyring)))
}

/// The master key a device wrote beside its database before the app could reach
/// the OS keychain, if it has one.
///
/// Dart calls this once, when the keychain has nothing: a device that updated
/// into this version is unlocked by that file and by nothing else, so the key
/// has to be adopted rather than replaced. The file is left where it is.
///
/// It hands a master key to Dart, which is the point of item 3b rather than a
/// lapse in it: the key lives in the keychain now, and Dart is what can reach
/// one. What the core keeps is that it never chooses where the key is kept.
#[cfg(not(target_arch = "wasm32"))]
#[frb(sync)]
pub fn master_key_beside_the_database(dir: String) -> Result<Option<Vec<u8>>, String> {
    let path = std::path::PathBuf::from(dir).join("master.key");
    Ok(easynotes_core::store::existing_master_key(path)
        .map_err(|e| e.to_string())?
        .map(|key| key.0.to_vec()))
}

/// A browser has no such file, and no database beside it either.
#[cfg(target_arch = "wasm32")]
#[frb(sync)]
pub fn master_key_beside_the_database(_dir: String) -> Result<Option<Vec<u8>>, String> {
    Ok(None)
}

/// A browser has no disk, and nothing here pretends otherwise: the web build's
/// library is [`Library::open_in_browser`], which is handed the rows a browser
/// kept and hands back the writes it should keep next.
#[cfg(target_arch = "wasm32")]
fn on_disk(_root: &std::path::Path, _master: Vec<u8>) -> Result<Backing, String> {
    Err("this platform has no directory to open; use Library.openInBrowser".to_string())
}

// ---------------------------------------------------------------------------
// The store a browser gets
// ---------------------------------------------------------------------------
//
// IndexedDB is asynchronous and `OpStore` is not, so `easynotes_core::store::web`
// splits the problem: it serves every read from memory and hands every write out
// as a batch of key-value changes for something that can reach a database to
// apply. This is the near side of that — the queue those batches wait in — and
// Dart is the far side.
//
// **Dart and not Rust**, for the reason `master_key_beside_the_database` gives
// about the keychain: the platform is Dart's. There is a sharper reason here
// too. On the web, flutter_rust_bridge runs a `#[frb(sync)]` or `async` method
// on the page's own thread and every other method on a Web Worker, and an
// `IDBDatabase` belongs to the thread that opened it. A handle held in Rust
// would therefore be reachable from some of the methods that write to it and
// not from the others. A queue of plain bytes has no such problem, and Dart
// takes it from the thread it is already on.

/// One row of a browser's object store.
pub struct StorageRow {
    pub key: String,
    pub value: Vec<u8>,
}

/// Which object store a batch is for. The log and the keys are kept apart in a
/// browser as they are in two SQLite files on a device — as far apart as an
/// origin allows, which [ADR 0011](../../../docs/adr/0011-the-web-store.md) is
/// honest about.
pub enum StorageArea {
    Notes,
    Keys,
}

/// What one write does.
pub enum StorageWriteKind {
    Put,
    Delete,
    /// Every key beginning with the given one, which is a single range delete.
    DeletePrefix,
}

/// One write, in the shape an object store takes it.
///
/// Flat — a kind beside a key and a value — rather than an enum carrying its
/// own fields, because flutter_rust_bridge renders the latter with `freezed`,
/// and a code generator and its build step are a lot to add to the app for one
/// three-armed type at an FFI boundary.
pub struct StorageWrite {
    pub kind: StorageWriteKind,
    /// The key, or for a `DeletePrefix` the prefix every key to remove begins
    /// with.
    pub key: String,
    /// What to store, for a `Put`. Empty for the other two — and an empty
    /// *value* is legal, so it is the kind that says which this is.
    pub value: Vec<u8>,
}

/// Writes that must land together or not at all: **one batch is one
/// transaction**.
///
/// A batch is what the core says must not be applied by halves — the two steps
/// of a compaction, or everything a forgotten note had — so a caller that split
/// one across two transactions would reintroduce the crash windows the store is
/// written to avoid.
pub struct StorageBatch {
    pub area: StorageArea,
    pub writes: Vec<StorageWrite>,
}

impl From<&easynotes_core::store::Change> for StorageWrite {
    fn from(change: &easynotes_core::store::Change) -> Self {
        use easynotes_core::store::Change;
        match change {
            Change::Put { key, value } => StorageWrite {
                kind: StorageWriteKind::Put,
                key: key.clone(),
                value: value.clone(),
            },
            Change::Delete { key } => StorageWrite {
                kind: StorageWriteKind::Delete,
                key: key.clone(),
                value: Vec::new(),
            },
            Change::DeletePrefix { prefix } => StorageWrite {
                kind: StorageWriteKind::DeletePrefix,
                key: prefix.clone(),
                value: Vec::new(),
            },
        }
    }
}

// Where the batches wait between the store and Dart is `crate::storage`, which
// is outside this module for the reason `crate::relay` is: everything in here
// becomes Dart, and a queue of batches is not something Dart has any business
// holding a handle to.

fn item_id(s: &str) -> Result<ItemId, String> {
    ItemId::decode(s).map_err(|e| e.to_string())
}

fn note_id(s: &str) -> Result<NoteId, String> {
    NoteId::decode(s).map_err(|e| e.to_string())
}

fn label_id(s: &str) -> Result<LabelId, String> {
    LabelId::decode(s).map_err(|e| e.to_string())
}

/// Every note this device holds.
///
/// One per app. Opening it reads each note's sealed index entry rather than each
/// note — a title is inside an envelope, so a device with no index would have to
/// replay every log to draw a grid, which is what `easynotes_core::index`
/// exists to stop.
pub struct Library {
    shared: Arc<Shared>,
    /// Set by [`Library::stop_watching`]. The watcher tasks check it when they
    /// wake.
    stop: Arc<AtomicBool>,
    /// The writes a browser has yet to be given. Empty and never filled on a
    /// device with a disk, where the store writes for itself.
    pending: Arc<Pending>,
}

impl Library {
    /// Open the notes stored in `dir`, on a device that has a disk, unlocked by
    /// `master_key`.
    ///
    /// `dir` holds the SQLite database and the keyring; `master_key` is the 32
    /// bytes that seal everything in them which is not already an envelope, and
    /// it comes from the OS keychain by way of Dart — the core does not know or
    /// care where it was kept. Slice 1's directory of files is carried into the
    /// database the first time this runs, and a device that has the key in a
    /// file beside it is carried over by
    /// [`master_key_beside_the_database`].
    ///
    /// The wrong key is not a soft failure: nothing in the database opens, and
    /// this says so rather than presenting an empty device to someone whose
    /// notes are right there.
    #[frb(sync)]
    pub fn open(dir: String, master_key: Vec<u8>) -> Result<Library, String> {
        let root = std::path::PathBuf::from(dir);
        let (store, keyring) = on_disk(&root, master_key)?;
        Self::wrap(NoteCore::open(store, keyring).map_err(|e| e.to_string())?)
    }

    /// Notes that live only in memory and are gone when the tab closes.
    ///
    /// This is what the web build uses today. A browser has no filesystem, and
    /// the op log's IndexedDB backing is item 4 of the Slice 2 plan — see
    /// `docs/adr/0002-web-core.md`. Everything above the store is the same code
    /// the native build runs, which is what makes this a stepping stone rather
    /// than a mock.
    #[frb(sync)]
    pub fn open_ephemeral() -> Result<Library, String> {
        Self::wrap(
            NoteCore::open(
                boxed_store(MemoryStore::new()),
                boxed_keyring(MemoryKeyring::new()),
            )
            .map_err(|e| e.to_string())?,
        )
    }

    /// Open the notes a browser is holding for this origin, unlocked by
    /// `master_key`.
    ///
    /// `notes` and `keys` are every row of the two object stores, which the
    /// caller has already read — that read is the one thing about a browser's
    /// storage that has to be waited for, and it happens here, at launch, where
    /// the app is waiting anyway. Afterwards nothing waits: the store answers
    /// from memory and leaves its writes in a queue for
    /// [`Library::take_storage_batches`].
    ///
    /// Not `#[cfg(target_arch = "wasm32")]`, deliberately. There is no browser
    /// in any of this — it is a store, a queue and some rows — so the native
    /// build can open one too, and `app/test/rust_bridge_test.dart` drives the
    /// whole of the web build's persistence on a machine that has no browser on
    /// it. A web-only path is a path nothing tests.
    #[frb(sync)]
    pub fn open_in_browser(
        notes: Vec<StorageRow>,
        keys: Vec<StorageRow>,
        master_key: Vec<u8>,
    ) -> Result<Library, String> {
        use easynotes_core::store::{WebKeyring, WebStore};
        let master = easynotes_core::crypto::MasterKey::from_slice(&master_key)
            .map_err(|e| e.to_string())?;
        let pending = Arc::new(Pending::default());
        let rows = |rows: Vec<StorageRow>| rows.into_iter().map(|r| (r.key, r.value));
        let queue = |area| {
            Box::new(Queue {
                area,
                pending: pending.clone(),
            })
        };
        let store = WebStore::from_rows(master.clone(), queue(StorageArea::Notes), rows(notes))
            .map_err(|e| e.to_string())?;
        let keyring = WebKeyring::from_rows(master, queue(StorageArea::Keys), rows(keys))
            .map_err(|e| e.to_string())?;
        Self::wrap_with(
            NoteCore::open(boxed_store(store), boxed_keyring(keyring))
                .map_err(|e| e.to_string())?,
            pending,
        )
    }

    /// Every write the store has made since this was last called, oldest first,
    /// and forget them.
    ///
    /// The caller is expected to apply them — one batch, one transaction — and
    /// to call this often, because until it does, an edit is in memory and
    /// nowhere else. It is a *pull* rather than a stream for the reason the
    /// section above this gives: the methods that write run on a Web Worker and
    /// the one that reads runs on the page's thread, so what passes between them
    /// has to be bytes behind a lock and nothing that belongs to a thread.
    ///
    /// Taking them and then failing to write them loses them, which is why this
    /// is the only thing that empties the queue: a caller that cannot apply a
    /// batch has already been handed it, and the honest thing for it to do is
    /// say so rather than ask for it again.
    #[frb(sync)]
    pub fn take_storage_batches(&self) -> Vec<StorageBatch> {
        self.pending.take()
    }

    /// How many batches are waiting. Zero on a device with a disk, always.
    #[frb(sync)]
    pub fn unwritten_batches(&self) -> u32 {
        self.pending.len()
    }

    fn wrap(core: Core) -> Result<Library, String> {
        Self::wrap_with(core, Arc::new(Pending::default()))
    }

    fn wrap_with(mut core: Core, pending: Arc<Pending>) -> Result<Library, String> {
        // Everything on the disk, read once, so that the list is populated
        // before the first frame asks for it.
        core.notes().map_err(|e| e.to_string())?;
        Ok(Library {
            shared: Arc::new(Shared {
                core: Arc::new(Mutex::new(core)),
                relay: Mutex::new(None),
                connections: Mutex::new(HashMap::new()),
                statuses: Arc::new(Statuses::new()),
            }),
            stop: Arc::new(AtomicBool::new(false)),
            pending,
        })
    }

    // -- the list -----------------------------------------------------------

    #[frb(sync)]
    pub fn notes(&self) -> Result<Vec<NoteSummaryView>, String> {
        let mut core = self.shared.locked()?;
        Ok(core
            .notes()
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(Into::into)
            .collect())
    }

    /// The note list now, and every version of it after that.
    ///
    /// Every edit reaches it, whether it was made here or arrived from another
    /// device, because both go through the core's one refresh.
    pub async fn watch_notes(&self, sink: StreamSink<Vec<NoteSummaryView>>) -> Result<(), String> {
        let mut updates = { self.shared.locked()?.watch_notes() };
        let stop = self.stop.clone();
        let send = |summaries: Vec<easynotes_core::note::NoteSummary>| {
            sink.add(summaries.into_iter().map(Into::into).collect::<Vec<_>>())
        };
        if send(updates.borrow_and_update().clone()).is_err() {
            return Ok(());
        }
        loop {
            if updates.changed().await.is_err() {
                return Ok(()); // the library was dropped
            }
            if stop.load(Ordering::SeqCst) {
                return Ok(());
            }
            let next = updates.borrow_and_update().clone();
            if send(next).is_err() {
                return Ok(()); // Dart stopped listening
            }
        }
    }

    /// Every note whose text contains all of the query's words.
    ///
    /// Local, and local in the strong sense: it reads the same sealed index the
    /// grid is drawn from, so it works with no network and never asks a relay
    /// anything. Archived and trashed notes come back too, with their placements
    /// on them — which of them to show is the screen's question, as it is for
    /// the list.
    pub fn search(&self, query: String) -> Result<Vec<SearchHitView>, String> {
        let mut core = self.shared.locked()?;
        Ok(core
            .search(&query)
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(Into::into)
            .collect())
    }

    /// Make a new, empty note and return its ID.
    pub fn create_note(&self) -> Result<String, String> {
        let id = {
            let mut core = self.shared.locked()?;
            core.create_note(NoteKind::Checklist)
                .map_err(|e| e.to_string())?
        };
        self.shared.connect_note(id)?;
        Ok(id.encode())
    }

    /// Accept a share blob and add the note it describes to this device.
    ///
    /// An addition, not a replacement — which is the whole difference between
    /// this and Slice 1's one-note join. A blob for a note this device already
    /// has is not an error and does not make a second copy.
    pub fn join(&self, blob: String) -> Result<String, String> {
        let id = {
            let mut core = self.shared.locked()?;
            // Replays whatever this device already holds for the note under the
            // key the blob just supplied, so a second join finds the log that is
            // there rather than putting an empty card over it.
            core.import_share_blob(&blob).map_err(|e| e.to_string())?
        };
        self.shared.connect_note(id)?;
        Ok(id.encode())
    }

    /// A handle to one note, for a screen that is showing it.
    ///
    /// Cheap, and there may be several: they share the library's core, so two
    /// handles to one note are two views of the same document rather than two
    /// documents.
    #[frb(sync)]
    pub fn note(&self, id: String) -> Result<Note, String> {
        let id = note_id(&id)?;
        // Opening is what makes the watch channel and the view real; a note the
        // library listed is already open, and this is what covers one that is
        // not.
        self.shared
            .locked()?
            .open_note(id)
            .map_err(|e| e.to_string())?;
        Ok(Note {
            shared: self.shared.clone(),
            id,
            status: self.shared.statuses.channel(id),
            stop: Arc::new(AtomicBool::new(false)),
        })
    }

    // -- what this person thinks of their notes ------------------------------
    //
    // Placements and settings, from the account document. They are on the
    // library rather than on a note for the reason the relay address is: they
    // belong to the device's view of the whole set, and a note that is not open
    // on any screen can still be pinned from the grid.
    //
    // None of this reaches the note, so none of it reaches whoever else is on
    // it — see `docs/adr/0008-account-document.md`.

    pub fn set_colour(&self, id: String, colour: Option<NoteColour>) -> Result<(), String> {
        let id = note_id(&id)?;
        self.shared
            .locked()?
            .set_colour(id, colour.map(Into::into))
            .map_err(|e| e.to_string())
    }

    pub fn set_pinned(&self, id: String, pinned: bool) -> Result<(), String> {
        let id = note_id(&id)?;
        self.shared
            .locked()?
            .set_pinned(id, pinned)
            .map_err(|e| e.to_string())
    }

    /// Move a note so that it takes `target`'s place in the grid's order.
    ///
    /// Named by the note underneath rather than by an index, because that is what
    /// a drag knows: the grid is filtered to a shelf or a label, and the order it
    /// writes into is over every note this device has. See
    /// `NoteCore::move_note_to` for which side of the target a drop lands on.
    pub fn move_note_to(&self, id: String, target: String) -> Result<(), String> {
        let id = note_id(&id)?;
        let target = note_id(&target)?;
        self.shared
            .locked()?
            .move_note_to(id, target)
            .map_err(|e| e.to_string())
    }

    /// Every label in use on this device, with how many notes carry each.
    ///
    /// Sorted the way a menu wants them, which the core decides: a label is read
    /// by a person, so the order ignores case.
    #[frb(sync)]
    pub fn tags(&self) -> Result<Vec<TagView>, String> {
        Ok(self
            .shared
            .locked()?
            .tags()
            .into_iter()
            .map(|(id, name, notes)| TagView {
                id: id.encode(),
                name,
                notes: notes as u32,
            })
            .collect())
    }

    /// Put a label on a note, and return the label it went on under.
    ///
    /// `null` for something that is not a name — whitespace, or nothing — so a
    /// field submitted empty is a no-op rather than a label called "". The label
    /// that comes back may not be a new one: a name differing only in case from
    /// one this device already has *is* that label, and the UI has to draw the
    /// label that exists rather than the casing that was typed.
    pub fn add_tag(&self, id: String, tag: String) -> Result<Option<String>, String> {
        let id = note_id(&id)?;
        Ok(self
            .shared
            .locked()?
            .add_tag(id, &tag)
            .map_err(|e| e.to_string())?
            .map(|label| label.encode()))
    }

    /// Make a label that no note carries yet, and return it. An existing label
    /// with this name is returned rather than a second one made.
    pub fn make_tag(&self, name: String) -> Result<String, String> {
        Ok(self
            .shared
            .locked()?
            .make_tag(&name)
            .map_err(|e| e.to_string())?
            .encode())
    }

    /// Take a label off a note. Not an error if it was never on it.
    ///
    /// The label itself stays, with one fewer note on it. Throwing the label
    /// away is [`NoteLibrary::forget_tag`], and it is a different gesture on
    /// purpose.
    pub fn remove_tag(&self, id: String, label: String) -> Result<(), String> {
        let id = note_id(&id)?;
        let label = label_id(&label)?;
        self.shared
            .locked()?
            .remove_tag(id, label)
            .map_err(|e| e.to_string())
    }

    /// Rename a label. Every note carrying it keeps it, because a membership
    /// points at the ID and has never mentioned the name.
    ///
    /// Returns the label that survived, which is **not** always the one passed
    /// in: renaming onto a name another label already has merges the two, and
    /// the survivor is the one that already had the name. `null` for a name that
    /// is not a name, or a label this device does not have.
    pub fn rename_tag(&self, label: String, name: String) -> Result<Option<String>, String> {
        let label = label_id(&label)?;
        Ok(self
            .shared
            .locked()?
            .rename_tag(label, &name)
            .map_err(|e| e.to_string())?
            .map(|survivor| survivor.encode()))
    }

    /// Throw a label away: off every note that has it, and gone from the menu.
    /// The notes themselves are untouched.
    pub fn forget_tag(&self, label: String) -> Result<(), String> {
        let label = label_id(&label)?;
        self.shared
            .locked()?
            .forget_tag(label)
            .map_err(|e| e.to_string())
    }

    pub fn set_archived(&self, id: String, archived: bool) -> Result<(), String> {
        let id = note_id(&id)?;
        self.shared
            .locked()?
            .set_archived(id, archived)
            .map_err(|e| e.to_string())
    }

    /// Throw a note away, or take it back out of the trash.
    ///
    /// Reversible, and nothing is destroyed: the note is still on the disk and
    /// still syncing, and what has changed is that this person does not see it.
    /// [`Library::forget_note`] is the irreversible one.
    pub fn set_trashed(&self, id: String, trashed: bool) -> Result<(), String> {
        let id = note_id(&id)?;
        self.shared
            .locked()?
            .set_trashed(id, trashed)
            .map_err(|e| e.to_string())
    }

    /// Forget a note for good: its log, its snapshot, its index entry, its key
    /// and this person's placement of it.
    ///
    /// There is no undo, which is why the UI offers it only from the trash and
    /// only behind a confirmation. It removes **this device's copy**: a note that
    /// has been shared is on the relay and on every guest's device under a key
    /// they hold, and nothing here reaches either. See
    /// `docs/adr/0010-forgetting-a-note.md`.
    pub fn forget_note(&self, id: String) -> Result<(), String> {
        let id = note_id(&id)?;
        self.forget(id)
    }

    /// Forget every note in the trash. Returns how many there were.
    pub fn empty_trash(&self) -> Result<u32, String> {
        // Asked for the list, then told to empty it, rather than one call: every
        // one of these notes has a socket that has to be closed before its rows
        // go, and the core cannot close a socket.
        let trashed = self
            .shared
            .locked()?
            .trashed_notes()
            .map_err(|e| e.to_string())?;
        let mut forgotten = 0u32;
        for id in trashed {
            self.forget(id)?;
            forgotten += 1;
        }
        Ok(forgotten)
    }

    /// The socket first, then the rows.
    ///
    /// The order matters and is not obvious. An envelope that arrives for a note
    /// the core has just forgotten cannot be applied — there is no key — but the
    /// engine still records how far it has read, and that sync cursor is a store
    /// row, and a store row is enough to make the note exist again as far as the
    /// store is concerned. So the engine is asked to stop, and its status channel
    /// removed, before the core is asked to forget anything.
    ///
    /// It closes the window rather than the possibility: the engine's task may
    /// already be mid-delivery when the shutdown is queued. What that can leave
    /// is one sealed cursor row for a note with no key, which holds no content
    /// and draws no card — `adopt_unplaced` in the core is what keeps it from
    /// turning into anything more.
    fn forget(&self, id: NoteId) -> Result<(), String> {
        self.shared.disconnect_note(id)?;
        self.shared
            .locked()?
            .forget_note(id)
            .map_err(|e| e.to_string())
    }

    /// Whether the grid is a masonry or one column, as this person last left it.
    #[frb(sync)]
    pub fn layout(&self) -> Result<NoteLayout, String> {
        Ok(self.shared.locked()?.layout().into())
    }

    pub fn set_layout(&self, layout: NoteLayout) -> Result<(), String> {
        self.shared
            .locked()?
            .set_layout(layout.into())
            .map_err(|e| e.to_string())
    }

    // -- sync ---------------------------------------------------------------

    /// Start syncing every note with the relay at `relay` (`http://host:port`
    /// or `wss://host`), and every note made or joined after this.
    ///
    /// Returns as soon as the engines are started, not when they are connected:
    /// there may be no network, and the app is not allowed to care. Watch
    /// [`Library::watch_sync`] to find out how it goes. Calling this again moves
    /// the device to a different relay.
    pub fn connect(&self, relay: String) -> Result<(), String> {
        *self
            .shared
            .relay
            .lock()
            .map_err(|_| "relay lock poisoned".to_string())? = Some(relay);
        let notes: Vec<NoteId> = {
            let mut core = self.shared.locked()?;
            core.notes()
                .map_err(|e| e.to_string())?
                .into_iter()
                .map(|s| s.id)
                .collect()
        };
        for note in notes {
            self.shared.connect_note(note)?;
        }
        Ok(())
    }

    /// Stop syncing everything. Edits still work and still queue; nothing is
    /// lost.
    pub fn disconnect(&self) -> Result<(), String> {
        *self
            .shared
            .relay
            .lock()
            .map_err(|_| "relay lock poisoned".to_string())? = None;
        let connections: Vec<Connection> = {
            let mut open = self
                .shared
                .connections
                .lock()
                .map_err(|_| "sync lock poisoned".to_string())?;
            open.drain().map(|(_, connection)| connection).collect()
        };
        for connection in connections {
            connection.shutdown();
        }
        self.shared.statuses.all_offline();
        Ok(())
    }

    /// Where this device stands with the relay, over all its notes.
    #[frb(sync)]
    pub fn sync_status(&self) -> SyncStatus {
        self.shared.statuses.overall.borrow().clone()
    }

    pub async fn watch_sync(&self, sink: StreamSink<SyncStatus>) -> Result<(), String> {
        watch_status(
            self.shared.statuses.overall.subscribe(),
            self.stop.clone(),
            sink,
        )
        .await
    }

    /// End the watchers started above and let their tasks finish.
    ///
    /// Necessary rather than tidy: a watcher is parked on a channel, so without
    /// a wake it waits forever, the task leaks, and anything waiting for the
    /// stream to close — such as Dart cancelling its subscription — waits
    /// forever too.
    pub fn stop_watching(&self) -> Result<(), String> {
        self.stop.store(true, Ordering::SeqCst);
        let overall = self.shared.statuses.overall.borrow().clone();
        self.shared.statuses.overall.send_replace(overall);
        // The note list watcher is parked on its own channel and needs the same
        // wake. Re-sending the list it already has changes nothing and moves it.
        self.shared.locked()?.touch_notes();
        Ok(())
    }
}

/// One open note.
///
/// `Mutex` on the core because the bridge hands Dart a shared reference and
/// dispatches calls onto a worker pool, so two edits can arrive on different
/// threads. The core is `Send` (there is a test in `core/tests/convergence.rs`
/// that keeps it so), and an edit is fast enough that a lock is not worth
/// optimising away.
pub struct Note {
    shared: Arc<Shared>,
    id: NoteId,
    /// This note's sync status, for the life of the note rather than of one
    /// connection.
    status: Arc<watch::Sender<SyncStatus>>,
    stop: Arc<AtomicBool>,
}

impl Note {
    #[frb(sync)]
    pub fn id(&self) -> String {
        self.id.encode()
    }

    #[frb(sync)]
    pub fn view(&self) -> Result<NoteView, String> {
        let core = self.shared.locked()?;
        core.view(self.id)
            .map(Into::into)
            .map_err(|e| e.to_string())
    }

    /// Every view from now on, starting with the current one.
    ///
    /// This is the only way the UI learns anything. A local edit and an edit
    /// arriving from another device both come out here, which is what keeps the
    /// one-device and two-device cases identical.
    pub async fn watch(&self, sink: StreamSink<NoteView>) -> Result<(), String> {
        let mut rx = {
            self.shared
                .locked()?
                .watch(self.id)
                .map_err(|e| e.to_string())?
        };
        let stop = self.stop.clone();

        // An async task rather than a thread, and not as a style preference:
        // `std::thread::spawn` is unsupported on `wasm32-unknown-unknown` and
        // panics with `operation not supported on this platform`, which took the
        // whole web app down on the first frame. Awaiting works on both targets,
        // and the bridge drives this future for the life of the stream.
        //
        // `changed()` is runtime-agnostic, so nothing polls and an idle watcher
        // costs nothing.
        if sink.add(rx.borrow_and_update().clone().into()).is_err() {
            return Ok(());
        }
        loop {
            if rx.changed().await.is_err() {
                return Ok(()); // the library was dropped
            }
            if stop.load(Ordering::SeqCst) {
                return Ok(());
            }
            let view = rx.borrow_and_update().clone();
            if sink.add(view.into()).is_err() {
                return Ok(()); // Dart stopped listening
            }
        }
    }

    /// Where this note stands with the relay.
    #[frb(sync)]
    pub fn sync_status(&self) -> SyncStatus {
        self.status.borrow().clone()
    }

    pub async fn watch_sync(&self, sink: StreamSink<SyncStatus>) -> Result<(), String> {
        watch_status(self.status.subscribe(), self.stop.clone(), sink).await
    }

    /// End this handle's watchers and let their tasks finish. The note stays
    /// open in the library, because another screen may be showing it.
    pub fn stop_watching(&self) -> Result<(), String> {
        self.stop.store(true, Ordering::SeqCst);
        // The clone is bound first on purpose: `borrow()` holds a read guard
        // until the end of the statement, and a send inside that statement would
        // wait for a lock the same expression is holding.
        let unchanged = self.status.borrow().clone();
        self.status.send_replace(unchanged);
        let core = self.shared.locked()?;
        core.touch(self.id).map_err(|e| e.to_string())
    }

    /// The blob another device pastes to join. Slice 3 turns it into an invite
    /// link's fragment without changing the bytes.
    #[frb(sync)]
    pub fn share_blob(&self) -> Result<String, String> {
        let core = self.shared.locked()?;
        core.export_share_blob(self.id).map_err(|e| e.to_string())
    }

    // -- edits --------------------------------------------------------------
    //
    // Each one applies the edit locally and hands the sealed envelope to the
    // relay. The local half is what the UI sees, and it never waits for the
    // remote half: the edit is in the log before `publish` is called, and stays
    // there whether or not there is a network.

    pub fn set_title(&self, title: String) -> Result<(), String> {
        let envelope = {
            let mut core = self.shared.locked()?;
            core.set_title(self.id, title).map_err(|e| e.to_string())?
        };
        self.shared.publish(self.id, envelope);
        Ok(())
    }

    pub fn add_item(&self, text: String) -> Result<String, String> {
        let (id, envelope) = {
            let mut core = self.shared.locked()?;
            core.add_item(self.id, text).map_err(|e| e.to_string())?
        };
        self.shared.publish(self.id, envelope);
        Ok(id.encode())
    }

    pub fn set_item_text(&self, item: String, text: String) -> Result<(), String> {
        let envelope = {
            let mut core = self.shared.locked()?;
            core.set_item_text(self.id, item_id(&item)?, text)
                .map_err(|e| e.to_string())?
        };
        self.shared.publish(self.id, envelope);
        Ok(())
    }

    pub fn set_checked(&self, item: String, checked: bool) -> Result<(), String> {
        let envelope = {
            let mut core = self.shared.locked()?;
            core.set_checked(self.id, item_id(&item)?, checked)
                .map_err(|e| e.to_string())?
        };
        self.shared.publish(self.id, envelope);
        Ok(())
    }

    pub fn move_item(&self, item: String, to_index: u32) -> Result<(), String> {
        let envelope = {
            let mut core = self.shared.locked()?;
            core.move_item(self.id, item_id(&item)?, to_index as usize)
                .map_err(|e| e.to_string())?
        };
        self.shared.publish(self.id, envelope);
        Ok(())
    }

    pub fn remove_item(&self, item: String) -> Result<(), String> {
        let envelope = {
            let mut core = self.shared.locked()?;
            core.remove_item(self.id, item_id(&item)?)
                .map_err(|e| e.to_string())?
        };
        self.shared.publish(self.id, envelope);
        Ok(())
    }
}

/// The status now, and every change to it, until Dart stops listening or
/// `stop_watching` says to. Shared by the library's status and each note's,
/// which are the same loop over different channels.
async fn watch_status(
    mut updates: watch::Receiver<SyncStatus>,
    stop: Arc<AtomicBool>,
    sink: StreamSink<SyncStatus>,
) -> Result<(), String> {
    if sink.add(updates.borrow_and_update().clone()).is_err() {
        return Ok(());
    }
    loop {
        if updates.changed().await.is_err() {
            return Ok(());
        }
        if stop.load(Ordering::SeqCst) {
            return Ok(());
        }
        if sink.add(updates.borrow_and_update().clone()).is_err() {
            return Ok(());
        }
    }
}
