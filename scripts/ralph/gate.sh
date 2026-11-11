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
# ci-exempt: set -o pipefail ;; covers: none ;; platform: CI installs Tauri's system libraries with a bounded retry; this machine already has them
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
# 2026-11-02 (R-6): 531/93 -> 523/93 and 624/0 -> 616/0. The drop is exactly 8 in both
# shapes and it is accounted for, not merely observed: retiring `undo.rs` removed its 10
# tests, and this row added 2 (`a_reflow_edit_reaches_the_document_and_its_inverse_restores_it`,
# `a_vector_leg_refuses_when_its_operations_changed_since`). 10 - 2 = 8. The ignored count is
# unchanged at 93, which is the check that matters: no PDFium-backed test was lost.
# 2026-11-02 (U-2): +1 in both shapes, 523/93 -> 524/93 and 616/0 -> 617/0, for
# `a_reflow_shaped_multi_operand_edit_is_one_undo`. One test, counted once in each shape; the
# ignored pin is again the check that it was not a PDFium test changing shape.
# 2026-11-02 (R-7): +1 in both shapes, 524/93 -> 525/93 and 617/0 -> 618/0, for
# `the_parity_matrix_promotes_a10_only_with_a_recorded_installer`. It reads two Markdown files
# and never opens PDFium, so the ignored pin stays 93.
# 2026-11-02 (A-3): +16 in both shapes, 537/93 -> 553/93 and 630/0 -> 646/0. The 16 is
# accounted for, and it is two groups. Ten are `annotate/appearance.rs`'s own tests: that file
# was written earlier in this row and its tests had never been billed into a pin, so the first
# time they counted they showed up as an unexplained jump. Six are new here -- five in
# `annotate.rs` for the acceptance row (`/Subtype /FreeText`, `/IT` for the typewriter only,
# `/CL` for callout only, plus the two refusals) and one in `export.rs` for the flattening
# data-loss guard. 10 + 6 = 16. The ignored count is unchanged at 93, which is the check that
# matters: no PDFium-backed test changed shape.
# 2026-11-04 (A-7/A-8): +13 in both shapes, 568/93 -> 581/93 and 661/0 -> 674/0. The 13 is
# accounted for, and it is two rows. Eight are A-8's reply/status wire format in `annotate.rs`:
# `a_status_writes_state_and_its_review_model`, `every_status_writes_its_own_state_name`,
# `an_annotation_without_a_status_writes_no_state_keys`, `a_status_this_build_does_not_know_writes_nothing`,
# `a_v1_blob_still_writes_its_geometry`, `a_reply_writes_irt_naming_its_parents_object`,
# `a_reply_written_before_its_parent_still_finds_it`, `a_reply_whose_parent_is_gone_is_detached_not_dropped`.
# Five are A-7's storage and cascade in `db/annotations.rs`:
# `deleting_a_parent_deletes_its_replies`, `deleting_a_reply_leaves_its_parent_alone`,
# `a_reply_to_a_reply_is_found_by_replies_to`, `set_status_rewrites_only_the_status_key`,
# `clearing_a_status_removes_the_key_rather_than_storing_null`. 8 + 5 = 13. The ignored count is
# unchanged at 93, which is the check that matters: no PDFium-backed test changed shape.
# 2026-11-04 (A-9/A-10): +28 in both shapes, 581/93 -> 609/93 and 674/0 -> 702/0. Two rows
# again. Twenty-five are XFDF in `editor/xfdf.rs`: export spelling per kind (squiggly keeps its
# own element, arrow keeps its head, a plain line writes none), the union rect and per-line
# quads for a multi-line highlight, ink one gesture per stroke, date formatting in UTC, the
# round trip, the repeat import's stable names, D-A10's skip-and-count in `merge`, the quad
# rebuild into `rects`, and the three failure classes (unsupported, malformed, truncated)
# counted apart rather than folded into one number. Three are the import path in
# `db/annotations.rs`: `add_many` writes every row and indexes its text,
# `add_many_refuses_an_unknown_document_and_writes_nothing`, and `a_replys_text_is_searchable`,
# which closes DoD 3's requirement that a reply's text reach FTS. 25 + 3 = 28. Ignored stays
# at 93: no PDFium-backed test changed shape.
# 2026-11-05 (A-11 free-text defect): +6 in the plain shape (611 -> 617) and +3 ignored
# (93 -> 96), and the --include-ignored shape +9 (704 -> 713). The six are the wrap and
# short-box unit tests in `appearance.rs` plus the rect/BBox guard in `annotate.rs`; the
# three are the new pixel tests in `tests/free_text_appearance.rs`, which need PDFium. The
# include-ignored shape moves by all nine because it runs both sets. Measured, not computed:
# `cargo test` reported 617/96 and `-- --include-ignored` 713/0 on this tree.
# 2026-11-07 (A-11b note-prompt chrome): +4 in the plain shape (617 -> 621) and the same +4
# in the include-ignored shape (713 -> 717). The four are `tests/app_css_chrome.rs`: the
# shared selector, the line-height override, the top inset, and the guard that the free-text
# preview keeps its metrics out of the stylesheet. None touch PDFium, so ignored stays 96.
# Measured, not computed: 621/96 and 717/0 on this tree.
# 2026-11-07 (v1.1.0 tag prep): +2 in both shapes, 621 -> 623 and 717 -> 719, for the two
# release-body guards in `tests/release_workflow.rs`. Measured: 623/96 and 719/0.
# Then +1 more each (623 -> 624, 719 -> 720) for `the_notes_link_points_at_the_tag_not_a_branch`,
# written after v1.1.0 published a notes link pointing at a branch 67 commits stale.
#
# 2026-11-10 (P3.2 SCOPE-C trust store): pins were STALE by 214/10 before this unit and had been
# since 2cbca07 (2026-11-07). 27 test-touching commits landed after that one without moving them,
# so `bash scripts/ralph/gate.sh` reported red on `2cbca07..16dd068` for arithmetic, not for a
# defect. Re-measured from the tree rather than incremented, per the rule that says so: at
# HEAD 16dd068 the plain shape was 838/106 and include-ignored 944/0. The ignored count had also
# moved 96 -> 106 without this pin following it; `ci.yml` was already correct at 106, which is
# why CI stayed green while the local gate did not. Re-measured HEAD: 838/106 and 944/0.
# The ignored count is unchanged by this unit, so the new numbers are 838/106 and 944/0 plus
# this unit's own +39:
#   +12 `tests/trust_store.rs`  (the end-to-end trust-store cases)
#   +3  `tests/badge_evidence_source.rs` (the command still calls `assess`)
#   +24 `signature/trust.rs` `#[cfg(test)]` (path walk: cycles, shortest path, expiry, pathLen)
# 39 = 12 + 3 + 24, and it moves BOTH shapes by the same amount because none of the three is
# PDFium-gated. That unit: 877/106 and 983/0.
#
# 2026-11-10 (I-SG5 / D-SG7 later-revision seam): +1 in `tests/trust_store.rs` for
# `an_anchored_clean_signature_stops_being_valid_when_a_revision_is_appended`. It moves both shapes
# by one, is not PDFium-gated, and leaves the ignored count at 106. That unit: 878/106 and 984/0.
#
# 2026-11-10 (D-SG7 revocation detail): +1 unit test in `signature/badge.rs` for
# `every_revocation_answer_gets_a_line_including_we_did_not_check`. Moves both shapes by one.
#
# 2026-11-10 (I-SG4 unsigned badge): +4, all PDFium-independent, so both shapes move by four and
# the ignored count stays at 106: two unit tests in `signature/badge.rs`
# (`an_unsigned_document_renders_unsigned_and_says_nothing_was_checked`,
# `the_unsigned_detail_agrees_with_the_aggregate_decision`), one in `commands/mod.rs`
# (`no_assessment_is_rendered_as_unsigned_not_unknown`) and one new source guard in
# `tests/badge_evidence_source.rs` (`the_command_hands_the_store_assessment_to_the_mapping_unmodified`).
# Measured, not incremented: 883 passed with PDFIUM_LIBRARY_PATH unset and 883 with it pointed at a
# nonexistent path, 106 ignored in both.
#
# 2026-11-10 (+1, CRLF guard regression): `bounded_body_finds_its_bound_in_a_crlf_checkout` in
# `tests/command_registration.rs`. That test exists because a `"\n}\n"` bound passed on Linux and
# failed on the Windows runner's CRLF checkout, so the guard was red only on the shipping target.
# +1 PDFium-independent; ignored count stays 106.
#
# 2026-11-11 (I-SG5 edit policy): +10, all PDFium-independent, so both shapes move by ten and the
# ignored count stays at 106: five unit tests in `editor/signature/editpolicy.rs` (the normative
# P=1/2/3 matrix, the uncertified arm, the /P round-trip, the refusal wording, and check()/permits()
# agreement) and five in the new `tests/sign_edit_policy.rs` (per-level permitted+forbidden edit with
# the badge read off a real certified file, signed bytes intact after an appended revision, an
# uncertified document, an approval signature, and the three levels being distinguishable).
# Measured, not incremented: 894 passed with PDFIUM_LIBRARY_PATH unset, 1000 with --include-ignored.
#
# +3 more for `tests/certification_gate.rs`, the command-layer guard for the same invariant: it scans
# `src/commands/mod.rs` to prove every governed command calls the gate, calls it with the right
# EditKind, calls it before the mutation, and propagates the refusal with `?`. That guard found a real
# omission while being written -- `remove_annotation` was ungated, and deleting an annotation is the
# same change to the signed document's annots array that P=1/P=2 refuse -- and it caught a mutation
# that deleted the `?`, turning the gate into a silent no-op.
# +1 more: `no_command_calls_the_gate_without_being_listed`, the inverse direction -- every gate call
# in the source must be covered by a GOVERNED entry, so shrinking the list fails instead of silently
# narrowing the forward guard. An earlier `!GOVERNED.is_empty()` could never fail (a compile-time
# constant) and clippy said so, which is the same "guard that cannot fail" defect this file keeps
# finding in production code.
# Measured, not incremented: 898 passed with PDFIUM_LIBRARY_PATH unset, 1004 with --include-ignored.
#
# 2026-11-11 (D-SG4 trust import): +15, all PDFium-independent, so both shapes move by fifteen and
# the ignored count stays at 106. Eight unit tests in `editor/signature/trust.rs` (PEM<->DER
# passthrough both ways, a PEM bundle refused rather than narrowed to its first certificate, CRL
# decode, a certificate refused by the CRL decoder, and empty input not mistaken for PEM) and seven
# in `tests/trust_store.rs` (the import command reaching `valid` on a real file, what it reports as
# stored vs new, a PEM anchor taking the same path as DER, an unreadable import refused with a code
# the UI can read, a CRL reporting the `nextUpdate` that decides when it stops being evidence, a CRL
# naming the signer suppressing `valid`, and the size cap refusing with the size named).
# Measured, not incremented: 913 passed with PDFIUM_LIBRARY_PATH unset, 1019 with --include-ignored.
#
# 2026-11-11 (P3.2 SCOPE-B command layer): +22, all PDFium-independent, so both shapes move by
# twenty-two and the ignored count stays at 106. Twelve unit tests in `db/redactions.rs` (the
# round trip, a mark that survives reopen, ordering by page, the NaN and non-finite refusals, the
# unparseable stored rect refused rather than defaulted, `add_many` writing one batch including the
# pattern it was given, clearing by document, and removing one mark by id) and ten in
# `tests/redact_command.rs` (marking touches no file, apply writes a new file and the text is gone,
# marks cleared after apply, a repeat apply reports nothing to apply, and the certification refusal
# on both the mark and the apply path).
# SCOPE-C unit 5 adds four in `tests/trust_store.rs` (an import is listed through the command, an
# empty store lists nothing, forgetting an anchor takes a `valid` badge back to `unknown`, and a
# repeated forget reports not-removed rather than failing). All four are PDFium-independent.
#
# SCOPE-C unit 6 adds four more there for the CRL store (an import is listed with its issuer and
# the `nextUpdate` that decides staleness, an empty store lists nothing, forgetting a revocation
# takes the badge from demoted back to `valid`, and a repeated forget reports not-removed). Also
# PDFium-independent.
# The apt-stall guard adds one in `tests/ci_workflow.rs`.
# Measured, not incremented: 944 passed with PDFIUM_LIBRARY_PATH unset, 1050 with --include-ignored.
# Now 944/106 and 1050/0.
PIN_PLAIN_PASSED=947
PIN_PLAIN_IGNORED=106
PIN_IGNORED_PASSED=1053
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
