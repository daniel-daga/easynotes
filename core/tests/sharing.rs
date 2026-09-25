//! Sharing with people, not devices: access packages end to end, in memory.
//!
//! `Relay` stands in for the real one — an append-only log per note that
//! hands every device everything after its cursor — so that each scenario in
//! the plan (`docs/plan-account-collaboration.md`, "Scenarios that must pass")
//! can be driven envelope by envelope, in whatever order it needs.
//! `relay/tests/sharing.rs` runs the flow through a real relay and socket.
//! Every scenario runs on both CRDT backends: the switch rule is backend code.

use easynotes_core::access::{self, Package};
use easynotes_core::account::RecoveryPhrase;
use easynotes_core::crdt::{AutomergeNote, NoteDoc, YrsNote};
use easynotes_core::crypto::{Envelope, EnvelopeKind, NoteKey};
use easynotes_core::error::{CoreError, Result};
use easynotes_core::ids::{InviteId, NoteId};
use easynotes_core::invite::{self, InviteSecret};
use easynotes_core::note::{Approved, NoteCore, NoteKind, Outgoing};
use easynotes_core::store::{Keyring, MemoryKeyring, MemoryStore, OpStore};
use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

/// An append-only log per document, and nothing else.
#[derive(Default)]
struct Relay {
    logs: HashMap<NoteId, Vec<Vec<u8>>>,
}

impl Relay {
    fn append(&mut self, note: NoteId, envelope: &[u8]) {
        let log = self.logs.entry(note).or_default();
        // The real relay stores duplicates and receivers dedupe them. Storing
        // each once keeps these cursors simple and changes nothing a device
        // can learn.
        if !log.iter().any(|e| e == envelope) {
            log.push(envelope.to_vec());
        }
    }

    fn log(&self, note: NoteId) -> Vec<Vec<u8>> {
        self.logs.get(&note).cloned().unwrap_or_default()
    }
}

// ---------------------------------------------------------------------------
// A store that can be made to fail, for the crash scenarios
// ---------------------------------------------------------------------------

/// Counts writes down and fails the one that reaches zero, as a process
/// killed mid-operation would: everything before it is kept, nothing after.
#[derive(Clone, Default)]
struct Crash {
    left: Rc<Cell<Option<usize>>>,
    fired: Rc<Cell<bool>>,
}

impl Crash {
    fn after(&self, writes: usize) {
        self.left.set(Some(writes));
        self.fired.set(false);
    }

    fn never(&self) {
        self.left.set(None);
    }

    fn write(&self) -> Result<()> {
        match self.left.get() {
            Some(0) => {
                self.fired.set(true);
                Err(CoreError::Storage("crashed".into()))
            }
            Some(n) => {
                self.left.set(Some(n - 1));
                Ok(())
            }
            None => Ok(()),
        }
    }

    fn crashed(&self) -> bool {
        self.fired.get()
    }
}

#[derive(Clone, Default)]
struct Store {
    inner: MemoryStore,
    crash: Crash,
}

impl OpStore for Store {
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
struct Keys {
    inner: MemoryKeyring,
    crash: Crash,
}

impl Keyring for Keys {
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

type Core<D> = NoteCore<D, Store, Keys>;

// ---------------------------------------------------------------------------
// Phones
// ---------------------------------------------------------------------------

/// One phone: a core, and how far it has read each log.
struct Phone<D: NoteDoc> {
    core: Option<Core<D>>,
    crash: Crash,
    cursors: HashMap<NoteId, usize>,
    phrase: Option<RecoveryPhrase>,
}

impl<D: NoteDoc> Phone<D> {
    fn new() -> Self {
        let crash = Crash::default();
        let store = Store {
            inner: MemoryStore::new(),
            crash: crash.clone(),
        };
        let keys = Keys {
            inner: MemoryKeyring::new(),
            crash: crash.clone(),
        };
        Self {
            core: Some(NoteCore::open(store, keys).unwrap()),
            crash,
            cursors: HashMap::new(),
            phrase: None,
        }
    }

    fn core(&mut self) -> &mut Core<D> {
        self.core.as_mut().unwrap()
    }

    fn peek(&self) -> &Core<D> {
        self.core.as_ref().unwrap()
    }

    /// A person: a phone with an account and a name.
    fn person(name: &str) -> Self {
        let mut phone = Self::new();
        let phrase = phone.core().create_account().unwrap();
        phone.core().set_profile_name(name).unwrap();
        phone.phrase = Some(phrase);
        phone
    }

    /// Another phone of the same person, restored from the words.
    fn restored(&self) -> Self {
        let mut phone = Self::new();
        let words = self.phrase.as_ref().unwrap().words();
        let phrase = RecoveryPhrase::parse(&words).unwrap();
        phone.core().adopt_account(&phrase).unwrap();
        phone.phrase = Some(phrase);
        phone
    }

    /// The app killed and launched again: the same store and keyring, and a
    /// core opened from them. The relay cursors are the store's, so they
    /// stay where the last saved one was — here, where reading got to.
    fn relaunch(&mut self) {
        self.crash.never();
        let (store, keys) = self.core.take().unwrap().into_parts();
        self.core = Some(NoteCore::open(store, keys).unwrap());
    }

    /// Everything this phone has to send for a document, onto the relay.
    fn push(&mut self, relay: &mut Relay, note: NoteId) {
        if Some(note) == self.peek().account_id() {
            for raw in self.peek().store().load(note).unwrap() {
                relay.append(note, &raw);
            }
            return;
        }
        for (_, raw) in self.peek().unsent(note, None).unwrap() {
            relay.append(note, &raw);
        }
    }

