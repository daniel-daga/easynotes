//! The account document: what one person thinks about their own notes.
//!
//! A note's *content* is shared with everyone who has its key. Where that note
//! sits in the grid, what colour it is, whether it is pinned, archived or in the
//! trash, and how this person likes their list drawn are not shared with anyone:
//! the design doc keeps them in a private per-user document so that one person
//! colouring a shared shopping list does not recolour it for the household. This
//! is that document.
//!
//! ## Why it is a flat map and not a map of maps
//!
//! A note's document hangs one nested map off the root per item ([`super`]), and
//! that is safe there because an item ID is random: two devices never create the
//! same one. Here they would. Two of this person's devices colouring the same
//! note would each create a container under the same key, and neither Automerge
//! nor Yrs can create a container deterministically across replicas — one side's
//! object wins and the other side's fields vanish with it.
//!
//! So the account document has no containers at all. Every field is a root key
//! with a scalar in it:
//!
//! ```text
//! "p:<note id>:colour"    -> string     a placement, one key per field
//! "p:<note id>:pinned"    -> bool
//! "p:<note id>:archived"  -> bool
//! "p:<note id>:trashed"   -> bool
//! "p:<note id>:order"     -> string     a fractional index; see crate::order
//! "s:<name>"              -> string     a setting
//! ```
//!
//! Every write is then a last-writer-wins register on a root key, which is the
//! weakest thing that can lose anything: two devices that pin and colour the same
//! note concurrently keep both, and two that colour it differently keep one
//! colour rather than half of each.
//!
//! A key this version does not understand is ignored rather than dropped, so a
//! device on an older build shows a note it cannot colour instead of deleting the
//! colour a newer one set.

use crate::error::Result;
use crate::ids::NoteId;
use std::collections::BTreeMap;

/// Keys that hold a placement field, and keys that hold a setting.
const PLACEMENT: &str = "p:";
const SETTING: &str = "s:";

const COLOUR: &str = "colour";
const PINNED: &str = "pinned";
const ARCHIVED: &str = "archived";
const TRASHED: &str = "trashed";
const ORDER: &str = "order";

/// Everything a backend has to be able to store. Deliberately two cases: the
/// account document is registers, not structure, and keeping it that way is what
/// makes the paragraph above true.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Value {
    Str(String),
    Bool(bool),
}

/// The palette. Keep's is nine colours plus "no colour", and a fixed set is what
/// lets a device render one it did not choose — there is no way to send a
/// `Color` through a CRDT and have it mean the same thing in dark mode.
///
/// A colour is written as its name, and a name this build does not know reads
/// back as no colour: a device on an older version shows a plain card rather
/// than refusing to open the note.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Colour {
    Red,
    Orange,
    Yellow,
    Green,
    Teal,
    Blue,
    Purple,
    Pink,
    Brown,
    Grey,
}

impl Colour {
    pub const ALL: [Colour; 10] = [
        Colour::Red,
        Colour::Orange,
        Colour::Yellow,
        Colour::Green,
        Colour::Teal,
        Colour::Blue,
        Colour::Purple,
        Colour::Pink,
        Colour::Brown,
        Colour::Grey,
    ];

    pub fn name(&self) -> &'static str {
        match self {
            Colour::Red => "red",
            Colour::Orange => "orange",
            Colour::Yellow => "yellow",
            Colour::Green => "green",
            Colour::Teal => "teal",
            Colour::Blue => "blue",
            Colour::Purple => "purple",
            Colour::Pink => "pink",
            Colour::Brown => "brown",
            Colour::Grey => "grey",
        }
    }

    /// `None` for a name this build does not know, which is the forward
    /// compatibility rule above.
    pub fn from_name(name: &str) -> Option<Colour> {
        Colour::ALL.into_iter().find(|c| c.name() == name)
    }
}

/// How this person likes their notes drawn. A setting rather than a placement:
/// it is about the list, not about one note.
///
/// A value this build does not know reads back as the default, for the same
/// reason an unknown colour does.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Layout {
    #[default]
    Grid,
    List,
}

impl Layout {
    pub fn name(&self) -> &'static str {
        match self {
            Layout::Grid => "grid",
            Layout::List => "list",
        }
    }

    pub fn from_name(name: &str) -> Layout {
        match name {
            "list" => Layout::List,
            _ => Layout::Grid,
        }
    }
}

/// Where one note sits for this person, and what it looks like.
///
/// The default is a note nobody has touched: no colour, not pinned, in the grid
/// rather than the archive or the trash. `order` is empty for a note that has no
/// placement yet, which sorts it to the top — [`crate::note::NoteCore`] gives
/// every note a real one as it is created, joined or carried over.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Placement {
    pub colour: Option<Colour>,
    pub pinned: bool,
    pub archived: bool,
    pub trashed: bool,
    pub order: String,
}

