//! Slice 3 item 4: a creator can replace the shared key without handing the
//! replacement to former collaborators.

use easynotes_core::crdt::{account, Edit, NoteDoc};
use easynotes_core::crypto::Envelope;
use easynotes_core::invite::{self, GuestKey};
use easynotes_core::note::{NoteCore, NoteKind, SyncCursor};
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
    let guest_identity = easynotes_core::crypto::DeviceIdentity::generate();
    let request = invite::open_request(
        &outgoing.secret,
        &invite::seal_request(&outgoing.secret, &guest_key, &guest_identity, "Bea").unwrap(),
    )
    .unwrap();
    let grant = owner.seal_grant(&outgoing, &request).unwrap();
    let (id, key, creator, generation) =
        invite::open_owned_grant(&outgoing.secret, &guest_key, &grant).unwrap();
    guest
        .accept_owned_grant(outgoing.secret.id(), id, key, creator, generation)
        .unwrap();
}

fn note_access<D: NoteDoc>(
    core: &Core<D>,
    note: easynotes_core::ids::NoteId,
) -> account::NoteAccess {
    account::note_access_of(core.account_entries())
        .remove(&note)
        .unwrap()
}

fn concurrent_owner_rotations_converge_by_account_register<D: NoteDoc>() {
    for account_first in [false, true] {
        for reverse_snapshots in [false, true] {
            let mut phone = core::<D>();
            let phrase = phone.create_account().unwrap();
            let note = phone.create_note(NoteKind::Checklist).unwrap();
            phone
                .apply_local(note, Edit::SetTitle("shared".into()))
                .unwrap();

            let mut laptop = core::<D>();
            laptop.adopt_account(&phrase).unwrap();
            deliver_account(&phone, &mut laptop);
            for raw in phone.store().load(note).unwrap() {
                laptop.apply_remote(note, &raw).unwrap();
            }

            phone
                .apply_local(note, Edit::SetTitle("phone rotation".into()))
                .unwrap();
            laptop
                .apply_local(note, Edit::SetTitle("laptop rotation".into()))
                .unwrap();
            let phone_snapshot = phone.revoke_collaborators(note).unwrap();
            let laptop_snapshot = laptop.revoke_collaborators(note).unwrap();

            if account_first {
                deliver_account(&phone, &mut laptop);
                deliver_account(&laptop, &mut phone);
            }
            if reverse_snapshots {
                phone.apply_remote(note, &laptop_snapshot).unwrap();
                laptop.apply_remote(note, &phone_snapshot).unwrap();
            } else {
                laptop.apply_remote(note, &phone_snapshot).unwrap();
                phone.apply_remote(note, &laptop_snapshot).unwrap();
            }
            if !account_first {
                deliver_account(&phone, &mut laptop);
                deliver_account(&laptop, &mut phone);
            }
            phone.adopt_note_keys().unwrap();
            laptop.adopt_note_keys().unwrap();

            let phone_access = note_access(&phone, note);
            let laptop_access = note_access(&laptop, note);
            assert_eq!(phone_access.generation, 1);
            assert_eq!(phone_access.key.id(), laptop_access.key.id());
            let phone_key_id = Envelope::decode(&phone_snapshot).unwrap().key_id.unwrap();
            let winning_title = if phone_access.key.id() == phone_key_id {
                "phone rotation"
            } else {
                "laptop rotation"
            };
            assert_eq!(
                phone.keyring().note_key(note).unwrap().unwrap().id(),
                phone_access.key.id()
            );
            assert_eq!(
                laptop.keyring().note_key(note).unwrap().unwrap().id(),
                phone_access.key.id()
            );
            assert_eq!(phone.view(note).unwrap().title, winning_title);
            assert_eq!(laptop.view(note).unwrap().title, winning_title);

            let edit = phone
                .apply_local(note, Edit::SetTitle("after reconciliation".into()))
                .unwrap();
            laptop.apply_remote(note, &edit).unwrap();
            assert_eq!(laptop.view(note).unwrap().title, "after reconciliation");
        }
    }
}

