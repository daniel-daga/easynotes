//! Demo step 6: kill the app, relaunch it, and the list is still there — with no
//! network and nothing but ciphertext on disk.

use easynotes_core::crdt::NoteDoc;
use easynotes_core::crypto::MasterKey;
use easynotes_core::ids::NoteId;
use easynotes_core::note::{NoteCore, NoteKind};
use easynotes_core::store::{
    master_key_from_file, FileKeyring, FileStore, Keyring, OpStore, SqliteKeyring, SqliteStore,
};
use std::path::{Path, PathBuf};

/// A way of putting a note on a disk. Every test below runs over both, because
/// "the list is still there after a relaunch" has to mean the same thing whether
/// the device writes files or a database.
trait Backing {
    type S: OpStore;
    type K: Keyring;

    /// Open a core over `dir`, as a relaunch would: same directory, nothing kept
    /// in memory from last time.
    fn open<D: NoteDoc>(dir: &Path) -> NoteCore<D, Self::S, Self::K>;

    /// Everything this backing wrote, for the test that greps it all for
    /// plaintext.
    fn files(dir: &Path) -> Vec<PathBuf>;
}

struct Files;

impl Backing for Files {
    type S = FileStore;
    type K = FileKeyring;

    fn open<D: NoteDoc>(dir: &Path) -> NoteCore<D, Self::S, Self::K> {
        NoteCore::open(
            FileStore::open(dir.join("log")).unwrap(),
            FileKeyring::open(dir.join("keys")).unwrap(),
        )
        .unwrap()
    }

    fn files(dir: &Path) -> Vec<PathBuf> {
        walk(&dir.join("log"))
    }
}

struct Sqlite;

impl Sqlite {
    /// The master key is read from the directory, so a "relaunch" finds the same
    /// one the first launch generated — which is what the OS keychain will do
    /// once the app can reach it.
    fn master(dir: &Path) -> MasterKey {
        master_key_from_file(dir.join("master.key")).unwrap()
    }
}

impl Backing for Sqlite {
    type S = SqliteStore;
    type K = SqliteKeyring;

    fn open<D: NoteDoc>(dir: &Path) -> NoteCore<D, Self::S, Self::K> {
        let master = Self::master(dir);
        NoteCore::open(
            SqliteStore::open(dir.join("notes.db"), master.clone()).unwrap(),
            SqliteKeyring::open(dir.join("keys.db"), master).unwrap(),
        )
        .unwrap()
    }

    fn files(dir: &Path) -> Vec<PathBuf> {
        // Including the master key file, which is not supposed to contain any of
        // the secrets either — it is the key to them, not a copy.
        walk(dir)
    }
}

