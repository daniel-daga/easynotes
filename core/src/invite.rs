//! Invite links: how a note key reaches another *person*.
//!
//! A share blob put the note key in the link, which made the link the key: it
//! never expired, nobody could withdraw it, and anyone it was forwarded to was
//! a member. An invite link carries a secret instead, and the key is handed
//! over only when the owner says so. See `docs/adr/0015-invites.md` for the
//! decisions; this module is the cryptography, and nothing here talks to a
//! relay or touches a disk.
//!
//! ## What the link's secret derives
//!
//! Thirty-two random bytes, after the `#`, so a browser never sends them to a
//! server. From them, with HKDF-SHA256 under separate labels:
//!
//! * the **invite ID** — sixteen bytes, the invite's address on the relay;
//! * the **join token** — what a guest presents to the relay to leave a
//!   request, of which the relay holds only the hash;
//! * the **request key** — which seals what a guest says about itself, so the
//!   relay carries a request without reading the name in it or the key it
//!   asks to be answered under.
//!
//! The owner has one more secret, random and never in the link, whose hash the
//! relay keeps: presenting it is what lets the owner list the requests and
//! answer one. A guest holding the link cannot approve itself.
//!
//! ## The grant
//!
//! A guest makes a fresh X25519 key for each request and puts the public half
//! in it. Approving seals the note ID and key to that public key: an ephemeral
//! X25519 agreement, run through HKDF with the request key as the salt, into
//! XChaCha20-Poly1305. So the grant opens for the one guest the owner approved
//! — not for someone else holding the same link, and not for the relay — and
//! the salt means only something that held the link could have made it.
//!
//! The key is fresh per request rather than the device's own X25519 key on
//! purpose: a device key in a request would be a stable identifier the owner's
//! device learns about the guest's, which it has no need of.
//!
//! ## The safety code
//!
//! Six digits both screens show, computed from the invite and the guest's
//! request key. A leaked link lets a stranger ask to join; the code is how an
//! owner who is on the phone with the person they meant to invite can tell
//! the two requests apart. It is a check a person may make, not one the
//! protocol needs.

use crate::crypto::{
    local_aad, open_blob, random_bytes, seal_blob, AuthorKey, NoteKey, NOTE_KEY_LEN, PUBKEY_LEN,
};
use crate::error::{CoreError, Result};
use crate::ids::{InviteId, NoteId};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chacha20poly1305::aead::KeyInit;
use chacha20poly1305::XChaCha20Poly1305;
use hkdf::Hkdf;
use sha2::{Digest, Sha256};
use zeroize::{Zeroize, ZeroizeOnDrop};

/// The first byte of a link's secret, so a later format is a new number rather
/// than a guess.
pub const INVITE_VERSION: u8 = 1;

/// What goes in front of the secret when the app shows a link. Anything up to a
/// `#` is ignored when one is read back, so a link pasted from a web page or a
/// bare code both work.
pub const LINK_PREFIX: &str = "easynotes://join#";

/// The longest name a guest may give, in bytes. A request is sealed and the
/// relay caps it; this keeps a name well inside the cap and a screen.
pub const MAX_NAME_BYTES: usize = 64;

const SECRET_LEN: usize = 32;
const PUBLIC_LEN: usize = 32;

const ID_LABEL: &[u8] = b"easynotes/invite/id/v1";
const TOKEN_LABEL: &[u8] = b"easynotes/invite/token/v1";
const REQUEST_KEY_LABEL: &[u8] = b"easynotes/invite/request-key/v1";
const REQUEST_DOMAIN: &[u8] = b"easynotes/invite/request/v1";
const GRANT_DOMAIN: &[u8] = b"easynotes/invite/grant/v1";
const CODE_DOMAIN: &[u8] = b"easynotes/invite/code/v1";
const OWNED_GRANT_VERSION: u8 = 2;
const ROTATING_GRANT_VERSION: u8 = 3;

