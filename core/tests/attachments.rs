//! Attachments: the sealed format, the local media store, and the descriptors
//! in both CRDT backends ([ADR 0027](../../docs/adr/0027-attachments.md)).

use easynotes_core::attachment::seal::{seal_whole, verify_sealed, CHUNK_OVERHEAD};
use easynotes_core::attachment::store::{ImportHints, MediaStore, ThumbnailInput};
use easynotes_core::attachment::{
    AttachmentKind, AttachmentState, ContentKey, Media, MediaFormat, MediaOpener, MediaSealer,
    Variant, MAX_ATTACHMENTS_PER_NOTE,
};
use easynotes_core::crdt::{NoteDoc, NoteView};
use easynotes_core::crypto::MasterKey;
use easynotes_core::ids::{AttachmentId, NoteId};
use easynotes_core::note::{NoteCore, NoteKind};
use easynotes_core::store::{MemoryKeyring, MemoryStore};
use easynotes_core::CoreError;

type Core<D> = NoteCore<D, MemoryStore, MemoryKeyring>;

/// A PNG header claiming `w` × `h`, then filler to `len` bytes. Nothing here
/// decodes pixels, so a header is all an image has to be.
fn png(w: u32, h: u32, len: usize) -> Vec<u8> {
    let mut bytes = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
    bytes.extend_from_slice(&w.to_be_bytes());
    bytes.extend_from_slice(&h.to_be_bytes());
    bytes.resize(len, 0x5A);
    bytes
}

fn wav(len: usize) -> Vec<u8> {
    let mut bytes = b"RIFF\0\0\0\0WAVEfmt ".to_vec();
    bytes.resize(len, 0x11);
    bytes
}

fn store() -> (tempfile::TempDir, MediaStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = MediaStore::open(dir.path(), MasterKey::random()).unwrap();
    (dir, store)
}

#[test]
fn a_local_attachment_can_be_restaged_after_restart_without_resealing() {
    let dir = tempfile::tempdir().unwrap();
    let key = MasterKey::random();
    let store = MediaStore::open(dir.path(), key.clone()).unwrap();
    let note = NoteId::random();
    let photo = png(8, 8, 500);
    let staged = store.import(note, "local.png", &photo[..], photo.len() as u64,
        None, ImportHints::default()).unwrap();
    let sealed = store.read_sealed(note, &staged.media.original).unwrap();
    store.settle(staged.id).unwrap();
    store.restage(&staged).unwrap();
    drop(store);
    let reopened = MediaStore::open(dir.path(), key).unwrap();
    assert_eq!(reopened.staged().unwrap(), vec![staged.clone()]);
    assert_eq!(reopened.read_sealed(note, &staged.media.original).unwrap(), sealed);
    assert_eq!(reopened.read(note, staged.id, &staged.media, Variant::Original).unwrap(), photo);
}

// ---------------------------------------------------------------------------
// The sealed format
// ---------------------------------------------------------------------------

fn sealed(
    len: usize,
    chunk: u32,
) -> (
    ContentKey,
    NoteId,
    AttachmentId,
    Vec<u8>,
    Vec<u8>,
    easynotes_core::attachment::MediaRef,
) {
    let key = ContentKey::random();
    let (note, id) = (NoteId::random(), AttachmentId::random());
    let plain: Vec<u8> = (0..len).map(|i| (i * 7 % 251) as u8).collect();
    let (media, bytes) = seal_whole(&key, note, id, Variant::Original, &plain, chunk).unwrap();
    (key, note, id, plain, bytes, media)
}

#[test]
fn a_file_round_trips_across_chunk_boundaries() {
    for len in [1, 99, 100, 101, 1000] {
        let (key, note, id, plain, bytes, media) = sealed(len, 100);
        assert_eq!(media.chunks(), len.div_ceil(100) as u32);
        assert_eq!(bytes.len() as u64, media.sealed_len());
        let opener = MediaOpener::new(&key, note, id, Variant::Original, &media).unwrap();
        assert_eq!(opener.open_whole(&bytes).unwrap(), plain);
        // And one chunk alone, without the ones before it.
        let last = opener.chunks() - 1;
        let (at, n) = opener.sealed_range(last);
        let tail = opener
            .open_chunk(last, &bytes[at as usize..at as usize + n])
            .unwrap();
        assert_eq!(tail, plain[last as usize * 100..]);
    }
}

