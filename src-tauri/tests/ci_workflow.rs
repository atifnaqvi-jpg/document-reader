//! The CI gate is release infrastructure too, and it fails in the quietest way possible: by
//! simply not running.
//!
//! That is not hypothetical here. `on.push.branches` was `[main, master]`, but all of P2.7.x
//! and the CID phase were developed on the long-lived `p2-windows-test` branch, which that
//! filter never matched. The only workflow that ran there was the Windows release build —
//! `cargo test` plus the PDFium assertion, but no `cargo fmt`, no `clippy`, no `vitest` — so
//! three weeks of unformatted code accumulated while every reported "gates green" referred to
//! someone's laptop. A gate that does not run is not green, and nothing about a green check on
//! a different branch tells you that.
//!
//! The second invariant is that the toolchain is pinned. rustfmt is allowed to change its
//! output between releases; with `@stable` the format gate's verdict depends on the date
//! rather than the code, so it can go red on a day nobody edited anything — or reformat a
//! release branch mid-flight. The pin has to agree between CI and `rust-toolchain.toml`, or a
//! local `cargo fmt --check` proves nothing about what CI will say.

use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("src-tauri must have a parent directory")
        .to_path_buf()
}

fn ci_workflow() -> String {
    let path = repo_root().join(".github/workflows/ci.yml");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

fn unquote(s: &str) -> String {
    s.trim().trim_matches(['"', '\'']).to_string()
}

/// The keys directly under each `on.<event>` block at indent 4, and the event names
/// themselves. An event with no child keys (`push:`) is a valid, unrestricted trigger and
/// yields just the event name — which is the shape the gate must have.
fn push_trigger(text: &str) -> (Vec<String>, Vec<String>) {
    let mut events = Vec::new();
    let mut keys = Vec::new();
    let mut top = String::new();
    let mut sub = String::new();
    for raw in text.lines() {
        let line = raw.split('#').next().unwrap_or("");
        if line.trim().is_empty() {
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        let item = line.trim();
        match indent {
            0 => {
                top = item.trim_end_matches(':').to_string();
                sub.clear();
            }
            2 => {
                sub = item.trim_end_matches(':').to_string();
                if top == "on" {
                    events.push(sub.clone());
                }
            }
            4 if top == "on" && sub == "push" => {
                // The key, before any inline value: `branches: [main, master]` must yield
                // `branches`, or the restriction check silently passes.
                keys.push(
                    item.split(':')
                        .next()
                        .unwrap_or("")
                        .trim_end_matches(':')
                        .to_string(),
                );
            }
            _ => {}
        }
    }
    (events, keys)
}

/// The `with.toolchain:` value of each `dtolnay/rust-toolchain` step.
fn toolchain_pins(text: &str) -> Vec<String> {
    let mut pins = Vec::new();
    let mut lines = text.lines().peekable();
    while let Some(raw) = lines.next() {
        let line = raw.split('#').next().unwrap_or("");
        let trimmed = line.trim();
        let Some(idx) = trimmed.find("dtolnay/rust-toolchain@") else {
            continue;
        };
        // The action ref itself, e.g. `@master` or `@stable`.
        pins.push(unquote(&trimmed[idx + "dtolnay/rust-toolchain@".len()..]));
        // Then `with.toolchain:` among the following indented lines.
        while let Some(next) = lines.peek() {
            let next = next.split('#').next().unwrap_or("");
            let next_trimmed = next.trim();
            if next_trimmed.is_empty() {
                lines.next();
                continue;
            }
            if next_trimmed.starts_with("- ") {
                break;
            }
            lines.next();
            if let Some(value) = next_trimmed.strip_prefix("toolchain:") {
                pins.push(unquote(value));
            }
        }
    }
    pins
}

fn rust_toolchain_file() -> String {
    let path = repo_root().join("rust-toolchain.toml");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

#[test]
fn the_gate_runs_on_every_branch() {
    let text = ci_workflow();
    let (events, keys) = push_trigger(&text);
    assert!(
        events.contains(&"push".to_string()),
        "the workflow has no `on.push` trigger at all: {events:?}"
    );
    assert!(
        !keys.contains(&"branches".to_string()) && !keys.contains(&"branches-ignore".to_string()),
        "`on.push` restricts branches ({keys:?}). Every branch that can ship must be gated; \
         a long-lived feature branch is exactly the case this restriction hid"
    );
}

#[test]
fn the_toolchain_is_pinned_and_the_pin_agrees() {
    let ci = ci_workflow();
    let pins = toolchain_pins(&ci);
    assert!(
        !pins.is_empty(),
        "no dtolnay/rust-toolchain step found; the pin check would pass vacuously"
    );
    assert!(
        !pins.iter().any(|p| p == "stable"),
        "CI uses a floating `stable` toolchain ({pins:?}); rustfmt output may then change \
         without any code change"
    );

    let declared = rust_toolchain_file();
    let channel = declared
        .lines()
        .map(|l| l.split('#').next().unwrap_or(""))
        .find_map(|l| l.trim().strip_prefix("channel"))
        .map(|rest| unquote(rest.trim_start_matches([' ', '=', ':'])))
        .expect("rust-toolchain.toml has no `channel`");

    assert!(
        pins.contains(&channel),
        "CI pins {pins:?} but rust-toolchain.toml pins {channel:?}; a local \
         `cargo fmt --check` would then prove nothing about CI"
    );
}

/// A test that reports `ok` without running is worse than no test: the summary cannot tell it
/// apart from a pass. `tests/textedit_gates.rs` used to open 33 of 60 tests with
/// `if !pdfium_ready() { return; }`, which printed a line to *captured* output and then
/// asserted nothing, so the suite said `60 passed; 0 failed; 0 ignored` in a twentieth of its
/// real runtime. A harness that reports work it did not do is the most dangerous kind of
/// green, because every later measurement is taken through it.
///
/// `#[ignore]` was the ruling: it converts an unreported omission into a counted one. This
/// test is the other half -- it fails if a skip arm is reintroduced, so the counting cannot
/// quietly stop. It matches the *shape* of a skip (a helper that greps availability and
/// returns early) rather than one file, because the defect is the shape, not the name.
/// Every `fn` carrying `#[test]`, with its attributes and brace-matched body.
fn test_functions(text: &str) -> Vec<(String, String, String)> {
    let bytes = text.as_bytes();
    let mut found = Vec::new();
    let mut cursor = 0;
    while let Some(offset) = text[cursor..].find("#[test]") {
        let start = cursor + offset;
        let Some(fn_at) = text[start..].find("fn ") else {
            break;
        };
        let fn_at = start + fn_at;
        let name_start = fn_at + 3;
        let name_end = text[name_start..]
            .find(|c: char| !(c.is_alphanumeric() || c == '_'))
            .map(|i| name_start + i)
            .unwrap_or(text.len());
        let Some(brace) = text[name_end..].find('{') else {
            break;
        };
        let mut depth = 0usize;
        let mut index = name_end + brace;
        let end = loop {
            match bytes[index] {
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        break index;
                    }
                }
                _ => {}
            }
            index += 1;
            if index >= bytes.len() {
                break bytes.len() - 1;
            }
        };
        found.push((
            text[name_start..name_end].to_string(),
            text[start..start + (fn_at - start)].to_string(),
            text[name_end + brace..=end].to_string(),
        ));
        cursor = end;
    }
    found
}

#[test]
fn no_test_skips_itself_instead_of_being_ignored() {
    fn sources(dir: &Path) -> Vec<PathBuf> {
        let mut found = Vec::new();
        let Ok(entries) = std::fs::read_dir(dir) else {
            return found;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                found.extend(sources(&path));
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                found.push(path);
            }
        }
        found
    }

    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut offenders = Vec::new();
    for root in ["src", "tests"] {
        for path in sources(&manifest.join(root)) {
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            for (name, attrs, body) in test_functions(&text) {
                if attrs.contains("#[ignore") {
                    continue;
                }
                // The tell is an early `return` guarded by an availability probe. Matching the
                // body rather than the raw file keeps this detector from finding its own text.
                let skips = body.lines().any(|line| {
                    line.contains("if !")
                        && (line.contains("pdfium_available()") || line.contains("pdfium_ready()"))
                }) && body.contains("return;");
                if skips {
                    offenders.push(format!(
                        "{}: {name}",
                        path.strip_prefix(&manifest).unwrap_or(&path).display()
                    ));
                }
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "these tests skip their own body instead of carrying `#[ignore]`, so a green suite \
         cannot tell that they never ran: {offenders:#?}"
    );
}

/// The Windows job exists because it is the only one that can load PDFium. That makes its
/// assertion the only evidence the gated tests ran at all, so the assertion has to be
/// capable of failing. The previous version was not: it grepped plain `cargo test` output
/// for "skipping: PDFium", but libtest captures the stdout of passing tests, so the string
/// never reached the log and the check passed unconditionally.
#[test]
fn the_windows_job_asks_for_ignored_tests_and_counts_them() {
    let text = ci_workflow();

    assert!(
        text.contains("cargo test -- --include-ignored"),
        "the Windows job must run the `#[ignore]`d PDFium tests, not skip them"
    );
    assert!(
        !text.contains(r#"Matches($output, "skipping: PDFium")"#),
        "the captured-stdout grep cannot match in a passing suite; count libtest's `ignored` instead"
    );
    let assertion = text
        .split("- name: Assert the PDFium tests actually ran")
        .nth(1)
        .expect("the Windows job must assert the PDFium tests ran");
    assert!(
        assertion.contains("ignored") && assertion.contains("if ($ignored -gt 0)"),
        "the assertion must fail when tests were ignored, using libtest's own count"
    );
}

/// The Linux job cannot load PDFium, so its gated tests stay ignored -- which is why a skip
/// arm must not reappear there, and why the count is asserted rather than trusted. Both
/// halves of that argument have failed in this repository before: guards that skipped
/// silently, and a CI check that could never fail.
#[test]
fn the_linux_job_does_not_force_pdfium_tests_without_a_library() {
    let text = ci_workflow();
    let tests_step = text
        .split("- name: Tests")
        .nth(1)
        .expect("the Linux job must run the tests");
    let tests_step = tests_step.split("- name:").next().unwrap_or(tests_step);

    assert!(
        !tests_step.contains("--include-ignored"),
        "`--include-ignored` here runs the PDFium tests with no library: measured as 27 failures"
    );
    assert!(
        tests_step.contains("ignored") && tests_step.contains("ne 74"),
        "the Linux job must assert exactly the 74 known-ignored tests"
    );
}

/// The render-smoke step ran **zero** tests for as long as it existed. All three tests in
/// `tests/memory.rs` are `#[ignore]`d, and the step did not pass `--include-ignored`, so
/// every run logged `0 passed; 0 failed; 3 ignored` and exited 0. It was green, it was named
/// "Render smoke tests (real PDFium)", and it validated nothing.
///
/// Mutation-verified: dropping `--include-ignored` from that step fails this test.
#[test]
fn the_render_smoke_step_actually_runs_the_ignored_memory_tests() {
    let text = ci_workflow();
    let step = text
        .split("- name: Render smoke tests (real PDFium)")
        .nth(1)
        .expect("the Windows job must have a render smoke step");
    let step = step.split("- name:").next().unwrap_or(step);
    let command = step
        .lines()
        .find(|line| line.trim_start().starts_with("run:"))
        .expect("the render smoke step must have a run command");

    assert!(
        command.contains("--include-ignored"),
        "the render smoke step must pass --include-ignored: every test in tests/memory.rs \
         carries #[ignore], so without it the step reports 0 passed and 3 ignored. \
         Command: {command:?}"
    );
    assert!(
        command.contains("--test memory"),
        "the render smoke step must target tests/memory.rs. Command: {command:?}"
    );
}

/// The `passed` floor is the other half of the same pin, and it had already drifted: it still
/// said 419 when the measured non-PDFium baseline was 428, because nothing read it. A floor
/// that nobody checks is a comment. This makes raising it an explicit act, in the same commit
/// as whatever added the tests, exactly like the ignored count.
///
/// Mutation-verified: `-lt 433` -> `-lt 434` fails this test; restoring it passes. It was
/// raised here from 432 by the B-1 boundary guard, which adds one test.
#[test]
fn the_linux_job_pins_the_measured_passed_floor() {
    let text = ci_workflow();
    let tests_step = text
        .split("- name: Tests")
        .nth(1)
        .expect("the Linux job must run the tests");
    let tests_step = tests_step.split("- name:").next().unwrap_or(tests_step);

    assert!(
        tests_step.contains(r#"if [ "$passed" -lt 464 ]"#),
        "the Linux job must pin the non-PDFium baseline at the measured 464. \
         A floor below the current count cannot detect a test that stopped being collected."
    );
}
