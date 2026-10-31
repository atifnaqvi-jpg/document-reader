#!/usr/bin/env bash
# Ralph loop runner.
#
# Deliberately dumb: it does not choose tasks. Each iteration starts a fresh agent with
# scripts/ralph/PROMPT.md, which tells it to read the plan and pick the next task itself.
# The loop only enforces: one agent run, then the gate, then a log check.
#
# Usage:
#   ./scripts/ralph/loop.sh [--max-iterations N] [--agent-cmd "..."] [--dry-run]
#
# Environment:
#   RALPH_AGENT_CMD   command used to run one iteration (default: scripts/ralph/run-iteration.sh)
#   RALPH_GATE_ONLY   set to 1 to run the gate without invoking an agent (useful in CI)

set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$REPO_ROOT" || exit 1

MAX_ITERATIONS=10
DRY_RUN=0
AGENT_CMD="${RALPH_AGENT_CMD:-}"

while [ $# -gt 0 ]; do
  case "$1" in
    --max-iterations) MAX_ITERATIONS="$2"; shift 2 ;;
    --agent-cmd)      AGENT_CMD="$2"; shift 2 ;;
    --dry-run)        DRY_RUN=1; shift ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

if [ -z "$AGENT_CMD" ]; then
  # A fresh conversation on the local agent server. The CLI cannot be used here:
  # `openhands --headless` requires `~/.openhands/agent_settings.json`, which this
  # environment never writes (the Agent Canvas backend owns the settings).
  AGENT_CMD="scripts/ralph/run-iteration.sh"
fi

log()  { printf '\n[ralph] %s\n' "$*"; }
fail() { printf '\n[ralph] FAILED: %s\n' "$*" >&2; }

# Expected counts for each shape. These are pins, not observations: a count that moves must be
# an explicit decision, because the failure they exist to catch is a test that stopped running
# rather than a test that started failing.
PIN_PLAIN_PASSED=509
PIN_PLAIN_IGNORED=90
PIN_IGNORED_PASSED=599
PIN_IGNORED_IGNORED=0

# Run one cargo test shape, print its counts, and assert them against a pin. Prints the counts
# even when the run fails, because "0 passed" and "509 passed" are different problems and the
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

run_gate() {
  local status=0

  # Both shapes, always. The plain shape catches a test that stopped compiling or was deleted;
  # the --include-ignored shape is the only one that runs the PDFium-backed ink tests. Running
  # one and calling it green is how 89 skips stayed hidden for fifteen iterations.
  assert_shape "plain" "$PIN_PLAIN_PASSED" "$PIN_PLAIN_IGNORED" || status=1
  assert_shape "--include-ignored" "$PIN_IGNORED_PASSED" "$PIN_IGNORED_IGNORED" -- --include-ignored || status=1

  log "gate: cargo clippy"
  ( cd src-tauri && PDFIUM_LIBRARY_PATH="$PWD/resources/libpdfium.so" cargo clippy --all-targets ) || status=1

  log "gate: tsc"
  npx tsc --noEmit || status=1

  log "gate: vitest"
  npx vitest run || status=1

  return $status
}

# Refuse to start on a dirty tree: an iteration must build on a committed base, otherwise a
# failed iteration is indistinguishable from the previous one's leftovers.
if [ -n "$(git status --porcelain)" ] && [ "${RALPH_ALLOW_DIRTY:-0}" != "1" ]; then
  git status --short
  fail "working tree is dirty. Commit or stash first (RALPH_ALLOW_DIRTY=1 to override)."
  exit 1
fi

start_sha="$(git rev-parse --short HEAD)"
log "starting at $start_sha, max $MAX_ITERATIONS iteration(s)"

if [ "$DRY_RUN" = "1" ]; then
  log "dry run: would execute -> $AGENT_CMD"
  exit 0
fi

for iteration in $(seq 1 "$MAX_ITERATIONS"); do
  log "iteration $iteration"

  if [ "${RALPH_GATE_ONLY:-0}" != "1" ]; then
    # shellcheck disable=SC2086
    if ! $AGENT_CMD; then
      # A run that exits non-zero (timeout, step-ceiling truncation, no verdict) has not
      # completed its task. Falling through to "no commit => nothing was completed" reports
      # a truncation as a clean stop and hides the unfinished work, so stop here instead.
      fail "agent exited non-zero — the iteration is truncated, not complete"
      log "leaving the tree as-is for inspection; not proceeding"
      exit 1
    fi
  fi

  if run_gate; then
    log "gate green"
  else
    fail "gate red after iteration $iteration"
    log "leaving the tree as-is for inspection; not proceeding"
    exit 1
  fi

  if [ ! -s scripts/ralph/LOOP_LOG.md ]; then
    fail "LOOP_LOG.md is empty — the iteration did not record itself"
    exit 1
  fi

  new_sha="$(git rev-parse --short HEAD)"
  if [ "$new_sha" = "$start_sha" ]; then
    # Exit 0 here means "the agent ran, chose to finish, and committed nothing" — a normal
    # BLOCKED. It must not be reachable after a truncation, which is why a non-zero agent exit
    # stops the loop above. A dirty tree means work was done and left uncommitted.
    if [ -n "$(git status --porcelain)" ]; then
      git status --short
      fail "no commit since $start_sha, but the tree is dirty — the iteration did not finish"
      exit 1
    fi
    log "no commit since $start_sha; stopping (nothing was completed)"
    exit 0
  fi
  start_sha="$new_sha"

  # The durable record is the log heading; the agent also emits an ITERATION line in its
  # final message, but that only reaches run-iteration.sh's stdout, not this log.
  if grep -qiE '^(ITERATION .*BLOCKED|## Iteration .*blocked)' <(tail -5 scripts/ralph/LOOP_LOG.md); then
    log "the last iteration reported BLOCKED; stopping"
    exit 0
  fi
done

log "reached max iterations"
