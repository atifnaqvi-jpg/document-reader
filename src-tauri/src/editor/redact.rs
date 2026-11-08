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

use lopdf::{content::Content, Document, Object, ObjectId};

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
    /// Annotations deleted because their box intersected a mark (D-RD2).
    pub annotations_removed: usize,
    /// Image draws deleted because the image intersected a mark, whole (D-RD2/D-RD7).
    pub images_removed: usize,
    /// Pre-existing `/Subtype /Redact` annotations stripped, wherever they sat (I-RD7).
    ///
    /// These are not ours: another tool's redaction marks carry the rectangle they cover in
    /// their `/QuadPoints`, so leaving one behind tells a viewer exactly what we removed.
    pub redact_annotations_stripped: usize,
    /// Objects dropped by the reachability sweep afterwards (D-RD4).
    pub objects_dropped: usize,
}

/// What one whole-document apply removed: every page's report plus the sanitize pass.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ApplyRedaction {
    /// One entry per page that had at least one mark.
    pub pages: Vec<PageRedaction>,
    /// D-RD5's sanitize pass, run once after every page.
    pub sanitized: crate::editor::sanitize::Sanitized,
}

impl ApplyRedaction {
    /// Total text-showing operations deleted across the document.
    pub fn runs_removed(&self) -> usize {
        self.pages
            .iter()
            .map(|page| page.runs_removed + page.xobject_runs_removed)
            .sum()
    }

    /// True when this apply removed no content at all (I-RD6).
    ///
    /// Applying the same marks to an already-redacted file finds nothing to remove; the
    /// caller reports "nothing to apply" instead of writing a second file that differs only
    /// in its timestamp. Sanitize does not count here: it reports its own work, and a second
    /// pass finding no metadata is also a no-op.
    pub fn nothing_to_apply(&self) -> bool {
        self.runs_removed() == 0
            && self
                .pages
                .iter()
                .all(|page| page.annotations_removed == 0 && page.images_removed == 0)
            && self.sanitized.is_empty()
    }
}

/// Applies every page's marks and then sanitizes (D-RD5), in that order.
///
/// Sanitize runs **last** and once, not per page: it clears annotation text outside the
/// rectangles, which is only correct after every rectangle is known. Running it per page with
/// that page's marks would clear annotation text on page 1 that a mark on page 2 was meant to
/// keep.
///
/// Ordering also matters for the sweep. `redact_page` sweeps per page and sanitize sweeps at
/// the end; the second is what removes the objects sanitize orphaned.
pub fn apply_redactions(
    document: &mut lopdf::Document,
    marks: &[(u32, Vec<Rect>)],
) -> Result<ApplyRedaction, AppError> {
    let mut report = ApplyRedaction::default();

    // A pre-existing /Redact annotation is not ours to keep, wherever it sits (I-RD7). Done
    // before the per-page work so the object is gone by the time the sweep runs.
    let stripped = strip_redact_annotations(document)?;

    for (page, rects) in marks {
        if rects.is_empty() {
            continue;
        }
        let mut page_report = redact_page(document, *page, rects)?;
        page_report.redact_annotations_stripped =
            if report.pages.is_empty() { stripped } else { 0 };
        report.pages.push(page_report);
    }

    report.sanitized = crate::editor::sanitize::sanitize(document)?;

    Ok(report)
}