#[test]
fn nothing_but_the_right_key_and_place_opens_a_chunk() {
    let (key, note, id, _, bytes, media) = sealed(250, 100);
    let first = &bytes[..100 + CHUNK_OVERHEAD];
    let open = |key: &ContentKey, note, id, variant| {
        MediaOpener::new(key, note, id, variant, &media)
            .unwrap()
            .open_chunk(0, first)
    };
    assert!(open(&key, note, id, Variant::Original).is_ok());
    assert!(open(&ContentKey::random(), note, id, Variant::Original).is_err());
    assert!(open(&key, NoteId::random(), id, Variant::Original).is_err());
    assert!(open(&key, note, AttachmentId::random(), Variant::Original).is_err());
    // The same bytes presented as the thumbnail: another subkey, another AAD.
    assert!(open(&key, note, id, Variant::Thumbnail).is_err());

    let opener = MediaOpener::new(&key, note, id, Variant::Original, &media).unwrap();
    // A chunk moved to another index.
    let second = &bytes[100 + CHUNK_OVERHEAD..2 * (100 + CHUNK_OVERHEAD)];
    assert!(opener.open_chunk(0, second).is_err());
    // One flipped bit.
    let mut altered = first.to_vec();
    altered[CHUNK_OVERHEAD + 3] ^= 1;
    assert!(opener.open_chunk(0, &altered).is_err());
}

#[test]
fn a_file_cut_short_padded_or_resealed_fails_closed() {
    let (key, note, id, plain, bytes, media) = sealed(250, 100);
    let opener = MediaOpener::new(&key, note, id, Variant::Original, &media).unwrap();

    // Truncated to its first two chunks: wrong length before any decryption.
    let cut = &bytes[..2 * (100 + CHUNK_OVERHEAD)];
    assert!(opener.open_whole(cut).is_err());
    // A descriptor lying about the length to match does not help: every chunk
    // committed to the real length and count.
    let mut short = media.clone();
    short.len = 200;
    let liar = MediaOpener::new(&key, note, id, Variant::Original, &short).unwrap();
    assert!(liar.open_chunk(0, &bytes[..100 + CHUNK_OVERHEAD]).is_err());

    let mut padded = bytes.clone();
    padded.push(0);
    assert!(verify_sealed(&media, &padded).is_err());

    // Someone else holding the key seals the same plaintext again: every chunk
    // opens, but it is not the file the descriptor committed to.
    let mut sealer = MediaSealer::new(&key, note, id, Variant::Original, 250, 100).unwrap();
    let mut again = Vec::new();
    for chunk in plain.chunks(100) {
        again.extend(sealer.seal_chunk(chunk).unwrap());
    }
    assert!(verify_sealed(&media, &again).is_err());
    assert!(verify_sealed(&media, &bytes).is_ok());
}

#[test]
fn a_sealer_refuses_a_file_that_changes_size() {
    let key = ContentKey::random();
    let mut sealer = MediaSealer::new(
        &key,
        NoteId::random(),
        AttachmentId::random(),
        Variant::Original,
        150,
        100,
    )
    .unwrap();
    assert!(sealer.seal_chunk(&[0; 99]).is_err());
    sealer.seal_chunk(&[0; 100]).unwrap();
    // Stopping one chunk early is not a file.
    assert!(matches!(sealer.finish(), Err(CoreError::Attachment(_))));
}

// ---------------------------------------------------------------------------
// The media store
// ---------------------------------------------------------------------------

