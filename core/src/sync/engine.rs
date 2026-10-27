//! The sync loop: connect, catch up, publish, reconnect, repeat.
//!
//! the initial implementation proved two cores could sync through a real relay, but the loop that
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
//!   between them ([ADR 0017](../../../docs/decisions.md#adr-0017)).
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
use crate::note::{NoteCore, Outgoing, SyncCursor};
use crate::relay_access::RelayAccess;
use crate::store::{Keyring, OpStore};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::Poll;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot, watch};

/// How long to wait before the first reconnection attempt.
pub const FIRST_RETRY: Duration = Duration::from_secs(1);

/// The ceiling on the backoff. A phone that has been in a tunnel for an hour
/// should come back within seconds of leaving it, so this stays small.
pub const MAX_RETRY: Duration = Duration::from_secs(60);

/// When an edit made with [`SyncHandle::publish_soon`] goes to the relay.
///
/// Typing is held back and sent as one envelope per burst rather than one per
/// keystroke; [ADR 0016](../../../docs/decisions.md#adr-0016) has the numbers
/// and the argument. Everything else — a tick, an item added or moved — goes the
/// moment it is made, because somebody on the other end of a shared list is
/// waiting to see it.
///
/// There is deliberately no minimum spacing between automatic batches. A long
/// enough pause is the only thing that sends one, so spacing on top of it would
/// turn a person's own rhythm into bursts, which is what the issue this came
/// from reported as "every few seconds".
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
            quiet: Duration::from_secs(6),
            max_hold: Duration::from_secs(20),
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
    /// The private account stream, when this device has one. It must settle
    /// before note outboxes are published on a reconnect.
    fn account_topic(&self) -> Result<Option<NoteId>> {
        Ok(None)
    }

    fn is_deleted(&self, note: NoteId) -> Result<bool> {
        let _ = note;
        Ok(false)
    }

    /// Whether the relay has said this topic was purged
    /// ([ADR 0026](../../../docs/decisions.md#adr-0026)). Such a
    /// topic is never subscribed again.
    fn is_gone(&self, note: NoteId) -> Result<bool> {
        let _ = note;
        Ok(false)
    }

    /// The relay says this topic was purged. Remember it, so no later
    /// connection asks again; keep everything local.
    fn gone(&self, note: NoteId) -> Result<()> {
        let _ = note;
        Ok(())
    }

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
    /// message that says the backlog is drained; see [ADR 0013](../../../docs/decisions.md#adr-0013) decision 5. Afterwards
    /// every arriving envelope is its own settled batch, and this is called for
    /// each — a live edit is one edit, from a device that was up to date.
    ///
    /// It must therefore be cheap and idempotent: it runs per envelope for as
    /// long as a connection is up.
    ///
    /// For a note it is when the note's access packages are acted on — a new
    /// key switched to, a removal carried out, an owner's change published —
    /// and what comes back is sent at once ([ADR 0023](../../../docs/decisions.md#adr-0023)–7). Nothing, by
    /// default: a note without packages means the same thing whether its
    /// envelopes arrived in a backlog or one at a time.
    fn settled(&self, note: NoteId) -> Result<Outgoing> {
        let _ = note;
        Ok(Outgoing::default())
    }

    /// A base for the relay to compact this note's log to, covering everything
    /// up to `covers` — see [`NoteCore::compaction`]. `None` when this device
    /// cannot vouch for all of it, which is the default: a peer that never
    /// compacts only ever costs the relay space.
    fn compaction(&self, note: NoteId, covers: i64) -> Result<Option<crate::compaction::Request>> {
        let _ = (note, covers);
        Ok(None)
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

    fn sign_challenge(
        &self,
        nonce: &[u8; crate::crypto::RELAY_AUTH_NONCE_LEN],
    ) -> Result<[u8; crate::crypto::SIG_LEN]> {
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
    fn account_topic(&self) -> Result<Option<NoteId>> {
        Ok(self.lock().map_err(|_| poisoned())?.account_id())
    }

    fn is_deleted(&self, note: NoteId) -> Result<bool> {
        let core = self.lock().map_err(|_| poisoned())?;
        Ok(core.is_deleted_note(note))
    }

    fn is_gone(&self, note: NoteId) -> Result<bool> {
        self.lock().map_err(|_| poisoned())?.is_gone(note)
    }

    fn gone(&self, note: NoteId) -> Result<()> {
        self.lock().map_err(|_| poisoned())?.mark_gone(note)
    }

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

    fn settled(&self, note: NoteId) -> Result<Outgoing> {
        let mut core = self.lock().map_err(|_| poisoned())?;
        if core.account_id() == Some(note) {
            core.adopt_note_keys()?;
            return Ok(Outgoing::default());
        }
        core.settle_note(note)
    }

    fn compaction(&self, note: NoteId, covers: i64) -> Result<Option<crate::compaction::Request>> {
        self.lock()
            .map_err(|_| poisoned())?
            .compaction(note, covers)
    }
}

// ---------------------------------------------------------------------------
// Where the relay is
// ---------------------------------------------------------------------------

/// One relay, as two base URLs.
///
/// The handshake is HTTP and the notes are WebSocket, and a user should not have
/// to know that: they give one address and this derives the other.
///
/// `http_base` is also the relay's *canonical* address: two spellings of one
/// relay give the same one, and two relays never do. The app keeps a separate
/// profile per relay, looked up by that address, so this and the app's
/// `canonicalRelayUrl` must agree about it exactly. Both are held to the cases
/// in `core/tests/fixtures/relay_addresses.json`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RelayEndpoint {
    pub http_base: String,
    pub ws_base: String,
    /// The relay's access key, if its operator gave this device one ([ADR 0028](../../../docs/decisions.md#adr-0028)). Not part
    /// of the relay's address: changing it is the same relay.
    pub access: Option<RelayAccess>,
}

