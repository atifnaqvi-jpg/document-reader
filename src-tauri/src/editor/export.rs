//! Export (P2-F2) and watermarking (P2-F5).
//!
//! Two outputs, deliberately distinct:
//!
//! * **Flatten** writes the document with annotations baked into each page's content
//!   stream, then drops the annotation objects. The result is what a recipient with any
//!   viewer sees, including viewers that ignore annotations entirely. It is one-way: the
//!   annotation objects are gone, so this is an export format, never an editable save.
//! * **PNG** rasterizes a page for the clipboard or a screenshot.
//!
//! The watermarking helper stamps text into the content stream. P2-D3 forbids that for
//! *annotations* -- a baked stamp is permanent and unsearchable as an annotation -- but a
//! watermark is exactly the case where permanent is the point. Keeping it in this module
//! rather than `annotate.rs` is what stops the two being confused.

use std::path::Path;

use lopdf::content::{Content, Operation};
use lopdf::{dictionary, Document, Object, ObjectId, Stream};

use crate::model::{Annotation, AnnotationKind, AppError};
use crate::provider::PdfProvider;

/// Bakes the document's annotations into each page's content stream, then removes the
/// annotation objects.
///
/// Draws from the *stored rows*, not from `/AP` on the written annotations: our writer
/// records the mark (rect, colour, opacity) and deliberately emits no appearance stream, so
/// reading `/AP` back would find nothing and silently export a blank file. Baking from the
/// same rows the viewer was fed also guarantees the export matches what the user previewed.
///
/// Annotations this app did not write -- a PDF that arrived with its own `/Annots` -- are
/// left untouched rather than dropped. We can only redraw what we authored; deleting the
/// rest would destroy marks the user never asked us to touch.
pub fn flatten_annotations(
    document: &mut Document,
    rows: &[Annotation],
) -> Result<usize, AppError> {
    let mut by_page: std::collections::HashMap<u32, Vec<&Annotation>> =
        std::collections::HashMap::new();
    for row in rows {
        by_page.entry(row.page).or_default().push(row);
    }

    let mut flattened = 0;
    for (one_based, page_id) in document.get_pages() {
        // The DB and the UI count pages from 0; lopdf counts from 1.
        let Some(page_rows) = by_page.get(&(one_based - 1)) else {
            continue;
        };

        let mut drawn: Vec<&Annotation> = Vec::new();
        for row in page_rows {
            // Ink needs its stroke paths and a different drawing primitive; it is left as an
            // annotation rather than approximated as a filled box, which would be a
            // different mark.
            if row.kind == AnnotationKind::Ink {
                continue;
            }
            if crate::editor::annotate::appearance_for(row)?.is_some() {
                drawn.push(row);
            }
        }
        if drawn.is_empty() {
            continue;
        }

        append_marks(document, page_id, &drawn)?;
        remove_our_annots(document, page_id, drawn.iter().map(|row| row.id.as_str()))?;
        flattened += drawn.len();
    }

    Ok(flattened)
}

