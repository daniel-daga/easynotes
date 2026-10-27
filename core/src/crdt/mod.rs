//! The CRDT layer, behind a trait so decision D1 stays reversible.
//!
//! the initial implementation ships two interchangeable backends, Automerge and Yrs, and a benchmark
//! that measures them (see `benches/` and `docs/decisions.md#adr-0001`). Everything above
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

use crate::attachment::{AttachmentView, Media};
use crate::error::Result;
use crate::ids::{AttachmentId, ItemId};
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
/// colour is shared for the same reason — two people looking at one list should
/// see it the same shade — while a pin or the archive is not.
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
/// rather than choosing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NoteView {
    pub kind: NoteKind,
    pub title: String,
    pub body: String,
    pub items: Vec<ItemView>,
    /// The colour everyone on this note sees, or none. `colour_set` says
    /// whether the note has recorded one at all: a note that never has falls
    /// back to each person's own placement, so colours chosen before this was
    /// shared keep showing ([ADR 0008](../../../docs/decisions.md#adr-0008)).
    pub colour: Option<Colour>,
    /// Whether [`colour`](Self::colour) came from the note document rather
    /// than from the default. A colour cleared on one device is cleared for
    /// everyone, which "no colour" alone cannot say.
    pub colour_set: bool,
    /// Live attachments, sorted by `(order, id)` like items. Removed ones are
    /// in the document as tombstones and not here. Neither kind of note owns
    /// them, so converting one never touches this.
    pub attachments: Vec<AttachmentView>,
    /// Attachments the document holds and this view does not draw: removed, or
    /// with a descriptor this build cannot read. Sorted. An ID here is spent —
    /// adding it again would write a fresh map over the tombstone and bring a
    /// removed file back for everyone — so [`crate::note::NoteCore`] refuses it.
    pub retired_attachments: Vec<AttachmentId>,
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
    /// setting, per the design, so it belongs to the UI and not to the data.
    pub fn checked_count(&self) -> usize {
        self.items.iter().filter(|i| i.checked).count()
    }

    pub fn attachment(&self, id: AttachmentId) -> Option<&AttachmentView> {
        self.attachments.iter().find(|a| a.id == id)
    }

    /// The order keys of the live attachments, for
    /// [`crate::order::key_for_index`].
    pub fn attachment_order_keys(&self) -> Vec<String> {
        self.attachments.iter().map(|a| a.order.clone()).collect()
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
    /// Record the kind and nothing else. What a new text note is created with.
    SetKind(NoteKind),
    /// A colour everyone on the note sees, or none. It is the note document's
    /// rather than a placement's because two people looking at one list should
    /// see it the same shade; whether a note is pinned, archived or labelled is
    /// still each person's own ([ADR 0008](../../../docs/decisions.md#adr-0008)).
    SetColour(Option<Colour>),
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
    /// Attach a file whose bytes are already sealed and stored. The descriptor
    /// is written once and never again, except by [`Edit::RemoveAttachment`].
    AddAttachment {
        id: AttachmentId,
        /// Boxed: a descriptor is several times the size of every other edit.
        media: Box<Media>,
        order: String,
    },
    MoveAttachment {
        id: AttachmentId,
        order: String,
    },
    SetAttachmentDescription {
        id: AttachmentId,
        description: String,
    },
    /// Remove an attachment for everyone on the note. Leaves a tombstone: the
    /// descriptor stays, marked removed and without its key, so a delayed
    /// update cannot bring it back and the relay's clean-up can still find
    /// where its bytes were.
    RemoveAttachment {
        id: AttachmentId,
    },
    /// A person's current name, signed by their own member key
    /// ([ADR 0032](../../../docs/decisions.md#adr-0032)).
    ///
    /// Shared content, like the title: a rename has to reach everybody the
    /// package named, and only a record that travels with the note can do that
    /// without the owner republishing for somebody else's name.
    SetMemberName {
        member: crate::crypto::AuthorKey,
        /// [`crate::collaborators::sign`]'s output, already encoded.
        record: String,
    },
    /// A guest's signed statement that they left the note, written while they
    /// delete their account
    /// ([ADR 0034](../../../docs/decisions.md#adr-0034)).
    ///
    /// The owner is the only one who can act on it: their next settle enqueues
    /// the removal and rotates the note key.
    SetMemberLeft {
        member: crate::crypto::AuthorKey,
        /// [`crate::collaborators::sign_left`]'s output, already encoded.
        record: String,
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
    /// one per keystroke ([ADR 0016](../../../docs/decisions.md#adr-0016)).
    /// Applying the result must leave any replica exactly where applying every
    /// input in order would, including one that has already seen some of them.
    fn merge_updates(updates: &[Vec<u8>]) -> Result<Vec<u8>>;

    fn view(&self) -> NoteView;

    /// Every member name record this document holds, as written, keyed by the
    /// member it belongs to. Verification is the caller's, because it needs
    /// the note ID and the member's key.
    fn member_names(&self) -> Vec<(crate::crypto::AuthorKey, String)>;

    /// Every leave record this document holds, as written, keyed by the member
    /// who signed it. Verification is the caller's, because it needs the note
    /// ID and the member's key.
    fn member_leaves(&self) -> Vec<(crate::crypto::AuthorKey, String)>;

    /// Every attachment object this document still names, or `None` when one
    /// of its descriptors does not say: see [`live_objects`].
    fn live_media(&self) -> Option<Vec<crate::ids::MediaObjectId>>;

    /// The full state, for the encrypted snapshot that lets a client skip the log.
    fn snapshot(&self) -> Vec<u8>;

    /// Fold a whole document, as [`NoteDoc::snapshot`] produces one, into this
    /// one. Idempotent like [`NoteDoc::apply_remote`], and it keeps everything
    /// this document already had: a device switching to a new key merges the
    /// snapshot rather than replacing its copy with it
    /// ([ADR 0023](../../../docs/decisions.md#adr-0023)).
    fn merge_snapshot(&mut self, snapshot: &[u8]) -> Result<()>;

    /// One update holding everything this document has that `base` — a
    /// snapshot this document has already merged — lacks, or `None` when it
    /// has nothing more. What a device re-sends under a new key: its own
    /// offline typing, and edits it received just before the switch.
    fn missing_from(&self, base: &[u8]) -> Result<Option<Vec<u8>>>;

    fn backend_name() -> &'static str;
}

/// The root key an attachment's map hangs off. The `a:` prefix cannot occur in
/// an item ID — `:` is not base64url — so an item and an attachment can share
/// the root without either being mistaken for the other, and a build that
/// predates attachments skips these keys as it skips any it cannot decode.
pub(crate) fn attachment_key(id: AttachmentId) -> String {
    format!("a:{}", id.encode())
}

pub(crate) fn parse_attachment_key(key: &str) -> Option<AttachmentId> {
    AttachmentId::decode(key.strip_prefix("a:")?).ok()
}

/// The root key one person's name record hangs off. `n:` cannot occur in an
/// item ID — `:` is not base64url — so an item and a name record can share the
/// root without either being mistaken for the other.
pub(crate) fn name_key(member: crate::crypto::AuthorKey) -> String {
    format!("n:{}", member.encode())
}

pub(crate) fn parse_name_key(key: &str) -> Option<crate::crypto::AuthorKey> {
    let raw = key.strip_prefix("n:")?;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;
    let bytes: [u8; 32] = URL_SAFE_NO_PAD.decode(raw).ok()?.try_into().ok()?;
    Some(crate::crypto::AuthorKey(bytes))
}

/// The root key one person's leave record hangs off. A separate prefix from
/// the name record's so an old build — and a note's view — can tell them apart.
pub(crate) fn left_key(member: crate::crypto::AuthorKey) -> String {
    format!("l:{}", member.encode())
}

pub(crate) fn parse_left_key(key: &str) -> Option<crate::crypto::AuthorKey> {
    let raw = key.strip_prefix("l:")?;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;
    let bytes: [u8; 32] = URL_SAFE_NO_PAD.decode(raw).ok()?.try_into().ok()?;
    Some(crate::crypto::AuthorKey(bytes))
}

/// The fields of an attachment's map. Shared so the two backends cannot spell
/// them differently.
pub(crate) mod attachment_fields {
    /// The descriptor, as [`crate::attachment::Media::encode`] writes it.
    pub const MEDIA: &str = "media";
    pub const ORDER: &str = "order";
    pub const DESCRIPTION: &str = "description";
    /// `true` once removed, and never written `false`: removal is monotonic,
    /// so it wins over a concurrent move or description on another device.
    pub const REMOVED: &str = "removed";
}

/// Turn one attachment's fields into what a screen sees, or `None` for one it
/// should not: removed, or with a descriptor this build cannot read or trust.
pub(crate) fn attachment_view(
    id: AttachmentId,
    media: &str,
    order: String,
    description: String,
    removed: bool,
) -> Option<AttachmentView> {
    if removed {
        return None;
    }
    let media = Media::decode(media)?;
    // A live descriptor without a key is one nobody can open; drawing it would
    // be a placeholder that never fills.
    media.key.as_ref()?;
    Some(AttachmentView {
        id,
        order,
        description,
        media,
    })
}

/// The attachment objects a document still names, from each attachment map's
/// `media` field and `removed` flag: what a compaction lists for the relay
/// to keep ([ADR 0027](../../../docs/decisions.md#adr-0027)).
///
/// Read for the object IDs alone, not through [`Media::decode`]: a descriptor
/// this build cannot read may be a newer build's live one, and its bytes are
/// not this device's to let go. A map that is not removed and does not yield
/// even its object IDs makes the whole answer `None`, which lists nothing and
/// so lets nothing go. Never more than
/// [`crate::compaction::MAX_LIVE_OBJECTS`], for the same reason.
#[cfg(any(feature = "automerge-backend", feature = "yrs-backend"))]
pub(crate) fn live_objects(
    maps: impl IntoIterator<Item = (String, bool)>,
) -> Option<Vec<crate::ids::MediaObjectId>> {
    #[derive(serde::Deserialize)]
    struct Named {
        object: crate::ids::MediaObjectId,
    }
    #[derive(serde::Deserialize)]
    struct Thumbnail {
        media: Named,
    }
    #[derive(serde::Deserialize)]
    struct Objects {
        original: Named,
        thumbnail: Option<Thumbnail>,
    }
    let mut live = Vec::new();
    for (media, removed) in maps {
        if removed {
            continue;
        }
        let objects: Objects = serde_json::from_str(&media).ok()?;
        live.push(objects.original.object);
        live.extend(objects.thumbnail.map(|t| t.media.object));
    }
    live.sort();
    live.dedup();
    (live.len() <= crate::compaction::MAX_LIVE_OBJECTS).then_some(live)
}

#[cfg(any(feature = "automerge-backend", feature = "yrs-backend"))]
pub(crate) fn sort_attachments(attachments: &mut [AttachmentView]) {
    attachments.sort_by(|a, b| a.order.cmp(&b.order).then_with(|| a.id.cmp(&b.id)));
}

/// Sort items into display order. Both backends share this so a difference
/// between them can never be a difference in sorting.
#[cfg(any(feature = "automerge-backend", feature = "yrs-backend"))]
pub(crate) fn sort_items(items: &mut [ItemView]) {
    items.sort_by(|a, b| a.order.cmp(&b.order).then_with(|| a.id.cmp(&b.id)));
}

/// The backend the app uses. See [ADR 0001](../../../docs/decisions.md#adr-0001).
///
/// Everything outside this module refers to the alias, never the concrete type,
/// so revisiting D1 means changing this line and re-running the suite. The
/// account document follows it, through [`NoteDoc::Account`].
#[cfg(feature = "yrs-backend")]
pub type DefaultNoteDoc = YrsNote;
#[cfg(all(not(feature = "yrs-backend"), feature = "automerge-backend"))]
pub type DefaultNoteDoc = AutomergeNote;

#[cfg(all(test, any(feature = "automerge-backend", feature = "yrs-backend")))]
mod live_objects_tests {
    use super::live_objects;
    use crate::ids::MediaObjectId;

    fn id(byte: u8) -> MediaObjectId {
        MediaObjectId::from_bytes([byte; 16])
    }

    fn descriptor(original: u8, thumbnail: Option<u8>) -> String {
        let named = |b: u8| serde_json::json!({ "object": id(b) });
        serde_json::json!({
            "v": 1,
            "original": named(original),
            "thumbnail": thumbnail.map(|t| serde_json::json!({ "media": named(t) })),
        })
        .to_string()
    }

    #[test]
    fn removed_maps_are_left_out_and_the_rest_are_listed_once() {
        let maps = [
            (descriptor(3, Some(4)), false),
            (descriptor(1, None), true),
            (descriptor(3, None), false),
        ];
        assert_eq!(live_objects(maps), Some(vec![id(3), id(4)]));
    }

    #[test]
    fn a_descriptor_from_a_newer_build_still_keeps_its_objects() {
        // A version and fields this build has never heard of: not something
        // it can draw, but its bytes are still somebody's.
        let newer = serde_json::json!({
            "v": 9,
            "format": "holo",
            "original": { "object": id(5), "layers": 3 },
            "thumbnail": null,
        })
        .to_string();
        assert_eq!(live_objects([(newer, false)]), Some(vec![id(5)]));
    }

    #[test]
    fn a_live_map_without_object_ids_lists_nothing_at_all() {
        let maps = [
            (descriptor(3, None), false),
            (r#"{"v":9,"elsewhere":true}"#.to_string(), false),
        ];
        assert_eq!(live_objects(maps), None);
        // The same map removed says nothing about anything.
        let maps = [(String::new(), true), (descriptor(3, None), false)];
        assert_eq!(live_objects(maps), Some(vec![id(3)]));
    }

    #[test]
    fn a_list_past_the_cap_is_no_list() {
        let maps = (0..=crate::compaction::MAX_LIVE_OBJECTS).map(|n| {
            let object = MediaObjectId::from_bytes((n as u128).to_be_bytes());
            (
                serde_json::json!({ "original": { "object": object } }).to_string(),
                false,
            )
        });
        assert_eq!(live_objects(maps), None);
    }
}
