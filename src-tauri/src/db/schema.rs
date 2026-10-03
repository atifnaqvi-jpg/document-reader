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
  missing        INTEGER DEFAULT 0
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
"#;

pub fn apply(connection: &Connection) -> Result<(), AppError> {
    connection.execute_batch(SCHEMA)?;
    Ok(())
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
