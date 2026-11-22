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

use crate::editor::textlocate;
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
            // Ink and line have no flattening primitive: neither a stroke path nor a
            // two-endpoint line can be baked by this pass, and drawing either as a filled box
            // would be a different mark. Both are skipped *here*, before the drawn-guard.
            //
            // This is defence in depth, and today it is redundant: `appearance_for` returns
            // None for both kinds, so the drawn-guard below would exclude them anyway. The
            // skip is kept because that coincidence is not a rule -- the day either kind gains
            // an appearance the guard thinks drawable, this line is what stops the flattening
            // pass deleting it. A mutation that removes Line from this match changes no test
            // result, which is the honest measure of how much it is doing right now.
            if matches!(row.kind, AnnotationKind::Ink | AnnotationKind::Line { .. }) {
                continue;
            }
            // Only a row this pass can actually emit operations for may be counted as drawn:
            // the row is deleted after `append_marks`, so counting one that emits nothing
            // would delete it without redrawing it. Squiggly is such a row -- the flattening
            // match cannot draw a wave, so it must be left for the viewer's own synthesis
            // from /QuadPoints.
            if row.kind != AnnotationKind::Squiggly
                && crate::editor::annotate::appearance_for(row)?.is_some()
            {
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
            AnnotationKind::Note | AnnotationKind::Rect => ("CA", None),
            // Ink is NOT flattened: the flatten loop above skips it and this match continues past
            // it, so a saved signature stays a real annotation the viewer can move. There is no
            // ink-flattening pass anywhere in this file, and `flattening_keeps_ink_as_an_annotation_
            // rather_than_deleting_it` asserts that ink survives with `flattened == 0`. Squiggly is
            // a wavy line that this flattening path can only approximate as a straight one, so it is
            // left to the viewer's own synthesis from /QuadPoints rather than drawn wrong. The
            // remaining P3.1 kinds have no flattening yet.
            AnnotationKind::Ink
            | AnnotationKind::Squiggly
            | AnnotationKind::FreeText(_)
            | AnnotationKind::Line { .. }
            | AnnotationKind::Oval => continue,
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
            AnnotationKind::Rect => {
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
            // Unreachable by construction: the alpha/width match above `continue`s for these,
            // so this arm exists only to keep the two matches exhaustive together.
            AnnotationKind::Ink
            | AnnotationKind::Squiggly
            | AnnotationKind::FreeText(_)
            | AnnotationKind::Line { .. }
            | AnnotationKind::Oval => {}
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

/// The resource name the watermark's own font is installed under.
///
/// Not `F1`: a page's content stream almost always draws with `F1`, and taking that name over
/// redraws the page's own text in the watermark's font. The name is unlikely to collide, and
/// `watermark` reuses one font object per page rather than adding one per pass.
const WATERMARK_FONT: &[u8] = b"WmkFont";

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
            Operation::new(
                "Tf",
                vec![Object::Name(WATERMARK_FONT.to_vec()), fitted.into()],
            ),
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
        page.set(b"Contents", Object::Array(array));

        let resources = textlocate::writable_resources_mut(document, page_id)?;
        let mut fonts = resources
            .get(b"Font")
            .ok()
            .and_then(|object| object.as_dict().ok())
            .cloned()
            .unwrap_or_default();
        fonts.set(WATERMARK_FONT, Object::Reference(font_id));
        resources.set(b"Font", Object::Dictionary(fonts));
    }

    Ok(document.get_pages().len())
}

/// The resource name a Fill & Sign label's font is installed under.
///
/// Same reasoning as [`WATERMARK_FONT`]: a page's own text almost always draws with `F1`, and
/// taking that name over would redraw the page in the label's font. `SignLbl` is short, legal in
/// a PDF name (no delimiter, no `#`), and cannot collide with a name this app writes.
const SIGNATURE_LABEL_FONT: &[u8] = b"SignLbl";

