//! The Flutter-facing API.
//!
//! This is a thin shell over `easynotes-core` and should stay that way: no
//! crypto, no CRDT logic, no policy. Anything that belongs in the core belongs
//! in the core, where `cargo test` can reach it without a Flutter toolchain.
//!
//! Types are mirrored rather than re-exported because flutter_rust_bridge needs
//! to see plain structs it can generate Dart for.
//!
//! Two objects, and the split between them is the shape of the app. A
//! [`Library`] is the device: every note it holds, the relay it syncs them with,
//! and the connections that do it. A [`Note`] is one note open on one screen,
//! and there may be several or none. They share one core behind one lock, so a
//! note edited on a screen and the same note in the grid behind it cannot
//! disagree.

use crate::frb_generated::StreamSink;
use crate::relay::{on_network, Connection, Device};
use crate::storage::{Pending, Queue};
use easynotes_core::crdt::{Colour, DefaultNoteDoc, Layout, RepeatRule};
use easynotes_core::ids::{InviteId, ItemId, LabelId, NoteId, ReminderId};
use easynotes_core::invite::{GuestKey, InviteSecret};
use easynotes_core::note::{NoteCore, NoteKind, OutgoingInvite, PendingJoin};
use easynotes_core::store::{Keyring, MemoryKeyring, MemoryStore, OpStore};
use easynotes_core::sync::invite::{self, Answer};
use easynotes_core::sync::RequestState;
use easynotes_core::CoreError;
use flutter_rust_bridge::frb;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use tokio::sync::watch;

/// The store and keyring are boxed so one `Library` type serves every platform:
/// SQLite on desktop and mobile, IndexedDB in a browser, and memory for a test
/// or a tab that has neither, without this file changing.
type Core = NoteCore<DefaultNoteDoc, Box<dyn OpStore + Send>, Box<dyn Keyring + Send>>;

/// What a platform offers a core to write into: a log and the keys for it.
type Backing = (Box<dyn OpStore + Send>, Box<dyn Keyring + Send>);

#[frb(init)]
pub fn init_app() {
    flutter_rust_bridge::setup_default_user_utils();
}

// ---------------------------------------------------------------------------
// What Dart sees
// ---------------------------------------------------------------------------

/// One checklist item, as Dart sees it.
pub struct ItemView {
    pub id: String,
    pub text: String,
    pub checked: bool,
    pub order: String,
}

/// One note, as Dart sees it. Items arrive already in display order.
///
/// `body` and `items` both arrive whatever `kind` says, because a merge can
/// leave content on the side a conversion emptied and a screen draws it rather
/// than hiding it — see `easynotes_core::crdt::NoteView`.
pub struct NoteView {
    pub kind: NoteKindView,
    pub title: String,
    pub body: String,
    pub items: Vec<ItemView>,
}

/// Whether a note is prose or a checklist. Shared: it is in the note's own
/// document, so switching it switches it for everyone on the note.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoteKindView {
    Checklist,
    Text,
}

impl From<NoteKind> for NoteKindView {
    fn from(k: NoteKind) -> Self {
        match k {
            NoteKind::Checklist => NoteKindView::Checklist,
            NoteKind::Text => NoteKindView::Text,
        }
    }
}

impl From<NoteKindView> for NoteKind {
    fn from(k: NoteKindView) -> Self {
        match k {
            NoteKindView::Checklist => NoteKind::Checklist,
            NoteKindView::Text => NoteKind::Text,
        }
    }
}

impl From<easynotes_core::crdt::NoteView> for NoteView {
    fn from(v: easynotes_core::crdt::NoteView) -> Self {
        Self {
            kind: v.kind.into(),
            title: v.title,
            body: v.body,
            items: v
                .items
                .into_iter()
                .map(|i| ItemView {
                    id: i.id.encode(),
                    text: i.text,
                    checked: i.checked,
                    order: i.order,
                })
                .collect(),
        }
    }
}

/// One card in the grid: enough to draw a note without opening it.
pub struct NoteSummaryView {
    pub collaborators: Vec<CollaboratorView>,
    pub id: String,
    pub kind: NoteKindView,
    pub title: String,
    pub items: u32,
    pub checked: u32,
    /// The first few lines of the body, then the first few items. A card shows
    /// these under the title, and most shopping lists do not have one.
    pub preview: Vec<String>,
    /// What this person has done with the note. From the account document, so
    /// none of it is shared with whoever else is on the note.
    ///
    /// Archived and trashed notes are in the list: which of them a screen draws
    /// is the screen's question, and the grid, the archive and the trash are
    /// three answers to it.
    pub colour: Option<NoteColour>,
    pub pinned: bool,
    pub archived: bool,
    pub trashed: bool,
    /// The labels this person has put on the note, sorted. A `Vec` rather than a
    /// set because that is what crosses the bridge; it holds each label once,
    /// which is the document's doing and not this type's.
    ///
    /// Each carries an ID as well as a name, because the name is the thing that
    /// can change: a screen draws the name and acts on the ID, and a rename
    /// landing between the two is then a redraw rather than a lost label.
    pub tags: Vec<LabelView>,
}

/// One label on a note: what it is, and what to call it.
pub struct LabelView {
    pub id: String,
    pub name: String,
}

impl From<easynotes_core::note::NoteSummary> for NoteSummaryView {
    fn from(s: easynotes_core::note::NoteSummary) -> Self {
        Self {
            collaborators: s.collaborators.into_iter().map(Into::into).collect(),
            id: s.id.encode(),
            kind: s.kind.into(),
            title: s.title,
            items: s.items as u32,
            checked: s.checked as u32,
            preview: s.preview,
            colour: s.placement.colour.map(Into::into),
            pinned: s.placement.pinned,
            archived: s.placement.archived,
            trashed: s.placement.trashed,
            tags: s
                .labels
                .into_iter()
                .map(|(id, name)| LabelView {
                    id: id.encode(),
                    name,
                })
                .collect(),
        }
    }
}

/// One label, and how many notes carry it.
///
/// The count is what a filter menu shows beside the name. It can be **zero**: a
/// label exists in its own right rather than only through the notes carrying it,
/// so taking a label off the last note leaves the label. That is what an `id`
/// separate from the `name` buys, and it is what makes renaming one possible.
pub struct TagView {
    pub id: String,
    pub name: String,
    pub notes: u32,
}

pub struct CollaboratorView {
    pub id: String,
    pub name: String,
}

impl From<easynotes_core::collaborators::Collaborator> for CollaboratorView {
    fn from(c: easynotes_core::collaborators::Collaborator) -> Self {
        Self {
            id: c.id,
            name: c.name,
        }
    }
}

pub struct CollaboratorsView {
    pub people: Vec<CollaboratorView>,
    pub can_manage: bool,
    pub current_device: String,
}

/// How a reminder repeats, mirrored for Flutter rather than exposing a core
/// type to the bridge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReminderRepeat {
    Never,
    Daily,
    Weekly,
    Monthly,
    Yearly,
}

impl From<RepeatRule> for ReminderRepeat {
    fn from(repeat: RepeatRule) -> Self {
        match repeat {
            RepeatRule::Never => Self::Never,
            RepeatRule::Daily => Self::Daily,
            RepeatRule::Weekly => Self::Weekly,
            RepeatRule::Monthly => Self::Monthly,
            RepeatRule::Yearly => Self::Yearly,
        }
    }
}

impl From<ReminderRepeat> for RepeatRule {
    fn from(repeat: ReminderRepeat) -> Self {
        match repeat {
            ReminderRepeat::Never => Self::Never,
            ReminderRepeat::Daily => Self::Daily,
            ReminderRepeat::Weekly => Self::Weekly,
            ReminderRepeat::Monthly => Self::Monthly,
            ReminderRepeat::Yearly => Self::Yearly,
        }
    }
}

/// One personal reminder as Flutter sees it. The timestamp remains a decimal
/// string so web builds never narrow a 64-bit Unix-millisecond value.
pub struct ReminderView {
    pub id: String,
    pub note: String,
    pub fire_at: String,
    pub repeat: ReminderRepeat,
    pub timezone: String,
}

impl From<easynotes_core::crdt::Reminder> for ReminderView {
    fn from(reminder: easynotes_core::crdt::Reminder) -> Self {
        Self {
            id: reminder.id.encode(),
            note: reminder.note.encode(),
            fire_at: reminder.fire_at.to_string(),
            repeat: reminder.repeat.into(),
            timezone: reminder.timezone,
        }
    }
}

/// One note a search found, and the line it was found on.
///
/// The line is carried because a card shows four items and a shopping list has
/// twenty: a result with nothing on it to say why it is a result reads as a bug.
pub struct SearchHitView {
    pub note: NoteSummaryView,
    /// The title, or the item, that the query matched.
    pub matched: String,
}

impl From<easynotes_core::note::SearchHit> for SearchHitView {
    fn from(hit: easynotes_core::note::SearchHit) -> Self {
        Self {
            note: hit.summary.into(),
            matched: hit.matched,
        }
    }
}

/// A note's colour, as Dart sees it. A fixed palette rather than a number,
/// because what a colour has to look like is a question for the theme on the
/// device drawing it — the same name is one thing in daylight and another in
/// dark mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoteColour {
    Red,
    Orange,
    Yellow,
    Green,
    Teal,
    Blue,
    Purple,
    Pink,
    Brown,
    Grey,
}

impl From<Colour> for NoteColour {
    fn from(c: Colour) -> Self {
        match c {
            Colour::Red => NoteColour::Red,
            Colour::Orange => NoteColour::Orange,
            Colour::Yellow => NoteColour::Yellow,
            Colour::Green => NoteColour::Green,
            Colour::Teal => NoteColour::Teal,
            Colour::Blue => NoteColour::Blue,
            Colour::Purple => NoteColour::Purple,
            Colour::Pink => NoteColour::Pink,
            Colour::Brown => NoteColour::Brown,
            Colour::Grey => NoteColour::Grey,
        }
    }
}

impl From<NoteColour> for Colour {
    fn from(c: NoteColour) -> Self {
        match c {
            NoteColour::Red => Colour::Red,
            NoteColour::Orange => Colour::Orange,
            NoteColour::Yellow => Colour::Yellow,
            NoteColour::Green => Colour::Green,
            NoteColour::Teal => Colour::Teal,
            NoteColour::Blue => Colour::Blue,
            NoteColour::Purple => Colour::Purple,
            NoteColour::Pink => Colour::Pink,
            NoteColour::Brown => Colour::Brown,
            NoteColour::Grey => Colour::Grey,
        }
    }
}

/// Whether the note list is a masonry grid or one column. A view setting, and
/// so a personal one: it lives in the account document with the placements.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoteLayout {
    Grid,
    List,
}

impl From<Layout> for NoteLayout {
    fn from(l: Layout) -> Self {
        match l {
            Layout::Grid => NoteLayout::Grid,
            Layout::List => NoteLayout::List,
        }
    }
}

impl From<NoteLayout> for Layout {
    fn from(l: NoteLayout) -> Self {
        match l {
            NoteLayout::Grid => Layout::Grid,
            NoteLayout::List => Layout::List,
        }
    }
}

