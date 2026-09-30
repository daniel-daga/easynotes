//! Invites, end to end: real cores asking a real relay over HTTP.
//!
//! The acceptance for Slice 3 item 3 at the protocol level, as ADR 0025 left
//! it. A link that carries a token rather than a key, a token that expires,
//! and a link that one person claims and nobody after them can — and, because
//! this is the item that opens the relay's protocol, a relay that learns
//! nothing from any of it that ADR 0014 says it must not.

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
    let granted = invite::open(http, None, &outgoing.secret, &outgoing.owner, expires_in)
        .await
        .unwrap();
    outgoing.expires_at = granted as i64;
    owner.remember_invite(&outgoing).unwrap();
    (owner, outgoing)
}

/// Somebody new who has read the link and asked to join as `name`.
async fn ask(
    http: &str,
    link: &str,
    name: &str,
) -> Result<(PendingJoin, MemberIdentity), CoreError> {
    let member = RecoveryPhrase::generate().root().member();
    ask_as(http, link, &member, name)
        .await
        .map(|join| (join, member))
}

/// `member` asking to join through `link` as `name`.
async fn ask_as(
    http: &str,
    link: &str,
    member: &MemberIdentity,
    name: &str,
) -> Result<PendingJoin, CoreError> {
    let secret = InviteSecret::from_link(link)?;
    let sealed = easynotes_core::invite::seal_request(&secret, member, name)?;
    let request = invite::request(http, None, &secret, &member.id(), &sealed).await?;
    Ok(PendingJoin {
        secret,
        request,
        name: name.to_string(),
    })
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
async fn whoever_opens_the_link_gets_the_note_and_can_sync_it() {
    let (relay, _) = relay().await;
    let http = relay.http_base();
    let (owner, outgoing) = owner_with_invite(&http, 3600).await;

    // The link is all that travels, and it holds no key.
    let link = outgoing.secret.link();
    let mut guest = person("Anna");
    let secret = InviteSecret::from_link(&link).unwrap();
    let sealed = guest.seal_join_request(&secret).unwrap();
    let member = guest.member().unwrap();
    let request_id = invite::request(&http, None, &secret, &member.id(), &sealed)
        .await
        .unwrap();
    let join = PendingJoin {
        secret: secret.clone(),
        request: request_id,
        name: "Anna".into(),
    };
    guest.remember_join(&join).unwrap();

    // Nothing yet: the owner's device has not seen it.
    match invite::poll(&http, None, &join.secret, &member, &join.request)
        .await
        .unwrap()
    {
        Answer::Pending { expires_in } => assert!(expires_in > 3500),
        _ => panic!("answered before the owner's device did anything"),
    }

    // The owner's device finds the request, with the name the guest's
    // account shares under.
    let listing = invite::list(&http, None, &outgoing.secret, &outgoing.owner)
        .await
        .unwrap();
    assert!(listing.open);
    assert_eq!(listing.requests.len(), 1);
    let listed = &listing.requests[0];
    assert_eq!(listed.state, RequestState::Pending);
    let request = listed.join.clone().expect("the request opens");
    assert_eq!(request.name, "Anna");

    // And lets them in, asking nobody: saved, then the package out and back,
    // then the answer.
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
        None,
        &outgoing.secret,
        &outgoing.owner,
        &listed.request,
        &answer,
    )
    .await
    .unwrap();
    owner.core.forget_invite(outgoing.secret.id()).unwrap();

    // The guest opens the answer, follows the note, and reads it.
    let (got, owner_key) = match invite::poll(&http, None, &join.secret, &member, &join.request)
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

    let anna_member = RecoveryPhrase::generate().root().member();
    let anna = ask_as(&http, &link, &anna_member, "Anna").await.unwrap();

    // The link is claimed: anybody else is told so, before anyone answers.
    is_invite_error(ask(&http, &link, "Someone else").await, "already been used");

    // Anna asking again — a retry after a lost reply, or her other phone —
    // is the same request, not a second one.
    let again = ask_as(&http, &link, &anna_member, "Anna").await.unwrap();
    assert_eq!(again.request, anna.request);

    let listing = invite::list(&http, None, &outgoing.secret, &outgoing.owner)
        .await
        .unwrap();
    assert_eq!(listing.requests.len(), 1);
    let answer = answer_for(&outgoing, listing.requests[0].join.as_ref().unwrap());
    invite::approve(
        &http,
        None,
        &outgoing.secret,
        &outgoing.owner,
        &anna.request,
        &answer,
    )
    .await
    .unwrap();
    let listing = invite::list(&http, None, &outgoing.secret, &outgoing.owner)
        .await
        .unwrap();
    assert!(!listing.open);

    // Still spent, and answering it again is refused.
    is_invite_error(ask(&http, &link, "Late").await, "already been used");
    is_invite_error(
        invite::approve(
            &http,
            None,
            &outgoing.secret,
            &outgoing.owner,
            &anna.request,
            &answer,
        )
        .await,
        "answered",
    );
    // Anna can still find her answer.
    let again = ask_as(&http, &link, &anna_member, "Anna").await.unwrap();
    assert!(matches!(
        invite::poll(&http, None, &again.secret, &anna_member, &again.request)
            .await
            .unwrap(),
        Answer::Approved { .. }
    ));
}

