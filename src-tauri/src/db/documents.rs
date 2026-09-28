use rusqlite::{params, Connection, OptionalExtension};

use crate::model::{AppError, DocumentMeta, Format, LibraryEntry};

pub struct Import<'a> {
    pub path: &'a str,
    pub meta: &'a DocumentMeta,
    pub size: i64,
    pub content_hash: &'a str,
}

/// Inserts or refreshes a library row and returns the stored entry.
///
/// Dedupe rule (docs/P0-DESIGN.md §5): same `content_hash` wins over same `path`,
/// because a moved file keeps its hash. Unchanged rows only bump `last_opened_at`.
pub fn upsert(
    connection: &Connection,
    import: Import<'_>,
    now: i64,
) -> Result<LibraryEntry, AppError> {
    let existing: Option<(String, Option<String>)> = connection
        .query_row(
            "SELECT id, content_hash FROM Document WHERE path = ?1 OR content_hash = ?2 LIMIT 1",
            params![import.path, import.content_hash],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;

    let id = match existing {
        Some((id, stored_hash)) => {
            let hash_changed = stored_hash.as_deref() != Some(import.content_hash);
            if hash_changed {
                connection.execute(
                    "UPDATE Document
                        SET path = ?2, title = ?3, author = ?4, format = ?5, size = ?6,
                            pages = ?7, content_hash = ?8, missing = 0, last_opened_at = ?9
                      WHERE id = ?1",
                    params![
                        id,
                        import.path,
                        import.meta.title,
                        import.meta.author,
                        import.meta.format.as_str(),
                        import.size,
                        import.meta.pages,
                        import.content_hash,
                        now
                    ],
                )?;
            } else {
                connection.execute(
                    "UPDATE Document SET path = ?2, missing = 0, last_opened_at = ?3 WHERE id = ?1",
                    params![id, import.path, now],
                )?;
            }
            id
        }
        None => {
            let id = uuid::Uuid::new_v4().to_string();
            connection.execute(
                "INSERT INTO Document
                    (id, path, title, author, format, size, pages, added_at, last_opened_at, content_hash, missing)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8, ?9, 0)",
                params![
                    id,
                    import.path,
                    import.meta.title,
                    import.meta.author,
                    import.meta.format.as_str(),
                    import.size,
                    import.meta.pages,
                    now,
                    import.content_hash
                ],
            )?;
            id
        }
    };

    get(connection, &id)?
        .ok_or_else(|| AppError::Other("document vanished after upsert".to_string()))
}

pub fn get(connection: &Connection, id: &str) -> Result<Option<LibraryEntry>, AppError> {
    let entry = connection
        .query_row(
            "SELECT id, path, title, author, format, pages, added_at, last_opened_at, missing
               FROM Document WHERE id = ?1",
            params![id],
            |row| {
                Ok(LibraryEntry {
                    meta: DocumentMeta {
                        id: row.get(0)?,
                        title: row.get(2)?,
                        author: row.get(3)?,
                        pages: row.get(5)?,
                        format: parse_format(row.get::<_, String>(4)?),
                    },
                    path: row.get(1)?,
                    added_at: row.get(6)?,
                    last_opened_at: row.get(7)?,
                    missing: row.get::<_, i64>(8)? != 0,
                })
            },
        )
        .optional()?;
    Ok(entry)
}

