//! One-time encryption for adding a device from an approved QR pairing.

use crate::account::AccountRoot;
use crate::error::{CoreError, Result};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use hkdf::Hkdf;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::{Zeroize, ZeroizeOnDrop};

const INFO: &[u8] = b"easynotes/account-transfer/v1";

#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct TransferKeyPair {
    secret: [u8; 32],
    public: [u8; 32],
}

impl TransferKeyPair {
    pub fn generate() -> Self {
        let mut secret = [0u8; 32];
        crate::crypto::random_bytes(&mut secret);
        // Arrays are copied, not borrowed, so the bytes here are the ones that
        // must be kept: wiping them before they reach `Self` left every
        // keypair holding the same all-zero secret while still advertising a
        // random public key, so two devices derived different codes.
        let private = x25519_dalek::StaticSecret::from(secret);
        let public = x25519_dalek::PublicKey::from(&private).to_bytes();
        Self { secret, public }
    }

    pub fn public_key(&self) -> [u8; 32] {
        self.public
    }

    fn shared(&self, peer: &[u8; 32]) -> Result<[u8; 32]> {
        let secret = x25519_dalek::StaticSecret::from(self.secret);
        let shared = secret
            .diffie_hellman(&x25519_dalek::PublicKey::from(*peer))
            .to_bytes();
        if shared.iter().all(|b| *b == 0) {
            return Err(CoreError::Crypto("invalid transfer key"));
        }
        Ok(shared)
    }

    pub fn verification_code(&self, peer: &[u8; 32], session: &[u8]) -> Result<String> {
        let mut shared = self.shared(peer)?;
        let mut hash = Sha256::new();
        hash.update(INFO);
        hash.update(session);
        hash.update(shared);
        shared.zeroize();
        let digest = hash.finalize();
        let number = u32::from_be_bytes([digest[0], digest[1], digest[2], digest[3]]) % 1_000_000;
        Ok(format!("{number:06}"))
    }

    pub fn seal(
        &self,
        recipient: &[u8; 32],
        session: &[u8],
        root: &AccountRoot,
        relay_url: &str,
        relay_access: Option<&str>,
    ) -> Result<Vec<u8>> {
        let mut shared = self.shared(recipient)?;
        let hk = Hkdf::<Sha256>::new(Some(session), &shared);
        shared.zeroize();
        let mut key = [0u8; 32];
        hk.expand(INFO, &mut key)
            .map_err(|_| CoreError::Crypto("could not derive transfer key"))?;
        let cipher = ChaCha20Poly1305::new((&key).into());
        key.zeroize();
        let mut root_bytes = *root.as_bytes();
        let mut payload = PayloadData {
            root: URL_SAFE_NO_PAD.encode(root_bytes),
            relay_url: relay_url.to_string(),
            relay_access: relay_access.map(str::to_owned),
        };
        root_bytes.zeroize();
        let mut plaintext =
            serde_json::to_vec(&payload).map_err(|e| CoreError::Storage(e.to_string()))?;
        payload.zeroize();
        let mut nonce = [0u8; 12];
        crate::crypto::random_bytes(&mut nonce);
        let encrypted = cipher
            .encrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: &plaintext,
                    aad: session,
                },
            )
            .map_err(|_| CoreError::Crypto("could not encrypt account transfer"))?;
        plaintext.zeroize();
        let mut out = nonce.to_vec();
        out.extend(encrypted);
        Ok(out)
    }

    pub fn open(
        &self,
        sender: &[u8; 32],
        session: &[u8],
        encrypted: &[u8],
    ) -> Result<OpenedTransfer> {
        if encrypted.len() < 28 {
            return Err(CoreError::Crypto("account transfer is incomplete"));
        }
        let mut shared = self.shared(sender)?;
        let hk = Hkdf::<Sha256>::new(Some(session), &shared);
        shared.zeroize();
        let mut key = [0u8; 32];
        hk.expand(INFO, &mut key)
            .map_err(|_| CoreError::Crypto("could not derive transfer key"))?;
        let cipher = ChaCha20Poly1305::new((&key).into());
        key.zeroize();
        let mut plain = cipher
            .decrypt(
                Nonce::from_slice(&encrypted[..12]),
                Payload {
                    msg: &encrypted[12..],
                    aad: session,
                },
            )
            .map_err(|_| CoreError::Crypto("account transfer could not be opened"))?;
        let data: PayloadData = serde_json::from_slice(&plain)
            .map_err(|_| CoreError::Crypto("invalid account transfer"))?;
        plain.zeroize();
        let mut raw = URL_SAFE_NO_PAD
            .decode(&data.root)
            .map_err(|_| CoreError::Crypto("invalid account key in transfer"))?;
        let root = AccountRoot::from_slice(&raw)?;
        raw.zeroize();
        Ok(OpenedTransfer {
            root,
            relay_url: data.relay_url.clone(),
            relay_access: data.relay_access.clone(),
        })
    }
}

