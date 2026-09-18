//! One conformance suite, run over every store.
//!
//! The stores are interchangeable by design — the app picks one at runtime and
//! nothing above the trait knows which — so "it works" has to mean the same thing
//! for all of them. Everything in the first half runs against all three; the
//! second half is about the SQLite store specifically, which is the one that
//! holds a key and a file format.

use easynotes_core::crypto::{DeviceSeed, MasterKey, NoteKey};
use easynotes_core::ids::NoteId;
use easynotes_core::store::{Keyring, MemoryStore, OpStore};
use std::path::Path;

// ---------------------------------------------------------------------------
// The suite every store passes
// ---------------------------------------------------------------------------

fn appends_come_back_in_order(store: &mut impl OpStore) {
    let note = NoteId::random();
    for i in 0..5u8 {
        store.append(note, &[i; 4]).unwrap();
    }
    assert_eq!(
        store.load(note).unwrap(),
        (0..5u8).map(|i| vec![i; 4]).collect::<Vec<_>>()
    );
}

fn an_unknown_note_has_an_empty_log(store: &mut impl OpStore) {
    assert!(store.load(NoteId::random()).unwrap().is_empty());
    assert!(store.get_snapshot(NoteId::random()).unwrap().is_none());
}

fn logs_do_not_bleed_between_notes(store: &mut impl OpStore) {
    let (a, b) = (NoteId::random(), NoteId::random());
    store.append(a, b"a1").unwrap();
    store.append(b, b"b1").unwrap();
    store.append(a, b"a2").unwrap();
    assert_eq!(store.load(a).unwrap(), vec![b"a1".to_vec(), b"a2".to_vec()]);
    assert_eq!(store.load(b).unwrap(), vec![b"b1".to_vec()]);
}

fn a_snapshot_replaces_the_previous_one(store: &mut impl OpStore) {
    let note = NoteId::random();
    store.put_snapshot(note, b"first").unwrap();
    store.put_snapshot(note, b"second").unwrap();
    assert_eq!(store.get_snapshot(note).unwrap().unwrap(), b"second");
}

fn meta_round_trips_and_overwrites(store: &mut impl OpStore) {
    let note = NoteId::random();
    assert!(store.get_meta(note, "sync").unwrap().is_none());
    store.put_meta(note, "sync", b"one").unwrap();
    assert_eq!(store.get_meta(note, "sync").unwrap().unwrap(), b"one");
    store.put_meta(note, "sync", b"two").unwrap();
    assert_eq!(store.get_meta(note, "sync").unwrap().unwrap(), b"two");
}

fn meta_is_keyed_by_note_and_name(store: &mut impl OpStore) {
    let (a, b) = (NoteId::random(), NoteId::random());
    store.put_meta(a, "sync", b"a-sync").unwrap();
    store.put_meta(a, "other", b"a-other").unwrap();
    store.put_meta(b, "sync", b"b-sync").unwrap();
    assert_eq!(store.get_meta(a, "sync").unwrap().unwrap(), b"a-sync");
    assert_eq!(store.get_meta(a, "other").unwrap().unwrap(), b"a-other");
    assert_eq!(store.get_meta(b, "sync").unwrap().unwrap(), b"b-sync");
}

fn notes_lists_everything_once_and_in_order(store: &mut impl OpStore) {
    let mut made: Vec<NoteId> = (0..3).map(|_| NoteId::random()).collect();
    store.append(made[0], b"x").unwrap();
    store.append(made[0], b"y").unwrap();
    store.append(made[1], b"x").unwrap();
    // A note that has been compacted to a snapshot with no log is still a note.
    store.put_snapshot(made[2], b"s").unwrap();
    store.put_snapshot(made[1], b"s").unwrap();
    made.sort();
    assert_eq!(store.notes().unwrap(), made);
}

fn an_empty_envelope_is_still_an_entry(store: &mut impl OpStore) {
    // Nothing in the core writes one, but a store that silently dropped it would
    // be the kind of difference that only shows up under another implementation.
    let note = NoteId::random();
    store.append(note, b"").unwrap();
    assert_eq!(store.load(note).unwrap(), vec![Vec::<u8>::new()]);
}

