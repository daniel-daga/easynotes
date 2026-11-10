//! Who may write to a relay ([ADR 0028](../../docs/decisions.md#adr-0028)):
//! a relay gated on an access key lets in only devices proving it, at the
//! handshake and on every invite call, and leaves purge open; and every relay
//! holds each client address to a write budget, over the socket and for
//! attachment chunks alike.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use easynotes_core::account::RecoveryPhrase;
use easynotes_core::attachment::seal::seal_whole;
use easynotes_core::attachment::{ContentKey, Variant};
use easynotes_core::crypto::{DeviceIdentity, DeviceSigner, Envelope, NoteKey};
use easynotes_core::error::CoreError;
use easynotes_core::ids::{AttachmentId, InviteId, NoteId};
use easynotes_core::invite::InviteSecret;
use easynotes_core::relay_access::{RelayAccess, RelayToken};
use easynotes_core::sync::invite::Answer;
use easynotes_core::sync::{authenticate, invite, pairing, DeviceSync, Incoming, Routed};
use easynotes_relay::limit::RateLimit;
use easynotes_relay::{Access, MemoryStore, Relay, RelayConfig, RelayStore};
use futures_util::{SinkExt, StreamExt};
use reqwest::StatusCode;
use std::sync::Arc;
use std::time::Duration;
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

/// A relay that takes per-device tokens (ADR 0038), and the store those are
/// minted in.
async fn token_relay(store: Arc<dyn RelayStore>) -> Relay {
    token_relay_ttl(store, Duration::from_secs(900)).await
}

async fn token_relay_ttl(store: Arc<dyn RelayStore>, token_ttl: Duration) -> Relay {
    Relay::serve_with(
        store,
        "127.0.0.1:0".parse().unwrap(),
        RelayConfig {
            tokens: true,
            token_ttl,
            ..RelayConfig::default()
        },
    )
    .await
    .unwrap()
}

/// One token as billing would mint it: in the store, unbound, with a known
/// secret so the test can prove it.
async fn mint(
    store: &dyn RelayStore,
    id: [u8; 16],
    secret: [u8; 32],
    expires_at: Option<i64>,
) -> RelayToken {
    let id = URL_SAFE_NO_PAD.encode(id);
    assert!(store
        .create_access_token(&id, &secret, expires_at)
        .await
        .unwrap());
    RelayToken {
        id: Some(id),
        secret: RelayAccess::from_bytes(secret),
    }
}

fn with_secret(token: &RelayToken, secret: RelayAccess) -> RelayToken {
    RelayToken {
        id: token.id.clone(),
        secret,
    }
}

#[tokio::test]
async fn a_minted_token_opens_the_relay_and_nothing_else_does() {
    let store: Arc<dyn RelayStore> = Arc::new(MemoryStore::new());
    let relay = token_relay(store.clone()).await;
    let http = relay.http_base();
    let token = mint(&*store, [1; 16], [2; 32], None).await;
    let device = DeviceIdentity::generate();

    let issued = authenticate(&http, Some(&token), &device).await.unwrap();
    let mut socket = NoteSocket::connect(&relay.ws_base(), &issued, NoteId::random(), 0)
        .await
        .unwrap();
    assert!(matches!(
        socket.next().await.unwrap(),
        Some(Incoming::Caught { .. })
    ));

    // The right ID with the wrong secret, an ID nobody minted, and a legacy
    // key with no ID at all: a token relay does not fall open.
    refused(
        authenticate(
            &http,
            Some(&with_secret(&token, RelayAccess::random())),
            &device,
        )
        .await,
    );
    refused(authenticate(&http, Some(&RelayToken::random()), &device).await);
    refused(
        authenticate(
            &http,
            Some(&RelayToken::from(RelayAccess::random())),
            &device,
        )
        .await,
    );
}

