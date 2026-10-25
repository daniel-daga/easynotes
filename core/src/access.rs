//! Access packages: who can read a shared note, and the key they read it with.
//!
//! One envelope of kind [`EnvelopeKind::Access`] on the note's own route,
//! signed by the note's owner key. It carries the whole list of people, the
//! change it makes to that list, a grant of the note key for every person on
//! it, and — when it removes anyone — a fresh key and a snapshot sealed under
//! it. See [ADR 0023](../../docs/adr/0023-access-packages.md) for the
//! decisions; this module is the format and the rule for which package is
//! current, and nothing here touches a store or a relay.
//!
//! ## Layout of the ciphertext field
//!
//! ```text
//! 0      1        format (01)
//! 1      8        revision (= the envelope counter)
//! 9      32       parent digest (zeros at revision 1)
//! 41     32       compaction key for this package's note key
//! 73     32 × 80  grant slots
//! 2633   …        body, sealed under the note key with the prefix in its AAD
//! ```
//!
//! The prefix is signed and not hidden, so any device — including one that has
//! just been removed and holds no key that opens anything — reaches the same
//! answer about which package is current. So does the relay, which reads the
//! compaction key out of the current package to decide who may compact the
//! note's log ([ADR 0024](../../docs/adr/0024-relay-storage-quota.md)).

use crate::account::MemberIdentity;
use crate::crypto::{random_bytes, AuthorKey, DeviceIdentity, Envelope, EnvelopeKind, NoteKey};
use crate::error::{CoreError, Result};
use crate::ids::{InviteId, NoteId};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use hkdf::Hkdf;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use zeroize::Zeroize;

/// People on one note, the owner included. The number of grant slots.
pub const MAX_MEMBERS: usize = 32;
/// The longest name a package records, in bytes.
pub const MAX_NAME_BYTES: usize = 64;

const FORMAT: u8 = 1;
const SLOT_LEN: usize = 32 + 32 + 16;
/// Bytes of the ciphertext field that are signed and not encrypted.
pub const PREFIX_LEN: usize = SLOTS_AT + MAX_MEMBERS * SLOT_LEN;
/// Where the grant slots start in the prefix.
const SLOTS_AT: usize = 1 + 8 + 32 + 32;
const RECORD_LEN: usize = 32 + 32 + 1 + 16 + 1 + MAX_NAME_BYTES;
const MEMBERSHIP_LEN: usize = 32 + 16;
const BODY_FIXED_LEN: usize =
    1 + MAX_MEMBERS * RECORD_LEN + 2 * (1 + MAX_MEMBERS * MEMBERSHIP_LEN) + 4;

const GRANT_DOMAIN: &[u8] = b"easynotes/access/grant/v1";
const DIGEST_DOMAIN: &[u8] = b"easynotes/access/digest/v1";

/// A package's identity: SHA-256 over its whole envelope.
pub type Digest = [u8; 32];

/// The invite ID the owner's own entry carries: the owner was never invited.
pub const OWNER_INVITE: InviteId = InviteId([0u8; 16]);

/// One person's admission to a note. A person let in twice — removed and
/// invited again — is two memberships, so removing the first can never cancel
/// the second.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Membership {
    pub member: AuthorKey,
    pub invite: InviteId,
}

/// One person on a package's list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Member {
    pub member: AuthorKey,
    pub box_key: [u8; 32],
    pub owner: bool,
    pub invite: InviteId,
    /// The name they joined under, chosen by the person: a label, not a
    /// verified identity. A record in the note's document may be newer — a
    /// rename travels there, signed by the person it belongs to
    /// ([ADR 0032](../../docs/adr/0032-a-name-that-follows-the-person.md)).
    pub name: String,
}

impl Member {
    pub fn membership(&self) -> Membership {
        Membership {
            member: self.member,
            invite: self.invite,
        }
    }
}

