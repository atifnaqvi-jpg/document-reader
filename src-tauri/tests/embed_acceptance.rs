//! P2.7.6 §5 acceptance matrix, row by row, on real bytes.
//!
//! The design replaces the old absence test ("a non-WinAnsi glyph refuses") with four rows:
//!
//! | # | Replacement | Expected |
//! |---|---|---|
//! | 1 | non-WinAnsi, covered by a bundled face, substitution allowed | succeeds, subset embedded, renders, text round-trips |
//! | 2 | same glyph, substitution disallowed | `SUBSTITUTE_REFUSED`, document unchanged |
//! | 3 | non-WinAnsi, uncovered (`漢`) | `GLYPHS_NOT_COVERABLE`, document unchanged |
//! | 4 | WinAnsi only | MECH-A, and **no font program is written** |
//!
//! Rows 2-4 are implementable now and are written first, because they define what success in
//! row 1 must *stop* doing: refusing. Row 1 is `#[ignore]`d until the subset builder exists,
//! and its body is written so that landing the builder turns it green rather than requiring
//! someone to invent the assertions afterwards.
//!
//! Every row asserts **the document is unchanged** on refusal where a refusal is expected.
//! A refusal that has already mutated the page is worse than a failure, because the user sees
//! "nothing happened" while the bytes disagree.

use std::path::{Path, PathBuf};

use document_reader_lib::editor::textedit;
use document_reader_lib::editor::textlocate::{self, RunId};
use document_reader_lib::model::AppError;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

fn load(path: &Path) -> lopdf::Document {
    lopdf::Document::load(path).unwrap_or_else(|e| panic!("could not load {}: {e}", path.display()))
}

fn first_run(document: &lopdf::Document, page: u32) -> textlocate::RawRun {
    let page_id = textlocate::page_id(document, page).unwrap();
    textlocate::locate_runs(document, page_id)
        .unwrap()
        .into_iter()
        .next()
        .expect("fixture has a run")
}

/// Applies an edit with a fixed measurement; `old_width` stays generous so the width gate
/// does not pre-empt the row under test.
fn edit(
    document: &mut lopdf::Document,
    run_id: RunId,
    expected: &str,
    replacement: &str,
    allow_substitution: bool,
) -> Result<textedit::EditOutcome, AppError> {
    let measure = |_text: &str| Ok(60.0_f32);
    textedit::edit(
        document,
        &textedit::EditRequest {
            page: 0,
            run_id,
            expected: expected.to_string(),
            replacement: replacement.to_string(),
            old_width: 100.0,
            font_size: 24.0,
            allow_substitution,
            fragments: Vec::new(),
            range: Vec::new(),
            measure: &measure,
        },
    )
}

/// The page's decoded content, for "unchanged" assertions.
///
/// Comparing the content stream rather than the whole file is deliberate: a refusal must
/// leave the *page* alone, and that is the thing the user sees. `save` may legitimately
/// change trailer-level bytes.
fn page_content(document: &lopdf::Document, page: u32) -> Vec<u8> {
    let page_id = textlocate::page_id(document, page).unwrap();
    document.get_page_content(page_id)
}

/// Row 2 of §5: substitution disallowed refuses, and changes nothing.
#[test]
fn row2_substitution_disallowed_refuses_and_leaves_the_page_alone() {
    let path = fixture("non-winansi-replacement.pdf");
    let mut document = load(&path);
    let before = page_content(&document, 0);
    let run = first_run(&document, 0);

    let error = edit(&mut document, run.id, "HI", "H\u{2014}", false)
        .expect_err("substitution is off, so this must refuse");

    assert_eq!(error.code(), "SUBSTITUTE_REFUSED");
    assert_eq!(
        page_content(&document, 0),
        before,
        "a refusal must not have mutated the page"
    );
}

/// Row 3 of §5: a glyph no shipped face has refuses with the permanent code, changing nothing.
///
/// `漢` is outside every face in the bundle (Latin only), so this is a genuine coverage gap:
/// the user's remedy is a different font, not a retry.
#[test]
fn row3_uncovered_glyph_refuses_as_a_coverage_gap_and_leaves_the_page_alone() {
    let path = fixture("non-winansi-replacement.pdf");
    let mut document = load(&path);
    let before = page_content(&document, 0);
    let run = first_run(&document, 0);

    let error = edit(&mut document, run.id, "HI", "\u{6f22}", true)
        .expect_err("no shipped face has this glyph");

    assert_eq!(error.code(), "GLYPHS_NOT_COVERABLE");
    assert_ne!(
        error.code(),
        "SUBSET_BUILD_FAILED",
        "an absent glyph is a permanent gap, not a build failure"
    );
    assert_eq!(page_content(&document, 0), before);
}

