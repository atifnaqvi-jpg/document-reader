use rusqlite::{params, Connection, OptionalExtension};

use crate::model::AppError;

pub fn set(connection: &Connection, key: &str, value: &str) -> Result<(), AppError> {
    connection.execute(
        "INSERT INTO AppState (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

pub fn get(connection: &Connection, key: &str) -> Result<Option<String>, AppError> {
    let value = connection
        .query_row(
            "SELECT value FROM AppState WHERE key = ?1",
            params![key],
            |row| row.get(0),
        )
        .optional()?;
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Connection {
        let connection = Connection::open_in_memory().unwrap();
        crate::db::schema::apply(&connection).unwrap();
        connection
    }

    #[test]
    fn missing_key_is_none() {
        let connection = setup();
        assert_eq!(get(&connection, "theme").unwrap(), None);
    }

    #[test]
    fn set_then_get_round_trips() {
        let connection = setup();
        set(&connection, "theme", "dark").unwrap();
        assert_eq!(get(&connection, "theme").unwrap().as_deref(), Some("dark"));
    }

    #[test]
    fn set_overwrites_existing_value() {
        let connection = setup();
        set(&connection, "theme", "dark").unwrap();
        set(&connection, "theme", "light").unwrap();
        assert_eq!(get(&connection, "theme").unwrap().as_deref(), Some("light"));

        let count: i64 = connection
            .query_row("SELECT count(*) FROM AppState", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }
}
