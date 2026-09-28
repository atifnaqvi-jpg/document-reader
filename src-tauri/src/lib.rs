pub mod commands;
pub mod db;
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
            commands::get_document_page_render,
            commands::get_document_page_text,
            commands::get_document_page_count,
        ])
        .run(tauri::generate_context!())
        .expect("error while running the application");
}
