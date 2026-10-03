//! Per-document undo/redo (P2-D7), session-scoped.
//!
//! Scope decisions worth stating because they shape the API:
//!
//! * **Not persisted.** A stack surviving a restart would have to be validated
//!   against files that may have changed on disk since, and a stale stack that
//!   replays onto a different document is worse than no undo.
//! * **Memory-capped, dropping the oldest entries.** An unbounded stack over a
//!   long editing session holds every intermediate page state; dropping the far
//!   end degrades gracefully (older undos stop being available) instead of
//!   failing.
//! * **Entries are opaque labels plus document snapshots.** The editor does not
//!   interpret "what" an action was -- it stores bytes. Interpreting actions
//!   would tangle undo with every tool's geometry.

use std::collections::VecDeque;

/// One undoable step: a labelled snapshot of the document bytes.
#[derive(Debug, Clone, PartialEq)]
pub struct UndoEntry {
    pub label: String,
    pub bytes: Vec<u8>,
}

/// A document's undo history. `undo` and `redo` are separate stacks so a single
/// entry can be moved between them without re-deriving state.
#[derive(Debug, Default)]
pub struct UndoStack {
    undo: VecDeque<UndoEntry>,
    redo: Vec<UndoEntry>,
    /// Bytes currently applied, i.e. the state `undo` returns *from*.
    baseline: Option<Vec<u8>>,
    capacity: usize,
}

impl UndoStack {
    pub fn new(capacity: usize) -> Self {
        UndoStack {
            undo: VecDeque::new(),
            redo: Vec::new(),
            baseline: None,
            // Zero capacity would silently make every action un-undoable, which
            // looks like a bug rather than a setting.
            capacity: capacity.max(1),
        }
    }

