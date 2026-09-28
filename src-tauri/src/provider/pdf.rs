use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use pdfium_render::prelude::*;

use crate::model::{AppError, DocumentMeta, Format, LibraryEntry};

use super::ReaderProvider;

const PDFIUM_DEV_ENV_VAR: &str = "PDFIUM_LIBRARY_PATH";
/// Shipped beside the executable in release installs (docs/P0-DESIGN.md §2).
const PDFIUM_BUNDLED_FILE: &str = "pdfium.dll";

/// Clamp for the render scale. A caller asking for 0 would otherwise produce a
/// zero-pixel image; an unbounded value (600 DPI ≈ 8.3×) can allocate gigabytes.
const MIN_RENDER_SCALE: f32 = 0.1;
const MAX_RENDER_SCALE: f32 = 8.0;

/// Every place a bundled `pdfium.dll` may live, in priority order, given the
/// executable's directory. Pure so the resolution order is unit-testable.
fn candidate_paths_in(exe_dir: &Path) -> Vec<PathBuf> {
    let mut candidates = vec![exe_dir.join(PDFIUM_BUNDLED_FILE)];

    // Walk up a few levels looking for a `resources/` directory. One walk covers
    // both layouts we care about: a Tauri install (`<install>/resources/`) and a
    // dev build (`src-tauri/target/<profile>/` reaching `src-tauri/resources/`).
    // Tauri's exact resource placement has varied across versions, so we probe
    // rather than assume — a wrong guess here silently costs PDF support.
    let mut current = Some(exe_dir);
    for _ in 0..3 {
        let Some(dir) = current else { break };
        candidates.push(dir.join("resources").join(PDFIUM_BUNDLED_FILE));
        current = dir.parent();
    }

    candidates
}

fn exe_directory() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
}

/// Resolution order, first hit wins:
///   1. `PDFIUM_LIBRARY_PATH` — explicit override, for dev and troubleshooting.
///   2. `pdfium.dll` beside the executable — where NSIS puts bundled resources.
///   3. `resources/pdfium.dll` under the executable directory or a close ancestor —
///      the other layout Tauri uses, depending on version.
///
/// Pure on purpose: environment and executable paths are passed in, so the whole
/// priority order is testable without mutating process-global env state.
fn resolve_library_path(
    env_override: Option<&str>,
    exe_dir: Option<&Path>,
) -> Result<PathBuf, AppError> {
    if let Some(raw) = env_override {
        let path = PathBuf::from(raw);
        if path.is_file() {
            return Ok(path);
        }
        // An explicit override that does not resolve is a configuration mistake
        // worth reporting, not something to silently paper over by falling back.
        return Err(AppError::PdfiumUnavailable(format!(
            "{PDFIUM_DEV_ENV_VAR} points at a missing file: {}",
            path.display()
        )));
    }

    if let Some(dir) = exe_dir {
        for candidate in candidate_paths_in(dir) {
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }

    Err(AppError::PdfiumUnavailable(format!(
        "set {PDFIUM_DEV_ENV_VAR} or place {PDFIUM_BUNDLED_FILE} next to the executable"
    )))
}

/// Native PDFium rendering (DECISIONS Q1) — the OS webview PDF viewer is never used.
#[derive(Debug)]
pub struct PdfProvider;

/// A rasterized page: encoded PNG plus the pixel dimensions of that image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RasterizedPage {
    pub bytes: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// Clamps a caller-supplied scale into a range that always yields a usable image.
/// A non-finite or non-positive scale would otherwise produce a zero-pixel render,
/// and an unbounded one can try to allocate gigabytes.
fn clamp_scale(scale: f32) -> f32 {
    if !scale.is_finite() {
        return 1.0;
    }
    scale.clamp(MIN_RENDER_SCALE, MAX_RENDER_SCALE)
}

/// Handle-cache capacity (P1-D5). Each entry holds a loaded PDFium document, whose
/// resident cost scales with the file, not with the page being rendered. A handful
/// of recently-viewed documents is the useful working set for a reader; the bound
/// exists so a long session does not accumulate every file ever opened.
const MAX_OPEN_DOCUMENTS: usize = 8;

/// One process-wide `Pdfium` instance (P1-D5): binding the library is comparatively
/// expensive and only needs doing once.
///
/// **At most one live `Pdfium` per process.** With pdfium-render's default
/// `thread_safe` feature, constructing a `Pdfium` calls `FPDF_InitLibrary`, which
/// takes a process-global mutex that is released only by `FPDF_DestroyLibrary` — and
/// `Pdfium`'s `Drop` is the only thing that calls it. A second `Pdfium` therefore
/// blocks forever on that mutex, so every operation in this module must go through
/// this instance. Do not reintroduce `Pdfium::new(...)` at a call site.
fn pdfium() -> Result<&'static Pdfium, AppError> {
    static INSTANCE: OnceLock<Result<Pdfium, AppError>> = OnceLock::new();

    INSTANCE
        .get_or_init(|| Ok(Pdfium::new(PdfProvider::bind()?)))
        .as_ref()
        .map_err(|e| AppError::PdfiumUnavailable(e.to_string()))
}

