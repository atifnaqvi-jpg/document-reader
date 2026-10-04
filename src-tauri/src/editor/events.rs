//! Debounced annotation-change notification (P2-D11).
//!
//! Listeners (SearchPane re-running a query, the viewer reloading a page's overlay) need
//! to know a document's annotations changed, but a drag that produces one annotation per
//! frame would otherwise fire a burst of events and make every listener re-query on each
//! one. Changes are therefore collapsed onto a trailing edge: a burst emits once, 200ms
//! after the last change.
//!
//! The event carries only a document id. It is deliberately not a source of truth
//! (P2-D11): a listener must re-read the DB, because the payload describes *that
//! something changed*, not the resulting state.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use tauri::{AppHandle, Emitter, Manager};

/// Event name emitted after an annotation change settles (P2-D11).
pub const ANNOTATION_CHANGED_EVENT: &str = "annotation://changed";

/// How long the changes must be quiet before an event is emitted.
pub const DEBOUNCE: Duration = Duration::from_millis(200);

/// Payload of [`ANNOTATION_CHANGED_EVENT`]. Only the document id: see the module note.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AnnotationChanged {
    pub document_id: String,
}

/// Tracks the newest change ticket per document.
///
/// The generation-counter idiom matches [`RenderService::begin_pre_render`], where a
/// superseded ticket is how stale speculative work is detected. Here it is what makes the
/// debounce a *trailing* one: every change bumps the document's ticket, and a timer that
/// wakes to find a newer ticket stays silent, so only the last change in a burst survives.
///
/// [`RenderService::begin_pre_render`]: crate::render::RenderService::begin_pre_render
#[derive(Debug, Default)]
pub struct ChangeTickets {
    tickets: Mutex<HashMap<String, u64>>,
}

impl ChangeTickets {
    pub fn new() -> Self {
        Self::default()
    }

    /// Records a change to `document_id` and returns the ticket it was given.
    pub fn record(&self, document_id: &str) -> u64 {
        let mut tickets = self.tickets.lock().unwrap_or_else(|e| e.into_inner());
        let ticket = tickets.entry(document_id.to_string()).or_insert(0);
        *ticket += 1;
        *ticket
    }

    /// True while `ticket` is still the newest change recorded for `document_id`.
    pub fn is_current(&self, document_id: &str, ticket: u64) -> bool {
        let tickets = self.tickets.lock().unwrap_or_else(|e| e.into_inner());
        tickets.get(document_id).copied() == Some(ticket)
    }
}

/// Records a change and emits [`ANNOTATION_CHANGED_EVENT`] once the burst settles.
///
/// Blocking for [`DEBOUNCE`] on purpose, so callers put it on a detached task and the
/// command that made the change returns immediately. Emission is best-effort: no listener
/// is not an error, and a failed emit must never fail the write that caused it.
pub fn emit_after_debounce(app: AppHandle, document_id: String) {
    let tickets = app.state::<ChangeTickets>();
    let ticket = tickets.record(&document_id);

    std::thread::sleep(DEBOUNCE);

    // The trailing edge: a newer change for this document superseded us, so its own timer
    // is the one that will announce the burst.
    if tickets.is_current(&document_id, ticket) {
        let _ = app.emit(ANNOTATION_CHANGED_EVENT, AnnotationChanged { document_id });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_ticket_stays_current_until_another_change_is_recorded() {
        let tickets = ChangeTickets::new();

        let first = tickets.record("doc-a");
        assert!(tickets.is_current("doc-a", first));

        // A newer change supersedes it: this is what silences the earlier timer, turning
        // a burst of changes into a single trailing emission.
        let second = tickets.record("doc-a");
        assert!(!tickets.is_current("doc-a", first));
        assert!(tickets.is_current("doc-a", second));
    }

    #[test]
    fn documents_are_debounced_independently() {
        let tickets = ChangeTickets::new();

        let a = tickets.record("doc-a");
        let b = tickets.record("doc-b");

        // A change to one document must not cancel a pending event for another.
        assert!(tickets.is_current("doc-a", a));
        assert!(tickets.is_current("doc-b", b));
    }

    #[test]
    fn an_unknown_document_is_never_current() {
        let tickets = ChangeTickets::new();
        assert!(!tickets.is_current("never-touched", 1));
    }
}
