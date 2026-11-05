//! Who may write to a relay ([ADR 0028](../../docs/decisions.md#adr-0028)):
//! a relay gated on an access key lets in only devices proving it, at the
//! handshake and on every invite call, and leaves purge open; and every relay
//! holds each client address to a write budget, over the socket and for
//! attachment chunks alike.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use easynotes_core::attachment::seal::seal_whole;
use easynotes_core::attachment::{ContentKey, Variant};
use easynotes_core::crypto::{DeviceIdentity, DeviceSigner, Envelope, NoteKey};
use easynotes_core::error::CoreError;
use easynotes_core::ids::{AttachmentId, NoteId};
use easynotes_core::invite::InviteSecret;
use easynotes_core::relay_access::{RelayAccess, RelayToken};
use easynotes_core::sync::{authenticate, invite, DeviceSync, Incoming, Routed};
use easynotes_relay::limit::RateLimit;
use easynotes_relay::{Access, MemoryStore, Relay, RelayConfig, RelayStore};
use futures_util::{SinkExt, StreamExt};
use reqwest::StatusCode;
use std::sync::Arc;
use support::NoteSocket;
use tokio_tungstenite::tungstenite::Message;

mod support;

async fn gated(keys: &[&RelayAccess]) -> Relay {
    gated_on(Arc::new(MemoryStore::new()), keys, [9; 32]).await
}

async fn gated_on(store: Arc<dyn RelayStore>, keys: &[&RelayAccess], signing: [u8; 32]) -> Relay {
    Relay::serve_with(
        store,
        "127.0.0.1:0".parse().unwrap(),
        RelayConfig {
            access: Access::Keys(keys.iter().map(|k| (*k).clone()).collect()),
            token_signing_key: signing,
            ..RelayConfig::default()
        },
    )
    .await
    .unwrap()
}

fn refused(result: Result<impl Sized, CoreError>) {
    match result {
        Err(CoreError::AccessRequired) => {}
        Err(other) => panic!("expected the relay to ask for its key, got {other}"),
        Ok(_) => panic!("expected the relay to ask for its key, got in"),
    }
}

#[tokio::test]
async fn a_gated_relay_issues_tokens_only_to_holders_of_its_key() {
    let key = RelayAccess::random();
    let relay = gated(&[&key]).await;
    let http = relay.http_base();
    let device = DeviceIdentity::generate();

    refused(authenticate(&http, None, &device).await);
    refused(
        authenticate(
            &http,
            Some(&RelayToken::from(RelayAccess::random())),
            &device,
        )
        .await,
    );
    let token = authenticate(&http, Some(&RelayToken::from(key.clone())), &device)
        .await
        .unwrap();

    // And the token opens the socket, as on any relay.
    let mut socket = NoteSocket::connect(&relay.ws_base(), &token, NoteId::random(), 0)
        .await
        .unwrap();
    assert!(matches!(
        socket.next().await.unwrap(),
        Some(Incoming::Caught { .. })
    ));
}