fn forgetting_a_note_leaves_nothing_of_it(store: &mut impl OpStore) {
    let note = NoteId::random();
    store.append(note, b"one").unwrap();
    store.append(note, b"two").unwrap();
    store.put_snapshot(note, b"snap").unwrap();
    store.put_meta(note, "sync", b"cursor").unwrap();
    // The one that matters most: the index entry holds the note's whole text, so
    // a delete that missed it would leave the readable copy behind.
    store
        .put_meta(note, "index", b"the note, in words")
        .unwrap();

    store.forget(note).unwrap();

    assert!(store.load(note).unwrap().is_empty());
    assert!(store.get_snapshot(note).unwrap().is_none());
    assert!(store.get_meta(note, "sync").unwrap().is_none());
    assert!(store.get_meta(note, "index").unwrap().is_none());
    assert_eq!(store.log_len(note).unwrap(), 0);
    // And it is not a note this device has any more, which is what the note list
    // reads.
    assert!(!store.notes().unwrap().contains(&note));
}

fn forgetting_a_note_leaves_the_others_alone(store: &mut impl OpStore) {
    let (gone, kept) = (NoteId::random(), NoteId::random());
    for note in [gone, kept] {
        store.append(note, b"log").unwrap();
        store.put_snapshot(note, b"snap").unwrap();
        store.put_meta(note, "index", b"words").unwrap();
    }

    store.forget(gone).unwrap();

    assert_eq!(store.load(kept).unwrap(), vec![b"log".to_vec()]);
    assert_eq!(store.get_snapshot(kept).unwrap().unwrap(), b"snap");
    assert_eq!(store.get_meta(kept, "index").unwrap().unwrap(), b"words");
    assert_eq!(store.notes().unwrap(), vec![kept]);
}

fn forgetting_a_note_that_is_not_there_is_not_an_error(store: &mut impl OpStore) {
    // Emptying an empty trash, and finishing a delete a crash interrupted, both
    // arrive here.
    let note = NoteId::random();
    store.forget(note).unwrap();
    store.append(note, b"log").unwrap();
    store.forget(note).unwrap();
    store.forget(note).unwrap();
    assert!(store.load(note).unwrap().is_empty());
}

fn a_forgotten_note_can_be_written_again(store: &mut impl OpStore) {
    // Not because anything asks for it — a note ID is random and never reused —
    // but because a store that left a tombstone where a note had been would be
    // hiding state the trait does not describe.
    let note = NoteId::random();
    store.append(note, b"first life").unwrap();
    store.forget(note).unwrap();
    store.append(note, b"second life").unwrap();
    assert_eq!(store.load(note).unwrap(), vec![b"second life".to_vec()]);
}

fn the_keyring_forgets_one_key_and_keeps_the_rest(keyring: &mut impl Keyring) {
    let (gone, kept) = (NoteId::random(), NoteId::random());
    keyring.put_device_seed(&DeviceSeed([7u8; 64])).unwrap();
    keyring.put_note_key(gone, &NoteKey([1u8; 32])).unwrap();
    keyring.put_note_key(kept, &NoteKey([2u8; 32])).unwrap();

    keyring.forget_note_key(gone).unwrap();

    assert!(keyring.note_key(gone).unwrap().is_none());
    assert_eq!(keyring.note_key(kept).unwrap().unwrap().0, [2u8; 32]);
    // The identity is not a note key, and forgetting a note is not forgetting
    // the device.
    assert_eq!(keyring.device_seed().unwrap().unwrap().0, [7u8; 64]);
    // Twice is the same as once.
    keyring.forget_note_key(gone).unwrap();
}

fn the_keyring_round_trips(keyring: &mut impl Keyring) {
    let note = NoteId::random();
    assert!(keyring.device_seed().unwrap().is_none());
    assert!(keyring.note_key(note).unwrap().is_none());

    keyring.put_device_seed(&DeviceSeed([7u8; 64])).unwrap();
    keyring.put_note_key(note, &NoteKey([9u8; 32])).unwrap();

    assert_eq!(keyring.device_seed().unwrap().unwrap().0, [7u8; 64]);
    assert_eq!(keyring.note_key(note).unwrap().unwrap().0, [9u8; 32]);
    assert!(keyring.note_key(NoteId::random()).unwrap().is_none());
}

