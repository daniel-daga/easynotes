//! One person's two devices.
//!
//! the later milestone item 2. Everything here is the thing that did not exist before: a
//! person. Until this landed a colour, a label, a pin and a placement stayed on
//! the device that chose them, not because syncing them was hard but because
//! there was nothing to sync them *to* — no such thing as one person's second
//! device. Twelve words are what makes two devices one person's, and these are
//! the properties that has to mean.
//!
//! There is no relay in this file. Two cores are stood up, and an envelope one
//! of them wrote is handed to the other by hand, which is exactly what the sync
//! engine will do over a socket and lets the property be tested without one.
//! See `core/tests/convergence.rs` for the same trick on a note.

use easynotes_core::account::RecoveryPhrase;
use easynotes_core::crdt::{Colour, NoteDoc};
use easynotes_core::crypto::{Envelope, MasterKey};
use easynotes_core::ids::NoteId;
use easynotes_core::note::{NoteCore, NoteKind, ACCOUNT_DOC};
use easynotes_core::store::{
    master_key_from_file, Keyring, MemoryKeyring, MemoryStore, OpStore, SqliteKeyring, SqliteStore,
};
use std::path::Path;

type Memory<D> = NoteCore<D, MemoryStore, MemoryKeyring>;

fn in_memory<D: NoteDoc>() -> Memory<D> {
    NoteCore::open(MemoryStore::new(), MemoryKeyring::new()).unwrap()
}

fn on_disk<D: NoteDoc>(dir: &Path) -> NoteCore<D, SqliteStore, SqliteKeyring> {
    let master: MasterKey = master_key_from_file(dir.join("master.key")).unwrap();
    NoteCore::open(
        SqliteStore::open(dir.join("notes.db"), master.clone()).unwrap(),
        SqliteKeyring::open(dir.join("keys.db"), master).unwrap(),
    )
    .unwrap()
}

/// Carry everything one core has said about its account document to the other,
/// the way a relay would: whole envelopes, in order, signature and all, and
/// then the settle step the relay's `Caught` will trigger.
///
/// Deliberately not "copy the state" — that would test the CRDT and not the
/// thing being built. What crosses here is exactly what would cross a socket.
fn deliver_account<D: NoteDoc, S: OpStore, K: Keyring, S2: OpStore, K2: Keyring>(
    from: &NoteCore<D, S, K>,
    to: &mut NoteCore<D, S2, K2>,
) {
    let id = from.account_id().expect("the sender has an account");
    for envelope in from.store().load(id).unwrap() {
        to.apply_remote_account(&envelope).unwrap();
    }
    // The backlog is drained. This is `Incoming::Caught`, and it is where keys
    // are taken out of the document — see `NoteCore::adopt_note_keys` for why
    // it is here and not inside the loop.
    to.adopt_note_keys().unwrap();
}

// ---------------------------------------------------------------------------
// The address and the key
// ---------------------------------------------------------------------------

/// The constraint that is easy to miss and is fatal without it: the relay
/// keeps one log per note ID, so an account document that synced under the
/// reserved ID would put every user of every relay on one topic.
fn an_account_moves_the_document_off_the_reserved_id<D: NoteDoc>() {
    let mut core = in_memory::<D>();
    assert_eq!(
        core.account_id(),
        None,
        "no account, nothing to subscribe to"
    );

    core.create_account().unwrap();
    let id = core.account_id().expect("an account now");
    assert_ne!(id, ACCOUNT_DOC);
    assert!(
        core.store().load(ACCOUNT_DOC).unwrap().is_empty(),
        "the document moved rather than being copied"
    );
    assert!(!core.store().load(id).unwrap().is_empty());
}

/// Two people are two topics. The relay sees two unrelated sixteen-byte IDs and
/// has no way to tell either is an account document at all.
fn two_phrases_are_two_documents<D: NoteDoc>() {
    let mut mine = in_memory::<D>();
    let mut yours = in_memory::<D>();
    mine.create_account().unwrap();
    yours.create_account().unwrap();
    assert_ne!(mine.account_id(), yours.account_id());
}

/// What a device keeps is the root, not the words. A relaunch must find its
/// account without anybody typing anything.
fn a_device_finds_its_account_again_after_a_relaunch<D: NoteDoc>() {
    let dir = tempfile::tempdir().unwrap();
    let id = {
        let mut core = on_disk::<D>(dir.path());
        let phrase = core.create_account().unwrap();
        // The phrase is shown once and goes nowhere near the disk.
        drop(phrase);
        core.account_id().unwrap()
    };
    let core = on_disk::<D>(dir.path());
    assert_eq!(core.account_id(), Some(id));
}

