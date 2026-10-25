//! P2.4 §6 gate matrix, against the committed fixtures.
//!
//! Every gate in the frozen design has an assertion here, and each one reads the fixture
//! **from disk** rather than rebuilding it in memory — the committed file is the artefact
//! a reviewer can open, and the unit tests in `editor::textedit` / `editor::textlocate`
//! already cover the in-memory shapes. Between them no gate is left untested.
//!
//! Gate → test:
//!
//! | Gate (P2.4 §3) | Fixture | Test |
//! |----------------|---------|------|
//! | happy path | `simple-winansi.pdf` | `simple_winansi_edits_and_saves_a_new_file` |
//! | (a) simple font | `cid-identity.pdf` | `cid_identity_is_refused_by_gate_a` |
//! | (b) round-trip | `roundtrip-fail.pdf` | `roundtrip_fail_is_refused_by_gate_b` |
//! | (d) width | `too-wide.pdf` | `too_wide_is_refused_by_gate_d_with_both_widths` |
//! | (e) / F3 cap | `bomb.pdf` | `bomb_is_refused_as_content_too_large` |
//! | F2 collapse | `multi-stream-page.pdf` | `a_multi_stream_page_edits_and_stays_renderable` |
//! | T7 read-only | `encrypted-aes.pdf` | `encrypted_documents_refuse_every_edit` |
//! | T8 decrypt copy | `encrypted-aes.pdf` | `decrypt_copy_writes_an_unprotected_file` |
//!
//! Stage 2 (§3 turns `font-subset` into MECH-A substitution, T10–T13):
//!
//! | Invariant | Fixture | Test |
//! |-----------|---------|------|
//! | T11 keep-original | `subset-winansi.pdf` | `subset_winansi_keeps_the_original_font_when_it_covers` |
//! | MECH-A fires | `subset-winansi-escape.pdf` | `subset_winansi_escape_substitutes_when_the_glyph_is_missing` |
//! | MECH-A + gate d | `subset-wider-after-substitute.pdf` | `wider_after_substitute_refuses_with_substitute_metrics` |
//! | T13 substitute metrics | `subset-winansi-escape.pdf` | `subset_font_is_substituted_by_stage_two` |
//! | O-ST2-4 off | `subset-winansi-escape.pdf` | `subset_font_is_refused_when_substitution_is_off` |
//! | GLYPHS_NOT_COVERABLE | `non-winansi-replacement.pdf` | `non_winansi_replacement_is_not_coverable` |
//! | O-ST2-6 style-match | `style-match-cases.pdf` | `style_match_cases_map_to_the_documented_fonts` |
//! | R5/T16 re-index | `subset-winansi-escape.pdf` | `a_substitution_edit_re_indexes_the_copy_so_search_finds_the_new_text` |
//!
//! (The Stage-1 `simple-embedded-subset.pdf` gate-(c) refusal became
//! `subset_font_is_substituted_by_stage_two` once §3 replaced that gate with MECH-A.)

use std::path::{Path, PathBuf};

use document_reader_lib::editor::geometry::PdfRect;
use document_reader_lib::editor::protect;
use document_reader_lib::editor::runmetrics::{self, PdfiumRun};
use document_reader_lib::editor::textedit;
use document_reader_lib::editor::textlocate::{self, RunId};
use document_reader_lib::model::AppError;
use document_reader_lib::provider::PdfProvider;

mod support;

use document_reader_lib::db::{search, Db};
use document_reader_lib::index::{app as index_app, IndexService};
use document_reader_lib::library;
use document_reader_lib::render::RenderService;
use std::sync::Arc;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

fn load(path: &Path) -> lopdf::Document {
    lopdf::Document::load(path).unwrap_or_else(|e| panic!("could not load {}: {e}", path.display()))
}

/// True when PDFium is present, so the metric-dependent gates can be skipped rather than
/// failing on a machine without the native library. CI asserts the skip count is zero on
/// Windows; locally (aarch64 Linux) the arm64 build is loaded via `PDFIUM_LIBRARY_PATH`.
fn pdfium_ready() -> bool {
    if support::pdfium_available() {
        return true;
    }
    eprintln!("skipping: PDFium not available on this machine");
    false
}

fn pdfium_runs(path: &Path, page: u32) -> Vec<PdfiumRun> {
    runmetrics::extract_runs(path, page, None).expect("PDFium run extraction failed")
}

/// PDFium's whole-page character grid, the span oracle (P2.4.3 T23a), read the same way the
/// command does: through a handle cache, read-only.
fn char_grid(path: &Path, page: u32) -> runmetrics::PdfiumCharGrid {
    document_reader_lib::provider::DocumentHandles::new()
        .with_document(path, None, |document| {
            runmetrics::char_grid(document, page, path)
        })
        .expect("PDFium char grid extraction failed")
}

/// Applies an edit with a fixed measurement, so a gate test does not depend on PDFium
/// metrics for the gates it is not testing.
fn edit_with(
    document: &mut lopdf::Document,
    page: u32,
    run_id: RunId,
    expected: &str,
    replacement: &str,
    old_width: f32,
    new_width: f32,
) -> Result<textedit::EditOutcome, AppError> {
    edit_with_options(
        document,
        page,
        run_id,
        expected,
        replacement,
        old_width,
        new_width,
        true,
    )
}

/// The same, with the O-ST2-4 toggle and font size exposed for the substitution gates.
#[allow(clippy::too_many_arguments)]
fn edit_with_options(
    document: &mut lopdf::Document,
    page: u32,
    run_id: RunId,
    expected: &str,
    replacement: &str,
    old_width: f32,
    new_width: f32,
    allow_substitution: bool,
) -> Result<textedit::EditOutcome, AppError> {
    let measure = move |_text: &str| Ok(new_width);
    let request = textedit::EditRequest {
        page,
        run_id,
        expected: expected.to_string(),
        replacement: replacement.to_string(),
        old_width,
        font_size: 24.0,
        allow_substitution,
        fragments: Vec::new(),
        range: Vec::new(),
        measure: &measure,
    };
    textedit::edit(document, &request)
}

/// Applies an edit with a caller-chosen font size, so a width-gate test whose fixture is not
/// at 24pt measures the replacement at the size the run actually is (P2.4.2 T22).
#[allow(clippy::too_many_arguments)]
fn edit_with_size(
    document: &mut lopdf::Document,
    page: u32,
    run_id: RunId,
    expected: &str,
    replacement: &str,
    old_width: f32,
    new_width: f32,
    font_size: f32,
) -> Result<textedit::EditOutcome, AppError> {
    let measure = move |_text: &str| Ok(new_width);
    let request = textedit::EditRequest {
        page,
        run_id,
        expected: expected.to_string(),
        replacement: replacement.to_string(),
        old_width,
        font_size,
        allow_substitution: true,
        fragments: Vec::new(),
        range: Vec::new(),
        measure: &measure,
    };
    textedit::edit(document, &request)
}

fn first_run(document: &lopdf::Document, page: u32) -> textlocate::RawRun {
    let page_id = textlocate::page_id(document, page).unwrap();
    textlocate::locate_runs(document, page_id).unwrap()[0].clone()
}

/// Measures a replacement the way the write path does, using the fixture fonts' real in-file
/// metrics (P2.4.2 T22/O1): the fixtures' fonts are `Helvetica`/subset programs, so the AFM
/// table answers exactly. This keeps a width-gate test independent of a hand-picked constant.
fn measured_width(text: &str, font_size: f32) -> f32 {
    document_reader_lib::editor::afm::text_width("Helvetica", text, font_size)
        .expect("Helvetica is a standard-14 font with an AFM table")
}

// --- happy path ----------------------------------------------------------------

#[test]
fn simple_winansi_edits_and_saves_a_new_file() {
    let mut document = load(&fixture("simple-winansi.pdf"));
    let run = first_run(&document, 0);
    assert_eq!(run.text, "ALPHA");

    let outcome = edit_with(
        &mut document,
        0,
        run.id.clone(),
        "ALPHA",
        "BRAVO",
        38.7,
        39.0,
    )
    .expect("the best case must be editable");

    assert_eq!(outcome.inverse.old_bytes, b"ALPHA");
    assert_eq!(outcome.inverse.new_bytes, b"BRAVO");
    // A standard-14 original keeps its own font: no substitution, no disclosure (T11/T10).
    assert!(outcome.substitution.is_none());
    assert!(outcome.note.is_none());

    // The written bytes re-read as the edit, and the original file is untouched on disk.
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("edited.pdf");
    document.save(&target).unwrap();
    let reread = textlocate::locate_runs(
        &load(&target),
        textlocate::page_id(&load(&target), 0).unwrap(),
    )
    .unwrap();
    assert_eq!(reread[0].text, "BRAVO");

    let original = load(&fixture("simple-winansi.pdf"));
    let original_run = first_run(&original, 0);
    assert_eq!(
        original_run.text, "ALPHA",
        "the fixture on disk was not modified"
    );
}

// --- gate (a) ------------------------------------------------------------------

#[test]
fn cid_identity_without_tounicode_is_refused_by_gate_a() {
    let mut document = load(&fixture("cid-identity.pdf"));
    let run = first_run(&document, 0);

    let error = edit_with(&mut document, 0, run.id, "CID", "TEXT", 100.0, 10.0)
        .expect_err("a CID font with no /ToUnicode must be refused");
    assert_eq!(error.code(), "TEXT_RUN_NOT_EDITABLE");
    assert!(error.to_string().contains("font-encoding"), "{error}");
}

// --- gate (b) ------------------------------------------------------------------

#[test]
fn roundtrip_fail_is_refused_by_gate_b() {
    let document = load(&fixture("roundtrip-fail.pdf"));
    let run = first_run(&document, 0);

    let gates = textedit::resolve_gates(&document, 0, &run).unwrap();
    assert!(
        !gates.round_trips,
        "the fixture's encoding must not round-trip its own byte"
    );
    let verdict = textedit::font_admissibility(&gates);
    assert_eq!(verdict.gate.as_deref(), Some("encoding-round-trip"));

    let mut document = load(&fixture("roundtrip-fail.pdf"));
    let run = first_run(&document, 0);
    let error = edit_with(&mut document, 0, run.id, &run.text, "x", 100.0, 10.0)
        .expect_err("an unroundtrippable encoding must be refused");
    assert_eq!(error.code(), "TEXT_RUN_NOT_EDITABLE");
    assert!(error.to_string().contains("encoding-round-trip"), "{error}");
}

// --- gate (c) → Stage 2 substitution -------------------------------------------

/// Stage 1 refused a subset font outright. Stage 2 (§3) turns that refusal into MECH-A:
/// with substitution on (the default), a subset run is edited by switching it to a
/// standard-14 substitute, and the success path names the substitute (T10).
///
/// **Design-vs-reality note:** Stage 1's `subset_font_is_refused_by_gate_c` asserted a
/// `not-subset` refusal against `simple-embedded-subset.pdf`. That assertion is obsolete —
/// the design's §3 gate table explicitly says `font-subset` becomes "MECH-A substitute".
/// The fixture still has a subset-flagged name and no program, so it is not covered and
/// substitutes; the refusal it used to prove is now proved by `refuses_...when_off` below
/// (with the toggle off) and by `non_winansi_replacement_is_not_coverable`.
#[test]
fn subset_font_is_substituted_by_stage_two() {
    let mut document = load(&fixture("simple-embedded-subset.pdf"));
    let run = first_run(&document, 0);
    assert_eq!(run.text, "SUBSET");

    // old_width is generous so the width gate does not pre-empt the substitution path.
    let outcome = edit_with(&mut document, 0, run.id, "SUBSET", "EDIT", 100.0, 5.0)
        .expect("a subset font is editable in Stage 2");
    assert_eq!(outcome.substitution.as_deref(), Some("Helvetica"));
    assert!(
        outcome.note.as_deref().unwrap().contains("substituting"),
        "the note must disclose the substitution: {:?}",
        outcome.note
    );
    assert!(
        outcome.inverse.font_swap.is_some(),
        "the /Tf swap is recorded for undo"
    );
}

/// O-ST2-4: the toggle off restores Stage-1 behaviour for the same run.
#[test]
fn subset_font_is_refused_when_substitution_is_off() {
    let mut document = load(&fixture("simple-embedded-subset.pdf"));
    let run = first_run(&document, 0);

    let error = edit_with_options(
        &mut document,
        0,
        run.id,
        "SUBSET",
        "EDIT",
        100.0,
        5.0,
        false,
    )
    .expect_err("with substitution off, a subset must refuse");
    assert_eq!(error.code(), "SUBSTITUTE_REFUSED");
    assert!(
        error.to_string().contains("annotation tools to add a note"),
        "{error}"
    );
}

// --- T11: coverage keeps the original font -------------------------------------

