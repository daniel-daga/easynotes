//! Where a tab keeps attachment bytes: rows in a key-value store it has to
//! wait for ([ADR 0027](../../../docs/adr/0027-attachments.md) §3).
//!
//! The same promises as the directory in [`super::store`], kept with the tools
//! a browser has. IndexedDB has no files to rename and no `fsync`, but it has
//! transactions that are all-or-nothing and that it tells you have committed —
//! and that is enough, because every commit point the directory makes with a
//! rename is made here with one transaction:
//!
//! ```text
//! o/<note>/<object>/<index>   one sealed chunk, exactly as it travels
//! w/<note>/<object>           "whole": the object's sealed length; present
//!                             only once every chunk is, and checked
//! s/<seq>/<attachment>        a staging record, sealed under the master key
//! ```
//!
//! A chunk is written in a transaction of its own, so a 50 MB file is fifty
//! small transactions and never fifty megabytes in one. Chunks without a
//! `w/` row are the directory's `tmp/`: nothing reads them, and opening the
//! store deletes them. An import's last transaction writes its objects' `w/`
//! rows *and* its staging record together, so there is no moment at which a
//! record names an object that is not whole, nor one at which a whole object
//! is named by nothing and a collection could take it.
//!
//! Unlike [`crate::store::WebStore`], nothing here is loaded at open and
//! nothing is written without being awaited. That store holds every row in
//! memory because the core above it is synchronous; a file is not, and a tab
//! that read every photo at startup would not start.
//!
//! The rules are ordinary Rust over [`MediaRows`], so `cargo test` holds them
//! to the same tests as the directory with [`MemoryRows`]. What the browser
//! adds is three operations and no rules, in [`super::idb`].

use super::seal::{seal_whole, verify_sealed, MediaOpener, MediaSealer, Variant, CHUNK_OVERHEAD};
use super::storage::{
    inspect_original, inspect_thumbnail, objects_of, open_record, refused, seal_record, variant_of,
    Fut, ImportHints, IncomingMedia, MediaStorage, Staged, ThumbnailInput,
};
use super::{AttachmentKind, ContentKey, Media, MediaRef, Thumbnail, CHUNK_SIZE, MEDIA_VERSION};
use crate::crypto::MasterKey;
use crate::error::{CoreError, Result};
use crate::ids::{AttachmentId, MediaObjectId, NoteId};
use crate::store::web::Change;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// A key-value store of byte rows under string keys, ordered by key.
///
/// **[`apply`](MediaRows::apply) is one transaction, and returns once it has
/// committed.** That is the whole of what the store above relies on, and it is
/// stronger than what [`crate::store::ChangeSink`] promises, because here the
/// caller waits.
pub trait MediaRows: Send + Sync + 'static {
    fn get<'a>(&'a self, key: &'a str) -> Fut<'a, Result<Option<Vec<u8>>>>;

    /// Every key that begins with `prefix`, in order.
    fn keys<'a>(&'a self, prefix: &'a str) -> Fut<'a, Result<Vec<String>>>;

    fn apply(&self, changes: Vec<Change>) -> Fut<'_, Result<()>>;
}

/// Shared rows: what lets a test open a second store over the first one's
/// database, as a reload does.
impl<R: MediaRows + ?Sized> MediaRows for Arc<R> {
    fn get<'a>(&'a self, key: &'a str) -> Fut<'a, Result<Option<Vec<u8>>>> {
        (**self).get(key)
    }

    fn keys<'a>(&'a self, prefix: &'a str) -> Fut<'a, Result<Vec<String>>> {
        (**self).keys(prefix)
    }

    fn apply(&self, changes: Vec<Change>) -> Fut<'_, Result<()>> {
        (**self).apply(changes)
    }
}

