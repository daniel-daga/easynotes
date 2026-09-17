//! The sync engine against a real relay over a real socket.
//!
//! `two_devices.rs` proves the *protocol* by driving it by hand. This proves the
//! part the app actually runs: a loop that connects itself, reconnects itself,
//! and decides what still needs sending. Every test here is a thing that happens
//! to a phone — the network goes away mid-edit, the app is killed and relaunched,
//! the relay is restarted — rather than a thing that happens to a socket.

mod support;

use easynotes_core::crdt::DefaultNoteDoc;
use easynotes_core::ids::{ItemId, NoteId};
use easynotes_core::note::{NoteCore, NoteKind};
use easynotes_core::store::{MemoryKeyring, MemoryStore};
use easynotes_core::sync::{RelayEndpoint, SyncEngine, SyncHandle, SyncState};
use easynotes_relay::{MemoryStore as RelayLog, Relay, RelayStore};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

type Core = NoteCore<DefaultNoteDoc, MemoryStore, MemoryKeyring>;

/// How long a test will wait for something to become true before failing.
/// Generous: a slow CI box should make a test slow, not red.
const PATIENCE: Duration = Duration::from_secs(10);

/// One app: a core, and — when it is connected — an engine driving it.
struct App {
    name: &'static str,
    core: Arc<Mutex<Core>>,
    note: NoteId,
    handle: Option<SyncHandle>,
}

impl App {
    fn owner(name: &'static str) -> Self {
        let mut core: Core = NoteCore::open(MemoryStore::new(), MemoryKeyring::new()).unwrap();
        let note = core.create_note(NoteKind::Checklist).unwrap();
        Self::wrap(name, core, note)
    }

    fn guest(name: &'static str, blob: &str) -> Self {
        let mut core: Core = NoteCore::open(MemoryStore::new(), MemoryKeyring::new()).unwrap();
        let note = core.import_share_blob(blob).unwrap();
        Self::wrap(name, core, note)
    }

    fn wrap(name: &'static str, core: Core, note: NoteId) -> Self {
        Self {
            name,
            core: Arc::new(Mutex::new(core)),
            note,
            handle: None,
        }
    }

    fn connect(&mut self, endpoint: &RelayEndpoint) {
        let (engine, handle) = SyncEngine::start(self.core.clone(), self.note, endpoint.clone());
        tokio::spawn(engine.run());
        self.handle = Some(handle);
    }

    /// Close the connection and let the engine's task finish, as closing the note
    /// in the app does.
    fn disconnect(&mut self) {
        if let Some(handle) = self.handle.take() {
            handle.shutdown();
        }
    }

    /// The app being killed and relaunched: the store and the keyring survive,
    /// every decrypted document and everything the engine remembered does not.
    fn relaunch(&mut self) {
        self.disconnect();
        let mut core = self.core.lock().unwrap();
        core.close_all();
        core.open_note(self.note).unwrap();
    }

    fn share_blob(&self) -> String {
        self.core
            .lock()
            .unwrap()
            .export_share_blob(self.note)
            .unwrap()
    }

    fn texts(&self) -> Vec<String> {
        self.core
            .lock()
            .unwrap()
            .view(self.note)
            .unwrap()
            .items
            .iter()
            .map(|i| i.text.clone())
            .collect()
    }

    fn item_called(&self, text: &str) -> ItemId {
        self.core
            .lock()
            .unwrap()
            .view(self.note)
            .unwrap()
            .items
            .iter()
            .find(|i| i.text == text)
            .unwrap_or_else(|| panic!("{} has no item called {text:?}", self.name))
            .id
    }

    fn is_checked(&self, item: ItemId) -> bool {
        self.core
            .lock()
            .unwrap()
            .view(self.note)
            .unwrap()
            .item(item)
            .is_some_and(|i| i.checked)
    }

    /// How many envelopes this device's own log holds. The log is the outbox's
    /// source of truth, so a test that cares about re-sending cares about this.
    fn log_len(&self) -> usize {
        self.core.lock().unwrap().store().len(self.note)
    }

