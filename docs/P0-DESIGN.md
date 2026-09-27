# P0 Design — App Skeleton (v1.0)

> **Status:** Approved for implementation. Derived from `docs/DECISIONS.md` (frozen v1.0)
> and `docs/ARCHITECTURE.md`. Implementer: follow this literally; do not re-open
> decided questions. Out-of-scope items are listed in §10.

---

## 1. P0 Scope

**Exit criteria** (from DECISIONS §8 roadmap): *App launches, opens a file, shows metadata.*

In scope for P0:

1. Tauri 2 project skeleton: Rust backend (`src-tauri/`) + React/TS frontend (`src/`).
2. `ReaderProvider` trait seam (DECISIONS §5) with two providers wired in:
   `PdfProvider` and `TextProvider` (txt/md). `open()` works for both;
   `render_page`/`extract_text` are defined but return typed errors in P0 (P1 implements).
3. SQLite DB (rusqlite, bundled) with the full §3 schema — Document, Bookmark,
   FTS5 `Search`, AppState — created on startup.
4. Command surface (DECISIONS §4): **all eight commands exist and are registered**.
   Implemented in P0: `open_document`, `list_library`, `search` (empty index),
   bookmarks (add/list/remove), `set_app_state`. Deferred to P1: real work in
   `get_document_page_render`.
5. Frontend shell: Open-file dialog → `open_document` → metadata shown; library
   list from `list_library`; dark/light theme scaffolding (CSS variables).
6. CI: Linux job (fmt, clippy, cargo test, frontend build) + Windows job (compile-only).

Out of scope: rendering, text extraction, FTS5 population (indexer is P1),
file watching (`notify`), packaging installers/updater, OCR, EPUB, DOCX.

---

## 2. Backend Layout (`src-tauri/`)

```
src-tauri/
  Cargo.toml
  tauri.conf.json
  build.rs
  src/
    main.rs          # entry: calls document_reader_lib::run()
    lib.rs           # tauri::Builder, state setup, command registration
    model/
      mod.rs         # Format, DocumentMeta, Bookmark, SearchHit, AppError
    provider/
      mod.rs         # ReaderProvider trait + registry (provider_for)
      pdf.rs         # PdfProvider (pdfium-render, dynamic binding)
      text.rs        # TextProvider (txt/md)
    db/
      mod.rs         # open_db(), connection wrapper
      schema.rs      # DDL (DECISIONS §3 verbatim) + schema test
      documents.rs   # upsert/get/list
      bookmarks.rs   # add/list/remove
      search.rs      # FTS5 query
      app_state.rs   # get/set key-value
    commands/
      mod.rs         # the 8 tauri commands; thin wrappers over services
    library.rs       # import/open flow: hash, upsert, provider pick
  tests/             # integration tests (db, registry, providers)
```

Rule: `commands/` never touches rusqlite directly — they call `db::` and
`provider::` modules. Keeps the layering testable without Tauri.

### Dependencies (Cargo.toml)

```toml
[dependencies]
tauri = { version = "2", features = [] }
tauri-plugin-dialog = "2"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
rusqlite = { version = "0.32", features = ["bundled"] }   # bundled => FTS5 enabled
uuid = { version = "1", features = ["v4"] }
thiserror = "2"
sha2 = "0.10"        # amendment: content_hash per DECISIONS §3
hex = "0.4"
pdfium-render = "0.8"

[dev-dependencies]
tempfile = "3"
```

`sha2`/`hex` are an **approved amendment** to the DECISIONS §7 crate list — the
schema requires `content_hash` and nothing else in the list can compute it.

**PDFium binding note (design constraint for P1):** `pdfium-render` binds to a
PDFium shared library at runtime; the crate does not vendor the binary. P0
defines the binding strategy: `Pdfium::bind_to_library(pdfium_path)` where the
path comes from (1) `PDFIUM_LIBRARY_PATH` env var, else (2) bundled
`resources/pdfium.dll` next to the app. `scripts/fetch-pdfium.ps1` (Windows,
added in P1) downloads the official pdfium build into `resources/`. If binding
fails, `open_document` returns `AppError::PdfiumUnavailable` with a clear
message — never a panic. P0 compile does not require the binary (dynamic
binding resolves at runtime); CI therefore works without it.

---

