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
    #[error("storage: {0}")]
    Storage(String),
}

impl IntoResponse for RelayError {
    fn into_response(self) -> Response {
        let status = match self {
            RelayError::BadRequest(_) => StatusCode::BAD_REQUEST,
            RelayError::Unauthorized => StatusCode::UNAUTHORIZED,
            RelayError::TooLarge => StatusCode::PAYLOAD_TOO_LARGE,
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
