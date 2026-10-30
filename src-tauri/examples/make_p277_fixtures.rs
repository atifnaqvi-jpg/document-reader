//! Generates the P2.7.7 §5 fixture: `type0-shared-font-multi-run.pdf`.
//!
//! Run via `scripts/make-p277-fixtures.sh` (or `cargo run --example make_p277_fixtures
//! --features testfixtures`). The PDF is committed, so the row-6 gate is a file a reviewer can
//! open, and the tests never depend on a generator having run first.
//!
//! The subset is built here with `embed::subset` — the same function a real producer uses —
//! rather than inside `testpdf`. `testpdf` is `#[path]`-included by this example and by
//! `make_p24_fixtures`, so it may not reach back into the library; keeping the subset build on
//! this side is what lets one module serve both generators.
//!
//! **Determinism.** The document is built twice and compared before it is saved. Row 6 compares
//! rendered pixels byte-exactly, so a fixture that differed run to run would make the gate
//! flaky for a reason unrelated to the code under test. Nothing here is encrypted (no AES salt,
//! no `/Encrypt`) and lopdf writes no timestamp, so two runs must agree — asserted, not assumed.

use document_reader_lib::editor::testpdf;

fn main() {
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");

    let first = build();
    let second = build();
    assert_eq!(
        first, second,
        "the fixture is not deterministic: two builds produced different bytes, so the \
         byte-exact pixel comparison in row 6 would fail for a reason that has nothing to do \
         with the code under test"
    );

    let path = dir.join("type0-shared-font-multi-run.pdf");
    std::fs::write(&path, &first).unwrap();
    println!("wrote {} ({} bytes)", path.display(), first.len());
}

fn build() -> Vec<u8> {
    let face = document_reader_lib::editor::embed::face_for_style("Helvetica")
        .expect("a bundled Sans face");
    let subset = document_reader_lib::editor::embed::subset(face, "ABCab")
        .expect("the bundled face subsets");
    testpdf::bytes(&testpdf::type0_page_with_subset(
        &subset.program,
        &subset.base_font,
        subset.bbox,
        subset.ascent,
        subset.descent,
        subset.cap_height,
    ))
}