#[tokio::test]
async fn a_revoked_expired_or_twice_claimed_token_is_refused() {
    let store: Arc<dyn RelayStore> = Arc::new(MemoryStore::new());
    let relay = token_relay(store.clone()).await;
    let http = relay.http_base();

    let expired = mint(&*store, [3; 16], [4; 32], Some(0)).await;
    refused(authenticate(&http, Some(&expired), &DeviceIdentity::generate()).await);

    let revoked = mint(&*store, [5; 16], [6; 32], None).await;
    let revoked_id = revoked.id.as_deref().unwrap();
    assert!(store.revoke_access_token(revoked_id, 1).await.unwrap());
    refused(authenticate(&http, Some(&revoked), &DeviceIdentity::generate()).await);

    // One token, one device: the first handshake binds it, and the device
    // that won keeps it.
    let bound = mint(&*store, [7; 16], [8; 32], None).await;
    let first = DeviceIdentity::generate();
    authenticate(&http, Some(&bound), &first).await.unwrap();
    refused(authenticate(&http, Some(&bound), &DeviceIdentity::generate()).await);
    authenticate(&http, Some(&bound), &first).await.unwrap();
}

#[tokio::test]
async fn a_relay_with_keys_and_tokens_accepts_either() {
    let key = RelayAccess::random();
    let store: Arc<dyn RelayStore> = Arc::new(MemoryStore::new());
    let relay = Relay::serve_with(
        store.clone(),
        "127.0.0.1:0".parse().unwrap(),
        RelayConfig {
            access: Access::Keys(vec![key.clone()]),
            tokens: true,
            ..RelayConfig::default()
        },
    )
    .await
    .unwrap();
    let http = relay.http_base();

    authenticate(
        &http,
        Some(&RelayToken::from(key)),
        &DeviceIdentity::generate(),
    )
    .await
    .unwrap();
    let token = mint(&*store, [9; 16], [10; 32], None).await;
    authenticate(&http, Some(&token), &DeviceIdentity::generate())
        .await
        .unwrap();
}

/// A token request with a valid token proof and token ID, but a signature that
/// is not the device's.
async fn token_with_bogus_signature(
    http: &str,
    device: &DeviceIdentity,
    token: &RelayToken,
) -> StatusCode {
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
    let proof = token.token_proof(&raw, &device.public_key().0);
    client
        .post(format!("{http}/v1/token?p=2"))
        .json(&serde_json::json!({
            "device": key,
            "nonce": nonce,
            "signature": URL_SAFE_NO_PAD.encode([0u8; 64]),
            "access": proof,
            "access_id": token.id.as_deref(),
        }))
        .send()
        .await
        .unwrap()
        .status()
}

#[tokio::test]
async fn a_bogus_signature_does_not_claim_a_token() {
    let store: Arc<dyn RelayStore> = Arc::new(MemoryStore::new());
    let relay = token_relay(store.clone()).await;
    let token = mint(&*store, [11; 16], [12; 32], None).await;
    let device = DeviceIdentity::generate();

    assert_eq!(
        token_with_bogus_signature(&relay.http_base(), &device, &token).await,
        StatusCode::UNAUTHORIZED
    );
    let row = store
        .access_token(token.id.as_deref().unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.device_key, None, "a bad signature claimed the token");
}

#[tokio::test]
async fn only_one_of_two_devices_claiming_a_token_wins() {
    let store: Arc<dyn RelayStore> = Arc::new(MemoryStore::new());
    let relay = token_relay(store.clone()).await;
    let http = relay.http_base();
    let token = mint(&*store, [13; 16], [14; 32], None).await;
    let (a, b) = (DeviceIdentity::generate(), DeviceIdentity::generate());

    let (first, second) = tokio::join!(
        authenticate(&http, Some(&token), &a),
        authenticate(&http, Some(&token), &b),
    );
    let winner = match (first, second) {
        (Ok(_), Err(CoreError::AccessRequired)) => &a,
        (Err(CoreError::AccessRequired), Ok(_)) => &b,
        (first, second) => {
            panic!("expected one winner and one refusal, got {first:?} and {second:?}")
        }
    };
    let row = store
        .access_token(token.id.as_deref().unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.device_key, Some(winner.public_key().0));
}

