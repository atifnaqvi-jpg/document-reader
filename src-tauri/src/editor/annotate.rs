//! Turns stored annotations into real PDF annotation objects (P2-D3/D8).
//!
//! The database is the source of truth; the PDF file is a projection of it. So
//! "save my edits" is: load the original bytes, rebuild *every* annotation for the
//! document from the DB, and write the result. That is deliberately not an
//! incremental "apply the pending change" path -- rebuilding is idempotent, so a
//! failed or repeated save converges on the same file instead of stacking
//! duplicate annotations on top of each other.
//!
//! Annotations the user made in *another* PDF application are left alone: they are
//! not in our DB, and P2 has no business deleting someone else's work.

use std::collections::HashMap;

use lopdf::{Document, Object};

pub mod appearance;

use crate::editor::geometry::PdfRect;
use crate::editor::{export, lopdf_write};
use crate::model::{Annotation, AnnotationKind, AnnotationStatus, AppError, FreeTextMode};

/// The `data` blob shape this module understands. `v` is explicit so a future
/// tool can change the geometry without guessing at older rows.
///
/// D-A13: version 2 adds the common `status` and `parent` keys. A v1 blob is read by this
/// reader unchanged -- the v1 shapes are all still valid -- and a v2 blob is skipped by a
/// 1.0.x build rather than misread, which is what lets a hand-test install share a database
/// with a 1.1.0 one during the transition.
const SUPPORTED_BLOB_VERSION: u32 = 2;

/// The blob key that marks an `Ink` or `FreeText` row as a **visual signature** (P3.3a D-N2).
///
/// A drawn signature is `AnnotationKind::Ink` and a typed one is `FreeText` (D-N3a/N1/N2), so
/// the *kind* cannot say which rows are signatures and which are ordinary marks. This flag can.
/// It is additive and read with `serde(default)`, so no version bump is needed: an older build
/// reads a signature row as the plain annotation it already knows how to draw and merely omits
/// the label, which is a downgrade rather than a misread.
///
/// The label is not a per-row visual preference that a user might want to switch off: D-N2 makes
/// it part of the artifact, so a row carrying this flag gets the label written into page content
/// on every save. If the flag were dropped on a later save the label would vanish from the next
/// written file, and the disclosure would depend on which build happened to save last.
pub const SIGNATURE_FLAG: &str = "visualSignature";

/// The vertical gap between a signature's rect and the label's baseline, in points.
///
/// Small and negative -- *below* the mark, not above it -- because a signature sits on a line and
/// a label above it would land in the text of the row above. Two points clears the mark's own
/// descender without crowding the next line.
const SIGNATURE_LABEL_GAP: f32 = 2.0;

/// A highlight with no stored opacity: matches the UI's default so a blob missing
/// the field renders the same as one that has it.
const DEFAULT_HIGHLIGHT_OPACITY: f32 = 0.4;

/// The conventional sticky-note icon size, in points.
pub const NOTE_ICON_POINTS: f32 = 20.0;

#[derive(serde::Deserialize)]
struct HighlightBlob {
    rects: Vec<[f32; 4]>,
    #[serde(default)]
    color: Option<String>,
    #[serde(default)]
    opacity: Option<f32>,
}

#[derive(serde::Deserialize)]
struct InkBlob {
    strokes: Vec<Vec<[f32; 2]>>,
    #[serde(default)]
    color: Option<String>,
}

/// A line or arrow: two endpoints in PDF points. `/L` and the appearance are both derived from
/// these, so the endpoints cannot disagree between the semantic record and the drawn stroke.
#[derive(serde::Deserialize)]
struct LineBlob {
    from: [f32; 2],
    to: [f32; 2],
    #[serde(default)]
    color: Option<String>,
}

#[derive(serde::Deserialize)]
struct ShapeBlob {
    rect: [f32; 4],
    #[serde(default)]
    color: Option<String>,
    /// A callout's leader-line target, in PDF points. Absent for every other kind, and absent
    /// for a callout blob written before A-3, which the writer refuses rather than guesses at.
    #[serde(default)]
    callout: Option<[f32; 2]>,
}

/// Writes every annotation for one document into `bytes`, returning new bytes.
///
/// `bytes` must be the *original* document: applying this to an already-annotated
/// file would duplicate every annotation it wrote last time.
pub fn apply_annotations(bytes: &[u8], annotations: &[Annotation]) -> Result<Vec<u8>, AppError> {
    let mut document = Document::load_mem(bytes)
        .map_err(|error| AppError::Other(format!("could not read pdf: {error}")))?;

    let page_ids = document.get_pages();
    // Which object a row was written as, so a reply can point at its parent (D-A5). Filled
    // during the first pass and read during the second: `/IRT` needs the parent's *object
    // id*, which does not exist until the parent has been written, and the rows are not
    // ordered parent-first.
    let mut written: HashMap<String, lopdf::ObjectId> = HashMap::new();
    let mut replies: Vec<(lopdf::ObjectId, String)> = Vec::new();
    // (page index, rect) for every row that is a visual signature, so the disclosure can be
    // written once per row after the annotations exist. Collected rather than written inline
    // because `signature_label` needs `&mut document` while the row's own write already holds it.
    let mut labels: Vec<(u32, PdfRect)> = Vec::new();

    for annotation in annotations {
        // `get_pages()` is keyed by lopdf's **1-based** page number, while the DB and the
        // UI both use 0-based indices (the renderer's `page` is a 0-based index). Without
        // the +1 every annotation would land one page late -- and page 0's would silently
        // disappear, since lopdf has no page 0.
        let Some(page_id) = page_ids.get(&(annotation.page + 1)).copied() else {
            // An annotation pointing past the end of the file means the document
            // was replaced under a stable id. Skip it rather than writing it onto
            // an unrelated page.
            continue;
        };
        // A row flagged as a visual signature is resolved *before* the writeability check
        // below, because that check `continue`s on a row it cannot draw -- and a signature row
        // is exactly the one that must not be skipped in silence. A flagged row that draws
        // nothing therefore fails the save here rather than being saved with no disclosure.
        let signature = signature_label_rect(annotation)?;

        let Some(mut dictionary) = dictionary_for(&mut document, annotation)? else {
            continue;
        };
        // `/NM` is the annotation's stable name in the file. Without it an annotation on
        // disk is anonymous, which makes flattening unable to tell this app's marks from a
        // document's own -- and the safe fallback there is to touch nothing, so flatten
        // would silently export a blank file. It also gives the file itself an identity to
        // match against the DB row.
        dictionary.set("NM", Object::string_literal(annotation.id.clone()));

        let blob: serde_json::Value = serde_json::from_str(&annotation.data).unwrap_or_default();
        apply_review_state(&mut dictionary, &blob);
        let parent = common_blob(&blob).parent;

        // Recorded for the third pass, where the disclosure is written into page content.
        if let Some(rect) = signature {
            labels.push((annotation.page, rect));
        }

        let object_id = lopdf_write::attach_annotation(&mut document, page_id, dictionary)?;
        written.insert(annotation.id.clone(), object_id);
        if let Some(parent) = parent {
            replies.push((object_id, parent));
        }
    }

    // Second pass: a reply whose parent was written gets `/IRT`; one whose parent is gone
    // stays detached at the root rather than being dropped (D-A5 §5). Written by mutating
    // the stored object, because the dictionary was already attached in pass one.
    for (reply_id, parent) in replies {
        let Some(parent_id) = written.get(&parent).copied() else {
            continue;
        };
        let object = document
            .get_object_mut(reply_id)
            .map_err(|error| AppError::Other(format!("reply object unreadable: {error}")))?;
        let dictionary = object
            .as_dict_mut()
            .map_err(|error| AppError::Other(format!("reply is not a dictionary: {error}")))?;
        dictionary.set("IRT", Object::Reference(parent_id));
    }

    // Third pass: the disclosure. Written after the annotations so the page carries both, and
    // after the reply pass so a failure here cannot leave a half-written reply chain.
    for (page, rect) in labels {
        // The label's baseline sits just under the mark's own bottom edge. `rect` is normalised
        // by the caller, so `y0` is the lower edge in page space and the label reads left to
        // right from the mark's left edge -- aligned with the signature rather than centred on
        // it, because a centred label drifts as the mark grows.
        let baseline = rect.y0 - SIGNATURE_LABEL_GAP;
        export::signature_label(
            &mut document,
            page,
            rect.x0,
            baseline,
            export::SIGNATURE_LABEL_FONT_SIZE,
        )?;
    }

    lopdf_write::save_to_bytes(&mut document)
}

/// Whether a blob marks its row as a visual signature (D-N2).
fn is_visual_signature(blob: &serde_json::Value) -> bool {
    blob.get(SIGNATURE_FLAG)
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
}

/// The rect a signature label is placed against: the **same** rect the mark itself is drawn to.
///
/// Derived per kind, mirroring `dictionary_for`, and *not* through [`appearance_for`]: that
/// function answers a different question -- "what does flattening bake?" -- and correctly returns
/// `None` for `Ink` and `FreeText`, which D-N10 leaves as annotations. A label placer built on it
/// would therefore refuse every real signature, which is how this was found.
///
/// The distinction that actually matters: an ink blob carries `strokes`, not a `rect`, so any
/// helper that reads `blob["rect"]` finds nothing for a drawn signature and silently places no
/// label while the save reports success.
///
/// A flagged row that draws nothing, or whose kind cannot carry a signature, is an **error**: the
/// row claims to be a signature, so it either has a mark to disclose or the save must not pretend
/// it succeeded.
fn signature_label_rect(annotation: &Annotation) -> Result<Option<PdfRect>, AppError> {
    let blob: serde_json::Value = serde_json::from_str(&annotation.data).unwrap_or_default();
    if !is_visual_signature(&blob) {
        return Ok(None);
    }

    let rect = match annotation.kind {
        // A drawn signature: the box is the ink's extent, exactly as the writer computes it.
        AnnotationKind::Ink => {
            let parsed: InkBlob = serde_json::from_value(blob).map_err(|error| {
                AppError::Other(format!("annotation {}: {error}", annotation.id))
            })?;
            let strokes: Vec<Vec<(f32, f32)>> = parsed
                .strokes
                .iter()
                .map(|stroke| stroke.iter().map(|point| (point[0], point[1])).collect())
                .collect();
            if !strokes.iter().any(|stroke| stroke.len() >= 2) {
                return Err(no_label_error(&annotation.id));
            }
            strokes_bounds(&strokes)
        }
        // A typed signature: the box is the one the writer draws text into.
        AnnotationKind::FreeText(_) => {
            let parsed: ShapeBlob = serde_json::from_value(blob).map_err(|error| {
                AppError::Other(format!("annotation {}: {error}", annotation.id))
            })?;
            rect_from(&parsed.rect)
        }
        // Every other kind is refused rather than labelled: a highlight is not a signature, and
        // labelling one would put a legal disclosure on a page the user never signed.
        _ => {
            return Err(AppError::Other(format!(
                "annotation {} is flagged as a visual signature but is a {}, which cannot carry a \
                 signature; only ink and free text can, so the flag is wrong and the save stops",
                annotation.id,
                annotation.kind.as_str()
            )))
        }
    };

    Ok(Some(rect.normalised()))
}

