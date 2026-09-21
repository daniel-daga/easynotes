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
            RelayError::Gone(_) => StatusCode::GONE,
            RelayError::Conflict(_) => StatusCode::CONFLICT,
            RelayError::Storage(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
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
