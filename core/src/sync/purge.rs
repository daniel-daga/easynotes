//! Asking the relay to forget topics for good
//! ([ADR 0026](../../../docs/decisions.md#adr-0026)).
//!
//! One plain HTTP call, like the invite calls and for the same reason: it needs
//! no socket and authenticates as no device. Its authority is the signature on
//! each topic, made by [`crate::purge::Request::sign`].

use super::transport::post;
use super::wire::{self, invite_status};
use crate::error::{CoreError, Result};
use crate::ids::NoteId;
use crate::purge::Request;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;

/// How many topics go in one request: the relay's own limit.
pub const MAX_TOPICS: usize = 512;

/// Purge every topic named. Returns the ones the relay refused, with its
/// reason; every other one is gone, including those that already were.
///
/// An error means the relay could not be asked, or did not answer, and says
/// nothing about which topics went. Purging is idempotent, so the answer to
/// an error is to call this again with the same list.
pub async fn purge(http_base: &str, topics: &[(NoteId, Request)]) -> Result<Vec<(NoteId, String)>> {
    let mut refused = Vec::new();
    for chunk in topics.chunks(MAX_TOPICS) {
        let encoded: Vec<String> = chunk.iter().map(|(note, _)| note.encode()).collect();
        let body = wire::PurgeRequest {
            topics: chunk
                .iter()
                .zip(&encoded)
                .map(|((_, request), note)| wire::PurgeTopic {
                    note,
                    key: URL_SAFE_NO_PAD.encode(request.key.0),
                    signature: URL_SAFE_NO_PAD.encode(request.signature),
                })
                .collect(),
        };
        let body = serde_json::to_string(&body)
            .map_err(|e| CoreError::Storage(format!("unserializable request: {e}")))?;
        let (status, text, _) = post(&wire::api(http_base, "/purge"), body).await?;
        invite_status(status, &text)?;
        let answer: wire::PurgeResponse = serde_json::from_str(&text)
            .map_err(|e| wire::net(format!("sync server sent nonsense: {e}")))?;
        for entry in answer.refused {
            let note = NoteId::decode(&entry.note)
                .map_err(|_| wire::net("sync server refused a topic it was not asked about"))?;
            refused.push((note, entry.reason));
        }
    }
    Ok(refused)
}

/// Purge everything a [`crate::note::NoteCore::purge_plan`] names: each topic
/// under its first key, then any the relay refused as unauthorised under its
/// fallback. Succeeds only when every topic is gone.
///
/// Topics go in the order, which puts the account document last: if
/// this stops partway, the account document still says what is left, and the
/// deletion can be finished from here or from any device with the phrase.
pub async fn purge_all(http_base: &str, plan: &[crate::note::PurgeTarget]) -> Result<()> {
    let first: Vec<_> = plan
        .iter()
        .filter(|t| !t.account)
        .map(|t| (t.note, t.request.clone()))
        .collect();
    let refused = purge(http_base, &first).await?;
    let mut retry = Vec::new();
    let mut left = Vec::new();
    for (note, reason) in refused {
        match plan
            .iter()
            .find(|t| t.note == note)
            .and_then(|t| t.fallback.clone())
        {
            Some(fallback) => retry.push((note, fallback)),
            None => left.push((note, reason)),
        }
    }
    left.extend(purge(http_base, &retry).await?);
    if left.is_empty() {
        let accounts: Vec<_> = plan
            .iter()
            .filter(|t| t.account)
            .map(|t| (t.note, t.request.clone()))
            .collect();
        left.extend(purge(http_base, &accounts).await?);
    }
    match left.first() {
        None => Ok(()),
        Some((note, reason)) => Err(CoreError::Storage(format!(
            "the sync server kept {} of these ({note}: {reason})",
            left.len()
        ))),
    }
}