/// What a package says, once its body is open.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Body {
    pub list: Vec<Member>,
    pub added: Vec<Membership>,
    pub removed: Vec<Membership>,
    /// The whole document under the package's key. Present exactly when the
    /// package changes the key.
    pub snapshot: Option<Vec<u8>>,
}

/// `name` trimmed and cut to [`MAX_NAME_BYTES`] on a character boundary.
pub fn clip_name(name: &str) -> &str {
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

fn put_membership(out: &mut Vec<u8>, m: &Membership) {
    out.extend_from_slice(&m.member.0);
    out.extend_from_slice(m.invite.as_bytes());
}

fn take<'a>(raw: &'a [u8], at: &mut usize, n: usize) -> Result<&'a [u8]> {
    let slice = raw
        .get(*at..*at + n)
        .ok_or(CoreError::MalformedEnvelope("access body is truncated"))?;
    *at += n;
    Ok(slice)
}

fn membership_at(raw: &[u8]) -> Membership {
    Membership {
        member: AuthorKey(raw[..32].try_into().unwrap()),
        invite: InviteId::from_bytes(raw[32..48].try_into().unwrap()),
    }
}

impl Body {
    /// Fixed-width everywhere but the snapshot, so a package's size says
    /// nothing about how many people are on it.
    pub fn encode(&self) -> Result<Vec<u8>> {
        if self.list.len() > MAX_MEMBERS
            || self.added.len() > MAX_MEMBERS
            || self.removed.len() > MAX_MEMBERS
        {
            return Err(CoreError::Crypto("a note has at most 32 people"));
        }
        let snapshot = self.snapshot.as_deref().unwrap_or_default();
        let mut out = Vec::with_capacity(BODY_FIXED_LEN + snapshot.len());
        out.push(self.list.len() as u8);
        for i in 0..MAX_MEMBERS {
            match self.list.get(i) {
                Some(m) => {
                    let name = clip_name(&m.name);
                    out.extend_from_slice(&m.member.0);
                    out.extend_from_slice(&m.box_key);
                    out.push(u8::from(m.owner));
                    out.extend_from_slice(m.invite.as_bytes());
                    out.push(name.len() as u8);
                    out.extend_from_slice(name.as_bytes());
                    out.resize(out.len() + MAX_NAME_BYTES - name.len(), 0);
                }
                None => out.resize(out.len() + RECORD_LEN, 0),
            }
        }
        for set in [&self.added, &self.removed] {
            out.push(set.len() as u8);
            for i in 0..MAX_MEMBERS {
                match set.get(i) {
                    Some(m) => put_membership(&mut out, m),
                    None => out.resize(out.len() + MEMBERSHIP_LEN, 0),
                }
            }
        }
        let length = u32::try_from(snapshot.len()).map_err(|_| CoreError::TooLarge {
            limit: u32::MAX as usize,
        })?;
        out.extend_from_slice(&length.to_be_bytes());
        out.extend_from_slice(snapshot);
        Ok(out)
    }

