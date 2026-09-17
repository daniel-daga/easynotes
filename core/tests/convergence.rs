//! M1 acceptance: two cores that diverge must converge to *identical* state, and
//! the path there must survive duplicates, reordering and a network partition.
//!
//! Every test runs against both CRDT backends, so decision D1 cannot be made by
//! accident — whichever we pick, the behaviour asserted here is the behaviour we
//! are picking.

use easynotes_core::crdt::{Edit, NoteDoc, NoteView};
use easynotes_core::ids::{ItemId, NoteId};
use easynotes_core::note::{NoteCore, NoteKind};
use easynotes_core::store::{MemoryKeyring, MemoryStore};

type Core<D> = NoteCore<D, MemoryStore, MemoryKeyring>;

fn core<D: NoteDoc>() -> Core<D> {
    NoteCore::open(MemoryStore::new(), MemoryKeyring::new()).unwrap()
}

/// Two cores sharing one note, as the demo has them.
fn pair<D: NoteDoc>() -> (Core<D>, Core<D>, NoteId) {
    let mut a = core::<D>();
    let id = a.create_note(NoteKind::Checklist).unwrap();
    let blob = a.export_share_blob(id).unwrap();
    let mut b = core::<D>();
    assert_eq!(b.import_share_blob(&blob).unwrap(), id);
    (a, b, id)
}

/// Deliver envelopes to a core, ignoring the ones it already has.
fn deliver<D: NoteDoc>(to: &mut Core<D>, id: NoteId, envelopes: &[Vec<u8>]) {
    for e in envelopes {
        to.apply_remote(id, e).unwrap();
    }
}

fn texts(view: &NoteView) -> Vec<String> {
    view.items.iter().map(|i| i.text.clone()).collect()
}

// ---------------------------------------------------------------------------
// The generic bodies. Each is instantiated per backend at the bottom.
// ---------------------------------------------------------------------------

fn edits_flow_one_way<D: NoteDoc>() {
    let (mut a, mut b, id) = pair::<D>();
    let mut wire = vec![a.set_title(id, "Groceries").unwrap()];
    for item in ["milk", "bread", "eggs"] {
        wire.push(a.add_item(id, item).unwrap().1);
    }
    deliver(&mut b, id, &wire);

    let view = b.view(id).unwrap();
    assert_eq!(view.title, "Groceries");
    assert_eq!(texts(&view), ["milk", "bread", "eggs"]);
    assert_eq!(a.view(id).unwrap(), view);
}

fn a_check_flows_back<D: NoteDoc>() {
    // Demo step 3.
    let (mut a, mut b, id) = pair::<D>();
    let mut wire = Vec::new();
    let mut ids = Vec::new();
    for item in ["milk", "bread", "eggs"] {
        let (item_id, e) = a.add_item(id, item).unwrap();
        ids.push(item_id);
        wire.push(e);
    }
    deliver(&mut b, id, &wire);

    let back = b.set_checked(id, ids[1], true).unwrap();
    a.apply_remote(id, &back).unwrap();

    let view = a.view(id).unwrap();
    assert!(view.item(ids[1]).unwrap().checked);
    assert_eq!(view.checked_count(), 1);
    assert_eq!(a.view(id).unwrap(), b.view(id).unwrap());
}