/// The label a visual signature carries, verbatim (D-N2, Q3 signed off 2026-11-22).
///
/// This constant is the single source for the page-content string **and** the UI string. One
/// string, two readers, so the label in the artefact and the label on screen cannot drift -- which
/// is the only way "the file says what the app says" stays true after an edit to either.
///
/// The em dash is deliberate and is why [`win_ansi_bytes`] exists: `—` as UTF-8 is three bytes
/// (`E2 80 94`) and a `/WinAnsiEncoding` font would draw three glyphs of garbage. It is `0x97` in
/// WinAnsi. Q3 approved this wording *including* the dash, so the dash is encoded rather than
/// swapped for a hyphen.
pub const VISUAL_SIGNATURE_LABEL: &str =
    "Visual signature \u{2014} no cryptographic weight. This is not a digital signature.";

/// Font size for the label, in points.
///
/// Small on purpose: the label must be legible when reading the page, and must not compete with
/// the document. 6pt is the same floor the watermark uses for a fitted mark.
pub const SIGNATURE_LABEL_FONT_SIZE: f32 = 6.0;

/// Encodes `text` as WinAnsi bytes for a `/WinAnsiEncoding` font.
///
/// Only the two characters this label needs are mapped beyond ASCII, and anything unmapped is
/// refused rather than written as `?`: a label that silently loses a character no longer says what
/// the ruling approved, and this is a disclosure string where "nearly the same words" is not good
/// enough.
fn win_ansi_bytes(text: &str) -> Result<Vec<u8>, AppError> {
    let mut bytes = Vec::with_capacity(text.len());
    for character in text.chars() {
        let byte = match character {
            // WinAnsi 0x97 is the em dash. `0x80`-`0x9F` are the range where WinAnsi differs
            // from Latin-1, so this cannot be inferred from a cast.
            '\u{2014}' => 0x97_u8,
            '\u{2013}' => 0x96_u8, // en dash, mapped for completeness of the same range
            c if (c as u32) < 0x80 => c as u8,
            other => {
                return Err(AppError::Other(format!(
                    "the signature label contains {other:?}, which WinAnsi cannot encode; the \
                     label is a disclosure and must be written exactly"
                )))
            }
        };
        bytes.push(byte);
    }
    Ok(bytes)
}

/// Escapes WinAnsi bytes for a PDF `Tj` literal.
///
/// The sibling of [`escape_text`], which takes a `&str` and so cannot carry a `0x97` byte: an
/// escaped string is only correct once the *encoding* has been decided, not before. Bytes above
/// `0x7F` need no escape in a literal string, only `\`, `(` and `)` do.
fn escape_bytes(bytes: &[u8]) -> Vec<u8> {
    let mut escaped = Vec::with_capacity(bytes.len());
    for &byte in bytes {
        match byte {
            b'\\' => escaped.extend_from_slice(b"\\\\"),
            b'(' => escaped.extend_from_slice(b"\\("),
            b')' => escaped.extend_from_slice(b"\\)"),
            other => escaped.push(other),
        }
    }
    escaped
}

