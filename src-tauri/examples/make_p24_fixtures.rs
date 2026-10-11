//! Generates the P2.4 §6 fixture matrix into `tests/fixtures/`.
//!
//! Run once via `scripts/make-p24-fixtures.sh`; the resulting PDFs are committed so the
//! gate files are reviewable in a diff and tests never depend on a generator running
//! first. Every fixture is built with lopdf's **public API only** — no new dependency,
//! and the same API the product code uses, so a fixture that lopdf cannot express is a
//! real signal about what Stage 1 can express.
//!
//! The builders are shared with the unit tests through `src/editor/testpdf.rs` rather
//! than copied: two generators that drift would let a committed fixture and an in-memory
//! test validate different documents under the same gate name.

#[path = "../src/editor/testpdf.rs"]
mod testpdf;

use std::path::PathBuf;

fn main() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");

    let fixtures: Vec<(&str, Vec<u8>)> = vec![
        (
            "simple-winansi.pdf",
            testpdf::bytes(&testpdf::simple_winansi()),
        ),
        (
            "simple-embedded-subset.pdf",
            testpdf::bytes(&testpdf::subset_font_page()),
        ),
        (
            "cid-identity.pdf",
            testpdf::bytes(&testpdf::cid_identity_page()),
        ),
        (
            "roundtrip-fail.pdf",
            testpdf::bytes(&testpdf::roundtrip_fail_page()),
        ),
        ("too-wide.pdf", testpdf::bytes(&testpdf::too_wide_page())),
        (
            "encrypted-aes.pdf",
            testpdf::bytes(&testpdf::encrypted_aes()),
        ),
        (
            "multi-stream-page.pdf",
            testpdf::bytes(&testpdf::multi_stream()),
        ),
        ("bomb.pdf", testpdf::bytes(&testpdf::bomb())),
        // Stage 2 §6. The first two carry a real embedded program whose glyph set is
        // exact, so T11's coverage check is decided by the font, not by a name convention.
        (
            "subset-winansi.pdf",
            testpdf::bytes(&testpdf::embedded_subset_page(
                "SUBSET",
                &"SUBET".chars().collect::<Vec<_>>(),
            )),
        ),
        (
            "subset-winansi-escape.pdf",
            testpdf::bytes(&testpdf::embedded_subset_page(
                "WIND",
                &"WIND".chars().collect::<Vec<_>>(),
            )),
        ),
        (
            "subset-wider-after-substitute.pdf",
            testpdf::bytes(&testpdf::embedded_subset_page(
                "HI",
                &"HI".chars().collect::<Vec<_>>(),
            )),
        ),
        (
            "non-winansi-replacement.pdf",
            testpdf::bytes(&testpdf::embedded_subset_page(
                "HI",
                &"HI".chars().collect::<Vec<_>>(),
            )),
        ),
        (
            "style-match-cases.pdf",
            testpdf::bytes(&testpdf::style_match_cases()),
        ),
    ];

    for (name, bytes) in fixtures {
        let path = dir.join(name);
        std::fs::write(&path, &bytes).unwrap();
        println!("wrote {} ({} bytes)", path.display(), bytes.len());
    }
}
