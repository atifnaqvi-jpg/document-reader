//! Render cache and the cache-aware render service (P1-D3, P1-D4).
//!
//! Two caches sit in front of a page render:
//!
//! * [`DocumentHandles`](crate::provider::DocumentHandles) avoids re-parsing the file.
//! * [`RenderCache`] avoids re-rasterizing a page that was just drawn.
//!
//! Both are keyed so that a changed document can never serve stale pixels, and this
//! module is deliberately free of PDFium so the eviction policy can be tested without
//! the native library present.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::model::{AppError, RenderedPage};
use crate::provider::{DocumentHandles, PdfProvider};

/// Render cache budget (P1-D3). Bounded by *bytes*, not entries: a 600 DPI page is
/// roughly an order of magnitude larger than a 72 DPI one, so an entry-count bound
/// would let the cache hold tens of megabytes or several gigabytes depending only on
/// the zoom level the user happened to use.
pub const DEFAULT_RENDER_CACHE_BYTES: usize = 200 * 1024 * 1024;

/// Scale is part of the cache key, so it must be quantised: `1.0` and `1.0000001`
/// are the same render, and floating-point noise from a UI zoom control should not
/// produce a new cache entry every frame. Quantised to 1/4 steps across the range the
/// provider will actually honour (see its own scale clamp).
const SCALE_STEP: f32 = 0.25;
const MIN_SCALE_STEPS: u16 = 4; // 1.0×
const MAX_SCALE_STEPS: u16 = 32; // 8.0×

/// Identifies one cached raster (P1-D3): `(doc_id, page, scale)`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RenderKey {
    pub doc_id: String,
    pub page: u32,
    /// Quantised scale, in 1/4 steps, avoiding float keys.
    pub scale_steps: u16,
}

impl RenderKey {
    pub fn new(doc_id: &str, page: u32, scale: f32) -> Self {
        Self {
            doc_id: doc_id.to_string(),
            page,
            scale_steps: quantise_scale(scale),
        }
    }
}

/// Quantises a scale to 1/4 steps, clamped to the range the provider honours.
fn quantise_scale(scale: f32) -> u16 {
    if !scale.is_finite() || scale <= 0.0 {
        return MIN_SCALE_STEPS;
    }
    let steps = (scale / SCALE_STEP).round();
    (steps as i64).clamp(MIN_SCALE_STEPS as i64, MAX_SCALE_STEPS as i64) as u16
}

struct CacheEntry {
    page: Arc<RenderedPage>,
    cost: usize,
    used_at: u64,
}

struct CacheState {
    entries: HashMap<RenderKey, CacheEntry>,
    bytes: usize,
    clock: u64,
}

/// In-process, byte-budgeted LRU of rendered pages (P1-D3). A miss is never an
/// error — it just means rasterizing again.
pub struct RenderCache {
    budget: usize,
    state: Mutex<CacheState>,
}

impl std::fmt::Debug for RenderCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        f.debug_struct("RenderCache")
            .field("entries", &state.entries.len())
            .field("bytes", &state.bytes)
            .field("budget", &self.budget)
            .finish()
    }
}

impl Default for RenderCache {
    fn default() -> Self {
        Self::new(DEFAULT_RENDER_CACHE_BYTES)
    }
}

impl RenderCache {
    pub fn new(budget: usize) -> Self {
        Self {
            budget,
            state: Mutex::new(CacheState {
                entries: HashMap::new(),
                bytes: 0,
                clock: 0,
            }),
        }
    }

