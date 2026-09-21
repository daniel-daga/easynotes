//! M1 acceptance for the crypto envelope: tamper, forgery and replay are rejected,
//! and the share blob round-trips at a size a QR code can carry.

use easynotes_core::crypto::*;
use easynotes_core::error::CoreError;
use easynotes_core::ids::NoteId;

fn fixture() -> (DeviceIdentity, NoteId, NoteKey) {
    (
        DeviceIdentity::generate(),
        NoteId::random(),
        NoteKey::random(),
    )
}

#[test]
fn round_trips_plaintext() {
    let (id, note, key) = fixture();
    let env = Envelope::seal(&id, note, &key, 0, b"milk, bread, eggs").unwrap();
    assert_eq!(env.open(&key).unwrap(), b"milk, bread, eggs");
}

#[test]
fn round_trips_through_the_wire_format() {
    let (id, note, key) = fixture();
    let env = Envelope::seal(&id, note, &key, 7, b"some crdt update").unwrap();
    let bytes = env.encode();
    assert_eq!(bytes.len(), env.encoded_len());
    let back = Envelope::decode(&bytes).unwrap();
    assert_eq!(back, env);
    assert_eq!(back.open(&key).unwrap(), b"some crdt update");
}

#[test]
fn ciphertext_never_contains_the_plaintext() {
    let (id, note, key) = fixture();
    let env = Envelope::seal(&id, note, &key, 0, b"asparagus").unwrap();
    let wire = env.encode();
    assert!(
        !wire.windows(9).any(|w| w == b"asparagus"),
        "plaintext leaked into the envelope"
    );
}

#[test]
fn rejects_a_tampered_ciphertext() {
    let (id, note, key) = fixture();
    let mut env = Envelope::seal(&id, note, &key, 0, b"milk").unwrap();
    env.ciphertext[0] ^= 0x01;
    assert!(matches!(env.verify(), Err(CoreError::BadSignature)));
    assert!(env.open(&key).is_err());
}

#[test]
fn rejects_a_tampered_counter() {
    let (id, note, key) = fixture();
    let mut env = Envelope::seal(&id, note, &key, 3, b"milk").unwrap();
    env.counter = 4;
    assert!(matches!(env.open(&key), Err(CoreError::BadSignature)));
}

#[test]
fn rejects_an_envelope_moved_to_another_note() {
    // The AAD binds the note ID, so an envelope cannot be cut and pasted onto a
    // different note even by someone who holds both note keys.
    let (id, note, key) = fixture();
    let env = Envelope::seal(&id, note, &key, 0, b"milk").unwrap();
    let mut moved = env.clone();
    moved.note_id = NoteId::random();
    assert!(matches!(moved.open(&key), Err(CoreError::BadSignature)));
}

#[test]
fn rejects_an_update_from_an_unknown_key() {
    // Forgery: someone re-signs a stolen ciphertext under their own key. The
    // signature verifies, but the AAD still names the original author, so the
    // decryption fails.
    let (author, note, key) = fixture();
    let attacker = DeviceIdentity::generate();
    let real = Envelope::seal(&author, note, &key, 0, b"milk").unwrap();

    let mut forged = real.clone();
    forged.author = attacker.public_key();
    // Re-sign so the signature check itself passes.
    let mut signed = Vec::new();
    signed.push(ENVELOPE_VERSION);
    signed.extend_from_slice(note.as_bytes());
    signed.extend_from_slice(&forged.author.0);
    signed.extend_from_slice(&0u64.to_be_bytes());
    signed.extend_from_slice(&key.id());
    signed.push(0); // EnvelopeKind::Update
    signed.extend_from_slice(&forged.nonce);
    signed.extend_from_slice(&forged.ciphertext);
    forged.signature = attacker.sign(&signed);

    assert!(
        forged.verify().is_ok(),
        "the re-signed envelope should pass the signature check"
    );
    assert!(
        forged.open(&key).is_err(),
        "but it must not decrypt under the moved author"
    );
}

