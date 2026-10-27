use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

#[derive(Debug, thiserror::Error)]
pub enum RelayError {
    #[error("bad request: {0}")]
    BadRequest(String),
    #[error("unauthorized")]
    Unauthorized,
    #[error("payload too large")]
    TooLarge,
    /// Unknown, or known and not yours to see — the relay does not say which.
    #[error("not found")]
    NotFound,
    /// An invite that existed and has expired or already admitted someone.
    #[error("{0}")]
    Gone(&'static str),
    #[error("{0}")]
    Conflict(&'static str),
    /// The note's live log is at its cap ([`crate::quota::Limits`]).
    #[error("note_full")]
    NoteFull,
    /// The relay is at its ceiling and will not start a new note.
    #[error("relay_full")]
    RelayFull,
    /// The topic was purged ([ADR 0026](../../docs/decisions.md#adr-0026)).
    #[error("gone")]
    TopicGone,
    /// Something this relay was not set up to do: media without a media
    /// directory.
    #[error("{0}")]
    Unavailable(&'static str),
    /// A token or invite request without a valid proof of the relay's access
    /// key ([ADR 0028](../../docs/decisions.md#adr-0028)).
    #[error("access_required")]
    AccessRequired,
    /// Past the address's write budget; try again after this many seconds.
    #[error("rate_limited")]
    RateLimited(u64),
    #[error("storage: {0}")]
    Storage(String),
}

impl IntoResponse for RelayError {
    fn into_response(self) -> Response {
        let status = match self {
            RelayError::BadRequest(_) => StatusCode::BAD_REQUEST,
            RelayError::Unauthorized => StatusCode::UNAUTHORIZED,
            RelayError::TooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            RelayError::NotFound => StatusCode::NOT_FOUND,
            RelayError::Gone(_) | RelayError::TopicGone => StatusCode::GONE,
            RelayError::Conflict(_) => StatusCode::CONFLICT,
            RelayError::NoteFull | RelayError::RelayFull => StatusCode::INSUFFICIENT_STORAGE,
            RelayError::Unavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
            RelayError::Storage(_) => StatusCode::INTERNAL_SERVER_ERROR,
            RelayError::AccessRequired => StatusCode::FORBIDDEN,
            RelayError::RateLimited(_) => StatusCode::TOO_MANY_REQUESTS,
        };
        if let RelayError::RateLimited(retry) = self {
            return (
                status,
                [(axum::http::header::RETRY_AFTER, retry.to_string())],
            )
                .into_response();
        }
        // A code the client matches on, as the `426` has: a `403` from a proxy
        // in front of the relay must not look like the relay asking for its key.
        if let RelayError::AccessRequired = self {
            return (
                status,
                axum::Json(serde_json::json!({ "code": "access_required" })),
            )
                .into_response();
        }
        // The body carries the category and never the detail: a storage error's
        // text could name a note ID, and the relay's whole job is to not leak.
        let body = match self {
            RelayError::Storage(ref detail) => {
                tracing::error!(%detail, "storage error");
                "internal error".to_string()
            }
            ref other => other.to_string(),
        };
        (status, body).into_response()
    }
}

pub type Result<T> = std::result::Result<T, RelayError>;