/// The requirement's headline test: the original subset genuinely contains every glyph
/// the replacement needs, so the original font is kept, no substitution fires, and no
/// note is produced.
#[test]
fn subset_winansi_keeps_the_original_font_when_it_covers() {
    let mut document = load(&fixture("subset-winansi.pdf"));
    let run = first_run(&document, 0);
    assert_eq!(run.text, "SUBSET");

    // "SET" uses only glyphs the fixture's program maps.
    let outcome = edit_with(&mut document, 0, run.id, "SUBSET", "SET", 100.0, 5.0)
        .expect("a covered subset edit must succeed");
    assert!(
        outcome.substitution.is_none(),
        "no substitution when the font covers"
    );
    assert!(
        outcome.note.is_none(),
        "no substitution note when the font covers"
    );
    assert!(
        outcome.inverse.font_swap.is_none(),
        "the /Tf stays on the original font"
    );
    assert_eq!(outcome.inverse.new_bytes, b"SET");
}

/// T11's other half: the subset lacks a needed glyph, so MECH-A substitutes.
#[test]
fn subset_winansi_escape_substitutes_when_the_glyph_is_missing() {
    let mut document = load(&fixture("subset-winansi-escape.pdf"));
    let run = first_run(&document, 0);
    assert_eq!(run.text, "WIND");

    // "WINDOW" needs O, which the program does not map. old_width is generous so the
    // width gate (which now measures the substitute) does not pre-empt the substitution.
    let outcome = edit_with(&mut document, 0, run.id, "WIND", "WINDOW", 200.0, 5.0)
        .expect("an uncovered subset edit substitutes");
    assert_eq!(outcome.substitution.as_deref(), Some("Helvetica"));
    assert!(outcome.note.as_deref().unwrap().contains("substituting"));
    assert!(outcome.inverse.font_swap.is_some());

    // The swap really happened: reload and confirm the run's /Tf now names the substitute
    // and the text reads back as the replacement.
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("edited.pdf");
    document.save(&target).unwrap();
    let reread = load(&target);
    let after = first_run(&reread, 0);
    assert_eq!(after.text, "WINDOW");
    assert_eq!(after.font_key, b"FS1");
}

// --- T13: the width gate uses the FINAL font's metrics -------------------------

/// After a substitution the width gate compares the *substitute's* AFM width (T13), and a
/// refusal reports that measured width.
#[test]
fn wider_after_substitute_refuses_with_substitute_metrics() {
    let mut document = load(&fixture("subset-wider-after-substitute.pdf"));
    let run = first_run(&document, 0);
    assert_eq!(run.text, "HI");

    // Helvetica AFM width of "WWWWWWWW" at 24pt is far past a 5pt run; the measured
    // `new_width` supplied here is irrelevant because MECH-A overrides it with AFM.
    let error = edit_with(&mut document, 0, run.id, "HI", "WWWWWWWW", 5.0, 5.0)
        .expect_err("the substitute's metrics overflow the run");
    assert_eq!(error.code(), "TEXT_RUN_NOT_EDITABLE");
    let message = error.to_string();
    assert!(message.contains("fits-width"), "{message}");
    // 8 x Helvetica 'W' (944/1000 em) at 24pt = 181.2pt; the run has 5.0pt.
    assert!(
        message.contains("181.2"),
        "must quote the substitute's width: {message}"
    );
    assert!(
        message.contains("5.0"),
        "must quote the available width: {message}"
    );
    assert!(
        message.contains("annotation tools to add a note"),
        "{message}"
    );
}

// --- MECH-B absence: non-WinAnsi is not coverable -------------------------------

/// §6 / MECH-B: a replacement needing a glyph outside WinAnsi (an em-dash, a typographic
/// arrow) can be supplied by neither the subset nor any standard-14 substitute, so it
/// refuses `GLYPHS_NOT_COVERABLE` — the documented case for MECH-B.
#[test]
fn non_winansi_replacement_is_not_coverable() {
    let mut document = load(&fixture("non-winansi-replacement.pdf"));
    let run = first_run(&document, 0);
    assert_eq!(run.text, "HI");

    // U+2014 EM DASH and U+2192 RIGHTWARDS ARROW have no WinAnsi code.
    let error = edit_with(
        &mut document,
        0,
        run.id,
        "HI",
        "H\u{2014}\u{2192}",
        100.0,
        5.0,
    )
    .expect_err("a non-WinAnsi replacement cannot be covered");
    assert_eq!(error.code(), "GLYPHS_NOT_COVERABLE");
    assert!(
        error.to_string().contains("annotation tools to add a note"),
        "{error}"
    );
}

// --- style match (§6 case table) ------------------------------------------------

/// Every original-name page maps to the documented standard-14 family (O-ST2-6), with the
/// subset prefix stripped first.
#[test]
fn style_match_cases_map_to_the_documented_fonts() {
    use document_reader_lib::editor::substitute::style_match;

    let document = load(&fixture("style-match-cases.pdf"));
    let expected = [
        "Times-Roman",
        "Times-BoldItalic",
        "Courier",
        "Times-Roman",
        "Helvetica",
        "Helvetica-Bold",
        "Helvetica-Oblique",
    ];

    let page_ids: Vec<_> = (0..expected.len() as u32)
        .map(|page| textlocate::page_id(&document, page).unwrap())
        .collect();
    assert_eq!(page_ids.len(), expected.len(), "one page per case");

    for (page, want) in expected.iter().enumerate() {
        let page_id = page_ids[page];
        let run = &textlocate::locate_runs(&document, page_id).unwrap()[0];
        let info = textlocate::font_info(&document, page_id, &run.font_key).unwrap();
        assert_eq!(
            style_match(info.base_font.as_deref()),
            *want,
            "page {page} ({:?})",
            info.base_font
        );
    }
}

// --- gate (d) ------------------------------------------------------------------

#[test]
fn too_wide_is_refused_by_gate_d_with_both_widths() {
    let mut document = load(&fixture("too-wide.pdf"));
    let run = first_run(&document, 0);
    assert_eq!(run.text, "HI");

    // 38.7pt available, 41.3pt needed: the design's worked example (O1). The gate is
    // width-only here, so the two numbers are supplied rather than measured.
    let error = edit_with(&mut document, 0, run.id, "HI", "ALPHABETIC", 38.7, 41.3)
        .expect_err("an over-wide replacement must be refused");
    assert_eq!(error.code(), "TEXT_RUN_NOT_EDITABLE");
    let message = error.to_string();
    assert!(
        message.contains("41.3"),
        "must report needed width: {message}"
    );
    assert!(
        message.contains("38.7"),
        "must report available width: {message}"
    );
    assert!(message.contains("fits-width"), "{message}");
}

/// The PDFium-measured version: the widths the gate compares come from real metrics, not
/// constants, and the measured replacement really does overflow the narrow run.
#[test]
fn measured_widths_refuse_a_genuinely_wider_replacement() {
    if !pdfium_ready() {
        return;
    }
    let path = fixture("too-wide.pdf");
    let runs = pdfium_runs(&path, 0);
    let run = runs.iter().find(|run| run.text == "HI").expect("HI run");

    let wider = runmetrics::extract_runs(&path, 0, None)
        .unwrap()
        .into_iter()
        .find(|candidate| candidate.text == "HI")
        .unwrap();
    let replacement = "HI HI HI HI";
    let new_width = document_reader_lib::provider::DocumentHandles::new()
        .with_document(&path, None, |document| {
            runmetrics::measure(document, 0, &wider.font_name, wider.font_size, replacement)
        })
        .unwrap();

    let mut document = load(&path);
    let raw = first_run(&document, 0);
    let error = edit_with(
        &mut document,
        0,
        raw.id,
        "HI",
        replacement,
        run.width,
        new_width,
    )
    .expect_err("a measured over-wide replacement must be refused");
    let message = error.to_string();
    assert!(message.contains("fits-width"), "{message}");
    assert!(
        message.contains(&format!("{new_width:.1}"))
            && message.contains(&format!("{:.1}", run.width)),
        "both measured widths must appear: {message}"
    );
}

// --- gate (e) / F3 -------------------------------------------------------------

#[test]
fn bomb_is_refused_as_content_too_large() {
    let document = load(&fixture("bomb.pdf"));
    let page_id = textlocate::page_id(&document, 0).unwrap();
    let error = textlocate::locate_runs(&document, page_id)
        .expect_err("a decompression bomb must be refused, not allocated");
    assert_eq!(error.code(), "CONTENT_TOO_LARGE");
}

// --- F2: multi-stream page -----------------------------------------------------

#[test]
fn a_multi_stream_page_edits_and_stays_renderable() {
    if !pdfium_ready() {
        return;
    }
    let path = fixture("multi-stream-page.pdf");
    let mut document = load(&path);

    // Runs from both streams are located across the concatenation seam (F2).
    let page_id = textlocate::page_id(&document, 0).unwrap();
    let texts: Vec<String> = textlocate::locate_runs(&document, page_id)
        .unwrap()
        .into_iter()
        .map(|run| run.text)
        .collect();
    assert_eq!(texts, vec!["FIRST", "SECOND"]);

    let run = first_run(&document, 0);
    edit_with(&mut document, 0, run.id, "FIRST", "EDITED", 100.0, 40.0).unwrap();

    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("edited.pdf");
    document.save(&target).unwrap();

    // The rewritten page is one stream now, and it still renders: PDFium opening and
    // rasterizing it is what proves the collapse did not corrupt the page.
    assert_eq!(PdfProvider::page_count(&target).unwrap(), 1);
    let raster = PdfProvider::render_page_at(&target, 0, 1.0).unwrap();
    assert_eq!(&raster.bytes[..8], b"\x89PNG\r\n\x1a\n");
}

// --- T7: encrypted read-only ---------------------------------------------------

#[test]
fn encrypted_documents_refuse_every_edit() {
    let path = fixture("encrypted-aes.pdf");
    assert!(protect::is_encrypted(&path).unwrap());

    let error = protect::read_only_guard(&path).expect_err("a protected document must refuse");
    assert_eq!(error.code(), "ENCRYPTED_READ_ONLY");
}

// --- T8: decrypt a copy --------------------------------------------------------

#[test]
fn decrypt_copy_writes_an_unprotected_file() {
    // The page count below goes through PDFium, so this needs the same guard as its
    // siblings. It was written during the window when `ci.yml` did not trigger for this
    // branch, so its absence only surfaced when the gate was widened.
    if !pdfium_ready() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let original = dir.path().join("secret.pdf");
    let original_bytes = std::fs::read(fixture("encrypted-aes.pdf")).unwrap();
    std::fs::write(&original, &original_bytes).unwrap();

    let outcome = protect::decrypt_copy(&original, "secret").unwrap();

    let copy = Path::new(&outcome.path);
    assert!(!protect::is_encrypted(copy).unwrap());
    assert_eq!(PdfProvider::page_count(copy).unwrap(), 3);
    assert!(
        copy.file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .contains("(unprotected)"),
        "unexpected name: {outcome:?}"
    );

    // T8: the original is byte-identical.
    assert_eq!(std::fs::read(&original).unwrap(), original_bytes);

    // A wrong password is refused with its own code, never the generic one.
    let error = protect::decrypt_copy(&original, "nope").unwrap_err();
    assert_eq!(error.code(), "WRONG_PASSWORD");
}

// --- T4 + picker shape ---------------------------------------------------------

/// The picker's list pairs lopdf addresses with PDFium geometry, and precomputes the
/// verdict: an editable run has a rect and a width, and a refused run carries its gate.
#[test]
fn describe_runs_pairs_addresses_with_geometry_and_precomputes_gates() {
    if !pdfium_ready() {
        return;
    }
    let path = fixture("simple-winansi.pdf");
    let document = load(&path);
    let runs = textedit::describe_runs(&document, 0, &pdfium_runs(&path, 0), &char_grid(&path, 0))
        .unwrap();

    assert!(runs.len() >= 2);
    let alpha = runs.iter().find(|run| run.text == "ALPHA").expect("ALPHA");
    assert!(alpha.admissibility.editable);
    assert!(alpha.rect.is_some(), "an editable run needs a box to click");
    assert!(alpha.width.is_some_and(|width| width > 0.0));
    assert_eq!(alpha.font_key, "F1");

    // The CID fixture's single run is refused with its gate named, so the UI can grey it
    // out before the user types (P2.4 §4).
    let cid_path = fixture("cid-identity.pdf");
    let cid = load(&cid_path);
    let verdicts = textedit::describe_runs(
        &cid,
        0,
        &[],
        &runmetrics::PdfiumCharGrid {
            text: String::new(),
            chars: Vec::new(),
        },
    )
    .unwrap();
    assert_eq!(verdicts.len(), 1);
    assert!(!verdicts[0].admissibility.editable);
    assert_eq!(
        verdicts[0].admissibility.gate.as_deref(),
        Some("font-encoding")
    );
}

// --- P2.4.1 T17: geometric pairing on fragmented runs --------------------------

