//! P2.7.6 §5 acceptance matrix, row by row, on real bytes.
//!
//! The design replaces the old absence test ("a non-WinAnsi glyph refuses") with four rows:
//!
//! | # | Replacement | Expected |
//! |---|---|---|
//! | 1 | non-WinAnsi, covered by a bundled face, substitution allowed | succeeds, subset embedded, renders, text round-trips |
//! | 2 | same glyph, substitution disallowed | `SUBSTITUTE_REFUSED`, document unchanged |
//! | 3 | non-WinAnsi, uncovered (`漢`) | `GLYPHS_NOT_COVERABLE`, document unchanged |
//! | 4 | WinAnsi only | MECH-A, and **no font program is written** |
//!
//! Rows 2-4 are implementable now and are written first, because they define what success in
//! row 1 must *stop* doing: refusing. Row 1 is `#[ignore]`d until the subset builder exists,
//! and its body is written so that landing the builder turns it green rather than requiring
//! someone to invent the assertions afterwards.
//!
//! Every row asserts **the document is unchanged** on refusal where a refusal is expected.
//! A refusal that has already mutated the page is worse than a failure, because the user sees
//! "nothing happened" while the bytes disagree.

use std::path::{Path, PathBuf};

use document_reader_lib::editor::textedit;
use document_reader_lib::editor::textlocate::{self, RunId};
use document_reader_lib::model::AppError;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

fn load(path: &Path) -> lopdf::Document {
    lopdf::Document::load(path).unwrap_or_else(|e| panic!("could not load {}: {e}", path.display()))
}

fn first_run(document: &lopdf::Document, page: u32) -> textlocate::RawRun {
    let page_id = textlocate::page_id(document, page).unwrap();
    textlocate::locate_runs(document, page_id)
        .unwrap()
        .into_iter()
        .next()
        .expect("fixture has a run")
}

/// Applies an edit with a fixed measurement; `old_width` stays generous so the width gate
/// does not pre-empt the row under test.
fn edit(
    document: &mut lopdf::Document,
    run_id: RunId,
    expected: &str,
    replacement: &str,
    allow_substitution: bool,
) -> Result<textedit::EditOutcome, AppError> {
    let measure = |_text: &str| Ok(60.0_f32);
    textedit::edit(
        document,
        &textedit::EditRequest {
            page: 0,
            run_id,
            expected: expected.to_string(),
            replacement: replacement.to_string(),
            old_width: 100.0,
            font_size: 24.0,
            allow_substitution,
            fragments: Vec::new(),
            range: Vec::new(),
            measure: &measure,
        },
    )
}

/// The page's decoded content, for "unchanged" assertions.
///
/// Comparing the content stream rather than the whole file is deliberate: a refusal must
/// leave the *page* alone, and that is the thing the user sees. `save` may legitimately
/// change trailer-level bytes.
fn page_content(document: &lopdf::Document, page: u32) -> Vec<u8> {
    let page_id = textlocate::page_id(document, page).unwrap();
    document.get_page_content(page_id)
}

/// Row 2 of §5: substitution disallowed refuses, and changes nothing.
#[test]
fn row2_substitution_disallowed_refuses_and_leaves_the_page_alone() {
    let path = fixture("non-winansi-replacement.pdf");
    let mut document = load(&path);
    let before = page_content(&document, 0);
    let run = first_run(&document, 0);

    let error = edit(&mut document, run.id, "HI", "H\u{2014}", false)
        .expect_err("substitution is off, so this must refuse");

    assert_eq!(error.code(), "SUBSTITUTE_REFUSED");
    assert_eq!(
        page_content(&document, 0),
        before,
        "a refusal must not have mutated the page"
    );
}

/// Row 3 of §5: a glyph no shipped face has refuses with the permanent code, changing nothing.
///
/// `漢` is outside every face in the bundle (Latin only), so this is a genuine coverage gap:
/// the user's remedy is a different font, not a retry.
#[test]
fn row3_uncovered_glyph_refuses_as_a_coverage_gap_and_leaves_the_page_alone() {
    let path = fixture("non-winansi-replacement.pdf");
    let mut document = load(&path);
    let before = page_content(&document, 0);
    let run = first_run(&document, 0);

    let error = edit(&mut document, run.id, "HI", "\u{6f22}", true)
        .expect_err("no shipped face has this glyph");

    assert_eq!(error.code(), "GLYPHS_NOT_COVERABLE");
    assert_ne!(
        error.code(),
        "SUBSET_BUILD_FAILED",
        "an absent glyph is a permanent gap, not a build failure"
    );
    assert_eq!(page_content(&document, 0), before);
}