fn b64(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

fn sha256_b64(bytes: &[u8]) -> String {
    b64(&Sha256::digest(bytes))
}

/// The secret a link carries.
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct InviteSecret([u8; SECRET_LEN]);

impl InviteSecret {
    pub fn random() -> Self {
        let mut s = [0u8; SECRET_LEN];
        random_bytes(&mut s);
        Self(s)
    }

    /// Read a link, or the code after its `#`, or a code on its own.
    pub fn from_link(link: &str) -> Result<Self> {
        let code = link.trim();
        let code = code
            .rsplit_once('#')
            .map_or(code, |(_, after)| after)
            .trim();
        let mut raw = URL_SAFE_NO_PAD
            .decode(code)
            .map_err(|_| CoreError::MalformedInvite("not base64url"))?;
        if raw.len() != 1 + SECRET_LEN {
            raw.zeroize();
            return Err(CoreError::MalformedInvite("wrong length"));
        }
        if raw[0] != INVITE_VERSION {
            raw.zeroize();
            return Err(CoreError::MalformedInvite("unknown version"));
        }
        let mut secret = [0u8; SECRET_LEN];
        secret.copy_from_slice(&raw[1..]);
        raw.zeroize();
        Ok(Self(secret))
    }

    /// The code after the `#`: 44 characters.
    pub fn code(&self) -> String {
        let mut raw = Vec::with_capacity(1 + SECRET_LEN);
        raw.push(INVITE_VERSION);
        raw.extend_from_slice(&self.0);
        let code = b64(&raw);
        raw.zeroize();
        code
    }

    pub fn link(&self) -> String {
        format!("{LINK_PREFIX}{}", self.code())
    }

    fn derive<const N: usize>(&self, label: &[u8]) -> [u8; N] {
        let mut out = [0u8; N];
        Hkdf::<Sha256>::new(None, &self.0)
            .expand(label, &mut out)
            .expect("HKDF-SHA256 can always produce 32 bytes");
        out
    }

    pub fn id(&self) -> InviteId {
        InviteId::from_bytes(self.derive(ID_LABEL))
    }

    /// What a guest presents to the relay, base64url.
    pub fn token(&self) -> String {
        let mut raw: [u8; 32] = self.derive(TOKEN_LABEL);
        let token = b64(&raw);
        raw.zeroize();
        token
    }

    /// What the relay keeps in place of the token.
    pub fn token_hash(&self) -> String {
        let mut raw: [u8; 32] = self.derive(TOKEN_LABEL);
        let hash = sha256_b64(&raw);
        raw.zeroize();
        hash
    }

    fn request_key(&self) -> [u8; 32] {
        self.derive(REQUEST_KEY_LABEL)
    }

    fn request_cipher(&self) -> XChaCha20Poly1305 {
        let mut key = self.request_key();
        let cipher = XChaCha20Poly1305::new((&key).into());
        key.zeroize();
        cipher
    }

    fn aad(&self, domain: &[u8]) -> Vec<u8> {
        local_aad(domain, &[self.id().as_bytes()])
    }
}

impl std::fmt::Debug for InviteSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("InviteSecret(redacted)")
    }
}

/// What lets an owner read and answer an invite's requests. Random, and never in
/// the link.
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct OwnerSecret([u8; SECRET_LEN]);

impl OwnerSecret {
    pub fn random() -> Self {
        let mut s = [0u8; SECRET_LEN];
        random_bytes(&mut s);
        Self(s)
    }

    pub fn encode(&self) -> String {
        b64(&self.0)
    }

    pub fn decode(raw: &str) -> Result<Self> {
        let bytes = URL_SAFE_NO_PAD
            .decode(raw)
            .map_err(|_| CoreError::MalformedInvite("owner secret is not base64url"))?;
        let secret: [u8; SECRET_LEN] = bytes
            .try_into()
            .map_err(|_| CoreError::MalformedInvite("owner secret is not 32 bytes"))?;
        Ok(Self(secret))
    }

    /// What the relay keeps in place of it.
    pub fn hash(&self) -> String {
        sha256_b64(&self.0)
    }
}

impl std::fmt::Debug for OwnerSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OwnerSecret(redacted)")
    }
}

/// The key a guest's request asks to be answered under. One per request.
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct GuestKey([u8; SECRET_LEN]);

impl GuestKey {
    pub fn random() -> Self {
        let mut s = [0u8; SECRET_LEN];
        random_bytes(&mut s);
        Self(s)
    }

    pub fn encode(&self) -> String {
        b64(&self.0)
    }

    pub fn decode(raw: &str) -> Result<Self> {
        let bytes = URL_SAFE_NO_PAD
            .decode(raw)
            .map_err(|_| CoreError::MalformedInvite("guest key is not base64url"))?;
        let secret: [u8; SECRET_LEN] = bytes
            .try_into()
            .map_err(|_| CoreError::MalformedInvite("guest key is not 32 bytes"))?;
        Ok(Self(secret))
    }

    fn secret(&self) -> x25519_dalek::StaticSecret {
        x25519_dalek::StaticSecret::from(self.0)
    }

    pub fn public(&self) -> [u8; PUBLIC_LEN] {
        x25519_dalek::PublicKey::from(&self.secret()).to_bytes()
    }
}

impl std::fmt::Debug for GuestKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("GuestKey(redacted)")
    }
}

