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
    /// The relay access proof (ADR 0028), when this device holds a key.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub access: Option<String>,
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
    /// The note was purged from the relay, by its owner deleting it or their
    /// account ([ADR 0026](../../../docs/adr/0026-deleting-an-account.md)).
    /// Nothing more of it will be stored or sent.
    Gone,
    /// This device has written more than the relay takes from one address in
    /// a while ([ADR 0028](../../../docs/adr/0028-who-may-write-to-a-relay.md)
    /// §3). The relay closes the socket after saying so; what was refused is
    /// still in the outbox, and goes again once the engine has backed off.
    RateLimited,
}

/// What the relay refuses a purged topic with, and tells its subscribers.
pub(crate) const GONE: &str = "gone";

/// What the relay refuses an append with past its write budget.
pub(crate) const RATE_LIMITED: &str = "rate_limited";

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

// -- the protocol version ----------------------------------------------------
//
// ADR 0029 §4. Every request names the protocol it speaks in `?p=`, and a relay
// that does not speak it answers `426` with the range it does. A query
// parameter rather than a header, because a browser cannot set headers on a
// WebSocket and a new header would put a CORS preflight in front of requests
// that have none.

/// The protocol this build speaks, and sends on every request.
///
/// Bumped only for a change an older peer cannot ignore: a new required field,
/// a changed meaning, a removed message. Something an older peer can skip
/// (ADR 0029 §3) does not bump it. A client is to keep speaking the version
/// before this one as well (ADR 0029 §4); while there has only been one
/// version, there is nothing to fall back to.
pub const PROTOCOL: u32 = 1;

/// `{base}/v1{path}`, naming the protocol. `path` starts with `/` and carries
/// no query of its own; one that needs more (the socket's token) appends it
/// after this with `&`.
pub(crate) fn api(base: &str, path: &str) -> String {
    format!("{base}/v1{path}?p={PROTOCOL}")
}

#[derive(Deserialize)]
struct Refusal {
    code: String,
    protocol: Range,
}

/// Only the top of the relay's range matters to a build that speaks one
/// version at a time; `min` is in the body for whoever reads it.
#[derive(Deserialize)]
struct Range {
    max: u32,
}

/// What a `426` means, if `status` is one and its body is the relay's.
///
/// The two answers are different requests of different people: "update the
/// app" is a button, "the relay is too old" is an email to whoever runs it, so
/// the relay's range is compared with this build's to know which to say. A
/// relay whose newest protocol is older than this build's is behind; anything
/// else has moved on without it. (A build that could fall back to the version
/// before its own would try that first; none has one to fall back to yet.) A
/// `426` without the relay's body is still a refusal to talk, and is taken as
/// the likelier case, an app that has fallen behind.
pub(crate) fn outdated(status: u16, body: &[u8]) -> Option<CoreError> {
    if status != 426 {
        return None;
    }
    let relay = serde_json::from_slice::<Refusal>(body)
        .ok()
        .filter(|refusal| refusal.code == "update_app")
        .map(|refusal| refusal.protocol);
    Some(match relay {
        Some(relay) if relay.max < PROTOCOL => CoreError::RelayOutdated,
        _ => CoreError::AppOutdated,
    })
}

/// The relay's code for a request without a valid access proof (ADR 0028).
pub(crate) const ACCESS_REQUIRED: &str = "access_required";

#[derive(Deserialize)]
struct Coded {
    code: String,
}

/// A refusal that is not about the request itself but about who may make it:
/// a protocol this relay does not speak ([`outdated`]), or a relay that wants
/// an access key this device does not hold. A `403` without the relay's code
/// is left to the caller, since a proxy's own 403 is not this.
pub(crate) fn refused(status: u16, body: &[u8]) -> Option<CoreError> {
    if status == 403
        && serde_json::from_slice::<Coded>(body).is_ok_and(|coded| coded.code == ACCESS_REQUIRED)
    {
        return Some(CoreError::AccessRequired);
    }
    outdated(status, body)
}

#[cfg(test)]
mod protocol_tests {
    use super::*;

    #[test]
    fn every_request_names_the_protocol() {
        assert_eq!(
            api("https://relay.example", "/challenge"),
            format!("https://relay.example/v1/challenge?p={PROTOCOL}")
        );
        assert!(sync_url("wss://relay.example", "t0k")
            .starts_with(&format!("wss://relay.example/v1/sync?p={PROTOCOL}&")));
        assert!(sync_url("wss://relay.example", "t0k").ends_with("&token=t0k"));
    }

