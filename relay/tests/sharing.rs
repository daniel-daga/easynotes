//! Sharing with people, through a real relay, with the engine the app runs.
//!
//! `core/tests/sharing.rs` drives every scenario in the plan envelope by
//! envelope. This runs the same flow the app does — invite endpoints over
//! HTTP, one engine per document, packages published when a note settles —
//! and checks the two things only a real relay can show: that it all works
//! without the test handing anything across, and that the relay learns no
//! member ID, box key or name from any of it (ADR 0023 §9, scenario 9).

mod support;

use easynotes_core::crdt::DefaultNoteDoc;
use easynotes_core::crypto::{Envelope, EnvelopeKind};
use easynotes_core::ids::NoteId;
use easynotes_core::invite::InviteSecret;
use easynotes_core::note::{NoteCore, NoteKind};
use easynotes_core::store::{MemoryKeyring, MemoryStore};
use easynotes_core::sync::invite::{self, Answer};
use easynotes_core::sync::{DeviceSync, Incoming, RelayEndpoint, Routed, SyncEngine, SyncHandle};
use easynotes_relay::{Limits, MemoryStore as RelayLog, Relay, RelayConfig, RelayStore};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use support::{contains, Tap};

type Core = NoteCore<DefaultNoteDoc, MemoryStore, MemoryKeyring>;

const PATIENCE: Duration = Duration::from_secs(15);

/// One person's phone: a core, and an engine for every document it syncs.
struct Phone {
    name: &'static str,
    core: Arc<Mutex<Core>>,
    engines: Arc<Mutex<HashMap<NoteId, SyncHandle>>>,
    endpoint: RelayEndpoint,
    http: String,
}

impl Phone {
    fn new(name: &'static str, endpoint: &RelayEndpoint) -> Self {
        let mut core: Core = NoteCore::open(MemoryStore::new(), MemoryKeyring::new()).unwrap();
        core.create_account().unwrap();
        core.set_profile_name(name).unwrap();
        let engines: Arc<Mutex<HashMap<NoteId, SyncHandle>>> = Arc::default();
        let sink = engines.clone();
        core.on_account_envelope(Box::new(move |id, envelope| {
            if let Some(handle) = sink.lock().unwrap().get(&id) {
                handle.publish(envelope.to_vec());
            }
        }));
        let phone = Self {
            name,
            core: Arc::new(Mutex::new(core)),
            engines,
            endpoint: endpoint.clone(),
            http: endpoint.http_base.clone(),
        };
        phone.connect_everything();
        phone
    }

    /// What the app does when a relay is named and whenever the account
    /// settles: connect what can be synced, and let go of what cannot.
    fn connect_everything(&self) {
        let (mut ids, account) = {
            let core = self.core.lock().unwrap();
            (core.syncable().unwrap(), core.account_id())
        };
        ids.extend(account);
        let mut engines = self.engines.lock().unwrap();
        engines.retain(|id, handle| {
            let keep = ids.contains(id);
            if !keep {
                handle.shutdown();
            }
            keep
        });
        for id in ids {
            if engines.contains_key(&id) {
                continue;
            }
            let (engine, handle) = SyncEngine::start(self.core.clone(), id, self.endpoint.clone());
            tokio::spawn(engine.run());
            engines.insert(id, handle);
        }
    }

    fn settle(&self, note: NoteId) {
        if let Some(handle) = self.engines.lock().unwrap().get(&note) {
            handle.settle();
        }
    }

    fn create_note(&self, title: &str) -> NoteId {
        let (note, envelope) = {
            let mut core = self.core.lock().unwrap();
            let note = core.create_note(NoteKind::Checklist).unwrap();
            let envelope = core.set_title(note, title).unwrap();
            (note, envelope)
        };
        self.connect_everything();
        self.engines.lock().unwrap()[&note].publish(envelope);
        note
    }

    fn set_title(&self, note: NoteId, title: &str) {
        let envelope = self.core.lock().unwrap().set_title(note, title).unwrap();
        self.engines.lock().unwrap()[&note].publish(envelope);
    }