fn restart_restores_the_list<D: NoteDoc, B: Backing>() {
    let dir = tempfile::tempdir().unwrap();

    let id = {
        let mut core = B::open::<D>(dir.path());
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

    let mut core = B::open::<D>(dir.path());
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

fn the_device_identity_survives_a_restart<D: NoteDoc, B: Backing>() {
    let dir = tempfile::tempdir().unwrap();
    let first = B::open::<D>(dir.path()).device_key();
    let second = B::open::<D>(dir.path()).device_key();
    assert_eq!(first, second, "a restart must not mint a new identity");
}

fn editing_after_a_restart_does_not_reuse_a_counter<D: NoteDoc, B: Backing>() {
    // If a relaunched app restarted its counter at zero, its new updates would be
    // mistaken for replays by every peer that already saw counter 0. The counter is
    // recovered from the log for exactly this reason.
    let dir = tempfile::tempdir().unwrap();

    let id = {
        let mut core = B::open::<D>(dir.path());
        let id = core.create_note(NoteKind::Checklist).unwrap();
        core.add_item(id, "milk").unwrap();
        core.add_item(id, "bread").unwrap();
        id
    };

    let mut core = B::open::<D>(dir.path());
    core.open_note(id).unwrap();
    let envelope = core.add_item(id, "eggs").unwrap().1;
    let env = easynotes_core::crypto::Envelope::decode(&envelope).unwrap();
    assert_eq!(env.counter, 2, "counter restarted instead of resuming");

    // And the peer's view really does pick the new item up.
    assert_eq!(core.view(id).unwrap().items.len(), 3);
}

fn a_snapshot_alone_restores_the_note<D: NoteDoc, B: Backing>() {
    let dir = tempfile::tempdir().unwrap();
    let id = {
        let mut core = B::open::<D>(dir.path());
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
    let mut core = B::open::<D>(dir.path());
    let view = core.open_note(id).unwrap();
    assert_eq!(view.title, "Groceries");
    assert_eq!(view.items.len(), 2, "snapshot plus log double-applied");
}

fn nothing_on_disk_is_plaintext<D: NoteDoc, B: Backing>() {
    // The same check the relay gets in `tests/no_plaintext.rs`, applied to local
    // storage: the decrypted document is held in memory only, and every store
    // is handed sealed bytes and nothing else.
    let dir = tempfile::tempdir().unwrap();
    let secrets = ["Groceries", "asparagus", "cortado", "sourdough"];

    let mut core = B::open::<D>(dir.path());
    let id = core.create_note(NoteKind::Checklist).unwrap();
    core.set_title(id, secrets[0]).unwrap();
    for item in &secrets[1..] {
        core.add_item(id, *item).unwrap();
    }
    core.save_snapshot(id).unwrap();

    for entry in B::files(dir.path()) {
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
    // The filesystem store's own format, so this one does not run over both: it
    // does not fsync per append, so a crash can leave a half-written record.
    // Losing the last edit is acceptable; losing the note is not. The SQLite
    // store's answer to the same question is `synchronous = FULL` and a commit
    // per append, which is why it does not lose the last edit either.
    let dir = tempfile::tempdir().unwrap();
    let id = {
        let mut core = Files::open::<D>(dir.path());
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

    let mut core = Files::open::<D>(dir.path());
    let view = core.open_note(id).unwrap();
    assert_eq!(
        view.items.len(),
        2,
        "a torn tail should cost one edit, not the note"
    );
}

fn the_store_lists_its_notes<D: NoteDoc, B: Backing>() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = B::open::<D>(dir.path());
    let mut made: Vec<NoteId> = Vec::new();
    for _ in 0..3 {
        let id = core.create_note(NoteKind::Checklist).unwrap();
        core.add_item(id, "x").unwrap();
        made.push(id);
    }
    made.sort();
    assert_eq!(core.store().notes().unwrap(), made);
}

fn moving_to_the_new_store_keeps_the_notes<D: NoteDoc>() {
    // The app changed store between Slice 1 and Slice 2. A device that had notes
    // in the old one has to find them in the new one, under the same identity —
    // a migration that mints a new device key would make this device a stranger
    // to every peer it already shares a list with.
    let dir = tempfile::tempdir().unwrap();
    let (id, device, cursor) = {
        let mut core = Files::open::<D>(dir.path());
        let id = core.create_note(NoteKind::Checklist).unwrap();
        core.set_title(id, "Groceries").unwrap();
        for item in ["milk", "bread"] {
            core.add_item(id, item).unwrap();
        }
        core.set_sync_cursor(
            id,
            easynotes_core::note::SyncCursor {
                last_seq: 4,
                sent_upto: Some(2),
            },
        )
        .unwrap();
        (id, core.device_key(), core.sync_cursor(id).unwrap())
    };

    // What the app itself runs on the first launch after the update.
    let log_length = {
        let (store, _) = easynotes_core::store::open_local(dir.path()).unwrap();
        store.load(id).unwrap().len()
    };
    assert!(log_length > 0, "the migration copied no envelopes");

    // And on the second launch, which must not copy the log in again.
    {
        let (store, _) = easynotes_core::store::open_local(dir.path()).unwrap();
        assert_eq!(
            store.load(id).unwrap().len(),
            log_length,
            "the migration ran twice and doubled the log"
        );
    }

    let mut core = Sqlite::open::<D>(dir.path());
    let view = core.open_note(id).unwrap();
    assert_eq!(view.title, "Groceries");
    assert_eq!(view.items.len(), 2);
    assert_eq!(
        core.device_key(),
        device,
        "the migration minted a new identity"
    );
    assert_eq!(
        core.sync_cursor(id).unwrap(),
        cursor,
        "the migration lost the sync cursor, so the whole log would go to the relay again"
    );

    // And the old store still has everything, because nothing was deleted.
    let mut old = Files::open::<D>(dir.path());
    assert_eq!(old.open_note(id).unwrap().items.len(), 2);
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

            store_suite!(files, $backend, super::super::Files);
            store_suite!(sqlite, $backend, super::super::Sqlite);

            #[test]
            fn a_torn_log_tail_does_not_lose_the_whole_note() {
                super::a_torn_log_tail_does_not_lose_the_whole_note::<$backend>()
            }

            #[test]
            fn moving_to_the_new_store_keeps_the_notes() {
                super::moving_to_the_new_store_keeps_the_notes::<$backend>()
            }
        }
    };
}

/// Every persistence test, for one CRDT backend on one store. A relaunch means
/// the same thing on both, or one of them is wrong.
macro_rules! store_suite {
    ($mod_name:ident, $backend:ty, $backing:ty) => {
        mod $mod_name {
            #[allow(unused_imports)]
            use super::*;
            #[test]
            fn restart_restores_the_list() {
                super::super::restart_restores_the_list::<$backend, $backing>()
            }
            #[test]
            fn the_device_identity_survives_a_restart() {
                super::super::the_device_identity_survives_a_restart::<$backend, $backing>()
            }
            #[test]
            fn editing_after_a_restart_does_not_reuse_a_counter() {
                super::super::editing_after_a_restart_does_not_reuse_a_counter::<$backend, $backing>()
            }
            #[test]
            fn a_snapshot_alone_restores_the_note() {
                super::super::a_snapshot_alone_restores_the_note::<$backend, $backing>()
            }
            #[test]
            fn nothing_on_disk_is_plaintext() {
                super::super::nothing_on_disk_is_plaintext::<$backend, $backing>()
            }
            #[test]
            fn the_store_lists_its_notes() {
                super::super::the_store_lists_its_notes::<$backend, $backing>()
            }
        }
    };
}

#[cfg(feature = "automerge-backend")]
backend_suite!(automerge, easynotes_core::crdt::AutomergeNote);
#[cfg(feature = "yrs-backend")]
backend_suite!(yrs, easynotes_core::crdt::YrsNote);
