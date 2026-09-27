//! Where a device keeps attachment bytes: a directory of sealed files.
//!
//! ```text
//! <root>/objects/<note id>/<object id>   a sealed variant, exactly as it
//!                                        travels to the relay
//! <root>/staging/<attachment id>         a sealed record of an import whose
//!                                        descriptor is not in the note yet
//! <root>/tmp/                            half-written files; emptied on open
//! ```
//!
//! An object is stored as the chunks [`super::seal`] made, and nothing else.
//! The chunks are already encrypted under a key only the note's readers have,
//! so sealing them again under the master key would buy nothing; and keeping
//! the relay's bytes means an upload is a read and a download is a write, with
//! no step in between that could see plaintext.
//!
//! What *is* sealed under the master key is the staging record, because it
//! holds a content key that is not in any note yet. The order an import commits
//! in is the whole of the crash story ([ADR 0027](../../../docs/adr/0027-attachments.md) §3):
//!
//! 1. every object is written to `tmp/`, synced, and renamed into `objects/`;
//! 2. then the staging record is written the same way. From here the import is
//!    "saved on this device": whatever happens next, the record names the
//!    objects and holds their key;
//! 3. the caller publishes the descriptor into the note, when its bytes are on
//!    the relay or at once for a note that is not synced;
//! 4. then [`MediaStore::settle`] deletes the record.
//!
//! A crash before 2 leaves objects nothing names, which
//! [`MediaStore::collect_garbage`] removes. A crash between 2 and 4 leaves a
//! record, which [`MediaStore::staged`] hands back on the next launch — as an
//! attachment still waiting to be published, or, if its descriptor made it into
//! the note after all, one to settle. There is no state in which a descriptor
//! names bytes this device has lost.

use super::seal::{seal_whole, verify_sealed, MediaOpener, MediaSealer, Variant, CHUNK_OVERHEAD};
use super::storage::{
    inspect_original, inspect_thumbnail, objects_of, open_record, refused, seal_record, variant_of,
    Fut, IncomingMedia, MediaStorage,
};
pub use super::storage::{ImportHints, Staged, ThumbnailInput};
use super::{AttachmentKind, ContentKey, Media, MediaRef, Thumbnail, CHUNK_SIZE, MEDIA_VERSION};
use crate::crypto::MasterKey;
use crate::error::{CoreError, Result};
use crate::ids::{AttachmentId, MediaObjectId, NoteId};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

fn io(e: std::io::Error) -> CoreError {
    CoreError::Storage(e.to_string())
}

pub struct MediaStore {
    root: PathBuf,
    master: MasterKey,
    /// Held by an import from its first object to its record, and by anything
    /// that deletes objects by what is *not* named. Between an import's step 1
    /// and step 2 its objects are named by nothing, and a collection that ran
    /// then would take bytes the record is about to promise are here.
    commit: Mutex<()>,
}

impl MediaStore {
    pub fn open(root: impl AsRef<Path>, master: MasterKey) -> Result<Self> {
        let root = root.as_ref().to_path_buf();
        for dir in ["objects", "staging"] {
            fs::create_dir_all(root.join(dir)).map_err(io)?;
        }
        // Whatever is in `tmp/` was being written when the app last stopped,
        // and nothing refers to it: step 1 renames before step 2 names.
        let tmp = root.join("tmp");
        if tmp.exists() {
            fs::remove_dir_all(&tmp).map_err(io)?;
        }
        fs::create_dir_all(&tmp).map_err(io)?;
        Ok(Self {
            root,
            master,
            commit: Mutex::new(()),
        })
    }

    fn lock(&self) -> MutexGuard<'_, ()> {
        // The guard protects no data, only an order of file operations, so a
        // panic elsewhere while it was held leaves nothing to repair.
        self.commit.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn note_dir(&self, note: NoteId) -> PathBuf {
        self.root.join("objects").join(note.encode())
    }

    fn object_path(&self, note: NoteId, object: MediaObjectId) -> PathBuf {
        self.note_dir(note).join(object.encode())
    }

