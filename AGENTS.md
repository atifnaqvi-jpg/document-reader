# Agent instructions — Document Reader

## Standing rule: mutation-verify every guard that reads config text

A guard test that parses a config file (`ci.yml`, `release-windows.yml`, `tauri.conf.json`,
a lockfile, a version carrier) is not verified by seeing it pass. It is verified by
**restoring the defect on purpose, re-running it, and confirming it fails** — then putting
the file back and confirming it passes again. Report the mutation and the observed failure
in the commit message.

This rule exists because this codebase has produced a vacuous guard twice, and both passed
on the first look:

1. `tests/ci_workflow.rs` — the "no branch filter" check compared
   `"branches: [main, master]"` against `"branches"` without splitting on `:`. The defect
   was restored and the test still passed. It was only caught by mutating the workflow.
2. An SDK test using `MagicMock`, which answers *any* attribute access, so the assertion
   held regardless of what the code under test did.

Both are the same failure: the guard tested the *shape of its own reading*, not the
property it claimed to protect. A guard that can pass vacuously is worse than no guard,
because it is recorded as coverage.

Applies to any new or modified guard that reads a file rather than calling code. Where a
guard has a parser helper, add a second test that feeds the helper a crafted sample —
including the *defect* shape — so a broken parser fails on its own test rather than
silently returning "nothing found".

## PDFium in tests

Any test calling `PdfProvider::*` must guard on the native library:

- `tests/textedit_gates.rs` → `if !pdfium_ready() { return; }`
- `tests/memory.rs` → `if support::skip_without_pdfium() { return; }`

Both log `skipping: PDFium not available on this machine`. Ubuntu CI has no PDFium, so an
unguarded test fails there while passing on a developer machine that happens to have it.
Skipping does not cost coverage: `windows-check` in `ci.yml` fails if any such line appears
in a run that fetched PDFium. Do **not** use `#[ignore]` for this — the assertion only
recognises the skip message.

## Gates

`ci.yml` runs on **every branch**; do not reintroduce a `branches:` filter. The toolchain is
pinned to 1.91.0 in `ci.yml`, `release-windows.yml` and `rust-toolchain.toml`; all three must
agree, or a local `cargo fmt --check` stops meaning anything about CI.

## Version and phase numbering

One minor bump per phase (ruling Q7, `docs/P2.7-TEXT-EDITING-PARITY-PLAN.md`). CID is
**P2.7.5 / 0.8.0**. **P2.8 is line-level reflow at 1.0.0** and must not be used for anything
else. Six version carriers must agree (`tests/release_version.rs` enforces it), and
`docs/RELEASE-NOTES.md` must gain a section before the bump lands.

Release tags publish as **pre-releases** until 1.0.0. The `prerelease: true` in
`release-windows.yml` is guarded; flip it and the guard together when P2.8 ships.