impl RelayEndpoint {
    /// Accept either scheme and derive the other. `https` maps to `wss`, so a
    /// relay reached over TLS cannot be silently downgraded by the pairing.
    ///
    /// The scheme and host are lowercased, a port that is the scheme's default
    /// is dropped, and so are trailing slashes; the rest of a path is kept, so
    /// relays behind one host at different paths stay different relays.
    /// Credentials, a query and a fragment are refused rather than dropped:
    /// each would mean the address is not what the person thinks it is.
    /// So are `%` escapes and `.` or `..` path segments, which the app would
    /// otherwise resolve differently from this.
    pub fn parse(url: &str) -> Result<Self> {
        let url = url.trim();
        let bad = || CoreError::Storage(format!("not a sync server address: {url:?}"));
        let (scheme, rest) = url.split_once("://").ok_or_else(bad)?;
        let (tls, default_port) = match scheme.to_ascii_lowercase().as_str() {
            "http" | "ws" => (false, 80),
            "https" | "wss" => (true, 443),
            _ => return Err(bad()),
        };
        // `%` escapes and dot segments are refused rather than resolved: the
        // app's `Uri` would rewrite them, and then the two would disagree.
        if rest.contains(['?', '#', '%']) || rest.chars().any(char::is_whitespace) {
            return Err(bad());
        }
        let (authority, path) = match rest.find('/') {
            Some(slash) => rest.split_at(slash),
            None => (rest, ""),
        };
        if path
            .split('/')
            .any(|segment| segment == "." || segment == "..")
        {
            return Err(bad());
        }
        if authority.contains('@') {
            return Err(bad());
        }
        // A bracketed IPv6 literal has colons of its own, so the port is
        // whatever follows the closing bracket.
        let (host, port) = if let Some(literal) = authority.strip_prefix('[') {
            let (inside, after) = literal.split_once(']').ok_or_else(bad)?;
            let port = match after {
                "" => None,
                _ => Some(after.strip_prefix(':').ok_or_else(bad)?),
            };
            (format!("[{}]", inside.to_ascii_lowercase()), port)
        } else {
            match authority.split_once(':') {
                Some((host, port)) => (host.to_ascii_lowercase(), Some(port)),
                None => (authority.to_ascii_lowercase(), None),
            }
        };
        if host.is_empty() || host == "[]" {
            return Err(bad());
        }
        let port = match port {
            None | Some("") => None,
            Some(digits) => {
                let port: u16 = digits
                    .parse()
                    .ok()
                    .filter(|p| *p != 0 && digits.bytes().all(|b| b.is_ascii_digit()))
                    .ok_or_else(bad)?;
                (port != default_port).then_some(port)
            }
        };
        let path = path.trim_end_matches('/');
        let address = match port {
            Some(port) => format!("{host}:{port}{path}"),
            None => format!("{host}{path}"),
        };
        let (http, ws) = if tls {
            (format!("https://{address}"), format!("wss://{address}"))
        } else {
            (format!("http://{address}"), format!("ws://{address}"))
        };
        Ok(Self {
            http_base: http,
            ws_base: ws,
            access: None,
        })
    }

