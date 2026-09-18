//! What it costs to open a device's whole note list.
//!
//! Prints the table in `docs/adr/0007-note-list.md`. Run it with:
//!
//! ```sh
//! cargo run --release -p easynotes-core --example bench_library
//! ```
//!
//! The question this answers is a launch-time one. A note's title lives inside
//! an envelope, so listing notes means opening them, and the app opens every
//! note it holds before it can draw its first frame. That is fine for a
//! household and not fine for ever, and the point of measuring is to know where
//! the line is rather than to guess it.
//!
//! Unlike `bench_compaction` this runs against SQLite in a real directory,
//! because a launch reads a disk and that read is part of the answer.

use easynotes_core::crdt::{DefaultNoteDoc, Edit, NoteDoc};
use easynotes_core::crypto::MasterKey;
use easynotes_core::ids::NoteId;
use easynotes_core::note::{NoteCore, NoteKind, SyncCursor};
use easynotes_core::store::{master_key_from_file, SqliteKeyring, SqliteStore};
use std::path::Path;
use std::time::{Duration, Instant};

/// How many notes a device holds. The first is a household after a year; the
/// last is somebody using this as a notes app in earnest.
const COUNTS: &[usize] = &[10, 50, 200];

/// Items per note, and edits made on them afterwards. A shopping list that has
/// been ticked off a few times.
const ITEMS: usize = 20;
const EDITS: usize = 32;

type Core = NoteCore<DefaultNoteDoc, SqliteStore, SqliteKeyring>;

fn open(dir: &Path) -> Core {
    let master: MasterKey = master_key_from_file(dir.join("master.key")).unwrap();
    NoteCore::open(
        SqliteStore::open(dir.join("notes.db"), master.clone()).unwrap(),
        SqliteKeyring::open(dir.join("keys.db"), master).unwrap(),
    )
    .unwrap()
}

/// A device with `notes` notes on it, each with a list and some edits on it.
fn fill(core: &mut Core, notes: usize) -> Vec<NoteId> {
    let mut made = Vec::with_capacity(notes);
    for n in 0..notes {
        let id = core.create_note(NoteKind::Checklist).unwrap();
        core.set_title(id, format!("List {n}")).unwrap();
        let mut items = Vec::with_capacity(ITEMS);
        for i in 0..ITEMS {
            items.push(core.add_item(id, format!("item number {i}")).unwrap().0);
        }
        for i in 0..EDITS {
            core.apply_local(
                id,
                Edit::SetChecked {
                    id: items[i % items.len()],
                    checked: i % 2 == 0,
                },
            )
            .unwrap();
        }
        made.push(id);
    }
    made
}

/// The state a device that has been syncing all week is in, which is the only
/// state in which compaction may drop anything.
fn pretend_the_relay_has_it_all(core: &mut Core, notes: &[NoteId]) {
    for id in notes {
        let last = core.unsent(*id, None).unwrap().last().unwrap().0;
        core.set_sync_cursor(
            *id,
            SyncCursor {
                last_seq: last as i64,
                sent_upto: Some(last),
            },
        )
        .unwrap();
    }
}

fn time(mut f: impl FnMut()) -> Duration {
    let start = Instant::now();
    f();
    start.elapsed()
}

struct Row {
    notes: usize,
    from_logs: Duration,
    from_snapshots: Duration,
}

fn measure(notes: usize) -> Row {
    let dir = tempfile::tempdir().unwrap();
    let mut core = open(dir.path());
    let ids = fill(&mut core, notes);
    drop(core);

    // A launch on a device whose notes still have their logs: 53 envelopes each,
    // every one of them signature-verified before it is decrypted.
    let mut core = open(dir.path());
    let from_logs = time(|| {
        core.notes().unwrap();
    });
    assert_eq!(core.notes().unwrap().len(), notes);

    // And the same device once its notes have been compacted, which is what a
    // second launch finds after the first one has done it.
    pretend_the_relay_has_it_all(&mut core, &ids);
    for id in &ids {
        core.compact(*id).unwrap();
    }
    drop(core);

    let mut core = open(dir.path());
    let from_snapshots = time(|| {
        core.notes().unwrap();
    });
    assert_eq!(core.notes().unwrap().len(), notes);

    Row {
        notes,
        from_logs,
        from_snapshots,
    }
}

fn main() {
    println!(
        "{} backend, {ITEMS} items and {EDITS} edits a note, SQLite on this disk\n",
        DefaultNoteDoc::backend_name()
    );
    println!(
        "{:>6}  {:>16}  {:>16}  {:>14}",
        "notes", "open from logs", "from snapshots", "per note"
    );
    for &count in COUNTS {
        let row = measure(count);
        println!(
            "{:>6}  {:>13.1} ms  {:>13.1} ms  {:>11.2} ms",
            row.notes,
            row.from_logs.as_secs_f64() * 1e3,
            row.from_snapshots.as_secs_f64() * 1e3,
            row.from_snapshots.as_secs_f64() * 1e3 / row.notes as f64,
        );
    }
}
