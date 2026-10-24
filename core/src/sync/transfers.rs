//! The attachment transfer queue: what moves a photo from "saved on this
//! device" to "on the note for everyone", and fetches other people's
//! ([ADR 0027](../../../docs/adr/0027-attachments.md) §3–§4).
//!
//! [`super::media`] knows how to move one variant; this decides which, when,
//! and what to do afterwards. It is a loop of its own beside the sync engine,
//! not a part of it, because the two have nothing to wait for from each other
//! but one answer — is this note caught up? — and a 50 MB upload has no
//! business holding up a socket that carries typing.
//!
//! * **Uploads are durable because staging records are.** The queue keeps no
//!   list of its own that could drift: every launch, and every [`kick`], it
//!   reads [`MediaStore::staged`] and reconciles each record against
//!   [`TransferPeer::attachment_state`], as §3's table says. An upload
//!   interrupted anywhere is picked up from the record, and the relay's status
//!   call says which chunks it already has.
//! * **A descriptor waits for its bytes and for the note.** An import into a
//!   synced note is uploaded only once the engine says the note is
//!   [`Reach::Synced`] — the account caught up first, so a deleted note is known
//!   to be deleted, and the note's access packages settled (ADR 0023 §7) — and
//!   published only once the relay has committed every object it names. A note
//!   that does not sync publishes at once. Publication goes through
//!   `add_attachment`, which refuses a spent ID, after `attachment_state` has
//!   been asked again, so a completion that arrives after a removal, a
//!   cancellation or the note's deletion cannot bring the attachment back.
//! * **Downloads are asked for, not kept.** The app asks for a thumbnail when a
//!   card is drawn and an original when it is opened; what it asked for is
//!   derived again from what is on screen after a restart, so a queue of them
//!   on disk would only be one more thing to reconcile. A download whose
//!   attachment has been removed is dropped before it starts.
//! * **Bounded, and in order.** At most [`MAX_RUNNING`] transfers at once:
//!   thumbnails first, because a grid is waiting for them, then uploads, then
//!   originals. A failure that the network explains is retried with backoff,
//!   from [`FIRST_BACKOFF`] to [`MAX_BACKOFF`]. One that the relay explains —
//!   no room, no attachments on this relay, bytes that are not the digest — is
//!   held as failed until the person says [`retry`](TransferQueue::retry).
//! * **Its own token.** The relay's tokens last fifteen minutes and a socket
//!   can stay up for hours, so borrowing the engine's would mean borrowing a
//!   stale one. The queue authenticates as the same device, keeps the token
//!   for ten minutes, and asks for a new one after any failure.
//!
//! Written over [`MediaStorage`], so it runs the same on a device, over its
//! directory, and in a tab, over IndexedDB. What differs between the two is the
//! future it hands the executor — `Send` on a device, not in a tab — and the
//! clock ([`super::timer::now`]), and neither is visible from here.
//!
//! [`kick`]: TransferQueue::kick

use super::authenticate;
use super::media::{download, upload};
use super::timer::now;
use crate::attachment::storage::{variant_ref, Fut, IncomingMedia, MediaStorage, Staged};
use crate::attachment::{AttachmentState, Media, MediaRef, Variant};
use crate::crypto::DeviceSigner;
use crate::error::{CoreError, Result};
use crate::ids::{AttachmentId, NoteId};
use crate::relay_access::RelayAccess;
use futures_util::stream::{FuturesUnordered, StreamExt};
use rand::RngExt;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, watch};

/// Transfers in flight at once, uploads and downloads together.
pub const MAX_RUNNING: usize = 3;

/// The wait after a transfer's first failure. It doubles with each failure
/// after that, up to [`MAX_BACKOFF`].
pub const FIRST_BACKOFF: Duration = Duration::from_secs(1);

/// The longest a failed transfer waits to be tried again. Longer than the
/// engine's eight seconds: a transfer that fails is usually a large one on a
/// poor network, and the note's text does not wait on it.
pub const MAX_BACKOFF: Duration = Duration::from_secs(60);

/// How often a transfer waiting for its note to catch up asks again, in case
/// the app's [`kick`](TransferQueue::kick) on a status change was missed.
const RECHECK: Duration = Duration::from_secs(5);

