//! P2.7.7 §5, **row 6**: glyph-id preservation.
//!
//! Row 6 exists because a Type0 subset can be rebuilt wrongly and still produce a PDF that
//! parses, measures and draws. Under `/CIDToGIDMap /Identity` the CID *is* the glyph id, so a
//! writer that numbers the bundled face's glyphs in first-use order silently moves every
//! retained glyph. The edited run looks right — it was written with the new numbering — and
//! every run the edit never touched starts drawing somebody else's shapes. There is no parse
//! error and no refusal: the page simply shows different ink.
//!
//! Three assertions, each of which fails for a different reason, so that passing all three
//! means what the row claims and not merely that something rendered:
//!
//! * `6a` **rendered identity, region by region** — the pixels inside each untouched run's
//!   bounding box are identical before and after the edit, byte for byte, in luma.
//! * `6b` **glyph id, structurally** — the written program's `cmap` still resolves each
//!   retained character to the id it had. This is the one that cannot be bought by a writer
//!   that renumbers to equal-looking glyphs.
//! * `6c` **the edited run is not exempt** — it must render at the advance `/W` claims, so a
//!   row cannot be passed by leaving the edit broken or drawing it at the wrong width.
//!
//! **These tests fail today, and that is the point.** No Type0 writer exists, so the edit
//! that row 6 is about cannot be performed at all. The failures are asserted to name that
//! reason rather than a missing helper, which is why each begins by pinning down what the
//! fixture is and what the current refusal says. When `P2.7.7` lands, the refusal becomes a
//! successful edit and these turn green without being rewritten.
//!
//! `6b` is deliberately **not** `#[ignore]`d: it needs `lopdf` and `ttf-parser` only, both of
//! which are always present, so it must run everywhere. `6a` and `6c` need PDFium and carry
//! `#[ignore = "requires a real PDFium library"]` instead of returning early — a test that
//! returns early is a green pass wearing a label, which `tests/ci_workflow.rs` refuses.

use std::path::{Path, PathBuf};

use document_reader_lib::editor::cid::CidFont;
use document_reader_lib::editor::embed;
use document_reader_lib::editor::substitute;
use document_reader_lib::editor::textedit;
use document_reader_lib::editor::textlocate::{self, RawRun};
use document_reader_lib::model::AppError;
use document_reader_lib::provider::PdfProvider;

mod support;

// Liberation Sans glyph ids and advances, read from the committed program's own `cmap` and
// `hmtx`. Written out here rather than recomputed, because the point of the row is that these
// are the *document's* ids: if the fixture builder and the assertion both derived them, a
// builder that renumbered would take the assertion with it.
// The subset renumbers glyphs in sorted source order: `.notdef`=0, then A=1, B=2, C=3,
// a=4, b=5. These are the CIDs the content stream carries and, under /Identity, the glyph
// ids a reader must reach.
const GID_A: u16 = 1;
const GID_B: u16 = 2;
const GID_C: u16 = 3;
const GID_A_LOWER: u16 = 4;
const GID_B_LOWER: u16 = 5;

const FIXTURE: &str = "type0-shared-font-multi-run.pdf";

fn fixture_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(FIXTURE)
}

fn load() -> lopdf::Document {
    let path = fixture_path();
    lopdf::Document::load(&path)
        .unwrap_or_else(|e| panic!("could not load {}: {e}", path.display()))
}

/// The fixture's runs, top to bottom.
fn runs(document: &lopdf::Document) -> Vec<RawRun> {
    let page_id = textlocate::page_id(document, 0).unwrap();
    textlocate::locate_runs(document, page_id).unwrap()
}

/// The page rendered next to `bytes`, at a scale where one point is several pixels.
///
/// Scale 4.0 is chosen so that a glyph's own antialiasing spans more than one pixel: at
/// scale 1.0 a one-pixel shift can round to the same pixel and hide a real move.
fn render(bytes: &[u8], name: &str) -> image::GrayImage {
    let path = write_temp(name, bytes);
    let raster = PdfProvider::render_page_at(&path, 0, 4.0).unwrap();
    image::load_from_memory(&raster.bytes)
        .unwrap_or_else(|e| panic!("render did not decode as an image: {e}"))
        .to_luma8()
}

