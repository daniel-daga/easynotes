//! Attachment bytes: sealed objects the relay holds for a topic and cannot open
//! ([ADR 0027](../../docs/adr/0027-attachments.md) §4).
//!
//! An object is uploaded in the chunks the core sealed it in, then committed,
//! and from then on it is immutable and can be fetched by anyone who can read
//! the topic. The relay checks sizes, that every chunk arrived, and that SHA-256
//! over them is the digest the uploader names. It cannot check anything else,
//! and like the log it does not try.
//!
//! ```text
//! POST /v1/media/<note>/<object>          reserve {sealed_len, chunk_size}
//! PUT  /v1/media/<note>/<object>/<index>  one sealed chunk, raw bytes
//! GET  /v1/media/<note>/<object>/status   {committed, received}
//! POST /v1/media/<note>/<object>/commit   {digest}
//! GET  /v1/media/<note>/<object>          the object, or a `Range` of it
//! ```
//!
//! Every call is idempotent, so an upload interrupted anywhere resumes by
//! asking for its status and sending what is missing. Every call wants the
//! bearer token of an authenticated device, as a socket does: that is how
//! writes to a topic are authorised, and the object ID alone authorises
//! nothing.
//!
//! Two halves, kept apart for the reason the log and its bookkeeping are kept
//! together: the **metadata** — what is reserved, what is committed, and the
//! budgets they count against — lives in [`RelayStore`](crate::store::RelayStore),
//! so a reservation is checked against the quota atomically, in Postgres as in
//! memory. The **bytes** live behind [`Blobs`]: a directory on a persistent
//! volume in production, memory for tests and `just run-relay`. The two are
//! backed up together (`deploy/synology/README.md`).
//!
//! Writes to one object are serialised under a per-process lock. Several
//! instances over one directory stay correct where it matters — the metadata
//! is transactional, files are renamed into place, and a commit that misses a
//! chunk racing it on another instance says so and is retried — but can leave
//! a stray chunk behind an assembled object.

use crate::app::AppState;
use crate::error::{RelayError, Result};
use async_trait::async_trait;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use easynotes_core::attachment::seal::CHUNK_OVERHEAD;
use easynotes_core::attachment::{MAX_AUDIO_BYTES, MAX_CHUNK_SIZE};
use easynotes_core::ids::{MediaObjectId, NoteId};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// The smallest chunk the relay takes. The core seals in 1 MiB chunks; this is
/// what stops a 50 MiB object from arriving as fifty million requests.
pub const MIN_CHUNK_SIZE: u32 = 64 << 10;

/// The largest sealed chunk a `PUT` may carry.
pub const MAX_PART_BYTES: usize = MAX_CHUNK_SIZE as usize + CHUNK_OVERHEAD;

/// How much one topic's objects, and all of them together, may hold, and how
/// long an upload may sit unfinished.
#[derive(Clone, Copy, Debug)]
pub struct MediaLimits {
    /// Reserved and committed bytes in one topic. Well above the 200 MB a note
    /// may have live at once, because until clean-up exists (ADR 0027 §5) a
    /// removed attachment keeps its bytes here.
    pub note_bytes: u64,
    /// Reserved and committed bytes in every topic together, or `None`.
    pub ceiling: Option<u64>,
    /// An uncommitted reservation with no chunk for this long is dropped, and
    /// stops counting.
    pub staging_ttl_secs: i64,
}

impl Default for MediaLimits {
    fn default() -> Self {
        Self {
            note_bytes: 1 << 30,
            ceiling: None,
            staging_ttl_secs: 24 * 60 * 60,
        }
    }
}

/// Where a public relay stops reserving new media unless told otherwise.
pub const DEFAULT_MEDIA_CEILING: u64 = 64 << 30;

/// Media storage for a relay: the bytes, and the limits on them. `blobs` is
/// `None` on a relay that keeps its log in Postgres and was given nowhere
/// durable to keep media, which then answers every media call with 503 rather
/// than keeping bytes in memory that its database says it has.
#[derive(Clone)]
pub struct MediaConfig {
    pub blobs: Option<Arc<dyn Blobs>>,
    pub limits: MediaLimits,
}

impl Default for MediaConfig {
    fn default() -> Self {
        Self {
            blobs: Some(Arc::new(MemoryBlobs::default())),
            limits: MediaLimits::default(),
        }
    }
}

