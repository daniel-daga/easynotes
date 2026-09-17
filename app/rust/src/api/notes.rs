//! The Flutter-facing API.
//!
//! This is a thin shell over `easynotes-core` and should stay that way: no
//! crypto, no CRDT logic, no policy. Anything that belongs in the core belongs
//! in the core, where `cargo test` can reach it without a Flutter toolchain.
//!
//! Types are mirrored rather than re-exported because flutter_rust_bridge needs
//! to see plain structs it can generate Dart for.

use crate::frb_generated::StreamSink;
use crate::relay::Connection;
use easynotes_core::crdt::DefaultNoteDoc;
use easynotes_core::ids::{ItemId, NoteId};
use easynotes_core::note::{NoteCore, NoteKind};
use easynotes_core::store::{Keyring, MemoryKeyring, MemoryStore, OpStore};
use flutter_rust_bridge::frb;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::watch;

/// The store and keyring are boxed so one `Note` type serves every platform:
/// SQLite on desktop and mobile, memory in a browser, and — once item 4 of the
/// Slice 2 plan lands — IndexedDB, without this file changing.
type Core = NoteCore<DefaultNoteDoc, Box<dyn OpStore + Send>, Box<dyn Keyring + Send>>;

/// What a platform offers a core to write into: a log and the keys for it.
type Backing = (Box<dyn OpStore + Send>, Box<dyn Keyring + Send>);

#[frb(init)]
pub fn init_app() {
    flutter_rust_bridge::setup_default_user_utils();
}

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

/// Where this note's sync stands, as Dart sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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

/// A handle to one open note.
///
/// `Mutex` because the bridge hands Dart a shared reference and dispatches calls
/// onto a worker pool, so two edits can arrive on different threads. The core is
/// `Send` (there is a test in `core/tests/convergence.rs` that keeps it so), and
/// an edit is fast enough that a lock is not worth optimising away.
pub struct Note {
    core: Arc<Mutex<Core>>,
    id: NoteId,
    /// Set by [`Note::stop_watching`]. The watcher thread checks it every time
    /// it wakes.
    stop: Arc<AtomicBool>,
    /// The relay connection, once [`Note::connect`] has made one.
    sync: Mutex<Option<Connection>>,
    /// Sync status for the life of the note, not of one connection, so that the
    /// UI subscribes once and keeps seeing the truth across a reconnect.
    status: Arc<watch::Sender<SyncStatus>>,
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
/// on the first write: the web build's note is [`Note::open_ephemeral`] until the
/// IndexedDB store lands.
#[cfg(target_arch = "wasm32")]
fn on_disk(_root: &std::path::Path) -> Result<Backing, String> {
    Err("this platform has no local database yet; open the note in memory instead".to_string())
}

fn item_id(s: &str) -> Result<ItemId, String> {
    ItemId::decode(s).map_err(|e| e.to_string())
}

impl Note {
    /// Open the note stored in `dir`, creating it on first run.
    ///
    /// `dir` holds the SQLite database, the keyring and — until the app can reach
    /// the OS keychain — the master key that seals the second. Slice 1's
    /// directory of files is carried into it the first time this runs.
    ///
    /// There is still exactly one note and no note list: that is item 5 of the
    /// Slice 2 plan, and it is what replaces the marker file below.
    #[frb(sync)]
    pub fn open(dir: String) -> Result<Note, String> {
        let root = std::path::PathBuf::from(dir);
        let (store, keyring) = on_disk(&root)?;
        let mut core: Core = NoteCore::open(store, keyring).map_err(|e| e.to_string())?;

        // The note ID is written beside the log so a relaunch reopens the same
        // note. A real note list replaces this in Slice 2.
        let marker = root.join("note-id");
        let id = match std::fs::read_to_string(&marker) {
            Ok(saved) => {
                let id = NoteId::decode(saved.trim()).map_err(|e| e.to_string())?;
                core.open_note(id).map_err(|e| e.to_string())?;
                id
            }
            Err(_) => {
                let id = core
                    .create_note(NoteKind::Checklist)
                    .map_err(|e| e.to_string())?;
                std::fs::write(&marker, id.encode()).map_err(|e| e.to_string())?;
                id
            }
        };

        Ok(Note::wrap(core, id))
    }

