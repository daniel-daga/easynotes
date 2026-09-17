//! The note list: several notes on one device, and what a list of them has to
//! keep true.
//!
//! Slice 1 and most of Slice 2 had exactly one note, so nothing until now held
//! the core to behaving when there are three. These are the properties a grid of
//! notes rests on — it shows every note the device holds, it shows what each one
//! currently says, and it holds still while you are looking at it.

use easynotes_core::crdt::NoteDoc;
use easynotes_core::crypto::MasterKey;
use easynotes_core::ids::NoteId;
use easynotes_core::note::{NoteCore, NoteKind, NoteSummary};
use easynotes_core::store::{
    master_key_from_file, MemoryKeyring, MemoryStore, OpStore, SqliteKeyring, SqliteStore,
};
use std::path::Path;

type Memory<D> = NoteCore<D, MemoryStore, MemoryKeyring>;

fn in_memory<D: NoteDoc>() -> Memory<D> {
    NoteCore::open(MemoryStore::new(), MemoryKeyring::new()).unwrap()
}

/// A core over the store the app actually uses, opened the way a relaunch opens
/// it: same directory, nothing carried over in memory.
fn on_disk<D: NoteDoc>(dir: &Path) -> NoteCore<D, SqliteStore, SqliteKeyring> {
    let master: MasterKey = master_key_from_file(dir.join("master.key")).unwrap();
    NoteCore::open(
        SqliteStore::open(dir.join("notes.db"), master.clone()).unwrap(),
        SqliteKeyring::open(dir.join("keys.db"), master).unwrap(),
    )
    .unwrap()
}

fn titles(summaries: &[NoteSummary]) -> Vec<&str> {
    summaries.iter().map(|s| s.title.as_str()).collect()
}

// ---------------------------------------------------------------------------

fn the_newest_note_is_first<D: NoteDoc>() {
    let mut core = in_memory::<D>();
    for title in ["groceries", "hardware", "books"] {
        let id = core.create_note(NoteKind::Checklist).unwrap();
        core.set_title(id, title).unwrap();
    }
    assert_eq!(
        titles(&core.notes().unwrap()),
        ["books", "hardware", "groceries"]
    );
}

fn a_summary_says_what_the_note_says<D: NoteDoc>() {
    let mut core = in_memory::<D>();
    let id = core.create_note(NoteKind::Checklist).unwrap();
    core.set_title(id, "groceries").unwrap();
    let (milk, _) = core.add_item(id, "milk").unwrap();
    core.add_item(id, "bread").unwrap();
    core.set_checked(id, milk, true).unwrap();

    let notes = core.notes().unwrap();
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].id, id);
    assert_eq!(notes[0].title, "groceries");
    assert_eq!(notes[0].items, 2);
    assert_eq!(notes[0].checked, 1);
    // What a card shows when the note has no title, which most of them do not.
    assert_eq!(notes[0].preview, ["milk", "bread"]);
}

/// A preview is a preview: a long list must not put its whole self through the
/// bridge every time someone ticks something off it.
fn a_preview_stops_at_a_few_items<D: NoteDoc>() {
    let mut core = in_memory::<D>();
    let id = core.create_note(NoteKind::Checklist).unwrap();
    for i in 0..20 {
        core.add_item(id, format!("item {i}")).unwrap();
    }
    let notes = core.notes().unwrap();
    assert_eq!(notes[0].items, 20);
    assert_eq!(notes[0].preview.len(), easynotes_core::note::PREVIEW_ITEMS);
    assert_eq!(notes[0].preview[0], "item 0");
}

/// The list is a stream, not a poll: the grid subscribes once and every edit —
/// from this device or another — arrives on it. Without this a note card would
/// go stale the moment someone else ticked something off.
fn an_edit_reaches_the_list<D: NoteDoc>() {
    let mut core = in_memory::<D>();
    let mut list = core.watch_notes();
    let id = core.create_note(NoteKind::Checklist).unwrap();
    assert_eq!(list.borrow_and_update().len(), 1);

    core.set_title(id, "groceries").unwrap();
    assert!(list.has_changed().unwrap());
    assert_eq!(titles(&list.borrow_and_update()), ["groceries"]);

    core.add_item(id, "milk").unwrap();
    assert!(list.has_changed().unwrap());
    assert_eq!(list.borrow().first().unwrap().items, 1);
}

/// An envelope from a peer goes through the same refresh as a local edit, which
/// is the note-list half of the rule the core is built on: the two paths are one
/// path.
fn a_peers_edit_reaches_the_list<D: NoteDoc>() {
    let mut owner = in_memory::<D>();
    let id = owner.create_note(NoteKind::Checklist).unwrap();
    let blob = owner.export_share_blob(id).unwrap();

    let mut guest = in_memory::<D>();
    guest.import_share_blob(&blob).unwrap();
    let mut list = guest.watch_notes();
    let _ = list.borrow_and_update();

    let envelope = owner.set_title(id, "groceries").unwrap();
    guest.apply_remote(id, &envelope).unwrap();

    assert!(list.has_changed().unwrap());
    assert_eq!(titles(&list.borrow_and_update()), ["groceries"]);
}

