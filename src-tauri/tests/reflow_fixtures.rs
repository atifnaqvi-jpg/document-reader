//! P2.8 §6.2: the committed fixtures row A10 is written against.
//!
//! `two_run_line()` has the right idea and the wrong topology — one line, no `TJ` seam,
//! nothing near the content box — so the row's "and nothing else" half and R3's refusal have
//! nothing to run against. The design names the replacement files in §6.2; these guards read
//! them the way a reviewer opening them would.
//!
//! The shape is asserted on the **parsed operator list**, not on bytes, for the reason R-2a
//! recorded: a `TJ` array closed in the wrong place re-encodes to the same tokens in a
//! different order, so a `contains` check would pass on a stream whose semantics are wrong.

use std::path::{Path, PathBuf};

use document_reader_lib::editor::{afm, textlocate};

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

fn load(name: &str) -> lopdf::Document {
    let path = fixture(name);
    lopdf::Document::load(&path)
        .unwrap_or_else(|e| panic!("could not load {}: {e}", path.display()))
}

/// The fixture names in `docs/P2.8-reflow-DESIGN.md` §6.2, as written.
fn documented_fixtures() -> Vec<String> {
    let text = std::fs::read_to_string(repo_root().join("docs/P2.8-reflow-DESIGN.md"))
        .expect("the P2.8 design doc");
    documented_fixtures_in(&text)
}