fn write_temp(name: &str, bytes: &[u8]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("docreader-p277-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, bytes).unwrap();
    path
}

fn saved(document: &lopdf::Document) -> Vec<u8> {
    let mut bytes = Vec::new();
    document.clone().save_to(&mut bytes).unwrap();
    bytes
}

/// The run's bounding box in pixels, from its origin and advance.
///
/// `RawRun::origin` and `RawRun::advance` are in PDF points with the origin at the
/// **bottom-left**; the raster has its origin at the top-left, and one point is `scale`
/// pixels. The height comes from the font size, not from the advance: a run occupies its
/// baseline's ascent and descent, and using the advance would give a box as tall as the text
/// is wide.
///
/// **The box is deliberately tight**, and each edge is meant to be argued with:
///
/// * `x` spans exactly `origin.0 .. origin.0 + advance`. The glyphs' own left and right side
///   bearings live *inside* this span, so the box contains the ink and nothing else.
/// * `y` spans the font size above and a quarter of it below the baseline. Liberation's
///   descender is ~0.21 em, so 0.25 em clears it without reaching a neighbouring line.
///
/// A box that was "large enough to contain the ink" would pass while covering half the page,
/// which turns the comparison into "is this page blank". The tightness test below measures
/// that directly.
fn run_rect(run: &RawRun, scale: f32, page_height: f32) -> (u32, u32, u32, u32) {
    let x0 = (run.origin.0 * scale).floor().max(0.0) as u32;
    let x1 = ((run.origin.0 + run.advance) * scale).ceil().max(0.0) as u32;
    // Bottom-left origin -> top-left raster.
    let y_bottom = (run.origin.1 - run.font_size * 0.25) * scale;
    let y_top = (run.origin.1 + run.font_size) * scale;
    let y0 = (page_height * scale - y_top).floor().max(0.0) as u32;
    let y1 = (page_height * scale - y_bottom).ceil().max(0.0) as u32;
    (x0, y0, x1, y1)
}

/// Ink inside `rect`: the count, and the bounding box of the darkened pixels.
///
/// `None` when no pixel is dark enough, which is what an empty region or a program that
/// draws nothing both look like.
fn ink(
    image: &image::GrayImage,
    rect: (u32, u32, u32, u32),
) -> Option<(usize, (u32, u32, u32, u32))> {
    let (x0, y0, x1, y1) = rect;
    let (width, height) = image.dimensions();
    let mut count = 0;
    let mut bbox = (u32::MAX, u32::MAX, 0, 0);
    for y in y0..y1.min(height) {
        for x in x0..x1.min(width) {
            if image.get_pixel(x, y)[0] < 128 {
                count += 1;
                bbox = (bbox.0.min(x), bbox.1.min(y), bbox.2.max(x), bbox.3.max(y));
            }
        }
    }
    (count > 0).then_some((count, bbox))
}

fn ink_pixels(image: &image::GrayImage, rect: (u32, u32, u32, u32)) -> usize {
    ink(image, rect).map_or(0, |(count, _)| count)
}

/// A Type0 replacement that reaches the refusal under test.
///
/// `Ω` is outside WinAnsi, so MECH-A's AFM table cannot cover it; Liberation Sans can, so the
/// coverage check passes; the run's font is `/Type0`, so `plan` refuses. Starting from a
/// WinAnsi character instead would take MECH-A and never reach the Type0 branch — a test that
/// "passed" without exercising the code it names.
const NON_WINANSI: &str = "\u{3a9}";

fn edit_middle_run(
    document: &mut lopdf::Document,
    replacement: &str,
) -> Result<textedit::EditOutcome, AppError> {
    let target = runs(document)
        .into_iter()
        .nth(1)
        .expect("fixture has a middle run");
    let measure = |_text: &str| Ok(50.0_f32);
    textedit::edit(
        document,
        &textedit::EditRequest {
            page: 0,
            run_id: target.id,
            expected: "AB".to_string(),
            replacement: replacement.to_string(),
            old_width: 100.0,
            font_size: 24.0,
            allow_substitution: true,
            fragments: Vec::new(),
            range: Vec::new(),
            measure: &measure,
        },
    )
}

/// The pixel comparison 6a performs, extracted so it can be tested on its own.
///
/// Extracting it is not tidiness. Row 6a cannot currently reach its own comparison: the edit
/// is refused before any rendering, so the test fails at the refusal and would "catch" any
/// mutant whatsoever — a catch that proves nothing. Pulling the comparison out lets a test
/// present it with two images that differ only inside the region and demand a failure, which
/// is the question mutation testing actually needs answered.
///
/// Returns the coordinates of the first differing pixel, or `None` when the regions agree.
fn first_difference(
    before: &image::GrayImage,
    after: &image::GrayImage,
    rect: (u32, u32, u32, u32),
) -> Option<(u32, u32)> {
    let (x0, y0, x1, y1) = rect;
    for y in y0..y1 {
        for x in x0..x1 {
            if before.get_pixel(x, y) != after.get_pixel(x, y) {
                return Some((x, y));
            }
        }
    }
    None
}

/// Whether a measured advance disagrees with the width the document claims, by more than a
/// point.
///
/// Extracted for the same reason as the other two: 6c cannot reach its comparison while the
/// edit is refused, so the tolerance itself must be shown to be strict enough to mean
/// something. A tolerance loose enough to accept anything is the failure mode that matters
/// here — it would let a writer declare any `/W` it liked.
fn advance_disagrees(measured: f32, claimed: f32) -> bool {
    (measured - claimed).abs() >= 1.0
}

/// The tolerance must accept a sub-point difference and reject a one-point one.
///
/// A point at 24pt is about 1.2% of an em, which is the smallest error a reader would notice
/// as a drifting word. Anything looser stops being a width check.
#[test]
fn the_width_tolerance_accepts_rounding_and_rejects_a_real_drift() {
    assert!(
        !advance_disagrees(32.016, 32.0),
        "a 0.016pt difference is glyph-rounding noise and must be accepted"
    );
    assert!(
        !advance_disagrees(32.5, 32.0),
        "half a point is within the tolerance"
    );
    assert!(
        advance_disagrees(33.0, 32.0),
        "a full point of drift must be rejected; a tolerance that accepted it would let a \
         writer state any /W at all and still pass row 6c"
    );
    assert!(
        advance_disagrees(32.0, 40.0),
        "a large disagreement in the other direction must be rejected too"
    );
}

/// The glyph-placement check 6b performs, extracted for the same reason as
/// [`first_difference`]: 6b cannot reach its own assertions while the edit is refused, so a
/// mutation run against it "catches" everything and proves nothing. Pulling the check out lets
/// a test hand it a program that has been renumbered and demand the exact character back.
///
/// Returns the first character that is not where the document's CIDs say it is, as
/// `(character, expected id, actual id if the program maps it at all)`.
/// The raw `glyf` bytes at glyph id `gid`, read through `loca`.
///
/// The `cmap` alone cannot answer "did this glyph move". A writer that puts the replacement's
/// outline into a *retained* CID's slot leaves the `cmap` nominally correct — it still resolves
/// the retained character to that id — while the outline underneath is a different glyph. Only
/// the glyph data shows that, so the placement assertion compares glyph data.
fn glyph_outline(program: &[u8], gid: u16) -> Option<Vec<u8>> {
    fn u16_at(data: &[u8], at: usize) -> Option<u16> {
        Some(u16::from_be_bytes([*data.get(at)?, *data.get(at + 1)?]))
    }
    fn u32_at(data: &[u8], at: usize) -> Option<u32> {
        Some(u32::from_be_bytes([
            *data.get(at)?,
            *data.get(at + 1)?,
            *data.get(at + 2)?,
            *data.get(at + 3)?,
        ]))
    }

    let count = u16_at(program, 4)? as usize;
    let mut tables = std::collections::BTreeMap::new();
    for index in 0..count {
        let base = 12 + index * 16;
        let tag: [u8; 4] = program.get(base..base + 4)?.try_into().ok()?;
        tables.insert(tag, (u32_at(program, base + 8)? as usize, base));
    }
    let loca_at = tables.get(b"loca")?.0;
    let glyf_at = tables.get(b"glyf")?.0;
    let glyf_len = u32_at(program, tables.get(b"glyf")?.1 + 12)? as usize;
    let head_at = tables.get(b"head")?.0;
    let long = u16_at(program, head_at + 50)? != 0;
    let at = usize::from(gid);
    let (start, end) = if long {
        (
            u32_at(program, loca_at + at * 4)? as usize,
            u32_at(program, loca_at + at * 4 + 4)? as usize,
        )
    } else {
        (
            u16_at(program, loca_at + at * 2)? as usize * 2,
            u16_at(program, loca_at + at * 2 + 2)? as usize * 2,
        )
    };
    if end <= start || end > glyf_len {
        return Some(Vec::new());
    }
    program
        .get(glyf_at + start..glyf_at + end)
        .map(<[u8]>::to_vec)
}

/// `(character, expected id, actual id if the program maps it at all)`.
fn first_misplaced_glyph(
    program: &[u8],
    expected: &[(char, u16)],
) -> Option<(char, u16, Option<u16>)> {
    let face = ttf_parser::Face::parse(program, 0)
        .expect("a program under this check parses as a TrueType font");
    expected.iter().find_map(|(ch, gid)| {
        let actual = face.glyph_index(*ch).map(|glyph| glyph.0);
        (actual != Some(*gid)).then_some((*ch, *gid, actual))
    })
}

/// The placement check must fail when a glyph has moved and pass when it has not.
///
/// Run against two real programs: the fixture's own subset, where the ids are 1..5, and the
/// whole bundled face, where `a` sits at 68 — which is precisely the id a first-use
/// renumbering produces, measured rather than guessed.
#[test]
fn the_placement_check_catches_a_moved_glyph_and_accepts_a_preserved_one() {
    let page_id_doc = load();
    let page_id = textlocate::page_id(&page_id_doc, 0).unwrap();
    let font = textlocate::font_dict(&page_id_doc, page_id, b"F1").unwrap();
    let descendants = font
        .get(b"DescendantFonts")
        .unwrap()
        .as_array()
        .unwrap()
        .clone();
    let descendant = page_id_doc
        .get_object(descendants[0].as_reference().unwrap())
        .unwrap()
        .as_dict()
        .unwrap()
        .clone();
    let descriptor = page_id_doc
        .get_object(
            descendant
                .get(b"FontDescriptor")
                .unwrap()
                .as_reference()
                .unwrap(),
        )
        .unwrap()
        .as_dict()
        .unwrap()
        .clone();
    let subset = page_id_doc
        .get_object(
            descriptor
                .get(b"FontFile2")
                .unwrap()
                .as_reference()
                .unwrap(),
        )
        .unwrap()
        .as_stream()
        .unwrap()
        .decompressed_content()
        .unwrap();

    let expected = [('C', GID_C), ('a', GID_A_LOWER), ('b', GID_B_LOWER)];

    // The preserved program passes: every character is at its document id.
    assert_eq!(
        first_misplaced_glyph(&subset, &expected),
        None,
        "the fixture's own subset places every retained character at its CID; if this fails the \
         fixture is wrong, not the writer"
    );

    // The renumbered program fails, and says which character moved and where to.
    let whole = std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("resources/fonts/LiberationSans-Regular.ttf"),
    )
    .unwrap();
    assert_eq!(
        first_misplaced_glyph(&whole, &expected),
        Some(('C', GID_C, Some(38))),
        "the whole face puts 'C' at glyph 38 while the document's CID says {GID_C}; the check \
         must report exactly that, or row 6b cannot tell a preserved program from a rebuilt one"
    );

    // And a program that drops a glyph entirely is a miss, not a pass by absence.
    assert_eq!(
        first_misplaced_glyph(&subset, &[('\u{3a9}', 9)]),
        Some(('\u{3a9}', 9, None)),
        "a character the program does not map must be reported, not skipped"
    );
}

