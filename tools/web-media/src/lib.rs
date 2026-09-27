//! The browser's attachment store and transfer queue, run where they will run:
//! in a tab, over IndexedDB, against a relay reached with `fetch`.
//!
//! `core/tests/attachments_web.rs` holds the store's rules to the directory's
//! promises with its rows in memory, and `relay/tests/transfers.rs` runs the
//! queue over that store against a real relay. What neither can reach is the
//! browser: IndexedDB's transactions, a `fetch` with a bearer token and a
//! `Range` header crossing origins, the page's clock, and a queue driven by the
//! page's event loop instead of a tokio runtime. This is those, and nothing
//! else — no Flutter, as in `tools/web-bench`.
//!
//! [`media.mjs`](../media.mjs) calls each export from Playwright and reads
//! IndexedDB itself afterwards, to look for plaintext.
#![cfg(target_arch = "wasm32")]

use easynotes_core::attachment::idb::IdbRows;
use easynotes_core::attachment::storage::{ImportHints, ThumbnailInput};
use easynotes_core::attachment::web::{MediaRows, WebMediaStore};
use easynotes_core::attachment::{AttachmentState, Media, Variant};
use easynotes_core::crypto::{AuthorKey, DeviceIdentity, DeviceSigner, MasterKey};
use easynotes_core::ids::{AttachmentId, NoteId};
use easynotes_core::sync::transfers::{Reach, TransferPeer, TransferRunner};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use wasm_bindgen::prelude::*;

fn fail(what: impl std::fmt::Display) -> JsValue {
    JsValue::from_str(&what.to_string())
}

fn check(ok: bool, what: &str) -> Result<(), JsValue> {
    if ok {
        Ok(())
    } else {
        Err(fail(what))
    }
}

fn png(w: u32, h: u32, len: usize) -> Vec<u8> {
    let mut bytes = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
    bytes.extend_from_slice(&w.to_be_bytes());
    bytes.extend_from_slice(&h.to_be_bytes());
    bytes.extend((0..len - bytes.len()).map(|i| (i * 13 % 251) as u8));
    bytes
}

/// Three chunks and a bit: enough that chunk order and the short last chunk
/// are both exercised.
fn photo() -> Vec<u8> {
    png(1600, 1200, (2 << 20) + 321)
}

fn thumb() -> Vec<u8> {
    png(320, 240, 4000)
}

/// Sixty-four bytes of the photo that no sealed row may contain.
#[wasm_bindgen]
pub fn plaintext_sample() -> Vec<u8> {
    photo()[1000..1064].to_vec()
}

async fn sleep(ms: i32) {
    let promise = js_sys::Promise::new(&mut |resolve, _| {
        let global = js_sys::global();
        let set = js_sys::Reflect::get(&global, &"setTimeout".into())
            .ok()
            .and_then(|f| f.dyn_into::<js_sys::Function>().ok());
        match set {
            Some(set) => {
                let _ = set.call2(&global, &resolve, &ms.into());
            }
            None => {
                let _ = resolve.call0(&JsValue::NULL);
            }
        }
    });
    let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
}

/// Ask `done` every 20 ms for up to fifteen seconds.
async fn eventually<F: std::future::Future<Output = bool>>(
    what: &str,
    mut done: impl FnMut() -> F,
) -> Result<(), JsValue> {
    for _ in 0..750 {
        if done().await {
            return Ok(());
        }
        sleep(20).await;
    }
    Err(fail(format!("timed out waiting for {what}")))
}

async fn fresh(name: &str, master: &MasterKey) -> Result<WebMediaStore<IdbRows>, JsValue> {
    IdbRows::delete(name).await.map_err(fail)?;
    WebMediaStore::open(IdbRows::new(name), master.clone())
        .await
        .map_err(fail)
}

