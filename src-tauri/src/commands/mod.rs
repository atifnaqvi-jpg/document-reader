use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use tauri::State;

use crate::db::{app_state, bookmarks, search, Db};
use crate::library;
use crate::model::{
    AppError, Bookmark, DocumentMeta, Format, LibraryEntry, RenderedPage, SearchHit,
};
use crate::provider::{DocumentHandles, PdfProvider, ReaderProvider};

/// Base64 alphabet used to move PNG bytes across the IPC boundary.
const BASE64: base64::engine::general_purpose::GeneralPurpose =
    base64::engine::general_purpose::STANDARD;

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or_default()
}

#[tauri::command]
pub fn open_document(db: State<'_, Db>, path: String) -> Result<DocumentMeta, AppError> {
    let entry = db.with(|connection| library::open_and_import(connection, path.as_ref(), now()))?;
    Ok(entry.meta)
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

/// Renders one page as PNG bytes plus its pixel size (P1-D2).
///
/// `async` on purpose (P1-D5/D3): rasterizing a page is CPU-bound and can take
/// hundreds of milliseconds, so it runs on a blocking worker rather than on the IPC
/// thread, which would freeze every other command — including the UI's own polling —
/// for the duration of the render.
#[tauri::command]
pub async fn get_document_page_render(
    db: State<'_, Db>,
    handles: State<'_, Arc<DocumentHandles>>,
    doc_id: String,
    page: u32,
    scale: f32,
) -> Result<RenderedPage, AppError> {
    // Resolve what the worker needs while we still have the `State` borrow. Doing this
    // up front means the closure only owns plain data (`State` is not `'static`), and
    // the `Db` mutex is never held across the render itself.
    let (path, content_hash, format) = db.with(|connection| {
        let entry = crate::db::documents::get(connection, &doc_id)?
            .ok_or_else(|| AppError::Other("document not found".to_string()))?;
        let hash = crate::db::documents::content_hash(connection, &doc_id)?;
        Ok((entry.path, hash, entry.meta.format))
    })?;

    // Only PDF is renderable in P1 (PDFium); other formats have no page raster.
    if format != Format::Pdf {
        return Err(AppError::RenderUnavailable);
    }

    let handles = Arc::clone(&handles);
    tauri::async_runtime::spawn_blocking(move || {
        let rendered = PdfProvider::render_cached(
            &handles,
            Path::new(&path),
            content_hash.as_deref(),
            page,
            scale,
        )?;

        Ok(RenderedPage {
            // Encoded here rather than left as raw bytes: see the field docs.
            bytes: BASE64.encode(&rendered.bytes),
            width: rendered.width,
            height: rendered.height,
        })
    })
    .await
    .map_err(|e| AppError::Other(format!("render task failed: {e}")))?
}

/// Extracts one page's text layer. Text extraction is also PDFium work, so it takes
/// the same blocking-worker path (`extract_text` feeds the P1.6 indexer).
#[tauri::command]
pub async fn get_document_page_text(
    db: State<'_, Db>,
    handles: State<'_, Arc<DocumentHandles>>,
    doc_id: String,
    page: u32,
) -> Result<String, AppError> {
    let (path, content_hash, format) = db.with(|connection| {
        let entry = crate::db::documents::get(connection, &doc_id)?
            .ok_or_else(|| AppError::Other("document not found".to_string()))?;
        let hash = crate::db::documents::content_hash(connection, &doc_id)?;
        Ok((entry.path, hash, entry.meta.format))
    })?;

    let handles = Arc::clone(&handles);
    tauri::async_runtime::spawn_blocking(move || match format {
        Format::Pdf => {
            PdfProvider::extract_cached(&handles, Path::new(&path), content_hash.as_deref(), page)
        }
        // Plain-text formats have no paginated text layer; the whole file is the text.
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
