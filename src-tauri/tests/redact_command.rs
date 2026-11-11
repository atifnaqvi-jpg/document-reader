//! P3.2 SCOPE-B: the redact **commands** reach the editor, end to end.
//!
//! `tests/redact_content.rs` proves `apply_redactions` removes content, and `tests/redact_pdfium.rs`
//! asks PDFium whether the text is really gone. Both call the editor module directly, and that is
//! exactly the blind spot this file exists to close: `sign_save` was once fully built, fully tested
//! and completely unreachable, because every test of it called the module and none called the
//! command. The signer shipped invisible for a unit.
//!
//! So these tests drive `commands::redact_*_with`, which is the body of the registered Tauri command.
//! `redact_mark(doc, page, rect)` from the frontend and `redact_mark_with(&db, ...)` here are the same
//! function, so a command that stopped delegating to the editor fails here rather than in a user's
//! hands.
//!
//! What each test is for, stated so a future reader does not delete one as redundant:
//!
//! * **reachability** — a mark lands in the DB and apply writes a PDF whose text is gone. If the
//!   command ever stopped calling `redact::apply_redactions`, this fails and nothing else does.
//! * **the oracles over the command path** — I-RD3 (no survivor in the object graph) and I-RD6
//!   (a second apply is a no-op that says so), asserted on the *file the command wrote*, not on a
//!   document a test built.
//! * **the never-overwrite rule** — the source the command was pointed at is byte-identical
//!   afterwards, which is I-RD5's "the original is intact" and D-RD1's "apply produces a new file".
//! * **the certification gate** — a `P=1` document refuses the mark, because applying it would
//!   rewrite the content stream and no level permits that.

use std::path::{Path, PathBuf};

use document_reader_lib::commands::{
    redact_apply_with, redact_mark_with, redact_marks_with, redact_search_with, redact_unmark_with,
};
use document_reader_lib::db::{documents, Db};
use document_reader_lib::editor::redact::Rect;
use document_reader_lib::model::{DocumentMeta, Format, RedactionKind};
use lopdf::{dictionary, Document, Object, Stream};

const SECRET: &str = "TOPSECRETVALUE";
const KEPT: &str = "KEEPME";

/// Registers `path` in a fresh in-memory DB and returns the document id.
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

/// Two text runs at known positions, plus a font PDFium can extract, written to disk.
fn source_on_disk(name: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join(name);

    let mut document = Document::with_version("1.7");
    let content =
        format!("BT /F1 12 Tf 72 700 Td ({SECRET}) Tj ET\nBT /F1 12 Tf 72 650 Td ({KEPT}) Tj ET\n");
    let content_id = document.add_object(Stream::new(dictionary! {}, content.into_bytes()));
    let font_id = document.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
    });
    let pages_id = document.new_object_id();
    let page_id = document.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        "Contents" => Object::Reference(content_id),
        "Resources" => dictionary! { "Font" => dictionary! { "F1" => Object::Reference(font_id) } },
    });
    document.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => vec![Object::Reference(page_id)],
            "Count" => 1,
        }),
    );
    let catalog_id = document.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => Object::Reference(pages_id),
    });
    document.trailer.set("Root", Object::Reference(catalog_id));
    document.save(&path).expect("write source");
    (dir, path)
}

/// The rectangle covering the secret's run and nothing else (see `redact_content.rs`).
fn secret_rect() -> Rect {
    Rect::new(70.0, 690.0, 200.0, 706.0)
}

/// Every decompressed byte of every object in a saved file (I-RD3's raw material).
fn all_object_bytes(path: &Path) -> Vec<u8> {
    let document = Document::load(path).expect("reload the written file");
    let mut out = Vec::new();
    for object in document.objects.values() {
        match object {
            Object::Stream(stream) => {
                match stream.decompressed_content() {
                    Ok(bytes) => out.extend_from_slice(&bytes),
                    Err(_) => out.extend_from_slice(&stream.content),
                }
                out.extend_from_slice(format!("{:?}", stream.dict).as_bytes());
            }
            other => out.extend_from_slice(format!("{other:?}").as_bytes()),
        }
    }
    out
}

