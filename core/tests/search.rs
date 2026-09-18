//! Local search, and the sealed index it reads.
//!
//! Two claims are under test here and they are not the same one. *Search finds
//! the note* is the feature; *the list does not open every note to draw itself*
//! is what the index bought on the way, and it is the one that can regress
//! silently — a correct app that has quietly gone back to replaying every log at
//! launch looks exactly like a correct app. So the launch test counts the reads
//! rather than trusting the clock.

use easynotes_core::crdt::NoteDoc;
use easynotes_core::crypto::MasterKey;
use easynotes_core::ids::NoteId;
use easynotes_core::note::{NoteCore, NoteKind, INDEX};
use easynotes_core::store::{
    copy_store, master_key_from_file, MemoryKeyring, MemoryStore, OpStore, SqliteKeyring,
    SqliteStore,
};
use easynotes_core::Result;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

type Memory<D> = NoteCore<D, MemoryStore, MemoryKeyring>;

fn in_memory<D: NoteDoc>() -> Memory<D> {
    NoteCore::open(MemoryStore::new(), MemoryKeyring::new()).unwrap()
}

fn master(dir: &Path) -> MasterKey {
    master_key_from_file(dir.join("master.key")).unwrap()
}

/// A store that says how often something asked it for a note's log.
///
/// Opening a note is the expensive thing the index exists to avoid, and reading
/// the log is the first thing opening a note does. Counting the calls is
/// therefore a measurement of the property rather than of this machine.
struct Counting<S: OpStore> {
    inner: S,
    loads: Arc<AtomicUsize>,
}

impl<S: OpStore> OpStore for Counting<S> {
    fn append(&mut self, note: NoteId, envelope: &[u8]) -> Result<()> {
        self.inner.append(note, envelope)
    }
    fn load(&self, note: NoteId) -> Result<Vec<Vec<u8>>> {
        self.loads.fetch_add(1, Ordering::SeqCst);
        self.inner.load(note)
    }
    fn put_snapshot(&mut self, note: NoteId, snapshot: &[u8]) -> Result<()> {
        self.inner.put_snapshot(note, snapshot)
    }
    fn get_snapshot(&self, note: NoteId) -> Result<Option<Vec<u8>>> {
        self.inner.get_snapshot(note)
    }
    fn replace_log(&mut self, note: NoteId, envelopes: &[Vec<u8>]) -> Result<()> {
        self.inner.replace_log(note, envelopes)
    }
    fn log_len(&self, note: NoteId) -> Result<usize> {
        self.inner.log_len(note)
    }
    fn notes(&self) -> Result<Vec<NoteId>> {
        self.inner.notes()
    }
    fn put_meta(&mut self, note: NoteId, key: &str, value: &[u8]) -> Result<()> {
        self.inner.put_meta(note, key, value)
    }
    fn get_meta(&self, note: NoteId, key: &str) -> Result<Option<Vec<u8>>> {
        self.inner.get_meta(note, key)
    }
}

/// A core over the real store, opened the way a relaunch opens it, with every
/// read of a log counted.
fn on_disk<D: NoteDoc>(
    dir: &Path,
    loads: Arc<AtomicUsize>,
) -> NoteCore<D, Counting<SqliteStore>, SqliteKeyring> {
    let master = master(dir);
    NoteCore::open(
        Counting {
            inner: SqliteStore::open(dir.join("notes.db"), master.clone()).unwrap(),
            loads,
        },
        SqliteKeyring::open(dir.join("keys.db"), master).unwrap(),
    )
    .unwrap()
}

/// A shopping list with enough on it that the interesting item is off the card.
fn a_long_list<D: NoteDoc>(core: &mut Memory<D>, title: &str) -> NoteId {
    let id = core.create_note(NoteKind::Checklist).unwrap();
    core.set_title(id, title).unwrap();
    for i in 0..11 {
        core.add_item(id, format!("thing {i}")).unwrap();
    }
    core.add_item(id, "asparagus").unwrap();
    id
}

