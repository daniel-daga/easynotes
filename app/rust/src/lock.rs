//! Taking a lock from a page's main thread.
//!
//! On the web, flutter_rust_bridge runs a `#[frb(sync)]` or `async` method on
//! the page's own thread and every other method on a Web Worker (see
//! `storage.rs`), and both reach the same core through the same `Mutex`. A
//! `Mutex` that is held waits with `Atomics.wait`, which throws on a page's
//! main thread — so a synchronous call that lands while a worker is inside the
//! core does not wait its turn, it panics. The two-tab test found it: the
//! guest's list screen asking for its notes while a worker accepted the
//! owner's answer.
//!
//! So on the web every lock in this crate is taken by asking until it is free.
//! What holds one is never waiting on the main thread — the store's writes
//! queue for Dart rather than calling it — so the wait is as short as the
//! holder's work. Natively a lock is a lock.
//!
//! This covers the locks in this crate. flutter_rust_bridge's own pool of the
//! objects Dart holds is taken the same way, by a patch in
//! `third_party/flutter_rust_bridge`. The `#[frb(sync)]` status getters read
//! [`crate::api::notes::StatusCell`]'s copied value rather than a Tokio watch
//! channel, which could block the page thread; stream methods subscribe and
//! start their watcher on a Web Worker. See ADR 0002, "Not done, and honest
//! about it".

use std::sync::{LockResult, Mutex, MutexGuard};

#[cfg(target_arch = "wasm32")]
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> LockResult<MutexGuard<'_, T>> {
    use std::sync::TryLockError;
    loop {
        match mutex.try_lock() {
            Ok(guard) => return Ok(guard),
            Err(TryLockError::Poisoned(poisoned)) => return Err(poisoned),
            Err(TryLockError::WouldBlock) => std::hint::spin_loop(),
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> LockResult<MutexGuard<'_, T>> {
    mutex.lock()
}