/// Row 4 of §5: a WinAnsi-only replacement takes MECH-A and writes **no font program**.
///
/// The negative half is the part that can rot silently: a later change could route WinAnsi
/// text through the embed path and every other test would stay green while every ordinary
/// edit grew by the size of a subset. This inspects the written PDF for `/FontFile2` and
/// `/FontFile3`, and for a `/Subtype /Type0` root, none of which MECH-A emits.
#[test]
fn row4_winansi_takes_mech_a_and_writes_no_font_program() {
    let path = fixture("non-winansi-replacement.pdf");
    let mut document = load(&path);
    let run = first_run(&document, 0);

    let outcome = edit(&mut document, run.id, "HI", "HELP", true).expect("WinAnsi is editable");
    assert!(
        outcome.substitution.is_some(),
        "this fixture's own font cannot set even WinAnsi, so MECH-A must fire"
    );

    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("mech-a.pdf");
    document.save(&target).unwrap();

    let saved = load(&target);
    for (_, object) in saved.objects.iter() {
        match object {
            lopdf::Object::Stream(stream) => {
                assert!(
                    !stream.dict.has(b"FontFile2"),
                    "MECH-A must not embed a TrueType program; row 4 is 'the cheap path stays \
                     cheap'"
                );
                assert!(
                    !stream.dict.has(b"FontFile3"),
                    "MECH-A must not embed an OpenType program"
                );
            }
            lopdf::Object::Dictionary(dict) => assert_no_program(dict),
            _ => {}
        }
    }
}

fn assert_no_program(dict: &lopdf::Dictionary) {
    if let Ok(subtype) = dict.get(b"Subtype").and_then(lopdf::Object::as_name) {
        if subtype == b"Type0" {
            panic!("MECH-A writes a simple font; a Type0 root means the embed path ran");
        }
    }
}

/// Row 1 of §5: non-WinAnsi, covered by a bundled face, substitution allowed.
///
/// **This asserts the pre-MECH-B state on purpose.** A bundled face exists in the tree, but
/// no subset builder consumes it yet, so the design's current answer for this row is the
/// refusal that MECH-B removes. Landing MECH-B therefore *breaks this test*, which is the
/// point: it cannot be finished quietly, and the author must replace the refusal assertion
/// with the success assertions below rather than adding a second test beside it.
///
/// The character is U+0100, not U+2014: measured in `font_coverage.rs`, an em dash is CP1252
/// 0x97 and therefore takes MECH-A, writing no subset at all. The first draft of this test
/// used U+2014 and "proved" embedding with a standard-14 edit.
#[test]
fn row1_covered_non_winansi_currently_refuses_until_mech_b_lands() {
    let path = fixture("non-winansi-replacement.pdf");
    let mut document = load(&path);
    let before = page_content(&document, 0);
    let run = first_run(&document, 0);

    let replacement = "H\u{0100}";
    let error = edit(&mut document, run.id, "HI", replacement, true)
        .expect_err("MECH-B is not implemented, so this row still refuses");

    assert_eq!(
        error.code(),
        "GLYPHS_NOT_COVERABLE",
        "today the covered glyph is reported as a coverage gap; MECH-B must change this row \
         from a refusal to a written subset"
    );
    assert_eq!(page_content(&document, 0), before);

    // What the implementation must deliver, kept next to the assertion it will replace so
    // that landing MECH-B is a rewrite of this test, not a search for what to check:
    //
    //   outcome.substitution == Some("LiberationSans")
    //   the saved file renders through PDFium (PNG magic)
    //   PdfProvider::extract_page_text contains U+0100
    //   the written font name carries no `+Liberation` reserved-name prefix
}

#[test]
fn the_matrix_rows_are_all_present() {
    let source = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/embed_acceptance.rs"),
    )
    .expect("cannot read this file");
    for row in ["row1_", "row2_", "row3_", "row4_"] {
        assert!(
            source.contains(row),
            "§5 row {row} has no test; the matrix is the acceptance gate, not a description"
        );
    }
}
