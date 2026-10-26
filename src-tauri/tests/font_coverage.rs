//! What the shipped faces actually contain, measured with the same parser the embed path
//! will use.
//!
//! This file exists because a test in `embed_plan_order.rs` claimed U+2192 was uncovered by
//! any shipped face. It is covered by **all twelve** styles, and the claim went unnoticed
//! because MECH-B does not exist yet: the plan refuses every non-WinAnsi replacement, so
//! "uncovered" and "covered but unimplemented" produced the same code. The test was green and
//! wrong, and would have inverted its meaning the moment the embed path landed.
//!
//! The lesson is not "be careful with code points"; it is that a fact about shipped data has
//! to be measured against that data, in a test, or it is an assumption wearing a test's
//! clothes. Every coverage claim in the suite can now cite this file.

use std::path::Path;

use ttf_parser::{Face, GlyphId};

fn fonts_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("resources/fonts")
}

/// The twelve shipped faces, named so a missing file is a failure rather than a silent skip.
const STYLES: [&str; 12] = [
    "LiberationSans-Regular",
    "LiberationSans-Bold",
    "LiberationSans-Italic",
    "LiberationSans-BoldItalic",
    "LiberationSerif-Regular",
    "LiberationSerif-Bold",
    "LiberationSerif-Italic",
    "LiberationSerif-BoldItalic",
    "LiberationMono-Regular",
    "LiberationMono-Bold",
    "LiberationMono-Italic",
    "LiberationMono-BoldItalic",
];

fn has_glyph(style: &str, ch: char) -> bool {
    let path = fonts_dir().join(format!("{style}.ttf"));
    let data =
        std::fs::read(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    let face = Face::parse(&data, 0).unwrap_or_else(|e| panic!("cannot parse {style}: {e}"));
    // `None` is *absence*, and absence must read as `false`. The first version of this
    // function was `face.glyph_index(ch) != Some(GlyphId(0))`, which answers `true` for an
    // unmapped character -- so every one of the five tests passed or failed for the wrong
    // reason. It is the same absence-of-evidence mistake the file exists to prevent, and it
    // was caught only by the one test that expected `false`.
    face.glyph_index(ch)
        .is_some_and(|glyph| glyph != GlyphId(0))
}

fn any_face_has(ch: char) -> bool {
    STYLES.iter().any(|style| has_glyph(style, ch))
}

fn all_faces_have(ch: char) -> bool {
    STYLES.iter().all(|style| has_glyph(style, ch))
}

/// The character `embed_plan_order.rs` uses for its covered case must be covered.
///
/// U+2192 is the design's own example. If a future bundle drops it or swaps the faces, this
/// fails here rather than silently turning a "covered" test into an "uncovered" one.
#[test]
fn an_arrow_is_covered_by_every_shipped_style() {
    assert!(
        all_faces_have('\u{2192}'),
        "U+2192 must be in every shipped style; a partial subset of the bundle would make the \
         embed path style-dependent in a way the plan order does not express"
    );
}

/// U+0100 (Latin capital A with macron) is the alternative covered character.
///
/// It matters because it is absent from WinAnsi, so it cannot take MECH-A: it is the cheapest
/// possible probe of the embed path, needing one glyph and no layout.
#[test]
fn a_latin_extended_letter_is_covered_and_outside_winansi() {
    assert!(all_faces_have('\u{0100}'));
    assert!(
        document_reader_lib::editor::afm::winansi_encode("\u{0100}").is_none(),
        "U+0100 must be outside WinAnsi, or it would take MECH-A and prove nothing about \
         embedding"
    );
}

/// U+2014 is **inside** WinAnsi, so it is not a non-WinAnsi probe.
///
/// The design's §2.1 example used U+2014 for the embedded case. Measured: CP1252 code 0x97 is
/// an em dash, so MECH-A sets it and no subset is written. Asserting this stops the mistake
/// recurring, and explains why `embed_acceptance.rs` row 1 does not use it.
#[test]
fn an_em_dash_is_winansi_and_cannot_prove_embedding() {
    assert!(
        document_reader_lib::editor::afm::winansi_encode("\u{2014}").is_some(),
        "U+2014 is CP1252 0x97; if this ever fails, the WinAnsi table changed and the row-1 \
         character has to be re-chosen"
    );
}

/// `漢` must be in **no** face: it is the permanent-coverage case (design §5 row 3).
#[test]
fn cjk_is_covered_by_no_shipped_face() {
    assert!(
        !any_face_has('\u{6f22}'),
        "the bundle is Latin-only; if 漢 ever ships, the coverage-gap tests need a new \
         representative character"
    );
}

/// A glyph inside WinAnsi is covered by the standard-14 path without any bundle at all.
#[test]
fn ascii_needs_no_bundle() {
    assert!(document_reader_lib::editor::afm::winansi_encode("EDIT").is_some());
}
