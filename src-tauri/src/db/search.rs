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
}