/// The log is addressed to the document it is for, the same property
/// `the_account_log_is_addressed_to_the_account_document` holds for the
/// local-only case — and it has to still hold after the move, because the AAD
/// and the signature both bind the ID.
fn the_moved_log_is_addressed_to_the_derived_id<D: NoteDoc>() {
    let mut core = in_memory::<D>();
    core.create_note(NoteKind::Checklist).unwrap();
    core.create_account().unwrap();
    let id = core.account_id().unwrap();

    let envelopes = core.store().load(id).unwrap();
    assert!(!envelopes.is_empty());
    for raw in envelopes {
        let envelope = Envelope::decode(&raw).unwrap();
        assert_eq!(envelope.note_id, id);
        // And it opens under the derived key, which is the other half of the
        // claim: a device that can say the words can read this.
        envelope.verify().unwrap();
    }
}

// ---------------------------------------------------------------------------
// The second device
// ---------------------------------------------------------------------------

/// Item 2's headline, and point 2 of the slice's definition of done: one
/// person's two devices, adopted from one recovery phrase, agree about a note's
/// contents *and* about its colour, labels, pin and placement.
fn two_devices_from_one_phrase_agree_about_a_note_and_its_placement<D: NoteDoc>() {
    let mut phone = in_memory::<D>();
    let phrase = phone.create_account().unwrap();

    // A note with something in it, coloured, labelled and pinned.
    let note = phone.create_note(NoteKind::Checklist).unwrap();
    phone.set_title(note, "Groceries").unwrap();
    phone.add_item(note, "oat milk").unwrap();
    phone.set_colour(note, Some(Colour::Orange)).unwrap();
    phone.set_pinned(note, true).unwrap();
    phone.add_tag(note, "errands").unwrap();

    // The second device: nothing on it but the twelve words.
    let mut laptop = in_memory::<D>();
    laptop.adopt_account(&phrase).unwrap();
    assert_eq!(laptop.account_id(), phone.account_id());

    deliver_account(&phone, &mut laptop);

    // The placement crossed.
    let there = laptop.placement(note);
    assert_eq!(there.colour, Some(Colour::Orange));
    assert!(there.pinned);
    assert_eq!(
        laptop
            .tags_on(note)
            .iter()
            .filter_map(|l| laptop.tag_name(*l))
            .collect::<Vec<_>>(),
        vec!["errands"]
    );
    assert_eq!(laptop.placement(note).order, phone.placement(note).order);

    // And so did the key, which is what makes the note readable rather than
    // merely present. The note's own envelopes travel the ordinary way.
    let key = laptop
        .keyring()
        .note_key(note)
        .unwrap()
        .expect("the key came with the account");
    for raw in phone.store().load(note).unwrap() {
        laptop.apply_remote(note, &raw).unwrap();
    }
    let view = laptop.view(note).unwrap();
    assert_eq!(view.title, "Groceries");
    assert_eq!(
        view.items
            .iter()
            .map(|i| i.text.as_str())
            .collect::<Vec<_>>(),
        vec!["oat milk"]
    );
    drop(key);
}

/// Without the key register the second device would hold ciphertext it could
/// never open — a note it can see the existence of and not the contents. This
/// is that register, isolated.
fn a_note_key_reaches_the_second_device<D: NoteDoc>() {
    let mut phone = in_memory::<D>();
    let phrase = phone.create_account().unwrap();
    let note = phone.create_note(NoteKind::Checklist).unwrap();

    let mut laptop = in_memory::<D>();
    laptop.adopt_account(&phrase).unwrap();
    assert!(laptop.keyring().note_key(note).unwrap().is_none());

    deliver_account(&phone, &mut laptop);

    assert_eq!(
        laptop.keyring().note_key(note).unwrap().map(|k| k.0),
        phone.keyring().note_key(note).unwrap().map(|k| k.0),
        "the same key, not merely some key"
    );
}

