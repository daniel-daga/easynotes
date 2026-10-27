//! Invite links: how a note reaches another *person*.
//!
//! A share blob put the note key in the link, which made the link the key: it
//! never expired, nobody could withdraw it, and anyone it was forwarded to was
//! a member. An invite link carries a secret instead: the first person to open
//! it claims it, and the owner's device lets them in. See `docs/decisions.md#adr-0015` and
//! `docs/decisions.md#adr-0023` for the decisions; this module is the
//! cryptography, and nothing here talks to a relay or touches a disk.
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
//!   relay carries a request without reading who is asking.
//!
//! The owner has one more secret, random and never in the link, whose hash the
//! relay keeps: presenting it is what lets the owner read the request and
//! answer it. A guest holding the link cannot answer itself; it can only
//! claim the link, and the owner's device does the rest without asking anyone.
//!
//! ## The request and the answer
//!
//! A request names a *person*: their account's member ID and box key, and the
//! name they chose, signed by their member key over the invite ID so that it
//! cannot be replayed onto another link. An invite takes one request, and the
//! request's name on the relay is derived from the link and the member ID, so
//! the same person asking twice — a retry, or their second phone — finds the
//! request they already made, and anybody else finds the link used. The answer tells that person which
//! note they were let into and which owner key signs its access packages,
//! sealed to their box key with the request key as the salt — so it opens on
//! any of their phones, not for someone else holding the same link, and not
//! for the relay. It carries no note key: the note's current access package
//! is where every person on the note gets that.

use crate::account::MemberIdentity;
use crate::crypto::{
    local_aad, open_blob, random_bytes, seal_blob, AuthorKey, NoteKey, PUBKEY_LEN, SIG_LEN,
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
pub const MAX_NAME_BYTES: usize = crate::access::MAX_NAME_BYTES;

const SECRET_LEN: usize = 32;
const PUBLIC_LEN: usize = 32;

const ID_LABEL: &[u8] = b"easynotes/invite/id/v1";
const TOKEN_LABEL: &[u8] = b"easynotes/invite/token/v1";
const REQUEST_KEY_LABEL: &[u8] = b"easynotes/invite/request-key/v1";
const REQUEST_DOMAIN: &[u8] = b"easynotes/invite/request/v1";
const ANSWER_DOMAIN: &[u8] = b"easynotes/invite/answer/v1";
const PRE_GRANT_DOMAIN: &[u8] = b"easynotes/invite/pre-grant/v1";
const REQUEST_VERSION: u8 = 4;
const ANSWER_VERSION: u8 = 5;
const PRE_GRANT_VERSION: u8 = 6;
const REQUEST_MEMBER_DOMAIN: &[u8] = b"easynotes/invite/request-member/v1";
const REQUEST_NAME_DOMAIN: &[u8] = b"easynotes/invite/request-name/v1";

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

/// What a guest says when it asks to join, as the owner reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JoinRequest {
    /// The person asking: their account's member ID.
    pub member: AuthorKey,
    /// The public half of the key the answer and every grant are sealed to.
    pub box_key: [u8; PUBLIC_LEN],
    /// What they call themselves. Not verified by anything — it is a person's
    /// word.
    pub name: String,
}

impl JoinRequest {
    /// Whether this request carries the name its person would have given it.
    /// One that does not was not made by this app, and is not let in.
    pub fn is_named(&self, secret: &InviteSecret, request: &str) -> bool {
        request_name(secret, &self.member) == request
    }
}

/// The name a person's request goes by on the relay: sixteen bytes, base64url,
/// from the link's request key and the member ID. The relay has neither the
/// key nor the member ID, so it cannot tell whose request it is, or that two
/// invites were claimed by one person.
pub fn request_name(secret: &InviteSecret, member: &AuthorKey) -> String {
    let mut key = secret.request_key();
    let mut hasher = Sha256::new();
    hasher.update(REQUEST_NAME_DOMAIN);
    hasher.update(key);
    hasher.update(member.0);
    key.zeroize();
    b64(&hasher.finalize()[..16])
}

