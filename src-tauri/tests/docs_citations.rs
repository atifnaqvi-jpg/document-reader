//! The programme's documents must not cite things that are not there, and must not still be
//! claiming a defect that was fixed.
//!
//! A design doc is the contract for a phase, and its §8 table is how the phase is judged. When
//! that table names a test that was renamed, or a helper that never existed, the doc stops
//! describing the code and starts describing an intention. That is the same class of defect as
//! a stale `ActualText`: plausible on the page, wrong when checked.
//!
//! It has already happened twice in this programme: the loop log records a citation to a test
//! that does not exist, and `shared_actualtext_is_reported_not_rewritten` was renamed to
//! `shared_actualtext_is_copied_before_it_is_rewritten` under ruling B-7 while §8 kept the old
//! name. Both survived review because nothing read the doc against the tree. Hence a test.
//!
//! `P2.7-REVIEW-OPEN-QUESTIONS.md` is an audit: §A quotes superseded identifiers *deliberately*,
//! as the record of what was wrong. Checking those against the tree would flag the record of the
//! bug as the bug, so retracted text is written `~~struck~~` and this guard skips struck spans.
//! Everything outside them is a live claim and is checked.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("src-tauri must have a parent directory")
        .to_path_buf()
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

/// Every `.rs` file under the crate, so a citation can be looked up without git or a build map.
///
/// The guard excludes itself. Otherwise a token it merely *mentions* — such as the superseded
/// `t27_char_spacing_is_in_the_pen_walk` quoted in this header — would count as "present in the
/// tree" and the guard would pass on the strength of its own prose.
fn rust_sources() -> Vec<(PathBuf, String)> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    let mut stack = vec![root.join("src"), root.join("tests")];
    while let Some(path) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&path) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|ext| ext == "rs")
                && path
                    .file_name()
                    .is_some_and(|name| name != "docs_citations.rs")
            {
                let text = read(&path);
                files.push((path, text));
            }
        }
    }
    files
}

/// Every `.rs` stem in the crate, including `examples/`. A bare `x.rs` citation names a file
/// rather than a path, and the fixtures generator lives in `examples/`, so a stem set built from
/// `src/` and `tests/` alone would reject a correct citation.
fn crate_stems() -> BTreeSet<String> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut stems = BTreeSet::new();
    let mut stack = vec![root.join("src"), root.join("tests"), root.join("examples")];
    while let Some(path) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&path) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                if let Some(name) = path.file_name() {
                    stems.insert(name.to_string_lossy().into_owned());
                }
            }
        }
    }
    stems
}

/// Byte spans covered by `~~…~~`, paired outermost-first. An unclosed `~~` yields no span, so a
/// typo cannot silently exempt the rest of a document.
fn strikethrough_spans(text: &str) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut open = None;
    let mut cursor = 0;
    while let Some(found) = text[cursor..].find("~~") {
        let at = cursor + found;
        match open.take() {
            None => open = Some(at),
            Some(start) => spans.push((start, at + 2)),
        }
        cursor = at + 2;
    }
    spans
}

/// What a document says, split by whether it still says it.
#[derive(Default)]
struct Citations {
    /// Tokens in ordinary prose: claims about the tree, and they must resolve.
    live: BTreeSet<String>,
    /// Tokens inside `~~…~~`: retracted, kept only as the audit record.
    retracted: BTreeSet<String>,
}

/// Every backticked token the document contains, split into live and retracted.
fn cited_tokens(text: &str) -> Citations {
    let spans = strikethrough_spans(text);
    let mut found = Citations::default();
    let mut cursor = 0;
    while let Some(open) = text[cursor..].find('`') {
        let at = cursor + open;
        let after = &text[at + 1..];
        let Some(close) = after.find('`') else { break };
        let token = &after[..close];
        cursor = at + 1 + close + 1;
        let struck = spans.iter().any(|(start, end)| at >= *start && at < *end);
        if struck {
            found.retracted.insert(token.to_string());
        } else {
            found.live.insert(token.to_string());
        }
    }
    found
}

