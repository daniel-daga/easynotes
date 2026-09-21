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
use crate::error::{CoreError, Result};
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
pub async fn authenticate(http_base: &str, signer: &impl DeviceSigner) -> Result<String> {
    let device = signer.device_key()?.encode();
    let http = reqwest::Client::new();

    let challenge: ChallengeResponse = http
        .post(format!("{http_base}/v1/challenge"))
        .json(&wire::ChallengeRequest { device: &device })
        .send()
        .await
        .map_err(net)?
        .error_for_status()
        .map_err(net)?
        .json()
        .await
        .map_err(net)?;

    let nonce = URL_SAFE_NO_PAD
        .decode(&challenge.nonce)
        .map_err(|_| CoreError::Storage("relay sent a malformed nonce".into()))?;
    let signature = URL_SAFE_NO_PAD.encode(signer.sign_challenge(&nonce)?);

    let token: TokenResponse = http
        .post(format!("{http_base}/v1/token"))
        .json(&wire::TokenRequest {
            device: &device,
            nonce: &challenge.nonce,
            signature: &signature,
        })
        .send()
        .await
        .map_err(net)?
        .error_for_status()
        .map_err(net)?
        .json()
        .await
        .map_err(net)?;

    Ok(token.token)
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

/// A WebSocket that carries text, and nothing that knows what the text means.
///
/// What is said over it — to one note at `/v1/notes/{id}` or to all of a
/// device's at `/v1/sync` — is [`super::socket`], written once for both
/// transports.
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
