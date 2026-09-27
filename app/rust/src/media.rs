//! Where this library keeps attachment bytes, whichever kind of place that is.
//!
//! Not part of the Flutter-facing API: `crate::api::notes` is, and it sees one
//! [`Store`] whatever the platform. A device keeps them in a directory beside
//! its database (`easynotes_core::attachment::store`); a tab keeps them in an
//! IndexedDB database of their own (`attachment::web` over `attachment::idb`);
//! a library that lives in memory keeps them in memory too. The transfer queue
//! takes one type, so the choice is an enum here rather than a generic
//! parameter on everything above it — the same reason the op log is a
//! `Box<dyn OpStore>`.

use easynotes_core::attachment::storage::{
    Fut, ImportHints, IncomingMedia, MediaStorage, Staged, ThumbnailInput,
};
use easynotes_core::attachment::web::{MediaRows, WebIncoming, WebMediaStore};
use easynotes_core::attachment::{Media, MediaRef, Variant};
use easynotes_core::crypto::MasterKey;
use easynotes_core::ids::{AttachmentId, NoteId};
use easynotes_core::Result;
use std::sync::Arc;

#[cfg(not(target_arch = "wasm32"))]
use easynotes_core::attachment::store::{Incoming as DirIncoming, MediaStore};

pub(crate) type Rows = Arc<dyn MediaRows>;

/// Where a library's attachment store is to be opened, decided when the
/// library is and opened on first use, because opening one in a tab has to be
/// waited for.
pub(crate) enum Home {
    /// A directory on a device's disk.
    #[cfg(not(target_arch = "wasm32"))]
    Dir(std::path::PathBuf),
    /// An IndexedDB database, by name.
    #[cfg(target_arch = "wasm32")]
    Browser(String),
    /// Memory, for a library that keeps its notes there too.
    Memory,
}

impl Home {
    pub(crate) async fn open(&self, master: MasterKey) -> Result<Store> {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Home::Dir(dir) => Ok(Store::Dir(MediaStore::open(dir, master)?)),
            #[cfg(target_arch = "wasm32")]
            Home::Browser(name) => {
                let rows: Rows = Arc::new(easynotes_core::attachment::idb::IdbRows::new(name));
                Ok(Store::Rows(WebMediaStore::open(rows, master).await?))
            }
            Home::Memory => {
                let rows: Rows = Arc::new(easynotes_core::attachment::web::MemoryRows::default());
                Ok(Store::Rows(WebMediaStore::open(rows, master).await?))
            }
        }
    }
}

pub(crate) enum Store {
    #[cfg(not(target_arch = "wasm32"))]
    Dir(MediaStore),
    Rows(WebMediaStore<Rows>),
}

pub(crate) enum Incoming {
    #[cfg(not(target_arch = "wasm32"))]
    Dir(DirIncoming),
    Rows(WebIncoming<Rows>),
}

