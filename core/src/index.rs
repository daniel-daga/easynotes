//! The sealed index: what a note says, written down so that listing and
//! searching do not have to open it.
//!
//! Two problems, one answer. [ADR 0007](../../docs/adr/0007-note-list.md) said
//! that listing notes means opening them — a title is ciphertext until a key
//! decrypts it — and measured what that costs at launch: two hundred notes take
//! most of a second, because every envelope in every log is signature-verified
//! before it is decrypted. [ADR 0005](../../docs/adr/0005-local-store.md) said
//! that search cannot use SQLite's own FTS5, because the text is not in a column
//! at all. Both of them pointed at the same thing: an index of titles and text,
//! sealed like everything else this device keeps, built once and used for both.
//!
//! This is that index. One entry per note, holding what a card draws and what a
//! search reads, sealed under **the note's own key** — see
//! [`NoteKey::seal_index`](crate::crypto::NoteKey::seal_index) for why that key
//! and not the master key — and kept in the store's per-note metadata beside the
//! sync cursor.
//!
//! ## It is a cache, and it says so
//!
//! An index is a second copy of the data, which means it can be wrong. The whole
//! of the defence is that an entry records how many envelopes the note's log held
//! when it was written, and is believed only while the log is still that length:
//!
//! * Every write that changes a document appends to the log *and then* rewrites
//!   the entry, so an entry is never ahead of the log it summarises.
//! * A crash between the two leaves an entry one edit stale, and a length that
//!   no longer matches. It is not believed, the note is opened, and the entry is
//!   rebuilt — which is exactly what every launch did before this existed.
//! * A note with no entry at all — one carried in by
//!   [`copy_store`](crate::store::copy_store), or one written by a version of the
//!   app that predates this file — is the same case and takes the same path.
//!
//! So the failure mode of the index is *slowness*, never a stale card. That is
//! the property that makes it safe to put a copy of the note's text somewhere
//! the CRDT does not control.

use crate::crdt::{Colour, NoteKind};
use crate::crypto::NoteKey;
use crate::error::{CoreError, Result};
use crate::ids::NoteId;
use serde::{Deserialize, Serialize};

/// Bumped when the shape below changes. An entry written by a different version
/// is not migrated: it is ignored, the note is opened, and a current one is
/// written over it. A cache is the one thing that never needs a migration.
pub const ENTRY_VERSION: u8 = 7;

/// One checklist item as the index keeps it: its text, and whether it has been
/// crossed off.
///
/// The flag is here so that a card can leave crossed-off items out of its
/// preview while a search still reads every item. The index is a cache, so
/// adding it is a version bump and nothing more: an old entry is ignored and
/// rebuilt from the note.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexItem {
    pub text: String,
    pub checked: bool,
}

/// One note, as the list and the search read it.
///
/// It holds the note's whole text rather than the four-item preview a card
/// shows, because search is the other half of what it is for: an entry that held
/// only what was on the card could not find the twelfth item on a shopping list.
/// What that costs is an entry about the size of the note, rewritten on every
/// keystroke — which is the same order as the envelope that keystroke already
/// seals and appends.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexEntry {
    pub collaborators: Vec<crate::collaborators::Collaborator>,
    pub version: u8,
    pub kind: NoteKind,
    pub title: String,
    /// The body, one entry per line with something on it. Lines rather than one
    /// string because a card previews lines and a search reports one.
    pub body: Vec<String>,
    /// Every item's text and checked state, in display order.
    pub items: Vec<IndexItem>,
    /// Each live attachment's kind and file name, in display order. Names are
    /// searched; what is in a picture or a recording is not, since reading it
    /// would take OCR or transcription this device does not do.
    pub attachments: Vec<(crate::attachment::AttachmentKind, String)>,
    /// The first live picture that has a thumbnail, whole: what a card shows
    /// above its title. The descriptor, key and all, is here so that a grid
    /// can fetch and open a thumbnail without replaying the note; the entry
    /// is sealed under the note's key, which already opens that descriptor,
    /// and it is rewritten whenever the note changes, so a removal takes it
    /// out.
    pub cover: Option<(crate::ids::AttachmentId, crate::attachment::Media)>,
    /// The colour everyone on the note sees, as the note document records it.
    /// `colour_set` is false for a note that has never recorded one — every
    /// note written before colour was shared — and a card then falls back to
    /// this person's own placement.
    ///
    /// `default` so an entry written before the field existed still reads;
    /// the next edit to the note rewrites it with the field.
    #[serde(default)]
    pub colour: Option<Colour>,
    #[serde(default)]
    pub colour_set: bool,
    /// How many envelopes the note's log held when this was written. The whole
    /// of the staleness check; see the module docs.
    pub log_len: usize,
}

impl IndexEntry {
    pub fn seal(&self, note: NoteId, key: &NoteKey) -> Result<Vec<u8>> {
        let plain = serde_json::to_vec(self)
            .map_err(|e| CoreError::Storage(format!("unwritable index entry: {e}")))?;
        key.seal_index(&note, &plain)
    }

