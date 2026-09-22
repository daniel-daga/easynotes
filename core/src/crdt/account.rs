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
//! "p:<note id>:colour"      -> string   a placement, one key per field
//! "p:<note id>:pinned"      -> bool
//! "p:<note id>:archived"    -> bool
//! "p:<note id>:trashed"     -> bool
//! "p:<note id>:order"       -> string   a fractional index; see crate::order
//! "l:<label id>"            -> string   a label's name
//! "m:<note id>:<label id>"  -> bool     that note carries that label
//! "s:<name>"                -> string   a setting
//! "k:<note id>"             -> string   that note's key, base64url
//!
//! "t:<note id>:<name>"      -> bool     a label, as it used to be stored
//! ```
//!
//! Every write is then a last-writer-wins register on a root key, which is the
//! weakest thing that can lose anything: two devices that pin and colour the same
//! note concurrently keep both, and two that colour it differently keep one
//! colour rather than half of each.
//!
//! ## Why a label has an identity, and why membership is a key per pair
//!
//! A note's labels are a *set*, and the obvious shape — one key per note holding
//! `"errands,recipes"` — is the one that loses: two devices adding two different
//! labels to the same note would each write the whole set, and last-writer-wins
//! would keep one of the two additions. A key per `(note, label)` makes each
//! membership its own register, so those two devices keep both labels for the
//! same reason they keep a pin and a colour.
//!
//! What the pair is made of changed. A label used to *be* its name, so the
//! membership key was `t:<note>:<name>` and there was nothing else — which meant
//! a label could not be renamed (every note carrying it would have to be
//! rewritten, and a peer replaying that would see every note lose its label and
//! gain a different one) and could not exist without a note (there was nowhere to
//! put it). A label has an ID now, `l:<id>` holds its name, and `m:<note>:<id>`
//! is the membership. Renaming is one register write that every device converges
//! on exactly as it converges on a colour, and a label with no notes is a name
//! key with no membership keys, which is a perfectly ordinary thing for this
//! document to hold.
//!
//! Taking a label off **deletes** the membership key rather than writing
//! `false`. Nothing compacts a note's document
//! ([ADR 0006](../../../docs/adr/0006-compaction.md)) and this one is compacted
//! only at a threshold ([ADR 0012](../../../docs/adr/0012-compacting-the-account-document.md)),
//! so a `false` left behind is a key replayed at every launch, about a label this
//! person has stopped using — and unlike the five placement fields, which are
//! bounded, the labels one note could accumulate are not. The price is the one
//! case a register does not cover: one device deleting a membership while another
//! re-adds it resolves to whichever the backend prefers rather than to a rule of
//! ours, and the answer is a label that is on or off, never half of one.
//!
//! `t:` keys are what the old shape wrote, and they are still read.
//! [`NoteCore::open`](crate::note::NoteCore::open) turns each of them into a real
//! label the first time it sees one and removes them; until it has, they are
//! memberships of a label that has no ID yet, and [`view_of`] reports them as
//! such so that nothing between here and that migration has to know.
//!
//! ## Why a note's key is in here
//!
//! This document is the only thing one person's two devices share that nobody
//! else can read, so it is the only place a note key can travel between them.
//! A `k:` register per note is what makes a second device adopted from a
//! recovery phrase able to *open* the notes it syncs, rather than merely to
//! hold their ciphertext ([ADR
//! 0013](../../../docs/adr/0013-recovery-phrase.md)).
//!
//! These are registers like any other, which is what keeps item 4's rotation
//! possible: rotating a note's key is writing a new value here, not a migration.
//! Two devices that somehow wrote different keys for one note would converge on
//! one of them — that is last-writer-wins doing exactly what it does everywhere
//! else in this document, and the losing key is not lost, because a key only
//! ever gets here from the device that minted it or from a share blob.
//!
//! **[`view_of`] ignores them, on purpose.** An [`AccountView`] is handed out by
//! [`NoteCore::account`](crate::note::NoteCore::account), cloned on every
//! change and printed by `Debug`, and key material has no business in a
//! structure like that. The keys come out through [`note_keys_of`] instead,
//! which has exactly one caller.
//!
//! A key this version does not understand is ignored rather than dropped, so a
//! device on an older build shows a note it cannot colour instead of deleting the
//! colour a newer one set.

use crate::error::Result;
use crate::ids::{LabelId, NoteId, ReminderId};
use std::collections::{BTreeMap, BTreeSet};

/// Keys that hold a placement field, a label's name, a membership, and a
/// setting — plus the shape a membership used to have.
const PLACEMENT: &str = "p:";
const LABEL: &str = "l:";
const MEMBER: &str = "m:";
const SETTING: &str = "s:";
/// A note's key, so that this person's other devices can open the note and not
/// merely store it. See the module docs.
const NOTE_KEY: &str = "k:";
/// A reminder's scalar fields. Reminders deliberately have no container: two
/// devices may create or amend them at once, so they follow the account
/// document's flat-register rule.
const REMINDER: &str = "r:";
/// A membership as it was stored before a label had an ID. Read, migrated and
/// never written. See the module docs.
const LEGACY_TAG: &str = "t:";