impl Store {
    /// Seal a file handed over whole — what a browser can hand over, and what
    /// a device is given for a photo it has in memory.
    pub(crate) async fn import(
        &self,
        note: NoteId,
        name: &str,
        bytes: &[u8],
        thumbnail: Option<ThumbnailInput>,
        hints: ImportHints,
    ) -> Result<Staged> {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Store::Dir(store) => {
                store.import(note, name, bytes, bytes.len() as u64, thumbnail, hints)
            }
            Store::Rows(store) => store.import(note, name, bytes, thumbnail, hints).await,
        }
    }

    /// Seal a file on this device's disk, reading it a chunk at a time.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) async fn import_file(
        &self,
        note: NoteId,
        name: &str,
        path: &std::path::Path,
        thumbnail: Option<ThumbnailInput>,
        hints: ImportHints,
    ) -> Result<Staged> {
        let io = |e: std::io::Error| easynotes_core::CoreError::Storage(e.to_string());
        match self {
            Store::Dir(store) => {
                let file = std::fs::File::open(path).map_err(io)?;
                let len = file.metadata().map_err(io)?.len();
                store.import(note, name, file, len, thumbnail, hints)
            }
            // A library in memory has nowhere to stream to but memory.
            Store::Rows(store) => {
                let bytes = std::fs::read(path).map_err(io)?;
                store.import(note, name, &bytes, thumbnail, hints).await
            }
        }
    }

    /// A whole variant, decrypted, if it is here.
    pub(crate) async fn read(
        &self,
        note: NoteId,
        id: AttachmentId,
        media: &Media,
        variant: Variant,
    ) -> Result<Vec<u8>> {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Store::Dir(store) => store.read(note, id, media, variant),
            Store::Rows(store) => store.read(note, id, media, variant).await,
        }
    }

    pub(crate) async fn remove(&self, note: NoteId, media: &Media) -> Result<()> {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Store::Dir(store) => store.remove(note, media),
            Store::Rows(store) => store.remove(note, media).await,
        }
    }

    pub(crate) async fn purge_note(&self, note: NoteId) -> Result<()> {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Store::Dir(store) => store.purge_note(note),
            Store::Rows(store) => store.purge_note(note).await,
        }
    }

    pub(crate) async fn collect_garbage(&self, note: NoteId, live: &[&Media]) -> Result<usize> {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Store::Dir(store) => store.collect_garbage(note, live),
            Store::Rows(store) => store.collect_garbage(note, live).await,
        }
    }

    pub(crate) async fn held_notes(&self) -> Result<Vec<NoteId>> {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Store::Dir(store) => store.held_notes(),
            Store::Rows(store) => store.held_notes().await,
        }
    }

    pub(crate) async fn wipe(&self) -> Result<()> {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Store::Dir(store) => store.wipe(),
            Store::Rows(store) => store.wipe().await,
        }
    }
}

impl MediaStorage for Store {
    type Incoming = Incoming;

    fn staged(&self) -> Fut<'_, Result<Vec<Staged>>> {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Store::Dir(store) => MediaStorage::staged(store),
            Store::Rows(store) => MediaStorage::staged(store),
        }
    }

    fn settle(&self, id: AttachmentId) -> Fut<'_, Result<()>> {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Store::Dir(store) => MediaStorage::settle(store, id),
            Store::Rows(store) => MediaStorage::settle(store, id),
        }
    }

    fn discard<'a>(&'a self, staged: &'a Staged) -> Fut<'a, Result<()>> {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Store::Dir(store) => MediaStorage::discard(store, staged),
            Store::Rows(store) => MediaStorage::discard(store, staged),
        }
    }

    fn has<'a>(&'a self, note: NoteId, media: &'a MediaRef) -> Fut<'a, bool> {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Store::Dir(store) => MediaStorage::has(store, note, media),
            Store::Rows(store) => MediaStorage::has(store, note, media),
        }
    }

    fn read_sealed_chunk<'a>(
        &'a self,
        note: NoteId,
        media: &'a MediaRef,
        index: u32,
    ) -> Fut<'a, Result<Vec<u8>>> {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Store::Dir(store) => MediaStorage::read_sealed_chunk(store, note, media, index),
            Store::Rows(store) => MediaStorage::read_sealed_chunk(store, note, media, index),
        }
    }

    fn incoming<'a>(
        &'a self,
        note: NoteId,
        media: &'a MediaRef,
    ) -> Fut<'a, Result<Self::Incoming>> {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Store::Dir(store) => Box::pin(async move {
                Ok(Incoming::Dir(
                    MediaStorage::incoming(store, note, media).await?,
                ))
            }),
            Store::Rows(store) => Box::pin(async move {
                Ok(Incoming::Rows(
                    MediaStorage::incoming(store, note, media).await?,
                ))
            }),
        }
    }
}

impl IncomingMedia for Incoming {
    fn write<'a>(&'a mut self, sealed: &'a [u8]) -> Fut<'a, Result<()>> {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Incoming::Dir(incoming) => IncomingMedia::write(incoming, sealed),
            Incoming::Rows(incoming) => IncomingMedia::write(incoming, sealed),
        }
    }

    fn finish(self) -> Fut<'static, Result<()>> {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Incoming::Dir(incoming) => IncomingMedia::finish(incoming),
            Incoming::Rows(incoming) => IncomingMedia::finish(incoming),
        }
    }
}
