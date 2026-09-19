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

use crate::crdt::{
    account, AccountDoc, AccountEdit, AccountView, Colour, Edit, Layout, NoteDoc, NoteView,
    Placement,
};
use crate::crypto::{AuthorKey, DeviceIdentity, DeviceSeed, Envelope, NoteKey};
use crate::error::{CoreError, Result};
use crate::ids::{ItemId, NoteId};
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

/// The setting that says whether the note list is a grid or one column.
pub const LAYOUT: &str = "layout";

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
    pub title: String,
    /// Every item, checked or not.
    pub items: usize,
    pub checked: usize,
    /// The first few items, in display order, so a card can show what the note
    /// is about. Most shopping lists never get a title.
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

/// Slice 1 builds checklists. `Text` is here because the design doc's data model
/// has both and the share blob format should not change when text notes land.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoteKind {
    Checklist,
    Text,
}

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
        let (account, length) = Self::open_account(&mut store, &mut keyring, &identity, &actor)?;
        let placements = account.doc.view();
        let mut core = Self {
            identity,
            actor,
            store,
            keyring,
            open: HashMap::new(),
            account,
            placements,
            index: HashMap::new(),
            library: watch::channel(Vec::new()).0,
        };
        // The same moment `open_note` compacts at, for the same reason: the
        // document has just been rebuilt, so the snapshot is free, and the replay
        // that made compaction worth doing has just been paid for the last time.
        // The difference is that this one happens at launch rather than when a
        // screen opens, because opening the device *is* opening this document.
        if length > COMPACT_AFTER {
            core.compact_account()?;
        }
        Ok(core)
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
    ) -> Result<(OpenAccount<D::Account>, usize)> {
        let key = match keyring.note_key(ACCOUNT_DOC)? {
            Some(key) => key,
            None => {
                let key = NoteKey::random();
                keyring.put_note_key(ACCOUNT_DOC, &key)?;
                key
            }
        };

        let me = identity.public_key();
        let mut doc = <D::Account as AccountDoc>::create(actor);
        let mut next_counter = 0u64;
        let mut seen = HashSet::new();
        let mut length = 0usize;
        for raw in store.load(ACCOUNT_DOC)? {
            length += 1;
            let env = Envelope::decode(&raw)?;
            if env.note_id != ACCOUNT_DOC {
                return Err(CoreError::WrongNote {
                    expected: ACCOUNT_DOC.encode(),
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
        // An empty entry, so that the note has a card from the moment it exists
        // rather than from its first edit. `place` below refreshes the list, and
        // the list is built from the index.
        self.reindex(id)?;
        // Where it sits is the account document's answer, and a new note goes to
        // the top of the grid. `place` refreshes the list, which is why nothing
        // here does.
        self.place(id, 0)?;
        // `kind` does not change the document shape in Slice 1 (a text note is a
        // checklist with no items); it is recorded so the UI can branch later.
        let _ = kind;
        Ok(id)
    }

    /// Open a note that is already in the store, replaying its snapshot and log.
    /// This is what makes demo step 6 — relaunch with no network — work.
    pub fn open_note(&mut self, id: NoteId) -> Result<NoteView> {
        if id == ACCOUNT_DOC {
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
        let mut next_counter = 0u64;

        let mut doc = match self.store.get_snapshot(id)? {
            Some(blob) => {
                let env = Envelope::decode(&blob)?;
                let plain = env.open(&key)?;
                // A snapshot is sealed with a counter from this device's own
                // sequence, which is what lets the counter survive a compaction
                // that removed every envelope it could otherwise be read from.
                if env.author == me {
                    next_counter = env.counter + 1;
                }
                D::load(&self.actor, &plain)?
            }
            None => D::create(&self.actor),
        };

        let mut seen = HashSet::new();
        let mut length = 0usize;
        for raw in self.store.load(id)? {
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
            if id == ACCOUNT_DOC || self.index.contains_key(&id) {
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
            title: view.title,
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

    fn refresh_library(&self) {
        let mut summaries: Vec<NoteSummary> = self
            .index
            .iter()
            .map(|(id, entry)| NoteSummary {
                id: *id,
                title: entry.title.clone(),
                items: entry.items.len(),
                checked: entry.checked,
                preview: entry.items.iter().take(PREVIEW_ITEMS).cloned().collect(),
                placement: self.placements.placement(*id),
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

    /// Every label in use on this device, and how many notes carry each.
    ///
    /// There is no list of labels apart from the notes that carry them: a label
    /// exists because a note has it, and taking it off the last one takes the
    /// label with it. See [`AccountView::tags`].
    pub fn tags(&self) -> Vec<(String, usize)> {
        self.placements.tags()
    }

    /// Put a label on a note, and return the name it was stored under.
    ///
    /// `None` for something that is not a name at all — whitespace, or nothing.
    /// Otherwise the name after [`normalise_tag`], **except** when this device
    /// already has a label that differs only in case: then it is that one. Typing
    /// `recipes` under a `Recipes` this person made last week means the label they
    /// already have, not a second one that looks the same on a chip; and since
    /// the two are different keys in the document, nothing but this reuse keeps
    /// them from both existing.
    ///
    /// What it cannot do is stop two *devices* from making that pair, each
    /// offline and each the first to use the name. That is a real residue and it
    /// is the honest cost of having no separate list of labels to agree about —
    /// both show, both work, and either can be taken off.
    ///
    /// Labelling a note this device cannot open is refused, for the reason
    /// forgetting the account document is: a placement is about a note, and a
    /// note with a placement and no key is a card that cannot be drawn.
    pub fn add_tag(&mut self, id: NoteId, tag: &str) -> Result<Option<String>> {
        let Some(name) = account::normalise_tag(tag) else {
            return Ok(None);
        };
        let name = self.existing_tag(&name).unwrap_or(name);
        self.apply_account(AccountEdit::SetTag {
            note: id,
            tag: name.clone(),
            on: true,
        })?;
        Ok(Some(name))
    }

    /// Take a label off a note. Not an error if it was never on it.
    ///
    /// Matched the way [`NoteCore::add_tag`] matches, so that a caller holding
    /// the name a person typed removes the label they meant rather than nothing.
    pub fn remove_tag(&mut self, id: NoteId, tag: &str) -> Result<()> {
        let Some(name) = account::normalise_tag(tag) else {
            return Ok(());
        };
        let tags = self.placement(id).tags;
        let Some(name) = tags
            .iter()
            .find(|held| held.eq_ignore_ascii_case(&name))
            .cloned()
        else {
            return Ok(());
        };
        self.apply_account(AccountEdit::SetTag {
            note: id,
            tag: name,
            on: false,
        })
    }

    /// The label this device already has under this name, ignoring case.
    ///
    /// ASCII-insensitive and not Unicode-insensitive, deliberately: a full case
    /// fold depends on a locale — Turkish dotless i is the standing example — and
    /// a label that is the same one on a phone and a different one on a laptop is
    /// worse than a label that is only ever folded where every device folds it
    /// the same way.
    fn existing_tag(&self, name: &str) -> Option<String> {
        self.placements
            .placements
            .values()
            .flat_map(|placement| placement.tags.iter())
            .find(|held| held.eq_ignore_ascii_case(name))
            .cloned()
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
        if id == ACCOUNT_DOC {
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
        // The labels have to be read before anything else is removed, because
        // they are the one part of a placement that cannot be named without
        // looking ([`AccountEdit::ForgetNote`]).
        let tags = self.placement(id).tags.into_iter().collect();
        self.apply_account(AccountEdit::ForgetNote { note: id, tags })?;
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
        let mine = self.placement(id).order;
        let mut keys = self.order_keys();
        if let Some(at) = keys.iter().position(|key| *key == mine) {
            keys.remove(at);
        }
        let order = order::key_for_index(&keys, to_index.min(keys.len()));
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
    /// envelope back so the bridge can publish it; there is no relay for this
    /// document yet, and when there is one the engine will read the envelope out
    /// of the log like every other outbox does
    /// ([ADR 0004](../../docs/adr/0004-sync-engine.md)) rather than being handed
    /// it here.
    fn apply_account(&mut self, edit: AccountEdit) -> Result<()> {
        let update = self.account.doc.apply_local(&edit)?;
        if update.is_empty() {
            // A write that changed nothing — clearing a colour that was never
            // set. Sealing it would put an envelope in the log that says nothing.
            return Ok(());
        }
        let counter = self.account.next_counter;
        self.account.next_counter += 1;
        let envelope = Envelope::seal(
            &self.identity,
            ACCOUNT_DOC,
            &self.account.key,
            counter,
            &update,
        )?;
        self.account.seen.insert((envelope.author, counter));
        self.store.append(ACCOUNT_DOC, &envelope.encode())?;
        self.placements = self.account.doc.view();
        self.refresh_library();
        Ok(())
    }

    /// Every order key in use, ascending.
    fn order_keys(&self) -> Vec<String> {
        let mut keys: Vec<String> = self
            .placements
            .placements
            .values()
            .map(|placement| placement.order.clone())
            .filter(|key| !key.is_empty())
            .collect();
        keys.sort();
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
            if id == ACCOUNT_DOC || self.placements.placements.contains_key(&id) {
                continue;
            }
            // A note this device has no key for gets no placement, for the same
            // reason `notes` gives it no card: there is nothing to show and
            // nothing that could be shown. It is also what keeps a forgotten note
            // forgotten — a late-arriving envelope can leave a sync cursor behind
            // for one ([`NoteCore::trashed_notes`]), and a placement written from
            // that row would be a permanent entry in a document nothing compacts,
            // about a note that does not exist.
            if self.keyring.note_key(id)?.is_none() {
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
        // An arrival is the other thing that can be the first to need a note's
        // document: the list no longer opens every note, so a peer's edit to a
        // note nobody has looked at today opens it here.
        self.ensure_open(id)?;
        let note = self
            .open
            .get_mut(&id)
            .ok_or_else(|| CoreError::UnknownNote(id.encode()))?;

        let env = Envelope::decode(envelope)?;
        if env.note_id != id {
            return Err(CoreError::WrongNote {
                expected: id.encode(),
                got: env.note_id.encode(),
            });
        }
        // `open` verifies the signature before it decrypts.
        let plain = env.open(&note.key)?;

        if !note.seen.insert((env.author, env.counter)) {
            // Already applied. Persisting it again would grow the log for nothing.
            return Ok(note.doc.view());
        }
        note.doc.apply_remote(&plain)?;
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
        if id == ACCOUNT_DOC {
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
        let env = Envelope::seal(identity_ref, id, &note.key, counter, &blob)?;
        self.store.put_snapshot(id, &env.encode())
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
        let before = self.store.log_len(ACCOUNT_DOC)?;
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
            ACCOUNT_DOC,
            &self.account.key,
            counter,
            &blob,
        )?;
        self.account.seen.insert((envelope.author, counter));
        self.store.replace_log(ACCOUNT_DOC, &[envelope.encode()])?;
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
