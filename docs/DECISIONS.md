# Document Reader — Frozen Decisions (v1.0)

> **Status:** FROZEN. These supersede §7 (Open Questions) and §1 (Assumptions)
> and §5 (Technology Options) in `docs/ARCHITECTURE.md`.
> **Product:** Local-first personal library document reader.

---

## 0. Product Definition

A **local-first personal library reader** for desktop.

- No accounts. No server. No cloud. No sync in v1.
- Documents never leave the device. State this in the README.
- "Scalability" here means *local* scale: a 50k-document library,
  huge PDFs, fast search, no server.

---

## 1. Answers to the Section 7 Questions

| # | Question | ANSWER |
|---|----------|--------|
| Q1 | Stack: Tauri+Rust or Electron+TS? | **Tauri 2 + Rust** backend, **React + TypeScript** frontend. PDF rendering via native **PDFium** (`pdfium-render` crate), NOT the OS webview PDF viewer. |
| Q2 | Platforms at launch? | **Windows-only for v1.** macOS/Linux are additive later; defer signing/notarization. |
| Q3 | Scanned PDFs / OCR in scope? | **OUT of scope for v1.** `TextExtractor` trait with a PDFium text-layer impl; OCR is a future impl slot behind the same trait. Detect image-only PDFs and surface \"no text layer — search unavailable\" rather than failing silently. |
| Q4 | Formats beyond PDF? | **PDF + TXT + MD at launch.** EPUB in v1.1 (design the reflow seam now, do not build). DOCX = text-extraction-only, later, explicitly labeled non-faithful. No MOBI/AZW. |
| Q5 | Nice-to-haves for v1? | **SHIP: dark mode, full-text search, minimal bookmarks.** DEFER: highlights/annotations/notes, library/collections, multi-document tabs, sync. |

### Rationale, briefly

- **Q1:** installers ~10 MB vs Electron 80–150 MB; far lower idle memory.
  For an app people install casually and leave open, footprint is the
  decisive product quality. Rust is also the natural home for the hard
  parts: PDFium rendering, text extraction, FTS5 indexing. Frontend stays
  React/TS, so no loss of web tooling. Rendering in native PDFium, not the
  webview, neutralises the main Tauri risk (OS webview inconsistency).
- **Q2:** Windows dominates the consumer/SMB desktop install base. Each
  extra OS multiplies CI, code-signing, and packaging work. Tauri keeps
  macOS/Linux cheap to add later; Windows-only v1 costs almost no
  optionality.
- **Q3:** OCR is a separate subsystem (models, language packs, preprocessing,
  accuracy QA) and it changes search to fuzzy/confidence handling. Not
  v1. Interface seam keeps it additive.
- **Q4:** PDF is mandatory; TXT/MD are near-free. EPUB is the natural third
  but needs a reflow/pagination path distinct from fixed-layout PDF — v1.1.
  DOCX layout fidelity is effectively unreproducible without an Office-class
  layout engine; text extraction only, later, clearly labeled.
- **Q5:** dark mode is cheap and high-perceived-quality. Full-text search is
  a *readers* core value and shares the extraction work you already need.
  Bookmarks are a trivial data model. Highlights/annotations are the heavy
  sibling (overlay rendering, persistence, export) — v1.1.

---

## 2. Amendments to the Draft (ARCHITECTURE.md)

The draft is sound. These changes apply:

| Draft item | Amendment |
|-----------|-----------|
| §1 A5 "Runs on Windows, macOS, Linux from one codebase" | **Windows-only v1.** Codebase stays portable; shipping targets only Windows. |
| §1 A4 "no OCR in v1" | **Confirmed** (Q3). |
| §1 A2 "EPUB optional later" | **Confirmed** — EPUB is v1.1, not launch. |
| §1 A6 "no annotation, no cloud sync in v1" | **Confirmed** (Q5). |
| §3 "No database. Plain JSON settings only." | **Superseded.** Search + bookmarks require persistence: use a **single local SQLite DB** (metadata + FTS5 index + bookmarks + app state). Still no server. |
| §4 interface sketch is Python-ish | Rewrite in **Rust** types (see §3 below). Keep the same seam concept. |
| §5 recommends Tauri **or** Electron | **RATIFIED: Tauri 2 + Rust** (Q1). Electron option closed. |
| §6 D5 "Plain files, no database" | **Superseded** as above: SQLite for local state/index. |
| §8 P3 "Full-text search" listed as extensions | **Moved forward**: full-text search ships in v1 (Q5). Add to P2. |
| §9 risks | Add: filesystem mess (moved/deleted files), large-library indexing time, FTS5 index size. |

---

## 3. Domain Model (SQLite)

