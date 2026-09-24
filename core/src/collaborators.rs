//! Owner-signed assertions stored inside the encrypted note (ADR 0020).
use crate::crypto::{AuthorKey, DeviceIdentity};
use crate::ids::NoteId;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Collaborator {
    pub id: String,
    pub name: String,
}

fn message(note: NoteId, generation: u64, person: &Collaborator) -> Vec<u8> {
    let mut bytes = b"easynotes/collaborator/v1".to_vec();
    bytes.extend_from_slice(note.as_bytes());
    bytes.extend_from_slice(&generation.to_be_bytes());
    bytes.extend_from_slice(person.id.as_bytes());
    bytes.extend_from_slice(person.name.as_bytes());
    bytes
}

pub fn sign(
    owner: &DeviceIdentity,
    note: NoteId,
    generation: u64,
    author: AuthorKey,
    name: &str,
) -> String {
    let person = Collaborator {
        id: URL_SAFE_NO_PAD.encode(author.0),
        name: name.to_owned(),
    };
    let signature = URL_SAFE_NO_PAD.encode(owner.sign(&message(note, generation, &person)));
    serde_json::to_string(&(person, signature)).expect("strings serialize")
}

pub fn verify(raw: &str, owner: AuthorKey, note: NoteId, generation: u64) -> Option<Collaborator> {
    let (person, signature): (Collaborator, String) = serde_json::from_str(raw).ok()?;
    let author = URL_SAFE_NO_PAD.decode(&person.id).ok()?;
    if author.len() != 32 || URL_SAFE_NO_PAD.encode(&author) != person.id {
        return None;
    }
    let signature: [u8; 64] = URL_SAFE_NO_PAD.decode(signature).ok()?.try_into().ok()?;
    owner
        .verify(&message(note, generation, &person), &signature)
        .then_some(person)
}

/// Content-addressed keys keep a forged assertion from overwriting a valid one.
pub fn key(raw: &str) -> String {
    use sha2::{Digest, Sha256};
    format!(
        "collaborator:{}",
        URL_SAFE_NO_PAD.encode(Sha256::digest(raw.as_bytes()))
    )
}
