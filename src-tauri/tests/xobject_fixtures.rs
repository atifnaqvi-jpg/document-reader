//! P2.7.3 §8: the **committed** Form XObject fixtures, read from disk.
//!
//! Every X1–X3 test builds its document in memory, which proves the mechanisms but not that
//! the artefacts the design promises exist or carry the shape it claims. A committed fixture
//! that drifted from its builder — or was never generated — leaves the phase's fixture matrix
//! describing an intention, the same defect class the doc guard exists for.
//!
//! So these read the files the way a reviewer opening them would, and assert only the shape
//! the §8 table states. The mechanisms themselves stay where they are tested, in `textedit`
//! and `textlocate`.

use std::path::{Path, PathBuf};

use document_reader_lib::editor::textedit;
use document_reader_lib::editor::textlocate::{self, RawRun, XObjectRefusal};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("src-tauri must have a parent directory")
        .to_path_buf()
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

/// The fixture names in the design's §8 table, as written.
fn documented_fixtures() -> Vec<String> {
    let text = std::fs::read_to_string(repo_root().join("docs/P2.7.3-xobject-DESIGN.md"))
        .expect("the P2.7.3 design doc");
    let section = text
        .split("## 8. Tests")
        .nth(1)
        .and_then(|rest| rest.split("\n## ").next())
        .expect("the design doc must still have an §8 fixture table");

    let mut names = Vec::new();
    for line in section.lines() {
        let Some(first) = line.strip_prefix('|') else {
            continue;
        };
        let cell = first.split('|').next().unwrap_or("").trim();
        if let Some(name) = cell.strip_prefix('`').and_then(|t| t.strip_suffix('`')) {
            if name.ends_with(".pdf") {
                names.push(name.to_string());
            }
        }
    }
    names
}

/// §8 promises seven fixtures. A design doc that names a file nobody generated is the same
/// defect as an acceptance table naming a test nobody runs: the contract describes an
/// intention. The table must also stay in step with the generator, so the list is compared
/// with what is on disk rather than merely counted.
#[test]
fn every_fixture_the_design_doc_names_is_committed() {
    let named = documented_fixtures();
    assert!(
        named.len() >= 7,
        "§8's fixture table parsed to {named:?}; the table's shape changed and the check is blind"
    );

    let missing: Vec<&String> = named
        .iter()
        .filter(|name| !fixture(name).exists())
        .collect();
    assert!(
        missing.is_empty(),
        "§8 names fixtures that are not committed: {missing:?}"
    );
}

fn load(name: &str) -> lopdf::Document {
    let path = fixture(name);
    lopdf::Document::load(&path)
        .unwrap_or_else(|e| panic!("could not load {}: {e}", path.display()))
}

fn runs(document: &lopdf::Document, page: u32) -> Vec<RawRun> {
    let page_id = textlocate::page_id(document, page).unwrap();
    textlocate::locate_runs(document, page_id).unwrap()
}

fn path_names(run: &RawRun) -> Vec<String> {
    run.id
        .xobject_path
        .iter()
        .map(|step| String::from_utf8_lossy(&step.name).into_owned())
        .collect()
}

/// One `Do` operand name per page, so a test can say which form a draw resolved to.
fn page_draws(document: &lopdf::Document, page: u32) -> Vec<String> {
    let page_id = textlocate::page_id(document, page).unwrap();
    textlocate::decoded_content(document, page_id)
        .unwrap()
        .operations
        .iter()
        .filter(|operation| operation.operator == "Do")
        .filter_map(|operation| operation.operands.first())
        .filter_map(|operand| operand.as_name().ok())
        .map(|name| String::from_utf8_lossy(name).into_owned())
        .collect()
}

/// The `/Resources` reference a form dictionary holds, as written. `xobject-shared-resources`
/// is the fixture whose point is that two forms name the *same* one.
fn form_resources_reference(
    document: &lopdf::Document,
    page: u32,
    name: &str,
) -> Option<lopdf::ObjectId> {
    let page_id = textlocate::page_id(document, page).unwrap();
    let (inline, referenced) = document.get_page_resources(page_id).unwrap();
    let mut dictionaries: Vec<&lopdf::Dictionary> = inline.into_iter().collect();
    dictionaries.extend(
        referenced
            .iter()
            .filter_map(|id| document.get_dictionary(*id).ok()),
    );
    let form = dictionaries.iter().find_map(|resources| {
        let xobjects = resources
            .get(b"XObject")
            .ok()
            .and_then(|entry| document.dereference(entry).ok())
            .and_then(|(_, object)| object.as_dict().ok())?;
        xobjects
            .get(name.as_bytes())
            .ok()
            .and_then(|object| document.dereference(object).ok())
            .and_then(|(_, object)| object.as_stream().ok())
    })?;
    form.dict.get(b"Resources").ok()?.as_reference().ok()
}