```sql
Document (
  id            TEXT PRIMARY KEY,   -- UUID
  path          TEXT NOT NULL,
  title         TEXT,
  author        TEXT,
  format        TEXT NOT NULL,      -- pdf|txt|md
  size          INTEGER,
  pages         INTEGER,
  added_at      INTEGER NOT NULL,
  last_opened_at INTEGER,
  content_hash  TEXT,               -- dedupe + change detection
  missing       INTEGER DEFAULT 0   -- file moved/deleted
);

Bookmark (
  id          TEXT PRIMARY KEY,     -- UUID
  document_id TEXT NOT NULL REFERENCES Document(id) ON DELETE CASCADE,
  page        INTEGER,
  offset      INTEGER,
  label       TEXT,
  created_at  INTEGER NOT NULL
);

-- FTS5 index over extracted text
CREATE VIRTUAL TABLE Search USING fts5(
  document_id UNINDEXED,
  page UNINDEXED,
  content
);

AppState (key TEXT PRIMARY KEY, value TEXT);
```

**Rules**
- UUIDs, not autoincrement ints — makes future multi-device sync additive.
- Include `updated_at`/timestamps throughout for the same reason.
- `content_hash` avoids needless re-indexing.
- Files move and vanish: mark `missing = 1`, never crash.

---

## 4. Rust ↔ React Command Surface (Tauri commands)

```
open_document(path)                 -> DocumentMeta
list_library()                      -> DocumentMeta[]
search(query, limit)                -> SearchHit[]   // doc_id, page, snippet
add_bookmark(doc_id, page, offset, label) -> Bookmark
list_bookmarks(doc_id)              -> Bookmark[]
remove_bookmark(bookmark_id)
set_app_state(key, value)
get_document_page_render(doc_id, page, scale) -> image bytes
```

Rendering goes through **PDFium in Rust** and is handed to React as an
image; do **not** rely on the webview PDF viewer.

---

## 5. Core Interfaces (Rust)

```rust
pub struct DocumentMeta {
    pub id: String,
    pub title: Option<String>,
    pub author: Option<String>,
    pub pages: Option<u32>,
    pub format: Format,
}

pub trait ReaderProvider: Send + Sync {
    fn open(&self, path: &Path) -> Result<DocumentMeta>;
    fn render_page(&self, doc: &DocumentMeta, page: u32, scale: f32)
        -> Result<Vec<u8>>;               // encoded image
    fn extract_text(&self, doc: &DocumentMeta, page: u32) -> Result<String>;
}

// OCR slots in behind this same trait later; v1 impl = PDFium text layer.
pub trait TextExtractor: Send + Sync {
    fn extract(&self, path: &Path, page: u32) -> Result<Option<String>>;
}
```

---

## 6. Storage, Search, Deploy

- **Storage:** one local SQLite DB. Documents are read **in place** from
  their original paths. A "managed library folder" is a v2 decision.
- **Search:** SQLite **FTS5**. Index lazily in a background Rust task on
  import or first open; never block the UI. Stream page-by-page for
  large PDFs.
- **File watching:** `notify` crate — detect moved/deleted files, flip
  `missing`.
- **Deploy:** signed Windows installer (MSI/NSIS) + Tauri built-in
  updater. **No backend.** Update artifacts served from static hosting
  (GitHub Releases or equivalent).

---

## 7. Repo Layout

```
/src-tauri        Rust: main.rs, commands/, pdf/, search/, db/, model/
/src              React + TS: viewer, library, search, bookmarks, theme
/package.json     Tauri + React deps
/Cargo.toml       tauri, pdfium-render, rusqlite (bundled sqlite + fts5),
                  uuid, serde, notify
/docs             ARCHITECTURE.md (draft), DECISIONS.md (this file)
README.md         Scope, privacy statement, build instructions
```

---

## 8. Explicit Non-Goals for v1

OCR; DOCX layout rendering; EPUB rendering; cloud sync; accounts;
multi-tenant anything; macOS/Linux builds; highlights/annotations.

> **SUPERSEDED (2026-10-04, user instruction) — by `docs/P2.4-DESIGN.md`:**
> text editing within a page's content stream is now an ACTIVE requirement
> (staged: constrained edit → font embedding/subsetting → reflow; see T11 and
> §0 stage gate). The annotations prohibition above was already amended by
> P2-DESIGN (real PDF annotations shipped in P2). All other non-goals STAND.

---

*Frozen 2026-09-27. The Section 7 questions are CLOSED. Scaffold against this
document; do not re-ask.*

---

## 9. Amendments (2026-11-01, user rulings — Acrobat parity programme)

> These follow the P2-DESIGN precedent: they extend the frozen decisions; they do
> not re-litigate them. Binding for all phases from P2.8 onward.

1. **Charter reaffirmed — local-first.** No accounts, no server, no cloud, no
   telemetry. Acrobat's cloud/collab surface (feature-extraction report §2.9:
   IMS/NGL auth, Document Cloud, storage connectors, shared reviews, Adobe Sign,
   comment sync) is **out of parity scope** and is not planned in any phase. The
   only considered carve-out is a signed auto-update channel, and only if later
   ruled.