/// The comparison must fail on a one-pixel change *inside* the region and pass on one
/// *outside* it.
///
/// This is the guard for 6a's mechanism, and it is what makes the mutant meaningful when the
/// writer lands: if this ever stops catching a localised change, 6a is decorative.
#[test]
fn the_region_comparison_catches_a_change_inside_and_ignores_one_outside() {
    let before = image::GrayImage::from_pixel(40, 40, image::Luma([255]));
    let rect = (10, 10, 20, 20);

    // A change inside the region must be found, and named precisely.
    let mut inside = before.clone();
    inside.put_pixel(15, 17, image::Luma([0]));
    assert_eq!(
        first_difference(&before, &inside, rect),
        Some((15, 17)),
        "a darkened pixel inside the region must be reported, or the comparison is blind"
    );

    // A change one pixel outside each edge must not be.
    for (x, y) in [(9, 15), (20, 15), (15, 9), (15, 20)] {
        let mut outside = before.clone();
        outside.put_pixel(x, y, image::Luma([0]));
        assert_eq!(
            first_difference(&before, &outside, rect),
            None,
            "a change at ({x}, {y}), one pixel outside {rect:?}, must not be reported: the \
             region's edges are the claim, and a comparison that leaked past them would be \
             asserting something wider than row 6 states"
        );
    }

    // The region must be half-open: (x1, y1) is the first *excluded* pixel, so (19, 19) --
    // the last one inside -- must still be compared.
    let blank = image::GrayImage::from_pixel(40, 40, image::Luma([255]));
    let mut last_inside = blank.clone();
    last_inside.put_pixel(19, 19, image::Luma([0]));
    assert!(
        first_difference(&blank, &last_inside, rect).is_some(),
        "the last pixel inside the region must be compared; an exclusive upper bound that \
         dropped it would leave a one-pixel seam where a moved glyph goes unseen"
    );
}

// --- The fixture itself -------------------------------------------------------

/// The fixture must be the shape row 6 needs: three runs, one shared `Type0` resource, an
/// honest `/Identity` map and a real program.
///
/// This runs before the others on purpose. If the fixture is wrong — two runs instead of
/// three, a cloned font resource instead of a shared one, a synthetic program with no glyphs —
/// then `6a` and `6b` would fail for a reason that says nothing about the writer, and a
/// reader would spend the time fixing the wrong file.
#[test]
fn the_row_6_fixture_is_three_runs_on_one_shared_type0_resource() {
    let document = load();
    let runs = runs(&document);
    assert_eq!(
        runs.len(),
        3,
        "row 6 needs three runs: one edit target, two untouched"
    );

    // One resource name, resolved through one font object. A fixture that gave each run its
    // own `/F1` would let a writer rebuild one program and leave the others alone.
    let names: Vec<&[u8]> = runs.iter().map(|run| run.font_key.as_slice()).collect();
    assert_eq!(
        names,
        vec![b"F1".as_slice(); 3],
        "all three runs draw with F1"
    );

    let page_id = textlocate::page_id(&document, 0).unwrap();
    let font = textlocate::font_dict(&document, page_id, b"F1").expect("F1 resolves");
    assert_eq!(
        font.get(b"Subtype").unwrap().as_name().unwrap(),
        b"Type0",
        "the fixture's font is a composite font, not a simple one"
    );
    assert_eq!(
        font.get(b"Encoding").unwrap().as_name().unwrap(),
        b"Identity-H",
        "two-byte identity encoding is what the phase is about"
    );

    // The descendant must carry a real program and an identity map, or `/Identity` is a
    // claim about numbers nothing draws.
    let descendants = font.get(b"DescendantFonts").unwrap().as_array().unwrap();
    let descendant = document
        .get_object(descendants[0].as_reference().unwrap())
        .unwrap()
        .as_dict()
        .unwrap();
    assert_eq!(
        descendant.get(b"CIDToGIDMap").unwrap().as_name().unwrap(),
        b"Identity",
        "row 6 is specifically about the identity map"
    );
    let descriptor = document
        .get_object(
            descendant
                .get(b"FontDescriptor")
                .unwrap()
                .as_reference()
                .unwrap(),
        )
        .unwrap()
        .as_dict()
        .unwrap();
    let program = document
        .get_object(
            descriptor
                .get(b"FontFile2")
                .unwrap()
                .as_reference()
                .unwrap(),
        )
        .unwrap()
        .as_stream()
        .unwrap();
    let program = program
        .decompressed_content()
        .expect("the program is a readable Flate stream");
    assert!(
        program.len() > 1_000,
        "the program is a real subset ({} bytes decompressed), not a synthetic stub; a stub \
         with an empty `glyf` draws no ink and makes every pixel comparison vacuous",
        program.len()
    );
    // And it must really be a subset: a program that maps every character would let the edit
    // keep the original font, so no writer would run and row 6 would assert nothing.
    let face = ttf_parser::Face::parse(&program, 0).expect("the program parses");
    assert!(
        face.glyph_index('\u{3a9}').is_none(),
        "the fixture's program must NOT map U+03A9. If it does, `substitute::original_covers` \
         succeeds, the edit keeps the original font, and no Type0 program is ever written."
    );
    assert!(
        face.glyph_index('A').is_some() && face.glyph_index('C').is_some(),
        "the subset must still map the characters the untouched runs draw"
    );

    // The CIDs really are the program's own glyph ids, which is what makes `/Identity` honest.
    let cid_font = CidFont::resolve(&document, font).expect("the fixture's font is readable");
    assert!(cid_font.is_readable(), "the reader understands the fixture");
    for (text, gid) in [("A", GID_A), ("B", GID_B), ("C", GID_C)] {
        assert_eq!(
            cid_font.encode(text),
            Ok(gid.to_be_bytes().to_vec()),
            "{text} must encode to its own glyph id ({gid}) as a two-byte CID: route 1 via \
             /ToUnicode where the map names it, route 2 via the program's cmap where it does not"
        );
    }
}

