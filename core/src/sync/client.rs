//! The wire on a device: a socket that carries text, and the handshake that
//! opens it.
//!
//! Native only — `tokio-tungstenite` for the socket and `reqwest` for the JSON
//! calls. `client_web` is the same methods over the browser's own WebSocket and
//! `fetch`, and everything the two would otherwise have to agree about is in
//! [`super::wire`] and [`super::socket`] rather than written twice.
//! [`super::engine`] drives this, and holds everything that is policy rather
//! than protocol.
//!
//! The client is deliberately dumb in the opposite direction from the relay: it
//! trusts the relay for *ordering and delivery* and for nothing else. Every
//! envelope that comes back is verified and decrypted locally before it reaches
//! the document, so a hostile relay can withhold updates or reorder them, but it
//! cannot forge one.

use super::wire::{self, ChallengeResponse, TokenResponse};
use crate::crypto::DeviceSigner;
use crate::error::Result;
use crate::relay_access::RelayAccess;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use wire::net;

/// Prove possession of the device key and get a short-lived token.
///
/// There is no account here and nothing to register: the relay learns a public
/// key, and that is the whole of the identity.
///
/// Takes a [`DeviceSigner`] rather than the identity itself, so the engine can
/// authenticate against a core held behind a lock without the seed leaving it.
pub async fn authenticate(
    http_base: &str,
    access: Option<&RelayAccess>,
    signer: &impl DeviceSigner,
) -> Result<String> {
    let device_key = signer.device_key()?;
    let device = device_key.encode();
    let http = reqwest::Client::new();

    let challenge: ChallengeResponse = post_json(
        &http,
        &wire::api(http_base, "/challenge"),
        &wire::ChallengeRequest { device: &device },
    )
    .await?;

    let nonce = wire::decode_challenge_nonce(&challenge.nonce)?;
    let signature = URL_SAFE_NO_PAD.encode(signer.sign_challenge(&nonce)?);

    let token: TokenResponse = post_json(
        &http,
        &wire::api(http_base, "/token"),
        &wire::TokenRequest {
            device: &device,
            nonce: &challenge.nonce,
            signature: &signature,
            access: access.map(|key| key.token_proof(&nonce, &device_key.0)),
        },
    )
    .await?;

    Ok(token.token)
}

/// A JSON `POST` that has to succeed. A relay that does not speak this build's
/// protocol says so here first, since the handshake is the first thing any
/// connection asks for.
async fn post_json<Q: serde::Serialize, R: serde::de::DeserializeOwned>(
    http: &reqwest::Client,
    url: &str,
    body: &Q,
) -> Result<R> {
    let response = http.post(url).json(body).send().await.map_err(net)?;
    let status = response.status().as_u16();
    let bytes = response.bytes().await.map_err(net)?;
    if let Some(outdated) = wire::refused(status, &bytes) {
        return Err(outdated);
    }
    if !(200..300).contains(&status) {
        return Err(net(format!(
            "{status} {}",
            String::from_utf8_lossy(&bytes).trim()
        )));
    }
    serde_json::from_slice(&bytes).map_err(|e| net(format!("relay sent nonsense: {e}")))
}

/// A JSON `POST`, and whatever came back, whatever its status.
///
/// The invite calls need the status — "expired" and "withdrawn" are answers,
/// not failures — so it is handed back rather than turned into an error here.
pub(crate) async fn post(url: &str, body: String) -> Result<(u16, String)> {
    let response = reqwest::Client::new()
        .post(url)
        .header("content-type", "application/json")
        .body(body)
        .send()
        .await
        .map_err(net)?;
    let status = response.status().as_u16();
    let text = response.text().await.map_err(net)?;
    Ok((status, text))
}

/// A call made as a device — its token as a bearer — and the status and body
/// that came back, whatever the status. What the attachment calls are made of:
/// a `409` or a `404` there is an answer the caller acts on.
pub(crate) async fn call(
    method: &str,
    url: &str,
    token: &str,
    request: super::media::Request,
) -> Result<(u16, Vec<u8>)> {
    let method = reqwest::Method::from_bytes(method.as_bytes())
        .map_err(|_| crate::error::CoreError::Storage(format!("no method {method}")))?;
    let mut call = reqwest::Client::new()
        .request(method, url)
        .bearer_auth(token);
    call = match request {
        super::media::Request::Empty => call,
        super::media::Request::Json(body) => {
            call.header("content-type", "application/json").body(body)
        }
        super::media::Request::Bytes(body) => call
            .header("content-type", "application/octet-stream")
            .body(body),
        super::media::Request::Range(at, len) => {
            call.header("range", format!("bytes={at}-{}", at + len - 1))
        }
    };
    let response = call.send().await.map_err(net)?;
    let status = response.status().as_u16();
    let body = response.bytes().await.map_err(net)?;
    Ok((status, body.to_vec()))
}

/// A WebSocket that carries text, and nothing that knows what the text means.
///
/// What is said over it — every note a device syncs, at `/v1/sync` — is
/// [`super::socket`], written once for both transports.
pub(crate) struct Socket {
    inner: WebSocketStream<MaybeTlsStream<TcpStream>>,
}

impl Socket {
    pub(crate) async fn open(url: &str) -> Result<Self> {
        let (inner, _) = tokio_tungstenite::connect_async(url).await.map_err(net)?;
        Ok(Self { inner })
    }

    pub(crate) async fn send(&mut self, text: String) -> Result<()> {
        self.inner
            .send(Message::Text(text.into()))
            .await
            .map_err(net)
    }

    /// The next text frame, or `None` when the relay closes.
    ///
    /// Cancel-safe, which the engine's `select!` needs: the stream buffers a
    /// partial frame internally, so dropping this future loses nothing.
    pub(crate) async fn recv(&mut self) -> Result<Option<String>> {
        while let Some(msg) = self.inner.next().await {
            match msg.map_err(net)? {
                Message::Text(t) => return Ok(Some(t.to_string())),
                Message::Close(_) => return Ok(None),
                // Ping/pong and binary frames are not part of the protocol.
                _ => continue,
            }
        }
        Ok(None)
    }

    pub(crate) async fn close(mut self) -> Result<()> {
        self.inner.close(None).await.map_err(net)
    }
}
