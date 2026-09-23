//! The sync loop: connect, catch up, publish, reconnect, repeat.
//!
//! Slice 1 proved two cores could sync through a real relay, but the loop that
//! did it lived in a test. This is that loop made real, and it is where every
//! decision a connection needs lives:
//!
//! * **The outbox is derived, not kept.** It is recomputed from the op log on
//!   every connection: our own envelopes that the relay has not echoed back.
//!   An app killed between an offline edit and the next connection therefore
//!   loses nothing, and no separate queue can drift from the log.
//! * **An echo is the acknowledgement.** The relay fans an append back to its
//!   sender like any other subscriber, so "the relay has it" needs no new
//!   message type: it is the envelope arriving back.
//! * **Delivery is at-least-once, and that is safe.** Reconnecting re-sends
//!   anything unacknowledged, which can duplicate an envelope the relay already
//!   stored. `NoteCore::apply_remote` deduplicates on `(author, counter)`, so a
//!   duplicate costs a log entry and changes nothing.
//! * **Nothing here decrypts anything.** The engine moves sealed envelopes
//!   between the socket and the core. It cannot read them and does not have the
//!   keys to try.
//! * **One socket per device, not per note.** Every note the device syncs is a
//!   subscription on one connection, with one token handshake and one backoff
//!   between them ([ADR 0017](../../docs/adr/0017-one-socket-per-device.md)).
//!   Everything above is still per note: its own cursor, outbox and held
//!   typing, exactly as when each note had a socket of its own.

// From the module rather than from one transport: which of `client` and
// `client_web` these come from is the target's business, and this file must not
// be able to tell.
use super::wire::{Incoming, Routed};
use super::{authenticate, DeviceSync};
use crate::crdt::NoteDoc;
use crate::crypto::{AuthorKey, DeviceSigner, Envelope};
use crate::error::{CoreError, Result};
use crate::ids::NoteId;
use crate::note::{NoteCore, SyncCursor};
use crate::store::{Keyring, OpStore};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::Poll;
use std::time::Duration;
use tokio::sync::{mpsc, watch};

/// How long to wait before the first reconnection attempt.
pub const FIRST_RETRY: Duration = Duration::from_millis(250);

/// The ceiling on the backoff. A phone that has been in a tunnel for an hour
/// should come back within seconds of leaving it, so this stays small.
pub const MAX_RETRY: Duration = Duration::from_secs(8);

/// When an edit made with [`SyncHandle::publish_soon`] goes to the relay.
///
/// Typing is held back and sent as one envelope per burst rather than one per
/// keystroke; [ADR 0016](../../docs/adr/0016-batching-edits.md) has the numbers
/// and the argument. Everything else — a tick, an item added or moved — goes the
/// moment it is made, because somebody on the other end of a shared list is
/// waiting to see it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pacing {
    /// A pause in the typing this long sends what it wrote.
    pub quiet: Duration,
    /// However long the typing goes on, nothing waits longer than this.
    pub max_hold: Duration,
}

impl Default for Pacing {
    fn default() -> Self {
        Self {
            quiet: Duration::from_secs(1),
            max_hold: Duration::from_secs(10),
        }
    }
}

// ---------------------------------------------------------------------------
// What the engine needs from the core
// ---------------------------------------------------------------------------

/// The core, as the engine sees it.
///
/// A trait rather than a concrete `NoteCore` because the engine runs in its own
/// task while the UI keeps editing: the real implementation is a lock, and the
/// engine must not know or care. It also keeps the engine testable against a
/// core that does nothing.
pub trait SyncPeer: DeviceSigner + Send + 'static {
    /// Verify, decrypt and apply an envelope from the relay.
    fn apply_remote(&self, note: NoteId, envelope: &[u8]) -> Result<()>;

    fn cursor(&self, note: NoteId) -> Result<SyncCursor>;

    fn save_cursor(&self, note: NoteId, cursor: SyncCursor) -> Result<()>;

    /// Our own envelopes the relay has not acknowledged, oldest first.
    fn unsent(&self, note: NoteId, after: Option<u64>) -> Result<Vec<(u64, Vec<u8>)>>;

    /// Fold our own unsent `envelopes` into as few as will do, rewrite the log
    /// to match, and return what to send in their place, oldest first — see
    /// [`NoteCore::squash`] for what makes that safe.
    ///
    /// Sending exactly what came in is always a correct answer, hence the
    /// default: it is what the engine did before it batched anything.
    fn squash(&self, note: NoteId, envelopes: Vec<Vec<u8>>) -> Result<Vec<Vec<u8>>> {
        let _ = note;
        Ok(envelopes)
    }

    /// The relay has said `Caught`: everything it had for this note, we have.
    ///
    /// The one thing the engine says about a *batch* rather than about an
    /// envelope, and the account document needs it. Taking a note key out of a
    /// replay one envelope at a time walks through states the document was never
    /// left in — a note created and later forgotten appears, with its key, in the
    /// middle of a log whose end does not mention it — and adoption only ever
    /// adds, so that key would be kept for ever. `Caught` is precisely the
    /// message that says the backlog is drained; see [ADR
    /// 0013](../../docs/adr/0013-recovery-phrase.md) decision 5. Afterwards
    /// every arriving envelope is its own settled batch, and this is called for
    /// each — a live edit is one edit, from a device that was up to date.
    ///
    /// It must therefore be cheap and idempotent: it runs per envelope for as
    /// long as a connection is up.
    ///
    /// Nothing for a note, hence the default: a note's envelopes mean the same
    /// thing whether they arrived in a backlog or one at a time.
    fn settled(&self, note: NoteId) -> Result<()> {
        let _ = note;
        Ok(())
    }
}

