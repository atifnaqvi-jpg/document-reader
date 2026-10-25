//! The app-facing side of indexing: resolves a document, extracts through the
//! providers, and reports progress.
//!
//! This is where the pure loop in [`crate::index`] meets PDFium and Tauri. Keeping the
//! split means the loop's batching, cancellation and retry behaviour is tested without
//! either; this module only has to get the wiring right.

use std::path::Path;
use std::sync::Arc;

use tauri::{AppHandle, Emitter};

use crate::db::Db;
use crate::index::{
    mark_failed, mark_progress, needs_indexing as index_needs_indexing, read_status,
    run_until_current, IndexService, IndexSource, IndexStatus, IndexSummary,
};
use crate::model::{AppError, Format, LibraryEntry};
use crate::provider::{ReaderProvider, TextProvider};
use crate::render::RenderService;

/// Tauri event carrying [`IndexProgress`] (P1-D6).
pub const PROGRESS_EVENT: &str = "index://progress";

/// Resolves what a run needs to index a document: its current hash and page count.
///
/// Re-resolved on every attempt, because a file can change underneath a long index.
pub fn resolve_source(
    db: &Db,
    render: &RenderService,
    doc_id: &str,
) -> Result<Option<IndexSource>, AppError> {
    let Some((entry, content_hash)) = db.with(|connection| {
        let Some(entry) = crate::db::documents::get(connection, doc_id)? else {
            return Ok(None);
        };
        let hash = crate::db::documents::content_hash(connection, doc_id)?;
        Ok(Some((entry, hash)))
    })?
    else {
        return Ok(None);
    };

    let page_count = resolve_page_count(render, &entry, content_hash.as_deref())?;
    Ok(Some(IndexSource {
        content_hash,
        page_count,
    }))
}

/// The number of pages to index.
///
/// The import-time count comes from the provider and is `null` for text files (they
/// reflow, so pagination is a view concern) and for the PDF fixtures. The renderer is
/// the authoritative source for a PDF; for anything else one page is the whole story.
fn resolve_page_count(
    render: &RenderService,
    entry: &LibraryEntry,
    content_hash: Option<&str>,
) -> Result<u32, AppError> {
    if entry.meta.format != Format::Pdf {
        return Ok(1);
    }
    if let Some(pages) = entry.meta.pages {
        return Ok(pages);
    }
    render.page_count(Path::new(&entry.path), content_hash)
}

/// Extracts one page of a document through its provider.
///
/// Blocking (PDFium for a PDF): called from a blocking worker.
pub fn extract_page(
    render: &RenderService,
    entry: &LibraryEntry,
    content_hash: Option<&str>,
    page: u32,
) -> Result<String, AppError> {
    match entry.meta.format {
        Format::Pdf => render.extract_text(Path::new(&entry.path), content_hash, page),
        // One page, read whole: text formats have no layout engine and no page breaks
        // of their own, so there is nothing to split on yet (P2 gives them pagination).
        _ => TextProvider.extract_text(entry, page),
    }
}

/// Loads a document's library row for extraction. Returns `None` if it vanished.
fn load_entry(db: &Db, doc_id: &str) -> Result<Option<LibraryEntry>, AppError> {
    db.with(|connection| crate::db::documents::get(connection, doc_id))
}

/// Indexes a document to completion, surviving re-imports mid-run (P1-D6).
///
/// Blocking: call from a `spawn_blocking` worker. `app` is used only to emit progress;
/// pass `None` in tests.
pub fn index_document(
    app: Option<&AppHandle>,
    db: &Db,
    render: &Arc<RenderService>,
    service: &IndexService,
    doc_id: &str,
) -> Result<IndexSummary, AppError> {
    let render_for_resolve = Arc::clone(render);

    let summary = run_until_current(
        db,
        service,
        doc_id,
        || resolve_source(db, &render_for_resolve, doc_id),
        |page| {
            // Re-read per page: an entry that disappears mid-run (deleted file, moved
            // library row) should not abort the pages already extracted.
            let entry = load_entry(db, doc_id)?
                .ok_or_else(|| AppError::FileNotFound(doc_id.to_string()))?;
            let hash =
                db.with(|connection| crate::db::documents::content_hash(connection, doc_id))?;
            extract_page(&render_for_resolve, &entry, hash.as_deref(), page)
        },
        |progress| {
            // The status key is what survives a pane remount, so it is written for every
            // batch; the event is best-effort because no listener is not an error.
            let _ = mark_progress(db, &progress);
            if let Some(app) = app {
                let _ = app.emit(PROGRESS_EVENT, progress);
            }
        },
    );

    // A successful run records its own completion inside the loop. A failure needs the
    // status marked here, or the UI would show "indexing…" forever on a run that died.
    if summary.is_err() {
        if let Ok(Some(source)) = resolve_source(db, render, doc_id) {
            let _ = mark_failed(db, doc_id, source.page_count);
        }
    }

    summary
}

