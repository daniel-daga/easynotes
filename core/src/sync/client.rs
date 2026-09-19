//! The wire on a device: one socket to one note, and the handshake that opens it.
//!
//! Native only — `tokio-tungstenite` for the socket and `reqwest` for the two
//! JSON calls. `client_web` is the same four methods over the browser's own
//! WebSocket and `fetch`, and everything the two would otherwise have to agree
//! about is in [`super::wire`] rather than written twice.
//! [`super::engine`] drives this, and holds everything that is policy rather
//! than protocol.
//!
//! The client is deliberately dumb in the opposite direction from the relay: it
//! trusts the relay for *ordering and delivery* and for nothing else. Every
//! envelope that comes back is verified and decrypted locally before it reaches
//! the document, so a hostile relay can withhold updates or reorder them, but it
//! cannot forge one.

use super::wire::{self, ChallengeResponse, Incoming, TokenResponse};
use crate::crypto::DeviceSigner;
use crate::error::{CoreError, Result};
use crate::ids::NoteId;
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

/// A live connection to one note.
pub struct NoteSync {
    socket: WebSocketStream<MaybeTlsStream<TcpStream>>,
    /// The highest sequence number applied. Survives a reconnect: it is what the
    /// next `subscribe` replays from.
    pub last_seq: i64,
}

impl NoteSync {
    /// Connect and subscribe from `since`. Pass the `last_seq` from a previous
    /// connection to resume; pass 0 for the whole log.
    pub async fn connect(ws_base: &str, token: &str, note: NoteId, since: i64) -> Result<Self> {
        let url = wire::note_url(ws_base, note, token);
        let (mut socket, _) = tokio_tungstenite::connect_async(&url).await.map_err(net)?;
        socket
            .send(Message::Text(wire::subscribe(since).into()))
            .await
            .map_err(net)?;
        Ok(Self {
            socket,
            last_seq: since,
        })
    }

    /// Hand an envelope to the relay. It comes back through `next` like any other,
    /// which is what keeps the local and remote paths identical.
    pub async fn append(&mut self, envelope: &[u8]) -> Result<()> {
        self.socket
            .send(Message::Text(wire::append(envelope).into()))
            .await
            .map_err(net)
    }

    /// The next thing from the relay, or `None` when it closes.
    pub async fn next(&mut self) -> Result<Option<Incoming>> {
        while let Some(msg) = self.socket.next().await {
            let msg = msg.map_err(net)?;
            let text = match msg {
                Message::Text(t) => t,
                Message::Close(_) => return Ok(None),
                // Ping/pong and binary frames are not part of the protocol.
                _ => continue,
            };
            return wire::parse(&text, &mut self.last_seq).map(Some);
        }
        Ok(None)
    }

    pub async fn close(mut self) -> Result<()> {
        self.socket.close(None).await.map_err(net)
    }
}
