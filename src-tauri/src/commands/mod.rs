use std::time::{SystemTime, UNIX_EPOCH};

use tauri::State;

use crate::db::{app_state, bookmarks, search, Db};
use crate::library;
use crate::model::{AppError, Bookmark, DocumentMeta, LibraryEntry, SearchHit};
use crate::provider::provider_for;

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

/// P1 wires this to `ReaderProvider::render_page` (PDFium in Rust, returned as
/// encoded image bytes). P0 keeps the surface complete and returns a typed error
/// so the frontend contract is already exercised.
#[tauri::command]
pub fn get_document_page_render(
    db: State<'_, Db>,
    doc_id: String,
    page: u32,
    scale: f32,
) -> Result<Vec<u8>, AppError> {
    db.with(|connection| {
        let entry = crate::db::documents::get(connection, &doc_id)?
            .ok_or_else(|| AppError::Other("document not found".to_string()))?;
        provider_for(std::path::Path::new(&entry.path))?.render_page(&entry, page, scale)
    })
}