#[test]
fn a_swapped_signature_does_not_verify() {
    let (author, note, key) = fixture();
    let other = DeviceIdentity::generate();
    let mut env = Envelope::seal(&author, note, &key, 0, b"milk").unwrap();
    let decoy = Envelope::seal(&other, note, &key, 0, b"milk").unwrap();
    env.signature = decoy.signature;
    assert!(matches!(env.verify(), Err(CoreError::BadSignature)));
}

#[test]
fn does_not_decrypt_under_the_wrong_note_key() {
    let (id, note, key) = fixture();
    let env = Envelope::seal(&id, note, &key, 0, b"milk").unwrap();
    assert!(env.open(&NoteKey::random()).is_err());
}

#[test]
fn a_replayed_envelope_is_byte_identical_and_so_deduplicable() {
    // Replay protection is the (author, counter) pair: a replayed envelope is the
    // same envelope, so a receiver that tracks seen pairs drops it. Two *different*
    // updates from one author can never share a counter.
    let (id, note, key) = fixture();
    let env = Envelope::seal(&id, note, &key, 9, b"milk").unwrap();
    let replay = Envelope::decode(&env.encode()).unwrap();
    assert_eq!((replay.author, replay.counter), (env.author, env.counter));
    assert_eq!(replay.encode(), env.encode());
}

#[test]
fn every_seal_uses_a_fresh_nonce() {
    let (id, note, key) = fixture();
    let a = Envelope::seal(&id, note, &key, 0, b"milk").unwrap();
    let b = Envelope::seal(&id, note, &key, 1, b"milk").unwrap();
    assert_ne!(a.nonce, b.nonce);
    assert_ne!(
        a.ciphertext, b.ciphertext,
        "identical plaintext must not give identical ciphertext"
    );
}

#[test]
fn rejects_malformed_wire_bytes() {
    let (id, note, key) = fixture();
    let good = Envelope::seal(&id, note, &key, 0, b"milk")
        .unwrap()
        .encode();

    assert!(Envelope::decode(&[]).is_err());
    assert!(
        Envelope::decode(&good[..ENVELOPE_HEADER_LEN]).is_err(),
        "header with no tag"
    );

    let mut wrong_version = good.clone();
    wrong_version[0] = ENVELOPE_VERSION.wrapping_add(1);
    assert!(Envelope::decode(&wrong_version).is_err());

    assert!(
        Envelope::decode(&vec![1u8; MAX_ENVELOPE_BYTES + 1]).is_err(),
        "over the size cap"
    );
}

#[test]
fn share_blob_round_trips_and_fits_in_a_qr_code() {
    let note = NoteId::random();
    let key = NoteKey::random();
    let blob = export_share_blob(&note, &key);

    assert!(
        blob.len() <= 120,
        "share blob is {} chars, budget is 120",
        blob.len()
    );
    assert!(
        blob.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
        "share blob must be URL-fragment safe"
    );

    let (got_note, got_key) = import_share_blob(&blob).unwrap();
    assert_eq!(got_note, note);
    assert_eq!(got_key.0, key.0);
}

#[test]
fn rejects_malformed_share_blobs() {
    let blob = export_share_blob(&NoteId::random(), &NoteKey::random());
    assert!(import_share_blob("not base64!!").is_err());
    assert!(
        import_share_blob(&blob[..blob.len() - 4]).is_err(),
        "truncated"
    );
    assert!(
        import_share_blob(&format!("{blob}AAAA")).is_err(),
        "too long"
    );
}

#[test]
fn share_blob_tolerates_surrounding_whitespace() {
    // Users paste these by hand in Slice 1.
    let blob = export_share_blob(&NoteId::random(), &NoteKey::random());
    assert!(import_share_blob(&format!("  {blob}\n")).is_ok());
}

#[test]
fn an_identity_is_reproducible_from_its_seed() {
    let id = DeviceIdentity::generate();
    let seed = DeviceSeed([7u8; 64]);
    let a = DeviceIdentity::from_seed(&seed);
    let b = DeviceIdentity::from_seed(&seed);
    assert_eq!(a.public_key(), b.public_key());
    assert_eq!(a.x25519_public(), b.x25519_public());
    assert_ne!(a.public_key(), id.public_key());
}