/// Stored content hash for a document, used as part of the render handle-cache key
/// (P1-D5): if the file was replaced in place, the old handle must not be reused.
pub fn content_hash(connection: &Connection, id: &str) -> Result<Option<String>, AppError> {
    let hash = connection
        .query_row(
            "SELECT content_hash FROM Document WHERE id = ?1",
            params![id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten();
    Ok(hash)
}

pub fn list(connection: &Connection) -> Result<Vec<LibraryEntry>, AppError> {
    let mut statement = connection.prepare(
        "SELECT id, path, title, author, format, pages, added_at, last_opened_at, missing
           FROM Document
          ORDER BY COALESCE(last_opened_at, added_at) DESC",
    )?;
    let entries = statement
        .query_map([], |row| {
            Ok(LibraryEntry {
                meta: DocumentMeta {
                    id: row.get(0)?,
                    title: row.get(2)?,
                    author: row.get(3)?,
                    pages: row.get(5)?,
                    format: parse_format(row.get::<_, String>(4)?),
                },
                path: row.get(1)?,
                added_at: row.get(6)?,
                last_opened_at: row.get(7)?,
                missing: row.get::<_, i64>(8)? != 0,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(entries)
}

pub fn set_missing(connection: &Connection, id: &str, missing: bool) -> Result<(), AppError> {
    connection.execute(
        "UPDATE Document SET missing = ?2 WHERE id = ?1",
        params![id, i64::from(missing)],
    )?;
    Ok(())
}

/// Cheap stand-in for `notify`-based watching (which arrives later, DECISIONS §6):
/// re-check each row's file once, per library listing.
pub fn refresh_missing_flags(connection: &Connection) -> Result<(), AppError> {
    for entry in list(connection)? {
        let exists = std::path::Path::new(&entry.path).is_file();
        if exists == entry.missing {
            set_missing(connection, &entry.meta.id, !exists)?;
        }
    }
    Ok(())
}

fn parse_format(raw: String) -> Format {
    Format::from_extension(&raw).unwrap_or(Format::Txt)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Connection {
        let connection = Connection::open_in_memory().unwrap();
        crate::db::schema::apply(&connection).unwrap();
        connection
    }

    fn meta(format: Format) -> DocumentMeta {
        DocumentMeta {
            id: String::new(),
            title: Some("Title".into()),
            author: Some("Author".into()),
            pages: Some(10),
            format,
        }
    }

    fn import<'a>(path: &'a str, meta: &'a DocumentMeta, hash: &'a str) -> Import<'a> {
        Import {
            path,
            meta,
            size: 42,
            content_hash: hash,
        }
    }

    #[test]
    fn inserts_new_document_with_generated_uuid() {
        let connection = setup();
        let m = meta(Format::Pdf);
        let entry = upsert(&connection, import("C:/docs/a.pdf", &m, "hash-a"), 100).unwrap();

        assert!(!entry.meta.id.is_empty());
        assert_eq!(entry.meta.title.as_deref(), Some("Title"));
        assert_eq!(entry.meta.pages, Some(10));
        assert_eq!(entry.meta.format, Format::Pdf);
        assert_eq!(entry.path, "C:/docs/a.pdf");
        assert_eq!(entry.added_at, 100);
        assert!(!entry.missing);
    }

    #[test]
    fn same_path_and_hash_reuses_row_and_only_bumps_last_opened() {
        let connection = setup();
        let m = meta(Format::Txt);
        let first = upsert(&connection, import("C:/docs/a.txt", &m, "same"), 100).unwrap();
        let second = upsert(&connection, import("C:/docs/a.txt", &m, "same"), 200).unwrap();

        assert_eq!(first.meta.id, second.meta.id);
        assert_eq!(second.added_at, 100);
        assert_eq!(second.last_opened_at, Some(200));
        assert_eq!(list(&connection).unwrap().len(), 1);
    }

    #[test]
    fn moved_file_is_deduped_by_content_hash_and_path_is_updated() {
        let connection = setup();
        let m = meta(Format::Pdf);
        let first = upsert(&connection, import("C:/old/a.pdf", &m, "hash-a"), 100).unwrap();
        let moved = upsert(&connection, import("C:/new/a.pdf", &m, "hash-a"), 200).unwrap();

        assert_eq!(first.meta.id, moved.meta.id);
        assert_eq!(moved.path, "C:/new/a.pdf");
        assert_eq!(list(&connection).unwrap().len(), 1);
    }

    #[test]
    fn changed_hash_updates_metadata_in_place() {
        let connection = setup();
        let m = meta(Format::Pdf);
        let first = upsert(&connection, import("C:/a.pdf", &m, "v1"), 100).unwrap();

        let mut updated = meta(Format::Pdf);
        updated.title = Some("Second Edition".into());
        updated.pages = Some(20);
        let second = upsert(&connection, import("C:/a.pdf", &updated, "v2"), 300).unwrap();

        assert_eq!(first.meta.id, second.meta.id);
        assert_eq!(second.meta.title.as_deref(), Some("Second Edition"));
        assert_eq!(second.meta.pages, Some(20));
    }

    #[test]
    fn missing_flag_flips_when_the_file_disappears() {
        let connection = setup();
        let missing_path = std::env::temp_dir().join("docreader-gone.txt");
        let _ = std::fs::remove_file(&missing_path);
        let m = meta(Format::Txt);
        let entry = upsert(
            &connection,
            import(&missing_path.display().to_string(), &m, "hash"),
            1,
        )
        .unwrap();
        assert!(!entry.missing);

        refresh_missing_flags(&connection).unwrap();
        assert!(get(&connection, &entry.meta.id).unwrap().unwrap().missing);
    }

    #[test]
    fn existing_file_is_not_flagged_missing() {
        let connection = setup();
        let path = std::env::temp_dir().join("docreader-present.txt");
        std::fs::write(&path, b"here").unwrap();
        let m = meta(Format::Txt);
        let entry = upsert(
            &connection,
            import(&path.display().to_string(), &m, "hash"),
            1,
        )
        .unwrap();

        refresh_missing_flags(&connection).unwrap();
        assert!(!get(&connection, &entry.meta.id).unwrap().unwrap().missing);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn list_is_ordered_by_most_recently_opened() {
        let connection = setup();
        let m = meta(Format::Txt);
        let old = upsert(&connection, import("C:/old.txt", &m, "h-old"), 10).unwrap();
        let new = upsert(&connection, import("C:/new.txt", &m, "h-new"), 20).unwrap();

        let listed = list(&connection).unwrap();
        assert_eq!(listed[0].meta.id, new.meta.id);
        assert_eq!(listed[1].meta.id, old.meta.id);
    }
}