/// A document handle handed out to a render: shared ownership of the loaded document.
pub type SharedDocument = Arc<Mutex<PdfDocument<'static>>>;

/// Open `PdfDocument` handles keyed by `(path, content_hash)` (P1-D5), so page
/// turns do not re-open and re-parse the file.
///
/// Locking discipline — this is the part that must not be got wrong:
///
/// * The `Mutex` is held only for map operations, **never** across a render. PDFium
///   rendering can take hundreds of milliseconds, and holding the map lock for that
///   long would serialise every page turn behind one render.
/// * A render therefore needs a *reference* to a document that outlives the borrow
///   of the guard. Handing out `&PdfDocument` would not compile for exactly that
///   reason, so the cache yields an `Arc<Mutex<PdfDocument>>` instead.
/// * The per-document `Mutex` is what enforces "never share one `PdfDocument` across
///   threads concurrently": two threads rendering different pages of the *same*
///   document serialise on it, which is required, because a `PdfDocument` is only
///   `Send`/`Sync` as a promise from us that access is exclusive — pdfium-render's
///   default `thread_safe` feature serialises the API boundary but the feature is
///   only sound while documents are not touched from two threads at once.
pub struct DocumentHandles {
    entries: Mutex<HashMap<String, (SharedDocument, u64)>>,
    clock: Mutex<u64>,
}

impl std::fmt::Debug for DocumentHandles {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let open = self.entries.lock().map(|e| e.len()).unwrap_or_default();
        f.debug_struct("DocumentHandles")
            .field("open_documents", &open)
            .field("capacity", &MAX_OPEN_DOCUMENTS)
            .finish()
    }
}

impl Default for DocumentHandles {
    fn default() -> Self {
        Self::new()
    }
}

/// Cache key: the same file at a different path (or with different contents) must
/// not reuse a handle. Hashing the path keeps keys uniform; `content_hash` catches
/// a file replaced in place, which would otherwise serve stale pages.
fn handle_key(path: &Path, content_hash: Option<&str>) -> String {
    format!("{}\u{1}{}", path.display(), content_hash.unwrap_or("-"))
}

impl DocumentHandles {
    pub fn new() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            clock: Mutex::new(0),
        }
    }

    fn next_tick(&self) -> u64 {
        let mut clock = self.clock.lock().unwrap_or_else(|e| e.into_inner());
        *clock += 1;
        *clock
    }

    /// Returns a shareable handle to `path`'s document, opening it on first use.
    pub fn acquire(
        &self,
        path: &Path,
        content_hash: Option<&str>,
    ) -> Result<SharedDocument, AppError> {
        PdfProvider::ensure_file(path)?;
        let key = handle_key(path, content_hash);
        let tick = self.next_tick();

        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());

        if let Some((handle, last_used)) = entries.get_mut(&key) {
            *last_used = tick;
            return Ok(Arc::clone(handle));
        }

        let document = PdfProvider::load(pdfium()?, path)?;
        let handle = Arc::new(Mutex::new(document));
        entries.insert(key, (Arc::clone(&handle), tick));

        // Evict the least-recently-used entries. Dropping the last `Arc` closes the
        // document, unless a render still holds a clone — in which case the drop
        // happens when that render finishes, which is precisely when it is safe.
        if entries.len() > MAX_OPEN_DOCUMENTS {
            let mut by_age: Vec<(String, u64)> = entries
                .iter()
                .map(|(key, (_, last_used))| (key.clone(), *last_used))
                .collect();
            by_age.sort_by_key(|(_, last_used)| *last_used);
            for (stale, _) in by_age.into_iter().take(entries.len() - MAX_OPEN_DOCUMENTS) {
                entries.remove(&stale);
            }
        }

        Ok(handle)
    }

    /// Drops a cached handle, e.g. when the file is known to have changed.
    pub fn forget(&self, path: &Path, content_hash: Option<&str>) {
        let key = handle_key(path, content_hash);
        self.entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&key);
    }

    /// Number of documents currently held open.
    pub fn open_count(&self) -> usize {
        self.entries.lock().unwrap_or_else(|e| e.into_inner()).len()
    }
}

