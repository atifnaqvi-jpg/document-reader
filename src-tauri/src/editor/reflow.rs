//! P2.8 reflow, restricted to the one case the §5 guard can judge.
//!
//! The design (`docs/P2.8-reflow-DESIGN.md` §2) is written for the general case. This module
//! implements **exactly one** of its cases and refuses everything else by naming the ruling
//! that refuses it:
//!
//! * one line, one baseline, one level, one merge scope;
//! * a wider replacement of a run inside a multi-fragment `TJ` array;
//! * the array's trailing fragments moved by an **inserted `Td`** computed as *line origin +
//!   measured prefix* (R2.1), never as a pen delta;
//! * no spacing adjustment (R1), no wrapping (R4), no cross-level line (R5), no non-uniform
//!   transform (R6), no run in another tagged unit left of the line start (R8b).
//!
//! Refusing here is not a stub: each refusal names the rule, because a guard must be able to
//! tell a refusal from a wrong answer, and silent skips are banned in this repository.

use lopdf::{content::Content, content::Operation, Document, Object, ObjectId};

use crate::editor::geometry::PdfRect;
use crate::editor::substitute::FontMetrics;
use crate::editor::textlocate::{decoded_content, RawRun, RunId, XObjectStep};
use crate::model::AppError;

/// Which ruling refused a reflow request.
///
/// Deliberately not [`crate::editor::textedit::Gate`]: `Gate` names *admissibility* — whether a
/// run may be edited at all — and reflow must not be able to widen it (R7). This names
/// *feasibility of the move*, a different question about a run that already passed every gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReflowRefusal {
    /// R1: an equal-or-narrower replacement changes no operator; there is nothing to move.
    R1NoMove,
    /// R4: the replacement does not fit the line, and wrapping is out of scope.
    R4Wrap,
    /// R5: the line straddles content levels — page text and form text on one baseline.
    R5StraddlesLevels,
    /// R6: the drawing matrix is non-uniform, so "the rest of the line" is not horizontal.
    R6NonUniformTransform,
    /// R8b: another tagged unit has a run on this baseline, left of this line's own start, so the
    /// line origin the `Td` is measured from is not this line's.
    R8bScopeLeftOfLine,
    /// R10b: the text before this run is positioned by an operator outside the run's `TJ` array,
    /// so the array's own walk cannot say where the rest of the line starts.
    R10bUnattributedPrefix,
    /// The run is not a fragment of a multi-fragment `TJ` array with something after it. Not a
    /// numbered ruling: it is the shape with no trailing run to move.
    NotOneArray,
}

impl ReflowRefusal {
    /// Stable identifier, so a test asserts the *rule* rather than matching prose.
    pub fn name(self) -> &'static str {
        match self {
            Self::R1NoMove => "R1-no-move",
            Self::R4Wrap => "R4-wrap",
            Self::R5StraddlesLevels => "R5-straddles-levels",
            Self::R6NonUniformTransform => "R6-non-uniform-transform",
            Self::R8bScopeLeftOfLine => "R8b-scope-left-of-line",
            Self::R10bUnattributedPrefix => "R10b-unattributed-kern",
            Self::NotOneArray => "not-one-array",
        }
    }

    /// The rule's own sentence, for the UI and for a log.
    pub fn message(self) -> &'static str {
        match self {
            Self::R1NoMove => {
                "this replacement is not wider than the text it replaces, so nothing after it \
                 needs to move"
            }
            Self::R4Wrap => {
                "this replacement does not fit on its line, and reflow does not wrap text onto \
                 the next line"
            }
            Self::R5StraddlesLevels => {
                "this line mixes page text with text inside a form, so its runs cannot be moved \
                 as one line"
            }
            Self::R6NonUniformTransform => {
                "this text is drawn through a non-uniform transform, so the rest of its line is \
                 not horizontal"
            }
            Self::R8bScopeLeftOfLine => {
                "text from another tagged or structure unit lies to the left of this line, so \
                 the rest of the line cannot be moved as one"
            }
            Self::R10bUnattributedPrefix => {
                "another run on this line is positioned outside this TJ array, so reflow cannot \
                 tell where the rest of the line starts"
            }
            Self::NotOneArray => {
                "this text has no following run in the same TJ array, which is the only shape \
                 reflow can currently move"
            }
        }
    }
}

/// A refusal, naming both the rule and the run it applies to.
#[derive(Debug, Clone, PartialEq)]
pub struct ReflowRefused {
    pub rule: ReflowRefusal,
    pub run: RunId,
    /// The two widths a refusal compared, as `(needed, available)`, when it compared two.
    ///
    /// R3 keeps the `fits-width` sentence's shape — both measured widths — and the numbers are
    /// measured once, where the decision is made. Re-measuring them at the seam would let the
    /// refusal say one thing while the rule decided another.
    pub measured: Option<(f32, f32)>,
}

impl ReflowRefused {
    fn new(rule: ReflowRefusal, run: &RunId) -> Self {
        Self {
            rule,
            run: run.clone(),
            measured: None,
        }
    }

    /// The same refusal, carrying the widths it compared: `needed` first, `available` second.
    fn measured(rule: ReflowRefusal, run: &RunId, needed: f32, available: f32) -> Self {
        Self {
            rule,
            run: run.clone(),
            measured: Some((needed, available)),
        }
    }
}

impl std::fmt::Display for ReflowRefused {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.rule.name(), self.rule.message())
    }
}

/// The record of one reflow write.
///
/// **§2.2 predicted a new struct was required; for case (a) it was not.** See §2.5 of the
/// design, where that prediction is corrected against this measurement.
///
/// The prediction rested on §2.3: "reflow creates operators that do not exist yet", so a record
/// whose only verb is "change the operand at `operand_path`" has nothing to point at. Case (a)
/// does insert an operator — the `Td` — but the *operator vector* keeps its length: the single
/// `TJ` operation at `op_index` becomes three operations (two `TJ`, one `Td`), which is a
/// **replacement of the vector at `op_index`**, exactly what `ReflowOperation` describes. So the
/// struct is required, and §2.2 is right; §2.3's reason for needing it is what changes.
///
/// Undo is deliberately **not** wired: case (a) needs the record to exist and be correct, not to
/// be replayable through the transaction log yet.
// Not `PartialEq`: `lopdf::content::Operation` does not implement it in 0.45, so equality is
// defined by [`ReflowOperation::same_write_as`] over the encoded form, which is also what a test
// can meaningfully compare.
#[derive(Debug, Clone)]
pub struct ReflowOperation {
    pub page: u32,
    /// The index of the `TJ` operation this write replaced.
    pub op_index: usize,
    pub xobject_path: Vec<XObjectStep>,
    /// The operations at `op_index` before the write.
    pub old_ops: Vec<Operation>,
    /// The operations at `op_index` after the write.
    pub new_ops: Vec<Operation>,
}

impl ReflowOperation {
    /// True when two records describe the same write: same address, same encoded effect.
    pub fn same_write_as(&self, other: &Self) -> bool {
        self.page == other.page
            && self.op_index == other.op_index
            && self.xobject_path == other.xobject_path
            && encode(&self.old_ops) == encode(&other.old_ops)
            && encode(&self.new_ops) == encode(&other.new_ops)
    }
}

/// The operations' encoded form, for comparing and for recording.
///
/// Uses `Content::encode`, the same encoder that writes the page: recording with a second
/// encoder could describe a stream that never gets written.
pub(crate) fn encode(operations: &[Operation]) -> Vec<u8> {
    Content {
        operations: operations.to_vec(),
    }
    .encode()
    .unwrap_or_default()
}

/// What a reflow did.
#[derive(Debug, Clone)]
pub struct ReflowApplied {
    pub operation: ReflowOperation,
    /// Where the trailing run now starts, in text space.
    pub moved_to: f32,
    /// How far it moved.
    pub shift: f32,
}

/// The `Td` offset a reflow must write for a trailing run at `pen_x` on a line that began at
/// `line_origin`, once the run before it has grown to `prefix`.
///
/// This is the whole of R2.1 in one function, and it is public so the rule can be tested without
/// building a document:
///
/// * `Td` sets `tm` and `tlm` to `translate(tx,ty) x tlm` (ISO 32000-1 9.4.2), i.e. it is relative
///   to the **line** origin and discards the pen's accumulated advance;
/// * so the offset is `(line_origin + prefix) - line_origin`, and the `pen_x` argument exists
///   only to make the wrong formulation expressible — see the tests, where substituting it
///   reproduces the measured 1.984 pt.
pub fn td_offset(line_origin: f32, prefix: f32) -> f32 {
    (line_origin + prefix) - line_origin
}

