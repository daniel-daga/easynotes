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

// From the module rather than from one transport: which of `client` and
// `client_web` these come from is the target's business, and this file must not
// be able to tell.
use super::wire::Incoming;
use super::{authenticate, NoteSync};
use crate::crdt::NoteDoc;
use crate::crypto::{AuthorKey, DeviceSigner, Envelope};
use crate::error::{CoreError, Result};
use crate::ids::NoteId;
use crate::note::{NoteCore, SyncCursor};
use crate::store::{Keyring, OpStore};
use std::sync::{Arc, Mutex};
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
    /// `now` is false for an edit that may wait for the ones after it.
    Publish {
        envelope: Vec<u8>,
        now: bool,
    },
    Flush,
    Shutdown,
}

/// The engine's remote control: hand it envelopes, read its status, stop it.
///
/// Cloneable and cheap. Dropping every clone stops the engine, so a note that
/// goes away cannot leave a task talking to the relay about it.
#[derive(Clone)]
pub struct SyncHandle {
    commands: mpsc::UnboundedSender<Command>,
    status: watch::Receiver<SyncStatus>,
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
            envelope,
            now: false,
        });
    }

    /// Send whatever is being held back, now. For the moments the app may not
    /// get another chance to: going into the background, a tab being hidden, a
    /// note being closed.
    pub fn flush(&self) {
        let _ = self.commands.send(Command::Flush);
    }

    pub fn status(&self) -> SyncStatus {
        self.status.borrow().clone()
    }

    /// The current status and every change to it.
    pub fn watch_status(&self) -> watch::Receiver<SyncStatus> {
        self.status.clone()
    }

    /// Ask the engine to close its connection and finish.
    pub fn shutdown(&self) {
        let _ = self.commands.send(Command::Shutdown);
    }
}

// ---------------------------------------------------------------------------
// The engine
// ---------------------------------------------------------------------------

pub struct SyncEngine<P: SyncPeer> {
    peer: P,
    note: NoteId,
    endpoint: RelayEndpoint,
    commands: mpsc::UnboundedReceiver<Command>,
    live: Live,
}

impl<P: SyncPeer> SyncEngine<P> {
    /// Build an engine and its handle. Nothing happens until [`run`](Self::run)
    /// is spawned — on whatever executor the caller has, which on the app side
    /// is a runtime the bridge owns and in tests is the test's own.
    pub fn start(peer: P, note: NoteId, endpoint: RelayEndpoint) -> (Self, SyncHandle) {
        Self::start_paced(peer, note, endpoint, Pacing::default())
    }

    /// [`start`](Self::start), holding typing back for other than the default
    /// times. For tests, which cannot wait ten seconds to see a cap work.
    pub fn start_paced(
        peer: P,
        note: NoteId,
        endpoint: RelayEndpoint,
        pacing: Pacing,
    ) -> (Self, SyncHandle) {
        let (tx, rx) = mpsc::unbounded_channel();
        let (status_tx, status_rx) = watch::channel(SyncStatus::default());
        let engine = Self {
            peer,
            note,
            endpoint,
            commands: rx,
            live: Live {
                status: status_tx,
                current: SyncStatus::default(),
                outbox: Vec::new(),
                held: Vec::new(),
                covered: None,
                pacing,
                quiet: None,
                cap: None,
                cursor: SyncCursor::default(),
                saved: SyncCursor::default(),
            },
        };
        let handle = SyncHandle {
            commands: tx,
            status: status_rx,
        };
        (engine, handle)
    }

    /// Run until the handle is dropped or [`SyncHandle::shutdown`] is called.
    ///
    /// Never returns an error: a relay that cannot be reached is a state to show,
    /// not a failure to report, and the app keeps working without one.
    pub async fn run(self) {
        // Destructured so the borrow checker can see that the command channel and
        // the engine's state are separate things. `select!` holds both at once.
        let Self {
            peer,
            note,
            endpoint,
            mut commands,
            mut live,
        } = self;

        let me = match peer.device_key() {
            Ok(key) => key,
            Err(e) => {
                live.fail(e.to_string());
                return;
            }
        };

        let mut backoff = FIRST_RETRY;
        loop {
            live.set_state(SyncState::Connecting);
            match session(&peer, note, &endpoint, &me, &mut commands, &mut live).await {
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
                    live.fail(reason);
                    backoff = FIRST_RETRY;
                }
                Ended::Connected { reason, .. } | Ended::Failed(reason) => {
                    live.fail(reason);
                    backoff = (backoff * 2).min(MAX_RETRY);
                }
            }
            if !wait(backoff, &mut commands, &mut live).await {
                break;
            }
        }
        live.set_state(SyncState::Offline);
    }
}