/// What a guest says when it asks to join, as the owner reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JoinRequest {
    /// The public half of the key the grant is to be sealed to.
    pub guest: [u8; PUBLIC_LEN],
    /// Whatever the guest typed. Not verified by anything — it is a person's
    /// word, and the safety code is what a careful owner checks instead.
    pub name: String,
}

impl JoinRequest {
    /// The six digits both screens show for this request.
    pub fn code(&self, secret: &InviteSecret) -> String {
        safety_code(secret, &self.guest)
    }
}

/// `name` cut to [`MAX_NAME_BYTES`] on a character boundary, and trimmed.
fn clip_name(name: &str) -> &str {
    let name = name.trim();
    if name.len() <= MAX_NAME_BYTES {
        return name;
    }
    let mut end = MAX_NAME_BYTES;
    while !name.is_char_boundary(end) {
        end -= 1;
    }
    &name[..end]
}

/// Seal a request for the owner: the guest's public key and a name.
///
/// `version | guest public key | name`, under the request key, bound to the
/// invite's ID so a request cannot be moved to another invite.
pub fn seal_request(secret: &InviteSecret, guest: &GuestKey, name: &str) -> Result<Vec<u8>> {
    let name = clip_name(name);
    let mut plain = Vec::with_capacity(1 + PUBLIC_LEN + name.len());
    plain.push(INVITE_VERSION);
    plain.extend_from_slice(&guest.public());
    plain.extend_from_slice(name.as_bytes());
    seal_blob(
        &secret.request_cipher(),
        &secret.aad(REQUEST_DOMAIN),
        &plain,
    )
}

pub fn open_request(secret: &InviteSecret, sealed: &[u8]) -> Result<JoinRequest> {
    let plain = open_blob(
        &secret.request_cipher(),
        &secret.aad(REQUEST_DOMAIN),
        sealed,
    )?;
    if plain.len() < 1 + PUBLIC_LEN || plain[0] != INVITE_VERSION {
        return Err(CoreError::MalformedInvite("request has an unknown shape"));
    }
    let guest: [u8; PUBLIC_LEN] = plain[1..1 + PUBLIC_LEN].try_into().unwrap();
    let name = String::from_utf8(plain[1 + PUBLIC_LEN..].to_vec())
        .map_err(|_| CoreError::MalformedInvite("name is not UTF-8"))?;
    if name.len() > MAX_NAME_BYTES {
        return Err(CoreError::MalformedInvite("name is too long"));
    }
    Ok(JoinRequest { guest, name })
}

/// The key a grant is sealed under, from one side of the agreement or the other.
fn grant_cipher(
    secret: &InviteSecret,
    shared: &x25519_dalek::SharedSecret,
    ephemeral: &[u8; PUBLIC_LEN],
    guest: &[u8; PUBLIC_LEN],
) -> Result<XChaCha20Poly1305> {
    // A guest key of low order agrees on all zeros with anybody, which would
    // make the grant's key a function of the link alone.
    if !shared.was_contributory() {
        return Err(CoreError::Crypto("the guest's key is not usable"));
    }
    let mut salt = secret.request_key();
    let mut info = Vec::with_capacity(GRANT_DOMAIN.len() + 2 * PUBLIC_LEN);
    info.extend_from_slice(GRANT_DOMAIN);
    info.extend_from_slice(ephemeral);
    info.extend_from_slice(guest);
    let mut key = [0u8; 32];
    Hkdf::<Sha256>::new(Some(&salt), shared.as_bytes())
        .expand(&info, &mut key)
        .expect("HKDF-SHA256 can always produce 32 bytes");
    let cipher = XChaCha20Poly1305::new((&key).into());
    key.zeroize();
    salt.zeroize();
    Ok(cipher)
}

/// Seal a note's ID and key for the guest who made `request`.
///
/// `ephemeral public key | sealed(version | note ID | note key)`.
pub fn seal_grant(
    secret: &InviteSecret,
    request: &JoinRequest,
    note: NoteId,
    key: &NoteKey,
) -> Result<Vec<u8>> {
    // The same construction as a guest key, and for the same reason: the
    // bytes come from the one RNG everything here draws from.
    let ephemeral = GuestKey::random().secret();
    let ephemeral_public = x25519_dalek::PublicKey::from(&ephemeral).to_bytes();
    let shared = ephemeral.diffie_hellman(&x25519_dalek::PublicKey::from(request.guest));
    let cipher = grant_cipher(secret, &shared, &ephemeral_public, &request.guest)?;

    let mut plain = Vec::with_capacity(1 + 16 + NOTE_KEY_LEN);
    plain.push(INVITE_VERSION);
    plain.extend_from_slice(note.as_bytes());
    plain.extend_from_slice(&key.0);
    let sealed = seal_blob(&cipher, &secret.aad(GRANT_DOMAIN), &plain);
    plain.zeroize();

    let mut out = Vec::with_capacity(PUBLIC_LEN + 128);
    out.extend_from_slice(&ephemeral_public);
    out.extend_from_slice(&sealed?);
    Ok(out)
}

