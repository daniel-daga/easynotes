//! Account-transfer pairing at the relay: the guest may lose an HTTP response
//! and ask again (#36). The answer stays until the TTL or the owner's close.
//! The refusals around that — a wrong secret, a second device, a premature
//! approval — and the access gate are below (#35).

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use easynotes_core::ids::InviteId;
use easynotes_core::relay_access::RelayAccess;
use easynotes_relay::{Access, MemoryStore, Relay, RelayConfig};
use std::sync::Arc;
use std::time::Duration;

async fn relay() -> Relay {
    Relay::serve(Arc::new(MemoryStore::new()), "127.0.0.1:0".parse().unwrap())
        .await
        .unwrap()
}

fn b64(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

/// As the relay checks it: the SHA-256 of the capability's bytes, not of its text.
fn hash(token: &str) -> String {
    use sha2::{Digest, Sha256};
    b64(&Sha256::digest(URL_SAFE_NO_PAD.decode(token).unwrap()))
}

async fn post(http: &str, path: &str, body: serde_json::Value) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("{http}{path}?p=2"))
        .json(&body)
        .send()
        .await
        .unwrap()
}

/// A create body. `access` is `None` on an open relay; a gated one wants the
/// proof here or it refuses `create`.
fn create(
    id: &str,
    guest: &str,
    owner: &str,
    expires_in: u64,
    access: Option<&str>,
) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "guest_hash": hash(guest),
        "owner_hash": hash(owner),
        "expires_in": expires_in,
        "access": access,
    })
}

#[tokio::test]
async fn a_lost_poll_response_can_be_asked_again() {
    let relay = relay().await;
    let http = relay.http_base();
    let id = b64(&[7u8; 16]);
    let guest = b64(&[1u8; 32]);
    let owner = b64(&[2u8; 32]);
    let public = b64(&[3u8; 32]);
    let ciphertext = b64(b"the transfer");

    let created = post(
        &http,
        "/v1/pairings",
        serde_json::json!({
            "id": id,
            "guest_hash": hash(&guest),
            "owner_hash": hash(&owner),
            "expires_in": 300,
        }),
    )
    .await;
    assert_eq!(created.status(), 201);

    let requested = post(
        &http,
        &format!("/v1/pairings/{id}/request"),
        serde_json::json!({ "token": guest, "public_key": public }),
    )
    .await;
    assert_eq!(requested.status(), 204);

    let approved = post(
        &http,
        &format!("/v1/pairings/{id}/response"),
        serde_json::json!({ "owner": owner, "ciphertext": ciphertext }),
    )
    .await;
    assert_eq!(approved.status(), 204);

    // The guest loses the first answer and polls again: it is still there.
    for _ in 0..2 {
        let polled = post(
            &http,
            &format!("/v1/pairings/{id}/poll-response"),
            serde_json::json!({ "token": guest }),
        )
        .await;
        assert_eq!(polled.status(), 200);
        let body: serde_json::Value = polled.json().await.unwrap();
        assert_eq!(body["ciphertext"], ciphertext);
    }

    // Close still ends the pairing, so nobody waits out the five minutes.
    let closed = post(
        &http,
        &format!("/v1/pairings/{id}/close"),
        serde_json::json!({ "owner": owner }),
    )
    .await;
    assert_eq!(closed.status(), 204);
    let after = post(
        &http,
        &format!("/v1/pairings/{id}/poll-response"),
        serde_json::json!({ "token": guest }),
    )
    .await;
    assert_eq!(after.status(), 404);
}

#[tokio::test]
async fn a_long_lived_pairing_is_clamped_to_five_minutes() {
    let relay = relay().await;
    let http = relay.http_base();
    let created = post(
        &http,
        "/v1/pairings",
        create(
            &b64(&[7u8; 16]),
            &b64(&[1u8; 32]),
            &b64(&[2u8; 32]),
            9999,
            None,
        ),
    )
    .await;
    assert_eq!(created.status(), 201);
    let body: serde_json::Value = created.json().await.unwrap();
    assert_eq!(body["expires_in"], 300);
}

#[tokio::test]
async fn an_expired_pairing_is_gone_and_its_id_is_free_again() {
    let relay = relay().await;
    let http = relay.http_base();
    let (id, guest, owner) = (b64(&[7u8; 16]), b64(&[1u8; 32]), b64(&[2u8; 32]));
    let created = post(&http, "/v1/pairings", create(&id, &guest, &owner, 1, None)).await;
    assert_eq!(created.status(), 201);

    tokio::time::sleep(Duration::from_millis(1100)).await;
    // The next call finds nothing, and drops the expired entry: its id is free.
    let polled = post(
        &http,
        &format!("/v1/pairings/{id}/poll-request"),
        serde_json::json!({ "owner": owner }),
    )
    .await;
    assert_eq!(polled.status(), 404);
    let again = post(
        &http,
        "/v1/pairings",
        create(&id, &guest, &owner, 300, None),
    )
    .await;
    assert_eq!(again.status(), 201);
}

