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
        let private = x25519_dalek::StaticSecret::from(secret);
        secret.zeroize();
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