/// Widens one run of a multi-fragment `TJ` array and moves the runs after it with an inserted
/// `Td`.
///
/// Returns the edited level and the record of the write. Kept separate from `write_level` so the
/// arithmetic can be exercised without mutating a document.
pub fn reflow_tj_array(
    document: &Document,
    page_index: u32,
    page_id: ObjectId,
    runs: &[RawRun],
    target: &RunId,
    replacement: &str,
    // The page-space x the line may not grow past: the content box's right edge. Case (a) has no
    // content box of its own, and inventing one would make R4 untestable by refusal.
    limit_x: f32,
) -> Result<(Content<Vec<Operation>>, ReflowApplied), ReflowRefused> {
    let run = runs
        .iter()
        .find(|candidate| candidate.id == *target)
        .ok_or_else(|| ReflowRefused::new(ReflowRefusal::NotOneArray, target))?;

    let level = target.xobject_path.clone();
    let on_baseline: Vec<&RawRun> = runs
        .iter()
        .filter(|candidate| candidate.baseline() == run.baseline())
        .collect();

    // R5 first: a straddling line cannot be written atomically whatever else is true of it, so it
    // is the least useful refusal to defer.
    if on_baseline
        .iter()
        .any(|candidate| candidate.id.xobject_path != level)
    {
        return Err(ReflowRefused::new(ReflowRefusal::R5StraddlesLevels, target));
    }

    // R6: a sheared or anisotropically scaled matrix makes "the rest of the line" a
    // non-horizontal concept. `page_scale` cannot answer this — it reports the advance axis only,
    // and a shear keeps a usable advance length — so the test is orthogonality itself.
    if !run.frame.is_orthogonal() {
        return Err(ReflowRefused::new(
            ReflowRefusal::R6NonUniformTransform,
            target,
        ));
    }

    // R8b: a same-baseline run in **another tagged unit** is not on this line. `baseline()` is
    // `origin.1 + rise` and carries no tag term, so `merge_scope()` is the only thing that can
    // tell a table cell from the caption beside it. Both quantities the move depends on come from
    // the runs that would merge with the target: the line's extent (`line_right`, R4) and its
    // origin (`line_origin`, R2.1). A run outside the scope lying left of the line start makes
    // `line_origin` too small, and because `Td` is relative to the line matrix the tail is then
    // drawn right of where it belongs — displaced by `true tlm - line_origin`, which no value in
    // the returned record can contradict (design R8b.1). So that shape is refused.
    //
    // Checked **before** R10b, and the ordering is load-bearing rather than stylistic. R10b
    // refuses a same-baseline run at a different `op_index`, and no constructible two-unit line
    // can avoid one: a `BDC`/`EMC` pair is an operator pair, and a `TJ` array's elements are
    // strings and numbers only, so two fragments of one array cannot differ in scope (asserted by
    // `one_tj_array_cannot_carry_two_tagged_units`). Left after R10b, R8b would be unreachable.
    // Both rules refuse, so the precedence changes which remedy is named and nothing else — and
    // R10b's message would name the wrong one here: "positioned outside this TJ array" when the
    // user's route out is to edit within one tagged unit.
    let in_scope: Vec<&RawRun> = on_baseline
        .iter()
        .copied()
        .filter(|candidate| candidate.merge_scope() == run.merge_scope())
        .collect();
    // `run` is in `on_baseline` and so in `in_scope`, which makes the fold's identity unreachable.
    let line_origin = in_scope
        .iter()
        .map(|candidate| candidate.origin.0)
        .fold(f32::INFINITY, f32::min);
    if on_baseline.iter().any(|candidate| {
        candidate.merge_scope() != run.merge_scope() && candidate.origin.0 < line_origin
    }) {
        return Err(ReflowRefused::new(
            ReflowRefusal::R8bScopeLeftOfLine,
            target,
        ));
    }

    // R10b: the measured prefix belongs to **one** `TJ` array. `tj_fragment_origins` advances the
    // pen only between the elements of an array, and `TextMatrixWalk::step` is a deliberate no-op
    // for a show operator, so any text on this baseline that came from a different operation was
    // positioned by something the array walk never saw — a preceding `Tj`, or a `Td`/`TD` that
    // began this array. Including that text's displacement in the prefix would make `moved_to`
    // an intention rather than the place the ink lands (design R10, R8b.1's failure class).
    //
    // Refused rather than corrected for the same reason R8b refuses: `moved_to` reduces to
    // `run.origin.0 + new_width` whatever `line_origin` is, so the record cannot show the
    // mistake. A `Td` *inside* an array is not a separate case — an array's elements are strings
    // and numbers only, so it is not expressible.
    //
    // Scoped to `in_scope`, not to `on_baseline`, and the scope is what R8b needs: a run in
    // another tagged unit that lies to the **right** of this line is not part of this line's
    // prefix, and refusing it here would make R8b's `line_right` exclusion dead code. Only the
    // runs that could enter this line's arithmetic can make it unattributable.
    if in_scope
        .iter()
        .any(|candidate| candidate.id.op_index != target.op_index)
    {
        return Err(ReflowRefused::new(
            ReflowRefusal::R10bUnattributedPrefix,
            target,
        ));
    }

    // Case (a): the run is one fragment of a `TJ` array, with at least one fragment after it.
    if run.operator != "TJ" || target.operand_path.len() < 2 {
        return Err(ReflowRefused::new(ReflowRefusal::NotOneArray, target));
    }

    let mut content: Content<Vec<Operation>> = decoded_content(document, page_id)
        .map_err(|_| ReflowRefused::new(ReflowRefusal::NotOneArray, target))?;

    let operand_index = target.operand_path[0];
    let fragment_index = target.operand_path[1];
    let items = match content
        .operations
        .get(target.op_index)
        .and_then(|operation| operation.operands.get(operand_index))
    {
        Some(Object::Array(items)) => items.clone(),
        _ => return Err(ReflowRefused::new(ReflowRefusal::NotOneArray, target)),
    };
    if fragment_index + 1 >= items.len() {
        return Err(ReflowRefused::new(ReflowRefusal::NotOneArray, target));
    }

    // Measure through the writer's own metrics: the width gate and `spaced_advance` both do, and
    // an approximation here would disagree with the write that follows it.
    let metrics = FontMetrics::resolve(document, page_id, &run.font_key);
    let measure = |text: &str| -> Option<f32> { metrics.advance(text, run.font_size) };

    // T-8b: the font metrics are **unscaled** text space — `advance` is the font's own `w0` sum at
    // `Tf`, with no `Tz`. The line's origins and advances are *scaled*, because the walk applied
    // `Tz/100` to every glyph advance and every `TJ` kern (ISO 32000-1 9.4.3, 9.4.4). A replacement
    // measured unscaled and then placed in scaled space is `1 - Tz/100` too wide, and at `Tz 50`
    // on `kerned-line-tz.pdf` that reported a tail at 136.704 that the ink draws at 104.352.
    let scale = run.horizontal_scale;
    let measure_scaled = |text: &str| -> Option<f32> {
        let width = measure(text)?;
        // `Tz 0` collapses the advance axis: the run cannot be laid out at all, and the gate that
        // says so (`DegenerateText`) is `textedit`'s, not reflow's. Reporting a zero width here
        // would satisfy `new_width <= old_width` and refuse as R1, naming the wrong remedy.
        if scale <= 0.0 {
            return None;
        }
        Some(width * scale)
    };

    let old_width = measure_scaled(&run.text)
        .ok_or_else(|| ReflowRefused::new(ReflowRefusal::NotOneArray, target))?;
    let new_width = measure_scaled(replacement)
        .ok_or_else(|| ReflowRefused::new(ReflowRefusal::NotOneArray, target))?;

    // R1: an equal-or-narrower replacement moves nothing, and moving nothing is not reflow.
    if new_width <= old_width {
        return Err(ReflowRefused::new(ReflowRefusal::R1NoMove, target));
    }

    // R4: the widened line must still fit. If its right edge would pass `limit_x` there is
    // nowhere on this baseline to go, so refuse rather than wrap (R4 is out of scope). The edge is
    // this line's own: a run in another tagged unit is not room this line can use (R8b).
    let growth = new_width - old_width;
    let line_right = in_scope
        .iter()
        .map(|candidate| candidate.page_origin.0 + candidate.advance)
        .fold(f32::NEG_INFINITY, f32::max);
    if line_right + growth > limit_x + f32::EPSILON {
        // The two widths the decision compared travel with the refusal: R3 keeps the gate's
        // sentence, and re-measuring them at the seam would let the message describe a different
        // comparison from the one that was made.
        return Err(ReflowRefused::measured(
            ReflowRefusal::R4Wrap,
            target,
            growth,
            limit_x - line_right,
        ));
    }

    // R2.1: the trailing run's new origin is **line origin + measured prefix**. The prefix is
    // everything on the line before the trailing run, re-measured — here, the widened target's
    // own advance, because the target is what grew and it sits immediately before the tail.
    //
    // `line_origin` is the leftmost run **in the target's merge scope** (R8b): a run on the same
    // baseline in another tagged unit is another line's text, and R8b refuses the one shape where
    // using it would go unnoticed.
    //
    // R10: a `TJ` number between the target's string and the tail's is in the tail's prefix, so it
    // moves the tail in the original line and must move with the replacement. Omitting it makes
    // `moved_to` an intention rather than the position the ink lands on — the R8b.1 failure in a
    // different disguise. The numbers are thousandths of an em and are subtracted (ISO 32000-1
    // 9.4.3), and the resulting displacement is scaled by `Tz/100` like every other advance on the
    // line (9.4.4) — the same factor the reader's own kern term carries since T-8, so the two
    // sides of this module cannot disagree about where a kerned tail starts (T-8b).
    let kern_after_target = -items[fragment_index + 1..]
        .iter()
        .take_while(|item| !matches!(item, Object::String(_, _)))
        .filter_map(|item| item.as_float().ok())
        .map(|number| number / 1000.0 * run.font_size * scale)
        .sum::<f32>();
    let prefix_before_target = run.origin.0 - line_origin;
    // Where the `Td` must put the pen: the tail's start **before** its kern. The number stays in
    // the tail array and is applied there, so adding it to the offset as well would apply it
    // twice — measured as 142.704 against the correct 136.704.
    let tail_pen = prefix_before_target + new_width;
    // The measured prefix — the distance from the line origin to where the tail's ink actually
    // starts — includes the kern (R10). This is the number a caller compares against ink.
    let moved_to = line_origin + tail_pen + kern_after_target;
    let offset = td_offset(line_origin, tail_pen);
    // The tail sat at the old prefix, which carried the same kern, so the displacement is exactly
    // the growth: a `shift` that added the kern would overstate the move. T-8b: both operands are
    // scaled, so this is the *scaled* growth — it was the correct number before the scale entered
    // the widths too, but only because both sides were unscaled by the same factor.
    let shift = moved_to - (run.origin.0 + old_width + kern_after_target);

    // Split the array at the tail: everything up to and including the target in one `TJ`, an
    // inserted `Td`, then the rest in a second `TJ`. Splitting rather than shifting a `TJ` number
    // is what makes the move expressible at all — a kern cannot move a run rightwards past its
    // own glyph advances.
    let before: Vec<Object> = items[..=fragment_index].to_vec();
    let after: Vec<Object> = items[fragment_index + 1..].to_vec();

    let mut head = before.clone();
    if let Some(Object::String(bytes, _)) = head.last_mut() {
        *bytes = escape_literal(replacement);
    }

    let replacement_ops = vec![
        Operation::new("TJ", vec![Object::Array(head)]),
        Operation::new("Td", vec![Object::Real(offset), Object::Integer(0)]),
        Operation::new("TJ", vec![Object::Array(after)]),
    ];
    let old_ops = vec![content.operations[target.op_index].clone()];

    content.operations.splice(
        target.op_index..target.op_index + 1,
        replacement_ops.clone(),
    );

    let applied = ReflowApplied {
        operation: ReflowOperation {
            page: page_index,
            op_index: target.op_index,
            xobject_path: level,
            old_ops,
            new_ops: replacement_ops,
        },
        moved_to,
        shift,
    };
    Ok((content, applied))
}

/// Widens one run of a multi-fragment `TJ` array and moves the runs after it with an inserted
/// `Td`, refusing when the widened line leaves the page's **content box**.
///
/// This is the entry point a user edit takes: the box is resolved from the document itself
/// (§3), so no caller can hand in a limit that is not this page's. [`reflow_tj_array`] keeps its
/// caller-supplied limit because the gate tests exercise rules against a chosen number; R3's
/// contract is about *where the number comes from*, and only this function can state it.
///
/// R3's refusal is the `fits-width` gate, not a reflow code of its own: the user is about to be
/// judged by that gate, so a refusal that named a different rule would send them to a remedy
/// that does not exist. It carries the gate's own sentence and both measured widths, with the
/// line's available width standing where the run's own advance used to.
pub fn reflow_line_within_content_box(
    document: &Document,
    page_index: u32,
    page_id: ObjectId,
    runs: &[RawRun],
    target: &RunId,
    replacement: &str,
) -> Result<(Content<Vec<Operation>>, ReflowApplied), AppError> {
    let box_ = content_box(document, page_id)?;
    if !runs.iter().any(|candidate| candidate.id == *target) {
        return Err(AppError::TextRunMismatch);
    }

    match reflow_tj_array(
        document,
        page_index,
        page_id,
        runs,
        target,
        replacement,
        box_.x1,
    ) {
        Ok(success) => Ok(success),
        // R4 is the rule that refuses when the *widened line* does not fit, which is exactly R3's
        // decision. It reaches the user as `fits-width`, with this line's available width — the
        // change R3 calls "a message change, not a gate change".
        Err(refused) if refused.rule == ReflowRefusal::R4Wrap => {
            let (needed, available) = refused.measured.ok_or_else(|| {
                AppError::Other(
                    "the width refusal did not record the widths it compared".to_string(),
                )
            })?;
            Err(AppError::TextRunNotEditable(format!(
                "the replacement needs {needed:.1}pt, available {available:.1}pt; \
                 select a narrower range, or use the annotation tools to add a note \
                 (gate: {})",
                crate::editor::textedit::Gate::FitsWidth.name()
            )))
        }
        Err(refused) => Err(AppError::TextRunNotEditable(format!("{refused}"))),
    }
}