// --- 6a: rendered identity, region by region ----------------------------------

/// Row 6, assertion 1: every pixel inside each untouched run's box is unchanged.
#[test]
#[ignore = "requires a real PDFium library; `cargo test -- --include-ignored` after fetching it"]
fn row6a_untouched_runs_render_identically_before_and_after_the_edit() {
    let mut document = load();
    let before = render(&saved(&document), "row6a-before.pdf");

    match edit_middle_run(&mut document, NON_WINANSI) {
        Ok(_) => {}
        Err(e) => panic!(
            "row 6a cannot run: the edit that row 6 is about is refused before any rendering \
             happens ({e}). This is the expected initial failure — no Type0 writer exists. \
             When P2.7.7 lands the edit succeeds and this test proceeds to the comparison."
        ),
    }

    let after = render(&saved(&document), "row6a-after.pdf");
    assert_eq!(
        before.dimensions(),
        after.dimensions(),
        "the edit must not change the page's size"
    );

    let page_height = 792.0_f32;
    let scale = 4.0_f32;
    let runs = runs(&load());

    // The two runs the edit must not touch: the first and the last.
    for (index, expected_cids) in [(0_usize, vec![GID_C]), (2, vec![GID_A_LOWER, GID_B_LOWER])] {
        let run = &runs[index];
        let rect = run_rect(run, scale, page_height);

        // Tightness, measured rather than asserted by construction. Three things must hold,
        // and each rules out a different way of cheating:
        //
        // 1. the box contains ink -- otherwise the comparison is vacuously true;
        // 2. the ink it contains stays inside the box's own edges -- so the box is not
        //    clipping the run and silently excluding the pixels that would differ;
        // 3. the box is not far wider than the ink -- so it has not been inflated to swallow
        //    a neighbour or the whole page, which is how a "generous rect passes by covering
        //    the page" regression would hide.
        let (count, ink_bbox) = ink(&before, rect).unwrap_or_else(|| {
            panic!(
                "run {index} (CIDs {expected_cids:?}) draws no ink inside its own box {rect:?}. \
                 Either the box misses the run or the program draws nothing; both make the \
                 pixel comparison vacuous."
            )
        });
        let (x0, y0, x1, y1) = rect;
        eprintln!(
            "row6a run {index}: rect={rect:?} ({}x{} px), ink={count} px, ink_bbox={ink_bbox:?}",
            x1 - x0,
            y1 - y0
        );
        assert!(
            ink_bbox.0 >= x0 && ink_bbox.1 >= y0 && ink_bbox.2 < x1 && ink_bbox.3 < y1,
            "run {index}'s ink {ink_bbox:?} touches or crosses its box {rect:?}: the box must \
             contain the run's ink with room to spare, or the comparison below drops exactly \
             the pixels that moved"
        );
        let ink_width = ink_bbox.2 - ink_bbox.0 + 1;
        let box_width = x1 - x0;
        assert!(
            ink_width * 2 >= box_width,
            "run {index}'s box is {box_width} px wide but its ink is only {ink_width} px, so \
             the box is more than twice as wide as what it encloses. A box inflated past the \
             run stops being a region and starts covering its neighbours."
        );
        let box_area = box_width as u64 * (y1 - y0) as u64;
        let page_area = before.width() as u64 * before.height() as u64;
        assert!(
            box_area * 20 < page_area,
            "run {index}'s box is {box_area} px but the page is {page_area} px, so the box \
             covers more than 5% of the page. A box that grew to cover the page would make \
             this comparison assert 'the page still exists', not 'this run is unchanged'."
        );

        if let Some((x, y)) = first_difference(&before, &after, rect) {
            panic!(
                "run {index} was never edited, yet the pixel at ({x}, {y}) inside its box \
                 {rect:?} changed. Under /CIDToGIDMap /Identity this is what a renumbered \
                 subset looks like: the document's CIDs no longer name the same glyphs."
            );
        }
    }
}

// --- 6b: glyph id, structurally ------------------------------------------------

