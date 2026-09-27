//! What the transfer queue needs from wherever a device keeps attachment bytes,
//! and the rules every such place shares.
//!
//! There are two places. A device has a directory ([`super::store`]): a file
//! per object, synchronous, and fsynced. A tab has IndexedDB
//! ([`super::web`]): a row per chunk, and nothing it can do without waiting.
//! The queue in `sync::transfers` is written once over [`MediaStorage`], so
//! the reconciliation, the ordering and the backoff are the same code in both —
//! which is the same split `sync` makes for the socket.
//!
//! The trait is asynchronous because the browser is. The directory answers at
//! once and wraps its answer in a ready future; a 50 MB upload does its disk
//! reads a chunk at a time either way.
//!
//! What is here besides the trait is what an import has to decide before it
//! writes anything, whichever store it writes to: what the file is, whether a
//! thumbnail is one, and how a staging record is sealed. Two stores that
//! decided those separately would sooner or later accept different files.

use super::seal::{ContentKey, Variant};
use super::{
    image_size, AttachmentKind, Media, MediaFormat, MediaRef, MAX_IMAGE_PIXELS,
    MAX_THUMBNAIL_BYTES, MAX_THUMBNAIL_EDGE,
};
use crate::crypto::MasterKey;
use crate::error::{CoreError, Result};
use crate::ids::{AttachmentId, MediaObjectId, NoteId};
use serde::{Deserialize, Serialize};
use std::future::Future;
use std::pin::Pin;

/// A boxed future: `Send` on a device, where the queue runs on a
/// multi-threaded runtime, and not in a tab, where a promise cannot be.
#[cfg(not(target_arch = "wasm32"))]
pub type Fut<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
#[cfg(target_arch = "wasm32")]
pub type Fut<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

/// An import that is saved on this device and not yet in its note.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Staged {
    pub note: NoteId,
    pub id: AttachmentId,
    pub media: Media,
}

/// A preview the app made. Decoding and scaling are the platform's job — the
/// core has no image decoder in it, on purpose — so what arrives here is a
/// finished JPEG, PNG or WebP no larger than [`MAX_THUMBNAIL_EDGE`] a side.
pub struct ThumbnailInput {
    pub bytes: Vec<u8>,
}

/// What the app knows about a file that the core cannot read from its header.
#[derive(Default)]
pub struct ImportHints {
    /// A sound file's length, as the platform's decoder reported it.
    pub duration_ms: Option<u64>,
}

/// Where a device keeps attachment bytes, as the transfer queue sees it.
///
/// The queue needs only these: the staging records that are its uploads, a
/// sealed chunk to send, and somewhere a download can be written that keeps
/// nothing until the whole of it has proved to be the file attached.
pub trait MediaStorage: Send + Sync + 'static {
    type Incoming: IncomingMedia;

    /// Every import still waiting to be published, oldest first.
    fn staged(&self) -> Fut<'_, Result<Vec<Staged>>>;

    /// The descriptor is in the note: forget the record, keep the objects.
    fn settle(&self, id: AttachmentId) -> Fut<'_, Result<()>>;

    /// Cancel an import: its objects and its record.
    fn discard<'a>(&'a self, staged: &'a Staged) -> Fut<'a, Result<()>>;

    /// Whether a variant is here, whole.
    fn has<'a>(&'a self, note: NoteId, media: &'a MediaRef) -> Fut<'a, bool>;

    /// One sealed chunk of a variant, as it goes to the relay.
    fn read_sealed_chunk<'a>(
        &'a self,
        note: NoteId,
        media: &'a MediaRef,
        index: u32,
    ) -> Fut<'a, Result<Vec<u8>>>;

    /// Start keeping a variant that arrives a chunk at a time.
    fn incoming<'a>(&'a self, note: NoteId, media: &'a MediaRef)
        -> Fut<'a, Result<Self::Incoming>>;
}

/// A variant being downloaded. Each [`write`](IncomingMedia::write) is the
/// next sealed chunk, in order; [`finish`](IncomingMedia::finish) checks the
/// length and the digest and only then makes it something
/// [`MediaStorage::has`] answers yes to. Dropped unfinished, it leaves nothing
/// a reader can see.
pub trait IncomingMedia: Send {
    fn write<'a>(&'a mut self, sealed: &'a [u8]) -> Fut<'a, Result<()>>;
    fn finish(self) -> Fut<'static, Result<()>>;
}