#[tokio::test]
async fn an_open_relay_ignores_a_proof() {
    let relay = Relay::serve(Arc::new(MemoryStore::new()), "127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let device = DeviceIdentity::generate();
    authenticate(
        &relay.http_base(),
        Some(&RelayToken::from(RelayAccess::random())),
        &device,
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn either_key_works_while_a_rotation_overlaps() {
    let (old, new) = (RelayAccess::random(), RelayAccess::random());
    let relay = gated(&[&old, &new]).await;
    for key in [old, new] {
        authenticate(
            &relay.http_base(),
            Some(&RelayToken::from(key)),
            &DeviceIdentity::generate(),
        )
        .await
        .unwrap();
    }
}

/// A token request by hand: `device` signs `nonce` properly, and presents
/// whatever `proof` it is given.
async fn token_with(http: &str, device: &DeviceIdentity, proof: &str) -> StatusCode {
    let client = reqwest::Client::new();
    let key = device.public_key().encode();
    let challenge: serde_json::Value = client
        .post(format!("{http}/v1/challenge?p=2"))
        .json(&serde_json::json!({ "device": key }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let nonce = challenge["nonce"].as_str().unwrap();
    let raw: [u8; 32] = URL_SAFE_NO_PAD.decode(nonce).unwrap().try_into().unwrap();
    let signature = URL_SAFE_NO_PAD.encode(device.sign_challenge(&raw).unwrap());
    client
        .post(format!("{http}/v1/token?p=2"))
        .json(&serde_json::json!({
            "device": key,
            "nonce": nonce,
            "signature": signature,
            "access": proof,
        }))
        .send()
        .await
        .unwrap()
        .status()
}

#[tokio::test]
async fn a_proof_is_good_for_one_handshake_of_one_device() {
    let key = RelayAccess::random();
    let relay = gated(&[&key]).await;
    let http = relay.http_base();
    let (a, b) = (DeviceIdentity::generate(), DeviceIdentity::generate());

    // A proof seen on the wire, for some other nonce: replayed, it is refused.
    let seen = key.token_proof(&[1; 32], &a.public_key().0);
    assert_eq!(token_with(&http, &a, &seen).await, StatusCode::FORBIDDEN);
    // Nor does one device's proof let another in.
    let theirs = key.token_proof(&[1; 32], &b.public_key().0);
    assert_eq!(token_with(&http, &a, &theirs).await, StatusCode::FORBIDDEN);
    assert_eq!(
        token_with(&http, &a, "not a proof").await,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn removing_a_key_stops_new_tokens_and_lets_old_ones_expire() {
    let (old, new) = (RelayAccess::random(), RelayAccess::random());
    let store: Arc<dyn RelayStore> = Arc::new(MemoryStore::new());
    let signing = [3; 32];
    let before = gated_on(store.clone(), &[&old], signing).await;
    let device = DeviceIdentity::generate();
    let token = authenticate(
        &before.http_base(),
        Some(&RelayToken::from(old.clone())),
        &device,
    )
    .await
    .unwrap();
    drop(before);

    // The operator hands out a new key and removes the old one.
    let after = gated_on(store, &[&new], signing).await;
    refused(authenticate(&after.http_base(), Some(&RelayToken::from(old)), &device).await);
    // A token already issued is not revoked: it runs out within fifteen
    // minutes, or at once if the signing key is rotated too.
    NoteSocket::connect(&after.ws_base(), &token, NoteId::random(), 0)
        .await
        .unwrap();
}

#[tokio::test]
async fn every_invite_call_needs_the_key_on_a_gated_relay() {
    let key = RelayAccess::random();
    let token = RelayToken::from(key.clone());
    let relay = gated(&[&key]).await;
    let http = relay.http_base();

    let secret = InviteSecret::random();
    let owner = easynotes_core::invite::OwnerSecret::random();
    refused(invite::open(&http, None, &secret, &owner, 3600).await);
    refused(
        invite::open(
            &http,
            Some(&RelayToken::from(RelayAccess::random())),
            &secret,
            &owner,
            3600,
        )
        .await,
    );
    invite::open(&http, Some(&token), &secret, &owner, 3600)
        .await
        .unwrap();

    // A proof is bound to its invite: one for another invite does not do.
    let other = InviteSecret::random();
    let body = serde_json::json!({
        "owner": owner.encode(),
        "access": key.invite_proof(&other.id()),
    });
    let status = reqwest::Client::new()
        .post(format!(
            "{http}/v1/invites/{}/list?p=2",
            secret.id().encode()
        ))
        .json(&body)
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(status, StatusCode::FORBIDDEN);

    refused(invite::list(&http, None, &secret, &owner).await);
    let listing = invite::list(&http, Some(&token), &secret, &owner)
        .await
        .unwrap();
    assert!(listing.open);
    refused(invite::withdraw(&http, None, &secret, &owner).await);
    invite::withdraw(&http, Some(&token), &secret, &owner)
        .await
        .unwrap();
}

#[tokio::test]
async fn purge_needs_no_key() {
    // Someone whose access was taken away can still delete what they put here:
    // purge's authority is the note key, and it can only destroy.
    let relay = gated(&[&RelayAccess::random()]).await;
    let status = reqwest::Client::new()
        .post(format!("{}/v1/purge?p=2", relay.http_base()))
        .json(&serde_json::json!({ "topics": [] }))
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(status, StatusCode::OK);
}

/// A relay that takes `burst` bytes from an address and then next to nothing.
async fn budgeted(burst: u32) -> Relay {
    Relay::serve_with(
        Arc::new(MemoryStore::new()),
        "127.0.0.1:0".parse().unwrap(),
        RelayConfig {
            writes: RateLimit::new(1, burst),
            ..RelayConfig::default()
        },
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn a_flood_of_appends_is_told_to_back_off_and_disconnected() {
    let relay = budgeted(8 << 10).await;
    let device = DeviceIdentity::generate();
    let token = authenticate(&relay.http_base(), None, &device)
        .await
        .unwrap();
    let (note, key) = (NoteId::random(), NoteKey::random());
    let mut socket = NoteSocket::connect(&relay.ws_base(), &token, note, 0)
        .await
        .unwrap();

    let body = vec![b'x'; 2 << 10];
    for counter in 0..20 {
        let envelope = Envelope::seal(&device, note, &key, counter, &body)
            .unwrap()
            .encode();
        if socket.append(&envelope).await.is_err() {
            break; // closed under us, which is the point
        }
    }
    loop {
        match socket.next().await.unwrap() {
            Some(Incoming::RateLimited { .. }) => break,
            Some(Incoming::Rejected(m)) => panic!("refused otherwise: {m}"),
            Some(_) => {}
            None => panic!("closed without saying why"),
        }
    }
    // And then the relay closes the socket, so the client backs off.
    let closed = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            match socket.next().await {
                Ok(None) | Err(_) => return,
                Ok(Some(_)) => {}
            }
        }
    })
    .await;
    assert!(closed.is_ok(), "the relay kept the socket open");

    // What went before the budget ran out was stored; what came after was not.
    // Reading is not writing, so the same device reads it all back.
    let mut reader = NoteSocket::connect(&relay.ws_base(), &token, note, 0)
        .await
        .unwrap();
    let mut stored = 0;
    loop {
        match reader.next().await.unwrap() {
            Some(Incoming::Envelope { .. }) => stored += 1,
            Some(Incoming::Caught { .. }) => break,
            Some(_) => {}
            None => panic!("the relay closed a socket that only reads"),
        }
    }
    assert!(
        (1..20).contains(&stored),
        "some appends are taken before the budget runs out, not all: {stored}"
    );
}

#[tokio::test]
async fn a_refresh_is_charged_for_every_note_it_reads() {
    let relay = budgeted(8 << 10).await;
    let token = authenticate(&relay.http_base(), None, &DeviceIdentity::generate())
        .await
        .unwrap();
    let mut sync = DeviceSync::connect(&relay.ws_base(), &token).await.unwrap();
    // Four notes, one store read each: that leaves room for one refresh of
    // four, and not for a second.
    let notes: Vec<NoteId> = (0..4).map(|_| NoteId::random()).collect();
    for note in &notes {
        sync.subscribe(*note, 0).await.unwrap();
    }
    let mut caught = 0;
    while caught < notes.len() {
        match sync.next().await.unwrap() {
            Some(Routed::Note(_, Incoming::Caught { .. })) => caught += 1,
            Some(_) => {}
            None => panic!("the relay closed a socket that only subscribed"),
        }
    }

    sync.refresh("first").await.unwrap();
    let mut answered = 0;
    while answered < notes.len() {
        match sync.next().await.unwrap() {
            Some(Routed::Refresh { .. }) => answered += 1,
            Some(_) => {}
            None => panic!("the relay closed inside a refresh the budget covers"),
        }
    }

    // The second refresh is over the budget. Charged before it reads a head,
    // so it is answered before any of the four notes is, and the socket closes.
    sync.refresh("second").await.unwrap();
    match sync.next().await.unwrap() {
        Some(Routed::Note(_, Incoming::RateLimited { .. })) => {}
        // A refresh answers per note: any answer means the heads were read.
        Some(Routed::Refresh { .. }) => panic!("the over-budget refresh did its work first"),
        Some(Routed::Unrouted(message)) => {
            panic!("the refusal was not routed to a note: {message}")
        }
        Some(_) => panic!("something else came back before the refusal"),
        None => panic!("closed without saying why"),
    }
    let closed = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            match sync.next().await {
                Ok(None) | Err(_) => return,
                Ok(Some(_)) => {}
            }
        }
    })
    .await;
    assert!(closed.is_ok(), "the relay kept the socket open");
}

/// A raw socket, for the frames a `DeviceSync` will not send.
type RawSocket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn raw_socket(relay: &Relay) -> RawSocket {
    let token = authenticate(&relay.http_base(), None, &DeviceIdentity::generate())
        .await
        .unwrap();
    let url = format!("{}/v1/sync?p=2&token={token}", relay.ws_base());
    tokio_tungstenite::connect_async(&url).await.unwrap().0
}

/// The next text the relay sends on `socket`.
async fn text(socket: &mut RawSocket) -> String {
    loop {
        if let Message::Text(text) = socket.next().await.unwrap().unwrap() {
            return text.to_string();
        }
    }
}

/// Waits for the relay to close `socket`.
async fn closed(socket: &mut RawSocket) {
    let closed = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while let Some(Ok(_)) = socket.next().await {}
    })
    .await;
    assert!(closed.is_ok(), "the relay kept the socket open");
}

#[tokio::test]
async fn a_flood_of_malformed_messages_is_charged_to_the_budget() {
    let relay = budgeted(8 << 10).await;
    let mut socket = raw_socket(&relay).await;
    // Nothing is done for text that does not parse, but it is charged all the
    // same: the burst covers a few dozen frames and no more.
    for _ in 0..100 {
        if socket.send(Message::text("not json")).await.is_err() {
            break; // closed under us, which is the point
        }
    }
    let mut refused = 0;
    let limited = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let reply = text(&mut socket).await;
            if reply.contains("rate_limited") {
                return refused;
            }
            assert!(reply.contains("malformed message"), "got: {reply}");
            refused += 1;
        }
    })
    .await
    .expect("a flood of malformed frames was never charged");
    assert!(limited > 0, "the budget ran out before one was answered");
    closed(&mut socket).await;
}