/// Row 6, assertion 2: the written program still resolves each retained character to the id
/// it had.
///
/// This runs everywhere and is never ignored. Rendering cannot tell "the id moved but the
/// glyph looks the same" from "the id never moved"; this can.
#[test]
fn row6b_the_written_program_resolves_each_retained_character_to_its_original_glyph_id() {
    let mut document = load();

    // Every CID the source document drew, captured *before* the edit.
    //
    // R5's invariant is stated over this set — "for every CID the previous document drew, the
    // new program must still draw the same glyph at the same id" — so it is read here rather
    // than restated as a list of the characters the assertions below happen to mention.
    let source_pairs: Vec<(u16, char)> = {
        let page_id = textlocate::page_id(&document, 0).unwrap();
        let font = textlocate::font_dict(&document, page_id, b"F1").expect("F1 resolves");
        CidFont::resolve(&document, font)
            .expect("the fixture's font is readable")
            .pairs()
            .expect("the fixture's font states a /ToUnicode")
            .to_vec()
    };
    assert!(
        source_pairs.len() >= 5,
        "the fixture draws five CIDs; got {source_pairs:?}"
    );

    match edit_middle_run(&mut document, NON_WINANSI) {
        Ok(_) => {}
        Err(e) => panic!(
            "row 6b cannot run: the edit is refused before a program is written ({e}). This is \
             the expected initial failure — no Type0 writer exists. When P2.7.7 lands the edit \
             succeeds and this test reads the program it wrote."
        ),
    }

    // Read the program the edit wrote, through the same font object the page now draws with.
    //
    // **This must be the edited run's own resource, not the original `F1`.** The writer leaves
    // `F1` in place — the runs the edit did not touch still name it — and installs a new
    // resource beside it. Reading `F1` therefore reads the *source* program, whose ids are
    // trivially unchanged, and every assertion below would pass while proving nothing. That
    // was a real defect in this test, caught by 6c failing on the same mistake.
    let page_id = textlocate::page_id(&document, 0).unwrap();
    let edited_key = runs(&document)[1].font_key.clone();
    let font = textlocate::font_dict(&document, page_id, &edited_key)
        .expect("the edited run's font resource resolves");
    let descendants = font
        .get(b"DescendantFonts")
        .unwrap()
        .as_array()
        .unwrap()
        .clone();
    let descendant_id = descendants[0].as_reference().unwrap();
    let descendant = document
        .get_object(descendant_id)
        .unwrap()
        .as_dict()
        .unwrap()
        .clone();
    let descriptor_id = descendant
        .get(b"FontDescriptor")
        .unwrap()
        .as_reference()
        .unwrap();
    let descriptor = document
        .get_object(descriptor_id)
        .unwrap()
        .as_dict()
        .unwrap()
        .clone();
    let program_id = descriptor
        .get(b"FontFile2")
        .unwrap()
        .as_reference()
        .unwrap();
    // `decompressed_content`, not `content`: the stream is Flate-compressed, and handing the
    // compressed bytes to a font parser yields `UnknownMagic` — a parser error that reads like
    // "the writer produced a broken font" when it produced a perfectly good one.
    let program = document
        .get_object(program_id)
        .unwrap()
        .as_stream()
        .unwrap()
        .decompressed_content()
        .expect("the written program is a readable Flate stream");

    // The assertion this row exists for. Under `/Identity` the CID *is* the glyph id, so the
    // program must still put each retained character at its original id.
    //
    // Deliberately read from the **program's own `cmap`** rather than through
    // `CidFont::encode`. `encode` tries route 1 (`/ToUnicode`) first and only falls back to the
    // program, so a writer that kept a correct `/ToUnicode` beside a renumbered program is
    // invisible to it — measured, not assumed: with the whole face substituted for the subset,
    // `encode('a')` still answered CID 4 while the program had moved `a` to glyph 68.
    if let Some((ch, gid, actual)) = first_misplaced_glyph(
        &program,
        &[('C', GID_C), ('a', GID_A_LOWER), ('b', GID_B_LOWER)],
    ) {
        panic!(
            "the written program's `cmap` puts {ch:?} at glyph {actual:?} instead of {gid}. The \
             document's CIDs name glyph ids, so a program that disagrees with them draws the \
             wrong shapes with no parse error — and every untouched run still showing that CID \
             changes appearance. This is R5's placement rule being violated: the bundled \
             glyphs were renumbered instead of placed at the document's ids."
        );
    }

    // The `cmap` check above is necessary and **not sufficient**, and mutation testing proved
    // it. A mutant that placed the *new* CID at the lowest source CID instead of above the
    // highest left both `B` and the replacement mapping to slot 2: the `cmap` still named `B` at
    // 2, so every check above passed, while the outline in slot 2 was the replacement's. Only
    // the glyph data shows that, so compare the glyph data — for every retained character.
    let source_program = {
        let page_id = textlocate::page_id(&document, 0).unwrap();
        // The edit leaves `F1` in place for the runs it did not rewrite, so the source program
        // is still in the file and still authoritative for what the source drew.
        let source_font = textlocate::font_dict(&document, page_id, b"F1")
            .expect("the untouched resource is still there")
            .clone();
        CidFont::resolve(&document, &source_font)
            .expect("the source font is still readable")
            .program()
            .expect("the source font still carries a program")
            .to_vec()
    };
    for (cid, ch) in &source_pairs {
        let want = glyph_outline(&source_program, *cid);
        let got = glyph_outline(&program, *cid);
        assert_eq!(
            want, got,
            "CID {cid} drew {ch:?} in the source document, and glyph slot {cid} of the written \
             program does not hold that glyph's outline any more. R5 is a *placement* rule: the \
             outline comes from the bundle and the id comes from the document. Putting the \
             replacement's outline into a retained CID's slot moves a glyph that every run this \
             edit did not touch still draws."
        );
    }

    // ---- R2: `/CIDToGIDMap /Identity`, and nothing else -------------------------------
    //
    // This assertion exists because mutation testing found its absence. A mutant that wrote
    // `/Identity-H` instead of `/Identity` — an invalid name, not a valid map — was SURVIVED by
    // this whole file: our own `/ToUnicode` names every CID we write, so route 1 carries the
    // edit and route 2 is never consulted. The rendering stays correct, PDFium stays lenient,
    // and the file is malformed. R2 says `/Identity` is what makes a CID a glyph id; if nothing
    // reads the map, nothing can be wrong about it.
    let map = descendant
        .get(b"CIDToGIDMap")
        .ok()
        .and_then(|o| o.as_name().ok());
    assert_eq!(
        map,
        Some(b"Identity".as_slice()),
        "the written /CIDToGIDMap must be /Identity (got {map:?}). R2 permits it only because \
         R5 placed each glyph at its own CID; any other value is either a malformed file or a \
         claim that the placement was different from what it was."
    );

    // ---- R3: the CIDFontType2's own `/CIDSystemInfo`, which is not the CMap's ------------
    //
    // `UCS` here would be a false claim — our CIDs are glyph indices, not code points — and it
    // is the plausible-looking error R3 names, because the identical triple is *correct* inside
    // the `/ToUnicode` stream a few lines away. Assert both, so the two cannot drift into one.
    let info = descendant
        .get(b"CIDSystemInfo")
        .ok()
        .and_then(|o| o.as_dict().ok());
    let info = info.unwrap_or_else(|| panic!("the descendant must carry a /CIDSystemInfo"));
    let read = |key: &[u8]| -> String {
        info.get(key)
            .ok()
            .and_then(|o| o.as_str().ok())
            .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
            .unwrap_or_default()
    };
    assert_eq!(read(b"Registry"), "Adobe", "R3 fixes the registry");
    assert_eq!(
        read(b"Ordering"),
        "Identity",
        "the descendant's /Ordering names the collection our CIDs are drawn from, which is the \
         identity ordering. `UCS` belongs to the /ToUnicode stream, where the destinations are \
         code points; copying it here is the silent error R3 exists to prevent"
    );
    assert_eq!(
        info.get(b"Supplement").ok().and_then(|o| o.as_i64().ok()),
        Some(0)
    );
    let cmap_bytes = document
        .get_object(font.get(b"ToUnicode").unwrap().as_reference().unwrap())
        .unwrap()
        .as_stream()
        .unwrap()
        .decompressed_content()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&cmap_bytes).contains("/Ordering (UCS)"),
        "the /ToUnicode stream's own /CIDSystemInfo must stay UCS; if this ever reads \
         (Identity), the two objects have been conflated"
    );

    // ---- R1: `/DW` present, `/W` in explicit-pair form, no range, no CID 0 -------------
    assert!(
        descendant.get(b"DW").is_ok(),
        "/DW must always be written: a CID the /W array does not name still has to measure"
    );
    let widths = descendant.get(b"W").ok().and_then(|o| o.as_array().ok());
    let widths = widths.unwrap_or_else(|| panic!("/W must be an array"));
    assert!(
        !widths.is_empty() && widths.len().is_multiple_of(2),
        "/W must alternate cid and width-list; got {} entries",
        widths.len()
    );
    for pair in widths.chunks(2) {
        assert!(
            pair[1].as_array().is_ok(),
            "every /W entry must be [cid [w1 w2 …]]; a bare integer here would be the \
             [cfirst clast w] range form, which equalises advances the program distinguishes"
        );
        assert_ne!(
            pair[0].as_i64().unwrap_or(0),
            0,
            "CID 0 is .notdef and no run can draw it; an /W entry for it is a claim about a \
             glyph that is not in the text"
        );
    }

    // ---- R5's invariant, over every CID the source drew -------------------------------
    //
    // This is what catches a writer that places the *new* CID below the highest retained one —
    // a plausible slip (`min` for `max`) that overwrites a retained glyph's slot with the
    // replacement's outline. Nothing draws that glyph in this fixture, so no pixel assertion
    // can see the damage; the correspondence can.
    let written_font =
        CidFont::resolve(&document, font).expect("the written font parses as a Type0 font");
    let written_pairs = written_font
        .pairs()
        .expect("the written font states a /ToUnicode");
    for (cid, ch) in &source_pairs {
        assert!(
            written_pairs.contains(&(*cid, *ch)),
            "CID {cid} drew {ch:?} in the source document, and the written font no longer maps \
             it to that character. The written correspondence is {written_pairs:?}. R5 permits \
             adding a CID above the highest the source used; it does not permit disturbing one \
             below that boundary."
        );
    }

    // And the CIDs the document itself still shows must be the ones the program answers for.
    // Without this, a writer that renumbered *both* the program and the content consistently
    // would pass the loop above while still corrupting every run it did not rewrite.
    let runs_after = {
        let page_id = textlocate::page_id(&document, 0).unwrap();
        textlocate::locate_runs(&document, page_id).unwrap()
    };
    let untouched = &runs_after[2];
    assert_eq!(
        untouched.id.op_index, 13,
        "the third run is still the last operand in the page's own content stream"
    );
    let shown = &untouched.bytes;
    assert_eq!(
        shown,
        &[0, GID_A_LOWER as u8, 0, GID_B_LOWER as u8],
        "the run the edit never touched must still show CIDs {GID_A_LOWER} and {GID_B_LOWER}, \
         the ids it had before. It now shows {shown:?}. A writer may add glyphs, but it may not \
         renumber the ones already in the document."
    );
}

