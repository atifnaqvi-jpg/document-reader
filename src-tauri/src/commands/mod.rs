use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use tauri::{AppHandle, State};

use crate::db::{annotations, app_state, bookmarks, documents, search, Db};
use crate::editor::session::{PasswordStore, SessionPassword};
use crate::editor::xfdf;
use crate::index::{IndexService, IndexStatus, IndexSummary};
use crate::library;
use crate::model::{
    Annotation, AnnotationKind, AnnotationStatus, AppError, Bookmark, DocumentMeta, Format,
    LibraryEntry, RenderedPage, SearchHit,
};
use crate::provider::ReaderProvider;
use crate::render::RenderService;

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or_default()
}

#[tauri::command]
pub fn open_document(
    app: tauri::AppHandle,
    db: State<'_, Db>,
    service: State<'_, Arc<RenderService>>,
    index_service: State<'_, Arc<IndexService>>,
    path: String,
) -> Result<DocumentMeta, AppError> {
    let (meta, doc_id, path_on_disk) = db.with(|connection| {
        let entry = library::open_and_import(connection, path.as_ref(), now())?;

        // Re-opening is the only moment a document's bytes can change under a stable id,
        // so it is where any cached raster or handle for it must be dropped: otherwise a
        // file edited outside the app would keep serving its previous pages.
        service.forget_document(&entry.meta.id, Path::new(&entry.path));

        // A run already indexing this document is working from content that no longer
        // exists, so it is told to abandon the rest. It cannot be joined by a second run
        // (single writer per document), so the surviving run restarts for the new bytes.
        index_service.invalidate(&entry.meta.id);

        let doc_id = entry.meta.id.clone();
        Ok((entry.meta, doc_id, entry.path))
    })?;

    // Indexing is started here rather than from the viewer, so a document is searchable
    // even if the user opens it and immediately moves to another one.
    spawn_indexing(&app, doc_id, path_on_disk);

    Ok(meta)
}

/// Starts a background index for a document if one is needed and none is running.
///
/// Detached on purpose: the import must not wait for a 500-page extraction to finish.
/// Failures are swallowed here because they are already recorded as an index status,
/// which is what the UI reads.
fn spawn_indexing(app: &tauri::AppHandle, doc_id: String, path: String) {
    use tauri::Manager;

    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        // State is re-fetched from the handle inside the task: `State<'_, T>` borrows the
        // app and cannot be moved into a `'static` worker.
        let db = app.state::<Db>();
        let render = app.state::<Arc<RenderService>>();
        let index_service = app.state::<Arc<IndexService>>();

        // Format is decided by the file itself, so a `.docx` never reaches the indexer.
        let Ok(Some(entry)) = db.with(|connection| crate::db::documents::get(connection, &doc_id))
        else {
            return;
        };
        if !crate::index::app::indexable(&entry) {
            return;
        }
        // A missing file would only produce a page's worth of failures.
        if !Path::new(&path).is_file() {
            return;
        }

        let _ =
            crate::index::app::index_document(Some(&app), &db, &render, &index_service, &doc_id);
    });
}

#[tauri::command]
pub fn list_library(db: State<'_, Db>) -> Result<Vec<LibraryEntry>, AppError> {
    db.with(library::list_library)
}

#[tauri::command]
pub fn search(db: State<'_, Db>, query: String, limit: u32) -> Result<Vec<SearchHit>, AppError> {
    db.with(|connection| search::query(connection, &query, limit))
}

#[tauri::command]
pub fn add_bookmark(
    db: State<'_, Db>,
    document_id: String,
    page: Option<u32>,
    offset: Option<u32>,
    label: Option<String>,
) -> Result<Bookmark, AppError> {
    db.with(|connection| bookmarks::add(connection, &document_id, page, offset, label, now()))
}

#[tauri::command]
pub fn list_bookmarks(db: State<'_, Db>, document_id: String) -> Result<Vec<Bookmark>, AppError> {
    db.with(|connection| bookmarks::list(connection, &document_id))
}

#[tauri::command]
pub fn remove_bookmark(db: State<'_, Db>, bookmark_id: String) -> Result<(), AppError> {
    db.with(|connection| bookmarks::remove(connection, &bookmark_id))
}

#[tauri::command]
pub fn set_app_state(db: State<'_, Db>, key: String, value: String) -> Result<(), AppError> {
    db.with(|connection| app_state::set(connection, &key, &value))
}

/// Reads a value written by [`set_app_state`] (P1-D7).
///
/// P0 could only write state, so the theme was applied on change and lost on restart.
/// A missing key is `None`, not an error: on first run every key is missing.
#[tauri::command]
pub fn get_app_state(db: State<'_, Db>, key: String) -> Result<Option<String>, AppError> {
    db.with(|connection| app_state::get(connection, &key))
}

/// The one-time data-directory migration notice, or `None` when nothing moved (D-R6).
///
/// Read by the Notices surface. It is deliberately a read, not an event: a notice the user
/// can re-open is honest, and the migration itself already happened at startup.
#[tauri::command]
pub fn get_migration_notice(
    notice: State<'_, crate::editor::migrate::MigrationNotice>,
) -> Result<Option<String>, AppError> {
    Ok(notice.get())
}

/// A document's index status, or `None` if it has never been indexed (P1-D6).
#[tauri::command]
pub fn get_index_status(
    db: State<'_, Db>,
    document_id: String,
) -> Result<Option<IndexStatus>, AppError> {
    crate::index::app::document_status(&db, &document_id)
}

/// Indexes a document and waits for it to finish (P1-D6).
///
/// `open_document` already starts indexing, so the UI needs this only for an explicit
/// retry after a failure. It is `async` and returns once the index is complete, which
/// can be seconds for a large document; callers should rely on `index://progress`
/// rather than blocking a UI affordance on it.
#[tauri::command]
pub async fn index_document(
    app: tauri::AppHandle,
    service: State<'_, Arc<RenderService>>,
    index_service: State<'_, Arc<IndexService>>,
    document_id: String,
) -> Result<IndexSummary, AppError> {
    let render = Arc::clone(&service);
    let index_service = Arc::clone(&index_service);

    tauri::async_runtime::spawn_blocking(move || {
        use tauri::Manager;
        let db = app.state::<Db>();
        crate::index::app::index_document(Some(&app), &db, &render, &index_service, &document_id)
    })
    .await
    .map_err(|e| AppError::Other(format!("index task failed: {e}")))?
}

