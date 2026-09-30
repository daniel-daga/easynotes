//! M3 acceptance for the relay on its own: sequencing, fan-out, replay, and the
//! handful of things it is allowed to refuse.

use easynotes_core::crypto::{DeviceIdentity, Envelope, NoteKey};
use easynotes_core::ids::NoteId;
use easynotes_core::sync::Incoming;
use easynotes_relay::limit::{RateLimit, TrustedProxies};
use easynotes_relay::{MemoryStore, Origins, Relay, RelayConfig, RelayStore};
use std::sync::Arc;
use support::NoteSocket;

mod support;

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
async fn drain(sync: &mut NoteSocket) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        match sync.next().await.unwrap() {
            Some(Incoming::Envelope { envelope, .. }) => out.push(envelope),
            Some(Incoming::Caught { .. }) => return out,
            Some(Incoming::Reset) => panic!("unexpected relay reset"),
            Some(Incoming::Rejected(m)) => panic!("unexpected rejection: {m}"),
            Some(Incoming::Gone) => panic!("unexpectedly purged"),
            // About compaction, which none of these tests is.
            Some(Incoming::Crowded | Incoming::CompactRefused(_)) => {}
            None => return out,
        }
    }
}

/// Read exactly `n` envelopes, failing rather than hanging forever.
async fn take(sync: &mut NoteSocket, n: usize) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    while out.len() < n {
        let next = tokio::time::timeout(std::time::Duration::from_secs(5), sync.next())
            .await
            .expect("timed out waiting for the relay")
            .unwrap();
        match next {
            Some(Incoming::Envelope { envelope, .. }) => out.push(envelope),
            Some(Incoming::Caught { .. }) => {}
            Some(Incoming::Reset) => panic!("unexpected relay reset"),
            Some(Incoming::Rejected(m)) => panic!("unexpected rejection: {m}"),
            Some(Incoming::Gone) => panic!("unexpectedly purged"),
            // About compaction, which none of these tests is.
            Some(Incoming::Crowded | Incoming::CompactRefused(_)) => {}
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

    let mut sync_a = NoteSocket::connect(&relay.ws_base(), &token_a, note, 0)
        .await
        .unwrap();
    let mut sync_b = NoteSocket::connect(&relay.ws_base(), &token_b, note, 0)
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

    let mut sync = NoteSocket::connect(&relay.ws_base(), &token, note, 0)
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
    let mut other = NoteSocket::connect(&relay.ws_base(), &token, note, cursor)
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
    let mut back = NoteSocket::connect(&relay.ws_base(), &token, note, cursor)
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
    let nonce: [u8; easynotes_core::crypto::RELAY_AUTH_NONCE_LEN] =
        b64_decode(nonce_b64).try_into().unwrap();
    let transcript = easynotes_core::crypto::relay_auth_transcript(&nonce);

    let response = http
        .post(format!("{}/v1/token", relay.http_base()))
        .json(&serde_json::json!({
            "device": real.public_key().encode(),
            "nonce": nonce_b64,
            // Signed by someone else entirely.
            "signature": b64_encode(&attacker.sign(&transcript)),
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
    let nonce: [u8; easynotes_core::crypto::RELAY_AUTH_NONCE_LEN] =
        b64_decode(&nonce_b64).try_into().unwrap();
    let transcript = easynotes_core::crypto::relay_auth_transcript(&nonce);
    let body = serde_json::json!({
        "device": device.public_key().encode(),
        "nonce": nonce_b64,
        "signature": b64_encode(&device.sign(&transcript)),
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
async fn authentication_requires_the_shared_transcript_and_rejects_raw_nonce_signatures() {
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
    let nonce: [u8; easynotes_core::crypto::RELAY_AUTH_NONCE_LEN] =
        b64_decode(&nonce_b64).try_into().unwrap();
    let transcript = easynotes_core::crypto::relay_auth_transcript(&nonce);
    let body = serde_json::json!({
        "device": device.public_key().encode(),
        "nonce": nonce_b64,
        "signature": b64_encode(&device.sign(&transcript)),
    });

    let first = http
        .post(format!("{}/v1/token", relay.http_base()))
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(first.status(), 200);
    let replay = http
        .post(format!("{}/v1/token", relay.http_base()))
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(replay.status(), 401);

    let raw_challenge: serde_json::Value = http
        .post(format!("{}/v1/challenge", relay.http_base()))
        .json(&serde_json::json!({ "device": device.public_key().encode() }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let nonce_b64 = raw_challenge["nonce"].as_str().unwrap();
    let raw_signature = b64_encode(&device.sign(&b64_decode(nonce_b64)));
    let raw = http
        .post(format!("{}/v1/token", relay.http_base()))
        .json(&serde_json::json!({
            "device": device.public_key().encode(),
            "nonce": nonce_b64,
            "signature": raw_signature,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(raw.status(), 401);
}

#[tokio::test]
async fn refuses_a_malformed_note_id() {
    // A note ID that cannot exist is refused before anything is subscribed, so
    // a typo cannot create a log for it. The socket stays open.
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;
    let (relay, _store) = relay().await;
    let device = DeviceIdentity::generate();
    let token = easynotes_core::sync::authenticate(&relay.http_base(), &device)
        .await
        .unwrap();
    let url = format!("{}/v1/sync?token={token}", relay.ws_base());
    let (mut socket, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    socket
        .send(Message::text(
            r#"{"type":"subscribe","note":"not-a-note-id","since":0}"#,
        ))
        .await
        .unwrap();
    let reply = loop {
        match socket.next().await.unwrap().unwrap() {
            Message::Text(text) => break text.to_string(),
            _ => continue,
        }
    };
    assert!(reply.contains("malformed note id"), "got: {reply}");
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

    let mut sync = NoteSocket::connect(&relay.ws_base(), &token, note, 0)
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

    let mut sync = NoteSocket::connect(&relay.ws_base(), &token, note, 0)
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

    let mut sync = NoteSocket::connect(&relay.ws_base(), &token, note, 0)
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
    let socket = http
        .get(format!("{}/v1/sync?token=nonsense", relay.http_base()))
        .send()
        .await
        .unwrap();
    assert_ne!(
        socket.status(),
        reqwest::StatusCode::TOO_MANY_REQUESTS,
        "the socket path was spending the handshake's allowance",
    );
}

/// Behind a trusted reverse proxy, each client the proxy names has a bucket of
/// its own.
///
/// This is the Synology deployment: every request arrives from the proxy's
/// address, and without `RELAY_TRUSTED_PROXIES` they would all share one
/// allowance. Here the test is the proxy, on 127.0.0.1.
#[tokio::test]
async fn clients_behind_a_trusted_proxy_are_limited_apart() {
    let relay = relay_with(RelayConfig {
        handshake: RateLimit::new(60, 1),
        trusted_proxies: TrustedProxies::parse("127.0.0.1").unwrap(),
        ..RelayConfig::default()
    })
    .await;
    let http = reqwest::Client::new();
    let identity = DeviceIdentity::generate();
    let ask = |client: &'static str| {
        http.post(format!("{}/v1/challenge", relay.http_base()))
            .header("X-Forwarded-For", client)
            .json(&serde_json::json!({ "device": identity.public_key().encode() }))
            .send()
    };

    assert!(ask("203.0.113.1").await.unwrap().status().is_success());
    assert_eq!(
        ask("203.0.113.1").await.unwrap().status(),
        reqwest::StatusCode::TOO_MANY_REQUESTS,
        "one client behind the proxy still has a limit",
    );
    assert!(
        ask("203.0.113.2").await.unwrap().status().is_success(),
        "a second client behind the proxy was refused for the first one's requests",
    );
    // What a client writes itself, to the left of what the proxy appended, is
    // not a way out of its own bucket.
    assert_eq!(
        ask("198.51.100.99, 203.0.113.1").await.unwrap().status(),
        reqwest::StatusCode::TOO_MANY_REQUESTS,
    );
}

/// Without a trust list, `X-Forwarded-For` is only a string a stranger chose,
/// and a fresh one on every request must not buy a fresh allowance.
#[tokio::test]
async fn an_untrusted_forwarded_for_is_ignored() {
    let relay = relay_with(RelayConfig {
        handshake: RateLimit::new(60, 1),
        ..RelayConfig::default()
    })
    .await;
    let http = reqwest::Client::new();
    let identity = DeviceIdentity::generate();
    let ask = |client: &'static str| {
        http.post(format!("{}/v1/challenge", relay.http_base()))
            .header("X-Forwarded-For", client)
            .json(&serde_json::json!({ "device": identity.public_key().encode() }))
            .send()
    };

    assert!(ask("203.0.113.1").await.unwrap().status().is_success());
    assert_eq!(
        ask("203.0.113.2").await.unwrap().status(),
        reqwest::StatusCode::TOO_MANY_REQUESTS,
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

/// Readiness answers with the relay's version while the store is there.
#[tokio::test]
async fn ready_reports_the_version_while_the_store_answers() {
    let (relay, _) = relay().await;
    let r = reqwest::get(format!("{}/v1/ready", relay.http_base()))
        .await
        .unwrap();
    assert_eq!(r.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = r.json().await.unwrap();
    assert_eq!(body["version"], env!("CARGO_PKG_VERSION"));
}

/// And refuses, while health still says the process is up, once it is not:
/// the one state monitoring has to tell apart from a healthy relay.
#[tokio::test]
async fn ready_fails_when_the_store_does_not_answer() {
    let relay = Relay::serve(Arc::new(Unreachable), "127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let ready = reqwest::get(format!("{}/v1/ready", relay.http_base()))
        .await
        .unwrap();
    assert_eq!(ready.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);
    let text = ready.text().await.unwrap();
    assert!(
        !text.contains("database is gone"),
        "the store's error reached a public response: {text}"
    );
    let health = reqwest::get(format!("{}/v1/health", relay.http_base()))
        .await
        .unwrap();
    assert!(health.status().is_success());
}

/// A store whose database has gone away: every call fails.
struct Unreachable;

/// A stop closes every sync socket cleanly, so that each device hears it and
/// reconnects rather than waiting to time out, and then stops listening. The
/// sockets are what axum would not wait for on its own: once upgraded, they are
/// no longer its connections.
#[tokio::test]
async fn a_stop_closes_every_socket_and_then_the_listener() {
    let (relay, _store) = relay().await;
    let (addr, http, ws) = (relay.addr, relay.http_base(), relay.ws_base());
    let mut sockets = Vec::new();
    for _ in 0..3 {
        let token = easynotes_core::sync::authenticate(&http, &DeviceIdentity::generate())
            .await
            .unwrap();
        let mut socket = NoteSocket::connect(&ws, &token, NoteId::random(), 0)
            .await
            .unwrap();
        assert!(drain(&mut socket).await.is_empty());
        sockets.push(socket);
    }

    let stop = tokio::spawn(relay.shutdown(std::time::Duration::from_secs(5)));
    for mut socket in sockets {
        let closed = tokio::time::timeout(std::time::Duration::from_secs(5), socket.next())
            .await
            .expect("the socket was told the relay is stopping");
        assert!(
            matches!(closed, Ok(None)),
            "expected a clean close, not a message or an error"
        );
    }
    assert!(
        stop.await.unwrap(),
        "the relay did not drain inside its window"
    );
    assert!(
        tokio::net::TcpStream::connect(addr).await.is_err(),
        "the relay is still listening after it stopped"
    );
}

/// A stop with nothing connected is over at once, not at the end of the drain
/// window: a redeploy should not wait out a timer for nobody.
#[tokio::test]
async fn an_idle_relay_stops_at_once() {
    let (relay, _store) = relay().await;
    let started = std::time::Instant::now();
    assert!(relay.shutdown(std::time::Duration::from_secs(30)).await);
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
}

fn gone<T>() -> easynotes_relay::error::Result<T> {
    Err(easynotes_relay::error::RelayError::Storage(
        "database is gone".into(),
    ))
}

#[async_trait::async_trait]
impl RelayStore for Unreachable {
    async fn append_within(
        &self,
        _: &str,
        _: &[u8],
        _: &easynotes_relay::Limits,
    ) -> easynotes_relay::error::Result<easynotes_relay::Appended> {
        gone()
    }
    async fn compact(
        &self,
        _: &str,
        _: &easynotes_relay::quota::Proposal<'_>,
    ) -> easynotes_relay::error::Result<Result<i64, easynotes_relay::quota::Refusal>> {
        gone()
    }
    async fn topic(
        &self,
        _: &str,
    ) -> easynotes_relay::error::Result<easynotes_relay::quota::Topic> {
        gone()
    }
    async fn since(
        &self,
        _: &str,
        _: i64,
    ) -> easynotes_relay::error::Result<Vec<easynotes_relay::store::Stored>> {
        gone()
    }
    async fn page(
        &self,
        _: &str,
        _: i64,
        _: usize,
    ) -> easynotes_relay::error::Result<Vec<easynotes_relay::store::Stored>> {
        gone()
    }
    async fn head(&self, _: &str) -> easynotes_relay::error::Result<i64> {
        gone()
    }
    async fn save_auth_challenge(
        &self,
        _: &[u8],
        _: &[u8; 32],
        _: i64,
    ) -> easynotes_relay::error::Result<()> {
        gone()
    }
    async fn consume_auth_challenge(
        &self,
        _: &[u8],
        _: i64,
    ) -> easynotes_relay::error::Result<Option<[u8; 32]>> {
        gone()
    }
    async fn create_invite(
        &self,
        _: easynotes_relay::store::NewInvite<'_>,
        _: i64,
    ) -> easynotes_relay::error::Result<bool> {
        gone()
    }
    async fn invite(
        &self,
        _: &str,
    ) -> easynotes_relay::error::Result<Option<easynotes_relay::store::Invite>> {
        gone()
    }
    async fn add_invite_request(
        &self,
        _: &str,
        _: &str,
        _: &[u8],
        _: i64,
    ) -> easynotes_relay::error::Result<easynotes_relay::store::AddRequest> {
        gone()
    }
    async fn answer_invite_request(
        &self,
        _: &str,
        _: &str,
        _: Option<&[u8]>,
        _: i64,
    ) -> easynotes_relay::error::Result<easynotes_relay::store::Answered> {
        gone()
    }
    async fn withdraw_invite(&self, _: &str) -> easynotes_relay::error::Result<()> {
        gone()
    }
    async fn purge(
        &self,
        _: &str,
        _: [u8; 32],
        _: [u8; easynotes_core::crypto::SIG_LEN],
    ) -> easynotes_relay::error::Result<Result<(), easynotes_relay::quota::Refusal>> {
        gone()
    }
    async fn reserve_media(
        &self,
        _: &str,
        _: &str,
        _: easynotes_relay::media::Geometry,
        _: &easynotes_relay::MediaLimits,
        _: i64,
    ) -> easynotes_relay::error::Result<easynotes_relay::media::Reserved> {
        gone()
    }
    async fn media(
        &self,
        _: &str,
        _: &str,
    ) -> easynotes_relay::error::Result<Option<easynotes_relay::media::MediaRow>> {
        gone()
    }
    async fn touch_media(&self, _: &str, _: &str, _: i64) -> easynotes_relay::error::Result<()> {
        gone()
    }
    async fn commit_media(
        &self,
        _: &str,
        _: &str,
        _: [u8; 32],
    ) -> easynotes_relay::error::Result<()> {
        gone()
    }
    async fn expire_media(&self, _: i64) -> easynotes_relay::error::Result<Vec<(String, String)>> {
        gone()
    }
    async fn check(&self) -> easynotes_relay::error::Result<()> {
        gone()
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
