use crate::document::Document;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

const MAX_UNDO_DEPTH: usize = 50;

struct DocEntry {
    current: Document,
    undo_stack: Vec<Document>,
    redo_stack: Vec<Document>,
}

/// In-memory registry of open documents, shared between the UI front-end
/// and the MCP server front-end so both drive the same live state and the
/// same undo history. Phase 0 undo is snapshot-based (clones the whole
/// document before each mutation); later phases replace the storage with a
/// real command log without changing this API.
#[derive(Clone)]
pub struct DocumentStore {
    docs: Arc<Mutex<HashMap<Uuid, DocEntry>>>,
    /// Set by `request_focus` (the `document.focus` MCP tool) and drained
    /// by `take_focus_request` (a UI's poll loop) - lets an agent bring a
    /// specific document's tab to the front in a UI that has this same
    /// store open, without the store knowing anything about UIs, windows,
    /// or tabs itself. `None` once consumed; a UI that isn't polling
    /// simply never drains it, which is harmless.
    focus_request: Arc<Mutex<Option<Uuid>>>,
    /// Fired by `mutate` after every successful mutation (see `on_change`) -
    /// a UI's push-based "this document just changed, re-render it now"
    /// signal, so an agent's edit shows up immediately instead of waiting
    /// for the next poll tick.
    change_listener: Arc<Mutex<Option<Box<dyn Fn(Uuid) + Send + Sync>>>>,
    /// The color most recently used by a paint/fill-type MCP tool call
    /// against each document (set by the handful of dispatch arms that
    /// have an obvious single "color" or "brush.color" - see
    /// `set_last_color`), so a UI's color swatch can mirror what an agent
    /// just painted with. Not a general "current tool state": most tools
    /// have no color concept, and this only ever reflects the *last*
    /// color used, not anything the UI is required to keep using.
    last_color: Arc<Mutex<HashMap<Uuid, [u8; 4]>>>,
}

impl Default for DocumentStore {
    fn default() -> Self {
        Self::new()
    }
}

