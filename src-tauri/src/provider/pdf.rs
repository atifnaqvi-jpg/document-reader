use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

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

/// The process-wide PDFium serialisation lock, re-entrant per thread.
///
/// pdfium-render's `thread_safe` feature does **not** make concurrent PDFium calls
/// safe, despite what its name and our earlier comments claimed. It wraps only
/// `FPDF_InitLibrary`/`FPDF_DestroyLibrary` in a mutex whose guard is held from init
/// until destroy, and delegates the other 76 FFI entry points straight through
/// unguarded. Upstream's README says the mutex "has the effect of sequencing all calls
/// to Pdfium", but that only follows if every thread owns its own `Pdfium` and so
/// contends for the mutex. D5 mandates a *single* instance, whose mutex is therefore
/// taken once and serialises nothing.
///
/// The consequence was real heap corruption: two renders of *different* documents on
/// different threads abort the process (`tcache_thread_shutdown(): unaligned tcache
/// chunk detected` on Linux; `STATUS_ACCESS_VIOULATION` on Windows). Renders of the
/// *same* document survived only because the per-document mutex happened to serialise
/// them. It surfaced only once the suite actually loaded PDFium — the guarded tests had
/// been skipping, so nothing exercised it.
///
/// So the lock lives here. Every public entry point that calls into PDFium holds it for
/// the whole logical operation, which makes PDFium calls execute as if single-threaded.
/// Re-entrancy matters because the call graph nests (`render_cached` -> `acquire` ->
/// `load`) and a plain `Mutex` would self-deadlock; it also lets document `Drop`s —
/// which call `FPDF_CloseDocument` — happen under the lock.
///
/// Lock order is always gate -> handle map -> document. Every thread takes the gate
/// first, so there is no cycle to deadlock on.
struct PdfiumGate {
    /// `Some` only on the outermost acquisition; inner ones just raise the depth.
    _guard: Option<MutexGuard<'static, ()>>,
}

