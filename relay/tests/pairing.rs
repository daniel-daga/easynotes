//! Account-transfer pairing at the relay: the guest may lose an HTTP response
//! and ask again (#36). The answer stays until the TTL or the owner's close.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use easynotes_relay::{MemoryStore, Relay};
use std::sync::Arc;

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
