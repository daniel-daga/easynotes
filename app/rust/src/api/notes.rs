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
use easynotes_core::crdt::DefaultNoteDoc;
use easynotes_core::ids::{ItemId, NoteId};
use easynotes_core::note::{NoteCore, NoteKind};
use easynotes_core::store::{Keyring, MemoryKeyring, MemoryStore, OpStore};
use flutter_rust_bridge::frb;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::watch;

/// The store and keyring are boxed so one `Library` type serves every platform:
/// SQLite on desktop and mobile, memory in a browser, and — once item 4 of the
/// Slice 2 plan lands — IndexedDB, without this file changing.
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
}

impl From<easynotes_core::note::NoteSummary> for NoteSummaryView {
    fn from(s: easynotes_core::note::NoteSummary) -> Self {
        Self {
            id: s.id.encode(),
            title: s.title,
            items: s.items as u32,
            checked: s.checked as u32,
            preview: s.preview,
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

#[cfg(not(target_arch = "wasm32"))]
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
    pub(crate) fn report(&self, note: NoteId, epoch: u64, status: SyncStatus) {
        let current = self.entry(note, |note| {
            if note.epoch == epoch {
                note.tx.send_replace(status);
            }
            note.epoch
        });
        if current == epoch {
            self.refresh();
        }
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
fn on_disk(root: &std::path::Path) -> Result<Backing, String> {
    let (store, keyring) = easynotes_core::store::open_local(root).map_err(|e| e.to_string())?;
    Ok((boxed_store(store), boxed_keyring(keyring)))
}

/// A browser has no disk. It says so, rather than opening something that fails
/// on the first write: the web build's library is [`Library::open_ephemeral`]
/// until the IndexedDB store lands.
#[cfg(target_arch = "wasm32")]
fn on_disk(_root: &std::path::Path) -> Result<Backing, String> {
    Err("this platform has no local database yet; open the notes in memory instead".to_string())
}

fn item_id(s: &str) -> Result<ItemId, String> {
    ItemId::decode(s).map_err(|e| e.to_string())
}

fn note_id(s: &str) -> Result<NoteId, String> {
    NoteId::decode(s).map_err(|e| e.to_string())
}

/// Every note this device holds.
///
/// One per app. Opening it opens every note, because a note's title is inside an
/// envelope and listing notes therefore means decrypting them — see
/// `NoteCore::notes`.
pub struct Library {
    shared: Arc<Shared>,
    /// Set by [`Library::stop_watching`]. The watcher tasks check it when they
    /// wake.
    stop: Arc<AtomicBool>,
}

impl Library {
    /// Open the notes stored in `dir`, on a device that has a disk.
    ///
    /// `dir` holds the SQLite database, the keyring and — until the app can
    /// reach the OS keychain — the master key that seals the second. Slice 1's
    /// directory of files is carried into it the first time this runs.
    #[frb(sync)]
    pub fn open(dir: String) -> Result<Library, String> {
        let root = std::path::PathBuf::from(dir);
        let (store, keyring) = on_disk(&root)?;
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

    fn wrap(mut core: Core) -> Result<Library, String> {
        // Everything on the disk, opened once, so that the list is populated
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
            let id = core.import_share_blob(&blob).map_err(|e| e.to_string())?;
            // `import_share_blob` opens an empty document, which is right for a
            // first join and wrong for a second one: this device may already
            // hold the log. Reopening replays whatever is on disk under the key
            // the blob just supplied.
            core.close_all();
            core.notes().map_err(|e| e.to_string())?;
            id
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
