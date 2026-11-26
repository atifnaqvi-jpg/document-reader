//! Edit-copy GC against the real filesystem (P2-D12).
//!
//! Kept out of the unit tests in `editor::gc` because this is the behaviour that decides whether a
//! file the user can see survives: the predicate can be perfectly correct while the wiring deletes
//! too eagerly, and only a real directory shows that.
//!
//! **These tests were inverted on 2026-11-25.** They used to assert that the last annotation's
//! removal *collects* the edit copy. The user overruled that: a saved PDF vanishing because the
//! last annotation was removed is data loss from the user's point of view, however it is
//! implemented. So each test now asserts the copy is still there after the delete -- the same
//! fixtures, the same wiring, the opposite verdict. Keeping the old fixtures is the point: they
//! are the exact situations in which the deletion used to fire.

use std::fs;
use std::path::{Path, PathBuf};

use document_reader_lib::db::{annotations, documents, Db};
use document_reader_lib::model::{AnnotationKind, DocumentMeta, Format};

/// Builds a document plus a saved edit copy, the way a real save would leave them.
fn fixture() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let original = dir.path().join("report.pdf");
    fs::write(&original, b"%PDF-1.7\n").unwrap();
    fs::write(dir.path().join("report (edited 1).pdf"), b"%PDF-1.7\n").unwrap();

    // A neighbouring file that must survive: it shares the stem but is not a generated
    // edit copy, so deleting it would destroy something the user made themselves.
    fs::write(
        dir.path().join("report (edited 1) final.pdf"),
        b"user's own",
    )
    .unwrap();
    fs::write(dir.path().join("notes.txt"), b"unrelated").unwrap();

    (dir, original)
}

fn register(db: &Db, path: &Path) -> String {
    let meta = DocumentMeta {
        id: String::new(),
        title: Some("report".to_string()),
        author: None,
        pages: Some(1),
        format: Format::Pdf,
        encrypted: false,
    };
    let path = path.to_string_lossy().to_string();
    db.with_mut(|connection| {
        documents::upsert(
            connection,
            documents::Import {
                path: &path,
                meta: &meta,
                size: 9,
                content_hash: "hash-report",
            },
            1,
        )
    })
    .unwrap()
    .meta
    .id
}

fn annotate(db: &Db, document_id: &str, page: u32) -> String {
    db.with_mut(|connection| {
        annotations::add(
            connection,
            document_id,
            page,
            AnnotationKind::Highlight,
            "{}".to_string(),
            None,
            1,
        )
    })
    .unwrap()
    .id
}

#[test]
fn collecting_removes_nothing_at_all() {
    let (dir, original) = fixture();

    let removed = document_reader_lib::editor::gc::collect_edit_copies(&original).unwrap();

    assert!(
        removed.is_empty(),
        "the collector must not name a single file: {removed:?}"
    );
    assert!(
        dir.path().join("report (edited 1).pdf").exists(),
        "a saved edit copy is a document the user was told had been saved (P2-D12 ruling)"
    );
    assert!(original.exists(), "the original must survive");
    assert!(
        dir.path().join("report (edited 1) final.pdf").exists(),
        "a user file sharing the stem must survive"
    );
    assert!(dir.path().join("notes.txt").exists());
}

#[test]
fn deleting_the_last_annotation_leaves_the_saved_copy_on_disk() {
    let (dir, original) = fixture();
    let db = Db::open_in_memory().unwrap();
    let document_id = register(&db, &original);

    let first = annotate(&db, &document_id, 0);
    let second = annotate(&db, &document_id, 1);
    let copy = dir.path().join("report (edited 1).pdf");

    document_reader_lib::commands::remove_annotation_and_gc(&db, &first).unwrap();
    assert!(copy.exists(), "an annotated document keeps its edit copy");

    // The case that used to delete. Removing the last mark is an edit to the annotation list; it
    // is not an instruction to delete a document. The user may have renamed, copied or emailed
    // this file since the save that created it.
    document_reader_lib::commands::remove_annotation_and_gc(&db, &second).unwrap();
    assert!(
        copy.exists(),
        "removing the last annotation must not delete a file the user was told had been saved \
         (P2-D12 ruling, 2026-11-25)"
    );
    assert!(
        std::fs::read(&copy).expect("still readable") == b"%PDF-1.7\n",
        "the surviving copy must be the bytes that were saved, not a truncated remnant"
    );
    assert!(original.exists(), "the original is never a GC candidate");
}

#[test]
fn an_unknown_document_does_not_collect_anything() {
    // The collector resolves the path through the DB, so a document with no row collects nothing
    // rather than falling back to a guess about which directory to sweep.
    let (dir, _original) = fixture();
    let db = Db::open_in_memory().unwrap();

    document_reader_lib::commands::collect_edit_copies_if_unannotated(&db, "no-such-document");

    assert!(dir.path().join("report (edited 1).pdf").exists());
}
