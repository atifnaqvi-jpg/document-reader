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
# Usage:
#   scripts/mutate.sh <cargo-test-args...>
#
# Run it after applying a mutant to a file the guard reads. It exits:
#   0  the mutant compiled and the tests failed  -> caught, as intended
#   1  the tests passed                          -> SURVIVED: coverage finding
#   2  the mutant did not compile               -> MEASUREMENT ERROR: not a catch
#   3  no summary line although the build was clean -> MEASUREMENT ERROR
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

if [ "$status" -eq 0 ]; then
    echo "SURVIVED: the tests passed against the mutant." >&2
    echo "That is a coverage finding, not a measurement error." >&2
    exit 1
fi

echo "CAUGHT: the mutant compiled and the tests failed."
grep -E '^test result:' "$log" | tail -1
exit 0
