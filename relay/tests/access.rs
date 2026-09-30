//! Who may write to a relay ([ADR 0028](../../docs/adr/0028-who-may-write-to-a-relay.md)):
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
use easynotes_core::relay_access::RelayAccess;
use easynotes_core::sync::{authenticate, invite, Incoming};
use easynotes_relay::limit::RateLimit;
use easynotes_relay::{Access, MemoryStore, Relay, RelayConfig, RelayStore};
use reqwest::StatusCode;
use std::sync::Arc;
use support::NoteSocket;

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
    refused(authenticate(&http, Some(&RelayAccess::random()), &device).await);
    let token = authenticate(&http, Some(&key), &device).await.unwrap();

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
    authenticate(&relay.http_base(), Some(&RelayAccess::random()), &device)
        .await
        .unwrap();
}

#[tokio::test]
async fn either_key_works_while_a_rotation_overlaps() {
    let (old, new) = (RelayAccess::random(), RelayAccess::random());
    let relay = gated(&[&old, &new]).await;
    for key in [&old, &new] {
        authenticate(&relay.http_base(), Some(key), &DeviceIdentity::generate())
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
        .post(format!("{http}/v1/challenge?p=1"))
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
        .post(format!("{http}/v1/token?p=1"))
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
    let token = authenticate(&before.http_base(), Some(&old), &device)
        .await
        .unwrap();
    drop(before);

    // The operator hands out a new key and removes the old one.
    let after = gated_on(store, &[&new], signing).await;
    refused(authenticate(&after.http_base(), Some(&old), &device).await);
    // A token already issued is not revoked: it runs out within fifteen
    // minutes, or at once if the signing key is rotated too.
    NoteSocket::connect(&after.ws_base(), &token, NoteId::random(), 0)
        .await
        .unwrap();
}

#[tokio::test]
async fn every_invite_call_needs_the_key_on_a_gated_relay() {
    let key = RelayAccess::random();
    let relay = gated(&[&key]).await;
    let http = relay.http_base();

    let secret = InviteSecret::random();
    let owner = easynotes_core::invite::OwnerSecret::random();
    refused(invite::open(&http, None, &secret, &owner, 3600).await);
    refused(invite::open(&http, Some(&RelayAccess::random()), &secret, &owner, 3600).await);
    invite::open(&http, Some(&key), &secret, &owner, 3600)
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
            "{http}/v1/invites/{}/list?p=1",
            secret.id().encode()
        ))
        .json(&body)
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(status, StatusCode::FORBIDDEN);

    refused(invite::list(&http, None, &secret, &owner).await);
    let listing = invite::list(&http, Some(&key), &secret, &owner)
        .await
        .unwrap();
    assert!(listing.open);
    refused(invite::withdraw(&http, None, &secret, &owner).await);
    invite::withdraw(&http, Some(&key), &secret, &owner)
        .await
        .unwrap();
}

#[tokio::test]
async fn purge_needs_no_key() {
    // Someone whose access was taken away can still delete what they put here:
    // purge's authority is the note key, and it can only destroy.
    let relay = gated(&[&RelayAccess::random()]).await;
    let status = reqwest::Client::new()
        .post(format!("{}/v1/purge?p=1", relay.http_base()))
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
            Some(Incoming::RateLimited) => break,
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
            "{base}/v1/media/{}/{}{rest}?p=1",
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
