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
            let data_dir = app
                .path()
                .app_data_dir()
                .map_err(|e| format!("cannot resolve app data dir: {e}"))?;

            // The identifier changed in 1.0.1, and `app_data_dir()` is derived from it, so an
            // install that predates the rename has its library one directory across. Copy it
            // across before `Db::open` can create an empty one (D-R4).
            let legacy_dir = data_dir
                .parent()
                .map(|parent| parent.join(editor::migrate::LEGACY_IDENTIFIER));
            let migration = match legacy_dir {
                Some(legacy) => editor::migrate::migrate_legacy_data_dir(&data_dir, &legacy),
                None => editor::migrate::MigrationOutcome::NotNeeded,
            };
            // D-R6: shown once, on the surface the user already reads. It is carried through
            // managed state rather than printed, because a Windows GUI user sees no stderr.
            app.manage(editor::migrate::MigrationNotice::new(migration.notice()));

            let db_path = data_dir.join("library.db");

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
            // Session-scoped document passwords (P2.4 T10): in memory only, zeroised on
            // clear, and never written to SQLite or an event.
            app.manage(editor::session::PasswordStore::new());
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
            commands::get_migration_notice,
            commands::index_document,
            commands::get_index_status,
            commands::get_document_page_render,
            commands::get_document_page_text,
            commands::get_document_page_count,
            commands::add_annotation,
            commands::update_annotation,
            commands::set_annotation_status,
            commands::export_xfdf,
            commands::import_xfdf,
            commands::save_xfdf,
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
            commands::get_page_text_runs,
            commands::edit_page_text,
            commands::revert_page_text,
            commands::decrypt_copy,
            commands::protect_document,
            commands::forget_password,
            commands::signature_badge,
            commands::list_signing_certificates,
            commands::sign_document,
            commands::import_trust_anchor,
            commands::import_trust_crl,
        ])
        .run(tauri::generate_context!())
        .expect("error while running the application");
}
