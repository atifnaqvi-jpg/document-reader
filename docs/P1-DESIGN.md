# P1 Design — Read PDF (v1.0)

> **Status:** Frozen for implementation. Derived from `docs/DECISIONS.md` (v1.0),
> `docs/ARCHITECTURE.md` §8 (P1 exit criteria) and `docs/P0-DESIGN.md`.
> Implementer: follow literally; do not re-open decided questions.

---

## 0. Phase Goal

**Exit criteria:** *Smooth reading of a 500-page PDF at stable memory.*

In plain terms: open a PDF, see real rendered pages, navigate them, zoom,
without memory growing with document size. Text extraction feeds P1s
indexer so search becomes real.

**PDF EDITING IS OUT OF SCOPE.** No page operations, no annotations, no form
filling, no text editing. `DECISIONS.md` §8 lists annotations as non-goal;
editing is a separate product surface and is not part of P1. Reading only.

---

## 1. Scope

| # | Deliverable |
|---|-------------|
| 1 | `PdfProvider::render_page` — real rasterization via PDFium |
| 2 | `PdfProvider::extract_text` — PDFium text layer, per page |
| 3 | **Fix `TextProvider::extract_text`** — currently uses `Path::new(&doc.id)` (a UUID) as a path; broken |
| 4 | **Async command surface** — rendering/extraction off the IPC thread via `spawn_blocking` |
| 5 | **LRU render cache** — byte-budgeted, keyed `(doc_id, page, scale)` |
| 6 | **PDFium provisioning** — bundle `pdfium.dll` as a Tauri resource; document dev setup |
| 7 | **Viewer UI** — image page view, page nav, zoom, loading/error states |
| 8 | **Indexer** — background text extraction into FTS5 `Search`, with progress events |
| 9 | **Tests** — committed PDF fixtures (P0 left `tests/fixtures/` empty) + render/extract/cache tests |
| 10 | **Memory assertion test** — render across a large PDF; assert bounded memory |

Out of scope: editing, annotations, EPUB, DOCX, OCR, file watching (`notify`),
installers/updater/signing, macOS/Linux targets, search UI polish (thin UI only).

---

## 2. Frozen Decisions (P1)

| ID | Decision | Rationale |
|----|----------|-----------|
| **D1** | **Render output = PNG** (via `image` crate), returned as encoded bytes. | Crisp glyphs, no JPEG ringing around text. A readers primary output must be artifact-free. WebP is a later optimisation if size matters. |
| **D2** | **Return shape = `{ bytes, width, height }`** (encoded pixel dims) from `get_document_page_render`. | Frontend can size the container without a second round trip. Additive change to the P0 command contract; TS types updated. |
| **D3** | **Render cache = in-process byte-budgeted LRU**, ~200 MB, keyed `(doc_id, page, scale)`. | A 600 DPI render is ~10× a 72 DPI one — bound by *bytes*, not entry count. Cache miss is never an error. |
| **D4** | **Pre-render neighbours (±1 page)** in a background task after each page view. | Page turns must feel instant; rendering ahead is the standard viewer pattern. |
| **D5** | **PDFium document handle cache**: one `Pdfium` instance in Tauri state; open `PdfDocument`s held in a `Mutex`-guarded map keyed by `(path, content_hash)`. **All PDFium calls run on `spawn_blocking` workers.** | Re-binding + re-loading per page render is wasteful. PDFium documents are not `Sync`; never share one across threads concurrently. |
| **D6** | **Indexing = background task per import**, extracting all pages then batch-inserting into `Search`, emitting progress events. | A 500-page index must not look like a hang. Progress via Tauri events + `AppState` key `index_status:<doc_id>`. |
| **D7** | **Text extraction granularity**: per-page on demand (`extract_text(doc, page)`) AND a whole-document pass for indexing. Same provider call in a loop. | One code path, two call sites. |

---

## 3. Command Surface Changes

| Command | P0 | P1 |
|---------|----|----|
| `get_document_page_render(doc_id, page, scale)` | sync, returns `Vec<u8>`, `Err(RenderUnavailable)` | **`async`**, returns `{ bytes, width, height }`, real PNG |
| `search(query, limit)` | works, empty index | unchanged — now returns real hits |
| *(new)* index progress | — | Tauri **event** `index://progress` `{ doc_id, done, total }` |
| *(new)* `get_app_state(key)` | set-only | **add `get_app_state`** — P0 set state but never read it back (theme/app-state restore needs it) |

All other commands unchanged. Keep every command that does PDFium or file
I/O `async` + `spawn_blocking`; pure DB calls may stay sync (they are fast).

---

## 4. Rust Changes (`src-tauri/`)

```
src/
  provider/
    pdf.rs        # render_page, extract_text; handle cache; blocking workers
    text.rs       # FIX extract_text path bug
  render/
    mod.rs        # NEW: LRU cache + byte accounting + neighbour pre-render
  index/
    mod.rs        # NEW: background extraction → FTS5, progress events
  commands/
    mod.rs        # async render/extract; get_app_state
  model/
    mod.rs        # add RenderedPage { bytes, width, height }
```

### `model/` addition

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RenderedPage {
    pub bytes: Vec<u8>,   // encoded PNG
    pub width: u32,       // pixels
    pub height: u32,
}
```

### `render/` — cache design

- `LruCache<RenderKey, Arc<RenderedPage>>`, `RenderKey = (doc_id, page, scale_bits)`.
- Byte budget enforced on insert: evict LRU until `total_bytes + new <= budget`.
- `Arc` so a returned page isnt cloned out of the cache.
- Neighbour pre-render: after serving page *n*, `spawn_blocking` renders *n-1*, *n+1* into the cache. Cancelled implicitly if the doc changes.

### `index/` — indexer design

- On `open_document` (after upsert): if `Search` has no rows for this `doc_id`, spawn a background indexing task.
- Extract page-by-page (streaming, not whole-doc in memory ideally), batch INSERT into `Search`.
- Emit `index://progress` events; write `AppState` key `index_status:<doc_id>` so status survives a view change.
- Idempotent: re-index only if `content_hash` changed or rows missing.