/// The page's content box: the `/CropBox`, defaulting to `/MediaBox` (ISO 32000-1 Table 30).
///
/// Both are **inheritable page attributes** (§7.7.3.4), so both are resolved up the `/Parent`
/// chain, normalised, and read fresh. `export.rs`'s `media_box` must not be reused (§3.3): it has
/// a US-Letter fallback and does not resolve inheritance, which is acceptable for placing a
/// watermark and wrong for a gate — an A4 page that inherits its `/MediaBox` would be treated as
/// Letter and reflowed into a margin the user cannot see.
///
/// A page with neither box anywhere in its chain is malformed, and inventing one would turn a
/// malformed file into a wrong reflow decision, so it is an error rather than a default.
pub fn content_box(document: &Document, page_id: ObjectId) -> Result<PdfRect, AppError> {
    for key in [b"CropBox".as_slice(), b"MediaBox".as_slice()] {
        if let Some(found) = inherited_box(document, page_id, key)? {
            return Ok(found);
        }
    }
    Err(AppError::Other(
        "this page has neither a CropBox nor a MediaBox, so how much room its text has cannot \
         be decided"
            .to_string(),
    ))
}

/// One inheritable box attribute, resolved up the `/Parent` chain and normalised.
///
/// `Ok(None)` means **absent from the page and every ancestor**, which is the only case that
/// falls through to the next box attribute. A box that is *present* and unreadable is an error
/// rather than an absence: treating a malformed `/CropBox` as missing would silently substitute
/// the `/MediaBox` and reflow text into a region the user cannot see, which is the failure §3.2
/// step 3 refuses to invent a box for, one level down.
///
/// Normalisation is not cosmetic either: reflow subtracts the line's right edge from `x1`, so a
/// box given with its corners swapped would report a negative width, read as "no room", and
/// refuse every edit on the page.
fn inherited_box(
    document: &Document,
    page_id: ObjectId,
    key: &[u8],
) -> Result<Option<PdfRect>, AppError> {
    let mut node_id = page_id;
    for _ in 0..crate::editor::textlocate::MAX_PARENT_DEPTH {
        let Ok(node) = document.get_dictionary(node_id) else {
            break;
        };
        if let Ok(value) = node.get(key) {
            return parse_box(document, value, key).map(Some);
        }
        let Ok(parent) = node.get(b"Parent").and_then(|object| object.as_reference()) else {
            break;
        };
        node_id = parent;
    }
    Ok(None)
}

/// One box attribute's four coordinates, dereferenced and normalised.
fn parse_box(document: &Document, value: &Object, key: &[u8]) -> Result<PdfRect, AppError> {
    let malformed = || {
        AppError::Other(format!(
            "this page's {} is not four numbers, so how much room its text has cannot be \
             decided",
            String::from_utf8_lossy(key)
        ))
    };
    let (_, resolved) = document.dereference(value).map_err(|_| malformed())?;
    let array = resolved.as_array().map_err(|_| malformed())?;
    let coordinate = |index: usize| -> Result<f32, AppError> {
        array.get(index).ok_or_else(malformed).and_then(|operand| {
            operand
                .as_float()
                .or_else(|_| operand.as_i64().map(|number| number as f32))
                .map_err(|_| malformed())
        })
    };
    Ok(PdfRect {
        x0: coordinate(0)?,
        y0: coordinate(1)?,
        x1: coordinate(2)?,
        y1: coordinate(3)?,
    }
    .normalised())
}

