use rusqlite::{params, Connection};

use crate::model::{AppError, SearchHit, SearchOrigin};

/// FTS5 query over extracted text and user annotations (P2-D5).
///
/// The index is populated from P1 onwards, so an empty result is a normal answer,
/// not an error.
///
/// Both tables are searched and the results merged, tagged by origin. A `UNION`
/// in one statement would be tidier, but the two tables have different columns
/// (only annotations have an id), and `snippet()` cannot be used across a union
/// without a subquery that FTS5 would not be able to optimise with the index.
pub fn query(connection: &Connection, query: &str, limit: u32) -> Result<Vec<SearchHit>, AppError> {
    let phrase = query.trim();
    if phrase.is_empty() {
        return Ok(Vec::new());
    }
    let phrase = quote_phrase(phrase);

    // Document hits first: for a term that appears in both, what the file says is
    // usually what the user is looking for.
    let mut hits = query_documents(connection, &phrase, limit)?;
    let remaining = limit.saturating_sub(hits.len() as u32);
    if remaining > 0 {
        hits.extend(query_annotations(connection, &phrase, remaining)?);
    }
    Ok(hits)
}

fn query_documents(
    connection: &Connection,
    phrase: &str,
    limit: u32,
) -> Result<Vec<SearchHit>, AppError> {
    let mut statement = connection.prepare(
        "SELECT document_id, page, snippet(Search, 2, '[', ']', '…', 12)
           FROM Search
          WHERE Search MATCH ?1
          LIMIT ?2",
    )?;

    let rows = statement.query_map(params![phrase, limit], |row| {
        Ok(SearchHit {
            document_id: row.get(0)?,
            page: row.get::<_, Option<i64>>(1)?.unwrap_or(0).max(0) as u32,
            snippet: row.get(2)?,
            origin: SearchOrigin::Document,
            annotation_id: None,
        })
    });

    collect(rows)
}

fn query_annotations(
    connection: &Connection,
    phrase: &str,
    limit: u32,
) -> Result<Vec<SearchHit>, AppError> {
    // `column 3` is `content`; the snippet markers match the document query so the UI can
    // highlight both the same way.
    //
    // It is 3, not 2, because `AnnotationSearch` carries two extra UNINDEXED columns ahead of
    // `content` (`annotation_id`, `document_id`, `page`, `content`), where `Search` carries two
    // (`document_id`, `page`, `content`). Asking for 2 here snippet-ised the *page* column:
    // correct-looking, non-empty, and the page number where the matched text should be.
    let mut statement = connection.prepare(
        "SELECT document_id, page, snippet(AnnotationSearch, 3, '[', ']', '…', 12), annotation_id
           FROM AnnotationSearch
          WHERE AnnotationSearch MATCH ?1
          LIMIT ?2",
    )?;

    let rows = statement.query_map(params![phrase, limit], |row| {
        Ok(SearchHit {
            document_id: row.get(0)?,
            page: row.get::<_, Option<i64>>(1)?.unwrap_or(0).max(0) as u32,
            snippet: row.get(2)?,
            origin: SearchOrigin::Annotation,
            annotation_id: Some(row.get(3)?),
        })
    });

    collect(rows)
}

