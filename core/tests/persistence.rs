//! Demo step 6: kill the app, relaunch it, and the list is still there — with no
//! network and nothing but ciphertext on disk.

use easynotes_core::crdt::NoteDoc;
use easynotes_core::ids::NoteId;
use easynotes_core::note::{NoteCore, NoteKind};
use easynotes_core::store::{FileKeyring, FileStore, OpStore};
use std::path::Path;

type Core<D> = NoteCore<D, FileStore, FileKeyring>;

fn open<D: NoteDoc>(dir: &Path) -> Core<D> {
    NoteCore::open(
        FileStore::open(dir.join("log")).unwrap(),
        FileKeyring::open(dir.join("keys")).unwrap(),
    )
    .unwrap()
}

fn restart_restores_the_list<D: NoteDoc>() {
    let dir = tempfile::tempdir().unwrap();

    let id = {
        let mut core = open::<D>(dir.path());
        let id = core.create_note(NoteKind::Checklist).unwrap();
        core.set_title(id, "Groceries").unwrap();
        for item in ["milk", "bread", "eggs"] {
            core.add_item(id, item).unwrap();
        }
        let (coffee, _) = core.add_item(id, "coffee").unwrap();
        core.set_checked(id, coffee, true).unwrap();
        core.move_item(id, coffee, 0).unwrap();
        id
    }; // the app is killed here

    let mut core = open::<D>(dir.path());
    let view = core.open_note(id).unwrap();
    assert_eq!(view.title, "Groceries");
    assert_eq!(
        view.items
            .iter()
            .map(|i| i.text.as_str())
            .collect::<Vec<_>>(),
        ["coffee", "milk", "bread", "eggs"]
    );
    assert_eq!(view.checked_count(), 1);
    assert!(view.items[0].checked);
}

fn the_device_identity_survives_a_restart<D: NoteDoc>() {
    let dir = tempfile::tempdir().unwrap();
    let first = open::<D>(dir.path()).device_key();
    let second = open::<D>(dir.path()).device_key();
    assert_eq!(first, second, "a restart must not mint a new identity");
}

fn editing_after_a_restart_does_not_reuse_a_counter<D: NoteDoc>() {
    // If a relaunched app restarted its counter at zero, its new updates would be
    // mistaken for replays by every peer that already saw counter 0. The counter is
    // recovered from the log for exactly this reason.
    let dir = tempfile::tempdir().unwrap();

    let id = {
        let mut core = open::<D>(dir.path());
        let id = core.create_note(NoteKind::Checklist).unwrap();
        core.add_item(id, "milk").unwrap();
        core.add_item(id, "bread").unwrap();
        id
    };

    let mut core = open::<D>(dir.path());
    core.open_note(id).unwrap();
    let envelope = core.add_item(id, "eggs").unwrap().1;
    let env = easynotes_core::crypto::Envelope::decode(&envelope).unwrap();
    assert_eq!(env.counter, 2, "counter restarted instead of resuming");

    // And the peer's view really does pick the new item up.
    assert_eq!(core.view(id).unwrap().items.len(), 3);
}

fn a_snapshot_alone_restores_the_note<D: NoteDoc>() {
    let dir = tempfile::tempdir().unwrap();
    let id = {
        let mut core = open::<D>(dir.path());
        let id = core.create_note(NoteKind::Checklist).unwrap();
        core.set_title(id, "Groceries").unwrap();
        for item in ["milk", "bread"] {
            core.add_item(id, item).unwrap();
        }
        core.save_snapshot(id).unwrap();
        id
    };

    // Reopening reads the snapshot and then replays the log on top of it. Both
    // paths run, and the result must not double-count anything.
    let mut core = open::<D>(dir.path());
    let view = core.open_note(id).unwrap();
    assert_eq!(view.title, "Groceries");
    assert_eq!(view.items.len(), 2, "snapshot plus log double-applied");
}