/// How often an upload whose descriptor the relay has not acknowledged yet
/// asks the relay again about its bytes. Half the day for which the relay
/// leaves a committed or asked-about object alone, so that a device away for
/// longer finds a doom cancelled, or the bytes gone and sends them again,
/// before anyone else has to (ADR 0027 §5).
const REVOUCH: Duration = Duration::from_secs(12 * 60 * 60);

/// How long a token is used for. The relay's last fifteen minutes; this leaves
/// room for a long upload that started with one.
const TOKEN_LIFE: Duration = Duration::from_secs(10 * 60);

/// How far a note has got with the relay, as the sync engine sees it now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reach {
    /// The note does not sync: nothing to upload to or fetch from, and a
    /// descriptor goes into it at once.
    LocalOnly,
    /// It syncs, and has no connection now.
    Offline,
    /// Connected, and still catching up or settling access. Enough to fetch,
    /// not to publish.
    Connected,
    /// Caught up, with access settled: uploads and publication may go ahead.
    Synced,
}

/// What the queue needs from the rest of the device.
///
/// A trait for the reason [`super::SyncPeer`] is one: the real answers are
/// behind the app's lock on the core and its table of engine statuses, and the
/// queue should not know either.
pub trait TransferPeer: DeviceSigner + Send + Sync + 'static {
    /// See [`crate::note::NoteCore::attachment_state`].
    fn attachment_state(&self, note: NoteId, id: AttachmentId) -> Result<AttachmentState>;

    /// A live attachment's descriptor, or `None` if the note does not draw it.
    fn attachment(&self, note: NoteId, id: AttachmentId) -> Result<Option<Media>>;

    fn reach(&self, note: NoteId) -> Reach;

    /// Whether the relay has every edit this device made to the note: synced,
    /// and nothing of this device's waiting to be acknowledged. What an
    /// upload's record waits for after publishing, since until then its
    /// descriptor may not be on the relay, and nothing there names its bytes
    /// (ADR 0027 §5).
    fn acknowledged(&self, note: NoteId) -> bool;

    /// Put the descriptor into the note — `NoteCore::add_attachment` — and hand
    /// the envelope to the engine, as any edit is handed.
    fn publish(&self, note: NoteId, id: AttachmentId, media: Media) -> Result<()>;
}

/// Which way a transfer goes. An upload is an attachment's original and
/// thumbnail together, since neither is any use to anyone without the other's
/// descriptor; a download is one variant, since a grid wants only thumbnails.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Direction {
    Up,
    Down(Variant),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Transfer {
    pub note: NoteId,
    pub id: AttachmentId,
    pub direction: Direction,
}

impl Transfer {
    /// Thumbnails, then uploads, then originals.
    fn priority(&self) -> u8 {
        match self.direction {
            Direction::Down(Variant::Thumbnail) => 0,
            Direction::Up => 1,
            Direction::Down(Variant::Original) => 2,
        }
    }
}

/// Where one transfer is, for the screen to draw on the item.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TransferState {
    /// Saved here and waiting for the note to be caught up with the relay —
    /// "Waiting to upload" — or, for a download, for a connection.
    Waiting,
    /// `done` of `of` chunks sent or received in this attempt.
    Running { done: u32, of: u32 },
    /// The last attempt failed for a reason the network explains, and another
    /// will follow on its own.
    Retrying { error: String },
    /// The relay or the note refused it, and nothing more happens until
    /// [`TransferQueue::retry`]. For an upload, the local copy is kept.
    Failed { error: String },
}

/// Every transfer that is not finished. One that finishes leaves the map: a
/// finished upload is in the note, and a finished download is in the store.
pub type TransferMap = HashMap<Transfer, TransferState>;

enum Command {
    Stop(tokio::sync::oneshot::Sender<()>),
    Kick,
    Fetch(NoteId, AttachmentId, Variant),
    Cancel(AttachmentId),
    Retry,
}

/// The app's side of the queue. Cloneable and cheap; the queue finishes once
/// every clone is dropped.
#[derive(Clone)]
pub struct TransferQueue {
    commands: mpsc::UnboundedSender<Command>,
    status: watch::Receiver<TransferMap>,
}

impl TransferQueue {
    /// Stop after the current store operation, before another queue reuses
    /// its records. In-flight network work is dropped with the runner.
    pub async fn stop(&self) {
        let (done, stopped) = tokio::sync::oneshot::channel();
        if self.commands.send(Command::Stop(done)).is_ok() {
            let _ = stopped.await;
        }
    }