/// The store's promises, against IndexedDB: an import survives a reopen and
/// reads back; a download that never finished is swept; a purge leaves
/// nothing.
#[wasm_bindgen]
pub async fn store_checks(db: String) -> Result<String, JsValue> {
    let master = MasterKey::random();
    let note = NoteId::random();
    let store = fresh(&db, &master).await?;
    let staged = store
        .import(
            note,
            "kitchen.png",
            &photo(),
            Some(ThumbnailInput { bytes: thumb() }),
            ImportHints::default(),
        )
        .await
        .map_err(fail)?;
    drop(store);

    // A new store over the same database, as a reload makes.
    let store = WebMediaStore::open(IdbRows::new(&db), master.clone())
        .await
        .map_err(fail)?;
    check(
        store.staged().await.map_err(fail)? == vec![staged.clone()],
        "the staged import did not survive a reopen",
    )?;
    let thumbnail = &staged.media.thumbnail.as_ref().unwrap().media;
    check(
        store.has(note, &staged.media.original).await && store.has(note, thumbnail).await,
        "the import is not whole after a reopen",
    )?;
    check(
        store
            .read(note, staged.id, &staged.media, Variant::Original)
            .await
            .map_err(fail)?
            == photo(),
        "the original did not read back",
    )?;
    check(
        store
            .read(note, staged.id, &staged.media, Variant::Thumbnail)
            .await
            .map_err(fail)?
            == thumb(),
        "the thumbnail did not read back",
    )?;
    check(
        store
            .read_chunk(note, staged.id, &staged.media, Variant::Original, 2)
            .await
            .map_err(fail)?
            == photo()[2 << 20..],
        "the last chunk did not read back alone",
    )?;

    // Half a download: one chunk of another tab's import, then nothing.
    let other_name = format!("{db}-other");
    let other = fresh(&other_name, &MasterKey::random()).await?;
    let arriving = other
        .import(note, "other.png", &photo(), None, ImportHints::default())
        .await
        .map_err(fail)?;
    let first = other
        .read_sealed_chunk(note, &arriving.media.original, 0)
        .await
        .map_err(fail)?;
    let mut incoming = store
        .incoming(note, &arriving.media.original)
        .await
        .map_err(fail)?;
    incoming.write(&first).await.map_err(fail)?;
    drop(incoming);
    IdbRows::delete(&other_name).await.map_err(fail)?;
    let rows = IdbRows::new(&db);
    let partial = format!(
        "o/{}/{}/",
        note.encode(),
        arriving.media.original.object.encode()
    );
    check(
        rows.keys(&partial).await.map_err(fail)?.len() == 1,
        "the half-downloaded chunk was not written",
    )?;
    check(
        !store.has(note, &arriving.media.original).await,
        "half a download counts as whole",
    )?;
    drop(store);
    let store = WebMediaStore::open(IdbRows::new(&db), master.clone())
        .await
        .map_err(fail)?;
    check(
        rows.keys(&partial).await.map_err(fail)?.is_empty(),
        "reopening left half a download behind",
    )?;
    check(
        store.has(note, &staged.media.original).await,
        "reopening swept a whole object",
    )?;

    let rows_before = rows.keys("").await.map_err(fail)?.len();
    store.purge_note(note).await.map_err(fail)?;
    check(
        rows.keys("").await.map_err(fail)?.is_empty(),
        "a purge left rows behind",
    )?;
    Ok(format!(
        "store: import, reopen, read, sweep and purge held ({rows_before} rows before the purge)"
    ))
}

/// Leave one import in `db`, for the driver to read IndexedDB for plaintext.
#[wasm_bindgen]
pub async fn import_one(db: String) -> Result<(), JsValue> {
    let store = fresh(&db, &MasterKey::random()).await?;
    store
        .import(
            NoteId::random(),
            "secret-name.png",
            &photo(),
            Some(ThumbnailInput { bytes: thumb() }),
            ImportHints::default(),
        )
        .await
        .map_err(fail)?;
    Ok(())
}

/// A device, as the queue needs one: a key to authenticate with, and a note in
/// which a published descriptor is live. The two tabs share their notes, which
/// stands in for the note's own sync — that is not what is under test here.
#[derive(Clone)]
struct Peer {
    identity: Arc<DeviceIdentity>,
    published: Arc<Mutex<HashMap<(NoteId, AttachmentId), Media>>>,
}

impl DeviceSigner for Peer {
    fn device_key(&self) -> easynotes_core::Result<AuthorKey> {
        self.identity.device_key()
    }