    /// A note that lives only in memory and is gone when the tab closes.
    ///
    /// This is what the web build uses today. A browser has no filesystem, and
    /// the op log's IndexedDB backing is M4 work this slice did not finish — see
    /// `docs/adr/0002-web-core.md`. Everything above the store is the same code
    /// the native build runs, which is what makes this a stepping stone rather
    /// than a mock.
    #[frb(sync)]
    pub fn open_ephemeral() -> Result<Note, String> {
        let mut core: Core = NoteCore::open(
            boxed_store(MemoryStore::new()),
            boxed_keyring(MemoryKeyring::new()),
        )
        .map_err(|e| e.to_string())?;
        let id = core
            .create_note(NoteKind::Checklist)
            .map_err(|e| e.to_string())?;
        Ok(Note::wrap(core, id))
    }

    /// Open the note a share blob describes, and make it this device's note.
    ///
    /// The blob carries the note's ID and its key, so this is the join half of
    /// demo step 2. It replaces whatever note the directory pointed at before,
    /// which is all Slice 1's one-note model can mean by "join"; a note list
    /// makes it an addition instead.
    #[frb(sync)]
    pub fn join(dir: String, blob: String) -> Result<Note, String> {
        let root = std::path::PathBuf::from(dir);
        let (store, keyring) = on_disk(&root)?;
        let mut core: Core = NoteCore::open(store, keyring).map_err(|e| e.to_string())?;

        let id = core.import_share_blob(&blob).map_err(|e| e.to_string())?;
        // `import_share_blob` opens an empty document, which is right for a first
        // join and wrong for a second one: this device may already hold the log.
        // Closing and reopening replays whatever is on disk under the key the
        // blob just supplied.
        core.close_all();
        core.open_note(id).map_err(|e| e.to_string())?;

        std::fs::write(root.join("note-id"), id.encode()).map_err(|e| e.to_string())?;
        Ok(Note::wrap(core, id))
    }

    fn wrap(core: Core, id: NoteId) -> Note {
        Note {
            core: Arc::new(Mutex::new(core)),
            id,
            stop: Arc::new(AtomicBool::new(false)),
            sync: Mutex::new(None),
            status: Arc::new(watch::channel(SyncStatus::offline()).0),
        }
    }

    // -- sync ---------------------------------------------------------------

    /// Start syncing this note with the relay at `relay` (`http://host:port` or
    /// `wss://host`).
    ///
    /// Returns as soon as the engine is started, not when it is connected: there
    /// may be no network, and the app is not allowed to care. Watch
    /// [`Note::watch_sync`] to find out how it goes. Calling this again replaces
    /// the connection, which is how the app moves to a different relay.
    pub fn connect(&self, relay: String) -> Result<(), String> {
        let connection = Connection::open(self.core.clone(), self.id, &relay, self.status.clone())?;
        if let Some(previous) = self
            .sync
            .lock()
            .map_err(|_| "sync lock poisoned".to_string())?
            .replace(connection)
        {
            previous.shutdown();
        }
        Ok(())
    }

    /// Stop syncing. Edits still work and still queue; nothing is lost.
    pub fn disconnect(&self) -> Result<(), String> {
        let previous = self
            .sync
            .lock()
            .map_err(|_| "sync lock poisoned".to_string())?
            .take();
        if let Some(connection) = previous {
            connection.shutdown();
        }
        // The engine's own last word may not arrive before its task is gone, so
        // say it here: nothing is connected any more.
        self.status.send_replace(SyncStatus::offline());
        Ok(())
    }

    #[frb(sync)]
    pub fn sync_status(&self) -> SyncStatus {
        self.status.borrow().clone()
    }

    /// The sync status now, and every change to it.
    pub async fn watch_sync(&self, sink: StreamSink<SyncStatus>) -> Result<(), String> {
        let mut updates = self.status.subscribe();
        let stop = self.stop.clone();
        if sink.add(updates.borrow_and_update().clone()).is_err() {
            return Ok(());
        }
        loop {
            if updates.changed().await.is_err() {
                return Ok(()); // the note was dropped
            }
            if stop.load(Ordering::SeqCst) {
                return Ok(());
            }
            if sink.add(updates.borrow_and_update().clone()).is_err() {
                return Ok(()); // Dart stopped listening
            }
        }
    }

    /// Hand a sealed envelope to the relay, if there is a connection to hand it
    /// to. There being none is not an error: the edit is already in the log, and
    /// the next connection finds it there.
    fn publish(&self, envelope: Vec<u8>) {
        if let Ok(sync) = self.sync.lock() {
            if let Some(connection) = sync.as_ref() {
                connection.publish(envelope);
            }
        }
    }