/// Where this note's sync stands, as Dart sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum SyncState {
    /// There is nothing to sync: this device holds no notes.
    ///
    /// Distinct from [`SyncState::Offline`] because they look identical to a
    /// user and mean opposite things. A fresh install reported `Offline` — "Not
    /// syncing" — with a perfectly healthy relay, which is the first screen
    /// anyone sees and a false negative for anyone debugging one.
    ///
    /// First in the ordering, so that if one ever did reach the fold below it
    /// would be the *worst* state rather than silently outranking a real one.
    /// Nothing produces it per note; only the empty fold does.
    Empty,
    /// Not connected. Everything still works; the relay just has not heard it yet.
    Offline,
    Connecting,
    /// Connected, still catching up on what arrived while we were away.
    Online,
    /// Connected and caught up.
    Synced,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyncStatus {
    pub state: SyncState,
    /// Our own edits the relay has not acknowledged yet.
    pub pending: u32,
    pub last_seq: i64,
    /// Why the last attempt failed, if it did. Shown to the user verbatim, so it
    /// is the relay's or the network's words, not ours.
    pub last_error: Option<String>,
}

impl SyncStatus {
    /// What to show before anything has been connected.
    pub(crate) fn offline() -> Self {
        Self {
            state: SyncState::Offline,
            pending: 0,
            last_seq: 0,
            last_error: None,
        }
    }

    /// What a device with no notes on it shows. See [`SyncState::Empty`].
    pub(crate) fn nothing_to_sync() -> Self {
        Self {
            state: SyncState::Empty,
            pending: 0,
            last_seq: 0,
            last_error: None,
        }
    }

    /// One status for a device with several notes.
    ///
    /// The state is the *worst* of them, because the question the status button
    /// answers is "is everything I typed somewhere other than this device", and
    /// one note that has not made it is a no. Pending edits add up for the same
    /// reason, and the first error is shown because a device with no network has
    /// the same error for every note and repeating it says nothing.
    fn combined<'a>(each: impl Iterator<Item = &'a SyncStatus>) -> Self {
        let mut seen = false;
        let mut worst = SyncState::Synced;
        let mut pending = 0u32;
        let mut last_seq = 0i64;
        let mut last_error = None;
        for status in each {
            seen = true;
            worst = worst.min(status.state);
            pending = pending.saturating_add(status.pending);
            last_seq = last_seq.max(status.last_seq);
            if last_error.is_none() {
                last_error.clone_from(&status.last_error);
            }
        }
        if !seen {
            // Not `offline()`: a device with no notes is not a device that
            // cannot reach the relay, and saying so cost a session's debugging
            // once. `Synced` would be the fold's identity element and is the
            // other tempting answer, but it claims a relay agrees with us when
            // we have never spoken to one.
            return Self::nothing_to_sync();
        }
        Self {
            state: worst,
            pending,
            last_seq,
            last_error,
        }
    }
}

impl From<easynotes_core::sync::SyncStatus> for SyncStatus {
    fn from(s: easynotes_core::sync::SyncStatus) -> Self {
        use easynotes_core::sync::SyncState as Core;
        Self {
            state: match s.state {
                Core::Offline => SyncState::Offline,
                Core::Connecting => SyncState::Connecting,
                Core::Online => SyncState::Online,
                Core::Synced => SyncState::Synced,
            },
            pending: s.pending as u32,
            last_seq: s.last_seq,
            last_error: s.last_error,
        }
    }
}

// ---------------------------------------------------------------------------
// Where sync status goes
// ---------------------------------------------------------------------------

/// One note's status channel, and which connection is allowed to write to it.
struct NoteStatus {
    tx: Arc<watch::Sender<SyncStatus>>,
    /// Bumped every time a connection is started or stopped for this note. A
    /// report from an older one is dropped.
    ///
    /// Without this, replacing a connection is a race the stale engine can win:
    /// it says `Offline` with no error as its task ends, *after* the engine that
    /// replaced it has already said why it could not connect, and the button
    /// then shows a device that is quietly offline for no reason. The epoch is
    /// checked and the value written under one lock, so there is no window.
    epoch: u64,
}

/// Every note's sync status, and the one-line answer for all of them.
///
/// Separate from [`Shared`] on purpose: an engine's forwarder task holds this,
/// and if it held the connections too, a connection would keep alive the task
/// that keeps alive the connection, and a closed library would never let go.
pub(crate) struct Statuses {
    per_note: Mutex<HashMap<NoteId, NoteStatus>>,
    overall: Arc<watch::Sender<SyncStatus>>,
}

impl Statuses {
    fn new() -> Self {
        Self {
            per_note: Mutex::new(HashMap::new()),
            // Not `offline()`: there are no notes yet, by construction, so this
            // is the empty fold's answer and has to agree with it — otherwise a
            // library that never refreshes (because it never gets a note) keeps
            // reporting a relay problem it does not have.
            overall: Arc::new(watch::channel(SyncStatus::nothing_to_sync()).0),
        }
    }

    fn entry<T>(&self, note: NoteId, f: impl FnOnce(&mut NoteStatus) -> T) -> T {
        let mut per_note = self.per_note.lock().expect("status lock poisoned");
        f(per_note.entry(note).or_insert_with(|| NoteStatus {
            tx: Arc::new(watch::channel(SyncStatus::offline()).0),
            epoch: 0,
        }))
    }

    /// The same, for a note that must already have a channel. `None` means it
    /// does not — which, since [`Statuses::forget`], means the note is gone.
    fn existing<T>(&self, note: NoteId, f: impl FnOnce(&mut NoteStatus) -> T) -> Option<T> {
        let mut per_note = self.per_note.lock().expect("status lock poisoned");
        per_note.get_mut(&note).map(f)
    }

    /// The channel for one note, created on first use.
    ///
    /// It belongs to the note rather than to a connection, so the UI subscribes
    /// once and keeps seeing the truth across a disconnect, a reconnect, or a
    /// move to a different relay.
    fn channel(&self, note: NoteId) -> Arc<watch::Sender<SyncStatus>> {
        self.entry(note, |status| status.tx.clone())
    }

    /// Start an epoch for a note: the connection about to be made owns its
    /// status, and whatever was there before no longer does.
    fn begin(&self, note: NoteId) -> u64 {
        self.entry(note, |status| {
            status.epoch += 1;
            status.epoch
        })
    }

    /// An engine said something. Tell that note's watchers, then everyone
    /// watching the device as a whole.
    ///
    pub(crate) fn report(&self, note: NoteId, epoch: u64, status: SyncStatus) {
        // `existing` rather than `entry`: a report can arrive for a note that has
        // just been forgotten, from an engine whose task had not yet noticed, and
        // creating a channel for a note that no longer exists would leave an
        // `Offline` in the device's combined status for ever — one deleted note
        // would make the whole device look disconnected.
        let current = self.existing(note, |note| {
            if note.epoch == epoch {
                note.tx.send_replace(status);
            }
            note.epoch
        });
        if current == Some(epoch) {
            self.refresh();
        }
    }

    /// A note is gone for good. Its epoch ends, so anything its engine says from
    /// here is dropped, and its channel goes with it so that the device's status
    /// stops counting a note that does not exist toward what is pending.
    fn forget(&self, note: NoteId) {
        let mut per_note = self.per_note.lock().expect("status lock poisoned");
        per_note.remove(&note);
        drop(per_note);
        self.refresh();
    }

    fn refresh(&self) {
        let per_note = self.per_note.lock().expect("status lock poisoned");
        let each: Vec<SyncStatus> = per_note
            .values()
            .map(|status| status.tx.borrow().clone())
            .collect();
        drop(per_note);
        self.overall.send_replace(SyncStatus::combined(each.iter()));
    }

    /// Nothing is connected any more. Said here rather than left to the engines,
    /// whose last word may not arrive before their tasks are gone — and whose
    /// last word is now ignored, because ending an epoch is what this is.
    fn all_offline(&self) {
        let mut per_note = self.per_note.lock().expect("status lock poisoned");
        for status in per_note.values_mut() {
            status.epoch += 1;
            status.tx.send_replace(SyncStatus::offline());
        }
        drop(per_note);
        self.refresh();
    }
}

// ---------------------------------------------------------------------------
// The device
// ---------------------------------------------------------------------------

/// The core as everything that is not the core holds it: behind a lock, and
/// only *maybe* there.
///
/// The `Option` is what [`Library::close`] needs and the reason this is a type
/// rather than a bare `Arc<Mutex<Core>>`. Letting the `Arc` fall out of scope
/// cannot be how a database is closed here, because the `Arc` has other holders
/// — every live [`Note`] handle, and every sync engine, which took a clone as
/// its peer — and the bridge keeps *its* side of those alive until Dart
/// collects them, at a moment nobody chooses. So closing is something the app
/// says, and this is where it is recorded.
///
/// `SyncPeer` is implemented here rather than taken from the core's blanket impl
/// on `Arc<Mutex<NoteCore>>` for the same reason: an engine that is still
/// draining when the library closes has to be told the core is gone, not handed
/// one that is not there.
#[derive(Clone)]
pub(crate) struct Peer {
    core: Arc<Mutex<Option<Core>>>,
    /// The device this core belongs to, for the one thing an engine asks for
    /// that is not a read or a write: a note whose key has just arrived in the
    /// account document needs a connection of its own, and only [`Shared`] can
    /// make one.
    ///
    /// Weak, because `Shared` holds this peer: an `Arc` here would be a cycle
    /// and the library would never be dropped.
    device: Weak<Shared>,
}

fn closed() -> String {
    "the library is closed".to_string()
}

impl Peer {
    fn new(core: Core, device: Weak<Shared>) -> Self {
        Self {
            core: Arc::new(Mutex::new(Some(core))),
            device,
        }
    }

    fn locked(&self) -> Result<Open<'_>, String> {
        let guard = self
            .core
            .lock()
            .map_err(|_| "core lock poisoned".to_string())?;
        if guard.is_none() {
            return Err(closed());
        }
        Ok(Open(guard))
    }

    /// Drop the core, and with it the store — which is what shuts the SQLite
    /// connection and lets the last WAL checkpoint run. Closing twice is not an
    /// error: the second one has nothing to do.
    fn close(&self) -> Result<(), String> {
        let taken = self
            .core
            .lock()
            .map_err(|_| "core lock poisoned".to_string())?
            .take();
        drop(taken);
        Ok(())
    }
}

/// A locked core that is known to be open, so that the 30-odd callers of
/// [`Peer::locked`] read as if the `Option` were not there.
pub(crate) struct Open<'a>(std::sync::MutexGuard<'a, Option<Core>>);

impl std::ops::Deref for Open<'_> {
    type Target = Core;

    fn deref(&self) -> &Core {
        self.0.as_ref().expect("checked by Peer::locked")
    }
}

impl std::ops::DerefMut for Open<'_> {
    fn deref_mut(&mut self) -> &mut Core {
        self.0.as_mut().expect("checked by Peer::locked")
    }
}

/// An engine that outlives the close sees `Err` rather than a core: every one of
/// these is a call the engine makes from its own task, and the honest answer
/// once the store is shut is that there is nothing to read or write.
impl easynotes_core::crypto::DeviceSigner for Peer {
    fn device_key(&self) -> easynotes_core::error::Result<easynotes_core::crypto::AuthorKey> {
        Ok(self.open()?.device_key())
    }

    fn sign_challenge(
        &self,
        nonce: &[u8; easynotes_core::crypto::RELAY_AUTH_NONCE_LEN],
    ) -> easynotes_core::error::Result<[u8; easynotes_core::crypto::SIG_LEN]> {
        self.open()?.identity().sign_challenge(nonce)
    }
}

impl easynotes_core::sync::SyncPeer for Peer {
    fn account_topic(&self) -> easynotes_core::error::Result<Option<NoteId>> {
        Ok(self.open()?.account_id())
    }

