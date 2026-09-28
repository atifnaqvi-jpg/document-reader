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
```

### Running the app

```bash
npm run tauri dev      # desktop app, hot reload on both sides
npm run tauri build    # Windows installer (NSIS) in src-tauri/target/release/bundle/
```

`tauri dev` starts the Vite server and the Rust binary together. The first run
compiles the full Tauri dependency tree (~5 min); later runs are incremental.

**UI-only iteration**: `npm run dev` serves the frontend at
http://localhost:1420 in a normal browser. The UI renders and the error banner
works, but every `invoke()` fails — there is no Rust backend, so `list_library`
surfaces `UNKNOWN`. Useful for styling; not for anything touching data.

**This app cannot run on a headless machine.** It is a native desktop GUI, and
Tauri panics at startup with `Failed to initialize GTK` when there is no display.
On a Linux box without a desktop session, prefix with `xvfb-run -a`, or work on
Windows (the v1 target). This applies to the dev sandbox used to build P0.

### Windows installer

```bash
npm run tauri build
```

The NSIS installer lands in `src-tauri/target/release/bundle/nsis/`. It installs
**per-user** (no admin rights), bundles PDFium so PDF support works with no setup,
and downloads the WebView2 runtime only if the machine lacks it.

**Build the installer on Windows.** It cannot be cross-compiled from Linux/macOS
in practice: the WebView2 and MSVC pieces are Windows-only, and Tauri's Windows
bundler is not usable on a Linux host. `npm run tauri build` therefore only
produces a Windows installer when run on Windows.

Two supported routes:

1. **Locally on Windows** — install Rust + Node, then `pwsh ./scripts/fetch-pdfium.ps1`
   followed by `npm run tauri build`.
2. **GitHub Actions** (`.github/workflows/release-windows.yml`) — tag a commit and
   the installer is built on a real Windows runner and attached to the release:

   ```bash
   git tag v0.1.0
   git push origin v0.1.0
   ```

   Or run the workflow manually from the Actions tab to get the installer as a
   downloadable artifact without publishing a release.

### PDFium

PDFium is a ~4 MB native library bound at runtime, so it is **fetched rather than
committed**. Fetch it once on Windows:

```powershell
pwsh ./scripts/fetch-pdfium.ps1
$env:PDFIUM_LIBRARY_PATH = "$PWD/src-tauri/resources/pdfium.dll"   # for `tauri dev`
```

The installed app finds it automatically — it is bundled beside the executable, and
the loader also probes a `resources/` subdirectory. If PDFium is missing entirely,
opening a PDF reports a clear `PDFIUM_UNAVAILABLE` error; text and Markdown files
work regardless.

**Resolution order** (first hit wins):

1. `PDFIUM_LIBRARY_PATH`, if set. A value that does not point at a real file is
   reported as an error rather than silently ignored — a typo here is worth
   surfacing.
2. `pdfium.dll` next to the executable — where the NSIS installer places it.
3. `resources/pdfium.dll` next to the executable or a close ancestor directory —
   covers `tauri dev`, which runs from `target/<profile>/`.

**Troubleshooting.** If a PDF reports `PDFIUM_UNAVAILABLE`:

- Confirm the DLL is actually present: `Test-Path "$env:LOCALAPPDATA\Programs\Document Reader\pdfium.dll"`.
- Check architecture: the fetch script downloads **Windows x64**. An ARM64 build
  cannot load it.
- Set `PDFIUM_LIBRARY_PATH` to an absolute path to bypass resolution entirely.
- The error message names the path that was tried and why it failed.

## Tests

```bash
cd src-tauri && cargo test          # 48 tests: schema, FTS5, search, library, providers
cd src-tauri && cargo clippy --all-targets -- -D warnings
npm run build                       # typecheck + frontend bundle
```