/// A note that existed before the account did must come along too. This is the
/// sweep in `record_held_note_keys`, and it is the ordinary case: a person uses
/// the app first and sets up sync afterwards.
fn notes_made_before_the_account_come_with_it<D: NoteDoc>() {
    let mut phone = in_memory::<D>();
    let note = phone.create_note(NoteKind::Checklist).unwrap();
    phone.set_title(note, "Before").unwrap();
    phone.set_colour(note, Some(Colour::Teal)).unwrap();
    let phrase = phone.create_account().unwrap();

    let mut laptop = in_memory::<D>();
    laptop.adopt_account(&phrase).unwrap();
    deliver_account(&phone, &mut laptop);

    assert!(laptop.keyring().note_key(note).unwrap().is_some());
    assert_eq!(laptop.placement(note).colour, Some(Colour::Teal));
}

/// The document survives the move with everything it said. A person who had
/// coloured half their notes before setting up an account keeps those colours.
fn the_placements_a_device_had_survive_the_move<D: NoteDoc>() {
    let mut core = in_memory::<D>();
    let a = core.create_note(NoteKind::Checklist).unwrap();
    let b = core.create_note(NoteKind::Checklist).unwrap();
    core.set_colour(a, Some(Colour::Red)).unwrap();
    core.set_pinned(b, true).unwrap();
    core.add_tag(a, "errands").unwrap();
    let before = core.account().clone();

    core.create_account().unwrap();

    assert_eq!(core.account(), &before, "the move said nothing new");
    assert_eq!(core.placement(a).colour, Some(Colour::Red));
    assert!(core.placement(b).pinned);
}

/// Adoption is idempotent in the way that matters: a phrase typed twice is one
/// account, not two, and a device that already belongs to an account refuses to
/// be moved to another rather than stranding the first.
fn a_device_with_an_account_refuses_a_second_one<D: NoteDoc>() {
    let mut core = in_memory::<D>();
    let phrase = core.create_account().unwrap();
    let id = core.account_id().unwrap();

    assert!(core.create_account().is_err());
    assert!(core.adopt_account(&phrase).is_err());
    assert!(core.adopt_account(&RecoveryPhrase::generate()).is_err());
    assert_eq!(core.account_id(), Some(id), "and nothing moved");
}

/// Adoption fills gaps and does not resolve disagreements. A key this device
/// already holds is what opens the envelopes it already has, so replacing it
/// with one the document names would lose the note rather than update it.
///
/// This is deliberately *not* rotation. Item 4 has to revisit exactly this
/// line, and will need re-encryption and a way to tell which key an envelope
/// used before it can — see `NoteCore::adopt_note_keys`.
fn an_arriving_key_never_replaces_one_this_device_holds<D: NoteDoc>() {
    let mut phone = in_memory::<D>();
    let phrase = phone.create_account().unwrap();
    let note = phone.create_note(NoteKind::Checklist).unwrap();

    // The laptop met this note the other way first: somebody shared it, so it
    // has a key of its own for that ID before the account document arrives.
    let mut laptop = in_memory::<D>();
    laptop.adopt_account(&phrase).unwrap();
    let mine = easynotes_core::crypto::NoteKey::random();
    let blob = easynotes_core::crypto::export_share_blob(&note, &mine);
    laptop.import_share_blob(&blob).unwrap();

    deliver_account(&phone, &mut laptop);

    assert_eq!(
        laptop.keyring().note_key(note).unwrap().unwrap().0,
        mine.0,
        "what this device holds is what opens its envelopes"
    );
}

/// A forgotten note takes its key out of the account document, so the next
/// device set up from this phrase never learns it. Otherwise emptying the trash
/// would remove the note here and hand it back there, which is the opposite of
/// what the gesture promises.
///
/// A marker removes the key from devices that already adopted it, not only from
/// devices set up after deletion.
fn forgetting_a_note_takes_its_key_out_of_the_account<D: NoteDoc>() {
    let mut phone = in_memory::<D>();
    let phrase = phone.create_account().unwrap();
    let note = phone.create_note(NoteKind::Checklist).unwrap();
    phone.set_trashed(note, true).unwrap();
    phone.forget_note(note).unwrap();

    // A device adopted after the fact sees the marker before key adoption.
    let mut laptop = in_memory::<D>();
    laptop.adopt_account(&phrase).unwrap();
    deliver_account(&phone, &mut laptop);

    assert!(
        !easynotes_core::crdt::account::note_access_of(phone.account_entries()).contains_key(&note),
        "the register is gone from the document"
    );
    assert!(
        laptop.keyring().note_key(note).unwrap().is_none(),
        "so a device that only ever saw the finished document never had it"
    );
    // The historical key envelope is still in the replay, but the marker takes
    // precedence over it.
}