    pub fn decode(raw: &[u8]) -> Result<Self> {
        let mut at = 0;
        let count = take(raw, &mut at, 1)?[0] as usize;
        if count > MAX_MEMBERS {
            return Err(CoreError::MalformedEnvelope("access list is too long"));
        }
        let mut list = Vec::with_capacity(count);
        for i in 0..MAX_MEMBERS {
            let record = take(raw, &mut at, RECORD_LEN)?;
            if i >= count {
                continue;
            }
            let name_len = record[81] as usize;
            if name_len > MAX_NAME_BYTES {
                return Err(CoreError::MalformedEnvelope("access name is too long"));
            }
            let name = std::str::from_utf8(&record[82..82 + name_len])
                .map_err(|_| CoreError::MalformedEnvelope("access name is not UTF-8"))?;
            list.push(Member {
                member: AuthorKey(record[..32].try_into().unwrap()),
                box_key: record[32..64].try_into().unwrap(),
                owner: match record[64] {
                    0 => false,
                    1 => true,
                    _ => return Err(CoreError::MalformedEnvelope("unknown access role")),
                },
                invite: InviteId::from_bytes(record[65..81].try_into().unwrap()),
                name: name.to_string(),
            });
        }
        let mut sets = [Vec::new(), Vec::new()];
        for set in &mut sets {
            let count = take(raw, &mut at, 1)?[0] as usize;
            if count > MAX_MEMBERS {
                return Err(CoreError::MalformedEnvelope("access change is too long"));
            }
            for i in 0..MAX_MEMBERS {
                let entry = take(raw, &mut at, MEMBERSHIP_LEN)?;
                if i < count {
                    set.push(membership_at(entry));
                }
            }
        }
        let length = u32::from_be_bytes(take(raw, &mut at, 4)?.try_into().unwrap()) as usize;
        let snapshot = take(raw, &mut at, length)?;
        if at != raw.len() {
            return Err(CoreError::MalformedEnvelope(
                "access body has trailing bytes",
            ));
        }
        if list.iter().filter(|m| m.owner).count() != 1 {
            return Err(CoreError::MalformedEnvelope(
                "an access list names exactly one owner",
            ));
        }
        let [added, removed] = sets;
        Ok(Self {
            list,
            added,
            removed,
            snapshot: (length > 0).then(|| snapshot.to_vec()),
        })
    }

    pub fn memberships(&self) -> BTreeSet<Membership> {
        self.list.iter().map(Member::membership).collect()
    }
}

fn grant_parts(
    note: NoteId,
    revision: u64,
    parent: &Digest,
    recipient: &[u8; 32],
    ephemeral: &[u8; 32],
    shared: &x25519_dalek::SharedSecret,
) -> Option<(XChaCha20Poly1305, Vec<u8>)> {
    // A recipient key of low order agrees on zeros with everybody.
    if !shared.was_contributory() {
        return None;
    }
    let mut info = Vec::with_capacity(GRANT_DOMAIN.len() + 64);
    info.extend_from_slice(GRANT_DOMAIN);
    info.extend_from_slice(ephemeral);
    info.extend_from_slice(recipient);
    let mut key = [0u8; 32];
    Hkdf::<Sha256>::new(Some(note.as_bytes()), shared.as_bytes())
        .expand(&info, &mut key)
        .expect("HKDF-SHA256 can always produce 32 bytes");
    let cipher = XChaCha20Poly1305::new((&key).into());
    key.zeroize();
    let mut aad = Vec::with_capacity(GRANT_DOMAIN.len() + 16 + 8 + 32 + 32);
    aad.extend_from_slice(GRANT_DOMAIN);
    aad.extend_from_slice(note.as_bytes());
    aad.extend_from_slice(&revision.to_be_bytes());
    aad.extend_from_slice(parent);
    aad.extend_from_slice(recipient);
    Some((cipher, aad))
}

/// A fresh X25519 key pair, from the one RNG everything here draws from.
fn ephemeral() -> (x25519_dalek::StaticSecret, [u8; 32]) {
    let mut raw = [0u8; 32];
    random_bytes(&mut raw);
    let secret = x25519_dalek::StaticSecret::from(raw);
    raw.zeroize();
    let public = x25519_dalek::PublicKey::from(&secret).to_bytes();
    (secret, public)
}

fn seal_slot(
    note: NoteId,
    revision: u64,
    parent: &Digest,
    recipient: &[u8; 32],
    key: &NoteKey,
) -> Result<Vec<u8>> {
    let (secret, public) = ephemeral();
    let shared = secret.diffie_hellman(&x25519_dalek::PublicKey::from(*recipient));
    let (cipher, aad) = grant_parts(note, revision, parent, recipient, &public, &shared)
        .ok_or(CoreError::Crypto("a person's box key is not usable"))?;
    // A fixed nonce is safe: every slot has its own ephemeral key, so every
    // slot has its own cipher key.
    let sealed = cipher
        .encrypt(
            XNonce::from_slice(&[0u8; 24]),
            Payload {
                msg: &key.0,
                aad: &aad,
            },
        )
        .map_err(|_| CoreError::Crypto("encryption failed"))?;
    let mut slot = Vec::with_capacity(SLOT_LEN);
    slot.extend_from_slice(&public);
    slot.extend_from_slice(&sealed);
    Ok(slot)
}

