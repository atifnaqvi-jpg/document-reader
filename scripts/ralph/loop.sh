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
#   RALPH_AGENT_CMD   command used to run one iteration (default: openhands headless, see below)
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
  # The OpenHands CLI in headless mode reads the prompt from a file and exits when done.
  AGENT_CMD="openhands --headless --always-approve --exit-without-confirmation -f scripts/ralph/PROMPT.md"
fi

log()  { printf '\n[ralph] %s\n' "$*"; }
fail() { printf '\n[ralph] FAILED: %s\n' "$*" >&2; }

run_gate() {
  local status=0

  log "gate: cargo test"
  ( cd src-tauri && PDFIUM_LIBRARY_PATH="$PWD/resources/libpdfium.so" cargo test ) || status=1

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
    $AGENT_CMD || log "agent exited non-zero; continuing to gate"
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
    log "no commit since $start_sha; stopping (nothing was completed)"
    exit 0
  fi
  start_sha="$new_sha"

  if grep -q '^ITERATION .*BLOCKED' <(tail -5 scripts/ralph/LOOP_LOG.md); then
    log "the last iteration reported BLOCKED; stopping"
    exit 0
  fi
done

log "reached max iterations"