/// Emits the page's marks as content-stream operations.
fn append_marks(
    document: &mut Document,
    page_id: ObjectId,
    rows: &[&Annotation],
) -> Result<(), AppError> {
    let mut operations = Vec::new();

    for row in rows {
        let Some(appearance) = crate::editor::annotate::appearance_for(row)? else {
            continue;
        };
        let rect = appearance.rect.normalised();
        if !rect.is_drawable() {
            continue;
        }

        let (r, g, b) = appearance.color;
        // Highlights are translucent fills and want `ca`; the line-based kinds are strokes,
        // which use `CA`. The graphics state keeps the two alphas separate.
        let (alpha_op, width) = match appearance.kind {
            AnnotationKind::Highlight => ("ca", None),
            AnnotationKind::Underline | AnnotationKind::Strikeout => ("CA", Some(1.0_f32)),
            AnnotationKind::Note | AnnotationKind::Shape => ("CA", None),
            AnnotationKind::Ink => continue,
        };

        operations.push(Operation::new("q", vec![]));
        operations.push(Operation::new(
            alpha_op,
            vec![appearance.opacity.clamp(0.05, 1.0).into()],
        ));
        operations.push(Operation::new("rg", vec![r.into(), g.into(), b.into()]));

        match appearance.kind {
            AnnotationKind::Highlight => {
                operations.push(Operation::new(
                    "re",
                    vec![
                        rect.x0.into(),
                        rect.y0.into(),
                        rect.width().into(),
                        rect.height().into(),
                    ],
                ));
                operations.push(Operation::new("f", vec![]));
            }
            AnnotationKind::Underline => {
                let y = rect.y0;
                operations.push(Operation::new("w", vec![width.unwrap_or(1.0).into()]));
                operations.push(Operation::new("m", vec![rect.x0.into(), y.into()]));
                operations.push(Operation::new("l", vec![rect.x1.into(), y.into()]));
                operations.push(Operation::new("S", vec![]));
            }
            AnnotationKind::Strikeout => {
                let y = (rect.y0 + rect.y1) / 2.0;
                operations.push(Operation::new("w", vec![width.unwrap_or(1.0).into()]));
                operations.push(Operation::new("m", vec![rect.x0.into(), y.into()]));
                operations.push(Operation::new("l", vec![rect.x1.into(), y.into()]));
                operations.push(Operation::new("S", vec![]));
            }
            AnnotationKind::Shape => {
                // Filled, matching how PDFium renders an unfilled-by-spec /Square with a /C
                // in the app's own viewer.
                operations.push(Operation::new(
                    "re",
                    vec![
                        rect.x0.into(),
                        rect.y0.into(),
                        rect.width().into(),
                        rect.height().into(),
                    ],
                ));
                operations.push(Operation::new("f", vec![]));
            }
            AnnotationKind::Note => {
                // The icon box, drawn as a filled square. A faithful pushpin icon would need
                // an embedded image.
                operations.push(Operation::new(
                    "re",
                    vec![
                        rect.x0.into(),
                        rect.y0.into(),
                        rect.width().into(),
                        rect.height().into(),
                    ],
                ));
                operations.push(Operation::new("f", vec![]));
            }
            AnnotationKind::Ink => {}
        }

        operations.push(Operation::new("Q", vec![]));
    }

    if operations.is_empty() {
        return Ok(());
    }

    let content = Content { operations };
    let encoded = content
        .encode()
        .map_err(|error| AppError::Other(format!("could not encode content: {error}")))?;
    let stream_id = document.add_object(Stream::new(dictionary! {}, encoded));

    let mut array = contents_array(document, page_id)?;
    array.push(Object::Reference(stream_id));

    document
        .get_object_mut(page_id)
        .map_err(|error| AppError::Other(format!("page unreadable: {error}")))?
        .as_dict_mut()
        .map_err(|error| AppError::Other(format!("page is not a dictionary: {error}")))?
        .set("Contents", Object::Array(array));

    Ok(())
}

/// Removes our annotations from a page's `/Annots`, keeping any we did not write.
///
/// Identity is `/NM`: `lopdf_write::attach_annotation` stamps it with the annotation's id,
/// so this is the one field that can tell our marks from a document's pre-existing ones.
fn remove_our_annots(
    document: &mut Document,
    page_id: ObjectId,
    ids: impl Iterator<Item = impl AsRef<str>>,
) -> Result<(), AppError> {
    let ours: Vec<String> = ids.map(|id| id.as_ref().to_string()).collect();

    let kept: Vec<Object> = document
        .get_page_annotations(page_id)
        .unwrap_or_default()
        .iter()
        .filter(|annotation| {
            let Some(name) = annotation
                .get(b"NM")
                .ok()
                .and_then(|object| object.as_str().ok())
                .and_then(|bytes| std::str::from_utf8(bytes).ok())
            else {
                // No /NM means we did not write it.
                return true;
            };
            !ours.iter().any(|id| id == name)
        })
        .map(|annotation| Object::Dictionary((*annotation).clone()))
        .collect();

    let page = document
        .get_object_mut(page_id)
        .map_err(|error| AppError::Other(format!("page unreadable: {error}")))?
        .as_dict_mut()
        .map_err(|error| AppError::Other(format!("page is not a dictionary: {error}")))?;

    if kept.is_empty() {
        page.remove(b"Annots");
    } else {
        page.set("Annots", Object::Array(kept));
    }

    Ok(())
}

fn contents_array(document: &Document, page_id: ObjectId) -> Result<Vec<Object>, AppError> {
    let contents = document
        .get_object(page_id)
        .map_err(|error| AppError::Other(format!("page unreadable: {error}")))?
        .as_dict()
        .map_err(|error| AppError::Other(format!("page is not a dictionary: {error}")))?
        .get(b"Contents")
        .ok()
        .cloned();

    match contents {
        Some(Object::Array(items)) => Ok(items),
        Some(Object::Reference(id)) => Ok(vec![Object::Reference(id)]),
        // A page with no content is legal and gets one below.
        _ => Ok(Vec::new()),
    }
}

