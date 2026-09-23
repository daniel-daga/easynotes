//! The invite calls, over whichever transport this build has.
//!
//! Six plain HTTP calls, one per step of the flow in `relay::invite`. Nothing
//! here holds state between them: what an owner or a guest must remember —
//! the secrets, the request it is waiting on — is [`crate::note::NoteCore`]'s
//! to keep, and the order the calls go in is the app's.
//!
//! None of them authenticates as the device. That is the point, not an
//! omission: see `relay::invite` and ADR 0015.

use super::transport::post;
use super::wire::{self, invite_status, invite_url, RequestState};
use crate::crypto::{AuthorKey, NoteKey};
use crate::error::{CoreError, Result};
use crate::ids::NoteId;
use crate::invite::{self, GuestKey, InviteSecret, JoinRequest, OwnerSecret};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde::de::DeserializeOwned;
use serde::Serialize;

/// How long an invite lasts unless asked otherwise: the design doc's seven
/// days, which is also the relay's own default.
pub const DEFAULT_TTL_SECS: u64 = 7 * 24 * 60 * 60;

async fn call<Q: Serialize>(url: &str, body: &Q) -> Result<String> {
    let body = serde_json::to_string(body)
        .map_err(|e| CoreError::Storage(format!("unserializable request: {e}")))?;
    let (status, text) = post(url, body).await?;
    invite_status(status, &text)?;
    Ok(text)
}

async fn call_json<Q: Serialize, R: DeserializeOwned>(url: &str, body: &Q) -> Result<R> {
    let text = call(url, body).await?;
    serde_json::from_str(&text).map_err(|e| wire::net(format!("relay sent nonsense: {e}")))
}

fn unb64(raw: &str) -> Result<Vec<u8>> {
    URL_SAFE_NO_PAD
        .decode(raw)
        .map_err(|_| wire::net("relay sent malformed base64"))
}

/// Open an invite on the relay. Returns how many seconds it will last, which
/// may be less than asked for.
pub async fn open(
    http_base: &str,
    secret: &InviteSecret,
    owner: &OwnerSecret,
    expires_in: u64,
) -> Result<u64> {
    let id = secret.id().encode();
    let created: wire::CreatedInvite = call_json(
        &format!("{http_base}/v1/invites"),
        &wire::CreateInvite {
            invite: &id,
            token_hash: &secret.token_hash(),
            owner_hash: &owner.hash(),
            expires_in,
        },
    )
    .await?;
    Ok(created.expires_in)
}

/// Leave a request on an invite. Returns the relay's name for it.
pub async fn request(
    http_base: &str,
    secret: &InviteSecret,
    sealed_request: &[u8],
) -> Result<String> {
    let payload = URL_SAFE_NO_PAD.encode(sealed_request);
    let joined: wire::JoinedInvite = call_json(
        &invite_url(http_base, secret.id(), "/requests"),
        &wire::JoinInvite {
            token: &secret.token(),
            payload: &payload,
        },
    )
    .await?;
    Ok(joined.request)
}

/// Where a guest's request stands.
pub enum Answer {
    Pending {
        expires_in: u64,
    },
    Declined,
    /// Approved, and the grant opened: here is the note.
    Approved {
        note: NoteId,
        key: NoteKey,
        owner: Option<AuthorKey>,
        generation: u64,
    },
}

/// Ask how a request went, and open the grant if there is one.
pub async fn poll(
    http_base: &str,
    secret: &InviteSecret,
    guest: &GuestKey,
    request: &str,
) -> Result<Answer> {
    let polled: wire::PolledRequest = call_json(
        &invite_url(http_base, secret.id(), &format!("/requests/{request}")),
        &wire::PollInvite {
            token: &secret.token(),
        },
    )
    .await?;
    Ok(match (polled.state, polled.grant) {
        (RequestState::Pending, _) => Answer::Pending {
            expires_in: polled.expires_in,
        },
        (RequestState::Declined, _) => Answer::Declined,
        (RequestState::Approved, Some(grant)) => {
            let (note, key, owner, generation) =
                invite::open_owned_grant(secret, guest, &unb64(&grant)?)?;
            Answer::Approved {
                note,
                key,
                owner,
                generation,
            }
        }
        (RequestState::Approved, None) => {
            return Err(wire::net("relay approved a request and sent no grant"))
        }
    })
}

/// One request, as its owner sees it.
pub struct Listed {
    pub request: String,
    pub state: RequestState,
    /// `None` when the request does not open under this invite's key: somebody
    /// who reached the relay's endpoint without the link, or a corrupted row.
    /// Shown as unreadable rather than dropped, so an owner is not left
    /// wondering why an invite says it is full.
    pub join: Option<JoinRequest>,
}

pub struct Listing {
    pub open: bool,
    pub expires_in: u64,
    pub requests: Vec<Listed>,
}

/// Every request on an invite, opened.
pub async fn list(http_base: &str, secret: &InviteSecret, owner: &OwnerSecret) -> Result<Listing> {
    let listing: wire::InviteListing = call_json(
        &invite_url(http_base, secret.id(), "/list"),
        &wire::OwnInvite {
            owner: &owner.encode(),
        },
    )
    .await?;
    Ok(Listing {
        open: listing.open,
        expires_in: listing.expires_in,
        requests: listing
            .requests
            .into_iter()
            .map(|r| Listed {
                join: unb64(&r.payload)
                    .ok()
                    .and_then(|sealed| invite::open_request(secret, &sealed).ok()),
                request: r.request,
                state: r.state,
            })
            .collect(),
    })
}

/// Approve a request with a grant sealed for it —
/// [`crate::note::NoteCore::seal_grant`] makes one, so the note key never
/// passes through here.
pub async fn approve(
    http_base: &str,
    secret: &InviteSecret,
    owner: &OwnerSecret,
    request: &str,
    grant: &[u8],
) -> Result<()> {
    let grant = URL_SAFE_NO_PAD.encode(grant);
    answer(http_base, secret, owner, request, Some(&grant)).await
}

pub async fn decline(
    http_base: &str,
    secret: &InviteSecret,
    owner: &OwnerSecret,
    request: &str,
) -> Result<()> {
    answer(http_base, secret, owner, request, None).await
}

async fn answer(
    http_base: &str,
    secret: &InviteSecret,
    owner: &OwnerSecret,
    request: &str,
    grant: Option<&str>,
) -> Result<()> {
    call(
        &invite_url(
            http_base,
            secret.id(),
            &format!("/requests/{request}/answer"),
        ),
        &wire::AnswerInvite {
            owner: &owner.encode(),
            grant,
        },
    )
    .await
    .map(|_| ())
}

/// Take an invite down. Anyone holding the link is told it does not exist.
pub async fn withdraw(http_base: &str, secret: &InviteSecret, owner: &OwnerSecret) -> Result<()> {
    call(
        &invite_url(http_base, secret.id(), "/withdraw"),
        &wire::OwnInvite {
            owner: &owner.encode(),
        },
    )
    .await
    .map(|_| ())
}
