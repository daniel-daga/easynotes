//! Changing a selection of notes at once.
//!
//! The grid lets a person select several cards and archive, colour or label
//! them together. What has to hold is that a selection is one change: one
//! envelope in the account document's log, so that a peer never sees half of it
//! and the grid is redrawn once rather than once per card.

use easynotes_core::crdt::{Colour, NoteDoc};
use easynotes_core::ids::NoteId;
use easynotes_core::note::{NoteCore, NoteKind, ACCOUNT_DOC};
use easynotes_core::store::{MemoryKeyring, MemoryStore, OpStore};

type Memory<D> = NoteCore<D, MemoryStore, MemoryKeyring>;

fn in_memory<D: NoteDoc>() -> Memory<D> {
    NoteCore::open(MemoryStore::new(), MemoryKeyring::new()).unwrap()
}

fn three<D: NoteDoc>(core: &mut Memory<D>) -> [NoteId; 3] {
    [
        core.create_note(NoteKind::Checklist).unwrap(),
        core.create_note(NoteKind::Text).unwrap(),
        core.create_note(NoteKind::Checklist).unwrap(),
    ]
}

fn logged<D: NoteDoc>(core: &Memory<D>) -> usize {
    core.store().load(ACCOUNT_DOC).unwrap().len()
}

// ---------------------------------------------------------------------------

fn colouring_a_selection_is_one_envelope<D: NoteDoc>() {
    let mut core = in_memory::<D>();
    let notes = three(&mut core);
    let before = logged(&core);

    core.set_colour_many(&notes, Some(Colour::Teal)).unwrap();

    assert_eq!(logged(&core), before + 1);
    for note in notes {
        assert_eq!(core.placement(note).colour, Some(Colour::Teal));
    }
}

fn pinning_a_selection_is_one_envelope<D: NoteDoc>() {
    let mut core = in_memory::<D>();
    let notes = three(&mut core);
    let before = logged(&core);

    core.set_pinned_many(&notes, true).unwrap();

    assert_eq!(logged(&core), before + 1);
    assert!(notes.iter().all(|note| core.placement(*note).pinned));
}

/// The unpinning that archiving does goes in the same envelope as the archive,
/// so no peer sees a note both archived and pinned.
fn archiving_a_selection_unpins_it_in_the_same_envelope<D: NoteDoc>() {
    let mut core = in_memory::<D>();
    let notes = three(&mut core);
    core.set_pinned(notes[0], true).unwrap();
    let before = logged(&core);

    core.set_archived_many(&notes, true).unwrap();

    assert_eq!(logged(&core), before + 1);
    for note in notes {
        let placement = core.placement(note);
        assert!(placement.archived);
        assert!(!placement.pinned);
    }

    core.set_archived_many(&notes, false).unwrap();
    assert!(notes.iter().all(|note| core.placement(*note).in_grid()));
}

fn trashing_a_selection_unpins_it_in_the_same_envelope<D: NoteDoc>() {
    let mut core = in_memory::<D>();
    let notes = three(&mut core);
    core.set_pinned(notes[1], true).unwrap();
    let before = logged(&core);

    core.set_trashed_many(&notes, true).unwrap();

    assert_eq!(logged(&core), before + 1);
    for note in notes {
        let placement = core.placement(note);
        assert!(placement.trashed);
        assert!(!placement.pinned);
    }
}

/// A label nobody has yet is made in the same envelope that puts it on the
/// selection, and only once.
fn labelling_a_selection_makes_the_label_once<D: NoteDoc>() {
    let mut core = in_memory::<D>();
    let notes = three(&mut core);
    let before = logged(&core);

    let label = core.add_tag_many(&notes, "  Recipes ").unwrap().unwrap();

    assert_eq!(logged(&core), before + 1);
    let tags = core.tags();
    assert_eq!(tags.len(), 1);
    assert_eq!(tags[0], (label, "Recipes".to_string(), 3));
    for note in notes {
        assert!(core.placement(note).tags.contains(&label));
    }

    // An existing label, named in another case, is the same label.
    let again = core.add_tag_many(&notes[..1], "recipes").unwrap();
    assert_eq!(again, Some(label));
    assert_eq!(core.tags().len(), 1);
}

fn a_blank_label_changes_nothing<D: NoteDoc>() {
    let mut core = in_memory::<D>();
    let notes = three(&mut core);
    let before = logged(&core);

    assert_eq!(core.add_tag_many(&notes, "   ").unwrap(), None);
    assert_eq!(logged(&core), before);
}

