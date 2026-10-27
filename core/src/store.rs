//! Where the encrypted op log lives.
//!
//! Every store here holds the *encrypted* log plus an encrypted snapshot and keeps
//! the decrypted CRDT in memory only, so no store has ever been handed plaintext
//! to write. What the real one adds is the master key: the rows that are not
//! already envelopes — the device seed, the note keys, the sync cursor — are
//! sealed rather than lying beside the log in the clear.
//!
//! The trait is here so the web build can back it with IndexedDB without anything
//! above it changing.
//!
//! Four implementations today: [`MemoryStore`] for tests and for a tab with
//! nowhere to keep anything, [`FileStore`] from the initial implementation, [`SqliteStore`] — what
//! the native app opens — and [`WebStore`], which is what a browser gets.
//! [ADR 0005](../../docs/decisions.md#adr-0005) says why the third is plain
//! SQLite with sealed values rather than SQLCipher, and
//! [ADR 0011](../../docs/decisions.md#adr-0011) why the fourth cannot write
//! for itself.

use crate::error::Result;
use crate::ids::NoteId;
use std::collections::HashMap;

pub trait OpStore {
    /// Append one encoded envelope to a note's log.
    fn append(&mut self, note: NoteId, envelope: &[u8]) -> Result<()>;

    /// Every envelope for a note, oldest first.
    fn load(&self, note: NoteId) -> Result<Vec<Vec<u8>>>;

    /// Write a note's encrypted snapshot, replacing any previous one. It does not
    /// touch the log: compaction is a snapshot *and then* a shorter log, in that
    /// order, so that a crash between the two costs replay time and never an
    /// edit.
    fn put_snapshot(&mut self, note: NoteId, snapshot: &[u8]) -> Result<()>;

    fn get_snapshot(&self, note: NoteId) -> Result<Option<Vec<u8>>>;

    /// Replace a note's log with exactly `envelopes`, in this order.
    ///
    /// The second half of compaction, and one call rather than a clear followed
    /// by appends because those two can be interrupted between each other and
    /// this cannot.
    fn replace_log(&mut self, note: NoteId, envelopes: &[Vec<u8>]) -> Result<()>;

    /// How many envelopes a note's log holds.
    ///
    /// The default answer reads the log to count it, which is what deciding
    /// whether to compact would otherwise cost. A store that can answer without
    /// reading should.
    fn log_len(&self, note: NoteId) -> Result<usize> {
        Ok(self.load(note)?.len())
    }

    /// Every note this store holds anything for, in a stable order.
    ///
    /// "Anything" is the whole of it: a log, a snapshot, or only the metadata a
    /// note gets when it is created. A note made and not yet edited has nothing
    /// but that row, and a note list that dropped it would show a note until the
    /// app restarted and then lose it.
    fn notes(&self) -> Result<Vec<NoteId>>;

    /// Small per-note bookkeeping that is not part of the log: the sync cursor,
    /// and where the note sits in this device's list.
    ///
    /// It lives beside the log rather than in it because it is *local*: it
    /// records what this device has sent, received and shows first, it is
    /// meaningless on another device, and it must never be handed to the relay.
    /// `key` is an internal name, not user data.
    fn put_meta(&mut self, note: NoteId, key: &str, value: &[u8]) -> Result<()>;

    fn get_meta(&self, note: NoteId, key: &str) -> Result<Option<Vec<u8>>>;

    /// Forget a note entirely: its log, its snapshot, and every metadata row it
    /// has — the sync cursor, the ordinal, the sealed index entry, all of them.
    ///
    /// The only thing in this trait that destroys something a person would
    /// miss, and what emptying the trash is made of. Three things follow from
    /// that:
    ///
    /// * **One call, not a delete per table.** A store that can do it in one
    ///   transaction should, because the halfway state is the dangerous one: a
    ///   log removed with the snapshot still there has deleted the edits and
    ///   kept the note.
    /// * **Every metadata row, whatever it is called.** The index entry holds
    ///   the note's whole text ([`crate::index`]), so a delete that removed the
    ///   log and left the metadata would leave the readable copy behind and
    ///   remove the authoritative one. No implementation here names the keys it
    ///   knows; they all clear whatever is there.
    /// * **Forgetting a note the store does not hold is not an error.** Emptying
    ///   an empty trash and finishing a delete that a crash interrupted both ask
    ///   for exactly this, and neither is a problem.
    ///
    /// The key is not here, because key material is not kept where the log is:
    /// [`Keyring::forget_note_key`] is the other half, and the order the two go
    /// in is a decision of its own —
    /// [ADR 0010](../../docs/decisions.md#adr-0010).
    fn forget(&mut self, note: NoteId) -> Result<()>;
}