/// Renders one page as PNG bytes plus its pixel size (P1-D2).
///
/// `async` on purpose (P1-D5/D3): rasterizing a page is CPU-bound and can take
/// hundreds of milliseconds, so it runs on a blocking worker rather than on the IPC
/// thread, which would freeze every other command — including the UI's own polling —
/// for the duration of the render.
#[tauri::command]
pub async fn get_document_page_render(
    db: State<'_, Db>,
    service: State<'_, Arc<RenderService>>,
    doc_id: String,
    page: u32,
    scale: f32,
    annotations: Option<bool>,
) -> Result<Arc<RenderedPage>, AppError> {
    // P2-D9: default is the annotated view; the "original" view passes `false`.
    let annotations = annotations.unwrap_or(true);

    // Resolve what the worker needs while we still have the `State` borrow. Doing this
    // up front means the closure only owns plain data (`State` is not `'static`), and
    // the `Db` mutex is never held across the render itself.
    let (path, content_hash) = document_location(&db, &doc_id)?;

    // Supersede any pre-render still running for a previous page request, so rapid page
    // turns do not stack up speculative work (P1-D4).
    let ticket = service.begin_pre_render();

    // Clone what each blocking task needs before the first closure takes ownership;
    // `path`/`content_hash` are shared with the pre-render task below.
    let render_service = Arc::clone(&service);
    let prerender_service = Arc::clone(&service);
    let render_path = path.clone();
    let render_hash = content_hash.clone();
    let render_doc_id = doc_id.clone();
    let rendered = tauri::async_runtime::spawn_blocking(move || {
        render_service.render(
            &render_doc_id,
            Path::new(&render_path),
            render_hash.as_deref(),
            page,
            scale,
            annotations,
        )
    })
    .await
    .map_err(|e| AppError::Other(format!("render task failed: {e}")))??;

    // Pre-render ±1 page after the requested page is in hand (P1-D4), so the request
    // the user is waiting on is never queued behind speculative work. Detached on
    // purpose: the command returns as soon as the page it was asked for is ready.
    tauri::async_runtime::spawn_blocking(move || {
        prerender_service.prerender_neighbours(
            ticket,
            &doc_id,
            Path::new(&path),
            content_hash.as_deref(),
            page,
            scale,
            annotations,
        );
    });

    Ok(rendered)
}

/// Resolves a document's on-disk location for a render: `(path, content_hash)`.
/// The hash is part of both cache keys, so it is required, not optional.
fn document_location(
    db: &State<'_, Db>,
    doc_id: &str,
) -> Result<(String, Option<String>), AppError> {
    db.with(|connection| {
        let entry = crate::db::documents::get(connection, doc_id)?
            .ok_or_else(|| AppError::Other("document not found".to_string()))?;

        if entry.meta.format != Format::Pdf {
            // Only PDF is renderable in P1 (PDFium); other formats have no page raster.
            return Err(AppError::RenderUnavailable);
        }
        if entry.missing {
            return Err(AppError::FileNotFound(entry.path));
        }

        let hash = crate::db::documents::content_hash(connection, doc_id)?;
        Ok((entry.path, hash))
    })
}

/// Page count as PDFium sees it, which is authoritative when the P0 import-time count
/// was unavailable (and the viewer needs it to bound navigation and pre-render).
#[tauri::command]
pub async fn get_document_page_count(
    db: State<'_, Db>,
    service: State<'_, Arc<RenderService>>,
    doc_id: String,
) -> Result<u32, AppError> {
    let (path, content_hash) = document_location(&db, &doc_id)?;

    let service = Arc::clone(&service);
    tauri::async_runtime::spawn_blocking(move || {
        service.page_count(Path::new(&path), content_hash.as_deref())
    })
    .await
    .map_err(|e| AppError::Other(format!("page count task failed: {e}")))?
}

/// Extracts one page's text layer. Text extraction is also PDFium work, so it takes
/// the same blocking-worker path (this feeds the P1.6 indexer).
///
/// Unlike rendering, this accepts every supported format: a PDF has a per-page text
/// layer, while a `.txt`/`.md` file has no pagination and yields the whole file.
#[tauri::command]
pub async fn get_document_page_text(
    db: State<'_, Db>,
    service: State<'_, Arc<RenderService>>,
    doc_id: String,
    page: u32,
) -> Result<String, AppError> {
    let (path, content_hash, format) = db.with(|connection| {
        let entry = crate::db::documents::get(connection, &doc_id)?
            .ok_or_else(|| AppError::Other("document not found".to_string()))?;
        let hash = crate::db::documents::content_hash(connection, &doc_id)?;
        Ok((entry.path, hash, entry.meta.format))
    })?;

    let service = Arc::clone(&service);
    tauri::async_runtime::spawn_blocking(move || match format {
        Format::Pdf => service.extract_text(Path::new(&path), content_hash.as_deref(), page),
        // Text formats: the page number is ignored by the provider; there is one page.
        _ => crate::provider::TextProvider.extract_text(
            &LibraryEntry {
                meta: DocumentMeta {
                    id: doc_id,
                    title: None,
                    author: None,
                    pages: None,
                    format,
                    encrypted: false,
                },
                path,
                missing: false,
                added_at: 0,
                last_opened_at: None,
            },
            page,
        ),
    })
    .await
    .map_err(|e| AppError::Other(format!("text task failed: {e}")))?
}

// --- P2: annotations ---------------------------------------------------------
//
// These commands are the D1 seam: they resolve paths and translate arguments, and
// every decision about PDF structure lives in `crate::editor`. Keeping that split
// means the write path is testable without a Tauri app.

/// Creates an annotation and, in the same call, its FTS index row (P2-D5).
#[tauri::command]
pub fn add_annotation(
    app: AppHandle,
    db: State<'_, Db>,
    document_id: String,
    page: u32,
    kind: String,
    data: String,
    text: Option<String>,
) -> Result<Annotation, AppError> {
    let kind = AnnotationKind::parse(&kind)
        .ok_or_else(|| AppError::Other(format!("unknown annotation kind: {kind}")))?;
    let annotation = db.with_mut(|connection| {
        annotations::add(connection, &document_id, page, kind, data, text, now())
    })?;

    announce_change(app, document_id);
    Ok(annotation)
}

