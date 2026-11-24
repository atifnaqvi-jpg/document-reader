//! One source for every test-count pin, and a guard that fails when a carrier disagrees.
//!
//! ## The defect this closes
//!
//! The same number lived in three files as three independent literals. On 2026-11-22 a unit added
//! ten tests, moved the pins in `scripts/ralph/gate.sh`, ran the gate green, pushed, and CI failed
//! -- because `.github/workflows/ci.yml` had its own copy of the ignored count that nothing local
//! could see. The fix for that moved the workflow copy but not the copy inside
//! `tests/ci_workflow.rs`, which reads the workflow as text, so CI failed a second time on the same
//! number. Two red CI runs, no product defect, both from editing one number in one place.
//!
//! The inventory when this file was written, each verified by reading the file:
//!
//! | carrier | pin | was it locally guarded? |
//! |---|---|---|
//! | `scripts/ralph/gate.sh` | four `PIN_*` values | it compares them to measured output |
//! | `.github/workflows/ci.yml` | ignored `-ne`, Linux passed `-lt`, Windows passed `-lt` | only via literals copied into `ci_workflow.rs` |
//! | `.github/workflows/release-windows.yml` | passed `-lt` | **no test read this file at all** |
//! | `src-tauri/tests/ci_workflow.rs` | two literals | it *is* the guard, but from a copy |
//! | `src-tauri/tests/gate_parity.rs` | guards commands, not numbers | n/a |
//!
//! ## The shape of the fix
//!
//! The numbers now live here, once, and every carrier is *verified* against them rather than
//! restated. This follows `gate_parity.rs`, which already solved the same drift for CI's `run:`
//! commands: a correspondence that is declared and checked beats two lists that are hoped to agree.
//!
//! Verification is not the same as generation. Generating the workflow would move the numbers into
//! a build step and make the YAML unreadable in review; verifying keeps each carrier legible while
//! making disagreement a red test. What matters is that a number cannot be edited in two places
//! independently, and after this it cannot be edited in one place at all without this file changing.
//!
//! ## Why the guard is a plain test
//!
//! `gate.sh` runs `cargo test`, so this runs inside the gate. That is deliberate: the gate-vs-CI gap
//! was not a missing check, it was a check the gate did not execute. Putting the pin arithmetic
//! where the gate already looks is what makes "a pin the gate does not govern" impossible rather
//! than merely unlikely.

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

// ---------------------------------------------------------------------------------------------
// The single source. Every carrier below is asserted to agree with these.
// ---------------------------------------------------------------------------------------------

/// `cargo test`, plain shape, on this machine (Linux, PDFium present, LEA fixture present).
///
/// 2026-11-24: 1042 -> 1045. The three are the D10 leak guards in `model::tests`
/// (`every_kind_serialises_as_a_json_string`, `every_kind_round_trips_through_its_wire_name`,
/// `an_unknown_wire_name_is_refused`). They are unit tests, so they run in both shapes and on
/// both platforms, which is why every carrier below moved by the same three rather than by
/// a platform-specific amount.
pub const PLAIN_PASSED: usize = 1045;
/// `cargo test`, plain shape: tests that carry `#[ignore]`.
pub const PLAIN_IGNORED: usize = 117;
/// `cargo test -- --include-ignored`: nothing may remain ignored.
pub const INCLUDED_PASSED: usize = 1162;
pub const INCLUDED_IGNORED: usize = 0;

/// The Linux CI job runs without PDFium, so every gated test stays ignored. It asserts the ignored
/// count exactly, because a test that silently stops being collected is the failure mode there.
pub const CI_LINUX_IGNORED: usize = 117;

/// The Linux CI job's `passed` floor. This is a *floor*, not an equality, and it had drifted below
/// the tree: it said 959 while the tree reports 1021, so 62 tests could stop being collected before
/// CI noticed. Raised here to the measured value, which is what the file's own comment says the
/// floor is for -- headroom is a floor that cannot fail.
pub const CI_LINUX_PASSED_FLOOR: usize = 1045;