impl PdfProvider {
    /// Renders a page through the handle cache — the path both the command surface
    /// (P1.3) and neighbour pre-render (P1.4) use.
    pub fn render_cached(
        handles: &DocumentHandles,
        path: &Path,
        content_hash: Option<&str>,
        page: u32,
        scale: f32,
    ) -> Result<RasterizedPage, AppError> {
        let handle = handles.acquire(path, content_hash)?;
        // Lock only this document. Different documents render concurrently; the same
        // document serialises, which is the required behaviour.
        let document = handle.lock().unwrap_or_else(|e| e.into_inner());
        Self::rasterize(&document, page, scale)
    }

    /// Extracts one page's text through the handle cache.
    pub fn extract_cached(
        handles: &DocumentHandles,
        path: &Path,
        content_hash: Option<&str>,
        page: u32,
    ) -> Result<String, AppError> {
        let handle = handles.acquire(path, content_hash)?;
        let document = handle.lock().unwrap_or_else(|e| e.into_inner());
        Self::page_text(&document, page)
    }

    fn ensure_file(path: &Path) -> Result<(), AppError> {
        if path.is_file() {
            Ok(())
        } else {
            Err(AppError::FileNotFound(path.display().to_string()))
        }
    }

    /// Loads a document from disk. Kept as one place so the handle cache (P1.3) can
    /// reuse exactly the same open path.
    fn load<'a>(pdfium: &'a Pdfium, path: &Path) -> Result<PdfDocument<'a>, AppError> {
        pdfium
            .load_pdf_from_file(&path.display().to_string(), None)
            .map_err(|e| AppError::Other(format!("failed to open PDF: {e}")))
    }

    /// Rasterizes one page to PNG, with the bitmap's pixel dimensions.
    fn rasterize(
        document: &PdfDocument<'_>,
        page: u32,
        scale: f32,
    ) -> Result<RasterizedPage, AppError> {
        let page = document
            .pages()
            .get(page as u16)
            .map_err(|e| AppError::Other(format!("page {page} unavailable: {e}")))?;

        let config = PdfRenderConfig::new().scale_page_by_factor(clamp_scale(scale));
        let bitmap = page
            .render_with_config(&config)
            .map_err(|e| AppError::Other(format!("render failed: {e}")))?;

        let width = bitmap.width() as u32;
        let height = bitmap.height() as u32;

        // `as_image()` copies out of PDFium's own buffer, so the encoded result
        // stays valid after the bitmap and document are dropped.
        let mut png = Vec::new();
        bitmap
            .as_image()
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .map_err(|e| AppError::Other(format!("PNG encode failed: {e}")))?;

        Ok(RasterizedPage {
            bytes: png,
            width,
            height,
        })
    }

    /// Extracts one page's text layer. An empty string means the page simply has no
    /// text layer (a scanned image) — a valid answer, not an error, per DECISIONS Q3.
    fn page_text(document: &PdfDocument<'_>, page: u32) -> Result<String, AppError> {
        let page = document
            .pages()
            .get(page as u16)
            .map_err(|e| AppError::Other(format!("page {page} unavailable: {e}")))?;

        let text = page
            .text()
            .map_err(|e| AppError::Other(format!("text extraction failed: {e}")))?;

        Ok(text.all().trim_end().to_string())
    }

    /// Renders a page of the PDF at `path`. This is the workhorse the handle cache
    /// and neighbour pre-render build on.
    pub fn render_page_at(path: &Path, page: u32, scale: f32) -> Result<RasterizedPage, AppError> {
        Self::ensure_file(path)?;
        let document = Self::load(pdfium()?, path)?;
        Self::rasterize(&document, page, scale)
    }

    /// Extracts the text layer of a single page.
    pub fn extract_page_text(path: &Path, page: u32) -> Result<String, AppError> {
        Self::ensure_file(path)?;
        let document = Self::load(pdfium()?, path)?;
        Self::page_text(&document, page)
    }

    /// Page count, read without keeping the document open.
    pub fn page_count(path: &Path) -> Result<u32, AppError> {
        Self::ensure_file(path)?;
        let document = Self::load(pdfium()?, path)?;
        Ok(document.pages().len() as u32)
    }

    /// Resolves once per process. Deliberately lazy: `current_exe()` only reflects
    /// the real install path once Tauri has run `set_current_dir` during setup.
    fn library_path() -> Result<&'static PathBuf, AppError> {
        static RESOLVED: OnceLock<Result<PathBuf, AppError>> = OnceLock::new();

        RESOLVED
            .get_or_init(|| {
                let env_override = std::env::var(PDFIUM_DEV_ENV_VAR).ok();
                resolve_library_path(env_override.as_deref(), exe_directory().as_deref())
            })
            .as_ref()
            .map_err(|e| AppError::PdfiumUnavailable(e.to_string()))
    }

    /// Whether a usable PDFium library is present on this machine.
    ///
    /// Tests gate real rasterization on this: the build machines (aarch64 Linux)
    /// cannot load Windows PDFium, so a plain `cargo test` must stay green without it.
    pub fn pdfium_available() -> bool {
        Self::bind().is_ok()
    }

    fn bind() -> Result<Box<dyn PdfiumLibraryBindings>, AppError> {
        let library = Self::library_path()?;
        Pdfium::bind_to_library(library)
            .map_err(|e| AppError::PdfiumUnavailable(format!("{}: {e}", library.display())))
    }
}