const COLOUR: &str = "colour";
const PINNED: &str = "pinned";
const ARCHIVED: &str = "archived";
const TRASHED: &str = "trashed";
const ORDER: &str = "order";
const REMINDER_NOTE: &str = "note";
const REMINDER_FIRE_AT: &str = "fire-at";
const REMINDER_REPEAT: &str = "repeat";
const REMINDER_TIMEZONE: &str = "timezone";

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
    /// The labels this person has put on the note, by ID. Resolve a name with
    /// [`AccountView::label_name`]; the IDs are here rather than the names
    /// because a name can change and a membership must not have to.
    pub tags: BTreeSet<LabelId>,
    /// Memberships still in the old shape, which named the label instead of
    /// pointing at one.
    ///
    /// Empty on any device that has opened since the migration ran, which is
    /// every device after its first launch on this build — so nothing above the
    /// core reads this, and [`AccountView::labels_on`] folds it in for the one
    /// launch where it is not yet empty. Kept separate rather than mixed in with
    /// `tags` so that the migration has something exact to work from.
    pub legacy_tags: BTreeSet<String>,
}

/// How a reminder repeats. The platform scheduler interprets its date and time
/// in the reminder's IANA timezone.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum RepeatRule {
    #[default]
    Never,
    Daily,
    Weekly,
    Monthly,
    Yearly,
}

impl RepeatRule {
    pub fn name(self) -> &'static str {
        match self {
            Self::Never => "never",
            Self::Daily => "daily",
            Self::Weekly => "weekly",
            Self::Monthly => "monthly",
            Self::Yearly => "yearly",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "never" => Some(Self::Never),
            "daily" => Some(Self::Daily),
            "weekly" => Some(Self::Weekly),
            "monthly" => Some(Self::Monthly),
            "yearly" => Some(Self::Yearly),
            _ => None,
        }
    }
}

/// One complete reminder. An incomplete record is intentionally absent from
/// [`AccountView::reminders`]: fields can arrive out of order from the relay,
/// and scheduling a made-up time or note would be worse than waiting.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Reminder {
    pub id: ReminderId,
    pub note: NoteId,
    /// Milliseconds since the Unix epoch, stored as a string scalar so it is
    /// exact on the Dart/JavaScript boundary.
    pub fire_at: i64,
    pub repeat: RepeatRule,
    /// IANA name, for example `Europe/Vienna`.
    pub timezone: String,
}

impl Placement {
    /// Whether this note belongs in the main grid rather than in the archive or
    /// the trash. The trash wins over the archive: a note that was archived and
    /// then thrown away is in the trash, and is in one place only.
    pub fn in_grid(&self) -> bool {
        !self.archived && !self.trashed
    }
}

/// The longest a tag name may be.
///
/// A label, not a note: it has to fit on a chip on a card, and something that
/// does not is text that belongs in the note itself. The UI's field stops at the
/// same number, so [`normalise_tag`] truncating rather than refusing is the last
/// line rather than the usual one — and truncating is the kinder end of a
/// gesture that would otherwise silently do nothing.
pub const MAX_TAG_LEN: usize = 48;

/// What a tag name looks like once it is stored, or `None` for one that is not a
/// name at all.
///
/// Whitespace is trimmed and any run of it inside collapses to one space, so
/// `" weekly  shop "` and `"weekly shop"` are the same label rather than two
/// that look identical on a chip. Case is **kept**: a person who types
/// `Recipes` sees `Recipes`. Matching an existing label is case-insensitive and
/// happens a layer up, in
/// [`NoteCore::add_tag`](crate::note::NoteCore::add_tag), because that is the
/// layer that knows which labels this device already has.
pub fn normalise_tag(raw: &str) -> Option<String> {
    let mut name = String::new();
    for word in raw.split_whitespace() {
        if !name.is_empty() {
            name.push(' ');
        }
        name.push_str(word);
    }
    if name.is_empty() {
        return None;
    }
    // By characters, not bytes: the boundary has to be one a `String` can be cut
    // at, and "48 bytes" through the middle of an emoji is not one.
    Some(name.chars().take(MAX_TAG_LEN).collect())
}