/// §5 `fragmented-tj.pdf`: a per-glyph `TJ` array must pair by **position**, not text.
///
/// This is the 0.4.0 defect in one assertion. lopdf addresses five operands; PDFium reports
/// one box ("Hello"). Under text-equality pairing none of the five matched and every one was
/// refused `run-mismatch` — the "only occasional runs editable" report. Under geometric pairing
/// each fragment's origin falls inside that box, and T18 then merges the five into ONE picker
/// entry with one width.
#[test]
fn fragmented_tj_fragments_pair_by_origin_not_text() {
    if !pdfium_ready() {
        return;
    }
    let path = fixture("fragmented-tj.pdf");
    let document = load(&path);
    let runs = textedit::describe_runs(&document, 0, &pdfium_runs(&path, 0), &char_grid(&path, 0))
        .unwrap();

    assert_eq!(runs.len(), 1, "the word is one entry, not five");
    let word = &runs[0];
    assert_eq!(word.text, "Hello", "the joined fragment text");
    assert_eq!(word.fragment_count, 5, "five fragments folded in");
    assert!(word.admissibility.editable);
    assert_eq!(word.pair_confidence, Some(textedit::PairConfidence::Strong));
    assert!(word.rect.is_some(), "the merged run has the union box");
    assert!(
        word.width.is_some_and(|width| width > 0.0),
        "the merged run carries the union box's width, the O1 old width"
    );

    // O-Pair-3: the second level lists the fragments, head first, each a single run.
    assert_eq!(word.fragments.len(), 5);
    let texts: Vec<&str> = word.fragments.iter().map(|f| f.text.as_str()).collect();
    assert_eq!(texts, vec!["H", "e", "l", "l", "o"]);
    for fragment in &word.fragments {
        assert_eq!(fragment.fragment_count, 1);
        assert!(fragment.fragments.is_empty(), "two levels only");
        assert_eq!(
            fragment.pair_confidence,
            Some(textedit::PairConfidence::Strong)
        );
    }

    // The merged run edits end-to-end in one atomic write (T18/T19).
    let mut document = load(&path);
    // The head fragment holds only "H"; the replacement is the whole new word, written into
    // the head and folded across the tail (T4 re-validates the head's own text).
    let tail: Vec<textedit::FragmentRef> = word.fragments[1..]
        .iter()
        .map(|fragment| textedit::FragmentRef {
            run_id: fragment.run_id.clone(),
            text: fragment.text.clone(),
        })
        .collect();
    let measure = |_text: &str| Ok(25.0_f32);
    let request = textedit::EditRequest {
        page: 0,
        run_id: word.fragments[0].run_id.clone(),
        expected: "H".to_string(),
        replacement: "Hello!".to_string(),
        old_width: 26.0,
        font_size: 12.0,
        allow_substitution: true,
        fragments: tail,
        measure: &measure,
        range: Vec::new(),
    };
    let outcome = textedit::edit(&mut document, &request).unwrap();
    assert_eq!(
        outcome.inverse.old_bytes, b"H",
        "the head fragment's original bytes"
    );
    assert_eq!(
        outcome.inverse.also.len(),
        4,
        "one recorded write per fragment"
    );
}

/// §5 `fragmented-mixed-fonts.pdf`: T19 — fragments never merge across a font or a size.
///
/// Two per-glyph runs on one baseline, "AB" at 12pt Helvetica and "CD" at 10pt Times, only
/// a point apart. A merger that ignored the font or the size would weld them into one run that
/// no single font could re-encode. They must stay two entries, each merged internally.
#[test]
fn fragments_never_merge_across_a_font_or_a_size() {
    if !pdfium_ready() {
        return;
    }
    let path = fixture("fragmented-mixed-fonts.pdf");
    let document = load(&path);
    let runs = textedit::describe_runs(&document, 0, &pdfium_runs(&path, 0), &char_grid(&path, 0))
        .unwrap();

    assert_eq!(
        runs.len(),
        2,
        "one entry per font/size run, never one welded run"
    );
    let ab = runs
        .iter()
        .find(|run| run.text == "AB")
        .expect("the 12pt run");
    let cd = runs
        .iter()
        .find(|run| run.text == "CD")
        .expect("the 10pt run");
    assert_eq!(ab.font_key, "F1");
    assert_eq!(cd.font_key, "F2");
    assert_ne!(ab.font_key, cd.font_key);
    assert_eq!(ab.fragment_count, 2, "its two glyphs merged internally");
    assert_eq!(cd.fragment_count, 2, "its two glyphs merged internally");
    // Distinct boxes: the merge did not reach across the seam.
    assert_ne!(ab.rect.map(|r| (r.x0, r.x1)), cd.rect.map(|r| (r.x0, r.x1)));
}

/// §5 `fragmented-wider-merged.pdf`: the width gate quotes the **merged** width (O1/§4).
///
/// The merged run is the union of its fragments' boxes, so the old width the refusal reports
/// is that union's advance — not any one glyph's — and a replacement longer than the union must
/// be refused with both numbers.
#[test]
fn merged_width_refusal_quotes_both_widths() {
    if !pdfium_ready() {
        return;
    }
    let path = fixture("fragmented-wider-merged.pdf");
    let document = load(&path);
    let runs = textedit::describe_runs(&document, 0, &pdfium_runs(&path, 0), &char_grid(&path, 0))
        .unwrap();
    assert_eq!(runs.len(), 1, "Hi merges into one entry");
    let word = &runs[0];
    assert_eq!(word.fragment_count, 2);

    // The union width is the merged box's advance, comfortably wider than one glyph's.
    let merged_width = word.width.expect("a paired merged run has a width");
    assert!(
        merged_width > 5.0,
        "the union box is wider than a single glyph: {merged_width}"
    );

    // A replacement measured wider than the union is refused, quoting the merged old width.
    let mut document = load(&path);
    let error = edit_with(
        &mut document,
        0,
        word.fragments[0].run_id.clone(),
        "H",
        "Hi there",
        merged_width,
        merged_width + 10.0,
    )
    .expect_err("an over-wide replacement must be refused");
    assert_eq!(error.code(), "TEXT_RUN_NOT_EDITABLE");
    let message = error.to_string();
    assert!(
        message.contains(&format!("{:.1}", merged_width + 10.0)),
        "{message}"
    );
    assert!(
        message.contains(&format!("{:.1}", merged_width)),
        "{message}"
    );
    assert!(message.contains("fits-width"), "{message}");
}

/// §5 `fragmented-tj.pdf`: a merged edit writes **one** atomic pass and one undo (T18/T20).
///
/// The head fragment takes the replacement, the four tail fragments are cleared, the `TJ`
/// numbers between them are left alone, and a single undo restores all five originals.
#[test]
fn a_merged_edit_is_one_atomic_write_and_one_undo() {
    if !pdfium_ready() {
        return;
    }
    let path = fixture("fragmented-tj.pdf");
    let described = {
        let document = load(&path);
        textedit::describe_runs(&document, 0, &pdfium_runs(&path, 0), &char_grid(&path, 0)).unwrap()
    };
    let word = &described[0];
    let tail: Vec<textedit::FragmentRef> = word.fragments[1..]
        .iter()
        .map(|fragment| textedit::FragmentRef {
            run_id: fragment.run_id.clone(),
            text: fragment.text.clone(),
        })
        .collect();

    let mut document = load(&path);
    // Within the merged union width, so the width gate is not what this test exercises.
    let measure = |_text: &str| Ok(10.0_f32);
    let request = textedit::EditRequest {
        page: 0,
        run_id: word.fragments[0].run_id.clone(),
        expected: "H".to_string(),
        replacement: "Hello!".to_string(),
        old_width: word.width.unwrap(),
        font_size: word.font_size,
        allow_substitution: true,
        fragments: tail,
        measure: &measure,
        range: Vec::new(),
    };
    let outcome = textedit::edit(&mut document, &request).unwrap();

    // ONE decode -> mutate -> encode pass: the head holds the whole replacement ...
    let page_id = textlocate::page_id(&document, 0).unwrap();
    let after = textlocate::locate_runs(&document, page_id).unwrap();
    assert_eq!(
        after[0].text, "Hello!",
        "the head fragment carries the whole word"
    );
    // ... and every tail fragment is empty, so the word reads once, not five times.
    for fragment in &after[1..] {
        assert_eq!(fragment.text, "", "tail fragments are cleared");
    }
    // One undo entry: the inverse records the head plus one write per tail fragment.
    assert_eq!(outcome.inverse.also.len(), 4);

    // The undo restores all five originals in one call.
    textedit::revert(&mut document, &outcome.inverse).unwrap();
    let restored = textlocate::locate_runs(&document, page_id).unwrap();
    let texts: Vec<&str> = restored.iter().map(|run| run.text.as_str()).collect();
    assert_eq!(
        texts,
        vec!["H", "e", "l", "l", "o"],
        "one undo restored every fragment"
    );
}

/// §5 `fragmented-unpairable.pdf`: T21 — the refusal names the missing box.
#[test]
fn an_unpaired_run_refuses_naming_the_missing_box() {
    if !pdfium_ready() {
        return;
    }
    let path = fixture("fragmented-unpairable.pdf");
    let document = load(&path);
    let runs = textedit::describe_runs(&document, 0, &pdfium_runs(&path, 0), &char_grid(&path, 0))
        .unwrap();

    // The normal run merged and paired; the size-0 run did not.
    let paired = runs
        .iter()
        .find(|run| run.text == "GHOST")
        .expect("the GHOST run");
    assert_eq!(paired.fragment_count, 5, "its five glyphs merged");
    assert!(paired.admissibility.editable);
    assert_eq!(
        paired.pair_confidence,
        Some(textedit::PairConfidence::Strong)
    );

    let unpaired = runs
        .iter()
        .find(|run| run.text == "ZEROSIZE")
        .expect("the size-0 run");
    assert!(!unpaired.admissibility.editable);
    assert!(unpaired.rect.is_none());
    assert!(
        unpaired.pair_confidence.is_none(),
        "no box, so no pair confidence"
    );
    assert_eq!(unpaired.admissibility.gate.as_deref(), Some("run-mismatch"));
    // T21: the message names the *reason*, not merely the gate.
    let message = unpaired
        .admissibility
        .message
        .as_deref()
        .unwrap_or_default();
    assert!(
        message.contains("no matching rendered box on the page"),
        "the refusal must name the reason: {message}"
    );
    assert!(
        message.contains("annotation tools to add a note"),
        "{message}"
    );
}

/// The picker's rect is in the same PDF-point space as annotations, so the overlay can
/// reuse the geometry converter rather than inventing a second coordinate path.
#[test]
fn a_run_rect_converts_through_the_shared_geometry() {
    if !pdfium_ready() {
        return;
    }
    let path = fixture("simple-winansi.pdf");
    let document = load(&path);
    let runs = textedit::describe_runs(&document, 0, &pdfium_runs(&path, 0), &char_grid(&path, 0))
        .unwrap();
    let run = runs.iter().find(|run| run.rect.is_some()).unwrap();
    let rect: PdfRect = run.rect.unwrap();
    assert!(rect.width() > 0.0 && rect.height() >= 0.0);

    // 792pt tall page: the conversion flips y so the overlay can draw in CSS space.
    let (_x, y, _w, _h) = document_reader_lib::editor::geometry::pdf_to_page(rect, 1.0, 792.0);
    assert!(
        (0.0..=792.0).contains(&y),
        "converted y out of page bounds: {y}"
    );
}

// --- P2.4.2 Phase A: exact in-file metrics for the merge chain ----------------

/// §3 `chain-break-orphan.pdf`: a tail fragment sitting exactly where the head's **in-file**
/// advances end must merge, where the 0.4.1 `0.5em` approximation would have severed it.
///
/// The embedded face's glyph advance is `1.0em`, twice the per-character approximation the old
/// code fell back to. Under the approximation the head `Sci` measured `1.5em` and the tail was a
/// `1.5em` gap away — a chain break. With the program's own `hmtx` metrics the gap is zero and the
/// line is one run (P2.4.2 Phase A).
#[test]
fn chain_break_orphan_merges_once_exact_metrics_run() {
    if !pdfium_ready() {
        return;
    }
    let path = fixture("chain-break-orphan.pdf");
    let document = load(&path);

    // The metrics really do come from the file: `Sci` at 12pt in a 1.0em face is 36pt, not the
    // 18pt the `0.5em` approximation would give.
    let page_id = textlocate::page_id(&document, 0).unwrap();
    let raw = textlocate::locate_runs(&document, page_id).unwrap();
    let head = raw
        .iter()
        .find(|run| run.text == "Sci")
        .expect("the Sci fragment");
    let exact = head
        .advance_exact
        .expect("an embedded font has in-file metrics");
    assert!(
        (exact - 36.0).abs() < 0.01,
        "Sci in a 1.0em face at 12pt is 36pt, got {exact}"
    );
    assert!(
        (head.advance - 36.0).abs() < 0.01,
        "the walk and the quota share the same exact metric"
    );

    let runs = textedit::describe_runs(&document, 0, &pdfium_runs(&path, 0), &char_grid(&path, 0))
        .unwrap();
    assert_eq!(runs.len(), 1, "the whole line is one entry, no orphan tail");
    let word = &runs[0];
    assert_eq!(word.text, "Science", "the joined line");
    assert_eq!(
        word.fragment_count, 2,
        "head and tail folded into one run, not split"
    );
    assert!(word.admissibility.editable, "the font gates pass");
    // P2.4.2 T22: here the two fragments genuinely tile the word box (36+48 = 84 ≈ box 72),
    // so the box is ≈ the span and the quota is well-formed — the run is not box-suspicious.
    assert!(!word.box_suspicious);
    assert!(
        word.width.is_some_and(|width| width > 0.0),
        "a measurable span is the quota: {:?}",
        word.width
    );
}

