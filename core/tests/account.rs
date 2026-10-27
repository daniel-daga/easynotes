//! The account document: what one person thinks about their own notes.
//!
//! Two things are being held here. The first is that a placement *is* personal —
//! colouring a note must put nothing in the note, because the note is the thing
//! other people have. The second is that it behaves like the rest of the core:
//! it survives a relaunch, it converges, and a device that had notes before it
//! existed keeps them where they were.

use easynotes_core::crdt::account::{AccountDoc, AccountEdit, Value};
use easynotes_core::crdt::{Colour, Layout, NoteDoc, RepeatRule};
use easynotes_core::crypto::{Envelope, MasterKey};
use easynotes_core::ids::{LabelId, NoteId};
use easynotes_core::note::{
    NoteCore, NoteKind, NoteSummary, SyncCursor, ACCOUNT_DOC, COMPACT_AFTER, INDEX,
};
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

/// A set of label IDs, as the names a person would see. Sorted, because a
/// `BTreeSet<LabelId>` is in the order random bytes happen to fall in and a test
/// that asserted on that would be asserting on nothing.
fn names<D: NoteDoc, S: OpStore, K: Keyring>(
    core: &NoteCore<D, S, K>,
    labels: &std::collections::BTreeSet<LabelId>,
) -> Vec<String> {
    let mut out: Vec<String> = labels
        .iter()
        .filter_map(|id| core.tag_name(*id))
        .map(|name| name.to_string())
        .collect();
    out.sort();
    out
}

