//! Every view the core publishes carries a revision, and every edit can learn
//! the revision it landed at.
//!
//! That is what a shared text field compares an incoming view against: anything
//! at or below the revision of its own last write is an echo, and anything above
//! is somebody else's change. The counter does not have to mean anything, only
//! never go backwards — and it must not be confused by a merge, so both
//! backends are checked.

use easynotes_core::crdt::NoteDoc;
use easynotes_core::ids::NoteId;
use easynotes_core::note::{NoteCore, NoteKind};
use easynotes_core::store::{MemoryKeyring, MemoryStore};

type Core<D> = NoteCore<D, MemoryStore, MemoryKeyring>;

fn core<D: NoteDoc>() -> Core<D> {
    NoteCore::open(MemoryStore::new(), MemoryKeyring::new()).unwrap()
}

fn pair<D: NoteDoc>() -> (Core<D>, Core<D>, NoteId) {
    let mut a = core::<D>();
    let id = a.create_note(NoteKind::Checklist).unwrap();
    let blob = a.export_share_blob(id).unwrap();
    let mut b = core::<D>();
    assert_eq!(b.import_share_blob(&blob).unwrap(), id);
    (a, b, id)
}

/// A fresh note starts at one, and the view and the accessor agree.
fn a_new_note_starts_at_one<D: NoteDoc>() {
    let mut a = core::<D>();
    let id = a.create_note(NoteKind::Checklist).unwrap();
    assert_eq!(a.rev(id).unwrap(), 1);
    assert_eq!(a.view_at(id).unwrap().rev, 1);
    assert_eq!(a.view_at(id).unwrap().view, a.view(id).unwrap());
}

/// Each local edit bumps the revision, and leaves exactly the revision it
/// produced behind — the value the field that made the edit records.
fn every_local_edit_bumps<D: NoteDoc>() {
    let mut a = core::<D>();
    let id = a.create_note(NoteKind::Checklist).unwrap();
    let mut last = a.rev(id).unwrap();

    let mut bump = |a: &mut Core<D>, what: &str| {
        let now = a.rev(id).unwrap();
        assert!(
            now > last,
            "{what} should have bumped the revision past {last}, but it is {now}"
        );
        last = now;
    };

    a.set_title(id, "Groceries").unwrap();
    bump(&mut a, "a title");
    let (item, envelope) = a.add_item(id, "milk").unwrap();
    drop(envelope);
    bump(&mut a, "an added item");
    a.set_item_text(id, item, "milk and eggs").unwrap();
    bump(&mut a, "an item's text");
    a.set_checked(id, item, true).unwrap();
    bump(&mut a, "a tick");
}

/// A body send that would leave the text as it is returns `None` and publishes
/// nothing, so a field that sends what the core already holds cannot make its
/// own next echo look like somebody else's change.
fn a_no_op_body_send_does_not_bump<D: NoteDoc>() {
    let mut a = core::<D>();
    let id = a.create_note(NoteKind::Checklist).unwrap();
    let after_create = a.rev(id).unwrap();

    assert_eq!(a.set_body(id, "").unwrap(), None);
    assert_eq!(a.rev(id).unwrap(), after_create);
}

/// A view published for an edit that changed nothing still says the same
/// thing, so a field that adopts it moves nothing and keeps the revision it
/// sent from.
fn a_no_op_view_says_the_same<D: NoteDoc>() {
    let mut a = core::<D>();
    let id = a.create_note(NoteKind::Checklist).unwrap();
    let (item, _) = a.add_item(id, "milk").unwrap();
    let before = a.view(id).unwrap().items[0].text.clone();

    a.set_item_text(id, item, "milk").unwrap();
    assert_eq!(a.view(id).unwrap().items[0].text, before);
}