/// §3 `space-kern-gap.pdf`: an inter-word gap carried as kerning merges when, and only when,
/// PDFium's reconstructed text shows the space (P2.4.2 O-Chain-2).
#[test]
fn space_kern_gap_merges_a_kerned_word_break() {
    if !pdfium_ready() {
        return;
    }
    let path = fixture("space-kern-gap.pdf");
    let runs = pdfium_runs(&path, 0);
    // PDFium reconstructs the space the producer carried as kerning; without that signal the
    // 0.4em seam would be past the quarter-em bound and the words would stay apart.
    let rendered = runs
        .iter()
        .find(|run| run.text.contains(' '))
        .expect("PDFium must reconstruct the kerned space; the fixture depends on it (O-Chain-2)");
    assert!(
        rendered.text.contains("foo") && rendered.text.contains("bar"),
        "both words sit in the one box PDFium joins: {}",
        rendered.text
    );

    let document = load(&path);
    let described = textedit::describe_runs(&document, 0, &runs, &char_grid(&path, 0)).unwrap();
    assert_eq!(
        described.len(),
        1,
        "foo and bar merge across the kerned space"
    );
    // P2.6 T26 item 4: the space lived in the kern, not in a glyph, so the operands have no
    // `0x20` to join — but the page reads "foo bar", and the picker now says so. The separator
    // is display-only; `EditRequest::expected` stays the operands' bytes (`foobar`), which is
    // what the write path re-validates against the content stream.
    assert_eq!(described[0].text, "foo bar");
    assert_eq!(described[0].fragment_count, 2);
}

/// P2.6 T26 item 4: the whole point of the reading-order pass is that a kern seam the page
/// reads as a word space is still **editable**. The display text gains the separator; the
/// write path must accept the document's own bytes and replace them.
#[test]
fn a_kerned_word_break_is_editable_without_the_display_space() {
    if !pdfium_ready() {
        return;
    }
    let path = fixture("space-kern-gap.pdf");
    let document = load(&path);
    let runs = pdfium_runs(&path, 0);
    let described = textedit::describe_runs(&document, 0, &runs, &char_grid(&path, 0)).unwrap();
    let line = &described[0];
    assert_eq!(line.text, "foo bar");
    assert_eq!(line.fragment_count, 2);
    assert!(line.admissibility.editable, "the merged run is editable");

    let mut document = document;
    let measure = move |_text: &str| Ok(40.0);
    textedit::edit(
        &mut document,
        &textedit::EditRequest {
            page: 0,
            run_id: line.run_id.clone(),
            // The head *operand*, not the display text: `expected` is re-validated against the
            // content stream, where no space exists.
            expected: line.fragments[0].text.clone(),
            replacement: "baz qux".to_string(),
            old_width: line.width.unwrap_or(0.0),
            font_size: line.font_size,
            allow_substitution: true,
            fragments: line
                .fragments
                .iter()
                .skip(1)
                .map(|fragment| textedit::FragmentRef {
                    run_id: fragment.run_id.clone(),
                    text: fragment.text.clone(),
                })
                .collect(),
            range: Vec::new(),
            measure: &measure,
        },
    )
    .expect("the edit is admitted");

    let edited = std::env::temp_dir().join("kern-seam-edited.pdf");
    document.save(&edited).unwrap();
    let reread = textedit::describe_runs(
        &load(&edited),
        0,
        &pdfium_runs(&edited, 0),
        &char_grid(&edited, 0),
    )
    .unwrap();
    let text: String = reread.iter().map(|run| run.text.as_str()).collect();
    assert!(
        text.contains("baz qux"),
        "the replacement is in the page text: {text}"
    );
}

// --- P2.4.2 T22: the width quota is the run's own span, never a line box ----------

/// §3 `wider-replacement-orphan.pdf`: the user's exact 0.4.1 failure, memorialised.
///
/// The line `DataScien|ce` breaks between head and tail, so the tail `ce` is an orphan with no
/// box of its own — its only plausible box is the whole **line**. Under 0.4.1 the width gate took
/// that line box (~127pt) as the orphan's old width, so a ~110pt replacement passed a quota that
/// should have refused it and was written into the ~24pt tail operand alone.
///
/// T22 fixes it: the tail's quota is its **own** measured span (24pt), the pair is flagged
/// box-suspicious, and the same replacement now **hard-refuses** quoting both widths (O1).
#[test]
fn the_orphans_own_span_is_the_quota_and_a_wider_replacement_refuses() {
    if !pdfium_ready() {
        return;
    }
    let path = fixture("wider-replacement-orphan.pdf");
    let document = load(&path);
    let runs = textedit::describe_runs(&document, 0, &pdfium_runs(&path, 0), &char_grid(&path, 0))
        .unwrap();

    // The tail is its own entry, and its width is its own ~24pt span — never the ~127pt line.
    let tail = runs
        .iter()
        .find(|run| run.text == "ce")
        .expect("the orphan tail");
    let line_box = runs
        .iter()
        .find(|run| run.text == "DataScien")
        .and_then(|run| run.rect.map(|rect| rect.x1 - rect.x0))
        .expect("the head pairs to the line box");
    assert!(
        line_box > 120.0,
        "the fixture's box really is line-sized: {line_box}"
    );
    assert!(
        tail.width.is_some_and(|width| width < 30.0),
        "the orphan's quota is its own ~24pt span, not the line box: {:?}",
        tail.width
    );
    assert!(
        (tail.width.unwrap() - 24.0).abs() < 0.5,
        "the tail is 2 glyphs x 1.0em at 12pt = 24pt: {:?}",
        tail.width
    );
    assert!(
        tail.box_suspicious,
        "the pair is flagged box-suspicious (it claimed a line box)"
    );
    assert_eq!(
        tail.pair_confidence,
        Some(textedit::PairConfidence::Weak),
        "a line-sized box is a weak match"
    );

    // O1: the ~110pt replacement the user's report describes now hard-refuses, quoting both
    // measured widths — the quota and the replacement — and pointing at the annotation tool.
    let mut document = load(&path);
    let replacement = "Hello from this world";
    let new_width = measured_width(replacement, tail.font_size);
    assert!(
        new_width > 90.0,
        "the described replacement is genuinely line-wide: {new_width}"
    );
    let error = edit_with_size(
        &mut document,
        0,
        tail.run_id.clone(),
        "ce",
        replacement,
        tail.width.unwrap(),
        new_width,
        tail.font_size,
    )
    .expect_err("writing a line-wide replacement into a 24pt orphan must be refused");
    assert_eq!(error.code(), "TEXT_RUN_NOT_EDITABLE");
    let message = error.to_string();
    assert!(message.contains("fits-width"), "{message}");
    assert!(
        message.contains(&format!("{new_width:.1}pt")),
        "quotes the replacement width: {message}"
    );
    assert!(
        message.contains(&format!("{:.1}pt", tail.width.unwrap())),
        "quotes the run's own span, not the line: {message}"
    );
    assert!(
        message.contains("annotation tools to add a note"),
        "{message}"
    );
}

/// O1: the replacement that the user actually wrote into the tail must fit the tail's own span
/// — a ~24pt quota is generous for two glyphs and refuses a word.
#[test]
fn a_small_replacement_still_fits_the_orphans_own_span() {
    if !pdfium_ready() {
        return;
    }
    let path = fixture("wider-replacement-orphan.pdf");
    let document = load(&path);
    let runs = textedit::describe_runs(&document, 0, &pdfium_runs(&path, 0), &char_grid(&path, 0))
        .unwrap();
    let tail = runs.iter().find(|run| run.text == "ce").unwrap();
    let mut document = load(&path);
    let replacement = "ex";
    let new_width = measured_width(replacement, tail.font_size);
    assert!(new_width <= tail.width.unwrap() + 0.5, "{new_width}");
    edit_with_size(
        &mut document,
        0,
        tail.run_id.clone(),
        "ce",
        replacement,
        tail.width.unwrap(),
        new_width,
        tail.font_size,
    )
    .expect("two glyphs for two glyphs fits the orphan's own span");
}

// --- T16 / R5: the edited copy is re-indexed, and search finds the NEW text ----------

/// T16 extends T6: a substituting edit changes the page's text, so the copy's FTS5 rows
/// must be rebuilt from the bytes actually written — not left pointing at the pre-edit
/// text, and not merely cache-invalidated.
///
/// The test walks the real path the edit command takes: apply the edit to the fixture
/// through lopdf, write the copy, import it through the library layer, and index it with
/// the index service. It asserts the new text is searchable *and* the old text is not,
/// which is the difference between "re-extracted" and "stale rows survive".
#[test]
fn a_substitution_edit_re_indexes_the_copy_so_search_finds_the_new_text() {
    if !pdfium_ready() {
        return;
    }

    let db = Db::open_in_memory().unwrap();
    let render = Arc::new(RenderService::new());
    let index_service = IndexService::new();

    // Edit the fixture in memory and write the copy to a temp path.
    let mut document = load(&fixture("subset-winansi-escape.pdf"));
    let run = first_run(&document, 0);
    assert_eq!(run.text, "WIND");
    let outcome = edit_with(&mut document, 0, run.id, "WIND", "WINDOW", 200.0, 5.0).unwrap();
    assert!(outcome.substitution.is_some(), "this edit substitutes");

    let dir = tempfile::tempdir().unwrap();
    let copy = dir.path().join("edited.pdf");
    document.save(&copy).unwrap();

    // Import the copy through the ordinary library route, then index it exactly as the
    // command's detached `spawn_import_and_index` does.
    let entry = db
        .with(|connection| library::open_and_import(connection, &copy, 1))
        .unwrap();
    let doc_id = entry.meta.id.clone();
    index_app::index_document(None, &db, &render, &index_service, &doc_id).unwrap();

    // R5: search finds the NEW text.
    let hits = db
        .with(|connection| search::query(connection, "WINDOW", 10))
        .unwrap();
    assert_eq!(hits.len(), 1, "the replacement text must be searchable");
    assert_eq!(hits[0].document_id, doc_id);
    assert_eq!(hits[0].page, 0);

    // And the pre-edit text is gone: the rows were replaced, not appended.
    let stale = db
        .with(|connection| search::query(connection, "WIND", 10))
        .unwrap();
    assert!(
        stale.is_empty(),
        "the pre-edit token must not survive re-indexing: {stale:?}"
    );
}

// --- P2.4.3 T23: the rendered page is the measurement oracle -------------------------

/// §3 `unmeasurable-font.pdf`: the over-refusal 0.4.2 produced, and its cure.
///
/// The font is a subset-named `TrueType` dict with **no embedded program and no standard-14
/// name**. 0.4.2's `FontMetrics::resolve` finds no `FontFile*` to read and no AFM entry, returns
/// `Nominal`, and the run has no in-file span — so `group_span` was `None`, `resolve_quota` hit
/// its `Unmeasurable` arm, and the gate refused **every** run on the page with "font carries no
/// width metrics". That is the Maastricht lockout in miniature.
///
/// P2.6 T26 also reads `/Widths`, which this fixture *does* publish (224 codes, all 600). The
/// run is therefore measurable in-file now, exactly as the Maastricht Times resources are; the
/// grid still has to agree with it.
///
/// T23a measures the span from PDFium's character grid instead: the run is editable and its
/// quota is the union of its characters' advance boxes.
#[test]
fn an_unmeasurable_font_run_is_measurable_from_the_char_grid() {
    if !pdfium_ready() {
        return;
    }
    let path = fixture("unmeasurable-font.pdf");
    let document = load(&path);
    let runs = textedit::describe_runs(&document, 0, &pdfium_runs(&path, 0), &char_grid(&path, 0))
        .unwrap();

    assert_eq!(runs.len(), 1, "one run on the page");
    let run = &runs[0];
    assert_eq!(run.text, "Data Science");
    assert!(
        run.admissibility.editable,
        "T23a restores editability where 0.4.2 refused the page: {:?}",
        run.admissibility
    );
    assert_eq!(run.admissibility.gate, None);

    // The fixture publishes `/Widths` (600/1000 for every code), so the advance is exact and
    // must agree with the rendered span below rather than fall back to the `0.5em`
    // approximation. P2.6 T26 made this measurable; before it, only the grid was.
    let raw = first_run(&document, 0);
    assert_eq!(raw.text, "Data Science");
    assert_eq!(
        raw.advance_exact,
        Some(86.4),
        "12 codes x 600/1000 at 12pt, from the font's own /Widths"
    );

    // The char-grid quota is the rendered span, 12 glyphs x 0.6em at 12pt = 86.4pt.
    let width = run.width.expect("an editable run has a quota");
    assert!(
        (width - 86.4).abs() < 0.5,
        "the quota is the rendered span, not the approximation: {width}"
    );
    assert!(
        !run.box_suspicious,
        "the box is ~= the span, so it is not line-sized"
    );
}

