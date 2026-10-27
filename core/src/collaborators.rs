//! A person on a shared note, as a list or a search shows them.
//!
//! Read off the note's current access package
//! ([ADR 0023](../../docs/decisions.md#adr-0023)): one entry per member
//! ID, so one person on two phones is one entry and two people who chose the
//! same name are two.
//!
//! The package names a person as they were when they were let in. A name that
//! changed afterwards travels in the note's own document instead, as one
//! signed record per member
//! ([ADR 0032](../../docs/decisions.md#adr-0032)). Only
//! that person's member key can sign it, so a collaborator cannot rename
//! somebody else — and because it lives inside the note rather than in the
//! owner's package, a guest can rename themselves without the owner having to
//! republish anything.
//!
//! The same document carries a signed "I left" record when a guest deletes
//! their account ([ADR 0034](../../docs/decisions.md#adr-0034)).
//! It is keyed by the member who signed it and names the invite they joined
//! with, so the owner can remove exactly that membership and rotate the note
//! key.
use crate::access::MAX_NAME_BYTES;
use crate::account::MemberIdentity;
use crate::crypto::{AuthorKey, SIG_LEN};
use crate::ids::{InviteId, NoteId};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde::{Deserialize, Serialize};

/// What binds a record to this application, this note, this person and this
/// generation. Public. A record copied into another note, or signed over
/// different bytes, fails to verify.
const NAME_CONTEXT: &[u8] = b"easynotes/member-name/v1";

/// The domain for a guest's signed statement that they left a note. Separate
/// from the name record's so neither signature can stand in for the other.
const LEFT_CONTEXT: &[u8] = b"easynotes/member-left/v1";

/// Bytes of the rename generation in front of a name.
const SEQ_LEN: usize = 8;

/// Longer than any record this build writes (137 bytes, about 184 base64
/// characters), so a peer cannot make every device decode a megabyte per read.
const MAX_RECORD_CHARS: usize = 256;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Collaborator {
    /// The member ID, base64url.
    pub id: String,
    /// Chosen by the person. A label, not a verified identity.
    pub name: String,
}

/// One person's name as the note's document records it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemberName {
    /// The rename this record belongs to. Compare, never decode: a device
    /// whose account has not caught up with a rename has a lower one, and
    /// must not overwrite the record of a rename it has not seen.
    pub seq: u64,
    pub name: String,
}

impl MemberName {
    /// Whether this record can be replaced by one for `name` at `seq`.
    ///
    /// The answer is no when the record is from a later rename than the one
    /// this device knows about: another phone of the same account changed the
    /// name and this device has not read that yet.
    pub fn is_behind(&self, name: &str, seq: u64) -> bool {
        self.name != name && self.seq <= seq
    }
}

fn signed(note: NoteId, member: AuthorKey, name: &str, seq: u64) -> Vec<u8> {
    let mut message = Vec::with_capacity(NAME_CONTEXT.len() + 16 + 32 + SEQ_LEN + name.len());
    message.extend_from_slice(NAME_CONTEXT);
    message.extend_from_slice(note.as_bytes());
    message.extend_from_slice(&member.0);
    message.extend_from_slice(&seq.to_be_bytes());
    message.extend_from_slice(name.as_bytes());
    message
}

/// One member's name, signed by their own member key, as a note document
/// stores it.
pub fn sign(member: &MemberIdentity, note: NoteId, name: &str, seq: u64) -> String {
    let name = crate::access::clip_name(name);
    let mut out = Vec::with_capacity(SEQ_LEN + 1 + name.len() + SIG_LEN);
    out.extend_from_slice(&seq.to_be_bytes());
    let len = u8::try_from(name.len()).expect("clip_name cuts to 64 bytes");
    out.push(len);
    out.extend_from_slice(name.as_bytes());
    out.extend_from_slice(&member.sign(&signed(note, member.id(), name, seq)));
    URL_SAFE_NO_PAD.encode(out)
}

