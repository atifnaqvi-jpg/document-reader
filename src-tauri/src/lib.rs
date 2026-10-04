pub mod commands;
pub mod db;
pub mod editor;
pub mod index;
pub mod library;
pub mod model;
pub mod provider;
pub mod render;

use tauri::Manager;

use db::Db;
use render::RenderService;

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let db_path = app
                .path()
                .app_data_dir()
                .map(|dir| dir.join("library.db"))
                .map_err(|e| format!("cannot resolve app data dir: {e}"))?;

            let db = Db::open(&db_path).map_err(|e| {
                format!("cannot open library database at {}: {e}", db_path.display())
            })?;
            app.manage(db);
            // One render service for the process: owns the PDFium document handle
            // cache (P1-D5) and the byte-budgeted page raster cache (P1-D3), so both
            // are shared across commands and blocking workers.
            app.manage(std::sync::Arc::new(RenderService::new()));
            // One index service for the process, so "who is indexing this document" is a
            // single answer. Kept separate from `RenderService` because indexing outlives
            // a single render and must be cancellable per document (P1-D6).
            app.manage(std::sync::Arc::new(index::IndexService::new()));
            // Debounce tickets for the annotation-change event (P2-D11).
            app.manage(editor::events::ChangeTickets::new());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::open_document,
            commands::list_library,
            commands::search,
            commands::add_bookmark,
            commands::list_bookmarks,
            commands::remove_bookmark,
            commands::set_app_state,
            commands::get_app_state,
            commands::index_document,
            commands::get_index_status,
            commands::get_document_page_render,
            commands::get_document_page_text,
            commands::get_document_page_count,
            commands::add_annotation,
            commands::update_annotation,
            commands::list_annotations,
            commands::list_page_annotations,
            commands::remove_annotation,
            commands::next_save_path,
            commands::restore_annotation,
            commands::save_edits,
            commands::overwrite_edits,
            commands::rotate_page,
            commands::delete_pages,
            commands::move_page,
            commands::extract_pages,
            commands::split_document,
            commands::export_flattened,
            commands::add_watermark,
            commands::export_page_png,
        ])
        .run(tauri::generate_context!())
        .expect("error while running the application");
}
