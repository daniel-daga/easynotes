//! The recovery phrase, and the account root it derives.
//!
//! This is the whole of "one person" in EasyNotes. There is no server-side
//! account, no password and no email address: a person *is* twelve words, and a
//! device belongs to them if it can derive the same account root from those
//! words. See [ADR 0013](../../docs/adr/0013-recovery-phrase.md) for why, and
//! [ADR 0014](../../docs/adr/0014-what-the-relay-learns.md) for what this
//! deliberately does not tell the relay.
//!
//! ## What the phrase derives, and what it does not
//!
//! ```text
//! phrase ──BIP-39──> seed ──HKDF──> account root
//!                                      ├──> the account document's key
//!                                      └──> the account document's ID
//! ```
//!
//! And that is all. In particular it does **not** derive note keys. Those stay
//! random, and a second device gets them by reading them out of the account
//! document once it can open it — which is the one design point that makes item
//! 4's revocation possible later. A note key that could be re-derived from a
//! phrase could never be rotated away from anybody who had held the phrase; a
//! note key that is merely *stored* under the phrase's authority is replaced by
//! writing a new value into one register.
//!
//! Nor does it derive the device seed. Every device mints its own, so two
//! devices of one person are two unrelated Ed25519 keys to the relay,
//! indistinguishable from two people — which is the privacy claim ADR 0014
//! exists to hold onto.
//!
//! ## Why the document needs an ID of its own
//!
//! Locally the account document lives under a reserved ID of sixteen zero bytes
//! ([`crate::note::ACCOUNT_DOC`], [ADR
//! 0008](../../docs/adr/0008-account-document.md)). That is a fine local
//! address and an impossible wire address: the relay keeps one log per
//! note ID, so an account document that synced under zeros would
//! put every user of every relay on one topic — reading each other's placements
//! if they shared a key, and colliding on sequence numbers even if they did
//! not. So the ID a *relay* sees is derived from the root, which makes it
//! sixteen bytes indistinguishable from any note ID and unlinkable to anything
//! else this person has.

use crate::crypto::{NoteKey, MASTER_KEY_LEN, NOTE_KEY_LEN};
use crate::error::{CoreError, Result};
use crate::ids::NoteId;
use hkdf::Hkdf;
use sha2::Sha256;
use zeroize::{Zeroize, ZeroizeOnDrop};

/// How many words a phrase is. Twelve is 128 bits of entropy plus a four-bit
/// checksum, and everything downstream of it is 256-bit symmetric anyway, so
/// twenty-four words would buy nothing a person could measure and cost them
/// twelve more words to copy off a screen and onto paper.
pub const PHRASE_WORDS: usize = 12;

/// Bytes of entropy behind [`PHRASE_WORDS`].
const PHRASE_ENTROPY_BYTES: usize = 16;

/// The HKDF salt. Fixed, public, and unique to this application: it is what
/// stops a phrase that somebody reused from another BIP-39 wallet from
/// producing key material either side could recognise.
const SALT: &[u8] = b"easynotes/account/v1";

const INFO_ROOT: &[u8] = b"account-root/v1";
const INFO_DOC_KEY: &[u8] = b"account-doc-key/v1";
const INFO_DOC_ID: &[u8] = b"account-doc-id/v1";

/// A recovery phrase: twelve English words, with BIP-39's checksum in them.
///
/// The checksum is the reason to take a standard here rather than invent a
/// wordlist. A person copying twelve words off a screen onto paper and back
/// into another device will sometimes swap two of them or write `flight` for
/// `flit`, and without a checksum that produces a *valid* phrase deriving a
/// perfectly good account root that simply has nothing in it. The failure mode
/// of a silent wrong answer here is "all my notes are gone", which is the worst
/// message this app could ever give someone who did everything right.
pub struct RecoveryPhrase(bip39::Mnemonic);

impl RecoveryPhrase {
    /// Mint a new phrase from the platform RNG.
    ///
    /// Entropy comes from [`crate::crypto::random_bytes`] rather than from
    /// `bip39`'s own `rand` feature, so this crate has exactly one source of
    /// randomness and one place where that claim can be checked.
    pub fn generate() -> Self {
        let mut entropy = [0u8; PHRASE_ENTROPY_BYTES];
        crate::crypto::random_bytes(&mut entropy);
        let mnemonic = bip39::Mnemonic::from_entropy(&entropy)
            .expect("16 bytes of entropy is always a valid 12-word mnemonic");
        entropy.zeroize();
        Self(mnemonic)
    }