/// A device that adopted a key before deletion purges it when the marker arrives.
fn a_device_that_already_had_the_key_purges_it_after_account_deletion<D: NoteDoc>() {
    let mut phone = in_memory::<D>();
    let phrase = phone.create_account().unwrap();
    let note = phone.create_note(NoteKind::Checklist).unwrap();

    let mut laptop = in_memory::<D>();
    laptop.adopt_account(&phrase).unwrap();
    deliver_account(&phone, &mut laptop);
    assert!(laptop.keyring().note_key(note).unwrap().is_some());

    phone.set_trashed(note, true).unwrap();
    phone.forget_note(note).unwrap();
    deliver_account(&phone, &mut laptop);

    assert!(
        laptop.keyring().note_key(note).unwrap().is_none(),
        "the account deletion marker removes previously adopted keys too"
    );
}

/// The reserved ID stays reserved after the move. A peer on an older build, or
/// a hostile one, must not be able to hand this device its own account document
/// as a note.
fn the_reserved_id_is_still_refused_on_a_device_with_an_account<D: NoteDoc>() {
    let mut core = in_memory::<D>();
    core.create_account().unwrap();
    let id = core.account_id().unwrap();

    assert!(core.open_note(ACCOUNT_DOC).is_err());
    assert!(core.open_note(id).is_err(), "nor the derived one");
    assert!(core.forget_note(ACCOUNT_DOC).is_err());
    assert!(core.forget_note(id).is_err());

    // Nor by share blob, under either address.
    for claimed in [ACCOUNT_DOC, id] {
        let blob = easynotes_core::crypto::export_share_blob(
            &claimed,
            &easynotes_core::crypto::NoteKey::random(),
        );
        assert!(core.import_share_blob(&blob).is_err());
    }
}

/// An envelope for somebody else's account document is refused, rather than
/// being appended to this one's log where it would be replayed for ever.
fn an_envelope_for_another_account_is_refused<D: NoteDoc>() {
    let mut mine = in_memory::<D>();
    mine.create_account().unwrap();
    let mut yours = in_memory::<D>();
    yours.create_account().unwrap();
    yours.create_note(NoteKind::Checklist).unwrap();

    let stranger = yours.store().load(yours.account_id().unwrap()).unwrap();
    let before = mine.store().load(mine.account_id().unwrap()).unwrap().len();
    for raw in &stranger {
        assert!(mine.apply_remote_account(raw).is_err());
    }
    assert_eq!(
        mine.store().load(mine.account_id().unwrap()).unwrap().len(),
        before
    );
}

/// Nothing can have arrived for a document that has never been on a relay.
fn a_device_with_no_account_takes_no_account_updates<D: NoteDoc>() {
    let mut phone = in_memory::<D>();
    phone.create_account().unwrap();
    let envelope = phone
        .store()
        .load(phone.account_id().unwrap())
        .unwrap()
        .remove(0);

    let mut fresh = in_memory::<D>();
    assert!(fresh.apply_remote_account(&envelope).is_err());
}

/// Delivering the same envelope twice changes nothing and grows nothing — the
/// property every other log in this project holds, for the same reason: a relay
/// reconnect replays.
fn a_repeated_account_envelope_is_dropped<D: NoteDoc>() {
    let mut phone = in_memory::<D>();
    let phrase = phone.create_account().unwrap();
    phone.create_note(NoteKind::Checklist).unwrap();

    let mut laptop = in_memory::<D>();
    laptop.adopt_account(&phrase).unwrap();
    deliver_account(&phone, &mut laptop);
    let settled = laptop.account().clone();
    let logged = laptop
        .store()
        .load(laptop.account_id().unwrap())
        .unwrap()
        .len();

    deliver_account(&phone, &mut laptop);
    deliver_account(&phone, &mut laptop);

    assert_eq!(laptop.account(), &settled);
    assert_eq!(
        laptop
            .store()
            .load(laptop.account_id().unwrap())
            .unwrap()
            .len(),
        logged
    );
}

