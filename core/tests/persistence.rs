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

fn compaction_keeps_the_list_and_loses_the_log<D: NoteDoc, B: Backing>() {
    let dir = tempfile::tempdir().unwrap();
    let id = {
        let mut core = B::open::<D>(dir.path());
        let id = core.create_note(NoteKind::Checklist).unwrap();
        core.set_title(id, "Groceries").unwrap();
        for item in ["milk", "bread", "eggs"] {
            core.add_item(id, item).unwrap();
        }
        // The relay has everything: nothing is waiting to be sent, so nothing
        // has to be kept.
        core.set_sync_cursor(
            id,
            easynotes_core::note::SyncCursor {
                last_seq: 4,
                sent_upto: Some(3),
            },
        )
        .unwrap();

        let before = core.store().log_len(id).unwrap();
        assert_eq!(core.compact(id).unwrap(), before);
        assert_eq!(core.store().log_len(id).unwrap(), 0);
        id
    };

    // And the note is still the note, from the snapshot alone.
    let mut core = B::open::<D>(dir.path());
    let view = core.open_note(id).unwrap();
    assert_eq!(view.title, "Groceries");
    assert_eq!(
        view.items
            .iter()
            .map(|i| i.text.as_str())
            .collect::<Vec<_>>(),
        ["milk", "bread", "eggs"]
    );
}

fn compaction_keeps_what_the_relay_has_not_acknowledged<D: NoteDoc, B: Backing>() {
    // The outbox is derived from the log (ADR 0004), so an unacknowledged
    // envelope is the only copy of that edit anywhere. Compaction may not be the
    // thing that loses it.
    let dir = tempfile::tempdir().unwrap();
    let mut core = B::open::<D>(dir.path());
    let id = core.create_note(NoteKind::Checklist).unwrap();
    for item in ["milk", "bread", "eggs", "coffee"] {
        core.add_item(id, item).unwrap();
    }
    core.set_sync_cursor(
        id,
        easynotes_core::note::SyncCursor {
            last_seq: 2,
            sent_upto: Some(1),
        },
    )
    .unwrap();

    let unsent_before = core.unsent(id, Some(1)).unwrap();
    assert_eq!(unsent_before.len(), 2, "the test is not testing anything");

    core.compact(id).unwrap();

    assert_eq!(
        core.unsent(id, Some(1)).unwrap(),
        unsent_before,
        "compaction dropped an edit the relay has never seen"
    );
}

fn a_note_that_has_never_synced_keeps_its_whole_log<D: NoteDoc, B: Backing>() {
    // Because that log is the only way its content ever reaches a relay, and the
    // only way a guest who joins tomorrow sees anything at all.
    let dir = tempfile::tempdir().unwrap();
    let mut core = B::open::<D>(dir.path());
    let id = core.create_note(NoteKind::Checklist).unwrap();
    for item in ["milk", "bread"] {
        core.add_item(id, item).unwrap();
    }
    let before = core.store().log_len(id).unwrap();

    assert_eq!(core.compact(id).unwrap(), 0);
    assert_eq!(core.store().log_len(id).unwrap(), before);
    assert!(
        core.store().get_snapshot(id).unwrap().is_none(),
        "a compaction that dropped nothing still wrote a snapshot"
    );
}

fn an_envelope_compaction_dropped_is_still_a_no_op<D: NoteDoc, B: Backing>() {
    // Compaction forgets the replay guard for what it drops: the guard is the set
    // of (author, counter) pairs rebuilt from the log. The CRDT's own idempotence
    // is what remains, and it is enough to keep the list right. The only cost is
    // that the envelope is written back into the log, which the next compaction
    // removes again.
    let dir = tempfile::tempdir().unwrap();
    let (id, dropped) = {
        let mut core = B::open::<D>(dir.path());
        let id = core.create_note(NoteKind::Checklist).unwrap();
        for item in ["milk", "bread"] {
            core.add_item(id, item).unwrap();
        }
        let dropped = core.store().load(id).unwrap()[0].clone();
        core.set_sync_cursor(
            id,
            easynotes_core::note::SyncCursor {
                last_seq: 2,
                sent_upto: Some(1),
            },
        )
        .unwrap();
        core.compact(id).unwrap();
        (id, dropped)
    };

    // A relaunch, so the guard is only what the shortened log rebuilds, and then
    // the relay re-delivers something it was never told we had.
    let mut core = B::open::<D>(dir.path());
    let before = core.open_note(id).unwrap();
    let after = core.apply_remote(id, &dropped).unwrap();
    assert_eq!(
        before, after,
        "a re-delivered old envelope changed the list"
    );
    assert_eq!(
        core.store().log_len(id).unwrap(),
        1,
        "the re-delivered envelope should cost exactly one log entry"
    );
}