// ---------------------------------------------------------------------------

fn a_search_finds_a_note_by_its_title<D: NoteDoc>() {
    let mut core = in_memory::<D>();
    let groceries = a_long_list(&mut core, "Groceries");
    a_long_list(&mut core, "Hardware");

    let hits = core.search("groc").unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].summary.id, groceries);
    assert_eq!(hits[0].matched, "Groceries");
}

/// The reason the index holds the note's whole text and not the card's four
/// items: a match on the twelfth line has to be findable, and has to be shown.
fn a_search_finds_an_item_the_card_does_not_show<D: NoteDoc>() {
    let mut core = in_memory::<D>();
    let groceries = a_long_list(&mut core, "Groceries");

    let hits = core.search("aspara").unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].summary.id, groceries);
    assert_eq!(hits[0].matched, "asparagus");
    assert!(
        !hits[0].summary.preview.contains(&"asparagus".to_string()),
        "the test proved nothing: the match is on the card already"
    );
}

fn a_search_ignores_case_on_both_sides<D: NoteDoc>() {
    let mut core = in_memory::<D>();
    let id = core.create_note(NoteKind::Checklist).unwrap();
    core.set_title(id, "Sourdough STARTER").unwrap();

    for query in ["sourdough starter", "SOURDOUGH", "Starter"] {
        assert_eq!(core.search(query).unwrap().len(), 1, "query {query:?}");
    }
}

/// Two words mean both words, and they do not have to be on the same line — a
/// list is one thing, and "milk bread" is a search for the list with both on it.
fn every_term_has_to_appear<D: NoteDoc>() {
    let mut core = in_memory::<D>();
    let both = core.create_note(NoteKind::Checklist).unwrap();
    core.add_item(both, "milk").unwrap();
    core.add_item(both, "bread").unwrap();
    let one = core.create_note(NoteKind::Checklist).unwrap();
    core.add_item(one, "milk").unwrap();

    let hits = core.search("milk bread").unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].summary.id, both);
    assert_eq!(core.search("milk").unwrap().len(), 2);
}

fn an_empty_query_finds_nothing<D: NoteDoc>() {
    let mut core = in_memory::<D>();
    a_long_list(&mut core, "Groceries");
    for query in ["", "   "] {
        assert!(core.search(query).unwrap().is_empty(), "query {query:?}");
    }
}

/// A hit carries the placement, because the screen that shows it has shelves.
fn a_hit_knows_where_the_note_lives<D: NoteDoc>() {
    let mut core = in_memory::<D>();
    let id = a_long_list(&mut core, "Groceries");
    core.set_archived(id, true).unwrap();

    let hits = core.search("groceries").unwrap();
    assert_eq!(hits.len(), 1);
    assert!(hits[0].summary.placement.archived);
}

/// An edit reaches the index the moment it reaches the log, so a note becomes
/// findable by what was just typed into it rather than on the next launch.
fn an_edit_is_searchable_at_once<D: NoteDoc>() {
    let mut core = in_memory::<D>();
    let id = a_long_list(&mut core, "Groceries");
    assert!(core.search("cortado").unwrap().is_empty());

    core.add_item(id, "cortado").unwrap();
    assert_eq!(core.search("cortado").unwrap().len(), 1);

    // And stops being findable when it is taken off the list again.
    let item = core.view(id).unwrap().items.last().unwrap().id;
    core.remove_item(id, item).unwrap();
    assert!(core.search("cortado").unwrap().is_empty());
}

// ---------------------------------------------------------------------------
// The index, on a disk

