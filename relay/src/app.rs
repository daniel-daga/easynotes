//! The relay itself: three endpoints and a fan-out table.
//!
//! What the relay is allowed to check, in full:
//!   * the token is valid and unexpired;
//!   * the note ID in the path is well-formed;
//!   * the envelope is under the size cap and parses as a header;
//!   * the envelope's note ID matches the path.
//!
//! It cannot check anything else, because it cannot decrypt anything, and it must
//! not try. It does not verify signatures either — only members hold the keys to
//! judge authorship, and a relay that rejected "bad" updates would be a relay that
//! could censor good ones.

use crate::auth::Auth;
use crate::error::{RelayError, Result};
use crate::protocol::*;
use crate::store::RelayStore;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Response;
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use easynotes_core::crypto::Envelope;
use easynotes_core::ids::NoteId;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::broadcast;

/// How many pending fan-out messages a slow subscriber may fall behind by before
/// the relay stops buffering for it. It is not dropped: it is told to resubscribe
/// from its last `seq`, which is exactly the reconnect path.
const FANOUT_BUFFER: usize = 256;

/// `(seq, envelope)` — what one note's subscribers receive.
type Fanout = broadcast::Sender<(i64, Vec<u8>)>;

/// The write half of an upgraded WebSocket.
type Sink = futures_util::stream::SplitSink<WebSocket, Message>;

pub struct AppState {
    pub store: Arc<dyn RelayStore>,
    pub auth: Auth,
    /// One broadcast channel per note that currently has subscribers.
    channels: Mutex<HashMap<String, Fanout>>,
}

impl AppState {
    pub fn new(store: Arc<dyn RelayStore>) -> Arc<Self> {
        Arc::new(Self {
            store,
            auth: Auth::new(),
            channels: Mutex::new(HashMap::new()),
        })
    }

    fn channel(&self, note: &str) -> Fanout {
        let mut channels = self.channels.lock().unwrap();
        channels
            .entry(note.to_string())
            .or_insert_with(|| broadcast::channel(FANOUT_BUFFER).0)
            .clone()
    }
}

/// The handshake is reachable from a browser, and that needs saying out loud.
///
/// A page served from anywhere may POST to `/v1/challenge` and `/v1/token`. The
/// WebSocket needs nothing here — sockets are not subject to CORS — so this is
/// only about the two JSON calls that get a token, and those are a JSON POST,
/// which means a preflight rather than a simple request.
///
/// **Why any origin.** The relay has no cookies, no sessions and no ambient
/// authority of any kind: the only way to get a token is to sign a nonce with a
/// device's private key, which a hostile page does not have. So the most a page
/// can do by calling these is ask for a nonce for a device key it names and read
/// it back, and a nonce without the key it was minted for opens nothing. There
/// is no request a browser could be tricked into making here that it could not
/// already make from anywhere else, and nothing it could read that is worth
/// reading.
///
/// What this does *not* do is rate-limit or restrict anything, and a public
/// relay will want both. It is on the same list as the tokens living in memory:
/// [`plan-slice-02.md`](../../docs/plan-slice-02.md) carries it.
fn browser_access() -> tower_http::cors::CorsLayer {
    use axum::http::{header, Method};
    tower_http::cors::CorsLayer::new()
        .allow_origin(tower_http::cors::Any)
        .allow_methods([Method::POST])
        .allow_headers([header::CONTENT_TYPE])
}

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/v1/challenge", post(challenge))
        .route("/v1/token", post(token))
        .route("/v1/notes/{note_id}", get(notes_ws))
        .route("/v1/health", get(|| async { StatusCode::OK }))
        .layer(browser_access())
        .with_state(state)
}

async fn challenge(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ChallengeRequest>,
) -> Result<Json<ChallengeResponse>> {
    let nonce = state
        .auth
        .challenge(&req.device, Duration::from_secs(CHALLENGE_TTL_SECS))?;
    Ok(Json(ChallengeResponse {
        nonce,
        expires_in: CHALLENGE_TTL_SECS,
    }))
}

async fn token(
    State(state): State<Arc<AppState>>,
    Json(req): Json<TokenRequest>,
) -> Result<Json<TokenResponse>> {
    let token = state.auth.redeem(
        &req.device,
        &req.nonce,
        &req.signature,
        Duration::from_secs(TOKEN_TTL_SECS),
    )?;
    Ok(Json(TokenResponse {
        token,
        expires_in: TOKEN_TTL_SECS,
    }))
}

#[derive(Deserialize)]
pub struct WsQuery {
    token: String,
}

async fn notes_ws(
    State(state): State<Arc<AppState>>,
    Path(note_id): Path<String>,
    Query(q): Query<WsQuery>,
    ws: WebSocketUpgrade,
) -> Result<Response> {
    // A malformed note ID is rejected before a socket is opened, so a typo cannot
    // create a log entry for a note that can never exist.
    NoteId::decode(&note_id).map_err(|_| RelayError::BadRequest("malformed note id".into()))?;
    if state.auth.device_for(&q.token).is_none() {
        return Err(RelayError::Unauthorized);
    }
    Ok(ws.on_upgrade(move |socket| session(socket, state, note_id)))
}

