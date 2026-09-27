//! The attachment transfer queue against a real relay, with real engines
//! carrying the note ([ADR 0027](../../docs/adr/0027-attachments.md) §3–§4).
//!
//! `attachments.rs` moves bytes by hand. This is the part the app will run: a
//! staged import that waits for its note, goes up, is published and settled on
//! its own, and a download the other device asks for.

use easynotes_core::attachment::store::{ImportHints, MediaStore, Staged, ThumbnailInput};
use easynotes_core::attachment::{AttachmentState, Media, Variant};
use easynotes_core::crdt::DefaultNoteDoc;
use easynotes_core::crypto::{AuthorKey, DeviceSigner, MasterKey};
use easynotes_core::ids::{AttachmentId, NoteId};
use easynotes_core::note::{NoteCore, NoteKind};
use easynotes_core::store::{MemoryKeyring, MemoryStore};
use easynotes_core::sync::media::download_into;
use easynotes_core::sync::transfers::{
    Direction, Reach, Transfer, TransferPeer, TransferQueue, TransferRunner, TransferState,
};
use easynotes_core::sync::{authenticate, RelayEndpoint, SyncEngine, SyncHandle, SyncState};
use easynotes_relay::{MediaConfig, MediaLimits, MemoryBlobs, Relay, RelayConfig};
use std::sync::{Arc, Mutex};
use std::time::Duration;

type Core = NoteCore<DefaultNoteDoc, MemoryStore, MemoryKeyring>;

const PATIENCE: Duration = Duration::from_secs(10);

fn png(w: u32, h: u32, len: usize) -> Vec<u8> {
    let mut bytes = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
    bytes.extend_from_slice(&w.to_be_bytes());
    bytes.extend_from_slice(&h.to_be_bytes());
    bytes.extend((0..len - bytes.len()).map(|i| (i * 13 % 251) as u8));
    bytes
}