// --- 6c: the edited run is not exempt ------------------------------------------

/// Row 6, assertion 3: the edited run draws at the advance `/W` claims.
///
/// Without this the row could be passed by leaving the edit broken: an untouched run is
/// trivially unchanged when the write refuses, so 6a and 6b say nothing about whether the
/// *edited* run came out right. This one is about the run that changed.
///
/// It must edit to a character the original cannot draw, or `original_covers` succeeds, the
/// original font is kept, and the check measures the fixture instead of the writer. `Ω` is
/// outside WinAnsi and absent from the fixture's subset, so a new program is unavoidable —
/// which is exactly the state this test must observe.
#[test]
#[ignore = "requires a real PDFium library; `cargo test -- --include-ignored` after fetching it"]
fn row6c_the_edited_run_renders_at_the_advance_the_width_array_claims() {
    let mut document = load();
    match edit_middle_run(&mut document, NON_WINANSI) {
        Ok(_) => {}
        Err(e) => panic!(
            "row 6c cannot run: the edit is refused before anything is drawn ({e}). This is \
             the expected initial failure — no Type0 writer exists. When P2.7.7 lands the edit \
             succeeds and the width check below runs against the /W the writer produced."
        ),
    }

    // Read the width the *writer* declared for the CID that now draws the replacement, rather
    // than a number this test chose. A test that asserted against its own constant would pass
    // for any /W at all.
    let page_id = textlocate::page_id(&document, 0).unwrap();
    // The edited run's own resource: the writer leaves the original `F1` for the untouched
    // runs and installs a new one beside it, so `F1` is the *source* font and cannot express
    // the replacement at all.
    let edited_key = runs(&document)[1].font_key.clone();
    let font = textlocate::font_dict(&document, page_id, &edited_key)
        .expect("the edited run's font resource resolves");
    let written = CidFont::resolve(&document, font).expect("the written font is readable");
    let codes = written
        .encode(NON_WINANSI)
        .expect("the written font can express the replacement it was built for");
    let claimed = written.advance(&codes, 24.0).expect(
        "the written font states a width for its own CID; a /W that omits it and a /DW \
                 that is absent would make the width unmeasurable, which is a refusal not a pass",
    );

    let image = render(&saved(&document), "row6c.pdf");
    let runs_after = runs(&document);
    let edited = &runs_after[1];

    // `/W` must agree with the **program's own `hmtx`**, and these are two genuinely
    // independent numbers: `/W` is a claim in a PDF dictionary, `hmtx` is a table inside the
    // font program. Comparing `/W` against `CidFont::advance` would compare `/W` with itself —
    // that is what an earlier version of this test did, and mutation testing showed it: a
    // writer that inflated every `/W` entry by 200/1000 em survived, because both sides of the
    // comparison read the inflated value.
    let program = {
        let descendants = font
            .get(b"DescendantFonts")
            .unwrap()
            .as_array()
            .unwrap()
            .clone();
        let descendant = document
            .get_object(descendants[0].as_reference().unwrap())
            .unwrap()
            .as_dict()
            .unwrap()
            .clone();
        let descriptor = document
            .get_object(
                descendant
                    .get(b"FontDescriptor")
                    .unwrap()
                    .as_reference()
                    .unwrap(),
            )
            .unwrap()
            .as_dict()
            .unwrap()
            .clone();
        document
            .get_object(
                descriptor
                    .get(b"FontFile2")
                    .unwrap()
                    .as_reference()
                    .unwrap(),
            )
            .unwrap()
            .as_stream()
            .unwrap()
            .decompressed_content()
            .expect("the written program is a readable Flate stream")
    };
    let face = ttf_parser::Face::parse(&program, 0).expect("the written program parses");
    let cid = u16::from_be_bytes([codes[0], codes[1]]);
    let scaled = u32::from(
        face.glyph_hor_advance(ttf_parser::GlyphId(cid))
            .unwrap_or(0),
    ) * 1000
        / u32::from(face.units_per_em());
    let truth = scaled as f32 * 24.0 / 1000.0;
    assert!(
        !advance_disagrees(claimed, truth),
        "the /W written for CID {cid} claims {claimed} pt, but the glyph actually advances \
         {truth} pt ({scaled}/1000 em at 24pt) according to the program's own `hmtx`. A /W that \
         disagrees with the hmtx it ships is the silent-wrongness shape: the file parses, the \
         text is right, and every following character sits at the wrong x."
    );

    // And it must actually draw, at that advance. An edit that produced no ink would satisfy
    // 6a's letter while breaking the document.
    let rect = run_rect(edited, 4.0, 792.0);
    assert!(
        ink_pixels(&image, rect) > 0,
        "the edited run draws no ink inside {rect:?}; a write that produced nothing is not a \
         pass, it is a deletion"
    );
}

