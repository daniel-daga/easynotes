//! Billing's private admin API for per-device access tokens
//! ([ADR 0038](../../docs/decisions.md#adr-0038)).
//!
//! Minting, revoking, marking read-only and unbinding are the only way a token
//! comes into or leaves existence. That is billing's job, not a device's, so
//! this is a listener of its own ([`router`]), like [`crate::metrics`], and
//! never a route on the public one. Every call carries one bearer secret in
//! `RELAY_ADMIN_SECRET`; no CORS, no protocol header, no rate limiter, no
//! household.

use crate::app::AppState;
use axum::extract::{Path, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use easynotes_core::relay_access::RelayAccess;
use serde::Deserialize;
use std::sync::Arc;

/// How long a revoked row is kept before any admin call sweeps it. Long enough
/// for billing to answer "was this token revoked?" after the fact, short enough
/// that dead rows do not accumulate for ever.
const REVOKED_RETENTION_SECS: i64 = 30 * 24 * 60 * 60;

/// How long an unbound token's link stays usable. A link can leak before the
/// device that owns it claims it, so minting and unbinding both start this
/// clock; a claimed token lives until it is revoked ([ADR 0038]).
const UNBOUND_TTL_SECS: i64 = 24 * 60 * 60;

/// The admin listener's routes, behind `secret`.
pub fn router(state: Arc<AppState>, secret: RelayAccess) -> Router {
    Router::new()
        .route("/tokens", post(create))
        .route("/tokens/{id}/revoke", post(revoke))
        .route("/tokens/{id}/read-only", post(read_only))
        .route("/tokens/{id}/unbind", post(unbind))
        .layer(axum::middleware::from_fn_with_state(
            (state.clone(), secret),
            authorise,
        ))
        .with_state(state)
}

/// The bearer secret, then the best-effort sweep. A wrong or missing secret is
/// refused before the store is touched.
async fn authorise(
    State((state, secret)): State<(Arc<AppState>, RelayAccess)>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let presented = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .and_then(|raw| RelayAccess::decode(raw).ok());
    let Some(presented) = presented else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    if !same_secret(&presented, &secret) {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    let before = crate::auth::unix_now() as i64 - REVOKED_RETENTION_SECS;
    // Off the request path: housekeeping must not delay or fail a mint. A
    // previous call's sweep is enough, and a failure is only a warning.
    let store = state.store.clone();
    tokio::spawn(async move {
        if let Err(error) = store.purge_revoked_access_tokens(before).await {
            tracing::warn!(%error, "admin: could not purge revoked access tokens");
        }
    });
    next.run(request).await
}

/// Whether `presented` is the configured secret, in constant time.
///
/// [`RelayAccess`] keeps its bytes private and `==` on an array may stop at the
/// first difference, so compare through the HMAC check the relay already uses:
/// equal keys make equal proofs, and the comparison inside `verifies_token`
/// takes the same time whatever the difference.
fn same_secret(presented: &RelayAccess, expected: &RelayAccess) -> bool {
    const NONCE: [u8; 32] = [0; 32];
    const DEVICE: [u8; 32] = [0; 32];
    expected.verifies_token(&NONCE, &DEVICE, &presented.token_proof(&NONCE, &DEVICE))
}

#[derive(Deserialize)]
struct CreateRequest {
    /// The caller's ID: sixteen random bytes, base64url.
    id: String,
}

#[derive(serde::Serialize)]
struct CreateResponse {
    secret: String,
}

/// Mint a token under the ID billing chose. A retry of the same ID is safe: the
/// first secret comes back again, unless the token has been revoked since, so a
/// late retry cannot revive it.
async fn create(
    State(state): State<Arc<AppState>>,
    Json(body): Json<CreateRequest>,
) -> Result<Response, StatusCode> {
    if !is_token_id(&body.id) {
        return Err(StatusCode::BAD_REQUEST);
    }
    let mut bytes = [0u8; 32];
    easynotes_core::crypto::random_bytes(&mut bytes);
    let expires_at = crate::auth::unix_now() as i64 + UNBOUND_TTL_SECS;
    match state
        .store
        .create_access_token(&body.id, &bytes, Some(expires_at))
        .await
        .map_err(failed)?
    {
        true => Ok((
            StatusCode::CREATED,
            Json(CreateResponse {
                secret: URL_SAFE_NO_PAD.encode(bytes),
            }),
        )
            .into_response()),
        false => {
            let Some(token) = state.store.access_token(&body.id).await.map_err(failed)? else {
                // The row vanished between the insert and this read. Only the
                // sweep removes rows, and only revoked ones, so this ID was
                // revoked; a retry would mint a fresh live secret for it.
                return Err(StatusCode::CONFLICT);
            };
            if token.revoked_at.is_some() {
                return Err(StatusCode::CONFLICT);
            }
            let now = crate::auth::unix_now() as i64;
            if token.device_key.is_none() && token.expires_at.is_some_and(|at| at <= now) {
                // An unbound link past its expiry is dead. Returning its secret
                // would hand billing something no handshake accepts; a new ID
                // starts a live link.
                return Err(StatusCode::CONFLICT);
            }
            Ok((
                StatusCode::OK,
                Json(CreateResponse {
                    secret: RelayAccess::from_bytes(token.secret).encode(),
                }),
            )
                .into_response())
        }
    }
}

async fn revoke(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<StatusCode, StatusCode> {
    let now = crate::auth::unix_now() as i64;
    match state
        .store
        .revoke_access_token(&id, now)
        .await
        .map_err(failed)?
    {
        true => Ok(StatusCode::NO_CONTENT),
        false => Err(StatusCode::NOT_FOUND),
    }
}

#[derive(Deserialize)]
struct ReadOnlyRequest {
    read_only: bool,
}

async fn read_only(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(body): Json<ReadOnlyRequest>,
) -> Result<StatusCode, StatusCode> {
    if state
        .store
        .access_token(&id)
        .await
        .map_err(failed)?
        .is_none()
    {
        return Err(StatusCode::NOT_FOUND);
    }
    state
        .store
        .set_access_token_read_only(&id, body.read_only)
        .await
        .map_err(failed)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn unbind(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<StatusCode, StatusCode> {
    if state
        .store
        .access_token(&id)
        .await
        .map_err(failed)?
        .is_none()
    {
        return Err(StatusCode::NOT_FOUND);
    }
    let expires_at = crate::auth::unix_now() as i64 + UNBOUND_TTL_SECS;
    state
        .store
        .unbind_access_token(&id, expires_at)
        .await
        .map_err(failed)?;
    Ok(StatusCode::NO_CONTENT)
}

fn is_token_id(id: &str) -> bool {
    URL_SAFE_NO_PAD
        .decode(id)
        .is_ok_and(|bytes| bytes.len() == 16)
}

fn failed(error: crate::error::RelayError) -> StatusCode {
    tracing::error!(%error, "admin: the store did not answer");
    StatusCode::INTERNAL_SERVER_ERROR
}
