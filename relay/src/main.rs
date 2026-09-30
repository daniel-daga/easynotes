//! `easynotes-relay` — run a relay.
//!
//! With `DATABASE_URL` set it stores to Postgres; without it, in memory, which is
//! what `just run-relay` uses for the demo.
//!
//! Two more knobs, both for a relay that strangers can reach:
//!
//! * `RELAY_ALLOWED_ORIGINS` — a comma-separated list of the origins allowed to
//!   drive the handshake from a browser. Unset means any, which is what a
//!   development relay and the browser smoke tests need.
//! * `RELAY_RATE_LIMIT` — `off` to answer as fast as it is asked. Unset means
//!   the defaults in [`RelayConfig::public`], because a process listening on a
//!   socket should not need an environment variable set to refuse a flood.
//! * `RELAY_TOKEN_SIGNING_KEY` — a stable base64url Ed25519 seed for tokens
//!   shared across restarts and instances. Unset generates a temporary key.
//! * `RELAY_STORAGE_CEILING` — how many bytes every note's live log may hold
//!   together before the relay refuses to start new notes (appends to existing
//!   ones still go). A whole number of bytes, or `off`. Unset means
//!   [`easynotes_relay::app::DEFAULT_CEILING`], 16 GiB.
//! * `RELAY_MEDIA_DIR` — where attachment bytes live: a directory on a
//!   persistent volume, backed up with the database. Unset keeps them in
//!   memory when the log is in memory too, and turns attachments off when the
//!   log is in Postgres, rather than keep bytes a restart would lose while the
//!   database says they are there.
//! * `RELAY_MEDIA_PER_NOTE` — bytes one note's attachments may hold, reserved
//!   and committed. Unset means 1 GiB.
//! * `RELAY_MEDIA_CEILING` — bytes every note's attachments may hold together,
//!   or `off`. Unset means [`easynotes_relay::media::DEFAULT_MEDIA_CEILING`],
//!   64 GiB.
//!
//! `easynotes-relay check` does not serve: it asks the relay at `RELAY_ADDR`
//! whether it is ready (`/v1/ready`) and exits 0 if so, 1 if not. It is the
//! container health check, which the slim runtime image has no `curl` for.
//!
//! * `RELAY_DRAIN_SECONDS` — on `SIGTERM` or Ctrl-C, how long to let requests
//!   in flight finish and sync sockets close before exiting anyway. Unset means
//!   [`DEFAULT_DRAIN`]. Keep it under the grace period whatever stops the
//!   container allows (`stop_grace_period` in compose, ten seconds by default),
//!   or the process is killed before it is done.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use easynotes_relay::{
    DirBlobs, MemoryStore, Origins, PostgresStore, Relay, RelayConfig, RelayStore, TrustedProxies,
};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