#[tokio::test]
async fn a_wrong_secret_is_not_found_rather_than_forbidden() {
    let relay = relay().await;
    let http = relay.http_base();
    let (id, guest, owner, stranger) = (
        b64(&[7u8; 16]),
        b64(&[1u8; 32]),
        b64(&[2u8; 32]),
        b64(&[9u8; 32]),
    );
    let created = post(
        &http,
        "/v1/pairings",
        create(&id, &guest, &owner, 300, None),
    )
    .await;
    assert_eq!(created.status(), 201);

    // The relay does not say a pairing exists and is not yours.
    let requested = post(
        &http,
        &format!("/v1/pairings/{id}/request"),
        serde_json::json!({ "token": stranger, "public_key": b64(&[3u8; 32]) }),
    )
    .await;
    assert_eq!(requested.status(), 404);

    for (path, body) in [
        ("poll-request", serde_json::json!({ "owner": stranger })),
        (
            "response",
            serde_json::json!({ "owner": stranger, "ciphertext": b64(b"no") }),
        ),
    ] {
        let refused = post(&http, &format!("/v1/pairings/{id}/{path}"), body).await;
        assert_eq!(refused.status(), 404, "{path}");
    }

    // The owner's secret on a guest call is equally unknown.
    let polled = post(
        &http,
        &format!("/v1/pairings/{id}/poll-response"),
        serde_json::json!({ "token": owner }),
    )
    .await;
    assert_eq!(polled.status(), 404);
}

#[tokio::test]
async fn a_second_device_request_is_refused() {
    let relay = relay().await;
    let http = relay.http_base();
    let (id, guest, owner) = (b64(&[7u8; 16]), b64(&[1u8; 32]), b64(&[2u8; 32]));
    let created = post(
        &http,
        "/v1/pairings",
        create(&id, &guest, &owner, 300, None),
    )
    .await;
    assert_eq!(created.status(), 201);

    let first = b64(&[3u8; 32]);
    for _ in 0..2 {
        // The same key again is the guest retrying, not a second device.
        let requested = post(
            &http,
            &format!("/v1/pairings/{id}/request"),
            serde_json::json!({ "token": guest, "public_key": first }),
        )
        .await;
        assert_eq!(requested.status(), 204);
    }
    let other = post(
        &http,
        &format!("/v1/pairings/{id}/request"),
        serde_json::json!({ "token": guest, "public_key": b64(&[4u8; 32]) }),
    )
    .await;
    assert_eq!(other.status(), 409);
}

#[tokio::test]
async fn approving_before_any_device_asked_is_refused() {
    let relay = relay().await;
    let http = relay.http_base();
    let (id, guest, owner) = (b64(&[7u8; 16]), b64(&[1u8; 32]), b64(&[2u8; 32]));
    let created = post(
        &http,
        "/v1/pairings",
        create(&id, &guest, &owner, 300, None),
    )
    .await;
    assert_eq!(created.status(), 201);

    // There is no key to encrypt to yet.
    let approved = post(
        &http,
        &format!("/v1/pairings/{id}/response"),
        serde_json::json!({ "owner": owner, "ciphertext": b64(b"early") }),
    )
    .await;
    assert_eq!(approved.status(), 409);
}

#[tokio::test]
async fn only_the_owner_can_close() {
    let relay = relay().await;
    let http = relay.http_base();
    let (id, guest, owner) = (b64(&[7u8; 16]), b64(&[1u8; 32]), b64(&[2u8; 32]));
    let created = post(
        &http,
        "/v1/pairings",
        create(&id, &guest, &owner, 300, None),
    )
    .await;
    assert_eq!(created.status(), 201);

    for wrong in [guest.clone(), b64(&[9u8; 32])] {
        let refused = post(
            &http,
            &format!("/v1/pairings/{id}/close"),
            serde_json::json!({ "owner": wrong }),
        )
        .await;
        assert_eq!(refused.status(), 404);
    }
    // Still there for its owner, who is the one who may end it.
    let closed = post(
        &http,
        &format!("/v1/pairings/{id}/close"),
        serde_json::json!({ "owner": owner }),
    )
    .await;
    assert_eq!(closed.status(), 204);
    let after = post(
        &http,
        &format!("/v1/pairings/{id}/poll-request"),
        serde_json::json!({ "owner": owner }),
    )
    .await;
    assert_eq!(after.status(), 404);
}

#[tokio::test]
async fn a_gated_relay_asks_the_creator_for_its_key_and_not_the_guest() {
    let key = RelayAccess::random();
    let relay = Relay::serve_with(
        Arc::new(MemoryStore::new()),
        "127.0.0.1:0".parse().unwrap(),
        RelayConfig {
            access: Access::Keys(vec![key.clone()]),
            ..RelayConfig::default()
        },
    )
    .await
    .unwrap();
    let http = relay.http_base();
    let (id, guest, owner) = (b64(&[7u8; 16]), b64(&[1u8; 32]), b64(&[2u8; 32]));

    // Starting a pairing stores something, so it needs the relay's key.
    let refused = post(
        &http,
        "/v1/pairings",
        create(&id, &guest, &owner, 300, None),
    )
    .await;
    assert_eq!(refused.status(), 403);
    let proof = key.invite_proof(&InviteId::decode(&id).unwrap());
    let created = post(
        &http,
        "/v1/pairings",
        create(&id, &guest, &owner, 300, Some(&proof)),
    )
    .await;
    assert_eq!(created.status(), 201);

    // From here the two secrets are the authority: the guest has no key,
    // and its calls carry no proof.
    let requested = post(
        &http,
        &format!("/v1/pairings/{id}/request"),
        serde_json::json!({ "token": guest, "public_key": b64(&[3u8; 32]) }),
    )
    .await;
    assert_eq!(requested.status(), 204);
    let approved = post(
        &http,
        &format!("/v1/pairings/{id}/response"),
        serde_json::json!({ "owner": owner, "ciphertext": b64(b"the transfer") }),
    )
    .await;
    assert_eq!(approved.status(), 204);
    let polled = post(
        &http,
        &format!("/v1/pairings/{id}/poll-response"),
        serde_json::json!({ "token": guest }),
    )
    .await;
    assert_eq!(polled.status(), 200);
}