#[tauri::command]
pub fn update_annotation(
    app: AppHandle,
    db: State<'_, Db>,
    annotation_id: String,
    data: String,
    text: Option<String>,
) -> Result<Annotation, AppError> {
    // Resolve the owning document before mutating: the event needs it, and asking the DB
    // (rather than trusting a frontend-supplied id) is what keeps the payload honest.
    let document_id =
        db.with(|connection| annotations::document_id_for(connection, &annotation_id))?;

    let annotation = db.with_mut(|connection| {
        annotations::update(connection, &annotation_id, data, text, now())
    })?;

    announce_change(app, document_id);
    Ok(annotation)
}

/// Sets an annotation's review status (D-A7), rewriting only the common `status` key.
///
/// A dedicated command rather than an `update_annotation` call, because that one replaces
/// the whole blob: the comment pane holds one key of it, and letting a status click send a
/// blob assembled from a stale render is how a concurrent edit gets overwritten. The blob is
/// read, one key is replaced, and the result goes back through the same update path so the
/// FTS index and `modified_at` stay correct.
#[tauri::command]
pub fn set_annotation_status(
    app: AppHandle,
    db: State<'_, Db>,
    annotation_id: String,
    status: Option<String>,
) -> Result<Annotation, AppError> {
    let status = match status.as_deref() {
        None => None,
        Some(value) => Some(
            AnnotationStatus::parse(value)
                .ok_or_else(|| AppError::Other(format!("unknown annotation status: {value}")))?,
        ),
    };

    let document_id =
        db.with(|connection| annotations::document_id_for(connection, &annotation_id))?;
    let annotation = db.with_mut(|connection| {
        annotations::set_status(connection, &annotation_id, status, now())
    })?;
    announce_change(app, document_id);
    Ok(annotation)
}

/// Exports a document's annotations as XFDF (P3.1 D-A9).
///
/// Returns the file contents plus the count of rows that had no XFDF spelling. The count is
/// returned rather than logged because export is where a user would otherwise be surprised:
/// they would save, re-import, and find one annotation missing with no explanation.
#[tauri::command]
pub fn export_xfdf(db: State<'_, Db>, document_id: String) -> Result<XfdfExport, AppError> {
    let rows = db.with(|connection| annotations::list_for_document(connection, &document_id))?;
    let (contents, unsupported) = xfdf::export(&rows)?;
    Ok(XfdfExport {
        contents,
        exported: rows.len() - unsupported,
        unsupported,
    })
}

/// Writes an XFDF export to a path the user chose.
///
/// The path comes from the frontend's save dialog rather than from the command, so the command
/// cannot invent a destination; and it is written here rather than through a webview download
/// because a blob download in a desktop webview has no destination the user picked.
#[tauri::command]
pub fn save_xfdf(contents: String, path: String) -> Result<(), AppError> {
    std::fs::write(&path, contents)
        .map_err(|error| AppError::Other(format!("could not write {path}: {error}")))
}

/// What an XFDF export produced.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct XfdfExport {
    pub contents: String,
    pub exported: usize,
    pub unsupported: usize,
}

/// Imports an XFDF document into a document's annotations (P3.1 D-A9/D-A10).
///
/// Nothing is overwritten: a parsed annotation whose name already exists is skipped and
/// counted (D-A10), and the report goes back to the caller so the user is told what happened.
#[tauri::command]
pub fn import_xfdf(
    app: AppHandle,
    db: State<'_, Db>,
    document_id: String,
    contents: String,
) -> Result<xfdf::ImportReport, AppError> {
    let (parsed, report) = xfdf::parse(&contents)?;

    let existing: std::collections::HashSet<String> = db.with(|connection| {
        Ok(annotations::list_for_document(connection, &document_id)?
            .into_iter()
            .map(|annotation| annotation.id)
            .collect())
    })?;

    // Decide first, write second: `merge` answers "which of these are already here", and the
    // surviving rows go in through one transaction.
    let stamp = now();
    let mut to_insert = Vec::new();
    let report = xfdf::merge(parsed, &existing, report, |annotation| {
        to_insert.push((
            annotation.page,
            annotation.kind,
            xfdf::blob_for(annotation).to_string(),
            annotation.contents.clone(),
        ));
        Ok(())
    })?;
    db.with_mut(|connection| annotations::add_many(connection, &document_id, to_insert, stamp))?;

    if report.imported > 0 {
        announce_change(app, document_id);
    }
    Ok(report)
}

#[tauri::command]
pub fn list_annotations(
    db: State<'_, Db>,
    document_id: String,
) -> Result<Vec<Annotation>, AppError> {
    db.with(|connection| annotations::list_for_document(connection, &document_id))
}

/// Lists annotations for a single page, which is what the viewer overlay needs.
#[tauri::command]
pub fn list_page_annotations(
    db: State<'_, Db>,
    document_id: String,
    page: u32,
) -> Result<Vec<Annotation>, AppError> {
    db.with(|connection| annotations::list_for_page(connection, &document_id, page))
}

#[tauri::command]
pub fn remove_annotation(
    app: AppHandle,
    db: State<'_, Db>,
    annotation_id: String,
) -> Result<(), AppError> {
    let document_id = remove_annotation_and_gc(&db, &annotation_id)?;
    announce_change(app, document_id);
    Ok(())
}

/// Deletes an annotation and applies the P2-D12 edit-copy GC, returning the owning
/// document id.
///
/// Split out of the command because the caller needs the id for the change event and it no
/// longer exists once the row is gone -- and because this path deletes a user-visible file,
/// so it has to be testable without a Tauri app.
pub fn remove_annotation_and_gc(db: &Db, annotation_id: &str) -> Result<String, AppError> {
    let document_id =
        db.with(|connection| annotations::document_id_for(connection, annotation_id))?;

    db.with_mut(|connection| annotations::remove(connection, annotation_id))?;

    // P2-D12: the edit copy's reason to exist has just gone away.
    collect_edit_copies_if_unannotated(db, &document_id);
    Ok(document_id)
}

