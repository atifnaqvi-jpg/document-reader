//! P2.7.6 R4: the plan order is checked before the embed path exists.
//!
//! R4 is explicit that this guard is written *first*: "the first guard this phase writes
//! asserts the order, and it is written **before** the embed path exists, so the path cannot
//! be introduced with the order already bypassed."
//!
//! That timing is the whole point. Once a `Plan::Embed` variant exists, a test asserting
//! "keep-original wins" can be satisfied by a `plan()` that happens to check coverage first.
//! Written now, while the variant is absent, it forces the next change to keep the order
//! rather than rediscover it.
//!
//! Order under test (design §2.1):
//!
//!   KeepOriginal -> MECH-A (standard-14, WinAnsi) -> MECH-B (bundled face) -> refusal
//!
//! The properties, each of which a naive implementation gets wrong:
//!
//! 1. **Keep-original first, unconditionally.** Even with substitution disabled, and even
//!    though a bundled face is now in the tree, a run whose own font covers the replacement
//!    is never rewritten.
//! 2. **A WinAnsi replacement takes MECH-A.** The standard-14 reference costs no bytes and
//!    every viewer has it; reaching for a subset first would bloat every ordinary edit.
//! 3. **Refusal stays distinct.** `SUBSTITUTE_REFUSED` (the user said no) and
//!    `GLYPHS_NOT_COVERABLE` (no font can) are different problems (R3).
//!
//! These read committed fixtures rather than `testpdf`, which is `cfg(test)`-only and
//! unavailable to integration tests.

use document_reader_lib::editor::substitute::{self, Plan};
use document_reader_lib::editor::textlocate;
use lopdf::Document;

fn fixture(name: &str) -> Document {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    Document::load(&path).unwrap_or_else(|e| panic!("cannot load {}: {e}", path.display()))
}

fn first_run(document: &Document) -> textlocate::RawRun {
    let page_id = textlocate::page_id(document, 0).unwrap();
    textlocate::locate_runs(document, page_id)
        .unwrap()
        .into_iter()
        .next()
        .expect("fixture has a run")
}

/// Plans one replacement against the fixture's first run.
fn plan_for(document: &Document, replacement: &str, allow: bool) -> Result<Plan, String> {
    let page_id = textlocate::page_id(document, 0).unwrap();
    let run = first_run(document);
    let info = textlocate::font_info(document, page_id, &run.font_key).unwrap();
    substitute::plan(document, page_id, &run.font_key, &info, replacement, allow)
        .map_err(|e| format!("{}: {e}", e.code()))
}

/// Property 2: a WinAnsi-only replacement is satisfied by MECH-A.
///
/// `simple-embedded-subset.pdf` has a subset-flagged name and no program, so its run is not
/// covered by its own font. The replacement `EDIT` is pure WinAnsi: the plan must be a
/// standard-14 substitute, which writes no font program. If the embed path were tried first,
/// this edit would carry a subset it does not need — matrix row 4, "the cheap path stays
/// cheap".
#[test]
fn a_winansi_replacement_takes_the_standard_14_path() {
    let document = fixture("simple-embedded-subset.pdf");
    match plan_for(&document, "EDIT", true) {
        Ok(Plan::Substitute { base_font, .. }) => assert!(
            ["Helvetica", "Times-Roman", "Courier"].contains(&base_font.as_str()),
            "a WinAnsi replacement must take a standard-14 face, got {base_font}"
        ),
        other => panic!("expected MECH-A, got {other:?}"),
    }
}

