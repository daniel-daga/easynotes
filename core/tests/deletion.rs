//! Deleting a note for good, across one person's devices.
//!
//! The acceptance checks of `docs/plan-permanent-deletion.md`, as far as a core
//! can run them without a relay: two cores from one recovery phrase, and what
//! one of them wrote carried to the other by hand, exactly as the sync engine
//! would carry it over a socket. `relay/tests/account.rs` runs the same path
//! over a real relay.
//!
//! [ADR 0022](../../docs/adr/0022-account-wide-deletion.md) is the design.

use easynotes_core::account::RecoveryPhrase;
use easynotes_core::crdt::account::{deleted_notes_of, note_access_of};
use easynotes_core::crdt::{Colour, NoteDoc, RepeatRule};
use easynotes_core::crypto::{MasterKey, NoteKey};
use easynotes_core::error::{CoreError, Result};
use easynotes_core::ids::NoteId;
use easynotes_core::note::{NoteCore, NoteKind};
use easynotes_core::store::{
    master_key_from_file, Keyring, MemoryKeyring, MemoryStore, OpStore, SqliteKeyring, SqliteStore,
};
use std::cell::Cell;
use std::path::Path;
use std::rc::Rc;

type Memory<D> = NoteCore<D, MemoryStore, MemoryKeyring>;

fn in_memory<D: NoteDoc>() -> Memory<D> {
    NoteCore::open(MemoryStore::new(), MemoryKeyring::new()).unwrap()
}

fn on_disk<D: NoteDoc>(dir: &Path) -> NoteCore<D, SqliteStore, SqliteKeyring> {
    let master: MasterKey = master_key_from_file(dir.join("master.key")).unwrap();
    NoteCore::open(
        SqliteStore::open(dir.join("notes.db"), master.clone()).unwrap(),
        SqliteKeyring::open(dir.join("keys.db"), master).unwrap(),
    )
    .unwrap()
}

/// A second device from the same words.
fn restored<D: NoteDoc>(phrase: &RecoveryPhrase) -> Memory<D> {
    let mut device = in_memory::<D>();
    let phrase = RecoveryPhrase::parse(&phrase.words()).unwrap();
    device.adopt_account(&phrase).unwrap();
    device
}

/// Everything one device has said about its account, to the other, and then
/// the settle step a relay's `Caught` triggers. Duplicates are the receiver's
/// problem, as they are over a socket.
fn deliver_account<D: NoteDoc, S: OpStore, K: Keyring, S2: OpStore, K2: Keyring>(
    from: &NoteCore<D, S, K>,
    to: &mut NoteCore<D, S2, K2>,
) {
    let id = from.account_id().expect("the sender has an account");
    for envelope in from.store().load(id).unwrap() {
        to.apply_remote_account(&envelope).unwrap();
    }
    to.adopt_note_keys().unwrap();
}

/// A note's log, from one device to another, as envelopes.
fn deliver_note<D: NoteDoc>(from: &Memory<D>, to: &mut Memory<D>, note: NoteId) {
    for envelope in from.store().load(note).unwrap() {
        to.apply_remote(note, &envelope).unwrap();
    }
}

fn account_log<D: NoteDoc, S: OpStore, K: Keyring>(core: &NoteCore<D, S, K>) -> usize {
    core.store().load(core.account_id().unwrap()).unwrap().len()
}

