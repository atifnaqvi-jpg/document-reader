//! Shared test scaffolding: fixture paths and a runtime PDF generator.
//!
//! The P1.8 memory assertion needs a document with enough pages to force many evictions
//! from the render cache. Committing a 500-page PDF to the repository would be a large
//! binary that is awkward to review and never changes, so it is built at test runtime
//! from the same hand-rolled template as `tests/fixtures/sample.pdf`
//! (see `scripts/make-fixture-pdf.py`, which this mirrors).

use std::path::PathBuf;

/// Writes a text-layer PDF with `pages` pages to a unique path under the temp dir.
///
/// Deliberately not a PDF library dependency: the file only has to be valid enough for
/// PDFium to open, count and rasterize, and building it here keeps the test hermetic on
/// Windows CI where no system PDF tooling can be assumed.
pub fn write_large_pdf(name: &str, pages: u32) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("docreader-fixtures-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, build_pdf(pages)).unwrap();
    path
}

/// A minimal PDF whose every page carries the token `PAGE <n>` in its text layer.
pub fn build_pdf(pages: u32) -> Vec<u8> {
    // Object 1: catalog, 2: page tree, 3: font, then two objects per page.
    const FONT_ID: u32 = 3;
    let page_ids = |index: u32| 4 + index * 2;

    let mut objects: Vec<Vec<u8>> = Vec::new();
    objects.push(b"<< /Type /Catalog /Pages 2 0 R >>".to_vec());

    let kids: String = (0..pages)
        .map(|index| format!("{} 0 R", page_ids(index)))
        .collect::<Vec<_>>()
        .join(" ");
    objects.push(format!("<< /Type /Pages /Kids [{kids}] /Count {pages} >>").into_bytes());

    objects.push(
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>"
            .to_vec(),
    );

    for index in 0..pages {
        let page_id = page_ids(index);
        let content_id = page_id + 1;
        objects.push(
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
                 /Resources << /Font << /F1 {FONT_ID} 0 R >> >> /Contents {content_id} 0 R >>"
            )
            .into_bytes(),
        );

        let stream = format!(
            "BT /F1 24 Tf 72 700 Td (PAGE {}) Tj ET\n{}",
            index + 1,
            (0..8)
                .map(|line| format!(
                    "BT /F1 10 Tf 72 {} Td (filler text for page {} line {}) Tj ET\n",
                    640 - line * 14,
                    index + 1,
                    line + 1
                ))
                .collect::<String>()
        )
        .into_bytes();
        objects.push(
            [
                format!("<< /Length {} >>\nstream\n", stream.len()).into_bytes(),
                stream,
                b"endstream".to_vec(),
            ]
            .concat(),
        );
    }

    let mut out: Vec<u8> = b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n".to_vec();
    let mut offsets = Vec::with_capacity(objects.len());
    for (number, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", number + 1).as_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
    }

    let xref_at = out.len();
    let count = objects.len() + 1;
    out.extend_from_slice(format!("xref\n0 {count}\n").as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n");
    for offset in &offsets {
        out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!("trailer\n<< /Size {count} /Root 1 0 R >>\nstartxref\n{xref_at}\n%%EOF\n")
            .as_bytes(),
    );
    out
}

/// True when a usable PDFium is present, so render tests can be skipped rather than
/// failing on a machine without the native library.
pub fn pdfium_available() -> bool {
    document_reader_lib::provider::PdfProvider::pdfium_available()
}

/// Skips the calling test's body when PDFium is missing. Returns `true` if the caller
/// should return early.
pub fn skip_without_pdfium() -> bool {
    if pdfium_available() {
        return false;
    }
    eprintln!("skipping: PDFium not available on this machine");
    true
}

/// Current resident set size in bytes, or `None` where it cannot be read.
///
/// Linux only: `/proc/self/statm` is exact and dependency-free. Windows CI cannot report
/// this without pulling in a crate for it, and the memory test's primary assertion (the
/// cache's own byte counter) holds everywhere — this is the secondary, best-effort check.
pub fn resident_bytes() -> Option<usize> {
    let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
    let pages: usize = statm.split_whitespace().nth(1)?.parse().ok()?;
    Some(pages * 4096)
}
