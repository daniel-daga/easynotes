//! Yrs (Rust Yjs) backend.
//!
//! Same document shape as the Automerge backend — a root map holding `"title"`
//! plus one nested map per item ID — so the two are compared on the same workload
//! and not on two different data models. See `automerge_doc` for why items hang
//! off the root rather than off a nested container.

use super::{sort_items, Edit, ItemView, NoteDoc, NoteView};
use crate::error::{CoreError, Result};
use crate::ids::ItemId;
use std::sync::{Arc, Mutex};
use yrs::updates::decoder::Decode;
use yrs::{Any, Doc, Map, MapPrelim, MapRef, Out, ReadTxn, StateVector, Transact, Update};

const ROOT: &str = "note";
const TITLE: &str = "title";
const TEXT: &str = "text";
const CHECKED: &str = "checked";
const ORDER: &str = "order";

pub struct YrsNote {
    doc: Doc,
    root: MapRef,
    /// The update produced by the most recent transaction, captured by the
    /// observer below.
    ///
    /// The obvious alternative — snapshot the state vector, edit, then call
    /// `encode_diff_v1` — is wrong for an incremental update: that encoding
    /// carries the document's *entire* delete set, which grows with every
    /// overwritten map value. Measured on the D1 workload it turned a 35-byte
    /// toggle into a 3 KB one. The observer emits only the transaction's own ops.
    /// `Arc<Mutex<_>>` rather than `Rc<RefCell<_>>`, and a *keyed* subscription
    /// rather than a `Subscription` handle, so that a `YrsNote` is `Send`.
    /// `yrs::Subscription` wraps a `Box<dyn Drop>` that is neither `Send` nor
    /// `Sync`, so holding one would pin the document to a single thread — which
    /// flutter_rust_bridge, running Rust on a worker pool, will not accept.
    pending: Arc<Mutex<Option<Vec<u8>>>>,
}

/// Yrs identifies a replica by a client ID that must fit in **53 bits** — a
/// constraint inherited from Yjs, where the ID has to survive a round trip
/// through a JavaScript number. Automerge takes an arbitrary-length actor ID,
/// so this is a real difference between the two backends and is recorded in
/// ADR 0001.
///
/// 53 random bits puts a 1% collision chance at roughly 13 million devices
/// sharing a single note, which no household will reach. Yrs documents that a
/// collision corrupts the store, so if a future slice needs stronger identity
/// it has to allocate client IDs rather than hash them.
///
/// Shared with the account document, which is the same replica on the same
/// device and has no reason to call itself anything else.
pub(crate) fn client_id(actor: &[u8; 16]) -> u64 {
    const FIFTY_THREE_BITS: u64 = (1 << 53) - 1;
    u64::from_be_bytes(actor[..8].try_into().unwrap()) & FIFTY_THREE_BITS
}

impl YrsNote {
    /// A root type is addressed by name, so every replica resolves `ROOT` to the
    /// same object without anyone having to create it in a bootstrap transaction.
    fn open(doc: Doc) -> Self {
        let root = doc.get_or_insert_map(ROOT);
        let pending: Arc<Mutex<Option<Vec<u8>>>> = Arc::new(Mutex::new(None));
        let sink = pending.clone();
        doc.observe_update_v1_with("easynotes", move |_txn, event| {
            *sink.lock().expect("the update sink is never poisoned") = Some(event.update.clone());
        })
        .expect("a freshly created document has no open transaction");
        Self { doc, root, pending }
    }

    fn take_pending(&self) -> Option<Vec<u8>> {
        self.pending
            .lock()
            .expect("the update sink is never poisoned")
            .take()
    }

    fn item(&self, txn: &impl ReadTxn, id: ItemId) -> Option<MapRef> {
        match self.root.get(txn, &id.encode()) {
            Some(Out::YMap(m)) => Some(m),
            _ => None,
        }
    }
}

fn any_str(v: Option<Out>) -> String {
    match v {
        Some(Out::Any(Any::String(s))) => s.to_string(),
        _ => String::new(),
    }
}