    fn is_deleted(&self, note: NoteId) -> easynotes_core::error::Result<bool> {
        Ok(self.open()?.is_deleted_note(note))
    }

    fn apply_remote(&self, note: NoteId, envelope: &[u8]) -> easynotes_core::error::Result<()> {
        let mut core = self.open()?;
        // One of these connections is not a note. The account document reaches
        // the relay as a topic like any other and is an `AccountDoc` when it
        // gets here, so it has an entry point of its own; which of the two this
        // is is the core's answer rather than this layer's.
        if core.account_id() == Some(note) {
            return core.apply_remote_account(envelope);
        }
        core.apply_remote(note, envelope).map(|_| ())
    }

    fn cursor(
        &self,
        note: NoteId,
    ) -> easynotes_core::error::Result<easynotes_core::note::SyncCursor> {
        self.open()?.sync_cursor(note)
    }

    fn save_cursor(
        &self,
        note: NoteId,
        cursor: easynotes_core::note::SyncCursor,
    ) -> easynotes_core::error::Result<()> {
        self.open()?.set_sync_cursor(note, cursor)
    }

    fn unsent(
        &self,
        note: NoteId,
        after: Option<u64>,
    ) -> easynotes_core::error::Result<Vec<(u64, Vec<u8>)>> {
        self.open()?.unsent(note, after)
    }

    /// The account document's backlog has landed: take the note keys it carries,
    /// and connect whatever they turned into a note this device can read.
    ///
    /// The second half is why a peer knows about its device at all. A note
    /// adopted this way has a key and nothing else — no log, no index entry, no
    /// card — so nothing else on this device would ever notice it, and its
    /// contents arrive only over a connection it does not yet have.
    fn settled(&self, note: NoteId) -> easynotes_core::error::Result<()> {
        {
            let mut core = self.open()?;
            if core.account_id() != Some(note) {
                return Ok(());
            }
            core.adopt_note_keys()?;
        }
        if let Some(device) = self.device.upgrade() {
            device
                .connect_new_documents()
                .map_err(easynotes_core::error::CoreError::Storage)?;
        }
        Ok(())
    }
}

impl Peer {
    /// [`Peer::locked`] for the engine's side, whose errors are the core's own
    /// type rather than the bridge's strings.
    fn open(&self) -> easynotes_core::error::Result<Open<'_>> {
        self.locked()
            .map_err(easynotes_core::error::CoreError::Storage)
    }
}

/// What every note on this device shares.
pub(crate) struct Shared {
    core: Peer,
    /// The relay to sync with, once [`Library::connect`] has named one. Kept so
    /// that a note created or joined later can be connected on the spot.
    relay: Mutex<Option<String>>,
    /// The engine for that relay: one socket, every note on it.
    device: Mutex<Option<Device>>,
    connections: Mutex<HashMap<NoteId, Connection>>,
    statuses: Arc<Statuses>,
}

impl Shared {
    fn locked(&self) -> Result<Open<'_>, String> {
        self.core.locked()
    }

    /// Start syncing one note, replacing whatever connection it had.
    ///
    /// A subscription on the device's one socket, not a socket of its own
    /// ([ADR 0017](../../../../docs/adr/0017-one-socket-per-device.md)): a
    /// device with twenty notes does one token handshake and holds one
    /// connection. Replacing a note's connection hands the engine a new handle
    /// for it and nothing more — its cursor and anything it is holding carry
    /// over, and the relay is not asked for its backlog again.
    fn connect_note(&self, note: NoteId) -> Result<(), String> {
        let connection = {
            let device = self.device.lock().map_err(|_| "sync lock poisoned")?;
            let Some(device) = device.as_ref() else {
                return Ok(());
            };
            let epoch = self.statuses.begin(note);
            device.connect(note, self.statuses.clone(), epoch)?
        };
        let previous = self
            .connections
            .lock()
            .map_err(|_| "sync lock poisoned".to_string())?
            .insert(note, connection);
        if let Some(previous) = previous {
            previous.shutdown();
        }
        Ok(())
    }

    /// The relay's HTTP address, for the calls that are not a socket.
    fn http_base(&self) -> Result<String, String> {
        let relay = self.relay.lock().map_err(|_| "relay lock poisoned")?;
        let relay = relay
            .as_ref()
            .ok_or("Sharing needs a relay, and this device is not connected to one.")?;
        easynotes_core::sync::RelayEndpoint::parse(relay)
            .map(|e| e.http_base)
            .map_err(|e| e.to_string())
    }

    /// An invite this device has open, by the ID the screen was given.
    fn outgoing(&self, invite: &str) -> Result<OutgoingInvite, String> {
        let core = self.locked()?;
        core.invites()
            .map_err(|e| e.to_string())?
            .into_iter()
            .find(|i| i.secret.id().encode() == invite)
            .ok_or_else(|| "This device has no such invite open.".to_string())
    }

    fn forget_invite(&self, invite: &OutgoingInvite) -> Result<(), String> {
        self.locked()?
            .forget_invite(invite.secret.id())
            .map_err(|e| e.to_string())
    }

    /// Connect everything this device can open and is not connected to already.
    ///
    /// Called when a relay is named and again whenever the account document
    /// settles, because that is when a note key can arrive: a note adopted from
    /// a key has no card and no log, so [`NoteCore::syncable`] rather than the
    /// note list is what knows about it.
    ///
    /// The account document is in the list. To a relay it *is* a note — an ID,
    /// a log and a sequence number, with a body it cannot read — and the only
    /// thing that sets it apart is that its ID comes from the recovery phrase
    /// rather than from the note list. A device with no account has no such ID
    /// and nothing is connected for it: its document is local-only, under the
    /// reserved ID that must never reach a relay ([ADR
    /// 0013](../../docs/adr/0013-recovery-phrase.md)).
    ///
    /// The locks are taken one at a time and in this order on purpose. The
    /// account sink publishes while the core lock is held, and publishing takes
    /// the connection lock; holding the connections here and then asking for the
    /// core would be the other order, and the two together are a deadlock.
    fn connect_new_documents(&self) -> Result<(), String> {
        let mut wanted: Vec<NoteId> = {
            let core = self.locked()?;
            let mut wanted = core.syncable().map_err(|e| e.to_string())?;
            wanted.extend(core.account_id());
            wanted
        };
        let allowed: std::collections::HashSet<_> = wanted.iter().copied().collect();
        let retired = {
            let mut connections = self
                .connections
                .lock()
                .map_err(|_| "sync lock poisoned".to_string())?;
            let retired: Vec<_> = connections
                .keys()
                .copied()
                .filter(|id| !allowed.contains(id))
                .collect();
            retired
                .into_iter()
                .filter_map(|id| connections.remove(&id).map(|connection| (id, connection)))
                .collect::<Vec<_>>()
        };
        for (id, connection) in retired {
            connection.shutdown();
            self.statuses.forget(id);
        }
        {
            let open = self
                .connections
                .lock()
                .map_err(|_| "sync lock poisoned".to_string())?;
            wanted.retain(|id| !open.contains_key(id));
        }
        for id in wanted {
            self.connect_note(id)?;
        }
        Ok(())
    }

    /// Stop syncing one note because it is about to stop existing.
    ///
    /// Not the same as replacing a connection, which leaves the note's status
    /// channel in place because the note is still there and a screen may still be
    /// watching it. Here there will be no note, so the channel goes too.
    fn disconnect_note(&self, note: NoteId) -> Result<(), String> {
        let connection = self
            .connections
            .lock()
            .map_err(|_| "sync lock poisoned".to_string())?
            .remove(&note);
        if let Some(connection) = connection {
            connection.shutdown();
        }
        self.statuses.forget(note);
        Ok(())
    }

    /// Hand a sealed envelope to the relay, if there is a connection to hand it
    /// to. There being none is not an error: the edit is already in the log, and
    /// the next connection finds it there.
    fn publish(&self, note: NoteId, envelope: Vec<u8>) {
        self.with_connection(note, |c| c.publish(envelope));
    }

    /// [`Shared::publish`] for a keystroke: the envelope waits until the typing
    /// pauses and goes with the rest of the word, as one
    /// ([ADR 0016](../../../../docs/adr/0016-batching-edits.md)).
    fn publish_soon(&self, note: NoteId, envelope: Vec<u8>) {
        self.with_connection(note, |c| c.publish_soon(envelope));
    }

    fn rekey(&self, note: NoteId, envelope: Vec<u8>) {
        self.with_connection(note, |c| c.rekey(envelope));
    }

    fn with_connection(&self, note: NoteId, then: impl FnOnce(&Connection)) {
        if let Ok(connections) = self.connections.lock() {
            if let Some(connection) = connections.get(&note) {
                then(connection);
            }
        }
    }
}

fn boxed_store(s: impl OpStore + Send + 'static) -> Box<dyn OpStore + Send> {
    Box::new(s)
}

fn boxed_keyring(k: impl Keyring + Send + 'static) -> Box<dyn Keyring + Send> {
    Box::new(k)
}

/// The store a device with a disk gets: SQLite for the log, SQLite for the
/// wrapped keys, and a master key that seals the second one.
///
/// All of it is `easynotes_core::store::open_local`, including the one-time
/// carry-over from the directory of files Slice 1 wrote, so that a `cargo test`
/// with no Flutter in the loop can cover what an app update does to a device
/// that already has notes on it.
#[cfg(not(target_arch = "wasm32"))]
fn on_disk(root: &std::path::Path, master: Vec<u8>) -> Result<Backing, String> {
    let master =
        easynotes_core::crypto::MasterKey::from_slice(&master).map_err(|e| e.to_string())?;
    let (store, keyring) =
        easynotes_core::store::open_local(root, master).map_err(|e| e.to_string())?;
    Ok((boxed_store(store), boxed_keyring(keyring)))
}

/// The master key a device wrote beside its database before the app could reach
/// the OS keychain, if it has one.
///
/// Dart calls this once, when the keychain has nothing: a device that updated
/// into this version is unlocked by that file and by nothing else, so the key
/// has to be adopted rather than replaced. The file is left where it is.
///
/// It hands a master key to Dart, which is the point of item 3b rather than a
/// lapse in it: the key lives in the keychain now, and Dart is what can reach
/// one. What the core keeps is that it never chooses where the key is kept.
#[cfg(not(target_arch = "wasm32"))]
#[frb(sync)]
pub fn master_key_beside_the_database(dir: String) -> Result<Option<Vec<u8>>, String> {
    let path = std::path::PathBuf::from(dir).join("master.key");
    Ok(easynotes_core::store::existing_master_key(path)
        .map_err(|e| e.to_string())?
        .map(|key| key.0.to_vec()))
}

/// A browser has no such file, and no database beside it either.
#[cfg(target_arch = "wasm32")]
#[frb(sync)]
pub fn master_key_beside_the_database(_dir: String) -> Result<Option<Vec<u8>>, String> {
    Ok(None)
}

/// A browser has no disk, and nothing here pretends otherwise: the web build's
/// library is [`Library::open_in_browser`], which is handed the rows a browser
/// kept and hands back the writes it should keep next.
#[cfg(target_arch = "wasm32")]
fn on_disk(_root: &std::path::Path, _master: Vec<u8>) -> Result<Backing, String> {
    Err("this platform has no directory to open; use Library.openInBrowser".to_string())
}