/// `xobject-single` and `xobject-nested`: reachability, and the depth the walk must descend.
#[test]
fn the_single_and_nested_fixtures_locate_their_text_at_the_documented_depth() {
    let single = load("xobject-single.pdf");
    let run = runs(&single, 0)
        .into_iter()
        .find(|run| run.text == "ALPHA")
        .expect("xobject-single.pdf draws ALPHA");
    assert_eq!(path_names(&run), vec!["X1".to_string()]);

    let nested = load("xobject-nested.pdf");
    let run = runs(&nested, 0)
        .into_iter()
        .find(|run| run.text == "ALPHA")
        .expect("xobject-nested.pdf draws ALPHA two levels down");
    assert_eq!(path_names(&run), vec!["X1".to_string(), "X2".to_string()]);
}

/// `xobject-two-draws` and `xobject-two-pages`: the two shapes Q2's per-draw unit is about.
#[test]
fn the_two_draw_and_two_page_fixtures_repeat_the_same_form() {
    let draws = load("xobject-two-draws.pdf");
    assert_eq!(
        page_draws(&draws, 0),
        vec!["X1".to_string(), "X1".to_string()]
    );
    let located: Vec<usize> = runs(&draws, 0)
        .iter()
        .filter(|run| run.text == "ALPHA")
        .map(|run| run.id.xobject_path[0].op_index)
        .collect();
    assert_eq!(
        located,
        vec![0, 2],
        "one run per draw, at different Do indices"
    );

    let pages = load("xobject-two-pages.pdf");
    for page in 0..2 {
        assert_eq!(page_draws(&pages, page), vec!["X1".to_string()]);
        assert_eq!(
            runs(&pages, page)
                .iter()
                .filter(|run| run.text == "ALPHA")
                .count(),
            1,
            "page {page} draws the shared form"
        );
    }
}

/// `xobject-cycle` and `xobject-depth-9`: both halves of Q1 — the cycle, and the cap that
/// still leaves depth 8 editable.
#[test]
fn the_cycle_and_depth_fixtures_refuse_visibly_at_the_documented_boundary() {
    let cycle = load("xobject-cycle.pdf");
    let placeholder = runs(&cycle, 0)
        .into_iter()
        .find(|run| run.text == "/X1")
        .expect("the undescendable Do still reports itself");
    assert_eq!(placeholder.xobject_refusal, Some(XObjectRefusal::Cycle));

    let deep = load("xobject-depth-9.pdf");
    let located = runs(&deep, 0);
    let edge = located
        .iter()
        .find(|run| run.text == "EDGE")
        .expect("depth 8 is within the cap");
    assert_eq!(edge.id.xobject_path.len(), 8);
    let refused = located
        .iter()
        .find(|run| run.text == "/X9")
        .expect("the ninth level still reports itself");
    assert_eq!(refused.xobject_refusal, Some(XObjectRefusal::Depth));

    // The refusal reaches the picker as a gate, not as a silent gap.
    let gates = textedit::resolve_gates(&deep, 0, refused).unwrap();
    assert_eq!(
        textedit::font_admissibility(&gates).gate.as_deref(),
        Some("nested-xobject")
    );
}

/// `xobject-shared-resources`: the one fixture whose *point* is ownership, so it is asserted
/// on the object identity rather than on text.
#[test]
fn the_shared_resources_fixture_gives_both_forms_one_indirect_dictionary() {
    let document = load("xobject-shared-resources.pdf");
    let x1 = form_resources_reference(&document, 0, "X1").expect("X1 names an indirect /Resources");
    let x2 = form_resources_reference(&document, 0, "X2").expect("X2 names an indirect /Resources");
    assert_eq!(x1, x2, "both forms read the same resources object");
    assert!(
        runs(&document, 0).iter().any(|run| run.text == "ALPHA"),
        "X1's text is reachable through the shared dictionary"
    );
}

/// The committed artefact must support the edit, not just the read: a draw is copied, saved,
/// and re-read. This is the only test in the suite that goes through `Document::save` for a
/// form edit, which is where a resource name that `renumber_objects` invalidated would show.
#[test]
fn editing_the_committed_two_draw_fixture_saves_a_reloadable_clone() {
    let mut document = load("xobject-two-draws.pdf");
    let run = runs(&document, 0)
        .into_iter()
        .filter(|run| run.text == "ALPHA")
        .max_by_key(|run| run.id.xobject_path[0].op_index)
        .expect("both draws are located");

    let measure = |_: &str| Ok(60.0f32);
    let request = textedit::EditRequest {
        page: 0,
        run_id: run.id.clone(),
        expected: run.text.clone(),
        replacement: "BRAVO".to_string(),
        old_width: 60.0,
        font_size: 24.0,
        allow_substitution: true,
        fragments: Vec::new(),
        range: Vec::new(),
        measure: &measure,
    };
    textedit::edit(&mut document, &request).expect("a form run in a committed fixture is editable");

    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("edited.pdf");
    document.save(&target).unwrap();

    let reread = lopdf::Document::load(&target).unwrap();
    assert_eq!(
        page_draws(&reread, 0),
        vec!["X1".to_string(), "X1_edit1".to_string()],
        "the clicked draw survived the save pointing at its clone"
    );
    let edited = runs(&reread, 0);
    assert_eq!(
        edited.iter().filter(|run| run.text == "ALPHA").count(),
        1,
        "the untouched draw still reads ALPHA"
    );
    assert_eq!(
        edited.iter().filter(|run| run.text == "BRAVO").count(),
        1,
        "the edited draw reads BRAVO"
    );

    // And the fixture on disk is not what changed.
    let original = load("xobject-two-draws.pdf");
    assert_eq!(
        runs(&original, 0)
            .iter()
            .filter(|run| run.text == "ALPHA")
            .count(),
        2
    );
}