/// A slot nobody can open that looks like one somebody can: a real public key,
/// so that a curve-point test cannot tell it from a grant, and random bytes.
fn filler_slot() -> Vec<u8> {
    let (_, public) = ephemeral();
    let mut slot = public.to_vec();
    let mut rest = [0u8; SLOT_LEN - 32];
    random_bytes(&mut rest);
    slot.extend_from_slice(&rest);
    slot
}

/// Seal a package. `owner` is the note's owner key, `key` the key this package
/// establishes; `body.list` gets one grant each.
pub fn build(
    owner: &DeviceIdentity,
    note: NoteId,
    revision: u64,
    parent: Option<Digest>,
    key: &NoteKey,
    body: &Body,
) -> Result<Vec<u8>> {
    if revision == 0 || (revision == 1) != parent.is_none() {
        return Err(CoreError::Crypto(
            "a package's revision and parent disagree",
        ));
    }
    let parent = parent.unwrap_or([0u8; 32]);
    let mut slots = Vec::with_capacity(MAX_MEMBERS);
    for member in &body.list {
        slots.push(seal_slot(note, revision, &parent, &member.box_key, key)?);
    }
    while slots.len() < MAX_MEMBERS {
        slots.push(filler_slot());
    }
    // Fisher–Yates on the platform RNG, so a slot's position says nothing
    // about whose it is.
    for i in (1..slots.len()).rev() {
        let mut r = [0u8; 8];
        random_bytes(&mut r);
        let j = (u64::from_be_bytes(r) % (i as u64 + 1)) as usize;
        slots.swap(i, j);
    }
    let mut prefix = Vec::with_capacity(PREFIX_LEN);
    prefix.push(FORMAT);
    prefix.extend_from_slice(&revision.to_be_bytes());
    prefix.extend_from_slice(&parent);
    prefix.extend_from_slice(&crate::compaction::public_key(note, key).0);
    for slot in slots {
        prefix.extend_from_slice(&slot);
    }
    let mut plain = body.encode()?;
    let envelope = Envelope::seal_with_clear(
        owner,
        note,
        key,
        revision,
        EnvelopeKind::Access,
        &prefix,
        &plain,
    );
    plain.zeroize();
    Ok(envelope?.encode())
}

/// SHA-256 over a package's envelope, under a domain of its own.
pub fn digest(bytes: &[u8]) -> Digest {
    let mut hasher = Sha256::new();
    hasher.update(DIGEST_DOMAIN);
    hasher.update(bytes);
    hasher.finalize().into()
}

/// A package whose signature and shape have been checked.
#[derive(Clone, Debug)]
pub struct Package {
    pub bytes: Vec<u8>,
    pub envelope: Envelope,
    pub revision: u64,
    pub parent: Option<Digest>,
    pub digest: Digest,
    /// Who may compact the note's relay log while this package is current:
    /// the compaction key of the note key it establishes.
    pub compaction: AuthorKey,
}

