//! What the relay is doing, for whoever runs it (production readiness review,
//! F6).
//!
//! Counters live in [`Metrics`], which the handlers bump as they go; totals
//! come from the store when asked, as [`crate::store::Usage`]. [`router`]
//! serves both in the Prometheus text format at `/metrics`, on a listener of
//! its own that `main` opens only when `RELAY_METRICS_ADDR` is set. Not on the
//! public one: nothing here is secret in the way a note is, but how many notes
//! a relay holds and how close it is to refusing more is the operator's
//! business, and a public route would also sit behind the rate limiter.
//!
//! For a relay anyone can write to ([ADR 0024](../../docs/adr/0024-relay-storage-quota.md)),
//! how close a ceiling is *is* the operational signal: the example alerts in
//! `deploy/prometheus/alerts.yml` fire at 80% of each.

use crate::app::AppState;
use crate::protocol::{PROTOCOL_MAX, PROTOCOL_MIN};
use axum::extract::State;
use axum::http::header;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;
use std::fmt::Write;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::Arc;

/// Which rate limit refused a request.
#[derive(Clone, Copy)]
pub(crate) enum Bucket {
    Handshake,
    Sockets,
    Invites,
    Media,
    /// Bytes written, appends and chunks together ([ADR
    /// 0028](../../docs/adr/0028-who-may-write-to-a-relay.md) §3).
    Writes,
}

const BUCKETS: [(Bucket, &str); 5] = [
    (Bucket::Handshake, "handshake"),
    (Bucket::Sockets, "sockets"),
    (Bucket::Invites, "invites"),
    (Bucket::Media, "media"),
    (Bucket::Writes, "writes"),
];

/// Which budget refused a write ([ADR 0024](../../docs/adr/0024-relay-storage-quota.md),
/// [ADR 0027](../../docs/adr/0027-attachments.md) §4).
#[derive(Clone, Copy)]
pub(crate) enum Refused {
    LogNote,
    LogRelay,
    MediaNote,
    MediaRelay,
}

const REFUSALS: [(Refused, &str, &str); 4] = [
    (Refused::LogNote, "log", "note"),
    (Refused::LogRelay, "log", "relay"),
    (Refused::MediaNote, "media", "note"),
    (Refused::MediaRelay, "media", "relay"),
];

/// The relay's running counts. Relaxed atomics throughout: each number stands
/// alone, and a scrape that reads one a moment before another is still right
/// about both.
#[derive(Default)]
pub struct Metrics {
    sockets_open: AtomicU64,
    sockets_opened: AtomicU64,
    rate_limited: [AtomicU64; 5],
    refused: [AtomicU64; 4],
    /// By status class, 1xx to 5xx. A socket's upgrade is a 1xx.
    responses: [AtomicU64; 5],
    /// Requests by the protocol they named, from `PROTOCOL_MIN` up.
    spoken: [AtomicU64; SPOKEN],
    /// Requests refused with `426`.
    outdated: AtomicU64,
    /// Requests refused for want of the relay's access key.
    access_refused: AtomicU64,
}

/// How many protocol versions this relay speaks, and so counts.
const SPOKEN: usize = (PROTOCOL_MAX - PROTOCOL_MIN + 1) as usize;

impl Metrics {
    pub(crate) fn rate_limited(&self, bucket: Bucket) {
        self.rate_limited[bucket as usize].fetch_add(1, Relaxed);
    }

    pub(crate) fn refused(&self, why: Refused) {
        self.refused[why as usize].fetch_add(1, Relaxed);
    }

    pub(crate) fn responded(&self, status: axum::http::StatusCode) {
        let class = (status.as_u16() / 100).clamp(1, 5) as usize - 1;
        self.responses[class].fetch_add(1, Relaxed);
    }

    /// A request in protocol `p`, which the caller has checked is spoken.
    pub(crate) fn spoke(&self, p: u32) {
        self.spoken[(p - PROTOCOL_MIN) as usize].fetch_add(1, Relaxed);
    }

    pub(crate) fn outdated(&self) {
        self.outdated.fetch_add(1, Relaxed);
    }

    pub(crate) fn access_refused(&self) {
        self.access_refused.fetch_add(1, Relaxed);
    }

    /// Count a sync socket as open for as long as the guard lives.
    pub(crate) fn socket(self: &Arc<Self>) -> OpenSocket {
        self.sockets_open.fetch_add(1, Relaxed);
        self.sockets_opened.fetch_add(1, Relaxed);
        OpenSocket(self.clone())
    }
}

/// One open sync socket. Dropped however the session ends.
pub(crate) struct OpenSocket(Arc<Metrics>);

impl Drop for OpenSocket {
    fn drop(&mut self) {
        self.0.sockets_open.fetch_sub(1, Relaxed);
    }
}

/// `/metrics` alone, for the private listener.
pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/metrics", get(scrape))
        .with_state(state)
}

async fn scrape(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    (
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        render(&state).await,
    )
}