/// How long a stop waits for the relay to drain, unless told otherwise. Long
/// enough for a chunk upload in flight on a slow line to finish; short enough
/// to fit inside Docker's default ten-second grace period.
const DEFAULT_DRAIN: Duration = Duration::from_secs(8);

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "easynotes_relay=info".into()),
        )
        .init();

    let addr: SocketAddr = std::env::var("RELAY_ADDR")
        .unwrap_or_else(|_| "127.0.0.1:8080".into())
        .parse()?;

    if std::env::args().nth(1).as_deref() == Some("check") {
        std::process::exit(match check(addr).await {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("not ready: {e}");
                1
            }
        });
    }

    let durable = std::env::var("DATABASE_URL").is_ok();
    let store: Arc<dyn RelayStore> = match std::env::var("DATABASE_URL") {
        Ok(url) => {
            tracing::info!("storing to Postgres");
            Arc::new(PostgresStore::connect(&url).await?)
        }
        Err(_) => {
            tracing::warn!("DATABASE_URL is unset: storing in memory, nothing survives a restart");
            Arc::new(MemoryStore::new())
        }
    };

    let mut config = match std::env::var("RELAY_RATE_LIMIT").as_deref() {
        Ok("off") => {
            tracing::warn!("RELAY_RATE_LIMIT=off: answering every caller as fast as it asks");
            RelayConfig::default()
        }
        _ => RelayConfig::public(),
    };
    match std::env::var("RELAY_TOKEN_SIGNING_KEY") {
        Ok(raw) => {
            let bytes = URL_SAFE_NO_PAD
                .decode(&raw)
                .map_err(|_| anyhow::anyhow!("RELAY_TOKEN_SIGNING_KEY must be base64url"))?;
            config.token_signing_key = bytes.try_into().map_err(|_| {
                anyhow::anyhow!("RELAY_TOKEN_SIGNING_KEY must decode to exactly 32 bytes")
            })?;
            tracing::info!("using configured relay token signing key");
        }
        Err(_) => tracing::warn!(
            "RELAY_TOKEN_SIGNING_KEY is unset: device tokens will not survive a restart and instances will not share tokens"
        ),
    }
    match std::env::var("RELAY_STORAGE_CEILING")
        .as_deref()
        .map(str::trim)
    {
        // Empty is unset: what compose passes when `.env` leaves it blank.
        Ok("") | Err(_) => {}
        Ok("off") => {
            tracing::warn!(
                "RELAY_STORAGE_CEILING=off: new notes are accepted however full the relay is"
            );
            config.storage.ceiling = None;
        }
        Ok(raw) => {
            let bytes: u64 = raw.trim().parse().map_err(|_| {
                anyhow::anyhow!("RELAY_STORAGE_CEILING must be a whole number of bytes, or off")
            })?;
            config.storage.ceiling = Some(bytes);
            tracing::info!(bytes, "new notes stop at this much stored");
        }
    }
    match std::env::var("RELAY_ALLOWED_ORIGINS") {
        Ok(raw) => {
            // A typo here fails to start rather than falling back to "any": an
            // allowlist that silently does not apply is worse than none, because
            // it is the one everybody believes in.
            config.origins = Origins::parse(&raw).map_err(|e| {
                anyhow::anyhow!("RELAY_ALLOWED_ORIGINS could not be understood: {e}")
            })?;
            tracing::info!(origins = %raw, "the handshake is allowed from these origins");
        }
        Err(_) => tracing::info!("RELAY_ALLOWED_ORIGINS is unset: any page may handshake"),
    }

    match std::env::var("RELAY_TRUSTED_PROXIES").as_deref().map(str::trim) {
        Ok(raw) if !raw.is_empty() => {
            // Like the origins, a list that cannot be read fails to start: one
            // that quietly trusted nobody would put every client behind the
            // proxy back into one bucket, and one that trusted too much would
            // let anybody pick their own address.
            config.trusted_proxies = TrustedProxies::parse(raw).map_err(|e| {
                anyhow::anyhow!("RELAY_TRUSTED_PROXIES could not be understood: {e}")
            })?;
            tracing::info!(
                proxies = raw,
                "rate limits follow X-Forwarded-For from these peers"
            );
        }
        _ => tracing::info!(
            "RELAY_TRUSTED_PROXIES is unset: rate limits are per connecting address, so a reverse proxy is one client"
        ),
    }

    match std::env::var("RELAY_MEDIA_DIR").as_deref().map(str::trim) {
        Ok(dir) if !dir.is_empty() => {
            let blobs = DirBlobs::open(dir).map_err(|e| {
                anyhow::anyhow!("RELAY_MEDIA_DIR {dir:?} could not be opened for writing: {e}")
            })?;
            config.media.blobs = Some(Arc::new(blobs));
            tracing::info!(dir, "attachments are stored here");
        }
        _ if durable => {
            tracing::warn!(
                "RELAY_MEDIA_DIR is unset: attachments are off, because their bytes would not survive a restart that the database does"
            );
            config.media.blobs = None;
        }
        _ => tracing::warn!("RELAY_MEDIA_DIR is unset: attachments are kept in memory"),
    }
    if let Some(bytes) = bytes_or_off("RELAY_MEDIA_PER_NOTE", false)? {
        config.media.limits.note_bytes = bytes.unwrap_or(u64::MAX);
        tracing::info!(bytes, "one note's attachments stop at this much");
    }
    if let Some(bytes) = bytes_or_off("RELAY_MEDIA_CEILING", true)? {
        config.media.limits.ceiling = bytes;
        match bytes {
            Some(bytes) => tracing::info!(bytes, "new attachments stop at this much stored"),
            None => tracing::warn!(
                "RELAY_MEDIA_CEILING=off: attachments are accepted however full the relay is"
            ),
        }
    }

    let drain = match std::env::var("RELAY_DRAIN_SECONDS")
        .as_deref()
        .map(str::trim)
    {
        Ok("") | Err(_) => DEFAULT_DRAIN,
        Ok(raw) => Duration::from_secs(raw.parse().map_err(|_| {
            anyhow::anyhow!("RELAY_DRAIN_SECONDS must be a whole number of seconds")
        })?),
    };

    let relay = Relay::serve_with(store, addr, config).await?;
    tracing::info!(addr = %relay.addr, "relay listening");

    stopped().await?;
    tracing::info!(
        seconds = drain.as_secs(),
        "stopping: closing sockets and finishing requests in flight"
    );
    if relay.shutdown(drain).await {
        tracing::info!("stopped cleanly");
    } else {
        tracing::warn!("stopped with work still in flight: the drain period ran out");
    }
    Ok(())
}