#[tokio::test]
async fn a_subscribe_is_charged_as_a_store_read() {
    let relay = budgeted(4 << 10).await;
    let mut socket = raw_socket(&relay).await;
    // Four subscribes, one store read each: the whole burst, to the byte.
    // Priced at the fixed cost they would leave room for what follows.
    for _ in 0..4 {
        let subscribe = serde_json::json!({
            "type": "subscribe",
            "note": NoteId::random().encode(),
            "since": 0,
        });
        socket
            .send(Message::text(subscribe.to_string()))
            .await
            .unwrap();
    }
    socket.send(Message::text("not json")).await.unwrap();
    let reply = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let reply = text(&mut socket).await;
            if reply.contains("rate_limited") || reply.contains("malformed message") {
                return reply;
            }
        }
    })
    .await
    .expect("the malformed frame was never answered");
    assert!(
        reply.contains("rate_limited"),
        "a subscribe was priced at the fixed cost: {reply}"
    );
    closed(&mut socket).await;
}

#[tokio::test]
async fn a_compact_is_charged_by_the_size_of_its_base() {
    let relay = budgeted(6 << 10).await;
    let mut socket = raw_socket(&relay).await;
    let compact = |bytes: usize| {
        serde_json::json!({
            "type": "compact",
            "note": NoteId::random().encode(),
            "covers": 0,
            "base": URL_SAFE_NO_PAD.encode(vec![0u8; bytes]),
            "key": URL_SAFE_NO_PAD.encode([0u8; 32]),
            "signature": URL_SAFE_NO_PAD.encode([0u8; 64]),
        })
        .to_string()
    };
    // A 2 KiB base fits. A 4 KiB one does not, though the burst covers it
    // whole and bytes remain: the charge follows the base. At the fixed cost
    // both would fit.
    socket.send(Message::text(compact(2 << 10))).await.unwrap();
    let first = text(&mut socket).await;
    assert!(
        first.contains("not subscribed to that note"),
        "got: {first}"
    );
    socket.send(Message::text(compact(4 << 10))).await.unwrap();
    let second = text(&mut socket).await;
    assert!(
        second.contains("rate_limited"),
        "a base was not charged by its size: {second}"
    );
    closed(&mut socket).await;
}