/// §3 `unmeasurable-chain-break.pdf`: the Maastricht line-1 shape, memorialised.
///
/// Two `Tj` operands — `DataSci` and `ence` — placed so the tail begins exactly where the head's
/// **rendered** advance ends (a zero seam). The font has no in-file metrics, so 0.4.2 measured
/// the head with the `0.5em` approximation (`42pt` instead of `50.4pt`), computed a spurious
/// `8.4pt` gap, broke the chain, left `ence` an orphan on a line-sized box, and refused the page.
///
/// T23b measures the seam from the char grid: gap = `next.x0 - head.x1 = 0`, the chain is whole,
/// and the line is **one** editable run with one quota. A short replacement fits; an over-wide
/// one hard-refuses quoting both widths (O1).
#[test]
fn unmeasurable_chain_break_re_merges_via_char_grid_gaps() {
    if !pdfium_ready() {
        return;
    }
    let path = fixture("unmeasurable-chain-break.pdf");
    let document = load(&path);
    let runs = textedit::describe_runs(&document, 0, &pdfium_runs(&path, 0), &char_grid(&path, 0))
        .unwrap();

    // The line is ONE entry: T23b re-merged the two operands that the approximation severed.
    assert_eq!(
        runs.len(),
        1,
        "the whole line is one run, not a head + orphan"
    );
    let line = &runs[0];
    assert_eq!(line.text, "DataScience");
    assert_eq!(line.fragment_count, 2, "both operands folded in");
    assert!(line.admissibility.editable);
    assert_eq!(
        line.pair_confidence,
        Some(textedit::PairConfidence::Strong),
        "the head's pair is strong"
    );
    assert!(
        !line.box_suspicious,
        "a genuinely merged line is not box-suspicious"
    );
    let quota = line.width.expect("the merged line has a quota");
    assert!(quota > 70.0, "the whole line's span: {quota}");

    // A short replacement — same glyph count, same width — fits the merged line.
    let short = "DataSci";
    let new_width = measured_width(short, line.font_size);
    assert!(new_width <= quota, "{new_width} <= {quota}");
    let mut document = load(&path);
    let tail: Vec<textedit::FragmentRef> = line.fragments[1..]
        .iter()
        .map(|fragment| textedit::FragmentRef {
            run_id: fragment.run_id.clone(),
            text: fragment.text.clone(),
        })
        .collect();
    let measure = move |_text: &str| Ok(new_width);
    let request = textedit::EditRequest {
        page: 0,
        run_id: line.fragments[0].run_id.clone(),
        expected: line.fragments[0].text.clone(),
        replacement: short.to_string(),
        old_width: quota,
        font_size: line.font_size,
        allow_substitution: true,
        fragments: tail,
        measure: &measure,
        range: Vec::new(),
    };
    textedit::edit(&mut document, &request).expect("a short replacement fits the merged line");

    // An over-wide replacement hard-refuses, quoting the run's own span and the replacement.
    let wide = "DataScience and more";
    let wide_width = measured_width(wide, line.font_size);
    assert!(wide_width > quota, "{wide_width} > {quota}");
    let mut document = load(&path);
    let tail: Vec<textedit::FragmentRef> = line.fragments[1..]
        .iter()
        .map(|fragment| textedit::FragmentRef {
            run_id: fragment.run_id.clone(),
            text: fragment.text.clone(),
        })
        .collect();
    let measure = move |_text: &str| Ok(wide_width);
    let request = textedit::EditRequest {
        page: 0,
        run_id: line.fragments[0].run_id.clone(),
        expected: line.fragments[0].text.clone(),
        replacement: wide.to_string(),
        old_width: quota,
        font_size: line.font_size,
        allow_substitution: true,
        fragments: tail,
        measure: &measure,
        range: Vec::new(),
    };
    let error = textedit::edit(&mut document, &request)
        .expect_err("a line-wide replacement into the merged run must be refused");
    assert_eq!(error.code(), "TEXT_RUN_NOT_EDITABLE");
    let message = error.to_string();
    assert!(message.contains("fits-width"), "{message}");
    assert!(
        message.contains(&format!("{wide_width:.1}pt")),
        "quotes the replacement width: {message}"
    );
    assert!(
        message.contains(&format!("{quota:.1}pt")),
        "quotes the run's own span: {message}"
    );
    assert!(
        message.contains("annotation tools to add a note"),
        "{message}"
    );
}

// --- P2.4.4 T24: grid-walk spans from the rendered parent object ---------------

/// §3 T24-A: the user's real Maastricht brochure, page 1, end to end.
///
/// Line 1 is a 22-operand `TJ` in `ABCDEE+Monaco` — a face with **no** usable in-file metric
/// (its `/FontFile2` is Flate-compressed, so `substitute::stream_bytes` hands `ttf-parser` bytes
/// that do not parse). The lopdf walk therefore charges `0.5em`/glyph against a real `0.6em`
/// face and its per-fragment pens drift ~1.5pt/char, ~6pt by `ence `. Before T24 that drifting
/// anchor mis-located the tail's grid chars, the chain broke, and the picker offered a
/// `box_suspicious` `Weak` singleton. T24 re-derives each fragment's span by *character
/// alignment* inside the object's own grid window, so the pens never decide a span.
///
/// Ground truth from the rendered page (probed): the object's box is x 47.043..379.008,
/// baseline band y0 438.02 y1 454.19; the grid's loose span runs 45.888..388.008 = **342.12pt**.
/// The box's `y0` sits 3.39pt *below* the 441.41 baseline because `versity` has a descender —
/// which is why `rendered_baseline` (T24) reads the baseline from the grid, not the box edge.
#[test]
fn t24_real_maastricht_line_one_is_one_editable_run() {
    if !pdfium_ready() {
        return;
    }
    let path = fixture("real-maastricht-brochure.pdf");
    let document = load(&path);
    let runs = textedit::describe_runs(&document, 1, &pdfium_runs(&path, 1), &char_grid(&path, 1))
        .unwrap();

    let line = runs
        .iter()
        .find(|run| run.text.contains("Data Science"))
        .expect("the brochure's first line is described");

    // ONE run, not 22 singletons: the whole line folded together.
    assert_eq!(
        line.text, "5 month University level Data Science ",
        "the joined line text"
    );
    assert_eq!(
        line.fragment_count, 22,
        "all 22 TJ operands folded in (the design doc's '23' counted the box's trailing \\r\\n)"
    );
    assert!(
        line.admissibility.editable,
        "the line is editable: {:?}",
        line.admissibility
    );
    assert!(
        !line.box_suspicious,
        "a genuinely merged line is not box-suspicious"
    );
    assert_eq!(
        line.pair_confidence,
        Some(textedit::PairConfidence::Strong),
        "the descender-depth baseline is read from the grid, so the pair is strong"
    );
    let quota = line.width.expect("the merged line has a measured quota");
    assert!(
        (quota - 342.0).abs() < 1.0,
        "the quota is the true rendered span (342.0pt), not a drifting anchor: {quota}"
    );

    // A narrower replacement fits the line's quota and is written.
    let short = "Data Science";
    let short_width = measured_width(short, line.font_size);
    assert!(short_width <= quota, "{short_width} <= {quota}");
    let mut document = load(&path);
    let tail: Vec<textedit::FragmentRef> = line.fragments[1..]
        .iter()
        .map(|fragment| textedit::FragmentRef {
            run_id: fragment.run_id.clone(),
            text: fragment.text.clone(),
        })
        .collect();
    let measure = move |_text: &str| Ok(short_width);
    let request = textedit::EditRequest {
        page: 1,
        run_id: line.fragments[0].run_id.clone(),
        expected: line.fragments[0].text.clone(),
        replacement: short.to_string(),
        old_width: quota,
        font_size: line.font_size,
        allow_substitution: true,
        fragments: tail,
        measure: &measure,
        range: Vec::new(),
    };
    textedit::edit(&mut document, &request)
        .expect("a narrower replacement fits the merged line's span");

    // A wider replacement hard-refuses, quoting both measured widths (O1).
    let wide = "5 month University level Data Science and Engineering ";
    let wide_width = measured_width(wide, line.font_size);
    assert!(wide_width > quota, "{wide_width} > {quota}");
    let mut document = load(&path);
    let tail: Vec<textedit::FragmentRef> = line.fragments[1..]
        .iter()
        .map(|fragment| textedit::FragmentRef {
            run_id: fragment.run_id.clone(),
            text: fragment.text.clone(),
        })
        .collect();
    let measure = move |_text: &str| Ok(wide_width);
    let request = textedit::EditRequest {
        page: 1,
        run_id: line.fragments[0].run_id.clone(),
        expected: line.fragments[0].text.clone(),
        replacement: wide.to_string(),
        old_width: quota,
        font_size: line.font_size,
        allow_substitution: true,
        fragments: tail,
        measure: &measure,
        range: Vec::new(),
    };
    let error = textedit::edit(&mut document, &request)
        .expect_err("a line-wide replacement must hard-refuse");
    assert_eq!(error.code(), "TEXT_RUN_NOT_EDITABLE");
    let message = error.to_string();
    assert!(message.contains("fits-width"), "{message}");
    assert!(
        message.contains(&format!("{wide_width:.1}pt")),
        "quotes the replacement width: {message}"
    );
    assert!(
        message.contains(&format!("{quota:.1}pt")),
        "quotes the line's own rendered span: {message}"
    );
    assert!(
        message.contains("annotation tools to add a note"),
        "{message}"
    );
}

/// §3 T24-B: a synthetic 22-element `TJ` in a metrics-less wide-glyph face.
///
/// The Maastricht line's shape, rebuilt without the 385KB file: `ABCDEE+Monaco` (a
/// Flate-compressed `/FontFile2`, so `FontMetrics::resolve` yields `Nominal`) with real `0.6em`
/// advances over the same 22 operands and kerns. The `0.5em` approximation drifts ~6pt by
/// `ence ` — by construction, on any architecture. Before T24 the chain fragmented; T24's
/// character-aligned spans merge it into one run whose quota is the true span (38 glyphs x
/// 0.6em at 15pt = 342pt).
#[test]
fn t24_synthetic_wide_glyph_tj_merges_via_character_alignment() {
    if !pdfium_ready() {
        return;
    }
    let path = fixture("tj-grid-walk.pdf");
    let document = load(&path);
    let runs = textedit::describe_runs(&document, 0, &pdfium_runs(&path, 0), &char_grid(&path, 0))
        .unwrap();

    assert_eq!(
        runs.len(),
        1,
        "the 22 operands are one run, not 22 singletons"
    );
    let line = &runs[0];
    assert_eq!(line.text, "5 month University level Data Science ");
    assert_eq!(line.fragment_count, 22, "every operand folded in");
    assert!(line.admissibility.editable);
    assert!(
        !line.box_suspicious,
        "a genuinely merged line is not suspicious"
    );
    assert_eq!(line.pair_confidence, Some(textedit::PairConfidence::Strong));
    assert_eq!(
        line.width,
        Some(342.0),
        "the quota is the true rendered span, not the drifting 337.5pt approximate one"
    );
}

/// §3 T24-C: a kern seam wide enough for PDFium to synthesise a space.
///
/// Two operands, `Data` and `Sci`, separated by a `-400` kern in the same metrics-less `0.6em`
/// face. PDFium reconstructs the seam as a **generated** space (grid index 4, `gen=true`, width
/// 0) while the operands read `Data`/`Sci`. T24's character walk must *skip* that generated
/// character: assigning it to either fragment would shift the rest of the line by one and
/// mis-measure both. With it skipped, the two fragments keep exactly their own rendered chars
/// (4 and 3), their spans are exact, and they merge across the seam.
#[test]
fn t24_generated_space_is_skipped_when_aligning_fragments() {
    if !pdfium_ready() {
        return;
    }
    let path = fixture("tj-kern-seam.pdf");
    let document = load(&path);
    let runs = textedit::describe_runs(&document, 0, &pdfium_runs(&path, 0), &char_grid(&path, 0))
        .unwrap();

    assert_eq!(runs.len(), 1, "Data + Sci merge across the generated seam");
    let line = &runs[0];
    // P2.6 T26 item 4: item 3 of the T26 list asked this case to keep the operands' bytes. The
    // reading-order pass makes the picker agree with the page instead, which reconstructs the
    // kern as a word space. The operands still carry no `0x20`; the separator is display-only.
    assert_eq!(
        line.text, "Data Sci",
        "the page reads two words across the kern"
    );
    assert_eq!(line.fragment_count, 2, "both operands folded in");

    // The seam really is a PDFium-generated space: a grid character that occupies a slot but no
    // operand's bytes. Asserting it here keeps the fixture honest about what it exercises.
    let grid = char_grid(&path, 0);
    let generated = grid
        .chars
        .iter()
        .filter(|character| character.generated && character.x0 > 82.0 && character.x0 < 90.0)
        .count();
    assert_eq!(generated, 1, "PDFium synthesised exactly one seam space");

    // The merged span is the two fragments' own glyph runs — 4 glyphs and 3 glyphs of 0.6em at
    // 15pt (9pt a glyph): 36 + 27 = 63 — plus the 6pt seam the page really advances across
    // (`Data` ends at 82, `Sci` begins at 88). The generated character itself is still charged
    // to nobody: it has zero advance, so a mis-assigned slot would shift a fragment's window by
    // a whole glyph and this 69 would not come out exact.
    assert_eq!(
        line.width,
        Some(69.0),
        "Data (36pt) + seam (6pt) + Sci (27pt): the generated slot itself is not charged"
    );
}

