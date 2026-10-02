use rusqlite::{params, Connection};

use crate::model::{AppError, SearchHit};

/// FTS5 query over extracted text. The index is populated from P1 onwards, so an
/// empty result is a normal answer, not an error.
///
/// User input is passed to MATCH as a quoted phrase: raw FTS5 syntax from the
/// search box (`AND`, `*`, `"`, `-`) would otherwise be a parse error.
pub fn query(connection: &Connection, query: &str, limit: u32) -> Result<Vec<SearchHit>, AppError> {
    let phrase = query.trim();
    if phrase.is_empty() {
        return Ok(Vec::new());
    }

    let mut statement = connection.prepare(
        "SELECT document_id, page, snippet(Search, 2, '[', ']', '…', 12)
           FROM Search
          WHERE Search MATCH ?1
          LIMIT ?2",
    )?;

    let rows = statement.query_map(params![quote_phrase(phrase), limit], |row| {
        Ok(SearchHit {
            document_id: row.get(0)?,
            page: row.get::<_, Option<i64>>(1)?.unwrap_or(0).max(0) as u32,
            snippet: row.get(2)?,
        })
    });

    match rows {
        Ok(rows) => Ok(rows.collect::<Result<Vec<_>, _>>()?),
        // A malformed MATCH expression is a user-input problem, not a failure:
        // report "no results" instead of surfacing a database error.
        Err(rusqlite::Error::SqliteFailure(_, _)) => Ok(Vec::new()),
        Err(error) => Err(AppError::Db(error)),
    }
}

/// Inserts a batch of pages for one document (P1-D6).
///
/// Batched rather than one INSERT per page: a 500-page index is 500 statements, and
/// each one would otherwise be its own implicit transaction (500 fsyncs on a real
/// database). One transaction for the batch is what makes indexing a 500-page PDF
/// finish in seconds rather than minutes.
///
/// Pages with no extractable text are skipped: FTS5 has nothing to index for them, and
/// storing an empty row would only inflate the table and produce noise hits.
pub fn insert_pages(
    connection: &Connection,
    document_id: &str,
    pages: &[(u32, String)],
) -> Result<u64, AppError> {
    let mut statement = connection
        .prepare("INSERT INTO Search (document_id, page, content) VALUES (?1, ?2, ?3)")?;

    let mut inserted = 0;
    for (page, content) in pages {
        if content.trim().is_empty() {
            continue;
        }
        statement.execute(params![document_id, *page as i64, content])?;
        inserted += 1;
    }
    Ok(inserted)
}

/// Removes every indexed row for a document. Must run before re-indexing, or a
/// re-index would leave the previous generation's rows behind and return stale hits
/// for pages whose text has since changed.
pub fn delete_document(connection: &Connection, document_id: &str) -> Result<usize, AppError> {
    let removed = connection.execute(
        "DELETE FROM Search WHERE document_id = ?1",
        params![document_id],
    )?;
    Ok(removed)
}

/// How many rows a document has in the index. Zero means "never indexed", which is
/// how a content change is detected (P1-D6): the id is stable across re-imports, so
/// row count plus the stored `content_hash` is what distinguishes first-open from
/// re-index.
pub fn row_count(connection: &Connection, document_id: &str) -> Result<u64, AppError> {
    let count: i64 = connection.query_row(
        "SELECT count(*) FROM Search WHERE document_id = ?1",
        params![document_id],
        |row| row.get(0),
    )?;
    Ok(count.max(0) as u64)
}

