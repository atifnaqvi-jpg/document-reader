# P3 — Acrobat-parity gap analysis (the B-1 inventory)

Produced 2026-11-01 at the user's direction, under the B-1 amendment in
`docs/P2.7-TEXT-EDITING-PARITY-PLAN.md` (early production of the full-parity
inventory, sanctioned; it must not disturb P2.8). Basis: the tree at `1183b45`
(branch `p2-windows-test`, version 0.9.1) read against the parity requirements
source `docs/reference/acrobat/acrobat-reader-feature-extraction-report.md`
(referred to below as "the report"). Every "verified" claim below was checked
against the tree, the tag list, or the test files on 2026-11-01.

---

## 1. Where the programme stands (verified)

- **Shipped:** P0 → P2.7.6 complete, plus **P2.7.7 (Type0/CID embedding) which
  landed inside 0.9.0** — writer `dcffa91` and `da360fc`, CID-run routing ahead
  of MECH-A in `6caaedd`. `v0.9.0` is tagged on `6caaedd` and published; the
  release record lives in `docs/RELEASE-NOTES.md` §0.9.0, including the
  corrected build → hand-confirm → tag order.
- **In flight:** P2.8 (line-level reflow, target 1.0.0). Design frozen
  (`39d2b26`); reflow case (a) landed (`b260966`); refusal rows R8b/R-1a/R-1b/R-2c
  pinned; kerned-line fixtures landed (`6809678`); ink-position guards prove
  only-edited-text moves (`9471313`). Remaining: the other reflow cases and
  acceptance row A10, then the 1.0.0 closeout under the same
  build → hand-confirm → tag order.
- **Test posture:** 612 Rust tests plus the vitest suite; 70 of the Rust tests
  are PDFium-gated `#[ignore]`s that only run with a real library (Windows CI
  runs them via `--include-ignored`; the Linux job asserts the exact ignored
  count). Guards cover the CI itself, the docs citations, the fixture tree, and
  the release-version carriers.
- **Open ledger (small):** page **insert** is missing and `pages::merge` is dead
  code (B1b); U-3 (the page-operation leg of multi-level undo) is tracked in the
  parity plan; the Windows CI teardown crash (`STATUS_ACCESS_VIOLATION`) has two
  timestamped reproductions (`40908435884` attempt 1, `41278018858` on a
  documentation-only commit with `324c131` passing the same job twenty minutes
  earlier) — together they rule out anything code-dependent; the cause remains
  unidentified (record: AGENTS.md, `docs/RELEASE-NOTES.md` §0.9.1).
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
| Tool breadth | Partial | highlight + sticky note ship; underline geometry exists in the frontend; strikeout/squiggly/callout/textbox/ink/line/arrow/shapes/stamps are Missing → **P3.1** |
| Comment manager (list/sort/filter/statuses), reply chains | Missing | P3.1 |
| XFDF/FDF import-export | Missing | P3.1 (paired with P3.3 forms data) |
| Measurement (2D) tools | Missing | optional-later |
| Print comment summaries | Missing | needs a print pipeline; optional-later |
| Shared reviews, collab sync | Out | charter (§2.9) |

### C. Forms (report §2.3)
| Capability | State | Notes |
|---|---|---|
| AcroForm fill + save | Missing | **P3.3**; pdfium-render's form FFI needs the spike the P2 design already demanded (its §1 facts F1/F7) |
| FDF/XFDF/XML import-export | Missing | P3.3/P3.1 pair |
| Field authoring (create fields, tab/calculation order) | Missing | optional later; design spike first |
| Barcode fields (PDF417/QR) | Missing | optional later |

### D. Fill & Sign (report §2.4)
| Capability | State | Notes |
|---|---|---|
| Drawn/typed/image signature overlay | Missing | P3.3; visual-only, feasible with the existing page-object machinery |
| Auto-detect form fields | Missing | P3.3 optional row |

### E. Security (report §2.5)
| Capability | State | Notes |
|---|---|---|
| Open encrypted PDFs; explicit decrypt-a-copy with password memory | Have | `editor/protect.rs` (three-part policy), `decrypt_copy`, `PasswordStore` |
| Set password + permissions on save | Missing | **P3.2**; correctness-obligation feature |
| True redaction (mark/apply), sanitize / remove hidden info | Missing | **P3.2**; redaction is not delete+flatten and must be said so in the design; pdfium-render exposes no redaction, so the writer is our own |
| PKCS#7 sign / verify, certification badges | Missing | **P3.2**, offline mode per R3 (no TSA call, doc hash never leaves the machine) |
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
| Protect / Redact | Missing → P3.2 |
| Certificates | Missing → P3.2 (offline, R3) |
| Index/Catalog | Divergent (D-8): the library's FTS5 index serves the local scale better |
| Print Production / PDF Standards | Missing; optional-later (veraPDF) |
| Compare Files | Missing; optional-later |
| Rich Media / 3D / Flash-era | Out (report §4.2.16: lowest ROI) |
| Action Wizard / custom tools | Out (tied to the JS/plugin story) |
| Prepare Form | Missing → P3.3 |
| Stamp / Measure / JS developer / A11y wizards | Missing → P3.1 partial, rest optional-later |
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

1. **P2.8 remainder** (in flight; silent wrongness class if left half-done) → A10 → 1.0.0.
2. Capability gaps, in the user-ruled order (DECISIONS §9 + `docs/P3-PLAN.md`):
   P3.1 annotations breadth → P3.2 protect/redact + offline signatures →
   P3.3 forms → P3.4 organize completion + compress → P3.5 OCR → P3.6 view
   modes.
3. Optional-later list and the two Out-forever lists close the matrix.

No *silent wrongness* item is currently known open besides P2.8: the shipped
refusals (`rotated-text`, `run-mismatch`, `nested-xobject`, `GLYPHS_NOT_COVERABLE`,
`TYPE0_EMBED_UNIMPLEMENTED` retirements) are honest and tested, and P2.7.4's
transform work closed the mis-measurement class the stock take recorded.

## 6. What this inventory does NOT plan

Per-phase designs, dependency choices inside phases (qpdf bundling shape, OCR
language-pack set, forms FFI approach), and anything in the Out lists. Those
are each phase's own frozen-decision work, per the stage gate.