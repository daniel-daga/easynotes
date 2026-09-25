//! What a device remembers about invites while somebody decides.
//!
//! An owner opens an invite and closes the app; a guest asks to join and puts
//! the phone away. Both answers arrive later, and the secrets that let each
//! side act on them have to still be there — sealed, like everything else this
//! device keeps. The relay half is `relay/tests/invites.rs`.

use easynotes_core::crdt::DefaultNoteDoc;
use easynotes_core::crypto::MasterKey;
use easynotes_core::invite::{self, InviteSecret};
use easynotes_core::note::{NoteCore, NoteKind, PendingJoin};
use easynotes_core::store::{MemoryKeyring, MemoryStore, SqliteKeyring, SqliteStore};
use std::path::Path;

type Core = NoteCore<DefaultNoteDoc, MemoryStore, MemoryKeyring>;
type OnDisk = NoteCore<DefaultNoteDoc, SqliteStore, SqliteKeyring>;

/// A device that can share: an account, and a name to share under.
fn core() -> Core {
    let mut core = NoteCore::open(MemoryStore::new(), MemoryKeyring::new()).unwrap();
    core.create_account().unwrap();
    core.set_profile_name("Olga").unwrap();
    core
}

fn on_disk(dir: &Path) -> OnDisk {
    let master = MasterKey([7u8; 32]);
    let mut core: OnDisk = NoteCore::open(
        SqliteStore::open(dir.join("notes.db"), master.clone()).unwrap(),
        SqliteKeyring::open(dir.join("keys.db"), master).unwrap(),
    )
    .unwrap();
    if !core.has_account() {
        core.create_account().unwrap();
        core.set_profile_name("Olga").unwrap();
    }
    core
}

