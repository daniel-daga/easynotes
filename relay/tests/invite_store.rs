//! The invite half of the store, held to one set of rules in both stores.
//!
//! The memory store always; Postgres when `DATABASE_URL` is set, which CI does.
//! The concurrency tests are the reason this exists: the memory store answers
//! them under one mutex and cannot get them wrong, and ADR 0003 records two
//! Postgres bugs that only a real database found. An invite's cap and its one
//! approval are the same shape of rule as a dense sequence number, so they get
//! the same treatment.

use easynotes_relay::protocol::RequestState;
use easynotes_relay::store::{AddRequest, Answered, NewInvite};
use easynotes_relay::{MemoryStore, PostgresStore, RelayStore};
use std::sync::Arc;

const NOW: i64 = 1_800_000_000;

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

fn id() -> String {
    easynotes_core::ids::InviteId::random().encode()
}

async fn open(store: &Arc<dyn RelayStore>, expires_at: i64) -> String {
    let id = id();
    let created = store
        .create_invite(
            NewInvite {
                id: &id,
                token_hash: &[1u8; 32],
                owner_hash: &[2u8; 32],
                expires_at,
                pre_grant: None,
            },
            NOW,
        )
        .await
        .unwrap();
    assert!(created);
    id
}

#[tokio::test]
async fn an_invite_reads_back_as_it_was_opened() {
    for (name, store) in stores().await {
        let id = open(&store, NOW + 60).await;
        let invite = store.invite(&id).await.unwrap().unwrap();
        assert_eq!(invite.token_hash, vec![1u8; 32], "{name}");
        assert_eq!(invite.owner_hash, vec![2u8; 32], "{name}");
        assert_eq!(invite.expires_at, NOW + 60, "{name}");
        assert!(invite.open, "{name}");
        assert!(invite.requests.is_empty(), "{name}");
        assert!(store.invite(&self::id()).await.unwrap().is_none(), "{name}");
    }
}

#[tokio::test]
async fn an_invite_id_cannot_be_opened_twice() {
    for (name, store) in stores().await {
        let id = open(&store, NOW + 60).await;
        let again = store
            .create_invite(
                NewInvite {
                    id: &id,
                    token_hash: &[9u8; 32],
                    owner_hash: &[9u8; 32],
                    pre_grant: None,
                    expires_at: NOW + 60,
                },
                NOW,
            )
            .await
            .unwrap();
        assert!(!again, "{name}: a second open replaced the first");
        let kept = store.invite(&id).await.unwrap().unwrap();
        assert_eq!(kept.owner_hash, vec![2u8; 32], "{name}: the owner changed");
    }
}

#[tokio::test]
async fn an_invite_takes_one_request_and_the_same_one_again() {
    for (name, store) in stores().await {
        let id = open(&store, NOW + 60).await;
        let payload: Vec<u8> = (0..=255u8).collect();
        assert_eq!(
            store
                .add_invite_request(&id, "r1", &payload, NOW)
                .await
                .unwrap(),
            AddRequest::Added,
            "{name}"
        );
        // The same person again — a retry, or their other phone — is given
        // back what they left, which is not overwritten.
        assert_eq!(
            store
                .add_invite_request(&id, "r1", b"other", NOW)
                .await
                .unwrap(),
            AddRequest::Again,
            "{name}"
        );
        // Anybody else finds the link used.
        assert_eq!(
            store
                .add_invite_request(&id, "r2", &payload, NOW)
                .await
                .unwrap(),
            AddRequest::Closed,
            "{name}"
        );
        let invite = store.invite(&id).await.unwrap().unwrap();
        let ids: Vec<_> = invite.requests.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, ["r1"], "{name}");
        assert_eq!(invite.requests[0].payload, payload, "{name}");
        assert_eq!(invite.requests[0].state, RequestState::Pending, "{name}");
        assert!(invite.open, "{name}: asking is not being let in");
    }
}

#[tokio::test]
async fn an_expired_invite_takes_no_requests() {
    for (name, store) in stores().await {
        let id = open(&store, NOW).await;
        assert_eq!(
            store.add_invite_request(&id, "r", b"x", NOW).await.unwrap(),
            AddRequest::Closed,
            "{name}"
        );
        assert_eq!(
            store
                .add_invite_request(&self::id(), "r", b"x", NOW)
                .await
                .unwrap(),
            AddRequest::Missing,
            "{name}"
        );
        // A request made in time can still be found after it, so that its
        // person learns how it went.
        let id = open(&store, NOW + 10).await;
        store.add_invite_request(&id, "r", b"x", NOW).await.unwrap();
        assert_eq!(
            store
                .add_invite_request(&id, "r", b"x", NOW + 10)
                .await
                .unwrap(),
            AddRequest::Again,
            "{name}"
        );
    }
}