/// Nothing of the note is left on this device: not its log, its key, its card,
/// its search entry, its placement, its labels, its reminders or its invites.
fn nothing_left<D: NoteDoc, S: OpStore, K: Keyring>(core: &mut NoteCore<D, S, K>, note: NoteId) {
    assert!(core.is_deleted_note(note), "the marker is there");
    assert!(!core.store().notes().unwrap().contains(&note), "no rows");
    assert!(core.store().load(note).unwrap().is_empty(), "no log");
    assert!(core.store().get_snapshot(note).unwrap().is_none());
    assert!(core.keyring().note_key(note).unwrap().is_none(), "no key");
    assert!(core.open_note(note).is_err(), "it does not open");
    assert!(
        core.notes().unwrap().iter().all(|n| n.id != note),
        "no card"
    );
    assert!(
        core.search("milk")
            .unwrap()
            .iter()
            .all(|h| h.summary.id != note),
        "no search hit"
    );
    assert!(core.tags_on(note).is_empty(), "no labels");
    assert!(core.reminders_on(note).is_empty(), "no reminders");
    assert!(
        !core.account().placements.contains_key(&note),
        "no placement"
    );
    assert!(
        !note_access_of(core.account_entries()).contains_key(&note),
        "no key in the account"
    );
    assert!(core.invites().unwrap().iter().all(|i| i.note != note));
    assert!(
        core.unsent(note, None).unwrap().is_empty(),
        "nothing to send"
    );
    assert!(!core.syncable().unwrap().contains(&note), "not subscribed");
}

/// A note with something in every place a deletion has to reach.
fn a_full_note<D: NoteDoc, S: OpStore, K: Keyring>(core: &mut NoteCore<D, S, K>) -> NoteId {
    let note = core.create_note(NoteKind::Checklist).unwrap();
    core.set_title(note, "Groceries").unwrap();
    core.add_item(note, "milk").unwrap();
    core.set_colour(note, Some(Colour::Yellow)).unwrap();
    core.set_pinned(note, true).unwrap();
    core.add_tag(note, "home").unwrap();
    core.create_reminder(note, 1_800_000_000_000, RepeatRule::Never, "UTC".into())
        .unwrap();
    note
}

// ---------------------------------------------------------------------------
// One person's two devices
// ---------------------------------------------------------------------------

/// The central promise: delete on the phone, and the laptop loses everything
/// of the note when it syncs.
fn a_deletion_reaches_the_other_device_and_takes_everything<D: NoteDoc>() {
    let mut phone = in_memory::<D>();
    let phrase = phone.create_account().unwrap();
    let note = a_full_note(&mut phone);

    let mut laptop = restored::<D>(&phrase);
    deliver_account(&phone, &mut laptop);
    deliver_note(&phone, &mut laptop, note);
    laptop.set_profile_name("Dana").unwrap();
    let invite = laptop.new_invite(note).unwrap();
    laptop.remember_invite(&invite).unwrap();
    assert_eq!(laptop.search("milk").unwrap().len(), 1, "it was there");
    assert_eq!(laptop.reminders_on(note).len(), 1);

    phone.set_trashed(note, true).unwrap();
    phone.forget_note(note).unwrap();
    nothing_left(&mut phone, note);

    deliver_account(&phone, &mut laptop);
    nothing_left(&mut laptop, note);
    // The label itself is this person's, not the note's, and stays.
    assert!(laptop.tags().iter().any(|(_, name, _)| name == "home"));
}

/// Deleting and then syncing is a finished conversation. A device that has
/// enforced a marker must not write another envelope about it — not on a
/// relaunch, and not on receiving the other device's account. If it did, two
/// online devices would answer each other for ever.
fn devices_that_agree_about_a_deletion_stop_writing<D: NoteDoc>() {
    let mut phone = in_memory::<D>();
    let phrase = phone.create_account().unwrap();
    let note = a_full_note(&mut phone);
    let mut laptop = restored::<D>(&phrase);
    deliver_account(&phone, &mut laptop);

    phone.set_trashed(note, true).unwrap();
    phone.forget_note(note).unwrap();
    deliver_account(&phone, &mut laptop);
    deliver_account(&laptop, &mut phone);

    let (p, l) = (account_log(&phone), account_log(&laptop));
    for _ in 0..3 {
        deliver_account(&phone, &mut laptop);
        deliver_account(&laptop, &mut phone);
    }
    assert_eq!(account_log(&phone), p, "the phone has nothing more to say");
    assert_eq!(account_log(&laptop), l, "nor the laptop");

    let (store, keys) = laptop.into_parts();
    let laptop = NoteCore::<D, _, _>::open(store, keys).unwrap();
    assert_eq!(account_log(&laptop), l, "nor after a relaunch");
}

