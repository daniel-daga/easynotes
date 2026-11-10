//! Attachment objects on the relay ([ADR 0027](../../docs/decisions.md#adr-0027)):
//! reserved, uploaded a chunk at a time, committed against a digest, fetched
//! whole or by range, resumed after an interruption, held to budgets, expired
//! when abandoned and purged with their topic.
//!
//! The memory store always; Postgres when `DATABASE_URL` is set. Bytes in
//! memory, and in a directory for the tests that care where they land.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use easynotes_core::attachment::seal::{seal_whole, verify_sealed};
use easynotes_core::attachment::{ContentKey, MediaOpener, MediaRef, Variant};
use easynotes_core::crypto::DeviceIdentity;
use easynotes_core::ids::{AttachmentId, MediaObjectId, NoteId};
use easynotes_core::relay_access::{RelayAccess, RelayToken};
use easynotes_relay::error::{RelayError, Result as RelayResult};
use easynotes_relay::media::{Assembled, MediaStatus, PartPut};
use easynotes_relay::{
    Blobs, DirBlobs, MediaConfig, MediaLimits, MemoryBlobs, MemoryStore, PostgresStore, Relay,
    RelayConfig, RelayStore,
};
use reqwest::StatusCode;
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

async fn relay(store: Arc<dyn RelayStore>, media: MediaConfig) -> Relay {
    Relay::serve_with(
        store,
        "127.0.0.1:0".parse().unwrap(),
        RelayConfig {
            media,
            ..RelayConfig::default()
        },
    )
    .await
    .unwrap()
}

/// One sealed file, as the core makes it, and a device to send it with.
struct Upload {
    base: String,
    http: reqwest::Client,
    token: String,
    note: NoteId,
    id: AttachmentId,
    key: ContentKey,
    plain: Vec<u8>,
    media: MediaRef,
    sealed: Vec<u8>,
}

impl Upload {
    async fn new(relay: &Relay, len: usize) -> Self {
        let base = relay.http_base();
        let token = easynotes_core::sync::authenticate(&base, None, &DeviceIdentity::generate())
            .await
            .unwrap();
        Self::with_token(relay, len, token)
    }

    /// An upload made by a device holding `token`, for a relay that takes
    /// per-device access tokens rather than an open handshake.
    fn with_token(relay: &Relay, len: usize, token: String) -> Self {
        let (note, id, key) = (
            NoteId::random(),
            AttachmentId::random(),
            ContentKey::random(),
        );
        let plain: Vec<u8> = (0..len).map(|i| (i * 31 % 251) as u8).collect();
        let (media, sealed) =
            seal_whole(&key, note, id, Variant::Original, &plain, 1 << 20).unwrap();
        Self {
            base: relay.http_base(),
            http: reqwest::Client::new(),
            token,
            note,
            id,
            key,
            plain,
            media,
            sealed,
        }
    }

    /// Another file in the same note.
    fn sibling(&self, len: usize) -> Self {
        let id = AttachmentId::random();
        let plain = vec![7u8; len];
        let (media, sealed) =
            seal_whole(&self.key, self.note, id, Variant::Original, &plain, 1 << 20).unwrap();
        Self {
            base: self.base.clone(),
            http: self.http.clone(),
            token: self.token.clone(),
            note: self.note,
            id,
            key: self.key.clone(),
            plain,
            media,
            sealed,
        }
    }

    fn url(&self, rest: &str) -> String {
        format!(
            "{}/v1/media/{}/{}{rest}?p=2",
            self.base,
            self.note.encode(),
            self.media.object.encode()
        )
    }

    fn chunk(&self, index: u32) -> Vec<u8> {
        let full = self.media.chunk_size as usize + 40;
        let at = index as usize * full;
        self.sealed[at..(at + full).min(self.sealed.len())].to_vec()
    }

    async fn reserve(&self) -> StatusCode {
        self.http
            .post(self.url(""))
            .bearer_auth(&self.token)
            .json(&serde_json::json!({
                "sealed_len": self.media.sealed_len(),
                "chunk_size": self.media.chunk_size,
            }))
            .send()
            .await
            .unwrap()
            .status()
    }

    async fn put(&self, index: u32, bytes: Vec<u8>) -> StatusCode {
        self.http
            .put(self.url(&format!("/{index}")))
            .bearer_auth(&self.token)
            .body(bytes)
            .send()
            .await
            .unwrap()
            .status()
    }

    async fn status(&self) -> Result<MediaStatus, StatusCode> {
        let response = self
            .http
            .get(self.url("/status"))
            .bearer_auth(&self.token)
            .send()
            .await
            .unwrap();
        match response.status() {
            StatusCode::OK => Ok(response.json().await.unwrap()),
            other => Err(other),
        }
    }

    async fn commit_with(&self, digest: [u8; 32]) -> StatusCode {
        self.http
            .post(self.url("/commit"))
            .bearer_auth(&self.token)
            .json(&serde_json::json!({ "digest": URL_SAFE_NO_PAD.encode(digest) }))
            .send()
            .await
            .unwrap()
            .status()
    }

    async fn commit(&self) -> StatusCode {
        self.commit_with(self.media.digest).await
    }

    /// Reserve, send every chunk, commit.
    async fn send(&self) {
        assert!(self.reserve().await.is_success());
        for i in 0..self.media.chunks() {
            assert!(self.put(i, self.chunk(i)).await.is_success());
        }
        assert_eq!(self.commit().await, StatusCode::OK);
    }

    async fn fetch(&self, range: Option<&str>) -> reqwest::Response {
        let mut request = self.http.get(self.url("")).bearer_auth(&self.token);
        if let Some(range) = range {
            request = request.header("Range", range);
        }
        request.send().await.unwrap()
    }

