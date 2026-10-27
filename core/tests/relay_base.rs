//! A base for the relay: what a device hands over when the relay asks it to
//! compact a note's log ([ADR 0024](../../docs/decisions.md#adr-0024)).
//!
//! The relay drops what a base covers, so everything here is about whether a
//! base is the whole of what it replaces: that a device which only ever sees
//! the base arrives at the same document, that the device which made it never
//! reuses its counter, and that a device which cannot vouch for everything it
//! holds makes none. `relay/tests` runs the same over a socket.

use easynotes_core::account::RecoveryPhrase;
use easynotes_core::crdt::{Colour, NoteDoc};
use easynotes_core::crypto::{DeviceIdentity, Envelope, NoteKey};
use easynotes_core::note::{NoteCore, NoteKind};
use easynotes_core::store::{MemoryKeyring, MemoryStore, OpStore};

type Memory<D> = NoteCore<D, MemoryStore, MemoryKeyring>;

fn in_memory<D: NoteDoc>() -> Memory<D> {
    NoteCore::open(MemoryStore::new(), MemoryKeyring::new()).unwrap()
}

fn a_base_is_the_whole_note_to_a_device_that_sees_nothing_else<D: NoteDoc>() {
    let mut a = in_memory::<D>();
    let note = a.create_note(NoteKind::Checklist).unwrap();
    a.set_title(note, "Groceries").unwrap();
    for item in ["milk", "bread", "eggs"] {
        a.add_item(note, item).unwrap();
    }
    let bread = a.view(note).unwrap().items[1].id;
    a.set_checked(note, bread, true).unwrap();

    let request = a.compaction(note, 7).unwrap().expect("a base");
    assert_eq!(request.covers, 7);
    assert!(request.verifies(note));
    assert_eq!(
        request.key,
        easynotes_core::compaction::public_key(
            note,
            &easynotes_core::crypto::import_share_blob(&a.export_share_blob(note).unwrap())
                .unwrap()
                .1
        )
    );

    let mut b = in_memory::<D>();
    b.import_share_blob(&a.export_share_blob(note).unwrap())
        .unwrap();
    b.apply_remote(note, &request.base).unwrap();
    assert_eq!(b.view(note).unwrap(), a.view(note).unwrap());

    // The base is not in the maker's log: its content already is.
    let log = a.store().load(note).unwrap();
    assert!(!log.contains(&request.base));

    // A relaunch must not hand the base's counter to the next edit: a peer
    // that has the base would take that edit for a replay and drop it.
    let counter = Envelope::decode(&request.base).unwrap().counter;
    a.close_all();
    a.open_note(note).unwrap();
    let (_, edit) = a.add_item(note, "butter").unwrap();
    assert!(Envelope::decode(&edit).unwrap().counter > counter);
    b.apply_remote(note, &edit).unwrap();
    assert_eq!(b.view(note).unwrap(), a.view(note).unwrap());
}

fn no_base_from_a_device_holding_what_it_cannot_read<D: NoteDoc>() {
    let mut a = in_memory::<D>();
    let note = a.create_note(NoteKind::Checklist).unwrap();
    a.add_item(note, "milk").unwrap();

    // Nothing to cover.
    assert!(a.compaction(note, 0).unwrap().is_none());

    // An envelope under a key this device has never held: authentic, kept,
    // and not in the document. A base from here would drop it for everyone.
    let stranger = DeviceIdentity::generate();
    let future = Envelope::seal(&stranger, note, &NoteKey::random(), 1, b"later")
        .unwrap()
        .encode();
    let _ = a.apply_remote(note, &future);
    assert!(a.store().load(note).unwrap().contains(&future));
    assert!(a.compaction(note, 3).unwrap().is_none());
}

fn the_account_base_is_the_account_log<D: NoteDoc>() {
    let mut a = in_memory::<D>();
    let phrase = a.create_account().unwrap();
    let note = a.create_note(NoteKind::Checklist).unwrap();
    for colour in [Colour::Red, Colour::Teal, Colour::Yellow] {
        a.set_colour(note, Some(colour)).unwrap();
    }
    let account = a.account_id().unwrap();
    let request = a.compaction(account, 9).unwrap().expect("a base");
    assert!(request.verifies(account));
    assert_eq!(a.store().load(account).unwrap(), vec![request.base.clone()]);

    let mut b = in_memory::<D>();
    b.adopt_account(&RecoveryPhrase::parse(&phrase.words()).unwrap())
        .unwrap();
    b.apply_remote_account(&request.base).unwrap();
    assert_eq!(b.placement(note).colour, Some(Colour::Yellow));

    // And the next edit is not mistaken for the base.
    a.set_colour(note, Some(Colour::Blue)).unwrap();
    for envelope in a.store().load(account).unwrap() {
        b.apply_remote_account(&envelope).unwrap();
    }
    assert_eq!(b.placement(note).colour, Some(Colour::Blue));
}

macro_rules! backend_suite {
    ($name:ident, $backend:ty) => {
        mod $name {
            #[test]
            fn a_base_is_the_whole_note_to_a_device_that_sees_nothing_else() {
                super::a_base_is_the_whole_note_to_a_device_that_sees_nothing_else::<$backend>()
            }
            #[test]
            fn no_base_from_a_device_holding_what_it_cannot_read() {
                super::no_base_from_a_device_holding_what_it_cannot_read::<$backend>()
            }
            #[test]
            fn the_account_base_is_the_account_log() {
                super::the_account_base_is_the_account_log::<$backend>()
            }
        }
    };
}

backend_suite!(yrs, easynotes_core::crdt::YrsNote);
backend_suite!(automerge, easynotes_core::crdt::AutomergeNote);