/// Deleting a note that is already deleted, or that this person never had,
/// writes nothing: no second marker, and no marker for an ID someone made up.
fn deleting_what_is_already_gone_writes_nothing<D: NoteDoc>() {
    let mut phone = in_memory::<D>();
    phone.create_account().unwrap();
    let note = a_full_note(&mut phone);
    phone.forget_note(note).unwrap();
    let logged = account_log(&phone);

    phone.forget_note(note).unwrap();
    assert_eq!(
        account_log(&phone),
        logged,
        "a repeat is not a new deletion"
    );

    let stranger = NoteId::random();
    phone.forget_note(stranger).unwrap();
    assert_eq!(
        account_log(&phone),
        logged,
        "an unknown ID is not a deletion"
    );
    assert!(!deleted_notes_of(phone.account_entries()).contains(&stranger));
}

/// The laptop was offline, and did everything to the note a person can do to
/// it, while the phone deleted it. Whichever way round the two accounts meet —
/// and however often — both end up with the note deleted and nothing of it
/// left: not the restore, not the label, not the reminder, not the edit.
fn offline_changes_lose_to_a_deletion_in_either_order<D: NoteDoc>(laptop_first: bool) {
    let mut phone = in_memory::<D>();
    let phrase = phone.create_account().unwrap();
    let note = a_full_note(&mut phone);
    phone.set_trashed(note, true).unwrap();
    let mut laptop = restored::<D>(&phrase);
    deliver_account(&phone, &mut laptop);
    deliver_note(&phone, &mut laptop, note);

    // Offline, on the laptop.
    laptop.set_trashed(note, false).unwrap();
    laptop.add_item(note, "milk, oat").unwrap();
    laptop.add_tag(note, "urgent").unwrap();
    laptop.set_colour(note, Some(Colour::Red)).unwrap();
    laptop
        .create_reminder(note, 1_900_000_000_000, RepeatRule::Weekly, "UTC".into())
        .unwrap();
    // Meanwhile, on the phone.
    phone.forget_note(note).unwrap();

    if laptop_first {
        deliver_account(&laptop, &mut phone);
        deliver_account(&phone, &mut laptop);
    } else {
        deliver_account(&phone, &mut laptop);
        deliver_account(&laptop, &mut phone);
    }
    // And again, as a reconnect replays.
    deliver_account(&phone, &mut laptop);
    deliver_account(&laptop, &mut phone);

    nothing_left(&mut phone, note);
    nothing_left(&mut laptop, note);
    assert_eq!(
        phone.account(),
        laptop.account(),
        "the two devices hold the same account"
    );
}

/// A device set up after the deletion replays the note's creation, its key and
/// its deletion — from a compacted account, too — and never shows the note or
/// takes its key.
fn a_device_restored_after_a_deletion_never_sees_the_note<D: NoteDoc>() {
    let mut phone = in_memory::<D>();
    let phrase = phone.create_account().unwrap();
    let note = a_full_note(&mut phone);
    let kept = phone.create_note(NoteKind::Checklist).unwrap();
    phone.forget_note(note).unwrap();

    let mut tablet = restored::<D>(&phrase);
    deliver_account(&phone, &mut tablet);
    nothing_left(&mut tablet, note);
    assert!(tablet.keyring().note_key(kept).unwrap().is_some());

    phone.compact_account().unwrap();
    assert!(phone.is_deleted_note(note), "the snapshot keeps the marker");
    let mut watch = restored::<D>(&phrase);
    deliver_account(&phone, &mut watch);
    nothing_left(&mut watch, note);
    assert!(watch.keyring().note_key(kept).unwrap().is_some());
}