/// A malformed MATCH expression is a user-input problem, not a failure: report
/// "no results" instead of surfacing a database error.
fn collect(
    rows: Result<
        rusqlite::MappedRows<'_, impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<SearchHit>>,
        rusqlite::Error,
    >,
) -> Result<Vec<SearchHit>, AppError> {
    match rows {
        Ok(rows) => Ok(rows.collect::<Result<Vec<_>, _>>()?),
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
    use crate::model::{AnnotationKind, FreeTextMode};

    fn setup() -> Connection {
        let connection = Connection::open_in_memory().unwrap();
        crate::db::schema::apply(&connection).unwrap();
        // Annotation inserts carry a FK to Document, so tests that attach notes
        // need the row to exist even though search itself never reads it.
        connection
            .execute(
                "INSERT INTO Document (id, path, format, added_at) VALUES ('d1', 'C:/a.pdf', 'pdf', 0)",
                [],
            )
            .unwrap();
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

    #[test]
    fn document_hits_are_tagged_as_document_text() {
        let connection = setup();
        insert_pages(&connection, "d1", &[(0, "the needle is here".to_string())]).unwrap();

        let hits = query(&connection, "needle", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].origin, SearchOrigin::Document);
        assert_eq!(hits[0].annotation_id, None);
    }

    #[test]
    fn annotation_hits_are_tagged_and_carry_their_id() {
        // P2-D5: a note the user wrote must not be presented as document text.
        let mut connection = setup();
        insert_pages(&connection, "d1", &[(0, "unrelated body text".to_string())]).unwrap();
        let note = crate::db::annotations::add(
            &mut connection,
            "d1",
            2,
            crate::model::AnnotationKind::Note,
            "{}".to_string(),
            Some("my needle remark".to_string()),
            1,
        )
        .unwrap();

        let hits = query(&connection, "needle", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].origin, SearchOrigin::Annotation);
        assert_eq!(hits[0].annotation_id.as_deref(), Some(note.id.as_str()));
        assert_eq!(hits[0].page, 2);
    }

    /// One row per text-bearing kind, each carrying a distinct needle.
    ///
    /// D-A12 asks that a hit in *each* text-bearing type resolve to page + id. The kinds are
    /// listed here explicitly rather than generated from `AnnotationKind::as_str()`: the
    /// point of the assertion is that the text-bearing set is known and enumerated, so a new
    /// kind added to the enum shows up as a decision here rather than silently joining a loop.
    const TEXT_BEARING: &[(AnnotationKind, &str, u32, &str)] = &[
        (AnnotationKind::Note, "needle-note", 0, "sticky note"),
        (
            AnnotationKind::FreeText(FreeTextMode::Textbox),
            "needle-textbox",
            1,
            "textbox",
        ),
        (
            AnnotationKind::FreeText(FreeTextMode::Callout),
            "needle-callout",
            2,
            "callout",
        ),
        (
            AnnotationKind::FreeText(FreeTextMode::Typewriter),
            "needle-typewriter",
            3,
            "typewriter",
        ),
        // A highlight carries no text of its own, but P2-D5 indexes whatever `text` the row
        // holds, and the comment pane lets a user attach one to any kind. Included so the
        // "markup with a comment" case is covered too.
        (
            AnnotationKind::Highlight,
            "needle-highlight",
            4,
            "highlight with a comment",
        ),
    ];

    #[test]
    fn every_text_bearing_kind_produces_a_hit_that_resolves_to_its_page_and_id() {
        // D-A12, and the reason this is not a tautology: `index_text` is called from `add`
        // for whatever `text` the caller passed. If any kind's creation path stopped passing
        // text, that kind would go quietly unsearchable and no existing test would notice --
        // the only hit test in this module inserts a Note.
        let mut connection = setup();
        for (kind, needle, page, _what) in TEXT_BEARING {
            crate::db::annotations::add(
                &mut connection,
                "d1",
                *page,
                *kind,
                "{}".to_string(),
                Some((*needle).to_string()),
                1,
            )
            .unwrap();
        }

        for (kind, needle, page, what) in TEXT_BEARING {
            let hits = query(&connection, needle, 10).unwrap();
            assert_eq!(
                hits.len(),
                1,
                "{what} ({kind:?}) produced no hit for {needle}"
            );
            assert_eq!(hits[0].origin, SearchOrigin::Annotation, "{what}");
            assert_eq!(hits[0].page, *page, "{what} resolved to the wrong page");
            assert!(
                hits[0].annotation_id.is_some(),
                "{what} hit carries no id, so it cannot jump to its annotation"
            );
            assert!(
                hits[0].snippet.contains(needle),
                "{what} snippet lost its match: {}",
                hits[0].snippet
            );
        }
    }

    #[test]
    fn a_reply_is_indexed_and_resolves_to_its_parents_page() {
        // D-A12: "a reply is indexed with its parent's document/page". The reply lives on the
        // parent's page by construction (the provider anchors it there), so this asserts that
        // the page the hit reports is the one the viewer will jump to.
        let mut connection = setup();
        let parent = crate::db::annotations::add(
            &mut connection,
            "d1",
            7,
            AnnotationKind::Note,
            "{}".to_string(),
            Some("the question".to_string()),
            1,
        )
        .unwrap();
        crate::db::annotations::add(
            &mut connection,
            "d1",
            parent.page,
            AnnotationKind::Note,
            serde_json::json!({"v": 1, "point": [1.0, 1.0], "parent": parent.id}).to_string(),
            Some("a needle reply".to_string()),
            2,
        )
        .unwrap();

        let hits = query(&connection, "needle", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].origin, SearchOrigin::Annotation);
        assert_eq!(hits[0].page, 7);
        assert!(
            // The match is wrapped by snippet(), so assert the marker rather than the raw
            // phrase: `contains("needle reply")` fails on `a [needle] reply`, which is the
            // correct output. Same trap as the A-4 substring assertion.
            hits[0].snippet.contains("[needle]"),
            "snippet: {}",
            hits[0].snippet
        );
    }

    #[test]
    fn document_hits_rank_before_annotation_hits() {
        let mut connection = setup();
        insert_pages(
            &connection,
            "d1",
            &[(0, "shared term in the book".to_string())],
        )
        .unwrap();
        crate::db::annotations::add(
            &mut connection,
            "d1",
            5,
            crate::model::AnnotationKind::Note,
            "{}".to_string(),
            Some("shared term in my note".to_string()),
            1,
        )
        .unwrap();

        let hits = query(&connection, "shared", 10).unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].origin, SearchOrigin::Document);
        assert_eq!(hits[1].origin, SearchOrigin::Annotation);
    }

    #[test]
    fn removing_an_annotation_removes_its_search_hit() {
        let mut connection = setup();
        let note = crate::db::annotations::add(
            &mut connection,
            "d1",
            1,
            crate::model::AnnotationKind::Note,
            "{}".to_string(),
            Some("ephemeral needle".to_string()),
            1,
        )
        .unwrap();

        assert_eq!(query(&connection, "needle", 10).unwrap().len(), 1);
        crate::db::annotations::remove(&mut connection, &note.id).unwrap();
        assert!(query(&connection, "needle", 10).unwrap().is_empty());
    }

    #[test]
    fn the_limit_is_shared_between_both_sources() {
        // A flood of document hits must not let annotation hits push the result
        // past the caller's limit -- the caller gets one list of `limit` items.
        let mut connection = setup();
        let pages: Vec<(u32, String)> = (0..5)
            .map(|page| (page, "repeated needle".to_string()))
            .collect();
        insert_pages(&connection, "d1", &pages).unwrap();
        for page in 0..5 {
            crate::db::annotations::add(
                &mut connection,
                "d1",
                page,
                crate::model::AnnotationKind::Note,
                "{}".to_string(),
                Some("note needle".to_string()),
                1,
            )
            .unwrap();
        }

        let hits = query(&connection, "needle", 4).unwrap();
        assert_eq!(hits.len(), 4);
    }
}