/// In-memory store, for tests and for the headless cores in the integration test.
#[derive(Default, Clone)]
pub struct MemoryStore {
    logs: HashMap<NoteId, Vec<Vec<u8>>>,
    snapshots: HashMap<NoteId, Vec<u8>>,
    meta: HashMap<(NoteId, String), Vec<u8>>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// How many envelopes a note's log holds. Tests assert on this to catch a
    /// replay being persisted twice.
    pub fn len(&self, note: NoteId) -> usize {
        self.logs.get(&note).map_or(0, |l| l.len())
    }

    pub fn is_empty(&self, note: NoteId) -> bool {
        self.len(note) == 0
    }
}

impl OpStore for MemoryStore {
    fn append(&mut self, note: NoteId, envelope: &[u8]) -> Result<()> {
        self.logs.entry(note).or_default().push(envelope.to_vec());
        Ok(())
    }

    fn load(&self, note: NoteId) -> Result<Vec<Vec<u8>>> {
        Ok(self.logs.get(&note).cloned().unwrap_or_default())
    }

    fn put_snapshot(&mut self, note: NoteId, snapshot: &[u8]) -> Result<()> {
        self.snapshots.insert(note, snapshot.to_vec());
        Ok(())
    }

    fn replace_log(&mut self, note: NoteId, envelopes: &[Vec<u8>]) -> Result<()> {
        self.logs.insert(note, envelopes.to_vec());
        Ok(())
    }

    fn get_snapshot(&self, note: NoteId) -> Result<Option<Vec<u8>>> {
        Ok(self.snapshots.get(&note).cloned())
    }

    fn log_len(&self, note: NoteId) -> Result<usize> {
        // Asked once per edit by the index, so answering it by cloning the whole
        // log would make every keystroke quadratic in the log behind it.
        Ok(self.len(note))
    }

    fn put_meta(&mut self, note: NoteId, key: &str, value: &[u8]) -> Result<()> {
        self.meta.insert((note, key.to_string()), value.to_vec());
        Ok(())
    }

    fn get_meta(&self, note: NoteId, key: &str) -> Result<Option<Vec<u8>>> {
        Ok(self.meta.get(&(note, key.to_string())).cloned())
    }

    fn forget(&mut self, note: NoteId) -> Result<()> {
        self.logs.remove(&note);
        self.snapshots.remove(&note);
        self.meta.retain(|(id, _), _| *id != note);
        Ok(())
    }

    fn notes(&self) -> Result<Vec<NoteId>> {
        let mut ids: Vec<NoteId> = self.logs.keys().copied().collect();
        for id in self
            .snapshots
            .keys()
            .chain(self.meta.keys().map(|(id, _)| id))
        {
            if !ids.contains(id) {
                ids.push(*id);
            }
        }
        ids.sort();
        Ok(ids)
    }
}

/// Native store: one length-prefixed append-only log file per note, plus a
/// snapshot file. Everything written here is already ciphertext.
#[cfg(feature = "file-store")]
pub mod file {
    use super::*;
    use crate::error::CoreError;
    use std::fs;
    use std::io::Write;
    use std::path::{Path, PathBuf};

    pub struct FileStore {
        root: PathBuf,
    }

    fn io(e: std::io::Error) -> CoreError {
        CoreError::Storage(e.to_string())
    }

    impl FileStore {
        pub fn open(root: impl AsRef<Path>) -> Result<Self> {
            let root = root.as_ref().to_path_buf();
            fs::create_dir_all(&root).map_err(io)?;
            Ok(Self { root })
        }

        fn log_path(&self, note: NoteId) -> PathBuf {
            self.root.join(format!("{}.log", note.encode()))
        }

        fn snapshot_path(&self, note: NoteId) -> PathBuf {
            self.root.join(format!("{}.snap", note.encode()))
        }

        fn meta_path(&self, note: NoteId, key: &str) -> PathBuf {
            self.root.join(format!("{}.{key}.meta", note.encode()))
        }
    }

