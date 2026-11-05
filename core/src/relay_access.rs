//! Who may write to a relay ([ADR 0028](../../docs/decisions.md#adr-0028)).
//!
//! A relay's operator may set an access key and hand it to the people they let
//! in. Everyone holds the same one, so proving it tells the relay nothing about
//! who anybody is — only that a device's owner was told the key, which is true
//! of everyone it serves.
//!
//! The key itself never crosses the wire. At the handshake a device sends an
//! HMAC of the relay's single-use nonce and its own device key, so a proof
//! cannot be replayed onto another handshake or another device, and a proxy
//! that logs request bodies never sees anything that opens the relay. Invite
//! calls, which have no device key on purpose (ADR 0015), send an HMAC of the
//! invite ID instead: the same for everyone who holds the link and the key, so
//! it links nobody.
//!
//! Both ends build the proofs here, the relay to check them and a device to
//! make them, so the two cannot disagree about the bytes.

use crate::error::{CoreError, Result};
use crate::ids::InviteId;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use hmac::digest::KeyInit;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use zeroize::{Zeroize, ZeroizeOnDrop};

/// How many bytes an access key is.
pub const ACCESS_KEY_LEN: usize = 32;

/// What a device proves at `POST /v1/token`: the nonce, then its device key.
const TOKEN_DOMAIN: &[u8] = b"easynotes/relay-access/v1/token\0";

/// What an invite call proves: the invite ID.
const INVITE_DOMAIN: &[u8] = b"easynotes/relay-access/v1/invite\0";

/// The fragment a relay link carries its key in: `https://relay.example/#access=…`.
/// A fragment, so that a link pasted into a browser never sends the key to a
/// server, and never lands in an access log.
pub const LINK_FRAGMENT: &str = "access=";

/// A relay's access key. Never printed: `Debug` says only that there is one.
#[derive(Clone, PartialEq, Eq, Zeroize, ZeroizeOnDrop)]
pub struct RelayAccess([u8; ACCESS_KEY_LEN]);

impl std::fmt::Debug for RelayAccess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RelayAccess(..)")
    }
}

impl RelayAccess {
    pub fn from_bytes(bytes: [u8; ACCESS_KEY_LEN]) -> Self {
        Self(bytes)
    }

    /// A new key, for an operator setting up a relay.
    pub fn random() -> Self {
        let mut key = [0; ACCESS_KEY_LEN];
        crate::crypto::random_bytes(&mut key);
        Self(key)
    }

    /// Read a key as base64url, the way an operator writes it and a relay link
    /// carries it. Padding and surrounding space are forgiven; anything that is
    /// not exactly 32 bytes is not a key.
    pub fn decode(raw: &str) -> Result<Self> {
        let bad = || CoreError::Storage("that is not a server access key".into());
        let raw = raw.trim().trim_end_matches('=');
        let bytes = URL_SAFE_NO_PAD.decode(raw).map_err(|_| bad())?;
        let key: [u8; ACCESS_KEY_LEN] = bytes.try_into().map_err(|_| bad())?;
        Ok(Self(key))
    }

    pub fn encode(&self) -> String {
        URL_SAFE_NO_PAD.encode(self.0)
    }

    fn mac(&self, domain: &[u8], parts: &[&[u8]]) -> Hmac<Sha256> {
        let mut mac =
            <Hmac<Sha256> as KeyInit>::new_from_slice(&self.0).expect("HMAC takes any key length");
        mac.update(domain);
        for part in parts {
            mac.update(part);
        }
        mac
    }

    /// The proof a device sends with its token request, base64url.
    pub fn token_proof(&self, nonce: &[u8], device: &[u8; 32]) -> String {
        let proof = self.mac(TOKEN_DOMAIN, &[nonce, device]).finalize();
        URL_SAFE_NO_PAD.encode(proof.into_bytes())
    }

    /// Whether `proof` is this key's for that handshake. Constant time.
    pub fn verifies_token(&self, nonce: &[u8], device: &[u8; 32], proof: &str) -> bool {
        match URL_SAFE_NO_PAD.decode(proof) {
            Ok(proof) => self
                .mac(TOKEN_DOMAIN, &[nonce, device])
                .verify_slice(&proof)
                .is_ok(),
            Err(_) => false,
        }
    }

    /// The proof an invite call carries, base64url.
    pub fn invite_proof(&self, invite: &InviteId) -> String {
        let proof = self.mac(INVITE_DOMAIN, &[&invite.0]).finalize();
        URL_SAFE_NO_PAD.encode(proof.into_bytes())
    }

    /// Whether `proof` is this key's for that invite. Constant time.
    pub fn verifies_invite(&self, invite: &InviteId, proof: &str) -> bool {
        match URL_SAFE_NO_PAD.decode(proof) {
            Ok(proof) => self
                .mac(INVITE_DOMAIN, &[&invite.0])
                .verify_slice(&proof)
                .is_ok(),
            Err(_) => false,
        }
    }

    /// A relay link: the relay's address with this key in its fragment.
    pub fn link(&self, address: &str) -> String {
        format!(
            "{}/#{LINK_FRAGMENT}{}",
            address.trim_end_matches('/'),
            self.encode()
        )
    }
}

/// How many bytes a device token's ID is.
const TOKEN_ID_LEN: usize = 16;

/// A device's access to one relay ([ADR 0038](../../docs/decisions.md#adr-0038)).
///
/// `{id, secret}`: the ID is a public lookup handle the relay stores, and the
/// secret is a [`RelayAccess`] that never crosses the wire. An operator's static
/// key is the same thing with no ID, so both shapes travel the same way.
#[derive(Clone, PartialEq, Eq, Zeroize, ZeroizeOnDrop)]
pub struct RelayToken {
    /// `None` for a legacy operator key; `Some` for a billed device token.
    pub id: Option<String>,
    pub secret: RelayAccess,
}

