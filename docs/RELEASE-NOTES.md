# Release notes

One section per shipped version, newest first. A version section is written **before** the
version bump lands, because the bump is what puts those bytes in an installer. The test
`src-tauri/tests/release_version.rs` enforces both halves: every version carrier agrees, and
the version in `Cargo.toml` has a section here.

---

## 1.1.0

**P3.1 — annotations breadth. Released.** Tagged `v1.1.0` after the hand-test passed: the user
ran the build-only dispatch artifact and reported the note box fixed. Provenance, including the
dispatch the human actually ran and the fact that the tag is one docs-only commit past it, is in
"Hand-test provenance" below.

This is the first release a user can feel as an annotation product. Before it, the palette was
five markup types and a note; the comment surface did not exist.

### What it ships

**Thirteen annotation types, in three families** (D-A1). Markup: highlight, underline,
strikeout, squiggly. Text: note, textbox, callout, typewriter. Drawing: ink, line, arrow, rect,
oval. All thirteen are **real PDF annotations** written by the lopdf path (P2-D3) — the same
bytes a third-party viewer reads — with the database still the source of truth and the file a
projection of it (D-A2). `polygon` and `cloud` are **named and not scheduled**: they need the
same lopdf geometry path as line and arrow, and committing to them here would have meant a
vertex-handle editor that nothing else in the palette requires.

Two details worth stating because they are the difference between "looks right in our window"
and "correct in someone else's":

- **Appearance streams are written where there is nothing to synthesise** (D-A8): ink, free
  text, callout and line/arrow carry an `/AP`. Markup does not — it keeps relying on
  `/QuadPoints`, which viewers already synthesise, and writing `/AP` for it would add a second
  description of the same geometry that can disagree with the first.
- **Line, arrow and oval are lopdf-only** (D-A3). PDFium cannot author them, so it is used to
  *measure and validate* geometry in every case and to write bytes in none.

**A comment pane with the report's four statuses** (D-A7, D-A11). One table over every
annotation in the document — replies nested under their parent rather than scattered through
the sort order — with sort by page, author, date, type or status; filter by kind, by status
(including "unmarked"), and by text; and status editing inline. It is a **stateless view**: it
holds sort and filter and never a cached copy of the rows, which is what stops the pane and the
page disagreeing. The four statuses are accepted, rejected, completed and cancelled, plus
"none/marked". They live in the blob and the `/State` key is written when one is set, with
`/StateModel /Review`, so a triage decision made here is visible in a viewer that has never
heard of this application. A status change is **one undo entry** (D-A6), like any other
annotation update: making it silent would let a user lose a triage decision with no way back.

**Reply chains, in the file format's own mechanism** (D-A5). A reply is an annotation with a
`parent`, not a row in a second table, and on write it carries **`/IRT`** — the standard
"in reply to" reference — pointing at the parent's *object*. Replies are drawn in the pane, not
as icons on the page. Making replies annotations is why they inherit search indexing, undo and
deletion cascade rather than each needing its own copy of that machinery.

**XFDF import and export** (D-A9, D-A10). Export covers the annotation list, page, geometry,
colour, contents, author, dates and status. Import **never overwrites**: an incoming annotation
whose `/NM` matches an existing row is skipped **and counted**, and the count is reported.
Three skip classes are counted apart — already-present, unsupported, malformed — because one
number would hide which of them happened. Import also **decides first and writes second**: the
whole file is evaluated, then one transaction inserts all of it, so a file that fails on row
forty cannot leave thirty-nine rows behind with a report describing a document the user does not
have. Export writes through the **real save dialog** and the path the user chose; a cancelled
save writes nothing and claims nothing.

**Search now resolves an annotation, not just its page** (D-A12). A query matching a note,
textbox, callout, typewriter or reply body returns a hit carrying the annotation's page *and*
its id; clicking it selects the annotation and jumps to it. This was the row that looked like a
formality — the DB layer was already kind-agnostic and every kind already delivered text — and
was not: the snippet was reading the wrong column, so **every annotation hit was displaying its
page number as its snippet**, and the pane was dropping the id, so a hit inside a note jumped to
the right page and selected nothing. The page being right is what made both invisible.

**The migration machinery, from 1.0.1, still runs on upgrade.** 1.1.0 inherits the 1.0.1 rename
(`com.documentreader.app` → `com.docupro.app`): `editor/migrate.rs` copies the database into the
new directory at startup, before the database is opened, as a copy and never a move. A 1.0.x
install upgrading to this build keeps its library, and the About/notices surface says where it
came from. That is a one-time notice, not a 1.1.0 feature — it is repeated here because the
first upgrade *across* 1.0.1 is the one where a user finds out whether it worked.

**Blob version 2, forward-safe in both directions** (D-A13). A v1 blob is read by this build;
a v2 blob is **skipped** by an older reader rather than misread. That is the property that lets
a 1.0.x install and a 1.1.0 install share a database during the transition without corrupting
each other's rows.

### Two defects found after these notes were first written, and fixed before the tag

Both were reported by the user hand-testing a build of this version. Neither is in the feature
list above, and both would have shipped: the notes were written, the installer was built, and
only then did a human open the product and find them. They are recorded here rather than being
folded into "what it ships", because the sequence is the point — the notes claimed a version
that a person had not yet used.

**A-11 — the free-text note box hid its own text** (`d05b7db`, `e49dc8b`). Typed text in a note
did not appear where it was typed, and at small zoom a box could look like it fitted text the
saved file would overflow. Four independent causes, each measured against real Chromium and a
PDFium raster of the saved file rather than reasoned about:

1. The preview drew at a fixed `12px` while the writer draws `12pt × zoom`, so preview and file
   agreed only at 100%. The mismatch was measured across zoom 0.5–3 as 6/12/24/36px in the file
   against a constant 12px on screen. Preview metrics now come from the writer's own constants.
2. Typed newlines collapsed: the wrapper split on whitespace, so three typed lines were written
   as two re-flowed ones. `/Contents` kept the newlines and the page did not.
3. A box shorter than one line drew **nothing** — a legal PDF with an empty appearance stream,
   which no stream-level assertion can catch.
4. Free text had no `/Rect`-versus-`/BBox` guard, though ink already had one.

The guards are the part worth knowing about: three of them had passed throughout, because
reading the appearance stream cannot tell you a box renders its text off the page. The one that
catches it rasterises the saved file through PDFium and counts ink
(`tests/free_text_appearance.rs`), and borderless typewriter mode is used for ink-only claims
because a bordered box's own frame satisfies "something was drawn" from the wrong cause. Rendered
before/after: `docs/evidence/free-text-before-after.png`.

**A-11b — the note-prompt textarea sat flush against its top border** (`be00673`). Found on a
build that already contained A-11: typed text overlapped the top border and the first line was
half-clipped. The saved file was correct — A-11's pixel tests proved that — so this was editor
chrome, a different surface from the free-text mark, sharing no rule with it.

The cause, measured on the live element: `.page-stage` sets `line-height: 0` so it can shrink-wrap
the page raster, and the textareas said `font: inherit`. That is a *shorthand*, so it re-applied
the inherited zero along with the family and size — computed `lineHeight` was `"0px"` against a
14px font. Every line box was zero pixels tall, lines drew at the same y overprinting each other,
and the first hung above the content box where the border clipped it. A named `line-height` is
what overrides a shorthand; `padding` supplied the inset the control had never had. Guarded by
four tests in `tests/app_css_chrome.rs`, each mutation-verified. Rendered before/after:
`docs/evidence/note-prompt-before-after.png`.

Both fixes are in the installer this release publishes. Version stays `1.1.0`: these are defects
in a version no user had received, fixed inside its own hand-test cycle, not a new feature set.

### Gates

Facts about the tree this section describes. Numbers were measured at `72c7ed2`, re-measured
after the version bump at `5b6b026`, and re-measured again after the A-11 and A-11b fixes at
`be00673` — a bump or a test that moved a suite would be a different tree than the one
described. The numbers below are the tagged tree's; the hand-tested bytes come from `be00673`,
which carries two fewer tests (621/96 and 717) because the two release-body guards were written
after that build.

| Shape | Result |
|---|---|
| `cargo test` (Linux, PDFium present) | **623 passed, 96 ignored** |
| `cargo test -- --include-ignored` (Linux, PDFium present) | **719 passed, 0 ignored** |
| Windows CI, `-- --include-ignored` | **718 passed, 0 ignored** |
| Frontend | vitest **117**, `tsc` and `vite build` clean |

The Windows figure is one lower **by construction, not by a skip**: one test is gated
`#[cfg(unix)]` because it drives poppler's CLI tools, which are a Unix install here, and Windows
runs the PDFium equivalent instead. Both shapes are pinned in `scripts/ralph/gate.sh`, including
the unchanged ignored count — a suite that started skipping would move that pin and fail.