    fn sign_challenge(
        &self,
        nonce: &[u8; easynotes_core::crypto::RELAY_AUTH_NONCE_LEN],
    ) -> easynotes_core::Result<[u8; easynotes_core::crypto::SIG_LEN]> {
        self.identity.sign_challenge(nonce)
    }
}

impl TransferPeer for Peer {
    fn attachment_state(
        &self,
        note: NoteId,
        id: AttachmentId,
    ) -> easynotes_core::Result<AttachmentState> {
        Ok(
            if self.published.lock().unwrap().contains_key(&(note, id)) {
                AttachmentState::Live
            } else {
                AttachmentState::Pending
            },
        )
    }

    fn attachment(&self, note: NoteId, id: AttachmentId) -> easynotes_core::Result<Option<Media>> {
        Ok(self.published.lock().unwrap().get(&(note, id)).cloned())
    }

    fn reach(&self, _note: NoteId) -> Reach {
        Reach::Synced
    }

    fn publish(&self, note: NoteId, id: AttachmentId, media: Media) -> easynotes_core::Result<()> {
        self.published.lock().unwrap().insert((note, id), media);
        Ok(())
    }
}

/// Two tabs and a relay: the first imports a photo and its queue uploads and
/// publishes it; the second's queue fetches both variants when asked.
#[wasm_bindgen]
pub async fn round_trip(db_a: String, db_b: String, http_base: String) -> Result<String, JsValue> {
    let store_a = Arc::new(fresh(&db_a, &MasterKey::random()).await?);
    let store_b = Arc::new(fresh(&db_b, &MasterKey::random()).await?);
    let published: Arc<Mutex<HashMap<(NoteId, AttachmentId), Media>>> = Arc::default();
    let peer = |published: &Arc<Mutex<HashMap<(NoteId, AttachmentId), Media>>>| Peer {
        identity: Arc::new(DeviceIdentity::generate()),
        published: Arc::clone(published),
    };
    let (runner_a, queue_a) =
        TransferRunner::new(peer(&published), store_a.clone(), Some(http_base.clone()));
    let (runner_b, queue_b) =
        TransferRunner::new(peer(&published), store_b.clone(), Some(http_base.clone()));
    wasm_bindgen_futures::spawn_local(runner_a.run());
    wasm_bindgen_futures::spawn_local(runner_b.run());

    let note = NoteId::random();
    let staged = store_a
        .import(
            note,
            "tab.png",
            &photo(),
            Some(ThumbnailInput { bytes: thumb() }),
            ImportHints::default(),
        )
        .await
        .map_err(fail)?;
    queue_a.kick();
    let (id, p, q, s) = (staged.id, &published, &queue_a, &store_a);
    eventually("the upload to publish and settle", move || async move {
        p.lock().unwrap().contains_key(&(note, id))
            && q.transfers().is_empty()
            && s.staged().await.is_ok_and(|s| s.is_empty())
    })
    .await
    .map_err(|e| {
        let states = format!("{:?}", queue_a.transfers());
        fail(format!(
            "{}; transfers: {states}",
            e.as_string().unwrap_or_default()
        ))
    })?;

    let thumbnail = staged.media.thumbnail.as_ref().unwrap().media.clone();
    queue_b.fetch(note, staged.id, Variant::Thumbnail);
    queue_b.fetch(note, staged.id, Variant::Original);
    let (s, t, o) = (&store_b, &thumbnail, &staged.media.original);
    eventually("both variants on the second tab", move || async move {
        s.has(note, t).await && s.has(note, o).await
    })
    .await
    .map_err(|e| {
        let states = format!("{:?}", queue_b.transfers());
        fail(format!(
            "{}; transfers: {states}",
            e.as_string().unwrap_or_default()
        ))
    })?;
    check(
        store_b
            .read(note, staged.id, &staged.media, Variant::Original)
            .await
            .map_err(fail)?
            == photo(),
        "the original did not arrive intact",
    )?;
    check(
        store_b
            .read(note, staged.id, &staged.media, Variant::Thumbnail)
            .await
            .map_err(fail)?
            == thumb(),
        "the thumbnail did not arrive intact",
    )?;
    Ok(format!(
        "relay: {} chunks up and down through two tabs' queues",
        staged.media.original.chunks() + thumbnail.chunks()
    ))
}