impl std::fmt::Debug for RelayToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RelayToken(..)")
    }
}

impl RelayToken {
    /// A new token, for a billing program issuing one device.
    pub fn random() -> Self {
        let mut id = [0u8; TOKEN_ID_LEN];
        crate::crypto::random_bytes(&mut id);
        Self {
            id: Some(URL_SAFE_NO_PAD.encode(id)),
            secret: RelayAccess::random(),
        }
    }

    /// `<id>.<secret>`, or the bare secret for an operator key.
    pub fn encode(&self) -> String {
        match &self.id {
            Some(id) => format!("{id}.{}", self.secret.encode()),
            None => self.secret.encode(),
        }
    }

    /// Read a token back. A string with no dot is a legacy operator key.
    pub fn decode(raw: &str) -> Result<Self> {
        let bad = || CoreError::Storage("that is not a device access token".into());
        let Some((id, secret)) = raw.split_once('.') else {
            return Ok(Self {
                id: None,
                secret: RelayAccess::decode(raw)?,
            });
        };
        if URL_SAFE_NO_PAD.decode(id).map_err(|_| bad())?.len() != TOKEN_ID_LEN {
            return Err(bad());
        }
        Ok(Self {
            id: Some(id.to_owned()),
            secret: RelayAccess::decode(secret)?,
        })
    }

    /// The proof a device sends with its token request, base64url.
    pub fn token_proof(&self, nonce: &[u8], device: &[u8; 32]) -> String {
        self.secret.token_proof(nonce, device)
    }

    /// The proof an invite call carries, base64url.
    pub fn invite_proof(&self, invite: &InviteId) -> String {
        self.secret.invite_proof(invite)
    }

    /// A relay link: the relay's address with this token in its fragment.
    pub fn link(&self, address: &str) -> String {
        format!(
            "{}/#{LINK_FRAGMENT}{}",
            address.trim_end_matches('/'),
            self.encode()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_round_trips_and_refuses_the_wrong_length() {
        let key = RelayAccess::random();
        assert_eq!(RelayAccess::decode(&key.encode()).unwrap(), key);
        assert_eq!(
            RelayAccess::decode(&format!(" {}= ", key.encode())).unwrap(),
            key
        );
        assert!(RelayAccess::decode("").is_err());
        assert!(RelayAccess::decode(&URL_SAFE_NO_PAD.encode([0u8; 31])).is_err());
        assert!(RelayAccess::decode("not a key!").is_err());
    }

    #[test]
    fn a_key_is_never_printed() {
        let key = RelayAccess::random();
        assert!(!format!("{key:?}").contains(&key.encode()));
    }

    #[test]
    fn a_token_proof_is_bound_to_its_nonce_and_device() {
        let key = RelayAccess::random();
        let (nonce, device) = ([1u8; 32], [2u8; 32]);
        let proof = key.token_proof(&nonce, &device);
        assert!(key.verifies_token(&nonce, &device, &proof));
        assert!(!key.verifies_token(&[3u8; 32], &device, &proof));
        assert!(!key.verifies_token(&nonce, &[3u8; 32], &proof));
        assert!(!RelayAccess::random().verifies_token(&nonce, &device, &proof));
        assert!(!key.verifies_token(&nonce, &device, "!"));
        assert!(!key.verifies_token(&nonce, &device, ""));
    }

    #[test]
    fn an_invite_proof_is_bound_to_its_invite_and_is_not_a_token_proof() {
        let key = RelayAccess::random();
        let invite = InviteId::from_bytes([4; 16]);
        let proof = key.invite_proof(&invite);
        assert!(key.verifies_invite(&invite, &proof));
        assert!(!key.verifies_invite(&InviteId::from_bytes([5; 16]), &proof));
        assert!(!RelayAccess::random().verifies_invite(&invite, &proof));
        // The domains keep one kind of proof from standing in for the other.
        let token = key.token_proof(&[4; 16], &[0; 32]);
        assert!(!key.verifies_invite(&invite, &token));
    }

    #[test]
    fn a_relay_link_carries_the_key_in_its_fragment() {
        let key = RelayAccess::random();
        assert_eq!(
            key.link("https://relay.example/"),
            format!("https://relay.example/#access={}", key.encode())
        );
    }

    #[test]
    fn a_token_is_an_id_and_a_secret() {
        let token = RelayToken::random();
        assert_eq!(RelayToken::decode(&token.encode()).unwrap(), token);
        assert_eq!(token.encode().matches('.').count(), 1);
    }

    #[test]
    fn a_bare_key_is_still_a_key() {
        let key = RelayAccess::random();
        let token = RelayToken::decode(&key.encode()).unwrap();
        assert!(token.id.is_none());
        assert_eq!(token.secret, key);
    }

    #[test]
    fn a_link_carries_the_id_and_the_secret() {
        let token = RelayToken::random();
        assert_eq!(
            token.link("https://relay.example/"),
            format!("https://relay.example/#access={}", token.encode())
        );
    }

    #[test]
    fn a_token_without_a_sixteen_byte_id_is_not_a_token() {
        let key = RelayAccess::random();
        assert!(RelayToken::decode(&format!(".{}", key.encode())).is_err());
        let short = URL_SAFE_NO_PAD.encode([0u8; 15]);
        assert!(RelayToken::decode(&format!("{short}.{}", key.encode())).is_err());
    }
}