/// Joining adds a note; it does not replace the one that was there. That is the
/// whole difference between the Slice 1 one-note model and a note list, and it
/// is the case the app's join button now takes.
fn joining_adds_a_note<D: NoteDoc>() {
    let mut owner = in_memory::<D>();
    let shared = owner.create_note(NoteKind::Checklist).unwrap();
    owner.set_title(shared, "shared").unwrap();
    let blob = owner.export_share_blob(shared).unwrap();

    let mut guest = in_memory::<D>();
    let own = guest.create_note(NoteKind::Checklist).unwrap();
    guest.set_title(own, "mine").unwrap();
    let joined = guest.import_share_blob(&blob).unwrap();
    assert_eq!(joined, shared);

    let notes = guest.notes().unwrap();
    assert_eq!(notes.len(), 2);
    assert_eq!(notes.iter().filter(|s| s.id == own).count(), 1);
    assert_eq!(notes.iter().filter(|s| s.id == shared).count(), 1);
}

/// Pasting the same blob twice is a thing people do. It must not make a second
/// note, and it must not move the note it names to the top of someone's grid.
fn joining_twice_changes_nothing<D: NoteDoc>() {
    let mut owner = in_memory::<D>();
    let shared = owner.create_note(NoteKind::Checklist).unwrap();
    let blob = owner.export_share_blob(shared).unwrap();

    let mut guest = in_memory::<D>();
    guest.import_share_blob(&blob).unwrap();
    let later = guest.create_note(NoteKind::Checklist).unwrap();
    guest.import_share_blob(&blob).unwrap();

    let notes = guest.notes().unwrap();
    assert_eq!(notes.len(), 2);
    assert_eq!(notes[0].id, later, "re-joining moved the note to the top");
}

/// The list is the notes on the disk, not the notes this process happened to
/// open. A relaunch has to find all of them, in the order they were in.
fn the_list_survives_a_relaunch<D: NoteDoc>() {
    let dir = tempfile::tempdir().unwrap();
    let mut made = Vec::new();
    {
        let mut core = on_disk::<D>(dir.path());
        for title in ["groceries", "hardware", "books"] {
            let id = core.create_note(NoteKind::Checklist).unwrap();
            core.set_title(id, title).unwrap();
            made.push(id);
        }
    }

    let mut core = on_disk::<D>(dir.path());
    let notes = core.notes().unwrap();
    assert_eq!(titles(&notes), ["books", "hardware", "groceries"]);
    assert_eq!(
        notes.iter().map(|s| s.id).collect::<Vec<_>>(),
        made.iter().rev().copied().collect::<Vec<_>>()
    );
}

/// A note made and not yet typed into has no log and no snapshot, so it exists
/// only as the row that records where it sits. It is still a note, and losing it
/// on the next launch would be the app quietly discarding something the user
/// made.
fn an_empty_note_survives_a_relaunch<D: NoteDoc>() {
    let dir = tempfile::tempdir().unwrap();
    let empty = {
        let mut core = on_disk::<D>(dir.path());
        core.create_note(NoteKind::Checklist).unwrap()
    };

    let mut core = on_disk::<D>(dir.path());
    let notes = core.notes().unwrap();
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].id, empty);
    assert_eq!(notes[0].title, "");
}

/// A note whose key this device does not have cannot be shown, and must not
/// take the rest of the list down with it. `copy_store` can leave one — it
/// carries a log whose key never arrived — and so will revocation.
fn a_note_without_a_key_is_skipped<D: NoteDoc>() {
    // A note the store knows about and the keyring does not, put there before
    // the core opens it — which is how one really arrives.
    let mut store = MemoryStore::new();
    store.append(NoteId::random(), b"not ours").unwrap();

    let mut core: Memory<D> = NoteCore::open(store, MemoryKeyring::new()).unwrap();
    let id = core.create_note(NoteKind::Checklist).unwrap();
    core.set_title(id, "readable").unwrap();

    assert_eq!(titles(&core.notes().unwrap()), ["readable"]);
}

/// Every test above, for each CRDT backend. A note list that behaved differently
/// under Automerge and Yrs would be a note list that depended on the backend,
/// which is exactly what the trait exists to prevent.
macro_rules! backend_suite {
    ($name:ident, $backend:ty) => {
        mod $name {
            #[test]
            fn the_newest_note_is_first() {
                super::the_newest_note_is_first::<$backend>()
            }
            #[test]
            fn a_summary_says_what_the_note_says() {
                super::a_summary_says_what_the_note_says::<$backend>()
            }
            #[test]
            fn a_preview_stops_at_a_few_items() {
                super::a_preview_stops_at_a_few_items::<$backend>()
            }
            #[test]
            fn an_edit_reaches_the_list() {
                super::an_edit_reaches_the_list::<$backend>()
            }
            #[test]
            fn a_peers_edit_reaches_the_list() {
                super::a_peers_edit_reaches_the_list::<$backend>()
            }
            #[test]
            fn joining_adds_a_note() {
                super::joining_adds_a_note::<$backend>()
            }
            #[test]
            fn joining_twice_changes_nothing() {
                super::joining_twice_changes_nothing::<$backend>()
            }
            #[test]
            fn the_list_survives_a_relaunch() {
                super::the_list_survives_a_relaunch::<$backend>()
            }
            #[test]
            fn an_empty_note_survives_a_relaunch() {
                super::an_empty_note_survives_a_relaunch::<$backend>()
            }
            #[test]
            fn a_note_without_a_key_is_skipped() {
                super::a_note_without_a_key_is_skipped::<$backend>()
            }
        }
    };
}

#[cfg(feature = "automerge-backend")]
backend_suite!(automerge, easynotes_core::crdt::AutomergeNote);
#[cfg(feature = "yrs-backend")]
backend_suite!(yrs, easynotes_core::crdt::YrsNote);