fn unlabelling_a_selection_skips_the_notes_without_it<D: NoteDoc>() {
    let mut core = in_memory::<D>();
    let notes = three(&mut core);
    let label = core.add_tag_many(&notes[..2], "work").unwrap().unwrap();
    let before = logged(&core);

    core.remove_tag_many(&notes, label).unwrap();

    assert_eq!(logged(&core), before + 1);
    assert!(notes
        .iter()
        .all(|note| core.placement(*note).tags.is_empty()));
    // The label stays, with nothing on it.
    assert_eq!(core.tags()[0].2, 0);

    // Taking it off notes that do not have it writes nothing.
    core.remove_tag_many(&notes, label).unwrap();
    assert_eq!(logged(&core), before + 1);
}

/// A note named twice is changed once.
fn a_note_named_twice_is_one_note<D: NoteDoc>() {
    let mut core = in_memory::<D>();
    let [a, b, _] = three(&mut core);
    let label = core.add_tag_many(&[a, a, b], "twice").unwrap().unwrap();
    assert_eq!(core.tags(), [(label, "twice".to_string(), 2)]);
}

/// A selection that names a deleted note changes nothing at all, rather than
/// changing the notes that came before it.
fn a_batch_naming_a_deleted_note_writes_nothing<D: NoteDoc>() {
    let mut core = in_memory::<D>();
    let [a, gone, c] = three(&mut core);
    core.forget_note(gone).unwrap();
    let before = logged(&core);

    assert!(core
        .set_colour_many(&[a, gone, c], Some(Colour::Red))
        .is_err());

    assert_eq!(logged(&core), before);
    assert_eq!(core.placement(a).colour, None);
    assert_eq!(core.placement(c).colour, None);
}

/// Forgetting is the one bulk change that is not one envelope: each note keeps
/// its own deletion marker, which is its recovery record.
fn forgetting_a_selection_marks_each_note<D: NoteDoc>() {
    let mut core = in_memory::<D>();
    let [a, b, kept] = three(&mut core);

    assert_eq!(core.forget_notes(&[a, b, a]).unwrap(), 2);

    assert!(core.is_deleted_note(a));
    assert!(core.is_deleted_note(b));
    assert!(!core.is_deleted_note(kept));
    let left: Vec<NoteId> = core.notes().unwrap().iter().map(|s| s.id).collect();
    assert_eq!(left, [kept]);
}

macro_rules! backend_suite {
    ($name:ident, $backend:ty) => {
        mod $name {
            #[test]
            fn colouring_a_selection_is_one_envelope() {
                super::colouring_a_selection_is_one_envelope::<$backend>()
            }
            #[test]
            fn pinning_a_selection_is_one_envelope() {
                super::pinning_a_selection_is_one_envelope::<$backend>()
            }
            #[test]
            fn archiving_a_selection_unpins_it_in_the_same_envelope() {
                super::archiving_a_selection_unpins_it_in_the_same_envelope::<$backend>()
            }
            #[test]
            fn trashing_a_selection_unpins_it_in_the_same_envelope() {
                super::trashing_a_selection_unpins_it_in_the_same_envelope::<$backend>()
            }
            #[test]
            fn labelling_a_selection_makes_the_label_once() {
                super::labelling_a_selection_makes_the_label_once::<$backend>()
            }
            #[test]
            fn a_blank_label_changes_nothing() {
                super::a_blank_label_changes_nothing::<$backend>()
            }
            #[test]
            fn unlabelling_a_selection_skips_the_notes_without_it() {
                super::unlabelling_a_selection_skips_the_notes_without_it::<$backend>()
            }
            #[test]
            fn a_note_named_twice_is_one_note() {
                super::a_note_named_twice_is_one_note::<$backend>()
            }
            #[test]
            fn a_batch_naming_a_deleted_note_writes_nothing() {
                super::a_batch_naming_a_deleted_note_writes_nothing::<$backend>()
            }
            #[test]
            fn forgetting_a_selection_marks_each_note() {
                super::forgetting_a_selection_marks_each_note::<$backend>()
            }
        }
    };
}

#[cfg(feature = "automerge-backend")]
backend_suite!(automerge, easynotes_core::crdt::AutomergeNote);
#[cfg(feature = "yrs-backend")]
backend_suite!(yrs, easynotes_core::crdt::YrsNote);
