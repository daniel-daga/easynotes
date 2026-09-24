//! The CRDT layer, behind a trait so decision D1 stays reversible.
//!
//! Slice 1 ships two interchangeable backends, Automerge and Yrs, and a benchmark
//! that measures them (see `benches/` and `docs/adr/0001-crdt.md`). Everything above
//! this module talks to [`NoteDoc`] and never names a backend.
//!
//! ## Why a map of items and not the CRDT's own list type (decision D3)
//!
//! Items live in a map keyed by item ID, and each item carries an `order` field
//! holding a fractional index (see [`crate::order`]). A list CRDT would give us
//! ordering for free, but neither Automerge nor Yrs has a *move* operation, so a
//! reorder has to be expressed as a delete plus an insert — and two people
//! reordering concurrently then duplicate or lose items. That is exactly demo step 4.
//!
//! With a map plus an order field:
//!   * an item ID appears once by construction, so duplication is impossible;
//!   * a move writes one field, so concurrent moves resolve to one winner;
//!   * a concurrent move and rename touch different fields and both survive.

use crate::error::Result;
use crate::ids::ItemId;
use serde::{Deserialize, Serialize};

pub mod account;

#[cfg(feature = "automerge-backend")]
pub mod automerge_account;
#[cfg(feature = "automerge-backend")]
pub mod automerge_doc;
#[cfg(feature = "yrs-backend")]
pub mod yrs_account;
#[cfg(feature = "yrs-backend")]
pub mod yrs_doc;

pub use account::{
    AccountDoc, AccountEdit, AccountView, Colour, Layout, Placement, Reminder, RepeatRule,
};

#[cfg(feature = "automerge-backend")]
pub use automerge_account::AutomergeAccount;
#[cfg(feature = "automerge-backend")]
pub use automerge_doc::AutomergeNote;
#[cfg(feature = "yrs-backend")]
pub use yrs_account::YrsAccount;
#[cfg(feature = "yrs-backend")]
pub use yrs_doc::YrsNote;

/// One checklist item as the UI sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ItemView {
    pub id: ItemId,
    pub text: String,
    pub checked: bool,
    /// The fractional index. Exposed so the UI can compute a drop target's key.
    pub order: String,
}

/// Whether a note is written as prose or as a list of things to tick off.
///
/// It is shared rather than personal: it lives in the note's own document, so
/// turning a note into a checklist turns it into one for everyone on it. A
/// colour is a matter of taste; whether the shopping is a list is not.
///
/// A document that has never recorded a kind is a checklist, because every note
/// written before this field existed was one. That default is what saves those
/// notes a migration.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NoteKind {
    #[default]
    Checklist,
    Text,
}

impl NoteKind {
    /// The value written into the document. Stable: it is on other devices.
    pub fn as_str(self) -> &'static str {
        match self {
            NoteKind::Checklist => "checklist",
            NoteKind::Text => "text",
        }
    }

    /// Read a recorded kind. Anything unrecognised — missing, or written by a
    /// later version — is a checklist, the one kind every build can draw.
    pub fn parse(s: &str) -> Self {
        match s {
            "text" => NoteKind::Text,
            _ => NoteKind::Checklist,
        }
    }
}

/// The whole note as the UI sees it. Items are sorted by `(order, id)`; the ID
/// breaks ties so two devices that generate the same order key still agree on
/// the displayed sequence.
///
/// `body` and `items` are both always present, whatever `kind` says. Converting
/// a note moves its content from one to the other, but a peer who was typing
/// into the side that was just emptied still has their edit merged in — and the
/// rule is that a merge never *hides* anything. So a checklist can arrive with
/// text in its body, and a text note with items on it, and a screen draws both
/// rather than choosing. See `docs/plan-note-kinds.md`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NoteView {
    pub kind: NoteKind,
    pub title: String,
    pub body: String,
    pub items: Vec<ItemView>,
}

impl NoteView {
    pub fn item(&self, id: ItemId) -> Option<&ItemView> {
        self.items.iter().find(|i| i.id == id)
    }

    /// The order keys of the currently visible items, ready for
    /// [`crate::order::key_for_index`].
    pub fn order_keys(&self) -> Vec<String> {
        self.items.iter().map(|i| i.order.clone()).collect()
    }

    /// Checked items are not deleted and not hidden here: hiding them is a view
    /// setting, per the design doc, so it belongs to the UI and not to the data.
    pub fn checked_count(&self) -> usize {
        self.items.iter().filter(|i| i.checked).count()
    }
}

/// A single change. `AddItem` and `MoveItem` carry an already-computed order key
/// rather than an index, so the backends never have to reason about positions;
/// [`crate::note::Note`] turns "insert at index 3" into a key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Edit {
    AddCollaborator(String),
    ClearCollaborators,
    SetTitle(String),
    AddItem {
        id: ItemId,
        text: String,
        order: String,
    },
    SetItemText {
        id: ItemId,
        text: String,
    },
    SetChecked {
        id: ItemId,
        checked: bool,
    },
    MoveItem {
        id: ItemId,
        order: String,
    },
    RemoveItem {
        id: ItemId,
    },
    /// Record the kind and nothing else. What a new text note is created with.
    SetKind(NoteKind),
    /// Replace the body's text. The backend works out the smallest splice that
    /// turns the old text into this one and applies only that, so a peer typing
    /// elsewhere in the same body keeps their characters — which is the whole
    /// reason the body is the backend's text type rather than a register like
    /// an item's text.
    SetBody(String),
    /// Turn the note into a checklist in one transaction: record the kind, empty
    /// the body, and add these items. One transaction so that a peer never sees
    /// the items arrive before the body empties, or the other way round.
    ToChecklist {
        items: Vec<NewItem>,
    },
    /// Turn the note into text in one transaction: record the kind, set the body
    /// (as [`Edit::SetBody`] does, by splice) and remove these items.
    ToText {
        body: String,
        remove: Vec<ItemId>,
    },
}