pub(crate) fn refused(why: &str) -> CoreError {
    CoreError::Attachment(why.into())
}

/// What an original is, from its first chunk and its length, or why it cannot
/// be attached: the format, and an image's pixel size when its header says.
pub(crate) fn inspect_original(
    head: &[u8],
    len: u64,
) -> Result<(MediaFormat, Option<u32>, Option<u32>)> {
    let format = MediaFormat::sniff(head).ok_or_else(|| {
        refused("only JPEG, PNG and WebP images and MP3, M4A and WAV sound files can be attached")
    })?;
    if len > format.max_bytes() {
        return Err(refused(match format.kind() {
            AttachmentKind::Image => "images can be up to 20 MB",
            AttachmentKind::Audio => "sound files can be up to 50 MB",
        }));
    }
    let (width, height) = match format.kind() {
        AttachmentKind::Image => match image_size(format, head)? {
            Some((w, h)) if w as u64 * h as u64 > MAX_IMAGE_PIXELS => {
                return Err(refused("images can be up to 40 megapixels"))
            }
            Some((w, h)) => (Some(w), Some(h)),
            None => (None, None),
        },
        AttachmentKind::Audio => (None, None),
    };
    Ok((format, width, height))
}

/// Whether a thumbnail the app made is one: an image, small in bytes and in
/// edge. Its format and pixel size if so.
pub(crate) fn inspect_thumbnail(bytes: &[u8]) -> Result<(MediaFormat, u32, u32)> {
    let bad = || refused("a thumbnail must be an image of at most 512 pixels a side");
    if bytes.is_empty() || bytes.len() as u64 > MAX_THUMBNAIL_BYTES {
        return Err(bad());
    }
    let format = MediaFormat::sniff(bytes)
        .filter(|f| f.kind() == AttachmentKind::Image)
        .ok_or_else(bad)?;
    let (width, height) = image_size(format, bytes)?.ok_or_else(bad)?;
    if width > MAX_THUMBNAIL_EDGE || height > MAX_THUMBNAIL_EDGE {
        return Err(bad());
    }
    Ok((format, width, height))
}

/// The staging record's sealing context. It holds a content key that is in no
/// note yet, so it is sealed under the master key, bound to its own ID.
const STAGING_CONTEXT: &[u8] = b"attachment-staging";

pub(crate) fn seal_record(master: &MasterKey, staged: &Staged) -> Result<Vec<u8>> {
    let plain = serde_json::to_vec(staged)
        .map_err(|e| CoreError::Storage(format!("unwritable staging record: {e}")))?;
    master.seal(&[STAGING_CONTEXT, staged.id.as_bytes()], &plain)
}

/// A record that no longer opens — another master key, a torn write — or that
/// names another ID than the one it is filed under is not one this device can
/// do anything with, and is `None` rather than an error for the rest.
pub(crate) fn open_record(master: &MasterKey, id: AttachmentId, sealed: &[u8]) -> Option<Staged> {
    let plain = master
        .open(&[STAGING_CONTEXT, id.as_bytes()], sealed)
        .ok()?;
    let staged: Staged = serde_json::from_slice(&plain).ok()?;
    (staged.id == id).then_some(staged)
}

pub(crate) fn variant_of(media: &Media, variant: Variant) -> Result<(&ContentKey, &MediaRef)> {
    let key = media
        .key
        .as_ref()
        .ok_or_else(|| refused("this attachment was removed"))?;
    let media_ref =
        variant_ref(media, variant).ok_or_else(|| refused("this attachment has no thumbnail"))?;
    Ok((key, media_ref))
}

/// Where one variant of an attachment is, or `None` for a thumbnail it lacks.
pub fn variant_ref(media: &Media, variant: Variant) -> Option<&MediaRef> {
    match variant {
        Variant::Original => Some(&media.original),
        Variant::Thumbnail => media.thumbnail.as_ref().map(|t| &t.media),
    }
}

pub(crate) fn objects_of(media: &Media) -> impl Iterator<Item = MediaObjectId> + '_ {
    std::iter::once(media.original.object).chain(media.thumbnail.as_ref().map(|t| t.media.object))
}
