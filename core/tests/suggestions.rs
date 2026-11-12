//! What this person adds over and over: the per-note history and the lines it
//! offers back.
//!
//! Three claims, and they are the reason the feature is allowed to exist at all:
//!
//! * it never touches the note — no envelope, no CRDT edit, nothing a partner or
//!   the relay can see;
//! * it is per note, so the history of one list is not a profile of a person;
//! * it is forgettable, and forgetting it is one call.
//!
//! The ranking itself is unit-tested in `crate::suggestions`; what is tested here
//! is the plumbing through a real core and the promises above.

use easynotes_core::crdt::NoteDoc;
use easynotes_core::ids::NoteId;
use easynotes_core::note::NoteCore;
use easynotes_core::store::{MemoryKeyring, MemoryStore};

type Core<D> = NoteCore<D, MemoryStore, MemoryKeyring>;

const DAY: i64 = 86_400;
const NOW: i64 = 400 * DAY;

fn core<D: NoteDoc>() -> Core<D> {
    NoteCore::open(MemoryStore::new(), MemoryKeyring::new()).unwrap()
}

fn pair<D: NoteDoc>() -> (Core<D>, Core<D>, NoteId) {
    let mut a = core::<D>();
    let id = a.create_note(Default::default()).unwrap();
    let blob = a.export_share_blob(id).unwrap();
    let mut b = core::<D>();
    assert_eq!(b.import_share_blob(&blob).unwrap(), id);
    let created: Vec<Vec<u8>> = a
        .unsent(id, None)
        .unwrap()
        .into_iter()
        .map(|(_, e)| e)
        .collect();
    for e in created {
        b.apply_remote(id, &e).unwrap();
    }
    (a, b, id)
}

// ---------------------------------------------------------------------------

fn what_was_added_comes_back_and_nothing_is_sent<D: NoteDoc>() {
    let (mut a, _, id) = pair::<D>();
    let before = a.unsent(id, None).unwrap().len();

    a.remember_item(id, "Milk", NOW).unwrap();
    a.remember_item(id, "Bread", NOW).unwrap();
    a.remember_item(id, "Milk", NOW + 60).unwrap();

    assert_eq!(
        a.suggestions(id, "", 5, NOW).unwrap(),
        ["Milk", "Bread"],
        "twice is a stronger offer than once"
    );
    assert_eq!(
        a.unsent(id, None).unwrap().len(),
        before,
        "remembering is this device's own: there is nothing to send"
    );
}

fn nothing_already_on_the_list_is_suggested<D: NoteDoc>() {
    let (mut a, _, id) = pair::<D>();
    a.add_item(id, "milk").unwrap();
    a.remember_item(id, "Milk", NOW).unwrap();
    a.remember_item(id, "Butter", NOW).unwrap();

    // Matching a line already on the list is not a suggestion, whatever case it
    // was remembered in.
    assert_eq!(a.suggestions(id, "", 5, NOW).unwrap(), ["Butter"]);
}

fn a_history_belongs_to_one_list<D: NoteDoc>() {
    let (mut a, _, id) = pair::<D>();
    let other = a.create_note(Default::default()).unwrap();
    a.remember_item(id, "Milk", NOW).unwrap();

    assert_eq!(a.suggestions(id, "mi", 5, NOW).unwrap(), ["Milk"]);
    assert!(
        a.suggestions(other, "mi", 5, NOW).unwrap().is_empty(),
        "another list learns nothing about this one"
    );
}

fn suggestions_turned_off_learn_nothing_and_offer_nothing<D: NoteDoc>() {
    let (mut a, _, id) = pair::<D>();
    a.remember_item(id, "Milk", NOW).unwrap();
    a.set_suggestions_enabled(false).unwrap();

    a.remember_item(id, "Bread", NOW).unwrap();
    assert!(a.suggestions(id, "", 5, NOW).unwrap().is_empty());

    // And the setting is a placement, not a note: it travels to this person's
    // other devices and to nobody else's.
    a.set_suggestions_enabled(true).unwrap();
    assert_eq!(
        a.suggestions(id, "", 5, NOW).unwrap(),
        ["Milk"],
        "what was learned before the switch is still there"
    );
}

fn forgetting_one_list_forgets_only_that_list<D: NoteDoc>() {
    let (mut a, _, id) = pair::<D>();
    let other = a.create_note(Default::default()).unwrap();
    a.remember_item(id, "Milk", NOW).unwrap();
    a.remember_item(other, "Cable ties", NOW).unwrap();

    a.forget_history(id).unwrap();

    assert!(a.suggestions(id, "", 5, NOW).unwrap().is_empty());
    assert_eq!(a.suggestions(other, "", 5, NOW).unwrap(), ["Cable ties"]);
}

fn forgetting_everything_says_how_much_there_was<D: NoteDoc>() {
    let (mut a, _, id) = pair::<D>();
    let other = a.create_note(Default::default()).unwrap();
    let empty = a.create_note(Default::default()).unwrap();
    a.remember_item(id, "Milk", NOW).unwrap();
    a.remember_item(other, "Cable ties", NOW).unwrap();

    assert_eq!(a.forget_all_history().unwrap(), 2);
    assert!(a.suggestions(id, "", 5, NOW).unwrap().is_empty());
    assert!(a.suggestions(other, "", 5, NOW).unwrap().is_empty());
    assert!(
        a.suggestions(empty, "", 5, NOW).unwrap().is_empty(),
        "a list that never had a history is not an error"
    );
    // Nothing left to forget is not an error either.
    assert_eq!(a.forget_all_history().unwrap(), 0);
}

fn a_note_that_loses_its_history_on_deletion_loses_it_for_good<D: NoteDoc>() {
    // Deleting a note for good clears every metadata row it had, the history
    // among them — the promise permanent deletion already makes. The note ID is
    // then unknown, so nothing can ask for the history back.
    let (mut a, _, id) = pair::<D>();
    a.remember_item(id, "Milk", NOW).unwrap();
    a.forget_note(id).unwrap();

    assert!(a.suggestions(id, "", 5, NOW).is_err());
}

fn the_newest_spelling_is_the_one_offered<D: NoteDoc>() {
    let (mut a, _, id) = pair::<D>();
    a.remember_item(id, "oat milk", NOW).unwrap();
    a.remember_item(id, "Oat milk", NOW + 60).unwrap();

    assert_eq!(a.suggestions(id, "o", 5, NOW + 60).unwrap(), ["Oat milk"]);
}

// ---------------------------------------------------------------------------

macro_rules! backend_suite {
    ($mod_name:ident, $backend:ty, [$($name:ident),* $(,)?]) => {
        mod $mod_name {
            $(
                #[test]
                fn $name() {
                    super::$name::<$backend>()
                }
            )*
        }
    };
}

macro_rules! both_backends {
    ($($name:ident),* $(,)?) => {
        #[cfg(feature = "automerge-backend")]
        backend_suite!(automerge, easynotes_core::crdt::AutomergeNote, [$($name),*]);
        #[cfg(feature = "yrs-backend")]
        backend_suite!(yrs, easynotes_core::crdt::YrsNote, [$($name),*]);
    };
}

both_backends!(
    what_was_added_comes_back_and_nothing_is_sent,
    nothing_already_on_the_list_is_suggested,
    a_history_belongs_to_one_list,
    suggestions_turned_off_learn_nothing_and_offer_nothing,
    forgetting_one_list_forgets_only_that_list,
    forgetting_everything_says_how_much_there_was,
    a_note_that_loses_its_history_on_deletion_loses_it_for_good,
    the_newest_spelling_is_the_one_offered,
);