impl ReaderProvider for PdfProvider {
    fn open(&self, path: &Path) -> Result<DocumentMeta, AppError> {
        Self::ensure_file(path)?;
        let document = Self::load(pdfium()?, path)?;

        let metadata = document.metadata();
        let tag = |kind: PdfDocumentMetadataTagType| {
            metadata
                .get(kind)
                .map(|tag| tag.value().trim().to_owned())
                .filter(|value| !value.is_empty())
        };

        Ok(DocumentMeta {
            id: String::new(), // assigned by the library layer on import
            title: tag(PdfDocumentMetadataTagType::Title).or_else(|| {
                path.file_stem()
                    .and_then(|stem| stem.to_str())
                    .map(str::to_owned)
            }),
            author: tag(PdfDocumentMetadataTagType::Author),
            pages: Some(document.pages().len() as u32),
            format: Format::Pdf,
        })
    }

    /// Real rasterization via PDFium, returned as encoded PNG (DECISIONS P1-D1).
    fn render_page(
        &self,
        entry: &LibraryEntry,
        page: u32,
        scale: f32,
    ) -> Result<Vec<u8>, AppError> {
        Ok(Self::render_page_at(Path::new(&entry.path), page, scale)?.bytes)
    }

    /// PDFium text layer for one page (DECISIONS Q3/D7).
    fn extract_text(&self, entry: &LibraryEntry, page: u32) -> Result<String, AppError> {
        Self::extract_page_text(Path::new(&entry.path), page)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_fails_before_touching_pdfium() {
        let error = PdfProvider
            .open(Path::new("definitely/not/here.pdf"))
            .unwrap_err();
        assert_eq!(error.code(), "FILE_NOT_FOUND");
    }

    #[test]
    fn absent_pdfium_reports_a_clear_error_rather_than_panicking() {
        let path = std::env::temp_dir().join("docreader-p0-not-a-real.pdf");
        std::fs::write(&path, b"%PDF-1.4\nnot really a pdf\n").unwrap();

        // Either pdfium is present (dev machine) or we get a typed, actionable
        // error — the one thing that must never happen is a panic.
        if let Err(error) = PdfProvider.open(&path) {
            assert!(
                matches!(error.code(), "PDFIUM_UNAVAILABLE" | "OTHER"),
                "unexpected error: {error}"
            );
        }
        let _ = std::fs::remove_file(&path);
    }

    /// Library row pointing at the committed PDF fixture.
    fn fixture_entry() -> LibraryEntry {
        LibraryEntry {
            meta: DocumentMeta {
                id: uuid::Uuid::new_v4().to_string(),
                title: Some("Fixture".into()),
                author: None,
                pages: Some(3),
                format: Format::Pdf,
            },
            path: fixture_pdf().display().to_string(),
            missing: false,
            added_at: 0,
            last_opened_at: None,
        }
    }

    fn fixture_pdf() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join("sample.pdf")
    }