fn retained_rotation_survives_compaction_restart_and_late_register<D: NoteDoc>() {
    let mut phone = core::<D>();
    let phrase = phone.create_account().unwrap();
    let note = phone.create_note(NoteKind::Checklist).unwrap();
    phone
        .apply_local(note, Edit::SetTitle("shared".into()))
        .unwrap();

    let mut laptop = core::<D>();
    laptop.adopt_account(&phrase).unwrap();
    deliver_account(&phone, &mut laptop);
    for raw in phone.store().load(note).unwrap() {
        laptop.apply_remote(note, &raw).unwrap();
    }

    phone
        .apply_local(note, Edit::SetTitle("phone rotation".into()))
        .unwrap();
    laptop
        .apply_local(note, Edit::SetTitle("laptop rotation".into()))
        .unwrap();
    let phone_snapshot = phone.revoke_collaborators(note).unwrap();
    let laptop_snapshot = laptop.revoke_collaborators(note).unwrap();

    // A preview learns the merged register so the test can deliver the winning
    // snapshot to the other owner while deliberately withholding its register.
    let mut preview = NoteCore::<D, MemoryStore, MemoryKeyring>::open(
        laptop.store().clone(),
        laptop.keyring().clone(),
    )
    .unwrap();
    deliver_account(&phone, &mut preview);
    deliver_account(&laptop, &mut preview);
    let winning_access = note_access(&preview, note);
    let phone_rotation_key_id = Envelope::decode(&phone_snapshot).unwrap().key_id.unwrap();
    let phone_won = phone_rotation_key_id == winning_access.key.id();
    let winning_title = if phone_won {
        "phone rotation"
    } else {
        "laptop rotation"
    };
    let winning_snapshot = if phone_won {
        phone_snapshot
    } else {
        laptop_snapshot
    };

    let (mut candidate_core, mut delayed_core): (Core<D>, Core<D>) = if phone_won {
        (
            NoteCore::<D, MemoryStore, MemoryKeyring>::open(
                laptop.store().clone(),
                laptop.keyring().clone(),
            )
            .unwrap(),
            NoteCore::<D, MemoryStore, MemoryKeyring>::open(
                laptop.store().clone(),
                laptop.keyring().clone(),
            )
            .unwrap(),
        )
    } else {
        (
            NoteCore::<D, MemoryStore, MemoryKeyring>::open(
                phone.store().clone(),
                phone.keyring().clone(),
            )
            .unwrap(),
            NoteCore::<D, MemoryStore, MemoryKeyring>::open(
                phone.store().clone(),
                phone.keyring().clone(),
            )
            .unwrap(),
        )
    };

    // The first copy receives the candidate before its account update, then
    // compacts and restarts before the winning register arrives.
    candidate_core.open_note(note).unwrap();
    candidate_core
        .apply_remote(note, &winning_snapshot)
        .unwrap();
    let retained_log_len = candidate_core.store().log_len(note).unwrap();
    candidate_core
        .apply_remote(note, &winning_snapshot)
        .unwrap();
    assert_eq!(
        candidate_core.store().log_len(note).unwrap(),
        retained_log_len,
        "a duplicate retained snapshot does not grow the log"
    );
    candidate_core
        .set_sync_cursor(
            note,
            SyncCursor {
                last_seq: 0,
                sent_upto: Some(u64::MAX),
            },
        )
        .unwrap();
    candidate_core.compact(note).unwrap();
    let mut restarted = NoteCore::<D, MemoryStore, MemoryKeyring>::open(
        candidate_core.store().clone(),
        candidate_core.keyring().clone(),
    )
    .unwrap();
    restarted.open_note(note).unwrap();
    assert_ne!(
        restarted.keyring().note_key(note).unwrap().unwrap().id(),
        winning_access.key.id()
    );
    deliver_account(&preview, &mut restarted);
    assert_eq!(
        restarted.keyring().note_key(note).unwrap().unwrap().id(),
        winning_access.key.id()
    );
    assert_eq!(restarted.view(note).unwrap().title, winning_title);

    // The second copy receives the winning register first. Adoption stays
    // pending with the old document until its matching snapshot is delivered.
    delayed_core.open_note(note).unwrap();
    deliver_account(&preview, &mut delayed_core);
    assert_ne!(
        delayed_core.keyring().note_key(note).unwrap().unwrap().id(),
        winning_access.key.id()
    );
    delayed_core.apply_remote(note, &winning_snapshot).unwrap();
    assert_eq!(
        delayed_core.keyring().note_key(note).unwrap().unwrap().id(),
        winning_access.key.id()
    );
    assert_eq!(delayed_core.view(note).unwrap().title, winning_title);
}

