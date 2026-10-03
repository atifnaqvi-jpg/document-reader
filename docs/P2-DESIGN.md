# P2 Design — PDF Editing (v0.1)

Status: **design, frozen pending implementation**. Companion to P1.9-DESIGN.md.
Decisions here **amend** DECISIONS.md v1.0; they do not re-litigate frozen
constraints (Tauri 2 + Rust, local-first, PDFium, SQLite, Windows-only v1).

## 0. Phase Goal

Editing works on **the copy, never the original** (D2). The reader already
shows annotations (PDFium draws them). P2 adds: create/remove them, page
tools, export, watermarks. One seam, one DB, no new persistence model.

**Explicitly out of scope:** form-filling (spike needed), true redaction
(pdfium-render does not expose it), OCR/EPUB (frozen non-goals), cloud sync
of any kind (frozen constraint).

## 1. Verified capability facts (the ground the design stands on)

Checked against `pdfium-render 0.8.37` and `lopdf 0.45.0` crate sources.

| # | Fact | Consequence |
|---|------|-------------|
| F1 | `PdfPageAnnotations::create_annotation` is `pub(crate)`; per-type constructors are `pub(crate)`. Raw FFI `FPDFPage_CreateAnnot` exists but no public path reaches it. | **pdfium-render cannot create annotations from external code.** |
| F2 | `PdfDocument::save_to_bytes / save_to_writer / save_to_blob / save_to_file` are all public. | Saving copies is cheap with the current dep. |
| F3 | `PdfPages` exposes `create_page_at_*`, `delete_page_at_index/range`, `copy_page(s)_from_document`, `copy_page_range_from_document`, `append`, `tile_into_new_document`. | **Delete/rotate(see F5)/extract/split/merge/reorder are all doable now.** |
| F4 | Page objects are publicly creatable: `PdfPagePathObject::new/new_rect/new_circle/...`, `PdfPageTextObject::new`, `PdfPageImageObject::new_from_jpeg_file`; `objects.insert_object_at_index` public. `PdfPages::watermark` public. | Visual stamping (rect highlight, text note, watermark) is possible **now** — but stamps are baked into the content stream: permanent, unremovable, not real annots. |
| F5 | Rotation: `PdfPage::set_rotation` exists; `watermark` helper is public. | Rotate + watermark via current dep. |
| F6 | Save is a **full rewrite** (no incremental API). | Every edit writes a whole new file → temp-file-then-rename discipline (D2). |
| F7 | True redaction is **not exposed** (`PdfPageRedactedAnnotation` is a stub; `FPDFPage_Redact*` unwrapped). | Out of scope for P2 (listed in §5 Open). |
| F8 | Test fixture `tests/fixtures/sample.pdf` is a real PDF; all render ops route through `DocumentHandles::with_document` under `PdfiumGate`. | New operations must follow the same discipline (concurrent commands = crash risk). |

## 2. Frozen Decisions (P2)

| ID | Decision | Rationale |
|----|----------|-----------|
| **D1** | **One editing seam** — `editor::DocumentEditor` — every Tauri command delegates to it; no editing logic in commands. | Same pattern as `provider/` seam (DECISIONS §5), keeps handlers thin & testable. |
| **D2** | **Never overwrite the user's file from an edit action.** All saves go to `"{stem} (edited {N}).pdf"` alongside the original; N is monotonic per document. "Overwrite original" must be requested explicitly and confirmed. | Preserves user trust on save-heavy workflows; protects against write-races/open-file handles (file could be open in another app). |
| **D3** | Annotations are **PDF annotations** (lopdf write path), never content stamps saved to disk. Visual stamping may be used only as live rendering of an annotation *already in the model*. | F1+F4: content stamps are permanent, unremovable, unsearchable. Annotation objects are removable & selectable. |
| **D4** | DB table `Annotation` (UUID id, doc + page + created/modified + a schema-versioned JSON blob). Data model is JSON-first, one row per annotation. | Schema-versioned JSON evolves without migrations each time a tool is added; UUIDs and timestamps follow DECISIONS §3 rules. |
| **D5** | **Annotation content is searchable**: on write, text of free-text/sticky annotations is pushed to FTS5; on delete, removed. Search hits resolve to (page + snippet) exactly like text hits. D5c: jump-to-page on click. | Annotations are content too; readers expect to search them (D5b in earlier draft; collapsed here for clarity). |
| **D6** | Edit operations **invalidate the render cache by path+page**, not by hash. | P1 lesson: the same path's hash changes under it; only path guarantees we touch the right page. |
| **D7** | **Undo/redo, per document, session-scoped** (not persisted), with a **coalescing** gesture-based push + a **memory cap** (drop oldest when exceeding). | Session-only avoids re-hydrating big stacks on launch; cap keeps memory bounded. |
| **D8** | **The viewer must show annotations**. PDFium already renders them; render cache must be invalidated when they change (D6). | Correct display after edits is the core requirement of an editor. |
| **D9** | **Original vs annotated view toggle** in View menu; default = annotated. "Original" suppresses the annotation layer **at the render level** (render option), not by DOM hiding. | DOM hiding misleads: stale pixels or ghost selection targets. |
| **D10** | **Per-tool style persistence** in AppState (`annotation.style.<tool>` keys): last color/opacity/size reused next session. | Same pattern as theme persistence; re-picking color per session is friction. |
| **D11** | **Annotation change events** (`annotation://changed`, debounced 200ms) for SearchPane/other listeners. | Cross-surface sync; the event is not the source of truth, DB is. |
| **D12** | **Edit store GC:** when the last annotation for a document is deleted AND the doc has never been saved-as, the edit copy can be GC'd. | Edit copies outlive their usefulness; GC keeps the library tidy. |