/// The shipping target's floor, measured on Windows (run 45267725552, 2026-11-23, SHA `ff28a0a`:
/// `PDFium tests ran: 1156 passed, 0 ignored`).
///
/// It is now three below [`INCLUDED_PASSED`] (1162 - 3 = 1159) and the difference is accounted
/// for, not absorbed: three tests are compiled out off Linux. The D10 leak guards are plain unit
/// tests, so they exist on Windows too and the 2026-11-24 delta moved this floor from 1156 to
/// 1159 -- the Windows figure is asserted on the next dispatch, not assumed.
/// `tests/signing_store.rs`'s `listing_refuses_off_windows_rather_than_reporting_an_empty_store` and
/// `tests/signing_certificates.rs`'s `a_non_windows_build_refuses_the_certificate_store_instead_of_reporting_none`
/// are `#[cfg(not(windows))]`, and `tests/embed_acceptance.rs`'s
/// `row1_the_embedded_subset_renders_through_poppler` is `#[cfg(unix)]`. 1162 - 3 = 1159. A floor
/// that does not name that arithmetic is a floor nobody can check.
pub const CI_WINDOWS_PASSED_FLOOR: usize = 1159;

// ---------------------------------------------------------------------------------------------
// Parsing helpers. Each carrier is read as the artifact it is -- shell, YAML, Rust -- rather than
// through a shared regex that would let a mis-parse look like agreement.
// ---------------------------------------------------------------------------------------------

/// Reads `PIN_NAME=<n>` from a shell script.
fn shell_pin(text: &str, name: &str) -> usize {
    let prefix = format!("{name}=");
    let line = text
        .lines()
        .find(|line| line.trim_start().starts_with(&prefix))
        .unwrap_or_else(|| panic!("{name} is not assigned anywhere in the script"));
    text_of(line, &prefix)
}

fn text_of(line: &str, prefix: &str) -> usize {
    line.trim()
        .trim_start_matches(prefix)
        .parse()
        .unwrap_or_else(|e| panic!("{prefix} is not a number: {e}"))
}

/// Reads the integer out of `if [ "$name" -ne N ]`, scoped to the step whose body contains
/// `anchor`. Scoping matters: the workflow has several numeric assertions and a bare `-ne` search
/// would happily match the wrong one -- `$LASTEXITCODE -ne 0` in particular -- and report agreement
/// it never checked. The variable name is part of the marker for the same reason.
fn ci_assertion(text: &str, anchor: &str, variable: &str) -> usize {
    let step_start = text
        .find(anchor)
        .unwrap_or_else(|| panic!("no step contains the anchor {anchor:?}"));
    let body = &text[step_start..];
    let marker = format!("\"${variable}\" -");
    let at = body
        .find(&marker)
        .unwrap_or_else(|| panic!("the step at {anchor:?} has no \"${variable}\" assertion"));
    let tail = &body[at + marker.len()..];
    // Skip the operator itself (`ne`/`lt`) to reach the number.
    let after_op = tail
        .find(' ')
        .unwrap_or_else(|| panic!("malformed assertion at {anchor:?}"));
    let digits: String = tail[after_op..]
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(char::is_ascii_digit)
        .collect();
    digits
        .parse()
        .unwrap_or_else(|e| panic!("the assertion at {anchor:?} has no number: {e}"))
}

/// Reads the integer out of `if ($name -lt N) {`, the PowerShell spelling used by the Windows
/// job and the release workflow. Kept separate from [`ci_assertion`] rather than generalised: the
/// two shells put the bracket in different places, and one regex spanning both is how a mis-parse
/// starts looking like agreement.
fn powershell_assertion(text: &str, anchor: &str, variable: &str) -> usize {
    let step_start = text
        .find(anchor)
        .unwrap_or_else(|| panic!("no step contains the anchor {anchor:?}"));
    let body = &text[step_start..];
    let marker = format!("(${variable} -");
    let at = body
        .find(&marker)
        .unwrap_or_else(|| panic!("the step at {anchor:?} has no \"${variable}\" assertion"));
    let tail = &body[at + marker.len()..];
    let digits: String = tail
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(char::is_ascii_digit)
        .collect();
    digits
        .parse()
        .unwrap_or_else(|e| panic!("the assertion at {anchor:?} has no number: {e}"))
}

