//! What compaction buys, in bytes and in milliseconds.
//!
//! Prints the table in `docs/adr/0006-compaction.md`. Run it with:
//!
//! ```sh
//! cargo run --release -p easynotes-core --example bench_compaction
//! ```
//!
//! Unlike `bench_crdt`, this measures through `NoteCore` rather than the CRDT
//! trait, because the question is about what is on a disk: a log entry is a
//! sealed envelope with a 161-byte header and tag, and leaving that out would
//! flatter the log by the size of a small note.
//!
//! The store is in memory, so the times are the CPU cost of decrypting and
//! replaying — a real device adds its own reads and, on a compaction, one fsync.
//! That is the right way round for a decision about how much replaying to do.

use easynotes_core::crdt::{AutomergeNote, Edit, NoteDoc, YrsNote};
use easynotes_core::ids::NoteId;
use easynotes_core::note::{NoteCore, NoteKind, SyncCursor};
use easynotes_core::store::{MemoryKeyring, MemoryStore, OpStore};
use std::time::{Duration, Instant};

/// Items, then edits on them. The first is a household list after a few weeks;
/// the last is the size `bench_crdt` calls 30 days of history — 500 items and 50
/// edits a day — though the edits here are all toggles, so the snapshot sizes are
/// close to that benchmark's rather than the same.
const CASES: &[(usize, usize)] = &[(20, 256), (100, 1_000), (500, 30 * 50)];

struct Row {
    backend: &'static str,
    items: usize,
    edits: usize,
    entries: usize,
    log_bytes: usize,
    snapshot_bytes: usize,
    /// A long log on a note that has never reached a relay, so there is nothing
    /// compaction may drop: the replay, and a scan that decides so.
    open_without_compacting: Duration,
    /// The same log once the relay has acknowledged it all: the replay, plus the
    /// compaction that replay triggers.
    first_open: Duration,
    /// Every open after that one.
    later_open: Duration,
}

type Core<D> = NoteCore<D, MemoryStore, MemoryKeyring>;

fn build<D: NoteDoc>(items: usize, edits: usize) -> (Core<D>, NoteId) {
    let mut core: Core<D> = NoteCore::open(MemoryStore::new(), MemoryKeyring::new()).unwrap();
    let id = core.create_note(NoteKind::Checklist).unwrap();
    core.set_title(id, "Groceries").unwrap();

    let mut ids = Vec::with_capacity(items);
    for i in 0..items {
        ids.push(core.add_item(id, format!("item number {i}")).unwrap().0);
    }
    for i in 0..edits {
        // `set_checked` would do, and reads the whole note first to check the
        // item exists — nothing next to a user's keystroke, and a great deal
        // next to 1 500 of them. The edit that reaches the log is identical.
        core.apply_local(
            id,
            Edit::SetChecked {
                id: ids[i % ids.len()],
                checked: i % 2 == 0,
            },
        )
        .unwrap();
    }
    (core, id)
}

/// Mark everything as acknowledged, which is the state a note that has been
/// syncing all week is in — and the only state in which compaction may drop
/// anything at all.
fn pretend_the_relay_has_it_all<D: NoteDoc>(core: &mut Core<D>, id: NoteId) {
    let last = core.unsent(id, None).unwrap().last().unwrap().0;
    core.set_sync_cursor(
        id,
        SyncCursor {
            last_seq: last as i64,
            sent_upto: Some(last),
        },
    )
    .unwrap();
}

fn time(mut f: impl FnMut()) -> Duration {
    let start = Instant::now();
    f();
    start.elapsed()
}

/// One fixture, three opens, in the order that keeps them honest: the
/// never-synced open comes first because it is the only one that leaves the log
/// as it found it.
fn measure<D: NoteDoc>(backend: &'static str, items: usize, edits: usize) -> Row {
    let (mut core, id) = build::<D>(items, edits);
    let log = core.store().load(id).unwrap();
    let entries = log.len();
    let log_bytes = log.iter().map(|e| e.len()).sum();

    core.close_all();
    let open_without_compacting = time(|| {
        core.open_note(id).unwrap();
    });
    assert_eq!(
        core.store().log_len(id).unwrap(),
        entries,
        "a never-synced note compacted, which would have lost its only copy"
    );

    pretend_the_relay_has_it_all(&mut core, id);
    core.close_all();
    let first_open = time(|| {
        core.open_note(id).unwrap();
    });
    let snapshot_bytes = core.store().get_snapshot(id).unwrap().unwrap().len();
    assert_eq!(
        core.store().log_len(id).unwrap(),
        0,
        "an acknowledged log survived an open, so the benchmark is measuring nothing"
    );

    core.close_all();
    let later_open = time(|| {
        core.open_note(id).unwrap();
    });

    Row {
        backend,
        items,
        edits,
        entries,
        log_bytes,
        snapshot_bytes,
        open_without_compacting,
        first_open,
        later_open,
    }
}

fn kb(n: usize) -> String {
    if n >= 1024 * 1024 {
        format!("{:.2} MiB", n as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.1} KiB", n as f64 / 1024.0)
    }
}

fn ms(d: Duration) -> String {
    format!("{:.1} ms", d.as_secs_f64() * 1000.0)
}

fn main() {
    if cfg!(debug_assertions) {
        eprintln!("warning: run this with --release, or the timings mean nothing\n");
    }

    let rows: Vec<Row> = CASES
        .iter()
        .flat_map(|&(items, edits)| {
            [
                measure::<YrsNote>("Yrs", items, edits),
                measure::<AutomergeNote>("Automerge", items, edits),
            ]
        })
        .collect();

    println!("Sizes as stored: a log entry is a sealed envelope, header and tag included.\n");
    println!("| Note | Edits | Log | Snapshot | Ratio |");
    println!("| --- | --- | --- | --- | --- |");
    for r in &rows {
        println!(
            "| {} items, {} | {} | {} ({} entries) | **{}** | {:.2} |",
            r.items,
            r.backend,
            r.edits,
            kb(r.log_bytes),
            r.entries,
            kb(r.snapshot_bytes),
            r.snapshot_bytes as f64 / r.log_bytes as f64,
        );
    }

    println!("\nOpening the note, which is what a person actually waits for.\n");
    println!("| Note | Edits | Replay, no compaction | First open (replay + compaction) | Every open after |");
    println!("| --- | --- | --- | --- | --- |");
    for r in &rows {
        println!(
            "| {} items, {} | {} | {} | {} | **{}** |",
            r.items,
            r.backend,
            r.edits,
            ms(r.open_without_compacting),
            ms(r.first_open),
            ms(r.later_open),
        );
    }
}
