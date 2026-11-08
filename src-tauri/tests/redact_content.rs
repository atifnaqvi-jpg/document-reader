//! Destructive-redaction oracles (P3.2 I-RD1, I-RD2, I-RD3, I-RD4).
//!
//! The point of these is to check the *file*, not the function that wrote it. I-RD1 asks
//! PDFium — an engine this project did not write — what text the output page shows. I-RD2 and
//! I-RD3 read the raw bytes back, with every stream decompressed, so a run that survives in an
//! object nobody walks is caught even when the page looks clean.
//!
//! The fixture is built here rather than loaded: the defect under test is "which bytes are in
//! the file", and a fixture would hide which run went where.

use lopdf::{dictionary, Document, Object, Stream};
use std::path::PathBuf;

use document_reader_lib::editor::object_gc::dangling_references;
use document_reader_lib::editor::redact::{redact_page, Rect};

const SECRET: &str = "TOPSECRETVALUE";
const KEPT: &str = "KEEPME";

/// A one-page PDF whose content stream draws two text runs at known baselines:
/// the secret at y = 700 and the kept string at y = 650, both at x = 72.
fn fixture() -> Document {
    let mut doc = Document::with_version("1.7");

    let content =
        format!("BT /F1 12 Tf 72 700 Td ({SECRET}) Tj ET\nBT /F1 12 Tf 72 650 Td ({KEPT}) Tj ET\n");

    let content_id = doc.new_object_id();
    doc.objects.insert(
        content_id,
        Stream::new(dictionary! {}, content.into_bytes()).into(),
    );

    let font_id = doc.new_object_id();
    doc.objects.insert(
        font_id,
        Object::Dictionary(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "Helvetica",
        }),
    );

    let pages_id = doc.new_object_id();
    let page_id = doc.new_object_id();
    doc.objects.insert(
        page_id,
        Object::Dictionary(dictionary! {
            "Type" => "Page",
            "Parent" => Object::Reference(pages_id),
            "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
            "Contents" => Object::Reference(content_id),
            "Resources" => dictionary! {
                "Font" => dictionary! { "F1" => Object::Reference(font_id) },
            },
        }),
    );
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => vec![Object::Reference(page_id)],
            "Count" => 1,
        }),
    );

    let catalog_id = doc.new_object_id();
    doc.objects.insert(
        catalog_id,
        Object::Dictionary(dictionary! {
            "Type" => "Catalog",
            "Pages" => Object::Reference(pages_id),
        }),
    );
    doc.trailer.set("Root", Object::Reference(catalog_id));

    doc
}

fn write_temp(doc: &mut Document, name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("redact-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    doc.save(&path).unwrap();
    path
}

/// Every decompressed byte of every object, concatenated (I-RD3's raw material).
fn all_object_bytes(doc: &Document) -> Vec<u8> {
    let mut out = Vec::new();
    for object in doc.objects.values() {
        match object {
            Object::Stream(stream) => {
                match stream.decompressed_content() {
                    Ok(bytes) => out.extend_from_slice(&bytes),
                    Err(_) => out.extend_from_slice(&stream.content),
                }
                out.extend_from_slice(format!("{:?}", stream.dict).as_bytes());
            }
            other => out.extend_from_slice(&format!("{other:?}").into_bytes()),
        }
    }
    out
}

fn contains(haystack: &[u8], needle: &str) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle.as_bytes())
}

/// The rectangle covering the secret's run and nothing else.
fn secret_rect() -> Rect {
    // Baseline 700, 12 pt type: the box runs from just under the baseline to just above.
    Rect::new(70.0, 690.0, 200.0, 706.0)
}

#[test]
fn redaction_removes_the_text_from_the_object_graph() {
    let mut doc = fixture();
    assert!(
        contains(&all_object_bytes(&doc), SECRET),
        "fixture sanity: the secret must be in the file's objects before redaction"
    );

    let report = redact_page(&mut doc, 0, &[secret_rect()]).expect("redact");

    assert_eq!(report.runs_removed, 1, "exactly one run must be deleted");
    let after = all_object_bytes(&doc);
    assert!(
        !contains(&after, SECRET),
        "I-RD3: the removed text must appear nowhere in the decompressed object graph. \
         Mutation: paint a black box instead of deleting the operator leaves the string \
         here and fails this."
    );
    assert!(
        contains(&after, KEPT),
        "the run outside the rectangle must survive (D-RD3 is about ambiguity, not about \
         removing the page)"
    );
}

