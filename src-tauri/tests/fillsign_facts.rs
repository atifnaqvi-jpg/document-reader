//! Facts-first measurement for P3.3a (Fill & Sign), taken against the real LEA form.
//!
//! `docs/P3.3a-fill-and-sign-DESIGN.md` §3.2 chose "snap to a run's box" over "expose a
//! page-space baseline on the wire" on the strength of one claim: for a line-sized run the box's
//! bottom edge is within a rounding error of the page-space baseline. That claim was a reading,
//! not a measurement. These tests measure it on the fixture the acceptance row uses.
//!
//! The fixture is `lea-aufenthaltstitel.pdf` (Q5): **no `/AcroForm`**, its lines are **drawn
//! rules**, and it carries 4 pages of Excel 2016 table. That combination is what makes it useful
//! here -- nothing in it can be discovered by reading a widget rectangle, so anything these tests
//! report about placement came from measuring text and graphics.
//!
//! Every test is `#[ignore]`d because they all need a real PDFium library, following the pixel
//! test's precedent in `editor/forms.rs`: the Linux gate has no PDFium, so a plain `cargo test`
//! must stay green without one, and `--include-ignored` runs them.
//!
//! The fixture lives outside the repo (`/home/ubuntu/work/`, the user's own path) and is not
//! committed: it is a real identity document and does not belong in a git history. Tests locate
//! it through `LEA_FIXTURE` so a checkout without it skips rather than fails, and the skip prints
//! itself -- rule 5 forbids a silent skip.

use std::path::{Path, PathBuf};

use document_reader_lib::editor::annotate::apply_annotations;
use document_reader_lib::model::{Annotation, AnnotationKind};
use document_reader_lib::provider::{DocumentHandles, PdfProvider, RasterizedPage};
use image::GenericImageView;

/// The environment variable naming the fixture. Existence is checked before any assertion so a
/// machine without the document reports "skipped" out loud rather than passing by accident.
const FIXTURE_ENV: &str = "LEA_FIXTURE";

fn fixture() -> Option<PathBuf> {
    let raw = match std::env::var(FIXTURE_ENV) {
        Ok(value) if !value.trim().is_empty() => value,
        _ => {
            eprintln!(
                "SKIPPED: {FIXTURE_ENV} is not set, so the LEA form is not available. \
                 Set it to the fixture path to run the Fill & Sign facts measurement."
            );
            return None;
        }
    };
    let path = PathBuf::from(raw);
    if !path.is_file() {
        eprintln!(
            "SKIPPED: {FIXTURE_ENV} points at {}, which is not a file.",
            path.display()
        );
        return None;
    }
    Some(path)
}

/// Non-linearised ink count of a rendered page: pixels darker than mid-grey.
///
/// `RasterizedPage::bytes` is a **PNG**, not a pixel buffer — the provider encodes before
/// returning, so the buffer stays valid after PDFium's bitmap is dropped. Decoding is therefore
/// mandatory: counting `chunks_exact(4)` over PNG bytes counts compressed data, which is why this
/// helper first reported 39,932 "dark pixels" on a page and would have reported a number that
/// meant nothing on any page. Same decode as `editor/forms.rs`'s pixel test.
///
/// The threshold is the one that test uses. `dark` rather than `non-white` because a
/// white-background table has a huge number of near-white pixels and a rule drawn in light grey
/// would be invisible to a "not exactly white" test while being the thing under measurement.
fn dark_pixels(page: &RasterizedPage) -> usize {
    let image = image::load_from_memory(&page.bytes)
        .expect("render_page_at returns an encoded image")
        .to_luma8();
    image.pixels().filter(|pixel| pixel.0[0] < 128).count()
}

/// The document really is the fixture Q5 described, measured rather than trusted.
///
/// This is the test that stops the other four from silently measuring the wrong file: a
/// form-bearing PDF would make the snapping tests pass for a reason that does not exist in
/// production, because a widget rect would be available to lean on.
#[test]
#[ignore = "requires a real PDFium library and the LEA fixture; `cargo test -- --include-ignored`"]
fn the_lea_fixture_is_the_formless_baseline_the_design_names() {
    let Some(path) = fixture() else { return };

    let bytes = std::fs::read(&path).expect("the fixture is readable");
    assert!(
        !bytes.windows(9).any(|w| w == b"/AcroForm"),
        "the LEA fixture must carry no /AcroForm, or this phase's snapping tests could pass by \
         reading a widget rectangle instead of measuring geometry"
    );

    let pages = PdfProvider::page_count(&path).expect("page count");
    assert_eq!(pages, 4, "the fixture is a 4-page table");
}

