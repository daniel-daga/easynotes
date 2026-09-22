//! The API the UI talks to: one object that owns the keys, the CRDT and the log,
//! and hands out a stream of [`NoteView`]s.
//!
//! The shape is the one the Slice 1 plan names:
//! `create_note`, `open_note`, `apply_local`, `apply_remote`, `export_share_blob`,
//! `import_share_blob`, `watch`.
//!
//! The rule the whole app follows, established here: **the UI never mutates state
//! it then renders.** An edit goes in, an update comes out, and the new view
//! arrives on the watch channel. Local and remote edits therefore travel exactly
//! the same path, which is why the two-device case cannot drift from the one-device
//! case.

use crate::account::{AccountRoot, RecoveryPhrase};
use crate::crdt::{
    account, AccountDoc, AccountEdit, AccountView, Colour, Edit, Layout, NewItem, NoteDoc,
    NoteView, Placement,
};
use crate::crypto::{AuthorKey, DeviceIdentity, DeviceSeed, Envelope, EnvelopeKind, NoteKey};
use crate::error::{CoreError, Result};
use crate::ids::{ItemId, LabelId, NoteId, ReminderId};
use crate::index::IndexEntry;
use crate::order;
use crate::store::{Keyring, OpStore};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use tokio::sync::watch;

/// Where the sync cursor is kept in the store's per-note metadata.
///
/// Public because moving a device from one store to another has to carry it:
/// see [`crate::store::copy_store`].
pub const SYNC_CURSOR: &str = "sync";

/// What this device has sent to, and received from, the relay for one note.
///
/// Purely local bookkeeping: it never travels, and another device's copy would be
/// meaningless. It buys two things. `last_seq` makes a relaunch cheap — the client
/// resubscribes from where it left off instead of replaying the note's whole relay
/// log. `sent_upto` is what stops an edit made offline from being re-sent on every
/// launch for ever, which without it is the price of not losing it at all.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncCursor {
    /// The highest relay sequence number applied to this note.
    pub last_seq: i64,
    /// The highest counter of our own envelopes the relay has echoed back.
    /// `None` means the relay has acknowledged nothing from this device.
    pub sent_upto: Option<u64>,
}

/// Where a note's place in this device's list *was* kept, and what now says a
/// note exists at all.
///
/// An ordinal, not a timestamp: the core compiles for the browser, where the
/// clock is not `SystemTime`, and "which note is newer" is all it ever had to
/// answer. The account document has taken the answer over — a note's place in
/// the grid is a personal placement and lives there ([`crate::crdt::account`]) —
/// and this row keeps the two jobs it is still the only thing doing.
///
/// It is the row that puts a note on the disk the moment it is made, which is
/// what stops an untyped-in note from vanishing on the next launch
/// ([ADR 0007](../../docs/adr/0007-note-list.md)). And it is what a placement is
/// seeded from: a device that had notes before the account document existed gets
/// its grid back in the order it left it, because that order is still written
/// here.
pub const ADDED: &str = "added";

/// The account document's ID.
///
/// It is a document like any other — sealed under a key of its own, appended to
/// the same log, listed by the same store — so it needs an ID, and a reserved
/// one is what keeps it out of the note list. Sixteen zero bytes: no random ID
/// will ever be it, and [`NoteCore::import_share_blob`] refuses a share blob
/// that claims it, because accepting one would overwrite the key this device's
/// own placements are sealed with.
pub const ACCOUNT_DOC: NoteId = NoteId([0u8; 16]);

/// Where a note's row in the sealed index is kept, in the same per-note
/// metadata as the sync cursor.
///
/// Public for the same reason [`SYNC_CURSOR`] is: moving a device from one store
/// to another has to carry it. Leaving it behind would be correct — a missing
/// entry rebuilds itself — and would make the first launch after a migration pay
/// to open every note, which is precisely what the index exists to stop.
pub const INDEX: &str = "index";
const KEY_GENERATION: &str = "key-generation";
const NEXT_COUNTER: &str = "next-counter";

/// The setting that says whether the note list is a grid or one column.
pub const LAYOUT: &str = "layout";

/// Where the invites this device has opened are kept, and the join requests
/// it is waiting on: two metadata rows filed under [`ACCOUNT_DOC`].
///
/// Local and sealed, like every metadata row. Local because they are secrets
/// one device holds on its own — an owner secret, a guest's request key — and
/// because what they wait on is an answer only this device asked for. Filed
/// under the reserved ID because neither belongs to a note: a join has no note
/// yet, and an invite is this device's business about a note rather than part
/// of it. That row is the one address no note list, sync or relay ever shows.
pub const INVITES: &str = "invites";
pub const JOINS: &str = "joins";

/// An invite this device opened and has not seen answered, withdrawn or expired.
#[derive(Clone, Debug)]
pub struct OutgoingInvite {
    pub note: NoteId,
    pub secret: crate::invite::InviteSecret,
    pub owner: crate::invite::OwnerSecret,
    /// Unix seconds, from this device's clock and the lifetime the relay
    /// granted. Advisory: the relay's own expiry is the one that holds.
    pub expires_at: i64,
}

/// A join request this device made and is waiting on.
#[derive(Clone, Debug)]
pub struct PendingJoin {
    pub secret: crate::invite::InviteSecret,
    /// The relay's name for the request.
    pub request: String,
    pub guest: crate::invite::GuestKey,
    /// What this person called themselves, to show on the waiting screen.
    pub name: String,
}

impl PendingJoin {
    /// The six digits the owner will be shown for this request.
    pub fn code(&self) -> String {
        crate::invite::safety_code(&self.secret, &self.guest.public())
    }
}

/// The stored shape of both, which is strings so a row stays readable JSON.
#[derive(Serialize, Deserialize)]
struct StoredInvite {
    note: String,
    secret: String,
    owner: String,
    expires_at: i64,
}

#[derive(Serialize, Deserialize)]
struct StoredJoin {
    secret: String,
    request: String,
    guest: String,
    name: String,
}

/// One note, as a list of notes needs it: enough to draw a card, and no more.
///
/// The title and the counts come from the CRDT, so producing one means the note
/// is open and its log replayed. That is why [`NoteCore::notes`] opens
/// everything rather than reading a column somewhere: the title *is* ciphertext
/// until a key decrypts it, and no index beside the database may hold it in the
/// clear.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NoteSummary {
    pub id: NoteId,
    pub kind: NoteKind,
    pub title: String,
    /// Every item, checked or not.
    pub items: usize,
    pub checked: usize,
    /// The first few lines of the body, then the first few items, so a card can
    /// show what the note is about. Most shopping lists never get a title.
    pub preview: Vec<String>,
    /// What this person has done with the note: its colour, whether it is
    /// pinned, and whether it is in the archive or the trash. Personal, and from
    /// the account document rather than from the note.
    ///
    /// Archived and trashed notes are in this list. Which of them to *show* is
    /// the screen's question — a grid, an archive and a trash are three answers
    /// to it — and a core that filtered would have to be asked three times for
    /// what it already knows once.
    pub placement: Placement,
    /// The labels on this note, resolved to their names and sorted the way a
    /// menu sorts them.
    ///
    /// Resolved here rather than left to the caller because the names live in
    /// the account document, which nothing above this line reads — and because
    /// it is what makes a rename reach a card: renaming a label refreshes the
    /// list, and the list carries names, so every chip redraws without anything
    /// asking. The ID comes with it because the name is the part that can
    /// change: a screen draws the name and acts on the ID.
    pub labels: Vec<(LabelId, String)>,
}

/// One note a search found, and where it found the query.
///
/// The line matters as much as the note. A card shows four items, so a hit on
/// the twelfth is a note that appears in the results with nothing on it to say
/// why — which reads as a bug rather than as a match.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchHit {
    pub summary: NoteSummary,
    /// The title, or the first item, containing one of the query's terms.
    pub matched: String,
}

/// How many items a summary carries. Enough to recognise a list, few enough
/// that a hundred notes do not put a hundred whole documents through the
/// bridge on every keystroke.
pub const PREVIEW_ITEMS: usize = 4;

/// How long a note's log may get before opening it compacts the log away.
///
/// A number, not a science: 256 envelopes is a few weeks of a household's
/// shopping list and replays in well under a millisecond on the Yrs backend
/// ([ADR 0001](../../docs/adr/0001-crdt.md) measures 1 000 inserts at 10.7 ms).
/// What makes it safe to pick roughly is that the trade is not close. A 20-item
/// list with 256 toggles is a 55 KiB log against a 5 KiB snapshot on Yrs, and
/// opening it costs 18 ms from the log against 0.3 ms from the snapshot —
/// because replay verifies a signature per envelope, at 68 µs each, whichever
/// backend is underneath. `just bench-compaction` measures it, and
/// [ADR 0006](../../docs/adr/0006-compaction.md) reads it.
pub const COMPACT_AFTER: usize = 256;

/// The most plaintext one merged envelope is built from.
///
/// A quarter of what the relay accepts in one envelope
/// ([`crate::crypto::MAX_ENVELOPE_BYTES`]), so that a day of offline typing
/// becomes a handful of envelopes rather than one the relay refuses. Merging
/// only ever shrinks what goes in, so the budget is on the inputs.
pub const SQUASH_BUDGET: usize = crate::crypto::MAX_ENVELOPE_BYTES / 4;

/// One piece of what [`NoteCore::squash`] hands back.
enum Squashed {
    /// Passed through as it came.
    Kept(Vec<u8>),
    /// Sealed again as one envelope: each member's bytes and its update.
    Run(Vec<(Vec<u8>, Vec<u8>)>),
}

/// The run [`NoteCore::squash`] is building.
#[derive(Default)]
struct Run {
    members: Vec<(Vec<u8>, Vec<u8>)>,
    bytes: usize,
}

impl Run {
    fn close_into(&mut self, parts: &mut Vec<Squashed>) {
        if !self.members.is_empty() {
            parts.push(Squashed::Run(std::mem::take(&mut self.members)));
        }
        self.bytes = 0;
    }
}

/// Re-exported here because this is where callers have always found it: it was
/// a parameter of [`NoteCore::create_note`] long before a document recorded it.
pub use crate::crdt::NoteKind;

struct OpenNote<D: NoteDoc> {
    doc: D,
    key: NoteKey,
    /// Our own next counter. Recovered from the log on open, so a restart cannot
    /// reuse one.
    next_counter: u64,
    /// (author, counter) pairs already applied. This is what makes a replayed
    /// envelope a no-op at the core level, independently of the CRDT's own
    /// idempotence.
    seen: HashSet<(AuthorKey, u64)>,
    watch: watch::Sender<NoteView>,
}

/// The account document, open for as long as the core is.
///
/// The same four things a note needs — the document, the key it is sealed with,
/// our next counter and what we have already applied — because it *is* a note as
/// far as the crypto and the store are concerned. What it is not is a note as far
/// as the note list is concerned: it has no view, no watchers of its own, and no
/// card.
struct OpenAccount<A: AccountDoc> {
    doc: A,
    key: NoteKey,
    next_counter: u64,
    seen: HashSet<(AuthorKey, u64)>,
}

/// Where a sealed account-document envelope goes the moment it is made.
///
/// The account document's edits have no return value to hand back — there are
/// twenty entry points to it and several of them write more than once — so the
/// envelope is pushed here instead of being returned the way a note's is. What
/// receives it is the same publish a note edit reaches
/// ([`NoteCore::set_title`] and the bridge above it); this is only about who
/// does the handing over.
///
/// Set once, by whoever owns the core, and never called on a device that sets
/// none — which is every test that does not care, and the core itself, which
/// is not allowed to know that a relay exists.
pub type AccountEnvelopeSink = Box<dyn Fn(NoteId, &[u8]) + Send>;

pub struct NoteCore<D: NoteDoc, S: OpStore, K: Keyring> {
    identity: DeviceIdentity,
    actor: [u8; 16],
    store: S,
    keyring: K,
    open: HashMap<NoteId, OpenNote<D>>,
    /// This person's own view of their notes: colours, pins, the archive, the
    /// trash, the order of the grid and the view settings. One per device, open
    /// from the moment the core is.
    account: OpenAccount<D::Account>,
    /// Where the account document lives, for this device, today.
    ///
    /// [`ACCOUNT_DOC`] — sixteen zero bytes — until this device has an account,
    /// and the ID derived from the recovery phrase after that. It has to move,
    /// because the reserved ID is a fine *local* address and an impossible wire
    /// one: the relay keeps one log per note ID, so every user in the
    /// world syncing under zeros would be one topic. See
    /// [`crate::account`] and [ADR
    /// 0013](../../docs/adr/0013-recovery-phrase.md).
    account_id: NoteId,
    /// What that document currently says, kept beside it so that refreshing the
    /// list does not re-read the whole document on every keystroke.
    placements: AccountView,
    /// What every note this device holds says, without holding the notes open.
    ///
    /// This is the note list and the search index both: a summary is built from
    /// an entry rather than from a document, so a note that nothing has opened
    /// still has a card and is still found by a search. Rewritten from the
    /// document on every change, and reloaded from the store — or rebuilt by
    /// opening the note — on the first launch that finds one missing or stale.
    /// See [`crate::index`].
    index: HashMap<NoteId, IndexEntry>,
    /// The list of notes, for whoever is showing one. Recomputed from `open`
    /// every time a view changes, which is cheap because a summary is four
    /// fields off a document that is already in memory.
    library: watch::Sender<Vec<NoteSummary>>,
    /// Where a local account-document envelope goes. `None` until something
    /// asks for them, which on a device is the library wiring up its sync.
    account_sink: Option<AccountEnvelopeSink>,
}

