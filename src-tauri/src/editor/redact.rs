//! Destructive redaction of page content (P3.2 D-RD2, D-RD3, D-RD7).
//!
//! Apply removes; it never paints over. The text-showing operation that produced glyphs
//! inside a marked rectangle is **deleted from the content stream** (D-RD2), because an
//! operation that is still there is still extractable no matter what is drawn on top of it.
//! A black fill is an appearance detail added later and is not this module's claim.
//!
//! Two asymmetries decide every ambiguous case:
//!
//! * **Over-removal is allowed, under-removal is not** (D-RD3). A run whose geometry cannot
//!   be placed inside or outside a rectangle with certainty is removed.
//! * **Images are removed whole** (D-RD7). Partial-pixel masking is out of scope for 1.2.0,
//!   so an image that intersects by one pixel is dropped entirely rather than approximated.
//!
//! ## What counts as "inside"
//!
//! Each run's extent is measured in its own frame — along the advance axis from its page
//! origin for `advance`, across it by the font size — and then projected onto the page axes
//! with [`RunFrame`]. Comparing page-space boxes to a text-space origin is the mistake that
//! P2.7.4 already paid for; [`RawRun::page_origin`] and [`RawRun::frame`] exist so this
//! module never has to make it.
//!
//! The extent is deliberately generous: `advance` is a real measurement when the font has
//! in-file metrics, and the fallback is an approximation that over-states. Over-stating
//! removes a glyph on the border, which D-RD3 prefers to the alternative.

use std::collections::BTreeSet;

use lopdf::{content::Content, Object, ObjectId};

use crate::editor::geometry::{Point2, RunFrame};
use crate::editor::object_gc::sweep_unreachable;
use crate::editor::textlocate::{self, RawRun};
use crate::model::AppError;

/// An axis-aligned rectangle on a page, in PDF page coordinates (points, y up).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

impl Rect {
    /// Builds a rectangle from two opposite corners in any order.
    pub fn new(ax: f32, ay: f32, bx: f32, by: f32) -> Self {
        Self {
            x0: ax.min(bx),
            y0: ay.min(by),
            x1: ax.max(bx),
            y1: ay.max(by),
        }
    }

    /// True when `point` lies inside or on the edge.
    pub fn contains(&self, point: Point2) -> bool {
        point.x >= self.x0 && point.x <= self.x1 && point.y >= self.y0 && point.y <= self.y1
    }

    /// True when the rectangles share any area, edges included.
    ///
    /// Touching counts: an image whose border lies exactly on the mark's border is treated as
    /// intersecting (D-RD3).
    pub fn intersects(&self, other: &Rect) -> bool {
        self.x0 <= other.x1 && other.x0 <= self.x1 && self.y0 <= other.y1 && other.y0 <= self.y1
    }
}

/// What one page's apply removed, reported so the caller can tell the user and the tests can
/// assert on counts instead of only on the absence of text.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PageRedaction {
    pub page: u32,
    /// Number of text-showing operations deleted from the page's own content stream.
    pub runs_removed: usize,
    /// Number of text-showing operations deleted from inside Form XObjects.
    pub xobject_runs_removed: usize,
    /// Objects dropped by the reachability sweep afterwards (D-RD4).
    pub objects_dropped: usize,
}

