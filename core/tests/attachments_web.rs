//! The browser's media store, held to the directory's promises
//! ([ADR 0027](../../docs/decisions.md#adr-0027)) with its rows in memory.
//! What IndexedDB adds is three operations and no rules; `just web-media` runs
//! those in a real browser.

use easynotes_core::attachment::seal::CHUNK_OVERHEAD;
use easynotes_core::attachment::storage::{ImportHints, ThumbnailInput};
use easynotes_core::attachment::store::MediaStore;
use easynotes_core::attachment::web::{MediaRows, MemoryRows, WebMediaStore};
use easynotes_core::attachment::Variant;
use easynotes_core::crypto::MasterKey;
use easynotes_core::ids::NoteId;
use easynotes_core::store::Change;
use std::sync::Arc;

type Store = WebMediaStore<Arc<MemoryRows>>;

fn png(w: u32, h: u32, len: usize) -> Vec<u8> {
    let mut bytes = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
    bytes.extend_from_slice(&w.to_be_bytes());
    bytes.extend_from_slice(&h.to_be_bytes());
    // Not one repeated byte, so a chunk of plaintext is recognisable below.
    bytes.extend((0..len - bytes.len()).map(|i| (i * 13 % 251) as u8));
    bytes
}

fn wav(len: usize) -> Vec<u8> {
    let mut bytes = b"RIFF\0\0\0\0WAVEfmt ".to_vec();
    bytes.resize(len, 0x11);
    bytes
}

async fn open(rows: &Arc<MemoryRows>, master: &MasterKey) -> Store {
    WebMediaStore::open(rows.clone(), master.clone())
        .await
        .unwrap()
}