/// Read and verify a stored record, or `None` for one that is not this
/// person's signed statement about this note.
pub fn read(raw: &str, note: NoteId, member: AuthorKey) -> Option<MemberName> {
    if raw.len() > MAX_RECORD_CHARS {
        return None;
    }
    let bytes = URL_SAFE_NO_PAD.decode(raw).ok()?;
    if bytes.len() < SEQ_LEN + 2 + SIG_LEN {
        return None;
    }
    let seq = u64::from_be_bytes(bytes[..SEQ_LEN].try_into().ok()?);
    let len = usize::from(bytes[SEQ_LEN]);
    let rest = &bytes[SEQ_LEN + 1..];
    if len == 0 || len > MAX_NAME_BYTES || rest.len() != len + SIG_LEN {
        return None;
    }
    let (name, signature) = rest.split_at(len);
    let name = std::str::from_utf8(name).ok()?;
    if name.trim() != name || name.is_empty() {
        return None;
    }
    let signature: &[u8; SIG_LEN] = signature.try_into().ok()?;
    member
        .verify(&signed(note, member, name, seq), signature)
        .then(|| MemberName {
            seq,
            name: name.to_string(),
        })
}

fn left_signed(note: NoteId, member: AuthorKey, invite: InviteId) -> Vec<u8> {
    let mut message = Vec::with_capacity(LEFT_CONTEXT.len() + 16 + 32 + 16);
    message.extend_from_slice(LEFT_CONTEXT);
    message.extend_from_slice(note.as_bytes());
    message.extend_from_slice(&member.0);
    message.extend_from_slice(invite.as_bytes());
    message
}

/// One member's statement that they left the note, signed by their own member
/// key, as a note document stores it.
///
/// The invite is part of the statement because a membership is the pair: a
/// person invited again later has a new invite, and the old record must not
/// remove the new membership.
pub fn sign_left(member: &MemberIdentity, note: NoteId, invite: InviteId) -> String {
    let mut out = Vec::with_capacity(16 + SIG_LEN);
    out.extend_from_slice(invite.as_bytes());
    out.extend_from_slice(&member.sign(&left_signed(note, member.id(), invite)));
    URL_SAFE_NO_PAD.encode(out)
}