    #[test]
    fn scale_is_clamped_into_a_usable_range() {
        // Guards the render path against zero/absurd scales without needing PDFium.
        assert_eq!(clamp_scale(0.0), MIN_RENDER_SCALE);
        assert_eq!(clamp_scale(-3.0), MIN_RENDER_SCALE);
        assert_eq!(clamp_scale(1_000.0), MAX_RENDER_SCALE);
        assert_eq!(clamp_scale(1.5), 1.5);

        // Non-finite input falls back to the default rather than clamping to MAX:
        // a NaN or infinite zoom is a caller bug, and silently rendering at the
        // maximum scale could attempt a gigabyte-sized allocation.
        assert_eq!(clamp_scale(f32::NAN), 1.0);
        assert_eq!(clamp_scale(f32::INFINITY), 1.0);
        assert_eq!(clamp_scale(f32::NEG_INFINITY), 1.0);
    }

    #[test]
    fn renders_page_zero_of_the_fixture_to_a_valid_png() {
        if !PdfProvider::pdfium_available() {
            eprintln!("skipping: PDFium not available on this machine");
            return;
        }

        let rendered = PdfProvider::render_page_at(&fixture_pdf(), 0, 1.0).unwrap();

        // PNG magic number: a valid encoded image, not raw pixels.
        assert_eq!(&rendered.bytes[..8], b"\x89PNG\r\n\x1a\n");
        // Letter page at 1.0 scale ≈ 612x792; assert non-degenerate and roughly right.
        assert!(
            rendered.width > 100 && rendered.height > 100,
            "{rendered:?}"
        );
        assert!(rendered.height > rendered.width, "portrait page expected");
    }

    #[test]
    fn renders_are_scaled_by_the_requested_factor() {
        if !PdfProvider::pdfium_available() {
            eprintln!("skipping: PDFium not available on this machine");
            return;
        }

        let small = PdfProvider::render_page_at(&fixture_pdf(), 0, 1.0).unwrap();
        let large = PdfProvider::render_page_at(&fixture_pdf(), 0, 2.0).unwrap();

        assert!(large.width > small.width, "{large:?} vs {small:?}");
        assert!(large.height > small.height, "{large:?} vs {small:?}");
        assert!(large.bytes.len() > small.bytes.len());
    }

    #[test]
    fn extracts_the_text_layer_of_the_fixture_per_page() {
        if !PdfProvider::pdfium_available() {
            eprintln!("skipping: PDFium not available on this machine");
            return;
        }

        let path = fixture_pdf();
        for (page, token) in [(0_u32, "ALPHA"), (1, "BRAVO"), (2, "CHARLIE")] {
            let text = PdfProvider::extract_page_text(&path, page).unwrap();
            assert!(
                text.contains(token),
                "page {page} should contain {token}, got {text:?}"
            );
        }
    }

    #[test]
    fn text_extraction_does_not_bleed_between_pages() {
        if !PdfProvider::pdfium_available() {
            eprintln!("skipping: PDFium not available on this machine");
            return;
        }

        let path = fixture_pdf();
        let page_two = PdfProvider::extract_page_text(&path, 1).unwrap();
        assert!(page_two.contains("BRAVO"));
        assert!(
            !page_two.contains("ALPHA"),
            "page 2 leaked page 1 text: {page_two:?}"
        );
        assert!(
            !page_two.contains("CHARLIE"),
            "page 2 leaked page 3 text: {page_two:?}"
        );
    }

    #[test]
    fn out_of_range_page_is_an_error_not_a_panic() {
        // Deliberately not PDFium-gated in spirit, but page access needs the library.
        if !PdfProvider::pdfium_available() {
            eprintln!("skipping: PDFium not available on this machine");
            return;
        }

        let error = PdfProvider::render_page_at(&fixture_pdf(), 99, 1.0).unwrap_err();
        assert_eq!(error.code(), "OTHER");
    }

    #[test]
    fn provider_trait_render_and_extract_use_the_entry_path() {
        if !PdfProvider::pdfium_available() {
            eprintln!("skipping: PDFium not available on this machine");
            return;
        }

        let entry = fixture_entry();
        let bytes = PdfProvider.render_page(&entry, 0, 1.0).unwrap();
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
        assert!(PdfProvider
            .extract_text(&entry, 0)
            .unwrap()
            .contains("ALPHA"));
    }

