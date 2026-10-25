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

use crate::editor::geometry::{Point2, RunFrame};
use crate::model::AppError;

/// One text object as PDFium sees it, for the picker's display and hit-testing.
///
/// This is **not** the run's identity (T4). The address the write path uses is the lopdf
/// `(op_index, operand_path)` in [`crate::editor::textlocate`]; this struct only carries what
/// the UI needs to show a target and what pairing needs to match it to that address. Since
/// P2.4.1 T17 that match is made **by geometry** — the box that contains the lopdf run's
/// text-space origin — because PDFium reconstructs text at a different granularity (it joins
/// a per-glyph `TJ` array into one run) and string equality therefore pairs the wrong things.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PdfiumRun {
    /// The text PDFium reconstructed, for display only.
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
    let page = document.pages().get(page as u16).map_err(|e| {
        AppError::Other(format!(
            "page {page} unavailable on {}: {e}",
            path.display()
        ))
    })?;

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

/// One character of PDFium's **rendered** text grid (P2.4.3 T23a).
///
/// The char grid is the span oracle: it is present for every font class that can be drawn —
/// including the PowerPoint subsets whose in-file metrics `FontMetrics` cannot read (see
/// `docs/P2.4.3-FINDINGS.md`) — because it is measured from the *rendered* page rather than
/// from the font program.
#[derive(Debug, Clone, PartialEq)]
pub struct PdfiumChar {
    /// The Unicode scalar PDFium reports, or `None` when the byte has no mapping.
    pub unicode: Option<char>,
    /// True for a character PDFium synthesised (a line break, or the space it inserts at a
    /// kerning seam). Generated characters occupy grid slots but no run's bytes.
    pub generated: bool,
    /// The **advance** box (`FPDFText_GetLooseCharBox`): the pen moves these, so consecutive
    /// characters abut with no gap. This is the box spans are measured from — the tight box
    /// is *ink*, and summing ink would under-measure a proportional face by the side bearings.
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
    /// The ink box (`FPDFText_GetCharBox`), carried for diagnostics only.
    pub ink_x0: f32,
    pub ink_y0: f32,
    pub ink_x1: f32,
    pub ink_y1: f32,
    /// The pen origin, which is what pairing (T17) compares against a run's text-space origin.
    pub origin_x: f32,
    pub origin_y: f32,
    pub font_size: f32,
}

/// PDFium's whole-page character grid (P2.4.3 T23a).
#[derive(Debug, Clone, PartialEq)]
pub struct PdfiumCharGrid {
    /// PDFium's reconstructed page text, exactly as `page.text().all()` returns it. Kept so a
    /// caller can search it with the same whitespace rules [`Self::locate`] applies.
    pub text: String,
    pub chars: Vec<PdfiumChar>,
}

/// The character range one piece of text occupies in the grid, and its measured span.
///
/// `x0`/`x1` are the union of the matched characters' advance boxes **projected onto the
/// run's advance axis** (P2.7.4 §4). For horizontal text that is the page's `x` extent, so
/// every existing consumer reads the number it always did; for a turned run it is the extent
/// *along the line*, which is the only width a width gate may compare. The raw `y0`/`y1` are
/// kept so a box's perpendicular extent can be projected onto the up axis instead.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CharSpan {
    /// First and last grid indices of the match.
    pub first: usize,
    pub last: usize,
    /// The span's start and end along the advance axis, in points.
    pub x0: f32,
    pub x1: f32,
    /// The union of the matched characters' advance boxes' perpendicular extent.
    pub y0: f32,
    pub y1: f32,
}

impl CharSpan {
    /// The span width in points, along the run's advance axis.
    pub fn width(&self) -> f32 {
        self.x1 - self.x0
    }

    /// The span's extent along the run's up axis.
    pub fn height(&self) -> f32 {
        self.y1 - self.y0
    }
}

