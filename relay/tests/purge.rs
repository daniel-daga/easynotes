//! Purging a topic, held to one set of rules in both stores and on the socket
//! ([ADR 0026](../../docs/decisions.md#adr-0026)).
//!
//! The memory store always; Postgres when `DATABASE_URL` is set. The same
//! purge through real devices, with a guest who is told, is in `sharing.rs`.

mod support;

use easynotes_core::access::{self, Body, Member, OWNER_INVITE};
use easynotes_core::account::RecoveryPhrase;
use easynotes_core::crypto::{DeviceIdentity, Envelope, NoteKey};
use easynotes_core::ids::NoteId;
use easynotes_core::purge::Request;
use easynotes_core::sync::Incoming;
use easynotes_relay::quota::{Proposal, Refusal};
use easynotes_relay::{
    Appended, Limits, MediaLimits, MemoryStore, PostgresStore, Relay, RelayStore,
};
use std::sync::Arc;
use support::NoteSocket;

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
        Envelope::seal(&self.device, self.id, &self.key, self.counter, b"text")
            .unwrap()
            .encode()
    }

    /// Signed by the note's compaction key, as a never-shared note purges.
    fn by_compaction_key(&self) -> Request {
        Request::sign(
            self.id,
            &easynotes_core::compaction::identity(self.id, &self.key),
        )
    }

    fn package(&self, owner: &DeviceIdentity) -> Vec<u8> {
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
        access::build(owner, self.id, 1, None, &self.key, &body).unwrap()
    }
}

async fn purge(store: &Arc<dyn RelayStore>, topic: &str, request: &Request) -> Result<(), Refusal> {
    store
        .purge(topic, request.key.0, request.signature)
        .await
        .unwrap()
}

async fn is_gone(store: &Arc<dyn RelayStore>, topic: &str) -> bool {
    let gone = store.topic(topic).await.unwrap().gone;
    gone && store.since(topic, 0).await.unwrap().is_empty()
}

#[tokio::test]
async fn a_never_shared_note_is_purged_and_stays_gone() {
    for (name, store) in stores().await {
        // The relay-wide ceiling is checked against the memory store only:
        // on Postgres every test shares the table, so its usage moves under us.
        let memory = name == "memory";
        let mut note = Note::new();
        let topic = note.topic();
        let mut bytes = 0;
        for _ in 0..3 {
            let update = note.update();
            bytes += update.len() as u64;
            store.append(&topic, &update).await.unwrap();
        }
        // A purge must hand the note's bytes back: the ceiling is measured
        // against what the relay holds, and a deleted note is not held.
        let before = store.usage().await.unwrap().log_bytes;
        let limits = Limits {
            ceiling: memory.then_some(before),
            ..Limits::default()
        };
        let mut other = Note::new();
        if memory {
            assert_eq!(
                store
                    .append_within(&other.topic(), &other.update(), &limits)
                    .await
                    .unwrap(),
                Appended::RelayFull,
                "{name}: a note was taken at the ceiling"
            );
        }
        purge(&store, &topic, &note.by_compaction_key())
            .await
            .unwrap_or_else(|r| panic!("{name}: refused: {r:?}"));
        assert!(is_gone(&store, &topic).await, "{name}");
        if memory {
            let after = store.usage().await.unwrap().log_bytes;
            assert_eq!(after, before - bytes, "{name}: a purge kept its bytes");
            assert!(
                matches!(
                    store
                        .append_within(&other.topic(), &other.update(), &limits)
                        .await
                        .unwrap(),
                    Appended::Stored { .. }
                ),
                "{name}: the ceiling was not released"
            );
        }

        // An offline device's outbox cannot bring it back.
        let appended = store
            .append_within(&topic, &note.update(), &Limits::default())
            .await
            .unwrap();
        assert_eq!(appended, Appended::Gone, "{name}");
        let update = note.update();
        let base = easynotes_core::compaction::Request::sign(note.id, &note.key, 1, update, None);
        let proposal = Proposal {
            covers: 1,
            base: &base.base,
            media: None,
            key: base.key.0,
            signature: base.signature,
        };
        assert_eq!(
            store
                .compact(&topic, &proposal, &MediaLimits::default(), 0)
                .await
                .unwrap(),
            Err(Refusal::Gone),
            "{name}"
        );

        // Asking again succeeds, whoever asks: an interrupted deletion can be
        // run again, and there is nothing left to protect.
        let stranger = Request::sign(note.id, &DeviceIdentity::generate());
        assert_eq!(purge(&store, &topic, &stranger).await, Ok(()), "{name}");
    }
}