/// The claim ADR 0007 asked for: a launch reads what the notes say without
/// replaying what they did.
fn a_relaunch_lists_and_searches_without_opening_a_note<D: NoteDoc>() {
    let dir = tempfile::tempdir().unwrap();
    let ids: Vec<NoteId> = {
        let loads = Arc::new(AtomicUsize::new(0));
        let mut core = on_disk::<D>(dir.path(), loads);
        let mut ids = Vec::new();
        for title in ["Groceries", "Hardware", "Books"] {
            let id = core.create_note(NoteKind::Checklist).unwrap();
            core.set_title(id, title).unwrap();
            core.add_item(id, "asparagus").unwrap();
            ids.push(id);
        }
        ids
    }; // the app is killed here

    let loads = Arc::new(AtomicUsize::new(0));
    let mut core = on_disk::<D>(dir.path(), loads.clone());
    // Opening the core replays the account document, which is one read of one
    // log and is not a note.
    let after_open = loads.load(Ordering::SeqCst);

    let notes = core.notes().unwrap();
    assert_eq!(notes.len(), 3);
    assert_eq!(notes[0].title, "Books");
    assert_eq!(notes[0].items, 1);

    let hits = core.search("aspara").unwrap();
    assert_eq!(hits.len(), 3);
    assert_eq!(hits[0].summary.id, ids[2]);

    assert_eq!(
        loads.load(Ordering::SeqCst),
        after_open,
        "a note's log was replayed to list or search it"
    );
}

/// The other half of that: opening one really does still open it. Without this,
/// the count above could be zero because nothing works.
fn opening_a_note_still_replays_it<D: NoteDoc>() {
    let dir = tempfile::tempdir().unwrap();
    let id = {
        let mut core = on_disk::<D>(dir.path(), Arc::new(AtomicUsize::new(0)));
        let id = core.create_note(NoteKind::Checklist).unwrap();
        core.set_title(id, "Groceries").unwrap();
        core.add_item(id, "asparagus").unwrap();
        id
    };

    let loads = Arc::new(AtomicUsize::new(0));
    let mut core = on_disk::<D>(dir.path(), loads.clone());
    core.notes().unwrap();
    let before = loads.load(Ordering::SeqCst);
    let view = core.open_note(id).unwrap();
    assert_eq!(view.title, "Groceries");
    assert_eq!(view.items.len(), 1);
    assert!(loads.load(Ordering::SeqCst) > before);
}

/// An entry is believed only while it still matches the log it was written from.
/// This is the crash between the append and the entry, staged by appending
/// behind the core's back — which is the same thing from the store's side.
fn an_entry_that_does_not_match_the_log_is_rebuilt<D: NoteDoc>() {
    let dir = tempfile::tempdir().unwrap();

    // A peer with a list, and its first two edits as they cross a relay.
    let mut peer = in_memory::<D>();
    let peer_note = peer.create_note(NoteKind::Checklist).unwrap();
    let blob = peer.export_share_blob(peer_note).unwrap();
    let titled = peer.set_title(peer_note, "Groceries").unwrap();
    let added = peer.add_item(peer_note, "asparagus").unwrap().1;

    // This device joins and takes the first of them the ordinary way, so it ends
    // up with one envelope in the log and an entry that says so.
    let id = {
        let mut core = on_disk::<D>(dir.path(), Arc::new(AtomicUsize::new(0)));
        let id = core.import_share_blob(&blob).unwrap();
        core.apply_remote(id, &titled).unwrap();
        assert!(core.search("aspara").unwrap().is_empty());
        id
    };

    // The second lands in the log and the app dies before it can write down what
    // the note now says — which is the only way an entry can be behind, and the
    // one thing the length check is for.
    {
        let mut store = SqliteStore::open(dir.path().join("notes.db"), master(dir.path())).unwrap();
        store.append(id, &added).unwrap();
    }

    let mut core = on_disk::<D>(dir.path(), Arc::new(AtomicUsize::new(0)));
    let notes = core.notes().unwrap();
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].title, "Groceries");
    assert_eq!(notes[0].items, 1, "the stale entry was believed");
    assert_eq!(core.search("aspara").unwrap().len(), 1);
}