impl DocumentStore {
    pub fn new() -> Self {
        Self {
            docs: Arc::new(Mutex::new(HashMap::new())),
            focus_request: Arc::new(Mutex::new(None)),
            change_listener: Arc::new(Mutex::new(None)),
            last_color: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Registers `f` to be called with a document's id every time `mutate`
    /// successfully changes it. Only one listener at a time (last caller
    /// wins) - this store has exactly one UI attached to it (the desktop
    /// app that embeds it), so there's never a real need for more.
    pub fn on_change(&self, f: impl Fn(Uuid) + Send + Sync + 'static) {
        *self.change_listener.lock().unwrap() = Some(Box::new(f));
    }

    /// Records the color most recently used by a paint/fill MCP call
    /// against `id` - see `last_color`'s doc comment.
    pub fn set_last_color(&self, id: Uuid, color: [u8; 4]) {
        self.last_color.lock().unwrap().insert(id, color);
    }

    pub fn get_last_color(&self, id: Uuid) -> Option<[u8; 4]> {
        self.last_color.lock().unwrap().get(&id).copied()
    }

    pub fn insert(&self, doc: Document) -> Uuid {
        let id = doc.id;
        self.docs.lock().unwrap().insert(
            id,
            DocEntry {
                current: doc,
                undo_stack: Vec::new(),
                redo_stack: Vec::new(),
            },
        );
        id
    }

    pub fn list(&self) -> Vec<(Uuid, String)> {
        self.docs
            .lock()
            .unwrap()
            .values()
            .map(|e| (e.current.id, e.current.name.clone()))
            .collect()
    }

    pub fn close(&self, id: Uuid) -> bool {
        self.docs.lock().unwrap().remove(&id).is_some()
    }

    pub fn get_clone(&self, id: Uuid) -> Option<Document> {
        self.docs.lock().unwrap().get(&id).map(|e| e.current.clone())
    }

    /// Calls the registered `on_change` listener, if any, outside of any
    /// `docs` lock (so a listener that itself calls back into the store,
    /// e.g. `get_clone`, can't deadlock against `mutate`/`undo`/`redo`).
    fn notify_change(&self, id: Uuid) {
        if let Some(f) = self.change_listener.lock().unwrap().as_ref() {
            f(id);
        }
    }

    /// Runs `f` against the live document, snapshotting it onto the undo
    /// stack first so the mutation can be undone by both the UI and by
    /// agents through `history.undo`.
    pub fn mutate<F, R>(&self, id: Uuid, f: F) -> Option<R>
    where
        F: FnOnce(&mut Document) -> R,
    {
        let result = {
            let mut docs = self.docs.lock().unwrap();
            let entry = docs.get_mut(&id)?;
            entry.undo_stack.push(entry.current.clone());
            if entry.undo_stack.len() > MAX_UNDO_DEPTH {
                entry.undo_stack.remove(0);
            }
            entry.redo_stack.clear();
            f(&mut entry.current)
        };
        self.notify_change(id);
        Some(result)
    }

    /// Like `mutate`, but doesn't push a new undo snapshot - for a caller
    /// that already took one snapshot for the *start* of a gesture (a
    /// pointer-down) and is now extending that same gesture (pointer-move
    /// segments of one human drag). Without this, a single freehand
    /// stroke - which the UI sends as dozens of small per-segment calls
    /// for live visual feedback while dragging, unlike an MCP agent's one
    /// `paint.strokePath` call with the whole point path - would push one
    /// undo snapshot *per segment*, so clicking Undo once would only pop
    /// the last sliver of the stroke instead of the whole visible stroke.
    pub fn mutate_continue<F, R>(&self, id: Uuid, f: F) -> Option<R>
    where
        F: FnOnce(&mut Document) -> R,
    {
        let result = {
            let mut docs = self.docs.lock().unwrap();
            let entry = docs.get_mut(&id)?;
            f(&mut entry.current)
        };
        self.notify_change(id);
        Some(result)
    }

    pub fn undo(&self, id: Uuid) -> bool {
        let changed = {
            let mut docs = self.docs.lock().unwrap();
            let Some(entry) = docs.get_mut(&id) else { return false };
            let Some(prev) = entry.undo_stack.pop() else { return false };
            let current = std::mem::replace(&mut entry.current, prev);
            entry.redo_stack.push(current);
            true
        };
        if changed {
            self.notify_change(id);
        }
        changed
    }

    pub fn redo(&self, id: Uuid) -> bool {
        let changed = {
            let mut docs = self.docs.lock().unwrap();
            let Some(entry) = docs.get_mut(&id) else { return false };
            let Some(next) = entry.redo_stack.pop() else { return false };
            let current = std::mem::replace(&mut entry.current, next);
            entry.undo_stack.push(current);
            true
        };
        if changed {
            self.notify_change(id);
        }
        changed
    }

    /// `(undo depth, redo depth)` for `history.list` - how many steps are
    /// available in each direction. Undo is snapshot-based rather than a
    /// labeled command log (see the struct doc comment), so this reports
    /// depth, not a list of named actions.
    pub fn history_depth(&self, id: Uuid) -> Option<(usize, usize)> {
        let docs = self.docs.lock().unwrap();
        let entry = docs.get(&id)?;
        Some((entry.undo_stack.len(), entry.redo_stack.len()))
    }

    /// Records that `id` should be brought to the front by whatever UI is
    /// showing this store. Returns `false` (and records nothing) if `id`
    /// isn't an open document.
    pub fn request_focus(&self, id: Uuid) -> bool {
        if !self.docs.lock().unwrap().contains_key(&id) {
            return false;
        }
        *self.focus_request.lock().unwrap() = Some(id);
        true
    }

    /// Drains the pending focus request, if any - a UI calls this on its
    /// own poll cadence rather than the store pushing to it.
    pub fn take_focus_request(&self) -> Option<Uuid> {
        self.focus_request.lock().unwrap().take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Document;

    #[test]
    fn take_focus_request_returns_the_requested_document_exactly_once() {
        let store = DocumentStore::new();
        let id = store.insert(Document::new("Doc", 4, 4));

        assert_eq!(store.take_focus_request(), None, "nothing requested yet");
        assert!(store.request_focus(id));
        assert_eq!(store.take_focus_request(), Some(id));
        assert_eq!(store.take_focus_request(), None, "a request is drained once, not repeatedly");
    }

    #[test]
    fn request_focus_on_an_unknown_document_is_rejected_not_recorded() {
        let store = DocumentStore::new();
        let real_id = store.insert(Document::new("Doc", 4, 4));
        store.request_focus(real_id);

        let bogus_id = Uuid::new_v4();
        assert!(!store.request_focus(bogus_id), "an id for a document that isn't open must be rejected");
        assert_eq!(store.take_focus_request(), Some(real_id), "the earlier valid request must be unaffected by the rejected one");
    }

    #[test]
    fn mutate_continue_extends_the_same_gesture_as_one_undo_step() {
        let store = DocumentStore::new();
        let id = store.insert(Document::new("Doc", 4, 4));

        // Simulates a human drag: one `mutate` for the pointer-down
        // segment, then several `mutate_continue` calls for the
        // pointer-move segments of the same stroke.
        store.mutate(id, |doc| doc.name = "segment 1".to_string());
        store.mutate_continue(id, |doc| doc.name = "segment 2".to_string());
        store.mutate_continue(id, |doc| doc.name = "segment 3".to_string());
        assert_eq!(store.history_depth(id), Some((1, 0)), "the whole gesture must be exactly one undo step, not one per segment");

        assert!(store.undo(id));
        let after_undo = store.get_clone(id).unwrap();
        assert_eq!(after_undo.name, "Doc", "undoing the gesture must revert all its segments at once, back to before the stroke started");
    }

    #[test]
    fn mutate_continue_still_notifies_change_listeners() {
        let store = DocumentStore::new();
        let id = store.insert(Document::new("Doc", 4, 4));
        let calls = Arc::new(Mutex::new(Vec::new()));
        let calls_clone = calls.clone();
        store.on_change(move |changed_id| calls_clone.lock().unwrap().push(changed_id));

        store.mutate_continue(id, |_doc| ());
        assert_eq!(*calls.lock().unwrap(), vec![id], "mutate_continue must still push a live-update notification");
    }

    #[test]
    fn on_change_fires_for_mutate_undo_and_redo_but_not_for_a_failed_lookup() {
        let store = DocumentStore::new();
        let id = store.insert(Document::new("Doc", 4, 4));

        let calls = Arc::new(Mutex::new(Vec::new()));
        let calls_clone = calls.clone();
        store.on_change(move |changed_id| calls_clone.lock().unwrap().push(changed_id));

        store.mutate(id, |_doc| ());
        assert_eq!(*calls.lock().unwrap(), vec![id], "a successful mutate must fire the listener");

        assert!(!store.mutate(Uuid::new_v4(), |_doc| ()).is_some());
        assert_eq!(calls.lock().unwrap().len(), 1, "mutate against an unknown document must not fire the listener");

        store.undo(id);
        assert_eq!(*calls.lock().unwrap(), vec![id, id], "undo must fire the listener");

        store.redo(id);
        assert_eq!(*calls.lock().unwrap(), vec![id, id, id], "redo must fire the listener");

        assert!(!store.redo(id), "nothing left to redo");
        assert_eq!(calls.lock().unwrap().len(), 3, "a no-op redo must not fire the listener again");
    }

    #[test]
    fn last_color_reflects_the_most_recent_value_set_for_each_document() {
        let store = DocumentStore::new();
        let id_a = store.insert(Document::new("A", 4, 4));
        let id_b = store.insert(Document::new("B", 4, 4));

        assert_eq!(store.get_last_color(id_a), None);

        store.set_last_color(id_a, [255, 0, 0, 255]);
        store.set_last_color(id_b, [0, 255, 0, 255]);
        assert_eq!(store.get_last_color(id_a), Some([255, 0, 0, 255]));
        assert_eq!(store.get_last_color(id_b), Some([0, 255, 0, 255]));

        store.set_last_color(id_a, [0, 0, 255, 255]);
        assert_eq!(store.get_last_color(id_a), Some([0, 0, 255, 255]), "a later call must overwrite the earlier color, not append");
    }
}
