//! The P2.7.1 body's documents must not cite things that are not there.
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
//! Deliberately not covered: `P2.7-REVIEW-OPEN-QUESTIONS.md`, whose §A is an audit that
//! *quotes* stale names as defects (`t27_char_spacing_is_in_the_pen_walk`). Rewriting those
//! would erase the record of what was wrong.

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
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                let text = read(&path);
                files.push((path, text));
            }
        }
    }
    files
}

/// Backticked tokens that look like a Rust identifier or a test name: snake_case with at least
/// two underscores. Two-segment tokens are excluded because they are just as often file stems
/// (`textedit_gates`) or `Tc`-style operator names as they are identifiers.
fn cited_identifiers(text: &str) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    let mut rest = text;
    while let Some(open) = rest.find('`') {
        let after = &rest[open + 1..];
        let Some(close) = after.find('`') else { break };
        let token = &after[..close];
        rest = &after[close + 1..];
        let mut segments = token.split('_');
        if segments.clone().count() < 3 {
            continue;
        }
        if !token
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        {
            continue;
        }
        if segments.next().map_or(true, |s| s.is_empty()) || token.ends_with('_') {
            continue;
        }
        found.insert(token.to_string());
    }
    found
}

/// The documents that make present-tense claims about the shipped body: the phase contract and
/// the audit that claims to supersede the plan.
const BODY_DOCS: [&str; 2] = ["docs/P2.7.1-mcid-DESIGN.md", "docs/P2.7-STOCK-TAKE.md"];

#[test]
fn docs_cite_only_identifiers_that_exist() {
    let root = repo_root();
    let sources = rust_sources();
    let mut unresolved = Vec::new();

    for doc in BODY_DOCS {
        let text = read(&root.join(doc));
        for token in cited_identifiers(&text) {
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
         intention rather than the code:\n  {}",
        unresolved.join("\n  ")
    );
}

/// §8 of the design doc is the phase's acceptance table. Every test it names has to be a test
/// that runs, otherwise the phase can "pass" a table nobody executes.
#[test]
fn the_design_doc_acceptance_table_names_tests_that_exist() {
    let text = read(&repo_root().join("docs/P2.7.1-mcid-DESIGN.md"));
    let section = text
        .split("## 8. Acceptance tests")
        .nth(1)
        .and_then(|rest| rest.split("## 9.").next())
        .expect("the design doc must still have an §8 acceptance table");

    let sources = rust_sources();
    let mut named = Vec::new();
    let mut missing = Vec::new();
    for line in section.lines() {
        let Some(first_cell) = line.strip_prefix('|') else {
            continue;
        };
        let first_cell = first_cell.split('|').next().unwrap_or("");
        for token in cited_identifiers(first_cell) {
            named.push(token.clone());
            let is_test = sources
                .iter()
                .any(|(_, source)| source.contains(&format!("fn {token}")));
            if !is_test {
                missing.push(token);
            }
        }
    }

    assert!(
        !named.is_empty(),
        "§8 parsing found no test names; the table's shape changed and this guard is now blind"
    );
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