#[tokio::test]
async fn a_topic_the_relay_never_saw_is_left_gone() {
    for (name, store) in stores().await {
        let mut note = Note::new();
        let topic = note.topic();
        purge(&store, &topic, &note.by_compaction_key())
            .await
            .unwrap();
        let appended = store
            .append_within(&topic, &note.update(), &Limits::default())
            .await
            .unwrap();
        assert_eq!(appended, Appended::Gone, "{name}");
    }
}

#[tokio::test]
async fn a_registered_compaction_key_is_the_only_one_that_purges() {
    for (name, store) in stores().await {
        let mut note = Note::new();
        let topic = note.topic();
        store.append(&topic, &note.update()).await.unwrap();
        let update = note.update();
        let base = easynotes_core::compaction::Request::sign(note.id, &note.key, 1, update, None);
        let proposal = Proposal {
            covers: 1,
            base: &base.base,
            media: None,
            key: base.key.0,
            signature: base.signature,
        };
        store
            .compact(&topic, &proposal, &MediaLimits::default(), 0)
            .await
            .unwrap()
            .unwrap();

        let stranger = Request::sign(note.id, &DeviceIdentity::generate());
        assert_eq!(
            purge(&store, &topic, &stranger).await,
            Err(Refusal::Unauthorised),
            "{name}"
        );
        assert!(!is_gone(&store, &topic).await, "{name}");
        purge(&store, &topic, &note.by_compaction_key())
            .await
            .unwrap_or_else(|r| panic!("{name}: refused: {r:?}"));
        assert!(is_gone(&store, &topic).await, "{name}");
    }
}

#[tokio::test]
async fn a_shared_note_is_purged_by_its_owner_and_not_by_a_guest() {
    for (name, store) in stores().await {
        let mut note = Note::new();
        let topic = note.topic();
        let owner = DeviceIdentity::generate();
        store.append(&topic, &note.package(&owner)).await.unwrap();
        store.append(&topic, &note.update()).await.unwrap();

        // A guest holds the note key, and so its compaction key.
        assert_eq!(
            purge(&store, &topic, &note.by_compaction_key()).await,
            Err(Refusal::Unauthorised),
            "{name}"
        );
        let mut forged = Request::sign(note.id, &owner);
        forged.signature[0] ^= 1;
        assert_eq!(
            purge(&store, &topic, &forged).await,
            Err(Refusal::BadSignature),
            "{name}"
        );
        let elsewhere = Request::sign(NoteId::random(), &owner);
        assert_eq!(
            purge(&store, &topic, &elsewhere).await,
            Err(Refusal::BadSignature),
            "{name}"
        );
        assert!(!is_gone(&store, &topic).await, "{name}");

        purge(&store, &topic, &Request::sign(note.id, &owner))
            .await
            .unwrap_or_else(|r| panic!("{name}: refused: {r:?}"));
        assert!(is_gone(&store, &topic).await, "{name}");
    }
}

#[tokio::test]
async fn subscribers_are_told_and_later_ones_refused() {
    let store = MemoryStore::new();
    let relay = Relay::serve(Arc::new(store.clone()), "127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let mut note = Note::new();
    let device = DeviceIdentity::generate();
    let token = easynotes_core::sync::authenticate(&relay.http_base(), None, &device)
        .await
        .unwrap();
    let mut socket = NoteSocket::connect(&relay.ws_base(), &token, note.id, 0)
        .await
        .unwrap();
    socket.append(&note.update()).await.unwrap();
    loop {
        match socket.next().await.unwrap().unwrap() {
            Incoming::Envelope { .. } => break,
            _ => continue,
        }
    }

    let refused = easynotes_core::sync::purge::purge(
        &relay.http_base(),
        &[(note.id, note.by_compaction_key())],
    )
    .await
    .unwrap();
    assert!(refused.is_empty(), "{refused:?}");

    let told = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            match socket.next().await.unwrap() {
                Some(Incoming::Gone) => return true,
                Some(_) => continue,
                None => return false,
            }
        }
    })
    .await
    .expect("the subscriber was never told");
    assert!(told);

    // A later socket is refused at subscribe, and an append goes nowhere.
    let mut later = NoteSocket::connect(&relay.ws_base(), &token, note.id, 0)
        .await
        .unwrap();
    assert!(matches!(later.next().await.unwrap(), Some(Incoming::Gone)));
    assert!(store.all_envelopes().is_empty());
}