    #[test]
    fn an_entry_pointing_at_a_missing_file_reports_file_not_found() {
        let mut entry = fixture_entry();
        entry.path = "definitely/not/here.pdf".into();
        assert_eq!(
            PdfProvider.render_page(&entry, 0, 1.0).unwrap_err().code(),
            "FILE_NOT_FOUND"
        );
        assert_eq!(
            PdfProvider.extract_text(&entry, 0).unwrap_err().code(),
            "FILE_NOT_FOUND"
        );
    }

    #[test]
    fn page_count_matches_the_fixture() {
        if !PdfProvider::pdfium_available() {
            eprintln!("skipping: PDFium not available on this machine");
            return;
        }
        assert_eq!(PdfProvider::page_count(&fixture_pdf()).unwrap(), 3);
    }

    #[test]
    fn the_handle_cache_reuses_one_document_for_repeated_access() {
        if !PdfProvider::pdfium_available() {
            eprintln!("skipping: PDFium not available on this machine");
            return;
        }

        let handles = DocumentHandles::new();
        let path = fixture_pdf();

        for _ in 0..5 {
            let handle = handles.acquire(&path, None).unwrap();
            assert_eq!(handle.lock().unwrap().pages().len(), 3);
        }

        // Five acquisitions, one open document: the whole point of P1-D5. If the
        // cache were bypassed this would be 5 and page turns would re-parse the file.
        assert_eq!(handles.open_count(), 1);
    }

    #[test]
    fn distinct_paths_get_distinct_handles() {
        if !PdfProvider::pdfium_available() {
            eprintln!("skipping: PDFium not available on this machine");
            return;
        }

        let mut other = fixture_pdf();
        other.set_file_name("sample-copy.pdf");
        std::fs::copy(fixture_pdf(), &other).unwrap();

        let handles = DocumentHandles::new();
        handles.acquire(&fixture_pdf(), None).unwrap();
        handles.acquire(&other, None).unwrap();
        assert_eq!(handles.open_count(), 2);

        let _ = std::fs::remove_file(&other);
    }

    #[test]
    fn a_changed_content_hash_does_not_reuse_the_old_handle() {
        if !PdfProvider::pdfium_available() {
            eprintln!("skipping: PDFium not available on this machine");
            return;
        }

        let handles = DocumentHandles::new();
        let path = fixture_pdf();
        handles.acquire(&path, Some("hash-a")).unwrap();
        handles.acquire(&path, Some("hash-b")).unwrap();

        // Same path, different content: must not serve the stale document.
        assert_eq!(handles.open_count(), 2);
    }

    #[test]
    fn forgetting_a_document_closes_it() {
        if !PdfProvider::pdfium_available() {
            eprintln!("skipping: PDFium not available on this machine");
            return;
        }

        let handles = DocumentHandles::new();
        let path = fixture_pdf();
        handles.acquire(&path, Some("h")).unwrap();
        assert_eq!(handles.open_count(), 1);

        handles.forget(&path, Some("h"));
        assert_eq!(handles.open_count(), 0);
    }

    #[test]
    fn the_cache_is_bounded_and_evicts_the_least_recently_used() {
        if !PdfProvider::pdfium_available() {
            eprintln!("skipping: PDFium not available on this machine");
            return;
        }

        let mut copies = Vec::new();
        for index in 0..(MAX_OPEN_DOCUMENTS + 2) {
            let mut copy = fixture_pdf();
            copy.set_file_name(format!("sample-{index}.pdf"));
            std::fs::copy(fixture_pdf(), &copy).unwrap();
            copies.push(copy);
        }

        let handles = DocumentHandles::new();
        for copy in &copies {
            handles.acquire(copy, None).unwrap();
        }

        // Exceeding capacity evicts down to the bound rather than growing without
        // limit: this is what keeps a long session from holding every file open.
        assert_eq!(handles.open_count(), MAX_OPEN_DOCUMENTS);

        // The two oldest were evicted; the most recent is still resident.
        let newest = handles.acquire(copies.last().unwrap(), None).unwrap();
        assert_eq!(newest.lock().unwrap().pages().len(), 3);
        assert_eq!(handles.open_count(), MAX_OPEN_DOCUMENTS);

        for copy in &copies {
            let _ = std::fs::remove_file(copy);
        }
    }

