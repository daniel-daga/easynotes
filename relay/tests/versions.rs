//! The protocol version ([ADR 0029](../../docs/decisions.md#adr-0029)
//!–): every request names the protocol it speaks, a relay refuses one it
//! does not speak with a `426` a client can read, and the client turns that
//! into the right one of two things to tell a person.

use easynotes_core::crypto::DeviceIdentity;
use easynotes_relay::limit::RateLimit;
use easynotes_relay::protocol::{PROTOCOL_MAX, PROTOCOL_MIN};
use easynotes_relay::{MemoryStore, Relay, RelayConfig};
use std::sync::Arc;

async fn relay_with(config: RelayConfig) -> Relay {
    Relay::serve_with(
        Arc::new(MemoryStore::new()),
        "127.0.0.1:0".parse().unwrap(),
        config,
    )
    .await
    .unwrap()
}

async fn relay() -> Relay {
    relay_with(RelayConfig::default()).await
}

fn challenge_body() -> serde_json::Value {
    serde_json::json!({ "device": DeviceIdentity::generate().public_key().encode() })
}

/// No protocol, one below the range and one above it: each is refused before
/// the handler runs, with the range the relay does speak.
#[tokio::test]
async fn a_protocol_the_relay_does_not_speak_is_told_to_update() {
    let relay = relay().await;
    let http = reqwest::Client::new();
    for query in [
        String::new(),
        "?p=".into(),
        "?p=two".into(),
        format!("?p={}", PROTOCOL_MIN - 1),
        format!("?p={}", PROTOCOL_MAX + 1),
    ] {
        let response = http
            .post(format!("{}/v1/challenge{query}", relay.http_base()))
            .json(&challenge_body())
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 426, "{query:?} was not refused");
        let body: serde_json::Value = response.json().await.unwrap();
        assert_eq!(body["code"], "update_app");
        assert_eq!(body["protocol"]["min"], PROTOCOL_MIN);
        assert_eq!(body["protocol"]["max"], PROTOCOL_MAX);
    }

    // The socket too, before it is upgraded or its token is looked at.
    let socket = http
        .get(format!("{}/v1/sync?token=nonsense", relay.http_base()))
        .send()
        .await
        .unwrap();
    assert_eq!(socket.status(), 426);

    // And every protocol it does speak goes through to the handler.
    for p in PROTOCOL_MIN..=PROTOCOL_MAX {
        let response = http
            .post(format!("{}/v1/challenge?p={p}", relay.http_base()))
            .json(&challenge_body())
            .send()
            .await
            .unwrap();
        assert!(response.status().is_success(), "protocol {p} was refused");
    }
}

/// Health and readiness are for load balancers and monitoring, which name no
/// protocol; readiness is also where a client can read the range.
#[tokio::test]
async fn health_and_readiness_need_no_protocol_and_ready_names_the_range() {
    let relay = relay().await;
    let health = reqwest::get(format!("{}/v1/health", relay.http_base()))
        .await
        .unwrap();
    assert!(health.status().is_success());
    let ready = reqwest::get(format!("{}/v1/ready", relay.http_base()))
        .await
        .unwrap();
    assert!(ready.status().is_success());
    let body: serde_json::Value = ready.json().await.unwrap();
    assert_eq!(
        body["protocol"],
        serde_json::json!({ "min": PROTOCOL_MIN, "max": PROTOCOL_MAX })
    );
}

/// A client the relay cannot talk to is told so without spending the rate
/// limit that its own and everyone else's handshakes come out of.
#[tokio::test]
async fn a_refused_protocol_costs_no_rate_limit() {
    let relay = relay_with(RelayConfig {
        handshake: RateLimit::new(60, 2),
        ..RelayConfig::default()
    })
    .await;
    let http = reqwest::Client::new();
    for _ in 0..5 {
        let response = http
            .post(format!("{}/v1/challenge", relay.http_base()))
            .json(&challenge_body())
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 426);
    }
    // A challenge and a token: exactly the burst of two, still unspent.
    easynotes_core::sync::authenticate(&relay.http_base(), None, &DeviceIdentity::generate())
        .await
        .expect("the refusals spent the handshake budget");
}

/// ADR 0029 for HTTP: a newer client may send a field an older relay does
/// not know, and the older relay carries on.
#[tokio::test]
async fn a_request_field_the_relay_does_not_know_is_ignored() {
    let relay = relay().await;
    let mut body = challenge_body();
    body["something_new"] = serde_json::json!({ "from": "a later release" });
    let response = reqwest::Client::new()
        .post(format!(
            "{}/v1/challenge?p={PROTOCOL_MAX}",
            relay.http_base()
        ))
        .json(&body)
        .send()
        .await
        .unwrap();
    assert!(
        response.status().is_success(),
        "an unknown field was refused: {}",
        response.status()
    );
}

/// The refusals and who speaks what are both counted, so that an operator can
/// see when a protocol version stops being used before dropping it.
#[tokio::test]
async fn metrics_count_protocols_spoken_and_refused() {
    let relay = relay().await;
    let http = reqwest::Client::new();
    http.post(format!("{}/v1/challenge", relay.http_base()))
        .json(&challenge_body())
        .send()
        .await
        .unwrap();
    easynotes_core::sync::authenticate(&relay.http_base(), None, &DeviceIdentity::generate())
        .await
        .unwrap();
    let text = easynotes_relay::metrics::render(&relay.state).await;
    let value = |series: &str| {
        text.lines().find_map(|line| {
            line.strip_prefix(series)?
                .strip_prefix(' ')?
                .parse::<u64>()
                .ok()
        })
    };
    assert_eq!(value("easynotes_relay_outdated_total"), Some(1));
    assert_eq!(
        value(&format!(
            "easynotes_relay_protocol_requests_total{{protocol=\"{}\"}}",
            easynotes_core::sync::PROTOCOL
        )),
        Some(2),
        "a challenge and a token"
    );
}

/// What a person is told, from the client's side, for each of the two ways a
/// relay can refuse. A stand-in relay, since a real one always speaks this
/// build's protocol.
#[tokio::test]
async fn the_client_says_whose_update_it_is() {
    use axum::{http::StatusCode, routing::post, Json, Router};

    async fn stand_in(min: u32, max: u32) -> String {
        let app = Router::new().route(
            "/v1/challenge",
            post(move || async move {
                (
                    StatusCode::UPGRADE_REQUIRED,
                    Json(serde_json::json!({
                        "code": "update_app",
                        "protocol": { "min": min, "max": max },
                    })),
                )
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await });
        format!("http://{addr}")
    }

    let spoken = easynotes_core::sync::PROTOCOL;
    let device = DeviceIdentity::generate();

    let ahead = stand_in(spoken + 1, spoken + 1).await;
    let error = easynotes_core::sync::authenticate(&ahead, None, &device)
        .await
        .unwrap_err();
    assert!(
        matches!(error, easynotes_core::error::CoreError::AppOutdated),
        "{error}"
    );
    assert!(error.to_string().contains("Update the app"), "{error}");

    let behind = stand_in(0, spoken - 1).await;
    let error = easynotes_core::sync::authenticate(&behind, None, &device)
        .await
        .unwrap_err();
    assert!(
        matches!(error, easynotes_core::error::CoreError::RelayOutdated),
        "{error}"
    );
    assert!(error.to_string().contains("whoever runs it"), "{error}");
}
