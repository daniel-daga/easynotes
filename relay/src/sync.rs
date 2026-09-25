//! One socket per device: `/v1/sync`.
//!
//! A device opens one connection and does one token handshake, however many
//! notes it syncs, and every note is carried over it — see [ADR
//! 0017](../../docs/adr/0017-one-socket-per-device.md). Each note's stream
//! keeps three rules:
//!
//!   * subscribe to the fan-out *before* reading the backlog, so that nothing
//!     appended in between is lost;
//!   * deduplicate by `seq`, so that anything that arrives both ways is sent
//!     once;
//!   * replay from the store when a subscriber lags, rather than dropping it.
//!
//! And one note's backlog must not hold up the others. A socket is one ordered
//! stream, so a backlog is sent [`BACKLOG_CHUNK`] envelopes at a time, with the
//! socket's other work — live updates to other notes, appends, new
//! subscriptions — taken between chunks.
//!
//! And reading never waits for writing. Messages to the client go through a
//! bounded queue drained by a task of its own; when it is full, a live update is
//! left in the store and sent later as backlog, and a chunk waits for room. So
//! a client that is slow to read slows only what it is sent, never what it
//! sends — which matters because a device subscribing its notes on connecting
//! writes a great deal before it reads.
//!
//! **What the relay learns:** which notes one device key syncs, which it would
//! learn anyway from that key appending to them. It still cannot tell two of
//! one person's devices from two collaborators ([ADR
//! 0014](../../docs/adr/0014-what-the-relay-learns.md)).

use crate::app::{self, AppState};
use crate::error::{RelayError, Result};
use crate::protocol::*;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::response::Response;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use easynotes_core::ids::NoteId;
use futures_util::{SinkExt, StreamExt};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;

type Sink = futures_util::stream::SplitSink<WebSocket, Message>;

pub(crate) async fn sync_ws(
    State(state): State<Arc<AppState>>,
    Query(q): Query<app::WsQuery>,
    ws: WebSocketUpgrade,
) -> Result<Response> {
    if state.auth.device_for(&q.token).is_none() {
        return Err(RelayError::Unauthorized);
    }
    Ok(ws.on_upgrade(move |socket| async move {
        if let Err(e) = run(socket, state).await {
            tracing::debug!(error = %e, "device session ended");
        }
    }))
}

/// What a note's fan-out hands the session, tagged with the note.
enum Fanned {
    Update {
        note: String,
        seq: i64,
        envelope: Vec<u8>,
    },
    /// The note's fan-out dropped messages for this subscriber; read them back
    /// out of the store.
    Lagged { note: String },
}

struct Subscription {
    /// The highest `seq` sent to this socket for the note.
    last_sent: i64,
    /// True while the store still has envelopes past `last_sent` that have not
    /// been sent: the first backlog, or a replay after a lag. Live updates for
    /// the note are left to the store while it is, because sending one ahead
    /// of the backlog would move the client's cursor past envelopes it has not
    /// got.
    catching_up: bool,
    /// Whether `caught` has been said. Once: a replay after a lag is the
    /// relay's business and not a new backlog.
    caught: bool,
    /// Forwards the note's fan-out into the session. Aborted on unsubscribe.
    forwarder: JoinHandle<()>,
}

/// Aborts every forwarder however the session ends, a dropped socket included.
struct Subscriptions(HashMap<String, Subscription>);

impl Drop for Subscriptions {
    fn drop(&mut self) {
        for sub in self.0.values() {
            sub.forwarder.abort();
        }
    }
}

/// How many messages may wait for the socket. Past this, live updates are left
/// in the store and replayed, and backlog chunks wait, so a device that stops
/// reading costs the relay this much memory and no more.
const OUTGOING: usize = 4 * BACKLOG_CHUNK;

async fn run(socket: WebSocket, state: Arc<AppState>) -> anyhow::Result<()> {
    let (tx, mut rx) = socket.split();

    // Writing is a task of its own, so that reading never waits for it. A
    // session that stopped reading because its client stopped reading would
    // stop taking that client's own appends and subscriptions, and a client
    // that writes before it reads — twenty subscriptions on connecting, say,
    // while the first note's backlog comes back — would wait on the relay
    // while the relay waited on it.
    let (out, outgoing) = mpsc::channel::<SyncServerMessage>(OUTGOING);
    let writer = tokio::spawn(write(tx, outgoing));
    let result = serve(&state, &mut rx, out).await;
    // The sender went with `serve`, so the writer sends what is queued and
    // finishes.
    let _ = writer.await;
    result
}

