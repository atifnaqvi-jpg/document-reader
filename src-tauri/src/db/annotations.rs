use rusqlite::{params, Connection, OptionalExtension};

use crate::model::{Annotation, AnnotationKind, AnnotationStatus, AppError};

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

/// Inserts many annotations in one transaction, for an XFDF import (P3.1 D-A9).
///
/// One transaction rather than one per row: a file of fifty annotations that failed on the
/// fortieth would otherwise leave thirty-nine behind, and the import report would describe a
/// document state the user does not have.
pub fn add_many(
    connection: &mut Connection,
    document_id: &str,
    rows: Vec<(u32, AnnotationKind, String, Option<String>)>,
    now: i64,
) -> Result<Vec<Annotation>, AppError> {
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

    let transaction = connection.transaction()?;
    let mut inserted = Vec::with_capacity(rows.len());
    for (page, kind, data, text) in rows {
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
        inserted.push(annotation);
    }
    transaction.commit()?;
    Ok(inserted)
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
    // D-A5: deleting a parent deletes its replies. Done here rather than in the caller so
    // there is one delete path -- an undo that restores the parent must not find orphaned
    // replies behind it, and a caller that forgot the cascade would leave text in the DB
    // that is no longer reachable from the page.
    let replies = replies_to(connection, annotation_id)?;

    let transaction = connection.transaction()?;
    let deleted = transaction.execute(
        "DELETE FROM Annotation WHERE id = ?1",
        params![annotation_id],
    )?;
    if deleted == 0 {
        return Err(AppError::Other("annotation not found".to_string()));
    }
    transaction.execute(
        "DELETE FROM AnnotationSearch WHERE annotation_id = ?1",
        params![annotation_id],
    )?;
    for id in replies {
        transaction.execute("DELETE FROM Annotation WHERE id = ?1", params![id])?;
        transaction.execute(
            "DELETE FROM AnnotationSearch WHERE annotation_id = ?1",
            params![id],
        )?;
    }
    transaction.commit()?;
    Ok(())
}

/// Rewrites only the blob's `status` key (D-A7).
///
/// Read-modify-write of one key rather than a whole-blob replace, so a status click cannot
/// undo a geometry edit made since the pane rendered. `None` removes the key, which is
/// "no status" rather than a fifth value.
pub fn set_status(
    connection: &mut Connection,
    annotation_id: &str,
    status: Option<AnnotationStatus>,
    now: i64,
) -> Result<Annotation, AppError> {
    let existing = load(connection, annotation_id)?;
    let mut blob: serde_json::Value = serde_json::from_str(&existing.data).map_err(|error| {
        AppError::Other(format!(
            "annotation {annotation_id} has malformed data: {error}"
        ))
    })?;
    let object = blob.as_object_mut().ok_or_else(|| {
        AppError::Other(format!("annotation {annotation_id} data is not an object"))
    })?;
    match status {
        Some(status) => {
            object.insert(
                "status".to_string(),
                serde_json::Value::String(status.as_str().to_string()),
            );
        }
        None => {
            object.remove("status");
        }
    }
    update(
        connection,
        annotation_id,
        blob.to_string(),
        existing.text,
        now,
    )
}

