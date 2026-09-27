//! An attachment from one device to another through a real relay: imported and
//! sealed on the first, uploaded, published into the note only once the relay
//! has committed it, downloaded and opened on the second
//! ([ADR 0027](../../docs/adr/0027-attachments.md) §3–§4).
//!
//! The descriptor travels as any edit does; how that reaches a peer is covered
//! elsewhere, so it is handed across directly here and the relay carries only
//! what is new: the bytes.

use easynotes_core::attachment::store::{ImportHints, MediaStore, ThumbnailInput};
use easynotes_core::attachment::{AttachmentState, Variant};
use easynotes_core::crdt::DefaultNoteDoc;
use easynotes_core::crypto::{DeviceIdentity, MasterKey};
use easynotes_core::note::{NoteCore, NoteKind};
use easynotes_core::store::{MemoryKeyring, MemoryStore};
use easynotes_core::sync::authenticate;
use easynotes_core::sync::media::{download_into, upload, upload_media};
use easynotes_core::CoreError;
use easynotes_relay::{MediaConfig, MediaLimits, MemoryBlobs, Relay, RelayConfig};
use std::sync::Arc;

type Core = NoteCore<DefaultNoteDoc, MemoryStore, MemoryKeyring>;

fn png(w: u32, h: u32, len: usize) -> Vec<u8> {
    let mut bytes = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
    bytes.extend_from_slice(&w.to_be_bytes());
    bytes.extend_from_slice(&h.to_be_bytes());
    bytes.extend((0..len - bytes.len()).map(|i| (i * 13 % 251) as u8));
    bytes
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

#[tokio::test]
async fn a_photo_reaches_another_device_as_ciphertext_only() {
    let blobs = Arc::new(MemoryBlobs::default());
    let relay = Relay::serve_with(
        Arc::new(easynotes_relay::MemoryStore::new()),
        "127.0.0.1:0".parse().unwrap(),
        RelayConfig {
            media: MediaConfig {
                blobs: Some(blobs.clone()),
                limits: MediaLimits::default(),
            },
            ..RelayConfig::default()
        },
    )
    .await
    .unwrap();
    let base = relay.http_base();

    let mut a: Core = NoteCore::open(MemoryStore::new(), MemoryKeyring::new()).unwrap();
    let note = a.create_note(NoteKind::Text).unwrap();
    let mut b: Core = NoteCore::open(MemoryStore::new(), MemoryKeyring::new()).unwrap();
    b.import_share_blob(&a.export_share_blob(note).unwrap())
        .unwrap();
    for (_, envelope) in a.unsent(note, None).unwrap() {
        b.apply_remote(note, &envelope).unwrap();
    }
    let (dir_a, dir_b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let media_a = MediaStore::open(dir_a.path(), MasterKey::random()).unwrap();
    let media_b = MediaStore::open(dir_b.path(), MasterKey::random()).unwrap();
    let token_a = authenticate(&base, &DeviceIdentity::generate())
        .await
        .unwrap();
    let token_b = authenticate(&base, &DeviceIdentity::generate())
        .await
        .unwrap();

    // Three chunks and a thumbnail.
    let photo = png(4000, 3000, (2 << 20) + 777);
    let thumb = png(512, 384, 9000);
    let staged = media_a
        .import(
            note,
            "market.png",
            photo.as_slice(),
            photo.len() as u64,
            Some(ThumbnailInput {
                bytes: thumb.clone(),
            }),
            ImportHints::default(),
        )
        .unwrap();

    // Not in the note until the relay has it.
    assert_eq!(
        a.attachment_state(note, staged.id).unwrap(),
        AttachmentState::Pending
    );
    upload_media(&base, &token_a, &media_a, note, &staged.media)
        .await
        .unwrap();
    let published = a
        .add_attachment(note, staged.id, staged.media.clone())
        .unwrap();
    media_a.settle(staged.id).unwrap();

    // The other device learns of it from the note and fetches the bytes.
    b.apply_remote(note, &published).unwrap();
    let view = b.view(note).unwrap();
    let attachment = view.attachment(staged.id).unwrap();
    assert_eq!(attachment.media.name, "market.png");
    let thumbnail = &attachment.media.thumbnail.as_ref().unwrap().media;
    download_into(&base, &token_b, &media_b, note, thumbnail)
        .await
        .unwrap();
    assert_eq!(
        media_b
            .read(note, staged.id, &attachment.media, Variant::Thumbnail)
            .unwrap(),
        thumb
    );
    download_into(&base, &token_b, &media_b, note, &attachment.media.original)
        .await
        .unwrap();
    assert_eq!(
        media_b
            .read(note, staged.id, &attachment.media, Variant::Original)
            .unwrap(),
        photo
    );
    // Already here: a second download asks the relay nothing.
    download_into(&base, &token_b, &media_b, note, &attachment.media.original)
        .await
        .unwrap();

    // Everything the relay holds is ciphertext.
    let held = blobs.all_bytes();
    assert_eq!(held.len(), 2);
    for bytes in held {
        assert!(!contains(&bytes, &photo[4096..4128]));
        assert!(!contains(&bytes, &thumb[4096..4128]));
    }
}

#[tokio::test]
async fn an_upload_cut_off_part_way_finishes_on_the_next_try() {
    let relay = Relay::serve(
        Arc::new(easynotes_relay::MemoryStore::new()),
        "127.0.0.1:0".parse().unwrap(),
    )
    .await
    .unwrap();
    let base = relay.http_base();
    let token = authenticate(&base, &DeviceIdentity::generate())
        .await
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let store = MediaStore::open(dir.path(), MasterKey::random()).unwrap();
    let note = easynotes_core::ids::NoteId::random();
    let photo = png(10, 10, (3 << 20) + 1);
    let staged = store
        .import(
            note,
            "long.png",
            photo.as_slice(),
            photo.len() as u64,
            None,
            ImportHints::default(),
        )
        .unwrap();
    let original = &staged.media.original;

    // The network goes after the second chunk.
    let cut = upload(&base, &token, note, original, |index| {
        Box::pin(std::future::ready(if index >= 2 {
            Err(CoreError::Storage("relay: connection reset".into()))
        } else {
            store.read_sealed_chunk(note, original, index)
        }))
    })
    .await;
    assert!(cut.is_err());

    // The next launch sends only what is missing, and says which it sent.
    let mut sent = Vec::new();
    upload(&base, &token, note, original, |index| {
        sent.push(index);
        Box::pin(std::future::ready(
            store.read_sealed_chunk(note, original, index),
        ))
    })
    .await
    .unwrap();
    assert_eq!(sent, vec![2, 3]);

    // And once more, as after a crash before the answer was heard: nothing
    // is sent and it still succeeds.
    upload(&base, &token, note, original, |_| {
        panic!("a committed object needs no chunks")
    })
    .await
    .unwrap();

    let other = tempfile::tempdir().unwrap();
    let other = MediaStore::open(other.path(), MasterKey::random()).unwrap();
    download_into(&base, &token, &other, note, original)
        .await
        .unwrap();
    assert_eq!(
        other
            .read(note, staged.id, &staged.media, Variant::Original)
            .unwrap(),
        photo
    );
}