fn contains(haystack: &[u8], needle: &str) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle.as_bytes())
}

#[test]
fn the_mark_command_writes_a_pending_mark_and_touches_no_file() {
    let (_dir, path) = source_on_disk("mark.pdf");
    let db = Db::open_in_memory().unwrap();
    let id = register(&db, &path);
    let before = std::fs::read(&path).unwrap();

    let mark = redact_mark_with(&db, &id, 0, secret_rect()).expect("mark");

    assert_eq!(mark.kind, RedactionKind::Rect);
    assert_eq!(redact_marks_with(&db, &id).unwrap().len(), 1);
    assert_eq!(
        std::fs::read(&path).unwrap(),
        before,
        "marking a rectangle must not write to the document (D-RD1)"
    );
}

#[test]
fn the_apply_command_removes_the_text_and_writes_a_new_file() {
    let (_dir, path) = source_on_disk("apply.pdf");
    let db = Db::open_in_memory().unwrap();
    let id = register(&db, &path);
    let source_before = std::fs::read(&path).unwrap();
    assert!(contains(&all_object_bytes(&path), SECRET), "fixture must contain the secret to start with");

    redact_mark_with(&db, &id, 0, secret_rect()).expect("mark");
    let outcome = redact_apply_with(&db, &id).expect("apply");

    // The source is untouched (D-RD1/I-RD5) and the output is a different path.
    assert_eq!(
        std::fs::read(&path).unwrap(),
        source_before,
        "apply must never modify the source document"
    );
    assert_ne!(
        Path::new(&outcome.path),
        path.as_path(),
        "apply must write a new file, not the original"
    );
    assert!(Path::new(&outcome.path).exists());
    assert!(outcome.runs_removed > 0, "the secret's run must be deleted");

    // I-RD3 over the file the command wrote: the removed text survives nowhere in the object graph.
    let bytes = all_object_bytes(Path::new(&outcome.path));
    assert!(
        !contains(&bytes, SECRET),
        "I-RD3: the redacted text is still present somewhere in the output's object graph"
    );
    assert!(
        contains(&bytes, KEPT),
        "the content outside the mark must survive (I-RD5)"
    );

    // I-RD7: no redaction marks survive as marks.
    assert!(
        !contains(&bytes, "/Redact"),
        "I-RD7: the output must carry no /Redact annotation"
    );
}

#[test]
fn applying_clears_the_marks_so_the_next_apply_reports_nothing() {
    let (_dir, path) = source_on_disk("twice.pdf");
    let db = Db::open_in_memory().unwrap();
    let id = register(&db, &path);

    redact_mark_with(&db, &id, 0, secret_rect()).expect("mark");
    let first = redact_apply_with(&db, &id).expect("first apply");
    assert_eq!(first.marks_cleared, 1);
    assert_eq!(
        redact_marks_with(&db, &id).unwrap().len(),
        0,
        "apply must clear the marks it consumed"
    );

    // I-RD6: with no marks left there is nothing to apply, and that is reported rather than
    // producing a second byte-identical file the user cannot distinguish from the first.
    let second = redact_apply_with(&db, &id);
    assert!(
        second.is_err(),
        "a second apply with no marks must report that there is nothing to do"
    );
}

