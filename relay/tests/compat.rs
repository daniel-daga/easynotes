//! Compatibility fixtures ([ADR 0029](../../docs/adr/0029-compatibility-across-releases.md) §6):
//! data written by each release, read by every build after it.
//!
//! `compat/v<version>/` holds what one release wrote, made by
//! `write_compat_fixtures` below through `just compat-fixtures` before that
//! release is tagged, and never touched again. `every_fixture_set_still_reads`
//! runs on every `cargo test` and opens every one of them with the code as it
//! stands:
//!
//! * the owner's and a guest's SQLite stores, and the owner's attachment
//!   store, each opened from a copy (opening migrates, and a fixture must not
//!   change);
//! * a browser tab's note store and keyring, as the rows IndexedDB held;
//! * the recovery phrase, which recovers the account on a fresh device;
//! * the relay's database dump and media directory, restored into a fresh
//!   database and served by this build's relay, from which that fresh device
//!   gets every note, the shared one included, and the attachment's bytes. That
//!   part needs `DATABASE_URL`, as the other Postgres tests do; CI sets it;
//! * an invite link, parsed.
//!
//! Every sealed format the relay keeps (updates, access packages, a compacted
//! base, the account record, a media object) is in the dump and in the stores,
//! and is opened by reading them the way the app does rather than one by one:
//! a format that stopped opening would fail the read it is part of.
//!
//! What each copy should say is in the set's `expect.json`, taken from what the
//! devices showed when it was written. It is test data, phrase and keys
//! included: nothing in a fixture is anybody's.

mod support;

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use easynotes_core::account::RecoveryPhrase;
use easynotes_core::attachment::store::{ImportHints, MediaStore};
use easynotes_core::attachment::Variant;
use easynotes_core::crdt::{Colour, DefaultNoteDoc, NoteView};
use easynotes_core::crypto::{DeviceIdentity, MasterKey};
use easynotes_core::ids::NoteId;
use easynotes_core::invite::InviteSecret;
use easynotes_core::note::{NoteCore, NoteKind, NoteSummary};
use easynotes_core::store::{
    master_key_from_file, Change, ChangeSink, Keyring, MemoryKeyring, MemoryStore, OpStore,
    SqliteKeyring, SqliteStore, WebKeyring, WebStore,
};
use easynotes_core::sync::invite::{self, Answer};
use easynotes_core::sync::media::{download_into, upload_media};
use easynotes_core::sync::{authenticate, RelayEndpoint, SyncEngine, SyncHandle, SyncState};
use easynotes_relay::{DirBlobs, Limits, MediaConfig, PostgresStore, Relay, RelayConfig};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const PATIENCE: Duration = Duration::from_secs(30);

fn compat_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../compat")
}