/// A relaunch keeps the marker, and a note that somehow came back into the
/// store is removed again at open rather than shown.
fn a_relaunch_keeps_the_marker_and_finishes_the_job<D: NoteDoc>() {
    let dir = tempfile::tempdir().unwrap();
    let (note, envelopes) = {
        let mut core = on_disk::<D>(dir.path());
        core.create_account().unwrap();
        let note = a_full_note(&mut core);
        let envelopes = core.store().load(note).unwrap();
        core.forget_note(note).unwrap();
        (note, envelopes)
    };
    {
        // A backup restored under it, or a store write the process never
        // finished removing.
        let master: MasterKey = master_key_from_file(dir.path().join("master.key")).unwrap();
        let mut store = SqliteStore::open(dir.path().join("notes.db"), master).unwrap();
        for raw in &envelopes {
            store.append(note, raw).unwrap();
        }
    }
    let mut core = on_disk::<D>(dir.path());
    nothing_left(&mut core, note);
}

// ---------------------------------------------------------------------------
// What arrives late
// ---------------------------------------------------------------------------

/// After the laptop learns of the deletion, nothing that arrives for the note
/// recreates any of it: not an envelope from the phone's old log, not a sync
/// cursor, not a share blob, not an invite answer, not the key.
fn nothing_late_brings_a_deleted_note_back<D: NoteDoc>() {
    let mut phone = in_memory::<D>();
    let phrase = phone.create_account().unwrap();
    let note = a_full_note(&mut phone);
    let old = phone.store().load(note).unwrap();
    let key = phone.keyring().note_key(note).unwrap().unwrap();
    let mut laptop = restored::<D>(&phrase);
    deliver_account(&phone, &mut laptop);

    phone.forget_note(note).unwrap();
    deliver_account(&phone, &mut laptop);

    for raw in &old {
        assert!(laptop.apply_remote(note, raw).is_err());
    }
    laptop.set_sync_cursor(note, Default::default()).unwrap();
    let blob = easynotes_core::crypto::export_share_blob(&note, &key);
    assert!(laptop.import_share_blob(&blob).is_err());
    let owner = phone.device_key();
    assert!(matches!(
        laptop.accept_answer(easynotes_core::ids::InviteId::random(), note, owner),
        Err(CoreError::MalformedInvite(_))
    ));
    laptop.adopt_note_keys().unwrap();

    nothing_left(&mut laptop, note);
}

// ---------------------------------------------------------------------------
// The trash, and the people it does not reach
// ---------------------------------------------------------------------------

/// Emptying the trash deletes what is in it and nothing else, on both devices.
fn emptying_the_trash_deletes_only_the_trash<D: NoteDoc>() {
    let mut phone = in_memory::<D>();
    let phrase = phone.create_account().unwrap();
    let thrown: Vec<_> = (0..3).map(|_| a_full_note(&mut phone)).collect();
    let kept = a_full_note(&mut phone);
    for id in &thrown {
        phone.set_trashed(*id, true).unwrap();
    }
    let mut laptop = restored::<D>(&phrase);
    deliver_account(&phone, &mut laptop);
    for id in thrown.iter().chain([&kept]) {
        deliver_note(&phone, &mut laptop, *id);
    }

    assert_eq!(phone.empty_trash().unwrap(), 3);
    deliver_account(&phone, &mut laptop);

    for core in [&mut phone, &mut laptop] {
        for id in &thrown {
            nothing_left(core, *id);
        }
        assert!(core.notes().unwrap().iter().any(|n| n.id == kept));
        assert_eq!(core.reminders_on(kept).len(), 1);
        assert!(!core.is_deleted_note(kept));
    }
    assert_eq!(phone.empty_trash().unwrap(), 0, "and it is empty");
}