/// Seal a request for the owner: who is asking, where to seal their answer,
/// and what they are called, signed by their member key.
///
/// `04 | member ID | box key | signature | name`, under the request key
/// ([ADR 0023](../../docs/decisions.md#adr-0023)).
pub fn seal_request(secret: &InviteSecret, member: &MemberIdentity, name: &str) -> Result<Vec<u8>> {
    let name = crate::access::clip_name(name);
    if name.is_empty() {
        return Err(CoreError::MalformedInvite("a request needs a name"));
    }
    let id = member.id();
    let box_key = member.box_public();
    let signature = member.sign(&request_message(secret, &id, &box_key, name));
    let mut plain = Vec::with_capacity(1 + 2 * PUBKEY_LEN + SIG_LEN + name.len());
    plain.push(REQUEST_VERSION);
    plain.extend_from_slice(&id.0);
    plain.extend_from_slice(&box_key);
    plain.extend_from_slice(&signature);
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
    let name_at = 1 + 2 * PUBKEY_LEN + SIG_LEN;
    if plain.first() != Some(&REQUEST_VERSION) || plain.len() <= name_at {
        return Err(CoreError::MalformedInvite("request has an unknown shape"));
    }
    let member = AuthorKey(plain[1..1 + PUBKEY_LEN].try_into().unwrap());
    let box_key: [u8; PUBLIC_LEN] = plain[1 + PUBKEY_LEN..1 + 2 * PUBKEY_LEN]
        .try_into()
        .unwrap();
    let signature: [u8; SIG_LEN] = plain[1 + 2 * PUBKEY_LEN..name_at].try_into().unwrap();
    let name = String::from_utf8(plain[name_at..].to_vec())
        .map_err(|_| CoreError::MalformedInvite("name is not UTF-8"))?;
    if name.len() > MAX_NAME_BYTES || name.trim().is_empty() {
        return Err(CoreError::MalformedInvite("name is empty or too long"));
    }
    if !member.verify(
        &request_message(secret, &member, &box_key, &name),
        &signature,
    ) {
        return Err(CoreError::MalformedInvite("request signature is invalid"));
    }
    Ok(JoinRequest {
        member,
        box_key,
        name,
    })
}

fn request_message(
    secret: &InviteSecret,
    member: &AuthorKey,
    box_key: &[u8; PUBLIC_LEN],
    name: &str,
) -> Vec<u8> {
    let mut message = Vec::with_capacity(REQUEST_MEMBER_DOMAIN.len() + 16 + 64 + name.len());
    message.extend_from_slice(REQUEST_MEMBER_DOMAIN);
    message.extend_from_slice(secret.id().as_bytes());
    message.extend_from_slice(&member.0);
    message.extend_from_slice(box_key);
    message.extend_from_slice(name.as_bytes());
    message
}

/// The key an answer is sealed under, from one side of the agreement or the
/// other.
fn answer_cipher(
    secret: &InviteSecret,
    shared: &x25519_dalek::SharedSecret,
    ephemeral: &[u8; PUBLIC_LEN],
    box_key: &[u8; PUBLIC_LEN],
) -> Result<XChaCha20Poly1305> {
    // A box key of low order agrees on all zeros with anybody, which would
    // make the answer's key a function of the link alone.
    if !shared.was_contributory() {
        return Err(CoreError::Crypto("the guest's key is not usable"));
    }
    let mut salt = secret.request_key();
    let mut info = Vec::with_capacity(ANSWER_DOMAIN.len() + 2 * PUBLIC_LEN);
    info.extend_from_slice(ANSWER_DOMAIN);
    info.extend_from_slice(ephemeral);
    info.extend_from_slice(box_key);
    let mut key = [0u8; 32];
    Hkdf::<Sha256>::new(Some(&salt), shared.as_bytes())
        .expand(&info, &mut key)
        .expect("HKDF-SHA256 can always produce 32 bytes");
    let cipher = XChaCha20Poly1305::new((&key).into());
    key.zeroize();
    salt.zeroize();
    Ok(cipher)
}

/// Seal the answer to an approved request: which note, and whose key signs
/// its packages. No note key: that comes from the note's current package.
///
/// `ephemeral public key | sealed(05 | note ID | owner key)`.
pub fn seal_answer(
    secret: &InviteSecret,
    request: &JoinRequest,
    note: NoteId,
    owner: AuthorKey,
) -> Result<Vec<u8>> {
    let mut raw = [0u8; 32];
    random_bytes(&mut raw);
    let ephemeral = x25519_dalek::StaticSecret::from(raw);
    raw.zeroize();
    let ephemeral_public = x25519_dalek::PublicKey::from(&ephemeral).to_bytes();
    let shared = ephemeral.diffie_hellman(&x25519_dalek::PublicKey::from(request.box_key));
    let cipher = answer_cipher(secret, &shared, &ephemeral_public, &request.box_key)?;
    let mut plain = Vec::with_capacity(1 + 16 + PUBKEY_LEN);
    plain.push(ANSWER_VERSION);
    plain.extend_from_slice(note.as_bytes());
    plain.extend_from_slice(&owner.0);
    let sealed = seal_blob(&cipher, &secret.aad(ANSWER_DOMAIN), &plain)?;
    let mut out = Vec::with_capacity(PUBLIC_LEN + sealed.len());
    out.extend_from_slice(&ephemeral_public);
    out.extend_from_slice(&sealed);
    Ok(out)
}