These counts moved three times after the notes were first written: A-11 added six non-PDFium
tests and three PDFium pixel tests (`611/93 → 617/96`, include-ignored `704 → 713`), A-11b added
four stylesheet guards (`→ 621/96`, `→ 717`), and preparing the tag added two release-body
guards (`→ 623/96`, `→ 719`). The frontend count moved `113 → 117` when A-11 added
`FreeTextPreview.test.tsx`.

The Windows figure is one lower **by construction, not by a skip**: one test is gated
`#[cfg(unix)]` because it drives poppler's CLI tools, which are a Unix install here, and Windows
runs the PDFium equivalent instead. Both shapes are pinned in `scripts/ralph/gate.sh`, including
the unchanged ignored count — a suite that started skipping would move that pin and fail.

### Carried, not fixed

Recorded because a release note that lists only wins is a sales document, and each of these has
bitten this programme already.

- **`STATUS_ACCESS_VIOLATION` in the Windows PDFium suite.** Intermittent, and **undiagnosed**.
  Same-SHA re-runs pass; a documentation-only commit once tripped it. It is a watch item, not a
  known defect with a workaround, and nothing in this branch is known to cause it. The Windows
  job is therefore occasionally red for a reason that will not reproduce — do not read a single
  Windows failure as evidence about this release without a re-run.
- **One gate run reported red with no failing step in its log, and it remains unexplained.**
  Six subsequent full gate runs and five direct suite runs were green. My first read of it was
  wrong in a way worth keeping: `... | tail -3; echo $?` reports *tail's* exit status, so the
  `0` said nothing about the gate. Recorded as unexplained rather than called a flake.
- **The release body was hardcoded, so v1.0.1 shipped a false headline.** Its Releases page
  says *"Document Reader 1.0.0 — line-level reflow; text editing is complete"* over a tag three
  phases past that. The asset, the tag and `RELEASE-NOTES.md` were all correct; only the one
  sentence a visitor reads first was not, because nothing read it. Fixed here and guarded by
  `the_release_body_names_the_version_being_published` and
  `the_release_body_does_not_advertise_a_previous_phases_subject`. **v1.0.1 itself is not
  republished** — its tag and asset are what they were, and rewriting a published release's
  body would edit the record of what shipped rather than record it.
- **The rebuild delta.** An installer rebuilt from the identical commit is not byte-identical:
  1.0.1's published asset differs from its hand-tested artifact by 221 bytes, 1.0.0's by 3,748.
  The provenance claim is about the **source commit**, which is exact; it cannot make the binary
  reproducible. Expect the same shape between this release's dispatch artifact and any later
  tag-built one, and compare commits rather than checksums.

### Not in this release

It is annotation breadth, not parity. Digital signatures, forms data (FDF proper), redaction,
and OCR are P3.4–P3.6 and untouched here. `polygon` and `cloud` are named in D-A1 and not
scheduled. Nothing in this release changes the fact that the application is Windows-only.

- The minimum supported version, from `rust-version` in `src-tauri/Cargo.toml`, is **rust 1.90**.
- Verified on **rust 1.91.0**, the version pinned in `rust-toolchain.toml`, `.github/workflows/ci.yml`
  and `release-windows.yml`.

### Hand-test provenance

**Status: hand-tested and accepted. The tag follows the confirmation, not the build.**

Built by a **build-only** `workflow_dispatch` run on `p2-windows-test`; a dispatch keeps its
artifact and publishes no release, which is why the tag could wait for the hand-test.

| Field | Value |
|---|---|
| Dispatch run | `42331384386` at `be00673`, build-only (`workflow_dispatch`), 2026-11-07 |
| Artifact | `document-reader-windows-installer`, id `13557413255`, 10,432,931 bytes |
| File | `DocuPro_1.1.0_x64-setup.exe`, 10,448,919 bytes |
| SHA-256 | `5bbd6c00e9f08a6e6de29465b7b997a73badc9b2e29174ed8e04e65f92db4f05` |
| CI on that commit | `42331300847`, all three jobs, Windows green on the first attempt |
| Hand-test verdict | *"Note works fine — the reported note-box text defect is fixed on the new build."* |
| Tagged commit | `2ece9bc` (docs-only, one commit past `be00673`; see below) |

Three independent readings agree on the version before anyone runs it: the **filename**, the PE
**`FileVersion`** resource and the PE **`ProductVersion`** resource all say `1.1.0`, and
`ProductName` says `DocuPro`. That check exists because the carriers in the tree and the string
the installer embeds are produced in different places; 1.0.0 records the same three readings.

The dispatch reached step 10, *Assert the PDFium tests actually ran*, successfully — the same
gate that caught the Windows crash at 0.9.1 — so the Windows suite ran at this version rather
than skipping.

**The tag is one docs-only commit past the hand-tested build, and that is a deliberate departure
from the 0.9.0/1.0.1 order.** Those releases cut the tag at exactly the dispatch's commit. Here
the hand-test verdict, the corrected counts and this section were written *after* the build, and
putting a tag on `be00673` would publish a release whose notes still said "not yet released" and
carried `611/93` from a tree two defect-fixes old. So the amendment landed first and the tag sits
on it. Nothing in the delta can change behaviour: `2ece9bc` touches `docs/RELEASE-NOTES.md` and
`scripts/ralph/LOOP_LOG.md` only, and CI `42332240342` is green on it. The bytes a human ran are
still built from `be00673`, which is why that SHA is in the table above rather than the tag's.

A **second build-only dispatch** is run from the tagged commit so the equivalence is checkable
rather than asserted — there is then a dispatch record of the tag's own commit, not only of the
commit the hand-test ran. It publishes nothing, per the tag-gated attach step; its run id is
recorded in `scripts/ralph/LOOP_LOG.md` rather than here, because this section is committed
before that run exists and a number written now could only be guessed.

**Do not compare this release's asset against the hand-tested zip byte-for-byte.** An installer
rebuilt from the identical commit is not byte-identical — 1.0.1 differs from its own hand-tested
artifact by 221 bytes and 1.0.0 by 3,748. The claim this table makes is about the **source
commit**, which is exact, and it cannot make the binary reproducible.

---

## 1.0.0

**Reserved for P2.8. Not shipped.** There is no `v1.0.0` tag, no published release, and no
installer a human has run. This section is written first because the bump is what puts these
bytes in an installer, and the order is the point: notes, then bump, then a build-only
dispatch, then a human confirmation, then a tag (Q9; the sequence recorded in §0.9.0).

### What this version ships

Line-level reflow: **P2.8**, the last phase of the agreed text-editing programme.

- **A replacement wider than its run's own advance moves the rest of the line.** Nothing else
  on the page moves. The write replaces one `TJ` operation with three — `TJ`, `Td`, `TJ` — so
  the operator count changes, which is why the undo record is a vector replacement rather
  than a byte swap (see *One undo* below).
- **Positioning is line origin + measured prefix, never pen + Δ.** `Td` sets the line matrix,
  discarding the pen's accumulated advance, so a pen-delta formulation would silently drop the
  width of every run before the insertion point — correct on a one-character prefix, wrong on
  a long one. Measured on the kerned fixture: a pen-delta `Td` moved the tail 1.984 pt when
  40 pt was asked for; the line-origin form moved it 40.000 pt.
- **The measured prefix includes the `TJ` kern**, and both the prefix and the kern term are
  scaled by `Tz`, so a horizontally-scaled run is measured as it is drawn.
- **A replacement the content box has no room for is refused**, reporting both widths. The
  line's available width is measured to the content box — `/CropBox`, falling back to
  `/MediaBox` — not to the page's nominal width.
- **A narrower replacement changes no operator.** The gap widens and trailing text keeps its
  position. Acrobat behaves the same way. Closing a gap would mean moving runs *backward*,
  which is a layout change the user did not ask for, so the asymmetry is deliberate.
- **Reflow will not run into the middle of a word.** A selection that does not end at a word
  boundary is refused rather than reflowed, because the fragment after it would be the rest of
  the same word and moving it would duplicate or orphan glyphs. That case is what merging is
  for.

### One undo, whatever the operator count

A reflow's inverse is a **single** undo entry that puts the pre-edit operation vector back in
one splice. It is not one entry per operator: a per-operator undo would leave the stream
half-restored between steps.

The record is a persisted one — the same `ContentOperation` the editor stores and reads back —
with the two vectors encoded by the same encoder that writes the page, so the record and the
stream are the same bytes by construction. Because `lopdf`'s `Operation` carries no `serde`
impl, the operands travel as their encoded token list. A byte-level edit remains the
degenerate one-element case of the same record, so nothing about ordinary edits changed.

### What is still refused

Every refusal names itself and its remedy. A run refused before reflow is still refused after
it: reflow moves runs, it does not re-admit them.

