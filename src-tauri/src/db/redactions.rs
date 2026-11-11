//! Pending redaction marks (P3.2 D-RD1).
//!
//! Marks are working state: they live here so an unapplied redaction survives a crash, and they are
//! deleted the moment apply writes its output so an applied redaction cannot be mistaken for a
//! pending one. Deliberately not `Annotation` rows — see the `Redaction` table's comment in
//! `schema.rs` for why a mark must never ship inside the PDF.

use rusqlite::{params, Connection, OptionalExtension};

use crate::editor::redact::Rect;
use crate::model::{AppError, RedactionKind, RedactionMark};

/// Adds one mark and returns it as stored.
///
/// The rectangle is validated by round-tripping it through its stored form rather than trusting the
/// caller's `f32`s: `NaN` serialises to the string "NaN" and would otherwise be written, then read
/// back as an error at apply time, long after the user could connect the two.
pub fn add(
    connection: &Connection,
    document_id: &str,
    page: u32,
    kind: RedactionKind,
    rect: Rect,
    pattern: Option<String>,
    now: i64,
) -> Result<RedactionMark, AppError> {
    // Finiteness is checked on the input, before normalisation can launder a `NaN` corner into a
    // finite one (see `Rect::require_finite`). The round trip then proves the stored text parses.
    let stored = rect.require_finite()?.to_storage();
    Rect::from_storage(&stored)?;

    if !document_exists(connection, document_id)? {
        return Err(AppError::Other("document not found".to_string()));
    }

    let mark = RedactionMark {
        id: uuid::Uuid::new_v4().to_string(),
        document_id: document_id.to_string(),
        page,
        kind,
        rect: stored,
        pattern,
        created_at: now,
    };

    connection.execute(
        "INSERT INTO Redaction (id, document_id, page, kind, rect, pattern, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            mark.id,
            mark.document_id,
            mark.page,
            mark.kind.as_str(),
            mark.rect,
            mark.pattern,
            mark.created_at
        ],
    )?;

    Ok(mark)
}

/// Adds many marks in one transaction, for search-and-redact.
///
/// One transaction rather than N calls: a pattern that matches forty places either becomes forty
/// marks or none. Half a search-and-redact applied is worse than a failed one, because the user
/// would have to re-read every page to work out which half.
pub fn add_many(
    connection: &mut Connection,
    document_id: &str,
    marks: &[(u32, Rect)],
    kind: RedactionKind,
    pattern: Option<String>,
    now: i64,
) -> Result<Vec<RedactionMark>, AppError> {
    if !document_exists(connection, document_id)? {
        return Err(AppError::Other("document not found".to_string()));
    }
    // Every rectangle is validated before the transaction opens, so a bad one cannot strand a
    // partially-written batch behind a `?` halfway through.
    let stored: Vec<(u32, String)> = marks
        .iter()
        .map(|(page, rect)| {
            let text = rect.require_finite()?.to_storage();
            Rect::from_storage(&text)?;
            Ok((*page, text))
        })
        .collect::<Result<_, AppError>>()?;

    let transaction = connection.transaction()?;
    let mut written = Vec::with_capacity(stored.len());
    for (page, rect) in stored {
        let mark = RedactionMark {
            id: uuid::Uuid::new_v4().to_string(),
            document_id: document_id.to_string(),
            page,
            kind,
            rect,
            pattern: pattern.clone(),
            created_at: now,
        };
        transaction.execute(
            "INSERT INTO Redaction (id, document_id, page, kind, rect, pattern, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                mark.id,
                mark.document_id,
                mark.page,
                mark.kind.as_str(),
                mark.rect,
                mark.pattern,
                mark.created_at
            ],
        )?;
        written.push(mark);
    }
    transaction.commit()?;
    Ok(written)
}

