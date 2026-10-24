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
use crate::account::MemberIdentity;
use crate::crypto::AuthorKey;
use crate::error::{CoreError, Result};
use crate::ids::NoteId;
use crate::invite::{self, InviteSecret, JoinRequest, OwnerSecret};
use crate::relay_access::RelayAccess;
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
    let mut failures = 0u32;
    loop {
        match post(url, body.clone()).await {
            Ok((status, _text, retry_after)) if transient_invite_status(status) => {
                let delay = invitation_retry_delay(retry_after, failures);
                super::timer::sleep(delay).await;
                failures = failures.saturating_add(1);
            }
            Ok((status, text, _retry_after)) => {
                invite_status(status, &text)?;
                return Ok(text);
            }
            Err(_) => {
                super::timer::sleep(randomized_retry(failures)).await;
                failures = failures.saturating_add(1);
            }
        }
    }
}

async fn call_json<Q: Serialize, R: DeserializeOwned>(url: &str, body: &Q) -> Result<R> {
    let text = call(url, body).await?;
    serde_json::from_str(&text).map_err(|e| wire::net(format!("sync server sent nonsense: {e}")))
}

const FIRST_RETRY: std::time::Duration = std::time::Duration::from_secs(1);
const MAX_RETRY: std::time::Duration = std::time::Duration::from_secs(60);

fn transient_invite_status(status: u16) -> bool {
    status == 408 || status == 429 || (500..=599).contains(&status)
}

fn invitation_retry_delay(
    server_delay: Option<std::time::Duration>,
    failures: u32,
) -> std::time::Duration {
    server_delay.unwrap_or_else(|| randomized_retry(failures))
}

pub(crate) fn parse_retry_after(value: Option<&str>) -> Option<std::time::Duration> {
    value?
        .parse::<u64>()
        .ok()
        .map(std::time::Duration::from_secs)
}

fn randomized_retry(failures: u32) -> std::time::Duration {
    use rand::RngExt;
    let ceiling = FIRST_RETRY
        .saturating_mul(1u32 << failures.min(6))
        .min(MAX_RETRY);
    let max_ms = ceiling.as_millis() as u64;
    std::time::Duration::from_millis(rand::rng().random_range((max_ms / 2).max(1)..=max_ms))
}

fn unb64(raw: &str) -> Result<Vec<u8>> {
    URL_SAFE_NO_PAD
        .decode(raw)
        .map_err(|_| wire::net("sync server sent malformed base64"))
}

/// Open an invite on the relay. Returns how many seconds it will last, which
/// may be less than asked for.
pub async fn open(
    http_base: &str,
    access: Option<&RelayAccess>,
    secret: &InviteSecret,
    owner: &OwnerSecret,
    expires_in: u64,
) -> Result<u64> {
    let id = secret.id().encode();
    let created: wire::CreatedInvite = call_json(
        &wire::api(http_base, "/invites"),
        &wire::CreateInvite {
            invite: &id,
            token_hash: &secret.token_hash(),
            owner_hash: &owner.hash(),
            expires_in,
            access: access.map(|key| key.invite_proof(&secret.id())),
        },
    )
    .await?;
    Ok(created.expires_in)
}

