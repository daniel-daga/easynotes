//! One socket per device: `/v1/sync`.
//!
//! A device opens one connection and does one token handshake, however many
//! notes it syncs, and every note is carried over it — see [ADR 0017](../../docs/decisions.md#adr-0017). Each note's stream
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
//! queue bounded by message count and envelope bytes, drained by a task of its
//! own; when it is full, a live update is left in the store and sent later as
//! backlog, and a chunk waits for room. So
//! a client that is slow to read slows only what it is sent, never what it
//! sends — which matters because a device subscribing its notes on connecting
//! writes a great deal before it reads.
//!
//! **What the relay learns:** which notes one device key syncs, which it would
//! learn anyway from that key appending to them. It still cannot tell two of
//! one person's devices from two collaborators ([ADR 0014](../../docs/decisions.md#adr-0014)).

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
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{broadcast, mpsc, Notify};
use tokio::task::JoinHandle;

pub(crate) async fn sync_ws(
    State(state): State<Arc<AppState>>,
    Query(q): Query<app::WsQuery>,
    client: Option<axum::Extension<app::Client>>,
    ws: WebSocketUpgrade,
) -> Result<Response> {
    let Some(device) = state.auth.device_for(&q.token) else {
        return Err(RelayError::Unauthorized);
    };
    let connection = Connection {
        client: client.map(|c| c.0),
        read_only: device.read_only,
        expires_at: device.expires_at,
    };
    // Taken before the upgrade, so that a stop that begins in between still
    // waits for this session, which will see it and close at once.
    let stopping = state.shutdown.watch();
    if state.shutdown.is_draining() {
        return Err(RelayError::Unavailable("stopping"));
    }
    Ok(ws
        // Refused at the frame header, before the payload is buffered or
        // base64-decoded.
        .max_message_size(MAX_MESSAGE_BYTES)
        .max_frame_size(MAX_MESSAGE_BYTES)
        .on_upgrade(move |socket| async move {
            let _open = state.metrics.socket();
            if let Err(e) = run(socket, state.clone(), connection, stopping).await {
                tracing::debug!(error = %e, "device session ended");
            }
        }))
}

/// Who one socket belongs to: the address its write budget is keyed on,
/// whether its token allows writing (ADR 0038), and when the token expires.
#[derive(Clone, Copy)]
struct Connection {
    client: Option<app::Client>,
    read_only: bool,
    expires_at: u64,
}

/// What a note's fan-out hands the session, tagged with the note.
enum Fanned {
    Update {
        note: String,
        seq: i64,
        envelope: Arc<[u8]>,
    },
    /// The note's fan-out dropped messages for this subscriber; read them back
    /// out of the store.
    Lagged { note: String },
    /// The note was purged while subscribed.
    Gone { note: String },
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
    /// The refresh barriers still outstanding for this subscription, one per
    /// id the client has asked for.
    refresh: Vec<(String, i64)>,
    /// Forwards the note's fan-out into the session. Aborted on unsubscribe.
    forwarder: JoinHandle<()>,
    metrics: Arc<crate::metrics::Metrics>,
}