/// Every pending mark for a document, oldest first within each page.
pub fn list_for_document(
    connection: &Connection,
    document_id: &str,
) -> Result<Vec<RedactionMark>, AppError> {
    let mut statement = connection.prepare(
        "SELECT id, document_id, page, kind, rect, pattern, created_at
           FROM Redaction
          WHERE document_id = ?1
          ORDER BY page ASC, created_at ASC",
    )?;
    let rows = statement.query_map(params![document_id], row_to_mark)?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// The marks grouped by page, in the shape `redact::apply_redactions` takes.
///
/// The grouping happens here rather than in the command so the one conversion from stored text to
/// `Rect` is exercised by the storage tests, and so a corrupt row fails as an error the command
/// propagates rather than as a rectangle that silently covers nothing.
pub fn marks_by_page(
    connection: &Connection,
    document_id: &str,
) -> Result<Vec<(u32, Vec<Rect>)>, AppError> {
    let rows = list_for_document(connection, document_id)?;
    let mut grouped: Vec<(u32, Vec<Rect>)> = Vec::new();
    for row in rows {
        let rect = Rect::from_storage(&row.rect)?;
        match grouped.last_mut() {
            // `list_for_document` orders by page, so equal pages arrive together and appending keeps
            // the grouping exact without a map that would reorder them.
            Some((page, rects)) if *page == row.page => rects.push(rect),
            _ => grouped.push((row.page, vec![rect])),
        }
    }
    Ok(grouped)
}

/// How many pending marks a document has, for the panel's count and the apply guard.
pub fn count_for_document(connection: &Connection, document_id: &str) -> Result<u64, AppError> {
    let count: i64 = connection.query_row(
        "SELECT count(*) FROM Redaction WHERE document_id = ?1",
        params![document_id],
        |row| row.get(0),
    )?;
    Ok(count.max(0) as u64)
}

/// Removes one mark. Returns whether a row was actually deleted.
pub fn remove(connection: &Connection, mark_id: &str) -> Result<bool, AppError> {
    let deleted = connection.execute("DELETE FROM Redaction WHERE id = ?1", params![mark_id])?;
    Ok(deleted > 0)
}

/// Clears every mark for a document, returning how many were cleared.
///
/// Called only after apply has written its output: clearing beforehand would lose the user's marks
/// if the write failed, and they would have to redraw them to find out whether it had.
pub fn clear(connection: &Connection, document_id: &str) -> Result<u64, AppError> {
    let cleared = connection.execute(
        "DELETE FROM Redaction WHERE document_id = ?1",
        params![document_id],
    )?;
    Ok(cleared as u64)
}

fn document_exists(connection: &Connection, document_id: &str) -> Result<bool, AppError> {
    let found: Option<String> = connection
        .query_row(
            "SELECT id FROM Document WHERE id = ?1",
            params![document_id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(found.is_some())
}

fn row_to_mark(row: &rusqlite::Row<'_>) -> rusqlite::Result<RedactionMark> {
    let kind: String = row.get(3)?;
    Ok(RedactionMark {
        id: row.get(0)?,
        document_id: row.get(1)?,
        page: row.get(2)?,
        kind: kind.parse::<RedactionKind>().map_err(|message: String| {
            rusqlite::Error::FromSqlConversionFailure(
                3,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    message,
                )),
            )
        })?,
        rect: row.get(4)?,
        pattern: row.get(5)?,
        created_at: row.get(6)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema;

    fn connection() -> Connection {
        let connection = Connection::open_in_memory().unwrap();
        schema::apply(&connection).unwrap();
        connection
            .execute(
                "INSERT INTO Document (id, path, format, added_at) VALUES ('d1', 'C:/a.pdf', 'pdf', 0)",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO Document (id, path, format, added_at) VALUES ('d2', 'C:/b.pdf', 'pdf', 0)",
                [],
            )
            .unwrap();
        connection
    }

    #[test]
    fn a_mark_round_trips_through_storage_unchanged() {
        let connection = connection();
        let rect = Rect::new(70.0, 690.0, 200.0, 706.0);
        let written = add(&connection, "d1", 0, RedactionKind::Rect, rect, None, 10).unwrap();

        let listed = list_for_document(&connection, "d1").unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, written.id);
        assert_eq!(Rect::from_storage(&listed[0].rect).unwrap(), rect);
        assert_eq!(listed[0].kind, RedactionKind::Rect);
    }

    #[test]
    fn a_reversed_rectangle_is_stored_normalised() {
        // The stored STRING is what is asserted, not the read-back. `from_storage` normalises, so a
        // read-back comparison passes whether or not the write was ordered — measured: a mutant that
        // removed normalisation from the write survived a read-back assertion. The file's text is the
        // claim; the reader's tidiness is not evidence about it.
        let connection = connection();
        let reversed = Rect {
            x0: 200.0,
            y0: 706.0,
            x1: 70.0,
            y1: 690.0,
        };
        let mark = add(
            &connection,
            "d1",
            0,
            RedactionKind::Rect,
            reversed,
            None,
            10,
        )
        .unwrap();
        assert_eq!(mark.rect, "70,690,200,706");
    }

    #[test]
    fn marks_group_by_page_in_page_order() {
        let connection = connection();
        let rect = Rect::new(0.0, 0.0, 10.0, 10.0);
        // Inserted out of page order on purpose: the grouping must follow the query's ordering, not
        // the insertion order, or a mark would be applied to the wrong page.
        add(&connection, "d1", 2, RedactionKind::Rect, rect, None, 30).unwrap();
        add(&connection, "d1", 0, RedactionKind::Rect, rect, None, 10).unwrap();
        add(&connection, "d1", 0, RedactionKind::Rect, rect, None, 11).unwrap();

        let grouped = marks_by_page(&connection, "d1").unwrap();
        assert_eq!(
            grouped.iter().map(|(page, _)| *page).collect::<Vec<_>>(),
            vec![0, 2]
        );
        assert_eq!(grouped[0].1.len(), 2);
        assert_eq!(grouped[1].1.len(), 1);
    }

    #[test]
    fn a_pattern_mark_keeps_the_text_it_searched_for() {
        let connection = connection();
        let rect = Rect::new(1.0, 2.0, 3.0, 4.0);
        add(
            &connection,
            "d1",
            0,
            RedactionKind::Pattern,
            rect,
            Some("TOPSECRET".to_string()),
            10,
        )
        .unwrap();

        let listed = list_for_document(&connection, "d1").unwrap();
        assert_eq!(listed[0].pattern.as_deref(), Some("TOPSECRET"));
        assert_eq!(listed[0].kind, RedactionKind::Pattern);
    }

    #[test]
    fn marks_are_scoped_to_their_document() {
        let connection = connection();
        let rect = Rect::new(0.0, 0.0, 10.0, 10.0);
        add(&connection, "d1", 0, RedactionKind::Rect, rect, None, 10).unwrap();

        assert_eq!(list_for_document(&connection, "d2").unwrap().len(), 0);
        assert_eq!(count_for_document(&connection, "d1").unwrap(), 1);
    }

    #[test]
    fn clearing_removes_only_that_documents_marks() {
        let connection = connection();
        let rect = Rect::new(0.0, 0.0, 10.0, 10.0);
        add(&connection, "d1", 0, RedactionKind::Rect, rect, None, 10).unwrap();
        add(&connection, "d2", 0, RedactionKind::Rect, rect, None, 10).unwrap();

        assert_eq!(clear(&connection, "d1").unwrap(), 1);
        assert_eq!(count_for_document(&connection, "d1").unwrap(), 0);
        assert_eq!(count_for_document(&connection, "d2").unwrap(), 1);
    }

    #[test]
    fn a_mark_on_a_missing_document_is_refused() {
        let connection = connection();
        let rect = Rect::new(0.0, 0.0, 10.0, 10.0);
        assert!(add(&connection, "nope", 0, RedactionKind::Rect, rect, None, 10).is_err());
        assert_eq!(count_for_document(&connection, "nope").unwrap(), 0);
    }

    #[test]
    fn deleting_a_document_deletes_its_marks() {
        let connection = connection();
        let rect = Rect::new(0.0, 0.0, 10.0, 10.0);
        add(&connection, "d1", 0, RedactionKind::Rect, rect, None, 10).unwrap();
        connection
            .execute("DELETE FROM Document WHERE id = 'd1'", [])
            .unwrap();
        assert_eq!(count_for_document(&connection, "d1").unwrap(), 0);
    }

    #[test]
    fn remove_reports_whether_it_deleted_anything() {
        let connection = connection();
        let rect = Rect::new(0.0, 0.0, 10.0, 10.0);
        let mark = add(&connection, "d1", 0, RedactionKind::Rect, rect, None, 10).unwrap();

        assert!(remove(&connection, &mark.id).unwrap());
        assert!(
            !remove(&connection, &mark.id).unwrap(),
            "a second remove is a no-op"
        );
    }

    #[test]
    fn a_nan_rectangle_is_refused_rather_than_stored() {
        let connection = connection();
        // Built field by field, the way serde would deliver a command argument. `Rect::new` cannot
        // express this case: `f32::min`/`max` return the non-NaN operand, so `Rect::new(NaN,0,10,10)`
        // is the perfectly finite `10,0,10,10`. An earlier version of this test used `Rect::new` and
        // therefore asserted nothing -- it passed because nothing was ever non-finite.
        let bad = Rect {
            x0: f32::NAN,
            y0: 0.0,
            x1: 10.0,
            y1: 10.0,
        };
        assert!(
            add(&connection, "d1", 0, RedactionKind::Rect, bad, None, 10).is_err(),
            "a rectangle that does not survive storage must not be written"
        );
        assert_eq!(count_for_document(&connection, "d1").unwrap(), 0);
    }

    #[test]
    fn a_corrupt_row_fails_the_grouping_instead_of_covering_nothing() {
        let connection = connection();
        // Written directly, the way a hand-edited or foreign database would arrive.
        connection
            .execute(
                "INSERT INTO Redaction (id, document_id, page, kind, rect, pattern, created_at)
                 VALUES ('r1', 'd1', 0, 'rect', 'not-a-rect', NULL, 10)",
                [],
            )
            .unwrap();
        assert!(
            marks_by_page(&connection, "d1").is_err(),
            "a mark whose rectangle cannot be read must fail loudly; applying it as an empty rect \
             would report a redaction that removed nothing"
        );
    }

    #[test]
    fn add_many_writes_every_mark_or_none() {
        let mut connection = connection();
        let rect = Rect::new(0.0, 0.0, 10.0, 10.0);
        let good = [(0, rect), (1, rect)];
        let written = add_many(
            &mut connection,
            "d1",
            &good,
            RedactionKind::Pattern,
            Some("X".to_string()),
            10,
        )
        .unwrap();
        assert_eq!(written.len(), 2);

        let bad = [(0, rect), (1, Rect::new(f32::INFINITY, 0.0, 1.0, 1.0))];
        assert!(add_many(
            &mut connection,
            "d1",
            &bad,
            RedactionKind::Pattern,
            Some("Y".to_string()),
            11
        )
        .is_err());
        assert_eq!(
            count_for_document(&connection, "d1").unwrap(),
            2,
            "the refused batch must not have added the half that was valid"
        );
    }
}
