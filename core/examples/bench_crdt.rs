//! Decision D1: Automerge or Yrs?
//!
//! Measures the five criteria the initial implementation names, and prints a Markdown table
//! to paste into `docs/decisions.md#adr-0001`. Run it with:
//!
//! ```sh
//! cargo run --release -p easynotes-core --example bench_crdt
//! ```
//!
//! Numbers are measured, not argued. The WASM bundle contribution is the one
//! criterion this binary cannot measure — `just bench-wasm` does that by building
//! each backend to `wasm32-unknown-unknown` on its own.

use easynotes_core::crdt::{AutomergeNote, Edit, ItemView, NoteDoc, YrsNote};
use easynotes_core::ids::ItemId;
use easynotes_core::order;
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

// --- a tracking allocator, so "peak memory" is a measurement and not a guess ---

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

struct Tracking;

unsafe impl GlobalAlloc for Tracking {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            let live = LIVE.fetch_add(layout.size(), Ordering::Relaxed) + layout.size();
            PEAK.fetch_max(live, Ordering::Relaxed);
        }
        p
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOC: Tracking = Tracking;

fn reset_peak() {
    PEAK.store(LIVE.load(Ordering::Relaxed), Ordering::Relaxed);
}

fn peak_since_reset() -> usize {
    PEAK.load(Ordering::Relaxed)
        .saturating_sub(LIVE.load(Ordering::Relaxed))
}

// --- the workloads ---

const INSERTS: usize = 1_000;
const TOGGLES: usize = 1_000;
const REORDERS: usize = 100;

/// "30 days of history" on a 500-item list, spelled out: 500 items, then 50 edits
/// a day for 30 days. That is a heavily used household list, not a typical one.
const HISTORY_ITEMS: usize = 500;
const HISTORY_EDITS: usize = 30 * 50;

struct Measurements {
    backend: &'static str,
    insert: Duration,
    toggle: Duration,
    /// The move itself, with the order key already computed. Separated from
    /// `view` because a drag costs one of each and they are very different costs.
    reorder: Duration,
    /// One full read of a 1 000-item document. The UI pays this on every render,
    /// so it is arguably the most important number in the table.
    view: Duration,
    /// A toggle on a small, fresh document: the best case.
    toggle_update_fresh: usize,
    /// The same toggle after the full workload has aged the document. The gap
    /// between the two is what a long-lived list actually costs.
    toggle_update_aged: usize,
    add_update_bytes: usize,
    snapshot_bytes: usize,
    history_log_bytes: usize,
    history_snapshot_bytes: usize,
    peak_bytes: usize,
}

/// A deterministic pseudo-random sequence, so two runs of the benchmark compare
/// like for like and the two backends see exactly the same workload.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self, n: usize) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((self.0 >> 33) as usize) % n.max(1)
    }
}

fn append_key(prev: Option<&str>) -> String {
    match prev {
        None => order::first_key(),
        Some(p) => order::key_after(p),
    }
}