fn keyless_owner_waits_for_the_register_winning_snapshot<D: NoteDoc>() {
    let mut phone = core::<D>();
    let phrase = phone.create_account().unwrap();
    let note = phone.create_note(NoteKind::Checklist).unwrap();
    phone
        .apply_local(note, Edit::SetTitle("shared".into()))
        .unwrap();

    let mut laptop = core::<D>();
    laptop.adopt_account(&phrase).unwrap();
    deliver_account(&phone, &mut laptop);
    for raw in phone.store().load(note).unwrap() {
        laptop.apply_remote(note, &raw).unwrap();
    }

    phone
        .apply_local(note, Edit::SetTitle("phone rotation".into()))
        .unwrap();
    laptop
        .apply_local(note, Edit::SetTitle("laptop rotation".into()))
        .unwrap();
    let phone_snapshot = phone.revoke_collaborators(note).unwrap();
    let laptop_snapshot = laptop.revoke_collaborators(note).unwrap();

    let mut merged_account = NoteCore::<D, MemoryStore, MemoryKeyring>::open(
        laptop.store().clone(),
        laptop.keyring().clone(),
    )
    .unwrap();
    deliver_account(&phone, &mut merged_account);
    deliver_account(&laptop, &mut merged_account);
    let winning_access = note_access(&merged_account, note);
    let (winning_snapshot, losing_snapshot, winning_title) =
        if Envelope::decode(&phone_snapshot).unwrap().key_id == Some(winning_access.key.id()) {
            (&phone_snapshot, &laptop_snapshot, "phone rotation")
        } else {
            (&laptop_snapshot, &phone_snapshot, "laptop rotation")
        };

    let mut fresh_owner = core::<D>();
    fresh_owner.adopt_account(&phrase).unwrap();
    deliver_account(&merged_account, &mut fresh_owner);
    assert!(fresh_owner.keyring().note_key(note).unwrap().is_none());
    assert!(fresh_owner.syncable().unwrap().contains(&note));

    assert!(matches!(
        fresh_owner.apply_remote(note, losing_snapshot),
        Err(easynotes_core::CoreError::RotationPending)
    ));
    let retained_log_len = fresh_owner.store().log_len(note).unwrap();
    assert!(matches!(
        fresh_owner.apply_remote(note, losing_snapshot),
        Err(easynotes_core::CoreError::RotationPending)
    ));
    assert_eq!(fresh_owner.store().log_len(note).unwrap(), retained_log_len);

    assert_eq!(
        fresh_owner
            .apply_remote(note, winning_snapshot)
            .unwrap()
            .title,
        winning_title
    );
    assert_eq!(
        fresh_owner.keyring().note_key(note).unwrap().unwrap().id(),
        winning_access.key.id()
    );
}

fn guest_ignores_stale_rotation_and_equal_generation_competitor<D: NoteDoc>() {
    let mut owner = core::<D>();
    owner.create_account().unwrap();
    let note = owner.create_note(NoteKind::Checklist).unwrap();
    owner
        .apply_local(note, Edit::SetTitle("keep".into()))
        .unwrap();
    let mut guest = core::<D>();
    invite_guest(&mut owner, &mut guest, note);
    let generation_one = owner.revoke_collaborators(note).unwrap();
    guest.apply_remote(note, &generation_one).unwrap();
    invite_guest(&mut owner, &mut guest, note);
    guest.apply_remote(note, &generation_one).unwrap();
    let generation_two = owner.revoke_collaborators(note).unwrap();
    guest.apply_remote(note, &generation_two).unwrap();
    invite_guest(&mut owner, &mut guest, note);
    guest.apply_remote(note, &generation_two).unwrap();

    let before = guest.view(note).unwrap();
    let mut forged_stale = generation_one.clone();
    let last = forged_stale.len() - 1;
    forged_stale[last] ^= 1;
    assert!(guest.apply_remote(note, &forged_stale).is_err());
    guest.apply_remote(note, &generation_one).unwrap();
    assert_eq!(guest.view(note).unwrap().title, before.title);
    assert_eq!(
        guest.keyring().note_key(note).unwrap().unwrap().id(),
        note_access(&owner, note).key.id()
    );
}

