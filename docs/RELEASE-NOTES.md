# Release notes

One section per shipped version, newest first. A version section is written **before** the
version bump lands, because the bump is what puts those bytes in an installer. The test
`src-tauri/tests/release_version.rs` enforces both halves: every version carrier agrees, and
the version in `Cargo.toml` has a section here.

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

- Rotated and skewed text is refused, not editable (P2.7.4, targeted at 0.7.0).
- Text inside nested Form XObjects beyond the depth cap is refused (P2.7.3).
- A CID/Type0 font without a readable `/ToUnicode` is refused with `font-encoding`.
- Non-WinAnsi text that needs an embedded font is refused with `GLYPHS_NOT_COVERABLE` (P2.7.6).
- Line-level reflow does not exist: a replacement wider than the run is refused, quoting both
  measured widths (P2.8, targeted at 1.0.0).

> **Phase numbering.** This work was previously drafted as "P2.8, target v0.7.0". The frozen
> plan (ruling Q7) is authoritative: CID is **P2.7.5 → 0.8.0**, 0.7.0 belongs to P2.7.4
> transforms, and **P2.8 is line-level reflow at 1.0.0**. Only the label changed here; the
> design and scope did not.

### Upgrading

No data migration. Documents edited and saved by earlier versions re-open and edit as before.
Files saved by this version are ordinary PDFs with no new structures; the only content that
changes is text the user actually edited.

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