## 3. Core Types & Error Surface (`model/`)

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Format { Pdf, Txt, Md }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentMeta {
    pub id: String,            // UUID v4
    pub title: Option<String>,
    pub author: Option<String>,
    pub pages: Option<u32>,
    pub format: Format,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bookmark {
    pub id: String,
    pub document_id: String,
    pub page: Option<u32>,
    pub offset: Option<u32>,
    pub label: Option<String>,
    pub created_at: i64,       // unix seconds
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchHit { pub document_id: String, pub page: u32, pub snippet: String }

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("file not found: {0}")]            FileNotFound(String),
    #[error("unsupported format: {0}")]        UnsupportedFormat(String),
    #[error("pdfium library unavailable: {0}")] PdfiumUnavailable(String),
    #[error("feature available in a later phase: render")] RenderUnavailable,
    #[error("database error: {0}")]            Db(#[from] rusqlite::Error),
    #[error("{0}")]                            Other(String),
}
// impl serde::Serialize for AppError => { "code": variant name, "message": Display }
```

Frontend receives every error as `{ code, message }`; `code` is stable and
matchable in TS.

---

## 4. Provider Seam (`provider/`)

Exactly the DECISIONS §5 contract (only `Result` = `Result<_, AppError>`):

```rust
pub trait ReaderProvider: Send + Sync {
    fn open(&self, path: &Path) -> Result<DocumentMeta, AppError>;
    fn render_page(&self, doc: &DocumentMeta, page: u32, scale: f32)
        -> Result<Vec<u8>, AppError>;
    fn extract_text(&self, doc: &DocumentMeta, page: u32) -> Result<String, AppError>;
}

pub fn provider_for(path: &Path) -> Result<Arc<dyn ReaderProvider>, AppError> {
    match path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()) {
        Some(ext) if ext == "pdf" => Ok(Arc::new(PdfProvider::default())),
        Some(ext) if ext == "txt" || ext == "md" => Ok(Arc::new(TextProvider)),
        Some(ext) => Err(AppError::UnsupportedFormat(ext)),
        None => Err(AppError::UnsupportedFormat("<none>".into())),
    }
}
```

P0 behavior:

- `TextProvider::open` — file must exist and be valid UTF-8 (lossy-read guard:
  reject on invalid UTF-8 with `UnsupportedFormat`-quality error message
  "not valid UTF-8 text"); `title` = file stem; `pages = None`; author = None.
- `PdfProvider::open` — bind pdfium per §2 strategy, open doc, read page count
  and metadata (title/author) via pdfium `Document`; `pages = Some(count)`.
- Both providers: `render_page`/`extract_text` return `Err(AppError::RenderUnavailable)`
  in P0. P1 replaces these bodies; the trait and signatures are final.

---

## 5. Database (`db/`)

Single connection opened at app start, wrapped in `Mutex<Connection>`, managed
as Tauri state. Location: `app_data_dir()` via Tauri's path resolver →
`library.db`. DB file created + schema applied idempotently (`CREATE TABLE IF
NOT EXISTS`, `CREATE VIRTUAL TABLE IF NOT EXISTS ... USING fts5(...)`).

DDL is DECISIONS §3 **verbatim** (column names, types, `missing` default,
FK `ON DELETE CASCADE`). One deviation, required by SQLite: FTS5 table plus a
`Search_content` population trigger is **not** added in P0 (indexer is P1);
the virtual table exists so `search()` can run against it (returns empty).

Key operations:

- `documents::upsert_from_import(path, meta, content_hash) -> DocumentMeta` —
  dedupe rule: if a row with same `path` OR same `content_hash` exists and
  file unchanged, update `last_opened_at` and return the existing id;
  otherwise insert new UUID row.
- `documents::list_missing_guard()` — on `list_library`, each row checks file
  existence once; flips `missing = 0/1` accordingly (cheap P0 substitute for
  `notify` watching, which arrives later per DECISIONS §6).
- `search::query(q, limit)` — `SELECT document_id, page, snippet(Search, 2, '[', ']', '…') FROM Search WHERE Search MATCH ? LIMIT ?`; on empty index returns `[]`.
  FTS5 MATCH syntax errors from user input are caught → empty result + no error.

**Schema test** (P0 gate): in-memory + temp-file DB, create schema, assert
FTS5 virtual table is queryable (`SELECT count(*) FROM Search`), assert FK
cascade on bookmarks works. This test also *proves* the bundled feature ships
FTS5; if it fails at build time, the bundled-feature assumption is wrong and
must be fixed in P0, not discovered in P1.

---

## 6. Command Surface (`commands/`) — DECISIONS §4 mapping

| Command | P0 behavior |
|---------|-------------|
| `open_document(path) -> DocumentMeta` | provider_for → `open()` → hash file → `upsert_from_import` → meta (includes id). Missing file → `FileNotFound`. |
| `list_library() -> Vec<DocumentMeta>` | DB rows (missing files excluded from results but flagged via `missing` in a companion field — frontend shows "file moved/deleted" badge) |
| `search(query, limit) -> Vec<SearchHit>` | FTS5 query, empty index → `[]` |
| `add_bookmark(doc_id, page, offset, label) -> Bookmark` | FK-validated insert; unknown doc → `Other("document not found")` |
| `list_bookmarks(doc_id) -> Vec<Bookmark>` | ordered by `created_at` |
| `remove_bookmark(bookmark_id)` | delete; unknown id → `Other("bookmark not found")` |
| `set_app_state(key, value)` | UPSERT into AppState |
| `get_document_page_render(doc_id, page, scale)` | **P0: `Err(RenderUnavailable)`**; P1 swaps in pdfium render |

All commands are sync (`#[tauri::command]` without async) in P0 — file open and
hashing on single files is fast enough; P1 will move render to `spawn_blocking`.
State access via `tauri::State<'_, AppState>` holding `Mutex<Connection>`.

