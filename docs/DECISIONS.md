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
