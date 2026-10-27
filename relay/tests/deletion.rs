//! Deleting a note for good, between one person's two devices, through a real
//! relay.
//!
//! `core/tests/deletion.rs` holds the properties with envelopes handed across
//! by the test. Here nothing is handed across: each device has one socket, as
//! the app does, carrying its account document beside its notes, and what
//! arrives is what the engine fetched. That is the only place two things the
//! plan asks for can be seen at all:
//!
//! * **The account catches up before a note's outbox is sent.** A device that
//!   was offline while its other device deleted a note must learn of the
//!   deletion before it publishes what it typed into that note meanwhile.
//! * **Two devices that agree go quiet.** A device enforcing a marker must not
//!   write about it again, or two online devices answer each other for ever
//!   and the relay's log grows without anybody touching anything.
//!
//! [ADR 0022](../../docs/decisions.md#adr-0022) is the design.

mod support;

use easynotes_core::account::RecoveryPhrase;
use easynotes_core::crdt::DefaultNoteDoc;
use easynotes_core::ids::NoteId;
use easynotes_core::note::{NoteCore, NoteKind};
use easynotes_core::store::{Keyring, MemoryKeyring, MemoryStore, OpStore};
use easynotes_core::sync::{RelayEndpoint, SyncDevice, SyncEngine, SyncHandle, SyncState};
use easynotes_relay::{MemoryStore as RelayLog, Relay};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

type Core = NoteCore<DefaultNoteDoc, MemoryStore, MemoryKeyring>;

const PATIENCE: Duration = Duration::from_secs(10);

/// One device: a core, and one socket for everything it syncs.
struct Device {
    name: &'static str,
    core: Arc<Mutex<Core>>,
    sync: SyncDevice,
    handles: Arc<Mutex<HashMap<NoteId, SyncHandle>>>,
}

impl Device {
    fn new(name: &'static str, endpoint: &RelayEndpoint) -> Self {
        Self::open(
            name,
            endpoint,
            NoteCore::open(MemoryStore::new(), MemoryKeyring::new()).unwrap(),
        )
    }

    /// A device around a core that already exists — a relaunch.
    fn open(name: &'static str, endpoint: &RelayEndpoint, mut core: Core) -> Self {
        let handles: Arc<Mutex<HashMap<NoteId, SyncHandle>>> = Arc::default();
        let sink = handles.clone();
        core.on_account_envelope(Box::new(move |id, envelope| {
            if let Some(handle) = sink.lock().unwrap().get(&id) {
                handle.publish(envelope.to_vec());
            }
        }));
        let core = Arc::new(Mutex::new(core));
        let (engine, sync) = SyncEngine::device(core.clone(), endpoint.clone());
        tokio::spawn(engine.run());
        let device = Self {
            name,
            core,
            sync,
            handles,
        };
        device.connect_everything();
        device
    }

    /// The app's `connect_new_documents`: the account first, then every note.
    fn connect_everything(&self) {
        let (account, notes) = {
            let core = self.core.lock().unwrap();
            (core.account_id(), core.syncable().unwrap())
        };
        let mut handles = self.handles.lock().unwrap();
        for id in account.into_iter().chain(notes) {
            handles.entry(id).or_insert_with(|| self.sync.connect(id));
        }
    }

