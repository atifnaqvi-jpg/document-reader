//! A-11 free-text acceptance: the text the user typed must be **visible on the page and
//! inside its box**, measured on rendered pixels.
//!
//! Reported against 1.1.0: "text written in the note box is misaligned so that it gets
//! hidden". The comment pane and the appearance stream had been checked; the pixels had not.
//! Rendering a real saved file through PDFium showed the box empty for a short box, and text
//! starting above where the user placed it once typed line breaks were collapsed. Both were
//! invisible to a stream-shape assertion, because both were *legal* streams.
//!
//! These assertions are on ink: dark pixels inside the annotation's own rect. A test that
//! only read the stream would pass on a stream that draws nothing.
//!
//! PDFium tests carry `#[ignore]` rather than returning early -- a test that returns early is
//! a green pass wearing a label (`tests/ci_workflow.rs` refuses that shape). Run them with
//! `cargo test -- --include-ignored` and a real library, which is what phase close requires.

use std::path::PathBuf;

use document_reader_lib::editor::annotate::apply_annotations;
use document_reader_lib::model::{Annotation, AnnotationKind, FreeTextMode};
use document_reader_lib::provider::PdfProvider;

mod support;

/// A blank one-page PDF, so the only ink on the page is the annotation under test.
fn blank_page() -> Vec<u8> {
    use lopdf::content::{Content, Operation};
    use lopdf::dictionary;
    let mut document = lopdf::Document::with_version("1.4");
    let pages_id = document.new_object_id();
    let content = Content {
        operations: vec![Operation::new("q", vec![]), Operation::new("Q", vec![])],
    };
    let content_id = document.add_object(lopdf::Stream::new(
        lopdf::Dictionary::new(),
        content.encode().unwrap(),
    ));
    let page_id = document.add_object(lopdf::dictionary! {
        "Type" => "Page",
        "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        "Contents" => content_id,
        "Resources" => lopdf::dictionary! {},
    });
    document.objects.insert(
        pages_id,
        lopdf::Object::Dictionary(lopdf::dictionary! {
            "Type" => "Pages",
            "Kids" => vec![page_id.into()],
            "Count" => 1,
        }),
    );
    let catalog_id = document.add_object(lopdf::dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    document.trailer.set("Root", catalog_id);
    let mut bytes = Vec::new();
    document.save_to(&mut bytes).unwrap();
    bytes
}

fn free_text(rect: [f32; 4], text: &str) -> Annotation {
    free_text_mode(rect, text, FreeTextMode::Textbox)
}

/// The same row in a chosen mode. The typewriter draws no border, so tests that need the ink
/// to be *text and nothing else* use it: a bordered box puts its own frame on the page, and a
/// frame is enough dark pixels to satisfy "something was drawn" from the wrong cause.
fn free_text_mode(rect: [f32; 4], text: &str, mode: FreeTextMode) -> Annotation {
    Annotation {
        id: "ft-1".to_string(),
        document_id: "doc-1".to_string(),
        page: 0,
        kind: AnnotationKind::FreeText(mode),
        data: format!(
            r##"{{"v":1,"rect":[{},{},{},{}],"color":"#000000"}}"##,
            rect[0], rect[1], rect[2], rect[3]
        ),
        text: Some(text.to_string()),
        created_at: 0,
        modified_at: 0,
    }
}

/// The rendered page, plus the ink pixels inside `rect` (in PDF points).
///
/// The rect is converted to pixels at the render scale. Y is flipped because PDF space grows
/// upward and image space grows downward; without that flip an "inside" test would measure
/// the vertically mirrored box and pass on any symmetrical error.
fn ink_inside(
    bytes: &[u8],
    rect: [f32; 4],
    scale: f32,
    name: &str,
) -> (image::GrayImage, Vec<(u32, u32)>) {
    let dir = std::env::temp_dir().join(format!("docreader-a11-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    // One file per test. These run in parallel threads of one process, so a shared name means
    // one test renders another's bytes and the failures look like product bugs.
    let path: PathBuf = dir.join(format!("{name}.pdf"));
    std::fs::write(&path, bytes).unwrap();

    let raster = PdfProvider::render_page_at(&path, 0, scale).unwrap();
    let image = image::load_from_memory(&raster.bytes)
        .unwrap_or_else(|e| panic!("render did not decode as an image: {e}"))
        .to_luma8();

    let page_height_points = 792.0_f32;
    let x0 = (rect[0] * scale).floor().max(0.0) as u32;
    let x1 = (rect[2] * scale).ceil() as u32;
    // PDF y grows up: the top edge of the box is rect[3], measured from the page bottom.
    let y0 = ((page_height_points - rect[3]) * scale).floor().max(0.0) as u32;
    let y1 = ((page_height_points - rect[1]) * scale).ceil() as u32;

    let mut ink = Vec::new();
    for y in y0..y1.min(image.height()) {
        for x in x0..x1.min(image.width()) {
            if image.get_pixel(x, y)[0] < 128 {
                ink.push((x, y));
            }
        }
    }
    (image, ink)
}

#[test]
#[ignore = "requires a real PDFium library; `cargo test -- --include-ignored` after fetching it"]
fn a_short_free_text_box_still_shows_its_text() {
    // 10pt tall: shorter than the 12pt font. Before the fix the stream was `BT ... ET` with no
    // `Tj`, so the page rendered blank while the comment pane listed the annotation. The user
    // saw an annotation that existed nowhere they could look at it.
    let rect = [72.0, 600.0, 272.0, 610.0];
    let saved = apply_annotations(
        &blank_page(),
        &[free_text_mode(rect, "caption", FreeTextMode::Typewriter)],
    )
    .unwrap();
    let (_image, ink) = ink_inside(&saved, rect, 4.0, "short");
    assert!(
        !ink.is_empty(),
        "a 10pt free-text box rendered with no ink at all: the text is missing from the page"
    );
}

#[test]
#[ignore = "requires a real PDFium library; `cargo test -- --include-ignored` after fetching it"]
fn typed_line_breaks_start_the_text_where_the_user_put_it() {
    // Three lines typed with Enter, in a box tall enough for all three only if the breaks are
    // honoured. Before the fix the wrap re-flowed them into fewer, longer lines, so the block
    // began higher in the box and its tail was truncated away.
    let rect = [72.0, 540.0, 272.0, 660.0];
    let saved = apply_annotations(
        &blank_page(),
        &[free_text_mode(
            rect,
            "alpha\nbeta\ngamma",
            FreeTextMode::Typewriter,
        )],
    )
    .unwrap();
    let (image, ink) = ink_inside(&saved, rect, 4.0, "breaks");
    assert!(!ink.is_empty(), "no ink at all");

    // Three separate lines produce three distinct bands of ink. A collapsed, re-flowed block
    // fills fewer bands, so counting them distinguishes the two without measuring glyphs.
    let mut rows_with_ink: Vec<u32> = ink.iter().map(|(_, y)| *y).collect();
    rows_with_ink.sort_unstable();
    rows_with_ink.dedup();
    let mut bands = 0;
    let mut previous: Option<u32> = None;
    let mut gap = 0;
    for row in rows_with_ink {
        match previous {
            // A blank scanline run longer than a glyph's own height (12pt at scale 4 is 48px,
            // but a glyph's ink is far shorter) separates two lines.
            Some(last) if row != last + 1 => {
                gap = 0;
                bands += 1;
            }
            Some(_) => gap += 1,
            None => bands += 1,
        }
        previous = Some(row);
        let _ = gap;
    }
    assert!(
        bands >= 3,
        "expected 3 ink bands for 3 typed lines, found {bands}; the typed breaks were collapsed. \
         Rendered page was {}x{}",
        image.width(),
        image.height()
    );
}

#[test]
#[ignore = "requires a real PDFium library; `cargo test -- --include-ignored` after fetching it"]
fn the_text_stays_inside_its_own_box() {
    // The truncation rule exists so text never lands outside the annotation's `/Rect`, where
    // no hit-test or comment entry accounts for it. Measured here on pixels rather than on the
    // stream's own bookkeeping.
    let rect = [72.0, 500.0, 272.0, 620.0];
    let long = "This is a note that the user typed into the box. It is long enough to need \
                several lines, which is entirely normal for a free-text annotation.";
    let saved = apply_annotations(&blank_page(), &[free_text(rect, long)]).unwrap();
    let (image, inside) = ink_inside(&saved, rect, 4.0, "inside");
    assert!(!inside.is_empty(), "no ink inside the box");

    // Ink anywhere else on the page is overflow. The box is the only thing on this page.
    let mut total_dark = 0;
    for y in 0..image.height() {
        for x in 0..image.width() {
            if image.get_pixel(x, y)[0] < 128 {
                total_dark += 1;
            }
        }
    }
    assert_eq!(
        total_dark,
        inside.len(),
        "{} dark pixel(s) were drawn outside the annotation's box",
        total_dark - inside.len()
    );
}