async fn write(mut tx: Sink, mut outgoing: mpsc::Receiver<SyncServerMessage>) {
    while let Some(message) = outgoing.recv().await {
        let Ok(text) = serde_json::to_string(&message) else {
            continue;
        };
        if tx.send(Message::Text(text.into())).await.is_err() {
            return;
        }
    }
    let _ = tx.close().await;
}

async fn serve(
    state: &Arc<AppState>,
    rx: &mut futures_util::stream::SplitStream<WebSocket>,
    out: mpsc::Sender<SyncServerMessage>,
) -> anyhow::Result<()> {
    // Bounded, so that a session that cannot keep up backs up into the note's
    // broadcast buffer, which lags, which is replayed from the store.
    let (fanned_tx, mut fanned) = mpsc::channel::<Fanned>(app::FANOUT_BUFFER);
    let mut subs = Subscriptions(HashMap::new());
    // Notes with a backlog still to send, taken in turn one chunk at a time.
    let mut backlog: VecDeque<String> = VecDeque::new();

    loop {
        tokio::select! {
            Some(event) = fanned.recv() => match event {
                Fanned::Update { note, seq, envelope } => {
                    let Some(sub) = subs.0.get_mut(&note) else { continue };
                    if sub.catching_up || seq <= sub.last_sent {
                        continue;
                    }
                    let update = SyncServerMessage::Update {
                        note: note.clone(),
                        seq,
                        envelope: URL_SAFE_NO_PAD.encode(&envelope),
                    };
                    match out.try_send(update) {
                        Ok(()) => sub.last_sent = seq,
                        // No room: the client is not reading. The update is in
                        // the store, so it is left there and sent as backlog
                        // once there is room, rather than held here.
                        Err(mpsc::error::TrySendError::Full(_)) => {
                            sub.catching_up = true;
                            backlog.push_back(note);
                        }
                        Err(mpsc::error::TrySendError::Closed(_)) => return Ok(()),
                    }
                }
                Fanned::Lagged { note } => {
                    let Some(sub) = subs.0.get_mut(&note) else { continue };
                    tracing::debug!(note = %note, "subscriber lagged; replaying");
                    if !sub.catching_up {
                        sub.catching_up = true;
                        backlog.push_back(note);
                    }
                }
            },

            incoming = rx.next() => match incoming {
                Some(Ok(Message::Text(t))) => {
                    handle(state, &out, &fanned_tx, &mut subs, &mut backlog, &t).await;
                }
                Some(Ok(Message::Close(_))) | None => return Ok(()),
                Some(Ok(_)) => {}
                Some(Err(e)) => return Err(e.into()),
            },

            // Ready whenever something is catching up and a whole chunk, and
            // its `caught`, fits in what is waiting for the socket. `select!`
            // takes it in turn with whatever else is ready, so one chunk goes
            // and then the rest of the socket gets a look.
            permits = out.reserve_many(BACKLOG_CHUNK + 1), if !backlog.is_empty() => {
                let Ok(permits) = permits else { return Ok(()) };
                let Some(note) = backlog.pop_front() else { continue };
                if let Some(sub) = subs.0.get_mut(&note) {
                    if sub.catching_up && send_chunk(state, permits, &note, sub).await? {
                        backlog.push_back(note);
                    }
                }
            },
        }
    }
}

/// Send the next chunk of one note's backlog. True if there is more.
async fn send_chunk(
    state: &Arc<AppState>,
    mut permits: mpsc::PermitIterator<'_, SyncServerMessage>,
    note: &str,
    sub: &mut Subscription,
) -> anyhow::Result<bool> {
    let page = state.store.page(note, sub.last_sent, BACKLOG_CHUNK).await?;
    let more = page.len() == BACKLOG_CHUNK;
    for stored in page {
        let permit = permits.next().expect("a permit per envelope in a chunk");
        permit.send(SyncServerMessage::Update {
            note: note.to_string(),
            seq: stored.seq,
            envelope: URL_SAFE_NO_PAD.encode(&stored.envelope),
        });
        sub.last_sent = sub.last_sent.max(stored.seq);
    }
    if !more {
        sub.catching_up = false;
        if !sub.caught {
            sub.caught = true;
            let permit = permits.next().expect("a permit for caught");
            permit.send(SyncServerMessage::Caught {
                note: note.to_string(),
                seq: sub.last_sent,
            });
        }
    }
    Ok(more)
}

