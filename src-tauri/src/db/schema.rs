use rusqlite::Connection;

use crate::model::AppError;

/// Verbatim from docs/DECISIONS.md §3. The only addition is `PRAGMA foreign_keys`,
/// without which the `ON DELETE CASCADE` on Bookmark is silently ignored by SQLite.
const SCHEMA: &str = r#"
PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS Document (
  id             TEXT PRIMARY KEY,
  path           TEXT NOT NULL,
  title          TEXT,
  author         TEXT,
  format         TEXT NOT NULL,
  size           INTEGER,
  pages          INTEGER,
  added_at       INTEGER NOT NULL,
  last_opened_at INTEGER,
  content_hash   TEXT,
  missing        INTEGER DEFAULT 0,
  encrypted      INTEGER DEFAULT 0
);

CREATE INDEX IF NOT EXISTS idx_document_path ON Document(path);
CREATE INDEX IF NOT EXISTS idx_document_content_hash ON Document(content_hash);

CREATE TABLE IF NOT EXISTS Bookmark (
  id          TEXT PRIMARY KEY,
  document_id TEXT NOT NULL REFERENCES Document(id) ON DELETE CASCADE,
  page        INTEGER,
  offset      INTEGER,
  label       TEXT,
  created_at  INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_bookmark_document ON Bookmark(document_id);

CREATE VIRTUAL TABLE IF NOT EXISTS Search USING fts5(
  document_id UNINDEXED,
  page UNINDEXED,
  content
);

CREATE TABLE IF NOT EXISTS Annotation (
  id          TEXT PRIMARY KEY,
  document_id TEXT NOT NULL REFERENCES Document(id) ON DELETE CASCADE,
  page        INTEGER NOT NULL,
  atype       TEXT NOT NULL,
  data        TEXT NOT NULL,
  text        TEXT,
  created_at  INTEGER NOT NULL,
  modified_at INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_annotation_document ON Annotation(document_id);
CREATE INDEX IF NOT EXISTS idx_annotation_document_page ON Annotation(document_id, page);

-- Annotation text gets its own FTS5 index rather than sharing `Search` (P2-D5):
-- a hit must say *which* it was, and mixing the two would force the UI to guess.
-- Kept in lockstep with `Annotation` inside one transaction.
CREATE VIRTUAL TABLE IF NOT EXISTS AnnotationSearch USING fts5(
  annotation_id UNINDEXED,
  document_id UNINDEXED,
  page UNINDEXED,
  content
);

CREATE TABLE IF NOT EXISTS AppState (
  key   TEXT PRIMARY KEY,
  value TEXT
);

-- P3.2 D-SG4: the certificates the user has chosen to trust. DER, base64-encoded so the column
-- stays TEXT like every other table here. `subject` and `not_after` are denormalised out of the
-- DER so the picker can list anchors without re-parsing each one; they are display data and are
-- re-derived on import, never trusted as the authority for a path decision.
CREATE TABLE IF NOT EXISTS TrustAnchor (
  der        TEXT PRIMARY KEY,
  subject    TEXT NOT NULL,
  not_after  INTEGER NOT NULL,
  added_at   INTEGER NOT NULL
);

-- Revocation lists the user imported. Keyed by the CRL's own DER so re-importing one is a no-op
-- rather than a duplicate row.
CREATE TABLE IF NOT EXISTS TrustCrl (
  der         TEXT PRIMARY KEY,
  issuer      TEXT NOT NULL,
  this_update INTEGER NOT NULL,
  next_update INTEGER,
  added_at    INTEGER NOT NULL
);

-- P3.2 D-RD1: redaction *marks*, and only marks. A mark is working state that has to survive a
-- crash, so it lives here rather than in the PDF; applying writes a new file and clears these rows.
--
-- Deliberately not an `Annotation` row: annotations are renderable state that ships inside the PDF
-- (P2-D3), and a redaction mark must never ship at all (I-RD7). Storing one as an annotation would
-- mean the file the user is about to redact already discloses, in its own bytes, exactly what they
-- are about to hide.
CREATE TABLE IF NOT EXISTS Redaction (
  id          TEXT PRIMARY KEY,
  document_id TEXT NOT NULL REFERENCES Document(id) ON DELETE CASCADE,
  page        INTEGER NOT NULL,
  kind        TEXT NOT NULL,
  rect        TEXT NOT NULL,
  pattern     TEXT,
  created_at  INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_redaction_document ON Redaction(document_id);
CREATE INDEX IF NOT EXISTS idx_redaction_document_page ON Redaction(document_id, page);
"#;

pub fn apply(connection: &Connection) -> Result<(), AppError> {
    connection.execute_batch(SCHEMA)?;
    // Additive migrations for databases created before the column existed. `ALTER TABLE
    // ADD COLUMN` has no `IF NOT EXISTS`, so a duplicate-column error is the expected case
    // and must not fail setup; anything else is a real problem and is surfaced.
    add_column_if_missing(connection, "Document", "encrypted", "INTEGER DEFAULT 0")?;
    Ok(())
}

/// Adds `column` to `table` unless it is already there.
fn add_column_if_missing(
    connection: &Connection,
    table: &str,
    column: &str,
    definition: &str,
) -> Result<(), AppError> {
    let sql = format!("ALTER TABLE {table} ADD COLUMN {column} {definition}");
    match connection.execute(&sql, []) {
        Ok(_) => Ok(()),
        Err(rusqlite::Error::SqliteFailure(_, message)) => {
            // SQLite reports a re-add as "duplicate column name" (or the older
            // "duplicate column name: X"); both mean the migration already ran.
            let already = message
                .as_deref()
                .is_some_and(|text| text.contains("duplicate column name"));
            if already {
                Ok(())
            } else {
                Err(AppError::Db(rusqlite::Error::SqliteFailure(
                    rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_ERROR),
                    message,
                )))
            }
        }
        Err(error) => Err(AppError::Db(error)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connection() -> Connection {
        let connection = Connection::open_in_memory().unwrap();
        apply(&connection).unwrap();
        connection
    }

    fn table_names(connection: &Connection) -> Vec<String> {
        let mut statement = connection
            .prepare("SELECT name FROM sqlite_master WHERE type IN ('table', 'view')")
            .unwrap();
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        rows
    }

    #[test]
    fn creates_every_table_from_the_frozen_schema() {
        let connection = connection();
        let names = table_names(&connection);
        for expected in ["Document", "Bookmark", "Search", "AppState"] {
            assert!(
                names.iter().any(|name| name == expected),
                "missing table {expected} in {names:?}"
            );
        }
    }

    /// Guards the DECISIONS §2 assumption that `rusqlite`'s bundled SQLite ships
    /// FTS5. If this ever fails, P1's search work is blocked and must be fixed here.
    #[test]
    fn fts5_virtual_table_is_queryable() {
        let connection = connection();
        connection
            .execute(
                "INSERT INTO Search (document_id, page, content) VALUES ('doc', 1, 'hello world')",
                [],
            )
            .unwrap();

        let matches: i64 = connection
            .query_row(
                "SELECT count(*) FROM Search WHERE Search MATCH 'hello'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(matches, 1);
    }

    #[test]
    fn bookmark_cascade_delete_is_enforced() {
        let connection = connection();
        connection
            .execute(
                "INSERT INTO Document (id, path, format, added_at) VALUES ('d1', 'C:/a.pdf', 'pdf', 0)",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO Bookmark (id, document_id, created_at) VALUES ('b1', 'd1', 0)",
                [],
            )
            .unwrap();

        connection
            .execute("DELETE FROM Document WHERE id = 'd1'", [])
            .unwrap();

        let remaining: i64 = connection
            .query_row("SELECT count(*) FROM Bookmark", [], |row| row.get(0))
            .unwrap();
        assert_eq!(remaining, 0);
    }

    #[test]
    fn schema_application_is_idempotent() {
        let connection = connection();
        apply(&connection).unwrap();
        apply(&connection).unwrap();

        // Counting is not enough: re-applying must not duplicate anything.
        // (The count excludes FTS5's internal shadow tables.)
        let names = table_names(&connection);
        for expected in ["Document", "Bookmark", "Search", "AppState"] {
            assert_eq!(
                names.iter().filter(|name| *name == expected).count(),
                1,
                "table {expected} should exist exactly once in {names:?}"
            );
        }
    }

    #[test]
    fn missing_column_defaults_to_zero() {
        let connection = connection();
        connection
            .execute(
                "INSERT INTO Document (id, path, format, added_at) VALUES ('d1', 'C:/a.pdf', 'pdf', 0)",
                [],
            )
            .unwrap();
        let missing: i64 = connection
            .query_row("SELECT missing FROM Document WHERE id = 'd1'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(missing, 0);
    }
}