/// Every label with how many notes carry it, dropping the IDs — which are random
/// and so cannot be written down in a test.
fn counted<D: NoteDoc, S: OpStore, K: Keyring>(
    core: &NoteCore<D, S, K>,
) -> Vec<(&'static str, usize)> {
    core.tags()
        .into_iter()
        .map(|(_, name, count)| (Box::leak(name.into_boxed_str()) as &'static str, count))
        .collect()
}

// ---------------------------------------------------------------------------
// What a placement is, and is not
// ---------------------------------------------------------------------------

/// A colour is shared now, and everything else a person does to a note is not.
///
/// The colour has to reach a collaborator, so it is written into the note's own
/// document — encrypted with the note's key and sent to the relay like the rest
/// of the note. Pinning, archiving and the trash stay in the account document,
/// where they cannot repaint anyone else's card.
fn colouring_a_note_is_shared_but_pinning_is_not<D: NoteDoc>() {
    let mut core = in_memory::<D>();
    let id = core.create_note(NoteKind::Checklist).unwrap();
    core.set_title(id, "groceries").unwrap();
    let before = core.store().load(id).unwrap().len();

    core.set_colour(id, Some(Colour::Teal)).unwrap();
    assert_eq!(
        core.store().load(id).unwrap().len(),
        before + 1,
        "a shared colour belongs in the note's own log"
    );
    core.set_pinned(id, true).unwrap();
    core.set_archived(id, true).unwrap();
    assert_eq!(
        core.store().load(id).unwrap().len(),
        before + 1,
        "a personal placement was written to the note's own log"
    );
    // And it went to the account document too, which a build from before
    // colour was shared reads.
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

/// Reminders are private account state too: setting one must not append a
/// plaintext-or-ciphertext operation to the shared note's document.
fn a_reminder_lives_only_in_the_account_document<D: NoteDoc>() {
    let mut core = in_memory::<D>();
    let note = core.create_note(NoteKind::Checklist).unwrap();
    let before = core.store().load(note).unwrap().len();
    let reminder = core
        .create_reminder(
            note,
            1_800_000_000_000,
            RepeatRule::Weekly,
            "Europe/Vienna".into(),
        )
        .unwrap();

    assert_eq!(core.store().load(note).unwrap().len(), before);
    assert_eq!(core.reminders_on(note).len(), 1);
    assert_eq!(core.reminders_on(note)[0].id, reminder);
    assert!(!core.store().load(ACCOUNT_DOC).unwrap().is_empty());
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

/// What a drag reports. A drop lands on a card, and the screen it landed on is
/// filtered — so the move is named by the note underneath rather than by an
/// index counted off what happened to be drawn.
fn dropping_a_note_on_another_takes_its_place<D: NoteDoc>() {
    let mut core = in_memory::<D>();
    let mut ids = Vec::new();
    for title in ["a", "b", "c"] {
        let id = core.create_note(NoteKind::Checklist).unwrap();
        core.set_title(id, title).unwrap();
        ids.push(id);
    }
    // Newest first, so the grid reads c, b, a.
    let (a, b, c) = (ids[0], ids[1], ids[2]);
    assert_eq!(titles(&core.notes().unwrap()), ["c", "b", "a"]);

    // Dragged downwards: c lands after the card it was dropped on.
    core.move_note_to(c, b).unwrap();
    assert_eq!(titles(&core.notes().unwrap()), ["b", "c", "a"]);

    // Dragged upwards: a lands before it.
    core.move_note_to(a, b).unwrap();
    assert_eq!(titles(&core.notes().unwrap()), ["a", "b", "c"]);
}

/// The whole reason a drop names a note: the grid the drag happened on may be
/// showing three of this device's notes, and the order it writes into is over
/// all of them.
fn dropping_a_note_ignores_the_notes_that_were_not_drawn<D: NoteDoc>() {
    let mut core = in_memory::<D>();
    let mut ids = Vec::new();
    for title in ["a", "b", "c", "d"] {
        let id = core.create_note(NoteKind::Checklist).unwrap();
        core.set_title(id, title).unwrap();
        ids.push(id);
    }
    let (a, b) = (ids[0], ids[1]);
    assert_eq!(titles(&core.notes().unwrap()), ["d", "c", "b", "a"]);

    // c is archived, so the grid shows d, b, a — and b is at index 1 there and
    // at index 2 in the order. Dropping a on b has to mean the same either way.
    core.set_archived(ids[2], true).unwrap();
    core.move_note_to(a, b).unwrap();

    let all = core.notes().unwrap();
    let grid: Vec<&str> = all
        .iter()
        .filter(|note| !note.placement.archived)
        .map(|note| note.title.as_str())
        .collect();
    assert_eq!(grid, ["d", "a", "b"]);
    // And the archived note has not moved out from between them.
    assert_eq!(titles(&all), ["d", "c", "a", "b"]);
}

/// A drop that says nothing is not an error and writes nothing: a card released
/// on itself is the commonest way a drag ends.
fn dropping_a_note_on_itself_writes_nothing<D: NoteDoc>() {
    let mut core = in_memory::<D>();
    let id = core.create_note(NoteKind::Checklist).unwrap();
    core.notes().unwrap();
    let before = core.store().load(ACCOUNT_DOC).unwrap().len();

    core.move_note_to(id, id).unwrap();

    assert_eq!(core.store().load(ACCOUNT_DOC).unwrap().len(), before);
}

// ---------------------------------------------------------------------------
// Labels
// ---------------------------------------------------------------------------

/// The same property a colour has, for the same reason: a label is what *this
/// person* calls a note, and a household sharing a shopping list does not share
/// one person's filing.
fn labelling_a_note_puts_nothing_in_the_note<D: NoteDoc>() {
    let mut core = in_memory::<D>();
    let id = core.create_note(NoteKind::Checklist).unwrap();
    core.set_title(id, "groceries").unwrap();
    let before = core.store().load(id).unwrap().len();

    core.add_tag(id, "errands").unwrap();
    core.add_tag(id, "weekly shop").unwrap();

    assert_eq!(
        core.store().load(id).unwrap().len(),
        before,
        "a label was written to the note's own log"
    );
}

fn a_label_comes_back_on_the_summary<D: NoteDoc>() {
    let mut core = in_memory::<D>();
    let id = core.create_note(NoteKind::Checklist).unwrap();
    let errands = core.add_tag(id, "errands").unwrap().unwrap();
    let recipes = core.add_tag(id, "recipes").unwrap().unwrap();
    assert_eq!(core.tag_name(errands), Some("errands"));

    let notes = core.notes().unwrap();
    assert_eq!(
        names(&core, &notes[0].placement.tags),
        ["errands", "recipes"]
    );
    assert_eq!(counted(&core), [("errands", 1), ("recipes", 1)]);

    core.remove_tag(id, errands).unwrap();
    assert_eq!(names(&core, &core.placement(id).tags), ["recipes"]);
    // The label itself stays, carried by nobody. That is the half that changed
    // when a label stopped being its own name: taking it off the last note is
    // not the same gesture as throwing it away.
    assert_eq!(counted(&core), [("errands", 0), ("recipes", 1)]);
    assert!(core.tags().iter().any(|(id, _, _)| *id == recipes));
}

/// The reuse rule. Two labels that differ only in case are one label, decided
/// where the existing ones are known — otherwise a person who types `recipes`
/// under the `Recipes` they made last week gets two chips that look the same.
fn a_label_that_differs_only_in_case_is_the_one_already_there<D: NoteDoc>() {
    let mut core = in_memory::<D>();
    let first = core.create_note(NoteKind::Checklist).unwrap();
    let second = core.create_note(NoteKind::Checklist).unwrap();
    let recipes = core.add_tag(first, "Recipes").unwrap().unwrap();

    // On another note, under another casing, with whitespace around it.
    assert_eq!(core.add_tag(second, "  recipes ").unwrap(), Some(recipes));
    assert_eq!(counted(&core), [("Recipes", 2)]);

    core.remove_tag(second, recipes).unwrap();
    assert_eq!(counted(&core), [("Recipes", 1)]);
}

fn a_label_that_is_not_a_name_is_not_a_label<D: NoteDoc>() {
    let mut core = in_memory::<D>();
    let id = core.create_note(NoteKind::Checklist).unwrap();
    let before = core.store().load(ACCOUNT_DOC).unwrap().len();

    assert_eq!(core.add_tag(id, "   ").unwrap(), None);
    assert_eq!(core.add_tag(id, "").unwrap(), None);
    // Nothing that says nothing reaches the log.
    assert_eq!(core.store().load(ACCOUNT_DOC).unwrap().len(), before);
    assert!(core.tags().is_empty());

    // Nor does taking off a label that was never on.
    core.remove_tag(id, LabelId::random()).unwrap();
    assert_eq!(core.store().load(ACCOUNT_DOC).unwrap().len(), before);
}

/// A label is a placement, so it survives a relaunch the way a colour does —
/// through the log, on the disk, sealed.
fn a_label_survives_a_relaunch<D: NoteDoc>() {
    let dir = tempfile::tempdir().unwrap();
    let id = {
        let mut core = on_disk::<D>(dir.path());
        let id = core.create_note(NoteKind::Checklist).unwrap();
        core.set_title(id, "groceries").unwrap();
        core.add_tag(id, "weekly shop").unwrap();
        id
    };
    let mut core = on_disk::<D>(dir.path());
    let notes = core.notes().unwrap();
    assert_eq!(names(&core, &notes[0].placement.tags), ["weekly shop"]);
    assert_eq!(counted(&core), [("weekly shop", 1)]);
    assert_eq!(core.placement(id).tags.len(), 1);
}

// ---------------------------------------------------------------------------
// Renaming a label
// ---------------------------------------------------------------------------

/// The thing a label having an identity is for.
///
/// One register write: every note keeps the label, because every membership
/// points at the ID and no membership has ever mentioned the name.
fn a_label_can_be_renamed_and_its_notes_keep_it<D: NoteDoc>() {
    let mut core = in_memory::<D>();
    let one = core.create_note(NoteKind::Checklist).unwrap();
    let two = core.create_note(NoteKind::Checklist).unwrap();
    let label = core.add_tag(one, "Recipes").unwrap().unwrap();
    core.add_tag(two, "Recipes").unwrap();

    assert_eq!(core.rename_tag(label, "  Dinners ").unwrap(), Some(label));

    assert_eq!(core.tag_name(label), Some("Dinners"));
    assert_eq!(counted(&core), [("Dinners", 2)]);
    for note in [one, two] {
        assert_eq!(names(&core, &core.placement(note).tags), ["Dinners"]);
    }
    // And the old name is free again.
    let fresh = core.add_tag(one, "Recipes").unwrap().unwrap();
    assert_ne!(fresh, label);
}

/// Renaming onto a name another label already has merges the two.
///
/// Keep's behaviour, and the only answer that is not a lie: the person asked for
/// one label with that name, and refusing would leave them with two that look
/// identical. It is also the repair for the one residue `add_tag` cannot
/// prevent — two devices each inventing the same name offline — which nothing
/// could repair when a label *was* its name.
fn renaming_a_label_onto_another_merges_them<D: NoteDoc>() {
    let mut core = in_memory::<D>();
    let one = core.create_note(NoteKind::Checklist).unwrap();
    let two = core.create_note(NoteKind::Checklist).unwrap();
    let three = core.create_note(NoteKind::Checklist).unwrap();
    let shopping = core.add_tag(one, "shopping").unwrap().unwrap();
    let errands = core.add_tag(two, "errands").unwrap().unwrap();
    core.add_tag(three, "errands").unwrap();

    assert_eq!(core.rename_tag(shopping, "Errands").unwrap(), Some(errands));

    // One label, carrying every note either of them had.
    assert_eq!(counted(&core), [("errands", 3)]);
    for note in [one, two, three] {
        assert_eq!(core.tags_on(note), vec![errands]);
    }
    assert_eq!(core.tag_name(shopping), None);
}

/// Renaming a label to what it is already called, up to case, is a recasing
/// rather than a merge with itself.
fn a_label_can_be_recased<D: NoteDoc>() {
    let mut core = in_memory::<D>();
    let id = core.create_note(NoteKind::Checklist).unwrap();
    let label = core.add_tag(id, "recipes").unwrap().unwrap();

    assert_eq!(core.rename_tag(label, "Recipes").unwrap(), Some(label));
    assert_eq!(counted(&core), [("Recipes", 1)]);
    assert_eq!(core.tags_on(id), vec![label]);
}

fn renaming_to_nothing_or_renaming_nothing_does_nothing<D: NoteDoc>() {
    let mut core = in_memory::<D>();
    let id = core.create_note(NoteKind::Checklist).unwrap();
    let label = core.add_tag(id, "recipes").unwrap().unwrap();
    let before = core.store().load(ACCOUNT_DOC).unwrap().len();

    assert_eq!(core.rename_tag(label, "  ").unwrap(), None);
    assert_eq!(
        core.rename_tag(LabelId::random(), "anything").unwrap(),
        None
    );

    assert_eq!(core.store().load(ACCOUNT_DOC).unwrap().len(), before);
    assert_eq!(counted(&core), [("recipes", 1)]);
}

/// A label with no notes is a label. There was nowhere to hold one before.
fn a_label_can_exist_before_any_note_has_it<D: NoteDoc>() {
    let mut core = in_memory::<D>();
    let label = core.make_tag("Someday").unwrap();
    assert_eq!(counted(&core), [("Someday", 0)]);

    // And asking for it again is the same label, not a second one.
    assert_eq!(core.make_tag("someday").unwrap(), label);

    let id = core.create_note(NoteKind::Checklist).unwrap();
    assert_eq!(core.add_tag(id, "SOMEDAY").unwrap(), Some(label));
    assert_eq!(counted(&core), [("Someday", 1)]);
}

/// Throwing a label away is its own gesture, and it takes the label off every
/// note that had it without touching the notes themselves.
fn forgetting_a_label_takes_it_off_every_note<D: NoteDoc>() {
    let mut core = in_memory::<D>();
    let one = core.create_note(NoteKind::Checklist).unwrap();
    let two = core.create_note(NoteKind::Checklist).unwrap();
    core.set_title(one, "groceries").unwrap();
    let errands = core.add_tag(one, "errands").unwrap().unwrap();
    core.add_tag(two, "errands").unwrap();
    let recipes = core.add_tag(one, "recipes").unwrap().unwrap();

    core.forget_tag(errands).unwrap();

    assert_eq!(counted(&core), [("recipes", 1)]);
    assert_eq!(core.tags_on(one), vec![recipes]);
    assert!(core.tags_on(two).is_empty());
    // The notes are still there, with everything else about them intact. Newest
    // first, which is where the grid puts a note nobody has moved.
    assert_eq!(titles(&core.notes().unwrap()), vec!["", "groceries"]);
}

// ---------------------------------------------------------------------------
// The shape a label used to have
// ---------------------------------------------------------------------------

/// A device that labelled notes on the old build keeps those labels.
///
/// The old shape made a label out of its name — `t:<note>:<name>` and nothing
/// else — so there is no ID to recover and one has to be minted. Names matching
/// ignoring case land on one label, which is what `add_tag` would have done had
/// they been typed on this build.
fn labels_stored_the_old_way_are_adopted_at_launch<D: NoteDoc>() {
    let dir = tempfile::tempdir().unwrap();
    let (one, two) = {
        let mut core = on_disk::<D>(dir.path());
        let one = core.create_note(NoteKind::Checklist).unwrap();
        let two = core.create_note(NoteKind::Checklist).unwrap();
        core.set_title(one, "groceries").unwrap();
        // Written the way the old build wrote them, which is the only way to
        // produce a device in this state now.
        for (note, tag) in [(one, "Errands"), (one, "recipes"), (two, "errands")] {
            core.write_legacy_tag(note, tag).unwrap();
        }
        (one, two)
    };

    let mut core = on_disk::<D>(dir.path());
    core.notes().unwrap();

    // Two labels, not three: `Errands` and `errands` are one label. Which casing
    // survives is decided by sorting the names rather than by note order, so it
    // is the same answer on every device migrating the same document — see
    // `adopt_legacy_tags`.
    assert_eq!(counted(&core), [("Errands", 2), ("recipes", 1)]);
    let errands = core.tags()[0].0;
    assert!(core.tags_on(one).contains(&errands));
    assert_eq!(core.tags_on(two), vec![errands]);

    // The old keys are gone, so the next launch finds nothing to do.
    assert!(core.placement(one).legacy_tags.is_empty());
    assert!(core.placement(two).legacy_tags.is_empty());
    let settled = core.store().load(ACCOUNT_DOC).unwrap().len();
    let mut core = on_disk::<D>(dir.path());
    core.notes().unwrap();
    assert_eq!(
        core.store().load(ACCOUNT_DOC).unwrap().len(),
        settled,
        "the migration wrote something on a device with nothing to migrate"
    );
    assert_eq!(counted(&core), [("Errands", 2), ("recipes", 1)]);
}

/// And once adopted they behave like any other label, including the thing they
/// could never do before.
fn a_label_adopted_from_the_old_shape_can_be_renamed<D: NoteDoc>() {
    let dir = tempfile::tempdir().unwrap();
    let id = {
        let mut core = on_disk::<D>(dir.path());
        let id = core.create_note(NoteKind::Checklist).unwrap();
        core.write_legacy_tag(id, "errands").unwrap();
        id
    };

    let mut core = on_disk::<D>(dir.path());
    let errands = core.tags()[0].0;
    assert_eq!(core.rename_tag(errands, "Chores").unwrap(), Some(errands));
    assert_eq!(counted(&core), [("Chores", 1)]);
    assert_eq!(core.tags_on(id), vec![errands]);
}

/// Forgetting a note takes its memberships with it, in both shapes, and leaves
/// the labels standing.
fn forgetting_a_note_takes_its_labels_with_it<D: NoteDoc>() {
    let mut core = in_memory::<D>();
    let gone = core.create_note(NoteKind::Checklist).unwrap();
    let kept = core.create_note(NoteKind::Checklist).unwrap();
    core.add_tag(gone, "errands").unwrap();
    core.add_tag(gone, "recipes").unwrap();
    core.add_tag(kept, "recipes").unwrap();

    core.forget_note(gone).unwrap();
    core.notes().unwrap();

    // The note is off both labels. Both labels are still there — one carried by
    // the note that kept it, one carried by nobody.
    assert_eq!(counted(&core), [("errands", 0), ("recipes", 1)]);
    assert!(core.placement(gone).tags.is_empty());
}

/// Two labels on one note are two keys, which is what makes two devices adding
/// two of them keep both. Against the document directly, for the reason
/// [`two_devices_keep_both_placements`] is: there is no sync for this document
/// yet, and this is the property that has to hold when there is.
fn two_devices_keep_both_labels<D: NoteDoc>() {
    let note = NoteId::random();
    let mut phone = <D::Account as AccountDoc>::create(&[5u8; 16]);
    let mut laptop = <D::Account as AccountDoc>::create(&[6u8; 16]);
    let errands_id = LabelId::random();
    let recipes_id = LabelId::random();

    let errands = phone
        .apply_local(&AccountEdit::SetTag {
            note,
            label: errands_id,
            on: true,
        })
        .unwrap();
    let recipes = laptop
        .apply_local(&AccountEdit::SetTag {
            note,
            label: recipes_id,
            on: true,
        })
        .unwrap();

    phone.apply_remote(&recipes).unwrap();
    phone.apply_remote(&recipes).unwrap(); // and again, for nothing
    laptop.apply_remote(&errands).unwrap();

    for doc in [&phone, &laptop] {
        assert_eq!(
            doc.view()
                .placement(note)
                .tags
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            {
                let mut both = vec![errands_id, recipes_id];
                both.sort();
                both
            }
        );
    }
    assert_eq!(phone.view(), laptop.view());
}

/// Two devices renaming one label converge on one name, and neither note loses
/// its label on the way. The property the whole redesign exists to make true.
fn two_devices_renaming_one_label_converge<D: NoteDoc>() {
    let note = NoteId::random();
    let label = LabelId::random();
    let mut phone = <D::Account as AccountDoc>::create(&[5u8; 16]);
    let mut laptop = <D::Account as AccountDoc>::create(&[6u8; 16]);

    // Both start from a note carrying the label.
    let seed = phone
        .apply_local(&AccountEdit::SetTag {
            note,
            label,
            on: true,
        })
        .unwrap();
    laptop.apply_remote(&seed).unwrap();

    let a = phone
        .apply_local(&AccountEdit::SetLabelName {
            label,
            name: "Dinners".into(),
        })
        .unwrap();
    let b = laptop
        .apply_local(&AccountEdit::SetLabelName {
            label,
            name: "Chores".into(),
        })
        .unwrap();
    phone.apply_remote(&b).unwrap();
    laptop.apply_remote(&a).unwrap();

    // One name — whichever the backend's register picked — and both devices
    // agree on it. The membership is untouched by either write.
    assert_eq!(phone.view(), laptop.view());
    let name = phone.view().label_name(label).map(|s| s.to_string());
    assert!(matches!(name.as_deref(), Some("Dinners" | "Chores")));
    assert_eq!(phone.view().labels_on(note), vec![label]);
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

// ---------------------------------------------------------------------------
// Forgetting a note
// ---------------------------------------------------------------------------

/// What the trash was always going to need. A forgotten note leaves nothing: no
/// log, no snapshot, no index entry, no key, and no placement.
fn forgetting_a_note_leaves_nothing_of_it_anywhere<D: NoteDoc>() {
    let mut core = in_memory::<D>();
    let gone = core.create_note(NoteKind::Checklist).unwrap();
    core.set_title(gone, "tax papers").unwrap();
    core.add_item(gone, "shred this").unwrap();
    core.set_colour(gone, Some(Colour::Red)).unwrap();
    core.set_trashed(gone, true).unwrap();
    let kept = core.create_note(NoteKind::Checklist).unwrap();
    core.set_title(kept, "groceries").unwrap();
    core.notes().unwrap();

    core.forget_note(gone).unwrap();

    // Not in the list, and not in the store the list is built from.
    assert_eq!(titles(&core.notes().unwrap()), vec!["groceries"]);
    assert!(!core.store().notes().unwrap().contains(&gone));
    assert!(core.store().load(gone).unwrap().is_empty());
    assert!(core.store().get_snapshot(gone).unwrap().is_none());
    assert!(core.store().get_meta(gone, INDEX).unwrap().is_none());
    assert!(core.keyring().note_key(gone).unwrap().is_none());
    // And this person's opinion of it went with it, rather than being left in a
    // document that nothing compacts.
    assert!(!core.account().placements.contains_key(&gone));

    // The note beside it is untouched, including its key and its placement.
    assert!(core.keyring().note_key(kept).unwrap().is_some());
    assert_eq!(core.view(kept).unwrap().title, "groceries");
}

/// The note cannot be opened again, by anything, including a share blob that
/// somebody kept.
fn a_forgotten_note_cannot_be_opened<D: NoteDoc>() {
    let mut core = in_memory::<D>();
    let id = core.create_note(NoteKind::Checklist).unwrap();
    core.set_title(id, "gone").unwrap();
    let blob = core.export_share_blob(id).unwrap();

    core.forget_note(id).unwrap();

    assert!(core.open_note(id).is_err());
    assert!(core.view(id).is_err());
    // A blob carries the key, and it still cannot put the note back: the
    // account's deletion marker outranks every way a key can arrive (ADR 0022).
    assert!(core.import_share_blob(&blob).is_err());
    assert!(core.view(id).is_err());
}

/// It stays forgotten, and nothing gives the note a placement again.
///
/// The second half is the one that could quietly regress: `notes` gives every
/// unplaced note in the store an order key, so a note whose rows were removed
/// but whose metadata survived would come back as an empty card at the bottom of
/// the grid.
fn a_forgotten_note_is_still_gone_after_a_relaunch<D: NoteDoc>() {
    let dir = tempfile::tempdir().unwrap();
    let (gone, kept) = {
        let mut core = on_disk::<D>(dir.path());
        let gone = core.create_note(NoteKind::Checklist).unwrap();
        core.set_title(gone, "tax papers").unwrap();
        let kept = core.create_note(NoteKind::Checklist).unwrap();
        core.set_title(kept, "groceries").unwrap();
        core.notes().unwrap();
        core.forget_note(gone).unwrap();
        (gone, kept)
    };

    let mut core = on_disk::<D>(dir.path());
    assert_eq!(titles(&core.notes().unwrap()), vec!["groceries"]);
    assert!(!core.account().placements.contains_key(&gone));
    assert!(core.keyring().note_key(gone).unwrap().is_none());
    assert!(core.keyring().note_key(kept).unwrap().is_some());
}

/// The residue a forgotten note can leave, and the sweep that takes it.
///
/// Forgetting closes the note's relay connection first, but the engine's task
/// can already be mid-delivery: the envelope that arrives is rejected — no key —
/// and the cursor moves past it anyway, which writes a store row for a note that
/// has just been deleted. That row is a note as far as `store.notes` is
/// concerned, and before the sweep nothing ever removed it.
fn a_late_envelope_leaves_no_note_behind<D: NoteDoc>() {
    let mut core = in_memory::<D>();
    let gone = core.create_note(NoteKind::Checklist).unwrap();
    core.set_title(gone, "tax papers").unwrap();
    let kept = core.create_note(NoteKind::Checklist).unwrap();
    core.set_title(kept, "groceries").unwrap();
    core.notes().unwrap();

    core.forget_note(gone).unwrap();
    // The engine's last act for a note it is being disconnected from. Nothing
    // here is contrived: `run_note` saves the cursor after `apply_remote` fails.
    // The account's deletion marker makes that a no-op (ADR 0022), so it
    // leaves no row at all.
    core.set_sync_cursor(
        gone,
        SyncCursor {
            last_seq: 7,
            sent_upto: None,
        },
    )
    .unwrap();
    assert!(!core.store().notes().unwrap().contains(&gone));

    // A list is drawn, which is where the sweep runs.
    assert_eq!(titles(&core.notes().unwrap()), vec!["groceries"]);
    assert!(!core.store().notes().unwrap().contains(&gone));
    // And it took only the row it was aimed at.
    assert!(core.store().notes().unwrap().contains(&kept));
}

/// The sweep tests for ciphertext, not for a key — because a missing key is the
/// one thing the later milestone is meant to be able to undo.
///
/// This is the shape `copy_store` produces when a log is carried over and its
/// key is not, and the shape a device will be in between restoring a store and
/// restoring the keys that open it. There is no placement and no key, so it
/// reaches the sweep; the log is what stops it.
fn a_note_whose_key_never_arrived_is_not_swept<D: NoteDoc>() {
    let mut source = in_memory::<D>();
    let id = source.create_note(NoteKind::Checklist).unwrap();
    source.set_title(id, "somebody else's").unwrap();
    let log = source.store().load(id).unwrap();
    assert!(!log.is_empty());

    let mut store = MemoryStore::new();
    for envelope in &log {
        store.append(id, envelope).unwrap();
    }
    let mut core: Memory<D> = NoteCore::open(store, MemoryKeyring::new()).unwrap();

    // It cannot be drawn, and it is still on the disk.
    assert!(core.notes().unwrap().is_empty());
    assert!(core.store().notes().unwrap().contains(&id));
    assert_eq!(core.store().load(id).unwrap().len(), log.len());
}

/// Emptying the trash takes the trash and only the trash.
fn emptying_the_trash_forgets_what_is_in_it<D: NoteDoc>() {
    let mut core = in_memory::<D>();
    let mut ids = Vec::new();
    for title in ["one", "two", "three", "archived", "kept"] {
        let id = core.create_note(NoteKind::Checklist).unwrap();
        core.set_title(id, title).unwrap();
        ids.push(id);
    }
    for id in &ids[..3] {
        core.set_trashed(*id, true).unwrap();
    }
    // The archive is not the trash, which is the mistake this guards against.
    core.set_archived(ids[3], true).unwrap();

    assert_eq!(core.empty_trash().unwrap(), 3);

    let left = core.notes().unwrap();
    let mut names = titles(&left);
    names.sort();
    assert_eq!(names, vec!["archived", "kept"]);
    for id in &ids[..3] {
        assert!(core.keyring().note_key(*id).unwrap().is_none());
        assert!(core.store().load(*id).unwrap().is_empty());
    }
    // Twice is not an error and finds nothing the second time.
    assert_eq!(core.empty_trash().unwrap(), 0);
}

/// Emptying a trash that nothing was ever put in changes nothing at all — not
/// even one envelope in the account document, which is not compacted.
fn emptying_an_empty_trash_writes_nothing<D: NoteDoc>() {
    let mut core = in_memory::<D>();
    let id = core.create_note(NoteKind::Checklist).unwrap();
    core.set_title(id, "groceries").unwrap();
    core.notes().unwrap();
    let logged = core.store().load(ACCOUNT_DOC).unwrap().len();

    assert_eq!(core.empty_trash().unwrap(), 0);

    assert_eq!(core.store().load(ACCOUNT_DOC).unwrap().len(), logged);
    assert_eq!(titles(&core.notes().unwrap()), vec!["groceries"]);
}

/// The account document is not a note and cannot be forgotten as one. Doing it
/// would take every colour, pin and placement this person has with it.
fn the_account_document_cannot_be_forgotten<D: NoteDoc>() {
    let mut core = in_memory::<D>();
    let id = core.create_note(NoteKind::Checklist).unwrap();
    core.set_colour(id, Some(Colour::Teal)).unwrap();

    assert!(core.forget_note(ACCOUNT_DOC).is_err());

    assert_eq!(core.placement(id).colour, Some(Colour::Teal));
    assert!(core.keyring().note_key(ACCOUNT_DOC).unwrap().is_some());
    assert!(!core.store().load(ACCOUNT_DOC).unwrap().is_empty());
}

/// Whoever is watching the grid is told, including when the note being forgotten
/// had no placement of its own to clear.
fn forgetting_a_note_reaches_the_list<D: NoteDoc>() {
    let mut core = in_memory::<D>();
    let id = core.create_note(NoteKind::Checklist).unwrap();
    core.set_title(id, "gone").unwrap();
    core.notes().unwrap();
    let mut watch = core.watch_notes();
    watch.borrow_and_update();

    core.forget_note(id).unwrap();

    assert!(watch.has_changed().unwrap(), "the list was not refreshed");
    assert!(watch.borrow_and_update().is_empty());
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

/// The reminder equivalent of a label membership: two independent reminder IDs
/// are four independent flat-map keys each, so neither device overwrites the
/// other while offline.
fn two_devices_keep_both_reminders<D: NoteDoc>() {
    let note = NoteId::random();
    let first = easynotes_core::ids::ReminderId::random();
    let second = easynotes_core::ids::ReminderId::random();
    let mut phone = <D::Account as AccountDoc>::create(&[1u8; 16]);
    let mut laptop = <D::Account as AccountDoc>::create(&[2u8; 16]);

    let one = phone
        .apply_local(&AccountEdit::SetReminder {
            reminder: easynotes_core::crdt::Reminder {
                id: first,
                note,
                fire_at: 1_800_000_000_000,
                repeat: RepeatRule::Never,
                timezone: "Europe/Vienna".into(),
            },
        })
        .unwrap();
    let two = laptop
        .apply_local(&AccountEdit::SetReminder {
            reminder: easynotes_core::crdt::Reminder {
                id: second,
                note,
                fire_at: 1_800_000_100_000,
                repeat: RepeatRule::Daily,
                timezone: "Europe/Vienna".into(),
            },
        })
        .unwrap();

    phone.apply_remote(&two).unwrap();
    laptop.apply_remote(&one).unwrap();
    assert_eq!(phone.view().reminders_on(note).len(), 2);
    assert_eq!(phone.view(), laptop.view());
}

fn forgetting_a_note_takes_its_reminders_with_it<D: NoteDoc>() {
    let mut core = in_memory::<D>();
    let note = core.create_note(NoteKind::Checklist).unwrap();
    core.create_reminder(note, 1_800_000_000_000, RepeatRule::Never, "UTC".into())
        .unwrap();
    core.forget_note(note).unwrap();
    assert!(core.reminders_on(note).is_empty());
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
// Compacting it
// ---------------------------------------------------------------------------

/// A device whose account document has been edited more times than
/// [`COMPACT_AFTER`], and says enough for "it came back" to mean something.
///
/// A few notes with a colour, a label and an archive between them, because a
/// document of two keys would be reconstructed correctly by almost any mistake;
/// then one note pinned and unpinned until the log is long, because what grows
/// this log is placements *made* rather than notes held. The returned note is
/// the one being toggled.
fn with_a_long_account_log<D: NoteDoc>(edits: usize) -> (Memory<D>, NoteId) {
    let mut core = in_memory::<D>();
    for n in 0..5 {
        let other = core.create_note(NoteKind::Checklist).unwrap();
        core.set_title(other, format!("list {n}")).unwrap();
        core.set_colour(other, Some(Colour::ALL[n % Colour::ALL.len()]))
            .unwrap();
        core.add_tag(other, "errands").unwrap();
        if n % 2 == 0 {
            core.set_archived(other, true).unwrap();
        }
    }
    core.set_layout(Layout::List).unwrap();

    let id = core.create_note(NoteKind::Checklist).unwrap();
    core.set_title(id, "groceries").unwrap();
    for n in 0..edits {
        core.set_pinned(id, n % 2 == 0).unwrap();
    }
    assert!(
        core.store().load(ACCOUNT_DOC).unwrap().len() > COMPACT_AFTER,
        "the log this is about is not long enough to be compacted"
    );
    (core, id)
}

/// Relaunch the same store and keyring, which is what the app killed and started
/// again does.
fn relaunch<D: NoteDoc>(core: Memory<D>) -> Memory<D> {
    let (store, keyring) = core.into_parts();
    NoteCore::open(store, keyring).unwrap()
}

/// The launch after the threshold is passed replaces the whole log with one
/// envelope, and the document it comes back as is the one that went in.
fn a_long_account_log_is_compacted_at_launch<D: NoteDoc>() {
    let (core, id) = with_a_long_account_log::<D>(COMPACT_AFTER + 4);
    let before = core.store().load(ACCOUNT_DOC).unwrap().len();
    let placement = core.placement(id);
    let layout = core.layout();

    let core = relaunch(core);

    assert_eq!(
        core.store().load(ACCOUNT_DOC).unwrap().len(),
        1,
        "a compacted account document is one envelope, whatever it was before"
    );
    assert!(before > 1);
    assert_eq!(core.placement(id), placement, "a placement did not survive");
    assert_eq!(core.layout(), layout);

    // And the launch after that reads what the first one wrote, rather than
    // being the first to read it.
    let core = relaunch(core);
    assert_eq!(core.store().load(ACCOUNT_DOC).unwrap().len(), 1);
    assert_eq!(core.placement(id), placement);
}

/// The whole of [ADR 0012](../../docs/decisions.md#adr-0012)'s
/// safety argument, and the reason this may drop what a note's compaction may
/// not: the snapshot is an envelope *in the log*, so the log is still the whole
/// document to anyone who replays it — which is what a second device would do,
/// and what the outbox hands a relay.
///
/// Replayed here the way a peer would: into a document this device has never
/// touched, through `apply_remote` and not `load`.
fn a_compacted_account_log_is_still_the_whole_document<D: NoteDoc>() {
    let (core, id) = with_a_long_account_log::<D>(COMPACT_AFTER + 4);
    let mut core = relaunch(core);
    assert_eq!(core.store().load(ACCOUNT_DOC).unwrap().len(), 1);
    core.set_colour(id, Some(Colour::Teal)).unwrap();
    core.set_pinned(id, true).unwrap();
    core.add_tag(id, "errands").unwrap();

    let key = core.keyring().note_key(ACCOUNT_DOC).unwrap().unwrap();
    let mut peer = <D::Account as AccountDoc>::create(&[9u8; 16]);
    for raw in core.store().load(ACCOUNT_DOC).unwrap() {
        let envelope = Envelope::decode(&raw).unwrap();
        peer.apply_remote(&envelope.open(&key).unwrap()).unwrap();
    }

    assert_eq!(
        peer.view(),
        *core.account(),
        "a device replaying the compacted log did not arrive at this document"
    );
    // Not vacuously: there are six notes placed, five of them coloured and
    // labelled, one of them pinned and coloured here, and a layout.
    let view = peer.view();
    assert_eq!(view.placements.len(), 6);
    assert_eq!(view.placement(id).colour, Some(Colour::Teal));
    assert!(view.placement(id).pinned);
    assert_eq!(
        view.labels_on(id)
            .iter()
            .filter_map(|l| view.label_name(*l))
            .collect::<Vec<_>>(),
        ["errands"]
    );
    assert_eq!(view.tags().len(), 1);
}

/// The outbox is derived from the log, so what a relay would be told is whatever
/// compaction left there — and it has to be our own, above the cursor, and
/// enough to rebuild the document. One envelope, and it is the snapshot.
fn a_compacted_account_document_has_the_whole_document_in_its_outbox<D: NoteDoc>() {
    let (core, _) = with_a_long_account_log::<D>(COMPACT_AFTER + 4);
    let sent = core.unsent(ACCOUNT_DOC, None).unwrap().len();
    assert!(
        sent > COMPACT_AFTER,
        "nothing was acknowledged, so all of it"
    );

    let core = relaunch(core);
    let outbox = core.unsent(ACCOUNT_DOC, None).unwrap();
    assert_eq!(outbox.len(), 1);
    assert_eq!(
        outbox[0].1,
        core.store().load(ACCOUNT_DOC).unwrap()[0],
        "the outbox and the log disagree about what this device has to send"
    );
}

/// [ADR 0006](../../docs/decisions.md#adr-0006)'s decision 4, for this
/// document: the snapshot is sealed with a counter from this device's own
/// sequence, so the counter survives a compaction that removed every envelope it
/// could otherwise be read from. Reusing one would make this device's next edit
/// look like a replay to a peer that had already seen the number.
fn a_compacted_account_document_does_not_reuse_a_counter<D: NoteDoc>() {
    let (core, id) = with_a_long_account_log::<D>(COMPACT_AFTER + 4);
    let highest = |core: &Memory<D>| {
        core.store()
            .load(ACCOUNT_DOC)
            .unwrap()
            .iter()
            .map(|raw| Envelope::decode(raw).unwrap().counter)
            .max()
            .unwrap()
    };
    let before = highest(&core);

    let mut core = relaunch(core);
    let snapshot_counter = highest(&core);
    assert!(
        snapshot_counter > before,
        "the snapshot took a counter this device had already used"
    );

    core.set_colour(id, Some(Colour::Teal)).unwrap();
    assert!(
        highest(&core) > snapshot_counter,
        "the edit after a compaction reused the snapshot's counter"
    );

    // And the launch after that carries on from there rather than from zero.
    let mut core = relaunch(core);
    let carried = highest(&core);
    core.set_pinned(id, true).unwrap();
    assert!(highest(&core) > carried);
}

/// A log that has not passed the threshold is left exactly as it was. Rewriting
/// it would cost a signature and a store write on every launch for nothing.
fn a_short_account_log_is_left_alone<D: NoteDoc>() {
    let mut core = in_memory::<D>();
    let id = core.create_note(NoteKind::Checklist).unwrap();
    core.set_colour(id, Some(Colour::Purple)).unwrap();
    let before = core.store().load(ACCOUNT_DOC).unwrap();
    assert!(before.len() > 1 && before.len() <= COMPACT_AFTER);

    let core = relaunch(core);
    assert_eq!(
        core.store().load(ACCOUNT_DOC).unwrap(),
        before,
        "an account document under the threshold was rewritten anyway"
    );
}

/// Compacting what is already one envelope is not worth a second one, so asking
/// twice writes once.
fn compacting_the_account_document_again_changes_nothing<D: NoteDoc>() {
    let (core, _) = with_a_long_account_log::<D>(COMPACT_AFTER + 4);
    let mut core = relaunch(core);
    let compacted = core.store().load(ACCOUNT_DOC).unwrap();

    assert_eq!(core.compact_account().unwrap(), 0);
    assert_eq!(core.store().load(ACCOUNT_DOC).unwrap(), compacted);
}

/// The first thing that ever took something *out* of this document
/// ([ADR 0010](../../docs/decisions.md#adr-0010)) still takes it out
/// when what it is removing from is a snapshot rather than a log of appends.
fn a_note_forgotten_after_a_compaction_stays_forgotten<D: NoteDoc>() {
    let (core, gone) = with_a_long_account_log::<D>(COMPACT_AFTER + 4);
    let mut core = relaunch(core);
    assert_eq!(core.store().load(ACCOUNT_DOC).unwrap().len(), 1);
    let kept = core.create_note(NoteKind::Checklist).unwrap();

    core.set_trashed(gone, true).unwrap();
    core.forget_note(gone).unwrap();

    let core = relaunch(core);
    assert!(
        !core.account().placements.contains_key(&gone),
        "a note forgotten after a compaction came back with the next one"
    );
    assert!(core.account().placements.contains_key(&kept));
}

/// The same on the store the app actually uses, opened the way a relaunch opens
/// it. The store is where the atomicity of `replace_log` lives, and a memory one
/// cannot fail to be atomic.
fn an_account_document_on_disk_is_compacted_once<D: NoteDoc>() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = on_disk::<D>(dir.path());
    let id = core.create_note(NoteKind::Checklist).unwrap();
    for n in 0..COMPACT_AFTER + 4 {
        core.set_pinned(id, n % 2 == 0).unwrap();
    }
    let placement = core.placement(id);
    drop(core);

    let core = on_disk::<D>(dir.path());
    assert_eq!(core.store().load(ACCOUNT_DOC).unwrap().len(), 1);
    assert_eq!(core.placement(id), placement);
    drop(core);

    let mut core = on_disk::<D>(dir.path());
    assert_eq!(core.store().load(ACCOUNT_DOC).unwrap().len(), 1);
    assert_eq!(core.placement(id), placement);
    assert_eq!(core.notes().unwrap().len(), 1);
}

// ---------------------------------------------------------------------------

macro_rules! backend_suite {
    ($name:ident, $backend:ty) => {
        mod $name {
            #[test]
            fn colouring_a_note_is_shared_but_pinning_is_not() {
                super::colouring_a_note_is_shared_but_pinning_is_not::<$backend>()
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
            fn a_reminder_lives_only_in_the_account_document() {
                super::a_reminder_lives_only_in_the_account_document::<$backend>()
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
            fn dropping_a_note_on_another_takes_its_place() {
                super::dropping_a_note_on_another_takes_its_place::<$backend>()
            }
            #[test]
            fn dropping_a_note_ignores_the_notes_that_were_not_drawn() {
                super::dropping_a_note_ignores_the_notes_that_were_not_drawn::<$backend>()
            }
            #[test]
            fn dropping_a_note_on_itself_writes_nothing() {
                super::dropping_a_note_on_itself_writes_nothing::<$backend>()
            }
            #[test]
            fn a_placement_reaches_the_list() {
                super::a_placement_reaches_the_list::<$backend>()
            }
            #[test]
            fn labelling_a_note_puts_nothing_in_the_note() {
                super::labelling_a_note_puts_nothing_in_the_note::<$backend>()
            }
            #[test]
            fn a_label_comes_back_on_the_summary() {
                super::a_label_comes_back_on_the_summary::<$backend>()
            }
            #[test]
            fn a_label_that_differs_only_in_case_is_the_one_already_there() {
                super::a_label_that_differs_only_in_case_is_the_one_already_there::<$backend>()
            }
            #[test]
            fn a_label_that_is_not_a_name_is_not_a_label() {
                super::a_label_that_is_not_a_name_is_not_a_label::<$backend>()
            }
            #[test]
            fn a_label_survives_a_relaunch() {
                super::a_label_survives_a_relaunch::<$backend>()
            }
            #[test]
            fn forgetting_a_note_takes_its_labels_with_it() {
                super::forgetting_a_note_takes_its_labels_with_it::<$backend>()
            }
            #[test]
            fn a_label_can_be_renamed_and_its_notes_keep_it() {
                super::a_label_can_be_renamed_and_its_notes_keep_it::<$backend>()
            }
            #[test]
            fn renaming_a_label_onto_another_merges_them() {
                super::renaming_a_label_onto_another_merges_them::<$backend>()
            }
            #[test]
            fn a_label_can_be_recased() {
                super::a_label_can_be_recased::<$backend>()
            }
            #[test]
            fn renaming_to_nothing_or_renaming_nothing_does_nothing() {
                super::renaming_to_nothing_or_renaming_nothing_does_nothing::<$backend>()
            }
            #[test]
            fn a_label_can_exist_before_any_note_has_it() {
                super::a_label_can_exist_before_any_note_has_it::<$backend>()
            }
            #[test]
            fn forgetting_a_label_takes_it_off_every_note() {
                super::forgetting_a_label_takes_it_off_every_note::<$backend>()
            }
            #[test]
            fn labels_stored_the_old_way_are_adopted_at_launch() {
                super::labels_stored_the_old_way_are_adopted_at_launch::<$backend>()
            }
            #[test]
            fn a_label_adopted_from_the_old_shape_can_be_renamed() {
                super::a_label_adopted_from_the_old_shape_can_be_renamed::<$backend>()
            }
            #[test]
            fn two_devices_renaming_one_label_converge() {
                super::two_devices_renaming_one_label_converge::<$backend>()
            }
            #[test]
            fn two_devices_keep_both_labels() {
                super::two_devices_keep_both_labels::<$backend>()
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
            fn two_devices_keep_both_reminders() {
                super::two_devices_keep_both_reminders::<$backend>()
            }
            #[test]
            fn forgetting_a_note_takes_its_reminders_with_it() {
                super::forgetting_a_note_takes_its_reminders_with_it::<$backend>()
            }
            #[test]
            fn a_snapshot_reloads_to_the_same_document() {
                super::a_snapshot_reloads_to_the_same_document::<$backend>()
            }
            #[test]
            fn a_backend_keeps_a_key_it_does_not_understand() {
                super::a_backend_keeps_a_key_it_does_not_understand::<$backend>()
            }
            #[test]
            fn forgetting_a_note_leaves_nothing_of_it_anywhere() {
                super::forgetting_a_note_leaves_nothing_of_it_anywhere::<$backend>()
            }
            #[test]
            fn a_forgotten_note_cannot_be_opened() {
                super::a_forgotten_note_cannot_be_opened::<$backend>()
            }
            #[test]
            fn a_forgotten_note_is_still_gone_after_a_relaunch() {
                super::a_forgotten_note_is_still_gone_after_a_relaunch::<$backend>()
            }
            #[test]
            fn a_late_envelope_leaves_no_note_behind() {
                super::a_late_envelope_leaves_no_note_behind::<$backend>()
            }
            #[test]
            fn a_note_whose_key_never_arrived_is_not_swept() {
                super::a_note_whose_key_never_arrived_is_not_swept::<$backend>()
            }
            #[test]
            fn emptying_the_trash_forgets_what_is_in_it() {
                super::emptying_the_trash_forgets_what_is_in_it::<$backend>()
            }
            #[test]
            fn emptying_an_empty_trash_writes_nothing() {
                super::emptying_an_empty_trash_writes_nothing::<$backend>()
            }
            #[test]
            fn the_account_document_cannot_be_forgotten() {
                super::the_account_document_cannot_be_forgotten::<$backend>()
            }
            #[test]
            fn forgetting_a_note_reaches_the_list() {
                super::forgetting_a_note_reaches_the_list::<$backend>()
            }
            #[test]
            fn a_long_account_log_is_compacted_at_launch() {
                super::a_long_account_log_is_compacted_at_launch::<$backend>()
            }
            #[test]
            fn a_compacted_account_log_is_still_the_whole_document() {
                super::a_compacted_account_log_is_still_the_whole_document::<$backend>()
            }
            #[test]
            fn a_compacted_account_document_has_the_whole_document_in_its_outbox() {
                super::a_compacted_account_document_has_the_whole_document_in_its_outbox::<$backend>()
            }
            #[test]
            fn a_compacted_account_document_does_not_reuse_a_counter() {
                super::a_compacted_account_document_does_not_reuse_a_counter::<$backend>()
            }
            #[test]
            fn a_short_account_log_is_left_alone() {
                super::a_short_account_log_is_left_alone::<$backend>()
            }
            #[test]
            fn compacting_the_account_document_again_changes_nothing() {
                super::compacting_the_account_document_again_changes_nothing::<$backend>()
            }
            #[test]
            fn a_note_forgotten_after_a_compaction_stays_forgotten() {
                super::a_note_forgotten_after_a_compaction_stays_forgotten::<$backend>()
            }
            #[test]
            fn an_account_document_on_disk_is_compacted_once() {
                super::an_account_document_on_disk_is_compacted_once::<$backend>()
            }
        }
    };
}

#[cfg(feature = "automerge-backend")]
backend_suite!(automerge, easynotes_core::crdt::AutomergeNote);
#[cfg(feature = "yrs-backend")]
backend_suite!(yrs, easynotes_core::crdt::YrsNote);