/// Resolves a document to its path and runs the P2-D12 edit-copy GC if it now has no
/// annotations left.
///
/// Consulted after the delete commits, because "the last annotation is gone" describes the
/// new state, not the old. Best-effort: failing to tidy a stale copy must never turn a
/// successful delete into an error the user sees.
pub fn collect_edit_copies_if_unannotated(db: &Db, document_id: &str) {
    let Ok(remaining) =
        db.with(|connection| annotations::count_for_document(connection, document_id))
    else {
        return;
    };
    if remaining > 0 {
        return;
    }
    let Ok(Some(entry)) = db.with(|connection| documents::get(connection, document_id)) else {
        return;
    };
    let _ = crate::editor::gc::collect_edit_copies(Path::new(&entry.path));
}

/// Overwrites the original file with the document's annotations, in place (P2-D2).
///
/// Deliberately a *separate* command from `save_edits`, and separately named, so it cannot
/// be reached by anything that means "save": the ordinary save path must never be able to
/// destroy the user's source document. The frontend confirms with the user first.
///
/// Returns the path written, which is always the original.
#[tauri::command]
pub fn overwrite_edits(db: State<'_, Db>, document_id: String) -> Result<String, AppError> {
    let (original_path, rows) = db.with(|connection| {
        let path = documents::get(connection, &document_id)?
            .map(|entry| entry.path)
            .ok_or_else(|| AppError::Other(format!("document not found: {document_id}")))?;
        Ok((
            path,
            annotations::list_for_document(connection, &document_id)?,
        ))
    })?;

    let source = std::fs::read(&original_path)
        .map_err(|error| AppError::Other(format!("could not read {original_path}: {error}")))?;
    let edited = crate::editor::annotate::apply_annotations(&source, &rows)?;

    // Through the same atomic temp-file-and-rename path as a save: a half-written original
    // is the one outcome worse than not overwriting at all.
    crate::editor::save::write_atomically(Path::new(&original_path), &edited)?;

    Ok(original_path)
}

/// Resolves a document id to its path on disk.
///
/// Every write command starts here rather than taking a path from the frontend:
/// the frontend's copy could be stale, and an edit written to a path that no longer
/// matches the document is how a user's changes end up in the wrong file.
fn document_path(db: &Db, document_id: &str) -> Result<String, AppError> {
    db.with(|connection| {
        documents::get(connection, document_id)?
            .map(|entry| entry.path)
            .ok_or_else(|| AppError::Other(format!("document not found: {document_id}")))
    })
}

/// Where a "save edits" action would write, so the UI can confirm the filename
/// before the user commits (P2-D2). Never returns the original path.
#[tauri::command]
pub fn next_save_path(db: State<'_, Db>, document_id: String) -> Result<String, AppError> {
    let path = document_path(&db, &document_id)?;
    let next =
        crate::editor::save::next_edited_path(Path::new(&path), |candidate| candidate.exists())?;
    Ok(next.to_string_lossy().to_string())
}

/// Saves the document's annotations into a **new** file beside the original (P2-D2).
///
/// Returns the path written. The original file is never touched: it is opened
/// read-only, and the result goes to a versioned name chosen by
/// [`next_edited_path`]. An explicit overwrite is a separate, differently-named
/// command precisely so it cannot be reached by a path that means "save".
#[tauri::command]
pub fn save_edits(db: State<'_, Db>, document_id: String) -> Result<String, AppError> {
    let (original_path, rows) = db.with(|connection| {
        let path = documents::get(connection, &document_id)?
            .map(|entry| entry.path)
            .ok_or_else(|| AppError::Other(format!("document not found: {document_id}")))?;
        Ok((
            path,
            annotations::list_for_document(connection, &document_id)?,
        ))
    })?;

    let source = std::fs::read(&original_path)
        .map_err(|error| AppError::Other(format!("could not read {original_path}: {error}")))?;

    // Written from the original bytes, not from whatever the viewer last
    // rendered: rebuilding from source is idempotent, so a second save cannot
    // stack duplicate annotations on top of the first.
    let edited = crate::editor::annotate::apply_annotations(&source, &rows)?;

    let target = crate::editor::save::next_edited_path(Path::new(&original_path), |candidate| {
        candidate.exists()
    })?;
    crate::editor::save::write_atomically(&target, &edited)?;

    // No cache invalidation here on purpose: writes go to a *new* path, so cached
    // rasters of the original stay correct. Opening the edited file routes through
    // `open_document`, which invalidates by path (P2-D6).
    Ok(target.to_string_lossy().to_string())
}

/// Re-inserts a deleted annotation under its original id, for undo (P2-D7).
///
/// Takes the whole row rather than rebuilding it from the current DB state: the row is
/// gone, so the caller's captured copy is the only record of its id and timestamps.
#[tauri::command]
pub fn restore_annotation(
    app: AppHandle,
    db: State<'_, Db>,
    annotation: Annotation,
) -> Result<(), AppError> {
    let document_id = annotation.document_id.clone();
    db.with_mut(|connection| annotations::restore(connection, &annotation))?;

    announce_change(app, document_id);
    Ok(())
}

/// Fires the debounced annotation-change event (P2-D11) off the command's critical path.
///
/// Detached on purpose: the debounce sleeps for 200ms, and making a user wait that long
/// after every annotation would be a visible stall. The write has already committed, so
/// nothing depends on the emission completing.
fn announce_change(app: AppHandle, document_id: String) {
    tauri::async_runtime::spawn_blocking(move || {
        crate::editor::events::emit_after_debounce(app, document_id);
    });
}

// --- P2: page structure, export, watermark ------------------------------------
//
// Every command here follows one shape: load the original with lopdf, transform a copy,
// and write the result to a *new* file. None of them replace the original, for the same
// reason `save_edits` does not -- the source document belongs to the user, and the only
// command permitted to replace it is the separately-confirmed `overwrite_edits`.

/// Rotates one page clockwise by a multiple of 90 degrees (P2-F5).
///
/// Returns the resulting rotation rather than echoing the request: `/Rotate` accumulates,
/// so a second 90-degree turn gives 180, and the UI should show that.
#[tauri::command]
pub fn rotate_page(
    db: State<'_, Db>,
    document_id: String,
    page: u32,
    degrees: i64,
) -> Result<RotateResult, AppError> {
    let mut document = load_document(&db, &document_id)?;
    let page_id = page_object_id(&document, page)?;

    let rotation = crate::editor::pages::rotate_page(&mut document, page_id, degrees)?;
    let path = write_derived(&db, &document_id, "rotated", &serialize(&document)?)?;
    Ok(RotateResult { path, rotation })
}