#[tokio::test]
async fn attachment_chunks_spend_the_same_budget() {
    let relay = budgeted(100 << 10).await;
    let base = relay.http_base();
    let token = authenticate(&base, None, &DeviceIdentity::generate())
        .await
        .unwrap();
    let (note, id, key) = (
        NoteId::random(),
        AttachmentId::random(),
        ContentKey::random(),
    );
    let chunk = 64 << 10;
    let plain = vec![5u8; 150 << 10];
    let (media, sealed) = seal_whole(&key, note, id, Variant::Original, &plain, chunk).unwrap();
    let url = |rest: &str| {
        format!(
            "{base}/v1/media/{}/{}{rest}?p=2",
            note.encode(),
            media.object.encode()
        )
    };
    let http = reqwest::Client::new();
    let reserved = http
        .post(url(""))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "sealed_len": media.sealed_len(),
            "chunk_size": media.chunk_size,
        }))
        .send()
        .await
        .unwrap()
        .status();
    assert!(reserved.is_success(), "{reserved}");

    let part = chunk as usize + 40;
    let put = |index: usize| {
        http.put(url(&format!("/{index}")))
            .bearer_auth(&token)
            .body(sealed[index * part..((index + 1) * part).min(sealed.len())].to_vec())
            .send()
    };
    assert_eq!(put(0).await.unwrap().status(), StatusCode::CREATED);
    let refused = put(1).await.unwrap();
    assert_eq!(refused.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(refused.headers().contains_key("retry-after"));
}