fn poisoned() -> CoreError {
    CoreError::Storage("core lock poisoned".into())
}

impl<D, S, K> DeviceSigner for Arc<Mutex<NoteCore<D, S, K>>>
where
    D: NoteDoc + Send + 'static,
    S: OpStore + Send + 'static,
    K: Keyring + Send + 'static,
{
    fn device_key(&self) -> Result<AuthorKey> {
        Ok(self.lock().map_err(|_| poisoned())?.device_key())
    }

    fn sign_challenge(&self, nonce: &[u8]) -> Result<[u8; crate::crypto::SIG_LEN]> {
        self.lock()
            .map_err(|_| poisoned())?
            .identity()
            .sign_challenge(nonce)
    }
}

impl<D, S, K> SyncPeer for Arc<Mutex<NoteCore<D, S, K>>>
where
    D: NoteDoc + Send + 'static,
    S: OpStore + Send + 'static,
    K: Keyring + Send + 'static,
{
    fn apply_remote(&self, note: NoteId, envelope: &[u8]) -> Result<()> {
        let mut core = self.lock().map_err(|_| poisoned())?;
        // The account document arrives through a socket like a note and is not
        // one: it is an `AccountDoc`, sealed under a different key, and
        // `apply_remote` would open it as a checklist. Which of the two this is
        // is the core's own answer, so the engine never has to hold two kinds of
        // peer.
        if core.account_id() == Some(note) {
            return core.apply_remote_account(envelope);
        }
        core.apply_remote(note, envelope).map(|_| ())
    }

    fn cursor(&self, note: NoteId) -> Result<SyncCursor> {
        self.lock().map_err(|_| poisoned())?.sync_cursor(note)
    }

    fn save_cursor(&self, note: NoteId, cursor: SyncCursor) -> Result<()> {
        self.lock()
            .map_err(|_| poisoned())?
            .set_sync_cursor(note, cursor)
    }

    fn unsent(&self, note: NoteId, after: Option<u64>) -> Result<Vec<(u64, Vec<u8>)>> {
        self.lock().map_err(|_| poisoned())?.unsent(note, after)
    }

    fn squash(&self, note: NoteId, envelopes: Vec<Vec<u8>>) -> Result<Vec<Vec<u8>>> {
        self.lock()
            .map_err(|_| poisoned())?
            .squash(note, &envelopes)
    }

    fn settled(&self, note: NoteId) -> Result<()> {
        let mut core = self.lock().map_err(|_| poisoned())?;
        if core.account_id() == Some(note) {
            core.adopt_note_keys()?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Where the relay is
// ---------------------------------------------------------------------------

/// One relay, as two base URLs.
///
/// The handshake is HTTP and the notes are WebSocket, and a user should not have
/// to know that: they give one address and this derives the other.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RelayEndpoint {
    pub http_base: String,
    pub ws_base: String,
}

impl RelayEndpoint {
    /// Accept either scheme and derive the other. `https` maps to `wss`, so a
    /// relay reached over TLS cannot be silently downgraded by the pairing.
    pub fn parse(url: &str) -> Result<Self> {
        let url = url.trim().trim_end_matches('/');
        let bad = || CoreError::Storage(format!("not a relay address: {url:?}"));
        let (http, ws) = match url.split_once("://") {
            Some(("http", rest)) => (format!("http://{rest}"), format!("ws://{rest}")),
            Some(("https", rest)) => (format!("https://{rest}"), format!("wss://{rest}")),
            Some(("ws", rest)) => (format!("http://{rest}"), format!("ws://{rest}")),
            Some(("wss", rest)) => (format!("https://{rest}"), format!("wss://{rest}")),
            _ => return Err(bad()),
        };
        if url
            .split_once("://")
            .is_none_or(|(_, rest)| rest.is_empty())
        {
            return Err(bad());
        }
        Ok(Self {
            http_base: http,
            ws_base: ws,
        })
    }
}

// ---------------------------------------------------------------------------
// What the UI sees
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyncState {
    /// Not connected. If `last_error` is set, that is why.
    Offline,
    Connecting,
    /// Connected, still receiving the backlog.
    Online,
    /// Connected and caught up: everything the relay had, we have.
    Synced,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyncStatus {
    pub state: SyncState,
    /// Our own edits the relay has not acknowledged. Non-zero while offline is
    /// normal and is the honest thing to show: the work is safe locally and not
    /// yet anywhere else.
    pub pending: usize,
    /// The relay sequence number we have applied up to.
    pub last_seq: i64,
    /// Why the last attempt failed. Kept while offline so the UI can explain
    /// itself, cleared once a connection succeeds.
    pub last_error: Option<String>,
}

impl Default for SyncStatus {
    fn default() -> Self {
        Self {
            state: SyncState::Offline,
            pending: 0,
            last_seq: 0,
            last_error: None,
        }
    }
}

enum Command {
    /// Start syncing a note, or hand an already synced one a new handle.
    /// `id` names this subscription, so that the handle it replaced cannot
    /// unsubscribe it on its way out.
    Subscribe {
        note: NoteId,
        id: u64,
        status: watch::Sender<SyncStatus>,
    },
    Unsubscribe {
        note: NoteId,
        id: u64,
    },
    /// `now` is false for an edit that may wait for the ones after it.
    Publish {
        note: NoteId,
        envelope: Vec<u8>,
        now: bool,
    },
    /// A key rotation supersedes every old-generation envelope still queued.
    Rekey {
        note: NoteId,
        envelope: Vec<u8>,
    },
    /// One note's held typing, or every note's.
    Flush {
        note: Option<NoteId>,
    },
    /// Every [`SyncDevice`] is gone. The engine finishes once its last note has.
    Released,
    Shutdown,
}

/// The device's side of the engine: subscribe notes to it, flush them all, or
/// stop it.
///
/// Cloneable and cheap. The engine keeps running while any clone of this or any
/// note's [`SyncHandle`] is alive, and finishes once the last of them is gone —
/// so an engine nobody can reach does not stay connected to the relay.
#[derive(Clone)]
pub struct SyncDevice {
    commands: mpsc::UnboundedSender<Command>,
    ids: Arc<AtomicU64>,
    _released: Arc<Release>,
}

/// Sends [`Command::Released`] when the last [`SyncDevice`] clone is dropped.
struct Release(mpsc::UnboundedSender<Command>);

impl Drop for Release {
    fn drop(&mut self) {
        let _ = self.0.send(Command::Released);
    }
}

impl SyncDevice {
    /// Start syncing `note` over the device's connection, and return its handle.
    ///
    /// A note can be connected at any point, not only before the engine first
    /// connects: that is how a note made, joined or adopted from the account
    /// document gets synced without a socket of its own. Connecting a note that
    /// already has a handle replaces that handle; what the engine holds for the
    /// note — its cursor, its outbox, its held typing — carries over.
    pub fn connect(&self, note: NoteId) -> SyncHandle {
        let id = self.ids.fetch_add(1, Ordering::Relaxed);
        let (status_tx, status) = watch::channel(SyncStatus::default());
        let _ = self.commands.send(Command::Subscribe {
            note,
            id,
            status: status_tx,
        });
        SyncHandle {
            note,
            commands: self.commands.clone(),
            status,
            subscription: Arc::new(Subscription {
                note,
                id,
                commands: self.commands.clone(),
            }),
        }
    }

    /// Send every note's held typing now — see [`SyncHandle::flush`].
    pub fn flush(&self) {
        let _ = self.commands.send(Command::Flush { note: None });
    }

    /// Close the connection and finish, whatever notes are still connected.
    pub fn shutdown(&self) {
        let _ = self.commands.send(Command::Shutdown);
    }
}

/// Unsubscribes a note when the last clone of its handle is dropped.
struct Subscription {
    note: NoteId,
    id: u64,
    commands: mpsc::UnboundedSender<Command>,
}

impl Subscription {
    fn end(&self) {
        let _ = self.commands.send(Command::Unsubscribe {
            note: self.note,
            id: self.id,
        });
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        self.end();
    }
}

/// One note's remote control: hand it envelopes, read its status, stop it.
///
/// Cloneable and cheap. Dropping every clone stops syncing the note, so a note
/// that goes away cannot leave the engine talking to the relay about it.
#[derive(Clone)]
pub struct SyncHandle {
    note: NoteId,
    commands: mpsc::UnboundedSender<Command>,
    status: watch::Receiver<SyncStatus>,
    subscription: Arc<Subscription>,
}

impl SyncHandle {
    /// Send a sealed envelope to the relay now, and anything held back with it.
    ///
    /// Returns nothing and cannot fail: an edit is already in the log by the time
    /// it gets here, so the only question is when the relay hears about it, and
    /// the answer is "when there is a connection". A stopped engine drops it, and
    /// the next connection picks it up from the log.
    pub fn publish(&self, envelope: Vec<u8>) {
        let _ = self.commands.send(Command::Publish {
            note: self.note,
            envelope,
            now: true,
        });
    }

    /// Send a sealed envelope once the typing that made it pauses — see
    /// [`Pacing`]. What a keystroke calls, and nothing else should: an edit
    /// someone else is waiting to see goes through [`publish`](Self::publish).
    ///
    /// Held rather than lost: it is in the log already, so an app killed while
    /// it waits sends it on the next connection.
    pub fn publish_soon(&self, envelope: Vec<u8>) {
        let _ = self.commands.send(Command::Publish {
            note: self.note,
            envelope,
            now: false,
        });
    }

    /// Publish a key-rotation snapshot and discard old-key work still held by
    /// the pacing layer. The snapshot already contains those edits.
    pub fn rekey(&self, envelope: Vec<u8>) {
        let _ = self.commands.send(Command::Rekey {
            note: self.note,
            envelope,
        });
    }

    /// Send whatever is being held back, now. For the moments the app may not
    /// get another chance to: going into the background, a tab being hidden, a
    /// note being closed.
    pub fn flush(&self) {
        let _ = self.commands.send(Command::Flush {
            note: Some(self.note),
        });
    }

    pub fn status(&self) -> SyncStatus {
        self.status.borrow().clone()
    }

    /// The current status and every change to it.
    pub fn watch_status(&self) -> watch::Receiver<SyncStatus> {
        self.status.clone()
    }

    /// Stop syncing this note. The device's connection stays up for its other
    /// notes; an engine started with [`SyncEngine::start`] has no others, and
    /// closes it and finishes.
    pub fn shutdown(&self) {
        self.subscription.end();
    }
}

// ---------------------------------------------------------------------------
// The engine
// ---------------------------------------------------------------------------

/// One device's sync: one socket to the relay, carrying every note connected
/// to it ([ADR 0017](../../docs/adr/0017-one-socket-per-device.md)).
///
/// The split is along a line that was already in the engine. What is about the
/// socket — the token handshake, the reconnect loop and its one backoff,
/// routing frames in and out by note — is the device's. What is about a note —
/// its cursor, its outbox derived from the log, its held typing and their
/// timers, whether it has caught up — is the note's, exactly as it was when
/// each note had a socket of its own.
pub struct SyncEngine<P: SyncPeer> {
    peer: P,
    endpoint: RelayEndpoint,
    commands: mpsc::UnboundedReceiver<Command>,
    pacing: Pacing,
}

impl<P: SyncPeer> SyncEngine<P> {
    /// Build an engine for a device, and the handle to connect notes to it.
    /// Nothing happens until [`run`](Self::run) is spawned — on whatever
    /// executor the caller has, which on the app side is a runtime the bridge
    /// owns and in tests is the test's own.
    pub fn device(peer: P, endpoint: RelayEndpoint) -> (Self, SyncDevice) {
        Self::device_paced(peer, endpoint, Pacing::default())
    }

    /// [`device`](Self::device), holding typing back for other than the default
    /// times. For tests, which cannot wait ten seconds to see a cap work.
    pub fn device_paced(peer: P, endpoint: RelayEndpoint, pacing: Pacing) -> (Self, SyncDevice) {
        let (tx, rx) = mpsc::unbounded_channel();
        let engine = Self {
            peer,
            endpoint,
            commands: rx,
            pacing,
        };
        let device = SyncDevice {
            commands: tx.clone(),
            ids: Arc::new(AtomicU64::new(0)),
            _released: Arc::new(Release(tx)),
        };
        (engine, device)
    }

    /// An engine syncing one note, which finishes when the note's handle is
    /// shut down or dropped. A device with one note, and what every test that
    /// is about a note rather than a device wants.
    pub fn start(peer: P, note: NoteId, endpoint: RelayEndpoint) -> (Self, SyncHandle) {
        Self::start_paced(peer, note, endpoint, Pacing::default())
    }

    /// [`start`](Self::start) with [`Pacing`] of its own.
    pub fn start_paced(
        peer: P,
        note: NoteId,
        endpoint: RelayEndpoint,
        pacing: Pacing,
    ) -> (Self, SyncHandle) {
        let (engine, device) = Self::device_paced(peer, endpoint, pacing);
        (engine, device.connect(note))
    }

    /// Run until [`SyncDevice::shutdown`], or until every handle to the engine
    /// is gone.
    ///
    /// Never returns an error: a relay that cannot be reached is a state to show,
    /// not a failure to report, and the app keeps working without one.
    pub async fn run(self) {
        // Destructured so the borrow checker can see that the command channel and
        // the engine's state are separate things. `select!` holds both at once.
        let Self {
            peer,
            endpoint,
            mut commands,
            pacing,
        } = self;
        let mut device = Device {
            notes: HashMap::new(),
            released: false,
            pacing,
            last_error: None,
        };

        let mut backoff = FIRST_RETRY;
        loop {
            // Nothing to sync: wait for a note rather than hold a socket open
            // to a relay with nothing to say to it.
            if device.notes.is_empty() {
                if !wait(None, &mut commands, &mut device).await {
                    break;
                }
                continue;
            }
            device.each(|note| note.set_state(SyncState::Connecting));
            match session(&peer, &endpoint, &mut commands, &mut device).await {
                Ended::ShutDown => break,
                // A connection that worked and then went away deserves a quick
                // retry: this is a dropped socket, not a relay that is not
                // there. One that was accepted and never caught up does not —
                // otherwise a relay that flaps is a relay we hammer every 250 ms
                // for as long as it keeps flapping.
                Ended::Connected {
                    caught: true,
                    reason,
                } => {
                    device.fail(reason);
                    backoff = FIRST_RETRY;
                }
                Ended::Connected { reason, .. } | Ended::Failed(reason) => {
                    device.fail(reason);
                    backoff = (backoff * 2).min(MAX_RETRY);
                }
            }
            if !wait(Some(backoff), &mut commands, &mut device).await {
                break;
            }
        }
        device.each(|note| note.set_state(SyncState::Offline));
    }
}

enum Ended {
    /// The engine was asked to stop, or nothing can reach it any more.
    ShutDown,
    /// We were connected and lost it. `caught` says whether any note got as far
    /// as being up to date, which is the difference between a connection that
    /// worked and one that was accepted and then fell over.
    Connected { caught: bool, reason: String },
    /// We never got connected.
    Failed(String),
}

/// What handling a command while connected leads to.
enum Next {
    Continue,
    Stop,
}

/// What an incoming note message did to this connection.
enum Received {
    Continue,
    Caught,
    /// The relay was restored behind a local cursor. The cursor has been
    /// persisted at zero; reconnect so the outbox is derived again from it.
    Reset,
}

/// One connection, from handshake to whatever ends it.
async fn session<P: SyncPeer>(
    peer: &P,
    endpoint: &RelayEndpoint,
    commands: &mut mpsc::UnboundedReceiver<Command>,
    device: &mut Device,
) -> Ended {
    let me = match peer.device_key() {
        Ok(key) => key,
        Err(e) => return Ended::Failed(e.to_string()),
    };

    // A token every time. They expire, and a reconnect after a long offline spell
    // is exactly the case where the old one would be stale. One per connection,
    // however many notes it carries.
    let token = match authenticate(&endpoint.http_base, peer).await {
        Ok(token) => token,
        Err(e) => return Ended::Failed(e.to_string()),
    };

    let mut sync = match DeviceSync::connect(&endpoint.ws_base, &token).await {
        Ok(sync) => sync,
        Err(e) => return Ended::Failed(e.to_string()),
    };

    let mut caught = false;
    let lost = |caught: bool, reason: String| Ended::Connected { caught, reason };

    // Every note from its own cursor, each with its own outbox folded.
    let ids: Vec<NoteId> = device.notes.keys().copied().collect();
    for id in ids {
        if let Err(e) = device.start(peer, id, &mut sync).await {
            return lost(caught, e);
        }
    }

    loop {
        tokio::select! {
            command = commands.recv() => {
                let Some(command) = command else {
                    let _ = sync.close().await;
                    return Ended::ShutDown;
                };
                match device.online(peer, command, &mut sync).await {
                    Ok(Next::Continue) => {}
                    // Not flushed first. What is held is in the log, and the
                    // next connection sends it; a shutdown is also how a note
                    // about to be deleted is let go of, and rewriting its log on
                    // the way out would race the delete.
                    Ok(Next::Stop) => {
                        let _ = sync.close().await;
                        return Ended::ShutDown;
                    }
                    Err(e) => return lost(caught, e),
                }
            },

            // Pending for ever while nothing is held, so this only ever wins
            // when a pause or the cap says some note's typing has waited long
            // enough.
            id = device.due() => {
                if let Err(e) = device.flush(peer, id, &mut sync).await {
                    return lost(caught, e);
                }
            },

            // Cancel-safe: the socket buffers a partial frame internally, so
            // dropping this future when another branch wins loses nothing.
            incoming = sync.next() => match incoming {
                Ok(Some(Routed::Note(id, incoming))) => {
                    match device.receive(peer, &me, id, incoming) {
                        Ok(Received::Continue) => {}
                        Ok(Received::Caught) => caught = true,
                        Ok(Received::Reset) => {
                            let _ = sync.close().await;
                            return lost(true, "the relay was restored; rebuilding its copy".into());
                        }
                        Err(e) => return lost(caught, e),
                    }
                }
                Ok(Some(Routed::Unrouted(message))) => {
                    // The relay refused something without saying what it was
                    // about. Every note hears it, because any of them might
                    // be the one.
                    device.each(|note| note.note_error(message.clone()));
                }
                Ok(None) => return lost(caught, "the relay closed the connection".into()),
                Err(e) => return lost(caught, e.to_string()),
            },
        }
    }
}

/// Sleep out the backoff, or with `None` wait for a note to sync, but stay
/// listening: an edit made while offline belongs in the outbox now, not after
/// the next connection, or the UI would under-report what is pending.
///
/// Returns false if the engine should stop.
async fn wait(
    backoff: Option<Duration>,
    commands: &mut mpsc::UnboundedReceiver<Command>,
    device: &mut Device,
) -> bool {
    // `super::timer` rather than `tokio::time`, because a browser tab has no
    // runtime to drive a tokio timer and cannot be given one.
    let deadline = async {
        match backoff {
            Some(backoff) => super::timer::sleep(backoff).await,
            None => std::future::pending().await,
        }
    };
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            () = &mut deadline => return true,
            command = commands.recv() => match command {
                None | Some(Command::Shutdown) => return false,
                Some(Command::Subscribe { note, id, status }) => {
                    device.subscribe(note, id, status);
                    // Waiting for a note, and here is one.
                    if backoff.is_none() {
                        return true;
                    }
                }
                Some(Command::Unsubscribe { note, id }) => {
                    device.unsubscribe(note, id);
                    if device.finished() {
                        return false;
                    }
                }
                // Held with no timer on it: there is nowhere to send it, and the
                // next connection reads it back out of the log anyway. It is
                // here only so that `pending` counts it.
                Some(Command::Publish { note, envelope, .. }) => {
                    if let Some(note) = device.notes.get_mut(&note) {
                        note.held.push(envelope);
                        note.publish();
                    }
                }
                Some(Command::Rekey { note, envelope }) => {
                    if let Some(note) = device.notes.get_mut(&note) {
                        note.release();
                        note.outbox.clear();
                        note.covered = None;
                        note.held.push(envelope);
                        note.publish();
                    }
                }
                Some(Command::Flush { .. }) => {}
                Some(Command::Released) => {
                    device.released = true;
                    if device.finished() {
                        return false;
                    }
                }
            },
        }
    }
}

/// Everything the engine holds that outlives one connection.
struct Device {
    notes: HashMap<NoteId, Live>,
    /// Every [`SyncDevice`] has been dropped, so no note can be connected
    /// again: once the last one goes, so does the engine.
    released: bool,
    pacing: Pacing,
    /// Why the last connection ended, for a note connected while there is
    /// none.
    last_error: Option<String>,
}

impl Device {
    fn each(&mut self, mut f: impl FnMut(&mut Live)) {
        for note in self.notes.values_mut() {
            f(note);
        }
    }

    fn finished(&self) -> bool {
        self.released && self.notes.is_empty()
    }

    /// The connection is gone. Every note is offline with the same reason.
    fn fail(&mut self, reason: String) {
        self.last_error = Some(reason.clone());
        self.each(|note| {
            note.on_socket = false;
            note.fail(reason.clone());
        });
    }

    /// Take a note on, or give one already here its new handle. Returns true if
    /// the note is new, and so has to be started on a live connection.
    fn subscribe(&mut self, note: NoteId, id: u64, status: watch::Sender<SyncStatus>) -> bool {
        if let Some(live) = self.notes.get_mut(&note) {
            live.id = id;
            live.status = status;
            live.publish();
            return false;
        }
        let mut live = Live::new(id, status, self.pacing);
        // Not connected, and saying why if a connection has failed. A note
        // connected in the middle of a backoff is as offline as the rest.
        live.current.last_error = self.last_error.clone();
        live.publish();
        self.notes.insert(note, live);
        true
    }

    /// Let a note go, if `id` is still its subscription. Returns whether it was
    /// on the socket, and so has to be unsubscribed there too.
    fn unsubscribe(&mut self, note: NoteId, id: u64) -> bool {
        match self.notes.get(&note) {
            Some(live) if live.id == id => {}
            _ => return false,
        }
        let mut live = self.notes.remove(&note).expect("checked above");
        live.set_state(SyncState::Offline);
        live.on_socket
    }

    /// Put one note on a live connection: subscribe it from its cursor, and
    /// send what it has not.
    ///
    /// The error is the socket's, and ends the connection. A note whose own
    /// store fails is told so and left off the socket, rather than taking every
    /// other note's connection down with it.
    async fn start<P: SyncPeer>(
        &mut self,
        peer: &P,
        id: NoteId,
        sync: &mut DeviceSync,
    ) -> std::result::Result<(), String> {
        let Some(live) = self.notes.get_mut(&id) else {
            return Ok(());
        };
        live.on_socket = false;
        live.caught = false;

        // Re-read rather than trust what we remember: the core has been editing
        // and persisting while we were away.
        match peer.cursor(id) {
            Ok(cursor) => {
                live.cursor = cursor;
                live.saved = cursor;
            }
            Err(e) => {
                live.fail(e.to_string());
                return Ok(());
            }
        }

        sync.subscribe(id, live.cursor.last_seq)
            .await
            .map_err(|e| e.to_string())?;
        live.on_socket = true;
        live.connected();

        // Everything this device wrote that the relay has not acknowledged,
        // including edits from earlier runs of the app and whatever was held
        // back while offline — all of it is in the log, so the log is where it
        // is read from, and it replaces the outbox rather than joining it.
        // Folded into as few envelopes as will do: an hour of offline typing is
        // one burst as far as anyone else is concerned.
        match peer.unsent(id, live.cursor.sent_upto) {
            Ok(unsent) => {
                // A `Publish` still in the channel for anything read here is the
                // same edit arriving a second time. The counter says so, because
                // the log gets an edit before the channel does.
                live.covered = unsent.last().map(|(counter, _)| *counter);
                live.release();
                let unsent = unsent.into_iter().map(|(_, envelope)| envelope).collect();
                live.outbox = live.squash(peer, id, unsent);
            }
            Err(e) => {
                live.release();
                live.outbox.clear();
                live.note_error(e.to_string());
            }
        }
        live.publish();

        for envelope in live.outbox.clone() {
            sync.append(id, &envelope)
                .await
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    /// A command, while connected.
    async fn online<P: SyncPeer>(
        &mut self,
        peer: &P,
        command: Command,
        sync: &mut DeviceSync,
    ) -> std::result::Result<Next, String> {
        match command {
            Command::Shutdown => return Ok(Next::Stop),
            Command::Released => {
                self.released = true;
                if self.finished() {
                    return Ok(Next::Stop);
                }
            }
            Command::Subscribe { note, id, status } => {
                // A note already here is started again only if it never made
                // it onto this socket — its store failed the first time.
                let new = self.subscribe(note, id, status);
                if new || self.notes.get(&note).is_some_and(|live| !live.on_socket) {
                    self.start(peer, note, sync).await?;
                }
            }
            Command::Unsubscribe { note, id } => {
                if self.unsubscribe(note, id) {
                    sync.unsubscribe(note).await.map_err(|e| e.to_string())?;
                }
                if self.finished() {
                    return Ok(Next::Stop);
                }
            }
            Command::Publish {
                note,
                envelope,
                now,
            } => {
                let Some(live) = self.notes.get_mut(&note) else {
                    return Ok(Next::Continue);
                };
                // Already covered means already sent in this session: the edit
                // landed in the log before the note was subscribed, so the
                // catch-up picked it up and the command is the same envelope
                // arriving a second time. Sending it again would put a
                // duplicate in the relay's log on every launch where the user
                // edits while it is still connecting.
                if live.covers(&envelope) {
                    return Ok(Next::Continue);
                }
                live.hold(envelope);
                if now {
                    self.flush(peer, note, sync).await?;
                }
            }
            Command::Rekey { note, envelope } => {
                let Some(live) = self.notes.get_mut(&note) else {
                    return Ok(Next::Continue);
                };
                live.release();
                live.outbox.clear();
                live.covered = None;
                live.hold(envelope);
                self.flush(peer, note, sync).await?;
            }
            Command::Flush { note: Some(note) } => self.flush(peer, note, sync).await?,
            Command::Flush { note: None } => {
                let ids: Vec<NoteId> = self.notes.keys().copied().collect();
                for id in ids {
                    self.flush(peer, id, sync).await?;
                }
            }
        }
        Ok(Next::Continue)
    }

    /// Send everything one note holds back, folded into as few envelopes as
    /// will do.
    ///
    /// The error is the socket's, and ends the connection; nothing is lost with
    /// it, because what was held is in the log and the next connection reads it
    /// there.
    async fn flush<P: SyncPeer>(
        &mut self,
        peer: &P,
        id: NoteId,
        sync: &mut DeviceSync,
    ) -> std::result::Result<(), String> {
        let Some(live) = self.notes.get_mut(&id) else {
            return Ok(());
        };
        // Left held, with its timers stopped, when the note could not be put on
        // the socket: the relay would refuse it, and the log has it anyway.
        if !live.on_socket {
            live.quiet = None;
            live.cap = None;
            return Ok(());
        }
        let held = live.release();
        if held.is_empty() {
            return Ok(());
        }
        let batch = live.squash(peer, id, held);
        live.outbox.extend(batch.iter().cloned());
        live.publish();
        for envelope in &batch {
            sync.append(id, envelope).await.map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    /// Whichever note's batching timer goes off first, or never if none is
    /// running.
    ///
    /// Cancel-safe, which `select!` needs: the timers are polled where they
    /// are kept rather than moved in, so dropping this when another branch
    /// wins leaves them running. A timer that has fired is not polled again,
    /// because the flush that follows takes it away.
    fn due(&mut self) -> impl std::future::Future<Output = NoteId> + '_ {
        std::future::poll_fn(move |cx| {
            for (id, live) in self.notes.iter_mut() {
                for timer in [&mut live.quiet, &mut live.cap].into_iter().flatten() {
                    if timer.as_mut().poll(cx).is_ready() {
                        return Poll::Ready(*id);
                    }
                }
            }
            Poll::Pending
        })
    }

    /// Something arrived for a note.
    fn receive<P: SyncPeer>(
        &mut self,
        peer: &P,
        me: &AuthorKey,
        id: NoteId,
        incoming: Incoming,
    ) -> std::result::Result<Received, String> {
        // A note unsubscribed a moment ago can still have messages in flight.
        let Some(live) = self.notes.get_mut(&id) else {
            return Ok(Received::Continue);
        };
        match incoming {
            Incoming::Envelope { seq, envelope } => {
                live.receive(peer, id, me, seq, &envelope)?;
                // Once the backlog is drained, every envelope is its own
                // settled batch: it is one edit, made by a device that was
                // itself up to date, so the state it leaves the document in is
                // a state that device meant. During the backlog it is not,
                // which is the whole reason `settled` exists — hence the flag
                // rather than calling this unconditionally.
                if live.caught {
                    settle(peer, id, live);
                }
                Ok(Received::Continue)
            }
            Incoming::Caught { seq } => {
                live.caught = true;
                live.advance(peer, id, seq);
                // After the cursor has moved, so that a peer that does work
                // here and then fails does not also lose its place in the log
                // it has already applied.
                settle(peer, id, live);
                live.set_state(SyncState::Synced);
                Ok(Received::Caught)
            }
            Incoming::Reset => {
                live.reset(peer, id)?;
                Ok(Received::Reset)
            }
            Incoming::Rejected(message) => {
                // The relay refused one envelope. The connection is fine and the
                // rest of the outbox still goes.
                live.note_error(message);
                Ok(Received::Continue)
            }
        }
    }
}

/// Tell the peer the batch has settled, and keep going if it complains.
///
/// A peer that cannot finish its end-of-batch work has not invalidated anything
/// the connection has already applied, so this is a line for the UI rather than
/// a reason to drop a socket.
fn settle<P: SyncPeer>(peer: &P, note: NoteId, live: &mut Live) {
    if let Err(e) = peer.settled(note) {
        live.note_error(e.to_string());
    }
}

/// One note's state, and the only thing that touches its status channel.
struct Live {
    /// Which subscription this is; see [`Command::Subscribe`].
    id: u64,
    status: watch::Sender<SyncStatus>,
    current: SyncStatus,
    /// Whether the note is subscribed on the current connection.
    on_socket: bool,
    /// Whether the relay has said `caught` for it on the current connection.
    caught: bool,
    /// Sealed envelopes sent on this connection that the relay has not echoed
    /// back, in the order they were sent.
    outbox: Vec<Vec<u8>>,
    /// Edits made and logged but not sent yet, because the typing that made
    /// them has not paused. See [`Pacing`].
    held: Vec<Vec<u8>>,
    /// The highest counter of ours the connection's catch-up read from the log.
    /// Anything at or below it is already in the outbox, whatever it was folded
    /// into on the way.
    covered: Option<u64>,
    pacing: Pacing,
    /// Restarted by every held edit.
    quiet: Option<super::timer::Timer>,
    /// Started by the first held edit of a batch, and left alone by the rest.
    cap: Option<super::timer::Timer>,
    cursor: SyncCursor,
    /// What the store holds, so a cursor that has not moved is not written again.
    saved: SyncCursor,
}

impl Live {
    fn new(id: u64, status: watch::Sender<SyncStatus>, pacing: Pacing) -> Self {
        Self {
            id,
            status,
            current: SyncStatus::default(),
            on_socket: false,
            caught: false,
            outbox: Vec::new(),
            held: Vec::new(),
            covered: None,
            pacing,
            quiet: None,
            cap: None,
            cursor: SyncCursor::default(),
            saved: SyncCursor::default(),
        }
    }

    fn publish(&mut self) {
        self.current.pending = self.outbox.len() + self.held.len();
        self.current.last_seq = self.cursor.last_seq;
        // Fails only when nothing is watching, which is not a problem.
        let _ = self.status.send(self.current.clone());
    }

    fn set_state(&mut self, state: SyncState) {
        self.current.state = state;
        self.publish();
    }

    fn connected(&mut self) {
        self.current.last_error = None;
        self.set_state(SyncState::Online);
    }

    fn fail(&mut self, reason: String) {
        self.caught = false;
        self.current.last_error = Some(reason);
        self.set_state(SyncState::Offline);
    }

    /// Something went wrong that did not end the connection.
    fn note_error(&mut self, reason: String) {
        self.current.last_error = Some(reason);
        self.publish();
    }

    /// Hold an edit back until the typing pauses, or until the batch it starts
    /// has waited as long as any may.
    fn hold(&mut self, envelope: Vec<u8>) {
        if self.held.is_empty() {
            self.cap = Some(super::timer::timer(self.pacing.max_hold));
        }
        self.quiet = Some(super::timer::timer(self.pacing.quiet));
        self.held.push(envelope);
        self.publish();
    }

    /// Everything held, with the timers that were waiting to send it stopped.
    fn release(&mut self) -> Vec<Vec<u8>> {
        self.quiet = None;
        self.cap = None;
        std::mem::take(&mut self.held)
    }

    fn covers(&self, envelope: &[u8]) -> bool {
        let Some(covered) = self.covered else {
            return false;
        };
        Envelope::decode(envelope).is_ok_and(|e| e.counter <= covered)
    }

    /// [`SyncPeer::squash`], falling back to sending what came in unchanged.
    ///
    /// A merge that fails has cost nothing but the saving: the originals are
    /// still in the log and still correct to send.
    fn squash<P: SyncPeer>(
        &mut self,
        peer: &P,
        note: NoteId,
        envelopes: Vec<Vec<u8>>,
    ) -> Vec<Vec<u8>> {
        if envelopes.len() < 2 {
            return envelopes;
        }
        match peer.squash(note, envelopes.clone()) {
            Ok(batch) => batch,
            Err(e) => {
                self.current.last_error = Some(e.to_string());
                envelopes
            }
        }
    }

    /// An envelope arrived. Apply it, acknowledge it if it is ours, and move the
    /// cursor past it either way.
    fn receive<P: SyncPeer>(
        &mut self,
        peer: &P,
        note: NoteId,
        me: &AuthorKey,
        seq: i64,
        envelope: &[u8],
    ) -> std::result::Result<(), String> {
        if let Err(e) = peer.apply_remote(note, envelope) {
            if matches!(e, CoreError::Storage(_)) {
                // Do not advance past an envelope that could not be persisted.
                // Ending the session makes the next subscription replay it from
                // the last saved cursor.
                return Err(e.to_string());
            }
            // A rejected envelope is a fact about that envelope, not a reason to
            // stop or to ask for it again — hence the cursor moving on below. The
            // core has already refused to let it near the document.
            self.current.last_error = Some(e.to_string());
        }
        if let Ok(parsed) = Envelope::decode(envelope) {
            if &parsed.author == me {
                self.acknowledge(parsed.counter, envelope);
            } else {
                // A rotation snapshot is signed by the note's revocation key,
                // not the device key used for the relay handshake. Byte equality
                // still proves this is the envelope queued above; only the
                // per-device `sent_upto` watermark must remain untouched.
                self.outbox.retain(|queued| queued != envelope);
            }
        }
        self.advance(peer, note, seq);
        Ok(())
    }

    /// The relay has our envelope: drop it from the outbox and remember how far
    /// the acknowledgement reaches.
    fn acknowledge(&mut self, counter: u64, envelope: &[u8]) {
        self.outbox.retain(|queued| queued != envelope);
        self.cursor.sent_upto = Some(match self.cursor.sent_upto {
            Some(sent) => sent.max(counter),
            None => counter,
        });
    }

    /// The relay explicitly says its log is older than this device's cursor.
    /// This is the sole allowed cursor rewind: normal updates and `caught`
    /// messages stay monotonic. Clearing `sent_upto` is what makes the next
    /// connection derive every locally retained envelope into its outbox.
    fn reset<P: SyncPeer>(&mut self, peer: &P, note: NoteId) -> std::result::Result<(), String> {
        let cursor = SyncCursor::default();
        peer.save_cursor(note, cursor).map_err(|e| e.to_string())?;
        self.cursor = cursor;
        self.saved = cursor;
        self.caught = false;
        self.outbox.clear();
        self.covered = None;
        self.publish();
        Ok(())
    }

    fn advance<P: SyncPeer>(&mut self, peer: &P, note: NoteId, seq: i64) {
        if seq > self.cursor.last_seq {
            self.cursor.last_seq = seq;
        }
        // Only when it moved. Comparing against what is on disk rather than
        // tracking a dirty flag: it is one comparison, and it cannot get out of
        // step with what was actually written.
        if self.cursor != self.saved {
            match peer.save_cursor(note, self.cursor) {
                Ok(()) => self.saved = self.cursor,
                Err(e) => self.current.last_error = Some(e.to_string()),
            }
        }
        self.publish();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_relay_address_yields_both_schemes() {
        let http = RelayEndpoint::parse("http://relay.example:8080").unwrap();
        assert_eq!(http.http_base, "http://relay.example:8080");
        assert_eq!(http.ws_base, "ws://relay.example:8080");

        // TLS in, TLS out. Pairing `https` with plain `ws` would downgrade the
        // note traffic while the handshake looked secure.
        let https = RelayEndpoint::parse("https://relay.example/").unwrap();
        assert_eq!(https.http_base, "https://relay.example");
        assert_eq!(https.ws_base, "wss://relay.example");

        assert_eq!(
            RelayEndpoint::parse("wss://relay.example").unwrap(),
            RelayEndpoint::parse("https://relay.example").unwrap()
        );
    }

    #[test]
    fn nonsense_is_refused_rather_than_guessed_at() {
        for bad in ["", "relay.example:8080", "ftp://relay.example", "http://"] {
            assert!(
                RelayEndpoint::parse(bad).is_err(),
                "{bad:?} should not parse as a relay address"
            );
        }
    }
}