    fn pending(&self) -> usize {
        self.handle.as_ref().map_or(0, |h| h.status().pending)
    }

    /// Throw the log away and keep the snapshot, as opening a long-lived note
    /// does. Returns how many envelopes went.
    fn compact(&self) -> usize {
        self.core.lock().unwrap().compact(self.note).unwrap()
    }

    // -- edits, exactly as the bridge makes them ----------------------------

    fn add_item(&self, text: &str) {
        let (_, envelope) = self.core.lock().unwrap().add_item(self.note, text).unwrap();
        self.publish(envelope);
    }

    fn set_checked(&self, item: ItemId, checked: bool) {
        let envelope = self
            .core
            .lock()
            .unwrap()
            .set_checked(self.note, item, checked)
            .unwrap();
        self.publish(envelope);
    }

    /// Hand the envelope to the engine if there is one. With no engine the edit
    /// is still made and still logged — that is the whole point of local-first —
    /// and the next connection finds it.
    fn publish(&self, envelope: Vec<u8>) {
        if let Some(handle) = &self.handle {
            handle.publish(envelope);
        }
    }

    // -- waiting ------------------------------------------------------------

    async fn wait_for_items(&self, expected: &[&str]) {
        self.wait_until("the expected items", || self.texts() == expected)
            .await;
    }

    /// Connected, caught up, and with nothing of ours left unacknowledged.
    async fn wait_until_synced(&self) {
        self.wait_until("a synced connection", || {
            let status = self.handle.as_ref().expect("not connected").status();
            status.state == SyncState::Synced && status.pending == 0
        })
        .await;
    }

