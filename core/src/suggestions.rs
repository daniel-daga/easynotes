//! What this person adds to a list over and over.
//!
//! The suggestion history behind "frequently added items". It is a *local*
//! thing in the strongest sense the app has: one row in the note's own sealed
//! metadata, on this device, written from what this person typed into a row,
//! never a CRDT edit and never an envelope. The relay's view of the account does
//! not change by one byte, and a partner on the same list sees no trace of it.
//!
//! ## Why it is per note
//!
//! The hero case is one "Groceries" list reused every week, and its history is
//! the strongest signal there is: this is what *this list* keeps needing. A
//! device-wide table was the alternative and costs a store API, a SQLite
//! migration and a second web key for a weaker answer — "milk" suggested on a
//! hardware list. The upgrade path is a second scope summed over notes, not a
//! change to the shape below.
//!
//! ## Why the caller supplies the clock
//!
//! This crate also compiles for the browser, where `SystemTime` is not
//! available ([`crate::note`] says the same about ordinals). So ranking takes
//! `now` as unix seconds and every test can hand it a fixed instant.
//!
//! ## It is a cache, and it says so
//!
//! A row that cannot be read is an empty history, never an error: a wrong master
//! key or a half-written row costs suggestions, not a note. Deleting the note
//! deletes it (every metadata row goes with [`crate::store::OpStore::forget`]),
//! which is exactly the promise permanent deletion already makes.

use serde::{Deserialize, Serialize};

/// How many remembered lines one note keeps. Ordinary lists never approach it;
/// it is there so that a list used every day for years cannot grow without
/// bound. The least useful entry is the one dropped.
pub const MAX_ENTRIES: usize = 300;

/// The longest line worth remembering. Past this it is a pasted paragraph, not
/// something a person adds twice.
pub const MAX_TEXT_CHARS: usize = 120;

/// How quickly a memory fades: a line's count is halved for every two months
/// since it was last added, so "milk" this week outranks "chestnuts" from
/// December even after one Christmas.
const HALF_LIFE_DAYS: f64 = 60.0;

const SECONDS_PER_DAY: f64 = 86_400.0;

/// One remembered line: what it says, how often it was added, and when last.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub text: String,
    pub count: u32,
    /// Unix seconds, from the caller's clock. See the module docs.
    pub last: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct History {
    /// Bumped when the shape below changes. A row from another version is not
    /// migrated, it is ignored — a cache is the one thing that never needs one.
    pub v: u8,
    #[serde(default)]
    pub entries: Vec<Entry>,
}

/// The version this build writes.
pub const VERSION: u8 = 1;

impl Default for History {
    fn default() -> Self {
        Self {
            v: VERSION,
            entries: Vec::new(),
        }
    }
}

/// The form a line is remembered and matched in: trimmed, runs of whitespace
/// collapsed, and nothing else changed.
///
/// Casing is *not* folded here. A person who writes "Milk" should be offered
/// "Milk", and folding it would offer back their own line in lower case. Matching
/// folds case instead ([`fold`]).
pub fn normalise(text: &str) -> Option<String> {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return None;
    }
    // Count characters, not bytes: a line of emoji is short, and a line of
    // Cyrillic is not 120 characters because it is 240 bytes.
    let mut out: String = collapsed.chars().take(MAX_TEXT_CHARS).collect();
    // Do not leave a bit of a word behind when the cut fell inside one.
    if collapsed.chars().count() > MAX_TEXT_CHARS {
        if let Some(space) = out.rfind(' ') {
            out.truncate(space);
        }
        while out.ends_with(' ') {
            out.pop();
        }
        if out.is_empty() {
            return None;
        }
    }
    Some(out)
}

/// A line as matching sees it. Lower case is enough for the languages this app
/// ships in; the ß/ss fold and full Unicode case folding are deliberately not
/// here, because a suggestion list is a convenience and not an index.
fn fold(text: &str) -> String {
    text.to_lowercase()
}

impl History {
    pub fn open(raw: &[u8]) -> Self {
        if raw.is_empty() {
            return Self::default();
        }
        match serde_json::from_slice::<Self>(raw) {
            Ok(history) if history.v == VERSION => history,
            _ => Self::default(),
        }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_default()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Remember one committed line, as this person typed it.
    ///
    /// Returns nothing: a line that is not worth remembering is not an error,
    /// it is a line that is not worth remembering.
    pub fn learn(&mut self, text: &str, now: i64) {
        let Some(text) = normalise(text) else {
            return;
        };
        let folded = fold(&text);
        match self.entries.iter_mut().find(|e| fold(&e.text) == folded) {
            Some(entry) => {
                entry.count = entry.count.saturating_add(1);
                entry.last = now;
                // The spelling follows the person: if they have started writing
                // "Oat milk", offer that back.
                entry.text = text;
            }
            None => self.entries.push(Entry {
                text,
                count: 1,
                last: now,
            }),
        }
        self.trim(now);
    }

    /// Drop the least useful entries until the history fits [`MAX_ENTRIES`].
    fn trim(&mut self, now: i64) {
        if self.entries.len() <= MAX_ENTRIES {
            return;
        }
        self.entries.sort_by(|a, b| {
            score(b, now)
                .partial_cmp(&score(a, now))
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| b.last.cmp(&a.last))
                .then_with(|| a.text.cmp(&b.text))
        });
        self.entries.truncate(MAX_ENTRIES);
    }

