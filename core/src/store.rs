//! Where the encrypted op log lives.
//!
//! Slice 1 persists the *encrypted* log plus an encrypted snapshot and keeps the
//! decrypted CRDT in memory only. That sidesteps SQLCipher and OPFS key management
//! for now without ever writing plaintext to disk. The real local store, encrypted
//! at rest with the master key, is Slice 2.
//!
//! The trait is here so the web build can back it with IndexedDB without anything
//! above it changing.

use crate::error::Result;
use crate::ids::NoteId;
use std::collections::HashMap;

pub trait OpStore {
    /// Append one encoded envelope to a note's log.
    fn append(&mut self, note: NoteId, envelope: &[u8]) -> Result<()>;

    /// Every envelope for a note, oldest first.
    fn load(&self, note: NoteId) -> Result<Vec<Vec<u8>>>;

    /// Replace a note's log with a single encrypted snapshot blob. Log compaction
    /// (Slice 2) will use this; Slice 1 only reads it back.
    fn put_snapshot(&mut self, note: NoteId, snapshot: &[u8]) -> Result<()>;

    fn get_snapshot(&self, note: NoteId) -> Result<Option<Vec<u8>>>;

    fn notes(&self) -> Result<Vec<NoteId>>;
}

/// In-memory store, for tests and for the headless cores in the integration test.
#[derive(Default)]
pub struct MemoryStore {
    logs: HashMap<NoteId, Vec<Vec<u8>>>,
    snapshots: HashMap<NoteId, Vec<u8>>,
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

    fn get_snapshot(&self, note: NoteId) -> Result<Option<Vec<u8>>> {
        Ok(self.snapshots.get(&note).cloned())
    }

    fn notes(&self) -> Result<Vec<NoteId>> {
        let mut ids: Vec<NoteId> = self.logs.keys().copied().collect();
        for id in self.snapshots.keys() {
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
            // Slice 1 does not fsync per append; a crash may lose the last edit.
            // The real store (Slice 2) decides the durability policy.
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

        fn get_snapshot(&self, note: NoteId) -> Result<Option<Vec<u8>>> {
            let path = self.snapshot_path(note);
            if !path.exists() {
                return Ok(None);
            }
            Ok(Some(fs::read(&path).map_err(io)?))
        }

        fn notes(&self) -> Result<Vec<NoteId>> {
            let mut ids = Vec::new();
            for entry in fs::read_dir(&self.root).map_err(io)? {
                let entry = entry.map_err(io)?;
                let name = entry.file_name().to_string_lossy().into_owned();
                if let Some(stem) = name.strip_suffix(".log") {
                    if let Ok(id) = NoteId::decode(stem) {
                        ids.push(id);
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

// ---------------------------------------------------------------------------
// Keyring
// ---------------------------------------------------------------------------

/// Where note keys and the device seed live.
///
/// **Slice 1 stand-in.** In the shipping app this is the OS keychain (Keystore,
/// Keychain, or a passphrase-wrapped blob on web and desktop) and never a file
/// next to the op log. It is a separate trait from [`OpStore`] precisely so that
/// "the log holds only ciphertext" stays true: key material never goes near it.
pub trait Keyring {
    fn device_seed(&self) -> Result<Option<crate::crypto::DeviceSeed>>;
    fn put_device_seed(&mut self, seed: &crate::crypto::DeviceSeed) -> Result<()>;
    fn note_key(&self, note: NoteId) -> Result<Option<crate::crypto::NoteKey>>;
    fn put_note_key(&mut self, note: NoteId, key: &crate::crypto::NoteKey) -> Result<()>;
}

#[derive(Default)]
pub struct MemoryKeyring {
    seed: Option<crate::crypto::DeviceSeed>,
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

    fn note_key(&self, note: NoteId) -> Result<Option<crate::crypto::NoteKey>> {
        Ok(self.keys.get(&note).cloned())
    }

    fn put_note_key(&mut self, note: NoteId, key: &crate::crypto::NoteKey) -> Result<()> {
        self.keys.insert(note, key.clone());
        Ok(())
    }
}

/// File-backed keyring. Slice 1 only; see the note on [`Keyring`].
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
    }
}

#[cfg(feature = "file-store")]
pub use file_keyring::FileKeyring;

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
    fn notes(&self) -> Result<Vec<NoteId>> {
        (**self).notes()
    }
}

impl Keyring for Box<dyn Keyring + Send> {
    fn device_seed(&self) -> Result<Option<crate::crypto::DeviceSeed>> {
        (**self).device_seed()
    }
    fn put_device_seed(&mut self, seed: &crate::crypto::DeviceSeed) -> Result<()> {
        (**self).put_device_seed(seed)
    }
    fn note_key(&self, note: NoteId) -> Result<Option<crate::crypto::NoteKey>> {
        (**self).note_key(note)
    }
    fn put_note_key(&mut self, note: NoteId, key: &crate::crypto::NoteKey) -> Result<()> {
        (**self).put_note_key(note, key)
    }
}
