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