    /// The account document, both ways, and settled.
    fn sync_account(&mut self, relay: &mut Relay) {
        let id = self.peek().account_id().unwrap();
        self.push(relay, id);
        let log = relay.log(id);
        let from = self.cursors.get(&id).copied().unwrap_or(0);
        for raw in &log[from..] {
            self.core().apply_remote_account(raw).unwrap();
        }
        self.cursors.insert(id, log.len());
        self.core().adopt_note_keys().unwrap();
    }

    /// Read everything new on a note, then settle it the way the engine does
    /// on `Caught`, and send what settling says to.
    fn pull(&mut self, relay: &mut Relay, note: NoteId) -> Result<Outgoing> {
        let log = relay.log(note);
        // Where the device's own saved cursor says, as the engine resubscribes:
        // a note deleted and joined again starts from the beginning.
        let mut cursor = self.peek().sync_cursor(note)?;
        let from = cursor.last_seq as usize;
        for raw in log.iter().skip(from) {
            match self.core().apply_remote(note, raw) {
                Ok(_) | Err(CoreError::AccessPending) | Err(CoreError::UnknownNote(_)) => {}
                Err(e @ CoreError::Storage(_)) => return Err(e),
                Err(e) => panic!("applying an envelope: {e}"),
            }
        }
        cursor.last_seq = log.len() as i64;
        if self.core().can_open(note) || self.peek().syncable()?.contains(&note) {
            self.core().set_sync_cursor(note, cursor)?;
        }
        let out = self.core().settle_note(note)?;
        for raw in &out.envelopes {
            relay.append(note, raw);
        }
        Ok(out)
    }

    /// Both ways, twice: once to send and read, and once more so that what
    /// this phone sent comes back as the relay's echo.
    fn try_sync(&mut self, relay: &mut Relay, note: NoteId) -> Result<Outgoing> {
        self.push(relay, note);
        let first = self.pull(relay, note)?;
        let second = self.pull(relay, note)?;
        Ok(Outgoing {
            rekey: first.rekey || second.rekey,
            removed: first.removed || second.removed,
            envelopes: first
                .envelopes
                .into_iter()
                .chain(second.envelopes)
                .collect(),
        })
    }

    fn sync(&mut self, relay: &mut Relay, note: NoteId) -> Outgoing {
        self.try_sync(relay, note).unwrap()
    }

    fn title(&mut self, note: NoteId) -> Option<String> {
        self.core().open_note(note).ok().map(|view| view.title)
    }

    fn names(&self, note: NoteId) -> Vec<String> {
        self.peek()
            .people(note)
            .unwrap()
            .people
            .into_iter()
            .map(|p| p.name)
            .collect()
    }

    fn member(&self) -> String {
        self.peek().member().unwrap().id().encode()
    }

