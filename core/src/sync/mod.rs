//! Talking to the relay.
//!
//! Two layers, deliberately separated. [`client`] is the protocol and nothing
//! else: one socket, one note, messages in and out. [`engine`] is the policy on
//! top — when to connect, what to do when the connection dies, what to re-send,
//! and what the UI is told about any of it.
//!
//! The split is what lets the web build reuse the second half. A browser needs
//! its own transport, so `client` will gain a second implementation; the
//! reconnect loop, the outbox and the cursor are the same decisions in both
//! places and should not be made twice.
//!
//! The client is deliberately dumb in the opposite direction from the relay: it
//! trusts the relay for *ordering and delivery* and for nothing else. Every
//! envelope that comes back is verified and decrypted locally before it reaches
//! the document, so a hostile relay can withhold updates or reorder them, but it
//! cannot forge one.

mod client;
mod engine;

pub use client::{authenticate, Incoming, NoteSync};
pub use engine::{
    RelayEndpoint, SyncEngine, SyncHandle, SyncPeer, SyncState, SyncStatus, FIRST_RETRY, MAX_RETRY,
};
