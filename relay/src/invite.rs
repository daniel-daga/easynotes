//! Invites: a mailbox with a lifetime, which the relay keeps and cannot read.
//!
//! The flow, from the relay's side of it:
//!
//! 1. An owner opens an invite, naming it by an ID and handing over two hashes —
//!    of the join token every guest will present, and of an owner secret only
//!    the owner holds.
//! 2. A guest presents the token and leaves a sealed request. The first one
//!    to do so claims the invite: an invite holds one request, and anyone
//!    after is told it has been used.
//! 3. The owner's device, presenting its secret, reads the request and answers
//!    it: with a sealed grant to let the person in, or with nothing to turn
//!    away a request it cannot read. Nobody is asked; holding the link is
//!    what lets a person in. Approving closes the invite.
//! 4. The guest, presenting the token again, reads its answer.
//!
//! What the relay checks is the whole of what it can: that the hashes match,
//! that the invite has not expired or been claimed already, and that nothing
//! is larger than it should be. What it cannot check is anything inside
//! a request or a grant, and it does not try — see `docs/adr/0015-invites.md`.
//!
//! **None of this is tied to a device key.** The note endpoints want a token
//! from the signed handshake; these do not, on purpose. Were an invite opened
//! by a device and joined by another, the relay would hold an edge between two
//! device keys that says "these are two people", which is precisely what ADR
//! 0014 promises it never learns. The link's token and the owner's secret are
//! the whole of the authority here, and they name nobody.
//!
//! **A wrong secret and a missing invite get the same answer.** A caller that
//! does not hold the token learns nothing from asking, including whether the
//! invite exists.

use crate::app::AppState;
use crate::error::{RelayError, Result};
use crate::protocol::*;
use crate::store::{AddRequest, Answered, Invite, NewInvite};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use easynotes_core::ids::NoteId;
use sha2::{Digest, Sha256};
use std::sync::Arc;

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn b64(s: &str, what: &'static str) -> Result<Vec<u8>> {
    URL_SAFE_NO_PAD
        .decode(s)
        .map_err(|_| RelayError::BadRequest(format!("{what} is not base64url")))
}

/// An invite ID has the shape of a note ID and is checked the same way, so
/// that a typo cannot make a row.
fn invite_id(raw: &str) -> Result<()> {
    NoteId::decode(raw)
        .map(|_| ())
        .map_err(|_| RelayError::BadRequest("malformed invite id".into()))
}

fn hash32(raw: &str, what: &'static str) -> Result<Vec<u8>> {
    let bytes = b64(raw, what)?;
    if bytes.len() != 32 {
        return Err(RelayError::BadRequest(format!("{what} is not 32 bytes")));
    }
    Ok(bytes)
}

/// Whether `secret` hashes to `expected`.
///
/// Comparing hashes rather than secrets is what makes an ordinary `==` safe
/// here: a timing difference can tell a caller how many leading bytes of a
/// SHA-256 it guessed, which says nothing about the secret behind it.
fn holds(secret: &str, expected: &[u8]) -> bool {
    match URL_SAFE_NO_PAD.decode(secret) {
        Ok(raw) => Sha256::digest(&raw).as_slice() == expected,
        Err(_) => false,
    }
}

fn expires_in(invite: &Invite, now: i64) -> u64 {
    (invite.expires_at - now).max(0) as u64
}

async fn load(state: &AppState, id: &str) -> Result<Invite> {
    invite_id(id)?;
    state.store.invite(id).await?.ok_or(RelayError::NotFound)
}

/// Load an invite for a guest, who must hold the link.
async fn as_guest(state: &AppState, id: &str, token: &str) -> Result<Invite> {
    let invite = load(state, id).await?;
    if !holds(token, &invite.token_hash) {
        return Err(RelayError::NotFound);
    }
    Ok(invite)
}

/// Load an invite for its owner, who must hold the owner secret.
async fn as_owner(state: &AppState, id: &str, owner: &str) -> Result<Invite> {
    let invite = load(state, id).await?;
    if !holds(owner, &invite.owner_hash) {
        return Err(RelayError::NotFound);
    }
    Ok(invite)
}

