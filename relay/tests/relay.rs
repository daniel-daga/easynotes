//! M3 acceptance for the relay on its own: sequencing, fan-out, replay, and the
//! handful of things it is allowed to refuse.

use easynotes_core::crypto::{DeviceIdentity, Envelope, NoteKey};
use easynotes_core::ids::NoteId;
use easynotes_core::sync::{Incoming, NoteSync};
use easynotes_relay::{MemoryStore, Relay, RelayStore};
use std::sync::Arc;

async fn relay() -> (Relay, MemoryStore) {
    let store = MemoryStore::new();
    let relay = Relay::serve(Arc::new(store.clone()), "127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    (relay, store)
}

fn envelope(
    identity: &DeviceIdentity,
    note: NoteId,
    key: &NoteKey,
    counter: u64,
    body: &[u8],
) -> Vec<u8> {
    Envelope::seal(identity, note, key, counter, body)
        .unwrap()
        .encode()
}

/// Read until `Caught`, collecting envelopes.
async fn drain(sync: &mut NoteSync) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        match sync.next().await.unwrap() {
            Some(Incoming::Envelope { envelope, .. }) => out.push(envelope),
            Some(Incoming::Caught { .. }) => return out,
            Some(Incoming::Rejected(m)) => panic!("unexpected rejection: {m}"),
            None => return out,
        }
    }
}

/// Read exactly `n` envelopes, failing rather than hanging forever.
async fn take(sync: &mut NoteSync, n: usize) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    while out.len() < n {
        let next = tokio::time::timeout(std::time::Duration::from_secs(5), sync.next())
            .await
            .expect("timed out waiting for the relay")
            .unwrap();
        match next {
            Some(Incoming::Envelope { envelope, .. }) => out.push(envelope),
            Some(Incoming::Caught { .. }) => {}
            Some(Incoming::Rejected(m)) => panic!("unexpected rejection: {m}"),
            None => panic!("the relay closed after {} of {n} envelopes", out.len()),
        }
    }
    out
}

#[tokio::test]
async fn assigns_dense_increasing_sequence_numbers() {
    let (_relay, store) = relay().await;
    let note = NoteId::random().encode();
    for expected in 1..=5 {
        assert_eq!(store.append(&note, b"ciphertext").await.unwrap(), expected);
    }
    assert_eq!(store.head(&note).await.unwrap(), 5);

    // Sequence numbers are per note, not global.
    let other = NoteId::random().encode();
    assert_eq!(store.append(&other, b"ciphertext").await.unwrap(), 1);
}

#[tokio::test]
async fn since_replays_only_what_is_newer() {
    let (_relay, store) = relay().await;
    let note = NoteId::random().encode();
    for i in 0..5u8 {
        store.append(&note, &[i]).await.unwrap();
    }
    let got = store.since(&note, 2).await.unwrap();
    assert_eq!(got.iter().map(|s| s.seq).collect::<Vec<_>>(), [3, 4, 5]);
    assert!(store.since(&note, 5).await.unwrap().is_empty());
    assert_eq!(store.since(&note, 0).await.unwrap().len(), 5);
}

#[tokio::test]
async fn a_subscriber_receives_another_devices_append() {
    let (relay, _store) = relay().await;
    let a = DeviceIdentity::generate();
    let b = DeviceIdentity::generate();
    let note = NoteId::random();
    let key = NoteKey::random();

    let token_a = easynotes_core::sync::authenticate(&relay.http_base(), &a)
        .await
        .unwrap();
    let token_b = easynotes_core::sync::authenticate(&relay.http_base(), &b)
        .await
        .unwrap();

    let mut sync_a = NoteSync::connect(&relay.ws_base(), &token_a, note, 0)
        .await
        .unwrap();
    let mut sync_b = NoteSync::connect(&relay.ws_base(), &token_b, note, 0)
        .await
        .unwrap();
    assert!(drain(&mut sync_a).await.is_empty());
    assert!(drain(&mut sync_b).await.is_empty());

    let sent = envelope(&a, note, &key, 0, b"hello");
    sync_a.append(&sent).await.unwrap();

    assert_eq!(
        take(&mut sync_b, 1).await[0],
        sent,
        "B did not receive A's append"
    );
    // The sender gets its own append back too, so `seq` tracking is uniform.
    assert_eq!(take(&mut sync_a, 1).await[0], sent);
}

