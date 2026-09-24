use easynotes_core::crdt::{Edit, NoteDoc};
use easynotes_core::crypto::DeviceIdentity;
use easynotes_core::invite::{self, GuestKey};
use easynotes_core::note::{NoteCore, NoteKind};
use easynotes_core::store::{MemoryKeyring, MemoryStore, OpStore};

type Core<D> = NoteCore<D, MemoryStore, MemoryKeyring>;

fn directory_lifecycle<D: NoteDoc>() {
    let mut owner = Core::<D>::open(MemoryStore::new(), MemoryKeyring::new()).unwrap();
    let mut guest = Core::<D>::open(MemoryStore::new(), MemoryKeyring::new()).unwrap();
    let note = owner.create_note(NoteKind::Text).unwrap();
    owner
        .apply_local(note, Edit::SetBody("Keep this text".into()))
        .unwrap();
    let outgoing = owner.new_invite(note).unwrap();
    owner.remember_invite(&outgoing).unwrap();
    let key = GuestKey::random();
    let request = invite::open_request(
        &outgoing.secret,
        &invite::seal_request(&outgoing.secret, &key, guest.identity(), "Anna").unwrap(),
    )
    .unwrap();
    let grant = owner.seal_grant(&outgoing, &request).unwrap();
    assert!(
        owner.collaborators(note).unwrap().is_empty(),
        "pending invites are not access"
    );
    owner
        .record_collaborator(&outgoing, &request)
        .unwrap()
        .unwrap();
    assert!(owner
        .record_collaborator(&outgoing, &request)
        .unwrap()
        .is_none());
    let (id, note_key, pinned_owner, generation) =
        invite::open_owned_grant(&outgoing.secret, &key, &grant).unwrap();
    guest
        .accept_owned_grant(outgoing.secret.id(), id, note_key, pinned_owner, generation)
        .unwrap();
    for envelope in owner.store().load(note).unwrap() {
        guest.apply_remote(note, &envelope).unwrap();
    }
    let people = owner.collaborators(note).unwrap();
    assert_eq!(people.len(), 1);
    assert_eq!(people[0].name, "Anna");
    assert_eq!(people[0].id, guest.device_key().encode());
    assert_eq!(guest.collaborators(note).unwrap(), people);
    assert!(owner.can_manage_collaborators(note));
    assert!(!guest.can_manage_collaborators(note));
    assert!(guest.record_collaborator(&outgoing, &request).is_err());

    // A content editor cannot invent a valid access assertion or rename a member.
    let fake = easynotes_core::collaborators::sign(
        guest.identity(),
        note,
        0,
        guest.device_key(),
        "Impostor",
    );
    let forged = guest
        .apply_local(note, Edit::AddCollaborator(fake))
        .unwrap();
    owner.apply_remote(note, &forged).unwrap();
    assert_eq!(owner.collaborators(note).unwrap(), people);
    assert_eq!(owner.notes().unwrap()[0].collaborators, people);
    owner.save_snapshot(note).unwrap();
    let (store, keyring) = owner.into_parts();
    let mut owner = Core::<D>::open(store, keyring).unwrap();
    assert_eq!(owner.notes().unwrap()[0].collaborators, people);
    owner.open_note(note).unwrap();
    assert_eq!(owner.collaborators(note).unwrap(), people);

    owner.revoke_collaborators(note).unwrap();
    assert!(owner.collaborators(note).unwrap().is_empty());
    assert!(owner.notes().unwrap()[0].collaborators.is_empty());
    assert_eq!(owner.view(note).unwrap().body, "Keep this text");
    assert!(owner.seal_grant(&outgoing, &request).is_err());
    assert!(owner
        .record_collaborator(&outgoing, &request)
        .unwrap()
        .is_none());
    let fresh = owner.new_invite(note).unwrap();
    owner
        .record_collaborator(&fresh, &request)
        .unwrap()
        .unwrap();
    assert_eq!(owner.collaborators(note).unwrap(), people);
}

#[test]
fn assertions_bind_owner_note_generation_and_name() {
    let owner = DeviceIdentity::generate();
    let guest = DeviceIdentity::generate();
    let note = easynotes_core::ids::NoteId::random();
    let raw = easynotes_core::collaborators::sign(&owner, note, 2, guest.public_key(), "Anna");
    assert!(easynotes_core::collaborators::verify(&raw, owner.public_key(), note, 2).is_some());
    assert!(easynotes_core::collaborators::verify(&raw, guest.public_key(), note, 2).is_none());
    assert!(easynotes_core::collaborators::verify(&raw, owner.public_key(), note, 3).is_none());
    assert!(easynotes_core::collaborators::verify(
        &raw,
        owner.public_key(),
        easynotes_core::ids::NoteId::random(),
        2
    )
    .is_none());
    assert!(easynotes_core::collaborators::verify(
        &raw.replace("Anna", "Beth"),
        owner.public_key(),
        note,
        2
    )
    .is_none());
}

#[cfg(feature = "yrs-backend")]
#[test]
fn yrs_directory_lifecycle() {
    directory_lifecycle::<easynotes_core::crdt::YrsNote>();
}

#[cfg(feature = "automerge-backend")]
#[test]
fn automerge_directory_lifecycle() {
    directory_lifecycle::<easynotes_core::crdt::AutomergeNote>();
}