async fn handle(
    state: &Arc<AppState>,
    out: &mpsc::Sender<SyncServerMessage>,
    fanned: &mpsc::Sender<Fanned>,
    subs: &mut Subscriptions,
    backlog: &mut VecDeque<String>,
    text: &str,
) {
    let message = match serde_json::from_str::<SyncClientMessage>(text) {
        Ok(message) => message,
        Err(e) => return refuse(out, None, &format!("malformed message: {e}")),
    };
    match message {
        SyncClientMessage::Subscribe { note, since } => {
            if NoteId::decode(&note).is_err() {
                return refuse(out, Some(note), "malformed note id");
            }
            if subs.0.contains_key(&note) {
                return refuse(out, Some(note), "already subscribed");
            }
            if subs.0.len() >= MAX_SUBSCRIPTIONS {
                return refuse(out, Some(note), "too many notes on one socket");
            }
            let since = since.max(0);
            let head = match state.store.head(&note).await {
                Ok(head) => head,
                Err(e) => return refuse(out, Some(note), &e.to_string()),
            };
            if since > head {
                // Do not install a subscription: appends queued behind this
                // subscribe belong to the recovery connection, after its client
                // has rebuilt the cursor and outbox. `caught` cannot carry this
                // signal, because an append can advance a restored log before
                // the backlog is sent.
                tracing::warn!(note = %note, since, head, "client cursor is beyond relay head");
                let _ = out.send(SyncServerMessage::Reset { note }).await;
                return;
            }
            let forwarder = forward(state, note.clone(), fanned.clone());
            subs.0.insert(
                note.clone(),
                Subscription {
                    last_sent: since,
                    catching_up: true,
                    caught: false,
                    forwarder,
                },
            );
            backlog.push_back(note);
        }
        SyncClientMessage::Unsubscribe { note } => {
            // Anything of it still queued is dropped where it is read, because
            // the subscription it would be sent under is gone.
            if let Some(sub) = subs.0.remove(&note) {
                sub.forwarder.abort();
            }
        }
        SyncClientMessage::Append { note, envelope } => {
            // A socket writes only to notes it has subscribed to.
            if !subs.0.contains_key(&note) {
                return refuse(out, Some(note), "not subscribed to that note");
            }
            // The sender gets its own append back through the fan-out, like
            // everyone else.
            match app::append(state, &note, &envelope).await {
                Ok(accepted) => {
                    if accepted.crowded {
                        let _ = out.try_send(SyncServerMessage::Crowded { note });
                    }
                }
                Err(e) => refuse(out, Some(note), &e.to_string()),
            }
        }
        SyncClientMessage::Compact {
            note,
            covers,
            base,
            key,
            signature,
        } => {
            if !subs.0.contains_key(&note) {
                return refuse(out, Some(note), "not subscribed to that note");
            }
            if let Err(reason) = app::compact(state, &note, covers, &base, &key, &signature).await {
                tracing::debug!(note = %note, %reason, "compaction refused");
                let _ = out.try_send(SyncServerMessage::CompactRefused { note, reason });
            }
        }
    }
}

/// Tell the client something was refused, if there is room to.
///
/// Dropped rather than waited for when the client is not reading, because
/// waiting would stop the session reading too. Nothing is lost with it: a
/// refused append was never stored, and the client's log still has it.
fn refuse(out: &mpsc::Sender<SyncServerMessage>, note: Option<String>, message: &str) {
    let _ = out.try_send(SyncServerMessage::Error {
        note,
        message: message.to_string(),
    });
}

/// Subscribe to a note's fan-out now, and forward it into the session from a
/// task of its own.
///
/// The subscription is taken here, before the caller queues the backlog, which
/// is the ordering the stream relies on: an append landing between
/// the two is in the store by the time the backlog is read, and anything seen
/// both ways is dropped by `seq`.
fn forward(state: &Arc<AppState>, note: String, fanned: mpsc::Sender<Fanned>) -> JoinHandle<()> {
    let mut live = state.channel(&note).subscribe();
    tokio::spawn(async move {
        loop {
            let event = match live.recv().await {
                Ok((seq, envelope)) => Fanned::Update {
                    note: note.clone(),
                    seq,
                    envelope,
                },
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    Fanned::Lagged { note: note.clone() }
                }
                Err(broadcast::error::RecvError::Closed) => return,
            };
            if fanned.send(event).await.is_err() {
                return; // the session has ended
            }
        }
    })
}
