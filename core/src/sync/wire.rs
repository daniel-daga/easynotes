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

// -- one socket per device ---------------------------------------------------
//
// `/v1/sync`: the messages above with the note named in each, because the path
// no longer says which note a message is about (ADR 0017).

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum DeviceMessage<'a> {
    Subscribe { note: &'a str, since: i64 },
    Unsubscribe { note: &'a str },
    Append { note: &'a str, envelope: String },
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum DeviceReply {
    Update {
        note: String,
        seq: i64,
        envelope: String,
    },
    Caught {
        note: String,
        seq: i64,
    },
    Error {
        #[serde(default)]
        note: Option<String>,
        message: String,
    },
}

/// What arrived on a device's socket, and which note it is about.
pub enum Routed {
    Note(crate::ids::NoteId, Incoming),
    /// A refusal the relay could not pin on a note, because the message it
    /// answers could not be read far enough to say which.
    Unrouted(String),
}

/// The URL a device's one socket is opened at.
pub(crate) fn sync_url(ws_base: &str, token: &str) -> String {
    format!("{ws_base}/v1/sync?token={token}")
}

fn device(message: &DeviceMessage<'_>) -> String {
    serde_json::to_string(message).expect("a device message always serializes")
}

pub(crate) fn subscribe_note(note: crate::ids::NoteId, since: i64) -> String {
    device(&DeviceMessage::Subscribe {
        note: &note.encode(),
        since,
    })
}

pub(crate) fn unsubscribe_note(note: crate::ids::NoteId) -> String {
    device(&DeviceMessage::Unsubscribe {
        note: &note.encode(),
    })
}

pub(crate) fn append_note(note: crate::ids::NoteId, envelope: &[u8]) -> String {
    device(&DeviceMessage::Append {
        note: &note.encode(),
        envelope: URL_SAFE_NO_PAD.encode(envelope),
    })
}

/// Read one message from a device's socket.
///
/// No cursor here, unlike [`parse`]: there is one per note, and the engine
/// keeps them. An update or a `caught` for a note ID that does not parse is
/// nonsense, because this device never subscribed to one; a refusal naming one
/// is still a refusal, and is passed on unrouted rather than lost.
pub(crate) fn parse_routed(text: &str) -> Result<Routed> {
    let nonsense = |what: &str| CoreError::Storage(format!("relay sent nonsense: {what}"));
    let parsed: DeviceReply = serde_json::from_str(text).map_err(|e| nonsense(&e.to_string()))?;
    let note = |raw: &str| crate::ids::NoteId::decode(raw).map_err(|_| nonsense("a bad note id"));
    Ok(match parsed {
        DeviceReply::Update {
            note: raw,
            seq,
            envelope,
        } => {
            let envelope = URL_SAFE_NO_PAD
                .decode(&envelope)
                .map_err(|_| CoreError::Storage("relay sent a malformed envelope".into()))?;
            Routed::Note(note(&raw)?, Incoming::Envelope { seq, envelope })
        }
        DeviceReply::Caught { note: raw, seq } => {
            Routed::Note(note(&raw)?, Incoming::Caught { seq })
        }
        DeviceReply::Error { note: raw, message } => {
            match raw.and_then(|raw| crate::ids::NoteId::decode(&raw).ok()) {
                Some(note) => Routed::Note(note, Incoming::Rejected(message)),
                None => Routed::Unrouted(message),
            }
        }
    })
}

// -- invites -----------------------------------------------------------------
//
// The mirror of `relay::protocol`'s invite messages. Kept here rather than
// shared with the relay for the reason everything else in this file is: the
// relay builds the core without its sync client, and the two ends agreeing is
// what `relay/tests/invites.rs` checks by driving one against the other.

#[derive(Serialize)]
pub(crate) struct CreateInvite<'a> {
    pub invite: &'a str,
    pub token_hash: &'a str,
    pub owner_hash: &'a str,
    pub expires_in: u64,
}

#[derive(Deserialize)]
pub(crate) struct CreatedInvite {
    pub expires_in: u64,
}

#[derive(Serialize)]
pub(crate) struct JoinInvite<'a> {
    pub token: &'a str,
    pub payload: &'a str,
}

#[derive(Deserialize)]
pub(crate) struct JoinedInvite {
    pub request: String,
}

#[derive(Serialize)]
pub(crate) struct PollInvite<'a> {
    pub token: &'a str,
}

/// Where a join request stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestState {
    Pending,
    Approved,
    Declined,
}

#[derive(Deserialize)]
pub(crate) struct PolledRequest {
    pub state: RequestState,
    pub grant: Option<String>,
    pub expires_in: u64,
}

