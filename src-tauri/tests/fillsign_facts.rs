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
