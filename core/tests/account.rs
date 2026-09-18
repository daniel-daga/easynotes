//! The account document: what one person thinks about their own notes.
//!
//! Two things are being held here. The first is that a placement *is* personal —
//! colouring a note must put nothing in the note, because the note is the thing
//! other people have. The second is that it behaves like the rest of the core:
//! it survives a relaunch, it converges, and a device that had notes before it
//! existed keeps them where they were.

use easynotes_core::crdt::account::{AccountDoc, AccountEdit, Value};
use easynotes_core::crdt::{Colour, Layout, NoteDoc};
use easynotes_core::crypto::MasterKey;
use easynotes_core::ids::NoteId;
use easynotes_core::note::{NoteCore, NoteKind, NoteSummary, ACCOUNT_DOC};
use easynotes_core::store::{
    master_key_from_file, Keyring, MemoryKeyring, MemoryStore, OpStore, SqliteKeyring, SqliteStore,
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
// What a placement is, and is not
// ---------------------------------------------------------------------------

/// The property the whole document exists for. A colour is *this person's*, so
/// it must not reach the note — where it would be encrypted with the note's key,
/// sent to the relay, and repaint the shared shopping list on everyone else's
/// phone.
fn colouring_a_note_puts_nothing_in_the_note<D: NoteDoc>() {
    let mut core = in_memory::<D>();
    let id = core.create_note(NoteKind::Checklist).unwrap();
    core.set_title(id, "groceries").unwrap();
    let before = core.store().load(id).unwrap().len();

    core.set_colour(id, Some(Colour::Teal)).unwrap();
    core.set_pinned(id, true).unwrap();
    core.set_archived(id, true).unwrap();

    assert_eq!(
        core.store().load(id).unwrap().len(),
        before,
        "a personal placement was written to the note's own log"
    );
    // It went somewhere, though: the account document's log, under its own ID.
    assert!(!core.store().load(ACCOUNT_DOC).unwrap().is_empty());
}

/// And the account document is not readable by the note's key, because it is not
/// sealed with it. This is the same envelope machinery a note uses, so what
/// proves it is that the bytes in the log are an envelope for the *account*
/// document and for nothing else.
fn the_account_log_is_addressed_to_the_account_document<D: NoteDoc>() {
    let mut core = in_memory::<D>();
    let id = core.create_note(NoteKind::Checklist).unwrap();
    core.set_colour(id, Some(Colour::Red)).unwrap();

    for raw in core.store().load(ACCOUNT_DOC).unwrap() {
        let envelope = easynotes_core::crypto::Envelope::decode(&raw).unwrap();
        assert_eq!(envelope.note_id, ACCOUNT_DOC);
        envelope
            .verify()
            .expect("a placement is signed like an edit");
    }
}

fn a_colour_and_a_pin_come_back_on_the_summary<D: NoteDoc>() {
    let mut core = in_memory::<D>();
    let id = core.create_note(NoteKind::Checklist).unwrap();
    core.set_colour(id, Some(Colour::Yellow)).unwrap();
    core.set_pinned(id, true).unwrap();

    let notes = core.notes().unwrap();
    assert_eq!(notes[0].placement.colour, Some(Colour::Yellow));
    assert!(notes[0].placement.pinned);
    assert!(notes[0].placement.in_grid());
}

fn clearing_a_colour_leaves_no_colour<D: NoteDoc>() {
    let mut core = in_memory::<D>();
    let id = core.create_note(NoteKind::Checklist).unwrap();
    core.set_colour(id, Some(Colour::Blue)).unwrap();
    core.set_colour(id, None).unwrap();
    assert_eq!(core.placement(id).colour, None);
}

/// A pinned note is at the top however old it is. That is the whole of what
/// pinning means.
fn a_pinned_note_comes_first<D: NoteDoc>() {
    let mut core = in_memory::<D>();
    let mut made = Vec::new();
    for title in ["groceries", "hardware", "books"] {
        let id = core.create_note(NoteKind::Checklist).unwrap();
        core.set_title(id, title).unwrap();
        made.push(id);
    }
    assert_eq!(
        titles(&core.notes().unwrap()),
        ["books", "hardware", "groceries"]
    );

    core.set_pinned(made[0], true).unwrap();
    assert_eq!(
        titles(&core.notes().unwrap()),
        ["groceries", "books", "hardware"]
    );
}

/// The archive and the trash do not remove a note from the list — they say where
/// it belongs, and the screen decides what to draw. A core that filtered would
/// have to be asked three times for what it already knows once.
fn the_archive_and_the_trash_are_where_a_note_is_not_in_the_grid<D: NoteDoc>() {
    let mut core = in_memory::<D>();
    let archived = core.create_note(NoteKind::Checklist).unwrap();
    let trashed = core.create_note(NoteKind::Checklist).unwrap();
    let kept = core.create_note(NoteKind::Checklist).unwrap();
    core.set_archived(archived, true).unwrap();
    core.set_trashed(trashed, true).unwrap();

    let notes = core.notes().unwrap();
    assert_eq!(notes.len(), 3, "a note left the list instead of the grid");
    let in_grid: Vec<NoteId> = notes
        .iter()
        .filter(|s| s.placement.in_grid())
        .map(|s| s.id)
        .collect();
    assert_eq!(in_grid, [kept]);

    // And back out again.
    core.set_trashed(trashed, false).unwrap();
    assert!(core.placement(trashed).in_grid());
}

/// Keep's rule, and the sensible one: a note cannot be both held at the top and
/// put out of the way.
fn archiving_or_trashing_a_note_unpins_it<D: NoteDoc>() {
    let mut core = in_memory::<D>();
    let archived = core.create_note(NoteKind::Checklist).unwrap();
    let trashed = core.create_note(NoteKind::Checklist).unwrap();
    core.set_pinned(archived, true).unwrap();
    core.set_pinned(trashed, true).unwrap();

    core.set_archived(archived, true).unwrap();
    core.set_trashed(trashed, true).unwrap();

    assert!(!core.placement(archived).pinned);
    assert!(!core.placement(trashed).pinned);
}

/// The grid's order is a fractional index like an item's, so moving a note is a
/// write to one field and two devices moving two notes cannot lose either.
fn moving_a_note_puts_it_where_it_was_asked<D: NoteDoc>() {
    let mut core = in_memory::<D>();
    for title in ["a", "b", "c"] {
        let id = core.create_note(NoteKind::Checklist).unwrap();
        core.set_title(id, title).unwrap();
    }
    assert_eq!(titles(&core.notes().unwrap()), ["c", "b", "a"]);

    let c = core.notes().unwrap()[0].id;
    core.move_note(c, 2).unwrap();
    assert_eq!(titles(&core.notes().unwrap()), ["b", "a", "c"]);

    core.move_note(c, 0).unwrap();
    assert_eq!(titles(&core.notes().unwrap()), ["c", "b", "a"]);
}

/// A placement is an edit like any other, so the grid has to hear about it the
/// same way: on the stream, without being asked again.
fn a_placement_reaches_the_list<D: NoteDoc>() {
    let mut core = in_memory::<D>();
    let mut list = core.watch_notes();
    let id = core.create_note(NoteKind::Checklist).unwrap();
    let _ = list.borrow_and_update();

    core.set_colour(id, Some(Colour::Pink)).unwrap();
    assert!(list.has_changed().unwrap());
    assert_eq!(
        list.borrow_and_update()[0].placement.colour,
        Some(Colour::Pink)
    );
}

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

fn the_layout_is_a_grid_until_somebody_says_otherwise<D: NoteDoc>() {
    let mut core = in_memory::<D>();
    assert_eq!(core.layout(), Layout::Grid);
    core.set_layout(Layout::List).unwrap();
    assert_eq!(core.layout(), Layout::List);
}

/// The hole item 5 left: a view setting that is forgotten when the screen goes
/// away is not a setting.
fn the_layout_survives_a_relaunch<D: NoteDoc>() {
    let dir = tempfile::tempdir().unwrap();
    {
        let mut core = on_disk::<D>(dir.path());
        core.set_layout(Layout::List).unwrap();
    }
    assert_eq!(on_disk::<D>(dir.path()).layout(), Layout::List);
}

// ---------------------------------------------------------------------------
// Across a relaunch
// ---------------------------------------------------------------------------

fn placements_survive_a_relaunch<D: NoteDoc>() {
    let dir = tempfile::tempdir().unwrap();
    let (coloured, pinned, trashed) = {
        let mut core = on_disk::<D>(dir.path());
        let coloured = core.create_note(NoteKind::Checklist).unwrap();
        let pinned = core.create_note(NoteKind::Checklist).unwrap();
        let trashed = core.create_note(NoteKind::Checklist).unwrap();
        core.set_colour(coloured, Some(Colour::Green)).unwrap();
        core.set_pinned(pinned, true).unwrap();
        core.set_trashed(trashed, true).unwrap();
        (coloured, pinned, trashed)
    };

    let mut core = on_disk::<D>(dir.path());
    let notes = core.notes().unwrap();
    assert_eq!(notes[0].id, pinned, "the pinned note is not first");
    assert_eq!(core.placement(coloured).colour, Some(Colour::Green));
    assert!(core.placement(pinned).pinned);
    assert!(core.placement(trashed).trashed);
}

/// A relaunch must not *write*. Every note already has a placement, so opening
/// the device again has nothing to place — and a launch that appended one
/// envelope per note would grow the account document's log for ever.
fn a_relaunch_places_nothing_again<D: NoteDoc>() {
    let dir = tempfile::tempdir().unwrap();
    let logged = {
        let mut core = on_disk::<D>(dir.path());
        for _ in 0..3 {
            core.create_note(NoteKind::Checklist).unwrap();
        }
        core.notes().unwrap();
        core.store().load(ACCOUNT_DOC).unwrap().len()
    };

    let mut core = on_disk::<D>(dir.path());
    core.notes().unwrap();
    assert_eq!(core.store().load(ACCOUNT_DOC).unwrap().len(), logged);
}

/// The migration. A device that has notes and no account document — every device
/// that updates into this version — gets the grid it had, in the order it had
/// it, seeded from the ordinal those notes were made with.
///
/// The old device is built by hand rather than by an older binary: notes with
/// keys and an [`ADDED`](easynotes_core::note::ADDED) ordinal, and nothing that
/// says where they sit. That is exactly what was on the disk before this change.
fn a_device_that_had_notes_keeps_their_order<D: NoteDoc>() {
    use easynotes_core::crypto::{DeviceSeed, NoteKey};
    use easynotes_core::note::ADDED;

    let mut store = MemoryStore::new();
    let mut keyring = MemoryKeyring::new();
    keyring.put_device_seed(&DeviceSeed([7u8; 64])).unwrap();
    let made: Vec<NoteId> = (0..3).map(|_| NoteId::random()).collect();
    for (n, id) in made.iter().enumerate() {
        keyring.put_note_key(*id, &NoteKey::random()).unwrap();
        store
            .put_meta(*id, ADDED, &(n as u64 + 1).to_be_bytes())
            .unwrap();
    }

    let mut core: Memory<D> = NoteCore::open(store, keyring).unwrap();
    let listed: Vec<NoteId> = core.notes().unwrap().into_iter().map(|s| s.id).collect();
    assert_eq!(
        listed,
        made.iter().rev().copied().collect::<Vec<_>>(),
        "the grid came back in a different order than it was left in"
    );

    // And having adopted them once, it does not do it again: a launch that
    // placed every note afresh would grow the account document for ever.
    let logged = core.store().load(ACCOUNT_DOC).unwrap().len();
    core.notes().unwrap();
    assert_eq!(core.store().load(ACCOUNT_DOC).unwrap().len(), logged);
}

// ---------------------------------------------------------------------------
// The account document is not a note
// ---------------------------------------------------------------------------

/// A share blob that claims the account document's ID would put a key of the
/// sender's choosing where this device's placements are sealed with its own.
/// Pasting one is refused rather than believed.
fn a_share_blob_for_the_account_document_is_refused<D: NoteDoc>() {
    let mut core = in_memory::<D>();
    let id = core.create_note(NoteKind::Checklist).unwrap();
    core.set_colour(id, Some(Colour::Purple)).unwrap();

    let forged = easynotes_core::crypto::export_share_blob(
        &ACCOUNT_DOC,
        &easynotes_core::crypto::NoteKey::random(),
    );
    assert!(core.import_share_blob(&forged).is_err());
    assert!(core.open_note(ACCOUNT_DOC).is_err());
    // Still ours, still readable.
    assert_eq!(core.placement(id).colour, Some(Colour::Purple));
}

// ---------------------------------------------------------------------------
// The document itself
// ---------------------------------------------------------------------------

/// Two of this person's devices, editing the same note's placement at the same
/// moment. Nothing may be lost: a pin on one and a colour on the other are
/// different keys, and both survive whichever order the updates arrive in.
///
/// Straight against the document rather than through a [`NoteCore`], because
/// there is no sync for the account document yet — this is the property that has
/// to hold when there is.
fn two_devices_keep_both_placements<D: NoteDoc>() {
    let note = NoteId::random();
    let mut phone = <D::Account as AccountDoc>::create(&[1u8; 16]);
    let mut laptop = <D::Account as AccountDoc>::create(&[2u8; 16]);

    let pin = phone
        .apply_local(&AccountEdit::SetPinned { note, pinned: true })
        .unwrap();
    let colour = laptop
        .apply_local(&AccountEdit::SetColour {
            note,
            colour: Some(Colour::Orange),
        })
        .unwrap();

    // Crossed over, and the second time for nothing: an update applied twice
    // must leave the document as it was.
    phone.apply_remote(&colour).unwrap();
    phone.apply_remote(&colour).unwrap();
    laptop.apply_remote(&pin).unwrap();

    for doc in [&phone, &laptop] {
        let placement = doc.view().placement(note);
        assert!(placement.pinned);
        assert_eq!(placement.colour, Some(Colour::Orange));
    }
    assert_eq!(phone.view(), laptop.view());
}

/// A snapshot is the whole document, and reloading one is how a compacted
/// account document would come back.
fn a_snapshot_reloads_to_the_same_document<D: NoteDoc>() {
    let note = NoteId::random();
    let mut doc = <D::Account as AccountDoc>::create(&[3u8; 16]);
    doc.apply_local(&AccountEdit::SetColour {
        note,
        colour: Some(Colour::Brown),
    })
    .unwrap();
    doc.apply_local(&AccountEdit::SetSetting {
        name: "layout".into(),
        value: "list".into(),
    })
    .unwrap();

    let reloaded = <D::Account as AccountDoc>::load(&[3u8; 16], &doc.snapshot()).unwrap();
    assert_eq!(reloaded.view(), doc.view());
}

/// Backends store what they are given and nothing else. A key a future version
/// writes has to survive a round trip through an older one rather than being
/// dropped on the way.
fn a_backend_keeps_a_key_it_does_not_understand<D: NoteDoc>() {
    let mut doc = <D::Account as AccountDoc>::create(&[4u8; 16]);
    doc.write(&[("z:from the future".into(), Some(Value::Bool(true)))])
        .unwrap();
    assert!(doc
        .entries()
        .iter()
        .any(|(key, _)| key == "z:from the future"));
    assert!(doc.view().placements.is_empty());
}

// ---------------------------------------------------------------------------

macro_rules! backend_suite {
    ($name:ident, $backend:ty) => {
        mod $name {
            #[test]
            fn colouring_a_note_puts_nothing_in_the_note() {
                super::colouring_a_note_puts_nothing_in_the_note::<$backend>()
            }
            #[test]
            fn the_account_log_is_addressed_to_the_account_document() {
                super::the_account_log_is_addressed_to_the_account_document::<$backend>()
            }
            #[test]
            fn a_colour_and_a_pin_come_back_on_the_summary() {
                super::a_colour_and_a_pin_come_back_on_the_summary::<$backend>()
            }
            #[test]
            fn clearing_a_colour_leaves_no_colour() {
                super::clearing_a_colour_leaves_no_colour::<$backend>()
            }
            #[test]
            fn a_pinned_note_comes_first() {
                super::a_pinned_note_comes_first::<$backend>()
            }
            #[test]
            fn the_archive_and_the_trash_are_where_a_note_is_not_in_the_grid() {
                super::the_archive_and_the_trash_are_where_a_note_is_not_in_the_grid::<$backend>()
            }
            #[test]
            fn archiving_or_trashing_a_note_unpins_it() {
                super::archiving_or_trashing_a_note_unpins_it::<$backend>()
            }
            #[test]
            fn moving_a_note_puts_it_where_it_was_asked() {
                super::moving_a_note_puts_it_where_it_was_asked::<$backend>()
            }
            #[test]
            fn a_placement_reaches_the_list() {
                super::a_placement_reaches_the_list::<$backend>()
            }
            #[test]
            fn the_layout_is_a_grid_until_somebody_says_otherwise() {
                super::the_layout_is_a_grid_until_somebody_says_otherwise::<$backend>()
            }
            #[test]
            fn the_layout_survives_a_relaunch() {
                super::the_layout_survives_a_relaunch::<$backend>()
            }
            #[test]
            fn placements_survive_a_relaunch() {
                super::placements_survive_a_relaunch::<$backend>()
            }
            #[test]
            fn a_relaunch_places_nothing_again() {
                super::a_relaunch_places_nothing_again::<$backend>()
            }
            #[test]
            fn a_device_that_had_notes_keeps_their_order() {
                super::a_device_that_had_notes_keeps_their_order::<$backend>()
            }
            #[test]
            fn a_share_blob_for_the_account_document_is_refused() {
                super::a_share_blob_for_the_account_document_is_refused::<$backend>()
            }
            #[test]
            fn two_devices_keep_both_placements() {
                super::two_devices_keep_both_placements::<$backend>()
            }
            #[test]
            fn a_snapshot_reloads_to_the_same_document() {
                super::a_snapshot_reloads_to_the_same_document::<$backend>()
            }
            #[test]
            fn a_backend_keeps_a_key_it_does_not_understand() {
                super::a_backend_keeps_a_key_it_does_not_understand::<$backend>()
            }
        }
    };
}

#[cfg(feature = "automerge-backend")]
backend_suite!(automerge, easynotes_core::crdt::AutomergeNote);
#[cfg(feature = "yrs-backend")]
backend_suite!(yrs, easynotes_core::crdt::YrsNote);