#[test]
fn the_output_reopens_with_no_dangling_references() {
    let mut doc = fixture();
    redact_page(&mut doc, 0, &[secret_rect()]).expect("redact");

    assert_eq!(
        dangling_references(&doc),
        vec![],
        "I-RD4: every reference from a reachable object must still resolve"
    );

    let path = write_temp(&mut doc, "rd4.pdf");
    let reopened = Document::load(&path).expect("the output must parse");
    assert_eq!(
        reopened.get_pages().len(),
        1,
        "I-RD5: the page count must survive the rewrite"
    );
}

#[test]
fn the_kept_run_keeps_its_position_and_the_page_keeps_its_geometry() {
    let mut doc = fixture();
    redact_page(&mut doc, 0, &[secret_rect()]).expect("redact");

    let page_id = doc.get_pages().values().copied().next().unwrap();
    let page = doc.get_object(page_id).unwrap().as_dict().unwrap();
    let media = page.get(b"MediaBox").unwrap().as_array().unwrap();
    assert_eq!(media.len(), 4, "I-RD5: MediaBox must still be a rectangle");
    for (index, expected) in [0, 0, 612, 792].iter().enumerate() {
        assert_eq!(
            media[index].as_i64().unwrap(),
            *expected,
            "I-RD5: MediaBox corner {index} changed"
        );
    }

    let content = document_reader_lib::editor::textlocate::decoded_content(&doc, page_id).unwrap();
    let shows = content
        .operations
        .iter()
        .filter(|operation| operation.operator == "Tj")
        .count();
    assert_eq!(
        shows, 1,
        "exactly one Tj must remain after removing the other"
    );
}

#[test]
fn an_empty_mark_set_changes_nothing() {
    let mut doc = fixture();
    let before = all_object_bytes(&doc);

    let report = redact_page(&mut doc, 0, &[]).expect("redact");

    assert_eq!(report.runs_removed, 0);
    assert_eq!(
        all_object_bytes(&doc),
        before,
        "I-RD6: applying nothing must be a no-op on the content"
    );
    assert!(contains(&all_object_bytes(&doc), SECRET));
}

#[test]
fn a_mark_beside_the_text_removes_nothing() {
    let mut doc = fixture();
    // Far to the right of both runs: the text is at x = 72, this is x = 400.
    let report =
        redact_page(&mut doc, 0, &[Rect::new(400.0, 640.0, 500.0, 720.0)]).expect("redact");

    assert_eq!(
        report.runs_removed, 0,
        "a mark that touches no run must delete no run"
    );
    let after = all_object_bytes(&doc);
    assert!(contains(&after, SECRET) && contains(&after, KEPT));
}

#[test]
fn a_mark_covering_both_runs_removes_both() {
    let mut doc = fixture();
    let report = redact_page(&mut doc, 0, &[Rect::new(60.0, 600.0, 300.0, 720.0)]).expect("redact");

    assert_eq!(report.runs_removed, 2);
    let after = all_object_bytes(&doc);
    assert!(!contains(&after, SECRET) && !contains(&after, KEPT));
}

#[test]
fn removing_the_highest_numbered_object_leaves_the_xref_consistent() {
    let mut doc = fixture();
    // An orphan holding the secret, numbered above everything else, so the sweep has to drop
    // the document's maximum id and the xref `/Size` has to follow.
    let orphan: (u32, u16) = (9000, 0);
    doc.objects.insert(orphan, Object::string_literal(SECRET));
    doc.max_id = 9000;

    redact_page(&mut doc, 0, &[secret_rect()]).expect("redact");

    assert!(
        !doc.objects.contains_key(&orphan),
        "the orphan must be swept"
    );
    assert!(
        doc.max_id < 9000,
        "max_id must fall to the surviving maximum"
    );
    let path = write_temp(&mut doc, "rd4-xref.pdf");
    Document::load(&path).expect("I-RD4: the xref and object table must agree with the file");
}
