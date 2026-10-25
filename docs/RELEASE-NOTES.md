# Release notes

One section per shipped version, newest first. A version section is written **before** the
version bump lands, because the bump is what puts those bytes in an installer. The test
`src-tauri/tests/release_version.rs` enforces both halves: every version carrier agrees, and
the version in `Cargo.toml` has a section here.

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

- **Skew:** PDFium's box is axis-aligned in page space, so for skewed text the box's extent
  projected onto the advance axis is larger than the true advance. That path is only reached
  when no character span can be located, and the existing slack absorbs it at realistic
  angles.
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

**Built, not published, not yet accepted.** Per Q9 this phase stays `[~]` until the installer
has been hand-tested.

- Gate: CI run `40244724597` on `cb8693e` — all three jobs green. Linux `432 passed / 71
  ignored`, Windows `502 passed / 0 ignored`.
- Installer: run `40244727545` (workflow_dispatch, so artifact only — no tag, no release).
  `Document Reader_0.8.1_x64-setup.exe`, 7,824,005 bytes, SHA-256
  `7a20148667a2eff36231c6e2bedd442866a70d5325892e2d122f21a3c2be0a17`, artifact `12618531386`.
- Payload measured, not assumed: `document-reader.exe` 20,630,528 bytes and `pdfium.dll`
  7,380,992 bytes, both `machine 0x8664`, both PE32+. The version resource reports `0.8.1`.
  (The NSIS stub itself is x86, which is normal and unrelated to the payload.)

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