    /// Look again: at the staging records, because an import has just been
    /// saved, and at every note's reach, because a sync status has changed.
    pub fn kick(&self) {
        let _ = self.commands.send(Command::Kick);
    }

    /// Fetch one variant of a live attachment, unless it is here already.
    pub fn fetch(&self, note: NoteId, id: AttachmentId, variant: Variant) {
        let _ = self.commands.send(Command::Fetch(note, id, variant));
    }

    /// Give up on an import that has not been published: its bytes and its
    /// record are deleted, here and nowhere else, and it is never published —
    /// not even by an upload finishing after this. An attachment already in
    /// the note is removed with `remove_attachment`, not this, and is left
    /// alone here.
    pub fn cancel(&self, id: AttachmentId) {
        let _ = self.commands.send(Command::Cancel(id));
    }

    /// Try every failed transfer again now, and every one waiting out a
    /// backoff.
    pub fn retry(&self) {
        let _ = self.commands.send(Command::Retry);
    }

    pub fn transfers(&self) -> TransferMap {
        self.status.borrow().clone()
    }

    /// The current transfers and every change to them.
    pub fn watch(&self) -> watch::Receiver<TransferMap> {
        self.status.clone()
    }
}

/// A token, and when it was issued, on [`now`]'s clock.
#[derive(Default)]
struct Tokens {
    held: tokio::sync::Mutex<Option<(String, Duration)>>,
    /// The relay's access key, for the handshake (ADR 0028).
    access: Option<RelayAccess>,
}

impl Tokens {
    async fn get(&self, http_base: &str, signer: &impl DeviceSigner) -> Result<String> {
        let mut held = self.held.lock().await;
        if let Some((token, at)) = held.as_ref() {
            if now().saturating_sub(*at) < TOKEN_LIFE {
                return Ok(token.clone());
            }
        }
        let token = authenticate(http_base, self.access.as_ref(), signer).await?;
        *held = Some((token.clone(), now()));
        Ok(token)
    }

    /// After any failure: a refused token is one cause, and asking for a new
    /// one is two calls against a transfer that is being retried anyway.
    fn forget(&self) {
        if let Ok(mut held) = self.held.try_lock() {
            *held = None;
        }
    }
}

/// How a running transfer reports its chunks.
struct Progress {
    status: Arc<watch::Sender<TransferMap>>,
    transfer: Transfer,
}

impl Progress {
    fn set(&self, done: u32, of: u32) {
        self.status.send_modify(|map| {
            map.insert(self.transfer, TransferState::Running { done, of });
        });
    }
}

struct Job {
    /// The record an upload publishes; `None` for a download.
    staged: Option<Staged>,
    /// When an upload last ran to a commit, on [`now`]'s clock: that is when
    /// the relay last heard that this device wants its bytes. `None` before
    /// the first run since launch.
    vouched: Option<Duration>,
    failures: u32,
    /// On [`now`]'s clock.
    not_before: Option<Duration>,
    failed: bool,
    running: bool,
}

impl Job {
    fn new(staged: Option<Staged>) -> Self {
        Self {
            staged,
            vouched: None,
            failures: 0,
            not_before: None,
            failed: false,
            running: false,
        }
    }
}

/// What looking at one job decided.
enum Step {
    /// Finished, or no longer wanted.
    Done,
    /// Not yet: its note is not far enough with the relay.
    Wait,
    /// Refused for good, until asked again.
    Fail(String),
    /// A network failure before anything was sent: back off.
    Retry(String),
    Start,
}

type Finished = (Transfer, Result<()>);

/// The loop. Build it with [`TransferRunner::new`] and spawn
/// [`run`](TransferRunner::run) on the runtime the engine uses.
pub struct TransferRunner<P: TransferPeer, S: MediaStorage> {
    peer: Arc<P>,
    store: Arc<S>,
    http_base: Option<String>,
    commands: mpsc::UnboundedReceiver<Command>,
    status: Arc<watch::Sender<TransferMap>>,
    tokens: Arc<Tokens>,
    jobs: HashMap<Transfer, Job>,
    /// Imports cancelled while their upload was running. The upload finishes or
    /// fails on its own; either way what follows is a discard.
    cancelled: HashSet<AttachmentId>,
    running: FuturesUnordered<Fut<'static, Finished>>,
}

