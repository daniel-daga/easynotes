//! The client half of the relay protocol.
//!
//! Native only for Slice 1. The web build will implement the same two methods —
//! `connect` and `run` — over the browser's own WebSocket; the message shapes and
//! the `since` bookkeeping do not change, which is the point of keeping this
//! module thin.
//!
//! The client is deliberately dumb in the opposite direction from the relay: it
//! trusts the relay for *ordering and delivery* and for nothing else. Every
//! envelope that comes back is verified and decrypted locally before it reaches
//! the document, so a hostile relay can withhold updates or reorder them, but it
//! cannot forge one.

use crate::crypto::DeviceIdentity;
use crate::error::{CoreError, Result};
use crate::ids::NoteId;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

#[derive(Serialize)]
struct ChallengeRequest<'a> {
    device: &'a str,
}

#[derive(Deserialize)]
struct ChallengeResponse {
    nonce: String,
}

#[derive(Serialize)]
struct TokenRequest<'a> {
    device: &'a str,
    nonce: &'a str,
    signature: &'a str,
}

#[derive(Deserialize)]
struct TokenResponse {
    token: String,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ClientMessage {
    Subscribe { since: i64 },
    Append { envelope: String },
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ServerMessage {
    Update { seq: i64, envelope: String },
    Caught { seq: i64 },
    Error { message: String },
}

fn net(e: impl std::fmt::Display) -> CoreError {
    CoreError::Storage(format!("relay: {e}"))
}

/// Prove possession of the device key and get a short-lived token.
///
/// There is no account here and nothing to register: the relay learns a public
/// key, and that is the whole of the identity.
pub async fn authenticate(http_base: &str, identity: &DeviceIdentity) -> Result<String> {
    let device = identity.public_key().encode();
    let http = reqwest::Client::new();

    let challenge: ChallengeResponse = http
        .post(format!("{http_base}/v1/challenge"))
        .json(&ChallengeRequest { device: &device })
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
    let signature = URL_SAFE_NO_PAD.encode(identity.sign(&nonce));

    let token: TokenResponse = http
        .post(format!("{http_base}/v1/token"))
        .json(&TokenRequest {
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

/// What arrived from the relay.
pub enum Incoming {
    /// An envelope to hand to `NoteCore::apply_remote`.
    Envelope { seq: i64, envelope: Vec<u8> },
    /// The backlog is drained; everything after this is live.
    Caught { seq: i64 },
    /// The relay refused something. Not fatal — the connection stays up.
    Rejected(String),
}

impl NoteSync {
    /// Connect and subscribe from `since`. Pass the `last_seq` from a previous
    /// connection to resume; pass 0 for the whole log.
    pub async fn connect(ws_base: &str, token: &str, note: NoteId, since: i64) -> Result<Self> {
        let url = format!("{ws_base}/v1/notes/{}?token={token}", note.encode());
        let (mut socket, _) = tokio_tungstenite::connect_async(&url).await.map_err(net)?;
        let subscribe = serde_json::to_string(&ClientMessage::Subscribe { since })
            .expect("the subscribe message always serializes");
        socket
            .send(Message::Text(subscribe.into()))
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
        let msg = serde_json::to_string(&ClientMessage::Append {
            envelope: URL_SAFE_NO_PAD.encode(envelope),
        })
        .expect("the append message always serializes");
        self.socket
            .send(Message::Text(msg.into()))
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
            let parsed: ServerMessage = serde_json::from_str(&text)
                .map_err(|e| CoreError::Storage(format!("relay sent nonsense: {e}")))?;
            return Ok(Some(match parsed {
                ServerMessage::Update { seq, envelope } => {
                    let raw = URL_SAFE_NO_PAD.decode(&envelope).map_err(|_| {
                        CoreError::Storage("relay sent a malformed envelope".into())
                    })?;
                    self.last_seq = self.last_seq.max(seq);
                    Incoming::Envelope { seq, envelope: raw }
                }
                ServerMessage::Caught { seq } => {
                    self.last_seq = self.last_seq.max(seq);
                    Incoming::Caught { seq }
                }
                ServerMessage::Error { message } => Incoming::Rejected(message),
            }));
        }
        Ok(None)
    }

    pub async fn close(mut self) -> Result<()> {
        self.socket.close(None).await.map_err(net)
    }
}
