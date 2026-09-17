//! One error type for the whole core. The UI never sees a crypto-crate type.

#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("crypto: {0}")]
    Crypto(&'static str),
    #[error("malformed envelope: {0}")]
    MalformedEnvelope(&'static str),
    #[error("signature does not verify")]
    BadSignature,
    #[error("envelope is for note {got}, expected {expected}")]
    WrongNote { expected: String, got: String },
    #[error("malformed share blob: {0}")]
    MalformedShareBlob(&'static str),
    #[error("unknown note {0}")]
    UnknownNote(String),
    #[error("unknown item {0}")]
    UnknownItem(String),
    #[error("crdt: {0}")]
    Crdt(String),
    #[error("storage: {0}")]
    Storage(String),
    #[error("envelope exceeds the {limit} byte cap")]
    TooLarge { limit: usize },
}

pub type Result<T> = std::result::Result<T, CoreError>;
