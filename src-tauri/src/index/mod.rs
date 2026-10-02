//! Background full-text indexing into the FTS5 `Search` table (P1-D6).
//!
//! Indexing a 500-page PDF means 500 extractions and 500 rows. Both of those are
//! decisions this module owns:
//!
//! * Pages are extracted and inserted in **batches**, so memory is bounded by the
//!   batch (not the document) and the writes share one transaction per batch.
//! * The whole thing runs on a blocking worker and reports progress, because a
//!   500-page index must not look like a hang.
//!
//! The loop itself is free of PDFium and Tauri: it takes an extraction callback and a
//! progress callback, so its behaviour — batching, cancellation, partial failure — is
//! unit-testable without the native library or an app handle. The command layer wires
//! those callbacks to PDFium and to `index://progress` events.

use std::collections::HashMap;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::db::{app_state, search, Db};
use crate::model::AppError;

pub mod app;

/// Pages extracted per batch. Bounds peak memory: at this size the text held at once
/// is tens of kilobytes even for a dense page, and a 500-page document is 16
/// transactions instead of one per page.
pub const PAGES_PER_BATCH: u32 = 32;

/// Progress payload for the `index://progress` event (P1-D6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexProgress {
    pub doc_id: String,
    pub done: u32,
    pub total: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IndexState {
    Running,
    Done,
    Failed,
}

/// Status written to `AppState` under [`status_key`], so the viewer still knows a
/// document is indexing after a pane remount (P1-D6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexStatus {
    pub state: IndexState,
    pub done: u32,
    pub total: u32,
}

/// AppState key holding [`IndexStatus`] JSON.
pub fn status_key(doc_id: &str) -> String {
    format!("index_status:{doc_id}")
}