fn no_label_error(id: &str) -> AppError {
    AppError::Other(format!(
        "annotation {id} is flagged as a visual signature but draws nothing, so it would carry no \
         required label; refusing rather than saving a signature with no disclosure"
    ))
}

/// Builds an annotation dictionary from a stored row, or `None` if the row's
/// geometry is not writeable as a PDF annotation (e.g. a zero-area mark).
/// What a stored annotation looks like when drawn: the geometry the file records, plus the
/// fill the writer used.
///
/// Flattening needs this rather than reading `/AP` back off the written annotation, because
/// `dictionary_for` deliberately writes no appearance stream -- it records the mark (rect,
/// colour, opacity) and lets the viewer render it. Baking therefore has to redraw from the
/// same data the viewer was given.
pub struct Appearance {
    pub kind: AnnotationKind,
    pub rect: PdfRect,
    pub color: (f32, f32, f32),
    pub opacity: f32,
}

/// The drawable appearance of a stored annotation, or `None` if it draws nothing.
///
/// Shares `dictionary_for`'s parsing so a baked mark cannot drift from the annotation the
/// user saw: both read the same blob through the same helpers.
pub fn appearance_for(annotation: &Annotation) -> Result<Option<Appearance>, AppError> {
    let blob: serde_json::Value = serde_json::from_str(&annotation.data).map_err(|error| {
        AppError::Other(format!(
            "annotation {} has malformed data: {error}",
            annotation.id
        ))
    })?;

    // A blob from a newer build cannot be interpreted safely, so it is skipped rather than
    // baked in the wrong place.
    let version = blob
        .get("v")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(1);
    if version > SUPPORTED_BLOB_VERSION as u64 {
        return Ok(None);
    }

    match annotation.kind {
        AnnotationKind::Highlight
        | AnnotationKind::Underline
        | AnnotationKind::Strikeout
        | AnnotationKind::Squiggly => {
            let parsed: HighlightBlob = serde_json::from_value(blob).map_err(|error| {
                AppError::Other(format!("annotation {}: {error}", annotation.id))
            })?;
            let Some(first) = parsed.rects.first() else {
                return Ok(None);
            };
            // The union, matching what `dictionary_for` writes onto the page.
            let rect = parsed
                .rects
                .iter()
                .skip(1)
                .fold(rect_from(first), |acc, next| union(acc, rect_from(next)));
            Ok(Some(Appearance {
                kind: annotation.kind,
                rect: rect.normalised(),
                color: parse_color(parsed.color.as_deref()),
                opacity: parsed.opacity.unwrap_or(DEFAULT_HIGHLIGHT_OPACITY),
            }))
        }
        AnnotationKind::Note => {
            let point = blob
                .get("point")
                .and_then(|point| {
                    let array = point.as_array()?;
                    Some((
                        array.first()?.as_f64()? as f32,
                        array.get(1)?.as_f64()? as f32,
                    ))
                })
                .or_else(|| {
                    blob.get("rect").and_then(|rect| {
                        let array = rect.as_array()?;
                        Some((
                            array.first()?.as_f64()? as f32,
                            array.get(1)?.as_f64()? as f32,
                        ))
                    })
                });
            let Some((x, y)) = point else {
                return Ok(None);
            };
            Ok(Some(Appearance {
                kind: AnnotationKind::Note,
                rect: PdfRect {
                    x0: x,
                    y0: y,
                    x1: x + NOTE_ICON_POINTS,
                    y1: y + NOTE_ICON_POINTS,
                },
                color: parse_color(blob.get("color").and_then(serde_json::Value::as_str)),
                opacity: 1.0,
            }))
        }
        AnnotationKind::Rect => {
            let parsed: ShapeBlob = serde_json::from_value(blob).map_err(|error| {
                AppError::Other(format!("annotation {}: {error}", annotation.id))
            })?;
            Ok(Some(Appearance {
                kind: AnnotationKind::Rect,
                rect: rect_from(&parsed.rect).normalised(),
                color: parse_color(parsed.color.as_deref()),
                opacity: 1.0,
            }))
        }
        // Ink is flattened from its stroke paths by `export`, not from a bounding box: a
        // filled rectangle over an ink annotation would be a different mark.
        AnnotationKind::Ink => Ok(None),
        // P3.1 rows land one type at a time (D-A1/D-A2). A kind whose geometry builder has not
        // been written yet must produce *no* appearance rather than fall through to a
        // rectangle: drawing a line as a filled box would be a silent wrongness, and the
        // palette cannot offer these tools until their row is ticked.
        //
        // Free text is not flattenable yet either, but for a different reason: its appearance
        // is text, and the flattening pass draws shapes. Left for the same reason as ink --
        // a box around the text is not the text.
        AnnotationKind::FreeText(_) | AnnotationKind::Line { .. } | AnnotationKind::Oval => {
            Ok(None)
        }
    }
}

/// The review keys common to every annotation: `status` and `parent` (D-A5/D-A7).
///
/// Read in one place because they belong to *every* kind rather than to a geometry, and a
/// per-arm copy would let one arm ship without them. Absent or unparseable values are
/// `None`: a status this build does not know is not a status it should write, and an
/// unknown key must not fail a whole save.
#[derive(Default)]
struct CommonBlob {
    status: Option<AnnotationStatus>,
    parent: Option<String>,
}

fn common_blob(blob: &serde_json::Value) -> CommonBlob {
    CommonBlob {
        status: blob
            .get("status")
            .and_then(serde_json::Value::as_str)
            .and_then(AnnotationStatus::parse),
        // An empty string is not a parent. JSON null and an absent key both mean "root",
        // and so does "" -- a reply whose parent was cleared should detach, not point at
        // an annotation with no id.
        parent: blob
            .get("parent")
            .and_then(serde_json::Value::as_str)
            .filter(|parent| !parent.trim().is_empty())
            .map(str::to_string),
    }
}

/// Writes the review and reply keys onto a finished dictionary (D-A5/D-A7).
///
/// `/State` and `/StateModel` move together: `Accepted`/`Rejected`/`Completed`/`Cancelled`
/// are the Review model's values, and writing one without the model leaves a viewer to
/// interpret it under whatever default it has.
///
/// `/IRT` is an indirect reference to the parent annotation's **object**, so it needs the
/// parent's object id, which only exists once the parent has been written. `parent_ids`
/// carries that mapping; a parent that is not in it (deleted, or on a page that did not
/// draw) leaves the reply detached rather than pointing at nothing.
fn apply_review_state(dictionary: &mut lopdf::Dictionary, blob: &serde_json::Value) {
    if let Some(status) = common_blob(blob).status {
        // `/State` is a text **string** and `/StateModel` a **name** (ISO 32000-1 Table 171).
        // The two types are not interchangeable: a viewer looking for a name finds nothing in
        // a string, falls back to the default model, and reads `Accepted` as a value that
        // model does not define.
        dictionary.set("State", Object::string_literal(status.state_name()));
        dictionary.set("StateModel", Object::Name(b"Review".to_vec()));
    }
}

fn dictionary_for(
    document: &mut lopdf::Document,
    annotation: &Annotation,
) -> Result<Option<lopdf::Dictionary>, AppError> {
    let blob: serde_json::Value = serde_json::from_str(&annotation.data).map_err(|error| {
        AppError::Other(format!(
            "annotation {} has malformed data: {error}",
            annotation.id
        ))
    })?;

    // A blob from a newer build is not an error we can act on, but writing it
    // with the wrong interpretation would place a mark in the wrong place.
    let version = blob
        .get("v")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(1);
    if version > SUPPORTED_BLOB_VERSION as u64 {
        return Ok(None);
    }

    let contents = annotation.text.as_deref();

    match annotation.kind {
        AnnotationKind::Highlight
        | AnnotationKind::Underline
        | AnnotationKind::Strikeout
        | AnnotationKind::Squiggly => {
            let parsed: HighlightBlob = serde_json::from_value(blob).map_err(|error| {
                AppError::Other(format!("annotation {}: {error}", annotation.id))
            })?;
            let color = parse_color(parsed.color.as_deref());
            let opacity = parsed.opacity.unwrap_or(DEFAULT_HIGHLIGHT_OPACITY);
            let Some(first) = parsed.rects.first() else {
                return Ok(None);
            };
            let rect = rect_from(first);

            // Multi-rect highlights (a selection spanning lines) are written as one
            // annotation per rect with an /IRT parent link in a full implementation;
            // P2 writes the union, which every viewer renders as a single highlight.
            let union = parsed
                .rects
                .iter()
                .skip(1)
                .fold(rect, |acc, next| union(acc, rect_from(next)));

            Ok(Some(lopdf_write::annotation_dictionary(
                document,
                annotation.kind,
                union,
                color,
                opacity,
                contents,
            )?))
        }
        // An oval shares the rect-anchored path with `Rect`; the difference is the subtype and
        // the curve the appearance draws, both of which `annotation_dictionary` decides.
        AnnotationKind::Oval => {
            let parsed: ShapeBlob = serde_json::from_value(blob).map_err(|error| {
                AppError::Other(format!("annotation {}: {error}", annotation.id))
            })?;
            Ok(Some(lopdf_write::annotation_dictionary(
                document,
                AnnotationKind::Oval,
                rect_from(&parsed.rect),
                parse_color(parsed.color.as_deref()),
                1.0,
                contents,
            )?))
        }
        AnnotationKind::Note => {
            // A note's anchor is its /Rect; the blob for a note stores a point.
            let parsed: serde_json::Value = blob;
            let point = parsed
                .get("point")
                .and_then(|point| {
                    let array = point.as_array()?;
                    Some((
                        array.first()?.as_f64()? as f32,
                        array.get(1)?.as_f64()? as f32,
                    ))
                })
                .or_else(|| {
                    parsed.get("rect").and_then(|rect| {
                        let array = rect.as_array()?;
                        Some((
                            array.first()?.as_f64()? as f32,
                            array.get(1)?.as_f64()? as f32,
                        ))
                    })
                });
            let Some((x, y)) = point else {
                return Ok(None);
            };
            let anchor = PdfRect {
                x0: x,
                y0: y,
                x1: x,
                y1: y,
            };
            let color = parse_color(parsed.get("color").and_then(serde_json::Value::as_str));
            // `is_drawable` is false for a bare point, so the note path must not go
            // through the rect guard: the dictionary builder gives it an icon box.
            Ok(Some(lopdf_write::annotation_dictionary(
                document,
                AnnotationKind::Note,
                PdfRect {
                    x0: anchor.x0,
                    y0: anchor.y0,
                    // 1x1 is enough for the builder to accept it and then expand
                    // to the conventional 20x20 icon.
                    x1: anchor.x0 + 1.0,
                    y1: anchor.y0 + 1.0,
                },
                color,
                1.0,
                contents,
            )?))
        }
        AnnotationKind::Rect => {
            let parsed: ShapeBlob = serde_json::from_value(blob).map_err(|error| {
                AppError::Other(format!("annotation {}: {error}", annotation.id))
            })?;
            Ok(Some(lopdf_write::annotation_dictionary(
                document,
                AnnotationKind::Rect,
                rect_from(&parsed.rect),
                parse_color(parsed.color.as_deref()),
                1.0,
                contents,
            )?))
        }
        AnnotationKind::Ink => {
            let parsed: InkBlob = serde_json::from_value(blob).map_err(|error| {
                AppError::Other(format!("annotation {}: {error}", annotation.id))
            })?;
            let strokes: Vec<Vec<(f32, f32)>> = parsed
                .strokes
                .iter()
                .map(|stroke| stroke.iter().map(|point| (point[0], point[1])).collect())
                .collect();
            // A stroke with fewer than two points cannot be drawn; the builder
            // filters those and errors if nothing is left.
            // Stroke count is checked before the rect: a single tapped point
            // produces a zero-area bounds, and reporting that as "too small" would
            // misdescribe an ink row that simply has nothing to draw.
            if !strokes.iter().any(|stroke| stroke.len() >= 2) {
                return Ok(None);
            }
            let rect = strokes_bounds(&strokes);
            match lopdf_write::ink_annotation_dictionary(
                document,
                rect,
                &strokes,
                parse_color(parsed.color.as_deref()),
                1.0,
                contents,
            ) {
                Ok(dictionary) => Ok(Some(dictionary)),
                Err(error) if error.to_string().contains("no drawable strokes") => Ok(None),
                Err(error) => Err(error),
            }
        }
        // A line is lopdf-only (D-A3) and carries its geometry in `/L` rather than a rect, so
        // it needs its own builder the way ink does. `/Rect` is derived from the endpoints:
        // writing a rect that does not enclose them clips the stroke and misplaces hit-testing.
        AnnotationKind::Line { arrow } => {
            let parsed: LineBlob = serde_json::from_value(blob).map_err(|error| {
                AppError::Other(format!("annotation {}: {error}", annotation.id))
            })?;
            let from = (parsed.from[0], parsed.from[1]);
            let to = (parsed.to[0], parsed.to[1]);
            let rect = PdfRect {
                x0: from.0.min(to.0),
                y0: from.1.min(to.1),
                x1: from.0.max(to.0),
                y1: from.1.max(to.1),
            };
            match lopdf_write::line_annotation_dictionary(
                document,
                rect,
                from,
                to,
                parse_color(parsed.color.as_deref()),
                arrow,
                contents,
            ) {
                Ok(dictionary) => Ok(Some(dictionary)),
                // Coincident endpoints or a zero-area box: nothing to draw, so the row is
                // skipped rather than written as an invisible annotation.
                Err(error) if error.to_string().contains("distinct endpoints") => Err(error),
                Err(error) => Err(error),
            }
        }
        // A free-text annotation is the first kind whose *text* is part of its appearance, so
        // the blob's `rect` is the box and `contents` is what goes in it. A callout adds a
        // leader line (`/CL`) from the box to the point it refers to; that line is the only
        // thing distinguishing it from a text box.
        AnnotationKind::FreeText(mode) => {
            let parsed: ShapeBlob = serde_json::from_value(blob).map_err(|error| {
                AppError::Other(format!("annotation {}: {error}", annotation.id))
            })?;
            let rect = rect_from(&parsed.rect);
            let mut dictionary = lopdf_write::annotation_dictionary(
                document,
                AnnotationKind::FreeText(mode),
                rect,
                parse_color(parsed.color.as_deref()),
                1.0,
                contents,
            )?;

            // `/IT` is written only where it is not the default; see `appearance::it_value`.
            if let Some(it) = appearance::it_value(mode) {
                dictionary.set("IT", Object::Name(it.as_bytes().to_vec()));
            }

            // The leader line is a two-point array from the box's anchor to the target. A
            // callout without one is a text box, so it is refused rather than written as a
            // callout that looks like a box: the user drew a callout and would get a box.
            if mode == FreeTextMode::Callout {
                let Some(target) = parsed.callout else {
                    return Err(AppError::Other(
                        "a callout needs a target point for its leader line".to_string(),
                    ));
                };
                let normalised = rect.normalised();
                // The line runs from the middle of the box's bottom edge to the target: a
                // leader from a corner reads as an arrow that points at the box rather than
                // at the thing the box is talking about.
                let anchor_x = (normalised.x0 + normalised.x1) / 2.0;
                dictionary.set(
                    "CL",
                    Object::Array(vec![
                        Object::Real(anchor_x),
                        Object::Real(normalised.y0),
                        Object::Real(target[0]),
                        Object::Real(target[1]),
                    ]),
                );
            }

            Ok(Some(dictionary))
        }
    }
}