impl<D: NoteDoc, S: OpStore, K: Keyring> NoteCore<D, S, K> {
    /// Load the device identity from the keyring, generating one on first run.
    pub fn open(mut store: S, mut keyring: K) -> Result<Self> {
        let seed = match keyring.device_seed()? {
            Some(seed) => seed,
            None => {
                let mut seed = DeviceSeed([0u8; 64]);
                crate::crypto::random_bytes(&mut seed.0);
                keyring.put_device_seed(&seed)?;
                seed
            }
        };
        let identity = DeviceIdentity::from_seed(&seed);
        let mut actor = [0u8; 16];
        actor.copy_from_slice(&identity.public_key().0[..16]);
        // Which account document this device opens, and under which key, is the
        // whole of what having an account means down here. Everything after
        // this line is the same code for both cases.
        let (account_id, account_key) = Self::account_address(&mut keyring)?;
        let (account, length) = Self::open_account(
            &mut store,
            &mut keyring,
            &identity,
            &actor,
            account_id,
            account_key,
        )?;
        let placements = account.doc.view();
        let mut core = Self {
            identity,
            actor,
            store,
            keyring,
            open: HashMap::new(),
            account,
            account_id,
            placements,
            index: HashMap::new(),
            library: watch::channel(Vec::new()).0,
            account_sink: None,
        };
        // The same moment `open_note` compacts at, for the same reason: the
        // document has just been rebuilt, so the snapshot is free, and the replay
        // that made compaction worth doing has just been paid for the last time.
        // The difference is that this one happens at launch rather than when a
        // screen opens, because opening the device *is* opening this document.
        if length > COMPACT_AFTER {
            core.compact_account()?;
        }
        // Before the compaction check would be wrong and after it is right: the
        // migration appends, so running it first would compact a log and then
        // immediately lengthen it. A device with nothing to migrate — which is
        // every device after its first launch on this build — writes nothing
        // here.
        core.adopt_legacy_tags()?;
        // What the account document knows about note keys, the keyring may not
        // yet: a device adopted from a phrase learns its keys by syncing this
        // document, and the sync happened during the *last* run. Cheap on every
        // other launch — it compares what is already in memory against a
        // keyring lookup per note and writes nothing.
        core.adopt_note_keys()?;
        Ok(core)
    }

    /// Where this device's account document lives and what opens it.
    ///
    /// Two cases, and the difference between them is the whole of item 2. With
    /// no account root the document is local-only, addressed by the reserved
    /// ID, under a random key minted here and kept in the keyring — exactly
    /// what [ADR 0008](../../docs/adr/0008-account-document.md) built. With one,
    /// both the ID and the key are *derived*, so any device that can say the
    /// twelve words arrives at the same two values and needs nothing carried
    /// between them.
    fn account_address(keyring: &mut K) -> Result<(NoteId, NoteKey)> {
        if let Some(root) = keyring.account_root()? {
            return Ok((root.document_id(), root.document_key()));
        }
        let key = match keyring.note_key(ACCOUNT_DOC)? {
            Some(key) => key,
            None => {
                let key = NoteKey::random();
                keyring.put_note_key(ACCOUNT_DOC, &key)?;
                key
            }
        };
        Ok((ACCOUNT_DOC, key))
    }

    /// Read the account document out of the store, minting its key on first run.
    /// Returns it with the length of the log it was replayed from, which is what
    /// [`NoteCore::open`] decides whether to compact on.
    ///
    /// No snapshot row is read, because none is written: this document's
    /// snapshot is an envelope *in its own log*
    /// ([ADR 0012](../../docs/adr/0012-compacting-the-account-document.md)), so
    /// replaying the log is the whole of opening it whether or not it has been
    /// compacted, and the loop below does not know which it is looking at.
    fn open_account(
        store: &mut S,
        keyring: &mut K,
        identity: &DeviceIdentity,
        actor: &[u8; 16],
        id: NoteId,
        key: NoteKey,
    ) -> Result<(OpenAccount<D::Account>, usize)> {
        let _ = keyring;
        let me = identity.public_key();
        let mut doc = <D::Account as AccountDoc>::create(actor);
        let mut next_counter = 0u64;
        let mut seen = HashSet::new();
        let mut length = 0usize;
        for raw in store.load(id)? {
            length += 1;
            let env = Envelope::decode(&raw)?;
            if env.note_id != id {
                return Err(CoreError::WrongNote {
                    expected: id.encode(),
                    got: env.note_id.encode(),
                });
            }
            let plain = env.open(&key)?;
            if seen.insert((env.author, env.counter)) {
                doc.apply_remote(&plain)?;
            }
            if env.author == me {
                next_counter = next_counter.max(env.counter + 1);
            }
        }

        Ok((
            OpenAccount {
                doc,
                key,
                next_counter,
                seen,
            },
            length,
        ))
    }

    /// Is this ID the account document rather than a note?
    ///
    /// Both addresses, always. The derived one because that is where this
    /// person's placements actually are, and the reserved one because it stays
    /// reserved for ever: a device that has an account must still refuse a
    /// share blob claiming sixteen zero bytes, and must still keep that ID out
    /// of the note list, or a peer on an older build could hand it one.
    fn is_account(&self, id: NoteId) -> bool {
        id == self.account_id || id == ACCOUNT_DOC
    }

    // -- the account --------------------------------------------------------
    //
    // One person, as twelve words. There is no server-side account here, no
    // password and no email address: a phrase derives an account root, the root
    // derives this document's ID and key, and a device belongs to the person who
    // can say the words. See [ADR 0013](../../docs/adr/0013-recovery-phrase.md).

    /// The ID this device's account document syncs under, if it has an account.
    ///
    /// `None` is the ordinary state of a device nobody has set an account up on,
    /// and it is what tells the sync engine there is nothing here to subscribe
    /// to: the local-only document under [`ACCOUNT_DOC`] must never reach a
    /// relay, because every device in the world would be sharing that topic.
    pub fn account_id(&self) -> Option<NoteId> {
        (self.account_id != ACCOUNT_DOC).then_some(self.account_id)
    }

    pub fn has_account(&self) -> bool {
        self.account_id != ACCOUNT_DOC
    }

    /// Mint a new account and return its recovery phrase, to be shown once.
    ///
    /// The phrase is **not stored**, here or anywhere else. What is kept is the
    /// root it derives, which is enough to reopen the document on this device
    /// for ever and not enough to reconstruct the words. That is the whole
    /// reason the design doc asks for a forced backup check at setup: this is
    /// the only moment the phrase exists, and the app has to make somebody write
    /// it down before the moment passes.
    ///
    /// Refuses on a device that already has an account rather than replacing it,
    /// because replacing one would strand every other device of that person with
    /// no way back.
    pub fn create_account(&mut self) -> Result<RecoveryPhrase> {
        if self.has_account() {
            return Err(CoreError::Crypto("this device already has an account"));
        }
        let phrase = RecoveryPhrase::generate();
        self.adopt_root(phrase.root())?;
        Ok(phrase)
    }

    /// Adopt the account a phrase names.
    ///
    /// The second device. It keeps everything this device already had -- its
    /// notes, their keys and its placements are carried into the adopted
    /// document rather than dropped -- because a person who has been using the
    /// app and then recovers their account on the same device must not watch
    /// their colours disappear as the price of setting up sync. The cost is
    /// that a placement this device holds and the account also holds resolve
    /// last-writer-wins like any other concurrent register, which for a
    /// genuinely new device is a choice between a value and nothing.
    ///
    /// Nothing is fetched here. This makes the device *able* to open the
    /// account document; filling it is the sync engine's job on the next
    /// connection, and [`NoteCore::adopt_note_keys`] is what turns the keys that
    /// arrive into notes this device can read.
    pub fn adopt_account(&mut self, phrase: &RecoveryPhrase) -> Result<()> {
        if self.has_account() {
            return Err(CoreError::Crypto("this device already has an account"));
        }
        self.adopt_root(phrase.root())
    }

    /// Move this device's account document onto the address and key a root
    /// derives, carrying what it already says.
    ///
    /// One `replace_log` at the new ID and then a `forget` of the old, in that
    /// order, so that a crash in the middle leaves the document readable at
    /// *both* addresses rather than at neither. The next launch reads the new
    /// one -- the root is written first -- and the stale zeros log is so much
    /// dead weight until the `forget` that follows it is retried, which is
    /// exactly the shape [ADR 0010](../../docs/adr/0010-forgetting-a-note.md)
    /// gives an interrupted delete.
    ///
    /// The document is re-sealed rather than re-keyed in place, because there is
    /// no such thing as re-keying an envelope: every one of them binds its note
    /// ID into the AAD and its author's signature, so an envelope written for
    /// the reserved ID cannot be presented under the derived one. Rewriting the
    /// whole document as a single snapshot envelope is [ADR
    /// 0012](../../docs/adr/0012-compacting-the-account-document.md)'s
    /// compaction doing double duty, and it drops the history of placements this
    /// person set before they had an account -- which nothing was ever going to
    /// show them.
    fn adopt_root(&mut self, root: AccountRoot) -> Result<()> {
        let old_id = self.account_id;
        let new_id = root.document_id();
        if new_id == ACCOUNT_DOC {
            // Sixteen zero bytes out of HKDF is not going to happen, and a
            // silent collision here would put this person's account on the
            // topic every account-less device uses locally.
            return Err(CoreError::Crypto("that account root is not usable"));
        }

        // Every key this device holds goes into the document before it moves,
        // so that the other devices of this account can open the notes this one
        // already had. Notes created from now on write their key as they are
        // made; this is the sweep for everything that came first.
        self.record_held_note_keys()?;

        let key = root.document_key();
        let blob = self.account.doc.snapshot();
        let envelope = Envelope::seal(&self.identity, new_id, &key, 0, &blob)?;

        // The root first. It is what `account_address` reads on the next
        // launch, so writing it last would mean a crash after the log moved
        // reopened the *old* document and quietly lost everything since.
        self.keyring.put_account_root(&root)?;
        self.store.replace_log(new_id, &[envelope.encode()])?;

        self.account.key = key;
        self.account.next_counter = 1;
        self.account.seen.clear();
        self.account.seen.insert((self.identity.public_key(), 0));
        self.account_id = new_id;

        if old_id != new_id {
            // The invite rows are filed under the reserved ID, which is what
            // the old document may be. They are this device's and not the
            // document's, so they outlive it.
            let kept = [
                (INVITES, self.store.get_meta(ACCOUNT_DOC, INVITES)?),
                (JOINS, self.store.get_meta(ACCOUNT_DOC, JOINS)?),
            ];
            self.store.forget(old_id)?;
            self.keyring.forget_note_key(old_id)?;
            for (key, value) in kept {
                if let Some(value) = value {
                    self.store.put_meta(ACCOUNT_DOC, key, &value)?;
                }
            }
        }
        Ok(())
    }

    /// Put every note key this device holds into the account document.
    ///
    /// Reads the notes from the *store* rather than the keyring, because a
    /// keyring answers about one note at a time by design -- it is a place keys
    /// go in, not a list of what this person has.
    fn record_held_note_keys(&mut self) -> Result<()> {
        let known = account::note_access_of(self.account.doc.entries());
        let mut writes = Vec::new();
        for id in self.store.notes()? {
            // A clean rollout lets the device that first turns its local notes
            // into an account become their owner. Upgrade any old bare-key row
            // at the same time, so every note carried into an account has a
            // revocation identity.
            if self.is_account(id) || known.get(&id).is_some_and(|access| access.owner.is_some()) {
                continue;
            }
            if let Some(key) = self.keyring.note_key(id)? {
                let mut rotation_seed = DeviceSeed([0u8; 64]);
                crate::crypto::random_bytes(&mut rotation_seed.0);
                let rotation_identity = DeviceIdentity::from_seed(&rotation_seed);
                writes.push(account::AccountEdit::SetNoteKey {
                    note: id,
                    key: account::encode_note_access(
                        &key,
                        rotation_identity.public_key(),
                        true,
                        0,
                        Some(&rotation_seed),
                    ),
                });
            }
        }
        for edit in writes {
            self.apply_account(edit)?;
        }
        Ok(())
    }

    /// Take into the keyring any note key the account document holds and this
    /// device does not.
    ///
    /// This is how a second device comes to be able to *read* the notes it
    /// syncs, rather than merely to store their ciphertext.
    ///
    /// Run when a batch of updates has **settled** — at launch, and on the
    /// relay's `Caught` ([`crate::sync::wire::Incoming`]), which is exactly the
    /// message that says the backlog is drained — and not after each envelope.
    /// The difference is not an optimisation. Replaying a history one envelope
    /// at a time walks through states the document was never left in: a note
    /// created and later forgotten appears, with its key, in the middle of a
    /// log whose end does not mention it. Adopting per envelope would take that
    /// key and keep it, because this only ever adds. Adopting once the batch has
    /// settled reads the document as it actually stands.
    ///
    /// Fill a missing key, or replace a held key only when the account document
    /// has a strictly newer generation. Envelope key IDs keep old history from
    /// being confused with a new generation; an owner-signed snapshot performs
    /// the actual cutover. See [ADR 0018](../../docs/adr/0018-revocation.md).
    pub fn adopt_note_keys(&mut self) -> Result<usize> {
        let mut taken = 0;
        let mut rotated = Vec::new();
        let access = account::note_access_of(self.account.doc.entries());
        for (note, access) in access {
            if self.is_account(note) {
                continue;
            }
            let current = self.keyring.note_key(note)?;
            if current
                .as_ref()
                .is_some_and(|key| key.id() == access.key.id())
            {
                continue;
            }
            // The generation makes a newer account register authoritative for
            // both owners and guests, while preventing a stale register from
            // turning a revoked key back into a live one.
            let current_generation = self
                .store
                .get_meta(note, KEY_GENERATION)?
                .and_then(|raw| raw.as_slice().try_into().ok().map(u64::from_be_bytes))
                .unwrap_or(0);
            if current.is_some() && access.generation <= current_generation {
                continue;
            }
            if current.is_some() {
                rotated.push(note);
            }
            self.open.remove(&note);
            self.keyring.put_note_key(note, &access.key)?;
            self.store
                .put_meta(note, KEY_GENERATION, &access.generation.to_be_bytes())?;
            taken += 1;
        }
        if taken > 0 {
            for note in rotated {
                self.open_note(note)?;
            }
            // A note this device could not read a moment ago now has a key, so
            // the list it was missing from has changed.
            self.adopt_unplaced()?;
            self.refresh_library();
        }
        Ok(taken)
    }

