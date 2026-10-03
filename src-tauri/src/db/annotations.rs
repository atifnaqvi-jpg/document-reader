use rusqlite::{params, Connection, OptionalExtension};

use crate::model::{Annotation, AnnotationKind, AppError};

/// Creates an annotation and its search index entry (P2-D5).
///
/// Both writes happen in one transaction: if the FTS row were written after the
/// annotation row and the process died in between, search would drift from truth
/// permanently -- there is no reconciliation pass to repair it.
pub fn add(
    connection: &mut Connection,
    document_id: &str,
    page: u32,
    kind: AnnotationKind,
    data: String,
    text: Option<String>,
    now: i64,
) -> Result<Annotation, AppError> {
    let exists: Option<String> = connection
        .query_row(
            "SELECT id FROM Document WHERE id = ?1",
            params![document_id],
            |row| row.get(0),
        )
        .optional()?;
    if exists.is_none() {
        return Err(AppError::Other("document not found".to_string()));
    }

    let annotation = Annotation {
        id: uuid::Uuid::new_v4().to_string(),
        document_id: document_id.to_string(),
        page,
        kind,
        data,
        text,
        created_at: now,
        modified_at: now,
    };

    let transaction = connection.transaction()?;
    transaction.execute(
        "INSERT INTO Annotation (id, document_id, page, atype, data, text, created_at, modified_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            annotation.id,
            annotation.document_id,
            annotation.page,
            annotation.kind.as_str(),
            annotation.data,
            annotation.text,
            annotation.created_at,
            annotation.modified_at
        ],
    )?;
    if let Some(text) = annotation.text.as_deref() {
        index_text(&transaction, &annotation, text)?;
    }
    transaction.commit()?;

    Ok(annotation)
}

/// Re-inserts a previously deleted annotation **under its original id** (P2-D7).
///
/// Undo must be able to put an annotation back exactly as it was: a fresh id would
/// break anything already holding the old one (a selection, a search hit), and would
/// make redo-then-undo sequences accumulate rows nobody can reference. Callers pass the
/// full row they captured before deleting.
pub fn restore(connection: &mut Connection, annotation: &Annotation) -> Result<(), AppError> {
    let transaction = connection.transaction()?;
    transaction.execute(
        "INSERT INTO Annotation (id, document_id, page, atype, data, text, created_at, modified_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            annotation.id,
            annotation.document_id,
            annotation.page,
            annotation.kind.as_str(),
            annotation.data,
            annotation.text,
            annotation.created_at,
            annotation.modified_at
        ],
    )?;
    if let Some(text) = annotation.text.as_deref() {
        index_text(&transaction, annotation, text)?;
    }
    transaction.commit()?;
    Ok(())
}

/// Replaces an annotation's payload and text, updating both indexes.
pub fn update(
    connection: &mut Connection,
    annotation_id: &str,
    data: String,
    text: Option<String>,
    now: i64,
) -> Result<Annotation, AppError> {
    let transaction = connection.transaction()?;

    let affected = transaction.execute(
        "UPDATE Annotation SET data = ?1, text = ?2, modified_at = ?3 WHERE id = ?4",
        params![data, text, now, annotation_id],
    )?;
    if affected == 0 {
        return Err(AppError::Other("annotation not found".to_string()));
    }

    // Read back inside the transaction so the returned row and the index agree.
    let annotation = load(&transaction, annotation_id)?;
    transaction.execute(
        "DELETE FROM AnnotationSearch WHERE annotation_id = ?1",
        params![annotation_id],
    )?;
    if let Some(text) = annotation.text.as_deref() {
        index_text(&transaction, &annotation, text)?;
    }
    transaction.commit()?;

    Ok(annotation)
}

