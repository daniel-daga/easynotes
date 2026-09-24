//! A person on a shared note, as a list or a search shows them.
//!
//! Read off the note's current access package
//! ([ADR 0023](../../docs/adr/0023-access-packages.md)): one entry per member
//! ID, so one person on two phones is one entry and two people who chose the
//! same name are two.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Collaborator {
    /// The member ID, base64url.
    pub id: String,
    /// Chosen by the person. A label, not a verified identity.
    pub name: String,
}