    #[test]
    fn cached_render_and_extract_agree_with_the_uncached_paths() {
        if !PdfProvider::pdfium_available() {
            eprintln!("skipping: PDFium not available on this machine");
            return;
        }

        let handles = DocumentHandles::new();
        let path = fixture_pdf();

        let cached_render = PdfProvider::render_cached(&handles, &path, None, 1, 1.0).unwrap();
        let direct_render = PdfProvider::render_page_at(&path, 1, 1.0).unwrap();
        assert_eq!(cached_render, direct_render);

        let cached_text = PdfProvider::extract_cached(&handles, &path, None, 1).unwrap();
        assert_eq!(
            cached_text,
            PdfProvider::extract_page_text(&path, 1).unwrap()
        );
        assert!(cached_text.contains("BRAVO"));
    }

    #[test]
    fn concurrent_renders_of_the_same_document_do_not_interleave_badly() {
        if !PdfProvider::pdfium_available() {
            eprintln!("skipping: PDFium not available on this machine");
            return;
        }

        // Two threads, one document: the per-document mutex must serialise them
        // (pdfium-render's default thread_safe feature makes concurrent use of one
        // PdfDocument unsound, so this both exercises and guards that contract).
        // A third thread works a different document to show the map lock is not held
        // across renders, which would serialise unrelated documents.
        let handles = Arc::new(DocumentHandles::new());
        let path = fixture_pdf();
        let mut other = fixture_pdf();
        other.set_file_name("sample-concurrent.pdf");
        std::fs::copy(&path, &other).unwrap();

        let results = std::thread::scope(|scope| {
            let mut threads = Vec::new();
            for page in [0_u32, 1, 2] {
                let handles = Arc::clone(&handles);
                let path = path.clone();
                threads.push(scope.spawn(move || {
                    PdfProvider::render_cached(&handles, &path, None, page, 1.0)
                        .unwrap()
                        .bytes
                }));
            }
            let handles = Arc::clone(&handles);
            let other_for_thread = other.clone();
            threads.push(scope.spawn(move || {
                PdfProvider::render_cached(&handles, &other_for_thread, None, 0, 1.0)
                    .unwrap()
                    .bytes
            }));
            threads
                .into_iter()
                .map(|t| t.join().unwrap())
                .collect::<Vec<_>>()
        });

        for bytes in &results {
            assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
        }
        // Both documents were cached and the run did not deadlock or panic.
        assert_eq!(handles.open_count(), 2);

        let _ = std::fs::remove_file(&other);
    }

    #[test]
    fn handle_key_distinguishes_path_and_content() {
        let a = handle_key(Path::new("C:/x/a.pdf"), Some("h1"));
        let b = handle_key(Path::new("C:/x/b.pdf"), Some("h1"));
        let c = handle_key(Path::new("C:/x/a.pdf"), Some("h2"));
        assert_ne!(a, b);
        assert_ne!(a, c);
        assert_eq!(a, handle_key(Path::new("C:/x/a.pdf"), Some("h1")));
    }

    #[test]
    fn an_unavailable_page_reports_an_error_through_the_cache() {
        if !PdfProvider::pdfium_available() {
            eprintln!("skipping: PDFium not available on this machine");
            return;
        }

        let handles = DocumentHandles::new();
        let error =
            PdfProvider::render_cached(&handles, &fixture_pdf(), None, 42, 1.0).unwrap_err();
        assert_eq!(error.code(), "OTHER");
    }

    #[test]
    fn acquiring_a_missing_file_fails_without_caching_anything() {
        let handles = DocumentHandles::new();
        let error = handles
            .acquire(Path::new("definitely/not/here.pdf"), None)
            .unwrap_err();
        assert_eq!(error.code(), "FILE_NOT_FOUND");
        assert_eq!(handles.open_count(), 0);
    }

    fn names(paths: &[PathBuf]) -> Vec<String> {
        paths
            .iter()
            .map(|path| path.to_string_lossy().replace('\\', "/"))
            .collect()
    }

    #[test]
    fn dll_beside_the_executable_is_tried_first() {
        let found = names(&candidate_paths_in(Path::new("C:/Apps/DocumentReader")));
        assert_eq!(found[0], "C:/Apps/DocumentReader/pdfium.dll");
    }

    #[test]
    fn install_layouts_are_covered() {
        // NSIS install: resources alongside the exe.
        let install = names(&candidate_paths_in(Path::new("C:/Apps/DocumentReader")));
        assert!(
            install.contains(&"C:/Apps/DocumentReader/resources/pdfium.dll".to_string()),
            "install layout missing: {install:?}"
        );

        // Dev build: src-tauri/target/debug -> src-tauri/resources.
        let dev = names(&candidate_paths_in(Path::new(
            "C:/repo/src-tauri/target/debug",
        )));
        assert!(
            dev.contains(&"C:/repo/src-tauri/resources/pdfium.dll".to_string()),
            "dev layout missing: {dev:?}"
        );
    }

