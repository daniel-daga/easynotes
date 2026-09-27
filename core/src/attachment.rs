//! Photos and sound files on a note.
//!
//! An attachment is two things that travel separately
//! ([ADR 0027](../../docs/adr/0027-attachments.md)):
//!
//! * a **descriptor** in the note's own document — what the file is called,
//!   what it is, how big, and the key and digests that open and check its bytes.
//!   It is small, it is written once, and it rides the note's encrypted, signed
//!   updates like a title does, so everyone who can read the note can read it
//!   and nobody else can;
//! * the **media**: the file's bytes, encrypted in fixed-size chunks under a
//!   key of its own ([`seal`]), kept beside the note's log rather than in it
//!   ([`store`]), and never put in an update or a WebSocket frame.
//!
//! This module is the descriptor, the limits a descriptor is held to, and the
//! sniffing that decides whether a file is what its name says.

use crate::error::{CoreError, Result};
use crate::ids::MediaObjectId;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde::{Deserialize, Serialize};

pub mod seal;
#[cfg(feature = "file-store")]
pub mod store;

pub use seal::{ContentKey, MediaOpener, MediaSealer, Variant};

/// Bumped when [`Media`] changes shape. A descriptor of another version is not
/// drawn: a later build's attachment is invisible to this one rather than
/// misread by it.
pub const MEDIA_VERSION: u8 = 1;

/// The plaintext in one chunk. A megabyte: few enough round trips for a
/// 50 MiB recording, little enough to hold in memory on a phone and in a tab.
/// Recorded in each [`MediaRef`], so it can change without breaking a file
/// sealed under the old value.
pub const CHUNK_SIZE: u32 = 1 << 20;

/// The largest chunk a reader accepts, whatever a descriptor claims. What
/// stops a hostile descriptor from making a device allocate a gigabyte.
pub const MAX_CHUNK_SIZE: u32 = 4 << 20;

pub const MAX_IMAGE_BYTES: u64 = 20 << 20;
pub const MAX_AUDIO_BYTES: u64 = 50 << 20;
/// Width × height of a decoded image. What a decoder allocates is this times
/// four, so it is a memory limit more than a size one.
pub const MAX_IMAGE_PIXELS: u64 = 40_000_000;
/// A thumbnail is a preview someone else made, so it is bounded by bytes and
/// by edge: 512 px on the long side, and what a JPEG or WebP of that needs.
pub const MAX_THUMBNAIL_BYTES: u64 = 256 << 10;
pub const MAX_THUMBNAIL_EDGE: u32 = 512;
/// Live attachments on one note, and their original bytes between them. A
/// concurrent add on two devices can go over both; the merge keeps every file
/// that was saved, and the next add is refused until something is removed.
pub const MAX_ATTACHMENTS_PER_NOTE: usize = 20;
pub const MAX_MEDIA_BYTES_PER_NOTE: u64 = 200 << 20;
/// In characters. A file name longer than this is not a name anyone reads, and
/// every one of them is in every snapshot of the note.
pub const MAX_NAME_CHARS: usize = 255;
pub const MAX_DESCRIPTION_CHARS: usize = 1000;

/// What a screen does with the file: draw it or play it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AttachmentKind {
    Image,
    Audio,
}

/// The formats a first release accepts, decided from the bytes and never from
/// the name ([`MediaFormat::sniff`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MediaFormat {
    Jpeg,
    Png,
    /// Static only: an animated WebP is refused, because "animated images need
    /// a later explicit decision" and a still of the first frame is not one.
    Webp,
    Mp3,
    /// MPEG-4 audio: M4A, and AAC in an MP4 container.
    M4a,
    Wav,
}

impl MediaFormat {
    pub fn kind(self) -> AttachmentKind {
        match self {
            MediaFormat::Jpeg | MediaFormat::Png | MediaFormat::Webp => AttachmentKind::Image,
            MediaFormat::Mp3 | MediaFormat::M4a | MediaFormat::Wav => AttachmentKind::Audio,
        }
    }