    /// What a reader does: fetch chunk by chunk, check against the descriptor,
    /// open.
    async fn read_back(&self) -> Vec<u8> {
        let full = self.media.chunk_size as u64 + 40;
        let mut bytes = Vec::with_capacity(self.media.sealed_len() as usize);
        for index in 0..self.media.chunks() {
            let at = index as u64 * full;
            let end = (at + full).min(self.media.sealed_len()) - 1;
            let response = self.fetch(Some(&format!("bytes={at}-{end}"))).await;
            assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
            assert_eq!(
                response.headers()["cache-control"],
                "private, max-age=31536000, immutable"
            );
            bytes.extend_from_slice(&response.bytes().await.unwrap());
        }
        verify_sealed(&self.media, &bytes).unwrap();
        MediaOpener::new(
            &self.key,
            self.note,
            self.id,
            Variant::Original,
            &self.media,
        )
        .unwrap()
        .open_whole(&bytes)
        .unwrap()
    }
}

#[tokio::test]
async fn an_object_goes_up_in_chunks_and_comes_back_by_range() {
    for (name, store) in stores().await {
        let relay = relay(store, MediaConfig::default()).await;
        // Three chunks, the last one short.
        let up = Upload::new(&relay, (2 << 20) + 1234).await;
        assert_eq!(up.media.chunks(), 3, "{name}");
        assert_eq!(up.reserve().await, StatusCode::CREATED, "{name}");

        // Nothing is served before the commit.
        assert_eq!(
            up.fetch(None).await.status(),
            StatusCode::NOT_FOUND,
            "{name}"
        );

        // Out of order is fine; the index says where a chunk goes.
        for i in [2, 0, 1] {
            assert_eq!(up.put(i, up.chunk(i)).await, StatusCode::CREATED, "{name}");
        }
        assert_eq!(up.commit().await, StatusCode::OK, "{name}");
        assert_eq!(
            up.status().await.unwrap(),
            MediaStatus {
                committed: true,
                received: vec![]
            },
            "{name}"
        );
        assert_eq!(up.read_back().await, up.plain, "{name}");

        // A player seeking to the second chunk asks for just that.
        let full = (1u64 << 20) + 40;
        let response = up
            .fetch(Some(&format!("bytes={}-{}", full, 2 * full - 1)))
            .await;
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT, "{name}");
        assert_eq!(
            response.headers()["content-range"],
            format!("bytes {}-{}/{}", full, 2 * full - 1, up.media.sealed_len()).as_str(),
            "{name}"
        );
        let chunk = response.bytes().await.unwrap();
        let opener =
            MediaOpener::new(&up.key, up.note, up.id, Variant::Original, &up.media).unwrap();
        assert_eq!(
            opener.open_chunk(1, &chunk).unwrap(),
            up.plain[1 << 20..2 << 20],
            "{name}"
        );

        // A suffix range is the last n bytes.
        let sealed = up.media.sealed_len();
        let response = up.fetch(Some("bytes=-4096")).await;
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT, "{name}");
        assert_eq!(
            response.headers()["content-range"],
            format!("bytes {}-{}/{sealed}", sealed - 4096, sealed - 1).as_str(),
            "{name}"
        );
        assert_eq!(
            response.bytes().await.unwrap().as_ref(),
            &up.sealed[sealed as usize - 4096..],
            "{name}"
        );

        // A whole read of more than one chunk is refused: the core asks by
        // range, and the relay will not hold an object in memory for it.
        let response = up.fetch(None).await;
        assert_eq!(
            response.status(),
            StatusCode::RANGE_NOT_SATISFIABLE,
            "{name}"
        );
        assert_eq!(
            response.headers()["content-range"],
            format!("bytes */{sealed}").as_str(),
            "{name}"
        );

        // Nor is an open or oversized range a way round that: each is served
        // one chunk at a time, and says so.
        for asked in ["bytes=0-", "bytes=0-999999999"] {
            let response = up.fetch(Some(asked)).await;
            assert_eq!(
                response.status(),
                StatusCode::PARTIAL_CONTENT,
                "{name} {asked}"
            );
            assert_eq!(
                response.headers()["content-range"],
                format!("bytes 0-{}/{sealed}", full - 1).as_str(),
                "{name} {asked}"
            );
            assert_eq!(
                response.bytes().await.unwrap().as_ref(),
                &up.sealed[..full as usize],
                "{name} {asked}"
            );
        }
        // A range that starts mid-object is capped from where it starts.
        let response = up.fetch(Some(&format!("bytes={}-", full + 7))).await;
        assert_eq!(
            response.headers()["content-range"],
            format!("bytes {}-{}/{sealed}", full + 7, 2 * full + 6).as_str(),
            "{name}"
        );

        let past = format!("bytes={}-", up.media.sealed_len());
        assert_eq!(
            up.fetch(Some(&past)).await.status(),
            StatusCode::RANGE_NOT_SATISFIABLE,
            "{name}"
        );
    }
}