#[tokio::test]
async fn a_local_attachment_can_be_restaged_durably_in_a_tab() {
    let rows = Arc::new(MemoryRows::default());
    let master = MasterKey::random();
    let store = open(&rows, &master).await;
    let note = NoteId::random();
    let photo = png(8, 8, 500);
    let staged = store
        .import(note, "local.png", &photo, None, ImportHints::default())
        .await
        .unwrap();
    store.settle(staged.id).await.unwrap();
    store.restage(&staged).await.unwrap();
    store.restage(&staged).await.unwrap();
    drop(store);
    let reopened = open(&rows, &master).await;
    assert_eq!(reopened.staged().await.unwrap(), vec![staged.clone()]);
    assert_eq!(
        reopened
            .read(note, staged.id, &staged.media, Variant::Original)
            .await
            .unwrap(),
        photo
    );
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

#[tokio::test]
async fn an_import_is_kept_across_a_reload_and_reads_back() {
    let rows = Arc::new(MemoryRows::default());
    let master = MasterKey::random();
    let store = open(&rows, &master).await;
    let note = NoteId::random();
    let photo = png(640, 480, (2 << 20) + 17);
    let thumb = png(64, 48, 900);
    let staged = store
        .import(
            note,
            "  kitchen.png ",
            &photo,
            Some(ThumbnailInput {
                bytes: thumb.clone(),
            }),
            ImportHints::default(),
        )
        .await
        .unwrap();
    assert_eq!(staged.media.name, "kitchen.png");
    assert_eq!(staged.media.width, Some(640));
    assert_eq!(staged.media.original.chunks(), 3);
    drop(store);

    let store = open(&rows, &master).await;
    assert_eq!(store.staged().await.unwrap(), vec![staged.clone()]);
    assert!(store.has(note, &staged.media.original).await);
    let thumbnail = &staged.media.thumbnail.as_ref().unwrap().media;
    assert!(store.has(note, thumbnail).await);
    assert_eq!(
        store
            .read(note, staged.id, &staged.media, Variant::Original)
            .await
            .unwrap(),
        photo
    );
    assert_eq!(
        store
            .read(note, staged.id, &staged.media, Variant::Thumbnail)
            .await
            .unwrap(),
        thumb
    );
    // One chunk alone, as a player seeking reads it.
    assert_eq!(
        store
            .read_chunk(note, staged.id, &staged.media, Variant::Original, 2)
            .await
            .unwrap(),
        photo[2 << 20..]
    );

    // Another master key opens no record, and says so by returning none.
    let stranger = WebMediaStore::open(rows.clone(), MasterKey::random())
        .await
        .unwrap();
    assert!(stranger.staged().await.unwrap().is_empty());
}

#[tokio::test]
async fn nothing_in_the_rows_is_plaintext() {
    let rows = Arc::new(MemoryRows::default());
    let store = open(&rows, &MasterKey::random()).await;
    let note = NoteId::random();
    let photo = png(10, 10, 5000);
    store
        .import(
            note,
            "secret-name.png",
            &photo,
            None,
            ImportHints::default(),
        )
        .await
        .unwrap();
    for (key, value) in rows.snapshot() {
        assert!(!key.contains("secret-name"), "{key}");
        assert!(!contains(&value, &photo[100..164]), "{key}");
        assert!(!contains(&value, b"secret-name"), "{key}");
    }
}

#[tokio::test]
async fn staged_imports_come_back_oldest_first() {
    let rows = Arc::new(MemoryRows::default());
    let master = MasterKey::random();
    let store = open(&rows, &master).await;
    let note = NoteId::random();
    let mut ids = Vec::new();
    for n in 0..3 {
        let staged = store
            .import(
                note,
                &format!("{n}.wav"),
                &wav(100),
                None,
                ImportHints::default(),
            )
            .await
            .unwrap();
        ids.push(staged.id);
    }
    // And after a reload, the next is still after the ones before.
    let store = open(&rows, &master).await;
    ids.push(
        store
            .import(note, "3.wav", &wav(100), None, ImportHints::default())
            .await
            .unwrap()
            .id,
    );
    let order: Vec<_> = store
        .staged()
        .await
        .unwrap()
        .into_iter()
        .map(|s| s.id)
        .collect();
    assert_eq!(order, ids);

    store.settle(ids[1]).await.unwrap();
    store.settle(ids[1]).await.unwrap();
    assert_eq!(store.staged().await.unwrap().len(), 3);
}

#[tokio::test]
async fn imports_are_refused_as_the_directory_refuses_them() {
    let store = open(&Arc::new(MemoryRows::default()), &MasterKey::random()).await;
    let note = NoteId::random();
    let refused = |r: easynotes_core::Result<_>| r.is_err();
    assert!(refused(
        store
            .import(note, "x.png", b"", None, ImportHints::default())
            .await
    ));
    assert!(refused(
        store
            .import(
                note,
                "notes.txt",
                b"plain words, not a picture",
                None,
                ImportHints::default()
            )
            .await
    ));
    assert!(refused(
        store
            .import(
                note,
                "huge.png",
                &png(10_000, 10_000, 100),
                None,
                ImportHints::default()
            )
            .await
    ));
    assert!(refused(
        store
            .import(
                note,
                "a.wav",
                &wav(100),
                Some(ThumbnailInput {
                    bytes: png(10, 10, 100)
                }),
                ImportHints::default()
            )
            .await
    ));
    assert!(refused(
        store
            .import(
                note,
                "big-thumb.png",
                &png(2000, 2000, 1000),
                Some(ThumbnailInput {
                    bytes: png(1024, 1024, 100)
                }),
                ImportHints::default()
            )
            .await
    ));
    assert!(store.staged().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_refused_write_leaves_no_record_and_nothing_whole() {
    let rows = Arc::new(MemoryRows::default());
    let master = MasterKey::random();
    let store = open(&rows, &master).await;
    let note = NoteId::random();
    rows.refuse_writes(true);
    assert!(store
        .import(
            note,
            "x.png",
            &png(4, 4, 3 << 20),
            None,
            ImportHints::default()
        )
        .await
        .is_err());
    rows.refuse_writes(false);
    assert!(store.staged().await.unwrap().is_empty());
    assert!(rows.snapshot().is_empty());
}

/// Rows that accept `left` more writes and then refuse, as a quota running
/// out part way through an import does.
struct Failing {
    rows: Arc<MemoryRows>,
    left: std::sync::atomic::AtomicUsize,
}

impl MediaRows for Failing {
    fn get<'a>(
        &'a self,
        key: &'a str,
    ) -> easynotes_core::attachment::storage::Fut<'a, easynotes_core::Result<Option<Vec<u8>>>> {
        self.rows.get(key)
    }

    fn keys<'a>(
        &'a self,
        prefix: &'a str,
    ) -> easynotes_core::attachment::storage::Fut<'a, easynotes_core::Result<Vec<String>>> {
        self.rows.keys(prefix)
    }

    fn apply(
        &self,
        changes: Vec<Change>,
    ) -> easynotes_core::attachment::storage::Fut<'_, easynotes_core::Result<()>> {
        use std::sync::atomic::Ordering;
        // A compare-and-swap loop rather than `fetch_update`, which Rust 1.99
        // deprecates in favour of a `try_update` that 1.82 does not have.
        let mut left = self.left.load(Ordering::SeqCst);
        loop {
            let Some(next) = left.checked_sub(1) else {
                return Box::pin(std::future::ready(Err(easynotes_core::CoreError::Storage(
                    "quota".into(),
                ))));
            };
            match self
                .left
                .compare_exchange_weak(left, next, Ordering::SeqCst, Ordering::SeqCst)
            {
                Ok(_) => break,
                Err(actual) => left = actual,
            }
        }
        self.rows.apply(changes)
    }
}