enum Ended {
    /// The handle asked us to stop, or was dropped.
    ShutDown,
    /// We were connected and lost it. `caught` says whether we ever got as far
    /// as being up to date, which is the difference between a connection that
    /// worked and one that was accepted and then fell over.
    Connected { caught: bool, reason: String },
    /// We never got connected.
    Failed(String),
}

/// One connection, from handshake to whatever ends it.
async fn session<P: SyncPeer>(
    peer: &P,
    note: NoteId,
    endpoint: &RelayEndpoint,
    me: &AuthorKey,
    commands: &mut mpsc::UnboundedReceiver<Command>,
    live: &mut Live,
) -> Ended {
    // A token every time. They expire, and a reconnect after a long offline spell
    // is exactly the case where the old one would be stale.
    let token = match authenticate(&endpoint.http_base, peer).await {
        Ok(token) => token,
        Err(e) => return Ended::Failed(e.to_string()),
    };

    // Re-read rather than trust what we remember: the core has been editing and
    // persisting while we were away.
    match peer.cursor(note) {
        Ok(cursor) => {
            live.cursor = cursor;
            live.saved = cursor;
        }
        Err(e) => return Ended::Failed(e.to_string()),
    }

    let mut sync =
        match NoteSync::connect(&endpoint.ws_base, &token, note, live.cursor.last_seq).await {
            Ok(sync) => sync,
            Err(e) => return Ended::Failed(e.to_string()),
        };
    live.connected();

    // Everything this device wrote that the relay has not acknowledged, including
    // edits from earlier runs of the app and whatever was held back while
    // offline — all of it is in the log, so the log is where it is read from,
    // and it replaces the outbox rather than joining it. Folded into as few
    // envelopes as will do: an hour of offline typing is one burst as far as
    // anyone else is concerned.
    match peer.unsent(note, live.cursor.sent_upto) {
        Ok(unsent) => {
            // A `Publish` still in the channel for anything read here is the
            // same edit arriving a second time. The counter says so, because
            // the log gets an edit before the channel does.
            live.covered = unsent.last().map(|(counter, _)| *counter);
            live.release();
            let unsent = unsent.into_iter().map(|(_, envelope)| envelope).collect();
            live.outbox = live.squash(peer, note, unsent);
        }
        Err(e) => {
            return Ended::Connected {
                caught: false,
                reason: e.to_string(),
            }
        }
    }
    live.publish();

    let mut caught = false;
    let lost = |caught: bool, reason: String| Ended::Connected { caught, reason };

    for envelope in live.outbox.clone() {
        if let Err(e) = sync.append(&envelope).await {
            return lost(caught, e.to_string());
        }
    }

    loop {
        tokio::select! {
            command = commands.recv() => match command {
                // Not flushed first. What is held is in the log, and the next
                // connection sends it; a shutdown is also how a note about to be
                // deleted is let go of, and rewriting its log on the way out
                // would race the delete.
                None | Some(Command::Shutdown) => {
                    let _ = sync.close().await;
                    return Ended::ShutDown;
                }
                Some(Command::Publish { envelope, now }) => {
                    // Already covered means already sent in this session: the
                    // edit landed in the log before the handshake finished, so
                    // the catch-up above picked it up and the command is the
                    // same envelope arriving a second time. Sending it again
                    // would put a duplicate in the relay's log on every launch
                    // where the user edits while it is still connecting.
                    if live.covers(&envelope) {
                        continue;
                    }
                    live.hold(envelope);
                    if now {
                        if let Err(e) = flush(peer, note, live, &mut sync).await {
                            return lost(caught, e);
                        }
                    }
                }
                Some(Command::Flush) => {
                    if let Err(e) = flush(peer, note, live, &mut sync).await {
                        return lost(caught, e);
                    }
                }
            },

            // Pending for ever while nothing is held, so this only ever wins
            // when a pause or the cap says the typing has waited long enough.
            () = due(&mut live.quiet, &mut live.cap) => {
                if let Err(e) = flush(peer, note, live, &mut sync).await {
                    return lost(caught, e);
                }
            },

            // Cancel-safe: the socket buffers a partial frame internally, so
            // dropping this future when the other branch wins loses nothing.
            incoming = sync.next() => match incoming {
                Ok(Some(Incoming::Envelope { seq, envelope })) => {
                    live.receive(peer, note, me, seq, &envelope);
                    // Once the backlog is drained, every envelope is its own
                    // settled batch: it is one edit, made by a device that was
                    // itself up to date, so the state it leaves the document in
                    // is a state that device meant. During the backlog it is not,
                    // which is the whole reason `settled` exists — hence the
                    // flag rather than calling this unconditionally.
                    if caught {
                        settle(peer, note, live);
                    }
                }
                Ok(Some(Incoming::Caught { seq })) => {
                    caught = true;
                    live.advance(peer, note, seq);
                    // After the cursor has moved, so that a peer that does work
                    // here and then fails does not also lose its place in the
                    // log it has already applied.
                    settle(peer, note, live);
                    live.set_state(SyncState::Synced);
                }
                Ok(Some(Incoming::Rejected(message))) => {
                    // The relay refused one envelope. The connection is fine and
                    // the rest of the outbox still goes.
                    live.note_error(message);
                }
                Ok(None) => return lost(caught, "the relay closed the connection".into()),
                Err(e) => return lost(caught, e.to_string()),
            },
        }
    }
}