    pub fn mime(self) -> &'static str {
        match self {
            MediaFormat::Jpeg => "image/jpeg",
            MediaFormat::Png => "image/png",
            MediaFormat::Webp => "image/webp",
            MediaFormat::Mp3 => "audio/mpeg",
            MediaFormat::M4a => "audio/mp4",
            MediaFormat::Wav => "audio/wav",
        }
    }

    /// The largest original of this format that may be attached.
    pub fn max_bytes(self) -> u64 {
        match self.kind() {
            AttachmentKind::Image => MAX_IMAGE_BYTES,
            AttachmentKind::Audio => MAX_AUDIO_BYTES,
        }
    }

    /// What `head` — the first few kilobytes of a file — is, or `None` for
    /// anything this release does not take.
    ///
    /// It reads containers, not codecs: an M4A holding a codec this device
    /// cannot play is still an M4A, and stays attached with a player that says
    /// so. That is the plan's rule, and it is also all a core with no decoder in
    /// it can honestly check.
    pub fn sniff(head: &[u8]) -> Option<Self> {
        if head.starts_with(&[0xFF, 0xD8, 0xFF]) {
            return Some(MediaFormat::Jpeg);
        }
        if head.starts_with(b"\x89PNG\r\n\x1a\n") {
            return Some(MediaFormat::Png);
        }
        if head.len() >= 12 && &head[..4] == b"RIFF" {
            match &head[8..12] {
                b"WEBP" => return Some(MediaFormat::Webp),
                b"WAVE" => return Some(MediaFormat::Wav),
                _ => return None,
            }
        }
        if head.starts_with(b"ID3")
            || (head.len() >= 2 && head[0] == 0xFF && head[1] & 0xE0 == 0xE0)
        {
            return Some(MediaFormat::Mp3);
        }
        if head.len() >= 12 && &head[4..8] == b"ftyp" {
            // The major brand. `M4A `/`M4B ` say audio outright; the generic
            // MPEG-4 brands are what many recorders write for AAC.
            return match &head[8..12] {
                b"M4A " | b"M4B " | b"mp42" | b"mp41" | b"isom" | b"iso2" => Some(MediaFormat::M4a),
                _ => None,
            };
        }
        None
    }
}

/// An image's pixel size, read from its header, or `None` when the header does
/// not say or is not one this reads.
///
/// Refuses an animated WebP by answering `Err`, since a caller that asked for
/// the size of a WebP is about to accept it.
pub fn image_size(format: MediaFormat, head: &[u8]) -> Result<Option<(u32, u32)>> {
    let be16 = |at: usize| {
        head.get(at..at + 2)
            .map(|b| u16::from_be_bytes([b[0], b[1]]))
    };
    let le16 = |at: usize| {
        head.get(at..at + 2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
    };
    let le24 = |at: usize| {
        head.get(at..at + 3)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], 0]))
    };
    Ok(match format {
        MediaFormat::Png => head.get(16..24).map(|b| {
            (
                u32::from_be_bytes(b[..4].try_into().unwrap()),
                u32::from_be_bytes(b[4..].try_into().unwrap()),
            )
        }),
        MediaFormat::Jpeg => {
            // Walk the segments to the first start-of-frame. Every segment
            // before it carries its own length, so this is a skip, not a parse.
            let mut at = 2;
            let mut size = None;
            while at + 4 <= head.len() {
                if head[at] != 0xFF {
                    break;
                }
                let marker = head[at + 1];
                if marker == 0xFF {
                    at += 1;
                    continue;
                }
                let is_sof =
                    (0xC0..=0xCF).contains(&marker) && !matches!(marker, 0xC4 | 0xC8 | 0xCC);
                if is_sof {
                    size = match (be16(at + 5), be16(at + 7)) {
                        (Some(h), Some(w)) => Some((w as u32, h as u32)),
                        _ => None,
                    };
                    break;
                }
                let Some(len) = be16(at + 2) else { break };
                at += 2 + len as usize;
            }
            size
        }
        MediaFormat::Webp => match head.get(12..16) {
            Some(b"VP8 ") => match (le16(26), le16(28)) {
                (Some(w), Some(h)) => Some(((w & 0x3FFF) as u32, (h & 0x3FFF) as u32)),
                _ => None,
            },
            Some(b"VP8L") => head.get(21..25).map(|b| {
                let bits = u32::from_le_bytes(b.try_into().unwrap());
                ((bits & 0x3FFF) + 1, ((bits >> 14) & 0x3FFF) + 1)
            }),
            Some(b"VP8X") => {
                if head.get(20).is_some_and(|flags| flags & 0x02 != 0) {
                    return Err(CoreError::Attachment(
                        "animated images are not supported yet".into(),
                    ));
                }
                match (le24(24), le24(27)) {
                    (Some(w), Some(h)) => Some((w + 1, h + 1)),
                    _ => None,
                }
            }
            _ => None,
        },
        _ => None,
    })
}

