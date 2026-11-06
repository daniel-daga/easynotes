//! `easynotes-relay token` — the operator's side of the private admin listener
//! ([`crate::admin`]).
//!
//! A self-hosted relay has no billing to mint device tokens, so its operator
//! does: create one and hand `<id>.<secret>` over, make it read-only, unbind it
//! from a lost device, or revoke it. The relay's store is the only record, so
//! every command is an HTTP call to that listener — never a local write — and
//! `RELAY_ADMIN_ADDR` and `RELAY_ADMIN_SECRET` must be the relay's own.

use anyhow::{anyhow, bail, Result};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use easynotes_core::relay_access::{RelayAccess, RelayToken};
use serde::Deserialize;
use std::net::SocketAddr;
use std::time::Duration;

/// How long one admin call may take. A call that hangs would leave the operator
/// unsure whether a token was minted, which is worse than a clear failure.
const TIMEOUT: Duration = Duration::from_secs(5);

/// The admin listener and the bearer secret that opens it.
pub struct AdminClient {
    base: String,
    secret: RelayAccess,
    http: reqwest::Client,
}

impl AdminClient {
    /// From `RELAY_ADMIN_ADDR` and `RELAY_ADMIN_SECRET`, as the relay reads them.
    pub fn from_env() -> Result<Self> {
        Self::new(
            std::env::var("RELAY_ADMIN_ADDR").as_deref().ok(),
            std::env::var("RELAY_ADMIN_SECRET").as_deref().ok(),
        )
    }

    /// A client for a listener at `addr`, which is `RELAY_ADMIN_ADDR`; a
    /// listener on every interface is reached on loopback, as `check` does.
    pub fn new(addr: Option<&str>, secret: Option<&str>) -> Result<Self> {
        let addr = addr.map(str::trim).filter(|addr| !addr.is_empty()).ok_or_else(|| {
            anyhow!(
                "RELAY_ADMIN_ADDR is unset: token commands need the relay's private admin listener, which is only up when the relay was started with it"
            )
        })?;
        let mut addr: SocketAddr = addr.parse().map_err(|_| {
            anyhow!("RELAY_ADMIN_ADDR must be an address and port, such as 127.0.0.1:9091")
        })?;
        if addr.ip().is_unspecified() {
            addr.set_ip(match addr {
                SocketAddr::V4(_) => std::net::Ipv4Addr::LOCALHOST.into(),
                SocketAddr::V6(_) => std::net::Ipv6Addr::LOCALHOST.into(),
            });
        }
        let secret = secret
            .map(str::trim)
            .filter(|secret| !secret.is_empty())
            .ok_or_else(|| {
                anyhow!(
                    "RELAY_ADMIN_SECRET is unset: it must be the secret the relay's admin listener was started with"
                )
            })?;
        let secret = RelayAccess::decode(secret)
            .map_err(|_| anyhow!("RELAY_ADMIN_SECRET must be a base64url 32-byte secret"))?;
        Ok(Self {
            base: format!("http://{addr}"),
            secret,
            http: reqwest::Client::builder().timeout(TIMEOUT).build()?,
        })
    }

    /// Mint a token under a fresh random ID. The relay returns only the secret;
    /// the ID is the one sent, so the two together are the `<id>.<secret>` a
    /// device pastes into its relay link.
    pub async fn create(&self) -> Result<RelayToken> {
        let mut id = [0u8; 16];
        easynotes_core::crypto::random_bytes(&mut id);
        let id = URL_SAFE_NO_PAD.encode(id);
        let body = self
            .post("/tokens", Some(serde_json::json!({ "id": &id })))
            .await?;
        let minted: Minted = serde_json::from_str(&body)
            .map_err(|e| anyhow!("the relay's answer to POST /tokens was not a secret: {e}"))?;
        let secret = RelayAccess::decode(&minted.secret).map_err(|_| {
            anyhow!("the relay answered POST /tokens with something that is not a 32-byte secret")
        })?;
        Ok(RelayToken {
            id: Some(id),
            secret,
        })
    }

    /// Revoke: the token stops working, and its ID cannot be minted again.
    pub async fn revoke(&self, id: &str) -> Result<()> {
        self.post(&format!("/tokens/{id}/revoke"), None)
            .await
            .map(|_| ())
    }

    /// Make the token read-only (`on`) or writable again (`off`).
    pub async fn read_only(&self, id: &str, read_only: bool) -> Result<()> {
        self.post(
            &format!("/tokens/{id}/read-only"),
            Some(serde_json::json!({ "read_only": read_only })),
        )
        .await
        .map(|_| ())
    }

    /// Unbind: the token goes back to being an unclaimed link, with the relay's
    /// fresh 24-hour life for one.
    pub async fn unbind(&self, id: &str) -> Result<()> {
        self.post(&format!("/tokens/{id}/unbind"), None)
            .await
            .map(|_| ())
    }

    /// One admin call. The secret stays in the header: it is not in the path,
    /// the body or the error, which carries only the status and what the relay
    /// said.
    async fn post(&self, path: &str, body: Option<serde_json::Value>) -> Result<String> {
        let mut request = self
            .http
            .post(format!("{}{path}", self.base))
            .bearer_auth(self.secret.encode());
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request
            .send()
            .await
            .map_err(|e| anyhow!("POST {path} did not reach {}: {e}", self.base))?;
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        let body = body.trim();
        if !status.is_success() {
            if body.is_empty() {
                bail!("the relay answered {status} to POST {path}");
            }
            bail!("the relay answered {status} to POST {path}: {body}");
        }
        Ok(body.to_owned())
    }
}

#[derive(Deserialize)]
struct Minted {
    secret: String,
}