impl Package {
    /// Everything that can be checked without a key: the kind, the note, the
    /// owner's signature, and that the prefix agrees with the header.
    pub fn parse(bytes: &[u8], note: NoteId, owner: AuthorKey) -> Result<Self> {
        let envelope = Envelope::decode(bytes)?;
        if envelope.kind != EnvelopeKind::Access {
            return Err(CoreError::MalformedEnvelope("not an access package"));
        }
        if envelope.note_id != note {
            return Err(CoreError::WrongNote {
                expected: note.encode(),
                got: envelope.note_id.encode(),
            });
        }
        if envelope.author != owner {
            return Err(CoreError::Crypto(
                "an access package not signed by the owner",
            ));
        }
        envelope.verify()?;
        let prefix = envelope.clear(PREFIX_LEN)?;
        if prefix[0] != FORMAT {
            return Err(CoreError::MalformedEnvelope(
                "unknown access package format",
            ));
        }
        let revision = u64::from_be_bytes(prefix[1..9].try_into().unwrap());
        let parent: Digest = prefix[9..41].try_into().unwrap();
        let parent = (parent != [0u8; 32]).then_some(parent);
        let compaction = AuthorKey(prefix[41..SLOTS_AT].try_into().unwrap());
        if revision != envelope.counter || revision == 0 || (revision == 1) != parent.is_none() {
            return Err(CoreError::MalformedEnvelope(
                "an access package's revision and parent disagree",
            ));
        }
        Ok(Self {
            bytes: bytes.to_vec(),
            revision,
            parent,
            digest: digest(bytes),
            compaction,
            envelope,
        })
    }

    /// The key this package grants `member`, if it grants them one: every slot
    /// is tried, and a slot counts only if the key in it is the one the header
    /// names.
    pub fn open_grant(&self, member: &MemberIdentity) -> Option<NoteKey> {
        let prefix = self.envelope.clear(PREFIX_LEN).ok()?;
        let parent = self.parent.unwrap_or([0u8; 32]);
        let recipient = member.box_public();
        for slot in prefix[SLOTS_AT..].chunks_exact(SLOT_LEN) {
            let ephemeral: [u8; 32] = slot[..32].try_into().unwrap();
            let shared = member.agree(&ephemeral);
            let Some((cipher, aad)) = grant_parts(
                self.envelope.note_id,
                self.revision,
                &parent,
                &recipient,
                &ephemeral,
                &shared,
            ) else {
                continue;
            };
            let Ok(mut raw) = cipher.decrypt(
                XNonce::from_slice(&[0u8; 24]),
                Payload {
                    msg: &slot[32..],
                    aad: &aad,
                },
            ) else {
                continue;
            };
            let Ok(key) = <[u8; 32]>::try_from(raw.as_slice()) else {
                raw.zeroize();
                continue;
            };
            raw.zeroize();
            let key = NoteKey(key);
            if Some(key.id()) == self.envelope.key_id {
                return Some(key);
            }
        }
        None
    }

    pub fn key_id(&self) -> [u8; crate::crypto::KEY_ID_LEN] {
        self.envelope.key_id.expect("a v2 envelope has a key id")
    }

    pub fn open_body(&self, key: &NoteKey) -> Result<Body> {
        let mut plain = self.envelope.open_with_clear(key, PREFIX_LEN)?;
        let body = Body::decode(&plain);
        plain.zeroize();
        body
    }
}