fn ci_workflow() -> String {
    read(".github/workflows/ci.yml")
}

fn gate_script() -> String {
    read("scripts/ralph/gate.sh")
}

// ---------------------------------------------------------------------------------------------
// Guards. One per carrier, so a red names the file that disagrees rather than "a pin moved".
// ---------------------------------------------------------------------------------------------

/// The gate's four pins are the source values themselves, so this asserts the file says what this
/// file says. It is the one carrier that can fail without a number changing elsewhere.
#[test]
fn the_gate_carries_the_same_pins_as_this_file() {
    let gate = gate_script();
    assert_eq!(
        shell_pin(&gate, "PIN_PLAIN_PASSED"),
        PLAIN_PASSED,
        "gate.sh's plain passed pin and this file disagree; the gate would pass a tree this file \
         calls wrong"
    );
    assert_eq!(shell_pin(&gate, "PIN_PLAIN_IGNORED"), PLAIN_IGNORED);
    assert_eq!(shell_pin(&gate, "PIN_IGNORED_PASSED"), INCLUDED_PASSED);
    assert_eq!(shell_pin(&gate, "PIN_IGNORED_IGNORED"), INCLUDED_IGNORED);
}

/// The Linux job's ignored count. This is the pin that failed CI twice.
#[test]
fn the_linux_job_ignores_exactly_the_known_tests() {
    let workflow = ci_workflow();
    assert_eq!(
        ci_assertion(&workflow, "name: Tests", "ignored"),
        CI_LINUX_IGNORED,
        "the Linux job asserts a different ignored count than this file; adding an #[ignore]d test \
         moves both or neither"
    );
}

/// The Linux `passed` floor, raised to the measured tree so it can fail again.
#[test]
fn the_linux_floor_is_the_measured_tree_not_headroom() {
    let workflow = ci_workflow();
    assert_eq!(
        ci_assertion(&workflow, "name: Tests", "passed"),
        CI_LINUX_PASSED_FLOOR,
        "the Linux passed floor is not the measured count; a floor below the tree cannot notice \
         tests that stopped being collected, which is the only thing it exists to do"
    );
}

/// The Windows floor in `ci.yml`, on the shipping target.
#[test]
fn the_windows_floor_is_the_measured_count() {
    let workflow = ci_workflow();
    let job = workflow
        .split("Windows compile check + render smoke (shipping target)")
        .nth(1)
        .expect("the Windows job must exist");
    assert_eq!(
        powershell_assertion(job, "Assert the PDFium tests actually ran", "passed"),
        CI_WINDOWS_PASSED_FLOOR,
        "the Windows floor disagrees with this file"
    );
}

/// `release-windows.yml`'s floor, which **no test read** before this one. It builds the installer,
/// so a wrong floor here ships a build nobody validated.
#[test]
fn the_release_workflow_floor_matches_the_shipping_floor() {
    let release = read(".github/workflows/release-windows.yml");
    assert_eq!(
        powershell_assertion(&release, "Assert the PDFium tests actually ran", "passed"),
        CI_WINDOWS_PASSED_FLOOR,
        "the release workflow runs the same suite as CI's Windows job on the same target, so its \
         floor must be the same number; two floors for one suite is how one of them gets stale"
    );
}