/// The parser, pulled out so a crafted sample can exercise it.
///
/// A parser that silently returns nothing would make
/// [`every_fixture_the_reflow_design_names_is_committed`] pass on an empty list — the "absence
/// of evidence read as evidence" shape this programme has produced three times. Hence the
/// floor in the caller and the defect shapes below.
fn documented_fixtures_in(text: &str) -> Vec<String> {
    let Some(section) = text
        .split("### 6.2 ")
        .nth(1)
        .and_then(|rest| rest.split("\n---").next())
    else {
        return Vec::new();
    };

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

/// The parser sees a correct table, a table whose names are not backticked, and a document
/// whose §6.2 heading was renamed. The third is the defect shape: without it, deleting the
/// section would make the committed-file check pass by finding no names at all.
#[test]
fn the_fixture_table_parser_sees_its_own_source_and_the_defect_shape() {
    let good = "## 6. Acceptance\n\n### 6.2 The fixture\n\n| Fixture | Shape |\n|---|---|\n\
                | `reflow-line.pdf` | one line |\n| `reflow-line-near-edge.pdf` | near |\n\n---\n\n## 7.\n";
    assert_eq!(
        documented_fixtures_in(good),
        vec!["reflow-line.pdf", "reflow-line-near-edge.pdf"]
    );

    // Not backticked: the design's convention is that a fixture is named in a code span. An
    // unquoted name is prose, and the table's cells hold prose too ("to be written"), so
    // matching those would report the wrong files.
    let unquoted =
        "### 6.2 The fixture\n\n| Fixture | Shape |\n|---|---|\n| reflow-line.pdf | one |\n\n---\n";
    assert!(documented_fixtures_in(unquoted).is_empty());

    // The defect shape: the heading was renumbered, which is how a section silently stops
    // being parsed. The caller's floor is what turns this into a failure rather than a green
    // run over nothing.
    let renamed = "### 6.3 The fixture\n\n| `reflow-line.pdf` | one |\n\n---\n";
    assert!(documented_fixtures_in(renamed).is_empty());
}

/// A design doc that names a file nobody generated describes an intention, which is the same
/// defect class as an acceptance table naming a test nobody runs.
#[test]
fn every_fixture_the_reflow_design_names_is_committed() {
    let named = documented_fixtures();
    assert!(
        named.len() >= 2,
        "§6.2's fixture table parsed to {named:?}; the table's shape changed and the check is blind"
    );

    let missing: Vec<&String> = named
        .iter()
        .filter(|name| !fixture(name).exists())
        .collect();
    assert!(
        missing.is_empty(),
        "§6.2 names fixtures that are not committed: {missing:?}"
    );
}

/// The line's own operations, so a guard can say where each one sits.
struct Line {
    /// `Tf` size in force when line 1's array was shown.
    size: f32,
    /// The `Td` x that started line 1.
    start_x: f32,
    /// The `TJ` arrays, outermost first.
    arrays: Vec<Vec<lopdf::Object>>,
    /// Operator names, for the "the move must insert" property.
    operators: Vec<String>,
}

fn line(document: &lopdf::Document) -> Line {
    let page_id = textlocate::page_id(document, 0).expect("page 0");
    let content = textlocate::decoded_content(document, page_id).expect("page content decodes");

    let operators: Vec<String> = content
        .operations
        .iter()
        .map(|operation| operation.operator.clone())
        .collect();
    let size = content
        .operations
        .iter()
        .find(|operation| operation.operator == "Tf")
        .and_then(|operation| operation.operands.get(1))
        .and_then(|operand| operand.as_float().ok())
        .expect("the fixture sets a text size");
    let start_x = content
        .operations
        .iter()
        .find(|operation| operation.operator == "Td")
        .and_then(|operation| operation.operands.first())
        .and_then(|operand| operand.as_float().ok())
        .expect("line 1 is started by a Td");
    let arrays = content
        .operations
        .iter()
        .filter(|operation| operation.operator == "TJ")
        .filter_map(|operation| operation.operands.first())
        .filter_map(|operand| operand.as_array().ok())
        .map(|items| items.to_vec())
        .collect();

    Line {
        size,
        start_x,
        arrays,
        operators,
    }
}

/// The four properties §6.2 lists, each of which the old fixture lacks.
#[test]
fn the_reflow_fixture_carries_the_a10_topology() {
    let document = load("reflow-line.pdf");
    let line = line(&document);

    // (1) Two runs on one baseline sharing a single `Td`, so the move must **insert** a
    // positioning operator rather than edit one. A second `Td` would make the write a no-op
    // edit and the row would pass without exercising insertion at all.
    assert_eq!(
        line.operators.iter().filter(|name| *name == "Td").count(),
        1,
        "line 1 must be started by exactly one Td, or moving its tail edits an existing \
         operator instead of inserting one: {:?}",
        line.operators
    );

    // (3) A `TJ` seam: a number between two strings, which is the only way spacing enters a
    // line. `two_run_line()` uses two `Tj` with no number at all.
    assert_eq!(
        line.arrays.len(),
        2,
        "the fixture must carry two TJ arrays (one per line), found {:?}",
        line.arrays
    );
    let first = &line.arrays[0];
    assert_eq!(
        first.len(),
        3,
        "line 1 must be `[string, number, string]` — a seam — found {first:?}"
    );
    assert!(
        matches!(first[0], lopdf::Object::String(_, _))
            && first[1].as_float().is_ok()
            && matches!(first[2], lopdf::Object::String(_, _)),
        "the seam must be a number between two strings, found {first:?}"
    );

    // (2) A second line below, positioned by its own `Tm`, so it cannot inherit line 1's pen.
    assert_eq!(
        line.operators.iter().filter(|name| *name == "Tm").count(),
        1,
        "line 2 must be placed by exactly one Tm: {:?}",
        line.operators
    );

    let page_id = textlocate::page_id(&document, 0).unwrap();
    let runs = textlocate::locate_runs(&document, page_id).unwrap();
    let texts: Vec<&str> = runs.iter().map(|run| run.text.as_str()).collect();
    assert_eq!(
        texts,
        vec!["ALPHA ", "BETA", "WXYZ"],
        "the fixture must carry the edited line's two fragments and one unedited line"
    );
    assert!(
        (runs[0].baseline() - runs[1].baseline()).abs() < 1e-3,
        "the two fragments must share a baseline or the tail is never moved"
    );
    assert!(
        (runs[1].baseline() - runs[2].baseline()).abs() > 1.0,
        "line 2 must sit on a different baseline, or 'nothing else moved' has nothing to prove"
    );
}

/// (4) The near-edge variant, and the measured room it leaves.
///
/// Computed from the fixture's own `/MediaBox` and the AFM widths rather than from a
/// transcribed number: a variant that drifted away from the edge would leave R3's refusal
/// unreachable while every test still passed.
#[test]
fn the_near_edge_variant_has_almost_no_room_left() {
    let room = |name: &str| -> f32 {
        let document = load(name);
        let line = line(&document);
        let page_id = textlocate::page_id(&document, 0).unwrap();
        let media = document
            .get_dictionary(page_id)
            .expect("page dictionary")
            .get(b"MediaBox")
            .expect("the fixture states a MediaBox")
            .as_array()
            .expect("MediaBox is an array")
            .get(2)
            .and_then(|operand| operand.as_float().ok())
            .expect("MediaBox has a right edge");
        let width = afm::text_width("Helvetica", "ALPHA ", line.size)
            .expect("Helvetica covers ALPHA")
            + afm::text_width("Helvetica", "BETA", line.size).expect("Helvetica covers BETA");
        let right = line.start_x + width + 120.0 / 1000.0 * line.size;
        media - right
    };

    let ordinary = room("reflow-line.pdf");
    assert!(
        ordinary > 100.0,
        "the ordinary fixture must leave room for a wider replacement, {ordinary} pt left"
    );

    let near = room("reflow-line-near-edge.pdf");
    assert!(
        (0.0..=20.0).contains(&near),
        "the near-edge fixture must leave less than one replacement's worth of room, \
         {near} pt left"
    );
}