    /// The lines to offer, best first, at most `limit`.
    ///
    /// `prefix` is what has been typed so far: empty for a strip of suggestions
    /// beside a blank row, otherwise the half-typed line being completed.
    /// `exclude` is what the note already says — a suggestion the list already
    /// has is not a suggestion.
    pub fn suggest(
        &self,
        prefix: &str,
        now: i64,
        limit: usize,
        exclude: impl Fn(&str) -> bool,
    ) -> Vec<String> {
        let wanted = fold(prefix.trim());
        let mut ranked: Vec<&Entry> = self
            .entries
            .iter()
            .filter(|entry| {
                let folded = fold(&entry.text);
                folded.starts_with(&wanted) && folded != wanted && !exclude(&entry.text)
            })
            .collect();
        ranked.sort_by(|a, b| {
            score(b, now)
                .partial_cmp(&score(a, now))
                .unwrap_or(std::cmp::Ordering::Equal)
                // A tie on usefulness is broken by what was added last; both
                // are stable, so the list does not reshuffle between frames.
                .then_with(|| b.last.cmp(&a.last))
                .then_with(|| a.text.cmp(&b.text))
        });
        ranked
            .into_iter()
            .take(limit)
            .map(|entry| entry.text.clone())
            .collect()
    }
}

/// How useful an entry is now: its count, faded by how long ago it was last
/// used, so a weekly staple outranks a one-off from last year.
fn score(entry: &Entry, now: i64) -> f64 {
    let age_seconds = (now - entry.last).max(0) as f64;
    let age_days = age_seconds / SECONDS_PER_DAY;
    f64::from(entry.count) * 0.5_f64.powf(age_days / HALF_LIFE_DAYS)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: i64 = 86_400;

    fn history(lines: &[(&str, u32, i64)]) -> History {
        History {
            v: VERSION,
            entries: lines
                .iter()
                .map(|(text, count, last)| Entry {
                    text: (*text).to_owned(),
                    count: *count,
                    last: *last,
                })
                .collect(),
        }
    }

    #[test]
    fn a_line_is_remembered_trimmed_and_counted() {
        let mut h = History::default();
        h.learn("  oat   milk ", 100);
        h.learn("Oat Milk", 200);
        assert_eq!(h.entries.len(), 1, "one line, two spellings");
        assert_eq!(h.entries[0].count, 2);
        assert_eq!(h.entries[0].text, "Oat Milk", "the latest spelling wins");
        assert_eq!(h.entries[0].last, 200);
    }

    #[test]
    fn a_blank_or_absurd_line_is_not_remembered() {
        let mut h = History::default();
        h.learn("   ", 0);
        h.learn("", 0);
        assert!(h.is_empty());

        let long = "Zwiebeln ".repeat(40);
        h.learn(&long, 0);
        let text = &h.entries[0].text;
        assert!(text.chars().count() <= MAX_TEXT_CHARS, "{text:?}");
        assert!(!text.ends_with(' '));
        assert!(long.starts_with(text.as_str()), "a whole word was kept");
    }

    #[test]
    fn what_is_added_often_comes_first_and_a_stale_one_fades() {
        // "milk" six times last week beats "chestnuts" twelve times last
        // Christmas, which is the whole point of the half-life.
        let now = 400 * DAY;
        let h = history(&[
            // Twelve times three months ago still beats twice this week, and
            // loses to six times last week.
            ("Chestnuts", 12, 310 * DAY),
            ("Milk", 6, 395 * DAY),
            ("Bread", 2, 399 * DAY),
        ]);
        assert_eq!(
            h.suggest("", now, 5, |_| false),
            ["Milk", "Chestnuts", "Bread"]
        );
    }

    #[test]
    fn a_prefix_completes_a_line_and_the_note_keeps_its_own() {
        let now = 400 * DAY;
        let h = history(&[("Milk", 5, now), ("Oat milk", 3, now), ("Bread", 9, now)]);
        assert_eq!(h.suggest("mi", now, 5, |_| false), ["Milk"]);
        assert_eq!(h.suggest("mil", now, 5, |_| false), ["Milk"]);
        // Typed out in full is not something to complete.
        assert!(h.suggest("milk", now, 5, |_| false).is_empty());
        // What the list already says is not a suggestion.
        assert_eq!(
            h.suggest("", now, 5, |text| text == "Bread"),
            ["Milk", "Oat milk"]
        );
        assert!(h.suggest("", now, 5, |_| true).is_empty());
    }

    #[test]
    fn the_least_useful_line_is_dropped_at_the_cap() {
        let now = 10 * DAY;
        let mut h = History::default();
        // A full history of lines this person actually adds, each twice.
        for i in 0..MAX_ENTRIES {
            h.learn(&format!("line {i}"), now);
            h.learn(&format!("line {i}"), now);
        }
        assert_eq!(h.entries.len(), MAX_ENTRIES);
        // One line added once, and the one-off is what makes room for itself.
        h.learn("forgotten", now);
        assert_eq!(h.entries.len(), MAX_ENTRIES);
        assert!(
            !h.entries.iter().any(|e| e.text == "forgotten"),
            "the one-off went first"
        );
        assert!(h.entries.iter().any(|e| e.text == "line 0"));
    }

    #[test]
    fn a_row_this_build_cannot_read_is_an_empty_history() {
        assert!(History::open(b"").is_empty());
        assert!(History::open(b"not json").is_empty());
        assert!(
            History::open(br#"{"v": 9, "entries": [{"text":"Milk","count":3,"last":1}]}"#)
                .is_empty()
        );
        let mut h = History::default();
        h.learn("Milk", 1);
        assert_eq!(History::open(&h.to_bytes()), h);
    }
}