/// The whole account document, as the note list needs it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AccountView {
    pub placements: BTreeMap<NoteId, Placement>,
    /// Every label this person has, by ID, whether or not a note carries it.
    ///
    /// A label outlives its last note now: it is a name key of its own, and
    /// taking it off the last note removes a membership rather than the label.
    /// Keep does the same, and the alternative — a label vanishing because
    /// somebody tidied up the one note that had it — is a thing to explain.
    pub labels: BTreeMap<LabelId, String>,
    /// Complete reminders, by ID. They are private account data, never fields
    /// on a shared note.
    pub reminders: BTreeMap<ReminderId, Reminder>,
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

    /// What a label is called, or `None` for an ID no name key answers for.
    ///
    /// `None` is reachable and is not a bug: a membership and its label's name
    /// are separate keys, so a peer can deliver one before the other, and a
    /// device on an older build could delete a name while this one holds a
    /// membership. The caller's job is to skip it rather than to invent a name,
    /// which is what [`AccountView::labels`] listing only named labels does for
    /// it.
    pub fn label_name(&self, label: LabelId) -> Option<&str> {
        self.labels.get(&label).map(|s| s.as_str())
    }

    /// A label whose name matches, ignoring ASCII case.
    ///
    /// ASCII-insensitive and not Unicode-insensitive, deliberately: a full case
    /// fold depends on a locale — Turkish dotless i is the standing example —
    /// and a label that is the same one on a phone and a different one on a
    /// laptop is worse than one that is only ever folded where every device
    /// folds it the same way.
    ///
    /// Ties are broken by ID so that two labels with the same name — which two
    /// devices editing offline can produce — resolve to the same one of the pair
    /// on every device that asks.
    pub fn label_named(&self, name: &str) -> Option<LabelId> {
        self.labels
            .iter()
            .filter(|(_, held)| held.eq_ignore_ascii_case(name))
            .map(|(id, _)| *id)
            .min()
    }

    /// The labels on a note, by ID, including any the migration has not reached
    /// yet. Only labels this document has a name for.
    pub fn labels_on(&self, note: NoteId) -> Vec<LabelId> {
        let Some(placement) = self.placements.get(&note) else {
            return Vec::new();
        };
        let mut out: Vec<LabelId> = placement
            .tags
            .iter()
            .copied()
            .filter(|id| self.labels.contains_key(id))
            .collect();
        // A legacy membership names its label, so the label it means is the one
        // with that name — if the migration has already made it. Before that it
        // contributes nothing, which is the same answer `notes` gives for a note
        // it cannot open: nothing to show rather than something invented.
        for name in &placement.legacy_tags {
            if let Some(id) = self.label_named(name) {
                if !out.contains(&id) {
                    out.push(id);
                }
            }
        }
        out.sort();
        out
    }

    /// Every label this person has, with how many notes carry each, sorted the
    /// way a menu wants them.
    ///
    /// Case-insensitively by name, then by the name itself, then by ID — so that
    /// `Recipes` and `recipes`, which only two devices editing offline can
    /// produce, hold still rather than swapping places between launches.
    ///
    /// A count of zero is a real answer, not a filtered-out one: a label with no
    /// notes exists and is offerable. That is the half of this that changed when
    /// a label stopped being its own name.
    pub fn tags(&self) -> Vec<(LabelId, String, usize)> {
        let mut counts: BTreeMap<LabelId, usize> = self.labels.keys().map(|id| (*id, 0)).collect();
        for note in self.placements.keys() {
            for id in self.labels_on(*note) {
                *counts.entry(id).or_default() += 1;
            }
        }
        let mut tags: Vec<(LabelId, String, usize)> = counts
            .into_iter()
            .filter_map(|(id, count)| self.labels.get(&id).map(|name| (id, name.clone(), count)))
            .collect();
        tags.sort_by(|a, b| {
            a.1.to_lowercase()
                .cmp(&b.1.to_lowercase())
                .then_with(|| a.1.cmp(&b.1))
                .then_with(|| a.0.cmp(&b.0))
        });
        tags
    }

    /// Every reminder on one note, in due-time then ID order.
    pub fn reminders_on(&self, note: NoteId) -> Vec<Reminder> {
        let mut reminders: Vec<_> = self
            .reminders
            .values()
            .filter(|reminder| reminder.note == note)
            .cloned()
            .collect();
        reminders.sort_by_key(|reminder| (reminder.fire_at, reminder.id));
        reminders
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
    /// Put a label on a note, or take it off. One key, so two devices adding two
    /// labels to one note keep both.
    SetTag {
        note: NoteId,
        label: LabelId,
        on: bool,
    },
    /// Name a label, or rename one. The same register write either way, which is
    /// the whole point of a label having an ID: two devices renaming the same
    /// label converge on one name, and neither note loses its membership.
    SetLabelName {
        label: LabelId,
        /// Already through [`normalise_tag`]: this is the name as it is stored.
        name: String,
    },
    /// Remove a label everywhere: its name, and its membership of every note
    /// that carries it.
    ///
    /// One transaction for the reason [`AccountEdit::ForgetNote`] is one: a peer
    /// that saw the name go and the memberships stay would hold memberships of a
    /// label with no name, which [`AccountView::labels_on`] would then drop —
    /// silently taking labels off notes that nobody asked about.
    ForgetLabel {
        label: LabelId,
        /// Every note that carries it, which the caller reads off
        /// [`AccountView`] because the keys cannot be named from nothing.
        notes: Vec<NoteId>,
    },
    /// Drop a membership stored in the shape that named its label, which is what
    /// migrating one off the old `t:` keys takes.
    DropLegacyTag {
        note: NoteId,
        tag: String,
    },
    SetSetting {
        name: String,
        value: String,
    },
    /// Record a note's key, so this person's other devices can open it.
    ///
    /// Written when a note is created and when one arrives by share blob —
    /// the two moments a key enters this device — and carried as base64url
    /// because the document stores scalars and a `Value::Str` is the scalar a
    /// backend already has. The bytes are no more exposed for being text: this
    /// document is sealed under a key only the recovery phrase derives.
    SetNoteKey {
        note: NoteId,
        /// base64url, no padding: 43 characters for a 32-byte key.
        key: String,
    },
    /// Create or amend all fields in one CRDT transaction. The individual
    /// fields are registers so independent concurrent amendments survive.
    SetReminder {
        reminder: Reminder,
    },
    /// Delete every field belonging to one reminder.
    ForgetReminder {
        reminder: ReminderId,
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
        /// The labels the note currently has, because they are the one part of a
        /// placement that is not a fixed set of keys and so cannot be named from
        /// nothing. The caller reads them off
        /// [`AccountView::placement`]; a note this device never labelled passes
        /// an empty list and removes the same five keys it always did.
        ///
        /// The labels themselves are **not** removed. A label outlives the notes
        /// that carried it, so forgetting the last note with a label leaves a
        /// label with no notes rather than taking it away — which is
        /// [`AccountEdit::ForgetLabel`]'s job, and only when somebody asks.
        tags: Vec<LabelId>,
        /// Memberships the migration has not reached yet, named rather than
        /// pointed at. Empty on any device past its first launch on this build.
        legacy_tags: Vec<String>,
        /// The variable-size reminder set associated with this note.
        reminders: Vec<ReminderId>,
    },
}

