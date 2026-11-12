//! Authentication without accounts.
//!
//! A client proves it holds a device key by signing a fixed-size, domain-separated
//! relay-authentication transcript. There is no registration, no username, no
//! password and no email — the relay learns a public key and nothing else.
//!
//! Access tokens are signed by the relay and carry the device key and expiry.
//! They can be checked by every relay instance configured with the same signing
//! key, without keeping a long-lived device session row. One-use challenges are
//! transiently stored so replicas can redeem them atomically.

use crate::error::{RelayError, Result};
use crate::store::RelayStore;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub struct Auth {
    signing_key: SigningKey,
}

const TOKEN_DOMAIN: &[u8] = b"easynotes-relay-token/v1\0";
const TOKEN_PAYLOAD_V1_LEN: usize = 1 + 8 + 32 + 16;
/// Version 2 adds the device's read-only flag
/// ([ADR 0038](../../docs/decisions.md#adr-0038)).
const TOKEN_PAYLOAD_V2_LEN: usize = TOKEN_PAYLOAD_V1_LEN + 1;

/// The device behind a valid token.
///
/// A flip to read-only, or a revocation, takes effect at the device's next
/// handshake — up to one token lifetime later
/// ([`crate::protocol::TOKEN_TTL_SECS`], 15 minutes unless
/// `RELAY_TOKEN_TTL_SECS` says otherwise). A socket opened with a token
/// closes when the token does, so it must handshake again to learn the new
/// flag.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Device {
    /// A lapsed household may still read for 30 days before its tokens are
    /// revoked, but may not write ([ADR 0038](../../docs/decisions.md#adr-0038)).
    pub read_only: bool,
    /// Unix seconds from the signed payload.
    pub expires_at: u64,
}

fn b64(s: &str, what: &'static str) -> Result<Vec<u8>> {
    URL_SAFE_NO_PAD
        .decode(s)
        .map_err(|_| RelayError::BadRequest(format!("{what} is not base64url")))
}

fn device_key(s: &str) -> Result<[u8; 32]> {
    b64(s, "device key")?
        .try_into()
        .map_err(|_| RelayError::BadRequest("device key is not 32 bytes".into()))
}

impl Auth {
    pub fn new(signing_key: [u8; 32]) -> Self {
        Self {
            signing_key: SigningKey::from_bytes(&signing_key),
        }
    }

    /// Hand out a fresh fixed-size nonce bound to the claimed device key.
    pub fn issue_challenge(&self, device: &str) -> Result<(String, [u8; 32])> {
        let device = device_key(device)?;
        // Reject a key that is not a valid Ed25519 point now, rather than
        // discovering it when the signature check fails.
        VerifyingKey::from_bytes(&device).map_err(|_| {
            RelayError::BadRequest("device key is not a valid Ed25519 point".into())
        })?;

        let mut nonce = [0u8; easynotes_core::crypto::RELAY_AUTH_NONCE_LEN];
        easynotes_core::crypto::random_bytes(&mut nonce);

        Ok((URL_SAFE_NO_PAD.encode(nonce), device))
    }

