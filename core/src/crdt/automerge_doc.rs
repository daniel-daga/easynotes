//! Automerge backend.
//!
//! Document shape (shared with the Yrs backend, so the two are compared on the
//! same workload):
//!
//! ```text
//! root map
//!   "title"  -> string
//!   <item id> -> map { "text": string, "checked": bool, "order": string }
//! ```
//!
//! Items hang off the root directly rather than off a nested `items` container.
//! That is not cosmetic: neither Automerge nor Yrs can create a *container* object
//! deterministically across replicas, so two devices that each bootstrapped their
//! own `items` map would produce two conflicting objects and silently lose one
//! side's items on merge. Root keys, by contrast, are just names both sides agree
//! on. `"title"` is the one reserved key; every other key is an item ID.

use super::{sort_items, Edit, ItemView, NoteDoc, NoteView};
use crate::error::{CoreError, Result};
use crate::ids::ItemId;
use automerge::transaction::Transactable;
use automerge::{Automerge, AutomergeError, ObjId, ObjType, ReadDoc, ScalarValue, Value, ROOT};

const TITLE: &str = "title";
const TEXT: &str = "text";
const CHECKED: &str = "checked";
const ORDER: &str = "order";

pub struct AutomergeNote {
    doc: Automerge,
}

fn map_err(e: AutomergeError) -> CoreError {
    CoreError::Crdt(e.to_string())
}

impl AutomergeNote {
    fn item_obj(doc: &Automerge, id: ItemId) -> Option<ObjId> {
        match doc.get(ROOT, id.encode()) {
            Ok(Some((Value::Object(ObjType::Map), obj))) => Some(obj),
            _ => None,
        }
    }

    fn str_at(doc: &Automerge, obj: &ObjId, prop: &str) -> String {
        match doc.get(obj, prop) {
            Ok(Some((Value::Scalar(s), _))) => match s.as_ref() {
                ScalarValue::Str(s) => s.to_string(),
                _ => String::new(),
            },
            _ => String::new(),
        }
    }

    fn bool_at(doc: &Automerge, obj: &ObjId, prop: &str) -> bool {
        match doc.get(obj, prop) {
            Ok(Some((Value::Scalar(s), _))) => matches!(s.as_ref(), ScalarValue::Boolean(true)),
            _ => false,
        }
    }
}

impl NoteDoc for AutomergeNote {
    type Account = super::AutomergeAccount;

    fn create(actor: &[u8; 16]) -> Self {
        // No bootstrap transaction: an empty document has no changes, so two
        // devices that create a note from the same share blob start identical.
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

    fn apply_local(&mut self, edit: &Edit) -> Result<Vec<u8>> {
        let heads = self.doc.get_heads();
        self.doc
            .transact::<_, _, AutomergeError>(|tx| {
                match edit {
                    Edit::SetTitle(t) => {
                        tx.put(ROOT, TITLE, t.as_str())?;
                    }
                    Edit::AddItem { id, text, order } => {
                        let obj = tx.put_object(ROOT, id.encode(), ObjType::Map)?;
                        tx.put(&obj, TEXT, text.as_str())?;
                        tx.put(&obj, CHECKED, false)?;
                        tx.put(&obj, ORDER, order.as_str())?;
                    }
                    Edit::SetItemText { id, text } => {
                        if let Some((Value::Object(_), obj)) = tx.get(ROOT, id.encode())? {
                            tx.put(&obj, TEXT, text.as_str())?;
                        }
                    }
                    Edit::SetChecked { id, checked } => {
                        if let Some((Value::Object(_), obj)) = tx.get(ROOT, id.encode())? {
                            tx.put(&obj, CHECKED, *checked)?;
                        }
                    }
                    Edit::MoveItem { id, order } => {
                        if let Some((Value::Object(_), obj)) = tx.get(ROOT, id.encode())? {
                            tx.put(&obj, ORDER, order.as_str())?;
                        }
                    }
                    Edit::RemoveItem { id } => {
                        tx.delete(ROOT, id.encode())?;
                    }
                }
                Ok(())
            })
            .map_err(|e| map_err(e.error))?;

        Ok(self.doc.save_after(&heads))
    }

    fn apply_remote(&mut self, update: &[u8]) -> Result<()> {
        // Automerge identifies changes by hash, so re-applying one is a no-op.
        self.doc.load_incremental(update).map_err(map_err)?;
        Ok(())
    }

    fn view(&self) -> NoteView {
        let title = match self.doc.get(ROOT, TITLE) {
            Ok(Some((Value::Scalar(s), _))) => match s.as_ref() {
                ScalarValue::Str(s) => s.to_string(),
                _ => String::new(),
            },
            _ => String::new(),
        };

        let mut items = Vec::new();
        for key in self.doc.keys(ROOT) {
            if key == TITLE {
                continue;
            }
            // Anything that is not a well-formed item ID is a key from a future
            // version of the app; ignore it rather than crash on it.
            let Ok(id) = ItemId::decode(&key) else {
                continue;
            };
            let Some(obj) = Self::item_obj(&self.doc, id) else {
                continue;
            };
            items.push(ItemView {
                id,
                text: Self::str_at(&self.doc, &obj, TEXT),
                checked: Self::bool_at(&self.doc, &obj, CHECKED),
                order: Self::str_at(&self.doc, &obj, ORDER),
            });
        }
        sort_items(&mut items);
        NoteView { title, items }
    }

    fn snapshot(&self) -> Vec<u8> {
        self.doc.save()
    }

    fn backend_name() -> &'static str {
        "automerge"
    }
}