/// T25 regression guard: the brochure's page-3 "missing data …" line must be ONE merged,
/// editable run (the blue-text bug).
#[test]
fn t25_maastricht_missing_word_line_is_one_editable_run() {
    if !pdfium_ready() {
        return;
    }
    let path = fixture("real-maastricht-brochure.pdf");
    let document = load(&path);
    let page = 2u32;
    let runs = textedit::describe_runs(
        &document,
        page,
        &pdfium_runs(&path, page),
        &char_grid(&path, page),
    )
    .unwrap();
    for r in &runs {
        // The user's broken pick was an *editable* `an` singleton the picker offered mid-line.
        // Other operations on the page do contain non-editable `an` fragments (different
        // chains), so only the editable shape is this bug.
        assert!(
            !(r.text == "an" && r.admissibility.editable),
            "the 'an' singleton (the user's broken pick) must be gone"
        );
    }

    let line = runs
        .iter()
        .find(|r| r.text.contains("missing data"))
        .expect("the line's strong chain is offered as one merged run");
    // The merged run is editable, genuinely merged, and carries the previously stranded `an`
    // fragment the user could only reach as a broken singleton. (The tail after it —
    // "d basic reporting tools," — lives in unpaired fragments of the same operation, which
    // T21 intentionally reports as single runs; that is next ticket's territory, not the
    // phantom-singleton regression T25 pins down.)
    assert!(line.admissibility.editable, "the line is editable");
    assert!(
        line.fragment_count > 1,
        "the line is merged, not a singleton"
    );
    assert!(
        line.text.contains("statistics an"),
        "the stranded `an` is merged into the line: {:?}",
        line.text
    );
}

/// T25 a/b regression guard: the brochure's page-3 syllabus line
/// "programming in Data Science. Fundamental concepts of …" must be ONE editable run.
///
/// Both defects shared one cause: the line's boxes are line-sized, so T24's per-box character
/// walk handed operands characters from elsewhere on the line. `Fundamental ` came out with no
/// span (invisible to the editable pick, leaving "concepts of pro" as its own pick) and the
/// `concepts of pro` pick ended at the wrong glyph. The operation-level resync is what makes
/// the spans run rightward again; without it this line splits at the `ce. `/`Fu` seam.
#[test]
fn t25_maastricht_syllabus_line_is_one_editable_run() {
    if !pdfium_ready() {
        return;
    }
    let path = fixture("real-maastricht-brochure.pdf");
    let document = load(&path);
    let page = 2u32;
    let runs = textedit::describe_runs(
        &document,
        page,
        &pdfium_runs(&path, page),
        &char_grid(&path, page),
    )
    .unwrap();

    let line = runs
        .iter()
        .find(|r| r.text.contains("Fundamental concepts"))
        .expect("the syllabus line is one merged run across the Fundamental seam");
    assert!(line.admissibility.editable, "the line is editable");
    assert!(
        line.text
            .starts_with("programming in Data Science. Fundamental "),
        "the line starts at the operation's first operand: {:?}",
        line.text
    );
    assert!(
        line.text.contains("concepts of programming"),
        "`concepts` follows `Fundamental ` in the same run: {:?}",
        line.text
    );
    assert!(
        line.width.is_some_and(|width| width > 400.0),
        "the run is measured by its own glyph span, not a fragment box: {:?}",
        line.width
    );
    // The broken pick was `concepts of pro` alone, with `Fundamental ` unreachable.
    assert!(
        !runs
            .iter()
            .any(|r| r.admissibility.editable && r.text.trim() == "concepts of pro"),
        "the truncated `concepts of pro` pick must be gone"
    );
}

/// T26 regression guard: the `Overview` line on page 3 (index 2), which the user reported as
/// split in two — a selectable `Overview`, then only `O` editable and the rest of the line as
/// single-character picks.
///
/// The line's boxes overlap: the bold run (`F4`) and the roman run (`F5`) are separate PDFium
/// objects whose ink nearly abut, and `O` opens 0.48pt to the *left* of its own box's ink — but
/// still 3.19pt inside that box. `pair_run`'s tolerance reaches past a box's border by 0.25em,
/// so both boxes "contained" the origin, and the leftmost-edge tiebreak handed `O` to the
/// **preceding** box. Its span then pointed at the previous operation's characters, the seam
/// to `v` measured backwards, and the chain stopped dead: the tail of the line fell out as
/// one pick per operand.
///
/// Preferring the box the origin sits deeper inside keeps the pair on its own ink. This is a
/// regression guard, not a tolerance change: the boxes and the merge window are untouched.
#[test]
fn t26_maastricht_overview_line_is_one_editable_run() {
    if !pdfium_ready() {
        return;
    }
    let path = fixture("real-maastricht-brochure.pdf");
    let document = load(&path);
    let page = 2u32;
    let runs = textedit::describe_runs(
        &document,
        page,
        &pdfium_runs(&path, page),
        &char_grid(&path, page),
    )
    .unwrap();

    let line = runs
        .iter()
        .find(|run| run.text.contains("Overview on data preparations"))
        .expect("the Overview line is one merged run");

    assert_eq!(
        line.text, "Overview on data preparations techniques, (data cleaning, data binning, ",
        "the whole line from `Overview` to the trailing comma is one run"
    );
    assert_eq!(line.fragment_count, 44, "all 44 operands folded in");
    assert!(
        line.admissibility.editable,
        "the line is editable: {:?}",
        line.admissibility
    );
    assert_eq!(
        line.pair_confidence,
        Some(textedit::PairConfidence::Strong),
        "every fragment pairs with its own box"
    );

    // The reported symptom: `O` alone, and the tail as per-operand picks. Neither may return.
    assert!(
        !runs.iter().any(|run| run.text == "O"),
        "`O` is not its own pick"
    );
    assert!(
        !runs
            .iter()
            .any(|run| run.admissibility.editable && run.text == "v"),
        "`v` is not its own pick"
    );
    assert!(
        !runs
            .iter()
            .any(|run| run.admissibility.editable && run.text == ", " && run.fragment_count == 1),
        "the line's trailing comma is not a lone pick"
    );

    // `Overview` continues from the bold run it follows, so the line's opening pick carries
    // both operations — the user's "separates them into 2 parts".
    assert!(
        runs.iter()
            .any(|run| run.text == "Understanding and data Preparation "),
        "the bold lead-in stays its own run"
    );
}

/// T26 regression guard: the two T25 defects the user actually saw on page 2 (index 2).
///
/// 1. `Fundamental` was invisible to the editable pick: the `ce. `/`Fu` seam was refused
///    because the two fragments live in *different font resources* (`F4`/`F5`), even though
///    both are Times at 14.064pt. Merging by **style class** rather than resource name is
///    what joins them.
/// 2. `Scientific` was selectable on its own while `P` looked editable: the `TJ` operands
///    `(Scienti)` `(fi)` `(c )` `(P)` … never merged because the resource published only
///    `/Widths` — no AFM entry, no embedded program — so `advance_exact` was `None` and the
///    chain broke at the very first seam. Reading `/Widths` makes every seam exact.
#[test]
fn t26_maastricht_page_two_runs_are_whole_lines() {
    if !pdfium_ready() {
        return;
    }
    let path = fixture("real-maastricht-brochure.pdf");
    let document = load(&path);
    let page = 2u32;
    let runs = textedit::describe_runs(
        &document,
        page,
        &pdfium_runs(&path, page),
        &char_grid(&path, page),
    )
    .unwrap();

    // Defect 1: `Fundamental` is reachable, and as part of the whole syllabus line — not as a
    // truncated `concepts of pro` pick and not as an invisible fragment.
    let line = runs
        .iter()
        .find(|run| run.text.contains("Fundamental concepts"))
        .expect("`Fundamental` is part of an editable run");
    assert!(line.admissibility.editable);
    assert!(
        line.text
            .contains("programming in Data Science. Fundamental concepts"),
        "the seam is joined: {:?}",
        line.text
    );

    // Defect 2: the Scientific Programming heading is ONE run ending in "II", not `Scienti`
    // plus separately-pickable fragments.
    let heading = runs
        .iter()
        .find(|run| run.text.contains("Scientific Programming"))
        .expect("the heading is one editable run");
    assert!(
        heading.admissibility.editable,
        "{:?}",
        heading.admissibility
    );
    assert_eq!(
        heading.text.trim(),
        "Scientific Programming I and II",
        "the whole heading, including the previously severed `P`"
    );
    assert!(heading.fragment_count > 5, "the TJ operands folded in");
    assert!(
        !runs.iter().any(|run| run.admissibility.editable
            && ["Scienti", "fi", "c ", "P"].contains(&run.text.as_str())),
        "no mid-heading fragment is offered as its own pick"
    );
}

/// P2.6 T26: a selection is a **range of runs**, and one write replaces the whole range.
///
/// Two `Tj` operands on one baseline, one style, one size: the shape left over when a line
/// is split across operations. Replacing the selection writes the head and clears the second
/// operand in the *same* write, so the user's one edit is one undo.
#[test]
fn t26_a_selection_range_is_replaced_in_one_write() {
    let path = fixture("two-run-line.pdf");
    let mut document = load(&path);
    let runs =
        textlocate::locate_runs(&document, textlocate::page_id(&document, 0).unwrap()).unwrap();
    assert_eq!(runs.len(), 2, "the fixture draws two operands");

    let (head, beside) = (runs[0].clone(), runs[1].clone());
    let width = measured_width("ALPHA BETA", head.font_size);
    let measure = move |_text: &str| Ok(width);
    let request = textedit::EditRequest {
        page: 0,
        run_id: head.id.clone(),
        expected: head.text.clone(),
        replacement: "ALPHA BETA".to_string(),
        old_width: width,
        font_size: head.font_size,
        allow_substitution: false,
        fragments: Vec::new(),
        range: vec![textedit::FragmentRef {
            run_id: beside.id.clone(),
            text: beside.text.clone(),
        }],
        measure: &measure,
    };
    textedit::edit(&mut document, &request).expect("one contiguous same-style range");

    let after =
        textlocate::locate_runs(&document, textlocate::page_id(&document, 0).unwrap()).unwrap();
    assert_eq!(after.len(), 1, "the cleared operand is no longer a run");
    assert_eq!(
        after[0].text, "ALPHA BETA",
        "the head carries the replacement"
    );
}

/// A range that spans two baselines is refused `selection-range`, never written.
#[test]
fn t26_a_range_across_lines_is_refused() {
    if !pdfium_ready() {
        return;
    }
    let path = fixture("real-maastricht-brochure.pdf");
    let page = 2u32;
    let document = load(&path);
    let runs = textedit::describe_runs(
        &document,
        page,
        &pdfium_runs(&path, page),
        &char_grid(&path, page),
    )
    .unwrap();
    let head = runs
        .iter()
        .find(|run| run.admissibility.editable && run.text.contains("Scientific Programming"))
        .expect("the heading is editable");
    let below = runs
        .iter()
        .find(|run| {
            run.admissibility.editable
                && run
                    .rect
                    .zip(head.rect)
                    .is_some_and(|(a, b)| a.y0 > b.y0 + 5.0)
        })
        .expect("a run on a later line");

    let width = head.width.unwrap_or(0.0);
    let measure = move |_text: &str| Ok(width);
    let mut document = load(&path);
    let request = textedit::EditRequest {
        page,
        run_id: head.fragments[0].run_id.clone(),
        expected: head.fragments[0].text.clone(),
        replacement: "shorter".to_string(),
        old_width: width,
        font_size: head.font_size,
        allow_substitution: false,
        fragments: Vec::new(),
        range: vec![textedit::FragmentRef {
            run_id: below.run_id.clone(),
            text: below.text.clone(),
        }],
        measure: &measure,
    };
    let error = textedit::edit(&mut document, &request).expect_err("a cross-line range refuses");
    assert_eq!(error.code(), "TEXT_RUN_NOT_EDITABLE");
    assert!(error.to_string().contains("selection-range"), "{error}");
}

// --- P2.7.1 T27/T28: marked-content boundaries ---------------------------------