/// `POST /v1/invites`
pub async fn create(
    State(state): State<Arc<AppState>>,
    Json(req): Json<CreateInvite>,
) -> Result<(StatusCode, Json<CreatedInvite>)> {
    invite_id(&req.invite)?;
    let token_hash = hash32(&req.token_hash, "token hash")?;
    let owner_hash = hash32(&req.owner_hash, "owner hash")?;
    if token_hash == owner_hash {
        // Only a client bug gets here, and the bug is that a guest could
        // answer their own request.
        return Err(RelayError::BadRequest(
            "the owner secret must not be the join token".into(),
        ));
    }
    let ttl = req.expires_in.clamp(1, INVITE_MAX_TTL_SECS);
    let now = now();
    let created = state
        .store
        .create_invite(
            NewInvite {
                id: &req.invite,
                token_hash: &token_hash,
                owner_hash: &owner_hash,
                expires_at: now + ttl as i64,
            },
            now,
        )
        .await?;
    if !created {
        return Err(RelayError::Conflict("an invite with this id exists"));
    }
    Ok((StatusCode::CREATED, Json(CreatedInvite { expires_in: ttl })))
}

/// `POST /v1/invites/{invite}/requests`
pub async fn join(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(req): Json<JoinInvite>,
) -> Result<Json<JoinedInvite>> {
    let payload = b64(&req.payload, "payload")?;
    if payload.len() > MAX_INVITE_PAYLOAD_BYTES {
        return Err(RelayError::TooLarge);
    }
    // A request's name has the shape of a note ID, and is checked the same way.
    NoteId::decode(&req.request)
        .map_err(|_| RelayError::BadRequest("malformed request id".into()))?;
    as_guest(&state, &id, &req.token).await?;

    match state
        .store
        .add_invite_request(&id, &req.request, &payload, now())
        .await?
    {
        AddRequest::Added | AddRequest::Again => Ok(Json(JoinedInvite {
            request: req.request,
        })),
        // Withdrawn between the check and the write.
        AddRequest::Missing => Err(RelayError::NotFound),
        AddRequest::Closed => Err(RelayError::Gone("this invite has expired or been used")),
    }
}

/// `POST /v1/invites/{invite}/requests/{request}` — a guest reading its answer.
pub async fn poll(
    State(state): State<Arc<AppState>>,
    Path((id, request)): Path<(String, String)>,
    Json(req): Json<PollInvite>,
) -> Result<Json<PolledRequest>> {
    let invite = as_guest(&state, &id, &req.token).await?;
    let found = invite
        .requests
        .iter()
        .find(|r| r.id == request)
        .ok_or(RelayError::NotFound)?;
    Ok(Json(PolledRequest {
        state: found.state,
        grant: found.grant.as_ref().map(|g| URL_SAFE_NO_PAD.encode(g)),
        expires_in: expires_in(&invite, now()),
    }))
}

/// `POST /v1/invites/{invite}/list` — the owner reading the requests.
pub async fn list(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(req): Json<OwnInvite>,
) -> Result<Json<InviteListing>> {
    let invite = as_owner(&state, &id, &req.owner).await?;
    let now = now();
    Ok(Json(InviteListing {
        open: invite.open && invite.expires_at > now,
        expires_in: expires_in(&invite, now),
        requests: invite
            .requests
            .iter()
            .map(|r| ListedRequest {
                request: r.id.clone(),
                payload: URL_SAFE_NO_PAD.encode(&r.payload),
                state: r.state,
            })
            .collect(),
    }))
}

/// `POST /v1/invites/{invite}/requests/{request}/answer`
pub async fn answer(
    State(state): State<Arc<AppState>>,
    Path((id, request)): Path<(String, String)>,
    Json(req): Json<AnswerInvite>,
) -> Result<StatusCode> {
    let grant = match &req.grant {
        Some(g) => {
            let raw = b64(g, "grant")?;
            if raw.len() > MAX_INVITE_GRANT_BYTES {
                return Err(RelayError::TooLarge);
            }
            Some(raw)
        }
        None => None,
    };
    as_owner(&state, &id, &req.owner).await?;
    match state
        .store
        .answer_invite_request(&id, &request, grant.as_deref(), now())
        .await?
    {
        Answered::Done => Ok(StatusCode::NO_CONTENT),
        Answered::Missing => Err(RelayError::NotFound),
        Answered::NotPending => Err(RelayError::Conflict("that request was answered already")),
        Answered::Closed => Err(RelayError::Gone("this invite has expired or been used")),
    }
}

/// `POST /v1/invites/{invite}/withdraw`
pub async fn withdraw(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(req): Json<OwnInvite>,
) -> Result<StatusCode> {
    as_owner(&state, &id, &req.owner).await?;
    state.store.withdraw_invite(&id).await?;
    Ok(StatusCode::NO_CONTENT)
}