/// Somebody the note was shared with is another account. Their copy is theirs:
/// the owner's deletion writes nothing on the note's own topic, and the guest
/// keeps reading and editing it.
fn a_collaborator_keeps_their_copy<D: NoteDoc>() {
    let mut owner = in_memory::<D>();
    owner.create_account().unwrap();
    let note = a_full_note(&mut owner);
    let mut guest = in_memory::<D>();
    guest.create_account().unwrap();
    guest
        .import_share_blob(&owner.export_share_blob(note).unwrap())
        .unwrap();
    deliver_note(&owner, &mut guest, note);
    let logged = owner.store().load(note).unwrap();

    owner.forget_note(note).unwrap();

    assert!(!guest.is_deleted_note(note));
    assert!(guest.open_note(note).is_ok());
    guest.add_item(note, "bread").unwrap();
    assert_eq!(guest.open_note(note).unwrap().items.len(), 2);
    assert!(
        guest.store().load(note).unwrap().len() > logged.len(),
        "the guest's log goes on"
    );
}

/// Taking a note off a device because access to it ended is not deleting it:
/// a later invite must still work, so no marker may be written.
fn purging_a_local_copy_is_not_a_deletion<D: NoteDoc>() {
    let mut core = in_memory::<D>();
    core.create_account().unwrap();
    let note = a_full_note(&mut core);
    let logged = account_log(&core);
    core.purge_note_local(note).unwrap();
    assert!(!core.is_deleted_note(note));
    assert_eq!(account_log(&core), logged);
}

// ---------------------------------------------------------------------------
// Crashes
// ---------------------------------------------------------------------------

/// Counts writes down and fails the one that reaches zero, as a process killed
/// mid-operation would: everything before it is kept, nothing after.
#[derive(Clone, Default)]
struct Crash(Rc<Cell<Option<usize>>>);

impl Crash {
    fn write(&self) -> Result<()> {
        match self.0.get() {
            Some(0) => Err(CoreError::Storage("crashed".into())),
            Some(n) => {
                self.0.set(Some(n - 1));
                Ok(())
            }
            None => Ok(()),
        }
    }
}

/// A store that crashes on cue, over another: the in-memory one, or a real
/// SQLite file that a relaunch opens again from disk.
#[derive(Clone, Default)]
struct Store<S = MemoryStore> {
    inner: S,
    crash: Crash,
}

impl<S: OpStore> OpStore for Store<S> {
    fn append(&mut self, note: NoteId, envelope: &[u8]) -> Result<()> {
        self.crash.write()?;
        self.inner.append(note, envelope)
    }
    fn load(&self, note: NoteId) -> Result<Vec<Vec<u8>>> {
        self.inner.load(note)
    }
    fn put_snapshot(&mut self, note: NoteId, snapshot: &[u8]) -> Result<()> {
        self.crash.write()?;
        self.inner.put_snapshot(note, snapshot)
    }
    fn get_snapshot(&self, note: NoteId) -> Result<Option<Vec<u8>>> {
        self.inner.get_snapshot(note)
    }
    fn replace_log(&mut self, note: NoteId, envelopes: &[Vec<u8>]) -> Result<()> {
        self.crash.write()?;
        self.inner.replace_log(note, envelopes)
    }
    fn notes(&self) -> Result<Vec<NoteId>> {
        self.inner.notes()
    }
    fn put_meta(&mut self, note: NoteId, key: &str, value: &[u8]) -> Result<()> {
        self.crash.write()?;
        self.inner.put_meta(note, key, value)
    }
    fn get_meta(&self, note: NoteId, key: &str) -> Result<Option<Vec<u8>>> {
        self.inner.get_meta(note, key)
    }
    fn forget(&mut self, note: NoteId) -> Result<()> {
        self.crash.write()?;
        self.inner.forget(note)
    }
}

#[derive(Clone, Default)]
struct Keys<K = MemoryKeyring> {
    inner: K,
    crash: Crash,
}

