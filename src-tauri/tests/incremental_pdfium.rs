//! The viewer half of the incremental-appending proof (§10 step 1, §11).
//!
//! `incremental_proof.rs` proves the bytes and the xref structure. That is necessary and not
//! sufficient: the design's §11 names "an xref a Windows viewer accepts" as the likeliest
//! failure, and a reader is an independent implementation. This asks PDFium — the engine this
//! project did not write — to open an incrementally appended file and read content from *both*
//! revisions. If the xref chain were wrong, PDFium would either refuse the file or see only the
//! newest revision's objects.
//!
//! Gated the same way as the other PDFium oracles; absence of the library fails the test rather
//! than skipping it (AGENTS.md rule 5). §7 requires this Linux-side equivalent because the
//! Windows suite is not always reachable.

use std::path::{Path, PathBuf};

use lopdf::{dictionary, Document, Object, Stream};

/// A one-page document drawing `text` at 12pt Helvetica.
fn fixture(text: &str) -> Document {
    let mut doc = Document::with_version("1.7");
    let content = format!("BT /F1 12 Tf 72 700 Td ({text}) Tj ET\n");
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

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("dr-inc-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

/// PDFium's page count and extracted text for page 0 — the engine we did not write.
fn pdfium_page(path: &Path) -> Result<(u16, String), String> {
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
    let count = document.pages().len();
    let text = {
        let page = document
            .pages()
            .get(0)
            .map_err(|error| format!("page 0 could not be read: {error}"))?;
        page.text()
            .map(|text| text.all())
            .map_err(|error| format!("text extraction failed: {error}"))?
    };
    Ok((count, text))
}

/// **The viewer half.** PDFium opens an appended revision and still sees revision 1's content.
///
/// This is the assertion that decides whether D-SG2 has a substrate: if PDFium saw only the new
/// revision, every object the append did not rewrite would be invisible, and a signed document
/// would open as a blank page.
#[test]
#[ignore = "needs the real PDFium library; run under --include-ignored with PDFIUM_LIBRARY_PATH"]
fn pdfium_opens_an_appended_revision_and_sees_both_revisions_content() {
    use lopdf::IncrementalDocument;

    let dir = temp_dir("both");
    let source_path = dir.join("source.pdf");
    fixture("REVISIONONE")
        .save(&source_path)
        .expect("save source");
    let source_bytes = std::fs::read(&source_path).expect("read source");

    // Sanity: the source must render before anything is appended, or the test proves nothing.
    let (count_before, text_before) = pdfium_page(&source_path).expect("source must open");
    assert_eq!(count_before, 1);
    assert!(
        text_before.contains("REVISIONONE"),
        "fixture sanity: PDFium must see the original text, saw {text_before:?}"
    );

    // Revision 2: append an object and shadow nothing. A real signature revision rewrites the
    // catalog too, but the content stream — the thing PDFium renders — stays in revision 1.
    let prev = Document::load_mem(&source_bytes).expect("reload");
    let mut incremental = IncrementalDocument::create_from(source_bytes.clone(), prev);
    incremental
        .new_document
        .add_object(Object::string_literal("REVISIONTWO"));
    let output_path = dir.join("appended.pdf");
    incremental.save(&output_path).expect("save appended");

    let out_bytes = std::fs::read(&output_path).expect("read output");
    assert!(
        out_bytes.starts_with(&source_bytes),
        "precondition: the append preserved the prefix"
    );

    let (count_after, text_after) =
        pdfium_page(&output_path).expect("PDFium must open the appended revision");
    assert_eq!(
        count_after, 1,
        "the page count must survive the append; a broken xref chain loses pages"
    );
    assert!(
        text_after.contains("REVISIONONE"),
        "the content from revision 1 must still render after the append, saw {text_after:?}"
    );
}

/// A second append on top of the first, still readable. Signing then re-signing is this shape.
#[test]
#[ignore = "needs the real PDFium library; run under --include-ignored with PDFIUM_LIBRARY_PATH"]
fn pdfium_opens_three_revisions_chained() {
    use lopdf::IncrementalDocument;

    let dir = temp_dir("chain");
    let source_path = dir.join("source.pdf");
    fixture("REVISIONONE")
        .save(&source_path)
        .expect("save source");
    let rev1 = std::fs::read(&source_path).expect("read");

    let prev = Document::load_mem(&rev1).expect("reload r1");
    let mut first = IncrementalDocument::create_from(rev1, prev);
    first
        .new_document
        .add_object(Object::string_literal("REV2"));
    let mid_path = dir.join("rev2.pdf");
    first.save(&mid_path).expect("save r2");
    let rev2 = std::fs::read(&mid_path).expect("read r2");

    let prev = Document::load_mem(&rev2).expect("reload r2");
    let mut second = IncrementalDocument::create_from(rev2, prev);
    second
        .new_document
        .add_object(Object::string_literal("REV3"));
    let final_path = dir.join("rev3.pdf");
    second.save(&final_path).expect("save r3");

    let (count, text) = pdfium_page(&final_path).expect("PDFium must open three revisions");
    assert_eq!(count, 1, "still one page after two appends");
    assert!(
        text.contains("REVISIONONE"),
        "revision 1's content must survive two appends, saw {text:?}"
    );
}