#[test]
fn an_import_is_sealed_staged_and_readable() {
    let (dir, store) = store();
    let note = NoteId::random();
    let photo = png(1600, 1200, 3 * (1 << 20) + 5);
    let thumb = png(512, 384, 900);
    let staged = store
        .import(
            note,
            "  kitchen.png ",
            photo.as_slice(),
            photo.len() as u64,
            Some(ThumbnailInput {
                bytes: thumb.clone(),
            }),
            ImportHints::default(),
        )
        .unwrap();
    let media = &staged.media;
    assert_eq!(media.format, MediaFormat::Png);
    assert_eq!(media.name, "kitchen.png");
    assert_eq!((media.width, media.height), (Some(1600), Some(1200)));
    assert_eq!(media.original.chunks(), 4);
    let t = media.thumbnail.as_ref().unwrap();
    assert_eq!((t.width, t.height), (512, 384));

    assert_eq!(store.staged().unwrap(), vec![staged.clone()]);
    assert_eq!(
        store
            .read(note, staged.id, media, Variant::Original)
            .unwrap(),
        photo
    );
    assert_eq!(
        store
            .read(note, staged.id, media, Variant::Thumbnail)
            .unwrap(),
        thumb
    );
    assert_eq!(
        store
            .read_chunk(note, staged.id, media, Variant::Original, 3)
            .unwrap(),
        photo[3 << 20..]
    );

    // No plaintext on disk: the filler run is nowhere in any stored file.
    let needle = vec![0x5A; 64];
    for entry in walk(dir.path()) {
        let bytes = std::fs::read(&entry).unwrap();
        assert!(
            !bytes.windows(needle.len()).any(|w| w == needle.as_slice()),
            "plaintext in {entry:?}"
        );
    }

    // The record survives a restart under the same master key, and not under
    // another one.
    let master = MasterKey::random();
    let dir2 = tempfile::tempdir().unwrap();
    let s1 = MediaStore::open(dir2.path(), master.clone()).unwrap();
    let st = s1
        .import(
            note,
            "a.wav",
            wav(4000).as_slice(),
            4000,
            None,
            ImportHints {
                duration_ms: Some(25),
            },
        )
        .unwrap();
    drop(s1);
    let s2 = MediaStore::open(dir2.path(), master).unwrap();
    assert_eq!(s2.staged().unwrap(), vec![st.clone()]);
    assert_eq!(st.media.duration_ms, Some(25));
    let s3 = MediaStore::open(dir2.path(), MasterKey::random()).unwrap();
    assert!(s3.staged().unwrap().is_empty());
}

fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            out.extend(walk(&path));
        } else {
            out.push(path);
        }
    }
    out
}

#[test]
fn imports_are_refused_for_what_they_are_not_what_they_are_called() {
    let (dir, store) = store();
    let note = NoteId::random();
    let import = |name: &str, bytes: &[u8]| {
        store.import(
            note,
            name,
            bytes,
            bytes.len() as u64,
            None,
            ImportHints::default(),
        )
    };
    let pdf = b"%PDF-1.7 not a picture".to_vec();
    assert!(matches!(
        import("photo.jpg", &pdf),
        Err(CoreError::Attachment(_))
    ));
    assert!(import("empty.png", &[]).is_err());
    assert!(
        import("huge.png", &png(8000, 6000, 1000)).is_err(),
        "48 megapixels"
    );
    assert!(import("", &png(10, 10, 100)).is_err(), "no name");
    // A file that is not the length it was said to be.
    let short = png(10, 10, 100);
    assert!(store
        .import(
            note,
            "a.png",
            short.as_slice(),
            200,
            None,
            ImportHints::default()
        )
        .is_err());
    assert!(store
        .import(
            note,
            "a.png",
            short.as_slice(),
            50,
            None,
            ImportHints::default()
        )
        .is_err());
    // A thumbnail that is not small, or is on a sound file.
    let big_thumb = ThumbnailInput {
        bytes: png(1024, 10, 100),
    };
    assert!(store
        .import(
            note,
            "a.png",
            short.as_slice(),
            100,
            Some(big_thumb),
            ImportHints::default()
        )
        .is_err());
    let w = wav(100);
    let thumb = ThumbnailInput {
        bytes: png(10, 10, 100),
    };
    assert!(store
        .import(
            note,
            "a.wav",
            w.as_slice(),
            100,
            Some(thumb),
            ImportHints::default()
        )
        .is_err());

    // Nothing refused left anything behind.
    assert!(store.staged().unwrap().is_empty());
    assert!(walk(dir.path()).is_empty(), "{:?}", walk(dir.path()));
}