/// Writes the visual-signature disclosure beside a placed signature (D-N2).
///
/// **This is page content, not an annotation**, and that is the whole point of D-N2: an annotation
/// disappears when a viewer hides annotations, and the disclosure has to survive being mailed to
/// someone else. So this appends a content stream, exactly as `watermark` does, and for the same
/// stated reason.
///
/// The call is deliberately narrow -- one line of text at a given size, at a given page position --
/// because the two things it must not do are (a) become a general text writer with reflow and
/// wrapping it cannot justify, and (b) touch the page's own resources. It reuses one font object
/// across all pages like the watermark, and installs it under its own name.
///
/// `page` is a **0-based index**, matching the DB, the UI and the renderer. lopdf's `get_pages()`
/// is keyed from 1, so the conversion happens here, once, at the boundary where the two notions
/// meet -- the same place and the same `+ 1` as `annotate::apply_annotations`, and for the same
/// reason: a caller should never have to remember which convention a given helper speaks.
///
/// `(x, y)` is the label's **baseline start** in page space, matching `Tj`'s own convention, so a
/// caller positions it by the text's baseline rather than its top-left corner. Page rotation and a
/// non-zero `/MediaBox` origin are not compensated here; the caller composes them, and the caller
/// is the annotate path which already knows the page's geometry.
pub fn signature_label(
    document: &mut Document,
    page: u32,
    x: f32,
    y: f32,
    size: f32,
) -> Result<(), AppError> {
    let page_id = *document
        .get_pages()
        .get(&(page + 1))
        .ok_or_else(|| AppError::Other(format!("page {page} is not in the document")))?;

    // Refuse a nonsense position rather than writing a stream that draws nothing: a label that
    // is silently off-page is the failure mode where everything looks fine and the file carries
    // no disclosure at all.
    if !(x.is_finite() && y.is_finite() && size.is_finite()) || size <= 0.0 {
        return Err(AppError::Other(
            "the signature label needs a finite position and a positive size".to_string(),
        ));
    }

    let escaped = escape_bytes(&win_ansi_bytes(VISUAL_SIGNATURE_LABEL)?);

    let operations = vec![
        Operation::new("q", vec![]),
        // A mid-grey, so the label reads as a disclosure rather than as part of the document's
        // own text. It is not hidden and not white: the ruling asks for *visible*.
        Operation::new("rg", vec![0.35.into(), 0.35.into(), 0.35.into()]),
        Operation::new("BT", vec![]),
        Operation::new(
            "Tf",
            vec![Object::Name(SIGNATURE_LABEL_FONT.to_vec()), size.into()],
        ),
        Operation::new("Td", vec![x.into(), y.into()]),
        Operation::new("Tj", vec![Object::string_literal(escaped)]),
        Operation::new("ET", vec![]),
        Operation::new("Q", vec![]),
    ];

    let content = Content { operations };
    let encoded = content
        .encode()
        .map_err(|error| AppError::Other(format!("could not encode the label: {error}")))?;
    let stream_id = document.add_object(Stream::new(dictionary! {}, encoded));

    let mut array = contents_array(document, page_id)?;
    array.push(Object::Reference(stream_id));
    let page_dict = document
        .get_object_mut(page_id)
        .map_err(|error| AppError::Other(format!("page unreadable: {error}")))?
        .as_dict_mut()
        .map_err(|error| AppError::Other(format!("page is not a dictionary: {error}")))?;
    page_dict.set(b"Contents", Object::Array(array));

    // Whether the font object is needed is decided *before* the mutable resources borrow below,
    // because `add_object` also needs `&mut document` and the two cannot be held at once.
    //
    // A page whose `/Resources` is inherited reports no private `/Font` here, so the first write
    // on such a page creates one object and `writable_resources_mut` gives the page its own
    // resources; the second write then finds the name present and reuses it.
    let label_font_present = document
        .get_object(page_id)
        .ok()
        .and_then(|object| object.as_dict().ok())
        .and_then(|page| page.get(b"Resources").ok())
        .and_then(|resources| resources.as_dict().ok())
        .and_then(|resources| resources.get(b"Font").ok())
        .and_then(|fonts| fonts.as_dict().ok())
        .map(|fonts| fonts.has(SIGNATURE_LABEL_FONT))
        .unwrap_or(false);

    let label_font_id = if label_font_present {
        None
    } else {
        Some(document.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "Helvetica",
            // Explicit, because the em dash's byte is only the em dash under WinAnsi. Without
            // this the reader would be free to pick a different encoding and 0x97 becomes a
            // different glyph.
            "Encoding" => "WinAnsiEncoding",
        }))
    };

    let resources = textlocate::writable_resources_mut(document, page_id)?;
    let mut fonts = resources
        .get(b"Font")
        .ok()
        .and_then(|object| object.as_dict().ok())
        .cloned()
        .unwrap_or_default();
    if let Some(font_id) = label_font_id {
        fonts.set(SIGNATURE_LABEL_FONT, Object::Reference(font_id));
    }
    resources.set(b"Font", Object::Dictionary(fonts));

    Ok(())
}

