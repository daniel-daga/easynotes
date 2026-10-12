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
use crate::ids::{MediaObjectId, NoteId};
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

/// The most objects a live list may name. A note's own limits keep it to
/// forty or so (twenty attachments, each an original and a thumbnail); only
/// hostile descriptors reach this, and a note that names more sends no list.
pub const MAX_LIVE_OBJECTS: usize = 4096;

/// What a compaction signs: the domain, the topic, how far the base reaches, a
/// hash of the base, and the live list
/// ([ADR 0027](../../docs/adr/0027-attachments.md) §5): a byte saying whether
/// there is one, and if so a hash of its IDs sorted and without repeats, so
/// that the order a device listed them in does not matter. Fixed-length, so no
/// two different requests share one.
pub fn transcript(
    topic: NoteId,
    covers: i64,
    base: &[u8],
    media: Option<&[MediaObjectId]>,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(COMPACT_DOMAIN.len() + 16 + 8 + 32 + 1 + 32);
    out.extend_from_slice(COMPACT_DOMAIN);
    out.extend_from_slice(topic.as_bytes());
    out.extend_from_slice(&covers.to_be_bytes());
    out.extend_from_slice(&Sha256::digest(base));
    match media {
        None => out.push(0),
        Some(media) => {
            let mut sorted = media.to_vec();
            sorted.sort();
            sorted.dedup();
            let mut hash = Sha256::new();
            for object in &sorted {
                hash.update(object.as_bytes());
            }
            out.push(1);
            out.extend_from_slice(&hash.finalize());
        }
    }
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
    /// Every attachment object the base still names, and this device's own
    /// imports not in it yet; the relay lets the rest of the topic's go
    /// (ADR 0027 §5). `None` vouches for nothing, and the relay deletes
    /// nothing: a document with a descriptor this build cannot even find the
    /// objects of, and the account document, which has none.
    pub media: Option<Vec<MediaObjectId>>,
    /// The compaction key's public half.
    pub key: AuthorKey,
    pub signature: [u8; SIG_LEN],
}

impl Request {
    /// Sign a base, and the live list if there is one, with the topic's
    /// compaction key.
    pub fn sign(
        topic: NoteId,
        key: &NoteKey,
        covers: i64,
        base: Vec<u8>,
        media: Option<Vec<MediaObjectId>>,
    ) -> Self {
        let signer = identity(topic, key);
        let signature = signer.sign(&transcript(topic, covers, &base, media.as_deref()));
        Self {
            covers,
            base,
            media,
            key: signer.public_key(),
            signature,
        }
    }

    /// Whether the signature is the named key's over this base and list.
    pub fn verifies(&self, topic: NoteId) -> bool {
        self.key.verify(
            &transcript(topic, self.covers, &self.base, self.media.as_deref()),
            &self.signature,
        )
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
        let request = Request::sign(note, &key, 12, b"base".to_vec(), None);
        assert!(request.verifies(note));
        assert!(!request.verifies(NoteId::from_bytes([8u8; 16])));
        let mut moved = request.clone();
        moved.covers = 13;
        assert!(!moved.verifies(note));
        let mut swapped = request.clone();
        swapped.base = b"other".to_vec();
        assert!(!swapped.verifies(note));
        // A list added to a request signed without one is a list nobody
        // vouched for.
        let mut listed = request.clone();
        listed.media = Some(Vec::new());
        assert!(!listed.verifies(note));
    }

    #[test]
    fn the_live_list_is_signed_whatever_order_it_comes_in() {
        let note = NoteId::from_bytes([7u8; 16]);
        let key = NoteKey([1u8; 32]);
        let (a, b, c) = (
            MediaObjectId::from_bytes([1u8; 16]),
            MediaObjectId::from_bytes([2u8; 16]),
            MediaObjectId::from_bytes([3u8; 16]),
        );
        let request = Request::sign(note, &key, 12, b"base".to_vec(), Some(vec![a, b]));
        assert!(request.verifies(note));
        // The same objects in another order, or one twice, are the same list.
        let mut reordered = request.clone();
        reordered.media = Some(vec![b, a, b]);
        assert!(reordered.verifies(note));
        // Another object, one fewer, or an empty list is not.
        for other in [vec![a, b, c], vec![a], Vec::new()] {
            let mut changed = request.clone();
            changed.media = Some(other);
            assert!(!changed.verifies(note));
        }
        let mut dropped = request.clone();
        dropped.media = None;
        assert!(!dropped.verifies(note));
    }

    #[test]
    fn the_transcript_is_pinned() {
        // The relay checks this transcript as written here, so a change to it
        // is a protocol change, not a refactor.
        let note = NoteId::from_bytes([0u8; 16]);
        let without = transcript(note, 1, b"", None);
        let with = transcript(note, 1, b"", Some(&[MediaObjectId::from_bytes([0u8; 16])]));
        assert_eq!(without.len(), COMPACT_DOMAIN.len() + 16 + 8 + 32 + 1);
        assert_eq!(with.len(), without.len() + 32);
        assert_eq!(&without[without.len() - 1..], &[0]);
        assert_eq!(
            &with[without.len() - 1..],
            [&[1u8][..], &Sha256::digest([0u8; 16])[..]].concat()
        );
    }
}