#[tokio::test]
async fn every_call_can_be_repeated_and_nothing_can_be_changed() {
    for (name, store) in stores().await {
        let relay = relay(store, MediaConfig::default()).await;
        let up = Upload::new(&relay, 1 << 20 | 17).await;
        assert_eq!(up.reserve().await, StatusCode::CREATED, "{name}");
        assert_eq!(up.reserve().await, StatusCode::OK, "{name}");

        // The same object ID with other sizes is somebody else's object.
        let other = up.http.post(up.url("")).bearer_auth(&up.token).json(
            &serde_json::json!({ "sealed_len": up.media.sealed_len() + 1, "chunk_size": up.media.chunk_size }),
        );
        assert_eq!(
            other.send().await.unwrap().status(),
            StatusCode::CONFLICT,
            "{name}"
        );

        assert_eq!(up.put(0, up.chunk(0)).await, StatusCode::CREATED, "{name}");
        assert_eq!(up.put(0, up.chunk(0)).await, StatusCode::OK, "{name}");
        let mut flipped = up.chunk(0);
        flipped[100] ^= 1;
        assert_eq!(up.put(0, flipped).await, StatusCode::CONFLICT, "{name}");
        // A chunk of the wrong length, or past the end, is not one this
        // object has.
        assert_eq!(
            up.put(1, vec![0; 10]).await,
            StatusCode::BAD_REQUEST,
            "{name}"
        );
        assert_eq!(
            up.put(2, up.chunk(1)).await,
            StatusCode::BAD_REQUEST,
            "{name}"
        );

        // Committing with a chunk missing says so and changes nothing.
        assert_eq!(up.commit().await, StatusCode::CONFLICT, "{name}");
        assert_eq!(up.put(1, up.chunk(1)).await, StatusCode::CREATED, "{name}");
        assert_eq!(up.commit().await, StatusCode::OK, "{name}");
        // Again, as a device that never heard the first answer would.
        assert_eq!(up.commit().await, StatusCode::OK, "{name}");
        assert_eq!(
            up.commit_with([9; 32]).await,
            StatusCode::CONFLICT,
            "{name}"
        );
        // A chunk retried after the commit is fine if it is the same.
        assert_eq!(up.put(1, up.chunk(1)).await, StatusCode::OK, "{name}");
        let mut flipped = up.chunk(1);
        flipped[0] ^= 1;
        assert_eq!(up.put(1, flipped).await, StatusCode::CONFLICT, "{name}");
        assert_eq!(up.read_back().await, up.plain, "{name}");
    }
}

#[tokio::test]
async fn an_interrupted_upload_resumes_from_its_status() {
    for (name, store) in stores().await {
        let relay = relay(store, MediaConfig::default()).await;
        let up = Upload::new(&relay, (3 << 20) + 5).await;
        up.reserve().await;
        up.put(0, up.chunk(0)).await;
        up.put(2, up.chunk(2)).await;
        // The app is killed. On the next launch it reserves again, as it does
        // not know how far it got, and asks.
        assert_eq!(up.reserve().await, StatusCode::OK, "{name}");
        let status = up.status().await.unwrap();
        assert_eq!(status.received, vec![0, 2], "{name}");
        for i in (0..up.media.chunks()).filter(|i| !status.received.contains(i)) {
            assert_eq!(up.put(i, up.chunk(i)).await, StatusCode::CREATED, "{name}");
        }
        assert_eq!(up.commit().await, StatusCode::OK, "{name}");
        assert_eq!(up.read_back().await, up.plain, "{name}");
    }
}

#[tokio::test]
async fn chunks_that_are_not_the_digest_are_dropped_to_be_sent_again() {
    for (name, store) in stores().await {
        let relay = relay(store, MediaConfig::default()).await;
        let up = Upload::new(&relay, (1 << 20) + 100).await;
        up.reserve().await;
        // A torn write on the uploader's side, which no chunk length catches.
        let mut torn = up.chunk(1);
        torn[50] ^= 0xFF;
        up.put(0, up.chunk(0)).await;
        up.put(1, torn).await;
        assert_eq!(up.commit().await, StatusCode::CONFLICT, "{name}");
        assert_eq!(
            up.status().await.unwrap().received,
            Vec::<u32>::new(),
            "{name}"
        );
        assert_eq!(
            up.fetch(None).await.status(),
            StatusCode::NOT_FOUND,
            "{name}"
        );
        up.send().await;
        assert_eq!(up.read_back().await, up.plain, "{name}");
    }
}