    /// Fetches a cached page, marking it most-recently-used. Cheap: the value is
    /// behind an `Arc`, and the stored form is the *base64* payload that goes over
    /// IPC, so a hit also skips re-encoding.
    pub fn get(&self, key: &RenderKey) -> Option<Arc<RenderedPage>> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let clock = &mut state.clock;
        *clock += 1;
        let now = *clock;
        let entry = state.entries.get_mut(key)?;
        entry.used_at = now;
        Some(Arc::clone(&entry.page))
    }

    /// Inserts a rendered page, evicting least-recently-used entries to stay within
    /// the byte budget.
    pub fn insert(&self, key: RenderKey, page: Arc<RenderedPage>) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.clock += 1;
        let now = state.clock;
        let cost = page.bytes.len();

        if let Some(previous) = state.entries.insert(
            key,
            CacheEntry {
                page,
                cost,
                used_at: now,
            },
        ) {
            state.bytes = state.bytes.saturating_sub(previous.cost);
        }
        state.bytes = state.bytes.saturating_add(cost);

        // Evict by least-recent use until back inside budget. The loop stops with at
        // least one entry resident, so a single render larger than the whole budget
        // still gets cached rather than triggering an empty-cache thrash.
        while state.bytes > self.budget && state.entries.len() > 1 {
            let Some(oldest) = state
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.used_at)
                .map(|(key, _)| key.clone())
            else {
                break;
            };
            if let Some(removed) = state.entries.remove(&oldest) {
                state.bytes = state.bytes.saturating_sub(removed.cost);
            }
        }
    }

    /// Drops every raster of a document, e.g. when it is re-imported with new content.
    pub fn forget_document(&self, doc_id: &str) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let doomed: Vec<RenderKey> = state
            .entries
            .keys()
            .filter(|key| key.doc_id == doc_id)
            .cloned()
            .collect();
        for key in doomed {
            if let Some(removed) = state.entries.remove(&key) {
                state.bytes = state.bytes.saturating_sub(removed.cost);
            }
        }
    }

    /// Number of cached pages.
    pub fn len(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entries
            .len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Bytes currently held.
    pub fn bytes(&self) -> usize {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).bytes
    }
}

/// Cache-aware page rendering: the single entry point the command surface and the
/// pre-render task both use, so caching can never be bypassed by a new call site.
pub struct RenderService {
    handles: DocumentHandles,
    cache: RenderCache,
    /// Monotonic request counter, used to abandon superseded speculative work (P1-D4).
    generation: AtomicU64,
}

impl std::fmt::Debug for RenderService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RenderService")
            .field("handles", &self.handles)
            .field("cache", &self.cache)
            .finish()
    }
}

impl Default for RenderService {
    fn default() -> Self {
        Self::new()
    }
}

impl RenderService {
    pub fn new() -> Self {
        Self::with_cache_budget(DEFAULT_RENDER_CACHE_BYTES)
    }

    pub fn with_cache_budget(bytes: usize) -> Self {
        Self {
            handles: DocumentHandles::new(),
            cache: RenderCache::new(bytes),
            generation: AtomicU64::new(0),
        }
    }

    pub fn cache(&self) -> &RenderCache {
        &self.cache
    }

    /// Renders a page, serving from cache when possible.
    ///
    /// This is the *foreground* path: a caller is waiting on the result.
    ///
    /// Blocking and CPU-bound: call it from a `spawn_blocking` worker, never from the
    /// async IPC task.
    pub fn render(
        &self,
        doc_id: &str,
        path: &Path,
        content_hash: Option<&str>,
        page: u32,
        scale: f32,
    ) -> Result<Arc<RenderedPage>, AppError> {
        self.render_inner(doc_id, path, content_hash, page, scale, None)
    }