impl Drop for Subscription {
    fn drop(&mut self) {
        self.metrics.subscription_ended();
    }
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
/// in the store and replayed, and backlog chunks wait.
const OUTGOING: usize = 4 * BACKLOG_CHUNK;

/// How many envelope bytes may wait for the socket. A stalled reader would
/// otherwise hold up to [`OUTGOING`] × [`MAX_QUEUED_ENVELOPE`] of base64 —
/// about 341 MiB — per socket, whatever the size of the envelopes actually
/// flowing. Eight MiB is dozens of ordinary backlog chunks ([`BACKLOG_CHUNK`]
/// envelopes of a few KiB each), and still fits five envelopes at the 1 MiB
/// cap, so a replay moves even when every envelope is as large as one may be.
const OUTGOING_BYTES: usize = 8 << 20;

/// The most one queued envelope can take: base64 of a capped raw one.
const MAX_QUEUED_ENVELOPE: usize = (MAX_ENVELOPE_BYTES / 3 + 1) * 4;

// A chunk of ordinary envelopes must fit whole, and so must one envelope at
// the cap, or a replay could never start. The cap must also still be a cut
// below what counting messages alone allowed, and still hold five envelopes
// at the cap as the comment above says.
const _: () = assert!(OUTGOING_BYTES > BACKLOG_CHUNK * 64 * 1024);
const _: () = assert!(OUTGOING_BYTES > MAX_QUEUED_ENVELOPE);
const _: () = assert!(OUTGOING_BYTES < OUTGOING * MAX_QUEUED_ENVELOPE);
const _: () = assert!(OUTGOING_BYTES >= 5 * MAX_QUEUED_ENVELOPE);
const _: () = assert!(MAX_QUEUED_ENVELOPE < MAX_MESSAGE_BYTES);

async fn run(
    socket: WebSocket,
    state: Arc<AppState>,
    connection: Connection,
    mut stopping: crate::shutdown::Watch,
) -> anyhow::Result<()> {
    let (tx, mut rx) = socket.split();

    // Writing is a task of its own, so that reading never waits for it. A
    // session that stopped reading because its client stopped reading would
    // stop taking that client's own appends and subscriptions, and a client
    // that writes before it reads — twenty subscriptions on connecting, say,
    // while the first note's backlog comes back — would wait on the relay
    // while the relay waited on it.
    let (out, outgoing) = mpsc::channel::<SyncServerMessage>(OUTGOING);
    let queued = Arc::new(AtomicUsize::new(0));
    let freed = Arc::new(Notify::new());
    let writer = tokio::spawn(write(tx, outgoing, queued.clone(), freed.clone()));
    let result = serve(
        &state,
        connection,
        &mut rx,
        out,
        &mut stopping,
        &queued,
        &freed,
    )
    .await;
    // The sender went with `serve`, so the writer sends what is queued and
    // finishes, closing the socket. `stopping` is held until then: the relay
    // has not drained while a queue is still being written.
    //
    // But not for ever. A client that has stopped reading never takes the
    // rest, and the writer would wait on its socket past the token's expiry
    // and past the relay's stop. Past [`WRITER_GRACE`] the socket is cut:
    // nothing is lost, because whatever was unsent is in the store and the
    // client resumes from its cursor when it reconnects.
    let mut writer = writer;
    if tokio::time::timeout(WRITER_GRACE, &mut writer)
        .await
        .is_err()
    {
        tracing::debug!("the client stopped reading; cutting its socket");
        writer.abort();
        let _ = writer.await;
    }
    drop(stopping);
    result
}

async fn write<S>(
    mut tx: S,
    mut outgoing: mpsc::Receiver<SyncServerMessage>,
    queued: Arc<AtomicUsize>,
    freed: Arc<Notify>,
) where
    S: futures_util::Sink<Message> + Send + Unpin,
{
    while let Some(message) = outgoing.recv().await {
        let bytes = queued_bytes(&message);
        let sent = match serde_json::to_string(&message) {
            Ok(text) => tx.send(Message::Text(text.into())).await.is_ok(),
            Err(_) => true,
        };
        // Out of the queue either way, so its bytes are free for the next one.
        if bytes > 0 {
            queued.fetch_sub(bytes, Ordering::AcqRel);
            freed.notify_one();
        }
        if !sent {
            // No more bytes will ever be freed. Close the queue before the
            // wake, so a session waiting for room sees `is_closed` rather than
            // waiting for a byte that is not coming.
            drop(outgoing);
            freed.notify_one();
            return;
        }
    }
    let _ = tx.close().await;
}

/// Hold `bytes` of the session's outgoing budget. False when that would pass
/// [`OUTGOING_BYTES`]; the claim is not made then.
fn reserve(queued: &AtomicUsize, bytes: usize) -> bool {
    // Only the session adds and only the writer subtracts, so undoing a failed
    // claim cannot eat a release that landed in between.
    if queued.fetch_add(bytes, Ordering::AcqRel) + bytes > OUTGOING_BYTES {
        queued.fetch_sub(bytes, Ordering::AcqRel);
        return false;
    }
    true
}

/// Queue one message, holding an update's envelope against the byte budget.
/// `Full` means it did not fit and was not queued.
fn try_queue(
    out: &mpsc::Sender<SyncServerMessage>,
    queued: &AtomicUsize,
    message: SyncServerMessage,
) -> std::result::Result<(), mpsc::error::TrySendError<SyncServerMessage>> {
    let bytes = queued_bytes(&message);
    if !reserve(queued, bytes) {
        return Err(mpsc::error::TrySendError::Full(message));
    }
    if let Err(error) = out.try_send(message) {
        queued.fetch_sub(bytes, Ordering::AcqRel);
        return Err(error);
    }
    Ok(())
}

/// What one queued message holds of the session's byte budget. Only envelopes
/// are large; the markers are a few bytes each and bounded by [`OUTGOING`].
fn queued_bytes(message: &SyncServerMessage) -> usize {
    match message {
        SyncServerMessage::Update { envelope, .. } => envelope.len(),
        _ => 0,
    }
}

async fn serve(
    state: &Arc<AppState>,
    connection: Connection,
    rx: &mut futures_util::stream::SplitStream<WebSocket>,
    out: mpsc::Sender<SyncServerMessage>,
    stopping: &mut crate::shutdown::Watch,
    queued: &AtomicUsize,
    freed: &Notify,
) -> anyhow::Result<()> {
    // Bounded, so that a session that cannot keep up backs up into the note's
    // broadcast buffer, which lags, which is replayed from the store.
    let (fanned_tx, mut fanned) = mpsc::channel::<Fanned>(app::FANOUT_BUFFER);
    let mut subs = Subscriptions(HashMap::new());
    // Notes with a backlog still to send, taken in turn one chunk at a time.
    let mut backlog: VecDeque<String> = VecDeque::new();
    // Set when a chunk did not fit in the byte budget: wait for the writer
    // rather than retrying a chunk that still cannot fit.
    let mut awaiting_bytes = false;
    // The token's own expiry. Closing here makes the device handshake again,
    // which is how a read-only flip or a revocation reaches an open socket
    // (ADR 0038).
    let expires_in = Duration::from_secs(
        connection
            .expires_at
            .saturating_sub(crate::auth::unix_now()),
    );
    let expiry = tokio::time::sleep(expires_in);
    tokio::pin!(expiry);

    loop {
        tokio::select! {
            // The token has run out: say nothing and close, as a clean session
            // end, so the client reconnects with a fresh one.
            () = &mut expiry => return Ok(()),

            // The relay is stopping. Returning drops `out`, so the writer sends
            // what is already queued and closes the socket, and the client
            // reconnects — to this relay's replacement — as it would after any
            // clean close. Nothing half-done is lost: an append is answered or
            // it was never taken, and a client resends what it has not had
            // acknowledged.
            () = stopping.begun() => return Ok(()),

            Some(event) = fanned.recv() => match event {
                Fanned::Update { note, seq, envelope } => {
                    let Some(sub) = subs.0.get_mut(&note) else { continue };
                    if sub.catching_up || seq <= sub.last_sent {
                        continue;
                    }
                    // Seen live, a note's numbers are dense: every append and
                    // compaction on this instance takes the next one and is
                    // broadcast under the note's lock. A jump means another
                    // instance sharing the store took the ones in between,
                    // and sending this would move the client's cursor past
                    // them. Replay from the store instead, as for a lag.
                    if seq != sub.last_sent + 1 {
                        tracing::debug!(note = %note, seq, last = sub.last_sent, "gap in the fan-out; replaying");
                        sub.catching_up = true;
                        backlog.push_back(note);
                        continue;
                    }
                    let update = SyncServerMessage::Update {
                        note: note.clone(),
                        seq,
                        envelope: URL_SAFE_NO_PAD.encode(&envelope),
                    };
                    match try_queue(&out, queued, update) {
                        Ok(()) => {
                            state.metrics.delivered_update(envelope.len());
                            sub.last_sent = seq;
                        }
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
                Fanned::Gone { note } => {
                    // The subscription ends here, so nothing of the note is
                    // sent after this, backlog included.
                    if let Some(sub) = subs.0.remove(&note) {
                        sub.forwarder.abort();
                        backlog.retain(|n| n != &note);
                        refuse(&out, Some(note), GONE);
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
                    let session = Session {
                        state,
                        client: connection.client,
                        read_only: connection.read_only,
                        out: &out,
                        fanned: &fanned_tx,
                    };
                    if let Handled::Close = handle(&session, &mut subs, &mut backlog, &t).await {
                        break;
                    }
                }
                Some(Ok(Message::Close(_))) | None => return Ok(()),
                Some(Ok(_)) => {}
                Some(Err(e)) => return Err(e.into()),
            },

            // The writer freed bytes. Retry a chunk only when a worst-case
            // envelope would fit: a wake for one small message is not worth a
            // whole page read. A dead writer closes the queue, which ends the
            // session rather than waiting for a byte that will never free.
            () = freed.notified(), if awaiting_bytes => {
                if out.is_closed() {
                    return Ok(());
                }
                awaiting_bytes =
                    queued.load(Ordering::Acquire) + MAX_QUEUED_ENVELOPE > OUTGOING_BYTES;
            },

            // Ready whenever something is catching up and a whole chunk, and
            // its `caught`, fits in what is waiting for the socket. `select!`
            // takes it in turn with whatever else is ready, so one chunk goes
            // and then the rest of the socket gets a look.
            permits = out.reserve_many(BACKLOG_CHUNK + 2), if !backlog.is_empty() && !awaiting_bytes => {
                let Ok(permits) = permits else { return Ok(()) };
                let Some(note) = backlog.pop_front() else { continue };
                if let Some(sub) = subs.0.get_mut(&note) {
                    if sub.catching_up {
                        let more = send_chunk(state, permits, queued, &note, sub).await?;
                        if more {
                            backlog.push_back(note);
                            // The next envelope may not fit in what is left;
                            // wait for the writer rather than spinning on it.
                            awaiting_bytes =
                                queued.load(Ordering::Acquire) + MAX_QUEUED_ENVELOPE > OUTGOING_BYTES;
                        }
                    }
                }
            },
        }
    }

    // Closing on the client, after telling it why. Dropping `out` lets the
    // writer send what is queued, the refusal included, and then a close
    // frame. Reading on until the client answers that matters: a client
    // flooding appends has more of them in flight, and a socket closed with
    // data unread is reset, which can destroy the refusal before the client
    // has read it — and a client that never hears why reconnects at once.
    drop(out);
    let _ = tokio::time::timeout(CLOSE_GRACE, async {
        while let Some(Ok(message)) = rx.next().await {
            if let Message::Close(_) = message {
                break;
            }
        }
    })
    .await;
    Ok(())
}

/// How long a session closed on its client waits for the client to close too.
const CLOSE_GRACE: std::time::Duration = std::time::Duration::from_secs(2);

/// How long an ended session's queue may take to reach its client before the
/// socket is cut. With [`CLOSE_GRACE`] before it, inside the default drain
/// (`RELAY_DRAIN_SECONDS`, eight seconds), so a stop is not held by a client
/// that has stopped reading.
const WRITER_GRACE: std::time::Duration = std::time::Duration::from_secs(4);

/// Send the next chunk of one note's backlog. True if there is more.
async fn send_chunk(
    state: &Arc<AppState>,
    mut permits: mpsc::PermitIterator<'_, SyncServerMessage>,
    queued: &AtomicUsize,
    note: &str,
    sub: &mut Subscription,
) -> anyhow::Result<bool> {
    let page = state.store.page(note, sub.last_sent, BACKLOG_CHUNK).await?;
    let mut more = page.len() == BACKLOG_CHUNK;
    for stored in page {
        let permit = permits.next().expect("a permit per envelope in a chunk");
        let encoded = URL_SAFE_NO_PAD.encode(&stored.envelope);
        // No room for this one yet: leave it, and the rest of the page, in the
        // store and come back once the writer has sent something.
        if !reserve(queued, encoded.len()) {
            more = true;
            break;
        }
        permit.send(SyncServerMessage::Update {
            note: note.to_string(),
            seq: stored.seq,
            envelope: encoded,
        });
        state.metrics.delivered_update(stored.envelope.len());
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
        // Answer every barrier the send reached, not just one: the client has
        // a counter of its own per action and may have two ids waiting.
        let reached = sub.last_sent;
        let pending = std::mem::take(&mut sub.refresh);
        for (id, head) in pending {
            if reached < head {
                sub.refresh.push((id, head));
                continue;
            }
            // No permit left this chunk: answer it on the next one.
            let Some(permit) = permits.next() else {
                sub.refresh.push((id, head));
                continue;
            };
            permit.send(SyncServerMessage::RefreshComplete {
                id,
                note: note.to_string(),
                seq: head,
            });
            state.metrics.refresh_completed();
        }
        if !sub.refresh.is_empty() {
            sub.catching_up = true;
            return Ok(true);
        }
    }
    Ok(more)
}

/// What one message's handling needs of its session.
struct Session<'a> {
    state: &'a Arc<AppState>,
    client: Option<app::Client>,
    /// A lapsed household may read but not write (ADR 0038).
    read_only: bool,
    out: &'a mpsc::Sender<SyncServerMessage>,
    fanned: &'a mpsc::Sender<Fanned>,
}

/// Whether the session goes on after a message.
enum Handled {
    Continue,
    /// The client has written more than its budget: say so and close, so
    /// that it backs off rather than keeps sending into refusals.
    Close,
}

/// What a message costs the write budget when it asks for no store read and
/// writes nothing: an unsubscribe, or a type this relay does not know.
const MESSAGE_BYTES: usize = 256;

/// What one store read costs the write budget: one note's head for a
/// `Refresh`.
const REFRESH_BYTES: usize = 1024;

/// What a subscribe costs the write budget: a topic read, a backlog, and a
/// note's fan-out channel and forwarder held for as long as the socket is
/// open, about 1.4 KB of memory. Charged so that one address's burst holds
/// tens of megabytes at most, while a household's every device resubscribing
/// its notes on each token renewal stays a small part of the hourly budget.
const SUBSCRIBE_BYTES: usize = 4096;

async fn handle(
    session: &Session<'_>,
    subs: &mut Subscriptions,
    backlog: &mut VecDeque<String>,
    text: &str,
) -> Handled {
    let Session {
        state,
        client,
        read_only,
        out,
        fanned,
    } = *session;
    let parsed = serde_json::from_str::<SyncClientMessage>(text);
    // Everything the relay does for a message costs its write budget, charged
    // before the work rather than after: an append or a compaction writes what
    // it sends, a subscribe reads a topic and a refresh reads a head per note,
    // so a flood of any of them is a flood of store work. A malformed message
    // is charged the fixed cost, so text that parses as nothing is not free.
    let cost = match &parsed {
        Ok(SyncClientMessage::Append { envelope, .. }) => envelope.len(),
        Ok(SyncClientMessage::Compact { base, media, .. }) => {
            base.len()
                + media
                    .as_ref()
                    .map_or(0, |objects| objects.iter().map(String::len).sum())
        }
        Ok(SyncClientMessage::Subscribe { .. }) => SUBSCRIBE_BYTES,
        Ok(SyncClientMessage::Refresh { .. }) => REFRESH_BYTES * subs.0.len().max(1),
        _ => MESSAGE_BYTES,
    };
    if let Err(retry_after) = state.spend_writes(client, cost) {
        // Pinned on a subscription where there is one, because the client can
        // only throttle on a `rate_limited` routed to a note: an unrouted one
        // reaches every note and slows none of them.
        refuse(
            out,
            subs.0.keys().next().cloned(),
            &format!("{RATE_LIMITED}:{retry_after}"),
        );
        return Handled::Close;
    }
    let message = match parsed {
        Ok(message) => message,
        Err(e) => {
            refuse(out, None, &format!("malformed message: {e}"));
            return Handled::Continue;
        }
    };
    match message {
        SyncClientMessage::Unknown => {
            tracing::debug!("skipped a message type this relay does not know");
        }
        SyncClientMessage::Subscribe { note, since } => {
            if NoteId::decode(&note).is_err() {
                return refused(out, Some(note), "malformed note id");
            }
            if subs.0.contains_key(&note) {
                return refused(out, Some(note), "already subscribed");
            }
            if subs.0.len() >= MAX_SUBSCRIPTIONS {
                return refused(out, Some(note), "too many notes on one socket");
            }
            let since = since.max(0);
            // Subscribed before the topic is read: a purge landing between the
            // two reaches the forwarder as `Gone`, and one before it is in the
            // read. The other way round, the subscriber of a note purged in
            // between was never told and got an empty `caught` for it.
            let forwarder = forward(state, note.clone(), fanned.clone());
            let topic = match state.store.topic(&note).await {
                Ok(topic) => topic,
                Err(e) => {
                    forwarder.abort();
                    return refused(out, Some(note), &e.client_message());
                }
            };
            if topic.gone {
                forwarder.abort();
                return refused(out, Some(note), GONE);
            }
            if since > topic.head {
                // Do not keep a subscription: appends queued behind this
                // subscribe belong to the recovery connection, after its client
                // has rebuilt the cursor and outbox. `caught` cannot carry this
                // signal, because an append can advance a restored log before
                // the backlog is sent.
                tracing::warn!(
                    note = %note,
                    since,
                    head = topic.head,
                    "client cursor is beyond relay head"
                );
                forwarder.abort();
                // Not waited for, for the reason a refresh is not. A reset
                // cannot be dropped, though: the client waits for it before
                // it trusts the note again. With no room, close instead;
                // the client reconnects, subscribes again and is told then.
                return match out.try_send(SyncServerMessage::Reset { note }) {
                    Ok(()) => Handled::Continue,
                    Err(_) => Handled::Close,
                };
            }
            state.metrics.subscription_started();
            subs.0.insert(
                note.clone(),
                Subscription {
                    last_sent: since,
                    catching_up: true,
                    caught: false,
                    refresh: Vec::new(),
                    forwarder,
                    metrics: state.metrics.clone(),
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
        SyncClientMessage::Refresh { id } => {
            state.metrics.refresh_requested();
            for (note, sub) in &mut subs.0 {
                if sub.refresh.iter().any(|(pending, _)| pending == &id) {
                    continue;
                }
                let head = match state.store.head(note).await {
                    Ok(head) => head,
                    Err(e) => {
                        refuse(out, Some(note.clone()), &e.client_message());
                        continue;
                    }
                };
                // Answered now if the subscription is already there and the
                // queue has room. Never waited for: a client that has stopped
                // reading would stop this session reading too, and with it
                // the token's expiry and the relay's stop. With no room, the
                // barrier waits like any other and goes with the next chunk.
                let answered = !(sub.last_sent < head || sub.catching_up)
                    && match out.try_send(SyncServerMessage::RefreshComplete {
                        id: id.clone(),
                        note: note.clone(),
                        seq: head,
                    }) {
                        Ok(()) => {
                            state.metrics.refresh_completed();
                            true
                        }
                        Err(mpsc::error::TrySendError::Full(_)) => false,
                        // The writer has gone; the session ends with it.
                        Err(mpsc::error::TrySendError::Closed(_)) => true,
                    };
                if !answered {
                    sub.refresh.push((id.clone(), head));
                    if !sub.catching_up {
                        sub.catching_up = true;
                        backlog.push_back(note.clone());
                    }
                }
            }
        }
        SyncClientMessage::Append { note, envelope } => {
            if read_only {
                refuse(out, Some(note), &RelayError::ReadOnly.client_message());
                return Handled::Continue;
            }
            // A socket writes only to notes it has subscribed to.
            if !subs.0.contains_key(&note) {
                refuse(out, Some(note), "not subscribed to that note");
                return Handled::Continue;
            }
            // The sender gets its own append back through the fan-out, like
            // everyone else.
            match app::append(state, &note, &envelope).await {
                Ok(accepted) => {
                    state.metrics.accepted_update(envelope.len());
                    if accepted.crowded {
                        let _ = out.try_send(SyncServerMessage::Crowded { note });
                    }
                }
                Err(e) => refuse(out, Some(note), &e.client_message()),
            }
        }
        SyncClientMessage::Compact {
            note,
            covers,
            base,
            media,
            key,
            signature,
        } => {
            // A read-only device never appends, so it is never the appender
            // asked to compact. Refusing here keeps compaction a write.
            if read_only {
                let _ = out.try_send(SyncServerMessage::CompactRefused {
                    note,
                    reason: RelayError::ReadOnly.client_message(),
                });
                return Handled::Continue;
            }
            if !subs.0.contains_key(&note) {
                return refused(out, Some(note), "not subscribed to that note");
            }
            let compacted = app::compact(
                state,
                &note,
                covers,
                &base,
                media.as_deref(),
                &key,
                &signature,
            )
            .await;
            if let Err(reason) = compacted {
                tracing::debug!(note = %note, %reason, "compaction refused");
                let _ = out.try_send(SyncServerMessage::CompactRefused { note, reason });
            }
        }
    }
    Handled::Continue
}

/// [`refuse`], and carry on.
fn refused(out: &mpsc::Sender<SyncServerMessage>, note: Option<String>, message: &str) -> Handled {
    refuse(out, note, message);
    Handled::Continue
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
/// The subscription is taken here, before the caller reads the topic and
/// queues the backlog, which is the ordering the stream relies on: an append
/// landing between them is in the store by the time the backlog is read, and
/// anything seen both ways is dropped by `seq`; and a purge landing between them
/// is seen, here or in the topic read.
fn forward(state: &Arc<AppState>, note: String, fanned: mpsc::Sender<Fanned>) -> JoinHandle<()> {
    let mut live = state.subscribe(&note);
    tokio::spawn(async move {
        loop {
            let event = match live.recv().await {
                Ok((app::GONE_SEQ, _)) => Fanned::Gone { note: note.clone() },
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

#[cfg(test)]
mod queue_tests {
    use super::*;

    fn update(len: usize) -> SyncServerMessage {
        SyncServerMessage::Update {
            note: "note".into(),
            seq: 1,
            envelope: "x".repeat(len),
        }
    }

    #[tokio::test]
    async fn the_byte_budget_refuses_what_would_pass_the_cap() {
        let (out, _outgoing) = mpsc::channel(OUTGOING);
        let queued = AtomicUsize::new(0);
        let half = OUTGOING_BYTES / 2;
        assert!(try_queue(&out, &queued, update(half)).is_ok());
        assert!(try_queue(&out, &queued, update(OUTGOING_BYTES - half)).is_ok());
        assert_eq!(queued.load(Ordering::Acquire), OUTGOING_BYTES);
        assert!(matches!(
            try_queue(&out, &queued, update(1)),
            Err(mpsc::error::TrySendError::Full(_))
        ));
        assert_eq!(queued.load(Ordering::Acquire), OUTGOING_BYTES);
    }

    #[tokio::test]
    async fn a_message_that_cannot_be_queued_gives_its_bytes_back() {
        let (out, mut outgoing) = mpsc::channel(1);
        let queued = AtomicUsize::new(0);
        // The one slot is taken, so the next is refused for want of a slot
        // rather than of bytes, and must not keep its claim.
        assert!(try_queue(&out, &queued, update(64)).is_ok());
        assert!(matches!(
            try_queue(&out, &queued, update(128)),
            Err(mpsc::error::TrySendError::Full(_))
        ));
        assert_eq!(queued.load(Ordering::Acquire), 64);
        // What the writer does: once sent, the bytes are free again.
        let sent = outgoing.recv().await.unwrap();
        queued.fetch_sub(queued_bytes(&sent), Ordering::AcqRel);
        assert_eq!(queued.load(Ordering::Acquire), 0);
    }

    #[tokio::test]
    async fn the_writer_releases_bytes_and_wakes_the_session() {
        let (out, outgoing) = mpsc::channel(1);
        let queued = Arc::new(AtomicUsize::new(0));
        let freed = Arc::new(Notify::new());
        assert!(try_queue(&out, &queued, update(64)).is_ok());
        assert_eq!(queued.load(Ordering::Acquire), 64);

        let writer = tokio::spawn(write(
            futures_util::sink::drain::<Message>(),
            outgoing,
            queued.clone(),
            freed.clone(),
        ));
        tokio::time::timeout(std::time::Duration::from_secs(5), freed.notified())
            .await
            .expect("the writer did not free the bytes");
        assert_eq!(queued.load(Ordering::Acquire), 0);
        drop(out);
        writer.await.unwrap();
    }

    #[tokio::test]
    async fn a_dead_writer_closes_the_queue_and_wakes_the_session() {
        let (out, outgoing) = mpsc::channel(1);
        let queued = Arc::new(AtomicUsize::new(0));
        let freed = Arc::new(Notify::new());
        // A marker counts no bytes, so only the writer's own exit can wake.
        out.send(SyncServerMessage::Caught {
            note: "note".into(),
            seq: 1,
        })
        .await
        .unwrap();
        let gone = || async { Err::<(), std::io::Error>(std::io::Error::other("gone")) };
        let writer = tokio::spawn(write(
            Box::pin(futures_util::sink::unfold((), move |(), _: Message| gone())),
            outgoing,
            queued,
            freed.clone(),
        ));
        tokio::time::timeout(std::time::Duration::from_secs(5), freed.notified())
            .await
            .expect("a dead writer did not wake a waiting session");
        assert!(out.is_closed(), "the queue must be closed before the wake");
        writer.await.unwrap();
    }

    #[test]
    fn markers_are_not_counted() {
        assert_eq!(
            queued_bytes(&SyncServerMessage::Caught {
                note: "note".into(),
                seq: 1
            }),
            0
        );
    }
}

#[cfg(test)]
mod message_cap_tests {
    use super::*;

    /// Sizing the inbound cap on the largest legal message, not on a round
    /// number: a compact's full-size base, its maximum live-media list and the
    /// JSON around them must all fit, and nothing much more may.
    #[test]
    fn the_cap_admits_the_largest_legal_message_and_no_more() {
        let largest = SyncClientMessage::Compact {
            note: "n".repeat(22),
            covers: i64::MAX,
            base: URL_SAFE_NO_PAD.encode(vec![0u8; MAX_ENVELOPE_BYTES]),
            media: Some(vec![
                "m".repeat(22);
                easynotes_core::compaction::MAX_LIVE_OBJECTS
            ]),
            key: "k".repeat(43),
            signature: "s".repeat(86),
        };
        let len = serde_json::to_string(&largest).unwrap().len();
        assert!(
            len <= MAX_MESSAGE_BYTES,
            "the cap refuses a legal message: {len} > {MAX_MESSAGE_BYTES}"
        );
        assert!(
            MAX_MESSAGE_BYTES - len < 128 * 1024,
            "the cap is {} bytes above the largest legal message",
            MAX_MESSAGE_BYTES - len
        );
    }
}