async fn eventually(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + PATIENCE;
    while !done() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// The app's side of the queue, as the bridge will be: the core behind a lock,
/// and the engine's handle for the note, or none while it is not connected.
#[derive(Clone)]
struct Peer {
    core: Arc<Mutex<Core>>,
    handle: Arc<Mutex<Option<SyncHandle>>>,
    local_only: bool,
}

impl DeviceSigner for Peer {
    fn device_key(&self) -> easynotes_core::Result<AuthorKey> {
        self.core.device_key()
    }

    fn sign_challenge(
        &self,
        nonce: &[u8; easynotes_core::crypto::RELAY_AUTH_NONCE_LEN],
    ) -> easynotes_core::Result<[u8; easynotes_core::crypto::SIG_LEN]> {
        self.core.sign_challenge(nonce)
    }
}

impl TransferPeer for Peer {
    fn attachment_state(
        &self,
        note: NoteId,
        id: AttachmentId,
    ) -> easynotes_core::Result<AttachmentState> {
        self.core.lock().unwrap().attachment_state(note, id)
    }

    fn attachment(&self, note: NoteId, id: AttachmentId) -> easynotes_core::Result<Option<Media>> {
        let core = self.core.lock().unwrap();
        Ok(core.view(note)?.attachment(id).map(|a| a.media.clone()))
    }

    fn reach(&self, _note: NoteId) -> Reach {
        if self.local_only {
            return Reach::LocalOnly;
        }
        match self
            .handle
            .lock()
            .unwrap()
            .as_ref()
            .map(|h| h.status().state)
        {
            None | Some(SyncState::Offline) => Reach::Offline,
            Some(SyncState::Connecting | SyncState::Online) => Reach::Connected,
            Some(SyncState::Synced) => Reach::Synced,
        }
    }

    fn publish(&self, note: NoteId, id: AttachmentId, media: Media) -> easynotes_core::Result<()> {
        let envelope = self.core.lock().unwrap().add_attachment(note, id, media)?;
        if let Some(handle) = self.handle.lock().unwrap().as_ref() {
            handle.publish(envelope);
        }
        Ok(())
    }
}

/// One device: a core, its media store, and the queue over them.
struct App {
    peer: Peer,
    note: NoteId,
    media: Arc<MediaStore>,
    queue: TransferQueue,
    _dir: tempfile::TempDir,
}

impl App {
    fn new(core: Core, note: NoteId, relay: Option<&RelayEndpoint>) -> Self {
        let peer = Peer {
            core: Arc::new(Mutex::new(core)),
            handle: Arc::default(),
            local_only: relay.is_none(),
        };
        let dir = tempfile::tempdir().unwrap();
        let media = Arc::new(MediaStore::open(dir.path(), MasterKey::random()).unwrap());
        let queue = start(&peer, &media, relay);
        Self {
            peer,
            note,
            media,
            queue,
            _dir: dir,
        }
    }

    fn connect(&self, endpoint: &RelayEndpoint) {
        let (engine, handle) =
            SyncEngine::start(self.peer.core.clone(), self.note, endpoint.clone());
        tokio::spawn(engine.run());
        *self.peer.handle.lock().unwrap() = Some(handle);
        // What the bridge does on every status change.
        let queue = self.queue.clone();
        let mut status = self
            .peer
            .handle
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .watch_status();
        tokio::spawn(async move {
            while status.changed().await.is_ok() {
                queue.kick();
            }
        });
    }

    fn import(&self, name: &str, bytes: &[u8], thumbnail: Option<Vec<u8>>) -> Staged {
        let staged = self
            .media
            .import(
                self.note,
                name,
                bytes,
                bytes.len() as u64,
                thumbnail.map(|bytes| ThumbnailInput { bytes }),
                ImportHints::default(),
            )
            .unwrap();
        self.queue.kick();
        staged
    }

    fn state(&self, id: AttachmentId) -> AttachmentState {
        self.peer
            .core
            .lock()
            .unwrap()
            .attachment_state(self.note, id)
            .unwrap()
    }

    fn upload_state(&self, id: AttachmentId) -> Option<TransferState> {
        self.queue
            .transfers()
            .get(&Transfer {
                note: self.note,
                id,
                direction: Direction::Up,
            })
            .cloned()
    }
}

fn start(peer: &Peer, media: &Arc<MediaStore>, relay: Option<&RelayEndpoint>) -> TransferQueue {
    let (runner, queue) = TransferRunner::new(
        peer.clone(),
        media.clone(),
        relay.map(|r| r.http_base.clone()),
    );
    tokio::spawn(runner.run());
    queue
}

async fn relay_with(blobs: Option<Arc<MemoryBlobs>>) -> (Relay, RelayEndpoint) {
    let relay = Relay::serve_with(
        Arc::new(easynotes_relay::MemoryStore::new()),
        "127.0.0.1:0".parse().unwrap(),
        RelayConfig {
            media: MediaConfig {
                blobs: blobs.map(|b| b as Arc<dyn easynotes_relay::Blobs>),
                limits: MediaLimits::default(),
            },
            ..RelayConfig::default()
        },
    )
    .await
    .unwrap();
    let endpoint = RelayEndpoint::parse(&relay.http_base()).unwrap();
    (relay, endpoint)
}

/// An owner and a guest on one text note.
fn pair(endpoint: &RelayEndpoint) -> (App, App) {
    let mut a: Core = NoteCore::open(MemoryStore::new(), MemoryKeyring::new()).unwrap();
    let note = a.create_note(NoteKind::Text).unwrap();
    let mut b: Core = NoteCore::open(MemoryStore::new(), MemoryKeyring::new()).unwrap();
    b.import_share_blob(&a.export_share_blob(note).unwrap())
        .unwrap();
    (
        App::new(a, note, Some(endpoint)),
        App::new(b, note, Some(endpoint)),
    )
}

#[tokio::test]
async fn a_photo_goes_up_is_published_and_comes_down_on_its_own() {
    let blobs = Arc::new(MemoryBlobs::default());
    let (_relay, endpoint) = relay_with(Some(blobs.clone())).await;
    let (a, b) = pair(&endpoint);
    a.connect(&endpoint);
    b.connect(&endpoint);

    let photo = png(4000, 3000, (2 << 20) + 99);
    let thumb = png(512, 384, 7000);
    let staged = a.import("market.png", &photo, Some(thumb.clone()));

    // The other device learns of it from the note. By then the relay has
    // committed both objects: a direct fetch, which an uncommitted object
    // refuses, succeeds at once.
    eventually("the descriptor to reach b", || {
        b.state(staged.id) == AttachmentState::Live
    })
    .await;
    let token = authenticate(&endpoint.http_base, &b.peer).await.unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let scratch = MediaStore::open(scratch.path(), MasterKey::random()).unwrap();
    download_into(
        &endpoint.http_base,
        &token,
        &scratch,
        a.note,
        &staged.media.original,
    )
    .await
    .unwrap();

    // The author's record is settled and nothing is left in its queue.
    eventually("a's upload to finish", || {
        a.media.staged().unwrap().is_empty() && a.queue.transfers().is_empty()
    })
    .await;
    assert_eq!(a.state(staged.id), AttachmentState::Live);

    // b asks for the thumbnail, as a card would, then the original.
    let thumbnail = staged.media.thumbnail.as_ref().unwrap().media.clone();
    b.queue.fetch(b.note, staged.id, Variant::Thumbnail);
    eventually("the thumbnail on b", || b.media.has(b.note, &thumbnail)).await;
    let media = b
        .peer
        .attachment(b.note, staged.id)
        .unwrap()
        .expect("live on b");
    assert_eq!(
        b.media
            .read(b.note, staged.id, &media, Variant::Thumbnail)
            .unwrap(),
        thumb
    );
    assert!(!b.media.has(b.note, &media.original));
    b.queue.fetch(b.note, staged.id, Variant::Original);
    eventually("the original on b", || b.media.has(b.note, &media.original)).await;
    assert_eq!(
        b.media
            .read(b.note, staged.id, &media, Variant::Original)
            .unwrap(),
        photo
    );
    eventually("b's queue to empty", || b.queue.transfers().is_empty()).await;
}

#[tokio::test]
async fn an_import_made_offline_waits_for_its_note_and_then_goes() {
    let blobs = Arc::new(MemoryBlobs::default());
    let (_relay, endpoint) = relay_with(Some(blobs.clone())).await;
    let (a, _b) = pair(&endpoint);

    let staged = a.import("offline.png", &png(10, 10, 5000), None);
    eventually("the import to wait", || {
        a.upload_state(staged.id) == Some(TransferState::Waiting)
    })
    .await;
    // Saved here, and nowhere else yet.
    assert_eq!(a.state(staged.id), AttachmentState::Pending);
    assert_eq!(a.media.staged().unwrap(), vec![staged.clone()]);
    assert!(blobs.all_bytes().is_empty());

    a.connect(&endpoint);
    eventually("the import to be published", || {
        a.state(staged.id) == AttachmentState::Live && a.media.staged().unwrap().is_empty()
    })
    .await;
    assert_eq!(blobs.all_bytes().len(), 1);
}

#[tokio::test]
async fn a_cancelled_import_is_never_published() {
    let blobs = Arc::new(MemoryBlobs::default());
    let (_relay, endpoint) = relay_with(Some(blobs.clone())).await;
    let (a, _b) = pair(&endpoint);

    let staged = a.import("mistake.png", &png(10, 10, 5000), None);
    eventually("the import to wait", || a.upload_state(staged.id).is_some()).await;
    a.queue.cancel(staged.id);
    eventually("the import to go", || {
        a.queue.transfers().is_empty() && a.media.staged().unwrap().is_empty()
    })
    .await;
    assert!(!a.media.has(a.note, &staged.media.original));

    // Connecting afterwards publishes nothing and uploads nothing.
    a.connect(&endpoint);
    eventually("a to sync", || a.peer.reach(a.note) == Reach::Synced).await;
    a.queue.kick();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(a.state(staged.id), AttachmentState::Pending);
    assert!(blobs.all_bytes().is_empty());
}

#[tokio::test]
async fn staged_imports_are_reconciled_when_the_queue_starts() {
    // A note that does not sync: each import is published as soon as the
    // queue sees it, so what is left to check is what a crash left behind.
    let mut core: Core = NoteCore::open(MemoryStore::new(), MemoryKeyring::new()).unwrap();
    let note = core.create_note(NoteKind::Checklist).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let media = Arc::new(MediaStore::open(dir.path(), MasterKey::random()).unwrap());
    let import = |name: &str| {
        let bytes = png(20, 20, 3000);
        media
            .import(
                note,
                name,
                bytes.as_slice(),
                bytes.len() as u64,
                None,
                ImportHints::default(),
            )
            .unwrap()
    };
    // Published, and the app stopped before the record was settled.
    let published = import("published.png");
    core.add_attachment(note, published.id, published.media.clone())
        .unwrap();
    // Published, then removed by someone, before this launch.
    let removed = import("removed.png");
    core.add_attachment(note, removed.id, removed.media.clone())
        .unwrap();
    core.remove_attachment(note, removed.id).unwrap();
    // Never published.
    let pending = import("pending.png");

    let peer = Peer {
        core: Arc::new(Mutex::new(core)),
        handle: Arc::default(),
        local_only: true,
    };
    let queue = start(&peer, &media, None);
    let state = |id| {
        peer.core
            .lock()
            .unwrap()
            .attachment_state(note, id)
            .unwrap()
    };
    eventually("every record to be reconciled", || {
        media.staged().unwrap().is_empty() && queue.transfers().is_empty()
    })
    .await;

    assert_eq!(state(published.id), AttachmentState::Live);
    assert!(media.has(note, &published.media.original));
    // Not brought back, and its bytes are gone from here.
    assert_eq!(state(removed.id), AttachmentState::Removed);
    assert!(!media.has(note, &removed.media.original));
    assert_eq!(state(pending.id), AttachmentState::Live);
    assert!(media.has(note, &pending.media.original));
}

#[tokio::test]
async fn a_relay_that_refuses_attachments_fails_the_upload_and_keeps_the_file() {
    let (_relay, endpoint) = relay_with(None).await;
    let (a, _b) = pair(&endpoint);
    a.connect(&endpoint);

    let staged = a.import("refused.png", &png(10, 10, 5000), None);
    eventually("the upload to fail", || {
        matches!(
            a.upload_state(staged.id),
            Some(TransferState::Failed { .. })
        )
    })
    .await;
    let Some(TransferState::Failed { error }) = a.upload_state(staged.id) else {
        unreachable!()
    };
    assert!(error.contains("does not take attachments"), "{error}");
    // Still saved on this device, still not on the note.
    assert_eq!(a.state(staged.id), AttachmentState::Pending);
    assert_eq!(a.media.staged().unwrap(), vec![staged.clone()]);
    assert!(a.media.has(a.note, &staged.media.original));

    // Retrying asks the relay again, and is refused again.
    let mut watch = a.queue.watch();
    watch.borrow_and_update();
    a.queue.retry();
    tokio::time::timeout(PATIENCE, watch.changed())
        .await
        .unwrap()
        .unwrap();
    eventually("the retry to fail too", || {
        matches!(
            a.upload_state(staged.id),
            Some(TransferState::Failed { .. })
        )
    })
    .await;
}
