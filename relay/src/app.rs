//! The relay itself: the handshake, a socket per note and a fan-out table, and
//! the invite mailboxes in [`crate::invite`]. The socket a current app opens —
//! one per device, carrying every note — is [`crate::sync`], over the same
//! fan-out table and the same [`append`].
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
use crate::invite;
use crate::limit::RateLimit;
use crate::protocol::*;
use crate::store::RelayStore;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
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
use tokio::sync::{broadcast, OwnedMutexGuard};

/// How many pending fan-out messages a slow subscriber may fall behind by before
/// the relay stops buffering for it. It is not dropped: it is told to resubscribe
/// from its last `seq`, which is exactly the reconnect path.
pub(crate) const FANOUT_BUFFER: usize = 256;

/// `(seq, envelope)` — what one note's subscribers receive.
type Fanout = broadcast::Sender<(i64, Vec<u8>)>;

/// The write half of an upgraded WebSocket.
type Sink = futures_util::stream::SplitSink<WebSocket, Message>;

pub struct AppState {
    pub store: Arc<dyn RelayStore>,
    pub auth: Auth,
    /// One broadcast channel per note that currently has subscribers.
    channels: Mutex<HashMap<String, Fanout>>,
    /// Serializes appending and broadcasting one note's envelopes. The store
    /// assigns sequence numbers, so the corresponding fan-out must keep that
    /// same order even when concurrent websocket handlers append at once.
    append_locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

impl AppState {
    pub fn new(store: Arc<dyn RelayStore>) -> Arc<Self> {
        Arc::new(Self {
            store,
            auth: Auth::new(),
            channels: Mutex::new(HashMap::new()),
            append_locks: Mutex::new(HashMap::new()),
        })
    }

    pub(crate) fn channel(&self, note: &str) -> Fanout {
        let mut channels = self.channels.lock().unwrap();
        channels
            .entry(note.to_string())
            .or_insert_with(|| broadcast::channel(FANOUT_BUFFER).0)
            .clone()
    }