    impl OpStore for FileStore {
        fn append(&mut self, note: NoteId, envelope: &[u8]) -> Result<()> {
            let mut f = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(self.log_path(note))
                .map_err(io)?;
            let len = u32::try_from(envelope.len()).map_err(|_| CoreError::TooLarge {
                limit: u32::MAX as usize,
            })?;
            f.write_all(&len.to_be_bytes()).map_err(io)?;
            f.write_all(envelope).map_err(io)?;
            // the initial implementation does not fsync per append; a crash may lose the last edit.
            // The real store (the second milestone) decides the durability policy.
            Ok(())
        }

        fn load(&self, note: NoteId) -> Result<Vec<Vec<u8>>> {
            let path = self.log_path(note);
            if !path.exists() {
                return Ok(Vec::new());
            }
            let raw = fs::read(&path).map_err(io)?;
            let mut out = Vec::new();
            let mut at = 0usize;
            while at + 4 <= raw.len() {
                let len = u32::from_be_bytes(raw[at..at + 4].try_into().unwrap()) as usize;
                at += 4;
                if at + len > raw.len() {
                    // A torn tail from a crash mid-append: keep what is whole.
                    break;
                }
                out.push(raw[at..at + len].to_vec());
                at += len;
            }
            Ok(out)
        }

        fn put_snapshot(&mut self, note: NoteId, snapshot: &[u8]) -> Result<()> {
            fs::write(self.snapshot_path(note), snapshot).map_err(io)
        }

        fn replace_log(&mut self, note: NoteId, envelopes: &[Vec<u8>]) -> Result<()> {
            // Built whole and renamed over the old one, so a crash mid-compaction
            // leaves the log that was there rather than half of the one replacing
            // it.
            let path = self.log_path(note);
            let tmp = path.with_extension("log.tmp");
            let mut out = Vec::new();
            for envelope in envelopes {
                let len = u32::try_from(envelope.len()).map_err(|_| CoreError::TooLarge {
                    limit: u32::MAX as usize,
                })?;
                out.extend_from_slice(&len.to_be_bytes());
                out.extend_from_slice(envelope);
            }
            fs::write(&tmp, &out).map_err(io)?;
            fs::rename(&tmp, &path).map_err(io)
        }

        fn get_snapshot(&self, note: NoteId) -> Result<Option<Vec<u8>>> {
            let path = self.snapshot_path(note);
            if !path.exists() {
                return Ok(None);
            }
            Ok(Some(fs::read(&path).map_err(io)?))
        }

        fn put_meta(&mut self, note: NoteId, key: &str, value: &[u8]) -> Result<()> {
            // Written through a temporary file and renamed, because a half-written
            // cursor is worse than no cursor: it would make the client replay from
            // a sequence number the relay never issued.
            let path = self.meta_path(note, key);
            let tmp = path.with_extension("meta.tmp");
            fs::write(&tmp, value).map_err(io)?;
            fs::rename(&tmp, &path).map_err(io)
        }

        fn get_meta(&self, note: NoteId, key: &str) -> Result<Option<Vec<u8>>> {
            let path = self.meta_path(note, key);
            if !path.exists() {
                return Ok(None);
            }
            Ok(Some(fs::read(&path).map_err(io)?))
        }

        fn forget(&mut self, note: NoteId) -> Result<()> {
            // Every file whose name begins with the note's ID, rather than the
            // three suffixes this store knows about. An ID is a fixed 22
            // characters and the dot after it cannot be part of one, so nothing
            // else can match — and naming the suffixes would mean this needed
            // editing every time something new is kept per note, with the one
            // that was forgotten being the one left readable on the disk. It
            // catches the `.tmp` files an interrupted write leaves behind for
            // the same reason.
            let prefix = format!("{}.", note.encode());
            for entry in fs::read_dir(&self.root).map_err(io)? {
                let entry = entry.map_err(io)?;
                if !entry.file_name().to_string_lossy().starts_with(&prefix) {
                    continue;
                }
                match fs::remove_file(entry.path()) {
                    Ok(()) => {}
                    // Something else got there first, which is the outcome this
                    // asked for.
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(io(e)),
                }
            }
            Ok(())
        }