// ---- Composites ----------------------------------------------------------------------
//
// The first version of this file exercised only `A`,`B`,`C`,`a`,`b`,`Ω`, and all six are
// **simple** glyphs in Liberation Sans: `numberOfContours` is 0, 2, 2, 1, 1 and 1 respectively.
// So `subset_at_cids`'s composite path never ran, and a mutant that rewrote each component id
// to the *bundle* id instead of the new slot SURVIVED. Measured, not assumed — the numbers below
// are read from the bundled face by the test itself.
//
// `é` (U+00E9) is glyph 171 with `numberOfContours == -1` and components `[72, 118]`; `ü`
// (U+00FC) is 190 with `[88, 106]`; and `Å` (U+00C5) is 135 with `[36, 668]`, where component 36
// is `A` — a character the document already draws at its own CID. That last case is the sharp
// one: the component is both a live CID and a composite component, and it must resolve to the
// CID's slot rather than to a fresh one, or the ring is drawn from the wrong outline.

/// The component ids a written glyph record names, read straight from the program's `glyf`.
///
/// Deliberately not `SourceTables::components`: a check that reuses the reader it is checking
/// cannot fail when the reader is what broke.
fn written_components(program: &[u8], gid: u16) -> Option<Vec<u16>> {
    let read_u16 = |at: usize| -> Option<u16> {
        Some(u16::from_be_bytes([
            *program.get(at)?,
            *program.get(at + 1)?,
        ]))
    };
    let read_u32 = |at: usize| -> Option<u32> {
        Some(u32::from_be_bytes([
            *program.get(at)?,
            *program.get(at + 1)?,
            *program.get(at + 2)?,
            *program.get(at + 3)?,
        ]))
    };
    let tables = {
        let count = read_u16(4)? as usize;
        let mut map = std::collections::BTreeMap::new();
        for index in 0..count {
            let base = 12 + index * 16;
            let tag: [u8; 4] = program.get(base..base + 4)?.try_into().ok()?;
            map.insert(
                tag,
                (read_u32(base + 8)? as usize, read_u32(base + 12)? as usize),
            );
        }
        map
    };
    let loca_at = tables.get(b"loca")?.0;
    let glyf_at = tables.get(b"glyf")?.0;
    let long = read_u16(tables.get(b"head")?.0 + 50)? != 0;
    let at = usize::from(gid);
    let (start, end) = if long {
        (
            read_u32(loca_at + at * 4)? as usize,
            read_u32(loca_at + at * 4 + 4)? as usize,
        )
    } else {
        (
            read_u16(loca_at + at * 2)? as usize * 2,
            read_u16(loca_at + at * 2 + 2)? as usize * 2,
        )
    };
    if end <= start {
        return Some(Vec::new());
    }
    let data = program.get(glyf_at + start..glyf_at + end)?;
    if i16::from_be_bytes([data[0], data[1]]) != -1 {
        return Some(Vec::new());
    }
    let mut out = Vec::new();
    let mut rest = &data[10..];
    loop {
        let flags = u16::from_be_bytes([rest[0], rest[1]]);
        out.push(u16::from_be_bytes([rest[2], rest[3]]));
        let mut step = 4;
        step += if flags & 0x0001 != 0 { 4 } else { 2 };
        step += if flags & 0x0008 != 0 {
            2
        } else if flags & 0x0040 != 0 {
            4
        } else if flags & 0x0080 != 0 {
            8
        } else {
            0
        };
        rest = rest.get(step..)?;
        if flags & 0x0020 == 0 {
            break;
        }
    }
    Some(out)
}

/// `subset_at_cids` rewrites composite components to the slots it actually wrote.
///
/// This is the test the surviving mutant asked for. It fails if a component id is left as the
/// bundle's id: the bundle's `é` names `72` and `118`, and neither number is a CID in this
/// document, so the composite would point at empty slots and draw a bare accent.
#[test]
fn composites_are_rewritten_to_the_slots_the_writer_wrote() {
    let face = embed::face_for_style("Helvetica").expect("the bundled Sans face");
    let parsed = ttf_parser::Face::parse(face.bytes, 0).expect("the bundle parses");

    // Each string's source glyphs, straight from the bundle, so the test states what it means
    // by "composite" rather than assuming it.
    for (ch, expected_source_components) in [
        ('é', [72u16, 118]),
        ('ü', [88, 106]),
        // `Å`'s first component is `A` itself, which this document also draws.
        ('Å', [36, 668]),
    ] {
        let gid = parsed
            .glyph_index(ch)
            .map(|glyph| glyph.0)
            .unwrap_or_else(|| panic!("the bundle has no glyph for {ch:?}"));
        assert_eq!(
            written_components(face.bytes, gid).as_deref(),
            Some(&expected_source_components[..]),
            "the bundled face no longer draws {ch:?} (glyph {gid}) as a composite over \
             {expected_source_components:?}. The premise of this test is a measured fact about \
             the font, so it is asserted rather than assumed"
        );

        // `A` is placed at CID 1 and drawn from the same slot; `ch` is new, so R5 puts it above
        // the highest retained CID (1) at 2. Its components must go above *both*, or a
        // component would land on a slot the document already uses.
        let subset =
            embed::subset_at_cids(face, &[(1, 'A'), (2, ch)], &|c| (c == 'A').then_some(1))
                .unwrap_or_else(|e| panic!("subset_at_cids refused {ch:?}: {e}"));

        let written = ttf_parser::Face::parse(&subset.program, 0).expect("the written program");
        let written_gid = written
            .glyph_index(ch)
            .map(|glyph| glyph.0)
            .unwrap_or_else(|| panic!("the written program does not map {ch:?}"));
        assert_eq!(
            written_gid, 2,
            "{ch:?} must be placed at the CID R5 assigns it (2), not at {written_gid}"
        );

        let components = written_components(&subset.program, written_gid)
            .expect("the written composite record is readable");
        assert_eq!(
            components.len(),
            expected_source_components.len(),
            "the written {ch:?} names {} components but the bundle's version names {}",
            components.len(),
            expected_source_components.len()
        );
        assert!(
            !components
                .iter()
                .any(|c| expected_source_components.contains(c)),
            "the written {ch:?} still names the *bundle's* component ids {components:?}. Those \
             numbers are not CIDs in this document: under /Identity a glyph id is a CID, the \
             document placed nothing at {:?}, and the composite would draw empty slots. This is \
             the mutation that survived before this test existed",
            expected_source_components
        );
        // A component must name a slot that exists, and the only permitted overlap with a live
        // CID is `A` at its own CID — see the dedicated test below, which is what forces this.
        let slots = subset
            .assignments
            .iter()
            .map(|(cid, _)| *cid)
            .collect::<Vec<_>>();
        for component in &components {
            assert!(
                !slots.contains(component) || *component == 1,
                "component {component} of {ch:?} collides with the live CIDs {slots:?}; the \
                 only permitted overlap is `A` at its own CID"
            );
            assert!(
                written_components(&subset.program, *component).is_some(),
                "component {component} of {ch:?} does not name a readable glyph slot"
            );
        }
    }
}

