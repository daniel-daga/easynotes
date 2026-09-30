//! Who may write to this relay ([ADR 0028](../../docs/adr/0028-who-may-write-to-a-relay.md)).
//!
//! Open, as a development relay and the tests run, or gated on one or more
//! access keys that the operator hands to the people it serves. More than one
//! so that a rotation can overlap: add the new key, hand it out, remove the
//! old one once nobody uses it.
//!
//! What a gate covers is issuing a token, and so everything a token opens —
//! the sync socket and media — and every invite call, since an open invite
//! endpoint would store bytes without a token. Not purge, which only destroys
//! and only with authority from the note key, so a person whose access was
//! taken away can still delete what they put here. Not health, readiness or
//! metrics, which carry nothing anyone wrote.

use crate::error::{RelayError, Result};
use easynotes_core::ids::InviteId;
use easynotes_core::relay_access::RelayAccess;

#[derive(Clone, Debug, Default)]
pub enum Access {
    /// Anyone who can reach the relay may write to it.
    #[default]
    Open,
    /// Only a device proving one of these. Never empty.
    Keys(Vec<RelayAccess>),
}

impl Access {
    /// Read `RELAY_ACCESS_KEYS`: `open`, or base64url keys separated by
    /// commas. A key that does not read is an error, not a key skipped: a
    /// gate that silently lets fewer people in than the operator meant is
    /// found out by the people it locks out.
    pub fn parse(raw: &str) -> std::result::Result<Self, String> {
        if raw.trim() == "open" {
            return Ok(Self::Open);
        }
        let mut keys = Vec::new();
        for (i, entry) in raw.split(',').map(str::trim).enumerate() {
            if entry.is_empty() {
                continue;
            }
            keys.push(
                RelayAccess::decode(entry)
                    .map_err(|_| format!("entry {} is not a 32-byte base64url key", i + 1))?,
            );
        }
        if keys.is_empty() {
            return Err("no keys in the list; say `open` to run without one".into());
        }
        Ok(Self::Keys(keys))
    }

    pub fn gated(&self) -> bool {
        matches!(self, Self::Keys(_))
    }

    /// Check the proof a token request carries. Every key is tried, whatever
    /// the first answered, so how long this takes does not say which matched.
    pub fn check_token(&self, nonce: &[u8], device: &[u8; 32], proof: Option<&str>) -> Result<()> {
        self.check(proof, |key, proof| key.verifies_token(nonce, device, proof))
    }

    /// Check the proof an invite call carries, for the invite it names.
    pub fn check_invite(&self, invite: &str, proof: Option<&str>) -> Result<()> {
        if !self.gated() {
            return Ok(());
        }
        let invite = InviteId::decode(invite)
            .map_err(|_| RelayError::BadRequest("malformed invite id".into()))?;
        self.check(proof, |key, proof| key.verifies_invite(&invite, proof))
    }

    fn check(
        &self,
        proof: Option<&str>,
        verifies: impl Fn(&RelayAccess, &str) -> bool,
    ) -> Result<()> {
        let Self::Keys(keys) = self else {
            return Ok(());
        };
        let proof = proof.ok_or(RelayError::AccessRequired)?;
        let matched = keys
            .iter()
            .fold(false, |matched, key| verifies(key, proof) | matched);
        if matched {
            Ok(())
        } else {
            Err(RelayError::AccessRequired)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_list_reads_or_fails_to_start() {
        assert!(!Access::parse("open").unwrap().gated());
        let (a, b) = (RelayAccess::random(), RelayAccess::random());
        match Access::parse(&format!(" {}, {} ,", a.encode(), b.encode())).unwrap() {
            Access::Keys(keys) => assert_eq!(keys, vec![a, b]),
            Access::Open => panic!("keys were given"),
        }
        assert!(Access::parse("").is_err());
        assert!(Access::parse(" , ").is_err());
        assert!(Access::parse("not-a-key").is_err());
    }

    #[test]
    fn either_key_opens_the_relay_during_a_rotation() {
        let (old, new) = (RelayAccess::random(), RelayAccess::random());
        let access = Access::Keys(vec![old.clone(), new.clone()]);
        let (nonce, device) = ([7u8; 32], [8u8; 32]);
        for key in [&old, &new] {
            let proof = key.token_proof(&nonce, &device);
            assert!(access.check_token(&nonce, &device, Some(&proof)).is_ok());
        }
        let stranger = RelayAccess::random().token_proof(&nonce, &device);
        assert!(matches!(
            access.check_token(&nonce, &device, Some(&stranger)),
            Err(RelayError::AccessRequired)
        ));
        assert!(matches!(
            access.check_token(&nonce, &device, None),
            Err(RelayError::AccessRequired)
        ));
    }

    #[test]
    fn an_open_relay_asks_for_nothing() {
        assert!(Access::Open.check_token(&[0; 32], &[0; 32], None).is_ok());
        assert!(Access::Open.check_invite("not even an id", None).is_ok());
    }
}