        fn notes(&self) -> Result<Vec<NoteId>> {
            // Every suffix, not just `.log`: a note compacted to a snapshot has
            // no log left, and a note that was created and not yet edited has
            // only its metadata. Listing one of the three would make such a note
            // disappear while it is still right there. The conformance suite in
            // `tests/store.rs` caught the first of those by holding all three
            // stores to the same answer.
            let mut ids = Vec::new();
            for entry in fs::read_dir(&self.root).map_err(io)? {
                let entry = entry.map_err(io)?;
                let name = entry.file_name().to_string_lossy().into_owned();
                // A meta file is `{note}.{key}.meta`, so the note is what is
                // left once both suffixes are off.
                let stem = name
                    .strip_suffix(".log")
                    .or_else(|| name.strip_suffix(".snap"))
                    .or_else(|| {
                        name.strip_suffix(".meta")
                            .and_then(|rest| rest.rsplit_once('.'))
                            .map(|(note, _)| note)
                    });
                if let Some(stem) = stem {
                    if let Ok(id) = NoteId::decode(stem) {
                        if !ids.contains(&id) {
                            ids.push(id);
                        }
                    }
                }
            }
            ids.sort();
            Ok(ids)
        }
    }
}

#[cfg(feature = "file-store")]
pub use file::FileStore;

#[cfg(feature = "sqlite-store")]
pub mod sqlite;

#[cfg(feature = "sqlite-store")]
pub use sqlite::{open_local, SqliteKeyring, SqliteStore};

#[cfg(feature = "web-store")]
pub mod web;

#[cfg(feature = "web-store")]
pub use web::{Change, ChangeSink, WebKeyring, WebStore};

// ---------------------------------------------------------------------------
// What the master key is asked to seal, and against what
// ---------------------------------------------------------------------------
//
// Here rather than in one store, because two stores now hold the same rows and a
// device can be carried from either to the other. A context that differed
// between them would make that carry-over produce a database whose rows are all
// individually valid and none of which open.

/// A metadata row is sealed against the note it belongs to and the name it is
/// filed under, so that a row cannot be moved to another note or another name
/// and still open.
///
/// Gated because the relay builds this crate with no stores at all — it needs
/// the wire format and nothing else — and an ungated helper nothing calls is a
/// warning in that build.
#[cfg(any(feature = "sqlite-store", feature = "web-store"))]
pub(crate) fn meta_context<'a>(note: &'a NoteId, key: &'a str) -> [&'a [u8]; 3] {
    [b"meta", note.as_bytes(), key.as_bytes()]
}

/// A keyring row is sealed against what kind of secret it is and which note it
/// is for, so a note key cannot be handed back as the device seed by rewriting
/// one column. The device seed is the one row with no note, and passes an empty
/// ID.
#[cfg(any(feature = "sqlite-store", feature = "web-store"))]
pub(crate) fn key_context<'a>(kind: &'a str, id: &'a [u8]) -> [&'a [u8]; 2] {
    [kind.as_bytes(), id]
}

// ---------------------------------------------------------------------------
// Keyring
// ---------------------------------------------------------------------------

/// Where note keys and the device seed live.
///
/// A separate trait from [`OpStore`] precisely so that "the log holds only
/// ciphertext" stays true: key material never goes near it. What each
/// implementation keeps is already wrapped under a master key, and the master
/// key itself is the platform's business — the OS keychain on a device, and in
/// a browser the nearest thing one has
/// ([ADR 0011](../../docs/decisions.md#adr-0011)). [`FileKeyring`] is the
/// the initial implementation stand-in that wrapped nothing, and is kept for the migration off it.
pub trait Keyring {
    fn device_seed(&self) -> Result<Option<crate::crypto::DeviceSeed>>;
    fn put_device_seed(&mut self, seed: &crate::crypto::DeviceSeed) -> Result<()>;

    /// This device's account root, if it has been given one.
    ///
    /// The second device-wide secret, and here rather than in [`OpStore`] for
    /// exactly the reason the device seed is: it is key material, and the log
    /// holds only ciphertext. `None` is the ordinary state of a device that has
    /// notes and no account yet, not a fault.
    ///
    /// The recovery phrase itself is **never** stored — this is what it
    /// derives, and a device that has it can reopen the account document
    /// without asking anybody to type twelve words again. See
    /// [`crate::account`].
    fn account_root(&self) -> Result<Option<crate::account::AccountRoot>>;
    fn put_account_root(&mut self, root: &crate::account::AccountRoot) -> Result<()>;
    fn note_key(&self, note: NoteId) -> Result<Option<crate::crypto::NoteKey>>;
    fn put_note_key(&mut self, note: NoteId, key: &crate::crypto::NoteKey) -> Result<()>;

