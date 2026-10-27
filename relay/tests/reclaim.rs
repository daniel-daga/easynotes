//! Freeing a removed attachment's bytes: a compaction's live list, the
//! objects it dooms, and the sweep that deletes them
//! ([ADR 0027](../../docs/decisions.md#adr-0027)).
//!
//! The memory store always; Postgres when `DATABASE_URL` is set, which CI
//! does. Below the socket, like `quota.rs`: the stores are handed proposals
//! and times directly, so each rule is one assertion and no test waits a day.
//! The same through a relay's HTTP calls is at the end.

use easynotes_core::compaction::Request;
use easynotes_core::crypto::{DeviceIdentity, Envelope, NoteKey};
use easynotes_core::ids::{MediaObjectId, NoteId};
use easynotes_relay::media::{Geometry, Reserved};
use easynotes_relay::quota::{Proposal, Refusal};
use easynotes_relay::{Limits, MediaLimits, MemoryStore, PostgresStore, RelayStore};
use std::sync::Arc;

async fn stores() -> Vec<(&'static str, Arc<dyn RelayStore>)> {
    let mut out: Vec<(&'static str, Arc<dyn RelayStore>)> =
        vec![("memory", Arc::new(MemoryStore::new()))];
    match std::env::var("DATABASE_URL") {
        Ok(url) => {
            let pg = PostgresStore::connect(&url)
                .await
                .unwrap_or_else(|e| panic!("DATABASE_URL is set but unusable: {e}"));
            out.push(("postgres", Arc::new(pg)));
        }
        Err(_) => eprintln!("postgres skipped: DATABASE_URL is not set"),
    }
    out
}

const DAY: i64 = 24 * 60 * 60;
/// Far enough from zero that "a day before" is still a time.
const T0: i64 = 1_000_000_000;

const OBJECT: Geometry = Geometry {
    sealed_len: 1000,
    chunk_size: 1 << 16,
};

/// A note with a log to compact, and objects in its topic.
struct Note {
    id: NoteId,
    key: NoteKey,
    device: DeviceIdentity,
    counter: u64,
}

impl Note {
    fn new() -> Self {
        Self {
            id: NoteId::random(),
            key: NoteKey::random(),
            device: DeviceIdentity::generate(),
            counter: 0,
        }
    }

    fn topic(&self) -> String {
        self.id.encode()
    }

    fn update(&mut self) -> Vec<u8> {
        self.counter += 1;
        Envelope::seal(&self.device, self.id, &self.key, self.counter, b"edit")
            .unwrap()
            .encode()
    }

    /// Append an edit, so there is a head for a base to cover.
    async fn edit(&mut self, store: &Arc<dyn RelayStore>) -> i64 {
        let update = self.update();
        match store
            .append_within(&self.topic(), &update, &Limits::default())
            .await
            .unwrap()
        {
            easynotes_relay::Appended::Stored { seq, .. } => seq,
            other => panic!("not stored: {other:?}"),
        }
    }

    /// Commit an object to this topic, last touched at `at`.
    async fn object(&self, store: &Arc<dyn RelayStore>, at: i64) -> MediaObjectId {
        let object = MediaObjectId::random();
        let reserved = store
            .reserve_media(
                &self.topic(),
                &object.encode(),
                OBJECT,
                &MediaLimits::default(),
                at,
            )
            .await
            .unwrap();
        assert_eq!(reserved, Reserved::New);
        store
            .commit_media(&self.topic(), &object.encode(), [7; 32])
            .await
            .unwrap();
        object
    }

    /// Compact to the head, listing `live`, at `now`.
    async fn compact(
        &mut self,
        store: &Arc<dyn RelayStore>,
        live: Option<Vec<MediaObjectId>>,
        now: i64,
    ) -> Result<i64, Refusal> {
        let covers = self.edit(store).await;
        let base = self.update();
        let request = Request::sign(self.id, &self.key, covers, base, live);
        store
            .compact(
                &self.topic(),
                &Proposal {
                    covers: request.covers,
                    base: &request.base,
                    media: request.media.as_deref(),
                    key: request.key.0,
                    signature: request.signature,
                },
                &MediaLimits::default(),
                now,
            )
            .await
            .unwrap()
    }
}

async fn doomed(store: &Arc<dyn RelayStore>, note: &Note, object: MediaObjectId) -> Option<bool> {
    store
        .media(&note.topic(), &object.encode())
        .await
        .unwrap()
        .map(|row| row.doomed)
}

#[tokio::test]
async fn a_compaction_dooms_what_its_list_leaves_out_and_nothing_else() {
    for (name, store) in stores().await {
        let mut note = Note::new();
        let kept = note.object(&store, T0).await;
        let removed = note.object(&store, T0).await;
        // Committed within the day: maybe an upload whose descriptor is on
        // its way, so not this compaction's to doom.
        let fresh = note.object(&store, T0 + 2 * DAY - 60).await;
        // Reserved and never committed: the staging expiry's, not this.
        let staged = MediaObjectId::random();
        store
            .reserve_media(
                &note.topic(),
                &staged.encode(),
                OBJECT,
                &MediaLimits::default(),
                T0,
            )
            .await
            .unwrap();
        // Another note's object is not this note's list's business.
        let other = Note::new();
        let elsewhere = other.object(&store, T0).await;

        note.compact(&store, Some(vec![kept]), T0 + 2 * DAY)
            .await
            .unwrap();

        assert_eq!(doomed(&store, &note, kept).await, Some(false), "{name}");
        assert_eq!(doomed(&store, &note, removed).await, Some(true), "{name}");
        assert_eq!(doomed(&store, &note, fresh).await, Some(false), "{name}");
        assert_eq!(doomed(&store, &note, staged).await, Some(false), "{name}");
        assert_eq!(
            doomed(&store, &other, elsewhere).await,
            Some(false),
            "{name}"
        );
    }
}

#[tokio::test]
async fn no_list_dooms_nothing_and_a_refused_compaction_applies_none() {
    for (name, store) in stores().await {
        let mut note = Note::new();
        let object = note.object(&store, T0).await;
        note.compact(&store, None, T0 + 2 * DAY).await.unwrap();
        assert_eq!(doomed(&store, &note, object).await, Some(false), "{name}");

        // A stranger's key: refused, and its empty list with it.
        let mut stranger = Note {
            key: NoteKey::random(),
            ..Note::new()
        };
        stranger.id = note.id;
        stranger.counter = 100;
        let refused = stranger
            .compact(&store, Some(Vec::new()), T0 + 2 * DAY)
            .await;
        assert_eq!(refused, Err(Refusal::Unauthorised), "{name}");
        assert_eq!(doomed(&store, &note, object).await, Some(false), "{name}");
    }
}

#[tokio::test]
async fn asking_for_a_doomed_object_or_listing_it_again_keeps_it() {
    for (name, store) in stores().await {
        let mut note = Note::new();
        let asked = note.object(&store, T0).await;
        let relisted = note.object(&store, T0).await;
        note.compact(&store, Some(Vec::new()), T0 + 2 * DAY)
            .await
            .unwrap();
        assert_eq!(doomed(&store, &note, asked).await, Some(true), "{name}");

        // A status call, a commit or a fetch: what the handlers call.
        store
            .touch_media(&note.topic(), &asked.encode(), T0 + 3 * DAY)
            .await
            .unwrap();
        assert_eq!(doomed(&store, &note, asked).await, Some(false), "{name}");
        // And it is fresh again: a compaction the same day leaves it alone.
        note.compact(&store, Some(vec![relisted]), T0 + 3 * DAY + 60)
            .await
            .unwrap();
        assert_eq!(doomed(&store, &note, asked).await, Some(false), "{name}");
        assert_eq!(doomed(&store, &note, relisted).await, Some(false), "{name}");
    }
}

#[tokio::test]
async fn the_sweep_deletes_only_what_was_doomed_long_enough_ago() {
    // Earlier than every other test's times: on Postgres the tests share one
    // table, and a sweep deletes everything doomed before its cutoff, so this
    // one's must not reach theirs.
    const T: i64 = T0 / 2;
    for (name, store) in stores().await {
        let mut note = Note::new();
        let early = note.object(&store, T).await;
        let kept = note.object(&store, T).await;
        note.compact(&store, Some(vec![kept]), T + 2 * DAY)
            .await
            .unwrap();
        let late = note.object(&store, T + 2 * DAY).await;
        note.compact(&store, Some(vec![kept]), T + 4 * DAY)
            .await
            .unwrap();
        assert_eq!(doomed(&store, &note, late).await, Some(true), "{name}");

        // What the relay passes a week and a second after the first doom:
        // the first goes, the second has days left.
        let ttl = MediaLimits::default().doomed_ttl_secs;
        let now = T + 2 * DAY + ttl + 1;
        let swept = store.sweep_media(now - ttl).await.unwrap();
        let topic = note.topic();
        assert!(swept.contains(&(topic.clone(), early.encode())), "{name}");
        assert!(
            !swept
                .iter()
                .any(|(n, o)| n == &topic && *o != early.encode()),
            "{name}"
        );
        assert_eq!(doomed(&store, &note, early).await, None, "{name}");
        assert_eq!(doomed(&store, &note, late).await, Some(true), "{name}");
        assert_eq!(doomed(&store, &note, kept).await, Some(false), "{name}");
    }
}

#[tokio::test]
async fn a_doomed_object_frees_its_note_but_not_the_relay_until_it_goes() {
    for (name, store) in stores().await {
        let mut note = Note::new();
        // The relay-wide ceiling is checked against the memory store only.
        // On Postgres every test shares the table, so a ceiling this small is
        // already full of the others' objects.
        let memory = name == "memory";
        let limits = MediaLimits {
            note_bytes: 2 * OBJECT.sealed_len,
            ceiling: memory.then_some(3 * OBJECT.sealed_len),
            ..MediaLimits::default()
        };
        // Two objects fill the note.
        for _ in 0..2 {
            let object = MediaObjectId::random();
            let reserved = store
                .reserve_media(&note.topic(), &object.encode(), OBJECT, &limits, T0)
                .await
                .unwrap();
            assert_eq!(reserved, Reserved::New, "{name}");
            store
                .commit_media(&note.topic(), &object.encode(), [7; 32])
                .await
                .unwrap();
        }
        let third = MediaObjectId::random();
        let full = store
            .reserve_media(
                &note.topic(),
                &third.encode(),
                OBJECT,
                &limits,
                T0 + 2 * DAY,
            )
            .await
            .unwrap();
        assert_eq!(full, Reserved::NoteFull, "{name}");

        // Both removed: the note has room again at once.
        note.compact(&store, Some(Vec::new()), T0 + 2 * DAY)
            .await
            .unwrap();
        let usage = store.usage().await.unwrap();
        assert!(usage.media_doomed_bytes >= 2 * OBJECT.sealed_len, "{name}");
        let reserved = store
            .reserve_media(
                &note.topic(),
                &third.encode(),
                OBJECT,
                &limits,
                T0 + 2 * DAY,
            )
            .await
            .unwrap();
        assert_eq!(reserved, Reserved::New, "{name}");

        // But the relay's ceiling still counts the doomed bytes, which are
        // still on its disk.
        if memory {
            let fourth = MediaObjectId::random();
            let relay_full = store
                .reserve_media(&Note::new().topic(), &fourth.encode(), OBJECT, &limits, T0)
                .await
                .unwrap();
            assert_eq!(relay_full, Reserved::RelayFull, "{name}");
        }
    }
}
