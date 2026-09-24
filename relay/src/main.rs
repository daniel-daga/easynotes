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

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use easynotes_relay::{MemoryStore, Origins, PostgresStore, Relay, RelayConfig, RelayStore};
use std::net::SocketAddr;
use std::sync::Arc;

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

    let relay = Relay::serve_with(store, addr, config).await?;
    tracing::info!(addr = %relay.addr, "relay listening");
    // Hold the relay until the process is killed.
    std::future::pending::<()>().await;
    Ok(())
}