/// An object's size and how it is cut, as its uploader declared them. All the
/// relay knows of the file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Geometry {
    pub sealed_len: u64,
    pub chunk_size: u32,
}

impl Geometry {
    fn full(&self) -> u64 {
        self.chunk_size as u64 + CHUNK_OVERHEAD as u64
    }

    pub fn chunks(&self) -> u32 {
        self.sealed_len.div_ceil(self.full()) as u32
    }

    /// Where chunk `index` starts, and how long it is.
    pub fn part(&self, index: u32) -> (u64, u64) {
        let at = index as u64 * self.full();
        (at, self.full().min(self.sealed_len - at))
    }

    /// Whether this is an object the core could have sealed: a chunk size in
    /// range, a last chunk with something in it, and no more plaintext than
    /// the largest attachment.
    pub fn check(&self) -> Result<()> {
        let bad = |why: &str| Err(RelayError::BadRequest(why.into()));
        if !(MIN_CHUNK_SIZE..=MAX_CHUNK_SIZE).contains(&self.chunk_size) {
            return bad("chunk size out of range");
        }
        if self.sealed_len == 0 {
            return bad("an empty object");
        }
        let chunks = self.chunks() as u64;
        let (_, last) = self.part(self.chunks() - 1);
        if last <= CHUNK_OVERHEAD as u64 {
            return bad("a sealed length no chunking makes");
        }
        if self.sealed_len - chunks * CHUNK_OVERHEAD as u64 > MAX_AUDIO_BYTES {
            return bad("larger than any attachment");
        }
        Ok(())
    }
}

/// One object's metadata.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaRow {
    pub geometry: Geometry,
    /// The digest it was committed under, once it is.
    pub committed: Option<[u8; 32]>,
}

/// What became of a reservation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reserved {
    New,
    /// Already reserved, or committed, with the same geometry.
    Same,
    /// Already reserved or committed with a different one.
    Conflict,
    NoteFull,
    RelayFull,
    Gone,
}

/// What became of a chunk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PartPut {
    Stored,
    /// These bytes were already there.
    Same,
    /// Different bytes are already there.
    Differs,
}

/// What became of assembling an object from its chunks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Assembled {
    Done,
    /// These chunks have not arrived.
    Missing(Vec<u32>),
    /// Every chunk was there and together they are not the digest. They have
    /// been dropped, so the upload can be sent again.
    Mismatch,
}

/// Where an object's bytes are kept.
///
/// Knows nothing of reservations or quotas; the caller has checked those, and
/// the length of every chunk, before it gets here. Paths are made only from IDs
/// that decoded, so nothing a client sends reaches the filesystem as a name.
#[async_trait]
pub trait Blobs: Send + Sync + 'static {
    async fn put_part(&self, note: &str, object: &str, index: u32, bytes: Bytes)
        -> Result<PartPut>;

    /// The chunks received for an object not yet assembled, ascending.
    async fn parts(&self, note: &str, object: &str) -> Result<Vec<u32>>;

    /// Put chunks `0..chunks` together into the object if SHA-256 over them is
    /// `digest`. Idempotent: an object already assembled is checked against
    /// `digest` instead, so a crash between assembling and recording the
    /// commit is finished by the next commit.
    async fn assemble(
        &self,
        note: &str,
        object: &str,
        chunks: u32,
        digest: [u8; 32],
    ) -> Result<Assembled>;

    /// `len` bytes of an assembled object from `at`, or `None` if it is not.
    async fn read(&self, note: &str, object: &str, at: u64, len: u64) -> Result<Option<Vec<u8>>>;

    /// An object and any chunks of it.
    async fn remove(&self, note: &str, object: &str) -> Result<()>;

    /// Every object of a topic.
    async fn remove_note(&self, note: &str) -> Result<()>;
}

// ---------------------------------------------------------------------------
// In memory
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Held {
    parts: BTreeMap<u32, Bytes>,
    whole: Option<Vec<u8>>,
}

/// Media in memory, for tests and `just run-relay`. Gone when the process is.
#[derive(Default)]
pub struct MemoryBlobs {
    objects: Mutex<HashMap<(String, String), Held>>,
}