#[test]
fn settling_discarding_and_collecting() {
    let (dir, store) = store();
    let note = NoteId::random();
    let one = store
        .import(
            note,
            "1.png",
            png(1, 1, 50).as_slice(),
            50,
            None,
            ImportHints::default(),
        )
        .unwrap();
    let two = store
        .import(
            note,
            "2.png",
            png(1, 1, 60).as_slice(),
            60,
            None,
            ImportHints::default(),
        )
        .unwrap();

    // Settled: the record goes, the object stays.
    store.settle(one.id).unwrap();
    store.settle(one.id).unwrap();
    assert_eq!(store.staged().unwrap(), vec![two.clone()]);
    assert!(store.has(note, &one.media.original));

    // Garbage is what neither a live descriptor nor a staged import names.
    // `one` is settled and not live, `two` is staged, and a stray file from a
    // crash before its record is named by nothing.
    let stray = dir
        .path()
        .join("objects")
        .join(note.encode())
        .join(easynotes_core::ids::MediaObjectId::random().encode());
    std::fs::write(&stray, b"half an import").unwrap();
    assert_eq!(store.collect_garbage(note, &[]).unwrap(), 2);
    assert!(!store.has(note, &one.media.original));
    assert!(store.has(note, &two.media.original));

    store.discard(&two).unwrap();
    assert!(store.staged().unwrap().is_empty());
    assert!(!store.has(note, &two.media.original));

    // Half-written files in `tmp/` are swept on open.
    std::fs::write(dir.path().join("tmp").join("partial"), b"x").unwrap();
    let _ = MediaStore::open(dir.path(), MasterKey::random()).unwrap();
    assert!(!dir.path().join("tmp").join("partial").exists());
}

#[test]
fn a_download_is_kept_only_if_it_is_the_file_attached() {
    let (_d1, sender) = store();
    let (_d2, receiver) = store();
    let note = NoteId::random();
    let staged = sender
        .import(
            note,
            "x.png",
            png(2, 2, 3000).as_slice(),
            3000,
            None,
            ImportHints::default(),
        )
        .unwrap();
    let bytes = sender.read_sealed(note, &staged.media.original).unwrap();

    let mut altered = bytes.clone();
    altered[100] ^= 1;
    assert!(receiver
        .put_sealed(note, &staged.media.original, &altered)
        .is_err());
    assert!(!receiver.has(note, &staged.media.original));

    receiver
        .put_sealed(note, &staged.media.original, &bytes)
        .unwrap();
    receiver
        .put_sealed(note, &staged.media.original, &bytes)
        .unwrap();
    assert_eq!(
        receiver
            .read(note, staged.id, &staged.media, Variant::Original)
            .unwrap(),
        png(2, 2, 3000)
    );
}