fn converges_after_a_partition<D: NoteDoc>() {
    // Demo step 4, the one that matters: both sides go offline, both edit, both
    // come back. A adds an item; B renames one and reorders.
    let (mut a, mut b, id) = pair::<D>();
    let mut setup = Vec::new();
    let mut ids = Vec::new();
    for item in ["milk", "bread", "eggs"] {
        let (item_id, e) = a.add_item(id, item).unwrap();
        ids.push(item_id);
        setup.push(e);
    }
    deliver(&mut b, id, &setup);
    assert_eq!(a.view(id).unwrap(), b.view(id).unwrap());

    // --- partition ---
    let from_a = vec![a.add_item(id, "coffee").unwrap().1];
    let from_b = vec![
        b.set_item_text(id, ids[2], "eggs (12)").unwrap(),
        b.move_item(id, ids[2], 0).unwrap(),
    ];
    // --- heal, each side receiving the other's backlog ---
    deliver(&mut a, id, &from_b);
    deliver(&mut b, id, &from_a);

    let va = a.view(id).unwrap();
    let vb = b.view(id).unwrap();
    assert_eq!(va, vb, "the two devices did not converge");

    let t = texts(&va);
    assert_eq!(t.len(), 4, "expected four items, got {t:?}");
    assert_eq!(
        t.iter().filter(|s| s.starts_with("eggs")).count(),
        1,
        "eggs was duplicated: {t:?}"
    );
    assert!(
        t.contains(&"coffee".to_string()),
        "A's offline add was lost: {t:?}"
    );
    assert_eq!(
        t[0], "eggs (12)",
        "B's reorder and rename did not both survive: {t:?}"
    );
}

fn converges_whatever_order_updates_arrive_in<D: NoteDoc>() {
    // The relay guarantees per-note ordering, but a client that reconnects mid-fanout
    // can still see updates out of order. Convergence must not depend on it.
    let (mut a, mut b, id) = pair::<D>();
    let mut wire = Vec::new();
    let mut ids = Vec::new();
    for item in ["a", "b", "c", "d"] {
        let (item_id, e) = a.add_item(id, item).unwrap();
        ids.push(item_id);
        wire.push(e);
    }
    wire.push(a.set_checked(id, ids[1], true).unwrap());
    wire.push(a.move_item(id, ids[3], 0).unwrap());
    wire.push(a.remove_item(id, ids[0]).unwrap());

    // Reversed, then with every update delivered twice.
    let mut shuffled: Vec<Vec<u8>> = wire.iter().rev().cloned().collect();
    shuffled.extend(wire.iter().cloned());
    deliver(&mut b, id, &shuffled);

    assert_eq!(a.view(id).unwrap(), b.view(id).unwrap());
}

fn a_replayed_update_changes_nothing<D: NoteDoc>() {
    let (mut a, mut b, id) = pair::<D>();
    let (_, e) = a.add_item(id, "milk").unwrap();
    let first = b.apply_remote(id, &e).unwrap();
    let second = b.apply_remote(id, &e).unwrap();
    let third = b.apply_remote(id, &e).unwrap();
    assert_eq!(first, second);
    assert_eq!(second, third);
    assert_eq!(first.items.len(), 1);
    // And the replays did not grow the log.
    assert_eq!(b.store().len(id), 1);
}

fn concurrent_moves_of_the_same_item_do_not_duplicate_it<D: NoteDoc>() {
    let (mut a, mut b, id) = pair::<D>();
    let mut setup = Vec::new();
    let mut ids = Vec::new();
    for item in ["one", "two", "three", "four"] {
        let (item_id, e) = a.add_item(id, item).unwrap();
        ids.push(item_id);
        setup.push(e);
    }
    deliver(&mut b, id, &setup);

    // Both devices drag "four" somewhere, at the same time, to different places.
    let ea = a.move_item(id, ids[3], 0).unwrap();
    let eb = b.move_item(id, ids[3], 2).unwrap();
    a.apply_remote(id, &eb).unwrap();
    b.apply_remote(id, &ea).unwrap();

    let va = a.view(id).unwrap();
    assert_eq!(va, b.view(id).unwrap(), "concurrent moves diverged");
    assert_eq!(
        va.items.len(),
        4,
        "an item was duplicated or lost: {:?}",
        texts(&va)
    );
    assert_eq!(
        texts(&va).iter().filter(|s| *s == "four").count(),
        1,
        "the moved item appears more than once"
    );
}