/// Both devices editing, both directions. This is the convergence property the
/// account document was built for and could not be given until now —
/// `two_devices_keep_both_placements` held it against two bare `AccountDoc`s,
/// and this holds it against two whole cores over real envelopes.
fn two_devices_editing_at_once_converge<D: NoteDoc>() {
    let mut phone = in_memory::<D>();
    let phrase = phone.create_account().unwrap();
    let note = phone.create_note(NoteKind::Checklist).unwrap();

    let mut laptop = in_memory::<D>();
    laptop.adopt_account(&phrase).unwrap();
    deliver_account(&phone, &mut laptop);

    // Each changes a different field, neither having seen the other.
    phone.set_pinned(note, true).unwrap();
    laptop.set_colour(note, Some(Colour::Purple)).unwrap();

    deliver_account(&phone, &mut laptop);
    deliver_account(&laptop, &mut phone);

    for core in [&phone, &laptop] {
        let p = core.placement(note);
        assert!(p.pinned, "the phone's pin survived");
        assert_eq!(p.colour, Some(Colour::Purple), "and the laptop's colour");
    }
    assert_eq!(phone.account(), laptop.account());
}

/// And it survives the disk, on the store the app actually uses.
fn an_adopted_account_survives_a_relaunch<D: NoteDoc>() {
    let phone_dir = tempfile::tempdir().unwrap();
    let laptop_dir = tempfile::tempdir().unwrap();

    let (phrase, note) = {
        let mut phone = on_disk::<D>(phone_dir.path());
        let phrase = phone.create_account().unwrap();
        let note = phone.create_note(NoteKind::Checklist).unwrap();
        phone.set_colour(note, Some(Colour::Green)).unwrap();
        (phrase, note)
    };

    let envelopes = {
        let phone = on_disk::<D>(phone_dir.path());
        phone.store().load(phone.account_id().unwrap()).unwrap()
    };

    {
        let mut laptop = on_disk::<D>(laptop_dir.path());
        laptop.adopt_account(&phrase).unwrap();
        for raw in &envelopes {
            laptop.apply_remote_account(raw).unwrap();
        }
        assert_eq!(laptop.placement(note).colour, Some(Colour::Green));
    }

    // Killed and reopened: the account, the placement and the key are all still
    // there, read off the disk rather than out of memory.
    let laptop = on_disk::<D>(laptop_dir.path());
    assert!(laptop.has_account());
    assert_eq!(laptop.placement(note).colour, Some(Colour::Green));
    assert!(laptop.keyring().note_key(note).unwrap().is_some());
}

/// A note this device cannot read is not a card in the grid, and gaining the
/// key is what gives it one. The list has to notice.
fn a_note_appears_once_its_key_arrives<D: NoteDoc>() {
    let mut phone = in_memory::<D>();
    let phrase = phone.create_account().unwrap();
    let note = phone.create_note(NoteKind::Checklist).unwrap();
    phone.set_title(note, "Groceries").unwrap();

    let mut laptop = in_memory::<D>();
    laptop.adopt_account(&phrase).unwrap();

    // The note's envelopes arrive before the account document does, which is a
    // perfectly ordinary race: two sockets, no ordering between them.
    for raw in phone.store().load(note).unwrap() {
        // No key yet, so this cannot be applied — and must not be fatal.
        let _ = laptop.apply_remote(note, &raw);
    }
    assert!(!laptop.notes().unwrap().iter().any(|s| s.id == note));

    deliver_account(&phone, &mut laptop);
    for raw in phone.store().load(note).unwrap() {
        laptop.apply_remote(note, &raw).unwrap();
    }
    assert!(laptop.notes().unwrap().iter().any(|s| s.id == note));
}

/// A stranger's note ID must not be readable just because the account document
/// mentions a key. This checks the one direction that matters: the document is
/// sealed under a key only the phrase derives, so a device with the wrong
/// phrase gets nothing at all rather than getting keys.
fn the_wrong_phrase_opens_nothing<D: NoteDoc>() {
    let mut phone = in_memory::<D>();
    phone.create_account().unwrap();
    phone.create_note(NoteKind::Checklist).unwrap();
    let envelopes = phone.store().load(phone.account_id().unwrap()).unwrap();

    let mut impostor = in_memory::<D>();
    impostor.adopt_account(&RecoveryPhrase::generate()).unwrap();
    for raw in &envelopes {
        assert!(
            impostor.apply_remote_account(raw).is_err(),
            "a different phrase is a different document and a different key"
        );
    }
    assert!(impostor.account().placements.is_empty());
}

/// The account document is not readable off the disk, the same claim
/// `nothing_readable_is_in_the_database_or_its_wal` makes for notes. A note key
/// now lives in this document, so this is the test that putting it there did not
/// put it in the clear.
fn a_note_key_is_not_readable_in_the_account_log<D: NoteDoc>() {
    let mut core = in_memory::<D>();
    core.create_account().unwrap();
    let note = core.create_note(NoteKind::Checklist).unwrap();
    let key = core.keyring().note_key(note).unwrap().unwrap();

    for raw in core.store().load(core.account_id().unwrap()).unwrap() {
        assert!(
            !raw.windows(key.0.len()).any(|w| w == key.0),
            "a note key is in the ciphertext or it is nowhere"
        );
    }
}