impl<K: Keyring> Keyring for Keys<K> {
    fn device_seed(&self) -> Result<Option<easynotes_core::crypto::DeviceSeed>> {
        self.inner.device_seed()
    }
    fn put_device_seed(&mut self, seed: &easynotes_core::crypto::DeviceSeed) -> Result<()> {
        self.inner.put_device_seed(seed)
    }
    fn account_root(&self) -> Result<Option<easynotes_core::account::AccountRoot>> {
        self.inner.account_root()
    }
    fn put_account_root(&mut self, root: &easynotes_core::account::AccountRoot) -> Result<()> {
        self.inner.put_account_root(root)
    }
    fn note_key(&self, note: NoteId) -> Result<Option<NoteKey>> {
        self.inner.note_key(note)
    }
    fn put_note_key(&mut self, note: NoteId, key: &NoteKey) -> Result<()> {
        self.crash.write()?;
        self.inner.put_note_key(note, key)
    }
    fn forget_note_key(&mut self, note: NoteId) -> Result<()> {
        self.crash.write()?;
        self.inner.forget_note_key(note)
    }
}

/// Kill the process at every write a deletion makes, and launch again. Either
/// the deletion was never accepted — the note is all there and not marked — or
/// it was, and the launch finishes it. Never a note that is half gone, and
/// never one that comes back.
fn a_deletion_interrupted_anywhere_either_never_happened_or_finishes<D: NoteDoc>() {
    for writes in 0.. {
        let crash = Crash::default();
        let store = Store {
            inner: MemoryStore::new(),
            crash: crash.clone(),
        };
        let keys = Keys {
            inner: MemoryKeyring::new(),
            crash: crash.clone(),
        };
        let mut core = NoteCore::<D, _, _>::open(store, keys).unwrap();
        core.create_account().unwrap();
        let note = a_full_note(&mut core);

        crash.0.set(Some(writes));
        let result = core.forget_note(note);
        crash.0.set(None);
        let (store, keys) = core.into_parts();
        let mut core = NoteCore::<D, _, _>::open(store, keys).unwrap();

        if core.is_deleted_note(note) {
            nothing_left(&mut core, note);
        } else {
            assert!(result.is_err(), "a deletion that returned Ok is marked");
            let view = core.open_note(note).unwrap();
            assert_eq!(
                view.items.len(),
                1,
                "an unaccepted deletion kept everything"
            );
            assert_eq!(core.reminders_on(note).len(), 1);
        }
        if result.is_ok() {
            break;
        }
        assert!(writes < 50, "a deletion should not need fifty writes");
    }
}

/// Emptying a trash of three on a real SQLite store, killed at every write the
/// batch makes, and launched again from the files on disk. Each note is its
/// own durable marker, so a crash partway leaves the notes already marked
/// gone for good and the rest exactly as they were: still whole, still in the
/// trash, and deleted by emptying it again. The note that was never in the
/// trash is never touched, and the other device ends where this one does.
fn emptying_the_trash_interrupted_anywhere_loses_nothing<D: NoteDoc>() {
    for writes in 0.. {
        let dir = tempfile::tempdir().unwrap();
        let master: MasterKey = master_key_from_file(dir.path().join("master.key")).unwrap();
        let crash = Crash::default();
        let store = Store {
            inner: SqliteStore::open(dir.path().join("notes.db"), master.clone()).unwrap(),
            crash: crash.clone(),
        };
        let keys = Keys {
            inner: SqliteKeyring::open(dir.path().join("keys.db"), master).unwrap(),
            crash: crash.clone(),
        };
        let mut core = NoteCore::<D, _, _>::open(store, keys).unwrap();
        let phrase = core.create_account().unwrap();
        let thrown: Vec<_> = (0..3).map(|_| a_full_note(&mut core)).collect();
        let kept = a_full_note(&mut core);
        for id in &thrown {
            core.set_trashed(*id, true).unwrap();
        }
        let mut laptop = restored::<D>(&phrase);
        deliver_account(&core, &mut laptop);

        crash.0.set(Some(writes));
        let result = core.empty_trash();
        drop(core);

        let mut core = on_disk::<D>(dir.path());
        let marked: Vec<_> = thrown
            .iter()
            .copied()
            .filter(|id| core.is_deleted_note(*id))
            .collect();
        if result.is_ok() {
            assert_eq!(marked, thrown, "an emptied trash deleted all three");
        }
        for id in &thrown {
            if marked.contains(id) {
                nothing_left(&mut core, *id);
            } else {
                assert!(result.is_err(), "writes={writes}");
                let view = core.open_note(*id).unwrap();
                assert_eq!(view.items.len(), 1, "writes={writes}: kept whole");
                assert!(core.placement(*id).trashed, "and still in the trash");
                assert_eq!(core.reminders_on(*id).len(), 1);
            }
        }

        // Emptying it again finishes the job, and deletes what was left.
        assert_eq!(
            core.empty_trash().unwrap(),
            thrown.len() - marked.len(),
            "writes={writes}"
        );
        deliver_account(&core, &mut laptop);
        for id in &thrown {
            nothing_left(&mut core, *id);
            nothing_left(&mut laptop, *id);
        }
        for placement in [core.placement(kept), laptop.placement(kept)] {
            assert!(!placement.trashed, "the kept note never went in");
        }
        assert_eq!(core.open_note(kept).unwrap().items.len(), 1);
        assert_eq!(core.reminders_on(kept).len(), 1);
        assert!(!core.is_deleted_note(kept) && !laptop.is_deleted_note(kept));
        assert_eq!(laptop.reminders_on(kept).len(), 1);

        if result.is_ok() {
            break;
        }
        assert!(writes < 50, "three deletions should not need fifty writes");
    }
}

