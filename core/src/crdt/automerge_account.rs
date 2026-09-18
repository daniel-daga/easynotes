//! The account document on Automerge.
//!
//! Same document as [`super::yrs_account`] — one root map of scalars — so the
//! two backends are held to the same behaviour by the same suite, exactly as the
//! two note backends are.

use super::account::{AccountDoc, Value};
use crate::error::{CoreError, Result};
use automerge::transaction::Transactable;
use automerge::{Automerge, AutomergeError, ReadDoc, ScalarValue, ROOT};

pub struct AutomergeAccount {
    doc: Automerge,
}

fn map_err(e: AutomergeError) -> CoreError {
    CoreError::Crdt(e.to_string())
}

impl AccountDoc for AutomergeAccount {
    fn create(actor: &[u8; 16]) -> Self {
        Self {
            doc: Automerge::new().with_actor(actor.to_vec().into()),
        }
    }

    fn load(actor: &[u8; 16], snapshot: &[u8]) -> Result<Self> {
        let doc = Automerge::load(snapshot)
            .map_err(|e| CoreError::Crdt(format!("loading snapshot: {e}")))?
            .with_actor(actor.to_vec().into());
        Ok(Self { doc })
    }

    fn write(&mut self, writes: &[(String, Option<Value>)]) -> Result<Vec<u8>> {
        let heads = self.doc.get_heads();
        self.doc
            .transact::<_, _, AutomergeError>(|tx| {
                for (key, value) in writes {
                    match value {
                        Some(Value::Str(s)) => tx.put(ROOT, key.as_str(), s.as_str())?,
                        Some(Value::Bool(b)) => tx.put(ROOT, key.as_str(), *b)?,
                        None => {
                            // Deleting a key that is not there is an error in
                            // Automerge and a no-op everywhere else. It happens:
                            // forgetting a note touches five keys and a note
                            // nobody coloured has fewer.
                            if tx.get(ROOT, key.as_str())?.is_some() {
                                tx.delete(ROOT, key.as_str())?;
                            }
                        }
                    }
                }
                Ok(())
            })
            .map_err(|e| map_err(e.error))?;
        Ok(self.doc.save_after(&heads))
    }

    fn apply_remote(&mut self, update: &[u8]) -> Result<()> {
        if update.is_empty() {
            return Ok(());
        }
        self.doc.load_incremental(update).map_err(map_err)?;
        Ok(())
    }

    fn entries(&self) -> Vec<(String, Value)> {
        let mut out = Vec::new();
        for key in self.doc.keys(ROOT) {
            let Ok(Some((value, _))) = self.doc.get(ROOT, &key) else {
                continue;
            };
            let automerge::Value::Scalar(scalar) = value else {
                continue;
            };
            let value = match scalar.as_ref() {
                ScalarValue::Str(s) => Value::Str(s.to_string()),
                ScalarValue::Boolean(b) => Value::Bool(*b),
                _ => continue,
            };
            out.push((key, value));
        }
        out
    }

    fn snapshot(&self) -> Vec<u8> {
        self.doc.save()
    }
}