impl MemoryBlobs {
    /// Every byte held, for the no-plaintext tests.
    pub fn all_bytes(&self) -> Vec<Vec<u8>> {
        let objects = self.objects.lock().unwrap();
        objects
            .values()
            .flat_map(|held| {
                held.parts
                    .values()
                    .map(|b| b.to_vec())
                    .chain(held.whole.clone())
            })
            .collect()
    }
}

#[async_trait]
impl Blobs for MemoryBlobs {
    async fn put_part(
        &self,
        note: &str,
        object: &str,
        index: u32,
        bytes: Bytes,
    ) -> Result<PartPut> {
        let mut objects = self.objects.lock().unwrap();
        let held = objects
            .entry((note.to_string(), object.to_string()))
            .or_default();
        Ok(match held.parts.get(&index) {
            Some(existing) if *existing == bytes => PartPut::Same,
            Some(_) => PartPut::Differs,
            None => {
                held.parts.insert(index, bytes);
                PartPut::Stored
            }
        })
    }

    async fn parts(&self, note: &str, object: &str) -> Result<Vec<u32>> {
        let objects = self.objects.lock().unwrap();
        Ok(objects
            .get(&(note.to_string(), object.to_string()))
            .map(|held| held.parts.keys().copied().collect())
            .unwrap_or_default())
    }

    async fn assemble(
        &self,
        note: &str,
        object: &str,
        chunks: u32,
        digest: [u8; 32],
    ) -> Result<Assembled> {
        let mut objects = self.objects.lock().unwrap();
        let held = objects
            .entry((note.to_string(), object.to_string()))
            .or_default();
        if let Some(whole) = &held.whole {
            held.parts.clear();
            if <[u8; 32]>::from(Sha256::digest(whole)) == digest {
                return Ok(Assembled::Done);
            }
            held.whole = None;
            return Ok(Assembled::Mismatch);
        }
        let missing: Vec<u32> = (0..chunks)
            .filter(|i| !held.parts.contains_key(i))
            .collect();
        if !missing.is_empty() {
            return Ok(Assembled::Missing(missing));
        }
        let whole: Vec<u8> = (0..chunks)
            .flat_map(|i| held.parts[&i].iter().copied())
            .collect();
        held.parts.clear();
        if <[u8; 32]>::from(Sha256::digest(&whole)) != digest {
            return Ok(Assembled::Mismatch);
        }
        held.whole = Some(whole);
        Ok(Assembled::Done)
    }

    async fn read(&self, note: &str, object: &str, at: u64, len: u64) -> Result<Option<Vec<u8>>> {
        let objects = self.objects.lock().unwrap();
        Ok(objects
            .get(&(note.to_string(), object.to_string()))
            .and_then(|held| held.whole.as_ref())
            .and_then(|whole| whole.get(at as usize..(at + len) as usize))
            .map(<[u8]>::to_vec))
    }

    async fn remove(&self, note: &str, object: &str) -> Result<()> {
        self.objects
            .lock()
            .unwrap()
            .remove(&(note.to_string(), object.to_string()));
        Ok(())
    }