/// Backticked tokens that look like a Rust identifier or a test name: snake_case with at least
/// `min_segments` underscore-separated segments. Two segments are enough for the resolution
/// check because they are as often real functions (`split_at`, `save_edits`) as file stems; the
/// §8 check needs three, because its first cells also name test files (`textedit_gates`).
fn cited_identifiers(text: &str, min_segments: usize) -> Citations {
    let all = cited_tokens(text);
    let keep = |token: &str| {
        let mut segments = token.split('_');
        if segments.clone().count() < min_segments {
            return false;
        }
        if !token
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        {
            return false;
        }
        segments.next().is_some_and(|s| !s.is_empty()) && !token.ends_with('_')
    };
    Citations {
        live: all.live.into_iter().filter(|t| keep(t)).collect(),
        retracted: all.retracted.into_iter().filter(|t| keep(t)).collect(),
    }
}

/// Backticked repository paths: a promise that a file sits at that path. A bare `x.rs` is a
/// stem rather than a path, so it only has to name a file somewhere in the crate.
fn cited_paths(text: &str) -> Citations {
    let all = cited_tokens(text);
    let keep = |token: &str| {
        token.starts_with("docs/") || token.starts_with("src-tauri/") || token.ends_with(".rs")
    };
    Citations {
        live: all.live.into_iter().filter(|t| keep(t)).collect(),
        retracted: all.retracted.into_iter().filter(|t| keep(t)).collect(),
    }
}

/// The documents that make claims about the shipped body and the programme: the phase contract,
/// the audit that supersedes the plan, the plan itself, the review whose §A is the record, and
/// every P3 phase design.
///
/// A P3 design is governed **from the moment it is written, not from the moment it is
/// implemented**. Its decisions table is the contract a later phase is judged against, and its
/// §1 facts are claims about this tree; a design that cites a module it intends to create, or a
/// crate API that moved, is wrong before a line of code exists. `docs/P3.1-annotations-DESIGN.md`
/// is the case in point: writing it turned up that `docs/P2-DESIGN.md` F1 had overstated the
/// `pub(crate)` boundary, which is exactly the drift this list is here to catch.
const GOVERNED_DOCS: [&str; 9] = [
    "docs/P3-rename-housekeeping-DESIGN.md",
    "docs/P3.1-annotations-DESIGN.md",
    "docs/P2.7.1-mcid-DESIGN.md",
    "docs/P2.7.4-transform-DESIGN.md",
    "docs/P2.7.3-xobject-DESIGN.md",
    "docs/P2.7-STOCK-TAKE.md",
    "docs/P2.7-REVIEW-OPEN-QUESTIONS.md",
    "docs/P2.7-TEXT-EDITING-PARITY-PLAN.md",
    "docs/P2.8-reflow-DESIGN.md",
];

#[test]
fn docs_cite_only_identifiers_that_exist() {
    let root = repo_root();
    let sources = rust_sources();
    let mut unresolved = Vec::new();

    for doc in GOVERNED_DOCS {
        let text = read(&root.join(doc));
        for token in cited_identifiers(&text, 2).live {
            let present = sources.iter().any(|(_, source)| source.contains(&token))
                || root
                    .join("src-tauri/tests")
                    .join(format!("{token}.rs"))
                    .exists()
                || root
                    .join("src-tauri/examples")
                    .join(format!("{token}.rs"))
                    .exists();
            if !present {
                unresolved.push(format!("{doc}: `{token}`"));
            }
        }
    }

    assert!(
        unresolved.is_empty(),
        "these documents cite identifiers the tree does not contain, so they describe an \
         intention rather than the code (a deliberately quoted defect belongs inside ~~…~~):\n  {}",
        unresolved.join("\n  ")
    );
}

