//! The Postgres store, exercised against a real database.
//!
//! These tests are skipped unless `DATABASE_URL` is set, so `cargo test` stays
//! runnable on a laptop with no Postgres. CI sets it. Each test uses its own
//! random note IDs, so they neither collide nor need a clean database.

use easynotes_relay::media::{Geometry, Reserved};
use easynotes_relay::{PostgresStore, RelayStore};

/// Returns `None` — and prints why — when there is no database to talk to.
async fn store() -> Option<PostgresStore> {
    let Ok(url) = std::env::var("DATABASE_URL") else {
        eprintln!("skipping: DATABASE_URL is not set");
        return None;
    };
    match PostgresStore::connect(&url).await {
        Ok(s) => Some(s),
        Err(e) => panic!("DATABASE_URL is set but unusable: {e}"),
    }
}

macro_rules! store_or_skip {
    () => {
        match store().await {
            Some(s) => s,
            None => return,
        }
    };
}

fn note() -> String {
    easynotes_core::ids::NoteId::random().encode()
}

/// Opaque like a note ID, and random so that reruns against the shared
/// database do not collide on the token table's primary key.
fn token_id() -> String {
    easynotes_core::ids::NoteId::random().encode()
}

#[tokio::test]
async fn answers_the_readiness_check() {
    let store = store_or_skip!();
    store.check().await.unwrap();
}

/// Usage is what `/metrics` reports against the ceilings. Only ever more here,
/// never exactly: the other tests in this file share the database and append
/// to it at the same time.
#[tokio::test]
async fn usage_counts_a_new_note_and_its_bytes() {
    let store = store_or_skip!();
    let before = store.usage().await.unwrap();
    store.append(&note(), &[7; 300]).await.unwrap();
    let after = store.usage().await.unwrap();
    assert!(after.notes > before.notes);
    assert!(after.log_bytes >= before.log_bytes + 300);
    assert!(after.media_objects >= before.media_objects);
}

#[tokio::test]
async fn assigns_dense_increasing_sequence_numbers() {
    let store = store_or_skip!();
    let note = note();
    for expected in 1..=5 {
        assert_eq!(store.append(&note, b"ciphertext").await.unwrap(), expected);
    }
    assert_eq!(store.head(&note).await.unwrap(), 5);
}

#[tokio::test]
async fn sequence_numbers_are_per_note() {
    let store = store_or_skip!();
    let (a, b) = (note(), note());
    store.append(&a, b"x").await.unwrap();
    store.append(&a, b"x").await.unwrap();
    assert_eq!(
        store.append(&b, b"x").await.unwrap(),
        1,
        "note B inherited note A's numbering"
    );
}

#[tokio::test]
async fn since_replays_only_what_is_newer() {
    let store = store_or_skip!();
    let note = note();
    for i in 0..5u8 {
        store.append(&note, &[i]).await.unwrap();
    }
    let got = store.since(&note, 2).await.unwrap();
    assert_eq!(got.iter().map(|s| s.seq).collect::<Vec<_>>(), [3, 4, 5]);
    assert_eq!(got[0].envelope, vec![2u8]);
    assert!(store.since(&note, 5).await.unwrap().is_empty());
}

#[tokio::test]
async fn head_of_an_unknown_note_is_zero() {
    let store = store_or_skip!();
    assert_eq!(store.head(&note()).await.unwrap(), 0);
    assert!(store.since(&note(), 0).await.unwrap().is_empty());
}

#[tokio::test]
async fn concurrent_appends_to_one_note_do_not_collide() {
    // The insert picks `MAX(seq) + 1` in a single statement and retries on the
    // primary-key conflict, so racing writers still produce a dense sequence.
    let Ok(url) = std::env::var("DATABASE_URL") else {
        eprintln!("skipping: DATABASE_URL is not set");
        return;
    };
    let store = std::sync::Arc::new(PostgresStore::connect(&url).await.unwrap());
    let note = note();

    let mut tasks = Vec::new();
    for i in 0..16u8 {
        let store = store.clone();
        let note = note.clone();
        tasks.push(tokio::spawn(async move {
            store.append(&note, &[i]).await.unwrap()
        }));
    }
    let mut seqs = Vec::new();
    for t in tasks {
        seqs.push(t.await.unwrap());
    }
    seqs.sort();
    assert_eq!(
        seqs,
        (1..=16).collect::<Vec<i64>>(),
        "sequence numbers collided or skipped"
    );
    assert_eq!(store.since(&note, 0).await.unwrap().len(), 16);
}