/// A rotation's outcome: where it was written, and the page's resulting angle.
///
/// Both are needed. `/Rotate` accumulates, so the angle is not the one the caller asked
/// for, and the file it landed in is not the original.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RotateResult {
    pub path: String,
    pub rotation: i64,
}

/// Deletes pages, numbered as the UI shows them (1-based).
#[tauri::command]
pub fn delete_pages(
    db: State<'_, Db>,
    document_id: String,
    pages: Vec<u32>,
) -> Result<String, AppError> {
    let mut document = load_document(&db, &document_id)?;

    crate::editor::pages::delete_pages(&mut document, &pages)?;
    write_derived(&db, &document_id, "pages removed", &serialize(&document)?)
}

/// Moves a page to a new 1-based position (P2-F3 reorder).
#[tauri::command]
pub fn move_page(
    db: State<'_, Db>,
    document_id: String,
    from: u32,
    to: u32,
) -> Result<String, AppError> {
    let mut document = load_document(&db, &document_id)?;

    crate::editor::pages::move_page(&mut document, from, to)?;
    write_derived(&db, &document_id, "reordered", &serialize(&document)?)
}

/// Writes just the selected pages to a new file (P2-F3 extract).
#[tauri::command]
pub fn extract_pages(
    db: State<'_, Db>,
    document_id: String,
    pages: Vec<u32>,
) -> Result<String, AppError> {
    let document = load_document(&db, &document_id)?;

    let extracted = crate::editor::pages::extract_pages(&document, &pages)?;
    write_derived(&db, &document_id, "extracted", &serialize(&extracted)?)
}

/// Splits the document in two at `at` (1-based) and returns both written paths.
#[tauri::command]
pub fn split_document(
    db: State<'_, Db>,
    document_id: String,
    at: u32,
) -> Result<Vec<String>, AppError> {
    let document = load_document(&db, &document_id)?;

    let (first, second) = crate::editor::pages::split_at(&document, at)?;
    let first_path = write_derived(&db, &document_id, "part 1", &serialize(&first)?)?;
    let second_path = write_derived(&db, &document_id, "part 2", &serialize(&second)?)?;

    Ok(vec![first_path, second_path])
}

/// Flattens annotations into a new file, optionally watermarking it (P2-F2/F5).
///
/// One-way by design: the annotation objects do not survive, which is what makes the result
/// look right in viewers that ignore annotations. That is also why it is an export rather
/// than a save.
#[tauri::command]
pub fn export_flattened(
    db: State<'_, Db>,
    document_id: String,
    watermark: Option<String>,
) -> Result<String, AppError> {
    let source = load_original(&db, &document_id)?;
    // The rows are the source of truth for what to bake: the writer stores geometry and
    // colour but emits no appearance stream, so flattening has to redraw from here.
    let rows = db.with(|connection| annotations::list_for_document(connection, &document_id))?;
    let (bytes, _flattened) =
        crate::editor::export::export_with_flatten(&source, &rows, watermark.as_deref())?;

    write_derived(&db, &document_id, "flattened", &bytes)
}

/// Stamps a watermark across every page, into a new file (P2-F5).
#[tauri::command]
pub fn add_watermark(
    db: State<'_, Db>,
    document_id: String,
    text: String,
) -> Result<String, AppError> {
    let mut document = load_document(&db, &document_id)?;

    crate::editor::export::watermark(&mut document, &text)?;
    write_derived(&db, &document_id, "watermarked", &serialize(&document)?)
}

/// Renders one page to a PNG, base64-encoded, for the clipboard (P2-F2).
#[tauri::command]
pub async fn export_page_png(
    db: State<'_, Db>,
    document_id: String,
    page: u32,
    scale: f32,
) -> Result<String, AppError> {
    let path = document_path(&db, &document_id)?;

    // Off the async runtime: rasterizing blocks, and a large page would stall every other
    // command sharing the thread.
    let png = tauri::async_runtime::spawn_blocking(move || {
        crate::editor::export::page_png(Path::new(&path), page, scale)
    })
    .await
    .map_err(|error| AppError::Other(format!("render task failed: {error}")))??;

    use base64::Engine as _;
    Ok(base64::engine::general_purpose::STANDARD.encode(png))
}

/// Reads the original document's bytes.
fn load_original(db: &Db, document_id: &str) -> Result<Vec<u8>, AppError> {
    let path = document_path(db, document_id)?;
    std::fs::read(&path).map_err(|error| AppError::Other(format!("could not read {path}: {error}")))
}

/// Parses the original with lopdf, for the page-structure operations.
fn load_document(db: &Db, document_id: &str) -> Result<lopdf::Document, AppError> {
    let source = load_original(db, document_id)?;
    lopdf::Document::load_mem(&source)
        .map_err(|error| AppError::Other(format!("could not read PDF: {error}")))
}

/// Resolves a 1-based page number to its object id, as the UI counts pages.
fn page_object_id(document: &lopdf::Document, page: u32) -> Result<lopdf::ObjectId, AppError> {
    if page == 0 {
        return Err(AppError::Other("pages are numbered from 1".to_string()));
    }
    document.get_pages().get(&page).copied().ok_or_else(|| {
        AppError::Other(format!(
            "page {page} is out of range (document has {} pages)",
            document.get_pages().len()
        ))
    })
}

/// Serialises a transformed document, renumbering and compressing so the output is compact
/// and free of the gaps the page operations leave behind.
fn serialize(document: &lopdf::Document) -> Result<Vec<u8>, AppError> {
    let mut copy = document.clone();
    copy.renumber_objects();
    copy.compress();

    let mut bytes = Vec::new();
    copy.save_to(&mut bytes)
        .map_err(|error| AppError::Other(format!("could not write PDF: {error}")))?;
    Ok(bytes)
}

/// Writes transformed bytes beside the original and returns the path written.
fn write_derived(
    db: &Db,
    document_id: &str,
    label: &str,
    bytes: &[u8],
) -> Result<String, AppError> {
    let original_path = document_path(db, document_id)?;
    let target = next_derived_path(Path::new(&original_path), label)?;
    crate::editor::save::write_atomically(&target, bytes)?;
    Ok(target.to_string_lossy().to_string())
}