fn a_concurrent_rename_and_check_both_survive<D: NoteDoc>() {
    let (mut a, mut b, id) = pair::<D>();
    let (item, e) = a.add_item(id, "eggs").unwrap();
    b.apply_remote(id, &e).unwrap();

    let ea = a.set_item_text(id, item, "eggs (12)").unwrap();
    let eb = b.set_checked(id, item, true).unwrap();
    a.apply_remote(id, &eb).unwrap();
    b.apply_remote(id, &ea).unwrap();

    let va = a.view(id).unwrap();
    assert_eq!(va, b.view(id).unwrap());
    let it = va.item(item).unwrap();
    assert_eq!(it.text, "eggs (12)", "the rename was lost");
    assert!(it.checked, "the check was lost");
}

fn a_concurrent_delete_and_edit_do_not_resurrect_a_ghost<D: NoteDoc>() {
    let (mut a, mut b, id) = pair::<D>();
    let (item, e) = a.add_item(id, "bread").unwrap();
    b.apply_remote(id, &e).unwrap();

    let del = a.remove_item(id, item).unwrap();
    let edit = b.set_item_text(id, item, "sourdough").unwrap();
    a.apply_remote(id, &edit).unwrap();
    b.apply_remote(id, &del).unwrap();

    let va = a.view(id).unwrap();
    assert_eq!(va, b.view(id).unwrap(), "delete-vs-edit diverged");
    // Whichever way the CRDT resolves it, both devices must show the same thing,
    // and an item that does survive must not be a half-built husk.
    if let Some(it) = va.item(item) {
        assert!(
            !it.text.is_empty() && !it.order.is_empty(),
            "resurrected a partial item: {it:?}"
        );
    }
}

fn many_concurrent_edits_converge<D: NoteDoc>() {
    // A wider sweep than the hand-written cases: both sides interleave adds,
    // toggles, renames and moves while partitioned.
    let (mut a, mut b, id) = pair::<D>();
    let mut setup = Vec::new();
    let mut ids: Vec<ItemId> = Vec::new();
    for i in 0..10 {
        let (item, e) = a.add_item(id, format!("item {i}")).unwrap();
        ids.push(item);
        setup.push(e);
    }
    deliver(&mut b, id, &setup);

    let mut from_a = Vec::new();
    let mut from_b = Vec::new();
    for i in 0..10usize {
        from_a.push(a.set_checked(id, ids[i], i % 2 == 0).unwrap());
        from_a.push(a.move_item(id, ids[i], (i * 3) % 10).unwrap());
        from_b.push(b.set_item_text(id, ids[i], format!("renamed {i}")).unwrap());
        from_b.push(b.move_item(id, ids[9 - i], (i * 7) % 10).unwrap());
        if i % 3 == 0 {
            from_a.push(a.add_item(id, format!("extra {i}")).unwrap().1);
        }
    }
    deliver(&mut a, id, &from_b);
    deliver(&mut b, id, &from_a);

    let va = a.view(id).unwrap();
    let vb = b.view(id).unwrap();
    assert_eq!(va, vb, "wide concurrent edit did not converge");
    // Order keys must still be a strictly increasing sequence.
    let keys = va.order_keys();
    let mut sorted = keys.clone();
    sorted.sort();
    assert_eq!(keys, sorted);
}

fn three_devices_converge<D: NoteDoc>() {
    // Nothing in the design limits a note to two members.
    let (mut a, mut b, id) = pair::<D>();
    let blob = a.export_share_blob(id).unwrap();
    let mut c = core::<D>();
    c.import_share_blob(&blob).unwrap();

    let e1 = a.add_item(id, "milk").unwrap().1;
    let e2 = b.add_item(id, "bread").unwrap().1;
    let e3 = c.add_item(id, "eggs").unwrap().1;
    let all = [e1, e2, e3];

    for (core, skip) in [(&mut a, 0usize), (&mut b, 1), (&mut c, 2)] {
        for (i, e) in all.iter().enumerate() {
            if i != skip {
                core.apply_remote(id, e).unwrap();
            }
        }
    }
    assert_eq!(a.view(id).unwrap(), b.view(id).unwrap());
    assert_eq!(b.view(id).unwrap(), c.view(id).unwrap());
    assert_eq!(a.view(id).unwrap().items.len(), 3);
}