fn guest_keeps_note_after_equal_generation_competing_rotation<D: NoteDoc>() {
    let mut phone = core::<D>();
    let phrase = phone.create_account().unwrap();
    let note = phone.create_note(NoteKind::Checklist).unwrap();
    phone
        .apply_local(note, Edit::SetTitle("shared".into()))
        .unwrap();
    let mut laptop = core::<D>();
    laptop.adopt_account(&phrase).unwrap();
    deliver_account(&phone, &mut laptop);
    for raw in phone.store().load(note).unwrap() {
        laptop.apply_remote(note, &raw).unwrap();
    }

    let mut guest = core::<D>();
    invite_guest(&mut phone, &mut guest, note);
    let phone_snapshot = phone.revoke_collaborators(note).unwrap();
    laptop
        .apply_local(note, Edit::SetTitle("laptop branch".into()))
        .unwrap();
    let laptop_snapshot = laptop.revoke_collaborators(note).unwrap();
    guest.apply_remote(note, &phone_snapshot).unwrap();
    assert!(guest.keyring().note_key(note).unwrap().is_none());

    deliver_account(&phone, &mut laptop);
    laptop.apply_remote(note, &phone_snapshot).unwrap();
    phone.apply_remote(note, &laptop_snapshot).unwrap();
    deliver_account(&laptop, &mut phone);
    phone.adopt_note_keys().unwrap();
    laptop.adopt_note_keys().unwrap();

    let access = note_access(&phone, note);
    let winning_snapshot =
        if Envelope::decode(&phone_snapshot).unwrap().key_id.unwrap() == access.key.id() {
            &phone_snapshot
        } else {
            &laptop_snapshot
        };
    let losing_snapshot = if std::ptr::eq(winning_snapshot, &phone_snapshot) {
        &laptop_snapshot
    } else {
        &phone_snapshot
    };
    invite_guest(&mut phone, &mut guest, note);
    guest.apply_remote(note, winning_snapshot).unwrap();
    let current_title = guest.view(note).unwrap().title;
    guest.apply_remote(note, losing_snapshot).unwrap();
    assert_eq!(
        guest.keyring().note_key(note).unwrap().unwrap().id(),
        access.key.id()
    );
    assert_eq!(guest.view(note).unwrap().title, current_title);
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

            #[test]
            fn concurrent_owner_rotations_follow_the_merged_account_register() {
                super::concurrent_owner_rotations_converge_by_account_register::<$doc>();
            }

            #[test]
            fn competing_snapshots_survive_compaction_restart_and_delayed_delivery() {
                super::retained_rotation_survives_compaction_restart_and_late_register::<$doc>();
            }

            #[test]
            fn keyless_owner_recovers_from_the_register_winning_snapshot() {
                super::keyless_owner_waits_for_the_register_winning_snapshot::<$doc>();
            }

            #[test]
            fn stale_rotation_replay_cannot_remove_a_generation_two_guest() {
                super::guest_ignores_stale_rotation_and_equal_generation_competitor::<$doc>();
            }

            #[test]
            fn equal_generation_competing_snapshot_does_not_remove_a_guest() {
                super::guest_keeps_note_after_equal_generation_competing_rotation::<$doc>();
            }
        }
    };
}

#[cfg(feature = "automerge-backend")]
backend!(automerge, easynotes_core::crdt::AutomergeNote);
#[cfg(feature = "yrs-backend")]
backend!(yrs, easynotes_core::crdt::YrsNote);