/// Read and verify a stored leave record, or `None` for one that is not this
/// person's signed statement about this note and invite.
pub fn read_left(raw: &str, note: NoteId, member: AuthorKey) -> Option<InviteId> {
    if raw.len() > MAX_RECORD_CHARS {
        return None;
    }
    let bytes = URL_SAFE_NO_PAD.decode(raw).ok()?;
    if bytes.len() != 16 + SIG_LEN {
        return None;
    }
    let (invite, signature) = bytes.split_at(16);
    let invite = InviteId::from_bytes(invite.try_into().ok()?);
    let signature: &[u8; SIG_LEN] = signature.try_into().ok()?;
    member
        .verify(&left_signed(note, member, invite), signature)
        .then_some(invite)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::RecoveryPhrase;

    fn someone() -> MemberIdentity {
        RecoveryPhrase::generate().root().member()
    }

    /// Pinned so that two builds cannot disagree about the bytes, and so a
    /// signature made by one build reads in the other.
    #[test]
    fn a_record_is_a_fixed_shape() {
        let member = someone();
        let note = NoteId::from_bytes([3u8; 16]);
        let record = sign(&member, note, "Ada", 7);
        let raw = URL_SAFE_NO_PAD.decode(&record).unwrap();
        assert_eq!(raw.len(), SEQ_LEN + 1 + 3 + SIG_LEN);
        assert_eq!(&raw[..SEQ_LEN], &7u64.to_be_bytes());
        assert_eq!(raw[SEQ_LEN], 3);
        assert_eq!(&raw[SEQ_LEN + 1..SEQ_LEN + 4], b"Ada");
        assert_eq!(
            signed(note, member.id(), "Ada", 7),
            [
                NAME_CONTEXT,
                note.as_bytes(),
                &member.id().0,
                &7u64.to_be_bytes(),
                b"Ada"
            ]
            .concat()
        );
    }

    #[test]
    fn a_record_reads_back_only_for_its_own_note_and_member() {
        let member = someone();
        let other = someone();
        let note = NoteId::from_bytes([3u8; 16]);
        let record = sign(&member, note, "Ada", 7);
        assert_eq!(
            read(&record, note, member.id()),
            Some(MemberName {
                seq: 7,
                name: "Ada".into()
            })
        );
        assert_eq!(
            read(&record, NoteId::from_bytes([4u8; 16]), member.id()),
            None
        );
        assert_eq!(read(&record, note, other.id()), None);
    }

    #[test]
    fn a_tampered_record_is_refused() {
        let member = someone();
        let note = NoteId::from_bytes([3u8; 16]);
        let record = sign(&member, note, "Ada", 7);
        let mut raw = URL_SAFE_NO_PAD.decode(&record).unwrap();
        // Rename the person in the record without re-signing it.
        raw[SEQ_LEN + 1] = b'E';
        assert_eq!(read(&URL_SAFE_NO_PAD.encode(raw), note, member.id()), None);
        // And a record with a different sequence number is a different
        // statement, so its signature does not carry over either.
        let mut raw = URL_SAFE_NO_PAD.decode(&record).unwrap();
        raw[SEQ_LEN - 1] = 8;
        assert_eq!(read(&URL_SAFE_NO_PAD.encode(raw), note, member.id()), None);
    }

    #[test]
    fn a_name_longer_than_an_invite_carries_is_cut_the_same_way() {
        let member = someone();
        let note = NoteId::from_bytes([3u8; 16]);
        let long = "é".repeat(MAX_NAME_BYTES);
        let record = sign(&member, note, &long, 1);
        let read_back = read(&record, note, member.id()).unwrap();
        assert_eq!(read_back.name, crate::access::clip_name(&long));
        assert!(read_back.name.len() <= MAX_NAME_BYTES);
    }

    #[test]
    fn a_leave_record_reads_back_only_for_its_own_note_member_and_invite() {
        let member = someone();
        let other = someone();
        let note = NoteId::from_bytes([5u8; 16]);
        let invite = InviteId::from_bytes([9u8; 16]);
        let record = sign_left(&member, note, invite);
        assert_eq!(read_left(&record, note, member.id()), Some(invite));
        assert_eq!(
            read_left(&record, NoteId::from_bytes([6u8; 16]), member.id()),
            None
        );
        assert_eq!(read_left(&record, note, other.id()), None);
    }

    #[test]
    fn a_tampered_leave_record_is_refused() {
        let member = someone();
        let note = NoteId::from_bytes([5u8; 16]);
        let record = sign_left(&member, note, InviteId::from_bytes([9u8; 16]));
        let mut raw = URL_SAFE_NO_PAD.decode(&record).unwrap();
        raw[0] ^= 1;
        assert_eq!(
            read_left(&URL_SAFE_NO_PAD.encode(raw), note, member.id()),
            None
        );
    }

    #[test]
    fn a_leave_record_is_not_a_name_record() {
        let member = someone();
        let note = NoteId::from_bytes([5u8; 16]);
        let left = sign_left(&member, note, InviteId::from_bytes([9u8; 16]));
        let name = sign(&member, note, "Ada", 1);
        assert_eq!(read(&left, note, member.id()), None);
        assert_eq!(read_left(&name, note, member.id()), None);
    }

    #[test]
    fn a_record_from_a_later_rename_is_not_overwritten_by_an_earlier_one() {
        let record = MemberName {
            seq: 4,
            name: "Anna".into(),
        };
        assert!(!record.is_behind("Anna", 4), "already that name");
        assert!(
            !record.is_behind("Ann", 3),
            "a rename this device has not seen"
        );
        assert!(
            record.is_behind("Ann", 4),
            "the same generation, changed name"
        );
        assert!(record.is_behind("Ann", 5), "a later rename");
    }
}