/// T27: two fragments whose geometry is *identical* to a legitimate merge, but which sit in
/// different `MCID`s, must stay two runs.
///
/// This is the silent-wrong-write case: a table cell beside its neighbour, or two short
/// paragraphs on one baseline. Nothing in the old predicates could see it — same style, same
/// size, zero gap — so only the document's own structure separates them.
#[test]
fn mcid_boundary_stops_a_geometric_merge() {
    if !pdfium_ready() {
        return;
    }
    let path = fixture("mcid-boundaries.pdf");
    let document = load(&path);
    let runs = textedit::describe_runs(&document, 0, &pdfium_runs(&path, 0), &char_grid(&path, 0))
        .unwrap();

    assert_eq!(runs.len(), 2, "one run per tagged unit");
    assert_eq!(runs[0].text, "ALPHA ");
    assert_eq!(runs[1].text, "BETA");
    assert_eq!(runs[0].fragment_count, 1);
    assert_eq!(runs[1].fragment_count, 1);
}

/// T28: the same geometry, untagged, still merges into one run exactly as in 0.4.5.
///
/// The pair of assertions is the phase's whole safety argument: the boundary changes
/// behaviour only where the document has structure to offer.
#[test]
fn untagged_adjacent_runs_still_merge() {
    if !pdfium_ready() {
        return;
    }
    let path = fixture("two-run-line.pdf");
    let document = load(&path);
    let runs = textedit::describe_runs(&document, 0, &pdfium_runs(&path, 0), &char_grid(&path, 0))
        .unwrap();

    assert_eq!(runs.len(), 1, "untagged: one editable run");
    assert_eq!(runs[0].fragment_count, 2);
    assert_eq!(runs[0].text, "ALPHA BETA");
}

/// T30: an editable run under `/Span <</ActualText …>>` has its `ActualText` rewritten with
/// the edit, in the same pass, with no second save.
#[test]
fn actualtext_is_rewritten_with_the_edit() {
    let path = fixture("actualtext-span.pdf");
    let mut document = load(&path);
    let runs =
        textlocate::locate_runs(&document, textlocate::page_id(&document, 0).unwrap()).unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(
        runs[0].marked_content[0].actual_text.as_deref(),
        Some("ALPHA")
    );

    let outcome = edit_with(
        &mut document,
        0,
        runs[0].id.clone(),
        "ALPHA",
        "OMEGA",
        70.0,
        70.0,
    )
    .unwrap();
    assert!(
        outcome.actual_text_preserved,
        "an inline ActualText is the one case we can rewrite"
    );

    let page_id = textlocate::page_id(&document, 0).unwrap();
    let content = textlocate::decoded_content(&document, page_id).unwrap();
    let actual = content
        .operations
        .iter()
        .find(|operation| operation.operator == "BDC")
        .and_then(|operation| operation.operands.get(1))
        .and_then(|operand| operand.as_dict().ok())
        .and_then(|properties| properties.get(b"ActualText").ok())
        .and_then(|object| object.as_str().ok())
        .expect("ActualText survives the edit");
    assert_eq!(actual[0], 0xFE);
    assert_eq!(actual[1], 0xFF);
    let units: Vec<u16> = actual[2..]
        .chunks_exact(2)
        .map(|pair| u16::from_be_bytes([pair[0], pair[1]]))
        .collect();
    assert_eq!(String::from_utf16(&units).unwrap(), "OMEGA");
}

/// T30: a single inline `/Span` covering **two** operands describes both, so rewriting its
/// `ActualText` for one of them would change text outside the edit. The write must disclose
/// instead of rewriting.
///
/// This case had no test between T-3 and T-5: ruling B-7 renamed the only test that asserted
/// "reported, not rewritten" into one that asserts the *named* property is cloned, and the
/// inline multi-operand invariant went with the old name. §8 still claimed it.
#[test]
fn shared_actualtext_is_reported_not_rewritten() {
    let path = fixture("actualtext-shared-inline.pdf");
    let mut document = load(&path);
    let page_id = textlocate::page_id(&document, 0).unwrap();
    let runs = textlocate::locate_runs(&document, page_id).unwrap();
    assert_eq!(runs.len(), 2, "two operands in one span are two runs");

    let outcome = edit_with(
        &mut document,
        0,
        runs[0].id.clone(),
        "ALPHA",
        "OMEGA",
        70.0,
        70.0,
    )
    .unwrap();
    assert!(
        !outcome.actual_text_preserved,
        "one span describing two operands cannot be rewritten for one of them (T30)"
    );
    assert_eq!(outcome.inverse.new_bytes, b"OMEGA", "the edit itself lands");

    let page_id = textlocate::page_id(&document, 0).unwrap();
    let content = textlocate::decoded_content(&document, page_id).unwrap();
    let actual = content
        .operations
        .iter()
        .find(|operation| operation.operator == "BDC")
        .and_then(|operation| operation.operands.get(1))
        .and_then(|operand| operand.as_dict().ok())
        .and_then(|properties| properties.get(b"ActualText").ok())
        .and_then(|object| object.as_str().ok())
        .expect("ActualText survives the edit");
    assert_eq!(
        decode_utf16_be(actual),
        "ALPHA BETA",
        "the span still describes both operands, so it must be byte-identical"
    );
}

/// T30 + ruling B-7: a **named** `/Properties` entry is shared, so it is never rewritten in
/// place. Copy-on-write gives the edited sequence its own entry: the edit lands, the caller is
/// told the accessible text was preserved, and the shared entry keeps its original value.
#[test]
fn shared_actualtext_is_copied_before_it_is_rewritten() {
    let path = fixture("actualtext-named-property.pdf");
    let mut document = load(&path);
    let runs =
        textlocate::locate_runs(&document, textlocate::page_id(&document, 0).unwrap()).unwrap();
    assert_eq!(runs.len(), 1);

    let outcome = edit_with(
        &mut document,
        0,
        runs[0].id.clone(),
        "ALPHA",
        "OMEGA",
        70.0,
        70.0,
    )
    .unwrap();
    assert!(
        outcome.actual_text_preserved,
        "B-7 preserves the semantic text by cloning the entry, not by skipping it"
    );
    assert_eq!(outcome.inverse.new_bytes, b"OMEGA", "the edit itself lands");
    assert!(
        outcome.inverse.property_clone.is_some(),
        "the clone is recorded so undo can remove it"
    );

    let page_id = textlocate::page_id(&document, 0).unwrap();
    let runs = textlocate::locate_runs(&document, page_id).unwrap();
    assert_eq!(
        runs[0].marked_content[0].actual_text.as_deref(),
        Some("OMEGA"),
        "the edited span now reads the new semantic text"
    );
    assert_eq!(
        page_property_entry(&document, page_id, b"P0"),
        b"\xFE\xFF\0A\0L\0P\0H\0A".to_vec(),
        "the shared entry is byte-identical"
    );
}

/// T-2 (B-6): `ActualText` written in PDFDocEncoding must decode with the standard's table,
/// not Latin-1. `0x80` is a bullet and `0xA0` the euro sign; reading them as U+0080/U+00A0
/// would make the rewrite write back a different string than the one on the page.
#[test]
fn pdfdocencoding_actualtext_decodes_to_the_standard_characters() {
    let path = fixture("actualtext-pdfdocencoding.pdf");
    let document = load(&path);
    let page_id = textlocate::page_id(&document, 0).unwrap();
    let runs = textlocate::locate_runs(&document, page_id).unwrap();

    assert_eq!(runs.len(), 1);
    assert_eq!(
        runs[0].marked_content[0].actual_text.as_deref(),
        Some("ALPHA \u{2022} 5\u{20ac}"),
        "0x80 is a bullet and 0xA0 a euro sign in PDFDocEncoding"
    );
}

/// T-2 (B-6): an `ActualText` containing a code the standard leaves undefined cannot be
/// read, so it must not be silently replaced. The edit lands; the caller is told the
/// accessible text was not preserved.
#[test]
fn undecodable_actualtext_is_reported_not_rewritten() {
    let path = fixture("actualtext-undecodable.pdf");
    let mut document = load(&path);
    let page_id = textlocate::page_id(&document, 0).unwrap();
    let runs = textlocate::locate_runs(&document, page_id).unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(
        runs[0].marked_content[0].actual_text, None,
        "a string with an undefined PDFDocEncoding code decodes to nothing"
    );

    let outcome = edit_with(
        &mut document,
        0,
        runs[0].id.clone(),
        "ALPHA",
        "OMEGA",
        70.0,
        70.0,
    )
    .unwrap();
    assert!(
        !outcome.actual_text_preserved,
        "text we could not read must never be overwritten as 'preserved'"
    );
    assert_eq!(outcome.inverse.new_bytes, b"OMEGA", "the edit itself lands");

    let content = textlocate::decoded_content(&document, page_id).unwrap();
    let actual = content
        .operations
        .iter()
        .find(|operation| operation.operator == "BDC")
        .and_then(|operation| operation.operands.get(1))
        .and_then(|operand| operand.as_dict().ok())
        .and_then(|properties| properties.get(b"ActualText").ok())
        .and_then(|object| object.as_str().ok())
        .expect("the original ActualText survives untouched");
    assert_eq!(
        actual, b"ALPHA\xAD",
        "an undecodable ActualText is left byte-identical"
    );
}

/// T31: malformed marked content must not make a page uneditable.
#[test]
fn unbalanced_marked_content_does_not_refuse() {
    use lopdf::content::{Content, Operation};

    let mut document = lopdf::Document::load(fixture("simple-winansi.pdf")).unwrap();
    let page_id = textlocate::page_id(&document, 0).unwrap();
    let mut content = textlocate::decoded_content(&document, page_id).unwrap();
    // An `EMC` with nothing open, then a `BDC` that is never closed.
    content.operations.insert(0, Operation::new("EMC", vec![]));
    content.operations.insert(
        1,
        Operation::new(
            "BDC",
            vec![
                "P".into(),
                lopdf::Object::Dictionary(lopdf::dictionary! { "MCID" => 3 }),
            ],
        ),
    );
    let encoded = Content {
        operations: content.operations,
    }
    .encode()
    .unwrap();
    document.change_page_content(page_id, encoded).unwrap();

    let runs = textlocate::locate_runs(&document, page_id).unwrap();
    assert!(
        runs.len() >= 2,
        "the page still yields its runs; a broken tag stream is not a refusal"
    );
    assert_eq!(
        runs[0].innermost_mcid(),
        Some(3),
        "the unclosed BDC still scopes the text that follows it"
    );
}

/// T32: `Tc` moves the pen, so the second fragment of a `TJ` array starts where the page
/// draws it. Before P2.7.1 the walk ignored `Tc` and every seam after a spaced glyph was
/// `Tc` too narrow — enough to break a merge, or (worse) to merge the wrong pair.
#[test]
fn char_spacing_advances_the_tj_pen() {
    let path = fixture("char-spacing-tj.pdf");
    let document = load(&path);
    let runs =
        textlocate::locate_runs(&document, textlocate::page_id(&document, 0).unwrap()).unwrap();

    let first = runs.iter().find(|run| run.text == "AB").expect("AB");
    let second = runs.iter().find(|run| run.text == "CD").expect("CD");

    // Helvetica `A`+`B` at 24pt is 32.016pt of glyph; `Tc 5` adds 5pt after each of the two.
    assert!(
        (first.advance - 42.016).abs() < 0.05,
        "advance {:.3} must include 2 x Tc on top of 32.016pt of glyph",
        first.advance
    );
    // The pen moves by exactly that advance -- the second fragment starts where it is drawn.
    assert!(
        (second.origin.0 - first.origin.0 - first.advance).abs() < 0.05,
        "second origin gap {:.3} should equal the spaced advance {:.3}",
        second.origin.0 - first.origin.0,
        first.advance
    );
    assert_eq!(runs.len(), 2);
}

/// The block rule itself, on the real tagged document: `MCID`s 5/6/7 of page 3 are sibling
/// `Span`s under one `P`, so they are one merge scope even though their `MCID`s differ.
///
/// This is the correction that keeps T25/T26 green. If it regresses, a paragraph in every
/// InDesign-produced PDF becomes uneditable piece by piece.
#[test]
fn sibling_spans_share_one_merge_scope() {
    let path = fixture("real-maastricht-brochure.pdf");
    let document = load(&path);
    let page_id = textlocate::page_id(&document, 2).unwrap();
    let runs = textlocate::locate_runs(&document, page_id).unwrap();

    let spans: Vec<&textlocate::RawRun> = runs
        .iter()
        .filter(|run| matches!(run.innermost_mcid(), Some(5..=7)))
        .collect();
    assert!(!spans.is_empty(), "the fixture must still be tagged");
    assert!(
        spans
            .iter()
            .any(|run| run.innermost_mcid() != spans[0].innermost_mcid()),
        "the sample must actually span more than one MCID, or it proves nothing"
    );
    let scope = spans[0].merge_scope();
    assert!(
        matches!(scope, Some(textlocate::MergeScope::Block(_))),
        "a resolvable /ParentTree must yield a block scope, got {scope:?}"
    );
    assert!(
        spans.iter().all(|run| run.merge_scope() == scope),
        "sibling spans under one P are one scope"
    );
}