/// A merge from another device is a new view, one bump and no more.
fn a_remote_edit_bumps_once<D: NoteDoc>() {
    let (mut a, mut b, id) = pair::<D>();
    let (item, envelope) = a.add_item(id, "milk").unwrap();
    b.apply_remote(id, &envelope).unwrap();
    let after_one = b.rev(id).unwrap();
    assert!(after_one > 1);

    // A replay of the same envelope is not a change, so it does not publish.
    b.apply_remote(id, &envelope).unwrap();
    assert_eq!(b.rev(id).unwrap(), after_one);

    let envelope = a.set_item_text(id, item, "milk and eggs").unwrap();
    b.apply_remote(id, &envelope).unwrap();
    assert!(b.rev(id).unwrap() > after_one);
    assert_eq!(b.view(id).unwrap().items[0].text, "milk and eggs");
}

/// The counter is per note, not shared: editing one note must not make another
/// note's views look stale or fresh.
fn revisions_are_per_note<D: NoteDoc>() {
    let mut a = core::<D>();
    let first = a.create_note(NoteKind::Checklist).unwrap();
    let second = a.create_note(NoteKind::Checklist).unwrap();
    let before = a.rev(second).unwrap();

    a.set_title(first, "Groceries").unwrap();
    a.add_item(first, "milk").unwrap();

    assert_eq!(a.rev(second).unwrap(), before);
}

/// Two devices edit the same note and converge; each one's revisions moved and
/// neither one's counter is read from the other's, because it never travels.
fn two_devices_converge_with_their_own_counters<D: NoteDoc>() {
    let (mut a, mut b, id) = pair::<D>();
    let (item, envelope) = a.add_item(id, "milk").unwrap();
    b.apply_remote(id, &envelope).unwrap();

    let from_a = a.set_item_text(id, item, "milk and eggs").unwrap();
    // B types into the same line before hearing about A's edit.
    let from_b = b.set_item_text(id, item, "milk, eggs").unwrap();

    a.apply_remote(id, &from_b).unwrap();
    b.apply_remote(id, &from_a).unwrap();

    assert_eq!(a.view(id).unwrap(), b.view(id).unwrap());
    assert!(a.rev(id).unwrap() > 1);
    assert!(b.rev(id).unwrap() > 1);
}

/// The watcher's first value is the current view at the current revision, and
/// everything after it is strictly newer.
#[test]
fn watch_starts_at_the_current_revision_and_only_moves_up() {
    let mut a = core::<easynotes_core::crdt::AutomergeNote>();
    let id = a.create_note(NoteKind::Checklist).unwrap();
    let mut rx = a.watch(id).unwrap();
    let first = rx.borrow_and_update().clone();
    assert_eq!(first.rev, a.rev(id).unwrap());

    a.set_title(id, "Groceries").unwrap();
    let second = rx.borrow_and_update().clone();
    assert!(
        second.rev > first.rev,
        "a later view should have a later revision"
    );
    assert_eq!(second.view.title, "Groceries");
}

macro_rules! backend_suite {
    ($name:ident, $backend:ty) => {
        mod $name {
            #[test]
            fn a_new_note_starts_at_one() {
                super::a_new_note_starts_at_one::<$backend>()
            }
            #[test]
            fn every_local_edit_bumps() {
                super::every_local_edit_bumps::<$backend>()
            }
            #[test]
            fn a_no_op_body_send_does_not_bump() {
                super::a_no_op_body_send_does_not_bump::<$backend>()
            }
            #[test]
            fn a_no_op_view_says_the_same() {
                super::a_no_op_view_says_the_same::<$backend>()
            }
            #[test]
            fn a_remote_edit_bumps_once() {
                super::a_remote_edit_bumps_once::<$backend>()
            }
            #[test]
            fn revisions_are_per_note() {
                super::revisions_are_per_note::<$backend>()
            }
            #[test]
            fn two_devices_converge_with_their_own_counters() {
                super::two_devices_converge_with_their_own_counters::<$backend>()
            }
        }
    };
}

#[cfg(feature = "automerge-backend")]
backend_suite!(automerge, easynotes_core::crdt::AutomergeNote);
#[cfg(feature = "yrs-backend")]
backend_suite!(yrs, easynotes_core::crdt::YrsNote);