    async fn wait_until(&self, what: &str, mut ready: impl FnMut() -> bool) {
        let deadline = tokio::time::Instant::now() + PATIENCE;
        while tokio::time::Instant::now() < deadline {
            if ready() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!(
            "{} never reached {what}; items are {:?}, status is {:?}",
            self.name,
            self.texts(),
            self.handle.as_ref().map(|h| h.status()),
        );
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

/// The network between the app and the relay, as something a test can unplug.
///
/// Stopping the relay itself would not do: its connection tasks outlive the
/// accept loop, so an already-connected client keeps talking to a relay that is
/// supposedly gone. This owns every socket it makes, so cutting it is what a
/// tunnel or a dropped Wi-Fi link is — the existing connection dies and new ones
/// are refused.
struct Wire {
    addr: SocketAddr,
    accept: tokio::task::AbortHandle,
    connections: Arc<Mutex<Vec<tokio::task::AbortHandle>>>,
}

impl Wire {
    async fn to(upstream: SocketAddr, bind: SocketAddr) -> Self {
        let listener = tokio::net::TcpListener::bind(bind).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let connections: Arc<Mutex<Vec<tokio::task::AbortHandle>>> = Arc::default();

        let held = connections.clone();
        let accept = tokio::spawn(async move {
            while let Ok((mut inbound, _)) = listener.accept().await {
                let task = tokio::spawn(async move {
                    let Ok(mut outbound) = tokio::net::TcpStream::connect(upstream).await else {
                        return;
                    };
                    let _ = tokio::io::copy_bidirectional(&mut inbound, &mut outbound).await;
                });
                held.lock().unwrap().push(task.abort_handle());
            }
        })
        .abort_handle();

        Self {
            addr,
            accept,
            connections,
        }
    }

    fn endpoint(&self) -> RelayEndpoint {
        RelayEndpoint::parse(&format!("http://{}", self.addr)).unwrap()
    }

    /// Unplug it: no new connections, and every open one dies now.
    fn cut(&self) {
        self.accept.abort();
        for connection in self.connections.lock().unwrap().drain(..) {
            connection.abort();
        }
    }
}

impl Drop for Wire {
    fn drop(&mut self) {
        self.cut();
    }
}

/// The demo, but driven by the loop the app runs rather than by the test.
#[tokio::test]
async fn two_apps_stay_in_step_without_being_told_to() {
    let (_relay, _log, endpoint) = relay().await;

    let mut a = App::owner("A");
    a.connect(&endpoint);
    for item in ["milk", "bread", "eggs"] {
        a.add_item(item);
    }

    let mut b = App::guest("B", &a.share_blob());
    b.connect(&endpoint);
    b.wait_for_items(&["milk", "bread", "eggs"]).await;

    // And back the other way, live: nobody pumps anything.
    b.set_checked(b.item_called("bread"), true);
    // Resolved before the wait, not inside it: `item_called` takes the core's
    // lock, and a closure that takes it twice over would deadlock rather than
    // fail.
    let bread = a.item_called("bread");
    a.wait_until("B's check", || a.is_checked(bread)).await;
}

/// Demo step 4 as the app meets it: the network goes away mid-session, the user
/// keeps editing, and the edits arrive when it comes back. Nothing tells the app
/// that either happened — it finds out by trying.
#[tokio::test]
async fn edits_made_while_the_network_is_down_arrive_when_it_returns() {
    let (relay, log, _) = relay().await;
    let wire = Wire::to(relay.addr, "127.0.0.1:0".parse().unwrap()).await;
    let through_the_wire = wire.endpoint();

    let mut a = App::owner("A");
    a.connect(&through_the_wire);
    a.add_item("milk");
    a.wait_until_synced().await;

    wire.cut();
    for item in ["bread", "eggs"] {
        a.add_item(item);
    }
    a.wait_until("an unacknowledged outbox", || a.pending() >= 2)
        .await;
    assert_eq!(
        a.texts(),
        ["milk", "bread", "eggs"],
        "an offline edit must still be an edit"
    );

    // The same address comes back, as a reconnecting phone finds it.
    drop(wire);
    let _wire = Wire::to(relay.addr, through_the_wire_addr(&through_the_wire)).await;
    a.wait_until_synced().await;

    let mut b = App::guest("B", &a.share_blob());
    b.connect(&through_the_wire);
    b.wait_for_items(&["milk", "bread", "eggs"]).await;

    // Three edits, three envelopes: the reconnection re-sent only what was
    // outstanding.
    assert_eq!(log.head(&a.note.encode()).await.unwrap(), 3);
}

fn through_the_wire_addr(endpoint: &RelayEndpoint) -> SocketAddr {
    endpoint
        .http_base
        .trim_start_matches("http://")
        .parse()
        .unwrap()
}

/// An edit made before the app has ever had a connection. The engine's outbox is
/// derived from the log, so there is nothing for it to have missed.
#[tokio::test]
async fn an_edit_made_before_the_first_connection_is_not_lost() {
    let (_relay, _log, endpoint) = relay().await;

    let mut a = App::owner("A");
    a.add_item("milk"); // no engine at all yet
    a.add_item("bread");
    a.connect(&endpoint);
    a.wait_until_synced().await;

    let mut b = App::guest("B", &a.share_blob());
    b.connect(&endpoint);
    b.wait_for_items(&["milk", "bread"]).await;
}

/// The one that needs the cursor. Without a persisted acknowledgement watermark,
/// every relaunch would re-send the whole local log and the relay's copy of the
/// note would grow for ever while nothing about it changed.
#[tokio::test]
async fn a_relaunch_does_not_resend_what_the_relay_already_has() {
    let (_relay, log, endpoint) = relay().await;
    let note = {
        let mut a = App::owner("A");
        a.connect(&endpoint);
        for item in ["milk", "bread", "eggs"] {
            a.add_item(item);
        }
        a.wait_until_synced().await;

        let stored = log.head(&a.note.encode()).await.unwrap();
        assert_eq!(stored, 3, "the relay should have exactly the three edits");
        assert_eq!(a.log_len(), 3);

        a.relaunch();
        a.connect(&endpoint);
        a.wait_until_synced().await;

        // Same list, and the relay learned nothing new.
        assert_eq!(a.texts(), ["milk", "bread", "eggs"]);
        assert_eq!(
            log.head(&a.note.encode()).await.unwrap(),
            3,
            "a relaunch re-sent envelopes the relay already had"
        );
        assert_eq!(
            a.log_len(),
            3,
            "the replayed backlog was appended to the local log again"
        );

        // The cursor is what remembered both facts, and it is in the store.
        let cursor = a.core.lock().unwrap().sync_cursor(a.note).unwrap();
        assert_eq!(cursor.last_seq, 3);
        assert_eq!(cursor.sent_upto, Some(2));
        a.note
    };
    // The note ID is opaque to the relay; this just shows the test used one.
    assert_eq!(note.encode().len(), 22);
}

/// Compaction and the outbox share the log, and compaction runs while the network
/// is down as readily as when it is up. The edits the relay has never seen have to
/// survive it — they exist nowhere else.
#[tokio::test]
async fn a_compacted_note_still_sends_what_the_relay_never_got() {
    let (relay, log, _) = relay().await;
    let wire = Wire::to(relay.addr, "127.0.0.1:0".parse().unwrap()).await;
    let through_the_wire = wire.endpoint();

    let mut a = App::owner("A");
    a.connect(&through_the_wire);
    a.add_item("milk");
    a.wait_until_synced().await;

    wire.cut();
    for item in ["bread", "eggs"] {
        a.add_item(item);
    }
    a.wait_until("an unacknowledged outbox", || a.pending() >= 2)
        .await;

    // The acknowledged envelope goes; the two the relay has never seen stay.
    assert_eq!(a.compact(), 1, "compaction dropped the wrong number");
    assert_eq!(a.log_len(), 2);
    assert_eq!(
        a.texts(),
        ["milk", "bread", "eggs"],
        "compaction changed the list it was compacting"
    );

    drop(wire);
    let _wire = Wire::to(relay.addr, through_the_wire_addr(&through_the_wire)).await;
    a.wait_until_synced().await;

    assert_eq!(
        log.head(&a.note.encode()).await.unwrap(),
        3,
        "the relay did not get the edits that only existed in the compacted log"
    );

    // And a guest who has never seen this note gets all of it, including the
    // envelope this device no longer holds.
    let mut b = App::guest("B", &a.share_blob());
    b.connect(&through_the_wire);
    b.wait_for_items(&["milk", "bread", "eggs"]).await;
}

/// A compacted note is still the same author with the same counters, and a
/// relaunch of one still tells the relay nothing it already knows.
#[tokio::test]
async fn a_relaunch_after_a_compaction_resends_nothing() {
    let (_relay, log, endpoint) = relay().await;

    let mut a = App::owner("A");
    a.connect(&endpoint);
    for item in ["milk", "bread", "eggs"] {
        a.add_item(item);
    }
    a.wait_until_synced().await;
    assert_eq!(log.head(&a.note.encode()).await.unwrap(), 3);

    assert_eq!(a.compact(), 3, "an acknowledged log should compact away");
    assert_eq!(a.log_len(), 0);

    a.relaunch();
    a.connect(&endpoint);
    a.wait_until_synced().await;
    a.add_item("coffee");

    // Waited for on the guest rather than on A's own status: "synced with
    // nothing pending" is also what A looks like in the instant between the edit
    // and the engine noticing it.
    let mut b = App::guest("B", &a.share_blob());
    b.connect(&endpoint);
    b.wait_for_items(&["milk", "bread", "eggs", "coffee"]).await;

    assert_eq!(
        log.head(&a.note.encode()).await.unwrap(),
        4,
        "a relaunched, compacted note re-sent what the relay already had"
    );
}

/// Shutting an engine down stops it talking to the relay, and the app carries on.
#[tokio::test]
async fn disconnecting_leaves_the_app_working() {
    let (_relay, log, endpoint) = relay().await;

    let mut a = App::owner("A");
    a.connect(&endpoint);
    a.add_item("milk");
    a.wait_until_synced().await;
    let note = a.note.encode();

    a.disconnect();
    a.add_item("bread");
    assert_eq!(a.texts(), ["milk", "bread"]);

    // Nothing else reaches the relay, and "bread" is waiting in the log.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(log.head(&note).await.unwrap(), 1);
    let unsent = a.core.lock().unwrap().unsent(a.note, Some(0)).unwrap();
    assert_eq!(
        unsent.len(),
        1,
        "the offline edit should be waiting in the log"
    );
}
