//! [`MediaRows`] over the browser's own IndexedDB: three operations and no
//! rules. The rules are in [`super::web`], where `cargo test` reaches them.
//!
//! A database of its own, apart from the one `indexed_db.dart` keeps the notes
//! in, for two reasons. That one is read whole at startup, which a photo must
//! never be; and it is opened from Dart, while this is opened from whichever
//! thread the Rust asking for it is on.
//!
//! **One connection per thread.** An `IDBDatabase` belongs to the thread that
//! opened it, and flutter_rust_bridge runs some calls on the page and others on
//! workers (`app/rust/src/lock.rs`), while the transfer queue runs on a thread
//! of its own. So an [`IdbRows`] is only a name, which any thread may hold, and
//! each thread opens its own connection on first use and keeps it. IndexedDB
//! orders transactions across connections to the same database exactly as it
//! does within one, so a chunk written on one thread is there for a read on
//! another once the write's transaction has committed.
//!
//! Every request and transaction is awaited through a `Promise` whose own
//! `resolve` and `reject` are the handlers, so no Rust closure is left
//! registered with the browser when a future is dropped half way.

use super::storage::Fut;
use super::web::MediaRows;
use crate::error::{CoreError, Result};
use crate::store::web::Change;
use std::cell::RefCell;
use std::collections::HashMap;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    IdbDatabase, IdbFactory, IdbKeyRange, IdbRequest, IdbTransaction, IdbTransactionMode,
};

/// The object store.
const STORE: &str = "media";

/// Each step takes the database one IndexedDB version on: `STEPS[0]` creates
/// version 1 from nothing, `STEPS[n]` takes `n` to `n + 1` ([ADR 0029](../../../docs/decisions.md#adr-0029)). They run
/// in the browser's own upgrade transaction, so a step that throws aborts the
/// whole upgrade and the database stays at the version it was.
///
/// Never edit a step a release has shipped; add one at the end.
type Step = fn(&IdbDatabase) -> std::result::Result<(), JsValue>;

const STEPS: &[Step] = &[|db| db.create_object_store(STORE).map(|_| ())];

/// The version this build opens, which is the number of steps.
const VERSION: u32 = STEPS.len() as u32;

thread_local! {
    static OPEN: RefCell<HashMap<String, IdbDatabase>> = RefCell::new(HashMap::new());
}

fn js(e: JsValue) -> CoreError {
    let message = e
        .dyn_ref::<web_sys::DomException>()
        .map(|d| format!("{}: {}", d.name(), d.message()))
        .or_else(|| e.as_string())
        .unwrap_or_else(|| "IndexedDB refused the request".into());
    CoreError::Storage(format!("browser storage: {message}"))
}

/// A tab's attachment rows, in the IndexedDB database called `name`.
pub struct IdbRows {
    name: String,
}

impl IdbRows {
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into() }
    }

    /// Delete the whole database, as a wipe does. Every connection this thread
    /// holds to it is closed first, or the deletion would wait for them.
    pub async fn delete(name: &str) -> Result<()> {
        if let Some(db) = OPEN.with(|open| open.borrow_mut().remove(name)) {
            db.close();
        }
        let request = factory()?.delete_database(name).map_err(js)?;
        wait(&request).await.map(|_| ())
    }
}

fn factory() -> Result<IdbFactory> {
    let global = js_sys::global();
    let factory = if let Some(window) = global.dyn_ref::<web_sys::Window>() {
        window.indexed_db()
    } else if let Some(worker) = global.dyn_ref::<web_sys::WorkerGlobalScope>() {
        worker.indexed_db()
    } else {
        Ok(None)
    };
    factory
        .map_err(js)?
        .ok_or_else(|| CoreError::Storage("this browser has no IndexedDB".into()))
}

/// Resolve when `request` succeeds, with its result; reject when it fails.
async fn wait(request: &IdbRequest) -> Result<JsValue> {
    let promise = js_sys::Promise::new(&mut |resolve, reject| {
        request.set_onsuccess(Some(&resolve));
        request.set_onerror(Some(&reject));
    });
    let settled = JsFuture::from(promise).await;
    request.set_onsuccess(None);
    request.set_onerror(None);
    match settled {
        Ok(_) => request.result().map_err(js),
        Err(_) => Err(match request.error() {
            Ok(Some(e)) => js(e.into()),
            _ => CoreError::Storage("browser storage: a request failed".into()),
        }),
    }
}

/// Resolve when `tx` has committed; reject if it aborts for any reason.
async fn committed(tx: &IdbTransaction) -> Result<()> {
    let promise = js_sys::Promise::new(&mut |resolve, reject| {
        tx.set_oncomplete(Some(&resolve));
        tx.set_onerror(Some(&reject));
        tx.set_onabort(Some(&reject));
    });
    let settled = JsFuture::from(promise).await;
    tx.set_oncomplete(None);
    tx.set_onerror(None);
    tx.set_onabort(None);
    match settled {
        Ok(_) => Ok(()),
        Err(_) => Err(match tx.error() {
            Some(e) => js(e.into()),
            None => CoreError::Storage("browser storage: a write was abandoned".into()),
        }),
    }
}