/// Collapses every run of whitespace to a single space and trims the ends.
///
/// Both sides of [`PdfiumCharGrid::locate`]'s search go through this, because PDFium
/// reconstructs the same line differently depending on how the producer drew it: a `Tj` per
/// kerning group leaves no separator, a real line break arrives as a generated `\r\n`, and a
/// space drawn as kerning arrives as a *generated* space. Normalising whitespace is what lets
/// one rule find text drawn any of those ways. It is deliberately not a general text fold —
/// nothing else is normalised, so a search can never match text the page does not carry.
fn normalise_whitespace(chars: impl Iterator<Item = char>) -> Vec<char> {
    let mut out: Vec<char> = Vec::new();
    let mut pending_space = false;
    for ch in chars {
        if ch.is_whitespace() {
            pending_space = !out.is_empty();
            continue;
        }
        if pending_space {
            out.push(' ');
            pending_space = false;
        }
        out.push(ch);
    }
    out
}

impl PdfiumCharGrid {
    /// Finds `text` in the grid, from the first character whose advance box starts at or after
    /// `x_min` on the baseline in `band`, returning the matched range and its measured span.
    ///
    /// This is T23a's walk. The search is anchored in two places at once, and both are needed:
    ///
    /// * **the band** (`band.0..=band.1`, the paired box's y-range widened by the pairing
    ///   tolerance) keeps the walk on one baseline, so a run never measures text on the line
    ///   above or below; and
    /// * **`from`** (the head pair's start index) keeps a run that reads the same as an earlier
    ///   one on the page — a repeated word is ordinary — from measuring the *earlier* occurrence.
    ///
    /// Every character of the match must satisfy the band and the `x_min` window, so a partial
    /// match that wanders off the line is rejected rather than half-counted.
    pub fn locate(
        &self,
        text: &str,
        from: usize,
        band: (f32, f32),
        x_min: f32,
    ) -> Option<CharSpan> {
        self.locate_in_frame(text, from, band, x_min, RunFrame::PAGE)
    }

    /// [`Self::locate`] measured in a run's own frame (P2.7.4).
    ///
    /// `from` and `x_min` are indices and positions *along* the advance axis; `band` is the
    /// window *across* it. The returned [`CharSpan`] carries along-axis coordinates. For
    /// [`RunFrame::PAGE`] this is exactly [`Self::locate`]'s arithmetic — the frame's
    /// projections are `x` and `y` — so no horizontal caller's result moves.
    pub fn locate_in_frame(
        &self,
        text: &str,
        from: usize,
        band: (f32, f32),
        x_min: f32,
        frame: RunFrame,
    ) -> Option<CharSpan> {
        let needle = normalise_whitespace(text.chars());
        if needle.is_empty() {
            return None;
        }

        // The grid's own searchable form, with a map from normalised position back to grid index.
        let mut haystack: Vec<char> = Vec::new();
        let mut map: Vec<usize> = Vec::new();
        let mut pending_space = false;
        for (index, ch) in self.chars.iter().enumerate() {
            match ch.unicode {
                Some(c) if !c.is_whitespace() => {
                    if pending_space && !haystack.is_empty() {
                        haystack.push(' ');
                        map.push(index);
                    }
                    pending_space = false;
                    haystack.push(c);
                    map.push(index);
                }
                _ => pending_space = !haystack.is_empty(),
            }
        }

        // Start searching at the first normalised position whose grid index is `>= from`.
        let start = map
            .iter()
            .position(|index| *index >= from)
            .unwrap_or(map.len());

        let mut position = start;
        while position + needle.len() <= haystack.len() {
            if haystack[position..position + needle.len()] == needle[..] {
                let indices = &map[position..position + needle.len()];
                let on_band = indices.iter().all(|index| {
                    let ch = &self.chars[*index];
                    let origin = Point2::new(ch.origin_x, ch.origin_y);
                    (band.0..=band.1).contains(&frame.across(origin))
                        && Self::along_min(ch, frame) >= x_min
                });
                if on_band {
                    return Some(Self::span_of(indices, &self.chars, frame));
                }
            }
            position += 1;
        }
        None
    }