#[tokio::test]
async fn an_import_cut_off_at_any_write_is_all_or_nothing_after_a_reload() {
    let note = NoteId::random();
    let photo = png(8, 8, (3 << 20) + 5);
    let thumb = png(8, 8, 200);
    // A thumbnail chunk, four original chunks, and the commit: six writes.
    for allowed in 0..=6 {
        let rows = Arc::new(MemoryRows::default());
        let master = MasterKey::random();
        let failing = Failing {
            rows: rows.clone(),
            left: allowed.into(),
        };
        let store = WebMediaStore::open(failing, master.clone()).await.unwrap();
        let result = store
            .import(
                note,
                "x.png",
                &photo,
                Some(ThumbnailInput {
                    bytes: thumb.clone(),
                }),
                ImportHints::default(),
            )
            .await;
        drop(store);
        // The clean-up after a refusal is refused too, so what is left is
        // what a crash at that write would leave. A reload settles it.
        let store = open(&rows, &master).await;
        let staged = store.staged().await.unwrap();
        match result {
            Ok(done) => {
                assert_eq!(allowed, 6);
                assert_eq!(staged, vec![done.clone()]);
                assert_eq!(
                    store
                        .read(note, done.id, &done.media, Variant::Original)
                        .await
                        .unwrap(),
                    photo
                );
            }
            Err(_) => {
                assert!(staged.is_empty(), "after {allowed} writes");
                assert!(rows.snapshot().is_empty(), "after {allowed} writes");
            }
        }
    }
}

#[tokio::test]
async fn a_download_is_kept_only_once_it_is_whole_and_the_file_attached() {
    let sender = open(&Arc::new(MemoryRows::default()), &MasterKey::random()).await;
    let rows = Arc::new(MemoryRows::default());
    let master = MasterKey::random();
    let receiver = open(&rows, &master).await;
    let note = NoteId::random();
    let photo = png(2, 2, (2 << 20) + 9);
    let staged = sender
        .import(note, "x.png", &photo, None, ImportHints::default())
        .await
        .unwrap();
    let original = &staged.media.original;
    let mut chunks = Vec::new();
    for i in 0..original.chunks() {
        chunks.push(sender.read_sealed_chunk(note, original, i).await.unwrap());
    }
    assert!(sender
        .read_sealed_chunk(note, original, original.chunks())
        .await
        .is_err());

    let mut altered = chunks.clone();
    altered[1][7] ^= 1;
    let short = chunks[..2].to_vec();
    let mut long = chunks.clone();
    long.push(vec![0; 10]);
    let mut odd = chunks.clone();
    odd[0].truncate(CHUNK_OVERHEAD + 5);
    for bad in [altered, short, long, odd] {
        let mut incoming = receiver.incoming(note, original).await.unwrap();
        let mut written = Ok(());
        for chunk in &bad {
            written = incoming.write(chunk).await;
            if written.is_err() {
                break;
            }
        }
        if written.is_ok() {
            assert!(incoming.finish().await.is_err());
        }
        assert!(!receiver.has(note, original).await);
        assert!(receiver
            .read(note, staged.id, &staged.media, Variant::Original)
            .await
            .is_err());
    }
    // What those left is not whole, and a reload takes it.
    drop(receiver);
    let receiver = open(&rows, &master).await;
    assert!(rows.snapshot().is_empty());

    let mut incoming = receiver.incoming(note, original).await.unwrap();
    for chunk in &chunks {
        incoming.write(chunk).await.unwrap();
    }
    incoming.finish().await.unwrap();
    assert!(receiver.has(note, original).await);
    assert_eq!(
        receiver
            .read(note, staged.id, &staged.media, Variant::Original)
            .await
            .unwrap(),
        photo
    );

    // `put_sealed` is the same check on a whole buffer.
    let other = open(&Arc::new(MemoryRows::default()), &MasterKey::random()).await;
    let mut whole = chunks.concat();
    whole[40] ^= 1;
    assert!(other.put_sealed(note, original, &whole).await.is_err());
    assert!(!other.has(note, original).await);
    whole[40] ^= 1;
    other.put_sealed(note, original, &whole).await.unwrap();
    assert!(other.has(note, original).await);
}