impl<P: TransferPeer, S: MediaStorage> TransferRunner<P, S> {
    /// `http_base` is the relay's, or `None` for a device with no relay, whose
    /// every note is local-only.
    pub fn new(peer: P, store: Arc<S>, http_base: Option<String>) -> (Self, TransferQueue) {
        let (tx, rx) = mpsc::unbounded_channel();
        let (status_tx, status_rx) = watch::channel(TransferMap::new());
        let runner = Self {
            peer: Arc::new(peer),
            store,
            http_base,
            commands: rx,
            status: Arc::new(status_tx),
            tokens: Arc::default(),
            jobs: HashMap::new(),
            cancelled: HashSet::new(),
            running: FuturesUnordered::new(),
        };
        let queue = TransferQueue {
            commands: tx,
            status: status_rx,
        };
        (runner, queue)
    }

    /// The relay's access key, for a relay that lets in only the people its
    /// operator gave it to ([ADR 0028](../../../docs/adr/0028-who-may-write-to-a-relay.md)).
    /// Before [`run`](Self::run), which is the only time it is read.
    pub fn with_access(mut self, access: Option<RelayAccess>) -> Self {
        self.tokens = Arc::new(Tokens {
            held: Default::default(),
            access,
        });
        self
    }

    /// Run until every [`TransferQueue`] is dropped. A transfer in flight then
    /// is abandoned where it stands; the next launch resumes it.
    pub async fn run(mut self) {
        self.rescan().await;
        loop {
            self.schedule().await;
            let wake = self.next_wake();
            let sleep = async {
                match wake {
                    Some(after) => super::timer::sleep(after).await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                command = self.commands.recv() => match command {
                    Some(Command::Stop(done)) => {
                        let _ = done.send(());
                        break;
                    }
                    Some(command) => self.command(command).await,
                    None => break,
                },
                Some((transfer, result)) = self.running.next(), if !self.running.is_empty() => {
                    self.finished(transfer, result).await;
                }
                () = sleep => {}
            }
        }
    }

    async fn command(&mut self, command: Command) {
        match command {
            Command::Stop(_) => unreachable!("handled by run"),
            Command::Kick => self.rescan().await,
            Command::Fetch(note, id, variant) => {
                let transfer = Transfer {
                    note,
                    id,
                    direction: Direction::Down(variant),
                };
                self.jobs.entry(transfer).or_insert_with(|| Job::new(None));
            }
            Command::Cancel(id) => self.cancel(id).await,
            Command::Retry => {
                for job in self.jobs.values_mut() {
                    job.failed = false;
                    job.not_before = None;
                }
            }
        }
    }

    /// Take on every staging record the queue does not have yet. A record that
    /// has gone — settled, discarded, or purged with its note — takes its job
    /// with it, unless its upload is running; then the completion finds out.
    async fn rescan(&mut self) {
        let staged = match self.store.staged().await {
            Ok(staged) => staged,
            // The store cannot be read now. What it holds is still there, and
            // the next kick or recheck looks again.
            Err(_) => return,
        };
        let present: HashSet<AttachmentId> = staged.iter().map(|s| s.id).collect();
        let gone: Vec<Transfer> = self
            .jobs
            .iter()
            .filter(|(t, job)| {
                t.direction == Direction::Up && !job.running && !present.contains(&t.id)
            })
            .map(|(t, _)| *t)
            .collect();
        for transfer in gone {
            self.done(transfer);
        }
        for record in staged {
            let transfer = Transfer {
                note: record.note,
                id: record.id,
                direction: Direction::Up,
            };
            self.jobs
                .entry(transfer)
                .or_insert_with(|| Job::new(Some(record)));
        }
    }

    async fn cancel(&mut self, id: AttachmentId) {
        let transfer = self
            .jobs
            .keys()
            .find(|t| t.id == id && t.direction == Direction::Up)
            .copied();
        let staged = match transfer.and_then(|t| self.jobs.get(&t)) {
            Some(job) if job.running => {
                self.cancelled.insert(id);
                return;
            }
            Some(job) => job.staged.clone(),
            // Saved a moment ago and not scanned yet.
            None => self
                .store
                .staged()
                .await
                .ok()
                .and_then(|all| all.into_iter().find(|s| s.id == id)),
        };
        if let Some(staged) = staged {
            self.drop_import(&staged).await;
            self.done(Transfer {
                note: staged.note,
                id,
                direction: Direction::Up,
            });
        }
    }

    /// An import that will never be published. Discarded if nothing in the
    /// note names it; if its descriptor did get in — published before a crash,
    /// say — the objects are the note's and only the record goes.
    async fn drop_import(&mut self, staged: &Staged) {
        let _ = match self.peer.attachment_state(staged.note, staged.id) {
            Ok(AttachmentState::Live) => self.store.settle(staged.id).await,
            _ => self.store.discard(staged).await,
        };
    }

    /// Look at every job that is not running, not failed and not backing off,
    /// in priority order, and start what may start.
    async fn schedule(&mut self) {
        let now = now();
        let mut ready: Vec<Transfer> = self
            .jobs
            .iter()
            .filter(|(_, job)| {
                !job.running && !job.failed && job.not_before.is_none_or(|at| at <= now)
            })
            .map(|(t, _)| *t)
            .collect();
        ready.sort_by_key(|t| (t.priority(), t.id));
        for transfer in ready {
            match self.decide(transfer).await {
                Step::Done => self.done(transfer),
                Step::Wait => self.show(transfer, TransferState::Waiting),
                Step::Fail(error) => self.fail(transfer, error),
                Step::Retry(error) => self.back_off(transfer, error, None),
                Step::Start => self.start(transfer),
            }
        }
    }

    async fn decide(&mut self, transfer: Transfer) -> Step {
        let room = self.running.len() < MAX_RUNNING;
        let (note, id) = (transfer.note, transfer.id);
        match transfer.direction {
            Direction::Up => {
                let Some(staged) = self.jobs.get(&transfer).and_then(|j| j.staged.clone()) else {
                    return Step::Done;
                };
                let vouched = self.jobs.get(&transfer).and_then(|job| job.vouched);
                match self.peer.attachment_state(note, id) {
                    Err(e) => Step::Retry(e.to_string()),
                    // Published. Settled once the relay has the descriptor;
                    // until then, its bytes are this device's to vouch for.
                    Ok(AttachmentState::Live) => match self.peer.reach(note) {
                        Reach::LocalOnly => {
                            let _ = self.store.settle(id).await;
                            Step::Done
                        }
                        _ if self.peer.acknowledged(note) && vouched.is_some() => {
                            let _ = self.store.settle(id).await;
                            Step::Done
                        }
                        Reach::Synced if self.http_base.is_some() => {
                            if vouched.is_some_and(|at| now() < at + REVOUCH) {
                                Step::Wait
                            } else if room {
                                // The same upload: three calls and no bytes
                                // for an object the relay still has, and the
                                // whole object again for one it has deleted.
                                Step::Start
                            } else {
                                Step::Wait
                            }
                        }
                        _ => Step::Wait,
                    },
                    Ok(AttachmentState::Removed | AttachmentState::NoNote) => {
                        let _ = self.store.discard(&staged).await;
                        Step::Done
                    }
                    Ok(AttachmentState::Pending) => match self.peer.reach(note) {
                        Reach::LocalOnly => self.publish(&staged).await,
                        Reach::Synced if self.http_base.is_some() => {
                            if room {
                                Step::Start
                            } else {
                                Step::Wait
                            }
                        }
                        _ => Step::Wait,
                    },
                }
            }
            Direction::Down(variant) => {
                let media = match self.peer.attachment(note, id) {
                    Ok(Some(media)) => media,
                    // Removed, or the note itself forgotten: nothing to fetch.
                    Ok(None) | Err(CoreError::UnknownNote(_)) => return Step::Done,
                    Err(e) => return Step::Retry(e.to_string()),
                };
                let Some(media) = variant_ref(&media, variant) else {
                    return Step::Done;
                };
                if self.store.has(note, media).await {
                    return Step::Done;
                }
                match self.peer.reach(note) {
                    Reach::LocalOnly => Step::Done,
                    Reach::Connected | Reach::Synced if self.http_base.is_some() && room => {
                        Step::Start
                    }
                    _ => Step::Wait,
                }
            }
        }
    }

    /// Publish a staged import whose bytes are where they need to be, and
    /// forget its record. The last thing that happens to an upload.
    async fn publish(&mut self, staged: &Staged) -> Step {
        match self
            .peer
            .publish(staged.note, staged.id, staged.media.clone())
        {
            // Published and not settled is the state §3 recovers from: the next
            // scan finds the record and sees `Live`. Settled now only where
            // there is no relay; otherwise once the relay has the descriptor
            // (`decide`), so that an upload committed and never named on the
            // relay is still somebody's to vouch for (ADR 0027 §5).
            Ok(()) if self.peer.reach(staged.note) == Reach::LocalOnly => {
                let _ = self.store.settle(staged.id).await;
                Step::Done
            }
            Ok(()) => Step::Wait,
            Err(e) if permanent(&e) => Step::Fail(e.to_string()),
            Err(e) => Step::Retry(e.to_string()),
        }
    }

    fn start(&mut self, transfer: Transfer) {
        let Some(http_base) = self.http_base.clone() else {
            return;
        };
        let Some(job) = self.jobs.get_mut(&transfer) else {
            return;
        };
        let progress = Progress {
            status: self.status.clone(),
            transfer,
        };
        let (peer, store, tokens) = (self.peer.clone(), self.store.clone(), self.tokens.clone());
        let work: Fut<'static, Result<()>> = match transfer.direction {
            Direction::Up => {
                let Some(staged) = job.staged.clone() else {
                    return;
                };
                Box::pin(push(peer, store, tokens, http_base, staged, progress))
            }
            Direction::Down(variant) => {
                // Asked again here rather than carried from `decide`, which
                // has just done so: the two are one lock apart.
                let Ok(Some(media)) = peer.attachment(transfer.note, transfer.id) else {
                    return;
                };
                let Some(media) = variant_ref(&media, variant).cloned() else {
                    return;
                };
                Box::pin(pull(
                    peer,
                    store,
                    tokens,
                    http_base,
                    transfer.note,
                    media,
                    progress,
                ))
            }
        };
        job.running = true;
        self.running
            .push(Box::pin(async move { (transfer, work.await) }));
    }