    /// Forget a note's key.
    ///
    /// The other half of [`OpStore::forget`]. Two traits rather than one because
    /// key material is not kept where the log is — two SQLite files on a device,
    /// and one day the OS keychain — so nothing can remove both under one
    /// transaction, and pretending otherwise would hide the window rather than
    /// close it. Forgetting a key that is not there is not an error.
    fn forget_note_key(&mut self, note: NoteId) -> Result<()>;

    /// Forget every key this keyring holds: the device seed, the account root
    /// and every note key. What deleting an account leaves on the device
    /// ([ADR 0026](../../docs/decisions.md#adr-0026)): nothing that
    /// opens anything, and no identity the next thing this device does could
    /// be linked through.
    fn forget_everything(&mut self) -> Result<()>;
}

#[derive(Default, Clone)]
pub struct MemoryKeyring {
    seed: Option<crate::crypto::DeviceSeed>,
    root: Option<crate::account::AccountRoot>,
    keys: HashMap<NoteId, crate::crypto::NoteKey>,
}

impl MemoryKeyring {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Keyring for MemoryKeyring {
    fn device_seed(&self) -> Result<Option<crate::crypto::DeviceSeed>> {
        Ok(self.seed.clone())
    }

    fn put_device_seed(&mut self, seed: &crate::crypto::DeviceSeed) -> Result<()> {
        self.seed = Some(seed.clone());
        Ok(())
    }

    fn account_root(&self) -> Result<Option<crate::account::AccountRoot>> {
        Ok(self.root.clone())
    }

    fn put_account_root(&mut self, root: &crate::account::AccountRoot) -> Result<()> {
        self.root = Some(root.clone());
        Ok(())
    }

    fn note_key(&self, note: NoteId) -> Result<Option<crate::crypto::NoteKey>> {
        Ok(self.keys.get(&note).cloned())
    }

    fn put_note_key(&mut self, note: NoteId, key: &crate::crypto::NoteKey) -> Result<()> {
        self.keys.insert(note, key.clone());
        Ok(())
    }

    fn forget_everything(&mut self) -> Result<()> {
        *self = Self::default();
        Ok(())
    }

    fn forget_note_key(&mut self, note: NoteId) -> Result<()> {
        self.keys.remove(&note);
        Ok(())
    }
}

/// File-backed keyring. the initial implementation only; see the note on [`Keyring`].
#[cfg(feature = "file-store")]
pub mod file_keyring {
    use super::*;
    use crate::crypto::{DeviceSeed, NoteKey};
    use crate::error::CoreError;
    use std::fs;
    use std::path::{Path, PathBuf};

    pub struct FileKeyring {
        root: PathBuf,
    }

    fn io(e: std::io::Error) -> CoreError {
        CoreError::Storage(e.to_string())
    }

    impl FileKeyring {
        pub fn open(root: impl AsRef<Path>) -> Result<Self> {
            let root = root.as_ref().to_path_buf();
            fs::create_dir_all(&root).map_err(io)?;
            Ok(Self { root })
        }
    }

    impl Keyring for FileKeyring {
        fn device_seed(&self) -> Result<Option<DeviceSeed>> {
            let p = self.root.join("device.seed");
            if !p.exists() {
                return Ok(None);
            }
            let raw = fs::read(&p).map_err(io)?;
            let b: [u8; 64] = raw
                .try_into()
                .map_err(|_| CoreError::Storage("device seed is not 64 bytes".into()))?;
            Ok(Some(DeviceSeed(b)))
        }

        fn put_device_seed(&mut self, seed: &DeviceSeed) -> Result<()> {
            fs::write(self.root.join("device.seed"), seed.0).map_err(io)
        }

        fn account_root(&self) -> Result<Option<crate::account::AccountRoot>> {
            let p = self.root.join("account.root");
            if !p.exists() {
                return Ok(None);
            }
            Ok(Some(crate::account::AccountRoot::from_slice(
                &fs::read(&p).map_err(io)?,
            )?))
        }

        fn put_account_root(&mut self, root: &crate::account::AccountRoot) -> Result<()> {
            fs::write(self.root.join("account.root"), root.as_bytes()).map_err(io)
        }

