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
const TOKEN_PAYLOAD_LEN: usize = 1 + 8 + 32 + 16;

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
        let mut payload = Vec::with_capacity(TOKEN_PAYLOAD_LEN);
        payload.push(1);
        payload.extend_from_slice(&expires_at.to_be_bytes());
        payload.extend_from_slice(&device);
        payload.extend_from_slice(&nonce);
        let mut transcript = TOKEN_DOMAIN.to_vec();
        transcript.extend_from_slice(&payload);
        let signature = self.signing_key.sign(&transcript);
        payload.extend_from_slice(&signature.to_bytes());
        Ok(URL_SAFE_NO_PAD.encode(payload))
    }

    /// The device behind a token, if it is still valid.
    pub fn device_for(&self, token: &str) -> Option<[u8; 32]> {
        let bytes = URL_SAFE_NO_PAD.decode(token).ok()?;
        if bytes.len() != TOKEN_PAYLOAD_LEN + 64 || bytes[0] != 1 {
            return None;
        }
        let (payload, signature) = bytes.split_at(TOKEN_PAYLOAD_LEN);
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
        payload[9..41].try_into().ok()
    }
}

pub(crate) fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