fn nothing_on_disk_is_plaintext<D: NoteDoc>() {
    // The same check the relay gets in `tests/no_plaintext.rs`, applied to local
    // storage: Slice 1 holds the decrypted document in memory only.
    let dir = tempfile::tempdir().unwrap();
    let secrets = ["Groceries", "asparagus", "cortado", "sourdough"];

    let mut core = open::<D>(dir.path());
    let id = core.create_note(NoteKind::Checklist).unwrap();
    core.set_title(id, secrets[0]).unwrap();
    for item in &secrets[1..] {
        core.add_item(id, *item).unwrap();
    }
    core.save_snapshot(id).unwrap();

    for entry in walk(&dir.path().join("log")) {
        let raw = std::fs::read(&entry).unwrap();
        for secret in secrets {
            assert!(
                !contains(&raw, secret.as_bytes()),
                "{secret:?} appears in plaintext in {}",
                entry.display()
            );
        }
    }
}

fn a_torn_log_tail_does_not_lose_the_whole_note<D: NoteDoc>() {
    // Slice 1 does not fsync per append, so a crash can leave a half-written
    // record. Losing the last edit is acceptable; losing the note is not.
    let dir = tempfile::tempdir().unwrap();
    let id = {
        let mut core = open::<D>(dir.path());
        let id = core.create_note(NoteKind::Checklist).unwrap();
        for item in ["milk", "bread", "eggs"] {
            core.add_item(id, item).unwrap();
        }
        id
    };

    let log = dir.path().join("log").join(format!("{}.log", id.encode()));
    let mut raw = std::fs::read(&log).unwrap();
    raw.truncate(raw.len() - 20); // chop the tail mid-record
    std::fs::write(&log, &raw).unwrap();

    let mut core = open::<D>(dir.path());
    let view = core.open_note(id).unwrap();
    assert_eq!(
        view.items.len(),
        2,
        "a torn tail should cost one edit, not the note"
    );
}

fn the_store_lists_its_notes<D: NoteDoc>() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = open::<D>(dir.path());
    let mut made: Vec<NoteId> = Vec::new();
    for _ in 0..3 {
        let id = core.create_note(NoteKind::Checklist).unwrap();
        core.add_item(id, "x").unwrap();
        made.push(id);
    }
    made.sort();
    assert_eq!(core.store().notes().unwrap(), made);
}

fn walk(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                out.extend(walk(&p));
            } else {
                out.push(p);
            }
        }
    }
    out
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

macro_rules! backend_suite {
    ($mod_name:ident, $backend:ty) => {
        mod $mod_name {
            #[allow(unused_imports)]
            use super::*;
            #[test]
            fn restart_restores_the_list() {
                super::restart_restores_the_list::<$backend>()
            }
            #[test]
            fn the_device_identity_survives_a_restart() {
                super::the_device_identity_survives_a_restart::<$backend>()
            }
            #[test]
            fn editing_after_a_restart_does_not_reuse_a_counter() {
                super::editing_after_a_restart_does_not_reuse_a_counter::<$backend>()
            }
            #[test]
            fn a_snapshot_alone_restores_the_note() {
                super::a_snapshot_alone_restores_the_note::<$backend>()
            }
            #[test]
            fn nothing_on_disk_is_plaintext() {
                super::nothing_on_disk_is_plaintext::<$backend>()
            }
            #[test]
            fn a_torn_log_tail_does_not_lose_the_whole_note() {
                super::a_torn_log_tail_does_not_lose_the_whole_note::<$backend>()
            }
            #[test]
            fn the_store_lists_its_notes() {
                super::the_store_lists_its_notes::<$backend>()
            }
        }
    };
}

#[cfg(feature = "automerge-backend")]
backend_suite!(automerge, easynotes_core::crdt::AutomergeNote);
#[cfg(feature = "yrs-backend")]
backend_suite!(yrs, easynotes_core::crdt::YrsNote);