#[tokio::test]
async fn concurrent_reservations_to_one_note_stay_within_its_budget() {
    // The reservation lock is keyed by the note, so same-note writers queue
    // and the note's sum stays exact: of sixteen racing reservations, only as
    // many as fit succeed.
    let Ok(url) = std::env::var("DATABASE_URL") else {
        eprintln!("skipping: DATABASE_URL is not set");
        return;
    };
    let store = std::sync::Arc::new(PostgresStore::connect(&url).await.unwrap());
    let note = note();
    let geometry = Geometry {
        sealed_len: 1000,
        chunk_size: 1 << 16,
    };
    let limits = easynotes_relay::MediaLimits {
        note_bytes: 3 * geometry.sealed_len,
        ..Default::default()
    };

    let mut tasks = Vec::new();
    for _ in 0..16 {
        let store = store.clone();
        let note = note.clone();
        tasks.push(tokio::spawn(async move {
            let object = easynotes_core::ids::MediaObjectId::random().encode();
            store
                .reserve_media(&note, &object, geometry, &limits, 1_000)
                .await
                .unwrap()
        }));
    }
    let mut new = 0;
    for task in tasks {
        if task.await.unwrap() == Reserved::New {
            new += 1;
        }
    }
    assert_eq!(new, 3, "the note budget was exceeded or under-used");
}

#[tokio::test]
async fn stores_ciphertext_verbatim() {
    // Bytes in, the same bytes out — including a NUL and high bytes, which a
    // TEXT column would have mangled.
    let store = store_or_skip!();
    let note = note();
    let envelope: Vec<u8> = (0..=255u8).collect();
    store.append(&note, &envelope).await.unwrap();
    assert_eq!(store.since(&note, 0).await.unwrap()[0].envelope, envelope);
}

#[tokio::test]
async fn migrating_twice_is_harmless() {
    let store = store_or_skip!();
    store.migrate().await.unwrap();
    store.migrate().await.unwrap();
}

/// Connecting brings the schema to this build's, and records it (ADR 0029).
#[tokio::test]
async fn records_every_migration_it_applies() {
    let store = store_or_skip!();
    let newest = easynotes_relay::MIGRATIONS.last().unwrap().version;
    assert_eq!(store.schema_version().await.unwrap(), newest);
}

/// A token is created secret-only, bound by exactly one device, and stays
/// revoked once revoked (ADR 0038). The bound device is the whole point: a
/// second bind must lose, because two devices sharing one token would be two
/// devices with one key.
#[tokio::test]
async fn a_token_is_created_bound_and_revoked() {
    let store = store_or_skip!();
    let id = token_id();
    let secret = [7u8; 32];
    let expires = 1_700_000_000;
    store
        .create_access_token(&id, &secret, Some(expires))
        .await
        .unwrap();

    let created = store.access_token(&id).await.unwrap().unwrap();
    assert_eq!(created.secret, secret);
    assert_eq!(created.device_key, None);
    assert_eq!(created.expires_at, Some(expires));
    assert!(!created.read_only);
    assert_eq!(created.revoked_at, None);

    assert!(store.bind_access_token(&id, &[8u8; 32]).await.unwrap());
    assert!(
        !store.bind_access_token(&id, &[9u8; 32]).await.unwrap(),
        "a bound token was bound again"
    );
    let bound = store.access_token(&id).await.unwrap().unwrap();
    assert_eq!(bound.device_key, Some([8u8; 32]));

    store.set_access_token_read_only(&id, true).await.unwrap();
    assert!(store.access_token(&id).await.unwrap().unwrap().read_only);

    store.unbind_access_token(&id).await.unwrap();
    assert_eq!(
        store.access_token(&id).await.unwrap().unwrap().device_key,
        None
    );

    store.revoke_access_token(&id, 1_700_000_100).await.unwrap();
    store.revoke_access_token(&id, 1_700_000_200).await.unwrap();
    assert_eq!(
        store.access_token(&id).await.unwrap().unwrap().revoked_at,
        Some(1_700_000_100),
        "revoking twice moved the revocation time"
    );

    let missing = token_id();
    assert!(store.access_token(&missing).await.unwrap().is_none());
    assert!(!store.bind_access_token(&missing, &[0u8; 32]).await.unwrap());
}

/// Instances start together on a deploy, and each runs the migrations. They
/// queue on the lock; none fails and none applies anything twice.
#[tokio::test]
async fn instances_starting_together_migrate_once() {
    let Ok(url) = std::env::var("DATABASE_URL") else {
        eprintln!("skipping: DATABASE_URL is not set");
        return;
    };
    let starts = (0..4).map(|_| PostgresStore::connect(&url));
    for start in futures_util::future::join_all(starts).await {
        let store = start.expect("an instance failed to start");
        assert_eq!(
            store.schema_version().await.unwrap(),
            easynotes_relay::MIGRATIONS.last().unwrap().version
        );
    }
}
