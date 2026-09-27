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

Decisions frozen (see `docs/DECISIONS.md`). Next step: scaffold the P0
app skeleton per `docs/DECISIONS.md` and `docs/ARCHITECTURE.md`.
