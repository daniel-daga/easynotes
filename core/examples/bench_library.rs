//! What it costs to open a device's whole note list.
//!
//! Prints the table in `docs/adr/0007-note-list.md`. Run it with:
//!
//! ```sh
//! cargo run --release -p easynotes-core --example bench_library
//! ```
//!
//! The question this answers is a launch-time one. A note's title lives inside
//! an envelope, so a device that kept nothing but the log would have to open
//! every note it holds before it could draw its first frame. That is fine for a
//! household and not fine for ever, which is what the sealed index
//! ([`easynotes_core::index`]) is for, and the point of measuring is to know
//! what it is worth rather than to assume it.
//!
//! So each row is measured three ways over the same notes: with no index, which
//! is what every launch cost before it existed and what the first launch after
//! an update still costs; the same device once its logs are compacted; and with
//! the index, which is every launch after the first.
//!
//! Unlike `bench_compaction` this runs against SQLite in a real directory,
//! because a launch reads a disk and that read is part of the answer.

use easynotes_core::crdt::{Colour, DefaultNoteDoc, Edit, NoteDoc};
use easynotes_core::crypto::MasterKey;
use easynotes_core::ids::NoteId;
use easynotes_core::note::{NoteCore, NoteKind, SyncCursor, ADDED, SYNC_CURSOR};
use easynotes_core::store::{copy_store, master_key_from_file, SqliteKeyring, SqliteStore};
use std::path::Path;
use std::time::{Duration, Instant};
use tempfile::TempDir;

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
    // What the account document holds by the time a device looks like this: a
    // placement for every note, because creating one writes where it goes, plus
    // the colours and pins somebody has actually chosen. A third and a seventh
    // is a guess at how many people bother, and it is the shape that matters —
    // the account document grows with the notes, not with the edits on them.
    for (n, id) in made.iter().enumerate() {
        if n % 3 == 0 {
            core.set_colour(*id, Some(Colour::ALL[n % Colour::ALL.len()]))
                .unwrap();
        }
        if n % 7 == 0 {
            core.set_pinned(*id, true).unwrap();
        }
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

/// The same device with no index entries: what it looked like before this
/// existed, and what a device that updates into this version looks like on its
/// first launch.
///
/// Built by copying it through the trait and leaving the index rows behind,
/// which is exactly what `open_local` does for a device coming off Slice 1's
/// directory of files.
fn without_the_index(from: &Path) -> TempDir {
    let to = tempfile::tempdir().unwrap();
    let source = master_key_from_file(from.join("master.key")).unwrap();
    let target = master_key_from_file(to.path().join("master.key")).unwrap();
    copy_store(
        &SqliteStore::open(from.join("notes.db"), source.clone()).unwrap(),
        &SqliteKeyring::open(from.join("keys.db"), source).unwrap(),
        &mut SqliteStore::open(to.path().join("notes.db"), target.clone()).unwrap(),
        &mut SqliteKeyring::open(to.path().join("keys.db"), target).unwrap(),
        &[SYNC_CURSOR, ADDED],
    )
    .unwrap();
    to
}

fn time(mut f: impl FnMut()) -> Duration {
    let start = Instant::now();
    f();
    start.elapsed()
}

struct Row {
    notes: usize,
    account: Duration,
    from_logs: Duration,
    from_snapshots: Duration,
    from_index: Duration,
}

fn measure(notes: usize) -> Row {
    let dir = tempfile::tempdir().unwrap();
    let mut core = open(dir.path());
    let ids = fill(&mut core, notes);
    drop(core);

    // A launch with no index, on a device whose notes still have their logs: 53
    // envelopes each, every one of them signature-verified before it is
    // decrypted. This is the column the index was built against.
    let bare = without_the_index(dir.path());
    let mut core = open(bare.path());
    let from_logs = time(|| {
        core.notes().unwrap();
    });
    assert_eq!(core.notes().unwrap().len(), notes);
    drop(core);
    drop(bare);

    // The same, once the notes have been compacted: one snapshot to verify
    // instead of 53 envelopes, and still every note opened.
    let mut core = open(dir.path());
    core.notes().unwrap();
    pretend_the_relay_has_it_all(&mut core, &ids);
    for id in &ids {
        core.compact(*id).unwrap();
    }
    drop(core);

    let bare = without_the_index(dir.path());
    let mut core = open(bare.path());
    let from_snapshots = time(|| {
        core.notes().unwrap();
    });
    assert_eq!(core.notes().unwrap().len(), notes);
    drop(core);
    drop(bare);

    // Opening the device is opening the account document: its log is replayed
    // before the first note is, because a note cannot be placed in a grid whose
    // order is not known yet. Nothing compacts it, so this is the whole of it.
    let account = time(|| {
        drop(open(dir.path()));
    });

    // And the launch the app actually does: one sealed row a note, no document
    // replayed and no signature verified.
    let mut core = open(dir.path());
    let from_index = time(|| {
        core.notes().unwrap();
    });
    assert_eq!(core.notes().unwrap().len(), notes);

    Row {
        notes,
        account,
        from_logs,
        from_snapshots,
        from_index,
    }
}

fn main() {
    println!(
        "{} backend, {ITEMS} items and {EDITS} edits a note, SQLite on this disk\n",
        DefaultNoteDoc::backend_name()
    );
    println!(
        "{:>6}  {:>16}  {:>16}  {:>16}  {:>14}  {:>16}",
        "notes",
        "no index, logs",
        "no index, snaps",
        "from the index",
        "per note",
        "account document"
    );
    for &count in COUNTS {
        let row = measure(count);
        println!(
            "{:>6}  {:>13.1} ms  {:>13.1} ms  {:>13.1} ms  {:>11.3} ms  {:>13.1} ms",
            row.notes,
            row.from_logs.as_secs_f64() * 1e3,
            row.from_snapshots.as_secs_f64() * 1e3,
            row.from_index.as_secs_f64() * 1e3,
            row.from_index.as_secs_f64() * 1e3 / row.notes as f64,
            row.account.as_secs_f64() * 1e3,
        );
    }
}