fn placement_key(note: NoteId, field: &str) -> String {
    format!("{PLACEMENT}{}:{field}", note.encode())
}

fn member_key(note: NoteId, label: LabelId) -> String {
    format!("{MEMBER}{}:{}", note.encode(), label.encode())
}

fn note_key_key(note: NoteId) -> String {
    format!("{NOTE_KEY}{}", note.encode())
}

fn reminder_key(reminder: ReminderId, field: &str) -> String {
    format!("{REMINDER}{}:{field}", reminder.encode())
}

fn label_key(label: LabelId) -> String {
    format!("{LABEL}{}", label.encode())
}

/// A colon in a label's name needs no escaping: the note ID is fixed-width and
/// the name is everything after the *first* colon, so `split_once` gives it back
/// whole. Only ever used to *remove* one of these now.
fn legacy_tag_key(note: NoteId, tag: &str) -> String {
    format!("{LEGACY_TAG}{}:{tag}", note.encode())
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
            // `None` rather than `Bool(false)`, for the reason the module docs
            // give: the labels one note might accumulate are not bounded, and a
            // `false` left behind is replayed at every launch about a label
            // nobody uses.
            AccountEdit::SetTag { note, label, on } => {
                vec![(member_key(*note, *label), on.then_some(Value::Bool(true)))]
            }
            AccountEdit::SetLabelName { label, name } => {
                vec![(label_key(*label), Some(Value::Str(name.clone())))]
            }
            // The name last, so that a peer applying this by halves — which it
            // cannot, since `write` is one transaction, but the order is what
            // documents the intent — loses memberships before it loses the name
            // that explains them.
            AccountEdit::ForgetLabel { label, notes } => notes
                .iter()
                .map(|note| (member_key(*note, *label), None))
                .chain(std::iter::once((label_key(*label), None)))
                .collect(),
            AccountEdit::DropLegacyTag { note, tag } => {
                vec![(legacy_tag_key(*note, tag), None)]
            }
            AccountEdit::SetSetting { name, value } => {
                vec![(format!("{SETTING}{name}"), Some(Value::Str(value.clone())))]
            }
            AccountEdit::SetNoteKey { note, key } => {
                vec![(note_key_key(*note), Some(Value::Str(key.clone())))]
            }
            AccountEdit::SetReminder { reminder } => vec![
                (
                    reminder_key(reminder.id, REMINDER_NOTE),
                    Some(Value::Str(reminder.note.encode())),
                ),
                (
                    reminder_key(reminder.id, REMINDER_FIRE_AT),
                    Some(Value::Str(reminder.fire_at.to_string())),
                ),
                (
                    reminder_key(reminder.id, REMINDER_REPEAT),
                    Some(Value::Str(reminder.repeat.name().to_string())),
                ),
                (
                    reminder_key(reminder.id, REMINDER_TIMEZONE),
                    Some(Value::Str(reminder.timezone.clone())),
                ),
            ],
            AccountEdit::ForgetReminder { reminder } => [
                REMINDER_NOTE,
                REMINDER_FIRE_AT,
                REMINDER_REPEAT,
                REMINDER_TIMEZONE,
            ]
            .into_iter()
            .map(|field| (reminder_key(*reminder, field), None))
            .collect(),
            // The five fields are named rather than collected from whatever the
            // document happens to hold, so that forgetting a note is the same
            // five keys on a device that coloured it and on one that never did.
            // The tags cannot be: they are named by the caller, which is the
            // only thing that knows them.
            AccountEdit::ForgetNote {
                note,
                tags,
                legacy_tags,
                reminders,
            } => [COLOUR, PINNED, ARCHIVED, TRASHED, ORDER]
                .into_iter()
                .map(|field| (placement_key(*note, field), None))
                // The key goes with them. A note that has been forgotten is one
                // this person cannot open on *any* of their devices, and a key
                // register left behind would hand it back to the next device
                // they set up — which is precisely the promise ADR 0010's
                // `forget` makes and this document would quietly break.
                .chain(std::iter::once((note_key_key(*note), None)))
                .chain(tags.iter().map(|label| (member_key(*note, *label), None)))
                .chain(
                    legacy_tags
                        .iter()
                        .map(|tag| (legacy_tag_key(*note, tag), None)),
                )
                .chain(reminders.iter().flat_map(|reminder| {
                    [
                        REMINDER_NOTE,
                        REMINDER_FIRE_AT,
                        REMINDER_REPEAT,
                        REMINDER_TIMEZONE,
                    ]
                    .into_iter()
                    .map(move |field| (reminder_key(*reminder, field), None))
                }))
                .collect(),
        }
    }
}