    /// Parse a phrase a person typed back in.
    ///
    /// Forgiving about everything that is not the words themselves — case,
    /// runs of whitespace, leading and trailing space — because a phrase gets
    /// read aloud, pasted out of a password manager, and typed by someone whose
    /// keyboard capitalises the first word for them. It is not forgiving about
    /// the words or their order, because that is what the checksum is for.
    pub fn parse(input: &str) -> Result<Self> {
        let normalised = input.split_whitespace().collect::<Vec<_>>().join(" ");
        let normalised = normalised.to_lowercase();
        let count = normalised.split_whitespace().count();
        if count != PHRASE_WORDS {
            return Err(CoreError::MalformedPhrase(if count < PHRASE_WORDS {
                "a recovery phrase is twelve words; this is short"
            } else {
                "a recovery phrase is twelve words; this is long"
            }));
        }
        bip39::Mnemonic::parse_in_normalized(bip39::Language::English, &normalised)
            .map(Self)
            .map_err(|e| match e {
                bip39::Error::UnknownWord(_) => {
                    CoreError::MalformedPhrase("that is not a word from the recovery wordlist")
                }
                bip39::Error::InvalidChecksum => CoreError::MalformedPhrase(
                    "those are all real words, but not in that order — check for a swap or a typo",
                ),
                _ => CoreError::MalformedPhrase("not a valid recovery phrase"),
            })
    }

    /// The twelve words, space-separated and lowercase.
    pub fn words(&self) -> String {
        self.0.to_string()
    }

    /// The words one at a time, for a screen that numbers them.
    pub fn word_list(&self) -> Vec<String> {
        self.0.words().map(|w| w.to_string()).collect()
    }

    /// Derive this account's root secret.
    ///
    /// BIP-39's own PBKDF2 stretching first, then HKDF for domain separation.
    /// The stretching buys nothing against a phrase this crate generated — 128
    /// bits of real entropy is not getting guessed — and it is here because it
    /// is what the standard says `to_seed` is, and a phrase somebody restores
    /// on a future version has to derive the same root as the one they wrote
    /// down today.
    pub fn root(&self) -> AccountRoot {
        let mut seed = self.0.to_seed_normalized("");
        let hk = Hkdf::<Sha256>::new(Some(SALT), &seed);
        let mut root = [0u8; MASTER_KEY_LEN];
        hk.expand(INFO_ROOT, &mut root)
            .expect("32 bytes is a valid HKDF-SHA256 output length");
        seed.zeroize();
        AccountRoot(root)
    }
}

impl std::fmt::Debug for RecoveryPhrase {
    /// Never print the words, not even in a panic message. A phrase in a log
    /// file is the account.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RecoveryPhrase(redacted)")
    }
}

/// What a phrase derives, and what a device keeps once it has an account.
///
/// Stored in the keyring beside the device seed — sealed under the master key,
/// like everything else there — so a launch derives the document's ID and key
/// without ever asking the person for their phrase again. The phrase itself is
/// shown once and never stored: a phrase on the disk is a phrase that can be
/// read off the disk, and the account root is strictly less useful to an
/// attacker than the words that made it, because the words are also every
/// *future* thing this project might derive.
#[derive(Clone, PartialEq, Eq, Zeroize, ZeroizeOnDrop)]
pub struct AccountRoot([u8; MASTER_KEY_LEN]);

impl AccountRoot {
    pub(crate) fn from_slice(raw: &[u8]) -> Result<Self> {
        let k: [u8; MASTER_KEY_LEN] = raw
            .try_into()
            .map_err(|_| CoreError::Crypto("an account root is 32 bytes"))?;
        Ok(Self(k))
    }

    pub(crate) fn as_bytes(&self) -> &[u8; MASTER_KEY_LEN] {
        &self.0
    }

    fn hkdf(&self) -> Hkdf<Sha256> {
        Hkdf::<Sha256>::new(Some(SALT), &self.0)
    }

    /// The key the account document's envelopes are sealed under.
    ///
    /// A [`NoteKey`] and not a new type, because to everything below
    /// [`crate::note::NoteCore`] the account document *is* a note: the same
    /// envelopes, the same AAD, the same log. What differs is only where the
    /// key comes from — random and kept, versus derived and re-derivable.
    pub fn document_key(&self) -> NoteKey {
        let mut key = [0u8; NOTE_KEY_LEN];
        self.hkdf()
            .expand(INFO_DOC_KEY, &mut key)
            .expect("32 bytes is a valid HKDF-SHA256 output length");
        NoteKey(key)
    }

    /// The ID the relay sees this account document under.
    ///
    /// Sixteen bytes that look like any other note ID, which is the point: a
    /// relay watching a socket cannot tell an account document from a shopping
    /// list, and cannot tell two of this person's devices from two people who
    /// share a note. See ADR 0014.
    pub fn document_id(&self) -> NoteId {
        let mut id = [0u8; 16];
        self.hkdf()
            .expand(INFO_DOC_ID, &mut id)
            .expect("16 bytes is a valid HKDF-SHA256 output length");
        NoteId::from_bytes(id)
    }
}