/// Ask the relay listening on `addr` whether it is ready. By hand over one
/// socket, because an HTTP client is a lot of dependency for one request to
/// localhost. The relay speaks HTTP/1.1 and closes when asked to, so reading to
/// the end is bounded.
async fn check(addr: SocketAddr) -> anyhow::Result<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    // Listening on every interface means reachable on this one.
    let mut addr = addr;
    if addr.ip().is_unspecified() {
        addr.set_ip(match addr {
            SocketAddr::V4(_) => std::net::Ipv4Addr::LOCALHOST.into(),
            SocketAddr::V6(_) => std::net::Ipv6Addr::LOCALHOST.into(),
        });
    }
    let asked = async {
        let mut socket = tokio::net::TcpStream::connect(addr).await?;
        socket
            .write_all(b"GET /v1/ready HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .await?;
        let mut answer = Vec::new();
        socket.read_to_end(&mut answer).await?;
        anyhow::Ok(answer)
    };
    let answer = tokio::time::timeout(Duration::from_secs(5), asked)
        .await
        .map_err(|_| anyhow::anyhow!("no answer from {addr} in five seconds"))??;
    let status = answer.split(|&b| b == b'\r').next().unwrap_or_default();
    let status = String::from_utf8_lossy(status);
    match status.split(' ').nth(1) {
        Some("200") => Ok(()),
        _ => anyhow::bail!("{addr} answered {status:?}"),
    }
}

/// Resolves on `SIGTERM` — what `docker stop` and systemd send — or on Ctrl-C.
async fn stopped() -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut term = signal(SignalKind::terminate())?;
        tokio::select! {
            _ = term.recv() => {}
            result = tokio::signal::ctrl_c() => result?,
        }
    }
    #[cfg(not(unix))]
    tokio::signal::ctrl_c().await?;
    Ok(())
}

/// A size in bytes from the environment: `None` when unset or empty, `Some(None)`
/// for `off` where that is allowed, and an error for anything else, so a typo
/// fails to start rather than silently keeping the default.
fn bytes_or_off(name: &str, off_allowed: bool) -> anyhow::Result<Option<Option<u64>>> {
    match std::env::var(name).as_deref().map(str::trim) {
        Ok("") | Err(_) => Ok(None),
        Ok("off") if off_allowed => Ok(Some(None)),
        Ok(raw) => raw.parse().map(|b| Some(Some(b))).map_err(|_| {
            anyhow::anyhow!(
                "{name} must be a whole number of bytes{}",
                if off_allowed { ", or off" } else { "" }
            )
        }),
    }
}