/// The next free `"{stem} ({label} {n}).{ext}"` path beside the original.
///
/// Shares `next_edited_path`'s never-clobber rule but deliberately not its `(edited N)`
/// naming: a rotated or split file is not an *edit* of the original in the annotation sense,
/// and borrowing that name would mislead both the user and the D12 GC, which treats
/// `(edited N)` files as the ones it owns and may delete.
pub fn next_derived_path(original: &Path, label: &str) -> Result<std::path::PathBuf, AppError> {
    let parent = original
        .parent()
        .ok_or_else(|| AppError::Other("document has no parent directory".to_string()))?;
    let stem = original
        .file_stem()
        .and_then(|stem| stem.to_str())
        .ok_or_else(|| AppError::Other("document name is not valid utf-8".to_string()))?;
    let extension = original
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or("pdf");

    for number in 1..=9_999_u32 {
        let candidate = parent.join(format!("{stem} ({label} {number}).{extension}"));
        if !candidate.exists() {
            return Ok(candidate);
        }
    }

    Err(AppError::Other(
        "reached the limit of derived copies for this document".to_string(),
    ))
}

// --- P2.4: content-stream text editing ----------------------------------------
//
// Three commands, each a thin seam over `crate::editor` (P2-D1): find the runs the
// picker can offer, apply one edit, and produce an unprotected copy. The editing rules
// (gates, run addressing, encryption policy) all live in the editor modules, so this
// file only resolves paths, measures with PDFium where the design says PDFium measures,
// and persists through the D2 save discipline.

/// A page's text runs, with the Stage-1 verdict precomputed (P2.4 §4/T3/T4).
///
/// The run **addresses** and the admissibility gates come from lopdf; the geometry and
/// width come from PDFium, read-only. Both parses happen here so the picker never has to
/// reconcile them itself, and so "can this run be edited" is answered before the user
/// types rather than discovered on commit.
#[tauri::command]
pub async fn get_page_text_runs(
    db: State<'_, Db>,
    service: State<'_, Arc<RenderService>>,
    document_id: String,
    page: u32,
) -> Result<Vec<crate::editor::textedit::PageTextRun>, AppError> {
    let (path, content_hash) = db.with(|connection| {
        let entry = documents::get(connection, &document_id)?
            .ok_or_else(|| AppError::Other(format!("document not found: {document_id}")))?;
        let hash = documents::content_hash(connection, &document_id)?;
        Ok::<_, AppError>((entry.path, hash))
    })?;

    // A protected document still opens for reading, so the picker is allowed to *show*
    // its runs; the edit command is where the read-only refusal belongs (T7). Asking for
    // runs here therefore does not prompt for a password.

    // PDFium work (runs + measurement) is CPU-bound; run it off the IPC thread, like
    // every other extraction.
    let service = Arc::clone(&service);
    let pdfium_path = path.clone();
    let pdfium_hash = content_hash.clone();
    // The char grid (T23a) and the text objects (T17) come from the same PDFium document in one
    // pass, so a run's span is measured against the very layout its box came from.
    let (pdfium_runs, grid) = tauri::async_runtime::spawn_blocking(move || {
        let runs = service.page_runs(Path::new(&pdfium_path), pdfium_hash.as_deref(), page)?;
        let grid = service.char_grid(Path::new(&pdfium_path), pdfium_hash.as_deref(), page)?;
        Ok::<_, AppError>((runs, grid))
    })
    .await
    .map_err(|error| AppError::Other(format!("run extraction task failed: {error}")))??;

    // lopdf parse and gate evaluation are cheap by comparison (bounded by the T3e cap)
    // but still not IPC-thread work.
    tauri::async_runtime::spawn_blocking(move || {
        let bytes = std::fs::read(&path)
            .map_err(|error| AppError::Other(format!("could not read {path}: {error}")))?;
        let document = lopdf::Document::load_mem(&bytes)
            .map_err(|error| AppError::Other(format!("could not read PDF: {error}")))?;
        crate::editor::textedit::describe_runs(&document, page, &pdfium_runs, &grid)
    })
    .await
    .map_err(|error| AppError::Other(format!("run description task failed: {error}")))?
}

/// The result of a successful text edit.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TextEditResult {
    /// Where the edited copy was written. Never the original (D2).
    pub path: String,
    /// The inverse operation, for the session undo stack (T5).
    pub inverse: crate::editor::textlocate::ContentOperation,
    /// The replacement as it was actually written (through the font's own encoding).
    pub text: String,
    /// The standard-14 `/BaseFont` MECH-A substituted, when it fired (T10). `None` for an
    /// edit that kept the run's own font.
    pub substitution: Option<String>,
    /// The disclosure sentence shown to the user when MECH-A fired (T10). Substitution is
    /// never silent; when this is `Some`, the edit changed the document's typeface.
    pub note: Option<String>,
    /// False when the edited run sat under an `/ActualText` this edit could not update
    /// (P2.7.1 T30): the page now renders the new text while the document's accessible text
    /// still states the old one.
    pub actual_text_preserved: bool,
}

