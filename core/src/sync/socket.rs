//! What is said over the socket to the relay, written once for both
//! transports.
//!
//! [`DeviceSync`] is what the engine uses: one socket for every note a device
//! syncs, at `/v1/sync`.
//!
//! It does not know what a socket is. `client` and `client_web` each provide a
//! `Socket` that carries text, and [`super::wire`] turns the text into
//! messages; this is the few lines in between, which would otherwise be
//! written once per transport and could disagree.

use super::transport::Socket;
use super::wire::{self, Routed};
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

    /// Hand the relay a base for a subscribed note. What becomes of it comes
    /// back through [`next`](Self::next): the base itself, as an update, or a
    /// refusal.
    pub async fn compact(
        &mut self,
        note: NoteId,
        request: &crate::compaction::Request,
    ) -> Result<()> {
        self.socket.send(wire::compact_note(note, request)).await
    }

    /// The next thing from the relay, or `None` when it closes. Cancel-safe.
    pub async fn next(&mut self) -> Result<Option<Routed>> {
        // Cancel-safe still: a skipped message was read whole, so dropping
        // this future between two reads loses nothing.
        loop {
            match self.socket.recv().await? {
                Some(text) => match wire::parse_routed(&text)? {
                    Some(routed) => return Ok(Some(routed)),
                    None => continue,
                },
                None => return Ok(None),
            }
        }
    }

    pub async fn close(self) -> Result<()> {
        self.socket.close().await
    }
}