macro_rules! store_suite {
    ($name:ident, $store:expr, $keyring:expr) => {
        mod $name {
            #[allow(unused_imports)]
            use super::*;

            #[test]
            fn appends_come_back_in_order() {
                super::appends_come_back_in_order(&mut $store);
            }
            #[test]
            fn an_unknown_note_has_an_empty_log() {
                super::an_unknown_note_has_an_empty_log(&mut $store);
            }
            #[test]
            fn logs_do_not_bleed_between_notes() {
                super::logs_do_not_bleed_between_notes(&mut $store);
            }
            #[test]
            fn a_snapshot_replaces_the_previous_one() {
                super::a_snapshot_replaces_the_previous_one(&mut $store);
            }
            #[test]
            fn meta_round_trips_and_overwrites() {
                super::meta_round_trips_and_overwrites(&mut $store);
            }
            #[test]
            fn meta_is_keyed_by_note_and_name() {
                super::meta_is_keyed_by_note_and_name(&mut $store);
            }
            #[test]
            fn notes_lists_everything_once_and_in_order() {
                super::notes_lists_everything_once_and_in_order(&mut $store);
            }
            #[test]
            fn an_empty_envelope_is_still_an_entry() {
                super::an_empty_envelope_is_still_an_entry(&mut $store);
            }
            #[test]
            fn forgetting_a_note_leaves_nothing_of_it() {
                super::forgetting_a_note_leaves_nothing_of_it(&mut $store);
            }
            #[test]
            fn forgetting_a_note_leaves_the_others_alone() {
                super::forgetting_a_note_leaves_the_others_alone(&mut $store);
            }
            #[test]
            fn forgetting_a_note_that_is_not_there_is_not_an_error() {
                super::forgetting_a_note_that_is_not_there_is_not_an_error(&mut $store);
            }
            #[test]
            fn a_forgotten_note_can_be_written_again() {
                super::a_forgotten_note_can_be_written_again(&mut $store);
            }
            #[test]
            fn the_keyring_round_trips() {
                super::the_keyring_round_trips(&mut $keyring);
            }
            #[test]
            fn the_keyring_forgets_one_key_and_keeps_the_rest() {
                super::the_keyring_forgets_one_key_and_keeps_the_rest(&mut $keyring);
            }
        }
    };
}

store_suite!(
    memory,
    MemoryStore::new(),
    easynotes_core::store::MemoryKeyring::new()
);

store_suite!(
    file,
    {
        let dir = tempfile::tempdir().unwrap();
        let store = easynotes_core::store::FileStore::open(dir.path().join("log")).unwrap();
        // The directory has to outlive the store, and the store borrows nothing
        // from it, so leaking the handle is the whole trick: the test process
        // ends and the temporary directory goes with it.
        std::mem::forget(dir);
        store
    },
    {
        let dir = tempfile::tempdir().unwrap();
        let keyring = easynotes_core::store::FileKeyring::open(dir.path().join("keys")).unwrap();
        std::mem::forget(dir);
        keyring
    }
);

store_suite!(
    sqlite,
    easynotes_core::store::SqliteStore::in_memory(MasterKey::random()).unwrap(),
    easynotes_core::store::SqliteKeyring::in_memory(MasterKey::random()).unwrap()
);

// ---------------------------------------------------------------------------
// The SQLite store in particular
// ---------------------------------------------------------------------------

mod sqlite_file {
    use super::*;
    use easynotes_core::store::{SqliteKeyring, SqliteStore};