#[test]
fn re_marking_already_redacted_text_reports_nothing_to_apply() {
    // I-RD6's real shape: the same rectangle exists, but the file has already had it removed, so
    // the rewrite finds no content at that rect. Reporting success here is the silent no-op the
    // design forbids.
    let (_dir, path) = source_on_disk("noop.pdf");
    let db = Db::open_in_memory().unwrap();
    let id = register(&db, &path);

    redact_mark_with(&db, &id, 0, secret_rect()).expect("mark");
    redact_apply_with(&db, &id).expect("first apply");

    // Point the document at the output, then apply the same rectangle again.
    let output = std::fs::read_dir(path.parent().unwrap())
        .unwrap()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .find(|candidate| {
            candidate
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.contains("(redacted 1)"))
        })
        .expect("the apply wrote a redacted copy");
    let id2 = register(&db, &output);

    redact_mark_with(&db, &id2, 0, secret_rect()).expect("re-mark");
    let second = redact_apply_with(&db, &id2);
    assert!(
        second.is_err(),
        "applying to an already-redacted file must report nothing to apply (I-RD6), not write a \
         second copy"
    );
}

#[test]
fn a_mark_on_a_certified_document_is_refused() {
    // I-SG5: redaction rewrites the content stream, and `permits` refuses it at every level. The
    // refusal has to happen at the command, before a mark exists, or the user draws a page of
    // rectangles and only finds out at apply time.
    let (_dir, path) = source_on_disk("certified.pdf");
    let db = Db::open_in_memory().unwrap();
    // The uncertified source is registered by `register` below on the certified output instead;
    // `sign_save_with` needs a path in the DB only for the document, not the signature.
    let _uncertified = register(&db, &path);

    // Certify at P=3, the most permissive level; redaction is still refused there.
    let identity = {
        let fixture =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/signing/signer.pfx");
        let bytes = std::fs::read(&fixture).expect("signer fixture");
        document_reader_lib::editor::signature::pfx::import_bytes(&bytes, "correct-horse")
            .expect("the fixture PFX opens")
            .identities
            .into_iter()
            .next()
            .expect("one identity")
    };
    let options = document_reader_lib::editor::signature::sign::SignOptions {
        certification: Some(
            document_reader_lib::editor::signature::sign::Certification::doc_mdp(3).unwrap(),
        ),
        reason: None,
        location: None,
    };
    let signed =
        document_reader_lib::editor::signature::sign::sign_save_with(&path, &identity, options)
            .expect("certify");

    // Register the certified output, and mark on that.
    let certified_id = register(&db, Path::new(&signed.path));
    let refused = redact_mark_with(&db, &certified_id, 0, secret_rect());

    let error = refused.expect_err("P=3 still forbids redaction");
    assert_eq!(error.code(), "CERTIFICATION_REFUSES_EDIT");
    assert_eq!(
        redact_marks_with(&db, &certified_id).unwrap().len(),
        0,
        "a refused mark must not be stored"
    );
}

/// Certifies `path` at `level` (3 = most permissive) and returns the signed output's path.
fn certify(path: &Path, level: i64) -> PathBuf {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/signing/signer.pfx");
    let bytes = std::fs::read(&fixture).expect("signer fixture");
    let identity =
        document_reader_lib::editor::signature::pfx::import_bytes(&bytes, "correct-horse")
            .expect("the fixture PFX opens")
            .identities
            .into_iter()
            .next()
            .expect("one identity");
    let options = document_reader_lib::editor::signature::sign::SignOptions {
        certification: Some(
            document_reader_lib::editor::signature::sign::Certification::doc_mdp(level).unwrap(),
        ),
        reason: None,
        location: None,
    };
    let signed =
        document_reader_lib::editor::signature::sign::sign_save_with(path, &identity, options)
            .expect("certify");
    PathBuf::from(signed.path)
}

