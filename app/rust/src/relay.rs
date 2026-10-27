//! Where the relay connection is made, and the one place the platforms differ.
//!
//! Not part of the Flutter-facing API — `crate::api` is, and it must look the
//! same on every target so that one set of generated bindings serves all of
//! them. A browser cannot open a native WebSocket, so the difference lives here
//! rather than as `#[cfg]` scattered through the API — and it is now only this:
//! what runs a task. A tokio runtime for native background work, and the
//! current WebAssembly instance's event loop in a tab. The engine, the outbox,
//! the cursor, the reconnect policy and the protocol are the same code in both
//! — `easynotes_core::sync`.
//!
//! **One socket per device.** A [`Device`] is one engine, and so one
//! connection to the relay and one token handshake, however many notes are
//! synced over it ([ADR 0017](../../../docs/decisions.md#adr-0017)).
//! A [`Connection`] is one note's subscription on it.

use crate::api::notes::Statuses;
use easynotes_core::ids::NoteId;
use easynotes_core::sync::{RelayEndpoint, SyncDevice, SyncEngine, SyncHandle, SyncPeer};
use std::sync::Arc;

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use std::sync::OnceLock;

    /// The app's background tasks need a runtime the bridge does not provide.
    /// One runtime is started on the first task and never shut down — a relay
    /// being changed must not take the runtime the next task needs with it.
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

    pub(crate) fn spawn<F>(work: F) -> Result<(), String>
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        runtime()?.spawn(work);
        Ok(())
    }

    /// Run a call to the relay where a call to the relay can run.
    ///
    /// The bridge's own executor runs the `async fn`s in the generated API and
    /// has no reactor, so an HTTP request started on it would never be woken.
    /// The sync runtime has one; the work goes there and the bridge awaits the
    /// handle, which any executor can.
    pub async fn on_network<F, T>(work: F) -> Result<T, String>
    where
        F: std::future::Future<Output = Result<T, String>> + Send + 'static,
        T: Send + 'static,
    {
        runtime()?
            .spawn(work)
            .await
            .map_err(|e| format!("the network call did not finish: {e}"))?
    }
}

#[cfg(target_arch = "wasm32")]
mod web {
    /// The current WebAssembly instance's event loop, instead of a tokio
    /// runtime. Ordinary bridge calls run on workers, keeping detached watcher
    /// tasks and lock-guard drops off the page's main thread.
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
    pub(crate) fn spawn<F>(work: F) -> Result<(), String>
    where
        F: std::future::Future<Output = ()> + 'static,
    {
        wasm_bindgen_futures::spawn_local(work);
        Ok(())
    }

    /// Run a call to the relay. In a tab that is simply awaiting it: `fetch`
    /// needs the page's event loop, and an `async` bridge method is already on
    /// it.
    pub async fn on_network<F, T>(work: F) -> Result<T, String>
    where
        F: std::future::Future<Output = Result<T, String>>,
    {
        work.await
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub use native::on_network;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use native::spawn;
#[cfg(target_arch = "wasm32")]
pub use web::on_network;
#[cfg(target_arch = "wasm32")]
pub(crate) use web::spawn;

/// This device's connection to one relay: one engine, one socket.
pub struct Device {
    sync: SyncDevice,
}

impl Device {
    /// Start the engine. It connects once there is a note to sync.
    pub fn open<P: SyncPeer>(
        peer: P,
        relay: &str,
        access: Option<easynotes_core::relay_access::RelayAccess>,
    ) -> Result<Self, String> {
        let endpoint = RelayEndpoint::parse(relay)
            .map_err(|e| e.to_string())?
            .with_access(access);
        let (engine, sync) = SyncEngine::device(peer, endpoint);
        spawn(engine.run())?;
        Ok(Self { sync })
    }

    /// Start syncing `note` over this device's socket and report its status to
    /// `statuses`.
    ///
    /// The report goes to the library rather than to a channel of this
    /// connection's own, so the UI subscribes once and keeps seeing the truth
    /// across a disconnect, a reconnect, or a move to a different relay — and
    /// so that the device's one status button can be the sum of every note's.
    pub fn connect(
        &self,
        note: NoteId,
        statuses: Arc<Statuses>,
        epoch: u64,
    ) -> Result<Connection, String> {
        let handle = self.sync.connect(note);
        let mut updates = handle.watch_status();
        spawn(async move {
            loop {
                let next = updates.borrow_and_update().clone();
                statuses.report(note, epoch, next.into());
                if updates.changed().await.is_err() {
                    break; // the note has been let go of
                }
            }
        })?;
        Ok(Connection { handle })
    }

    /// Close the socket, whatever is still subscribed on it.
    pub fn shutdown(&self) {
        self.sync.shutdown();
    }

    /// Request an ordered refresh barrier for this device's subscriptions.
    pub fn refresh(
        &self,
        id: String,
    ) -> impl std::future::Future<Output = std::result::Result<(), String>> {
        let sync = self.sync.clone();
        async move { sync.refresh(id).await }
    }

    pub fn set_foreground(&self, foreground: bool) {
        self.sync.set_foreground(foreground);
    }
}

/// One note's subscription on the device's socket.
pub struct Connection {
    handle: SyncHandle,
}

impl Connection {
    pub fn publish(&self, envelope: Vec<u8>) {
        self.handle.publish(envelope);
    }

    pub fn publish_soon(&self, envelope: Vec<u8>) {
        self.handle.publish_soon(envelope);
    }

    /// Act on the note's access packages now, if it is caught up.
    pub fn settle(&self) {
        self.handle.settle();
    }

    pub fn flush(&self) {
        self.handle.flush();
    }

    /// Compact the note soon, so the relay can free what was just removed
    /// from it (ADR 0027).
    pub fn reclaim(&self) {
        self.handle.reclaim();
    }

    /// Stop syncing the note. The socket stays up for the device's other notes.
    pub fn shutdown(&self) {
        self.handle.shutdown();
    }
}
