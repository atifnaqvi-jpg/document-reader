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
#   4  the crate did not rebuild                -> MEASUREMENT ERROR: stale binary
#
# It never edits or restores files: restoring is the caller's job, and a harness that
# silently reverts your work is worse than one that tells you it cannot decide.

set -uo pipefail

if [ "$#" -eq 0 ]; then
    echo "usage: $0 <cargo test args...>" >&2
    exit 64
fi

cd "$(dirname "$0")/../src-tauri" || exit 64

log=$(mktemp)
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
    exit 2
fi

if ! grep -qE '^test result:' "$log"; then
    echo "MEASUREMENT ERROR: the build succeeded but no 'test result:' line appeared." >&2
    echo "No test ran, so nothing was proved. Check the test target name." >&2
    exit 3
fi

# Did the crate under test actually get compiled?
#
# This is the load-bearing check, and it is deliberately strict. The observed failure was a
# `Finished in 0.01s` run where cargo concluded nothing had changed, so the tests executed a
# binary that never contained the mutant, and the suite came back green. A summary line
# cannot distinguish that from a real pass.
#
# An mtime comparison cannot catch it either: when the mutated source is *older* than the
# binary (preserved timestamp, clock skew, a file written before the last build), the
# binary is "newer than its sources" and looks current. Only cargo's own decision -- did it
# emit `Compiling document-reader`? -- reports what it actually did.
#
# MUTATE_ALLOW_CACHED=1 skips this, for self-testing the harness against a clean tree.
if ! grep -qE '^ *Compiling document-reader' "$log"; then
    if [ "${MUTATE_ALLOW_CACHED:-0}" = "1" ]; then
        echo "NOTE: no 'Compiling document-reader' line; MUTATE_ALLOW_CACHED=1 permits it." >&2
    else
        echo "MEASUREMENT ERROR: cargo did not compile document-reader, so the mutant never" >&2
        echo "ran and the summary line came from a stale binary. Touch the mutated file and" >&2
        echo "re-run; if you meant to test the harness itself, set MUTATE_ALLOW_CACHED=1." >&2
        grep -E '^ *(Compiling|Finished)' "$log" | head -5 >&2
        exit 4
    fi
fi

# Secondary: even with a `Compiling` line, a binary older than the newest source means the
# build that ran did not include the edit. Catches a target that compiled but was not the
# one executed.
newest_source=$(find src tests -name '*.rs' -printf '%T@\n' 2>/dev/null | sort -n | tail -1 | cut -d. -f1)
binary=$(artifact "$@")
binary_mtime=$( [ -n "$binary" ] && stat -c '%Y' "$binary" 2>/dev/null || echo "" )
if [ "${MUTATE_ALLOW_CACHED:-0}" != "1" ] && [ -n "$binary_mtime" ] && [ -n "$newest_source" ] \
    && [ "$binary_mtime" -lt "$newest_source" ]; then
    echo "MEASUREMENT ERROR: the test binary predates the newest source file." >&2
    echo "The mutant is not in the binary that ran." >&2
    exit 4
fi

if [ "$status" -eq 0 ]; then
    echo "SURVIVED: the tests passed against the mutant." >&2
    echo "That is a coverage finding, not a measurement error." >&2
    exit 1
fi

echo "CAUGHT: the mutant compiled and the tests failed."
grep -E '^test result:' "$log" | tail -1
exit 0
