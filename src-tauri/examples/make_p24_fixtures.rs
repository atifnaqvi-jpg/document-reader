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
        // P2.4.1 §5: the fragmented-run matrix.
        (
            "fragmented-tj.pdf",
            testpdf::bytes(&testpdf::fragmented_tj()),
        ),
        (
            "fragmented-mixed-fonts.pdf",
            testpdf::bytes(&testpdf::fragmented_mixed_fonts()),
        ),
        (
            "fragmented-unpairable.pdf",
            testpdf::bytes(&testpdf::fragmented_unpairable()),
        ),
        (
            "fragmented-wider-merged.pdf",
            testpdf::bytes(&testpdf::fragmented_wider_merged()),
        ),
        // P2.4.2 §3: chain-break + space-kern + wider-replacement.
        (
            "chain-break-orphan.pdf",
            testpdf::bytes(&testpdf::chain_break_orphan()),
        ),
        (
            "space-kern-gap.pdf",
            testpdf::bytes(&testpdf::space_kern_gap()),
        ),
        (
            "wider-replacement-orphan.pdf",
            testpdf::bytes(&testpdf::wider_replacement_orphan()),
        ),
        // P2.4.3 §3: the span oracle's two fixtures (a font with no in-file metrics).
        (
            "unmeasurable-font.pdf",
            testpdf::bytes(&testpdf::unmeasurable_font_page()),
        ),
        (
            "unmeasurable-chain-break.pdf",
            testpdf::bytes(&testpdf::unmeasurable_chain_break()),
        ),
        // P2.4.4 §3: T24's grid-walk regression + generated-char alignment.
        ("tj-grid-walk.pdf", testpdf::bytes(&testpdf::tj_grid_walk())),
        ("tj-kern-seam.pdf", testpdf::bytes(&testpdf::tj_kern_seam())),
        // P2.6 T26: two operands, one baseline, one style -- the selection-range shape.
        ("two-run-line.pdf", testpdf::bytes(&testpdf::two_run_line())),
        // P2.7.1: tagged structure as a merge boundary, and the ActualText shapes a write
        // must (and must not) rewrite.
        (
            "mcid-boundaries.pdf",
            testpdf::bytes(&testpdf::mcid_boundaries()),
        ),
        (
            "actualtext-span.pdf",
            testpdf::bytes(&testpdf::actualtext_span()),
        ),
        (
            "actualtext-shared-inline.pdf",
            testpdf::bytes(&testpdf::actualtext_shared_inline()),
        ),
        (
            "actualtext-pdfdocencoding.pdf",
            testpdf::bytes(&testpdf::actualtext_pdf_doc_encoding()),
        ),
        (
            "actualtext-undecodable.pdf",
            testpdf::bytes(&testpdf::actualtext_undecodable()),
        ),
        (
            "actualtext-named-property.pdf",
            testpdf::bytes(&testpdf::actualtext_named_property()),
        ),
        (
            "named-property-shared-actualtext.pdf",
            testpdf::bytes(&testpdf::named_property_shared_actualtext()),
        ),
        (
            "named-property-no-actualtext.pdf",
            testpdf::bytes(&testpdf::named_property_without_actualtext()),
        ),
        (
            "char-spacing-tj.pdf",
            testpdf::bytes(&testpdf::char_spacing_tj()),
        ),
        // P2.8 §6.2: A10's fixture, and the first committed artefact that exercises the pen
        // walk's kern branch. A `TJ` number between two strings had never appeared in a fixture
        // before, so the branch that reads it was untested.
        ("kerned-line.pdf", testpdf::bytes(&testpdf::kerned_line())),
        // P2.8 R8b: two tagged units on one baseline, so a scope-blind `line_origin` is
        // falsifiable on a committed file rather than only on synthesized runs.
        (
            "two-mcid-one-baseline.pdf",
            testpdf::bytes(&testpdf::two_mcid_one_baseline()),
        ),
        // The same page at `Tz 50`. At `Tz 100` the kern term's missing `Tz/100` factor is
        // invisible, so without this file the defect is unfalsifiable.
        (
            "kerned-line-tz.pdf",
            testpdf::bytes(&testpdf::kerned_line_tz()),
        ),
        // P2.7.1 T-3b: a page's `/Resources` is inherited from the `/Pages` node, the form
        // the P2.7.1 fixtures never exercised.
        (
            "indirect-resources-inherited.pdf",
            testpdf::bytes(&testpdf::indirect_resources_inherited()),
        ),
        // P2.7.3 §8: the Form XObject shapes. Committed so the phase's fixture matrix is
        // reviewable and `tests/xobject_fixtures.rs` can read the artefacts a reviewer opens.
        (
            "xobject-single.pdf",
            testpdf::bytes(&testpdf::xobject_single()),
        ),
        // P2.8 R5: page text and form text on one baseline. Without it R5 is reachable only
        // through runs synthesized in a test, so no committed file proves the gate can fire on
        // real content.
        (
            "page-and-form-one-line.pdf",
            testpdf::bytes(&testpdf::page_and_form_one_line()),
        ),
        (
            "xobject-two-draws.pdf",
            testpdf::bytes(&testpdf::xobject_two_draws()),
        ),
        (
            "xobject-two-pages.pdf",
            testpdf::bytes(&testpdf::xobject_two_pages()),
        ),
        (
            "xobject-nested.pdf",
            testpdf::bytes(&testpdf::xobject_nested()),
        ),
        (
            "xobject-cycle.pdf",
            testpdf::bytes(&testpdf::xobject_cycle()),
        ),
        (
            "xobject-depth-9.pdf",
            testpdf::bytes(&testpdf::xobject_depth_nine()),
        ),
        (
            "xobject-interleaved.pdf",
            testpdf::bytes(&testpdf::xobject_interleaved()),
        ),
        (
            "xobject-shared-resources.pdf",
            testpdf::bytes(&testpdf::xobject_shared_resources()),
        ),
        (
            "xobject-subset-font.pdf",
            testpdf::bytes(&testpdf::xobject_subset_font()),
        ),
        (
            "xobject-subset-font-siblings.pdf",
            testpdf::bytes(&testpdf::xobject_subset_font_siblings()),
        ),
    ];

    for (name, bytes) in fixtures {
        let path = dir.join(name);
        std::fs::write(&path, &bytes).unwrap();
        println!("wrote {} ({} bytes)", path.display(), bytes.len());
    }
}