    #[test]
    fn candidate_discovery_never_panics_on_shallow_paths() {
        // A root-level exe dir has no ancestors; the walk must terminate cleanly.
        for dir in ["/", "C:/"] {
            let paths = candidate_paths_in(Path::new(dir));
            assert!(!paths.is_empty());
        }
    }

    #[test]
    fn the_first_existing_candidate_wins() {
        let dir = std::env::temp_dir().join(format!("docreader-pdfium-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // Nothing present yet: no candidate resolves.
        assert!(!candidate_paths_in(&dir).iter().any(|p| p.is_file()));

        // Only the nested resource copy exists; it must still be found.
        let nested = dir.join("resources");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join(PDFIUM_BUNDLED_FILE), b"fake").unwrap();

        let resolved = candidate_paths_in(&dir).into_iter().find(|p| p.is_file());
        assert_eq!(resolved, Some(nested.join(PDFIUM_BUNDLED_FILE)));

        let _ = std::fs::remove_dir_all(&dir);
    }

    fn scratch_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("docreader-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn installed_layout_resolves_the_dll_beside_the_executable() {
        // The exact shape the NSIS installer produces (verified against a real
        // installer payload): pdfium.dll sits next to document-reader.exe.
        let dir = scratch_dir("installed");
        std::fs::write(dir.join(PDFIUM_BUNDLED_FILE), b"dll").unwrap();

        let resolved = resolve_library_path(None, Some(&dir)).unwrap();
        assert_eq!(resolved, dir.join(PDFIUM_BUNDLED_FILE));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dev_layout_resolves_the_resource_copy_up_the_tree() {
        // target/debug/ -> walk up -> src-tauri/resources/pdfium.dll
        let root = scratch_dir("dev");
        let resources = root.join("resources");
        let profile = root.join("target").join("debug");
        std::fs::create_dir_all(&resources).unwrap();
        std::fs::create_dir_all(&profile).unwrap();
        std::fs::write(resources.join(PDFIUM_BUNDLED_FILE), b"dll").unwrap();

        let resolved = resolve_library_path(None, Some(&profile)).unwrap();
        assert_eq!(resolved, resources.join(PDFIUM_BUNDLED_FILE));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn env_override_takes_priority_over_bundled_copies() {
        let root = scratch_dir("override");
        let bundled = root.join(PDFIUM_BUNDLED_FILE);
        let override_path = root.join("elsewhere.pdfium");
        std::fs::write(&bundled, b"bundled").unwrap();
        std::fs::write(&override_path, b"override").unwrap();

        let resolved =
            resolve_library_path(Some(&override_path.display().to_string()), Some(&root)).unwrap();
        assert_eq!(resolved, override_path, "env override must win");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_broken_env_override_is_reported_rather_than_silently_ignored() {
        // Falling back to a bundled copy would mask a real misconfiguration.
        let root = scratch_dir("broken-override");
        std::fs::write(root.join(PDFIUM_BUNDLED_FILE), b"bundled").unwrap();

        let error = resolve_library_path(Some("C:/nope/missing.dll"), Some(&root)).unwrap_err();
        assert_eq!(error.code(), "PDFIUM_UNAVAILABLE");
        assert!(error.to_string().contains("missing file"), "{error}");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn absent_pdfium_reports_an_actionable_error() {
        let root = scratch_dir("absent");
        let error = resolve_library_path(None, Some(&root)).unwrap_err();
        assert_eq!(error.code(), "PDFIUM_UNAVAILABLE");
        // The message must tell the user how to fix it.
        assert!(error.to_string().contains(PDFIUM_DEV_ENV_VAR), "{error}");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn resolution_without_an_exe_directory_still_reports_cleanly() {
        // current_exe() can fail; that must be an error, never a panic.
        assert_eq!(
            resolve_library_path(None, None).unwrap_err().code(),
            "PDFIUM_UNAVAILABLE"
        );
    }

    #[test]
    fn pdfium_availability_check_never_panics() {
        // Gated render tests call this; it must be safe on machines without PDFium.
        let _ = PdfProvider::pdfium_available();
    }
}