/// Claim an invite with a request from `member`, and return the request's
/// name. Asking again as the same person is safe: it names the same request,
/// and the relay gives it back rather than finding the link used.
pub async fn request(
    http_base: &str,
    access: Option<&RelayAccess>,
    secret: &InviteSecret,
    member: &AuthorKey,
    sealed_request: &[u8],
) -> Result<String> {
    let payload = URL_SAFE_NO_PAD.encode(sealed_request);
    let joined: wire::JoinedInvite = call_json(
        &invite_url(http_base, secret.id(), "/requests"),
        &wire::JoinInvite {
            token: &secret.token(),
            request: &invite::request_name(secret, member),
            payload: &payload,
            access: access.map(|key| key.invite_proof(&secret.id())),
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
    /// Approved, and the answer opened: which note, and whose key signs its
    /// access packages. The note key comes from the note's current package.
    Approved {
        note: NoteId,
        owner: AuthorKey,
    },
}

/// Ask how a request went, and open the answer if there is one.
pub async fn poll(
    http_base: &str,
    access: Option<&RelayAccess>,
    secret: &InviteSecret,
    member: &MemberIdentity,
    request: &str,
) -> Result<Answer> {
    let polled: wire::PolledRequest = call_json(
        &invite_url(http_base, secret.id(), &format!("/requests/{request}")),
        &wire::PollInvite {
            token: &secret.token(),
            access: access.map(|key| key.invite_proof(&secret.id())),
        },
    )
    .await?;
    Ok(match (polled.state, polled.grant) {
        (RequestState::Pending, _) => Answer::Pending {
            expires_in: polled.expires_in,
        },
        (RequestState::Declined, _) => Answer::Declined,
        (RequestState::Approved, Some(answer)) => {
            let (note, owner) = invite::open_answer(secret, member, &unb64(&answer)?)?;
            Answer::Approved { note, owner }
        }
        (RequestState::Approved, None) => {
            return Err(wire::net(
                "sync server approved a request and sent no grant",
            ))
        }
    })
}

/// One request, as its owner sees it.
pub struct Listed {
    pub request: String,
    pub state: RequestState,
    /// `None` when the request does not open under this invite's key, or is
    /// not filed under the name its person would have given it: not made by
    /// this app, or a corrupted row. Such a request is turned away rather than
    /// let in.
    pub join: Option<JoinRequest>,
}

pub struct Listing {
    pub open: bool,
    pub expires_in: u64,
    pub requests: Vec<Listed>,
}

/// Every request on an invite, opened.
pub async fn list(
    http_base: &str,
    access: Option<&RelayAccess>,
    secret: &InviteSecret,
    owner: &OwnerSecret,
) -> Result<Listing> {
    let listing: wire::InviteListing = call_json(
        &invite_url(http_base, secret.id(), "/list"),
        &wire::OwnInvite {
            owner: &owner.encode(),
            access: access.map(|key| key.invite_proof(&secret.id())),
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
                    .and_then(|sealed| invite::open_request(secret, &sealed).ok())
                    .filter(|join| join.is_named(secret, &r.request)),
                request: r.request,
                state: r.state,
            })
            .collect(),
    })
}

/// Let a request's person in with the answer sealed for it —
/// [`crate::note::NoteCore::approval_answer`] makes one once the package that
/// lets the person in is on the relay.
pub async fn approve(
    http_base: &str,
    access: Option<&RelayAccess>,
    secret: &InviteSecret,
    owner: &OwnerSecret,
    request: &str,
    grant: &[u8],
) -> Result<()> {
    let grant = URL_SAFE_NO_PAD.encode(grant);
    answer(http_base, access, secret, owner, request, Some(&grant)).await
}

/// Turn a request away. Only a request that could not be read is: the
/// invite has been claimed either way, and nobody else can ask on it.
pub async fn decline(
    http_base: &str,
    access: Option<&RelayAccess>,
    secret: &InviteSecret,
    owner: &OwnerSecret,
    request: &str,
) -> Result<()> {
    answer(http_base, access, secret, owner, request, None).await
}

async fn answer(
    http_base: &str,
    access: Option<&RelayAccess>,
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
            access: access.map(|key| key.invite_proof(&secret.id())),
        },
    )
    .await
    .map(|_| ())
}

/// Take an invite down. Anyone holding the link is told it does not exist.
pub async fn withdraw(
    http_base: &str,
    access: Option<&RelayAccess>,
    secret: &InviteSecret,
    owner: &OwnerSecret,
) -> Result<()> {
    call(
        &invite_url(http_base, secret.id(), "/withdraw"),
        &wire::OwnInvite {
            owner: &owner.encode(),
            access: access.map(|key| key.invite_proof(&secret.id())),
        },
    )
    .await
    .map(|_| ())
}

#[cfg(test)]
mod retry_tests {
    use super::*;

    #[test]
    fn invitations_retry_only_transient_statuses() {
        for status in [408, 429, 500, 502, 599] {
            assert!(transient_invite_status(status));
        }
        for status in [400, 401, 403, 404, 409, 410, 413] {
            assert!(!transient_invite_status(status));
        }
    }

    #[test]
    fn invitation_retry_delays_share_the_transfer_ceilings() {
        let ceilings = [1, 2, 4, 8, 16, 32, 60].map(std::time::Duration::from_secs);
        for (failure, ceiling) in ceilings.into_iter().enumerate() {
            for _ in 0..100 {
                let delay = randomized_retry(failure as u32);
                assert!(delay >= ceiling / 2);
                assert!(delay <= ceiling);
            }
        }
        assert!(randomized_retry(20) <= MAX_RETRY);
        let server = std::time::Duration::from_secs(23);
        assert_eq!(invitation_retry_delay(Some(server), 0), server);
        assert_eq!(
            parse_retry_after(Some("7")),
            Some(std::time::Duration::from_secs(7))
        );
        assert_eq!(parse_retry_after(Some("invalid")), None);
        assert_eq!(parse_retry_after(None), None);
    }
}