/// T30, the case that matters most in practice: a **named** `/Properties` bundle that holds
/// only an `MCID` declares no `ActualText`, so there is nothing to preserve and nothing to
/// warn about. Treating "named" as "shared ActualText" would raise the accessibility warning
/// on almost every InDesign-produced page.
#[test]
fn named_properties_without_actualtext_is_not_a_warning() {
    let path = fixture("named-property-no-actualtext.pdf");
    let mut document = load(&path);
    let runs =
        textlocate::locate_runs(&document, textlocate::page_id(&document, 0).unwrap()).unwrap();
    assert_eq!(runs.len(), 1);

    let outcome = edit_with(
        &mut document,
        0,
        runs[0].id.clone(),
        "ALPHA",
        "OMEGA",
        70.0,
        70.0,
    )
    .unwrap();
    assert!(
        outcome.actual_text_preserved,
        "no ActualText was declared, so the edit preserved all there was"
    );
}

/// Ruling B-7: a named `/Properties` entry that *does* carry `ActualText` is cloned rather
/// than mutated in place. Two sequences name `/P0`; editing one must rewrite only that one's
/// semantic text and leave `/P0` — and therefore the other sequence's `ActualText` — exactly
/// as it was.
#[test]
fn a_shared_named_property_is_cloned_not_rewritten_in_place() {
    let path = fixture("named-property-shared-actualtext.pdf");
    let mut document = load(&path);
    let page_id = textlocate::page_id(&document, 0).unwrap();
    let runs = textlocate::locate_runs(&document, page_id).unwrap();
    assert_eq!(runs.len(), 2, "two baselines, two runs");
    assert_eq!(
        runs[0].marked_content[0].actual_text.as_deref(),
        Some("ALPHA")
    );
    assert_eq!(
        runs[1].marked_content[0].actual_text.as_deref(),
        Some("ALPHA")
    );

    let outcome = edit_with(
        &mut document,
        0,
        runs[0].id.clone(),
        "ALPHA",
        "OMEGA",
        70.0,
        70.0,
    )
    .unwrap();
    assert!(
        outcome.actual_text_preserved,
        "a clone is exactly how B-7 lets us preserve it"
    );

    let content = textlocate::decoded_content(&document, page_id).unwrap();
    let names: Vec<Vec<u8>> = content
        .operations
        .iter()
        .filter(|operation| operation.operator == "BDC")
        .filter_map(|operation| operation.operands.get(1))
        .filter_map(|operand| operand.as_name().ok().map(<[u8]>::to_vec))
        .collect();
    assert_eq!(names.len(), 2, "both sequences still have properties");
    assert_ne!(
        names[0], names[1],
        "the edited sequence must be repointed at its own clone, not left sharing /P0"
    );
    assert_eq!(
        names[1], b"P0",
        "the untouched sequence keeps its original name"
    );

    // The original entry is byte-identical: no sequence outside the edit changed meaning.
    let original = page_property_entry(&document, page_id, b"P0");
    assert_eq!(
        original,
        b"\xFE\xFF\0A\0L\0P\0H\0A".to_vec(),
        "the shared /P0 must not be touched by the edit"
    );
    let clone = page_property_entry(&document, page_id, &names[0]);
    assert_eq!(
        decode_utf16_be(&clone),
        "OMEGA",
        "the clone carries the edited semantic text"
    );

    // The plan's acceptance is "byte-identical after save", so the clone and the untouched
    // entry must survive a real write and reload, not just the in-memory document.
    let saved = std::env::temp_dir().join("t3-shared-named-property.pdf");
    document.save(&saved).unwrap();
    let reloaded = load(&saved);
    let reloaded_page = textlocate::page_id(&reloaded, 0).unwrap();
    assert_eq!(
        page_property_entry(&reloaded, reloaded_page, b"P0"),
        b"\xFE\xFF\0A\0L\0P\0H\0A".to_vec(),
        "the shared /P0 is unchanged on disk"
    );
    let runs = textlocate::locate_runs(&reloaded, reloaded_page).unwrap();
    assert_eq!(runs.len(), 2, "both runs still read after reload");
    assert_eq!(
        runs[0].marked_content[0].actual_text.as_deref(),
        Some("OMEGA"),
        "the edited span keeps its cloned ActualText"
    );
    assert_eq!(
        runs[1].marked_content[0].actual_text.as_deref(),
        Some("ALPHA"),
        "the untouched span still reads the original ActualText"
    );
    std::fs::remove_file(&saved).unwrap();
}

// --- T-3b: a page's `/Resources` may be an indirect object, or inherited ---------

/// The `/BaseFont` each name in a page's `/Font` resources resolves to **as a viewer sees
/// it**: a page's own `/Resources` replaces an inherited one outright (ISO 32000-1 §7.7.3.4),
/// so nothing is merged across the page tree.
///
/// Deliberately not `Document::get_page_fonts`, which unions the page's dictionary with every
/// referenced one: that is the right question for "can this font be found at all", and the
/// wrong one for "will the viewer resolve the name the content stream writes".
fn page_fonts(document: &lopdf::Document, page: u32) -> Vec<(String, String)> {
    let page_id = textlocate::page_id(document, page).unwrap();
    let page = document.get_dictionary(page_id).unwrap();
    let mut node = page;
    let resources = loop {
        if let Ok(resources) = node.get(b"Resources") {
            break document
                .dereference(resources)
                .unwrap()
                .1
                .as_dict()
                .unwrap()
                .clone();
        }
        let Ok(parent) = node.get(b"Parent").and_then(lopdf::Object::as_reference) else {
            break lopdf::Dictionary::new();
        };
        node = document.get_dictionary(parent).unwrap();
    };
    let fonts = resources
        .get(b"Font")
        .ok()
        .and_then(|object| document.dereference(object).ok())
        .and_then(|(_, object)| object.as_dict().ok())
        .cloned()
        .unwrap_or_default();
    let mut names: Vec<(String, String)> = fonts
        .iter()
        .filter_map(|(name, font)| {
            let font = document.dereference(font).ok()?.1.as_dict().ok()?;
            let base = font.get(b"BaseFont").ok()?.as_name().ok()?;
            Some((
                String::from_utf8_lossy(name).into_owned(),
                String::from_utf8_lossy(base).into_owned(),
            ))
        })
        .collect();
    names.sort();
    names
}

/// T-3b, read side: with `/Resources` on the page tree, a page that shadows `/F1` sees its own
/// font and a page that inherits sees the ancestor's. `get_page_fonts` merges the page's inline
/// dictionary with the referenced ones, so the shadowing entry must win.
#[test]
fn an_inherited_resources_page_reads_the_font_it_actually_uses() {
    let document = load(&fixture("indirect-resources-inherited.pdf"));

    assert_eq!(
        page_fonts(&document, 0),
        vec![("F1".to_string(), "ABCDEF+Shadowed".to_string())],
        "the shadowing page reads its own /Resources, not the inherited one"
    );
    assert_eq!(
        page_fonts(&document, 1),
        vec![("F1".to_string(), "ABCDEF+Inherited".to_string())],
        "a page with no /Resources of its own reads the inherited one"
    );
}

/// T-3b, write side: the substitute must land in the resources dictionary **the edited page
/// actually resolves**. On a page that shadows an inherited `/Resources`, writing to the
/// ancestor's dictionary leaves the page pointing at a font name (`FS1`) it cannot resolve,
/// so the edited text stops rendering — a silent wrong write, not a refusal.
#[test]
fn a_substitute_lands_on_the_page_that_shadows_its_inherited_resources() {
    let mut document = load(&fixture("indirect-resources-inherited.pdf"));
    let run = first_run(&document, 0);
    assert_eq!(run.text, "SUBSET");

    edit_with(&mut document, 0, run.id, "SUBSET", "OMEGA", 200.0, 5.0)
        .expect("a subset run on a shadowing page is editable");

    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("edited.pdf");
    document.save(&target).unwrap();
    let reread = load(&target);

    let fonts = page_fonts(&reread, 0);
    assert!(
        fonts
            .iter()
            .any(|(name, base)| name == "FS1" && base == "Helvetica"),
        "the substitute must be visible to the page it was installed for, got {fonts:?}"
    );
    let runs = textlocate::locate_runs(&reread, textlocate::page_id(&reread, 0).unwrap()).unwrap();
    assert_eq!(runs.len(), 1, "the edited run must still be locatable");
    assert_eq!(runs[0].text, "OMEGA");
    assert_eq!(
        runs[0].font_key, b"FS1",
        "the run names the substitute it was swapped to"
    );
}

/// T-3b + ruling B-7: two pages inheriting one `/Resources` object must not be edited through
/// each other. Substituting on one page gives **that** page its own copy; its sibling's font
/// resources stay exactly as they were.
#[test]
fn a_substitution_on_an_inheriting_page_does_not_reach_its_sibling() {
    let mut document = load(&fixture("indirect-resources-inherited.pdf"));
    let run = first_run(&document, 1);
    assert_eq!(run.text, "ALPHA");

    edit_with(&mut document, 1, run.id, "ALPHA", "OMEGA", 200.0, 5.0)
        .expect("a subset run on an inheriting page is editable");

    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("edited.pdf");
    document.save(&target).unwrap();
    let reread = load(&target);

    let edited = page_fonts(&reread, 1);
    assert!(
        edited
            .iter()
            .any(|(name, base)| name == "FS1" && base == "Helvetica"),
        "the edited page resolves its substitute, got {edited:?}"
    );

    assert_eq!(
        page_fonts(&reread, 2),
        vec![("F1".to_string(), "ABCDEF+Inherited".to_string())],
        "the sibling page shares the inherited dictionary and must not have changed"
    );
    let sibling =
        textlocate::locate_runs(&reread, textlocate::page_id(&reread, 2).unwrap()).unwrap();
    assert_eq!(sibling.len(), 1);
    assert_eq!(sibling[0].text, "BRAVO");
    assert_eq!(sibling[0].font_key, b"F1");
}

/// One `/Properties` entry's raw `ActualText` bytes, read back through the page resources.
fn page_property_entry(
    document: &lopdf::Document,
    page_id: lopdf::ObjectId,
    name: &[u8],
) -> Vec<u8> {
    let page = document.get_dictionary(page_id).unwrap();
    let resources = document
        .dereference(page.get(b"Resources").unwrap())
        .unwrap()
        .1
        .as_dict()
        .unwrap();
    let properties = document
        .dereference(resources.get(b"Properties").unwrap())
        .unwrap()
        .1
        .as_dict()
        .unwrap();
    document
        .dereference(properties.get(name).unwrap())
        .unwrap()
        .1
        .as_dict()
        .unwrap()
        .get(b"ActualText")
        .unwrap()
        .as_str()
        .unwrap()
        .to_vec()
}

fn decode_utf16_be(bytes: &[u8]) -> String {
    assert_eq!(
        &bytes[..2],
        b"\xFE\xFF",
        "ActualText is written as UTF-16BE"
    );
    let units: Vec<u16> = bytes[2..]
        .chunks_exact(2)
        .map(|pair| u16::from_be_bytes([pair[0], pair[1]]))
        .collect();
    String::from_utf16(&units).unwrap()
}

/// P2.7.5 regression guard: the line the user reported, "Each hour of lecture – 2-3 hours of
/// study" on page 4 (index 3) of the Maastricht brochure.
///
/// v0.6.0 refused it with `simple-font`, because the picker merged it across three faces and
/// the leftmost fragment was the Arial CID bullet. Two things must hold now:
///
/// * every text fragment on the line is editable in its own font, including the `F6` CID
///   dash, so the user can edit the prose the report was about; and
/// * the body text is one run — merging it per-glyph would put 30+ entries in the picker.
///   It is the *same face* (`ABCDEE+Monaco`) throughout, so the face rule keeps it together;
///   only the bullet and the dash are separate, and they are separate fonts.
#[test]
fn p2_8_maastricht_lecture_line_is_editable_in_its_own_fonts() {
    if !pdfium_ready() {
        return;
    }
    let path = fixture("real-maastricht-brochure.pdf");
    let document = load(&path);
    let page = 3u32;
    let runs = textedit::describe_runs(
        &document,
        page,
        &pdfium_runs(&path, page),
        &char_grid(&path, page),
    )
    .unwrap();

    let body = runs
        .iter()
        .find(|run| run.text.contains("Each hour of lecture"))
        .expect("the reported line's body is one run");
    assert_eq!(body.font_key, "F3");
    assert!(
        body.admissibility.editable,
        "the body is editable: {:?}",
        body.admissibility
    );

    let tail = runs
        .iter()
        .find(|run| run.text.contains("2-3 hours of study"))
        .expect("the line's tail is one run");
    assert!(tail.admissibility.editable, "{:?}", tail.admissibility);

    let dash = runs
        .iter()
        .find(|run| run.text == "–")
        .expect("the CID dash is offered");
    assert_eq!(
        dash.font_key, "F6",
        "the dash is a Type0 fragment, not merged into the body"
    );
    assert!(
        dash.admissibility.editable,
        "P2.7.5: a CID dash whose /ToUnicode names it is editable: {:?}",
        dash.admissibility
    );
}