#[tokio::test]
async fn a_tab_and_a_device_hold_the_same_bytes() {
    // What a tab uploads, a device downloads, and the other way about: both
    // keep the relay's bytes, so there is nothing to convert.
    let tab = open(&Arc::new(MemoryRows::default()), &MasterKey::random()).await;
    let dir = tempfile::tempdir().unwrap();
    let device = MediaStore::open(dir.path(), MasterKey::random()).unwrap();
    let note = NoteId::random();

    let photo = png(3, 3, (1 << 20) + 3);
    let from_tab = tab
        .import(note, "t.png", &photo, None, ImportHints::default())
        .await
        .unwrap();
    let sealed = tab
        .read_sealed(note, &from_tab.media.original)
        .await
        .unwrap();
    device
        .put_sealed(note, &from_tab.media.original, &sealed)
        .unwrap();
    assert_eq!(
        device
            .read(note, from_tab.id, &from_tab.media, Variant::Original)
            .unwrap(),
        photo
    );

    let sound = wav(3000);
    let from_device = device
        .import(
            note,
            "d.wav",
            sound.as_slice(),
            sound.len() as u64,
            None,
            ImportHints::default(),
        )
        .unwrap();
    let sealed = device
        .read_sealed(note, &from_device.media.original)
        .unwrap();
    tab.put_sealed(note, &from_device.media.original, &sealed)
        .await
        .unwrap();
    assert_eq!(
        tab.read(note, from_device.id, &from_device.media, Variant::Original)
            .await
            .unwrap(),
        sound
    );
}

#[tokio::test]
async fn discarding_purging_and_collecting_take_only_what_they_should() {
    let rows = Arc::new(MemoryRows::default());
    let store = open(&rows, &MasterKey::random()).await;
    let (note, other) = (NoteId::random(), NoteId::random());
    let import = |note, name: &'static str| {
        let store = &store;
        async move {
            store
                .import(note, name, &wav(500), None, ImportHints::default())
                .await
                .unwrap()
        }
    };
    let live = import(note, "live.wav").await;
    let dropped = import(note, "dropped.wav").await;
    let pending = import(note, "pending.wav").await;
    let elsewhere = import(other, "elsewhere.wav").await;
    store.settle(live.id).await.unwrap();
    store.settle(dropped.id).await.unwrap();

    // A download under way: chunks that are not whole yet.
    let sender = open(&Arc::new(MemoryRows::default()), &MasterKey::random()).await;
    let arriving = sender
        .import(
            note,
            "arriving.wav",
            &wav(500),
            None,
            ImportHints::default(),
        )
        .await
        .unwrap();
    let chunk = sender
        .read_sealed_chunk(note, &arriving.media.original, 0)
        .await
        .unwrap();
    let mut incoming = store
        .incoming(note, &arriving.media.original)
        .await
        .unwrap();
    incoming.write(&chunk).await.unwrap();

    // `dropped` is named by nothing; `pending` by its record; the download by
    // nobody yet, but it is not whole, so it is not the collector's.
    assert_eq!(
        store.collect_garbage(note, &[&live.media]).await.unwrap(),
        1
    );
    assert!(store.has(note, &live.media.original).await);
    assert!(!store.has(note, &dropped.media.original).await);
    assert!(store.has(note, &pending.media.original).await);
    incoming.finish().await.unwrap();
    assert!(store.has(note, &arriving.media.original).await);

    store.discard(&pending).await.unwrap();
    assert!(!store.has(note, &pending.media.original).await);
    assert_eq!(store.staged().await.unwrap(), vec![elsewhere.clone()]);

    store.purge_note(other).await.unwrap();
    assert!(store.staged().await.unwrap().is_empty());
    assert!(!store.has(other, &elsewhere.media.original).await);
    assert!(store.has(note, &live.media.original).await);

    store.purge_note(note).await.unwrap();
    assert!(rows.snapshot().is_empty());
}
