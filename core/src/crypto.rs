//! Crypto for one note.
//!
//! Standard constructions only, no hand-rolled primitives:
//!   * XChaCha20-Poly1305 for every update and snapshot (`chacha20poly1305`)
//!   * Ed25519 for authorship (`ed25519-dalek`)
//!   * X25519 for the key agreement that Slice 3's invite links will need (`x25519-dalek`)
//!
//! The one rule this module exists to enforce: **a receiver verifies the author's
//! signature before it decrypts anything.** `Envelope::open` is the only way to get
//! plaintext out, and it verifies first.

use crate::error::{CoreError, Result};
use crate::ids::NoteId;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use zeroize::{Zeroize, ZeroizeOnDrop};

/// Wire-format version. Bound into the AAD and the signature, so an attacker
/// cannot re-present a v1 envelope as some future version.
pub const ENVELOPE_VERSION: u8 = 1;

pub const NONCE_LEN: usize = 24;
pub const SIG_LEN: usize = 64;
pub const PUBKEY_LEN: usize = 32;
pub const NOTE_KEY_LEN: usize = 32;

/// version | note_id | author | counter | nonce | signature
pub const ENVELOPE_HEADER_LEN: usize = 1 + 16 + PUBKEY_LEN + 8 + NONCE_LEN + SIG_LEN;

/// The relay enforces this too; see `relay::protocol`. Big enough for a snapshot of
/// a few thousand checklist items, small enough that a single client cannot use the
/// relay as bulk storage.
pub const MAX_ENVELOPE_BYTES: usize = 1 << 20; // 1 MiB

pub fn random_bytes(dest: &mut [u8]) {
    getrandom::fill(dest).expect("the platform RNG must work; there is no safe fallback");
}

// ---------------------------------------------------------------------------
// Device identity
// ---------------------------------------------------------------------------

/// The 64-byte secret a device is. In production this lives in the OS keychain;
/// Slice 1 keeps it in a file and says so in the slice write-up.
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct DeviceSeed(pub [u8; 64]);

/// Ed25519 for signing updates, X25519 for the wrapped-key exchange Slice 3 adds.
/// Both are derived from one seed so a device has exactly one thing to back up.
pub struct DeviceIdentity {
    signing: SigningKey,
    x25519: x25519_dalek::StaticSecret,
}

impl DeviceIdentity {
    pub fn generate() -> Self {
        let mut seed = DeviceSeed([0u8; 64]);
        random_bytes(&mut seed.0);
        Self::from_seed(&seed)
    }

    pub fn from_seed(seed: &DeviceSeed) -> Self {
        let mut ed = [0u8; 32];
        let mut x = [0u8; 32];
        ed.copy_from_slice(&seed.0[..32]);
        x.copy_from_slice(&seed.0[32..]);
        let identity = Self {
            signing: SigningKey::from_bytes(&ed),
            x25519: x25519_dalek::StaticSecret::from(x),
        };
        ed.zeroize();
        x.zeroize();
        identity
    }

    /// Ed25519 public key. This is the device's identity everywhere: it is what the
    /// relay authenticates, and what collaborators check updates against.
    pub fn public_key(&self) -> AuthorKey {
        AuthorKey(self.signing.verifying_key().to_bytes())
    }

    pub fn x25519_public(&self) -> [u8; 32] {
        x25519_dalek::PublicKey::from(&self.x25519).to_bytes()
    }

    /// Slice 3 uses this to wrap a note key for a joining guest.
    pub fn agree(&self, peer: &[u8; 32]) -> [u8; 32] {
        self.x25519
            .diffie_hellman(&x25519_dalek::PublicKey::from(*peer))
            .to_bytes()
    }

    pub fn sign(&self, msg: &[u8]) -> [u8; SIG_LEN] {
        self.signing.sign(msg).to_bytes()
    }
}

/// An Ed25519 public key, used as the author of an update.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AuthorKey(pub [u8; PUBKEY_LEN]);

impl AuthorKey {
    pub fn encode(&self) -> String {
        URL_SAFE_NO_PAD.encode(self.0)
    }

    fn verifying_key(&self) -> Result<VerifyingKey> {
        VerifyingKey::from_bytes(&self.0)
            .map_err(|_| CoreError::Crypto("not a valid Ed25519 point"))
    }
}

