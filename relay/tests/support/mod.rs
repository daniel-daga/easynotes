//! Test scaffolding: a headless device, and a TCP tap that records every byte
//! that crosses the wire.

#![allow(dead_code)]

use easynotes_core::crdt::{DefaultNoteDoc, NoteView};
use easynotes_core::ids::{ItemId, NoteId};
use easynotes_core::note::{NoteCore, NoteKind};
use easynotes_core::store::{MemoryKeyring, MemoryStore};
use easynotes_core::sync::{authenticate, Incoming, NoteSync};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub type Core = NoteCore<DefaultNoteDoc, MemoryStore, MemoryKeyring>;

/// How long to wait for the relay to go quiet before deciding it has nothing
/// more to say. Generous, because a loaded CI box is slow and a flaky test is
/// worse than a slow one.
const QUIET: Duration = Duration::from_millis(400);

/// One headless device: a core, a relay connection, and an outbox for the edits
/// it made while offline.
pub struct Device {
    pub name: &'static str,
    pub core: Core,
    pub note: NoteId,
    sync: Option<NoteSync>,
    outbox: Vec<Vec<u8>>,
    last_seq: i64,
    http_base: String,
    ws_base: String,
    token: String,
}

impl Device {
    /// A device that creates the note.
    pub async fn owner(name: &'static str, http_base: &str, ws_base: &str) -> Self {
        let mut core: Core = NoteCore::open(MemoryStore::new(), MemoryKeyring::new()).unwrap();
        let note = core.create_note(NoteKind::Checklist).unwrap();
        Self::wrap(name, core, note, http_base, ws_base).await
    }

    /// A device that joins from a share blob — the Slice 1 stand-in for an
    /// invite link.
    pub async fn guest(name: &'static str, blob: &str, http_base: &str, ws_base: &str) -> Self {
        let mut core: Core = NoteCore::open(MemoryStore::new(), MemoryKeyring::new()).unwrap();
        let note = core.import_share_blob(blob).unwrap();
        Self::wrap(name, core, note, http_base, ws_base).await
    }

    /// A device around a core that already has the note — joined by an invite,
    /// say, which is not something this scaffolding knows how to do itself.
    pub async fn with_core(
        name: &'static str,
        core: Core,
        note: NoteId,
        http_base: &str,
        ws_base: &str,
    ) -> Self {
        Self::wrap(name, core, note, http_base, ws_base).await
    }

    async fn wrap(
        name: &'static str,
        core: Core,
        note: NoteId,
        http_base: &str,
        ws_base: &str,
    ) -> Self {
        let token = authenticate(http_base, core.identity()).await.unwrap();
        let mut device = Self {
            name,
            core,
            note,
            sync: None,
            outbox: Vec::new(),
            last_seq: 0,
            http_base: http_base.to_string(),
            ws_base: ws_base.to_string(),
            token,
        };
        device.go_online().await;
        device
    }

    pub fn share_blob(&self) -> String {
        self.core.export_share_blob(self.note).unwrap()
    }

    pub fn view(&self) -> NoteView {
        self.core.view(self.note).unwrap()
    }

    pub fn texts(&self) -> Vec<String> {
        self.view().items.iter().map(|i| i.text.clone()).collect()
    }

    // -- connectivity -------------------------------------------------------

    pub fn is_online(&self) -> bool {
        self.sync.is_some()
    }

    /// Airplane mode. Edits still work; they queue in the outbox.
    pub async fn go_offline(&mut self) {
        if let Some(sync) = self.sync.take() {
            self.last_seq = sync.last_seq;
            let _ = sync.close().await;
        }
    }

    /// Reconnect, replay from the cursor, and flush anything edited while away.
    pub async fn go_online(&mut self) {
        let sync = NoteSync::connect(&self.ws_base, &self.token, self.note, self.last_seq)
            .await
            .unwrap_or_else(|e| panic!("{} could not reach the relay: {e}", self.name));
        self.sync = Some(sync);
        self.pump().await;
        let queued = std::mem::take(&mut self.outbox);
        for envelope in queued {
            self.send(&envelope).await;
        }
        self.pump().await;
    }

    pub async fn send(&mut self, envelope: &[u8]) {
        match self.sync.as_mut() {
            Some(sync) => sync.append(envelope).await.unwrap(),
            None => self.outbox.push(envelope.to_vec()),
        }
    }

    /// Apply everything the relay has for us, then wait `QUIET` for more.
    ///
    /// Settles the note on `Caught`, as the engine does, and sends what
    /// settling says to: a device joining a shared note gets its key there.
    pub async fn pump(&mut self) {
        loop {
            let Some(sync) = self.sync.as_mut() else {
                return;
            };
            let next = tokio::time::timeout(QUIET, sync.next()).await;
            let Ok(next) = next else {
                // Quiet: nothing more coming. Every envelope after `Caught` is
                // a settled batch of its own, so settle once more for them.
                self.settle().await;
                return;
            };
            match next.unwrap() {
                Some(Incoming::Envelope { envelope, .. }) => {
                    match self.core.apply_remote(self.note, &envelope) {
                        Ok(_) | Err(easynotes_core::CoreError::AccessPending) => {}
                        Err(e) => panic!("{} rejected an envelope from the relay: {e}", self.name),
                    }
                }
                Some(Incoming::Caught { .. }) => self.settle().await,
                Some(Incoming::Reset) => panic!("{} unexpectedly reset", self.name),
                Some(Incoming::Rejected(m)) => panic!("{} was rejected: {m}", self.name),
                None => {
                    self.sync = None;
                    return;
                }
            }
        }
    }

