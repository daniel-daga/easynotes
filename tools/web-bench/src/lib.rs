//! What opening a device costs *in a browser*, which is the one number
//! `just bench-library` cannot produce.
//!
//! [ADR 0011](../../../docs/adr/0011-the-web-store.md) has a tab read its whole
//! database at open and keep it in memory, because `OpStore` is synchronous and
//! nothing a browser offers is. That is a bet about size: it holds if reading
//! and unsealing everything is cheap at the sizes people have, and it stops
//! holding at whatever size it is not. The native benchmark measures SQLite on a
//! disk, which answers a different question — a browser reads out of IndexedDB
//! and unseals in WebAssembly, and neither is the same machine code or the same
//! I/O.
//!
//! So this is the same device, built the same way, measured in a tab. Three
//! numbers per size, because a launch is three things and only the middle one is
//! shared with a phone:
//!
//! 1. **Reading the database**, which is IndexedDB and JavaScript and no Rust at
//!    all — [`tools/web-bench/bench.mjs`](./bench.mjs) times it around
//!    `getAll`, exactly as `app/lib/core/indexed_db.dart` does it.
//! 2. **Opening the device**: [`WebStore::from_rows`] over those rows, then
//!    [`NoteCore::open`], which replays the account document before a note is
//!    read.
//! 3. **Listing the notes**: [`NoteCore::notes`], which unseals one index entry
//!    per note.
//!
//! Every one of them is timed by the driver rather than here, so the boundary
//! crossing is inside the number. That is the honest place for it: in the app
//! those rows cross from Dart to Rust too, and a measurement that excused itself
//! from the crossing would be measuring something nobody runs.
//!
//! What it does *not* include is Flutter: no widget is built and no frame is
//! drawn. The question is what the store and the crypto cost a tab, and putting
//! a rendering engine in front of them would answer a vaguer one.

use easynotes_core::crdt::{Colour, DefaultNoteDoc, Edit};
use easynotes_core::crypto::MasterKey;
use easynotes_core::ids::NoteId;
use easynotes_core::note::{NoteCore, NoteKind};
use easynotes_core::store::web::{Change, ChangeSink, WebKeyring, WebStore};
use js_sys::{Array, Map, Uint8Array};
use std::cell::RefCell;
use std::rc::Rc;
use wasm_bindgen::prelude::*;

/// The same shape `core/examples/bench_library.rs` fills a device with, and the
/// numbers are only comparable while it stays the same shape. Twenty items a
/// note and thirty-two edits on them: a shopping list that has been ticked off a
/// few times.
const ITEMS: usize = 20;
const EDITS: usize = 32;

type Store = NoteCore<DefaultNoteDoc, WebStore, WebKeyring>;

/// Collects what the store would have written, so the driver can put it in a
/// real IndexedDB.
///
/// A browser's own sink hands each batch to Dart and returns; this one keeps
/// them, because the rows are the point — they are what a second launch would
/// have to read.
#[derive(Default)]
struct Collect(Rc<RefCell<Vec<Change>>>);

impl ChangeSink for Collect {
    fn changed(&self, changes: &[Change]) {
        self.0.borrow_mut().extend_from_slice(changes);
    }
}

// `ChangeSink: Send` and an `Rc` is not, but a wasm32 build is single-threaded
// and this type never leaves it. The alternative is an `Arc<Mutex<_>>` paying
// for a lock that cannot be contended.
unsafe impl Send for Collect {}

/// The rows a database would be holding: what a browser's object store looks
/// like once every batch it was handed has been applied.
#[derive(Default)]
struct Rows(Vec<(String, Vec<u8>)>);

impl Rows {
    fn apply(&mut self, changes: &[Change]) {
        for change in changes {
            match change {
                Change::Put { key, value } => match self.0.iter_mut().find(|(k, _)| k == key) {
                    Some(row) => row.1 = value.clone(),
                    None => self.0.push((key.clone(), value.clone())),
                },
                Change::Delete { key } => self.0.retain(|(k, _)| k != key),
                Change::DeletePrefix { prefix } => self.0.retain(|(k, _)| !k.starts_with(prefix)),
            }
        }
    }
}

fn to_js(held: &[(String, Vec<u8>)]) -> Map {
    let map = Map::new();
    for (key, value) in held {
        map.set(&JsValue::from_str(key), &Uint8Array::from(value.as_slice()));
    }
    map
}

fn from_js(map: &Map) -> Vec<(String, Vec<u8>)> {
    let mut held = Vec::new();
    let entries = Array::from(&JsValue::from(map.entries()));
    for entry in entries.iter() {
        let pair = Array::from(&entry);
        let key = pair.get(0).as_string().unwrap_or_default();
        held.push((key, Uint8Array::new(&pair.get(1)).to_vec()));
    }
    held
}

