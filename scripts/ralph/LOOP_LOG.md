# Ralph loop log

Append-only. One section per iteration. The loop reads the tail before starting, so keep the
most recent iteration last.

---

## Iteration 0 — setup — green

- Task: harness + ruled-answer doc updates
- Tests: none (documentation and scaffolding only)
- Files: `scripts/ralph/{README,IMPLEMENTATION_PLAN,PROMPT,loop.sh}`, `docs/P2.7-*`
- Gate: not run; no production code touched
- Commit: none yet
- Learned:
  - Repo `8eed510…` is a **Tauri/Rust + React PDF editor**, branch `p2-windows-test`.
  - Build and test need `PDFIUM_LIBRARY_PATH=$PWD/resources/libpdfium.so` from `src-tauri/`.
  - `cargo test` must be run **without** `--features testfixtures` (release command).
  - P2.7 docs were untracked at the start of the programme; they are part of the first commit.
  - Baseline before the loop: 393 Rust tests and 62 vitest tests passing, clippy clean.
  - **Known defect:** rotated text is editable and mis-measured (`advance` is horizontal for a
    vertical run). This is H0 and must be fixed before any new phase.
  - **Known defect:** `decode_text_string` uses Latin-1 for non-UTF-16 strings while claiming
    PDFDocEncoding. This is T-2 and can write a different character than it read.

## Iteration 1 — H0.1/H0.2/H0.4 — green

- Task: refuse rotated/skewed runs instead of measuring them on the wrong axis
- Test: `refuses_a_rotated_run_with_gate_h` (red before: **yes** — verified by deleting the
  gate and watching `assert!(!verdict.editable)` fail), plus
  `a_rotated_text_matrix_is_flagged_on_the_run`, `an_axis_aligned_matrix_is_not_flagged_as_rotated`,
  `a_scaled_axis_aligned_matrix_is_not_flagged_as_rotated`,
  `an_axis_aligned_run_stays_editable`, `a_rotated_run_refuses_as_rotated_not_as_unpaired`
- Files: `src/editor/textlocate.rs`, `src/editor/textedit.rs`, `src/editor/testpdf.rs`
- Gate: cargo test 327 + 72 integration passed, clippy clean (0 warnings), tsc clean,
  vitest 62 passed
- Commit: see `fix(p2.7-hotfix)`
- Learned:
  - `cargo fmt --check` **fails at HEAD** on files this programme never touches. It is not in
    the gate; do not reformat the tree inside a task.
  - `RawRun` construction sites: `textlocate::locate_runs` (real) and `textedit::tests::test_run`
    (helper). Adding a field means touching both.
  - `describe_runs` overwrites the admissibility verdict with `run-mismatch` when no PDFium box
    pairs. A geometry refusal must be decided *before* that, or the user sees the wrong reason.
    Test `a_rotated_run_refuses_as_rotated_not_as_unpaired` pins that ordering.
  - The rotated-text probe from the stock take is now a committed fixture pair
    (`rotated_text` / `unrotated_text`) rather than an example binary — the 0.4.4 lesson.

## Iteration 2 — T-1 (`Ts`) — green

- Task: track text rise and treat it as an ink-baseline offset, never a pen move
- Test: `does_not_merge_across_a_rise_change` (red before: **yes** — reverting `mergeable` to
  compare `origin.1` makes it fail), `text_rise_is_recorded_without_moving_the_pen`,
  `text_rise_is_reset_by_bt`, `merges_two_runs_raised_by_the_same_ts`
- Files: `src/editor/textlocate.rs`, `src/editor/textedit.rs`, `src/editor/testpdf.rs`
- Gate: cargo test 331 + 72 integration passed, clippy 0 warnings
- Learned:
  - `Ts` belongs in the walker's text state and is reset by `BT` like `Tc`/`Tw`/`Tz`.
    It must **not** enter `origin()`: the pen does not move, only the ink does.
  - Three baseline comparisons existed (`pair_run` containment + delta + overshoot,
    `mergeable`, `validate_run_range`, `smallest_containing`); all now use
    `RawRun::baseline() = origin.1 + rise`. Missing any one of them re-introduces the bug
    for a different code path, which is why each is covered.
  - **New finding, not fixed here:** `TextMatrixWalk` does not advance the pen after a plain
    `Tj`/`'`/`"` — only `TJ` advances internally. Consecutive `Tj` operands therefore report
    the same origin x. Pre-existing, independent of `Ts`. Needs its own task; recorded in
    `PENDING` below rather than silently fixed inside this one.

### Iteration 2b — T-1 acceptance test

The plan's stated acceptance (a `Ts 5` run pairs to a box offset by 5pt) was **not** covered by
the first iteration's tests: `does_not_merge_across_a_rise_change` covered merging, not pairing.
Added `a_raised_run_pairs_to_the_box_offset_by_its_rise`. The first version of it passed
against the *old* code too (both boxes contained the pen y), which is the exact failure mode
the prompt's "confirm it fails for the right reason" step exists to catch — the fixture was
rebuilt so the two readings select different boxes, and it now fails with `left: 0, right: 1`
when `ink_y` is reverted to `run.origin.1`.
