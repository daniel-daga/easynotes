//! One person's two devices, through a real relay.
//!
//! [`engine.rs`](./engine.rs) proves the loop for a note; this proves it for the
//! document that makes two devices one person's. the later milestone's definition of done,
//! point 2, asks that two devices adopted from one recovery phrase agree about a
//! note's contents *and* about its colour, labels, pin and placement.
//! `core/tests/recovery.rs` holds that with the envelopes handed across by the
//! test. Here nothing is handed across: both devices talk to a relay over a
//! socket, and what arrives is what the engine fetched.
//!
//! Two things about the account document are not true of a note, and both are
//! exercised here rather than described:
//!
//! * **It is not a `NoteDoc`.** An envelope for it has to reach
//!   `apply_remote_account`, and the dispatch is the core's
//!   (`SyncPeer for Arc<Mutex<NoteCore>>`), so the engine never learns there are
//!   two kinds of document.
//! * **Note keys are taken when a batch settles.** `Caught` is the only message
//!   that says the backlog is drained, and adopting a key out of the middle of a
//!   replay would keep a key for a note whose end-of-log does not mention it
//!   ([ADR 0013](../../docs/decisions.md#adr-0013) decision 5).

mod support;

use easynotes_core::account::RecoveryPhrase;
use easynotes_core::crdt::{Colour, DefaultNoteDoc};
use easynotes_core::ids::NoteId;
use easynotes_core::note::{NoteCore, NoteKind, NoteSummary};
use easynotes_core::store::{MemoryKeyring, MemoryStore};
use easynotes_core::sync::{RelayEndpoint, SyncEngine, SyncHandle, SyncState};
use easynotes_relay::{Limits, MemoryStore as RelayLog, Relay, RelayConfig, RelayStore};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

type Core = NoteCore<DefaultNoteDoc, MemoryStore, MemoryKeyring>;

const PATIENCE: Duration = Duration::from_secs(10);

/// One device of one person: a core, and an engine for every document it syncs.
///
/// The engines are what the app's `Shared` holds, and the map is keyed the same
/// way — by document ID, with the account document in it beside the notes,
/// because to a relay it is a topic like any other.
struct Device {
    name: &'static str,
    core: Arc<Mutex<Core>>,
    engines: Arc<Mutex<HashMap<NoteId, SyncHandle>>>,
    endpoint: RelayEndpoint,
}

impl Device {
    fn new(name: &'static str, endpoint: &RelayEndpoint) -> Self {
        let mut core: Core = NoteCore::open(MemoryStore::new(), MemoryKeyring::new()).unwrap();
        let engines: Arc<Mutex<HashMap<NoteId, SyncHandle>>> = Arc::default();

        // What the app does at the same moment, for the same reason: an account
        // edit has no envelope to return, so the core hands it to whoever asked
        // for one. Without this a colour set while connected would sit in the log
        // until the next reconnection.
        let sink = engines.clone();
        core.on_account_envelope(Box::new(move |id, envelope| {
            if let Some(handle) = sink.lock().unwrap().get(&id) {
                handle.publish(envelope.to_vec());
            }
        }));

        Self {
            name,
            core: Arc::new(Mutex::new(core)),
            engines,
            endpoint: endpoint.clone(),
        }
    }

