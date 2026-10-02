use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use tauri::State;

use crate::db::{app_state, bookmarks, search, Db};
use crate::index::{IndexService, IndexStatus, IndexSummary};
use crate::library;
use crate::model::{
    AppError, Bookmark, DocumentMeta, Format, LibraryEntry, RenderedPage, SearchHit,
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
) -> Result<Arc<RenderedPage>, AppError> {
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
