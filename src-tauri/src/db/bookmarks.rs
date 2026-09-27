use rusqlite::{params, Connection, OptionalExtension};

use crate::model::{AppError, Bookmark};

pub fn add(
    connection: &Connection,
    document_id: &str,
    page: Option<u32>,
    offset: Option<u32>,
    label: Option<String>,
    now: i64,
) -> Result<Bookmark, AppError> {
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

    let bookmark = Bookmark {
        id: uuid::Uuid::new_v4().to_string(),
        document_id: document_id.to_string(),
        page,
        offset,
        label,
        created_at: now,
    };

    connection.execute(
        "INSERT INTO Bookmark (id, document_id, page, offset, label, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            bookmark.id,
            bookmark.document_id,
            bookmark.page,
            bookmark.offset,
            bookmark.label,
            bookmark.created_at
        ],
    )?;

    Ok(bookmark)
}

pub fn list(connection: &Connection, document_id: &str) -> Result<Vec<Bookmark>, AppError> {
    let mut statement = connection.prepare(
        "SELECT id, document_id, page, offset, label, created_at
           FROM Bookmark WHERE document_id = ?1 ORDER BY created_at ASC",
    )?;
    let bookmarks = statement
        .query_map(params![document_id], |row| {
            Ok(Bookmark {
                id: row.get(0)?,
                document_id: row.get(1)?,
                page: row.get(2)?,
                offset: row.get(3)?,
                label: row.get(4)?,
                created_at: row.get(5)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(bookmarks)
}

pub fn remove(connection: &Connection, bookmark_id: &str) -> Result<(), AppError> {
    let affected =
        connection.execute("DELETE FROM Bookmark WHERE id = ?1", params![bookmark_id])?;
    if affected == 0 {
        return Err(AppError::Other("bookmark not found".to_string()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Connection {
        let connection = Connection::open_in_memory().unwrap();
        crate::db::schema::apply(&connection).unwrap();
        connection
    }

    fn insert_document(connection: &Connection) {
        connection
            .execute(
                "INSERT INTO Document (id, path, format, added_at) VALUES ('d1', 'C:/a.pdf', 'pdf', 0)",
                [],
            )
            .unwrap();
    }

    #[test]
    fn add_then_list_round_trips() {
        let connection = setup();
        insert_document(&connection);

        let first = add(
            &connection,
            "d1",
            Some(1),
            Some(0),
            Some("Intro".into()),
            10,
        )
        .unwrap();
        let second = add(&connection, "d1", Some(7), None, None, 20).unwrap();

        let listed = list(&connection, "d1").unwrap();
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].id, first.id);
        assert_eq!(listed[0].label.as_deref(), Some("Intro"));
        assert_eq!(listed[1].id, second.id);
        assert_eq!(listed[1].page, Some(7));
        assert_eq!(listed[1].label, None);
    }

    #[test]
    fn add_for_unknown_document_is_rejected() {
        let connection = setup();
        let error = add(&connection, "nope", Some(1), None, None, 1).unwrap_err();
        assert_eq!(error.to_string(), "document not found");
    }

    #[test]
    fn remove_deletes_and_reports_unknown_ids() {
        let connection = setup();
        insert_document(&connection);
        let bookmark = add(&connection, "d1", Some(1), None, None, 1).unwrap();

        remove(&connection, &bookmark.id).unwrap();
        assert!(list(&connection, "d1").unwrap().is_empty());

        let error = remove(&connection, &bookmark.id).unwrap_err();
        assert_eq!(error.to_string(), "bookmark not found");
    }

    #[test]
    fn bookmarks_are_cascaded_when_the_document_is_deleted() {
        let connection = setup();
        insert_document(&connection);
        add(&connection, "d1", Some(1), None, None, 1).unwrap();

        connection
            .execute("DELETE FROM Document WHERE id = 'd1'", [])
            .unwrap();
        assert!(list(&connection, "d1").unwrap().is_empty());
    }

    #[test]
    fn list_is_scoped_to_one_document() {
        let connection = setup();
        insert_document(&connection);
        connection
            .execute(
                "INSERT INTO Document (id, path, format, added_at) VALUES ('d2', 'C:/b.pdf', 'pdf', 0)",
                [],
            )
            .unwrap();
        add(&connection, "d1", Some(1), None, None, 1).unwrap();
        add(&connection, "d2", Some(2), None, None, 2).unwrap();

        assert_eq!(list(&connection, "d1").unwrap().len(), 1);
        assert_eq!(list(&connection, "d2").unwrap().len(), 1);
    }
}