/// AppState key holding the `content_hash` that was current when the index completed.
/// Written only on success, so an interrupted index is detected as stale and redone.
pub fn hash_key(doc_id: &str) -> String {
    format!("index_hash:{doc_id}")
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexSummary {
    /// Pages written to the index.
    pub pages_indexed: u32,
    /// Pages whose extraction failed and were skipped.
    pub pages_skipped: u32,
    /// True when a newer request superseded this run before it finished.
    pub cancelled: bool,
}

/// True when `doc_id` has no usable index for `content_hash`.
///
/// The document id is stable across re-imports (P0 dedupes by content hash), so the
/// stored hash — not the row count alone — is what distinguishes "never indexed" from
/// "indexed, then the file changed underneath us". An index that was interrupted never
/// writes the hash, so this returns true again and the work is redone.
pub fn needs_indexing(db: &Db, doc_id: &str, content_hash: Option<&str>) -> Result<bool, AppError> {
    db.with(|connection| {
        if search::row_count(connection, doc_id)? == 0 {
            return Ok(true);
        }
        let indexed = app_state::get(connection, &hash_key(doc_id))?;
        Ok(indexed.as_deref() != content_hash)
    })
}

/// Extracts every page of a document into the index, in batches.
///
/// `extract` is called per page and may return an error for a single bad page;
/// `cancelled` is polled between batches; `on_progress` is called after each batch.
///
/// Extraction errors are skipped rather than aborting the run — one unreadable page in
/// a 500-page document must not cost the other 499 their index. The exception is
/// [`AppError::PdfiumUnavailable`], which cannot be a per-page problem and aborts
/// immediately rather than producing 500 identical failures and a silently empty index.
///
/// Blocking: call from a `spawn_blocking` worker.
pub fn run(
    db: &Db,
    doc_id: &str,
    page_count: u32,
    mut extract: impl FnMut(u32) -> Result<String, AppError>,
    cancelled: impl Fn() -> bool,
    mut on_progress: impl FnMut(IndexProgress),
) -> Result<IndexSummary, AppError> {
    // Start from a clean slate: rows from a previous generation would otherwise mix
    // with the new ones and return hits for text the file no longer contains.
    db.with(|connection| search::delete_document(connection, doc_id).map(|_| ()))?;

    let mut summary = IndexSummary::default();
    let mut processed = 0u32;

    while processed < page_count {
        if cancelled() {
            summary.cancelled = true;
            return Ok(summary);
        }

        let batch_end = (processed + PAGES_PER_BATCH).min(page_count);
        let mut batch: Vec<(u32, String)> = Vec::with_capacity(PAGES_PER_BATCH as usize);

        for page in processed..batch_end {
            match extract(page) {
                Ok(text) => batch.push((page, text)),
                Err(error) if matches!(error, AppError::PdfiumUnavailable(_)) => {
                    return Err(error);
                }
                Err(_) => summary.pages_skipped += 1,
            }
        }

        let written = db.with(|connection| search::insert_pages(connection, doc_id, &batch))?;
        summary.pages_indexed += written as u32;
        processed = batch_end;

        on_progress(IndexProgress {
            doc_id: doc_id.to_string(),
            done: processed,
            total: page_count,
        });
    }

    Ok(summary)
}

/// Records the completed index: the hash that was indexed, and a terminal status.
pub fn mark_complete(
    db: &Db,
    doc_id: &str,
    content_hash: Option<&str>,
    total: u32,
) -> Result<(), AppError> {
    db.with(|connection| {
        if let Some(hash) = content_hash {
            app_state::set(connection, &hash_key(doc_id), hash)?;
        }
        write_status(
            connection,
            doc_id,
            &IndexStatus {
                state: IndexState::Done,
                done: total,
                total,
            },
        )
    })
}

/// Records a terminal failure. The hash is deliberately not written, so the next open
/// retries instead of treating the failed run as authoritative.
pub fn mark_failed(db: &Db, doc_id: &str, total: u32) -> Result<(), AppError> {
    db.with(|connection| {
        write_status(
            connection,
            doc_id,
            &IndexStatus {
                state: IndexState::Failed,
                done: 0,
                total,
            },
        )
    })
}

/// Records an in-progress status, so a pane that remounts mid-index can still show it.
pub fn mark_progress(db: &Db, progress: &IndexProgress) -> Result<(), AppError> {
    db.with(|connection| {
        write_status(
            connection,
            &progress.doc_id,
            &IndexStatus {
                state: IndexState::Running,
                done: progress.done,
                total: progress.total,
            },
        )
    })
}

fn write_status(
    connection: &rusqlite::Connection,
    doc_id: &str,
    status: &IndexStatus,
) -> Result<(), AppError> {
    let json = serde_json::to_string(status).map_err(|e| AppError::Other(e.to_string()))?;
    app_state::set(connection, &status_key(doc_id), &json)
}

/// Reads back a document's index status, if it has one.
pub fn read_status(db: &Db, doc_id: &str) -> Result<Option<IndexStatus>, AppError> {
    db.with(|connection| {
        let raw = app_state::get(connection, &status_key(doc_id))?;
        // A corrupt status is not worth failing a command over; treat it as absent and
        // let the indexer decide from the row count instead.
        Ok(raw.and_then(|json| serde_json::from_str(&json).ok()))
    })
}

/// The state a run needs, re-resolved on every attempt because the file can change
/// underneath a long index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexSource {
    pub content_hash: Option<String>,
    pub page_count: u32,
}

/// Maximum attempts for one document. A file that is rewritten faster than it can be
/// indexed is pathological; giving up after a few tries keeps the worker from spinning
/// forever. The index is left stale (no hash recorded), so an idle re-open retries.
const MAX_ATTEMPTS: u32 = 4;