#[tokio::test]
async fn a_reconnecting_client_replays_from_its_cursor() {
    let (relay, _store) = relay().await;
    let a = DeviceIdentity::generate();
    let note = NoteId::random();
    let key = NoteKey::random();
    let token = easynotes_core::sync::authenticate(&relay.http_base(), &a)
        .await
        .unwrap();

    let mut sync = NoteSync::connect(&relay.ws_base(), &token, note, 0)
        .await
        .unwrap();
    drain(&mut sync).await;
    for counter in 0..3 {
        sync.append(&envelope(&a, note, &key, counter, b"x"))
            .await
            .unwrap();
    }
    take(&mut sync, 3).await;
    let cursor = sync.last_seq;
    sync.close().await.unwrap();

    // Something happens while we are away.
    let mut other = NoteSync::connect(&relay.ws_base(), &token, note, cursor)
        .await
        .unwrap();
    assert!(drain(&mut other).await.is_empty(), "nothing new yet");
    other
        .append(&envelope(&a, note, &key, 99, b"while away"))
        .await
        .unwrap();
    take(&mut other, 1).await;
    other.close().await.unwrap();

    // Reconnecting from the cursor yields exactly the one missed update.
    let mut back = NoteSync::connect(&relay.ws_base(), &token, note, cursor)
        .await
        .unwrap();
    let missed = drain(&mut back).await;
    assert_eq!(missed.len(), 1, "expected exactly the update we missed");
    assert_eq!(
        Envelope::decode(&missed[0]).unwrap().open(&key).unwrap(),
        b"while away"
    );
}

#[tokio::test]
async fn refuses_a_connection_without_a_valid_token() {
    let (relay, _store) = relay().await;
    let note = NoteId::random();
    assert!(
        NoteSync::connect(&relay.ws_base(), "not-a-token", note, 0)
            .await
            .is_err(),
        "a made-up token was accepted"
    );
}