    /// The same relay, proving `access` at the handshake.
    pub fn with_access(self, access: Option<RelayAccess>) -> Self {
        Self { access, ..self }
    }
}

// ---------------------------------------------------------------------------
// What the UI sees
// ---------------------------------------------------------------------------

/// Why a device is offline after the relay's write budget refused it.
const THROTTLED: &str = "The relay is taking no more changes from this network for a moment. \
     Yours are safe on this device and will sync shortly.";

/// A note's sync status once its owner has deleted it from the relay.
pub const GONE_NOTE: &str =
    "This list was deleted by its owner. It no longer syncs; what is on this device stays.";

/// The account document's, once the account was deleted on another device.
pub const GONE_ACCOUNT: &str = "This account was deleted on another device. \
     Notes here no longer sync; remove the account from Your devices to delete them here too.";

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
    /// Monotonic count of this engine's local envelopes acknowledged by echo.
    /// Used as a refresh watermark so edits made after refresh starts do not
    /// extend its wait.
    pub acknowledged: u64,
    /// Automatic typing batches handed to the transport by this engine.
    pub typing_batches: u64,
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
            acknowledged: 0,
            typing_batches: 0,
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
    /// Act on the note's access packages now, if it is caught up: an owner's
    /// change has been saved and should not wait for the next arrival.
    Settle {
        note: NoteId,
    },
    /// Compact the note as if the relay had asked: this device removed an
    /// attachment, and the base's live list is what frees its bytes
    /// ([ADR 0027](../../../docs/decisions.md#adr-0027)).
    Reclaim {
        note: NoteId,
    },
    /// One note's held typing, or every note's.
    Flush {
        note: Option<NoteId>,
    },
    Refresh {
        id: String,
        complete: oneshot::Sender<std::result::Result<(), String>>,
    },
    SetForeground(bool),
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

    /// Request the relay's current heads for all active subscriptions.
    pub async fn refresh(&self, id: String) -> std::result::Result<(), String> {
        let (complete, result) = oneshot::channel();
        self.commands
            .send(Command::Refresh { id, complete })
            .map_err(|_| "sync engine stopped".to_string())?;
        result
            .await
            .map_err(|_| "sync engine stopped".to_string())?
    }

    pub fn set_foreground(&self, foreground: bool) {
        let _ = self.commands.send(Command::SetForeground(foreground));
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
    /// Request a relay refresh barrier for this subscription and wait until the
    /// relay has delivered through its captured head.
    pub async fn refresh(&self, id: String) -> std::result::Result<(), String> {
        let (complete, result) = oneshot::channel();
        self.commands
            .send(Command::Refresh { id, complete })
            .map_err(|_| "sync engine stopped".to_string())?;
        result
            .await
            .map_err(|_| "sync engine stopped".to_string())?
    }

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

    /// Ask the engine to settle the note — see [`SyncPeer::settled`] — if it
    /// is caught up with the relay. If it is not, it will be as soon as it is,
    /// and settles then.
    pub fn settle(&self) {
        let _ = self.commands.send(Command::Settle { note: self.note });
    }

    /// Hand the relay a base for this note soon, as if it had asked for one —
    /// once the edits this device has sent are acknowledged, as any base
    /// waits for. What removing an attachment calls: the base's live list is
    /// what lets the relay free the bytes, and it is also what takes the
    /// removed file's key out of the relay's log
    /// ([ADR 0027](../../../docs/decisions.md#adr-0027)).
    ///
    /// Forgotten if the connection drops first, like the relay's own request:
    /// the next compaction the note gets does the same.
    pub fn reclaim(&self) {
        let _ = self.commands.send(Command::Reclaim { note: self.note });
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
/// to it ([ADR 0017](../../../docs/decisions.md#adr-0017)).
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
            foreground: true,
            pacing,
            last_error: None,
            refreshes: HashMap::new(),
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
            let connected_at = super::timer::now();
            let mut server_retry_after = None;
            let retry_ceiling;
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
                    if super::timer::now().saturating_sub(connected_at) >= Duration::from_secs(30) {
                        backoff = FIRST_RETRY;
                        retry_ceiling = FIRST_RETRY;
                    } else {
                        retry_ceiling = backoff;
                        backoff = (backoff * 2).min(MAX_RETRY);
                    }
                }
                Ended::Connected { reason, .. } | Ended::Failed(reason) => {
                    device.fail(reason);
                    retry_ceiling = backoff;
                    backoff = (backoff * 2).min(MAX_RETRY);
                }
                Ended::Throttled(delay) => {
                    device.fail(THROTTLED.into());
                    retry_ceiling = backoff;
                    backoff = (backoff * 2).min(MAX_RETRY);
                    server_retry_after = Some(delay);
                }
            }
            let delay = server_retry_after.unwrap_or_else(|| randomized_retry(retry_ceiling));
            if !device.foreground {
                if !wait(None, &mut commands, &mut device).await {
                    break;
                }
                continue;
            }
            if !wait(Some(delay), &mut commands, &mut device).await {
                break;
            }
        }
        device.each(|note| note.set_state(SyncState::Offline));
    }
}

