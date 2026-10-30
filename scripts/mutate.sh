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
#   MUTATE_RUNTIME_READ=<path>   the guard reads this file at run time (see below)
#   MUTATE_TARGET=<path>         the file the caller mutated, for the evidence record
#   MUTATE_ALLOW_CACHED=1        harness self-test only; never a verdict
#   MUTATE_TIMEOUT=<seconds>     internal run budget; opts in to the TIMEOUT verdict
#   MUTATE_TIMEOUT_CONFIRM=<s>   budget for the run that separates a hang from a slow test
#                                (default: three times MUTATE_TIMEOUT)
#
# Run it after applying a mutant to a file the guard reads. It exits:
#   0  the mutant compiled and the tests failed  -> caught, as intended
#   1  the tests passed                          -> SURVIVED: coverage finding
#   2  the mutant did not compile               -> MEASUREMENT ERROR: not a catch
#   3  no summary line although the build was clean -> MEASUREMENT ERROR
#   4  freshness could not be established, or the build did not finish
#                                               -> MEASUREMENT ERROR: stale or undecidable
#   5  the run exceeded MUTATE_TIMEOUT          -> TIMEOUT: the walk did not terminate
#
# TIMEOUT is a verdict, not a measurement error, because it is the one outcome in which the
# mutant is known to have compiled, known to have run, and the symptom *is* non-termination.
# A guard that removes a termination check is caught by hanging, and no summary line will ever
# say so. It is distinguished from a slow-but-terminating run by the only thing that can
# distinguish them from outside: the caller's declared budget was exceeded while a named test
# was in flight. That budget is the opt-in; there is no second variable naming the expected
# test, because the in-flight test is already recorded on the record and the identity of the
# mutant is already declared by MUTATE_TARGET.
#
# MUTATE_TIMEOUT=<seconds> opts in. Unset means no internal budget and the behaviour is
# unchanged. The clock does not start when this script does: it starts at cargo's first
# `Finished` line, so neither a cold build nor the crate's own compile is charged to the run.
# Until that line appears the run is in phase=build, where a derived ceiling (twenty times the
# run budget, never below ten minutes) applies, and exceeding it is a MEASUREMENT ERROR --
# a build that did not finish says nothing about the mutant.
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

# The run budget, and what was observed when it was armed. Empty TIMEOUT_PHASE means the run
# finished on its own.
BUDGET="${MUTATE_TIMEOUT:-}"
if [ -n "$BUDGET" ] && ! [[ "$BUDGET" =~ ^[0-9]+$ ]]; then
    echo "MUTATE_TIMEOUT must be a whole number of seconds, got: $BUDGET" >&2
    exit 64
fi
TIMEOUT_PHASE=""
KILL_SIGNAL=""
BUDGET_START_SECONDS=""
BUDGET_MARKER=""
TIMEOUT_LAST_TEST=""
TIMEOUT_REPORTED=""
TIMEOUT_SUITES=""
CARGO_PID=""

# Which named test was in flight, from libtest's own lines. Only tests that already finished are
# visible: libtest writes `test NAME ... ` with no newline and flushes at the verdict, so a test
# that never returns leaves nothing readable in a redirected log. `last_test` is therefore the
# last one to *complete*, and a TIMEOUT means the hang is in the next one, or in the binary before
# it printed. The count is the progress measure; the name is a lower bound on where the run was.
last_test_name() {
    grep -oE '^test [A-Za-z0-9_:]+ \.\.\.' "$1" 2>/dev/null | tail -1 \
        | sed -E 's/^test //; s/[[:space:]]*\.\.\.[[:space:]]*$//'
}

tests_reported() {
    grep -cE '^test [A-Za-z0-9_:]+ \.\.\. (ok|FAILED|ignored)' "$1" 2>/dev/null || true
}

# The running-test header line, when the suite got far enough to print one. `running 1 test`
# followed by no completed test is a *different* shape from a run that never started, and it is
# the only in-flight statement the log carries.
suite_started() {
    grep -cE '^running [0-9]+ tests?$' "$1" 2>/dev/null || true
}

# The first `Finished` line is the build's own completion statement: `Compiling
# document-reader` is printed when the crate *starts* compiling, so arming there would charge
# the crate's own compile time to the run budget. Before `Finished` the run is in phase=build
# and only the derived ceiling applies.
budget_marker() {
    grep -m1 -E '^ *Finished ' "$1" 2>/dev/null
}