/// Rows in memory. What the tests use, and what a tab with no IndexedDB gets:
/// attachments that last as long as the tab does.
#[derive(Default)]
pub struct MemoryRows {
    rows: Mutex<BTreeMap<String, Vec<u8>>>,
    refuse: AtomicBool,
}

impl MemoryRows {
    /// Every row, for a test to look through for plaintext.
    pub fn snapshot(&self) -> BTreeMap<String, Vec<u8>> {
        self.locked().clone()
    }

    /// Refuse every write from now on, as a full or cleared database does.
    pub fn refuse_writes(&self, refuse: bool) {
        self.refuse.store(refuse, Ordering::SeqCst);
    }

    /// Taken by asking until it is free in a browser, where a lock that
    /// waits throws on the page's main thread (`app/rust/src/lock.rs`).
    fn locked(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, Vec<u8>>> {
        #[cfg(target_arch = "wasm32")]
        loop {
            match self.rows.try_lock() {
                Ok(guard) => return guard,
                Err(std::sync::TryLockError::Poisoned(e)) => return e.into_inner(),
                Err(std::sync::TryLockError::WouldBlock) => std::hint::spin_loop(),
            }
        }
        #[cfg(not(target_arch = "wasm32"))]
        self.rows.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl MediaRows for MemoryRows {
    fn get<'a>(&'a self, key: &'a str) -> Fut<'a, Result<Option<Vec<u8>>>> {
        Box::pin(std::future::ready(Ok(self.locked().get(key).cloned())))
    }

    fn keys<'a>(&'a self, prefix: &'a str) -> Fut<'a, Result<Vec<String>>> {
        let keys = self
            .locked()
            .range(prefix.to_owned()..)
            .take_while(|(k, _)| k.starts_with(prefix))
            .map(|(k, _)| k.clone())
            .collect();
        Box::pin(std::future::ready(Ok(keys)))
    }

    fn apply(&self, changes: Vec<Change>) -> Fut<'_, Result<()>> {
        if self.refuse.load(Ordering::SeqCst) {
            return Box::pin(std::future::ready(Err(CoreError::Storage(
                "the browser refused to store this".into(),
            ))));
        }
        let mut rows = self.locked();
        for change in changes {
            match change {
                Change::Put { key, value } => {
                    rows.insert(key, value);
                }
                Change::Delete { key } => {
                    rows.remove(&key);
                }
                Change::DeletePrefix { prefix } => {
                    rows.retain(|k, _| !k.starts_with(&prefix));
                }
            }
        }
        Box::pin(std::future::ready(Ok(())))
    }
}

// ---------------------------------------------------------------------------
// The key layout
// ---------------------------------------------------------------------------

fn object_prefix(note: NoteId, object: MediaObjectId) -> String {
    format!("o/{}/{}/", note.encode(), object.encode())
}

fn chunk_key(note: NoteId, object: MediaObjectId, index: u32) -> String {
    // Zero-padded hex, so the store's own order is the chunks' order.
    format!("{}{index:08x}", object_prefix(note, object))
}

fn whole_key(note: NoteId, object: MediaObjectId) -> String {
    format!("w/{}/{}", note.encode(), object.encode())
}

fn record_key(seq: u64, id: AttachmentId) -> String {
    format!("s/{seq:016x}/{}", id.encode())
}

/// `s/<seq>/<attachment>` back into its parts.
fn parse_record_key(key: &str) -> Option<(u64, AttachmentId)> {
    let mut parts = key.strip_prefix("s/")?.splitn(2, '/');
    let seq = u64::from_str_radix(parts.next()?, 16).ok()?;
    let id = AttachmentId::decode(parts.next()?).ok()?;
    Some((seq, id))
}

/// `o/<note>/<object>/<index>` back to the note and object.
fn parse_chunk_key(key: &str) -> Option<(NoteId, MediaObjectId)> {
    let mut parts = key.strip_prefix("o/")?.split('/');
    let note = NoteId::decode(parts.next()?).ok()?;
    let object = MediaObjectId::decode(parts.next()?).ok()?;
    Some((note, object))
}

fn whole_value(media: &MediaRef) -> Vec<u8> {
    media.sealed_len().to_be_bytes().to_vec()
}

/// What deleting one object takes: its `w/` row and every chunk.
fn delete_object(note: NoteId, object: MediaObjectId) -> [Change; 2] {
    [
        Change::Delete {
            key: whole_key(note, object),
        },
        Change::DeletePrefix {
            prefix: object_prefix(note, object),
        },
    ]
}

/// A variant's sealed chunks, one per [`MediaStorage::read_sealed_chunk`].
fn sealed_len_of(media: &MediaRef, index: u32) -> usize {
    let full = media.chunk_size as u64 + CHUNK_OVERHEAD as u64;
    full.min(media.sealed_len() - index as u64 * full) as usize
}

pub struct WebMediaStore<R: MediaRows> {
    rows: Arc<R>,
    master: MasterKey,
    /// The next staging record's place in line. Records are filed under it so
    /// that the store's key order is the order they were made in.
    seq: AtomicU64,
    /// Held by an import from its first chunk to its record, and by what
    /// deletes a note's objects. Asynchronous, because what holds it waits on
    /// the database, and a page's main thread may not block.
    commit: tokio::sync::Mutex<()>,
}

impl<R: MediaRows> WebMediaStore<R> {
    /// Open the store, deleting what an interrupted import or download left:
    /// chunks of an object that never became whole.
    pub async fn open(rows: R, master: MasterKey) -> Result<Self> {
        let rows = Arc::new(rows);
        let whole: HashSet<String> = rows.keys("w/").await?.into_iter().collect();
        let mut partial = Vec::new();
        let mut seen = HashSet::new();
        for key in rows.keys("o/").await? {
            let Some((note, object)) = parse_chunk_key(&key) else {
                continue;
            };
            if !whole.contains(&whole_key(note, object)) && seen.insert((note, object)) {
                partial.push(Change::DeletePrefix {
                    prefix: object_prefix(note, object),
                });
            }
        }
        if !partial.is_empty() {
            rows.apply(partial).await?;
        }
        let next = rows
            .keys("s/")
            .await?
            .iter()
            .filter_map(|k| parse_record_key(k))
            .map(|(seq, _)| seq + 1)
            .max()
            .unwrap_or(0);
        Ok(Self {
            rows,
            master,
            seq: AtomicU64::new(next),
            commit: tokio::sync::Mutex::new(()),
        })
    }

    /// Seal a file into this store and record it as staged. The browser hands
    /// over a picked file's bytes whole, so this takes them whole; what it
    /// writes, it writes a chunk per transaction.
    pub async fn import(
        &self,
        note: NoteId,
        name: &str,
        bytes: &[u8],
        thumbnail: Option<ThumbnailInput>,
        hints: ImportHints,
    ) -> Result<Staged> {
        if bytes.is_empty() {
            return Err(refused("the file is empty"));
        }
        let _commit = self.commit.lock().await;
        let id = AttachmentId::random();
        let key = ContentKey::random();

        let sealer = MediaSealer::new(
            &key,
            note,
            id,
            Variant::Original,
            bytes.len() as u64,
            CHUNK_SIZE,
        )?;
        let first_len = sealer.next_len().expect("a non-empty file has a chunk");
        let (format, width, height) = inspect_original(&bytes[..first_len], bytes.len() as u64)?;
        let thumbnail = match thumbnail {
            Some(t) if format.kind() == AttachmentKind::Image => Some(t),
            Some(_) => return Err(refused("only an image has a thumbnail")),
            None => None,
        };

        let mut written = Vec::new();
        let result = self
            .write_import(note, id, &key, sealer, bytes, thumbnail, &mut written)
            .await;
        let (original, thumbnail) = match result {
            Ok(done) => done,
            Err(e) => {
                self.forget_partial(note, &written).await;
                return Err(e);
            }
        };

        let media = Media {
            v: MEDIA_VERSION,
            format,
            name: name.trim().to_owned(),
            width,
            height,
            duration_ms: hints.duration_ms,
            key: Some(key),
            original,
            thumbnail,
        };
        let staged = Staged { note, id, media };
        let committed = async {
            staged.media.check()?;
            let mut changes: Vec<Change> = std::iter::once(&staged.media.original)
                .chain(staged.media.thumbnail.as_ref().map(|t| &t.media))
                .map(|m| Change::Put {
                    key: whole_key(note, m.object),
                    value: whole_value(m),
                })
                .collect();
            changes.push(Change::Put {
                key: record_key(self.seq.fetch_add(1, Ordering::SeqCst), id),
                value: seal_record(&self.master, &staged)?,
            });
            self.rows.apply(changes).await
        };
        if let Err(e) = committed.await {
            self.forget_partial(note, &written).await;
            return Err(e);
        }
        Ok(staged)
    }

    /// The chunks of an import's thumbnail and original, each in a transaction
    /// of its own. `written` names every object started, for a failure to
    /// clear up.
    #[allow(clippy::too_many_arguments)]
    async fn write_import(
        &self,
        note: NoteId,
        id: AttachmentId,
        key: &ContentKey,
        mut sealer: MediaSealer,
        bytes: &[u8],
        thumbnail: Option<ThumbnailInput>,
        written: &mut Vec<MediaObjectId>,
    ) -> Result<(MediaRef, Option<Thumbnail>)> {
        let thumbnail = match thumbnail {
            Some(t) => {
                let (format, width, height) = inspect_thumbnail(&t.bytes)?;
                let (media, sealed) =
                    seal_whole(key, note, id, Variant::Thumbnail, &t.bytes, CHUNK_SIZE)?;
                written.push(media.object);
                self.put_chunks(note, &media, &sealed).await?;
                Some(Thumbnail {
                    media,
                    format,
                    width,
                    height,
                })
            }
            None => None,
        };
        written.push(sealer.object());
        let mut rest = bytes;
        let mut index = 0;
        while let Some(n) = sealer.next_len() {
            let (chunk, tail) = rest.split_at(n);
            let sealed = sealer.seal_chunk(chunk)?;
            self.rows
                .apply(vec![Change::Put {
                    key: chunk_key(note, sealer.object(), index),
                    value: sealed,
                }])
                .await?;
            rest = tail;
            index += 1;
        }
        Ok((sealer.finish()?, thumbnail))
    }

    /// Write a whole sealed variant, a chunk per transaction, without making it
    /// whole: the caller does that, with whatever has to land beside it.
    async fn put_chunks(&self, note: NoteId, media: &MediaRef, sealed: &[u8]) -> Result<()> {
        let mut at = 0;
        for index in 0..media.chunks() {
            let len = sealed_len_of(media, index);
            self.rows
                .apply(vec![Change::Put {
                    key: chunk_key(note, media.object, index),
                    value: sealed[at..at + len].to_vec(),
                }])
                .await?;
            at += len;
        }
        Ok(())
    }

    /// Best effort: what a failed import wrote. If this fails too, the chunks
    /// are not whole, and the next open deletes them.
    async fn forget_partial(&self, note: NoteId, objects: &[MediaObjectId]) {
        let changes = objects
            .iter()
            .flat_map(|&object| delete_object(note, object))
            .collect();
        let _ = self.rows.apply(changes).await;
    }

    /// Every import still waiting to be published into its note, oldest
    /// first. A record that does not open is skipped, as the directory skips
    /// one.
    pub async fn staged(&self) -> Result<Vec<Staged>> {
        let mut out = Vec::new();
        for key in self.rows.keys("s/").await? {
            let Some((_, id)) = parse_record_key(&key) else {
                continue;
            };
            let Some(sealed) = self.rows.get(&key).await? else {
                continue;
            };
            if let Some(staged) = open_record(&self.master, id, &sealed) {
                out.push(staged);
            }
        }
        Ok(out)
    }

    /// Queue a previously local attachment for its first relay upload. Its
    /// sealed objects already exist; only the durable staging record is added.
    pub async fn restage(&self, staged: &Staged) -> Result<()> {
        staged.media.check()?;
        self.rows
            .apply(vec![Change::Put {
                key: record_key(self.seq.fetch_add(1, Ordering::SeqCst), staged.id),
                value: seal_record(&self.master, staged)?,
            }])
            .await
    }

    async fn record_keys(&self, id: AttachmentId) -> Result<Vec<String>> {
        Ok(self
            .rows
            .keys("s/")
            .await?
            .into_iter()
            .filter(|k| parse_record_key(k).is_some_and(|(_, found)| found == id))
            .collect())
    }

    /// The descriptor is in the note: forget the record, keep the objects.
    pub async fn settle(&self, id: AttachmentId) -> Result<()> {
        let changes: Vec<Change> = self
            .record_keys(id)
            .await?
            .into_iter()
            .map(|key| Change::Delete { key })
            .collect();
        if changes.is_empty() {
            return Ok(());
        }
        self.rows.apply(changes).await
    }

    /// Cancel an import: its objects and its record, in one transaction.
    pub async fn discard(&self, staged: &Staged) -> Result<()> {
        let mut changes: Vec<Change> = objects_of(&staged.media)
            .flat_map(|object| delete_object(staged.note, object))
            .collect();
        changes.extend(
            self.record_keys(staged.id)
                .await?
                .into_iter()
                .map(|key| Change::Delete { key }),
        );
        self.rows.apply(changes).await
    }

    /// Whether a variant is here, whole.
    pub async fn has(&self, note: NoteId, media: &MediaRef) -> bool {
        matches!(
            self.rows.get(&whole_key(note, media.object)).await,
            Ok(Some(value)) if value == whole_value(media)
        )
    }

    /// One sealed chunk of a variant, as it goes to the relay.
    pub async fn read_sealed_chunk(
        &self,
        note: NoteId,
        media: &MediaRef,
        index: u32,
    ) -> Result<Vec<u8>> {
        if index >= media.chunks() {
            return Err(CoreError::Crypto("no such chunk"));
        }
        match self.rows.get(&chunk_key(note, media.object, index)).await? {
            Some(sealed) if sealed.len() == sealed_len_of(media, index) => Ok(sealed),
            _ => Err(CoreError::Crypto("an attachment cut short")),
        }
    }

    /// A variant's sealed bytes, whole.
    pub async fn read_sealed(&self, note: NoteId, media: &MediaRef) -> Result<Vec<u8>> {
        let mut out = Vec::with_capacity(media.sealed_len() as usize);
        for index in 0..media.chunks() {
            out.extend(self.read_sealed_chunk(note, media, index).await?);
        }
        Ok(out)
    }

    /// Start keeping a variant that is being downloaded a chunk at a time.
    pub async fn incoming(&self, note: NoteId, media: &MediaRef) -> Result<WebIncoming<R>> {
        Ok(WebIncoming {
            rows: self.rows.clone(),
            note,
            media: media.clone(),
            next: 0,
            written: 0,
            digest: Sha256::new(),
        })
    }

    /// Keep a variant that was downloaded whole, once it has proved to be the
    /// one the descriptor names.
    pub async fn put_sealed(&self, note: NoteId, media: &MediaRef, sealed: &[u8]) -> Result<()> {
        verify_sealed(media, sealed)?;
        self.put_chunks(note, media, sealed).await?;
        self.rows
            .apply(vec![Change::Put {
                key: whole_key(note, media.object),
                value: whole_value(media),
            }])
            .await
    }

    /// Verify and decrypt one chunk of a variant, reading only that chunk.
    pub async fn read_chunk(
        &self,
        note: NoteId,
        id: AttachmentId,
        media: &Media,
        variant: Variant,
        index: u32,
    ) -> Result<Vec<u8>> {
        let (key, media_ref) = variant_of(media, variant)?;
        let opener = MediaOpener::new(key, note, id, variant, media_ref)?;
        if !self.has(note, media_ref).await {
            return Err(refused("this attachment is not on this device"));
        }
        opener.open_chunk(
            index,
            &self.read_sealed_chunk(note, media_ref, index).await?,
        )
    }

    /// A whole variant, decrypted.
    pub async fn read(
        &self,
        note: NoteId,
        id: AttachmentId,
        media: &Media,
        variant: Variant,
    ) -> Result<Vec<u8>> {
        let (key, media_ref) = variant_of(media, variant)?;
        let opener = MediaOpener::new(key, note, id, variant, media_ref)?;
        if !self.has(note, media_ref).await {
            return Err(refused("this attachment is not on this device"));
        }
        opener.open_whole(&self.read_sealed(note, media_ref).await?)
    }

    /// Delete an attachment's objects from this tab.
    pub async fn remove(&self, note: NoteId, media: &Media) -> Result<()> {
        let changes = objects_of(media)
            .flat_map(|object| delete_object(note, object))
            .collect();
        self.rows.apply(changes).await
    }

    /// Delete every object this tab holds for a note, and every import into it
    /// still staged, in one transaction.
    /// Every note this tab holds whole objects for, so that a launch can find
    /// the ones whose note has gone.
    pub async fn held_notes(&self) -> Result<Vec<NoteId>> {
        let mut notes: Vec<NoteId> = self
            .rows
            .keys("w/")
            .await?
            .iter()
            .filter_map(|k| k.strip_prefix("w/")?.split('/').next())
            .filter_map(|n| NoteId::decode(n).ok())
            .collect();
        notes.dedup();
        Ok(notes)
    }

    /// Delete everything: every object and every staged import. What wiping
    /// the device does.
    pub async fn wipe(&self) -> Result<()> {
        let _commit = self.commit.lock().await;
        self.rows
            .apply(vec![Change::DeletePrefix {
                prefix: String::new(),
            }])
            .await
    }

    pub async fn purge_note(&self, note: NoteId) -> Result<()> {
        let _commit = self.commit.lock().await;
        let mut changes = vec![
            Change::DeletePrefix {
                prefix: format!("o/{}/", note.encode()),
            },
            Change::DeletePrefix {
                prefix: format!("w/{}/", note.encode()),
            },
        ];
        for key in self.rows.keys("s/").await? {
            let Some((_, id)) = parse_record_key(&key) else {
                continue;
            };
            let Some(sealed) = self.rows.get(&key).await? else {
                continue;
            };
            if open_record(&self.master, id, &sealed).is_some_and(|s| s.note == note) {
                changes.push(Change::Delete { key });
            }
        }
        self.rows.apply(changes).await
    }

    /// Delete a note's whole objects that neither `live` nor a staged import
    /// names. A download still arriving is not whole, and is left alone.
    pub async fn collect_garbage(&self, note: NoteId, live: &[&Media]) -> Result<usize> {
        let _commit = self.commit.lock().await;
        let staged = self.staged().await?;
        let keep: HashSet<MediaObjectId> = live
            .iter()
            .flat_map(|m| objects_of(m))
            .chain(
                staged
                    .iter()
                    .filter(|s| s.note == note)
                    .flat_map(|s| objects_of(&s.media)),
            )
            .collect();
        let prefix = format!("w/{}/", note.encode());
        let mut changes = Vec::new();
        let mut removed = 0;
        for key in self.rows.keys(&prefix).await? {
            let Some(object) = key
                .strip_prefix(&prefix)
                .and_then(|o| MediaObjectId::decode(o).ok())
            else {
                continue;
            };
            if !keep.contains(&object) {
                changes.extend(delete_object(note, object));
                removed += 1;
            }
        }
        if !changes.is_empty() {
            self.rows.apply(changes).await?;
        }
        Ok(removed)
    }
}

/// A variant arriving in order: each chunk written as it comes, hashed as it
/// goes, and made whole only by [`WebIncoming::finish`].
///
/// A download delivers chunk by chunk, so each [`write`](WebIncoming::write)
/// must be exactly the next chunk. Dropped unfinished, or refused at the end,
/// it leaves chunks that are not whole, which nothing reads and the next open
/// deletes. They are not deleted here: another download of the same object
/// may be writing the same rows, and taking them from under it would let it
/// mark whole an object whose chunks had gone.
pub struct WebIncoming<R: MediaRows> {
    rows: Arc<R>,
    note: NoteId,
    media: MediaRef,
    next: u32,
    written: u64,
    digest: Sha256,
}

impl<R: MediaRows> WebIncoming<R> {
    pub async fn write(&mut self, sealed: &[u8]) -> Result<()> {
        if self.next >= self.media.chunks() || sealed.len() != sealed_len_of(&self.media, self.next)
        {
            return Err(CoreError::Crypto("an attachment of the wrong length"));
        }
        self.rows
            .apply(vec![Change::Put {
                key: chunk_key(self.note, self.media.object, self.next),
                value: sealed.to_vec(),
            }])
            .await?;
        self.digest.update(sealed);
        self.written += sealed.len() as u64;
        self.next += 1;
        Ok(())
    }

    pub async fn finish(self) -> Result<()> {
        if self.written != self.media.sealed_len() {
            return Err(CoreError::Crypto("an attachment of the wrong length"));
        }
        if <[u8; 32]>::from(self.digest.finalize()) != self.media.digest {
            return Err(CoreError::Crypto(
                "an attachment that is not the one attached",
            ));
        }
        self.rows
            .apply(vec![Change::Put {
                key: whole_key(self.note, self.media.object),
                value: whole_value(&self.media),
            }])
            .await
    }
}

impl<R: MediaRows> MediaStorage for WebMediaStore<R> {
    type Incoming = WebIncoming<R>;

    fn staged(&self) -> Fut<'_, Result<Vec<Staged>>> {
        Box::pin(WebMediaStore::staged(self))
    }

    fn settle(&self, id: AttachmentId) -> Fut<'_, Result<()>> {
        Box::pin(WebMediaStore::settle(self, id))
    }

    fn discard<'a>(&'a self, staged: &'a Staged) -> Fut<'a, Result<()>> {
        Box::pin(WebMediaStore::discard(self, staged))
    }

    fn has<'a>(&'a self, note: NoteId, media: &'a MediaRef) -> Fut<'a, bool> {
        Box::pin(WebMediaStore::has(self, note, media))
    }

    fn read_sealed_chunk<'a>(
        &'a self,
        note: NoteId,
        media: &'a MediaRef,
        index: u32,
    ) -> Fut<'a, Result<Vec<u8>>> {
        Box::pin(WebMediaStore::read_sealed_chunk(self, note, media, index))
    }

    fn incoming<'a>(
        &'a self,
        note: NoteId,
        media: &'a MediaRef,
    ) -> Fut<'a, Result<Self::Incoming>> {
        Box::pin(WebMediaStore::incoming(self, note, media))
    }
}

impl<R: MediaRows> IncomingMedia for WebIncoming<R> {
    fn write<'a>(&'a mut self, sealed: &'a [u8]) -> Fut<'a, Result<()>> {
        Box::pin(WebIncoming::write(self, sealed))
    }

    fn finish(self) -> Fut<'static, Result<()>> {
        Box::pin(WebIncoming::finish(self))
    }
}