/// This thread's connection to `name`, opened on first use.
async fn database(name: &str) -> Result<IdbDatabase> {
    if let Some(db) = OPEN.with(|open| open.borrow().get(name).cloned()) {
        return Ok(db);
    }
    let request = factory()?.open_with_u32(name, VERSION).map_err(js)?;
    // Runs when the database is new or older than this build: every step it
    // has not had. A database from a newer build fails to open with a
    // `VersionError` instead, which is the refusal a downgrade gets.
    // `once_into_js` frees the closure when it is called; one that never is
    // costs a few bytes per connection.
    let upgrade = Closure::once_into_js({
        let request = request.clone();
        move |event: web_sys::IdbVersionChangeEvent| {
            let Ok(db) = request.result() else { return };
            let db = db.unchecked_into::<IdbDatabase>();
            let from = event.old_version() as usize;
            for step in STEPS.iter().skip(from) {
                if step(&db).is_err() {
                    // Aborting the upgrade transaction rolls back every step
                    // and fails the open, rather than leaving half of one.
                    if let Some(upgrade) = request.transaction() {
                        let _ = upgrade.abort();
                    }
                    return;
                }
            }
        }
    });
    request.set_onupgradeneeded(Some(upgrade.unchecked_ref()));
    let opened = wait(&request).await;
    request.set_onupgradeneeded(None);
    let db: IdbDatabase = opened?.unchecked_into();

    // Another tab deleting or upgrading the database asks this connection to
    // go. Closing it lets that happen; the next call here opens a new one.
    let on_version_change = Closure::<dyn FnMut(web_sys::Event)>::new({
        let (name, db) = (name.to_owned(), db.clone());
        move |_: web_sys::Event| {
            OPEN.with(|open| {
                let mut open = open.borrow_mut();
                if open.get(&name).is_some_and(|held| *held == db) {
                    open.remove(&name);
                }
            });
            db.close();
        }
    });
    db.set_onversionchange(Some(on_version_change.as_ref().unchecked_ref()));
    // As long as the connection lives, which is as long as the thread does.
    on_version_change.forget();

    // Two calls on one thread can race to open; the first to finish is kept.
    Ok(OPEN.with(|open| {
        let mut open = open.borrow_mut();
        if let Some(held) = open.get(name) {
            db.close();
            held.clone()
        } else {
            open.insert(name.to_owned(), db.clone());
            db
        }
    }))
}

/// A transaction on the store, reopening the connection once if the browser
/// closed it underneath this thread.
async fn transaction(name: &str, mode: IdbTransactionMode) -> Result<IdbTransaction> {
    let db = database(name).await?;
    match db.transaction_with_str_and_mode(STORE, mode) {
        Ok(tx) => Ok(tx),
        Err(_) => {
            OPEN.with(|open| open.borrow_mut().remove(name));
            database(name)
                .await?
                .transaction_with_str_and_mode(STORE, mode)
                .map_err(js)
        }
    }
}

fn prefix_range(prefix: &str) -> Result<IdbKeyRange> {
    // Every key here is ASCII, so nothing that begins with `prefix` sorts
    // after `prefix` followed by the highest code unit.
    IdbKeyRange::bound(
        &JsValue::from_str(prefix),
        &JsValue::from_str(&format!("{prefix}\u{FFFF}")),
    )
    .map_err(js)
}

impl IdbRows {
    async fn get_row(&self, key: &str) -> Result<Option<Vec<u8>>> {
        let tx = transaction(&self.name, IdbTransactionMode::Readonly).await?;
        let store = tx.object_store(STORE).map_err(js)?;
        let value = wait(&store.get(&JsValue::from_str(key)).map_err(js)?).await?;
        if value.is_undefined() {
            return Ok(None);
        }
        Ok(Some(js_sys::Uint8Array::new(&value).to_vec()))
    }

    async fn keys_with(&self, prefix: &str) -> Result<Vec<String>> {
        let tx = transaction(&self.name, IdbTransactionMode::Readonly).await?;
        let store = tx.object_store(STORE).map_err(js)?;
        let request = store
            .get_all_keys_with_key(&prefix_range(prefix)?.into())
            .map_err(js)?;
        let keys: js_sys::Array = wait(&request).await?.unchecked_into();
        Ok(keys.iter().filter_map(|k| k.as_string()).collect())
    }

    async fn apply_all(&self, changes: Vec<Change>) -> Result<()> {
        let tx = transaction(&self.name, IdbTransactionMode::Readwrite).await?;
        let queued = (|| {
            let store = tx.object_store(STORE).map_err(js)?;
            for change in &changes {
                match change {
                    Change::Put { key, value } => {
                        store
                            .put_with_key(
                                &js_sys::Uint8Array::from(value.as_slice()),
                                &JsValue::from_str(key),
                            )
                            .map_err(js)?;
                    }
                    Change::Delete { key } => {
                        store.delete(&JsValue::from_str(key)).map_err(js)?;
                    }
                    Change::DeletePrefix { prefix } => {
                        store.delete(&prefix_range(prefix)?.into()).map_err(js)?;
                    }
                }
            }
            Ok(())
        })();
        if let Err(e) = queued {
            // None of it, rather than the part that was queued.
            let _ = tx.abort();
            return Err(e);
        }
        committed(&tx).await
    }
}

impl MediaRows for IdbRows {
    fn get<'a>(&'a self, key: &'a str) -> Fut<'a, Result<Option<Vec<u8>>>> {
        Box::pin(self.get_row(key))
    }

    fn keys<'a>(&'a self, prefix: &'a str) -> Fut<'a, Result<Vec<String>>> {
        Box::pin(self.keys_with(prefix))
    }

    fn apply(&self, changes: Vec<Change>) -> Fut<'_, Result<()>> {
        Box::pin(self.apply_all(changes))
    }
}