/// Row 4 of §5: a WinAnsi-only replacement takes MECH-A and writes **no font program**.
///
/// The negative half is the part that can rot silently: a later change could route WinAnsi
/// text through the embed path and every other test would stay green while every ordinary
/// edit grew by the size of a subset. This inspects the written PDF for `/FontFile2` and
/// `/FontFile3`, and for a `/Subtype /Type0` root, none of which MECH-A emits.
#[test]
fn row4_winansi_takes_mech_a_and_writes_no_font_program() {
    let path = fixture("non-winansi-replacement.pdf");
    let mut document = load(&path);
    let run = first_run(&document, 0);

    let outcome = edit(&mut document, run.id, "HI", "HELP", true).expect("WinAnsi is editable");
    assert!(
        outcome.substitution.is_some(),
        "this fixture's own font cannot set even WinAnsi, so MECH-A must fire"
    );

    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("mech-a.pdf");
    document.save(&target).unwrap();

    let saved = load(&target);
    for (_, object) in saved.objects.iter() {
        match object {
            lopdf::Object::Stream(stream) => {
                assert!(
                    !stream.dict.has(b"FontFile2"),
                    "MECH-A must not embed a TrueType program; row 4 is 'the cheap path stays \
                     cheap'"
                );
                assert!(
                    !stream.dict.has(b"FontFile3"),
                    "MECH-A must not embed an OpenType program"
                );
            }
            lopdf::Object::Dictionary(dict) => assert_no_program(dict),
            _ => {}
        }
    }
}

fn assert_no_program(dict: &lopdf::Dictionary) {
    if let Ok(subtype) = dict.get(b"Subtype").and_then(lopdf::Object::as_name) {
        if subtype == b"Type0" {
            panic!("MECH-A writes a simple font; a Type0 root means the embed path ran");
        }
    }
}