async fn session(socket: WebSocket, state: Arc<AppState>, note: String) {
    if let Err(e) = run_session(socket, state, &note).await {
        tracing::debug!(note = %note, error = %e, "session ended");
    }
}

async fn run_session(socket: WebSocket, state: Arc<AppState>, note: &str) -> anyhow::Result<()> {
    let (mut tx, mut rx) = socket.split();

    // Subscribe to the fan-out *before* reading the backlog. Do it the other way
    // round and an append landing in between is lost to this subscriber for good:
    // it is past the backlog and before the subscription. Anything that arrives
    // twice is deduplicated by `seq` below.
    let mut live = state.channel(note).subscribe();

    // The first message must be `subscribe`; it carries the cursor to replay from.
    let since = loop {
        match rx.next().await {
            Some(Ok(Message::Text(t))) => match serde_json::from_str::<ClientMessage>(&t) {
                Ok(ClientMessage::Subscribe { since }) => break since.max(0),
                Ok(_) => {
                    send_error(&mut tx, "expected subscribe first").await?;
                }
                Err(e) => send_error(&mut tx, &format!("malformed message: {e}")).await?,
            },
            Some(Ok(Message::Close(_))) | None => return Ok(()),
            Some(Ok(_)) => {}
            Some(Err(e)) => return Err(e.into()),
        }
    };

    let mut last_sent = since;
    for stored in state.store.since(note, since).await? {
        send(
            &mut tx,
            &ServerMessage::Update {
                seq: stored.seq,
                envelope: URL_SAFE_NO_PAD.encode(&stored.envelope),
            },
        )
        .await?;
        last_sent = last_sent.max(stored.seq);
    }
    send(&mut tx, &ServerMessage::Caught { seq: last_sent }).await?;

    loop {
        tokio::select! {
            // Fan-out from other sessions on this note.
            broadcast = live.recv() => match broadcast {
                Ok((seq, envelope)) => {
                    if seq > last_sent {
                        send(&mut tx, &ServerMessage::Update {
                            seq,
                            envelope: URL_SAFE_NO_PAD.encode(&envelope),
                        }).await?;
                        last_sent = seq;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    // This subscriber fell too far behind to buffer. Replay from
                    // the store rather than dropping it — the same path a
                    // reconnecting client takes.
                    tracing::debug!(note = %note, skipped = n, "subscriber lagged; replaying");
                    for stored in state.store.since(note, last_sent).await? {
                        send(&mut tx, &ServerMessage::Update {
                            seq: stored.seq,
                            envelope: URL_SAFE_NO_PAD.encode(&stored.envelope),
                        }).await?;
                        last_sent = last_sent.max(stored.seq);
                    }
                }
                Err(broadcast::error::RecvError::Closed) => return Ok(()),
            },

            incoming = rx.next() => match incoming {
                Some(Ok(Message::Text(t))) => {
                    match serde_json::from_str::<ClientMessage>(&t) {
                        Ok(ClientMessage::Append { envelope }) => {
                            match append(&state, note, &envelope).await {
                                Ok(seq) => {
                                    // The sender gets its own append back through
                                    // the broadcast, like everyone else.
                                    let _ = seq;
                                }
                                Err(e) => send_error(&mut tx, &e.to_string()).await?,
                            }
                        }
                        Ok(ClientMessage::Subscribe { .. }) => {
                            send_error(&mut tx, "already subscribed").await?;
                        }
                        Err(e) => send_error(&mut tx, &format!("malformed message: {e}")).await?,
                    }
                }
                Some(Ok(Message::Close(_))) | None => return Ok(()),
                Some(Ok(_)) => {}
                Some(Err(e)) => return Err(e.into()),
            },
        }
    }
}

/// Validate, store, fan out. The only place the relay writes anything.
async fn append(state: &Arc<AppState>, note: &str, envelope_b64: &str) -> Result<i64> {
    let raw = URL_SAFE_NO_PAD
        .decode(envelope_b64)
        .map_err(|_| RelayError::BadRequest("envelope is not base64url".into()))?;

    if raw.len() > MAX_ENVELOPE_BYTES {
        return Err(RelayError::TooLarge);
    }

    // Parse the header only. This needs no keys and yields nothing but routing
    // information the relay can already see.
    let parsed = Envelope::decode(&raw)
        .map_err(|e| RelayError::BadRequest(format!("malformed envelope: {e}")))?;
    if parsed.note_id.encode() != note {
        return Err(RelayError::BadRequest(
            "envelope is addressed to a different note".into(),
        ));
    }

    let seq = state.store.append(note, &raw).await?;
    // `send` fails only when nobody is listening, which is not an error.
    let _ = state.channel(note).send((seq, raw));
    Ok(seq)
}

async fn send(tx: &mut Sink, msg: &ServerMessage) -> anyhow::Result<()> {
    tx.send(Message::Text(serde_json::to_string(msg)?.into()))
        .await?;
    Ok(())
}

async fn send_error(tx: &mut Sink, message: &str) -> anyhow::Result<()> {
    send(
        tx,
        &ServerMessage::Error {
            message: message.to_string(),
        },
    )
    .await
}