thread_local! {
    /// Per-thread nesting depth. A `thread_local!` is enough: the invariant is only
    /// ever "this thread already holds the gate", which is thread-local by definition.
    static PDFIUM_DEPTH: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

impl PdfiumGate {
    fn acquire() -> Self {
        static GATE: Mutex<()> = Mutex::new(());
        let outermost = PDFIUM_DEPTH.with(|depth| {
            let current = depth.get();
            depth.set(current + 1);
            current == 0
        });
        Self {
            _guard: outermost.then(|| GATE.lock().unwrap_or_else(|e| e.into_inner())),
        }
    }
}

impl Drop for PdfiumGate {
    fn drop(&mut self) {
        PDFIUM_DEPTH.with(|depth| depth.set(depth.get().saturating_sub(1)));
    }
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
///   document serialise on it. That alone is not sufficient for safety, though — two
///   threads on *different* documents still race inside PDFium's global allocator — so
///   every entry point additionally holds [`PdfiumGate`]. See its docs for the crash
///   that demonstrated this.
pub struct DocumentHandles {
    /// The `Option<PasswordHandle>` is the password that unlocked this document, held so the
    /// `PdfDocument`'s borrow of it stays valid for as long as the entry lives. It is deliberately
    /// not read: dropping it early would free bytes a live document points at.
    entries: Mutex<HashMap<String, (SharedDocument, u64, Option<PasswordHandle>)>>,
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
    ///
    /// A remembered password (T10) is applied here rather than at each call site, because this is
    /// the only path that opens a document: every render, text extraction, page count and handle
    /// lookup funnels through it. Threading a password through each entry point instead would leave
    /// the viewer unable to draw a document the editor had just decrypted -- the same
    /// "opens nowhere" defect, one layer down.
    pub fn acquire(
        &self,
        path: &Path,
        content_hash: Option<&str>,
    ) -> Result<SharedDocument, AppError> {
        // Held for the whole operation, including the load below. Callers lock the
        // returned document and keep rendering while this guard is alive, so a render
        // is covered end to end.
        let _gate = PdfiumGate::acquire();
        PdfProvider::ensure_file(path)?;
        let key = handle_key(path, content_hash);
        let tick = self.next_tick();

        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());

        if let Some((handle, last_used, _)) = entries.get_mut(&key) {
            *last_used = tick;
            return Ok(Arc::clone(handle));
        }

        let (document, password) = PdfProvider::load_for_cache(pdfium()?, path)?;
        let handle = Arc::new(Mutex::new(document));
        entries.insert(key, (Arc::clone(&handle), tick, password));

        // Evict the least-recently-used entries. Dropping the last `Arc` closes the
        // document, unless a render still holds a clone — in which case the drop
        // happens when that render finishes, which is precisely when it is safe.
        if entries.len() > MAX_OPEN_DOCUMENTS {
            let mut by_age: Vec<(String, u64)> = entries
                .iter()
                .map(|(key, (_, last_used, _))| (key.clone(), *last_used))
                .collect();
            by_age.sort_by_key(|(_, last_used)| *last_used);
            for (stale, _) in by_age.into_iter().take(entries.len() - MAX_OPEN_DOCUMENTS) {
                entries.remove(&stale);
            }
        }

        Ok(handle)
    }

    /// Runs `f` against `path`'s document with every lock held in the correct order:
    /// gate → handle map → document.
    ///
    /// This is the only place PDFium is touched for a cached document, so the locking
    /// discipline cannot drift as call sites are added. The gate is acquired *before*
    /// the document lock and is **held while `f` runs**, which is what makes a render
    /// safe: acquiring it only for [`acquire`](Self::acquire) would release it before
    /// the actual PDFium work and reintroduce the race.
    pub fn with_document<T>(
        &self,
        path: &Path,
        content_hash: Option<&str>,
        f: impl FnOnce(&PdfDocument<'static>) -> Result<T, AppError>,
    ) -> Result<T, AppError> {
        let _gate = PdfiumGate::acquire();
        let handle = self.acquire(path, content_hash)?;
        let document = handle.lock().unwrap_or_else(|e| e.into_inner());
        f(&document)
    }

    /// Drops a cached handle, e.g. when the file is known to have changed.
    ///
    /// Takes the gate because dropping the last `Arc` calls `FPDF_CloseDocument`, which
    /// must not race a render on another thread.
    pub fn forget(&self, path: &Path, content_hash: Option<&str>) {
        let _gate = PdfiumGate::acquire();
        let key = handle_key(path, content_hash);
        self.entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&key);
    }

    /// Drops every handle for a path regardless of content hash.
    ///
    /// This is what re-import needs: when a file is replaced in place the id and path
    /// stay the same but the hash *changes*, so a hash-keyed [`forget`](Self::forget)
    /// would compute a key that was never inserted and leave the stale document — and
    /// its parsed pages — resident. Dropping by path cannot miss.
    pub fn forget_path(&self, path: &Path) {
        let _gate = PdfiumGate::acquire();
        let prefix = format!("{}\u{1}", path.display());
        self.entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|key, _| !key.starts_with(&prefix));
    }

    /// Number of documents currently held open.
    pub fn open_count(&self) -> usize {
        self.entries.lock().unwrap_or_else(|e| e.into_inner()).len()
    }
}

/// Keeps a password alive for as long as the document it unlocked is open.
///
/// A newtype so the field cannot be mistaken for unused state: the `Arc` has no readers, but its
/// *lifetime* is the point. Named with a leading underscore because nothing reads it.
struct PasswordHandle {
    _password: Arc<crate::editor::session::SessionPassword>,
}

/// Whether a load failure is PDFium's password error.
///
/// Matched on the **typed** variant rather than on the rendered message: `Display` for
/// `PdfiumLibraryInternalError` is `Debug`-shaped text that has already changed shape once in this
/// dependency, so a `contains("Password")` test would start classifying every failure as a password
/// failure the day it is reworded.
fn is_password_error(error: &PdfiumError) -> bool {
    matches!(
        error,
        PdfiumError::PdfiumLibraryInternalError(PdfiumInternalError::PasswordError)
    )
}

