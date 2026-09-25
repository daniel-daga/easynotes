//! Talking to the relay.
//!
//! Three layers, deliberately separated. [`wire`] is the protocol and nothing
//! else — messages, base64, what counts as nonsense — with no socket in it.
//! `client` and `client_web` are the two transports that carry it: a socket
//! that sends and receives text, and the handshake. `socket` is the few lines
//! that put the two together, once. [`engine`] is the policy on top — one
//! socket per device, when to connect, what to do when the connection dies,
//! what to re-send, and what the UI is told about any of it.
//!
//! That split is what lets a browser have the second and third layers for free.
//! A tab needs its own transport because it cannot open a TCP socket, and it
//! needs nothing else of its own: the reconnect loop, the outbox and the cursor
//! are the same decisions in both places, and the message shapes are the same
//! bytes. Only the middle layer is written twice, and [`wire`] is what keeps
//! the two halves of it from drifting apart.
//!
//! The client is deliberately dumb in the opposite direction from the relay: it
//! trusts the relay for *ordering and delivery* and for nothing else. Every
//! envelope that comes back is verified and decrypted locally before it reaches
//! the document, so a hostile relay can withhold updates or reorder them, but it
//! cannot forge one.

// Compiled even with no transport, so that a plain `cargo test` runs the
// protocol tests — which is the whole reason it is a module of its own. With no
// transport, though, nothing outside those tests calls any of it, and the relay
// builds this crate exactly that way: it needs the wire format and neither end
// of a socket. Hence the allowance, which is narrow and disappears the moment
// either transport is on.
#[cfg_attr(
    not(any(
        feature = "sync-native",
        all(feature = "sync-web", target_arch = "wasm32")
    )),
    allow(dead_code)
)]
mod wire;
pub use wire::{Incoming, RequestState, Routed};

// Exactly one transport is compiled, and which one is not a choice a caller
// makes: a device has no browser to borrow a socket from and a tab cannot open
// a TCP one. They present the same `authenticate` and `Socket`, so nothing
// above here has a `cfg` in it.
#[cfg(feature = "sync-native")]
mod client;
#[cfg(feature = "sync-native")]
use client as transport;
#[cfg(feature = "sync-native")]
pub use client::authenticate;

#[cfg(all(feature = "sync-web", target_arch = "wasm32"))]
mod client_web;
#[cfg(all(feature = "sync-web", target_arch = "wasm32"))]
use client_web as transport;
#[cfg(all(feature = "sync-web", target_arch = "wasm32"))]
pub use client_web::authenticate;

#[cfg(any(
    feature = "sync-native",
    all(feature = "sync-web", target_arch = "wasm32")
))]
mod socket;
#[cfg(any(
    feature = "sync-native",
    all(feature = "sync-web", target_arch = "wasm32")
))]
pub use socket::DeviceSync;

/// The invite calls. Plain HTTP over the same transport, with no socket and
/// no device key — see the module for why not the latter.
#[cfg(any(
    feature = "sync-native",
    all(feature = "sync-web", target_arch = "wasm32")
))]
pub mod invite;

#[cfg(any(
    feature = "sync-native",
    all(feature = "sync-web", target_arch = "wasm32")
))]
mod engine;
#[cfg(any(
    feature = "sync-native",
    all(feature = "sync-web", target_arch = "wasm32")
))]
mod timer;
#[cfg(any(
    feature = "sync-native",
    all(feature = "sync-web", target_arch = "wasm32")
))]
pub use engine::{
    Pacing, RelayEndpoint, SyncDevice, SyncEngine, SyncHandle, SyncPeer, SyncState, SyncStatus,
    FIRST_RETRY, MAX_RETRY,
};