| Rule | Refused when |
|---|---|
| `R1-no-move` | the replacement is not wider than what it replaces — nothing needs to move |
| `R4-wrap` | the replacement does not fit its line; reflow does not wrap onto the next line |
| `R5-straddles-levels` | the line mixes page text with text inside a form, so its runs cannot move as one line |
| `R6-non-uniform-transform` | the drawing matrix is non-uniform, so "the rest of the line" is not horizontal |
| `R8b-scope-left-of-line` | another tagged unit has a run on this baseline, left of this line's own start |
| `R10b-unattributed-kern` | the text before this run is positioned by an operator outside its `TJ` array |
| `not-one-array` | the run is not a fragment of a multi-fragment `TJ` array with something after it |

**Wrapping and paragraph reflow are out of scope** and stay out: a changed line count moves
every subsequent baseline on the page, which is paragraph-level reflow under another name.

### What 1.0.0 does not include

- **P2.7.7 — Type0/CID subset embedding.** Text in a Type0/CID face still refuses rather than
  embedding a new subset. P2.7.6 covered simple fonts; this is the remaining font gap.
- **Paragraph-level reflow and text wrapping.**
- **The full-parity inventory (B-1).** Produced after P2.8, not inside it.

1.0.0 completes **text editing** against the parity table. It is not Acrobat parity as a whole,
and the P3 programme is where the remaining domains are addressed.

### Toolchain: what the build is verified against

The minimum supported version, from `rust-version` in the manifest, is **rust 1.90**.

Verified on **rust 1.91.0**, the channel pinned in `rust-toolchain.toml`, in
`.github/workflows/ci.yml` and in `release-windows.yml`. All three must agree, or a local
`cargo fmt --check` stops meaning anything about CI.

### Measured on this tree

Counts are from the gate at the commit the bump lands on; they are restated here so a later
reader can tell a real change from a pin that moved with the code.

| Shape | Result |
|---|---|
| `cargo test` (Linux, PDFium present) | **524 passed, 93 ignored** |
| `cargo test -- --include-ignored` (Linux, PDFium present) | **617 passed, 0 ignored** |
| Windows CI, `-- --include-ignored` | **616 passed, 0 ignored** |
| Frontend | vitest **69**, `tsc` and `vite build` clean |

The Windows figure is one lower **by construction, not by a skip**: one test is gated
`#[cfg(unix)]` because it uses poppler's CLI tools, which are a Unix install here, and Windows
runs the PDFium equivalent instead. A platform gate removes the test from the binary; it is
not a runtime skip that could hide a green pass.

### Pre-release flag

`release-windows.yml` sets `prerelease: true`, which made every tag up to 0.9.0 a pre-release.
That flag is flipped here, together with `tests/release_workflow.rs`, which pins it: a missing
`prerelease` key reads as `false`, so the guard requires the key to be present and explicit at
the publish step's own depth. A 1.0.0 tag must publish as a full release, and the flag and its
guard have to move in the same commit or the tree contradicts itself.

### Hand-test provenance

**Status: awaiting a human run. This section names the exact bytes to run; it does not claim
anyone has run them.**

The installer comes from a **build-only** `workflow_dispatch` run, because the attach step is
tag-gated and a tag is the thing this gate exists to withhold. Run `41461007177` on
`p2-windows-test` at `1e6bfd8`, artifact `13149701933`:

| Field | Value |
|---|---|
| Artifact | `document-reader-windows-installer` |
| Artifact size | 10,359,389 bytes |
| File | `Document Reader_1.0.0_x64-setup.exe` |
| File size | 10,375,381 bytes |
| SHA-256 | `af18fca8274027dd7416c8d500e49a4a68554be72ea90cfedeae8ab5d9cc57b9` |
| Expires | 2027-01-31 |

Three independent readings agree on the version before anyone runs it: the **filename**, the
PE **`FileVersion`** resource, and the PE **`ProductVersion`** resource all say `1.0.0`. That
check exists because the carriers in the tree and the string the installer embeds are produced
by different steps, and a mismatch would produce a download that is not the build described
here.

**The artifact is not stale, and that is a measurement rather than an assumption.** It was
built at `1e6bfd8`; the tree has moved since. Every commit after it touches only
`docs/RELEASE-NOTES.md` and `docs/P2.7-TEXT-EDITING-PARITY-PLAN.md`, and no document is
compiled into the binary — the only `include_bytes!` in the crate is the font payload. So the
installer is the same program as a build of the current tree; it differs from it only in not
containing the notes that describe it. If a future commit changes code, this artifact stops
being the hand-test target and a fresh dispatch is required.

**What a human needs to confirm**, at minimum — the reflow path end to end, since that is the
whole of P2.8:

1. Open a PDF and edit text to something **wider**. The rest of the line moves; nothing else on
   the page does.
2. Undo **once**. The line returns exactly as it was, in one step rather than an operator at a
   time.
3. Redo. The reflowed state returns.
4. Edit to something **narrower**. Nothing moves — the gap widens (this is deliberate; see the
   asymmetry note above).
5. Edit a selection that ends **mid-word**. It refuses, and the refusal is legible.
6. Save, reopen the saved file. The line is where the edit put it.

The counts and the flag above are facts about a tree. This list is what turns that tree into a
release, and nothing in CI can satisfy it: rule 1 asks for a human on the installer.

### Confirmed, tagged, and published

- **Hand-confirmed 2026-11-02.** All six steps above were run on the artifact from dispatch
  `41461007177` and passed, including the A10 move case observed live: a wider replacement
  moved the tail, and where the box had no room it refused honestly and quoted both widths.
- **Tagged `v1.0.0` at `1e6bfd8`**, the commit that dispatch was built from — not at the later
  `e64c805`, which is documentation only. The staleness delta is recorded in the tag message
  rather than smoothed over, because "tag the confirmed tree" and "tag HEAD" are different
  claims and the difference is the whole point of the provenance rule.
- **Published as a full release.** The publish step's `prerelease` flag is `false` and
  `a_tag_publishes_a_full_release_since_1_0_0` asserts it, so the flag and its guard moved
  together with the phase rather than one asserting what the other denies.

The earlier 0.6.0–0.9.0 tags were pre-releases because each was an intermediate step and a
"latest release" badge would have claimed more than the build delivered. This one is not.

**Both installers, so a size difference is not later read as a different tree.** The published
asset was built by the tag-triggered run, not by the dispatch whose build was hand-tested. Two
NSIS builds of one commit do not produce identical bytes.

| Fact | Value |
|---|---|
| Tag | `v1.0.0` → `1e6bfd85bd38a21dec1873655effbbf558ee04a9` |
| Dispatch run (hand-tested) | `41461007177`, artifact `13149701933`, 10,375,381 bytes |
| Tag run (published) | `41508308763`, triggered by the tag push at the same commit |
| Release asset | `Document.Reader_1.0.0_x64-setup.exe`, 10,371,633 bytes, asset `702965834` |

The published asset differs from the hand-tested artifact by **3,748 bytes** and by no commit:
both runs are at `1e6bfd8`. 0.9.0 recorded the same phenomenon at 19,260 bytes on one tree, so the
cause is the NSIS build environment rather than the source — the embedded build timestamp alone
moves it. The delta is recorded, not explained away: what a human ran is the dispatch artifact,
and the release carries a rebuild of the identical tree.

The tag run reached step 10, *Assert the PDFium tests actually ran*, successfully.

---

## 1.0.1

**Released 2026-11-03. This section describes the R5 rename to DocuPro.** Tag `v1.0.1` points at
`330a4ad`, the commit the hand-tested installer was built from (dispatch `41651299040`, artifact
`13237103980`); the installer is `DocuPro_1.0.1_x64-setup.exe`, 10,359,989 bytes, sha256
`b7cbe7eb…decab03d`. **Hand-confirmed by the user on 2026-11-03: name checks, Add/Remove double
entry, migration notice, library survival — all pass.** The eight P3.1 commits that landed on top
of `330a4ad` before the tag was cut are the recorded staleness delta; none of them is in this
artifact, and the tag message says so rather than eliding it.

**The published asset is a rebuild, not the hand-tested bytes.** The tag-triggered run
`41685774608` rebuilt the installer from `330a4ad` rather than re-attaching the dispatched
artifact, so the download is 10,360,210 bytes — 221 bytes larger — with sha256
`d940a12f…79da73`. The hand-tested file was `b7cbe7eb…decab03d` at 10,359,989 bytes. Same source
commit, same `productName`, same `1.0.1` version resource; the difference is NSIS/makensis
non-determinism, not different code, and `v1.0.0` has the identical delta (published `de78e8f3`,
recorded `af18fca8`). The provenance rule is about the **source commit**, which is exact and
verified; it does not and cannot make the binary reproducible. Recorded because a sha256 in the
tag message that does not match the download looks like tampering until someone explains it.