    #[frb(sync)]
    pub fn view(&self) -> Result<NoteView, String> {
        let core = self
            .core
            .lock()
            .map_err(|_| "core lock poisoned".to_string())?;
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
            let core = self
                .core
                .lock()
                .map_err(|_| "core lock poisoned".to_string())?;
            core.watch(self.id).map_err(|e| e.to_string())?
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
                return Ok(()); // the note was dropped
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

    /// End the watcher started by [`Note::watch`] and let its task finish.
    ///
    /// Necessary rather than tidy: the watcher is parked on the view channel, so
    /// without a wake it waits forever, the task leaks, and anything waiting for
    /// the stream to close — such as Dart cancelling its subscription — waits
    /// forever too.
    pub fn stop_watching(&self) -> Result<(), String> {
        self.stop.store(true, Ordering::SeqCst);
        // The same wake for the sync watcher, which is parked on its own channel
        // and would otherwise outlive the screen that asked for it. The clone is
        // bound first on purpose: `borrow()` holds a read guard until the end of
        // the statement, and a send inside that statement would wait for a lock
        // the same expression is holding.
        let unchanged = self.status.borrow().clone();
        self.status.send_replace(unchanged);
        let core = self
            .core
            .lock()
            .map_err(|_| "core lock poisoned".to_string())?;
        core.touch(self.id).map_err(|e| e.to_string())
    }

    /// The blob another device pastes to join. Slice 3 turns it into an invite
    /// link's fragment without changing the bytes.
    #[frb(sync)]
    pub fn share_blob(&self) -> Result<String, String> {
        let core = self
            .core
            .lock()
            .map_err(|_| "core lock poisoned".to_string())?;
        core.export_share_blob(self.id).map_err(|e| e.to_string())
    }

    // -- edits --------------------------------------------------------------
    //
    // Each one applies the edit locally and hands the sealed envelope to the
    // relay. The local half is what the UI sees, and it never waits for the
    // remote half: the edit is in the log before `publish` is called, and stays
    // there whether or not there is a network.

    pub fn set_title(&self, title: String) -> Result<(), String> {
        let mut core = self
            .core
            .lock()
            .map_err(|_| "core lock poisoned".to_string())?;
        let envelope = core.set_title(self.id, title).map_err(|e| e.to_string())?;
        drop(core);
        self.publish(envelope);
        Ok(())
    }

    pub fn add_item(&self, text: String) -> Result<String, String> {
        let mut core = self
            .core
            .lock()
            .map_err(|_| "core lock poisoned".to_string())?;
        let (id, envelope) = core.add_item(self.id, text).map_err(|e| e.to_string())?;
        drop(core);
        self.publish(envelope);
        Ok(id.encode())
    }

    pub fn set_item_text(&self, item: String, text: String) -> Result<(), String> {
        let mut core = self
            .core
            .lock()
            .map_err(|_| "core lock poisoned".to_string())?;
        let envelope = core
            .set_item_text(self.id, item_id(&item)?, text)
            .map_err(|e| e.to_string())?;
        drop(core);
        self.publish(envelope);
        Ok(())
    }

    pub fn set_checked(&self, item: String, checked: bool) -> Result<(), String> {
        let mut core = self
            .core
            .lock()
            .map_err(|_| "core lock poisoned".to_string())?;
        let envelope = core
            .set_checked(self.id, item_id(&item)?, checked)
            .map_err(|e| e.to_string())?;
        drop(core);
        self.publish(envelope);
        Ok(())
    }

    pub fn move_item(&self, item: String, to_index: u32) -> Result<(), String> {
        let mut core = self
            .core
            .lock()
            .map_err(|_| "core lock poisoned".to_string())?;
        let envelope = core
            .move_item(self.id, item_id(&item)?, to_index as usize)
            .map_err(|e| e.to_string())?;
        drop(core);
        self.publish(envelope);
        Ok(())
    }

    pub fn remove_item(&self, item: String) -> Result<(), String> {
        let mut core = self
            .core
            .lock()
            .map_err(|_| "core lock poisoned".to_string())?;
        let envelope = core
            .remove_item(self.id, item_id(&item)?)
            .map_err(|e| e.to_string())?;
        drop(core);
        self.publish(envelope);
        Ok(())
    }
}