Dialog: `tauri-plugin-dialog` (Rust crate + `@tauri-apps/plugin-dialog` npm)
provides the native open dialog; frontend picks file then invokes
`open_document`.

---

## 7. Frontend (`src/`) — P0 shell

```
src/
  main.tsx, App.tsx
  api/
    types.ts      # TS mirrors of DocumentMeta, Bookmark, SearchHit, AppError
    invoke.ts     # typed wrappers: openDocument(path), listLibrary(), ...
  components/
    LibraryPane.tsx   # list from listLibrary(); missing badge
    ViewerPane.tsx    # P0: shows metadata (title, author, pages, format);
                      # placeholder message where page canvas will live (P1)
    Toolbar.tsx       # Open… button (dialog), theme toggle
  theme/
    ThemeProvider.tsx # dark/light via CSS variables; persisted through set_app_state
    tokens.css
```

Behavior:
- On mount: `listLibrary()` renders the library pane.
- Toolbar "Open…" → native dialog (plugin) → `openDocument(path)` → metadata
  appears in ViewerPane; document is added to library.
- Bookmark/search UI is **not** built in P0 (commands exist and are tested in
  Rust; UI lands P2). No dead UI stubs.
- Error display: any `invoke` rejection surfaces `{code, message}` in a toast;
  `PDFIUM_UNAVAILABLE` shows the §2 remediation hint.
- Types in `api/types.ts` must be kept in sync manually with `model/` (serde
  rename_all = camelCase for structs to match TS conventions — set
  `#[serde(rename_all = "camelCase")]` on all serializable models).

---

## 8. Tests (P0 definition of done, all `cargo test`)

1. `db::schema` — creates schema in temp file DB; FTS5 table queryable; FK cascade works.
2. `db::documents` — upsert dedupe by content_hash; list flips `missing` on a deleted fixture.
3. `db::bookmarks` — add/list/remove round-trip; cascade delete with document.
4. `db::search` — empty-index query returns `[]`; malformed MATCH string doesn't 500.
5. `provider::registry` — extension mapping (.pdf/.txt/.md/unknown/no-ext).
6. `provider::text` — opens a UTF-8 fixture; rejects invalid UTF-8.
7. `library` — open flow inserts a row and returns meta with id; second open of same file returns same id.
8. Frontend: no test framework in P0 (no logic worth testing yet); vitest arrives with P2 UI work.

Fixtures: tiny sample `.txt` (valid + invalid UTF-8) under `src-tauri/tests/fixtures/`. PDF open path needs pdfium at runtime → covered by manual smoke test on Windows dev machine (documented in README), not CI.

---

## 9. CI (`.github/workflows/ci.yml`)

- **linux job**: rust-toolchain stable; `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test`, `npm ci && npm run build` (frontend only, no tauri build — Linux tauri needs webkit2gtk system deps; not worth it for P0).
- **windows job**: `cargo check --target x86_64-pc-windows-msvc` (compile-only; validates the actual shipping target; unsigned, no packaging).
- Trigger: push + PR to main.

---

## 10. Explicit Non-Goals for P0 (do not implement)

Rendering, text extraction, FTS5 population/indexer, `notify` file watching,
installers/updater/code signing, OCR/EPUB/DOCX, bookmark/search **UI**,
multi-window, settings UI beyond theme toggle, macOS/Linux targets.

---

*Design complete. Implementation proceeds per the task list: frontend scaffold
and backend scaffold, then CI, then the §8 test gate.*