/// `Å`'s ring component is `A`, which the document also draws at CID 1.
///
/// The two uses must resolve to the *same* slot, because the outline is identical and
/// `/Identity` gives a glyph id exactly one meaning. A writer that handed `A` a fresh component
/// slot would duplicate the outline and, worse, leave CID 1 ambiguous: the same shape reachable
/// at two ids with only one of them named by `/W`.
#[test]
fn a_component_that_is_also_a_cid_reuses_that_cids_slot() {
    let face = embed::face_for_style("Helvetica").expect("the bundled Sans face");
    let subset = embed::subset_at_cids(face, &[(1, 'A'), (2, 'Å')], &|c| (c == 'A').then_some(1))
        .expect("subset_at_cids accepts Å over a retained A");

    // The slot count proves no duplicate was written: `.notdef` at 0, `A` at 1, `Å` at 2, and
    // one fresh slot for the ring at 3. Four. A fifth would mean `A` shipped twice — once as
    // CID 1 and again at a slot no run and no `/W` entry names.
    let written = ttf_parser::Face::parse(&subset.program, 0).expect("the written program");
    assert_eq!(
        written.number_of_glyphs(),
        4,
        "expected `.notdef`, `A`, `Å` and the ring, so four slots; the program has {}. More \
         means `A` was shipped twice, fewer means a component is missing",
        written.number_of_glyphs()
    );
    let components =
        written_components(&subset.program, 2).expect("the written Å is a readable composite");
    assert!(
        components.contains(&1),
        "Å is built from A plus a ring, and A is already CID 1 of this document. The written \
         composite names {components:?}, which does not include 1, so the ring is being drawn \
         over a *second* copy of A that no run and no /W entry refers to"
    );
}

// ---- The plan a retained run gets ----------------------------------------------------
//
// These two drive `substitute::plan`, the decision the Apply button reaches through
// `textedit::edit`. They are the click path's first half: what the writer is *asked* to do,
// before any bytes are written. Row 6's own tests start one step later, at `subset_at_cids`,
// so a misroute here would never have shown up in them.

/// The `(gates, font dictionary)` pair `textedit::edit` builds for one run of the fixture.
fn plan_inputs(
    document: &lopdf::Document,
    wanted: &str,
) -> (textedit::RunGates, lopdf::Dictionary) {
    let page_id = textlocate::page_id(document, 0).expect("page 0");
    let all = runs(document);
    let run = all
        .iter()
        .find(|run| run.text == wanted)
        .unwrap_or_else(|| panic!("the fixture has no run reading {wanted:?}"));
    let gates = textedit::resolve_gates(document, 0, run).expect("the run's gates resolve");
    let font = textlocate::font_dict(document, page_id, &run.font_key)
        .unwrap_or_else(|| {
            panic!(
                "the run's font {} does not resolve",
                String::from_utf8_lossy(&run.font_key)
            )
        })
        .clone();
    (gates, font)
}

/// With substitution off, a retained CID run that cannot set the replacement refuses
/// `SUBSTITUTE_REFUSED`, and the document is untouched.
///
/// `x` and `y` are absent from the fixture's embedded program: its `cmap` has exactly three
/// format-4 segments (`0041-0043`, `0061-0062`, `FFFF-FFFF`), so `glyph_index` returns `None`
/// for both rather than `.notdef`. That is what makes this a refusal and not a silent
/// `.notdef` write.
#[test]
fn editing_a_retained_run_refuses_without_substitution() {
    let document = load();
    let (gates, font) = plan_inputs(&document, "ab");

    let outcome = substitute::plan_with_font(&document, Some(&font), &gates.font, "xy", false);

    match outcome {
        Err(AppError::SubstituteRefused) => {}
        Err(other) => panic!("expected SUBSTITUTE_REFUSED, got {}: {other}", other.code()),
        Ok(plan) => panic!(
            "with substitution off and no glyph for `x` or `y` in the run's own font, the \
             edit must refuse; the plan was {plan:?}"
        ),
    }
}

/// With substitution ON, a CID run goes to the CID writer — never to MECH-A.
///
/// This is the ordering guard. A Type0 run cannot take the substitution route: its content
/// stream holds two-byte CIDs, and a standard-14 substitute draws one-byte codes, so
/// "substituting the font" would mean substituting the encoding too. The panel says as much
/// in its own label ("CID/Type0 runs keep their own font").
///
/// The replacement is deliberately an **in-WinAnsi** one. `afm::covers("Helvetica", "xy")` is
/// true, so before the reorder this returned `Plan::Substitute` and wrote Helvetica into a
/// CID-keyed page. A non-WinAnsi character such as `Ω` does *not* discriminate: it fails
/// `afm::covers` anyway and so reached `plan_cid` even with the defect present.
#[test]
fn editing_a_retained_run_reaches_the_cid_writer() {
    let document = load();
    let (gates, font) = plan_inputs(&document, "ab");

    let plan = substitute::plan_with_font(&document, Some(&font), &gates.font, "xy", true)
        .unwrap_or_else(|error| panic!("expected the CID writer, got {}: {error}", error.code()));

    match &plan {
        substitute::Plan::EmbedCid { subset, .. } => {
            // The replacement's bytes must be CIDs the written program draws, or the two
            // halves disagree even though the plan is the right variant.
            assert_eq!(
                subset.encode("xy").map(|bytes| bytes.len()),
                Some(4),
                "a two-character replacement is four bytes of two-byte CIDs"
            );
        }
        substitute::Plan::Substitute { base_font, .. } => panic!(
            "a CID-keyed run was routed to the standard-14 substitute `{base_font}`. That \
             replaces the face but not the two-byte encoding, so the run's codes would name \
             CIDs the substitute does not draw — and it contradicts the panel's own \
             \"CID/Type0 runs keep their own font\" label"
        ),
        other => panic!("expected EmbedCid for a CID-keyed run, got {other:?}"),
    }
}