/// Open an answer with this person's box key.
pub fn open_answer(
    secret: &InviteSecret,
    member: &MemberIdentity,
    sealed: &[u8],
) -> Result<(NoteId, AuthorKey)> {
    let ephemeral: [u8; PUBLIC_LEN] = sealed
        .get(..PUBLIC_LEN)
        .and_then(|raw| raw.try_into().ok())
        .ok_or(CoreError::MalformedInvite("answer is too short"))?;
    let shared = member.agree(&ephemeral);
    let cipher = answer_cipher(secret, &shared, &ephemeral, &member.box_public())?;
    let plain = open_blob(&cipher, &secret.aad(ANSWER_DOMAIN), &sealed[PUBLIC_LEN..])?;
    if plain.len() != 1 + 16 + PUBKEY_LEN || plain[0] != ANSWER_VERSION {
        return Err(CoreError::MalformedInvite("answer has an unknown shape"));
    }
    Ok((
        NoteId::from_bytes(plain[1..17].try_into().unwrap()),
        AuthorKey(plain[17..].try_into().unwrap()),
    ))
}

/// Seal the note's current key for whoever claims the link
/// ([ADR 0030](../../docs/decisions.md#adr-0030)).
///
/// `06 | note ID | owner key | note key`. Under the link's request key, so the
/// relay never sees it, and bound to the invite ID in the AAD so it cannot be
/// lifted from one invite to another. It carries the owner key as well,
/// because a guest who reads it has to know what to check the access packages
/// against before one has arrived.
pub fn seal_pre_grant(
    secret: &InviteSecret,
    note: NoteId,
    owner: AuthorKey,
    key: &NoteKey,
) -> Result<Vec<u8>> {
    let mut plain = Vec::with_capacity(1 + 16 + PUBKEY_LEN + 32);
    plain.push(PRE_GRANT_VERSION);
    plain.extend_from_slice(note.as_bytes());
    plain.extend_from_slice(&owner.0);
    plain.extend_from_slice(&key.0);
    let sealed = seal_blob(
        &secret.request_cipher(),
        &secret.aad(PRE_GRANT_DOMAIN),
        &plain,
    );
    plain.zeroize();
    sealed
}