    /// Builds the span for a run of grid indices, projected into `frame`.
    pub fn span_of(indices: &[usize], chars: &[PdfiumChar], frame: RunFrame) -> CharSpan {
        let mut span = CharSpan {
            first: indices[0],
            last: *indices.last().expect("a span has at least one character"),
            x0: f32::INFINITY,
            x1: f32::NEG_INFINITY,
            y0: f32::INFINITY,
            y1: f32::NEG_INFINITY,
        };
        for index in indices {
            let corners = corners(&chars[*index]);
            span.x0 = span.x0.min(
                corners
                    .iter()
                    .map(|p| frame.along(*p))
                    .fold(f32::INFINITY, f32::min),
            );
            span.x1 = span.x1.max(
                corners
                    .iter()
                    .map(|p| frame.along(*p))
                    .fold(f32::NEG_INFINITY, f32::max),
            );
            span.y0 = span.y0.min(
                corners
                    .iter()
                    .map(|p| frame.across(*p))
                    .fold(f32::INFINITY, f32::min),
            );
            span.y1 = span.y1.max(
                corners
                    .iter()
                    .map(|p| frame.across(*p))
                    .fold(f32::NEG_INFINITY, f32::max),
            );
        }
        span
    }

    /// The near edge of a character's advance box along `frame`'s advance axis.
    pub fn along_min(ch: &PdfiumChar, frame: RunFrame) -> f32 {
        corners(ch)
            .iter()
            .map(|p| frame.along(*p))
            .fold(f32::INFINITY, f32::min)
    }

    /// The far edge of a character's advance box along `frame`'s advance axis.
    pub fn along_max(ch: &PdfiumChar, frame: RunFrame) -> f32 {
        corners(ch)
            .iter()
            .map(|p| frame.along(*p))
            .fold(f32::NEG_INFINITY, f32::max)
    }

}

/// The four corners of a character's advance box.
///
/// Projecting all four — rather than assuming which edge is "near" — is what makes the
/// projection correct for a turned or skewed axis, where any corner can be the extreme.
fn corners(ch: &PdfiumChar) -> [Point2; 4] {
    [
        Point2::new(ch.x0, ch.y0),
        Point2::new(ch.x0, ch.y1),
        Point2::new(ch.x1, ch.y0),
        Point2::new(ch.x1, ch.y1),
    ]
}

/// Reads PDFium's whole-page character grid (P2.4.3 T23a).
///
/// Read-only and routed through the same handle cache a render uses. The grid is what makes a
/// span measurable for a font whose program carries no metrics: the renderer has already laid
/// the glyphs out, so their boxes exist whatever the font file contains.
pub fn char_grid(
    document: &PdfDocument<'_>,
    page: u32,
    path: &Path,
) -> Result<PdfiumCharGrid, AppError> {
    let page = document.pages().get(page as u16).map_err(|e| {
        AppError::Other(format!(
            "page {page} unavailable on {}: {e}",
            path.display()
        ))
    })?;

    let text = page
        .text()
        .map_err(|e| AppError::Other(format!("text extraction failed: {e}")))?;
    let page_text = text.all();

    let mut chars = Vec::new();
    for character in text.chars().iter() {
        let Ok(loose) = character.loose_bounds() else {
            continue;
        };
        let Ok(ink) = character.tight_bounds() else {
            continue;
        };
        let Ok((origin_x, origin_y)) = character.origin() else {
            continue;
        };
        chars.push(PdfiumChar {
            unicode: character.unicode_char(),
            generated: character.is_generated().unwrap_or(false),
            x0: loose.left().value,
            y0: loose.bottom().value,
            x1: loose.right().value,
            y1: loose.top().value,
            ink_x0: ink.left().value,
            ink_y0: ink.bottom().value,
            ink_x1: ink.right().value,
            ink_y1: ink.top().value,
            origin_x: origin_x.value,
            origin_y: origin_y.value,
            font_size: character.unscaled_font_size().value,
        });
    }

    Ok(PdfiumCharGrid {
        text: page_text,
        chars,
    })
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