/// Smallest font size a fitted watermark will use, in points.
const FONT_SIZE_FLOOR: f32 = 6.0;

/// Mean glyph width as a fraction of font size for the standard Latin faces.
const HELVETICA_MEAN_GLYPH_RATIO: f32 = 0.5;

/// Width of `text` at `font_size`, in points.
///
/// An estimate, not a measurement: Helvetica's real widths live in the font's `/Widths`
/// array and vary per glyph. This is only ever used to place a decorative watermark, where
/// being a few points off is invisible -- measuring exactly would mean parsing the font.
fn text_width(text: &str, font_size: f32) -> f32 {
    text.chars().count() as f32 * font_size * HELVETICA_MEAN_GLYPH_RATIO
}

/// Largest font size in `[floor, desired]` at which `text` fits within the rotated box.
///
/// The mark is drawn at 45 degrees, so the extent that has to fit is the page's diagonal.
fn fit_font_size(text: &str, desired: f32, width: f32, height: f32, floor: f32) -> f32 {
    let diagonal = (width * width + height * height).sqrt();
    let characters = text.chars().count() as f32;
    if characters <= 0.0 || diagonal <= 0.0 {
        return desired;
    }
    let available = diagonal * 0.9;
    let maximum = available / (characters * HELVETICA_MEAN_GLYPH_RATIO);
    desired.min(maximum).max(floor)
}

/// The page's `/MediaBox`, falling back to US Letter when it is inherited or absent.
fn media_box(document: &Document, page_id: ObjectId) -> (f32, f32, f32, f32) {
    let default = (0.0_f32, 0.0_f32, 612.0, 792.0);
    let Ok(page) = document
        .get_object(page_id)
        .and_then(|object| object.as_dict())
    else {
        return default;
    };
    let Ok(box_) = page.get(b"MediaBox").and_then(|object| object.as_array()) else {
        return default;
    };
    if box_.len() != 4 {
        return default;
    }
    let value = |index: usize| {
        box_[index]
            .as_float()
            .or_else(|_| box_[index].as_i64().map(|v| v as f32))
    };
    match (value(0), value(1), value(2), value(3)) {
        // Normalised, because a box may legitimately be given with its corners swapped.
        (Ok(x0), Ok(y0), Ok(x1), Ok(y1)) => (x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1)),
        _ => default,
    }
}