impl PdfProvider {
    /// Renders a page through the handle cache — the path both the command surface
    /// (P1.3) and neighbour pre-render (P1.4) use.
    ///
    /// `annotations` is the P2-D9 "original vs annotated" view switch. It suppresses the
    /// annotation layer PDFium composites during rasterization, rather than hiding
    /// already-rendered marks in the DOM: hiding cannot remove pixels that are already in
    /// the bitmap, and leaves stale selection targets behind.
    pub fn render_cached(
        handles: &DocumentHandles,
        path: &Path,
        content_hash: Option<&str>,
        page: u32,
        scale: f32,
        annotations: bool,
    ) -> Result<RasterizedPage, AppError> {
        // The gate is held for the whole rasterization, not just the lookup: PDFium
        // renders on different documents must not overlap.
        handles.with_document(path, content_hash, |document| {
            Self::rasterize(document, page, scale, annotations)
        })
    }

    /// Extracts one page's text through the handle cache.
    pub fn extract_cached(
        handles: &DocumentHandles,
        path: &Path,
        content_hash: Option<&str>,
        page: u32,
    ) -> Result<String, AppError> {
        handles.with_document(path, content_hash, |document| {
            Self::page_text(document, page)
        })
    }

    /// Loads `path`, returning the password handle that must outlive the document.
    ///
    /// `PdfDocument<'a>` borrows the password for `'a` (see `load_pdf_from_file`'s signature), which
    /// is the process-wide `Pdfium`'s lifetime. A password fetched from the store therefore cannot be
    /// dropped when this call returns, so it is handed back with the document and the caller must
    /// keep both together: a cached entry stores them as one tuple, and a one-shot read binds them in
    /// the same scope. Returning the owner rather than leaking it is what keeps the bytes zeroisable
    /// on `forget`/`clear`, per T10.
    fn load_for_cache<'a>(
        pdfium: &'a Pdfium,
        path: &Path,
    ) -> Result<(PdfDocument<'a>, Option<PasswordHandle>), AppError> {
        let store = crate::editor::session::shared();
        let key = path.to_string_lossy().to_string();
        match store.get(&key) {
            Some(password) => {
                // SAFETY: `PdfDocument<'a>` borrows the password for as long as the document lives,
                // and here `'a` is the lifetime of the process-wide `Pdfium` (`&'static`). The bytes
                // are kept alive for exactly that long because the `Arc` is *moved into* the
                // returned `PasswordHandle`, which the caller stores in the same cache entry as the
                // document and drops with it. Nothing can invalidate them in the meantime: `wipe`
                // needs `&mut`, and `Arc::into_inner` fails while this clone exists, so the store's
                // own `forget`/`clear` cannot zeroise bytes a live document is reading.
                let borrowed: &'static crate::editor::session::SessionPassword =
                    unsafe { &*Arc::as_ptr(&password) };
                let document = Self::load_with_password(pdfium, path, Some(borrowed.expose()))?;
                Ok((
                    document,
                    Some(PasswordHandle {
                        _password: password,
                    }),
                ))
            }
            None => Ok((Self::load(pdfium, path)?, None)),
        }
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
        Self::load_with_password(pdfium, path, None)
    }

    /// [`load`](Self::load) with a password.
    ///
    /// A PDF this app protects carries a **non-empty user password** (`protect_save` refuses an
    /// empty one), so every load without it fails with `PasswordError`. That reached the user as
    /// `PdfiumLibraryInternalError(PasswordError)` in a banner, with no prompt anywhere and no way
    /// to open the file the app had just written. The library's text is never forwarded for this
    /// case: the distinction the user needs is "enter the password" versus "that one is wrong",
    /// and neither is served by naming a PDFium internal.
    fn load_with_password<'a>(
        pdfium: &'a Pdfium,
        path: &Path,
        password: Option<&'a str>,
    ) -> Result<PdfDocument<'a>, AppError> {
        pdfium
            .load_pdf_from_file(&path.display().to_string(), password)
            .map_err(|error| {
                if is_password_error(&error) {
                    // A refusal with a password in hand means the one we held is stale, which the
                    // user fixes by typing another; with none in hand it means "ask".
                    return match password {
                        Some(_) => AppError::WrongPassword,
                        None => AppError::PasswordRequired,
                    };
                }
                AppError::Other(format!("failed to open PDF: {error}"))
            })
    }

    /// Rasterizes one page to PNG, with the bitmap's pixel dimensions.
    fn rasterize(
        document: &PdfDocument<'_>,
        page: u32,
        scale: f32,
        annotations: bool,
    ) -> Result<RasterizedPage, AppError> {
        let page = document
            .pages()
            .get(page as u16)
            .map_err(|e| AppError::Other(format!("page {page} unavailable: {e}")))?;

        let config = PdfRenderConfig::new()
            .scale_page_by_factor(clamp_scale(scale))
            // Default is `true` (PDFium composites the annotation layer), which is what
            // the "annotated" view wants; `false` is the D9 "original" view.
            .render_annotations(annotations);
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
        let _gate = PdfiumGate::acquire();
        Self::ensure_file(path)?;
        // `_password` is bound so the handle outlives `document`, which borrows its bytes.
        let (document, _password) = Self::load_for_cache(pdfium()?, path)?;
        // Annotations visible: the product default (D9). Callers that need the
        // suppressed variant go through `render_cached`, which is the path the viewer
        // uses and therefore the one that has to carry the flag.
        Self::rasterize(&document, page, scale, true)
    }

    /// Extracts the text layer of a single page.
    pub fn extract_page_text(path: &Path, page: u32) -> Result<String, AppError> {
        let _gate = PdfiumGate::acquire();
        Self::ensure_file(path)?;
        // `_password` is bound so the handle outlives `document`, which borrows its bytes.
        let (document, _password) = Self::load_for_cache(pdfium()?, path)?;
        Self::page_text(&document, page)
    }

    /// Page count, read without keeping the document open.
    pub fn page_count(path: &Path) -> Result<u32, AppError> {
        let _gate = PdfiumGate::acquire();
        Self::ensure_file(path)?;
        // `_password` is bound so the handle outlives `document`, which borrows its bytes.
        let (document, _password) = Self::load_for_cache(pdfium()?, path)?;
        Ok(document.pages().len() as u32)
    }

    /// Per-page annotation counts, read back by PDFium (P2 §7 item 4).
    ///
    /// PDFium exposes the annotation *list* even though it cannot create annotations
    /// (P2 F1), which makes this the only check that the lopdf write path produced a
    /// file a reader will actually show marks in. Reading it back with lopdf would only
    /// prove lopdf agrees with itself.
    pub fn annotation_counts(path: &Path, pages: u32) -> Result<Vec<usize>, AppError> {
        let _gate = PdfiumGate::acquire();
        Self::ensure_file(path)?;
        // `_password` is bound so the handle outlives `document`, which borrows its bytes.
        let (document, _password) = Self::load_for_cache(pdfium()?, path)?;

        (0..pages)
            .map(|index| {
                let page = document
                    .pages()
                    .get(index as u16)
                    .map_err(|e| AppError::Other(format!("page {index} unavailable: {e}")))?;
                Ok(page.annotations().len())
            })
            .collect()
    }

    /// The text objects PDFium sees on one page, for the P2.4 picker (T4).
    ///
    /// Read-only, and deliberately **not** the run's identity: the authoritative address
    /// the write path uses is the lopdf `(op_index, operand_path)`; this only supplies
    /// the displayed text, position and font the UI needs to present a target. Routed
    /// through the shared handle cache so it cannot race a render (P2-D8/F8).
    pub fn page_runs(
        handles: &DocumentHandles,
        path: &Path,
        content_hash: Option<&str>,
        page: u32,
    ) -> Result<Vec<crate::editor::runmetrics::PdfiumRun>, AppError> {
        handles.with_document(path, content_hash, |document| {
            crate::editor::runmetrics::page_runs(document, page, path)
        })
    }

    /// PDFium's whole-page character grid, the span oracle (P2.4.3 T23a).
    ///
    /// Same read-only discipline as [`Self::page_runs`]: routed through the shared handle
    /// cache, no mutation, no save.
    pub fn char_grid(
        handles: &DocumentHandles,
        path: &Path,
        content_hash: Option<&str>,
        page: u32,
    ) -> Result<crate::editor::runmetrics::PdfiumCharGrid, AppError> {
        handles.with_document(path, content_hash, |document| {
            crate::editor::runmetrics::char_grid(document, page, path)
        })
    }

    /// Measures a candidate replacement string through PDFium's own text metrics (T3d).
    ///
    /// Read-only: the text object built to measure is never inserted into the document and
    /// no save is ever performed (T1). `font_name` and `font_size` come from the run the
    /// user picked, so the number is comparable with the run's `old_width`.
    pub fn measure_text(
        handles: &DocumentHandles,
        path: &Path,
        content_hash: Option<&str>,
        page: u32,
        font_name: &str,
        font_size: f32,
        text: &str,
    ) -> Result<f32, AppError> {
        handles.with_document(path, content_hash, |document| {
            crate::editor::runmetrics::measure(document, page, font_name, font_size, text)
        })
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

    /// The DocMDP level PDFium's typed signature reader reports, or `None` when it reports nothing.
    ///
    /// This is I-SG5's read-back half **through the platform PDFium** (K17: `PdfDocument::signatures()`
    /// and `PdfSignature::modification_detection_permission()` are typed, so this needs no raw binding
    /// work). It is advisory rather than authoritative for two reasons:
    ///
    /// 1. it needs a PDFium library, so the Linux gate cannot run it, while
    ///    [`crate::editor::signature::verify::certification_level`] can; and
    /// 2. PDFium reports the level of the **newest** signature it can read, which on a document with
    ///    several signatures is not necessarily the certification.
    ///
    /// Callers therefore treat `None` as "no opinion" and fall back to our own parse. Returning an
    /// `Option` rather than an error keeps a missing library from becoming a reason a user cannot
    /// edit their file: an unenforceable level must never silently become "permitted", but the
    /// *parse* is what decides that, and it does not need PDFium.
    pub fn doc_mdp_level(path: &Path) -> Option<u8> {
        // Same gate as every other PDFium entry point: the library is not thread-safe outside this
        // process-wide serialisation, and a certification read must not race a render.
        let _gate = PdfiumGate::acquire();
        let (document, _password) = Self::load_for_cache(pdfium().ok()?, path).ok()?;
        let signatures = document.signatures();
        // `get` rather than a `first()` helper: pdfium-render 0.8.37's `PdfSignatures` exposes
        // `len`/`is_empty`/`get`/`iter` and no `first`, and `get(0)` on an unsigned document is an
        // out-of-bounds error rather than an empty optional, so the emptiness test has to come first.
        if signatures.is_empty() {
            return None;
        }
        let signature = signatures.get(0).ok()?;
        match signature.modification_detection_permission() {
            Ok(PdfSignatureModificationDetectionPermission::Mdp1) => Some(1),
            Ok(PdfSignatureModificationDetectionPermission::Mdp2) => Some(2),
            Ok(PdfSignatureModificationDetectionPermission::Mdp3) => Some(3),
            // A signature PDFium cannot read, or one that is not a certification, is not a level we
            // may invent. Every arm is spelled out: a wildcard here would let a future variant
            // silently become a permissive answer. The caller's own parse still gets a turn.
            Err(_) => None,
        }
    }
}

