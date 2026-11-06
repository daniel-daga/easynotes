//! The private admin listener: the only way billing mints, revokes, marks
//! read-only and unbinds a per-device access token ([ADR 0038]).
//!
//! Never on the public router, and every route behind one bearer secret.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use easynotes_core::relay_access::{RelayAccess, RelayToken};
use easynotes_relay::cli::AdminClient;
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

    /// The command-line client, pointed at this listener the way an operator's
    /// environment would.
    fn client(&self) -> AdminClient {
        let addr = self.addr.to_string();
        AdminClient::new(Some(&addr), Some(&self.secret)).unwrap()
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

/// How long an unbound link is good for, as `admin::create` and `unbind` set it.
const UNBOUND_TTL_SECS: i64 = 24 * 60 * 60;

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
    let minted_at = now();

    let first = admin.create(&id).await;
    assert_eq!(first.status(), StatusCode::CREATED);
    let first: Value = first.json().await.unwrap();
    let secret = first["secret"].as_str().unwrap().to_owned();
    let bytes: [u8; 32] = URL_SAFE_NO_PAD.decode(&secret).unwrap().try_into().unwrap();
    let row = admin.store.access_token(&id).await.unwrap().unwrap();
    assert_eq!(row.secret, bytes);
    assert_eq!(row.revoked_at, None);
    assert!(
        (row.expires_at.unwrap() - (minted_at + UNBOUND_TTL_SECS)).abs() <= 5,
        "a fresh token did not start its 24-hour link life"
    );

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

    let unbind_id = token_id(6);
    assert!(admin
        .store
        .create_access_token(&unbind_id, &[7; 32], Some(0))
        .await
        .unwrap());
    assert!(admin
        .store
        .bind_access_token(&unbind_id, &[9; 32])
        .await
        .unwrap());
    let rebound_at = now();
    assert_eq!(
        admin.unbind(&unbind_id).await.status(),
        StatusCode::NO_CONTENT
    );
    let unbound = admin.store.access_token(&unbind_id).await.unwrap().unwrap();
    assert_eq!(unbound.device_key, None);
    assert!(
        (unbound.expires_at.unwrap() - (rebound_at + UNBOUND_TTL_SECS)).abs() <= 5,
        "unbinding left the old link expiry in place"
    );

    let missing = token_id(7);
    assert_eq!(
        admin.set_read_only(&missing, true).await.status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(admin.unbind(&missing).await.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_duplicate_id_on_a_dead_unbound_link_conflicts() {
    let admin = admin().await;
    let id = token_id(12);
    let expired = now() - 1;
    assert!(admin
        .store
        .create_access_token(&id, &[3; 32], Some(expired))
        .await
        .unwrap());

    assert_eq!(admin.create(&id).await.status(), StatusCode::CONFLICT);

    // The dead row is left alone: billing gets a refusal, not a live secret
    // over an expired link.
    let row = admin.store.access_token(&id).await.unwrap().unwrap();
    assert_eq!(row.secret, [3; 32], "the dead link was re-minted");
    assert_eq!(row.expires_at, Some(expired));
}

#[tokio::test]
async fn the_public_listener_has_no_admin_routes() {
    let admin = admin().await;
    let existing = token_id(7);
    assert_eq!(admin.create(&existing).await.status(), StatusCode::CREATED);

    let client = reqwest::Client::new();
    let public = admin.relay.http_base();
    let fresh = token_id(8);
    let created = client
        .post(format!("{public}/tokens?p=2"))
        .bearer_auth(&admin.secret)
        .json(&json!({ "id": &fresh }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::NOT_FOUND);
    assert!(
        admin.store.access_token(&fresh).await.unwrap().is_none(),
        "the public listener minted a token"
    );

    let revoked = client
        .post(format!("{public}/tokens/{existing}/revoke?p=2"))
        .bearer_auth(&admin.secret)
        .send()
        .await
        .unwrap();
    assert_eq!(revoked.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        admin
            .store
            .access_token(&existing)
            .await
            .unwrap()
            .unwrap()
            .revoked_at,
        None,
        "the public listener revoked a token"
    );
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

    // Any authenticated admin call sweeps, off the request path: wait for it
    // rather than assuming the task has run before the response.
    assert_eq!(
        admin.create(&token_id(11)).await.status(),
        StatusCode::CREATED
    );
    let swept = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if admin.store.access_token(&old).await.unwrap().is_none() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(
        swept.is_ok(),
        "a token revoked 31 days ago survived the purge"
    );

    assert!(admin.store.access_token(&recent).await.unwrap().is_some());
    assert!(admin.store.access_token(&live).await.unwrap().is_some());
}

#[tokio::test]
async fn the_cli_mints_a_token_the_relay_stored() {
    let admin = admin().await;
    let token = admin.client().create().await.unwrap();

    let printed = token.encode();
    assert_eq!(printed.matches('.').count(), 1);
    assert_eq!(
        RelayToken::decode(&printed).unwrap(),
        token,
        "the printed token would not parse"
    );

    let id = token.id.clone().unwrap();
    let row = admin.store.access_token(&id).await.unwrap().unwrap();
    assert_eq!(
        RelayAccess::from_bytes(row.secret).encode(),
        token.secret.encode(),
        "the relay stored a different secret than the one printed"
    );
    assert_eq!(row.revoked_at, None);
}

#[tokio::test]
async fn the_cli_revokes_makes_read_only_and_unbinds() {
    let admin = admin().await;
    let client = admin.client();
    let id = token_id(13);
    assert_eq!(admin.create(&id).await.status(), StatusCode::CREATED);
    assert!(admin.store.bind_access_token(&id, &[3; 32]).await.unwrap());

    client.read_only(&id, true).await.unwrap();
    assert!(
        admin
            .store
            .access_token(&id)
            .await
            .unwrap()
            .unwrap()
            .read_only
    );
    client.read_only(&id, false).await.unwrap();
    assert!(
        !admin
            .store
            .access_token(&id)
            .await
            .unwrap()
            .unwrap()
            .read_only
    );

    client.unbind(&id).await.unwrap();
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

    client.revoke(&id).await.unwrap();
    assert!(admin
        .store
        .access_token(&id)
        .await
        .unwrap()
        .unwrap()
        .revoked_at
        .is_some());
}

#[test]
fn the_cli_refuses_to_guess_the_admin_listener() {
    let secret = RelayAccess::random().encode();
    assert!(AdminClient::new(None, Some(&secret)).is_err());
    assert!(AdminClient::new(Some("  "), Some(&secret)).is_err());
    assert!(AdminClient::new(Some("127.0.0.1:9091"), None).is_err());
    assert!(AdminClient::new(Some("127.0.0.1:9091"), Some("not a secret")).is_err());

    // The same two variables, as the binary reads them. Only this test touches
    // them, and both are cleared again before it returns.
    std::env::remove_var("RELAY_ADMIN_ADDR");
    std::env::remove_var("RELAY_ADMIN_SECRET");
    assert!(AdminClient::from_env().is_err(), "no address and no secret");
    std::env::set_var("RELAY_ADMIN_ADDR", "127.0.0.1:9091");
    assert!(
        AdminClient::from_env().is_err(),
        "an address with no secret was accepted"
    );
    std::env::set_var("RELAY_ADMIN_SECRET", &secret);
    assert!(AdminClient::from_env().is_ok());
    std::env::remove_var("RELAY_ADMIN_ADDR");
    std::env::remove_var("RELAY_ADMIN_SECRET");
}

#[tokio::test]
async fn the_cli_reaches_an_unspecified_listener_on_loopback() {
    let admin = admin().await;
    let any = format!("0.0.0.0:{}", admin.addr.port());
    let client = AdminClient::new(Some(&any), Some(&admin.secret)).unwrap();
    assert!(client.create().await.is_ok());
}

#[tokio::test]
async fn the_cli_reports_a_refused_call_with_its_status_and_no_secret() {
    let admin = admin().await;

    let missing = admin
        .client()
        .revoke(&token_id(14))
        .await
        .unwrap_err()
        .to_string();
    assert!(missing.contains("404"), "{missing}");

    let wrong = RelayAccess::random().encode();
    let addr = admin.addr.to_string();
    let refused = AdminClient::new(Some(&addr), Some(&wrong)).unwrap();
    let error = refused.unbind(&token_id(14)).await.unwrap_err().to_string();
    assert!(error.contains("401"), "{error}");
    assert!(
        !error.contains(&wrong),
        "the admin secret reached the error: {error}"
    );
}
