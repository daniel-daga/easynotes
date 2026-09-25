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

/// Decode and validate the relay's fixed-size challenge before a signer sees it.
pub(crate) fn decode_challenge_nonce(
    encoded: &str,
) -> Result<[u8; crate::crypto::RELAY_AUTH_NONCE_LEN]> {
    let decoded = URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| CoreError::Storage("relay sent a malformed nonce".into()))?;
    decoded
        .try_into()
        .map_err(|_| CoreError::Storage("relay sent a nonce of the wrong length".into()))
}

#[cfg(test)]
mod challenge_tests {
    use super::*;

    #[test]
    fn challenge_decoding_requires_exactly_thirty_two_bytes() {
        for length in [0, 31, 33, crate::crypto::ENVELOPE_HEADER_LEN] {
            let encoded = URL_SAFE_NO_PAD.encode(vec![0u8; length]);
            assert!(decode_challenge_nonce(&encoded).is_err());
        }
        let encoded = URL_SAFE_NO_PAD.encode([0u8; crate::crypto::RELAY_AUTH_NONCE_LEN]);
        assert_eq!(
            decode_challenge_nonce(&encoded).unwrap(),
            [0u8; crate::crypto::RELAY_AUTH_NONCE_LEN]
        );
        assert!(decode_challenge_nonce("not_base64!").is_err());
    }
}

/// What arrived from the relay.
pub enum Incoming {
    /// An envelope to hand to `NoteCore::apply_remote`.
    Envelope { seq: i64, envelope: Vec<u8> },
    /// The backlog is drained; everything after this is live.
    Caught { seq: i64 },
    /// The relay was restored behind this device's cursor. Reset the local
    /// cursor and acknowledgement watermark, then subscribe again from zero.
    Reset,
    /// The relay refused something. Not fatal — the connection stays up.
    Rejected(String),
    /// The note's relay log has grown enough to compact. Said only to the
    /// device whose append crossed the line ([ADR
    /// 0024](../../../docs/adr/0024-relay-storage-quota.md)).
    Crowded,
    /// The relay would not take this device's base. Nothing is lost: the log
    /// it would have replaced is still there.
    CompactRefused(String),
}

/// What a person is told about a refusal the relay names with a code.
fn said(message: String) -> String {
    match message.as_str() {
        "note_full" => "This note has grown too large for the relay to keep more of it. \
                        Your changes are safe on this device."
            .into(),
        "relay_full" => "The relay is not accepting new notes right now. \
                         This one is safe on this device."
            .into(),
        _ => message,
    }
}

pub(crate) fn net(e: impl std::fmt::Display) -> CoreError {
    CoreError::Storage(format!("relay: {e}"))
}

// -- the device socket -------------------------------------------------------
//
// `/v1/sync`: one socket for every note a device syncs, so every message names
// its note (ADR 0017).

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum DeviceMessage<'a> {
    Subscribe {
        note: &'a str,
        since: i64,
    },
    Unsubscribe {
        note: &'a str,
    },
    Append {
        note: &'a str,
        envelope: String,
    },
    Compact {
        note: &'a str,
        covers: i64,
        base: String,
        key: String,
        signature: String,
    },
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
    Reset {
        note: String,
    },
    Crowded {
        note: String,
    },
    CompactRefused {
        note: String,
        reason: String,
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

/// Hand the relay a base for a note — see [`crate::compaction`].
pub(crate) fn compact_note(
    note: crate::ids::NoteId,
    request: &crate::compaction::Request,
) -> String {
    device(&DeviceMessage::Compact {
        note: &note.encode(),
        covers: request.covers,
        base: URL_SAFE_NO_PAD.encode(&request.base),
        key: URL_SAFE_NO_PAD.encode(request.key.0),
        signature: URL_SAFE_NO_PAD.encode(request.signature),
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
        DeviceReply::Reset { note: raw } => Routed::Note(note(&raw)?, Incoming::Reset),
        DeviceReply::Crowded { note: raw } => Routed::Note(note(&raw)?, Incoming::Crowded),
        DeviceReply::CompactRefused { note: raw, reason } => {
            Routed::Note(note(&raw)?, Incoming::CompactRefused(reason))
        }
        DeviceReply::Error { note: raw, message } => {
            let message = said(message);
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
    pub request: &'a str,
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
    fn a_rejection_is_not_an_error() {
        // The relay refused one envelope; the socket is fine and the rest of
        // the outbox still goes.
        let text = r#"{"type":"error","note":"AAAAAAAAAAAAAAAAAAAAAA","message":"too big"}"#;
        match parse_routed(text).unwrap() {
            Routed::Note(_, Incoming::Rejected(m)) => assert_eq!(m, "too big"),
            _ => panic!("expected a rejection"),
        }
    }

    #[test]
    fn nonsense_is_an_error_rather_than_a_guess() {
        assert!(parse_routed("{}").is_err());
        assert!(parse_routed("not json at all").is_err());
        assert!(parse_routed(
            r#"{"type":"update","note":"AAAAAAAAAAAAAAAAAAAAAA","seq":1,"envelope":"!!!"}"#
        )
        .is_err());
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
        assert!(matches!(
            parse_routed(r#"{"type":"reset","note":"AAAAAAAAAAAAAAAAAAAAAA"}"#).unwrap(),
            Routed::Note(n, Incoming::Reset) if n == note
        ));
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
    fn a_full_note_is_said_in_words() {
        let note = crate::ids::NoteId::from_bytes([0u8; 16]);
        match parse_routed(
            r#"{"type":"error","note":"AAAAAAAAAAAAAAAAAAAAAA","message":"note_full"}"#,
        )
        .unwrap()
        {
            Routed::Note(n, Incoming::Rejected(words)) => {
                assert_eq!(n, note);
                assert!(words.contains("too large"), "{words}");
            }
            _ => panic!("expected a rejection"),
        }
        assert!(matches!(
            parse_routed(r#"{"type":"crowded","note":"AAAAAAAAAAAAAAAAAAAAAA"}"#).unwrap(),
            Routed::Note(n, Incoming::Crowded) if n == note
        ));
        assert!(matches!(
            parse_routed(r#"{"type":"compact_refused","note":"AAAAAAAAAAAAAAAAAAAAAA","reason":"stale"}"#).unwrap(),
            Routed::Note(n, Incoming::CompactRefused(r)) if n == note && r == "stale"
        ));
    }

    #[test]
    fn a_compaction_says_everything_the_relay_checks() {
        let note = crate::ids::NoteId::from_bytes([0u8; 16]);
        let request = crate::compaction::Request::sign(
            note,
            &crate::crypto::NoteKey([1u8; 32]),
            9,
            b"hi".to_vec(),
        );
        let text = compact_note(note, &request);
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["type"], "compact");
        assert_eq!(value["note"], "AAAAAAAAAAAAAAAAAAAAAA");
        assert_eq!(value["covers"], 9);
        assert_eq!(value["base"], "aGk");
        assert_eq!(value["key"], URL_SAFE_NO_PAD.encode(request.key.0));
        assert_eq!(
            value["signature"],
            URL_SAFE_NO_PAD.encode(request.signature)
        );
    }
}