/// An item as [`Edit::ToChecklist`] adds, with its order key already worked out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewItem {
    pub id: ItemId,
    pub text: String,
    pub checked: bool,
    pub order: String,
}

/// The one splice that turns `old` into `new`: `(byte offset, bytes to delete,
/// text to insert)`, found by trimming the prefix and suffix the two share.
///
/// Byte offsets on `char` boundaries, which is what Yrs counts in by default.
/// It is not a minimal diff in general — two separate changes become one splice
/// spanning both — but the UI sends one body per keystroke, and a keystroke is
/// one change, so in practice it is exactly the edit that was made.
#[cfg(feature = "yrs-backend")]
pub(crate) fn splice<'a>(old: &str, new: &'a str) -> Option<(usize, usize, &'a str)> {
    if old == new {
        return None;
    }
    let prefix: usize = old
        .char_indices()
        .zip(new.chars())
        .take_while(|((_, a), b)| a == b)
        .last()
        .map(|((i, c), _)| i + c.len_utf8())
        .unwrap_or(0);
    let (old_rest, new_rest) = (&old[prefix..], &new[prefix..]);
    let suffix: usize = old_rest
        .chars()
        .rev()
        .zip(new_rest.chars().rev())
        .take_while(|(a, b)| a == b)
        .map(|(c, _)| c.len_utf8())
        .sum();
    let deleted = old_rest.len() - suffix;
    let inserted = &new_rest[..new_rest.len() - suffix];
    Some((prefix, deleted, inserted))
}

/// A note's CRDT state.
///
/// Implementations must be *convergent*: any two documents that have applied the
/// same set of updates, in any order and with any duplicates, must produce equal
/// [`NoteView`]s. `tests/convergence.rs` holds that to account for both backends.
pub trait NoteDoc: Sized {
    /// Untrusted assertions; callers must verify against the pinned owner.
    fn collaborator_assertions(&self) -> Vec<String>;
    /// The account document from the same backend.
    ///
    /// An associated type rather than a second parameter on
    /// [`crate::note::NoteCore`]: a device runs one CRDT implementation, and
    /// picking Yrs for its notes and Automerge for its placements is not a
    /// choice anyone should be able to express. It also means the suites that
    /// run over both note backends run over both account backends for free.
    type Account: AccountDoc;

    /// A stable, short identity for this device's edits. Lives inside the
    /// encrypted payload, so the relay never sees it.
    fn create(actor: &[u8; 16]) -> Self;

    fn load(actor: &[u8; 16], snapshot: &[u8]) -> Result<Self>;

    /// Apply an edit and return just the delta it produced, ready to be sealed
    /// into an [`crate::crypto::Envelope`].
    fn apply_local(&mut self, edit: &Edit) -> Result<Vec<u8>>;

    /// Apply a delta from someone else. Must be idempotent: the same update
    /// applied twice leaves the document unchanged the second time.
    fn apply_remote(&mut self, update: &[u8]) -> Result<()>;

    /// One delta that does what `updates` do between them, in the same form
    /// [`NoteDoc::apply_local`] returns.
    ///
    /// What lets a burst of typing reach the relay as one envelope rather than
    /// one per keystroke ([ADR 0016](../../../docs/adr/0016-batching-edits.md)).
    /// Applying the result must leave any replica exactly where applying every
    /// input in order would, including one that has already seen some of them.
    fn merge_updates(updates: &[Vec<u8>]) -> Result<Vec<u8>>;

    fn view(&self) -> NoteView;

    /// The full state, for the encrypted snapshot that lets a client skip the log.
    fn snapshot(&self) -> Vec<u8>;

    fn backend_name() -> &'static str;
}

/// Sort items into display order. Both backends share this so a difference
/// between them can never be a difference in sorting.
#[cfg(any(feature = "automerge-backend", feature = "yrs-backend"))]
pub(crate) fn sort_items(items: &mut [ItemView]) {
    items.sort_by(|a, b| a.order.cmp(&b.order).then_with(|| a.id.cmp(&b.id)));
}

/// The backend the app uses. See [ADR 0001](../../../docs/adr/0001-crdt.md).
///
/// Everything outside this module refers to the alias, never the concrete type,
/// so revisiting D1 means changing this line and re-running the suite. The
/// account document follows it, through [`NoteDoc::Account`].
#[cfg(feature = "yrs-backend")]
pub type DefaultNoteDoc = YrsNote;
#[cfg(all(not(feature = "yrs-backend"), feature = "automerge-backend"))]
pub type DefaultNoteDoc = AutomergeNote;