    /// Start an engine for everything this device can open and has not connected
    /// yet — its notes, and its account document if it has one.
    ///
    /// The app calls this when a relay is named, when a note is made or joined,
    /// and when the account document settles: adopting keys is what turns an
    /// arriving register into a note this device can sync, and a note that
    /// arrived that way has no card, no log and nothing else to announce it.
    /// Here it is called from the waiting loop below, which is the same thing
    /// with the timing left to the test.
    fn connect_everything(&self) {
        let mut ids = self.core.lock().unwrap().syncable().unwrap();
        ids.extend(self.core.lock().unwrap().account_id());
        for id in ids {
            let mut engines = self.engines.lock().unwrap();
            if engines.contains_key(&id) {
                continue;
            }
            let (engine, handle) = SyncEngine::start(self.core.clone(), id, self.endpoint.clone());
            tokio::spawn(engine.run());
            engines.insert(id, handle);
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
        self.publish(note, envelope);
    }

    fn publish(&self, note: NoteId, envelope: Vec<u8>) {
        if let Some(handle) = self.engines.lock().unwrap().get(&note) {
            handle.publish(envelope);
        }
    }

    fn summary(&self, note: NoteId) -> Option<NoteSummary> {
        self.core
            .lock()
            .unwrap()
            .notes()
            .unwrap()
            .into_iter()
            .find(|s| s.id == note)
    }

    fn texts(&self, note: NoteId) -> Vec<String> {
        match self.core.lock().unwrap().view(note) {
            Ok(view) => view.items.iter().map(|i| i.text.clone()).collect(),
            Err(_) => Vec::new(),
        }
    }

    /// Every engine connected and caught up, with nothing of ours outstanding.
    async fn wait_until_synced(&self) {
        self.wait_until("every document synced", || {
            let engines = self.engines.lock().unwrap();
            !engines.is_empty()
                && engines.values().all(|handle| {
                    let status = handle.status();
                    status.state == SyncState::Synced && status.pending == 0
                })
        })
        .await;
    }

    async fn wait_until(&self, what: &str, mut ready: impl FnMut() -> bool) {
        let deadline = tokio::time::Instant::now() + PATIENCE;
        while tokio::time::Instant::now() < deadline {
            // Before the check rather than after it: a note this device has only
            // just learned the key for needs a connection before it can have
            // anything to be waited for.
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

/// The slice's definition of done, point 2, over a socket: a second device
/// adopted from one phrase ends up with the note *and* with what this person did
/// to it.
#[tokio::test]
async fn a_second_device_adopted_from_a_phrase_gets_the_notes_and_the_placements() {
    let (_relay, _log, endpoint) = relay().await;

    let a = Device::new("A", &endpoint);
    let phrase = a.create_account();
    let note = a.create_note();
    a.add_item(note, "milk");
    a.add_item(note, "bread");
    {
        let mut core = a.core.lock().unwrap();
        core.set_colour(note, Some(Colour::Teal)).unwrap();
        core.set_pinned(note, true).unwrap();
        core.add_tag(note, "shopping").unwrap();
    }
    a.wait_until_synced().await;

    // B has never seen a share blob. Twelve words is all it is given, and the
    // note key comes out of the document those words open.
    let b = Device::new("B", &endpoint);
    b.adopt_account(&phrase);

    b.wait_until("A's note, with A's placements on it", || {
        let Some(summary) = b.summary(note) else {
            return false;
        };
        b.texts(note) == ["milk", "bread"]
            && summary.placement.colour == Some(Colour::Teal)
            && summary.placement.pinned
            && summary.labels.iter().any(|(_, name)| name == "shopping")
    })
    .await;

    // And the order: the grid is a placement like the rest of them, so two
    // devices of one person open on the same arrangement.
    let a_order = a.summary(note).unwrap().placement.order;
    assert_eq!(b.summary(note).unwrap().placement.order, a_order);
}

/// The other direction, which is the half that proves this is sync rather than a
/// restore: B edits, and A — already running — sees it.
#[tokio::test]
async fn either_device_can_edit_and_the_other_follows() {
    let (_relay, _log, endpoint) = relay().await;

    let a = Device::new("A", &endpoint);
    let phrase = a.create_account();
    let note = a.create_note();
    a.add_item(note, "milk");
    a.wait_until_synced().await;

    let b = Device::new("B", &endpoint);
    b.adopt_account(&phrase);
    b.wait_until("A's note", || b.texts(note) == ["milk"]).await;

    b.add_item(note, "bread");
    b.core
        .lock()
        .unwrap()
        .set_colour(note, Some(Colour::Red))
        .unwrap();

    a.wait_until("B's item and B's colour", || {
        a.texts(note) == ["milk", "bread"]
            && a.summary(note)
                .is_some_and(|s| s.placement.colour == Some(Colour::Red))
    })
    .await;
}

/// A note made on the second device after it was adopted reaches the first,
/// key and all — so an account is not a one-way restore of what was there when
/// it was set up.
#[tokio::test]
async fn a_note_made_on_the_second_device_reaches_the_first() {
    let (_relay, _log, endpoint) = relay().await;

    let a = Device::new("A", &endpoint);
    let phrase = a.create_account();
    a.wait_until_synced().await;

    let b = Device::new("B", &endpoint);
    b.adopt_account(&phrase);
    b.wait_until_synced().await;

    let made_on_b = b.create_note();
    b.add_item(made_on_b, "drill bits");

    a.wait_until("the note B made", || a.texts(made_on_b) == ["drill bits"])
        .await;
}

/// ADR 0024 for the account document, whose log is the one that grows with
/// every colour change. The device that writes compacts it with a key its
/// phrase derives, and a device adopted afterwards gets the placements from
/// the base.
#[tokio::test]
async fn the_account_document_compacts_on_the_relay_too() {
    let log = RelayLog::new();
    let relay = Relay::serve_with(
        Arc::new(log.clone()),
        "127.0.0.1:0".parse().unwrap(),
        RelayConfig {
            storage: Limits {
                crowded_envelopes: 8,
                ..Limits::default()
            },
            ..RelayConfig::default()
        },
    )
    .await
    .unwrap();
    let endpoint = RelayEndpoint::parse(&relay.http_base()).unwrap();

    let a = Device::new("A", &endpoint);
    let phrase = a.create_account();
    let note = a.create_note();
    a.add_item(note, "milk");
    a.wait_until_synced().await;
    let account = a.core.lock().unwrap().account_id().unwrap().encode();

    let colours = [Colour::Red, Colour::Teal, Colour::Yellow];
    let mut i = 0;
    let deadline = tokio::time::Instant::now() + PATIENCE;
    while log.topic(&account).await.unwrap().covers == 0 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the account was never compacted: {:?}",
            log.topic(&account).await.unwrap()
        );
        let head = log.head(&account).await.unwrap();
        a.core
            .lock()
            .unwrap()
            .set_colour(note, Some(colours[i % 3]))
            .unwrap();
        i += 1;
        // Until the relay has it. Waiting for "synced" alone would not do:
        // the status says so before the engine has even been handed the edit.
        while log.head(&account).await.unwrap() == head {
            assert!(
                tokio::time::Instant::now() < deadline,
                "a colour never reached the relay"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        a.wait_until_synced().await;
    }
    let last = colours[(i - 1) % 3];
    assert!(log.topic(&account).await.unwrap().registered.is_some());

    let b = Device::new("B", &endpoint);
    b.adopt_account(&phrase);
    b.wait_until("A's note in A's last colour", || {
        // The summary first: it is what opens a note that arrived before its
        // key, and `texts` reads only an open one.
        let Some(summary) = b.summary(note) else {
            return false;
        };
        summary.placement.colour == Some(last) && b.texts(note) == ["milk"]
    })
    .await;

    // And A goes on writing after the base, under counters it has not used.
    a.core
        .lock()
        .unwrap()
        .set_colour(note, Some(Colour::Blue))
        .unwrap();
    b.wait_until("A's next colour", || {
        b.summary(note)
            .is_some_and(|s| s.placement.colour == Some(Colour::Blue))
    })
    .await;
}