/// Reads a document's index status for the UI, if one is recorded.
pub fn document_status(db: &Db, doc_id: &str) -> Result<Option<IndexStatus>, AppError> {
    read_status(db, doc_id)
}

/// True when the document has no usable index for its current content (P1-D6).
pub fn needs_indexing(db: &Db, doc_id: &str) -> Result<bool, AppError> {
    let hash = db.with(|connection| crate::db::documents::content_hash(connection, doc_id))?;
    index_needs_indexing(db, doc_id, hash.as_deref())
}

/// The provider used for indexing, exposed so the command layer can report format
/// problems consistently.
pub fn indexable(entry: &LibraryEntry) -> bool {
    matches!(entry.meta.format, Format::Pdf | Format::Txt | Format::Md)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::search;
    use crate::index::IndexState;

    fn fixture(name: &str) -> String {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join(name)
            .display()
            .to_string()
    }

    fn setup() -> (Db, Arc<RenderService>, IndexService) {
        (
            Db::open_in_memory().unwrap(),
            Arc::new(RenderService::new()),
            IndexService::new(),
        )
    }

    #[test]
    fn resolve_source_reports_the_text_document_as_a_single_page() {
        let (db, render, _) = setup();
        let entry = db
            .with(|connection| {
                crate::library::open_and_import(connection, Path::new(&fixture("sample.txt")), 1)
            })
            .unwrap();

        let source = resolve_source(&db, &render, &entry.meta.id)
            .unwrap()
            .unwrap();
        assert_eq!(source.page_count, 1);
        assert!(source.content_hash.is_some());
    }

    #[test]
    fn resolve_source_is_none_for_an_unknown_document() {
        let (db, render, _) = setup();
        assert!(resolve_source(&db, &render, "never-seen")
            .unwrap()
            .is_none());
    }

    /// PDFium-dependent: indexes the real fixture through PDFium and checks the text
    /// layer landed in FTS5.
    #[test]
    #[ignore = "requires a real PDFium library; `cargo test -- --include-ignored` after fetching it"]
    fn indexes_a_pdf_fixture_through_pdfium_into_the_search_index() {
        let (db, render, service) = setup();
        let entry = db
            .with(|connection| {
                crate::library::open_and_import(connection, Path::new(&fixture("sample.pdf")), 1)
            })
            .unwrap();
        let doc_id = entry.meta.id.clone();

        let summary = index_document(None, &db, &render, &service, &doc_id).unwrap();
        assert_eq!(summary.pages_indexed, 3);
        assert!(!summary.cancelled);

        // The fixture's per-page tokens (see scripts/make-fixture-pdf.py).
        let hits = db
            .with(|connection| search::query(connection, "BRAVO", 10))
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].page, 1);
        assert_eq!(hits[0].document_id, doc_id);

        // A completed run records the hash, so a second open will not re-index.
        assert!(!needs_indexing(&db, &doc_id).unwrap());
    }

    #[test]
    #[ignore = "requires a real PDFium library; `cargo test -- --include-ignored` after fetching it"]
    fn a_completed_run_records_a_done_status() {
        let (db, render, service) = setup();
        let entry = db
            .with(|connection| {
                crate::library::open_and_import(connection, Path::new(&fixture("sample.pdf")), 1)
            })
            .unwrap();

        index_document(None, &db, &render, &service, &entry.meta.id).unwrap();

        let status = document_status(&db, &entry.meta.id).unwrap().unwrap();
        assert_eq!(status.state, IndexState::Done);
        assert_eq!(status.total, 3);
    }

    #[test]
    #[ignore = "requires a real PDFium library; `cargo test -- --include-ignored` after fetching it"]
    fn re_indexing_the_same_pdf_is_idempotent() {
        let (db, render, service) = setup();
        let entry = db
            .with(|connection| {
                crate::library::open_and_import(connection, Path::new(&fixture("sample.pdf")), 1)
            })
            .unwrap();

        index_document(None, &db, &render, &service, &entry.meta.id).unwrap();
        index_document(None, &db, &render, &service, &entry.meta.id).unwrap();

        // Second run replaced rather than appended.
        let rows = db
            .with(|connection| search::row_count(connection, &entry.meta.id))
            .unwrap();
        assert_eq!(rows, 3);

        let hits = db
            .with(|connection| search::query(connection, "ALPHA", 10))
            .unwrap();
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn text_documents_are_indexed_through_the_text_provider() {
        let (db, render, service) = setup();
        let entry = db
            .with(|connection| {
                crate::library::open_and_import(connection, Path::new(&fixture("sample.txt")), 1)
            })
            .unwrap();
        let doc_id = entry.meta.id.clone();

        let summary = index_document(None, &db, &render, &service, &doc_id).unwrap();
        assert_eq!(summary.pages_indexed, 1);

        // The fixture's contents are searchable without any PDFium involvement.
        let expected = std::fs::read_to_string(fixture("sample.txt")).unwrap();
        let word = expected
            .split_whitespace()
            .next()
            .unwrap()
            .trim_matches(|c: char| !c.is_alphanumeric());
        assert!(
            !word.is_empty(),
            "fixture should contain a word to search for"
        );
        let hits = db
            .with(|connection| search::query(connection, word, 10))
            .unwrap();
        assert_eq!(hits.len(), 1, "expected a hit for {word:?}");
        assert_eq!(hits[0].document_id, doc_id);
    }

    #[test]
    fn needs_indexing_is_false_right_after_a_successful_index() {
        let (db, render, service) = setup();
        let entry = db
            .with(|connection| {
                crate::library::open_and_import(connection, Path::new(&fixture("sample.txt")), 1)
            })
            .unwrap();

        index_document(None, &db, &render, &service, &entry.meta.id).unwrap();
        assert!(!needs_indexing(&db, &entry.meta.id).unwrap());
    }

    #[test]
    fn needs_indexing_is_true_for_a_document_that_was_never_indexed() {
        let (db, _, _) = setup();
        let entry = db
            .with(|connection| {
                crate::library::open_and_import(connection, Path::new(&fixture("sample.txt")), 1)
            })
            .unwrap();

        assert!(needs_indexing(&db, &entry.meta.id).unwrap());
    }

    #[test]
    fn indexing_a_vanished_document_is_a_no_op() {
        let (db, render, service) = setup();
        let summary = index_document(None, &db, &render, &service, "never-seen").unwrap();
        assert_eq!(summary.pages_indexed, 0);
        // The claim is released even though there was nothing to index.
        assert!(!service.is_running("never-seen"));
    }

    #[test]
    fn extraction_of_a_text_page_goes_through_the_provider() {
        let (db, render, _) = setup();
        let entry = db
            .with(|connection| {
                crate::library::open_and_import(connection, Path::new(&fixture("sample.txt")), 1)
            })
            .unwrap();

        let text = extract_page(&render, &entry, None, 0).unwrap();
        assert_eq!(
            text,
            std::fs::read_to_string(fixture("sample.txt")).unwrap()
        );
    }

    #[test]
    fn indexable_covers_exactly_the_supported_formats() {
        let (db, _, _) = setup();
        // Rows are inserted directly because importing would need PDFium for the PDF's
        // metadata, and this test is about the format check, not extraction.
        for format in ["pdf", "txt", "md"] {
            db.with(|connection| {
                connection.execute(
                    "INSERT INTO Document (id, path, format, added_at) VALUES (?1, ?2, ?3, 0)",
                    rusqlite::params![
                        format!("doc-{format}"),
                        format!("C:/books/a.{format}"),
                        format
                    ],
                )?;
                Ok(())
            })
            .unwrap();

            let entry = load_entry(&db, &format!("doc-{format}")).unwrap().unwrap();
            assert!(indexable(&entry), "for {format}");
        }
        assert_eq!(db.with(count_documents).unwrap(), 3);
    }

    /// Pins a P0 behaviour this module has to live with: an unrecognised stored format
    /// is coerced to `Txt` on read (`documents::parse_format`). Import rejects unknown
    /// extensions, so such a row cannot normally exist — but it means `indexable` is not
    /// a strong validation, and a text provider reading a binary file would surface as
    /// `NOT_TEXT` rather than as a skipped document. Recorded so this is not mistaken for
    /// a guarantee.
    #[test]
    fn an_unknown_stored_format_is_coerced_to_text_by_the_p0_reader() {
        let (db, _, _) = setup();
        db.with(|connection| {
            connection.execute(
                "INSERT INTO Document (id, path, format, added_at) VALUES ('d1', 'C:/books/a.epub', 'epub', 0)",
                [],
            )?;
            Ok(())
        })
        .unwrap();

        let entry = load_entry(&db, "d1").unwrap().unwrap();
        assert_eq!(entry.meta.format, Format::Txt);
    }

    fn count_documents(connection: &rusqlite::Connection) -> Result<i64, AppError> {
        Ok(connection.query_row("SELECT count(*) FROM Document", [], |row| row.get(0))?)
    }
}
