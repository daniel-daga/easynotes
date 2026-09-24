//! A dumb WebSocket relay. It stores and forwards encrypted blobs and knows
//! nothing about notes, items or people.

pub mod app;
pub mod auth;
pub mod error;
pub mod invite;
pub mod limit;
pub mod protocol;
pub mod store;
pub mod sync;

pub use app::{router, router_with, AppState, Origins, RelayConfig};
pub use store::{MemoryStore, PostgresStore, RelayStore};

use std::net::SocketAddr;
use std::sync::Arc;

/// A running relay, bound to an address. `serve` returns as soon as the listener
/// is up, so a test can connect without racing the server.
pub struct Relay {
    pub addr: SocketAddr,
    pub state: Arc<AppState>,
    handle: tokio::task::JoinHandle<()>,
}

impl Relay {
    /// Bind and start serving, with nothing limited and any origin allowed.
    /// Pass port 0 to let the OS choose, then read `addr` for the real one.
    ///
    /// This is the shape the tests want: a relay that answers whatever they ask
    /// it, as fast as they ask. A relay anyone can reach wants
    /// [`Relay::serve_with`].
    pub async fn serve(store: Arc<dyn RelayStore>, addr: SocketAddr) -> anyhow::Result<Self> {
        Self::serve_with(store, addr, RelayConfig::default()).await
    }

    /// Bind and start serving under `config`.
    pub async fn serve_with(
        store: Arc<dyn RelayStore>,
        addr: SocketAddr,
        config: RelayConfig,
    ) -> anyhow::Result<Self> {
        let state = AppState::new(store, config.token_signing_key);
        let listener = tokio::net::TcpListener::bind(addr).await?;
        let addr = listener.local_addr()?;
        let app = router_with(state.clone(), config);
        let handle = tokio::spawn(async move {
            // With connect info, because the rate limiter is keyed on the
            // address the connection came from and there is nowhere else to
            // learn it. Without this the limiter sees no peer and lets
            // everything through, which is the failure direction chosen in
            // `enforce_limits` but not the one to ship.
            let service = app.into_make_service_with_connect_info::<SocketAddr>();
            if let Err(e) = axum::serve(listener, service).await {
                tracing::error!(error = %e, "relay stopped");
            }
        });
        Ok(Self {
            addr,
            state,
            handle,
        })
    }

    pub fn http_base(&self) -> String {
        format!("http://{}", self.addr)
    }

    pub fn ws_base(&self) -> String {
        format!("ws://{}", self.addr)
    }
}

impl Drop for Relay {
    fn drop(&mut self) {
        self.handle.abort();
    }
}