#[test]
fn the_apply_command_refuses_a_certified_document_even_with_marks_pending() {
    // The apply gate, tested behaviourally and separately from the mark gate. Worth its own test:
    // a mutant that regated `redact_apply_with` with the permissive `EditKind::Signature` passed all
    // nine other tests in this file, because the certified-document test only exercises the mark.
    // The marks are inserted straight into the DB, so nothing but the gate stands between the user
    // and a rewrite of a certified file.
    let (_dir, path) = source_on_disk("apply-certified.pdf");
    let db = Db::open_in_memory().unwrap();
    let certified = certify(&path, 3);
    let id = register(&db, &certified);

    db.with(|connection| {
        document_reader_lib::db::redactions::add(
            connection,
            &id,
            0,
            RedactionKind::Rect,
            secret_rect(),
            None,
            1,
        )
    })
    .expect("mark inserted directly");

    let before = std::fs::read(&certified).expect("read the certified file");

    let refused = redact_apply_with(&db, &id);
    let error = refused.expect_err("P=3 still forbids applying a redaction");
    assert_eq!(error.code(), "CERTIFICATION_REFUSES_EDIT");
    assert_eq!(
        std::fs::read(&certified).expect("re-read the certified file"),
        before,
        "the certified file must be untouched"
    );
    assert_eq!(
        redact_marks_with(&db, &id).unwrap().len(),
        1,
        "a refused apply must not clear the user's marks"
    );
}

#[test]
fn the_search_command_turns_a_pattern_into_marks_that_then_remove_the_text() {
    // D-RD6 through the command: the pattern is matched by the product's locator, the hits become
    // marks, and applying those marks removes the matched text. This is the whole search-and-redact
    // path, which only the UI's two calls would otherwise exercise.
    let (_dir, path) = source_on_disk("search.pdf");
    let db = Db::open_in_memory().unwrap();
    let id = register(&db, &path);

    let marks = redact_search_with(&db, &id, SECRET).expect("search");
    assert_eq!(marks.len(), 1, "one occurrence of {SECRET} on the page");
    assert_eq!(marks[0].kind, RedactionKind::Pattern);
    assert_eq!(marks[0].pattern.as_deref(), Some(SECRET));

    let outcome = redact_apply_with(&db, &id).expect("apply");
    let bytes = all_object_bytes(Path::new(&outcome.path));
    assert!(!contains(&bytes, SECRET), "I-RD3 over the search path");
    assert!(contains(&bytes, KEPT));
}

#[test]
fn a_pattern_that_occurs_nowhere_marks_nothing_and_says_so_by_returning_empty() {
    let (_dir, path) = source_on_disk("nomatch.pdf");
    let db = Db::open_in_memory().unwrap();
    let id = register(&db, &path);

    let marks = redact_search_with(&db, &id, "this-text-is-not-in-the-document").expect("search");
    assert!(
        marks.is_empty(),
        "a pattern with no hits must produce no marks"
    );
    assert_eq!(redact_marks_with(&db, &id).unwrap().len(), 0);
}

#[test]
fn a_mark_can_be_removed_before_applying() {
    let (_dir, path) = source_on_disk("unmark.pdf");
    let db = Db::open_in_memory().unwrap();
    let id = register(&db, &path);

    let mark = redact_mark_with(&db, &id, 0, secret_rect()).expect("mark");
    assert!(redact_unmark_with(&db, &mark.id).expect("unmark"));
    assert_eq!(redact_marks_with(&db, &id).unwrap().len(), 0);
    assert!(
        !redact_unmark_with(&db, &mark.id).expect("second unmark"),
        "removing a mark twice is a no-op that reports it"
    );
}

#[test]
fn a_rectangle_arriving_unnormalised_is_normalised_by_the_command() {
    // serde builds `Rect` field by field, so a rectangle can arrive with its corners reversed. The
    // command re-orders it; without that, `intersects` would treat the mark as covering everything
    // outside the drawn area.
    let (_dir, path) = source_on_disk("reverse.pdf");
    let db = Db::open_in_memory().unwrap();
    let id = register(&db, &path);

    let reversed = Rect {
        x0: 200.0,
        y0: 706.0,
        x1: 70.0,
        y1: 690.0,
    };
    let mark = redact_mark_with(&db, &id, 0, reversed).expect("mark");
    // The stored string, not the parsed rect: `from_storage` normalises on read, so parsing first
    // would assert the reader's behaviour rather than what was written.
    assert_eq!(
        mark.rect, "70,690,200,706",
        "the command must store the rectangle corner-ordered"
    );
}
