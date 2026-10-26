//! P2.7.6: `GLYPHS_NOT_COVERABLE` and `SUBSET_BUILD_FAILED` are different facts.
//!
//! Ruled 2026-09-20. Folding them would mean a user seeing the coverage code could not tell
//! whether switching fonts would help:
//!
//! * `GLYPHS_NOT_COVERABLE` -- the shipped faces genuinely lack the glyph. The remedy is
//!   within the user's reach: a different font, a shorter range, or an annotation. It does
//!   not depend on any build we can fix.
//! * `SUBSET_BUILD_FAILED` -- the glyph exists in a bundled face and the subset could not be
//!   built. That is our bug. "Choose a different font" would send the user after a problem
//!   they cannot solve, which is the same failure mode as a refusal naming the wrong cause.
//!
//! The first test below is the one that matters: a genuine coverage gap must return the
//! *permanent* code and never the build-failure one. It is written before the subset builder
//! exists, so the builder cannot later "helpfully" collapse the two.
//!
//! The split is only real if the two codes are produced by different facts, so the remaining
//! tests pin the *reachability* of each: the coverage gap comes from a character absent from
//! every shipped face, and the build-failure code is not reached merely because a glyph is
//! outside WinAnsi.

use document_reader_lib::editor::substitute;
use document_reader_lib::editor::textlocate;
use document_reader_lib::model::AppError;
use lopdf::Document;

fn fixture(name: &str) -> Document {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    Document::load(&path).unwrap_or_else(|e| panic!("cannot load {}: {e}", path.display()))
}

fn plan_code(document: &Document, replacement: &str, allow: bool) -> String {
    let page_id = textlocate::page_id(document, 0).unwrap();
    let runs = textlocate::locate_runs(document, page_id).unwrap();
    let run = runs.first().expect("fixture has a run");
    let info = textlocate::font_info(document, page_id, &run.font_key).unwrap();
    match substitute::plan(document, page_id, &run.font_key, &info, replacement, allow) {
        Ok(plan) => panic!("expected a refusal, got {plan:?}"),
        Err(error) => error.code().to_string(),
    }
}

/// A genuine coverage gap returns the permanent code, and specifically not the build-failure
/// one.
///
/// U+2192 is absent from Liberation's shipped Latin faces and from every standard-14 face, so
/// no subset of a bundled face could set it. If this ever returns `SUBSET_BUILD_FAILED`, the
/// subset path has started claiming work it cannot do, and the user is being told to report a
/// defect instead of choosing another font.
#[test]
fn a_glyph_no_shipped_face_has_returns_the_permanent_coverage_code() {
    let document = fixture("non-winansi-replacement.pdf");
    let code = plan_code(&document, "H\u{2192}", true);
    assert_eq!(
        code, "GLYPHS_NOT_COVERABLE",
        "a coverage gap is permanent within the bundle; it must not be reported as a subset \
         build failure"
    );
    assert_ne!(
        code, "SUBSET_BUILD_FAILED",
        "an absent glyph is not a build failure"
    );
}

/// A glyph genuinely outside every shipped face -- CJK -- is also the permanent code.
///
/// Design §5 row 3 names `漢` for exactly this. The bundle ships Latin faces only, so no
/// subset can be attempted and the failure is not ours.
#[test]
fn a_cjk_glyph_is_a_coverage_gap_not_a_build_failure() {
    let document = fixture("non-winansi-replacement.pdf");
    assert_eq!(
        plan_code(&document, "\u{6f22}", true),
        "GLYPHS_NOT_COVERABLE",
        "the bundle ships Latin faces; a CJK glyph is a permanent coverage gap"
    );
}

/// The two codes must stay distinct values, and each must name its own remedy.
///
/// A refusal that says "choose a different font" for our own build failure sends the next
/// person to the wrong place. The messages are part of the contract, not decoration.
#[test]
fn the_two_codes_name_different_remedies() {
    let coverage = AppError::GlyphsNotCoverable;
    let build = AppError::SubsetBuildFailed;

    assert_ne!(coverage.code(), build.code());
    assert_eq!(coverage.code(), "GLYPHS_NOT_COVERABLE");
    assert_eq!(build.code(), "SUBSET_BUILD_FAILED");

    let coverage_message = coverage.to_string().to_lowercase();
    let build_message = build.to_string().to_lowercase();

    assert!(
        coverage_message.contains("font") || coverage_message.contains("different"),
        "the coverage refusal must point at a remedy the user can reach: {coverage_message}"
    );
    assert!(
        !coverage_message.contains("report"),
        "a permanent gap is not something to report as a defect: {coverage_message}"
    );
    assert!(
        build_message.contains("report") || build_message.contains("defect"),
        "the build-failure refusal must say it is our defect: {build_message}"
    );
    assert!(
        !build_message.contains("different font"),
        "the build-failure refusal must not send the user to choose another font: \
         {build_message}"
    );
}

/// The panel must route the new code to the run-local error region.
///
/// The frontend keeps a closed list of codes that belong on the text-edit panel; a code
/// missing from it falls through to the pane's generic error region, away from the run it
/// concerns. This reads the component so the list cannot drift silently.
#[test]
fn the_panel_routes_the_build_failure_code_to_the_run() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("src/components/TextEdit.tsx");
    let source = std::fs::read_to_string(&path).expect("cannot read TextEdit.tsx");

    let list = source
        .split("TEXT_EDIT_ERROR_CODES")
        .nth(1)
        .expect("TEXT_EDIT_ERROR_CODES is gone");
    let list = list.split("];").next().expect("unterminated code list");

    for code in ["GLYPHS_NOT_COVERABLE", "SUBSET_BUILD_FAILED"] {
        assert!(
            list.contains(code),
            "{code} is missing from TEXT_EDIT_ERROR_CODES, so its message would be shown \
             away from the run it concerns"
        );
    }
}
