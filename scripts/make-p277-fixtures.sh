#!/usr/bin/env bash
# Regenerates the P2.7.7 §5 fixture. Run from the repository root.
#
# The fixture is committed, so this only needs re-running when
# `testpdf::type0_shared_font_multi_run` changes. It is a cargo example rather than a Python
# script for the reason `make-p24-fixtures.sh` gives: the fixture must be produced by lopdf's
# public API, the same library the product writes with, or the two drift.
set -euo pipefail

cd "$(dirname "$0")/../src-tauri"
cargo run --quiet --example make_p277_fixtures --features testfixtures