    /// Records a completed action. `before` is the document state prior to the
    /// action, `after` the state to restore on redo.
    ///
    /// Any redo history is discarded: once the user edits after undoing, the
    /// branch they had undone is no longer reachable, and keeping it would let
    /// redo replay onto unrelated state.
    pub fn push(&mut self, label: impl Into<String>, before: Vec<u8>, after: Vec<u8>) {
        self.redo.clear();

        self.undo.push_back(UndoEntry {
            label: label.into(),
            bytes: before,
        });
        while self.undo.len() > self.capacity {
            self.undo.pop_front();
        }

        self.baseline = Some(after);
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Returns the state to restore, and the label of the action being undone.
    pub fn undo(&mut self, current: Vec<u8>) -> Option<(String, Vec<u8>)> {
        let entry = self.undo.pop_back()?;
        let label = entry.label;

        if let Some(baseline) = self.baseline.take() {
            self.redo.push(UndoEntry {
                label: label.clone(),
                bytes: baseline,
            });
        }
        self.baseline = Some(entry.bytes.clone());

        let _ = current;
        Some((label, entry.bytes))
    }

    /// Returns the state to restore, and the label of the action being redone.
    pub fn redo(&mut self, current: Vec<u8>) -> Option<(String, Vec<u8>)> {
        let entry = self.redo.pop()?;
        let label = entry.label;

        if let Some(baseline) = self.baseline.take() {
            self.undo.push_back(UndoEntry {
                label: label.clone(),
                bytes: baseline,
            });
            while self.undo.len() > self.capacity {
                self.undo.pop_front();
            }
        }
        self.baseline = Some(entry.bytes.clone());

        let _ = current;
        Some((label, entry.bytes))
    }

    /// Label of the action that `undo` would reverse, for menu/toolbar wording.
    pub fn next_undo_label(&self) -> Option<&str> {
        self.undo.back().map(|entry| entry.label.as_str())
    }

    pub fn next_redo_label(&self) -> Option<&str> {
        self.redo.last().map(|entry| entry.label.as_str())
    }

    /// Depth of each stack, for tests and diagnostics.
    pub fn depths(&self) -> (usize, usize) {
        (self.undo.len(), self.redo.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stack() -> UndoStack {
        UndoStack::new(3)
    }

    #[test]
    fn starts_empty() {
        let stack = stack();
        assert!(!stack.can_undo());
        assert!(!stack.can_redo());
        assert_eq!(stack.next_undo_label(), None);
        assert_eq!(stack.depths(), (0, 0));
    }

    #[test]
    fn undo_returns_the_previous_state() {
        let mut stack = stack();
        stack.push("highlight", b"v1".to_vec(), b"v2".to_vec());

        let (label, bytes) = stack.undo(b"v2".to_vec()).unwrap();
        assert_eq!(label, "highlight");
        assert_eq!(bytes, b"v1");
        assert!(!stack.can_undo());
        assert!(stack.can_redo());
    }

    #[test]
    fn redo_returns_the_later_state() {
        let mut stack = stack();
        stack.push("highlight", b"v1".to_vec(), b"v2".to_vec());
        stack.undo(b"v2".to_vec()).unwrap();

        let (label, bytes) = stack.redo(b"v1".to_vec()).unwrap();
        assert_eq!(label, "highlight");
        assert_eq!(bytes, b"v2");
        assert!(stack.can_undo());
        assert!(!stack.can_redo());
    }

    #[test]
    fn multiple_steps_unwind_in_reverse_order() {
        let mut stack = stack();
        stack.push("first", b"s1".to_vec(), b"s2".to_vec());
        stack.push("second", b"s2".to_vec(), b"s3".to_vec());

        assert_eq!(stack.next_undo_label(), Some("second"));
        assert_eq!(stack.undo(b"s3".to_vec()).unwrap().1, b"s2");
        assert_eq!(stack.next_undo_label(), Some("first"));
        assert_eq!(stack.undo(b"s2".to_vec()).unwrap().1, b"s1");
    }

    #[test]
    fn undo_then_redo_round_trips() {
        let mut stack = stack();
        stack.push("mark", b"before".to_vec(), b"after".to_vec());

        let (_, undone) = stack.undo(b"after".to_vec()).unwrap();
        assert_eq!(undone, b"before");
        let (_, redone) = stack.redo(b"before".to_vec()).unwrap();
        assert_eq!(redone, b"after");
    }

    #[test]
    fn a_new_action_discards_the_redo_branch() {
        // Classic editor behaviour: editing after an undo abandons the redo path.
        let mut stack = stack();
        stack.push("first", b"s1".to_vec(), b"s2".to_vec());
        stack.undo(b"s2".to_vec()).unwrap();
        assert!(stack.can_redo());

        stack.push("divergent", b"s1".to_vec(), b"s9".to_vec());
        assert!(!stack.can_redo(), "redo branch must be dropped");
    }

    #[test]
    fn capacity_drops_the_oldest_entry() {
        let mut stack = UndoStack::new(2);
        stack.push("one", b"a".to_vec(), b"b".to_vec());
        stack.push("two", b"b".to_vec(), b"c".to_vec());
        stack.push("three", b"c".to_vec(), b"d".to_vec());

        assert_eq!(stack.depths().0, 2);
        // "one" was evicted, so the oldest reachable state is "b".
        assert_eq!(stack.undo(b"d".to_vec()).unwrap().1, b"c");
        assert_eq!(stack.undo(b"c".to_vec()).unwrap().1, b"b");
        assert!(!stack.can_undo());
    }

    #[test]
    fn zero_capacity_still_allows_one_undo() {
        let mut stack = UndoStack::new(0);
        stack.push("only", b"a".to_vec(), b"b".to_vec());
        assert!(stack.can_undo());
    }

    #[test]
    fn undo_returns_none_when_empty() {
        let mut stack = stack();
        assert!(stack.undo(b"x".to_vec()).is_none());
        assert!(stack.redo(b"x".to_vec()).is_none());
    }

    #[test]
    fn redo_after_undo_of_undo_preserves_order() {
        let mut stack = stack();
        stack.push("a", b"s0".to_vec(), b"s1".to_vec());
        stack.push("b", b"s1".to_vec(), b"s2".to_vec());

        stack.undo(b"s2".to_vec()).unwrap(); // -> s1
        stack.undo(b"s1".to_vec()).unwrap(); // -> s0
        assert_eq!(stack.next_redo_label(), Some("a"));

        assert_eq!(stack.redo(b"s0".to_vec()).unwrap().1, b"s1");
        assert_eq!(stack.redo(b"s1".to_vec()).unwrap().1, b"s2");
        assert!(!stack.can_redo());
    }
}