        fn note_key(&self, note: NoteId) -> Result<Option<NoteKey>> {
            let p = self.root.join(format!("{}.key", note.encode()));
            if !p.exists() {
                return Ok(None);
            }
            let raw = fs::read(&p).map_err(io)?;
            let b: [u8; 32] = raw
                .try_into()
                .map_err(|_| CoreError::Storage("note key is not 32 bytes".into()))?;
            Ok(Some(NoteKey(b)))
        }

        fn put_note_key(&mut self, note: NoteId, key: &NoteKey) -> Result<()> {
            fs::write(self.root.join(format!("{}.key", note.encode())), key.0).map_err(io)
        }

        fn forget_note_key(&mut self, note: NoteId) -> Result<()> {
            match fs::remove_file(self.root.join(format!("{}.key", note.encode()))) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(io(e)),
            }
        }

        fn forget_everything(&mut self) -> Result<()> {
            for entry in fs::read_dir(&self.root).map_err(io)? {
                let path = entry.map_err(io)?.path();
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if name == "device.seed" || name == "account.root" || name.ends_with(".key") {
                    fs::remove_file(&path).map_err(io)?;
                }
            }
            Ok(())
        }
    }
}

#[cfg(feature = "file-store")]
pub use file_keyring::FileKeyring;

/// The master key a device wrote beside its database before the app could reach
/// the OS keychain, if it has one. Never creates one.
///
/// This is the whole of the migration off that file. A device that has it is
/// unlocked by it and by nothing else, so the app reads it once, puts it in the
/// keychain, and finds it there from then on. The file is left where it is, like
/// every other thing this store has replaced.
#[cfg(feature = "file-store")]
pub fn existing_master_key(
    path: impl AsRef<std::path::Path>,
) -> Result<Option<crate::crypto::MasterKey>> {
    use crate::crypto::MasterKey;
    let path = path.as_ref();
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read(path).map_err(|e| crate::error::CoreError::Storage(e.to_string()))?;
    Ok(Some(MasterKey::from_slice(&raw)?))
}

/// Read the master key from `path`, generating one on first run.
///
/// **Not what the app does any more.** A device's key is in the OS keychain and
/// is handed to [`open_local`](sqlite::open_local) by the caller; this is what a
/// `cargo test` or a benchmark uses instead, because neither has a keychain and
/// both need a key that survives being reopened. What the app kept from it is
/// the shape: exactly one secret, in one place, with one way in and out.
#[cfg(feature = "file-store")]
pub fn master_key_from_file(path: impl AsRef<std::path::Path>) -> Result<crate::crypto::MasterKey> {
    use crate::crypto::MasterKey;
    use crate::error::CoreError;

    let path = path.as_ref();
    let io = |e: std::io::Error| CoreError::Storage(e.to_string());

    if path.exists() {
        return MasterKey::from_slice(&std::fs::read(path).map_err(io)?);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(io)?;
    }

    // Written to a private temporary file and linked into place, which does two
    // things at once. A reader never sees a half-written key, and a second
    // process that got here at the same moment does not overwrite the key the
    // first one has already sealed a database with: the link fails, and it reads
    // that key instead of minting a rival.
    let key = MasterKey::random();
    let mut unique = [0u8; 8];
    crate::crypto::random_bytes(&mut unique);
    let tmp = path.with_extension(format!("tmp.{}", hex::encode(unique)));
    std::fs::write(&tmp, key.0).map_err(io)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600)).map_err(io)?;
    }
    let linked = std::fs::hard_link(&tmp, path);
    let _ = std::fs::remove_file(&tmp);
    match linked {
        Ok(()) => Ok(key),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            MasterKey::from_slice(&std::fs::read(path).map_err(io)?)
        }
        Err(e) => Err(io(e)),
    }
}

// ---------------------------------------------------------------------------
// Moving between stores
// ---------------------------------------------------------------------------