/// The membership key the old shape wrote, for the one caller that still has to
/// produce one: a test standing a device up in the state the migration exists
/// for. Nothing in the app writes this, and `AccountEdit` has no variant that
/// does — retiring a shape means not being able to write it by accident.
#[doc(hidden)]
pub fn legacy_tag_write(note: NoteId, tag: &str) -> (String, Option<Value>) {
    (legacy_tag_key(note, tag), Some(Value::Bool(true)))
}

/// The note keys this document carries, for the one caller that needs them.
///
/// Deliberately not part of [`view_of`], and deliberately not a field on
/// [`AccountView`]. That view is cloned on every change, handed out by
/// [`NoteCore::account`](crate::note::NoteCore::account) and derives `Debug`;
/// key material in it would be key material in a log line one day. This
/// function is called once, at the moment a device reconciles what the document
/// knows with what its keyring holds, and the result goes straight into the
/// keyring.
///
/// A malformed or wrong-length value is skipped rather than fatal, for the
/// reason an unknown key is kept: a build that writes something else here must
/// not be able to stop this one from opening the notes it *can* read.
#[derive(Clone)]
pub struct NoteAccess {
    pub key: crate::crypto::NoteKey,
    /// The device that created the note and is allowed to rotate its key.
    /// `None` is a legacy share whose owner was never recorded.
    pub owner: Option<crate::crypto::AuthorKey>,
    pub owned: bool,
    pub generation: u64,
    /// Present only in the creator's account document. Guests receive the
    /// public `owner` half in their grant, never this signing seed.
    pub rotation_seed: Option<crate::crypto::DeviceSeed>,
}

pub fn note_access_of(
    entries: impl IntoIterator<Item = (String, Value)>,
) -> BTreeMap<NoteId, NoteAccess> {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;

    let mut keys = BTreeMap::new();
    for (key, value) in entries {
        let (Some(id), Value::Str(encoded)) = (key.strip_prefix(NOTE_KEY), value) else {
            continue;
        };
        let (Ok(note), Ok(raw)) = (NoteId::decode(id), URL_SAFE_NO_PAD.decode(&encoded)) else {
            continue;
        };
        let access = if raw.len() == crate::crypto::NOTE_KEY_LEN {
            NoteAccess {
                key: crate::crypto::NoteKey(raw.as_slice().try_into().unwrap()),
                owner: None,
                owned: false,
                generation: 0,
                rotation_seed: None,
            }
        } else if raw.len() >= 2 + crate::crypto::NOTE_KEY_LEN + crate::crypto::PUBKEY_LEN + 8
            && raw[0] == 2
        {
            let base = 2 + crate::crypto::NOTE_KEY_LEN + crate::crypto::PUBKEY_LEN + 8;
            let owned = raw[1 + crate::crypto::NOTE_KEY_LEN + crate::crypto::PUBKEY_LEN] == 1;
            if raw.len() != base + if owned { 64 } else { 0 } {
                continue;
            }
            NoteAccess {
                key: crate::crypto::NoteKey(
                    raw[1..1 + crate::crypto::NOTE_KEY_LEN].try_into().unwrap(),
                ),
                owner: Some(crate::crypto::AuthorKey(
                    raw[1 + crate::crypto::NOTE_KEY_LEN
                        ..1 + crate::crypto::NOTE_KEY_LEN + crate::crypto::PUBKEY_LEN]
                        .try_into()
                        .unwrap(),
                )),
                owned,
                generation: u64::from_be_bytes(
                    raw[2 + crate::crypto::NOTE_KEY_LEN + crate::crypto::PUBKEY_LEN..base]
                        .try_into()
                        .unwrap(),
                ),
                rotation_seed: owned
                    .then(|| crate::crypto::DeviceSeed(raw[base..].try_into().unwrap())),
            }
        } else {
            continue;
        };
        keys.insert(note, access);
    }
    keys
}

pub fn note_keys_of(
    entries: impl IntoIterator<Item = (String, Value)>,
) -> BTreeMap<NoteId, crate::crypto::NoteKey> {
    note_access_of(entries)
        .into_iter()
        .map(|(note, access)| (note, access.key))
        .collect()
}

/// Write a note's key into the document, as [`AccountEdit::SetNoteKey`] wants it.
pub fn encode_note_key(key: &crate::crypto::NoteKey) -> String {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;
    URL_SAFE_NO_PAD.encode(key.0)
}

pub fn encode_note_access(
    key: &crate::crypto::NoteKey,
    owner: crate::crypto::AuthorKey,
    owned: bool,
    generation: u64,
    rotation_seed: Option<&crate::crypto::DeviceSeed>,
) -> String {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;
    let mut raw = Vec::with_capacity(
        2 + crate::crypto::NOTE_KEY_LEN
            + crate::crypto::PUBKEY_LEN
            + 8
            + rotation_seed.map_or(0, |_| 64),
    );
    raw.push(2);
    raw.extend_from_slice(&key.0);
    raw.extend_from_slice(&owner.0);
    raw.push(u8::from(owned));
    raw.extend_from_slice(&generation.to_be_bytes());
    if let Some(seed) = rotation_seed {
        raw.extend_from_slice(&seed.0);
    }
    URL_SAFE_NO_PAD.encode(raw)
}

