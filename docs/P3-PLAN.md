# P3 — Acrobat-parity programme (PLAN)

Status: **APPROVED in principle** by the user on 2026-11-01 (rulings recorded in
`docs/DECISIONS.md` §9; inventory in `docs/P3-GAP-ANALYSIS.md`). Phases below are
**named, not designed**: every phase gets its own design doc, frozen decisions
before code, user sign-off, then implementation — the stage gate that has governed
every phase since P2.4.

> **Design docs written 2026-11-03, awaiting sign-off:** the rename housekeeping
> (`docs/P3-rename-housekeeping-DESIGN.md`) and P3.1 annotations
> (`docs/P3.1-annotations-DESIGN.md`). Both are **design only — no code has been written for
> either phase**, per the stage gate. Sign-off is on the two frozen decisions tables, and on
> the bundle identifier the rename doc proposes (D-R3).

> **Entry condition: SATISFIED 2026-11-02.** P2.8 closed with tag `v1.0.0` at `1e6bfd8` —
> the user hand-confirmed the installer (build-only dispatch `41461007177`), the tag run
> published a full release carrying `Document Reader_1.0.0_x64-setup.exe`, R-7 is ticked,
> and A10 sits in the Have column (`c16c388`). **P3 is armed.** The rename housekeeping
> remains the first phase (tree reserves **1.0.1** for it): design note first, user sign-off
> on the bundle identifier before any code. The P3.1 design doc follows in the same
> sign-off pass.

Requirements source: `docs/reference/document-reader-capabilities.md`.
Charter: local-first (DECISIONS §9.1). Version cadence: one minor bump per phase
(Q7 rule), continuing after 1.0.0.

## 1. Phase rows

| Phase | Delivers | Ruling | Notes and invariants | Version |
|---|---|---|---|---|
| **Rename housekeeping** ✅ | Product name + bundle identifier → **DocuPro** | R5 | Accepted consequence (DECISIONS §9.5): the identifier is the Windows upgrade identity, so existing hand-test installs update by reinstall and both entries appear in Add/Remove — deliberate now, costly later. **Delivered and hand-tested 2026-11-03** on the installer from dispatch `41651299040` at `330a4ad`; tagged `v1.0.1` there | 1.0.1 |
| **P3.1 — Annotations breadth** ✅ | Palette to ~15 types (highlight family: highlight/underline/strikeout/squiggly; note family: sticky/textbox/callout/typewriter; ink; line/arrow; rect/oval/polygon-family shapes), comment-manager pane (list/sort/filter/statuses), reply chains, XFDF import/export | — | Annotations stay **real PDF annotations** written by the lopdf path (P2-D3), DB remains the source of truth (P2-D4), FTS5 push and jump-to-page keep (P2-D5/D5c), undo keeps its coalescing rules (P2-D7). Sharing stays Out (charter). **CLOSED at v1.1.0** — see below | 1.1.0 |
| **P3.2 — Protect & redact + offline signatures** ✅ | Password + permissions **on save**; true redaction (mark/apply) incl. search-and-redact text patterns; sanitize / remove-hidden-info; PKCS#7 sign + verify, certify (DocMDP), certificate selection from the Windows store | R3 | Redaction and signatures carry **correctness obligations**: named invariants up front in the phase design (per the B-1 note). R3: offline only — no TSA call, the document hash never leaves the machine; timestamping/trust-services stay Out until separately ruled. Redaction ≠ delete-pages+flatten; the design must prove content removal, not visual hiding  **Delivered and hand-tested 2026-11-13** on the installer from dispatch `43164026953` at `b2d0b3b` (`DocuPro_1.2.0_x64-setup.exe`, 11,091,275 bytes, sha-256 `989e2dfb…c2597c30`). The one case that did not complete is vector drawing under a mark, which is the **decided** behaviour (§8) and now has its own future row in §2 — not a defect. | 1.2.0 |
| **P3.3 — Forms** | AcroForm fill (text/checkbox/radio/list/dropdown) + save through the edited-copy discipline (P2-D2); FDF/XFDF form-data import/export | — | **Design: `docs/P3.3-forms-DESIGN.md` (draft, awaiting sign-off; no code).** The row's old "pdfium-render form FFI spike first (`pub(crate)` per P2-DESIGN F1/F7)" premise was **measured wrong** on 2026-11-13: text fields fill through the public API, and F1/F7 are annotation-creation and true redaction, not forms. What *is* measured: `set_checked` in **pdfium-render 0.8.37** returns `Ok` and writes `/V(Yes)` as a **string** with no `/AS`, so buttons cannot ship through the crate — **ruled 2026-11-14: we write `/AS` and a name-typed `/V` with `lopdf`, proved by a pixel test that the box ticks in a real viewer.** Combo/list have no mutable accessor at all. A real-producer fixture is permitted without personal data (README must say what it is and why). Authoring (create fields, tab/calc order) and barcode fields remain optional rows needing their own design. Fill & Sign is **not** here — see P3.3a | 1.3.0 |
| **P3.3a — Fill & Sign (visual signatures)** | Drawn / typed / image signature placed as a page overlay, with a visible permanent "visual signature, no cryptographic weight" label | — | **Ruled 2026-11-14: its own phase, the phase after forms.** Shares no machinery with AcroForm (a content overlay, not a field-dictionary edit) and would double P3.3's hand-test surface. Inherits D-F8 and I-F6 from `docs/P3.3-forms-DESIGN.md`: never presented beside P3.2's `valid`/`modified` badges, and no code path returns a cryptographic verdict for one. Its own design doc and sign-off before code, per the stage gate | 1.4.0 |
| **P3.4 — Organize completion + compress** | Page **insert** and document merge (clears B1b; retires `pages::merge` dead code), combine files, crop, page numbering/Bates-light; compress/optimize (linearization + image downsampling + object-stream cleanup) | R4 | Compress runs in a **bundled qpdf sidecar process** (Apache-2.0): bundling, invocation and crash containment frozen in the phase design before code; the editor's own writer never shells out silently | 1.5.0 |
| **P3.5 — OCR** | Recognize text on-device (tesseract + bundled language packs), produce searchable PDFs (image page + invisible text layer behind the extraction path), suspects-correction UX deferred | R2 | Entirely behind the `TextExtractor` trait seam (DECISIONS Q3 seam); no network calls, ever (charter). The detect-and-surface behaviour for image-only PDFs is superseded by recognition but the surface-not-fail rule keeps | 1.6.0 |
| **P3.6 — View modes** | Continuous / facing / continuous-facing layouts, fit-page/fit-width/actual, thumbnails pane, tabbed multi-document | — | Render-cache keys must respect mode changes (P2-D6 lesson: keys carry path+page, mode enters the key space by design, not by hash); memory budget re-measured with facing layouts | 1.7.0 |