/// The comment above `release-windows.yml`'s floor states the number that line enforces.
///
/// This exists because the comment had already drifted: it said "Measured at 1141" while the
/// assertion below it enforced 1154. The guard above cannot see that, because it only parses the
/// `-lt` value -- a stale *explanation* passes every check that reads the code. That is the same
/// class of defect as a design doc citing a renamed test: plausible, and wrong when read.
///
/// So the prose is held to the number. Editing the floor without editing the comment is now red,
/// which is the only thing that keeps a rationale attached to the value it justifies.
#[test]
fn the_release_workflow_comment_states_the_floor_it_enforces() {
    let release = read(".github/workflows/release-windows.yml");
    let anchor = release
        .find("Assert the PDFium tests actually ran")
        .expect("the release workflow must still assert the PDFium tests ran");
    let step = &release[anchor..];
    let assertion = step
        .find("$passed -lt")
        .expect("the assertion must follow its own comment");
    // The comment block is the run of `#` lines immediately above the assertion. The match
    // starts at `$passed`, mid-line, so the partial line it sits on is discarded first --
    // the first version of this guard kept it, found a non-comment line, and collected
    // nothing at all, failing on its own bug rather than on the workflow's.
    let line_start = step[..assertion]
        .rfind('\n')
        .map_or(0, |newline| newline + 1);
    let comment: String = step[..line_start]
        .lines()
        .rev()
        .skip_while(|line| line.trim().is_empty())
        .take_while(|line| line.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n");

    assert!(
        comment.contains(&CI_WINDOWS_PASSED_FLOOR.to_string()),
        "the comment above release-windows.yml's floor does not state the number it enforces \
         ({CI_WINDOWS_PASSED_FLOOR}). A comment justifying a different number is how the previous \
         one came to say 1141 above an assertion of 1154.\n--- comment found ---\n{comment}"
    );
}

/// The two Rust literals in `ci_workflow.rs` are a *copy* of the workflow text. They must equal the
/// source here, otherwise that guard is checking the workflow against a third opinion.
#[test]
fn the_workflow_text_guard_agrees_with_the_source() {
    let guard = read("src-tauri/tests/ci_workflow.rs");

    // `tests_step.contains("ne N")` pins the ignored count.
    let ne_at = guard
        .find("contains(\"ne ")
        .expect("ci_workflow.rs must pin the ignored count");
    let ne_digits: String = guard[ne_at + "contains(\"ne ".len()..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    assert_eq!(
        ne_digits.parse::<usize>().unwrap(),
        CI_LINUX_IGNORED,
        "ci_workflow.rs pins a different ignored count than this file"
    );

    // `if [ "$passed" -lt N ]` pins the Linux floor as a literal string. The needle is a raw
    // string because the guard file itself uses `r#"..."#`; escaping it here would search for
    // backslashes the file does not contain.
    let needle = r#"if [ "$passed" -lt "#;
    let lt_at = guard
        .find(needle)
        .expect("ci_workflow.rs must pin the Linux passed floor as a literal");
    let lt_digits: String = guard[lt_at + needle.len()..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    assert_eq!(
        lt_digits.parse::<usize>().unwrap(),
        CI_LINUX_PASSED_FLOOR,
        "ci_workflow.rs pins a different Linux floor than this file"
    );
}

/// The arithmetic in the Windows comment is real: Linux and Windows counts differ by exactly the
/// number of tests that are compiled out off Linux. This is asserted as a fact about the two
/// numbers rather than trusted as prose, because a comment cannot be wrong out loud.
#[test]
fn the_windows_count_is_the_linux_count_less_the_tests_that_are_linux_only() {
    let linux_only = 3;
    assert_eq!(
        INCLUDED_PASSED - linux_only,
        CI_WINDOWS_PASSED_FLOOR,
        "the Windows floor must be the Linux include-ignored count less the tests compiled out \
         off Linux, and that difference must be explained rather than absorbed"
    );
}

/// The Linux floor cannot exceed what the tree reports, or CI would fail on a correct commit. It
/// also cannot sit below it, which is exactly the state this unit found it in, at 62 below.
///
/// Only the equality is asserted. An `assert!(CI_LINUX_PASSED_FLOOR <= PLAIN_PASSED)` beside it
/// would be the weaker half of the same fact *and* clippy is right that
/// `assertions_on_constants` deletes it, so it is not written: a guard the compiler removes is
/// worse than no guard, because it looks like one.
#[test]
fn the_linux_floor_is_reachable_and_not_vacuous() {
    assert_eq!(
        CI_LINUX_PASSED_FLOOR, PLAIN_PASSED,
        "the Linux floor must equal the measured count: above it, CI fails on a correct tree; \
         below it, the floor cannot fail and stops noticing tests that are no longer collected"
    );
}