**What 1.0.1 contains.** R5 (`docs/DECISIONS.md` §9 item 5, scheduled as 1.0.1 in
`docs/P3-PLAN.md`): the product name and bundle identifier change together in one housekeeping
phase. The name is **DocuPro** — one word, no space, nothing appended, as the user re-ruled on
2026-11-03. The identifier becomes **`com.docupro.app`**.

**The upgrade moves the library, and this version copies it across.** `app_data_dir()` resolves
to `data_dir()/{bundle_identifier}`, so changing the identifier moves `library.db` from
`com.documentreader.app` to `com.docupro.app`. `editor/migrate.rs` copies the old database into
the new directory at startup, before the database is opened (`migrate_legacy_data_dir`). It is a
copy, never a move: the old file stays where it is, so the operation is idempotent, safe across a
crash, and reversible by hand. A destination that already holds a database is left alone, so a
second launch cannot overwrite a library the user has since edited.

**The migration is visible, once.** The About/notices surface reports where the library came
from (`MigrationNotice`, `get_migration_notice`). A migration nobody can see is indistinguishable
from one that failed.

**What 1.0.1 does not do.** It does not rename the repository, the git remote, the crate
(`document-reader`) or the npm package. Those are independent of the product name (D-R7) and a
separate ruling would be needed.

**The Add/Remove consequence is accepted, and hand-observed rather than assumed.** The
identifier is the Windows upgrade identity, so an existing hand-test install is a *different*
application to the installer: both entries appear in Add/Remove Programs until the old one is
removed. That is on record as accepted while the install base is hand-test machines only —
deliberate now, costly later.

**No P3 feature lands under 1.0.1.** Annotations breadth (P3.1) targets 1.1.0.

- The minimum supported version, from `rust-version` in `src-tauri/Cargo.toml`, is **rust 1.90**.
- Verified on **rust 1.91.0**, the version pinned in `rust-toolchain.toml`, `.github/workflows/ci.yml`
  and `release-windows.yml`.

That is the whole of it: this number shipped the rename and the migration, and nothing else.

---

## 1.0.2

**Never shipped, never dispatched, and now retired. It held nothing and no build was made under
it. The tree went from 1.0.1 to 1.1.0 directly.**

This section exists for the mechanical reason `0.9.1` and `1.0.1` did, and the guard proves it:
`v1.0.1` points at `330a4ad` while `HEAD` is untagged, and
`the_tree_version_is_ahead_of_every_published_tag` requires the carriers to name something
higher than the highest published tag. Left at `1.0.1`, that guard is unsatisfiable by
construction for every commit after the tag.

**What the reservation said, and what happened to it.** This number was held — as a *hold*, not
a claim — for "the first user-facing P3.1 release", on the reasoning that a bare patch bump is
the obvious thing for any small intervening change to take and would spend the number the first
installable P3.1 artefact should carry. The P3.1 plan schedules the phase at **1.1.0**
(`docs/P3-PLAN.md` §1), and when the phase reached release prep the plan was followed. So the
hold was never drawn on: 1.0.2 is skipped.

That is the reservation rule working as written rather than an outcome to tidy away. AGENTS.md
calls a version assigned to an unshipped phase *a reservation, not a fact* — if a later phase
publishes first, the earlier one needs a new number. Here the phase it was held for shipped
under its scheduled number instead, which is the same rule read from the other end: the hold
lapsed because the thing it was held for took 1.1.0. A reserved number that was never used is a
smaller problem than a shipped artefact carrying a number that meant "whatever landed that
week" — and it costs nothing but this paragraph.

**No build, no tag, no artifact, no installer.** Nothing was ever dispatched at 1.0.2, so there
are no bytes to reconcile and no provenance to record. Anyone reading a 1.0.2 file name in a log
is looking at a tree state, not a release.

- The minimum supported version, from `rust-version` in `src-tauri/Cargo.toml`, is **rust 1.90**.
- Verified on **rust 1.91.0**, the version pinned in `rust-toolchain.toml`, `.github/workflows/ci.yml`
  and `release-windows.yml`.

---

## 0.9.1

**Development version superseded by 1.0.0. No build was hand-run for it and none is published.**

This section exists because the tree version must stay ahead of every published tag. `v0.9.0`
points at `6caaedd`, and `the_tree_version_is_ahead_of_every_published_tag` requires the
carriers to name something higher; leaving them at `0.9.0` made that guard unsatisfiable by
construction for every commit after the tag.

- The minimum supported version, from `rust-version` in `src-tauri/Cargo.toml`, is **rust 1.90**.
- Verified on **rust 1.91.0**, the version pinned in `rust-toolchain.toml`, `.github/workflows/ci.yml`
  and `release-windows.yml`; `cargo fmt --version` on the verifying box reported
  `rustfmt 1.8.0-stable (f8297e351a 2025-10-28)`.

There is no changelog beyond those two facts, because nothing has shipped under this number.
The next section below is the last release.

### The Windows suite crash: two reproductions, neither from this branch

`94d2d34` is a **documentation-only** commit — it edits `docs/RELEASE-NOTES.md` and
`docs/P2.7.6-embed-DESIGN.md` and no source file — and CI run `41278018858` failed on it at
Windows step 10 (*Assert the PDFium tests actually ran*) with
`process didn't exit successfully ... (exit code: 0xc0000005, STATUS_ACCESS_VIOLATION)`.

`324c131` passed the same job twenty minutes earlier. There are now two timestamped
reproductions:

| Run | Commit | Outcome |
|---|---|---|
| `40908435884` attempt 1 | `3942c8c` | aborted `STATUS_ACCESS_VIOLATION` after 354 of 393 tests; a same-SHA re-run passed |
| `41278018858` | `94d2d34` | `STATUS_ACCESS_VIOLATION` on a documentation-only change |

**This is not caused by any code in this branch.** A docs-only commit cannot introduce a
memory fault, and the first reproduction already passed on a re-run of the identical SHA.
It is the known intermittent teardown fault, still undiagnosed. No fix is attempted here.

## 0.9.0

Closing **P2.7.6** — font embedding (MECH-B), plus the A7 skew fix found while freezing it.

**This section describes `6caaedd`, and that is the commit `v0.9.0` points at.** Earlier
revisions of this block called the section a draft on two grounds; both are now recorded
against the commit they were actually about.

- The red CI run named here originally (`40688437973` @ `dd8efce`) is a property of `dd8efce`,
  not of the tagged commit. At `6caaedd` all four checks are green: *Rust (fmt, clippy, test)*,
  *Frontend (typecheck, test, build)*, *Windows compile check + render smoke*, and *Build
  Windows installer* (run `41154631220`).
- Condition 2 said the installer "from that tagged run" must be hand-confirmed. That is
  unsatisfiable by construction, not merely stale: a tag run *publishes*, so the condition
  demanded confirmation of a build that does not exist until the release is already public,
  and the publish cannot be taken back if the confirmation then fails. The criterion is
  corrected here in both documents, and this is the order 1.0.0 must follow:

  1. **Build only.** `workflow_dispatch` on the release commit publishes nothing; it uploads
     the installer as a workflow artifact.
  2. **A human hand-confirms that artifact.**
  3. **Only then tag.** The tag run republishes the same tree; its asset may differ from the
     hand-tested one by the embedded NSIS build timestamp and by nothing else.

  This is what 0.9.0 did: dispatch `41154631220` (artifact `13024068656`, 10,321,346 bytes) was
  built from `6caaedd` and hand-confirmed, and `v0.9.0` was created on `6caaedd` afterwards.

**What that means in plain terms:** the installer you can download from this release is built
from exactly the source the tag points at. It is *not* the `dd8efce` installer described under
*Hand-test provenance*, which is 12 commits older and lacks the Type0/CID writer, the recursive
Form XObject walk, and the inline-`/Resources` fix.

### What this version contains

P2.7.6 is one phase: **MECH-B, embedding a bundled font for a replacement the run's own font
cannot set.** Measured on the tree at `6caaedd` (the tagged commit), which is 12 commits past
`dd8efce` and 5 past the MECH-B implementation `559e4c7`:

- A replacement containing characters outside WinAnsi (`—`, `→`, `’`, Greek) is no longer
  refused outright. It embeds a subset of a bundled face and writes `/ToUnicode` so copy and
  search return the real characters. Substitution happens only with the user's consent.
- A run whose own font covers the replacement **keeps its own font**
  (`a_subset_that_covers_the_replacement_keeps_the_original_font`).
- Three refusals stay, deliberately, and they are distinct
  (`refusal_split.rs`; `row2`, `row3`, and the subset-builder tests):
  `SUBSTITUTE_REFUSED` (needs a bundled face, substitution not allowed),
  `SUBSET_BUILD_FAILED` (the face had the glyph, the subset could not be written), and
  `GLYPHS_NOT_COVERABLE` (no shipped face has the glyph — the bundle is Latin only).
  **`GLYPHS_NOT_COVERABLE` is not tested for a specific out-of-bundle character.** `row3`
  covers an uncovered glyph; the claim that `漢` is a permanent gap rather than a retry was
  never encoded as a test. Stated here as a design intent, not a measurement.