/// Escapes a string for a PDF `Tj` literal, for callers whose text is already known-encodable.
///
/// PDF string literals treat `\`, `(` and `)` specially; unescaped text either fails to
/// parse or silently truncates at the first `)`, which would make a watermark appear
/// missing rather than wrong.
///
/// Takes a `&str` and pushes its `char`s, so it is only correct for ASCII text. The signature
/// label needs `0x97` for its em dash and therefore goes through [`escape_bytes`] instead.
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
    use crate::editor::geometry::PdfRect;

    /// An openable one-page PDF with an empty content stream. Local rather than shared:
    /// the equivalent helper in `lopdf_write`'s tests is private to that module, and
    /// widening it to `pub(crate)` to reach it here would expose test scaffolding to the
    /// shipping build.
    fn blank_page_pdf() -> Document {
        let mut document = Document::with_version("1.4");
        let pages_id = document.new_object_id();
        let content = document.add_object(Stream::new(lopdf::Dictionary::new(), Vec::new()));
        let page_id = document.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => Object::Reference(pages_id),
            "MediaBox" => Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(595),
                Object::Integer(842),
            ]),
            "Contents" => Object::Reference(content),
        });
        document.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => Object::Array(vec![Object::Reference(page_id)]),
                "Count" => Object::Integer(1),
            }),
        );
        // `/Root` must reference a real Catalog object. An inline dictionary parses but
        // leaves `get_pages()` empty, which then fails the test for an unrelated reason.
        let catalog_id = document.add_object(dictionary! {
            "Type" => "Catalog",
            "Pages" => Object::Reference(pages_id),
        });
        document.trailer.set("Root", Object::Reference(catalog_id));
        document.renumber_objects();
        document
    }

    /// A squiggly is left for the viewer to synthesise, so flattening must not claim to
    /// have baked it.
    ///
    /// The same data-loss trap as the squiggly above, for the same reason: a free text's
    /// appearance is text, which the flattening pass cannot draw, so it must not be counted
    /// as drawn -- or the export deletes it and the text is gone.
    #[test]
    fn flattening_does_not_count_free_text_as_drawn() {
        let mut document = blank_page_pdf();
        let page_id = document.get_pages().values().next().copied().unwrap();
        let rect = PdfRect {
            x0: 72.0,
            y0: 600.0,
            x1: 272.0,
            y1: 660.0,
        };
        let row = Annotation {
            id: "free-text-1".to_string(),
            document_id: "doc-1".to_string(),
            page: 0,
            kind: AnnotationKind::FreeText(crate::model::FreeTextMode::Textbox),
            data: format!(
                r##"{{"v":1,"rect":[{},{},{},{}],"color":"#111418"}}"##,
                rect.x0, rect.y0, rect.x1, rect.y1
            ),
            text: Some("do not lose me".to_string()),
            created_at: 0,
            modified_at: 0,
        };

        let dictionary = crate::editor::lopdf_write::annotation_dictionary(
            &mut document,
            row.kind,
            rect,
            (0.07, 0.08, 0.09),
            1.0,
            row.text.as_deref(),
        )
        .unwrap();
        crate::editor::lopdf_write::attach_annotation(&mut document, page_id, dictionary).unwrap();

        let flattened = flatten_annotations(&mut document, std::slice::from_ref(&row)).unwrap();
        assert_eq!(flattened, 0, "free text was counted as flattened");

        let reloaded =
            Document::load_mem(&crate::editor::lopdf_write::save_to_bytes(&mut document).unwrap())
                .unwrap();
        let page = reloaded.get_pages().values().next().copied().unwrap();
        let annotations = reloaded.get_page_annotations(page).unwrap();
        assert_eq!(annotations.len(), 1, "flattening deleted the free text");
        assert_eq!(
            annotations[0]
                .get(b"Subtype")
                .and_then(Object::as_name)
                .unwrap(),
            b"FreeText"
        );
    }

    /// A-4 writes an appearance stream for ink, which makes ink look like a kind this pass
    /// could bake. It still cannot: the flattening match has no stroke-path primitive and
    /// drawing ink as a filled box would be a different mark. Ink is therefore skipped in the
    /// first loop, before the drawn-guard, so it is never deleted -- and this test pins that
    /// ordering, because moving ink past the guard would silently destroy every stroke on
    /// flatten while the appearance stream made the row look drawable.
    #[test]
    fn flattening_keeps_ink_as_an_annotation_rather_than_deleting_it() {
        let mut document = blank_page_pdf();
        let page_id = document.get_pages().values().next().copied().unwrap();
        let row = Annotation {
            id: "ink-1".to_string(),
            document_id: "doc-1".to_string(),
            page: 0,
            kind: AnnotationKind::Ink,
            data: r##"{"v":1,"color":"#111418","strokes":[[[72,600],[140,640],[200,600]]]}"##
                .to_string(),
            text: None,
            created_at: 0,
            modified_at: 0,
        };

        let dictionary = crate::editor::lopdf_write::ink_annotation_dictionary(
            &mut document,
            PdfRect {
                x0: 72.0,
                y0: 600.0,
                x1: 200.0,
                y1: 640.0,
            },
            &[vec![(72.0, 600.0), (140.0, 640.0), (200.0, 600.0)]],
            (0.07, 0.08, 0.09),
            1.0,
            None,
        )
        .unwrap();
        crate::editor::lopdf_write::attach_annotation(&mut document, page_id, dictionary).unwrap();

        let flattened = flatten_annotations(&mut document, std::slice::from_ref(&row)).unwrap();
        assert_eq!(flattened, 0, "ink must not be counted as flattened");

        let reloaded =
            Document::load_mem(&crate::editor::lopdf_write::save_to_bytes(&mut document).unwrap())
                .unwrap();
        let page = reloaded.get_pages().values().next().copied().unwrap();
        let annotations = reloaded.get_page_annotations(page).unwrap();
        assert_eq!(annotations.len(), 1, "flattening deleted the ink stroke");
        assert_eq!(
            annotations[0]
                .get(b"Subtype")
                .and_then(Object::as_name)
                .unwrap(),
            b"Ink"
        );
    }

    /// A-5's line is the same trap as ink: it has an appearance stream now, so it looks
    /// drawable to the flattening guard, but no arm of the flattening match draws a stroked
    /// path between two endpoints. It is skipped before the guard, and this pins that.
    #[test]
    fn flattening_keeps_a_line_as_an_annotation_rather_than_deleting_it() {
        let mut document = blank_page_pdf();
        let page_id = document.get_pages().values().next().copied().unwrap();
        let row = Annotation {
            id: "line-1".to_string(),
            document_id: "doc-1".to_string(),
            page: 0,
            kind: AnnotationKind::Line { arrow: true },
            data: r##"{"v":1,"color":"#111418","from":[72,600],"to":[300,640]}"##.to_string(),
            text: None,
            created_at: 0,
            modified_at: 0,
        };

        let dictionary = crate::editor::lopdf_write::line_annotation_dictionary(
            &mut document,
            PdfRect {
                x0: 72.0,
                y0: 600.0,
                x1: 300.0,
                y1: 640.0,
            },
            (72.0, 600.0),
            (300.0, 640.0),
            (0.07, 0.08, 0.09),
            true,
            None,
        )
        .unwrap();
        crate::editor::lopdf_write::attach_annotation(&mut document, page_id, dictionary).unwrap();

        let flattened = flatten_annotations(&mut document, std::slice::from_ref(&row)).unwrap();
        assert_eq!(flattened, 0, "a line must not be counted as flattened");

        let reloaded =
            Document::load_mem(&crate::editor::lopdf_write::save_to_bytes(&mut document).unwrap())
                .unwrap();
        let page = reloaded.get_pages().values().next().copied().unwrap();
        let annotations = reloaded.get_page_annotations(page).unwrap();
        assert_eq!(annotations.len(), 1, "flattening deleted the line");
    }

    /// This is not a cosmetic rule. `flatten_annotations` decides what was drawn and then
    /// deletes exactly those annotations, because a baked mark plus a live annotation would
    /// draw twice. A kind that is counted as drawn but emits no content-stream operations
    /// therefore gets **deleted and never redrawn**, and the export silently loses it.
    #[test]
    fn flattening_does_not_count_a_squiggly_as_drawn() {
        let mut document = blank_page_pdf();
        let page_id = document.get_pages().values().next().copied().unwrap();
        let rect = PdfRect {
            x0: 72.0,
            y0: 700.0,
            x1: 300.0,
            y1: 720.0,
        };
        let row = Annotation {
            id: "squiggle-1".to_string(),
            document_id: "doc-1".to_string(),
            page: 0,
            kind: AnnotationKind::Squiggly,
            data: format!(
                r##"{{"v":1,"rects":[[{},{},{},{}]],"color":"#d1242f","opacity":1.0}}"##,
                rect.x0, rect.y0, rect.x1, rect.y1
            ),
            text: None,
            created_at: 0,
            modified_at: 0,
        };

        // Attach it first, so "left alone" and "deleted" are distinguishable outcomes.
        let dictionary = crate::editor::lopdf_write::annotation_dictionary(
            &mut document,
            AnnotationKind::Squiggly,
            rect,
            (0.82, 0.14, 0.18),
            1.0,
            None,
        )
        .unwrap();
        crate::editor::lopdf_write::attach_annotation(&mut document, page_id, dictionary).unwrap();

        let flattened = flatten_annotations(&mut document, std::slice::from_ref(&row)).unwrap();
        assert_eq!(flattened, 0, "a squiggly was counted as flattened");

        let reloaded =
            Document::load_mem(&crate::editor::lopdf_write::save_to_bytes(&mut document).unwrap())
                .unwrap();
        let page = reloaded.get_pages().values().next().copied().unwrap();
        let annotations = reloaded.get_page_annotations(page).unwrap();
        assert_eq!(
            annotations.len(),
            1,
            "flattening deleted the squiggly instead of leaving it for the viewer"
        );
        assert_eq!(
            annotations[0]
                .get(b"Subtype")
                .and_then(Object::as_name)
                .unwrap(),
            b"Squiggly"
        );
    }

    #[test]
    fn text_is_escaped_for_a_pdf_literal() {
        assert_eq!(escape_text("DRAFT"), "DRAFT");
        assert_eq!(escape_text("50% (draft)"), "50% \\(draft\\)");
        assert_eq!(escape_text("back\\slash"), "back\\\\slash");
    }

    /// The label the content stream carries is the label the ruling approved, character for
    /// character.
    ///
    /// Q3 approved a wording *verbatim*, so this is an exact comparison against a literal typed
    /// out here rather than against the constant: comparing the constant with itself would pass
    /// for any string, including an edited one, which is exactly the drift a verbatim ruling
    /// exists to prevent.
    #[test]
    fn the_label_wording_is_the_one_that_was_approved() {
        assert_eq!(
            VISUAL_SIGNATURE_LABEL,
            "Visual signature \u{2014} no cryptographic weight. This is not a digital signature.",
            "the approved wording must not be paraphrased; Q3 approved this sentence exactly"
        );
    }

    /// The em dash is encoded as WinAnsi `0x97`, not as three UTF-8 bytes.
    ///
    /// This is the defect the encoding exists to prevent and it is invisible without a test: a
    /// `/WinAnsiEncoding` font reads `0x97` as an em dash and `E2 80 94` as three unrelated
    /// glyphs. The label would still be *present* -- it would just say something else.
    #[test]
    fn the_em_dash_becomes_one_win_ansi_byte_not_three_utf8_bytes() {
        let bytes = win_ansi_bytes(VISUAL_SIGNATURE_LABEL).expect("the label is encodable");
        assert!(
            bytes.contains(&0x97),
            "the em dash must be WinAnsi 0x97; got {bytes:?}"
        );
        assert!(
            !bytes.windows(3).any(|w| w == [0xE2, 0x80, 0x94]),
            "the em dash must not be written as UTF-8; a WinAnsi font would draw three glyphs"
        );
        // The rest of the label is ASCII, so the encoded length is the char count.
        assert_eq!(bytes.len(), VISUAL_SIGNATURE_LABEL.chars().count());
    }

    /// A character WinAnsi cannot encode is refused rather than written as a placeholder.
    #[test]
    fn an_unencodable_character_is_refused_rather_than_mangled() {
        let error = win_ansi_bytes("signed by 李")
            .expect_err("a CJK character has no WinAnsi byte and must not be silently dropped");
        assert!(
            error.to_string().contains("WinAnsi cannot encode"),
            "the refusal must name the reason; got {error}"
        );
    }

    /// The label is appended as a **content stream** and installs its font in the page resources.
    ///
    /// The assertion that matters is the negative one: the page's `Contents` grows by exactly one
    /// stream and the page gains no annotation. D-N2 chose content over annotation because an
    /// annotation can be switched off by a viewer, and a test that only checked "the label text
    /// appears somewhere in the file" would pass for the wrong mechanism.
    #[test]
    fn the_label_is_written_into_page_content_and_not_as_an_annotation() {
        let mut document = blank_page_pdf();
        let page_id = *document.get_pages().get(&1).expect("page 1");
        let before = contents_array(&document, page_id).expect("contents").len();
        let annotations_before = document
            .get_object(page_id)
            .and_then(|o| o.as_dict())
            .ok()
            .and_then(|d| d.get(b"Annots").ok())
            .and_then(|a| a.as_array().ok())
            .map(|a| a.len())
            .unwrap_or(0);

        signature_label(&mut document, 0, 72.0, 100.0, 6.0).expect("label writes");

        let after = contents_array(&document, page_id).expect("contents").len();
        assert_eq!(
            after,
            before + 1,
            "the label must append exactly one content stream, not rewrite the page's contents"
        );

        let annotations_after = document
            .get_object(page_id)
            .and_then(|o| o.as_dict())
            .ok()
            .and_then(|d| d.get(b"Annots").ok())
            .and_then(|a| a.as_array().ok())
            .map(|a| a.len())
            .unwrap_or(0);
        assert_eq!(
            annotations_after, annotations_before,
            "the label must be content, not an annotation: an annotation is switchable, and a \
             disclosure that a viewer can hide is not a disclosure"
        );

        // The font really is installed under the label's own name, with the encoding that makes
        // 0x97 an em dash.
        let resources = document
            .get_object(page_id)
            .and_then(|o| o.as_dict())
            .ok()
            .and_then(|d| d.get(b"Resources").ok())
            .and_then(|r| r.as_dict().ok())
            .expect("the page must have private resources after the write");
        let fonts = resources
            .get(b"Font")
            .and_then(|f| f.as_dict())
            .expect("a /Font dictionary");
        let font_ref = fonts
            .get(SIGNATURE_LABEL_FONT)
            .expect("the label font must be registered");
        let font = document
            .get_object(font_ref.as_reference().expect("a reference"))
            .and_then(|o| o.as_dict())
            .expect("the font object");
        assert_eq!(
            font.get(b"Encoding").and_then(Object::as_name).unwrap(),
            b"WinAnsiEncoding",
            "the font must declare WinAnsi or the em dash byte means something else"
        );
    }

    /// Writing twice reuses the font object rather than adding a second one.
    #[test]
    fn a_second_label_reuses_the_font_rather_than_growing_the_file() {
        let mut document = blank_page_pdf();
        signature_label(&mut document, 0, 72.0, 100.0, 6.0).expect("first");
        let font_objects_after_first = count_font_objects(&document);
        signature_label(&mut document, 0, 72.0, 120.0, 6.0).expect("second");
        assert_eq!(
            count_font_objects(&document),
            font_objects_after_first,
            "the label's font is identical on every page and must not be duplicated"
        );
    }

    fn count_font_objects(document: &Document) -> usize {
        document
            .objects
            .values()
            .filter(|object| {
                object
                    .as_dict()
                    .map(|d| d.get(b"BaseFont").is_ok())
                    .unwrap_or(false)
            })
            .count()
    }

    /// A non-finite or non-positive position is refused, because a label drawn off-page is a
    /// document that claims to carry a disclosure and does not.
    #[test]
    fn a_nonsense_position_is_refused_rather_than_drawn_off_page() {
        let mut document = blank_page_pdf();
        assert!(signature_label(&mut document, 0, f32::NAN, 100.0, 6.0).is_err());
        assert!(signature_label(&mut document, 0, 72.0, f32::INFINITY, 6.0).is_err());
        assert!(signature_label(&mut document, 0, 72.0, 100.0, 0.0).is_err());
        assert!(
            signature_label(&mut document, 1, 72.0, 100.0, 6.0).is_err(),
            "page 1 is past this one-page document"
        );
    }
}