fn b64(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

fn unb64<const N: usize>(raw: &str) -> Result<[u8; N]> {
    URL_SAFE_NO_PAD
        .decode(raw)
        .ok()
        .and_then(|v| v.try_into().ok())
        .ok_or_else(|| CoreError::Storage("malformed access ledger".into()))
}

/// What a device remembers about one package after its bytes may be gone.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub revision: u64,
    pub parent: Option<String>,
    pub key_id: String,
    /// Whether this person's grant opened in it.
    pub mine: bool,
    /// Whether the relay has echoed it back.
    pub on_relay: bool,
    /// Whether it carries a snapshot, and so introduces its key.
    pub snapshot: bool,
    /// The list and change, when this device could open the body. Kept by
    /// owner devices for the merge, and by everyone for the current list.
    pub list: Option<Vec<StoredMember>>,
    pub added: Vec<(String, String)>,
    pub removed: Vec<(String, String)>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredMember {
    pub member: String,
    pub box_key: String,
    pub owner: bool,
    pub invite: String,
    pub name: String,
}

fn store_membership(m: &Membership) -> (String, String) {
    (m.member.encode(), m.invite.encode())
}

fn load_membership((member, invite): &(String, String)) -> Result<Membership> {
    Ok(Membership {
        member: AuthorKey(unb64(member)?),
        invite: InviteId::decode(invite)?,
    })
}

impl Entry {
    pub fn of(package: &Package, mine: bool, body: Option<&Body>) -> Self {
        Self {
            revision: package.revision,
            parent: package.parent.map(|p| b64(&p)),
            key_id: b64(&package.key_id()),
            mine,
            on_relay: false,
            snapshot: body.is_some_and(|b| b.snapshot.is_some()),
            list: body.map(|b| {
                b.list
                    .iter()
                    .map(|m| StoredMember {
                        member: m.member.encode(),
                        box_key: b64(&m.box_key),
                        owner: m.owner,
                        invite: m.invite.encode(),
                        name: m.name.clone(),
                    })
                    .collect()
            }),
            added: body
                .map(|b| b.added.iter().map(store_membership).collect())
                .unwrap_or_default(),
            removed: body
                .map(|b| b.removed.iter().map(store_membership).collect())
                .unwrap_or_default(),
        }
    }

    pub fn members(&self) -> Result<Option<Vec<Member>>> {
        let Some(list) = &self.list else {
            return Ok(None);
        };
        list.iter()
            .map(|m| {
                Ok(Member {
                    member: AuthorKey(unb64(&m.member)?),
                    box_key: unb64(&m.box_key)?,
                    owner: m.owner,
                    invite: InviteId::decode(&m.invite)?,
                    name: m.name.clone(),
                })
            })
            .collect::<Result<Vec<_>>>()
            .map(Some)
    }

    pub fn added(&self) -> Result<Vec<Membership>> {
        self.added.iter().map(load_membership).collect()
    }

    pub fn removed(&self) -> Result<Vec<Membership>> {
        self.removed.iter().map(load_membership).collect()
    }

    pub fn key_id(&self) -> Result<[u8; crate::crypto::KEY_ID_LEN]> {
        unb64(&self.key_id)
    }
}

/// Every package one device has seen for one note, and every key it has held.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ledger {
    /// By digest, base64url.
    pub packages: BTreeMap<String, Entry>,
    /// Key IDs this device has used for the note and moved on from. An
    /// envelope under one of these is history; under any other unknown key it
    /// may be the future, and is kept.
    pub held: BTreeSet<String>,
}

impl Ledger {
    pub fn decode(raw: &[u8]) -> Result<Self> {
        serde_json::from_slice(raw).map_err(|e| CoreError::Storage(format!("access ledger: {e}")))
    }

    pub fn encode(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("a ledger serializes")
    }

    pub fn get(&self, digest: &Digest) -> Option<&Entry> {
        self.packages.get(&b64(digest))
    }

    pub fn get_mut(&mut self, digest: &Digest) -> Option<&mut Entry> {
        self.packages.get_mut(&b64(digest))
    }

    pub fn insert(&mut self, digest: &Digest, entry: Entry) {
        self.packages.insert(b64(digest), entry);
    }

    pub fn contains(&self, digest: &Digest) -> bool {
        self.packages.contains_key(&b64(digest))
    }

    /// Whether a package counts: revision 1 with no parent, or a counting
    /// parent one revision below it. Iterative, so a long chain cannot
    /// overflow the stack.
    pub fn counts(&self, digest: &str) -> bool {
        let mut at = digest.to_string();
        loop {
            let Some(entry) = self.packages.get(&at) else {
                return false;
            };
            match &entry.parent {
                None => return entry.revision == 1,
                Some(parent) => {
                    let Some(up) = self.packages.get(parent) else {
                        return false;
                    };
                    if up.revision + 1 != entry.revision {
                        return false;
                    }
                    at = parent.clone();
                }
            }
        }
    }

