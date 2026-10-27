#!/usr/bin/env bash
# Mutation harness for guards. A mutant counts as caught only when the test run both
# produced a summary line AND failed.
#
# Why this exists: during P2.7.6 R4 two mutants were read as "caught" because
# `cargo test | grep "^test result"` printed nothing. Both had failed to compile
# (E0433, E0004), so no test ran at all. "No summary line" and "no failure" look
# identical to a grep, and the difference between them is the difference between a
# coverage finding and a measurement error. This script refuses to conflate them.
#
# A third shape arrived later: a real summary line from a *stale* build, where cargo
# decided nothing had changed and never recompiled the mutant. The test suite was
# green because the mutant was not in the binary. Requiring a rebuild (exit 4) closes
# it. Both the log line and an artifact fingerprint are checked: the line catches the
# obvious case, the fingerprint catches the one where cargo prints "Compiling" for a
# dependency but not for the crate under test.
#
# Usage:
#   scripts/mutate.sh <cargo-test-args...>
#
# Run it after applying a mutant to a file the guard reads. It exits:
#   0  the mutant compiled and the tests failed  -> caught, as intended
#   1  the tests passed                          -> SURVIVED: coverage finding
#   2  the mutant did not compile               -> MEASUREMENT ERROR: not a catch
#   3  no summary line although the build was clean -> MEASUREMENT ERROR
#   4  freshness could not be established       -> MEASUREMENT ERROR: stale or undecidable
#
# Two classes of guard, and the distinction is load-bearing:
#
#   COMPILED-IN   the guard reaches the mutant through a compiled-in path, so a mutant that
#                 is not recompiled cannot be executed. The `Compiling document-reader` rule
#                 below is correct and required.
#
#   RUNTIME-READ  the guard calls std::fs on a file at run time (.github/workflows/ci.yml,
#                 Cargo.toml, a doc, a .tsx). The file is not a source dependency, so cargo has
#                 no reason to rebuild and no `Compiling` line will appear -- not because the
#                 run was stale, but because freshness of the binary is irrelevant to whether
#                 the mutant was observed. Keying exit 4 on `Compiling` here fails every
#                 correctly-run mutation, and silently accepting it would let a genuinely
#                 stale run through. So the class must be declared, the evidence differs, and
#                 the record states which one the verdict rests on.
#
# Declare a runtime-read target with:
#   MUTATE_RUNTIME_READ=<path the guard reads at run time>
# and the harness will log a RUNTIME_READ line on every path, so a verdict that rests on a
# run-time file read is never mistaken for one that rests on a rebuild.
#
# It never edits or restores files: restoring is the caller's job, and a harness that
# silently reverts your work is worse than one that tells you it cannot decide.

set -uo pipefail

if [ "$#" -eq 0 ]; then
    echo "usage: $0 <cargo test args...>" >&2
    exit 64
fi

REPO_ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$REPO_ROOT/src-tauri" || exit 64
RUN_ARGS="$*"

# Everything the freshness decision needs, captured before cargo starts. RUN_START is an
# ISO timestamp for the log and an epoch value for comparisons.
RUN_START_EPOCH=$(date -u +%s)
RUN_START=$(date -u -d "@$RUN_START_EPOCH" +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || date -u +%Y-%m-%dT%H:%M:%SZ)