/// Removes every `/Subtype /Redact` annotation from every page (I-RD7).
///
/// The design's claim is that *the marks do not survive as marks*, so a viewer cannot reveal
/// what was hidden by un-marking it. Our own marks are never written as annotations, but a
/// file that arrived with someone else's carries the same disclosure.
pub fn strip_redact_annotations(document: &mut lopdf::Document) -> Result<usize, AppError> {
    let pages: Vec<ObjectId> = document.get_pages().values().copied().collect();
    let mut stripped = 0;

    for page_id in pages {
        let annots = match document.get_object(page_id).and_then(Object::as_dict) {
            Ok(page) => match page.get(b"Annots").ok() {
                Some(Object::Reference(id)) => document
                    .get_object(*id)
                    .and_then(Object::as_array)
                    .cloned()
                    .unwrap_or_default(),
                Some(Object::Array(array)) => array.clone(),
                _ => continue,
            },
            Err(_) => continue,
        };

        let mut kept = Vec::with_capacity(annots.len());
        let mut removed = 0;
        for entry in annots {
            let is_redact = entry
                .as_reference()
                .ok()
                .and_then(|id| document.get_object(id).ok())
                .and_then(|object| object.as_dict().ok())
                .map(|dict| dict.get(b"Subtype").and_then(Object::as_name).ok() == Some(b"Redact"))
                .unwrap_or(false);
            if is_redact {
                removed += 1;
            } else {
                kept.push(entry);
            }
        }

        if removed > 0 {
            stripped += removed;
            let page = document
                .get_object_mut(page_id)
                .and_then(Object::as_dict_mut)
                .map_err(|error| {
                    AppError::Other(format!("page object is not a dictionary: {error}"))
                })?;
            if kept.is_empty() {
                page.remove(b"Annots");
            } else {
                page.set("Annots", kept);
            }
        }
    }

    Ok(stripped)
}

/// One pattern hit, as the rectangle that covers the matched characters (D-RD6).
///
/// The rectangle comes from the locator's own run measurement rather than a second text
/// model: `along()`/`across()` project a page point into the run's frame, so a substring's
/// extent is its proportion of the run's advance. Re-implementing "where is this word" is the
/// duplication K14 names.
#[derive(Debug, Clone, PartialEq)]
pub struct PatternHit {
    /// The page the hit is on, 0-based.
    pub page: u32,
    /// The matched text, as decoded through the run's font.
    pub text: String,
    /// The rectangle to mark, in page coordinates.
    pub rect: Rect,
}

/// Finds every occurrence of `pattern` and returns the rectangle to mark for each (D-RD6).
///
/// Matching is case-insensitive and operates on the *decoded* run text, which is what the
/// user sees. A match that spans two runs is not found: runs are separate text-showing
/// operations and this build does not join them into words, so a pattern split across a
/// `Tj`/`TJ` seam is a miss. That is a real limitation and it is stated here rather than
/// discovered as "search-and-redact missed it" later — see the design's §8 boundary list.
pub fn find_and_mark(
    document: &lopdf::Document,
    page: u32,
    pattern: &str,
) -> Result<Vec<PatternHit>, AppError> {
    if pattern.is_empty() {
        return Ok(Vec::new());
    }
    let page_id = textlocate::page_id(document, page)?;
    let runs = textlocate::locate_runs(document, page_id)?;
    let needle = pattern.to_lowercase();

    let mut hits = Vec::new();
    for run in &runs {
        // A refused descent means text this walk could not see. Marking nothing there would
        // claim a search covered the page when it did not (D-RD3's reasoning applied to
        // search): fail rather than under-report.
        if run.xobject_refusal.is_some() {
            return Err(AppError::RedactionIncomplete(
                "search cannot cover a form this build refuses to descend into".to_string(),
            ));
        }

        let haystack = run.text.to_lowercase();
        if haystack.is_empty() || run.text.len() != haystack.len() {
            // A byte-length change means lowercasing re-encoded something; offsets into
            // `run.text` would no longer line up with character positions. Skipping is safe
            // only because the caller is told nothing was found for this run.
            continue;
        }

        let mut from = 0usize;
        while let Some(offset) = haystack[from..].find(&needle) {
            let start = from + offset;
            let end = start + needle.len();
            if end == start {
                // A zero-length match cannot advance `from`, so looping would spin forever.
                // The empty-pattern guard above already returns, but a hang must never be the
                // thing that reports a missing guard: break and stay terminating.
                break;
            }
            if let Some(rect) = run_span_rect(run, start, end) {
                hits.push(PatternHit {
                    page,
                    text: run.text[start..end].to_string(),
                    rect,
                });
            }
            from = end;
            if from >= haystack.len() {
                break;
            }
        }
    }

    Ok(hits)
}