#[tokio::test]
async fn approving_closes_the_invite() {
    for (name, store) in stores().await {
        let id = open(&store, NOW + 60).await;
        store.add_invite_request(&id, "a", b"x", NOW).await.unwrap();
        assert_eq!(
            store
                .answer_invite_request(&id, "a", Some(b"grant"), NOW)
                .await
                .unwrap(),
            Answered::Done,
            "{name}"
        );
        let invite = store.invite(&id).await.unwrap().unwrap();
        assert!(!invite.open, "{name}");
        let states: Vec<_> = invite
            .requests
            .iter()
            .map(|r| (r.id.as_str(), r.state, r.grant.clone()))
            .collect();
        assert_eq!(
            states,
            [("a", RequestState::Approved, Some(b"grant".to_vec()))],
            "{name}"
        );
        assert_eq!(
            store
                .answer_invite_request(&id, "a", None, NOW)
                .await
                .unwrap(),
            Answered::NotPending,
            "{name}"
        );
        assert_eq!(
            store.add_invite_request(&id, "a", b"x", NOW).await.unwrap(),
            AddRequest::Again,
            "{name}"
        );
        assert_eq!(
            store.add_invite_request(&id, "d", b"x", NOW).await.unwrap(),
            AddRequest::Closed,
            "{name}"
        );
        assert_eq!(
            store
                .answer_invite_request(&id, "zz", None, NOW)
                .await
                .unwrap(),
            Answered::Missing,
            "{name}"
        );
    }
}

#[tokio::test]
async fn a_declined_request_still_spends_the_invite() {
    for (name, store) in stores().await {
        let id = open(&store, NOW + 60).await;
        store.add_invite_request(&id, "a", b"x", NOW).await.unwrap();
        assert_eq!(
            store
                .answer_invite_request(&id, "a", None, NOW)
                .await
                .unwrap(),
            Answered::Done,
            "{name}"
        );
        assert_eq!(
            store.add_invite_request(&id, "b", b"x", NOW).await.unwrap(),
            AddRequest::Closed,
            "{name}"
        );
    }
}

#[tokio::test]
async fn an_expired_invite_cannot_be_approved_but_can_be_declined() {
    for (name, store) in stores().await {
        let id = open(&store, NOW + 10).await;
        store.add_invite_request(&id, "a", b"x", NOW).await.unwrap();
        let later = NOW + 10;
        assert_eq!(
            store
                .answer_invite_request(&id, "a", Some(b"g"), later)
                .await
                .unwrap(),
            Answered::Closed,
            "{name}"
        );
        assert_eq!(
            store
                .answer_invite_request(&id, "a", None, later)
                .await
                .unwrap(),
            Answered::Done,
            "{name}"
        );
    }
}

#[tokio::test]
async fn one_of_a_crowd_claims_the_invite() {
    for (name, store) in stores().await {
        let id = open(&store, NOW + 60).await;
        let tasks: Vec<_> = (0..32)
            .map(|n| {
                let store = store.clone();
                let id = id.clone();
                tokio::spawn(async move {
                    store
                        .add_invite_request(&id, &format!("r{n}"), b"x", NOW)
                        .await
                        .unwrap()
                })
            })
            .collect();
        let mut added = 0;
        for t in tasks {
            match t.await.unwrap() {
                AddRequest::Added => added += 1,
                AddRequest::Closed => {}
                other => panic!("{name}: {other:?}"),
            }
        }
        assert_eq!(added, 1, "{name}: more than one person claimed the link");
        let kept = store.invite(&id).await.unwrap().unwrap().requests.len();
        assert_eq!(kept, 1, "{name}");
    }
}

#[tokio::test]
async fn only_one_of_many_approvals_wins() {
    for (name, store) in stores().await {
        let id = open(&store, NOW + 60).await;
        store.add_invite_request(&id, "r", b"x", NOW).await.unwrap();
        let tasks: Vec<_> = (0..8)
            .map(|_| {
                let store = store.clone();
                let id = id.clone();
                tokio::spawn(async move {
                    store
                        .answer_invite_request(&id, "r", Some(b"g"), NOW)
                        .await
                        .unwrap()
                })
            })
            .collect();
        let mut done = 0;
        for t in tasks {
            if t.await.unwrap() == Answered::Done {
                done += 1;
            }
        }
        assert_eq!(done, 1, "{name}: one request was answered more than once");
    }
}

#[tokio::test]
async fn withdrawing_removes_the_requests_too() {
    for (name, store) in stores().await {
        let id = open(&store, NOW + 60).await;
        store.add_invite_request(&id, "a", b"x", NOW).await.unwrap();
        store.withdraw_invite(&id).await.unwrap();
        assert!(store.invite(&id).await.unwrap().is_none(), "{name}");
        // And a second withdrawal is not an error.
        store.withdraw_invite(&id).await.unwrap();
        // The ID is free again, with nothing left over from before.
        let reopened = store
            .create_invite(
                NewInvite {
                    id: &id,
                    token_hash: &[1u8; 32],
                    owner_hash: &[2u8; 32],
                    pre_grant: None,
                    expires_at: NOW + 60,
                },
                NOW,
            )
            .await
            .unwrap();
        assert!(reopened, "{name}");
        assert!(
            store
                .invite(&id)
                .await
                .unwrap()
                .unwrap()
                .requests
                .is_empty(),
            "{name}: a request outlived its invite"
        );
    }
}

#[tokio::test]
async fn an_invite_long_past_expiry_is_swept() {
    use easynotes_relay::protocol::INVITE_GRACE_SECS;
    for (name, store) in stores().await {
        let stale = open(&store, NOW - INVITE_GRACE_SECS - 1).await;
        let recent = open(&store, NOW - 1).await;
        // Sweeping happens when an invite is opened.
        open(&store, NOW + 60).await;
        assert!(store.invite(&stale).await.unwrap().is_none(), "{name}");
        // Expired, but inside the grace period: still there to say so.
        assert!(store.invite(&recent).await.unwrap().is_some(), "{name}");
    }
}