# The runtime-read target, relative to the repo root or absolute. Normalised to an absolute
# path once, so every later comparison is against the same file.
RT_PATH=""
RT_CLASS=""
if [ -n "${MUTATE_RUNTIME_READ:-}" ]; then
    case "$MUTATE_RUNTIME_READ" in
        /*) RT_PATH="$MUTATE_RUNTIME_READ" ;;
        *)  RT_PATH="$REPO_ROOT/$MUTATE_RUNTIME_READ" ;;
    esac
    RT_CLASS="runtime-read"
else
    RT_CLASS="compiled-in"
fi

log=$(mktemp)

# Durable evidence trail. The mktemp log above is still the working buffer, but it is deleted
# on exit, so until this existed no mutation verdict outlived the shell that produced it: the
# only surviving record was whatever the caller happened to paste into a history file. Append
# one record per run, for every exit path including the measurement errors.
#
# MUTATE_TARGET names the file the caller mutated. It cannot be inferred reliably: this script
# never sees the edit, and a stale/void mutant is exactly the case a record must not conceal.
# MUTATE_TARGET_SHA records the tree the mutant was applied to, so a verdict can be tied to a
# commit after the fact.
MUTATE_LOG_DIR="${MUTATE_LOG_DIR:-$REPO_ROOT/.mutation-log}"
mkdir -p "$MUTATE_LOG_DIR"
durable_log="$MUTATE_LOG_DIR/$(date -u +%Y-%m-%d).log"

# One record: timestamp, verdict+exit, tree state, target, args, applied diff, summary.
# The diff is the mutation text, taken from git so it reflects what is actually on disk rather
# than what the caller intended to write.
record_evidence() {
    local verdict="$1" code="$2"
    local summary
    summary=$(grep -E '^test result:' "$log" 2>/dev/null | tail -1)
    local target_sha
    target_sha=$(git -C "$REPO_ROOT" rev-parse HEAD 2>/dev/null || echo unknown)
    # Which binary ran, when it ran, and whether an observable run-time file was touched
    # after it. These are the facts the verdict's freshness claim rests on; recording them
    # means a later reader does not have to trust this run's phrasing.
    local rt_mtime="not-applicable"
    if [ -n "$RT_PATH" ]; then
        rt_mtime=$(stat -c '%y' "$RT_PATH" 2>/dev/null || echo "missing")
    fi
    local bin_path="${BINARY:-<none>}"
    local bin_mtime="missing"
    if [ -n "${BINARY:-}" ] && [ -e "$BINARY" ]; then
        bin_mtime=$(stat -c '%y' "$BINARY" 2>/dev/null || echo "missing")
    fi
    {
        printf '=== %s UTC exit=%s verdict=%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$code" "$verdict"
        printf 'target: %s\n' "${MUTATE_TARGET:-<unset>}"
        printf 'tree_sha: %s\n' "$target_sha"
        printf 'args: %s\n' "$RUN_ARGS"
        printf 'summary: %s\n' "${summary:-<none>}"
        printf 'stale_check: %s\n' "${STALE_BRANCH:-not-evaluated}"
        printf 'run_start: %s\n' "$RUN_START"
        printf 'binary: %s mtime=%s\n' "$bin_path" "$bin_mtime"
        if [ -n "$RT_PATH" ]; then
            printf 'RUNTIME_READ: %s mtime=%s\n' "$RT_PATH" "$rt_mtime"
            printf 'RUNTIME_READ: staleness check rests on a run-time file read, not on a rebuild;\n'
            printf 'RUNTIME_READ: no Compiling line is required and its absence is not evidence of staleness.\n'
        fi
        printf 'cached_override: %s\n' "${MUTATE_ALLOW_CACHED:-0}"
        printf 'diff:\n'
        git -C "$REPO_ROOT" --no-pager diff --no-color -- "${MUTATE_TARGET:-}" 2>/dev/null | sed 's/^/  /'
        printf 'end\n\n'
    } >>"$durable_log"
    echo "evidence appended: $durable_log"
}
trap 'rm -f "$log"' EXIT

# Fingerprint the binary that this run executes. The target is named by `--test NAME`
# (integration binary) or `--lib` (unit binary); several builds of each accumulate, so take
# the newest. Fingerprinting the lib binary while running an integration test compares the
# wrong artifact and produces a false stale-build finding.
artifact() {
    local pattern='document_reader_lib-*'
    local prev=''
    for arg in "$@"; do
        if [ "$prev" = "--test" ]; then pattern="$(printf '%s' "$arg" | tr '-' '_')-*"; fi
        prev=$arg
    done
    find target/release/deps -maxdepth 1 -name "$pattern" -type f -perm -u+x \
        -printf '%T@ %p\n' 2>/dev/null | sort -n | tail -1 | cut -d' ' -f2-
}

cargo test --release "$@" >"$log" 2>&1
status=$?

# Only *compilation* failures count. `error: test failed, to rerun pass ...` is the normal
# footer of a caught mutant, so matching a bare `^error:` misclassifies a real catch as a
# measurement error -- observed on the first run of this script against M1 below.
if grep -qE '^error\[|^error: could not compile|could not compile `' "$log"; then
    echo "MEASUREMENT ERROR: the mutant did not compile, so no test ran." >&2
    echo "This is not a catch. Fix the mutant until it builds, then re-run." >&2
    grep -E '^error\[|^error: could not compile|could not compile `' "$log" | head -5 >&2
    record_evidence "MEASUREMENT_ERROR:did_not_compile" 2
    exit 2
fi

if ! grep -qE '^test result:' "$log"; then
    echo "MEASUREMENT ERROR: the build succeeded but no 'test result:' line appeared." >&2
    echo "No test ran, so nothing was proved. Check the test target name." >&2
    record_evidence "MEASUREMENT_ERROR:no_summary" 3
    exit 3
fi

# Did the run observe the mutant?
#
# This is the load-bearing check, and what counts as evidence depends on how the guard
# reaches the mutated file. See the class note at the top of this script.
#
# MUTATE_ALLOW_CACHED=1 skips it, for self-testing the harness against a clean tree. That
# flag is not a verdict: every record it touches says cached_override: 1.

BINARY=$(artifact "$@")

if [ "$RT_CLASS" = "compiled-in" ]; then
    # The observed failure was a `Finished in 0.01s` run where cargo concluded nothing had
    # changed, so the tests executed a binary that never contained the mutant, and the suite
    # came back green. A summary line cannot distinguish that from a real pass.
    #
    # An mtime comparison cannot catch it either: when the mutated source is *older* than the
    # binary (preserved timestamp, clock skew, a file written before the last build), the
    # binary is "newer than its sources" and looks current. Only cargo's own decision -- did
    # it emit `Compiling document-reader`? -- reports what it actually did.
    if ! grep -qE '^ *Compiling document-reader' "$log"; then
        if [ "${MUTATE_ALLOW_CACHED:-0}" = "1" ]; then
            echo "NOTE: no 'Compiling document-reader' line; MUTATE_ALLOW_CACHED=1 permits it." >&2
            STALE_BRANCH="compiled_in_override_no_compile"
        else
            echo "MEASUREMENT ERROR: cargo did not compile document-reader, so the mutant never" >&2
            echo "ran and the summary line came from a stale binary. Touch the mutated file and" >&2
            echo "re-run; if you meant to test the harness itself, set MUTATE_ALLOW_CACHED=1." >&2
            grep -E '^ *(Compiling|Finished)' "$log" | head -5 >&2
            STALE_BRANCH="compiled_in_no_compile"
            record_evidence "MEASUREMENT_ERROR:stale_build_no_compile" 4
            exit 4
        fi
    else
        STALE_BRANCH="compiled_in_rebuilt"
    fi

    # Secondary: even with a `Compiling` line, a binary older than the newest source means
    # the build that ran did not include the edit.
    newest_source=$(find src tests -name '*.rs' -printf '%T@\n' 2>/dev/null | sort -n | tail -1 | cut -d. -f1)
    binary_mtime=$( [ -n "$BINARY" ] && stat -c '%Y' "$BINARY" 2>/dev/null || echo "" )
    if [ "${MUTATE_ALLOW_CACHED:-0}" != "1" ] && [ -n "$binary_mtime" ] && [ -n "$newest_source" ] \
        && [ "$binary_mtime" -lt "$newest_source" ]; then
        echo "MEASUREMENT ERROR: the test binary predates the newest source file." >&2
        echo "The mutant is not in the binary that ran." >&2
        STALE_BRANCH="compiled_in_binary_predates_source"
        record_evidence "MEASUREMENT_ERROR:binary_predates_source" 4
        exit 4
    fi
else
    # RUNTIME-READ. The binary is irrelevant: the guard opened the file during the run, so
    # what must hold is that the file it opened already carried the mutation. Three checks,
    # and each can fail on its own:
    #
    #   1. the declared runtime-read path exists -- a typo must not pass as a verdict;
    #   2. it was modified at or before the run started -- a mutation applied *after* cargo
    #      began is not what the run observed, even though it is on disk now;
    #   3. it is still modified relative to HEAD -- if the caller restored the file before
    #      this script got to it, the mutation text is gone and the verdict describes a
    #      tree that no longer exists.
    #
    # What cannot be established, and is therefore stated rather than implied: that the
    # file was unchanged between the run's read and the comparison above. A concurrent edit
    # during a 0.1s window is not testable from outside; the log says so on every record.
    if [ ! -e "$RT_PATH" ]; then
        echo "MEASUREMENT ERROR: MUTATE_RUNTIME_READ names a path that does not exist:" >&2
        echo "  $RT_PATH" >&2
        echo "The guard cannot have read a mutant from it." >&2
        STALE_BRANCH="runtime_read_target_missing"
        record_evidence "MEASUREMENT_ERROR:runtime_read_target_missing" 4
        exit 4
    fi

    rt_mtime_epoch=$(stat -c '%Y' "$RT_PATH" 2>/dev/null || echo "")
    if [ -z "$rt_mtime_epoch" ] || [ "$rt_mtime_epoch" -gt "$RUN_START_EPOCH" ]; then
        echo "MEASUREMENT ERROR: the runtime-read target was modified after this run started," >&2
        echo "so the mutant on disk now is not what the run observed:" >&2
        echo "  $RT_PATH (mtime ${rt_mtime_epoch:-unknown}, run start $RUN_START_EPOCH)" >&2
        STALE_BRANCH="runtime_read_modified_after_run_start"
        record_evidence "MEASUREMENT_ERROR:runtime_read_modified_after_run_start" 4
        exit 4
    fi

    if ! git -C "$REPO_ROOT" diff --quiet -- "$RT_PATH" 2>/dev/null; then
        STALE_BRANCH="runtime_read_mutated_before_run_start"
    else
        if [ "${MUTATE_ALLOW_CACHED:-0}" = "1" ]; then
            echo "NOTE: the runtime-read target is unmodified relative to HEAD;" >&2
            echo "MUTATE_ALLOW_CACHED=1 permits it." >&2
            STALE_BRANCH="runtime_read_override_unmodified"
        else
            echo "MEASUREMENT ERROR: the runtime-read target is not modified relative to HEAD." >&2
            echo "Nothing distinguishes this run from one against an unmutated tree:" >&2
            echo "  $RT_PATH" >&2
            STALE_BRANCH="runtime_read_unmodified"
            record_evidence "MEASUREMENT_ERROR:runtime_read_unmodified" 4
            exit 4
        fi
    fi
fi

if [ "$status" -eq 0 ]; then
    echo "SURVIVED: the tests passed against the mutant." >&2
    echo "That is a coverage finding, not a measurement error." >&2
    record_evidence "SURVIVED" 1
    exit 1
fi

echo "CAUGHT: the mutant compiled and the tests failed."
grep -E '^test result:' "$log" | tail -1
record_evidence "CAUGHT" 0
exit 0