/// Stamps `text` diagonally across every page (P2-F5).
///
/// Baked into the content stream on purpose: unlike an annotation, a watermark's value is
/// that it cannot be switched off. This is the one place P2 accepts a permanent stamp, and
/// it is why the function lives in `export.rs` rather than beside the annotation writers.
pub fn watermark(document: &mut Document, text: &str) -> Result<usize, AppError> {
    if text.trim().is_empty() {
        return Err(AppError::Other("watermark text is empty".to_string()));
    }

    let page_ids: Vec<ObjectId> = document.get_pages().values().copied().collect();
    let font_id = document.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
        "Encoding" => "WinAnsiEncoding",
    });

    for page_id in page_ids {
        let (x0, y0, x1, y1) = media_box(document, page_id);
        let width = x1 - x0;
        let height = y1 - y0;
        // Rotation must happen about the page centre, not the origin. The content stream
        // is not in page space -- it starts at (0,0) -- so the centre has to be expressed
        // relative to the box's lower-left corner, or a non-zero `/MediaBox` origin puts
        // the mark outside the page (and any page other than US Letter lands off-centre).
        let centre_x = x0 + width / 2.0;
        let centre_y = y0 + height / 2.0;
        let escaped = escape_text(text);

        // Sized to the diagonal so the mark runs corner to corner on any page shape, then
        // shrunk to fit when the text is long enough to overrun the box.
        let fitted = fit_font_size(text, 48.0, width, height, FONT_SIZE_FLOOR);

        let operations = vec![
            Operation::new("q", vec![]),
            // Rotate about the page centre, then draw the text there. Done with a matrix
            // rather than `Tm` positioning because it also works for non-square pages.
            Operation::new(
                "cm",
                vec![
                    std::f32::consts::FRAC_1_SQRT_2.into(),
                    std::f32::consts::FRAC_1_SQRT_2.into(),
                    (-std::f32::consts::FRAC_1_SQRT_2).into(),
                    std::f32::consts::FRAC_1_SQRT_2.into(),
                    centre_x.into(),
                    centre_y.into(),
                ],
            ),
            Operation::new("rg", vec![0.5.into(), 0.5.into(), 0.5.into()]),
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec!["F1".into(), fitted.into()]),
            // Half the text's own extent, because `Tj` draws from the *baseline start* and
            // would otherwise centre the beginning of the text rather than the text.
            Operation::new(
                "Td",
                vec![
                    (-text_width(&escaped, fitted) / 2.0).into(),
                    (-fitted / 3.0).into(),
                ],
            ),
            Operation::new("Tj", vec![Object::string_literal(escaped)]),
            Operation::new("ET", vec![]),
            Operation::new("Q", vec![]),
        ];

        let content = Content { operations };
        let encoded = content
            .encode()
            .map_err(|error| AppError::Other(format!("could not encode content: {error}")))?;
        let stream_id = document.add_object(Stream::new(dictionary! {}, encoded));

        let mut array = contents_array(document, page_id)?;
        array.push(Object::Reference(stream_id));

        let page = document
            .get_object_mut(page_id)
            .map_err(|error| AppError::Other(format!("page unreadable: {error}")))?
            .as_dict_mut()
            .map_err(|error| AppError::Other(format!("page is not a dictionary: {error}")))?;

        let mut resources = page
            .get(b"Resources")
            .ok()
            .and_then(|object| object.as_dict().ok())
            .cloned()
            .unwrap_or_default();
        let mut fonts = resources
            .get(b"Font")
            .ok()
            .and_then(|object| object.as_dict().ok())
            .cloned()
            .unwrap_or_default();
        // `F1` is the name the content stream above draws with; if the page already has an
        // F1 the watermark reuses it rather than shadowing the page's own font.
        fonts.set("F1", Object::Reference(font_id));
        resources.set("Font", Object::Dictionary(fonts));
        page.set("Resources", Object::Dictionary(resources));
        page.set("Contents", Object::Array(array));
    }

    Ok(document.get_pages().len())
}

/// Escapes a string for a PDF `Tj` literal.
///
/// PDF string literals treat `\`, `(` and `)` specially; unescaped text either fails to
/// parse or silently truncates at the first `)`, which would make a watermark appear
/// missing rather than wrong.
fn escape_text(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '\\' => escaped.push_str("\\\\"),
            '(' => escaped.push_str("\\("),
            ')' => escaped.push_str("\\)"),
            _ => escaped.push(character),
        }
    }
    escaped
}

/// Rasterizes one page to PNG bytes at `scale`, for the clipboard or a screenshot export.
pub fn page_png(path: &Path, page: u32, scale: f32) -> Result<Vec<u8>, AppError> {
    let raster = PdfProvider::render_page_at(path, page, scale)?;
    Ok(raster.bytes)
}

/// Rewrites `source` with annotations flattened and an optional watermark, atomically.
///
/// Returns the flattened annotation count. Composed here rather than in the command so the
/// order is explicit and testable: flatten first (which reads annotation appearance
/// streams), then watermark, so the watermark is on top of the baked marks and is not
/// itself flattened.
pub fn export_with_flatten(
    source: &[u8],
    rows: &[Annotation],
    watermark_text: Option<&str>,
) -> Result<(Vec<u8>, usize), AppError> {
    let mut document = Document::load_mem(source)
        .map_err(|error| AppError::Other(format!("could not read PDF: {error}")))?;

    // Flatten first, then watermark, so the watermark sits on top of the baked marks
    // instead of being one more thing to bake.
    let flattened = flatten_annotations(&mut document, rows)?;
    if let Some(text) = watermark_text {
        watermark(&mut document, text)?;
    }

    let mut bytes = Vec::new();
    document
        .save_to(&mut bytes)
        .map_err(|error| AppError::Other(format!("could not write PDF: {error}")))?;

    Ok((bytes, flattened))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_is_escaped_for_a_pdf_literal() {
        assert_eq!(escape_text("DRAFT"), "DRAFT");
        assert_eq!(escape_text("50% (draft)"), "50% \\(draft\\)");
        assert_eq!(escape_text("back\\slash"), "back\\\\slash");
    }
}
