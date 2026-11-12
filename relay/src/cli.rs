//! `easynotes-relay token` — the operator's side of the private admin listener
//! ([`crate::admin`]).
//!
//! A self-hosted relay has no billing to mint device tokens, so its operator
//! does: create one and hand `<id>.<secret>` over, make it read-only, unbind it
//! from a lost device, or revoke it. The relay's store is the only record, so
//! every command is an HTTP call to that listener — never a local write — and
//! `RELAY_ADMIN_ADDR` and `RELAY_ADMIN_SECRET` must be the relay's own.
//!
//! The call is by hand over one socket, as `main`'s `check` is: the listener is
//! plain HTTP on loopback and its answers are small or empty, so an HTTP client
//! and its TLS stack would be a lot of binary for four requests.

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

/// What `easynotes-relay token` was asked to do.
pub enum Command {
    Create,
    Revoke(String),
    ReadOnly(String, bool),
    Unbind(String),
}

/// Read the arguments after `token`. Nothing here touches the network or exits:
/// `main` runs the command and chooses the exit status.
pub fn parse(mut args: impl Iterator<Item = String>) -> Result<Command> {
    const USAGE: &str =
        "usage: easynotes-relay token create | revoke <id> | read-only <id> [on|off] | unbind <id>";
    let command = args.next().ok_or_else(|| anyhow!(USAGE))?;
    // The fourth slot is always empty, so a stray argument is a usage error
    // rather than silently ignored.
    match (command.as_str(), args.next(), args.next(), args.next()) {
        ("create", None, None, None) => Ok(Command::Create),
        ("revoke", Some(id), None, None) => Ok(Command::Revoke(id)),
        ("read-only", Some(id), state, None) => Ok(Command::ReadOnly(
            id,
            match state.as_deref() {
                None | Some("on") => true,
                Some("off") => false,
                Some(other) => bail!("token read-only takes on or off, not {other:?}"),
            },
        )),
        ("unbind", Some(id), None, None) => Ok(Command::Unbind(id)),
        _ => bail!(USAGE),
    }
}

/// The admin listener and the bearer secret that opens it.
pub struct AdminClient {
    addr: SocketAddr,
    secret: RelayAccess,
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
        let addr = addr
            .map(str::trim)
            .filter(|addr| !addr.is_empty())
            .ok_or_else(|| {
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
        Ok(Self { addr, secret })
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
        self.post(&format!("/tokens/{}/revoke", checked_id(id)?), None)
            .await
            .map(|_| ())
    }

    /// Make the token read-only (`on`) or writable again (`off`).
    pub async fn read_only(&self, id: &str, read_only: bool) -> Result<()> {
        self.post(
            &format!("/tokens/{}/read-only", checked_id(id)?),
            Some(serde_json::json!({ "read_only": read_only })),
        )
        .await
        .map(|_| ())
    }

    /// Unbind: the token goes back to being an unclaimed link, with the relay's
    /// fresh 24-hour life for one.
    pub async fn unbind(&self, id: &str) -> Result<()> {
        self.post(&format!("/tokens/{}/unbind", checked_id(id)?), None)
            .await
            .map(|_| ())
    }

    /// One admin call. The secret stays in the header: it is not in the path,
    /// the body or the error, which carries only the status and what the relay
    /// said.
    async fn post(&self, path: &str, body: Option<serde_json::Value>) -> Result<String> {
        let body = body.map(|body| body.to_string()).unwrap_or_default();
        let request = format!(
            "POST {path} HTTP/1.0\r\nHost: {}\r\nAuthorization: Bearer {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            self.addr,
            self.secret.encode(),
            body.len()
        );
        let answer = tokio::time::timeout(TIMEOUT, self.ask(&request))
            .await
            .map_err(|_| anyhow!("{} did not answer in five seconds", self.addr))??;
        let answer = String::from_utf8_lossy(&answer);
        let (head, body) = answer.split_once("\r\n\r\n").unwrap_or((&answer, ""));
        // "HTTP/1.0 204 No Content" → "204 No Content".
        let status = head
            .split("\r\n")
            .next()
            .unwrap_or_default()
            .split_once(' ')
            .map_or("", |(_, status)| status);
        if !status.starts_with('2') {
            let body = body.trim();
            if body.is_empty() {
                bail!("the relay answered {status} to POST {path}");
            }
            bail!("the relay answered {status} to POST {path}: {body}");
        }
        Ok(body.to_owned())
    }

    /// One request over one connection, read to the end. The listener closes
    /// when asked to, so the read is bounded.
    async fn ask(&self, request: &str) -> Result<Vec<u8>> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let exchange = async {
            let mut socket = tokio::net::TcpStream::connect(self.addr).await?;
            socket.write_all(request.as_bytes()).await?;
            let mut answer = Vec::new();
            socket.read_to_end(&mut answer).await?;
            std::io::Result::Ok(answer)
        };
        exchange
            .await
            .map_err(|e| anyhow!("cannot reach {}: {e}", self.addr))
    }
}

/// An ID as `token create` printed it: sixteen bytes, base64url. Checked before
/// it goes into a path, or `junk/../<id>` would ask the relay about another
/// token.
fn checked_id(id: &str) -> Result<&str> {
    if crate::admin::is_token_id(id) {
        Ok(id)
    } else {
        bail!("a token ID is 22 base64url characters, as printed by `token create`")
    }
}

#[derive(Deserialize)]
struct Minted {
    secret: String,
}