    async fn remove_note(&self, note: &str) -> Result<()> {
        self.objects.lock().unwrap().retain(|(n, _), _| n != note);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// In a directory
// ---------------------------------------------------------------------------

/// Media in a directory on a persistent volume.
///
/// ```text
/// <root>/objects/<note>/<object>          an assembled object
/// <root>/parts/<note>/<object>/<index>    chunks of one not yet assembled
/// <root>/tmp/                             half-written files; emptied on open
/// ```
///
/// Every file is written to `tmp/` and renamed into place, so a crash leaves a
/// whole chunk or none, and a whole object or none. The relay's metadata says
/// which objects are committed; one assembled here and not yet recorded there
/// is finished by the next commit ([`Blobs::assemble`]).
#[derive(Clone)]
pub struct DirBlobs {
    root: PathBuf,
}

fn io(e: std::io::Error) -> RelayError {
    RelayError::Storage(e.to_string())
}

fn not_found_ok(result: std::io::Result<()>) -> Result<()> {
    match result {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(io(e)),
        _ => Ok(()),
    }
}

impl DirBlobs {
    pub fn open(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        for dir in ["objects", "parts"] {
            std::fs::create_dir_all(root.join(dir)).map_err(io)?;
        }
        let tmp = root.join("tmp");
        not_found_ok(std::fs::remove_dir_all(&tmp))?;
        std::fs::create_dir_all(&tmp).map_err(io)?;
        Ok(Self { root })
    }

    fn object_path(&self, note: &str, object: &str) -> PathBuf {
        self.root.join("objects").join(note).join(object)
    }

    fn parts_dir(&self, note: &str, object: &str) -> PathBuf {
        self.root.join("parts").join(note).join(object)
    }

    fn tmp(&self) -> PathBuf {
        self.root.join("tmp").join(MediaObjectId::random().encode())
    }

    /// Write `bytes` to `to` by way of `tmp/`.
    fn write(&self, to: &std::path::Path, bytes: &[u8]) -> Result<()> {
        use std::io::Write;
        let tmp = self.tmp();
        let written = (|| {
            let mut file = std::fs::File::create(&tmp)?;
            file.write_all(bytes)?;
            file.sync_all()?;
            if let Some(dir) = to.parent() {
                std::fs::create_dir_all(dir)?;
            }
            std::fs::rename(&tmp, to)
        })();
        if written.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        written.map_err(io)
    }

    fn assemble_blocking(
        &self,
        note: &str,
        object: &str,
        chunks: u32,
        digest: [u8; 32],
    ) -> Result<Assembled> {
        use std::io::Write;
        let parts = self.parts_dir(note, object);
        let to = self.object_path(note, object);
        if to.exists() {
            not_found_ok(std::fs::remove_dir_all(&parts))?;
            let whole = std::fs::read(&to).map_err(io)?;
            if <[u8; 32]>::from(Sha256::digest(&whole)) == digest {
                return Ok(Assembled::Done);
            }
            // Assembled for a commit that never recorded, and not what this
            // one names: drop it, as unassembled chunks would be.
            not_found_ok(std::fs::remove_file(&to))?;
            return Ok(Assembled::Mismatch);
        }
        let missing: Vec<u32> = (0..chunks)
            .filter(|i| !parts.join(i.to_string()).exists())
            .collect();
        if !missing.is_empty() {
            return Ok(Assembled::Missing(missing));
        }
        let tmp = self.tmp();
        let mut hash = Sha256::new();
        let written = (|| {
            let mut file = std::fs::File::create(&tmp)?;
            for i in 0..chunks {
                let chunk = std::fs::read(parts.join(i.to_string()))?;
                hash.update(&chunk);
                file.write_all(&chunk)?;
            }
            file.sync_all()
        })();
        if let Err(e) = written {
            let _ = std::fs::remove_file(&tmp);
            return Err(io(e));
        }
        if <[u8; 32]>::from(hash.finalize()) != digest {
            let _ = std::fs::remove_file(&tmp);
            not_found_ok(std::fs::remove_dir_all(&parts))?;
            return Ok(Assembled::Mismatch);
        }
        std::fs::create_dir_all(to.parent().expect("an object has a directory")).map_err(io)?;
        std::fs::rename(&tmp, &to).map_err(io)?;
        not_found_ok(std::fs::remove_dir_all(&parts))?;
        Ok(Assembled::Done)
    }
}

/// Run blocking file work off the async threads.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|e| RelayError::Storage(e.to_string()))?
}

#[async_trait]
impl Blobs for DirBlobs {
    async fn put_part(
        &self,
        note: &str,
        object: &str,
        index: u32,
        bytes: Bytes,
    ) -> Result<PartPut> {
        let me = self.clone();
        let path = self.parts_dir(note, object).join(index.to_string());
        blocking(move || match std::fs::read(&path) {
            Ok(existing) if existing == bytes.as_ref() => Ok(PartPut::Same),
            Ok(_) => Ok(PartPut::Differs),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                me.write(&path, &bytes)?;
                Ok(PartPut::Stored)
            }
            Err(e) => Err(io(e)),
        })
        .await
    }

    async fn parts(&self, note: &str, object: &str) -> Result<Vec<u32>> {
        let dir = self.parts_dir(note, object);
        blocking(move || {
            let entries = match std::fs::read_dir(&dir) {
                Ok(entries) => entries,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
                Err(e) => return Err(io(e)),
            };
            let mut out = Vec::new();
            for entry in entries {
                if let Some(index) = entry
                    .map_err(io)?
                    .file_name()
                    .to_str()
                    .and_then(|n| n.parse().ok())
                {
                    out.push(index);
                }
            }
            out.sort_unstable();
            Ok(out)
        })
        .await
    }