/// Open a pre-grant with the link that sealed it.
pub fn open_pre_grant(
    secret: &InviteSecret,
    sealed: &[u8],
) -> Result<(NoteId, AuthorKey, NoteKey)> {
    let mut plain = open_blob(
        &secret.request_cipher(),
        &secret.aad(PRE_GRANT_DOMAIN),
        sealed,
    )?;
    let expected = 1 + 16 + PUBKEY_LEN + 32;
    if plain.len() != expected || plain[0] != PRE_GRANT_VERSION {
        plain.zeroize();
        return Err(CoreError::MalformedInvite("pre-grant has an unknown shape"));
    }
    let note = NoteId::from_bytes(plain[1..17].try_into().unwrap());
    let owner = AuthorKey(plain[17..17 + PUBKEY_LEN].try_into().unwrap());
    let key = NoteKey(plain[17 + PUBKEY_LEN..].try_into().unwrap());
    plain.zeroize();
    Ok((note, owner, key))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::RecoveryPhrase;
    use crate::crypto::NoteKey;

    fn member() -> MemberIdentity {
        RecoveryPhrase::generate().root().member()
    }

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
        let anna = member();
        let sealed = seal_request(&secret, &anna, "  Anna  ").unwrap();
        let request = open_request(&secret, &sealed).unwrap();
        assert_eq!(request.name, "Anna");
        assert_eq!(request.member, anna.id());
        assert_eq!(request.box_key, anna.box_public());
        assert!(open_request(&InviteSecret::random(), &sealed).is_err());
    }

    #[test]
    fn a_request_needs_a_name() {
        assert!(seal_request(&InviteSecret::random(), &member(), "   ").is_err());
    }

    #[test]
    fn a_long_name_is_cut_on_a_character_boundary() {
        let secret = InviteSecret::random();
        let name = "ü".repeat(MAX_NAME_BYTES);
        let sealed = seal_request(&secret, &member(), &name).unwrap();
        let request = open_request(&secret, &sealed).unwrap();
        assert!(request.name.len() <= MAX_NAME_BYTES);
        assert!(request.name.chars().all(|c| c == 'ü'));
        assert!(sealed.len() <= 250, "{}", sealed.len());
    }

    #[test]
    fn a_request_signed_for_another_invite_is_refused() {
        // The same member key, box key and name, signed over a different
        // invite ID: the signature is what binds a request to this link.
        let secret = InviteSecret::random();
        let other = InviteSecret::random();
        let anna = member();
        let id = anna.id();
        let box_key = anna.box_public();
        let signature = anna.sign(&request_message(&other, &id, &box_key, "Anna"));
        let mut plain = vec![REQUEST_VERSION];
        plain.extend_from_slice(&id.0);
        plain.extend_from_slice(&box_key);
        plain.extend_from_slice(&signature);
        plain.extend_from_slice(b"Anna");
        let sealed = seal_blob(
            &secret.request_cipher(),
            &secret.aad(REQUEST_DOMAIN),
            &plain,
        )
        .unwrap();
        assert!(open_request(&secret, &sealed).is_err());
    }

    #[test]
    fn an_answer_opens_for_the_person_it_was_made_for_and_nobody_else() {
        let secret = InviteSecret::random();
        let anna = member();
        let request =
            open_request(&secret, &seal_request(&secret, &anna, "Anna").unwrap()).unwrap();
        let note = NoteId::random();
        let owner = crate::crypto::DeviceIdentity::generate().public_key();
        let answer = seal_answer(&secret, &request, note, owner).unwrap();
        assert_eq!(open_answer(&secret, &anna, &answer).unwrap(), (note, owner));
        assert!(open_answer(&secret, &member(), &answer).is_err());
        assert!(open_answer(&InviteSecret::random(), &anna, &answer).is_err());
        let mut tampered = answer.clone();
        let last = tampered.len() - 1;
        tampered[last] ^= 1;
        assert!(open_answer(&secret, &anna, &tampered).is_err());
    }

    #[test]
    fn a_low_order_box_key_is_refused() {
        let secret = InviteSecret::random();
        let request = JoinRequest {
            member: member().id(),
            box_key: [0u8; 32],
            name: "x".into(),
        };
        assert!(seal_answer(&secret, &request, NoteId::random(), member().id()).is_err());
    }

    #[test]
    fn a_pre_grant_opens_only_with_the_link_that_sealed_it() {
        let secret = InviteSecret::random();
        let note = NoteId::random();
        let owner = crate::crypto::DeviceIdentity::generate().public_key();
        let key = NoteKey::random();
        let sealed = seal_pre_grant(&secret, note, owner, &key).unwrap();

        let (opened_note, opened_owner, opened_key) = open_pre_grant(&secret, &sealed).unwrap();
        assert_eq!(opened_note, note);
        assert_eq!(opened_owner, owner);
        assert_eq!(opened_key.id(), key.id());
        assert!(open_pre_grant(&InviteSecret::random(), &sealed).is_err());

        let mut tampered = sealed.clone();
        let last = tampered.len() - 1;
        tampered[last] ^= 1;
        assert!(open_pre_grant(&secret, &tampered).is_err());
    }

    /// A pre-grant is tied to one invite: the AAD carries the invite ID, so a
    /// grant lifted from another link does not open even with the same secret.
    #[test]
    fn a_pre_grant_cannot_be_lifted_to_another_invite() {
        let secret = InviteSecret::random();
        let other = InviteSecret::random();
        let sealed =
            seal_pre_grant(&secret, NoteId::random(), member().id(), &NoteKey::random()).unwrap();
        // Sealed under the first link's key at all: the second cannot open it.
        assert!(open_pre_grant(&other, &sealed).is_err());
    }

    #[test]
    fn every_phone_of_one_person_names_its_request_alike() {
        let secret = InviteSecret::random();
        let phrase = RecoveryPhrase::generate();
        let phone = phrase.root().member();
        let other_phone = RecoveryPhrase::parse(&phrase.words())
            .unwrap()
            .root()
            .member();
        let name = request_name(&secret, &phone.id());
        assert_eq!(name, request_name(&secret, &other_phone.id()));
        assert_eq!(URL_SAFE_NO_PAD.decode(&name).unwrap().len(), 16);
        // Somebody else, or the same person on another link, is another name.
        assert_ne!(name, request_name(&secret, &member().id()));
        assert_ne!(name, request_name(&InviteSecret::random(), &phone.id()));

        let request = open_request(&secret, &seal_request(&secret, &phone, "A").unwrap()).unwrap();
        assert!(request.is_named(&secret, &name));
        assert!(!request.is_named(&secret, &request_name(&secret, &member().id())));
    }
}