pub fn list_for_page(
    connection: &Connection,
    document_id: &str,
    page: u32,
) -> Result<Vec<Annotation>, AppError> {
    let mut statement = connection.prepare(
        "SELECT id, document_id, page, atype, data, text, created_at, modified_at
           FROM Annotation
          WHERE document_id = ?1 AND page = ?2
          ORDER BY created_at ASC",
    )?;
    let rows = statement.query_map(params![document_id, page], row_to_annotation)?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

pub fn list_for_document(
    connection: &Connection,
    document_id: &str,
) -> Result<Vec<Annotation>, AppError> {
    let mut statement = connection.prepare(
        "SELECT id, document_id, page, atype, data, text, created_at, modified_at
           FROM Annotation
          WHERE document_id = ?1
          ORDER BY page ASC, created_at ASC",
    )?;
    let rows = statement.query_map(params![document_id], row_to_annotation)?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

pub fn remove(connection: &mut Connection, annotation_id: &str) -> Result<(), AppError> {
    let transaction = connection.transaction()?;
    transaction.execute(
        "DELETE FROM AnnotationSearch WHERE annotation_id = ?1",
        params![annotation_id],
    )?;
    let affected = transaction.execute(
        "DELETE FROM Annotation WHERE id = ?1",
        params![annotation_id],
    )?;
    if affected == 0 {
        return Err(AppError::Other("annotation not found".to_string()));
    }
    transaction.commit()?;
    Ok(())
}

/// Number of annotations for a document, used by the P2-D12 edit-store GC.
pub fn count_for_document(connection: &Connection, document_id: &str) -> Result<u64, AppError> {
    let count: i64 = connection.query_row(
        "SELECT count(*) FROM Annotation WHERE document_id = ?1",
        params![document_id],
        |row| row.get(0),
    )?;
    Ok(count.max(0) as u64)
}

fn load(connection: &Connection, annotation_id: &str) -> Result<Annotation, AppError> {
    connection
        .query_row(
            "SELECT id, document_id, page, atype, data, text, created_at, modified_at
               FROM Annotation WHERE id = ?1",
            params![annotation_id],
            row_to_annotation,
        )
        .optional()?
        .ok_or_else(|| AppError::Other("annotation not found".to_string()))
}

fn index_text(
    connection: &Connection,
    annotation: &Annotation,
    text: &str,
) -> Result<(), AppError> {
    if text.trim().is_empty() {
        return Ok(());
    }
    connection.execute(
        "INSERT INTO AnnotationSearch (annotation_id, document_id, page, content)
         VALUES (?1, ?2, ?3, ?4)",
        params![annotation.id, annotation.document_id, annotation.page, text],
    )?;
    Ok(())
}

fn row_to_annotation(row: &rusqlite::Row<'_>) -> rusqlite::Result<Annotation> {
    let atype: String = row.get(3)?;
    // An unknown `atype` means the row was written by a newer build; surfacing it
    // as a hard error would make one stray row poison the whole page. Shape is
    // the nearest safe stand-in because it renders from rectangles only.
    let kind = AnnotationKind::parse(&atype).unwrap_or(AnnotationKind::Shape);
    Ok(Annotation {
        id: row.get(0)?,
        document_id: row.get(1)?,
        page: row.get(2)?,
        kind,
        data: row.get(4)?,
        text: row.get(5)?,
        created_at: row.get(6)?,
        modified_at: row.get(7)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Connection {
        let connection = Connection::open_in_memory().unwrap();
        crate::db::schema::apply(&connection).unwrap();
        connection
            .execute(
                "INSERT INTO Document (id, path, format, added_at) VALUES ('d1', 'C:/a.pdf', 'pdf', 0)",
                [],
            )
            .unwrap();
        connection
    }

    fn highlight() -> (AnnotationKind, String, Option<String>) {
        (
            AnnotationKind::Highlight,
            r##"{"v":1,"rects":[[10.0,10.0,100.0,20.0]],"color":"#fff3b0","opacity":0.4}"##
                .to_string(),
            None,
        )
    }

    #[test]
    fn add_then_list_round_trips() {
        let mut connection = setup();
        let (kind, data, text) = highlight();
        let created = add(&mut connection, "d1", 3, kind, data, text, 10).unwrap();

        let listed = list_for_page(&connection, "d1", 3).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, created.id);
        assert_eq!(listed[0].kind, AnnotationKind::Highlight);
        assert_eq!(listed[0].page, 3);
        assert_eq!(listed[0].data, created.data);
        assert_eq!(listed[0].text, None);
    }

    #[test]
    fn add_for_unknown_document_is_rejected() {
        let mut connection = setup();
        let (kind, data, text) = highlight();
        let error = add(&mut connection, "nope", 1, kind, data, text, 1).unwrap_err();
        assert_eq!(error.to_string(), "document not found");
    }

    #[test]
    fn list_is_scoped_to_one_document_and_page() {
        let mut connection = setup();
        connection
            .execute(
                "INSERT INTO Document (id, path, format, added_at) VALUES ('d2', 'C:/b.pdf', 'pdf', 0)",
                [],
            )
            .unwrap();
        let (kind, data, text) = highlight();
        add(
            &mut connection,
            "d1",
            1,
            kind,
            data.clone(),
            text.clone(),
            1,
        )
        .unwrap();
        add(
            &mut connection,
            "d1",
            2,
            AnnotationKind::Ink,
            data.clone(),
            text.clone(),
            2,
        )
        .unwrap();
        add(&mut connection, "d2", 1, kind, data, text, 3).unwrap();

        assert_eq!(list_for_page(&connection, "d1", 1).unwrap().len(), 1);
        assert_eq!(list_for_page(&connection, "d1", 2).unwrap().len(), 1);
        assert_eq!(list_for_document(&connection, "d1").unwrap().len(), 2);
        assert_eq!(list_for_document(&connection, "d2").unwrap().len(), 1);
    }

    #[test]
    fn text_annotations_are_searchable_and_deindexed_on_remove() {
        let mut connection = setup();
        let note = add(
            &mut connection,
            "d1",
            4,
            AnnotationKind::Note,
            r#"{"v":1,"point":[5.0,5.0]}"#.to_string(),
            Some("check the footnote here".to_string()),
            10,
        )
        .unwrap();

        let hits: i64 = connection
            .query_row(
                "SELECT count(*) FROM AnnotationSearch WHERE AnnotationSearch MATCH 'footnote'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(hits, 1, "note text should be indexed (P2-D5)");

        remove(&mut connection, &note.id).unwrap();
        let after: i64 = connection
            .query_row(
                "SELECT count(*) FROM AnnotationSearch WHERE AnnotationSearch MATCH 'footnote'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(after, 0, "removing an annotation must deindex its text");
    }

    #[test]
    fn update_refreshes_text_index() {
        let mut connection = setup();
        let note = add(
            &mut connection,
            "d1",
            1,
            AnnotationKind::Note,
            "{}".to_string(),
            Some("original wording".to_string()),
            1,
        )
        .unwrap();

        update(
            &mut connection,
            &note.id,
            "{}".to_string(),
            Some("replacement wording".to_string()),
            2,
        )
        .unwrap();

        let stale: i64 = connection
            .query_row(
                "SELECT count(*) FROM AnnotationSearch WHERE AnnotationSearch MATCH 'original'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let fresh: i64 = connection
            .query_row(
                "SELECT count(*) FROM AnnotationSearch WHERE AnnotationSearch MATCH 'replacement'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stale, 0, "old text must not linger in the index");
        assert_eq!(fresh, 1);
    }

    #[test]
    fn update_reports_unknown_ids() {
        let mut connection = setup();
        let error = update(&mut connection, "nope", "{}".to_string(), None, 1).unwrap_err();
        assert_eq!(error.to_string(), "annotation not found");
    }

    #[test]
    fn remove_deletes_and_reports_unknown_ids() {
        let mut connection = setup();
        let (kind, data, text) = highlight();
        let created = add(&mut connection, "d1", 1, kind, data, text, 1).unwrap();

        remove(&mut connection, &created.id).unwrap();
        assert!(list_for_page(&connection, "d1", 1).unwrap().is_empty());

        let error = remove(&mut connection, &created.id).unwrap_err();
        assert_eq!(error.to_string(), "annotation not found");
    }

    #[test]
    fn annotations_are_cascaded_when_the_document_is_deleted() {
        let mut connection = setup();
        let (kind, data, text) = highlight();
        add(&mut connection, "d1", 1, kind, data, text, 1).unwrap();

        connection
            .execute("DELETE FROM Document WHERE id = 'd1'", [])
            .unwrap();
        assert!(list_for_document(&connection, "d1").unwrap().is_empty());
    }

    #[test]
    fn count_for_document_tracks_edits() {
        let mut connection = setup();
        assert_eq!(count_for_document(&connection, "d1").unwrap(), 0);

        let (kind, data, text) = highlight();
        let created = add(&mut connection, "d1", 1, kind, data, text, 1).unwrap();
        assert_eq!(count_for_document(&connection, "d1").unwrap(), 1);

        remove(&mut connection, &created.id).unwrap();
        assert_eq!(count_for_document(&connection, "d1").unwrap(), 0);
    }

    #[test]
    fn unknown_atype_degrades_instead_of_failing_the_page() {
        let connection = setup();
        connection
            .execute(
                "INSERT INTO Annotation (id, document_id, page, atype, data, text, created_at, modified_at)
                 VALUES ('a1', 'd1', 1, 'from-a-newer-build', '{}', NULL, 1, 1)",
                [],
            )
            .unwrap();

        let listed = list_for_page(&connection, "d1", 1).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].kind, AnnotationKind::Shape);
    }

    #[test]
    fn note_without_text_is_not_indexed() {
        let mut connection = setup();
        add(
            &mut connection,
            "d1",
            1,
            AnnotationKind::Note,
            "{}".to_string(),
            Some("   ".to_string()),
            1,
        )
        .unwrap();

        let rows: i64 = connection
            .query_row("SELECT count(*) FROM AnnotationSearch", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(rows, 0, "whitespace-only text has nothing to index");
    }

    #[test]
    fn restore_reinstates_a_deleted_row_exactly() {
        // P2-D7: undo must put the annotation back as it was, or anything holding
        // its id (a selection, a search hit) breaks.
        let mut connection = setup();
        let created = add(
            &mut connection,
            "d1",
            3,
            AnnotationKind::Note,
            "{\"v\":1}".to_string(),
            Some("remember".to_string()),
            10,
        )
        .unwrap();

        remove(&mut connection, &created.id).unwrap();
        assert_eq!(count_for_document(&connection, "d1").unwrap(), 0);

        restore(&mut connection, &created).unwrap();

        let rows = list_for_document(&connection, "d1").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0], created, "every field survives the round trip");
    }

    #[test]
    fn restore_puts_the_row_back_in_the_search_index() {
        // A restored note must be searchable again; otherwise undo quietly loses the
        // annotation from search while it is visible on the page.
        let mut connection = setup();
        let created = add(
            &mut connection,
            "d1",
            1,
            AnnotationKind::Note,
            "{\"v\":1}".to_string(),
            Some("quartzite".to_string()),
            10,
        )
        .unwrap();

        remove(&mut connection, &created.id).unwrap();
        assert_eq!(matches(&connection, "quartzite"), 0);

        restore(&mut connection, &created).unwrap();
        assert_eq!(matches(&connection, "quartzite"), 1);
    }

    #[test]
    fn restoring_twice_is_rejected_rather_than_duplicated() {
        // A double undo (or a race) must not silently create a second row with the
        // same id; the primary key rejects it.
        let mut connection = setup();
        let created = add(
            &mut connection,
            "d1",
            1,
            AnnotationKind::Note,
            "{\"v\":1}".to_string(),
            None,
            10,
        )
        .unwrap();

        remove(&mut connection, &created.id).unwrap();
        restore(&mut connection, &created).unwrap();
        assert!(restore(&mut connection, &created).is_err());
        assert_eq!(count_for_document(&connection, "d1").unwrap(), 1);
    }

    /// How many indexed rows match a term, for asserting the FTS side is in lockstep.
    fn matches(connection: &Connection, term: &str) -> i64 {
        connection
            .query_row(
                "SELECT count(*) FROM AnnotationSearch WHERE AnnotationSearch MATCH ?1",
                params![term],
                |row| row.get(0),
            )
            .unwrap()
    }
}
