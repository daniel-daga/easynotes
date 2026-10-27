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
    /// A shared note this device cannot read yet: no package has granted it a
    /// key. The envelope was verified and kept, so sync can move its cursor on
    /// and the note opens when its grant arrives.
    #[error("waiting for access to this note")]
    AccessPending,
    #[error("sharing needs a name: set one up first")]
    NoProfileName,
    #[error("unknown item {0}")]
    UnknownItem(String),
    #[error("crdt: {0}")]
    Crdt(String),
    #[error("storage: {0}")]
    Storage(String),
    /// A relay supplied an HTTP `Retry-After` value for a transient refusal.
    #[error("the sync server rate limited this request; retry after {0:?}")]
    RetryAfter(std::time::Duration),
    /// An attachment refused, in words a person can be shown: too big, not a
    /// format this release takes, too many on one note.
    #[error("{0}")]
    Attachment(String),
    /// The relay no longer speaks the protocol this build does (ADR 0029).
    /// Nothing local is affected: the app keeps working and stops syncing
    /// until it is updated.
    #[error("This sync server needs a newer version of EasyNotes. Update the app to sync again; your notes are safe on this device.")]
    AppOutdated,
    /// The relay is older than anything this build still speaks. Not the
    /// person's to fix with a button: whoever runs the relay has to update it.
    #[error("This sync server runs an older version of EasyNotes than this app can sync with. Ask whoever runs it to update it; your notes are safe on this device.")]
    RelayOutdated,
    /// The relay lets in only the people its operator gave its access key
    /// to, and this device has not got it, or has an old one ([ADR 0028](../../docs/decisions.md#adr-0028)). The person
    /// fixes it in Settings, with the relay link they were given.
    #[error("This sync server needs an access key. Ask whoever runs it for its server link, and add it under Settings, Sync server. Your notes are safe on this device.")]
    AccessRequired,
    #[error("envelope exceeds the {limit} byte cap")]
    TooLarge { limit: usize },
}

pub type Result<T> = std::result::Result<T, CoreError>;
