//! Generates the P3.3 §7 W1 form fixture matrix into `tests/fixtures/`.
//!
//! Run via `scripts/make-p33-fixtures.sh`, or `cargo run --example make_p33_fixtures
//! --features testfixtures`. The PDFs are committed, so the gates are files a reviewer can open
//! and no test depends on a generator having run first.
//!
//! The builders live in `testpdf` and are shared with the unit tests rather than copied: two
//! generators that drift would let a committed fixture and an in-memory test validate different
//! documents under the same gate name.
//!
//! **Determinism is asserted, not assumed.** W4's `the_checkbox_ticks_in_a_real_viewer` compares
//! rendered pixels of the widget rectangle, so a fixture that differed run to run would make that
//! gate flaky for a reason unrelated to the code under test. Every document here is built twice and
//! the two byte strings compared before anything is written.

use document_reader_lib::editor::testpdf;
use lopdf::Document;

fn main() {
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");

    let fixtures: Vec<(&str, fn() -> Document)> = vec![
        ("form-text-fields.pdf", testpdf::form_text_fields),
        ("form-checkbox-yes.pdf", testpdf::form_checkbox_yes),
        ("form-checkbox-on.pdf", testpdf::form_checkbox_on),
        (
            "form-checkbox-lowercase-yes.pdf",
            testpdf::form_checkbox_lowercase_yes,
        ),
        ("form-radio-group.pdf", testpdf::form_radio_group),
        ("form-choice-fields.pdf", testpdf::form_choice_fields),
        ("form-read-only.pdf", testpdf::form_read_only_field),
        ("form-xfa.pdf", testpdf::form_with_xfa),
    ];

    for (name, build) in fixtures {
        let first = testpdf::bytes(&build());
        let second = testpdf::bytes(&build());
        assert_eq!(
            first, second,
            "{name} is not deterministic: two builds produced different bytes, so a pixel \
             comparison against it would fail for a reason that has nothing to do with the code \
             under test"
        );

        let path = dir.join(name);
        std::fs::write(&path, &first).unwrap();
        println!("wrote {} ({} bytes)", path.display(), first.len());
    }
}