fn any_bool(v: Option<Out>) -> bool {
    matches!(v, Some(Out::Any(Any::Bool(true))))
}

impl NoteDoc for YrsNote {
    type Account = super::YrsAccount;

    fn create(actor: &[u8; 16]) -> Self {
        Self::open(Doc::with_client_id(client_id(actor)))
    }

    fn load(actor: &[u8; 16], snapshot: &[u8]) -> Result<Self> {
        let doc = Doc::with_client_id(client_id(actor));
        let update = Update::decode_v1(snapshot)
            .map_err(|e| CoreError::Crdt(format!("decoding snapshot: {e}")))?;
        doc.transact_mut()
            .apply_update(update)
            .map_err(|e| CoreError::Crdt(format!("applying snapshot: {e}")))?;
        Ok(Self::open(doc))
    }

    fn apply_local(&mut self, edit: &Edit) -> Result<Vec<u8>> {
        self.take_pending();
        {
            let mut txn = self.doc.transact_mut();
            let items = &self.root;
            match edit {
                Edit::SetTitle(t) => {
                    self.root.insert(&mut txn, TITLE, t.as_str());
                }
                Edit::AddItem { id, text, order } => {
                    let obj = items.insert(&mut txn, id.encode(), MapPrelim::default());
                    obj.insert(&mut txn, TEXT, text.as_str());
                    obj.insert(&mut txn, CHECKED, false);
                    obj.insert(&mut txn, ORDER, order.as_str());
                }
                Edit::SetItemText { id, text } => {
                    if let Some(Out::YMap(obj)) = items.get(&txn, &id.encode()) {
                        obj.insert(&mut txn, TEXT, text.as_str());
                    }
                }
                Edit::SetChecked { id, checked } => {
                    if let Some(Out::YMap(obj)) = items.get(&txn, &id.encode()) {
                        obj.insert(&mut txn, CHECKED, *checked);
                    }
                }
                Edit::MoveItem { id, order } => {
                    if let Some(Out::YMap(obj)) = items.get(&txn, &id.encode()) {
                        obj.insert(&mut txn, ORDER, order.as_str());
                    }
                }
                Edit::RemoveItem { id } => {
                    items.remove(&mut txn, &id.encode());
                }
            }
        }
        self.take_pending()
            .ok_or_else(|| CoreError::Crdt("the edit produced no update".into()))
    }

    fn apply_remote(&mut self, update: &[u8]) -> Result<()> {
        // Yrs deduplicates by (client, clock), so re-applying an update is a no-op.
        // Clear the observer's buffer so a remote update is never mistaken for a
        // local one and echoed back to the relay.
        self.take_pending();
        let update = Update::decode_v1(update)
            .map_err(|e| CoreError::Crdt(format!("decoding update: {e}")))?;
        self.doc
            .transact_mut()
            .apply_update(update)
            .map_err(|e| CoreError::Crdt(format!("applying update: {e}")))?;
        self.take_pending();
        Ok(())
    }

    fn view(&self) -> NoteView {
        let txn = self.doc.transact();
        let title = any_str(self.root.get(&txn, TITLE));
        let mut items = Vec::new();
        {
            let items_map = &self.root;
            for (key, _) in items_map.iter(&txn) {
                if key == TITLE {
                    continue;
                }
                let Ok(id) = ItemId::decode(key) else {
                    continue;
                };
                let Some(obj) = self.item(&txn, id) else {
                    continue;
                };
                items.push(ItemView {
                    id,
                    text: any_str(obj.get(&txn, TEXT)),
                    checked: any_bool(obj.get(&txn, CHECKED)),
                    order: any_str(obj.get(&txn, ORDER)),
                });
            }
        }
        sort_items(&mut items);
        NoteView { title, items }
    }

    fn snapshot(&self) -> Vec<u8> {
        self.doc
            .transact()
            .encode_state_as_update_v1(&StateVector::default())
    }

    fn backend_name() -> &'static str {
        "yrs"
    }
}
