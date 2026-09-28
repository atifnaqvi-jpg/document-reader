pub mod commands;
pub mod db;
pub mod library;
pub mod model;
pub mod provider;

use tauri::Manager;

use db::Db;
use provider::DocumentHandles;

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
            // One handle cache for the process (P1-D5): page turns reuse open PDFium
            // documents instead of re-parsing the file each time. Behind an `Arc` so a
            // blocking worker can hold it for the duration of a render.
            app.manage(std::sync::Arc::new(DocumentHandles::new()));
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
        ])
        .run(tauri::generate_context!())
        .expect("error while running the application");
}