    async fn finished(&mut self, transfer: Transfer, result: Result<()>) {
        if let Some(job) = self.jobs.get_mut(&transfer) {
            job.running = false;
            if result.is_ok() && transfer.direction == Direction::Up {
                job.vouched = Some(now());
            }
        }
        if let Err(e) = result {
            self.tokens.forget();
            if self.cancelled.remove(&transfer.id) {
                self.cancel(transfer.id).await;
            } else if permanent(&e) {
                self.fail(transfer, e.to_string());
            } else {
                let server_delay = match &e {
                    CoreError::RetryAfter(delay) => Some(*delay),
                    _ => None,
                };
                self.back_off(transfer, e.to_string(), server_delay);
            }
            return;
        }
        match transfer.direction {
            Direction::Down(_) => self.done(transfer),
            Direction::Up => {
                if self.cancelled.remove(&transfer.id) {
                    self.cancel(transfer.id).await;
                    return;
                }
                // Asked again: a removal, a deletion or a revocation may have
                // arrived while the bytes were going up. `decide` answers with
                // the same table, and for a pending import it publishes now,
                // since the only thing a synced note was waiting for is done.
                let Some(staged) = self.jobs.get(&transfer).and_then(|j| j.staged.clone()) else {
                    return;
                };
                let step = match self.peer.attachment_state(transfer.note, transfer.id) {
                    Ok(AttachmentState::Pending) => self.publish(&staged).await,
                    _ => self.decide(transfer).await,
                };
                match step {
                    Step::Done => self.done(transfer),
                    Step::Fail(error) => self.fail(transfer, error),
                    Step::Retry(error) => self.back_off(transfer, error, None),
                    Step::Wait | Step::Start => {}
                }
            }
        }
    }

