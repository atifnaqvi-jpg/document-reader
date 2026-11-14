# P3 — Acrobat-parity gap analysis (the B-1 inventory)

Produced 2026-11-01 at the user's direction, under the B-1 amendment in
`docs/P2.7-TEXT-EDITING-PARITY-PLAN.md` (early production of the full-parity
inventory, sanctioned; it must not disturb P2.8). Basis: the tree at `1183b45`
(branch `p2-windows-test`, version 0.9.1) read against the parity requirements
source `docs/reference/document-reader-capabilities.md`
(referred to below as "the report"). Every "verified" claim below was checked
against the tree, the tag list, or the test files on 2026-11-01.

**Revised 2026-11-14** at `8e84703` (version 1.2.1) so that the inventory stops
reporting shipped capabilities as missing: §1, §B, §E and §H were stale by three
shipped phases (P2.8, P3.1, P3.2). The revision is mechanical — state cells and the
facts they rest on — and changes no plan row, no ruling and no scope. Every changed
cell is grounded in the tree or a tag rather than in recollection; where a claim was
already true it is left alone. Sections still dated 2026-11-01 have not been
re-verified since.

---

## 1. Where the programme stands (verified)

- **Shipped:** P0 → P2.7.6, **P2.7.7** (Type0/CID embedding) inside 0.9.0, **P2.8** (line-level
  reflow) closed at `v1.0.0`, **P3.1** annotations breadth at `v1.1.0`, and **P3.2** protect,
  redaction and offline signatures at `v1.2.0`. The P2.7.7 record is `docs/RELEASE-NOTES.md`
  §0.9.0, including the corrected build → hand-confirm → tag order.
- **In flight:** nothing. **P3.3 forms** is the next phase and has a design
  (`docs/P3.3-forms-DESIGN.md`) awaiting sign-off; no code is written for it. The phase order
  after it is P3.3a, P3.4, P3.5, P3.6 (`docs/P3-PLAN.md` §1).
- **Test posture:** **964** plain / **107** ignored, and **1071** with `--include-ignored`
  (measured 2026-11-14; the Linux job pins 107 ignored, the Windows job asserts 0 after
  `--include-ignored`). Plus 196 vitest tests in 15 files. Guards cover the CI itself, the docs
  citations, the fixture tree, and the release-version carriers.
- **Open ledger (small):** page **insert** is missing and `pages::merge` is dead code (B1b); U-3
  (the page-operation leg of multi-level undo) is tracked in the
  parity plan; the Windows `STATUS_ACCESS_VIOLATION` has two timestamped reproductions
  (`40908435884` attempt 1, `41278018858` on a documentation-only commit with `324c131` passing
  the same job twenty minutes earlier) — together they rule out anything code-dependent. The
  cause remains unidentified (record: AGENTS.md, `docs/RELEASE-NOTES.md` §0.9.1). **A third
  candidate was examined on 2026-11-14 and is *not* added to that record:** release run
  `43075237167` failed attempt 1 and passed attempt 2 on the same SHA `27138bc`, which looks like
  the same signature, but the failing job is **"Build Windows installer"** — the packaging
  workflow, not the Rust test suite — so it is a separate intermittent and is recorded separately
  rather than folded in. **Also distinct from the Windows CI *hang* diagnosed and fixed on
  2026-11-14** (`c71a270`): a global `Pdfium` mutex taken twice in one process, a different defect
  in a different binary, which does not explain the crash.
- ~~Dead helper `text_without_rise()` has zero callers (T1).~~ **Closed.** Struck
  2026-11-01 after the loop's challenge: `text_without_rise` was deleted in
  `dcffa91`, shipped inside 0.9.0. It is the third time this item resurfaced as
  outstanding after being done, so it is recorded retracted here rather than
  carried forward again.

## 2. Charter (binding)