# Kills cargo and its children, not a named process pattern.
terminate_tree() {
    local sig="$1" pid="$2" kid grandkid
    for kid in $(ps -o pid= --ppid "$pid" 2>/dev/null); do
        for grandkid in $(ps -o pid= --ppid "$kid" 2>/dev/null); do
            kill -s "$sig" "$grandkid" 2>/dev/null
        done
        kill -s "$sig" "$kid" 2>/dev/null
    done
    kill -s "$sig" "$pid" 2>/dev/null
}

# Runs the suite under the declared budget. The clock is armed by cargo's build-complete line,
# not by this script's start, so a cold build cannot consume the run budget: before that line
# only a derived build ceiling applies (twenty times the run budget, never below ten minutes).
# The log is a file that grows, so the marker is re-read on every poll -- that is how a streamed
# build is observed without a pipe the harness would have to drain.
run_under_budget() {
    local budget="$1" logfile="$2"
    shift 2
    local elapsed start_seconds=0 build_ceiling=600
    if [ -n "$budget" ]; then
        local derived=$((budget * 20))
        [ "$derived" -gt "$build_ceiling" ] && build_ceiling=$derived
        echo "BUDGET: ${budget}s run budget, build ceiling ${build_ceiling}s, armed at the first build-complete line" >&2
    fi

    cargo test --release "$@" >"$logfile" 2>&1 &
    CARGO_PID=$!

    while kill -0 "$CARGO_PID" 2>/dev/null; do
        sleep 1
        elapsed=$SECONDS

        if [ -n "$budget" ] && [ -z "$BUDGET_START_SECONDS" ]; then
            BUDGET_MARKER=$(budget_marker "$logfile")
            if [ -n "$BUDGET_MARKER" ]; then
                BUDGET_START_SECONDS=$elapsed
                start_seconds=$elapsed
                echo "BUDGET: run clock armed after ${elapsed}s at: ${BUDGET_MARKER}" >&2
            elif [ "$elapsed" -ge "$build_ceiling" ]; then
                TIMEOUT_PHASE="build"
                break
            fi
        elif [ -n "$budget" ] && [ $((elapsed - start_seconds)) -ge "$budget" ]; then
            TIMEOUT_PHASE="run"
            break
        fi
    done

    if [ -n "$TIMEOUT_PHASE" ]; then
        # Captured before the kill, because the log stops changing when the process dies.
        TIMEOUT_LAST_TEST=$(last_test_name "$logfile")
        TIMEOUT_REPORTED=$(tests_reported "$logfile")
        TIMEOUT_SUITES=$(suite_started "$logfile")
        if [ "$TIMEOUT_PHASE" = "build" ]; then
            echo "BUDGET: the build exceeded ${build_ceiling}s; killing it." >&2
        else
            echo "BUDGET: ${budget}s elapsed after the build; killing the run." >&2
        fi
        terminate_tree TERM "$CARGO_PID"
        KILL_SIGNAL="TERM"
        local grace=0
        while kill -0 "$CARGO_PID" 2>/dev/null && [ "$grace" -lt 10 ]; do
            sleep 1
            grace=$((grace + 1))
        done
        if kill -0 "$CARGO_PID" 2>/dev/null; then
            terminate_tree KILL "$CARGO_PID"
            KILL_SIGNAL="KILL(after TERM)"
        fi
    fi

    wait "$CARGO_PID"
    status=$?
}

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
        if [ -n "$TIMEOUT_PHASE" ]; then
            printf 'budget: %ss phase=%s signal=%s\n' "$BUDGET" "$TIMEOUT_PHASE" "$KILL_SIGNAL"
            printf 'budget_armed: %s\n' "${BUDGET_MARKER:-<never; build did not complete>}"
            printf 'suites_started: %s\n' "${TIMEOUT_SUITES:-0}"
            printf 'last_test_completed: %s\n' "${TIMEOUT_LAST_TEST:-<none>}"
            printf 'tests_reported: %s\n' "${TIMEOUT_REPORTED:-0}"
            if [ -n "${TIMEOUT_CONF_S:-}" ]; then
                printf 'confirmation_budget: %ss outcome=%s\n' "$TIMEOUT_CONF_S" "${TIMEOUT_CONF_OUTCOME:-unknown}"
            fi
            if [ "$TIMEOUT_PHASE" = "run" ]; then
                printf 'TIMEOUT_CLAIM: verdict=TIMEOUT rests on TWO runs, %ss and %ss,\n' "$BUDGET" "${TIMEOUT_CONF_S:-?}"
                printf 'TIMEOUT_CLAIM: each with the build complete (%s) and no summary line.\n' "$BUDGET_MARKER"
                printf 'TIMEOUT_CLAIM: A slow suite terminates at the larger budget and reports; this one did not.\n'
                printf 'TIMEOUT_CLAIM: Progress in the redirect cannot say which test hung -- libtest flushes a header\n'
                printf 'TIMEOUT_CLAIM: only at its verdict -- so the identity of the hung test is NOT claimed here.\n'
                printf 'TIMEOUT_CLAIM: last_test_completed is the last one that returned. Pair with MUTATE_TARGET.\n'
            elif [ "$TIMEOUT_PHASE" = "stale" ]; then
                printf 'TIMEOUT_CLAIM: the run hung, but cargo never compiled document-reader, so the\n'
                printf 'TIMEOUT_CLAIM: binary may not have contained the mutant. Not a catch.\n'
            elif [ "$TIMEOUT_PHASE" = "slow" ]; then
                printf 'TIMEOUT_CLAIM: the run exceeded %ss but the confirmation run completed.\n' "$BUDGET"
                printf 'TIMEOUT_CLAIM: verdict is MEASUREMENT_ERROR, not TIMEOUT: slowness, not non-termination.\n'
            else
                printf 'TIMEOUT_CLAIM: the build never completed, so no test ran and nothing about the\n'
                printf 'TIMEOUT_CLAIM: mutant was observed. This is a measurement error, not a catch.\n'
            fi
        fi
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

