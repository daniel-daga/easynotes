//! The relay itself: the handshake, a fan-out table, and the invite mailboxes
//! in [`crate::invite`]. The socket a device opens — one, carrying every note it
//! syncs — is [`crate::sync`], over that fan-out table and [`append`].
//!
//! What the relay is allowed to check, in full:
//!   * the token is valid and unexpired;
//!   * the note ID a message names is well-formed;
//!   * the envelope is under the size cap and parses as a header;
//!   * the envelope's note ID matches the note the message names.
//!
//! It cannot check anything else, because it cannot decrypt anything, and it must
//! not try. It does not verify signatures either — only members hold the keys to
//! judge authorship, and a relay that rejected "bad" updates would be a relay that
//! could censor good ones.

use crate::access::Access;
use crate::auth::Auth;
use crate::error::{RelayError, Result};
use crate::invite;
use crate::limit::{RateLimit, TrustedProxies};
use crate::protocol::*;
use crate::quota::{Limits, Proposal, Refusal};
use crate::store::{Appended, RelayStore};
use axum::extract::{ConnectInfo, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use easynotes_core::crypto::Envelope;
use easynotes_core::ids::NoteId;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;
use tokio::sync::{broadcast, OwnedMutexGuard};

/// How many pending fan-out messages a slow subscriber may fall behind by before
/// the relay stops buffering for it. It is not dropped: it is told to resubscribe
/// from its last `seq`, which is exactly the reconnect path.
pub(crate) const FANOUT_BUFFER: usize = 256;

/// The `seq` a fan-out carries to say the topic was purged, with no envelope.
/// No stored envelope has it: sequence numbers start at 1.
pub(crate) const GONE_SEQ: i64 = -1;

/// `(seq, envelope)` — what one note's subscribers receive, or [`GONE_SEQ`].
type Fanout = broadcast::Sender<(i64, Vec<u8>)>;

/// Removes the channel when its last subscriber goes away, including when a
/// forwarder is aborted before its first poll.
pub(crate) struct Listener {
    receiver: Option<broadcast::Receiver<(i64, Vec<u8>)>>,
    state: Arc<AppState>,
    note: String,
}

impl Listener {
    pub(crate) async fn recv(
        &mut self,
    ) -> std::result::Result<(i64, Vec<u8>), broadcast::error::RecvError> {
        self.receiver.as_mut().expect("live listener").recv().await
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        let mut channels = self.state.channels.lock().unwrap();
        self.receiver.take();
        if channels
            .get(&self.note)
            .is_some_and(|channel| channel.receiver_count() == 0)
        {
            channels.remove(&self.note);
        }
    }
}

pub struct AppState {
    pub store: Arc<dyn RelayStore>,
    pub auth: Auth,
    /// How much a note may keep, and when to ask for a compaction.
    pub limits: Limits,
    /// Where attachment bytes go, and how much of them there may be.
    pub media: crate::media::MediaConfig,
    /// One broadcast channel per note that currently has subscribers.
    channels: Mutex<HashMap<String, Fanout>>,
    /// Serializes appending and broadcasting one note's envelopes. The store
    /// assigns sequence numbers, so the corresponding fan-out must keep that
    /// same order even when concurrent websocket handlers append at once.
    append_locks: Mutex<HashMap<String, Weak<tokio::sync::Mutex<()>>>>,
    /// Tells the sync sessions to close when the relay is stopping.
    pub shutdown: crate::shutdown::Shutdown,
    /// Running counts for `/metrics` ([`crate::metrics`]).
    pub metrics: Arc<crate::metrics::Metrics>,
    /// Who may get a token or use an invite ([`crate::access`]).
    pub access: Access,
    /// Bytes written per client address, appends and attachment chunks
    /// together ([ADR 0028](../../docs/adr/0028-who-may-write-to-a-relay.md) §3).
    pub writes: RateLimit,
}

impl AppState {
    pub fn new(store: Arc<dyn RelayStore>, token_signing_key: [u8; 32]) -> Arc<Self> {
        Self::with_limits(store, token_signing_key, Limits::default())
    }

    pub fn with_limits(
        store: Arc<dyn RelayStore>,
        token_signing_key: [u8; 32],
        limits: Limits,
    ) -> Arc<Self> {
        Self::with_media(
            store,
            token_signing_key,
            limits,
            crate::media::MediaConfig::default(),
        )
    }

    pub fn with_media(
        store: Arc<dyn RelayStore>,
        token_signing_key: [u8; 32],
        limits: Limits,
        media: crate::media::MediaConfig,
    ) -> Arc<Self> {
        Self::build(
            store,
            token_signing_key,
            limits,
            media,
            Access::Open,
            RateLimit::unlimited(),
        )
    }

    /// Everything of `config` the handlers read, as opposed to the limits the
    /// middleware applies. Takes the write budget out of it: a bucket has one
    /// owner.
    pub fn configured(store: Arc<dyn RelayStore>, config: &mut RelayConfig) -> Arc<Self> {
        Self::build(
            store,
            config.token_signing_key,
            config.storage,
            config.media.clone(),
            config.access.clone(),
            std::mem::replace(&mut config.writes, RateLimit::unlimited()),
        )
    }

    fn build(
        store: Arc<dyn RelayStore>,
        token_signing_key: [u8; 32],
        limits: Limits,
        media: crate::media::MediaConfig,
        access: Access,
        writes: RateLimit,
    ) -> Arc<Self> {
        Arc::new(Self {
            store,
            auth: Auth::new(token_signing_key),
            limits,
            media,
            channels: Mutex::new(HashMap::new()),
            append_locks: Mutex::new(HashMap::new()),
            shutdown: crate::shutdown::Shutdown::default(),
            metrics: Arc::default(),
            access,
            writes,
        })
    }

    pub(crate) fn subscribe(self: &Arc<Self>, note: &str) -> Listener {
        let mut channels = self.channels.lock().unwrap();
        let receiver = channels
            .entry(note.to_string())
            .or_insert_with(|| broadcast::channel(FANOUT_BUFFER).0)
            .subscribe();
        Listener {
            receiver: Some(receiver),
            state: self.clone(),
            note: note.to_string(),
        }
    }

    /// Tell every socket subscribed to `note` that it is gone.
    fn broadcast_gone(&self, note: &str) {
        self.broadcast(note, GONE_SEQ, Vec::new());
    }

    fn broadcast(&self, note: &str, seq: i64, envelope: Vec<u8>) {
        if let Some(channel) = self.channels.lock().unwrap().get(note) {
            let _ = channel.send((seq, envelope));
        }
    }

    pub(crate) async fn append_lock(&self, note: &str) -> OwnedMutexGuard<()> {
        let lock = {
            let mut locks = self.append_locks.lock().unwrap();
            // Keep only locks held by an appender or a waiter. Holding this
            // map lock through upgrade/insert prevents two locks for one note.
            locks.retain(|_, lock| lock.strong_count() > 0);
            match locks.get(note).and_then(Weak::upgrade) {
                Some(lock) => lock,
                None => {
                    let lock = Arc::new(tokio::sync::Mutex::new(()));
                    locks.insert(note.to_string(), Arc::downgrade(&lock));
                    lock
                }
            }
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
        .allow_methods([Method::GET, Method::POST, Method::PUT])
        .allow_headers([header::CONTENT_TYPE, header::AUTHORIZATION, header::RANGE])
        .expose_headers([header::CONTENT_RANGE])
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
    /// Ed25519 seed used to sign stateless device tokens. Every instance in a
    /// deployment must use the same value.
    pub token_signing_key: [u8; 32],
    /// `/v1/challenge` and `/v1/token`, per address.
    pub handshake: RateLimit,
    /// Opening a device's one socket at `/v1/sync`, per address.
    pub sockets: RateLimit,
    /// Every invite call, per address. Its own bucket, because a guest waiting
    /// on an answer polls, and that must not spend the budget its device needs
    /// to reconnect its notes.
    pub invites: RateLimit,
    /// How much one note may keep, and the whole relay
    /// ([ADR 0024](../../docs/adr/0024-relay-storage-quota.md)).
    pub storage: Limits,
    /// Every attachment call, per address. A 50 MB file is fifty-odd chunk
    /// requests, so this is looser than the rest.
    pub media_rate: RateLimit,
    /// Where attachment bytes go and how much of them there may be
    /// ([ADR 0027](../../docs/adr/0027-attachments.md) §4).
    pub media: crate::media::MediaConfig,
    /// The peers whose `X-Forwarded-For` names the client the rate limits are
    /// keyed on: the relay's own reverse proxy, and nothing else.
    pub trusted_proxies: TrustedProxies,
    /// Who may get a token or use an invite
    /// ([ADR 0028](../../docs/adr/0028-who-may-write-to-a-relay.md)).
    pub access: Access,
    /// Bytes written per address, appends and attachment chunks together.
    pub writes: RateLimit,
}

impl Default for RelayConfig {
    fn default() -> Self {
        Self {
            origins: Origins::Any,
            token_signing_key: {
                let mut key = [0; 32];
                easynotes_core::crypto::random_bytes(&mut key);
                key
            },
            handshake: RateLimit::unlimited(),
            sockets: RateLimit::unlimited(),
            invites: RateLimit::unlimited(),
            storage: Limits::default(),
            media_rate: RateLimit::unlimited(),
            media: crate::media::MediaConfig::default(),
            trusted_proxies: TrustedProxies::default(),
            access: Access::Open,
            writes: RateLimit::unlimited(),
        }
    }
}

impl RelayConfig {
    /// The defaults a public relay should start from, and what `main` uses
    /// unless the environment says otherwise.
    ///
    /// The numbers are deliberately loose. A device does one handshake — a
    /// challenge and a token, two requests — and opens one socket per
    /// connection, whatever it syncs, and asks again only when its token
    /// expires or its network drops. The bursts let twenty devices behind one
    /// address, a household or an office behind one router, all come back at
    /// once after an outage. They are meant to stop a flood, not to shape
    /// traffic, and anything that starts refusing real clients is set too low
    /// rather than defended.
    pub fn public() -> Self {
        Self {
            origins: Origins::Any,
            token_signing_key: {
                let mut key = [0; 32];
                easynotes_core::crypto::random_bytes(&mut key);
                key
            },
            handshake: RateLimit::new(60, 40),
            sockets: RateLimit::new(60, 20),
            // A guest and an owner each poll every few seconds while a screen
            // is waiting; this is several of those at once from one address.
            invites: RateLimit::new(120, 60),
            storage: Limits {
                ceiling: Some(DEFAULT_CEILING),
                ..Limits::default()
            },
            media_rate: RateLimit::new(600, 200),
            media: crate::media::MediaConfig {
                limits: crate::media::MediaLimits {
                    ceiling: Some(crate::media::DEFAULT_MEDIA_CEILING),
                    ..crate::media::MediaLimits::default()
                },
                ..crate::media::MediaConfig::default()
            },
            trusted_proxies: TrustedProxies::default(),
            // `main` sets this from `RELAY_ACCESS_KEYS`. Unset stays open, with
            // a warning; the Synology Compose file will not start without it.
            access: Access::Open,
            // A household's evening of holiday photos from behind one router
            // goes through at once; a flood slows to 200 MiB an hour.
            writes: RateLimit::new(WRITES_PER_HOUR / 60, WRITES_BURST),
        }
    }
}

/// The write budget's burst: bytes one address may write at once.
pub const WRITES_BURST: u32 = 200 << 20;

/// The write budget's sustained rate, in bytes an hour.
pub const WRITES_PER_HOUR: u32 = 200 << 20;

/// Where a public relay stops starting new notes unless told otherwise:
/// 16 GiB of live logs. ADR 0006's largest measured note compacts to under
/// 100 KiB, so this is a very large household's worth many times over, and
/// small enough to leave a home server's disk room when somebody floods it.
pub const DEFAULT_CEILING: u64 = 16 << 30;

pub fn router(state: Arc<AppState>) -> Router {
    router_with(state, RelayConfig::default())
}

pub fn router_with(state: Arc<AppState>, config: RelayConfig) -> Router {
    let cors = browser_access(&config.origins);
    let limits = Arc::new(config);
    let metrics = state.metrics.clone();
    let counted = state.metrics.clone();
    let spoken = state.metrics.clone();
    Router::new()
        .route("/v1/challenge", post(challenge))
        .route("/v1/token", post(token))
        // One socket per device, every note on it.
        .route("/v1/sync", get(crate::sync::sync_ws))
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
        .route("/v1/purge", post(purge))
        // Attachment objects (ADR 0027 §4). `status` and `commit` are static
        // segments, so they win over `{index}` in the same place.
        .route(
            "/v1/media/{note}/{object}",
            post(crate::media::reserve).get(crate::media::fetch),
        )
        .route(
            "/v1/media/{note}/{object}/status",
            get(crate::media::status),
        )
        .route(
            "/v1/media/{note}/{object}/commit",
            post(crate::media::commit),
        )
        .route(
            "/v1/media/{note}/{object}/{index}",
            axum::routing::put(crate::media::put_chunk).layer(
                axum::extract::DefaultBodyLimit::max(crate::media::MAX_PART_BYTES),
            ),
        )
        // Health is outside the limiter on purpose: it is what a load balancer
        // polls, it costs nothing to answer, and a relay that fails its health
        // check because it is busy gets taken out of rotation for being busy.
        .route("/v1/health", get(|| async { StatusCode::OK }))
        // Readiness, unlike health, asks the database. It is what monitoring
        // should alert on: a relay that is up with its store gone accepts
        // sockets and then fails every write. Outside the limiter for the same
        // reason as health.
        .route("/v1/ready", get(ready))
        .layer(axum::middleware::from_fn(move |req, next| {
            let limits = limits.clone();
            let metrics = metrics.clone();
            async move { enforce_limits(limits, metrics, req, next).await }
        }))
        // Outside the limiter, so that a client this relay cannot talk to is
        // told so without spending anyone's budget; inside CORS, so that a tab
        // can read the refusal.
        .layer(axum::middleware::from_fn(move |req, next| {
            let spoken = spoken.clone();
            async move { speak(spoken, req, next).await }
        }))
        .layer(cors)
        // Outermost, so that refusals from the limiter and CORS are counted too.
        .layer(axum::middleware::from_fn(
            move |req, next: axum::middleware::Next| {
                let counted = counted.clone();
                async move {
                    let response = next.run(req).await;
                    counted.responded(response.status());
                    response
                }
            },
        ))
        .with_state(state)
}

/// `200` with the relay's version when the store answers, `503` when it does
/// not. The reason is logged rather than returned: the endpoint is public, and
/// a database error can name hosts and users.
async fn ready(State(state): State<Arc<AppState>>) -> Response {
    let version = serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
        "protocol": ProtocolRange::SPOKEN,
    });
    match state.store.check().await {
        Ok(()) => Json(version).into_response(),
        Err(error) => {
            tracing::warn!(%error, "readiness check failed: the store did not answer");
            (StatusCode::SERVICE_UNAVAILABLE, Json(version)).into_response()
        }
    }
}

/// Refuse a client that does not speak a protocol this relay does (ADR 0029
/// §4), with the range it would have needed.
///
/// Every `/v1` route but health and readiness, which are for load balancers and
/// monitoring rather than clients, and which a client reads to learn the range
/// in the first place. A request that names no protocol at all is from a build
/// older than the version check, and is told to update like any other.
async fn speak(
    metrics: Arc<crate::metrics::Metrics>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let path = req.uri().path();
    if !path.starts_with("/v1/") || path == "/v1/health" || path == "/v1/ready" {
        return next.run(req).await;
    }
    let spoken = req.uri().query().and_then(|query| {
        query
            .split('&')
            .find_map(|pair| pair.strip_prefix("p="))
            .and_then(|p| p.parse::<u32>().ok())
    });
    match spoken {
        Some(p) if (PROTOCOL_MIN..=PROTOCOL_MAX).contains(&p) => {
            metrics.spoke(p);
            next.run(req).await
        }
        _ => {
            tracing::debug!(
                path,
                ?spoken,
                "refused a protocol this relay does not speak"
            );
            metrics.outdated();
            (
                StatusCode::UPGRADE_REQUIRED,
                Json(Outdated {
                    code: "update_app".into(),
                    protocol: ProtocolRange::SPOKEN,
                }),
            )
                .into_response()
        }
    }
}

/// The client a request is from, as the rate limits key it: its peer, or the
/// address a trusted proxy says it had. Absent on a router with no connect
/// info, which limits nothing, as [`enforce_limits`] says.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Client(pub std::net::IpAddr);

impl AppState {
    /// Spend `bytes` of `client`'s write budget. `Err` with the seconds to wait
    /// when it is spent; always `Ok` for a client nobody could name.
    pub(crate) fn spend_writes(
        &self,
        client: Option<Client>,
        bytes: usize,
    ) -> std::result::Result<(), u64> {
        let Some(Client(client)) = client else {
            return Ok(());
        };
        if self.writes.spend(client, bytes as u64) {
            return Ok(());
        }
        tracing::debug!(%client, bytes, "write budget spent");
        self.metrics.rate_limited(crate::metrics::Bucket::Writes);
        Err(self.writes.retry_after_for(bytes as u64).as_secs())
    }
}

/// Refuse a caller that is asking too often, before the handler does any work.
///
/// The peer address comes out of the request extensions rather than through the
/// `ConnectInfo` extractor, so that a router built without connect info — which
/// is every use of [`router`] in a test — degrades to no limiting instead of
/// failing every request with a 500.
async fn enforce_limits(
    config: Arc<RelayConfig>,
    metrics: Arc<crate::metrics::Metrics>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    use crate::metrics::Bucket;
    let (limit, bucket) = match req.uri().path() {
        "/v1/challenge" | "/v1/token" | "/v1/purge" => (&config.handshake, Bucket::Handshake),
        "/v1/sync" => (&config.sockets, Bucket::Sockets),
        path if path.starts_with("/v1/invites") => (&config.invites, Bucket::Invites),
        path if path.starts_with("/v1/media") => (&config.media_rate, Bucket::Media),
        _ => return next.run(req).await,
    };
    let peer = req
        .extensions()
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
        .map(|ConnectInfo(addr)| addr.ip());
    let Some(peer) = peer else {
        return next.run(req).await;
    };
    // A value that is not text cannot be skipped, since nobody can say which
    // hops it held: it stands in as an entry that stops the walk there.
    let client = config.trusted_proxies.client(
        peer,
        req.headers()
            .get_all("x-forwarded-for")
            .iter()
            .map(|v| v.to_str().unwrap_or("unreadable")),
    );
    if limit.allow(client) {
        // The handlers that spend the write budget need to know whose.
        let mut req = req;
        req.extensions_mut().insert(Client(client));
        return next.run(req).await;
    }
    tracing::debug!(%client, %peer, path = %req.uri().path(), "rate limited");
    metrics.rate_limited(bucket);
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
    let (nonce, device) = state.auth.issue_challenge(&req.device)?;
    let raw_nonce = URL_SAFE_NO_PAD
        .decode(&nonce)
        .expect("the auth layer returned base64url");
    state
        .store
        .save_auth_challenge(
            &raw_nonce,
            &device,
            crate::auth::unix_now() as i64 + CHALLENGE_TTL_SECS as i64,
        )
        .await?;
    Ok(Json(ChallengeResponse {
        nonce,
        expires_in: CHALLENGE_TTL_SECS,
    }))
}

async fn token(
    State(state): State<Arc<AppState>>,
    Json(req): Json<TokenRequest>,
) -> Result<Json<TokenResponse>> {
    if state.access.gated() {
        // Before the challenge is spent, so a device that is asked for the key
        // and is given it can try again without starting over; a nonce is good
        // for a minute and for one token either way.
        let bytes = |raw: &str| URL_SAFE_NO_PAD.decode(raw).ok();
        let (Some(nonce), Some(device)) = (bytes(&req.nonce), bytes(&req.device)) else {
            return Err(RelayError::BadRequest(
                "nonce or device is not base64url".into(),
            ));
        };
        let device: [u8; 32] = device
            .try_into()
            .map_err(|_| RelayError::BadRequest("device key is not 32 bytes".into()))?;
        if let Err(refused) = state
            .access
            .check_token(&nonce, &device, req.access.as_deref())
        {
            state.metrics.access_refused();
            return Err(refused);
        }
    }
    let token = state
        .auth
        .redeem(
            state.store.as_ref(),
            &req.device,
            &req.nonce,
            &req.signature,
            Duration::from_secs(TOKEN_TTL_SECS),
        )
        .await?;
    Ok(Json(TokenResponse {
        token,
        expires_in: TOKEN_TTL_SECS,
    }))
}

#[derive(Deserialize)]
pub struct WsQuery {
    pub(crate) token: String,
}

/// An append that was stored.
pub(crate) struct Accepted {
    /// The note's log has grown enough that the appender should compact it.
    pub crowded: bool,
}

/// Validate, store, fan out. The only place the relay writes an envelope a
/// device sent; [`compact`] is the only place it removes any.
pub(crate) async fn append(
    state: &Arc<AppState>,
    note: &str,
    envelope_b64: &str,
) -> Result<Accepted> {
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
    match state.store.append_within(note, &raw, &state.limits).await? {
        Appended::Stored { seq, crowded } => {
            // `send` fails only when nobody is listening, which is not an error.
            state.broadcast(note, seq, raw);
            Ok(Accepted { crowded })
        }
        Appended::NoteFull => {
            state.metrics.refused(crate::metrics::Refused::LogNote);
            Err(RelayError::NoteFull)
        }
        Appended::RelayFull => {
            state.metrics.refused(crate::metrics::Refused::LogRelay);
            Err(RelayError::RelayFull)
        }
        Appended::Gone => Err(RelayError::TopicGone),
    }
}

/// Purge topics ([ADR 0026](../../docs/adr/0026-deleting-an-account.md)).
///
/// Each under its own append lock, so an append racing a purge lands wholly
/// before it (and is deleted) or finds the topic gone. Subscribers are told
/// once the tombstone is written.
async fn purge(
    State(state): State<Arc<AppState>>,
    Json(request): Json<PurgeRequest>,
) -> Result<Json<PurgeResponse>> {
    if request.topics.len() > MAX_PURGE_TOPICS {
        return Err(RelayError::BadRequest(format!(
            "at most {MAX_PURGE_TOPICS} topics per request"
        )));
    }
    let mut response = PurgeResponse::default();
    for topic in request.topics {
        let refuse = |reason: &str| PurgeRefused {
            note: topic.note.clone(),
            reason: reason.to_string(),
        };
        if NoteId::decode(&topic.note).is_err() {
            response.refused.push(refuse("malformed note id"));
            continue;
        }
        let key = URL_SAFE_NO_PAD
            .decode(&topic.key)
            .ok()
            .and_then(|k| <[u8; 32]>::try_from(k).ok());
        let signature = URL_SAFE_NO_PAD
            .decode(&topic.signature)
            .ok()
            .and_then(|s| <[u8; easynotes_core::crypto::SIG_LEN]>::try_from(s).ok());
        let (Some(key), Some(signature)) = (key, signature) else {
            response
                .refused
                .push(refuse("a key or signature of the wrong form"));
            continue;
        };
        let _append = state.append_lock(&topic.note).await;
        match state.store.purge(&topic.note, key, signature).await? {
            Ok(()) => {
                // The metadata went in the same transaction; the bytes follow.
                // A crash in between leaves bytes nothing names, which only a
                // later purge of the same topic removes — never bytes a
                // reader is told exist.
                if let Some(blobs) = &state.media.blobs {
                    blobs.remove_note(&topic.note).await?;
                }
                state.broadcast_gone(&topic.note)
            }
            Err(refusal) => response.refused.push(refuse(refusal.as_str())),
        }
    }
    Ok(Json(response))
}

/// Check a compaction, apply it, and fan the base out like any append.
///
/// Every subscriber gets the base, the ones already caught up included: it is
/// a merge that changes nothing for them, and one snapshot per compaction is
/// cheaper than telling who needs it. One whose cursor is below `covers`
/// finds the envelopes the base replaced gone and the base in their place.
pub(crate) async fn compact(
    state: &Arc<AppState>,
    note: &str,
    covers: i64,
    base_b64: &str,
    key_b64: &str,
    signature_b64: &str,
) -> std::result::Result<i64, String> {
    let decode = |raw: &str| URL_SAFE_NO_PAD.decode(raw).ok();
    let (Some(base), Some(key), Some(signature)) =
        (decode(base_b64), decode(key_b64), decode(signature_b64))
    else {
        return Err("not base64url".into());
    };
    if base.len() > MAX_ENVELOPE_BYTES {
        return Err("the base is too large".into());
    }
    let (Ok(key), Ok(signature)) = (key.try_into(), signature.try_into()) else {
        return Err("a key or signature of the wrong length".into());
    };
    let proposal = Proposal {
        covers,
        base: &base,
        key,
        signature,
    };
    let _append = state.append_lock(note).await;
    match state.store.compact(note, &proposal).await {
        Ok(Ok(seq)) => {
            state.broadcast(note, seq, base);
            Ok(seq)
        }
        Ok(Err(refusal)) => Err(Refusal::as_str(refusal).to_string()),
        Err(e) => Err(e.to_string()),
    }
}

#[cfg(test)]
mod lifetime_tests {
    use super::*;

    fn state() -> Arc<AppState> {
        AppState::new(Arc::new(crate::store::MemoryStore::new()), [0; 32])
    }

    #[tokio::test]
    async fn subscription_churn_releases_channels_without_disconnect() {
        let state = state();
        let mut keeper = state.subscribe("kept");
        for i in 0..10_000 {
            let listener = state.subscribe(&i.to_string());
            drop(listener);
            assert_eq!(state.channels.lock().unwrap().len(), 1);
        }
        state.broadcast("kept", 1, vec![1]);
        assert_eq!(keeper.recv().await.unwrap(), (1, vec![1]));
        drop(keeper);
        assert!(state.channels.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn dropping_one_listener_preserves_the_other_and_resubscription() {
        let state = state();
        let first = state.subscribe("note");
        let mut second = state.subscribe("note");
        drop(first);
        state.broadcast("note", 1, vec![1]);
        assert_eq!(second.recv().await.unwrap().0, 1);
        drop(second);
        let mut third = state.subscribe("note");
        state.broadcast("note", 2, vec![2]);
        assert_eq!(third.recv().await.unwrap().0, 2);
    }

    #[tokio::test]
    async fn aborting_a_forwarder_releases_its_listener() {
        let state = state();
        let mut listener = state.subscribe("note");
        let task = tokio::spawn(async move { listener.recv().await });
        task.abort();
        let _ = task.await;
        assert!(state.channels.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn append_lock_churn_is_bounded_and_waiters_keep_the_same_lock() {
        let state = state();
        for i in 0..10_000 {
            drop(state.append_lock(&i.to_string()).await);
            assert!(state.append_locks.lock().unwrap().len() <= 1);
        }
        let guard = state.append_lock("note").await;
        let waiting = state.append_lock("note");
        tokio::pin!(waiting);
        assert!(
            tokio::time::timeout(Duration::from_millis(10), &mut waiting)
                .await
                .is_err()
        );
        drop(state.append_lock("other").await);
        drop(guard);
        let guard = waiting.await;
        assert!(
            tokio::time::timeout(Duration::from_millis(10), state.append_lock("note"))
                .await
                .is_err()
        );
        drop(guard);
        drop(state.append_lock("note").await);
    }
}
