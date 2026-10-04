//! Export and watermark verified through PDFium (P2-F2/F5).
//!
//! These paths produce a *file the user keeps*, so the useful question is not whether lopdf
//! can re-read what lopdf wrote -- it can, even for output PDFium would reject. The
//! assertions here read the result back with PDFium, which is what actually renders it.

use document_reader_lib::editor::annotate::apply_annotations;
use document_reader_lib::editor::export::{export_with_flatten, page_png, watermark};
use document_reader_lib::model::{Annotation, AnnotationKind};
use document_reader_lib::provider::PdfProvider;

mod support;

/// A two-page document whose pages carry text, so rasterizing has something to draw.
fn sample() -> Vec<u8> {
    support::build_pdf(2)
}

fn write_temp(name: &str, bytes: &[u8]) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("docreader-export-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, bytes).unwrap();
    path
}

/// PDFium-guarded: without the library there is nothing to verify against, and a silent
/// pass would be worse than a skip (CI asserts the skip count is zero on Windows).
fn pdfium_ready() -> bool {
    if !PdfProvider::pdfium_available() {
        eprintln!("skipping: PDFium not available on this machine");
        return false;
    }
    true
}

#[test]
fn a_watermarked_document_still_opens_and_renders_in_pdfium() {
    if !pdfium_ready() {
        return;
    }

    let mut document = lopdf::Document::load_mem(&sample()).unwrap();
    let stamped = watermark(&mut document, "DRAFT").unwrap();
    assert_eq!(stamped, 2, "every page is stamped");

    let mut bytes = Vec::new();
    document.save_to(&mut bytes).unwrap();
    let path = write_temp("watermarked.pdf", &bytes);

    // The page count is the cheapest proof the tree survived the extra content streams.
    assert_eq!(PdfProvider::page_count(&path).unwrap(), 2);

    // Rasterizing proves the appended content parses: an unbalanced q/Q or a bad `Tf`
    // operand makes PDFium fail the page render, which is exactly the failure mode a
    // hand-built content stream risks.
    let raster = PdfProvider::render_page_at(&path, 0, 1.0).unwrap();
    assert_eq!(&raster.bytes[..8], b"\x89PNG\r\n\x1a\n");
    assert!(raster.width > 0 && raster.height > 0);
}

#[test]
fn watermarking_twice_still_produces_a_renderable_document() {
    if !pdfium_ready() {
        return;
    }

    // Guards the content-array append: a second pass must add another stream rather than
    // overwrite the first, and the page must still render.
    let mut document = lopdf::Document::load_mem(&sample()).unwrap();
    watermark(&mut document, "FIRST").unwrap();
    watermark(&mut document, "SECOND").unwrap();

    let mut bytes = Vec::new();
    document.save_to(&mut bytes).unwrap();
    let path = write_temp("watermarked-twice.pdf", &bytes);

    assert_eq!(PdfProvider::page_count(&path).unwrap(), 2);
    PdfProvider::render_page_at(&path, 1, 1.0).unwrap();
}

#[test]
fn flattening_removes_the_annotation_objects_but_keeps_the_page_renderable() {
    if !pdfium_ready() {
        return;
    }

    // Flatten on a document with no annotations is the degenerate case, and it must not
    // disturb the pages: it is the shape the UI hits when the user exports an unannotated
    // file.
    let (bytes, flattened) = export_with_flatten(&sample(), &[], None).unwrap();
    assert_eq!(flattened, 0);

    let path = write_temp("flattened-plain.pdf", &bytes);
    assert_eq!(PdfProvider::page_count(&path).unwrap(), 2);

    let counts = PdfProvider::annotation_counts(&path, 2).unwrap();
    assert_eq!(counts, vec![0, 0], "nothing to flatten means nothing added");

    let raster = PdfProvider::render_page_at(&path, 0, 1.0).unwrap();
    assert!(raster.width > 0 && raster.height > 0);
}