#[tokio::test]
async fn a_bound_token_outlives_its_link_expiry() {
    let store: Arc<dyn RelayStore> = Arc::new(MemoryStore::new());
    let relay = token_relay(store.clone()).await;
    let http = relay.http_base();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let token = mint(&*store, [15; 16], [16; 32], Some(now - 1)).await;
    let device = DeviceIdentity::generate();
    let id = token.id.as_deref().unwrap();

    // Bound first, by the store: the handler refuses an expired unbound token.
    assert!(store
        .bind_access_token(id, &device.public_key().0)
        .await
        .unwrap());
    authenticate(&http, Some(&token), &device).await.unwrap();

    assert!(store.revoke_access_token(id, now).await.unwrap());
    refused(authenticate(&http, Some(&token), &device).await);
}

#[tokio::test]
async fn revoking_one_devices_token_leaves_another_device_on_the_note_working() {
    let store: Arc<dyn RelayStore> = Arc::new(MemoryStore::new());
    let relay = token_relay(store.clone()).await;
    let http = relay.http_base();
    let (note, key) = (NoteId::random(), NoteKey::random());
    let (a, b) = (DeviceIdentity::generate(), DeviceIdentity::generate());
    let a_token = mint(&*store, [48; 16], [49; 32], None).await;
    let b_token = mint(&*store, [50; 16], [51; 32], None).await;
    let a_session = authenticate(&http, Some(&a_token), &a).await.unwrap();
    let b_session = authenticate(&http, Some(&b_token), &b).await.unwrap();

    // A writes the note, and B reads what A wrote and adds its own line.
    let mut a_socket = NoteSocket::connect(&relay.ws_base(), &a_session, note, 0)
        .await
        .unwrap();
    let first = Envelope::seal(&a, note, &key, 0, b"from A")
        .unwrap()
        .encode();
    a_socket.append(&first).await.unwrap();
    loop {
        match a_socket.next().await.unwrap() {
            Some(Incoming::Envelope { envelope, .. }) if envelope == first => break,
            Some(_) => {}
            None => panic!("closed before A's append settled"),
        }
    }

    let mut b_socket = NoteSocket::connect(&relay.ws_base(), &b_session, note, 0)
        .await
        .unwrap();
    assert_eq!(history(&mut b_socket).await, vec![first.clone()]);
    let second = Envelope::seal(&b, note, &key, 1, b"from B")
        .unwrap()
        .encode();
    b_socket.append(&second).await.unwrap();
    loop {
        match b_socket.next().await.unwrap() {
            Some(Incoming::Envelope { envelope, .. }) if envelope == second => break,
            Some(_) => {}
            None => panic!("closed before B's append settled"),
        }
    }
    assert_eq!(store.head(&note.encode()).await.unwrap(), 2);

    // B loses its token; the next handshake is refused. A is untouched: it
    // opens a fresh session, reads both lines and writes a third.
    assert!(store
        .revoke_access_token(b_token.id.as_deref().unwrap(), 1)
        .await
        .unwrap());
    refused(authenticate(&http, Some(&b_token), &b).await);

    let a_session = authenticate(&http, Some(&a_token), &a).await.unwrap();
    let mut a_socket = NoteSocket::connect(&relay.ws_base(), &a_session, note, 0)
        .await
        .unwrap();
    assert_eq!(history(&mut a_socket).await, vec![first, second]);
    let third = Envelope::seal(&a, note, &key, 2, b"still A")
        .unwrap()
        .encode();
    a_socket.append(&third).await.unwrap();
    loop {
        match a_socket.next().await.unwrap() {
            Some(Incoming::Envelope { envelope, .. }) if envelope == third => break,
            Some(_) => {}
            None => panic!("closed before A's last append settled"),
        }
    }
    assert_eq!(store.head(&note.encode()).await.unwrap(), 3);
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

/// A relay that takes both a static key and per-device tokens.
async fn token_relay_with_key(store: Arc<dyn RelayStore>, key: &RelayAccess) -> Relay {
    Relay::serve_with(
        store,
        "127.0.0.1:0".parse().unwrap(),
        RelayConfig {
            access: Access::Keys(vec![key.clone()]),
            tokens: true,
            ..RelayConfig::default()
        },
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn a_static_key_still_opens_an_invite_on_a_key_and_tokens_relay() {
    let key = RelayAccess::random();
    let relay = token_relay_with_key(Arc::new(MemoryStore::new()), &key).await;
    let http = relay.http_base();
    let token = RelayToken::from(key);
    let secret = InviteSecret::random();
    let owner = easynotes_core::invite::OwnerSecret::random();

    invite::open(&http, Some(&token), &secret, &owner, 3600)
        .await
        .unwrap();
    assert!(
        invite::list(&http, Some(&token), &secret, &owner)
            .await
            .unwrap()
            .open
    );
}

#[tokio::test]
async fn a_guest_token_joins_and_reads_an_invite_on_a_token_only_relay() {
    let store: Arc<dyn RelayStore> = Arc::new(MemoryStore::new());
    let relay = token_relay(store.clone()).await;
    let http = relay.http_base();
    let owner_token = mint(&*store, [30; 16], [31; 32], None).await;
    let guest_token = mint(&*store, [32; 16], [33; 32], None).await;
    let secret = InviteSecret::random();
    let owner = easynotes_core::invite::OwnerSecret::random();

    invite::open(&http, Some(&owner_token), &secret, &owner, 3600)
        .await
        .unwrap();

    let member = RecoveryPhrase::generate().root().member();
    let sealed = easynotes_core::invite::seal_request(&secret, &member, "Gus").unwrap();
    let wrong = with_secret(&guest_token, RelayAccess::random());
    refused(invite::request(&http, Some(&wrong), &secret, &member.id(), &sealed).await);
    let (request, _) = invite::request(&http, Some(&guest_token), &secret, &member.id(), &sealed)
        .await
        .unwrap();

    // The owner sees the request the guest left.
    let listing = invite::list(&http, Some(&owner_token), &secret, &owner)
        .await
        .unwrap();
    assert_eq!(listing.requests.len(), 1);

    // The owner answers it, and the guest reads the answer, all with tokens.
    invite::decline(&http, Some(&owner_token), &secret, &owner, &request)
        .await
        .unwrap();
    let answer = invite::poll(&http, Some(&guest_token), &secret, &member, &request)
        .await
        .unwrap();
    assert!(matches!(answer, Answer::Declined));

    // An invite call proves the token; it does not claim it (the handshake does).
    let row = store
        .access_token(guest_token.id.as_deref().unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.device_key, None, "an invite call bound the token");
}

#[tokio::test]
async fn a_revoked_token_is_refused_on_an_invite_call() {
    let store: Arc<dyn RelayStore> = Arc::new(MemoryStore::new());
    let relay = token_relay(store.clone()).await;
    let http = relay.http_base();
    let token = mint(&*store, [34; 16], [35; 32], None).await;
    assert!(store
        .revoke_access_token(token.id.as_deref().unwrap(), 1)
        .await
        .unwrap());

    refused(
        invite::open(
            &http,
            Some(&token),
            &InviteSecret::random(),
            &easynotes_core::invite::OwnerSecret::random(),
            3600,
        )
        .await,
    );
}

#[tokio::test]
async fn a_token_only_relay_refuses_an_invite_call_with_no_token() {
    let store: Arc<dyn RelayStore> = Arc::new(MemoryStore::new());
    let relay = token_relay(store).await;
    let http = relay.http_base();

    refused(
        invite::open(
            &http,
            None,
            &InviteSecret::random(),
            &easynotes_core::invite::OwnerSecret::random(),
            3600,
        )
        .await,
    );
}

#[tokio::test]
async fn a_read_only_token_cannot_create_an_invite_but_can_join_and_read_one() {
    let store: Arc<dyn RelayStore> = Arc::new(MemoryStore::new());
    let relay = token_relay(store.clone()).await;
    let http = relay.http_base();
    let writer = mint(&*store, [36; 16], [37; 32], None).await;
    let reader = mint(&*store, [38; 16], [39; 32], None).await;
    let reader_id = reader.id.as_deref().unwrap();
    store
        .set_access_token_read_only(reader_id, true)
        .await
        .unwrap();
    let secret = InviteSecret::random();
    let owner = easynotes_core::invite::OwnerSecret::random();

    match invite::open(&http, Some(&reader), &secret, &owner, 3600).await {
        Err(CoreError::ReadOnly) => {}
        other => panic!("a read-only token was let open an invite: {other:?}"),
    }

    invite::open(&http, Some(&writer), &secret, &owner, 3600)
        .await
        .unwrap();

    let member = RecoveryPhrase::generate().root().member();
    let sealed = easynotes_core::invite::seal_request(&secret, &member, "Gus").unwrap();
    let (request, _) = invite::request(&http, Some(&reader), &secret, &member.id(), &sealed)
        .await
        .unwrap();
    assert!(matches!(
        invite::poll(&http, Some(&reader), &secret, &member, &request)
            .await
            .unwrap(),
        Answer::Pending { .. }
    ));
    assert_eq!(
        invite::list(&http, Some(&reader), &secret, &owner)
            .await
            .unwrap()
            .requests
            .len(),
        1
    );
}

#[tokio::test]
async fn an_expired_unbound_token_is_refused_on_an_invite_call() {
    let store: Arc<dyn RelayStore> = Arc::new(MemoryStore::new());
    let relay = token_relay(store.clone()).await;
    let token = mint(&*store, [40; 16], [41; 32], Some(0)).await;

    refused(
        invite::open(
            &relay.http_base(),
            Some(&token),
            &InviteSecret::random(),
            &easynotes_core::invite::OwnerSecret::random(),
            3600,
        )
        .await,
    );
}

#[tokio::test]
async fn an_access_id_on_a_keys_only_relay_is_refused() {
    let key = RelayAccess::random();
    let store: Arc<dyn RelayStore> = Arc::new(MemoryStore::new());
    let relay = gated_on(store.clone(), &[&key], [9; 32]).await;
    // A real row, and a proof its secret really makes: only the relay not
    // taking tokens can be the reason this is refused.
    let token = mint(&*store, [46; 16], [47; 32], None).await;

    refused(
        invite::open(
            &relay.http_base(),
            Some(&token),
            &InviteSecret::random(),
            &easynotes_core::invite::OwnerSecret::random(),
            3600,
        )
        .await,
    );
}

#[tokio::test]
async fn a_static_key_is_refused_on_a_token_only_relay() {
    let store: Arc<dyn RelayStore> = Arc::new(MemoryStore::new());
    let relay = token_relay(store).await;

    refused(
        invite::open(
            &relay.http_base(),
            Some(&RelayToken::from(RelayAccess::random())),
            &InviteSecret::random(),
            &easynotes_core::invite::OwnerSecret::random(),
            3600,
        )
        .await,
    );
}

#[tokio::test]
async fn pairing_create_is_gated_like_an_invite() {
    let store: Arc<dyn RelayStore> = Arc::new(MemoryStore::new());
    let relay = token_relay(store.clone()).await;
    let http = relay.http_base();

    // A token-only relay takes no key, so a create with nothing to prove is
    // refused rather than left open.
    refused(
        pairing::open(
            http.clone(),
            None,
            InviteId::random().encode(),
            [1; 32],
            [2; 32],
            300,
        )
        .await,
    );

    let revoked = mint(&*store, [42; 16], [43; 32], None).await;
    assert!(store
        .revoke_access_token(revoked.id.as_deref().unwrap(), 1)
        .await
        .unwrap());
    refused(
        pairing::open(
            http,
            Some(revoked),
            InviteId::random().encode(),
            [1; 32],
            [2; 32],
            300,
        )
        .await,
    );
}

#[tokio::test]
async fn a_billed_token_creates_a_pairing_on_a_key_and_tokens_relay() {
    let key = RelayAccess::random();
    let store: Arc<dyn RelayStore> = Arc::new(MemoryStore::new());
    let relay = token_relay_with_key(store.clone(), &key).await;
    let token = mint(&*store, [44; 16], [45; 32], None).await;

    pairing::open(
        relay.http_base(),
        Some(token),
        InviteId::random().encode(),
        [1; 32],
        [2; 32],
        300,
    )
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
    raw_socket_with(relay, &token).await
}

async fn raw_socket_with(relay: &Relay, token: &str) -> RawSocket {
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

/// Read until the relay says the backlog is drained, collecting its envelopes.
async fn history(socket: &mut NoteSocket) -> Vec<Vec<u8>> {
    let mut envelopes = Vec::new();
    loop {
        match socket.next().await.unwrap() {
            Some(Incoming::Envelope { envelope, .. }) => envelopes.push(envelope),
            Some(Incoming::Caught { .. }) => return envelopes,
            Some(Incoming::Rejected(m)) => panic!("refused while reading: {m}"),
            Some(_) => {}
            None => panic!("closed while reading"),
        }
    }
}

#[tokio::test]
async fn a_read_only_token_reads_history_and_cannot_append() {
    let store: Arc<dyn RelayStore> = Arc::new(MemoryStore::new());
    let relay = token_relay(store.clone()).await;
    let http = relay.http_base();
    let (note, key) = (NoteId::random(), NoteKey::random());

    // A writable device puts one envelope in the note.
    let writer = DeviceIdentity::generate();
    let token = mint(&*store, [17; 16], [18; 32], None).await;
    let token = authenticate(&http, Some(&token), &writer).await.unwrap();
    let mut socket = NoteSocket::connect(&relay.ws_base(), &token, note, 0)
        .await
        .unwrap();
    let written = Envelope::seal(&writer, note, &key, 0, b"before")
        .unwrap()
        .encode();
    socket.append(&written).await.unwrap();
    // `caught` and the append's echo can arrive in either order.
    let mut seen = Vec::new();
    let mut caught = false;
    while !caught || seen.is_empty() {
        match socket.next().await.unwrap() {
            Some(Incoming::Envelope { envelope, .. }) => seen.push(envelope),
            Some(Incoming::Caught { .. }) => caught = true,
            Some(_) => {}
            None => panic!("closed before the append settled"),
        }
    }
    assert_eq!(seen, vec![written.clone()]);

    // A read-only row yields a version-2 token whose flag is set.
    let token = mint(&*store, [19; 16], [20; 32], None).await;
    let id = token.id.as_deref().unwrap();
    store.set_access_token_read_only(id, true).await.unwrap();
    let reader = DeviceIdentity::generate();
    let token = authenticate(&http, Some(&token), &reader).await.unwrap();
    let payload = URL_SAFE_NO_PAD.decode(&token).unwrap();
    assert_eq!(payload[0], 2);
    // Version, expiry, device key, nonce, then the flag.
    assert_eq!(payload[1 + 8 + 32 + 16], 1);

    // It reads the note's history...
    let mut socket = NoteSocket::connect(&relay.ws_base(), &token, note, 0)
        .await
        .unwrap();
    assert_eq!(history(&mut socket).await, vec![written]);

    // ...and an append is refused, in words, and not stored.
    let refused = Envelope::seal(&reader, note, &key, 1, b"after")
        .unwrap()
        .encode();
    socket.append(&refused).await.unwrap();
    match socket.next().await.unwrap() {
        Some(Incoming::Rejected(words)) => assert!(words.contains("read-only"), "{words}"),
        Some(_) => panic!("expected the append to be refused"),
        None => panic!("closed instead of refusing the append"),
    }
    assert_eq!(store.head(&note.encode()).await.unwrap(), 1);
}

#[tokio::test]
async fn a_read_only_device_cannot_compact() {
    let store: Arc<dyn RelayStore> = Arc::new(MemoryStore::new());
    let relay = token_relay(store.clone()).await;
    let (note, key) = (NoteId::random(), NoteKey::random());
    let note_id = note.encode();
    let written = Envelope::seal(&DeviceIdentity::generate(), note, &key, 0, b"before")
        .unwrap()
        .encode();
    store
        .append_within(&note_id, &written, &easynotes_relay::Limits::default())
        .await
        .unwrap();
    let before_head = store.head(&note_id).await.unwrap();
    let before_log = store.page(&note_id, 0, 100).await.unwrap();

    let token = mint(&*store, [25; 16], [26; 32], None).await;
    let id = token.id.as_deref().unwrap();
    store.set_access_token_read_only(id, true).await.unwrap();
    let token = authenticate(
        &relay.http_base(),
        Some(&token),
        &DeviceIdentity::generate(),
    )
    .await
    .unwrap();
    let mut socket = raw_socket_with(&relay, &token).await;
    socket
        .send(Message::text(
            serde_json::json!({ "type": "subscribe", "note": &note_id, "since": 0 }).to_string(),
        ))
        .await
        .unwrap();
    loop {
        if text(&mut socket).await.contains("\"caught\"") {
            break;
        }
    }

    let compact = serde_json::json!({
        "type": "compact",
        "note": &note_id,
        "covers": 1,
        "base": URL_SAFE_NO_PAD.encode([0u8; 8]),
        "key": URL_SAFE_NO_PAD.encode([1u8; 32]),
        "signature": URL_SAFE_NO_PAD.encode([0u8; 64]),
    });
    socket
        .send(Message::text(compact.to_string()))
        .await
        .unwrap();
    let refusal = text(&mut socket).await;
    assert!(refusal.contains("\"compact_refused\""), "{refusal}");
    assert!(refusal.contains("read_only"), "{refusal}");

    // The log, and what a client would replay, are exactly as they were.
    assert_eq!(store.head(&note_id).await.unwrap(), before_head);
    let after_log = store.page(&note_id, 0, 100).await.unwrap();
    assert_eq!(after_log.len(), before_log.len());
    for (before, after) in before_log.iter().zip(&after_log) {
        assert_eq!(before.seq, after.seq);
        assert_eq!(before.envelope, after.envelope);
    }
}

#[tokio::test]
async fn a_socket_closes_when_its_token_expires() {
    let store: Arc<dyn RelayStore> = Arc::new(MemoryStore::new());
    let relay = token_relay_ttl(store.clone(), Duration::from_secs(1)).await;

    // Start just after a whole second, so the token's one-second life is not
    // shortened by an expiry truncated to seconds.
    let subsec = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .subsec_millis();
    tokio::time::sleep(Duration::from_millis(1000 - subsec as u64 + 10)).await;

    let token = mint(&*store, [27; 16], [28; 32], None).await;
    let device = DeviceIdentity::generate();
    let token = authenticate(&relay.http_base(), Some(&token), &device)
        .await
        .unwrap();
    let (note, key) = (NoteId::random(), NoteKey::random());
    let mut socket = NoteSocket::connect(&relay.ws_base(), &token, note, 0)
        .await
        .unwrap();
    assert!(matches!(
        socket.next().await.unwrap(),
        Some(Incoming::Caught { .. })
    ));

    tokio::time::sleep(Duration::from_secs(2)).await;
    let closed = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match socket.next().await {
                Ok(None) | Err(_) => return,
                Ok(Some(_)) => {}
            }
        }
    })
    .await;
    assert!(closed.is_ok(), "the relay kept an expired session open");

    // The socket no longer takes writes, so nothing lands after expiry.
    let late = Envelope::seal(&device, note, &key, 0, b"late")
        .unwrap()
        .encode();
    let _ = socket.append(&late).await;
    assert_eq!(store.head(&note.encode()).await.unwrap(), 0);
}

#[tokio::test]
async fn a_read_only_device_can_read_attachments_but_not_upload() {
    let store: Arc<dyn RelayStore> = Arc::new(MemoryStore::new());
    let relay = token_relay(store.clone()).await;
    let http = relay.http_base();
    let (note, id, key) = (
        NoteId::random(),
        AttachmentId::random(),
        ContentKey::random(),
    );
    let chunk = 64 << 10;
    let plain = vec![5u8; 100 << 10];
    let (media, sealed) = seal_whole(&key, note, id, Variant::Original, &plain, chunk).unwrap();
    let client = reqwest::Client::new();
    let url = |rest: &str| {
        format!(
            "{http}/v1/media/{}/{}{rest}?p=2",
            note.encode(),
            media.object.encode()
        )
    };

    // A writable device uploads and commits the object.
    let token = mint(&*store, [21; 16], [22; 32], None).await;
    let token = authenticate(&http, Some(&token), &DeviceIdentity::generate())
        .await
        .unwrap();
    let reserved = client
        .post(url(""))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "sealed_len": media.sealed_len(),
            "chunk_size": media.chunk_size,
        }))
        .send()
        .await
        .unwrap();
    assert!(reserved.status().is_success(), "{}", reserved.status());
    let part = chunk as usize + 40;
    for index in 0..media.chunks() as usize {
        let body = sealed[index * part..((index + 1) * part).min(sealed.len())].to_vec();
        let put = client
            .put(url(&format!("/{index}")))
            .bearer_auth(&token)
            .body(body)
            .send()
            .await
            .unwrap();
        assert!(put.status().is_success(), "{}", put.status());
    }
    let committed = client
        .post(url("/commit"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "digest": URL_SAFE_NO_PAD.encode(media.digest) }))
        .send()
        .await
        .unwrap();
    assert!(committed.status().is_success(), "{}", committed.status());

    // A read-only device reads it.
    let token = mint(&*store, [23; 16], [24; 32], None).await;
    let id = token.id.as_deref().unwrap();
    store.set_access_token_read_only(id, true).await.unwrap();
    let token = authenticate(&http, Some(&token), &DeviceIdentity::generate())
        .await
        .unwrap();
    let status = client
        .get(url("/status"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert!(status.status().is_success(), "{}", status.status());
    // A chunk at a time, as the core reads: the relay serves no more per
    // request.
    let mut fetched = Vec::new();
    for index in 0..media.chunks() as usize {
        let at = index * part;
        let end = ((index + 1) * part).min(sealed.len()) - 1;
        let response = client
            .get(url(""))
            .bearer_auth(&token)
            .header("range", format!("bytes={at}-{end}"))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        fetched.extend_from_slice(&response.bytes().await.unwrap());
    }
    assert_eq!(fetched, sealed);

    // But its uploads are refused, with the code.
    for response in [
        client
            .post(url(""))
            .bearer_auth(&token)
            .json(&serde_json::json!({
                "sealed_len": media.sealed_len(),
                "chunk_size": media.chunk_size,
            }))
            .send()
            .await
            .unwrap(),
        client
            .put(url("/0"))
            .bearer_auth(&token)
            .body(sealed[..part].to_vec())
            .send()
            .await
            .unwrap(),
        client
            .post(url("/commit"))
            .bearer_auth(&token)
            .json(&serde_json::json!({ "digest": URL_SAFE_NO_PAD.encode(media.digest) }))
            .send()
            .await
            .unwrap(),
    ] {
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let code: serde_json::Value = response.json().await.unwrap();
        assert_eq!(code["code"], "read_only");
    }

    // The core turns that code into a permanent refusal with a sentence, not
    // a retryable storage error.
    let error = easynotes_core::sync::media::upload(&http, &token, note, &media, |index| {
        let at = index as usize * part;
        let body = sealed[at..(at + part).min(sealed.len())].to_vec();
        Box::pin(async move { Ok::<_, CoreError>(body) })
    })
    .await
    .unwrap_err();
    assert!(error.to_string().contains("read-only"), "{error}");
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
