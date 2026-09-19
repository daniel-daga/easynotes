//! M3 acceptance for the relay on its own: sequencing, fan-out, replay, and the
//! handful of things it is allowed to refuse.

use easynotes_core::crypto::{DeviceIdentity, Envelope, NoteKey};
use easynotes_core::ids::NoteId;
use easynotes_core::sync::{Incoming, NoteSync};
use easynotes_relay::limit::RateLimit;
use easynotes_relay::{MemoryStore, Origins, Relay, RelayConfig, RelayStore};
use std::sync::Arc;

async fn relay() -> (Relay, MemoryStore) {
    let store = MemoryStore::new();
    let relay = Relay::serve(Arc::new(store.clone()), "127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    (relay, store)
}

/// A relay configured the way a public one would be, so that the two things a
/// public one adds can be asserted on.
async fn relay_with(config: RelayConfig) -> Relay {
    Relay::serve_with(
        Arc::new(MemoryStore::new()),
        "127.0.0.1:0".parse().unwrap(),
        config,
    )
    .await
    .unwrap()
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

/// The handshake has to be reachable from a page, or the web build cannot get a
/// token at all — and a JSON POST is a preflighted request, so the browser asks
/// before it tries. Both halves are asserted, because passing one and failing
/// the other still leaves the app unable to connect.
#[tokio::test]
async fn a_browser_may_do_the_handshake() {
    let (relay, _store) = relay().await;
    let http = reqwest::Client::new();

    let preflight = http
        .request(
            reqwest::Method::OPTIONS,
            format!("{}/v1/challenge", relay.http_base()),
        )
        .header("Origin", "https://notes.example")
        .header("Access-Control-Request-Method", "POST")
        .header("Access-Control-Request-Headers", "content-type")
        .send()
        .await
        .unwrap();
    assert!(
        preflight.status().is_success(),
        "the preflight was refused: {}",
        preflight.status()
    );
    let allowed = |r: &reqwest::Response| {
        r.headers()
            .get("access-control-allow-origin")
            .map(|v| v.to_str().unwrap().to_string())
    };
    assert_eq!(allowed(&preflight).as_deref(), Some("*"));

    let identity = DeviceIdentity::generate();
    let real = http
        .post(format!("{}/v1/challenge", relay.http_base()))
        .header("Origin", "https://notes.example")
        .json(&serde_json::json!({ "device": identity.public_key().encode() }))
        .send()
        .await
        .unwrap();
    assert!(real.status().is_success());
    assert_eq!(
        allowed(&real).as_deref(),
        Some("*"),
        "the response a page is allowed to read has to say so on the response too, \
         not only on the preflight"
    );
}

/// An allowlist that is set is an allowlist that applies.
///
/// CORS is not what protects this relay — the handshake is — so this is not a
/// security boundary and is not claimed as one. It is the narrower answer given
/// when the deployment knows which page it serves, and the test is here because
/// an allowlist that quietly allows everything is the worst of the three
/// possible behaviours.
#[tokio::test]
async fn an_allowlist_refuses_an_origin_that_is_not_on_it() {
    let relay = relay_with(RelayConfig {
        origins: Origins::parse("https://notes.example").unwrap(),
        ..RelayConfig::default()
    })
    .await;
    let http = reqwest::Client::new();

    let preflight = |origin: &'static str| {
        let http = http.clone();
        let url = format!("{}/v1/challenge", relay.http_base());
        async move {
            http.request(reqwest::Method::OPTIONS, url)
                .header("Origin", origin)
                .header("Access-Control-Request-Method", "POST")
                .header("Access-Control-Request-Headers", "content-type")
                .send()
                .await
                .unwrap()
        }
    };

    let allowed = preflight("https://notes.example").await;
    assert_eq!(
        allowed
            .headers()
            .get("access-control-allow-origin")
            .map(|v| v.to_str().unwrap()),
        Some("https://notes.example"),
    );

    // The refusal is the *absence* of the header, which is how CORS says no: the
    // response still arrives, and the browser is what refuses to hand it over.
    let refused = preflight("https://phishing.example").await;
    assert!(
        refused
            .headers()
            .get("access-control-allow-origin")
            .is_none(),
        "an origin not on the list was told it was allowed",
    );
}

/// The handshake is the unauthenticated door, and it is the one that has to hold
/// against somebody leaning on it.
///
/// Two devices' worth of burst, then a refusal — with `Retry-After` on it, so a
/// well-behaved client knows how long to wait rather than guessing or spinning.
#[tokio::test]
async fn a_flood_of_handshakes_is_refused() {
    let relay = relay_with(RelayConfig {
        handshake: RateLimit::new(60, 3),
        ..RelayConfig::default()
    })
    .await;
    let http = reqwest::Client::new();
    let identity = DeviceIdentity::generate();
    let ask = || {
        http.post(format!("{}/v1/challenge", relay.http_base()))
            .json(&serde_json::json!({ "device": identity.public_key().encode() }))
            .send()
    };

    for i in 0..3 {
        let r = ask().await.unwrap();
        assert!(r.status().is_success(), "handshake {i} was refused early");
    }

    let refused = ask().await.unwrap();
    assert_eq!(refused.status(), reqwest::StatusCode::TOO_MANY_REQUESTS);
    let retry = refused
        .headers()
        .get("retry-after")
        .expect("a refusal has to say when to come back")
        .to_str()
        .unwrap()
        .parse::<u64>()
        .unwrap();
    assert!(retry >= 1, "Retry-After: 0 invites the retry just refused");
}

/// Opening sockets is limited too, and on its own budget.
///
/// A client that has spent its socket allowance can still get a token, and one
/// that has spent its handshake allowance can still use the token it already
/// has. Two buckets rather than one, because the alternative is a reconnecting
/// device locking itself out of the door it just came through.
#[tokio::test]
async fn the_two_limits_are_kept_apart() {
    let relay = relay_with(RelayConfig {
        handshake: RateLimit::new(60, 1),
        sockets: RateLimit::new(60, 1),
        ..RelayConfig::default()
    })
    .await;
    let http = reqwest::Client::new();
    let identity = DeviceIdentity::generate();

    // Spend the handshake bucket.
    let first = http
        .post(format!("{}/v1/challenge", relay.http_base()))
        .json(&serde_json::json!({ "device": identity.public_key().encode() }))
        .send()
        .await
        .unwrap();
    assert!(first.status().is_success());
    let second = http
        .post(format!("{}/v1/challenge", relay.http_base()))
        .json(&serde_json::json!({ "device": identity.public_key().encode() }))
        .send()
        .await
        .unwrap();
    assert_eq!(second.status(), reqwest::StatusCode::TOO_MANY_REQUESTS);

    // The socket bucket is untouched, so this reaches the handler rather than
    // being refused for asking too often. What the handler then says is a 400:
    // it is a plain GET with none of the headers an upgrade needs, which the
    // `WebSocketUpgrade` extractor refuses before the token is ever looked at.
    // The assertion is only that it is not a 429, because that is the whole
    // question — anything else means the request got through the limiter.
    let note = NoteId::random();
    let socket = http
        .get(format!(
            "{}/v1/notes/{}?token=nonsense",
            relay.http_base(),
            note.encode()
        ))
        .send()
        .await
        .unwrap();
    assert_ne!(
        socket.status(),
        reqwest::StatusCode::TOO_MANY_REQUESTS,
        "the socket path was spending the handshake's allowance",
    );
}

/// Health is outside the limiter, because a relay that fails its health check
/// for being busy gets taken out of rotation for being busy.
#[tokio::test]
async fn the_health_check_is_never_rate_limited() {
    let relay = relay_with(RelayConfig {
        handshake: RateLimit::new(60, 1),
        sockets: RateLimit::new(60, 1),
        ..RelayConfig::default()
    })
    .await;
    let http = reqwest::Client::new();
    for i in 0..20 {
        let r = http
            .get(format!("{}/v1/health", relay.http_base()))
            .send()
            .await
            .unwrap();
        assert!(r.status().is_success(), "health check {i} was refused");
    }
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
