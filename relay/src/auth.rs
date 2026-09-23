//! Authentication without accounts.
//!
//! A client proves it holds a device key by signing a fixed-size, domain-separated
//! relay-authentication transcript. There is no registration, no username, no
//! password and no email — the relay learns a public key and nothing else.
//!
//! Tokens are opaque random strings held in memory. That is fine for one relay
//! process and is what Slice 1 needs; running more than one instance means moving
//! to a signed token (the device key is already in hand to bind it to), and that
//! is a Slice 3 problem, not a design change.

use crate::error::{RelayError, Result};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub struct Issued {
    pub device: [u8; 32],
    expires_at: Instant,
}

struct Challenge {
    device: [u8; 32],
    expires_at: Instant,
}

#[derive(Default)]
pub struct Auth {
    challenges: Mutex<HashMap<Vec<u8>, Challenge>>,
    tokens: Mutex<HashMap<String, Issued>>,
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
    pub fn new() -> Self {
        Self::default()
    }

    /// Hand out a fresh fixed-size nonce bound to the claimed device key.
    pub fn challenge(&self, device: &str, ttl: Duration) -> Result<String> {
        let device = device_key(device)?;
        // Reject a key that is not a valid Ed25519 point now, rather than
        // discovering it when the signature check fails.
        VerifyingKey::from_bytes(&device).map_err(|_| {
            RelayError::BadRequest("device key is not a valid Ed25519 point".into())
        })?;

        let mut nonce = [0u8; easynotes_core::crypto::RELAY_AUTH_NONCE_LEN];
        easynotes_core::crypto::random_bytes(&mut nonce);

        let mut challenges = self.challenges.lock().unwrap();
        Self::expire_challenges(&mut challenges);
        challenges.insert(
            nonce.to_vec(),
            Challenge {
                device,
                expires_at: Instant::now() + ttl,
            },
        );
        Ok(URL_SAFE_NO_PAD.encode(nonce))
    }

    /// Redeem a domain-separated authentication signature for a token.
    pub fn redeem(
        &self,
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

        // Single-use: taking it out is what stops a captured challenge being
        // redeemed twice.
        let challenge = {
            let mut challenges = self.challenges.lock().unwrap();
            Self::expire_challenges(&mut challenges);
            challenges
                .remove(nonce.as_slice())
                .ok_or(RelayError::Unauthorized)?
        };
        if challenge.expires_at < Instant::now() || challenge.device != device {
            return Err(RelayError::Unauthorized);
        }

        let key = VerifyingKey::from_bytes(&device).map_err(|_| RelayError::Unauthorized)?;
        let transcript = easynotes_core::crypto::relay_auth_transcript(&nonce);
        key.verify(&transcript, &Signature::from_bytes(&signature))
            .map_err(|_| RelayError::Unauthorized)?;

        let mut raw = [0u8; 32];
        easynotes_core::crypto::random_bytes(&mut raw);
        let token = URL_SAFE_NO_PAD.encode(raw);

        let mut tokens = self.tokens.lock().unwrap();
        tokens.retain(|_, t| t.expires_at > Instant::now());
        tokens.insert(
            token.clone(),
            Issued {
                device,
                expires_at: Instant::now() + ttl,
            },
        );
        Ok(token)
    }

    /// The device behind a token, if it is still valid.
    pub fn device_for(&self, token: &str) -> Option<[u8; 32]> {
        let tokens = self.tokens.lock().unwrap();
        tokens
            .get(token)
            .filter(|t| t.expires_at > Instant::now())
            .map(|t| t.device)
    }

    fn expire_challenges(challenges: &mut HashMap<Vec<u8>, Challenge>) {
        let now = Instant::now();
        challenges.retain(|_, c| c.expires_at > now);
    }
}