    /// The current package: the counting one with the highest revision, the
    /// lowest digest among equals. A function of the set alone.
    pub fn current(&self) -> Option<Digest> {
        self.packages
            .iter()
            .filter(|(digest, _)| self.counts(digest))
            // BTreeMap iterates digests ascending, so the first of the highest
            // revision is the lowest digest.
            .fold(None::<(&String, u64)>, |best, (digest, entry)| match best {
                Some((_, revision)) if revision >= entry.revision => best,
                _ => Some((digest, entry.revision)),
            })
            .and_then(|(digest, _)| unb64(digest).ok())
    }

    /// `digest` and every package it descends from, newest first.
    pub fn lineage(&self, digest: &Digest) -> Vec<(Digest, &Entry)> {
        let mut out = Vec::new();
        let mut at = Some(b64(digest));
        while let Some(key) = at {
            let Some(entry) = self.packages.get(&key) else {
                break;
            };
            if let Ok(d) = unb64(&key) {
                out.push((d, entry));
            }
            at = entry.parent.clone();
        }
        out
    }

    /// The package that introduced `current`'s key: the newest in its lineage
    /// with that key and a snapshot. `None` for a key from before the note was
    /// ever shared, whose content is its update history.
    pub fn introduced(&self, current: &Digest) -> Option<Digest> {
        let key_id = self.get(current)?.key_id.clone();
        self.lineage(current)
            .into_iter()
            .take_while(|(_, e)| e.key_id == key_id)
            .find(|(_, e)| e.snapshot)
            .map(|(d, _)| d)
    }

    pub fn has_held(&self, key_id: &[u8; crate::crypto::KEY_ID_LEN]) -> bool {
        self.held.contains(&b64(key_id))
    }

    pub fn hold(&mut self, key_id: &[u8; crate::crypto::KEY_ID_LEN]) {
        self.held.insert(b64(key_id));
    }

    /// What the owner meant, across every package this device can read: every
    /// membership any of them added, minus every one any of them removed.
    /// Details come from the newest list that names each membership.
    pub fn intended(&self) -> Result<BTreeMap<Membership, Member>> {
        let mut added = BTreeSet::new();
        let mut removed = BTreeSet::new();
        let mut details: BTreeMap<Membership, (u64, Member)> = BTreeMap::new();
        for entry in self.packages.values() {
            added.extend(entry.added()?);
            removed.extend(entry.removed()?);
            for member in entry.members()?.unwrap_or_default() {
                let m = member.membership();
                if details.get(&m).is_none_or(|(rev, _)| *rev < entry.revision) {
                    details.insert(m, (entry.revision, member));
                }
            }
        }
        Ok(added
            .difference(&removed)
            .filter_map(|m| details.remove(m).map(|(_, member)| (*m, member)))
            .collect())
    }
}

