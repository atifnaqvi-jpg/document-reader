//! The D-N2 disclosure is *visible*, proved by a renderer rather than by the bytes.
//!
//! ## Why this exists when the label already has tests
//!
//! `editor/export.rs` proves `signature_label` appends one content stream, installs a font, encodes
//! the em dash as WinAnsi `0x97`, and writes the exact sentence. Every one of those assertions can
//! pass on a file where a user sees nothing:
//!
//! * a `Tf` naming a font the page never registers draws no text and raises no error;
//! * a `Td` outside the `/MediaBox` draws off-page;
//! * a fill set to the background colour draws text that is present and invisible;
//! * a stream appended after a page's own `Q` with an unbalanced graphics state can be swallowed.
//!
//! The ruling (D-N2) is about what a person receives in an email, so the only honest oracle is a
//! PDF viewer's raster. This test renders the saved file twice -- once without the label, once with
//! it -- and requires the label to have changed dark pixels in the region it claims to occupy.
//!
//! ## The shape of the oracle, and what it deliberately does not claim
//!
//! It measures *differential ink in a rectangle*, not glyph identity. That is weaker than OCR and
//! stronger than a byte search, and it is chosen because it is the property a disclosure needs:
//! something legible appeared where the code says it did. A stronger oracle (comparing against a
//! known-good raster) would break on every PDFium or Helvetica metric change and say nothing more
//! about the ruling.
//!
//! Both renders go through `PdfProvider::render_page_at`, the same path the viewer uses, at a scale
//! where 6pt text is measurable. The label's own size is taken from `SIGNATURE_LABEL_FONT_SIZE`
//! rather than hardcoded, so a change to the constant moves the measurement instead of silently
//! measuring the wrong band.

use std::path::PathBuf;

use document_reader_lib::editor::export::{
    signature_label, SIGNATURE_LABEL_FONT_SIZE, VISUAL_SIGNATURE_LABEL,
};
use document_reader_lib::provider::{PdfProvider, RasterizedPage};

use lopdf::{dictionary, Document, Object, Stream};

/// A 595x842pt blank page (A4 at 72dpi), matching the helper the export unit tests use. Written to
/// a temp file because the oracle is a *renderer*, and a renderer needs a file on disk.
fn blank_page_pdf() -> Document {
    let mut document = Document::with_version("1.4");
    let pages_id = document.new_object_id();
    let content = document.add_object(Stream::new(lopdf::Dictionary::new(), Vec::new()));
    let page_id = document.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => Object::Reference(pages_id),
        "MediaBox" => Object::Array(vec![
            Object::Integer(0),
            Object::Integer(0),
            Object::Integer(595),
            Object::Integer(842),
        ]),
        "Contents" => Object::Reference(content),
    });
    document.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => Object::Array(vec![Object::Reference(page_id)]),
            "Count" => Object::Integer(1),
        }),
    );
    // `/Root` must reference a real Catalog object. An inline dictionary parses but leaves
    // `get_pages()` empty, which fails later for an unrelated reason.
    let catalog_id = document.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => Object::Reference(pages_id),
    });
    document.trailer.set("Root", Object::Reference(catalog_id));
    document.renumber_objects();
    document
}

/// Writes a document to a unique temp path and returns it. No `tempfile` dependency: the crate does
/// not use one, and a path built from the PID and a counter is enough for a test that cleans up.
fn write_temp(document: &mut Document, tag: &str) -> PathBuf {
    let mut path = std::env::temp_dir();
    path.push(format!(
        "docureader-signlabel-{tag}-{}.pdf",
        std::process::id()
    ));
    document
        .save(&path)
        .unwrap_or_else(|e| panic!("cannot write {}: {e}", path.display()));
    path
}

/// Grayscale pixels darker than mid-grey, inside a page-space rectangle.
///
/// Page space is y-up from the bottom-left; a raster is y-down from the top-left, so the vertical
/// bounds are mirrored here. Getting that wrong is the classic way this kind of test measures the
/// wrong band and then passes because a blank page is uniformly blank.
fn dark_pixels_in(page: &RasterizedPage, scale: f32, rect: (f32, f32, f32, f32)) -> usize {
    let image = image::load_from_memory(&page.bytes)
        .expect("render_page_at returns an encoded image")
        .to_luma8();
    let (x0, y0, x1, y1) = rect;
    assert!(
        (x1 - x0).abs() > 0.0 && (y1 - y0).abs() > 0.0,
        "the measurement rectangle must have area, or every count is zero for the wrong reason"
    );
    let left = (x0.min(x1) * scale).floor().max(0.0) as u32;
    let right = ((x0.max(x1) * scale).ceil() as u32).min(image.width());
    // Page-space top edge -> raster row.
    let top = ((page.height as f32 - y1.max(y0) * scale).max(0.0)) as u32;
    let bottom = ((page.height as f32 - y0.min(y1) * scale).min(page.height as f32)) as u32;

    let mut count = 0;
    for y in top..bottom.min(image.height()) {
        for x in left..right.min(image.width()) {
            if image.get_pixel(x, y).0[0] < 128 {
                count += 1;
            }
        }
    }
    count
}