## 3. Data model

```sql
CREATE TABLE Annotation (
  id           TEXT PRIMARY KEY,      -- UUID
  document_id  TEXT NOT NULL REFERENCES Document(id) ON DELETE CASCADE,
  page         INTEGER NOT NULL,
  atype        TEXT NOT NULL,         -- highlight|note|ink|shape
  data         TEXT NOT NULL,         -- schema-versioned JSON blob (D4)
  created_at   INTEGER NOT NULL,
  modified_at  INTEGER NOT NULL
);
-- data blob (one annotation):
-- {"v":1,"tool":"highlight","rects":[[x0,y0,x1,y1]],"color":"#fff3b0","opacity":0.4,
--  "text":null}
-- FTS5 push: for note/free-text tools, `text` is indexed with a source tag so
-- results can be labeled "annotation".
```

## 4. Architectural shape

```
src-tauri/src/editor/
  mod.rs        DocumentEditor (D1): owns the seam. Trait `PdfEditor` +
                impl `PdfiumEditorImpl`. Each operation goes
                through DocumentHandles::with_document + PdfiumGate (F8).
  annotate.rs   Annotation CRUD ops (D3/D4/D5)
  pages.rs      rotate / delete / extract / split / merge / reorder (F3)
  export.rs     page→PNG; doc→copy with annots baked (flatten) or kept (F2)
  undo.rs       per-doc undo stack (D7): coalescing + cap
  lopdf_write.rs  annotation write path (D3): pdfium-render -> lopdf -> bytes
src/ (frontend)
  components/EditorPane.tsx   tool palette + canvas overlay for in-progress draw
  state/EditorProvider.tsx    selected tool/style; undo/redo UI state
  api/invoke.ts               typed wrappers for new commands
```

**Coordinate systems (the perennial PDF trap):** PDF native space is
origin-bottom-left, +y up, 72 dpi points. The UI works top-left, +y down,
at zoom. **One conversion layer** in `editor/geometry.rs` with a single
`page_to_pdf` / `pdf_to_page` pair is mandated; every editor op declares
which space it speaks, so no op silently mixes them.

## 5. Open Questions (not blocking design)

- **Form filling:** F8 - spike needed on `FPDF_FFLDraw`/form-init path; if it
  proves a save path, form fill becomes its own phase (P2.3). If not, P2
  ships without it and the spike result gets documented.
- **True redaction:** F7 blocked. Masked-redaction (opaque rect, text still
  in content stream) could ship as a clearly-labeled "limited" tool; left to
  a future phase rather than a dangerous half-measure in P2.
- **EPUB editing / reflow:** frozen out (DECISIONS Q4).

## 6. Risks

| Risk | Mitigation |
|------|-----------|
| Pdfium concurrent edit commands crash | All ops through `with_document` + `PdfiumGate` (F8) |
| Stale renders showing ghost annots | Invalidate by path+page (D6), tested |
| Annotations lost on lopdf write failure | Save to temp, fsync, rename atomically (D2) |
| Search index drift vs annotation truth | FTS5 push happens in the same transaction as the annotation CRUD (D5) |
| Undo stack memory blowup | Cap + drop-oldest policy (D7), tested |
| Users confused about which file is which | Versioned filenames (D2) + explicit overwrite dialog (D2) |
| Rotation vs annotation coordinates | `editor/geometry.rs` single conversion layer; op declares space |
| lopdf write path produces unreadable PDFs | Round-trip test: open-with-pdfium → assert pageCount & render smoke |

## 7. Definition of done

1. `cargo test` green: new editor tests + 147 existing, 0 skips for PDFium tests.
2. `tsc --noEmit && vite build` clean.
3. `cargo fmt --check && cargo clippy --all-targets -- -D warnings` clean.
4. Real-PDF fixture: annotate → save → re-open → annotations present and rendered.
5. Tag + Windows installer; user validates on hardware (P1.9 pattern).

## 8. Task order

1. `editor` seam + `Annotation` table + DB CRUD (D1/D4) with tests.
2. lopdf write path (D3): annotate → save-as → verify with PDFium round-trip.
3. Render cache invalidation by path+page (D6) + tests.
4. FTS5 push for annotation content (D5) + search-hit labeling + jump.
5. Frontend: EditorPane tools, overlay, undo/redo UI, style persistence.
6. Page tools + export + watermark (F3/F5).
7. Versioned saves (D2) + overwrite-original confirm dialog.
8. Full validation + Windows installer (P1.9 pattern).