/// Applies a Stage-2 in-place text edit and saves it to a **new** file
/// (P2.4 T2/T3/T5/T6/T10/T11/T12/T13).
///
/// The original is opened read-only and never a write target; the result goes through
/// [`crate::editor::save::next_edited_path`] like every other save (D2), so a text edit
/// cannot clobber the user's source document.
///
/// `allow_substitution` is the O-ST2-4 toggle. When it is false, a run whose font cannot
/// set the replacement refuses (`SUBSTITUTE_REFUSED`) instead of being switched to a
/// standard-14 font. It defaults to `true` at the UI, and is explicit on the wire so the
/// backend never has to guess the user's intent.
#[tauri::command]
// The run's font name and size travel with the request so the width gate measures with
// the same metrics the picker showed the user (T3d); folding them into a struct would
// only move the argument count, not reduce it.
#[allow(clippy::too_many_arguments)]
pub async fn edit_page_text(
    app: AppHandle,
    db: State<'_, Db>,
    service: State<'_, Arc<RenderService>>,
    document_id: String,
    page: u32,
    run_id: crate::editor::textlocate::RunId,
    expected: String,
    replacement: String,
    old_width: f32,
    font_name: String,
    font_size: f32,
    allow_substitution: bool,
    fragments: Vec<crate::editor::textedit::FragmentRef>,
    range: Option<Vec<crate::editor::textedit::FragmentRef>>,
) -> Result<TextEditResult, AppError> {
    let path = document_path(&db, &document_id)?;

    // T7: the single read-only gate. A protected document refuses every edit here, before
    // any other work, and never prompts for a password.
    crate::editor::protect::read_only_guard(Path::new(&path))?;

    let content_hash = db.with(|connection| documents::content_hash(connection, &document_id))?;

    let service = Arc::clone(&service);
    let measure_path = path.clone();
    let target_path = path.clone();
    let target_page = page;
    let written = replacement.clone();

    let (outcome, bytes) = tauri::async_runtime::spawn_blocking(move || {
        // PDFium measures through the shared handle cache, read-only (T1/T3d). Used only
        // for the keep-original path: when MECH-A substitutes, the width gate measures the
        // substitute's AFM metrics instead (T13), so PDFium is never asked about a font it
        // does not have.
        let measure = |text: &str| -> Result<f32, AppError> {
            service.measure_text(
                Path::new(&measure_path),
                content_hash.as_deref(),
                target_page,
                &font_name,
                font_size,
                text,
            )
        };

        let source = std::fs::read(&target_path)
            .map_err(|error| AppError::Other(format!("could not read {target_path}: {error}")))?;
        let mut document = lopdf::Document::load_mem(&source)
            .map_err(|error| AppError::Other(format!("could not read PDF: {error}")))?;

        let request = crate::editor::textedit::EditRequest {
            page: target_page,
            run_id,
            expected,
            replacement: written,
            old_width,
            font_size,
            allow_substitution,
            fragments,
            range: range.unwrap_or_default(),
            measure: &measure,
        };
        let outcome = crate::editor::textedit::edit(&mut document, &request)?;

        // Same serialisation discipline as every other lopdf write: renumber and compress
        // so the result is compact for a PDF reader (not a debugging aid).
        document.renumber_objects();
        document.compress();
        let mut bytes = Vec::new();
        document
            .save_to(&mut bytes)
            .map_err(|error| AppError::Other(format!("could not write PDF: {error}")))?;
        Ok::<_, AppError>((outcome, bytes))
    })
    .await
    .map_err(|error| AppError::Other(format!("text edit task failed: {error}")))??;

    let target =
        crate::editor::save::next_edited_path(Path::new(&path), |candidate| candidate.exists())?;
    crate::editor::save::write_atomically(&target, &bytes)?;

    // T6 / R5: the edit invalidated derived state. The write went to a *new* path, so the
    // original's rasters and index are still correct; the copy is the document whose
    // derived state must be built, and it is imported through the ordinary library route
    // so it gets its own id, rasters and FTS5 rows exactly like a freshly opened file.
    // The import re-extracts the page text and replaces the copy's stored rows with it,
    // which is why a search must find the *new* text and not the pre-edit text: the FTS5
    // rows come from a fresh PDFium extraction of the bytes just written, never from the
    // old rows and never from the editor's own string.
    let edited_path = target.to_string_lossy().to_string();
    announce_change(app.clone(), document_id.clone());
    spawn_import_and_index(&app, edited_path.clone());

    Ok(TextEditResult {
        path: edited_path,
        inverse: outcome.inverse,
        text: replacement,
        substitution: outcome.substitution,
        note: outcome.note,
        actual_text_preserved: outcome.actual_text_preserved,
    })
}

/// Imports an edited copy and indexes it, off the command's critical path (T6).
///
/// Re-importing through `library::open_and_import` rather than hand-writing FTS5 rows is
/// what keeps the copy's index and metadata consistent with every other document: the
/// dedupe rule, the render-cache invalidation by path, and the index service's single
/// writer per document are all the existing behaviours, not a second implementation.
///
/// Detached because a 500-page re-extract must not make the edit feel slow, exactly like
/// indexing on open. The change event already told every pane to re-read, so nothing waits
/// on this.
fn spawn_import_and_index(app: &AppHandle, path: String) {
    use tauri::Manager;

    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let db = app.state::<Db>();
        let render = app.state::<Arc<RenderService>>();
        let index_service = app.state::<Arc<IndexService>>();

        let Ok(entry) = db.with(|connection| {
            let entry = crate::library::open_and_import(connection, Path::new(&path), now())?;
            Ok::<_, AppError>(entry)
        }) else {
            // A failure here is a derived-state problem, never a data problem: the edited
            // file is already written and correct. The change event has fired, and the
            // next open of the copy will retry the import through the same route.
            return;
        };

        // The new copy's path cannot be in either cache under its id yet, but the *path*
        // may be if the user re-edits an already-open copy; dropping it here is cheap and
        // guarantees the first render of the copy reads its new bytes.
        render.forget_document(&entry.meta.id, Path::new(&path));

        if crate::index::app::indexable(&entry) {
            index_service.invalidate(&entry.meta.id);
            let _ = crate::index::app::index_document(
                Some(&app),
                &db,
                &render,
                &index_service,
                &entry.meta.id,
            );
        }
    });
}

/// Reverses one text edit, writing another new file (P2.4 T5).
///
/// The inverse records the stream address and both byte strings, so the undo is exact
/// rather than a re-render from the original: an edit made after this one is not lost.
/// `edited_path` is the file the edit wrote, because that is where the bytes the inverse
/// describes currently live.
#[tauri::command]
pub async fn revert_page_text(
    app: AppHandle,
    document_id: String,
    edited_path: String,
    operation: crate::editor::textlocate::ContentOperation,
) -> Result<String, AppError> {
    let path = edited_path.clone();

    let bytes = tauri::async_runtime::spawn_blocking(move || {
        let source = std::fs::read(&path)
            .map_err(|error| AppError::Other(format!("could not read {path}: {error}")))?;
        let mut document = lopdf::Document::load_mem(&source)
            .map_err(|error| AppError::Other(format!("could not read PDF: {error}")))?;

        crate::editor::textedit::revert(&mut document, &operation)?;

        document.renumber_objects();
        document.compress();
        let mut bytes = Vec::new();
        document
            .save_to(&mut bytes)
            .map_err(|error| AppError::Other(format!("could not write PDF: {error}")))?;
        Ok::<_, AppError>(bytes)
    })
    .await
    .map_err(|error| AppError::Other(format!("revert task failed: {error}")))??;

    let target = crate::editor::save::next_edited_path(Path::new(&edited_path), |candidate| {
        candidate.exists()
    })?;
    crate::editor::save::write_atomically(&target, &bytes)?;

    let undo_target = target.to_string_lossy().to_string();
    announce_change(app.clone(), document_id);
    spawn_import_and_index(&app, undo_target.clone());
    Ok(undo_target)
}

