//! The Postgres store, exercised against a real database.
//!
//! These tests are skipped unless `DATABASE_URL` is set, so `cargo test` stays
//! runnable on a laptop with no Postgres. CI sets it. Each test uses its own
//! random note IDs, so they neither collide nor need a clean database.

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

#[tokio::test]
async fn answers_the_readiness_check() {
    let store = store_or_skip!();
    store.check().await.unwrap();
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