// ---------------------------------------------------------------------------
// What a fixture set promises
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Debug, PartialEq)]
struct Expect {
    /// The release that wrote it.
    version: String,
    phrase: String,
    notes: Vec<ExpectNote>,
    /// A link to a note the owner never opened on the relay: only parsed.
    invite_link: String,
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
struct ExpectNote {
    id: String,
    kind: String,
    title: String,
    body: String,
    /// Text and whether it is ticked, in the note's order.
    items: Vec<(String, bool)>,
    /// `Debug` of the owner's colour for it, if any.
    colour: Option<String>,
    pinned: bool,
    /// Shared with the guest, who holds a copy of it.
    shared: bool,
    attachments: Vec<ExpectAttachment>,
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
struct ExpectAttachment {
    name: String,
    len: u64,
    sha256: String,
}

fn describe(view: &NoteView, summary: &NoteSummary, shared: bool) -> ExpectNote {
    ExpectNote {
        id: summary.id.encode(),
        kind: format!("{:?}", view.kind),
        title: view.title.clone(),
        body: view.body.clone(),
        items: view
            .items
            .iter()
            .map(|i| (i.text.clone(), i.checked))
            .collect(),
        colour: summary.placement.colour.map(|c| format!("{c:?}")),
        pinned: summary.placement.pinned,
        shared,
        attachments: Vec::new(),
    }
}

/// The contents of one note as a device shows them, for comparing with what
/// the fixture recorded. Attachments are compared by name here; their bytes
/// are checked wherever a device holds them.
fn content(view: &NoteView) -> (String, String, String, Vec<(String, bool)>, Vec<String>) {
    (
        format!("{:?}", view.kind),
        view.title.clone(),
        view.body.clone(),
        view.items
            .iter()
            .map(|i| (i.text.clone(), i.checked))
            .collect(),
        view.attachments
            .iter()
            .map(|a| a.media.name.clone())
            .collect(),
    )
}

fn expected_content(
    note: &ExpectNote,
) -> (String, String, String, Vec<(String, bool)>, Vec<String>) {
    (
        note.kind.clone(),
        note.title.clone(),
        note.body.clone(),
        note.items.clone(),
        note.attachments.iter().map(|a| a.name.clone()).collect(),
    )
}

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

// ---------------------------------------------------------------------------
// A device, over whatever stores it has
// ---------------------------------------------------------------------------

struct Device<S: OpStore + Send + 'static, K: Keyring + Send + 'static> {
    name: &'static str,
    core: Arc<Mutex<NoteCore<DefaultNoteDoc, S, K>>>,
    engines: Arc<Mutex<HashMap<NoteId, SyncHandle>>>,
    endpoint: RelayEndpoint,
}

impl<S: OpStore + Send + 'static, K: Keyring + Send + 'static> Device<S, K> {
    fn new(
        name: &'static str,
        mut core: NoteCore<DefaultNoteDoc, S, K>,
        endpoint: &RelayEndpoint,
    ) -> Self {
        let engines: Arc<Mutex<HashMap<NoteId, SyncHandle>>> = Arc::default();
        let sink = engines.clone();
        core.on_account_envelope(Box::new(move |id, envelope| {
            if let Some(handle) = sink.lock().unwrap().get(&id) {
                handle.publish(envelope.to_vec());
            }
        }));
        let device = Self {
            name,
            core: Arc::new(Mutex::new(core)),
            engines,
            endpoint: endpoint.clone(),
        };
        device.connect_everything();
        device
    }

    fn connect_everything(&self) {
        let (mut ids, account) = {
            let core = self.core.lock().unwrap();
            (core.syncable().unwrap(), core.account_id())
        };
        ids.extend(account);
        let mut engines = self.engines.lock().unwrap();
        for id in ids {
            if engines.contains_key(&id) {
                continue;
            }
            let (engine, handle) = SyncEngine::start(self.core.clone(), id, self.endpoint.clone());
            tokio::spawn(engine.run());
            engines.insert(id, handle);
        }
    }

    fn publish(&self, note: NoteId, envelope: Vec<u8>) {
        self.connect_everything();
        if let Some(handle) = self.engines.lock().unwrap().get(&note) {
            handle.publish(envelope);
        }
    }

