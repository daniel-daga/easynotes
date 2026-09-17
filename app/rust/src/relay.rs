//! Where the relay connection is made, and the one place the platforms differ.
//!
//! Not part of the Flutter-facing API — `crate::api` is, and it must look the
//! same on every target so that one set of generated bindings serves all of
//! them. A browser cannot open a native WebSocket, so the difference lives here,
//! as a type with two implementations and one shape, rather than as `#[cfg]`
//! scattered through the API.

use crate::api::notes::Statuses;

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use super::*;
    use easynotes_core::ids::NoteId;
    use easynotes_core::sync::{RelayEndpoint, SyncEngine, SyncHandle};
    use std::sync::{Arc, OnceLock};

    /// The engine needs a reactor and the bridge does not provide one: its own
    /// executor runs the `async fn`s in the generated API, and nothing there
    /// speaks TCP. So the app gets one runtime, shared by every note, started on
    /// the first connection and never shut down — a note closing must not take
    /// another note's socket with it.
    fn runtime() -> Result<&'static tokio::runtime::Runtime, String> {
        static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
        if let Some(runtime) = RUNTIME.get() {
            return Ok(runtime);
        }
        let built = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .thread_name("easynotes-sync")
            .build()
            .map_err(|e| format!("could not start the sync runtime: {e}"))?;
        // If another thread won the race, `built` is dropped here with no tasks
        // on it, which is safe outside an async context — and we are outside one.
        Ok(RUNTIME.get_or_init(|| built))
    }

    pub struct Connection {
        handle: SyncHandle,
    }

    impl Connection {
        /// Start an engine for `note` and report its status to `statuses`.
        ///
        /// The report goes to the library rather than to a channel of this
        /// connection's own, so the UI subscribes once and keeps seeing the
        /// truth across a disconnect, a reconnect, or a move to a different
        /// relay — and so that the device's one status button can be the sum of
        /// every note's.
        pub fn open<P>(
            peer: P,
            note: NoteId,
            relay: &str,
            statuses: Arc<Statuses>,
            epoch: u64,
        ) -> Result<Self, String>
        where
            P: easynotes_core::sync::SyncPeer,
        {
            let endpoint = RelayEndpoint::parse(relay).map_err(|e| e.to_string())?;
            let (engine, handle) = SyncEngine::start(peer, note, endpoint);
            let runtime = runtime()?;
            runtime.spawn(engine.run());

            let mut updates = handle.watch_status();
            runtime.spawn(async move {
                loop {
                    let next = updates.borrow_and_update().clone();
                    statuses.report(note, epoch, next.into());
                    if updates.changed().await.is_err() {
                        break; // the engine has finished
                    }
                }
            });

            Ok(Self { handle })
        }

        pub fn publish(&self, envelope: Vec<u8>) {
            self.handle.publish(envelope);
        }

        pub fn shutdown(&self) {
            self.handle.shutdown();
        }
    }
}

#[cfg(target_arch = "wasm32")]
mod web {
    use super::*;
    use easynotes_core::ids::NoteId;
    use std::sync::Arc;

    /// A connection that is never made.
    ///
    /// The core compiles for the browser and the CRDT and crypto run there
    /// unchanged; what is missing is the transport, because the native client
    /// speaks TCP through tokio and a tab cannot. The browser's own WebSocket
    /// behind the same `NoteSync` shape is the remaining work — see
    /// `docs/adr/0002-web-core.md` — and the app says so rather than pretending
    /// to be offline.
    pub struct Connection {
        /// Never constructed, so never read. The field is what makes this the
        /// same type as the native one rather than a different shape that
        /// happens to compile.
        #[allow(dead_code)]
        unreachable: std::convert::Infallible,
    }

    impl Connection {
        pub fn open<P>(
            _peer: P,
            _note: NoteId,
            _relay: &str,
            _statuses: Arc<Statuses>,
            _epoch: u64,
        ) -> Result<Self, String> {
            Err(
                "the web build cannot reach a relay yet: its notes stay in this \
                 tab until the browser transport lands"
                    .to_string(),
            )
        }

        pub fn publish(&self, _envelope: Vec<u8>) {}

        pub fn shutdown(&self) {}
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub use native::Connection;
#[cfg(target_arch = "wasm32")]
pub use web::Connection;
