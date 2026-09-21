//! The relay's wire protocol.
//!
//! Everything here is deliberately small. The relay stores and forwards opaque
//! blobs; it has no idea what a note, an item or a checkbox is, and it must never
//! learn. The only fields it reads out of an envelope are the ones it needs to
//! route and to refuse abuse.

use serde::{Deserialize, Serialize};

/// Mirrors `easynotes_core::crypto::MAX_ENVELOPE_BYTES`; `caps_match_the_core`
/// in `tests/protocol.rs` fails if the two ever drift.
pub const MAX_ENVELOPE_BYTES: usize = 1 << 20;

/// How long a challenge nonce stays redeemable.
pub const CHALLENGE_TTL_SECS: u64 = 60;

/// How long an issued token is good for.
pub const TOKEN_TTL_SECS: u64 = 15 * 60;

#[derive(Debug, Deserialize)]
pub struct ChallengeRequest {
    /// The device's Ed25519 public key, base64url, no padding.
    pub device: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ChallengeResponse {
    /// 32 random bytes, base64url.
    pub nonce: String,
    pub expires_in: u64,
}

#[derive(Debug, Deserialize)]
pub struct TokenRequest {
    pub device: String,
    pub nonce: String,
    /// Ed25519 signature over the raw nonce bytes, base64url.
    pub signature: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TokenResponse {
    pub token: String,
    pub expires_in: u64,
}

/// Client to server, over the note's WebSocket.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    /// Ask for everything after `since`, then stream. `since: 0` means "from the
    /// beginning"; sequence numbers start at 1.
    Subscribe { since: i64 },
    /// Hand the relay an envelope to append and fan out.
    Append {
        /// The encoded envelope, base64url.
        envelope: String,
    },
}

/// Server to client.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    /// One stored envelope. Sent for the backlog on subscribe and for every
    /// subsequent append, including the subscriber's own — clients deduplicate by
    /// `(author, counter)` regardless, and it keeps `seq` tracking uniform.
    Update { seq: i64, envelope: String },
    /// The backlog is drained; everything after this is live.
    Caught { seq: i64 },
    /// The client cursor is beyond this relay's log. This happens when the
    /// relay has been restored from an earlier backup; the client must reset
    /// its local cursor and resubscribe from the beginning.
    Reset,
    /// The append was rejected. The connection stays open.
    Error { message: String },
}

// ---------------------------------------------------------------------------
// One socket per device
// ---------------------------------------------------------------------------
//
// `/v1/sync` carries every note a device syncs over one socket, authenticated
// once. The messages are the per-note ones above with the note named in each,
// because the path no longer says which note a message is about. A note can be
// subscribed at any point in the socket's life, not only first.

/// How many notes one socket may be subscribed to at once. A device's notes,
/// not an enumeration of the relay.
pub const MAX_SUBSCRIPTIONS: usize = 4096;

/// How many stored envelopes of one note's backlog go out before the relay
/// looks at anything else on the socket. Small enough that a guest joining a
/// list with months of history does not hold up live edits to every other note
/// behind it; large enough that catching up is not a round trip per envelope.
pub const BACKLOG_CHUNK: usize = 64;

/// Client to server, over `/v1/sync`.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SyncClientMessage {
    /// Start sending this note: everything after `since`, then live.
    Subscribe { note: String, since: i64 },
    /// Stop sending this note.
    Unsubscribe { note: String },
    /// An envelope to append to a note this socket is subscribed to.
    Append { note: String, envelope: String },
}

/// Server to client, over `/v1/sync`.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SyncServerMessage {
    Update {
        note: String,
        seq: i64,
        envelope: String,
    },
    /// This note's backlog is drained. Per note, so a client can tell which
    /// notes are live while another is still catching up.
    Caught { note: String, seq: i64 },
    /// This note's cursor is beyond the relay's current log. The client must
    /// reset its local cursor and resubscribe from the beginning.
    Reset { note: String },
    /// Something was refused. `note` is absent when the message it answers
    /// could not be read far enough to say which note it was about.
    Error {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
        message: String,
    },
}