impl std::fmt::Debug for AuthorKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "AuthorKey({})", &self.encode()[..8])
    }
}

// ---------------------------------------------------------------------------
// Note key
// ---------------------------------------------------------------------------

/// The symmetric key for one note. Everyone who can read the note has it;
/// revocation (Slice 3) means rotating it.
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct NoteKey(pub [u8; NOTE_KEY_LEN]);

impl NoteKey {
    pub fn random() -> Self {
        let mut k = [0u8; NOTE_KEY_LEN];
        random_bytes(&mut k);
        Self(k)
    }

    fn cipher(&self) -> XChaCha20Poly1305 {
        XChaCha20Poly1305::new((&self.0).into())
    }
}

impl std::fmt::Debug for NoteKey {
    /// Never print key material, not even in a panic message.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("NoteKey(redacted)")
    }
}

// ---------------------------------------------------------------------------
// Envelope
// ---------------------------------------------------------------------------

/// One encrypted, signed CRDT update. This is the only thing that ever reaches the relay.
#[derive(Clone, PartialEq, Eq)]
pub struct Envelope {
    pub note_id: NoteId,
    pub author: AuthorKey,
    /// Per-author monotonic counter. Bound into the AAD, so an update cannot be
    /// replayed under a different counter, and gives receivers a cheap dedup key.
    pub counter: u64,
    pub nonce: [u8; NONCE_LEN],
    pub signature: [u8; SIG_LEN],
    pub ciphertext: Vec<u8>,
}

impl std::fmt::Debug for Envelope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Envelope")
            .field("note_id", &self.note_id)
            .field("author", &self.author)
            .field("counter", &self.counter)
            .field("ciphertext_len", &self.ciphertext.len())
            .finish()
    }
}

/// What the cipher authenticates but does not hide: which note, which author,
/// which counter. The relay can read all three anyway — binding them here stops
/// an envelope being cut and pasted onto a different note or author.
fn aad(note_id: &NoteId, author: &AuthorKey, counter: u64) -> Vec<u8> {
    let mut a = Vec::with_capacity(1 + 16 + PUBKEY_LEN + 8);
    a.push(ENVELOPE_VERSION);
    a.extend_from_slice(note_id.as_bytes());
    a.extend_from_slice(&author.0);
    a.extend_from_slice(&counter.to_be_bytes());
    a
}

/// Signed bytes: the AAD plus the nonce and ciphertext. Covering the ciphertext
/// means a tampered blob fails at the signature, before the cipher is touched.
fn signed_bytes(
    note_id: &NoteId,
    author: &AuthorKey,
    counter: u64,
    nonce: &[u8; NONCE_LEN],
    ct: &[u8],
) -> Vec<u8> {
    let mut m = aad(note_id, author, counter);
    m.extend_from_slice(nonce);
    m.extend_from_slice(ct);
    m
}