#[test]
fn a_download_arriving_in_chunks_is_checked_before_it_is_kept() {
    let (_d1, sender) = store();
    let (d2, receiver) = store();
    let note = NoteId::random();
    let plain = png(2, 2, (2 << 20) + 9);
    let staged = sender
        .import(
            note,
            "x.png",
            plain.as_slice(),
            plain.len() as u64,
            None,
            ImportHints::default(),
        )
        .unwrap();
    let original = &staged.media.original;
    let chunks: Vec<Vec<u8>> = (0..original.chunks())
        .map(|i| sender.read_sealed_chunk(note, original, i).unwrap())
        .collect();
    assert_eq!(chunks.concat(), sender.read_sealed(note, original).unwrap());
    assert!(sender
        .read_sealed_chunk(note, original, original.chunks())
        .is_err());

    // A flipped bit, a missing chunk, and one chunk too many: none is kept,
    // and nothing is left behind in the store.
    let mut altered = chunks.clone();
    altered[1][7] ^= 1;
    let short = chunks[..2].to_vec();
    let mut long = chunks.clone();
    long.push(vec![0; 10]);
    for bad in [altered, short, long] {
        let mut incoming = receiver.incoming(note, original).unwrap();
        let written: Result<(), _> = bad.iter().try_for_each(|c| incoming.write(c));
        if written.is_ok() {
            assert!(incoming.finish().is_err());
        }
        assert!(!receiver.has(note, original));
    }
    drop(receiver);
    assert!(walk(d2.path())
        .iter()
        .all(|p| !p.starts_with(d2.path().join("objects"))));

    let receiver = MediaStore::open(d2.path(), MasterKey::random()).unwrap();
    let mut incoming = receiver.incoming(note, original).unwrap();
    for chunk in &chunks {
        incoming.write(chunk).unwrap();
    }
    incoming.finish().unwrap();
    assert_eq!(
        receiver
            .read(note, staged.id, &staged.media, Variant::Original)
            .unwrap(),
        plain
    );
}

#[test]
fn purging_a_note_takes_its_media_and_its_staged_imports() {
    let (_dir, store) = store();
    let (a, b) = (NoteId::random(), NoteId::random());
    let on_a = store
        .import(
            a,
            "a.png",
            png(1, 1, 40).as_slice(),
            40,
            None,
            ImportHints::default(),
        )
        .unwrap();
    let on_b = store
        .import(
            b,
            "b.png",
            png(1, 1, 40).as_slice(),
            40,
            None,
            ImportHints::default(),
        )
        .unwrap();
    store.purge_note(a).unwrap();
    assert!(!store.has(a, &on_a.media.original));
    assert_eq!(store.staged().unwrap(), vec![on_b.clone()]);
    assert!(store.has(b, &on_b.media.original));
}

// ---------------------------------------------------------------------------
// Descriptors in the note, on both backends
// ---------------------------------------------------------------------------

fn core<D: NoteDoc>() -> Core<D> {
    NoteCore::open(MemoryStore::new(), MemoryKeyring::new()).unwrap()
}

fn pair<D: NoteDoc>(kind: NoteKind) -> (Core<D>, Core<D>, NoteId) {
    let mut a = core::<D>();
    let id = a.create_note(kind).unwrap();
    let blob = a.export_share_blob(id).unwrap();
    let mut b = core::<D>();
    b.import_share_blob(&blob).unwrap();
    // A text note is created with an edit of its own, which the other side
    // needs before anything that follows it.
    let created: Vec<Vec<u8>> = a
        .unsent(id, None)
        .unwrap()
        .into_iter()
        .map(|(_, e)| e)
        .collect();
    deliver(&mut b, id, &created);
    (a, b, id)
}

fn deliver<D: NoteDoc>(to: &mut Core<D>, id: NoteId, envelopes: &[Vec<u8>]) {
    for e in envelopes {
        to.apply_remote(id, e).unwrap();
    }
}

/// A descriptor for a file that was never stored. The note does not look at
/// bytes, only at what it is told about them.
fn media(name: &str, len: usize) -> (AttachmentId, Media) {
    let key = ContentKey::random();
    let id = AttachmentId::random();
    let (original, _) = seal_whole(
        &key,
        NoteId::random(),
        id,
        Variant::Original,
        &png(4, 3, len),
        1 << 20,
    )
    .unwrap();
    (
        id,
        Media {
            v: 1,
            format: MediaFormat::Png,
            name: name.into(),
            width: Some(4),
            height: Some(3),
            duration_ms: None,
            key: Some(key),
            original,
            thumbnail: None,
        },
    )
}

/// The same, with a thumbnail, as a picture imported on a device has.
fn media_with_thumbnail(name: &str) -> (AttachmentId, Media) {
    let (id, mut m) = media(name, 100);
    let (thumb, _) = seal_whole(
        m.key.as_ref().unwrap(),
        NoteId::random(),
        id,
        Variant::Thumbnail,
        &png(4, 3, 60),
        1 << 20,
    )
    .unwrap();
    m.thumbnail = Some(easynotes_core::attachment::Thumbnail {
        media: thumb,
        format: MediaFormat::Png,
        width: 4,
        height: 3,
    });
    (id, m)
}

