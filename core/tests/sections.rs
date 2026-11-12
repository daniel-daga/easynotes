//! Sections: a heading is a flag on an item, not a container.
//!
//! The whole point of the flag ([ADR 0037](../../docs/decisions.md#adr-0037))
//! is what it does *not* need: no second ordered list in the document, no
//! parent/child links, and therefore no new merge rule. Which rows a heading
//! owns is read off their positions, so a concurrent move or a concurrent
//! heading toggle can never produce half a section, only a different one.
//!
//! What is held to account here is that story: a heading merges, a heading that
//! a peer ticks stays a heading, deleting one never takes its items, and a
//! heading is a label for a card rather than an item on it.
//!
//! Every test runs against both backends, as the convergence suite does.

use easynotes_core::crdt::{NoteDoc, NoteKind};
use easynotes_core::ids::NoteId;
use easynotes_core::note::NoteCore;
use easynotes_core::store::{MemoryKeyring, MemoryStore};

type Core<D> = NoteCore<D, MemoryStore, MemoryKeyring>;

fn core<D: NoteDoc>() -> Core<D> {
    NoteCore::open(MemoryStore::new(), MemoryKeyring::new()).unwrap()
}

/// Two cores on one note, with everything the creator has written so far
/// already delivered.
fn pair<D: NoteDoc>() -> (Core<D>, Core<D>, NoteId) {
    pair_of::<D>(NoteKind::Checklist)
}

fn pair_of<D: NoteDoc>(kind: NoteKind) -> (Core<D>, Core<D>, NoteId) {
    let mut a = core::<D>();
    let id = a.create_note(kind).unwrap();
    let blob = a.export_share_blob(id).unwrap();
    let mut b = core::<D>();
    assert_eq!(b.import_share_blob(&blob).unwrap(), id);
    let created: Vec<Vec<u8>> = a
        .unsent(id, None)
        .unwrap()
        .into_iter()
        .map(|(_, e)| e)
        .collect();
    deliver(&mut b, id, &created);
    (a, b, id)
}

fn deliver<D: NoteDoc>(to: &mut Core<D>, id: NoteId, envelopes: &[Vec<u8>]) {
    for e in envelopes {
        to.apply_remote(id, e).unwrap();
    }
}

/// The rows as `(text, heading)`, in display order.
fn rows<D: NoteDoc>(c: &mut Core<D>, id: NoteId) -> Vec<(String, bool)> {
    c.view(id)
        .unwrap()
        .items
        .into_iter()
        .map(|item| (item.text, item.heading))
        .collect()
}

fn row(text: &str, heading: bool) -> (String, bool) {
    (text.to_owned(), heading)
}

// ---------------------------------------------------------------------------

fn a_heading_reaches_the_other_device_and_keeps_its_place<D: NoteDoc>() {
    let (mut a, mut b, id) = pair::<D>();
    let mut wire = Vec::new();
    for (text, heading) in [("Groceries", true), ("Dairy", true), ("Milk", false)] {
        let (_, e) = if heading {
            a.add_heading(id, text).unwrap()
        } else {
            a.add_item(id, text).unwrap()
        };
        wire.push(e);
    }
    deliver(&mut b, id, &wire);

    assert_eq!(
        rows(&mut b, id),
        [
            row("Groceries", true),
            row("Dairy", true),
            row("Milk", false)
        ]
    );
    assert_eq!(a.view(id).unwrap(), b.view(id).unwrap());
}

fn turning_a_heading_back_into_an_item_keeps_the_row<D: NoteDoc>() {
    let (mut a, mut b, id) = pair::<D>();
    let (heading, e) = a.add_heading(id, "Dairy").unwrap();
    let (item, e2) = a.add_item(id, "Milk").unwrap();
    deliver(&mut b, id, &[e, e2]);

    let back = a
        .set_heading(id, heading, false)
        .unwrap()
        .expect("it changed");
    deliver(&mut b, id, &[back]);

    assert_eq!(rows(&mut b, id), [row("Dairy", false), row("Milk", false)]);
    // The ID is the row: nothing was deleted and re-added.
    assert_eq!(b.view(id).unwrap().items[0].id, heading);
    assert_eq!(b.view(id).unwrap().items[1].id, item);
    // And setting what it already says writes nothing.
    assert!(a.set_heading(id, heading, false).unwrap().is_none());
}

fn deleting_a_heading_never_takes_its_items<D: NoteDoc>() {
    let (mut a, mut b, id) = pair::<D>();
    let (heading, e) = a.add_heading(id, "Dairy").unwrap();
    let (_, e2) = a.add_item(id, "Milk").unwrap();
    let (_, e3) = a.add_item(id, "Butter").unwrap();
    deliver(&mut b, id, &[e, e2, e3]);

    let removed = a.remove_item(id, heading).unwrap();
    deliver(&mut b, id, &[removed]);

    assert_eq!(rows(&mut b, id), [row("Milk", false), row("Butter", false)]);
}

fn a_tick_on_a_heading_survives_and_does_not_undo_it<D: NoteDoc>() {
    // An older build draws a heading as an ordinary item and offers its
    // checkbox. That tick must merge, and the heading must still be a heading:
    // the flag is a separate field, not a kind that the tick could overwrite.
    let (mut a, mut b, id) = pair::<D>();
    let (heading, e) = a.add_heading(id, "Dairy").unwrap();
    deliver(&mut b, id, &[e]);

    let tick = b.set_checked(id, heading, true).unwrap();
    deliver(&mut a, id, &[tick]);

    let view = a.view(id).unwrap();
    let shown = view.item(heading).expect("the heading is still there");
    assert!(shown.heading);
    assert!(shown.checked, "the tick merged like any field");
    assert_eq!(b.view(id).unwrap(), view);
}