/// Jitter the common exponential ceiling so devices do not retry in lockstep.
fn randomized_retry(ceiling: Duration) -> Duration {
    use rand::RngExt;
    let max_ms = ceiling.as_millis().min(u64::MAX as u128) as u64;
    let min_ms = (max_ms / 2).max(1);
    Duration::from_millis(rand::rng().random_range(min_ms..=max_ms.max(min_ms)))
}

enum Ended {
    /// The engine was asked to stop, or nothing can reach it any more.
    ShutDown,
    /// We were connected and lost it. `caught` says whether any note got as far
    /// as being up to date, which is the difference between a connection that
    /// worked and one that was accepted and then fell over.
    Connected { caught: bool, reason: String },
    /// The relay specified a delay after refusing a write.
    Throttled(Duration),
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
    /// The relay will take no more writes from here for a while. Reconnect
    /// later rather than sooner: the outbox goes again then.
    Throttled(Duration),
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
    let token = match authenticate(&endpoint.http_base, endpoint.access.as_ref(), peer).await {
        Ok(token) => token,
        Err(e) => return Ended::Failed(e.to_string()),
    };

    let mut sync = match DeviceSync::connect(&endpoint.ws_base, &token).await {
        Ok(sync) => sync,
        Err(e) => return Ended::Failed(e.to_string()),
    };

    // Catch up the account first. Its deletion markers decide which note
    // outboxes still exist, so publishing those before this barrier could send
    // edits for a note another device already deleted.
    let account_topic = match peer.account_topic() {
        Ok(topic) => topic.filter(|id| device.notes.contains_key(id)),
        Err(e) => return Ended::Failed(e.to_string()),
    };
    if let Some(account) = account_topic {
        if let Err(e) = device.start(peer, account, &mut sync).await {
            return Ended::Failed(e);
        }
        loop {
            match sync.next().await {
                Ok(Some(Routed::Note(id, incoming))) if id == account => {
                    match device.receive(peer, &me, id, incoming) {
                        Ok(Received::Caught) => {
                            if let Err(e) = device.send_urgent(&mut sync).await {
                                return Ended::Failed(e);
                            }
                            break;
                        }
                        Ok(Received::Reset) => {
                            let _ = sync.close().await;
                            return Ended::Connected {
                                caught: true,
                                reason: "the sync server was restored; rebuilding its copy".into(),
                            };
                        }
                        Ok(Received::Throttled(delay)) => {
                            let _ = sync.close().await;
                            return Ended::Throttled(delay);
                        }
                        Ok(Received::Continue) => {}
                        Err(e) => return Ended::Failed(e),
                    }
                }
                Ok(Some(_)) => {}
                Ok(None) => return Ended::Failed("the sync server closed the connection".into()),
                Err(e) => return Ended::Failed(e.to_string()),
            }
        }
    }

