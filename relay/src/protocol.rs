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
    /// The append was rejected. The connection stays open.
    Error { message: String },
}