Type0/CID fonts **are** embedded by this version. P2.7.7 landed in `dcffa91` and `da360fc`,
both ancestors of `6caaedd` (`git merge-base --is-ancestor`), and `6caaedd` routes a CID run to
the CID writer before MECH-A can claim it. See *What 0.9.0 does not include* below, where this
same question is answered; an earlier revision of this paragraph said the opposite and
contradicted that section in the same file.

### The A7 skew half, found after 0.8.1 shipped

This is a behaviour change: a page whose text is sheared (`cm` with a non-zero shear term) or
scaled anisotropically used to be offered in the picker as editable and then refused on save.
It now edits. Two defects, one commit (`6685208`):

1. The write path demanded a uniform scale. The conversion factor must be the length of the
   **advance axis**, because that is the direction a width runs along. Requiring the up axis
   to scale equally refused every sheared and every anisotropically scaled run.
2. A sheared run's quota came from PDFium's **axis-aligned** character boxes, which carry
   `|shear| × glyph height` in their width, so the quota **grew with the shear angle**. Sheared
   runs now use the font's own metric.

Rotation was not affected. `mirrored-text` still refuses. No previously-shipped fixture
changes verdict, because the fixture set had rotation in both `Tm` and `cm` form but no shear
— which is why 0.8.1 could ship this. The new guards build their own shear fixtures.

### The bundled payload

Twelve Liberation faces, from Debian `fonts-liberation 2.1.5-3`, with a per-face SHA-256 record
and a notice reachable from *About and notices* with no document open.

**Measured inside the 0.9.0 installer**, not inferred from the tree: the NSIS payload contains
`fonts/` holding 12 `.ttf` files totalling **4,360,440 bytes**, plus `FONT-RECORD.md` (7,356)
and `LICENSE-Liberation.txt` (4,516). The same 12 faces are also compiled into
`document-reader.exe` via `include_bytes!` (`src-tauri/src/editor/embed.rs:51`); that exe grew
from 20,630,528 to 25,036,800 bytes between the 0.8.1 release asset and this installer, and
0.8.1 contains no `LiberationSans` or `glyf` bytes at all while this one contains both
(`grep -c` over the extracted payload).

Faces were chosen by the distinct-class rule: a face ships only if it owns a style class no
other shipped face owns, which is why the four Narrow faces are not included.

Written font names do **not** use the Liberation name: a subset deletes glyphs, which makes it
a Modified Version under OFL 1.1, and condition 3 forbids a Modified Version from presenting
the Reserved Font Name as its primary name. The name is `/<PREFIX>+<Family>-<Style>`. The
mapping is in `src-tauri/resources/fonts/FONT-RECORD.md`, and a test fails if any written
token contains the reserved name.

### Measured on this tree

- **Linux, PDFium absent, at `dd8efce`:** `cargo test`, measured locally on 2026-10-29 →
  **484 passed / 0 failed / 76 ignored** across 21 targets. The CI job's own floor for this
  commit is `-lt 482` passed and `-ne 76` ignored (`.github/workflows/ci.yml` at `dd8efce`),
  so the local run clears it. **This number was not printed by CI:** the Linux job died before
  its summary step, which is why the run is red for an unrelated reason.
- **Windows, PDFium present, from the release run that built the hand-tested installer**
  (`40719287682`, job *Build Windows installer*, step 10 *Assert the PDFium tests actually
  ran*), verbatim:
  `PDFium tests ran: 559 passed, 0 ignored.`
- **Installer:** `Document Reader_0.9.0_x64-setup.exe`, **10,330,242 bytes**, artifact
  `12830611552` from run `40719287682` @ `dd8efce`. The artifact ZIP itself is 10,314,248
  bytes; the number above is the file inside it.

**The 0.5.0 comparison cannot be made.** No `v0.5.0` tag, no `v0.5.0` release, and no recorded
0.5.0 installer size exists in this repository or on the releases page — the tags are
`v0.1.0`, `v0.1.1`, `version-0.4.4`, `v0.6.0`, `v0.8.0`, `v0.8.1`. So "growth over 0.5.0" is
**not measurable** and is not claimed. What *is* measured is growth over **0.8.1**
(7,823,255 bytes, release asset): **+2,506,987 bytes**, of which the exe accounts for
+4,406,272 bytes, the `fonts/` directory adds 4,372,312 bytes as files, and `pdfium.dll` is
unchanged at 7,380,992 bytes. A large part of the exe growth is the same font data compiled
in, so those two numbers overlap and must not be added.

### Toolchain: what the build was verified against

The declared minimum is **rust 1.90** (`rust-version` in `src-tauri/Cargo.toml`), and the
release build was **verified** on **rust 1.91**, the pinned toolchain in `rust-toolchain.toml`
and in both workflows.

1.90 is not a preference. It is the highest floor the locked dependency graph declares, so it
is the lowest version that can build this tree at all. The previous declaration said 1.77,
which had been wrong for a while: `cargo metadata --locked` on 1.77 fails before compiling
anything (`digest 0.11.3` uses the `edition2024` cargo feature), and 123 locked packages
require a newer compiler. Nothing caught it because every gate runs on 1.91, where an
understated floor is invisible. `tests/release_version.rs` now enforces both halves.

**The guard for that floor failed in CI at `dd8efce`** — a property of that commit, not of
`6caaedd`. The fix is `10591db`, which is an ancestor of the tagged commit, so the guard is
green here.

### Hand-test provenance

`v0.9.0` points at **`6caaedd`**. The installer the tag publishes was built by the *tag* run,
`41273111581`, from that commit; the dispatch run `41154631220` from the same commit
(artifact `13024068656`) is the one hand-confirmed before tagging.

| Fact | Value |
|---|---|
| Tag | `v0.9.0` → `6caaedd5e70080d1da65f52efc4d41103c7650f9` |
| Dispatch run (hand-tested) | `41154631220`, artifact `13024068656`, 10,321,346 bytes |
| Tag run (published) | `41273111581` |
| Release asset | `Document.Reader_0.9.0_x64-setup.exe`, 10,340,606 bytes, asset `697368174` |

The published asset differs from the hand-tested artifact only by the embedded NSIS build
timestamp (10,340,606 vs 10,321,346 bytes, same tree at `6caaedd`) -- the table exists so that a
size difference is not later read as a different source.

The `--include-ignored` suite ran with PDFium on this commit in both runs, and the tag run
reached step 10 *Assert the PDFium tests actually ran* successfully.

Two earlier installers exist and neither is what this release publishes:

| Commit | Run | Why it is not this release |
|---|---|---|
| `dd8efce` | `40719287682` | 12 commits behind `6caaedd`. Lacks the recursive Form XObject walk (`44c9457`), its measurement guards (`3ea86f5`), and the inline-`/Resources` fix (`f9c75d5`). |
| `da360fc` | `41053446124` | 2 commits behind. Lacks the CID routing fix in `6caaedd`. |

An earlier revision of this section described `dd8efce` as the hand-tested commit and used it
to argue the tag should not be `6caaedd`. That argument rests on `dd8efce` being the installer a
human ran, and on this branch's record the 0.9.0 installer a human ran is the `6caaedd` one.
The `dd8efce` row is kept because it is the only installer whose *contents* have been measured
in detail (the `fonts/` payload and exe growth above were read out of that artifact).

### What 0.9.0 does not include

- **P2.7.7 — Type0/CID subset embedding.** **This is in 0.9.0**, and this line previously said
  it was not. It landed in `dcffa91` and `da360fc`, with `6caaedd` routing a CID run to the CID
  writer before MECH-A can claim it, and `v0.9.0` points at `6caaedd`. Corrected on 2026-11-01
  before tagging, from `git merge-base --is-ancestor 6caaedd v0.9.0`.
- **P2.8 — line-level reflow.** Not started. It is 1.0.0 and the last phase of the agreed
  programme.
- **The full-parity inventory (B-1).** Approved and deliberately deferred to *after* P2.8.

### Triage: claims in this section that were not backed

**Status: resolved 2026-10-29.** Each row below was either rewritten to what was measured,
deleted, or kept and marked unbacked in the text itself. The table stays so the next session
can see the reasoning rather than a clean page.

Provenance of the rows: a review on 2026-10-28 recorded nine unbacked lines. That list was
never written into the repository and is not recoverable, so the rows were re-derived on
2026-10-29 by checking every falsifiable claim against the tree and against CI.

Resolution classes used above: *reworded* (the claim now states only what was measured),
*deleted* (the claim was removed), *kept-unbacked* (retained but labelled as intent), and
*measured* (the blocker was settled by an artifact rather than by a gate).

