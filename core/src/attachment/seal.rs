//! Encrypting an attached file, a chunk at a time.
//!
//! The format ([ADR 0027](../../../docs/decisions.md#adr-0027)):
//!
//! * Each attachment has a random 32-byte **content key**, carried in its
//!   descriptor. The original and the thumbnail are sealed under two subkeys
//!   derived from it with HKDF-SHA256, one per [`Variant`], so neither can be
//!   opened as the other even before the associated data says which is which.
//! * The plaintext is cut into chunks of `chunk_size` bytes, the last one
//!   shorter. Each is sealed with XChaCha20-Poly1305 under a fresh random nonce
//!   and stored as `nonce | ciphertext | tag` — [`CHUNK_OVERHEAD`] bytes more
//!   than it holds, so where chunk *n* starts is arithmetic.
//! * Every chunk's associated data binds the note, the attachment, the variant,
//!   its own index, the chunk count, the total length and the chunk size. A
//!   chunk moved to another place, another file or another note does not open;
//!   a file cut short or padded out is missing a chunk the count says exists.
//! * The descriptor records SHA-256 over the sealed chunks end to end. That is
//!   the final commitment: the chunks already refuse to be rearranged, and the
//!   digest refuses a whole different encryption by another holder of the key.
//!
//! No plaintext leaves [`MediaOpener::open_chunk`] until its tag has verified,
//! so a decoder never sees a byte an attacker chose.

use super::MediaRef;
use crate::crypto::{local_aad, random_bytes, NONCE_LEN};
use crate::error::{CoreError, Result};
use crate::ids::{AttachmentId, MediaObjectId, NoteId};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use hkdf::Hkdf;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};
use zeroize::{Zeroize, ZeroizeOnDrop};

const MEDIA_DOMAIN: &[u8] = b"easynotes/media/v1";
const TAG_LEN: usize = 16;

/// What sealing adds to each chunk: its nonce and its tag.
pub const CHUNK_OVERHEAD: usize = NONCE_LEN + TAG_LEN;

/// How many chunks `len` bytes make. Never zero: an empty file is refused
/// before it gets here, and a count of zero would be a file with nothing to
/// authenticate.
pub fn chunk_count(len: u64, chunk_size: u32) -> u32 {
    if chunk_size == 0 {
        return 0;
    }
    len.div_ceil(chunk_size as u64).max(1) as u32
}

/// The key to one attachment's bytes.
#[derive(Clone, PartialEq, Eq, Zeroize, ZeroizeOnDrop)]
pub struct ContentKey(pub [u8; 32]);

impl ContentKey {
    pub fn random() -> Self {
        let mut k = [0u8; 32];
        random_bytes(&mut k);
        Self(k)
    }

    fn cipher(&self, variant: Variant) -> XChaCha20Poly1305 {
        let mut key = [0u8; 32];
        Hkdf::<Sha256>::new(None, &self.0)
            .expand(&[MEDIA_DOMAIN, &[variant as u8]].concat(), &mut key)
            .expect("HKDF-SHA256 can always produce 32 bytes");
        let cipher = XChaCha20Poly1305::new((&key).into());
        key.zeroize();
        cipher
    }
}

impl std::fmt::Debug for ContentKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ContentKey(redacted)")
    }
}

impl Serialize for ContentKey {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(&URL_SAFE_NO_PAD.encode(self.0))
    }
}

impl<'de> Deserialize<'de> for ContentKey {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        let mut raw = URL_SAFE_NO_PAD
            .decode(s)
            .map_err(|_| serde::de::Error::custom("a content key is base64url"))?;
        let key = raw
            .as_slice()
            .try_into()
            .map(ContentKey)
            .map_err(|_| serde::de::Error::custom("a content key is 32 bytes"));
        raw.zeroize();
        key
    }
}

/// Which of an attachment's files a chunk belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Variant {
    Original = 1,
    Thumbnail = 2,
}

/// Everything a chunk's associated data binds, but its index.
struct Frame {
    note: NoteId,
    attachment: AttachmentId,
    variant: Variant,
    len: u64,
    chunk_size: u32,
    chunks: u32,
}