    #[test]
    fn a_relay_that_has_moved_on_asks_for_an_app_update() {
        let body = format!(
            r#"{{"code":"update_app","protocol":{{"min":{},"max":{}}}}}"#,
            PROTOCOL + 1,
            PROTOCOL + 2
        );
        assert!(matches!(
            outdated(426, body.as_bytes()),
            Some(CoreError::AppOutdated)
        ));
    }

    #[test]
    fn a_relay_that_is_behind_is_its_operators_to_update() {
        let body = format!(
            r#"{{"code":"update_app","protocol":{{"min":0,"max":{}}}}}"#,
            PROTOCOL - 1
        );
        assert!(matches!(
            outdated(426, body.as_bytes()),
            Some(CoreError::RelayOutdated)
        ));
    }

    #[test]
    fn a_relay_that_wants_a_key_says_so() {
        assert!(matches!(
            refused(403, br#"{"code":"access_required"}"#),
            Some(CoreError::AccessRequired)
        ));
        // Somebody else's 403 is not the relay asking for its key.
        assert!(refused(403, b"<html>Forbidden</html>").is_none());
        assert!(refused(401, br#"{"code":"access_required"}"#).is_none());
    }

    #[test]
    fn a_token_request_names_its_proof_only_when_it_has_one() {
        let open = TokenRequest {
            device: "d",
            nonce: "n",
            signature: "s",
            access: None,
        };
        assert!(!serde_json::to_string(&open).unwrap().contains("access"));
        let gated = TokenRequest {
            access: Some("p".into()),
            ..open
        };
        assert!(serde_json::to_string(&gated)
            .unwrap()
            .contains(r#""access":"p""#));
    }

    #[test]
    fn only_a_426_is_a_protocol_refusal() {
        assert!(outdated(400, b"{}").is_none());
        assert!(outdated(200, b"").is_none());
        // A proxy's own 426 page is still a refusal to talk.
        assert!(matches!(
            outdated(426, b"<html>Upgrade Required</html>"),
            Some(CoreError::AppOutdated)
        ));
    }

    #[test]
    fn a_relay_answer_with_fields_this_build_does_not_know_still_reads() {
        // ADR 0029 §3: JSON bodies ignore what they do not know, in both
        // directions. A newer relay adding a field must not break the handshake.
        let challenge: ChallengeResponse =
            serde_json::from_str(r#"{"nonce":"abc","expires_in":60,"hint":"x"}"#).unwrap();
        assert_eq!(challenge.nonce, "abc");
        let token: TokenResponse =
            serde_json::from_str(r#"{"token":"t","scope":["notes"]}"#).unwrap();
        assert_eq!(token.token, "t");
    }
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
        /// The live list (ADR 0027 §5), as object IDs. Left out when there is
        /// none, which a relay reads as "vouch for nothing".
        #[serde(skip_serializing_if = "Option::is_none")]
        media: Option<Vec<String>>,
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
    /// A message type this build does not know: something a newer relay says
    /// that an older client can do without. Skipped, never an error — an
    /// error drops the socket, and a relay that added any message at all
    /// would put every older client into a reconnect loop.
    #[serde(other)]
    Unknown,
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
    format!("{}&token={token}", api(ws_base, "/sync"))
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
        media: request
            .media
            .as_ref()
            .map(|media| media.iter().map(|object| object.encode()).collect()),
        key: URL_SAFE_NO_PAD.encode(request.key.0),
        signature: URL_SAFE_NO_PAD.encode(request.signature),
    })
}

/// Read one message from a device's socket, or `None` for a message type
/// this build does not know, which the caller skips.
///
/// No cursor here, unlike [`parse`]: there is one per note, and the engine
/// keeps them. An update or a `caught` for a note ID that does not parse is
/// nonsense, because this device never subscribed to one; a refusal naming one
/// is still a refusal, and is passed on unrouted rather than lost. Fields a
/// known message gains are ignored, so adding one needs no new type.
pub(crate) fn parse_routed(text: &str) -> Result<Option<Routed>> {
    let nonsense = |what: &str| CoreError::Storage(format!("relay sent nonsense: {what}"));
    let parsed: DeviceReply = serde_json::from_str(text).map_err(|e| nonsense(&e.to_string()))?;
    let note = |raw: &str| crate::ids::NoteId::decode(raw).map_err(|_| nonsense("a bad note id"));
    Ok(Some(match parsed {
        DeviceReply::Unknown => return Ok(None),
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
        DeviceReply::Error {
            note: Some(raw),
            message,
        } if message == GONE => Routed::Note(note(&raw)?, Incoming::Gone),
        DeviceReply::Error {
            note: Some(raw),
            message,
        } if message == RATE_LIMITED => Routed::Note(note(&raw)?, Incoming::RateLimited),
        DeviceReply::Error { note: raw, message } => {
            let message = said(message);
            match raw.and_then(|raw| crate::ids::NoteId::decode(&raw).ok()) {
                Some(note) => Routed::Note(note, Incoming::Rejected(message)),
                None => Routed::Unrouted(message),
            }
        }
    }))
}

// -- purge -------------------------------------------------------------------
//
// The mirror of `relay::protocol`'s purge messages (ADR 0026).

#[derive(Serialize)]
pub(crate) struct PurgeRequest<'a> {
    pub topics: Vec<PurgeTopic<'a>>,
}

#[derive(Serialize)]
pub(crate) struct PurgeTopic<'a> {
    pub note: &'a str,
    pub key: String,
    pub signature: String,
}

#[derive(Deserialize)]
pub(crate) struct PurgeResponse {
    #[serde(default)]
    pub refused: Vec<PurgeRefused>,
}

#[derive(Deserialize)]
pub(crate) struct PurgeRefused {
    pub note: String,
    pub reason: String,
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
    /// The relay access proof for this invite (ADR 0028), when this device
    /// holds a key.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub access: Option<String>,
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
    /// The relay access proof for this invite (ADR 0028), when this device
    /// holds a key.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub access: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct JoinedInvite {
    pub request: String,
}

#[derive(Serialize)]
pub(crate) struct PollInvite<'a> {
    pub token: &'a str,
    /// The relay access proof for this invite (ADR 0028), when this device
    /// holds a key.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub access: Option<String>,
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
    /// The relay access proof for this invite (ADR 0028), when this device
    /// holds a key.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub access: Option<String>,
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
    /// The relay access proof for this invite (ADR 0028), when this device
    /// holds a key.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub access: Option<String>,
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
    if let Some(outdated) = refused(status, body.as_bytes()) {
        return Err(outdated);
    }
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
    api(http_base, &format!("/invites/{}{rest}", invite.encode()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A message every build knows, so never skipped.
    fn routed(text: &str) -> Result<Routed> {
        parse_routed(text).map(|routed| routed.expect("a known message type"))
    }

    #[test]
    fn a_message_type_this_build_does_not_know_is_skipped() {
        // What a newer relay might add. Not an error, which would drop the
        // socket, and not a refusal, which would be pinned on every note.
        for text in [
            r#"{"type":"notice","note":"AAAAAAAAAAAAAAAAAAAAAA","text":"hello"}"#,
            r#"{"type":"something_new"}"#,
        ] {
            assert!(parse_routed(text).unwrap().is_none(), "{text}");
        }
        // A field a known message gains is ignored in the same spirit.
        assert!(matches!(
            routed(r#"{"type":"caught","note":"AAAAAAAAAAAAAAAAAAAAAA","seq":3,"extra":[1]}"#)
                .unwrap(),
            Routed::Note(_, Incoming::Caught { seq: 3 })
        ));
        // Still nonsense: no type at all, or a known type missing what it needs.
        assert!(parse_routed(r#"{"note":"AAAAAAAAAAAAAAAAAAAAAA"}"#).is_err());
        assert!(parse_routed(r#"{"type":"caught","note":"AAAAAAAAAAAAAAAAAAAAAA"}"#).is_err());
    }

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
        match routed(text).unwrap() {
            Routed::Note(_, Incoming::Rejected(m)) => assert_eq!(m, "too big"),
            _ => panic!("expected a rejection"),
        }
    }

    #[test]
    fn nonsense_is_an_error_rather_than_a_guess() {
        assert!(routed("{}").is_err());
        assert!(routed("not json at all").is_err());
        assert!(routed(
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
        assert_eq!(
            sync_url("ws://r", "t0k"),
            format!("ws://r/v1/sync?p={PROTOCOL}&token=t0k")
        );
    }

    #[test]
    fn a_purged_note_is_gone_rather_than_refused() {
        let gone = r#"{"type":"error","note":"AAAAAAAAAAAAAAAAAAAAAA","message":"gone"}"#;
        assert!(matches!(
            routed(gone).unwrap(),
            Routed::Note(_, Incoming::Gone)
        ));
    }

    #[test]
    fn a_write_budget_refusal_is_a_reason_to_back_off() {
        let slow = r#"{"type":"error","note":"AAAAAAAAAAAAAAAAAAAAAA","message":"rate_limited"}"#;
        assert!(matches!(
            routed(slow).unwrap(),
            Routed::Note(_, Incoming::RateLimited)
        ));
    }

    #[test]
    fn what_a_device_hears_comes_back_with_its_note() {
        let note = crate::ids::NoteId::from_bytes([0u8; 16]);
        let update =
            r#"{"type":"update","note":"AAAAAAAAAAAAAAAAAAAAAA","seq":7,"envelope":"aGk"}"#;
        match routed(update).unwrap() {
            Routed::Note(n, Incoming::Envelope { seq, envelope }) => {
                assert_eq!(n, note);
                assert_eq!(seq, 7);
                assert_eq!(envelope, b"hi");
            }
            _ => panic!("expected an envelope for the note"),
        }
        match routed(r#"{"type":"caught","note":"AAAAAAAAAAAAAAAAAAAAAA","seq":3}"#).unwrap() {
            Routed::Note(n, Incoming::Caught { seq: 3 }) => assert_eq!(n, note),
            _ => panic!("expected caught for the note"),
        }
        assert!(matches!(
            routed(r#"{"type":"reset","note":"AAAAAAAAAAAAAAAAAAAAAA"}"#).unwrap(),
            Routed::Note(n, Incoming::Reset) if n == note
        ));
        // A refusal that names no note, or one that does not parse, still
        // reaches somebody.
        for text in [
            r#"{"type":"error","message":"malformed message"}"#,
            r#"{"type":"error","note":"??","message":"malformed note id"}"#,
        ] {
            assert!(matches!(routed(text).unwrap(), Routed::Unrouted(_)));
        }
        // An update for a note that cannot exist is the relay talking nonsense.
        assert!(routed(r#"{"type":"update","note":"??","seq":1,"envelope":"aGk"}"#).is_err());
    }

    #[test]
    fn a_full_note_is_said_in_words() {
        let note = crate::ids::NoteId::from_bytes([0u8; 16]);
        match routed(r#"{"type":"error","note":"AAAAAAAAAAAAAAAAAAAAAA","message":"note_full"}"#)
            .unwrap()
        {
            Routed::Note(n, Incoming::Rejected(words)) => {
                assert_eq!(n, note);
                assert!(words.contains("too large"), "{words}");
            }
            _ => panic!("expected a rejection"),
        }
        assert!(matches!(
            routed(r#"{"type":"crowded","note":"AAAAAAAAAAAAAAAAAAAAAA"}"#).unwrap(),
            Routed::Note(n, Incoming::Crowded) if n == note
        ));
        assert!(matches!(
            routed(r#"{"type":"compact_refused","note":"AAAAAAAAAAAAAAAAAAAAAA","reason":"stale"}"#).unwrap(),
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
            Some(vec![crate::ids::MediaObjectId::from_bytes([0u8; 16])]),
        );
        let text = compact_note(note, &request);
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["type"], "compact");
        assert_eq!(value["note"], "AAAAAAAAAAAAAAAAAAAAAA");
        assert_eq!(value["covers"], 9);
        assert_eq!(value["base"], "aGk");
        assert_eq!(
            value["media"],
            serde_json::json!(["AAAAAAAAAAAAAAAAAAAAAA"])
        );
        assert_eq!(value["key"], URL_SAFE_NO_PAD.encode(request.key.0));
        assert_eq!(
            value["signature"],
            URL_SAFE_NO_PAD.encode(request.signature)
        );

        // No list is no field, not an empty one: an empty list would let the
        // relay go of every object the note has.
        let unlisted = crate::compaction::Request {
            media: None,
            ..request
        };
        let value: serde_json::Value =
            serde_json::from_str(&compact_note(note, &unlisted)).unwrap();
        assert!(value.get("media").is_none());
    }
}
