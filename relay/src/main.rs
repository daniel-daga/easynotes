//! `easynotes-relay` — run a relay.
//!
//! With `DATABASE_URL` set it stores to Postgres; without it, in memory, which is
//! what `just run-relay` uses for the demo.

use easynotes_relay::{MemoryStore, PostgresStore, Relay, RelayStore};
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

    let relay = Relay::serve(store, addr).await?;
    tracing::info!(addr = %relay.addr, "relay listening");
    // Hold the relay until the process is killed.
    std::future::pending::<()>().await;
    Ok(())
}
