//! EasyNotes core: crypto, CRDT, op log and sync, with no UI and no platform policy.
//!
//! This crate is callable from a plain `cargo test` with no Flutter in the loop.
//! That is deliberate — it is what keeps the security-critical code auditable.

pub mod crdt;
pub mod crypto;
pub mod error;
pub mod ids;
pub mod index;
pub mod note;
pub mod order;
pub mod store;

#[cfg(feature = "sync-native")]
pub mod sync;

pub use error::{CoreError, Result};