TIMEOUT_CONFIRM_S="${MUTATE_TIMEOUT_CONFIRM:-}"
run_under_budget "$BUDGET" "$log" "$@"

# A timeout is classified before anything else, because a hung run has no summary line and would
# otherwise be reported as a missing one. The build phase is separated first: a build that did
# not finish says nothing about the mutant.
if [ "$TIMEOUT_PHASE" = "build" ]; then
    echo "MEASUREMENT ERROR: the build did not finish inside ${BUDGET}s x 20, so no test ran." >&2
    echo "Raise MUTATE_TIMEOUT or warm the build cache; nothing about the mutant was observed." >&2
    STALE_BRANCH="timeout_during_build"
    record_evidence "MEASUREMENT_ERROR:timeout_during_build" 4
    exit 4
fi

if [ "$TIMEOUT_PHASE" = "run" ] && ! grep -qE '^test result:' "$log"; then
    # The same freshness rule the passing paths use. A hang is only evidence if the binary that
    # hung contained the mutant, and for a compiled-in guard the only proof of that is cargo's
    # own `Compiling document-reader`. Checked on the FIRST log: the confirmation run is
    # legitimately cached, so its silence is expected and is not evidence either way.
    if [ "$RT_CLASS" = "compiled-in" ] && [ "${MUTATE_ALLOW_CACHED:-0}" != "1" ] \
        && ! grep -qE '^ *Compiling document-reader' "$log"; then
        echo "MEASUREMENT ERROR: the run hung, but cargo never compiled document-reader," >&2
        echo "so the binary that hung may not contain the mutant. Touch the mutated file and" >&2
        echo "re-run; if you meant to test the harness itself, set MUTATE_ALLOW_CACHED=1." >&2
        STALE_BRANCH="timeout_but_no_compile"
        TIMEOUT_PHASE="stale"
        record_evidence "MEASUREMENT_ERROR:timeout_but_no_compile" 4
        exit 4
    fi

    # Progress in the log cannot separate a hang from a slow test: libtest flushes a test header
    # only at its verdict, so a test that never returns and a test that is merely slow both leave
    # one `running N test` line and nothing else. The only sound discriminator is a second run at
    # a larger budget: a slow test terminates and reports, a hang does not, at any budget.
    confirm="${TIMEOUT_CONFIRM_S:-$((BUDGET * 3))}"
    echo "BUDGET: first run exceeded ${BUDGET}s with ${TIMEOUT_REPORTED:-0} completed test(s)."
    echo "BUDGET: confirming at ${confirm}s; a slow test terminates there, a hang does not."
    FIRST_KILL="$KILL_SIGNAL"
    FIRST_REPORTED="$TIMEOUT_REPORTED"
    FIRST_LAST="$TIMEOUT_LAST_TEST"
    FIRST_SUITES="$TIMEOUT_SUITES"
    TIMEOUT_PHASE=""
    KILL_SIGNAL=""
    BUDGET_START_SECONDS=""
    BUDGET_MARKER=""
    confirm_log=$(mktemp)
    run_under_budget "$confirm" "$confirm_log" "$@"
    if [ -z "$TIMEOUT_PHASE" ] && grep -qE '^test result:' "$confirm_log"; then
        echo "MEASUREMENT ERROR: the run exceeded ${BUDGET}s but completed inside ${confirm}s." >&2
        echo "That is a slow test, not a removed termination guard: nothing was proved about" >&2
        echo "non-termination. Raise MUTATE_TIMEOUT above the suite's real cost and re-run." >&2
        grep -E '^test result:' "$confirm_log" | tail -1 >&2
        STALE_BRANCH="slow_not_hung"
        TIMEOUT_PHASE="slow"
        TIMEOUT_CONF_S="$confirm"
        TIMEOUT_CONF_OUTCOME="completed"
        record_evidence "MEASUREMENT_ERROR:slow_not_hung" 3
        rm -f "$confirm_log"
        exit 3
    fi
    echo "TIMEOUT: the mutant compiled, the run began, and it exceeded both ${BUDGET}s and ${confirm}s."
    echo "First run: ${FIRST_SUITES:-0} suite(s) started, ${FIRST_REPORTED:-0} test(s) completed, last completed ${FIRST_LAST:-<none>}, killed by ${FIRST_KILL}."
    echo "Confirmation run: no summary line at ${confirm}s either."
    echo "Both runs share one shape and only non-termination explains it. This is a removed"
    echo "termination guard caught by hanging, not a slow suite."
    TIMEOUT_CONF_S="$confirm"
    TIMEOUT_CONF_OUTCOME="no_summary_at_${confirm}s"
    record_evidence "TIMEOUT" 5
    rm -f "$confirm_log"
    exit 5