/// The whole exposition. Always a `200`: a store that does not answer is
/// `easynotes_relay_store_up 0` and no usage, which is what an alert wants to
/// see, rather than a failed scrape it cannot tell from a dead relay.
pub async fn render(state: &AppState) -> String {
    let m = &state.metrics;
    let mut out = String::new();
    let mut family = |name: &str, kind: &str, help: &str, samples: &[(String, u64)]| {
        let _ = writeln!(out, "# HELP {name} {help}");
        let _ = writeln!(out, "# TYPE {name} {kind}");
        for (labels, value) in samples {
            let _ = writeln!(out, "{name}{labels} {value}");
        }
    };
    let one = |value: u64| [(String::new(), value)];

    family(
        "easynotes_relay_build_info",
        "gauge",
        "The relay's version, as a label.",
        &[(format!("{{version=\"{}\"}}", env!("CARGO_PKG_VERSION")), 1)],
    );
    family(
        "easynotes_relay_draining",
        "gauge",
        "1 once a stop has begun.",
        &one(state.shutdown.is_draining() as u64),
    );

    let usage = match state.store.usage().await {
        Ok(usage) => Some(usage),
        Err(error) => {
            tracing::warn!(%error, "metrics: the store did not answer");
            None
        }
    };
    family(
        "easynotes_relay_store_up",
        "gauge",
        "1 if the store answered this scrape's usage query.",
        &one(usage.is_some() as u64),
    );
    if let Some(usage) = usage {
        family(
            "easynotes_relay_notes",
            "gauge",
            "Notes with a log on the relay, purged ones excluded.",
            &one(usage.notes),
        );
        family(
            "easynotes_relay_log_bytes",
            "gauge",
            "Live log bytes across every note: what the storage ceiling is measured against.",
            &one(usage.log_bytes),
        );
        family(
            "easynotes_relay_media_objects",
            "gauge",
            "Committed attachment objects.",
            &one(usage.media_objects),
        );
        family(
            "easynotes_relay_media_bytes",
            "gauge",
            "Attachment bytes: committed, doomed (no compaction lists them, deleted a week later unless asked for), and reserved by uploads in progress. All three count against the media ceiling.",
            &[
                ("{state=\"committed\"}".into(), usage.media_committed_bytes),
                ("{state=\"doomed\"}".into(), usage.media_doomed_bytes),
                ("{state=\"reserved\"}".into(), usage.media_reserved_bytes),
            ],
        );
    }
    if let Some(ceiling) = state.limits.ceiling {
        family(
            "easynotes_relay_log_ceiling_bytes",
            "gauge",
            "Live log bytes at which the relay stops starting new notes. Absent when off.",
            &one(ceiling),
        );
    }
    family(
        "easynotes_relay_log_note_limit_bytes",
        "gauge",
        "Live log bytes one note may hold.",
        &one(state.limits.note_bytes),
    );
    family(
        "easynotes_relay_media_enabled",
        "gauge",
        "1 if the relay has somewhere to keep attachments.",
        &one(state.media.blobs.is_some() as u64),
    );
    if let Some(ceiling) = state.media.limits.ceiling {
        family(
            "easynotes_relay_media_ceiling_bytes",
            "gauge",
            "Attachment bytes at which the relay stops reserving more. Absent when off.",
            &one(ceiling),
        );
    }
    family(
        "easynotes_relay_media_note_limit_bytes",
        "gauge",
        "Attachment bytes one note may hold, committed and reserved.",
        &one(state.media.limits.note_bytes),
    );

    family(
        "easynotes_relay_sockets_open",
        "gauge",
        "Sync sockets open now: roughly, devices online.",
        &one(m.sockets_open.load(Relaxed)),
    );
    family(
        "easynotes_relay_sockets_opened_total",
        "counter",
        "Sync sockets opened since the relay started.",
        &one(m.sockets_opened.load(Relaxed)),
    );
    family(
        "easynotes_relay_rate_limited_total",
        "counter",
        "Requests refused with 429, by the limit that refused them.",
        &BUCKETS.map(|(bucket, name)| {
            (
                format!("{{limit=\"{name}\"}}"),
                m.rate_limited[bucket as usize].load(Relaxed),
            )
        }),
    );
    family(
        "easynotes_relay_quota_refusals_total",
        "counter",
        "Writes refused because a note or the whole relay was at its limit.",
        &REFUSALS.map(|(why, store, scope)| {
            (
                format!("{{store=\"{store}\",scope=\"{scope}\"}}"),
                m.refused[why as usize].load(Relaxed),
            )
        }),
    );
    family(
        "easynotes_relay_protocol_requests_total",
        "counter",
        "Requests by the protocol version the client named. A version is \
         dropped only once this stops rising for it (ADR 0029 §4).",
        &std::array::from_fn::<_, SPOKEN, _>(|i| {
            (
                format!("{{protocol=\"{}\"}}", PROTOCOL_MIN as usize + i),
                m.spoken[i].load(Relaxed),
            )
        }),
    );
    family(
        "easynotes_relay_outdated_total",
        "counter",
        "Requests refused with 426 because the client's protocol is not spoken.",
        &one(m.outdated.load(Relaxed)),
    );
    family(
        "easynotes_relay_access_gated",
        "gauge",
        "1 if the relay lets in only holders of its access key (ADR 0028).",
        &one(state.access.gated() as u64),
    );
    family(
        "easynotes_relay_access_refused_total",
        "counter",
        "Token and invite requests refused with 403 for want of the access key.",
        &one(m.access_refused.load(Relaxed)),
    );
    family(
        "easynotes_relay_http_responses_total",
        "counter",
        "HTTP responses on the public listener, by status class.",
        &std::array::from_fn::<_, 5, _>(|i| {
            (
                format!("{{class=\"{}xx\"}}", i + 1),
                m.responses[i].load(Relaxed),
            )
        }),
    );
    out
}
