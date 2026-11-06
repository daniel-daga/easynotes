//! The private admin listener: the only way billing mints, revokes, marks
//! read-only and unbinds a per-device access token ([ADR 0038]).
//!
//! Never on the public router, and every route behind one bearer secret.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use easynotes_core::relay_access::RelayAccess;
use easynotes_relay::{MemoryStore, Relay, RelayStore};
use reqwest::StatusCode;
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::sync::Arc;

/// A running public relay and a private admin listener over the same store.
struct Admin {
    relay: Relay,
    addr: SocketAddr,
    secret: String,
    store: Arc<dyn RelayStore>,
}

impl Admin {
    fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.addr)
    }

    async fn call(&self, path: &str, auth: Option<&str>, body: Option<Value>) -> reqwest::Response {
        let mut request = reqwest::Client::new().post(self.url(path));
        if let Some(auth) = auth {
            request = request.bearer_auth(auth);
        }
        if let Some(body) = body {
            request = request.json(&body);
        }
        request.send().await.unwrap()
    }

    async fn create(&self, id: &str) -> reqwest::Response {
        self.call("/tokens", Some(&self.secret), Some(json!({ "id": id })))
            .await
    }

    async fn revoke(&self, id: &str) -> reqwest::Response {
        self.call(&format!("/tokens/{id}/revoke"), Some(&self.secret), None)
            .await
    }

    async fn set_read_only(&self, id: &str, read_only: bool) -> reqwest::Response {
        self.call(
            &format!("/tokens/{id}/read-only"),
            Some(&self.secret),
            Some(json!({ "read_only": read_only })),
        )
        .await
    }

    async fn unbind(&self, id: &str) -> reqwest::Response {
        self.call(&format!("/tokens/{id}/unbind"), Some(&self.secret), None)
            .await
    }
}

async fn admin() -> Admin {
    let store: Arc<dyn RelayStore> = Arc::new(MemoryStore::new());
    let relay = Relay::serve(store.clone(), "127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let secret = RelayAccess::random();
    let app = easynotes_relay::admin::router(relay.state.clone(), secret.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    Admin {
        relay,
        addr,
        secret: secret.encode(),
        store,
    }
}

/// A caller-chosen ID: sixteen random bytes, base64url.
fn token_id(seed: u8) -> String {
    URL_SAFE_NO_PAD.encode([seed; 16])
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

#[tokio::test]
async fn a_call_without_the_right_bearer_is_refused() {
    let admin = admin().await;
    let id = token_id(1);
    let body = Some(json!({ "id": &id }));
    for auth in [None, Some("not the secret"), Some("Bearer nope")] {
        let response = admin.call("/tokens", auth, body.clone()).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
    assert!(
        admin.store.access_token(&id).await.unwrap().is_none(),
        "an unauthenticated call minted a token"
    );
}

#[tokio::test]
async fn create_is_idempotent_and_a_revoked_id_conflicts() {
    let admin = admin().await;
    let id = token_id(2);

    let first = admin.create(&id).await;
    assert_eq!(first.status(), StatusCode::CREATED);
    let first: Value = first.json().await.unwrap();
    let secret = first["secret"].as_str().unwrap().to_owned();
    let bytes: [u8; 32] = URL_SAFE_NO_PAD.decode(&secret).unwrap().try_into().unwrap();
    let row = admin.store.access_token(&id).await.unwrap().unwrap();
    assert_eq!(row.secret, bytes);
    assert_eq!(row.revoked_at, None);

    let again = admin.create(&id).await;
    assert_eq!(again.status(), StatusCode::OK);
    let again: Value = again.json().await.unwrap();
    assert_eq!(again["secret"], secret, "a retry changed the secret");

    assert_eq!(admin.revoke(&id).await.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        admin.create(&id).await.status(),
        StatusCode::CONFLICT,
        "a revoked ID was minted again"
    );
}

#[tokio::test]
async fn revoking_twice_still_succeeds_and_a_missing_id_is_404() {
    let admin = admin().await;
    let id = token_id(3);
    assert_eq!(admin.create(&id).await.status(), StatusCode::CREATED);
    assert_eq!(admin.revoke(&id).await.status(), StatusCode::NO_CONTENT);
    assert_eq!(admin.revoke(&id).await.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        admin.revoke(&token_id(4)).await.status(),
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn read_only_and_unbind_change_the_row_and_missing_ids_are_404() {
    let admin = admin().await;
    let id = token_id(5);
    assert_eq!(admin.create(&id).await.status(), StatusCode::CREATED);

    assert_eq!(
        admin.set_read_only(&id, true).await.status(),
        StatusCode::NO_CONTENT
    );
    assert!(
        admin
            .store
            .access_token(&id)
            .await
            .unwrap()
            .unwrap()
            .read_only
    );
    assert_eq!(
        admin.set_read_only(&id, false).await.status(),
        StatusCode::NO_CONTENT
    );
    assert!(
        !admin
            .store
            .access_token(&id)
            .await
            .unwrap()
            .unwrap()
            .read_only
    );

    assert!(admin.store.bind_access_token(&id, &[9; 32]).await.unwrap());
    assert_eq!(admin.unbind(&id).await.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        admin
            .store
            .access_token(&id)
            .await
            .unwrap()
            .unwrap()
            .device_key,
        None
    );

    let missing = token_id(6);
    assert_eq!(
        admin.set_read_only(&missing, true).await.status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(admin.unbind(&missing).await.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn the_public_listener_has_no_admin_routes() {
    let admin = admin().await;
    let response = reqwest::Client::new()
        .post(format!("{}/v1/admin/tokens?p=2", admin.relay.http_base()))
        .bearer_auth(&admin.secret)
        .json(&json!({ "id": token_id(7) }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_purge_drops_rows_revoked_more_than_thirty_days_ago() {
    let admin = admin().await;
    let at = now();
    let old = token_id(8);
    let recent = token_id(9);
    let live = token_id(10);
    for id in [&old, &recent, &live] {
        assert_eq!(admin.create(id).await.status(), StatusCode::CREATED);
    }
    let day = 24 * 60 * 60;
    assert!(admin
        .store
        .revoke_access_token(&old, at - 31 * day)
        .await
        .unwrap());
    assert!(admin
        .store
        .revoke_access_token(&recent, at - 29 * day)
        .await
        .unwrap());

    // Any authenticated admin call sweeps: the request still succeeds.
    assert_eq!(
        admin.create(&token_id(11)).await.status(),
        StatusCode::CREATED
    );

    assert!(
        admin.store.access_token(&old).await.unwrap().is_none(),
        "a token revoked 31 days ago survived the purge"
    );
    assert!(admin.store.access_token(&recent).await.unwrap().is_some());
    assert!(admin.store.access_token(&live).await.unwrap().is_some());
}