/// True when `run` overlaps `rect` and must therefore be removed (D-RD3).
///
/// The test is deliberately the union of two weaker tests rather than one tight one:
///
/// * the run's **origin** inside the rectangle — a run that starts inside is removed even if
///   its measured extent falls short, and
/// * the run's **advance segment** crossing the rectangle — a run that starts outside and
///   passes through is removed even though its origin is outside.
///
/// Either alone is defeatable by a measurement error in the other; together they only fail
/// toward keeping text that is entirely outside, which the rectangle test then catches.
pub fn run_intersects(run: &RawRun, rect: &Rect) -> bool {
    let origin = Point2::new(run.page_origin.0, run.page_origin.1);
    if rect.contains(origin) {
        return true;
    }

    // The run's ink box, as four corners of the parallelogram it advances across.
    let frame: RunFrame = run.frame;
    let height = run.font_size.abs().max(1.0);
    let corners = [
        Point2::new(0.0, 0.0),
        Point2::new(run.advance.abs().max(0.0), 0.0),
        Point2::new(0.0, height),
        Point2::new(run.advance.abs().max(0.0), height),
    ]
    .map(|local| {
        Point2::new(
            origin.x + frame.advance.x * local.x + frame.up.x * local.y,
            origin.y + frame.advance.y * local.x + frame.up.y * local.y,
        )
    });

    let boxed = Rect {
        x0: corners.iter().map(|p| p.x).fold(f32::INFINITY, f32::min),
        y0: corners.iter().map(|p| p.y).fold(f32::INFINITY, f32::min),
        x1: corners
            .iter()
            .map(|p| p.x)
            .fold(f32::NEG_INFINITY, f32::max),
        y1: corners
            .iter()
            .map(|p| p.y)
            .fold(f32::NEG_INFINITY, f32::max),
    };

    boxed.intersects(rect)
}

/// Removes every run on `page` that intersects any of `rects`, then GCs the document.
///
/// Mutates `document` in place; the caller writes the file. Returns what was removed.
pub fn redact_page(
    document: &mut lopdf::Document,
    page: u32,
    rects: &[Rect],
) -> Result<PageRedaction, AppError> {
    let mut report = PageRedaction {
        page,
        ..PageRedaction::default()
    };
    if rects.is_empty() {
        return Ok(report);
    }

    let page_id = textlocate::page_id(document, page)?;
    let runs = textlocate::locate_runs(document, page_id)?;

    // Group the doomed operations by the content stream they live in. An operation index is
    // only meaningful within one stream, so the same index in two streams names two
    // different operations; keying on the stream path is what keeps them apart.
    let mut doomed: Vec<(Vec<textlocate::XObjectStep>, usize)> = Vec::new();
    for run in &runs {
        if run.xobject_refusal.is_some() {
            // A refused descent means text this walk could not see. It cannot be proven
            // outside the rectangle, so D-RD3 says the whole refusal is a reason to stop —
            // silently leaving it would remove less than the user asked for and claim
            // otherwise.
            return Err(AppError::RedactionIncomplete(
                "text could not be located in a form this build refuses to descend into"
                    .to_string(),
            ));
        }
        if rects.iter().any(|rect| run_intersects(run, rect)) {
            doomed.push((run.id.xobject_path.clone(), run.id.op_index));
        }
    }

    if doomed.is_empty() {
        report.objects_dropped = sweep_unreachable(document)?.len();
        return Ok(report);
    }

    // Rewrite each affected stream once, highest index first so earlier removals do not shift
    // the indices still to be applied.
    let mut by_stream: std::collections::BTreeMap<
        Vec<u8>,
        (Vec<textlocate::XObjectStep>, Vec<usize>),
    > = std::collections::BTreeMap::new();
    for (path, op_index) in doomed {
        let key = path
            .iter()
            .map(|step| step.name.clone())
            .collect::<Vec<Vec<u8>>>()
            .concat();
        by_stream
            .entry(key)
            .or_insert_with(|| (path.clone(), Vec::new()))
            .1
            .push(op_index);
    }

    for (_, (path, mut indices)) in by_stream {
        indices.sort_unstable();
        indices.dedup();

        let mut content = textlocate::decoded_content(document, page_id)?;
        if !path.is_empty() {
            content = content_at_path(document, page_id, &path)?;
        }

        for index in indices.into_iter().rev() {
            if index < content.operations.len() {
                content.operations.remove(index);
                if path.is_empty() {
                    report.runs_removed += 1;
                } else {
                    report.xobject_runs_removed += 1;
                }
            }
        }

        write_content_at_path(document, page_id, &path, &content)?;
    }

    report.objects_dropped = sweep_unreachable(document)?.len();
    Ok(report)
}