/// A `file.rs:901` pointer is a citation that cannot be checked and is wrong after the next
/// edit that adds or removes a line above it. Both such pointers in these documents had already
/// drifted (one landed on `.first()`, the other on a struct field), which is exactly what makes
/// them worse than no pointer at all: they look verifiable.
#[test]
fn governed_docs_point_at_symbols_not_line_numbers() {
    let root = repo_root();
    let mut offenders = Vec::new();
    for doc in GOVERNED_DOCS {
        let text = read(&root.join(doc));
        for (index, line) in text.lines().enumerate() {
            if let Some(at) = line.find(".rs:") {
                let numbered = line[at + 4..].starts_with(|c: char| c.is_ascii_digit());
                if numbered {
                    offenders.push(format!("{doc}:{} — {}", index + 1, line.trim()));
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "cite a symbol instead of a line number; a line number is stale by construction:\n  {}",
        offenders.join("\n  ")
    );
}

/// A backticked `docs/….md` is a promise that the file is at that path. Two of these documents
/// are the record of a rename (ruling B-1 turned `P2.7-ADOBE-PARITY-PLAN.md` into
/// `P2.7-TEXT-EDITING-PARITY-PLAN.md`), so the old name survives only where it is struck.
#[test]
fn governed_docs_cite_only_paths_that_exist() {
    let root = repo_root();
    let stem_names = crate_stems();
    let mut missing = Vec::new();
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for doc in GOVERNED_DOCS {
        let text = read(&root.join(doc));
        for path in cited_paths(&text).live {
            let present = if path.contains('/') {
                // Paths in a crate-level design doc are written against the crate root
                // (`examples/x.rs`) as often as against the repository root (`docs/y.md`).
                root.join(&path).exists() || crate_root.join(&path).exists()
            } else {
                stem_names.contains(&path)
            };
            if !present {
                missing.push(format!("{doc}: `{path}`"));
            }
        }
    }
    assert!(
        missing.is_empty(),
        "these documents promise a file at a path that does not hold one:\n  {}",
        missing.join("\n  ")
    );
}

/// A bare `x.rs` citation is resolved against the stem set, and the design docs cite the
/// fixture generator without a directory. If the scan skipped `examples/`, that citation would
/// be reported as promising a file that does not exist — a false positive that invites widening
/// the exemption instead of the scan.
#[test]
fn a_bare_stem_from_the_examples_directory_resolves() {
    let stems = crate_stems();
    assert!(
        stems.contains("make_p24_fixtures.rs"),
        "the stem set must cover `examples/`, or a correct citation is reported missing; it holds \
         {} stems",
        stems.len()
    );
}

/// §A of the review is a table of defects. Each row must say what became of its defect: fixed,
/// closed, superseded, open, or retracted. A row left in the present tense ("Claims `Ts` is
/// tracked") reads as a live defect long after the fix shipped, which is how the stock take kept
/// reporting shipped work as outstanding.
#[test]
fn every_recorded_defect_states_its_disposition() {
    const MARKERS: [&str; 5] = [
        "**Fixed",
        "**Closed",
        "**Open",
        "**Superseded",
        "**Retracted",
    ];

    let text = read(&repo_root().join("docs/P2.7-REVIEW-OPEN-QUESTIONS.md"));
    let section = text
        .split("## A.")
        .nth(1)
        .and_then(|rest| rest.split("\n## ").next())
        .expect("the review must still have an §A defect table");

    let mut rows = 0;
    let mut silent = Vec::new();
    for line in section.lines() {
        let Some(rest) = line.strip_prefix("| A-") else {
            continue;
        };
        rows += 1;
        let id = format!("A-{}", rest.split('|').next().unwrap_or("?").trim());
        let problem = line.split('|').nth(3).unwrap_or("");
        if !MARKERS.iter().any(|marker| problem.contains(marker)) && !line.contains("~~") {
            silent.push(format!("{id}: {}", problem.trim()));
        }
    }

    assert!(
        rows > 0,
        "§A parsing found no defect rows; the table's shape changed and this guard is now blind"
    );
    assert!(
        silent.is_empty(),
        "these §A rows do not say whether their defect is fixed, so they read as live \
         defects:\n  {}",
        silent.join("\n  ")
    );
}

/// The strikethrough exemption is the whole basis for checking the audit's live claims, so it is
/// tested directly rather than inferred from the documents. A matcher that exempted everything
/// would make the citation test pass while checking nothing; one that exempted nothing would
/// flag the record of the bug as the bug.
#[test]
fn a_struck_citation_is_retracted_but_an_unstruck_one_is_live() {
    let retracted = cited_identifiers("~~`t27_char_spacing_is_in_the_pen_walk`~~", 3);
    assert!(
        retracted.live.is_empty(),
        "struck text is retracted, not claimed: {:?}",
        retracted.live
    );
    assert_eq!(retracted.retracted.len(), 1);

    let both = cited_identifiers(
        "~~`t27_char_spacing_is_in_the_pen_walk`~~ replaced by \
         `char_spacing_advances_the_tj_pen`",
        3,
    );
    assert!(both.live.contains("char_spacing_advances_the_tj_pen"));
    assert!(!both.live.contains("t27_char_spacing_is_in_the_pen_walk"));

    let unclosed = cited_identifiers("~~`t27_char_spacing_is_in_the_pen_walk`", 3);
    assert!(unclosed
        .live
        .contains("t27_char_spacing_is_in_the_pen_walk"));
}

/// §8 of the design doc is the phase's acceptance table. Every test it names has to be a test
/// that runs, otherwise the phase can "pass" a table nobody executes.
///
/// Each phase doc has its own acceptance table, and both tables are the contract for their
/// phase, so both are checked.
#[test]
fn the_design_doc_acceptance_table_names_tests_that_exist() {
    let root = repo_root();
    let sources = rust_sources();
    let mut named: Vec<String> = Vec::new();
    let mut missing: Vec<String> = Vec::new();

    for (doc, section_head, section_end) in [
        (
            "docs/P2.7.1-mcid-DESIGN.md",
            "## 8. Acceptance tests",
            "## 9.",
        ),
        ("docs/P2.7.3-xobject-DESIGN.md", "## 8. Tests", "\n## "),
    ] {
        let text = read(&root.join(doc));
        let section = text
            .split(section_head)
            .nth(1)
            .and_then(|rest| rest.split(section_end).next())
            .unwrap_or_else(|| panic!("{doc} must still have an §8 acceptance table"));

        let mut found_here = 0;
        for line in section.lines() {
            let Some(first_cell) = line.strip_prefix('|') else {
                continue;
            };
            let first_cell = first_cell.split('|').next().unwrap_or("");
            for token in cited_identifiers(first_cell, 3).live {
                let is_test = sources
                    .iter()
                    .any(|(_, source)| source.contains(&format!("fn {token}")));
                if !is_test {
                    missing.push(format!("{doc}: `{token}`"));
                }
                named.push(token);
                found_here += 1;
            }
        }
        assert!(
            found_here > 0,
            "{doc} §8 parsing found no test names; the table's shape changed and the guard is blind"
        );
    }

    assert!(
        missing.is_empty(),
        "§8 names tests that do not exist: {missing:?} (it names {named:?})"
    );
}

/// The stock take says it supersedes the parity plan, so its status rows are the authority on
/// what shipped. `Ts` is the row that went stale: it was written before T-1 landed and still
/// says the rise is absent, while `TextMatrixWalk` handles it.
#[test]
fn the_stock_take_agrees_with_the_code_on_text_rise() {
    let sources = rust_sources();
    let handles_rise = sources
        .iter()
        .any(|(path, source)| path.ends_with("textlocate.rs") && source.contains("\"Ts\" =>"));

    let stock_take = read(&repo_root().join("docs/P2.7-STOCK-TAKE.md"));
    let claims_absent = stock_take.lines().any(|line| {
        let mentions_rise = line.contains("`Ts`");
        let claims_missing = [("**Not done**"), ("**Missing**"), ("→ absent")]
            .iter()
            .any(|marker| line.contains(marker));
        mentions_rise && claims_missing
    });

    assert!(
        !(handles_rise && claims_absent),
        "the stock take claims `Ts` is absent while `TextMatrixWalk` handles it; an audit that \
         is older than the code it audits is how a shipped gap stays on the list"
    );
}
