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
        measure: &measure,
    };
    textedit::edit(document, &request)
}

fn first_run(document: &lopdf::Document, page: u32) -> textlocate::RawRun {
    let page_id = textlocate::page_id(document, page).unwrap();
    textlocate::locate_runs(document, page_id).unwrap()[0].clone()
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
fn cid_identity_is_refused_by_gate_a() {
    let mut document = load(&fixture("cid-identity.pdf"));
    let run = first_run(&document, 0);

    let error = edit_with(&mut document, 0, run.id, "CID", "TEXT", 100.0, 10.0)
        .expect_err("a CID font must be refused");
    assert_eq!(error.code(), "TEXT_RUN_NOT_EDITABLE");
    assert!(error.to_string().contains("simple-font"), "{error}");
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
        error.to_string().contains("add a text annotation"),
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
    assert!(message.contains("add a text annotation"), "{message}");
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
        error.to_string().contains("add a text annotation"),
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
    let runs = textedit::describe_runs(&document, 0, &pdfium_runs(&path, 0)).unwrap();

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
    let verdicts = textedit::describe_runs(&cid, 0, &[]).unwrap();
    assert_eq!(verdicts.len(), 1);
    assert!(!verdicts[0].admissibility.editable);
    assert_eq!(
        verdicts[0].admissibility.gate.as_deref(),
        Some("simple-font")
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
    let runs = textedit::describe_runs(&document, 0, &pdfium_runs(&path, 0)).unwrap();

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
    let runs = textedit::describe_runs(&document, 0, &pdfium_runs(&path, 0)).unwrap();

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
    let runs = textedit::describe_runs(&document, 0, &pdfium_runs(&path, 0)).unwrap();
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
        textedit::describe_runs(&document, 0, &pdfium_runs(&path, 0)).unwrap()
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
    let runs = textedit::describe_runs(&document, 0, &pdfium_runs(&path, 0)).unwrap();

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
    assert!(message.contains("add a text annotation"), "{message}");
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
    let runs = textedit::describe_runs(&document, 0, &pdfium_runs(&path, 0)).unwrap();
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