// ---------------------------------------------------------------------------
// The store a browser gets
// ---------------------------------------------------------------------------
//
// IndexedDB is asynchronous and `OpStore` is not, so `easynotes_core::store::web`
// splits the problem: it serves every read from memory and hands every write out
// as a batch of key-value changes for something that can reach a database to
// apply. This is the near side of that — the queue those batches wait in — and
// Dart is the far side.
//
// **Dart and not Rust**, for the reason `master_key_beside_the_database` gives
// about the keychain: the platform is Dart's. There is a sharper reason here
// too. On the web, flutter_rust_bridge runs a `#[frb(sync)]` or `async` method
// on the page's own thread and every other method on a Web Worker, and an
// `IDBDatabase` belongs to the thread that opened it. A handle held in Rust
// would therefore be reachable from some of the methods that write to it and
// not from the others. A queue of plain bytes has no such problem, and Dart
// takes it from the thread it is already on.

/// One row of a browser's object store.
pub struct StorageRow {
    pub key: String,
    pub value: Vec<u8>,
}

/// Which object store a batch is for. The log and the keys are kept apart in a
/// browser as they are in two SQLite files on a device — as far apart as an
/// origin allows, which [ADR 0011](../../../docs/adr/0011-the-web-store.md) is
/// honest about.
pub enum StorageArea {
    Notes,
    Keys,
}

/// What one write does.
pub enum StorageWriteKind {
    Put,
    Delete,
    /// Every key beginning with the given one, which is a single range delete.
    DeletePrefix,
}

/// One write, in the shape an object store takes it.
///
/// Flat — a kind beside a key and a value — rather than an enum carrying its
/// own fields, because flutter_rust_bridge renders the latter with `freezed`,
/// and a code generator and its build step are a lot to add to the app for one
/// three-armed type at an FFI boundary.
pub struct StorageWrite {
    pub kind: StorageWriteKind,
    /// The key, or for a `DeletePrefix` the prefix every key to remove begins
    /// with.
    pub key: String,
    /// What to store, for a `Put`. Empty for the other two — and an empty
    /// *value* is legal, so it is the kind that says which this is.
    pub value: Vec<u8>,
}

/// Writes that must land together or not at all: **one batch is one
/// transaction**.
///
/// A batch is what the core says must not be applied by halves — the two steps
/// of a compaction, or everything a forgotten note had — so a caller that split
/// one across two transactions would reintroduce the crash windows the store is
/// written to avoid.
pub struct StorageBatch {
    pub area: StorageArea,
    pub writes: Vec<StorageWrite>,
}

impl From<&easynotes_core::store::Change> for StorageWrite {
    fn from(change: &easynotes_core::store::Change) -> Self {
        use easynotes_core::store::Change;
        match change {
            Change::Put { key, value } => StorageWrite {
                kind: StorageWriteKind::Put,
                key: key.clone(),
                value: value.clone(),
            },
            Change::Delete { key } => StorageWrite {
                kind: StorageWriteKind::Delete,
                key: key.clone(),
                value: Vec::new(),
            },
            Change::DeletePrefix { prefix } => StorageWrite {
                kind: StorageWriteKind::DeletePrefix,
                key: prefix.clone(),
                value: Vec::new(),
            },
        }
    }
}

// Where the batches wait between the store and Dart is `crate::storage`, which
// is outside this module for the reason `crate::relay` is: everything in here
// becomes Dart, and a queue of batches is not something Dart has any business
// holding a handle to.

fn item_id(s: &str) -> Result<ItemId, String> {
    ItemId::decode(s).map_err(|e| e.to_string())
}

fn note_id(s: &str) -> Result<NoteId, String> {
    NoteId::decode(s).map_err(|e| e.to_string())
}

fn label_id(s: &str) -> Result<LabelId, String> {
    LabelId::decode(s).map_err(|e| e.to_string())
}

fn reminder_id(s: &str) -> Result<ReminderId, String> {
    ReminderId::decode(s).map_err(|e| e.to_string())
}

fn reminder_time(s: &str) -> Result<i64, String> {
    s.parse::<i64>()
        .ok()
        .filter(|value| *value >= 0)
        .ok_or_else(|| "a reminder needs a valid time".to_string())
}

// ---------------------------------------------------------------------------
// Invites
// ---------------------------------------------------------------------------

/// How long an invite lasts. The design doc's seven days: long enough for a
/// link sent to a partner to be opened after a weekend, short enough that a
/// link left in an old chat stops working on its own.
const INVITE_TTL_SECS: u64 = easynotes_core::sync::invite::DEFAULT_TTL_SECS;

/// Seconds as Dart gets them: an `int` everywhere, which a `u64` is not — the
/// bridge makes that a `BigInt` — and thirty days fits many times over.
fn clamp_secs(secs: u64) -> u32 {
    secs.min(u32::MAX as u64) as u32
}

/// Unix seconds. `SystemTime` panics in a browser, which has a clock of its own.
#[cfg(not(target_arch = "wasm32"))]
fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(target_arch = "wasm32")]
fn now() -> i64 {
    (js_sys::Date::now() / 1000.0) as i64
}

/// An invite this device has open.
pub struct InviteView {
    pub id: String,
    pub note: String,
    /// What to send. Carries a secret and no key.
    pub link: String,
    /// How long it has left by this device's clock; the relay's is the one
    /// that holds.
    pub expires_in_secs: u32,
}

impl From<&OutgoingInvite> for InviteView {
    fn from(invite: &OutgoingInvite) -> Self {
        Self {
            id: invite.secret.id().encode(),
            note: invite.note.encode(),
            link: invite.secret.link(),
            expires_in_secs: clamp_secs((invite.expires_at - now()).max(0) as u64),
        }
    }
}

pub enum RequestStateView {
    Pending,
    Approved,
    Declined,
}

impl From<RequestState> for RequestStateView {
    fn from(state: RequestState) -> Self {
        match state {
            RequestState::Pending => Self::Pending,
            RequestState::Approved => Self::Approved,
            RequestState::Declined => Self::Declined,
        }
    }
}

/// Somebody asking to join, as the owner is shown them.
pub struct JoinRequestView {
    pub id: String,
    /// What they typed. Their word only.
    pub name: String,
    /// The six digits their screen shows too.
    pub code: String,
    pub state: RequestStateView,
    /// False for a request that does not open under the invite's key: it was
    /// not made by anyone holding the link, and cannot be approved.
    pub readable: bool,
}

pub struct InviteRequestsView {
    /// False once somebody has been let in, or the invite has expired.
    pub open: bool,
    pub expires_in_secs: u32,
    pub requests: Vec<JoinRequestView>,
}

/// A join request this device is waiting on.
pub struct PendingJoinView {
    pub invite: String,
    pub name: String,
    /// What to read to the owner, if they ask.
    pub code: String,
}

impl From<&PendingJoin> for PendingJoinView {
    fn from(join: &PendingJoin) -> Self {
        Self {
            invite: join.secret.id().encode(),
            name: join.name.clone(),
            code: join.code(),
        }
    }
}

pub enum JoinStateView {
    /// Nobody has answered yet.
    Waiting,
    /// Let in: the note is on this device and syncing.
    Joined,
    /// Turned away.
    Declined,
    /// Expired, used by someone else, or withdrawn. `message` says which, as
    /// far as the relay would say.
    Gone,
}

pub struct JoinCheckView {
    pub state: JoinStateView,
    /// The note, once joined.
    pub note: Option<String>,
    pub message: Option<String>,
}

impl JoinCheckView {
    fn waiting() -> Self {
        Self {
            state: JoinStateView::Waiting,
            note: None,
            message: None,
        }
    }
}

/// The libraries this process has open on a disk, by directory, so that there
/// is only ever one per database.
///
/// The Rust side of the app lives as long as the process, and the Dart side
/// does not. On Android, backing out of the app destroys the Flutter engine
/// and its isolate while the process is kept, and nothing runs `dispose()` on
/// the way out — so the library that isolate opened is never closed, and its
/// sync engine keeps its note subscriptions and reconnects to the relay the
/// next time the process is thawed. Opening the app again then made a second
/// library on the same SQLite file, with an engine of its own: two writers to
/// one database, and one more socket to the relay for every back-and-reopen.
/// Seen on the emulators; see item 5 of the Slice 3 plan.
///
/// Dart cannot fix this from its side, because the moment it would need to act
/// is the moment its isolate is being torn down. So the next open does it: a
/// directory opened again retires whatever library had it, as `dispose()`
/// would have — sockets shut, watchers stopped, store closed — before the new
/// one touches the file. A library the app still holds and opens a second
/// library over is retired too, which is the right answer for the same reason:
/// two cores on one file disagree about everything they cache.
///
/// Weak, so that this is never what keeps a library alive.
struct OpenLibraries;

struct HeldLibrary {
    shared: Weak<Shared>,
    stop: Weak<AtomicBool>,
}

/// A directory claimed for a library about to open. The registry stays locked
/// until [`Claim::hold`] or the claim is dropped, so two opens of one
/// directory cannot race each other past the retirement.
struct Claim {
    dir: std::path::PathBuf,
    open: std::sync::MutexGuard<'static, HashMap<std::path::PathBuf, HeldLibrary>>,
}

impl OpenLibraries {
    fn registry() -> &'static Mutex<HashMap<std::path::PathBuf, HeldLibrary>> {
        static OPEN: std::sync::OnceLock<Mutex<HashMap<std::path::PathBuf, HeldLibrary>>> =
            std::sync::OnceLock::new();
        OPEN.get_or_init(|| Mutex::new(HashMap::new()))
    }

    /// Retire whatever library has `dir` open, and claim it for the next.
    fn claim(dir: &std::path::Path) -> Claim {
        // Canonical, so that two spellings of one directory are one entry. A
        // directory that does not exist yet has nothing open in it.
        let dir = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
        // A poisoned registry still holds the truth about what is open; the
        // panic that poisoned it was somebody else's.
        let mut open = Self::registry()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // Libraries Dart has already let go of: nothing to retire, and no
        // reason to keep an entry for every temp directory a test suite opens.
        open.retain(|_, held| held.shared.strong_count() > 0);
        if let Some(held) = open.remove(&dir) {
            if let (Some(shared), Some(stop)) = (held.shared.upgrade(), held.stop.upgrade()) {
                let orphan = Library {
                    shared,
                    stop,
                    pending: Arc::new(Pending::default()),
                };
                // Best effort, and each step regardless of the last: an orphan
                // that was half disposed already answers "closed" to some of
                // these, and that is the state this is trying to reach.
                let _ = orphan.disconnect();
                let _ = orphan.stop_watching();
                let _ = orphan.close();
            }
        }
        Claim { dir, open }
    }
}

impl Claim {
    fn hold(mut self, library: &Library) {
        self.open.insert(
            self.dir.clone(),
            HeldLibrary {
                shared: Arc::downgrade(&library.shared),
                stop: Arc::downgrade(&library.stop),
            },
        );
    }
}

/// Every note this device holds.
///
/// One per app. Opening it reads each note's sealed index entry rather than each
/// note — a title is inside an envelope, so a device with no index would have to
/// replay every log to draw a grid, which is what `easynotes_core::index`
/// exists to stop.
pub struct Library {
    shared: Arc<Shared>,
    /// Set by [`Library::stop_watching`]. The watcher tasks check it when they
    /// wake.
    stop: Arc<AtomicBool>,
    /// The writes a browser has yet to be given. Empty and never filled on a
    /// device with a disk, where the store writes for itself.
    pending: Arc<Pending>,
}

