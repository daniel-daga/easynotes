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
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use easynotes_core::account_transfer::{OpenedTransfer, TransferKeyPair};
use easynotes_core::crdt::{Colour, DefaultNoteDoc, Layout, RepeatRule};
use easynotes_core::ids::{InviteId, ItemId, LabelId, NoteId, ReminderId};
use easynotes_core::invite::InviteSecret;
use easynotes_core::note::{NoteCore, NoteKind, OutgoingInvite, PendingJoin};
use easynotes_core::store::{Keyring, MemoryKeyring, MemoryStore, OpStore};
use easynotes_core::sync::invite::{self, Answer};
use easynotes_core::sync::RequestState;
use easynotes_core::CoreError;
use flutter_rust_bridge::frb;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use tokio::sync::watch;

use crate::media::{Home, Store};
use easynotes_core::attachment::storage::{ImportHints, MediaStorage, Staged, ThumbnailInput};
use easynotes_core::attachment::{AttachmentKind, AttachmentState, Media, Variant};
use easynotes_core::ids::AttachmentId;
use easynotes_core::relay_access::RelayAccess;
use easynotes_core::sync::transfers::{
    Direction, Reach, TransferPeer, TransferQueue, TransferRunner, TransferState,
};

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
    /// The revision this view was published at. Strictly increasing for the
    /// life of an open note, never persisted and never sent to the relay, so a
    /// shared text field can tell its own echo from somebody else's edit
    /// without guessing from the text.
    ///
    /// A `u32`, not a `u64`: flutter_rust_bridge hands an `i64` to Dart as a
    /// `PlatformInt64`, which is a `BigInt` on the web — the trap
    /// [`SyncStatus::last_seq`] documents. Four billion views of one note in
    /// one process is beyond reach, and the counter is in memory only.
    pub rev: u32,
    pub kind: NoteKindView,
    pub title: String,
    pub body: String,
    /// The colour everyone on the note sees, and whether the note has recorded
    /// one at all. See `easynotes_core::crdt::NoteView`.
    pub colour: Option<NoteColour>,
    pub colour_set: bool,
    pub items: Vec<ItemView>,
    /// Live attachments, in display order. An import of this device's that is
    /// still waiting to be published is not here: see
    /// [`Note::pending_attachments`].
    pub attachments: Vec<AttachmentView>,
}

/// Whether an attachment is drawn or played.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttachmentKindView {
    Image,
    Audio,
}

/// One attachment, as Dart sees it: enough to lay out a gallery or a player
/// before any bytes are here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttachmentView {
    pub id: String,
    pub kind: AttachmentKindView,
    /// The person's file name. For showing, searching and saving a copy; never
    /// what decides what the bytes are.
    pub name: String,
    /// `image/jpeg`, `audio/mp4` and so on, read from the bytes.
    pub mime: String,
    /// The original's size in bytes.
    pub size: u64,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub duration_ms: Option<u64>,
    pub description: String,
    pub thumbnail_width: Option<u32>,
    pub thumbnail_height: Option<u32>,
    /// Saved on this device and not yet in the note: still to be uploaded, or
    /// uploading. Nobody else can see it yet.
    pub pending: bool,
}

impl AttachmentView {
    fn of(id: AttachmentId, media: &Media, description: String, pending: bool) -> Self {
        Self {
            id: id.encode(),
            kind: match media.kind() {
                AttachmentKind::Image => AttachmentKindView::Image,
                AttachmentKind::Audio => AttachmentKindView::Audio,
            },
            name: media.name.clone(),
            mime: media.format.mime().to_string(),
            size: media.original.len,
            width: media.width,
            height: media.height,
            duration_ms: media.duration_ms,
            description,
            thumbnail_width: media.thumbnail.as_ref().map(|t| t.width),
            thumbnail_height: media.thumbnail.as_ref().map(|t| t.height),
            pending,
        }
    }
}

/// Where one attachment transfer is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransferStateView {
    /// Saved here and waiting for its note to catch up with the relay —
    /// "Waiting to upload" — or, for a download, for a connection.
    Waiting,
    /// `done` of `of` chunks moved in this attempt.
    Running,
    /// Failed for a reason the network explains; it will be tried again.
    Retrying,
    /// Refused, and waiting for [`Library::retry_transfers`]. An upload's
    /// local copy is kept.
    Failed,
}

/// One transfer that has not finished. A finished one is not listed: an
/// upload is then in the note, and a download is on this device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransferView {
    pub note: String,
    pub attachment: String,
    /// Up (an import going to the relay) or down.
    pub upload: bool,
    /// For a download: the thumbnail rather than the original.
    pub thumbnail: bool,
    pub state: TransferStateView,
    pub done: u32,
    pub of: u32,
    pub error: Option<String>,
}

/// The attachment queue that is running: the relay it runs with (`None` for
/// none), that relay's access key, and the queue.
type RunningTransfers = (Option<String>, Option<RelayAccess>, TransferQueue);