    async fn append_lock(&self, note: &str) -> OwnedMutexGuard<()> {
        let lock = {
            let mut locks = self.append_locks.lock().unwrap();
            locks
                .entry(note.to_string())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                .clone()
        };
        lock.lock_owned().await
    }
}

/// The handshake is reachable from a browser, and which browsers is now a
/// deployment's decision.
///
/// A page may POST to `/v1/challenge` and `/v1/token`. The WebSocket needs
/// nothing here — sockets are not subject to CORS — so this is only about the
/// two JSON calls that get a token, and those are a JSON POST, which means a
/// preflight rather than a simple request.
///
/// **Why any origin is a defensible default.** The relay has no cookies, no
/// sessions and no ambient authority of any kind: the only way to get a token is
/// to sign a nonce with a device's private key, which a hostile page does not
/// have. So the most a page can do by calling these is ask for a nonce for a
/// device key it names and read it back, and a nonce without the key it was
/// minted for opens nothing. There is no request a browser could be tricked into
/// making here that it could not already make from anywhere else.
///
/// **Why it is nonetheless configurable.** CORS is not what protects this relay
/// and an allowlist would not make it safe — but a public deployment that knows
/// which origin its app is served from gains nothing by advertising that any
/// page may drive its handshake, and the narrower answer is the one to give when
/// it is known. `RELAY_ALLOWED_ORIGINS` is that answer; unset is the default
/// above, which is what a development relay and the browser smoke tests need,
/// since those are served from an ephemeral localhost port.
fn browser_access(origins: &Origins) -> tower_http::cors::CorsLayer {
    use axum::http::{header, Method};
    let allow = match origins {
        Origins::Any => tower_http::cors::AllowOrigin::any(),
        Origins::List(list) => tower_http::cors::AllowOrigin::list(list.iter().cloned()),
    };
    tower_http::cors::CorsLayer::new()
        .allow_origin(allow)
        .allow_methods([Method::POST])
        .allow_headers([header::CONTENT_TYPE])
}

/// Which pages may drive the handshake.
#[derive(Clone, Debug, Default)]
pub enum Origins {
    /// Any of them. The default, and what a development relay wants.
    #[default]
    Any,
    /// Exactly these. An origin not on the list gets no
    /// `Access-Control-Allow-Origin` header, which is how a browser refuses.
    List(Vec<axum::http::HeaderValue>),
}

impl Origins {
    /// Parse a comma-separated list, as `RELAY_ALLOWED_ORIGINS` holds it. An
    /// entry that is not a legal header value is an error rather than a silently
    /// dropped rule: a typo in an allowlist that fails open is the worst of both
    /// answers.
    pub fn parse(raw: &str) -> std::result::Result<Self, String> {
        let mut list = Vec::new();
        for entry in raw.split(',') {
            let entry = entry.trim();
            if entry.is_empty() {
                continue;
            }
            let value = axum::http::HeaderValue::from_str(entry)
                .map_err(|_| format!("{entry:?} is not a usable origin"))?;
            list.push(value);
        }
        if list.is_empty() {
            return Err("no origins in the list".into());
        }
        Ok(Origins::List(list))
    }
}

/// What a deployment gets to decide. Everything here fails open, because the
/// defaults are what `just run-relay`, the tests and the browser smoke tests
/// run with, and a relay that needs configuring to work in development is a
/// relay nobody runs in development.
pub struct RelayConfig {
    pub origins: Origins,
    /// `/v1/challenge` and `/v1/token`, per address.
    pub handshake: RateLimit,
    /// Opening a socket, per address: a device's one socket at `/v1/sync`, or
    /// a note's own at `/v1/notes/{id}` from an app that predates it.
    pub sockets: RateLimit,
    /// Every invite call, per address. Its own bucket, because a guest waiting
    /// on an answer polls, and that must not spend the budget its device needs
    /// to reconnect its notes.
    pub invites: RateLimit,
}

impl Default for RelayConfig {
    fn default() -> Self {
        Self {
            origins: Origins::Any,
            handshake: RateLimit::unlimited(),
            sockets: RateLimit::unlimited(),
            invites: RateLimit::unlimited(),
        }
    }
}

impl RelayConfig {
    /// The defaults a public relay should start from, and what `main` uses
    /// unless the environment says otherwise.
    ///
    /// The numbers are deliberately loose. A current device does one handshake
    /// and opens one socket per connection, whatever it syncs; an app from
    /// before `/v1/sync` does one of each per note, so twenty notes coming back
    /// after a network drop is twenty handshakes in a few seconds — which is
    /// why the burst is larger than the sustained rate, and why it stays so
    /// while the per-note route does. They can come down once it goes. They are meant to
    /// stop a flood, not to shape traffic, and anything that starts refusing
    /// real clients is set too low rather than defended.
    pub fn public() -> Self {
        Self {
            origins: Origins::Any,
            handshake: RateLimit::new(120, 60),
            sockets: RateLimit::new(240, 120),
            // A guest and an owner each poll every few seconds while a screen
            // is waiting; this is several of those at once from one address.
            invites: RateLimit::new(120, 60),
        }
    }
}

pub fn router(state: Arc<AppState>) -> Router {
    router_with(state, RelayConfig::default())
}

pub fn router_with(state: Arc<AppState>, config: RelayConfig) -> Router {
    let cors = browser_access(&config.origins);
    let limits = Arc::new(config);
    Router::new()
        .route("/v1/challenge", post(challenge))
        .route("/v1/token", post(token))
        // One socket per device, every note on it. The per-note route below
        // stays for a release, so that an app from before it keeps working.
        .route("/v1/sync", get(crate::sync::sync_ws))
        .route("/v1/notes/{note_id}", get(notes_ws))
        .route("/v1/invites", post(invite::create))
        .route("/v1/invites/{invite}/requests", post(invite::join))
        .route(
            "/v1/invites/{invite}/requests/{request}",
            post(invite::poll),
        )
        .route(
            "/v1/invites/{invite}/requests/{request}/answer",
            post(invite::answer),
        )
        .route("/v1/invites/{invite}/list", post(invite::list))
        .route("/v1/invites/{invite}/withdraw", post(invite::withdraw))
        // Health is outside the limiter on purpose: it is what a load balancer
        // polls, it costs nothing to answer, and a relay that fails its health
        // check because it is busy gets taken out of rotation for being busy.
        .route("/v1/health", get(|| async { StatusCode::OK }))
        .layer(axum::middleware::from_fn(move |req, next| {
            let limits = limits.clone();
            async move { enforce_limits(limits, req, next).await }
        }))
        .layer(cors)
        .with_state(state)
}

/// Refuse a caller that is asking too often, before the handler does any work.
///
/// The peer address comes out of the request extensions rather than through the
/// `ConnectInfo` extractor, so that a router built without connect info — which
/// is every use of [`router`] in a test — degrades to no limiting instead of
/// failing every request with a 500.
async fn enforce_limits(
    config: Arc<RelayConfig>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let limit = match req.uri().path() {
        "/v1/challenge" | "/v1/token" => &config.handshake,
        "/v1/sync" => &config.sockets,
        path if path.starts_with("/v1/notes/") => &config.sockets,
        path if path.starts_with("/v1/invites") => &config.invites,
        _ => return next.run(req).await,
    };
    let peer = req
        .extensions()
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
        .map(|ConnectInfo(addr)| addr.ip());
    let Some(peer) = peer else {
        return next.run(req).await;
    };
    if limit.allow(peer) {
        return next.run(req).await;
    }
    tracing::debug!(%peer, path = %req.uri().path(), "rate limited");
    let retry = limit.retry_after().as_secs();
    (
        StatusCode::TOO_MANY_REQUESTS,
        [(axum::http::header::RETRY_AFTER, retry.to_string())],
    )
        .into_response()
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
    pub(crate) token: String,
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

    let head = state.store.head(note).await?;
    if since > head {
        tracing::warn!(note = %note, since, head, "client cursor is beyond relay head");
        send(&mut tx, &ServerMessage::Reset).await?;
        return Ok(());
    }

    // Subscribe to the fan-out before reading the backlog. The head check above
    // establishes the lower edge of that backlog, so an append between the two
    // is either in the store when it is read or in this receiver.
    let mut live = state.channel(note).subscribe();

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
pub(crate) async fn append(state: &Arc<AppState>, note: &str, envelope_b64: &str) -> Result<i64> {
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

    // Hold the per-note lock through both persistence and fan-out. If two
    // handlers committed in sequence order but broadcast in the opposite order,
    // subscribers would advance their cursor past an edit they had not received.
    let _append = state.append_lock(note).await;
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
