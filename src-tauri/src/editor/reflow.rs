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
//!   transform (R6), no structure-boundary crossing (R8).
//!
//! Refusing here is not a stub: each refusal names the rule, because a guard must be able to
//! tell a refusal from a wrong answer, and silent skips are banned in this repository.

use lopdf::{content::Content, content::Operation, Document, Object, ObjectId};

use crate::editor::substitute::FontMetrics;
use crate::editor::textlocate::{decoded_content, RawRun, RunId, XObjectStep};

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
}

impl ReflowRefused {
    fn new(rule: ReflowRefusal, run: &RunId) -> Self {
        Self {
            rule,
            run: run.clone(),
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
fn encode(operations: &[Operation]) -> Vec<u8> {
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

    // R8 as written is superseded (docs/P2.8-reflow-DESIGN.md §4, R8b): a single-array write
    // cannot cross an `MCID` because that boundary is a separate operator. R8b is NOT implemented
    // here yet and is the next task: `on_baseline` filters on `baseline()` alone, which carries no
    // tag term, so a same-baseline run in another `MCID` and further left still corrupts the
    // `line_origin` that `Td` is computed from. Do not read this comment as a check that passed.

    // R6: a sheared or anisotropically scaled matrix makes "the rest of the line" a
    // non-horizontal concept. `page_scale` cannot answer this — it reports the advance axis only,
    // and a shear keeps a usable advance length — so the test is orthogonality itself.
    if !run.frame.is_orthogonal() {
        return Err(ReflowRefused::new(
            ReflowRefusal::R6NonUniformTransform,
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

    let old_width =
        measure(&run.text).ok_or_else(|| ReflowRefused::new(ReflowRefusal::NotOneArray, target))?;
    let new_width = measure(replacement)
        .ok_or_else(|| ReflowRefused::new(ReflowRefusal::NotOneArray, target))?;

    // R1: an equal-or-narrower replacement moves nothing, and moving nothing is not reflow.
    if new_width <= old_width {
        return Err(ReflowRefused::new(ReflowRefusal::R1NoMove, target));
    }

    // R4: the widened line must still fit. If its right edge would pass `limit_x` there is
    // nowhere on this baseline to go, so refuse rather than wrap (R4 is out of scope).
    let growth = new_width - old_width;
    let line_right = on_baseline
        .iter()
        .map(|candidate| candidate.page_origin.0 + candidate.advance)
        .fold(f32::NEG_INFINITY, f32::max);
    if line_right + growth > limit_x + f32::EPSILON {
        return Err(ReflowRefused::new(ReflowRefusal::R4Wrap, target));
    }

    // R2.1: the trailing run's new origin is **line origin + measured prefix**. The prefix is
    // everything on the line before the trailing run, re-measured — here, the widened target's
    // own advance, because the target is what grew and it sits immediately before the tail.
    //
    // `run.origin.0` is the target's own pen origin, which is the line origin when the target is
    // the first fragment of its array. Any earlier run on the same baseline would be a separate
    // operation, and R5/R8 do not forbid one, so the line origin is taken from the leftmost run
    // on the baseline, not from the target.
    let line_origin = on_baseline
        .iter()
        .map(|candidate| candidate.origin.0)
        .fold(f32::INFINITY, f32::min);
    let prefix_before_target = run.origin.0 - line_origin;
    let prefix = prefix_before_target + new_width;
    let offset = td_offset(line_origin, prefix);
    let moved_to = line_origin + prefix;
    let shift = moved_to - (run.origin.0 + old_width);

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
    use crate::editor::textlocate::{locate_runs, page_id, write_level};
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
}