| # | Line as originally written | Claim as written | Basis, and what it showed | Resolution |
|---|---|---|---|---|
| 1 | Draft block, condition 1 | "the PDFium row-1 check must pass in CI" | **CI run** `40908435884` @ `3942c8c`: aborted `STATUS_ACCESS_VIOLATION` after 354 of 393 tests; a re-run of the same SHA passed. Still undiagnosed. | **reworded** → the draft block now names the *actual* red gate at `dd8efce` (the MSRV guard) and the undiagnosed crash is not restated as a condition this section can satisfy. |
| 2 | Draft block, condition 2 | "the installer from that tagged run must be hand-confirmed" | **Git**: no 0.9.0 tag exists. The only installer is a dispatch artifact (`dd8efce`, run `40719287682`). | **reworded** → *Hand-test provenance* names that artifact and states the 16-commit gap. The old text implied the installer was behind by 11 commits; measured, it is 16. |
| 3 | Draft block | "the suite is green at 482 passed / 76 ignored on Linux" | **Measured locally** at `dd8efce`: 484 passed / 0 failed / 76 ignored. 482 was the old baseline; ci.yml's floor at that commit is also 482. | **reworded** → *Measured on this tree* quotes 484 / 76 and names the run and the date. |
| 4 | "What is in this release", list of four | "This section covers **four** things" | **Git**: the four named commits exist, but later commits are also unreleased. | **deleted** → the numbered list is gone; the section is organised by subject, not by commit count. |
| 5 | "MECH-B font embedding", opening | "Twelve Liberation faces now ship in the installer (4,360,440 bytes)" | **Measured in the installer**: `fonts/` holds 12 `.ttf` totalling 4,360,440 bytes. Also compiled into the exe via `include_bytes!`. | **measured** → the claim is now backed by artifact `12830611552` and says it was checked inside the payload *and* names the exe growth. |
| 6 | "MECH-B font embedding", error codes | The three refusals are distinct | **Tree**: distinct constants and paths; covered by `row2`, `row3`, `refusal_split.rs`, subset-builder tests. | **reworded** → names those tests inline. |
| 7 | "MECH-B font embedding" | "A run whose own font covers the replacement keeps its own font" | **Tree**: `a_subset_that_covers_the_replacement_keeps_the_original_font`. | **reworded** → names the test inline. |
| 8 | "MECH-B font embedding" | "`GLYPHS_NOT_COVERABLE` … `漢` is a permanent gap, not a retry" | **Tree**: no test asserts permanence for a specific out-of-bundle character. `row3` covers an uncovered glyph only. | **kept-unbacked** → the bullet now says it is a design intent, not a measurement. |
| 9 | 0.9.0 section throughout | The section describes what 0.9.0 *is* | **Git**: unreleased commits were undescribed — the Form XObject walk (`44c9457`, `3ea86f5`), the MSRV guards (`8c29534`, `10591db`), the inline-`/Resources` fix (`f9c75d5`). | **reworded** → *Hand-test provenance* lists them by SHA and says they are in no hand-tested installer. |

Row 5 is the one that moved class: it was `needs-installer` and is now measured, because the
artifact it needed existed and had only not been opened.

---

## 0.8.1

Closing **P2.7.4** — text drawn at an angle becomes editable in place. Rotated and skewed
runs used to be refused outright; they are now measured, so the width gate compares two
quantities that are actually comparable.

> **Pre-release.** P2.7.6 (font embedding) is still ahead, so text editing is not complete
> and this is not a stability guarantee.
>
> **This was drafted as 0.7.0 and is published as 0.8.1.** Q7 assigns P2.7.4 to 0.7.0, but
> 0.8.0 (P2.7.5, CID) was published *before* this phase landed. 0.7.0 is therefore below an
> already-published release, and an auto-updater comparing semver would never offer it. The
> version was raised to 0.8.1 so that published history stays monotonic and the label
> describes what the tree contains: transforms on top of CID. See "Phase and version
> sequences diverged" below.

### Text at any angle is measured along its own axis

The refusal was not there because rotated text is hard to write. It is not: a replacement
is drawn through the same text matrix, so its advance runs along the same axis as the
original's by construction. The refusal existed because the **other side** of the comparison
was wrong — `advance` was measured along the text axis and then compared against a
horizontal box width.

For a run rotated 90°, a 114.67pt word was being checked against an 18.12pt budget, which is
the height of its glyphs. Runs now carry their advance and up axes in page space, and every
measurement that used to read `x` or `y` projects through those axes instead. On horizontal
text the axes are `(1,0)`/`(0,1)`, so every previous number is unchanged — that identity is
pinned by a test, because it is what makes this safe.

Measured: the quota for the rotated fixture is **114.67pt** (its true advance), not 18.12pt.
An overlong replacement now refuses with both true numbers: *"needs 229.3pt, available
114.7pt"*. Before, the same edit refused for the wrong reason with a meaningless pair.

### Rotated edits save and round-trip

A rotated replacement is written, the file saved, re-read as `TURNED`, and the inverse edit
restores `ROTATED`. That acceptance path did not exist in the suite before; it does now, and
it is mutation-verified.

### Mirroring still refuses, and now says so

Text drawn reflected advances counter to its own glyph order, so a replacement would render
backwards. Those runs keep a refusal, under their own gate `mirrored-text` rather than being
lumped in with rotation. The picker and the write path both refuse, so a caller cannot skip
the check.

### A regression this phase introduced, and how it was caught

The refactor briefly compared a character's far edge where the original compared its near
edge. Because PDFium's box ends at the text's advance, that dropped **the last glyph of every
line** — 333pt measured against a true 342pt span on the brochure's first line.

All 428 tests that run without PDFium passed while this was broken. Only the PDFium-gated
tests caught it. That is now a standing rule rather than a lesson: `cargo test --
--include-ignored` must pass before a phase is marked complete, recorded in `AGENTS.md`. The
boundary also has a direct unit test now, so the next refactor of this code does not depend
on a whole-line assertion happening to notice.

### Known limits

- **Skew — corrected after release, fixed in 0.9.0.** The paragraph below was published with
  0.8.1 and is **not accurate for that release**. Measured 2026-09-20 and fixed on top of it:
  a *sheared* run was refused by the write path outright (every skew and every anisotropic
  scale), and where it was measured at all its quota came from PDFium's axis-aligned grid box
  rather than the fallback path described here — 124.88pt for a 114.67pt line at 20°, growing
  with the shear angle, so an overprinting replacement was *accepted*. Rotated text was
  correct in 0.8.1; skewed text was not. See §0.9.0 and the A7 fix.
  ~~PDFium's box is axis-aligned in page space, so for skewed text the box's extent
  projected onto the advance axis is larger than the true advance. That path is only reached
  when no character span can be located, and the existing slack absorbs it at realistic
  angles.~~
- **Near-parallel merging:** fragments whose angles differ by less than 1.15° merge. That is
  correct for rounding noise, but a rotated line adjacent to a horizontal one at a nearly
  identical angle is treated as one line.

### Phase and version sequences diverged

The phase sequence and the version sequence did not run in the same order, and the history
should say so rather than leave a gap someone has to reconstruct.

Q7 assigns one minor bump per phase in phase order: 0.5.0 → P2.7.1, 0.6.0 → P2.7.3,
0.7.0 → P2.7.4, 0.8.0 → P2.7.5, 0.9.0 → P2.7.6, 1.0.0 → P2.8. That ordering assumes the
`0.7.0` slot would be filled before `0.8.0`. It was not: **P2.7.5 (CID) was numbered 0.8.0
by ruling and published on 2026-10-25, while P2.7.4 (transforms) was still open.** CID
shipped ahead of transforms, so the version sequence and the phase sequence diverged at that
point — the published `v0.8.0` tree does not contain `RunFrame`, while this tree does.

Labelling this release 0.7.0 would have reproduced the divergence permanently, with a
superset numbered below a published subset. It was raised to **0.8.1** instead: it is above
every published release, and the minor component still identifies the phase it closes
(P2.7.4), so the map stays readable.

**The ruling was corrected, not just this one number.** The open question in Q7 was whether
the *next* phase would hit the same wall. It would: P2.7.6 was pencilled in at 0.9.0 while
0.8.1 exists, which leaves no room between them for the phase after it. The corrected map is:

| Phase | Version |
|---|---|
| P2.7.1 | 0.5.0 |
| P2.7.3 | 0.6.0 |
| P2.7.5 (CID) | 0.8.0 — published first |
| P2.7.4 (transforms) | **0.8.1 — this release** |
| P2.7.6 (embedding) | **0.9.0** |
| P2.8 (reflow) | 1.0.0 |

0.9.0 is the last minor before 1.0.0, so P2.7.6 is the last phase that may take a minor bump.
That is consistent with P2.8 being the final phase of the agreed programme, and it is why
1.0.0 must not be reached by accident.

