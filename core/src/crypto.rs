//! Crypto for one note.
//!
//! Standard constructions only, no hand-rolled primitives:
//!   * XChaCha20-Poly1305 for every update and snapshot (`chacha20poly1305`)
//!   * Ed25519 for authorship (`ed25519-dalek`)
//!   * X25519 for the key agreement an invite's grant is sealed with (`x25519-dalek`, see [`crate::invite`])
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
use sha2::{Digest, Sha256};
use zeroize::{Zeroize, ZeroizeOnDrop};

/// Wire-format version. Bound into the AAD and the signature, so an attacker
/// cannot re-present a v1 envelope as some future version.
pub const ENVELOPE_VERSION: u8 = 2;
const LEGACY_ENVELOPE_VERSION: u8 = 1;

pub const NONCE_LEN: usize = 24;
pub const SIG_LEN: usize = 64;
pub const PUBKEY_LEN: usize = 32;
pub const NOTE_KEY_LEN: usize = 32;

/// Relay challenge size and signature protocol. The transcript is built here so
/// the client and relay use the same domain separator and framing.
pub const RELAY_AUTH_NONCE_LEN: usize = 32;
pub const RELAY_AUTH_DOMAIN: &[u8] = b"easynotes/relay-auth/v1";

pub fn relay_auth_transcript(nonce: &[u8; RELAY_AUTH_NONCE_LEN]) -> Vec<u8> {
    let mut transcript = Vec::with_capacity(RELAY_AUTH_DOMAIN.len() + 4 + nonce.len());
    transcript.extend_from_slice(RELAY_AUTH_DOMAIN);
    transcript.extend_from_slice(&(nonce.len() as u32).to_be_bytes());
    transcript.extend_from_slice(nonce);
    transcript
}

/// version | note_id | author | counter | nonce | signature
pub const KEY_ID_LEN: usize = 8;
pub const ENVELOPE_HEADER_LEN: usize =
    1 + 16 + PUBKEY_LEN + 8 + KEY_ID_LEN + 1 + NONCE_LEN + SIG_LEN;
