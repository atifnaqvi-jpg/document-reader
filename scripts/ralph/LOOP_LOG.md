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