    fn title(&self, note: NoteId) -> Option<String> {
        self.core
            .lock()
            .unwrap()
            .open_note(note)
            .ok()
            .map(|v| v.title)
    }

    async fn wait_until(&self, what: &str, mut ready: impl FnMut() -> bool) {
        let deadline = tokio::time::Instant::now() + PATIENCE;
        while tokio::time::Instant::now() < deadline {
            self.connect_everything();
            if ready() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("{} never reached {what}", self.name);
    }
}

/// The whole invite, as the app runs it: a link, a request, an approval that
/// publishes a package and waits for the relay to have it, an answer, and a
/// guest who follows the note until its key arrives.
async fn let_in(owner: &Phone, guest: &Phone, note: NoteId) {
    let http = owner.http.clone();
    let outgoing = owner.core.lock().unwrap().new_invite(note).unwrap();
    invite::open(&http, &outgoing.secret, &outgoing.owner, 3600)
        .await
        .unwrap();
    owner
        .core
        .lock()
        .unwrap()
        .remember_invite(&outgoing)
        .unwrap();

    let secret = InviteSecret::from_link(&outgoing.secret.link()).unwrap();
    let (sealed, member) = {
        let core = guest.core.lock().unwrap();
        (
            core.seal_join_request(&secret).unwrap(),
            core.member().unwrap().id(),
        )
    };
    let request = invite::request(&http, &secret, &member, &sealed)
        .await
        .unwrap();

    let listing = invite::list(&http, &outgoing.secret, &outgoing.owner)
        .await
        .unwrap();
    let join = listing.requests[0].join.clone().unwrap();
    owner
        .core
        .lock()
        .unwrap()
        .approve(outgoing.secret.id(), &request, &join)
        .unwrap();
    owner.settle(note);
    let mut answer = None;
    owner
        .wait_until("the package on the relay", || {
            answer = owner
                .core
                .lock()
                .unwrap()
                .approval_answer(outgoing.secret.id())
                .unwrap();
            answer.is_some()
        })
        .await;
    invite::approve(
        &http,
        &outgoing.secret,
        &outgoing.owner,
        &request,
        &answer.unwrap(),
    )
    .await
    .unwrap();
    owner
        .core
        .lock()
        .unwrap()
        .forget_invite(outgoing.secret.id())
        .unwrap();

    let member = guest.core.lock().unwrap().member().unwrap();
    let Answer::Approved {
        note: got,
        owner: key,
    } = invite::poll(&http, &secret, &member, &request)
        .await
        .unwrap()
    else {
        panic!("not approved");
    };
    guest
        .core
        .lock()
        .unwrap()
        .accept_answer(secret.id(), got, key)
        .unwrap();
    guest
        .wait_until("the shared note open", || {
            guest.core.lock().unwrap().can_open(note)
        })
        .await;
}

async fn relay() -> (Relay, RelayLog) {
    let log = RelayLog::new();
    let relay = Relay::serve(Arc::new(log.clone()), "127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    (relay, log)
}

#[tokio::test]
async fn a_person_is_removed_and_everyone_else_carries_on() {
    let (relay, _) = relay().await;
    let endpoint = RelayEndpoint::parse(&relay.http_base()).unwrap();
    let owner = Phone::new("Olga", &endpoint);
    let anna = Phone::new("Anna", &endpoint);
    let ben = Phone::new("Ben", &endpoint);
    let note = owner.create_note("Groceries");
    let_in(&owner, &anna, note).await;
    let_in(&owner, &ben, note).await;
    anna.wait_until("the title", || {
        anna.title(note).as_deref() == Some("Groceries")
    })
    .await;
    ben.wait_until("the title", || {
        ben.title(note).as_deref() == Some("Groceries")
    })
    .await;

    let anna_member = anna.core.lock().unwrap().member().unwrap().id().encode();
    {
        let mut core = owner.core.lock().unwrap();
        let people = core.people(note).unwrap();
        let row = people
            .people
            .iter()
            .find(|p| p.member == anna_member)
            .unwrap();
        let (member, invite) = (row.member.clone(), row.invite.clone());
        core.remove_person(note, &member, &invite).unwrap();
    }
    owner.settle(note);
    owner
        .wait_until("the removal made", || {
            !owner.core.lock().unwrap().holds_back(note).unwrap()
        })
        .await;
    owner.set_title(note, "Groceries, just us");

    anna.wait_until("the note gone", || {
        !anna
            .core
            .lock()
            .unwrap()
            .syncable()
            .unwrap()
            .contains(&note)
    })
    .await;
    assert_eq!(
        anna.core.lock().unwrap().take_removals(),
        vec![(note, "Groceries".to_string())]
    );
    ben.wait_until("the new title", || {
        ben.title(note).as_deref() == Some("Groceries, just us")
    })
    .await;
}

/// Scenario 9: a search of relay traffic and storage finds no member ID, box
/// key or name, and a package's size does not depend on the number of people.
#[tokio::test]
async fn the_relay_learns_no_person_from_sharing() {
    let (relay, log) = relay().await;
    let tap = Tap::in_front_of(relay.addr).await;
    let endpoint = RelayEndpoint::parse(&tap.http_base()).unwrap();
    let owner = Phone::new("Olga Oakenshield", &endpoint);
    let anna = Phone::new("Annabelle Quist", &endpoint);
    let ben = Phone::new("Benedikt Ruhl", &endpoint);
    let note = owner.create_note("Groceries");
    let_in(&owner, &anna, note).await;
    let_in(&owner, &ben, note).await;
    {
        let mut core = owner.core.lock().unwrap();
        core.stop_sharing(note).unwrap();
    }
    owner.settle(note);
    anna.wait_until("the note gone", || {
        !anna
            .core
            .lock()
            .unwrap()
            .syncable()
            .unwrap()
            .contains(&note)
    })
    .await;

    let mut forbidden: Vec<(String, Vec<u8>)> = Vec::new();
    for (phone, name) in [
        (&owner, "Olga Oakenshield"),
        (&anna, "Annabelle Quist"),
        (&ben, "Benedikt Ruhl"),
    ] {
        let member = phone.core.lock().unwrap().member().unwrap();
        forbidden.push((format!("{name}'s name"), name.as_bytes().to_vec()));
        forbidden.push((format!("{name}'s member id"), member.id().0.to_vec()));
        forbidden.push((
            format!("{name}'s member id, encoded"),
            member.id().encode().into_bytes(),
        ));
        forbidden.push((format!("{name}'s box key"), member.box_public().to_vec()));
    }

    let stored: Vec<Vec<u8>> = log
        .all_envelopes()
        .into_iter()
        .chain(log.all_invite_bytes())
        .collect();
    let wire = [tap.bytes_to_server(), tap.bytes_to_clients()];
    assert!(!stored.is_empty() && !wire[0].is_empty() && !wire[1].is_empty());
    for (what, bytes) in &forbidden {
        for row in &stored {
            assert!(!contains(row, bytes), "the relay stored {what}");
        }
        for direction in &wire {
            assert!(!contains(direction, bytes), "{what} crossed the wire");
        }
    }

    // Two people and three: the same size. The removal carries a snapshot,
    // whose size varies with the note and not with who is on it.
    let packages: Vec<(usize, bool)> = log
        .all_envelopes()
        .into_iter()
        .filter_map(|raw| {
            let env = Envelope::decode(&raw).ok()?;
            (env.kind == EnvelopeKind::Access).then_some((raw.len(), env.counter < 3))
        })
        .collect();
    let adds: Vec<usize> = packages
        .iter()
        .filter(|(_, add)| *add)
        .map(|(len, _)| *len)
        .collect();
    assert_eq!(adds.len(), 2, "{packages:?}");
    assert_eq!(
        adds[0], adds[1],
        "a package's size says how many people share"
    );
}

/// ADR 0024 on a shared note. A guest compacts, because any member may. The
/// person removed still holds the old key, and the relay no longer takes a
/// compaction signed with it. And somebody let in after all of that gets the
/// note, because a compaction never drops the owner's packages, which are
/// how a newcomer learns the key.
#[tokio::test]
async fn a_member_compacts_a_shared_note_and_the_removed_one_cannot() {
    let log = RelayLog::new();
    let relay = Relay::serve_with(
        Arc::new(log.clone()),
        "127.0.0.1:0".parse().unwrap(),
        RelayConfig {
            storage: Limits {
                crowded_envelopes: 8,
                ..Limits::default()
            },
            ..RelayConfig::default()
        },
    )
    .await
    .unwrap();
    let endpoint = RelayEndpoint::parse(&relay.http_base()).unwrap();
    let owner = Phone::new("Olga", &endpoint);
    let anna = Phone::new("Anna", &endpoint);
    let ben = Phone::new("Ben", &endpoint);
    let note = owner.create_note("Groceries");
    let_in(&owner, &anna, note).await;
    let_in(&owner, &ben, note).await;
    ben.wait_until("the title", || {
        ben.title(note).as_deref() == Some("Groceries")
    })
    .await;

    // What Anna could keep from before she was removed.
    let blob = owner.core.lock().unwrap().export_share_blob(note).unwrap();
    let (_, old_key) = easynotes_core::crypto::import_share_blob(&blob).unwrap();

    let anna_member = anna.core.lock().unwrap().member().unwrap().id().encode();
    {
        let mut core = owner.core.lock().unwrap();
        let people = core.people(note).unwrap();
        let row = people
            .people
            .iter()
            .find(|p| p.member == anna_member)
            .unwrap();
        let (member, invite) = (row.member.clone(), row.invite.clone());
        core.remove_person(note, &member, &invite).unwrap();
    }
    owner.settle(note);
    owner
        .wait_until("the removal made", || {
            !owner.core.lock().unwrap().holds_back(note).unwrap()
        })
        .await;
    ben.wait_until("the new key", || ben.title(note).is_some())
        .await;

    // Ben writes until the relay asks somebody to compact, and somebody does.
    let topic = note.encode();
    let deadline = tokio::time::Instant::now() + PATIENCE;
    let mut last = String::new();
    let mut i = 0;
    while log.topic(&topic).await.unwrap().covers == 0 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "nobody compacted: {:?}",
            log.topic(&topic).await.unwrap()
        );
        i += 1;
        last = format!("Groceries {i}");
        ben.set_title(note, &last);
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    let state = log.topic(&topic).await.unwrap();
    let blob = ben.core.lock().unwrap().export_share_blob(note).unwrap();
    let (_, bens_key) = easynotes_core::crypto::import_share_blob(&blob).unwrap();
    assert_ne!(bens_key.0, old_key.0, "removing Anna changed the key");
    assert_eq!(
        state.authority(),
        Some(easynotes_core::compaction::public_key(note, &bens_key).0),
        "the current package names the key Ben holds"
    );

    // Anna, or anyone with her old key, is refused.
    let stranger = easynotes_core::crypto::DeviceIdentity::generate();
    let token = easynotes_core::sync::authenticate(&endpoint.http_base, &stranger)
        .await
        .unwrap();
    let mut sync = DeviceSync::connect(&endpoint.ws_base, &token)
        .await
        .unwrap();
    sync.subscribe(note, 0).await.unwrap();
    let head = log.head(&topic).await.unwrap();
    let base = easynotes_core::crypto::Envelope::seal(&stranger, note, &old_key, 1, b"nothing")
        .unwrap()
        .encode();
    let request = easynotes_core::compaction::Request::sign(note, &old_key, head, base);
    sync.compact(note, &request).await.unwrap();
    let refused = tokio::time::timeout(PATIENCE, async {
        loop {
            match sync.next().await.unwrap() {
                Some(Routed::Note(_, Incoming::CompactRefused(reason))) => return reason,
                Some(_) => {}
                None => panic!("the relay hung up"),
            }
        }
    })
    .await
    .expect("no answer to the compaction");
    assert_eq!(refused, "not this note's compaction key");
    assert_eq!(log.topic(&topic).await.unwrap().covers, state.covers);

    // Somebody new, after the compaction.
    let carl = Phone::new("Carl", &endpoint);
    let_in(&owner, &carl, note).await;
    carl.wait_until("Ben's last title", || {
        carl.title(note).as_deref() == Some(last.as_str())
    })
    .await;
}