    /// Opens the next pre-render generation (P1-D4).
    ///
    /// Called once per foreground render request, *before* the page is rendered. The
    /// returned ticket invalidates any pre-render still running for an earlier request,
    /// so a user holding down "next page" does not leave a queue of speculative renders
    /// of pages they have already scrolled past competing with the page they are
    /// waiting for. Speculative work is cancelled only between pages, never mid-render
    /// (there is no way to interrupt PDFium), which bounds the waste to one page.
    pub fn begin_pre_render(&self) -> u64 {
        self.generation.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// True while `ticket` is still the current pre-render generation.
    pub fn is_current(&self, ticket: u64) -> bool {
        self.generation.load(Ordering::SeqCst) == ticket
    }

    fn render_inner(
        &self,
        doc_id: &str,
        path: &Path,
        content_hash: Option<&str>,
        page: u32,
        scale: f32,
        ticket: Option<u64>,
    ) -> Result<Arc<RenderedPage>, AppError> {
        let key = RenderKey::new(doc_id, page, scale);
        if let Some(hit) = self.cache.get(&key) {
            return Ok(hit);
        }

        let raster = PdfProvider::render_cached(&self.handles, path, content_hash, page, scale)?;
        // Encode once, here: the cached value is exactly what goes over IPC, so a hit
        // costs no re-encoding and the byte budget tracks the real payload size.
        let page = Arc::new(RenderedPage {
            bytes: base64_encode(&raster.bytes),
            width: raster.width,
            height: raster.height,
        });

        // A superseded pre-render still inserts: the pixels are correct and may be
        // reached again, so discarding them would only repeat the work later.
        let _ = ticket;
        self.cache.insert(key, Arc::clone(&page));
        Ok(page)
    }

    /// Renders the neighbouring pages into the cache (P1-D4), so the next page turn is
    /// a hit. Best effort: a failure is ignored, because a missing neighbour only costs
    /// a render later and must never surface as an error to the user.
    ///
    /// `ticket` comes from [`begin_pre_render`](Self::begin_pre_render); if it is
    /// superseded the walk stops early.
    ///
    /// Returns how many neighbours were rendered, which is what tests assert on.
    pub fn prerender_neighbours(
        &self,
        ticket: u64,
        doc_id: &str,
        path: &Path,
        content_hash: Option<&str>,
        page: u32,
        scale: f32,
    ) -> usize {
        // Read the page count through the gate, then release it before rendering:
        // holding it across two renders would stall the page the user is waiting for.
        let Ok(page_count) = self.page_count(path, content_hash) else {
            return 0;
        };

        // The page the user is waiting for is already cached by the foreground render
        // that spawned this task, and LRU keeps it resident (it is the most recently
        // used), so only the neighbours need work here.
        let mut rendered = 0;
        for candidate in [page.checked_sub(1), Some(page + 1)].into_iter().flatten() {
            if candidate >= page_count {
                continue;
            }
            // Bail between pages once a newer request has superseded this one: the
            // remaining neighbour belongs to a page the user has already left.
            if !self.is_current(ticket) {
                break;
            }
            let key = RenderKey::new(doc_id, candidate, scale);
            if self.cache.get(&key).is_some() {
                continue;
            }
            if self
                .render_inner(doc_id, path, content_hash, candidate, scale, Some(ticket))
                .is_ok()
            {
                rendered += 1;
            }
        }
        rendered
    }

    /// Page count as PDFium sees it. Authoritative when P0's import-time count was
    /// unavailable, and needed by the viewer to bound navigation and pre-rendering.
    pub fn page_count(&self, path: &Path, content_hash: Option<&str>) -> Result<u32, AppError> {
        self.handles.with_document(path, content_hash, |document| {
            Ok(document.pages().len() as u32)
        })
    }

    /// Extracts one page's text layer through the shared handle cache.
    pub fn extract_text(
        &self,
        path: &Path,
        content_hash: Option<&str>,
        page: u32,
    ) -> Result<String, AppError> {
        PdfProvider::extract_cached(&self.handles, path, content_hash, page)
    }

    /// Drops all cached state for a document: rasters and the open handle. Called when
    /// a document is (re-)imported, so a file changed on disk can never be served from
    /// either cache.
    ///
    /// The handle is dropped by *path*, not by content hash: a re-imported file keeps
    /// its path while its hash changes, so a hash-keyed drop would look up a key that
    /// was never stored and silently leave the previous document resident.
    pub fn forget_document(&self, doc_id: &str, path: &Path) {
        self.cache.forget_document(doc_id);
        self.handles.forget_path(path);
    }

    /// Number of documents currently held open (diagnostics/tests).
    pub fn open_documents(&self) -> usize {
        self.handles.open_count()
    }
}

fn base64_encode(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn page(cost: usize) -> Arc<RenderedPage> {
        Arc::new(RenderedPage {
            bytes: "x".repeat(cost),
            width: 1,
            height: 1,
        })
    }

    fn fixture_pdf() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join("sample.pdf")
    }

    #[test]
    fn quantised_scale_treats_near_identical_zooms_as_one_entry() {
        // Float noise from a zoom control must not create a new entry per frame.
        assert_eq!(quantise_scale(1.0), quantise_scale(1.0000001));
        assert_eq!(quantise_scale(1.1), quantise_scale(1.12));
        // Genuinely different zoom levels stay distinct.
        assert_ne!(quantise_scale(1.0), quantise_scale(1.5));
    }

    #[test]
    fn absurd_scales_are_clamped_into_the_quantised_range() {
        assert_eq!(quantise_scale(0.0), MIN_SCALE_STEPS);
        assert_eq!(quantise_scale(-5.0), MIN_SCALE_STEPS);
        assert_eq!(quantise_scale(f32::NAN), MIN_SCALE_STEPS);
        assert_eq!(quantise_scale(1_000.0), MAX_SCALE_STEPS);
    }

