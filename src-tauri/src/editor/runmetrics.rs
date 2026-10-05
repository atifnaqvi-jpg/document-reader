//! PDFium's read-only half of text editing (P2.4 T1/T3d/T4).
//!
//! lopdf is the only writer (T1). PDFium's job here is exactly two things, both
//! read-only:
//!
//! * **Extract** the text objects on a page — their text, font, size and bounds — so the
//!   picker can draw a box over something the user can click, and so each PDFium run can
//!   be matched to the lopdf run that will actually be edited.
//! * **Measure** a candidate replacement string in the same font at the same size, which
//!   is what gate (d) compares against the run's advance width.
//!
//! Measuring with PDFium rather than a hand-rolled `/Widths` lookup is deliberate: the
//! width the *renderer* will draw is the width that matters, and re-deriving it from the
//! font dictionary would drift from what PDFium actually does (composite fonts, kerning,
//! `/Widths` vs `/W`). PDFium renders; PDFium therefore measures. It just never saves.
//!
//! Nothing here mutates a document. The `set_text`/`as_text_object_mut` surface exists in
//! `pdfium-render`, and calling it would make PDFium a second writer (T1/Q3) — so this
//! module never reaches for it, and the final grep check in CI asserts no save call
//! either.

use std::path::Path;

use pdfium_render::prelude::*;

use crate::model::AppError;

/// One text object as PDFium sees it, for the picker's display and hit-testing.
///
/// This is **not** the run's identity (T4). The address the write path uses is the lopdf
/// `(op_index, operand_path)` in [`crate::editor::textlocate`]; this struct only carries
/// what the UI needs to show a target and what the caller needs to match it to that
/// address (the displayed text, and its position on the page).
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PdfiumRun {
    /// The text PDFium reconstructed, for display and for matching to a lopdf run.
    pub text: String,
    /// Font family name as PDFium reports it (`Helvetica`), for the refusal message.
    pub font_name: String,
    /// Font size in points.
    pub font_size: f32,
    /// Bounding box in PDF points, origin bottom-left, matching
    /// [`crate::editor::geometry::PdfRect`] so the overlay can reuse its conversion.
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
    /// Total advance width of the run in points, from the bounding box. This is the
    /// `old_width` gate (d) measures against.
    pub width: f32,
}

impl PdfiumRun {
    /// Matches a PDFium run to a lopdf run by displayed text.
    ///
    /// Deliberately text equality, not position: PDFium joins adjacent objects and
    /// applies `/ToUnicode`, so a byte-for-byte position match is not available. Text is
    /// what the user saw and what T4 re-validates at write time, so it is the honest
    /// join key. When two runs share text, the caller disambiguates by ordering.
    pub fn matches_text(&self, text: &str) -> bool {
        self.text == text
    }
}

/// Reads every text object on one page of a PDF, in PDFium's order.
///
/// `Ok(empty)` is a valid answer: a scanned page has no text objects, and the picker
/// simply has nothing to offer. Only a genuinely unopenable document is an error.
///
/// This opens its own handle cache, so it takes the same [`PdfiumGate`] a render does and
/// cannot race a concurrent rasterization (P2-D8/F8). The product path goes through
/// `RenderService::page_runs`, which reuses the process-wide cache; this convenience is
/// for tests and one-off callers that have no `RenderService`.
pub fn extract_runs(
    path: &Path,
    page: u32,
    content_hash: Option<&str>,
) -> Result<Vec<PdfiumRun>, AppError> {
    crate::provider::DocumentHandles::new().with_document(path, content_hash, |document| {
        page_runs(document, page, path)
    })
}