fn snapshots_round_trip<D: NoteDoc>() {
    // A snapshot must carry the whole document, so a client can skip the log.
    let mut src = D::create(&[3u8; 16]);
    src.apply_local(&Edit::SetTitle("Groceries".into()))
        .unwrap();
    let mut order = String::new();
    for item in ["milk", "bread", "eggs"] {
        order = if order.is_empty() {
            easynotes_core::order::first_key()
        } else {
            easynotes_core::order::key_after(&order)
        };
        src.apply_local(&Edit::AddItem {
            id: ItemId::random(),
            text: item.into(),
            order: order.clone(),
        })
        .unwrap();
    }

    let restored = D::load(&[1u8; 16], &src.snapshot()).unwrap();
    assert_eq!(restored.view(), src.view());
    assert_eq!(texts(&restored.view()), ["milk", "bread", "eggs"]);

    // And a document restored from a snapshot must still accept new updates.
    let mut restored = restored;
    let update = src.apply_local(&Edit::SetTitle("Shopping".into())).unwrap();
    restored.apply_remote(&update).unwrap();
    assert_eq!(restored.view().title, "Shopping");
}

// ---------------------------------------------------------------------------
// Instantiation
// ---------------------------------------------------------------------------

macro_rules! backend_suite {
    ($mod_name:ident, $backend:ty) => {
        mod $mod_name {
            #[allow(unused_imports)]
            use super::*;

            #[test]
            fn edits_flow_one_way() {
                super::edits_flow_one_way::<$backend>()
            }
            #[test]
            fn a_check_flows_back() {
                super::a_check_flows_back::<$backend>()
            }
            #[test]
            fn converges_after_a_partition() {
                super::converges_after_a_partition::<$backend>()
            }
            #[test]
            fn converges_whatever_order_updates_arrive_in() {
                super::converges_whatever_order_updates_arrive_in::<$backend>()
            }
            #[test]
            fn a_replayed_update_changes_nothing() {
                super::a_replayed_update_changes_nothing::<$backend>()
            }
            #[test]
            fn concurrent_moves_of_the_same_item_do_not_duplicate_it() {
                super::concurrent_moves_of_the_same_item_do_not_duplicate_it::<$backend>()
            }
            #[test]
            fn a_concurrent_rename_and_check_both_survive() {
                super::a_concurrent_rename_and_check_both_survive::<$backend>()
            }
            #[test]
            fn a_concurrent_delete_and_edit_do_not_resurrect_a_ghost() {
                super::a_concurrent_delete_and_edit_do_not_resurrect_a_ghost::<$backend>()
            }
            #[test]
            fn many_concurrent_edits_converge() {
                super::many_concurrent_edits_converge::<$backend>()
            }
            #[test]
            fn three_devices_converge() {
                super::three_devices_converge::<$backend>()
            }
            #[test]
            fn snapshots_round_trip() {
                super::snapshots_round_trip::<$backend>()
            }
        }
    };
}

#[cfg(feature = "automerge-backend")]
backend_suite!(automerge, easynotes_core::crdt::AutomergeNote);
#[cfg(feature = "yrs-backend")]
backend_suite!(yrs, easynotes_core::crdt::YrsNote);

/// flutter_rust_bridge dispatches Rust calls onto a worker pool, so a document
/// that cannot cross a thread boundary cannot be driven from Flutter at all.
///
/// This is not hypothetical for Yrs: its `Subscription` handle wraps a
/// `Box<dyn Drop>` that is neither `Send` nor `Sync`, so the natural way to
/// observe updates pins the document to one thread. `YrsNote` uses a *keyed*
/// subscription to avoid holding one. If someone changes that back, this fails.
#[test]
fn both_backends_are_send() {
    fn assert_send<T: Send>() {}
    #[cfg(feature = "automerge-backend")]
    assert_send::<easynotes_core::crdt::AutomergeNote>();
    #[cfg(feature = "yrs-backend")]
    assert_send::<easynotes_core::crdt::YrsNote>();
    assert_send::<NoteCore<easynotes_core::crdt::AutomergeNote, MemoryStore, MemoryKeyring>>();
}