The lesson recorded here is narrower than "bump earlier": a version assigned to a phase that
has not shipped is a reservation, not a fact. When a later phase ships first, the reserved
number is spent and the earlier phase needs a new one.

### Build status

**Accepted and published as a pre-release on 2026-10-26.**

- Gate: CI run `40244724597` on `cb8693e` — all three jobs green. Linux `432 passed / 71
  ignored`, Windows `502 passed / 0 ignored`.
- Installer: run `40244727545` (workflow_dispatch, so artifact only — no tag, no release).
  `Document Reader_0.8.1_x64-setup.exe`, 7,824,005 bytes, SHA-256
  `7a20148667a2eff36231c6e2bedd442866a70d5325892e2d122f21a3c2be0a17`, artifact `12618531386`.
- **Hand-confirmed 2026-10-26** by the user against that artifact and that SHA-256. This is
  the Q9 confirmation: the gate was already green, so both closing conditions are met and
  the plan row moved `[~]` → `[x]`.
- Published as tag `v0.8.1` (pre-release; `release-windows.yml` sets `prerelease: true`
  because P2.7.6 and P2.8 are still ahead).
- Payload measured, not assumed: `document-reader.exe` 20,630,528 bytes and `pdfium.dll`
  7,380,992 bytes, both `machine 0x8664`, both PE32+. The version resource reports `0.8.1`.
  (The NSIS stub itself is x86, which is normal and unrelated to the payload.)

### The published installer is not the hand-tested one, and that is measured

The tag run rebuilt from source, and a rebuild is not reproducible here: this project embeds
a PE timestamp and a PDB identity. The published asset is therefore a **different file with a
different hash**, and it was not itself the thing the user ran:

| | hand-confirmed (artifact `12618531386`, run `40244727545` on `cb8693e`) | published (`v0.8.1`, run `40353002998` on `1b88c80`) |
|---|---|---|
| installer size | 7,824,005 | 7,823,255 |
| installer SHA-256 | `7a201486…be0a17` | `59ead919…75a9e0` |

Measured, not argued: the two installers were unpacked and their payloads compared.

- `pdfium.dll` — **byte-identical** (`d42c452a…`), 7,380,992 bytes.
- `document-reader.exe` — same size (20,630,528), **28 differing bytes in 9 runs**, and
  `.text` and `.rsrc` are **byte-identical** across their full extents.
- Of those 28 bytes: 16 are the PE timestamp in the header and COFF/debug-directory
  timestamps; 3 are the PDB `RSDS` GUID; 2 are one debug-directory field; 1 is the PDB age.
  The remaining 6 are a single `u32` in the Rust debug-string table that changes value from
  `0x27` to `0x23` — and `0x27`/`0x23` are 39/35, which are the line numbers of the **same**
  `#[derive(Debug, …)]` attribute in `render.rs` before and after the four-line doc comment
  added in `c0fad4f`. The only source difference between the two builds is a comment.
- Conclusion, stated as a limit rather than a reassurance: the published binary's executable
  code is identical to the hand-tested one, but **the published installer itself has not been
  run**. Both the SHA-256 above and this paragraph are here so that fact is not discoverable
  only by comparing release assets.

The remedy, if byte-for-byte traceability is wanted for future phases, is either to make the
build reproducible (drop the timestamp, fix `SOURCE_DATE_EPOCH`) or to tag the commit the
installer was actually built from and confirm *that* build — not to describe a rebuild as if
it were the tested bytes. Not changed in this release.

Between the gated commit `cb8693e` and this tag, `src-tauri/src/` changes only by a doc
comment (the `DEFAULT_RENDER_CACHE_BYTES` "unmeasured placeholder" note) plus the rewritten
`tests/memory.rs`. No product behaviour changed after the run that was hand-tested.

---

## 0.8.0

Closing **P2.7.5** — text set in CID-keyed (Type0) fonts becomes editable in place. This is the
phase that makes the reported line editable: *"Each hour of lecture – 2-3 hours of study"* on
page 4 of the Maastricht brochure refused with `simple-font` in 0.6.0 and is editable here.

Measuring that line turned up two defects that had nothing to do with CID, and both were losing
data or lying about a refusal, so they ship in the same section.

### CID / Type0 text is edited through its own `/ToUnicode`

A Type0 font stores **codes**, not characters: a byte pair is a CID, and the CID means nothing
without the font's own map back to text. 0.6.0 refused every such run, which is why real
producers — Word, LibreOffice, anything built on a modern PDF library — produced documents with
uneditable body text.

- The `/ToUnicode` CMap is read into a character↔CID map. A replacement is written as CIDs
  through that same map, so what is drawn is what the user typed. The CMap is **read, never
  rewritten**: it is shared per font, and edits must not affect other text set in it.
- `/Identity-H` fonts are handled by glyph id when `/ToUnicode` is absent but the encoding is
  identity.
- A Type0 font with no readable `/ToUnicode` is still refused — nothing can be shown to be
  what the user typed — now with gate `font-encoding` rather than a blanket `simple-font`.
- Advance width comes from the font's `/W` array, then `/DW`. It is never estimated at half an
  em, because `fits-width` refuses on that measurement and a guess would make it wrong.

### Merging no longer crosses font faces

The picker merged a line into one run when the fragments shared a *style class*, and
`style_match` maps `ArialMT`, `ABCDEE+Monaco` and a Wingdings face **all** to `Helvetica`. A
merged write draws the whole replacement through the head fragment's font and clears the
others, so the Arial bullet and the Wingdings icon in that line were being **silently
discarded** — a data-loss bug independent of CID.

Fragments now merge only when they agree on `style_class` **and** on the face (`/BaseFont`,
ignoring the subset prefix) **and** both can draw the replacement. Comparing the font resource
name instead would be too strict: the brochure sets one Times face under two names (`F4`/`F5`),
and that legitimately-is-one-run case must still merge.

### A compressed embedded font is decompressed before it is parsed

`/FontFile2` streams are usually Flate-compressed, and the coverage check handed ttf-parser the
still-compressed bytes, so parsing failed for **every real embedded font**. Coverage then
always reported "not covered" and the editor substituted a font the original could have set.
Synthetic fixtures stored their programs uncompressed, which is why only real documents showed
it. Compressed programs are now decompressed first; a filter that cannot be decoded degrades to
a refusal, never a panic.

### Refusal copy says what is actually possible

The message offered "add a text annotation instead", but the annotation tools are ink-only, so
it promised something the app cannot do. It now reads *"select a narrower range, or use the
annotation tools to add a note"*, and the substitute-font toggle says that CID/Type0 runs keep
their own font rather than implying the toggle covers them.

### Known gaps

- Rotated and skewed text is refused, not editable (P2.7.4, shipped as 0.8.1 — this
  section said 0.7.0 when `v0.8.0` was tagged; see §0.8.1).
- Text inside nested Form XObjects beyond the depth cap is refused (P2.7.3).
- A CID/Type0 font without a readable `/ToUnicode` is refused with `font-encoding`.
- Non-WinAnsi text that needs an embedded font is refused with `GLYPHS_NOT_COVERABLE` (P2.7.6).
- Line-level reflow does not exist: a replacement wider than the run is refused, quoting both
  measured widths (P2.8, targeted at 1.0.0).

> **Phase numbering.** This work was previously drafted as "P2.8, target v0.7.0". The frozen
> plan (ruling Q7) is authoritative: CID is **P2.7.5 → 0.8.0**, P2.7.4 transforms followed it
> as **0.8.1** (not 0.7.0 — 0.8.0 was already published, see §0.8.1), and **P2.8 is
> line-level reflow at 1.0.0**. Only the label changed here; the design and scope did not.

### Upgrading

No data migration. Documents edited and saved by earlier versions re-open and edit as before.
Files saved by this version are ordinary PDFs with no new structures; the only content that
changes is text the user actually edited.

### Pre-release status, acceptance and provenance

This is a **pre-release**, not a stable release. P2.7.4 (rotated/skewed text) and P2.7.6
(embedding) are still ahead of it, so `fits-width` still refuses on the un-rotated axis only
and non-WinAnsi text that needs an embedded font is still refused. Text editing is not
complete until P2.8 at 1.0.0; do not read this version as a stability guarantee.

**Acceptance.** P2.7.5 requires both a green gate and a confirmed installer (Q9), so both are
recorded here:

- Gate green: CI run `40157718290` on `2199903` — `Rust (fmt, clippy, test)`,
  `Frontend (typecheck, test, build)` and `Windows compile check + render smoke` all passed,
  on both `p2-windows-test` and `main`.
- Installer confirmed: `Document Reader_0.8.0_x64-setup.exe`, built by run `40158689217` from
  `2199903`, SHA-256 `5c11e43f02e05883597132f98cee4dc93400295e163241f33b602b141959d1cc`,
  7,827,785 bytes, PE32 (NSIS). Hand-tested and accepted by the user on 2026-10-25; the
  reported line on page 4 of the Maastricht brochure edits in place.