/// The body of [`extract_runs`], split out so the tests can exercise it against an
/// already-open document instead of a path.
pub fn page_runs(
    document: &PdfDocument<'_>,
    page: u32,
    path: &Path,
) -> Result<Vec<PdfiumRun>, AppError> {
    let page = document
        .pages()
        .get(page as u16)
        .map_err(|e| AppError::Other(format!("page {page} unavailable on {}: {e}", path.display())))?;

    let mut runs = Vec::new();
    for object in page.objects().iter() {
        let Some(text_object) = object.as_text_object() else {
            continue;
        };
        let text = text_object.text();
        if text.is_empty() {
            continue;
        }
        let bounds = match object.bounds() {
            Ok(bounds) => bounds,
            // An object with no bounds cannot be hit-tested or measured; skipping it is
            // the honest choice rather than inventing a zero rect that would sit on top
            // of the page origin.
            Err(_) => continue,
        };
        runs.push(PdfiumRun {
            text,
            font_name: text_object.font().family(),
            font_size: text_object.unscaled_font_size().value,
            x0: bounds.left().value,
            y0: bounds.bottom().value,
            x1: bounds.right().value,
            y1: bounds.top().value,
            width: bounds.width().value,
        });
    }
    Ok(runs)
}

/// Measures `text` in `font_name` at `font_size`, in PDF points (T3d).
///
/// Done by building a detached text object in PDFium's own model and reading its bounds —
/// the same machinery the renderer uses, so the number matches what will be drawn. The
/// object is never inserted into the document and the document is never saved (T1); it
/// exists only long enough to be measured and dropped.
///
/// The font is resolved by family from the page's existing fonts, falling back to a
/// built-in of the same family. A caller that cannot name the run's font cannot have a
/// trustworthy measurement, so an unknown font is an error rather than a guess.
pub fn measure(
    document: &PdfDocument<'_>,
    page: u32,
    font_name: &str,
    font_size: f32,
    text: &str,
) -> Result<f32, AppError> {
    let page = document
        .pages()
        .get(page as u16)
        .map_err(|e| AppError::Other(format!("page {page} unavailable: {e}")))?;

    // `""` measures zero, which is a legitimate answer for an empty replacement.
    if text.is_empty() {
        return Ok(0.0);
    }

    // The font is taken from a text object *already on the page* whose family matches,
    // rather than looked up in an abstract font table: that is the only handle that
    // carries the document's own font program, and therefore the only measurement that
    // reflects what the renderer will draw.
    let token = find_font_token(&page, font_name).ok_or_else(|| {
        // Unreachable on the real path: `font_name` is read back from a text object on
        // this very page. It can only happen if the page changed under us, and a
        // measurement in the wrong font would silently mis-gate the edit — so refuse
        // rather than substitute a built-in of unknown metrics.
        AppError::Other(format!(
            "the font {font_name:?} is no longer on this page; reopen the page and try again"
        ))
    })?;

    let object = PdfPageTextObject::new(document, text, token, PdfPoints::new(font_size))
        .map_err(|e| AppError::Other(format!("could not measure text: {e}")))?;
    let bounds = PdfPageObjectCommon::bounds(&object)
        .map_err(|e| AppError::Other(format!("measured text has no bounds: {e}")))?;

    Ok(bounds.width().value)
}

/// Finds the token of the page font whose family matches `font_name`, if any.
///
/// An explicit indexed loop rather than an iterator chain: `as_text_object()` borrows the
/// `PdfPageObject` it is called on, so a `filter_map` closure that owns the object cannot
/// return the borrow. Getting each object into a named local keeps the borrow alive for as
/// long as it is used.
fn find_font_token(page: &PdfPage<'_>, font_name: &str) -> Option<PdfFontToken> {
    let objects = page.objects();
    for index in 0..objects.len() {
        let Ok(object) = objects.get(index) else {
            continue;
        };
        let Some(text_object) = object.as_text_object() else {
            continue;
        };
        if family_matches(&text_object.font(), font_name) {
            return Some(text_object.font().token());
        }
    }
    None
}

/// Whether a document font's family is the one the run was drawn with.
///
/// Tolerant on purpose: PDFium renames families (`Helvetica` comes back as
/// `Chrom Sans OTF`), so an exact comparison would miss the very fonts being measured.
/// A substring match in either direction catches both the rename and a name that carries
/// a subset prefix (`ABCDEF+Helvetica`) or a style suffix (`Helvetica-Bold`).
fn family_matches(font: &PdfFont<'_>, font_name: &str) -> bool {
    let wanted = font_name.trim().to_ascii_lowercase();
    if wanted.is_empty() {
        return false;
    }
    let family = font.family().trim().to_ascii_lowercase();
    family == wanted || family.contains(&wanted) || wanted.contains(&family)
}
