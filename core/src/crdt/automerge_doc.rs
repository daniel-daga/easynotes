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
//! on. `"title"`, `"kind"` and `"body"` are the reserved keys; every other key
//! is an item ID.
//!
//! `"body"` is the one place that rule has to bend, because a body is a text
//! *object* and there is no root-level text type to name the way Yrs has one.
//! It is created lazily, by the first edit that needs it, and a text note makes
//! that edit when it is created — so the body exists in the note's first change
//! and every peer that joins receives it rather than making its own. What is
//! left is the note written before bodies existed that two devices both start
//! typing into offline: each creates a body, Automerge keeps one, and the other
//! device's text is lost. That is a real difference from Yrs, and one more reason
//! Yrs is the default ([ADR 0001](../../../docs/adr/0001-crdt.md)).

use super::{sort_items, Edit, ItemView, NoteDoc, NoteKind, NoteView};
use crate::error::{CoreError, Result};
use crate::ids::ItemId;
use automerge::transaction::Transactable;
use automerge::transaction::Transaction;
use automerge::{Automerge, AutomergeError, ObjId, ObjType, ReadDoc, ScalarValue, Value, ROOT};

const TITLE: &str = "title";
const KIND: &str = "kind";
const BODY: &str = "body";
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

    fn body_obj(doc: &impl ReadDoc) -> Option<ObjId> {
        match doc.get(ROOT, BODY) {
            Ok(Some((Value::Object(ObjType::Text), obj))) => Some(obj),
            _ => None,
        }
    }

    /// Set the body's text, creating the text object first if this is the first
    /// edit to need one. `update_text` does the diffing, and counts in whatever
    /// unit this build of Automerge counts in, so nothing here has to.
    fn set_body(tx: &mut Transaction, text: &str) -> std::result::Result<(), AutomergeError> {
        let obj = match Self::body_obj(tx) {
            Some(obj) => obj,
            None if text.is_empty() => return Ok(()),
            None => tx.put_object(ROOT, BODY, ObjType::Text)?,
        };
        tx.update_text(&obj, text)
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
                    Edit::SetKind(kind) => {
                        tx.put(ROOT, KIND, kind.as_str())?;
                        // A text note's body exists from its first change, for
                        // the reason the module docs give.
                        if *kind == NoteKind::Text && Self::body_obj(tx).is_none() {
                            tx.put_object(ROOT, BODY, ObjType::Text)?;
                        }
                    }
                    Edit::SetBody(text) => {
                        Self::set_body(tx, text)?;
                    }
                    Edit::ToChecklist { items } => {
                        tx.put(ROOT, KIND, NoteKind::Checklist.as_str())?;
                        Self::set_body(tx, "")?;
                        for item in items {
                            let obj = tx.put_object(ROOT, item.id.encode(), ObjType::Map)?;
                            tx.put(&obj, TEXT, item.text.as_str())?;
                            tx.put(&obj, CHECKED, false)?;
                            tx.put(&obj, ORDER, item.order.as_str())?;
                        }
                    }
                    Edit::ToText { body, remove } => {
                        tx.put(ROOT, KIND, NoteKind::Text.as_str())?;
                        Self::set_body(tx, body)?;
                        for id in remove {
                            tx.delete(ROOT, id.encode())?;
                        }
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

        let kind = NoteKind::parse(&Self::str_at(&self.doc, &ROOT, KIND));
        let body = Self::body_obj(&self.doc)
            .and_then(|obj| self.doc.text(&obj).ok())
            .unwrap_or_default();

        let mut items = Vec::new();
        for key in self.doc.keys(ROOT) {
            if key == TITLE || key == KIND || key == BODY {
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
        NoteView {
            kind,
            title,
            body,
            items,
        }
    }

    fn snapshot(&self) -> Vec<u8> {
        self.doc.save()
    }

    fn backend_name() -> &'static str {
        "automerge"
    }
}
