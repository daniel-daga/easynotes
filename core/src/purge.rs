//! Asking the relay to forget a topic for good.
//!
//! [ADR 0026](../../docs/decisions.md#adr-0026). A purge deletes
//! every envelope the relay holds for one topic and leaves a tombstone, so it is
//! the most destructive request the relay takes. It is signed, by the key the
//! relay already trusts to say what happens to that topic: the owner key the
//! first access package pinned, or for a topic that was never shared the
//! compaction key ([`crate::compaction`]).
//!
//! The transcript is the domain and the topic, nothing else. A purge is
//! idempotent and permanent, so a replayed one does nothing a first one did not,
//! and there is no time or nonce to get wrong.

use crate::crypto::{AuthorKey, DeviceIdentity, SIG_LEN};
use crate::ids::NoteId;

/// The first bytes of every purge transcript. Not the compaction domain, so a
/// signature made for one cannot be presented as the other.
pub const PURGE_DOMAIN: &[u8] = b"easynotes/relay-purge/v1";

/// What a purge signs.
pub fn transcript(topic: NoteId) -> Vec<u8> {
    let mut out = Vec::with_capacity(PURGE_DOMAIN.len() + 16);
    out.extend_from_slice(PURGE_DOMAIN);
    out.extend_from_slice(topic.as_bytes());
    out
}

/// A purge, ready to send.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub key: AuthorKey,
    pub signature: [u8; SIG_LEN],
}

impl Request {
    pub fn sign(topic: NoteId, signer: &DeviceIdentity) -> Self {
        Self {
            key: signer.public_key(),
            signature: signer.sign(&transcript(topic)),
        }
    }

    /// Whether the signature is the named key's over this topic.
    pub fn verifies(&self, topic: NoteId) -> bool {
        self.key.verify(&transcript(topic), &self.signature)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_purge_verifies_only_for_its_topic() {
        let signer = DeviceIdentity::from_signing_seed(&[3u8; 32]);
        let note = NoteId::from_bytes([7u8; 16]);
        let request = Request::sign(note, &signer);
        assert!(request.verifies(note));
        assert!(!request.verifies(NoteId::from_bytes([8u8; 16])));
        let mut other = request.clone();
        other.key = DeviceIdentity::from_signing_seed(&[4u8; 32]).public_key();
        assert!(!other.verifies(note));
    }

    #[test]
    fn a_compaction_signature_is_not_a_purge() {
        let note = NoteId::from_bytes([7u8; 16]);
        let key = crate::crypto::NoteKey([1u8; 32]);
        let compaction = crate::compaction::Request::sign(note, &key, 0, Vec::new(), None);
        let forged = Request {
            key: compaction.key,
            signature: compaction.signature,
        };
        assert!(!forged.verifies(note));
    }

    #[test]
    fn the_transcript_is_pinned() {
        // A change here makes every purge an old client sends fail to verify.
        let note = NoteId::from_bytes([0u8; 16]);
        let mut expected = b"easynotes/relay-purge/v1".to_vec();
        expected.extend_from_slice(&[0u8; 16]);
        assert_eq!(transcript(note), expected);
    }
}