#[test]
fn x25519_agreement_is_symmetric() {
    // Not used until Slice 3's invite links, but the key pair is part of the
    // identity now, so it is worth knowing it works.
    let a = DeviceIdentity::generate();
    let b = DeviceIdentity::generate();
    assert_eq!(a.agree(&b.x25519_public()), b.agree(&a.x25519_public()));
}

#[test]
fn note_key_is_not_printed_by_its_debug_impl() {
    let key = NoteKey([0xAB; 32]);
    assert_eq!(format!("{key:?}"), "NoteKey(redacted)");
}

// ---------------------------------------------------------------------------
// The master key: what this device seals for itself
// ---------------------------------------------------------------------------

#[test]
fn a_local_blob_round_trips() {
    let master = MasterKey::random();
    let sealed = master
        .seal(&[b"cursor", b"note-1"], b"{\"last_seq\":7}")
        .unwrap();
    assert_eq!(
        master.open(&[b"cursor", b"note-1"], &sealed).unwrap(),
        b"{\"last_seq\":7}"
    );
}

#[test]
fn a_local_blob_never_contains_its_plaintext() {
    let master = MasterKey::random();
    let sealed = master.seal(&[b"note-key"], b"asparagus").unwrap();
    assert!(
        !sealed.windows(9).any(|w| w == b"asparagus"),
        "plaintext leaked into a local blob"
    );
}

#[test]
fn a_local_blob_does_not_open_in_another_context() {
    // The row cannot be lifted out of the database and put back somewhere else:
    // a cursor moved to another note, or a note key offered as the device seed,
    // fails rather than being believed.
    let master = MasterKey::random();
    let sealed = master.seal(&[b"cursor", b"note-1"], b"7").unwrap();
    assert!(master.open(&[b"cursor", b"note-2"], &sealed).is_err());
    assert!(master.open(&[b"device-seed", b"note-1"], &sealed).is_err());
    assert!(master.open(&[b"cursor"], &sealed).is_err());
}

#[test]
fn context_parts_cannot_be_reflowed() {
    // Without a length in front of each part, ["note", "key"] and ["notek", "ey"]
    // would authenticate the same bytes and mean the same thing.
    let master = MasterKey::random();
    let sealed = master.seal(&[b"note", b"key"], b"x").unwrap();
    assert!(master.open(&[b"notek", b"ey"], &sealed).is_err());
}

#[test]
fn a_local_blob_does_not_open_under_another_master_key() {
    let sealed = MasterKey::random().seal(&[b"cursor"], b"7").unwrap();
    assert!(MasterKey::random().open(&[b"cursor"], &sealed).is_err());
}

#[test]
fn rejects_a_tampered_local_blob() {
    let master = MasterKey::random();
    let mut sealed = master.seal(&[b"cursor"], b"7").unwrap();
    let last = sealed.len() - 1;
    sealed[last] ^= 0x01;
    assert!(master.open(&[b"cursor"], &sealed).is_err());
    assert!(master
        .open(&[b"cursor"], &sealed[..LOCAL_BLOB_HEADER_LEN])
        .is_err());
    assert!(master.open(&[b"cursor"], b"").is_err());
}

#[test]
fn a_relay_envelope_is_not_a_local_blob() {
    // Same key bytes on purpose: the domain tag in the AAD is what keeps the two
    // apart, not the accident of their being different keys.
    let raw = [9u8; 32];
    let master = MasterKey(raw);
    let note_key = NoteKey(raw);
    let envelope = Envelope::seal(
        &DeviceIdentity::generate(),
        NoteId::random(),
        &note_key,
        0,
        b"milk",
    )
    .unwrap()
    .encode();
    assert!(master.open(&[b"op"], &envelope).is_err());

    let sealed = master.seal(&[b"op"], b"milk").unwrap();
    assert!(Envelope::decode(&sealed).is_err());
}

#[test]
fn master_key_is_not_printed_by_its_debug_impl() {
    assert_eq!(
        format!("{:?}", MasterKey([0xAB; 32])),
        "MasterKey(redacted)"
    );
}