    /// Read an entry back, or `None` if it cannot be believed.
    ///
    /// Every way of not believing one is the same way: the note gets opened and
    /// the entry rewritten. So a version this build does not know, a row that
    /// fails to decrypt because the note's key was rotated, and a row that does
    /// not match the log any more are one case with one answer, and none of them
    /// is an error the caller has to handle.
    pub fn open(note: NoteId, key: &NoteKey, sealed: &[u8], log_len: usize) -> Option<Self> {
        let plain = key.open_index(&note, sealed).ok()?;
        let entry: Self = serde_json::from_slice(&plain).ok()?;
        if entry.version != ENTRY_VERSION || entry.log_len != log_len {
            return None;
        }
        Some(entry)
    }

    /// What a card previews: the body's lines, then the items still to do. A
    /// crossed-off item is finished rather than forgotten, so it stays in the
    /// note — and in a search — but does not use up a line of the card. Both
    /// body and items whatever the kind, for the reason
    /// [`crate::crdt::NoteView`] gives.
    pub fn preview(&self) -> impl Iterator<Item = &String> {
        self.body.iter().chain(
            self.items
                .iter()
                .filter(|item| !item.checked)
                .map(|item| &item.text),
        )
    }

    /// Every line a search may match, whether or not a card would show it.
    fn searchable_lines(&self) -> impl Iterator<Item = &String> {
        std::iter::once(&self.title)
            .chain(self.body.iter())
            .chain(self.items.iter().map(|item| &item.text))
            .chain(self.attachments.iter().map(|(_, name)| name))
    }

    /// How many live attachments of `kind` the note has.
    pub fn count(&self, kind: crate::attachment::AttachmentKind) -> usize {
        self.attachments.iter().filter(|(k, _)| *k == kind).count()
    }

    /// The first line — the title, the body, the items in display order, then
    /// the attachments' file names — that
    /// contains any of `terms`, if every term appears somewhere in the note.
    ///
    /// This is the whole of the matching rule, and it is deliberately the
    /// smallest one that is useful: case-insensitive substrings, all terms
    /// required, nothing stemmed and nothing ranked. A household searching its
    /// shopping lists for "aspara" wants the list with asparagus on it, and
    /// anything cleverer is a promise about relevance that a hundred notes
    /// cannot keep honestly. What it is *not* is a prefix match on words only:
    /// "sparag" finds it too, which is the behaviour of a find-in-page and the
    /// one nobody has to be taught.
    ///
    /// The line comes back rather than a bare yes, because a match on the
    /// twelfth item of a list is invisible on a card that shows four.
    pub fn matched_line(&self, terms: &[String]) -> Option<&str> {
        if terms.is_empty() {
            return None;
        }
        let holds = |line: &str, term: &String| line.to_lowercase().contains(term.as_str());
        if !terms
            .iter()
            .all(|term| self.searchable_lines().any(|line| holds(line, term)))
        {
            return None;
        }
        self.searchable_lines()
            .find(|line| terms.iter().any(|term| holds(line, term)))
            .map(String::as_str)
    }
}

/// Split a query into the terms every hit must contain, lowercased once here so
/// that a search over a hundred notes does not lowercase it a hundred times.
pub fn terms(query: &str) -> Vec<String> {
    query
        .split_whitespace()
        .map(|term| term.to_lowercase())
        .collect()
}

/// A body as the index keeps it: its lines, minus the blank ones.
pub fn body_lines(body: &str) -> Vec<String> {
    body.lines()
        .filter(|line| !line.trim().is_empty())
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An entry written before colour was shared has no colour fields. It has
    /// to read back — the index is a cache and a missing field is not a reason
    /// to rebuild every note on a phone — and say "the note has not recorded a
    /// colour", so a card keeps the personal placement the old build wrote.
    #[test]
    fn an_entry_from_before_colour_was_shared_still_reads() {
        let old = r#"{
            "collaborators": [],
            "version": 7,
            "kind": "checklist",
            "title": "groceries",
            "body": [],
            "items": [{"text": "milk", "checked": false}],
            "attachments": [],
            "cover": null,
            "log_len": 3
        }"#;
        let entry: IndexEntry = serde_json::from_str(old).unwrap();
        assert_eq!(entry.colour, None);
        assert!(!entry.colour_set);
        assert_eq!(entry.title, "groceries");
    }

    #[test]
    fn a_colour_survives_the_entry_it_is_written_into() {
        let entry = IndexEntry {
            collaborators: Vec::new(),
            version: ENTRY_VERSION,
            kind: NoteKind::Checklist,
            title: String::new(),
            body: Vec::new(),
            items: Vec::new(),
            attachments: Vec::new(),
            cover: None,
            colour: Some(Colour::Teal),
            colour_set: true,
            log_len: 0,
        };
        let back: IndexEntry =
            serde_json::from_slice(&serde_json::to_vec(&entry).unwrap()).unwrap();
        assert_eq!(back.colour, Some(Colour::Teal));
        assert!(back.colour_set);
    }
}