    let mut caught = false;
    let lost = |caught: bool, reason: String| Ended::Connected { caught, reason };

    // Every note from its own cursor, each with its own outbox folded.
    let ids: Vec<NoteId> = device
        .notes
        .keys()
        .copied()
        .filter(|id| Some(*id) != account_topic)
        .collect();
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
                if let Err(e) = device.flush(peer, id, &mut sync, true).await {
                    return lost(caught, e);
                }
            },

            // Cancel-safe: the socket buffers a partial frame internally, so
            // dropping this future when another branch wins loses nothing.
            incoming = sync.next() => match incoming {
                Ok(Some(Routed::Note(id, incoming))) => {
                    let received = device.receive(peer, &me, id, incoming);
                    if let Err(e) = device.send_urgent(&mut sync).await {
                        return lost(caught, e);
                    }
                    match received {
                        Ok(Received::Continue) => {}
                        Ok(Received::Caught) => caught = true,
                        Ok(Received::Reset) => {
                            let _ = sync.close().await;
                            return lost(true, "the sync server was restored; rebuilding its copy".into());
                        }
                        // Not caught, whatever was: that is what makes the
                        // engine back off rather than come straight back.
                        Ok(Received::Throttled(delay)) => {
                            let _ = sync.close().await;
                            return Ended::Throttled(delay);
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
                Ok(Some(Routed::Refresh { id, note, .. })) => {
                    device.refresh_complete(&id, note);
                }
                Ok(None) => return lost(caught, "the sync server closed the connection".into()),
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
                // Offline: the next connection's `Caught` settles it.
                Some(Command::Settle { .. }) => {}
                Some(Command::Reclaim { .. }) => {}
                Some(Command::Flush { .. }) => {}
                Some(Command::Refresh { complete, .. }) => {
                    let _ = complete.send(Err("the sync server is not connected".into()));
                }
                Some(Command::SetForeground(foreground)) => {
                    device.foreground = foreground;
                    if foreground {
                        return true;
                    }
                }
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
/// A refresh in progress: the notes it is waiting for, and the channel that
/// completes when the last one arrives.
type Refresh = (
    HashSet<NoteId>,
    oneshot::Sender<std::result::Result<(), String>>,
);

struct Device {
    notes: HashMap<NoteId, Live>,
    refreshes: HashMap<String, Refresh>,
    /// Every [`SyncDevice`] has been dropped, so no note can be connected
    /// again: once the last one goes, so does the engine.
    released: bool,
    foreground: bool,
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
        for (_, (_, complete)) in self.refreshes.drain() {
            let _ = complete.send(Err(reason.clone()));
        }
    }

    fn refresh_complete(&mut self, id: &str, note: NoteId) {
        let Some((remaining, _complete)) = self.refreshes.get_mut(id) else {
            return;
        };
        remaining.remove(&note);
        if remaining.is_empty() {
            let (_, complete) = self.refreshes.remove(id).expect("refresh remains");
            let _ = complete.send(Ok(()));
        }
    }

    fn refresh_drop_note(&mut self, note: NoteId) {
        let ids: Vec<_> = self.refreshes.keys().cloned().collect();
        for id in ids {
            self.refresh_complete(&id, note);
        }
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

    /// Put one note on a live connection, subscribing from its saved cursor.
    /// Its outbox is rebuilt and sent only after access has settled at Caught.
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
        if peer.is_deleted(id).map_err(|e| e.to_string())? {
            if let Some(mut live) = self.notes.remove(&id) {
                live.set_state(SyncState::Offline);
            }
            return Ok(());
        }
        let gone = peer.is_gone(id).map_err(|e| e.to_string())?;
        let account = peer.account_topic().map_err(|e| e.to_string())? == Some(id);
        let Some(live) = self.notes.get_mut(&id) else {
            return Ok(());
        };
        if gone {
            live.gone(account);
            return Ok(());
        }
        live.on_socket = false;
        live.caught = false;
        live.crowded = false;
        live.compact = None;
        live.compacting = None;

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

        // The backlog may revoke our key. Keep edits local until Caught has
        // settled access, then rebuild the outbox under the resulting key.
        live.covered = None;
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
                    self.refresh_drop_note(note);
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
                // A key rotation also supersedes commands still carrying the
                // previous key, even when only an owner-signed snapshot remains.
                if live.covers(&envelope) {
                    return Ok(Next::Continue);
                }
                live.hold(envelope);
                if now {
                    self.flush(peer, note, sync, false).await?;
                }
            }
            Command::Settle { note } => {
                let Some(live) = self.notes.get_mut(&note) else {
                    return Ok(Next::Continue);
                };
                if live.caught {
                    settle(peer, note, live);
                    self.send_urgent(sync).await?;
                }
            }
            Command::Reclaim { note } => {
                let Some(live) = self.notes.get_mut(&note) else {
                    return Ok(Next::Continue);
                };
                live.crowded = true;
                live.compact_if_due(peer, note);
                self.send_urgent(sync).await?;
            }
            Command::Flush { note: Some(note) } => self.flush(peer, note, sync, false).await?,
            Command::Flush { note: None } => {
                let ids: Vec<NoteId> = self.notes.keys().copied().collect();
                for id in ids {
                    self.flush(peer, id, sync, false).await?;
                }
            }
            Command::Refresh { id, complete } => {
                if self.refreshes.contains_key(&id) {
                    let _ = complete.send(Err("a refresh is already in progress".into()));
                    return Ok(Next::Continue);
                }
                let remaining: HashSet<_> = self
                    .notes
                    .iter()
                    .filter_map(|(note, live)| live.on_socket.then_some(*note))
                    .collect();
                if remaining.is_empty() {
                    let _ = complete.send(Ok(()));
                } else {
                    sync.refresh(&id).await.map_err(|e| e.to_string())?;
                    self.refreshes.insert(id, (remaining, complete));
                }
            }
            Command::SetForeground(foreground) => self.foreground = foreground,
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
        automatic: bool,
    ) -> std::result::Result<(), String> {
        let Some(live) = self.notes.get_mut(&id) else {
            return Ok(());
        };
        // Catch-up may still change access. Leave edits in the log until the
        // barrier rebuilds the outbox; stop timers so they cannot spin here.
        if !live.on_socket || !live.caught {
            live.quiet = None;
            live.cap = None;
            return Ok(());
        }
        let held = live.release();
        if held.is_empty() {
            return Ok(());
        }
        let batch = live.squash(peer, id, held);
        if automatic {
            live.current.typing_batches = live.current.typing_batches.saturating_add(1);
        }
        live.outbox.extend(batch.iter().cloned());
        live.publish();
        for envelope in &batch {
            sync.append(id, envelope).await.map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    /// Send what settling any note said to send, and any base a note has
    /// made for the relay.
    async fn send_urgent(&mut self, sync: &mut DeviceSync) -> std::result::Result<(), String> {
        for (id, live) in self.notes.iter_mut() {
            if !live.on_socket || !live.caught {
                continue;
            }
            if let Some(request) = live.compact.take() {
                sync.compact(*id, &request)
                    .await
                    .map_err(|e| e.to_string())?;
            }
            if live.urgent.is_empty() {
                continue;
            }
            let urgent = std::mem::take(&mut live.urgent);
            for envelope in &urgent {
                if !live.outbox.contains(envelope) {
                    live.outbox.push(envelope.clone());
                }
            }
            live.publish();
            for envelope in &urgent {
                sync.append(*id, envelope)
                    .await
                    .map_err(|e| e.to_string())?;
            }
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
                // The echo that empties the outbox may be what a waiting
                // compaction was waiting for. After settling, which may have
                // something of its own to send first.
                live.compact_if_due(peer, id);
                Ok(Received::Continue)
            }
            Incoming::Caught { seq } => {
                live.caught = true;
                live.advance(peer, id, seq);
                // After the cursor has moved, so that a peer that does work
                // here and then fails does not also lose its place in the log
                // it has already applied.
                // A failed access transition must not release old-key edits.
                let out = peer.settled(id).map_err(|e| e.to_string())?;
                live.take(out);
                if live.on_socket {
                    live.rebuild_outbox(peer, id)?;
                    live.urgent = live.outbox.clone();
                    live.set_state(SyncState::Synced);
                }
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
            Incoming::RateLimited { retry_after } => Ok(Received::Throttled(retry_after)),
            Incoming::Crowded => {
                live.crowded = true;
                live.compact_if_due(peer, id);
                Ok(Received::Continue)
            }
            Incoming::Gone => {
                // Recorded before anything else, so that a failure to record
                // it is a failure to stop, not a quiet resubscribe next time.
                peer.gone(id).map_err(|e| e.to_string())?;
                let account = peer.account_topic().map_err(|e| e.to_string())? == Some(id);
                live.gone(account);
                Ok(Received::Continue)
            }
            // Not the person's problem: the log is as it was, and whoever
            // compacts it next will. This device does not try again on this
            // connection, so a relay that keeps refusing does not cost it a
            // snapshot per edit.
            Incoming::CompactRefused(_) => Ok(Received::Continue),
        }
    }
}

/// Tell the peer the batch has settled, and keep going if it complains.
///
/// A peer that cannot finish its end-of-batch work has not invalidated anything
/// the connection has already applied, so this is a line for the UI rather than
/// a reason to drop a socket.
fn settle<P: SyncPeer>(peer: &P, note: NoteId, live: &mut Live) {
    match peer.settled(note) {
        Ok(out) => live.take(out),
        Err(e) => live.note_error(e.to_string()),
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
    /// After a rotation, commands already queued by the UI may still carry
    /// the old key. Their content is in the replacement snapshot/update.
    publish_key: Option<[u8; crate::crypto::KEY_ID_LEN]>,
    /// What settling the note said to send, waiting for the socket.
    urgent: Vec<Vec<u8>>,
    /// The relay has asked for a compaction on this connection, and this
    /// device has not yet made one.
    crowded: bool,
    /// A base made for the relay, waiting for the socket.
    compact: Option<crate::compaction::Request>,
    /// The base sent on this connection, until the relay echoes it back. Set,
    /// it stops another being made; left set by a refusal, it stops this
    /// connection trying again.
    compacting: Option<Vec<u8>>,
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
            publish_key: None,
            urgent: Vec::new(),
            crowded: false,
            compact: None,
            compacting: None,
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
        self.crowded = false;
        self.compact = None;
        self.compacting = None;
        // Nothing here is lost with it: every envelope settling hands out is in
        // the log, and the next connection's catch-up reads it from there.
        self.urgent.clear();
        self.current.last_error = Some(reason);
        self.set_state(SyncState::Offline);
    }

    /// Take what settling the note said to send. A new key means everything
    /// held back and queued under the old one is superseded: its content is in
    /// the state the new key opens, and sending it would hand it to whoever the
    /// key change removed.
    fn take(&mut self, out: Outgoing) {
        if out.removed {
            self.on_socket = false;
            self.release();
            self.outbox.clear();
            self.urgent.clear();
            self.set_state(SyncState::Offline);
            return;
        }
        if out.rekey {
            self.release();
            self.outbox.clear();
            self.covered = None;
            self.publish_key = out
                .envelopes
                .iter()
                .find_map(|raw| Envelope::decode(raw).ok().and_then(|env| env.key_id));
        }
        self.urgent.extend(out.envelopes);
        self.publish();
    }

    /// The relay purged this topic. Nothing more goes to it, and the person is
    /// told why; the local copy is untouched.
    fn gone(&mut self, account: bool) {
        self.on_socket = false;
        self.caught = false;
        self.crowded = false;
        self.compact = None;
        self.compacting = None;
        self.release();
        self.outbox.clear();
        self.urgent.clear();
        self.current.last_error = Some(if account {
            GONE_ACCOUNT.to_string()
        } else {
            GONE_NOTE.to_string()
        });
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
        let quiet = self.pacing.quiet.min(self.pacing.max_hold);
        self.quiet = Some(super::timer::timer(quiet));
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
        Envelope::decode(envelope).is_ok_and(|e| {
            self.covered.is_some_and(|covered| e.counter <= covered)
                || self.publish_key.is_some_and(|key| e.key_id != Some(key))
        })
    }

    /// Read only after access has settled: a key rotation may replace the log.
    /// Commands queued during catch-up are already represented in these rows.
    fn rebuild_outbox<P: SyncPeer>(
        &mut self,
        peer: &P,
        note: NoteId,
    ) -> std::result::Result<(), String> {
        let unsent = peer
            .unsent(note, self.cursor.sent_upto)
            .map_err(|e| e.to_string())?;
        let me = peer.device_key().map_err(|e| e.to_string())?;
        self.covered = unsent
            .iter()
            .filter(|(_, raw)| Envelope::decode(raw).is_ok_and(|env| env.author == me))
            .map(|(counter, _)| *counter)
            .max()
            .max(self.cursor.sent_upto);
        self.release();
        self.outbox = self.squash(peer, note, unsent.into_iter().map(|(_, raw)| raw).collect());
        self.publish();
        Ok(())
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
            if matches!(e, CoreError::AccessPending) {
                // Verified and kept: this device has no key for the note yet,
                // and the package that grants one will make it readable. The
                // cursor may move past it.
            } else {
                // A rejected envelope is a fact about that envelope, not a reason to
                // stop or to ask for it again — hence the cursor moving on below. The
                // core has already refused to let it near the document.
                self.current.last_error = Some(e.to_string());
            }
        }
        if self.compacting.as_deref() == Some(envelope) {
            // The relay took the base: another may be made when it next asks.
            self.compacting = None;
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

    /// Make a base for the relay if it asked for one and this device can vouch
    /// for everything up to its cursor: caught up, and with nothing of its own
    /// in flight or held back, so that the relay has every edit this device
    /// made and the base has every edit the relay has
    /// ([ADR 0024](../../../docs/decisions.md#adr-0024)).
    fn compact_if_due<P: SyncPeer>(&mut self, peer: &P, note: NoteId) {
        if !self.crowded
            || !self.caught
            || self.compacting.is_some()
            || !self.outbox.is_empty()
            || !self.held.is_empty()
            || !self.urgent.is_empty()
        {
            return;
        }
        self.crowded = false;
        match peer.compaction(note, self.cursor.last_seq) {
            Ok(Some(request)) => {
                self.compacting = Some(request.base.clone());
                self.compact = Some(request);
            }
            Ok(None) => {}
            Err(e) => self.note_error(e.to_string()),
        }
    }

    /// The relay has our envelope: drop it from the outbox and remember how far
    /// the acknowledgement reaches.
    fn acknowledge(&mut self, counter: u64, envelope: &[u8]) {
        let before = self.outbox.len();
        self.outbox.retain(|queued| queued != envelope);
        if self.outbox.len() != before {
            self.current.acknowledged = self.current.acknowledged.saturating_add(1);
        }
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
    fn retry_jitter_stays_between_half_and_full_ceiling() {
        for ceiling in [1, 2, 4, 8, 16, 32, 60].map(Duration::from_secs) {
            for _ in 0..100 {
                let delay = randomized_retry(ceiling);
                assert!(delay >= ceiling / 2);
                assert!(delay <= ceiling);
            }
        }
    }

    #[tokio::test]
    async fn a_hidden_engine_stays_parked_until_foreground_resumes() {
        let (commands, mut receiver) = mpsc::unbounded_channel();
        let mut device = Device {
            notes: HashMap::new(),
            released: false,
            foreground: false,
            pacing: Pacing::default(),
            last_error: None,
            refreshes: HashMap::new(),
        };
        let parked = wait(None, &mut receiver, &mut device);
        tokio::pin!(parked);
        assert!(tokio::time::timeout(Duration::from_millis(10), &mut parked)
            .await
            .is_err());

        commands.send(Command::SetForeground(true)).unwrap();
        assert!(parked.await);
    }

    #[test]
    fn local_acknowledgement_watermark_advances_once_per_echoed_envelope() {
        let (status, _watcher) = watch::channel(SyncStatus::default());
        let mut live = Live::new(1, status, Pacing::default());
        let envelope = vec![1, 2, 3];
        live.outbox.push(envelope.clone());

        live.acknowledge(1, &envelope);
        assert_eq!(live.current.acknowledged, 1);
        live.acknowledge(1, &envelope);
        assert_eq!(live.current.acknowledged, 1);
    }

    #[test]
    fn typing_defaults_wait_for_a_six_second_pause_and_never_more_than_twenty_seconds() {
        assert_eq!(Pacing::default().quiet, Duration::from_secs(6));
        assert_eq!(Pacing::default().max_hold, Duration::from_secs(20));
    }

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