#[tokio::test]
async fn refuses_a_challenge_signed_by_the_wrong_key() {
    let (relay, _store) = relay().await;
    let real = DeviceIdentity::generate();
    let attacker = DeviceIdentity::generate();

    let http = reqwest::Client::new();
    let challenge: serde_json::Value = http
        .post(format!("{}/v1/challenge", relay.http_base()))
        .json(&serde_json::json!({ "device": real.public_key().encode() }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let nonce_b64 = challenge["nonce"].as_str().unwrap();
    let nonce = b64_decode(nonce_b64);

    let response = http
        .post(format!("{}/v1/token", relay.http_base()))
        .json(&serde_json::json!({
            "device": real.public_key().encode(),
            "nonce": nonce_b64,
            // Signed by someone else entirely.
            "signature": b64_encode(&attacker.sign(&nonce)),
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 401);
}

#[tokio::test]
async fn a_challenge_cannot_be_redeemed_twice() {
    let (relay, _store) = relay().await;
    let device = DeviceIdentity::generate();
    let http = reqwest::Client::new();

    let challenge: serde_json::Value = http
        .post(format!("{}/v1/challenge", relay.http_base()))
        .json(&serde_json::json!({ "device": device.public_key().encode() }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let nonce_b64 = challenge["nonce"].as_str().unwrap().to_string();
    let body = serde_json::json!({
        "device": device.public_key().encode(),
        "nonce": nonce_b64,
        "signature": b64_encode(&device.sign(&b64_decode(&nonce_b64))),
    });

    let first = http
        .post(format!("{}/v1/token", relay.http_base()))
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(first.status(), 200);
    let second = http
        .post(format!("{}/v1/token", relay.http_base()))
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(second.status(), 401, "a replayed challenge was accepted");
}

#[tokio::test]
async fn refuses_a_malformed_note_id() {
    let (relay, _store) = relay().await;
    let device = DeviceIdentity::generate();
    let token = easynotes_core::sync::authenticate(&relay.http_base(), &device)
        .await
        .unwrap();
    let url = format!("{}/v1/notes/not-a-note-id?token={token}", relay.ws_base());
    assert!(tokio_tungstenite::connect_async(&url).await.is_err());
}

#[tokio::test]
async fn refuses_an_envelope_addressed_to_another_note() {
    // The relay cannot read an envelope, but it can check that the one it is being
    // asked to file under this note says so itself.
    let (relay, store) = relay().await;
    let device = DeviceIdentity::generate();
    let token = easynotes_core::sync::authenticate(&relay.http_base(), &device)
        .await
        .unwrap();
    let note = NoteId::random();
    let elsewhere = NoteId::random();

    let mut sync = NoteSync::connect(&relay.ws_base(), &token, note, 0)
        .await
        .unwrap();
    drain(&mut sync).await;
    sync.append(&envelope(&device, elsewhere, &NoteKey::random(), 0, b"x"))
        .await
        .unwrap();

    match sync.next().await.unwrap() {
        Some(Incoming::Rejected(m)) => assert!(m.contains("different note"), "got: {m}"),
        other => panic!("expected a rejection, got {:?}", other.is_some()),
    }
    assert!(
        store.all_envelopes().is_empty(),
        "the misrouted envelope was stored anyway"
    );
}

#[tokio::test]
async fn refuses_an_oversized_envelope() {
    let (relay, store) = relay().await;
    let device = DeviceIdentity::generate();
    let token = easynotes_core::sync::authenticate(&relay.http_base(), &device)
        .await
        .unwrap();
    let note = NoteId::random();

    let mut sync = NoteSync::connect(&relay.ws_base(), &token, note, 0)
        .await
        .unwrap();
    drain(&mut sync).await;
    sync.append(&vec![
        1u8;
        easynotes_relay::protocol::MAX_ENVELOPE_BYTES + 1
    ])
    .await
    .unwrap();

    match sync.next().await.unwrap() {
        Some(Incoming::Rejected(_)) => {}
        other => panic!("expected a rejection, got {:?}", other.is_some()),
    }
    assert!(store.all_envelopes().is_empty());
}

#[tokio::test]
async fn refuses_a_blob_that_is_not_an_envelope() {
    let (relay, store) = relay().await;
    let device = DeviceIdentity::generate();
    let token = easynotes_core::sync::authenticate(&relay.http_base(), &device)
        .await
        .unwrap();
    let note = NoteId::random();

    let mut sync = NoteSync::connect(&relay.ws_base(), &token, note, 0)
        .await
        .unwrap();
    drain(&mut sync).await;
    sync.append(b"this is not an envelope").await.unwrap();

    match sync.next().await.unwrap() {
        Some(Incoming::Rejected(m)) => assert!(m.contains("malformed"), "got: {m}"),
        other => panic!("expected a rejection, got {:?}", other.is_some()),
    }
    assert!(store.all_envelopes().is_empty());
}

#[test]
fn caps_match_the_core() {
    // The relay's cap and the core's must agree, or one of them silently becomes
    // the real limit.
    assert_eq!(
        easynotes_relay::protocol::MAX_ENVELOPE_BYTES,
        easynotes_core::crypto::MAX_ENVELOPE_BYTES
    );
}

fn b64_encode(b: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
}

fn b64_decode(s: &str) -> Vec<u8> {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(s)
        .unwrap()
}