#[tokio::test]
async fn nobody_without_a_token_gets_anywhere() {
    let relay = relay(Arc::new(MemoryStore::new()), MediaConfig::default()).await;
    let up = Upload::new(&relay, 1000).await;
    up.send().await;
    let anonymous = Upload {
        token: "not-a-token".into(),
        media: up.media.clone(),
        ..up.sibling(1)
    };
    assert_eq!(anonymous.reserve().await, StatusCode::UNAUTHORIZED);
    assert_eq!(
        anonymous.put(0, up.chunk(0)).await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(anonymous.commit().await, StatusCode::UNAUTHORIZED);
    assert_eq!(
        anonymous.fetch(None).await.status(),
        StatusCode::UNAUTHORIZED
    );
    let no_header = reqwest::get(up.url("")).await.unwrap();
    assert_eq!(no_header.status(), StatusCode::UNAUTHORIZED);

    // An object nobody reserved is not there, whoever asks.
    let unknown = Upload {
        media: MediaRef {
            object: MediaObjectId::random(),
            ..up.media.clone()
        },
        ..up.sibling(1)
    };
    assert_eq!(unknown.status().await.unwrap_err(), StatusCode::NOT_FOUND);
    assert_eq!(unknown.put(0, up.chunk(0)).await, StatusCode::NOT_FOUND);
    assert_eq!(unknown.commit().await, StatusCode::NOT_FOUND);
}

/// A reservation costs nothing until its chunks arrive, so what is reserved
/// and not yet committed is held apart from the ceiling: a note may have only
/// so much of it, and so may the whole relay. Committing frees the room.
#[tokio::test]
async fn uploads_in_progress_are_held_apart_from_the_ceiling() {
    let limits = |staging_ceiling| MediaLimits {
        staging_note_bytes: 2500,
        staging_ceiling,
        ..MediaLimits::default()
    };
    for (name, store) in stores().await {
        let relay = relay(
            store,
            MediaConfig {
                limits: limits(None),
                ..MediaConfig::default()
            },
        )
        .await;
        let first = Upload::new(&relay, 1000).await;
        let second = first.sibling(1000);
        let third = first.sibling(1000);
        assert_eq!(first.reserve().await, StatusCode::CREATED, "{name}");
        assert_eq!(second.reserve().await, StatusCode::CREATED, "{name}");
        assert_eq!(
            third.reserve().await,
            StatusCode::INSUFFICIENT_STORAGE,
            "{name}: a third upload in progress in one note"
        );
        // A committed object is no longer in progress.
        first.send().await;
        assert_eq!(third.reserve().await, StatusCode::CREATED, "{name}");
        let metrics = easynotes_relay::metrics::render(&relay.state).await;
        assert!(
            metrics.lines().any(|l| l
                == "easynotes_relay_quota_refusals_total{store=\"media\",scope=\"staging\"} 1"),
            "{name}: {metrics}"
        );
    }

    // Relay-wide, on the memory store alone: the Postgres one holds the other
    // tests' uploads, so what it has in progress is not this test's to know.
    let relay = relay(
        Arc::new(MemoryStore::new()),
        MediaConfig {
            limits: limits(Some(2500)),
            ..MediaConfig::default()
        },
    )
    .await;
    let (a, b) = (
        Upload::new(&relay, 1000).await,
        Upload::new(&relay, 1000).await,
    );
    assert_eq!(a.reserve().await, StatusCode::CREATED);
    assert_eq!(b.reserve().await, StatusCode::CREATED);
    let c = Upload::new(&relay, 1000).await;
    assert_eq!(
        c.reserve().await,
        StatusCode::INSUFFICIENT_STORAGE,
        "a third upload in progress, in a note of its own"
    );
    a.send().await;
    assert_eq!(c.reserve().await, StatusCode::CREATED);
}

#[tokio::test]
async fn budgets_count_what_is_reserved_and_what_is_committed() {
    for (name, store) in stores().await {
        let relay = relay(
            store,
            MediaConfig {
                limits: MediaLimits {
                    note_bytes: 2500,
                    ceiling: None,
                    ..MediaLimits::default()
                },
                ..MediaConfig::default()
            },
        )
        .await;
        let first = Upload::new(&relay, 1000).await;
        first.send().await;
        // Reserved and never sent counts as much as committed.
        let second = first.sibling(1000);
        assert_eq!(second.reserve().await, StatusCode::CREATED, "{name}");
        let third = first.sibling(1000);
        assert_eq!(
            third.reserve().await,
            StatusCode::INSUFFICIENT_STORAGE,
            "{name}"
        );
        // Another note has its own budget.
        let elsewhere = Upload::new(&relay, 1000).await;
        assert_eq!(elsewhere.reserve().await, StatusCode::CREATED, "{name}");
        // And the refusal is counted where an operator will see it.
        let metrics = easynotes_relay::metrics::render(&relay.state).await;
        assert!(
            metrics
                .lines()
                .any(|l| l
                    == "easynotes_relay_quota_refusals_total{store=\"media\",scope=\"note\"} 1"),
            "{name}: {metrics}"
        );
    }

    for (name, store) in stores().await {
        let relay = relay(
            store.clone(),
            MediaConfig {
                limits: MediaLimits {
                    ceiling: Some(1),
                    ..MediaLimits::default()
                },
                ..MediaConfig::default()
            },
        )
        .await;
        // A relay already holding anything (the Postgres one holds the
        // earlier tests' objects) takes no more.
        let up = Upload::new(&relay, 1000).await;
        assert_eq!(
            up.reserve().await,
            StatusCode::INSUFFICIENT_STORAGE,
            "{name}"
        );
    }
}

#[tokio::test]
async fn an_abandoned_upload_expires_and_a_committed_object_does_not() {
    for (name, store) in stores().await {
        // Everything uncommitted is already stale, so the next reservation
        // sweeps it.
        let relay = relay(
            store,
            MediaConfig {
                limits: MediaLimits {
                    staging_ttl_secs: -60,
                    ..MediaLimits::default()
                },
                ..MediaConfig::default()
            },
        )
        .await;
        let kept = Upload::new(&relay, 1000).await;
        kept.send().await;
        let abandoned = kept.sibling(1000);
        abandoned.reserve().await;
        abandoned.put(0, abandoned.chunk(0)).await;

        let next = kept.sibling(1000);
        assert_eq!(next.reserve().await, StatusCode::CREATED, "{name}");
        assert_eq!(
            abandoned.status().await.unwrap_err(),
            StatusCode::NOT_FOUND,
            "{name}"
        );
        assert_eq!(kept.read_back().await, kept.plain, "{name}");
        // The uploader that comes back reserves again and starts over.
        assert_eq!(abandoned.reserve().await, StatusCode::CREATED, "{name}");
    }
}

#[tokio::test]
async fn purging_a_topic_takes_its_objects() {
    for (name, store) in stores().await {
        let dir = tempfile::tempdir().unwrap();
        let blobs: Arc<dyn Blobs> = Arc::new(DirBlobs::open(dir.path()).unwrap());
        let relay = relay(
            store,
            MediaConfig {
                blobs: Some(blobs),
                limits: MediaLimits::default(),
            },
        )
        .await;
        let up = Upload::new(&relay, 1000).await;
        up.send().await;
        let half = up.sibling(1 << 20 | 1);
        half.reserve().await;
        half.put(0, half.chunk(0)).await;
        let other_note = Upload::new(&relay, 1000).await;
        other_note.send().await;

        // Nobody registered a key for this topic, so any signer may purge it
        // (ADR 0026); the purge tests hold the rules themselves.
        let request = easynotes_core::purge::Request::sign(up.note, &DeviceIdentity::generate());
        let refused = easynotes_core::sync::purge::purge(&up.base, &[(up.note, request)])
            .await
            .unwrap();
        assert!(refused.is_empty(), "{name}: {refused:?}");

        assert_eq!(
            up.fetch(None).await.status(),
            StatusCode::NOT_FOUND,
            "{name}"
        );
        assert_eq!(
            half.status().await.unwrap_err(),
            StatusCode::NOT_FOUND,
            "{name}"
        );
        assert!(
            !dir.path().join("objects").join(up.note.encode()).exists(),
            "{name}"
        );
        assert!(
            !dir.path().join("parts").join(up.note.encode()).exists(),
            "{name}"
        );
        // And it stays gone: nothing more can be reserved there.
        assert_eq!(up.sibling(10).reserve().await, StatusCode::GONE, "{name}");
        assert_eq!(other_note.read_back().await, other_note.plain, "{name}");
    }
}

#[tokio::test]
async fn a_directory_holds_only_ciphertext_and_survives_a_restart() {
    for (name, store) in stores().await {
        let dir = tempfile::tempdir().unwrap();
        let up = {
            let relay = relay(
                store.clone(),
                MediaConfig {
                    blobs: Some(Arc::new(DirBlobs::open(dir.path()).unwrap())),
                    limits: MediaLimits::default(),
                },
            )
            .await;
            let up = Upload::new(&relay, (1 << 20) + 4096).await;
            up.send().await;
            let half = up.sibling(1 << 20 | 1);
            half.reserve().await;
            half.put(1, half.chunk(1)).await;
            up
        };

        let mut files = Vec::new();
        let mut stack = vec![dir.path().to_path_buf()];
        while let Some(at) = stack.pop() {
            for entry in std::fs::read_dir(at).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    stack.push(path);
                } else {
                    files.push(std::fs::read(path).unwrap());
                }
            }
        }
        assert_eq!(files.len(), 2, "{name}: one object and one chunk");
        let probe = &up.plain[5000..5032];
        for bytes in &files {
            assert!(
                !bytes.windows(probe.len()).any(|w| w == probe),
                "{name}: plaintext on the relay's disk"
            );
        }

        // Another process over the same directory and database.
        let relay = relay(
            store,
            MediaConfig {
                blobs: Some(Arc::new(DirBlobs::open(dir.path()).unwrap())),
                limits: MediaLimits::default(),
            },
        )
        .await;
        let token = easynotes_core::sync::authenticate(
            &relay.http_base(),
            None,
            &DeviceIdentity::generate(),
        )
        .await
        .unwrap();
        let up = Upload {
            base: relay.http_base(),
            token,
            ..up
        };
        assert_eq!(up.read_back().await, up.plain, "{name}");
    }
}

