//! One socket per device, against a real relay.
//!
//! `engine.rs` proves the engine keeps every promise it made about one note,
//! and passes unchanged over the device socket. This proves what the device
//! socket adds: however many notes a device has, it opens one socket and does
//! one token handshake; a note can join the socket at any point; one note's
//! long backlog does not hold up another's; and a socket that stops reading
//! loses nothing (ADR 0017).

mod support;

use easynotes_core::crdt::DefaultNoteDoc;
use easynotes_core::ids::NoteId;
use easynotes_core::note::{NoteCore, NoteKind};
use easynotes_core::store::{MemoryKeyring, MemoryStore};
use easynotes_core::sync::{
    authenticate, DeviceSync, Incoming, RelayEndpoint, Routed, SyncDevice, SyncEngine, SyncHandle,
    SyncState,
};
use easynotes_relay::protocol::BACKLOG_CHUNK;
use easynotes_relay::{MemoryStore as RelayLog, Relay, RelayStore};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use support::Tap;

type Core = NoteCore<DefaultNoteDoc, MemoryStore, MemoryKeyring>;

const PATIENCE: Duration = Duration::from_secs(20);

async fn relay() -> (Relay, RelayLog) {
    let log = RelayLog::new();
    let relay = Relay::serve(Arc::new(log.clone()), "127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    (relay, log)
}

fn endpoint(http_base: &str) -> RelayEndpoint {
    RelayEndpoint::parse(http_base).unwrap()
}

/// How many times the client side of the tap asked for `request` — an HTTP
/// request line, which crosses the wire in the clear before any socket is
/// upgraded. WebSocket frames from a client are masked, so a request line
/// cannot turn up inside one by accident.
fn requests(tap: &Tap, request: &str) -> usize {
    let bytes = tap.bytes_to_server();
    let needle = request.as_bytes();
    bytes.windows(needle.len()).filter(|w| *w == needle).count()
}

/// A device as the app runs one: a core, one engine, and a handle per note.
struct Phone {
    core: Arc<Mutex<Core>>,
    device: SyncDevice,
    handles: HashMap<NoteId, SyncHandle>,
}

impl Phone {
    fn new(core: Core, endpoint: RelayEndpoint) -> Self {
        let core = Arc::new(Mutex::new(core));
        let (engine, device) = SyncEngine::device(core.clone(), endpoint);
        tokio::spawn(engine.run());
        Self {
            core,
            device,
            handles: HashMap::new(),
        }
    }

    fn connect(&mut self, note: NoteId) {
        self.handles.insert(note, self.device.connect(note));
    }

    fn add_item(&self, note: NoteId, text: &str) {
        let (_, envelope) = self.core.lock().unwrap().add_item(note, text).unwrap();
        if let Some(handle) = self.handles.get(&note) {
            handle.publish(envelope);
        }
    }

    fn texts(&self, note: NoteId) -> Vec<String> {
        self.core
            .lock()
            .unwrap()
            .view(note)
            .map(|v| v.items.iter().map(|i| i.text.clone()).collect())
            .unwrap_or_default()
    }

    fn synced(&self, note: NoteId) -> bool {
        let status = self.handles[&note].status();
        status.state == SyncState::Synced && status.pending == 0
    }

    async fn wait_until(&self, what: &str, mut ready: impl FnMut(&Self) -> bool) {
        let deadline = tokio::time::Instant::now() + PATIENCE;
        while tokio::time::Instant::now() < deadline {
            if ready(self) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let statuses: Vec<_> = self.handles.values().map(|h| h.status()).collect();
        panic!("never reached {what}; statuses are {statuses:?}");
    }
}

fn blank_core() -> Core {
    NoteCore::open(MemoryStore::new(), MemoryKeyring::new()).unwrap()
}

#[tokio::test]
async fn refresh_marker_follows_the_captured_subscription_head() {
    let (relay, log) = relay().await;
    let (note, envelope) = an_envelope(32);
    log.append(&note.encode(), &envelope).await.unwrap();

    let mut sync = DeviceSync::connect(&relay.ws_base(), &token(&relay).await)
        .await
        .unwrap();
    sync.subscribe(note, 0).await.unwrap();
    loop {
        match tokio::time::timeout(PATIENCE, sync.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap()
        {
            Routed::Note(id, Incoming::Caught { seq: 1 }) if id == note => break,
            Routed::Note(_, Incoming::Envelope { .. }) => {}
            _ => panic!("unexpected catch-up message"),
        }
    }

    sync.refresh("refresh-1").await.unwrap();
    match tokio::time::timeout(PATIENCE, sync.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap()
    {
        Routed::Refresh {
            id,
            note: id_note,
            seq: 1,
        } => {
            assert_eq!(id, "refresh-1");
            assert_eq!(id_note, note);
        }
        _ => panic!("unexpected refresh response"),
    }
}

/// The point of the change: twenty notes, one socket, one token.
#[tokio::test]
async fn twenty_notes_are_one_socket_and_one_handshake() {
    let (relay, _log) = relay().await;
    let tap = Tap::in_front_of(relay.addr).await;

    let mut core = blank_core();
    let notes: Vec<NoteId> = (0..20)
        .map(|_| core.create_note(NoteKind::Checklist).unwrap())
        .collect();
    let mut guest_core = blank_core();
    for note in &notes {
        let blob = core.export_share_blob(*note).unwrap();
        guest_core.import_share_blob(&blob).unwrap();
    }

    let mut a = Phone::new(core, endpoint(&tap.http_base()));
    for (i, note) in notes.iter().enumerate() {
        a.connect(*note);
        a.add_item(*note, &format!("item {i}"));
    }
    a.wait_until("twenty synced notes", |a| {
        notes.iter().all(|n| a.synced(*n))
    })
    .await;

    // Another device, straight to the relay, sees every one of them.
    let mut b = Phone::new(guest_core, endpoint(&relay.http_base()));
    for note in &notes {
        b.connect(*note);
    }
    b.wait_until("every note's item", |b| {
        notes
            .iter()
            .enumerate()
            .all(|(i, n)| b.texts(*n) == [format!("item {i}")])
    })
    .await;

    assert_eq!(requests(&tap, "GET /v1/sync"), 1, "one socket");
    assert_eq!(requests(&tap, "POST /v1/token"), 1, "one token handshake");
}

/// A note made, joined or adopted while the device is connected goes onto the
/// socket it already has.
#[tokio::test]
async fn a_note_connected_later_joins_the_socket_already_open() {
    let (relay, _log) = relay().await;
    let tap = Tap::in_front_of(relay.addr).await;

    let mut core = blank_core();
    let first = core.create_note(NoteKind::Checklist).unwrap();
    let mut a = Phone::new(core, endpoint(&tap.http_base()));
    a.connect(first);
    a.add_item(first, "milk");
    a.wait_until("the first note synced", |a| a.synced(first))
        .await;

    let second = a
        .core
        .lock()
        .unwrap()
        .create_note(NoteKind::Checklist)
        .unwrap();
    a.connect(second);
    a.add_item(second, "stamps");
    a.wait_until("the second note synced", |a| a.synced(second))
        .await;

    let mut guest = blank_core();
    let blob = a.core.lock().unwrap().export_share_blob(second).unwrap();
    guest.import_share_blob(&blob).unwrap();
    let mut b = Phone::new(guest, endpoint(&relay.http_base()));
    b.connect(second);
    b.wait_until("the second note's item", |b| b.texts(second) == ["stamps"])
        .await;

    assert_eq!(requests(&tap, "GET /v1/sync"), 1);
    assert_eq!(requests(&tap, "POST /v1/token"), 1);
}

/// Letting one note go leaves the others on the socket, and dropping its handle
/// is letting it go.
#[tokio::test]
async fn letting_one_note_go_leaves_the_rest_syncing() {
    let (relay, _log) = relay().await;
    let tap = Tap::in_front_of(relay.addr).await;

    let mut core = blank_core();
    let kept = core.create_note(NoteKind::Checklist).unwrap();
    let dropped = core.create_note(NoteKind::Checklist).unwrap();
    let mut guest = blank_core();
    guest
        .import_share_blob(&core.export_share_blob(kept).unwrap())
        .unwrap();

    let mut a = Phone::new(core, endpoint(&tap.http_base()));
    a.connect(kept);
    a.connect(dropped);
    a.wait_until("both synced", |a| a.synced(kept) && a.synced(dropped))
        .await;

    let gone = a.handles.remove(&dropped).unwrap();
    let mut watch = gone.watch_status();
    drop(gone);
    // Its status channel closes once the engine has let it go.
    tokio::time::timeout(PATIENCE, async { while watch.changed().await.is_ok() {} })
        .await
        .expect("the dropped note's status never closed");

    let mut b = Phone::new(guest, endpoint(&relay.http_base()));
    b.connect(kept);
    a.add_item(kept, "still here");
    b.wait_until("the kept note's edit", |b| b.texts(kept) == ["still here"])
        .await;
    assert_eq!(requests(&tap, "GET /v1/sync"), 1);
}

/// A sealed envelope for `note`, the relay accepts as many copies of as it is
/// given. The relay cannot tell one copy from another, and these tests are
/// about how many there are, not what they say.
fn an_envelope(padding: usize) -> (NoteId, Vec<u8>) {
    let mut core = blank_core();
    let note = core.create_note(NoteKind::Checklist).unwrap();
    let (_, envelope) = core.add_item(note, "x".repeat(padding)).unwrap();
    (note, envelope)
}

async fn token(relay: &Relay) -> String {
    authenticate(&relay.http_base(), None, blank_core().identity())
        .await
        .unwrap()
}

/// A cursor is meaningful only against one incarnation of a relay log. If a
/// restore puts its head behind us, the relay says so before accepting the
/// subscription; a retry from zero then reads the restored history.
#[tokio::test]
async fn a_restored_relay_tells_ahead_clients_to_reset() {
    let (relay, log) = relay().await;
    let (note, envelope) = an_envelope(16);
    log.append(&note.encode(), &envelope).await.unwrap();

    let token = token(&relay).await;
    let mut ahead = DeviceSync::connect(&relay.ws_base(), &token).await.unwrap();
    ahead.subscribe(note, 3).await.unwrap();
    assert!(matches!(
        ahead.next().await,
        Ok(Some(Routed::Note(n, Incoming::Reset))) if n == note
    ));
    ahead.close().await.unwrap();

    let mut recovered = DeviceSync::connect(&relay.ws_base(), &token).await.unwrap();
    recovered.subscribe(note, 0).await.unwrap();
    assert!(matches!(
        recovered.next().await,
        Ok(Some(Routed::Note(n, Incoming::Envelope { seq: 1, envelope: got })))
            if n == note && got == envelope
    ));
    assert!(matches!(
        recovered.next().await,
        Ok(Some(Routed::Note(n, Incoming::Caught { seq: 1 }))) if n == note
    ));
}

/// A guest joining a list with months of history must not stop every other
/// note on the device from catching up while that history arrives.
#[tokio::test]
async fn a_long_backlog_does_not_hold_up_another_note() {
    let (relay, log) = relay().await;
    let (long, envelope) = an_envelope(16);
    let long_history = BACKLOG_CHUNK * 20;
    for _ in 0..long_history {
        log.append(&long.encode(), &envelope).await.unwrap();
    }
    let (short, envelope) = an_envelope(16);
    log.append(&short.encode(), &envelope).await.unwrap();

    let mut sync = DeviceSync::connect(&relay.ws_base(), &token(&relay).await)
        .await
        .unwrap();
    // The long one first, so that without chunking the short one waits for
    // all of it.
    sync.subscribe(long, 0).await.unwrap();
    sync.subscribe(short, 0).await.unwrap();

    let mut long_seen = 0;
    let mut short_caught_after = None;
    let mut long_caught = false;
    while !(long_caught && short_caught_after.is_some()) {
        let next = tokio::time::timeout(PATIENCE, sync.next())
            .await
            .expect("the relay went quiet")
            .unwrap()
            .expect("the relay closed the socket");
        match next {
            Routed::Note(n, Incoming::Envelope { .. }) if n == long => long_seen += 1,
            Routed::Note(n, Incoming::Caught { .. }) if n == long => long_caught = true,
            Routed::Note(n, Incoming::Caught { seq }) if n == short => {
                assert_eq!(seq, 1);
                short_caught_after = Some(long_seen);
            }
            Routed::Note(n, Incoming::Envelope { .. }) if n == short => {}
            Routed::Note(_, Incoming::Rejected(m)) | Routed::Unrouted(m) => {
                panic!("refused: {m}")
            }
            Routed::Refresh { .. } => panic!("unexpected refresh marker"),
            Routed::Note(n, _) => panic!("a message for a note never subscribed: {n:?}"),
        }
    }
    assert_eq!(
        long_seen, long_history,
        "every envelope of the long one, once"
    );
    let waited = short_caught_after.unwrap();
    assert!(
        waited < long_history / 2,
        "the short note caught up only after {waited} of the long one's {long_history}"
    );
}

/// A device socket that stops reading while a note is busy falls behind the
/// fan-out. When it reads again it gets everything, once each, in order.
#[tokio::test]
async fn a_socket_that_stops_reading_loses_nothing() {
    let (relay, _log) = relay().await;
    let (note, envelope) = an_envelope(4096);

    let mut slow = DeviceSync::connect(&relay.ws_base(), &token(&relay).await)
        .await
        .unwrap();
    slow.subscribe(note, 0).await.unwrap();
    match tokio::time::timeout(PATIENCE, slow.next()).await.unwrap() {
        Ok(Some(Routed::Note(n, Incoming::Caught { seq: 0 }))) if n == note => {}
        _ => panic!("expected an empty backlog"),
    }

    // Far past the fan-out's buffer and the socket's, while `slow` reads
    // nothing: several megabytes, so it cannot all sit in the kernel.
    let busy = 2_000;
    let mut writer = DeviceSync::connect(&relay.ws_base(), &token(&relay).await)
        .await
        .unwrap();
    writer.subscribe(note, 0).await.unwrap();
    for _ in 0..busy {
        writer.append(note, &envelope).await.unwrap();
    }

    let mut expected = 1;
    while expected <= busy {
        let next = tokio::time::timeout(PATIENCE, slow.next())
            .await
            .expect("the relay went quiet")
            .unwrap()
            .expect("the relay closed the socket");
        match next {
            Routed::Note(n, Incoming::Envelope { seq, .. }) if n == note => {
                assert_eq!(seq, expected as i64, "out of order, or one went missing");
                expected += 1;
            }
            Routed::Note(_, Incoming::Rejected(m)) | Routed::Unrouted(m) => {
                panic!("refused: {m}")
            }
            _ => {}
        }
    }
}

/// What the relay refuses on a device socket, and that none of it ends the
/// socket.
#[tokio::test]
async fn the_relay_refuses_what_it_should_and_keeps_the_socket() {
    let (relay, _log) = relay().await;
    let (note, envelope) = an_envelope(16);
    let (other, _) = an_envelope(16);
    let mut sync = DeviceSync::connect(&relay.ws_base(), &token(&relay).await)
        .await
        .unwrap();

    async fn refusal(sync: &mut DeviceSync) -> (Option<NoteId>, String) {
        match tokio::time::timeout(PATIENCE, sync.next()).await.unwrap() {
            Ok(Some(Routed::Note(n, Incoming::Rejected(m)))) => (Some(n), m),
            Ok(Some(Routed::Unrouted(m))) => (None, m),
            _ => panic!("expected a refusal"),
        }
    }

    // Appending to a note this socket has not subscribed to.
    sync.append(note, &envelope).await.unwrap();
    assert_eq!(refusal(&mut sync).await.0, Some(note));

    // An envelope sealed for one note, addressed to another.
    sync.subscribe(other, 0).await.unwrap();
    assert!(matches!(
        sync.next().await,
        Ok(Some(Routed::Note(_, Incoming::Caught { .. })))
    ));
    sync.append(other, &envelope).await.unwrap();
    assert_eq!(refusal(&mut sync).await.0, Some(other));

    // Subscribing twice.
    sync.subscribe(other, 0).await.unwrap();
    assert_eq!(refusal(&mut sync).await.0, Some(other));

    // And the socket still works.
    sync.subscribe(note, 0).await.unwrap();
    assert!(matches!(
        sync.next().await,
        Ok(Some(Routed::Note(n, Incoming::Caught { seq: 0 }))) if n == note
    ));
    sync.append(note, &envelope).await.unwrap();
    assert!(matches!(
        sync.next().await,
        Ok(Some(Routed::Note(n, Incoming::Envelope { seq: 1, .. }))) if n == note
    ));
}

/// A device socket needs a token.
#[tokio::test]
async fn a_device_socket_needs_a_token() {
    let (relay, _log) = relay().await;
    assert!(DeviceSync::connect(&relay.ws_base(), "not-a-token")
        .await
        .is_err());
}
