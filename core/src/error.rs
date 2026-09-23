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
    #[error("malformed recovery phrase: {0}")]
    MalformedPhrase(&'static str),
    /// Asking for the account when there is not one yet. A device has notes
    /// before it has an account and that is the ordinary case, not a fault.
    #[error("this device has no account yet")]
    NoAccount,
    #[error("that is not an invite link: {0}")]
    MalformedInvite(&'static str),
    /// The relay's answer about an invite, in words a person can be shown:
    /// expired, already used, full, withdrawn.
    #[error("{0}")]
    Invite(String),
    #[error("malformed share blob: {0}")]
    MalformedShareBlob(&'static str),
    #[error("unknown note {0}")]
    UnknownNote(String),
    /// A rotated owner note is waiting for the snapshot matching its account
    /// register. The envelope was safely skipped or retained, so sync can move
    /// its cursor forward and continue waiting for the matching snapshot.
    #[error("owner note rotation is waiting for its matching snapshot")]
    RotationPending,
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