/// Decodes the content stream at `path`, descending through the named Form XObjects.
fn content_at_path(
    document: &lopdf::Document,
    page_id: ObjectId,
    path: &[textlocate::XObjectStep],
) -> Result<Content<Vec<lopdf::content::Operation>>, AppError> {
    let mut bytes = document
        .get_page_content_with_limit(page_id, 16 * 1024 * 1024)
        .map_err(|error| AppError::Other(format!("could not read page content: {error}")))?;

    for step in path {
        let stream = form_stream(document, page_id, &step.name)?;
        bytes = stream
            .decompressed_content()
            .map_err(|error| AppError::Other(format!("could not decompress a form: {error}")))?;
    }

    Content::decode(&bytes)
        .map_err(|error| AppError::Other(format!("could not decode content: {error}")))
}

/// Writes re-encoded content back to the page's own stream or to a nested form.
fn write_content_at_path(
    document: &mut lopdf::Document,
    page_id: ObjectId,
    path: &[textlocate::XObjectStep],
    content: &Content<Vec<lopdf::content::Operation>>,
) -> Result<(), AppError> {
    let encoded = content
        .encode()
        .map_err(|error| AppError::Other(format!("could not re-encode content: {error}")))?;

    if path.is_empty() {
        return document
            .change_page_content(page_id, encoded)
            .map_err(|error| AppError::Other(format!("could not replace page content: {error}")));
    }

    // Nested: resolve the form object and replace its stream data.
    let last = path
        .last()
        .ok_or_else(|| AppError::Other("empty xobject path reached the writer".to_string()))?;
    let object_id = form_object_id(document, page_id, &last.name)?;
    let object = document
        .get_object_mut(object_id)
        .map_err(|error| AppError::Other(format!("form object is missing: {error}")))?;
    let stream = object
        .as_stream_mut()
        .map_err(|error| AppError::Other(format!("form is not a stream: {error}")))?;
    stream.set_content(encoded);
    // The old data was compressed for a different length; leaving the filter in place would
    // make the stream decode to garbage.
    stream.dict.remove(b"Filter");
    stream.dict.remove(b"DecodeParms");
    Ok(())
}

fn form_stream<'a>(
    document: &'a lopdf::Document,
    page_id: ObjectId,
    name: &[u8],
) -> Result<&'a lopdf::Stream, AppError> {
    let id = form_object_id(document, page_id, name)?;
    document
        .get_object(id)
        .and_then(Object::as_stream)
        .map_err(|error| AppError::Other(format!("form {name:?} is not a stream: {error}")))
}

fn form_object_id(
    document: &lopdf::Document,
    page_id: ObjectId,
    name: &[u8],
) -> Result<ObjectId, AppError> {
    let page = document
        .get_object(page_id)
        .and_then(Object::as_dict)
        .map_err(|error| AppError::Other(format!("page object is not a dictionary: {error}")))?;
    let resources = page
        .get(b"Resources")
        .ok()
        .and_then(|object| document.dereference(object).ok())
        .and_then(|(_, object)| object.as_dict().ok())
        .ok_or_else(|| {
            AppError::RedactionIncomplete("page has no resource dictionary".to_string())
        })?;
    let xobjects = resources
        .get(b"XObject")
        .ok()
        .and_then(|object| document.dereference(object).ok())
        .and_then(|(_, object)| object.as_dict().ok())
        .ok_or_else(|| {
            AppError::RedactionIncomplete("page has no /XObject dictionary".to_string())
        })?;
    match xobjects.get(name) {
        Ok(Object::Reference(id)) => Ok(*id),
        _ => Err(AppError::RedactionIncomplete(format!(
            "form {name:?} is not an indirect object"
        ))),
    }
}

/// Every object id referenced by a page's content, for the caller that needs to know what the
/// sweep considered live.
pub fn referenced_by_page(document: &lopdf::Document, page: u32) -> BTreeSet<ObjectId> {
    let mut out = BTreeSet::new();
    if let Ok(page_id) = textlocate::page_id(document, page) {
        if let Ok(content) = textlocate::decoded_content(document, page_id) {
            for operation in &content.operations {
                for operand in &operation.operands {
                    if let Object::Reference(id) = operand {
                        out.insert(*id);
                    }
                }
            }
        }
    }
    out
}