/// The current grant also pins the note creator. A guest needs that identity to
/// distinguish a real key rotation from a former collaborator trying to evict
/// everybody else.
pub fn seal_owned_grant(
    secret: &InviteSecret,
    request: &JoinRequest,
    note: NoteId,
    key: &NoteKey,
    owner: AuthorKey,
    generation: u64,
) -> Result<Vec<u8>> {
    let ephemeral = GuestKey::random().secret();
    let ephemeral_public = x25519_dalek::PublicKey::from(&ephemeral).to_bytes();
    let shared = ephemeral.diffie_hellman(&x25519_dalek::PublicKey::from(request.guest));
    let cipher = grant_cipher(secret, &shared, &ephemeral_public, &request.guest)?;

    let mut plain = Vec::with_capacity(1 + 16 + NOTE_KEY_LEN + PUBKEY_LEN + 8);
    plain.push(ROTATING_GRANT_VERSION);
    plain.extend_from_slice(note.as_bytes());
    plain.extend_from_slice(&key.0);
    plain.extend_from_slice(&owner.0);
    plain.extend_from_slice(&generation.to_be_bytes());
    let sealed = seal_blob(&cipher, &secret.aad(GRANT_DOMAIN), &plain);
    plain.zeroize();

    let mut out = Vec::with_capacity(PUBLIC_LEN + 160);
    out.extend_from_slice(&ephemeral_public);
    out.extend_from_slice(&sealed?);
    Ok(out)
}

/// Open a grant with the key the request was made under.
pub fn open_grant(
    secret: &InviteSecret,
    guest: &GuestKey,
    sealed: &[u8],
) -> Result<(NoteId, NoteKey)> {
    open_owned_grant(secret, guest, sealed).map(|(note, key, _, _)| (note, key))
}

pub fn open_owned_grant(
    secret: &InviteSecret,
    guest: &GuestKey,
    sealed: &[u8],
) -> Result<(NoteId, NoteKey, Option<AuthorKey>, u64)> {
    if sealed.len() < PUBLIC_LEN {
        return Err(CoreError::MalformedInvite("grant is too short"));
    }
    let ephemeral: [u8; PUBLIC_LEN] = sealed[..PUBLIC_LEN].try_into().unwrap();
    let shared = guest
        .secret()
        .diffie_hellman(&x25519_dalek::PublicKey::from(ephemeral));
    let cipher = grant_cipher(secret, &shared, &ephemeral, &guest.public())?;
    let mut plain = open_blob(&cipher, &secret.aad(GRANT_DOMAIN), &sealed[PUBLIC_LEN..])?;
    let legacy_len = 1 + 16 + NOTE_KEY_LEN;
    let owned_len = legacy_len + PUBKEY_LEN;
    let rotating_len = owned_len + 8;
    if (plain.len() != legacy_len && plain.len() != owned_len && plain.len() != rotating_len)
        || (plain[0] != INVITE_VERSION
            && plain[0] != OWNED_GRANT_VERSION
            && plain[0] != ROTATING_GRANT_VERSION)
    {
        plain.zeroize();
        return Err(CoreError::MalformedInvite("grant has an unknown shape"));
    }
    let note = NoteId::from_bytes(plain[1..17].try_into().unwrap());
    let mut key = [0u8; NOTE_KEY_LEN];
    key.copy_from_slice(&plain[17..17 + NOTE_KEY_LEN]);
    let owner = (plain[0] == OWNED_GRANT_VERSION || plain[0] == ROTATING_GRANT_VERSION)
        .then(|| AuthorKey(plain[17 + NOTE_KEY_LEN..owned_len].try_into().unwrap()));
    let generation = if plain[0] == ROTATING_GRANT_VERSION && plain.len() == rotating_len {
        u64::from_be_bytes(plain[owned_len..].try_into().unwrap())
    } else {
        0
    };
    plain.zeroize();
    Ok((note, NoteKey(key), owner, generation))
}

