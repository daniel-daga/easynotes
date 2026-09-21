//! EasyNotes core: crypto, CRDT, op log and sync, with no UI and no platform policy.
//!
//! This crate is callable from a plain `cargo test` with no Flutter in the loop.
//! That is deliberate — it is what keeps the security-critical code auditable.

pub mod account;
pub mod crdt;
pub mod crypto;
pub mod error;
pub mod ids;
pub mod index;
pub mod invite;
pub mod note;
pub mod order;
pub mod store;
pub mod text;

// Always, though the transports inside it are not: `sync::wire` is the protocol
// both of them speak and is the only part of talking to a relay that a plain
// `cargo test` can reach, so gating the module would mean the thing the two
// transports must agree about were tested by neither.
pub mod sync;

pub use error::{CoreError, Result};
