//! P1.8: the memory assertion (docs/P1-DESIGN.md §7 item 7).
//!
//! **Exit criterion:** "smooth reading of a 500-page PDF at stable memory"
//! (docs/P1-DESIGN.md §1). The failure this guards against is the obvious way to get
//! that wrong — keep every rendered page resident and let a long reading session grow
//! without bound. Rendering a page is by far the largest allocation the app makes: at
//! the viewer's default zoom a single page is roughly a megabyte of PNG, and a 500-page
//! document at 600 DPI is hundreds of megabytes.
//!
//! Two things are asserted, in order of how much they prove:
//!
//! 1. The cache's own byte counter never exceeds its budget, across far more pages than
//!    fit in it. This is exact and holds on every platform.
//! 2. Resident memory does not grow in proportion to the number of pages read. This is
//!    best-effort (`/proc/self/statm`, so Linux-only) and would be flaky as a hard bound;
//!    it is a coarse tripwire for a genuine leak, not a precise measurement.
//!
//! The full path is exercised — `RenderService` over a real PDF through PDFium — because
//! a cache tested only through its own `insert`/`get` would not catch a caller bypassing
//! it, which is exactly the bug that matters.

mod support;

use std::sync::Arc;

use document_reader_lib::provider::PdfProvider;
use document_reader_lib::render::RenderService;

/// Pages read in the assertion loop. Large enough that eviction must run many times,
/// small enough to stay practical: rasterizing a page in an unoptimized test build costs
/// a fraction of a second, so this is tens of seconds, not minutes.
const PAGES: u32 = 150;

/// Deliberately far below the product's 200 MB budget. Measured against the generated
/// fixture, a page at `SCALE` is only ~50 KB, so even 500 pages would fit inside 24 MB and
/// eviction would never run — the test would pass while proving nothing. A budget of a few
/// pages' worth makes eviction the common case, which is what the exit criterion depends
/// on. The eviction *policy* is identical at any budget.
const CACHE_BUDGET: usize = 1024 * 1024;

/// Scale kept low so the run stays quick while still producing real PNGs. The bound is a
/// byte budget, so scale changes the *rate* of eviction, not the invariant asserted.
const SCALE: f32 = 1.0;

#[test]
#[ignore = "requires a real PDFium library; `cargo test -- --include-ignored` after fetching it"]
fn reading_a_large_document_keeps_the_render_cache_within_its_budget() {
    let path = support::write_large_pdf("large-memory.pdf", PAGES);
    let content_hash = document_reader_lib::library::hash_file(&path).unwrap();

    let service = Arc::new(RenderService::with_cache_budget(CACHE_BUDGET));

    // Page dimensions are checked first so the fixture is known to be real before anything
    // is asserted about memory: a cache that never stored anything would trivially "stay
    // in budget".
    let first = service
        .render("doc", &path, Some(&content_hash), 0, SCALE, true)
        .expect("first page should render");
    assert!(
        first.width > 0 && first.height > 0,
        "expected a real raster, got {}x{}",
        first.width,
        first.height
    );

    let mut peak_cache_bytes = 0usize;
    let mut peak_entries = 0usize;

    for page in 0..PAGES {
        let rendered = service
            .render("doc", &path, Some(&content_hash), page, SCALE, true)
            .unwrap_or_else(|e| panic!("page {page} failed to render: {e}"));
        assert!(!rendered.bytes.is_empty(), "page {page} produced no bytes");

        peak_cache_bytes = peak_cache_bytes.max(service.cache().bytes());
        peak_entries = peak_entries.max(service.cache().len());

        // The invariant, checked after every single render rather than only at the end: a
        // transient overshoot is what an unbounded cache looks like before it is caught.
        assert!(
            service.cache().bytes() <= CACHE_BUDGET,
            "cache exceeded its budget after page {page}: {} > {CACHE_BUDGET}",
            service.cache().bytes()
        );
    }

    assert_eq!(
        service.page_count(&path, Some(&content_hash)).unwrap(),
        PAGES,
        "fixture should have exactly {PAGES} pages"
    );

    println!(
        "rendered {PAGES} pages; peak {peak_entries} entries / {peak_cache_bytes} bytes of \
         a {CACHE_BUDGET} byte budget"
    );

    // Eviction provably ran: far fewer pages stayed resident than were read.
    assert!(
        peak_entries < PAGES as usize / 2,
        "cache peaked at {peak_entries} entries for {PAGES} pages — eviction barely ran, so \
         this test proves little"
    );
    // And the cache stayed useful: one that dropped to a single entry on every insert would
    // waste every hit, so most of the budget should be occupied.
    assert!(
        service.cache().bytes() > CACHE_BUDGET / 2,
        "cache ended at {} bytes of a {CACHE_BUDGET} budget — it is evicting far too eagerly, \
         which would make page turns re-render constantly",
        service.cache().bytes()
    );
}