/// The label's visible ink, measured through PDFium, on the page it was written to.
///
/// The assertion is differential and regional: the same rectangle on the same page, rendered before
/// and after, must go from empty to non-empty. A label written anywhere else, in white, or with an
/// unregistered font leaves the rectangle empty and fails.
#[test]
#[ignore = "requires a real PDFium library; `cargo test -- --include-ignored`"]
fn the_label_is_visible_when_a_viewer_renders_the_saved_file() {
    // The position and size the test writes. 6pt text is small, so the band is deliberately a few
    // points taller than the type size to survive rasterisation without measuring a single row.
    const X: f32 = 72.0;
    const BASE: f32 = 100.0;
    const SCALE: f32 = 4.0;

    let mut with_label = blank_page_pdf();
    signature_label(&mut with_label, 0, X, BASE, SIGNATURE_LABEL_FONT_SIZE).expect("label writes");
    let with_path = write_temp(&mut with_label, "with");

    let mut without = blank_page_pdf();
    let without_path = write_temp(&mut without, "without");

    let rendered_with =
        PdfProvider::render_page_at(&with_path, 0, SCALE).expect("the labelled page renders");
    let rendered_without =
        PdfProvider::render_page_at(&without_path, 0, SCALE).expect("the blank page renders");

    // The band the label occupies: from a little below the baseline (descenders) to a little above
    // the cap height, across the width the sentence needs at this size. Wide on purpose -- the
    // claim is "ink appeared here", not "ink appeared exactly here".
    let rect = (
        X - 2.0,
        BASE - SIGNATURE_LABEL_FONT_SIZE,
        X + 220.0,
        BASE + SIGNATURE_LABEL_FONT_SIZE,
    );
    let ink_before = dark_pixels_in(&rendered_without, SCALE, rect);
    let ink_after = dark_pixels_in(&rendered_with, SCALE, rect);

    eprintln!(
        "label band {rect:?} at scale {SCALE}: {ink_before} dark pixels before, {ink_after} after \
         (label {} chars at {}pt)",
        VISUAL_SIGNATURE_LABEL.chars().count(),
        SIGNATURE_LABEL_FONT_SIZE
    );

    assert_eq!(
        ink_before, 0,
        "the blank page must render nothing in the label's band, or this test cannot tell the \
         label from the background and would pass on a page whose label drew nothing"
    );
    assert!(
        ink_after > 40,
        "the label is in the saved file's content stream but a viewer draws no ink for it \
         ({ink_after} dark pixels in the band). The sentence is {} characters at {}pt, so a \
         rendering that draws it cannot produce a handful of pixels -- the likely causes are an \
         unregistered font, a position outside the MediaBox, or a fill matching the background",
        VISUAL_SIGNATURE_LABEL.chars().count(),
        SIGNATURE_LABEL_FONT_SIZE
    );

    let _ = std::fs::remove_file(&with_path);
    let _ = std::fs::remove_file(&without_path);
}

/// The label is not merely visible somewhere -- it is visible *where it was placed*.
///
/// The previous test would also pass if the writer ignored `(x, y)` and always drew at the page
/// origin, because it only looks at one band. This one writes the label to a second, distant
/// position and requires the ink to *move*: the old band must stay empty and the new one must fill.
/// That is what makes the placement parameters load-bearing rather than decorative.
#[test]
#[ignore = "requires a real PDFium library; `cargo test -- --include-ignored`"]
fn the_label_lands_where_it_was_placed_not_at_a_fixed_origin() {
    const X: f32 = 72.0;
    const LOW: f32 = 100.0;
    const HIGH: f32 = 700.0;
    const SCALE: f32 = 4.0;

    let mut document = blank_page_pdf();
    signature_label(&mut document, 0, X, HIGH, SIGNATURE_LABEL_FONT_SIZE).expect("label writes");
    let path = write_temp(&mut document, "high");
    let rendered = PdfProvider::render_page_at(&path, 0, SCALE).expect("renders");

    let band_at = |base: f32| {
        (
            X - 2.0,
            base - SIGNATURE_LABEL_FONT_SIZE,
            X + 220.0,
            base + SIGNATURE_LABEL_FONT_SIZE,
        )
    };

    let at_low = dark_pixels_in(&rendered, SCALE, band_at(LOW));
    let at_high = dark_pixels_in(&rendered, SCALE, band_at(HIGH));

    eprintln!(
        "label written at y={HIGH}: ink at y={LOW} band = {at_low}, at y={HIGH} band = {at_high}"
    );

    assert_eq!(
        at_low, 0,
        "nothing was drawn at the unoccupied position, so the writer ignored its y argument"
    );
    assert!(
        at_high > 40,
        "the label was written at y={HIGH} and no ink appeared there; the placement argument is \
         not reaching the page"
    );

    let _ = std::fs::remove_file(&path);
}