/// Property 3: the two refusals stay distinct.
///
/// `SUBSTITUTE_REFUSED` is resolved by the user saying yes; `GLYPHS_NOT_COVERABLE` is not
/// resolved by any toggle. Collapsing them tells a user to change a setting that will not
/// help.
#[test]
fn the_two_refusals_stay_distinct() {
    let document = fixture("non-winansi-replacement.pdf");

    let refused = plan_for(&document, "EDIT", false).expect_err("substitution is off");
    assert!(
        refused.starts_with("SUBSTITUTE_REFUSED"),
        "expected SUBSTITUTE_REFUSED, got {refused}"
    );

    // U+6F22, not U+2192: measured against the shipped faces, **every** Liberation style has
    // U+2192 (and U+0100), while none has U+6F22. An earlier draft of this test used U+2192 and
    // called it uncovered; it passed only because MECH-B does not exist yet, and it would have
    // inverted meaning the moment the embed path landed -- "the uncovered case" would have
    // become a covered one, still refusing, and the test would have said "expected".
    let uncovered =
        plan_for(&document, "H\u{6f22}", true).expect_err("no shipped face covers U+6F22");
    assert!(
        uncovered.starts_with("GLYPHS_NOT_COVERABLE"),
        "expected GLYPHS_NOT_COVERABLE, got {uncovered}"
    );

    assert_ne!(refused, uncovered, "the refusals must not collapse (R3)");
}

/// Property 1: a covered run keeps its own font, with substitution enabled.
///
/// `simple-winansi.pdf` sets a standard-14 font, whose coverage is total for WinAnsi. A
/// bundled face in the tree must not make `plan` prefer to rewrite a run that already works.
#[test]
fn keep_original_is_decided_before_any_substitution() {
    let document = fixture("simple-winansi.pdf");
    assert_eq!(
        plan_for(&document, "EDITED", true),
        Ok(Plan::KeepOriginal),
        "a run the original font covers must keep it, even though a bundled face could also \
         set the text"
    );
}

/// Property 1, stronger: keep-original does not depend on the toggle.
///
/// The toggle governs *substitution*, not *keeping*. An implementation that reads the toggle
/// first would refuse a run it could have edited.
#[test]
fn keep_original_does_not_depend_on_the_substitution_toggle() {
    let document = fixture("simple-winansi.pdf");
    for allow in [true, false] {
        assert_eq!(
            plan_for(&document, "EDITED", allow),
            Ok(Plan::KeepOriginal),
            "allow={allow}"
        );
    }
}

/// The order must be observable in the source, not only in today's outcomes.
///
/// Behavioural tests pin the cases they build; a later refactor can keep them green while
/// calling an embed path earlier for a case these tests do not construct. The coverage check
/// is the first thing that decides anything in `plan_with_font`, and it must stay ahead of
/// the substitution toggle and of any embed selection.
#[test]
fn the_plan_function_checks_coverage_before_anything_else() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/editor/substitute.rs");
    let source = std::fs::read_to_string(&path).expect("cannot read substitute.rs");

    let body = source
        .split("pub fn plan_with_font(")
        .nth(1)
        .expect("substitute.rs no longer defines plan_with_font");
    let body = body
        .split("\n}\n")
        .next()
        .expect("unterminated plan_with_font");

    let coverage = body
        .find("original_covers(")
        .expect("plan_with_font must consult original_covers");
    // The *decision*, not the parameter. `allow_substitution` also appears in the signature,
    // above the coverage check, so searching for the bare name measured the parameter list
    // and failed on a correct function -- the same "satisfied by the wrong occurrence" shape
    // AGENTS.md warns about.
    let toggle = body
        .find("if !allow_substitution")
        .expect("plan_with_font must branch on the substitution toggle");

    assert!(
        coverage < toggle,
        "T11/R4: coverage must be checked before the substitution toggle; a run the original \
         can set is kept regardless of the toggle"
    );

    // `Plan::Embed` specifically: a comment mentioning "embed" is not a selection site, and
    // matching the bare word would both miss a renamed variant and fire on prose.
    if let Some(embed) = body.find("Plan::Embed") {
        assert!(
            coverage < embed,
            "R4: the embed path is selected before coverage is checked, so a bundle could \
             rewrite a run whose own font already covers the replacement"
        );
    }
}
