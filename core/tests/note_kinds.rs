//! Text notes, and the switch between text and checklist.
//!
//! A checklist's item text is a register: two people retyping one item at the
//! same moment lose one of the two, and for a grocery list that is the right
//! trade (ADR 0001, D3). A text note's body is not allowed that trade, because a
//! shared note is where two people really do type into one paragraph. So the
//! claim tested first is that concurrent typing in one body keeps both people's
//! characters.
//!
//! The second is the one the switch makes: converting moves content, and a merge
//! never hides it. A peer who was typing into the side a conversion emptied must
//! find their words still on the note.
//!
//! Every test runs against both backends, as the convergence suite does.

use easynotes_core::crdt::{NoteDoc, NoteKind, NoteView};
use easynotes_core::ids::NoteId;
use easynotes_core::note::NoteCore;
use easynotes_core::store::{MemoryKeyring, MemoryStore};

type Core<D> = NoteCore<D, MemoryStore, MemoryKeyring>;

fn core<D: NoteDoc>() -> Core<D> {
    NoteCore::open(MemoryStore::new(), MemoryKeyring::new()).unwrap()
}

/// Two cores sharing one note of `kind`, with everything the creator has
/// written so far already delivered — as a relay would have delivered it before
/// the second device could open the note at all.
fn pair<D: NoteDoc>(kind: NoteKind) -> (Core<D>, Core<D>, NoteId) {
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

fn texts(view: &NoteView) -> Vec<String> {
    view.items.iter().map(|i| i.text.clone()).collect()
}

fn body<D: NoteDoc>(c: &mut Core<D>, id: NoteId, text: &str) -> Vec<u8> {
    c.set_body(id, text).unwrap().expect("the body changed")
}

// ---------------------------------------------------------------------------

fn a_new_text_note_says_so_on_both_devices<D: NoteDoc>() {
    let (a, b, id) = pair::<D>(NoteKind::Text);
    assert_eq!(a.view(id).unwrap().kind, NoteKind::Text);
    assert_eq!(b.view(id).unwrap().kind, NoteKind::Text);
}

fn a_note_that_records_no_kind_is_a_checklist<D: NoteDoc>() {
    // Every note written before kinds existed is one of these.
    let (a, _, id) = pair::<D>(NoteKind::Checklist);
    assert_eq!(a.view(id).unwrap().kind, NoteKind::Checklist);
    assert!(a.unsent(id, None).unwrap().is_empty());
}

fn setting_the_body_to_what_it_says_writes_nothing<D: NoteDoc>() {
    let (mut a, _, id) = pair::<D>(NoteKind::Text);
    body(&mut a, id, "milk");
    assert!(a.set_body(id, "milk").unwrap().is_none());
}

fn concurrent_typing_in_one_body_keeps_both<D: NoteDoc>() {
    let (mut a, mut b, id) = pair::<D>(NoteKind::Text);
    let first = body(&mut a, id, "Party on Saturday");
    deliver(&mut b, id, &[first]);

    // Both type at once, at different ends of the same line.
    let from_a = body(&mut a, id, "Big party on Saturday");
    let from_b = body(&mut b, id, "Party on Saturday at eight");
    deliver(&mut a, id, &[from_b]);
    deliver(&mut b, id, &[from_a]);

    let view = a.view(id).unwrap();
    assert_eq!(view.body, "Big party on Saturday at eight");
    assert_eq!(b.view(id).unwrap(), view);
}

fn an_edit_from_a_stale_screen_keeps_what_arrived_meanwhile<D: NoteDoc>() {
    // B's screen shows "milk". A's "oat " arrives in B's core, and before B's
    // screen has redrawn, B types " and eggs" onto the "milk" it was showing.
    let (mut a, mut b, id) = pair::<D>(NoteKind::Text);
    let first = body(&mut a, id, "milk");
    deliver(&mut b, id, &[first]);

    let from_a = body(&mut a, id, "oat milk");
    deliver(&mut b, id, &[from_a]);
    let from_b = b
        .edit_body(id, "milk", "milk and eggs")
        .unwrap()
        .expect("the body changed");
    deliver(&mut a, id, &[from_b]);

    let view = a.view(id).unwrap();
    assert_eq!(view.body, "oat milk and eggs");
    assert_eq!(b.view(id).unwrap(), view);
}

fn typing_outside_ascii_converges<D: NoteDoc>() {
    // The splice is worked out in bytes for one backend and in characters for
    // the other; a character that is more than one of either is where they would
    // disagree, if they did.
    let (mut a, mut b, id) = pair::<D>(NoteKind::Text);
    let first = body(&mut a, id, "Grüße 👋 aus Wien");
    deliver(&mut b, id, &[first]);
    let from_a = body(&mut a, id, "Liebe Grüße 👋 aus Wien");
    let from_b = body(&mut b, id, "Grüße 👋👋 aus Wien");
    deliver(&mut a, id, &[from_b]);
    deliver(&mut b, id, &[from_a]);

    let view = a.view(id).unwrap();
    assert_eq!(view.body, "Liebe Grüße 👋👋 aus Wien");
    assert_eq!(b.view(id).unwrap(), view);
}

fn text_to_checklist_makes_a_line_an_item<D: NoteDoc>() {
    let (mut a, mut b, id) = pair::<D>(NoteKind::Text);
    let mut wire = vec![body(&mut a, id, "milk\n\n  \nbread\neggs\n")];
    wire.push(a.set_kind(id, NoteKind::Checklist).unwrap().unwrap());
    deliver(&mut b, id, &wire);

    let view = b.view(id).unwrap();
    assert_eq!(view.kind, NoteKind::Checklist);
    assert_eq!(view.body, "");
    assert_eq!(texts(&view), ["milk", "bread", "eggs"]);
    assert!(view.items.iter().all(|i| !i.checked));
    assert_eq!(a.view(id).unwrap(), view);
}

fn checklist_to_text_keeps_checked_items_as_lines<D: NoteDoc>() {
    let (mut a, mut b, id) = pair::<D>(NoteKind::Checklist);
    let mut wire = Vec::new();
    let mut ids = Vec::new();
    for item in ["milk", "bread", "eggs"] {
        let (item, e) = a.add_item(id, item).unwrap();
        ids.push(item);
        wire.push(e);
    }
    wire.push(a.set_checked(id, ids[1], true).unwrap());
    wire.push(a.set_kind(id, NoteKind::Text).unwrap().unwrap());
    deliver(&mut b, id, &wire);

    let view = b.view(id).unwrap();
    assert_eq!(view.kind, NoteKind::Text);
    assert_eq!(view.body, "milk\nbread\neggs");
    assert!(view.items.is_empty());
    assert_eq!(a.view(id).unwrap(), view);
}

fn switching_there_and_back_keeps_the_lines<D: NoteDoc>() {
    let (mut a, _, id) = pair::<D>(NoteKind::Text);
    body(&mut a, id, "one\ntwo\nthree");
    a.set_kind(id, NoteKind::Checklist).unwrap().unwrap();
    a.set_kind(id, NoteKind::Text).unwrap().unwrap();
    assert_eq!(a.view(id).unwrap().body, "one\ntwo\nthree");
    assert!(a.set_kind(id, NoteKind::Text).unwrap().is_none());
}

fn typing_while_the_other_device_converts_is_not_hidden<D: NoteDoc>() {
    // A turns the note into a checklist while B, offline, keeps typing in the
    // body A is emptying. B's words must still be on the note afterwards.
    let (mut a, mut b, id) = pair::<D>(NoteKind::Text);
    let first = body(&mut a, id, "milk\nbread");
    deliver(&mut b, id, &[first]);

    let convert = a.set_kind(id, NoteKind::Checklist).unwrap().unwrap();
    let typed = body(&mut b, id, "milk\nbread\ncoffee");
    deliver(&mut a, id, &[typed]);
    deliver(&mut b, id, &[convert]);

    let view = a.view(id).unwrap();
    assert_eq!(b.view(id).unwrap(), view);
    assert_eq!(view.kind, NoteKind::Checklist);
    assert_eq!(texts(&view), ["milk", "bread"]);
    assert!(view.body.contains("coffee"), "body was {:?}", view.body);

    // And converting again is what tidies it up, rather than what loses it.
    a.set_kind(id, NoteKind::Text).unwrap().unwrap();
    let tidy = a.view(id).unwrap();
    assert!(tidy.body.contains("coffee") && tidy.body.contains("milk"));
}

fn an_item_added_while_the_other_device_converts_is_not_hidden<D: NoteDoc>() {
    let (mut a, mut b, id) = pair::<D>(NoteKind::Checklist);
    let (_, first) = a.add_item(id, "milk").unwrap();
    deliver(&mut b, id, &[first]);

    let convert = a.set_kind(id, NoteKind::Text).unwrap().unwrap();
    let (_, added) = b.add_item(id, "coffee").unwrap();
    deliver(&mut a, id, &[added]);
    deliver(&mut b, id, &[convert]);

    let view = a.view(id).unwrap();
    assert_eq!(b.view(id).unwrap(), view);
    assert_eq!(view.kind, NoteKind::Text);
    assert_eq!(view.body, "milk");
    assert_eq!(texts(&view), ["coffee"]);
}

fn a_body_survives_a_relaunch_from_its_snapshot<D: NoteDoc>() {
    let mut src = D::create(&[3u8; 16]);
    src.apply_local(&easynotes_core::crdt::Edit::SetKind(NoteKind::Text))
        .unwrap();
    src.apply_local(&easynotes_core::crdt::Edit::SetBody("hello".into()))
        .unwrap();
    let mut restored = D::load(&[1u8; 16], &src.snapshot()).unwrap();
    assert_eq!(restored.view(), src.view());
    let update = src
        .apply_local(&easynotes_core::crdt::Edit::SetBody("hello there".into()))
        .unwrap();
    restored.apply_remote(&update).unwrap();
    assert_eq!(restored.view().body, "hello there");
}

fn search_and_the_card_read_the_body<D: NoteDoc>() {
    let (mut a, _, id) = pair::<D>(NoteKind::Text);
    body(&mut a, id, "Call the plumber\n\nabout the boiler");
    let hits = a.search("boiler").unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].matched, "about the boiler");
    assert_eq!(hits[0].summary.kind, NoteKind::Text);
    assert_eq!(
        hits[0].summary.preview,
        ["Call the plumber", "about the boiler"]
    );
    assert_eq!(hits[0].summary.items, 0);
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
    a_new_text_note_says_so_on_both_devices,
    a_note_that_records_no_kind_is_a_checklist,
    setting_the_body_to_what_it_says_writes_nothing,
    concurrent_typing_in_one_body_keeps_both,
    an_edit_from_a_stale_screen_keeps_what_arrived_meanwhile,
    typing_outside_ascii_converges,
    text_to_checklist_makes_a_line_an_item,
    checklist_to_text_keeps_checked_items_as_lines,
    switching_there_and_back_keeps_the_lines,
    typing_while_the_other_device_converts_is_not_hidden,
    an_item_added_while_the_other_device_converts_is_not_hidden,
    a_body_survives_a_relaunch_from_its_snapshot,
    search_and_the_card_read_the_body,
);
