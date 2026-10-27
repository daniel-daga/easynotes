//! Compaction and the caps, held to one set of rules in both stores
//! ([ADR 0024](../../docs/decisions.md#adr-0024)).
//!
//! The memory store always; Postgres when `DATABASE_URL` is set, which CI does.
//! Everything here is below the socket: the stores are handed envelopes and
//! compaction proposals directly, so that each rule is one assertion. The
//! same rules through real devices are in `engine.rs`, `sharing.rs` and
//! `account.rs`.

use easynotes_core::access::{self, Body, Member, OWNER_INVITE};
use easynotes_core::account::RecoveryPhrase;
use easynotes_core::compaction::Request;
use easynotes_core::crypto::{DeviceIdentity, Envelope, EnvelopeKind, NoteKey};
use easynotes_core::ids::NoteId;
use easynotes_relay::quota::{Proposal, Refusal};
use easynotes_relay::{Appended, Limits, MediaLimits, MemoryStore, PostgresStore, RelayStore};
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

/// A note, its key, and a device writing to it.
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

    fn update(&mut self, bytes: &[u8]) -> Vec<u8> {
        self.counter += 1;
        Envelope::seal(&self.device, self.id, &self.key, self.counter, bytes)
            .unwrap()
            .encode()
    }

    /// A base: an update sealed like any other, signed for the relay.
    fn base(&mut self, covers: i64) -> Request {
        let base = self.update(b"the whole document");
        Request::sign(self.id, &self.key, covers, base, None)
    }
}

fn proposal(request: &Request) -> Proposal<'_> {
    Proposal {
        covers: request.covers,
        base: &request.base,
        media: request.media.as_deref(),
        key: request.key.0,
        signature: request.signature,
    }
}

/// An owner-signed package for `note` at `revision`, establishing `key`.
fn package(
    owner: &DeviceIdentity,
    note: NoteId,
    revision: u64,
    parent: Option<access::Digest>,
    key: &NoteKey,
) -> Vec<u8> {
    let member = RecoveryPhrase::generate().root().member();
    let body = Body {
        list: vec![Member {
            member: member.id(),
            box_key: member.box_public(),
            owner: true,
            invite: OWNER_INVITE,
            name: "Owner".into(),
        }],
        ..Body::default()
    };
    access::build(owner, note, revision, parent, key, &body).unwrap()
}

async fn append(
    store: &Arc<dyn RelayStore>,
    topic: &str,
    bytes: &[u8],
    limits: &Limits,
) -> Appended {
    store.append_within(topic, bytes, limits).await.unwrap()
}

async fn seqs(store: &Arc<dyn RelayStore>, topic: &str) -> Vec<i64> {
    store
        .since(topic, 0)
        .await
        .unwrap()
        .iter()
        .map(|s| s.seq)
        .collect()
}

#[tokio::test]
async fn a_compaction_drops_what_its_base_covers_and_nothing_after() {
    for (name, store) in stores().await {
        let mut note = Note::new();
        let topic = note.topic();
        for i in 0..5u8 {
            store.append(&topic, &note.update(&[i])).await.unwrap();
        }
        let request = note.base(3);
        let seq = store
            .compact(&topic, &proposal(&request), &MediaLimits::default(), 0)
            .await
            .unwrap()
            .unwrap_or_else(|r| panic!("{name}: refused: {r:?}"));
        // The base goes at the head, after what it does not cover.
        assert_eq!(seq, 6, "{name}");
        assert_eq!(seqs(&store, &topic).await, vec![4, 5, 6], "{name}");
        let stored = store.since(&topic, 5).await.unwrap();
        assert_eq!(stored[0].envelope, request.base, "{name}");
        assert_eq!(store.head(&topic).await.unwrap(), 6, "{name}");

        let topic_state = store.topic(&topic).await.unwrap();
        assert_eq!(topic_state.covers, 3, "{name}");
        assert_eq!(topic_state.live_envelopes, 3, "{name}");
        assert_eq!(topic_state.loose_envelopes, 3, "{name}");
        assert_eq!(topic_state.registered, Some(request.key.0), "{name}");
        let bytes: u64 = store
            .since(&topic, 0)
            .await
            .unwrap()
            .iter()
            .map(|s| s.envelope.len() as u64)
            .sum();
        assert_eq!(topic_state.live_bytes, bytes, "{name}");

        // A lagging cursor below the base finds the base; one above it does not
        // need it and gets it anyway, once, as a live device would.
        assert_eq!(seqs_after(&store, &topic, 1).await, vec![4, 5, 6], "{name}");
    }
}

async fn seqs_after(store: &Arc<dyn RelayStore>, topic: &str, since: i64) -> Vec<i64> {
    store
        .since(topic, since)
        .await
        .unwrap()
        .iter()
        .map(|s| s.seq)
        .collect()
}

