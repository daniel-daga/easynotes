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

pub mod account;

#[cfg(feature = "automerge-backend")]
pub mod automerge_account;
#[cfg(feature = "automerge-backend")]
pub mod automerge_doc;
#[cfg(feature = "yrs-backend")]
pub mod yrs_account;
#[cfg(feature = "yrs-backend")]
pub mod yrs_doc;

pub use account::{AccountDoc, AccountEdit, AccountView, Colour, Layout, Placement};

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

/// The whole note as the UI sees it. Items are sorted by `(order, id)`; the ID
/// breaks ties so two devices that generate the same order key still agree on
/// the displayed sequence.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NoteView {
    pub title: String,
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
}

/// A note's CRDT state.
///
/// Implementations must be *convergent*: any two documents that have applied the
/// same set of updates, in any order and with any duplicates, must produce equal
/// [`NoteView`]s. `tests/convergence.rs` holds that to account for both backends.
pub trait NoteDoc: Sized {
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