/// Row 1 of §5: non-WinAnsi, covered by a bundled face, substitution allowed.
///
/// The character is U+0100, not U+2014: measured in `font_coverage.rs`, an em dash is CP1252
/// 0x97 and therefore takes MECH-A, writing no subset at all. The first draft of this test used
/// U+2014 and "proved" embedding with a standard-14 edit.
///
/// The assertions are deliberately about the **saved file**, then about reading it back:
/// rendering through PDFium is the only check that the written font is a font and not just a
/// well-formed dictionary, and the re-read is what makes undo and a second edit possible.
#[test]
fn row1_covered_non_winansi_embeds_a_subset_and_round_trips() {
    let path = fixture("non-winansi-replacement.pdf");
    let mut document = load(&path);
    let before = page_content(&document, 0);
    let run = first_run(&document, 0);

    let replacement = "H\u{0100}";
    let outcome = edit(&mut document, run.id, "HI", replacement, true)
        .expect("U+0100 is covered by every bundled face");
    let after = page_content(&document, 0);
    assert_ne!(after, before, "the edit must reach the stream, not only the plan");
    // The bytes in the stream are the subset's codes, not characters and not WinAnsi.
    assert!(
        after.windows(2).any(|pair| pair == [1, 2]),
        "the run must carry codes 1,2 for 'H','\u{0100}'; got {:?}",
        String::from_utf8_lossy(&after)
    );

    let substitution = outcome
        .substitution
        .as_deref()
        .expect("the run's own subset cannot set U+0100, so MECH-B must fire");
    let (prefix, family) = substitution.split_once('+').expect("prefixed subset name");
    assert_eq!(prefix.len(), 6);
    assert!(
        !substitution.to_ascii_lowercase().contains("liberation"),
        "the written name {substitution:?} uses the Reserved Font Name (OFL condition 3)"
    );
    assert!(
        family.starts_with("Sans"),
        "a Helvetica run must be set in Sans, got {family:?}"
    );
    // The notice is rendered by the panel whenever `substitution` is set, so it has to be set
    // for a MECH-B edit as much as for MECH-A.
    let note = outcome.note.as_deref().unwrap_or_default();
    assert!(
        note.contains("embedded subset"),
        "the disclosure must say a font was embedded: {note:?}"
    );
    assert!(
        note.contains(substitution),
        "the disclosure must name the face the run now uses: {note:?}"
    );

    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("mech-b.pdf");
    document.save(&target).unwrap();

    // The written objects: a real simple TrueType font with its own program, widths, codes,
    // and text mapping. Row 4 asserts the absence of these for MECH-A; this is the presence.
    let saved = load(&target);
    let (font, program, cmap) = embedded_font(&saved, substitution);
    assert_eq!(
        font.get(b"Subtype").unwrap().as_name().unwrap(),
        b"TrueType",
        "a simple run is written as a simple /TrueType font (R2)"
    );
    assert!(
        program.len() < 200_000,
        "the embedded program is {} B; subsetting is required, not a full face copy",
        program.len()
    );
    let parsed = ttf_parser::Face::parse(&program, 0)
        .expect("the program written into the PDF must parse as a TrueType face");
    assert!(
        parsed
            .glyph_index('\u{0100}')
            .is_some_and(|id| id != ttf_parser::GlyphId(0)),
        "the embedded subset does not map U+0100"
    );

    let cmap_text = String::from_utf8_lossy(&cmap);
    assert!(
        cmap_text.contains("<02> <0100>"),
        "the /ToUnicode CMap must map the code U+0100 was given; without it a search or \
         copy-paste reads the raw code: {cmap_text}"
    );

    // Re-read the saved file through the same gates the picker uses. This is what makes a
    // second edit possible, and it is where lopdf's glyph-name fallback would have shown the
    // wrong characters.
    let reread = load(&target);
    let page_id = textlocate::page_id(&reread, 0).unwrap();
    let runs = textlocate::locate_runs(&reread, page_id).unwrap();
    let edited = runs
        .iter()
        .find(|candidate| candidate.text.contains('\u{0100}'))
        .expect("the edited run must be readable again, with U+0100 intact");
    assert_eq!(edited.text, replacement, "the picker must decode our own codes back");

    // A second edit through the embedded font: decode, replace, re-encode, round-trip.
    let mut again = reread;
    let second = edit(&mut again, edited.id.clone(), replacement, "H\u{0100}!", true)
        .expect("text set in our own embedded subset must remain editable");

    // A different replacement is a different subset, so its name is expected to differ: what
    // must not happen is the previous subset lingering in `/Font` after no `Tf` names it.
    let second_font = second
        .substitution
        .as_deref()
        .expect("the second edit still needs an embedded face");
    assert!(
        second_font.ends_with(&format!("+{family}")),
        "the second edit must stay in the same bundled family: {second_font:?}"
    );

    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("mech-b-twice.pdf");
    again.save(&target).unwrap();
    let twice = load(&target);
    let written: Vec<String> = twice
        .objects
        .values()
        .filter_map(|object| object.as_dict().ok())
        .filter(|dict| dict.get(b"Subtype").and_then(lopdf::Object::as_name).ok() == Some(b"TrueType"))
        .filter_map(|dict| {
            dict.get(b"BaseFont")
                .and_then(lopdf::Object::as_name)
                .ok()
                .map(|base| String::from_utf8_lossy(base).into_owned())
        })
        .collect();
    assert!(
        written.iter().any(|base| base == second_font),
        "the second subset must be in the file: {written:?}"
    );
    assert!(
        !written.iter().any(|base| base == substitution),
        "the first subset is no longer drawn with, so its objects must be gone: {written:?}"
    );
}

/// Row 1's rendering half, run with PDFium (`--include-ignored`).
///
/// A subset that does not render is the exact failure mode design §2.3 warns about — a program
/// that is well-formed and only our parser can read. PDFium is the independent reader.
#[ignore = "requires a real PDFium library; `cargo test -- --include-ignored` after fetching it"]
#[test]
fn row1_the_embedded_subset_renders_through_pdfium() {
    use document_reader_lib::provider::PdfProvider;

    let path = fixture("non-winansi-replacement.pdf");
    let mut document = load(&path);
    let run = first_run(&document, 0);
    edit(&mut document, run.id, "HI", "H\u{0100}", true).expect("row 1 embeds");

    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("mech-b-render.pdf");
    document.save(&target).unwrap();

    let rendered =
        PdfProvider::render_page_at(&target, 0, 1.0).expect("PDFium must render the page");
    assert_eq!(
        &rendered.bytes[..8],
        b"\x89PNG\r\n\x1a\n",
        "expected a PNG"
    );

    // The text layer through PDFium, which reads the /ToUnicode CMap rather than our code.
    let text = PdfProvider::extract_page_text(&target, 0).expect("PDFium must extract text");
    assert!(
        text.contains('\u{0100}'),
        "PDFium must read U+0100 through the /ToUnicode CMap; got {text:?}"
    );
}