fn quote_phrase(phrase: &str) -> String {
    let escaped = phrase.replace('"', "\"\"");
    format!("\"{escaped}\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Connection {
        let connection = Connection::open_in_memory().unwrap();
        crate::db::schema::apply(&connection).unwrap();
        connection
    }

    fn index(connection: &Connection, document_id: &str, page: i64, content: &str) {
        connection
            .execute(
                "INSERT INTO Search (document_id, page, content) VALUES (?1, ?2, ?3)",
                params![document_id, page, content],
            )
            .unwrap();
    }

    #[test]
    fn empty_index_returns_no_hits() {
        let connection = setup();
        assert!(query(&connection, "anything", 10).unwrap().is_empty());
    }

    #[test]
    fn blank_query_returns_no_hits() {
        let connection = setup();
        index(&connection, "d1", 1, "hello world");
        assert!(query(&connection, "   ", 10).unwrap().is_empty());
    }

    #[test]
    fn finds_indexed_content_with_page_and_snippet() {
        let connection = setup();
        index(
            &connection,
            "d1",
            3,
            "the quick brown fox jumps over the lazy dog",
        );

        let hits = query(&connection, "brown", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].document_id, "d1");
        assert_eq!(hits[0].page, 3);
        assert!(hits[0].snippet.contains("[brown]"));
    }

    #[test]
    fn respects_the_limit() {
        let connection = setup();
        for page in 1..=5 {
            index(&connection, "d1", page, "repeated needle text");
        }
        assert_eq!(query(&connection, "needle", 2).unwrap().len(), 2);
    }

    #[test]
    fn fts_syntax_from_the_search_box_does_not_error() {
        let connection = setup();
        index(&connection, "d1", 1, "hello world");

        for query_text in ["AND", "*", "\"unbalanced", "-", "NEAR(", "hello OR"] {
            let result = query(&connection, query_text, 10);
            assert!(result.is_ok(), "query {query_text:?} failed: {result:?}");
        }
    }

    #[test]
    fn matches_are_phrase_scoped_not_token_scoped() {
        let connection = setup();
        index(&connection, "d1", 1, "alpha beta");

        assert_eq!(query(&connection, "alpha beta", 10).unwrap().len(), 1);
        // Word order matters inside a quoted phrase, which is the intent.
        assert!(query(&connection, "beta alpha", 10).unwrap().is_empty());
    }

    #[test]
    fn insert_pages_makes_all_pages_searchable() {
        let connection = setup();
        let pages = vec![
            (0, "ALPHA first page".to_string()),
            (1, "BRAVO second page".to_string()),
            (2, "CHARLIE third page".to_string()),
        ];

        assert_eq!(insert_pages(&connection, "d1", &pages).unwrap(), 3);

        assert_eq!(query(&connection, "ALPHA", 10).unwrap()[0].page, 0);
        assert_eq!(query(&connection, "BRAVO", 10).unwrap()[0].page, 1);
        assert_eq!(query(&connection, "CHARLIE", 10).unwrap()[0].page, 2);
        assert_eq!(row_count(&connection, "d1").unwrap(), 3);
    }

    #[test]
    fn insert_pages_skips_blank_content_rather_than_indexing_empty_rows() {
        let connection = setup();
        let pages = vec![
            (0, "real text".to_string()),
            (1, String::new()),
            (2, "   \n ".to_string()),
        ];

        // Only the page with actual text is stored; blank pages are not indexed.
        assert_eq!(insert_pages(&connection, "d1", &pages).unwrap(), 1);
        assert_eq!(row_count(&connection, "d1").unwrap(), 1);
    }

    #[test]
    fn delete_document_scopes_to_one_document() {
        let connection = setup();
        insert_pages(&connection, "d1", &[(0, "needle in doc one".to_string())]).unwrap();
        insert_pages(&connection, "d2", &[(0, "needle in doc two".to_string())]).unwrap();

        assert_eq!(delete_document(&connection, "d1").unwrap(), 1);

        // d2's rows survive: re-indexing one document must not wipe another's.
        assert_eq!(row_count(&connection, "d1").unwrap(), 0);
        assert_eq!(row_count(&connection, "d2").unwrap(), 1);
        let hits = query(&connection, "needle", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].document_id, "d2");
    }

    #[test]
    fn reindexing_replaces_previous_content_instead_of_duplicating() {
        let connection = setup();
        insert_pages(&connection, "d1", &[(0, "obsolete wording".to_string())]).unwrap();

        delete_document(&connection, "d1").unwrap();
        insert_pages(&connection, "d1", &[(0, "current wording".to_string())]).unwrap();

        assert!(query(&connection, "obsolete", 10).unwrap().is_empty());
        assert_eq!(query(&connection, "current", 10).unwrap().len(), 1);
        assert_eq!(row_count(&connection, "d1").unwrap(), 1);
    }

    #[test]
    fn row_count_for_an_unindexed_document_is_zero() {
        let connection = setup();
        assert_eq!(row_count(&connection, "never-seen").unwrap(), 0);
    }
}