macro_rules! backend_suite {
    ($name:ident, $backend:ty) => {
        mod $name {
            #[test]
            fn a_deletion_reaches_the_other_device_and_takes_everything() {
                super::a_deletion_reaches_the_other_device_and_takes_everything::<$backend>()
            }
            #[test]
            fn devices_that_agree_about_a_deletion_stop_writing() {
                super::devices_that_agree_about_a_deletion_stop_writing::<$backend>()
            }
            #[test]
            fn deleting_what_is_already_gone_writes_nothing() {
                super::deleting_what_is_already_gone_writes_nothing::<$backend>()
            }
            #[test]
            fn offline_changes_lose_to_a_deletion_that_arrives_first() {
                super::offline_changes_lose_to_a_deletion_in_either_order::<$backend>(false)
            }
            #[test]
            fn offline_changes_lose_to_a_deletion_that_arrives_second() {
                super::offline_changes_lose_to_a_deletion_in_either_order::<$backend>(true)
            }
            #[test]
            fn a_device_restored_after_a_deletion_never_sees_the_note() {
                super::a_device_restored_after_a_deletion_never_sees_the_note::<$backend>()
            }
            #[test]
            fn a_relaunch_keeps_the_marker_and_finishes_the_job() {
                super::a_relaunch_keeps_the_marker_and_finishes_the_job::<$backend>()
            }
            #[test]
            fn nothing_late_brings_a_deleted_note_back() {
                super::nothing_late_brings_a_deleted_note_back::<$backend>()
            }
            #[test]
            fn emptying_the_trash_deletes_only_the_trash() {
                super::emptying_the_trash_deletes_only_the_trash::<$backend>()
            }
            #[test]
            fn a_collaborator_keeps_their_copy() {
                super::a_collaborator_keeps_their_copy::<$backend>()
            }
            #[test]
            fn purging_a_local_copy_is_not_a_deletion() {
                super::purging_a_local_copy_is_not_a_deletion::<$backend>()
            }
            #[test]
            fn a_deletion_interrupted_anywhere_either_never_happened_or_finishes() {
                super::a_deletion_interrupted_anywhere_either_never_happened_or_finishes::<
                    $backend,
                >()
            }
            #[test]
            fn emptying_the_trash_interrupted_anywhere_loses_nothing() {
                super::emptying_the_trash_interrupted_anywhere_loses_nothing::<$backend>()
            }
        }
    };
}

backend_suite!(yrs, easynotes_core::crdt::YrsNote);
backend_suite!(automerge, easynotes_core::crdt::AutomergeNote);