    fn secrets_in(dir: &Path) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            if entry.path().is_file() {
                out.push(std::fs::read(entry.path()).unwrap());
            }
        }
        assert!(!out.is_empty(), "nothing was written at all");
        out
    }

    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        haystack.windows(needle.len()).any(|w| w == needle)
    }

    #[test]
    fn a_reopened_database_has_everything_in_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.db");
        let master = MasterKey([3u8; 32]);
        let note = NoteId::random();

        {
            let mut store = SqliteStore::open(&path, master.clone()).unwrap();
            store.append(note, b"one").unwrap();
            store.append(note, b"two").unwrap();
            store.put_snapshot(note, b"snap").unwrap();
            store.put_meta(note, "sync", b"cursor").unwrap();
        } // the app is killed here

        let store = SqliteStore::open(&path, master).unwrap();
        assert_eq!(
            store.load(note).unwrap(),
            vec![b"one".to_vec(), b"two".to_vec()]
        );
        assert_eq!(store.get_snapshot(note).unwrap().unwrap(), b"snap");
        assert_eq!(store.get_meta(note, "sync").unwrap().unwrap(), b"cursor");
        assert_eq!(store.notes().unwrap(), vec![note]);
    }

    #[test]
    fn an_append_is_committed_before_it_returns() {
        // `synchronous = FULL` and a commit per append: a second connection,
        // which is as close as a test gets to a process that did not do the
        // writing, sees the edit without the writer being closed first.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.db");
        let note = NoteId::random();

        let mut writer = SqliteStore::open(&path, MasterKey([1u8; 32])).unwrap();
        writer.append(note, b"milk").unwrap();

        let reader = SqliteStore::open(&path, MasterKey([1u8; 32])).unwrap();
        assert_eq!(reader.load(note).unwrap(), vec![b"milk".to_vec()]);
    }

    #[test]
    fn the_wrong_master_key_does_not_quietly_return_the_wrong_thing() {
        // A user restoring a database without its key should be told, not handed
        // a cursor that is not theirs.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.db");
        let note = NoteId::random();

        let mut store = SqliteStore::open(&path, MasterKey([1u8; 32])).unwrap();
        store.append(note, b"envelope").unwrap();
        store.put_meta(note, "sync", b"cursor").unwrap();
        drop(store);

        let other = SqliteStore::open(&path, MasterKey([2u8; 32])).unwrap();
        assert!(other.get_meta(note, "sync").is_err());
        // The log is sealed under the note key, not this one, so it is readable
        // as ciphertext and useless without the keyring. That is the design, and
        // it is worth asserting so a change to it is deliberate.
        assert_eq!(other.load(note).unwrap(), vec![b"envelope".to_vec()]);
    }

    #[test]
    fn a_keyring_needs_its_master_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keys.db");
        let note = NoteId::random();

        let mut keys = SqliteKeyring::open(&path, MasterKey([1u8; 32])).unwrap();
        keys.put_device_seed(&DeviceSeed([7u8; 64])).unwrap();
        keys.put_note_key(note, &NoteKey([9u8; 32])).unwrap();
        drop(keys);

        let stolen = SqliteKeyring::open(&path, MasterKey([2u8; 32])).unwrap();
        assert!(stolen.device_seed().is_err());
        assert!(stolen.note_key(note).is_err());

        let ours = SqliteKeyring::open(&path, MasterKey([1u8; 32])).unwrap();
        assert_eq!(ours.note_key(note).unwrap().unwrap().0, [9u8; 32]);
    }

    #[test]
    fn nothing_readable_is_in_the_database_or_its_wal() {
        let dir = tempfile::tempdir().unwrap();
        let master = MasterKey::random();
        let mut store = SqliteStore::open(dir.path().join("notes.db"), master.clone()).unwrap();
        let mut keys = SqliteKeyring::open(dir.path().join("keys.db"), master).unwrap();
        let note = NoteId::random();

        // The op log only ever receives sealed envelopes, so what is at risk here
        // is everything else: the cursor, the seed and the note key.
        store.append(note, b"sealed-envelope-stand-in").unwrap();
        store.put_meta(note, "sync", b"asparagus").unwrap();
        keys.put_device_seed(&DeviceSeed([0xA5; 64])).unwrap();
        keys.put_note_key(note, &NoteKey([0x5A; 32])).unwrap();

        for file in secrets_in(dir.path()) {
            assert!(
                !contains(&file, b"asparagus"),
                "a local value is in the database in the clear"
            );
            assert!(
                !contains(&file, &[0xA5; 64]),
                "the device seed is in the database in the clear"
            );
            assert!(
                !contains(&file, &[0x5A; 32]),
                "a note key is in the database in the clear"
            );
        }
    }

    #[test]
    fn a_forgotten_note_is_not_in_the_database_or_its_wal() {
        // The other half of `nothing_readable_is_in_the_database_or_its_wal`.
        // That one holds that nothing readable is ever written; this one holds
        // that what was written stops being there — which a `DELETE` does not do
        // on its own. Both read the actual bytes, because both are claims about
        // the file and not about the API.
        let dir = tempfile::tempdir().unwrap();
        let master = MasterKey([5u8; 32]);
        let note = NoteId::random();
        let mut store = SqliteStore::open(dir.path().join("notes.db"), master.clone()).unwrap();
        let mut keyring = SqliteKeyring::open(dir.path().join("keys.db"), master).unwrap();

        // Everything a note puts in the two files. The envelopes are ciphertext
        // and the metadata is sealed, so the strings below stand in for them: if
        // a recognisable one survives, so would the real thing.
        store.append(note, b"ENVELOPE-ONE").unwrap();
        store.append(note, b"ENVELOPE-TWO").unwrap();
        store.put_snapshot(note, b"SNAPSHOT-HERE").unwrap();
        store
            .put_meta(note, "index", b"THE-NOTES-OWN-WORDS")
            .unwrap();
        // A recognisable 32 bytes rather than a repeated one, so that the filler
        // below cannot accidentally write the needle this looks for.
        let secret = NoteKey(*b"NOTE-KEY-THAT-MUST-NOT-SURVIVE!!");
        keyring.put_note_key(note, &secret).unwrap();
        // Enough other notes that a page is not still in SQLite's cache,
        // unwritten.
        for i in 0..200u8 {
            store.append(NoteId::random(), &[i; 64]).unwrap();
        }

        store.forget(note).unwrap();
        keyring.forget_note_key(note).unwrap();

        // The connections are still open: this is the app mid-session, not a
        // relaunch, which is the harder case for the WAL.
        for file in secrets_in(dir.path()) {
            for needle in [
                &b"ENVELOPE-ONE"[..],
                b"ENVELOPE-TWO",
                b"SNAPSHOT-HERE",
                b"THE-NOTES-OWN-WORDS",
                &secret.0,
            ] {
                assert!(
                    !contains(&file, needle),
                    "a forgotten note left {} bytes behind",
                    needle.len()
                );
            }
        }
    }

    #[test]
    fn a_database_from_a_newer_app_is_refused() {
        // Better than opening it, ignoring the columns this build does not know
        // about, and writing rows the newer one cannot read back.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.db");
        SqliteStore::open(&path, MasterKey([1u8; 32])).unwrap();

        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.pragma_update(None, "user_version", 99).unwrap();
        drop(conn);

        let err = match SqliteStore::open(&path, MasterKey([1u8; 32])) {
            Ok(_) => panic!("a database from a newer app was opened anyway"),
            Err(e) => e,
        };
        assert!(
            err.to_string().contains("newer version"),
            "unhelpful error: {err}"
        );
    }

    #[test]
    fn two_launches_at_once_agree_on_one_master_key() {
        // Two windows of the app opening on first run. If they each minted a key
        // and the last one won, the first one's database would be sealed with a
        // key that no longer exists anywhere.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("master.key");
        let keys: Vec<[u8; 32]> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..8)
                .map(|_| {
                    scope.spawn(|| {
                        easynotes_core::store::master_key_from_file(&path)
                            .unwrap()
                            .0
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        assert!(
            keys.windows(2).all(|w| w[0] == w[1]),
            "concurrent first launches minted different master keys"
        );
    }

    #[test]
    fn the_master_key_file_is_made_once_and_then_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("master.key");
        let first = easynotes_core::store::master_key_from_file(&path).unwrap();
        let second = easynotes_core::store::master_key_from_file(&path).unwrap();
        assert_eq!(first.0, second.0, "a relaunch minted a new master key");
        assert_ne!(first.0, [0u8; 32]);
    }
}