fi

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

# A summary line is not the same thing as a test having run.
#
# Measured failure this closes: `scripts/mutate.sh test --test type0_row6` passes `test`
# twice -- this script already supplies it -- so cargo read the second one as a *filter*.
# It matched nothing, zero tests executed, and the harness reported SURVIVED against a clean
# tree. The summary line read `ok. 0 passed; 0 failed; ... 4 filtered out`, which looks like
# a pass in every respect except the counts. "No test failed" and "no test ran" are the same
# sentence until something counts, so count.
#
# `passed` + `failed` is what actually executed: an ignored test is selected but not run, and
# a filtered test is not even selected. Either way it proves nothing about the mutant.
SUMMARY_LINE=$(grep -E '^test result:' "$log" | tail -1)
TESTS_RUN=$(printf '%s\n' "$SUMMARY_LINE" | awk '{
    for (i = 1; i <= NF; i++) {
        if ($i == "passed;") p = $(i-1);
        if ($i == "failed;") f = $(i-1);
        if ($i == "filtered") g = $(i-1);
    }
    print p + f
}')
TESTS_FILTERED=$(printf '%s\n' "$SUMMARY_LINE" | awk '{
    for (i = 1; i <= NF; i++) if ($i == "filtered") g = $(i-1);
    print g + 0
}')

if [ "${TESTS_RUN:-0}" -eq 0 ]; then
    if [ "${TESTS_FILTERED:-0}" -gt 0 ]; then
        echo "MEASUREMENT ERROR: 0 tests ran; the filter matched nothing." >&2
        echo "$TESTS_FILTERED test(s) were deselected by a name filter, so no assertion was" >&2
        echo "evaluated and the green summary belongs to the filter, not to the mutant." >&2
        echo "Check the arguments: this script already supplies the 'test' subcommand, so" >&2
        echo "passing it again makes it a filter. Use e.g. 'mutate.sh --test my_target'." >&2
        STALE_BRANCH="zero_tests_filter_matched_nothing"
    else
        echo "MEASUREMENT ERROR: 0 tests ran; the target contains no runnable tests." >&2
        echo "The summary line is green because there was nothing to evaluate." >&2
        STALE_BRANCH="zero_tests_target_empty"
    fi
    printf 'summary: %s\n' "$SUMMARY_LINE" >&2
    record_evidence "MEASUREMENT_ERROR:zero_tests_ran" 3
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
