//! **I-RD1**: text removed by a redaction is gone when *PDFium* extracts the output.
//!
//! This is the oracle that matters, and it is deliberately not the same check as
//! `redact_content.rs`. That file proves the bytes are gone from our own object graph; this
//! one asks a separate implementation what the page says. If our writer produced a stream
//! that lopdf reads back correctly and PDFium renders as still carrying the text — a
//! mismatched `Tf`, an unbalanced `BT`/`ET`, a lost `TJ` operand — only this test notices.
//!
//! Gated like the SCOPE-A tests: `#[ignore]`d without the library, exercised by the
//! `--include-ignored` gate with `PDFIUM_LIBRARY_PATH` set. Absence of the library fails
//! these tests rather than skipping them, per AGENTS.md rule 5.

use std::path::{Path, PathBuf};

use lopdf::{dictionary, Document, Object, Stream};

use document_reader_lib::editor::redact::{redact_page, Rect};

const SECRET: &str = "TOPSECRETVALUE";
const KEPT: &str = "KEEPME";

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("dr-rd1-{tag}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

/// Two text runs at known positions, both drawn in Helvetica so PDFium extracts them.
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

/// Extracts page `page`'s text through PDFium — the engine we did not write.
fn pdfium_text(path: &Path, page: u32) -> Result<String, String> {
    use pdfium_render::prelude::*;

    let library = match std::env::var("PDFIUM_LIBRARY_PATH") {
        Ok(library) => library,
        Err(_) if cfg!(windows) => "pdfium.dll".to_string(),
        Err(_) => "resources/libpdfium.so".to_string(),
    };

    let bindings = Pdfium::bind_to_library(&library)
        .map_err(|error| format!("PDFIUM_UNAVAILABLE: {error}"))?;
    let pdfium = Pdfium::new(bindings);
    let document = pdfium
        .load_pdf_from_file(&path.display().to_string(), None)
        .map_err(|error| format!("open failed: {error}"))?;
    let page = document
        .pages()
        .get(page as u16)
        .map_err(|error| format!("page {page} could not be read: {error}"))?;

    page.text()
        .map(|text| text.all())
        .map_err(|error| format!("text extraction failed: {error}"))
}

/// **I-RD1**: the redacted text is not in PDFium's extraction of the output.
#[test]
#[ignore = "needs the real PDFium library; run under --include-ignored with PDFIUM_LIBRARY_PATH"]
fn pdfium_extracts_nothing_from_a_redacted_region() {
    let dir = temp_dir("secret");
    let mut doc = fixture();
    let source = dir.join("source.pdf");
    doc.save(&source).expect("save source");

    let before = pdfium_text(&source, 0).expect("extract the source");
    assert!(
        before.contains(SECRET) && before.contains(KEPT),
        "fixture sanity: PDFium must see both strings before redaction, saw {before:?}"
    );

    redact_page(&mut doc, 0, &[Rect::new(70.0, 690.0, 200.0, 706.0)]).expect("redact");
    let output = dir.join("redacted.pdf");
    doc.save(&output).expect("save output");

    let after = pdfium_text(&output, 0).expect("extract the output");
    assert!(
        !after.contains(SECRET),
        "I-RD1: PDFium must not extract the redacted string from the output, got {after:?}"
    );
    assert!(
        after.contains(KEPT),
        "I-RD1/D-RD3: the text outside the mark must still be extractable, got {after:?}"
    );
}

/// **I-RD5** and **I-RD4**: the output opens without repair and keeps its page count.
#[test]
#[ignore = "needs the real PDFium library; run under --include-ignored with PDFIUM_LIBRARY_PATH"]
fn the_redacted_output_opens_in_pdfium_with_the_same_page_count() {
    let dir = temp_dir("reopen");
    let mut doc = fixture();
    redact_page(&mut doc, 0, &[Rect::new(70.0, 690.0, 200.0, 706.0)]).expect("redact");

    let output = dir.join("redacted.pdf");
    doc.save(&output).expect("save output");

    use pdfium_render::prelude::*;
    let library = match std::env::var("PDFIUM_LIBRARY_PATH") {
        Ok(library) => library,
        Err(_) if cfg!(windows) => "pdfium.dll".to_string(),
        Err(_) => "resources/libpdfium.so".to_string(),
    };
    let bindings = Pdfium::bind_to_library(&library)
        .map_err(|error| format!("PDFIUM_UNAVAILABLE: {error}"))
        .unwrap();
    let pdfium = Pdfium::new(bindings);
    let document = pdfium
        .load_pdf_from_file(&output.display().to_string(), None)
        .expect("I-RD5: the output must open, not require repair");

    assert_eq!(document.pages().len(), 1, "the page count must survive");

    let page = document.pages().get(0).expect("the surviving page");
    let width = page.width().value;
    let height = page.height().value;
    assert!(
        (width - 612.0).abs() < 1.0 && (height - 792.0).abs() < 1.0,
        "I-RD5: page geometry must survive the rewrite, saw {width} x {height}"
    );
}

/// **I-RD2**: a raw byte scan of the *saved file* finds no trace of the redacted string.
///
/// I-RD1 asks PDFium what the page says; this asks the bytes what they contain. Both are
/// needed: a stream that PDFium renders correctly could still carry the old string in a
/// comment, an incremental-save remnant, or an object nothing draws. Scanning the file rather
/// than the in-memory document is what makes this a check on the writer.
#[test]
#[ignore = "needs the real PDFium library; run under --include-ignored with PDFIUM_LIBRARY_PATH"]
fn the_saved_file_contains_no_trace_of_the_redacted_string() {
    use document_reader_lib::editor::redact::{apply_redactions, Rect};

    let dir = temp_dir("ird2");
    let mut doc = fixture();
    // Leave metadata behind too, so the sanitize half of apply is exercised in the same file.
    let info_id = doc.add_object(dictionary! { "Author" => Object::string_literal("LEAKYNAME") });
    doc.trailer.set("Info", lopdf::Object::Reference(info_id));

    apply_redactions(&mut doc, &[(0, vec![Rect::new(70.0, 690.0, 200.0, 706.0)])]).expect("apply");

    let output = dir.join("redacted.pdf");
    doc.save(&output).expect("save output");

    let bytes = std::fs::read(&output).expect("read the saved file");
    let text = String::from_utf8_lossy(&bytes);

    assert!(
        !text.contains(SECRET),
        "I-RD2: the redacted string must not appear anywhere in the saved bytes"
    );
    assert!(
        !text.contains("LEAKYNAME"),
        "D-RD5: the metadata must not appear anywhere in the saved bytes"
    );
    assert!(
        text.contains(KEPT),
        "the unmarked text must still be present, or the test proves nothing"
    );
}