/// §3.2's chosen mechanism, measured — and the measurement contradicts the design's claim.
///
/// A signature is placed on a *page-space* baseline. The design snaps to the run's box bottom on
/// the claim that "for a line-sized run the box's bottom edge is within a rounding error of the
/// baseline". **Measured on the LEA form, it is not.** The loose box is the *advance* box, which
/// extends to the font's descender: at 12pt type the bottom sits ~2.5pt below the pen origin, and
/// that is a fifth of the type size, not a rounding error. Snapping to the box bottom would put a
/// signature a quarter of a line low on every run.
///
/// The relationship is still a constant, which is what makes snapping viable — it is just a
/// descender offset rather than zero. This test pins the corrected property so the implementation
/// cannot silently adopt the wrong one:
///
///   * the box bottom is **at or below** the origin for upright text (a positive descender),
///   * that depth is **bounded** by the type size, so no run can report an absurd offset, and
///   * the depth is **consistent** across the page's runs at the same size, which is what allows
///     one correction constant to serve the whole table rather than per-glyph tuning.
///
/// Reported values are printed so the design's correction is a recorded number, not a guess.
#[test]
#[ignore = "requires a real PDFium library and the LEA fixture; `cargo test -- --include-ignored`"]
fn a_run_box_bottom_sits_a_descender_below_the_baseline_not_on_it() {
    let Some(path) = fixture() else { return };

    let handles = DocumentHandles::new();
    let grid = PdfProvider::char_grid(&handles, &path, None, 0).expect("char grid on page 0");

    let positioned: Vec<_> = grid
        .chars
        .iter()
        .filter(|c| c.unicode.is_some() && !c.generated && c.font_size > 0.0)
        .collect();
    assert!(
        positioned.len() > 40,
        "the LEA page should carry a table's worth of text; got {} characters",
        positioned.len()
    );

    let font_size = positioned
        .iter()
        .map(|c| c.font_size)
        .fold(0.0_f32, f32::max);
    assert!(font_size > 0.0, "PDFium reported no usable font size");

    // `origin_y - y0` is the descender depth: positive when the box bottom is below the pen
    // origin, which is the normal case for a face with descenders.
    let depths: Vec<f32> = positioned.iter().map(|c| c.origin_y - c.y0).collect();

    let max_depth = depths.iter().fold(f32::MIN, |a, b| a.max(*b));
    let min_depth = depths.iter().fold(f32::MAX, |a, b| a.min(*b));
    let mean = depths.iter().sum::<f32>() / depths.len() as f32;

    eprintln!(
        "LEA page 0, {} positioned characters, type size {:.2}: \
         origin minus box-bottom = min {:.3}, mean {:.3}, max {:.3} pt",
        positioned.len(),
        font_size,
        min_depth,
        mean,
        max_depth
    );

    assert!(
        min_depth > -0.5,
        "a run box's bottom edge must not sit meaningfully ABOVE the baseline for upright text; \
         measured min {min_depth:.3}pt means some run's box excludes the baseline entirely and \
         snapping to it would be wrong in the other direction"
    );
    assert!(
        max_depth < font_size,
        "the box-to-baseline depth must stay inside one em, or the box is not a line box at all; \
         measured max {max_depth:.3}pt against type size {font_size:.2}"
    );
}

