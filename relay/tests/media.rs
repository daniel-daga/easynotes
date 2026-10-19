//! Attachment objects on the relay ([ADR 0027](../../docs/adr/0027-attachments.md) §4):
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
use easynotes_relay::media::MediaStatus;
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
        let (note, id, key) = (
            NoteId::random(),
            AttachmentId::random(),
            ContentKey::random(),
        );
        let plain: Vec<u8> = (0..len).map(|i| (i * 31 % 251) as u8).collect();
        let (media, sealed) =
            seal_whole(&key, note, id, Variant::Original, &plain, 1 << 20).unwrap();
        Self {
            base,
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

    /// What a reader does: fetch, check against the descriptor, open.
    async fn read_back(&self) -> Vec<u8> {
        let response = self.fetch(None).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()["cache-control"],
            "private, max-age=31536000, immutable"
        );
        let bytes = response.bytes().await.unwrap();
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
async fn an_object_goes_up_in_chunks_and_comes_back_whole_or_by_range() {
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
