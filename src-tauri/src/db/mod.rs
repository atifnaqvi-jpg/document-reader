use rusqlite::Connection;
use std::path::Path;
use std::sync::Mutex;

use crate::model::AppError;

pub mod annotations;
pub mod app_state;
pub mod bookmarks;
pub mod documents;
pub mod schema;
pub mod search;

/// Single connection, guarded by a mutex, managed as Tauri state.
/// Commands never touch `rusqlite` directly — they go through the modules here.
pub struct Db(pub Mutex<Connection>);

impl Db {
    pub fn open(path: &Path) -> Result<Self, AppError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| AppError::Other(e.to_string()))?;
        }
        let connection = Connection::open(path)?;
        schema::apply(&connection)?;
        Ok(Db(Mutex::new(connection)))
    }

    /// In-memory database, used by tests.
    pub fn open_in_memory() -> Result<Self, AppError> {
        let connection = Connection::open_in_memory()?;
        schema::apply(&connection)?;
        Ok(Db(Mutex::new(connection)))
    }

    pub fn with<T>(
        &self,
        f: impl FnOnce(&Connection) -> Result<T, AppError>,
    ) -> Result<T, AppError> {
        let connection = self
            .0
            .lock()
            .map_err(|_| AppError::Other("database lock poisoned".to_string()))?;
        f(&connection)
    }
}