impl Placement {
    /// Whether this note belongs in the main grid rather than in the archive or
    /// the trash. The trash wins over the archive: a note that was archived and
    /// then thrown away is in the trash, and is in one place only.
    pub fn in_grid(&self) -> bool {
        !self.archived && !self.trashed
    }
}

/// The whole account document, as the note list needs it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AccountView {
    pub placements: BTreeMap<NoteId, Placement>,
    /// View settings, by name. Typed accessors live on
    /// [`crate::note::NoteCore`]; the document itself keeps strings so that a
    /// setting added later needs no change here.
    pub settings: BTreeMap<String, String>,
}

impl AccountView {
    pub fn placement(&self, note: NoteId) -> Placement {
        self.placements.get(&note).cloned().unwrap_or_default()
    }

    pub fn setting(&self, name: &str) -> Option<&str> {
        self.settings.get(name).map(|s| s.as_str())
    }
}

/// One change to the account document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AccountEdit {
    SetColour {
        note: NoteId,
        colour: Option<Colour>,
    },
    SetPinned {
        note: NoteId,
        pinned: bool,
    },
    SetArchived {
        note: NoteId,
        archived: bool,
    },
    SetTrashed {
        note: NoteId,
        trashed: bool,
    },
    /// Where the note sits in the grid: a fractional index, as an item's order
    /// is ([`crate::order`]).
    SetOrder {
        note: NoteId,
        order: String,
    },
    SetSetting {
        name: String,
        value: String,
    },
    /// Drop everything this person ever said about one note, because the note
    /// itself is gone.
    ///
    /// Every placement field at once, and in one transaction for the reason
    /// [`AccountDoc::write`] takes a slice: a peer must not see half a
    /// placement removed. Leaving them would grow this document for ever with
    /// keys about notes nobody can open — and the document is not compacted
    /// ([ADR 0006](../../../docs/adr/0006-compaction.md)), so every one of them
    /// would be replayed at every launch.
    ///
    /// It is **not** an undo of [`AccountEdit::SetTrashed`]: a note with no keys
    /// here reads back as the default placement, which is a note in the grid.
    /// That is the right answer only once the note has been forgotten too, which
    /// is why nothing reaches this except
    /// [`NoteCore::forget_note`](crate::note::NoteCore::forget_note).
    ForgetNote {
        note: NoteId,
    },
}

fn placement_key(note: NoteId, field: &str) -> String {
    format!("{PLACEMENT}{}:{field}", note.encode())
}

impl AccountEdit {
    /// The key writes this edit makes. `None` deletes the key.
    ///
    /// Shared by every backend, so two backends cannot disagree about what an
    /// edit means — the same reason [`super::sort_items`] is shared.
    pub fn writes(&self) -> Vec<(String, Option<Value>)> {
        match self {
            AccountEdit::SetColour { note, colour } => vec![(
                placement_key(*note, COLOUR),
                colour.map(|c| Value::Str(c.name().to_string())),
            )],
            AccountEdit::SetPinned { note, pinned } => {
                vec![(placement_key(*note, PINNED), Some(Value::Bool(*pinned)))]
            }
            AccountEdit::SetArchived { note, archived } => {
                vec![(placement_key(*note, ARCHIVED), Some(Value::Bool(*archived)))]
            }
            AccountEdit::SetTrashed { note, trashed } => {
                vec![(placement_key(*note, TRASHED), Some(Value::Bool(*trashed)))]
            }
            AccountEdit::SetOrder { note, order } => {
                vec![(placement_key(*note, ORDER), Some(Value::Str(order.clone())))]
            }
            AccountEdit::SetSetting { name, value } => {
                vec![(format!("{SETTING}{name}"), Some(Value::Str(value.clone())))]
            }
            // Named rather than collected from whatever the document happens to
            // hold, so that forgetting a note is the same five keys on a device
            // that coloured it and on one that never did.
            AccountEdit::ForgetNote { note } => [COLOUR, PINNED, ARCHIVED, TRASHED, ORDER]
                .into_iter()
                .map(|field| (placement_key(*note, field), None))
                .collect(),
        }
    }
}