#[tokio::test]
async fn a_request_filed_under_someone_elses_name_is_not_let_in() {
    // Somebody with the link who knows Anna's member ID files their own
    // request under the name hers would have, to take her place. The owner's
    // device reads it as unreadable, and it is never let in.
    let (relay, _) = relay().await;
    let http = relay.http_base();
    let (_owner, outgoing) = owner_with_invite(&http, 3600).await;
    let anna = RecoveryPhrase::generate().root().member();
    let squatter = RecoveryPhrase::generate().root().member();
    let sealed = easynotes_core::invite::seal_request(&outgoing.secret, &squatter, "Anna").unwrap();
    invite::request(&http, None, &outgoing.secret, &anna.id(), &sealed)
        .await
        .unwrap();

    let listing = invite::list(&http, None, &outgoing.secret, &outgoing.owner)
        .await
        .unwrap();
    assert_eq!(listing.requests.len(), 1);
    assert!(listing.requests[0].join.is_none());
}

#[tokio::test]
async fn an_answer_meant_for_one_person_does_not_open_for_another() {
    // Two people hold the link. One claims it and is let in. The other can
    // read the relay's answer to that request — it only needs the link — and
    // still cannot open it, because it is sealed to a key only the person let
    // in has.
    let (relay, _) = relay().await;
    let http = relay.http_base();
    let (_owner, outgoing) = owner_with_invite(&http, 3600).await;
    let (anna, _) = ask(&http, &outgoing.secret.link(), "Anna").await.unwrap();

    let listing = invite::list(&http, None, &outgoing.secret, &outgoing.owner)
        .await
        .unwrap();
    let answer = answer_for(&outgoing, listing.requests[0].join.as_ref().unwrap());
    invite::approve(
        &http,
        None,
        &outgoing.secret,
        &outgoing.owner,
        &anna.request,
        &answer,
    )
    .await
    .unwrap();

    let eavesdropper = RecoveryPhrase::generate().root().member();
    assert!(
        invite::poll(&http, None, &anna.secret, &eavesdropper, &anna.request)
            .await
            .is_err(),
        "an answer opened for someone it was not sealed to"
    );
}

#[tokio::test]
async fn a_declined_request_is_told_and_spends_the_link() {
    let (relay, _) = relay().await;
    let http = relay.http_base();
    let (_owner, outgoing) = owner_with_invite(&http, 3600).await;
    let link = outgoing.secret.link();
    let (stranger, member) = ask(&http, &link, "Not who you think").await.unwrap();

    invite::decline(
        &http,
        None,
        &outgoing.secret,
        &outgoing.owner,
        &stranger.request,
    )
    .await
    .unwrap();
    assert!(matches!(
        invite::poll(&http, None, &stranger.secret, &member, &stranger.request)
            .await
            .unwrap(),
        Answer::Declined
    ));
    // One link, one request: the owner sends a new link.
    is_invite_error(ask(&http, &link, "Anna").await, "already been used");
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
            invite::list(&http, None, &guest.secret, &forged).await,
            "does not exist",
        );
        is_invite_error(
            invite::decline(&http, None, &guest.secret, &forged, &guest.request).await,
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
        invite::request(&http, None, &nobody, &member.id(), &sealed).await,
        "does not exist",
    );
    is_invite_error(
        invite::poll(&http, None, &nobody, &member, "whatever").await,
        "does not exist",
    );
}

#[tokio::test]
async fn a_withdrawn_invite_is_gone() {
    let (relay, _) = relay().await;
    let http = relay.http_base();
    let (_owner, outgoing) = owner_with_invite(&http, 3600).await;
    let (waiting, member) = ask(&http, &outgoing.secret.link(), "Anna").await.unwrap();

    invite::withdraw(&http, None, &outgoing.secret, &outgoing.owner)
        .await
        .unwrap();
    is_invite_error(
        ask(&http, &outgoing.secret.link(), "Late").await,
        "does not exist",
    );
    is_invite_error(
        invite::poll(&http, None, &waiting.secret, &member, &waiting.request).await,
        "does not exist",
    );
}

#[tokio::test]
async fn an_invite_expires() {
    let (relay, _) = relay().await;
    let http = relay.http_base();
    // Two seconds, not one: the relay keeps whole seconds, so an invite asked
    // to last one second and opened just before a second ticks over expires
    // almost at once, before the request in time can be made. Two leave at
    // least one full second for it, and the sleep still ends past expiry.
    let (_owner, outgoing) = owner_with_invite(&http, 2).await;
    let (early, _) = ask(&http, &outgoing.secret.link(), "Anna").await.unwrap();

    tokio::time::sleep(std::time::Duration::from_millis(3100)).await;

    is_invite_error(ask(&http, &outgoing.secret.link(), "Late").await, "expired");
    // Nor can a request made in time be approved after it.
    let listing = invite::list(&http, None, &outgoing.secret, &outgoing.owner)
        .await
        .unwrap();
    assert!(!listing.open);
    assert_eq!(listing.expires_in, 0);
    let answer = answer_for(&outgoing, listing.requests[0].join.as_ref().unwrap());
    is_invite_error(
        invite::approve(
            &http,
            None,
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
        None,
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
    let request = invite::request(
        &http,
        None,
        &outgoing.secret,
        &guest.member().unwrap().id(),
        &sealed,
    )
    .await
    .unwrap();
    let listing = invite::list(&http, None, &outgoing.secret, &outgoing.owner)
        .await
        .unwrap();
    let answer = answer_for(&outgoing, listing.requests[0].join.as_ref().unwrap());
    invite::approve(
        &http,
        None,
        &outgoing.secret,
        &outgoing.owner,
        &request,
        &answer,
    )
    .await
    .unwrap();
    let member = guest.member().unwrap();
    let Answer::Approved { .. } = invite::poll(&http, None, &outgoing.secret, &member, &request)
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