fn join(name: &str) -> PendingJoin {
    PendingJoin {
        secret: InviteSecret::random(),
        request: "req".into(),
        name: name.into(),
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

#[test]
fn an_invite_and_a_join_survive_a_relaunch_and_are_sealed_on_disk() {
    let dir = tempfile::tempdir().unwrap();
    let (invite_id, owner_secret, join_code) = {
        let mut core = on_disk(dir.path());
        let note = core.create_note(NoteKind::Checklist).unwrap();
        let mut outgoing = core.new_invite(note).unwrap();
        outgoing.expires_at = 1234;
        core.remember_invite(&outgoing).unwrap();
        let waiting = join("Annabelle Quist");
        core.remember_join(&waiting).unwrap();
        (
            outgoing.secret.id(),
            outgoing.owner.encode(),
            waiting.secret.code(),
        )
    };

    let core = on_disk(dir.path());
    let invites = core.invites().unwrap();
    assert_eq!(invites.len(), 1);
    assert_eq!(invites[0].secret.id(), invite_id);
    assert_eq!(invites[0].owner.encode(), owner_secret);
    assert_eq!(invites[0].expires_at, 1234);
    let joins = core.joins().unwrap();
    assert_eq!(joins.len(), 1);
    assert_eq!(joins[0].secret.code(), join_code);
    assert_eq!(joins[0].name, "Annabelle Quist");

    // The reserved row they are filed under is not a note.
    assert!(
        core.syncable().unwrap().len() == 1,
        "only the note is syncable"
    );

    drop(core);
    for entry in std::fs::read_dir(dir.path()).unwrap().flatten() {
        let bytes = std::fs::read(entry.path()).unwrap();
        for (what, secret) in [
            ("the owner secret", owner_secret.as_bytes()),
            ("the link", join_code.as_bytes()),
            ("the guest's name", b"Annabelle Quist".as_slice()),
        ] {
            assert!(
                !contains(&bytes, secret),
                "{what} is in {} in the clear",
                entry.path().display()
            );
        }
    }
}

#[test]
fn a_second_request_on_one_invite_replaces_the_first() {
    let mut core = core();
    let first = join("Anna");
    core.remember_join(&first).unwrap();
    let again = PendingJoin {
        request: "second".into(),
        ..first.clone()
    };
    core.remember_join(&again).unwrap();
    core.remember_join(&join("Elsewhere")).unwrap();
    let joins = core.joins().unwrap();
    assert_eq!(joins.len(), 2);
    assert_eq!(joins[0].request, "second");

    core.forget_join(first.secret.id()).unwrap();
    assert_eq!(core.joins().unwrap().len(), 1);
    // Forgetting what is not there is not an error.
    core.forget_join(first.secret.id()).unwrap();
}

#[test]
fn invites_outlive_making_an_account() {
    // Making an account moves the account document off the reserved ID and
    // forgets what was filed there. These rows are this device's, not the
    // document's, so they must come along. Sharing needs an account, so the
    // invite here is put straight into its row.
    let mut core: Core = NoteCore::open(MemoryStore::new(), MemoryKeyring::new()).unwrap();
    let note = core.create_note(NoteKind::Checklist).unwrap();
    let outgoing = easynotes_core::note::OutgoingInvite {
        note,
        secret: InviteSecret::random(),
        owner: invite::OwnerSecret::random(),
        expires_at: 0,
        approval: None,
    };
    core.remember_invite(&outgoing).unwrap();
    let waiting = join("Anna");
    core.remember_join(&waiting).unwrap();

    core.create_account().unwrap();

    assert_eq!(core.invites().unwrap()[0].secret.id(), outgoing.secret.id());
    assert_eq!(core.joins().unwrap()[0].secret.id(), waiting.secret.id());
}

#[test]
fn forgetting_a_note_forgets_its_invites() {
    let mut core = core();
    let kept = core.create_note(NoteKind::Checklist).unwrap();
    let gone = core.create_note(NoteKind::Checklist).unwrap();
    core.remember_invite(&core.new_invite(kept).unwrap())
        .unwrap();
    core.remember_invite(&core.new_invite(gone).unwrap())
        .unwrap();

    core.forget_note(gone).unwrap();

    let invites = core.invites().unwrap();
    assert_eq!(invites.len(), 1);
    assert_eq!(invites[0].note, kept);
}

#[test]
fn only_a_note_this_device_can_open_can_be_invited_to() {
    let core = core();
    assert!(core
        .new_invite(easynotes_core::ids::NoteId::random())
        .is_err());
    assert!(core.new_invite(easynotes_core::note::ACCOUNT_DOC).is_err());
}

#[test]
fn sharing_needs_an_account_and_a_name() {
    let mut bare: Core = NoteCore::open(MemoryStore::new(), MemoryKeyring::new()).unwrap();
    let note = bare.create_note(NoteKind::Checklist).unwrap();
    assert!(bare.new_invite(note).is_err(), "no account");
    assert!(bare.seal_join_request(&InviteSecret::random()).is_err());
    bare.create_account().unwrap();
    assert!(bare.new_invite(note).is_err(), "no name");
    assert!(bare.set_profile_name("   ").is_err());
    bare.set_profile_name("  Olga  ").unwrap();
    assert_eq!(bare.profile_name().as_deref(), Some("Olga"));
    assert!(bare.new_invite(note).is_ok());
}

#[test]
fn only_the_owner_invites() {
    let mut owner = core();
    let note = owner.create_note(NoteKind::Checklist).unwrap();
    let mut guest = core();
    let blob = owner.export_share_blob(note).unwrap();
    guest.import_share_blob(&blob).unwrap();
    assert!(guest.new_invite(note).is_err());
}

#[test]
fn an_answer_is_ready_only_once_the_package_is_on_the_relay() {
    let mut owner = core();
    let note = owner.create_note(NoteKind::Checklist).unwrap();
    let outgoing = owner.new_invite(note).unwrap();
    owner.remember_invite(&outgoing).unwrap();

    let mut guest = core();
    guest.set_profile_name("Anna").unwrap();
    let sealed = guest.seal_join_request(&outgoing.secret).unwrap();
    let request = invite::open_request(&outgoing.secret, &sealed).unwrap();
    assert_eq!(request.name, "Anna");
    owner
        .approve(outgoing.secret.id(), "req", &request)
        .unwrap();
    // Saved, so a crash here resumes from the row.
    let row = &owner.invites().unwrap()[0];
    assert_eq!(row.approval.as_ref().unwrap().1, request);
    assert!(owner
        .approval_answer(outgoing.secret.id())
        .unwrap()
        .is_none());

    // Built and sent; not yet echoed.
    let out = owner.settle_note(note).unwrap();
    assert_eq!(out.envelopes.len(), 1);
    assert!(owner
        .approval_answer(outgoing.secret.id())
        .unwrap()
        .is_none());
    // Echoed.
    owner.apply_remote(note, &out.envelopes[0]).unwrap();
    let answer = owner
        .approval_answer(outgoing.secret.id())
        .unwrap()
        .unwrap();

    let (id, owner_key) =
        invite::open_answer(&outgoing.secret, &guest.member().unwrap(), &answer).unwrap();
    assert_eq!(id, note);
    let joined = guest
        .accept_answer(outgoing.secret.id(), id, owner_key)
        .unwrap();
    assert_eq!(joined, note);
    // Recorded, and not readable until the package arrives.
    assert!(guest.syncable().unwrap().contains(&note));
    assert!(!guest.can_open(note));
    guest.apply_remote(note, &out.envelopes[0]).unwrap_err();
    guest.settle_note(note).unwrap();
    assert!(guest.can_open(note));
}