    #[test]
    fn a_hit_costs_nothing_and_a_miss_is_none() {
        let cache = RenderCache::new(1_000);
        let key = RenderKey::new("doc", 3, 1.0);

        assert!(cache.get(&key).is_none(), "cold cache must miss");
        cache.insert(key.clone(), page(10));
        assert!(cache.get(&key).is_some());
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn the_cache_is_bounded_by_bytes_not_entries() {
        // Room for exactly two 100-byte pages.
        let cache = RenderCache::new(200);
        for index in 0..5 {
            cache.insert(RenderKey::new("doc", index, 1.0), page(100));
            // Touching in order makes insertion order equal use order, so eviction
            // takes the lowest page each time.
            cache.get(&RenderKey::new("doc", index, 1.0));
        }

        // Five inserts of 100 bytes into a 200-byte budget must not hold 500 bytes.
        assert!(cache.bytes() <= 200, "budget exceeded: {}", cache.bytes());
        assert_eq!(cache.len(), 2);
    }

    #[test]
    fn the_least_recently_used_entry_is_evicted_first() {
        let cache = RenderCache::new(200);
        let first = RenderKey::new("doc", 0, 1.0);
        let second = RenderKey::new("doc", 1, 1.0);

        cache.insert(first.clone(), page(100));
        cache.insert(second.clone(), page(100));
        // Touch the first so the second becomes the least recently used.
        cache.get(&first);
        cache.insert(RenderKey::new("doc", 2, 1.0), page(100));

        assert!(
            cache.get(&first).is_some(),
            "recently used entry was evicted"
        );
        assert!(cache.get(&second).is_none(), "stale entry survived");
    }

    #[test]
    fn scale_is_part_of_the_key_so_zooming_does_not_serve_the_wrong_raster() {
        let cache = RenderCache::new(1_000);
        cache.insert(RenderKey::new("doc", 0, 1.0), page(10));

        // A different zoom is a different raster: it must miss, and the miss must not
        // disturb the entry already cached.
        assert!(cache.get(&RenderKey::new("doc", 0, 2.0)).is_none());
        assert_eq!(cache.len(), 1);

        cache.insert(RenderKey::new("doc", 0, 2.0), page(10));
        assert_eq!(cache.len(), 2);
    }

    #[test]
    fn a_single_oversized_page_is_still_cached() {
        // Budget 50, one page of 500 bytes: evicting everything else is right, but the
        // just-rendered page must not evict itself, or every draw would re-rasterize.
        let cache = RenderCache::new(50);
        cache.insert(RenderKey::new("doc", 0, 1.0), page(10));
        cache.insert(RenderKey::new("doc", 1, 1.0), page(500));

        assert_eq!(cache.len(), 1);
        assert!(cache.get(&RenderKey::new("doc", 1, 1.0)).is_some());
    }

    #[test]
    fn forgetting_a_document_drops_only_its_pages() {
        let cache = RenderCache::new(10_000);
        cache.insert(RenderKey::new("a", 0, 1.0), page(10));
        cache.insert(RenderKey::new("a", 1, 1.0), page(10));
        cache.insert(RenderKey::new("b", 0, 1.0), page(10));

        cache.forget_document("a");

        assert_eq!(cache.len(), 1);
        assert!(cache.get(&RenderKey::new("a", 0, 1.0)).is_none());
        assert!(cache.get(&RenderKey::new("b", 0, 1.0)).is_some());
        assert_eq!(cache.bytes(), 10);
    }

    #[test]
    fn byte_accounting_returns_to_zero_after_forgetting_everything() {
        let cache = RenderCache::new(10_000);
        for index in 0..4 {
            cache.insert(RenderKey::new("doc", index, 1.0), page(50));
        }
        assert_eq!(cache.bytes(), 200);

        cache.forget_document("doc");
        assert_eq!(cache.bytes(), 0);
        assert!(cache.is_empty());
    }

    #[test]
    fn service_serves_the_second_request_for_a_page_from_cache() {
        if !PdfProvider::pdfium_available() {
            eprintln!("skipping: PDFium not available on this machine");
            return;
        }

        let service = RenderService::new();
        let path = fixture_pdf();

        let first = service.render("doc-1", &path, None, 0, 1.0).unwrap();
        assert_eq!(service.cache().len(), 1);
        let second = service.render("doc-1", &path, None, 0, 1.0).unwrap();

        // Same cached Arc, so the second call did no work at all.
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(service.cache().len(), 1);
    }

    #[test]
    fn pre_rendering_fills_both_neighbours() {
        if !PdfProvider::pdfium_available() {
            eprintln!("skipping: PDFium not available on this machine");
            return;
        }

        let service = RenderService::new();
        let path = fixture_pdf();
        service.render("doc-1", &path, None, 1, 1.0).unwrap();

        let ticket = service.begin_pre_render();
        let rendered = service.prerender_neighbours(ticket, "doc-1", &path, None, 1, 1.0);
        assert_eq!(rendered, 2);

        // Pages 0, 1, 2 are all resident now: the next page turn is a cache hit.
        assert_eq!(service.cache().len(), 3);
        for page in 0..3 {
            assert!(
                service
                    .cache()
                    .get(&RenderKey::new("doc-1", page, 1.0))
                    .is_some(),
                "page {page} should have been pre-rendered"
            );
        }
    }

    #[test]
    fn pre_rendering_at_the_edges_only_fills_what_exists() {
        if !PdfProvider::pdfium_available() {
            eprintln!("skipping: PDFium not available on this machine");
            return;
        }

        let service = RenderService::new();
        let path = fixture_pdf();

        // Page 0 has no previous neighbour, so only page 1 is filled.
        let ticket = service.begin_pre_render();
        let rendered = service.prerender_neighbours(ticket, "doc-1", &path, None, 0, 1.0);
        assert_eq!(rendered, 1);
        assert!(service
            .cache()
            .get(&RenderKey::new("doc-1", 1, 1.0))
            .is_some());

        // Last page (2) has no next neighbour.
        let service = RenderService::new();
        let ticket = service.begin_pre_render();
        let rendered = service.prerender_neighbours(ticket, "doc-1", &path, None, 2, 1.0);
        assert_eq!(rendered, 1);
        assert!(service
            .cache()
            .get(&RenderKey::new("doc-1", 1, 1.0))
            .is_some());
    }

    #[test]
    fn forgetting_a_document_closes_its_handle_too() {
        if !PdfProvider::pdfium_available() {
            eprintln!("skipping: PDFium not available on this machine");
            return;
        }

        let service = RenderService::new();
        let path = fixture_pdf();
        service.render("doc-1", &path, None, 0, 1.0).unwrap();
        assert_eq!(service.open_documents(), 1);

        service.forget_document("doc-1", &path);

        // Both the raster and the open document are gone, so a changed file cannot be
        // served from either cache.
        assert!(service.cache().is_empty());
        assert_eq!(service.open_documents(), 0);
    }

    #[test]
    fn forgetting_a_document_drops_handles_even_when_the_hash_changed() {
        if !PdfProvider::pdfium_available() {
            eprintln!("skipping: PDFium not available on this machine");
            return;
        }

        let service = RenderService::new();
        let path = fixture_pdf();

        // Open under one hash, then discard using a *different* hash, which is what a
        // file edited in place looks like on re-import. A hash-keyed drop would miss.
        service
            .render("doc-1", &path, Some("old-hash"), 0, 1.0)
            .unwrap();
        assert_eq!(service.open_documents(), 1);

        service.forget_document("doc-1", &path);
        assert_eq!(
            service.open_documents(),
            0,
            "stale handle survived a content change"
        );
    }

    #[test]
    fn page_count_goes_through_the_shared_handle() {
        if !PdfProvider::pdfium_available() {
            eprintln!("skipping: PDFium not available on this machine");
            return;
        }

        let service = RenderService::new();
        let path = fixture_pdf();

        assert_eq!(service.page_count(&path, None).unwrap(), 3);
        // The handle is retained for the following render rather than reopened.
        assert_eq!(service.open_documents(), 1);
        assert_eq!(service.page_count(&path, None).unwrap(), 3);
        assert_eq!(service.open_documents(), 1);
    }

    #[test]
    fn a_new_page_request_supersedes_an_in_flight_pre_render() {
        if !PdfProvider::pdfium_available() {
            eprintln!("skipping: PDFium not available on this machine");
            return;
        }

        let service = RenderService::new();
        let path = fixture_pdf();

        // A pre-render ticket is invalidated by the next foreground request, so a user
        // paging quickly does not accumulate speculative renders.
        let stale = service.begin_pre_render();
        assert!(service.is_current(stale));

        let fresh = service.begin_pre_render();
        assert!(!service.is_current(stale));
        assert!(service.is_current(fresh));

        // A superseded pre-render stops before rendering anything: the two neighbours
        // of a page the user has already left are not worth the work.
        let rendered = service.prerender_neighbours(stale, "doc-1", &path, None, 1, 1.0);
        assert_eq!(rendered, 0);
        assert!(service.cache().is_empty());
    }
}
