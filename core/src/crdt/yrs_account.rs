//! The account document on Yrs.
//!
//! A single root map of scalars, for the reason [`super::account`] gives: no
//! containers, so two of this person's devices can never create rival ones.
//! Everything that decides what a key means is in that module; this file only
//! puts keys in a document and reads them back.

use super::account::{AccountDoc, Value};
use super::yrs_doc::client_id;
use crate::error::{CoreError, Result};
use std::sync::{Arc, Mutex};
use yrs::updates::decoder::Decode;
use yrs::{Any, Doc, Map, MapRef, Out, ReadTxn, StateVector, Transact, Update};

/// A different root name from a note's, so that a document opened as the wrong
/// kind reads as empty instead of as nonsense.
const ROOT: &str = "account";

pub struct YrsAccount {
    doc: Doc,
    root: MapRef,
    /// The update the last transaction produced. Same observer as
    /// [`super::YrsNote`], and for the same reason: `encode_diff_v1` would carry
    /// the whole delete set, which on a document of overwritten registers is
    /// most of what is in it.
    pending: Arc<Mutex<Option<Vec<u8>>>>,
}

impl YrsAccount {
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
}

impl AccountDoc for YrsAccount {
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

    fn write(&mut self, writes: &[(String, Option<Value>)]) -> Result<Vec<u8>> {
        self.take_pending();
        {
            let mut txn = self.doc.transact_mut();
            for (key, value) in writes {
                match value {
                    Some(Value::Str(s)) => {
                        self.root.insert(&mut txn, key.as_str(), s.as_str());
                    }
                    Some(Value::Bool(b)) => {
                        self.root.insert(&mut txn, key.as_str(), *b);
                    }
                    None => {
                        self.root.remove(&mut txn, key.as_str());
                    }
                }
            }
        }
        // A removal of a key that was never there is a transaction with no ops,
        // and Yrs emits no update for one. That is not an error: there is
        // nothing for a peer to apply.
        Ok(self.take_pending().unwrap_or_default())
    }

    fn apply_remote(&mut self, update: &[u8]) -> Result<()> {
        if update.is_empty() {
            return Ok(());
        }
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

    fn entries(&self) -> Vec<(String, Value)> {
        let txn = self.doc.transact();
        self.root
            .iter(&txn)
            .filter_map(|(key, value)| {
                let value = match value {
                    Out::Any(Any::String(s)) => Value::Str(s.to_string()),
                    Out::Any(Any::Bool(b)) => Value::Bool(b),
                    _ => return None,
                };
                Some((key.to_string(), value))
            })
            .collect()
    }

    fn snapshot(&self) -> Vec<u8> {
        self.doc
            .transact()
            .encode_state_as_update_v1(&StateVector::default())
    }
}
