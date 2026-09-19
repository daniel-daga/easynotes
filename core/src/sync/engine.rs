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
        self.lock()
            .map_err(|_| poisoned())?
            .apply_remote(note, envelope)
            .map(|_| ())
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
    Publish(Vec<u8>),
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
    /// Queue a sealed envelope for the relay.
    ///
    /// Returns nothing and cannot fail: an edit is already in the log by the time
    /// it gets here, so the only question is when the relay hears about it, and
    /// the answer is "when there is a connection". A stopped engine drops it, and
    /// the next connection picks it up from the log.
    pub fn publish(&self, envelope: Vec<u8>) {
        let _ = self.commands.send(Command::Publish(envelope));
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
    // edits from earlier runs of the app. Queued items that are already here are
    // not added twice.
    match peer.unsent(note, live.cursor.sent_upto) {
        Ok(unsent) => {
            for (_, envelope) in unsent {
                if !live.outbox.contains(&envelope) {
                    live.outbox.push(envelope);
                }
            }
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
                None | Some(Command::Shutdown) => {
                    let _ = sync.close().await;
                    return Ended::ShutDown;
                }
                Some(Command::Publish(envelope)) => {
                    // Already in the outbox means already sent in this session:
                    // the edit landed in the log before the handshake finished,
                    // so the catch-up above picked it up and the command is the
                    // same envelope arriving a second time. Sending it again
                    // would put a duplicate in the relay's log on every launch
                    // where the user edits while it is still connecting.
                    if live.outbox.contains(&envelope) {
                        continue;
                    }
                    live.queue(envelope.clone());
                    if let Err(e) = sync.append(&envelope).await {
                        return lost(caught, e.to_string());
                    }
                }
            },

            // Cancel-safe: the socket buffers a partial frame internally, so
            // dropping this future when the other branch wins loses nothing.
            incoming = sync.next() => match incoming {
                Ok(Some(Incoming::Envelope { seq, envelope })) => {
                    live.receive(peer, note, me, seq, &envelope);
                }
                Ok(Some(Incoming::Caught { seq })) => {
                    caught = true;
                    live.advance(peer, note, seq);
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
                Some(Command::Publish(envelope)) => live.queue(envelope),
            },
        }
    }
}

/// The engine's mutable state, and the only thing that touches the status channel.
struct Live {
    status: watch::Sender<SyncStatus>,
    current: SyncStatus,
    /// Sealed envelopes the relay has not echoed back, in the order they were made.
    outbox: Vec<Vec<u8>>,
    cursor: SyncCursor,
    /// What the store holds, so a cursor that has not moved is not written again.
    saved: SyncCursor,
}

impl Live {
    fn publish(&mut self) {
        self.current.pending = self.outbox.len();
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

    fn queue(&mut self, envelope: Vec<u8>) {
        self.outbox.push(envelope);
        self.publish();
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