#[derive(Serialize)]
pub(crate) struct OwnInvite<'a> {
    pub owner: &'a str,
}

#[derive(Deserialize)]
pub(crate) struct InviteListing {
    pub open: bool,
    pub expires_in: u64,
    pub requests: Vec<ListedRequest>,
}

#[derive(Deserialize)]
pub(crate) struct ListedRequest {
    pub request: String,
    pub payload: String,
    pub state: RequestState,
}

#[derive(Serialize)]
pub(crate) struct AnswerInvite<'a> {
    pub owner: &'a str,
    pub grant: Option<&'a str>,
}

/// Turn an invite call's status into an answer a person can be shown.
///
/// The relay gives a wrong secret and a missing invite the same 404 on
/// purpose, so "does not exist" here covers a mistyped link as well as a
/// withdrawn one — which is also the most a guest should be told.
pub(crate) fn invite_status(status: u16, body: &str) -> Result<()> {
    let said = |fallback: &str| {
        let body = body.trim();
        if body.is_empty() {
            fallback.to_string()
        } else {
            body.to_string()
        }
    };
    match status {
        200..=299 => Ok(()),
        404 => Err(CoreError::Invite(
            "This invite does not exist or was withdrawn.".into(),
        )),
        410 => Err(CoreError::Invite(
            "This invite has expired or has already been used.".into(),
        )),
        409 => Err(CoreError::Invite(said("That was already answered."))),
        413 => Err(CoreError::Invite("That is too large to send.".into())),
        429 => Err(CoreError::Invite(
            "The relay is busy. Try again in a moment.".into(),
        )),
        other => Err(net(format!("{other} {}", said("")))),
    }
}

pub(crate) fn invite_url(http_base: &str, invite: crate::ids::InviteId, rest: &str) -> String {
    format!("{http_base}/v1/invites/{}{rest}", invite.encode())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_invites_answers_are_words_and_not_codes() {
        assert!(invite_status(201, "").is_ok());
        assert!(invite_status(204, "").is_ok());
        for status in [404, 410, 409, 429] {
            match invite_status(status, "") {
                Err(CoreError::Invite(words)) => assert!(!words.is_empty()),
                other => panic!("{status} gave {other:?}"),
            }
        }
        // Anything else is the network's problem, not the invite's.
        assert!(matches!(
            invite_status(500, "internal error"),
            Err(CoreError::Storage(_))
        ));
    }

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
    fn a_device_says_which_note_every_time() {
        let note = crate::ids::NoteId::from_bytes([0u8; 16]);
        assert_eq!(
            subscribe_note(note, 4),
            r#"{"type":"subscribe","note":"AAAAAAAAAAAAAAAAAAAAAA","since":4}"#
        );
        assert_eq!(
            unsubscribe_note(note),
            r#"{"type":"unsubscribe","note":"AAAAAAAAAAAAAAAAAAAAAA"}"#
        );
        assert_eq!(
            append_note(note, b"hi"),
            r#"{"type":"append","note":"AAAAAAAAAAAAAAAAAAAAAA","envelope":"aGk"}"#
        );
        assert_eq!(sync_url("ws://r", "t0k"), "ws://r/v1/sync?token=t0k");
    }

    #[test]
    fn what_a_device_hears_comes_back_with_its_note() {
        let note = crate::ids::NoteId::from_bytes([0u8; 16]);
        let update =
            r#"{"type":"update","note":"AAAAAAAAAAAAAAAAAAAAAA","seq":7,"envelope":"aGk"}"#;
        match parse_routed(update).unwrap() {
            Routed::Note(n, Incoming::Envelope { seq, envelope }) => {
                assert_eq!(n, note);
                assert_eq!(seq, 7);
                assert_eq!(envelope, b"hi");
            }
            _ => panic!("expected an envelope for the note"),
        }
        match parse_routed(r#"{"type":"caught","note":"AAAAAAAAAAAAAAAAAAAAAA","seq":3}"#).unwrap()
        {
            Routed::Note(n, Incoming::Caught { seq: 3 }) => assert_eq!(n, note),
            _ => panic!("expected caught for the note"),
        }
        // A refusal that names no note, or one that does not parse, still
        // reaches somebody.
        for text in [
            r#"{"type":"error","message":"malformed message"}"#,
            r#"{"type":"error","note":"??","message":"malformed note id"}"#,
        ] {
            assert!(matches!(parse_routed(text).unwrap(), Routed::Unrouted(_)));
        }
        // An update for a note that cannot exist is the relay talking nonsense.
        assert!(parse_routed(r#"{"type":"update","note":"??","seq":1,"envelope":"aGk"}"#).is_err());
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