fn rect_from(rect: &[f32; 4]) -> PdfRect {
    PdfRect {
        x0: rect[0],
        y0: rect[1],
        x1: rect[2],
        y1: rect[3],
    }
}

fn union(a: PdfRect, b: PdfRect) -> PdfRect {
    PdfRect {
        x0: a.x0.min(b.x0),
        y0: a.y0.min(b.y0),
        x1: a.x1.max(b.x1),
        y1: a.y1.max(b.y1),
    }
}

fn strokes_bounds(strokes: &[Vec<(f32, f32)>]) -> PdfRect {
    let mut rect = PdfRect {
        x0: f32::MAX,
        y0: f32::MAX,
        x1: f32::MIN,
        y1: f32::MIN,
    };
    for (x, y) in strokes.iter().flatten() {
        rect.x0 = rect.x0.min(*x);
        rect.y0 = rect.y0.min(*y);
        rect.x1 = rect.x1.max(*x);
        rect.y1 = rect.y1.max(*y);
    }
    if rect.x0 > rect.x1 {
        // No points at all; a degenerate rect that the builder will reject.
        return PdfRect {
            x0: 0.0,
            y0: 0.0,
            x1: 0.0,
            y1: 0.0,
        };
    }
    rect
}

/// Parses `#rrggbb` into PDF's 0..1 component triple. An unparseable colour falls
/// back to yellow rather than failing the save: losing a highlight's colour is a
/// smaller harm than losing the user's whole edit session.
fn parse_color(color: Option<&str>) -> (f32, f32, f32) {
    const DEFAULT: (f32, f32, f32) = (1.0, 0.95, 0.69);
    let Some(color) = color else {
        return DEFAULT;
    };
    let hex = color.trim_start_matches('#');
    if hex.len() != 6 {
        return DEFAULT;
    }
    let parse = |range: std::ops::Range<usize>| {
        u8::from_str_radix(&hex[range], 16)
            .ok()
            .map(|value| value as f32 / 255.0)
    };
    match (parse(0..2), parse(2..4), parse(4..6)) {
        (Some(r), Some(g), Some(b)) => (r, g, b),
        _ => DEFAULT,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::content::{Content, Operation};
    use lopdf::dictionary;
    use lopdf::Object;

    fn sample_pdf() -> Vec<u8> {
        two_page_pdf(1)
    }

    /// A minimal valid PDF with `pages` pages, so page-number mapping can be tested.
    fn two_page_pdf(pages: usize) -> Vec<u8> {
        let mut document = Document::with_version("1.4");
        let pages_id = document.new_object_id();
        let font_id = document.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "Courier",
        });
        let resources_id = document.add_object(dictionary! {
            "Font" => dictionary! { "F1" => font_id },
        });
        let content = Content {
            operations: vec![
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec!["F1".into(), 24.into()]),
                Operation::new("Td", vec![72.into(), 700.into()]),
                Operation::new("Tj", vec![Object::string_literal("body")]),
                Operation::new("ET", vec![]),
            ],
        };
        let mut kids = Vec::new();
        for _ in 0..pages {
            // A fresh content stream per page: sharing one is legal but makes the
            // fixture harder to reason about when a test asserts on page contents.
            let content_id = document.add_object(lopdf::Stream::new(
                dictionary! {},
                content.encode().unwrap(),
            ));
            let page_id = document.add_object(dictionary! {
                "Type" => "Page",
                "Parent" => pages_id,
                "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
                "Contents" => content_id,
                "Resources" => resources_id,
            });
            kids.push(page_id.into());
        }
        document.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => kids,
                "Count" => pages as i64,
            }),
        );
        let catalog_id = document.add_object(dictionary! {
            "Type" => "Catalog",
            "Pages" => pages_id,
        });
        document.trailer.set("Root", catalog_id);
        document.renumber_objects();

        let mut bytes = Vec::new();
        document.save_to(&mut bytes).unwrap();
        bytes
    }

    fn annotation(page: u32, kind: AnnotationKind, data: &str, text: Option<&str>) -> Annotation {
        Annotation {
            id: uuid::Uuid::new_v4().to_string(),
            document_id: "d1".to_string(),
            page,
            kind,
            data: data.to_string(),
            text: text.map(str::to_string),
            created_at: 0,
            modified_at: 0,
        }
    }

    fn annotation_count(bytes: &[u8]) -> usize {
        let document = Document::load_mem(bytes).unwrap();
        document
            .get_pages()
            .values()
            .map(|page_id| lopdf_write::page_annotation_count(&document, *page_id))
            .sum()
    }

    /// The annotation dictionary written for `row`, after a save and reload.
    fn written_dictionary(bytes: &[u8], name: &str) -> lopdf::Dictionary {
        let document = Document::load_mem(bytes).unwrap();
        for page_id in document.get_pages().values() {
            let Ok(annotations) = document.get_page_annotations(*page_id) else {
                continue;
            };
            for dictionary in annotations {
                let named = dictionary
                    .get(b"NM")
                    .ok()
                    .and_then(|nm| nm.as_str().ok())
                    .map(String::from_utf8_lossy)
                    .map(|nm| nm.to_string());
                if named.as_deref() == Some(name) {
                    return dictionary.clone();
                }
            }
        }
        panic!("no annotation named {name} in the saved file");
    }

    /// A name entry, which is what `/StateModel` is (Table 171).
    fn name_of(dictionary: &lopdf::Dictionary, key: &str) -> Option<String> {
        dictionary
            .get(key.as_bytes())
            .ok()?
            .as_name()
            .ok()
            .map(|name| String::from_utf8_lossy(name).to_string())
    }

    /// A text-string entry, which is what `/State` is (Table 171).
    fn text_of(dictionary: &lopdf::Dictionary, key: &str) -> Option<String> {
        dictionary
            .get(key.as_bytes())
            .ok()?
            .as_str()
            .ok()
            .map(|text| String::from_utf8_lossy(text).to_string())
    }

    /// A highlight blob at `v: 2` carrying `status` and `parent`, which is what the
    /// comment pane writes (D-A13).
    fn highlight_blob_v2(status: Option<&str>, parent: Option<&str>) -> String {
        let mut blob = serde_json::json!({
            "v": 2,
            "rects": [[10.0, 10.0, 100.0, 20.0]],
            "color": "#fff3b0",
            "opacity": 0.4,
        });
        if let Some(status) = status {
            blob["status"] = serde_json::Value::String(status.to_string());
        }
        if let Some(parent) = parent {
            blob["parent"] = serde_json::Value::String(parent.to_string());
        }
        blob.to_string()
    }

    #[test]
    fn a_status_writes_state_and_its_review_model() {
        // D-A7: the status lives in the blob, and `/State` is what makes it visible outside
        // this app. `/StateModel` is asserted in the same test because `Accepted` under the
        // default model is a value a viewer cannot interpret -- writing one without the
        // other is the defect this guards.
        let row = annotation(
            0,
            AnnotationKind::Highlight,
            &highlight_blob_v2(Some("accepted"), None),
            None,
        );
        let bytes = apply_annotations(&sample_pdf(), std::slice::from_ref(&row)).unwrap();

        let written = written_dictionary(&bytes, &row.id);
        assert_eq!(text_of(&written, "State").as_deref(), Some("Accepted"));
        assert_eq!(name_of(&written, "StateModel").as_deref(), Some("Review"));
    }

    #[test]
    fn every_status_writes_its_own_state_name() {
        // Each of the four, because a `match` that collapses two arms into one name would
        // pass a single-status test.
        for (status, expected) in [
            ("accepted", "Accepted"),
            ("rejected", "Rejected"),
            ("completed", "Completed"),
            ("cancelled", "Cancelled"),
        ] {
            let row = annotation(
                0,
                AnnotationKind::Highlight,
                &highlight_blob_v2(Some(status), None),
                None,
            );
            let bytes = apply_annotations(&sample_pdf(), std::slice::from_ref(&row)).unwrap();
            let written = written_dictionary(&bytes, &row.id);
            assert_eq!(
                text_of(&written, "State").as_deref(),
                Some(expected),
                "status {status} wrote the wrong /State"
            );
        }
    }

    #[test]
    fn an_annotation_without_a_status_writes_no_state_keys() {
        // "none/marked" is the absence of a status, not a fifth value: writing
        // `/State (None)` would claim a triage decision nobody made.
        let row = annotation(
            0,
            AnnotationKind::Highlight,
            &highlight_blob_v2(None, None),
            None,
        );
        let bytes = apply_annotations(&sample_pdf(), std::slice::from_ref(&row)).unwrap();
        let written = written_dictionary(&bytes, &row.id);
        assert!(
            written.get(b"State").is_err(),
            "wrote /State with no status"
        );
        assert!(
            written.get(b"StateModel").is_err(),
            "wrote /StateModel with no status"
        );
    }

    #[test]
    fn a_status_this_build_does_not_know_writes_nothing() {
        // A blob from a newer build may carry a status outside the report's four. Skipping
        // it keeps the save working and keeps the file honest; inventing a `/State` for it
        // would put a guess in someone else's document.
        let row = annotation(
            0,
            AnnotationKind::Highlight,
            &highlight_blob_v2(Some("escalated"), None),
            None,
        );
        let bytes = apply_annotations(&sample_pdf(), std::slice::from_ref(&row)).unwrap();
        let written = written_dictionary(&bytes, &row.id);
        assert!(written.get(b"State").is_err());
    }

    #[test]
    fn a_v1_blob_still_writes_its_geometry() {
        // D-A13's forward-compatible promise, read in the direction that matters during a
        // hand-test install: a 1.0.x row (no `status`, no `parent`, `v: 1`) must still save.
        let row = annotation(
            0,
            AnnotationKind::Highlight,
            r##"{"v":1,"rects":[[10.0,10.0,100.0,20.0]],"color":"#fff3b0","opacity":0.4}"##,
            None,
        );
        let bytes = apply_annotations(&sample_pdf(), std::slice::from_ref(&row)).unwrap();
        assert_eq!(annotation_count(&bytes), 1);
        let written = written_dictionary(&bytes, &row.id);
        assert_eq!(name_of(&written, "Subtype").as_deref(), Some("Highlight"));
    }

    #[test]
    fn a_reply_writes_irt_naming_its_parents_object() {
        // D-A5: the file format's own "in reply to". `/IRT` is a reference to the parent
        // annotation's *object*, so this asserts the reference resolves to the object whose
        // `/NM` is the parent row's id -- not merely that some reference was written.
        let parent = annotation(
            0,
            AnnotationKind::Highlight,
            &highlight_blob_v2(None, None),
            None,
        );
        let reply = annotation(
            0,
            AnnotationKind::Note,
            &format!(r#"{{"v":2,"point":[50.0,50.0],"parent":"{}"}}"#, parent.id),
            Some("a reply"),
        );
        let bytes = apply_annotations(&sample_pdf(), &[reply.clone(), parent.clone()]).unwrap();

        let written = written_dictionary(&bytes, &reply.id);
        let target = written
            .get(b"IRT")
            .expect("/IRT missing on a reply")
            .as_reference()
            .expect("/IRT is not an indirect reference");
        let document = Document::load_mem(&bytes).unwrap();
        let parent_object = document.get_object(target).unwrap().as_dict().unwrap();
        assert_eq!(
            text_of(parent_object, "NM").as_deref(),
            Some(parent.id.as_str())
        );
    }

    #[test]
    fn a_reply_written_before_its_parent_still_finds_it() {
        // The rows arrive page-ordered, not parent-first, so `/IRT` cannot be written during
        // the pass that creates the dictionaries. This is the ordering that would fail if the
        // two-pass resolution regressed to one pass.
        let parent = annotation(
            0,
            AnnotationKind::Highlight,
            &highlight_blob_v2(None, None),
            None,
        );
        let reply = annotation(
            0,
            AnnotationKind::Note,
            &format!(r#"{{"v":2,"point":[50.0,50.0],"parent":"{}"}}"#, parent.id),
            Some("a reply"),
        );
        let bytes = apply_annotations(&sample_pdf(), &[reply.clone(), parent]).unwrap();
        let written = written_dictionary(&bytes, &reply.id);
        assert!(
            written.get(b"IRT").is_ok(),
            "a reply listed before its parent lost its /IRT"
        );
    }

    #[test]
    fn a_reply_whose_parent_is_gone_is_detached_not_dropped() {
        // D-A5 §5: the cascade deletes replies when the parent goes, but a row can still
        // outlive its parent (a save of a stale list, an import). Writing a dangling
        // reference would make the file unopenable in a strict viewer; dropping the reply
        // would lose the user's text. Detached is the only answer that keeps both.
        let reply = annotation(
            0,
            AnnotationKind::Note,
            r#"{"v":2,"point":[50.0,50.0],"parent":"00000000-0000-0000-0000-000000000000"}"#,
            Some("orphan"),
        );
        let bytes = apply_annotations(&sample_pdf(), std::slice::from_ref(&reply)).unwrap();
        assert_eq!(
            annotation_count(&bytes),
            1,
            "an orphaned reply was dropped from the file"
        );
        let written = written_dictionary(&bytes, &reply.id);
        assert!(
            written.get(b"IRT").is_err(),
            "an orphaned reply kept a dangling /IRT"
        );
    }

    #[test]
    fn a_highlight_survives_the_round_trip() {
        // P2 definition of done #4, in miniature: annotate -> save -> re-open.
        let rows = vec![annotation(
            0,
            AnnotationKind::Highlight,
            r##"{"v":1,"rects":[[72.0,700.0,300.0,720.0]],"color":"#fff3b0"}"##,
            None,
        )];

        let bytes = apply_annotations(&sample_pdf(), &rows).unwrap();
        assert_eq!(annotation_count(&bytes), 1);
    }

    #[test]
    fn every_tool_writes_an_annotation() {
        let rows = vec![
            annotation(
                0,
                AnnotationKind::Highlight,
                r##"{"v":1,"rects":[[72.0,700.0,300.0,720.0]],"color":"#ffff00"}"##,
                None,
            ),
            annotation(
                0,
                AnnotationKind::Underline,
                r##"{"v":1,"rects":[[72.0,680.0,300.0,700.0]],"color":"#0000ff"}"##,
                None,
            ),
            annotation(
                0,
                AnnotationKind::Strikeout,
                r##"{"v":1,"rects":[[72.0,660.0,300.0,680.0]],"color":"#ff0000"}"##,
                None,
            ),
            annotation(
                0,
                AnnotationKind::Note,
                r##"{"v":1,"point":[80.0,640.0],"color":"#ffcc00"}"##,
                Some("a remark"),
            ),
            annotation(
                0,
                AnnotationKind::Rect,
                r##"{"v":1,"rect":[72.0,600.0,300.0,640.0],"color":"#00ff00"}"##,
                None,
            ),
            annotation(
                0,
                AnnotationKind::Ink,
                r##"{"v":1,"strokes":[[[72.0,580.0],[120.0,600.0]]],"color":"#000000"}"##,
                None,
            ),
        ];

        let bytes = apply_annotations(&sample_pdf(), &rows).unwrap();
        assert_eq!(annotation_count(&bytes), 6);
    }

    #[test]
    fn rebuilding_is_idempotent_when_starting_from_the_original() {
        // Saving twice from the same source and DB state must not double up.
        let rows = vec![annotation(
            0,
            AnnotationKind::Highlight,
            r##"{"v":1,"rects":[[72.0,700.0,300.0,720.0]]}"##,
            None,
        )];
        let original = sample_pdf();

        let first = apply_annotations(&original, &rows).unwrap();
        let second = apply_annotations(&original, &rows).unwrap();
        assert_eq!(annotation_count(&first), 1);
        assert_eq!(annotation_count(&second), 1);
        assert_eq!(first, second, "same inputs must produce identical bytes");
    }

    #[test]
    fn applying_to_already_annotated_bytes_duplicates() {
        // Documents the trap the doc comment warns about: `bytes` must be the
        // original. This is the behaviour, asserted so it is not a surprise.
        let rows = vec![annotation(
            0,
            AnnotationKind::Highlight,
            r##"{"v":1,"rects":[[72.0,700.0,300.0,720.0]]}"##,
            None,
        )];
        let once = apply_annotations(&sample_pdf(), &rows).unwrap();
        let twice = apply_annotations(&once, &rows).unwrap();
        assert_eq!(annotation_count(&twice), 2);
    }

    #[test]
    fn an_annotation_past_the_last_page_is_skipped() {
        // Guards against a stale row landing on an unrelated page.
        let rows = vec![annotation(
            5,
            AnnotationKind::Highlight,
            r##"{"v":1,"rects":[[72.0,700.0,300.0,720.0]]}"##,
            None,
        )];
        let bytes = apply_annotations(&sample_pdf(), &rows).unwrap();
        assert_eq!(annotation_count(&bytes), 0);
    }

    #[test]
    fn a_newer_blob_version_is_ignored_rather_than_misread() {
        let rows = vec![annotation(
            0,
            AnnotationKind::Highlight,
            r##"{"v":99,"rects":[[1.0,2.0,3.0,4.0]]}"##,
            None,
        )];
        let bytes = apply_annotations(&sample_pdf(), &rows).unwrap();
        assert_eq!(annotation_count(&bytes), 0);
    }

    #[test]
    fn malformed_json_is_reported_with_the_annotation_id() {
        let row = annotation(0, AnnotationKind::Highlight, "{not json", None);
        let id = row.id.clone();
        let error = apply_annotations(&sample_pdf(), &[row]).unwrap_err();
        assert!(error.to_string().contains(&id));
    }

    #[test]
    fn multi_line_highlight_is_written_as_one_union_rect() {
        let rows = vec![annotation(
            0,
            AnnotationKind::Highlight,
            r##"{"v":1,"rects":[[72.0,700.0,300.0,720.0],[72.0,660.0,300.0,680.0]]}"##,
            None,
        )];
        let bytes = apply_annotations(&sample_pdf(), &rows).unwrap();

        let document = Document::load_mem(&bytes).unwrap();
        let page_id = *document.get_pages().values().next().unwrap();
        let annotations = document.get_page_annotations(page_id).unwrap();
        assert_eq!(annotations.len(), 1);
        let rect = annotations[0].get(b"Rect").unwrap().as_array().unwrap();
        let values: Vec<f32> = rect.iter().map(|o| o.as_float().unwrap()).collect();
        assert_eq!(
            values,
            vec![72.0, 660.0, 300.0, 720.0],
            "union of both rects"
        );
    }

    #[test]
    fn contents_are_carried_into_the_pdf() {
        let rows = vec![annotation(
            0,
            AnnotationKind::Note,
            r##"{"v":1,"point":[80.0,640.0]}"##,
            Some("remember this"),
        )];
        let bytes = apply_annotations(&sample_pdf(), &rows).unwrap();

        let document = Document::load_mem(&bytes).unwrap();
        let page_id = *document.get_pages().values().next().unwrap();
        let annotations = document.get_page_annotations(page_id).unwrap();
        assert_eq!(
            annotations[0]
                .get(b"Contents")
                .and_then(Object::as_str)
                .unwrap(),
            b"remember this"
        );
    }

    #[test]
    fn an_empty_annotation_list_returns_a_valid_document() {
        let bytes = apply_annotations(&sample_pdf(), &[]).unwrap();
        assert!(Document::load_mem(&bytes).is_ok());
        assert_eq!(annotation_count(&bytes), 0);
    }

    /// A-3's acceptance row, on the bytes rather than on the structs: save a free-text row,
    /// reopen the file, and read the dictionary a viewer would read.
    #[test]
    fn a_textbox_writes_freetext_with_an_appearance_and_no_it_key() {
        let annotations = vec![free_text_row(
            FreeTextMode::Textbox,
            r##"{"v":1,"rect":[72,600,272,660],"color":"#000000"}"##,
        )];
        let saved = apply_annotations(&sample_pdf(), &annotations).unwrap();
        let document = Document::load_mem(&saved).unwrap();
        let dictionary = only_annotation(&document);

        assert_eq!(
            dictionary
                .get(b"Subtype")
                .and_then(Object::as_name)
                .unwrap(),
            b"FreeText"
        );
        // A text box is the viewer's default, so `/IT` is absent rather than written as the
        // default. An explicit `/FreeText` would look like a distinction to a later reader.
        assert!(
            dictionary.get(b"IT").is_err(),
            "a text box must not write /IT"
        );
        assert_appearance_is_a_non_empty_form(dictionary);
        // `/DA` names the standard-14 font, so an editor reopening the annotation can resolve
        // the `Tf` it would use to re-lay-out edited text.
        let da = dictionary.get(b"DA").and_then(Object::as_str).unwrap();
        assert!(
            String::from_utf8_lossy(da).contains("/Helv"),
            "/DA must name the standard-14 font, got {da:?}"
        );
        // `/DA` is resolved against the annotation's `/DR`. A `/DA` naming a font the `/DR`
        // does not declare is a reference to nothing: an editor reopening this would fall back
        // to a default and re-lay-out the text in the wrong font. This assertion is the
        // difference between "the string says Helv" and "Helv can be looked up".
        let resources_fonts = dictionary
            .get(b"DR")
            .and_then(Object::as_dict)
            .and_then(|dr| dr.get(b"Font"))
            .and_then(Object::as_dict)
            .expect("/DR /Font is how /DA's font name resolves");
        let declared = resources_fonts
            .get(appearance::FREE_TEXT_FONT.as_bytes())
            .expect("/DR must declare the font /DA names")
            .as_reference()
            .expect("the declared font is a reference to a font dictionary");
        let font = document.get_object(declared).unwrap().as_dict().unwrap();
        assert_eq!(
            font.get(b"BaseFont").and_then(Object::as_name).unwrap(),
            b"Helvetica"
        );
    }

    #[test]
    fn a_typewriter_writes_the_typewriter_it_key() {
        let annotations = vec![free_text_row(
            FreeTextMode::Typewriter,
            r##"{"v":1,"rect":[72,600,272,660],"color":"#000000"}"##,
        )];
        let saved = apply_annotations(&sample_pdf(), &annotations).unwrap();
        let document = Document::load_mem(&saved).unwrap();
        let dictionary = only_annotation(&document);

        assert_eq!(
            dictionary.get(b"IT").and_then(Object::as_name).unwrap(),
            b"FreeTextTypewriter"
        );
        assert_appearance_is_a_non_empty_form(dictionary);
    }

    #[test]
    fn only_a_callout_writes_a_leader_line() {
        let callout = free_text_row(
            FreeTextMode::Callout,
            r##"{"v":1,"rect":[72,600,272,660],"color":"#000000","callout":[400,500]}"##,
        );
        let saved = apply_annotations(&sample_pdf(), &[callout]).unwrap();
        let document = Document::load_mem(&saved).unwrap();
        let dictionary = only_annotation(&document);
        let leader = dictionary.get(b"CL").and_then(Object::as_array).unwrap();
        assert_eq!(leader.len(), 4, "/CL is two points");
        // The line starts at the middle of the box's bottom edge and ends at the target.
        assert_eq!(leader[0].as_float().unwrap(), 172.0);
        assert_eq!(leader[1].as_float().unwrap(), 600.0);
        assert_eq!(leader[2].as_float().unwrap(), 400.0);
        assert_eq!(leader[3].as_float().unwrap(), 500.0);

        // A text box in the same shape must have no `/CL`: a leader line on a box that has
        // nothing to point at tells the viewer to draw an arrow to nowhere.
        let textbox = free_text_row(
            FreeTextMode::Textbox,
            r##"{"v":1,"rect":[72,600,272,660],"color":"#000000"}"##,
        );
        let saved = apply_annotations(&sample_pdf(), &[textbox]).unwrap();
        let document = Document::load_mem(&saved).unwrap();
        assert!(
            only_annotation(&document).get(b"CL").is_err(),
            "a text box must not write /CL"
        );
    }

    #[test]
    fn a_callout_without_a_target_is_refused() {
        // Not written as a plain box: the user drew a callout, and a box is a different mark.
        let callout = free_text_row(
            FreeTextMode::Callout,
            r##"{"v":1,"rect":[72,600,272,660],"color":"#000000"}"##,
        );
        let error = apply_annotations(&sample_pdf(), &[callout]).unwrap_err();
        assert!(error.to_string().contains("leader line"), "got {error}");
    }

    #[test]
    fn a_free_text_annotation_with_no_text_is_refused() {
        // An empty frame is a mark the user cannot explain and cannot select meaningfully.
        let empty = free_text_row(
            FreeTextMode::Textbox,
            r##"{"v":1,"rect":[72,600,272,660],"color":"#000000"}"##,
        );
        let mut empty = empty;
        empty.text = None;
        let error = apply_annotations(&sample_pdf(), &[empty]).unwrap_err();
        assert!(error.to_string().contains("needs text"), "got {error}");
    }

    fn free_text_row(mode: FreeTextMode, data: &str) -> Annotation {
        Annotation {
            id: format!("free-text-{}", mode_text(mode)),
            document_id: "doc-1".to_string(),
            page: 0,
            kind: AnnotationKind::FreeText(mode),
            data: data.to_string(),
            text: Some("a note in the margin".to_string()),
            created_at: 0,
            modified_at: 0,
        }
    }

    fn mode_text(mode: FreeTextMode) -> &'static str {
        match mode {
            FreeTextMode::Textbox => "textbox",
            FreeTextMode::Callout => "callout",
            FreeTextMode::Typewriter => "typewriter",
        }
    }

    fn only_annotation(document: &Document) -> &lopdf::Dictionary {
        let page_id = *document.get_pages().values().next().unwrap();
        let annotations = document.get_page_annotations(page_id).unwrap();
        assert_eq!(annotations.len(), 1, "expected exactly one annotation");
        annotations[0]
    }

    fn assert_appearance_is_a_non_empty_form(dictionary: &lopdf::Dictionary) {
        let normal = dictionary
            .get(b"AP")
            .and_then(Object::as_dict)
            .and_then(|ap| ap.get(b"N"))
            .expect("/AP /N is what a non-Adobe viewer draws");
        assert!(
            normal.as_reference().is_ok(),
            "/AP /N must be a stream reference, not an inline dictionary"
        );
    }

    /// A-4: D-A8 says ink gets an `/AP` stream. Nothing synthesises one -- `/InkList` is a
    /// list of point paths with no line thickness, no joins and no way to say what a stroke
    /// looks like without re-deriving it at render time -- so without `/AP` an ink stroke is
    /// visible in Adobe (which reads `/InkList`) and invisible in every viewer that draws only
    /// appearances. This is the whole point of the row: the mark must not save-and-vanish.
    #[test]
    fn an_ink_annotation_writes_an_appearance_stream() {
        let annotations = vec![ink_row(
            r##"{"v":1,"color":"#ff0000","strokes":[[[72,600],[140,640],[200,600]]]}"##,
        )];
        let saved = apply_annotations(&sample_pdf(), &annotations).unwrap();
        let document = Document::load_mem(&saved).unwrap();
        let dictionary = only_annotation(&document);

        assert_eq!(
            dictionary
                .get(b"Subtype")
                .and_then(Object::as_name)
                .unwrap(),
            b"Ink"
        );
        assert_appearance_is_a_non_empty_form(dictionary);
    }

    /// The appearance must draw the strokes, not a box around them. A filled `/Rect` would
    /// satisfy every structural assertion above while being a different mark on the page, so
    /// this reads the stream's own content and requires the path operators and the point
    /// coordinates the blob holds.
    #[test]
    fn the_ink_appearance_draws_the_stroke_path_not_a_bounding_box() {
        let annotations = vec![ink_row(
            r##"{"v":1,"color":"#ff0000","strokes":[[[72,600],[140,640],[200,600]]]}"##,
        )];
        let saved = apply_annotations(&sample_pdf(), &annotations).unwrap();
        let document = Document::load_mem(&saved).unwrap();
        let dictionary = only_annotation(&document);

        let stream = appearance_stream_of(&document, dictionary);
        assert_eq!(
            stream
                .dict
                .get(b"Subtype")
                .and_then(Object::as_name)
                .unwrap(),
            b"Form",
            "the appearance must be a form XObject"
        );
        let content = String::from_utf8_lossy(&stream.content).to_string();

        // The three points of the stroke, translated into the appearance's own box space.
        // The box is the stroke's bounds, so the first point is the origin corner.
        // Operators are checked as whole tokens on their own lines: `contains(" S")` would
        // miss a bare `S` operator line and pass on any word containing "S".
        let operators: Vec<&str> = content.lines().map(str::trim).collect();
        assert!(
            operators
                .iter()
                .any(|line| line.ends_with(" m") || *line == "m"),
            "a stroke needs a moveto, got {content:?}"
        );
        assert!(
            operators.iter().any(|line| line.ends_with(" l")),
            "a stroke needs a lineto, got {content:?}"
        );
        assert!(
            operators.contains(&"S"),
            "the stroke must be stroked, not filled, got {content:?}"
        );
        assert!(
            !operators.iter().any(|line| line.ends_with(" re")),
            "an ink appearance must not draw a rectangle, got {content:?}"
        );
        // The path must be in the appearance's own box space, not the page's. The box is
        // the stroke's bounds, so the first point lands exactly on the origin and the apex is
        // the box's height. Asserting on the parsed numbers rather than a substring matters:
        // a substring check for 40 also matches a page-space y of 640, so it would pass on a
        // stream that draws the stroke off its own appearance.
        let moveto = operators
            .iter()
            .find(|line| line.ends_with(" m"))
            .expect("a moveto");
        assert_eq!(
            *moveto, "0 0 m",
            "the first point must be the box origin, got {content:?}"
        );
        let apex = operators
            .iter()
            .find(|line| line.ends_with(" l"))
            .expect("a lineto");
        assert_eq!(
            *apex, "68 40 l",
            "the interior vertex must be in box space (68, 40), got {content:?}"
        );
    }

    /// The `/Rect` and the `/AP /BBox` must describe the same box. If they disagree the
    /// viewer positions the appearance against one and hit-tests against the other, so the
    /// user clicks where the stroke is not.
    #[test]
    fn the_ink_appearance_box_matches_the_annotation_rect() {
        let annotations = vec![ink_row(
            r##"{"v":1,"color":"#000000","strokes":[[[72,600],[140,640],[200,600]]]}"##,
        )];
        let saved = apply_annotations(&sample_pdf(), &annotations).unwrap();
        let document = Document::load_mem(&saved).unwrap();
        let dictionary = only_annotation(&document);

        let rect: Vec<f32> = numbers(dictionary.get(b"Rect").unwrap());
        let stream = appearance_stream_of(&document, dictionary);
        let bbox: Vec<f32> = numbers(stream.dict.get(b"BBox").unwrap());

        // The BBox is in the appearance's own space, so it starts at the origin and is the
        // rect's *size*; the rect carries the position. Equal size is the invariant.
        let rect_width = rect[2] - rect[0];
        let rect_height = rect[3] - rect[1];
        let (bbox_width, bbox_height) = (bbox[2] - bbox[0], bbox[3] - bbox[1]);
        assert!(
            (rect_width - bbox_width).abs() < 0.01 && (rect_height - bbox_height).abs() < 0.01,
            "appearance box {bbox:?} does not match rect {rect:?}"
        );
    }

    /// An ink row with nothing drawable must still be skipped rather than written with an
    /// empty appearance: a form XObject drawing nothing is a mark the user cannot see or
    /// delete, which is worse than the row not being written at all.
    #[test]
    fn an_ink_row_with_no_drawable_strokes_writes_no_appearance() {
        let annotations = vec![ink_row(r##"{"v":1,"strokes":[[[72,600]]]}"##)];
        let saved = apply_annotations(&sample_pdf(), &annotations).unwrap();
        let document = Document::load_mem(&saved).unwrap();
        let page_id = *document.get_pages().values().next().unwrap();
        assert_eq!(
            document.get_page_annotations(page_id).unwrap().len(),
            0,
            "a dead ink row must not be written"
        );
    }

    /// A-11 (post-landing defect D1): the free-text `/Rect` and its `/AP /BBox` must describe
    /// the same box, the way the ink pair already does.
    ///
    /// This is the guard whose absence let the 1.1.0 misalignment ship. Ink has
    /// `the_ink_appearance_box_matches_the_annotation_rect`; free text had no equivalent, so
    /// the caller could pass a width/height that was not the rect's and nothing noticed. It
    /// still holds that invariant, and additionally pins the box's position so a flipped rect
    /// (the classic y-flip bug) cannot pass by having the right size in the wrong place.
    #[test]
    fn the_free_text_appearance_box_matches_the_annotation_rect() {
        let annotations = vec![free_text_row(
            FreeTextMode::Textbox,
            r##"{"v":1,"rect":[72,600,272,660],"color":"#000000"}"##,
        )];
        let saved = apply_annotations(&sample_pdf(), &annotations).unwrap();
        let document = Document::load_mem(&saved).unwrap();
        let dictionary = only_annotation(&document);

        let rect: Vec<f32> = numbers(dictionary.get(b"Rect").unwrap());
        let stream = appearance_stream_of(&document, dictionary);
        let bbox: Vec<f32> = numbers(stream.dict.get(b"BBox").unwrap());

        // The BBox is in the appearance's own space, so it starts at the origin and is the
        // rect's *size*; the rect carries the position. Equal size is the invariant.
        let rect_width = rect[2] - rect[0];
        let rect_height = rect[3] - rect[1];
        let (bbox_width, bbox_height) = (bbox[2] - bbox[0], bbox[3] - bbox[1]);
        assert!(
            (rect_width - bbox_width).abs() < 0.01 && (rect_height - bbox_height).abs() < 0.01,
            "appearance box {bbox:?} does not match rect {rect:?}"
        );
        assert_eq!(
            (bbox[0], bbox[1]),
            (0.0, 0.0),
            "a non-origin BBox would double-displace the text: the viewer already \
             positions the form at /Rect"
        );
        // The stored rect must be the one the user swept, in PDF space: y grows upward, so a
        // box drawn below the middle of the page has y0 < y1 and both inside the page box.
        assert!(
            rect[1] < rect[3] && rect[0] < rect[2],
            "rect is inverted: {rect:?}"
        );

        // The text must actually land inside that box. `BBox` alone only says how big the
        // frame is; the placement is in the stream.
        let content = String::from_utf8_lossy(stream.content.as_slice()).to_string();
        let first_td = content
            .find(" Td")
            .map(|at| content[..at].rsplit('\n').next().unwrap_or(""))
            .expect("the stream must position the text");
        let mut numbers_in_td = first_td.split_whitespace();
        let _x = numbers_in_td.next();
        let y: f32 = numbers_in_td
            .next()
            .and_then(|value| value.parse().ok())
            .unwrap_or_else(|| panic!("no numeric y in {first_td:?}"));
        assert!(
            (0.0..=bbox_height).contains(&y),
            "first baseline y={y} is outside the box height {bbox_height}"
        );
    }

    /// A-11: a box shorter than one line still draws its text.
    ///
    /// The first baseline used to be the full font size unconditionally, so the truncation
    /// check `baseline_from_top > height` fired on the very first line and the stream came out
    /// with `BT` and `ET` and no `Tj` at all. The annotation was in the comment pane, in the
    /// file, and invisible on the page. Found by rendering a real saved file through PDFium:
    /// a 10pt-tall box produced zero dark pixels.
    #[test]
    fn a_free_text_box_shorter_than_one_line_still_draws_its_text() {
        let annotations = vec![free_text_row(
            FreeTextMode::Textbox,
            r##"{"v":1,"rect":[72,600,272,610],"color":"#000000"}"##,
        )];
        let saved = apply_annotations(&sample_pdf(), &annotations).unwrap();
        let document = Document::load_mem(&saved).unwrap();
        let dictionary = only_annotation(&document);
        let stream = appearance_stream_of(&document, dictionary);
        let content = String::from_utf8_lossy(stream.content.as_slice()).to_string();
        assert!(
            content.contains("Tj"),
            "a 10pt box drew no text at all: {content:?}"
        );
    }

    fn ink_row(data: &str) -> Annotation {
        Annotation {
            id: "ink-1".to_string(),
            document_id: "doc-1".to_string(),
            page: 0,
            kind: AnnotationKind::Ink,
            data: data.to_string(),
            text: None,
            created_at: 0,
            modified_at: 0,
        }
    }

    /// Collects every content stream on page 0 as **raw bytes**, so a test can look for an
    /// operator or a literal without caring which stream the writer put it in.
    ///
    /// Bytes rather than `String` because the label contains the WinAnsi em dash `0x97`, which is
    /// not valid UTF-8 on its own: `from_utf8_lossy` replaces it with U+FFFD and would make the
    /// encoding untestable, which is the one thing about the label most likely to break.
    fn page_0_content(document: &Document) -> Vec<u8> {
        let pages = document.get_pages();
        let page_id = *pages.get(&1).expect("page 1");
        let page = document.get_object(page_id).unwrap().as_dict().unwrap();
        let contents = page.get(b"Contents").unwrap();
        let ids: Vec<lopdf::ObjectId> = match contents {
            Object::Reference(id) => vec![*id],
            Object::Array(items) => items
                .iter()
                .filter_map(|item| item.as_reference().ok())
                .collect(),
            other => panic!("unexpected /Contents: {other:?}"),
        };
        let mut all = Vec::new();
        for id in ids {
            let stream = document.get_object(id).unwrap().as_stream().unwrap();
            all.extend_from_slice(&stream.content);
            all.push(b'\n');
        }
        all
    }

    fn contains_bytes(haystack: &[u8], needle: &str) -> bool {
        haystack
            .windows(needle.len())
            .any(|window| window == needle.as_bytes())
    }

    /// A **drawn** signature puts the disclosure in page content, positioned under the mark.
    ///
    /// This is the case a naive implementation gets wrong and reports success on. An ink row's
    /// blob carries `strokes`, not a `rect`, so a label placer that read `blob["rect"]` would find
    /// nothing, place no label, and return `Ok` -- a saved document with a signature and no
    /// disclosure, which is precisely what D-N2 forbids. The test therefore starts from a real
    /// ink blob with no rect in it at all.
    #[test]
    fn a_drawn_signature_carries_its_label_in_page_content() {
        let annotations = vec![ink_row(
            r##"{"v":2,"visualSignature":true,"strokes":[[[100.0,500.0],[140.0,540.0],[180.0,500.0]]],"color":"#000000"}"##,
        )];
        let saved = apply_annotations(&sample_pdf(), &annotations).unwrap();
        let document = Document::load_mem(&saved).unwrap();
        let content = page_0_content(&document);

        assert!(
            contains_bytes(&content, "Visual signature"),
            "the label must be in page content, not only on the annotation"
        );
        // The em dash is WinAnsi 0x97, not UTF-8 `E2 80 94`. Asserting on the byte is what proves
        // the encoding rather than proving "some bytes that roughly spell the label".
        assert!(
            content.contains(&0x97),
            "the em dash must be the WinAnsi byte 0x97 in the content stream"
        );
        assert!(
            !content.windows(3).any(|w| w == [0xE2, 0x80, 0x94]),
            "the em dash must not be written as UTF-8; a WinAnsi font would draw three glyphs"
        );
        // The label uses its own font, and is positioned from the mark's own left edge: the ink
        // strokes span x 100-180, y 500-540, so the baseline is just under y=500 at x=100.
        assert!(
            contains_bytes(&content, "SignLbl"),
            "the label must be drawn with its own font resource"
        );
        assert!(
            contains_bytes(&content, "100") && contains_bytes(&content, "498"),
            "the label must sit at the mark's left edge, just below its bottom: 100 / ~498"
        );
    }

    /// The same label is written for a **typed** signature, which is a `FreeText` row with a rect.
    #[test]
    fn a_typed_signature_carries_its_label_in_page_content() {
        let annotations = vec![free_text_row(
            FreeTextMode::Textbox,
            r##"{"v":2,"visualSignature":true,"rect":[72,600,272,640],"color":"#000000"}"##,
        )];
        let saved = apply_annotations(&sample_pdf(), &annotations).unwrap();
        let document = Document::load_mem(&saved).unwrap();
        assert!(
            contains_bytes(&page_0_content(&document), "Visual signature"),
            "a typed signature carries the same mandatory disclosure as a drawn one"
        );
    }

    /// An ordinary ink mark is **not** a signature and gets no label.
    ///
    /// The negative half of D-N2. Without it, "the label appears" could be satisfied by labelling
    /// every ink mark, which would put a legal disclosure on a hand-drawn arrow.
    #[test]
    fn an_ordinary_ink_mark_is_not_labelled_as_a_signature() {
        let annotations = vec![ink_row(
            r##"{"v":2,"strokes":[[[100.0,500.0],[140.0,540.0]]],"color":"#000000"}"##,
        )];
        let saved = apply_annotations(&sample_pdf(), &annotations).unwrap();
        let document = Document::load_mem(&saved).unwrap();
        assert!(
            !contains_bytes(&page_0_content(&document), "Visual signature"),
            "a plain ink mark must not be labelled as a signature"
        );
    }

    /// A row that claims to be a signature but draws nothing is refused, not saved unlabelled.
    ///
    /// This is the honest failure. A single-point stroke has no extent, so there is no mark and
    /// therefore nothing to disclose; writing the file anyway would produce a document that
    /// silently lost the user's signature while the save reported success.
    #[test]
    fn a_signature_that_draws_nothing_is_refused_rather_than_saved_unlabelled() {
        let annotations = vec![ink_row(
            r##"{"v":2,"visualSignature":true,"strokes":[[[100.0,500.0]]],"color":"#000000"}"##,
        )];
        let error = apply_annotations(&sample_pdf(), &annotations)
            .expect_err("a signature with no drawable mark must not save silently");
        assert!(
            error.to_string().contains("carry no required label"),
            "the refusal must say why; got {error}"
        );
    }

    /// Reads an array of reals as floats. lopdf preserves the written numeric type, so a
    /// value may come back as either `Real` or `Integer`.
    fn numbers(object: &Object) -> Vec<f32> {
        object
            .as_array()
            .unwrap()
            .iter()
            .map(|item| match item {
                Object::Real(value) => *value,
                Object::Integer(value) => *value as f32,
                other => panic!("expected a number, got {other:?}"),
            })
            .collect()
    }

    /// A-5: a line is lopdf-only (D-A3), and unlike markup it cannot lean on `/QuadPoints`
    /// synthesis -- a viewer has no way to know a line's two endpoints from a rect, so the
    /// endpoints must be written as `/L` *and* drawn in an appearance (D-A8).
    #[test]
    fn a_line_writes_its_two_endpoints_and_an_appearance() {
        let annotations = vec![line_row(
            false,
            r##"{"v":1,"color":"#000000","from":[72,600],"to":[300,640]}"##,
        )];
        let saved = apply_annotations(&sample_pdf(), &annotations).unwrap();
        let document = Document::load_mem(&saved).unwrap();
        let dictionary = only_annotation(&document);

        assert_eq!(
            dictionary
                .get(b"Subtype")
                .and_then(Object::as_name)
                .unwrap(),
            b"Line"
        );
        let line = numbers(dictionary.get(b"L").expect("/L carries the endpoints"));
        assert_eq!(line, vec![72.0, 600.0, 300.0, 640.0]);
        assert_appearance_is_a_non_empty_form(dictionary);
    }

    /// An arrow is a line with a head, and the head is recorded as `/LE`, not as geometry.
    /// The distinction has to live in the dictionary: a viewer that draws `/L` alone would
    /// render an arrow as a plain line, which is a different mark.
    #[test]
    fn an_arrow_writes_an_arrowhead_in_le_and_a_plain_line_writes_none() {
        // `OpenArrow` is a member of /LE's closed value set (ISO 32000-1 Table 179); `Arrow`
        // is not, and a viewer would silently ignore it.
        for (arrow, expected) in [(false, None::<&str>), (true, Some("OpenArrow"))] {
            let annotations = vec![line_row(
                arrow,
                r##"{"v":1,"color":"#000000","from":[72,600],"to":[300,640]}"##,
            )];
            let saved = apply_annotations(&sample_pdf(), &annotations).unwrap();
            let document = Document::load_mem(&saved).unwrap();
            let dictionary = only_annotation(&document);

            let le = dictionary
                .get(b"LE")
                .ok()
                .and_then(|object| object.as_array().ok())
                .map(|array| {
                    array
                        .iter()
                        .map(|item| String::from_utf8_lossy(item.as_name().unwrap()).to_string())
                        .collect::<Vec<_>>()
                });
            match expected {
                None => assert!(le.is_none(), "a plain line must write no /LE, got {le:?}"),
                Some(head) => {
                    let le = le.expect("an arrow must write /LE");
                    assert_eq!(le, vec!["None".to_string(), head.to_string()]);
                }
            }
            assert_appearance_is_a_non_empty_form(dictionary);
        }
    }

    /// A line whose endpoints coincide is a click, not a mark. It must be refused rather than
    /// written as a zero-length annotation with an empty appearance, because a viewer then has
    /// a selectable object on the page that draws nothing.
    #[test]
    fn a_line_with_coincident_endpoints_is_refused() {
        let annotations = vec![line_row(
            false,
            r##"{"v":1,"color":"#000000","from":[72,600],"to":[72,600]}"##,
        )];
        let error = apply_annotations(&sample_pdf(), &annotations).unwrap_err();
        assert!(
            error.to_string().contains("too small") || error.to_string().contains("endpoint"),
            "got {error}"
        );
    }

    /// The appearance must draw a stroked path between the endpoints, not a filled box: a
    /// filled rect over a diagonal line is a different mark, and would satisfy any structural
    /// check on `/AP` alone.
    #[test]
    fn the_line_appearance_strokes_the_diagonal_not_a_box() {
        let annotations = vec![line_row(
            false,
            r##"{"v":1,"color":"#000000","from":[72,600],"to":[300,640]}"##,
        )];
        let saved = apply_annotations(&sample_pdf(), &annotations).unwrap();
        let document = Document::load_mem(&saved).unwrap();
        let dictionary = only_annotation(&document);

        let stream = appearance_stream_of(&document, dictionary);
        let content = String::from_utf8_lossy(&stream.content).to_string();
        let operators: Vec<&str> = content.lines().map(str::trim).collect();

        assert!(
            operators.contains(&"S"),
            "the line must be stroked, got {content:?}"
        );
        assert!(
            !operators.iter().any(|line| line.ends_with(" re")),
            "a line appearance must not draw a rectangle, got {content:?}"
        );
        // In box space the endpoints are opposite corners of the 228x40 box.
        assert!(
            operators.contains(&"0 0 m"),
            "the start point must be the box origin, got {content:?}"
        );
        assert!(
            operators.contains(&"228 40 l"),
            "the end point must be the far corner, got {content:?}"
        );
    }

    /// The arrowhead has to be drawn, not just declared in `/LE`. A viewer that honours `/LE`
    /// draws its own; one that only draws `/AP` gets whatever the appearance contains, and
    /// `/LE` alone would leave it a plain line.
    #[test]
    fn the_arrow_appearance_draws_a_head_the_plain_line_does_not() {
        let plain = {
            let annotations = vec![line_row(
                false,
                r##"{"v":1,"color":"#000000","from":[72,600],"to":[300,640]}"##,
            )];
            let saved = apply_annotations(&sample_pdf(), &annotations).unwrap();
            let document = Document::load_mem(&saved).unwrap();
            let dictionary = only_annotation(&document);
            let stream = appearance_stream_of(&document, dictionary);
            String::from_utf8_lossy(&stream.content).to_string()
        };
        let arrow = {
            let annotations = vec![line_row(
                true,
                r##"{"v":1,"color":"#000000","from":[72,600],"to":[300,640]}"##,
            )];
            let saved = apply_annotations(&sample_pdf(), &annotations).unwrap();
            let document = Document::load_mem(&saved).unwrap();
            let dictionary = only_annotation(&document);
            let stream = appearance_stream_of(&document, dictionary);
            String::from_utf8_lossy(&stream.content).to_string()
        };

        // The head adds a filled triangle, so the arrow's stream is longer and carries a
        // fill the plain line has no reason to.
        assert!(
            arrow.matches(" l").count() > plain.matches(" l").count(),
            "the arrow must draw head segments, plain={plain:?} arrow={arrow:?}"
        );
        assert!(
            !plain.contains(" f\n") && !plain.contains("\nf "),
            "a plain line must not fill anything, got {plain:?}"
        );
    }

    fn line_row(arrow: bool, data: &str) -> Annotation {
        Annotation {
            id: if arrow { "arrow-1" } else { "line-1" }.to_string(),
            document_id: "doc-1".to_string(),
            page: 0,
            kind: AnnotationKind::Line { arrow },
            data: data.to_string(),
            text: None,
            created_at: 0,
            modified_at: 0,
        }
    }

    /// A-6: an oval is `/Subtype /Circle`, and the circle is *inscribed in* `/Rect`. That is
    /// the spec's definition (ISO 32000-1 12.5.6.8), and it is why a non-square rect gives an
    /// ellipse rather than a clipped circle: the rect is the bounding box, not a hint.
    #[test]
    fn an_oval_writes_a_circle_subtype_inscribed_in_its_rect() {
        let annotations = vec![oval_row(
            r##"{"v":1,"color":"#000000","rect":[72,600,272,700]}"##,
        )];
        let saved = apply_annotations(&sample_pdf(), &annotations).unwrap();
        let document = Document::load_mem(&saved).unwrap();
        let dictionary = only_annotation(&document);

        // `/Circle`, not `/Oval` and not `/Square`: the spec's name for this subtype is
        // Circle even though a non-square rect renders as an ellipse.
        assert_eq!(
            dictionary
                .get(b"Subtype")
                .and_then(Object::as_name)
                .unwrap(),
            b"Circle"
        );
        assert_eq!(
            numbers(dictionary.get(b"Rect").unwrap()),
            vec![72.0, 600.0, 272.0, 700.0]
        );
        assert_appearance_is_a_non_empty_form(dictionary);
    }

    /// The appearance must draw the ellipse, not the bounding rectangle. A rectangle would
    /// satisfy every structural check on `/Subtype` and `/Rect` while being a visibly
    /// different mark, and this is the assertion that separates them.
    #[test]
    fn the_oval_appearance_draws_a_curve_not_the_bounding_box() {
        let annotations = vec![oval_row(
            r##"{"v":1,"color":"#000000","rect":[72,600,272,700]}"##,
        )];
        let saved = apply_annotations(&sample_pdf(), &annotations).unwrap();
        let document = Document::load_mem(&saved).unwrap();
        let dictionary = only_annotation(&document);
        let stream = appearance_stream_of(&document, dictionary);
        let content = String::from_utf8_lossy(&stream.content).to_string();
        let operators: Vec<&str> = content.lines().map(str::trim).collect();

        // Bezier curves are what a PDF ellipse is built from; four of them is the standard
        // construction (one per quadrant).
        assert_eq!(
            operators.iter().filter(|line| line.ends_with(" c")).count(),
            4,
            "an ellipse needs four Bezier segments, got {content:?}"
        );
        assert!(
            !operators.iter().any(|line| line.ends_with(" re")),
            "an oval must not draw its bounding rectangle, got {content:?}"
        );
        // The four quadrant points of a 200x100 box, in box space.
        for corner in ["200 50 m", "100 0", "0 50", "100 100"] {
            assert!(
                content.contains(corner),
                "the ellipse's quadrant point {corner} is missing, got {content:?}"
            );
        }

        // The quadrant points alone do not make an ellipse: four curves that meet at the same
        // four points bulge differently depending on where their control points sit, and a
        // wrong constant still lands exactly on those points. The first segment runs from the
        // right quadrant point (200, 50) to the top one (100, 100). Its first control point sits
        // straight above the start by KAPPA * ry = 0.5522848 * 50 = 27.61424, and its second
        // left of the end by KAPPA * rx = 55.22848. Asserting those two numbers is what
        // separates an ellipse from a bulged quadrilateral.
        let first_curve = operators
            .iter()
            .find(|line| line.ends_with(" c"))
            .expect("a curve");
        assert_eq!(
            *first_curve, "200 77.61424 155.22849 100 100 100 c",
            "the first curve's control points are wrong, got {content:?}"
        );
    }

    /// An oval in a zero-area box is a click, not a mark. Refused for the same reason a line
    /// with coincident endpoints is: a viewer would hold a selectable object drawing nothing.
    #[test]
    fn an_oval_in_a_dead_box_is_refused() {
        let annotations = vec![oval_row(
            r##"{"v":1,"color":"#000000","rect":[72,600,72,600]}"##,
        )];
        let error = apply_annotations(&sample_pdf(), &annotations).unwrap_err();
        assert!(error.to_string().contains("too small"), "got {error}");
    }

    fn oval_row(data: &str) -> Annotation {
        Annotation {
            id: "oval-1".to_string(),
            document_id: "doc-1".to_string(),
            page: 0,
            kind: AnnotationKind::Oval,
            data: data.to_string(),
            text: None,
            created_at: 0,
            modified_at: 0,
        }
    }

    /// Resolves `/AP /N` to the stream it references.
    fn appearance_stream_of<'a>(
        document: &'a Document,
        dictionary: &lopdf::Dictionary,
    ) -> &'a lopdf::Stream {
        let id = dictionary
            .get(b"AP")
            .and_then(Object::as_dict)
            .and_then(|ap| ap.get(b"N"))
            .and_then(Object::as_reference)
            .expect("/AP /N must be a stream reference");
        document
            .get_object(id)
            .unwrap()
            .as_stream()
            .expect("/AP /N must resolve to a stream")
    }

    #[test]
    fn pre_existing_foreign_annotations_are_preserved() {
        // An annotation made in another app is not in our DB and must survive.
        let original = sample_pdf();
        let mut document = Document::load_mem(&original).unwrap();
        let page_id = *document.get_pages().values().next().unwrap();
        let foreign = lopdf_write::annotation_dictionary(
            &mut document,
            AnnotationKind::Highlight,
            PdfRect {
                x0: 10.0,
                y0: 10.0,
                x1: 100.0,
                y1: 30.0,
            },
            (0.0, 1.0, 0.0),
            0.4,
            None,
        )
        .unwrap();
        lopdf_write::attach_annotation(&mut document, page_id, foreign).unwrap();
        let with_foreign = lopdf_write::save_to_bytes(&mut document).unwrap();

        let rows = vec![annotation(
            0,
            AnnotationKind::Underline,
            r##"{"v":1,"rects":[[72.0,700.0,300.0,720.0]]}"##,
            None,
        )];
        let bytes = apply_annotations(&with_foreign, &rows).unwrap();
        assert_eq!(annotation_count(&bytes), 2);
    }

    #[test]
    fn colour_parsing_handles_hex_and_falls_back_safely() {
        assert_eq!(parse_color(Some("#ff0000")), (1.0, 0.0, 0.0));
        assert_eq!(parse_color(Some("00ff00")), (0.0, 1.0, 0.0));
        assert_eq!(parse_color(None), (1.0, 0.95, 0.69));
        assert_eq!(parse_color(Some("nonsense")), (1.0, 0.95, 0.69));
        assert_eq!(parse_color(Some("#fff")), (1.0, 0.95, 0.69));
    }

    #[test]
    fn an_ink_row_with_no_drawable_strokes_is_skipped_not_fatal() {
        let rows = vec![annotation(
            0,
            AnnotationKind::Ink,
            r##"{"v":1,"strokes":[[[10.0,10.0]]]}"##,
            None,
        )];
        let bytes = apply_annotations(&sample_pdf(), &rows).unwrap();
        assert_eq!(annotation_count(&bytes), 0);
    }

    #[test]
    fn a_note_without_a_point_is_skipped() {
        let rows = vec![annotation(1, AnnotationKind::Note, r##"{"v":1}"##, None)];
        let bytes = apply_annotations(&sample_pdf(), &rows).unwrap();
        assert_eq!(annotation_count(&bytes), 0);
    }

    #[test]
    fn corrupted_input_bytes_are_reported() {
        let error = apply_annotations(b"not a pdf at all", &[]).unwrap_err();
        assert!(error.to_string().contains("could not read pdf"));
    }

    #[test]
    fn page_zero_maps_to_the_first_pdf_page() {
        // Regression: the DB/UI use 0-based page indices, lopdf's get_pages() is 1-based.
        // Getting this wrong puts every annotation one page late and silently loses the
        // first page's entirely, so this asserts the *location*, not just a count.
        let rows = vec![annotation(
            0,
            AnnotationKind::Highlight,
            r##"{"v":1,"rects":[[72.0,700.0,300.0,720.0]]}"##,
            None,
        )];

        let bytes = apply_annotations(&two_page_pdf(2), &rows).unwrap();

        let document = Document::load_mem(&bytes).unwrap();
        let pages = document.get_pages();
        let first = pages.get(&1).copied().unwrap();
        let second = pages.get(&2).copied().unwrap();
        assert_eq!(lopdf_write::page_annotation_count(&document, first), 1);
        assert_eq!(
            lopdf_write::page_annotation_count(&document, second),
            0,
            "annotation belongs to page 0, not page 1"
        );
    }

    #[test]
    fn the_last_page_is_addressable() {
        let rows = vec![annotation(
            1,
            AnnotationKind::Highlight,
            r##"{"v":1,"rects":[[72.0,700.0,300.0,720.0]]}"##,
            None,
        )];

        let bytes = apply_annotations(&two_page_pdf(2), &rows).unwrap();
        let document = Document::load_mem(&bytes).unwrap();
        let pages = document.get_pages();
        let second = pages.get(&2).copied().unwrap();
        assert_eq!(lopdf_write::page_annotation_count(&document, second), 1);
    }

    #[test]
    fn one_annotation_per_page_lands_on_its_own_page() {
        let rows = vec![
            annotation(
                0,
                AnnotationKind::Highlight,
                r##"{"v":1,"rects":[[10.0,700.0,60.0,720.0]]}"##,
                None,
            ),
            annotation(
                1,
                AnnotationKind::Highlight,
                r##"{"v":1,"rects":[[20.0,600.0,70.0,620.0]]}"##,
                None,
            ),
        ];

        let bytes = apply_annotations(&two_page_pdf(2), &rows).unwrap();
        let document = Document::load_mem(&bytes).unwrap();
        let pages = document.get_pages();
        let first = pages.get(&1).copied().unwrap();
        let second = pages.get(&2).copied().unwrap();

        let rect_of = |page_id| {
            document.get_page_annotations(page_id).unwrap()[0]
                .get(b"Rect")
                .unwrap()
                .as_array()
                .unwrap()[0]
                .as_float()
                .unwrap()
        };
        assert_eq!(rect_of(first), 10.0);
        assert_eq!(rect_of(second), 20.0);
    }

    #[test]
    #[ignore = "requires a real PDFium library; `cargo test -- --include-ignored` after fetching it"]
    fn a_saved_document_reopens_in_pdfium_with_the_annotation_on_the_right_page() {
        use crate::provider::PdfProvider;

        // P2 §7 item 4: annotate -> save -> re-open -> annotations present. Every test
        // above reads the result back with *lopdf*, which is the same library that wrote
        // it — so they cannot tell a correct annotation from one lopdf accepts and
        // PDFium rejects. PDFium is what renders the file for the user, so it is the only
        // reader that can confirm the round trip (and F1: PDFium reads annotations even
        // though it cannot create them, so `len()` is a real check).
        let rows = vec![
            annotation(
                0,
                AnnotationKind::Highlight,
                r##"{"v":1,"rects":[[20.0,600.0,70.0,620.0]]}"##,
                None,
            ),
            annotation(
                1,
                AnnotationKind::Note,
                r##"{"v":1,"point":[100.0,200.0],"text":"a note"}"##,
                Some("a note"),
            ),
        ];

        let bytes = apply_annotations(&two_page_pdf(2), &rows).unwrap();

        // Held in a guard rather than removed at the end: this test asserts PDFium's reading of
        // a real file, and the interesting failures are exactly the ones that panic. A trailing
        // `remove_dir_all` is skipped on panic, which had leaked thousands of directories into
        // /tmp -- and a leaked directory per failing run is a slow way to fill a CI disk.
        let workspace = TempWorkspace::new("docreader-edited");
        let path = workspace.0.join("edited-round-trip.pdf");
        crate::editor::save::write_atomically(&path, &bytes).unwrap();

        assert_eq!(
            PdfProvider::page_count(&path).unwrap(),
            2,
            "page tree survived"
        );

        let counts: Vec<usize> = PdfProvider::annotation_counts(&path, 2).unwrap();
        assert_eq!(
            counts,
            vec![1, 1],
            "each annotation must be present on its own page when PDFium reads the file"
        );

        // Rendered, not merely present: a raster that fails or comes back degenerate would
        // leave the user staring at a blank page even with a valid /Annots array.
        for page in 0..2 {
            let raster = PdfProvider::render_page_at(&path, page, 1.0).unwrap();
            assert_eq!(&raster.bytes[..8], b"\x89PNG\r\n\x1a\n");
            assert!(raster.width > 0 && raster.height > 0);
        }

        // D9: the "original" view must produce *different pixels* from the annotated one,
        // for a page that actually carries a mark. Asserting only that both render would
        // pass even if the flag were ignored, so the bytes are compared directly.
        let handles = crate::provider::DocumentHandles::new();
        let annotated = PdfProvider::render_cached(&handles, &path, None, 0, 2.0, true).unwrap();
        let original = PdfProvider::render_cached(&handles, &path, None, 0, 2.0, false).unwrap();

        assert!(
            annotated.bytes != original.bytes,
            "suppressing the annotation layer changed nothing on a page with a highlight, \
             so the render flag is not reaching PDFium"
        );

        let _ = std::fs::remove_file(&path);
    }

    /// A temporary directory removed when it goes out of scope, including on panic.
    ///
    /// The rest of the suite removes its temp trees by hand at the end of each test, which is
    /// fine for tests that cannot fail after creating the directory. This one can, so the
    /// cleanup has to be tied to scope instead of to reaching the last line.
    struct TempWorkspace(std::path::PathBuf);

    impl TempWorkspace {
        fn new(prefix: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("{prefix}-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).expect("create the test workspace");
            Self(dir)
        }
    }

    impl Drop for TempWorkspace {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// The guard is what keeps the suite from filling `/tmp`, and a guard nobody checks is how
    /// that happened in the first place: the previous cleanup ran on the last line of a test
    /// that can panic before reaching it. Deleting the `remove_dir_all` in `Drop` leaves every
    /// test green, so this assertion is the only thing standing between a fixed leak and a
    /// silently reintroduced one.
    #[test]
    fn the_temp_workspace_removes_itself_when_it_goes_out_of_scope() {
        let path = {
            let workspace = TempWorkspace::new("docreader-guard-probe");
            let path = workspace.0.clone();
            std::fs::write(path.join("probe.txt"), b"x").unwrap();
            assert!(path.is_dir(), "the workspace must exist while held");
            path
        };
        assert!(
            !path.exists(),
            "TempWorkspace left {} behind; the suite would leak a directory per run",
            path.display()
        );
    }
}