    async fn assemble(
        &self,
        note: &str,
        object: &str,
        chunks: u32,
        digest: [u8; 32],
    ) -> Result<Assembled> {
        let me = self.clone();
        let (note, object) = (note.to_string(), object.to_string());
        blocking(move || me.assemble_blocking(&note, &object, chunks, digest)).await
    }

    async fn read(&self, note: &str, object: &str, at: u64, len: u64) -> Result<Option<Vec<u8>>> {
        use std::io::{Read, Seek, SeekFrom};
        let path = self.object_path(note, object);
        blocking(move || {
            let mut file = match std::fs::File::open(&path) {
                Ok(file) => file,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(e) => return Err(io(e)),
            };
            if file.metadata().map_err(io)?.len() < at + len {
                return Ok(None);
            }
            file.seek(SeekFrom::Start(at)).map_err(io)?;
            let mut out = vec![0; len as usize];
            file.read_exact(&mut out).map_err(io)?;
            Ok(Some(out))
        })
        .await
    }

    async fn remove(&self, note: &str, object: &str) -> Result<()> {
        let (whole, parts) = (self.object_path(note, object), self.parts_dir(note, object));
        blocking(move || {
            not_found_ok(std::fs::remove_file(whole))?;
            not_found_ok(std::fs::remove_dir_all(parts))
        })
        .await
    }

    async fn remove_note(&self, note: &str) -> Result<()> {
        let (objects, parts) = (
            self.root.join("objects").join(note),
            self.root.join("parts").join(note),
        );
        blocking(move || {
            not_found_ok(std::fs::remove_dir_all(objects))?;
            not_found_ok(std::fs::remove_dir_all(parts))
        })
        .await
    }
}