/// Where one sealed variant is, and everything a reader needs to check that
/// what it got is all of it and nothing else.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaRef {
    pub object: MediaObjectId,
    /// The plaintext length. Bound into every chunk, so a file cut short is a
    /// file whose chunks no longer open, not a shorter picture.
    pub len: u64,
    pub chunk_size: u32,
    /// SHA-256 of the sealed chunks laid end to end: the final commitment. The
    /// chunks already refuse to open out of place; this refuses a whole
    /// re-encryption by someone else who holds the key.
    #[serde(with = "b64_32")]
    pub digest: [u8; 32],
}

impl MediaRef {
    pub fn chunks(&self) -> u32 {
        seal::chunk_count(self.len, self.chunk_size)
    }

    /// Bytes the sealed object occupies.
    pub fn sealed_len(&self) -> u64 {
        self.len + self.chunks() as u64 * seal::CHUNK_OVERHEAD as u64
    }
}

/// A thumbnail's pixel size is recorded, so a grid can lay itself out before
/// anything is downloaded.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Thumbnail {
    pub media: MediaRef,
    pub format: MediaFormat,
    pub width: u32,
    pub height: u32,
}

/// The immutable half of a descriptor: what the file is, and how to open it.
///
/// Written once, when the attachment is added, as one register holding this
/// as JSON. One register rather than a field each because none of it ever
/// changes on its own — a new file is a new attachment — and one value cannot
/// be half-merged.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Media {
    pub v: u8,
    pub format: MediaFormat,
    /// What the person's file was called. Shown, searched, and used for Save a
    /// copy; never used to decide what the bytes are.
    pub name: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub duration_ms: Option<u64>,
    /// `None` once the attachment is removed: a tombstone keeps where its bytes
    /// were, for the relay's clean-up, and forgets how to read them, so a
    /// person who joins the note afterwards is not handed a key to something
    /// that was taken away before they came.
    pub key: Option<ContentKey>,
    pub original: MediaRef,
    pub thumbnail: Option<Thumbnail>,
}

impl Media {
    pub fn kind(&self) -> AttachmentKind {
        self.format.kind()
    }

    pub fn encode(&self) -> String {
        serde_json::to_string(self).expect("a descriptor always serialises")
    }

    /// Read a descriptor from a document, or `None` for one this build should
    /// not draw: another version, malformed, or over a limit. A peer's document
    /// is input like any other, and an attachment this cannot trust is one it
    /// leaves alone rather than one it fails the whole note over.
    pub fn decode(raw: &str) -> Option<Self> {
        let media: Self = serde_json::from_str(raw).ok()?;
        media.check().ok()?;
        Some(media)
    }

    /// The same without the key, for a removal to write over this one.
    pub fn tombstone(&self) -> Self {
        Self {
            key: None,
            ..self.clone()
        }
    }

    /// The limits, as a peer's descriptor is held to them as much as this
    /// device's own.
    pub fn check(&self) -> Result<()> {
        let bad = |why: &str| Err(CoreError::Attachment(why.into()));
        if self.v != MEDIA_VERSION {
            return bad("an attachment from a newer version of EasyNotes");
        }
        let name_chars = self.name.chars().count();
        if name_chars == 0 || name_chars > MAX_NAME_CHARS || self.name.chars().any(char::is_control)
        {
            return bad("an attachment needs a name of up to 255 characters");
        }
        if self.original.len == 0 {
            return bad("the file is empty");
        }
        if self.original.len > self.format.max_bytes() {
            return bad(match self.kind() {
                AttachmentKind::Image => "images can be up to 20 MB",
                AttachmentKind::Audio => "sound files can be up to 50 MB",
            });
        }
        if let (Some(w), Some(h)) = (self.width, self.height) {
            if w as u64 * h as u64 > MAX_IMAGE_PIXELS {
                return bad("images can be up to 40 megapixels");
            }
        }
        for media in
            std::iter::once(&self.original).chain(self.thumbnail.as_ref().map(|t| &t.media))
        {
            if media.chunk_size == 0 || media.chunk_size > MAX_CHUNK_SIZE {
                return bad("an attachment with an impossible chunk size");
            }
        }
        if let Some(thumb) = &self.thumbnail {
            if thumb.format.kind() != AttachmentKind::Image
                || thumb.media.len == 0
                || thumb.media.len > MAX_THUMBNAIL_BYTES
                || thumb.width > MAX_THUMBNAIL_EDGE
                || thumb.height > MAX_THUMBNAIL_EDGE
            {
                return bad("a thumbnail must be an image of at most 512 pixels a side");
            }
            if thumb.media.object == self.original.object {
                return bad("a thumbnail cannot share its original's object");
            }
        }
        Ok(())
    }
}

