//! A dumb WebSocket relay. It stores and forwards encrypted blobs and knows
//! nothing about notes, items or people.

pub mod app;
pub mod auth;
pub mod error;
pub mod protocol;
pub mod store;

pub use app::{router, AppState};
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
    /// Bind and start serving. Pass port 0 to let the OS choose, then read
    /// `addr` for the real one.
    pub async fn serve(store: Arc<dyn RelayStore>, addr: SocketAddr) -> anyhow::Result<Self> {
        let state = AppState::new(store);
        let listener = tokio::net::TcpListener::bind(addr).await?;
        let addr = listener.local_addr()?;
        let app = router(state.clone());
        let handle = tokio::spawn(async move {
            if let Err(e) = axum::serve(listener, app).await {
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