**Local-first stands, reaffirmed by the user on 2026-11-01.** The report's
§2.9 cloud surface (IMS/NGL auth, Document Cloud, storage connectors, shared
reviews, Adobe Sign, `comments.adobe.io` sync) is **out of parity scope**. The
user's four further rulings are recorded and binding in `docs/DECISIONS.md` §9:
R2 (OCR unfrozen, on-device), R3 (signatures offline-mode; TSA out), R4
(compress via a bundled qpdf sidecar), R5 (rename to docu PRO immediately after
1.0.0).

## 3. Capability inventory vs the report's catalog

Legend: **Have** / **Partial** / **Missing** / **Divergent** (deliberate, keep
ours, listed in §4) / **Out** (charter or rejected).

### A. Viewing and reading (report §2.1)
| Capability | State | Notes |
|---|---|---|
| Single-page display, zoom, page nav, keyboard | Have | P1; `ViewerPane` |
| Fullscreen / focus mode + menu | Have | P1.9, permission-guarded |
| Continuous / facing / continuous-facing layouts | Missing | P3.6 |
| Fit page / fit width / actual size | Missing | P3.6 |
| Thumbnails pane, autoscroll, zoom floating menu | Missing | P3.6 / optional |
| Bookmarks panel | Have | backend + menu UI |
| Full-text search over the library, annotation text indexed, hit→jump | Have | FTS5; **exceeds** the report's find baseline |
| Themes (light/dark), tabbed reading mode | Have | tabs-of-documents itself is Missing (P3.6) |
| Read Out Loud (TTS), reading reflow, screen-reader support | Missing | optional-later |
| Layers (OCG) / attachments / articles / model-tree panes | Missing | attachments pane is the useful first row |
| Protected View / sandboxed render | Divergent | D-7 |
| UI languages + Hunspell spelling | Missing | English-first (A4), optional-later |

### B. Annotations and commenting (report §2.2)
| Capability | State | Notes |
|---|---|---|
| Real PDF annotations (create/update/list/delete/undo), PDFium renders them | Have | `editor/annotate.rs` rebuilds every annotation from the DB on save (P2-D3/D8) |
| Tool breadth | Partial | **P3.1 shipped 13 concrete types** (D-A1): markup `highlight`/`underline`/`strikeout`/`squiggly`; text `note`/`textbox`/`callout`/`typewriter`; drawing `ink`/`line`/`arrow`/`rect`/`oval` — the full enum is `AnnotationKind` in `src-tauri/src/model/mod.rs`, written as real PDF annotations by the `lopdf` path. `polygon`/`cloud` are **named, not scheduled** (D-A1: they need a vertex-handles geometry editor nothing else in the palette requires). `stamps` remain Missing → optional-later |
| Comment manager (list/sort/filter/statuses), reply chains | Have | **P3.1 shipped it**: `src/components/CommentPane.tsx` sorts by page/author/date/type/status and filters by status; statuses are the four `AnnotationStatus` values persisted by `db::annotations::set_status`; reply chains are `/IRT`-linked (`annotate.rs`) and indented in the pane |
| XFDF/FDF import-export | Partial | **XFDF landed in P3.1** (D-A9/D-A10, `src-tauri/src/editor/xfdf.rs`, commands `export_xfdf`/`import_xfdf`/`save_xfdf`). **FDF is not implemented and, by ruling of 2026-11-14, will not be in 1.3.0** — refused by name with XFDF named as supported (`docs/DECISIONS.md` §10 R7); reachable as a designed row if a real need appears. Form-*data* XFDF is P3.3's, distinct from this annotation XFDF |
| Measurement (2D) tools | Missing | optional-later |
| Print comment summaries | Missing | needs a print pipeline; optional-later |
| Shared reviews, collab sync | Out | charter (§2.9) |

### C. Forms (report §2.3)
| Capability | State | Notes |
|---|---|---|
| AcroForm fill + save | Missing | **P3.3**; design `docs/P3.3-forms-DESIGN.md`. The old "form FFI spike (P2 §1 facts F1/F7)" note was **measured wrong 2026-11-13** — F1/F7 are annotation-creation and true redaction, not forms; text fill works through the public API. Buttons do not: `set_checked` in pdfium-render 0.8.37 returns `Ok` and writes a string-typed `/V` with no `/AS`, so the writer is ours (ruled 2026-11-14) |
| FDF/XFDF/XML import-export | Missing | P3.3/P3.1 pair |
| Field authoring (create fields, tab/calculation order) | Missing | optional later; design spike first |
| Barcode fields (PDF417/QR) | Missing | optional later |