// ---------------------------------------------------------------------------
// Invites
// ---------------------------------------------------------------------------
//
// An invite is a mailbox with a lifetime. The owner opens one, guests leave
// sealed join requests in it, and the owner answers one of them with a sealed
// grant. The relay stores all of it and can read none of it: the requests are
// sealed under a key derived from the link's secret, the grant under a key only
// the guest who asked can derive, and neither side authenticates with a device
// key — so the relay learns no new edge between two devices either
// (ADR 0015).
//
// Every call is a JSON `POST`, the reads included, so that no secret is ever in
// a URL, where proxies and access logs would keep it.

/// How long an invite lasts when the owner does not say.
pub const INVITE_DEFAULT_TTL_SECS: u64 = 7 * 24 * 60 * 60;

/// The longest an invite may last. A link is only as safe as the chat it was
/// pasted into, and a month is already generous for "come and join our list".
pub const INVITE_MAX_TTL_SECS: u64 = 30 * 24 * 60 * 60;

/// How long an expired invite is kept so that a guest who opens a stale link
/// is told it expired, rather than that it never existed. Past this it is swept.
pub const INVITE_GRACE_SECS: i64 = 24 * 60 * 60;

/// How many join requests one invite will hold, answered or not. A household
/// needs one; the cap is what stops a leaked link turning into a storage bill.
pub const MAX_INVITE_REQUESTS: usize = 16;

/// A sealed join request: an X25519 key, a short name, and AEAD overhead.
pub const MAX_INVITE_PAYLOAD_BYTES: usize = 1024;

/// A sealed grant: an ephemeral key, a note ID and a note key, and overhead.
pub const MAX_INVITE_GRANT_BYTES: usize = 1024;

#[derive(Debug, Serialize, Deserialize)]
pub struct CreateInvite {
    /// Sixteen bytes derived from the link's secret, base64url — the same shape
    /// as a note ID and, like one, meaningless to the relay.
    pub invite: String,
    /// SHA-256 of the join token, base64url. The relay keeps the hash, so a copy
    /// of its database cannot be used to join.
    pub token_hash: String,
    /// SHA-256 of the owner's secret, base64url. The owner's secret is random
    /// and never in the link, so a guest cannot answer their own request.
    pub owner_hash: String,
    /// Seconds from now. Clamped to [`INVITE_MAX_TTL_SECS`].
    pub expires_in: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CreatedInvite {
    /// What the relay actually granted, after clamping.
    pub expires_in: u64,
}

/// A guest asking to join. Proves it holds the link by presenting the token.
#[derive(Debug, Serialize, Deserialize)]
pub struct JoinInvite {
    pub token: String,
    /// The sealed request, base64url.
    pub payload: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct JoinedInvite {
    /// The relay's name for this request, for polling it and for answering it.
    pub request: String,
}

/// A guest asking how its request went.
#[derive(Debug, Serialize, Deserialize)]
pub struct PollInvite {
    pub token: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestState {
    Pending,
    Approved,
    Declined,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PolledRequest {
    pub state: RequestState,
    /// The sealed grant, base64url, once the owner has approved.
    pub grant: Option<String>,
    /// Seconds until the invite expires; 0 once it has.
    pub expires_in: u64,
}

/// The owner proving it is the owner.
#[derive(Debug, Serialize, Deserialize)]
pub struct OwnInvite {
    pub owner: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct InviteListing {
    /// False once a request has been approved or the invite has expired.
    pub open: bool,
    pub expires_in: u64,
    pub requests: Vec<ListedRequest>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ListedRequest {
    pub request: String,
    /// The sealed request, base64url.
    pub payload: String,
    pub state: RequestState,
}

/// The owner's answer. A grant approves; no grant declines.
#[derive(Debug, Serialize, Deserialize)]
pub struct AnswerInvite {
    pub owner: String,
    pub grant: Option<String>,
}
