# Agent instructions — Document Reader

## First action: `cd` to the project directory before running any `git`

An agent session starts in `/workspace/project/<conversation-id>`. That
directory may be **empty**, and its **parent** `/workspace/project` is itself a git
repository on branch `master` with **zero commits**. So a `git commit` run from the starting
directory walks up, finds that parent repository, and silently creates its *first* commit — on a
repository unrelated to this project, with no HEAD to compare against and nothing to show the
mistake. It does not fail; it succeeds somewhere else. Always change into
`/workspace/project/<project-id>` explicitly, and confirm with
`git rev-parse --show-toplevel` that the top level is the project directory, before any status,
diff, add or commit.

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
  (`-lt 464`) only catches values below it, so lowering a floor is caught and *raising* it is
  not; if the property is "no test was lost", the floor must equal the measured count, not sit
  under it.
- **A value that is still satisfied by the defect.** Mutating a required string to one that
  is also present elsewhere in the file passes, because `contains` found the other occurrence.
  Count occurrences before choosing the mutation, and prefer a parsed field over a substring.

### A mutation to a value the file already holds is not a mutation

Measured during P2.7.6 step 1, on the font-hash guard, and it produced a green run that looked
like a passing mutation test:

- The mutation wrote `\x00` at offset 20,000 of `LiberationSans-Regular.ttf`. That byte was
  **already `0x00`**. The file was unchanged, the hash matched, and the guard passed — so the
  run proved nothing about whether the guard can detect a swapped face.
- Rewriting the same offset with `\xff\xff` changed two bytes (`cmp -l` confirmed offsets
  20001/20002), the SHA-256 changed, and the guard failed at the hash assertion. That is the
  only one of the two runs that was a mutation.

`printf '\x00' | dd` is the trap: it silently no-ops against an existing zero byte and `dd`
still reports success. Before trusting any surviving mutant, verify the mutation landed:

```
cmp -l <original> <mutated> | head      # must be non-empty
git diff --stat                          # must name the guarded file
```

Restore and confirm green afterwards; a mutation result is a pair.

### A mutant that does not compile is not a mutant that was caught

Measured during P2.7.6 R4, on the plan-order guard, and it produced what looked like a pass:

- The mutation injected `crate::editor::fontbundle::face_for(...)` into `substitute.rs`. That
  module does not exist. The build failed with `E0433`, `cargo test | grep "^test result"`
  printed **nothing**, and the empty output was read as "no failure". It was not a pass: no
  test ran at all.
- The second attempt added `Plan::Embed` but left the two `match` consumers non-exhaustive, so
  the crate still failed to build (`E0004`), with the same silent-looking grep.
- Only the third attempt — variant plus both match arms — compiled, and then the guard failed
  with the intended message.

A compiling mutant that is still not caught is a coverage finding; a non-compiling mutant is
a measurement error, and the two look identical if you only grep for `test result`. **The check
is mechanical, not a discipline note** — use `scripts/mutate.sh`, which refuses to conflate
them:

```
# apply the mutant to the file the guard reads, then:
scripts/mutate.sh --test embed_plan_order
#   exit 0  CAUGHT        compiled, tests failed     -- what you want
#   exit 1  SURVIVED      tests passed               -- coverage finding
#   exit 2  MEASUREMENT   did not compile            -- not a catch, no test ran
#   exit 3  MEASUREMENT   no `test result:` line     -- not a catch, no test ran
#   exit 4  MEASUREMENT   crate did not compile      -- stale binary, mutant not in it
#   exit 5  TIMEOUT       compiled, ran, never terminated -- a verdict, not a measurement error
```

**Exit 5 is a verdict, not a measurement error.** It records something the mutant did: it
compiled, the suite began, and the run exceeded the declared budget. A guard that removes a
termination check is caught by hanging, and no summary line will ever say so — so the absence
of a summary line is the finding, not a lost measurement.

It is distinct from `MEASUREMENT_ERROR:slow_not_hung` (exit 3), and the two are **not**
interchangeable — a hang and a slow test look identical from outside:

- The first run exceeds `MUTATE_TIMEOUT`. That budget is the caller's **opt-in**; unset means
  no internal budget and the old behaviour, so a timeout verdict cannot arrive by accident.
- The harness then **re-runs at a larger budget** (`MUTATE_TIMEOUT_CONFIRM`, default three
  times the first). A slow test terminates there and reports a summary line → **exit 3,
  `slow_not_hung`**, and nothing was proved about non-termination. Only if the confirmation run
  also produces no summary does the verdict stand as **exit 5, TIMEOUT**.
- One budget can only guess. Two budgets let a slow suite terminate and report while a genuine
  hang stays terminated. Treating a `hang_witness` helper as the sole evidence instead was
  tried and withdrawn: it cannot distinguish the two cases either.