/// The descender depth is one constant per page, which is what a snap correction needs.
///
/// The previous test shows the box bottom is not the baseline. This one shows the offset is
/// *stable*, so the implementation can subtract a single measured depth instead of measuring per
/// glyph: a spread wider than a couple of points would make any constant wrong for some runs.
#[test]
#[ignore = "requires a real PDFium library and the LEA fixture; `cargo test -- --include-ignored`"]
fn the_box_to_baseline_depth_is_consistent_across_a_page() {
    let Some(path) = fixture() else { return };

    let handles = DocumentHandles::new();
    let grid = PdfProvider::char_grid(&handles, &path, None, 0).expect("char grid on page 0");

    // Only characters that share the dominant type size: a table mixing 9pt and 12pt would
    // legitimately report two depths, and mixing them would test the mixture rather than the
    // consistency of either.
    let mut sizes: Vec<f32> = grid
        .chars
        .iter()
        .filter(|c| c.unicode.is_some() && !c.generated && c.font_size > 0.0)
        .map(|c| (c.font_size * 100.0).round() / 100.0)
        .collect();
    sizes.sort_by(|a, b| a.partial_cmp(b).expect("finite"));
    let dominant = sizes[sizes.len() / 2];

    let depths: Vec<f32> = grid
        .chars
        .iter()
        .filter(|c| {
            c.unicode.is_some()
                && !c.generated
                && ((c.font_size * 100.0).round() / 100.0 - dominant).abs() < 0.01
        })
        .map(|c| c.origin_y - c.y0)
        .collect();
    assert!(
        depths.len() > 20,
        "need a sample at the dominant size {dominant} to judge consistency; got {}",
        depths.len()
    );

    let min = depths.iter().fold(f32::MAX, |a, b| a.min(*b));
    let max = depths.iter().fold(f32::MIN, |a, b| a.max(*b));
    eprintln!(
        "at type size {dominant}, {} characters: depth min {min:.3} max {max:.3} spread {:.3}pt",
        depths.len(),
        max - min
    );

    // Half a point, not two. Measured spread is 0.063pt over 1,658 characters, so a 2pt bound
    // would have accepted a twenty-fold regression without a word -- and mutation showed it: at
    // `<= 0.0` the test dies on the real 0.063pt spread (so the assertion is live), but at `2.0`
    // it would also have accepted an 0.5pt-per-glyph drift, which is visible at 6.96pt type.
    // 0.5pt is roughly 7% of that type size: the point at which a "constant" correction has
    // stopped being constant.
    assert!(
        (max - min) <= 0.5,
        "the box-to-baseline depth must be near-constant at one type size for a single snap \
         correction to be correct; measured spread {:.3}pt (min {min:.3}, max {max:.3}) at size \
         {dominant}. The measured spread on this fixture is 0.063pt, so anything approaching this \
         bound is a regression, not noise",
        max - min
    );
}

/// The LEA pages carry drawn rules, and this is how much ink they are, in pixels.
///
/// §3.2 and the task order both call for "detection of typable areas over drawn rules". Detection
/// has to be built on something measurable, so this records the measured ink of a page as the
/// baseline any rule-detector must explain. It deliberately asserts a range rather than an exact
/// count: the claim is "this page has graphics", not "it has exactly N dark pixels", because the
/// latter would be a brittleness that fails on a library update while proving nothing.
#[test]
#[ignore = "requires a real PDFium library and the LEA fixture; `cargo test -- --include-ignored`"]
fn the_lea_pages_draw_rules_that_are_measurable_through_the_provider() {
    let Some(path) = fixture() else { return };

    for page in 0..4u32 {
        let rendered = PdfProvider::render_page_at(&path, page, 2.0).expect("render");
        let dark = dark_pixels(&rendered);
        eprintln!(
            "LEA page {page}: {}x{} at 2x, {dark} dark pixels",
            rendered.width, rendered.height
        );
        // Measured 77,195 / 118,011 / 98,552 / 127,375 dark pixels on pages 0-3 at 2x. A floor of
        // 500 (the first draft) would have accepted a page rendering to almost nothing and still
        // called it "the table's rules"; the floor is set below the smallest measured page but far
        // enough above zero that a blank or near-blank render fails. 20,000 is ~26% of the
        // lightest page, so a render that dropped most of its content dies here.
        assert!(
            dark > 20_000,
            "page {page} of the LEA form should draw a table's rules and text; {dark} dark pixels \
             at 2x is far below the ~77k-127k this page measured, so the render has lost most of \
             its content and any geometry measured from it is suspect"
        );
    }
}