/// A PDF literal string holding `text`, with `\`, `(` and `)` escaped.
fn escape_literal(text: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len());
    for byte in text.bytes() {
        if matches!(byte, b'\\' | b'(' | b')') {
            out.push(b'\\');
        }
        out.push(byte);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::textlocate::{locate_runs, page_id, write_level, MergeScope};
    use lopdf::{dictionary, Stream};
    use std::path::Path;

    fn fixture(name: &str) -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join(name)
    }

    /// The §5 oracle: per-glyph ink, never `measure`. Duplicated from `textlocate`'s test module
    /// on purpose — a shared helper would let a bug in it make both sides agree.
    fn ink(name: &Path) -> Vec<(char, f32, f32)> {
        let grid = crate::provider::DocumentHandles::new()
            .with_document(name, None, |document| {
                crate::editor::runmetrics::char_grid(document, 0, name)
            })
            .expect("PDFium char grid");
        grid.chars
            .iter()
            .filter(|character| !character.generated)
            .filter_map(|character| {
                character
                    .unicode
                    .map(|glyph| (glyph, character.origin_x, character.origin_y))
            })
            .collect()
    }

    /// A run shaped like the fixture's `AB`, so a gate can be exercised without a document.
    ///
    /// The refusals run before any document access, which is why they can be tested this way —
    /// and must be, because a gate no test can fire is a claim, not a guard.
    fn synthetic(id: RunId, x: f32, text: &str) -> RawRun {
        RawRun {
            id,
            bytes: text.as_bytes().to_vec(),
            text: text.to_string(),
            font_key: b"F1".to_vec(),
            operator: "TJ".to_string(),
            origin: (x, 700.0),
            font_size: 24.0,
            advance: 16.008,
            advance_exact: Some(16.008),
            composable: true,
            face: "Helvetica".to_string(),
            style_class: "Helvetica".to_string(),
            marked_content: Vec::new(),
            rise: 0.0,
            horizontal_scale: 1.0,
            page_origin: (x, 700.0),
            page_scale: Some(1.0),
            frame: crate::editor::geometry::RunFrame::PAGE,
            xobject_refusal: None,
        }
    }

    fn address(op_index: usize, path: Vec<usize>) -> RunId {
        RunId {
            op_index,
            operand_path: path,
            xobject_path: Vec::new(),
        }
    }

    /// R5: a line whose runs come from different content levels is refused *before* any document
    /// is read, so the refusal is testable without a form.
    #[test]
    fn a_line_that_crosses_content_levels_refuses_under_r5() {
        let document = Document::new();
        let page = (1, 0);
        let target = address(3, vec![0, 0]);
        let mut plain = synthetic(target.clone(), 72.0, "AB");
        plain.operator = "Tj".to_string();
        let mut nested = synthetic(address(3, vec![0, 1]), 104.016, "CD");
        // A form's text is a different level: same baseline, different `xobject_path`.
        nested.id.xobject_path = vec![XObjectStep {
            name: b"X1".to_vec(),
            op_index: 2,
        }];
        let runs = vec![plain, nested];

        let refused = reflow_tj_array(&document, 0, page, &runs, &target, "ABab", 1000.0)
            .expect_err("a straddling line cannot be written atomically");
        assert_eq!(refused.rule, ReflowRefusal::R5StraddlesLevels);
        assert_eq!(refused.rule.name(), "R5-straddles-levels");
    }

    /// R6: a sheared matrix means the rest of the line is not horizontal, so there is no
    /// direction in which to move it.
    #[test]
    fn a_sheared_line_refuses_under_r6() {
        let document = Document::new();
        let page = (1, 0);
        let target = address(3, vec![0, 0]);
        let mut sheared = synthetic(target.clone(), 72.0, "AB");
        // 1 unit right, 8 units up: the advance and up axes are no longer perpendicular.
        sheared.frame = crate::editor::geometry::RunFrame::from_vectors((1.0, 8.0), (0.0, 1.0));
        assert!(!sheared.frame.is_orthogonal());
        let runs = vec![sheared];

        let refused = reflow_tj_array(&document, 0, page, &runs, &target, "ABab", 1000.0)
            .expect_err("a sheared line has no horizontal rest-of-line");
        assert_eq!(refused.rule, ReflowRefusal::R6NonUniformTransform);
        assert_eq!(refused.rule.name(), "R6-non-uniform-transform");
    }

    /// R8 is satisfied *structurally* for case (a), so there is no refusal to test.
    ///
    /// This is the T4 finding, written as a test so it cannot rot: a fragment that
    /// `group_runs` would not group with the target is either on another baseline — in which
    /// case `on_baseline` drops it before any write and it is never moved — or it is inside the
    /// same `TJ` array, where a tag/font/size change is not expressible at all. So a
    /// `R8CrossesStructure` refusal was implemented, found to be unreachable, and removed rather
    /// than shipped as a guard no test can fire.
    #[test]
    #[ignore = "requires a real PDFium library; `cargo test -- --include-ignored` after fetching it"]
    fn a_tail_off_the_baseline_is_never_moved_so_r8_needs_no_refusal() {
        let source = fixture("kerned-line.pdf");
        let document = Document::load(&source).expect("fixture loads");
        let page = page_id(&document, 0).expect("page 0");
        let runs = locate_runs(&document, page).expect("runs");
        let target = runs.iter().find(|run| run.text == "AB").expect("AB");

        // The control line's `W` is on baseline 600, the target on 700. Widening `AB` must leave
        // it alone — which is what `on_baseline` guarantees and what the R8 rule would otherwise
        // have to refuse.
        let w_before = ink(&source)
            .iter()
            .find(|(glyph, _, _)| *glyph == 'W')
            .map(|(_, x, _)| *x)
            .expect("W");
        let (content, applied) =
            reflow_tj_array(&document, 0, page, &runs, &target.id, "ABab", 1000.0)
                .expect("case (a)");
        let mut edited = document.clone();
        write_level(&mut edited, page, &[], &content).expect("write page");
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join("r8.pdf");
        edited.save(&path).expect("save");
        let w_after = ink(&path)
            .iter()
            .find(|(glyph, _, _)| *glyph == 'W')
            .map(|(_, x, _)| *x)
            .expect("W");
        assert!(
            (w_after - w_before).abs() < 0.01,
            "the off-baseline run moved by {:.3} pt although it is not on the target's line",
            w_after - w_before
        );
        // And the move it *did* make is the one R2.1 predicts, so the test is not vacuous.
        assert!(applied.shift > 0.0);
    }

    /// R8b's fixture, asserted on the **parsed operator list** rather than on bytes.
    ///
    /// R-2b's refusal is a property of two same-baseline runs in different `merge_scope`s, so
    /// the fixture must carry exactly that: two `BDC`/`EMC` units whose `/MCID`s differ, with
    /// the edit target inside the second, and both runs on one baseline. A byte-level assertion
    /// would pass on a stream that is textually right and semantically wrong — a `BDC` closed
    /// in the wrong place still contains the same tokens.
    #[test]
    fn the_r8b_fixture_has_two_units_on_one_baseline() {
        let source = fixture("two-mcid-one-baseline.pdf");
        let document = Document::load(&source).expect("the R8b fixture is committed");
        let page = page_id(&document, 0).expect("page 0");
        let content = decoded_content(&document, page).expect("page content decodes");

        let mcids: Vec<i64> = content
            .operations
            .iter()
            .filter(|operation| operation.operator == "BDC")
            .filter_map(|operation| operation.operands.get(1))
            .filter_map(|properties| properties.as_dict().ok())
            .filter_map(|properties| properties.get(b"MCID").ok())
            .filter_map(|mcid| mcid.as_i64().ok())
            .collect();
        let closes = content
            .operations
            .iter()
            .filter(|operation| operation.operator == "EMC")
            .count();
        assert_eq!(
            mcids.len(),
            2,
            "the fixture must carry two tagged units, found {mcids:?}"
        );
        assert_eq!(closes, 2, "each unit must be closed by its own EMC");
        assert_ne!(
            mcids[0], mcids[1],
            "the two units must differ by MCID; equal ids would be one unit written twice"
        );

        let runs = locate_runs(&document, page).expect("runs");
        assert_eq!(runs.len(), 3, "HI, AB and CD: {runs:?}");
        let left = runs.iter().find(|run| run.text == "HI").expect("HI");
        let target = runs.iter().find(|run| run.text == "AB").expect("AB");
        let tail = runs.iter().find(|run| run.text == "CD").expect("CD");

        assert!(
            (left.baseline() - target.baseline()).abs() < 1e-3,
            "the two units must share a baseline, or the scope term cannot be reached: {} vs {}",
            left.baseline(),
            target.baseline()
        );
        assert_ne!(
            left.merge_scope(),
            target.merge_scope(),
            "the units must be distinguishable by merge scope, not by tag: both are /P"
        );
        assert!(
            matches!(left.merge_scope(), Some(MergeScope::Block(_)))
                && matches!(target.merge_scope(), Some(MergeScope::Block(_))),
            "the doc says both units are resolved /P blocks; a raw-MCID fallback would make the \
             fixture test the conservative reading instead: {:?} vs {:?}",
            left.merge_scope(),
            target.merge_scope()
        );
        assert_eq!(
            target.merge_scope(),
            tail.merge_scope(),
            "the target and its tail are one unit; otherwise the tail is a third scope"
        );
        assert!(
            left.origin.0 < target.origin.0,
            "the other unit must lie to the left, so a scope-blind line origin is too small: \
             {} vs {}",
            left.origin.0,
            target.origin.0
        );
    }

    /// R5 must refuse **real content**, not only a run pair built in a test.
    ///
    /// `xobject-single.pdf` draws only `/X1 Do`, so before this fixture no committed document
    /// put page text and form text on one baseline: R5 was proven synthetically, against
    /// [`synthetic`] runs. A `locate_runs` that reported the two levels at different baselines
    /// would have left the gate unreachable on every real file while its unit test stayed green
    /// — and the *message* the user reads had no assertion at all, only `name()`.
    #[test]
    fn r5_reports_its_own_message_and_refuses_real_content() {
        let source = fixture("page-and-form-one-line.pdf");
        let document = Document::load(&source).expect("the R5 fixture is committed");
        let page = page_id(&document, 0).expect("page 0");
        let runs = locate_runs(&document, page).expect("runs");

        assert_eq!(runs.len(), 2, "one page run, one form run: {runs:?}");
        let page_run = runs.iter().find(|run| run.text == "PAGE").expect("PAGE");
        let form_run = runs.iter().find(|run| run.text == "FORM").expect("FORM");

        // Preconditions: a fixture that lost either level, or drew them on different
        // baselines, cannot fail below and would pass vacuously.
        assert!(
            page_run.id.xobject_path.is_empty(),
            "PAGE must be drawn by the page itself: {:?}",
            page_run.id.xobject_path
        );
        assert_eq!(
            form_run.id.xobject_path.len(),
            1,
            "FORM must be drawn inside the form"
        );
        assert!(
            (page_run.baseline() - form_run.baseline()).abs() < 1e-3,
            "the two levels must share a baseline, or nothing straddles it: {} vs {}",
            page_run.baseline(),
            form_run.baseline()
        );
        assert!(
            page_run.origin.0 < form_run.origin.0,
            "the page text must start left of the form's, so a level-blind line origin is \
             observably too small: {} vs {}",
            page_run.origin.0,
            form_run.origin.0
        );

        let refused = reflow_tj_array(&document, 0, page, &runs, &form_run.id, "FORMER", 1000.0)
            .expect_err("page text and form text on one baseline cannot be moved as one line");
        assert_eq!(refused.rule, ReflowRefusal::R5StraddlesLevels);
        assert_eq!(refused.rule.name(), "R5-straddles-levels");
        assert_eq!(
            refused.rule.message(),
            "this line mixes page text with text inside a form, so its runs cannot be moved as \
             one line",
            "the sentence the user reads is part of the refusal, not decoration"
        );
        assert!(
            refused.to_string().contains("R5-straddles-levels"),
            "Display must name the rule, so a log is attributable: {refused}"
        );
    }

    /// Every refusal's user-visible text is pinned, and the pin cannot go stale.
    ///
    /// `name()` and `message()` are two exhaustive `match`es, so a new variant is *forced* to
    /// gain both — but nothing forced the table below to gain it, and a message that changes
    /// silently is a user-visible change with no test to notice. So the variant list is read
    /// from this file's own `enum` body: a variant added there and absent here fails the guard,
    /// which is the direction a hand-maintained list alone cannot cover.
    #[test]
    fn every_reflow_refusal_names_itself_and_its_remedy() {
        const PINNED: [(ReflowRefusal, &str, &str); 7] = [
            (
                ReflowRefusal::R1NoMove,
                "R1-no-move",
                "this replacement is not wider than the text it replaces, so nothing after it \
                 needs to move",
            ),
            (
                ReflowRefusal::R4Wrap,
                "R4-wrap",
                "this replacement does not fit on its line, and reflow does not wrap text onto \
                 the next line",
            ),
            (
                ReflowRefusal::R5StraddlesLevels,
                "R5-straddles-levels",
                "this line mixes page text with text inside a form, so its runs cannot be moved \
                 as one line",
            ),
            (
                ReflowRefusal::R6NonUniformTransform,
                "R6-non-uniform-transform",
                "this text is drawn through a non-uniform transform, so the rest of its line is \
                 not horizontal",
            ),
            (
                ReflowRefusal::R8bScopeLeftOfLine,
                "R8b-scope-left-of-line",
                "text from another tagged or structure unit lies to the left of this line, so \
                 the rest of the line cannot be moved as one",
            ),
            (
                ReflowRefusal::R10bUnattributedPrefix,
                "R10b-unattributed-kern",
                "another run on this line is positioned outside this TJ array, so reflow cannot \
                 tell where the rest of the line starts",
            ),
            (
                ReflowRefusal::NotOneArray,
                "not-one-array",
                "this text has no following run in the same TJ array, which is the only shape \
                 reflow can currently move",
            ),
        ];

        let declared = declared_variants(include_str!("reflow.rs"));
        assert!(
            declared.len() >= 7,
            "the enum body parsed to {declared:?}; the guard is blind to its own source"
        );
        let pinned: Vec<String> = PINNED
            .iter()
            .map(|(rule, _, _)| format!("{rule:?}"))
            .collect();
        let mut unpinned: Vec<&String> = declared.iter().filter(|v| !pinned.contains(v)).collect();
        let mut unknown: Vec<&String> = pinned.iter().filter(|v| !declared.contains(v)).collect();
        unpinned.sort();
        unknown.sort();
        assert!(
            unpinned.is_empty() && unknown.is_empty(),
            "the pinned table and the enum disagree — a variant with no pinned message is \
             unasserted, and a pin with no variant is stale. Declared but unpinned: {unpinned:?}; \
             pinned but not declared: {unknown:?}"
        );

        for (rule, name, message) in PINNED {
            assert_eq!(rule.name(), name, "{rule:?} reports the wrong identifier");
            assert_eq!(
                rule.message(),
                message,
                "{rule:?} reports the wrong message"
            );
        }

        let names: std::collections::BTreeSet<&str> = PINNED.iter().map(|(_, n, _)| *n).collect();
        let messages: std::collections::BTreeSet<&str> =
            PINNED.iter().map(|(_, _, m)| *m).collect();
        assert_eq!(
            names.len(),
            PINNED.len(),
            "two refusals share an identifier"
        );
        assert_eq!(
            messages.len(),
            PINNED.len(),
            "two refusals share a message, so the remedy named would be the wrong one"
        );
    }

    /// The variant list is read out of source text, so the reader itself needs a crafted sample:
    /// a parser that silently returned "nothing found" would make
    /// [`every_reflow_refusal_names_itself_and_its_remedy`] pass on an empty set, which is the
    /// vacuous-guard failure this repository has produced twice.
    ///
    /// The sample carries every shape a variant can take in this file's style — a doc comment
    /// above a variant, a plain code comment, a `#[serde]` attribute, a one-line struct variant
    /// and a **multi-line** one — plus a second `enum` after it, so a split that ran past the
    /// closing brace would be visible.
    #[test]
    fn the_variant_reader_sees_its_own_source_and_not_a_neighbouring_enum() {
        const SAMPLE: &str = "\
pub enum ReflowRefusal {
    /// A doc comment, which is not a variant.
    First,
    // A plain code comment, also not a variant.
    #[serde(rename = \"second\")]
    Second { field: u8 },
    Third {
        width: f32,
    },
    Fourth,
}

