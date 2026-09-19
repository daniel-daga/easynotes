//! Where the relay connection is made, and the one place the platforms differ.
//!
//! Not part of the Flutter-facing API — `crate::api` is, and it must look the
//! same on every target so that one set of generated bindings serves all of
//! them. A browser cannot open a native WebSocket, so the difference lives here,
//! as a type with two implementations and one shape, rather than as `#[cfg]`
//! scattered through the API.
//!
//! Both of them now *connect*. What differs is only what drives the engine: a
//! tokio runtime the app starts on a device, and the page's own event loop in a
//! tab. The engine, the outbox, the cursor and the reconnect policy are the same
//! code in both — `easynotes_core::sync` — and so is the protocol.

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
    use easynotes_core::sync::{RelayEndpoint, SyncEngine, SyncHandle};
    use std::sync::Arc;

    /// The same engine as the native build, driven by the browser's event loop
    /// instead of a tokio runtime.
    ///
    /// There is no runtime to start and none to shut down: `spawn_local` hands
    /// the future to the JavaScript microtask queue of whichever thread we are
    /// on, and the engine's socket, its `fetch` calls and its timers are all
    /// created inside that future — so they all belong to that one thread, which
    /// is what a `WebSocket` requires.
    ///
    /// **The thread this runs on is not the thread that publishes to it.**
    /// flutter_rust_bridge runs a `#[frb(sync)]` or `async` method on the page
    /// and every other method on a Web Worker, so `Library::connect` and
    /// `Note::add_item` are on different threads. That is safe here for a
    /// reason worth naming rather than assuming: what crosses is
    /// `SyncHandle::publish`, a tokio channel, and waking the engine's task
    /// across threads works because the build has the `atomics` target feature
    /// — with it, wasm-bindgen's waker is an `Arc` that notifies through shared
    /// memory rather than an `Rc` that cannot leave its thread. Without
    /// `+atomics` this would be unsound, which is one more reason the threading
    /// flags in the justfile stay together.
    pub struct Connection {
        handle: SyncHandle,
    }

    impl Connection {
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
            wasm_bindgen_futures::spawn_local(engine.run());

            let mut updates = handle.watch_status();
            wasm_bindgen_futures::spawn_local(async move {
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

#[cfg(not(target_arch = "wasm32"))]
pub use native::Connection;
#[cfg(target_arch = "wasm32")]
pub use web::Connection;