/// Turn what a backend holds into what the app reads. Shared for the same reason
/// [`AccountEdit::writes`] is.
pub fn view_of(entries: impl IntoIterator<Item = (String, Value)>) -> AccountView {
    let mut view = AccountView::default();
    for (key, value) in entries {
        if let Some(name) = key.strip_prefix(SETTING) {
            if let Value::Str(s) = value {
                view.settings.insert(name.to_string(), s);
            }
            continue;
        }
        let Some(rest) = key.strip_prefix(PLACEMENT) else {
            continue; // a key from a future version of the app
        };
        let Some((note, field)) = rest.split_once(':') else {
            continue;
        };
        let Ok(note) = NoteId::decode(note) else {
            continue;
        };
        let placement = view.placements.entry(note).or_default();
        match (field, value) {
            (COLOUR, Value::Str(s)) => placement.colour = Colour::from_name(&s),
            (PINNED, Value::Bool(b)) => placement.pinned = b,
            (ARCHIVED, Value::Bool(b)) => placement.archived = b,
            (TRASHED, Value::Bool(b)) => placement.trashed = b,
            (ORDER, Value::Str(s)) => placement.order = s,
            _ => {}
        }
    }
    view
}

/// The account document's CRDT state.
///
/// Much narrower than [`super::NoteDoc`], and that is the point: a backend gets
/// to store keys and report them back, and everything that decides what a key
/// *means* is above this line. Convergence is the same requirement — any two
/// documents that have applied the same updates, in any order and with
/// duplicates, must produce equal [`AccountView`]s.
pub trait AccountDoc: Sized + Send {
    fn create(actor: &[u8; 16]) -> Self;

    fn load(actor: &[u8; 16], snapshot: &[u8]) -> Result<Self>;

    /// Apply every write in one transaction and return the update it produced.
    /// One transaction rather than one per key, so that an edit which touches
    /// several keys cannot be half-applied on a peer.
    fn write(&mut self, writes: &[(String, Option<Value>)]) -> Result<Vec<u8>>;

    /// Apply an update from another of this person's devices. Must be
    /// idempotent, like [`super::NoteDoc::apply_remote`].
    fn apply_remote(&mut self, update: &[u8]) -> Result<()>;

    /// Every key this document holds, in any order.
    fn entries(&self) -> Vec<(String, Value)>;

    fn snapshot(&self) -> Vec<u8>;

    fn view(&self) -> AccountView {
        view_of(self.entries())
    }

    /// Apply one edit. The default is the only implementation there should ever
    /// be; a backend implements [`AccountDoc::write`] and nothing about edits.
    fn apply_local(&mut self, edit: &AccountEdit) -> Result<Vec<u8>> {
        self.write(&edit.writes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_colour_survives_a_round_trip_through_its_name() {
        for colour in Colour::ALL {
            assert_eq!(Colour::from_name(colour.name()), Some(colour));
        }
    }

    #[test]
    fn a_colour_this_build_does_not_know_is_no_colour() {
        // A newer version of the app painted a note; this one draws it plain
        // rather than refusing to draw it.
        assert_eq!(Colour::from_name("coral"), None);
        let note = NoteId::random();
        let view = view_of([(
            format!("p:{}:colour", note.encode()),
            Value::Str("coral".into()),
        )]);
        assert_eq!(view.placement(note).colour, None);
    }

    #[test]
    fn an_unknown_key_is_ignored_rather_than_fatal() {
        let view = view_of([
            ("z:something".to_string(), Value::Bool(true)),
            (
                "p:not-a-note-id:colour".to_string(),
                Value::Str("red".into()),
            ),
            ("s:layout".to_string(), Value::Str("list".into())),
        ]);
        assert!(view.placements.is_empty());
        assert_eq!(view.setting("layout"), Some("list"));
    }

    #[test]
    fn forgetting_a_note_clears_every_field_it_had() {
        let note = NoteId::random();
        let writes = AccountEdit::ForgetNote { note }.writes();
        // Whatever this person had set, and not only the fields they did set:
        // the edit is the same on a device that coloured the note and on one
        // that never touched it.
        assert_eq!(writes.len(), 5);
        assert!(writes.iter().all(|(key, value)| {
            key.starts_with(&format!("p:{}:", note.encode())) && value.is_none()
        }));
        // Nothing about any other note, and no setting.
        let other = NoteId::random();
        assert!(!writes
            .iter()
            .any(|(key, _)| key.contains(&other.encode()) || key.starts_with("s:")));
    }

    #[test]
    fn a_note_with_no_placement_keys_is_a_note_in_the_grid() {
        // Which is why forgetting a note's placement is only ever right
        // alongside forgetting the note: on its own it would un-trash it.
        let note = NoteId::random();
        let view = view_of([]);
        assert!(view.placement(note).in_grid());
    }

    #[test]
    fn the_trash_wins_over_the_archive() {
        let placement = Placement {
            archived: true,
            trashed: true,
            ..Placement::default()
        };
        assert!(!placement.in_grid());
    }
}