/// Copy every note from one store into another, and the keys that go with them.
///
/// A device changes store when the app does — the initial implementation's files to the second milestone's
/// database, and the browser's memory to IndexedDB after that. What it must not
/// do is lose the notes that were there, so the move is a copy the core can make
/// through the same trait both sides implement, and neither store needs to know
/// about the other.
///
/// `meta` names the local rows worth carrying; the only one today is the sync
/// cursor ([`crate::note::SYNC_CURSOR`]). Leaving it behind would be correct but
/// expensive: the device would re-send its whole log to the relay once, which
/// the relay deduplicates into nothing while storing a second copy of it.
///
/// The source is left exactly as it was. A migration that deletes the only copy
/// of the data before anyone has opened the new one is not a migration.
pub fn copy_store(
    from: &dyn OpStore,
    from_keys: &dyn Keyring,
    to: &mut dyn OpStore,
    to_keys: &mut dyn Keyring,
    meta: &[&str],
) -> Result<usize> {
    if let Some(seed) = from_keys.device_seed()? {
        // The identity first: a half-copied device that mints a new one would be
        // a different author to every peer, and its own log would look forged.
        to_keys.put_device_seed(&seed)?;
    }
    if let Some(root) = from_keys.account_root()? {
        // For the same reason, one level up: a device that moved store and
        // forgot which account it was would keep its notes and lose its other
        // devices, which is the harder half to notice.
        to_keys.put_account_root(&root)?;
    }

    let notes = from.notes()?;
    for note in &notes {
        if let Some(key) = from_keys.note_key(*note)? {
            to_keys.put_note_key(*note, &key)?;
        }
        if let Some(snapshot) = from.get_snapshot(*note)? {
            to.put_snapshot(*note, &snapshot)?;
        }
        for envelope in from.load(*note)? {
            to.append(*note, &envelope)?;
        }
        for key in meta {
            if let Some(value) = from.get_meta(*note, key)? {
                to.put_meta(*note, key, &value)?;
            }
        }
    }
    Ok(notes.len())
}

// ---------------------------------------------------------------------------
// Boxed stores
// ---------------------------------------------------------------------------

// So a caller can decide at runtime which store to use — the Flutter bridge picks
// the filesystem on a desktop or phone and memory in a browser, from the same
// `NoteCore` type.

impl OpStore for Box<dyn OpStore + Send> {
    fn append(&mut self, note: NoteId, envelope: &[u8]) -> Result<()> {
        (**self).append(note, envelope)
    }
    fn load(&self, note: NoteId) -> Result<Vec<Vec<u8>>> {
        (**self).load(note)
    }
    fn put_snapshot(&mut self, note: NoteId, snapshot: &[u8]) -> Result<()> {
        (**self).put_snapshot(note, snapshot)
    }
    fn get_snapshot(&self, note: NoteId) -> Result<Option<Vec<u8>>> {
        (**self).get_snapshot(note)
    }
    fn replace_log(&mut self, note: NoteId, envelopes: &[Vec<u8>]) -> Result<()> {
        (**self).replace_log(note, envelopes)
    }
    fn log_len(&self, note: NoteId) -> Result<usize> {
        (**self).log_len(note)
    }
    fn notes(&self) -> Result<Vec<NoteId>> {
        (**self).notes()
    }
    fn put_meta(&mut self, note: NoteId, key: &str, value: &[u8]) -> Result<()> {
        (**self).put_meta(note, key, value)
    }
    fn get_meta(&self, note: NoteId, key: &str) -> Result<Option<Vec<u8>>> {
        (**self).get_meta(note, key)
    }
    fn forget(&mut self, note: NoteId) -> Result<()> {
        (**self).forget(note)
    }
}

impl Keyring for Box<dyn Keyring + Send> {
    fn device_seed(&self) -> Result<Option<crate::crypto::DeviceSeed>> {
        (**self).device_seed()
    }
    fn put_device_seed(&mut self, seed: &crate::crypto::DeviceSeed) -> Result<()> {
        (**self).put_device_seed(seed)
    }
    fn account_root(&self) -> Result<Option<crate::account::AccountRoot>> {
        (**self).account_root()
    }
    fn put_account_root(&mut self, root: &crate::account::AccountRoot) -> Result<()> {
        (**self).put_account_root(root)
    }
    fn note_key(&self, note: NoteId) -> Result<Option<crate::crypto::NoteKey>> {
        (**self).note_key(note)
    }
    fn put_note_key(&mut self, note: NoteId, key: &crate::crypto::NoteKey) -> Result<()> {
        (**self).put_note_key(note, key)
    }
    fn forget_note_key(&mut self, note: NoteId) -> Result<()> {
        (**self).forget_note_key(note)
    }
    fn forget_everything(&mut self) -> Result<()> {
        (**self).forget_everything()
    }
}
