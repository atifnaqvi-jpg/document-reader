# Release notes

One section per shipped version, newest first. A version section is written **before** the
version bump lands, because the bump is what puts those bytes in an installer. The test
`src-tauri/tests/release_version.rs` enforces both halves: every version carrier agrees, and
the version in `Cargo.toml` has a section here.

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

Rotated text (`Tm` with a non-zero off-diagonal) was reported editable while its advance was
measured on the horizontal axis, so `fits-width` judged the wrong dimension: editable *and*
wrong. Such runs now refuse with gate `rotated-text` and are still returned to the picker, so
the UI can explain the refusal. Correct measurement lands in P2.7.4.

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