/// Runs a document's index to completion, surviving re-imports that land mid-run.
///
/// The awkward case this exists for: a user opens a 500-page PDF, then re-opens it (or
/// replaces it) while indexing is still in flight. Only one run may own a document's
/// rows at a time, so the runner in flight cannot be joined by a second — instead it
/// notices it has been superseded, re-resolves the document, and starts again with the
/// new content.
///
/// A restart is triggered only by a **changed content hash**, which is the only thing
/// that makes the work already done worthless. Being interrupted for any other reason
/// (a redundant re-open of the same file) leaves the index incomplete but not wrong, so
/// the run gives up and lets an idle re-open finish it rather than burning CPU on a
/// restart nobody asked for. Recording the hash it *actually* indexed is what keeps
/// this honest: if the file changed again since, the recorded hash will not match and
/// the next open re-indexes.
///
/// `resolve` returns `None` if the document no longer exists, which ends the loop.
///
/// Blocking: call from a `spawn_blocking` worker.
pub fn run_until_current(
    db: &Db,
    service: &IndexService,
    doc_id: &str,
    mut resolve: impl FnMut() -> Result<Option<IndexSource>, AppError>,
    mut extract: impl FnMut(u32) -> Result<String, AppError>,
    mut on_progress: impl FnMut(IndexProgress),
) -> Result<IndexSummary, AppError> {
    // Someone else owns this document's rows; they will finish the job.
    let Some(mut ticket) = service.try_begin(doc_id) else {
        return Ok(IndexSummary::default());
    };

    let outcome = (|| -> Result<IndexSummary, AppError> {
        // What we last tried to index, set at the top of each attempt. A mismatch with
        // what we resolve now is the only reason to restart.
        let mut attempted: Option<Option<String>>;
        // Kept so that exhausting the attempt cap reports the last attempt's real
        // numbers rather than an empty "nothing happened" summary.
        let mut last = IndexSummary::default();

        for _ in 0..MAX_ATTEMPTS {
            let Some(source) = resolve()? else {
                return Ok(last);
            };
            attempted = Some(source.content_hash.clone());

            let summary = run(
                db,
                doc_id,
                source.page_count,
                &mut extract,
                || !service.is_current(doc_id, ticket),
                &mut on_progress,
            )?;

            if !summary.cancelled {
                mark_complete(
                    db,
                    doc_id,
                    source.content_hash.as_deref(),
                    source.page_count,
                )?;
                return Ok(summary);
            }
            last = summary;

            // Superseded. A restart is worth it only if the content actually changed:
            // being interrupted while indexing identical bytes leaves valid work behind.
            let changed = match resolve()? {
                Some(current) => attempted.as_ref() != Some(&current.content_hash),
                None => false,
            };
            if !changed {
                return Ok(last);
            }

            ticket = service.renew(doc_id);
        }

        // The attempt cap is reached only by a file that keeps changing underneath us.
        // Report it as cancelled and leave the index stale, so an idle re-open retries.
        Ok(last)
    })();

    // Always released, including on failure, so a later open can try again rather than
    // finding the document permanently claimed by a task that has already died.
    service.finish(doc_id);
    outcome
}

#[derive(Default, Clone, Copy)]
struct DocIndexState {
    generation: u64,
    running: bool,
}

/// Tracks which documents are being indexed and supersedes stale runs.
///
/// One document's indexing is independent of another's, so the generation counter is
/// per document: re-importing file A must not abandon a long index of file B.
pub struct IndexService {
    states: Mutex<HashMap<String, DocIndexState>>,
}

impl std::fmt::Debug for IndexService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let states = self.states.lock().unwrap_or_else(|e| e.into_inner());
        f.debug_struct("IndexService")
            .field("running", &states.values().filter(|s| s.running).count())
            .finish()
    }
}

impl Default for IndexService {
    fn default() -> Self {
        Self::new()
    }
}

impl IndexService {
    pub fn new() -> Self {
        Self {
            states: Mutex::new(HashMap::new()),
        }
    }

    /// Claims the right to index a document, bumping its generation.
    ///
    /// Returns `None` when a run is already in flight for this document. That is
    /// single-writer by design: two runs over the same rows could interleave a delete
    /// with an insert and leave a mixture of two generations of content in the index.
    /// A re-import while indexing is handled by [`invalidate`](Self::invalidate) —
    /// the in-flight run aborts and then re-runs for the new content — not by a second
    /// concurrent run.
    pub fn try_begin(&self, doc_id: &str) -> Option<u64> {
        let mut states = self.states.lock().unwrap_or_else(|e| e.into_inner());
        let state = states.entry(doc_id.to_string()).or_default();
        if state.running {
            return None;
        }
        state.generation += 1;
        state.running = true;
        Some(state.generation)
    }

    /// Marks a document's in-flight index as stale, so it stops at its next batch.
    ///
    /// Called when the document is re-imported: the running task cannot be interrupted
    /// mid-extraction, but it polls between batches and will abandon the rest.
    pub fn invalidate(&self, doc_id: &str) {
        let mut states = self.states.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(state) = states.get_mut(doc_id) {
            state.generation += 1;
        }
    }