impl Frame {
    fn aad(&self, index: u32) -> Vec<u8> {
        local_aad(
            MEDIA_DOMAIN,
            &[
                self.note.as_bytes(),
                self.attachment.as_bytes(),
                &[self.variant as u8],
                &index.to_be_bytes(),
                &self.chunks.to_be_bytes(),
                &self.len.to_be_bytes(),
                &self.chunk_size.to_be_bytes(),
            ],
        )
    }

    /// How much plaintext chunk `index` holds.
    fn plain_len(&self, index: u32) -> usize {
        if index + 1 < self.chunks {
            self.chunk_size as usize
        } else {
            (self.len - (self.chunks as u64 - 1) * self.chunk_size as u64) as usize
        }
    }
}

/// Seals one variant, chunk by chunk, in order.
///
/// The length is given up front because every chunk commits to it. A caller
/// that has a file has its size; one that is recording has no business sealing
/// until it has stopped.
pub struct MediaSealer {
    cipher: XChaCha20Poly1305,
    frame: Frame,
    object: MediaObjectId,
    next: u32,
    digest: Sha256,
}

impl MediaSealer {
    pub fn new(
        key: &ContentKey,
        note: NoteId,
        attachment: AttachmentId,
        variant: Variant,
        len: u64,
        chunk_size: u32,
    ) -> Result<Self> {
        if len == 0 {
            return Err(CoreError::Attachment("the file is empty".into()));
        }
        if chunk_size == 0 || chunk_size > super::MAX_CHUNK_SIZE {
            return Err(CoreError::Attachment("impossible chunk size".into()));
        }
        Ok(Self {
            cipher: key.cipher(variant),
            frame: Frame {
                note,
                attachment,
                variant,
                len,
                chunk_size,
                chunks: chunk_count(len, chunk_size),
            },
            object: MediaObjectId::random(),
            next: 0,
            digest: Sha256::new(),
        })
    }

    /// Where the sealed file will live. Known from the start, so a store that
    /// keeps chunks under their object's name can write each as it is sealed.
    pub fn object(&self) -> MediaObjectId {
        self.object
    }

    /// The plaintext the next chunk must hold, or `None` when all are sealed.
    pub fn next_len(&self) -> Option<usize> {
        (self.next < self.frame.chunks).then(|| self.frame.plain_len(self.next))
    }

    /// Seal the next chunk. It must be exactly [`MediaSealer::next_len`] bytes.
    pub fn seal_chunk(&mut self, plain: &[u8]) -> Result<Vec<u8>> {
        if self.next_len() != Some(plain.len()) {
            return Err(CoreError::Attachment(
                "the file changed while it was being added".into(),
            ));
        }
        let mut nonce = [0u8; NONCE_LEN];
        random_bytes(&mut nonce);
        let ciphertext = self
            .cipher
            .encrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: plain,
                    aad: &self.frame.aad(self.next),
                },
            )
            .map_err(|_| CoreError::Crypto("encryption failed"))?;
        let mut sealed = Vec::with_capacity(CHUNK_OVERHEAD + plain.len());
        sealed.extend_from_slice(&nonce);
        sealed.extend_from_slice(&ciphertext);
        self.digest.update(&sealed);
        self.next += 1;
        Ok(sealed)
    }

    /// Where the sealed file will live, and what it commits to. Refuses a
    /// sealer that has not sealed every chunk.
    pub fn finish(self) -> Result<MediaRef> {
        if self.next != self.frame.chunks {
            return Err(CoreError::Attachment(
                "the file ended before it was all read".into(),
            ));
        }
        Ok(MediaRef {
            object: self.object,
            len: self.frame.len,
            chunk_size: self.frame.chunk_size,
            digest: self.digest.finalize().into(),
        })
    }
}

/// Opens one variant's chunks, and at the end says whether they were all of it.
///
/// Chunks can be opened in any order and one at a time — a player seeking to
/// minute three needs chunk forty and not the thirty-nine before it — but
/// [`MediaOpener::verify_whole`] wants them in order, and is what a download
/// runs before it promotes a file to the cache.
pub struct MediaOpener {
    cipher: XChaCha20Poly1305,
    frame: Frame,
    media: MediaRef,
}