impl Envelope {
    /// Encrypt `plaintext` for `note_id` and sign it as `identity`.
    pub fn seal(
        identity: &DeviceIdentity,
        note_id: NoteId,
        key: &NoteKey,
        counter: u64,
        plaintext: &[u8],
    ) -> Result<Self> {
        let author = identity.public_key();
        let mut nonce = [0u8; NONCE_LEN];
        random_bytes(&mut nonce);

        let ciphertext = key
            .cipher()
            .encrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: plaintext,
                    aad: &aad(&note_id, &author, counter),
                },
            )
            .map_err(|_| CoreError::Crypto("encryption failed"))?;

        let signature = identity.sign(&signed_bytes(
            &note_id,
            &author,
            counter,
            &nonce,
            &ciphertext,
        ));

        let envelope = Self {
            note_id,
            author,
            counter,
            nonce,
            signature,
            ciphertext,
        };
        if envelope.encoded_len() > MAX_ENVELOPE_BYTES {
            return Err(CoreError::TooLarge {
                limit: MAX_ENVELOPE_BYTES,
            });
        }
        Ok(envelope)
    }

    /// Verify the author's signature, **then** decrypt. Never reorder these two.
    pub fn open(&self, key: &NoteKey) -> Result<Vec<u8>> {
        self.verify()?;
        key.cipher()
            .decrypt(
                XNonce::from_slice(&self.nonce),
                Payload {
                    msg: &self.ciphertext,
                    aad: &aad(&self.note_id, &self.author, self.counter),
                },
            )
            .map_err(|_| CoreError::Crypto("decryption failed"))
    }

    /// Check authorship alone. Useful to a receiver that wants to drop an update
    /// from an unknown device without spending a decryption on it.
    pub fn verify(&self) -> Result<()> {
        let vk = self.author.verifying_key()?;
        let sig = Signature::from_bytes(&self.signature);
        vk.verify(
            &signed_bytes(
                &self.note_id,
                &self.author,
                self.counter,
                &self.nonce,
                &self.ciphertext,
            ),
            &sig,
        )
        .map_err(|_| CoreError::BadSignature)
    }

    pub fn encoded_len(&self) -> usize {
        ENVELOPE_HEADER_LEN + self.ciphertext.len()
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.encoded_len());
        out.push(ENVELOPE_VERSION);
        out.extend_from_slice(self.note_id.as_bytes());
        out.extend_from_slice(&self.author.0);
        out.extend_from_slice(&self.counter.to_be_bytes());
        out.extend_from_slice(&self.nonce);
        out.extend_from_slice(&self.signature);
        out.extend_from_slice(&self.ciphertext);
        out
    }

    /// Parse without any key. The relay uses exactly this and nothing more.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > MAX_ENVELOPE_BYTES {
            return Err(CoreError::TooLarge {
                limit: MAX_ENVELOPE_BYTES,
            });
        }
        if bytes.len() < ENVELOPE_HEADER_LEN + 16 {
            // 16 = the Poly1305 tag; an envelope shorter than that cannot be real.
            return Err(CoreError::MalformedEnvelope(
                "shorter than the header plus a tag",
            ));
        }
        if bytes[0] != ENVELOPE_VERSION {
            return Err(CoreError::MalformedEnvelope("unknown envelope version"));
        }
        let mut at = 1;
        let mut take = |n: usize| {
            let s = &bytes[at..at + n];
            at += n;
            s
        };
        let note_id = NoteId::from_bytes(take(16).try_into().unwrap());
        let author = AuthorKey(take(PUBKEY_LEN).try_into().unwrap());
        let counter = u64::from_be_bytes(take(8).try_into().unwrap());
        let nonce: [u8; NONCE_LEN] = take(NONCE_LEN).try_into().unwrap();
        let signature: [u8; SIG_LEN] = take(SIG_LEN).try_into().unwrap();
        let ciphertext = bytes[at..].to_vec();
        Ok(Self {
            note_id,
            author,
            counter,
            nonce,
            signature,
            ciphertext,
        })
    }
}

// ---------------------------------------------------------------------------
// Share blob
// ---------------------------------------------------------------------------

/// `version | note_id | note_key`, base64url. 66 characters, which QR-encodes at
/// low error correction.
///
/// This is deliberately the exact payload a Slice 3 invite link will carry after
/// its `#`, so that slice replaces the transport and not the format.
pub const SHARE_BLOB_VERSION: u8 = 1;

pub fn export_share_blob(note_id: &NoteId, key: &NoteKey) -> String {
    let mut raw = Vec::with_capacity(1 + 16 + NOTE_KEY_LEN);
    raw.push(SHARE_BLOB_VERSION);
    raw.extend_from_slice(note_id.as_bytes());
    raw.extend_from_slice(&key.0);
    let blob = URL_SAFE_NO_PAD.encode(&raw);
    raw.zeroize();
    blob
}

pub fn import_share_blob(blob: &str) -> Result<(NoteId, NoteKey)> {
    let mut raw = URL_SAFE_NO_PAD
        .decode(blob.trim())
        .map_err(|_| CoreError::MalformedShareBlob("not base64url"))?;
    if raw.len() != 1 + 16 + NOTE_KEY_LEN {
        raw.zeroize();
        return Err(CoreError::MalformedShareBlob("wrong length"));
    }
    if raw[0] != SHARE_BLOB_VERSION {
        raw.zeroize();
        return Err(CoreError::MalformedShareBlob("unknown version"));
    }
    let note_id = NoteId::from_bytes(raw[1..17].try_into().unwrap());
    let mut key = [0u8; NOTE_KEY_LEN];
    key.copy_from_slice(&raw[17..]);
    raw.zeroize();
    Ok((note_id, NoteKey(key)))
}