#[derive(Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
struct PayloadData {
    root: String,
    relay_url: String,
    relay_access: Option<String>,
}

pub struct OpenedTransfer {
    pub root: AccountRoot,
    pub relay_url: String,
    pub relay_access: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::RecoveryPhrase;

    fn pair() -> (TransferKeyPair, TransferKeyPair) {
        (TransferKeyPair::generate(), TransferKeyPair::generate())
    }

    /// The six digits are the only thing a person can check, so they must match
    /// on both screens. They do only if each device kept the secret its public
    /// half was derived from: a wiped secret still produced a random-looking
    /// public key, and the two devices then hashed different shared secrets.
    #[test]
    fn both_devices_show_the_same_code() {
        let (owner, guest) = pair();
        let session = b"a transfer id";
        let on_owner = owner
            .verification_code(&guest.public_key(), session)
            .expect("a fresh keypair is a valid peer");
        let on_guest = guest
            .verification_code(&owner.public_key(), session)
            .expect("a fresh keypair is a valid peer");
        assert_eq!(on_owner, on_guest);
        assert_eq!(on_owner.len(), 6);
        assert!(on_owner.chars().all(|c| c.is_ascii_digit()));
    }

    /// Two keypairs must not be one keypair: the code is what separates a
    /// pairing the person started from one somebody else slipped in.
    #[test]
    fn two_pairings_are_two_codes() {
        let (owner, first) = pair();
        let second = TransferKeyPair::generate();
        assert_ne!(first.public_key(), second.public_key());
        assert_ne!(
            owner
                .verification_code(&first.public_key(), b"a transfer id")
                .unwrap(),
            owner
                .verification_code(&second.public_key(), b"a transfer id")
                .unwrap()
        );
    }

    #[test]
    fn a_sealed_account_opens_on_the_other_device() {
        let (owner, guest) = pair();
        let root = RecoveryPhrase::generate().root();
        let session = b"a transfer id";
        let sealed = owner
            .seal(
                &guest.public_key(),
                session,
                &root,
                "https://relay.example",
                Some("an access code"),
            )
            .unwrap();
        let opened = guest.open(&owner.public_key(), session, &sealed).unwrap();
        assert_eq!(opened.root, root);
        assert_eq!(opened.relay_url, "https://relay.example");
        assert_eq!(opened.relay_access.as_deref(), Some("an access code"));
    }

    /// The session IDs are the AAD, so a transfer replayed under another
    /// pairing's ID is refused rather than opened.
    #[test]
    fn a_transfer_does_not_open_under_another_session() {
        let (owner, guest) = pair();
        let root = RecoveryPhrase::generate().root();
        let sealed = owner
            .seal(
                &guest.public_key(),
                b"a transfer id",
                &root,
                "https://relay.example",
                None,
            )
            .unwrap();
        assert!(guest
            .open(&owner.public_key(), b"another transfer id", &sealed)
            .is_err());
    }
}