/// Turn what a backend holds into what the app reads. Shared for the same reason
/// [`AccountEdit::writes`] is.
///
/// `k:` keys fall through to the "a key from a future version" arm at the
/// bottom and are ignored, which is what keeps note keys out of this structure.
/// See [`note_keys_of`].
pub fn view_of(entries: impl IntoIterator<Item = (String, Value)>) -> AccountView {
    let mut view = AccountView::default();
    let mut reminder_fields: BTreeMap<ReminderId, BTreeMap<String, String>> = BTreeMap::new();
    for (key, value) in entries {
        if let Some(rest) = key.strip_prefix(REMINDER) {
            let Some((id, field)) = rest.split_once(':') else {
                continue;
            };
            let (Ok(id), Value::Str(value)) = (ReminderId::decode(id), value) else {
                continue;
            };
            reminder_fields
                .entry(id)
                .or_default()
                .insert(field.to_string(), value);
            continue;
        }
        if let Some(name) = key.strip_prefix(SETTING) {
            if let Value::Str(s) = value {
                view.settings.insert(name.to_string(), s);
            }
            continue;
        }
        if let Some(id) = key.strip_prefix(LABEL) {
            let (Ok(id), Value::Str(name)) = (LabelId::decode(id), value) else {
                continue;
            };
            // An empty name is not a label. `normalise_tag` cannot produce one,
            // so this is only reachable from a build that does not exist yet,
            // and dropping it is better than a nameless chip.
            if !name.is_empty() {
                view.labels.insert(id, name);
            }
            continue;
        }
        if let Some(rest) = key.strip_prefix(MEMBER) {
            // A membership key holds `true` and exists only while the label is
            // on the note, so a `false` — which this build never writes, and an
            // older or newer one might — is a note without that label.
            let Some((note, label)) = rest.split_once(':') else {
                continue;
            };
            let (Ok(note), Ok(label)) = (NoteId::decode(note), LabelId::decode(label)) else {
                continue;
            };
            if value == Value::Bool(true) {
                view.placements.entry(note).or_default().tags.insert(label);
            }
            continue;
        }
        if let Some(rest) = key.strip_prefix(LEGACY_TAG) {
            // The shape a membership had before a label had an ID. Reported as
            // its own field so that the migration has something exact to work
            // from and everything else can ignore it.
            let Some((note, tag)) = rest.split_once(':') else {
                continue;
            };
            let Ok(note) = NoteId::decode(note) else {
                continue;
            };
            if value == Value::Bool(true) && !tag.is_empty() {
                view.placements
                    .entry(note)
                    .or_default()
                    .legacy_tags
                    .insert(tag.to_string());
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
    for (id, fields) in reminder_fields {
        let Some(note) = fields
            .get(REMINDER_NOTE)
            .and_then(|value| NoteId::decode(value).ok())
        else {
            continue;
        };
        let Some(fire_at) = fields
            .get(REMINDER_FIRE_AT)
            .and_then(|value| value.parse::<i64>().ok())
            .filter(|value| *value >= 0)
        else {
            continue;
        };
        let Some(repeat) = fields
            .get(REMINDER_REPEAT)
            .and_then(|value| RepeatRule::from_name(value))
        else {
            continue;
        };
        let Some(timezone) = fields
            .get(REMINDER_TIMEZONE)
            .filter(|value| !value.trim().is_empty())
        else {
            continue;
        };
        view.reminders.insert(
            id,
            Reminder {
                id,
                note,
                fire_at,
                repeat,
                timezone: timezone.clone(),
            },
        );
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
        let writes = AccountEdit::ForgetNote {
            note,
            tags: Vec::new(),
            legacy_tags: Vec::new(),
            reminders: Vec::new(),
        }
        .writes();
        // Whatever this person had set, and not only the fields they did set:
        // the edit is the same on a device that coloured the note and on one
        // that never touched it.
        assert_eq!(writes.len(), 6);
        assert!(writes.iter().all(|(_, value)| value.is_none()));
        assert_eq!(
            writes
                .iter()
                .filter(|(key, _)| key.starts_with(&format!("p:{}:", note.encode())))
                .count(),
            5
        );
        // And the key. A forgotten note is one this person cannot open on any
        // of their devices, so the register that would hand it back to the next
        // device they set up goes with the placements.
        assert!(writes
            .iter()
            .any(|(key, _)| *key == format!("k:{}", note.encode())));
        // Nothing about any other note, and no setting.
        let other = NoteId::random();
        assert!(!writes
            .iter()
            .any(|(key, _)| key.contains(&other.encode()) || key.starts_with("s:")));
    }

    #[test]
    fn forgetting_a_note_takes_its_memberships_and_leaves_the_labels() {
        // Memberships are the one part of a placement that is not a fixed set of
        // keys, so the one part a caller has to name. The labels themselves are
        // not touched: a label outlives the notes that carried it.
        let note = NoteId::random();
        let errands = LabelId::random();
        let shop = LabelId::random();
        let writes = AccountEdit::ForgetNote {
            note,
            tags: vec![errands, shop],
            legacy_tags: Vec::new(),
            reminders: Vec::new(),
        }
        .writes();
        assert_eq!(writes.len(), 8);
        assert!(writes.iter().all(|(_, value)| value.is_none()));
        for label in [errands, shop] {
            assert!(writes
                .iter()
                .any(|(key, _)| *key == format!("m:{}:{}", note.encode(), label.encode())));
        }
        // Not the label names. Forgetting the last note with a label leaves a
        // label with no notes, which is a thing this document may hold.
        assert!(!writes.iter().any(|(key, _)| key.starts_with("l:")));
    }

    #[test]
    fn a_membership_is_a_key_of_its_own_so_two_of_them_cannot_collide() {
        let note = NoteId::random();
        let on = |label: LabelId| {
            AccountEdit::SetTag {
                note,
                label,
                on: true,
            }
            .writes()
        };
        let errands = LabelId::random();
        let recipes = LabelId::random();
        // Two different labels on one note are two different keys, which is what
        // makes them survive each other.
        assert_ne!(on(errands)[0].0, on(recipes)[0].0);
        assert_eq!(on(errands)[0].1, Some(Value::Bool(true)));
        // And taking one off is a delete, not a `false`.
        assert_eq!(
            AccountEdit::SetTag {
                note,
                label: errands,
                on: false,
            }
            .writes()[0]
                .1,
            None
        );
    }

    #[test]
    fn renaming_a_label_touches_one_key_and_no_membership() {
        // The whole of why a label has an ID. A rename is a register write on
        // `l:<id>`, and every membership key is built from the ID, so nothing
        // that says which notes carry the label is even mentioned.
        let label = LabelId::random();
        let writes = AccountEdit::SetLabelName {
            label,
            name: "Dinners".into(),
        }
        .writes();
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].0, format!("l:{}", label.encode()));
        assert_eq!(writes[0].1, Some(Value::Str("Dinners".into())));
    }

    #[test]
    fn a_label_and_its_memberships_round_trip() {
        let note = NoteId::random();
        let label = LabelId::random();
        // A colon in the name needs no escaping, because the name is not in a
        // key any more — it is a value.
        let name = "shop: weekly";
        let mut writes = AccountEdit::SetLabelName {
            label,
            name: name.into(),
        }
        .writes();
        writes.extend(
            AccountEdit::SetTag {
                note,
                label,
                on: true,
            }
            .writes(),
        );
        let view = view_of(writes.into_iter().map(|(k, v)| (k, v.unwrap())));
        assert_eq!(view.label_name(label), Some(name));
        assert_eq!(view.labels_on(note), vec![label]);
        assert_eq!(view.tags(), vec![(label, name.to_string(), 1)]);
    }

    #[test]
    fn a_label_with_no_notes_is_still_a_label() {
        // The other half of what an ID buys. There is nowhere to hold this in a
        // document where a label is its own name.
        let label = LabelId::random();
        let writes = AccountEdit::SetLabelName {
            label,
            name: "Someday".into(),
        }
        .writes();
        let view = view_of(writes.into_iter().map(|(k, v)| (k, v.unwrap())));
        assert_eq!(view.tags(), vec![(label, "Someday".to_string(), 0)]);
    }

    #[test]
    fn forgetting_a_label_takes_its_name_and_every_membership() {
        let label = LabelId::random();
        let one = NoteId::random();
        let two = NoteId::random();
        let writes = AccountEdit::ForgetLabel {
            label,
            notes: vec![one, two],
        }
        .writes();
        assert_eq!(writes.len(), 3);
        assert!(writes.iter().all(|(_, value)| value.is_none()));
        assert!(writes
            .iter()
            .any(|(key, _)| *key == format!("l:{}", label.encode())));
        for note in [one, two] {
            assert!(writes
                .iter()
                .any(|(key, _)| *key == format!("m:{}:{}", note.encode(), label.encode())));
        }
    }

    #[test]
    fn a_membership_without_a_name_is_not_a_label() {
        // The two keys can arrive apart — a peer delivers one before the other.
        // A membership whose label this device has no name for contributes
        // nothing, rather than a chip with nothing written on it.
        let note = NoteId::random();
        let label = LabelId::random();
        let view = view_of([(
            format!("m:{}:{}", note.encode(), label.encode()),
            Value::Bool(true),
        )]);
        assert!(view.labels_on(note).is_empty());
        assert!(view.tags().is_empty());
        // The membership itself is still held, so the name arriving later is
        // enough to make the label appear without the membership being re-sent.
        assert_eq!(
            view.placement(note).tags.into_iter().collect::<Vec<_>>(),
            vec![label]
        );
    }

    #[test]
    fn a_name_that_matches_ignoring_case_is_the_same_label() {
        let a = LabelId::random();
        let b = LabelId::random();
        let view = view_of([
            (format!("l:{}", a.encode()), Value::Str("Recipes".into())),
            (format!("l:{}", b.encode()), Value::Str("errands".into())),
        ]);
        assert_eq!(view.label_named("recipes"), Some(a));
        assert_eq!(view.label_named("ERRANDS"), Some(b));
        assert_eq!(view.label_named("dinners"), None);
    }

    #[test]
    fn two_labels_with_one_name_resolve_the_same_way_everywhere() {
        // What two devices offline can make, and what `rename_tag` can now
        // repair. Until it does, every device has to pick the same one of the
        // pair, or one device takes a label off and another does not.
        let a = LabelId::random();
        let b = LabelId::random();
        let view = view_of([
            (format!("l:{}", a.encode()), Value::Str("Recipes".into())),
            (format!("l:{}", b.encode()), Value::Str("recipes".into())),
        ]);
        assert_eq!(view.label_named("recipes"), Some(a.min(b)));
    }

    // -- the shape a label used to have -------------------------------------

    #[test]
    fn a_legacy_membership_is_read_and_kept_apart() {
        // Read, so that a device upgrading does not appear to lose its labels
        // between opening the document and the migration running; kept in its
        // own field, so the migration has something exact to work from.
        let note = NoteId::random();
        let view = view_of([(format!("t:{}:errands", note.encode()), Value::Bool(true))]);
        assert_eq!(
            view.placement(note)
                .legacy_tags
                .into_iter()
                .collect::<Vec<_>>(),
            vec!["errands".to_string()]
        );
        // It names a label that does not exist yet, so it is on no note until
        // one does.
        assert!(view.labels_on(note).is_empty());
    }

    #[test]
    fn a_legacy_membership_finds_the_label_the_migration_made() {
        let note = NoteId::random();
        let label = LabelId::random();
        let view = view_of([
            (format!("t:{}:Errands", note.encode()), Value::Bool(true)),
            (
                format!("l:{}", label.encode()),
                Value::Str("errands".into()),
            ),
        ]);
        // Matched by name, ignoring case, which is what the migration does when
        // it decides whether to mint a label or reuse one.
        assert_eq!(view.labels_on(note), vec![label]);
        assert_eq!(view.tags(), vec![(label, "errands".to_string(), 1)]);
    }

    #[test]
    fn a_note_is_not_counted_twice_for_one_label() {
        // A crash halfway through the migration leaves a note holding both
        // shapes of the same membership. It is one label on one note.
        let note = NoteId::random();
        let label = LabelId::random();
        let view = view_of([
            (format!("t:{}:errands", note.encode()), Value::Bool(true)),
            (
                format!("m:{}:{}", note.encode(), label.encode()),
                Value::Bool(true),
            ),
            (
                format!("l:{}", label.encode()),
                Value::Str("errands".into()),
            ),
        ]);
        assert_eq!(view.labels_on(note), vec![label]);
        assert_eq!(view.tags(), vec![(label, "errands".to_string(), 1)]);
    }

    #[test]
    fn a_membership_key_that_is_off_or_malformed_is_not_a_label() {
        let note = NoteId::random();
        let label = LabelId::random();
        let view = view_of([
            (
                format!("l:{}", label.encode()),
                Value::Str("errands".into()),
            ),
            // What an older or newer build might leave: this one deletes instead.
            (
                format!("m:{}:{}", note.encode(), label.encode()),
                Value::Bool(false),
            ),
            // A string where a bool belongs.
            (
                format!("m:{}:{}", note.encode(), LabelId::random().encode()),
                Value::Str("x".into()),
            ),
            (
                format!("m:not-a-note-id:{}", label.encode()),
                Value::Bool(true),
            ),
            (
                format!("m:{}:not-a-label-id", note.encode()),
                Value::Bool(true),
            ),
            // And a label key that is not a name.
            ("l:not-a-label-id".to_string(), Value::Str("x".into())),
            (
                format!("l:{}", LabelId::random().encode()),
                Value::Bool(true),
            ),
        ]);
        assert!(view.placement(note).tags.is_empty());
        assert!(view.labels_on(note).is_empty());
        // The one real label is there, carried by nobody.
        assert_eq!(view.tags(), vec![(label, "errands".to_string(), 0)]);
    }

    #[test]
    fn a_label_is_counted_by_the_notes_that_carry_it() {
        let (a, b) = (NoteId::random(), NoteId::random());
        let errands = LabelId::random();
        let recipes = LabelId::random();
        let view = view_of([
            (
                format!("l:{}", errands.encode()),
                Value::Str("errands".into()),
            ),
            (
                format!("l:{}", recipes.encode()),
                Value::Str("Recipes".into()),
            ),
            (
                format!("m:{}:{}", a.encode(), errands.encode()),
                Value::Bool(true),
            ),
            (
                format!("m:{}:{}", b.encode(), errands.encode()),
                Value::Bool(true),
            ),
            (
                format!("m:{}:{}", a.encode(), recipes.encode()),
                Value::Bool(true),
            ),
        ]);
        // Case-insensitively sorted, because a filter menu is read by a person.
        assert_eq!(
            view.tags(),
            vec![
                (errands, "errands".to_string(), 2),
                (recipes, "Recipes".to_string(), 1)
            ]
        );
    }

    #[test]
    fn a_name_is_trimmed_and_its_whitespace_collapsed() {
        assert_eq!(
            normalise_tag("  weekly   shop \n"),
            Some("weekly shop".into())
        );
        assert_eq!(normalise_tag("errands"), Some("errands".into()));
        // Case is kept: reusing an existing label is decided a layer up.
        assert_eq!(normalise_tag("Recipes"), Some("Recipes".into()));
        // Nothing that is not a name.
        assert_eq!(normalise_tag("   "), None);
        assert_eq!(normalise_tag(""), None);
    }

    #[test]
    fn a_name_too_long_to_be_a_label_is_cut_on_a_character() {
        // Cut by characters and not by bytes, or the `String` would be cut
        // through the middle of one.
        let long = "é".repeat(MAX_TAG_LEN + 10);
        let name = normalise_tag(&long).unwrap();
        assert_eq!(name.chars().count(), MAX_TAG_LEN);
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