    /// Apply an envelope the relay had for the account document.
    ///
    /// The note path's [`NoteCore::apply_remote`] cannot serve here -- it opens
    /// a `NoteDoc` and this is an `AccountDoc`, a deliberately much narrower
    /// thing -- but everything that makes it safe is the same and in the same
    /// order: the ID must match, the signature is verified before anything is
    /// decrypted, a repeat is dropped rather than re-applied, and the envelope
    /// is appended only once it has been believed.
    pub fn apply_remote_account(&mut self, envelope: &[u8]) -> Result<()> {
        if !self.has_account() {
            // Nothing can have arrived for a document that has never been on a
            // relay, so this is a bug or a lie either way.
            return Err(CoreError::NoAccount);
        }
        let env = Envelope::decode(envelope)?;
        if env.note_id != self.account_id {
            return Err(CoreError::WrongNote {
                expected: self.account_id.encode(),
                got: env.note_id.encode(),
            });
        }
        let plain = env.open(&self.account.key)?;
        if !self.account.seen.insert((env.author, env.counter)) {
            return Ok(());
        }
        self.account.doc.apply_remote(&plain)?;
        self.store.append(self.account_id, envelope)?;
        if env.author == self.identity.public_key() {
            self.account.next_counter = self.account.next_counter.max(env.counter + 1);
        }
        self.placements = self.account.doc.view();
        // Note keys are deliberately *not* adopted here. This is one envelope of
        // what may be a long replay, and a key taken out of a passing state is a
        // key this device would then keep for ever; see
        // [`NoteCore::adopt_note_keys`] for the case that breaks. The caller
        // runs it when the batch has settled.
        self.adopt_unplaced()?;
        self.refresh_library();
        Ok(())
    }

    pub fn device_key(&self) -> AuthorKey {
        self.identity.public_key()
    }

    pub fn identity(&self) -> &DeviceIdentity {
        &self.identity
    }

    // -- lifecycle ----------------------------------------------------------

    pub fn create_note(&mut self, kind: NoteKind) -> Result<NoteId> {
        let id = NoteId::random();
        let key = NoteKey::random();
        self.keyring.put_note_key(id, &key)?;
        let mut rotation_seed = DeviceSeed([0u8; 64]);
        crate::crypto::random_bytes(&mut rotation_seed.0);
        let rotation_identity = DeviceIdentity::from_seed(&rotation_seed);
        let recorded = account::encode_note_access(
            &key,
            rotation_identity.public_key(),
            true,
            0,
            Some(&rotation_seed),
        );
        let doc = D::create(&self.actor);
        let (tx, _) = watch::channel(doc.view());
        let added = self.next_ordinal()?;
        self.open.insert(
            id,
            OpenNote {
                doc,
                key,
                next_counter: 0,
                seen: HashSet::new(),
                watch: tx,
            },
        );
        // This is also what puts the note on disk. A note exists from the moment
        // it is created rather than from its first edit, because an empty note
        // that is in the list until the app restarts and then is not would be a
        // worse answer than either keeping it or refusing to make it. What it
        // costs is that a note made by accident stays until there is a way to
        // delete one, which is the trash — a personal placement in the account
        // document, item 6 of the Slice 2 plan.
        self.store.put_meta(id, ADDED, &added.to_be_bytes())?;
        self.store
            .put_meta(id, KEY_GENERATION, &0u64.to_be_bytes())?;
        self.store.put_meta(id, NEXT_COUNTER, &0u64.to_be_bytes())?;
        // An empty entry, so that the note has a card from the moment it exists
        // rather than from its first edit. `place` below refreshes the list, and
        // the list is built from the index.
        self.reindex(id)?;
        // Where it sits is the account document's answer, and a new note goes to
        // the top of the grid. `place` refreshes the list, which is why nothing
        // here does.
        self.place(id, 0)?;
        // And the key, so this person's other devices can open it. A register in
        // the account document is the only channel one person's devices have
        // that nobody else can read; see `crate::crdt::account`.
        self.apply_account(account::AccountEdit::SetNoteKey {
            note: id,
            key: recorded,
        })?;
        // A checklist is what a document that records no kind already is, so
        // only a text note says so — and says it in its first change, which is
        // also what gives the Automerge backend a body every peer shares (see
        // `crdt::automerge_doc`). Its envelope waits in the log like any offline
        // edit and goes when the note is connected.
        if kind == NoteKind::Text {
            self.apply_local(id, Edit::SetKind(kind))?;
        }
        Ok(id)
    }

    /// Open a note that is already in the store, replaying its snapshot and log.
    /// This is what makes demo step 6 — relaunch with no network — work.
    pub fn open_note(&mut self, id: NoteId) -> Result<NoteView> {
        if self.is_account(id) {
            // Not a note, whatever the caller was handed. Opening it as one would
            // replay this person's placements into an empty checklist and put a
            // blank card in their grid.
            return Err(CoreError::UnknownNote(id.encode()));
        }
        if let Some(note) = self.open.get(&id) {
            return Ok(note.doc.view());
        }
        let key = self
            .keyring
            .note_key(id)?
            .ok_or_else(|| CoreError::UnknownNote(id.encode()))?;

        let me = self.identity.public_key();
        let mut next_counter = self
            .store
            .get_meta(id, NEXT_COUNTER)?
            .and_then(|raw| raw.as_slice().try_into().ok().map(u64::from_be_bytes))
            .unwrap_or(0);
        let key_id = key.id();
        let log = self.store.load(id)?;
        let owner = account::note_access_of(self.account.doc.entries())
            .remove(&id)
            .and_then(|access| access.owner);
        let has_current_snapshot = log.iter().any(|raw| {
            Envelope::decode(raw).is_ok_and(|env| {
                env.key_id == Some(key_id)
                    && env.kind == EnvelopeKind::Snapshot
                    && owner == Some(env.author)
            })
        });

        let mut doc = match self.store.get_snapshot(id)? {
            Some(blob) => {
                let env = Envelope::decode(&blob)?;
                if env.key_id.is_some_and(|seen| seen != key_id) {
                    D::create(&self.actor)
                } else {
                    let plain = env.open(&key)?;
                    // A snapshot is sealed with a counter from this device's own
                    // sequence, which is what lets the counter survive a compaction
                    // that removed every envelope it could otherwise be read from.
                    if env.author == me {
                        next_counter = env.counter + 1;
                    }
                    D::load(&self.actor, &plain)?
                }
            }
            None => D::create(&self.actor),
        };

        let mut seen = HashSet::new();
        let mut length = 0usize;
        for raw in log {
            length += 1;
            let env = Envelope::decode(&raw)?;
            if env.note_id != id {
                return Err(CoreError::WrongNote {
                    expected: id.encode(),
                    got: env.note_id.encode(),
                });
            }
            if env.key_id.is_some_and(|seen| seen != key_id)
                || (env.key_id.is_none() && has_current_snapshot)
                || (env.kind == EnvelopeKind::Snapshot && owner != Some(env.author))
            {
                continue;
            }
            let plain = env.open(&key)?;
            if seen.insert((env.author, env.counter)) {
                if env.kind == EnvelopeKind::Snapshot {
                    doc = D::load(&self.actor, &plain)?;
                } else if env.kind == EnvelopeKind::Update {
                    doc.apply_remote(&plain)?;
                }
            }
            if env.author == me {
                next_counter = next_counter.max(env.counter + 1);
            }
        }

        let view = doc.view();
        let (tx, _) = watch::channel(view.clone());
        self.open.insert(
            id,
            OpenNote {
                doc,
                key,
                next_counter,
                seen,
                watch: tx,
            },
        );
        // The document has just been replayed, so this is the cheapest moment
        // there will ever be to write down what it says — and if the entry was
        // missing or stale, which is why we are here, it is now neither.
        self.reindex(id)?;
        self.refresh_library();

        // The one place where opening a note writes to the store, and the right
        // one: the document has just been rebuilt, so the snapshot costs nothing
        // to produce, and the replay that made compaction worth doing has just
        // been paid for the last time.
        if length > COMPACT_AFTER {
            self.compact(id)?;
            return self.view(id);
        }
        Ok(view)
    }

    pub fn view(&self, id: NoteId) -> Result<NoteView> {
        Ok(self.note(id)?.doc.view())
    }

    /// A receiver that yields the current view and then every subsequent one.
    /// The UI renders from this and from nothing else.
    pub fn watch(&self, id: NoteId) -> Result<watch::Receiver<NoteView>> {
        Ok(self.note(id)?.watch.subscribe())
    }

    fn note(&self, id: NoteId) -> Result<&OpenNote<D>> {
        self.open
            .get(&id)
            .ok_or_else(|| CoreError::UnknownNote(id.encode()))
    }

    // -- the note list ------------------------------------------------------
    //
    // A note list is not a query: every title in it is inside an envelope that
    // only a key opens, so a device that kept nothing but the log would have to
    // open every note to draw a grid. That is the cost of the store holding
    // nothing in the clear ([ADR 0005](../../docs/adr/0005-local-store.md)), and
    // the sealed index is how it is paid once instead of at every launch — a
    // card is drawn from an entry, and the entry is rewritten from the document
    // by everything that changes one, so the two cannot disagree
    // ([`crate::index`]).

    /// Every note this device holds, newest first.
    ///
    /// Reads each note's index entry, and opens only the notes whose entry is
    /// missing or no longer matches their log — a note created before the index
    /// existed, one carried in by [`crate::store::copy_store`], or one whose last
    /// edit was interrupted between the log and the entry. Those are rebuilt
    /// here, which costs exactly what every launch used to cost and happens once.
    pub fn notes(&mut self) -> Result<Vec<NoteSummary>> {
        for id in self.store.notes()? {
            if self.is_account(id) || self.index.contains_key(&id) {
                continue;
            }
            if self.load_index(id)? {
                continue;
            }
            match self.open_note(id) {
                Ok(_) => {}
                // A note whose key this device does not have is not a note this
                // device can show. It can happen: `copy_store` carries a log
                // whose key never arrived, and a future revocation takes a key
                // away while its log stays. Skipping it keeps the list usable
                // rather than making one unreadable note hide the rest.
                Err(CoreError::UnknownNote(_)) => continue,
                Err(e) => return Err(e),
            }
        }
        self.adopt_unplaced()?;
        self.refresh_library();
        Ok(self.library.borrow().clone())
    }

    /// Every note whose text contains all of `query`'s terms, in the order the
    /// list shows them.
    ///
    /// Local and nothing but local: it reads the same sealed entries the grid is
    /// drawn from, so it works with no network, in an aeroplane, and against a
    /// relay that is down — which is the point of the design doc putting search
    /// over "the decrypted local database only". Archived and trashed notes are
    /// in the results with their placements on them, for the reason they are in
    /// the list: which of them to show is the screen's question.
    ///
    /// It lists first, so that a search made before anything has drawn a grid
    /// still searches the whole device.
    pub fn search(&mut self, query: &str) -> Result<Vec<SearchHit>> {
        let terms = crate::index::terms(query);
        if terms.is_empty() {
            return Ok(Vec::new());
        }
        let all = self.notes()?;
        let mut hits = Vec::new();
        for summary in all {
            let Some(entry) = self.index.get(&summary.id) else {
                continue;
            };
            if let Some(line) = entry.matched_line(&terms) {
                hits.push(SearchHit {
                    matched: line.to_string(),
                    summary,
                });
            }
        }
        Ok(hits)
    }

    // -- the index ----------------------------------------------------------

    /// Write down what an open note currently says.
    ///
    /// Called by everything that changes a document, *after* the change is in the
    /// log. The order is the whole of the staleness story: an entry is never
    /// ahead of the log it summarises, so a crash in between leaves one that
    /// fails its length check and is rebuilt rather than one that claims an edit
    /// the log does not have. See [`crate::index`].
    fn reindex(&mut self, id: NoteId) -> Result<()> {
        let Some(note) = self.open.get(&id) else {
            // Nothing to write it from. Not an error: a note can be listed
            // without being open, and it is the open ones that are out of date.
            return Ok(());
        };
        let view = note.doc.view();
        let entry = IndexEntry {
            version: crate::index::ENTRY_VERSION,
            kind: view.kind,
            title: view.title,
            body: crate::index::body_lines(&view.body),
            checked: view.items.iter().filter(|i| i.checked).count(),
            items: view.items.into_iter().map(|i| i.text).collect(),
            log_len: self.store.log_len(id)?,
        };
        let sealed = entry.seal(id, &note.key)?;
        self.store.put_meta(id, INDEX, &sealed)?;
        self.index.insert(id, entry);
        Ok(())
    }