/// Send everything held back, folded into as few envelopes as will do.
///
/// The error is the socket's, and ends the connection; nothing is lost with it,
/// because what was held is in the log and the next connection reads it there.
async fn flush<P: SyncPeer>(
    peer: &P,
    note: NoteId,
    live: &mut Live,
    sync: &mut NoteSync,
) -> std::result::Result<(), String> {
    let held = live.release();
    if held.is_empty() {
        return Ok(());
    }
    let batch = live.squash(peer, note, held);
    live.outbox.extend(batch.iter().cloned());
    live.publish();
    for envelope in &batch {
        sync.append(envelope).await.map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Whichever of the two batching timers goes off first, or never if neither is
/// running.
///
/// Cancel-safe, which `select!` needs: the timers are borrowed rather than
/// moved in, so dropping this when another branch wins leaves them running.
async fn due(quiet: &mut Option<super::timer::Timer>, cap: &mut Option<super::timer::Timer>) {
    match (quiet.as_mut(), cap.as_mut()) {
        (Some(quiet), Some(cap)) => {
            tokio::select! {
                () = quiet => {},
                () = cap => {},
            }
        }
        (Some(timer), None) | (None, Some(timer)) => timer.await,
        (None, None) => std::future::pending().await,
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

/// Sleep out the backoff, but stay listening: an edit made while offline belongs
/// in the outbox now, not after the next connection, or the UI would under-report
/// what is pending.
///
/// Returns false if the engine should stop.
async fn wait(
    backoff: Duration,
    commands: &mut mpsc::UnboundedReceiver<Command>,
    live: &mut Live,
) -> bool {
    // `super::timer` rather than `tokio::time`, because a browser tab has no
    // runtime to drive a tokio timer and cannot be given one. It is the only
    // line in this file that differs between a device and a tab.
    let deadline = super::timer::sleep(backoff);
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            _ = &mut deadline => return true,
            command = commands.recv() => match command {
                None | Some(Command::Shutdown) => return false,
                // Held with no timer on it: there is nowhere to send it, and the
                // next connection reads it back out of the log anyway. It is
                // here only so that `pending` counts it.
                Some(Command::Publish { envelope, .. }) => {
                    live.held.push(envelope);
                    live.publish();
                }
                Some(Command::Flush) => {}
            },
        }
    }
}

/// The engine's mutable state, and the only thing that touches the status channel.
struct Live {
    status: watch::Sender<SyncStatus>,
    current: SyncStatus,
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
    ) {
        if let Err(e) = peer.apply_remote(note, envelope) {
            // A rejected envelope is a fact about that envelope, not a reason to
            // stop or to ask for it again — hence the cursor moving on below. The
            // core has already refused to let it near the document.
            self.current.last_error = Some(e.to_string());
        }
        if let Ok(parsed) = Envelope::decode(envelope) {
            if &parsed.author == me {
                self.acknowledge(parsed.counter, envelope);
            }
        }
        self.advance(peer, note, seq);
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
