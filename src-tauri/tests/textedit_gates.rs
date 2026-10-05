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
//! | (c) not subset | `simple-embedded-subset.pdf` | `subset_font_is_refused_by_gate_c` |
//! | (d) width | `too-wide.pdf` | `too_wide_is_refused_by_gate_d_with_both_widths` |
//! | (e) / F3 cap | `bomb.pdf` | `bomb_is_refused_as_content_too_large` |
//! | F2 collapse | `multi-stream-page.pdf` | `a_multi_stream_page_edits_and_stays_renderable` |
//! | T7 read-only | `encrypted-aes.pdf` | `encrypted_documents_refuse_every_edit` |
//! | T8 decrypt copy | `encrypted-aes.pdf` | `decrypt_copy_writes_an_unprotected_file` |

use std::path::{Path, PathBuf};

use document_reader_lib::editor::geometry::PdfRect;
use document_reader_lib::editor::protect;
use document_reader_lib::editor::runmetrics::{self, PdfiumRun};
use document_reader_lib::editor::textedit;
use document_reader_lib::editor::textlocate::{self, ContentOperation, RunId};
use document_reader_lib::model::AppError;
use document_reader_lib::provider::PdfProvider;

mod support;

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
) -> Result<ContentOperation, AppError> {
    let measure = move |_text: &str| Ok(new_width);
    let request = textedit::EditRequest {
        page,
        run_id,
        expected: expected.to_string(),
        replacement: replacement.to_string(),
        old_width,
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

    let inverse = edit_with(
        &mut document,
        0,
        run.id.clone(),
        "ALPHA",
        "BRAVO",
        38.7,
        39.0,
    )
    .expect("the best case must be editable");

    assert_eq!(inverse.old_bytes, b"ALPHA");
    assert_eq!(inverse.new_bytes, b"BRAVO");

    // The written bytes re-read as the edit, and the original file is untouched on disk.
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("edited.pdf");
    document.save(&target).unwrap();
    let reread = textlocate::locate_runs(&load(&target), textlocate::page_id(&load(&target), 0).unwrap())
        .unwrap();
    assert_eq!(reread[0].text, "BRAVO");

    let original = load(&fixture("simple-winansi.pdf"));
    let original_run = first_run(&original, 0);
    assert_eq!(original_run.text, "ALPHA", "the fixture on disk was not modified");
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

// --- gate (c) ------------------------------------------------------------------

#[test]
fn subset_font_is_refused_by_gate_c() {
    let mut document = load(&fixture("simple-embedded-subset.pdf"));
    let run = first_run(&document, 0);
    assert_eq!(run.text, "SUBSET");

    let error = edit_with(&mut document, 0, run.id, "SUBSET", "EDIT", 100.0, 10.0)
        .expect_err("a subset font must be refused");
    assert_eq!(error.code(), "TEXT_RUN_NOT_EDITABLE");
    assert!(error.to_string().contains("not-subset"), "{error}");
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
    assert!(message.contains("41.3"), "must report needed width: {message}");
    assert!(message.contains("38.7"), "must report available width: {message}");
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
        message.contains(&format!("{new_width:.1}")) && message.contains(&format!("{:.1}", run.width)),
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
        copy.file_name().unwrap().to_str().unwrap().contains("(unprotected)"),
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
    assert_eq!(verdicts[0].admissibility.gate.as_deref(), Some("simple-font"));
}

// --- T3d geometry conversion ---------------------------------------------------

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
    assert!((0.0..=792.0).contains(&y), "converted y out of page bounds: {y}");
}
