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
        tests_step.contains("ignored") && tests_step.contains("ne 107"),
        "the Linux job must assert exactly the 90 known machine-dependent tests: 76 from before
         the Form XObject walk, the three PDFium-side walk tests it added, the two that compose
         the picker path (font search; describe/edit/save/re-open), two from P2.7.7's Type0
         writer (row6a renders pixels, row6c reads the program's hmtx; row6b is not ignored),
         four from P2.8's expectation reader, which needs PDFium's character grid by design,
         two from P2.8 case (a)'s reflow writes, whose oracle is per-glyph ink for the same
         reason, one from R-1a's kerned prefix, which compares a reported tail origin against
         that same ink, one from R-4, which compares three runs' positions against it, and
         P3.2 SCOPE-A's five protect_save tests, which reopen the protected copy through
         PDFium because lopdf wrote it and cannot be its own oracle, plus the D1 regression added
         while fixing the 1.2.0 hand-test defects, which opens a document this app protected --
         the same reason as those five"
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
///
/// Raised from 746 to 808 with SCOPE-C's badge unit: the workflow floor had drifted 34 below the
/// tree (CI reported 780 passed against a floor of 746), which is precisely the drift this guard
/// exists to make visible -- and it did not, because this guard pinned the same stale number that
/// the workflow held. Two copies of a stale value agree with each other and with nothing else.
#[test]
fn the_linux_job_pins_the_measured_passed_floor() {
    let text = ci_workflow();
    let tests_step = text
        .split("- name: Tests")
        .nth(1)
        .expect("the Linux job must run the tests");
    let tests_step = tests_step.split("- name:").next().unwrap_or(tests_step);

    assert!(
        tests_step.contains(r#"if [ "$passed" -lt 959 ]"#),
        "the Linux job must pin the non-PDFium baseline at the measured 959. \
         A floor below the current count cannot detect a test that stopped being collected."
    );
}

/// Before MECH-B, no CI job ever asked the *shipping* reader to draw an embedded subset.
/// Poppler covered the developer machine; PDFium is what users run. This step is the only
/// evidence that the font-embed path produces something a real viewer can render, so it has
/// to be incapable of passing without running the test.
///
/// Both halves are load-bearing, and both have failed elsewhere in this file's history:
/// `--include-ignored` because the test is `#[ignore]`d, and the `... ok` assertion because a
/// filter that matches nothing exits 0. Mutation-verified: removing `--include-ignored`
/// fails this test, and so does replacing the `... ok` check with a bare exit-code check.
#[test]
fn the_row_one_pdfium_step_cannot_pass_without_running() {
    let text = ci_workflow();
    let step = text
        .split("- name: Row-1 embedded subset renders through PDFium")
        .nth(1)
        .expect("the Windows job must run the row-1 PDFium render check");
    let step = step.split("- name:").next().unwrap_or(step);

    // Inspect the `run:` command, not the whole step. The step body mentions the test name
    // and `--include-ignored` in comments and in its own assertion, so a whole-step
    // `contains` passes even when the command drops the flag -- measured: that mutant
    // survived the first version of this guard.
    //
    // The command is a YAML block scalar (`run: |`), so it is the `run:` line plus every
    // following line indented deeper than it. Stopping at the `run:` line alone matched
    // nothing and failed this guard against a correct workflow.
    let mut lines = step
        .lines()
        .skip_while(|l| !l.trim_start().starts_with("run:"));
    let run_header = lines
        .next()
        .expect("the row-1 step must have a run command");
    let indent = run_header.len() - run_header.trim_start().len();
    let command = std::iter::once(run_header)
        .chain(lines.take_while(|line| {
            line.trim().is_empty() || line.len() - line.trim_start().len() > indent
        }))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        command.contains("row1_the_embedded_subset_renders_through_pdfium"),
        "the step must name the test, not run the whole target and hope"
    );
    assert!(
        command.contains("--include-ignored"),
        "the test is `#[ignore]`d; without this flag it is skipped and the step proves nothing"
    );
    assert!(
        step.contains("PDFIUM_LIBRARY_PATH"),
        "PDFium is loaded from this variable; unset, the test fails with PdfiumUnavailable"
    );
    assert!(
        step.contains(r#"\.\.\. ok""#),
        "a filter that matches no test exits 0, so the step must require libtest's pass line"
    );
    assert!(
        step.contains("PdfiumUnavailable"),
        "a missing library must be diagnosed by name, not inferred from a nonzero exit"
    );
}

/// A hung job is worse than a red one, and this is not a hypothetical: on 2026-11-11 three
/// separate runs on three commits sat in `Install Tauri system dependencies` for 37, 47 and 102
/// minutes while the Windows and Frontend jobs on the same commits passed. The job reported
/// `in_progress` the whole time, so nothing told anyone the tree was unverified -- and GitHub
/// refuses to re-run a workflow that is still running, so one stalled step blocked the whole
/// check until it was cancelled by hand.
///
/// Two properties prevent a repeat, and both are asserted here because either alone is not
/// enough. The job timeout bounds the total, but 40 minutes of a hung `apt-get` still wastes the
/// runner and still blocks a re-run for 40 minutes. The per-attempt `timeout` bounds the step
/// itself and turns a hang into a retry.
///
/// Mutation-verified: removing `timeout-minutes: 40` fails this test; removing `timeout 600`
/// from the apt lines also fails it.
#[test]
fn a_stalled_dependency_install_fails_visibly_instead_of_hanging() {
    let text = ci_workflow();
    let rust_job = text
        .split("  rust:")
        .nth(1)
        .expect("the workflow must define the Linux rust job");
    let job_body = rust_job.split("\n  frontend:").next().unwrap_or(rust_job);

    assert!(
        job_body.contains("timeout-minutes: 40"),
        "the Rust job must carry a `timeout-minutes` bound. Without it a stalled step reports \
         `in_progress` indefinitely, which is the one CI outcome that tells nobody anything and \
         also blocks `rerun-failed-jobs`."
    );

    let install = job_body
        .split("- name: Install Tauri system dependencies")
        .nth(1)
        .expect("the Rust job must install the system dependencies")
        .split("- name:")
        .next()
        .unwrap_or("");
    assert!(
        install.matches("timeout 600").count() >= 2,
        "both `apt-get update` and `apt-get install` must be bounded by `timeout`, so a hung \
         mirror becomes a failed attempt rather than an open-ended wait; found {} occurrence(s)",
        install.matches("timeout 600").count()
    );
    assert!(
        install.contains("attempt"),
        "the install must retry rather than fail on the first slow mirror; the step that hung \
         three times was transient, not broken"
    );
}

/// The Windows job's `--include-ignored` step has two failure modes, and only one of them
/// produces a red check.
///
/// Measured 2026-11-12 at `8d12b49`, four attempts on the same binary: two died with
/// `0xc0000005 STATUS_ACCESS_VIOLATION` in `textedit_gates` after ~30 of 65 tests, two hung
/// (25 min in CI, 62 min in the release run), and one passed in 198 s. The crash is the
/// documented intermittent and is at least *visible*. The hang is the dangerous one: the step
/// declared no `timeout-minutes`, so it reported `in_progress` until GitHub's 6 h cap — a
/// six-hour silence that says nothing and, because GitHub refuses to re-run a running
/// workflow, also blocks the recovery.
///
/// The bound is measured, not guessed: the whole job took 380 s on run 43126273588. 30 min is
/// roughly five times that, so a cold cache and a slow runner still fit.
///
/// This test does not claim the flake is fixed. It asserts the flake may no longer be silent.
///
/// Mutation-verified: deleting `timeout-minutes: 30` from `ci.yml`'s `windows-check` job fails
/// this test. Moving it to a different job also fails it, because the assertion reads the
/// `windows-check` body specifically rather than the file as a whole.
#[test]
fn the_windows_job_is_bounded_so_a_hung_suite_fails_instead_of_going_quiet() {
    let text = ci_workflow();
    let windows_job = text
        .split("  windows-check:")
        .nth(1)
        .expect("the workflow must define the Windows job");
    // The next job at this indent ends this one; there is none after it today, but slicing on
    // the *next* job keeps the assertion honest if one is added below.
    let job_body = windows_job
        .split("\n  frontend:")
        .next()
        .unwrap_or(windows_job);

    assert!(
        job_body.contains("timeout-minutes: 30"),
        "the Windows job must carry a `timeout-minutes: 30` bound. Its `--include-ignored` step \
         has hung for 25-62 minutes on a commit that passed in 198 s on another attempt; \
         unbounded, that reports `in_progress` until the 6 h job cap and blocks re-runs."
    );

    // A bound on the wrong job would satisfy the string check above while leaving the hanging
    // step unbounded, so assert this job actually contains the step that hangs.
    assert!(
        job_body.contains("Assert the PDFium tests actually ran"),
        "the `windows-check` job must still contain the `--include-ignored` step the timeout is \
         there to bound; if that step moved, move the timeout with it"
    );
}

/// The assertion step must carry its own bound, not only the job's.
///
/// The guard above is satisfied by the *job* bound — a whole-job `contains` cannot tell a
/// step-level `timeout-minutes` from a job-level one. That gap was measured on 2026-11-14: runs
/// 43550684995 and 43548978550 both entered this step, ran it past 19 minutes against a healthy
/// 185 s, and were cancelled with the job at **30.1 min**. A job-level bound turns a hang into
/// a *late* cancellation, and a cancelled job cannot be `rerun-failed-jobs`d — so the tree reads
/// unverified for half an hour and then cannot be retried without a fresh push. The release
/// workflow got exactly this step-level fix on 2026-11-13; CI did not, because the first three
/// failures were seen there.
///
/// `continue-on-error` is deliberately *not* asserted here, unlike the release workflow: a
/// release must not be withheld by a flake, but a branch gate must not pass quietly either. A
/// timed-out suite here fails, and says so.
///
/// Mutation-verified: deleting the step's `timeout-minutes: 20` fails this test while leaving
/// the job-level `timeout-minutes: 30` in place, which is the case the guard above accepts.
#[test]
fn the_pdfium_assertion_step_is_bounded_not_only_its_job() {
    let text = ci_workflow();
    let lines: Vec<&str> = text.lines().collect();

    let start = lines
        .iter()
        .position(|line| {
            line.trim_start()
                .starts_with("- name: Assert the PDFium tests")
        })
        .expect(
            "ci.yml must still contain the `Assert the PDFium tests actually ran` step; if it \
             was renamed, rename this guard's needle rather than deleting the guard",
        );
    let step_indent = lines[start].len() - lines[start].trim_start().len();
    // The header line sits at exactly `step_indent`, so a `take_while` over the whole slice
    // would reject line one and yield an empty string no needle can match. Take it
    // unconditionally, then extend -- the same bug the redaction guard records.
    let mut step: Vec<&str> = vec![lines[start]];
    step.extend(lines[start + 1..].iter().take_while(|line| {
        line.trim().is_empty() || (line.len() - line.trim_start().len()) > step_indent
    }));
    let step = step.join("\n");

    assert!(
        step.contains("timeout-minutes: 20"),
        "the `--include-ignored` step must carry its own `timeout-minutes: 20`; the job's 30 min \
         is spent *inside* this step when it hangs, and a job-level cancellation blocks \
         `rerun-failed-jobs`. Step block was: {step:?}"
    );

    // The bound must not have been bought with silence: a `continue-on-error` here would let a
    // hung Windows suite report green, which is the "unseen skip" the charter bans.
    //
    // Comments are stripped first. The step's own comment *explains* that `continue-on-error` is
    // not set here, and a raw `contains` read that sentence as the setting itself -- measured:
    // the guard failed against the very comment describing why it should pass. A guard that
    // cannot tell code from prose about code is the same defect as one satisfied by prose.
    let settings: String = step
        .lines()
        .map(|line| line.split('#').next().unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !settings.contains("continue-on-error"),
        "the CI assertion step must NOT set `continue-on-error` -- unlike the release workflow, \
         where a hang must not withhold a tag, here a timed-out suite has to fail the branch \
         gate rather than pass it. Step settings were: {settings:?}"
    );
}
