#!/usr/bin/env bash
# Regenerates the P3.3 §7 W1 form fixture matrix. Run from the repository root.
#
# The fixtures are committed, so this only needs re-running when a builder in
# `src-tauri/src/editor/testpdf.rs`'s P3.3 section changes. It is a cargo example rather than a
# Python script for the reason `make-p24-fixtures.sh` gives: the fixtures must be produced by
# lopdf's public API, the same library the product writes with, or the two drift.
#
# This generator writes only the *synthesised* half of W1. The real-producer half —
# `libreoffice-checkbox-form.pdf` — is written by LibreOffice itself, and its regeneration
# command is recorded in `src-tauri/tests/fixtures/README.md`. The split is deliberate: a
# synthetic builder cannot produce the `/Yes`-value-with-`/yes`-appearance split, because that
# split is a quirk of one producer's writer rather than a shape we would choose.
set -euo pipefail

cd "$(dirname "$0")/../src-tauri"
cargo run --quiet --example make_p33_fixtures --features testfixtures