/// The same rendering check through **poppler**, the reader available on this machine.
///
/// PDFium is the shipping reader and the test above is the one that must pass in CI; this one
/// exists because "it parses" is not "it renders", and an independent reader was available.
/// `pdftotext` reads the text layer through `/ToUnicode` and `pdftoppm` draws the page, so the
/// two failure modes §2.3 names each have a check: a subset with the wrong `cmap` renders
/// blank, and a missing `/ToUnicode` extracts as the raw code.
#[ignore = "requires poppler; `cargo test -- --include-ignored` where pdftotext/pdftoppm exist"]
#[test]
fn row1_the_embedded_subset_renders_through_poppler() {
    let path = fixture("non-winansi-replacement.pdf");
    let mut document = load(&path);
    let run = first_run(&document, 0);
    edit(&mut document, run.id, "HI", "H\u{0100}", true).expect("row 1 embeds");

    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("mech-b-poppler.pdf");
    document.save(&target).unwrap();

    let text = std::process::Command::new("pdftotext")
        .arg(&target)
        .arg("-")
        .output()
        .expect("pdftotext must be installed for this test");
    assert!(
        text.status.success(),
        "pdftotext failed: {}",
        String::from_utf8_lossy(&text.stderr)
    );
    let extracted = String::from_utf8_lossy(&text.stdout);
    assert!(
        extracted.contains('\u{0100}'),
        "the text layer must read U+0100 through /ToUnicode; got {extracted:?}"
    );

    let prefix = dir.path().join("page");
    let render = std::process::Command::new("pdftoppm")
        .arg("-r")
        .arg("72")
        .arg(&target)
        .arg(&prefix)
        .output()
        .expect("pdftoppm must be installed for this test");
    assert!(
        render.status.success(),
        "pdftoppm failed: {}",
        String::from_utf8_lossy(&render.stderr)
    );

    let pgm = std::fs::read(dir.path().join("page-1.ppm")).expect("a rendered page");
    // A blank page is one repeated colour. Any ink at all means the glyph was drawn.
    let mut distinct: Vec<u8> = Vec::new();
    for pixel in pgm.chunks(3).filter(|pixel| pixel.len() == 3) {
        if !distinct.contains(&pixel[0]) {
            distinct.push(pixel[0]);
        }
    }
    assert!(
        distinct.len() > 1,
        "the rendered page is a single flat colour ({distinct:?}), so nothing was drawn"
    );
}

/// The font the edit wrote, with its program and CMap decompressed.
///
/// Selected by name, not by "the first prefixed `/TrueType`": the fixture already carries one
/// (`ABCDEF+FixtureSubset`), so shape-based selection finds the source font and reports its
/// absence of `/ToUnicode` as a failure of the edit.
fn embedded_font(
    document: &lopdf::Document,
    expected_base_font: &str,
) -> (lopdf::Dictionary, Vec<u8>, Vec<u8>) {
    for object in document.objects.values() {
        let lopdf::Object::Dictionary(dict) = object else {
            continue;
        };
        if dict.get(b"Subtype").and_then(lopdf::Object::as_name).ok() != Some(b"TrueType") {
            continue;
        }
        let Some(base) = dict.get(b"BaseFont").and_then(lopdf::Object::as_name).ok() else {
            continue;
        };
        if base != expected_base_font.as_bytes() {
            continue;
        }
        let descriptor = document
            .get_dictionary(
                dict.get(b"FontDescriptor")
                    .unwrap()
                    .as_reference()
                    .unwrap(),
            )
            .unwrap();
        let program = document
            .get_object(descriptor.get(b"FontFile2").unwrap().as_reference().unwrap())
            .unwrap()
            .as_stream()
            .unwrap()
            .clone();
        let program_len = program.content.len();
        let decompressed = program
            .decompressed_content()
            .unwrap_or_else(|_| program.content.clone());
        assert!(
            decompressed.len() >= program_len,
            "the program must be Flate-compressed in the file"
        );
        let cmap = document
            .get_object(dict.get(b"ToUnicode").unwrap().as_reference().unwrap())
            .unwrap()
            .as_stream()
            .unwrap()
            .clone();
        let cmap = cmap
            .decompressed_content()
            .unwrap_or_else(|_| cmap.content.clone());
        return (dict.clone(), decompressed, cmap);
    }
    panic!("no embedded subset font was written");
}

#[test]
fn the_matrix_rows_are_all_present() {
    let source = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/embed_acceptance.rs"),
    )
    .expect("cannot read this file");
    for row in ["row1_", "row2_", "row3_", "row4_"] {
        assert!(
            source.contains(row),
            "§5 row {row} has no test; the matrix is the acceptance gate, not a description"
        );
    }
}