    fn staging_path(&self, id: AttachmentId) -> PathBuf {
        self.root.join("staging").join(id.encode())
    }

    /// Write `fill`'s output to `tmp/`, sync it, and rename it to `to`.
    fn write_atomic(&self, to: &Path, fill: impl FnOnce(&mut File) -> Result<()>) -> Result<()> {
        let tmp = self.root.join("tmp").join(MediaObjectId::random().encode());
        let result = (|| {
            let mut file = File::create(&tmp).map_err(io)?;
            fill(&mut file)?;
            file.sync_all().map_err(io)?;
            if let Some(dir) = to.parent() {
                fs::create_dir_all(dir).map_err(io)?;
            }
            fs::rename(&tmp, to).map_err(io)?;
            sync_dir(to.parent())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        result
    }

    /// Copy a file into this store, encrypted, and record it as staged.
    ///
    /// `len` is the source's size, and the source must deliver exactly that
    /// many bytes: every chunk commits to it, so a file that grows or shrinks
    /// while it is read is refused rather than half-attached. The format is
    /// read from the bytes; `name` is only what to call it.
    pub fn import(
        &self,
        note: NoteId,
        name: &str,
        mut source: impl Read,
        len: u64,
        thumbnail: Option<ThumbnailInput>,
        hints: ImportHints,
    ) -> Result<Staged> {
        if len == 0 {
            return Err(refused("the file is empty"));
        }
        let _commit = self.lock();
        let id = AttachmentId::random();
        let key = ContentKey::random();

        let mut sealer = MediaSealer::new(&key, note, id, Variant::Original, len, CHUNK_SIZE)?;
        let first_len = sealer.next_len().expect("a non-empty file has a chunk");
        let mut chunk = vec![0u8; first_len];
        source
            .read_exact(&mut chunk)
            .map_err(|_| refused("the file ended early"))?;

        let (format, width, height) = inspect_original(&chunk, len)?;

        let thumbnail = match thumbnail {
            Some(t) if format.kind() == AttachmentKind::Image => {
                Some(self.write_thumbnail(note, id, &key, &t.bytes)?)
            }
            Some(_) => return Err(refused("only an image has a thumbnail")),
            None => None,
        };

        let original_object;
        let original = {
            // The object's name is the sealer's, so it is only known at the end;
            // write to a temporary path and let `finish` say where it goes.
            let tmp = self.root.join("tmp").join(MediaObjectId::random().encode());
            let written = (|| {
                let mut file = File::create(&tmp).map_err(io)?;
                file.write_all(&sealer.seal_chunk(&chunk)?).map_err(io)?;
                while let Some(n) = sealer.next_len() {
                    chunk.resize(n, 0);
                    source
                        .read_exact(&mut chunk)
                        .map_err(|_| refused("the file changed while it was being added"))?;
                    file.write_all(&sealer.seal_chunk(&chunk)?).map_err(io)?;
                }
                if source.read(&mut [0u8; 1]).map_err(io)? != 0 {
                    return Err(refused("the file changed while it was being added"));
                }
                file.sync_all().map_err(io)?;
                let media = sealer.finish()?;
                let to = self.object_path(note, media.object);
                fs::create_dir_all(self.note_dir(note)).map_err(io)?;
                fs::rename(&tmp, &to).map_err(io)?;
                sync_dir(Some(&self.note_dir(note)))?;
                Ok(media)
            })();
            if written.is_err() {
                let _ = fs::remove_file(&tmp);
                if let Some(t) = &thumbnail {
                    let _ = fs::remove_file(self.object_path(note, t.media.object));
                }
            }
            let media = written?;
            original_object = media.object;
            media
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
        if let Err(e) = staged
            .media
            .check()
            .and_then(|()| self.write_record(&staged))
        {
            let _ = fs::remove_file(self.object_path(note, original_object));
            if let Some(t) = &staged.media.thumbnail {
                let _ = fs::remove_file(self.object_path(note, t.media.object));
            }
            return Err(e);
        }
        Ok(staged)
    }

    fn write_thumbnail(
        &self,
        note: NoteId,
        id: AttachmentId,
        key: &ContentKey,
        bytes: &[u8],
    ) -> Result<Thumbnail> {
        let (format, width, height) = inspect_thumbnail(bytes)?;
        let (media, sealed) = seal_whole(key, note, id, Variant::Thumbnail, bytes, CHUNK_SIZE)?;
        self.write_atomic(&self.object_path(note, media.object), |f| {
            f.write_all(&sealed).map_err(io)
        })?;
        Ok(Thumbnail {
            media,
            format,
            width,
            height,
        })
    }

    fn write_record(&self, staged: &Staged) -> Result<()> {
        let sealed = seal_record(&self.master, staged)?;
        self.write_atomic(&self.staging_path(staged.id), |f| {
            f.write_all(&sealed).map_err(io)
        })
    }

    /// Every import still waiting to be published into its note, oldest
    /// first. A record that no longer opens — another master key, a torn
    /// write that somehow survived the rename — is not one this device can do
    /// anything with, and is skipped rather than failing the rest.
    pub fn staged(&self) -> Result<Vec<Staged>> {
        let mut out = Vec::new();
        for entry in fs::read_dir(self.root.join("staging")).map_err(io)? {
            let entry = entry.map_err(io)?;
            let Some(id) = entry
                .file_name()
                .to_str()
                .and_then(|n| AttachmentId::decode(n).ok())
            else {
                continue;
            };
            let sealed = fs::read(entry.path()).map_err(io)?;
            if let Some(staged) = open_record(&self.master, id, &sealed) {
                let modified = entry.metadata().and_then(|m| m.modified()).ok();
                out.push((modified, staged));
            }
        }
        out.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.id.cmp(&b.1.id)));
        Ok(out.into_iter().map(|(_, s)| s).collect())
    }

    /// The descriptor is in the note: forget the record, keep the objects.
    pub fn settle(&self, id: AttachmentId) -> Result<()> {
        match fs::remove_file(self.staging_path(id)) {
            Ok(()) => sync_dir(Some(&self.root.join("staging"))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(io(e)),
        }
    }

    /// Cancel an import: its objects, then its record. Objects first, so a
    /// crash in between leaves a record naming missing objects — which
    /// [`MediaStore::staged`] still returns, and which a second discard
    /// finishes — rather than objects nothing names.
    pub fn discard(&self, staged: &Staged) -> Result<()> {
        self.remove(staged.note, &staged.media)?;
        self.settle(staged.id)
    }

    /// Whether a variant is here, whole. Only the length is checked: the digest
    /// was checked when it arrived, and every chunk is checked when it is read.
    pub fn has(&self, note: NoteId, media: &MediaRef) -> bool {
        fs::metadata(self.object_path(note, media.object))
            .is_ok_and(|m| m.len() == media.sealed_len())
    }

    /// A variant's sealed bytes, for the relay.
    pub fn read_sealed(&self, note: NoteId, media: &MediaRef) -> Result<Vec<u8>> {
        fs::read(self.object_path(note, media.object)).map_err(io)
    }

    /// One sealed chunk of a variant, as it goes to the relay. An upload reads
    /// these one at a time, so a 50 MB recording is never in memory whole.
    pub fn read_sealed_chunk(&self, note: NoteId, media: &MediaRef, index: u32) -> Result<Vec<u8>> {
        if index >= media.chunks() {
            return Err(CoreError::Crypto("no such chunk"));
        }
        let full = media.chunk_size as u64 + CHUNK_OVERHEAD as u64;
        let at = index as u64 * full;
        let len = full.min(media.sealed_len() - at);
        let mut file = File::open(self.object_path(note, media.object)).map_err(io)?;
        file.seek(SeekFrom::Start(at)).map_err(io)?;
        let mut sealed = vec![0u8; len as usize];
        file.read_exact(&mut sealed)
            .map_err(|_| CoreError::Crypto("an attachment cut short"))?;
        Ok(sealed)
    }

    /// Start keeping a variant that is being downloaded a chunk at a time.
    /// Nothing is in `objects/` until [`Incoming::finish`] has checked the
    /// whole of it against the descriptor.
    pub fn incoming(&self, note: NoteId, media: &MediaRef) -> Result<Incoming> {
        let tmp = self.root.join("tmp").join(MediaObjectId::random().encode());
        Ok(Incoming {
            file: File::create(&tmp).map_err(io)?,
            tmp,
            to: self.object_path(note, media.object),
            media: media.clone(),
            written: 0,
            digest: Sha256::new(),
        })
    }

    /// Keep a variant that was downloaded, once it has proved to be the one the
    /// descriptor names. Idempotent: the same bytes arriving twice are one file.
    pub fn put_sealed(&self, note: NoteId, media: &MediaRef, sealed: &[u8]) -> Result<()> {
        verify_sealed(media, sealed)?;
        self.write_atomic(&self.object_path(note, media.object), |f| {
            f.write_all(sealed).map_err(io)
        })
    }

    /// Verify and decrypt one chunk of a variant, reading only that chunk.
    pub fn read_chunk(
        &self,
        note: NoteId,
        id: AttachmentId,
        media: &Media,
        variant: Variant,
        index: u32,
    ) -> Result<Vec<u8>> {
        let (key, media_ref) = variant_of(media, variant)?;
        let opener = MediaOpener::new(key, note, id, variant, media_ref)?;
        if index >= opener.chunks() {
            return Err(CoreError::Crypto("no such chunk"));
        }
        let (at, len) = opener.sealed_range(index);
        let mut file = File::open(self.object_path(note, media_ref.object)).map_err(io)?;
        file.seek(SeekFrom::Start(at)).map_err(io)?;
        let mut sealed = vec![0u8; len];
        file.read_exact(&mut sealed)
            .map_err(|_| CoreError::Crypto("an attachment cut short"))?;
        opener.open_chunk(index, &sealed)
    }

    /// A whole variant, decrypted. For a thumbnail, or a picture being shown;
    /// a player reads [`MediaStore::read_chunk`] as it goes.
    pub fn read(
        &self,
        note: NoteId,
        id: AttachmentId,
        media: &Media,
        variant: Variant,
    ) -> Result<Vec<u8>> {
        let (key, media_ref) = variant_of(media, variant)?;
        let opener = MediaOpener::new(key, note, id, variant, media_ref)?;
        opener.open_whole(&self.read_sealed(note, media_ref)?)
    }

    /// Delete an attachment's objects from this device. Not from the relay,
    /// and not from anyone else's device.
    pub fn remove(&self, note: NoteId, media: &Media) -> Result<()> {
        for object in objects_of(media) {
            match fs::remove_file(self.object_path(note, object)) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(io(e)),
            }
        }
        Ok(())
    }

    /// Delete every object this device holds for a note, and every import into
    /// it still staged. What forgetting or purging the note does.
    pub fn purge_note(&self, note: NoteId) -> Result<()> {
        let _commit = self.lock();
        for staged in self.staged()? {
            if staged.note == note {
                self.settle(staged.id)?;
            }
        }
        match fs::remove_dir_all(self.note_dir(note)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(io(e)),
        }
    }

    /// Delete a note's objects that neither `live` nor a staged import names:
    /// what a crash before a record was written leaves, and what a removal
    /// leaves on a device that was not the one to remove it. Returns how many.
    pub fn collect_garbage(&self, note: NoteId, live: &[&Media]) -> Result<usize> {
        let _commit = self.lock();
        let staged = self.staged()?;
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
        let dir = self.note_dir(note);
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(io(e)),
        };
        let mut removed = 0;
        for entry in entries {
            let entry = entry.map_err(io)?;
            let named = entry
                .file_name()
                .to_str()
                .and_then(|n| MediaObjectId::decode(n).ok());
            if named.is_some_and(|object| !keep.contains(&object)) {
                fs::remove_file(entry.path()).map_err(io)?;
                removed += 1;
            }
        }
        Ok(removed)
    }
}