fn compaction_does_not_restart_the_author_counter<D: NoteDoc, B: Backing>() {
    // The counter is recovered by replaying the log, and compaction is the one
    // thing that can take the log away. If it restarted at zero, every peer that
    // had already seen counter 0 would treat this device's next edit as a replay
    // and drop it.
    let dir = tempfile::tempdir().unwrap();
    let id = {
        let mut core = B::open::<D>(dir.path());
        let id = core.create_note(NoteKind::Checklist).unwrap();
        for item in ["milk", "bread", "eggs"] {
            core.add_item(id, item).unwrap();
        }
        core.set_sync_cursor(
            id,
            easynotes_core::note::SyncCursor {
                last_seq: 3,
                sent_upto: Some(2),
            },
        )
        .unwrap();
        core.compact(id).unwrap();
        id
    };

    let mut core = B::open::<D>(dir.path());
    core.open_note(id).unwrap();
    let envelope = core.add_item(id, "coffee").unwrap().1;
    let env = easynotes_core::crypto::Envelope::decode(&envelope).unwrap();
    assert!(
        env.counter > 3,
        "counter went back to {} after a compaction",
        env.counter
    );
    assert_eq!(core.view(id).unwrap().items.len(), 4);
}

fn opening_a_long_log_compacts_it<D: NoteDoc, B: Backing>() {
    use easynotes_core::note::COMPACT_AFTER;

    let dir = tempfile::tempdir().unwrap();
    let id = {
        let mut core = B::open::<D>(dir.path());
        let id = core.create_note(NoteKind::Checklist).unwrap();
        core.set_title(id, "Groceries").unwrap();
        core.add_item(id, "milk").unwrap();
        let (item, _) = core.add_item(id, "bread").unwrap();
        for i in 0..COMPACT_AFTER {
            core.set_checked(id, item, i % 2 == 0).unwrap();
        }
        // Everything has been acknowledged, which is the state a note that has
        // been syncing all week is in.
        let last = core.unsent(id, None).unwrap().last().unwrap().0;
        core.set_sync_cursor(
            id,
            easynotes_core::note::SyncCursor {
                last_seq: last as i64,
                sent_upto: Some(last),
            },
        )
        .unwrap();
        assert!(core.store().log_len(id).unwrap() > COMPACT_AFTER);
        id
    };

    let mut core = B::open::<D>(dir.path());
    let view = core.open_note(id).unwrap();
    assert_eq!(view.title, "Groceries");
    assert_eq!(view.items.len(), 2);
    assert_eq!(
        core.store().log_len(id).unwrap(),
        0,
        "opening a long log left it long"
    );

    // And the next open is cheap and still right.
    let mut core = B::open::<D>(dir.path());
    let view = core.open_note(id).unwrap();
    assert_eq!(view.items.len(), 2);
    assert_eq!(
        view.items[1].checked,
        core.view(id).unwrap().items[1].checked
    );
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
            #[test]
            fn compaction_keeps_the_list_and_loses_the_log() {
                super::super::compaction_keeps_the_list_and_loses_the_log::<$backend, $backing>()
            }
            #[test]
            fn compaction_keeps_what_the_relay_has_not_acknowledged() {
                super::super::compaction_keeps_what_the_relay_has_not_acknowledged::<$backend, $backing>()
            }
            #[test]
            fn a_note_that_has_never_synced_keeps_its_whole_log() {
                super::super::a_note_that_has_never_synced_keeps_its_whole_log::<$backend, $backing>()
            }
            #[test]
            fn an_envelope_compaction_dropped_is_still_a_no_op() {
                super::super::an_envelope_compaction_dropped_is_still_a_no_op::<$backend, $backing>()
            }
            #[test]
            fn compaction_does_not_restart_the_author_counter() {
                super::super::compaction_does_not_restart_the_author_counter::<$backend, $backing>()
            }
            #[test]
            fn opening_a_long_log_compacts_it() {
                super::super::opening_a_long_log_compacts_it::<$backend, $backing>()
            }
        }
    };
}

#[cfg(feature = "automerge-backend")]
backend_suite!(automerge, easynotes_core::crdt::AutomergeNote);
#[cfg(feature = "yrs-backend")]
backend_suite!(yrs, easynotes_core::crdt::YrsNote);