    /// Redeem a domain-separated authentication signature for a token.
    pub async fn redeem(
        &self,
        store: &dyn RelayStore,
        device: &str,
        nonce: &str,
        signature: &str,
        ttl: Duration,
        read_only: bool,
    ) -> Result<String> {
        let device = device_key(device)?;
        let nonce: [u8; easynotes_core::crypto::RELAY_AUTH_NONCE_LEN] = b64(nonce, "nonce")?
            .try_into()
            .map_err(|_| RelayError::BadRequest("nonce is not 32 bytes".into()))?;
        let signature: [u8; 64] = b64(signature, "signature")?
            .try_into()
            .map_err(|_| RelayError::BadRequest("signature is not 64 bytes".into()))?;

        // The shared store makes a challenge redeemable through any instance,
        // and the atomic delete preserves the single-use property.
        let challenged_device = store
            .consume_auth_challenge(&nonce, unix_now() as i64)
            .await?
            .ok_or(RelayError::Unauthorized)?;
        if challenged_device != device {
            return Err(RelayError::Unauthorized);
        }

        let key = VerifyingKey::from_bytes(&device).map_err(|_| RelayError::Unauthorized)?;
        let transcript = easynotes_core::crypto::relay_auth_transcript(&nonce);
        key.verify(&transcript, &Signature::from_bytes(&signature))
            .map_err(|_| RelayError::Unauthorized)?;

        let expires_at = unix_now().saturating_add(ttl.as_secs());
        let mut nonce = [0u8; 16];
        easynotes_core::crypto::random_bytes(&mut nonce);
        let mut payload = Vec::with_capacity(TOKEN_PAYLOAD_V2_LEN);
        payload.push(2);
        payload.extend_from_slice(&expires_at.to_be_bytes());
        payload.extend_from_slice(&device);
        payload.extend_from_slice(&nonce);
        payload.push(read_only as u8);
        let mut transcript = TOKEN_DOMAIN.to_vec();
        transcript.extend_from_slice(&payload);
        let signature = self.signing_key.sign(&transcript);
        payload.extend_from_slice(&signature.to_bytes());
        Ok(URL_SAFE_NO_PAD.encode(payload))
    }

    /// The device behind a token, if it is still valid. Version 1 tokens are
    /// writable; version 2 carries the read-only flag.
    pub fn device_for(&self, token: &str) -> Option<Device> {
        let bytes = URL_SAFE_NO_PAD.decode(token).ok()?;
        let payload_len = match bytes.first()? {
            1 => TOKEN_PAYLOAD_V1_LEN,
            2 => TOKEN_PAYLOAD_V2_LEN,
            _ => return None,
        };
        if bytes.len() != payload_len + 64 {
            return None;
        }
        let (payload, signature) = bytes.split_at(payload_len);
        let signature: [u8; 64] = signature.try_into().ok()?;
        let mut transcript = TOKEN_DOMAIN.to_vec();
        transcript.extend_from_slice(payload);
        self.signing_key
            .verifying_key()
            .verify(&transcript, &Signature::from_bytes(&signature))
            .ok()?;
        let expires_at = u64::from_be_bytes(payload[1..9].try_into().ok()?);
        if expires_at <= unix_now() {
            return None;
        }
        Some(Device {
            read_only: payload_len == TOKEN_PAYLOAD_V2_LEN && payload[TOKEN_PAYLOAD_V1_LEN] != 0,
            expires_at,
        })
    }
}