/// A variant arriving in order, written to `tmp/` and hashed as it goes.
/// Dropped unfinished, it leaves a file the next open empties.
pub struct Incoming {
    file: File,
    tmp: PathBuf,
    to: PathBuf,
    media: MediaRef,
    written: u64,
    digest: Sha256,
}

impl Incoming {
    pub fn write(&mut self, sealed: &[u8]) -> Result<()> {
        if self.written + sealed.len() as u64 > self.media.sealed_len() {
            return Err(CoreError::Crypto("an attachment of the wrong length"));
        }
        self.file.write_all(sealed).map_err(io)?;
        self.digest.update(sealed);
        self.written += sealed.len() as u64;
        Ok(())
    }

    /// Check the length and the digest, then move the file into place. The
    /// same checks [`MediaStore::put_sealed`] makes on a whole buffer.
    pub fn finish(self) -> Result<()> {
        let Incoming {
            file,
            tmp,
            to,
            media,
            written,
            digest,
        } = self;
        let checked = (|| {
            if written != media.sealed_len() {
                return Err(CoreError::Crypto("an attachment of the wrong length"));
            }
            if <[u8; 32]>::from(digest.finalize()) != media.digest {
                return Err(CoreError::Crypto(
                    "an attachment that is not the one attached",
                ));
            }
            file.sync_all().map_err(io)?;
            drop(file);
            if let Some(dir) = to.parent() {
                fs::create_dir_all(dir).map_err(io)?;
            }
            fs::rename(&tmp, &to).map_err(io)?;
            sync_dir(to.parent())
        })();
        if checked.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        checked
    }
}

