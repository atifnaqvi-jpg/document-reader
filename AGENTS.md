# Agent instructions — Document Reader

## Standing rule: `--include-ignored` must pass before a phase is marked `[x]`

A phase is not complete until `cargo test -- --include-ignored` passes with PDFium available.
Run it, and record the pass/fail counts in the commit message. This is a gate, not a
reminder.

The rule exists because the alternative was measured: P2.7.4 introduced a window bug that
dropped the last glyph of every line (333pt measured against a true 342pt span). All 428
non-PDFium tests passed. Only the `#[ignore]`d, PDFium-gated tests caught it. A plain
`cargo test` therefore reports green while the document pass is broken on every line.

The 74 `#[ignore]`d tests are the only coverage that opens a real PDF and exercises the
rendering path. Treating them as optional means regressions on every real document ship
unvalidated. Both CI workflows already assert this; local verification must match them.

Note the asymmetry, and do not "fix" it by adding the flag to the Linux job: Linux has no
PDFium, so `--include-ignored` there produces failures, not coverage. The Linux job instead
asserts the exact ignored count, and the Windows job is where the real run happens.

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

### The mutation must be in the guarded file, not in the guard

Measured during the A7 skew fix, twice in the same run, and both times it first looked like
proof:

- The ignored-count guard reads `ci.yml`. Mutating the **guard's own assertion string**
  (`"ne 74"` → `"ne 71"` in `tests/ci_workflow.rs`) made the guard *pass* against an unchanged
  `ci.yml`. Nothing was proved, because nothing the guard reads had changed.
- Mutating `ci.yml` itself (`-ne 74` → `-ne 71`) failed the guard, correctly. That is the
  only one of the two that was a mutation at all.

The check is mechanical: **`git diff` must show the change in the file the guard reads.** If
the only diff is under `tests/`, you have edited the guard and learned nothing. Restore the
guarded file and confirm the guard passes again — a mutation result is a pair, not a single
red run.

Two further shapes that can pass for the wrong reason, so test them deliberately:

- **A pin that is an equality is direction-free.** `-ne 74` catches both 71 and 75. A floor
  (`-lt 435`) only catches values below it, so lowering a floor is caught and *raising* it is
  not; if the property is "no test was lost", the floor must equal the measured count, not sit
  under it.
- **A value that is still satisfied by the defect.** Mutating a required string to one that
  is also present elsewhere in the file passes, because `contains` found the other occurrence.
  Count occurrences before choosing the mutation, and prefer a parsed field over a substring.

Applies to any new or modified guard that reads a file rather than calling code. Where a
guard has a parser helper, add a second test that feeds the helper a crafted sample —
including the *defect* shape — so a broken parser fails on its own test rather than
silently returning "nothing found".

## PDFium in tests

Any test calling `PdfProvider::*` must carry `#[ignore]` and be run with the library present:

```rust
#[ignore = "requires a real PDFium library; `cargo test -- --include-ignored` after fetching it"]
#[test]
fn ...
```

Ubuntu CI has no PDFium, so the test must stay out of the default run there. `#[ignore]` does
that while keeping the omission *counted*: `ci.yml` asserts exactly 74 ignored on Linux, and
the Windows job asserts 0 ignored after `--include-ignored`.

**Adding an ignored test means updating that pin in the same commit.** The count is literal on
purpose: a moving baseline would let a `#[ignore]` disappear unnoticed. The cost is that it
breaks on the next added test — it did at `2ddbb4f`, where the pin still said 70 and the next
push would have failed. Change the number, and say in the commit message that the new count
was measured rather than incremented.

An early `return` from a skip arm (`if !pdfium_ready() { return; }`) is forbidden. libtest
captures the stdout of a passing test, so the skip message never reaches the log and the suite
reports `0 ignored` while having run nothing. `tests/ci_workflow.rs` fails on that shape.

## Gates

`ci.yml` runs on **every branch**; do not reintroduce a `branches:` filter. The toolchain is
pinned to 1.91.0 in `ci.yml`, `release-windows.yml` and `rust-toolchain.toml`; all three must
agree, or a local `cargo fmt --check` stops meaning anything about CI.

## Version and phase numbering

One minor bump per phase (ruling Q7, `docs/P2.7-TEXT-EDITING-PARITY-PLAN.md`). CID is
**P2.7.5 / 0.8.0** and published first; transforms are **P2.7.4 / 0.8.1**, because 0.7.0 would
sit below an already-published release. **P2.7.6 is 0.9.0** — the last minor before 1.0.0 —
and **P2.8 is line-level reflow at 1.0.0**, not to be used for anything else. A version
assigned to a phase that has not shipped is a *reservation, not a fact*: if a later phase
publishes first, the earlier one needs a new number.

Six version carriers must agree (`tests/release_version.rs` enforces it), and
`docs/RELEASE-NOTES.md` must gain a section before the bump lands.

Release tags publish as **pre-releases** until 1.0.0. The `prerelease: true` in
`release-windows.yml` is guarded; flip it and the guard together when P2.8 ships.