macro_rules! backend_suite {
    ($name:ident, $backend:ty) => {
        mod $name {
            #[test]
            fn an_account_moves_the_document_off_the_reserved_id() {
                super::an_account_moves_the_document_off_the_reserved_id::<$backend>()
            }
            #[test]
            fn two_phrases_are_two_documents() {
                super::two_phrases_are_two_documents::<$backend>()
            }
            #[test]
            fn a_device_finds_its_account_again_after_a_relaunch() {
                super::a_device_finds_its_account_again_after_a_relaunch::<$backend>()
            }
            #[test]
            fn the_moved_log_is_addressed_to_the_derived_id() {
                super::the_moved_log_is_addressed_to_the_derived_id::<$backend>()
            }
            #[test]
            fn two_devices_from_one_phrase_agree_about_a_note_and_its_placement() {
                super::two_devices_from_one_phrase_agree_about_a_note_and_its_placement::<$backend>(
                )
            }
            #[test]
            fn a_note_key_reaches_the_second_device() {
                super::a_note_key_reaches_the_second_device::<$backend>()
            }
            #[test]
            fn notes_made_before_the_account_come_with_it() {
                super::notes_made_before_the_account_come_with_it::<$backend>()
            }
            #[test]
            fn the_placements_a_device_had_survive_the_move() {
                super::the_placements_a_device_had_survive_the_move::<$backend>()
            }
            #[test]
            fn a_device_with_an_account_refuses_a_second_one() {
                super::a_device_with_an_account_refuses_a_second_one::<$backend>()
            }
            #[test]
            fn an_arriving_key_never_replaces_one_this_device_holds() {
                super::an_arriving_key_never_replaces_one_this_device_holds::<$backend>()
            }
            #[test]
            fn forgetting_a_note_takes_its_key_out_of_the_account() {
                super::forgetting_a_note_takes_its_key_out_of_the_account::<$backend>()
            }
            #[test]
            fn a_device_that_already_had_the_key_purges_it_after_account_deletion() {
                super::a_device_that_already_had_the_key_purges_it_after_account_deletion::<$backend>()
            }
            #[test]
            fn the_reserved_id_is_still_refused_on_a_device_with_an_account() {
                super::the_reserved_id_is_still_refused_on_a_device_with_an_account::<$backend>()
            }
            #[test]
            fn an_envelope_for_another_account_is_refused() {
                super::an_envelope_for_another_account_is_refused::<$backend>()
            }
            #[test]
            fn a_device_with_no_account_takes_no_account_updates() {
                super::a_device_with_no_account_takes_no_account_updates::<$backend>()
            }
            #[test]
            fn a_repeated_account_envelope_is_dropped() {
                super::a_repeated_account_envelope_is_dropped::<$backend>()
            }
            #[test]
            fn two_devices_editing_at_once_converge() {
                super::two_devices_editing_at_once_converge::<$backend>()
            }
            #[test]
            fn an_adopted_account_survives_a_relaunch() {
                super::an_adopted_account_survives_a_relaunch::<$backend>()
            }
            #[test]
            fn a_note_appears_once_its_key_arrives() {
                super::a_note_appears_once_its_key_arrives::<$backend>()
            }
            #[test]
            fn the_wrong_phrase_opens_nothing() {
                super::the_wrong_phrase_opens_nothing::<$backend>()
            }
            #[test]
            fn a_note_key_is_not_readable_in_the_account_log() {
                super::a_note_key_is_not_readable_in_the_account_log::<$backend>()
            }
        }
    };
}

#[cfg(feature = "automerge-backend")]
backend_suite!(automerge, easynotes_core::crdt::AutomergeNote);
#[cfg(feature = "yrs-backend")]
backend_suite!(yrs, easynotes_core::crdt::YrsNote);

/// Not backend-specific: the ID is a note ID, so it must be one nothing else can
/// be confused for.
#[test]
fn a_derived_document_id_is_not_a_reserved_one() {
    for _ in 0..64 {
        let id = RecoveryPhrase::generate().root().document_id();
        assert_ne!(id, ACCOUNT_DOC);
        assert_ne!(id, NoteId::from_bytes([0xFF; 16]));
    }
}