---

## 5. PDFium Provisioning

- `scripts/fetch-pdfium.ps1` already downloads Windows x64 PDFium **to
  `src-tauri/resources/pdfium.dll`** (dev). P1 must:
  1. Declare `resources/pdfium.dll` in `tauri.conf.json` → `bundle.resources` so
     NSIS installs ship it.
  2. Verify `candidate_paths()` resolution order picks it up in both dev and
     installed layouts (the resolution logic already exists and is tested).
  3. Document the `PDFIUM_LIBRARY_PATH` override in README for troubleshooting.
- **Pin the PDFium version** (`chromium/8066` in the script). Bump deliberately,
  re-run the PDF smoke test before shipping.

### ARM64 dev limitation (IMPORTANT)

The VM is **aarch64 Linux**. `fetch-pdfium.ps1` fetches Windows x64. Therefore:

- **PDF rendering cannot be end-to-end tested on the VM.**
- Real render verification happens in **CI on `windows-latest`** (the existing
  `windows-check` job) — extend it to build the app and run a render smoke test
  with a committed fixture and a fetched PDFium.
- Pure-Rust pieces (cache logic, indexer, `lopdf`-free code, text plumbing) are
  unit-testable on the VM. Guard render tests behind a
  `pdfium_available()` check so `cargo test` stays green where PDFium is absent.

---

## 6. Viewer UI (`src/`)

- `ViewerPane` replaces the P0 placeholder with a real page view:
  - Renders `RenderedPage.bytes` as a blob URL in an `<img>` sized by `width`/`height`.
  - **Page navigation**: prev/next, page number input, keyboard (arrows, Home/End).
  - **Zoom**: scale control (fit-width, 100%, +/-); triggers a re-render at the new scale.
  - **Loading state**: skeleton/placeholder while a render is in flight.
  - **Error state**: `PDFIUM_UNAVAILABLE` shows the remediation hint; `RENDER_UNAVAILABLE` should no longer occur.
  - **Indexing indicator**: subtle "indexing…" + progress when `index://progress` fires.
- Search box (minimal): calls existing `search`; results jump to page. No fancy UI.
- Virtualise the page list if multiple pages are shown at once; single-page view
  is acceptable for P1 and is the simpler default.
- Keep all `invoke` calls behind `api/invoke.ts`; update `api/types.ts` for
  `RenderedPage` and `getAppState`. **Types stay manually synced with `model/`.**

---

## 7. Tests (P1 definition of done)

1. **TextProvider extract_text** — regression test proving it reads the real file (currently broken).
2. **PdfProvider::render_page** — renders page 0 of a fixture to a valid PNG (guarded by `pdfium_available()`).
3. **PdfProvider::extract_text** — extracts known text from a text-layer fixture.
4. **render cache** — byte budget evicts LRU; hit/miss correctness; `Arc` sharing; concurrent access.
5. **indexer** — extracting a fixture populates `Search`; re-run is idempotent; progress values monotonic.
6. **command surface** — `get_document_page_render` returns `{bytes,width,height}`; `get_app_state` round-trips.
7. **memory assertion** — render across N pages of a large fixture, assert cache stays within budget and no unbounded growth (RSS check in CI where feasible).
8. **fixtures** — commit a small text-layer PDF and a tiny TXT; keep them under a few hundred KB.

Fixtures dir `src-tauri/tests/fixtures/` is **currently empty** — P1 fills it.

---

## 8. Dependencies

```toml
image = "0.25"   # PNG encoding
lru   = "0.12"   # render cache
```

No `lopdf` — editing is out of scope.

---

## 9. Risks

| Risk | Mitigation |
|------|-----------|
| PDFium needs ARM64 build on VM | Accept Windows-CI verification; guard render tests with `pdfium_available()`. |
| PDFium thread-safety | One document per blocking worker; never share `PdfDocument` concurrently; `Mutex`-guarded handle map. |
| Memory blow-up | Byte-budgeted LRU (§4) + memory assertion test (7). |
| Corrupt/odd PDFs | Provider error containment already exists; add a malformed-PDF fixture to the error path tests. |
| Long indexing blocks UI | Background task + progress events (§4), never on the IPC thread. |
| Sync render blocking IPC | Convert to `async` + `spawn_blocking` (D3/§3) — a P0 defect carried in. |

---

## 10. Task Order

```
P1.0  Fix TextProvider::extract_text path bug + regression test
P1.1  PDFium provisioning: bundle pdfium.dll, tauri.conf resources, verify resolution
P1.2  PdfProvider::render_page (PNG) + extract_text, tested vs fixture (pdfium-guarded)
P1.3  Document handle cache + convert render/extract commands to async/spawn_blocking
P1.4  LRU render cache (byte-budgeted) + neighbour pre-render
P1.5  Viewer UI: page canvas, nav, zoom, loading/error states
P1.6  Indexer: background extraction → FTS5 + progress events
P1.7  Minimal search UI; get_app_state; typed API updates
P1.8  Memory assertion test + CI render smoke on windows-latest
```

Each step must leave the app **building and launching** even if features are
partially stubbed.

---

*Frozen 2026-09-27. Reading only. Editing, annotations, OCR, EPUB, DOCX are not P1.*