/// The replies whose `parent` is `parent_id` (D-A5).
///
/// Read from the blob rather than from a column: the parent link lives in the JSON (D-A2),
/// and a column would be a second copy that could disagree with the file. SQLite can filter
/// on a json field, but the parse here is the same one the writer uses, so the cascade and
/// the save agree about what a reply is by construction.
pub fn replies_to(connection: &Connection, parent_id: &str) -> Result<Vec<String>, AppError> {
    let mut statement =
        connection.prepare("SELECT id, data FROM Annotation WHERE document_id = (SELECT document_id FROM Annotation WHERE id = ?1)")?;
    let rows = statement.query_map(params![parent_id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut replies = Vec::new();
    for row in rows {
        let (id, data) = row?;
        let parent = serde_json::from_str::<serde_json::Value>(&data)
            .ok()
            .and_then(|blob| {
                blob.get("parent")
                    .and_then(|p| p.as_str())
                    .map(str::to_string)
            });
        if parent.as_deref() == Some(parent_id) {
            replies.push(id);
        }
    }
    Ok(replies)
}

/// The document an annotation belongs to.
///
/// Lets the annotation-change event (P2-D11) name a document when the command that
/// mutated the row only received an annotation id, and keeps the event's payload backed
/// by the DB rather than by whatever the frontend happened to pass.
pub fn document_id_for(connection: &Connection, annotation_id: &str) -> Result<String, AppError> {
    connection
        .query_row(
            "SELECT document_id FROM Annotation WHERE id = ?1",
            params![annotation_id],
            |row| row.get(0),
        )
        .map_err(|_| AppError::Other("annotation not found".to_string()))
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
    let kind = AnnotationKind::parse(&atype).unwrap_or(AnnotationKind::Rect);
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

    /// A note blob at `v: 2`, optionally replying to `parent`.
    fn note_blob(parent: Option<&str>) -> String {
        match parent {
            Some(parent) => format!(r#"{{"v":2,"point":[10.0,10.0],"parent":"{parent}"}}"#),
            None => r#"{"v":2,"point":[10.0,10.0]}"#.to_string(),
        }
    }

    #[test]
    fn deleting_a_parent_deletes_its_replies() {
        // D-A5: the reply's text would otherwise stay in the DB and in the FTS index, so a
        // search would keep returning a hit the user cannot reach on the page.
        let mut connection = setup();
        let parent = add(
            &mut connection,
            "d1",
            0,
            AnnotationKind::Highlight,
            highlight().1,
            None,
            1,
        )
        .unwrap();
        let reply = add(
            &mut connection,
            "d1",
            0,
            AnnotationKind::Note,
            note_blob(Some(&parent.id)),
            Some("a reply".to_string()),
            2,
        )
        .unwrap();

        remove(&mut connection, &parent.id).unwrap();

        assert_eq!(list_for_document(&connection, "d1").unwrap().len(), 0);
        assert_eq!(
            replies_to(&connection, &parent.id).unwrap(),
            Vec::<String>::new(),
            "the reply row outlived its parent"
        );
        let hits = crate::db::search::query(&connection, "reply", 10).unwrap();
        assert!(
            hits.iter()
                .all(|hit| hit.annotation_id.as_deref() != Some(reply.id.as_str())),
            "the deleted reply is still in the search index"
        );
    }

    #[test]
    fn add_many_writes_every_row_and_indexes_its_text() {
        // The import path (P3.1 D-A9) relies on this being one transaction that also pushes
        // FTS rows: an import that wrote annotations but not their index entries would make
        // the document searchable only for the annotations that already existed.
        let mut connection = setup();
        let rows = vec![
            (0u32, AnnotationKind::Highlight, highlight().1, None),
            (
                1,
                AnnotationKind::Note,
                note_blob(None),
                Some("an imported remark".to_string()),
            ),
        ];

        let inserted = add_many(&mut connection, "d1", rows, 5).unwrap();
        assert_eq!(inserted.len(), 2);
        assert_eq!(inserted[0].page, 0);
        assert_eq!(inserted[1].page, 1);

        let on_page = list_for_page(&connection, "d1", 1).unwrap();
        assert_eq!(on_page.len(), 1, "the second row did not land on its page");

        let hits = crate::db::search::query(&connection, "imported", 10).unwrap();
        assert_eq!(hits.len(), 1, "the imported text never reached the index");
    }

    #[test]
    fn add_many_refuses_an_unknown_document_and_writes_nothing() {
        // A silent insert into a document that does not exist would leave orphan rows no query
        // returns, so the check has to fail before the transaction opens.
        let mut connection = setup();
        let result = add_many(
            &mut connection,
            "no-such-document",
            vec![(0, AnnotationKind::Note, note_blob(None), None)],
            5,
        );
        assert!(result.is_err());
    }

    #[test]
    fn a_replys_text_is_searchable() {
        // DoD 3 of P3.1: the reply's text reaches FTS. It is a `note` row like any other, so
        // this passes only because replies are not special-cased on the write path -- which is
        // the thing worth pinning, since a separate reply table would have needed its own
        // index write and could have been forgotten.
        let mut connection = setup();
        let parent = add(
            &mut connection,
            "d1",
            0,
            AnnotationKind::Highlight,
            highlight().1,
            None,
            1,
        )
        .unwrap();
        add(
            &mut connection,
            "d1",
            0,
            AnnotationKind::Note,
            note_blob(Some(&parent.id)),
            Some("a distinctive reply".to_string()),
            2,
        )
        .unwrap();

        let hits = crate::db::search::query(&connection, "distinctive", 10).unwrap();
        assert_eq!(hits.len(), 1, "the reply's text never reached the index");
        assert_eq!(hits[0].page, 0);
    }

    #[test]
    fn deleting_a_reply_leaves_its_parent_alone() {
        // The cascade must not run upwards: removing a reply is not removing the thread.
        let mut connection = setup();
        let parent = add(
            &mut connection,
            "d1",
            0,
            AnnotationKind::Highlight,
            highlight().1,
            None,
            1,
        )
        .unwrap();
        let reply = add(
            &mut connection,
            "d1",
            0,
            AnnotationKind::Note,
            note_blob(Some(&parent.id)),
            Some("a reply".to_string()),
            2,
        )
        .unwrap();

        remove(&mut connection, &reply.id).unwrap();

        let remaining = list_for_document(&connection, "d1").unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].id, parent.id);
    }

    #[test]
    fn a_reply_to_a_reply_is_found_by_replies_to() {
        // The link is per-blob, so a chain is a chain: nothing here flattens replies onto
        // the root.
        let mut connection = setup();
        let parent = add(
            &mut connection,
            "d1",
            0,
            AnnotationKind::Highlight,
            highlight().1,
            None,
            1,
        )
        .unwrap();
        let first = add(
            &mut connection,
            "d1",
            0,
            AnnotationKind::Note,
            note_blob(Some(&parent.id)),
            Some("first".to_string()),
            2,
        )
        .unwrap();
        let second = add(
            &mut connection,
            "d1",
            0,
            AnnotationKind::Note,
            note_blob(Some(&first.id)),
            Some("second".to_string()),
            3,
        )
        .unwrap();

        assert_eq!(
            replies_to(&connection, &parent.id).unwrap(),
            vec![first.id.clone()]
        );
        assert_eq!(replies_to(&connection, &first.id).unwrap(), vec![second.id]);
    }

    #[test]
    fn set_status_rewrites_only_the_status_key() {
        // The geometry must survive a status click: this is the property that makes the
        // single-key write worth having instead of a whole-blob replace.
        let mut connection = setup();
        let row = add(
            &mut connection,
            "d1",
            0,
            AnnotationKind::Highlight,
            r##"{"v":2,"rects":[[5.0,6.0,7.0,8.0]],"color":"#fff3b0","opacity":0.4}"##.to_string(),
            None,
            1,
        )
        .unwrap();

        let updated = set_status(
            &mut connection,
            &row.id,
            Some(AnnotationStatus::Completed),
            2,
        )
        .unwrap();

        let blob: serde_json::Value = serde_json::from_str(&updated.data).unwrap();
        assert_eq!(blob["status"], serde_json::json!("completed"));
        assert_eq!(blob["rects"], serde_json::json!([[5.0, 6.0, 7.0, 8.0]]));
        assert_eq!(blob["opacity"], serde_json::json!(0.4));
        assert_eq!(updated.modified_at, 2);
        assert_eq!(updated.created_at, 1);
    }

    #[test]
    fn clearing_a_status_removes_the_key_rather_than_storing_null() {
        // "none" is the absence of a status. A stored JSON null would parse as absent in Rust
        // but would read as a present-and-null field to anything looking at the raw blob.
        let mut connection = setup();
        let row = add(
            &mut connection,
            "d1",
            0,
            AnnotationKind::Highlight,
            r##"{"v":2,"rects":[[5.0,6.0,7.0,8.0]],"status":"accepted"}"##.to_string(),
            None,
            1,
        )
        .unwrap();

        let updated = set_status(&mut connection, &row.id, None, 2).unwrap();

        let blob: serde_json::Value = serde_json::from_str(&updated.data).unwrap();
        assert!(
            blob.get("status").is_none(),
            "clearing a status left a value in the blob: {blob}"
        );
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
        assert_eq!(listed[0].kind, AnnotationKind::Rect);
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