impl Library {
    /// Open the notes stored in `dir`, on a device that has a disk, unlocked by
    /// `master_key`.
    ///
    /// `dir` holds the SQLite database and the keyring; `master_key` is the 32
    /// bytes that seal everything in them which is not already an envelope, and
    /// it comes from the OS keychain by way of Dart — the core does not know or
    /// care where it was kept. Slice 1's directory of files is carried into the
    /// database the first time this runs, and a device that has the key in a
    /// file beside it is carried over by
    /// [`master_key_beside_the_database`].
    ///
    /// The wrong key is not a soft failure: nothing in the database opens, and
    /// this says so rather than presenting an empty device to someone whose
    /// notes are right there.
    ///
    /// Opening a directory this process already has open retires the library
    /// that had it first — see [`OpenLibraries`].
    #[frb(sync)]
    pub fn open(dir: String, master_key: Vec<u8>) -> Result<Library, String> {
        let root = std::path::PathBuf::from(dir);
        let claim = OpenLibraries::claim(&root);
        let (store, keyring) = on_disk(&root, master_key)?;
        let library = Self::wrap(NoteCore::open(store, keyring).map_err(|e| e.to_string())?)?;
        claim.hold(&library);
        Ok(library)
    }

    /// Notes that live only in memory and are gone when the tab closes.
    ///
    /// This is what the web build uses today. A browser has no filesystem, and
    /// the op log's IndexedDB backing is item 4 of the Slice 2 plan — see
    /// `docs/adr/0002-web-core.md`. Everything above the store is the same code
    /// the native build runs, which is what makes this a stepping stone rather
    /// than a mock.
    #[frb(sync)]
    pub fn open_ephemeral() -> Result<Library, String> {
        Self::wrap(
            NoteCore::open(
                boxed_store(MemoryStore::new()),
                boxed_keyring(MemoryKeyring::new()),
            )
            .map_err(|e| e.to_string())?,
        )
    }

    /// Open the notes a browser is holding for this origin, unlocked by
    /// `master_key`.
    ///
    /// `notes` and `keys` are every row of the two object stores, which the
    /// caller has already read — that read is the one thing about a browser's
    /// storage that has to be waited for, and it happens here, at launch, where
    /// the app is waiting anyway. Afterwards nothing waits: the store answers
    /// from memory and leaves its writes in a queue for
    /// [`Library::take_storage_batches`].
    ///
    /// Not `#[cfg(target_arch = "wasm32")]`, deliberately. There is no browser
    /// in any of this — it is a store, a queue and some rows — so the native
    /// build can open one too, and `app/test/rust_bridge_test.dart` drives the
    /// whole of the web build's persistence on a machine that has no browser on
    /// it. A web-only path is a path nothing tests.
    #[frb(sync)]
    pub fn open_in_browser(
        notes: Vec<StorageRow>,
        keys: Vec<StorageRow>,
        master_key: Vec<u8>,
    ) -> Result<Library, String> {
        use easynotes_core::store::{WebKeyring, WebStore};
        let master = easynotes_core::crypto::MasterKey::from_slice(&master_key)
            .map_err(|e| e.to_string())?;
        let pending = Arc::new(Pending::default());
        let rows = |rows: Vec<StorageRow>| rows.into_iter().map(|r| (r.key, r.value));
        let queue = |area| {
            Box::new(Queue {
                area,
                pending: pending.clone(),
            })
        };
        let store = WebStore::from_rows(master.clone(), queue(StorageArea::Notes), rows(notes))
            .map_err(|e| e.to_string())?;
        let keyring = WebKeyring::from_rows(master, queue(StorageArea::Keys), rows(keys))
            .map_err(|e| e.to_string())?;
        Self::wrap_with(
            NoteCore::open(boxed_store(store), boxed_keyring(keyring))
                .map_err(|e| e.to_string())?,
            pending,
        )
    }

    /// Every write the store has made since this was last called, oldest first,
    /// and forget them.
    ///
    /// The caller is expected to apply them — one batch, one transaction — and
    /// to call this often, because until it does, an edit is in memory and
    /// nowhere else. It is a *pull* rather than a stream for the reason the
    /// section above this gives: the methods that write run on a Web Worker and
    /// the one that reads runs on the page's thread, so what passes between them
    /// has to be bytes behind a lock and nothing that belongs to a thread.
    ///
    /// Taking them and then failing to write them loses them, which is why this
    /// is the only thing that empties the queue: a caller that cannot apply a
    /// batch has already been handed it, and the honest thing for it to do is
    /// say so rather than ask for it again.
    #[frb(sync)]
    pub fn take_storage_batches(&self) -> Vec<StorageBatch> {
        self.pending.take()
    }

    /// How many batches are waiting. Zero on a device with a disk, always.
    #[frb(sync)]
    pub fn unwritten_batches(&self) -> u32 {
        self.pending.len()
    }

    fn wrap(core: Core) -> Result<Library, String> {
        Self::wrap_with(core, Arc::new(Pending::default()))
    }

    fn wrap_with(mut core: Core, pending: Arc<Pending>) -> Result<Library, String> {
        // Everything on the disk, read once, so that the list is populated
        // before the first frame asks for it.
        core.notes().map_err(|e| e.to_string())?;
        // Cyclic because the device holds the core and the core has to be able
        // to reach back: an account-document edit has no envelope to return —
        // there are twenty ways to make one and several of them write twice — so
        // the core pushes it here, where a note edit hands it over explicitly.
        // Both ends up in the same `publish`. Weak both ways round, so that the
        // library is still dropped when Dart lets go of it.
        let shared = Arc::new_cyclic(move |device: &Weak<Shared>| {
            let sink = device.clone();
            core.on_account_envelope(Box::new(move |id, envelope| {
                if let Some(device) = sink.upgrade() {
                    device.publish(id, envelope.to_vec());
                }
            }));
            Shared {
                core: Peer::new(core, device.clone()),
                relay: Mutex::new(None),
                device: Mutex::new(None),
                connections: Mutex::new(HashMap::new()),
                statuses: Arc::new(Statuses::new()),
            }
        });
        Ok(Library {
            shared,
            stop: Arc::new(AtomicBool::new(false)),
            pending,
        })
    }

    // -- the list -----------------------------------------------------------

    #[frb(sync)]
    pub fn notes(&self) -> Result<Vec<NoteSummaryView>, String> {
        let mut core = self.shared.locked()?;
        Ok(core
            .notes()
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(Into::into)
            .collect())
    }

    /// The note list now, and every version of it after that.
    ///
    /// Every edit reaches it, whether it was made here or arrived from another
    /// device, because both go through the core's one refresh.
    pub async fn watch_notes(&self, sink: StreamSink<Vec<NoteSummaryView>>) -> Result<(), String> {
        let mut updates = { self.shared.locked()?.watch_notes() };
        let stop = self.stop.clone();
        let send = |summaries: Vec<easynotes_core::note::NoteSummary>| {
            sink.add(summaries.into_iter().map(Into::into).collect::<Vec<_>>())
        };
        if send(updates.borrow_and_update().clone()).is_err() {
            return Ok(());
        }
        loop {
            if updates.changed().await.is_err() {
                return Ok(()); // the library was dropped
            }
            if stop.load(Ordering::SeqCst) {
                return Ok(());
            }
            let next = updates.borrow_and_update().clone();
            if send(next).is_err() {
                return Ok(()); // Dart stopped listening
            }
        }
    }

    /// Every note whose text contains all of the query's words.
    ///
    /// Local, and local in the strong sense: it reads the same sealed index the
    /// grid is drawn from, so it works with no network and never asks a relay
    /// anything. Archived and trashed notes come back too, with their placements
    /// on them — which of them to show is the screen's question, as it is for
    /// the list.
    pub fn search(&self, query: String) -> Result<Vec<SearchHitView>, String> {
        let mut core = self.shared.locked()?;
        Ok(core
            .search(&query)
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(Into::into)
            .collect())
    }

    /// Make a new, empty note of `kind` and return its ID.
    pub fn create_note(&self, kind: NoteKindView) -> Result<String, String> {
        let id = {
            let mut core = self.shared.locked()?;
            core.create_note(kind.into()).map_err(|e| e.to_string())?
        };
        self.shared.connect_note(id)?;
        Ok(id.encode())
    }

    /// Accept a share blob and add the note it describes to this device.
    ///
    /// An addition, not a replacement — which is the whole difference between
    /// this and Slice 1's one-note join. A blob for a note this device already
    /// has is not an error and does not make a second copy.
    pub fn join(&self, blob: String) -> Result<String, String> {
        let id = {
            let mut core = self.shared.locked()?;
            // Replays whatever this device already holds for the note under the
            // key the blob just supplied, so a second join finds the log that is
            // there rather than putting an empty card over it.
            core.import_share_blob(&blob).map_err(|e| e.to_string())?
        };
        self.shared.connect_note(id)?;
        Ok(id.encode())
    }

    /// A handle to one note, for a screen that is showing it.
    ///
    /// Cheap, and there may be several: they share the library's core, so two
    /// handles to one note are two views of the same document rather than two
    /// documents.
    #[frb(sync)]
    pub fn note(&self, id: String) -> Result<Note, String> {
        let id = note_id(&id)?;
        // Opening is what makes the watch channel and the view real; a note the
        // library listed is already open, and this is what covers one that is
        // not.
        self.shared
            .locked()?
            .open_note(id)
            .map_err(|e| e.to_string())?;
        Ok(Note {
            shared: self.shared.clone(),
            id,
            status: self.shared.statuses.channel(id),
            stop: Arc::new(AtomicBool::new(false)),
        })
    }

    // -- what this person thinks of their notes ------------------------------
    //
    // Placements and settings, from the account document. They are on the
    // library rather than on a note for the reason the relay address is: they
    // belong to the device's view of the whole set, and a note that is not open
    // on any screen can still be pinned from the grid.
    //
    // None of this reaches the note, so none of it reaches whoever else is on
    // it — see `docs/adr/0008-account-document.md`.

    pub fn set_colour(&self, id: String, colour: Option<NoteColour>) -> Result<(), String> {
        let id = note_id(&id)?;
        self.shared
            .locked()?
            .set_colour(id, colour.map(Into::into))
            .map_err(|e| e.to_string())
    }

    pub fn set_pinned(&self, id: String, pinned: bool) -> Result<(), String> {
        let id = note_id(&id)?;
        self.shared
            .locked()?
            .set_pinned(id, pinned)
            .map_err(|e| e.to_string())
    }

    /// Move a note so that it takes `target`'s place in the grid's order.
    ///
    /// Named by the note underneath rather than by an index, because that is what
    /// a drag knows: the grid is filtered to a shelf or a label, and the order it
    /// writes into is over every note this device has. See
    /// `NoteCore::move_note_to` for which side of the target a drop lands on.
    pub fn move_note_to(&self, id: String, target: String) -> Result<(), String> {
        let id = note_id(&id)?;
        let target = note_id(&target)?;
        self.shared
            .locked()?
            .move_note_to(id, target)
            .map_err(|e| e.to_string())
    }

    /// Every label in use on this device, with how many notes carry each.
    ///
    /// Sorted the way a menu wants them, which the core decides: a label is read
    /// by a person, so the order ignores case.
    #[frb(sync)]
    pub fn tags(&self) -> Result<Vec<TagView>, String> {
        Ok(self
            .shared
            .locked()?
            .tags()
            .into_iter()
            .map(|(id, name, notes)| TagView {
                id: id.encode(),
                name,
                notes: notes as u32,
            })
            .collect())
    }