    fn settle(&self, note: NoteId) {
        if let Some(handle) = self.engines.lock().unwrap().get(&note) {
            handle.settle();
        }
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

    async fn wait_until_synced(&self) {
        let engines = self.engines.clone();
        self.wait_until("every document synced", || {
            let engines = engines.lock().unwrap();
            !engines.is_empty()
                && engines.values().all(|handle| {
                    let status = handle.status();
                    status.state == SyncState::Synced && status.pending == 0
                })
        })
        .await;
    }

    /// Stop every engine and hand back the core once nothing else holds it, so
    /// that its stores close.
    async fn shut_down(self) -> NoteCore<DefaultNoteDoc, S, K> {
        for handle in self.engines.lock().unwrap().values() {
            handle.shutdown();
        }
        let deadline = tokio::time::Instant::now() + PATIENCE;
        let mut core = self.core;
        loop {
            match Arc::try_unwrap(core) {
                Ok(inner) => return inner.into_inner().unwrap(),
                Err(still) => {
                    assert!(
                        tokio::time::Instant::now() < deadline,
                        "{}'s engines never let go",
                        self.name
                    );
                    core = still;
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            }
        }
    }
}

type OnDisk = NoteCore<DefaultNoteDoc, SqliteStore, SqliteKeyring>;
type InTab = NoteCore<DefaultNoteDoc, WebStore, WebKeyring>;
type InMemory = NoteCore<DefaultNoteDoc, MemoryStore, MemoryKeyring>;

fn on_disk(dir: &Path) -> OnDisk {
    std::fs::create_dir_all(dir).unwrap();
    let master = master_key_from_file(dir.join("master.key")).unwrap();
    NoteCore::open(
        SqliteStore::open(dir.join("notes.db"), master.clone()).unwrap(),
        SqliteKeyring::open(dir.join("keys.db"), master).unwrap(),
    )
    .unwrap()
}

/// Rows a browser's object store would hold, kept as the sink is told.
#[derive(Clone, Default)]
struct Rows(Arc<Mutex<BTreeMap<String, Vec<u8>>>>);

impl ChangeSink for Rows {
    fn changed(&self, changes: &[Change]) {
        let mut rows = self.0.lock().unwrap();
        for change in changes {
            match change {
                Change::Put { key, value } => {
                    rows.insert(key.clone(), value.clone());
                }
                Change::Delete { key } => {
                    rows.remove(key);
                }
                Change::DeletePrefix { prefix } => {
                    rows.retain(|key, _| !key.starts_with(prefix.as_str()));
                }
            }
        }
    }
}

#[derive(Serialize, Deserialize)]
struct TabExport {
    /// The master key the tab sealed its rows under, base64.
    master: String,
    notes: BTreeMap<String, String>,
    keys: BTreeMap<String, String>,
}

fn export(rows: &Rows) -> BTreeMap<String, String> {
    rows.0
        .lock()
        .unwrap()
        .iter()
        .map(|(k, v)| (k.clone(), B64.encode(v)))
        .collect()
}

fn import(rows: &BTreeMap<String, String>) -> Vec<(String, Vec<u8>)> {
    rows.iter()
        .map(|(k, v)| (k.clone(), B64.decode(v).unwrap()))
        .collect()
}

/// A PNG header and deterministic bytes: the importer checks the header. Small,
/// because every release adds a copy of it to the repository twice; chunked
/// uploads are `media.rs`'s to test, not this file's.
fn png(len: usize) -> Vec<u8> {
    let mut bytes = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
    bytes.extend_from_slice(&640u32.to_be_bytes());
    bytes.extend_from_slice(&480u32.to_be_bytes());
    bytes.extend((0..len - bytes.len()).map(|i| (i * 31 % 251) as u8));
    bytes
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

// ---------------------------------------------------------------------------
// Writing a set
// ---------------------------------------------------------------------------

/// Write this build's fixture set to `compat/v<version>/`.
///
/// Run through `just compat-fixtures` before a release is tagged, with
/// `DATABASE_URL` naming an **empty** database and `pg_dump` on the path; the
/// release workflow refuses a tag whose set is missing. It refuses to write
/// over a set that exists: once a release has one, it is that release's
/// record, and regenerating it would make the test agree with whatever the
/// code does now.
#[tokio::test]
#[ignore = "writes compat/; run by `just compat-fixtures`"]
async fn write_compat_fixtures() {
    let version = env!("CARGO_PKG_VERSION");
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL must name an empty database");
    let out = compat_root().join(format!("v{version}"));
    assert!(
        !out.exists() || std::env::var_os("COMPAT_REPLACE").is_some(),
        "{} exists; a release's fixtures are never rewritten (COMPAT_REPLACE=1 overrides, \
         only for a version that has not been tagged)",
        out.display()
    );
    if out.exists() {
        std::fs::remove_dir_all(&out).unwrap();
    }
    std::fs::create_dir_all(&out).unwrap();

    // The relay: Postgres, attachments on disk inside the set, and a log that
    // asks for compaction early so that a base is among what it keeps.
    let store = PostgresStore::connect(&url).await.unwrap();
    assert_eq!(
        easynotes_relay::RelayStore::usage(&store)
            .await
            .unwrap()
            .notes,
        0,
        "DATABASE_URL must name an empty database"
    );
    let relay_media = out.join("relay/media");
    let relay = Relay::serve_with(
        Arc::new(store),
        "127.0.0.1:0".parse().unwrap(),
        RelayConfig {
            storage: Limits {
                crowded_envelopes: 8,
                ..Limits::default()
            },
            media: MediaConfig {
                blobs: Some(Arc::new(DirBlobs::open(&relay_media).unwrap())),
                ..MediaConfig::default()
            },
            ..RelayConfig::default()
        },
    )
    .await
    .unwrap();
    let endpoint = RelayEndpoint::parse(&relay.http_base()).unwrap();
    let http = endpoint.http_base.clone();

    // The owner, with an account, and the guest, with one of their own.
    let mut ada_core = on_disk(&out.join("ada"));
    let phrase = ada_core.create_account().unwrap();
    ada_core.set_profile_name("Ada").unwrap();
    let ada = Device::new("Ada", ada_core, &endpoint);
    let mut ben_core = on_disk(&out.join("ben"));
    ben_core.create_account().unwrap();
    ben_core.set_profile_name("Ben").unwrap();
    let ben = Device::new("Ben", ben_core, &endpoint);

    // A checklist, coloured and pinned, with enough edits to be compacted.
    let groceries = ada
        .core
        .lock()
        .unwrap()
        .create_note(NoteKind::Checklist)
        .unwrap();
    let envelope = ada
        .core
        .lock()
        .unwrap()
        .set_title(groceries, "Groceries")
        .unwrap();
    ada.publish(groceries, envelope);
    for (text, ticked) in [
        ("milk", true),
        ("bread", false),
        ("olive oil", false),
        ("lemons", true),
        ("rice", false),
        ("coffee", false),
        ("apples", false),
        ("butter", true),
        ("flour", false),
        ("tea", false),
    ] {
        let (item, envelope) = ada.core.lock().unwrap().add_item(groceries, text).unwrap();
        ada.publish(groceries, envelope);
        if ticked {
            let envelope = ada
                .core
                .lock()
                .unwrap()
                .set_checked(groceries, item, true)
                .unwrap();
            ada.publish(groceries, envelope);
        }
    }
    {
        let mut core = ada.core.lock().unwrap();
        core.set_colour(groceries, Some(Colour::Green)).unwrap();
        core.set_pinned(groceries, true).unwrap();
    }

    // Prose, with a photo in it.
    let letter = ada
        .core
        .lock()
        .unwrap()
        .create_note(NoteKind::Text)
        .unwrap();
    let envelope = ada
        .core
        .lock()
        .unwrap()
        .set_title(letter, "Letter")
        .unwrap();
    ada.publish(letter, envelope);
    let envelope = ada
        .core
        .lock()
        .unwrap()
        .set_body(letter, "Dear Ben,\n\nThe garden is doing well.\n")
        .unwrap()
        .unwrap();
    ada.publish(letter, envelope);
    let ada_master = master_key_from_file(out.join("ada/master.key")).unwrap();
    let ada_media = MediaStore::open(out.join("ada/media"), ada_master.clone()).unwrap();
    let photo = png(20_000);
    let staged = ada_media
        .import(
            letter,
            "garden.png",
            photo.as_slice(),
            photo.len() as u64,
            None,
            ImportHints::default(),
        )
        .unwrap();
    let token = authenticate(&http, None, &DeviceIdentity::generate())
        .await
        .unwrap();
    upload_media(&http, &token, &ada_media, letter, &staged.media)
        .await
        .unwrap();
    let envelope = ada
        .core
        .lock()
        .unwrap()
        .add_attachment(letter, staged.id, staged.media.clone())
        .unwrap();
    ada.publish(letter, envelope);
    ada_media.settle(staged.id).unwrap();
    ada.wait_until_synced().await;

    // The checklist shared with the guest, who adds to it.
    let_in(&ada, &ben, groceries, &http).await;
    ben.wait_until("the shared note's items", || {
        ben.core
            .lock()
            .unwrap()
            .view(groceries)
            .is_ok_and(|v| v.items.len() == 10)
    })
    .await;
    let (_, envelope) = ben
        .core
        .lock()
        .unwrap()
        .add_item(groceries, "eggs")
        .unwrap();
    ben.publish(groceries, envelope);
    ben.wait_until_synced().await;
    ada.wait_until("the guest's item", || {
        ada.core
            .lock()
            .unwrap()
            .view(groceries)
            .is_ok_and(|v| v.items.iter().any(|i| i.text == "eggs"))
    })
    .await;
    ada.wait_until_synced().await;

    // A link for a third note, never opened on the relay.
    let invite_link = ada
        .core
        .lock()
        .unwrap()
        .new_invite(letter)
        .unwrap()
        .secret
        .link();

    // A browser tab recovering the account from the phrase.
    let tab_master = MasterKey::random();
    let (tab_notes, tab_keys) = (Rows::default(), Rows::default());
    let mut tab_core: InTab = NoteCore::open(
        WebStore::new(tab_master.clone(), Box::new(tab_notes.clone())),
        WebKeyring::new(tab_master.clone(), Box::new(tab_keys.clone())),
    )
    .unwrap();
    tab_core.adopt_account(&phrase).unwrap();
    let tab = Device::new("the tab", tab_core, &endpoint);
    tab.wait_until("both notes", || {
        let mut core = tab.core.lock().unwrap();
        core.notes().unwrap().len() == 2
            && core.view(groceries).is_ok_and(|v| v.items.len() == 11)
            && core.view(letter).is_ok_and(|v| v.attachments.len() == 1)
    })
    .await;
    tab.wait_until_synced().await;

    // What the owner sees is the record: the others are checked against it.
    let notes = {
        let mut core = ada.core.lock().unwrap();
        let summaries = core.notes().unwrap();
        let mut notes = Vec::new();
        for summary in summaries {
            let view = core.view(summary.id).unwrap();
            let mut note = describe(&view, &summary, summary.id == groceries);
            for attachment in &view.attachments {
                let bytes = ada_media
                    .read(
                        summary.id,
                        attachment.id,
                        &attachment.media,
                        Variant::Original,
                    )
                    .unwrap();
                note.attachments.push(ExpectAttachment {
                    name: attachment.media.name.clone(),
                    len: bytes.len() as u64,
                    sha256: sha256(&bytes),
                });
            }
            notes.push(note);
        }
        notes.sort_by(|a, b| a.title.cmp(&b.title));
        notes
    };
    assert_eq!(notes.len(), 2);
    assert_eq!(notes[1].attachments[0].sha256, sha256(&photo));
    assert_eq!(
        content(&ben.core.lock().unwrap().view(groceries).unwrap()),
        expected_content(&notes[0]),
        "the guest does not see what the owner does"
    );

    // Close everything, and compact the owner's prose locally so that a
    // snapshot row is among what the store holds.
    tab.shut_down().await;
    drop(ben.shut_down().await);
    let mut ada_core = ada.shut_down().await;
    ada_core.compact(letter).unwrap();
    drop(ada_core);
    drop(ada_media);
    for device in ["ada", "ben"] {
        settle_sqlite(&out.join(device));
    }

    std::fs::write(
        out.join("tab.json"),
        serde_json::to_string_pretty(&TabExport {
            master: B64.encode(tab_master.0),
            notes: export(&tab_notes),
            keys: export(&tab_keys),
        })
        .unwrap(),
    )
    .unwrap();

    // The relay's database, as a backup would have it, and a check that a
    // compacted base is among it.
    let pool = sqlx::PgPool::connect(&url).await.unwrap();
    let bases: i64 = sqlx::query_scalar("SELECT count(*) FROM note_head WHERE covers > 0")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(bases > 0, "no note was compacted on the relay");
    drop(relay);
    let dump = std::process::Command::new("pg_dump")
        .args(["--inserts", "--no-owner", "--no-privileges", &url])
        .output()
        .expect("pg_dump must be on the path");
    assert!(
        dump.status.success(),
        "pg_dump failed: {}",
        String::from_utf8_lossy(&dump.stderr)
    );
    std::fs::write(out.join("relay/dump.sql"), dump.stdout).unwrap();
    let _ = std::fs::remove_dir_all(relay_media.join("tmp"));
    let _ = std::fs::remove_dir_all(relay_media.join("parts"));

    let expect = Expect {
        version: version.into(),
        phrase: phrase.words(),
        notes,
        invite_link,
    };
    std::fs::write(
        out.join("expect.json"),
        serde_json::to_string_pretty(&expect).unwrap() + "\n",
    )
    .unwrap();
    std::fs::write(
        out.join("README.md"),
        format!(
            "# Compatibility fixtures written by {version}\n\n\
             Made by `just compat-fixtures` and read by `relay/tests/compat.rs` on every \
             test run ([ADR 0029](../../docs/adr/0029-compatibility-across-releases.md) §6). \
             Never edit anything here: once this release is tagged, these files are its \
             record of what it wrote. The phrase and keys are test data.\n"
        ),
    )
    .unwrap();
}

/// Leave a SQLite database as one self-contained file: the log folded in and
/// the journal mode that needs no side files. The next open sets WAL again.
fn settle_sqlite(dir: &Path) {
    for file in ["notes.db", "keys.db"] {
        let conn = rusqlite::Connection::open(dir.join(file)).unwrap();
        conn.pragma_update(None, "wal_checkpoint", "TRUNCATE")
            .unwrap();
        conn.pragma_update(None, "journal_mode", "DELETE").unwrap();
    }
    for leftover in ["notes.db-wal", "notes.db-shm", "keys.db-wal", "keys.db-shm"] {
        let _ = std::fs::remove_file(dir.join(leftover));
    }
}

/// The whole invite, as the app runs it (the same steps as `sharing.rs`).
async fn let_in<S1, K1, S2, K2>(
    owner: &Device<S1, K1>,
    guest: &Device<S2, K2>,
    note: NoteId,
    http: &str,
) where
    S1: OpStore + Send + 'static,
    K1: Keyring + Send + 'static,
    S2: OpStore + Send + 'static,
    K2: Keyring + Send + 'static,
{
    let outgoing = owner.core.lock().unwrap().new_invite(note).unwrap();
    invite::open(http, None, &outgoing.secret, &outgoing.owner, 3600)
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
    let request = invite::request(http, None, &secret, &member, &sealed)
        .await
        .unwrap();
    let listing = invite::list(http, None, &outgoing.secret, &outgoing.owner)
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
        http,
        None,
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
    } = invite::poll(http, None, &secret, &member, &request)
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

// ---------------------------------------------------------------------------
// Reading every set
// ---------------------------------------------------------------------------

/// Every set under `compat/`, opened by this build. See the module comment for
/// what is checked; a set that no longer reads is a failed build.
#[tokio::test]
async fn every_fixture_set_still_reads() {
    let mut sets: Vec<PathBuf> = std::fs::read_dir(compat_root())
        .map(|entries| {
            entries
                .map(|e| e.unwrap().path())
                .filter(|p| p.join("expect.json").exists())
                .collect()
        })
        .unwrap_or_default();
    sets.sort();
    assert!(
        !sets.is_empty(),
        "no fixture sets in compat/: run `just compat-fixtures`"
    );
    for set in sets {
        let expect: Expect =
            serde_json::from_str(&std::fs::read_to_string(set.join("expect.json")).unwrap())
                .unwrap();
        let name = set.file_name().unwrap().to_string_lossy().into_owned();
        read_local_stores(&set, &expect, &name);
        read_the_tab(&set, &expect, &name);
        InviteSecret::from_link(&expect.invite_link)
            .unwrap_or_else(|e| panic!("{name}: its invite link no longer parses: {e}"));
        RecoveryPhrase::parse(&expect.phrase)
            .unwrap_or_else(|e| panic!("{name}: its phrase no longer parses: {e}"));
        read_the_relay(&set, &expect, &name).await;
    }
}

fn note_id(note: &ExpectNote) -> NoteId {
    NoteId::decode(&note.id).unwrap()
}

/// The owner's and the guest's SQLite stores, and the owner's attachments.
fn read_local_stores(set: &Path, expect: &Expect, name: &str) {
    let scratch = tempfile::tempdir().unwrap();
    for device in ["ada", "ben"] {
        copy_dir(&set.join(device), &scratch.path().join(device));
    }

    let mut ada = on_disk(&scratch.path().join("ada"));
    let summaries = ada.notes().unwrap();
    assert_eq!(
        summaries.len(),
        expect.notes.len(),
        "{name}: the owner's notes"
    );
    let master = master_key_from_file(scratch.path().join("ada/master.key")).unwrap();
    let media = MediaStore::open(scratch.path().join("ada/media"), master).unwrap();
    for note in &expect.notes {
        let id = note_id(note);
        let view = ada
            .open_note(id)
            .unwrap_or_else(|e| panic!("{name}: the owner cannot open {:?}: {e}", note.title));
        assert_eq!(
            content(&view),
            expected_content(note),
            "{name}: the owner's {:?}",
            note.title
        );
        let summary = summaries.iter().find(|s| s.id == id).unwrap();
        assert_eq!(
            summary.placement.colour.map(|c| format!("{c:?}")),
            note.colour,
            "{name}: {:?}'s colour",
            note.title
        );
        assert_eq!(
            summary.placement.pinned, note.pinned,
            "{name}: {:?} pinned",
            note.title
        );
        for (attachment, expected) in view.attachments.iter().zip(&note.attachments) {
            let bytes = media
                .read(id, attachment.id, &attachment.media, Variant::Original)
                .unwrap_or_else(|e| panic!("{name}: the owner's {:?}: {e}", expected.name));
            assert_eq!(
                sha256(&bytes),
                expected.sha256,
                "{name}: {:?}'s bytes",
                expected.name
            );
        }
    }
    assert!(ada.has_account(), "{name}: the owner's account");

    let mut ben = on_disk(&scratch.path().join("ben"));
    for note in expect.notes.iter().filter(|n| n.shared) {
        let view = ben
            .open_note(note_id(note))
            .unwrap_or_else(|e| panic!("{name}: the guest cannot open {:?}: {e}", note.title));
        assert_eq!(
            content(&view),
            expected_content(note),
            "{name}: the guest's {:?}",
            note.title
        );
    }
}

/// A browser tab's rows, as IndexedDB held them.
fn read_the_tab(set: &Path, expect: &Expect, name: &str) {
    let tab: TabExport =
        serde_json::from_str(&std::fs::read_to_string(set.join("tab.json")).unwrap()).unwrap();
    let master = MasterKey(B64.decode(&tab.master).unwrap().try_into().unwrap());
    let mut core: InTab = NoteCore::open(
        WebStore::from_rows(
            master.clone(),
            Box::new(Rows::default()),
            import(&tab.notes),
        )
        .unwrap_or_else(|e| panic!("{name}: the tab's notes: {e}")),
        WebKeyring::from_rows(master, Box::new(Rows::default()), import(&tab.keys))
            .unwrap_or_else(|e| panic!("{name}: the tab's keys: {e}")),
    )
    .unwrap();
    assert!(core.has_account(), "{name}: the tab's account");
    for note in &expect.notes {
        let view = core
            .open_note(note_id(note))
            .unwrap_or_else(|e| panic!("{name}: the tab cannot open {:?}: {e}", note.title));
        assert_eq!(
            content(&view),
            expected_content(note),
            "{name}: the tab's {:?}",
            note.title
        );
    }
}

/// The relay's backup restored into a fresh database, served by this build's
/// relay, and the account recovered from its phrase on a fresh device.
async fn read_the_relay(set: &Path, expect: &Expect, name: &str) {
    let Ok(url) = std::env::var("DATABASE_URL") else {
        eprintln!("{name}: skipping the relay's dump: DATABASE_URL is not set");
        return;
    };
    let database = format!(
        "compat_{}",
        NoteId::random()
            .encode()
            .to_lowercase()
            .replace(['-', '_'], "")
    );
    let admin = sqlx::PgPool::connect(&url).await.unwrap();
    sqlx::query(&format!("CREATE DATABASE {database}"))
        .execute(&admin)
        .await
        .unwrap();
    let restored = with_database(&url, &database);
    {
        // `psql` would run the dump's meta-commands; there are none this needs,
        // and a plain connection runs the rest.
        let dump = std::fs::read_to_string(set.join("relay/dump.sql")).unwrap();
        let sql: String = dump
            .lines()
            .filter(|line| !line.starts_with('\\'))
            .collect::<Vec<_>>()
            .join("\n");
        let mut conn = <sqlx::PgConnection as sqlx::Connection>::connect(&restored)
            .await
            .unwrap();
        sqlx::raw_sql(&sql)
            .execute(&mut conn)
            .await
            .unwrap_or_else(|e| panic!("{name}: the relay's dump does not restore: {e}"));
    }

    let media = tempfile::tempdir().unwrap();
    copy_dir(&set.join("relay/media"), media.path());
    let store = PostgresStore::connect(&restored)
        .await
        .unwrap_or_else(|e| panic!("{name}: this relay cannot open that database: {e}"));
    let relay = Relay::serve_with(
        Arc::new(store),
        "127.0.0.1:0".parse().unwrap(),
        RelayConfig {
            media: MediaConfig {
                blobs: Some(Arc::new(DirBlobs::open(media.path()).unwrap())),
                ..MediaConfig::default()
            },
            ..RelayConfig::default()
        },
    )
    .await
    .unwrap();
    let endpoint = RelayEndpoint::parse(&relay.http_base()).unwrap();

    let mut core: InMemory = NoteCore::open(MemoryStore::new(), MemoryKeyring::new()).unwrap();
    core.adopt_account(&RecoveryPhrase::parse(&expect.phrase).unwrap())
        .unwrap();
    let device = Device::new("a recovered device", core, &endpoint);
    device
        .wait_until("every note from the relay", || {
            let core = device.core.lock().unwrap();
            expect.notes.iter().all(|note| {
                core.view(note_id(note))
                    .is_ok_and(|view| content(&view) == expected_content(note))
            })
        })
        .await;
    let summaries = device.core.lock().unwrap().notes().unwrap();
    for note in &expect.notes {
        let summary = summaries.iter().find(|s| s.id == note_id(note)).unwrap();
        assert_eq!(
            summary.placement.colour.map(|c| format!("{c:?}")),
            note.colour,
            "{name}: {:?}'s colour, recovered",
            note.title
        );
    }

    // The attachment's bytes, through this build's download.
    let scratch = tempfile::tempdir().unwrap();
    let downloaded = MediaStore::open(scratch.path(), MasterKey::random()).unwrap();
    let token = authenticate(&endpoint.http_base, None, &DeviceIdentity::generate())
        .await
        .unwrap();
    for note in &expect.notes {
        let id = note_id(note);
        let view = device.core.lock().unwrap().view(id).unwrap();
        for (attachment, expected) in view.attachments.iter().zip(&note.attachments) {
            download_into(
                &endpoint.http_base,
                &token,
                &downloaded,
                id,
                &attachment.media.original,
            )
            .await
            .unwrap_or_else(|e| panic!("{name}: {:?} from the relay: {e}", expected.name));
            let bytes = downloaded
                .read(id, attachment.id, &attachment.media, Variant::Original)
                .unwrap();
            assert_eq!(
                sha256(&bytes),
                expected.sha256,
                "{name}: {:?} from the relay",
                expected.name
            );
        }
    }
    drop(device.shut_down().await);
    drop(relay);
    let _ = sqlx::query(&format!("DROP DATABASE {database} WITH (FORCE)"))
        .execute(&admin)
        .await;
}

/// `url` with its database name replaced.
fn with_database(url: &str, database: &str) -> String {
    let (base, query) = match url.split_once('?') {
        Some((base, query)) => (base, format!("?{query}")),
        None => (url, String::new()),
    };
    let (server, _) = base
        .rsplit_once('/')
        .expect("DATABASE_URL names a database");
    format!("{server}/{database}{query}")
}