#[test]
fn flattening_bakes_real_annotations_so_they_no_longer_exist_as_objects() {
    if !pdfium_ready() {
        return;
    }

    use document_reader_lib::editor::annotate::apply_annotations;
    use document_reader_lib::model::{Annotation, AnnotationKind};

    let annotation = |page: u32| Annotation {
        id: format!("a{page}"),
        document_id: "doc".to_string(),
        page,
        kind: AnnotationKind::Highlight,
        data: r##"{"v":1,"rects":[[20.0,600.0,70.0,620.0]],"color":"#fff3b0"}"##.to_string(),
        text: None,
        created_at: 1,
        modified_at: 1,
    };

    let annotated = apply_annotations(&sample(), &[annotation(0), annotation(1)]).unwrap();

    // The starting point is what the user actually has: two pages, one mark each.
    let annotated_path = write_temp("flatten-before.pdf", &annotated);
    assert_eq!(
        PdfProvider::annotation_counts(&annotated_path, 2).unwrap(),
        vec![1, 1],
    );

    let rows = [annotation(0), annotation(1)];
    let (flattened, count) = export_with_flatten(&annotated, &rows, None).unwrap();
    assert_eq!(count, 2, "both annotations are flattened");

    let path = write_temp("flatten-after.pdf", &flattened);
    assert_eq!(
        PdfProvider::annotation_counts(&path, 2).unwrap(),
        vec![0, 0],
        "the annotation objects are gone, which is what makes the export one-way"
    );

    // Baked, not merely deleted: the page must still render, and its pixels must differ
    // from an unannotated page of the same document. Without this the test would pass even
    // if flattening threw the marks away entirely.
    let plain = write_temp("flatten-plain.pdf", &sample());
    let baked_png = PdfProvider::render_page_at(&path, 0, 1.0).unwrap().bytes;
    let plain_png = PdfProvider::render_page_at(&plain, 0, 1.0).unwrap().bytes;
    assert_ne!(
        baked_png, plain_png,
        "a flattened page must still show the mark; identical pixels mean it was dropped"
    );
}

#[test]
fn flattening_with_a_watermark_applies_both() {
    if !pdfium_ready() {
        return;
    }

    let (bytes, _) = export_with_flatten(&sample(), &[], Some("CONFIDENTIAL")).unwrap();
    let path = write_temp("flattened-watermarked.pdf", &bytes);

    // Both operations ran on one document: the watermark adds content streams, flatten
    // removes /Annots, and the two must not corrupt each other.
    assert_eq!(PdfProvider::page_count(&path).unwrap(), 2);
    PdfProvider::render_page_at(&path, 0, 1.0).unwrap();
}

#[test]
fn page_png_exports_a_real_png() {
    if !pdfium_ready() {
        return;
    }

    let path = write_temp("for-png.pdf", &sample());
    let png = page_png(&path, 0, 1.0).unwrap();

    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    assert!(png.len() > 100, "a rendered page is not an empty image");
}

#[test]
fn flattening_leaves_annotations_it_did_not_write_alone() {
    // A PDF can arrive with its own /Annots -- a commented download, a form, a signature.
    // We can only redraw what we authored, so the safe behaviour is to bake ours and leave
    // the rest in place. Dropping them would destroy marks the user never asked us to touch.
    let mut document = lopdf::Document::load_mem(&sample()).unwrap();

    let page_id = *document.get_pages().get(&1).unwrap();
    let mut foreign = lopdf::Dictionary::new();
    foreign.set("Type", lopdf::Object::Name(b"Annot".to_vec()));
    foreign.set("Subtype", lopdf::Object::Name(b"Highlight".to_vec()));
    foreign.set(
        "Rect",
        lopdf::Object::Array(vec![
            lopdf::Object::Real(10.0),
            lopdf::Object::Real(10.0),
            lopdf::Object::Real(50.0),
            lopdf::Object::Real(50.0),
        ]),
    );
    foreign.set("NM", lopdf::Object::string_literal("their-comment"));
    let foreign_id = document.add_object(foreign);

    // Attach it the way the app attaches its own, so the array shape is identical and the
    // only difference between the two marks is /NM.
    document
        .get_object_mut(page_id)
        .unwrap()
        .as_dict_mut()
        .unwrap()
        .set(
            "Annots",
            lopdf::Object::Array(vec![lopdf::Object::Reference(foreign_id)]),
        );

    let our_row = Annotation {
        id: "ours-1".to_string(),
        document_id: "doc".to_string(),
        page: 0,
        kind: AnnotationKind::Highlight,
        data: r##"{"v":1,"rects":[[100.0,500.0,150.0,520.0]],"color":"#fff3b0"}"##.to_string(),
        text: None,
        created_at: 1,
        modified_at: 1,
    };

    let mut bytes = Vec::new();
    document.save_to(&mut bytes).unwrap();
    let annotated = apply_annotations(&bytes, std::slice::from_ref(&our_row)).unwrap();

    let (flattened, count) =
        export_with_flatten(&annotated, std::slice::from_ref(&our_row), None).unwrap();
    assert_eq!(count, 1, "only our mark is baked");

    let result = lopdf::Document::load_mem(&flattened).unwrap();
    let page = *result.get_pages().get(&1).unwrap();
    let remaining = result.get_page_annotations(page).unwrap();
    assert_eq!(remaining.len(), 1, "the foreign annotation is still there");

    let name = remaining[0].get(b"NM").unwrap().as_str().unwrap();
    assert_eq!(
        name, b"their-comment",
        "the survivor is theirs, not ours: ours must be the one removed"
    );
}

#[test]
fn an_empty_watermark_is_rejected_rather_than_written() {
    // A blank watermark would silently produce a file the user believes is stamped.
    let mut document = lopdf::Document::load_mem(&sample()).unwrap();
    assert!(watermark(&mut document, "   ").is_err());
    assert!(watermark(&mut document, "").is_err());
}