/// Encode a digest as the ledger does, for callers that key things by it.
pub fn encode_digest(digest: &Digest) -> String {
    b64(digest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::RecoveryPhrase;
    use crate::crypto::DeviceSeed;

    fn owner() -> DeviceIdentity {
        let mut seed = DeviceSeed([0u8; 64]);
        random_bytes(&mut seed.0);
        DeviceIdentity::from_seed(&seed)
    }

    fn person(name: &str, owner: bool) -> (MemberIdentity, Member) {
        let member = RecoveryPhrase::generate().root().member();
        let entry = Member {
            member: member.id(),
            box_key: member.box_public(),
            owner,
            invite: if owner {
                OWNER_INVITE
            } else {
                InviteId::random()
            },
            name: name.into(),
        };
        (member, entry)
    }

    #[test]
    fn a_grant_opens_for_everyone_listed_and_nobody_else() {
        let signer = owner();
        let note = NoteId::random();
        let key = NoteKey::random();
        let (o, oe) = person("Owner", true);
        let (a, ae) = person("Anna", false);
        let (stranger, _) = person("Ben", false);
        let body = Body {
            list: vec![oe.clone(), ae.clone()],
            added: vec![oe.membership(), ae.membership()],
            ..Body::default()
        };
        let bytes = build(&signer, note, 1, None, &key, &body).unwrap();
        let package = Package::parse(&bytes, note, signer.public_key()).unwrap();
        assert_eq!(package.open_grant(&o).unwrap().0, key.0);
        assert_eq!(package.open_grant(&a).unwrap().0, key.0);
        assert!(package.open_grant(&stranger).is_none());
        assert_eq!(package.open_body(&key).unwrap(), body);
    }

    #[test]
    fn a_package_is_the_same_size_for_two_people_and_for_thirty_two() {
        let signer = owner();
        let note = NoteId::random();
        let key = NoteKey::random();
        let (_, oe) = person("O", true);
        let few = Body {
            list: vec![oe.clone(), person("A", false).1],
            ..Body::default()
        };
        let mut many = few.clone();
        while many.list.len() < MAX_MEMBERS {
            many.list.push(person("somebody with a long name", false).1);
        }
        let a = build(&signer, note, 1, None, &key, &few).unwrap();
        let b = build(&signer, note, 1, None, &key, &many).unwrap();
        assert_eq!(a.len(), b.len());
        assert_eq!(a.len(), 10_554);
    }

    #[test]
    fn a_package_names_the_compaction_key_of_the_key_it_establishes() {
        let signer = owner();
        let note = NoteId::random();
        let key = NoteKey::random();
        let (_, oe) = person("O", true);
        let body = Body {
            list: vec![oe],
            ..Body::default()
        };
        let bytes = build(&signer, note, 1, None, &key, &body).unwrap();
        let package = Package::parse(&bytes, note, signer.public_key()).unwrap();
        assert_eq!(
            package.compaction,
            crate::compaction::public_key(note, &key)
        );
    }

    #[test]
    fn only_the_owner_signs_a_package_that_counts() {
        let signer = owner();
        let note = NoteId::random();
        let (_, oe) = person("O", true);
        let body = Body {
            list: vec![oe],
            ..Body::default()
        };
        let bytes = build(&owner(), note, 1, None, &NoteKey::random(), &body).unwrap();
        assert!(Package::parse(&bytes, note, signer.public_key()).is_err());
        let bytes = build(&signer, note, 1, None, &NoteKey::random(), &body).unwrap();
        assert!(Package::parse(&bytes, NoteId::random(), signer.public_key()).is_err());
        let mut tampered = bytes.clone();
        tampered[200] ^= 1;
        assert!(Package::parse(&tampered, note, signer.public_key()).is_err());
        assert!(Package::parse(&bytes[..bytes.len() - 1], note, signer.public_key()).is_err());
    }

    #[test]
    fn the_current_package_does_not_depend_on_arrival_order() {
        let mut ledger = Ledger::default();
        let mk = |rev: u64, parent: Option<&str>| Entry {
            revision: rev,
            parent: parent.map(str::to_string),
            ..Entry::default()
        };
        let d = |n: u8| b64(&[n; 32]);
        ledger.packages.insert(d(1), mk(1, None));
        ledger.packages.insert(d(9), mk(2, Some(&d(1))));
        ledger.packages.insert(d(4), mk(2, Some(&d(1))));
        // Revision 3 whose parent is missing does not count.
        ledger.packages.insert(d(7), mk(3, Some(&d(8))));
        assert_eq!(ledger.current(), Some([4; 32]));
        // Its parent arrives: now it counts and is current.
        ledger.packages.insert(d(8), mk(2, Some(&d(1))));
        assert_eq!(ledger.current(), Some([7; 32]));
        // A revision that skips does not count.
        ledger.packages.insert(d(2), mk(5, Some(&d(7))));
        assert_eq!(ledger.current(), Some([7; 32]));
    }
}