/// Every page of the fixture renders without panicking and reports a non-empty extent.
///
/// Cheap, but it is the precondition the acceptance row assumes: a signature placed on page N
/// requires page N to be renderable through the same path the viewer uses.
#[test]
#[ignore = "requires a real PDFium library and the LEA fixture; `cargo test -- --include-ignored`"]
fn every_lea_page_renders_through_the_viewer_path() {
    let Some(path) = fixture() else { return };

    for page in 0..4u32 {
        let rendered = PdfProvider::render_page_at(&path, page, 1.0)
            .unwrap_or_else(|e| panic!("page {page} must render through the provider: {e}"));
        assert!(
            rendered.width > 0 && rendered.height > 0,
            "page {page} rendered to a zero-area image"
        );
        // The reported extent must match the decoded image: `bytes` is a PNG, so the check is
        // that it decodes to exactly the width and height claimed alongside it. A raw-length
        // comparison here would be asserting the encoding format, not the page.
        let image = image::load_from_memory(&rendered.bytes)
            .unwrap_or_else(|e| panic!("page {page}'s render must decode: {e}"));
        assert_eq!(
            image.dimensions(),
            (rendered.width, rendered.height),
            "page {page}'s decoded image must match its reported extent"
        );
    }
}

/// The fixture path is outside the repository on purpose; this pins that it is not committed.
///
/// A real identity document in a git history would be a data-protection problem that no later
/// commit can undo, so the rule is enforced rather than remembered.
#[test]
fn the_lea_fixture_is_not_committed_to_the_repository() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crate is a child of the repo root");
    let mut found = Vec::new();
    collect_named(root, &mut found, 0);
    assert!(
        found.is_empty(),
        "the LEA fixture must not live in the repository; found {found:?} under {}. It is a real \
         identity document and belongs outside the tree, referenced through {FIXTURE_ENV}.",
        root.display()
    );
}

fn collect_named(dir: &Path, found: &mut Vec<PathBuf>, depth: usize) {
    if depth > 4 || found.len() > 4 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') || name == "node_modules" || name == "target" {
            continue;
        }
        if path.is_dir() {
            collect_named(&path, found, depth + 1);
        } else if name.to_ascii_lowercase().contains("lea-aufenthaltstitel") {
            found.push(path);
        }
    }
}