    fn key_id(&self, note: NoteId) -> Option<[u8; 8]> {
        self.peek()
            .keyring()
            .note_key(note)
            .unwrap()
            .map(|k| k.id())
    }
}

/// The owner opens an invite and the guest asks on it. Returns the invite and
/// the secret the guest holds.
fn ask<D: NoteDoc>(
    owner: &mut Phone<D>,
    guest: &mut Phone<D>,
    note: NoteId,
) -> (InviteId, InviteSecret, invite::JoinRequest) {
    let outgoing = owner.core().new_invite(note).unwrap();
    owner.core().remember_invite(&outgoing).unwrap();
    let secret = InviteSecret::from_link(&outgoing.secret.link()).unwrap();
    let sealed = guest.core().seal_join_request(&secret).unwrap();
    let request = invite::open_request(&outgoing.secret, &sealed).unwrap();
    (outgoing.secret.id(), secret, request)
}

/// The guest opens the answer and follows the note.
fn accept<D: NoteDoc>(
    relay: &mut Relay,
    guest: &mut Phone<D>,
    secret: &InviteSecret,
    answer: &[u8],
) -> NoteId {
    let member = guest.peek().member().unwrap();
    let (note, owner_key) = invite::open_answer(secret, &member, answer).unwrap();
    guest
        .core()
        .accept_answer(secret.id(), note, owner_key)
        .unwrap();
    guest.sync_account(relay);
    guest.sync(relay, note);
    note
}

/// The whole invite: the guest asks, the owner approves, the package goes out
/// and comes back, the answer is sealed and opened, and the guest follows.
fn let_in<D: NoteDoc>(
    relay: &mut Relay,
    owner: &mut Phone<D>,
    guest: &mut Phone<D>,
    note: NoteId,
) -> InviteId {
    let (invite, secret, request) = ask(owner, guest, note);
    assert_eq!(
        owner.core().approve(invite, "req", &request).unwrap(),
        Approved::Adding
    );
    owner.sync(relay, note);
    let answer = owner
        .peek()
        .approval_answer(invite)
        .unwrap()
        .expect("the package is on the relay");
    owner.core().forget_invite(invite).unwrap();
    accept(relay, guest, &secret, &answer);
    invite
}

fn set_title<D: NoteDoc>(phone: &mut Phone<D>, relay: &mut Relay, note: NoteId, title: &str) {
    phone.core().set_title(note, title).unwrap();
    phone.sync(relay, note);
}

fn remove<D: NoteDoc>(owner: &mut Phone<D>, note: NoteId, who: &str) {
    let people = owner.peek().people(note).unwrap();
    let person = people
        .people
        .iter()
        .find(|p| p.member == who)
        .expect("on the list");
    owner
        .core()
        .remove_person(note, &person.member, &person.invite)
        .unwrap();
}

/// A shared note: an owner who has set up sharing, and a titled note.
fn shared<D: NoteDoc>(relay: &mut Relay, title: &str) -> (Phone<D>, NoteId) {
    let mut owner = Phone::<D>::person("Olga");
    owner.sync_account(relay);
    let note = owner.core().create_note(NoteKind::Checklist).unwrap();
    owner.sync_account(relay);
    set_title(&mut owner, relay, note, title);
    (owner, note)
}

macro_rules! both_backends {
    ($($name:ident),* $(,)?) => {
        mod yrs {
            $(#[test] fn $name() { super::$name::<super::YrsNote>(); })*
        }
        mod automerge {
            $(#[test] fn $name() { super::$name::<super::AutomergeNote>(); })*
        }
    };
}

both_backends!(
    a_person_is_let_in_and_reads_the_note,
    removing_anna_reaches_both_her_phones_and_ben_keeps_the_note,
    a_new_phone_recovers_shared_notes_with_the_original_gone,
    two_owner_devices_removing_at_once_converge,
    an_offline_removal_holds_back_and_completes,
    offline_typing_survives_a_removal_and_removed_typing_does_not,
    an_approval_racing_a_removal_then_a_reinvite,
    forgeries_remove_nobody_and_switch_nobody,
    a_crash_at_every_step_of_remove_resumes,
    a_crash_at_every_step_of_approve_resumes,
    a_crash_at_every_step_of_a_switch_resumes,
    people_are_accounts_not_names_or_phones,
    every_arrival_order_picks_the_same_current_package,
    stop_sharing_removes_everyone_but_the_owner,
    an_offline_rotation_loses_to_a_deletion_that_syncs_first,
    an_offline_rotation_loses_to_a_deletion_that_syncs_second,
);

// ---------------------------------------------------------------------------

fn a_person_is_let_in_and_reads_the_note<D: NoteDoc>() {
    let mut relay = Relay::default();
    let (mut owner, note) = shared::<D>(&mut relay, "Groceries");
    let mut anna = Phone::<D>::person("Anna");

    let_in(&mut relay, &mut owner, &mut anna, note);

    assert_eq!(anna.title(note).as_deref(), Some("Groceries"));
    set_title(&mut anna, &mut relay, note, "Groceries and wine");
    owner.sync(&mut relay, note);
    assert_eq!(owner.title(note).as_deref(), Some("Groceries and wine"));
    assert_eq!(owner.names(note), ["Olga", "Anna"]);
    assert_eq!(anna.names(note), ["Olga", "Anna"]);
    // Search offers the other person, not oneself.
    let people = anna.peek().collaborators(note).unwrap();
    assert_eq!(people.len(), 1);
    assert_eq!(people[0].name, "Olga");
    // Only the owner manages.
    assert!(owner.peek().people(note).unwrap().can_manage);
    assert!(!anna.peek().people(note).unwrap().can_manage);
}

/// Scenario 1.
fn removing_anna_reaches_both_her_phones_and_ben_keeps_the_note<D: NoteDoc>() {
    let mut relay = Relay::default();
    let (mut owner, note) = shared::<D>(&mut relay, "Groceries");
    let mut anna = Phone::<D>::person("Anna");
    let mut ben = Phone::<D>::person("Ben");
    let_in(&mut relay, &mut owner, &mut anna, note);
    let_in(&mut relay, &mut owner, &mut ben, note);

    // Anna's second phone finds the note through her account, with nobody
    // asked.
    let mut anna2 = anna.restored();
    anna2.sync_account(&mut relay);
    assert!(anna2.peek().syncable().unwrap().contains(&note));
    anna2.sync(&mut relay, note);
    assert_eq!(anna2.title(note).as_deref(), Some("Groceries"));
    anna.sync(&mut relay, note);

    // Ben goes offline. Anna is removed, and the owner writes something new.
    let anna_member = anna.member();
    remove(&mut owner, note, &anna_member);
    assert!(owner.sync(&mut relay, note).rekey);
    set_title(&mut owner, &mut relay, note, "Groceries, privately");
    assert_eq!(owner.names(note), ["Olga", "Ben"]);

    for phone in [&mut anna, &mut anna2] {
        assert!(phone.sync(&mut relay, note).removed);
        assert!(phone.title(note).is_none(), "the note left the phone");
        let removals = phone.core().take_removals();
        assert_eq!(removals, [(note, "Groceries".to_string())]);
        phone.sync_account(&mut relay);
        assert!(!phone.peek().syncable().unwrap().contains(&note));
    }

    // A phone Anna restores later does not even fetch it.
    let mut anna3 = anna.restored();
    anna3.sync_account(&mut relay);
    assert!(!anna3.peek().syncable().unwrap().contains(&note));
    // And would read nothing if it did.
    assert!(anna3
        .core()
        .apply_remote(note, &relay.log(note)[0])
        .is_err());

    // The owner goes away. Ben catches up with no invite and reads what was
    // written after Anna left.
    ben.sync(&mut relay, note);
    assert_eq!(ben.title(note).as_deref(), Some("Groceries, privately"));
    assert_eq!(ben.key_id(note), owner.key_id(note));
}

/// Scenario 2.
fn a_new_phone_recovers_shared_notes_with_the_original_gone<D: NoteDoc>() {
    let mut relay = Relay::default();
    let (mut owner, note) = shared::<D>(&mut relay, "Holiday");
    let mut anna = Phone::<D>::person("Anna");
    let mut ben = Phone::<D>::person("Ben");
    let mut cara = Phone::<D>::person("Cara");
    let_in(&mut relay, &mut owner, &mut anna, note);
    let_in(&mut relay, &mut owner, &mut ben, note);
    let ben_member = ben.member();
    remove(&mut owner, note, &ben_member);
    owner.sync(&mut relay, note);
    let_in(&mut relay, &mut owner, &mut cara, note);
    set_title(&mut cara, &mut relay, note, "Holiday plans");
    for phone in [&mut owner, &mut anna] {
        phone.sync(&mut relay, note);
        phone.core().compact(note).unwrap();
        phone.relaunch();
        phone.sync(&mut relay, note);
    }

    // Anna's phone is lost; a new one from her words has the note.
    let mut anna_new = anna.restored();
    drop(anna);
    anna_new.sync_account(&mut relay);
    anna_new.sync(&mut relay, note);
    assert_eq!(anna_new.title(note).as_deref(), Some("Holiday plans"));

    // So is the owner's; the new one can still manage the note.
    let mut owner_new = owner.restored();
    drop(owner);
    owner_new.sync_account(&mut relay);
    owner_new.sync(&mut relay, note);
    assert_eq!(owner_new.title(note).as_deref(), Some("Holiday plans"));
    assert_eq!(owner_new.names(note), ["Olga", "Anna", "Cara"]);
    let cara_member = cara.member();
    remove(&mut owner_new, note, &cara_member);
    owner_new.sync(&mut relay, note);
    assert!(cara.sync(&mut relay, note).removed);
    anna_new.sync(&mut relay, note);
    set_title(&mut anna_new, &mut relay, note, "Holiday, two of us");
    owner_new.sync(&mut relay, note);
    assert_eq!(owner_new.title(note).as_deref(), Some("Holiday, two of us"));
}

/// Scenario 3.
fn two_owner_devices_removing_at_once_converge<D: NoteDoc>() {
    for laptop_first in [false, true] {
        let mut relay = Relay::default();
        let (mut phone, note) = shared::<D>(&mut relay, "Chores");
        let mut laptop = phone.restored();
        laptop.sync_account(&mut relay);
        laptop.sync(&mut relay, note);
        let mut anna = Phone::<D>::person("Anna");
        let mut ben = Phone::<D>::person("Ben");
        let mut cara = Phone::<D>::person("Cara");
        let_in(&mut relay, &mut phone, &mut anna, note);
        let_in(&mut relay, &mut phone, &mut ben, note);
        let_in(&mut relay, &mut phone, &mut cara, note);
        laptop.sync(&mut relay, note);
        anna.sync(&mut relay, note);
        ben.sync(&mut relay, note);

        // The same moment: each builds on what it has, before seeing the other.
        let (anna_member, ben_member) = (anna.member(), ben.member());
        remove(&mut phone, note, &anna_member);
        remove(&mut laptop, note, &ben_member);
        let a = phone.core().settle_note(note).unwrap();
        let b = laptop.core().settle_note(note).unwrap();
        let (first, second) = if laptop_first { (&b, &a) } else { (&a, &b) };
        for raw in first.envelopes.iter().chain(&second.envelopes) {
            relay.append(note, raw);
        }

        // Everyone syncs until nobody has anything more to say.
        for _ in 0..4 {
            phone.sync(&mut relay, note);
            laptop.sync(&mut relay, note);
            cara.sync(&mut relay, note);
        }
        assert!(anna.sync(&mut relay, note).removed);
        assert!(ben.sync(&mut relay, note).removed);
        assert_eq!(phone.names(note), ["Olga", "Cara"]);
        assert_eq!(laptop.names(note), ["Olga", "Cara"]);
        assert_eq!(cara.names(note), ["Olga", "Cara"]);
        let current = phone.peek().current_package(note).unwrap();
        assert_eq!(laptop.peek().current_package(note).unwrap(), current);
        assert_eq!(cara.peek().current_package(note).unwrap(), current);
        assert_eq!(phone.key_id(note), cara.key_id(note));
        set_title(&mut cara, &mut relay, note, "Chores for two");
        phone.sync(&mut relay, note);
        laptop.sync(&mut relay, note);
        assert_eq!(phone.title(note).as_deref(), Some("Chores for two"));
        assert_eq!(laptop.title(note).as_deref(), Some("Chores for two"));
        // Once they agree, nothing more is published.
        let before = relay.log(note).len();
        phone.sync(&mut relay, note);
        laptop.sync(&mut relay, note);
        assert_eq!(relay.log(note).len(), before);
    }
}

/// Scenario 4.
fn an_offline_removal_holds_back_and_completes<D: NoteDoc>() {
    let mut relay = Relay::default();
    let (mut owner, note) = shared::<D>(&mut relay, "Presents");
    let mut anna = Phone::<D>::person("Anna");
    let mut ben = Phone::<D>::person("Ben");
    let_in(&mut relay, &mut owner, &mut anna, note);
    let_in(&mut relay, &mut owner, &mut ben, note);
    let old_key = owner.key_id(note).unwrap();

    // Offline: nothing is synced from here until the owner reconnects.
    let anna_member = anna.member();
    remove(&mut owner, note, &anna_member);
    let people = owner.peek().people(note).unwrap();
    let row = people
        .people
        .iter()
        .find(|p| p.member == anna_member)
        .unwrap();
    assert!(row.removing, "the row says Removing…");
    owner.core().set_title(note, "A bike for Anna").unwrap();
    assert!(
        owner
            .peek()
            .unsent(note, None)
            .unwrap()
            .iter()
            .all(|(_, raw)| Envelope::decode(raw).unwrap().kind == EnvelopeKind::Access),
        "no update is sent under the old key while the removal waits"
    );

    // Online again.
    owner.sync(&mut relay, note);
    assert!(owner
        .peek()
        .people(note)
        .unwrap()
        .people
        .iter()
        .all(|p| p.member != anna_member));
    for raw in relay.log(note) {
        let env = Envelope::decode(&raw).unwrap();
        if env.key_id == Some(old_key) && env.kind == EnvelopeKind::Update {
            let plain = env.open(&NoteKey([0u8; 32])).err();
            assert!(plain.is_some());
            // Anna's copy of the old key opens every old-key update; none of
            // them holds what was typed during the removal.
            anna.core().open_note(note).unwrap();
        }
    }
    anna.sync(&mut relay, note);
    assert!(anna.title(note).is_none());
    ben.sync(&mut relay, note);
    assert_eq!(ben.title(note).as_deref(), Some("A bike for Anna"));
    assert_ne!(ben.key_id(note), Some(old_key));
}

/// Scenario 5.
fn offline_typing_survives_a_removal_and_removed_typing_does_not<D: NoteDoc>() {
    let mut relay = Relay::default();
    let (mut owner, note) = shared::<D>(&mut relay, "Menu");
    let mut anna = Phone::<D>::person("Anna");
    let mut ben = Phone::<D>::person("Ben");
    let_in(&mut relay, &mut owner, &mut anna, note);
    let_in(&mut relay, &mut owner, &mut ben, note);

    // Ben types offline, under the key Anna still holds.
    let (item, _) = ben.core().add_item(note, "Ben's lasagne").unwrap();
    let anna_member = anna.member();
    remove(&mut owner, note, &anna_member);
    owner.sync(&mut relay, note);

    // Ben reconnects: his typing survives, re-sent under the new key.
    let out = ben.sync(&mut relay, note);
    assert!(out.rekey);
    owner.sync(&mut relay, note);
    let view = owner.core().open_note(note).unwrap();
    assert!(
        view.item(item).is_some(),
        "Ben's offline item reached the owner"
    );

    // Anna, not yet told, types — under the old key. Nobody who switched sees it.
    anna.core().add_item(note, "Anna's late addition").unwrap();
    for (_, raw) in anna.peek().unsent(note, None).unwrap() {
        relay.append(note, &raw);
    }
    owner.sync(&mut relay, note);
    ben.sync(&mut relay, note);
    for phone in [&mut owner, &mut ben] {
        let view = phone.core().open_note(note).unwrap();
        assert!(view.items.iter().all(|i| i.text != "Anna's late addition"));
    }
    assert!(anna.sync(&mut relay, note).removed);
}

/// Scenario 6.
fn an_approval_racing_a_removal_then_a_reinvite<D: NoteDoc>() {
    let mut relay = Relay::default();
    let (mut phone, note) = shared::<D>(&mut relay, "Garden");
    let mut laptop = phone.restored();
    laptop.sync_account(&mut relay);
    let mut anna = Phone::<D>::person("Anna");
    let mut cara = Phone::<D>::person("Cara");
    let first = let_in(&mut relay, &mut phone, &mut anna, note);
    laptop.sync(&mut relay, note);

    // The phone approves Cara while the laptop removes Anna.
    let (invite, secret, request) = ask(&mut phone, &mut cara, note);
    phone.core().approve(invite, "req", &request).unwrap();
    let anna_member = anna.member();
    remove(&mut laptop, note, &anna_member);
    let a = phone.core().settle_note(note).unwrap();
    let b = laptop.core().settle_note(note).unwrap();
    for raw in b.envelopes.iter().chain(&a.envelopes) {
        relay.append(note, raw);
    }
    for _ in 0..4 {
        phone.sync(&mut relay, note);
        laptop.sync(&mut relay, note);
    }
    let answer = phone.peek().approval_answer(invite).unwrap().unwrap();
    accept(&mut relay, &mut cara, &secret, &answer);
    assert_eq!(
        cara.core()
            .open_note(note)
            .map(|v| v.title)
            .map_err(|e| e.to_string()),
        Ok("Garden".to_string())
    );
    assert!(anna.sync(&mut relay, note).removed);
    anna.sync_account(&mut relay);
    assert_eq!(phone.names(note), ["Olga", "Cara"]);

    // Anna is invited again: a new membership, not blocked by the old one.
    let second = let_in(&mut relay, &mut phone, &mut anna, note);
    assert_ne!(first, second);
    assert_eq!(anna.title(note).as_deref(), Some("Garden"));

    // Late arrivals change nothing: the first answer is refused, the whole
    // relay history replayed again is a no-op, and the list stands.
    let current = phone.peek().current_package(note).unwrap();
    let mut late = anna.restored();
    late.sync_account(&mut relay);
    late.sync(&mut relay, note);
    for raw in relay.log(note) {
        let _ = late.core().apply_remote(note, &raw);
        let _ = anna.core().apply_remote(note, &raw);
    }
    late.core().settle_note(note).unwrap();
    anna.core().settle_note(note).unwrap();
    assert_eq!(late.title(note).as_deref(), Some("Garden"));
    assert_eq!(anna.title(note).as_deref(), Some("Garden"));
    assert_eq!(late.peek().current_package(note).unwrap(), current);
    let owner_key = invite::open_answer(&secret, &cara.peek().member().unwrap(), &answer)
        .unwrap()
        .1;
    assert!(anna.core().accept_answer(first, note, owner_key).is_err());
    assert_eq!(anna.title(note).as_deref(), Some("Garden"));
    assert_eq!(phone.names(note), ["Olga", "Anna", "Cara"]);
}

/// Scenario 7.
fn forgeries_remove_nobody_and_switch_nobody<D: NoteDoc>() {
    let mut relay = Relay::default();
    let (mut owner, note) = shared::<D>(&mut relay, "Budget");
    let mut anna = Phone::<D>::person("Anna");
    let mut ben = Phone::<D>::person("Ben");
    let_in(&mut relay, &mut owner, &mut anna, note);
    let_in(&mut relay, &mut owner, &mut ben, note);
    anna.sync(&mut relay, note);
    let key = anna.key_id(note);
    let current = anna.peek().current_package(note).unwrap().unwrap();
    let packages: Vec<Vec<u8>> = relay
        .log(note)
        .into_iter()
        .filter(|raw| Envelope::decode(raw).unwrap().kind == EnvelopeKind::Access)
        .collect();
    let newest = packages.last().unwrap().clone();

    // Signed by somebody other than the owner: Ben, say, with a package that
    // leaves Anna out and grants a key of his own.
    let forger = easynotes_core::crypto::DeviceIdentity::generate();
    let ben_entry = access::Member {
        member: ben.peek().member().unwrap().id(),
        box_key: ben.peek().member().unwrap().box_public(),
        owner: true,
        invite: access::OWNER_INVITE,
        name: "Ben".into(),
    };
    let forged = access::build(
        &forger,
        note,
        3,
        Some(current),
        &NoteKey::random(),
        &access::Body {
            list: vec![ben_entry],
            snapshot: Some(Vec::new()),
            ..access::Body::default()
        },
    )
    .unwrap();
    let mut truncated = newest.clone();
    truncated.truncate(newest.len() - 10);
    let mut flipped = newest.clone();
    let at = 1 + 16 + 32 + 8 + 8 + 1 + 24 + 64 + 100;
    flipped[at] ^= 1;
    // A grant moved from another package into this one.
    let mut moved = newest.clone();
    let slots = 1 + 16 + 32 + 8 + 8 + 1 + 24 + 64 + 41;
    moved[slots..slots + 80].copy_from_slice(&packages[0][slots..slots + 80]);

    for bad in [&forged, &truncated, &flipped, &moved] {
        assert!(anna.core().apply_remote(note, bad).is_err());
    }
    // Replayed and duplicated: accepted and meaningless.
    for _ in 0..2 {
        for raw in &packages {
            let _ = anna.core().apply_remote(note, raw);
        }
    }
    let out = anna.core().settle_note(note).unwrap();
    assert!(!out.removed && !out.rekey);
    assert_eq!(anna.key_id(note), key);
    assert_eq!(anna.peek().current_package(note).unwrap(), Some(current));
    assert_eq!(anna.title(note).as_deref(), Some("Budget"));
}

/// Scenario 8, for Remove: a crash at every write of it, and a relaunch,
/// ends where an uninterrupted Remove does.
fn a_crash_at_every_step_of_remove_resumes<D: NoteDoc>() {
    for writes in 0.. {
        let mut relay = Relay::default();
        let (mut owner, note) = shared::<D>(&mut relay, "Plans");
        let mut anna = Phone::<D>::person("Anna");
        let mut ben = Phone::<D>::person("Ben");
        let_in(&mut relay, &mut owner, &mut anna, note);
        let_in(&mut relay, &mut owner, &mut ben, note);
        owner.core().set_title(note, "Plans, typed before").unwrap();

        owner.crash.after(writes);
        let anna_member = anna.member();
        let attempt = (|| -> Result<()> {
            let people = owner.peek().people(note)?;
            let person = people
                .people
                .iter()
                .find(|p| p.member == anna_member)
                .unwrap();
            let (member, invite) = (person.member.clone(), person.invite.clone());
            owner.core().remove_person(note, &member, &invite)?;
            owner.try_sync(&mut relay, note)?;
            Ok(())
        })();
        let crashed = owner.crash.crashed();
        owner.relaunch();
        if crashed {
            assert!(attempt.is_err());
            // The owner asks again if the tap itself was lost.
            if owner
                .peek()
                .people(note)
                .unwrap()
                .people
                .iter()
                .any(|p| p.member == anna_member && !p.removing)
            {
                remove(&mut owner, note, &anna_member);
            }
        }
        owner.sync(&mut relay, note);
        owner.sync(&mut relay, note);
        assert!(anna.sync(&mut relay, note).removed, "writes={writes}");
        ben.sync(&mut relay, note);
        assert_eq!(
            ben.title(note).as_deref(),
            Some("Plans, typed before"),
            "writes={writes}"
        );
        assert_eq!(owner.title(note).as_deref(), Some("Plans, typed before"));
        assert_eq!(owner.names(note), ["Olga", "Ben"]);
        if !crashed {
            break;
        }
    }
}

/// Scenario 8, for Approve.
fn a_crash_at_every_step_of_approve_resumes<D: NoteDoc>() {
    for writes in 0.. {
        let mut relay = Relay::default();
        let (mut owner, note) = shared::<D>(&mut relay, "Recipes");
        let mut anna = Phone::<D>::person("Anna");
        let (invite, secret, request) = ask(&mut owner, &mut anna, note);

        owner.crash.after(writes);
        let attempt = (|| -> Result<()> {
            owner.core().approve(invite, "req", &request)?;
            owner.try_sync(&mut relay, note)?;
            Ok(())
        })();
        let crashed = owner.crash.crashed();
        owner.relaunch();
        if crashed {
            assert!(attempt.is_err());
            // The app resumes an approval by making it again, from the saved
            // row if the row made it and from the request if it did not.
            owner.core().approve(invite, "req", &request).unwrap();
        }
        owner.sync(&mut relay, note);
        let answer = owner
            .peek()
            .approval_answer(invite)
            .unwrap()
            .unwrap_or_else(|| panic!("ready at writes={writes}"));
        accept(&mut relay, &mut anna, &secret, &answer);
        assert_eq!(
            anna.title(note).as_deref(),
            Some("Recipes"),
            "writes={writes}"
        );
        assert_eq!(owner.names(note), ["Olga", "Anna"]);
        if !crashed {
            break;
        }
    }
}

/// Scenario 8, for a switch: Ben's phone stops at every write of moving to the
/// new key, and still ends with the new key, his offline typing, and the
/// owner's.
fn a_crash_at_every_step_of_a_switch_resumes<D: NoteDoc>() {
    for writes in 0.. {
        let mut relay = Relay::default();
        let (mut owner, note) = shared::<D>(&mut relay, "Trip");
        let mut anna = Phone::<D>::person("Anna");
        let mut ben = Phone::<D>::person("Ben");
        let_in(&mut relay, &mut owner, &mut anna, note);
        let_in(&mut relay, &mut owner, &mut ben, note);
        let (item, _) = ben.core().add_item(note, "Tent").unwrap();
        let anna_member = anna.member();
        remove(&mut owner, note, &anna_member);
        owner.sync(&mut relay, note);
        set_title(&mut owner, &mut relay, note, "Trip, just us");

        ben.crash.after(writes);
        let attempt = ben.try_sync(&mut relay, note);
        let crashed = ben.crash.crashed();
        ben.relaunch();
        if crashed {
            assert!(attempt.is_err());
        }
        ben.sync(&mut relay, note);
        owner.sync(&mut relay, note);
        assert_eq!(ben.key_id(note), owner.key_id(note), "writes={writes}");
        assert_eq!(
            ben.title(note).as_deref(),
            Some("Trip, just us"),
            "writes={writes}"
        );
        assert!(
            owner.core().open_note(note).unwrap().item(item).is_some(),
            "writes={writes}"
        );
        if !crashed {
            break;
        }
    }
}

/// Scenario 10.
fn people_are_accounts_not_names_or_phones<D: NoteDoc>() {
    let mut relay = Relay::default();
    let (mut owner, note) = shared::<D>(&mut relay, "Party");
    let mut anna = Phone::<D>::person("Anna");
    let mut other_anna = Phone::<D>::person("Anna");
    let_in(&mut relay, &mut owner, &mut anna, note);
    let_in(&mut relay, &mut owner, &mut other_anna, note);
    assert_eq!(owner.names(note), ["Olga", "Anna", "Anna"]);

    // Anna's second phone asks too: it is Anna, who is already in.
    let mut anna2 = anna.restored();
    anna2.sync_account(&mut relay);
    let (invite, secret, request) = ask(&mut owner, &mut anna2, note);
    assert_eq!(
        owner.core().approve(invite, "req", &request).unwrap(),
        Approved::AlreadyHasAccess
    );
    owner.sync(&mut relay, note);
    let answer = owner.peek().approval_answer(invite).unwrap().unwrap();
    accept(&mut relay, &mut anna2, &secret, &answer);
    assert_eq!(anna2.title(note).as_deref(), Some("Party"));
    assert_eq!(owner.names(note), ["Olga", "Anna", "Anna"]);
    let search = owner.peek().collaborators(note).unwrap();
    assert_eq!(search.len(), 2);
    assert_ne!(search[0].id, search[1].id);
}

/// The current package is a function of the set of packages, whatever order
/// they arrive in: every order of a set with a fork, a follow-up and a
/// package whose parent comes last.
fn every_arrival_order_picks_the_same_current_package<D: NoteDoc>() {
    let mut relay = Relay::default();
    let (mut phone, note) = shared::<D>(&mut relay, "Order");
    let mut laptop = phone.restored();
    laptop.sync_account(&mut relay);
    let mut anna = Phone::<D>::person("Anna");
    let mut ben = Phone::<D>::person("Ben");
    let_in(&mut relay, &mut phone, &mut anna, note);
    let_in(&mut relay, &mut phone, &mut ben, note);
    laptop.sync(&mut relay, note);
    let (a, b) = (anna.member(), ben.member());
    remove(&mut phone, note, &a);
    remove(&mut laptop, note, &b);
    let x = phone.core().settle_note(note).unwrap();
    let y = laptop.core().settle_note(note).unwrap();
    for raw in x.envelopes.iter().chain(&y.envelopes) {
        relay.append(note, raw);
    }
    phone.sync(&mut relay, note);
    laptop.sync(&mut relay, note);
    let packages: Vec<Vec<u8>> = relay
        .log(note)
        .into_iter()
        .filter(|raw| Envelope::decode(raw).unwrap().kind == EnvelopeKind::Access)
        .collect();
    assert!(packages.len() >= 5, "{}", packages.len());
    let owner_key = Envelope::decode(&packages[0]).unwrap().author;
    let expected = phone.peek().current_package(note).unwrap();
    assert!(packages
        .iter()
        .all(|raw| Package::parse(raw, note, owner_key).is_ok()));

    // Heap's algorithm over the packages.
    let mut order: Vec<usize> = (0..packages.len()).collect();
    let mut counters = vec![0usize; order.len()];
    let mut seen = 0;
    let mut check = |order: &[usize]| {
        let mut observer = laptop.restored();
        observer.sync_account(&mut relay);
        for &i in order {
            let _ = observer.core().apply_remote(note, &packages[i]);
        }
        assert_eq!(
            observer.peek().current_package(note).unwrap(),
            expected,
            "{order:?}"
        );
        seen += 1;
    };
    check(&order);
    let mut i = 0;
    while i < order.len() {
        if counters[i] < i {
            if i % 2 == 0 {
                order.swap(0, i);
            } else {
                order.swap(counters[i], i);
            }
            check(&order);
            counters[i] += 1;
            i = 0;
        } else {
            counters[i] = 0;
            i += 1;
        }
    }
    assert!(seen >= 120);
}

fn stop_sharing_removes_everyone_but_the_owner<D: NoteDoc>() {
    let mut relay = Relay::default();
    let (mut owner, note) = shared::<D>(&mut relay, "Secret");
    let mut anna = Phone::<D>::person("Anna");
    let mut ben = Phone::<D>::person("Ben");
    let_in(&mut relay, &mut owner, &mut anna, note);
    let_in(&mut relay, &mut owner, &mut ben, note);
    owner.core().stop_sharing(note).unwrap();
    owner.sync(&mut relay, note);
    assert_eq!(owner.names(note), ["Olga"]);
    assert!(!owner.peek().people(note).unwrap().shared);
    assert!(anna.sync(&mut relay, note).removed);
    assert!(ben.sync(&mut relay, note).removed);
    assert_eq!(owner.title(note).as_deref(), Some("Secret"));
}

// ---------------------------------------------------------------------------
// A rotation against a permanent deletion
// ---------------------------------------------------------------------------

/// Nothing of the note is left on this owner device, and nothing it can send.
fn deleted_here<D: NoteDoc>(phone: &mut Phone<D>, note: NoteId) {
    let core = phone.core();
    assert!(core.is_deleted_note(note), "the marker is there");
    assert!(!core.store().notes().unwrap().contains(&note), "no rows");
    assert!(core.keyring().note_key(note).unwrap().is_none(), "no key");
    assert!(core.open_note(note).is_err(), "it does not open");
    assert!(
        core.unsent(note, None).unwrap().is_empty(),
        "nothing to send"
    );
    assert!(!core.syncable().unwrap().contains(&note), "not subscribed");
    assert!(
        core.settle_note(note).unwrap().is_empty(),
        "nothing to settle"
    );
}

/// The owner's laptop removes Anna while offline — a rotation to a fresh key,
/// with a new access package granting it to the owner's account and to Ben —
/// while the owner's phone deletes the note for good (plan-permanent-deletion,
/// "B … rotates while offline; A deletes").
///
/// `rotation_first` says which reaches the relay first. If the deletion does,
/// the laptop learns of it on reconnecting, before it publishes, and the
/// rotation never leaves it. If the rotation does, it cannot be recalled: Anna
/// is removed and Ben moves to the new key, as any rotation would do. Either
/// way, both owner devices end deleted, neither takes the new key from the
/// package that grants it to them, a device restored from the words later
/// takes nothing either, and nobody's account keeps writing about it. Ben's
/// copy is his own and keeps working.
fn an_offline_rotation_against_a_deletion<D: NoteDoc>(rotation_first: bool) {
    let mut relay = Relay::default();
    let (mut phone, note) = shared::<D>(&mut relay, "Chores");
    let mut laptop = phone.restored();
    laptop.sync_account(&mut relay);
    laptop.sync(&mut relay, note);
    let mut anna = Phone::<D>::person("Anna");
    let mut ben = Phone::<D>::person("Ben");
    let_in(&mut relay, &mut phone, &mut anna, note);
    let_in(&mut relay, &mut phone, &mut ben, note);
    laptop.sync(&mut relay, note);
    let old_key = laptop.key_id(note).unwrap();
    let before = relay.log(note).len();

    // Offline, on the laptop: remove Anna, which the app settles at once, and
    // type something that waits behind it.
    let anna_member = anna.member();
    remove(&mut laptop, note, &anna_member);
    let out = laptop.core().settle_note(note).unwrap();
    assert!(!out.envelopes.is_empty(), "the rotation is built");
    assert_ne!(laptop.key_id(note), Some(old_key), "under a new key");
    laptop.core().set_title(note, "Chores, rotated").unwrap();
    assert!(!laptop.peek().unsent(note, None).unwrap().is_empty());

    // Offline, on the phone: delete it.
    phone.core().set_trashed(note, true).unwrap();
    phone.core().forget_note(note).unwrap();

    if rotation_first {
        // The laptop's note socket catches up before its account one does.
        laptop.sync(&mut relay, note);
        assert!(relay.log(note).len() > before, "the rotation went out");
        phone.sync_account(&mut relay);
        laptop.sync_account(&mut relay);
        assert!(
            anna.sync(&mut relay, note).removed,
            "a sent rotation stands"
        );
        ben.sync(&mut relay, note);
        assert_eq!(ben.title(note).as_deref(), Some("Chores, rotated"));
        assert_ne!(ben.key_id(note), Some(old_key));
    } else {
        phone.sync_account(&mut relay);
        // Reconnecting catches the account up before any note publishes.
        laptop.sync_account(&mut relay);
        laptop.push(&mut relay, note);
        assert_eq!(relay.log(note).len(), before, "the rotation never left");
        assert!(!anna.sync(&mut relay, note).removed, "Anna was not removed");
        assert_eq!(anna.title(note).as_deref(), Some("Chores"));
        ben.sync(&mut relay, note);
        assert_eq!(ben.key_id(note), Some(old_key));
    }

    // Whatever else arrives — the whole note log, the rotation's package with
    // its grant for this account among it, and the account again — nothing
    // comes back on either owner device.
    for _ in 0..2 {
        for owner in [&mut phone, &mut laptop] {
            owner.sync_account(&mut relay);
            owner.try_sync(&mut relay, note).unwrap();
            owner.cursors.remove(&note);
            owner
                .core()
                .set_sync_cursor(note, Default::default())
                .unwrap();
            owner.try_sync(&mut relay, note).unwrap();
            owner.core().adopt_note_keys().unwrap();
            deleted_here(owner, note);
        }
    }
    let mut tablet = phone.restored();
    tablet.sync_account(&mut relay);
    tablet.try_sync(&mut relay, note).unwrap();
    deleted_here(&mut tablet, note);

    // The two owner devices have said all they have to say.
    let account = phone.peek().account_id().unwrap();
    let logged = relay.log(account).len();
    for owner in [&mut phone, &mut laptop, &mut tablet] {
        owner.sync_account(&mut relay);
    }
    assert_eq!(relay.log(account).len(), logged, "the account went quiet");

    // Ben's copy is his.
    set_title(&mut ben, &mut relay, note, "Chores, Ben's");
    ben.relaunch();
    ben.sync(&mut relay, note);
    assert_eq!(ben.title(note).as_deref(), Some("Chores, Ben's"));
}

fn an_offline_rotation_loses_to_a_deletion_that_syncs_first<D: NoteDoc>() {
    an_offline_rotation_against_a_deletion::<D>(false);
}

fn an_offline_rotation_loses_to_a_deletion_that_syncs_second<D: NoteDoc>() {
    an_offline_rotation_against_a_deletion::<D>(true);
}