pub enum SomethingElse {
    NotAVariant,
}
";
        assert_eq!(
            declared_variants(SAMPLE),
            vec!["First", "Second", "Third", "Fourth"],
            "the reader must skip comments and attributes, join a multi-line variant body, and \
             stop at the closing brace"
        );
        assert_eq!(
            declared_variants(include_str!("reflow.rs")),
            vec![
                "R1NoMove",
                "R4Wrap",
                "R5StraddlesLevels",
                "R6NonUniformTransform",
                "R8bScopeLeftOfLine",
                "R10bUnattributedPrefix",
                "NotOneArray",
            ],
            "the guard must see exactly the seven declared variants, in source order"
        );
    }

    /// The variant names in a `pub enum ReflowRefusal { … }` body, in source order.
    ///
    /// Brace depth is tracked rather than assumed, so a variant written with a body across
    /// several lines is still one entry: a line-based reader would report the *fields* as
    /// variants and the variant itself as nothing, which is a silently shorter list.
    fn declared_variants(source: &str) -> Vec<String> {
        let body = source
            .split("pub enum ReflowRefusal {")
            .nth(1)
            .and_then(|rest| rest.split("\n}").next())
            .expect("the guard must find the enum body it governs");

        let mut names = Vec::new();
        let mut pending = String::new();
        let mut depth = 0usize;
        for line in body.lines() {
            let line = line.trim();
            if depth == 0 && (line.starts_with("//") || line.starts_with("#[")) {
                continue;
            }
            pending.push(' ');
            pending.push_str(line);
            depth += line.matches('{').count();
            depth = depth.saturating_sub(line.matches('}').count());
            if depth > 0 || !line.ends_with(',') {
                continue;
            }
            let declaration = pending.trim().trim_end_matches(',').trim();
            let name: String = declaration
                .chars()
                .take_while(|character| character.is_ascii_alphanumeric())
                .collect();
            if !name.is_empty() {
                names.push(name);
            }
            pending.clear();
        }
        names
    }

    /// R8b: a same-baseline run in another tagged unit must not set the line origin, and where it
    /// lies to the left of the target's own line start the move is refused rather than measured.
    ///
    /// `line_origin` is the minimum `origin.0` over the runs `on_baseline` keeps, and
    /// `on_baseline` filters on `baseline()` alone — `origin.1 + rise`, with no scope term — so
    /// `two-mcid-one-baseline.pdf`'s `HI` (another `/P` unit, 128 pt further left) makes the
    /// computed origin too small by exactly that difference. `Td` is relative to the line matrix,
    /// so the tail would be drawn 128 pt right of where it belongs.
    ///
    /// The refusal is the ruling's, not a stronger one: R8b.1 shows `moved_to` reduces to
    /// `run.origin.0 + new_width` whatever `line_origin` is, so the record agrees with itself
    /// while the ink is displaced. Correcting the origin would leave that reporting defect in
    /// place, and a caller comparing the record to the ink would go on being told they agree.
    #[test]
    fn a_same_baseline_run_in_another_mcid_does_not_dislocate_the_tail() {
        let source = fixture("two-mcid-one-baseline.pdf");
        let document = Document::load(&source).expect("the R8b fixture is committed");
        let page = page_id(&document, 0).expect("page 0");
        let runs = locate_runs(&document, page).expect("runs");
        let target = runs.iter().find(|run| run.text == "AB").expect("AB");
        let left = runs.iter().find(|run| run.text == "HI").expect("HI");

        // Preconditions: the shape below cannot fail on a fixture that lost one of its units, so
        // say which half is missing instead of passing vacuously.
        assert!(
            (left.baseline() - target.baseline()).abs() < 1e-3,
            "the other unit must share the target's baseline: {} vs {}",
            left.baseline(),
            target.baseline()
        );
        assert!(
            left.origin.0 < target.origin.0,
            "the other unit must lie to the left, or a scope-blind line origin is too *large* and \
             no displacement follows: {} vs {}",
            left.origin.0,
            target.origin.0
        );
        assert_ne!(
            left.merge_scope(),
            target.merge_scope(),
            "the two units must be in different merge scopes"
        );

        let refused = reflow_tj_array(&document, 0, page, &runs, &target.id, "ABab", 1000.0)
            .expect_err("a line whose origin another tagged unit owns cannot be moved");
        assert_eq!(
            refused.rule.name(),
            "R8b-scope-left-of-line",
            "the out-of-scope run must be refused by R8b; a `Td` computed from it would put the \
             tail 128.0 pt right of its line"
        );
    }

    /// The other half of R8b: a run in another tagged unit does not set this line's extent either,
    /// so one that lies to the **right** is excluded rather than refused.
    ///
    /// Written against the committed fixture *plus* one injected run, not against synthesized
    /// runs alone, because the extent check sits after the document read: a wholly synthetic pair
    /// fails at the content read on an empty document and never reaches R4, so it would pass
    /// whatever `line_right` counted. Measured by mutation — swapping `line_right` back to
    /// `on_baseline` left the synthetic version green.
    ///
    /// The foreign run is placed at x=400, past any limit that lets this line grow, so counting it
    /// refuses under R4 while excluding it lets the move through. That difference is the only
    /// observation of the extent half there is; the fixture's own other unit is to the left.
    #[test]
    fn a_run_outside_the_scope_does_not_set_the_line_extent() {
        use crate::editor::textlocate::MarkedContent;

        let source = fixture("two-mcid-one-baseline.pdf");
        let document = Document::load(&source).expect("the R8b fixture is committed");
        let page = page_id(&document, 0).expect("page 0");
        // The fixture's own second unit (`HI`) lies left of this line, which is R8b's *refusal*
        // case; it is dropped here so the only out-of-scope run is the injected right-hand one.
        let runs: Vec<RawRun> = locate_runs(&document, page)
            .expect("runs")
            .into_iter()
            .filter(|run| run.text != "HI")
            .collect();
        let target = runs.iter().find(|run| run.text == "AB").expect("AB");
        let tail = runs.iter().find(|run| run.text == "CD").expect("CD");
        assert_eq!(target.merge_scope(), tail.merge_scope());
        let line_right = tail.page_origin.0 + tail.advance;

        // A neighbour in another unit, on this baseline, clear of this line's own text.
        let mut neighbour = synthetic(address(3, vec![1, 0]), 400.0, "XY");
        neighbour.marked_content = vec![MarkedContent {
            tag: "P".to_string(),
            mcid: Some(7),
            actual_text: None,
            block: Some((9, 0)),
        }];
        assert_ne!(neighbour.merge_scope(), target.merge_scope());
        assert!(
            neighbour.page_origin.0 > line_right,
            "the neighbour must sit right of this line, or it is the R8b refusal case instead"
        );

        let mut with_neighbour = runs.clone();
        with_neighbour.push(neighbour);
        // Room for the growth (26.688) on this line, and none for the neighbour.
        let limit = line_right + 27.0;
        let (_, applied) = reflow_tj_array(
            &document,
            0,
            page,
            &with_neighbour,
            &target.id,
            "ABab",
            limit,
        )
        .expect("this line fits its own limit; the neighbour's text is not its extent");
        assert!(
            (applied.shift - 26.688).abs() < 0.01,
            "the tail moved by {:.3} pt; only the growth moves it, so the other unit's text did \
             not enter this line's arithmetic",
            applied.shift
        );
    }

    /// R10b: the measured prefix belongs to **one** `TJ` array. A run whose position also
    /// depends on an operator outside that array — a show operator before it, or a `Td` that
    /// started a second array on the same baseline — is refused, because the array's own walk
    /// cannot see the displacement and the reported tail origin would be an intention.
    ///
    /// Both shapes are built as run pairs rather than found as files: the reader advances the
    /// pen only *inside* an array (`TextMatrixWalk::step` is a no-op for `Tj`), so a preceding
    /// show leaves the array's reported origin equal to its line matrix origin while the ink
    /// starts an advance further right. No committed fixture has that shape.
    #[test]
    fn a_kern_that_spans_a_tj_or_mid_array_td_refuses() {
        let document = Document::new();
        let page = (1, 0);

        // Shape 1 — a `Tj` before the array. The reader reports both runs at x=72 because a
        // show does not move `tm`, so the target's prefix looks like zero when the ink says
        // otherwise.
        let before = address(3, vec![0]);
        let mut preceding = synthetic(before, 72.0, "WXYZ");
        preceding.operator = "Tj".to_string();
        let target = address(4, vec![0, 0]);
        let mut arrayed = synthetic(target.clone(), 72.0, "AB");
        arrayed.operator = "TJ".to_string();
        let refused = reflow_tj_array(
            &document,
            0,
            page,
            &[preceding.clone(), arrayed.clone()],
            &target,
            "ABab",
            1000.0,
        )
        .expect_err("a prefix the array walk cannot see must be refused, not measured");
        assert_eq!(
            refused.rule.name(),
            "R10b-unattributed-kern",
            "shape 1 (a `Tj` before the array) refused as {:?}",
            refused.rule.name()
        );

        // Shape 2 — a `Td` that began a second array on the same baseline, further left. A `Td`
        // cannot occur inside an array (its elements are strings and numbers only), so this is
        // the shape the ruling describes.
        let earlier = synthetic(address(3, vec![0, 0]), 40.0, "AB");
        let later = address(6, vec![0, 0]);
        let mut positioned = synthetic(later.clone(), 120.0, "CD");
        positioned.operator = "TJ".to_string();
        let refused = reflow_tj_array(
            &document,
            0,
            page,
            &[earlier, positioned],
            &later,
            "CDcd",
            1000.0,
        )
        .expect_err(
            "a run positioned by its own `Td` leaves the earlier line's prefix unattributed",
        );
        assert_eq!(
            refused.rule.name(),
            "R10b-unattributed-kern",
            "shape 2 (a second array started by `Td`) refused as {:?}",
            refused.rule.name()
        );

        // Non-vacuity control: one clean array reaches the *document* read and fails there. If
        // R10b had fired on it, this would be the R10b refusal instead — so the rule is not a
        // refusal of every run pair.
        let clean = address(3, vec![0, 0]);
        let control = reflow_tj_array(
            &document,
            0,
            page,
            &[synthetic(clean.clone(), 72.0, "AB")],
            &clean,
            "ABab",
            1000.0,
        )
        .expect_err("an empty document has no page content to read");
        assert_eq!(control.rule.name(), "not-one-array");
    }

    /// The other half of R8: two fragments inside one `TJ` still cannot carry different tags,
    /// which is asserted rather than assumed, so the day a producer makes it possible the test
    /// is what notices.
    #[test]
    fn one_tj_array_cannot_carry_two_tagged_units() {
        let source = fixture("kerned-line.pdf");
        let document = Document::load(&source).expect("fixture loads");
        let page = page_id(&document, 0).expect("page 0");
        let runs = locate_runs(&document, page).expect("runs");
        let arrays: std::collections::BTreeMap<usize, Vec<&RawRun>> =
            runs.iter().fold(Default::default(), |mut map, run| {
                map.entry(run.id.op_index).or_default().push(run);
                map
            });
        for (op_index, members) in arrays.iter().filter(|(_, members)| members.len() > 1) {
            let first = members[0].merge_scope();
            assert!(
                members.iter().all(|member| member.merge_scope() == first),
                "a TJ array at op {op_index} carries fragments in different tagged units"
            );
        }
    }

    /// The x of `glyph` on the baseline at `y`, which is the only unambiguous lookup once a
    /// replacement may contain the very letter being tracked.
    fn at(ink: &[(char, f32, f32)], glyph: char, y: f32) -> f32 {
        let matches: Vec<f32> = ink
            .iter()
            .filter(|(candidate, _, baseline)| *candidate == glyph && (*baseline - y).abs() < 0.5)
            .map(|(_, x, _)| *x)
            .collect();
        assert_eq!(
            matches.len(),
            1,
            "expected exactly one {glyph:?} on baseline {y}, got {ink:?}"
        );
        matches[0]
    }

    /// R2.1 in isolation: the offset is measured from the line origin, so the pen's accumulated
    /// advance is discarded by construction and not by accident.
    #[test]
    fn the_td_offset_is_measured_from_the_line_origin() {
        // `CD` starts at 110.016 on kerned-line.pdf; the line began at 72.
        let line_origin = 72.0;
        let prefix = 110.016 - 72.0;
        assert!((td_offset(line_origin, prefix) - 38.016).abs() < 1e-3);

        // The wrong formulation, written down so the difference is a number and not a claim. A
        // pen-delta writer asks for `Td shift`, believing the pen carries; `Td` is relative to the
        // line origin, so the tail lands `pen - line_origin` short of where it was asked for.
        // On this line the pen has advanced 38.016 pt when the tail starts, so:
        let shift = 40.0_f32;
        let pen_x = line_origin + 38.016;
        let landed_by_pen_delta_thinking = line_origin + shift; // what `Td shift` really does
        let error = (pen_x + shift) - landed_by_pen_delta_thinking;
        assert!((error - 38.016).abs() < 1e-3, "error {error}");
        // And that is the measured 1.984 pt: asked 40, got 40 - 38.016.
        assert!((shift - error - 1.984).abs() < 1e-3);
    }

    /// A wider replacement of `AB` moves `CD` right by exactly the growth, and leaves the control
    /// line untouched.
    ///
    /// Gated on PDFium because the oracle is per-glyph ink: comparing `measure` to `measure` would
    /// make both sides of the assertion agree by construction and prove nothing.
    #[test]
    #[ignore = "requires a real PDFium library; `cargo test -- --include-ignored` after fetching it"]
    fn a_wider_replacement_moves_the_trailing_run_by_the_growth() {
        let source = fixture("kerned-line.pdf");
        let mut document = Document::load(&source).expect("fixture loads");
        let page = page_id(&document, 0).expect("page 0");
        let runs = locate_runs(&document, page).expect("runs");
        let target = runs
            .iter()
            .find(|run| run.text == "AB")
            .expect("the AB run");

        let before = ink(&source);
        let cd_before = at(&before, 'C', 700.0);
        let w_before = at(&before, 'W', 600.0);

        // At 24 pt: "AB" = (667+667)/1000*24 = 32.016; "ABab" = (667+667+556+556)/1000*24 =
        // 58.704. Growth 26.688. `a`/`b` are chosen because they appear nowhere else on the
        // page, so the ink lookup stays unambiguous.
        let (content, applied) =
            reflow_tj_array(&document, 0, page, &runs, &target.id, "ABab", 1000.0)
                .expect("a wider replacement in one array is case (a)");
        assert!(
            (applied.shift - 26.688).abs() < 0.01,
            "shift {}",
            applied.shift
        );

        write_level(&mut document, page, &[], &content).expect("write page");
        let directory = tempfile::tempdir().expect("temp dir");
        let edited = directory.path().join("reflowed.pdf");
        document.save(&edited).expect("save");

        let after = ink(&edited);
        // The replacement deliberately avoids `C` and `D`: widening `AB` to `ABC` puts a second
        // `C` on the page, and a lookup by character alone then silently tracks the wrong glyph.
        // That is a real weakness of an ink oracle, and `at` closes it by requiring one match.
        let cd_after = at(&after, 'C', 700.0);
        let w_after = at(&after, 'W', 600.0);

        assert!(
            (cd_after - (cd_before + applied.shift)).abs() < 0.05,
            "C moved {:.3} pt, the growth and the Td both say {:.3}",
            cd_after - cd_before,
            applied.shift
        );
        assert!(
            (w_after - w_before).abs() < 0.05,
            "the control line moved {:.3} pt",
            w_after - w_before
        );
    }

    /// T-8b: the reflow's own measurements carry `Tz`, so the position it reports is the
    /// position a scaled page's ink lands on.
    ///
    /// `kerned-line-tz.pdf` is `Tz 50`, `Td 72 700`, `TJ [(AB) -250 (CD)]` at `Tf 24`. The reader
    /// places `CD` at **91.008** (T-8: `72 + 16.008 + 3`), so its origin is *scaled* text space.
    /// Widening `AB` to `ABab` grows the line by **29.352** and the 3 pt kern stays in the tail's
    /// prefix, so the tail's ink starts at **104.352**. Unscaled arithmetic reports and writes
    /// **136.704** — the `Tz 100` answer, 32.352 pt too far right, of which 29.352 is the width
    /// factor and 3.000 the kern factor.
    ///
    /// This runs without PDFium on purpose: the defect is arithmetic, and the `#[ignore]`d sibling
    /// below is the only test that also compares against ink. Linux CI never runs that one.
    #[test]
    fn the_reflows_tail_position_carries_the_horizontal_scale() {
        const SIZE: f32 = 24.0;
        const UNSCALED_AB: f32 = (667.0 + 667.0) / 1000.0 * SIZE; // 32.016
        const UNSCALED_ABAB: f32 = (667.0 + 667.0 + 556.0 + 556.0) / 1000.0 * SIZE; // 58.704
        const UNSCALED_KERN: f32 = 250.0 / 1000.0 * SIZE; // 6.000
        const LINE_ORIGIN: f32 = 72.0;
        let scale = crate::editor::testpdf::KERNED_LINE_TZ as f32 / 100.0;

        let source = fixture("kerned-line-tz.pdf");
        let document = Document::load(&source).expect("fixture loads");
        let page = page_id(&document, 0).expect("page 0");
        let runs = locate_runs(&document, page).expect("runs");
        let ab = runs.iter().find(|run| run.text == "AB").expect("AB");
        let cd = runs.iter().find(|run| run.text == "CD").expect("CD");

        // Precondition: the reader *is* scaled, so `origin` is not the unscaled pen. Without
        // this a fixture that lost its `Tz` would make every number below agree with itself.
        assert!(
            (cd.origin.0 - (LINE_ORIGIN + UNSCALED_AB * scale + UNSCALED_KERN * scale)).abs()
                < 0.05,
            "the reader puts `CD` at {:.3}; a scaled walk puts it at {:.3}",
            cd.origin.0,
            LINE_ORIGIN + UNSCALED_AB * scale + UNSCALED_KERN * scale
        );

        let (_, applied) = reflow_tj_array(&document, 0, page, &runs, &ab.id, "ABab", 1000.0)
            .expect("a wider replacement in one array is case (a)");

        let scaled = LINE_ORIGIN + UNSCALED_ABAB * scale + UNSCALED_KERN * scale;
        let unscaled = LINE_ORIGIN + UNSCALED_ABAB + UNSCALED_KERN;
        assert!(
            (unscaled - scaled).abs() > 0.05,
            "`Tz {}` moves the answer only {:.3} pt — this fixture could not tell the two \
             readings apart",
            crate::editor::testpdf::KERNED_LINE_TZ,
            unscaled - scaled
        );
        assert!(
            (applied.moved_to - scaled).abs() < 0.05,
            "the reflow reports its tail at {:.3}; a scaled walk puts it at {:.3} \
             (the `Tz 100` answer is {:.3})",
            applied.moved_to,
            scaled,
            unscaled
        );
        // The move is the *scaled* growth: comparing against the unscaled one would put the
        // tail a further 13.344 pt right without changing the reported number.
        let scaled_growth = (UNSCALED_ABAB - UNSCALED_AB) * scale;
        assert!(
            (applied.shift - scaled_growth).abs() < 0.01,
            "the tail moved {} pt; the scaled growth is {scaled_growth}",
            applied.shift
        );
    }

    /// T-8b's acceptance test: the position the reflow **reports** is the position the ink
    /// **lands on**, at a horizontal scaling that is not 100.
    ///
    /// `a_kerned_prefix_includes_the_kern` compares `moved_to` to a hand-computed expectation at
    /// `Tz 100`, where every `Tz/100` factor is arithmetically invisible; it agrees with itself
    /// whatever the reflow does. The quantity that must be compared to ink is `moved_to`, not
    /// `shift` — `shift` is the growth, and a report that is the unscaled answer can still move
    /// by a plausible number.
    #[test]
    #[ignore = "requires a real PDFium library; `cargo test -- --include-ignored` after fetching it"]
    fn a_reflows_reported_tail_origin_matches_the_ink_at_tz_50() {
        const SIZE: f32 = 24.0;
        const UNSCALED_AB: f32 = (667.0 + 667.0) / 1000.0 * SIZE;
        const UNSCALED_ABAB: f32 = (667.0 + 667.0 + 556.0 + 556.0) / 1000.0 * SIZE;
        const UNSCALED_KERN: f32 = 250.0 / 1000.0 * SIZE;
        const LINE_ORIGIN: f32 = 72.0;
        let scale = crate::editor::testpdf::KERNED_LINE_TZ as f32 / 100.0;

        let source = fixture("kerned-line-tz.pdf");
        let mut document = Document::load(&source).expect("fixture loads");
        let page = page_id(&document, 0).expect("page 0");
        let runs = locate_runs(&document, page).expect("runs");
        let ab = runs.iter().find(|run| run.text == "AB").expect("AB");

        let c_before = at(&ink(&source), 'C', 700.0);
        assert!(
            (c_before - (LINE_ORIGIN + UNSCALED_AB * scale + UNSCALED_KERN * scale)).abs() < 0.05,
            "`CD` is not scaled where 9.4.3 puts it: `C` drew at {c_before:.3}, expected {:.3}",
            LINE_ORIGIN + UNSCALED_AB * scale + UNSCALED_KERN * scale
        );

        let (content, applied) = reflow_tj_array(&document, 0, page, &runs, &ab.id, "ABab", 1000.0)
            .expect("a wider replacement in one array is case (a)");
        write_level(&mut document, page, &[], &content).expect("write page");
        let directory = tempfile::tempdir().expect("temp dir");
        let edited = directory.path().join("reflowed-tz.pdf");
        document.save(&edited).expect("save");

        let c_after = at(&ink(&edited), 'C', 700.0);
        let expected = LINE_ORIGIN + UNSCALED_ABAB * scale + UNSCALED_KERN * scale;
        assert!(
            (c_after - expected).abs() < 0.05,
            "the write put `C` at {c_after:.3}; a scaled walk puts it at {expected:.3} \
             (`Tz 100` would give {:.3})",
            LINE_ORIGIN + UNSCALED_ABAB + UNSCALED_KERN
        );
        assert!(
            (applied.moved_to - c_after).abs() < 0.05,
            "the reflow reports its tail at {:.3}; the renderer drew it at {c_after:.3} — off by \
             {:.3} pt",
            applied.moved_to,
            applied.moved_to - c_after
        );
    }

    /// R10: the measured prefix of a run positioned by a kern inside one `TJ` array includes
    /// that kern, so the position `reflow_tj_array` reports is the position the ink lands on.
    ///
    /// `kerned-line.pdf` line 1 is `Td 72 700; [(AB) -250 (CD)] TJ` at `Tf 24`. `CD` therefore
    /// starts the width of `AB` **plus** `-(-250)/1000 x 24 = 6 pt` from the line origin, and
    /// that 6 pt belongs to its prefix. Excluding it leaves `moved_to` short by exactly the
    /// kern: the reported number becomes an intention rather than the fact, which is the
    /// defect R8b.1 refuses over in a different disguise.
    #[test]
    #[ignore = "requires a real PDFium library; `cargo test -- --include-ignored` after fetching it"]
    fn a_kerned_prefix_includes_the_kern() {
        // Helvetica at 24 pt, thousandths of an em: `A` = `B` = 667, `a` = `b` = 556.
        const OLD_WIDTH: f32 = (667.0 + 667.0) / 1000.0 * FONT_SIZE; // 32.016
        const NEW_WIDTH: f32 = (667.0 + 667.0 + 556.0 + 556.0) / 1000.0 * FONT_SIZE; // 58.704
        const LINE_ORIGIN: f32 = 72.0;
        const FONT_SIZE: f32 = 24.0;
        // `-KERNED_LINE_KERN / 1000 x Tf`, at `Tz 100` where the reader and 9.4.3 agree (T-8).
        // Taken from the fixture's own constant rather than transcribed, so a fixture that changed
        // its kern would move the expectation with it instead of passing on a stale 6.
        const KERN: f32 = -(crate::editor::testpdf::KERNED_LINE_KERN as f32) / 1000.0 * FONT_SIZE;

        let source = fixture("kerned-line.pdf");
        let mut document = Document::load(&source).expect("fixture loads");
        let page = page_id(&document, 0).expect("page 0");
        let runs = locate_runs(&document, page).expect("runs");
        let target = runs
            .iter()
            .find(|run| run.text == "AB")
            .expect("the AB run");
        assert!(
            (target.advance - OLD_WIDTH).abs() < 1e-3,
            "the reader measures `AB` at {}; the AFM sum is {OLD_WIDTH}",
            target.advance
        );

        let before = ink(&source);
        let c_before = at(&before, 'C', 700.0);
        // Precondition: on a fixture that lost its kern nothing below can fail, so say so here
        // rather than passing vacuously.
        assert!(
            (c_before - (LINE_ORIGIN + OLD_WIDTH + KERN)).abs() < 0.05,
            "`CD` is not kern-positioned: it drew at {c_before:.3}, and {} + {OLD_WIDTH} + {KERN} \
             is {:.3}",
            LINE_ORIGIN,
            LINE_ORIGIN + OLD_WIDTH + KERN
        );

        let (content, applied) =
            reflow_tj_array(&document, 0, page, &runs, &target.id, "ABab", 1000.0)
                .expect("a wider replacement in one array is case (a)");

        assert!(
            (applied.moved_to - (LINE_ORIGIN + NEW_WIDTH + KERN)).abs() < 0.05,
            "the prefix ends at {:.3}; the tail's ink origin is {} + {NEW_WIDTH} + {KERN} = {:.3}",
            applied.moved_to,
            LINE_ORIGIN,
            LINE_ORIGIN + NEW_WIDTH + KERN
        );
        assert!(
            (applied.shift - (NEW_WIDTH - OLD_WIDTH)).abs() < 0.01,
            "the tail moved {} pt; only the growth ({}) should move it",
            applied.shift,
            NEW_WIDTH - OLD_WIDTH
        );

        write_level(&mut document, page, &[], &content).expect("write page");
        let directory = tempfile::tempdir().expect("temp dir");
        let edited = directory.path().join("kerned-prefix.pdf");
        document.save(&edited).expect("save");

        let c_after = at(&ink(&edited), 'C', 700.0);
        assert!(
            (c_after - applied.moved_to).abs() < 0.05,
            "the write reports its tail at {:.3}; the renderer drew it at {c_after:.3}",
            applied.moved_to
        );
    }

    #[test]
    fn a_narrower_replacement_refuses_under_r1() {
        let source = fixture("kerned-line.pdf");
        let document = Document::load(&source).expect("fixture loads");
        let page = page_id(&document, 0).expect("page 0");
        let runs = locate_runs(&document, page).expect("runs");
        let target = runs.iter().find(|run| run.text == "AB").expect("AB");

        let refused = reflow_tj_array(&document, 0, page, &runs, &target.id, "A", 1000.0)
            .expect_err("a narrower replacement is not reflow");
        assert_eq!(refused.rule, ReflowRefusal::R1NoMove);
        assert_eq!(refused.rule.name(), "R1-no-move");
    }

    /// Row A10's guard, on the fixture R-3 committed for it.
    ///
    /// `kerned-line.pdf`'s guard above asserts the arithmetic of one move. This asserts the
    /// row's whole contract on a document whose second line exists: the run before the edit
    /// point does not move, the tail moves by exactly the measured growth, every glyph on the
    /// untouched line keeps its origin, and the edited run reads back as the requested string
    /// after a save.
    ///
    /// The replacement is `ALPHAab ` rather than `ALPHABET ` for the reason the neighbouring
    /// test records: widening into letters the tail already carries (`B`, `E`, `T`) puts a
    /// second copy of the tracked glyph on the page, and an ink oracle that looks a character up
    /// then silently reads the wrong one. `a` and `b` appear nowhere in the fixture, and the
    /// glyph this test tracks for "did the front of the line move" is `L`, of which the head
    /// holds exactly one before and after.
    #[test]
    #[ignore = "requires a real PDFium library; `cargo test -- --include-ignored` after fetching it"]
    fn a_wider_replacement_moves_the_rest_of_the_line_and_nothing_else() {
        const LINE_Y: f32 = 700.0;
        const OTHER_Y: f32 = 640.0;
        /// The content box's right edge: the row's `fits-width` limit (design §4 R3).
        const LIMIT_X: f32 = 612.0;
        const SIZE: f32 = 24.0;

        let width = |text: &str| {
            crate::editor::afm::text_width("Helvetica", text, SIZE)
                .unwrap_or_else(|| panic!("Helvetica covers {text:?}"))
        };

        let source = fixture("reflow-line.pdf");
        let mut document = Document::load(&source).expect("the A10 fixture is committed");
        let page = page_id(&document, 0).expect("page 0");
        let runs = locate_runs(&document, page).expect("runs");
        let target = runs
            .iter()
            .find(|run| run.text == "ALPHA ")
            .expect("the A10 fixture's edited run");
        assert!(
            runs.iter().any(|run| run.text == "BETA"),
            "the fixture lost its tail: {:?}",
            runs.iter().map(|run| run.text.as_str()).collect::<Vec<_>>()
        );

        let old_width = width("ALPHA ");
        let new_width = width("ALPHAab ");
        let growth = new_width - old_width;
        assert!(
            growth > 0.0,
            "the replacement must be wider, or R1 refuses and there is nothing to move"
        );
        assert!(
            (target.advance - old_width).abs() < 1e-3,
            "the reader measures `ALPHA ` at {}; the AFM sum is {old_width}",
            target.advance
        );

        let before = ink(&source);
        // The line origin, from the reader rather than from a glyph: `A` occurs twice in the
        // fixture (`ALPHA ` and `BETA`'s neighbour line), so an ink lookup by character cannot
        // name it uniquely.
        let line_origin = target.origin.0;
        let l_before = at(&before, 'L', LINE_Y);
        let b_before = at(&before, 'B', LINE_Y);
        let w_before = at(&before, 'W', OTHER_Y);

        assert!(
            (l_before - (line_origin + width("A"))).abs() < 0.05,
            "`L` drew at {l_before:.3}; the head's own advances put it at {:.3}",
            line_origin + width("A")
        );
        // Precondition: the tail must be the `-120` kern's position, or a fixture that lost its
        // seam would make (2) below agree with a wrong expectation.
        let kern = 120.0 / 1000.0 * SIZE;
        assert!(
            (b_before - (line_origin + old_width + kern)).abs() < 0.05,
            "the fixture's tail is not where the seam puts it: `B` drew at {b_before:.3}, \
             expected {:.3}",
            line_origin + old_width + kern
        );

        let (content, applied) =
            reflow_tj_array(&document, 0, page, &runs, &target.id, "ALPHAab ", LIMIT_X)
                .expect("a wider replacement that fits its line is case (a)");
        assert!(
            (applied.shift - growth).abs() < 0.01,
            "the write reports a shift of {}; the measured growth is {growth}",
            applied.shift
        );

        write_level(&mut document, page, &[], &content).expect("write page");
        let directory = tempfile::tempdir().expect("temp dir");
        let edited = directory.path().join("reflow-line-edited.pdf");
        document.save(&edited).expect("save");

        let after = ink(&edited);

        // (1) The text before the edit point did not move. A guard that only looked at the tail
        // would pass on a write that shifted the whole line.
        let l_after = at(&after, 'L', LINE_Y);
        assert!(
            (l_after - l_before).abs() < 0.05,
            "the head of the line moved {:.3} pt; nothing before the edit point may move",
            l_after - l_before
        );

        // (2) Every run after the edit point moved by exactly the measured growth.
        let b_after = at(&after, 'B', LINE_Y);
        assert!(
            (b_after - (b_before + growth)).abs() < 0.05,
            "the tail's ink moved {:.3} pt; the measured growth is {growth}",
            b_after - b_before
        );

        // (3) Nothing on the other line moved.
        let w_after = at(&after, 'W', OTHER_Y);
        assert!(
            (w_after - w_before).abs() < 0.05,
            "the untouched line moved {:.3} pt",
            w_after - w_before
        );

        // (3b) The same statement in run terms, over **every** run on the page, so a tail glyph
        // that happened to land right cannot stand in for the whole line. The fixture has two
        // runs on the edited baseline and one elsewhere, and all three are named.
        let reloaded = Document::load(&edited).expect("the edited file loads");
        let edited_page = page_id(&reloaded, 0).expect("page 0");
        let described = locate_runs(&reloaded, edited_page).expect("runs");
        let origin_after = |text: &str| -> f32 {
            let found: Vec<f32> = described
                .iter()
                .filter(|run| run.text == text)
                .map(|run| run.page_origin.0)
                .collect();
            assert_eq!(
                found.len(),
                1,
                "expected exactly one {text:?} after the write"
            );
            found[0]
        };
        assert!(
            (origin_after("ALPHAab ") - target.page_origin.0).abs() < 0.05,
            "the edited run's origin is {} after the write; it began at {}",
            origin_after("ALPHAab "),
            target.page_origin.0
        );
        let tail_before = runs
            .iter()
            .find(|run| run.text == "BETA")
            .expect("the tail before the edit")
            .page_origin
            .0;
        assert!(
            (origin_after("BETA") - (tail_before + growth)).abs() < 0.05,
            "the tail's run origin is {} after the write; {tail_before} + {growth} is {}",
            origin_after("BETA"),
            tail_before + growth
        );
        let wxyz_before = runs
            .iter()
            .find(|run| run.text == "WXYZ")
            .expect("the untouched line before the edit")
            .page_origin
            .0;
        assert!(
            (origin_after("WXYZ") - wxyz_before).abs() < 0.05,
            "the untouched line's run origin moved from {wxyz_before} to {}",
            origin_after("WXYZ")
        );

        // (4) The edited run reads back as the requested string, and the tail's reported origin
        // agrees with the position the record names — `moved_to` includes the kern (R10.3),
        // which is also where the reader puts the tail.
        let texts: Vec<&str> = described.iter().map(|run| run.text.as_str()).collect();
        assert!(
            texts.contains(&"ALPHAab "),
            "the edited run does not read back as `ALPHAab `: {texts:?}"
        );
        assert!(
            !texts.contains(&"ALPHA "),
            "the old text survived the write: {texts:?}"
        );
        let tail = described
            .iter()
            .find(|run| run.text == "BETA")
            .expect("the tail survived the write");
        assert!(
            (tail.origin.0 - applied.moved_to).abs() < 0.05,
            "the write reports its tail at {:.3}; the reader puts it at {:.3}",
            applied.moved_to,
            tail.origin.0
        );
        assert!(
            (tail.origin.0 - (line_origin + new_width + kern)).abs() < 0.05,
            "the tail's text-space origin is {:.3}; the measured prefix puts it at {:.3}",
            tail.origin.0,
            line_origin + new_width + kern
        );
    }

    #[test]
    fn a_replacement_past_the_limit_refuses_under_r4() {
        let source = fixture("kerned-line.pdf");
        let document = Document::load(&source).expect("fixture loads");
        let page = page_id(&document, 0).expect("page 0");
        let runs = locate_runs(&document, page).expect("runs");
        let target = runs.iter().find(|run| run.text == "AB").expect("AB");

        // The line already ends near 129; a limit below that refuses.
        let refused = reflow_tj_array(&document, 0, page, &runs, &target.id, "ABC", 100.0)
            .expect_err("no room on the line");
        assert_eq!(refused.rule, ReflowRefusal::R4Wrap);
    }

    /// Row A10's refusal case, on R-3's near-edge fixture.
    ///
    /// The row's refusal has to arrive through `fits-width` — the gate the user is judged by —
    /// and it has to arrive because *this page's content box* has no room, not because a caller
    /// handed in a limit. The document is the authority on its own box (§3), so this test names
    /// no limit at all.
    #[test]
    fn a_replacement_too_wide_for_the_content_box_refuses_and_writes_nothing() {
        const SIZE: f32 = 24.0;
        let width = |text: &str| {
            crate::editor::afm::text_width("Helvetica", text, SIZE)
                .unwrap_or_else(|| panic!("Helvetica covers {text:?}"))
        };

        let source = fixture("reflow-line-near-edge.pdf");
        let document = Document::load(&source).expect("the A10 near-edge fixture is committed");
        let page = page_id(&document, 0).expect("page 0");
        let runs = locate_runs(&document, page).expect("runs");
        let target = runs
            .iter()
            .find(|run| run.text == "ALPHA ")
            .expect("the A10 fixture's edited run");

        // The room this page has left, from the fixture itself: the content box's right edge
        // less the line's own right edge. Both numbers are read, not transcribed.
        let box_ = content_box(&document, page).expect("the fixture states a MediaBox");
        let line_right = runs
            .iter()
            .map(|run| run.page_origin.0 + run.advance)
            .fold(f32::NEG_INFINITY, f32::max);
        let room = box_.x1 - line_right;
        assert!(
            (room - 9.064).abs() < 0.05,
            "the near-edge fixture leaves {room:.3} pt, not the 9.064 the builder intends"
        );

        // Preconditions: the replacement must be wider than the room and must be a *wider*
        // replacement at all, or R1 refuses first and this test would be measuring R1.
        let growth = width("ALPHAabc ") - width("ALPHA ");
        assert!(
            growth > room,
            "the replacement grows the line by {growth:.3} pt and the room is {room:.3}; a \
             replacement that fits cannot exercise the refusal"
        );

        let stream_before = document.get_page_content(page);
        let refused =
            reflow_line_within_content_box(&document, 0, page, &runs, &target.id, "ALPHAabc ")
                .expect_err("a replacement wider than the content box has nowhere to go");

        let AppError::TextRunNotEditable(message) = &refused else {
            panic!("the content box's refusal must reach the user as a gate, got {refused:?}");
        };
        assert!(
            message.contains("fits-width"),
            "the refusal must name the gate the user is judged by: {message}"
        );
        // R3: the message keeps the gate's shape — both measured widths — with the *line's*
        // available width standing where the run's own advance used to.
        assert!(
            message.contains(&format!("needs {growth:.1}pt")),
            "the message must quote the width the replacement needs ({growth:.1}): {message}"
        );
        assert!(
            message.contains(&format!("available {room:.1}pt")),
            "the message must quote the room the line has left ({room:.1}): {message}"
        );

        assert_eq!(
            document.get_page_content(page),
            stream_before,
            "a refusal must write nothing: the content stream changed"
        );

        // Non-vacuity, in both directions.
        //
        // (a) The same request on the wide-margin fixture succeeds, so the refusal above is this
        // page's box talking and not the replacement. A guard that refused everything would pass
        // the assertions above without a content box existing at all.
        let roomy = fixture("reflow-line.pdf");
        let mut roomy_document = Document::load(&roomy).expect("the A10 fixture is committed");
        let roomy_page = page_id(&roomy_document, 0).expect("page 0");
        let roomy_runs = locate_runs(&roomy_document, roomy_page).expect("runs");
        let roomy_target = roomy_runs
            .iter()
            .find(|run| run.text == "ALPHA ")
            .expect("the edited run");
        let (content, applied) = reflow_line_within_content_box(
            &roomy_document,
            0,
            roomy_page,
            &roomy_runs,
            &roomy_target.id,
            "ALPHAabc ",
        )
        .expect("the same replacement fits a page with 389 pt to spare");
        assert!(
            (applied.shift - growth).abs() < 0.01,
            "the write reports a shift of {}; the measured growth is {growth}",
            applied.shift
        );

        // (b) A write *does* change the stream, so the equality above is not a statement about a
        // function that never writes. Without this, the byte check would pass on a refusal that
        // half-wrote and then returned an error.
        write_level(&mut roomy_document, roomy_page, &[], &content).expect("write page");
        assert_ne!(
            roomy_document.get_page_content(roomy_page),
            Document::load(&roomy)
                .expect("the fixture reloads")
                .get_page_content(roomy_page),
            "the positive control must actually change the stream"
        );
    }

    /// A one-page document whose page dictionary carries the given box attributes.
    ///
    /// `page` entries land on the page object, `inherited` ones on the `/Pages` node above it,
    /// which is how a real producer writes a document-wide box. Both are needed: the resolver's
    /// whole job is to tell "on this page" from "inherited", and a builder that could only write
    /// one of them would leave half the chain untested.
    fn boxed_page(
        page: &[(&str, Vec<lopdf::Object>)],
        inherited: &[(&str, Vec<lopdf::Object>)],
    ) -> Document {
        let mut document = Document::with_version("1.4");
        let pages_id = document.new_object_id();
        // The box is the subject here, so the font only has to be a real one: a Type1
        // Helvetica with WinAnsi is what the fixtures use.
        let font_id = document.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "Helvetica",
            "Encoding" => "WinAnsiEncoding",
        });
        let resources_id = document.add_object(dictionary! {
            "Font" => dictionary! { "F1" => font_id },
        });
        let content_id = document.add_object(Stream::new(
            dictionary! {},
            Content {
                operations: Vec::new(),
            }
            .encode()
            .unwrap(),
        ));

        let mut page_dict = dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "Contents" => content_id,
            "Resources" => resources_id,
        };
        for (key, value) in page {
            page_dict.set(key.to_string(), Object::Array(value.clone()));
        }
        let page_id = document.add_object(page_dict);

        let mut pages_dict = dictionary! {
            "Type" => "Pages",
            "Kids" => vec![Object::Reference(page_id)],
            "Count" => 1,
        };
        for (key, value) in inherited {
            pages_dict.set(key.to_string(), Object::Array(value.clone()));
        }
        document
            .objects
            .insert(pages_id, Object::Dictionary(pages_dict));

        let catalog_id = document.add_object(dictionary! {
            "Type" => "Catalog",
            "Pages" => pages_id,
        });
        document.trailer.set("Root", catalog_id);
        document.renumber_objects();
        document
    }

    fn rect(values: [i64; 4]) -> Vec<lopdf::Object> {
        values.iter().map(|value| Object::Integer(*value)).collect()
    }

    /// §3.1–§3.2: which box is the content box, in which order, through whose inheritance.
    ///
    /// Four properties, each of which is a way the wrong box has been chosen before:
    ///
    /// 1. `/CropBox` wins when the page states both;
    /// 2. `/CropBox`'s absence falls back to `/MediaBox` (the default Table 30 names);
    /// 3. both are resolved up the `/Parent` chain, so a document-wide box is not "absent";
    /// 4. a box with swapped corners is normalised, or every edit on the page refuses.
    ///
    /// (3) is §3.3's reuse hazard made observable: `export.rs`'s `media_box` does not walk the
    /// chain, so on an inherited box it returns US Letter's 612 whatever the page is. A resolver
    /// that reused it would answer 612 to every question below.
    #[test]
    fn the_content_box_is_the_crop_box_defaulting_to_the_media_box_through_the_page_tree() {
        let page_of = |document: &Document| page_id(document, 0).expect("page 0");

        // (1) CropBox wins.
        let both = boxed_page(
            &[
                ("MediaBox", rect([0, 0, 612, 792])),
                ("CropBox", rect([10, 20, 300, 400])),
            ],
            &[],
        );
        assert_eq!(
            content_box(&both, page_of(&both)).expect("both boxes stated"),
            PdfRect {
                x0: 10.0,
                y0: 20.0,
                x1: 300.0,
                y1: 400.0
            }
        );

        // (2) Absent CropBox defaults to the MediaBox.
        let media_only = boxed_page(&[("MediaBox", rect([0, 0, 595, 842]))], &[]);
        assert_eq!(
            content_box(&media_only, page_of(&media_only)).expect("MediaBox states the box"),
            PdfRect {
                x0: 0.0,
                y0: 0.0,
                x1: 595.0,
                y1: 842.0
            },
            "an A4 page must report A4, not a Letter fallback"
        );

        // (3) Both are inheritable. A page that states neither takes the /Pages node's box —
        // and for the CropBox specifically, so the chain is walked once per attribute rather
        // than once in total.
        let inherited_media = boxed_page(&[], &[("MediaBox", rect([0, 0, 595, 842]))]);
        assert_eq!(
            content_box(&inherited_media, page_of(&inherited_media)).expect("inherited MediaBox"),
            PdfRect {
                x0: 0.0,
                y0: 0.0,
                x1: 595.0,
                y1: 842.0
            },
            "an inherited box is not an absent one"
        );
        let inherited_crop = boxed_page(
            &[("MediaBox", rect([0, 0, 612, 792]))],
            &[("CropBox", rect([36, 36, 576, 756]))],
        );
        assert_eq!(
            content_box(&inherited_crop, page_of(&inherited_crop)).expect("inherited CropBox"),
            PdfRect {
                x0: 36.0,
                y0: 36.0,
                x1: 576.0,
                y1: 756.0
            },
            "an inherited CropBox must beat the page's own MediaBox"
        );

        // (4) Normalisation: the same rectangle with its corners swapped.
        let swapped = boxed_page(&[("MediaBox", rect([612, 792, 0, 0]))], &[]);
        assert_eq!(
            content_box(&swapped, page_of(&swapped)).expect("swapped corners"),
            PdfRect {
                x0: 0.0,
                y0: 0.0,
                x1: 612.0,
                y1: 792.0
            },
            "a reversed box must be normalised, or every edit on the page reports no room"
        );
    }

    /// §3.2 step 3 and its neighbour: the resolver refuses rather than inventing a box.
    ///
    /// Two shapes, and they are different failures:
    ///
    /// * **no box anywhere** — a malformed page. Inventing Letter here would turn a malformed
    ///   file into a wrong reflow decision;
    /// * **a box that is present but unreadable** — and this one is the trap. Falling through to
    ///   the next attribute would substitute the `/MediaBox` for a broken `/CropBox`, silently
    ///   widening the region text may enter. The assertion is that it is an error, not that the
    ///   MediaBox came back, because the MediaBox *would* come back if the fall-through were
    ///   written the obvious way.
    #[test]
    fn a_page_without_a_readable_box_refuses_instead_of_inventing_one() {
        let absent = boxed_page(&[], &[]);
        let error = content_box(&absent, page_id(&absent, 0).expect("page 0"))
            .expect_err("a page with no box anywhere is malformed, not Letter");
        assert!(
            error
                .to_string()
                .contains("neither a CropBox nor a MediaBox"),
            "the refusal must say which attributes it looked for: {error}"
        );

        let mut broken = boxed_page(&[("MediaBox", rect([0, 0, 612, 792]))], &[]);
        let page = page_id(&broken, 0).expect("page 0");
        broken
            .get_object_mut(page)
            .expect("page object")
            .as_dict_mut()
            .expect("page dictionary")
            .set("CropBox", Object::Array(vec![Object::Integer(1)]));
        let error = content_box(&broken, page)
            .expect_err("a malformed CropBox must not fall through to the MediaBox");
        assert!(
            error.to_string().contains("CropBox is not four numbers"),
            "the refusal must name the attribute that is malformed: {error}"
        );
    }
}