// ---------------------------------------------------------------------------
// The endpoints
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
pub struct ReserveRequest {
    pub sealed_len: u64,
    pub chunk_size: u32,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct MediaStatus {
    pub committed: bool,
    /// Chunks received, ascending. Empty once committed.
    pub received: Vec<u32>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CommitRequest {
    /// SHA-256 over the sealed chunks in order, base64url: the descriptor's
    /// `digest`.
    pub digest: String,
}

/// The device a request's bearer token names, or 401.
fn authorise(state: &AppState, headers: &HeaderMap) -> Result<()> {
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or(RelayError::Unauthorized)?;
    state
        .auth
        .device_for(token)
        .map(|_| ())
        .ok_or(RelayError::Unauthorized)
}

fn ids(note: &str, object: &str) -> Result<()> {
    NoteId::decode(note).map_err(|_| RelayError::BadRequest("malformed note id".into()))?;
    MediaObjectId::decode(object)
        .map_err(|_| RelayError::BadRequest("malformed object id".into()))?;
    Ok(())
}

fn blobs(state: &AppState) -> Result<&Arc<dyn Blobs>> {
    state
        .media
        .blobs
        .as_ref()
        .ok_or(RelayError::Unavailable("media storage is not configured"))
}

fn now() -> i64 {
    crate::auth::unix_now() as i64
}

/// The lock one object's writes are serialised under. Keyed apart from notes,
/// whose IDs cannot contain a `/`.
fn lock_key(note: &str, object: &str) -> String {
    format!("{note}/{object}")
}

async fn row(state: &AppState, note: &str, object: &str) -> Result<MediaRow> {
    state
        .store
        .media(note, object)
        .await?
        .ok_or(RelayError::NotFound)
}

/// Drop uploads that were abandoned. Run on each reservation, which is when
/// the space would be wanted; no timer to forget to start.
async fn expire(state: &Arc<AppState>, blobs: &Arc<dyn Blobs>) -> Result<()> {
    let before = now() - state.media.limits.staging_ttl_secs;
    for (note, object) in state.store.expire_media(before).await? {
        // Under the object's lock, after its row is gone: a chunk being
        // written as it expired lands first and is removed with the rest.
        let _object = state.append_lock(&lock_key(&note, &object)).await;
        blobs.remove(&note, &object).await?;
    }
    Ok(())
}

pub(crate) async fn reserve(
    State(state): State<Arc<AppState>>,
    Path((note, object)): Path<(String, String)>,
    headers: HeaderMap,
    Json(request): Json<ReserveRequest>,
) -> Result<StatusCode> {
    authorise(&state, &headers)?;
    ids(&note, &object)?;
    let blobs = blobs(&state)?;
    let geometry = Geometry {
        sealed_len: request.sealed_len,
        chunk_size: request.chunk_size,
    };
    geometry.check()?;
    expire(&state, blobs).await?;
    match state
        .store
        .reserve_media(&note, &object, geometry, &state.media.limits, now())
        .await?
    {
        Reserved::New => Ok(StatusCode::CREATED),
        Reserved::Same => Ok(StatusCode::OK),
        Reserved::Conflict => Err(RelayError::Conflict(
            "that object is already reserved with other sizes",
        )),
        Reserved::NoteFull => {
            state.metrics.refused(crate::metrics::Refused::MediaNote);
            Err(RelayError::NoteFull)
        }
        Reserved::RelayFull => {
            state.metrics.refused(crate::metrics::Refused::MediaRelay);
            Err(RelayError::RelayFull)
        }
        Reserved::Gone => Err(RelayError::TopicGone),
    }
}

pub(crate) async fn put_chunk(
    State(state): State<Arc<AppState>>,
    Path((note, object, index)): Path<(String, String, u32)>,
    client: Option<axum::Extension<crate::app::Client>>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<StatusCode> {
    authorise(&state, &headers)?;
    ids(&note, &object)?;
    // Charged before anything is written, and for a retry of a chunk already
    // stored too: the budget is on what a client sends, which is what a flood
    // costs whether or not it is new.
    state
        .spend_writes(client.map(|c| c.0), body.len())
        .map_err(RelayError::RateLimited)?;
    let blobs = blobs(&state)?;
    let _object = state.append_lock(&lock_key(&note, &object)).await;
    let row = row(&state, &note, &object).await?;
    let geometry = row.geometry;
    if index >= geometry.chunks() {
        return Err(RelayError::BadRequest("no such chunk".into()));
    }
    let (at, len) = geometry.part(index);
    if body.len() as u64 != len {
        return Err(RelayError::BadRequest(format!(
            "chunk {index} is {len} bytes"
        )));
    }
    if row.committed.is_some() {
        // A retry after the commit it raced: the same bytes are fine.
        return match blobs.read(&note, &object, at, len).await? {
            Some(existing) if existing == body.as_ref() => Ok(StatusCode::OK),
            _ => Err(RelayError::Conflict("that object is committed")),
        };
    }
    match blobs.put_part(&note, &object, index, body).await? {
        PartPut::Stored => {
            state.store.touch_media(&note, &object, now()).await?;
            Ok(StatusCode::CREATED)
        }
        PartPut::Same => Ok(StatusCode::OK),
        PartPut::Differs => Err(RelayError::Conflict("different bytes for that chunk")),
    }
}

pub(crate) async fn status(
    State(state): State<Arc<AppState>>,
    Path((note, object)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Json<MediaStatus>> {
    authorise(&state, &headers)?;
    ids(&note, &object)?;
    let blobs = blobs(&state)?;
    let row = row(&state, &note, &object).await?;
    Ok(Json(match row.committed {
        Some(_) => MediaStatus {
            committed: true,
            received: Vec::new(),
        },
        None => MediaStatus {
            committed: false,
            received: blobs.parts(&note, &object).await?,
        },
    }))
}

pub(crate) async fn commit(
    State(state): State<Arc<AppState>>,
    Path((note, object)): Path<(String, String)>,
    headers: HeaderMap,
    Json(request): Json<CommitRequest>,
) -> Result<StatusCode> {
    authorise(&state, &headers)?;
    ids(&note, &object)?;
    let blobs = blobs(&state)?;
    let digest: [u8; 32] = URL_SAFE_NO_PAD
        .decode(&request.digest)
        .ok()
        .and_then(|d| d.try_into().ok())
        .ok_or_else(|| RelayError::BadRequest("a digest is 32 bytes of base64url".into()))?;
    let _object = state.append_lock(&lock_key(&note, &object)).await;
    let row = row(&state, &note, &object).await?;
    if let Some(committed) = row.committed {
        return if committed == digest {
            Ok(StatusCode::OK)
        } else {
            Err(RelayError::Conflict("committed under another digest"))
        };
    }
    match blobs
        .assemble(&note, &object, row.geometry.chunks(), digest)
        .await?
    {
        Assembled::Done => {
            state.store.commit_media(&note, &object, digest).await?;
            Ok(StatusCode::OK)
        }
        Assembled::Missing(_) => Err(RelayError::Conflict("chunks are missing")),
        Assembled::Mismatch => Err(RelayError::Conflict(
            "the chunks are not that digest; send them again",
        )),
    }
}

/// The byte range a `Range` header asks for, of an object `len` long. One
/// range only, which is all a chunk read needs.
fn range(headers: &HeaderMap, len: u64) -> Option<Option<(u64, u64)>> {
    let Some(raw) = headers.get(header::RANGE) else {
        return Some(None);
    };
    let spec = raw.to_str().ok()?.strip_prefix("bytes=")?;
    let (from, to) = spec.split_once('-')?;
    let from: u64 = from.trim().parse().ok()?;
    let to: u64 = match to.trim() {
        "" => len.checked_sub(1)?,
        to => to.parse::<u64>().ok()?.min(len.checked_sub(1)?),
    };
    (from <= to && to < len).then(|| Some((from, to - from + 1)))
}

pub(crate) async fn fetch(
    State(state): State<Arc<AppState>>,
    Path((note, object)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Response> {
    authorise(&state, &headers)?;
    ids(&note, &object)?;
    let blobs = blobs(&state)?;
    let row = row(&state, &note, &object).await?;
    if row.committed.is_none() {
        return Err(RelayError::NotFound);
    }
    let len = row.geometry.sealed_len;
    let Some(wanted) = range(&headers, len) else {
        return Ok((
            StatusCode::RANGE_NOT_SATISFIABLE,
            [(header::CONTENT_RANGE, format!("bytes */{len}"))],
        )
            .into_response());
    };
    let (at, count) = wanted.unwrap_or((0, len));
    let bytes = blobs
        .read(&note, &object, at, count)
        .await?
        .ok_or_else(|| {
            // Committed in the metadata and not in the directory: a restore
            // that brought back one without the other. Readers report it and
            // a device holding the object uploads it again (ADR 0027 §4).
            tracing::warn!("a committed object is missing its bytes");
            RelayError::NotFound
        })?;
    let mut response = bytes.into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    // Private: a shared cache has no business keeping it, encrypted or not.
    // Immutable: an object never changes once committed.
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, max-age=31536000, immutable"),
    );
    if wanted.is_some() {
        headers.insert(
            header::CONTENT_RANGE,
            HeaderValue::from_str(&format!("bytes {at}-{}/{len}", at + count - 1))
                .expect("digits and dashes are a header value"),
        );
        *response.status_mut() = StatusCode::PARTIAL_CONTENT;
    }
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn geometry_follows_the_core() {
        let g = Geometry {
            sealed_len: (1 << 20) * 2 + 40 * 3 + 5,
            chunk_size: 1 << 20,
        };
        assert_eq!(g.chunks(), 3);
        assert_eq!(g.part(2), (2 * ((1 << 20) + 40), 45));
        g.check().unwrap();

        // A last chunk that is all overhead is no chunk the core makes.
        let g = Geometry {
            sealed_len: (1 << 20) + 40 + 40,
            chunk_size: 1 << 20,
        };
        assert!(g.check().is_err());
        for chunk_size in [0, 1024, MAX_CHUNK_SIZE + 1] {
            let g = Geometry {
                sealed_len: 100,
                chunk_size,
            };
            assert!(g.check().is_err());
        }
        let g = Geometry {
            sealed_len: MAX_AUDIO_BYTES + 51 * 40 + 1,
            chunk_size: 1 << 20,
        };
        assert!(g.check().is_err());
    }

    #[test]
    fn ranges() {
        let with = |v: &str| {
            let mut h = HeaderMap::new();
            h.insert(header::RANGE, HeaderValue::from_str(v).unwrap());
            h
        };
        assert_eq!(range(&HeaderMap::new(), 10), Some(None));
        assert_eq!(range(&with("bytes=2-5"), 10), Some(Some((2, 4))));
        assert_eq!(range(&with("bytes=2-"), 10), Some(Some((2, 8))));
        assert_eq!(range(&with("bytes=8-99"), 10), Some(Some((8, 2))));
        assert_eq!(range(&with("bytes=10-"), 10), None);
        assert_eq!(range(&with("bytes=5-2"), 10), None);
        assert_eq!(range(&with("bytes=-5"), 10), None);
    }
}