impl ReaderProvider for PdfProvider {
    fn open(&self, path: &Path) -> Result<DocumentMeta, AppError> {
        let _gate = PdfiumGate::acquire();
        Self::ensure_file(path)?;
        // `_password` is bound so the handle outlives `document`, which borrows its bytes.
        let (document, _password) = Self::load_for_cache(pdfium()?, path)?;

        let metadata = document.metadata();
        let tag = |kind: PdfDocumentMetadataTagType| {
            metadata
                .get(kind)
                .map(|tag| tag.value().trim().to_owned())
                .filter(|value| !value.is_empty())
        };

        // P2.4 T7: the trailer decides, not whether a password was needed. Read through
        // lopdf rather than PDFium because PDFium's `metadata()` does not expose the
        // `/Encrypt` declaration, and a PDF with an empty user password opens fine while
        // still being protected.
        let encrypted = crate::editor::protect::is_encrypted(path).unwrap_or(false);

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
            encrypted,
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
                encrypted: false,
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
    #[ignore = "requires a real PDFium library; `cargo test -- --include-ignored` after fetching it"]
    fn renders_page_zero_of_the_fixture_to_a_valid_png() {
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
    #[ignore = "requires a real PDFium library; `cargo test -- --include-ignored` after fetching it"]
    fn renders_are_scaled_by_the_requested_factor() {
        let small = PdfProvider::render_page_at(&fixture_pdf(), 0, 1.0).unwrap();
        let large = PdfProvider::render_page_at(&fixture_pdf(), 0, 2.0).unwrap();

        assert!(large.width > small.width, "{large:?} vs {small:?}");
        assert!(large.height > small.height, "{large:?} vs {small:?}");
        assert!(large.bytes.len() > small.bytes.len());
    }

    #[test]
    #[ignore = "requires a real PDFium library; `cargo test -- --include-ignored` after fetching it"]
    fn extracts_the_text_layer_of_the_fixture_per_page() {
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
    #[ignore = "requires a real PDFium library; `cargo test -- --include-ignored` after fetching it"]
    fn text_extraction_does_not_bleed_between_pages() {
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
    #[ignore = "requires a real PDFium library; `cargo test -- --include-ignored` after fetching it"]
    fn out_of_range_page_is_an_error_not_a_panic() {
        let error = PdfProvider::render_page_at(&fixture_pdf(), 99, 1.0).unwrap_err();
        assert_eq!(error.code(), "OTHER");
    }

    #[test]
    #[ignore = "requires a real PDFium library; `cargo test -- --include-ignored` after fetching it"]
    fn provider_trait_render_and_extract_use_the_entry_path() {
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
    #[ignore = "requires a real PDFium library; `cargo test -- --include-ignored` after fetching it"]
    fn page_count_matches_the_fixture() {
        assert_eq!(PdfProvider::page_count(&fixture_pdf()).unwrap(), 3);
    }

    #[test]
    #[ignore = "requires a real PDFium library; `cargo test -- --include-ignored` after fetching it"]
    fn the_handle_cache_reuses_one_document_for_repeated_access() {
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
    #[ignore = "requires a real PDFium library; `cargo test -- --include-ignored` after fetching it"]
    fn distinct_paths_get_distinct_handles() {
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
    #[ignore = "requires a real PDFium library; `cargo test -- --include-ignored` after fetching it"]
    fn a_changed_content_hash_does_not_reuse_the_old_handle() {
        let handles = DocumentHandles::new();
        let path = fixture_pdf();
        handles.acquire(&path, Some("hash-a")).unwrap();
        handles.acquire(&path, Some("hash-b")).unwrap();

        // Same path, different content: must not serve the stale document.
        assert_eq!(handles.open_count(), 2);
    }

    #[test]
    #[ignore = "requires a real PDFium library; `cargo test -- --include-ignored` after fetching it"]
    fn forgetting_a_document_closes_it() {
        let handles = DocumentHandles::new();
        let path = fixture_pdf();
        handles.acquire(&path, Some("h")).unwrap();
        assert_eq!(handles.open_count(), 1);

        handles.forget(&path, Some("h"));
        assert_eq!(handles.open_count(), 0);
    }

    #[test]
    #[ignore = "requires a real PDFium library; `cargo test -- --include-ignored` after fetching it"]
    fn the_cache_is_bounded_and_evicts_the_least_recently_used() {
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
    #[ignore = "requires a real PDFium library; `cargo test -- --include-ignored` after fetching it"]
    fn cached_render_and_extract_agree_with_the_uncached_paths() {
        let handles = DocumentHandles::new();
        let path = fixture_pdf();

        let cached_render =
            PdfProvider::render_cached(&handles, &path, None, 1, 1.0, true).unwrap();
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
    #[ignore = "requires a real PDFium library; `cargo test -- --include-ignored` after fetching it"]
    fn concurrent_renders_of_the_same_document_do_not_interleave_badly() {
        // Two threads, one document: the per-document mutex serialises them. A third
        // thread works a different document, which is the case that used to corrupt
        // the heap — see `concurrent_renders_and_evictions_do_not_corrupt_pdfium`.
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
                    PdfProvider::render_cached(&handles, &path, None, page, 1.0, true)
                        .unwrap()
                        .bytes
                }));
            }
            let handles = Arc::clone(&handles);
            let other_for_thread = other.clone();
            threads.push(scope.spawn(move || {
                PdfProvider::render_cached(&handles, &other_for_thread, None, 0, 1.0, true)
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

    /// Regression guard for the crash that `PdfiumGate` exists to prevent.
    ///
    /// PDFium is not thread-safe beyond init/teardown, and pdfium-render's
    /// `thread_safe` feature does not change that (see [`PdfiumGate`]). The unsound
    /// interleavings are: two renders of *different* documents, and a cached render
    /// racing an uncached load that drops its document (`FPDF_CloseDocument` on `Drop`).
    /// This test drives both, plus eviction past [`MAX_OPEN_DOCUMENTS`]. Without the
    /// gate it aborts reliably (`tcache_thread_shutdown(): unaligned tcache chunk
    /// detected` here, `STATUS_ACCESS_VIOLATION` on Windows).
    ///
    /// The failure mode is a process abort, not a wrong return value, so a clean join is
    /// the whole assertion.
    #[test]
    #[ignore = "requires a real PDFium library; `cargo test -- --include-ignored` after fetching it"]
    fn concurrent_renders_and_evictions_do_not_corrupt_pdfium() {
        let handles = Arc::new(DocumentHandles::new());
        let source = fixture_pdf();

        // Comfortably more documents than the cache can hold, so every acquire past
        // the first MAX_OPEN_DOCUMENTS also evicts.
        let copies: Vec<PathBuf> = (0..(MAX_OPEN_DOCUMENTS * 3))
            .map(|index| {
                let copy = source.with_file_name(format!("docreader-stress-{index}.pdf"));
                std::fs::copy(&source, &copy).unwrap();
                copy
            })
            .collect();

        std::thread::scope(|scope| {
            let mut threads = Vec::new();
            for (index, path) in copies.iter().enumerate() {
                let handles = Arc::clone(&handles);
                let path = path.clone();
                threads.push(scope.spawn(move || {
                    let page = (index % 3) as u32;
                    let rendered =
                        PdfProvider::render_cached(&handles, &path, None, page, 1.0, true).unwrap();
                    assert_eq!(&rendered.bytes[..8], b"\x89PNG\r\n\x1a\n");
                    // Force contention with the eviction path in other threads.
                    let _ = PdfProvider::extract_cached(&handles, &path, None, page);
                }));
            }
            // Uncached loads open and immediately drop a document, so these threads call
            // `FPDF_CloseDocument` while the render threads are inside PDFium.
            for path in copies.iter() {
                let path = path.clone();
                threads.push(scope.spawn(move || {
                    for _ in 0..8 {
                        let _ = PdfProvider::page_count(&path);
                        let _ = PdfProvider::render_page_at(&path, 0, 1.0);
                    }
                }));
            }
            for thread in threads {
                thread.join().unwrap();
            }
        });

        // The cache stayed within its bound despite the churn.
        assert!(handles.open_count() <= MAX_OPEN_DOCUMENTS);

        for copy in &copies {
            let _ = std::fs::remove_file(copy);
        }
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
    #[ignore = "requires a real PDFium library; `cargo test -- --include-ignored` after fetching it"]
    fn an_unavailable_page_reports_an_error_through_the_cache() {
        let handles = DocumentHandles::new();
        let error =
            PdfProvider::render_cached(&handles, &fixture_pdf(), None, 42, 1.0, true).unwrap_err();
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
