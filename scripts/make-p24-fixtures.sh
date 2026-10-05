#!/usr/bin/env bash
# Regenerates the P2.4 §6 fixture matrix. Run from the repository root.
#
# The fixtures are committed, so this only needs re-running when a builder in
# `src-tauri/src/editor/testpdf.rs` changes. It is a real cargo example rather than a
# Python script because the fixtures must be produced by lopdf's public API — the same
# library the product writes with — and reimplementing that in Python would let the two
# drift.
set -euo pipefail

cd "$(dirname "$0")/../src-tauri"
cargo run --quiet --example make_p24_fixtures --features testfixtures
