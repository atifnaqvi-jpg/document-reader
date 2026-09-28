//! Cross-module regression test for the P1.0 bug.
//!
//! `TextProvider::extract_text` used `Path::new(&doc.id)` — a library UUID — as a
//! filesystem path, so extraction could never succeed. The unit tests in
//! `provider::text` cover the provider in isolation; this test proves the fix holds
//! through the real path the app takes: import a file, then extract using only the
//! `LibraryEntry` the library layer stored.

use std::path::PathBuf;

use document_reader_lib::db::Db;
use document_reader_lib::library;
use document_reader_lib::provider::provider_for;

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("sample.txt")
}

#[test]
fn imported_text_document_extracts_through_its_stored_path() {
    let db = Db::open_in_memory().unwrap();
    let path = fixture_path();
    let expected = std::fs::read_to_string(&path).unwrap();

    let entry = db
        .with(|connection| library::open_and_import(connection, &path, 1))
        .unwrap();

    // The id is a UUID and is emphatically not a path.
    assert!(!entry.meta.id.is_empty());
    assert!(
        !PathBuf::from(&entry.meta.id).is_file(),
        "id must not resolve as a file: {}",
        entry.meta.id
    );

    let provider = provider_for(std::path::Path::new(&entry.path)).unwrap();
    let text = provider.extract_text(&entry, 0).unwrap();

    assert_eq!(text, expected);
    assert!(text.contains("quick brown fox"));
}

#[test]
fn extraction_survives_a_copy_with_a_different_name() {
    // Same content, new path: dedupe keeps one row but the path must still resolve.
    let db = Db::open_in_memory().unwrap();
    let dir = std::env::temp_dir().join(format!("docreader-p10-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let copy = dir.join("renamed-copy.txt");
    std::fs::copy(fixture_path(), &copy).unwrap();

    let entry = db
        .with(|connection| library::open_and_import(connection, &copy, 1))
        .unwrap();

    let provider = provider_for(std::path::Path::new(&entry.path)).unwrap();
    let text = provider.extract_text(&entry, 0).unwrap();
    assert!(text.contains("quick brown fox"));

    let _ = std::fs::remove_dir_all(&dir);
}