/// Six digits, as `123 456`, from the invite and a guest's public key.
pub fn safety_code(secret: &InviteSecret, guest: &[u8; PUBLIC_LEN]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(CODE_DOMAIN);
    hasher.update(secret.id().as_bytes());
    hasher.update(guest);
    let digest = hasher.finalize();
    let n = u32::from_be_bytes(digest[..4].try_into().unwrap()) % 1_000_000;
    format!("{:03} {:03}", n / 1000, n % 1000)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_link_reads_back_as_the_same_secret() {
        let secret = InviteSecret::random();
        let link = secret.link();
        assert!(link.starts_with(LINK_PREFIX));
        for form in [
            link.clone(),
            secret.code(),
            format!("  https://example.test/join#{}\n", secret.code()),
        ] {
            let back = InviteSecret::from_link(&form).unwrap();
            assert_eq!(back.id(), secret.id(), "{form:?}");
            assert_eq!(back.token(), secret.token());
        }
    }

    #[test]
    fn a_link_is_not_a_share_blob_and_says_so() {
        let blob = crate::crypto::export_share_blob(&NoteId::random(), &NoteKey::random());
        assert!(InviteSecret::from_link(&blob).is_err());
        assert!(InviteSecret::from_link("").is_err());
        assert!(InviteSecret::from_link("easynotes://join#not-a-code!").is_err());
    }

    #[test]
    fn the_derived_values_are_distinct() {
        let secret = InviteSecret::random();
        let token = URL_SAFE_NO_PAD.decode(secret.token()).unwrap();
        assert_ne!(&token[..16], secret.id().as_bytes());
        assert_ne!(token, secret.request_key());
        assert_ne!(secret.token_hash(), secret.token());
    }

    #[test]
    fn a_request_opens_for_the_link_and_not_for_another() {
        let secret = InviteSecret::random();
        let guest = GuestKey::random();
        let sealed = seal_request(&secret, &guest, "  Anna  ").unwrap();
        let request = open_request(&secret, &sealed).unwrap();
        assert_eq!(request.name, "Anna");
        assert_eq!(request.guest, guest.public());
        assert!(open_request(&InviteSecret::random(), &sealed).is_err());
    }

    #[test]
    fn a_long_name_is_cut_on_a_character_boundary() {
        let secret = InviteSecret::random();
        let name = "ü".repeat(MAX_NAME_BYTES);
        let sealed = seal_request(&secret, &GuestKey::random(), &name).unwrap();
        let request = open_request(&secret, &sealed).unwrap();
        assert!(request.name.len() <= MAX_NAME_BYTES);
        assert!(request.name.chars().all(|c| c == 'ü'));
    }

    #[test]
    fn a_grant_opens_for_the_guest_it_was_made_for_and_nobody_else() {
        let secret = InviteSecret::random();
        let guest = GuestKey::random();
        let request = open_request(&secret, &seal_request(&secret, &guest, "").unwrap()).unwrap();
        let (note, key) = (NoteId::random(), NoteKey::random());
        let grant = seal_grant(&secret, &request, note, &key).unwrap();

        let (got_note, got_key) = open_grant(&secret, &guest, &grant).unwrap();
        assert_eq!(got_note, note);
        assert_eq!(got_key.0, key.0);

        // Another guest on the same link cannot read it.
        assert!(open_grant(&secret, &GuestKey::random(), &grant).is_err());
        // Nor can the right guest under a different link: the link is in the
        // key, so nothing that lacked it could have made this.
        assert!(open_grant(&InviteSecret::random(), &guest, &grant).is_err());
    }

    #[test]
    fn a_tampered_grant_does_not_open() {
        let secret = InviteSecret::random();
        let guest = GuestKey::random();
        let request = open_request(&secret, &seal_request(&secret, &guest, "").unwrap()).unwrap();
        let mut grant =
            seal_grant(&secret, &request, NoteId::random(), &NoteKey::random()).unwrap();
        let last = grant.len() - 1;
        grant[last] ^= 1;
        assert!(open_grant(&secret, &guest, &grant).is_err());
    }

    #[test]
    fn a_low_order_guest_key_is_refused() {
        let secret = InviteSecret::random();
        let request = JoinRequest {
            guest: [0u8; 32],
            name: String::new(),
        };
        assert!(seal_grant(&secret, &request, NoteId::random(), &NoteKey::random()).is_err());
    }

    #[test]
    fn both_sides_compute_the_same_safety_code() {
        let secret = InviteSecret::random();
        let guest = GuestKey::random();
        let request = open_request(&secret, &seal_request(&secret, &guest, "").unwrap()).unwrap();
        let code = request.code(&secret);
        assert_eq!(code, safety_code(&secret, &guest.public()));
        assert_eq!(code.len(), 7);
        assert_ne!(code, safety_code(&secret, &GuestKey::random().public()));
    }
}