/// A device with `notes` notes on it, and the rows a database would be holding
/// for it.
///
/// Returns `{ master, notes, keys }` — the master key as bytes, and a `Map` per
/// object store, which is the two the app creates.
///
/// The device is opened a second time before its rows are handed over, and that
/// is not tidiness. The account document is compacted on the first launch after
/// its log passes [`COMPACT_AFTER`](easynotes_core::note::COMPACT_AFTER), which
/// at two hundred notes is this one — so a database handed over before it would
/// make every measured launch the one unrepeatable launch that pays for a
/// compaction, and would report that as what opening this device costs. What
/// comes back is the steady state, which is the column
/// [ADR 0012](../../../docs/adr/0012-compacting-the-account-document.md) reports
/// for a device with a disk.
#[wasm_bindgen]
pub fn seed(notes: u32) -> Result<JsValue, JsValue> {
    let master = MasterKey::random();
    let mut note_rows = Rows::default();
    let mut key_rows = Rows::default();

    {
        let changes = (
            Rc::new(RefCell::new(Vec::new())),
            Rc::new(RefCell::new(Vec::new())),
        );
        let mut core: Store = NoteCore::open(
            WebStore::new(master.clone(), Box::new(Collect(changes.0.clone()))),
            WebKeyring::new(master.clone(), Box::new(Collect(changes.1.clone()))),
        )
        .map_err(fail)?;
        fill(&mut core, notes as usize).map_err(fail)?;
        drop(core);
        note_rows.apply(&changes.0.borrow());
        key_rows.apply(&changes.1.borrow());
    }

    // The settling launch. It lists, because a launch lists, and because a note
    // that has never been summarised would leave an index row to be written on
    // the launch being measured rather than before it.
    {
        let changes = (
            Rc::new(RefCell::new(Vec::new())),
            Rc::new(RefCell::new(Vec::new())),
        );
        let mut core: Store = NoteCore::open(
            WebStore::from_rows(
                master.clone(),
                Box::new(Collect(changes.0.clone())),
                note_rows.0.clone(),
            )
            .map_err(fail)?,
            WebKeyring::from_rows(
                master.clone(),
                Box::new(Collect(changes.1.clone())),
                key_rows.0.clone(),
            )
            .map_err(fail)?,
        )
        .map_err(fail)?;
        core.notes().map_err(fail)?;
        drop(core);
        note_rows.apply(&changes.0.borrow());
        key_rows.apply(&changes.1.borrow());
    }

    let out = Map::new();
    out.set(
        &JsValue::from_str("master"),
        &Uint8Array::from(master.0.as_slice()),
    );
    out.set(&JsValue::from_str("notes"), &to_js(&note_rows.0));
    out.set(&JsValue::from_str("keys"), &to_js(&key_rows.0));
    Ok(out.into())
}

/// Open a device from the rows a browser read back, and hold it for [`list`].
///
/// This is `Library::open_in_browser` with the bridge taken out: the same
/// `from_rows` and the same `NoteCore::open`, which replays the account document
/// before any note is looked at.
#[wasm_bindgen]
pub fn open(master: &[u8], notes: &Map, keys: &Map) -> Result<(), JsValue> {
    let master = MasterKey::from_slice(master).map_err(fail)?;
    let core: Store = NoteCore::open(
        WebStore::from_rows(master.clone(), Box::new(Collect::default()), from_js(notes))
            .map_err(fail)?,
        WebKeyring::from_rows(master, Box::new(Collect::default()), from_js(keys)).map_err(fail)?,
    )
    .map_err(fail)?;
    OPEN.with(|held| *held.borrow_mut() = Some(core));
    Ok(())
}

/// List what the open device holds, and say how many that was.
///
/// The number is returned so the driver can check it: a launch that unsealed
/// nothing would be very fast and would mean nothing.
#[wasm_bindgen]
pub fn list() -> Result<u32, JsValue> {
    OPEN.with(|held| {
        let mut held = held.borrow_mut();
        let core = held
            .as_mut()
            .ok_or_else(|| JsValue::from_str("no device"))?;
        Ok(core.notes().map_err(fail)?.len() as u32)
    })
}

/// Drop the open device, so the next size starts from nothing.
#[wasm_bindgen]
pub fn close() {
    OPEN.with(|held| *held.borrow_mut() = None);
}

thread_local! {
    /// The device between [`open`] and [`list`], because the two are timed apart
    /// and a launch does them in that order.
    static OPEN: RefCell<Option<Store>> = const { RefCell::new(None) };
}

fn fail(e: impl std::fmt::Display) -> JsValue {
    JsValue::from_str(&e.to_string())
}

/// The same filling as `core/examples/bench_library.rs`, kept in step with it by
/// hand: there is no library both a `cargo` example and a WebAssembly bundle can
/// share without putting a benchmark's fixtures in the core.
fn fill(core: &mut Store, notes: usize) -> easynotes_core::error::Result<()> {
    let mut made: Vec<NoteId> = Vec::with_capacity(notes);
    for n in 0..notes {
        let id = core.create_note(NoteKind::Checklist)?;
        core.set_title(id, format!("List {n}"))?;
        let mut items = Vec::with_capacity(ITEMS);
        for i in 0..ITEMS {
            items.push(core.add_item(id, format!("item number {i}"))?.0);
        }
        for i in 0..EDITS {
            core.apply_local(
                id,
                Edit::SetChecked {
                    id: items[i % items.len()],
                    checked: i % 2 == 0,
                },
            )?;
        }
        made.push(id);
    }
    // A placement for every note, because creating one writes where it goes,
    // plus the colours and pins somebody has actually chosen — a third and a
    // seventh, which is the other benchmark's guess and has to stay its guess.
    for (n, id) in made.iter().enumerate() {
        if n % 3 == 0 {
            core.set_colour(*id, Some(Colour::ALL[n % Colour::ALL.len()]))?;
        }
        if n % 7 == 0 {
            core.set_pinned(*id, true)?;
        }
    }
    Ok(())
}