/// The page-space rectangle covering characters `start..end` of `run` (D-RD6).
///
/// Positions are interpolated along the run's measured advance, so a hit in the middle of a
/// long `TJ` operand covers only its own characters instead of the whole run. A run whose
/// advance is unusable (zero, or non-finite) falls back to the whole run's box, which
/// over-marks — the direction D-RD3 prefers.
fn run_span_rect(run: &RawRun, start: usize, end: usize) -> Option<Rect> {
    let characters = run.text.chars().count();
    if characters == 0 {
        return None;
    }

    let origin = Point2::new(run.page_origin.0, run.page_origin.1);
    let height = run.font_size.abs().max(1.0);
    let total = run.advance.abs();

    let (along_start, along_end) = if total.is_finite() && total > 0.0 {
        let per_character = total / characters as f32;
        (per_character * start as f32, per_character * end as f32)
    } else {
        (0.0, if total.is_finite() { total } else { 0.0 })
    };

    let frame: RunFrame = run.frame;
    let corners = [
        (along_start, 0.0),
        (along_end, 0.0),
        (along_start, height),
        (along_end, height),
    ]
    .map(|(along, across)| {
        Point2::new(
            origin.x + frame.advance.x * along + frame.up.x * across,
            origin.y + frame.advance.y * along + frame.up.y * across,
        )
    });

    Some(Rect {
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
    })
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

    // Annotations and images are removed whatever the text pass finds: a mark over a picture
    // with no text under it is still a mark the user asked to apply.
    report.annotations_removed = remove_intersecting_annotations(document, page, rects)?;
    report.images_removed = remove_intersecting_images(document, page, rects)?;

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

/// Deletes every annotation whose `/Rect` intersects any mark (D-RD2).
///
/// An annotation is a first-class object with its own `/Contents`, so leaving it in place
/// leaves the text the user redacted *in the file* even when the page's content stream no
/// longer draws it. Removing the `/Annots` entry and then dropping the object is what makes
/// it unrecoverable; the reachability sweep after [`redact_page`] does the dropping.
///
/// A `/Rect` that cannot be read is treated as intersecting (D-RD3): an annotation of unknown
/// extent that might sit under the mark is removed rather than kept.
pub fn remove_intersecting_annotations(
    document: &mut lopdf::Document,
    page: u32,
    rects: &[Rect],
) -> Result<usize, AppError> {
    if rects.is_empty() {
        return Ok(0);
    }
    let page_id = textlocate::page_id(document, page)?;

    let annots = match document
        .get_object(page_id)
        .and_then(Object::as_dict)
        .and_then(|page| page.get(b"Annots").cloned())
    {
        Ok(entry) => entry,
        Err(_) => return Ok(0),
    };

    let entries: Vec<Object> = match &annots {
        Object::Reference(id) => document
            .get_object(*id)
            .and_then(Object::as_array)
            .cloned()
            .unwrap_or_default(),
        Object::Array(array) => array.clone(),
        _ => return Ok(0),
    };

    let mut kept = Vec::with_capacity(entries.len());
    let mut removed = 0usize;
    for entry in entries {
        let object = match &entry {
            Object::Reference(id) => document.get_object(*id).ok(),
            other => Some(other),
        };
        let doomed = match object.and_then(|object| object.as_dict().ok()) {
            Some(dict) => annotation_intersects(dict, document, rects),
            None => true,
        };
        if doomed {
            removed += 1;
        } else {
            kept.push(entry);
        }
    }

    if removed > 0 {
        let page = document
            .get_object_mut(page_id)
            .and_then(Object::as_dict_mut)
            .map_err(|error| {
                AppError::Other(format!("page object is not a dictionary: {error}"))
            })?;
        if kept.is_empty() {
            page.remove(b"Annots");
        } else {
            page.set("Annots", kept);
        }
    }

    Ok(removed)
}

/// Whether one annotation's box intersects any mark; unreadable means yes (D-RD3).
fn annotation_intersects(dict: &lopdf::Dictionary, document: &Document, rects: &[Rect]) -> bool {
    match annotation_rect(dict, document) {
        Some(rect) => rects.iter().any(|mark| rect.intersects(mark)),
        None => true,
    }
}

fn annotation_rect(dict: &lopdf::Dictionary, document: &Document) -> Option<Rect> {
    let array = dict.get(b"Rect").ok()?;
    let array = document.dereference(array).ok()?.1.as_array().ok()?;
    if array.len() != 4 {
        return None;
    }
    let mut corners = [0.0f32; 4];
    for (index, value) in array.iter().enumerate() {
        corners[index] = match value {
            Object::Integer(value) => *value as f32,
            Object::Real(value) => *value,
            _ => return None,
        };
    }
    Some(Rect::new(corners[0], corners[1], corners[2], corners[3]))
}

/// Deletes every image XObject the page draws inside a mark (D-RD2, D-RD7).
///
/// Whole-object removal, never partial masking: a `Do` that would draw an intersecting image
/// is deleted, which is what stops the pixels being drawn at all. The image object itself is
/// dropped by the reachability sweep when nothing else references it; when another page shares
/// it, the shared object survives for that page, which is the page-scoped removal D-RD4's
/// reachability rule implies.
///
/// The image's page-space box comes from the `cm` in force at its `Do`, so a scaled or placed
/// image is judged where it actually appears rather than at its natural size.
pub fn remove_intersecting_images(
    document: &mut lopdf::Document,
    page: u32,
    rects: &[Rect],
) -> Result<usize, AppError> {
    if rects.is_empty() {
        return Ok(0);
    }
    let page_id = textlocate::page_id(document, page)?;
    let mut content = textlocate::decoded_content(document, page_id)?;

    // Replay `cm`/`q`/`Q` so each `Do` is judged through the transform in force.
    let mut saved: Vec<[f32; 6]> = Vec::new();
    let mut matrix = [1.0f32, 0.0, 0.0, 1.0, 0.0, 0.0];
    let mut doomed = Vec::new();

    for (index, operation) in content.operations.iter().enumerate() {
        match operation.operator.as_str() {
            "q" => saved.push(matrix),
            "Q" => {
                if let Some(previous) = saved.pop() {
                    matrix = previous;
                }
            }
            "cm" => {
                if let Some(next) = matrix_from_operands(&operation.operands) {
                    matrix = multiply(matrix, next);
                }
            }
            "Do" => {
                let Some(Object::Name(name)) = operation.operands.first() else {
                    continue;
                };
                let Some(stream) = xobject_entry(document, page_id, name) else {
                    continue;
                };
                // Only images are judged here; forms hold content, which the text pass owns.
                if stream.dict.get(b"Subtype").and_then(Object::as_name).ok() != Some(b"Image") {
                    continue;
                }
                if image_intersects(matrix, rects) {
                    doomed.push(index);
                }
            }
            _ => {}
        }
    }

    for index in doomed.iter().rev() {
        content.operations.remove(*index);
    }
    if !doomed.is_empty() {
        write_content_at_path(document, page_id, &[], &content)?;
        prune_unused_xobjects(document, page_id, &content)?;
    }

    Ok(doomed.len())
}

/// Drops `/XObject` entries the rewritten content no longer draws (D-RD2, I-RD3).
///
/// Deleting the `Do` stops the image being *drawn*, which is not the same as removing it:
/// the resource dictionary still references the object, so the reachability sweep keeps it
/// alive and the pixels stay in the file for anyone with a recovery tool. That is the
/// "leftover XObject" leak I-RD3 exists to catch, and it was caught here by
/// `an_image_drawn_under_the_mark_is_removed_whole` failing after the `Do` was already gone.
///
/// An entry is removed only when no `Do` in the rewritten content names it, so an image still
/// drawn by another `Do` on the same page is untouched.
fn prune_unused_xobjects(
    document: &mut Document,
    page_id: ObjectId,
    content: &Content<Vec<lopdf::content::Operation>>,
) -> Result<(), AppError> {
    let drawn: BTreeSet<Vec<u8>> = content
        .operations
        .iter()
        .filter(|operation| operation.operator == "Do")
        .filter_map(|operation| match operation.operands.first() {
            Some(Object::Name(name)) => Some(name.clone()),
            _ => None,
        })
        .collect();

    let page = document
        .get_object(page_id)
        .and_then(Object::as_dict)
        .map_err(|error| AppError::Other(format!("page object is not a dictionary: {error}")))?;
    // `/Resources` may be inline or indirect; only the indirect form needs a mutable lookup.
    let resources_id = match page.get(b"Resources").ok() {
        Some(Object::Reference(id)) => Some(*id),
        _ => None,
    };

    let unused: Vec<Vec<u8>> = {
        let resources = match resources_id {
            Some(id) => document
                .get_object(id)
                .and_then(Object::as_dict)
                .map_err(|error| {
                    AppError::Other(format!("resources are not a dictionary: {error}"))
                })?,
            // Inline `/Resources`: the dictionary lives one level down inside the page, so
            // reading `/XObject` off the page itself finds nothing and returns early.
            None => page
                .get(b"Resources")
                .and_then(Object::as_dict)
                .map_err(|error| {
                    AppError::Other(format!("page has no readable /Resources: {error}"))
                })?,
        };
        let Some(xobjects) = resources
            .get(b"XObject")
            .ok()
            .and_then(|object| document.dereference(object).ok())
            .and_then(|(_, object)| object.as_dict().ok())
        else {
            return Ok(());
        };
        xobjects
            .iter()
            .map(|(name, _)| name.clone())
            .filter(|name| !drawn.contains(name))
            .collect()
    };

    if unused.is_empty() {
        return Ok(());
    }

    // The inline case must step *through* `/Resources`: `get_object_mut(page)` yields the page
    // dictionary, whose `/XObject` key does not exist — the entry lives one level down.
    let resources = match resources_id {
        Some(id) => document.get_object_mut(id),
        None => document.get_object_mut(page_id).and_then(|page| {
            page.as_dict_mut()
                .and_then(|page| page.get_mut(b"Resources"))
        }),
    }
    .and_then(Object::as_dict_mut)
    .map_err(|error| AppError::Other(format!("resources are not a dictionary: {error}")))?;
    let xobjects = resources
        .get_mut(b"XObject")
        .map_err(|error| AppError::Other(format!("resources have no /XObject: {error}")))?
        .as_dict_mut()
        .map_err(|error| AppError::Other(format!("/XObject is not a dictionary: {error}")))?;
    for name in unused {
        xobjects.remove(&name);
    }

    Ok(())
}

/// Whether an image drawn through `matrix` can touch any mark (D-RD3: unreadable is yes).
fn image_intersects(matrix: [f32; 6], rects: &[Rect]) -> bool {
    // An image occupies the unit square, transformed by the `cm` in force.
    let corners = [(0.0f32, 0.0f32), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0)].map(|(x, y)| {
        Point2::new(
            matrix[0] * x + matrix[2] * y + matrix[4],
            matrix[1] * x + matrix[3] * y + matrix[5],
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
    rects.iter().any(|mark| boxed.intersects(mark))
}

fn matrix_from_operands(operands: &[Object]) -> Option<[f32; 6]> {
    if operands.len() != 6 {
        return None;
    }
    let mut out = [0.0f32; 6];
    for (index, operand) in operands.iter().enumerate() {
        out[index] = match operand {
            Object::Integer(value) => *value as f32,
            Object::Real(value) => *value,
            _ => return None,
        };
    }
    Some(out)
}

fn multiply(left: [f32; 6], right: [f32; 6]) -> [f32; 6] {
    [
        left[0] * right[0] + left[1] * right[2],
        left[0] * right[1] + left[1] * right[3],
        left[2] * right[0] + left[3] * right[2],
        left[2] * right[1] + left[3] * right[3],
        left[4] * right[0] + left[5] * right[2] + right[4],
        left[4] * right[1] + left[5] * right[3] + right[5],
    ]
}

fn xobject_entry<'a>(
    document: &'a Document,
    page_id: ObjectId,
    name: &[u8],
) -> Option<&'a lopdf::Stream> {
    let page = document.get_object(page_id).ok()?.as_dict().ok()?;
    let resources = page
        .get(b"Resources")
        .ok()
        .and_then(|object| document.dereference(object).ok())
        .and_then(|(_, object)| object.as_dict().ok())?;
    let xobjects = resources
        .get(b"XObject")
        .ok()
        .and_then(|object| document.dereference(object).ok())
        .and_then(|(_, object)| object.as_dict().ok())?;
    xobjects
        .get(name)
        .ok()
        .and_then(|object| document.dereference(object).ok())
        .and_then(|(_, object)| object.as_stream().ok())
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