    /// Every reminder this device has decrypted from its account document.
    /// `watch_notes` is also its invalidation stream for local and remote
    /// account edits, so a scheduler does not need a second Rust watcher.
    #[frb(sync)]
    pub fn reminders(&self) -> Result<Vec<ReminderView>, String> {
        Ok(self
            .shared
            .locked()?
            .reminders()
            .into_iter()
            .map(Into::into)
            .collect())
    }

    /// Make a new reminder and return its stable ID.
    pub fn create_reminder(
        &self,
        note: String,
        fire_at: String,
        repeat: ReminderRepeat,
        timezone: String,
    ) -> Result<String, String> {
        let note = note_id(&note)?;
        let fire_at = reminder_time(&fire_at)?;
        Ok(self
            .shared
            .locked()?
            .create_reminder(note, fire_at, repeat.into(), timezone)
            .map_err(|e| e.to_string())?
            .encode())
    }

    /// Update an existing reminder.
    pub fn set_reminder(
        &self,
        reminder: String,
        note: String,
        fire_at: String,
        repeat: ReminderRepeat,
        timezone: String,
    ) -> Result<(), String> {
        let reminder = reminder_id(&reminder)?;
        let note = note_id(&note)?;
        let fire_at = reminder_time(&fire_at)?;
        self.shared
            .locked()?
            .set_reminder(reminder, note, fire_at, repeat.into(), timezone)
            .map_err(|e| e.to_string())
    }

    /// Remove one reminder without touching any other reminder on its note.
    pub fn forget_reminder(&self, reminder: String) -> Result<(), String> {
        let reminder = reminder_id(&reminder)?;
        self.shared
            .locked()?
            .forget_reminder(reminder)
            .map_err(|e| e.to_string())
    }

    /// Put a label on a note, and return the label it went on under.
    ///
    /// `null` for something that is not a name — whitespace, or nothing — so a
    /// field submitted empty is a no-op rather than a label called "". The label
    /// that comes back may not be a new one: a name differing only in case from
    /// one this device already has *is* that label, and the UI has to draw the
    /// label that exists rather than the casing that was typed.
    pub fn add_tag(&self, id: String, tag: String) -> Result<Option<String>, String> {
        let id = note_id(&id)?;
        Ok(self
            .shared
            .locked()?
            .add_tag(id, &tag)
            .map_err(|e| e.to_string())?
            .map(|label| label.encode()))
    }

    /// Make a label that no note carries yet, and return it. An existing label
    /// with this name is returned rather than a second one made.
    pub fn make_tag(&self, name: String) -> Result<String, String> {
        Ok(self
            .shared
            .locked()?
            .make_tag(&name)
            .map_err(|e| e.to_string())?
            .encode())
    }

    /// Take a label off a note. Not an error if it was never on it.
    ///
    /// The label itself stays, with one fewer note on it. Throwing the label
    /// away is [`NoteLibrary::forget_tag`], and it is a different gesture on
    /// purpose.
    pub fn remove_tag(&self, id: String, label: String) -> Result<(), String> {
        let id = note_id(&id)?;
        let label = label_id(&label)?;
        self.shared
            .locked()?
            .remove_tag(id, label)
            .map_err(|e| e.to_string())
    }

    /// Rename a label. Every note carrying it keeps it, because a membership
    /// points at the ID and has never mentioned the name.
    ///
    /// Returns the label that survived, which is **not** always the one passed
    /// in: renaming onto a name another label already has merges the two, and
    /// the survivor is the one that already had the name. `null` for a name that
    /// is not a name, or a label this device does not have.
    pub fn rename_tag(&self, label: String, name: String) -> Result<Option<String>, String> {
        let label = label_id(&label)?;
        Ok(self
            .shared
            .locked()?
            .rename_tag(label, &name)
            .map_err(|e| e.to_string())?
            .map(|survivor| survivor.encode()))
    }

    /// Throw a label away: off every note that has it, and gone from the menu.
    /// The notes themselves are untouched.
    pub fn forget_tag(&self, label: String) -> Result<(), String> {
        let label = label_id(&label)?;
        self.shared
            .locked()?
            .forget_tag(label)
            .map_err(|e| e.to_string())
    }

    pub fn set_archived(&self, id: String, archived: bool) -> Result<(), String> {
        let id = note_id(&id)?;
        self.shared
            .locked()?
            .set_archived(id, archived)
            .map_err(|e| e.to_string())
    }

    /// Throw a note away, or take it back out of the trash.
    ///
    /// Reversible, and nothing is destroyed: the note is still on the disk and
    /// still syncing, and what has changed is that this person does not see it.
    /// [`Library::forget_note`] is the irreversible one.
    pub fn set_trashed(&self, id: String, trashed: bool) -> Result<(), String> {
        let id = note_id(&id)?;
        self.shared
            .locked()?
            .set_trashed(id, trashed)
            .map_err(|e| e.to_string())
    }

    /// Permanently delete a note from this person's account and synced devices.
    ///
    /// There is no undo, which is why the UI offers it only from the trash and
    /// only behind a confirmation. Other devices purge their copies when they
    /// sync. A collaborator's separate account and relay history remain. See
    /// `docs/adr/0010-forgetting-a-note.md`.
    pub fn forget_note(&self, id: String) -> Result<(), String> {
        let id = note_id(&id)?;
        self.forget(id)
    }

    /// Rotate a note's key and remove every collaborator while retaining the
    /// creator's own account devices. This cannot erase a copy somebody already
    /// decrypted; it prevents them from opening the rotated snapshot and every
    /// later edit.
    pub fn revoke_collaborators(&self, id: String) -> Result<(), String> {
        let id = note_id(&id)?;
        let envelope = self
            .shared
            .locked()?
            .revoke_collaborators(id)
            .map_err(|e| e.to_string())?;
        self.shared.rekey(id, envelope);
        Ok(())
    }

    #[frb(sync)]
    pub fn collaborators(&self, id: String) -> Result<CollaboratorsView, String> {
        let id = note_id(&id)?;
        let mut core = self.shared.locked()?;
        core.open_note(id).map_err(|e| e.to_string())?;
        Ok(CollaboratorsView {
            people: core
                .collaborators(id)
                .map_err(|e| e.to_string())?
                .into_iter()
                .map(Into::into)
                .collect(),
            can_manage: core.can_manage_collaborators(id),
            current_device: core.device_key().encode(),
        })
    }

    /// Forget every note in the trash. Returns how many there were.
    pub fn empty_trash(&self) -> Result<u32, String> {
        // Asked for the list, then told to empty it, rather than one call: every
        // one of these notes has a socket that has to be closed before its rows
        // go, and the core cannot close a socket.
        let trashed = self
            .shared
            .locked()?
            .trashed_notes()
            .map_err(|e| e.to_string())?;
        let mut forgotten = 0u32;
        for id in trashed {
            self.forget(id)?;
            forgotten += 1;
        }
        Ok(forgotten)
    }

    /// The socket first, then the rows.
    ///
    /// The order matters and is not obvious. An envelope that arrives for a note
    /// the core has just forgotten cannot be applied — there is no key — but the
    /// engine still records how far it has read, and that sync cursor is a store
    /// row, and a store row is enough to make the note exist again as far as the
    /// store is concerned. So the engine is asked to stop, and its status channel
    /// removed, before the core is asked to forget anything.
    ///
    /// It closes the window rather than the possibility: the engine's task may
    /// already be mid-delivery when the shutdown is queued. What that can leave
    /// is one sealed cursor row for a note with no key, which holds no content
    /// and draws no card — `adopt_unplaced` in the core is what keeps it from
    /// turning into anything more.
    fn forget(&self, id: NoteId) -> Result<(), String> {
        self.shared.disconnect_note(id)?;
        self.shared
            .locked()?
            .forget_note(id)
            .map_err(|e| e.to_string())
    }

    /// Whether the grid is a masonry or one column, as this person last left it.
    #[frb(sync)]
    pub fn layout(&self) -> Result<NoteLayout, String> {
        Ok(self.shared.locked()?.layout().into())
    }

    pub fn set_layout(&self, layout: NoteLayout) -> Result<(), String> {
        self.shared
            .locked()?
            .set_layout(layout.into())
            .map_err(|e| e.to_string())
    }

    // -- the person ---------------------------------------------------------
    //
    // Twelve words, and what they unlock. There is no server-side account here:
    // no password, no email address, no login. A person *is* a recovery phrase,
    // and a device belongs to them if it can derive the same account root from
    // it — see [ADR 0013](../../docs/adr/0013-recovery-phrase.md).

    /// Whether this device has an account, and so whether its notes can reach
    /// another device of the same person.
    #[frb(sync)]
    pub fn has_account(&self) -> Result<bool, String> {
        Ok(self.shared.locked()?.account_id().is_some())
    }

    /// Mint an account for this device and return its recovery phrase, once.
    ///
    /// The twelve words are returned one at a time because a screen numbers
    /// them; they are not stored, here or anywhere, and this is the only moment
    /// they exist. **The caller must make somebody write them down before the
    /// moment passes** — a phrase nobody has is the one state this design cannot
    /// recover from, and the design doc's risk row asks for a backup check for
    /// exactly that reason.
    ///
    /// The account document starts syncing immediately if a relay has been
    /// named, which is what makes the second device possible.
    pub fn create_account(&self) -> Result<Vec<String>, String> {
        let words = {
            let mut core = self.shared.locked()?;
            core.create_account()
                .map_err(|e| e.to_string())?
                .word_list()
        };
        self.shared.connect_new_documents()?;
        Ok(words)
    }

    /// Adopt the account twelve words name, keeping what this device already
    /// has.
    ///
    /// Nothing is fetched here and nothing appears at once: this makes the
    /// device *able* to open that person's account document, and the notes,
    /// their keys and their placements arrive over the relay afterwards. A
    /// device with no relay named adopts perfectly well and simply waits.
    ///
    /// The words are forgiving about case and spacing and not about themselves:
    /// a swap or a typo is refused with a message to show, rather than deriving
    /// a perfectly good account that has nothing in it.
    pub fn adopt_account(&self, phrase: String) -> Result<(), String> {
        let phrase =
            easynotes_core::account::RecoveryPhrase::parse(&phrase).map_err(|e| e.to_string())?;
        self.shared
            .locked()?
            .adopt_account(&phrase)
            .map_err(|e| e.to_string())?;
        self.shared.connect_new_documents()?;
        Ok(())
    }

    // -- invites ------------------------------------------------------------
    //
    // How a note reaches another person: a link that carries no key, a request
    // the owner sees, and a key handed over only when they say so. The core
    // keeps what must survive a restart and does the cryptography; the calls
    // to the relay are `easynotes_core::sync::invite`; what is here is the
    // order they go in and the lock never being held across one of them.
    //
    // See `docs/adr/0015-invites.md`.

    /// Open an invite to a note and return the link to send.
    ///
    /// Needs a relay: the invite lives on it until somebody asks to join.
    pub async fn create_invite(&self, note: String) -> Result<InviteView, String> {
        let note = note_id(&note)?;
        let http = self.shared.http_base()?;
        let mut invite = {
            let core = self.shared.locked()?;
            core.new_invite(note).map_err(|e| e.to_string())?
        };
        let (secret, owner) = (invite.secret.clone(), invite.owner.clone());
        let granted = on_network(async move {
            invite::open(&http, &secret, &owner, INVITE_TTL_SECS)
                .await
                .map_err(|e| e.to_string())
        })
        .await?;
        invite.expires_at = now() + granted as i64;
        self.shared
            .locked()?
            .remember_invite(&invite)
            .map_err(|e| e.to_string())?;
        Ok(InviteView::from(&invite))
    }

