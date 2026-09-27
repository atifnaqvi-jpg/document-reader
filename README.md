# Document Reader

A local-first personal library reader for desktop. Open, read, and search
your own documents — nothing leaves your machine.

- 📐 [Architecture design (draft)](docs/ARCHITECTURE.md) — components, modules, roadmap.
- ✅ [Frozen decisions (v1.0)](docs/DECISIONS.md) — resolves the architecture open questions. **Read this first.**

## Privacy

Documents are read from your local disk and never transmitted anywhere.
There is no server, no account, and no telemetry in v1.

## v1 Scope

- **Target:** Windows desktop. macOS/Linux later.
- **Reads:** PDF, TXT, Markdown.
- **Includes:** dark mode, full-text search over your library, bookmarks.
- **Not included:** OCR / scanned PDFs, EPUB, DOCX, annotations, sync.

## Stack

Tauri 2 + Rust backend (native PDFium rendering, SQLite + FTS5) with a
React + TypeScript frontend.

## Status

**P0 skeleton implemented.** The app shell runs: open a file, it appears in your
library with its metadata. Page rendering, text extraction and search indexing
are P1 work — the seams for them are already in place.

- ✅ P0 — Skeleton: app shell, provider seam, SQLite schema, full command surface.
- ⏭️ P1 — Read PDF: PDFium rendering, page navigation, zoom, text extraction.

## Build

Requirements: **Rust stable**, **Node 22+**, and on Linux the Tauri system
dependencies (`libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev
librsvg2-dev libsoup-3.0-dev`).

```bash
npm install
npm run tauri dev      # run the app
npm run tauri build    # produce the Windows installer (NSIS)
```

### PDFium (needed for PDF metadata, and for rendering in P1)

The app binds PDFium at runtime. Fetch it once on Windows:

```powershell
pwsh ./scripts/fetch-pdfium.ps1
$env:PDFIUM_LIBRARY_PATH = "$PWD/src-tauri/resources/pdfium.dll"
```

If PDFium is absent, opening a PDF reports a clear `PDFIUM_UNAVAILABLE` error;
plain text and Markdown files work regardless.

## Tests

```bash
cd src-tauri && cargo test          # 48 tests: schema, FTS5, search, library, providers
cd src-tauri && cargo clippy --all-targets -- -D warnings
npm run build                       # typecheck + frontend bundle
```
