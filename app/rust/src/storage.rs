//! Where a browser's writes wait between the store that made them and the Dart
//! that applies them.
//!
//! Not part of the Flutter-facing API — `crate::api` is, and everything in there
//! becomes Dart. A queue of batches is not something Dart holds a handle to; it
//! is handed the batches, by `Library::take_storage_batches`.
//!
//! Plain Rust behind a lock and not one byte of JavaScript, which is the whole
//! reason it exists. On the web, flutter_rust_bridge runs a `#[frb(sync)]` or
//! `async` method on the page's own thread and every other method on a Web
//! Worker, so the methods that write to the store and the method that drains it
//! are not on the same thread. Bytes behind a `Mutex` cross that; an
//! `IDBDatabase` does not.

use crate::api::notes::{StorageArea, StorageBatch};
use easynotes_core::store::{Change, ChangeSink};
use std::sync::{Arc, Mutex};

/// The batches, oldest first.
#[derive(Default)]
pub struct Pending(Mutex<Vec<StorageBatch>>);

impl Pending {
    /// Everything queued, and forget it.
    ///
    /// Taking a batch and then failing to write it loses it, which is why this
    /// is the only thing that empties the queue: a caller that cannot apply a
    /// batch has already been handed it, and has to say so rather than ask
    /// again.
    pub fn take(&self) -> Vec<StorageBatch> {
        std::mem::take(&mut *self.locked())
    }

    pub fn len(&self) -> u32 {
        self.locked().len() as u32
    }

    /// A poisoned lock here would mean a panic while a batch was being queued.
    /// Losing a batch is the one thing that must not happen, so the queue is
    /// read through the poison rather than unwrapped.
    fn locked(&self) -> std::sync::MutexGuard<'_, Vec<StorageBatch>> {
        match self.0.lock() {
            Ok(pending) => pending,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}

/// The store's side of the queue, for one object store.
pub struct Queue {
    pub area: StorageArea,
    pub pending: Arc<Pending>,
}

impl ChangeSink for Queue {
    fn changed(&self, changes: &[Change]) {
        self.pending.locked().push(StorageBatch {
            area: match self.area {
                StorageArea::Notes => StorageArea::Notes,
                StorageArea::Keys => StorageArea::Keys,
            },
            writes: changes.iter().map(Into::into).collect(),
        });
    }
}
