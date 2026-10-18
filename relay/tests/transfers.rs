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
    /// Say the relay has not acknowledged anything, whatever the engine says:
    /// a device whose descriptor has not reached the relay, without having to
    /// take its network away.
    unheard: Arc<std::sync::atomic::AtomicBool>,
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

    fn acknowledged(&self, _note: NoteId) -> bool {
        if self.unheard.load(std::sync::atomic::Ordering::SeqCst) {
            return false;
        }
        self.local_only
            || self.handle.lock().unwrap().as_ref().is_some_and(|h| {
                let status = h.status();
                status.state == SyncState::Synced && status.pending == 0
            })
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
            unheard: Arc::default(),
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

#[tokio::test]
async fn a_published_local_attachment_uploads_even_after_its_note_is_acknowledged() {
    let blobs = Arc::new(MemoryBlobs::default());
    let (_relay, endpoint) = relay_with(Some(blobs.clone())).await;
    let mut core: Core = NoteCore::open(MemoryStore::new(), MemoryKeyring::new()).unwrap();
    let note = core.create_note(NoteKind::Text).unwrap();
    let mut app = App::new(core, note, None);
    let staged = app.import("local.png", &png(8, 8, 500), None);
    eventually("the local attachment to settle", || {
        app.state(staged.id) == AttachmentState::Live && app.media.staged().unwrap().is_empty()
    }).await;
    app.queue.stop().await;
    app.media.restage(&staged).unwrap();
    app.peer.local_only = false;
    // Let the note reach the relay before starting its replacement queue.
    app.connect(&endpoint);
    eventually("the descriptor to be acknowledged", || app.peer.acknowledged(note)).await;
    app.queue = start(&app.peer, &app.media, Some(&endpoint));
    eventually("the existing attachment bytes to upload", || {
        app.media.staged().unwrap().is_empty() && !blobs.all_bytes().is_empty()
    }).await;
    assert_eq!(blobs.all_bytes().len(), 1);
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
    let token = authenticate(&endpoint.http_base, None, &b.peer)
        .await
        .unwrap();
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
        unheard: Arc::default(),
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

#[tokio::test]
async fn the_same_queue_runs_over_a_tabs_store() {
    use easynotes_core::attachment::web::{MemoryRows, WebMediaStore};

    let blobs = Arc::new(MemoryBlobs::default());
    let (_relay, endpoint) = relay_with(Some(blobs.clone())).await;
    let (a, b) = pair(&endpoint);
    a.connect(&endpoint);
    b.connect(&endpoint);

    // A tab's store and queue beside each device's own, over the same core:
    // the rows are in memory here, and in IndexedDB in a browser.
    let tab = |peer: &Peer| {
        let peer = peer.clone();
        let http_base = endpoint.http_base.clone();
        async move {
            let store = Arc::new(
                WebMediaStore::open(MemoryRows::default(), MasterKey::random())
                    .await
                    .unwrap(),
            );
            let (runner, queue) = TransferRunner::new(peer, store.clone(), Some(http_base));
            tokio::spawn(runner.run());
            (store, queue)
        }
    };
    let (store_a, queue_a) = tab(&a.peer).await;
    let (store_b, queue_b) = tab(&b.peer).await;

    let photo = png(1200, 900, (1 << 20) + 5);
    let thumb = png(256, 192, 3000);
    let staged = store_a
        .import(
            a.note,
            "tab.png",
            &photo,
            Some(ThumbnailInput {
                bytes: thumb.clone(),
            }),
            ImportHints::default(),
        )
        .await
        .unwrap();
    // Kicked as the bridge will kick it, on every status change; here, on
    // every look, so the test does not wait out a recheck.
    eventually("the tab's import to reach b", || {
        queue_a.kick();
        b.state(staged.id) == AttachmentState::Live
    })
    .await;
    eventually("the tab's record to settle", || {
        queue_a.transfers().is_empty()
    })
    .await;
    assert!(store_a.staged().await.unwrap().is_empty());

    let media = b.peer.attachment(b.note, staged.id).unwrap().unwrap();
    let thumbnail = media.thumbnail.as_ref().unwrap().media.clone();
    queue_b.fetch(b.note, staged.id, Variant::Thumbnail);
    queue_b.fetch(b.note, staged.id, Variant::Original);
    let mut both = false;
    for _ in 0..500 {
        if store_b.has(b.note, &thumbnail).await && store_b.has(b.note, &media.original).await {
            both = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(both, "b's tab never fetched both variants");
    assert_eq!(
        store_b
            .read(b.note, staged.id, &media, Variant::Original)
            .await
            .unwrap(),
        photo
    );
    assert_eq!(
        store_b
            .read(b.note, staged.id, &media, Variant::Thumbnail)
            .await
            .unwrap(),
        thumb
    );
}

/// ADR 0027 §5 from the uploader's side. A published upload whose descriptor
/// the relay has not acknowledged keeps its record, and with it the bytes. If
/// the relay let them go meanwhile, as a compaction from a device that never
/// saw the descriptor would, the queue's next look at the record puts them
/// back, and the record settles once the relay has the descriptor.
#[tokio::test]
async fn an_upload_not_yet_heard_of_is_kept_and_put_back_if_lost() {
    use easynotes_core::compaction::Request;
    use easynotes_relay::quota::Proposal;
    use easynotes_relay::RelayStore;

    let store = Arc::new(easynotes_relay::MemoryStore::new());
    let blobs = Arc::new(MemoryBlobs::default());
    // Nothing is ever fresh, so one compaction dooms what it does not list.
    let limits = MediaLimits {
        fresh_secs: -60,
        ..MediaLimits::default()
    };
    let relay = Relay::serve_with(
        store.clone(),
        "127.0.0.1:0".parse().unwrap(),
        RelayConfig {
            media: MediaConfig {
                blobs: Some(blobs.clone() as Arc<dyn easynotes_relay::Blobs>),
                limits,
            },
            ..RelayConfig::default()
        },
    )
    .await
    .unwrap();
    let endpoint = RelayEndpoint::parse(&relay.http_base()).unwrap();
    let (a, b) = pair(&endpoint);
    a.connect(&endpoint);
    b.connect(&endpoint);

    a.peer
        .unheard
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let photo = png(64, 48, 5000);
    let staged = a.import("kept.png", &photo, None);
    eventually("the descriptor to reach b", || {
        b.state(staged.id) == AttachmentState::Live
    })
    .await;
    // Published, and not settled: the relay has not been heard to have it.
    assert_eq!(a.state(staged.id), AttachmentState::Live);
    assert_eq!(a.media.staged().unwrap(), vec![staged.clone()]);

    // A compaction that does not list it, and the week gone by: the relay
    // forgets the object, bytes and all.
    let topic = a.note.encode();
    let object = staged.media.original.object.encode();
    let head = store.head(&topic).await.unwrap();
    let base = a
        .peer
        .core
        .lock()
        .unwrap()
        .compaction(a.note, head)
        .unwrap()
        .expect("a base");
    let key = easynotes_core::crypto::import_share_blob(
        &a.peer
            .core
            .lock()
            .unwrap()
            .export_share_blob(a.note)
            .unwrap(),
    )
    .unwrap()
    .1;
    let unlisted = Request::sign(a.note, &key, base.covers, base.base, Some(Vec::new()));
    store
        .compact(
            &topic,
            &Proposal {
                covers: unlisted.covers,
                base: &unlisted.base,
                media: unlisted.media.as_deref(),
                key: unlisted.key.0,
                signature: unlisted.signature,
            },
            &limits,
            i64::MAX / 2,
        )
        .await
        .unwrap()
        .unwrap();
    for (note, gone) in store.sweep_media(i64::MAX).await.unwrap() {
        easynotes_relay::Blobs::remove(blobs.as_ref(), &note, &gone)
            .await
            .unwrap();
    }
    assert!(store.media(&topic, &object).await.unwrap().is_none());

    // The app is opened again: the queue looks at the record, and sends it.
    let again = start(&a.peer, &a.media, Some(&endpoint));
    again.kick();
    let mut back = false;
    for _ in 0..500 {
        back = store
            .media(&topic, &object)
            .await
            .unwrap()
            .is_some_and(|row| row.committed.is_some());
        if back {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(back, "the bytes the relay lost were not put back");
    assert_eq!(a.media.staged().unwrap(), vec![staged.clone()]);

    // Heard: the record settles.
    a.peer
        .unheard
        .store(false, std::sync::atomic::Ordering::SeqCst);
    again.kick();
    eventually("the record to settle", || {
        a.media.staged().unwrap().is_empty()
    })
    .await;
}