/// Produces an unprotected copy of a password-protected document (P2.4 T8).
///
/// The original is opened read-only and never written. The password is used for this call
/// and, on success, retained for the session only (T10) so a subsequent edit of the copy
/// can reuse it without a second prompt; it is never stored on disk, in SQLite, in a log,
/// or in an event payload. On any failure the partial copy is deleted and the command
/// refuses.
#[tauri::command]
pub async fn decrypt_copy(
    db: State<'_, Db>,
    passwords: State<'_, PasswordStore>,
    document_id: String,
    password: String,
) -> Result<crate::editor::protect::DecryptOutcome, AppError> {
    let path = document_path(&db, &document_id)?;

    let outcome_path = path.clone();
    let supplied = password.clone();
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        crate::editor::protect::decrypt_copy(Path::new(&outcome_path), &supplied)
    })
    .await
    .map_err(|error| AppError::Other(format!("decrypt task failed: {error}")))??;

    // T10: remembered for the session, keyed by the *copy's* path. The original's
    // protection is untouched, and no password ever leaves memory.
    passwords.remember(&outcome.path, SessionPassword::new(password));
    Ok(outcome)
}

/// Produces a protected (password + permissions) copy of a document (P3.2 SCOPE-A).
///
/// The source is opened read-only and never written (D-PR4): the copy lands beside it as
/// `"{stem} (protected).pdf"`. `protect_save` verifies the output by reopening it with the
/// password before returning, so a success here means the file the user is handed actually
/// opens. Passwords are used for this call and are not stored (the *copy* has just been
/// given one; a later edit of it goes through the existing password prompt).
#[tauri::command]
pub async fn protect_document(
    db: State<'_, Db>,
    document_id: String,
    user_password: String,
    owner_password: String,
    permissions: crate::editor::protect::PermissionsChoice,
) -> Result<crate::editor::protect::ProtectOutcome, AppError> {
    let path = document_path(&db, &document_id)?;

    let source = path.clone();
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        crate::editor::protect::protect_save(
            Path::new(&source),
            &user_password,
            &owner_password,
            permissions,
        )
    })
    .await
    .map_err(|error| AppError::Other(format!("protect task failed: {error}")))??;

    Ok(outcome)
}

/// What the UI renders for a document's signature (D-SG7).
///
/// Shaped for rendering, not for reuse: the badge, one line explaining it, and the list of things
/// that were and were not checked. Every string here is produced by `badge::explain`, which is the
/// single place that turns evidence into user-facing wording; duplicating the sentences in the
/// frontend would be a second place for a green badge to say something the logic never established.
#[derive(serde::Serialize)]
pub struct SignatureBadge {
    /// One of `valid`, `modified`, `unknown`, `unsigned` (D-SG7's four, and only these four).
    pub badge: &'static str,
    pub lines: Vec<&'static str>,
    /// True only for `valid`. The frontend colours on this rather than on the string, so a new
    /// variant cannot become green by being added to a list somewhere else.
    pub is_green: bool,
    /// The declared DocMDP level, when this is a certification signature.
    pub certification_level: Option<u8>,
}

/// Report the signature badge for a document (P3.2 SCOPE-C, D-SG7).
///
/// Reads the saved file, never the in-memory edits: the badge answers "does the signature attest to
/// the bytes in this file", and unsaved edits are not in it. A signature over a file the user has
/// since edited in the editor is reported on the file, and the edit is still unsaved -- which is
/// why this does not claim anything about the editor's buffer.
///
/// `anchored` is `false` because no local trust store exists yet (D-SG4 leaves trust to a store the
/// user chooses, and that store is a later unit). The consequence is that a perfectly good
/// signature reports `unknown`, which is the honest answer rather than a defect: D-SG7 says a
/// certificate that chains to nothing local is `unknown`, never `valid`.
#[tauri::command]
pub async fn signature_badge(
    db: State<'_, Db>,
    document_id: String,
) -> Result<SignatureBadge, AppError> {
    let path = document_path(&db, &document_id)?;
    let bytes = std::fs::read(&path)
        .map_err(|error| AppError::Other(format!("could not read {path}: {error}")))?;

    let reading = tauri::async_runtime::spawn_blocking(move || {
        crate::editor::signature::verify::inspect(&bytes)
    })
    .await
    .map_err(|error| AppError::Other(format!("verify task failed: {error}")))??;

    let Some(reading) = reading else {
        let evidence = crate::editor::signature::badge::SignatureEvidence {
            integrity: crate::editor::signature::badge::Integrity::NotChecked,
            later_revision: false,
            anchored: false,
            revocation: crate::editor::signature::revocation::Revocation::NotChecked {
                reason: crate::editor::signature::revocation::NotCheckedReason::NoData,
            },
            certification_level: None,
        };
        let detail = crate::editor::signature::badge::explain(&evidence);
        return Ok(SignatureBadge {
            badge: detail.badge.code(),
            lines: detail.lines(),
            is_green: detail.badge.is_green(),
            certification_level: None,
        });
    };

    // No revocation parser has landed, so no data was read. `decide(None, None, now)` is the honest
    // call and returns `NotChecked`, which cannot support `valid` -- the reason a signature over an
    // untouched file still reports `unknown` today.
    let revocation = crate::editor::signature::revocation::decide(None, None, 0);
    let evidence = crate::editor::signature::badge::SignatureEvidence {
        integrity: reading.integrity,
        // `inspect` reads the newest signature only, so a later revision is not representable from
        // here: a second signature that is newest is the one being judged, and if it is intact its
        // tail runs to the end of the file. An edit appended after it would move the end.
        later_revision: false,
        anchored: false,
        revocation,
        certification_level: reading.certification_level,
    };
    let detail = crate::editor::signature::badge::explain(&evidence);
    Ok(SignatureBadge {
        badge: detail.badge.code(),
        lines: detail.lines(),
        is_green: detail.badge.is_green(),
        certification_level: reading.certification_level,
    })
}

/// Drops a document's remembered password (P2.4 T10), called when the document closes.
#[tauri::command]
pub fn forget_password(passwords: State<'_, PasswordStore>, path: String) -> Result<(), AppError> {
    passwords.forget(&path);
    Ok(())
}