### P3.1 — CLOSED at v1.1.0

Closed 2026-11-07 on the two conditions the phase protocol requires, both recorded rather than
asserted:

| Condition | Evidence |
|---|---|
| Gate green, both shapes | plain `623 passed / 96 ignored`, `--include-ignored` `719 passed / 0 ignored` at the tagged tree; vitest 9 files / 117 tests; clippy clean with `-D warnings` |
| User hand-tested the installer | build-only dispatch `42331384386` at `be00673` → artifact `13557413255`, `DocuPro_1.1.0_x64-setup.exe` (10,448,919 bytes, sha-256 `5bbd6c00…`). Verdict: *"Note works fine — the reported note-box text defect is fixed on the new build."* |
| Tag | `v1.1.0` at `8950403` |
| Published release | https://github.com/atifnaqvi-jpg/document-reader/releases/tag/v1.1.0 — full release (`prerelease: false`), Windows installer attached |

The tag is **three commits past** the hand-tested `be00673`: two fix the release body and the
notes, one records this. That gap is disclosed in `docs/RELEASE-NOTES.md` §1.1.0 rather than
smoothed over — the hand-test ran on a build whose source differs from the tag, and the diff is
named there.

Two defects were found and fixed inside the phase's own close-out rather than deferred: the
release-body headline still describing 1.0.0, and the release-notes link pointing at `main`
(a branch 67 commits behind). Both now have guards; both were mutation-verified by restoring the
defective text.

**What closing does not mean:** P3.1 delivered annotation breadth, not annotation parity. The
parity matrix in `docs/P2.7-TEXT-EDITING-PARITY-PLAN.md` remains the ledger for that, and rows
still marked `Missing` stay `Missing`.

### P3.2 — CLOSED at v1.2.0

Closed 2026-11-13 on the two conditions the phase protocol requires, both recorded rather than
asserted:

| Condition | Evidence |
|---|---|
| Gate green, both shapes | plain `959 passed / 107 ignored`, `--include-ignored` `1066 passed / 0 ignored`; vitest 196 tests / 15 files; clippy clean with `-D warnings`; tsc and vite build clean |
| User hand-tested the installer | build-only dispatch `43164026953` at `b2d0b3b` → artifact `13920020055`, `DocuPro_1.2.0_x64-setup.exe` (**11,091,275 bytes**, sha-256 `989e2dfb…c2597c30`). Verdict: install and About version pass; protected-document prompt and open pass; highlight palette names and drag preview pass; signing with a `.pfx` and its password, anchor import, and the valid/unsigned badges pass; certification `P=1` refusal and `P=3` annotation permission pass; edit-in-place and search smoke pass |
| Tag | `v1.2.0` — see the tag record below |
| Published release | verified after the tag run; full release (`prerelease: false`) with the DocuPro-named installer attached |