/// A device that has notes and no entries for them — one that updated into this
/// version, or one `copy_store` moved — lists and searches them all the same, by
/// opening them once and writing the entries as it goes.
fn a_device_with_no_index_builds_one<D: NoteDoc>() {
    let mut old = in_memory::<D>();
    a_long_list(&mut old, "Groceries");
    a_long_list(&mut old, "Hardware");

    // Everything the old device had except the index, which is what a version
    // that never wrote one leaves behind.
    let mut store = MemoryStore::new();
    let mut keyring = MemoryKeyring::new();
    let moved = copy_store(
        old.store(),
        old.keyring(),
        &mut store,
        &mut keyring,
        &[easynotes_core::note::ADDED],
    )
    .unwrap();
    assert!(moved >= 2);
    assert!(
        store.get_meta(NoteId([0u8; 16]), INDEX).unwrap().is_none(),
        "the test proved nothing: an entry came across"
    );

    let mut core: Memory<D> = NoteCore::open(store, keyring).unwrap();
    assert_eq!(core.notes().unwrap().len(), 2);
    assert_eq!(core.search("aspara").unwrap().len(), 2);
}

/// The index is a copy of the note's text, so it is sealed like the note's text.
/// `tests/persistence.rs` greps every file for the same secrets; this one holds
/// the row itself, before any store has had a chance to seal it again.
fn an_index_entry_is_ciphertext<D: NoteDoc>() {
    let mut core = in_memory::<D>();
    let id = a_long_list(&mut core, "Groceries");

    let sealed = core.store().get_meta(id, INDEX).unwrap().unwrap();
    for secret in ["Groceries", "asparagus"] {
        assert!(
            !sealed.windows(secret.len()).any(|w| w == secret.as_bytes()),
            "{secret:?} is in the index in the clear"
        );
    }
}

macro_rules! backend_suite {
    ($name:ident, $backend:ty) => {
        mod $name {
            #[test]
            fn a_search_finds_a_note_by_its_title() {
                super::a_search_finds_a_note_by_its_title::<$backend>()
            }
            #[test]
            fn a_search_finds_an_item_the_card_does_not_show() {
                super::a_search_finds_an_item_the_card_does_not_show::<$backend>()
            }
            #[test]
            fn a_search_ignores_case_on_both_sides() {
                super::a_search_ignores_case_on_both_sides::<$backend>()
            }
            #[test]
            fn every_term_has_to_appear() {
                super::every_term_has_to_appear::<$backend>()
            }
            #[test]
            fn an_empty_query_finds_nothing() {
                super::an_empty_query_finds_nothing::<$backend>()
            }
            #[test]
            fn a_hit_knows_where_the_note_lives() {
                super::a_hit_knows_where_the_note_lives::<$backend>()
            }
            #[test]
            fn an_edit_is_searchable_at_once() {
                super::an_edit_is_searchable_at_once::<$backend>()
            }
            #[test]
            fn a_relaunch_lists_and_searches_without_opening_a_note() {
                super::a_relaunch_lists_and_searches_without_opening_a_note::<$backend>()
            }
            #[test]
            fn opening_a_note_still_replays_it() {
                super::opening_a_note_still_replays_it::<$backend>()
            }
            #[test]
            fn an_entry_that_does_not_match_the_log_is_rebuilt() {
                super::an_entry_that_does_not_match_the_log_is_rebuilt::<$backend>()
            }
            #[test]
            fn a_device_with_no_index_builds_one() {
                super::a_device_with_no_index_builds_one::<$backend>()
            }
            #[test]
            fn an_index_entry_is_ciphertext() {
                super::an_index_entry_is_ciphertext::<$backend>()
            }
        }
    };
}

#[cfg(feature = "automerge-backend")]
backend_suite!(automerge, easynotes_core::crdt::AutomergeNote);
#[cfg(feature = "yrs-backend")]
backend_suite!(yrs, easynotes_core::crdt::YrsNote);
