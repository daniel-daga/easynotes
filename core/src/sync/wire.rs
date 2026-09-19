//! What goes over the socket, independent of what the socket is.
//!
//! Two transports carry this — `tokio-tungstenite` on a device and the browser's
//! own `WebSocket` in a tab — and the messages, the base64, the `last_seq`
//! bookkeeping and the rules about what counts as nonsense are the same in both.
//! Writing them twice would mean two chances to disagree with the relay, and the
//! disagreement would show up as a sync bug on one platform only.
//!
//! So this module has no transport in it at all: strings in, strings out. It is
//! also the only part of the sync client `cargo test` can reach without a
//! browser or a socket, which is the other reason to put everything with a
//! decision in it here.

use crate::error::{CoreError, Result};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde::{Deserialize, Serialize};

#[derive(Serialize)]
pub(crate) struct ChallengeRequest<'a> {
    pub device: &'a str,
}

#[derive(Deserialize)]
pub(crate) struct ChallengeResponse {
    pub nonce: String,
}

#[derive(Serialize)]
pub(crate) struct TokenRequest<'a> {
    pub device: &'a str,
    pub nonce: &'a str,
    pub signature: &'a str,
}

#[derive(Deserialize)]
pub(crate) struct TokenResponse {
    pub token: String,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ClientMessage {
    Subscribe { since: i64 },
    Append { envelope: String },
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ServerMessage {
    Update { seq: i64, envelope: String },
    Caught { seq: i64 },
    Error { message: String },
}

/// What arrived from the relay.
pub enum Incoming {
    /// An envelope to hand to `NoteCore::apply_remote`.
    Envelope { seq: i64, envelope: Vec<u8> },
    /// The backlog is drained; everything after this is live.
    Caught { seq: i64 },
    /// The relay refused something. Not fatal — the connection stays up.
    Rejected(String),
}

pub(crate) fn net(e: impl std::fmt::Display) -> CoreError {
    CoreError::Storage(format!("relay: {e}"))
}

/// The first thing a connection says: replay everything after `since`.
pub(crate) fn subscribe(since: i64) -> String {
    serde_json::to_string(&ClientMessage::Subscribe { since })
        .expect("the subscribe message always serializes")
}

pub(crate) fn append(envelope: &[u8]) -> String {
    serde_json::to_string(&ClientMessage::Append {
        envelope: URL_SAFE_NO_PAD.encode(envelope),
    })
    .expect("the append message always serializes")
}

/// Read one message from the relay, advancing `last_seq`.
///
/// `last_seq` only ever moves forward — `max` rather than assignment — because
/// it is what the next `subscribe` replays from, and a relay that sent an older
/// sequence number, by confusion or on purpose, would otherwise make this
/// device ask for a backlog it has already applied.
pub(crate) fn parse(text: &str, last_seq: &mut i64) -> Result<Incoming> {
    let parsed: ServerMessage = serde_json::from_str(text)
        .map_err(|e| CoreError::Storage(format!("relay sent nonsense: {e}")))?;
    Ok(match parsed {
        ServerMessage::Update { seq, envelope } => {
            let raw = URL_SAFE_NO_PAD
                .decode(&envelope)
                .map_err(|_| CoreError::Storage("relay sent a malformed envelope".into()))?;
            *last_seq = (*last_seq).max(seq);
            Incoming::Envelope { seq, envelope: raw }
        }
        ServerMessage::Caught { seq } => {
            *last_seq = (*last_seq).max(seq);
            Incoming::Caught { seq }
        }
        ServerMessage::Error { message } => Incoming::Rejected(message),
    })
}

/// The URL one note's socket is opened at.
pub(crate) fn note_url(ws_base: &str, note: crate::ids::NoteId, token: &str) -> String {
    format!("{ws_base}/v1/notes/{}?token={token}", note.encode())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_update_comes_back_decoded_and_moves_the_cursor() {
        let mut seq = 0;
        let text = r#"{"type":"update","seq":7,"envelope":"aGk"}"#;
        match parse(text, &mut seq).unwrap() {
            Incoming::Envelope { seq, envelope } => {
                assert_eq!(seq, 7);
                assert_eq!(envelope, b"hi");
            }
            _ => panic!("expected an envelope"),
        }
        assert_eq!(seq, 7);
    }

    #[test]
    fn the_cursor_never_goes_backwards() {
        // A relay that replayed an older sequence number would otherwise make
        // the next `subscribe` ask for a backlog this device already has.
        let mut seq = 9;
        parse(r#"{"type":"update","seq":3,"envelope":"aGk"}"#, &mut seq).unwrap();
        assert_eq!(seq, 9);
        parse(r#"{"type":"caught","seq":4}"#, &mut seq).unwrap();
        assert_eq!(seq, 9);
    }

    #[test]
    fn a_rejection_is_not_an_error() {
        // The relay refused one envelope; the connection is fine and the rest of
        // the outbox still goes.
        let mut seq = 0;
        match parse(r#"{"type":"error","message":"too big"}"#, &mut seq).unwrap() {
            Incoming::Rejected(m) => assert_eq!(m, "too big"),
            _ => panic!("expected a rejection"),
        }
    }

    #[test]
    fn nonsense_is_an_error_rather_than_a_guess() {
        let mut seq = 0;
        assert!(parse("{}", &mut seq).is_err());
        assert!(parse("not json at all", &mut seq).is_err());
        assert!(parse(r#"{"type":"update","seq":1,"envelope":"!!!"}"#, &mut seq).is_err());
    }

    #[test]
    fn what_we_send_is_what_the_relay_reads() {
        assert_eq!(subscribe(4), r#"{"type":"subscribe","since":4}"#);
        assert_eq!(append(b"hi"), r#"{"type":"append","envelope":"aGk"}"#);
    }

    #[test]
    fn a_notes_url_carries_the_token() {
        let note = crate::ids::NoteId::from_bytes([0u8; 16]);
        assert_eq!(
            note_url("ws://r", note, "t0k"),
            "ws://r/v1/notes/AAAAAAAAAAAAAAAAAAAAAA?token=t0k"
        );
    }
}