### D. Fill & Sign (report §2.4)
| Capability | State | Notes |
|---|---|---|
| Drawn/typed/image signature overlay | Missing | **P3.3a** — ruled 2026-11-14 to be its own phase, the one after forms (it shares no machinery with AcroForm); visual-only, feasible with the existing page-object machinery |
| Auto-detect form fields | Missing | P3.3 optional row |

### E. Security (report §2.5)
| Capability | State | Notes |
|---|---|---|
| Open encrypted PDFs; explicit decrypt-a-copy with password memory | Have | `editor/protect.rs` (three-part policy), `decrypt_copy`, `PasswordStore` |
| Set password + permissions on save | **Have** (1.2.0, P3.2; hand-tested 2026-11-13) | `editor/protect.rs`; writes a protected copy through the P2-D2 discipline and verifies it before reporting success. Reached from *File > Protect…* |
| True redaction (mark/apply), sanitize / remove hidden info | **Have** (1.2.0, P3.2; hand-tested 2026-11-13) | Content is **deleted, not covered**, with the sanitize list applied and reachability GC after it (D-RD2..D-RD7, I-RD1..I-RD7). **Vector drawing under a mark is refused**, naming the path count, and that is the decided behaviour (option A, ruled 2026-11-13), not a defect — the clipping question is left open in `docs/P3-PLAN.md` §2 |
| PKCS#7 sign / verify, certification badges | **Have** (1.2.0, P3.2; hand-tested 2026-11-13) | Offline detached CMS over a reserved `/Contents` gap appended as a second revision; PFX import, offline verify, the four-state badge (`valid`/`modified`/`unknown`/`unsigned`), CRL-based revocation that states "not checked" when it has none, and DocMDP level refusal (D-SG6/D-SG7). **Not claimed:** signing with a Windows-store private key |
| RFC 3161 timestamping, AATL/EU trust services, LTV | Out for now | R3 keeps them out until separately ruled |
| Security policies, IRM/MSRMS | Out | cloud/DRM charter |

### F. JavaScript (report §2.6)
| Capability | State | Notes |
|---|---|---|
| Embedded JS engine, host objects, debugger, batch | **Divergent — none built** | D-9: attack surface with no local-first payoff; revisit only on explicit user demand |

### G. Accessibility (report §2.7)
App-UI accessibility: partial (UX-DESIGN §9). MakeAccessible-style autotag,
checker + HTML report, tags tree, reading reflow, TTS: Missing — optional-later;
veraPDF would serve PDF/UA-style checks when standards work happens.