    fn show(&self, transfer: Transfer, state: TransferState) {
        self.status.send_if_modified(|map| {
            if map.get(&transfer) == Some(&state) {
                return false;
            }
            map.insert(transfer, state);
            true
        });
    }

    fn done(&mut self, transfer: Transfer) {
        self.jobs.remove(&transfer);
        self.status
            .send_if_modified(|map| map.remove(&transfer).is_some());
    }

    fn fail(&mut self, transfer: Transfer, error: String) {
        if let Some(job) = self.jobs.get_mut(&transfer) {
            job.failed = true;
        }
        self.show(transfer, TransferState::Failed { error });
    }

    fn back_off(&mut self, transfer: Transfer, error: String, server_delay: Option<Duration>) {
        if let Some(job) = self.jobs.get_mut(&transfer) {
            let ceiling = FIRST_BACKOFF
                .saturating_mul(1 << job.failures.min(16))
                .min(MAX_BACKOFF);
            let wait = server_delay.unwrap_or_else(|| randomized_retry(ceiling));
            job.failures += 1;
            job.not_before = Some(now() + wait);
        }
        self.show(transfer, TransferState::Retrying { error });
    }

    /// When the loop next has something to look at on its own: the earliest
    /// backoff to run out, or a recheck while anything waits for its note.
    fn next_wake(&self) -> Option<Duration> {
        let now = now();
        let mut wake: Option<Duration> = None;
        let mut soonest = |after: Duration| {
            wake = Some(wake.map_or(after, |w| w.min(after)));
        };
        for job in self.jobs.values() {
            if job.running || job.failed {
                continue;
            }
            match job.not_before {
                Some(at) => soonest(at.saturating_sub(now)),
                None => soonest(RECHECK),
            }
        }
        wake
    }
}