pub(crate) fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::MemoryStore;

    /// Mint a token through the real challenge/redeem path.
    async fn mint(
        auth: &Auth,
        store: &MemoryStore,
        device: &SigningKey,
        ttl: Duration,
        read_only: bool,
    ) -> String {
        let device_b64 = URL_SAFE_NO_PAD.encode(device.verifying_key().to_bytes());
        let (nonce_b64, device_bytes) = auth.issue_challenge(&device_b64).unwrap();
        let nonce: [u8; easynotes_core::crypto::RELAY_AUTH_NONCE_LEN] = URL_SAFE_NO_PAD
            .decode(&nonce_b64)
            .unwrap()
            .try_into()
            .unwrap();
        store
            .save_auth_challenge(&nonce, &device_bytes, unix_now() as i64 + 60)
            .await
            .unwrap();
        let signature = device.sign(&easynotes_core::crypto::relay_auth_transcript(&nonce));
        auth.redeem(
            store,
            &device_b64,
            &nonce_b64,
            &URL_SAFE_NO_PAD.encode(signature.to_bytes()),
            ttl,
            read_only,
        )
        .await
        .unwrap()
    }

    /// A token as this relay issued before the read-only flag existed.
    fn mint_v1(auth: &Auth, device: [u8; 32], ttl: Duration) -> String {
        let mut payload = vec![1];
        payload.extend_from_slice(&(unix_now() + ttl.as_secs()).to_be_bytes());
        payload.extend_from_slice(&device);
        payload.extend_from_slice(&[7; 16]);
        let mut transcript = TOKEN_DOMAIN.to_vec();
        transcript.extend_from_slice(&payload);
        payload.extend_from_slice(&auth.signing_key.sign(&transcript).to_bytes());
        URL_SAFE_NO_PAD.encode(payload)
    }

    #[tokio::test]
    async fn an_expired_token_is_rejected() {
        let auth = Auth::new([1; 32]);
        let store = MemoryStore::new();
        let device = SigningKey::from_bytes(&[2; 32]);

        let token = mint(&auth, &store, &device, Duration::ZERO, false).await;

        assert_eq!(auth.device_for(&token), None);
    }

    #[tokio::test]
    async fn a_token_with_a_flipped_payload_byte_is_rejected() {
        let auth = Auth::new([1; 32]);
        let store = MemoryStore::new();
        let device = SigningKey::from_bytes(&[2; 32]);
        let token = mint(&auth, &store, &device, Duration::from_secs(900), false).await;
        let device = auth.device_for(&token).unwrap();
        assert!(!device.read_only);
        assert!(device.expires_at > unix_now());

        let mut bytes = URL_SAFE_NO_PAD.decode(&token).unwrap();
        bytes[1] ^= 1;
        let forged = URL_SAFE_NO_PAD.encode(bytes);

        assert_eq!(auth.device_for(&forged), None);
    }

    #[tokio::test]
    async fn a_read_only_token_says_so_and_a_v1_token_stays_writable() {
        let auth = Auth::new([1; 32]);
        let store = MemoryStore::new();
        let device = SigningKey::from_bytes(&[2; 32]);
        let key = device.verifying_key().to_bytes();

        let token = mint(&auth, &store, &device, Duration::from_secs(900), true).await;
        assert!(auth.device_for(&token).unwrap().read_only);

        // An old relay's version-1 token still verifies, and means writable.
        let token = mint_v1(&auth, key, Duration::from_secs(900));
        assert!(!auth.device_for(&token).unwrap().read_only);

        // A token of neither length is nobody's.
        let mut bytes = URL_SAFE_NO_PAD.decode(&token).unwrap();
        bytes.pop();
        assert_eq!(auth.device_for(&URL_SAFE_NO_PAD.encode(&bytes)), None);
    }

    #[tokio::test]
    async fn a_token_cannot_be_edited_into_another_shape() {
        let auth = Auth::new([1; 32]);
        let store = MemoryStore::new();
        let device = SigningKey::from_bytes(&[2; 32]);
        let key = device.verifying_key().to_bytes();

        // A read-only token whose flag is flipped says writable, but its
        // signature no longer verifies.
        let token = mint(&auth, &store, &device, Duration::from_secs(900), true).await;
        let mut bytes = URL_SAFE_NO_PAD.decode(&token).unwrap();
        bytes[TOKEN_PAYLOAD_V1_LEN] ^= 1;
        assert_eq!(auth.device_for(&URL_SAFE_NO_PAD.encode(&bytes)), None);

        // A version-2 token relabelled version 1 has the wrong length.
        let mut bytes = URL_SAFE_NO_PAD.decode(&token).unwrap();
        bytes[0] = 1;
        assert_eq!(auth.device_for(&URL_SAFE_NO_PAD.encode(&bytes)), None);

        // Nor may a version-1 token grow a version-2 flag.
        let mut bytes = URL_SAFE_NO_PAD
            .decode(mint_v1(&auth, key, Duration::from_secs(900)))
            .unwrap();
        bytes.push(0);
        assert_eq!(auth.device_for(&URL_SAFE_NO_PAD.encode(&bytes)), None);
    }

    #[tokio::test]
    async fn a_token_from_another_signing_key_is_rejected() {
        let auth = Auth::new([1; 32]);
        let elsewhere = Auth::new([3; 32]);
        let store = MemoryStore::new();
        let device = SigningKey::from_bytes(&[2; 32]);

        let token = mint(&elsewhere, &store, &device, Duration::from_secs(900), false).await;

        assert_eq!(auth.device_for(&token), None);
    }
}
