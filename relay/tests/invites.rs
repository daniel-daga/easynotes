//! Invites, end to end: real cores asking a real relay over HTTP.
//!
//! The acceptance for Slice 3 item 3 at the protocol level, as ADR 0023 left
//! it. A link that carries a token rather than a key, a token that expires,
//! and an owner who approves a person before anything is handed over — and,
//! because this is the item that opens the relay's protocol, a relay that
//! learns nothing from any of it that ADR 0014 says it must not.

mod support;

use easynotes_core::account::{MemberIdentity, RecoveryPhrase};
use easynotes_core::invite::{InviteSecret, OwnerSecret};
use easynotes_core::note::{NoteCore, NoteKind, OutgoingInvite, PendingJoin};
use easynotes_core::store::{MemoryKeyring, MemoryStore as CoreStore};
use easynotes_core::sync::invite::{self, Answer};
use easynotes_core::sync::RequestState;
use easynotes_core::CoreError;
use easynotes_relay::{MemoryStore, Relay};
use std::sync::Arc;
use support::{contains, Core, Device, Tap};

async fn relay() -> (Relay, MemoryStore) {
    let store = MemoryStore::new();
    let relay = Relay::serve(Arc::new(store.clone()), "127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    (relay, store)
}

/// A device that can share: an account, and a name.
fn person(name: &str) -> Core {
    let mut core: Core = NoteCore::open(CoreStore::new(), MemoryKeyring::new()).unwrap();
    core.create_account().unwrap();
    core.set_profile_name(name).unwrap();
    core
}

/// An owner with a note and an invite to it open on the relay.
async fn owner_with_invite(http: &str, expires_in: u64) -> (Core, OutgoingInvite) {
    let mut owner = person("Olga");
    let note = owner.create_note(NoteKind::Checklist).unwrap();
    let mut outgoing = owner.new_invite(note).unwrap();
    let granted = invite::open(http, &outgoing.secret, &outgoing.owner, expires_in)
        .await
        .unwrap();
    outgoing.expires_at = granted as i64;
    owner.remember_invite(&outgoing).unwrap();
    (owner, outgoing)
}

/// Somebody who has read the link and asked to join as `name`.
async fn ask(
    http: &str,
    link: &str,
    name: &str,
) -> Result<(PendingJoin, MemberIdentity), CoreError> {
    let secret = InviteSecret::from_link(link)?;
    let member = RecoveryPhrase::generate().root().member();
    let sealed = easynotes_core::invite::seal_request(&secret, &member, name)?;
    let request = invite::request(http, &secret, &sealed).await?;
    Ok((
        PendingJoin {
            secret,
            request,
            member: member.id(),
            name: name.to_string(),
        },
        member,
    ))
}

/// An answer for a request, sealed as an owner would. Enough for the tests
/// that are about the invite protocol rather than about packages.
fn answer_for(outgoing: &OutgoingInvite, request: &easynotes_core::invite::JoinRequest) -> Vec<u8> {
    let owner = easynotes_core::crypto::DeviceIdentity::generate().public_key();
    easynotes_core::invite::seal_answer(&outgoing.secret, request, outgoing.note, owner).unwrap()
}

fn is_invite_error(result: Result<impl Sized, CoreError>, needle: &str) {
    match result {
        Err(CoreError::Invite(words)) => {
            assert!(words.contains(needle), "expected {needle:?} in {words:?}")
        }
        Err(other) => panic!("expected an invite error, got {other}"),
        Ok(_) => panic!("expected an invite error, got success"),
    }
}

#[tokio::test]
async fn an_approved_person_gets_the_note_and_can_sync_it() {
    let (relay, _) = relay().await;
    let http = relay.http_base();
    let (owner, outgoing) = owner_with_invite(&http, 3600).await;

    // The link is all that travels, and it holds no key.
    let link = outgoing.secret.link();
    let mut guest = person("Anna");
    let secret = InviteSecret::from_link(&link).unwrap();
    let sealed = guest.seal_join_request(&secret).unwrap();
    let request_id = invite::request(&http, &secret, &sealed).await.unwrap();
    let member = guest.member().unwrap();
    let join = PendingJoin {
        secret: secret.clone(),
        request: request_id,
        member: member.id(),
        name: "Anna".into(),
    };
    guest.remember_join(&join).unwrap();

    // Nothing yet: the owner has not said.
    match invite::poll(&http, &join.secret, &member, &join.request)
        .await
        .unwrap()
    {
        Answer::Pending { expires_in } => assert!(expires_in > 3500),
        _ => panic!("answered before the owner said anything"),
    }

    // The owner sees the request, with the name the guest's account shares
    // under and the same six digits the guest's screen shows.
    let listing = invite::list(&http, &outgoing.secret, &outgoing.owner)
        .await
        .unwrap();
    assert!(listing.open);
    assert_eq!(listing.requests.len(), 1);
    let listed = &listing.requests[0];
    assert_eq!(listed.state, RequestState::Pending);
    let request = listed.join.clone().expect("the request opens");
    assert_eq!(request.name, "Anna");
    assert_eq!(request.code(&outgoing.secret), join.code());

    // Approve: saved, then the package out and back, then the answer.
    let note = outgoing.note;
    let mut owner = Device::with_core("owner", owner, note, &http, &relay.ws_base()).await;
    owner
        .core
        .approve(outgoing.secret.id(), &listed.request, &request)
        .unwrap();
    owner.settle().await;
    owner.pump().await;
    let answer = owner
        .core
        .approval_answer(outgoing.secret.id())
        .unwrap()
        .expect("the package is on the relay");
    invite::approve(
        &http,
        &outgoing.secret,
        &outgoing.owner,
        &listed.request,
        &answer,
    )
    .await
    .unwrap();
    owner.core.forget_invite(outgoing.secret.id()).unwrap();

    // The guest opens the answer, follows the note, and reads it.
    let (got, owner_key) = match invite::poll(&http, &join.secret, &member, &join.request)
        .await
        .unwrap()
    {
        Answer::Approved { note, owner } => (note, owner),
        _ => panic!("approved, but the guest was not told"),
    };
    assert_eq!(got, note);
    guest
        .accept_answer(join.secret.id(), got, owner_key)
        .unwrap();
    owner.set_title("Groceries").await;
    owner.add_item("oat milk").await;
    owner.pump().await;
    let mut b = Device::with_core("guest", guest, note, &http, &relay.ws_base()).await;
    b.pump().await;
    assert!(b.core.can_open(note));
    b.core.forget_join(join.secret.id()).unwrap();
    assert_eq!(b.texts(), vec!["oat milk".to_string()]);
    assert_eq!(b.view().title, "Groceries");
}

#[tokio::test]
async fn an_invite_admits_one_person() {
    let (relay, _) = relay().await;
    let http = relay.http_base();
    let (_owner, outgoing) = owner_with_invite(&http, 3600).await;
    let link = outgoing.secret.link();

    let (_first, _) = ask(&http, &link, "Anna").await.unwrap();
    let (second, second_member) = ask(&http, &link, "Someone else").await.unwrap();

    let listing = invite::list(&http, &outgoing.secret, &outgoing.owner)
        .await
        .unwrap();
    let anna = listing
        .requests
        .iter()
        .find(|r| r.join.as_ref().unwrap().name == "Anna")
        .unwrap();
    let answer = answer_for(&outgoing, anna.join.as_ref().unwrap());
    invite::approve(
        &http,
        &outgoing.secret,
        &outgoing.owner,
        &anna.request,
        &answer,
    )
    .await
    .unwrap();

    // The request left waiting is declined, not left hanging.
    assert!(matches!(
        invite::poll(&http, &second.secret, &second_member, &second.request)
            .await
            .unwrap(),
        Answer::Declined
    ));
    // The link is spent.
    is_invite_error(ask(&http, &link, "Late").await, "already been used");
    let listing = invite::list(&http, &outgoing.secret, &outgoing.owner)
        .await
        .unwrap();
    assert!(!listing.open);

    // Approving again — the second request, or the first twice — is refused.
    let other = listing
        .requests
        .iter()
        .find(|r| r.request == second.request)
        .unwrap();
    let answer = answer_for(&outgoing, other.join.as_ref().unwrap());
    is_invite_error(
        invite::approve(
            &http,
            &outgoing.secret,
            &outgoing.owner,
            &other.request,
            &answer,
        )
        .await,
        "answered",
    );
}

#[tokio::test]
async fn an_answer_meant_for_one_person_does_not_open_for_another() {
    // Two people hold the link. The owner approves one. The other can read the
    // relay's answer to that request — it only needs the link — and still
    // cannot open it, because it is sealed to a key only the approved person
    // has.
    let (relay, _) = relay().await;
    let http = relay.http_base();
    let (_owner, outgoing) = owner_with_invite(&http, 3600).await;
    let (anna, _) = ask(&http, &outgoing.secret.link(), "Anna").await.unwrap();

    let listing = invite::list(&http, &outgoing.secret, &outgoing.owner)
        .await
        .unwrap();
    let answer = answer_for(&outgoing, listing.requests[0].join.as_ref().unwrap());
    invite::approve(
        &http,
        &outgoing.secret,
        &outgoing.owner,
        &anna.request,
        &answer,
    )
    .await
    .unwrap();

    let eavesdropper = RecoveryPhrase::generate().root().member();
    assert!(
        invite::poll(&http, &anna.secret, &eavesdropper, &anna.request)
            .await
            .is_err(),
        "an answer opened for someone it was not sealed to"
    );
}

#[tokio::test]
async fn a_declined_person_is_told_and_the_invite_stays_open() {
    let (relay, _) = relay().await;
    let http = relay.http_base();
    let (_owner, outgoing) = owner_with_invite(&http, 3600).await;
    let link = outgoing.secret.link();
    let (stranger, member) = ask(&http, &link, "Not who you think").await.unwrap();

    invite::decline(&http, &outgoing.secret, &outgoing.owner, &stranger.request)
        .await
        .unwrap();
    assert!(matches!(
        invite::poll(&http, &stranger.secret, &member, &stranger.request)
            .await
            .unwrap(),
        Answer::Declined
    ));
    // Declining a leaked link's stranger does not spend it on the person it
    // was meant for.
    assert!(ask(&http, &link, "Anna").await.is_ok());
}

#[tokio::test]
async fn a_guest_cannot_answer_their_own_request() {
    let (relay, _) = relay().await;
    let http = relay.http_base();
    let (_owner, outgoing) = owner_with_invite(&http, 3600).await;
    let (guest, _) = ask(&http, &outgoing.secret.link(), "Me").await.unwrap();

    // Everything a guest has, tried as the owner's secret.
    for forged in [OwnerSecret::random()] {
        is_invite_error(
            invite::list(&http, &guest.secret, &forged).await,
            "does not exist",
        );
        is_invite_error(
            invite::decline(&http, &guest.secret, &forged, &guest.request).await,
            "does not exist",
        );
    }
}

#[tokio::test]
async fn a_wrong_link_learns_nothing() {
    let (relay, _) = relay().await;
    let http = relay.http_base();
    let (_owner, _outgoing) = owner_with_invite(&http, 3600).await;

    // A link nobody made: the same answer as a wrong token, so a wrong token
    // cannot tell anyone the invite exists.
    let nobody = InviteSecret::random();
    let member = RecoveryPhrase::generate().root().member();
    let sealed = easynotes_core::invite::seal_request(&nobody, &member, "x").unwrap();
    is_invite_error(
        invite::request(&http, &nobody, &sealed).await,
        "does not exist",
    );
    is_invite_error(
        invite::poll(&http, &nobody, &member, "whatever").await,
        "does not exist",
    );
}

#[tokio::test]
async fn a_withdrawn_invite_is_gone() {
    let (relay, _) = relay().await;
    let http = relay.http_base();
    let (_owner, outgoing) = owner_with_invite(&http, 3600).await;
    let (waiting, member) = ask(&http, &outgoing.secret.link(), "Anna").await.unwrap();

    invite::withdraw(&http, &outgoing.secret, &outgoing.owner)
        .await
        .unwrap();
    is_invite_error(
        ask(&http, &outgoing.secret.link(), "Late").await,
        "does not exist",
    );
    is_invite_error(
        invite::poll(&http, &waiting.secret, &member, &waiting.request).await,
        "does not exist",
    );
}

#[tokio::test]
async fn an_invite_expires() {
    let (relay, _) = relay().await;
    let http = relay.http_base();
    let (_owner, outgoing) = owner_with_invite(&http, 1).await;
    let (early, _) = ask(&http, &outgoing.secret.link(), "Anna").await.unwrap();

    tokio::time::sleep(std::time::Duration::from_millis(2100)).await;

    is_invite_error(ask(&http, &outgoing.secret.link(), "Late").await, "expired");
    // Nor can a request made in time be approved after it.
    let listing = invite::list(&http, &outgoing.secret, &outgoing.owner)
        .await
        .unwrap();
    assert!(!listing.open);
    assert_eq!(listing.expires_in, 0);
    let answer = answer_for(&outgoing, listing.requests[0].join.as_ref().unwrap());
    is_invite_error(
        invite::approve(
            &http,
            &outgoing.secret,
            &outgoing.owner,
            &early.request,
            &answer,
        )
        .await,
        "expired",
    );
}

#[tokio::test]
async fn an_invite_cannot_be_asked_to_last_for_ever() {
    let (relay, _) = relay().await;
    let secret = InviteSecret::random();
    let granted = invite::open(
        &relay.http_base(),
        &secret,
        &OwnerSecret::random(),
        u64::MAX,
    )
    .await
    .unwrap();
    assert_eq!(
        granted,
        easynotes_relay::protocol::INVITE_MAX_TTL_SECS,
        "the relay granted more than its cap"
    );
}

#[tokio::test]
async fn a_leaked_link_cannot_fill_the_relay() {
    let (relay, _) = relay().await;
    let http = relay.http_base();
    let (_owner, outgoing) = owner_with_invite(&http, 3600).await;
    let link = outgoing.secret.link();
    for _ in 0..easynotes_relay::protocol::MAX_INVITE_REQUESTS {
        ask(&http, &link, "flood").await.unwrap();
    }
    is_invite_error(ask(&http, &link, "one more").await, "as many requests");
}

#[tokio::test]
async fn an_invite_names_no_device_no_note_and_no_person() {
    // ADR 0014, held against the one item that opens the relay's protocol: the
    // relay must not learn which devices are on either side of an invite,
    // which note it is for, or anything about the person asking.
    let (relay, store) = relay().await;
    let tap = Tap::in_front_of(relay.addr).await;
    let http = tap.http_base();

    let (owner, outgoing) = owner_with_invite(&http, 3600).await;
    let guest = person("Annabelle Quist");
    let sealed = guest.seal_join_request(&outgoing.secret).unwrap();
    let request = invite::request(&http, &outgoing.secret, &sealed)
        .await
        .unwrap();
    let listing = invite::list(&http, &outgoing.secret, &outgoing.owner)
        .await
        .unwrap();
    let answer = answer_for(&outgoing, listing.requests[0].join.as_ref().unwrap());
    invite::approve(&http, &outgoing.secret, &outgoing.owner, &request, &answer)
        .await
        .unwrap();
    let member = guest.member().unwrap();
    let Answer::Approved { .. } = invite::poll(&http, &outgoing.secret, &member, &request)
        .await
        .unwrap()
    else {
        panic!("not approved");
    };

    let note = outgoing.note;
    let owner_member = owner.member().unwrap();
    let forbidden: Vec<(&str, Vec<u8>)> = vec![
        ("the note id", note.as_bytes().to_vec()),
        ("the note id, encoded", note.encode().into_bytes()),
        ("the guest's name", b"Annabelle Quist".to_vec()),
        ("the guest's member id", member.id().0.to_vec()),
        (
            "the guest's member id, encoded",
            member.id().encode().into_bytes(),
        ),
        ("the guest's box key", member.box_public().to_vec()),
        ("the owner's member id", owner_member.id().0.to_vec()),
        (
            "the owner's device key",
            owner.device_key().encode().into_bytes(),
        ),
        (
            "the guest's device key",
            guest.device_key().encode().into_bytes(),
        ),
        ("the owner's device key, raw", owner.device_key().0.to_vec()),
        ("the link's secret", outgoing.secret.code().into_bytes()),
        ("the join token", outgoing.secret.token().into_bytes()),
        ("the owner secret", outgoing.owner.encode().into_bytes()),
    ];

    let stored = store.all_invite_bytes();
    assert!(
        !stored.is_empty(),
        "the test proved nothing: nothing stored"
    );
    let wire = [tap.bytes_to_server(), tap.bytes_to_clients()];
    assert!(
        !wire[0].is_empty() && !wire[1].is_empty(),
        "the tap saw nothing"
    );

    for (what, bytes) in &forbidden {
        for row in &stored {
            assert!(!contains(row, bytes), "the relay stored {what}");
        }
    }
    // The link's secret and the owner's must never be sent at all; the token
    // and the owner secret are sent, as proof, and only their hashes kept —
    // which the stored check above covers. Everything else is never sent.
    for (what, bytes) in forbidden
        .iter()
        .filter(|(what, _)| !matches!(*what, "the join token" | "the owner secret"))
    {
        for direction in &wire {
            assert!(!contains(direction, bytes), "{what} crossed the wire");
        }
    }
}