/// §6 row: **a signature placed on the LEA form renders on the line it was placed on.**
///
/// The other tests in this file measure the form. This one places a signature on it and looks at
/// the ink, which is the only check that can catch a placement that is arithmetically right and
/// visually wrong — the failure the design's own §3.1 warns about, where a rise moves the mark and
/// the code still reports success.
///
/// ## How "on the line" is decided without a human eye
///
/// A run's box bottom is a descender below its baseline; D-N1 places the mark on the baseline by
/// subtracting that depth. So the check is differential rather than absolute: render the page
/// without the signature, render it with one, and locate the ink that appeared. That ink must be
/// **narrower in y than an em** (it is sitting on one line, not spanning several) and its bottom
/// must land within a line of the run's own box bottom, where "a line" is that run's font size.
///
/// A mark placed a line too low, or at the page origin, fails the first bound; one placed in the
/// middle of a paragraph fails the second. Printing the measured band keeps the number reviewable
/// rather than only asserted.
#[test]
#[ignore = "requires a real PDFium library and the LEA fixture; `cargo test -- --include-ignored`"]
fn the_lea_form_accepts_a_signature_on_its_own_line() {
    let Some(fixture_path) = fixture() else {
        return;
    };

    // A run on page 0 to place the signature against, and its font size, measured.
    let handles = DocumentHandles::new();
    let grid = PdfProvider::char_grid(&handles, &fixture_path, None, 0).expect("char grid");
    // The widest run of real glyphs at the page's largest size, so the stroke spans a word rather
    // than one character. A single character is a few points wide, and the writer refuses a box
    // too small to draw in -- which is a real refusal, not something to work around.
    let size = grid
        .chars
        .iter()
        .filter(|c| c.unicode.is_some() && !c.generated && c.font_size > 0.0)
        .map(|c| c.font_size)
        .fold(0.0_f32, f32::max);
    let run = grid
        .chars
        .iter()
        .filter(|c| {
            c.unicode.is_some()
                && !c.generated
                && (c.font_size - size).abs() < 0.01
                && c.unicode.is_some_and(|ch| !ch.is_whitespace())
        })
        .max_by(|a, b| {
            (a.x1 - a.x0)
                .partial_cmp(&(b.x1 - b.x0))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .expect("the LEA page carries text");
    // The baseline, page space: the origin minus the box's descender depth, which is the same
    // correction `state/fillsign.ts` applies as `DESCENT_EM`.
    let baseline = run.origin_y - (run.origin_y - run.y0).clamp(0.0, size);
    // A drawn signature has height as well as width: the stroke rises and falls across the word.
    // A flat line would satisfy the writer but not resemble a mark, and its band would be a
    // one-pixel row that says nothing about whether it landed on the line.
    let x0 = run.x0;
    let x1 = run.x1;
    let rise = size * 0.7;
    let baseline_rise = baseline + rise;
    eprintln!(
        "LEA placement target: {:?} at size {size}, box y0={}, origin_y={}, baseline={baseline}",
        run.unicode.map(|c| c.to_string()),
        run.y0,
        run.origin_y
    );

    // Sign it through the shipped save path: an Ink row flagged as a visual signature, its stroke
    // drawn on the resolved baseline across the run's own width.
    let row = Annotation {
        id: "lea-sig".to_string(),
        document_id: "lea".to_string(),
        page: 0,
        kind: AnnotationKind::Ink,
        data: format!(
            r##"{{"v":1,"visualSignature":true,"color":"#000000","strokes":[[[{x0},{baseline}],[{x0},{baseline_rise}],[{x1},{baseline_rise}],[{x1},{baseline}]]]}}"##
        ),
        text: None,
        created_at: 0,
        modified_at: 0,
    };
    let source = std::fs::read(&fixture_path).expect("the fixture is readable");
    let signed = apply_annotations(&source, &[row]).expect("the signature saves");

    // Write beside the fixture, so the real document is never modified.
    let dir = tempfile::tempdir().expect("temp dir");
    let signed_path = dir.path().join("lea-signed.pdf");
    std::fs::write(&signed_path, &signed).expect("the signed copy is written");

    // Differential ink: the dark pixels the overlay added, and where they are.
    let before = PdfProvider::render_page_at(&fixture_path, 0, 2.0).expect("render original");
    let after = PdfProvider::render_page_at(&signed_path, 0, 2.0).expect("render signed");
    assert_eq!(
        (before.width, before.height),
        (after.width, after.height),
        "the overlay must not change the page extent, or the two renders cannot be compared"
    );

    let before_image = image::load_from_memory(&before.bytes)
        .expect("decodes")
        .to_luma8();
    let after_image = image::load_from_memory(&after.bytes)
        .expect("decodes")
        .to_luma8();
    assert_eq!(before_image.dimensions(), after_image.dimensions());

    // Every pixel that became dark is ink the overlay added. Reported as a bounding band in
    // *page* units, dividing the pixel rows by the scale, so the numbers compare to the run.
    let scale = 2.0_f32;
    let mut min_row = u32::MAX;
    let mut max_row = 0u32;
    let mut added = 0usize;
    for (x, y, pixel) in after_image.enumerate_pixels() {
        let was = before_image.get_pixel(x, y).0[0];
        if was >= 128 && pixel.0[0] < 128 {
            added += 1;
            min_row = min_row.min(y);
            max_row = max_row.max(y);
        }
    }
    assert!(
        added > 20,
        "the signature must actually draw something: {added} pixels changed. A placement that          saved but drew nothing would otherwise pass this row"
    );

    // Pixel rows run top-down; PDF y runs bottom-up. Convert the band's extent, not its position,
    // because the extent is what says "one line" or "half a paragraph".
    let band_height = (max_row - min_row + 1) as f32 / scale;
    let page_height = after.height as f32 / scale;
    let band_bottom = page_height - (max_row as f32 / scale);
    eprintln!(
        "LEA overlay ink: {added} px, band {band_height:.1}pt tall, bottom at y={band_bottom:.1} \
         (target baseline {baseline:.1}, size {size:.1})"
    );

    assert!(
        band_height <= size * 1.5,
        "the mark must sit on one line, not span several: the added ink is {band_height:.1}pt \
         tall and the line is {size:.1}pt"
    );
    assert!(
        (band_bottom - baseline).abs() <= size,
        "the mark must land on the line it was placed on: its bottom is at y={band_bottom:.1}, \
         the target baseline is {baseline:.1}, and the line is {size:.1}pt tall"
    );
}

/// §6 row: **a saved signature ticks/renders in a real viewer, not only in our own raster.**
///
/// "Our raster" is PDFium through `PdfProvider`. The claim this row makes is that a *different*
/// reader sees the mark too, which no amount of re-rendering through the same library can
/// establish. What can be established here, without shipping a second PDF engine, is that the
/// artifact carries the things every reader keys on independently of us:
///
///   * an `/Annot` with a `/Subtype` of `Ink` — a reader that walks annotations finds it;
///   * a `/Rect` that is non-degenerate and inside the page — a reader that clips to the page
///     keeps it;
///   * an `/AP` appearance stream that actually draws — a reader that never runs our code still
///     paints something, because the appearance is in the file.
///
/// The honest limit is stated rather than papered over: this does not prove Adobe Reader paints
/// it. It proves the file carries an appearance rather than only a `/Contents` our own renderer
/// happens to interpret, which is the failure this row exists to exclude. The remaining check is
/// the hand test on a real viewer, which the phase gate already requires.
#[test]
#[ignore = "requires a real PDFium library and the LEA fixture; `cargo test -- --include-ignored`"]
fn a_saved_signature_is_visible_in_a_real_viewer() {
    let Some(fixture_path) = fixture() else {
        return;
    };

    let row = Annotation {
        id: "lea-visible".to_string(),
        document_id: "lea".to_string(),
        page: 0,
        kind: AnnotationKind::Ink,
        data: r##"{"v":1,"visualSignature":true,"color":"#000000","strokes":[[[100,400],[160,440],[220,400]]]}"##
            .to_string(),
        text: None,
        created_at: 0,
        modified_at: 0,
    };
    let source = std::fs::read(&fixture_path).expect("the fixture is readable");
    let signed = apply_annotations(&source, &[row]).expect("the signature saves");

    // Read the file the way another application would: parse it, walk the page's annotations.
    let document = lopdf::Document::load_mem(&signed).expect("the saved file parses");
    let page_id = *document.get_pages().values().next().expect("one page");
    let annotations = document
        .get_page_annotations(page_id)
        .expect("the page's annotations are readable");
    assert_eq!(
        annotations.len(),
        1,
        "exactly the placed signature must be on the page"
    );

    let annotation = &annotations[0];
    assert_eq!(
        annotation.get(b"Subtype").and_then(|s| s.as_name()).ok(),
        Some(b"Ink".as_ref()),
        "a reader keys on /Subtype; it must be Ink"
    );

    // `/Rect` must be real and inside the page, or a reader clips the mark away entirely.
    let rect = annotation
        .get(b"Rect")
        .and_then(|value| value.as_array())
        .expect("/Rect is an array");
    let numbers: Vec<f32> = rect
        .iter()
        .map(|value| value.as_float().unwrap_or(0.0))
        .collect();
    assert_eq!(numbers.len(), 4, "/Rect is four numbers");
    assert!(
        numbers[2] > numbers[0] && numbers[3] > numbers[1],
        "the rectangle must be non-degenerate, got {numbers:?}; a zero-area rect is clipped away"
    );

    // `/AP` with a stream: the appearance another reader paints. Without this the mark exists
    // only in a form our own renderer understands.
    let appearance = annotation
        .get(b"AP")
        .and_then(|value| value.as_dict())
        .expect("the annotation must carry /AP, or a viewer that does not run our code paints                  nothing");
    let normal = appearance
        .get(b"N")
        .expect("/AP must name a normal appearance");
    let stream_id = normal.as_reference().expect("/AP /N is a stream reference");
    let stream = document
        .get_object(stream_id)
        .and_then(|object| object.as_stream())
        .expect("/AP /N resolves to a stream");
    // "Non-empty" is not enough: a stream holding only `q`/`Q` is non-empty and paints nothing.
    // The claim is that the appearance *paints a stroked path*, so the operators that do that are
    // named. `m`/`l` build the path, `S` strokes it -- without the stroke the path is constructed
    // and never drawn, which renders exactly as blank as an empty stream.
    let content = String::from_utf8_lossy(&stream.content).into_owned();
    for operator in ["m", "l", "S"] {
        assert!(
            content.split_whitespace().any(|token| token == operator),
            "the appearance must paint the stroke: no `{operator}` operator in {content:?}. A \
             stream with only `q`/`Q` is present and paints nothing, which is the failure this \
             row exists to exclude"
        );
    }
    assert!(
        stream.dict.get(b"BBox").is_ok(),
        "/AP must declare a /BBox, or a reader has no box to paint the appearance into"
    );
}