- The asset attached to the `v0.8.0` release is **rebuilt** by the tag run from the same
  application source, so it is source-identical to the accepted build but not byte-identical.
  Measured, rather than assumed: the two installers differ in 24 bytes of 20,619,264 in the
  packed `document-reader.exe` — the linker's build timestamps and the `RSDS` PDB GUID — while
  `pdfium.dll` and every NSIS file are identical. Nothing in the compiled code differs.
  Published asset: 7,827,607 bytes, SHA-256
  `bfb7504af9a8e6e3d5668bf98ba2ccad95d732d7fa9a29aee844188aacde66cc`.

## 0.6.0

Closing **P2.7.3** — text inside Form XObjects becomes editable. The theme is still silent
wrongness: the whole phase is shaped so that editing one instance of a shared form cannot change
another, and so that text the walker cannot reach is *shown* as refused rather than quietly
missing from the picker.

### Text drawn by a Form XObject is reachable

A form is a content stream a page draws with `Do`, and plenty of real producers put running
headers, sidebars and cover art in one. Every run inside such a form was absent from the picker
before this release, so a large class of documents was partly untouchable with no visible reason.
The locator now descends `Do` → `/Subtype /Form` and addresses runs inside with the resource
**names** that reach them, which survive `renumber_objects` on save where an object id would not.

Nested forms are walked level by level (a form may draw another), up to a depth of 8 with a
visited-set for cycles — a form that draws itself is legal to write and hangs a naive walker. Past
either limit the `Do` reports itself as a **placeholder run** carrying the new `nested-xobject`
gate, naming the depth or the cycle and pointing at text annotations as the way out. A refusal
you can see is the point: text silently dropped from the picker reads as "there is no text here".

### The edit unit is the **draw**, not the page (Q2)

When the same form is drawn twice on a page, or on two pages, editing the instance you clicked
changes that instance and nothing else. The copy is per draw — not per page, and not per form:
the write path clones the form along the whole path (outermost first, fresh names `X1_edit1`, …),
repoints only the clicked `Do` at the clone, and writes the clone's own stream. The other draw
keeps its stream byte for byte. Duplicating a small stream is harmless; an unintended visible
change on a page you did not edit is not.

### A private `/Resources` only when the edit touches it (Q3)

The stream is always copied; a form's `/Resources` is copied only when the edit needs to change
it — a font substitution, or a named `/Properties` rewrite. Then the clone gets its own dictionary
and the original stays readable by every other form. Sharing it would recreate the named
`/Properties` problem one level down. A form that declares no `/Resources` still resolves names
through the page, exactly as ISO 32000-1 §8.10 says, so inherited fonts and child forms keep
working.

### One undo per edit

The clone chain rides the existing grouped inverse, so a single undo repoints each `Do` back to
the original name, restores the bytes, font and `ActualText`, and drops the clone objects and any
private resources dictionary. Edit-then-undo leaves no orphan in the file, and it does not touch
the draw you never edited.

### Known gaps

- Rotated and skewed text is refused, not editable (P2.7.4).
- Forms nested more than 8 deep, and forms that draw themselves, are refused with
  `nested-xobject` (P2.7.3 Q1). A text annotation is the way out.
- CID / Identity-H fonts are refused (P2.7.5).
- Non-WinAnsi text that needs an embedded font is refused with `GLYPHS_NOT_COVERABLE` (P2.7.6).
- Line-level reflow does not exist: a replacement wider than the run is refused, quoting both
  measured widths (P2.8).

### Upgrading

No data migration. Run addresses gained an optional XObject path that is omitted when empty, so
every 0.5.0 payload and every un-migrated undo record keeps its exact meaning. Files gain content
only where a form was edited, and only as the added clones described above.

---

## 0.5.0

Closing P2.7.1 — tagged-structure awareness for text editing. The theme is **silent
wrongness**: both defects below produced a successful-looking edit that wrote the wrong thing,
which outranks any refusal.

### Merges stop at the document's own structure (`MCID` boundaries)

Our merge boundary was a heuristic (same style class, same baseline, gap inside a window);
Acrobat's is the document's own structure. Two same-style fragments in different tagged units —
a table cell and the cell beside it, a caption and the figure label under it — satisfied every
geometric predicate and became one editable run, so an edit to "Revenue 2024" could land across
a cell boundary. Runs now carry their marked-content context (`BDC`/`BMC`/`EMC` with `MCID`),
and two adjacent fragments with a different innermost structure element stay two runs whatever
their geometry says.

The boundary is the resolved **block** structure element (via `/StructTreeRoot /ParentTree`),
not the raw `MCID`: the Maastricht brochure is fully tagged with five `MCIDs` inside one visible
sentence, so splitting per `MCID` would have fragmented lines that must stay single runs.
Untagged pages behave exactly as in 0.4.5.

### `ActualText` is preserved on edit

A tagged PDF may wrap text in `/Span <</ActualText <FEFF…>>> BDC`. Editing the operand bytes
left that string untouched, so the rendered text and the semantic text diverged — an
accessibility regression that also makes copy/paste in Acrobat disagree with the page. A
successful edit now rewrites the `ActualText` of the span it edited, encoded `FE FF` +
UTF-16BE, as part of the same pass, so one undo restores both.

Two cases deliberately do not rewrite:

- A `BDC` shared by more than one operand is left alone (`actual_text_preserved = false`): we
  cannot know which operand the string should describe.
- A value we cannot decode is never overwritten. The flag goes false and the property stays
  byte-identical.

### `ActualText` decoding: real PDFDocEncoding (B-6)

`decode_text_string` claimed PDFDocEncoding but mapped bytes with `byte as char`, which is
Latin-1. The two differ across `0x80`–`0x9F`, so a `•` (byte `0x80`) could be rewritten as
U+0080 — a string different from the one we read. The decoder now implements ISO 32000-1
Annex D table D.2, tested exhaustively over all 256 codes and cross-checked against qpdf,
poppler and pypdf. The three codes the standard leaves undefined (`0x7F`, `0x9F`, `0xAD`) yield
`None` rather than a guess.

### Named `/Properties` are copied before they are rewritten — copy-on-write (B-7)

`ActualText` is often declared once in a page's `/Properties` dictionary and named by several
`BDC`s. Rewriting it in place would change text on other, unedited sequences. The write path now
clones the entry under a fresh key (`/P0_edit1`, …), repoints only this `BDC` at the clone, and
rewrites the clone; the original stays byte-identical for every other sequence. The clone is
recorded on the inverse, so undo repoints the `BDC`, deletes the key and drops the object — an
edit-then-undo leaves no orphan in the file.

### `Ts` (text rise) is tracked

Text rise does not move the pen, but it moves the ink, so it changes which box a run pairs to.
It was ignored, so a superscript could pair to the baseline box. Now recorded and compared as
part of the run's baseline, without touching the advance `fits-width` measures.

### Inherited and shared page resources are resolved before writing

Found while testing the above, and fixed in the same phase:

- **Substitute font dropped from the page.** `ensure_substitute_resource` wrote into the first
  *referenced* `/Resources` object up the `/Parent` chain. On a page that shadows its inherited
  resources that is the ancestor's dictionary, which the page cannot resolve — the edited run
  named `FS1` and stopped rendering. It now resolves the slot the page actually reads and gives
  the page a private copy of what it inherits.
- **Watermark erased the page's fonts.** The watermark rebuilt `/Resources` from the page's own
  inline dictionary only, dropping every inherited font and XObject, and reused the name `/F1`,
  redrawing the page's own text in Helvetica. It now uses the same resolver and its own name,
  `WmkFont`.

### Rotation is refused, not mis-measured (hotfix, shipped separately)

Rotated text was reported editable while its advance was measured on the horizontal axis, so
`fits-width` judged the wrong dimension: editable *and* wrong. Such runs now refuse with gate
`rotated-text` and are still returned to the picker, so the UI can explain the refusal. The
rotation is read from the matrix the renderer actually uses, `Tm` composed with the current
transformation matrix, so a page that rotates with `cm` and leaves `Tm` at the identity is
caught too; `q`/`Q` restore the graphics state as the renderer does. Correct measurement lands
in P2.7.4.

### Known gaps

- Rotated and skewed text is refused, not editable (P2.7.4).
- Text inside Form XObjects is not reachable (P2.7.3).
- CID / Identity-H fonts are refused (P2.7.5).
- Non-WinAnsi text that needs an embedded font is refused with `GLYPHS_NOT_COVERABLE` (P2.7.6).
- Line-level reflow does not exist: a replacement wider than the run is refused, quoting both
  measured widths (P2.8).

### Upgrading

No data migration. Edited copies written by earlier versions re-open and edit as before; the
only files that gain content are ones saved by 0.5.0, which may carry an extra `/Properties`
clone while an edit stands.
