//! The page at the relay's root: the landing end of a shared https invite.
//!
//! An invite used to travel as `easynotes://join#…`, which messengers do not
//! linkify. It now travels as `<relay>/#join#…`, and this page turns the one
//! tap into the custom scheme the installed app already handles. The invite
//! sits in the fragment, which the browser never sends here, so the relay
//! learns nothing about who was invited.
//!
//! It is a fixed page with no interpolation: nothing from the request reaches
//! the response, and the script reads only `location.hash`.

use axum::http::header;
use axum::response::{Html, IntoResponse, Response};

const PAGE: &str = include_str!("join.html");

/// `GET /`: the join page, with the headers a page that handles a secret
/// should have. It is deliberately outside the `/v1` limiter and protocol
/// check: a guest who knows no relay protocol still has to be able to open it.
pub async fn page() -> Response {
    (
        [
            (
                header::CONTENT_SECURITY_POLICY,
                "default-src 'none'; script-src 'unsafe-inline'; \
                 style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'; \
                 frame-ancestors 'none'",
            ),
            (header::REFERRER_POLICY, "no-referrer"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        Html(PAGE),
    )
        .into_response()
}