fn measure<D: NoteDoc>() -> Measurements {
    let mut doc = D::create(&[1u8; 16]);
    let mut ids: Vec<ItemId> = Vec::with_capacity(INSERTS);
    let mut last_key: Option<String> = None;

    // 1. inserts
    let t = Instant::now();
    for i in 0..INSERTS {
        let id = ItemId::random();
        let order = append_key(last_key.as_deref());
        doc.apply_local(&Edit::AddItem {
            id,
            text: format!("item {i}"),
            order: order.clone(),
        })
        .unwrap();
        last_key = Some(order);
        ids.push(id);
    }
    let insert = t.elapsed();

    // 2. toggles
    let t = Instant::now();
    for i in 0..TOGGLES {
        doc.apply_local(&Edit::SetChecked {
            id: ids[i % ids.len()],
            checked: i % 2 == 0,
        })
        .unwrap();
    }
    let toggle = t.elapsed();

    // 3. one full read of the document, which is what the UI does per render.
    let t = Instant::now();
    let view = doc.view();
    let view_time = t.elapsed();
    assert_eq!(view.items.len(), INSERTS);

    // 4. random reorders. The order key is computed up front so this times the
    //    CRDT write alone and not the view read, which row 3 already covers.
    let mut rng = Lcg(0x5EED);
    let moves: Vec<(ItemId, String)> = {
        let mut keys: Vec<String> = view
            .items
            .iter()
            .map(|i: &ItemView| i.order.clone())
            .collect();
        (0..REORDERS)
            .map(|_| {
                let from = rng.next(keys.len());
                let to = rng.next(keys.len());
                let id = view.items[from].id;
                let mut without = keys.clone();
                without.remove(from);
                let key = order::key_for_index(&without, to.min(without.len()));
                keys[from] = key.clone();
                (id, key)
            })
            .collect()
    };
    let t = Instant::now();
    for (id, order) in &moves {
        doc.apply_local(&Edit::MoveItem {
            id: *id,
            order: order.clone(),
        })
        .unwrap();
    }
    let reorder = t.elapsed();

    // 5. the size of one update. This is what we pay per keystroke, per device,
    //    on the relay, forever — so it is the number that compounds. Flip the
    //    item's *current* value: writing back the value already there can be a
    //    no-op that produces an empty update and flatters the number.
    let current = doc.view().item(ids[0]).map(|i| i.checked).unwrap_or(false);
    let toggle_update_aged = doc
        .apply_local(&Edit::SetChecked {
            id: ids[0],
            checked: !current,
        })
        .unwrap()
        .len();
    let add_update_bytes = doc
        .apply_local(&Edit::AddItem {
            id: ItemId::random(),
            text: "milk".into(),
            order: append_key(last_key.as_deref()),
        })
        .unwrap()
        .len();

    let snapshot_bytes = doc.snapshot().len();

    // The same toggle on a fresh ten-item list, for the best case.
    let toggle_update_fresh = {
        let mut fresh = D::create(&[9u8; 16]);
        let mut key: Option<String> = None;
        let mut first = None;
        for i in 0..10 {
            let id = ItemId::random();
            let order = append_key(key.as_deref());
            fresh
                .apply_local(&Edit::AddItem {
                    id,
                    text: format!("item {i}"),
                    order: order.clone(),
                })
                .unwrap();
            key = Some(order);
            first.get_or_insert(id);
        }
        fresh
            .apply_local(&Edit::SetChecked {
                id: first.unwrap(),
                checked: true,
            })
            .unwrap()
            .len()
    };

    // 5. peak memory for a 500-item list with 30 days of history, rebuilt from its
    //    log the way a cold start does.
    drop(doc);
    let mut fresh = D::create(&[2u8; 16]);
    let mut hist_ids = Vec::new();
    let mut key: Option<String> = None;
    let mut log = Vec::new();
    for i in 0..HISTORY_ITEMS {
        let id = ItemId::random();
        let order = append_key(key.as_deref());
        log.push(
            fresh
                .apply_local(&Edit::AddItem {
                    id,
                    text: format!("item {i}"),
                    order: order.clone(),
                })
                .unwrap(),
        );
        key = Some(order);
        hist_ids.push(id);
    }
    let mut rng = Lcg(0xC0FFEE);
    for i in 0..HISTORY_EDITS {
        let id = hist_ids[rng.next(hist_ids.len())];
        let edit = match i % 3 {
            0 => Edit::SetChecked {
                id,
                checked: i % 2 == 0,
            },
            1 => Edit::SetItemText {
                id,
                text: format!("edited {i}"),
            },
            _ => Edit::MoveItem {
                id,
                order: append_key(key.as_deref()),
            },
        };
        if let Edit::MoveItem { order, .. } = &edit {
            key = Some(order.clone());
        }
        log.push(fresh.apply_local(&edit).unwrap());
    }
    let history_log_bytes: usize = log.iter().map(|u| u.len()).sum();
    let history_snapshot = fresh.snapshot();
    let history_snapshot_bytes = history_snapshot.len();
    drop(fresh);
    drop(log);

    // Rebuild from the snapshot and hold it live while we read the peak.
    reset_peak();
    let rebuilt = D::load(&[3u8; 16], &history_snapshot).unwrap();
    let _ = rebuilt.view();
    let peak_bytes = peak_since_reset();
    drop(rebuilt);

    Measurements {
        backend: D::backend_name(),
        insert,
        toggle,
        reorder,
        view: view_time,
        toggle_update_fresh,
        toggle_update_aged,
        add_update_bytes,
        snapshot_bytes,
        history_log_bytes,
        history_snapshot_bytes,
        peak_bytes,
    }
}

fn ms(d: Duration) -> String {
    format!("{:.1} ms", d.as_secs_f64() * 1000.0)
}

fn kb(n: usize) -> String {
    format!("{:.1} KiB", n as f64 / 1024.0)
}

fn main() {
    if cfg!(debug_assertions) {
        eprintln!("warning: run this with --release, or the timings mean nothing\n");
    }

    let rows = [measure::<AutomergeNote>(), measure::<YrsNote>()];

    println!("Workload: {INSERTS} inserts, {TOGGLES} toggles, {REORDERS} random reorders.");
    println!("History: {HISTORY_ITEMS} items + {HISTORY_EDITS} edits (30 days at 50 edits/day).\n");

    let header = ["Criterion", rows[0].backend, rows[1].backend];
    println!("| {} | {} | {} |", header[0], header[1], header[2]);
    println!("| --- | --- | --- |");

    let line = |name: &str, f: &dyn Fn(&Measurements) -> String| {
        println!("| {} | {} | {} |", name, f(&rows[0]), f(&rows[1]));
    };

    line(&format!("{INSERTS} inserts"), &|m| ms(m.insert));
    line(&format!("{TOGGLES} toggles"), &|m| ms(m.toggle));
    line(&format!("{REORDERS} reorders (write only)"), &|m| {
        ms(m.reorder)
    });
    line(&format!("One full view() read of {INSERTS} items"), &|m| {
        ms(m.view)
    });
    line("One toggle update, fresh 10-item list", &|m| {
        format!("{} B", m.toggle_update_fresh)
    });
    line("One toggle update, after the workload", &|m| {
        format!("{} B", m.toggle_update_aged)
    });
    line("One add-item update, after the workload", &|m| {
        format!("{} B", m.add_update_bytes)
    });
    line("Snapshot after the workload", &|m| kb(m.snapshot_bytes));
    line("Log for 500 items + 30 days", &|m| kb(m.history_log_bytes));
    line("Snapshot for 500 items + 30 days", &|m| {
        kb(m.history_snapshot_bytes)
    });
    line("Peak heap rebuilding that history", &|m| kb(m.peak_bytes));
}