// --- P2.7.7: the PDFium-side walk --------------------------------------------
//
// These are the tests `xobject_fixtures.rs` was blind to: every assertion above reads the
// fixtures through **lopdf**, so a production picker that saw no runs at all still passed.
// These read the same files through PDFium, the reader the picker actually uses.

use document_reader_lib::editor::runmetrics;
use document_reader_lib::provider::DocumentHandles;

fn pdfium_walk(name: &str) -> runmetrics::PageRuns {
    let path = fixture(name);
    DocumentHandles::new()
        .with_document(&path, None, |document| {
            runmetrics::page_runs_walked(document, 0, &path)
        })
        .expect("PDFium walk failed")
}

/// The form's child bounds are in the form's space, so the second draw's text must land where
/// the second draw is, not where the first one is.
#[test]
#[ignore = "requires a real PDFium library; `cargo test -- --include-ignored` after fetching it"]
fn the_recursive_walk_maps_a_form_box_into_page_space() {
    let walked = pdfium_walk("xobject-two-draws.pdf");
    assert_eq!(walked.runs.len(), 2, "both draws carry text: {walked:?}");
    let first = &walked.runs[0];
    let second = &walked.runs[1];
    assert_eq!(first.text.trim(), "ALPHA");
    assert_eq!(second.text.trim(), "ALPHA");
    // The second draw is translated by `1 0 0 1 0 -100 cm`. PDFium reports both children at
    // the same y, so a walk that emits the child box unchanged makes these two identical.
    assert!(
        (first.y0 - second.y0 - 100.0).abs() < 0.01,
        "second draw must sit 100pt below the first: {} vs {}",
        first.y0,
        second.y0
    );
    assert!((first.y0 - 700.0).abs() < 0.5, "first draw keeps its own y");
}

/// `emit-at-visit` and the order `page.text()` reports must agree, or pairing aligns fragments
/// to the wrong character window.
#[test]
#[ignore = "requires a real PDFium library; `cargo test -- --include-ignored` after fetching it"]
fn the_walk_visits_runs_in_the_order_the_page_text_reports_them() {
    let walked = pdfium_walk("xobject-interleaved.pdf");
    let joined: String = walked
        .runs
        .iter()
        .map(|run| run.text.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect::<Vec<_>>()
        .join(" ");
    assert_eq!(
        joined, "BEFORE ALPHA AFTER",
        "a walk that emits a form after its neighbours reorders this: {walked:?}"
    );

    let path = fixture("xobject-interleaved.pdf");
    let text = DocumentHandles::new()
        .with_document(&path, None, |document| {
            Ok::<_, document_reader_lib::model::AppError>(
                document
                    .pages()
                    .get(0)
                    .map_err(|e| document_reader_lib::model::AppError::Other(e.to_string()))?
                    .text()
                    .map_err(|e| document_reader_lib::model::AppError::Other(e.to_string()))?
                    .all()
                    .to_string(),
            )
        })
        .unwrap();
    let rendered: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        rendered.starts_with(&joined),
        "walk order {joined:?} must be a prefix of PDFium's own {rendered:?}"
    );
}

/// The cap is the only termination guard. `xobject-depth-9` must reach the text at depth 8 and
/// refuse the draw one level deeper; a self-drawing form must stop at the cap rather than hang.
#[test]
#[ignore = "requires a real PDFium library; `cargo test -- --include-ignored` after fetching it"]
fn the_walk_stops_at_the_cap_and_a_self_drawing_form_terminates() {
    let deep = pdfium_walk("xobject-depth-9.pdf");
    assert!(
        deep.runs.iter().any(|run| run.text.trim() == "EDGE"),
        "depth 8 is within the cap and must be emitted: {deep:?}"
    );
    assert!(
        !deep.runs.iter().any(|run| run.text.trim() == "ALPHA"),
        "depth 9 is past the cap and must not be emitted"
    );
    assert_eq!(
        deep.refusals,
        vec![document_reader_lib::editor::textlocate::XObjectRefusal::Depth],
        "the cap must report itself as Depth, once"
    );

    // Reaching this assertion at all is the cycle half: the walk returned.
    let cycle = pdfium_walk("xobject-cycle.pdf");
    assert!(cycle.runs.is_empty(), "a self-drawing form has no text");
    assert_eq!(
        cycle.refusals,
        vec![document_reader_lib::editor::textlocate::XObjectRefusal::Depth],
        "a self-drawing form terminates at the cap, not by object identity"
    );
}