fn names(view: &NoteView) -> Vec<&str> {
    view.attachments
        .iter()
        .map(|a| a.media.name.as_str())
        .collect()
}

fn attachments_travel_and_survive_conversion<D: NoteDoc>() {
    let (mut a, mut b, id) = pair::<D>(NoteKind::Text);
    let mut wire = vec![a.set_body(id, "eggs\nflour").unwrap().unwrap()];
    for name in ["one.png", "two.png", "three.png"] {
        let (att, m) = media(name, 100);
        wire.push(a.add_attachment(id, att, m).unwrap());
    }
    deliver(&mut b, id, &wire);
    assert_eq!(
        names(&b.view(id).unwrap()),
        ["one.png", "two.png", "three.png"]
    );
    assert_eq!(a.view(id).unwrap(), b.view(id).unwrap());

    let to_list = b.set_kind(id, NoteKind::Checklist).unwrap().unwrap();
    deliver(&mut a, id, &[to_list]);
    let view = a.view(id).unwrap();
    assert_eq!(view.kind, NoteKind::Checklist);
    assert_eq!(view.items.len(), 2);
    assert_eq!(names(&view), ["one.png", "two.png", "three.png"]);
    let back = a.set_kind(id, NoteKind::Text).unwrap().unwrap();
    deliver(&mut b, id, &[back]);
    assert_eq!(
        names(&b.view(id).unwrap()),
        ["one.png", "two.png", "three.png"]
    );
    assert_eq!(a.view(id).unwrap(), b.view(id).unwrap());

    // The card and the search know about them without opening the note.
    let summary = b.notes().unwrap().into_iter().find(|s| s.id == id).unwrap();
    assert_eq!((summary.images, summary.audio), (3, 0));
    let hits = b.search("two.png").unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].matched, "two.png");
}

fn a_card_shows_the_first_picture_that_has_a_thumbnail<D: NoteDoc>() {
    let (mut a, mut b, id) = pair::<D>(NoteKind::Checklist);
    let cover = |core: &mut Core<D>| {
        core.notes()
            .unwrap()
            .into_iter()
            .find(|s| s.id == id)
            .unwrap()
            .cover
            .map(|(attachment, media)| (attachment, media.name))
    };
    assert_eq!(cover(&mut b), None);

    // A picture with no preview is not one a card can draw.
    let (bare, m0) = media("bare.png", 10);
    let (first, m1) = media_with_thumbnail("first.png");
    let (second, m2) = media_with_thumbnail("second.png");
    let wire = vec![
        a.add_attachment(id, bare, m0).unwrap(),
        a.add_attachment(id, first, m1).unwrap(),
        a.add_attachment(id, second, m2).unwrap(),
    ];
    deliver(&mut b, id, &wire);
    assert_eq!(cover(&mut b), Some((first, "first.png".into())));

    // Moving the second ahead of the first makes it the cover; removing it
    // hands the cover back.
    deliver(&mut b, id, &[a.move_attachment(id, second, 0).unwrap()]);
    assert_eq!(cover(&mut b), Some((second, "second.png".into())));
    deliver(&mut b, id, &[a.remove_attachment(id, second).unwrap()]);
    assert_eq!(cover(&mut b), Some((first, "first.png".into())));
    deliver(&mut b, id, &[a.remove_attachment(id, first).unwrap()]);
    assert_eq!(cover(&mut b), None);
}

