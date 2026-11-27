use std::fs::File;
use std::io::Read;
use std::path::Path;

use rusqlite::{params, Connection, OptionalExtension};
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

/// Everything `forget` needs to know about one document before it removes the row.
///
/// Captured first, because after the `Document` row is gone the path is unrecoverable: the
/// password cache is keyed by *path*, and the row is the only local record of it.
pub struct ForgettableDocument {
    pub id: String,
    pub path: String,
}

/// Detaches a document from the library. **This never touches the file.**
///
/// The user's ruling for D14 is that "remove from library" means *forget*, not *delete*, and
/// the distinction is the whole contract: the PDF on disk is the user's, and an application
/// that reads files has no business destroying one because a list stopped showing it. So this
/// function issues no `fs::` call at all -- no delete, no move, no rewrite, not even a stat.
/// That is deliberate and it is why requirement 3 (a missing file must still be removable)
/// falls out for free: the row's own text is all that is needed, and `Path::exists()` is never
/// consulted.
///
/// What is removed, and why each one is here:
///
/// * `Document` -- the row itself. Everything with a `document_id` foreign key and
///   `ON DELETE CASCADE` (`Bookmark`, `Annotation`, `Redaction`) goes with it.
/// * `Search` -- page text. An FTS5 virtual table, so it has **no** foreign key and cascade
///   does not reach it; without this an orphan hit would answer searches for a document the
///   user can no longer see, and clicking it would open nothing.
/// * `AnnotationSearch` -- annotation text, same reason. Also an FTS5 table, also uncascaded.
///   (Deleting the `Annotation` rows first is what leaves these orphaned, so this has to run
///   in the same transaction rather than after it.)
/// * `AppState` rows keyed to this document -- `index_status:<id>` and `index_hash:<id>`.
///   Per-document, so they are not shared, and leaving them would make a re-imported
///   document inherit a previous index's completeness claim.
///
/// What is deliberately *not* removed: the trust anchors and CRLs. Those are keyed by
/// certificate DER, not by document, and they are a decision the user made about a
/// *certificate* -- "I trust this CA" -- not about this file. Forgetting a PDF cannot
/// reasonably un-trust a certificate the user imported for every document that carries it.
/// The password cache *is* dropped, because that one is keyed by this document's path.
///
/// Returns the captured identity, or `None` when no such row exists (a repeated click is not
/// a fault).
pub fn forget(
    connection: &Connection,
    document_id: &str,
) -> Result<Option<ForgettableDocument>, AppError> {
    let path: Option<String> = connection
        .query_row(
            "SELECT path FROM Document WHERE id = ?1",
            params![document_id],
            |row| row.get(0),
        )
        .optional()?;

    let Some(path) = path else {
        return Ok(None);
    };

    connection.execute("BEGIN IMMEDIATE", [])?;
    let removed = (|| -> Result<(), AppError> {
        // Unindexed tables first: the cascade below is about to remove the `Annotation` rows
        // that `AnnotationSearch` is keyed by, and no foreign key ties the two. Doing this
        // after the `Document` delete would work; doing it in the same transaction means a
        // failure leaves the library consistent rather than half-forgotten.
        connection.execute(
            "DELETE FROM Search WHERE document_id = ?1",
            params![document_id],
        )?;
        connection.execute(
            "DELETE FROM AnnotationSearch WHERE document_id = ?1",
            params![document_id],
        )?;
        connection.execute(
            "DELETE FROM AppState WHERE key IN (?1, ?2)",
            params![
                crate::index::status_key(document_id),
                crate::index::hash_key(document_id)
            ],
        )?;
        connection.execute("DELETE FROM Document WHERE id = ?1", params![document_id])?;
        Ok(())
    })();

    match removed {
        Ok(()) => {
            connection.execute("COMMIT", [])?;
            Ok(Some(ForgettableDocument {
                id: document_id.to_string(),
                path,
            }))
        }
        Err(error) => {
            connection.execute("ROLLBACK", [])?;
            Err(error)
        }
    }
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

    // ---- D14: forgetting a document never touches the file -------------------------------

    /// Modification time, or `None` when the platform cannot report it.
    fn mtime(path: &std::path::Path) -> Option<std::time::SystemTime> {
        std::fs::metadata(path).ok()?.modified().ok()
    }

    /// The load-bearing guard for D14: the ruling is *forget, never delete*, so the proof has
    /// to be the file's own bytes and timestamp, not the absence of an error.
    ///
    /// Deliberately on a **real fixture written to disk**, not a stub, because the property
    /// under test is a non-effect on a real file: a stubbed filesystem would pass whether or
    /// not the code called `fs::remove_file`. Both halves are asserted -- identical bytes and
    /// an unchanged mtime -- because a rewrite that happened to reproduce the content would
    /// still move the mtime, and that is a write the user did not ask for.
    #[test]
    fn forgetting_a_document_leaves_the_file_byte_identical_and_untouched() {
        let connection = connection();
        let path = fixture("keep-me.txt", b"the user's own bytes\n");
        let entry = open_and_import(&connection, &path, 1_000).unwrap();
        let id = entry.meta.id.clone();

        let before_bytes = std::fs::read(&path).unwrap();
        let before_mtime = mtime(&path);
        assert_eq!(list_library(&connection).unwrap().len(), 1);

        let forgotten = forget(&connection, &id).unwrap();

        assert!(
            forgotten.is_some(),
            "the row existed, so it must be reported as forgotten"
        );
        assert_eq!(forgotten.unwrap().path, path.display().to_string());
        // The library lost the row...
        assert!(list_library(&connection).unwrap().is_empty());
        // ...and the file is exactly as the user left it.
        assert_eq!(
            std::fs::read(&path).unwrap(),
            before_bytes,
            "the file's bytes changed, which means something wrote to it"
        );
        assert_eq!(
            mtime(&path),
            before_mtime,
            "the file's mtime moved, which means it was written even if content matched"
        );
        assert!(path.is_file(), "the file must still exist on disk");
    }

    /// Requirement 3: a row whose file is gone is the main cleanup case, so removing it must
    /// not consult the file at all. If `forget` ever grew a `path.exists()` guard or a stat,
    /// this is the test that would fail.
    #[test]
    fn a_missing_file_can_still_be_forgotten_and_other_rows_survive() {
        let connection = connection();
        let gone = fixture("vanished.txt", b"soon gone");
        let kept = fixture("stays.txt", b"stays");

        let gone_id = open_and_import(&connection, &gone, 1).unwrap().meta.id;
        let kept_id = open_and_import(&connection, &kept, 2).unwrap().meta.id;
        assert_eq!(list_library(&connection).unwrap().len(), 2);

        std::fs::remove_file(&gone).unwrap();
        // A listing would mark it missing; forgetting must work either way, so this runs both
        // before and after the flag is refreshed.
        crate::db::documents::refresh_missing_flags(&connection).unwrap();

        let forgotten = forget(&connection, &gone_id).unwrap();

        assert!(forgotten.is_some());
        let remaining = list_library(&connection).unwrap();
        assert_eq!(remaining.len(), 1, "only the removed row may disappear");
        assert_eq!(remaining[0].meta.id, kept_id);
        assert!(
            kept.is_file(),
            "the surviving document's file must be untouched"
        );
    }

    /// The index rows are what a search reads, and neither FTS5 table has a foreign key, so
    /// cascade cannot be relied on. Asserted directly on both tables.
    #[test]
    fn forgetting_removes_page_and_annotation_index_rows() {
        let connection = connection();
        let path = fixture("indexed.txt", b"some text");
        let id = open_and_import(&connection, &path, 1).unwrap().meta.id;

        connection
            .execute(
                "INSERT INTO Search (document_id, page, content) VALUES (?1, 0, 'seahorse')",
                params![id],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO AnnotationSearch (annotation_id, document_id, page, content)
                 VALUES ('a1', ?1, 0, 'narwhal')",
                params![id],
            )
            .unwrap();

        // Precondition: without this the assertions below could pass on rows that were never
        // inserted, which is the "fixture that cannot distinguish" failure.
        let hits = |term: &str| -> i64 {
            connection
                .query_row(
                    "SELECT count(*) FROM Search WHERE Search MATCH ?1",
                    params![term],
                    |row| row.get(0),
                )
                .unwrap()
        };
        let annotation_hits = |term: &str| -> i64 {
            connection
                .query_row(
                    "SELECT count(*) FROM AnnotationSearch WHERE AnnotationSearch MATCH ?1",
                    params![term],
                    |row| row.get(0),
                )
                .unwrap()
        };
        assert_eq!(hits("seahorse"), 1);
        assert_eq!(annotation_hits("narwhal"), 1);

        forget(&connection, &id).unwrap();

        assert_eq!(
            hits("seahorse"),
            0,
            "page index rows must not survive the removal"
        );
        assert_eq!(
            annotation_hits("narwhal"),
            0,
            "annotation index rows must not survive the removal"
        );
    }

    /// Per-document index state lives in `AppState` under two derived keys. Left behind, a
    /// re-import would inherit a claim about an index that no longer exists.
    #[test]
    fn forgetting_removes_this_documents_index_state_and_nobody_elses() {
        let connection = connection();
        let path = fixture("state.txt", b"x");
        let id = open_and_import(&connection, &path, 1).unwrap().meta.id;
        let other = fixture("other.txt", b"y");
        let other_id = open_and_import(&connection, &other, 2).unwrap().meta.id;

        crate::db::app_state::set(&connection, &crate::index::status_key(&id), "{}").unwrap();
        crate::db::app_state::set(&connection, &crate::index::hash_key(&id), "abc").unwrap();
        crate::db::app_state::set(&connection, &crate::index::status_key(&other_id), "{}").unwrap();

        forget(&connection, &id).unwrap();

        assert_eq!(
            crate::db::app_state::get(&connection, &crate::index::status_key(&id)).unwrap(),
            None
        );
        assert_eq!(
            crate::db::app_state::get(&connection, &crate::index::hash_key(&id)).unwrap(),
            None
        );
        // A per-document key is only safe to delete if it really is per-document.
        assert_eq!(
            crate::db::app_state::get(&connection, &crate::index::status_key(&other_id)).unwrap(),
            Some("{}".to_string()),
            "another document's index state must survive"
        );
    }

    /// Cascades that this depends on, asserted rather than assumed: if `PRAGMA foreign_keys`
    /// ever stopped being applied, annotations and bookmarks would silently outlive their
    /// document and the search guard above would still pass.
    #[test]
    fn forgetting_cascades_to_annotations_bookmarks_and_redactions() {
        let connection = connection();
        let path = fixture("cascade.txt", b"x");
        let id = open_and_import(&connection, &path, 1).unwrap().meta.id;

        connection
            .execute(
                "INSERT INTO Annotation (id, document_id, page, atype, data, text, created_at, modified_at)
                 VALUES ('ann-1', ?1, 0, 'note', '{}', 'text', 1, 1)",
                params![id],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO Bookmark (id, document_id, page, label, created_at)
                 VALUES ('bm-1', ?1, 0, 'here', 1)",
                params![id],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO Redaction (id, document_id, page, kind, rect, created_at)
                 VALUES ('rd-1', ?1, 0, 'area', '0 0 1 1', 1)",
                params![id],
            )
            .unwrap();

        forget(&connection, &id).unwrap();

        for table in ["Annotation", "Bookmark", "Redaction"] {
            let remaining: i64 = connection
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(remaining, 0, "{table} rows outlived their document");
        }
    }

    /// Forgetting is idempotent, so a second click (or a stale UI) is not an error.
    #[test]
    fn forgetting_an_unknown_document_reports_none_rather_than_failing() {
        let connection = connection();
        assert!(forget(&connection, "no-such-id").unwrap().is_none());
        // And the library is still usable afterwards -- the transaction was not left open.
        let path = fixture("after-noop.txt", b"x");
        open_and_import(&connection, &path, 1).unwrap();
        assert_eq!(list_library(&connection).unwrap().len(), 1);
    }
}