#[tokio::test]
async fn once_a_key_compacts_a_note_no_other_key_can() {
    for (name, store) in stores().await {
        let mut note = Note::new();
        let topic = note.topic();
        for i in 0..4u8 {
            store.append(&topic, &note.update(&[i])).await.unwrap();
        }
        store
            .compact(&topic, &proposal(&note.base(2)), &MediaLimits::default(), 0)
            .await
            .unwrap()
            .unwrap();

        // Somebody who learned the ID, with a key of their own.
        let mut stranger = Note {
            id: note.id,
            key: NoteKey::random(),
            device: DeviceIdentity::generate(),
            counter: 0,
        };
        let refused = store
            .compact(
                &topic,
                &proposal(&stranger.base(4)),
                &MediaLimits::default(),
                0,
            )
            .await
            .unwrap();
        assert_eq!(refused, Err(Refusal::Unauthorised), "{name}");
        assert_eq!(store.topic(&topic).await.unwrap().covers, 2, "{name}");
    }
}

#[tokio::test]
async fn a_stale_or_forged_or_foreign_compaction_is_refused() {
    for (name, store) in stores().await {
        let mut note = Note::new();
        let topic = note.topic();
        for i in 0..6u8 {
            store.append(&topic, &note.update(&[i])).await.unwrap();
        }
        store
            .compact(&topic, &proposal(&note.base(4)), &MediaLimits::default(), 0)
            .await
            .unwrap()
            .unwrap();

        // No further than the base already reaches: two devices compacting at
        // once cannot move it backwards.
        for covers in [3, 4] {
            let refused = store
                .compact(
                    &topic,
                    &proposal(&note.base(covers)),
                    &MediaLimits::default(),
                    0,
                )
                .await
                .unwrap();
            assert_eq!(refused, Err(Refusal::Stale), "{name}: covers {covers}");
        }
        // Past the head is a promise nobody can keep.
        let head = store.head(&topic).await.unwrap();
        let refused = store
            .compact(
                &topic,
                &proposal(&note.base(head + 1)),
                &MediaLimits::default(),
                0,
            )
            .await
            .unwrap();
        assert_eq!(refused, Err(Refusal::Stale), "{name}");

        // Signed for one reach and sent with another.
        let mut request = note.base(5);
        request.covers = 6;
        let refused = store
            .compact(&topic, &proposal(&request), &MediaLimits::default(), 0)
            .await
            .unwrap();
        assert_eq!(refused, Err(Refusal::BadSignature), "{name}");

        // A snapshot kind, or another note's update, is not a base.
        note.counter += 1;
        let snapshot = Envelope::seal_kind(
            &note.device,
            note.id,
            &note.key,
            note.counter,
            EnvelopeKind::Snapshot,
            b"doc",
        )
        .unwrap()
        .encode();
        let request = Request::sign(note.id, &note.key, 5, snapshot, None);
        let refused = store
            .compact(&topic, &proposal(&request), &MediaLimits::default(), 0)
            .await
            .unwrap();
        assert_eq!(refused, Err(Refusal::NotABase), "{name}");
        let mut other = Note {
            id: NoteId::random(),
            ..Note::new()
        };
        let foreign = other.update(b"doc");
        let request = Request::sign(note.id, &note.key, 5, foreign, None);
        let refused = store
            .compact(&topic, &proposal(&request), &MediaLimits::default(), 0)
            .await
            .unwrap();
        assert_eq!(refused, Err(Refusal::NotABase), "{name}");

        assert_eq!(store.topic(&topic).await.unwrap().covers, 4, "{name}");
    }
}

#[tokio::test]
async fn packages_outlive_compaction_and_name_who_may_compact() {
    for (name, store) in stores().await {
        let owner = DeviceIdentity::generate();
        let mut note = Note::new();
        let topic = note.topic();
        store.append(&topic, &note.update(b"a")).await.unwrap();
        let first = package(&owner, note.id, 1, None, &note.key);
        store.append(&topic, &first).await.unwrap();
        store.append(&topic, &note.update(b"b")).await.unwrap();
        // Somebody else's package is not the owner's, and does not count.
        let forged = package(
            &DeviceIdentity::generate(),
            note.id,
            2,
            Some(access::digest(&first)),
            &NoteKey::random(),
        );
        store.append(&topic, &forged).await.unwrap();

        let state = store.topic(&topic).await.unwrap();
        assert_eq!(state.owner, Some(owner.public_key().0), "{name}");
        let current = state.current.expect("the owner's package is current");
        assert_eq!(current.revision, 1, "{name}");
        assert_eq!(
            current.compaction,
            easynotes_core::compaction::public_key(note.id, &note.key).0,
            "{name}"
        );

        store
            .compact(&topic, &proposal(&note.base(4)), &MediaLimits::default(), 0)
            .await
            .unwrap()
            .unwrap();
        let left = store.since(&topic, 0).await.unwrap();
        assert_eq!(
            left.iter().map(|s| s.seq).collect::<Vec<_>>(),
            vec![2, 5],
            "{name}: the owner's package stays and the forged one goes"
        );
        assert_eq!(left[0].envelope, first, "{name}");

        // A rotation: the owner's next package names a new key, and the old
        // key's holder — somebody just removed — can no longer compact.
        let rotated = NoteKey::random();
        let second = package(&owner, note.id, 2, Some(access::digest(&first)), &rotated);
        store.append(&topic, &second).await.unwrap();
        store.append(&topic, &note.update(b"c")).await.unwrap();
        let head = store.head(&topic).await.unwrap();
        let refused = store
            .compact(
                &topic,
                &proposal(&note.base(head)),
                &MediaLimits::default(),
                0,
            )
            .await
            .unwrap();
        assert_eq!(refused, Err(Refusal::Unauthorised), "{name}");

        let mut member = Note {
            id: note.id,
            key: rotated,
            device: DeviceIdentity::generate(),
            counter: 0,
        };
        let head = store.head(&topic).await.unwrap();
        store
            .compact(
                &topic,
                &proposal(&member.base(head)),
                &MediaLimits::default(),
                0,
            )
            .await
            .unwrap()
            .unwrap_or_else(|r| panic!("{name}: the current key was refused: {r:?}"));
        let kept: Vec<Vec<u8>> = store
            .since(&topic, 0)
            .await
            .unwrap()
            .into_iter()
            .map(|s| s.envelope)
            .collect();
        assert!(kept.contains(&first) && kept.contains(&second), "{name}");
        assert_eq!(kept.len(), 3, "{name}: two packages and the base");
        let state = store.topic(&topic).await.unwrap();
        assert_eq!(state.loose_envelopes, 1, "{name}");
        assert_eq!(state.live_envelopes, 3, "{name}");
    }
}