2. **R2 — OCR unfrozen (amends §8 and Q3).** OCR is an active requirement for
   `docs/P3-PLAN.md` P3.5, implemented behind the existing `TextExtractor` trait
   seam, on-device (tesseract), never a network call. The detect-and-surface
   behaviour for image-only PDFs (search unavailable) keeps its current wording
   until P3.5 lands.
3. **R3 — digital signatures: offline mode ruled in, TSA mode out.** Signing and
   verifying must complete with no network call; the document hash never leaves
   the machine. RFC-3161 timestamping and AATL-equivalent trust services stay out
   until separately ruled. Redaction, sanitize, and signatures carry correctness
   obligations: named invariants up front, per the B-1 note in
   `docs/P2.7-TEXT-EDITING-PARITY-PLAN.md`.
4. **R4 — compress/optimize uses a bundled qpdf sidecar** (Apache-2.0) as an
   out-of-process worker instead of a lopdf re-implementation. The P3.4 design
   freezes the bundling and invocation shape before any code (stage gate).
5. **R5 — rename to "docu PRO" immediately after 1.0.0.** Product name and bundle
   identifier change together in one housekeeping phase. Accepted consequence, on
   record: the identifier is the Windows upgrade identity, so existing hand-test
   installs update by reinstall and both entries appear in Add/Remove —
   deliberate, while the install base is hand-test machines only.

Requirements source for the parity target: the Adobe feature/capability survey at
`docs/reference/document-reader-capabilities.md`.

---

## 10. Amendments (2026-11-14, user rulings — P3.3 forms)

> Same precedent as §9: these extend the frozen decisions and settle questions a phase design
> left open. They bind from P3.3 onward.

6. **R6 — a certified document permits form fill at every level that permits it at all, and
   `P=3` is not the threshold.** A field value is not page content, so filling a form on a
   certified document is judged by the level's own permission table, not refused as a
   content change.

   The normative basis is **ISO 32000-1 §12.8.2.2**, whose DocMDP transform parameters
   dictionary is **Table 254**. Its `/P` entry reads: `1` no changes permitted; `2` form
   filling, instantiating page templates, and signing; `3` the same as `2`, plus annotation
   creation, deletion and modification. Form filling is therefore named explicitly at
   **`P=2` as well as `P=3`** — it is not a permission `P=3` adds, and treating `P=3` as the
   only level that allows it would refuse a `P=2` document the specification says may be
   filled.

   The reasoning beyond the citation: refusal would make forms unusable on every certified
   file, and certification is the feature this product just shipped. The recoverable
   direction is not always "refuse" — here a refusal would break the ordinary use of a
   signed document while protecting nothing the certifier asked for. This settles the question
   `docs/P3.3-forms-DESIGN.md` D-F7 left open and **replaces** that decision's
   refuse-until-ruled default.

   **Consistency note, checked rather than assumed:** `src-tauri/src/editor/signature/editpolicy.rs`
   already answers this question this way. Its `permits` gives `FormFillAndSignatures` exactly
   `EditKind::FormFill`, and `AnnotationsToo` `EditKind::FormFill | EditKind::Annotation`, and
   `the_permitted_edit_matrix_matches_the_specification` pins the full matrix. The design's
   default was therefore *stricter than shipped behaviour*: implementing it as drafted would
   have introduced a refusal the product does not currently make at `P=2` or `P=3`. The
   ruling aligns the design with the code that exists instead of adding a second, quieter
   answer.

7. **R7 — XFDF only. FDF is refused, by name, and the refusal is a decision, not an
   omission.** P3.3 ships XFDF form-data import and export. FDF — the binary container — is
   not implemented, in 1.3.0 or in the export/import surface.

   The reasoning: FDF is a legacy binary container whose parser would double the format
   surface of this feature for one import path, and XFDF already covers what P3.1's
   annotation interchange established (`src-tauri/src/editor/xfdf.rs`, D-A9/D-A10). Two
   parsers that must agree is a second place for a silent divergence to live.

   The refusal is **user-facing wording, not a silent skip**: a `.fdf` input is refused with
   the format named and XFDF named as what is supported. A file a user believes may be
   imported and is quietly ignored is the "green pass wearing a label" failure this charter
   bans, in its input form.

   **This amends `docs/P3.3-forms-DESIGN.md` §2**, which had FDF "IN, but paired" with XFDF
   as one work item on the argument that a half-format is not a format. That argument loses
   to the surface-area one, and the amendment is recorded here rather than by editing the
   design's reasoning out of sight.

   **FDF may return as a designed row.** It is refused for this phase on cost, not excluded
   for principle, so if a real need appears (a counterparty's system that emits only FDF) it
   returns as its own planned capability with its own design — the same shape P3.3a took when
   Fill & Sign was split out. Ledgered as an optional-later row in `docs/P3-PLAN.md` §2.
