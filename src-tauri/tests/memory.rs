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
//!    fit in it. This is exact on every platform *provided a single rendered page fits
//!    inside the budget*: `RenderCache::insert` deliberately keeps at least one entry
//!    resident, so a raster larger than the entire budget is cached and leaves the
//!    counter above it. The test satisfies that precondition by a wide margin — a page
//!    at `SCALE` is ~48 KB against a 1 MiB budget.
//! 2. Resident memory does not grow in proportion to the number of pages read. This is
//!    best-effort (`/proc/self/statm`, so Linux-only), so it is a tripwire rather than an
//!    exact bound — but the tripwire is calibrated. Both arms were measured: bounded
//!    growth is +0 MiB, and with eviction disabled it is +24 MiB over the same 480 pages,
//!    against a 16 MiB allowance. At the original 120 pages the unbounded arm grew ~5 MiB
//!    and passed, which is why the page count was raised; see `RSS_PAGES`.
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

/// Pages read by the resident-memory test.
///
/// This number is what gives the assertion its power, and it was measured rather than
/// chosen. At 120 pages the signal was the same size as allocator noise (~1-5 MiB): a
/// page of the fixture rasterizes to ~48 KB, so 96 further pages of unbounded caching
/// only add ~5 MiB, which sat inside the allowance and passed with eviction disabled.
/// At 600 pages the two outcomes separate — bounded stays near one budget, unbounded
/// grows by the 480 measured pages at ~48 KB each (~23 MiB) — so "a leak is present"
/// and "the allocator is untidy" are no longer the same number.
const RSS_PAGES: u32 = 600;

/// Slack for an allocator that does not return freed pages promptly. Measured growth on
/// the bounded arm is 1-5 MiB; unbounded growth at `RSS_PAGES` is ~23 MiB.
const ALLOCATOR_SLACK: usize = 16 * 1024 * 1024;

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

    // Two parts: what the cache is allowed to hold, plus allocator slack. The slack is
    // measured, not reasoned about. It used to be 64 MiB, justified by a "~1 MB PNG"
    // figure from the docstring above; the fixture actually renders ~48 KB pages, which
    // put the unbounded case inside the allowance. Disabling eviction passed. See the
    // commit that raised RSS_PAGES for both arms' measurements.
    let allowance = CACHE_BUDGET + ALLOCATOR_SLACK;
    assert!(
        growth < allowance,
        "resident memory grew by {} MiB over {} pages, which is more than the cache \
         budget plus {ALLOCATOR_SLACK} bytes of allocator slack — a per-page leak is the \
         likely cause",
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