impl std::fmt::Debug for AccountRoot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AccountRoot(redacted)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_generated_phrase_is_twelve_words_and_parses_back() {
        let phrase = RecoveryPhrase::generate();
        assert_eq!(phrase.word_list().len(), PHRASE_WORDS);
        let again = RecoveryPhrase::parse(&phrase.words()).expect("our own phrase parses");
        assert_eq!(again.root(), phrase.root());
    }

    #[test]
    fn a_phrase_is_read_back_however_it_was_typed() {
        let phrase = RecoveryPhrase::generate();
        let words = phrase.words();
        let shouted = format!("  {}  ", words.to_uppercase().replace(' ', "\n  "));
        assert_eq!(
            RecoveryPhrase::parse(&shouted)
                .expect("case and spacing are not the secret")
                .root(),
            phrase.root()
        );
    }

    #[test]
    fn two_phrases_are_two_accounts() {
        let a = RecoveryPhrase::generate().root();
        let b = RecoveryPhrase::generate().root();
        assert_ne!(a.document_id(), b.document_id());
        assert_ne!(a.document_key().0, b.document_key().0);
    }

    /// The whole reason for taking a checksummed standard rather than inventing
    /// a wordlist: a swap has to be *refused*, not silently derive an empty
    /// account.
    #[test]
    fn swapping_two_words_is_refused_rather_than_deriving_an_empty_account() {
        // Not every swap breaks the checksum — it is four bits, so roughly one
        // in sixteen slips through — so try phrases until one does, which also
        // proves the checksum is being checked at all.
        let mut caught = 0;
        for _ in 0..64 {
            let phrase = RecoveryPhrase::generate();
            let mut words = phrase.word_list();
            words.swap(0, 1);
            if words[0] == words[1] {
                continue;
            }
            if RecoveryPhrase::parse(&words.join(" ")).is_err() {
                caught += 1;
            }
        }
        assert!(
            caught > 32,
            "a word swap should almost always be caught; caught {caught} of 64"
        );
    }

    #[test]
    fn a_phrase_of_the_wrong_length_says_which_way_it_is_wrong() {
        let phrase = RecoveryPhrase::generate();
        let words = phrase.word_list();
        let short = RecoveryPhrase::parse(&words[..11].join(" ")).unwrap_err();
        assert!(format!("{short}").contains("short"), "{short}");
        let long = RecoveryPhrase::parse(&format!("{} {}", phrase.words(), words[0])).unwrap_err();
        assert!(format!("{long}").contains("long"), "{long}");
    }

    #[test]
    fn a_word_that_is_not_in_the_list_is_named_as_the_problem() {
        let phrase = RecoveryPhrase::generate();
        let mut words = phrase.word_list();
        words[3] = "easynotes".to_string();
        let e = RecoveryPhrase::parse(&words.join(" ")).unwrap_err();
        assert!(format!("{e}").contains("wordlist"), "{e}");
    }

    /// The three derived values must never be each other. They come out of one
    /// HKDF with three `info` strings, and this is the test that the strings are
    /// actually distinct rather than three copies of a constant.
    #[test]
    fn the_root_the_key_and_the_id_are_three_different_secrets() {
        let root = RecoveryPhrase::generate().root();
        assert_ne!(root.as_bytes()[..16], root.document_id().as_bytes()[..]);
        assert_ne!(root.as_bytes(), &root.document_key().0);
        assert_ne!(&root.document_key().0[..16], root.document_id().as_bytes());
    }

    /// A phrase must derive the same account for ever — somebody writes these
    /// words on paper and reads them back in five years. This pins the whole
    /// chain (BIP-39 seed, salt, and all three `info` strings) against a known
    /// phrase, so any change to the derivation fails here loudly instead of
    /// quietly stranding everyone's notes.
    #[test]
    fn the_derivation_is_pinned_to_a_known_phrase() {
        let phrase = RecoveryPhrase::parse(
            "abandon abandon abandon abandon abandon abandon \
             abandon abandon abandon abandon abandon about",
        )
        .expect("the BIP-39 all-zero test vector");
        let root = phrase.root();
        assert_eq!(hex::encode(root.as_bytes()), ROOT_HEX);
        assert_eq!(root.document_id().encode(), DOC_ID);
        assert_eq!(hex::encode(root.document_key().0), DOC_KEY_HEX);
    }

    // Filled in from the first run; see the test above for what changing them
    // would mean.
    const ROOT_HEX: &str = "925ff2328daba3c06a4e97f31a2573a212ce985a0d973aecece63b0872859461";
    const DOC_ID: &str = "UezCwQnEs1enTxs3dJy2Zw";
    const DOC_KEY_HEX: &str = "76d537d4b496604659066d842b61b301aee278834e9e6142a02f712f0b70a814";
}