/// One live attachment, as a screen sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttachmentView {
    pub id: crate::ids::AttachmentId,
    /// A fractional index, like an item's, among this note's attachments only.
    pub order: String,
    /// The shared, optional words for someone who cannot see the picture.
    pub description: String,
    pub media: Media,
}

/// Validate a description before it is written.
pub fn check_description(description: &str) -> Result<()> {
    if description.chars().count() > MAX_DESCRIPTION_CHARS {
        return Err(CoreError::Attachment(
            "a description can be up to 1000 characters".into(),
        ));
    }
    Ok(())
}

mod b64_32 {
    use super::*;
    use serde::{Deserializer, Serializer};

    pub fn serialize<S: Serializer>(
        bytes: &[u8; 32],
        s: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(&URL_SAFE_NO_PAD.encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> std::result::Result<[u8; 32], D::Error> {
        let s = String::deserialize(d)?;
        URL_SAFE_NO_PAD
            .decode(s)
            .ok()
            .and_then(|v| v.try_into().ok())
            .ok_or_else(|| serde::de::Error::custom("expected 32 bytes of base64url"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_are_read_from_the_bytes() {
        assert_eq!(
            MediaFormat::sniff(b"\xFF\xD8\xFF\xE0rest"),
            Some(MediaFormat::Jpeg)
        );
        assert_eq!(
            MediaFormat::sniff(b"\x89PNG\r\n\x1a\n...."),
            Some(MediaFormat::Png)
        );
        assert_eq!(
            MediaFormat::sniff(b"RIFF\0\0\0\0WEBPVP8 "),
            Some(MediaFormat::Webp)
        );
        assert_eq!(
            MediaFormat::sniff(b"RIFF\0\0\0\0WAVEfmt "),
            Some(MediaFormat::Wav)
        );
        assert_eq!(MediaFormat::sniff(b"RIFF\0\0\0\0AVI LIST"), None);
        assert_eq!(MediaFormat::sniff(b"ID3\x04\0"), Some(MediaFormat::Mp3));
        assert_eq!(
            MediaFormat::sniff(b"\xFF\xFB\x90\x00"),
            Some(MediaFormat::Mp3)
        );
        assert_eq!(
            MediaFormat::sniff(b"\0\0\0\x20ftypM4A \0\0"),
            Some(MediaFormat::M4a)
        );
        assert_eq!(MediaFormat::sniff(b"\0\0\0\x20ftypqt  \0\0"), None);
        assert_eq!(MediaFormat::sniff(b"%PDF-1.7"), None);
        assert_eq!(MediaFormat::sniff(b""), None);
    }

    #[test]
    fn image_sizes_come_from_the_header() {
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
        png.extend_from_slice(&640u32.to_be_bytes());
        png.extend_from_slice(&480u32.to_be_bytes());
        assert_eq!(
            image_size(MediaFormat::Png, &png).unwrap(),
            Some((640, 480))
        );

        // SOI, an APP0 of length 16, then SOF0 with height 300, width 400.
        let mut jpeg = vec![0xFF, 0xD8, 0xFF, 0xE0, 0, 16];
        jpeg.extend_from_slice(&[0; 14]);
        jpeg.extend_from_slice(&[0xFF, 0xC0, 0, 17, 8, 0x01, 0x2C, 0x01, 0x90]);
        assert_eq!(
            image_size(MediaFormat::Jpeg, &jpeg).unwrap(),
            Some((400, 300))
        );

        let mut animated = b"RIFF\0\0\0\0WEBPVP8X\x0a\0\0\0".to_vec();
        animated.push(0x02);
        animated.extend_from_slice(&[0; 9]);
        assert!(image_size(MediaFormat::Webp, &animated).is_err());

        let mut still = b"RIFF\0\0\0\0WEBPVP8X\x0a\0\0\0".to_vec();
        still.push(0x00);
        still.extend_from_slice(&[0, 0, 0]);
        still.extend_from_slice(&[99, 0, 0, 49, 0, 0]);
        assert_eq!(
            image_size(MediaFormat::Webp, &still).unwrap(),
            Some((100, 50))
        );
    }
}