fn transfer_views(map: &easynotes_core::sync::transfers::TransferMap) -> Vec<TransferView> {
    let mut views: Vec<TransferView> = map
        .iter()
        .map(|(transfer, state)| {
            let (state, done, of, error) = match state {
                TransferState::Waiting => (TransferStateView::Waiting, 0, 0, None),
                TransferState::Running { done, of } => {
                    (TransferStateView::Running, *done, *of, None)
                }
                TransferState::Retrying { error } => {
                    (TransferStateView::Retrying, 0, 0, Some(error.clone()))
                }
                TransferState::Failed { error } => {
                    (TransferStateView::Failed, 0, 0, Some(error.clone()))
                }
            };
            TransferView {
                note: transfer.note.encode(),
                attachment: transfer.id.encode(),
                upload: transfer.direction == Direction::Up,
                thumbnail: transfer.direction == Direction::Down(Variant::Thumbnail),
                state,
                done,
                of,
                error,
            }
        })
        .collect();
    views.sort_by(|a, b| {
        (&a.note, &a.attachment, a.upload).cmp(&(&b.note, &b.attachment, b.upload))
    });
    views
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

impl From<easynotes_core::note::NoteViewAt> for NoteView {
    fn from(at: easynotes_core::note::NoteViewAt) -> Self {
        let v = at.view;
        Self {
            rev: at.rev,
            kind: v.kind.into(),
            title: v.title,
            body: v.body,
            colour: v.colour.map(Into::into),
            colour_set: v.colour_set,
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
            attachments: v
                .attachments
                .into_iter()
                .map(|a| AttachmentView::of(a.id, &a.media, a.description, false))
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
    /// The first few lines of the body, then the first few items still to do.
    /// A card shows these under the title, and most shopping lists do not have
    /// one. Crossed-off items are left out.
    pub preview: Vec<String>,
    /// How many live pictures and sound files the note holds, for a card to
    /// say so and a search to filter by, without opening it.
    pub images: u32,
    pub audio: u32,
    /// The picture a card shows above its title, by attachment ID, and its
    /// thumbnail's size so the card can lay out before the bytes are here.
    /// Its bytes come from [`Library::read_cover`].
    pub cover: Option<String>,
    pub cover_width: Option<u32>,
    pub cover_height: Option<u32>,
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
            images: s.images as u32,
            audio: s.audio as u32,
            cover: s.cover.as_ref().map(|(id, _)| id.encode()),
            cover_width: s
                .cover
                .as_ref()
                .and_then(|(_, m)| m.thumbnail.as_ref().map(|t| t.width)),
            cover_height: s
                .cover
                .as_ref()
                .and_then(|(_, m)| m.thumbnail.as_ref().map(|t| t.height)),
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

/// One person with access to a note, as the share screen lists them.
pub struct PersonView {
    /// The member ID: one per person, however many phones.
    pub member: String,
    /// The invite that let them in. Empty for the owner.
    pub invite: String,
    /// Chosen by the person. A label, not a verified identity.
    pub name: String,
    pub owner: bool,
    /// The person looking at the screen.
    pub me: bool,
    /// Asked to be removed, and not yet: “Removing… finishes when you're
    /// online”.
    pub removing: bool,
}

/// Everybody with access to a note, and what is on its way.
pub struct PeopleView {
    /// Whether anyone but the owner has been let in.
    pub shared: bool,
    /// Only the owner adds and removes people.
    pub can_manage: bool,
    pub people: Vec<PersonView>,
    /// People approved whose package has not yet taken effect.
    pub adding: Vec<String>,
}

/// A note this device was removed from, for the one message it shows.
pub struct RemovalView {
    pub note: String,
    pub title: String,
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
    /// Local acknowledgement watermark for refresh completion.
    pub acknowledged: u64,
    /// Automatic typing batches sent by the sync engine.
    pub typing_batches: u64,
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
            acknowledged: 0,
            typing_batches: 0,
            last_seq: 0,
            last_error: None,
        }
    }

    /// What a device with no notes on it shows. See [`SyncState::Empty`].
    pub(crate) fn nothing_to_sync() -> Self {
        Self {
            state: SyncState::Empty,
            pending: 0,
            acknowledged: 0,
            typing_batches: 0,
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
        let mut acknowledged = 0u64;
        let mut typing_batches = 0u64;
        let mut last_seq = 0i64;
        let mut last_error = None;
        for status in each {
            seen = true;
            worst = worst.min(status.state);
            pending = pending.saturating_add(status.pending);
            acknowledged = acknowledged.saturating_add(status.acknowledged);
            typing_batches = typing_batches.saturating_add(status.typing_batches);
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
            acknowledged,
            typing_batches,
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
            acknowledged: s.acknowledged,
            typing_batches: s.typing_batches,
            last_seq: s.last_seq,
            last_error: s.last_error,
        }
    }
}

// ---------------------------------------------------------------------------
// Where sync status goes
// ---------------------------------------------------------------------------

/// A sync status, as a stream for the watchers and as a value for a getter.
///
/// Two copies because they are read on different threads. Stream methods copy
/// their receiver and start watcher tasks on a Web Worker; a getter is a
/// `#[frb(sync)]` call, which runs on the page's main thread. Reading a
/// `watch` channel takes the `std::sync::RwLock` inside it, and a worker
/// reporting a status can be holding that lock, so a getter reading the
/// channel on the main thread waits for it with `Atomics.wait`, which throws
/// there (ADR 0002). The getter's copy is behind a lock taken with
/// [`crate::lock::lock`] instead, which never waits.
pub(crate) struct StatusCell {
    tx: watch::Sender<SyncStatus>,
    last: Mutex<SyncStatus>,
}

impl StatusCell {
    fn new(status: SyncStatus) -> Self {
        Self {
            tx: watch::channel(status.clone()).0,
            last: Mutex::new(status),
        }
    }

    /// The status now. Safe on the page's main thread.
    fn get(&self) -> SyncStatus {
        crate::lock::lock(&self.last)
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Both copies change under the value's lock, so a getter never sees one
    /// status while the watchers are told another.
    fn send_replace(&self, status: SyncStatus) {
        let mut last =
            crate::lock::lock(&self.last).unwrap_or_else(|poisoned| poisoned.into_inner());
        *last = status.clone();
        self.tx.send_replace(status);
    }

    /// Wake every watcher with the status they already have, so that one told
    /// to stop notices.
    fn resend(&self) {
        self.send_replace(self.get());
    }

    fn subscribe(&self) -> watch::Receiver<SyncStatus> {
        self.tx.subscribe()
    }
}

/// One note's status channel, and which connection is allowed to write to it.
struct NoteStatus {
    tx: Arc<StatusCell>,
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
    overall: StatusCell,
    /// The attachment queue, and the relay it runs with (`None` for a library
    /// that has never been connected, whose notes are local). Here rather than
    /// in [`Shared`] because a status change is what moves a waiting upload:
    /// every report kicks it.
    transfers: Mutex<Option<RunningTransfers>>,
    /// What the queue says, as Dart sees it. A channel of its own rather than
    /// the queue's, so that a watcher outlives the queue being replaced when
    /// the library is connected.
    transfer_views: watch::Sender<Vec<TransferView>>,
}

impl Statuses {
    fn new() -> Self {
        Self {
            per_note: Mutex::new(HashMap::new()),
            // Not `offline()`: there are no notes yet, by construction, so this
            // is the empty fold's answer and has to agree with it — otherwise a
            // library that never refreshes (because it never gets a note) keeps
            // reporting a relay problem it does not have.
            overall: StatusCell::new(SyncStatus::nothing_to_sync()),
            transfers: Mutex::new(None),
            transfer_views: watch::channel(Vec::new()).0,
        }
    }

    fn queue(&self) -> Option<TransferQueue> {
        crate::lock::lock(&self.transfers)
            .ok()
            .and_then(|held| held.as_ref().map(|(_, _, queue)| queue.clone()))
    }

    /// One note's state now, or `None` if it has no channel.
    fn state(&self, note: NoteId) -> Option<SyncState> {
        self.existing(note, |status| status.tx.get().state)
    }

    /// Synced, with nothing of this device's waiting for the relay.
    fn acknowledged(&self, note: NoteId) -> bool {
        self.existing(note, |status| {
            let now = status.tx.get();
            now.state == SyncState::Synced && now.pending == 0
        })
        .unwrap_or(false)
    }

    fn entry<T>(&self, note: NoteId, f: impl FnOnce(&mut NoteStatus) -> T) -> T {
        let mut per_note = crate::lock::lock(&self.per_note).expect("status lock poisoned");
        f(per_note.entry(note).or_insert_with(|| NoteStatus {
            tx: Arc::new(StatusCell::new(SyncStatus::offline())),
            epoch: 0,
        }))
    }

    /// The same, for a note that must already have a channel. `None` means it
    /// does not — which, since [`Statuses::forget`], means the note is gone.
    fn existing<T>(&self, note: NoteId, f: impl FnOnce(&mut NoteStatus) -> T) -> Option<T> {
        let mut per_note = crate::lock::lock(&self.per_note).expect("status lock poisoned");
        per_note.get_mut(&note).map(f)
    }

    /// The channel for one note, created on first use.
    ///
    /// It belongs to the note rather than to a connection, so the UI subscribes
    /// once and keeps seeing the truth across a disconnect, a reconnect, or a
    /// move to a different relay.
    fn channel(&self, note: NoteId) -> Arc<StatusCell> {
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
            if let Some(queue) = self.queue() {
                queue.kick();
            }
        }
    }

    /// A note is gone for good. Its epoch ends, so anything its engine says from
    /// here is dropped, and its channel goes with it so that the device's status
    /// stops counting a note that does not exist toward what is pending.
    fn forget(&self, note: NoteId) {
        let mut per_note = crate::lock::lock(&self.per_note).expect("status lock poisoned");
        per_note.remove(&note);
        drop(per_note);
        self.refresh();
    }

    fn refresh(&self) {
        let per_note = crate::lock::lock(&self.per_note).expect("status lock poisoned");
        let each: Vec<SyncStatus> = per_note.values().map(|status| status.tx.get()).collect();
        drop(per_note);
        self.overall.send_replace(SyncStatus::combined(each.iter()));
    }

    /// Nothing is connected any more. Said here rather than left to the engines,
    /// whose last word may not arrive before their tasks are gone — and whose
    /// last word is now ignored, because ending an epoch is what this is.
    fn all_offline(&self) {
        let mut per_note = crate::lock::lock(&self.per_note).expect("status lock poisoned");
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
        let guard = crate::lock::lock(&self.core).map_err(|_| "core lock poisoned".to_string())?;
        if guard.is_none() {
            return Err(closed());
        }
        Ok(Open(guard))
    }

    /// Drop the core, and with it the store — which is what shuts the SQLite
    /// connection and lets the last WAL checkpoint run. Closing twice is not an
    /// error: the second one has nothing to do.
    fn close(&self) -> Result<(), String> {
        let taken = crate::lock::lock(&self.core)
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

    /// The relay asked for a base, and the engine found this device caught up
    /// with nothing in flight: the core says whether it can vouch for the rest.
    fn compaction(
        &self,
        note: NoteId,
        covers: i64,
    ) -> easynotes_core::error::Result<Option<easynotes_core::compaction::Request>> {
        self.open()?.compaction(note, covers)
    }

    /// The account document's backlog has landed: take the note keys it carries,
    /// and connect whatever they turned into a note this device can read.
    ///
    /// The second half is why a peer knows about its device at all. A note
    /// adopted this way has a key and nothing else — no log, no index entry, no
    /// card — so nothing else on this device would ever notice it, and its
    /// contents arrive only over a connection it does not yet have.
    ///
    /// A note settling is when its access packages are acted on: a new key
    /// switched to, an owner's change published, or this device's copy
    /// deleted because the person was removed — after which the note has no
    /// business keeping a connection, and the same retirement lets it go.
    fn settled(
        &self,
        note: NoteId,
    ) -> easynotes_core::error::Result<easynotes_core::note::Outgoing> {
        let (out, reconnect) = {
            let mut core = self.open()?;
            if core.account_id() == Some(note) {
                core.adopt_note_keys()?;
                (Default::default(), true)
            } else {
                let out = core.settle_note(note)?;
                let removed = out.removed;
                (out, removed)
            }
        };
        if reconnect {
            if let Some(device) = self.device.upgrade() {
                device
                    .connect_new_documents()
                    .map_err(easynotes_core::error::CoreError::Storage)?;
            }
        }
        Ok(out)
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
    /// The relay's access key, if its operator gave this device one ([ADR 0028](../../../../docs/decisions.md#adr-0028)). Named
    /// with the relay by [`Library::connect`], and not part of its address:
    /// changing it is the same relay, reconnected.
    access: Mutex<Option<RelayAccess>>,
    /// The relay this library belongs to: the canonical address of the first
    /// one it was connected to. It is kept across [`Library::disconnect`] and
    /// never changes, because everything in the store — the account, the
    /// device identity, the note keys, the cursors — was made with that relay,
    /// and another relay must get a library of its own.
    bound: Mutex<Option<String>>,
    /// The engine for that relay: one socket, every note on it.
    device: Mutex<Option<Device>>,
    connections: Mutex<HashMap<NoteId, Connection>>,
    statuses: Arc<Statuses>,
    /// Where attachment bytes are kept, opened on first use.
    media_home: Home,
    media_key: easynotes_core::crypto::MasterKey,
    media: tokio::sync::OnceCell<Arc<Store>>,
}

impl Shared {
    fn locked(&self) -> Result<Open<'_>, String> {
        self.core.locked()
    }

    /// Start syncing one note, replacing whatever connection it had.
    ///
    /// A subscription on the device's one socket, not a socket of its own
    /// ([ADR 0017](../../../../docs/decisions.md#adr-0017)): a
    /// device with twenty notes does one token handshake and holds one
    /// connection. Replacing a note's connection hands the engine a new handle
    /// for it and nothing more — its cursor and anything it is holding carry
    /// over, and the relay is not asked for its backlog again.
    fn connect_note(&self, note: NoteId) -> Result<(), String> {
        let connection = {
            let device = crate::lock::lock(&self.device).map_err(|_| "sync lock poisoned")?;
            let Some(device) = device.as_ref() else {
                return Ok(());
            };
            let epoch = self.statuses.begin(note);
            device.connect(note, self.statuses.clone(), epoch)?
        };
        let previous = crate::lock::lock(&self.connections)
            .map_err(|_| "sync lock poisoned".to_string())?
            .insert(note, connection);
        if let Some(previous) = previous {
            previous.shutdown();
        }
        Ok(())
    }

    /// The relay's HTTP address, for the calls that are not a socket, and
    /// its access key if it has one.
    fn http_base(&self) -> Result<(String, Option<RelayAccess>), String> {
        let relay = crate::lock::lock(&self.relay).map_err(|_| "relay lock poisoned")?;
        let relay = relay
            .as_ref()
            .ok_or("Sharing needs a relay, and this device is not connected to one.")?;
        let http = easynotes_core::sync::RelayEndpoint::parse(relay)
            .map(|e| e.http_base)
            .map_err(|e| e.to_string())?;
        let access = crate::lock::lock(&self.access)
            .map_err(|_| "relay lock poisoned")?
            .clone();
        Ok((http, access))
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
    /// reserved ID that must never reach a relay ([ADR 0013](../../../../docs/decisions.md#adr-0013)).
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
            let mut connections = crate::lock::lock(&self.connections)
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
            let open = crate::lock::lock(&self.connections)
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
        let connection = crate::lock::lock(&self.connections)
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
    /// ([ADR 0016](../../../../docs/decisions.md#adr-0016)).
    fn publish_soon(&self, note: NoteId, envelope: Vec<u8>) {
        self.with_connection(note, |c| c.publish_soon(envelope));
    }

    /// Ask the engine to act on a note's access packages now, rather than at
    /// the next arrival: an owner's change was just saved.
    fn settle(&self, note: NoteId) {
        self.with_connection(note, Connection::settle);
    }

    fn with_connection(&self, note: NoteId, then: impl FnOnce(&Connection)) {
        if let Ok(connections) = crate::lock::lock(&self.connections) {
            if let Some(connection) = connections.get(&note) {
                then(connection);
            }
        }
    }

    /// Hand an edit to the relay — unless a removal is waiting to be made, when
    /// it stays in the log: it is under the key the person being removed holds,
    /// and it reaches everyone else inside the removal's package instead
    /// (ADR 0023).
    fn send_edit(&self, note: NoteId, envelope: Vec<u8>, soon: bool) {
        let holding = self
            .locked()
            .ok()
            .and_then(|core| core.holds_back(note).ok())
            .unwrap_or(false);
        if holding {
            return;
        }
        if soon {
            self.publish_soon(note, envelope);
        } else {
            self.publish(note, envelope);
        }
    }

    // -- attachments ----------------------------------------------------------

    /// This library's attachment store, opened on first use.
    ///
    /// Opening it tidies up after whatever happened while it was shut: a note
    /// that has gone — forgotten, deleted on another device, or taken away by
    /// its owner — has its bytes purged, and a note that is still here loses
    /// the bytes of attachments nobody draws any more. Best effort: a store
    /// that cannot be tidied is still a store.
    async fn media_store(&self) -> Result<Arc<Store>, String> {
        self.media
            .get_or_try_init(|| async {
                let store = self
                    .media_home
                    .open(self.media_key.clone())
                    .await
                    .map_err(|e| e.to_string())?;
                for note in store.held_notes().await.unwrap_or_default() {
                    let live = {
                        let Ok(mut core) = self.locked() else { break };
                        match core.attachment_state(note, AttachmentId::random()) {
                            Ok(AttachmentState::NoNote) => None,
                            Ok(_) => core
                                .view(note)
                                .ok()
                                .map(|v| v.attachments.into_iter().map(|a| a.media).collect()),
                            Err(_) => continue,
                        }
                    };
                    let _ = match live {
                        None => store.purge_note(note).await,
                        Some(live) => {
                            let live: Vec<Media> = live;
                            let refs: Vec<&Media> = live.iter().collect();
                            store.collect_garbage(note, &refs).await.map(|_| ())
                        }
                    };
                }
                Ok(Arc::new(store))
            })
            .await
            .cloned()
    }

    /// Run the attachment queue with `relay`, replacing one that runs with
    /// another — or with none, which is how a library that has never been
    /// connected runs it, publishing every import at once.
    fn start_transfers(
        self: &Arc<Self>,
        store: Arc<Store>,
        relay: Option<String>,
        access: Option<RelayAccess>,
    ) {
        let mut held = match crate::lock::lock(&self.statuses.transfers) {
            Ok(held) => held,
            Err(poisoned) => poisoned.into_inner(),
        };
        if held
            .as_ref()
            .is_some_and(|(running, key, _)| *running == relay && *key == access)
        {
            return;
        }
        let peer = MediaPeer {
            shared: Arc::downgrade(self),
            local: relay.is_none(),
        };
        let (runner, queue) = TransferRunner::new(peer, store, relay.clone());
        let runner = runner.with_access(access.clone());
        let mut states = queue.watch();
        let statuses = self.statuses.clone();
        // Ends with the runner, whose end drops the sender `states` reads.
        let _ = crate::relay::spawn(async move {
            loop {
                let views = transfer_views(&states.borrow_and_update());
                statuses.transfer_views.send_replace(views);
                if states.changed().await.is_err() {
                    return;
                }
            }
        });
        if crate::relay::spawn(runner.run()).is_ok() {
            // The one it replaces ends when its last handle is dropped, here;
            // what it was doing is in the staging records and starts again.
            *held = Some((relay, access, queue));
        }
    }

    /// The queue, started now if this library has never been connected — the
    /// one moment it would not have been. A library that has been waits for
    /// [`Library::connect`] to start it with its relay: an import made before
    /// then waits, rather than being published before its bytes can be sent.
    async fn transfers(self: &Arc<Self>) -> Result<(Arc<Store>, Option<TransferQueue>), String> {
        let store = self.media_store().await?;
        let unbound = crate::lock::lock(&self.bound)
            .map_err(|_| "relay lock poisoned")?
            .is_none();
        if unbound && self.statuses.queue().is_none() {
            self.start_transfers(store.clone(), None, None);
        }
        Ok((store, self.statuses.queue()))
    }

    /// Stop the queue. What closing and wiping do; staged imports stay staged.
    fn stop_transfers(&self) {
        if let Ok(mut held) = crate::lock::lock(&self.statuses.transfers) {
            held.take();
        }
    }

    /// Drop a note's attachment bytes from this device, in the background.
    fn purge_media(self: &Arc<Self>, note: NoteId) {
        let shared = self.clone();
        let _ = crate::relay::spawn(async move {
            if let Ok(store) = shared.media_store().await {
                let _ = store.purge_note(note).await;
            }
        });
    }
}

/// What the attachment queue asks of the library. Weak, because the library
/// holds the queue and the queue holds this.
struct MediaPeer {
    shared: Weak<Shared>,
    local: bool,
}

impl MediaPeer {
    fn shared(&self) -> easynotes_core::error::Result<Arc<Shared>> {
        self.shared
            .upgrade()
            .ok_or_else(|| CoreError::Storage(closed()))
    }
}

impl easynotes_core::crypto::DeviceSigner for MediaPeer {
    fn device_key(&self) -> easynotes_core::error::Result<easynotes_core::crypto::AuthorKey> {
        self.shared()?.core.device_key()
    }

    fn sign_challenge(
        &self,
        nonce: &[u8; easynotes_core::crypto::RELAY_AUTH_NONCE_LEN],
    ) -> easynotes_core::error::Result<[u8; easynotes_core::crypto::SIG_LEN]> {
        self.shared()?.core.sign_challenge(nonce)
    }
}

impl TransferPeer for MediaPeer {
    fn attachment_state(
        &self,
        note: NoteId,
        id: AttachmentId,
    ) -> easynotes_core::error::Result<AttachmentState> {
        self.shared()?.core.open()?.attachment_state(note, id)
    }

    fn attachment(
        &self,
        note: NoteId,
        id: AttachmentId,
    ) -> easynotes_core::error::Result<Option<Media>> {
        let shared = self.shared()?;
        let core = shared.core.open()?;
        Ok(core.view(note)?.attachment(id).map(|a| a.media.clone()))
    }

    fn reach(&self, note: NoteId) -> Reach {
        if self.local {
            return Reach::LocalOnly;
        }
        match self.shared().ok().and_then(|s| s.statuses.state(note)) {
            Some(SyncState::Synced) => Reach::Synced,
            Some(SyncState::Connecting | SyncState::Online) => Reach::Connected,
            _ => Reach::Offline,
        }
    }

    fn acknowledged(&self, note: NoteId) -> bool {
        self.local
            || self
                .shared()
                .ok()
                .is_some_and(|s| s.statuses.acknowledged(note))
    }

    fn publish(
        &self,
        note: NoteId,
        id: AttachmentId,
        media: Media,
    ) -> easynotes_core::error::Result<()> {
        let shared = self.shared()?;
        let envelope = shared.core.open()?.add_attachment(note, id, media)?;
        shared.send_edit(note, envelope, false);
        Ok(())
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
/// carry-over from the directory of files the initial implementation wrote, so that a `cargo test`
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

/// Attachment bytes live in a directory beside the database.
#[cfg(not(target_arch = "wasm32"))]
fn media_on_disk(root: &std::path::Path) -> Home {
    Home::Dir(root.join("media"))
}

#[cfg(target_arch = "wasm32")]
fn media_on_disk(_root: &std::path::Path) -> Home {
    Home::Memory
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
/// origin allows, which [ADR 0011](../../../../docs/decisions.md#adr-0011) is
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

fn attachment_id(s: &str) -> Result<AttachmentId, String> {
    AttachmentId::decode(s).map_err(|e| e.to_string())
}

fn item_id(s: &str) -> Result<ItemId, String> {
    ItemId::decode(s).map_err(|e| e.to_string())
}

fn note_id(s: &str) -> Result<NoteId, String> {
    NoteId::decode(s).map_err(|e| e.to_string())
}

/// Every ID of a selection, or the first that is not one. Parsed before any is
/// used, so a bad ID changes nothing rather than half a selection.
fn note_ids(ids: &[String]) -> Result<Vec<NoteId>, String> {
    ids.iter().map(|id| note_id(id)).collect()
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

/// How long an invite lasts. Seven days: long enough for a
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

/// Where an invite stands, as its owner is shown it.
pub struct InviteStatusView {
    /// False once somebody has opened the link, or it has expired.
    pub open: bool,
    pub expires_in_secs: u32,
    /// Somebody who opened the link, whose access is on its way: the package
    /// that lets them in is being published, and the answer goes once the
    /// relay has it.
    pub letting_in: Option<String>,
    /// Somebody who opened the link and is now in. Said once: the invite is
    /// forgotten on the way.
    pub joined: Option<String>,
}

/// A join request this device is waiting on.
pub struct PendingJoinView {
    pub invite: String,
    pub name: String,
    /// The note, when the link's pre-grant opened it and this device can read
    /// it already ([ADR 0030](../../../../docs/decisions.md#adr-0030)).
    /// The app opens it instead of showing the waiting dialog.
    pub note: Option<String>,
}

impl From<&PendingJoin> for PendingJoinView {
    fn from(join: &PendingJoin) -> Self {
        Self {
            invite: join.secret.id().encode(),
            name: join.name.clone(),
            note: None,
        }
    }
}

pub enum JoinStateView {
    /// The owner's device has not seen the request yet.
    Waiting,
    /// Let in: the note is on this device and syncing.
    Joined,
    /// Let in, and the note's key is still on its way.
    Opening,
    /// Turned away: the request could not be read.
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
/// Seen on the emulators; see item 5 of the later milestone.
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
        let mut open =
            crate::lock::lock(Self::registry()).unwrap_or_else(|poisoned| poisoned.into_inner());
        // Libraries Dart has already let go of: nothing to retire, and no
        // reason to keep an entry for every temp directory a test suite opens.
        open.retain(|_, held| held.shared.strong_count() > 0);
        if let Some(held) = open.remove(&dir) {
            if let (Some(shared), Some(stop)) = (held.shared.upgrade(), held.stop.upgrade()) {
                let orphan = Library {
                    shared,
                    stop,
                    pending: Arc::new(Pending::default()),
                    transfer_owner: Mutex::new(None),
                    transfer_guest: Mutex::new(None),
                    transfer_result: Mutex::new(None),
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
    transfer_owner: Mutex<Option<OwnerTransfer>>,
    transfer_guest: Mutex<Option<GuestTransfer>>,
    transfer_result: Mutex<Option<OpenedTransfer>>,
}

#[derive(Clone, Serialize, Deserialize)]
struct TransferOffer {
    version: u8,
    relay: String,
    id: String,
    guest: String,
    public_key: String,
}

struct OwnerTransfer {
    offer: TransferOffer,
    owner: String,
    keypair: TransferKeyPair,
    access: Option<String>,
    recipient: Option<[u8; 32]>,
}

#[derive(Clone)]
struct GuestTransfer {
    offer: TransferOffer,
    keypair: TransferKeyPair,
}

#[derive(Clone, Debug)]
pub struct AccountTransferSettings {
    pub relay_url: String,
    pub relay_access: Option<String>,
}

impl Library {
    /// Open the notes stored in `dir`, on a device that has a disk, unlocked by
    /// `master_key`.
    ///
    /// `dir` holds the SQLite database and the keyring; `master_key` is the 32
    /// bytes that seal everything in them which is not already an envelope, and
    /// it comes from the OS keychain by way of Dart — the core does not know or
    /// care where it was kept. the initial implementation's directory of files is carried into the
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
        let media_key = easynotes_core::crypto::MasterKey::from_slice(&master_key)
            .map_err(|e| e.to_string())?;
        let (store, keyring) = on_disk(&root, master_key)?;
        let library = Self::wrap_with(
            NoteCore::open(store, keyring).map_err(|e| e.to_string())?,
            Arc::new(Pending::default()),
            media_on_disk(&root),
            media_key,
        )?;
        claim.hold(&library);
        Ok(library)
    }

    /// Notes that live only in memory and are gone when the tab closes.
    ///
    /// This is what the web build uses today. A browser has no filesystem, and
    /// the op log's IndexedDB backing is item 4 of the second milestone — see
    /// `docs/decisions.md#adr-0002`. Everything above the store is the same code
    /// the native build runs, which is what makes this a stepping stone rather
    /// than a mock.
    #[frb(sync)]
    pub fn open_ephemeral() -> Result<Library, String> {
        Self::wrap_with(
            NoteCore::open(
                boxed_store(MemoryStore::new()),
                boxed_keyring(MemoryKeyring::new()),
            )
            .map_err(|e| e.to_string())?,
            Arc::new(Pending::default()),
            Home::Memory,
            easynotes_core::crypto::MasterKey::random(),
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
    /// `media_database` names the IndexedDB database attachment bytes are kept
    /// in, apart from the one `notes` and `keys` came from: that one is read
    /// whole at launch and a photo must never be. A native build, which opens
    /// this only in tests, keeps them in memory.
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
        media_database: String,
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
        let keyring = WebKeyring::from_rows(master.clone(), queue(StorageArea::Keys), rows(keys))
            .map_err(|e| e.to_string())?;
        #[cfg(target_arch = "wasm32")]
        let home = Home::Browser(media_database);
        #[cfg(not(target_arch = "wasm32"))]
        let home = {
            let _ = media_database;
            Home::Memory
        };
        Self::wrap_with(
            NoteCore::open(boxed_store(store), boxed_keyring(keyring))
                .map_err(|e| e.to_string())?,
            pending,
            home,
            master,
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

    fn wrap_with(
        mut core: Core,
        pending: Arc<Pending>,
        media_home: Home,
        media_key: easynotes_core::crypto::MasterKey,
    ) -> Result<Library, String> {
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
                access: Mutex::new(None),
                bound: Mutex::new(None),
                device: Mutex::new(None),
                connections: Mutex::new(HashMap::new()),
                statuses: Arc::new(Statuses::new()),
                media_home,
                media_key,
                media: tokio::sync::OnceCell::new(),
            }
        });
        Ok(Library {
            shared,
            stop: Arc::new(AtomicBool::new(false)),
            pending,
            transfer_owner: Mutex::new(None),
            transfer_guest: Mutex::new(None),
            transfer_result: Mutex::new(None),
        })
    }

    // -- the list -----------------------------------------------------------

    /// Every note, reading in any the core has not read yet.
    ///
    /// Not `#[frb(sync)]`, and neither is [`Library::note`]: both can tell the
    /// core's watchers something — the list is refreshed, a note opened — and
    /// telling a `watch` channel takes locks a watcher on a Web Worker may be
    /// holding. A synchronous call runs on the page's main thread, where waiting
    /// for one throws (ADR 0002), so these run on a worker like an edit does.
    /// What stays synchronous only reads: [`Library::listed`] is the list for a
    /// first frame, once this has been called.
    pub fn notes(&self) -> Result<Vec<NoteSummaryView>, String> {
        let mut core = self.shared.locked()?;
        Ok(core
            .notes()
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(Into::into)
            .collect())
    }

    /// The list as [`Library::notes`] last left it, and as every edit since has
    /// changed it. Loads nothing and tells nobody, so it is safe to call from
    /// the page's main thread.
    #[frb(sync)]
    pub fn listed(&self) -> Result<Vec<NoteSummaryView>, String> {
        Ok(self
            .shared
            .locked()?
            .listed()
            .into_iter()
            .map(Into::into)
            .collect())
    }

    /// The note list now, and every version of it after that.
    ///
    /// Every edit reaches it, whether it was made here or arrived from another
    /// device, because both go through the core's one refresh.
    pub fn watch_notes(&self, sink: StreamSink<Vec<NoteSummaryView>>) -> Result<(), String> {
        let mut updates = { self.shared.locked()?.watch_notes() };
        let stop = self.stop.clone();
        crate::relay::spawn(async move {
            let send = |summaries: Vec<easynotes_core::note::NoteSummary>| {
                sink.add(summaries.into_iter().map(Into::into).collect::<Vec<_>>())
            };
            if send(updates.borrow_and_update().clone()).is_err() {
                return;
            }
            // Before the first wait, not only after each wake: see
            // [`watch_status`].
            if stop.load(Ordering::SeqCst) {
                return;
            }
            loop {
                if updates.changed().await.is_err() {
                    return; // the library was dropped
                }
                if stop.load(Ordering::SeqCst) {
                    return;
                }
                let next = updates.borrow_and_update().clone();
                if send(next).is_err() {
                    return; // Dart stopped listening
                }
            }
        })
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

    /// A handle to one note, for a screen that is showing it.
    ///
    /// Cheap, and there may be several: they share the library's core, so two
    /// handles to one note are two views of the same document rather than two
    /// documents.
    pub fn note(&self, id: String) -> Result<Note, String> {
        let id = note_id(&id)?;
        // Opening is what makes the watch channel and the view real; a note the
        // library listed is already open, and this is what covers one that is
        // not.
        self.shared
            .locked()?
            .open_note(id)
            .map_err(|e| e.to_string())?;
        // Settling is what brings this person's name record up to date if a
        // rename happened on another phone while this note was shut, and it
        // sends the envelope that says so ([ADR 0032](../../../../docs/decisions.md#adr-0032)).
        self.shared.settle(id);
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
    // it — see `docs/decisions.md#adr-0008`. A colour is the one
    // exception: it is written into the note as well, because two people
    // looking at one list should see it the same shade.

    pub fn set_colour(&self, id: String, colour: Option<NoteColour>) -> Result<(), String> {
        let id = note_id(&id)?;
        let envelopes = self
            .shared
            .locked()?
            .set_colour(id, colour.map(Into::into))
            .map_err(|e| e.to_string())?;
        // A colour is one gesture, not typing: it goes at once, like a tick.
        for (note, envelope) in envelopes {
            self.shared.send_edit(note, envelope, false);
        }
        Ok(())
    }

    pub fn set_pinned(&self, id: String, pinned: bool) -> Result<(), String> {
        let id = note_id(&id)?;
        self.shared
            .locked()?
            .set_pinned(id, pinned)
            .map_err(|e| e.to_string())
    }

    // -- a selection of notes ------------------------------------------------
    //
    // What the grid's selection bar calls. Each is one change to the account
    // document however many notes are named, so a peer never sees half a
    // selection archived and the grid is redrawn once. A colour is the
    // exception: it is written into each note's own document as well.

    pub fn set_colour_many(
        &self,
        ids: Vec<String>,
        colour: Option<NoteColour>,
    ) -> Result<(), String> {
        let ids = note_ids(&ids)?;
        let envelopes = self
            .shared
            .locked()?
            .set_colour_many(&ids, colour.map(Into::into))
            .map_err(|e| e.to_string())?;
        for (note, envelope) in envelopes {
            self.shared.send_edit(note, envelope, false);
        }
        Ok(())
    }

    pub fn set_pinned_many(&self, ids: Vec<String>, pinned: bool) -> Result<(), String> {
        let ids = note_ids(&ids)?;
        self.shared
            .locked()?
            .set_pinned_many(&ids, pinned)
            .map_err(|e| e.to_string())
    }

    pub fn set_archived_many(&self, ids: Vec<String>, archived: bool) -> Result<(), String> {
        let ids = note_ids(&ids)?;
        self.shared
            .locked()?
            .set_archived_many(&ids, archived)
            .map_err(|e| e.to_string())
    }

    pub fn set_trashed_many(&self, ids: Vec<String>, trashed: bool) -> Result<(), String> {
        let ids = note_ids(&ids)?;
        self.shared
            .locked()?
            .set_trashed_many(&ids, trashed)
            .map_err(|e| e.to_string())
    }

    /// Put a label on every note named, and return the label it went on
    /// under. `null` for a name that is not a name.
    pub fn add_tag_many(&self, ids: Vec<String>, tag: String) -> Result<Option<String>, String> {
        let ids = note_ids(&ids)?;
        Ok(self
            .shared
            .locked()?
            .add_tag_many(&ids, &tag)
            .map_err(|e| e.to_string())?
            .map(|label| label.encode()))
    }

    pub fn remove_tag_many(&self, ids: Vec<String>, label: String) -> Result<(), String> {
        let ids = note_ids(&ids)?;
        let label = label_id(&label)?;
        self.shared
            .locked()?
            .remove_tag_many(&ids, label)
            .map_err(|e| e.to_string())
    }

    /// Permanently delete every note named. Returns how many. Each goes the
    /// way [`Library::forget_note`] does — socket first, then rows — so this is
    /// a loop here rather than one call into the core.
    pub fn forget_notes(&self, ids: Vec<String>) -> Result<u32, String> {
        let mut ids = note_ids(&ids)?;
        let mut seen = std::collections::HashSet::new();
        ids.retain(|id| seen.insert(*id));
        for id in &ids {
            self.forget(*id)?;
        }
        Ok(ids.len() as u32)
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
    /// `docs/decisions.md#adr-0010`.
    pub fn forget_note(&self, id: String) -> Result<(), String> {
        let id = note_id(&id)?;
        self.forget(id)
    }

    /// Everybody with access to a note, owner first. What the share screen
    /// lists; it shows no devices.
    #[frb(sync)]
    pub fn people(&self, id: String) -> Result<PeopleView, String> {
        let id = note_id(&id)?;
        let core = self.shared.locked()?;
        let people = core.people(id).map_err(|e| e.to_string())?;
        Ok(PeopleView {
            shared: people.shared,
            can_manage: people.can_manage,
            people: people
                .people
                .into_iter()
                .map(|p| PersonView {
                    member: p.member,
                    invite: p.invite,
                    name: p.name,
                    owner: p.owner,
                    me: p.me,
                    removing: p.removing,
                })
                .collect(),
            adding: people.adding,
        })
    }

    /// Remove one person. Saved at once, and made as soon as this device is
    /// caught up with the note: offline, the row says “Removing…” until then.
    /// Everyone else keeps the note and does nothing.
    pub fn remove_person(&self, id: String, member: String, invite: String) -> Result<(), String> {
        let id = note_id(&id)?;
        self.shared
            .locked()?
            .remove_person(id, &member, &invite)
            .map_err(|e| e.to_string())?;
        self.shared.settle(id);
        Ok(())
    }

    /// Remove everyone but the owner, and cancel every open invite to the
    /// note. The invites need the relay; a withdrawal that cannot reach it
    /// leaves an invite that expires on its own and can no longer let anyone
    /// in, because this device will not answer it.
    pub async fn stop_sharing(&self, id: String) -> Result<(), String> {
        let note = note_id(&id)?;
        let invites = {
            let mut core = self.shared.locked()?;
            core.stop_sharing(note).map_err(|e| e.to_string())?;
            core.invites()
                .map_err(|e| e.to_string())?
                .into_iter()
                .filter(|i| i.note == note)
                .collect::<Vec<_>>()
        };
        self.shared.settle(note);
        let http = self.shared.http_base().ok();
        for outgoing in invites {
            if let Some((http, access)) = http.clone() {
                let (secret, owner) = (outgoing.secret.clone(), outgoing.owner.clone());
                let _ = on_network(async move {
                    invite::withdraw(&http, access.as_ref(), &secret, &owner)
                        .await
                        .map_err(|e| e.to_string())
                })
                .await;
            }
            self.shared.forget_invite(&outgoing)?;
        }
        Ok(())
    }

    /// Delete this person's account from the relay and everything from this
    /// device ([ADR 0026](../../../../docs/decisions.md#adr-0026)).
    ///
    /// Withdraws this device's open invites, tells every list this person
    /// joined that they have left
    /// ([ADR 0034](../../../../docs/decisions.md#adr-0034)),
    /// purges from the relay every note this person owns and then the account
    /// document, and wipes this profile's notes, keys and device identity.
    /// Lists this person joined are left to their owners; the leave records
    /// let those owners remove the membership and rotate the note key on their
    /// next sync. A profile with no relay has nothing to purge and nothing to
    /// tell.
    ///
    /// If the relay cannot be reached, or refuses, nothing local has changed
    /// and the error says so; it is safe to try again. On success the library
    /// is empty and must be closed and opened again, which gives a new device.
    pub async fn delete_account(&self) -> Result<(), String> {
        let http = crate::lock::lock(&self.shared.bound)
            .map_err(|_| "relay lock poisoned")?
            .clone();
        let access = crate::lock::lock(&self.shared.access)
            .map_err(|_| "relay lock poisoned")?
            .clone();
        let (plan, invites, leaves) = {
            let mut core = self.shared.locked()?;
            (
                core.purge_plan().map_err(|e| e.to_string())?,
                core.invites().map_err(|e| e.to_string())?,
                core.leave_joined_notes().map_err(|e| e.to_string())?,
            )
        };
        // The leave records go out while the socket that carries them is still
        // up. The owner's device acts on them on its next sync, which may be
        // long after this one is gone; a relay that does not take them means
        // nothing is deleted here, and the person can try again.
        if !leaves.is_empty() {
            for (note, envelope) in &leaves {
                self.shared.send_edit(*note, envelope.clone(), false);
            }
            let notes: Vec<NoteId> = leaves.iter().map(|(note, _)| *note).collect();
            self.flush_notes(&notes).await?;
        }
        // Every socket first. A relay that purges a topic tells whoever is
        // subscribed, and a `gone` recorded after the wipe would write a row
        // for a note this device no longer has.
        self.disconnect()?;
        let Some(http) = http else {
            self.shared.locked()?.wipe().map_err(|e| e.to_string())?;
            return self.wipe_media().await;
        };
        for outgoing in invites {
            let (http, access) = (http.clone(), access.clone());
            let _ = on_network(async move {
                invite::withdraw(&http, access.as_ref(), &outgoing.secret, &outgoing.owner)
                    .await
                    .map_err(|e| e.to_string())
            })
            .await;
        }
        let purged = on_network(async move {
            easynotes_core::sync::purge::purge_all(&http, &plan)
                .await
                .map_err(|e| e.to_string())
        })
        .await;
        if let Err(e) = purged {
            return Err(format!(
                "The relay could not delete this account ({e}). Nothing was removed \
                 from this device; try again when you are online."
            ));
        }
        self.shared.locked()?.wipe().map_err(|e| e.to_string())?;
        self.wipe_media().await
    }

    /// Every attachment byte on this device, staged or not, after the notes
    /// that named them.
    async fn wipe_media(&self) -> Result<(), String> {
        self.shared.stop_transfers();
        self.shared
            .media_store()
            .await?
            .wipe()
            .await
            .map_err(|e| e.to_string())
    }

    /// The notes this device was just removed from, each once, for the
    /// message “You no longer have access to ‘Groceries’”.
    #[frb(sync)]
    pub fn take_removals(&self) -> Result<Vec<RemovalView>, String> {
        Ok(self
            .shared
            .locked()?
            .take_removals()
            .into_iter()
            .map(|(note, title)| RemovalView {
                note: note.encode(),
                title,
            })
            .collect())
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
            .map_err(|e| e.to_string())?;
        // After the note, so that an upload finishing in between finds the
        // note gone and discards itself rather than being published.
        self.shared.purge_media(id);
        Ok(())
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

    /// Whether a checklist shows its ticked items, as this person last left it.
    #[frb(sync)]
    pub fn ticked_shown(&self) -> Result<bool, String> {
        Ok(self.shared.locked()?.ticked_shown())
    }

    pub fn set_ticked_shown(&self, shown: bool) -> Result<(), String> {
        self.shared
            .locked()?
            .set_ticked_shown(shown)
            .map_err(|e| e.to_string())
    }

    // -- the person ---------------------------------------------------------
    //
    // Twelve words, and what they unlock. There is no server-side account here:
    // no password, no email address, no login. A person *is* a recovery phrase,
    // and a device belongs to them if it can derive the same account root from
    // it — see [ADR 0013](../../../../docs/decisions.md#adr-0013).

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
    /// recover from, and the design calls for a backup check for
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

    /// The name this person shares under, once chosen. Asked once, as part of
    /// setting up sharing, and the same on every device of the account.
    #[frb(sync)]
    pub fn profile_name(&self) -> Result<Option<String>, String> {
        Ok(self.shared.locked()?.profile_name())
    }

    pub fn set_profile_name(&self, name: String) -> Result<(), String> {
        let written = {
            let mut core = self.shared.locked()?;
            core.set_profile_name(&name).map_err(|e| e.to_string())?
        };
        // Every share a rename touched, handed to the relay the way any other
        // edit is. A note with no connection keeps its envelope in the log
        // until one exists.
        for (note, envelope) in written {
            self.shared.send_edit(note, envelope, false);
        }
        Ok(())
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

    /// Start a five-minute pairing. The QR contains no account or relay secret.
    pub async fn begin_account_transfer(
        &self,
        relay_url: String,
        relay_access: Option<String>,
    ) -> Result<String, String> {
        if !self.has_account()? {
            return Err("Create or restore an account before adding another device.".into());
        }
        let (http, access) = self.shared.http_base()?;
        let parsed =
            easynotes_core::sync::RelayEndpoint::parse(&relay_url).map_err(|e| e.to_string())?;
        if parsed.http_base != http {
            return Err("This device is connected to a different relay.".into());
        }
        let id = InviteId::random().encode();
        let mut guest = [0u8; 32];
        let mut owner = [0u8; 32];
        easynotes_core::crypto::random_bytes(&mut guest);
        easynotes_core::crypto::random_bytes(&mut owner);
        let keypair = TransferKeyPair::generate();
        let offer = TransferOffer {
            version: 1,
            relay: relay_url,
            id: id.clone(),
            guest: URL_SAFE_NO_PAD.encode(guest),
            public_key: URL_SAFE_NO_PAD.encode(keypair.public_key()),
        };
        let open = easynotes_core::sync::pairing::open(http, access, id.clone(), guest, owner, 300);
        on_network(async move { open.await.map(|_| ()).map_err(|e| e.to_string()) }).await?;
        let qr = serde_json::to_string(&offer).map_err(|e| e.to_string())?;
        *self
            .transfer_owner
            .lock()
            .map_err(|_| "transfer state unavailable")? = Some(OwnerTransfer {
            offer,
            owner: URL_SAFE_NO_PAD.encode(owner),
            keypair,
            access: relay_access,
            recipient: None,
        });
        guest.fill(0);
        owner.fill(0);
        Ok(qr)
    }

    /// Whether a new device has scanned the QR, and the code to compare on
    /// both screens before approving it.
    pub async fn account_transfer_request_code(&self) -> Result<Option<String>, String> {
        let (http, _) = self.shared.http_base()?;
        let (id, owner) = {
            let state = self
                .transfer_owner
                .lock()
                .map_err(|_| "transfer state unavailable")?;
            let state = state.as_ref().ok_or("No device transfer is active.")?;
            (state.offer.id.clone(), state.owner.clone())
        };
        let request_id = id.clone();
        let result = easynotes_core::sync::pairing::poll_request(http, id, owner);
        let (public, _) =
            on_network(async move { result.await.map_err(|e| e.to_string()) }).await?;
        let Some(public) = public else {
            return Ok(None);
        };
        let mut state = self
            .transfer_owner
            .lock()
            .map_err(|_| "transfer state unavailable")?;
        let state = state.as_mut().ok_or("No device transfer is active.")?;
        let code = state
            .keypair
            .verification_code(&public, request_id.as_bytes())
            .map_err(|e| e.to_string())?;
        state.recipient = Some(public);
        Ok(Some(code))
    }

    /// Approve the displayed pairing code and send the encrypted account and
    /// relay settings to that one-time recipient key.
    pub async fn approve_account_transfer(&self) -> Result<(), String> {
        let (http, id, owner, public, keypair, relay, access) = {
            let state = self
                .transfer_owner
                .lock()
                .map_err(|_| "transfer state unavailable")?;
            let state = state.as_ref().ok_or("No device transfer is active.")?;
            (
                self.shared.http_base()?.0,
                state.offer.id.clone(),
                state.owner.clone(),
                state
                    .recipient
                    .ok_or("No new device has scanned the code.")?,
                state.keypair.clone(),
                state.offer.relay.clone(),
                state.access.clone(),
            )
        };
        let root = self
            .shared
            .locked()?
            .account_root_for_transfer()
            .map_err(|e| e.to_string())?;
        let encrypted = keypair
            .seal(&public, id.as_bytes(), &root, &relay, access.as_deref())
            .map_err(|e| e.to_string())?;
        let result = easynotes_core::sync::pairing::send_response(http, id, owner, encrypted);
        on_network(async move { result.await.map_err(|e| e.to_string()) }).await
    }

    pub async fn cancel_account_transfer(&self) -> Result<(), String> {
        let state = self
            .transfer_owner
            .lock()
            .map_err(|_| "transfer state unavailable")?
            .take();
        if let Some(state) = state {
            let http = self.shared.http_base()?.0;
            let id = state.offer.id;
            let owner = state.owner;
            let result = easynotes_core::sync::pairing::close(http, id, owner);
            on_network(async move { result.await.map_err(|e| e.to_string()) }).await?;
        }
        *self
            .transfer_guest
            .lock()
            .map_err(|_| "transfer state unavailable")? = None;
        *self
            .transfer_result
            .lock()
            .map_err(|_| "transfer state unavailable")? = None;
        Ok(())
    }

    /// Scan-side setup. The return code is displayed while the original device
    /// waits for approval.
    pub async fn begin_account_transfer_from_qr(&self, qr: String) -> Result<String, String> {
        let offer: TransferOffer =
            serde_json::from_str(&qr).map_err(|_| "This is not an EasyNotes transfer code.")?;
        if offer.version != 1 {
            return Err("Update EasyNotes on both devices to transfer this account.".into());
        }
        let endpoint =
            easynotes_core::sync::RelayEndpoint::parse(&offer.relay).map_err(|e| e.to_string())?;
        let public: [u8; 32] = URL_SAFE_NO_PAD
            .decode(&offer.public_key)
            .map_err(|_| "The transfer code is invalid.")?
            .try_into()
            .map_err(|_| "The transfer code is invalid.")?;
        let token = URL_SAFE_NO_PAD
            .decode(&offer.guest)
            .map_err(|_| "The transfer code is invalid.")?;
        if token.len() != 32 {
            return Err("The transfer code is invalid.".into());
        }
        let keypair = TransferKeyPair::generate();
        let code = keypair
            .verification_code(&public, offer.id.as_bytes())
            .map_err(|e| e.to_string())?;
        let key = URL_SAFE_NO_PAD.encode(keypair.public_key());
        let request = easynotes_core::sync::pairing::request(
            endpoint.http_base,
            offer.id.clone(),
            offer.guest.clone(),
            key,
        );
        on_network(async move { request.await.map_err(|e| e.to_string()) }).await?;
        *self
            .transfer_guest
            .lock()
            .map_err(|_| "transfer state unavailable")? = Some(GuestTransfer { offer, keypair });
        Ok(code)
    }

    /// Poll once for the approved encrypted transfer. The secret remains in
    /// Rust until `adopt_account_transfer` commits it into the device keyring.
    pub async fn poll_account_transfer_result(
        &self,
    ) -> Result<Option<AccountTransferSettings>, String> {
        if let Some(opened) = self
            .transfer_result
            .lock()
            .map_err(|_| "transfer state unavailable")?
            .as_ref()
        {
            return Ok(Some(AccountTransferSettings {
                relay_url: opened.relay_url.clone(),
                relay_access: opened.relay_access.clone(),
            }));
        }
        let state = self
            .transfer_guest
            .lock()
            .map_err(|_| "transfer state unavailable")?
            .clone()
            .ok_or("No device transfer is active.")?;
        let endpoint = easynotes_core::sync::RelayEndpoint::parse(&state.offer.relay)
            .map_err(|e| e.to_string())?;
        let id = state.offer.id.clone();
        let token = state.offer.guest.clone();
        let transfer_id = id.clone();
        let result = easynotes_core::sync::pairing::poll_response(endpoint.http_base, id, token);
        let (encrypted, _) =
            on_network(async move { result.await.map_err(|e| e.to_string()) }).await?;
        let Some(encrypted) = encrypted else {
            return Ok(None);
        };
        let peer: [u8; 32] = URL_SAFE_NO_PAD
            .decode(&state.offer.public_key)
            .map_err(|_| "Invalid transfer key.")?
            .try_into()
            .map_err(|_| "Invalid transfer key.")?;
        let opened = state
            .keypair
            .open(&peer, transfer_id.as_bytes(), &encrypted)
            .map_err(|e| e.to_string())?;
        let settings = AccountTransferSettings {
            relay_url: opened.relay_url.clone(),
            relay_access: opened.relay_access.clone(),
        };
        *self
            .transfer_result
            .lock()
            .map_err(|_| "transfer state unavailable")? = Some(opened);
        Ok(Some(settings))
    }

    pub fn adopt_account_transfer(&self) -> Result<(), String> {
        let transfer = self
            .transfer_result
            .lock()
            .map_err(|_| "transfer state unavailable")?
            .take()
            .ok_or("No approved account transfer is ready.")?;
        self.shared
            .locked()?
            .adopt_account_root(transfer.root)
            .map_err(|e| e.to_string())?;
        self.shared.connect_new_documents()?;
        *self
            .transfer_guest
            .lock()
            .map_err(|_| "transfer state unavailable")? = None;
        Ok(())
    }

    // -- invites ------------------------------------------------------------
    //
    // How a note reaches another person: a link that carries no key, which the
    // first person to open it claims, and a key the owner's device hands over
    // to that person — and nobody after them — without asking. The core
    // keeps what must survive a restart and does the cryptography; the calls
    // to the relay are `easynotes_core::sync::invite`; what is here is the
    // order they go in and the lock never being held across one of them.
    //
    // See `docs/decisions.md#adr-0015` and `docs/decisions.md#adr-0025`.

    /// Open an invite to a note and return the link to send.
    ///
    /// Needs a relay: the invite lives on it until somebody asks to join.
    pub async fn create_invite(&self, note: String) -> Result<InviteView, String> {
        let note = note_id(&note)?;
        let (http, access) = self.shared.http_base()?;
        let mut invite = {
            let core = self.shared.locked()?;
            core.new_invite(note).map_err(|e| e.to_string())?
        };
        let (secret, owner) = (invite.secret.clone(), invite.owner.clone());
        let granted = on_network(async move {
            invite::open(&http, access.as_ref(), &secret, &owner, INVITE_TTL_SECS)
                .await
                .map_err(|e| e.to_string())
        })
        .await?;
        invite.expires_at = now() + granted as i64;
        self.shared
            .locked()?
            .remember_invite(&invite)
            .map_err(|e| e.to_string())?;
        // The key this link hands over, sealed for whoever claims it, so the
        // guest can read the note without waiting for this device.
        self.refresh_pre_grant(&invite).await;
        Ok(InviteView::from(&invite))
    }

    /// The invites this device has open to `note`, oldest first. Local: it asks
    /// the relay nothing, so an invite that has quietly expired is still here
    /// until [`Library::check_invite`] finds out.
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

    /// Where an invite stands — and, if somebody has opened the link, let
    /// them in. Nobody is asked: the link works once, for the first account
    /// that opens it (ADR 0025). The invite's screen polls this, and so does
    /// the note list, which is how a person gets in while the share dialog is
    /// closed.
    ///
    /// Letting someone in is ADR 0023's approval: save it, publish the
    /// package that adds them, and answer the invite once the relay has the
    /// package. A step that cannot finish yet — offline, or the package still
    /// on its way — finishes on a later call, or after a relaunch.
    ///
    /// An invite the relay no longer has, or that has closed with nothing of
    /// this device's left to finish, is forgotten here on the way.
    pub async fn check_invite(&self, invite: String) -> Result<InviteStatusView, String> {
        let mut outgoing = self.shared.outgoing(&invite)?;
        let mut expires_in = (outgoing.expires_at - now()).max(0) as u64;
        if outgoing.approval.is_none() {
            let (http, access) = self.shared.http_base()?;
            let (secret, owner) = (outgoing.secret.clone(), outgoing.owner.clone());
            // The relay saying no is an answer and a network failing is not,
            // and only the first means the invite is gone.
            let listed = on_network(async move {
                Ok(invite::list(&http, access.as_ref(), &secret, &owner)
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
            expires_in = listing.expires_in;
            let claimed = listing
                .requests
                .into_iter()
                .find(|r| r.state == RequestState::Pending)
                .filter(|_| listing.open);
            let Some(claimed) = claimed else {
                if !listing.open {
                    self.shared.forget_invite(&outgoing)?;
                }
                return Ok(InviteStatusView {
                    open: listing.open,
                    expires_in_secs: clamp_secs(expires_in),
                    letting_in: None,
                    joined: None,
                });
            };
            // Only a request that can never be let in is turned away: one
            // that cannot be read, or one the note has no room for. Anything
            // else — a disk that failed, say — is tried again on the next call
            // rather than spending the link.
            let refused = match &claimed.join {
                Some(join) => match self.shared.locked()?.approve(
                    outgoing.secret.id(),
                    &claimed.request,
                    join,
                ) {
                    Ok(_) => None,
                    Err(CoreError::Invite(message)) => Some(message),
                    Err(other) => return Err(other.to_string()),
                },
                None => Some(
                    "Someone opened this link without EasyNotes, so nobody was let in. \
                     Send a new link."
                        .to_string(),
                ),
            };
            if let Some(message) = refused {
                // The link is spent either way: it takes one request. Say so
                // to whoever made it, and stop keeping the invite.
                self.turn_away(&outgoing, claimed.request).await?;
                return Err(message);
            }
            self.shared.settle(outgoing.note);
            outgoing = self.shared.outgoing(&invite)?;
        }
        let name = outgoing
            .approval
            .as_ref()
            .map(|(_, join)| join.name.clone());
        // The key the guest was handed should be the one the package adds
        // them with: if a removal rotated it in between, seal it again now.
        self.refresh_pre_grant(&outgoing).await;
        let done = self.finish_approval(&outgoing).await?;
        Ok(InviteStatusView {
            open: false,
            expires_in_secs: clamp_secs(expires_in),
            letting_in: if done { None } else { name.clone() },
            joined: if done { name } else { None },
        })
    }

    /// Seal the note's current key for a link, and leave it on the relay
    /// ([ADR 0030](../../../../docs/decisions.md#adr-0030)).
    ///
    /// Best effort on purpose: an invite whose pre-grant cannot be sealed or
    /// sent still works, with the guest waiting for this device as before. It
    /// is called when the link is made, and again just before an approval, so
    /// the grant the guest was handed is the key the package will add them
    /// with.
    async fn refresh_pre_grant(&self, outgoing: &OutgoingInvite) {
        let sealed = match self.shared.locked().and_then(|core| {
            core.pre_grant(outgoing.secret.id())
                .map_err(|e| e.to_string())
        }) {
            Ok(Some(sealed)) => sealed,
            _ => return,
        };
        let Ok((http, access)) = self.shared.http_base() else {
            return;
        };
        let (secret, owner) = (outgoing.secret.clone(), outgoing.owner.clone());
        let _ = on_network(async move {
            invite::set_pre_grant(&http, access.as_ref(), &secret, &owner, &sealed)
                .await
                .map_err(|e| e.to_string())
        })
        .await;
    }

    /// Answer a request with nothing, and forget its invite.
    ///
    /// When the invite carried a pre-grant, the note's key is changed as well:
    /// whoever held the link may have read the note already, and a rotation is
    /// the only thing that stops them reading it any further (ADR 0030). It is
    /// deliberately after the decline: the link is spent either way, and the
    /// rotation must not be lost because the relay call failed.
    async fn turn_away(&self, outgoing: &OutgoingInvite, request: String) -> Result<(), String> {
        let (http, access) = self.shared.http_base()?;
        let (secret, owner) = (outgoing.secret.clone(), outgoing.owner.clone());
        // Both of the checks below are best effort: the link is spent either
        // way, and a device that cannot read its own invite row must not turn a
        // decline into a failure the guest is told about.
        let pre_granted = self
            .shared
            .locked()
            .ok()
            .and_then(|core| core.pre_grant(outgoing.secret.id()).ok())
            .flatten()
            .is_some();
        on_network(async move {
            invite::decline(&http, access.as_ref(), &secret, &owner, &request)
                .await
                .map_err(|e| e.to_string())
        })
        .await?;
        if pre_granted {
            let note = outgoing.note;
            let rotated = self
                .shared
                .locked()
                .ok()
                .and_then(|mut core| core.rotate_key(note).ok())
                .flatten();
            if let Some(rotated) = rotated {
                for envelope in rotated.envelopes {
                    self.shared.send_edit(note, envelope, false);
                }
            }
        }
        self.shared.forget_invite(outgoing)
    }

    /// Answer a request that has been let in, once the package letting them
    /// in is on the relay, and forget the invite. `false` while the package
    /// is still on its way.
    async fn finish_approval(&self, outgoing: &OutgoingInvite) -> Result<bool, String> {
        let Some((request, join)) = outgoing.approval.clone() else {
            return Ok(false);
        };
        let answer = {
            let mut core = self.shared.locked()?;
            // Approving again is how an interrupted approval resumes: it puts
            // back whatever of it a crash lost.
            core.approve(outgoing.secret.id(), &request, &join)
                .map_err(|e| e.to_string())?;
            core.approval_answer(outgoing.secret.id())
                .map_err(|e| e.to_string())?
        };
        let Some(answer) = answer else {
            self.shared.settle(outgoing.note);
            return Ok(false);
        };
        let (http, access) = self.shared.http_base()?;
        let (secret, owner) = (outgoing.secret.clone(), outgoing.owner.clone());
        let answered = on_network(async move {
            Ok(
                invite::approve(&http, access.as_ref(), &secret, &owner, &request, &answer)
                    .await
                    .map_err(|e| (matches!(e, CoreError::Invite(_)), e.to_string())),
            )
        })
        .await?;
        match answered {
            // Already answered is as good as answered: a crash after the relay
            // took it and before this device forgot the row.
            Ok(()) | Err((true, _)) => {
                self.shared.forget_invite(outgoing)?;
                Ok(true)
            }
            Err((false, message)) => Err(message),
        }
    }

    /// Take an invite down, on the relay and here. Anyone holding the link is
    /// told it does not exist.
    pub async fn withdraw_invite(&self, invite: String) -> Result<(), String> {
        let outgoing = self.shared.outgoing(&invite)?;
        let (http, access) = self.shared.http_base()?;
        let (secret, owner) = (outgoing.secret.clone(), outgoing.owner.clone());
        let withdrawn = on_network(async move {
            Ok(invite::withdraw(&http, access.as_ref(), &secret, &owner)
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

    /// Claim a link, under the name this account shares with. The owner's
    /// device lets this person in the next time it is online; nobody there is
    /// asked. Asking again — a retry, or another device of this account — is
    /// the same request.
    pub async fn request_join(&self, link: String) -> Result<PendingJoinView, String> {
        let secret = InviteSecret::from_link(&link).map_err(|e| e.to_string())?;
        let (http, access) = self.shared.http_base()?;
        let (sealed_request, member, name) = {
            let core = self.shared.locked()?;
            let sealed = core.seal_join_request(&secret).map_err(|e| e.to_string())?;
            let member = core.member().map_err(|e| e.to_string())?.id();
            (sealed, member, core.profile_name().unwrap_or_default())
        };
        let (s, payload) = (secret.clone(), sealed_request);
        let (request, pre_grant) = on_network(async move {
            invite::request(&http, access.as_ref(), &s, &member, &payload)
                .await
                .map_err(|e| e.to_string())
        })
        .await?;
        let join = PendingJoin {
            secret,
            request,
            name,
            note: None,
        };
        self.shared
            .locked()?
            .remember_join(&join)
            .map_err(|e| e.to_string())?;
        let id = join.secret.id();
        let note = match pre_grant {
            Some(sealed) => self.take_pre_grant(id, &join.secret, &sealed)?,
            None => None,
        };
        if let Some(note) = note {
            // The row stays: the request is still answered later, and a
            // decline has to be able to take the note away again.
            let mut opened = join.clone();
            opened.note = Some(note);
            self.shared
                .locked()?
                .remember_join(&opened)
                .map_err(|e| e.to_string())?;
        }
        let mut view = PendingJoinView::from(&join);
        view.note = note.map(|id| id.encode());
        Ok(view)
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

    /// Ask the relay how a join request went, and act on the answer: one let
    /// in records the note in this person's account and follows it until its
    /// key arrives; a declined, expired or withdrawn one stops being waited
    /// on.
    pub async fn check_join(&self, invite: String) -> Result<JoinCheckView, String> {
        let (join, member) = {
            let core = self.shared.locked()?;
            let join = core
                .joins()
                .map_err(|e| e.to_string())?
                .into_iter()
                .find(|j| j.secret.id().encode() == invite)
                .ok_or("This device is not waiting on that invite.")?;
            (join, core.member().map_err(|e| e.to_string())?)
        };
        let (http, access) = self.shared.http_base()?;
        let (secret, request) = (join.secret.clone(), join.request.clone());
        let answer = on_network(async move {
            match invite::poll(&http, access.as_ref(), &secret, &member, &request).await {
                Ok(answer) => Ok(Ok(answer)),
                Err(CoreError::Invite(message)) => Ok(Err(message)),
                Err(other) => Err(other.to_string()),
            }
        })
        .await?;
        let id = join.secret.id();
        match answer {
            Ok(Answer::Pending { pre_grant, .. }) => {
                // The link's key, if this device has not read the note yet:
                // enough to open it before the owner's device has seen the
                // request. The row stays, because a decline has to reach this
                // device later.
                if let Some(sealed) = pre_grant {
                    if let Some(note) = self.take_pre_grant(id, &join.secret, &sealed)? {
                        let mut opened = join.clone();
                        opened.note = Some(note);
                        self.shared
                            .locked()?
                            .remember_join(&opened)
                            .map_err(|e| e.to_string())?;
                        return Ok(JoinCheckView {
                            state: JoinStateView::Joined,
                            note: Some(note.encode()),
                            message: None,
                        });
                    }
                }
                Ok(JoinCheckView::waiting())
            }
            Ok(Answer::Approved { note, owner }) => {
                let readable = {
                    let mut core = self.shared.locked()?;
                    core.accept_answer(id, note, owner)
                        .map_err(|e| e.to_string())?;
                    core.can_open(note)
                };
                self.shared.connect_new_documents()?;
                if !readable {
                    return Ok(JoinCheckView {
                        state: JoinStateView::Opening,
                        note: Some(note.encode()),
                        message: None,
                    });
                }
                self.shared
                    .locked()?
                    .forget_join(id)
                    .map_err(|e| e.to_string())?;
                Ok(JoinCheckView {
                    state: JoinStateView::Joined,
                    note: Some(note.encode()),
                    message: None,
                })
            }
            Ok(Answer::Declined) => {
                // A note read through this invite before it was answered goes
                // too: the person was turned away, and a key to a note nobody
                // will re-grant is not one to keep.
                self.shared
                    .locked()?
                    .forget_pre_grant(id)
                    .map_err(|e| e.to_string())?;
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

    /// Open a link's pre-grant, if this device can use it
    /// ([ADR 0030](../../../../docs/decisions.md#adr-0030)).
    ///
    /// `Some(note)` when the note is readable now; `None` when this device
    /// already holds a key for it (an older one is never replaced by a link)
    /// or the grant is for a note that has been deleted. A grant that does not
    /// open is not an error: the invite still works the way it did before.
    fn take_pre_grant(
        &self,
        invite: InviteId,
        secret: &InviteSecret,
        sealed: &[u8],
    ) -> Result<Option<NoteId>, String> {
        let Ok((note, owner, key)) = easynotes_core::invite::open_pre_grant(secret, sealed) else {
            return Ok(None);
        };
        let readable = {
            let mut core = self.shared.locked()?;
            if let Err(_error) = core.accept_pre_grant(invite, note, owner, key) {
                // Not a note this device may take: deleted, or a record it
                // must not overwrite. The old path still works.
                return Ok(None);
            }
            core.can_open(note)
        };
        if readable {
            self.shared.connect_new_documents()?;
        }
        Ok(readable.then_some(note))
    }

    /// Stop waiting on a join request. The relay keeps the request until the
    /// invite expires; the owner's device may still let this person in, and
    /// the note then arrives through the account like any other.
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
    /// [`Library::watch_sync`] to find out how it goes.
    ///
    /// The first relay a library is connected to is the only one it will ever
    /// connect to. Calling this again with another spelling of that relay
    /// reconnects; naming a different relay is refused, and the connection it
    /// already has is left as it was. Moving to another relay is opening
    /// another library, which is what the app's relay profiles do.
    ///
    /// `access` is the relay's access key, base64url, for a relay whose
    /// operator lets in only the people they gave it to ([ADR 0028](../../../../docs/decisions.md#adr-0028)); `None`
    /// for an open relay. Connecting again with another key is how a changed
    /// key takes effect.
    pub async fn connect(&self, relay: String, access: Option<String>) -> Result<(), String> {
        let relay = easynotes_core::sync::RelayEndpoint::parse(&relay)
            .map_err(|e| e.to_string())?
            .http_base;
        let access = match access.as_deref().map(str::trim) {
            None | Some("") => None,
            Some(raw) => Some(
                RelayAccess::decode(raw)
                    .map_err(|_| "That is not a relay access key.".to_string())?,
            ),
        };
        {
            let mut bound =
                crate::lock::lock(&self.shared.bound).map_err(|_| "relay lock poisoned")?;
            match bound.as_deref() {
                Some(own) if own != relay => {
                    return Err(format!(
                        "These notes belong to the relay at {own}, not {relay}. \
                         Each relay keeps its own notes: switch relay in Settings."
                    ))
                }
                Some(_) => {}
                None => *bound = Some(relay.clone()),
            }
        }
        // A new engine every time, as each note used to get a new connection.
        self.disconnect()?;
        // A local runner must finish touching its records before we restore
        // them for upload. Otherwise it could settle one after it was restored.
        if let Some(queue) = self.shared.statuses.queue() {
            queue.stop().await;
        }
        self.shared.stop_transfers();
        let store = self.shared.media_store().await?;
        // A note with no relay cursor has never uploaded its local media.
        // Restage every live descriptor before starting its note connection:
        // after a crash, either the zero cursor or these records retries it.
        let local_media = {
            let core = self.shared.locked()?;
            let mut media = Vec::new();
            for note in core.syncable().map_err(|e| e.to_string())? {
                if core.sync_cursor(note).map_err(|e| e.to_string())?.last_seq == 0 {
                    if let Ok(view) = core.view(note) {
                        media.extend(view.attachments.into_iter().map(|a| Staged {
                            note,
                            id: a.id,
                            media: a.media,
                        }));
                    }
                }
            }
            media
        };
        for staged in local_media {
            store.restage(&staged).await.map_err(|e| e.to_string())?;
        }
        let device = Device::open(self.shared.core.clone(), &relay, access.clone())?;
        *crate::lock::lock(&self.shared.device).map_err(|_| "sync lock poisoned".to_string())? =
            Some(device);
        *crate::lock::lock(&self.shared.relay).map_err(|_| "relay lock poisoned".to_string())? =
            Some(relay);
        *crate::lock::lock(&self.shared.access).map_err(|_| "relay lock poisoned".to_string())? =
            access.clone();
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
        // The attachment queue, with this relay. Started once the store is
        // open, which in a tab has to be waited for; an import made before
        // then is in the store and the queue finds it when it starts.
        let relay = crate::lock::lock(&self.shared.bound)
            .map_err(|_| "relay lock poisoned")?
            .clone();
        self.shared.start_transfers(store, relay, access);
        Ok(())
    }

    /// Check that a relay can be reached and will accept this device, without
    /// binding this library to it or needing a note to sync.
    ///
    /// The engine opens no socket until it has a note to sync, so a device
    /// with no notes — first-run setup — has no connection status to watch.
    /// This performs the handshake a connection would: prove the device key
    /// and the access key to the relay, then open and close the sync socket.
    /// It leaves the library, its bound relay and its selection untouched, so
    /// a relay that fails the check can be replaced without a profile switch.
    pub async fn verify_relay(&self, relay: String, access: Option<String>) -> Result<(), String> {
        let access = match access.as_deref().map(str::trim) {
            None | Some("") => None,
            Some(raw) => Some(
                RelayAccess::decode(raw)
                    .map_err(|_| "That is not a relay access key.".to_string())?,
            ),
        };
        let endpoint = easynotes_core::sync::RelayEndpoint::parse(&relay)
            .map_err(|e| e.to_string())?
            .with_access(access);
        // The device key is this profile's, which the relay accepts like any
        // other: what the check is for is the address and the access key.
        let peer = self.shared.core.clone();
        on_network(async move {
            let token = easynotes_core::sync::authenticate(
                &endpoint.http_base,
                endpoint.access.as_ref(),
                &peer,
            )
            .await
            .map_err(|e| e.to_string())?;
            let sync = easynotes_core::sync::DeviceSync::connect(&endpoint.ws_base, &token)
                .await
                .map_err(|e| e.to_string())?;
            sync.close().await.map_err(|e| e.to_string())
        })
        .await
    }

    /// Send every edit that is waiting for the typing to pause, now.
    ///
    /// For when the app may not get another chance: going into the background,
    /// or a tab being hidden. Nothing waiting is at risk either way — it is in
    /// the log, and the next connection sends it — but a person who types a
    /// line and pockets the phone expects the other phone to have it.
    #[frb(sync)]
    pub fn send_held_edits(&self) -> Result<(), String> {
        let connections = crate::lock::lock(&self.shared.connections)
            .map_err(|_| "sync lock poisoned".to_string())?;
        for connection in connections.values() {
            connection.flush();
        }
        Ok(())
    }

    /// Send what is waiting for `notes`, wait for the relay to deliver it back,
    /// and wait until none of it is unacknowledged.
    ///
    /// For the one caller that is about to destroy the log holding those
    /// envelopes: deleting an account, whose leave records must reach the
    /// owners of the lists this person joined
    /// ([ADR 0034](../../../../docs/decisions.md#adr-0034)).
    /// A refresh barrier goes first, because a status that was caught up
    /// before the publish would otherwise answer "done" too soon.
    async fn flush_notes(&self, notes: &[NoteId]) -> Result<(), String> {
        if notes.is_empty() {
            return Ok(());
        }
        // Refuse now rather than wait out the timeout: a deletion that must
        // leave joined lists needs a socket, and a device that is not
        // connected should hear that in words it can act on.
        let connected = crate::lock::lock(&self.shared.connections)
            .map_err(|_| "sync lock poisoned".to_string())?
            .keys()
            .copied()
            .collect::<std::collections::HashSet<_>>();
        if !notes.iter().all(|note| connected.contains(note)) {
            return Err(
                "This device is not connected to the sync server. Reconnect, then delete \
                 the account again."
                    .to_string(),
            );
        }
        self.send_held_edits()?;
        static FLUSH_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let refresh_id = FLUSH_ID.fetch_add(1, Ordering::Relaxed).to_string();
        let refresh = crate::lock::lock(&self.shared.device)
            .map_err(|_| "sync lock poisoned".to_string())?
            .as_ref()
            .map(|device| device.refresh(refresh_id));
        let Some(refresh) = refresh else {
            return Err(
                "This device is not connected to the sync server. Reconnect, then delete \
                 the account again."
                    .to_string(),
            );
        };
        refresh.await?;
        let mut status = self.shared.statuses.overall.subscribe();
        let wait = async {
            loop {
                if notes
                    .iter()
                    .all(|note| self.shared.statuses.acknowledged(*note))
                {
                    return Ok(());
                }
                status
                    .changed()
                    .await
                    .map_err(|_| "sync status closed".to_string())?;
            }
        };
        tokio::time::timeout(std::time::Duration::from_secs(30), wait)
            .await
            .map_err(|_| {
                "The sync server did not acknowledge the last changes within 30 seconds."
                    .to_string()
            })?
    }

    /// Lifecycle entry point for the bridge. Hidden callers flush local work
    /// and held edits before the Dart coordinator starts its bounded grace.
    pub async fn set_foreground(&self, foreground: bool) -> Result<(), String> {
        if !foreground {
            self.send_held_edits()?;
        }
        if let Some(device) = crate::lock::lock(&self.shared.device)
            .map_err(|_| "sync lock poisoned".to_string())?
            .as_ref()
        {
            device.set_foreground(foreground);
        }
        Ok(())
    }

    /// Flush held typing and wait up to 30 seconds for the library to report a
    /// caught-up state and the local acknowledgements captured at start.
    pub async fn sync_now(&self) -> Result<(), String> {
        let captured = self.shared.statuses.overall.get();
        let acknowledgement_target = captured
            .acknowledged
            .saturating_add(captured.pending as u64);
        self.send_held_edits()?;
        static REFRESH_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let refresh_id = REFRESH_ID.fetch_add(1, Ordering::Relaxed).to_string();
        let refresh = crate::lock::lock(&self.shared.device)
            .map_err(|_| "sync lock poisoned".to_string())?
            .as_ref()
            .map(|device| device.refresh(refresh_id));
        let mut status = self.shared.statuses.overall.subscribe();
        let wait = async {
            if let Some(refresh) = refresh {
                refresh.await?;
            }
            loop {
                let current = status.borrow().clone();
                if current.acknowledged >= acknowledgement_target
                    && matches!(current.state, SyncState::Synced | SyncState::Empty)
                {
                    return Ok(());
                }
                status
                    .changed()
                    .await
                    .map_err(|_| "sync status closed".to_string())?;
            }
        };
        tokio::time::timeout(std::time::Duration::from_secs(30), wait)
            .await
            .map_err(|_| "Sync did not finish within 30 seconds.".to_string())?
    }

    /// Stop syncing everything. Edits still work and still queue; nothing is
    /// lost.
    pub fn disconnect(&self) -> Result<(), String> {
        *crate::lock::lock(&self.shared.relay).map_err(|_| "relay lock poisoned".to_string())? =
            None;
        let connections: Vec<Connection> = {
            let mut open = crate::lock::lock(&self.shared.connections)
                .map_err(|_| "sync lock poisoned".to_string())?;
            open.drain().map(|(_, connection)| connection).collect()
        };
        for connection in connections {
            connection.shutdown();
        }
        let device = crate::lock::lock(&self.shared.device)
            .map_err(|_| "sync lock poisoned".to_string())?
            .take();
        if let Some(device) = device {
            device.shutdown();
        }
        self.shared.stop_transfers();
        self.shared.statuses.all_offline();
        Ok(())
    }

    /// Where this device stands with the relay, over all its notes.
    #[frb(sync)]
    pub fn sync_status(&self) -> SyncStatus {
        self.shared.statuses.overall.get()
    }

    pub fn watch_sync(&self, sink: StreamSink<SyncStatus>) -> Result<(), String> {
        spawn_watch_status(
            self.shared.statuses.overall.subscribe(),
            self.stop.clone(),
            sink,
        )
    }

    /// End the watchers started above and let their tasks finish.
    ///
    /// Necessary rather than tidy: a watcher is parked on a channel, so without
    /// a wake it waits forever, the task leaks, and anything waiting for the
    /// stream to close — such as Dart cancelling its subscription — waits
    /// forever too.
    pub fn stop_watching(&self) -> Result<(), String> {
        self.stop.store(true, Ordering::SeqCst);
        self.shared.statuses.overall.resend();
        self.shared.statuses.transfer_views.send_modify(|_| {});
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
        self.shared.stop_transfers();
        self.shared.core.close()
    }

    // -- attachment transfers -------------------------------------------------

    /// Every attachment transfer that has not finished.
    ///
    /// Not `#[frb(sync)]`: reading the channel takes a lock the queue's
    /// forwarder may hold, which the page's main thread may not wait on.
    pub fn transfers(&self) -> Vec<TransferView> {
        self.shared.statuses.transfer_views.borrow().clone()
    }

    /// The transfers now and at every change, until [`Library::stop_watching`].
    pub fn watch_transfers(&self, sink: StreamSink<Vec<TransferView>>) -> Result<(), String> {
        let mut updates = self.shared.statuses.transfer_views.subscribe();
        let stop = self.stop.clone();
        crate::relay::spawn(async move {
            if sink.add(updates.borrow_and_update().clone()).is_err() {
                return;
            }
            // Before the first wait as well: see [`watch_status`].
            if stop.load(Ordering::SeqCst) {
                return;
            }
            loop {
                if updates.changed().await.is_err() || stop.load(Ordering::SeqCst) {
                    return;
                }
                if sink.add(updates.borrow_and_update().clone()).is_err() {
                    return;
                }
            }
        })
    }

    /// The thumbnail a note's card shows, decrypted, or `None` while it is not
    /// on this device — it is then asked for, and [`Library::watch_transfers`]
    /// says when it arrives. Read from the index, so the note is not opened
    /// unless a download has to be.
    pub async fn read_cover(&self, note: String) -> Result<Option<Vec<u8>>, String> {
        let note = note_id(&note)?;
        let cover = {
            // `listed`, which only reads: this runs on a page's main thread
            // in a tab, where telling the list's watchers anything may not.
            self.shared
                .locked()?
                .listed()
                .into_iter()
                .find(|s| s.id == note)
                .and_then(|s| s.cover)
        };
        let Some((id, media)) = cover else {
            return Ok(None);
        };
        let Some(thumbnail) = media.thumbnail.as_ref().map(|t| &t.media) else {
            return Ok(None);
        };
        let (store, queue) = self.shared.transfers().await?;
        if store.has(note, thumbnail).await {
            return store
                .read(note, id, &media, Variant::Thumbnail)
                .await
                .map(Some)
                .map_err(|e| e.to_string());
        }
        if let Some(queue) = queue {
            queue.fetch(note, id, Variant::Thumbnail);
        }
        Ok(None)
    }

    /// Try every failed attachment transfer again now.
    pub fn retry_transfers(&self) {
        if let Some(queue) = self.shared.statuses.queue() {
            queue.retry();
        }
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
    status: Arc<StatusCell>,
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
        core.view_at(self.id)
            .map(Into::into)
            .map_err(|e| e.to_string())
    }

    /// Every view from now on, starting with the current one.
    ///
    /// This is the only way the UI learns anything. A local edit and an edit
    /// arriving from another device both come out here, which is what keeps the
    /// one-device and two-device cases identical.
    pub fn watch(&self, sink: StreamSink<NoteView>) -> Result<(), String> {
        let mut rx = {
            self.shared
                .locked()?
                .watch(self.id)
                .map_err(|e| e.to_string())?
        };
        let stop = self.stop.clone();

        // Do not make this method async. flutter_rust_bridge would hold the
        // auto-opaque read guard for the entire stream, and dropping Tokio's
        // guard can wait on its semaphore mutex. On the web that drop must stay
        // off the page's main thread. Capture the receiver and stop flag here,
        // then let the worker's event loop drive the watcher independently.
        //
        // `changed()` is runtime-agnostic, so nothing polls and an idle watcher
        // costs nothing.
        crate::relay::spawn(async move {
            if sink.add(rx.borrow_and_update().clone().into()).is_err() {
                return;
            }
            // Before the first wait, not only after each wake: see
            // [`watch_status`].
            if stop.load(Ordering::SeqCst) {
                return;
            }
            loop {
                if rx.changed().await.is_err() {
                    return; // the library was dropped
                }
                if stop.load(Ordering::SeqCst) {
                    return;
                }
                let view = rx.borrow_and_update().clone();
                if sink.add(view.into()).is_err() {
                    return; // Dart stopped listening
                }
            }
        })
    }

    /// Where this note stands with the relay.
    #[frb(sync)]
    pub fn sync_status(&self) -> SyncStatus {
        self.status.get()
    }

    pub fn watch_sync(&self, sink: StreamSink<SyncStatus>) -> Result<(), String> {
        spawn_watch_status(self.status.subscribe(), self.stop.clone(), sink)
    }

    /// End this handle's watchers and let their tasks finish. The note stays
    /// open in the library, because another screen may be showing it.
    pub fn stop_watching(&self) -> Result<(), String> {
        self.stop.store(true, Ordering::SeqCst);
        self.status.resend();
        // Closed: the note's channel went with the core, and its watcher with
        // that — as for [`Library::stop_watching`].
        // A note that is gone — deleted, or this person was removed from it —
        // took its channel with it, which ended the watcher already.
        match self.shared.locked() {
            Ok(core) => match core.touch(self.id) {
                Err(CoreError::UnknownNote(_)) => Ok(()),
                other => other.map_err(|e| e.to_string()),
            },
            Err(_) => Ok(()),
        }
    }

    // -- edits --------------------------------------------------------------

    /// Hand an edit to the relay — unless a removal is waiting to be made, when
    /// it stays in the log: it is under the key the person being removed holds,
    /// and it reaches everyone else inside the removal's package instead
    /// (ADR 0023).
    fn send(&self, envelope: Vec<u8>, soon: bool) {
        self.shared.send_edit(self.id, envelope, soon);
    }
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
        self.send(envelope, true);
        Ok(())
    }

    /// Say that this person changed the body from `from` to `to`. The core
    /// replays that change onto the body as it now stands, so a peer's edit
    /// that arrived after the screen last drew is kept rather than typed over.
    ///
    /// Returns the revision the edit landed at, or the current one when it
    /// changed nothing — the field that sent it records that and adopts only
    /// views newer than it.
    pub fn edit_body(&self, from: String, to: String) -> Result<u32, String> {
        let (envelope, rev) = {
            let mut core = self.shared.locked()?;
            let envelope = core
                .edit_body(self.id, &from, &to)
                .map_err(|e| e.to_string())?;
            (envelope, core.rev(self.id).map_err(|e| e.to_string())?)
        };
        if let Some(envelope) = envelope {
            self.send(envelope, true);
        }
        Ok(rev)
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
            self.send(envelope, false);
        }
        Ok(())
    }

    pub fn add_item(&self, text: String) -> Result<String, String> {
        let (id, envelope) = {
            let mut core = self.shared.locked()?;
            core.add_item(self.id, text).map_err(|e| e.to_string())?
        };
        self.send(envelope, false);
        Ok(id.encode())
    }

    /// Returns the revision the edit landed at: the field that sent it records
    /// that and adopts only views newer than it, so its own echo cannot wind
    /// the line back.
    pub fn set_item_text(&self, item: String, text: String) -> Result<u32, String> {
        let (envelope, rev) = {
            let mut core = self.shared.locked()?;
            let envelope = core
                .set_item_text(self.id, item_id(&item)?, text)
                .map_err(|e| e.to_string())?;
            (envelope, core.rev(self.id).map_err(|e| e.to_string())?)
        };
        self.send(envelope, true);
        Ok(rev)
    }

    /// Set an item's text from the field's own edit: the text it was showing
    /// and the text it shows now. Replayed onto what the item says by the time
    /// it arrives, so typing in the same item as somebody else keeps both
    /// people's words.
    ///
    /// Returns the revision the edit landed at, like
    /// [`set_item_text`](Self::set_item_text).
    pub fn edit_item_text(&self, item: String, from: String, to: String) -> Result<u32, String> {
        let (envelope, rev) = {
            let mut core = self.shared.locked()?;
            let envelope = core
                .edit_item_text(self.id, item_id(&item)?, &from, &to)
                .map_err(|e| e.to_string())?;
            (envelope, core.rev(self.id).map_err(|e| e.to_string())?)
        };
        self.send(envelope, true);
        Ok(rev)
    }

    pub fn set_checked(&self, item: String, checked: bool) -> Result<(), String> {
        let envelope = {
            let mut core = self.shared.locked()?;
            core.set_checked(self.id, item_id(&item)?, checked)
                .map_err(|e| e.to_string())?
        };
        self.send(envelope, false);
        Ok(())
    }

    pub fn move_item(&self, item: String, to_index: u32) -> Result<(), String> {
        let envelope = {
            let mut core = self.shared.locked()?;
            core.move_item(self.id, item_id(&item)?, to_index as usize)
                .map_err(|e| e.to_string())?
        };
        self.send(envelope, false);
        Ok(())
    }

    pub fn remove_item(&self, item: String) -> Result<(), String> {
        let envelope = {
            let mut core = self.shared.locked()?;
            core.remove_item(self.id, item_id(&item)?)
                .map_err(|e| e.to_string())?
        };
        self.send(envelope, false);
        Ok(())
    }

    // -- attachments --------------------------------------------------------
    //
    // An import is sealed into this device's store and staged; it is not in
    // the note until the transfer queue publishes it — at once in a note that
    // does not sync, and once its bytes are on the relay in one that does
    // (ADR 0027). Until then it is one of [`Note::pending_attachments`],
    // and nobody else can see it.

    /// Attach a file handed over whole: a tab's picked file, or a photo in
    /// memory. `thumbnail` is a preview the app made, of at most 512 pixels a
    /// side; `duration_ms` is a sound file's length, if the platform knows it.
    pub async fn import_attachment(
        &self,
        name: String,
        bytes: Vec<u8>,
        thumbnail: Option<Vec<u8>>,
        duration_ms: Option<u64>,
    ) -> Result<AttachmentView, String> {
        let (store, queue) = self.shared.transfers().await?;
        self.check_room(&store, bytes.len() as u64).await?;
        let staged = store
            .import(
                self.id,
                &name,
                &bytes,
                thumbnail.map(|bytes| ThumbnailInput { bytes }),
                ImportHints { duration_ms },
            )
            .await
            .map_err(|e| e.to_string())?;
        if let Some(queue) = queue {
            queue.kick();
        }
        Ok(AttachmentView::of(
            staged.id,
            &staged.media,
            String::new(),
            true,
        ))
    }

    /// Attach a file on this device's disk, read a chunk at a time rather than
    /// handed across whole. A browser has no path to give, and is refused.
    pub async fn import_attachment_file(
        &self,
        path: String,
        name: String,
        thumbnail: Option<Vec<u8>>,
        duration_ms: Option<u64>,
    ) -> Result<AttachmentView, String> {
        #[cfg(target_arch = "wasm32")]
        {
            let _ = (path, name, thumbnail, duration_ms);
            Err("a browser attaches files by their bytes, not a path".to_string())
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let (store, queue) = self.shared.transfers().await?;
            let len = std::fs::metadata(&path).map_err(|e| e.to_string())?.len();
            self.check_room(&store, len).await?;
            let staged = store
                .import_file(
                    self.id,
                    &name,
                    std::path::Path::new(&path),
                    thumbnail.map(|bytes| ThumbnailInput { bytes }),
                    ImportHints { duration_ms },
                )
                .await
                .map_err(|e| e.to_string())?;
            if let Some(queue) = queue {
                queue.kick();
            }
            Ok(AttachmentView::of(
                staged.id,
                &staged.media,
                String::new(),
                true,
            ))
        }
    }

    /// Refuse an import the note has no room for before sealing a byte of it,
    /// counting this device's own unpublished imports as the note's.
    async fn check_room(&self, store: &Store, adding: u64) -> Result<(), String> {
        use easynotes_core::attachment::{MAX_ATTACHMENTS_PER_NOTE, MAX_MEDIA_BYTES_PER_NOTE};
        let live: Vec<u64> = {
            let core = self.shared.locked()?;
            core.view(self.id)
                .map_err(|e| e.to_string())?
                .attachments
                .iter()
                .map(|a| a.media.original.len)
                .collect()
        };
        let pending: Vec<u64> = self
            .staged(store)
            .await?
            .iter()
            .map(|s| s.media.original.len)
            .collect();
        if live.len() + pending.len() >= MAX_ATTACHMENTS_PER_NOTE {
            return Err("a note can have up to 20 attachments".into());
        }
        let total: u64 = live.iter().chain(&pending).sum();
        if total + adding > MAX_MEDIA_BYTES_PER_NOTE {
            return Err("a note's attachments can be up to 200 MB between them".into());
        }
        Ok(())
    }

    async fn staged(&self, store: &Store) -> Result<Vec<Staged>, String> {
        Ok(store
            .staged()
            .await
            .map_err(|e| e.to_string())?
            .into_iter()
            .filter(|s| s.note == self.id)
            .collect())
    }

    /// This device's imports into this note that are not in it yet, oldest
    /// first. The queue's [`Library::transfers`] says where each one is.
    pub async fn pending_attachments(&self) -> Result<Vec<AttachmentView>, String> {
        let store = self.shared.media_store().await?;
        let staged = self.staged(&store).await?;
        // A record outlives its publication until the relay has the
        // descriptor (ADR 0027); by then the note draws it, and it is not
        // pending any more.
        let mut core = self.shared.locked()?;
        let mut pending = Vec::new();
        for s in &staged {
            if matches!(
                core.attachment_state(self.id, s.id),
                Ok(AttachmentState::Pending)
            ) {
                pending.push(AttachmentView::of(s.id, &s.media, String::new(), true));
            }
        }
        Ok(pending)
    }

    /// An attachment's bytes, decrypted: its thumbnail, or the whole file.
    ///
    /// `None` when they are not on this device yet. They are then asked for,
    /// and [`Library::transfers`] follows them; ask again when they arrive.
    /// A pending import is always here.
    pub async fn read_attachment(
        &self,
        attachment: String,
        thumbnail: bool,
    ) -> Result<Option<Vec<u8>>, String> {
        let id = attachment_id(&attachment)?;
        let variant = if thumbnail {
            Variant::Thumbnail
        } else {
            Variant::Original
        };
        let (store, queue) = self.shared.transfers().await?;
        let live = {
            let core = self.shared.locked()?;
            core.view(self.id)
                .map_err(|e| e.to_string())?
                .attachment(id)
                .map(|a| a.media.clone())
        };
        let media = match &live {
            Some(media) => media.clone(),
            None => self
                .staged(&store)
                .await?
                .into_iter()
                .find(|s| s.id == id)
                .map(|s| s.media)
                .ok_or("This attachment is not on the note.")?,
        };
        let part = easynotes_core::attachment::storage::variant_ref(&media, variant)
            .ok_or("This attachment has no thumbnail.")?;
        if store.has(self.id, part).await {
            return store
                .read(self.id, id, &media, variant)
                .await
                .map(Some)
                .map_err(|e| e.to_string());
        }
        if let (Some(_), Some(queue)) = (live, queue) {
            queue.fetch(self.id, id, variant);
        }
        Ok(None)
    }

    /// Remove an attachment for everyone on the note, or give up on an import
    /// of this device's that is not in it yet. The bytes on this device go
    /// too; nobody else's, and not the relay's (ADR 0027).
    pub async fn remove_attachment(&self, attachment: String) -> Result<(), String> {
        let id = attachment_id(&attachment)?;
        let removed = {
            let mut core = self.shared.locked()?;
            let live = core
                .view(self.id)
                .map_err(|e| e.to_string())?
                .attachment(id)
                .map(|a| a.media.clone());
            match live {
                Some(media) => Some((
                    core.remove_attachment(self.id, id)
                        .map_err(|e| e.to_string())?,
                    media,
                )),
                None => None,
            }
        };
        if let Some((envelope, media)) = removed {
            self.send(envelope, false);
            // A base after the removal is what lets the relay free its bytes,
            // and what takes its key out of the relay's log (ADR 0027).
            self.shared.with_connection(self.id, Connection::reclaim);
            let store = self.shared.media_store().await?;
            return store
                .remove(self.id, &media)
                .await
                .map_err(|e| e.to_string());
        }
        let (store, queue) = self.shared.transfers().await?;
        match queue {
            // The queue, and not the store directly, so that an upload it is
            // running is not published when it finishes.
            Some(queue) => queue.cancel(id),
            None => {
                if let Some(staged) = self.staged(&store).await?.into_iter().find(|s| s.id == id) {
                    store.discard(&staged).await.map_err(|e| e.to_string())?;
                }
            }
        }
        Ok(())
    }

    /// Move an attachment so it lands at `to_index` among the note's.
    pub fn move_attachment(&self, attachment: String, to_index: u32) -> Result<(), String> {
        let envelope = {
            let mut core = self.shared.locked()?;
            core.move_attachment(self.id, attachment_id(&attachment)?, to_index as usize)
                .map_err(|e| e.to_string())?
        };
        self.send(envelope, false);
        Ok(())
    }

    /// Set the shared words that describe an attachment to someone who cannot
    /// see or hear it.
    pub fn describe_attachment(
        &self,
        attachment: String,
        description: String,
    ) -> Result<(), String> {
        let envelope = {
            let mut core = self.shared.locked()?;
            core.set_attachment_description(self.id, attachment_id(&attachment)?, description)
                .map_err(|e| e.to_string())?
        };
        self.send(envelope, true);
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
    // Checked once before the first wait, and not only after each wake. Dart
    // starts a watcher and then stops it, but the bridge runs the two calls on
    // different threads, so `stop_watching` can finish before this receiver
    // was subscribed. Its one wake then happened before the subscription, a
    // receiver counts it as already seen, and `changed()` would wait for good
    // — and so would Dart, cancelling a stream that never closes. After this
    // point a stop cannot be missed: its wake comes after the value just
    // taken, so `changed()` returns.
    if stop.load(Ordering::SeqCst) {
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

/// Start a status stream without keeping the generated auto-opaque lock guard
/// alive for the stream's lifetime.
fn spawn_watch_status(
    updates: watch::Receiver<SyncStatus>,
    stop: Arc<AtomicBool>,
    sink: StreamSink<SyncStatus>,
) -> Result<(), String> {
    let error_sink = sink.clone();
    crate::relay::spawn(async move {
        if let Err(error) = watch_status(updates, stop, sink).await {
            let _ = error_sink.add_error(error);
        }
    })
}