fn randomized_retry(ceiling: Duration) -> Duration {
    let max_ms = ceiling.as_millis().min(u64::MAX as u128) as u64;
    let min_ms = (max_ms / 2).max(1);
    Duration::from_millis(rand::rng().random_range(min_ms..=max_ms.max(min_ms)))
}

/// Whether a failure is the relay's or the note's answer, rather than the
/// network's. [`super::media`] says an answer in words as
/// [`CoreError::Attachment`], and bytes that are not the digest as
/// [`CoreError::Crypto`]; everything the network does is `Storage`.
fn permanent(e: &CoreError) -> bool {
    matches!(e, CoreError::Attachment(_) | CoreError::Crypto(_))
}

/// Upload both of an import's variants, reporting each chunk as it goes.
async fn push<P: TransferPeer, S: MediaStorage>(
    peer: Arc<P>,
    store: Arc<S>,
    tokens: Arc<Tokens>,
    http_base: String,
    staged: Staged,
    progress: Progress,
) -> Result<()> {
    let token = tokens.get(&http_base, &*peer).await?;
    let variants: Vec<&MediaRef> = std::iter::once(&staged.media.original)
        .chain(staged.media.thumbnail.as_ref().map(|t| &t.media))
        .collect();
    let of = variants.iter().map(|v| v.chunks()).sum();
    let mut done = 0;
    progress.set(done, of);
    for variant in variants {
        let store = &*store;
        upload(&http_base, &token, staged.note, variant, |index| {
            // Counted as it is handed over rather than once it has arrived:
            // the relay's answer to the next call is what says it did.
            done += 1;
            progress.set(done, of);
            store.read_sealed_chunk(staged.note, variant, index)
        })
        .await?;
    }
    Ok(())
}

/// Download one variant into the store, which keeps it only once its length
/// and digest are the descriptor's.
async fn pull<P: TransferPeer, S: MediaStorage>(
    peer: Arc<P>,
    store: Arc<S>,
    tokens: Arc<Tokens>,
    http_base: String,
    note: NoteId,
    media: MediaRef,
    progress: Progress,
) -> Result<()> {
    if store.has(note, &media).await {
        return Ok(());
    }
    let token = tokens.get(&http_base, &*peer).await?;
    let of = media.chunks();
    let mut done = 0;
    progress.set(done, of);
    let mut incoming = store.incoming(note, &media).await?;
    download(&http_base, &token, note, &media, &mut incoming, || {
        done += 1;
        progress.set(done, of);
    })
    .await?;
    incoming.finish().await
}

#[cfg(test)]
mod retry_tests {
    use super::*;

    #[test]
    fn transfer_retry_delays_are_randomized_within_shared_ceilings() {
        for ceiling in [1, 2, 4, 8, 16, 32, 60].map(Duration::from_secs) {
            for _ in 0..100 {
                let delay = randomized_retry(ceiling);
                assert!(delay >= ceiling / 2);
                assert!(delay <= ceiling);
            }
        }
    }

    #[test]
    fn transfer_retry_ceiling_stops_at_sixty_seconds() {
        assert_eq!(MAX_BACKOFF, Duration::from_secs(60));
    }

    #[test]
    fn server_retry_after_is_transient_and_attachment_refusals_stay_terminal() {
        assert!(!permanent(&CoreError::RetryAfter(Duration::from_secs(9))));
        assert!(permanent(&CoreError::Attachment("relay refused".into())));
        assert!(permanent(&CoreError::Crypto("wrong digest")));
    }
}
