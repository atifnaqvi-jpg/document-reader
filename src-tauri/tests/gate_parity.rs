//! `scripts/ralph/gate.sh` and `.github/workflows/ci.yml` are two hand-kept lists of the same
//! checks, and they have drifted twice. `cargo fmt --check` was in CI and absent from the gate
//! until `144f65c`, so `714f2a9` passed the gate and failed CI on formatting alone. Clippy ran
//! without `-D warnings` in the gate until `dfb8549` — the same failure in a smaller size, where
//! a warning passes locally and fails CI. Neither drift was detected; both were found by
//! looking. That is what this guard is for.
//!
//! Literal equality between the two lists is not available, and pretending otherwise would be a
//! guard that tests the shape of its own reading:
//!
//! * the jobs run on different platforms, and CI installs Tauri's system libraries before
//!   anything compiles — setup, not a check, and this machine already has them;
//! * `cargo check --target x86_64-pc-windows-msvc`, `scripts/fetch-pdfium.ps1` and the
//!   PDFium-on-Windows steps cannot run on this aarch64 box, and `--include-ignored` here uses
//!   a different library that CI is explicitly told never to fetch on Linux;
//! * CI's `Tests` step is a shell script with count floors embedded in it, not a command the
//!   gate could contain. The gate expresses that check as two pinned shapes instead.
//!
//! So the correspondence is *declared*: every `run:` command in `ci.yml` is either executed by
//! `gate.sh` or named in gate.sh's `ci-exemptions` block with a reason and the command that
//! covers it. A new CI step fails this test until someone decides which it is, which is the
//! property that matters — the drift becomes a decision instead of an omission.
//!
//! The exemption's `covers:` field is asserted too, so an excuse cannot outlive the gate command
//! it claims: delete `npx tsc --noEmit` and the `npm run build` entry fails instead of quietly
//! excusing the loss.

use std::path::PathBuf;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("src-tauri must have a parent directory")
        .to_path_buf()
}

