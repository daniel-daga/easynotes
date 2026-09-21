//! Slice 3 item 4: a creator can replace the shared key without handing the
//! replacement to former collaborators.

use easynotes_core::crdt::{Edit, NoteDoc};
use easynotes_core::invite::{self, GuestKey};
use easynotes_core::note::{NoteCore, NoteKind};
use easynotes_core::store::{Keyring, MemoryKeyring, MemoryStore, OpStore};

type Core<D> = NoteCore<D, MemoryStore, MemoryKeyring>;

fn core<D: NoteDoc>() -> Core<D> {
    NoteCore::open(MemoryStore::new(), MemoryKeyring::new()).unwrap()
}

fn deliver_account<D: NoteDoc>(from: &Core<D>, to: &mut Core<D>) {
    let id = from.account_id().unwrap();
    for raw in from.store().load(id).unwrap() {
        to.apply_remote_account(&raw).unwrap();
    }
    to.adopt_note_keys().unwrap();
}

fn invite_guest<D: NoteDoc>(
    owner: &mut Core<D>,
    guest: &mut Core<D>,
    note: easynotes_core::ids::NoteId,
) {
    let outgoing = owner.new_invite(note).unwrap();
    let guest_key = GuestKey::random();
    let request = invite::open_request(
        &outgoing.secret,
        &invite::seal_request(&outgoing.secret, &guest_key, "Bea").unwrap(),
    )
    .unwrap();
    let grant = owner.seal_grant(&outgoing, &request).unwrap();
    let (id, key, creator, generation) =
        invite::open_owned_grant(&outgoing.secret, &guest_key, &grant).unwrap();
    guest
        .accept_owned_grant(outgoing.secret.id(), id, key, creator, generation)
        .unwrap();
}

fn revocation_removes_a_guest_and_refuses_its_old_key<D: NoteDoc>() {
    let mut owner = core::<D>();
    owner.create_account().unwrap();
    let note = owner.create_note(NoteKind::Checklist).unwrap();
    let first = owner
        .apply_local(note, Edit::SetTitle("Saturday".into()))
        .unwrap();

    let mut guest = core::<D>();
    invite_guest(&mut owner, &mut guest, note);
    guest.apply_remote(note, &first).unwrap();
    let stale = guest
        .apply_local(note, Edit::SetTitle("Former member".into()))
        .unwrap();

    let rotation = owner.revoke_collaborators(note).unwrap();
    owner.apply_remote(note, &stale).unwrap();
    assert_eq!(
        owner.view(note).unwrap().title,
        "Saturday",
        "the old key is dead"
    );

    guest.apply_remote(note, &rotation).unwrap();
    assert!(guest.keyring().note_key(note).unwrap().is_none());
    assert!(guest
        .notes()
        .unwrap()
        .iter()
        .all(|summary| summary.id != note));
    assert_eq!(owner.view(note).unwrap().title, "Saturday");

    // A collaborator who should remain can be invited again. Old-generation
    // relay history is harmlessly skipped before the new snapshot arrives.
    invite_guest(&mut owner, &mut guest, note);
    guest.apply_remote(note, &first).unwrap();
    guest.apply_remote(note, &rotation).unwrap();
    assert_eq!(guest.view(note).unwrap().title, "Saturday");
}

fn an_owners_other_device_survives_either_delivery_order<D: NoteDoc>() {
    for account_first in [false, true] {
        let mut phone = core::<D>();
        let phrase = phone.create_account().unwrap();
        let note = phone.create_note(NoteKind::Checklist).unwrap();
        phone
            .apply_local(note, Edit::SetTitle("Keep me".into()))
            .unwrap();

        let mut laptop = core::<D>();
        laptop.adopt_account(&phrase).unwrap();
        deliver_account(&phone, &mut laptop);
        for raw in phone.store().load(note).unwrap() {
            laptop.apply_remote(note, &raw).unwrap();
        }

        let rotation = phone.revoke_collaborators(note).unwrap();
        if account_first {
            deliver_account(&phone, &mut laptop);
            laptop.apply_remote(note, &rotation).unwrap();
        } else {
            // The snapshot is kept while its key is still in flight.
            laptop.apply_remote(note, &rotation).unwrap();
            deliver_account(&phone, &mut laptop);
        }

        assert_eq!(laptop.view(note).unwrap().title, "Keep me");
        let edit = laptop
            .apply_local(note, Edit::SetTitle("Still here".into()))
            .unwrap();
        phone.apply_remote(note, &edit).unwrap();
        assert_eq!(phone.view(note).unwrap().title, "Still here");
    }
}

fn any_device_in_the_owners_account_can_revoke<D: NoteDoc>() {
    let mut phone = core::<D>();
    let phrase = phone.create_account().unwrap();
    let note = phone.create_note(NoteKind::Checklist).unwrap();
    let edit = phone
        .apply_local(note, Edit::SetTitle("Household".into()))
        .unwrap();

    let mut laptop = core::<D>();
    laptop.adopt_account(&phrase).unwrap();
    deliver_account(&phone, &mut laptop);
    laptop.apply_remote(note, &edit).unwrap();

    // The revocation signing seed travelled only inside the account document;
    // this device did not create the note and can still perform the cutover.
    let rotation = laptop.revoke_collaborators(note).unwrap();
    deliver_account(&laptop, &mut phone);
    phone.apply_remote(note, &rotation).unwrap();
    assert_eq!(phone.view(note).unwrap().title, "Household");
}

fn a_note_created_before_its_account_is_revocable<D: NoteDoc>() {
    let mut owner = core::<D>();
    let note = owner.create_note(NoteKind::Checklist).unwrap();
    owner
        .apply_local(note, Edit::SetTitle("Created first".into()))
        .unwrap();

    // Account creation upgrades every locally held note into an owned access
    // record, including the revocation seed.
    owner.create_account().unwrap();
    owner.revoke_collaborators(note).unwrap();
    assert_eq!(owner.view(note).unwrap().title, "Created first");
}

macro_rules! backend {
    ($module:ident, $doc:ty) => {
        mod $module {
            #[test]
            fn revocation_removes_a_guest_and_refuses_its_old_key() {
                super::revocation_removes_a_guest_and_refuses_its_old_key::<$doc>();
            }

            #[test]
            fn an_owners_other_device_survives_either_delivery_order() {
                super::an_owners_other_device_survives_either_delivery_order::<$doc>();
            }

            #[test]
            fn any_device_in_the_owners_account_can_revoke() {
                super::any_device_in_the_owners_account_can_revoke::<$doc>();
            }

            #[test]
            fn a_note_created_before_its_account_is_revocable() {
                super::a_note_created_before_its_account_is_revocable::<$doc>();
            }
        }
    };
}

#[cfg(feature = "automerge-backend")]
backend!(automerge, easynotes_core::crdt::AutomergeNote);
#[cfg(feature = "yrs-backend")]
backend!(yrs, easynotes_core::crdt::YrsNote);