#[tokio::test]
async fn a_full_note_refuses_appends_but_not_its_owners_packages() {
    for (name, store) in stores().await {
        let mut note = Note::new();
        let topic = note.topic();
        let first = note.update(b"x");
        let limits = Limits {
            note_bytes: first.len() as u64 * 3,
            ..Limits::default()
        };
        for _ in 0..3 {
            let update = note.update(b"x");
            assert!(
                matches!(
                    append(&store, &topic, &update, &limits).await,
                    Appended::Stored { .. }
                ),
                "{name}"
            );
        }
        let update = note.update(b"x");
        assert_eq!(
            append(&store, &topic, &update, &limits).await,
            Appended::NoteFull,
            "{name}"
        );
        assert_eq!(store.head(&topic).await.unwrap(), 3, "{name}");

        // A compaction is the way out.
        store
            .compact(&topic, &proposal(&note.base(3)), &MediaLimits::default(), 0)
            .await
            .unwrap()
            .unwrap();
        let update = note.update(b"x");
        assert!(
            matches!(
                append(&store, &topic, &update, &limits).await,
                Appended::Stored { .. }
            ),
            "{name}: still full after compacting"
        );
        let update = note.update(b"x");
        assert_eq!(
            append(&store, &topic, &update, &limits).await,
            Appended::NoteFull,
            "{name}"
        );

        // And removing somebody must not wait on a full note.
        let owner = DeviceIdentity::generate();
        let package = package(&owner, note.id, 1, None, &note.key);
        assert!(
            matches!(
                append(&store, &topic, &package, &limits).await,
                Appended::Stored { .. }
            ),
            "{name}"
        );
    }
}

#[tokio::test]
async fn an_appender_is_told_when_a_note_is_crowded() {
    for (name, store) in stores().await {
        let mut note = Note::new();
        let topic = note.topic();
        let limits = Limits {
            crowded_envelopes: 3,
            ..Limits::default()
        };
        let mut said = Vec::new();
        for i in 0..5u8 {
            match append(&store, &topic, &note.update(&[i]), &limits).await {
                Appended::Stored { crowded, .. } => said.push(crowded),
                other => panic!("{name}: {other:?}"),
            }
        }
        assert_eq!(said, vec![false, false, false, true, true], "{name}");
        store
            .compact(&topic, &proposal(&note.base(5)), &MediaLimits::default(), 0)
            .await
            .unwrap()
            .unwrap();
        match append(&store, &topic, &note.update(b"after"), &limits).await {
            Appended::Stored { crowded, .. } => assert!(!crowded, "{name}"),
            other => panic!("{name}: {other:?}"),
        }
    }
}

#[tokio::test]
async fn a_relay_at_its_ceiling_takes_no_new_notes_and_keeps_the_old_ones() {
    // Memory only: the ceiling is over every note the store holds, and a
    // shared test database holds everybody's.
    let store: Arc<dyn RelayStore> = Arc::new(MemoryStore::new());
    let mut old = Note::new();
    let first = old.update(b"x");
    let limits = Limits {
        ceiling: Some(first.len() as u64),
        ..Limits::default()
    };
    assert!(matches!(
        append(&store, &old.topic(), &first, &limits).await,
        Appended::Stored { .. }
    ));
    let mut new = Note::new();
    let update = new.update(b"x");
    assert_eq!(
        append(&store, &new.topic(), &update, &limits).await,
        Appended::RelayFull
    );
    assert_eq!(store.head(&new.topic()).await.unwrap(), 0);
    let update = old.update(b"y");
    assert!(matches!(
        append(&store, &old.topic(), &update, &limits).await,
        Appended::Stored { .. }
    ));
}