    /// The invites this device has open to `note`, oldest first. Local: it asks
    /// the relay nothing, so an invite that has quietly expired is still here
    /// until [`Library::invite_requests`] finds out.
    #[frb(sync)]
    pub fn invites(&self, note: String) -> Result<Vec<InviteView>, String> {
        let note = note_id(&note)?;
        let core = self.shared.locked()?;
        Ok(core
            .invites()
            .map_err(|e| e.to_string())?
            .iter()
            .filter(|i| i.note == note)
            .map(InviteView::from)
            .collect())
    }

    /// Every invite this device has open, to any note.
    #[frb(sync)]
    pub fn all_invites(&self) -> Result<Vec<InviteView>, String> {
        let core = self.shared.locked()?;
        Ok(core
            .invites()
            .map_err(|e| e.to_string())?
            .iter()
            .map(InviteView::from)
            .collect())
    }

    /// Who has asked to join through an invite, as the owner is shown them.
    ///
    /// An invite the relay no longer has, or that has closed, is forgotten
    /// here on the way: there is nothing more it can be used for, and a list of
    /// open invites that included it would be a list of things to wonder about.
    pub async fn invite_requests(&self, invite: String) -> Result<InviteRequestsView, String> {
        let outgoing = self.shared.outgoing(&invite)?;
        let http = self.shared.http_base()?;
        let (secret, owner) = (outgoing.secret.clone(), outgoing.owner.clone());
        // The relay saying no is an answer and a network failing is not, and
        // only the first means the invite is gone.
        let listed = on_network(async move {
            Ok(invite::list(&http, &secret, &owner)
                .await
                .map_err(|e| (matches!(e, CoreError::Invite(_)), e.to_string())))
        })
        .await?;
        let listing = match listed {
            Ok(listing) => listing,
            Err((gone, message)) => {
                if gone {
                    self.shared.forget_invite(&outgoing)?;
                }
                return Err(message);
            }
        };
        if !listing.open {
            for request in &listing.requests {
                if matches!(request.state, RequestState::Approved) {
                    if let Some(join) = &request.join {
                        let envelope = self
                            .shared
                            .locked()?
                            .record_collaborator(&outgoing, join)
                            .map_err(|e| e.to_string())?;
                        if let Some(envelope) = envelope {
                            self.shared.publish(outgoing.note, envelope);
                        }
                    }
                }
            }
            self.shared.forget_invite(&outgoing)?;
        }
        Ok(InviteRequestsView {
            open: listing.open,
            expires_in_secs: clamp_secs(listing.expires_in),
            requests: listing
                .requests
                .iter()
                .map(|r| JoinRequestView {
                    id: r.request.clone(),
                    name: r.join.as_ref().map(|j| j.name.clone()).unwrap_or_default(),
                    code: r
                        .join
                        .as_ref()
                        .map(|j| j.code(&outgoing.secret))
                        .unwrap_or_default(),
                    state: r.state.into(),
                    readable: r.join.is_some(),
                })
                .collect(),
        })
    }

    /// Let the person who made `request` in: seal the note to them and close
    /// the invite. Every other request on it is declined by the relay.
    pub async fn approve_request(&self, invite: String, request: String) -> Result<(), String> {
        let outgoing = self.shared.outgoing(&invite)?;
        let http = self.shared.http_base()?;

        // The request is fetched again rather than taken from the screen, so
        // what is sealed to is exactly what the relay holds under that name.
        let (secret, owner, base) = (
            outgoing.secret.clone(),
            outgoing.owner.clone(),
            http.clone(),
        );
        let listing = on_network(async move {
            invite::list(&base, &secret, &owner)
                .await
                .map_err(|e| e.to_string())
        })
        .await?;
        let join = listing
            .requests
            .into_iter()
            .find(|r| r.request == request)
            .ok_or("That request is no longer there.")?
            .join
            .ok_or("That request cannot be read, so it cannot be approved.")?;

        let grant = {
            let core = self.shared.locked()?;
            core.seal_grant(&outgoing, &join)
                .map_err(|e| e.to_string())?
        };
        let (secret, owner) = (outgoing.secret.clone(), outgoing.owner.clone());
        on_network(async move {
            invite::approve(&http, &secret, &owner, &request, &grant)
                .await
                .map_err(|e| e.to_string())
        })
        .await?;
        let envelope = self
            .shared
            .locked()?
            .record_collaborator(&outgoing, &join)
            .map_err(|e| e.to_string())?;
        if let Some(envelope) = envelope {
            self.shared.publish(outgoing.note, envelope);
        }
        self.shared.forget_invite(&outgoing)
    }

    /// Turn a request away. The invite stays open for someone else.
    pub async fn decline_request(&self, invite: String, request: String) -> Result<(), String> {
        let outgoing = self.shared.outgoing(&invite)?;
        let http = self.shared.http_base()?;
        let (secret, owner) = (outgoing.secret.clone(), outgoing.owner.clone());
        on_network(async move {
            invite::decline(&http, &secret, &owner, &request)
                .await
                .map_err(|e| e.to_string())
        })
        .await
    }

    /// Take an invite down, on the relay and here. Anyone holding the link is
    /// told it does not exist.
    pub async fn withdraw_invite(&self, invite: String) -> Result<(), String> {
        let outgoing = self.shared.outgoing(&invite)?;
        let http = self.shared.http_base()?;
        let (secret, owner) = (outgoing.secret.clone(), outgoing.owner.clone());
        let withdrawn = on_network(async move {
            Ok(invite::withdraw(&http, &secret, &owner)
                .await
                .map_err(|e| (matches!(e, CoreError::Invite(_)), e.to_string())))
        })
        .await?;
        match withdrawn {
            // Already gone from the relay is as good as withdrawn.
            Ok(()) | Err((true, _)) => self.shared.forget_invite(&outgoing),
            Err((false, message)) => Err(message),
        }
    }

    /// Ask to join through a link, calling yourself `name` to the owner.
    ///
    /// Returns what to show while waiting, including the six digits the owner
    /// will see beside the request.
    pub async fn request_join(
        &self,
        link: String,
        name: String,
    ) -> Result<PendingJoinView, String> {
        let secret = InviteSecret::from_link(&link).map_err(|e| e.to_string())?;
        let http = self.shared.http_base()?;
        let guest = GuestKey::random();
        let sealed_request = self
            .shared
            .locked()?
            .seal_join_request(&secret, &guest, &name)
            .map_err(|e| e.to_string())?;
        let (s, payload) = (secret.clone(), sealed_request);
        let request = on_network(async move {
            invite::request(&http, &s, &payload)
                .await
                .map_err(|e| e.to_string())
        })
        .await?;
        let join = PendingJoin {
            secret,
            request,
            guest,
            name: name.trim().to_string(),
        };
        self.shared
            .locked()?
            .remember_join(&join)
            .map_err(|e| e.to_string())?;
        Ok(PendingJoinView::from(&join))
    }

    /// The join requests this device is waiting on.
    #[frb(sync)]
    pub fn pending_joins(&self) -> Result<Vec<PendingJoinView>, String> {
        let core = self.shared.locked()?;
        Ok(core
            .joins()
            .map_err(|e| e.to_string())?
            .iter()
            .map(PendingJoinView::from)
            .collect())
    }

    /// Ask the relay how a join request went, and act on the answer: an
    /// approved one adds the note and starts syncing it; a declined, expired
    /// or withdrawn one stops being waited on.
    pub async fn check_join(&self, invite: String) -> Result<JoinCheckView, String> {
        let join = {
            let core = self.shared.locked()?;
            core.joins()
                .map_err(|e| e.to_string())?
                .into_iter()
                .find(|j| j.secret.id().encode() == invite)
                .ok_or("This device is not waiting on that invite.")?
        };
        let http = self.shared.http_base()?;
        let (secret, guest, request) = (
            join.secret.clone(),
            join.guest.clone(),
            join.request.clone(),
        );
        let answer = on_network(async move {
            match invite::poll(&http, &secret, &guest, &request).await {
                Ok(answer) => Ok(Ok(answer)),
                Err(CoreError::Invite(message)) => Ok(Err(message)),
                Err(other) => Err(other.to_string()),
            }
        })
        .await?;
        let id = join.secret.id();
        match answer {
            Ok(Answer::Pending { .. }) => Ok(JoinCheckView::waiting()),
            Ok(Answer::Approved {
                note,
                key,
                owner,
                generation,
            }) => {
                let note = {
                    let mut core = self.shared.locked()?;
                    core.accept_owned_grant(id, note, key, owner, generation)
                        .map_err(|e| e.to_string())?
                };
                self.shared.connect_note(note)?;
                Ok(JoinCheckView {
                    state: JoinStateView::Joined,
                    note: Some(note.encode()),
                    message: None,
                })
            }
            Ok(Answer::Declined) => {
                self.shared
                    .locked()?
                    .forget_join(id)
                    .map_err(|e| e.to_string())?;
                Ok(JoinCheckView {
                    state: JoinStateView::Declined,
                    note: None,
                    message: None,
                })
            }
            Err(message) => {
                self.shared
                    .locked()?
                    .forget_join(id)
                    .map_err(|e| e.to_string())?;
                Ok(JoinCheckView {
                    state: JoinStateView::Gone,
                    note: None,
                    message: Some(message),
                })
            }
        }
    }

    /// Stop waiting on a join request. The relay keeps the request until the
    /// invite expires; the owner may still approve it, and nothing here will
    /// be listening.
    pub fn cancel_join(&self, invite: String) -> Result<(), String> {
        let id = InviteId::decode(&invite).map_err(|e| e.to_string())?;
        self.shared
            .locked()?
            .forget_join(id)
            .map_err(|e| e.to_string())
    }

    // -- sync ---------------------------------------------------------------

    /// Start syncing every note with the relay at `relay` (`http://host:port`
    /// or `wss://host`), and every note made or joined after this.
    ///
    /// Returns as soon as the engines are started, not when they are connected:
    /// there may be no network, and the app is not allowed to care. Watch
    /// [`Library::watch_sync`] to find out how it goes. Calling this again moves
    /// the device to a different relay.
    pub fn connect(&self, relay: String) -> Result<(), String> {
        // A new engine every time, as each note used to get a new connection:
        // this is also how a device is moved to a different relay.
        self.disconnect()?;
        let device = Device::open(self.shared.core.clone(), &relay)?;
        *self
            .shared
            .device
            .lock()
            .map_err(|_| "sync lock poisoned".to_string())? = Some(device);
        *self
            .shared
            .relay
            .lock()
            .map_err(|_| "relay lock poisoned".to_string())? = Some(relay);
        // `syncable` rather than the note list: the list is the grid, and a
        // note this device holds only a key for — one that arrived in the
        // account document and whose log has never been fetched — has no card
        // and is exactly the note that most needs a connection.
        let documents: Vec<NoteId> = {
            let core = self.shared.locked()?;
            let mut documents = core.syncable().map_err(|e| e.to_string())?;
            documents.extend(core.account_id());
            documents
        };
        for document in documents {
            self.shared.connect_note(document)?;
        }
        Ok(())
    }

    /// Send every edit that is waiting for the typing to pause, now.
    ///
    /// For when the app may not get another chance: going into the background,
    /// or a tab being hidden. Nothing waiting is at risk either way — it is in
    /// the log, and the next connection sends it — but a person who types a
    /// line and pockets the phone expects the other phone to have it.
    #[frb(sync)]
    pub fn send_held_edits(&self) -> Result<(), String> {
        let connections = self
            .shared
            .connections
            .lock()
            .map_err(|_| "sync lock poisoned".to_string())?;
        for connection in connections.values() {
            connection.flush();
        }
        Ok(())
    }