    /// Takes a fresh ticket for a document whose previous run was superseded.
    ///
    /// The runner calls this between attempts: it already owns the single-writer claim,
    /// so it must not go back through `try_begin` (which would refuse).
    pub fn renew(&self, doc_id: &str) -> u64 {
        let mut states = self.states.lock().unwrap_or_else(|e| e.into_inner());
        let state = states.entry(doc_id.to_string()).or_default();
        state.generation += 1;
        state.running = true;
        state.generation
    }

    /// True while `ticket` is still the newest run for `doc_id`.
    pub fn is_current(&self, doc_id: &str, ticket: u64) -> bool {
        let states = self.states.lock().unwrap_or_else(|e| e.into_inner());
        states.get(doc_id).is_some_and(|s| s.generation == ticket)
    }

    /// Releases the in-flight claim. Safe to call from a task that was superseded.
    pub fn finish(&self, doc_id: &str) {
        let mut states = self.states.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(state) = states.get_mut(doc_id) {
            state.running = false;
        }
    }

    pub fn is_running(&self, doc_id: &str) -> bool {
        let states = self.states.lock().unwrap_or_else(|e| e.into_inner());
        states.get(doc_id).is_some_and(|s| s.running)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Db {
        Db::open_in_memory().unwrap()
    }

    /// Extracts each page's text, recording every page it was asked for.
    fn counting_extract<'a>(
        seen: &'a Mutex<Vec<u32>>,
    ) -> impl Fn(u32) -> Result<String, AppError> + 'a {
        move |page| {
            seen.lock().unwrap().push(page);
            Ok(format!("unique token page{page}"))
        }
    }

    fn hits(db: &Db, query: &str) -> Vec<crate::model::SearchHit> {
        db.with(|connection| search::query(connection, query, 50))
            .unwrap()
    }

    #[test]
    fn indexes_every_page_and_makes_them_searchable() {
        let db = db();
        let seen = Mutex::new(Vec::new());

        let summary = run(&db, "doc-1", 5, counting_extract(&seen), || false, |_| {}).unwrap();

        assert_eq!(summary.pages_indexed, 5);
        assert_eq!(summary.pages_skipped, 0);
        assert!(!summary.cancelled);
        assert_eq!(*seen.lock().unwrap(), vec![0, 1, 2, 3, 4]);

        // Every page is individually findable, with the right page number.
        for page in 0..5u32 {
            let found = hits(&db, &format!("page{page}"));
            assert_eq!(found.len(), 1, "page {page} should be indexed");
            assert_eq!(found[0].page, page);
        }
    }

    #[test]
    fn progress_is_monotonic_and_ends_at_the_page_count() {
        let db = db();
        let seen = Mutex::new(Vec::new());
        let progress = Mutex::new(Vec::new());

        run(
            &db,
            "doc-1",
            100,
            counting_extract(&seen),
            || false,
            |p| progress.lock().unwrap().push(p),
        )
        .unwrap();

        let progress = progress.into_inner().unwrap();
        assert!(!progress.is_empty());

        let mut previous = 0;
        for step in &progress {
            assert_eq!(step.doc_id, "doc-1");
            assert_eq!(step.total, 100);
            assert!(
                step.done > previous,
                "progress must strictly increase: {step:?} after {previous}"
            );
            previous = step.done;
        }
        assert_eq!(progress.last().unwrap().done, 100);

        // Batched, not one event per page: 100 pages at 32/batch is 4 events.
        assert_eq!(progress.len(), 4);
    }

    #[test]
    fn a_shorter_document_than_one_batch_still_reports_completion() {
        let db = db();
        let seen = Mutex::new(Vec::new());
        let progress = Mutex::new(Vec::new());

        let summary = run(
            &db,
            "doc-1",
            3,
            counting_extract(&seen),
            || false,
            |p| progress.lock().unwrap().push(p),
        )
        .unwrap();

        assert_eq!(summary.pages_indexed, 3);
        assert_eq!(progress.into_inner().unwrap().len(), 1);
    }

    #[test]
    fn a_zero_page_document_indexes_nothing_without_reporting_progress() {
        let db = db();
        let progress = Mutex::new(Vec::new());

        let summary = run(
            &db,
            "doc-1",
            0,
            |_| Ok(String::new()),
            || false,
            |p| progress.lock().unwrap().push(p),
        )
        .unwrap();

        assert_eq!(summary.pages_indexed, 0);
        assert!(progress.into_inner().unwrap().is_empty());
    }

    #[test]
    fn re_indexing_replaces_rather_than_duplicating() {
        let db = db();
        let seen = Mutex::new(Vec::new());

        run(&db, "doc-1", 3, counting_extract(&seen), || false, |_| {}).unwrap();
        // Second pass over the same document, as a re-open after a content change.
        run(&db, "doc-1", 3, counting_extract(&seen), || false, |_| {}).unwrap();

        // Row count is unchanged, so the second run replaced the first instead of
        // appending a duplicate generation.
        let count = db
            .with(|connection| search::row_count(connection, "doc-1"))
            .unwrap();
        assert_eq!(count, 3);
        assert_eq!(hits(&db, "page0").len(), 1);
    }

    #[test]
    fn stale_rows_are_gone_after_a_re_index() {
        let db = db();

        run(
            &db,
            "doc-1",
            1,
            |_| Ok("obsolete wording".to_string()),
            || false,
            |_| {},
        )
        .unwrap();
        assert_eq!(hits(&db, "obsolete").len(), 1);

        run(
            &db,
            "doc-1",
            1,
            |_| Ok("current wording".to_string()),
            || false,
            |_| {},
        )
        .unwrap();

        assert!(hits(&db, "obsolete").is_empty());
        assert_eq!(hits(&db, "current").len(), 1);
    }

    #[test]
    fn one_bad_page_does_not_cost_the_others_their_index() {
        let db = db();

        let summary = run(
            &db,
            "doc-1",
            4,
            |page| {
                if page == 2 {
                    return Err(AppError::Other("corrupt page".to_string()));
                }
                Ok(format!("token page{page}"))
            },
            || false,
            |_| {},
        )
        .unwrap();

        assert_eq!(summary.pages_indexed, 3);
        assert_eq!(summary.pages_skipped, 1);
        for page in [0u32, 1, 3] {
            assert_eq!(hits(&db, &format!("page{page}")).len(), 1);
        }
    }

    #[test]
    fn an_unavailable_pdfium_aborts_instead_of_indexing_nothing_silently() {
        let db = db();

        let result = run(
            &db,
            "doc-1",
            4,
            |_| Err(AppError::PdfiumUnavailable("no library".to_string())),
            || false,
            |_| {},
        );

        // A missing library is not a per-page problem: reporting success with an empty
        // index would leave the user with a search that silently never matches.
        let error = result.unwrap_err();
        assert_eq!(error.code(), "PDFIUM_UNAVAILABLE");
    }

    #[test]
    fn cancellation_stops_the_run_before_the_next_batch() {
        let db = db();
        let progress = Mutex::new(Vec::new());
        let seen = Mutex::new(Vec::new());

        // Superseded part-way through: the run must stop instead of finishing all 200.
        let summary = run(
            &db,
            "doc-1",
            200,
            counting_extract(&seen),
            || progress.lock().unwrap().len() >= 2,
            |p| progress.lock().unwrap().push(p),
        )
        .unwrap();

        assert!(summary.cancelled);
        assert_eq!(progress.lock().unwrap().len(), 2);
        assert_eq!(*seen.lock().unwrap(), (0..64).collect::<Vec<u32>>());
        assert_eq!(summary.pages_indexed, 64);
    }

    #[test]
    fn cancellation_before_the_first_batch_does_no_work() {
        let db = db();
        let seen = Mutex::new(Vec::new());

        let summary = run(&db, "doc-1", 50, counting_extract(&seen), || true, |_| {}).unwrap();

        assert!(summary.cancelled);
        assert!(seen.lock().unwrap().is_empty());
        assert_eq!(summary.pages_indexed, 0);
    }

    #[test]
    fn needs_indexing_is_true_when_nothing_is_indexed() {
        let db = db();
        assert!(needs_indexing(&db, "doc-1", Some("hash-a")).unwrap());
    }

    #[test]
    fn needs_indexing_is_false_only_after_a_completed_run_records_the_hash() {
        let db = db();
        let seen = Mutex::new(Vec::new());

        run(&db, "doc-1", 2, counting_extract(&seen), || false, |_| {}).unwrap();
        mark_complete(&db, "doc-1", Some("hash-a"), 2).unwrap();

        assert!(!needs_indexing(&db, "doc-1", Some("hash-a")).unwrap());
        // A changed file under the same id must be re-indexed.
        assert!(needs_indexing(&db, "doc-1", Some("hash-b")).unwrap());
    }

    #[test]
    fn an_interrupted_run_leaves_the_index_stale_so_it_is_redone() {
        let db = db();
        let events = Mutex::new(0u32);

        // Cancelled, so no hash is recorded even though some rows exist.
        run(
            &db,
            "doc-1",
            100,
            |page| Ok(format!("text {page}")),
            || *events.lock().unwrap() >= 1,
            |_| *events.lock().unwrap() += 1,
        )
        .unwrap();

        assert!(needs_indexing(&db, "doc-1", Some("hash-a")).unwrap());
    }

    #[test]
    fn a_failed_run_is_not_treated_as_indexed() {
        let db = db();
        mark_failed(&db, "doc-1", 10).unwrap();
        assert!(needs_indexing(&db, "doc-1", Some("hash-a")).unwrap());

        let status = read_status(&db, "doc-1").unwrap().unwrap();
        assert_eq!(status.state, IndexState::Failed);
        assert_eq!(status.total, 10);
    }

    #[test]
    fn status_round_trips_through_app_state() {
        let db = db();
        mark_progress(
            &db,
            &IndexProgress {
                doc_id: "doc-1".to_string(),
                done: 32,
                total: 100,
            },
        )
        .unwrap();

        let status = read_status(&db, "doc-1").unwrap().unwrap();
        assert_eq!(status.state, IndexState::Running);
        assert_eq!(status.done, 32);
        assert_eq!(status.total, 100);

        mark_complete(&db, "doc-1", Some("h"), 100).unwrap();
        assert_eq!(
            read_status(&db, "doc-1").unwrap().unwrap().state,
            IndexState::Done
        );
    }

    #[test]
    fn a_corrupt_status_is_absent_rather_than_an_error() {
        let db = db();
        db.with(|connection| app_state::set(connection, &status_key("doc-1"), "not json"))
            .unwrap();

        assert_eq!(read_status(&db, "doc-1").unwrap(), None);
    }

    #[test]
    fn status_of_an_unknown_document_is_none() {
        let db = db();
        assert_eq!(read_status(&db, "never-seen").unwrap(), None);
    }

    #[test]
    fn status_keys_are_per_document() {
        let db = db();

        mark_complete(&db, "doc-1", Some("h"), 5).unwrap();
        mark_failed(&db, "doc-2", 9).unwrap();

        assert_eq!(
            read_status(&db, "doc-1").unwrap().unwrap().state,
            IndexState::Done
        );
        assert_eq!(
            read_status(&db, "doc-2").unwrap().unwrap().state,
            IndexState::Failed
        );
    }

    #[test]
    fn index_service_hands_out_one_ticket_per_document() {
        let service = IndexService::new();

        let first = service.try_begin("doc-1").unwrap();
        assert!(service.is_current("doc-1", first));
        assert!(service.is_running("doc-1"));

        // A second concurrent index of the same document is refused: two runs over the
        // same rows could interleave a delete with an insert.
        assert_eq!(service.try_begin("doc-1"), None);

        // A different document is unaffected.
        let other = service.try_begin("doc-2").unwrap();
        assert!(service.is_current("doc-2", other));

        service.finish("doc-1");
        assert!(!service.is_running("doc-1"));
        assert!(service.is_running("doc-2"));
    }

    #[test]
    fn a_new_run_supersedes_the_previous_ticket() {
        let service = IndexService::new();

        let stale = service.try_begin("doc-1").unwrap();
        service.finish("doc-1");
        let fresh = service.try_begin("doc-1").unwrap();

        assert!(service.is_current("doc-1", fresh));
        assert!(
            !service.is_current("doc-1", stale),
            "the superseded run must not consider itself current"
        );
    }

    #[test]
    fn invalidate_stops_an_in_flight_run_without_freeing_the_single_writer_claim() {
        let service = IndexService::new();
        let ticket = service.try_begin("doc-1").unwrap();

        service.invalidate("doc-1");

        // The running task sees itself as superseded...
        assert!(!service.is_current("doc-1", ticket));
        // ...but the claim is still held, so nothing else can start writing these rows
        // while the old run is winding down.
        assert!(service.is_running("doc-1"));
        assert_eq!(service.try_begin("doc-1"), None);

        service.finish("doc-1");
        // Generation 1 was the first claim, invalidate bumped it to 2, so the next
        // claim is 3 — what matters is that indexing can start again at all.
        assert_eq!(service.try_begin("doc-1"), Some(3));
    }

    #[test]
    fn renew_gives_the_winding_down_runner_a_fresh_ticket() {
        let service = IndexService::new();
        let first = service.try_begin("doc-1").unwrap();
        service.invalidate("doc-1");

        let second = service.renew("doc-1");

        assert!(service.is_current("doc-1", second));
        assert!(!service.is_current("doc-1", first));
        assert_ne!(first, second);
        assert!(service.is_running("doc-1"));
    }

    #[test]
    fn invalidating_an_untracked_document_is_harmless() {
        let service = IndexService::new();
        service.invalidate("never-started");
        // Nothing is claimed, so the next open can index normally.
        assert!(service.try_begin("never-started").is_some());
    }

    fn source(hash: &str, pages: u32) -> IndexSource {
        IndexSource {
            content_hash: Some(hash.to_string()),
            page_count: pages,
        }
    }

    #[test]
    fn run_until_current_indexes_and_records_the_hash_it_used() {
        let db = db();
        let service = IndexService::new();

        let summary = run_until_current(
            &db,
            &service,
            "doc-1",
            || Ok(Some(source("hash-a", 3))),
            |page| Ok(format!("token page{page}")),
            |_| {},
        )
        .unwrap();

        assert_eq!(summary.pages_indexed, 3);
        assert!(!service.is_running("doc-1"));
        assert!(!needs_indexing(&db, "doc-1", Some("hash-a")).unwrap());
        assert_eq!(hits(&db, "page2").len(), 1);
    }

    #[test]
    fn a_reimport_mid_run_restarts_the_index_for_the_new_content() {
        let db = db();
        let service = IndexService::new();
        // The document resolves to 40 pages (straddling two batches) on the first
        // attempt, then to a *different file* (new hash, 5 pages) afterwards.
        let attempts = Mutex::new(0u32);

        let summary = run_until_current(
            &db,
            &service,
            "doc-1",
            || {
                let mut attempts = attempts.lock().unwrap();
                *attempts += 1;
                if *attempts == 1 {
                    Ok(Some(source("hash-old", 40)))
                } else {
                    Ok(Some(source("hash-new", 5)))
                }
            },
            |page| Ok(format!("token page{page}")),
            |_| {
                // Land the re-import during the first batch: a later generation for the
                // same document, so the run in flight sees itself superseded.
                service.invalidate("doc-1");
            },
        )
        .unwrap();

        assert!(!summary.cancelled);
        // The second attempt indexed the *new* content and recorded its hash.
        assert_eq!(summary.pages_indexed, 5);
        assert!(!needs_indexing(&db, "doc-1", Some("hash-new")).unwrap());
        assert!(needs_indexing(&db, "doc-1", Some("hash-old")).unwrap());
        assert!(service.try_begin("doc-1").is_some());
        service.finish("doc-1");
    }

    #[test]
    fn repeated_invalidation_without_a_content_change_does_not_restart_the_work() {
        let db = db();
        let service = IndexService::new();
        let extracts = Mutex::new(0u32);

        let summary = run_until_current(
            &db,
            &service,
            "doc-1",
            || Ok(Some(source("unchanged", 100))),
            |page| {
                *extracts.lock().unwrap() += 1;
                Ok(format!("token page{page}"))
            },
            // Superseded after every batch, but the file never actually changes: a
            // redundant re-open. Restarting would redo work that is still valid.
            |_| service.invalidate("doc-1"),
        )
        .unwrap();

        assert!(summary.cancelled);
        // Exactly one batch: the run gives up rather than restarting identical work.
        assert_eq!(*extracts.lock().unwrap(), PAGES_PER_BATCH);
        // Left stale, so an idle reopen finishes it.
        assert!(needs_indexing(&db, "doc-1", Some("unchanged")).unwrap());
        assert!(!service.is_running("doc-1"));
    }

    #[test]
    fn a_document_that_really_changes_on_every_attempt_stops_instead_of_spinning() {
        let db = db();
        let service = IndexService::new();
        let extracts = Mutex::new(0u32);
        let resolves = Mutex::new(0u32);

        // A *different* hash every time it is asked, so each restart is genuinely
        // justified and the attempt cap is the only thing that ends the loop.
        let summary = run_until_current(
            &db,
            &service,
            "doc-1",
            || {
                let mut resolves = resolves.lock().unwrap();
                *resolves += 1;
                Ok(Some(source(&format!("hash-{resolves}"), 100)))
            },
            |page| {
                *extracts.lock().unwrap() += 1;
                Ok(format!("token page{page}"))
            },
            |_| service.invalidate("doc-1"),
        )
        .unwrap();

        // Bounded work: MAX_ATTEMPTS x one batch, never an unbounded retry loop.
        assert!(summary.cancelled);
        assert_eq!(*extracts.lock().unwrap(), MAX_ATTEMPTS * PAGES_PER_BATCH);
        assert!(!service.is_running("doc-1"));
    }

    #[test]
    fn a_vanished_document_ends_the_run_without_error() {
        let db = db();
        let service = IndexService::new();

        let summary = run_until_current(
            &db,
            &service,
            "doc-1",
            || Ok(None),
            |_| Ok("unused".to_string()),
            |_| {},
        )
        .unwrap();

        assert_eq!(summary.pages_indexed, 0);
        assert!(!service.is_running("doc-1"));
    }

    #[test]
    fn a_second_runner_defers_to_the_one_in_flight() {
        let db = db();
        let service = IndexService::new();
        let _owner = service.try_begin("doc-1").unwrap();
        let extracts = Mutex::new(0u32);

        let summary = run_until_current(
            &db,
            &service,
            "doc-1",
            || Ok(Some(source("hash-a", 10))),
            |page| {
                *extracts.lock().unwrap() += 1;
                Ok(format!("token page{page}"))
            },
            |_| {},
        )
        .unwrap();

        // It did no work at all, and crucially did not release the owner's claim.
        assert_eq!(summary.pages_indexed, 0);
        assert_eq!(*extracts.lock().unwrap(), 0);
        assert!(service.is_running("doc-1"));
    }

    #[test]
    fn a_failed_run_releases_the_claim_so_a_later_open_can_retry() {
        let db = db();
        let service = IndexService::new();

        let result = run_until_current(
            &db,
            &service,
            "doc-1",
            || Ok(Some(source("hash-a", 2))),
            |_| Err(AppError::PdfiumUnavailable("no library".to_string())),
            |_| {},
        );

        assert!(result.is_err());
        // The claim must not outlive the task, or the document could never be indexed.
        assert!(!service.is_running("doc-1"));
        assert!(service.try_begin("doc-1").is_some());
    }

    #[test]
    fn one_documents_reimport_does_not_supersede_anothers_index() {
        let service = IndexService::new();

        let long_run = service.try_begin("doc-big").unwrap();
        // Re-importing a different file must not abandon the long index.
        let other = service.try_begin("doc-small").unwrap();
        service.finish("doc-small");

        assert!(service.is_current("doc-big", long_run));
        assert!(service.is_current("doc-small", other));
    }

    #[test]
    fn finishing_an_unknown_document_is_harmless() {
        let service = IndexService::new();
        service.finish("never-started");
        assert!(!service.is_running("never-started"));
    }
}