**A timeout log cannot name the hung test, and must never be read as if it does.** libtest
writes `test NAME ... ` with no newline and flushes at the verdict, so a test that never returns
leaves nothing readable in a redirected log. `last_test_completed` is the last test that
*returned* — the hang is in the next one, or in the binary before it printed. Pair the record
with `MUTATE_TARGET` to say which guard was removed; do not read it as test identity. The
per-run record in `.mutation-log/` states this limitation itself, so a verdict that cannot
distinguish stale from fresh, or which test hung, says so every time.

The harness was itself mutation-tested: a non-compiling mutant exits 2, a clean tree exits 1,
a real catch exits 0. Its first version matched a bare `^error:` and therefore classified a
genuine catch as a measurement error, because `error: test failed, to rerun pass ...` is the
normal footer of a *caught* mutant. Only compilation failures (`^error[`, `could not compile`)
count. If you change the harness, break it in both directions before trusting it.

Exit 4 was added after a run finished in `0.01s`: cargo had decided nothing changed, so the
tests executed a binary that never contained the mutant and the suite came back green. The
check is `Compiling document-reader` must appear in the log — that is cargo's own report of
what it did, and it is the only reliable signal. An mtime comparison was tried first and is
**not** sufficient: if the mutated source is older than the binary (a preserved timestamp,
clock skew, a file written before the last build) the binary looks current while the mutant
is absent. Reproduced by backdating a mutant, which exits 4 rather than a false CAUGHT.

A clean tree therefore cannot be tested through the harness without `MUTATE_ALLOW_CACHED=1`,
which skips the rebuild requirement. That variable exists for self-testing only; a mutation
run that needs it is a mutation run that proved nothing.

Applies to any new or modified guard that reads a file rather than calling code. Where a
guard has a parser helper, add a second test that feeds the helper a crafted sample —
including the *defect* shape — so a broken parser fails on its own test rather than
silently returning "nothing found".

### Absence of evidence read as evidence

Three observed shapes in this programme, all of which look identical from the outside: green.

- **A non-compiling mutant.** `cargo test` printed no `test result:` line because no test
  ran, and the empty output was read as "no failure" (P2.7.6 R4, `E0433` then `E0004`).
- **A crate that never recompiled.** The summary line was real, but it came from a stale
  build: the mutant was never in the binary. One run finished in `0.01s` — a duration that
  is itself the tell, because nothing can link that fast. `scripts/mutate.sh` now refuses
  this shape by requiring a rebuild (exit 4).
- **A fixture that cannot distinguish correct from broken.** The guard is correct and the
  mutant is real, but both the correct and the broken path return the same answer, so the
  test passes either way. The Type0 guard test did this: an in-place CMap patch silently
  failed, leaving a font whose one-byte and two-byte decoders both produced the same text,
  and the mutant survived a test that looked like coverage.

The first two are **measurement errors** — the tooling can catch them, and now does. The
third is a **coverage hole** — no harness can catch it, because from the tool's point of
view a passing test is a passing test.

The defence is to make the two paths *disagree visibly*. `testpdf::cid_trap_cmap_page()`
maps `<0000>` to `A` alongside `<0002>` to `B`, so a byte-split reader turns the code `0002`
into `AA` where the correct reader produces `B`. Before writing a guard test, ask what the
broken implementation would return; if it would return the same thing, the test is a
rehearsal. Assert the *precondition* too (`has_to_unicode(...)`), so a fixture that fails to
parse fails loudly rather than passing by accident.

Two classes of guard, two kinds of evidence:
- compiled-in: the Compiling line is required; a mutant that was not
  built cannot have run.
- runtime-read: Compiling is neither required nor evidence. Declare the
  path with MUTATE_RUNTIME_READ and let the harness check mtime, not
  the build log. A verdict that cannot distinguish stale from fresh
  must say so in the log, every time.
A verification tool that cannot report its own blind spots is itself
unverified.

57 guards are in the runtime-read class (48 of them reading repo config, docs or source
rather than fixtures). Before the class existed, each was either an exit-4 false alarm on a
correct run or a silently accepted verdict.

## PDFium in tests

Any test calling `PdfProvider::*` must carry `#[ignore]` and be run with the library present:

```rust
#[ignore = "requires a real PDFium library; `cargo test -- --include-ignored` after fetching it"]
#[test]
fn ...
```

Ubuntu CI has no PDFium, so the test must stay out of the default run there. `#[ignore]` does
that while keeping the omission *counted*: `ci.yml` asserts exactly 76 ignored on Linux, and
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

## Out-of-scope projects

**No POS work.** A point-of-sale project is not in scope and never was. See
`the workspace-level agent instructions` ("Out-of-scope projects") for the binding rule. If you find
yourself designing POS architecture, ADRs, schemas, or asking deployment/country/hardware
questions about a POS system: stop, delete what you wrote, and report the hallucination.