    /// Gone offline: the socket closed, the core kept.
    async fn go_offline(self) -> Core {
        self.sync.shutdown();
        drop(self.handles);
        let mut core = self.core;
        let deadline = tokio::time::Instant::now() + PATIENCE;
        loop {
            match Arc::try_unwrap(core) {
                Ok(core) => return core.into_inner().unwrap(),
                Err(still_shared) => core = still_shared,
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "{}'s engine never let go of its core",
                self.name
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    fn create_account(&self) -> RecoveryPhrase {
        let phrase = self.core.lock().unwrap().create_account().unwrap();
        self.connect_everything();
        phrase
    }

    fn adopt_account(&self, phrase: &RecoveryPhrase) {
        self.core.lock().unwrap().adopt_account(phrase).unwrap();
        self.connect_everything();
    }

    fn create_note(&self) -> NoteId {
        let id = self
            .core
            .lock()
            .unwrap()
            .create_note(NoteKind::Checklist)
            .unwrap();
        self.connect_everything();
        id
    }

    fn add_item(&self, note: NoteId, text: &str) {
        let (_, envelope) = self.core.lock().unwrap().add_item(note, text).unwrap();
        if let Some(handle) = self.handles.lock().unwrap().get(&note) {
            handle.publish(envelope);
        }
    }

    fn delete(&self, note: NoteId) {
        let mut core = self.core.lock().unwrap();
        core.set_trashed(note, true).unwrap();
        core.forget_note(note).unwrap();
    }

    fn texts(&self, note: NoteId) -> Vec<String> {
        match self.core.lock().unwrap().view(note) {
            Ok(view) => view.items.iter().map(|i| i.text.clone()).collect(),
            Err(_) => Vec::new(),
        }
    }

    /// Nothing of the note on this device, and nothing left to send for it.
    fn has_nothing_of(&self, note: NoteId) -> bool {
        let core = self.core.lock().unwrap();
        core.is_deleted_note(note)
            && !core.store().notes().unwrap().contains(&note)
            && core.keyring().note_key(note).unwrap().is_none()
            && core.unsent(note, None).unwrap().is_empty()
            && !core.account().placements.contains_key(&note)
    }

    /// The account document connected and caught up, nothing of ours pending.
    fn account_synced(&self) -> bool {
        let Some(account) = self.core.lock().unwrap().account_id() else {
            return false;
        };
        self.handles
            .lock()
            .unwrap()
            .get(&account)
            .map(|handle| {
                let status = handle.status();
                status.state == SyncState::Synced && status.pending == 0
            })
            .unwrap_or(false)
    }

    async fn wait_until(&self, what: &str, mut ready: impl FnMut() -> bool) {
        let deadline = tokio::time::Instant::now() + PATIENCE;
        while tokio::time::Instant::now() < deadline {
            self.connect_everything();
            if ready() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("{} never reached {what}", self.name);
    }
}

async fn relay() -> (Relay, RelayLog, RelayEndpoint) {
    let log = RelayLog::new();
    let relay = Relay::serve(Arc::new(log.clone()), "127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let endpoint = RelayEndpoint::parse(&relay.http_base()).unwrap();
    (relay, log, endpoint)
}

/// The relay's log stops growing once both devices are synced. Measured over
/// a window long enough for several round trips on loopback.
async fn relay_goes_quiet(log: &RelayLog) {
    let before = log.all_envelopes().len();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        log.all_envelopes().len(),
        before,
        "two devices that agree about a deletion kept writing to each other"
    );
}

/// Both devices online. A deletes, B lets go of everything, and the two of
/// them then have nothing more to say.
#[tokio::test]
async fn a_deletion_reaches_the_other_device_online_and_then_it_is_quiet() {
    let (_relay, log, endpoint) = relay().await;

    let a = Device::new("A", &endpoint);
    let phrase = a.create_account();
    let note = a.create_note();
    let kept = a.create_note();
    a.add_item(note, "milk");
    a.add_item(kept, "drill bits");

    let b = Device::new("B", &endpoint);
    b.adopt_account(&phrase);
    b.wait_until("A's notes", || {
        b.texts(note) == ["milk"] && b.texts(kept) == ["drill bits"]
    })
    .await;

    a.delete(note);
    b.wait_until("nothing of the deleted note", || b.has_nothing_of(note))
        .await;
    assert_eq!(b.texts(kept), ["drill bits"], "the other note is untouched");

    a.wait_until("its account synced", || a.account_synced())
        .await;
    b.wait_until("its account synced", || b.account_synced())
        .await;
    relay_goes_quiet(&log).await;

    // And the note that was kept still syncs, on the same sockets.
    b.add_item(kept, "screws");
    a.wait_until("B's edit to the kept note", || {
        a.texts(kept) == ["drill bits", "screws"]
    })
    .await;
}

/// B is offline, and types into the note, while A deletes it. When B comes back
/// — relaunched, from what it had stored — it learns of the deletion before it
/// sends what it typed, so the relay never receives the edit, and B keeps
/// nothing of the note.
#[tokio::test]
async fn an_offline_device_learns_of_the_deletion_before_it_sends_its_edits() {
    let (_relay, log, endpoint) = relay().await;

    let a = Device::new("A", &endpoint);
    let phrase = a.create_account();
    let note = a.create_note();
    a.add_item(note, "milk");

    let b = Device::new("B", &endpoint);
    b.adopt_account(&phrase);
    b.wait_until("A's note", || b.texts(note) == ["milk"]).await;
    b.wait_until("its account synced", || b.account_synced())
        .await;

    // Offline, B types.
    let mut offline = b.go_offline().await;
    let (_, typed) = offline.add_item(note, "eggs").unwrap();
    assert!(
        !offline.unsent(note, None).unwrap().is_empty(),
        "B has an edit waiting"
    );

    // Meanwhile A deletes.
    a.delete(note);
    a.wait_until("its account synced", || a.account_synced())
        .await;

    // B is killed and launched again, and reconnects.
    let (store, keys) = offline.into_parts();
    let b = Device::open("B", &endpoint, NoteCore::open(store, keys).unwrap());
    b.wait_until("nothing of the deleted note", || b.has_nothing_of(note))
        .await;
    b.wait_until("its account synced", || b.account_synced())
        .await;

    assert!(
        !log.all_envelopes().contains(&typed),
        "the edit B made offline never reached the relay"
    );
    relay_goes_quiet(&log).await;
}
