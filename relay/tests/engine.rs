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
use easynotes_core::sync::{Pacing, RelayEndpoint, SyncEngine, SyncHandle, SyncState};
use easynotes_relay::{Limits, MemoryStore as RelayLog, Relay, RelayConfig, RelayStore};
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

    /// An owner who can share: an account and a name.
    fn sharing_owner(name: &'static str) -> Self {
        let mut core: Core = NoteCore::open(MemoryStore::new(), MemoryKeyring::new()).unwrap();
        core.create_account().unwrap();
        core.set_profile_name(name).unwrap();
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
        self.connect_paced(endpoint, Pacing::default());
    }

    /// Connect with typing held back for `pacing` rather than the app's times,
    /// so that a test can see a pause or a cap without waiting ten seconds.
    fn connect_paced(&mut self, endpoint: &RelayEndpoint, pacing: Pacing) {
        let (engine, handle) =
            SyncEngine::start_paced(self.core.clone(), self.note, endpoint.clone(), pacing);
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

    /// Type into an item, as a keystroke does: the envelope may wait for the
    /// ones after it.
    fn type_into(&self, item: ItemId, text: &str) {
        let envelope = self
            .core
            .lock()
            .unwrap()
            .set_item_text(self.note, item, text)
            .unwrap();
        if let Some(handle) = &self.handle {
            handle.publish_soon(envelope);
        }
    }

    /// Type `word` into `item` a letter at a time, after whatever it says.
    fn type_word(&self, item: ItemId, word: &str) {
        let start = self.text_of(item);
        for end in 1..=word.len() {
            self.type_into(item, &format!("{start}{}", &word[..end]));
        }
    }

    fn text_of(&self, item: ItemId) -> String {
        self.core
            .lock()
            .unwrap()
            .view(self.note)
            .unwrap()
            .item(item)
            .map(|i| i.text.clone())
            .unwrap_or_default()
    }

    fn flush(&self) {
        self.handle.as_ref().expect("not connected").flush();
    }

    /// Let a person in without the invite endpoints: the request is opened
    /// and approved in the core, and the engine publishes the package.
    fn let_in(&self, guest: &mut Core) {
        let mut core = self.core.lock().unwrap();
        let outgoing = core.new_invite(self.note).unwrap();
        core.remember_invite(&outgoing).unwrap();
        let sealed = guest.seal_join_request(&outgoing.secret).unwrap();
        let request = easynotes_core::invite::open_request(&outgoing.secret, &sealed).unwrap();
        core.approve(outgoing.secret.id(), "req", &request).unwrap();
        drop(core);
        self.handle.as_ref().expect("not connected").settle();
    }

    /// Remove everybody but the owner, and let the engine make it so.
    fn stop_sharing(&self) {
        self.core.lock().unwrap().stop_sharing(self.note).unwrap();
        self.handle.as_ref().expect("not connected").settle();
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

/// Wait until the relay holds exactly `n` envelopes for `note`.
async fn wait_for_head(log: &RelayLog, note: NoteId, n: i64) {
    let deadline = tokio::time::Instant::now() + PATIENCE;
    let mut head = 0;
    while tokio::time::Instant::now() < deadline {
        head = log.head(&note.encode()).await.unwrap();
        if head == n {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the relay never held {n} envelopes; it has {head}");
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

    // Three edits, two envelopes: the reconnection sent only what was
    // outstanding, and folded the two offline edits into one on the way.
    assert_eq!(log.head(&a.note.encode()).await.unwrap(), 2);
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

        // Made before the handshake finished, so they were in the log when the
        // connection read its outbox, and went as one envelope.
        let stored = log.head(&a.note.encode()).await.unwrap();
        assert_eq!(stored, 1, "the relay should have the three edits, folded");
        assert_eq!(
            a.log_len(),
            1,
            "the log should hold the fold, not the edits"
        );

        a.relaunch();
        a.connect(&endpoint);
        a.wait_until_synced().await;

        // Same list, and the relay learned nothing new.
        assert_eq!(a.texts(), ["milk", "bread", "eggs"]);
        assert_eq!(
            log.head(&a.note.encode()).await.unwrap(),
            1,
            "a relaunch re-sent envelopes the relay already had"
        );
        assert_eq!(
            a.log_len(),
            1,
            "the replayed backlog was appended to the local log again"
        );

        // The cursor is what remembered both facts, and it is in the store. The
        // fold took the next counter after the three edits it replaced.
        let cursor = a.core.lock().unwrap().sync_cursor(a.note).unwrap();
        assert_eq!(cursor.last_seq, 1);
        assert_eq!(cursor.sent_upto, Some(3));
        a.note
    };
    // The note ID is opaque to the relay; this just shows the test used one.
    assert_eq!(note.encode().len(), 22);
}

/// Restoring a relay behind a client's cursor used to drop the echo of its next
/// append, leaving `pending` at one forever. The reset handshake makes the
/// client persist a zero cursor, reconnect, and rebuild the relay from its log.
#[tokio::test]
async fn a_restored_relay_rebuilds_the_outbox_instead_of_leaving_an_edit_sending() {
    let (old_relay, old_log, _) = relay().await;
    let wire = Wire::to(old_relay.addr, "127.0.0.1:0".parse().unwrap()).await;
    let endpoint = wire.endpoint();

    let mut a = App::owner("A");
    a.connect(&endpoint);
    for (seq, item) in ["milk", "bread", "eggs"].into_iter().enumerate() {
        a.add_item(item);
        wait_for_head(&old_log, a.note, (seq + 1) as i64).await;
        a.wait_until_synced().await;
    }
    let old_cursor = a.core.lock().unwrap().sync_cursor(a.note).unwrap();
    assert_eq!(old_cursor.last_seq, 3);

    // The public address survives, but its upstream is a new, empty relay —
    // the shape of restoring an old store behind a stable deployment.
    let address = wire.addr;
    wire.cut();
    drop(wire);
    a.add_item("coffee");
    a.wait_until("an edit waiting behind the cut", || a.pending() > 0)
        .await;

    let (restored_relay, restored_log, _) = relay().await;
    let _wire = Wire::to(restored_relay.addr, address).await;
    a.wait_until_synced().await;

    let cursor = a.core.lock().unwrap().sync_cursor(a.note).unwrap();
    assert_eq!(
        cursor.last_seq,
        restored_log.head(&a.note.encode()).await.unwrap()
    );
    assert!(
        cursor.sent_upto.is_some(),
        "the rebuilt outbox was not acknowledged"
    );

    let mut b = App::guest("B", &a.share_blob());
    b.connect(&endpoint);
    b.wait_for_items(&["milk", "bread", "eggs", "coffee"]).await;
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
        2,
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
    assert_eq!(log.head(&a.note.encode()).await.unwrap(), 1);

    assert_eq!(a.compact(), 1, "an acknowledged log should compact away");
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
        2,
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

// ---------------------------------------------------------------------------
// Holding typing back (ADR 0016)
// ---------------------------------------------------------------------------

/// Pacing for a test: a short pause, and a cap long enough never to be reached
/// unless the test is about the cap.
fn quick() -> Pacing {
    Pacing {
        quiet: Duration::from_millis(300),
        max_hold: Duration::from_secs(60),
    }
}

/// Pacing that never sends typing by itself, so that whatever does send it is
/// the thing under test.
fn never() -> Pacing {
    Pacing {
        quiet: Duration::from_secs(3600),
        max_hold: Duration::from_secs(3600),
    }
}

/// The point of the change: a word typed a letter at a time reaches the relay as
/// one envelope once the typing pauses, and a guest sees the word.
#[tokio::test]
async fn a_typed_word_reaches_the_relay_as_one_envelope() {
    let (_relay, log, endpoint) = relay().await;

    let mut a = App::owner("A");
    a.connect_paced(&endpoint, quick());
    a.add_item("m");
    a.wait_until_synced().await;
    wait_for_head(&log, a.note, 1).await;
    let milk = a.item_called("m");

    a.type_word(milk, "ilk and honey");
    a.wait_until("the typing held back", || a.pending() > 0)
        .await;
    a.wait_until_synced().await;

    wait_for_head(&log, a.note, 2).await;
    assert_eq!(
        a.log_len(),
        2,
        "the log should hold the fold, not the letters"
    );

    let mut b = App::guest("B", &a.share_blob());
    b.connect(&endpoint);
    b.wait_for_items(&["milk and honey"]).await;
}

/// A tick is not typing. It goes the moment it is made, and takes whatever
/// typing was being held with it rather than overtaking it.
#[tokio::test]
async fn a_tick_goes_at_once_and_takes_the_typing_with_it() {
    let (_relay, log, endpoint) = relay().await;

    let mut a = App::owner("A");
    a.connect_paced(&endpoint, never());
    a.add_item("m");
    a.wait_until_synced().await;
    let milk = a.item_called("m");

    let mut b = App::guest("B", &a.share_blob());
    b.connect(&endpoint);
    b.wait_for_items(&["m"]).await;

    a.type_word(milk, "ilk");
    // Held: the relay has only the item.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(log.head(&a.note.encode()).await.unwrap(), 1);

    a.set_checked(milk, true);
    wait_for_head(&log, a.note, 2).await;
    b.wait_for_items(&["milk"]).await;
    let milk_there = b.item_called("milk");
    b.wait_until("A's tick", || b.is_checked(milk_there)).await;
}

/// Typing that never pauses still goes, once the batch has waited as long as
/// any may.
#[tokio::test]
async fn typing_that_never_pauses_still_goes_at_the_cap() {
    let (_relay, log, endpoint) = relay().await;

    let mut a = App::owner("A");
    a.connect_paced(
        &endpoint,
        Pacing {
            quiet: Duration::from_secs(3600),
            max_hold: Duration::from_millis(500),
        },
    );
    a.add_item("");
    a.wait_until_synced().await;
    let item = a.item_called("");

    // A letter every 50 ms for well past the cap: never a pause.
    let mut text = String::new();
    let mut sent_while_typing = false;
    for letter in "abcdefghijklmnopqrstuvwxyz".chars() {
        text.push(letter);
        a.type_into(item, &text);
        tokio::time::sleep(Duration::from_millis(50)).await;
        sent_while_typing |= log.head(&a.note.encode()).await.unwrap() > 1;
    }
    assert!(
        sent_while_typing,
        "nothing reached the relay while the typing went on"
    );
}

/// What the app does going into the background: send what is held now, not
/// after a pause it may not live to see.
#[tokio::test]
async fn a_flush_sends_what_is_held() {
    let (_relay, log, endpoint) = relay().await;

    let mut a = App::owner("A");
    a.connect_paced(&endpoint, never());
    a.add_item("m");
    a.wait_until_synced().await;
    let milk = a.item_called("m");

    a.type_word(milk, "ilk");
    a.wait_until("the typing held back", || a.pending() == 3)
        .await;
    a.flush();
    a.wait_until_synced().await;
    wait_for_head(&log, a.note, 2).await;
}

/// A removal's package already contains held typing. Sending those old-key
/// envelopes after it would hand the removed person what was typed while
/// they were being removed (ADR 0023 §6).
#[tokio::test]
async fn a_removal_supersedes_typing_held_under_the_old_key() {
    let (_relay, log, endpoint) = relay().await;

    let mut a = App::sharing_owner("A");
    a.connect_paced(&endpoint, never());
    a.add_item("m");
    a.wait_until_synced().await;
    let milk = a.item_called("m");
    let mut anna: Core = NoteCore::open(MemoryStore::new(), MemoryKeyring::new()).unwrap();
    anna.create_account().unwrap();
    anna.set_profile_name("Anna").unwrap();
    a.let_in(&mut anna);
    wait_for_head(&log, a.note, 2).await;
    a.wait_until_synced().await;

    a.type_word(milk, "ilk");
    a.wait_until("the typing held back", || a.pending() == 3)
        .await;
    a.stop_sharing();
    wait_for_head(&log, a.note, 3).await;
    a.wait_until_synced().await;

    // The item, the package adding Anna, and the one removing her — and none
    // of the three keystrokes under the key she holds.
    assert_eq!(log.head(&a.note.encode()).await.unwrap(), 3);
    assert_eq!(a.text_of(milk), "milk");
}

/// Held is not lost. The app is killed with typing waiting, and it goes on the
/// next launch — from the log, where it was all along.
#[tokio::test]
async fn typing_held_when_the_app_is_killed_goes_on_the_next_launch() {
    let (_relay, log, endpoint) = relay().await;

    let mut a = App::owner("A");
    a.connect_paced(&endpoint, never());
    a.add_item("m");
    a.wait_until_synced().await;
    let milk = a.item_called("m");

    a.type_word(milk, "ilk");
    a.wait_until("the typing held back", || a.pending() == 3)
        .await;
    assert_eq!(log.head(&a.note.encode()).await.unwrap(), 1);

    a.relaunch();
    a.connect_paced(&endpoint, never());
    a.wait_until_synced().await;
    wait_for_head(&log, a.note, 2).await;

    let mut b = App::guest("B", &a.share_blob());
    b.connect(&endpoint);
    b.wait_for_items(&["milk"]).await;
}

/// A relay that asks for a compaction after `envelopes` compactable envelopes
/// rather than ADR 0024's 1 024, so that a test can see one happen.
async fn crowded_relay(envelopes: u64) -> (Relay, RelayLog, RelayEndpoint) {
    let log = RelayLog::new();
    let relay = Relay::serve_with(
        Arc::new(log.clone()),
        "127.0.0.1:0".parse().unwrap(),
        RelayConfig {
            storage: Limits {
                crowded_envelopes: envelopes,
                ..Limits::default()
            },
            ..RelayConfig::default()
        },
    )
    .await
    .unwrap();
    let endpoint = RelayEndpoint::parse(&relay.http_base()).unwrap();
    (relay, log, endpoint)
}

async fn wait_for_compaction(log: &RelayLog, note: NoteId) {
    let deadline = tokio::time::Instant::now() + PATIENCE;
    while tokio::time::Instant::now() < deadline {
        if log.topic(&note.encode()).await.unwrap().covers > 0 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the relay's log was never compacted");
}

/// ADR 0024 end to end: the relay asks the device that is writing, the device
/// hands it a base, and the relay drops what the base covers. A device that
/// was away the whole time, and one that has never seen the note, both arrive
/// at the same list from what is left.
#[tokio::test]
async fn a_crowded_note_is_compacted_and_every_device_still_converges() {
    let (_relay, log, endpoint) = crowded_relay(8).await;

    let mut a = App::owner("A");
    a.connect(&endpoint);
    a.add_item("item 0");
    a.wait_until_synced().await;

    let mut b = App::guest("B", &a.share_blob());
    b.connect(&endpoint);
    b.wait_for_items(&["item 0"]).await;
    b.disconnect();

    let mut expected = vec!["item 0".to_string()];
    for i in 1..30 {
        let item = format!("item {i}");
        a.add_item(&item);
        expected.push(item);
        a.wait_until_synced().await;
    }
    wait_for_compaction(&log, a.note).await;
    let topic = log.topic(&a.note.encode()).await.unwrap();
    assert!(
        topic.live_envelopes < 20,
        "thirty edits should not all still be on the relay: {topic:?}"
    );
    assert!(
        topic.registered.is_some(),
        "the first compaction registers its key"
    );

    let expected: Vec<&str> = expected.iter().map(String::as_str).collect();
    // B's cursor is from before the base.
    b.connect(&endpoint);
    b.wait_for_items(&expected).await;
    b.wait_until_synced().await;

    let mut c = App::guest("C", &a.share_blob());
    c.connect(&endpoint);
    c.wait_for_items(&expected).await;

    // And the note goes on: an edit after the base reaches everyone.
    c.add_item("after");
    let mut more = expected.clone();
    more.push("after");
    a.wait_for_items(&more).await;
    b.wait_for_items(&more).await;
}

/// The device that compacted does not send its base again on a relaunch, and
/// does not count it as pending: the relay's echo acknowledged it.
#[tokio::test]
async fn a_base_is_acknowledged_like_an_edit() {
    let (_relay, log, endpoint) = crowded_relay(4).await;
    let mut a = App::owner("A");
    a.connect(&endpoint);
    for i in 0..8 {
        a.add_item(&format!("item {i}"));
        a.wait_until_synced().await;
    }
    wait_for_compaction(&log, a.note).await;
    a.wait_until_synced().await;
    let head = log.head(&a.note.encode()).await.unwrap();

    a.relaunch();
    a.connect(&endpoint);
    a.wait_until_synced().await;
    assert_eq!(log.head(&a.note.encode()).await.unwrap(), head);
}

/// ADR 0027 §5 end to end: removing an attachment and asking to reclaim
/// makes a base the relay never asked for, and its live list dooms the
/// removed file's object and keeps the other's.
#[tokio::test]
async fn removing_an_attachment_and_reclaiming_dooms_its_bytes() {
    use easynotes_core::attachment::seal::seal_whole;
    use easynotes_core::attachment::{ContentKey, Media, MediaFormat, Variant};
    use easynotes_core::ids::AttachmentId;
    use easynotes_relay::media::Geometry;
    use easynotes_relay::MediaLimits;

    // Far from crowded, so any compaction here is the reclaim's.
    let (_relay, log, endpoint) = relay().await;
    let mut a = App::owner("A");
    a.connect(&endpoint);
    a.add_item("milk");
    a.wait_until_synced().await;

    let topic = a.note.encode();
    let two_days_ago = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
        - 2 * 24 * 60 * 60;
    let attach = |name: &str| {
        let (id, key) = (AttachmentId::random(), ContentKey::random());
        let (original, _) =
            seal_whole(&key, a.note, id, Variant::Original, &[7u8; 300], 1 << 20).unwrap();
        let media = Media {
            v: 1,
            format: MediaFormat::Png,
            name: name.into(),
            width: Some(4),
            height: Some(3),
            duration_ms: None,
            key: Some(key),
            original,
            thumbnail: None,
        };
        let envelope = a
            .core
            .lock()
            .unwrap()
            .add_attachment(a.note, id, media.clone())
            .unwrap();
        a.publish(envelope);
        (id, media.original)
    };
    let (_, kept) = attach("kept.png");
    let (removed, gone) = attach("gone.png");
    a.wait_until_synced().await;
    // The bytes, as the queue would have left them: committed, and long
    // enough ago that neither is fresh.
    for object in [&kept, &gone] {
        let geometry = Geometry {
            sealed_len: object.sealed_len(),
            chunk_size: object.chunk_size,
        };
        log.reserve_media(
            &topic,
            &object.object.encode(),
            geometry,
            &MediaLimits::default(),
            two_days_ago,
        )
        .await
        .unwrap();
        log.commit_media(&topic, &object.object.encode(), object.digest)
            .await
            .unwrap();
    }

    let envelope = a
        .core
        .lock()
        .unwrap()
        .remove_attachment(a.note, removed)
        .unwrap();
    a.publish(envelope);
    a.handle.as_ref().unwrap().reclaim();
    wait_for_compaction(&log, a.note).await;

    let doomed = |object: &easynotes_core::attachment::MediaRef| {
        let (log, topic, object) = (log.clone(), topic.clone(), object.object.encode());
        async move { log.media(&topic, &object).await.unwrap().unwrap().doomed }
    };
    assert!(
        doomed(&gone).await,
        "the removed file's bytes are not doomed"
    );
    assert!(!doomed(&kept).await, "the kept file's bytes were doomed");
    // And the base covers the removal: nothing this device sent is lost.
    a.wait_until_synced().await;
    let mut b = App::guest("B", &a.share_blob());
    b.connect(&endpoint);
    b.wait_for_items(&["milk"]).await;
    b.wait_until("the kept attachment only", || {
        let view = b.core.lock().unwrap().view(b.note).unwrap();
        view.attachments.len() == 1 && view.attachments[0].media.name == "kept.png"
    })
    .await;
}