impl MediaOpener {
    pub fn new(
        key: &ContentKey,
        note: NoteId,
        attachment: AttachmentId,
        variant: Variant,
        media: &MediaRef,
    ) -> Result<Self> {
        if media.len == 0 || media.chunk_size == 0 || media.chunk_size > super::MAX_CHUNK_SIZE {
            return Err(CoreError::Attachment("impossible attachment".into()));
        }
        Ok(Self {
            cipher: key.cipher(variant),
            frame: Frame {
                note,
                attachment,
                variant,
                len: media.len,
                chunk_size: media.chunk_size,
                chunks: media.chunks(),
            },
            media: media.clone(),
        })
    }

    pub fn chunks(&self) -> u32 {
        self.frame.chunks
    }

    /// Where chunk `index` starts in the sealed object, and how long it is.
    pub fn sealed_range(&self, index: u32) -> (u64, usize) {
        let full = self.frame.chunk_size as u64 + CHUNK_OVERHEAD as u64;
        (
            index as u64 * full,
            self.frame.plain_len(index) + CHUNK_OVERHEAD,
        )
    }

    /// Verify and decrypt chunk `index`.
    pub fn open_chunk(&self, index: u32, sealed: &[u8]) -> Result<Vec<u8>> {
        if index >= self.frame.chunks {
            return Err(CoreError::Crypto("no such chunk"));
        }
        if sealed.len() != self.frame.plain_len(index) + CHUNK_OVERHEAD {
            return Err(CoreError::Crypto("a chunk of the wrong length"));
        }
        let (nonce, ciphertext) = sealed.split_at(NONCE_LEN);
        self.cipher
            .decrypt(
                XNonce::from_slice(nonce),
                Payload {
                    msg: ciphertext,
                    aad: &self.frame.aad(index),
                },
            )
            .map_err(|_| CoreError::Crypto("an attachment chunk does not verify"))
    }

    /// Check a whole sealed object against its descriptor without decrypting
    /// it: the length, then the digest. What a download does before trusting
    /// what it fetched, chunk tags and all, to be the file that was attached.
    pub fn verify_whole(&self, sealed: &[u8]) -> Result<()> {
        verify_sealed(&self.media, sealed)
    }

    /// Decrypt a whole sealed object, checking it first. For a thumbnail, and
    /// for a test; an original is read a chunk at a time.
    pub fn open_whole(&self, sealed: &[u8]) -> Result<Vec<u8>> {
        self.verify_whole(sealed)?;
        let mut plain = Vec::with_capacity(self.media.len as usize);
        for index in 0..self.frame.chunks {
            let (at, len) = self.sealed_range(index);
            plain.extend(self.open_chunk(index, &sealed[at as usize..at as usize + len])?);
        }
        Ok(plain)
    }
}

/// Check a whole sealed variant against the descriptor that names it: its
/// length, then its digest. Needs no key, so a device can check a download
/// before it keeps it, and the relay's bytes are never decrypted to do it.
pub fn verify_sealed(media: &MediaRef, sealed: &[u8]) -> Result<()> {
    if sealed.len() as u64 != media.sealed_len() {
        return Err(CoreError::Crypto("an attachment of the wrong length"));
    }
    if <[u8; 32]>::from(Sha256::digest(sealed)) != media.digest {
        return Err(CoreError::Crypto(
            "an attachment that is not the one attached",
        ));
    }
    Ok(())
}

/// Seal a whole buffer in one go. What a thumbnail uses, and tests.
pub fn seal_whole(
    key: &ContentKey,
    note: NoteId,
    attachment: AttachmentId,
    variant: Variant,
    plain: &[u8],
    chunk_size: u32,
) -> Result<(MediaRef, Vec<u8>)> {
    let mut sealer = MediaSealer::new(
        key,
        note,
        attachment,
        variant,
        plain.len() as u64,
        chunk_size,
    )?;
    let mut out = Vec::with_capacity(plain.len() + CHUNK_OVERHEAD);
    let mut rest = plain;
    while let Some(n) = sealer.next_len() {
        let (chunk, tail) = rest.split_at(n);
        out.extend(sealer.seal_chunk(chunk)?);
        rest = tail;
    }
    Ok((sealer.finish()?, out))
}
