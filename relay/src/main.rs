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
//! * `RELAY_RATE_LIMIT` — `off` to answer as fast as it is asked; the storage
//!   and media ceilings still apply. Unset means the rate limits in
//!   [`RelayConfig::public`], because a process listening on a socket should
//!   not need an environment variable set to refuse a flood.
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
//! * `RELAY_ACCESS_KEYS` — who may write here ([ADR 0028](../../docs/decisions.md#adr-0028)):
//!   one or more base64url access keys, comma separated, or `open` for anyone.
//!   More than one so a rotation can overlap. Unset is `open`, with a warning.
//!   `easynotes-relay access-key` prints a new key.
//! * `RELAY_TOKENS` — `on` to accept a per-device access token
//!   ([ADR 0038](../../docs/decisions.md#adr-0038)) at the handshake, minted by
//!   billing and checked against this relay's store. Unset or empty is off, and
//!   anything else is an error. Refuses to start with `RELAY_ACCESS_KEYS=open`;
//!   with a key configured too, either a static key or a token is enough.
//! * `RELAY_WRITE_BURST` and `RELAY_WRITE_PER_HOUR` — the write budget: bytes
//!   one client address may write at once, and sustained, every socket
//!   message and attachment together. Unset means 200 MiB and 200 MiB an hour
//!   ([`easynotes_relay::app::WRITES_BURST`]); `RELAY_WRITE_PER_HOUR=off`
//!   turns it off. Zero is refused for both: as a burst it would become one
//!   byte, which is the same as off without saying so.
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
//! * `RELAY_METRICS_ADDR` — where to serve `/metrics`, in the Prometheus text
//!   format ([`easynotes_relay::metrics`]). A listener of its own, so that it
//!   can be kept off the public network. Unset serves no metrics.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use easynotes_relay::limit::RateLimit;
use easynotes_relay::{
    Access, DirBlobs, MemoryStore, Origins, PostgresStore, Relay, RelayConfig, RelayStore,
    TrustedProxies,
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

    if std::env::args().nth(1).as_deref() == Some("access-key") {
        println!(
            "{}",
            easynotes_core::relay_access::RelayAccess::random().encode()
        );
        return Ok(());
    }

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

    let mut config = base_config(std::env::var("RELAY_RATE_LIMIT").ok().as_deref());
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

    // Before the keys, so the refusal below can see both.
    config.tokens = match std::env::var("RELAY_TOKENS").as_deref().map(str::trim) {
        Ok("") | Err(_) => false,
        Ok("on") => {
            tracing::info!("per-device access tokens are accepted");
            true
        }
        // A typo must not quietly switch off a gate an operator believes in.
        Ok(_) => anyhow::bail!("RELAY_TOKENS must be `on`, or unset for off"),
    };

    match std::env::var("RELAY_ACCESS_KEYS").as_deref().map(str::trim) {
        Ok(raw) if !raw.is_empty() => {
            // A list that cannot be read fails to start rather than falling
            // back to open: an operator who set a key believes the relay is
            // closed to strangers.
            config.access = Access::parse(raw)
                .map_err(|e| anyhow::anyhow!("RELAY_ACCESS_KEYS could not be understood: {e}"))?;
            // A token relay with `open` would take anyone who omits the token
            // ID, which is the whole gate gone.
            if config.tokens && !config.access.gated() {
                anyhow::bail!(
                    "RELAY_TOKENS=on and RELAY_ACCESS_KEYS=open: a relay with tokens must not accept anyone without one. Set a key too, or leave RELAY_ACCESS_KEYS unset for a token-only relay"
                );
            }
            match &config.access {
                Access::Keys(keys) => tracing::info!(
                    keys = keys.len(),
                    "only devices holding an access key may write here"
                ),
                Access::Open => tracing::warn!(
                    "RELAY_ACCESS_KEYS=open: anyone who can reach this relay may write to it"
                ),
            }
        }
        _ if config.tokens => {
            tracing::info!("RELAY_ACCESS_KEYS is unset: only a per-device token is accepted")
        }
        _ => tracing::warn!(
            "RELAY_ACCESS_KEYS is unset: anyone who can reach this relay may write to it. \
             Set it to a key from `easynotes-relay access-key`, or to `open` to say so on purpose"
        ),
    }
    let burst = write_budget("RELAY_WRITE_BURST", false)?.flatten();
    let per_hour = write_budget("RELAY_WRITE_PER_HOUR", true)?;
    if per_hour == Some(None) {
        tracing::warn!("RELAY_WRITE_PER_HOUR=off: no limit on how much one address writes");
        config.writes = RateLimit::unlimited();
    } else if burst.is_some() || per_hour.is_some() {
        use easynotes_relay::app::{WRITES_BURST, WRITES_PER_HOUR};
        let per_hour = per_hour.flatten().unwrap_or(WRITES_PER_HOUR.into());
        let burst = burst.unwrap_or(WRITES_BURST.into());
        let fit = |bytes: u64| u32::try_from(bytes).unwrap_or(u32::MAX);
        config.writes = RateLimit::new(fit(per_hour / 60).max(1), fit(burst));
        tracing::info!(burst, per_hour, "write budget per address");
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

    let metrics_addr: Option<SocketAddr> = match std::env::var("RELAY_METRICS_ADDR")
        .as_deref()
        .map(str::trim)
    {
        Ok("") | Err(_) => None,
        Ok(raw) => Some(raw.parse().map_err(|_| {
            anyhow::anyhow!("RELAY_METRICS_ADDR must be an address and port, such as 0.0.0.0:9090")
        })?),
    };

    let relay = Relay::serve_with(store, addr, config).await?;
    tracing::info!(addr = %relay.addr, "relay listening");

    // Dropped with the process: a scrape cut off by a stop is just a missed
    // scrape, so it is not among what a stop drains.
    if let Some(metrics_addr) = metrics_addr {
        let listener = tokio::net::TcpListener::bind(metrics_addr).await?;
        tracing::info!(addr = %listener.local_addr()?, "metrics listening");
        let app = easynotes_relay::metrics::router(relay.state.clone());
        tokio::spawn(async move {
            if let Err(e) = axum::serve(listener, app).await {
                tracing::error!(error = %e, "metrics listener stopped");
            }
        });
    }

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

/// The config before the other environment knobs are applied. `off` removes the
/// rate limits and leaves every other limit — above all the storage and media
/// ceilings — in force.
fn base_config(rate_limit: Option<&str>) -> RelayConfig {
    let mut config = RelayConfig::public();
    if rate_limit != Some("off") {
        return config;
    }
    tracing::warn!(
        "RELAY_RATE_LIMIT=off: answering every caller as fast as it asks; the storage and media ceilings still apply"
    );
    config.handshake = RateLimit::unlimited();
    config.sockets = RateLimit::unlimited();
    config.invites = RateLimit::unlimited();
    config.media_rate = RateLimit::unlimited();
    config.writes = RateLimit::unlimited();
    config
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
    parse_bytes(name, std::env::var(name).as_deref().ok(), off_allowed, true)
}

/// The write budget, where zero must fail: as a burst it silently becomes a
/// one-byte budget, which switches the limit off without saying so.
fn write_budget(name: &str, off_allowed: bool) -> anyhow::Result<Option<Option<u64>>> {
    parse_bytes(
        name,
        std::env::var(name).as_deref().ok(),
        off_allowed,
        false,
    )
}

fn parse_bytes(
    name: &str,
    value: Option<&str>,
    off_allowed: bool,
    zero_allowed: bool,
) -> anyhow::Result<Option<Option<u64>>> {
    let raw = match value.map(str::trim) {
        None | Some("") => return Ok(None),
        Some("off") if off_allowed => return Ok(Some(None)),
        Some(raw) => raw,
    };
    let or_off = if off_allowed { ", or off" } else { "" };
    let bytes: u64 = raw
        .parse()
        .map_err(|_| anyhow::anyhow!("{name} must be a whole number of bytes{or_off}"))?;
    if bytes == 0 && !zero_allowed {
        anyhow::bail!("{name} must be at least 1 byte{or_off}");
    }
    Ok(Some(Some(bytes)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use easynotes_relay::app::DEFAULT_CEILING;
    use easynotes_relay::media::DEFAULT_MEDIA_CEILING;

    /// The allowances `off` removes, listed in one place.
    fn rate_limits(config: &RelayConfig) -> [&RateLimit; 5] {
        [
            &config.handshake,
            &config.sockets,
            &config.invites,
            &config.media_rate,
            &config.writes,
        ]
    }

    /// Everything `public()` decides except the random token key: both
    /// ceilings, the rest of the limits, and the five rate limits.
    fn assert_public(config: &RelayConfig) {
        let public = RelayConfig::public();
        assert_eq!(config.storage, public.storage);
        assert_eq!(config.media.limits, public.media.limits);
        for (i, (limit, expected)) in rate_limits(config)
            .into_iter()
            .zip(rate_limits(&public))
            .enumerate()
        {
            assert!(limit == expected, "rate limit {i} changed");
        }
    }

    #[test]
    fn write_budgets_refuse_zero() {
        use parse_bytes as parse;
        for (name, off) in [("RELAY_WRITE_BURST", false), ("RELAY_WRITE_PER_HOUR", true)] {
            assert!(
                parse(name, Some("0"), off, false).is_err(),
                "{name}=0 accepted"
            );
            assert!(
                parse(name, Some(" 0 "), off, false).is_err(),
                "{name}= 0  accepted"
            );
            assert_eq!(
                parse(name, Some("1048576"), off, false).unwrap(),
                Some(Some(1048576))
            );
            for value in [None, Some(""), Some(" \t ")] {
                assert_eq!(
                    parse(name, value, off, false).unwrap(),
                    None,
                    "{name}={value:?}"
                );
            }
        }
        // `off` stays the per-hour switch and is not a burst.
        assert_eq!(
            parse("RELAY_WRITE_PER_HOUR", Some(" off "), true, false).unwrap(),
            Some(None)
        );
        assert!(parse("RELAY_WRITE_BURST", Some("off"), false, false).is_err());
        assert!(parse("RELAY_WRITE_PER_HOUR", Some("off"), false, false).is_err());
        // The media knobs still take zero: no attachments is a real setting.
        assert_eq!(
            parse("RELAY_MEDIA_CEILING", Some("0"), true, true).unwrap(),
            Some(Some(0))
        );
    }

    #[test]
    fn rate_limit_off_keeps_the_storage_and_media_ceilings() {
        let off = base_config(Some("off"));
        let public = RelayConfig::public();
        let RelayConfig {
            handshake,
            sockets,
            invites,
            storage,
            media_rate,
            media,
            writes,
            // Every field is named, so a new one fails to compile until `off`
            // says whether it keeps it.
            origins: _,
            token_signing_key: _,
            trusted_proxies: _,
            access: _,
            tokens: _,
        } = &off;
        assert_eq!(storage.ceiling, Some(DEFAULT_CEILING));
        assert_eq!(media.limits.ceiling, Some(DEFAULT_MEDIA_CEILING));
        assert_eq!(*storage, public.storage);
        assert_eq!(media.limits, public.media.limits);
        for limit in [handshake, sockets, invites, media_rate, writes] {
            assert!(limit.retry_after().is_zero(), "off left a rate limit on");
        }
    }

    #[test]
    fn anything_but_off_leaves_the_public_limits_alone() {
        for value in [None, Some(""), Some("on")] {
            assert_public(&base_config(value));
        }
        // Without this the comparisons above would pass if `public()` itself
        // were unlimited, and a limiter that never refuses anything is not a
        // limit the test can hold `off` against.
        for limit in rate_limits(&RelayConfig::public()) {
            assert!(!limit.retry_after().is_zero(), "public() is unlimited");
        }
    }
}