    /// What the engine does once the note is caught up: act on its access
    /// packages, and send what that says to.
    pub async fn settle(&mut self) {
        let out = self.core.settle_note(self.note).unwrap();
        for envelope in out.envelopes {
            self.send(&envelope).await;
        }
    }

    // -- edits --------------------------------------------------------------

    pub async fn set_title(&mut self, title: &str) {
        let e = self.core.set_title(self.note, title).unwrap();
        self.send(&e).await;
    }

    pub async fn add_item(&mut self, text: &str) -> ItemId {
        let (id, e) = self.core.add_item(self.note, text).unwrap();
        self.send(&e).await;
        id
    }

    pub async fn set_checked(&mut self, item: ItemId, checked: bool) {
        let e = self.core.set_checked(self.note, item, checked).unwrap();
        self.send(&e).await;
    }

    pub async fn set_item_text(&mut self, item: ItemId, text: &str) {
        let e = self.core.set_item_text(self.note, item, text).unwrap();
        self.send(&e).await;
    }

    pub async fn move_item(&mut self, item: ItemId, to: usize) {
        let e = self.core.move_item(self.note, item, to).unwrap();
        self.send(&e).await;
    }

    pub async fn remove_item(&mut self, item: ItemId) {
        let e = self.core.remove_item(self.note, item).unwrap();
        self.send(&e).await;
    }

    /// Find an item by its current text.
    pub fn item_called(&self, text: &str) -> ItemId {
        self.view()
            .items
            .iter()
            .find(|i| i.text == text)
            .unwrap_or_else(|| {
                panic!(
                    "{} has no item called {text:?}: {:?}",
                    self.name,
                    self.texts()
                )
            })
            .id
    }
}

// ---------------------------------------------------------------------------
// The wire tap
// ---------------------------------------------------------------------------

/// A transparent TCP proxy that records every byte in both directions.
///
/// This is the automated form of "run `tcpdump` on the relay and read the
/// frames": clients connect to the tap, the tap forwards to the relay, and the
/// test then searches the captured bytes for anything it should never see.
pub struct Tap {
    pub addr: SocketAddr,
    to_client: Arc<Mutex<Vec<u8>>>,
    to_server: Arc<Mutex<Vec<u8>>>,
}

impl Tap {
    pub async fn in_front_of(upstream: SocketAddr) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let to_client = Arc::new(Mutex::new(Vec::new()));
        let to_server = Arc::new(Mutex::new(Vec::new()));

        let (c, s) = (to_client.clone(), to_server.clone());
        tokio::spawn(async move {
            while let Ok((inbound, _)) = listener.accept().await {
                let (c, s) = (c.clone(), s.clone());
                tokio::spawn(async move {
                    let Ok(outbound) = tokio::net::TcpStream::connect(upstream).await else {
                        return;
                    };
                    let (cr, cw) = inbound.into_split();
                    let (sr, sw) = outbound.into_split();
                    tokio::join!(pipe(cr, sw, s), pipe(sr, cw, c));
                });
            }
        });

        Self {
            addr,
            to_client,
            to_server,
        }
    }

    pub fn http_base(&self) -> String {
        format!("http://{}", self.addr)
    }

    pub fn ws_base(&self) -> String {
        format!("ws://{}", self.addr)
    }

    /// Everything the relay sent to a client. WebSocket frames from the server
    /// are unmasked, so this is the real bytes of every update, verbatim.
    pub fn bytes_to_clients(&self) -> Vec<u8> {
        self.to_client.lock().unwrap().clone()
    }

    /// Everything clients sent to the relay. Note that WebSocket *client* frames
    /// are XOR-masked by the protocol, so a substring search here proves less
    /// than the one above — which is why the tests also search the relay's own
    /// stored envelopes, where nothing is masked.
    pub fn bytes_to_server(&self) -> Vec<u8> {
        self.to_server.lock().unwrap().clone()
    }
}

async fn pipe(
    mut from: tokio::net::tcp::OwnedReadHalf,
    mut to: tokio::net::tcp::OwnedWriteHalf,
    seen: Arc<Mutex<Vec<u8>>>,
) {
    let mut buf = vec![0u8; 16 * 1024];
    while let Ok(n) = from.read(&mut buf).await {
        if n == 0 {
            break;
        }
        seen.lock().unwrap().extend_from_slice(&buf[..n]);
        if to.write_all(&buf[..n]).await.is_err() {
            break;
        }
    }
    let _ = to.shutdown().await;
}

pub fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && haystack.windows(needle.len()).any(|w| w == needle)
}
