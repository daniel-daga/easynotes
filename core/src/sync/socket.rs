//! The two things said over a socket to the relay, written once for both
//! transports.
//!
//! [`DeviceSync`] is what the engine uses: one socket for every note a device
//! syncs, at `/v1/sync`. [`NoteSync`] is the socket per note that came before
//! it, at `/v1/notes/{id}`, kept because the relay keeps that route for a
//! release and the tests prove an old app and a new one agree on a note.
//!
//! Neither knows what a socket is. `client` and `client_web` each provide a
//! `Socket` that carries text, and [`super::wire`] turns the text into
//! messages; this is the few lines in between, which would otherwise be
//! written once per transport and could disagree.

use super::transport::Socket;
use super::wire::{self, Incoming, Routed};
use crate::error::Result;
use crate::ids::NoteId;

/// A device's one connection to the relay, carrying every note it syncs.
///
/// Each note is subscribed on it with a cursor of its own, at any point in the
/// socket's life, and everything that comes back says which note it is about.
pub struct DeviceSync {
    socket: Socket,
}

impl DeviceSync {
    /// Open the socket. Nothing is subscribed until [`subscribe`] is called.
    ///
    /// [`subscribe`]: Self::subscribe
    pub async fn connect(ws_base: &str, token: &str) -> Result<Self> {
        let socket = Socket::open(&wire::sync_url(ws_base, token)).await?;
        Ok(Self { socket })
    }

    /// Start receiving a note: everything after `since`, then a `caught`, then
    /// live.
    pub async fn subscribe(&mut self, note: NoteId, since: i64) -> Result<()> {
        self.socket.send(wire::subscribe_note(note, since)).await
    }

    /// Stop receiving a note. Anything of it already in flight still arrives.
    pub async fn unsubscribe(&mut self, note: NoteId) -> Result<()> {
        self.socket.send(wire::unsubscribe_note(note)).await
    }

    /// Hand an envelope for a subscribed note to the relay. It comes back
    /// through [`next`](Self::next) like any other.
    pub async fn append(&mut self, note: NoteId, envelope: &[u8]) -> Result<()> {
        self.socket.send(wire::append_note(note, envelope)).await
    }

    /// The next thing from the relay, or `None` when it closes. Cancel-safe.
    pub async fn next(&mut self) -> Result<Option<Routed>> {
        match self.socket.recv().await? {
            Some(text) => wire::parse_routed(&text).map(Some),
            None => Ok(None),
        }
    }

    pub async fn close(self) -> Result<()> {
        self.socket.close().await
    }
}

/// A live connection to one note, over the socket per note that predates
/// [`DeviceSync`].
pub struct NoteSync {
    socket: Socket,
    /// The highest sequence number applied. Survives a reconnect: it is what the
    /// next `subscribe` replays from.
    pub last_seq: i64,
}

impl NoteSync {
    /// Connect and subscribe from `since`. Pass the `last_seq` from a previous
    /// connection to resume; pass 0 for the whole log.
    pub async fn connect(ws_base: &str, token: &str, note: NoteId, since: i64) -> Result<Self> {
        let mut socket = Socket::open(&wire::note_url(ws_base, note, token)).await?;
        socket.send(wire::subscribe(since)).await?;
        Ok(Self {
            socket,
            last_seq: since,
        })
    }

    /// Hand an envelope to the relay. It comes back through `next` like any other,
    /// which is what keeps the local and remote paths identical.
    pub async fn append(&mut self, envelope: &[u8]) -> Result<()> {
        self.socket.send(wire::append(envelope)).await
    }

    /// The next thing from the relay, or `None` when it closes.
    pub async fn next(&mut self) -> Result<Option<Incoming>> {
        match self.socket.recv().await? {
            Some(text) => wire::parse(&text, &mut self.last_seq).map(Some),
            None => Ok(None),
        }
    }

    pub async fn close(self) -> Result<()> {
        self.socket.close().await
    }
}