/// The directory answers at once; the trait is asynchronous for the browser's
/// sake, and these are its answers wrapped in ready futures.
impl MediaStorage for MediaStore {
    type Incoming = Incoming;

    fn staged(&self) -> Fut<'_, Result<Vec<Staged>>> {
        Box::pin(std::future::ready(MediaStore::staged(self)))
    }

    fn settle(&self, id: AttachmentId) -> Fut<'_, Result<()>> {
        Box::pin(std::future::ready(MediaStore::settle(self, id)))
    }

    fn discard<'a>(&'a self, staged: &'a Staged) -> Fut<'a, Result<()>> {
        Box::pin(std::future::ready(MediaStore::discard(self, staged)))
    }

    fn has<'a>(&'a self, note: NoteId, media: &'a MediaRef) -> Fut<'a, bool> {
        Box::pin(std::future::ready(MediaStore::has(self, note, media)))
    }

    fn read_sealed_chunk<'a>(
        &'a self,
        note: NoteId,
        media: &'a MediaRef,
        index: u32,
    ) -> Fut<'a, Result<Vec<u8>>> {
        Box::pin(std::future::ready(MediaStore::read_sealed_chunk(
            self, note, media, index,
        )))
    }

    fn incoming<'a>(
        &'a self,
        note: NoteId,
        media: &'a MediaRef,
    ) -> Fut<'a, Result<Self::Incoming>> {
        Box::pin(std::future::ready(MediaStore::incoming(self, note, media)))
    }
}

impl IncomingMedia for Incoming {
    fn write<'a>(&'a mut self, sealed: &'a [u8]) -> Fut<'a, Result<()>> {
        Box::pin(std::future::ready(Incoming::write(self, sealed)))
    }

    fn finish(self) -> Fut<'static, Result<()>> {
        Box::pin(std::future::ready(Incoming::finish(self)))
    }
}

/// Make a rename durable. A directory cannot be opened for syncing on Windows,
/// and NTFS journals the rename itself, so there it is a no-op.
fn sync_dir(dir: Option<&Path>) -> Result<()> {
    #[cfg(unix)]
    if let Some(dir) = dir {
        File::open(dir).and_then(|d| d.sync_all()).map_err(io)?;
    }
    #[cfg(not(unix))]
    let _ = dir;
    Ok(())
}
