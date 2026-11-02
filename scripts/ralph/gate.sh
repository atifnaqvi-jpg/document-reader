#!/usr/bin/env bash
# The Ralph gate. Runs fmt, both `cargo test` shapes, clippy with warnings denied, tsc and
# vitest, and asserts the two test-count pins.
#
# This is a separate process on purpose. It used to be a function inside loop.sh, which meant
# bash parsed the pins once when the loop started and then reused them for every iteration.
# An iteration that adds a test legitimately moves a count and updates the pin in its own
# commit, and the gate then compared the new count against the pin from before the change --
# reporting a false red on correct work and leaving a green commit un-pushed. Executing this
# file afresh is what makes the pins current.
#
# Reading the file afresh is only enough if the pins in it are the committed ones, so the
# self-check below refuses to run otherwise. Without it, the gate would trust a working-tree
# edit to the pins, which is a weaker guarantee than "as committed" while looking identical.
#
# Usage: scripts/ralph/gate.sh      (from anywhere; it cd's to the repo root)
# Exit:  0 green, 1 red.

set -uo pipefail

# Every `run:` command in .github/workflows/ci.yml is either executed below or declared here.
# `tests/gate_parity.rs` asserts exactly that, so a new CI step fails the suite until someone
# decides which it is. Without this block the two lists drift silently, which they have done
# twice: fmt was in CI and absent from the gate until 144f65c, and clippy ran without
# `-D warnings` until dfb8549. A gate that is a subset of CI reports green on a tree CI rejects.
#
# The block is a set of decisions, not a wildcard. Each entry is
#   ci-exempt: <ci.yml command> ;; covers: <command run here, or `none`> ;; <reason>
# and the test asserts three things about it: the named command still exists in ci.yml (an
# excuse cannot outlive its step), the claimed `covers:` command is still run here (deleting a
# check cannot hide behind an old excuse), and no entry is a prefix of another (which would let
# one entry excuse a family of steps).
#
# >>> ci-exemptions -- parsed by src-tauri/tests/gate_parity.rs
# ci-exempt: sudo apt-get update ;; covers: none ;; platform: CI installs Tauri's system libraries; this machine has them
# ci-exempt: cargo test 2>&1 | tee /tmp/test-output.txt ;; covers: cargo test ;; the gate runs both shapes as pinned counts instead of CI's shell script of floors
# ci-exempt: npm ci ;; covers: none ;; install path, not a check; node_modules is already present on this machine
# ci-exempt: npm test ;; covers: npx vitest run ;; `npm test` is `vitest run`; the gate calls it directly
# ci-exempt: npm run build ;; covers: npx tsc --noEmit ;; `build` is `tsc --noEmit && vite build`; the typecheck is the check, the bundle is not
# ci-exempt: ./scripts/fetch-pdfium.ps1 ;; covers: none ;; pwsh fetches the Windows DLL; this machine loads resources/libpdfium.so
# ci-exempt: cargo check --target x86_64-pc-windows-msvc ;; covers: none ;; Windows-only target; no toolchain for it here
# ci-exempt: cargo test --test memory -- --include-ignored --nocapture ;; covers: cargo test "$@" ;; the gate's ignored shape runs the same target with the local library
# ci-exempt: $output = cargo test --test embed_acceptance ;; covers: cargo test "$@" ;; same, one test, same library
# ci-exempt: $output = cargo test -- --include-ignored ;; covers: cargo test "$@" ;; same suite, plus the assertion that nothing stayed ignored
# <<< ci-exemptions

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$REPO_ROOT" || exit 1

log()  { printf '\n[ralph] %s\n' "$*"; }
fail() { printf '\n[ralph] FAILED: %s\n' "$*" >&2; }

# The pins must be committed before they are trusted. A dirty pin file means the number being
# asserted is not the number the tree claims, which is the same confusion in a new disguise.
if ! git diff --quiet HEAD -- scripts/ralph/gate.sh; then
  fail "scripts/ralph/gate.sh has uncommitted changes — the pins would not be the committed ones"
  git diff --stat HEAD -- scripts/ralph/gate.sh >&2
  exit 1
fi

# Expected counts for each shape. These are pins, not observations: a count that moves must be
# an explicit decision, because the failure they exist to catch is a test that stopped running
# rather than a test that started failing.
PIN_PLAIN_PASSED=531
PIN_PLAIN_IGNORED=93
PIN_IGNORED_PASSED=624
PIN_IGNORED_IGNORED=0

# Run one cargo test shape, print its counts, and assert them against a pin. Prints the counts
# even when the run fails, because "0 passed" and "510 passed" are different problems and the
# log must say which one happened.
assert_shape() {
  local shape="$1" want_passed="$2" want_ignored="$3"
  shift 3
  local out got_passed got_ignored rc=0

  log "gate: cargo test ($shape)"
  out="$( cd src-tauri && PDFIUM_LIBRARY_PATH="$PWD/resources/libpdfium.so" cargo test "$@" 2>&1 )" || rc=$?
  printf '%s\n' "$out" | grep -E '^(test result|error|warning: unused)' | sed 's/^/    /'

  got_passed="$(printf '%s\n' "$out" | awk '/^test result/ {p+=$4} END {print p+0}')"
  got_ignored="$(printf '%s\n' "$out" | awk '/^test result/ {i+=$8} END {print i+0}')"

  if [ "$rc" -ne 0 ]; then
    fail "$shape: cargo test exited $rc (ran $got_passed passed, $got_ignored ignored)"
    return 1
  fi
  if [ "$got_passed" -ne "$want_passed" ]; then
    fail "$shape: expected $want_passed passed, got $got_passed — a test appeared, vanished or \
stopped running. This is a count change, not a green suite; decide it before proceeding."
    return 1
  fi
  if [ "$got_ignored" -ne "$want_ignored" ]; then
    fail "$shape: expected $want_ignored ignored, got $got_ignored — tests are being skipped \
silently. A skip is not a pass."
    return 1
  fi
  log "gate: $shape ok — $got_passed passed, $got_ignored ignored (pinned)"
  return 0
}

status=0

# Both shapes, always. The plain shape catches a test that stopped compiling or was deleted;
# the --include-ignored shape is the only one that runs the PDFium-backed ink tests. Running
# one and calling it green is how 89 skips stayed hidden for fifteen iterations.
assert_shape "plain" "$PIN_PLAIN_PASSED" "$PIN_PLAIN_IGNORED" || status=1
assert_shape "--include-ignored" "$PIN_IGNORED_PASSED" "$PIN_IGNORED_IGNORED" -- --include-ignored || status=1

# Before clippy, and before anything that compiles for minutes. CI runs this as its own step,
# and until iteration 18 the gate did not run it at all: 714f2a9 passed the gate and failed CI
# on Format alone, so "gate green" never implied "CI green". rustfmt is fast enough that there
# is no reason to order it late.
log "gate: cargo fmt --check"
( cd src-tauri && cargo fmt --check ) || status=1

log "gate: cargo clippy"
( cd src-tauri && PDFIUM_LIBRARY_PATH="$PWD/resources/libpdfium.so" cargo clippy --all-targets -- -D warnings ) || status=1

log "gate: tsc"
npx tsc --noEmit || status=1

log "gate: vitest"
npx vitest run || status=1

if [ "$status" -eq 0 ]; then
  log "gate green"
else
  log "gate red"
fi
exit "$status"
