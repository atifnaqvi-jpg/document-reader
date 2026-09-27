use std::fs::File;
use std::io::Read;
use std::path::Path;

use rusqlite::Connection;
use sha2::{Digest, Sha256};

use crate::db::documents::{self, Import};
use crate::model::{AppError, LibraryEntry};
use crate::provider::provider_for;

/// Opens a file through its provider and records it in the library.
///
/// The provider decides what the document *is*; this module decides what the
/// library *knows* about it (id, hash, timestamps).
pub fn open_and_import(
    connection: &Connection,
    path: &Path,
    now: i64,
) -> Result<LibraryEntry, AppError> {
    if !path.is_file() {
        return Err(AppError::FileNotFound(path.display().to_string()));
    }

    let content_hash = hash_file(path)?;
    let meta = provider_for(path)?.open(path)?;
    let size = std::fs::metadata(path)
        .map(|metadata| metadata.len() as i64)
        .unwrap_or_default();

    documents::upsert(
        connection,
        Import {
            path: &path.display().to_string(),
            meta: &meta,
            size,
            content_hash: &content_hash,
        },
        now,
    )
}

pub fn list_library(connection: &Connection) -> Result<Vec<LibraryEntry>, AppError> {
    documents::refresh_missing_flags(connection)?;
    documents::list(connection)
}

/// SHA-256, streamed so a 500 MB PDF does not land in memory.
pub fn hash_file(path: &Path) -> Result<String, AppError> {
    let mut file = File::open(path).map_err(|e| AppError::Other(e.to_string()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];

    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|e| AppError::Other(e.to_string()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }

    Ok(hex::encode(hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connection() -> Connection {
        let connection = Connection::open_in_memory().unwrap();
        crate::db::schema::apply(&connection).unwrap();
        connection
    }

    fn fixture(name: &str, contents: &[u8]) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("docreader-lib-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, contents).unwrap();
        path
    }

    #[test]
    fn imports_a_text_file_and_returns_metadata() {
        let connection = connection();
        let path = fixture("notes.txt", b"hello");

        let entry = open_and_import(&connection, &path, 1_000).unwrap();
        assert!(!entry.meta.id.is_empty());
        assert_eq!(entry.meta.title.as_deref(), Some("notes"));
        assert_eq!(entry.meta.format, crate::model::Format::Txt);
        assert_eq!(entry.last_opened_at, Some(1_000));
        assert_eq!(list_library(&connection).unwrap().len(), 1);
    }

    #[test]
    fn reopening_the_same_file_reuses_the_same_id() {
        let connection = connection();
        let path = fixture("stable.md", b"# title");

        let first = open_and_import(&connection, &path, 1).unwrap();
        let second = open_and_import(&connection, &path, 2).unwrap();

        assert_eq!(first.meta.id, second.meta.id);
        assert_eq!(list_library(&connection).unwrap().len(), 1);
    }

    #[test]
    fn a_copy_of_the_same_content_is_deduped_by_hash() {
        let connection = connection();
        let original = fixture("original.txt", b"identical bytes");
        let copy = fixture("copy.txt", b"identical bytes");

        let first = open_and_import(&connection, &original, 1).unwrap();
        let second = open_and_import(&connection, &copy, 2).unwrap();

        assert_eq!(first.meta.id, second.meta.id);
        assert_eq!(list_library(&connection).unwrap().len(), 1);
    }

    #[test]
    fn missing_file_is_reported_not_panicked() {
        let connection = connection();
        let error = open_and_import(&connection, Path::new("nope/none.txt"), 1).unwrap_err();
        assert_eq!(error.code(), "FILE_NOT_FOUND");
    }

    #[test]
    fn unsupported_format_is_rejected() {
        let connection = connection();
        let path = fixture("sheet.docx", b"not really docx");
        let error = open_and_import(&connection, &path, 1).unwrap_err();
        assert_eq!(error.code(), "UNSUPPORTED_FORMAT");
    }

    #[test]
    fn hash_is_stable_and_content_sensitive() {
        let one = fixture("hash-a.txt", b"abc");
        let two = fixture("hash-b.txt", b"abd");

        let hash_one = hash_file(&one).unwrap();
        assert_eq!(hash_one, hash_file(&one).unwrap());
        assert_ne!(hash_one, hash_file(&two).unwrap());
        assert_eq!(hash_one.len(), 64);
    }
}