const LEGACY_ENVELOPE_HEADER_LEN: usize = 1 + 16 + PUBKEY_LEN + 8 + NONCE_LEN + SIG_LEN;

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

    /// An identity that only ever signs: a note's owner key
    /// ([ADR 0018](../../docs/adr/0018-revocation.md) §1), rebuilt from the
    /// 32-byte seed the owner's account document keeps. Its X25519 half is
    /// derived so the type stays one type, and nothing agrees with it.
    pub fn from_signing_seed(seed: &[u8; 32]) -> Self {
        let mut x: [u8; 32] = Sha256::digest(seed).into();
        let identity = Self {
            signing: SigningKey::from_bytes(seed),
            x25519: x25519_dalek::StaticSecret::from(x),
        };
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

/// What the relay handshake needs from a device: the public key it authenticates,
/// and a signature over the nonce it hands back.
///
/// A trait rather than a `&DeviceIdentity` so that the sync engine can run against
/// a core behind a lock without a copy of the secret escaping to it. The seed stays
/// exactly where it was.
pub trait DeviceSigner: Send + Sync {
    fn device_key(&self) -> Result<AuthorKey>;

    /// Sign a relay challenge. Deliberately not a general-purpose signing method:
    /// the only thing a caller outside this module gets to sign is a fixed-size
    /// relay nonce, framed as a relay-authentication transcript.
    fn sign_challenge(&self, nonce: &[u8; RELAY_AUTH_NONCE_LEN]) -> Result<[u8; SIG_LEN]>;
}

impl DeviceSigner for DeviceIdentity {
    fn device_key(&self) -> Result<AuthorKey> {
        Ok(self.public_key())
    }

    fn sign_challenge(&self, nonce: &[u8; RELAY_AUTH_NONCE_LEN]) -> Result<[u8; SIG_LEN]> {
        Ok(self.sign(&relay_auth_transcript(nonce)))
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

    /// Verify a detached statement signed by this author key.
    pub fn verify(&self, message: &[u8], signature: &[u8; SIG_LEN]) -> bool {
        self.verifying_key().is_ok_and(|key| {
            key.verify(message, &Signature::from_bytes(signature))
                .is_ok()
        })
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

/// Domain tag for a note's row in the local search index. Bound into the AAD
/// exactly as [`LOCAL_DOMAIN`] is, and distinct from it and from the envelope's
/// own AAD so that the three uses of a key can never be confused for one
/// another.
const INDEX_DOMAIN: &[u8] = b"easynotes/index/v1";

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

    /// A non-secret identifier for the generation of key that sealed an
    /// envelope. Eight bytes are enough to make accidental collisions
    /// negligible; this is routing metadata, never key material.
    pub fn id(&self) -> [u8; KEY_ID_LEN] {
        let digest = Sha256::digest(self.0);
        digest[..KEY_ID_LEN]
            .try_into()
            .expect("the slice has eight bytes")
    }

    /// Seal this note's row in the local search index, bound to the note it
    /// summarises.
    ///
    /// The note's own key rather than the master key, for one reason and one
    /// consequence. The reason: the index entry is a copy of the note's title
    /// and text, so it should be readable by exactly what can read the note and
    /// by nothing else — a device that loses a note's key loses the note, and an
    /// index that stayed legible would be a way back in. The consequence: an
    /// entry travels with the note through [`crate::store::copy_store`] without
    /// being re-sealed, because both sides hold the same key.
    ///
    /// [`INDEX_DOMAIN`] is what stops the two uses of that key from being
    /// confused. An index entry and an [`Envelope`] payload are sealed under the
    /// same bytes, and the domain tag in the AAD means neither will ever open as
    /// the other: an entry cannot be replayed to a peer as an update, and an
    /// update cannot be planted in the index.
    pub fn seal_index(&self, note: &NoteId, plaintext: &[u8]) -> Result<Vec<u8>> {
        seal_blob(
            &self.cipher(),
            &local_aad(INDEX_DOMAIN, &[note.as_bytes()]),
            plaintext,
        )
    }

    pub fn open_index(&self, note: &NoteId, sealed: &[u8]) -> Result<Vec<u8>> {
        open_blob(
            &self.cipher(),
            &local_aad(INDEX_DOMAIN, &[note.as_bytes()]),
            sealed,
        )
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
    version: u8,
    pub note_id: NoteId,
    pub author: AuthorKey,
    /// Per-author monotonic counter. Bound into the AAD, so an update cannot be
    /// replayed under a different counter, and gives receivers a cheap dedup key.
    pub counter: u64,
    /// Which note-key generation encrypted this envelope. Absent only on the
    /// legacy v1 format, whose notes have never been rotated.
    pub key_id: Option<[u8; KEY_ID_LEN]>,
    pub kind: EnvelopeKind,
    pub nonce: [u8; NONCE_LEN],
    pub signature: [u8; SIG_LEN],
    pub ciphertext: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u8)]
pub enum EnvelopeKind {
    #[default]
    Update = 0,
    /// A whole document. Only ever a local row now: a note's relay log carries
    /// updates and access packages, and a snapshot there is refused.
    Snapshot = 1,
    /// An access package, signed by the note's owner key
    /// ([ADR 0023](../../docs/adr/0023-access-packages.md) §4). Its ciphertext
    /// field starts with a signed cleartext prefix; see
    /// [`Envelope::seal_with_clear`].
    Access = 3,
}

impl EnvelopeKind {
    fn decode(raw: u8) -> Result<Self> {
        match raw {
            0 => Ok(Self::Update),
            1 => Ok(Self::Snapshot),
            3 => Ok(Self::Access),
            _ => Err(CoreError::MalformedEnvelope("unknown envelope kind")),
        }
    }
}

impl std::fmt::Debug for Envelope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Envelope")
            .field("note_id", &self.note_id)
            .field("author", &self.author)
            .field("counter", &self.counter)
            .field("key_id", &self.key_id)
            .field("kind", &self.kind)
            .field("ciphertext_len", &self.ciphertext.len())
            .finish()
    }
}

/// What the cipher authenticates but does not hide: which note, which author,
/// which counter. The relay can read all three anyway — binding them here stops
/// an envelope being cut and pasted onto a different note or author.
fn aad(
    version: u8,
    note_id: &NoteId,
    author: &AuthorKey,
    counter: u64,
    key_id: Option<&[u8; KEY_ID_LEN]>,
    kind: EnvelopeKind,
) -> Vec<u8> {
    let mut a = Vec::with_capacity(1 + 16 + PUBKEY_LEN + 8 + KEY_ID_LEN + 1);
    a.push(version);
    a.extend_from_slice(note_id.as_bytes());
    a.extend_from_slice(&author.0);
    a.extend_from_slice(&counter.to_be_bytes());
    if let Some(key_id) = key_id {
        a.extend_from_slice(key_id);
        a.push(kind as u8);
    }
    a
}

impl Envelope {
    /// The authenticated header plus nonce and ciphertext. The signature field
    /// itself is deliberately absent.
    fn signed_bytes(&self) -> Vec<u8> {
        let mut bytes = aad(
            self.version,
            &self.note_id,
            &self.author,
            self.counter,
            self.key_id.as_ref(),
            self.kind,
        );
        bytes.extend_from_slice(&self.nonce);
        bytes.extend_from_slice(&self.ciphertext);
        bytes
    }

    /// Encrypt `plaintext` for `note_id` and sign it as `identity`.
    pub fn seal(
        identity: &DeviceIdentity,
        note_id: NoteId,
        key: &NoteKey,
        counter: u64,
        plaintext: &[u8],
    ) -> Result<Self> {
        Self::seal_kind(
            identity,
            note_id,
            key,
            counter,
            EnvelopeKind::Update,
            plaintext,
        )
    }

    pub fn seal_kind(
        identity: &DeviceIdentity,
        note_id: NoteId,
        key: &NoteKey,
        counter: u64,
        kind: EnvelopeKind,
        plaintext: &[u8],
    ) -> Result<Self> {
        let author = identity.public_key();
        let key_id = key.id();
        let mut nonce = [0u8; NONCE_LEN];
        random_bytes(&mut nonce);

        let ciphertext = key
            .cipher()
            .encrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: plaintext,
                    aad: &aad(
                        ENVELOPE_VERSION,
                        &note_id,
                        &author,
                        counter,
                        Some(&key_id),
                        kind,
                    ),
                },
            )
            .map_err(|_| CoreError::Crypto("encryption failed"))?;

        let mut envelope = Self {
            version: ENVELOPE_VERSION,
            note_id,
            author,
            counter,
            key_id: Some(key_id),
            kind,
            nonce,
            signature: [0u8; SIG_LEN],
            ciphertext,
        };
        envelope.signature = identity.sign(&envelope.signed_bytes());
        if envelope.encoded_len() > MAX_ENVELOPE_BYTES {
            return Err(CoreError::TooLarge {
                limit: MAX_ENVELOPE_BYTES,
            });
        }
        Ok(envelope)
    }

    /// Seal an envelope whose ciphertext field begins with `clear`: bytes the
    /// signature covers and the cipher does not hide, followed by `plaintext`
    /// encrypted with `clear` bound into its AAD.
    ///
    /// What an access package is made of: its revision, parent and grant slots
    /// have to be readable by a device that does not hold the key the body is
    /// sealed under, because working out which package is current must not need
    /// one.
    pub fn seal_with_clear(
        identity: &DeviceIdentity,
        note_id: NoteId,
        key: &NoteKey,
        counter: u64,
        kind: EnvelopeKind,
        clear: &[u8],
        plaintext: &[u8],
    ) -> Result<Self> {
        let author = identity.public_key();
        let key_id = key.id();
        let mut nonce = [0u8; NONCE_LEN];
        random_bytes(&mut nonce);
        let mut body_aad = aad(
            ENVELOPE_VERSION,
            &note_id,
            &author,
            counter,
            Some(&key_id),
            kind,
        );
        body_aad.extend_from_slice(clear);
        let sealed = key
            .cipher()
            .encrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: plaintext,
                    aad: &body_aad,
                },
            )
            .map_err(|_| CoreError::Crypto("encryption failed"))?;
        let mut ciphertext = Vec::with_capacity(clear.len() + sealed.len());
        ciphertext.extend_from_slice(clear);
        ciphertext.extend_from_slice(&sealed);
        let mut envelope = Self {
            version: ENVELOPE_VERSION,
            note_id,
            author,
            counter,
            key_id: Some(key_id),
            kind,
            nonce,
            signature: [0u8; SIG_LEN],
            ciphertext,
        };
        envelope.signature = identity.sign(&envelope.signed_bytes());
        if envelope.encoded_len() > MAX_ENVELOPE_BYTES {
            return Err(CoreError::TooLarge {
                limit: MAX_ENVELOPE_BYTES,
            });
        }
        Ok(envelope)
    }

    /// The first `clear_len` bytes of the ciphertext field, which only
    /// [`Envelope::seal_with_clear`] puts there. Not verified: call
    /// [`Envelope::verify`] before believing them.
    pub fn clear(&self, clear_len: usize) -> Result<&[u8]> {
        self.ciphertext
            .get(..clear_len)
            .ok_or(CoreError::MalformedEnvelope("shorter than its cleartext"))
    }

    /// Verify, then decrypt what follows a `clear_len`-byte cleartext.
    pub fn open_with_clear(&self, key: &NoteKey, clear_len: usize) -> Result<Vec<u8>> {
        self.verify()?;
        let clear = self.clear(clear_len)?;
        let mut body_aad = aad(
            self.version,
            &self.note_id,
            &self.author,
            self.counter,
            self.key_id.as_ref(),
            self.kind,
        );
        body_aad.extend_from_slice(clear);
        key.cipher()
            .decrypt(
                XNonce::from_slice(&self.nonce),
                Payload {
                    msg: &self.ciphertext[clear_len..],
                    aad: &body_aad,
                },
            )
            .map_err(|_| CoreError::Crypto("decryption failed"))
    }

    /// Verify the author's signature, **then** decrypt. Never reorder these two.
    pub fn open(&self, key: &NoteKey) -> Result<Vec<u8>> {
        self.verify()?;
        key.cipher()
            .decrypt(
                XNonce::from_slice(&self.nonce),
                Payload {
                    msg: &self.ciphertext,
                    aad: &aad(
                        self.version,
                        &self.note_id,
                        &self.author,
                        self.counter,
                        self.key_id.as_ref(),
                        self.kind,
                    ),
                },
            )
            .map_err(|_| CoreError::Crypto("decryption failed"))
    }

    /// Check authorship alone. Useful to a receiver that wants to drop an update
    /// from an unknown device without spending a decryption on it.
    pub fn verify(&self) -> Result<()> {
        let vk = self.author.verifying_key()?;
        let sig = Signature::from_bytes(&self.signature);
        vk.verify(&self.signed_bytes(), &sig)
            .map_err(|_| CoreError::BadSignature)
    }

    pub fn encoded_len(&self) -> usize {
        if self.version == LEGACY_ENVELOPE_VERSION {
            LEGACY_ENVELOPE_HEADER_LEN + self.ciphertext.len()
        } else {
            ENVELOPE_HEADER_LEN + self.ciphertext.len()
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.encoded_len());
        out.push(self.version);
        out.extend_from_slice(self.note_id.as_bytes());
        out.extend_from_slice(&self.author.0);
        out.extend_from_slice(&self.counter.to_be_bytes());
        if self.version == ENVELOPE_VERSION {
            out.extend_from_slice(self.key_id.as_ref().expect("v2 has a key id"));
            out.push(self.kind as u8);
        }
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
        let version = bytes.first().copied().ok_or(CoreError::MalformedEnvelope(
            "shorter than the header plus a tag",
        ))?;
        let header_len = match version {
            LEGACY_ENVELOPE_VERSION => LEGACY_ENVELOPE_HEADER_LEN,
            ENVELOPE_VERSION => ENVELOPE_HEADER_LEN,
            _ => return Err(CoreError::MalformedEnvelope("unknown envelope version")),
        };
        if bytes.len() < header_len + 16 {
            // 16 = the Poly1305 tag; an envelope shorter than that cannot be real.
            return Err(CoreError::MalformedEnvelope(
                "shorter than the header plus a tag",
            ));
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
        let (key_id, kind) = if version == ENVELOPE_VERSION {
            (
                Some(take(KEY_ID_LEN).try_into().unwrap()),
                EnvelopeKind::decode(take(1)[0])?,
            )
        } else {
            (None, EnvelopeKind::Update)
        };
        let nonce: [u8; NONCE_LEN] = take(NONCE_LEN).try_into().unwrap();
        let signature: [u8; SIG_LEN] = take(SIG_LEN).try_into().unwrap();
        let ciphertext = bytes[at..].to_vec();
        Ok(Self {
            version,
            note_id,
            author,
            counter,
            key_id,
            kind,
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

// ---------------------------------------------------------------------------
// Master key
// ---------------------------------------------------------------------------

/// Domain tag for everything sealed under the master key. Bound into the AAD, so
/// a local blob and a relay envelope can never be mistaken for one another even
/// when the two keys happen to be the same bytes.
const LOCAL_DOMAIN: &[u8] = b"easynotes/local/v1";

pub const MASTER_KEY_LEN: usize = 32;

/// Format of a sealed local blob: `version | nonce | ciphertext`.
pub const LOCAL_BLOB_VERSION: u8 = 1;
pub const LOCAL_BLOB_HEADER_LEN: usize = 1 + NONCE_LEN;

/// The key that encrypts what this device keeps for itself.
///
/// It is not a note key and it never travels: it wraps the device seed and every
/// note key, and it seals the local-only rows — the sync cursor today, the search
/// index and the account document later — that are not already envelopes. One
/// device, one master key; losing it costs this device's local copy and nothing
/// that is on the relay.
///
/// In the shipping app it lives in the OS keychain. Until the bridge can ask Dart
/// for it, the native build keeps it in a file beside the database, which is the
/// same stand-in Slice 1 used for the device seed and no worse than what it
/// replaces.
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct MasterKey(pub [u8; MASTER_KEY_LEN]);

impl MasterKey {
    pub fn random() -> Self {
        let mut k = [0u8; MASTER_KEY_LEN];
        random_bytes(&mut k);
        Self(k)
    }

    pub fn from_slice(raw: &[u8]) -> Result<Self> {
        let k: [u8; MASTER_KEY_LEN] = raw
            .try_into()
            .map_err(|_| CoreError::Crypto("a master key is 32 bytes"))?;
        Ok(Self(k))
    }

    fn cipher(&self) -> XChaCha20Poly1305 {
        XChaCha20Poly1305::new((&self.0).into())
    }

    /// Encrypt a local blob. `context` says what the blob is and where it lives —
    /// for a sync cursor, the word `cursor` and the note it belongs to.
    ///
    /// The context is authenticated, not hidden, and that is the point: a row
    /// cannot be lifted out of the database and put back somewhere else. A cursor
    /// moved to another note, or a note key presented as the device seed, fails to
    /// open rather than being believed.
    pub fn seal(&self, context: &[&[u8]], plaintext: &[u8]) -> Result<Vec<u8>> {
        seal_blob(&self.cipher(), &local_aad(LOCAL_DOMAIN, context), plaintext)
    }

    /// Decrypt a local blob sealed with the same key and the same context.
    pub fn open(&self, context: &[&[u8]], sealed: &[u8]) -> Result<Vec<u8>> {
        open_blob(&self.cipher(), &local_aad(LOCAL_DOMAIN, context), sealed)
    }
}

impl std::fmt::Debug for MasterKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("MasterKey(redacted)")
    }
}

/// The domain tag, then each context part with its length in front. The lengths
/// are what stop `["note", "key"]` and `["notek", "ey"]` from producing the same
/// AAD.
pub(crate) fn local_aad(domain: &[u8], context: &[&[u8]]) -> Vec<u8> {
    let mut aad = Vec::with_capacity(domain.len() + 8 * context.len());
    aad.extend_from_slice(domain);
    for part in context {
        aad.extend_from_slice(&(part.len() as u32).to_be_bytes());
        aad.extend_from_slice(part);
    }
    aad
}

/// `version | nonce | ciphertext`, for everything this device seals for itself.
///
/// One body for both keys that do it, so the format cannot drift between them
/// and a reader of either only has one thing to check. What differs is the AAD,
/// and that is the whole of what keeps the uses apart.
pub(crate) fn seal_blob(
    cipher: &XChaCha20Poly1305,
    aad: &[u8],
    plaintext: &[u8],
) -> Result<Vec<u8>> {
    let mut nonce = [0u8; NONCE_LEN];
    random_bytes(&mut nonce);
    let ciphertext = cipher
        .encrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .map_err(|_| CoreError::Crypto("encryption failed"))?;

    let mut out = Vec::with_capacity(LOCAL_BLOB_HEADER_LEN + ciphertext.len());
    out.push(LOCAL_BLOB_VERSION);
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

pub(crate) fn open_blob(cipher: &XChaCha20Poly1305, aad: &[u8], sealed: &[u8]) -> Result<Vec<u8>> {
    if sealed.len() < LOCAL_BLOB_HEADER_LEN + 16 {
        return Err(CoreError::MalformedEnvelope(
            "local blob shorter than the header plus a tag",
        ));
    }
    if sealed[0] != LOCAL_BLOB_VERSION {
        return Err(CoreError::MalformedEnvelope("unknown local blob version"));
    }
    let nonce: [u8; NONCE_LEN] = sealed[1..LOCAL_BLOB_HEADER_LEN].try_into().unwrap();
    cipher
        .decrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: &sealed[LOCAL_BLOB_HEADER_LEN..],
                aad,
            },
        )
        .map_err(|_| CoreError::Crypto("decryption failed"))
}