### H. Tools Center (report §2.8) mapped to the tree
| Acrobat tool | State |
|---|---|
| Edit PDF (in-place text) | **Have — the programme's deep capability** (P2.4…P2.7.7; six recorded divergences) |
| Organize Pages | Partial: delete/rotate/extract/split Have; **insert missing, merge dead** (B1b) → P3.4 |
| Combine Files | Missing → P3.4 (same fix as B1b) |
| Compress / Optimize | Missing → P3.4 via qpdf sidecar (R4) |
| Create/Export PDF (Office, HTML2PDF) | Missing; heavy worker work; optional-later |
| Scan & OCR | Missing → P3.5 (R2) behind `TextExtractor` |
| Protect / Redact | **Have** (1.2.0, P3.2) — one tool, two panels: *Protect…* writes a password/permission copy, *Redact* marks and applies true content removal |
| Certificates | **Have** (1.2.0, P3.2), with one gap named rather than glossed: signing from a **PFX** (import a `.pfx`/`.p12` and its password), offline verification, the four-state badge, and certification (DocMDP). The **Windows store is read and listed** (`editor/signature/winstore.rs`, `identity.rs`) and a certificate whose private key the store holds is shown as `can_sign` — but **signing with a store-held private key is not implemented**, so that certificate cannot yet be used to produce a signature. Listed, not silently hidden |
| Index/Catalog | Divergent (D-8): the library's FTS5 index serves the local scale better |
| Print Production / PDF Standards | Missing; optional-later (veraPDF) |
| Compare Files | Missing; optional-later |
| Rich Media / 3D / Flash-era | Out (report §4.2.16: lowest ROI) |
| Action Wizard / custom tools | Out (tied to the JS/plugin story) |
| Prepare Form | Missing → P3.3 |
| Stamp / Measure / JS developer / A11y wizards | **Missing** — stamps did not ship in P3.1 (D-A1's 13 types exclude them); measure and the JS developer tool are optional-later; the A11y wizards belong to the accessibility suite |
| Unified Share / Send & Track / Mobile Link | Out (charter) |

### I. Cloud services (report §2.9)
**Out, all of it** (charter). Optional charter-compatible carve-out later: a
signed auto-update channel — only on a separate explicit ruling.

### J. OS/Office integration (report §2.10)
Shell preview/thumbnail handlers, default-PDF takeover, browser extensions,
Office add-ins, Adobe-PDF-style printer port: all Missing, all optional-later
(Windows-COM heavy, no reader value at stake).

### K. Engine internals (report §6.3)
Two-tier plugin ABI, `.aapp`-style per-tool modules, brokered sandbox,
registered-name JS bridge: **not adopted** (D-7/D-9/D-11) — single-vendor local
app; the `provider/` and `editor/` seams already deliver the modularity that
matters. The report's function-level findings remain useful as background
reading only.

## 4. Deliberate divergences (keep ours — they survive every future phase)

Extends the stock-take's list (D-1…D-6 in `docs/P2.7-STOCK-TAKE.md` §3):

| # | Ours | Adobe | Why ours stays |
|---|---|---|---|
| D-7 | Single desktop process | broker + sandboxed child | local-first trust model, accepted risk (ARCHITECTURE §9); revisit only as hardening, not parity |
| D-8 | SQLite FTS5 library index | PDE/Catalog index | better fit for a 50k-doc local library; search hits resolve to page+snippet incl. annotations |
| D-9 | No embedded document-JS engine | EScript | attack surface with no local payoff; explicit user decision to omit |
| D-10 | No cloud, no accounts, no telemetry | Document Cloud + IMS/NGL | charter (user, 2026-11-01) |
| D-11 | No plugin ABI / per-tool manifests | `AX_*` exports + `.aapp` modules | single-vendor codebase; a plugin story would buy surface, not features |

## 5. Remaining gaps, ranked by the programme's own rule

The rule (stock-take §4): silent wrongness → refusals users hit → capability
gaps → polish.

1. **No silent-wrongness item is open.** P2.8 closed at `v1.0.0` (A10 hand-confirmed), so the
   "half-done reflow" class this row used to carry is retired.
2. Capability gaps, in the user-ruled order (DECISIONS §9 + `docs/P3-PLAN.md`):
   P3.1 annotations breadth ✅ → P3.2 protect/redact + offline signatures ✅ →
   **P3.3 forms** (next; design awaiting sign-off) → P3.3a Fill & Sign (inserted
   2026-11-14) → P3.4 organize completion + compress → P3.5 OCR → P3.6 view modes.
3. Optional-later list and the two Out-forever lists close the matrix.

The shipped refusals (`rotated-text`, `run-mismatch`, `nested-xobject`, `GLYPHS_NOT_COVERABLE`,
`TYPE0_EMBED_UNIMPLEMENTED` retirements) are honest and tested, and P2.7.4's
transform work closed the mis-measurement class the stock take recorded. The one
open defect is the intermittent Windows `STATUS_ACCESS_VIOLATION` in §1, which is a
crash rather than a silent wrong answer and does not invalidate any shipped claim.

## 6. What this inventory does NOT plan

Per-phase designs, dependency choices inside phases (qpdf bundling shape, OCR
language-pack set, forms FFI approach), and anything in the Out lists. Those
are each phase's own frozen-decision work, per the stage gate.