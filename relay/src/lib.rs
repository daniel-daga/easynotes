//! A dumb WebSocket relay. It stores and forwards encrypted blobs and knows
//! nothing about notes, items or people.

pub mod access;
pub mod app;
pub mod auth;
pub mod error;
pub mod invite;
pub mod limit;
pub mod media;
pub mod metrics;
pub mod protocol;
pub mod quota;
pub mod shutdown;
pub mod store;
pub mod sync;

pub use access::Access;
pub use app::{router, router_with, AppState, Origins, RelayConfig};
pub use limit::TrustedProxies;
pub use media::{Blobs, DirBlobs, MediaConfig, MediaLimits, MemoryBlobs};
pub use quota::Limits;
pub use store::{Appended, MemoryStore, Migration, PostgresStore, RelayStore, Usage, MIGRATIONS};

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

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
        mut config: RelayConfig,
    ) -> anyhow::Result<Self> {
        let state = AppState::configured(store, &mut config);
        let listener = tokio::net::TcpListener::bind(addr).await?;
        let addr = listener.local_addr()?;
        let app = router_with(state.clone(), config);
        // The listener's stop is the sessions' stop: taken from the same
        // signal, and let go of as soon as it fires, so that it is not counted
        // among the sessions `shutdown` waits for.
        let stopping = state.shutdown.watch();
        let handle = tokio::spawn(async move {
            // With connect info, because the rate limiter is keyed on the
            // address the connection came from and there is nowhere else to
            // learn it. Without this the limiter sees no peer and lets
            // everything through, which is the failure direction chosen in
            // `enforce_limits` but not the one to ship.
            let service = app.into_make_service_with_connect_info::<SocketAddr>();
            let stop = async move {
                let mut stopping = stopping;
                stopping.begun().await;
            };
            if let Err(e) = axum::serve(listener, service)
                .with_graceful_shutdown(stop)
                .await
            {
                tracing::error!(error = %e, "relay stopped");
            }
        });
        Ok(Self {
            addr,
            state,
            handle,
        })
    }

    /// Stop: take no new connections, let the requests in flight finish, and
    /// close every sync socket cleanly so its device reconnects elsewhere or
    /// later. Waits up to `drain` for all of that, then cuts off whatever is
    /// left. True if everything finished in time.
    pub async fn shutdown(mut self, drain: Duration) -> bool {
        self.state.shutdown.begin();
        let state = self.state.clone();
        let handle = &mut self.handle;
        tokio::time::timeout(drain, async move {
            let _ = handle.await;
            state.shutdown.drained().await;
        })
        .await
        .is_ok()
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