fn a_stale_screen_writing_the_old_value_does_not_undo_a_heading<D: NoteDoc>() {
    // A calls it a heading while B, offline, still draws an item. B's screen
    // asks for `false`, which is what B still sees: the core writes nothing,
    // because a heading is a boolean register and writing the old value again
    // is not a change. A's heading is what both end up reading.
    let (mut a, mut b, id) = pair::<D>();
    let (heading, e) = a.add_item(id, "Dairy").unwrap();
    deliver(&mut b, id, &[e]);

    let from_a = a
        .set_heading(id, heading, true)
        .unwrap()
        .expect("it changed");
    assert!(
        b.set_heading(id, heading, false).unwrap().is_none(),
        "a stale screen has nothing to say"
    );
    deliver(&mut b, id, &[from_a]);

    assert!(a.view(id).unwrap().item(heading).unwrap().heading);
    assert_eq!(a.view(id).unwrap(), b.view(id).unwrap());

    // Once both agree, turning it back off is one write that reaches both.
    let off = a
        .set_heading(id, heading, false)
        .unwrap()
        .expect("it changed");
    deliver(&mut b, id, &[off]);
    assert!(!b.view(id).unwrap().item(heading).unwrap().heading);
    assert_eq!(a.view(id).unwrap(), b.view(id).unwrap());
}

fn a_hash_line_typed_into_a_text_note_becomes_a_section<D: NoteDoc>() {
    let (mut a, mut b, id) = pair_of::<D>(NoteKind::Text);
    let body = a
        .set_body(id, "# Dairy\n[ ] milk\n## Frozen\n[ ] peas\n#Cocoa\n")
        .unwrap()
        .expect("the body changed");
    let converted = a.set_kind(id, NoteKind::Checklist).unwrap().unwrap();
    deliver(&mut b, id, &[body, converted]);

    assert_eq!(
        rows(&mut b, id),
        [
            row("Dairy", true),
            row("milk", false),
            row("Frozen", true),
            row("peas", false),
            // No space after the hash is an item, not a heading: Markdown asks
            // for the space, and a shopping line may start with one.
            row("#Cocoa", false),
        ]
    );
}

fn a_heading_is_a_card_label_and_not_a_card_line<D: NoteDoc>() {
    let (mut a, _, id) = pair::<D>();
    a.add_heading(id, "Dairy").unwrap();
    a.add_item(id, "Milk").unwrap();
    a.add_heading(id, "Frozen").unwrap();
    a.add_item(id, "Peas").unwrap();

    let summary = a
        .notes()
        .unwrap()
        .into_iter()
        .find(|s| s.id == id)
        .expect("the note is listed");
    assert_eq!(summary.items, 4, "a heading is an item to the counts");
    assert_eq!(
        summary.preview,
        ["Milk", "Peas"],
        "and not a line of the card"
    );

    // A heading is still a thing a search finds, and still a thing the note's
    // text is matched on.
    let hits = a.search("dairy").unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].matched, "Dairy");
}

fn a_folded_section_is_this_devices_own_and_never_a_note_edit<D: NoteDoc>() {
    let (mut a, _, id) = pair::<D>();
    let (dairy, _) = a.add_heading(id, "Dairy").unwrap();
    a.add_item(id, "Milk").unwrap();
    a.add_item(id, "Bread").unwrap();
    let before = a.unsent(id, None).unwrap().len();

    a.set_collapsed(id, dairy, true).unwrap();

    assert_eq!(a.collapsed(id).unwrap(), [dairy]);
    assert_eq!(
        a.unsent(id, None).unwrap().len(),
        before,
        "folding is local: nothing to send, so nothing about it can reach anybody"
    );

    a.set_collapsed(id, dairy, false).unwrap();
    assert!(a.collapsed(id).unwrap().is_empty());
}

fn a_fold_whose_heading_is_gone_folds_nothing<D: NoteDoc>() {
    // The heading is deleted on the other device while this one has it folded.
    // A stale ID must not sit in the row and fold some later heading if the ID
    // were ever reused.
    let (mut a, _, id) = pair::<D>();
    let (dairy, _) = a.add_heading(id, "Dairy").unwrap();
    a.set_collapsed(id, dairy, true).unwrap();
    assert_eq!(a.collapsed(id).unwrap(), [dairy]);

    a.remove_item(id, dairy).unwrap();
    assert!(a.collapsed(id).unwrap().is_empty());
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
    a_heading_reaches_the_other_device_and_keeps_its_place,
    turning_a_heading_back_into_an_item_keeps_the_row,
    deleting_a_heading_never_takes_its_items,
    a_tick_on_a_heading_survives_and_does_not_undo_it,
    a_stale_screen_writing_the_old_value_does_not_undo_a_heading,
    a_hash_line_typed_into_a_text_note_becomes_a_section,
    a_heading_is_a_card_label_and_not_a_card_line,
    a_folded_section_is_this_devices_own_and_never_a_note_edit,
    a_fold_whose_heading_is_gone_folds_nothing,
);