fn removal_beats_a_concurrent_move_and_description<D: NoteDoc>() {
    let (mut a, mut b, id) = pair::<D>(NoteKind::Checklist);
    let (first, m1) = media("first.png", 10);
    let (second, m2) = media("second.png", 10);
    let wire = vec![
        a.add_attachment(id, first, m1).unwrap(),
        a.add_attachment(id, second, m2).unwrap(),
    ];
    deliver(&mut b, id, &wire);

    // Offline on both sides: A removes `first`; B moves it last and describes it.
    let from_a = vec![a.remove_attachment(id, first).unwrap()];
    let from_b = vec![
        b.move_attachment(id, first, 1).unwrap(),
        b.set_attachment_description(id, first, "the fridge door")
            .unwrap(),
    ];
    assert_eq!(names(&b.view(id).unwrap()), ["second.png", "first.png"]);
    deliver(&mut a, id, &from_b);
    deliver(&mut b, id, &from_a);
    for core in [&a, &b] {
        assert_eq!(names(&core.view(id).unwrap()), ["second.png"]);
    }
    assert_eq!(a.view(id).unwrap(), b.view(id).unwrap());

    // A late duplicate of the add does not bring it back.
    deliver(&mut b, id, &wire);
    assert_eq!(names(&b.view(id).unwrap()), ["second.png"]);

    // Nor does compaction: a snapshot keeps the tombstone.
    a.compact(id).unwrap();
    assert_eq!(names(&a.view(id).unwrap()), ["second.png"]);
    assert!(a.remove_attachment(id, first).is_err());
}

fn concurrent_adds_both_survive<D: NoteDoc>() {
    let (mut a, mut b, id) = pair::<D>(NoteKind::Checklist);
    let (x, mx) = media("from-a.png", 10);
    let (y, my) = media("from-b.png", 10);
    let from_a = a.add_attachment(id, x, mx).unwrap();
    let from_b = b.add_attachment(id, y, my).unwrap();
    deliver(&mut a, id, &[from_b]);
    deliver(&mut b, id, &[from_a]);
    assert_eq!(a.view(id).unwrap(), b.view(id).unwrap());
    assert_eq!(a.view(id).unwrap().attachments.len(), 2);
}

fn limits_are_held<D: NoteDoc>() {
    let mut a = core::<D>();
    let id = a.create_note(NoteKind::Text).unwrap();
    for n in 0..MAX_ATTACHMENTS_PER_NOTE {
        let (att, m) = media(&format!("{n}.png"), 10);
        a.add_attachment(id, att, m).unwrap();
    }
    let (att, m) = media("one-too-many.png", 10);
    assert!(matches!(
        a.add_attachment(id, att, m),
        Err(CoreError::Attachment(_))
    ));

    // A descriptor without its key, or out of bounds, is refused before it is
    // written.
    let b_id = a.create_note(NoteKind::Text).unwrap();
    let (att, m) = media("tomb.png", 10);
    assert!(a.add_attachment(b_id, att, m.tombstone()).is_err());
    let (att, mut m) = media("big.png", 10);
    m.original.len = 21 << 20;
    assert!(a.add_attachment(b_id, att, m).is_err());
    let (att, m) = media("ok.png", 10);
    a.add_attachment(b_id, att, m).unwrap();
    assert!(a
        .set_attachment_description(b_id, att, "x".repeat(1001))
        .is_err());
}

/// The launch after a crash between publishing a staged import and settling
/// it. What the record is reconciled against has to tell "still to publish"
/// from "published and then removed by someone else" — republishing the second
/// would write a fresh map over the tombstone and bring the file back for
/// everyone on the note.
fn a_staged_import_is_reconciled_without_resurrecting_it<D: NoteDoc>() {
    let (mut a, mut b, id) = pair::<D>(NoteKind::Text);
    let (att, m) = media("receipt.png", 10);
    assert_eq!(
        a.attachment_state(id, att).unwrap(),
        AttachmentState::Pending
    );

    let add = a.add_attachment(id, att, m.clone()).unwrap();
    assert_eq!(a.attachment_state(id, att).unwrap(), AttachmentState::Live);
    deliver(&mut b, id, &[add]);
    let removal = b.remove_attachment(id, att).unwrap();
    deliver(&mut a, id, &[removal]);
    assert_eq!(
        a.attachment_state(id, att).unwrap(),
        AttachmentState::Removed
    );

    // The record still holds the live descriptor, key and all. Adding it
    // again is refused, on both devices.
    assert!(matches!(
        a.add_attachment(id, att, m.clone()),
        Err(CoreError::Attachment(_))
    ));
    assert!(b.add_attachment(id, att, m).is_err());
    assert!(a.view(id).unwrap().attachments.is_empty());
    assert_eq!(a.view(id).unwrap().retired_attachments, vec![att]);
    assert_eq!(a.view(id).unwrap(), b.view(id).unwrap());

    // A note that has gone is its own answer: discard the import.
    let gone = NoteId::random();
    assert_eq!(
        a.attachment_state(gone, att).unwrap(),
        AttachmentState::NoNote
    );
}