fn read(relative: &str) -> String {
    let path = repo_root().join(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

fn normalize(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn unquote(value: &str) -> String {
    value.trim().trim_matches(['"', '\'']).to_string()
}

#[derive(Debug)]
struct Step {
    job: String,
    name: String,
    command: String,
}

/// The `run:` command of every step in every job, whitespace-collapsed. Steps with no `run:`
/// (`uses:` actions) are not checks and are dropped.
fn ci_steps(text: &str) -> Vec<Step> {
    let mut steps: Vec<Step> = Vec::new();
    let mut job = String::new();
    let mut in_jobs = false;
    let mut current: Option<(String, String)> = None;

    fn finish(steps: &mut Vec<Step>, job: &str, current: Option<(String, String)>) {
        if let Some((name, command)) = current {
            if !command.is_empty() {
                steps.push(Step {
                    job: job.to_string(),
                    name,
                    command,
                });
            }
        }
    }

    let mut lines = text.lines().peekable();
    while let Some(raw) = lines.next() {
        let trimmed = raw.trim_start();
        if trimmed.trim().is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let indent = raw.len() - trimmed.len();
        let item = trimmed.trim_end();

        if indent == 0 {
            in_jobs = item == "jobs:";
            continue;
        }
        if !in_jobs {
            continue;
        }
        if indent == 2 && item.ends_with(':') {
            finish(&mut steps, &job, current.take());
            job = item.trim_end_matches(':').to_string();
            continue;
        }
        if indent == 6 && item.starts_with("- ") {
            finish(&mut steps, &job, current.take());
            let rest = item[2..].trim_start();
            current = Some(if let Some(value) = rest.strip_prefix("run:") {
                (String::new(), command_body(value, &mut lines))
            } else if let Some(value) = rest.strip_prefix("name:") {
                (unquote(value), String::new())
            } else {
                (String::new(), String::new())
            });
            continue;
        }
        let Some((name, command)) = current.as_mut() else {
            continue;
        };
        if indent == 8 {
            if let Some(value) = item.strip_prefix("name:") {
                *name = unquote(value);
            } else if let Some(value) = item.strip_prefix("run:") {
                *command = command_body(value, &mut lines);
            }
        }
    }
    finish(&mut steps, &job, current);
    steps
}

/// A `run:` value, either inline or a YAML block scalar. The block's own lines sit deeper than
/// the `run:` key (indent 8), so anything at or above that indent ends it.
fn command_body<'a, I: Iterator<Item = &'a str>>(
    value: &str,
    lines: &mut std::iter::Peekable<I>,
) -> String {
    let value = value.trim();
    if !matches!(value, "|" | "|-" | ">" | ">-") {
        return normalize(value);
    }
    let mut body: Vec<&str> = Vec::new();
    while let Some(next) = lines.peek() {
        let trimmed = next.trim();
        if trimmed.is_empty() {
            body.push("");
            lines.next();
            continue;
        }
        if next.len() - next.trim_start().len() <= 8 {
            break;
        }
        body.push(trimmed);
        lines.next();
    }
    normalize(&body.join("\n"))
}

#[derive(Debug)]
struct Exemption {
    command: String,
    covers: String,
    reason: String,
}

const EXEMPTIONS_OPEN: &str = ">>> ci-exemptions";
const EXEMPTIONS_CLOSE: &str = "<<< ci-exemptions";

/// The block in `gate.sh` naming the CI checks this gate does not run. One entry per line:
///
/// ```text
/// # ci-exempt: <ci.yml command> ;; covers: <command this file runs, or none> ;; <reason>
/// ```
fn gate_exemptions(text: &str) -> Vec<Exemption> {
    let mut out = Vec::new();
    let mut inside = false;
    for raw in text.lines() {
        let line = raw.trim();
        let comment = line.strip_prefix('#').map(str::trim);
        if comment.is_some_and(|c| c.starts_with(EXEMPTIONS_CLOSE)) {
            inside = false;
            continue;
        }
        if comment.is_some_and(|c| c.starts_with(EXEMPTIONS_OPEN)) {
            inside = true;
            continue;
        }
        if !inside {
            continue;
        }
        let body = comment
            .unwrap_or_else(|| panic!("the ci-exemptions block may hold only comments: {line:?}"))
            .trim();
        if body.is_empty() {
            continue;
        }
        let body = body
            .strip_prefix("ci-exempt:")
            .map(str::trim)
            .unwrap_or_else(|| {
                panic!("every line in the ci-exemptions block must start `ci-exempt:`: {line:?}")
            });
        let mut fields = body.splitn(3, ";;").map(str::trim);
        let command = fields.next().unwrap_or_default();
        let covers = fields.next().unwrap_or_default();
        let reason = fields.next().unwrap_or_default();
        assert!(
            !command.is_empty(),
            "an exemption names no ci.yml command: {line:?}"
        );
        assert!(
            !reason.is_empty(),
            "an exemption without a reason is an omission wearing a decision's clothes: {line:?}"
        );
        let covers = covers
            .strip_prefix("covers:")
            .unwrap_or_else(|| {
                panic!("the middle field of an exemption must be `covers: <command>`: {line:?}")
            })
            .trim();
        out.push(Exemption {
            command: normalize(command),
            covers: normalize(covers),
            reason: reason.to_string(),
        });
    }
    out
}

/// The commands `gate.sh` actually executes. Comment lines are dropped, because the exemption
/// block is made of them and an excuse must not be able to satisfy its own claim. Message lines
/// (`log`, `fail`, `printf`, `echo`) are dropped too: `log "gate: cargo fmt --check"` names the
/// check it is about to run, and if that string counted as the check, deleting the run and
/// leaving the message would pass — the "a value that is still satisfied by the defect" shape.
fn gate_commands(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter(|line| {
            let first = line.split_whitespace().next().unwrap_or("");
            !matches!(first, "log" | "fail" | "printf" | "echo" | "set")
        })
        .map(normalize)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Everything wrong with the declared correspondence. Kept separate from the files it reads so
/// that a crafted sample can show it reporting each defect, rather than a broken parser
/// returning `[]` and being indistinguishable from a clean tree.
fn problems(steps: &[Step], gate: &str, exemptions: &[Exemption]) -> Vec<String> {
    let mut problems = Vec::new();

    for (index, first) in exemptions.iter().enumerate() {
        for second in exemptions.iter().skip(index + 1) {
            let (short, long) = if first.command.len() <= second.command.len() {
                (&first.command, &second.command)
            } else {
                (&second.command, &first.command)
            };
            if short != long && long.starts_with(short.as_str()) {
                problems.push(format!(
                    "exemption {short:?} is a prefix of {long:?}; the allow-list must name \
                     checks, not wildcards"
                ));
            }
        }
        if !steps
            .iter()
            .any(|step| step.command.starts_with(&first.command))
        {
            problems.push(format!(
                "exemption {:?} names no `run:` command in ci.yml; it has outlived the step it \
                 excused (its recorded reason was {:?})",
                first.command, first.reason
            ));
        }
        if first.covers != "none" && !gate.contains(&first.covers) {
            problems.push(format!(
                "exemption {:?} says {:?} covers it, but gate.sh does not run that command",
                first.command, first.covers
            ));
        }
    }

    for step in steps {
        if gate.contains(&step.command) {
            continue;
        }
        if exemptions
            .iter()
            .any(|exemption| step.command.starts_with(&exemption.command))
        {
            continue;
        }
        problems.push(format!(
            "ci.yml job `{}` step {:?} runs a command gate.sh does not run and no exemption \
             names: {:?}",
            step.job, step.name, step.command
        ));
    }

    problems
}

#[test]
fn the_gate_runs_the_checks_ci_requires() {
    let steps = ci_steps(&read(".github/workflows/ci.yml"));
    let gate_text = read("scripts/ralph/gate.sh");
    let gate = gate_commands(&gate_text);
    let exemptions = gate_exemptions(&gate_text);

    assert!(
        steps.len() >= 10,
        "parsed only {} `run:` steps out of ci.yml. A parse that finds nothing, or almost \
         nothing, reports no drift and is indistinguishable from a clean tree: {steps:#?}",
        steps.len()
    );
    assert!(
        exemptions.len() >= 5,
        "parsed {} exemptions out of gate.sh's `ci-exemptions` block. An unparsed block excuses \
         nothing, so every CI-only step would have to be reported instead: {exemptions:#?}",
        exemptions.len()
    );

    // The two checks that have actually drifted out of this gate, pinned by name. An exemption
    // is a decision and this comparison allows it; for these two the decision is already made,
    // and it was made by a red CI run.
    for required in [
        "cargo fmt --check",
        "cargo clippy --all-targets -- -D warnings",
    ] {
        assert!(
            gate.contains(required),
            "gate.sh no longer runs `{required}`, which CI requires. Both of the drifts this \
             file exists to catch were exactly this shape: the check stayed in CI and left the \
             gate, and the gate kept reporting green"
        );
        assert!(
            !exemptions.iter().any(|e| required.starts_with(&e.command)),
            "`{required}` is exempted from the gate. It runs on this machine, so an exemption \
             for it records the drift instead of preventing it"
        );
    }

    let found = problems(&steps, &gate, &exemptions);
    assert!(
        found.is_empty(),
        "gate.sh and ci.yml disagree about which checks exist:\n  {}\n\n\
         Either add the check to gate.sh, or name it in the `ci-exemptions` block with the \
         reason it cannot run here and the command that covers it.",
        found.join("\n  ")
    );

    // Measured at 2: `cargo fmt --check` and `cargo clippy` are the CI commands this gate runs
    // literally. Lowering this floor means a check moved from the executed side to the excused
    // side, which is the drift, not a clean tree -- so the floor is the measured value.
    let executed = steps
        .iter()
        .filter(|step| gate.contains(&step.command))
        .count();
    assert!(
        executed >= 2,
        "only {executed} ci.yml commands are actually run by gate.sh (measured: 2). Every step \
         matching nothing means the comparison cannot detect drift at all"
    );
}

/// The guard above is only evidence if its parser can see a defect. Fed a clean sample it must
/// report nothing; fed each defect shape -- an unexcused step, an excuse whose claimed coverage
/// was removed, and a prefix that would act as a wildcard -- it must report that shape.
#[test]
fn the_parity_check_reports_what_it_is_given() {
    let sample_ci = "\
jobs:
  sample:
    name: Sample
    steps:
      - uses: actions/checkout@v4
      - name: Runs here
        run: cargo fmt --check
      - name: Excused
        run: |
          cargo test 2>&1 | tee /tmp/out.txt
      - name: Orphan
        run: npm run lint
";
    let sample_gate = "\
#!/usr/bin/env bash
# >>> ci-exemptions — parsed by tests/gate_parity.rs
# ci-exempt: cargo test 2>&1 | tee /tmp/out.txt ;; covers: cargo test ;; platform
# <<< ci-exemptions
( cd src-tauri && cargo fmt --check )
( cargo test \"$@\" )
";

    let steps = ci_steps(sample_ci);
    let names: Vec<&str> = steps.iter().map(|step| step.name.as_str()).collect();
    assert_eq!(
        names,
        ["Runs here", "Excused", "Orphan"],
        "the sample parser must see exactly the three `run:` steps, with the `uses:` action and \
         the block scalar read correctly"
    );
    assert!(
        steps.iter().all(|step| step.job == "sample"),
        "the job is part of the report; a step must not be attributed to the wrong job: {steps:#?}"
    );
    assert_eq!(
        steps[1].command, "cargo test 2>&1 | tee /tmp/out.txt",
        "a block scalar must be read as its commands, not as the marker that introduces them"
    );

    let gate = gate_commands(sample_gate);
    let exemptions = gate_exemptions(sample_gate);
    assert_eq!(
        exemptions.len(),
        1,
        "the block has one entry: {exemptions:#?}"
    );
    assert_eq!(exemptions[0].covers, "cargo test");

    // The clean sample: two checks, one run here and one declared. Nothing to report.
    let clean_gate = format!("{gate}\n( npm run lint )");
    assert_eq!(
        problems(&steps, &clean_gate, &exemptions),
        Vec::<String>::new(),
        "the clean sample must report nothing, or every finding below could be noise"
    );

    // Defect 1: a step the gate does not run and no exemption names.
    let unexcused = problems(&steps, &gate, &exemptions);
    assert_eq!(unexcused.len(), 1, "{unexcused:#?}");
    assert!(
        unexcused[0].contains("Orphan") && unexcused[0].contains("sample"),
        "the report must name the step and its job: {unexcused:#?}"
    );

    // Defect 2: the covering command was removed from the gate, so the excuse is stale.
    let without_test = gate.replace("( cargo test \"$@\" )", "");
    let stale: Vec<String> = problems(&steps, &without_test, &exemptions)
        .into_iter()
        .filter(|problem| problem.contains("covers it"))
        .collect();
    assert_eq!(stale.len(), 1, "{stale:#?}");
    assert!(
        stale[0].contains("cargo test"),
        "the report must name the claimed command: {stale:#?}"
    );

    // Defect 3: a prefix used as a wildcard.
    let mut wildcard = gate_exemptions(sample_gate);
    wildcard.push(Exemption {
        command: "cargo test".to_string(),
        covers: "none".to_string(),
        reason: "wildcard".to_string(),
    });
    let prefixes: Vec<String> = problems(&steps, &gate, &wildcard)
        .into_iter()
        .filter(|problem| problem.contains("prefix"))
        .collect();
    assert_eq!(prefixes.len(), 1, "{prefixes:#?}");

    // Defect 4: an exemption that names no step at all.
    let mut orphaned = gate_exemptions(sample_gate);
    orphaned.push(Exemption {
        command: "cargo miri test".to_string(),
        covers: "none".to_string(),
        reason: "no longer run anywhere".to_string(),
    });
    let outlived: Vec<String> = problems(&steps, &gate, &orphaned)
        .into_iter()
        .filter(|problem| problem.contains("outlived"))
        .collect();
    assert_eq!(outlived.len(), 1, "{outlived:#?}");
}