    /// Stop syncing everything. Edits still work and still queue; nothing is
    /// lost.
    pub fn disconnect(&self) -> Result<(), String> {
        *self
            .shared
            .relay
            .lock()
            .map_err(|_| "relay lock poisoned".to_string())? = None;
        let connections: Vec<Connection> = {
            let mut open = self
                .shared
                .connections
                .lock()
                .map_err(|_| "sync lock poisoned".to_string())?;
            open.drain().map(|(_, connection)| connection).collect()
        };
        for connection in connections {
            connection.shutdown();
        }
        let device = self
            .shared
            .device
            .lock()
            .map_err(|_| "sync lock poisoned".to_string())?
            .take();
        if let Some(device) = device {
            device.shutdown();
        }
        self.shared.statuses.all_offline();
        Ok(())
    }

    /// Where this device stands with the relay, over all its notes.
    #[frb(sync)]
    pub fn sync_status(&self) -> SyncStatus {
        self.shared.statuses.overall.borrow().clone()
    }

    pub async fn watch_sync(&self, sink: StreamSink<SyncStatus>) -> Result<(), String> {
        watch_status(
            self.shared.statuses.overall.subscribe(),
            self.stop.clone(),
            sink,
        )
        .await
    }

    /// End the watchers started above and let their tasks finish.
    ///
    /// Necessary rather than tidy: a watcher is parked on a channel, so without
    /// a wake it waits forever, the task leaks, and anything waiting for the
    /// stream to close — such as Dart cancelling its subscription — waits
    /// forever too.
    pub fn stop_watching(&self) -> Result<(), String> {
        self.stop.store(true, Ordering::SeqCst);
        let overall = self.shared.statuses.overall.borrow().clone();
        self.shared.statuses.overall.send_replace(overall);
        // The note list watcher is parked on its own channel and needs the same
        // wake. Re-sending the list it already has changes nothing and moves it.
        //
        // A closed library needs no wake: closing dropped the core, and with it
        // the channel's sender, which ends the watcher by itself. That is not
        // only the second `dispose()`'s case — a library retired by the next
        // open of its directory ([`OpenLibraries`]) is closed before its own
        // Dart side gets round to disposing it, if it ever does.
        if let Ok(core) = self.shared.locked() {
            core.touch_notes();
        }
        Ok(())
    }

    /// Shut the store. The library is unusable afterwards, and every method on
    /// it — or on a [`Note`] still held from it — answers "the library is
    /// closed".
    ///
    /// This is the last thing `dispose()` does on the Dart side, and it has to
    /// exist rather than being left to the handle going out of scope. The
    /// bridge frees a `RustOpaque` when Dart collects it, which is not a moment
    /// the app picks, so without this the SQLite file stays open for an
    /// unbounded time after the app believes it let go — and the last WAL
    /// checkpoint, the one `forget_note` relies on to get a deleted string off
    /// the disk, never runs.
    ///
    /// It went unnoticed because POSIX lets you unlink an open file: the tests
    /// delete their temp directory in teardown and Linux allows it. Windows
    /// does not, which is how this was found.
    ///
    /// Call [`Library::disconnect`] and [`Library::stop_watching`] first, as
    /// `dispose()` does — closing does not wait for an engine mid-flight, it
    /// only makes sure the engine finds nothing to write to.
    pub fn close(&self) -> Result<(), String> {
        self.shared.core.close()
    }
}

/// One open note.
///
/// `Mutex` on the core because the bridge hands Dart a shared reference and
/// dispatches calls onto a worker pool, so two edits can arrive on different
/// threads. The core is `Send` (there is a test in `core/tests/convergence.rs`
/// that keeps it so), and an edit is fast enough that a lock is not worth
/// optimising away.
pub struct Note {
    shared: Arc<Shared>,
    id: NoteId,
    /// This note's sync status, for the life of the note rather than of one
    /// connection.
    status: Arc<watch::Sender<SyncStatus>>,
    stop: Arc<AtomicBool>,
}

impl Note {
    #[frb(sync)]
    pub fn id(&self) -> String {
        self.id.encode()
    }

    #[frb(sync)]
    pub fn view(&self) -> Result<NoteView, String> {
        let core = self.shared.locked()?;
        core.view(self.id)
            .map(Into::into)
            .map_err(|e| e.to_string())
    }

    /// Every view from now on, starting with the current one.
    ///
    /// This is the only way the UI learns anything. A local edit and an edit
    /// arriving from another device both come out here, which is what keeps the
    /// one-device and two-device cases identical.
    pub async fn watch(&self, sink: StreamSink<NoteView>) -> Result<(), String> {
        let mut rx = {
            self.shared
                .locked()?
                .watch(self.id)
                .map_err(|e| e.to_string())?
        };
        let stop = self.stop.clone();

        // An async task rather than a thread, and not as a style preference:
        // `std::thread::spawn` is unsupported on `wasm32-unknown-unknown` and
        // panics with `operation not supported on this platform`, which took the
        // whole web app down on the first frame. Awaiting works on both targets,
        // and the bridge drives this future for the life of the stream.
        //
        // `changed()` is runtime-agnostic, so nothing polls and an idle watcher
        // costs nothing.
        if sink.add(rx.borrow_and_update().clone().into()).is_err() {
            return Ok(());
        }
        loop {
            if rx.changed().await.is_err() {
                return Ok(()); // the library was dropped
            }
            if stop.load(Ordering::SeqCst) {
                return Ok(());
            }
            let view = rx.borrow_and_update().clone();
            if sink.add(view.into()).is_err() {
                return Ok(()); // Dart stopped listening
            }
        }
    }

    /// Where this note stands with the relay.
    #[frb(sync)]
    pub fn sync_status(&self) -> SyncStatus {
        self.status.borrow().clone()
    }

    pub async fn watch_sync(&self, sink: StreamSink<SyncStatus>) -> Result<(), String> {
        watch_status(self.status.subscribe(), self.stop.clone(), sink).await
    }

    /// End this handle's watchers and let their tasks finish. The note stays
    /// open in the library, because another screen may be showing it.
    pub fn stop_watching(&self) -> Result<(), String> {
        self.stop.store(true, Ordering::SeqCst);
        // The clone is bound first on purpose: `borrow()` holds a read guard
        // until the end of the statement, and a send inside that statement would
        // wait for a lock the same expression is holding.
        let unchanged = self.status.borrow().clone();
        self.status.send_replace(unchanged);
        // Closed: the note's channel went with the core, and its watcher with
        // that — as for [`Library::stop_watching`].
        match self.shared.locked() {
            Ok(core) => core.touch(self.id).map_err(|e| e.to_string()),
            Err(_) => Ok(()),
        }
    }

    /// The blob another device pastes to join. Slice 3 turns it into an invite
    /// link's fragment without changing the bytes.
    #[frb(sync)]
    pub fn share_blob(&self) -> Result<String, String> {
        let core = self.shared.locked()?;
        core.export_share_blob(self.id).map_err(|e| e.to_string())
    }

    // -- edits --------------------------------------------------------------
    //
    // Each one applies the edit locally and hands the sealed envelope to the
    // relay. The local half is what the UI sees, and it never waits for the
    // remote half: the edit is in the log before `publish` is called, and stays
    // there whether or not there is a network.
    //
    // Typing — a title, a body, an item's text — goes by `publish_soon`, and is
    // sent once it pauses; everything else goes at once, because it is one
    // gesture and someone on a shared list is waiting to see it.

    /// Send whatever typing on this note is waiting to go. What a screen does
    /// as it closes, so that leaving a note is not a second's wait to share it.
    #[frb(sync)]
    pub fn send_held_edits(&self) {
        self.shared.with_connection(self.id, Connection::flush);
    }

    pub fn set_title(&self, title: String) -> Result<(), String> {
        let envelope = {
            let mut core = self.shared.locked()?;
            core.set_title(self.id, title).map_err(|e| e.to_string())?
        };
        self.shared.publish_soon(self.id, envelope);
        Ok(())
    }

    /// Say that this person changed the body from `from` to `to`. The core
    /// replays that change onto the body as it now stands, so a peer's edit
    /// that arrived after the screen last drew is kept rather than typed over.
    pub fn edit_body(&self, from: String, to: String) -> Result<(), String> {
        let envelope = {
            let mut core = self.shared.locked()?;
            core.edit_body(self.id, &from, &to)
                .map_err(|e| e.to_string())?
        };
        if let Some(envelope) = envelope {
            self.shared.publish_soon(self.id, envelope);
        }
        Ok(())
    }

    /// Switch the note between text and checklist, carrying its content across:
    /// lines become items, or items become lines.
    pub fn set_kind(&self, kind: NoteKindView) -> Result<(), String> {
        let envelope = {
            let mut core = self.shared.locked()?;
            core.set_kind(self.id, kind.into())
                .map_err(|e| e.to_string())?
        };
        if let Some(envelope) = envelope {
            self.shared.publish(self.id, envelope);
        }
        Ok(())
    }

    pub fn add_item(&self, text: String) -> Result<String, String> {
        let (id, envelope) = {
            let mut core = self.shared.locked()?;
            core.add_item(self.id, text).map_err(|e| e.to_string())?
        };
        self.shared.publish(self.id, envelope);
        Ok(id.encode())
    }

    pub fn set_item_text(&self, item: String, text: String) -> Result<(), String> {
        let envelope = {
            let mut core = self.shared.locked()?;
            core.set_item_text(self.id, item_id(&item)?, text)
                .map_err(|e| e.to_string())?
        };
        self.shared.publish_soon(self.id, envelope);
        Ok(())
    }

    pub fn set_checked(&self, item: String, checked: bool) -> Result<(), String> {
        let envelope = {
            let mut core = self.shared.locked()?;
            core.set_checked(self.id, item_id(&item)?, checked)
                .map_err(|e| e.to_string())?
        };
        self.shared.publish(self.id, envelope);
        Ok(())
    }

    pub fn move_item(&self, item: String, to_index: u32) -> Result<(), String> {
        let envelope = {
            let mut core = self.shared.locked()?;
            core.move_item(self.id, item_id(&item)?, to_index as usize)
                .map_err(|e| e.to_string())?
        };
        self.shared.publish(self.id, envelope);
        Ok(())
    }

    pub fn remove_item(&self, item: String) -> Result<(), String> {
        let envelope = {
            let mut core = self.shared.locked()?;
            core.remove_item(self.id, item_id(&item)?)
                .map_err(|e| e.to_string())?
        };
        self.shared.publish(self.id, envelope);
        Ok(())
    }
}

/// The status now, and every change to it, until Dart stops listening or
/// `stop_watching` says to. Shared by the library's status and each note's,
/// which are the same loop over different channels.
async fn watch_status(
    mut updates: watch::Receiver<SyncStatus>,
    stop: Arc<AtomicBool>,
    sink: StreamSink<SyncStatus>,
) -> Result<(), String> {
    if sink.add(updates.borrow_and_update().clone()).is_err() {
        return Ok(());
    }
    loop {
        if updates.changed().await.is_err() {
            return Ok(());
        }
        if stop.load(Ordering::SeqCst) {
            return Ok(());
        }
        if sink.add(updates.borrow_and_update().clone()).is_err() {
            return Ok(());
        }
    }
}