/// What a compaction tells the relay to keep (ADR 0027 §5): every object of
/// every attachment the note still has, thumbnails included, and nothing of
/// one that was removed, wherever the removal was made.
fn a_compaction_lists_the_objects_the_note_still_names<D: NoteDoc>() {
    let (mut a, mut b, id) = pair::<D>(NoteKind::Checklist);
    let (kept, with_thumbnail) = media_with_thumbnail("kept.png");
    let (gone, plain) = media("gone.png", 40);
    let wire = vec![
        a.add_attachment(id, kept, with_thumbnail.clone()).unwrap(),
        a.add_attachment(id, gone, plain).unwrap(),
    ];
    deliver(&mut b, id, &wire);
    // Removed on the other device: the tombstone still names its object.
    deliver(&mut a, id, &[b.remove_attachment(id, gone).unwrap()]);

    let mut want = vec![
        with_thumbnail.original.object,
        with_thumbnail.thumbnail.as_ref().unwrap().media.object,
    ];
    want.sort();
    let request = a.compaction(id, 5).unwrap().expect("a base");
    assert_eq!(request.media, Some(want));
    assert!(request.verifies(id));

    // A note with nothing attached lists nothing, which is not the same as
    // listing no list: every object it ever had may go.
    let (mut c, _, empty) = pair::<D>(NoteKind::Text);
    assert_eq!(
        c.compaction(empty, 1).unwrap().unwrap().media,
        Some(Vec::new())
    );
}

macro_rules! both_backends {
    ($($name:ident),* $(,)?) => {
        mod automerge_backend {
            $( #[test] fn $name() { super::$name::<easynotes_core::crdt::AutomergeNote>() } )*
        }
        mod yrs_backend {
            $( #[test] fn $name() { super::$name::<easynotes_core::crdt::YrsNote>() } )*
        }
    };
}

both_backends!(
    attachments_travel_and_survive_conversion,
    a_card_shows_the_first_picture_that_has_a_thumbnail,
    removal_beats_a_concurrent_move_and_description,
    concurrent_adds_both_survive,
    limits_are_held,
    a_staged_import_is_reconciled_without_resurrecting_it,
    a_compaction_lists_the_objects_the_note_still_names,
);

#[test]
fn the_default_backend_forgets_a_removed_key() {
    // Yrs collects an overwritten value, so after a removal the key is in no
    // snapshot a later joiner could be sent. Automerge keeps its history, and
    // ADR 0027 says so; this holds the default backend to it.
    use easynotes_core::crdt::{Edit, YrsNote};
    let (att, m) = media("secret.png", 10);
    let key = serde_json::to_string(m.key.as_ref().unwrap()).unwrap();
    let key = key.trim_matches('"').to_owned();
    let mut doc = YrsNote::create(&[7; 16]);
    doc.apply_local(&Edit::AddAttachment {
        id: att,
        media: Box::new(m),
        order: "V".into(),
    })
    .unwrap();
    let holds = |bytes: &[u8]| bytes.windows(key.len()).any(|w| w == key.as_bytes());
    assert!(holds(&doc.snapshot()), "the probe must find a live key");
    doc.apply_local(&Edit::RemoveAttachment { id: att })
        .unwrap();
    assert!(!holds(&doc.snapshot()));
    assert!(doc.view().attachments.is_empty());
    assert_eq!(AttachmentKind::Image, MediaFormat::Png.kind());
}

