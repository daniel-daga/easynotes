//! The page at the relay's root: the https landing end of a shared invite.
//!
//! The invite itself never reaches the relay, so these tests check the page
//! and its headers, not a join. The app tests cover the link format.

use easynotes_relay::{MemoryStore, Relay};
use std::sync::Arc;

async fn relay() -> Relay {
    Relay::serve(Arc::new(MemoryStore::new()), "127.0.0.1:0".parse().unwrap())
        .await
        .unwrap()
}

/// The root is the page a shared link opens, with the headers that keep the
/// fragment where it was: no referrer, no caching, no scripts from anywhere,
/// and no framing.
#[tokio::test]
async fn the_root_serves_the_join_page() {
    let relay = relay().await;
    let response = reqwest::get(format!("{}/", relay.http_base()))
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(
        response.headers()["content-type"],
        "text/html; charset=utf-8"
    );
    assert_eq!(response.headers()["referrer-policy"], "no-referrer");
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert_eq!(response.headers()["x-content-type-options"], "nosniff");
    let csp = response.headers()["content-security-policy"]
        .to_str()
        .unwrap();
    assert!(csp.contains("default-src 'none'"));
    assert!(csp.contains("frame-ancestors 'none'"));

    let body = response.text().await.unwrap();
    assert!(body.contains("Join a shared note"));
    assert!(body.contains("This link adds you to the note. It works once, for one person."));
    assert!(body.contains("Open in EasyNotes"));
    assert!(!body.to_lowercase().contains("encrypt"));
    assert!(!body.to_lowercase().contains("ciphertext"));
    assert!(!body.contains("access key"));
    // The page turns the fragment into the scheme the app handles; it does not
    // hardcode a relay or an app-store address.
    assert!(body.contains("easynotes://join#"));
}

/// A page load with a query or fragment is the same page: nothing from the
/// request is reflected into it.
#[tokio::test]
async fn the_page_never_reflects_the_request() {
    let relay = relay().await;
    let body = reqwest::get(format!(
        "{}/?join=%3Cscript%3Ealert(1)%3C/script%3E",
        relay.http_base()
    ))
    .await
    .unwrap()
    .text()
    .await
    .unwrap();
    assert!(!body.contains("<script>alert(1)</script>"));
}

/// The API is still where it was: adding the page did not shadow `/v1`.
#[tokio::test]
async fn the_health_check_still_answers() {
    let relay = relay().await;
    let response = reqwest::get(format!("{}/v1/health", relay.http_base()))
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
}