#[tokio::test]
async fn a_refusal_names_the_topic_and_the_rest_still_go() {
    let store = MemoryStore::new();
    let relay = Relay::serve(Arc::new(store.clone()), "127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let shared = Note::new();
    let owner = DeviceIdentity::generate();
    let log: Arc<dyn RelayStore> = Arc::new(store.clone());
    log.append(&shared.topic(), &shared.package(&owner))
        .await
        .unwrap();
    let mut plain = Note::new();
    log.append(&plain.topic(), &plain.update()).await.unwrap();

    let refused = easynotes_core::sync::purge::purge(
        &relay.http_base(),
        &[
            (shared.id, shared.by_compaction_key()),
            (plain.id, plain.by_compaction_key()),
        ],
    )
    .await
    .unwrap();
    assert_eq!(refused.len(), 1);
    assert_eq!(refused[0].0, shared.id);
    assert!(is_gone(&log, &plain.topic()).await);
    assert!(!is_gone(&log, &shared.topic()).await);
}

/// A topic the relay has never stored has nobody's authority to check, so a
/// purge of one writes a tombstone for whoever asks. Without a device token it
/// costs the address's write budget; with one it is free, and a topic already
/// gone is free either way.
#[tokio::test]
async fn an_unseen_topic_costs_a_purge_without_a_token() {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;
    use easynotes_relay::limit::RateLimit;
    use easynotes_relay::RelayConfig;
    // Room for two tombstones at 64 KiB, and next to no refill.
    let relay = Relay::serve_with(
        Arc::new(MemoryStore::new()),
        "127.0.0.1:0".parse().unwrap(),
        RelayConfig {
            writes: RateLimit::new(1, 128 << 10),
            ..RelayConfig::default()
        },
    )
    .await
    .unwrap();
    let http = reqwest::Client::new();
    let url = format!("{}/v1/purge?p=2", relay.http_base());
    let body = |note: &Note| {
        let request = note.by_compaction_key();
        serde_json::json!({ "topics": [{
            "note": note.topic(),
            "key": URL_SAFE_NO_PAD.encode(request.key.0),
            "signature": URL_SAFE_NO_PAD.encode(request.signature),
        }]})
    };
    let purge = |note: &Note, token: Option<&str>| {
        let mut call = http.post(&url).json(&body(note));
        if let Some(token) = token {
            call = call.bearer_auth(token);
        }
        call.send()
    };

    let (first, second, third) = (Note::new(), Note::new(), Note::new());
    assert_eq!(purge(&first, None).await.unwrap().status(), 200);
    assert_eq!(purge(&second, None).await.unwrap().status(), 200);
    let refused = purge(&third, None).await.unwrap();
    assert_eq!(
        refused.status(),
        429,
        "a third unseen topic without a token"
    );
    assert!(refused.headers().contains_key("retry-after"));

    // Purging what is already gone writes nothing, and costs nothing.
    assert_eq!(purge(&first, None).await.unwrap().status(), 200);

    // A device token is what a client purging its own account holds.
    let token =
        easynotes_core::sync::authenticate(&relay.http_base(), None, &DeviceIdentity::generate())
            .await
            .unwrap();
    assert_eq!(purge(&third, Some(&token)).await.unwrap().status(), 200);
    assert!(is_gone(&relay.state.store, &third.topic()).await);
    // And a bad one is no token at all.
    assert_eq!(
        purge(&Note::new(), Some("not-a-token"))
            .await
            .unwrap()
            .status(),
        429
    );
}