#[tokio::test]
async fn memory_blobs_hold_only_ciphertext() {
    let blobs = Arc::new(MemoryBlobs::default());
    let relay = relay(
        Arc::new(MemoryStore::new()),
        MediaConfig {
            blobs: Some(blobs.clone()),
            limits: MediaLimits::default(),
        },
    )
    .await;
    let up = Upload::new(&relay, 70_000).await;
    up.send().await;
    let probe = &up.plain[1000..1032];
    let held = blobs.all_bytes();
    assert!(!held.is_empty());
    for bytes in held {
        assert!(!bytes.windows(probe.len()).any(|w| w == probe));
    }
}

#[tokio::test]
async fn a_relay_with_nowhere_durable_for_media_says_so() {
    let relay = relay(
        Arc::new(MemoryStore::new()),
        MediaConfig {
            blobs: None,
            limits: MediaLimits::default(),
        },
    )
    .await;
    let up = Upload::new(&relay, 1000).await;
    assert_eq!(up.reserve().await, StatusCode::SERVICE_UNAVAILABLE);
}

/// Compact `upload`'s topic straight in the store, listing `live`: what a
/// member's base would do over the socket, without one.
async fn compact_listing(
    store: &Arc<dyn RelayStore>,
    upload: &Upload,
    live: Vec<MediaObjectId>,
    limits: &MediaLimits,
) {
    use easynotes_core::crypto::{Envelope, NoteKey};
    // The same key every time: the first compaction registers the topic's
    // compaction key, and the relay takes no other after it.
    let (device, key) = (DeviceIdentity::generate(), NoteKey([9; 32]));
    let topic = upload.note.encode();
    let seal = |counter| {
        Envelope::seal(&device, upload.note, &key, counter, b"edit")
            .unwrap()
            .encode()
    };
    let covers = match store
        .append_within(&topic, &seal(1), &easynotes_relay::Limits::default())
        .await
        .unwrap()
    {
        easynotes_relay::Appended::Stored { seq, .. } => seq,
        other => panic!("not stored: {other:?}"),
    };
    let request =
        easynotes_core::compaction::Request::sign(upload.note, &key, covers, seal(2), Some(live));
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    store
        .compact(
            &topic,
            &easynotes_relay::quota::Proposal {
                covers: request.covers,
                base: &request.base,
                media: request.media.as_deref(),
                key: request.key.0,
                signature: request.signature,
            },
            limits,
            now,
        )
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn a_doomed_object_is_kept_if_fetched_and_swept_if_not() {
    for (name, store) in stores().await {
        // Nothing is ever fresh and a doom is due at once, so one compaction
        // dooms and the next reservation sweeps.
        let limits = MediaLimits {
            fresh_secs: -60,
            doomed_ttl_secs: -60,
            ..MediaLimits::default()
        };
        let relay = relay(
            store.clone(),
            MediaConfig {
                limits,
                ..MediaConfig::default()
            },
        )
        .await;
        let kept = Upload::new(&relay, 1000).await;
        kept.send().await;
        let removed = kept.sibling(1000);
        removed.send().await;
        let doomed = |u: &Upload| {
            let (store, topic, object) = (store.clone(), u.note.encode(), u.media.object.encode());
            async move {
                store
                    .media(&topic, &object)
                    .await
                    .unwrap()
                    .map(|r| r.doomed)
            }
        };

        // Doomed, then fetched by a reader that has not seen it removed: kept.
        compact_listing(&store, &kept, vec![kept.media.object], &limits).await;
        assert_eq!(doomed(&removed).await, Some(true), "{name}");
        assert_eq!(removed.read_back().await, removed.plain, "{name}");
        assert_eq!(doomed(&removed).await, Some(false), "{name}");

        // Doomed again, and asked about by the device that uploaded it: kept.
        compact_listing(&store, &kept, vec![kept.media.object], &limits).await;
        assert!(removed.status().await.unwrap().committed, "{name}");
        assert_eq!(doomed(&removed).await, Some(false), "{name}");

        // Doomed again and left alone: the next reservation sweeps it, bytes
        // and all, and the listed one is untouched.
        compact_listing(&store, &kept, vec![kept.media.object], &limits).await;
        let next = kept.sibling(1000);
        assert_eq!(next.reserve().await, StatusCode::CREATED, "{name}");
        assert_eq!(
            removed.fetch(None).await.status(),
            StatusCode::NOT_FOUND,
            "{name}"
        );
        assert_eq!(doomed(&removed).await, None, "{name}");
        assert_eq!(kept.read_back().await, kept.plain, "{name}");
        // A device that still had it, and was not told, can put it back.
        removed.send().await;
        assert_eq!(removed.read_back().await, removed.plain, "{name}");
    }
}

/// A read is not asking for the object: a read-only device polling status
/// must not clear a doom, or a lapsed household could keep unreferenced bytes
/// alive for ever.
#[tokio::test]
async fn a_read_only_status_read_does_not_keep_a_doomed_object() {
    let store: Arc<dyn RelayStore> = Arc::new(MemoryStore::new());
    let relay = Relay::serve_with(
        store.clone(),
        "127.0.0.1:0".parse().unwrap(),
        RelayConfig {
            tokens: true,
            ..RelayConfig::default()
        },
    )
    .await
    .unwrap();
    let minted = |id: [u8; 16], secret: [u8; 32], read_only: bool| {
        let store = store.clone();
        let base = relay.http_base();
        async move {
            let id = URL_SAFE_NO_PAD.encode(id);
            assert!(store.create_access_token(&id, &secret, None).await.unwrap());
            if read_only {
                store.set_access_token_read_only(&id, true).await.unwrap();
            }
            easynotes_core::sync::authenticate(
                &base,
                Some(&RelayToken {
                    id: Some(id),
                    secret: RelayAccess::from_bytes(secret),
                }),
                &DeviceIdentity::generate(),
            )
            .await
            .unwrap()
        }
    };
    let limits = MediaLimits {
        fresh_secs: -60,
        ..MediaLimits::default()
    };

    let uploader = minted([21; 16], [22; 32], false).await;
    let kept = Upload::with_token(&relay, 1000, uploader);
    kept.send().await;
    let removed = kept.sibling(1000);
    removed.send().await;
    let doomed = |upload: &Upload| {
        let store = store.clone();
        let (note, object) = (upload.note.encode(), upload.media.object.encode());
        async move {
            store
                .media(&note, &object)
                .await
                .unwrap()
                .map(|row| row.doomed)
        }
    };

    // A writable device asking for status still makes the object wanted.
    compact_listing(&store, &kept, vec![kept.media.object], &limits).await;
    assert_eq!(doomed(&removed).await, Some(true));
    assert!(removed.status().await.unwrap().committed);
    assert_eq!(doomed(&removed).await, Some(false));

    // Doomed again, and asked about by a token that may only read.
    compact_listing(&store, &kept, vec![kept.media.object], &limits).await;
    assert_eq!(doomed(&removed).await, Some(true));
    let reader = minted([23; 16], [24; 32], true).await;
    let response = reqwest::Client::new()
        .get(removed.url("/status"))
        .bearer_auth(&reader)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let status: MediaStatus = response.json().await.unwrap();
    assert!(status.committed);
    assert_eq!(
        doomed(&removed).await,
        Some(true),
        "a read-only status read extended the doom"
    );
}

/// #34: expiring removes the rows *and* the bytes behind them. A committed
/// object no compaction lists and an abandoned upload both leave the disk.
#[tokio::test]
async fn an_expired_object_leaves_no_bytes_behind() {
    // Memory only: a Postgres store is shared between tests, and these TTLs
    // would sweep another test's media.
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn RelayStore> = Arc::new(MemoryStore::new());
    let limits = MediaLimits {
        staging_ttl_secs: -60,
        fresh_secs: -60,
        doomed_ttl_secs: -60,
        ..MediaLimits::default()
    };
    let relay = relay(
        store.clone(),
        MediaConfig {
            blobs: Some(Arc::new(DirBlobs::open(dir.path()).unwrap())),
            limits,
        },
    )
    .await;

    let doomed = Upload::new(&relay, 1000).await;
    doomed.send().await;
    let abandoned = doomed.sibling(1000);
    abandoned.reserve().await;
    abandoned.put(0, abandoned.chunk(0)).await;

    let object = || {
        dir.path()
            .join("objects")
            .join(doomed.note.encode())
            .join(doomed.media.object.encode())
    };
    let part = || {
        dir.path()
            .join("parts")
            .join(abandoned.note.encode())
            .join(abandoned.media.object.encode())
    };

    // Nothing live lists it, so the compaction dooms it. Both are on disk.
    compact_listing(&store, &doomed, vec![], &limits).await;
    assert!(object().exists(), "the committed object was never written");
    assert!(part().exists(), "the abandoned chunk was never written");

    // A reservation sweeps the abandoned upload and the doomed object.
    assert_eq!(doomed.sibling(1000).reserve().await, StatusCode::CREATED);
    assert!(!object().exists(), "the doomed bytes stayed");
    assert!(!part().exists(), "the abandoned bytes stayed");
}

/// A `Blobs` whose volume will not unlink one object, as a full or misbehaving
/// disk does.
struct FailsOneRemove {
    dir: Arc<DirBlobs>,
    failing: std::sync::Mutex<Option<String>>,
}

#[async_trait::async_trait]
impl Blobs for FailsOneRemove {
    async fn put_part(
        &self,
        note: &str,
        object: &str,
        index: u32,
        bytes: axum::body::Bytes,
    ) -> RelayResult<PartPut> {
        self.dir.put_part(note, object, index, bytes).await
    }

    async fn parts(&self, note: &str, object: &str) -> RelayResult<Vec<u32>> {
        self.dir.parts(note, object).await
    }

    async fn assemble(
        &self,
        note: &str,
        object: &str,
        chunks: u32,
        digest: [u8; 32],
    ) -> RelayResult<Assembled> {
        self.dir.assemble(note, object, chunks, digest).await
    }

    async fn read(
        &self,
        note: &str,
        object: &str,
        at: u64,
        len: u64,
    ) -> RelayResult<Option<Vec<u8>>> {
        self.dir.read(note, object, at, len).await
    }

    async fn remove(&self, note: &str, object: &str) -> RelayResult<()> {
        if self.failing.lock().unwrap().as_deref() == Some(object) {
            return Err(RelayError::Storage("this volume will not unlink".into()));
        }
        self.dir.remove(note, object).await
    }

    async fn remove_note(&self, note: &str) -> RelayResult<()> {
        self.dir.remove_note(note).await
    }
}

/// Issue #17: the sweep drops rows before it removes bytes, so a removal that
/// aborted the batch orphaned every later object with nothing left to name it.
#[tokio::test]
async fn a_sweep_that_cannot_unlink_one_object_still_finishes_the_rest() {
    // Memory only: a Postgres store is shared between tests, and another
    // test's sweep would remove the abandoned object before this one runs.
    let dir = tempfile::tempdir().unwrap();
    let blobs = Arc::new(FailsOneRemove {
        dir: Arc::new(DirBlobs::open(dir.path()).unwrap()),
        failing: std::sync::Mutex::new(None),
    });
    let store: Arc<dyn RelayStore> = Arc::new(MemoryStore::new());
    let limits = MediaLimits {
        staging_ttl_secs: -60,
        fresh_secs: -60,
        doomed_ttl_secs: -60,
        ..MediaLimits::default()
    };
    let relay = relay(
        store.clone(),
        MediaConfig {
            blobs: Some(blobs.clone()),
            limits,
        },
    )
    .await;

    // Committed, and the compaction below lists nothing, so it is doomed.
    let note = Upload::new(&relay, 1000).await;
    let doomed = note.sibling(1000);
    doomed.send().await;
    // Reserved after the last sweep, so it is still here when the next runs.
    let abandoned = note.sibling(1000);
    abandoned.reserve().await;
    abandoned.put(0, abandoned.chunk(0)).await;

    // The volume refuses the abandoned object, which the sweep reaches first.
    *blobs.failing.lock().unwrap() = Some(abandoned.media.object.encode());
    compact_listing(&store, &note, vec![], &limits).await;
    // A reservation runs the sweep: the abandoned object and the doomed one.
    assert_eq!(
        note.sibling(1000).reserve().await,
        StatusCode::CREATED,
        "a failed unlink must not refuse the reservation"
    );

    let held = |u: &Upload| {
        let (note, object) = (u.note.encode(), u.media.object.encode());
        dir.path()
            .join("objects")
            .join(&note)
            .join(&object)
            .exists()
            || dir.path().join("parts").join(&note).join(&object).exists()
    };
    assert!(
        !held(&doomed),
        "the sweep stopped at the first failed unlink"
    );
    assert!(
        held(&abandoned),
        "at most the object that would not unlink may leak"
    );
    for u in [&doomed, &abandoned] {
        assert!(
            store
                .media(&u.note.encode(), &u.media.object.encode())
                .await
                .unwrap()
                .is_none(),
            "a swept object's row is gone"
        );
    }
}

/// Another instance's commit landing between this one's row read and its
/// assemble: the race in #22, made to happen every time.
struct CommitsElsewhere {
    dir: Arc<DirBlobs>,
    store: Arc<dyn RelayStore>,
    landing: std::sync::Mutex<Option<Landing>>,
}

struct Landing {
    note: String,
    object: String,
    chunks: u32,
    digest: [u8; 32],
}

#[async_trait::async_trait]
impl Blobs for CommitsElsewhere {
    async fn put_part(
        &self,
        note: &str,
        object: &str,
        index: u32,
        bytes: axum::body::Bytes,
    ) -> RelayResult<PartPut> {
        self.dir.put_part(note, object, index, bytes).await
    }

    async fn parts(&self, note: &str, object: &str) -> RelayResult<Vec<u32>> {
        self.dir.parts(note, object).await
    }

    async fn assemble(
        &self,
        note: &str,
        object: &str,
        chunks: u32,
        digest: [u8; 32],
    ) -> RelayResult<Assembled> {
        let landing = self.landing.lock().unwrap().take();
        if let Some(other) = landing {
            // The other instance assembles, then records its commit.
            assert_eq!(
                self.dir
                    .assemble(&other.note, &other.object, other.chunks, other.digest)
                    .await
                    .unwrap(),
                Assembled::Done
            );
            self.store
                .commit_media(&other.note, &other.object, other.digest)
                .await
                .unwrap();
        }
        self.dir.assemble(note, object, chunks, digest).await
    }

    async fn read(
        &self,
        note: &str,
        object: &str,
        at: u64,
        len: u64,
    ) -> RelayResult<Option<Vec<u8>>> {
        self.dir.read(note, object, at, len).await
    }

    async fn remove(&self, note: &str, object: &str) -> RelayResult<()> {
        self.dir.remove(note, object).await
    }

    async fn remove_note(&self, note: &str) -> RelayResult<()> {
        self.dir.remove_note(note).await
    }
}

/// Issue #22: a commit on another instance deleted attachment bytes that were
/// already committed, because `assemble` cannot see the metadata that says so.
#[tokio::test]
async fn a_commit_that_loses_the_race_leaves_the_committed_bytes_alone() {
    for (name, store) in stores().await {
        let dir = tempfile::tempdir().unwrap();
        let blobs = Arc::new(DirBlobs::open(dir.path()).unwrap());
        let racer = Arc::new(CommitsElsewhere {
            dir: blobs.clone(),
            store: store.clone(),
            landing: std::sync::Mutex::new(None),
        });
        let relay = relay(
            store.clone(),
            MediaConfig {
                blobs: Some(racer.clone()),
                limits: MediaLimits::default(),
            },
        )
        .await;

        // This instance reserves, sends every chunk, and is about to commit.
        let up = Upload::new(&relay, (1 << 20) + 4000).await;
        assert_eq!(up.reserve().await, StatusCode::CREATED, "{name}");
        for i in 0..up.media.chunks() {
            assert_eq!(up.put(i, up.chunk(i)).await, StatusCode::CREATED, "{name}");
        }
        // Between this commit's row read and its assemble, the other instance
        // commits the same object under the digest of the same chunks.
        *racer.landing.lock().unwrap() = Some(Landing {
            note: up.note.encode(),
            object: up.media.object.encode(),
            chunks: up.media.chunks(),
            digest: up.media.digest,
        });

        // This commit names another digest. It must be refused and touch
        // nothing: those bytes are the object now.
        assert_eq!(
            up.commit_with([7; 32]).await,
            StatusCode::CONFLICT,
            "{name}"
        );
        // The hook did fire, rather than this passing because no commit ever
        // reached `assemble`.
        assert_eq!(
            store
                .media(&up.note.encode(), &up.media.object.encode())
                .await
                .unwrap()
                .unwrap()
                .committed,
            Some(up.media.digest),
            "{name}"
        );
        assert_eq!(
            up.fetch(Some("bytes=0-99")).await.status(),
            StatusCode::PARTIAL_CONTENT,
            "{name}"
        );
        assert_eq!(up.read_back().await, up.plain, "{name}");
        assert!(
            dir.path()
                .join("objects")
                .join(up.note.encode())
                .join(up.media.object.encode())
                .is_file(),
            "{name}"
        );
    }
}

/// The crash-between-assemble-and-commit state: an object on disk under a row
/// that still says uncommitted. A commit naming another digest must not delete
/// it — nothing here can prove that a commit on another instance is not
/// recording it — and the chunks it hashed are dropped so they are sent again.
#[tokio::test]
async fn a_mismatch_against_an_uncommitted_object_is_left_on_disk() {
    for (name, store) in stores().await {
        let dir = tempfile::tempdir().unwrap();
        let blobs = Arc::new(DirBlobs::open(dir.path()).unwrap());
        let relay = relay(
            store,
            MediaConfig {
                blobs: Some(blobs.clone()),
                limits: MediaLimits::default(),
            },
        )
        .await;
        let up = Upload::new(&relay, (1 << 20) + 4000).await;
        assert_eq!(up.reserve().await, StatusCode::CREATED, "{name}");
        for i in 0..up.media.chunks() {
            assert_eq!(up.put(i, up.chunk(i)).await, StatusCode::CREATED, "{name}");
        }
        // Assembled, and a crash before the commit was recorded: the object
        // is on disk under a row that still says uncommitted.
        let (note, object) = (up.note.encode(), up.media.object.encode());
        let on_disk = dir.path().join("objects").join(&note).join(&object);
        assert_eq!(
            blobs
                .assemble(&note, &object, up.media.chunks(), up.media.digest)
                .await
                .unwrap(),
            Assembled::Done,
            "{name}"
        );

        // A digest it is not. `assemble` leaves the object, and the commit
        // deletes nothing either: the row it read says nothing about what
        // another instance is doing this moment.
        assert_eq!(
            blobs
                .assemble(&note, &object, up.media.chunks(), [7; 32])
                .await
                .unwrap(),
            Assembled::Mismatch { assembled: true },
            "{name}"
        );
        assert!(on_disk.is_file(), "{name}: assemble dropped bytes");
        assert_eq!(
            up.commit_with([7; 32]).await,
            StatusCode::CONFLICT,
            "{name}"
        );
        assert!(on_disk.is_file(), "{name}: the commit dropped bytes");
        assert_eq!(
            up.status().await.unwrap().received,
            Vec::<u32>::new(),
            "{name}"
        );
        assert_eq!(
            up.fetch(None).await.status(),
            StatusCode::NOT_FOUND,
            "{name}"
        );
        // Nothing was thrown away: the digest the object does have commits it.
        up.send().await;
        assert_eq!(up.read_back().await, up.plain, "{name}");
    }
}
