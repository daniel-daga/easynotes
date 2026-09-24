//! Compacting a note's log on the relay: the key that may do it, and what it
//! signs.
//!
//! The relay keeps every envelope published to a topic until a member that is
//! caught up hands it a **base**: one ordinary sealed update holding the whole
//! document, and a promise that it covers everything up to relay sequence
//! `covers`. The relay then drops what the base makes redundant. That is
//! destructive, so the relay must be able to tell a member from a stranger who
//! learned the topic ID, without learning who either of them is
//! ([ADR 0024](../../docs/adr/0024-relay-storage-quota.md)).
//!
//! The answer is a key every member holds and nobody else does: an Ed25519 key
//! derived from the topic's own key. A member can derive it; the relay sees only
//! its public half, which is random per note and per key, so it links nothing
//! the topic ID does not already link. It is a separate key from the device key
//! and from anything that signs an envelope, so a signature made for the relay
//! can never be taken for either.
//!
//! Which public half the relay accepts comes from the note's current access
//! package, which carries it in its signed prefix, or — for a note that has
//! never been shared, and for the account document — from the first
//! compaction the relay sees.

use crate::crypto::{AuthorKey, DeviceIdentity, NoteKey, SIG_LEN};
use crate::ids::NoteId;
use hkdf::Hkdf;
use sha2::{Digest, Sha256};
use zeroize::Zeroize;

/// The HKDF label the compaction key is expanded under, and the first bytes of
/// every transcript it signs.
pub const COMPACT_DOMAIN: &[u8] = b"easynotes/relay-compact/v1";

/// The compaction key for a topic under one of its keys.
///
/// HKDF-SHA256 with the topic ID as salt and the topic key as input. A key
/// rotation therefore changes it, which is how a removed member loses the right
/// to compact along with the right to read.
pub fn identity(topic: NoteId, key: &NoteKey) -> DeviceIdentity {
    let mut seed = [0u8; 32];
    Hkdf::<Sha256>::new(Some(topic.as_bytes()), &key.0)
        .expand(COMPACT_DOMAIN, &mut seed)
        .expect("HKDF-SHA256 can always produce 32 bytes");
    let identity = DeviceIdentity::from_signing_seed(&seed);
    seed.zeroize();
    identity
}

/// The public half of [`identity`]: what an access package records and the
/// relay checks against.
pub fn public_key(topic: NoteId, key: &NoteKey) -> AuthorKey {
    identity(topic, key).public_key()
}

/// What a compaction signs: the domain, the topic, how far the base reaches,
/// and a hash of the base. Fixed-length, so no two different requests share one.
pub fn transcript(topic: NoteId, covers: i64, base: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(COMPACT_DOMAIN.len() + 16 + 8 + 32);
    out.extend_from_slice(COMPACT_DOMAIN);
    out.extend_from_slice(topic.as_bytes());
    out.extend_from_slice(&covers.to_be_bytes());
    out.extend_from_slice(&Sha256::digest(base));
    out
}

/// A base, ready to hand to the relay.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    /// How far the base reaches: the relay sequence number this device had
    /// applied everything up to when it made it.
    pub covers: i64,
    /// The whole document, sealed as an ordinary update by this device.
    pub base: Vec<u8>,
    /// The compaction key's public half.
    pub key: AuthorKey,
    pub signature: [u8; SIG_LEN],
}

impl Request {
    /// Sign a base with the topic's compaction key.
    pub fn sign(topic: NoteId, key: &NoteKey, covers: i64, base: Vec<u8>) -> Self {
        let signer = identity(topic, key);
        let signature = signer.sign(&transcript(topic, covers, &base));
        Self {
            covers,
            base,
            key: signer.public_key(),
            signature,
        }
    }

    /// Whether the signature is the named key's over this base.
    pub fn verifies(&self, topic: NoteId) -> bool {
        self.key
            .verify(&transcript(topic, self.covers, &self.base), &self.signature)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_key_follows_the_topic_key_and_nothing_else() {
        let note = NoteId::from_bytes([7u8; 16]);
        let key = NoteKey([1u8; 32]);
        assert_eq!(public_key(note, &key), public_key(note, &key));
        assert_ne!(
            public_key(note, &key),
            public_key(note, &NoteKey([2u8; 32]))
        );
        assert_ne!(
            public_key(note, &key),
            public_key(NoteId::from_bytes([8u8; 16]), &key)
        );
    }

    #[test]
    fn the_key_is_pinned() {
        // A change here strands every registration a relay holds. The value
        // was checked against an independent HKDF-SHA256 and OpenSSL's
        // Ed25519, not only against this code.
        let key = public_key(NoteId::from_bytes([0u8; 16]), &NoteKey([0u8; 32]));
        assert_eq!(key.encode(), "vnF8bIiIYzKrQ_d12cjyfhCgYTLRcGZ4yB3LxOx7YMo");
    }

    #[test]
    fn a_request_verifies_only_as_it_was_signed() {
        let note = NoteId::from_bytes([7u8; 16]);
        let key = NoteKey([1u8; 32]);
        let request = Request::sign(note, &key, 12, b"base".to_vec());
        assert!(request.verifies(note));
        assert!(!request.verifies(NoteId::from_bytes([8u8; 16])));
        let mut moved = request.clone();
        moved.covers = 13;
        assert!(!moved.verifies(note));
        let mut swapped = request.clone();
        swapped.base = b"other".to_vec();
        assert!(!swapped.verifies(note));
    }
}