**The one case that did not complete is the decided behaviour.** A mark covering only vector
drawing refused, wrote nothing, named the path count, kept the marks, and claimed no removal. The
user ruled it is correct as shipped (option A, 2026-11-13). It is logged above as its own future
capability with the clipping question open.

**What closing does not mean:** P3.2 delivered protect, redaction and offline signatures — not
full Acrobat security parity. Timestamping, trust services, LTV, PKCS#11/HSM and Windows-store-key
signing remain out or unbuilt, as the design's §8 states.

## 2. Optional-later (named, not scheduled)

Compare files; print pipeline + print comment summaries; PDF/A save + veraPDF
standards check; accessibility suite (autotag, checker + HTML report, reading
reflow, TTS); OS shell preview/thumbnail handlers + default-PDF manager; i18n +
Hunspell; measurement (2D) tools; attachments pane; signed auto-update channel
(**needs a separate explicit ruling** — charter carve-out).

**Overflow-edit UX (field-motivated 2026-11-02, user hand-test of 1.0.0-rc).**
The `fits-width` hard refusal is correct and stays (divergence D-2): the user
edited `that are discussed` to `that are discussed and considered` and got the
honest refusal — replacement needs 295.2 pt, available 180.0 pt, with both
widths quoted and annotation-tools suggested. The refusal behaved as designed
(`ends_a_word` correctly routed a same-word continuation away from reflow
rather than duplicating the tail). What 1.x should add on top, as designed UX:
(a) offer the alternatives in-refusal — shrink the replacement, reduce font
size for the new run, or extend onto the following line if one follows;
(b) allow the growth to span across the end-of-line boundary as a multi-line
reflow (P2.8 is line-level by frozen scope C-2, so this is a later, separately
designed capability); (c) copy-to-clipboard affordance in the refusal message
so the typed replacement can be pasted straight into an annotation note.


### Vector redaction — designed capability, not scheduled (ruled 2026-11-13, option A)

**What 1.2.0 does, and why that is the decision rather than a gap.** Applying a mark that covers
only vector drawing **refuses, writes nothing, keeps the marks, names the number of paths that
would remain, and claims no removal** (`redact.rs::count_vector_paths_under_marks`, surfaced in
`RedactPanel.tsx:197`). That is P3.2 design §8 stated as behaviour. The user hand-tested the
1.2.0 installer on 2026-11-13, hit exactly this case, and **ruled option A: 1.2.0 ships with the
honest refusal; vector redaction becomes a later phase with its own design.**

**Why it cannot be done by extending the current walk.** The text path deletes a *text-showing
operator*; an image is removed *whole* (D-RD3/D-RD7). A filled path is neither. Removing ink that
lies under a rectangle means splitting the path's geometry — and the honest question there is not
"how do I delete the operator" but **"what replaces the part of the path I remove"**. That is the
**clipping question, and it is left open here on purpose**: a clip to the complement of the mark
rectangle changes what the path paints and can change stroke joins, line caps and dash phase at
the new boundary; whether the correct answer is a clip, a geometric re-intersection, or a
rasterise-the-region fallback has not been measured, and guessing it in a plan row is exactly the
failure this programme keeps catching.

**What this row is not.** It is not a 1.2.0 defect and it is not a silent omission: the refusal is
visible, it states the count, and the release notes say so in the 1.2.0 section. It becomes a
phase when its design answers the clipping question with measurements, per the standing
stage gate.

## 3. Out of scope (standing)

Report §2.9 cloud in full; embedded document-JS engine (D-9); plugin ABI and
`.aapp`-style manifests (D-11); brokered sandbox as a parity item (D-7); rich
media/3D/Flash; XFA forms; IRM/MSRMS; Action-Wizard custom tooling (tied to the
JS story).

## 4. Per-phase protocol (binds every P3 phase)

1. Write `docs/P3.x-<slug>-DESIGN.md`; freeze its decisions table; user sign-off.
2. Implement; add fixture-level tests; mutation-verify every new guard.
3. `cargo fmt --check` + clippy deny-warnings + `cargo test -- --include-ignored`
   (PDFium available) + vitest; gate counts recorded in the commit message.
4. Version bump across all six carriers; `docs/RELEASE-NOTES.md` section written
   **before** the bump lands.
5. Installer: build-only dispatch → user hand-confirms → tag (order recorded in
   RELEASE-NOTES §0.9.0; Q9).
6. Requirements-source citations (`§` references into the report) go in the phase
   design, so the parity target stays traceable.