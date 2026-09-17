//! The Flutter-facing API.
//!
//! This is a thin shell over `easynotes-core` and should stay that way: no
//! crypto, no CRDT logic, no policy. Anything that belongs in the core belongs
//! in the core, where `cargo test` can reach it without a Flutter toolchain.
//!
//! Types are mirrored rather than re-exported because flutter_rust_bridge needs
//! to see plain structs it can generate Dart for.

use crate::frb_generated::StreamSink;
use easynotes_core::crdt::DefaultNoteDoc;
use easynotes_core::ids::{ItemId, NoteId};
use easynotes_core::note::{NoteCore, NoteKind};
use easynotes_core::store::{FileKeyring, FileStore, Keyring, MemoryKeyring, MemoryStore, OpStore};
use flutter_rust_bridge::frb;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// The store and keyring are boxed so one `Note` type serves every platform:
/// the filesystem on desktop and mobile, memory in a browser, and — once Slice 2
/// lands the real local store — IndexedDB, without this file changing.
type Core = NoteCore<DefaultNoteDoc, Box<dyn OpStore + Send>, Box<dyn Keyring + Send>>;

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
}

fn boxed_store(s: impl OpStore + Send + 'static) -> Box<dyn OpStore + Send> {
    Box::new(s)
}

fn boxed_keyring(k: impl Keyring + Send + 'static) -> Box<dyn Keyring + Send> {
    Box::new(k)
}

fn item_id(s: &str) -> Result<ItemId, String> {
    ItemId::decode(s).map_err(|e| e.to_string())
}

impl Note {
    /// Open the note stored in `dir`, creating it on first run.
    ///
    /// Slice 1 has exactly one note and no note list; Slice 2 brings the real
    /// local store and everything above it.
    #[frb(sync)]
    pub fn open(dir: String) -> Result<Note, String> {
        let root = std::path::PathBuf::from(dir);
        let store = FileStore::open(root.join("log")).map_err(|e| e.to_string())?;
        let keyring = FileKeyring::open(root.join("keys")).map_err(|e| e.to_string())?;
        let mut core: Core = NoteCore::open(boxed_store(store), boxed_keyring(keyring))
            .map_err(|e| e.to_string())?;

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

    fn wrap(core: Core, id: NoteId) -> Note {
        Note {
            core: Arc::new(Mutex::new(core)),
            id,
            stop: Arc::new(AtomicBool::new(false)),
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
    // Each returns the sealed envelope, which the caller hands to the relay.
    // Slice 1's app has no relay connection yet, so Dart discards it; the shape
    // is here so that adding sync does not change the API.

    pub fn set_title(&self, title: String) -> Result<(), String> {
        let mut core = self
            .core
            .lock()
            .map_err(|_| "core lock poisoned".to_string())?;
        core.set_title(self.id, title)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    pub fn add_item(&self, text: String) -> Result<String, String> {
        let mut core = self
            .core
            .lock()
            .map_err(|_| "core lock poisoned".to_string())?;
        core.add_item(self.id, text)
            .map(|(id, _)| id.encode())
            .map_err(|e| e.to_string())
    }

    pub fn set_item_text(&self, item: String, text: String) -> Result<(), String> {
        let mut core = self
            .core
            .lock()
            .map_err(|_| "core lock poisoned".to_string())?;
        core.set_item_text(self.id, item_id(&item)?, text)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    pub fn set_checked(&self, item: String, checked: bool) -> Result<(), String> {
        let mut core = self
            .core
            .lock()
            .map_err(|_| "core lock poisoned".to_string())?;
        core.set_checked(self.id, item_id(&item)?, checked)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    pub fn move_item(&self, item: String, to_index: u32) -> Result<(), String> {
        let mut core = self
            .core
            .lock()
            .map_err(|_| "core lock poisoned".to_string())?;
        core.move_item(self.id, item_id(&item)?, to_index as usize)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    pub fn remove_item(&self, item: String) -> Result<(), String> {
        let mut core = self
            .core
            .lock()
            .map_err(|_| "core lock poisoned".to_string())?;
        core.remove_item(self.id, item_id(&item)?)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
}