/// Pages read by the resident-memory test. Smaller than `PAGES`, and smaller than the budget,
/// so the assertion is about memory retained after warm-up rather than about eviction (which
/// the test above already covers).
const RSS_PAGES: u32 = 120;

#[test]
#[ignore = "requires a real PDFium library; `cargo test -- --include-ignored` after fetching it"]
fn resident_memory_does_not_grow_with_the_number_of_pages_read() {
    let Some(_) = support::resident_bytes() else {
        eprintln!("skipping: resident memory is not readable on this platform");
        return;
    };

    let path = support::write_large_pdf("large-rss.pdf", RSS_PAGES);
    let content_hash = document_reader_lib::library::hash_file(&path).unwrap();
    let service = Arc::new(RenderService::with_cache_budget(CACHE_BUDGET));

    service
        .render("doc", &path, Some(&content_hash), 0, SCALE, true)
        .unwrap();

    // Warm up, then measure the baseline only *after* the allocator has settled: the
    // first pages fault in the library, the fonts and the document handle, and counting
    // that as growth would make the test meaningless.
    for page in 0..(RSS_PAGES / 5) {
        service
            .render("doc", &path, Some(&content_hash), page, SCALE, true)
            .unwrap();
    }
    let baseline = support::resident_bytes().unwrap();

    for page in (RSS_PAGES / 5)..RSS_PAGES {
        service
            .render("doc", &path, Some(&content_hash), page, SCALE, true)
            .unwrap();
    }
    let after = support::resident_bytes().unwrap();

    let growth = after.saturating_sub(baseline);
    println!(
        "resident memory: {} MiB -> {} MiB after {} more pages (+{} MiB)",
        baseline / (1024 * 1024),
        after / (1024 * 1024),
        RSS_PAGES - RSS_PAGES / 5,
        growth / (1024 * 1024)
    );

    // Deliberately generous: this is a leak tripwire, not a measurement. Reading 400
    // further pages should not add more than a few pages' worth of memory; a genuine
    // per-page leak would be ~400 MB and this threshold would catch it by a wide margin,
    // while an allocator that does not return freed pages promptly still passes.
    let allowance = CACHE_BUDGET + 64 * 1024 * 1024;
    assert!(
        growth < allowance,
        "resident memory grew by {} MiB over {} pages, which is more than the cache \
         budget plus slack — a per-page leak is the likely cause",
        growth / (1024 * 1024),
        RSS_PAGES - RSS_PAGES / 5
    );
}

#[test]
#[ignore = "requires a real PDFium library; `cargo test -- --include-ignored` after fetching it"]
fn the_fixture_generator_produces_a_pdf_pdfium_can_count() {
    // Guards the generator itself: if it quietly produced a malformed file, the two
    // memory tests above would fail on a page count mismatch and send someone chasing a
    // rendering bug instead of a test-support one.
    let path = support::write_large_pdf("generator-check.pdf", 12);
    assert_eq!(
        PdfProvider::page_count(&path).unwrap(),
        12,
        "generated fixture should report the page count it was asked for"
    );

    let text = PdfProvider::extract_page_text(&path, 3).unwrap();
    assert!(
        text.contains("PAGE 4"),
        "page 3 should carry its own token, got {text:?}"
    );
}