    /// Read a note's entry out of the store. `false` means there was nothing
    /// usable there and the note has to be opened.
    fn load_index(&mut self, id: NoteId) -> Result<bool> {
        let Some(sealed) = self.store.get_meta(id, INDEX)? else {
            return Ok(false);
        };
        // A note whose key is gone cannot be shown at all, so it is not this
        // function's business to decide what happens next: it says no, and
        // `notes` skips the note for the same reason it always did.
        let Some(key) = self.keyring.note_key(id)? else {
            return Ok(false);
        };
        let log_len = self.store.log_len(id)?;
        match IndexEntry::open(id, &key, &sealed, log_len) {
            Some(entry) => {
                self.index.insert(id, entry);
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// Open a note that an edit or an arrival is about to touch, if it is not
    /// open already.
    ///
    /// Before the index, the note list opened everything, so anything the app
    /// could reach was already open. Now a note can be listed, coloured, pinned
    /// and searched without its document ever being replayed — so the first
    /// thing that needs the document has to say so.
    fn ensure_open(&mut self, id: NoteId) -> Result<()> {
        if self.open.contains_key(&id) {
            return Ok(());
        }
        self.open_note(id)?;
        Ok(())
    }

    /// The note list now, and every version of it after that.
    ///
    /// Only reflects notes this core has read, so a caller that wants the whole
    /// list calls [`NoteCore::notes`] first. Everything after that — an edit
    /// here, an edit from a peer, a note created or joined — arrives on this
    /// channel.
    pub fn watch_notes(&self) -> watch::Receiver<Vec<NoteSummary>> {
        self.library.subscribe()
    }

    /// The labels on a note, with their names, sorted as a menu sorts them.
    fn labels_of(&self, id: NoteId) -> Vec<(LabelId, String)> {
        let mut labels: Vec<(LabelId, String)> = self
            .placements
            .labels_on(id)
            .into_iter()
            .filter_map(|label| {
                self.placements
                    .label_name(label)
                    .map(|name| (label, name.to_string()))
            })
            .collect();
        labels.sort_by(|a, b| {
            a.1.to_lowercase()
                .cmp(&b.1.to_lowercase())
                .then_with(|| a.1.cmp(&b.1))
                .then_with(|| a.0.cmp(&b.0))
        });
        labels
    }

    fn refresh_library(&self) {
        let mut summaries: Vec<NoteSummary> = self
            .index
            .iter()
            .map(|(id, entry)| NoteSummary {
                id: *id,
                kind: entry.kind,
                title: entry.title.clone(),
                items: entry.items.len(),
                checked: entry.checked,
                preview: entry.preview().take(PREVIEW_ITEMS).cloned().collect(),
                placement: self.placements.placement(*id),
                labels: self.labels_of(*id),
            })
            .collect();
        // Pinned notes first, then the grid order, then the ID to break a tie —
        // because two notes placed at the top by two devices at the same moment
        // can share an order key, and a list that reshuffles itself between
        // frames is worse than an arbitrary order that holds still.
        summaries.sort_by(|a, b| {
            b.placement
                .pinned
                .cmp(&a.placement.pinned)
                .then_with(|| a.placement.order.cmp(&b.placement.order))
                .then_with(|| a.id.cmp(&b.id))
        });
        // `send_replace`, not `send`: the latter refuses when nothing is
        // subscribed yet and leaves the channel holding a list that is already
        // wrong. A screen that subscribes after the first edit would then be
        // handed that stale list as its opening frame.
        self.library.send_replace(summaries);
    }

    /// The ordinal a note was made with. `0` for one that predates [`ADDED`]
    /// being written, which is what a placement is seeded from and nothing else
    /// reads.
    fn ordinal(&self, id: NoteId) -> Result<u64> {
        let Some(raw) = self.store.get_meta(id, ADDED)? else {
            return Ok(0);
        };
        let bytes: [u8; 8] = raw
            .as_slice()
            .try_into()
            .map_err(|_| CoreError::Storage("unreadable note order".into()))?;
        Ok(u64::from_be_bytes(bytes))
    }

    fn next_ordinal(&self) -> Result<u64> {
        let mut highest = 0u64;
        for id in self.store.notes()? {
            highest = highest.max(self.ordinal(id)?);
        }
        Ok(highest + 1)
    }

    // -- personal placements and settings ------------------------------------
    //
    // The account document. Everything here changes what *this person* sees —
    // a colour, a pin, the archive, the trash, the order of the grid, a view
    // setting — and none of it reaches the note, so none of it reaches whoever
    // else is on the note. That separation is the whole reason the document
    // exists ([ADR 0008](../../docs/adr/0008-account-document.md)).

    /// This person's placements and settings, as they stand.
    pub fn account(&self) -> &AccountView {
        &self.placements
    }

    /// Where one note sits and what it looks like. The default — no colour, not
    /// pinned, in the grid — for a note nobody has touched.
    pub fn placement(&self, id: NoteId) -> Placement {
        self.placements.placement(id)
    }

    pub fn set_colour(&mut self, id: NoteId, colour: Option<Colour>) -> Result<()> {
        self.apply_account(AccountEdit::SetColour { note: id, colour })
    }

    pub fn set_pinned(&mut self, id: NoteId, pinned: bool) -> Result<()> {
        self.apply_account(AccountEdit::SetPinned { note: id, pinned })
    }

    /// Every label this person has, with how many notes carry each.
    ///
    /// A label exists on its own now rather than only through the notes that
    /// carry it, so a count of zero is a real answer: taking a label off the
    /// last note leaves the label. [`AccountView::tags`] sorts them the way a
    /// menu wants them.
    pub fn tags(&self) -> Vec<(LabelId, String, usize)> {
        self.placements.tags()
    }

    /// The labels on one note, by ID.
    pub fn tags_on(&self, id: NoteId) -> Vec<LabelId> {
        self.placements.labels_on(id)
    }

    /// What a label is called, or `None` if this device has no name for it.
    pub fn tag_name(&self, label: LabelId) -> Option<&str> {
        self.placements.label_name(label)
    }

    /// Every complete reminder this person has, ordered by its next stored due
    /// time. The platform owns scheduling; the core owns the encrypted source
    /// of truth that every one of this person's devices converges on.
    pub fn reminders(&self) -> Vec<account::Reminder> {
        let mut reminders: Vec<_> = self.placements.reminders.values().cloned().collect();
        reminders.sort_by_key(|reminder| (reminder.fire_at, reminder.id));
        reminders
    }

    /// Every reminder attached to one note.
    pub fn reminders_on(&self, note: NoteId) -> Vec<account::Reminder> {
        self.placements.reminders_on(note)
    }

    /// Make one reminder. Its random identity makes a second reminder on the
    /// same note independent rather than a last-writer-wins replacement.
    pub fn create_reminder(
        &mut self,
        note: NoteId,
        fire_at: i64,
        repeat: account::RepeatRule,
        timezone: String,
    ) -> Result<ReminderId> {
        let reminder = ReminderId::random();
        self.set_reminder(reminder, note, fire_at, repeat, timezone)?;
        Ok(reminder)
    }

    /// Change a reminder's complete local value. It is sealed as one account
    /// update, while the CRDT still keeps the individual fields independent
    /// when another device changes a different field concurrently.
    pub fn set_reminder(
        &mut self,
        id: ReminderId,
        note: NoteId,
        fire_at: i64,
        repeat: account::RepeatRule,
        timezone: String,
    ) -> Result<()> {
        if fire_at < 0 || timezone.trim().is_empty() {
            return Err(CoreError::UnknownNote(
                "a reminder needs a time and timezone".into(),
            ));
        }
        self.apply_account(AccountEdit::SetReminder {
            reminder: account::Reminder {
                id,
                note,
                fire_at,
                repeat,
                timezone,
            },
        })
    }

    /// Remove one reminder. It is not an error if a remote delete got there
    /// first; the CRDT write simply has nothing left to change.
    pub fn forget_reminder(&mut self, id: ReminderId) -> Result<()> {
        self.apply_account(AccountEdit::ForgetReminder { reminder: id })
    }

    /// Put a label on a note, and return the label it went on under.
    ///
    /// `None` for something that is not a name at all — whitespace, or nothing.
    /// Otherwise an existing label whose name matches ignoring case, or a new
    /// one. Typing `recipes` under a `Recipes` this person made last week means
    /// the label they already have, not a second one that looks the same on a
    /// chip.
    ///
    /// What it cannot do is stop two *devices* from making that pair, each
    /// offline and each the first to use the name. That residue is smaller than
    /// it was — [`NoteCore::rename_tag`] can now merge the two, which nothing
    /// could when a label *was* its name — and it is still real until the
    /// account document syncs at all.
    ///
    /// Labelling a note this device cannot open is refused, for the reason
    /// forgetting the account document is: a placement is about a note, and a
    /// note with a placement and no key is a card that cannot be drawn.
    pub fn add_tag(&mut self, id: NoteId, tag: &str) -> Result<Option<LabelId>> {
        let Some(name) = account::normalise_tag(tag) else {
            return Ok(None);
        };
        let label = match self.placements.label_named(&name) {
            Some(label) => label,
            None => self.make_tag(&name)?,
        };
        self.apply_account(AccountEdit::SetTag {
            note: id,
            label,
            on: true,
        })?;
        Ok(Some(label))
    }

    /// Make a label that no note carries.
    ///
    /// Reachable on its own now, which it was not when a label was its name:
    /// there was nowhere to put one. An existing label with this name is
    /// returned rather than a second one made.
    pub fn make_tag(&mut self, name: &str) -> Result<LabelId> {
        let Some(name) = account::normalise_tag(name) else {
            return Err(CoreError::UnknownNote("a label needs a name".into()));
        };
        if let Some(existing) = self.placements.label_named(&name) {
            return Ok(existing);
        }
        let label = LabelId::random();
        self.apply_account(AccountEdit::SetLabelName { label, name })?;
        Ok(label)
    }

    /// Take a label off a note. Not an error if it was never on it.
    ///
    /// The label itself stays, with one fewer note on it. Removing the label is
    /// [`NoteCore::forget_tag`], and it is a different gesture on purpose: a
    /// label that disappeared because somebody tidied up the one note carrying
    /// it is a thing to explain.
    pub fn remove_tag(&mut self, id: NoteId, label: LabelId) -> Result<()> {
        // A membership the migration has not reached is named rather than
        // pointed at, so taking it off means removing the old key as well —
        // otherwise the label would come back on the next launch.
        let placement = self.placement(id);
        for tag in &placement.legacy_tags {
            if self.placements.label_named(tag) == Some(label) {
                self.apply_account(AccountEdit::DropLegacyTag {
                    note: id,
                    tag: tag.clone(),
                })?;
            }
        }
        if !placement.tags.contains(&label) {
            return Ok(());
        }
        self.apply_account(AccountEdit::SetTag {
            note: id,
            label,
            on: false,
        })
    }

    /// Rename a label. Every note carrying it keeps it.
    ///
    /// This is the thing a label having an ID is for. It is one register write:
    /// the memberships point at the ID and never mention the name, so two
    /// devices renaming the same label converge on one name exactly as they
    /// converge on a colour, and no note loses a label on the way.
    ///
    /// **Renaming onto a name another label already has merges the two**, which
    /// is what Keep does and the only answer that is not a lie: the person asked
    /// for one label with that name, and refusing would leave them with two that
    /// look identical. Every note on the renamed label gains the surviving one,
    /// and the renamed label is removed. It is also the repair for the one
    /// residue [`NoteCore::add_tag`] cannot prevent — two devices each inventing
    /// the same name offline — which nothing could repair before.
    ///
    /// Returns the label that survived: `label` itself, or the one it merged
    /// into.
    pub fn rename_tag(&mut self, label: LabelId, name: &str) -> Result<Option<LabelId>> {
        let Some(name) = account::normalise_tag(name) else {
            return Ok(None);
        };
        if self.placements.label_name(label).is_none() {
            return Ok(None);
        }
        match self.placements.label_named(&name) {
            // Already called this, up to case. Write the name anyway, so that
            // renaming `recipes` to `Recipes` does what it looks like.
            Some(existing) if existing == label => {
                self.apply_account(AccountEdit::SetLabelName { label, name })?;
                Ok(Some(label))
            }
            Some(existing) => {
                self.merge_tag(label, existing)?;
                Ok(Some(existing))
            }
            None => {
                self.apply_account(AccountEdit::SetLabelName { label, name })?;
                Ok(Some(label))
            }
        }
    }

    /// Give every note on `from` the label `into`, then remove `from`.
    fn merge_tag(&mut self, from: LabelId, into: LabelId) -> Result<()> {
        for note in self.notes_with_tag(from) {
            self.apply_account(AccountEdit::SetTag {
                note,
                label: into,
                on: true,
            })?;
        }
        self.forget_tag(from)
    }

    /// Remove a label: its name, and its membership of every note that has it.
    ///
    /// The notes are untouched apart from losing the label. Nothing here deletes
    /// a note, and this is the only thing in the core that takes a label away
    /// from notes the caller did not name.
    pub fn forget_tag(&mut self, label: LabelId) -> Result<()> {
        let notes = self.notes_with_tag(label);
        // The old-shape memberships name the label, so `ForgetLabel` cannot
        // reach them: it removes keys built from an ID.
        for note in &notes {
            let legacy = self.placement(*note).legacy_tags;
            for tag in legacy {
                if self.placements.label_named(&tag) == Some(label) {
                    self.apply_account(AccountEdit::DropLegacyTag { note: *note, tag })?;
                }
            }
        }
        self.apply_account(AccountEdit::ForgetLabel { label, notes })
    }

    /// Every note carrying a label, including through a membership the migration
    /// has not reached yet.
    fn notes_with_tag(&self, label: LabelId) -> Vec<NoteId> {
        self.placements
            .placements
            .keys()
            .copied()
            .filter(|note| self.placements.labels_on(*note).contains(&label))
            .collect()
    }

    /// Turn every membership still stored in the old shape into a real label.
    ///
    /// The old shape made a label out of its name — `t:<note>:<name>` and
    /// nothing else — so there is no ID to recover and one has to be minted.
    /// Names that match ignoring case land on the same label, which is what
    /// `add_tag` would have done had they been typed on this build.
    ///
    /// Runs at launch, writes nothing when there is nothing to do, and is
    /// idempotent: the old keys are removed in the same pass, so the second
    /// launch finds none. A crash halfway leaves some notes migrated and some
    /// not, and the next launch finishes the job — which is why
    /// [`AccountView::labels_on`] reads both shapes rather than assuming one.
    ///
    /// **By name, not by note.** When two notes carry the same label under
    /// different casings, whichever is adopted first decides what the label is
    /// called. Walking the notes would decide that by note ID, which is random,
    /// so two devices migrating the same document would land on different
    /// casings — and with no compaction to reconcile them, that difference would
    /// be permanent. Sorting the names first makes it the same answer
    /// everywhere: lexicographically first wins.
    fn adopt_legacy_tags(&mut self) -> Result<()> {
        let mut legacy: Vec<(NoteId, String)> = self
            .placements
            .placements
            .iter()
            .flat_map(|(note, placement)| {
                placement
                    .legacy_tags
                    .iter()
                    .map(move |tag| (*note, tag.clone()))
            })
            .collect();
        if legacy.is_empty() {
            return Ok(());
        }
        legacy.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
        for (note, tag) in legacy {
            let label = match self.placements.label_named(&tag) {
                Some(label) => label,
                None => self.make_tag(&tag)?,
            };
            self.apply_account(AccountEdit::SetTag {
                note,
                label,
                on: true,
            })?;
            self.apply_account(AccountEdit::DropLegacyTag { note, tag })?;
        }
        Ok(())
    }

    /// Put a note in the archive, or take it out. Archiving a pinned note
    /// unpins it, as Keep does: a note cannot be both out of the way and held at
    /// the top.
    pub fn set_archived(&mut self, id: NoteId, archived: bool) -> Result<()> {
        if archived && self.placement(id).pinned {
            self.set_pinned(id, false)?;
        }
        self.apply_account(AccountEdit::SetArchived { note: id, archived })
    }

    /// Throw a note away, or take it back out.
    ///
    /// Reversible, and nothing is destroyed: the content is still on the disk and
    /// still syncing, and what has changed is that this person does not see it.
    /// [`NoteCore::forget_note`] is the irreversible one, and the trash is what
    /// it is reached from.
    pub fn set_trashed(&mut self, id: NoteId, trashed: bool) -> Result<()> {
        if trashed && self.placement(id).pinned {
            self.set_pinned(id, false)?;
        }
        self.apply_account(AccountEdit::SetTrashed { note: id, trashed })
    }

    // -- forgetting a note --------------------------------------------------
    //
    // The one operation here that destroys something. Everything else in this
    // file can be undone by doing the opposite of it; this cannot be undone at
    // all, which is why the UI reaches it only from the trash and only behind a
    // confirmation. [ADR 0010](../../docs/adr/0010-forgetting-a-note.md) records
    // what it does, in what order, and what it cannot do.

    /// Forget a note for good: its log, its snapshot, its sealed index entry,
    /// its key, and this person's placement of it.
    ///
    /// **The order is the whole of the crash story**, and it is store, then key,
    /// then placement:
    ///
    /// * The store goes first because that is where the note *is* — the log, the
    ///   snapshot, and the index entry that holds its whole text. Each store does
    ///   that much atomically ([`OpStore::forget`]), so there is no moment at
    ///   which some of those three are gone and the rest are readable.
    /// * The key goes second, and cannot go in the same transaction because it is
    ///   not in the same database. A crash in between leaves 32 bytes that open
    ///   nothing that exists, which is the harmless way round; the other order
    ///   would leave the note's ciphertext with no way to read it — indistinguishable
    ///   from a note whose key never arrived, and so something the next launch
    ///   would keep rather than finish deleting.
    /// * The placement goes last, because a note with no placement keys reads
    ///   back as a note *in the grid* ([`AccountEdit::ForgetNote`]). Clearing it
    ///   before the note was gone would put the note the person just deleted back
    ///   on the shelf they deleted it from.
    ///
    /// **What it does not do** is reach anything that is not this device. A note
    /// that has been shared is on the relay and on every guest's device, sealed
    /// under a key they hold; forgetting it here removes this device's copy and
    /// nothing else, and there is no message that would remove theirs. That is
    /// revocation, it needs key rotation, and it is Slice 3's.
    ///
    /// Forgetting a note this device does not have is not an error, for the same
    /// reason [`OpStore::forget`] says so: it is what finishing an interrupted
    /// delete looks like.
    pub fn forget_note(&mut self, id: NoteId) -> Result<()> {
        if self.is_account(id) {
            // Not a note, and forgetting it would take this person's every
            // colour, pin and placement with it — under a reserved ID no caller
            // can have been handed by the note list.
            return Err(CoreError::UnknownNote(id.encode()));
        }
        // In memory first, so that nothing between here and the end of this
        // function can write back what is about to be removed: a reindex of a
        // still-open document would put the note's text in a fresh metadata row
        // after the store had forgotten the old one.
        self.open.remove(&id);
        self.index.remove(&id);
        self.store.forget(id)?;
        self.keyring.forget_note_key(id)?;
        // An invite to a note this device no longer has could never be
        // approved: there is no key left to seal into the grant. The relay's
        // copy expires on its own.
        let invites = self.invites()?;
        if invites.iter().any(|i| i.note == id) {
            let kept: Vec<_> = invites.into_iter().filter(|i| i.note != id).collect();
            self.put_invites(&kept)?;
        }
        // The labels have to be read before anything else is removed, because
        // they are the one part of a placement that cannot be named without
        // looking ([`AccountEdit::ForgetNote`]). Both shapes, because a device
        // can be forgetting a note on the same launch that migrates it.
        let placement = self.placement(id);
        let tags = placement.tags.into_iter().collect();
        let legacy_tags = placement.legacy_tags.into_iter().collect();
        let reminders = self
            .placements
            .reminders_on(id)
            .into_iter()
            .map(|reminder| reminder.id)
            .collect();
        self.apply_account(AccountEdit::ForgetNote {
            note: id,
            tags,
            legacy_tags,
            reminders,
        })?;
        // `apply_account` refreshes the list, but only when it had something to
        // write: a note nobody ever coloured, pinned or ordered produces an empty
        // update and returns early. The card still has to go.
        self.refresh_library();
        Ok(())
    }

    /// Every note this person has thrown away, which is what emptying the trash
    /// would forget.
    ///
    /// Separate from [`NoteCore::empty_trash`] because a caller may have work to
    /// do per note before the rows go: the app has a socket open for each of
    /// them, and an envelope arriving for a note that has just been forgotten
    /// would write a sync cursor — a store row, and a store row is a note.
    ///
    /// A trashed note whose key this device no longer has is not in this list,
    /// for the reason [`NoteCore::notes`] skips it: it cannot be read, drawn or
    /// searched, and what is left of it is ciphertext.
    pub fn trashed_notes(&mut self) -> Result<Vec<NoteId>> {
        Ok(self
            .notes()?
            .into_iter()
            .filter(|note| note.placement.trashed)
            .map(|note| note.id)
            .collect())
    }

    /// Forget every note in the trash. Returns how many there were.
    ///
    /// The list is taken before anything is forgotten, because
    /// [`NoteCore::notes`] rebuilds the index and the placements, and iterating a
    /// list while removing from it is how a note gets missed.
    pub fn empty_trash(&mut self) -> Result<usize> {
        let trashed = self.trashed_notes()?;
        for id in &trashed {
            self.forget_note(*id)?;
        }
        Ok(trashed.len())
    }

    /// Move a note so that it lands at `to_index` in the grid's order.
    ///
    /// The same translation [`NoteCore::move_item`] does for an item, against the
    /// same fractional indices: the UI thinks in positions and the document holds
    /// keys. Pinned notes are drawn first but are not ordered separately — a pin
    /// lifts a note out of the sequence and putting it back down returns it to
    /// where it was.
    pub fn move_note(&mut self, id: NoteId, to_index: usize) -> Result<()> {
        let keys = self.order_keys_without(id);
        let order = order::key_for_index(&keys, to_index.min(keys.len()));
        self.apply_account(AccountEdit::SetOrder { note: id, order })
    }

    /// Move `id` so that it takes `target`'s place in the grid's order.
    ///
    /// What a drag reports, because a drag lands on a card and not on a number.
    /// The screen showing the cards is filtered — a shelf, or a label — and the
    /// order is over every note this device has, so an index counted off what is
    /// on the screen would mean something else by the time it got here. A note is
    /// a note on both sides.
    ///
    /// "Takes its place" is the same rule a reorderable list has, and it depends
    /// on which way the drag went: a note dragged *downwards* onto a card lands
    /// after it and a note dragged upwards lands before it, so that the card you
    /// dropped on moves out of the way in the direction you came from. Dropping a
    /// note on itself, or on one this device has no placement for, changes
    /// nothing.
    pub fn move_note_to(&mut self, id: NoteId, target: NoteId) -> Result<()> {
        if id == target {
            return Ok(());
        }
        let mine = self.placement(id).order;
        let theirs = self.placement(target).order;
        if theirs.is_empty() {
            return Ok(());
        }
        let keys = self.order_keys_without(id);
        // `partition_point`, not `position`: the keys are sorted and deduplicated,
        // so this is the slot `target` occupies whether or not anything shares its
        // key.
        let at = keys.partition_point(|key| *key < theirs);
        let to = if mine < theirs { at + 1 } else { at };
        let order = order::key_for_index(&keys, to.min(keys.len()));
        self.apply_account(AccountEdit::SetOrder { note: id, order })
    }

    /// A view setting, by name. [`LAYOUT`] is the only one so far.
    pub fn setting(&self, name: &str) -> Option<&str> {
        self.placements.setting(name)
    }

    pub fn set_setting(&mut self, name: impl Into<String>, value: impl Into<String>) -> Result<()> {
        self.apply_account(AccountEdit::SetSetting {
            name: name.into(),
            value: value.into(),
        })
    }

    /// Whether the note list is drawn as a grid or as one column.
    pub fn layout(&self) -> Layout {
        self.setting(LAYOUT)
            .map_or(Layout::default(), Layout::from_name)
    }

    pub fn set_layout(&mut self, layout: Layout) -> Result<()> {
        self.set_setting(LAYOUT, layout.name())
    }

    /// Apply one change to the account document: seal it, log it, and tell the
    /// list.
    ///
    /// The same path a note edit takes, minus the return value. A note hands its
    /// envelope back so the bridge can publish it; this document has twenty
    /// entry points rather than six, several of which write more than once, so
    /// it pushes to [`AccountEnvelopeSink`] instead. Either way the envelope is
    /// in the log before it is offered to anybody
    /// ([ADR 0004](../../docs/adr/0004-sync-engine.md)): the relay hearing about
    /// it is a separate thing from it being safe.
    fn apply_account(&mut self, edit: AccountEdit) -> Result<()> {
        let update = self.account.doc.apply_local(&edit)?;
        self.seal_account_update(update)
    }

    /// Seal one account-document update into the log and refresh what is drawn
    /// from it.
    fn seal_account_update(&mut self, update: Vec<u8>) -> Result<()> {
        if update.is_empty() {
            // A write that changed nothing — clearing a colour that was never
            // set. Sealing it would put an envelope in the log that says nothing.
            return Ok(());
        }
        let counter = self.account.next_counter;
        self.account.next_counter += 1;
        let envelope = Envelope::seal(
            &self.identity,
            self.account_id,
            &self.account.key,
            counter,
            &update,
        )?;
        self.account.seen.insert((envelope.author, counter));
        let encoded = envelope.encode();
        self.store.append(self.account_id, &encoded)?;
        self.placements = self.account.doc.view();
        self.refresh_library();
        // Last, and after the append: a sink that publishes to a relay is
        // offering an edit that is already kept. An account this device does not
        // have has no relay connection to reach, so the sink is handed the
        // reserved ID and finds nothing listening under it.
        if let Some(sink) = &self.account_sink {
            sink(self.account_id, &encoded);
        }
        Ok(())
    }

    /// Say where local account-document envelopes should go as they are made.
    ///
    /// The one call a device makes that a test does not: without it the document
    /// is still written, still read and still correct, and simply never leaves
    /// the device.
    pub fn on_account_envelope(&mut self, sink: AccountEnvelopeSink) {
        self.account_sink = Some(sink);
    }

    /// Every key the account document holds, for a test that needs to check the
    /// document itself rather than the view built from it — the note-key
    /// registers in particular, which `AccountView` deliberately does not carry.
    #[doc(hidden)]
    pub fn account_entries(&self) -> Vec<(String, crate::crdt::account::Value)> {
        self.account.doc.entries()
    }

    /// Write a membership in the shape a label used to have, so that a test can
    /// stand a device up in the state [`NoteCore::adopt_legacy_tags`] exists
    /// for. There is no other way to produce one now, which is the point.
    #[doc(hidden)]
    pub fn write_legacy_tag(&mut self, note: NoteId, tag: &str) -> Result<()> {
        let update = self
            .account
            .doc
            .write(&[account::legacy_tag_write(note, tag)])?;
        self.seal_account_update(update)
    }

    /// Every order key in use, ascending.
    fn order_keys(&self) -> Vec<String> {
        self.order_keys_except(None)
    }

    /// Every order key in use except the one `id` holds, ascending.
    ///
    /// What both moves compute their target slot against: a slot is a gap in the
    /// list the note is *not* in, so that "move to index 3" means the same thing
    /// whether the note came from above or below.
    fn order_keys_without(&self, id: NoteId) -> Vec<String> {
        self.order_keys_except(Some(id))
    }

    /// Deduplicated, which matters for both callers and not for display: two
    /// devices placing a note at the same moment can write the same key, and
    /// [`order::key_for_index`] asks for a key strictly between two neighbours —
    /// which does not exist between a key and itself. The list decides the order
    /// of such a pair by ID ([`NoteCore::refresh_library`]); a slot between them
    /// is not a thing anyone can point at.
    fn order_keys_except(&self, skip: Option<NoteId>) -> Vec<String> {
        let mut keys: Vec<String> = self
            .placements
            .placements
            .iter()
            .filter(|(note, _)| Some(**note) != skip)
            .map(|(_, placement)| placement.order.clone())
            .filter(|key| !key.is_empty())
            .collect();
        keys.sort();
        keys.dedup();
        keys
    }

    /// Give a note a place at `index` in the grid's order.
    fn place(&mut self, id: NoteId, index: usize) -> Result<()> {
        let keys = self.order_keys();
        let order = order::key_for_index(&keys, index.min(keys.len()));
        self.apply_account(AccountEdit::SetOrder { note: id, order })
    }

    /// Give a placement to every note in the store that has none.
    ///
    /// This is the migration off [`ADDED`], and it is not only a one-off: a note
    /// can arrive in the store without going through `create_note` or
    /// `import_share_blob` — [`crate::store::copy_store`] moves a whole device —
    /// and a note with no order key would sit at the top of the grid until
    /// something gave it one.
    ///
    /// They go to the bottom, newest of them first, which for a device upgrading
    /// into the account document is exactly the grid it had: every note is
    /// unplaced, so "the bottom" is the whole list and the ordinal it was made
    /// with decides the order.
    fn adopt_unplaced(&mut self) -> Result<()> {
        let mut unplaced: Vec<(u64, NoteId)> = Vec::new();
        for id in self.store.notes()? {
            if self.is_account(id) || self.placements.placements.contains_key(&id) {
                continue;
            }
            // A note this device has no key for gets no placement, for the same
            // reason `notes` gives it no card: there is nothing to show and
            // nothing that could be shown. It is also what keeps a forgotten note
            // forgotten — a late-arriving envelope can leave a sync cursor behind
            // for one, and a placement written from that row would be a permanent
            // entry in a document nothing compacts, about a note that does not
            // exist.
            if self.keyring.note_key(id)?.is_none() {
                self.sweep_if_residue(id)?;
                continue;
            }
            unplaced.push((self.ordinal(id)?, id));
        }
        // Newest first, and the ID to break a tie between two notes made before
        // anything recorded an ordinal.
        unplaced.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        for (_, id) in unplaced {
            self.place(id, usize::MAX)?;
        }
        Ok(())
    }

    /// Remove what a forgotten note can leave behind, and nothing else.
    ///
    /// Forgetting a note closes its relay connection before taking the rows, but
    /// the sync engine's task may already be mid-delivery: an envelope that
    /// arrives in that window is rejected by `apply_remote` — there is no key to
    /// open it with — and the engine moves the cursor past it anyway, because a
    /// cursor that sticks on an envelope it cannot use would stall the note for
    /// ever. Writing that cursor recreates a store row for a note that has just
    /// been deleted. What is left is a sealed cursor, no log, no snapshot, no key
    /// and no placement: nothing readable, but a row that `store.notes` returns
    /// and that nothing has ever removed.
    ///
    /// The test of residue is **ciphertext, not the key**. A note with a log or a
    /// snapshot is left exactly where it is even though this device cannot read
    /// it: `copy_store` can carry a log whose key never arrived, and Slice 3's
    /// recovery phrase is a key arriving later for a log that is still here.
    /// Sweeping on a missing key alone would delete the note that feature is
    /// meant to give back. A row with no ciphertext under it cannot be given back
    /// by anything, because there is nothing left to decrypt.
    fn sweep_if_residue(&mut self, id: NoteId) -> Result<()> {
        if !self.store.load(id)?.is_empty() || self.store.get_snapshot(id)?.is_some() {
            return Ok(());
        }
        self.store.forget(id)
    }

    // -- editing ------------------------------------------------------------

    /// Apply a local edit. Returns the sealed envelope to hand to the relay.
    pub fn apply_local(&mut self, id: NoteId, edit: Edit) -> Result<Vec<u8>> {
        self.ensure_open(id)?;
        let identity_ref = &self.identity;
        let note = self
            .open
            .get_mut(&id)
            .ok_or_else(|| CoreError::UnknownNote(id.encode()))?;

        let update = note.doc.apply_local(&edit)?;
        let counter = note.next_counter;
        note.next_counter += 1;

        let envelope = Envelope::seal(identity_ref, id, &note.key, counter, &update)?;
        note.seen.insert((envelope.author, counter));
        let encoded = envelope.encode();

        self.store.append(id, &encoded)?;
        self.store
            .put_meta(id, NEXT_COUNTER, &note.next_counter.to_be_bytes())?;
        let note = self.open.get_mut(&id).expect("just borrowed");
        let _ = note.watch.send_replace(note.doc.view());
        self.reindex(id)?;
        self.refresh_library();
        Ok(encoded)
    }

    /// Apply an envelope that arrived from the relay.
    ///
    /// Unknown, tampered, replayed and misrouted envelopes are all rejected here,
    /// before the CRDT sees anything.
    pub fn apply_remote(&mut self, id: NoteId, envelope: &[u8]) -> Result<NoteView> {
        let env = Envelope::decode(envelope)?;
        if env.note_id != id {
            return Err(CoreError::WrongNote {
                expected: id.encode(),
                got: env.note_id.encode(),
            });
        }
        // An arrival is the other thing that can be the first to need a note's
        // document: the list no longer opens every note, so a peer's edit to a
        // note nobody has looked at today opens it here.
        self.ensure_open(id)?;
        let current_key_id = self
            .open
            .get(&id)
            .ok_or_else(|| CoreError::UnknownNote(id.encode()))?
            .key
            .id();
        let access = account::note_access_of(self.account.doc.entries())
            .remove(&id)
            .ok_or_else(|| CoreError::UnknownNote(id.encode()))?;
        if env.kind == EnvelopeKind::Snapshot && access.owner != Some(env.author) {
            return Err(CoreError::Crypto("an unauthorised snapshot was refused"));
        }
        if env.key_id.is_some_and(|key_id| key_id != current_key_id) {
            env.verify()?;
            let old = self
                .open
                .get(&id)
                .expect("the note was opened above")
                .doc
                .view();
            // Relay history from an older generation is expected after a
            // rotation (and when somebody is invited again). It cannot decrypt
            // or mutate the current document, so advancing past it is correct.
            // Only a creator-signed snapshot is a cutover notice.
            if env.kind != EnvelopeKind::Snapshot {
                return Ok(old);
            }
            if access.owned {
                // The creator's other devices learn the key through their
                // account document. Keep the snapshot until that register
                // arrives, then `adopt_note_keys` reopens it under the new key.
                self.store.append(id, envelope)?;
            } else {
                // A correctly signed new-key snapshot is the revocation notice.
                // It contains no new key, so the former member cannot follow it.
                self.forget_note(id)?;
            }
            return Ok(old);
        }
        let note = self
            .open
            .get_mut(&id)
            .ok_or_else(|| CoreError::UnknownNote(id.encode()))?;

        // `open` verifies the signature before it decrypts.
        let plain = env.open(&note.key)?;

        if !note.seen.insert((env.author, env.counter)) {
            // Already applied. Persisting it again would grow the log for nothing.
            return Ok(note.doc.view());
        }
        if env.kind == EnvelopeKind::Snapshot {
            note.doc = D::load(&self.actor, &plain)?;
        } else if env.kind == EnvelopeKind::Update {
            note.doc.apply_remote(&plain)?;
        }
        self.store.append(id, envelope)?;

        let note = self.open.get_mut(&id).expect("just borrowed");
        let view = note.doc.view();
        let _ = note.watch.send_replace(view.clone());
        self.reindex(id)?;
        self.refresh_library();
        Ok(view)
    }

    // -- index-level helpers ------------------------------------------------
    //
    // The UI thinks in positions; the CRDT thinks in order keys. The translation
    // lives here so no other layer has to know about fractional indices.

    pub fn set_title(&mut self, id: NoteId, title: impl Into<String>) -> Result<Vec<u8>> {
        self.apply_local(id, Edit::SetTitle(title.into()))
    }

    /// Replace the body's text, or `None` if it already says exactly that.
    ///
    /// `None` rather than an empty envelope because an edit that changes nothing
    /// produces no update to seal, and a screen that sends its text on every
    /// change will sometimes send the text that is already there.
    pub fn set_body(&mut self, id: NoteId, text: impl Into<String>) -> Result<Option<Vec<u8>>> {
        self.ensure_open(id)?;
        let text = text.into();
        if self.view(id)?.body == text {
            return Ok(None);
        }
        self.apply_local(id, Edit::SetBody(text)).map(Some)
    }

    /// Record that this person changed the body from `from` to `to`, or `None`
    /// if that leaves it as it is.
    ///
    /// What a screen calls, rather than [`NoteCore::set_body`]: a screen knows
    /// what it was showing, not what the body says by the time the call lands,
    /// and a peer's edit may have arrived in between. Writing `to` over that
    /// would delete the peer's words, so the edit is replayed onto the current
    /// body instead — see [`crate::text::rebase`].
    pub fn edit_body(&mut self, id: NoteId, from: &str, to: &str) -> Result<Option<Vec<u8>>> {
        self.ensure_open(id)?;
        let current = self.view(id)?.body;
        let body = crate::text::rebase(from, to, &current);
        self.set_body(id, body)
    }

    /// Switch a note between text and checklist, carrying its content across —
    /// the button Keep calls "show checkboxes". `None` if it is that kind already.
    ///
    /// To a checklist, every line of the body that has anything on it becomes an
    /// unchecked item, after whatever items the note already had. To text, every
    /// item becomes a line, checked or not, after whatever the body already said —
    /// ticks are the one thing that does not survive the trip, as in Keep.
    /// "Already had" is not hypothetical: a merge can leave content on the side
    /// that was emptied (see [`NoteView`]), and converting is what tidies it up
    /// rather than what throws it away.
    ///
    /// Either way it is one edit and one envelope, so a peer sees the note switch
    /// rather than half of it.
    pub fn set_kind(&mut self, id: NoteId, kind: NoteKind) -> Result<Option<Vec<u8>>> {
        self.ensure_open(id)?;
        let view = self.view(id)?;
        if view.kind == kind {
            return Ok(None);
        }
        let edit = match kind {
            NoteKind::Checklist => {
                let mut keys = view.order_keys();
                let mut items = Vec::new();
                for line in view.body.lines().filter(|l| !l.trim().is_empty()) {
                    let order = order::key_for_index(&keys, keys.len());
                    keys.push(order.clone());
                    items.push(NewItem {
                        id: ItemId::random(),
                        text: line.to_owned(),
                        order,
                    });
                }
                Edit::ToChecklist { items }
            }
            NoteKind::Text => {
                let mut lines: Vec<&str> = Vec::new();
                let kept = view.body.trim_end_matches('\n');
                if !kept.is_empty() {
                    lines.push(kept);
                }
                lines.extend(view.items.iter().map(|i| i.text.as_str()));
                Edit::ToText {
                    body: lines.join("\n"),
                    remove: view.items.iter().map(|i| i.id).collect(),
                }
            }
        };
        self.apply_local(id, edit).map(Some)
    }

    /// Append an item and return `(item id, envelope)`.
    pub fn add_item(&mut self, id: NoteId, text: impl Into<String>) -> Result<(ItemId, Vec<u8>)> {
        self.ensure_open(id)?;
        let keys = self.view(id)?.order_keys();
        self.insert_item_at(id, text, keys.len())
    }

    pub fn insert_item_at(
        &mut self,
        id: NoteId,
        text: impl Into<String>,
        index: usize,
    ) -> Result<(ItemId, Vec<u8>)> {
        self.ensure_open(id)?;
        let keys = self.view(id)?.order_keys();
        let item = ItemId::random();
        let order = order::key_for_index(&keys, index.min(keys.len()));
        let env = self.apply_local(
            id,
            Edit::AddItem {
                id: item,
                text: text.into(),
                order,
            },
        )?;
        Ok((item, env))
    }

    pub fn set_item_text(
        &mut self,
        id: NoteId,
        item: ItemId,
        text: impl Into<String>,
    ) -> Result<Vec<u8>> {
        self.require_item(id, item)?;
        self.apply_local(
            id,
            Edit::SetItemText {
                id: item,
                text: text.into(),
            },
        )
    }

    pub fn set_checked(&mut self, id: NoteId, item: ItemId, checked: bool) -> Result<Vec<u8>> {
        self.require_item(id, item)?;
        self.apply_local(id, Edit::SetChecked { id: item, checked })
    }

    pub fn remove_item(&mut self, id: NoteId, item: ItemId) -> Result<Vec<u8>> {
        self.require_item(id, item)?;
        self.apply_local(id, Edit::RemoveItem { id: item })
    }

    /// Move `item` so that it lands at `to_index` in the current display order.
    pub fn move_item(&mut self, id: NoteId, item: ItemId, to_index: usize) -> Result<Vec<u8>> {
        self.ensure_open(id)?;
        let view = self.view(id)?;
        let from = view
            .items
            .iter()
            .position(|i| i.id == item)
            .ok_or_else(|| CoreError::UnknownItem(item.encode()))?;

        // Compute the target slot against the list with the item taken out, so
        // "move to index 3" means the same thing whether it moves up or down.
        let mut keys = view.order_keys();
        keys.remove(from);
        let order = order::key_for_index(&keys, to_index.min(keys.len()));
        self.apply_local(id, Edit::MoveItem { id: item, order })
    }

    fn require_item(&mut self, id: NoteId, item: ItemId) -> Result<()> {
        self.ensure_open(id)?;
        if self.view(id)?.item(item).is_none() {
            return Err(CoreError::UnknownItem(item.encode()));
        }
        Ok(())
    }

    // -- sharing ------------------------------------------------------------

    /// The blob device B scans or pastes. Slice 3 moves this into an invite link's
    /// fragment without changing the bytes.
    pub fn export_share_blob(&self, id: NoteId) -> Result<String> {
        let note = self.note(id)?;
        Ok(crate::crypto::export_share_blob(&id, &note.key))
    }

    /// Accept a share blob and open an empty document for it. The content arrives
    /// as updates from the relay.
    pub fn import_share_blob(&mut self, blob: &str) -> Result<NoteId> {
        let (id, key) = crate::crypto::import_share_blob(blob)?;
        self.add_shared_note(id, key)
    }

    // -- invites ------------------------------------------------------------
    //
    // The bookkeeping half. Talking to the relay is `crate::sync::invite`'s and
    // deciding when is the app's; what is here is what has to survive the app
    // being closed while somebody decides whether to let someone in.

    /// Everything the relay needs to open an invite for `note`, freshly minted.
    /// Nothing is written until [`NoteCore::remember_invite`], so an invite the
    /// relay refused leaves no row behind.
    pub fn new_invite(&self, note: NoteId) -> Result<OutgoingInvite> {
        if self.is_account(note) || self.keyring.note_key(note)?.is_none() {
            return Err(CoreError::UnknownNote(note.encode()));
        }
        let access = account::note_access_of(self.account.doc.entries())
            .remove(&note)
            .ok_or_else(|| CoreError::UnknownNote(note.encode()))?;
        if !access.owned || access.owner.is_none() {
            return Err(CoreError::Crypto("only the owner can invite collaborators"));
        }
        Ok(OutgoingInvite {
            note,
            secret: crate::invite::InviteSecret::random(),
            owner: crate::invite::OwnerSecret::random(),
            expires_at: 0,
        })
    }

    /// Every invite this device has open, oldest first.
    pub fn invites(&self) -> Result<Vec<OutgoingInvite>> {
        let Some(raw) = self.store.get_meta(ACCOUNT_DOC, INVITES)? else {
            return Ok(Vec::new());
        };
        let stored: Vec<StoredInvite> = serde_json::from_slice(&raw)
            .map_err(|e| CoreError::Storage(format!("unreadable invites row: {e}")))?;
        stored
            .into_iter()
            .map(|s| {
                Ok(OutgoingInvite {
                    note: NoteId::decode(&s.note)?,
                    secret: crate::invite::InviteSecret::from_link(&s.secret)?,
                    owner: crate::invite::OwnerSecret::decode(&s.owner)?,
                    expires_at: s.expires_at,
                })
            })
            .collect()
    }

    fn put_invites(&mut self, invites: &[OutgoingInvite]) -> Result<()> {
        let stored: Vec<StoredInvite> = invites
            .iter()
            .map(|i| StoredInvite {
                note: i.note.encode(),
                secret: i.secret.code(),
                owner: i.owner.encode(),
                expires_at: i.expires_at,
            })
            .collect();
        let raw = serde_json::to_vec(&stored)
            .map_err(|e| CoreError::Storage(format!("unwritable invites row: {e}")))?;
        self.store.put_meta(ACCOUNT_DOC, INVITES, &raw)
    }

    /// Keep an invite the relay has accepted, replacing one with the same ID.
    pub fn remember_invite(&mut self, invite: &OutgoingInvite) -> Result<()> {
        let id = invite.secret.id();
        let mut all = self.invites()?;
        all.retain(|i| i.secret.id() != id);
        all.push(invite.clone());
        self.put_invites(&all)
    }

    /// Stop keeping an invite: answered, withdrawn or expired. Not an error if
    /// it is not kept.
    pub fn forget_invite(&mut self, id: crate::ids::InviteId) -> Result<()> {
        let mut all = self.invites()?;
        let before = all.len();
        all.retain(|i| i.secret.id() != id);
        if all.len() == before {
            return Ok(());
        }
        self.put_invites(&all)
    }

    /// Seal `note` for the guest who made `request`. The note key goes into
    /// the grant and nowhere else: it never leaves the core in the clear.
    pub fn seal_grant(
        &self,
        invite: &OutgoingInvite,
        request: &crate::invite::JoinRequest,
    ) -> Result<Vec<u8>> {
        let key = self
            .keyring
            .note_key(invite.note)?
            .ok_or_else(|| CoreError::UnknownNote(invite.note.encode()))?;
        let access = account::note_access_of(self.account.doc.entries())
            .remove(&invite.note)
            .ok_or_else(|| CoreError::UnknownNote(invite.note.encode()))?;
        if !access.owned || access.owner.is_none() {
            return Err(CoreError::Crypto("only the owner can invite collaborators"));
        }
        crate::invite::seal_owned_grant(
            &invite.secret,
            request,
            invite.note,
            &key,
            access.owner.expect("checked above"),
            access.generation,
        )
    }

    /// Every join request this device is waiting on, oldest first.
    pub fn joins(&self) -> Result<Vec<PendingJoin>> {
        let Some(raw) = self.store.get_meta(ACCOUNT_DOC, JOINS)? else {
            return Ok(Vec::new());
        };
        let stored: Vec<StoredJoin> = serde_json::from_slice(&raw)
            .map_err(|e| CoreError::Storage(format!("unreadable joins row: {e}")))?;
        stored
            .into_iter()
            .map(|s| {
                Ok(PendingJoin {
                    secret: crate::invite::InviteSecret::from_link(&s.secret)?,
                    request: s.request,
                    guest: crate::invite::GuestKey::decode(&s.guest)?,
                    name: s.name,
                })
            })
            .collect()
    }

    fn put_joins(&mut self, joins: &[PendingJoin]) -> Result<()> {
        let stored: Vec<StoredJoin> = joins
            .iter()
            .map(|j| StoredJoin {
                secret: j.secret.code(),
                request: j.request.clone(),
                guest: j.guest.encode(),
                name: j.name.clone(),
            })
            .collect();
        let raw = serde_json::to_vec(&stored)
            .map_err(|e| CoreError::Storage(format!("unwritable joins row: {e}")))?;
        self.store.put_meta(ACCOUNT_DOC, JOINS, &raw)
    }

    /// Keep a request the relay has accepted, replacing any earlier one on the
    /// same invite: a person who asks twice is waiting on the second answer.
    pub fn remember_join(&mut self, join: &PendingJoin) -> Result<()> {
        let id = join.secret.id();
        let mut all = self.joins()?;
        all.retain(|j| j.secret.id() != id);
        all.push(join.clone());
        self.put_joins(&all)
    }

    /// Stop waiting on the request made on invite `id`.
    pub fn forget_join(&mut self, id: crate::ids::InviteId) -> Result<()> {
        let mut all = self.joins()?;
        let before = all.len();
        all.retain(|j| j.secret.id() != id);
        if all.len() == before {
            return Ok(());
        }
        self.put_joins(&all)
    }

    /// A grant opened: add the note it named and stop waiting for it.
    pub fn accept_grant(
        &mut self,
        invite: crate::ids::InviteId,
        note: NoteId,
        key: NoteKey,
    ) -> Result<NoteId> {
        let id = self.add_shared_note(note, key)?;
        self.forget_join(invite)?;
        Ok(id)
    }

    pub fn accept_owned_grant(
        &mut self,
        invite: crate::ids::InviteId,
        note: NoteId,
        key: NoteKey,
        owner: Option<AuthorKey>,
        generation: u64,
    ) -> Result<NoteId> {
        let id = self.add_shared_note_with_owner(note, key, owner, generation)?;
        self.forget_join(invite)?;
        Ok(id)
    }

    /// Add a note somebody else holds the key to, by its ID and key — however
    /// they arrived.
    fn add_shared_note(&mut self, id: NoteId, key: NoteKey) -> Result<NoteId> {
        self.add_shared_note_with_owner(id, key, None, 0)
    }

    fn add_shared_note_with_owner(
        &mut self,
        id: NoteId,
        key: NoteKey,
        owner: Option<AuthorKey>,
        generation: u64,
    ) -> Result<NoteId> {
        if self.is_account(id) {
            // Nobody shares the account document, and a blob that claims to would
            // put a key of someone else's choosing where this device's placements
            // are sealed with its own — which is to say, throw them away.
            return Err(CoreError::MalformedShareBlob(
                "the account document is not a note",
            ));
        }
        if self.open.contains_key(&id) {
            return Ok(id);
        }
        self.keyring.put_note_key(id, &key)?;
        self.store
            .put_meta(id, KEY_GENERATION, &generation.to_be_bytes())?;
        // And into the account document, so that a note somebody shared with
        // this person is a note they can open on their other devices too. A
        // shared note is still one of their notes; the key travels the same way
        // one they made themselves does.
        let recorded = owner.map_or_else(
            || account::encode_note_key(&key),
            |owner| account::encode_note_access(&key, owner, false, generation, None),
        );
        self.apply_account(account::AccountEdit::SetNoteKey {
            note: id,
            key: recorded,
        })?;
        if self.ordinal(id)? == 0 {
            let next = self.next_ordinal()?;
            self.store.put_meta(id, ADDED, &next.to_be_bytes())?;
        }
        // Opening rather than starting an empty document, because a second join
        // is as ordinary as a first: this device may already hold the note's
        // whole log, from before it was ever shared or from a previous join, and
        // an empty document would put a blank card over it until the relay
        // happened to send it all again. On a first join there is nothing there
        // and this is the empty document, at the cost of one store read.
        self.open_note(id)?;
        // A note this device has joined before keeps the place it already had, so
        // re-pasting a blob does not move someone else's list to the top.
        if self.placements.placements.contains_key(&id) {
            self.refresh_library();
        } else {
            self.place(id, 0)?;
        }
        Ok(id)
    }

    // -- persistence --------------------------------------------------------

    /// Write an encrypted snapshot. Slice 2 uses this to compact the log; Slice 1
    /// writes it so that `open_note` exercises the snapshot path.
    pub fn save_snapshot(&mut self, id: NoteId) -> Result<()> {
        self.ensure_open(id)?;
        let identity_ref = &self.identity;
        let note = self
            .open
            .get_mut(&id)
            .ok_or_else(|| CoreError::UnknownNote(id.encode()))?;
        let blob = note.doc.snapshot();
        let counter = note.next_counter;
        note.next_counter += 1;
        let env = Envelope::seal_kind(
            identity_ref,
            id,
            &note.key,
            counter,
            EnvelopeKind::Snapshot,
            &blob,
        )?;
        self.store
            .put_meta(id, NEXT_COUNTER, &note.next_counter.to_be_bytes())?;
        self.store.put_snapshot(id, &env.encode())
    }

    /// Remove every collaborator from a note while keeping this person's own
    /// account devices on it.
    ///
    /// The creator writes one full-document snapshot under a fresh key. The
    /// fresh key travels only through the creator's encrypted account document;
    /// a guest sees the creator-signed change of key, removes its local copy,
    /// and cannot decrypt this snapshot or anything written after it. A copy
    /// they decrypted before this call is necessarily beyond recall.
    ///
    /// Returns the snapshot envelope for the sync layer to publish. It is in
    /// the local log before this method returns, so an offline revocation is
    /// retried like any other edit.
    pub fn revoke_collaborators(&mut self, id: NoteId) -> Result<Vec<u8>> {
        self.ensure_open(id)?;
        let access = account::note_access_of(self.account.doc.entries())
            .remove(&id)
            .ok_or_else(|| CoreError::UnknownNote(id.encode()))?;
        let Some(rotation_seed) = access.rotation_seed.as_ref() else {
            return Err(CoreError::Crypto(
                "only the owner can revoke access to this note",
            ));
        };
        let rotation_identity = DeviceIdentity::from_seed(rotation_seed);
        if access.owner != Some(rotation_identity.public_key()) {
            return Err(CoreError::Crypto(
                "the note's revocation identity is invalid",
            ));
        }

        let note = self
            .open
            .get_mut(&id)
            .ok_or_else(|| CoreError::UnknownNote(id.encode()))?;
        let new_key = NoteKey::random();
        let generation = access.generation.saturating_add(1);
        let envelope = Envelope::seal_kind(
            &rotation_identity,
            id,
            &new_key,
            generation,
            EnvelopeKind::Snapshot,
            &note.doc.snapshot(),
        )?;
        let encoded = envelope.encode();
        self.store
            .put_meta(id, NEXT_COUNTER, &note.next_counter.to_be_bytes())?;

        // The snapshot is both the complete new-generation log and the local
        // fast-open snapshot. Neither old-key row survives on this device.
        self.store.replace_log(id, std::slice::from_ref(&encoded))?;
        self.store.put_snapshot(id, &encoded)?;
        self.keyring.put_note_key(id, &new_key)?;
        self.store
            .put_meta(id, KEY_GENERATION, &generation.to_be_bytes())?;
        note.key = new_key.clone();
        note.seen.clear();
        note.seen.insert((envelope.author, envelope.counter));

        // The account update is emitted before the note envelope is returned,
        // so another device normally learns the key before it sees the snapshot.
        // Receiving them in the other order is supported too: it keeps the
        // unread snapshot until `adopt_note_keys` reopens it.
        self.apply_account(AccountEdit::SetNoteKey {
            note: id,
            key: account::encode_note_access(
                &new_key,
                envelope.author,
                true,
                generation,
                Some(rotation_seed),
            ),
        })?;
        self.reindex(id)?;
        self.refresh_library();
        Ok(encoded)
    }

    /// Replace a note's log with a snapshot plus whatever the relay has not
    /// acknowledged. Returns how many envelopes were dropped.
    ///
    /// **What compaction may not drop** is the outbox. [ADR 0004](../../docs/adr/0004-sync-engine.md)
    /// derives it from the log rather than keeping it beside one, so this
    /// device's own envelopes are the only copy of any edit the relay has not
    /// confirmed. Two cases follow from that, and both are the same rule:
    ///
    /// * A note that syncs loses its peers' envelopes and its own acknowledged
    ///   ones, which is nearly all of them.
    /// * A note that has never reached a relay keeps its whole log, because that
    ///   log *is* how its content would reach one — and how a guest joining later
    ///   would ever see it. There is no path that sends a snapshot instead.
    ///
    /// The snapshot is written before the log is shortened. A crash in between
    /// leaves a note that replays a log it already has a snapshot of, which costs
    /// milliseconds and changes nothing; the other order would lose edits.
    pub fn compact(&mut self, id: NoteId) -> Result<usize> {
        // Up front, because a snapshot is written from the document and the note
        // list no longer leaves every document open — and so that compacting a
        // note this device has no key for fails the same way whether or not
        // there turns out to be anything to drop.
        self.ensure_open(id)?;

        let sent_upto = self.sync_cursor(id)?.sent_upto;
        let me = self.identity.public_key();

        let log = self.store.load(id)?;
        let before = log.len();
        let mut keep = Vec::with_capacity(before);
        for raw in log {
            let env = Envelope::decode(&raw)?;
            if env.author == me && sent_upto.is_none_or(|sent| env.counter > sent) {
                keep.push(raw);
            }
        }

        let dropped = before - keep.len();
        if dropped == 0 {
            // Nothing to gain, and a snapshot of a document that is already
            // wholly in its own log is work for its own sake.
            return Ok(0);
        }

        self.save_snapshot(id)?;
        self.store.replace_log(id, &keep)?;
        // The document is unchanged and its log is not, so the entry that was
        // right a moment ago now fails its length check. Rewriting it here is
        // what stops the next launch from reopening every note this one
        // compacted.
        self.reindex(id)?;
        Ok(dropped)
    }

    /// Replace the account document's log with one envelope holding the whole
    /// document. Returns how many envelopes were dropped.
    ///
    /// This is [`NoteCore::compact`] for the document that is not a note, and it
    /// can drop *everything* where a note's compaction may drop almost nothing.
    /// [ADR 0006](../../docs/adr/0006-compaction.md) forbids a never-synced note
    /// to shorten its log because the log is the only route its content has to a
    /// relay or a guest — a snapshot sits in a store row nothing sends. The rule
    /// is kept here and the premise is not: the snapshot goes into the log, as an
    /// ordinary sealed envelope with a counter above every one this device has
    /// used, so the outbox [ADR 0004](../../docs/adr/0004-sync-engine.md) derives
    /// from the log still holds the whole document. What a relay would be told is
    /// the same account, in one envelope instead of hundreds.
    /// [ADR 0012](../../docs/adr/0012-compacting-the-account-document.md) has the
    /// argument and what it rests on.
    ///
    /// One store call rather than two, because the snapshot *is* the new log:
    /// `replace_log` is atomic, so a crash leaves either the log this was
    /// replaying or the envelope that says the same thing, and never the gap
    /// between them that a note's snapshot-then-truncate has to be ordered
    /// around.
    pub fn compact_account(&mut self) -> Result<usize> {
        let before = self.store.log_len(self.account_id)?;
        if before <= 1 {
            // One envelope is already the shortest this log gets, and rewriting
            // it as a snapshot of itself is work for its own sake.
            return Ok(0);
        }
        let blob = self.account.doc.snapshot();
        let counter = self.account.next_counter;
        self.account.next_counter += 1;
        let envelope = Envelope::seal(
            &self.identity,
            self.account_id,
            &self.account.key,
            counter,
            &blob,
        )?;
        self.account.seen.insert((envelope.author, counter));
        self.store
            .replace_log(self.account_id, &[envelope.encode()])?;
        Ok(before)
    }

    /// Re-send the current view to everyone watching, without changing anything.
    ///
    /// This exists so a watcher blocked on the channel can be woken and told to
    /// stop. Without it a watcher parks until the next edit, which may never
    /// come — and anything waiting for it to finish parks too.
    pub fn touch(&self, id: NoteId) -> Result<()> {
        let note = self.note(id)?;
        let _ = note.watch.send_replace(note.doc.view());
        Ok(())
    }

    /// The same wake for whoever is watching the note list.
    pub fn touch_notes(&self) {
        self.refresh_library();
    }

    // -- sync bookkeeping ---------------------------------------------------
    //
    // The sync engine drives these; they are here rather than in the engine
    // because the store is here, and because they have to work for a note that
    // is not open — a relaunch reads the cursor before it decides what to ask
    // the relay for.

    /// Every note this device should be talking to a relay about.
    ///
    /// Not [`NoteCore::notes`], which is the *grid*: a card is built from an
    /// index entry, and a device that has just adopted an account holds a note
    /// key and nothing else — no log, no entry, no row in the store. That note
    /// has no card and must still be connected, because connecting is how the
    /// log it is missing arrives. Going the other way, a note whose key this
    /// device does not have is left out however much of its log it holds: an
    /// envelope it cannot open is an envelope it cannot use, and a connection
    /// for one would be a socket held open to no purpose.
    ///
    /// The account document is not in here. It is not a note, its ID comes from
    /// [`NoteCore::account_id`], and a caller that syncs it does so knowingly.
    pub fn syncable(&self) -> Result<Vec<NoteId>> {
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        let from_store = self.store.notes()?;
        let placed = self.placements.placements.keys().copied();
        for id in from_store.into_iter().chain(placed) {
            if self.is_account(id) || !seen.insert(id) {
                continue;
            }
            if self.keyring.note_key(id)?.is_none() {
                continue;
            }
            out.push(id);
        }
        Ok(out)
    }

    pub fn sync_cursor(&self, id: NoteId) -> Result<SyncCursor> {
        match self.store.get_meta(id, SYNC_CURSOR)? {
            Some(raw) => serde_json::from_slice(&raw)
                .map_err(|e| CoreError::Storage(format!("unreadable sync cursor: {e}"))),
            None => Ok(SyncCursor::default()),
        }
    }

    pub fn set_sync_cursor(&mut self, id: NoteId, cursor: SyncCursor) -> Result<()> {
        let raw = serde_json::to_vec(&cursor)
            .map_err(|e| CoreError::Storage(format!("unwritable sync cursor: {e}")))?;
        self.store.put_meta(id, SYNC_CURSOR, &raw)
    }

    /// Our own envelopes, oldest first, that the relay has not acknowledged.
    ///
    /// This is the outbox, and it is derived rather than kept: the log already
    /// holds every edit this device made, so an app that is killed between an
    /// offline edit and the next connection has lost nothing.
    pub fn unsent(&self, id: NoteId, after: Option<u64>) -> Result<Vec<(u64, Vec<u8>)>> {
        let me = self.identity.public_key();
        let mut out = Vec::new();
        for raw in self.store.load(id)? {
            let env = Envelope::decode(&raw)?;
            if env.author != me {
                continue;
            }
            if after.is_some_and(|sent| env.counter <= sent) {
                continue;
            }
            out.push((env.counter, raw));
        }
        out.sort_by_key(|(counter, _)| *counter);
        Ok(out)
    }

    /// Fold our own unsent `envelopes` into as few as will do, and put those in
    /// the log in their place. Returns what to send instead, oldest first.
    ///
    /// The other half of the engine holding typing back
    /// ([ADR 0016](../../docs/adr/0016-batching-edits.md)): a keystroke is still
    /// sealed and logged the moment it is made, so nothing waits in memory
    /// where a crash could take it, and what reaches the relay is one envelope
    /// per burst. Rewriting those rows is allowed for the reason compaction may
    /// not drop them — no other device has ever been handed them.
    ///
    /// Three rules keep it safe, and each is a way it could otherwise lose an
    /// edit:
    ///
    /// * **Only envelopes that are still in the log, and ours.** Anything else
    ///   is passed back untouched; its content is either not this device's to
    ///   rewrite or already inside something that is.
    /// * **A fresh counter for every envelope that comes out, never a reused
    ///   one.** One of the inputs may have reached the relay on a connection that
    ///   died before its echo did. Under its old counter the merge would be
    ///   deduplicated away wherever the original landed first, and the rest of
    ///   the burst with it. Under a new one, a peer applies both and the CRDT
    ///   makes the overlap a no-op — at-least-once, as ever.
    /// * **Counters rise in the order the result is sent.** The acknowledgement
    ///   is a high-water mark, so an echo of a higher counter arriving before a
    ///   lower one is on the relay would mark the lower one sent. Hence, once
    ///   anything is merged, every input this may touch is resealed, even one
    ///   standing alone in a run of its own — and nothing is merged at all while
    ///   the log holds an edit of ours newer than the batch, because the fold's
    ///   counter would be above that edit and the fold would be sent first.
    ///
    /// The account document is left alone: its edits are sent as they are made,
    /// and it has a compaction of its own.
    pub fn squash(&mut self, id: NoteId, envelopes: &[Vec<u8>]) -> Result<Vec<Vec<u8>>> {
        if envelopes.len() < 2 || self.is_account(id) {
            return Ok(envelopes.to_vec());
        }
        self.ensure_open(id)?;
        let me = self.identity.public_key();
        let log = self.store.load(id)?;
        let note = self
            .open
            .get_mut(&id)
            .ok_or_else(|| CoreError::UnknownNote(id.encode()))?;

        // Split into runs that each fit in one envelope, and the envelopes this
        // may not touch.
        let mut parts: Vec<Squashed> = Vec::new();
        let mut newest = None;
        {
            let in_log: HashSet<&[u8]> = log.iter().map(Vec::as_slice).collect();
            let mut run = Run::default();
            for raw in envelopes {
                let ours = in_log.contains(raw.as_slice())
                    && Envelope::decode(raw).is_ok_and(|e| e.author == me && e.note_id == id);
                if !ours {
                    run.close_into(&mut parts);
                    parts.push(Squashed::Kept(raw.clone()));
                    continue;
                }
                let envelope = Envelope::decode(raw)?;
                newest = newest.max(Some(envelope.counter));
                let plain = envelope.open(&note.key)?;
                if run.bytes + plain.len() > SQUASH_BUDGET {
                    run.close_into(&mut parts);
                }
                run.bytes += plain.len();
                run.members.push((raw.clone(), plain));
            }
            run.close_into(&mut parts);
        }
        if !parts
            .iter()
            .any(|p| matches!(p, Squashed::Run(members) if members.len() > 1))
        {
            // Nothing merged means nothing to reseal either, and a log rewritten
            // into itself.
            return Ok(envelopes.to_vec());
        }
        let overtaken = log.iter().any(|raw| {
            Envelope::decode(raw).is_ok_and(|e| e.author == me && Some(e.counter) > newest)
        });
        if overtaken {
            // An edit newer than the batch is in the log and not in the batch:
            // made while this was being called, and still on its way to the
            // engine. A fold would take a counter above it and be sent first,
            // and its echo would mark that edit sent before it was. Send the
            // batch as it is and fold the next one.
            return Ok(envelopes.to_vec());
        }

        let mut out = Vec::with_capacity(parts.len());
        let mut absorbed: HashSet<Vec<u8>> = HashSet::new();
        let mut added = Vec::new();
        for part in parts {
            match part {
                Squashed::Kept(raw) => out.push(raw),
                Squashed::Run(members) => {
                    let updates: Vec<Vec<u8>> =
                        members.iter().map(|(_, plain)| plain.clone()).collect();
                    let update = D::merge_updates(&updates)?;
                    let counter = note.next_counter;
                    note.next_counter += 1;
                    let envelope = Envelope::seal(&self.identity, id, &note.key, counter, &update)?;
                    note.seen.insert((envelope.author, counter));
                    let encoded = envelope.encode();
                    absorbed.extend(members.into_iter().map(|(raw, _)| raw));
                    added.push(encoded.clone());
                    out.push(encoded);
                }
            }
        }

        // At the end rather than where the originals were: everything a merged
        // update depends on is earlier in the log than its last input was, so
        // later is still after all of it. One call, because it is atomic — a
        // crash leaves the originals or the merge, never neither.
        let mut rewritten: Vec<Vec<u8>> = log
            .into_iter()
            .filter(|raw| !absorbed.contains(raw))
            .collect();
        rewritten.extend(added);
        self.store
            .put_meta(id, NEXT_COUNTER, &note.next_counter.to_be_bytes())?;
        self.store.replace_log(id, &rewritten)?;
        // The entry records the log's length, as after a compaction.
        self.reindex(id)?;
        Ok(out)
    }

    /// Forget every in-memory document, keeping the store and keyring. Used by
    /// tests to simulate the app being killed.
    pub fn close_all(&mut self) {
        self.open.clear();
        // The index goes with them. It is in memory as well as in the store, and
        // a "killed" app that kept its list would be telling the test it is only
        // half killed — the next `notes` reads the entries back off the disk,
        // which is exactly the path a real relaunch takes.
        self.index.clear();
        self.refresh_library();
    }

    /// Give the store and the keyring back, consuming the core.
    ///
    /// The other half of [`NoteCore::close_all`], and the half that is a real
    /// relaunch: opening a core is where a long account log is compacted and
    /// where every counter is recovered, so a test that wants the app killed and
    /// started again has to go back through [`NoteCore::open`] rather than only
    /// dropping what is in memory.
    pub fn into_parts(self) -> (S, K) {
        (self.store, self.keyring)
    }

    pub fn store(&self) -> &S {
        &self.store
    }

    /// The keyring, for the one caller that needs both halves of a device at
    /// once: [`crate::store::copy_store`], which moves a device from one store
    /// to another and has to carry the keys with the logs. Read-only, and it
    /// hands out nothing the holder of this core could not already decrypt.
    pub fn keyring(&self) -> &K {
        &self.keyring
    }
}
