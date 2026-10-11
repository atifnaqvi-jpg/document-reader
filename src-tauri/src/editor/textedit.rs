//! Stage-1 constrained in-place text editing (P2.4 T2/T3/T4/T5).
//!
//! The pipeline is fixed by the design and each step has one owner:
//!
//! ```text
//! decode (T3e-bounded) → locate run → gate (T3) → mutate operands → re-encode
//!   → replace stream object → caller persists via editor::save (D2)
//! ```
//!
//! Three rules shape the code more than anything else:
//!
//! * **lopdf is the only writer** (T1/Q3). Nothing here calls PDFium. Width metrics come
//!   in as numbers the caller measured with PDFium, read-only; the mutation and the
//!   bytes written are pure lopdf.
//! * **A refusal names the gate** (T3/Q2). Every gate failure is
//!   [`AppError::TextRunNotEditable`] carrying a message that says which gate and, for
//!   the width gate, both measured widths. A refusal the user cannot act on is
//!   indistinguishable from a bug.
//! * **Stage 1 only** (T11). No font embedding, no subsetting, no reflow. When a run
//!   needs any of those, it is refused.

use lopdf::Document;

use crate::editor::textlocate::{
    self, is_cid_font, is_simple_font, page_id, ContentOperation, FontInfo, RawRun, RunId,
};
use crate::model::AppError;

/// The Stage-1 admissibility gates (T3), each naming the reason it can refuse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gate {
    /// (a) simple font — Type1/TrueType, not CID/Identity-H.
    SimpleFont,
    /// (b) `encode_text(decode_text(bytes)) == bytes` for the run.
    EncodingRoundTrip,
    /// (c) the font is not subset-flagged (`ABCDEF+`, partial glyphs).
    NotSubset,
    /// (d) the replacement fits the run's advance width at its font size.
    FitsWidth,
    /// (f) the content-stream run no longer matches the run the user picked (T4).
    RunMismatch,
}

impl Gate {
    /// Stable identifier used in messages and the frontend, so a refusal can be
    /// asserted on and, later, translated without matching prose.
    pub fn name(self) -> &'static str {
        match self {
            Gate::SimpleFont => "simple-font",
            Gate::EncodingRoundTrip => "encoding-round-trip",
            Gate::NotSubset => "not-subset",
            Gate::FitsWidth => "fits-width",
            Gate::RunMismatch => "run-mismatch",
        }
    }

    /// A short, calm sentence naming the gate and the way out.
    pub fn message(self) -> &'static str {
        match self {
            Gate::SimpleFont => {
                "this run uses a CID/Type0 font, which Stage 1 cannot rewrite; \
                 add a text annotation instead"
            }
            Gate::EncodingRoundTrip => {
                "this run's font encoding cannot reproduce its own bytes, so an edit \
                 could corrupt it; add a text annotation instead"
            }
            Gate::NotSubset => {
                "this run's font is an embedded subset with only the glyphs the \
                 original used; add a text annotation instead"
            }
            Gate::FitsWidth => "the replacement is wider than the space available",
            Gate::RunMismatch => {
                "this text is not addressable in the page's content stream (or it changed \
                 since it was selected); add a text annotation instead"
            }
        }
    }
}

fn refuse(gate: Gate) -> AppError {
    AppError::TextRunNotEditable(format!("{} (gate: {})", gate.message(), gate.name()))
}

/// A run's Stage-1 admissibility, precomputed for the picker so the UI can grey out
/// runs the write path will refuse (P2.4 §4: "refusal reasons are pre-computed, not
/// discovered after typing").
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Admissibility {
    pub editable: bool,
    /// Name of the first violated gate, `None` when editable. The frontend shows
    /// `message` and keys off `gate`.
    pub gate: Option<String>,
    pub message: Option<String>,
}

impl Admissibility {
    fn editable() -> Self {
        Admissibility {
            editable: true,
            gate: None,
            message: None,
        }
    }

    fn refused(gate: Gate) -> Self {
        Admissibility {
            editable: false,
            gate: Some(gate.name().to_string()),
            message: Some(gate.message().to_string()),
        }
    }

    /// A refusal whose prose is the caller's rather than the gate's stock sentence.
    ///
    /// T21 needs this: an unpaired run is refused `run-mismatch`, but the *reason* the user
    /// must see is that no rendered box was found — an unpaired run and a changed run share a
    /// gate while needing different explanations.
    fn refused_with(gate: Gate, message: &str) -> Self {
        Admissibility {
            editable: false,
            gate: Some(gate.name().to_string()),
            message: Some(message.to_string()),
        }
    }
}

/// The font facts that gate (a), (b) and (c) depend on, resolved for one run.
///
/// Kept separate from [`Admissibility`] so the same checks run both when the picker
/// asks (to grey a run out) and immediately before a write (to re-validate), with no
/// chance of the two drifting.
#[derive(Debug, Clone)]
pub struct RunGates {
    pub font: FontInfo,
    /// True when `encode_text(decode_text(bytes)) == bytes` for the run's bytes.
    pub round_trips: bool,
}

/// Resolves the font gates for a run.
///
/// An unresolvable font is not an error here: it is simply not editable, and the
/// caller reports that as a gate refusal. Only a genuinely unreadable document (no
/// page, no content) surfaces as an error.
pub fn resolve_gates(document: &Document, page: u32, run: &RawRun) -> Result<RunGates, AppError> {
    let page_id = page_id(document, page)?;
    let font = textlocate::font_info(document, page_id, &run.font_key).ok_or_else(|| {
        AppError::TextRunNotEditable(format!(
            "this run's font ({}) could not be resolved; add a text annotation instead (gate: {})",
            String::from_utf8_lossy(&run.font_key),
            Gate::SimpleFont.name()
        ))
    })?;

    let fonts = document
        .get_page_fonts(page_id)
        .map_err(|error| AppError::Other(format!("could not read page fonts: {error}")))?;
    let round_trips = fonts
        .get(&run.font_key)
        .copied()
        .and_then(|font| font.get_font_encoding(document).ok())
        .map(|encoding| {
            let text = encoding.bytes_to_string(&run.bytes).unwrap_or_default();
            encoding.string_to_bytes(&text) == run.bytes
        })
        .unwrap_or(false);

    Ok(RunGates { font, round_trips })
}

/// The a/b/c verdict for a run, independent of the width gate (which needs PDFium and
/// is folded in by the caller).
///
/// Stage 2 (T14) keeps the SimpleFont and EncodingRoundTrip refusals exactly as Stage 1
/// had them, and **stops refusing a subset font**: a subset is now the *trigger* for
/// MECH-A substitution, whose keep-original/substitute decision needs the replacement text
/// (T11) and therefore cannot be made here. The picker therefore offers a subset run as
/// editable and the write path resolves the font; a replacement the original cannot set and
/// no substitute can cover still refuses, with `GLYPHS_NOT_COVERABLE` or
/// `SUBSTITUTE_REFUSED`.
pub fn font_admissibility(gates: &RunGates) -> Admissibility {
    if !is_simple_font(&gates.font) {
        return Admissibility::refused(Gate::SimpleFont);
    }
    if is_cid_font(&gates.font) {
        return Admissibility::refused(Gate::SimpleFont);
    }
    if !gates.round_trips {
        return Admissibility::refused(Gate::EncodingRoundTrip);
    }
    Admissibility::editable()
}

/// Measures a replacement string in PDF points, as the caller measured it with PDFium.
///
/// A trait object rather than a plain `f32` so the width gate cannot be satisfied by a
/// stale number: the caller must have measured *this* string (T3d), and the tests can
/// inject a deterministic metric without PDFium.
pub type WidthMeasure<'a> = &'a dyn Fn(&str) -> Result<f32, AppError>;

/// Checks gate (d): the replacement must fit the run's advance width.
///
/// The tolerance is `old_width + max(0.5pt, 1% of old_width)` (O1, confirmed): metric
/// rounding across font programs is real, and refusing on a tenth of a point would make
/// the feature feel broken. Anything beyond that is a hard refusal that reports **both**
/// measured widths, because "it does not fit" without the numbers is not actionable.
pub fn width_refusal(old_width: f32, new_width: f32) -> Option<AppError> {
    let tolerance = 0.5_f32.max(old_width.abs() * 0.01);
    if new_width > old_width + tolerance {
        return Some(AppError::TextRunNotEditable(format!(
            "the replacement needs {new_width:.1}pt, available {old_width:.1}pt; \
             add a text annotation instead (gate: {})",
            Gate::FitsWidth.name()
        )));
    }
    None
}

/// One run as the picker presents it (P2.4 §4): the address the write path will use, the
/// text the user sees, where it sits on the page, and whether Stage 1 will accept it.
///
/// PDFium supplies `rect`/`font_name`/`font_size`/`width` — display and measurement only.
/// The **address** (`run_id`) and the admissibility verdict come from lopdf, because those
/// are the things the write path depends on and PDFium's reconstruction must never be
/// mistaken for them (T4).
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PageTextRun {
    pub run_id: RunId,
    /// The decoded text, shown to the user and re-validated at write time. For a merged run
    /// this is the **joined** fragment text; the per-fragment text lives in `fragments`.
    pub text: String,
    /// The showing operator (`Tj`, `TJ`, `'`, `"`), for diagnostics. For a merged run, the
    /// head fragment's operator.
    pub operator: String,
    /// Resource name of the font (`F1`).
    pub font_key: String,
    /// Font facts for the refusal message.
    pub font_name: String,
    pub font_size: f32,
    /// Bounding box in PDF points, origin bottom-left; `None` when PDFium could not match
    /// this run, in which case the UI must not offer it.
    pub rect: Option<crate::editor::geometry::PdfRect>,
    /// Measured advance width in points (PDFium), the `old_width` the width gate uses.
    ///
    /// P2.4.2 T22: this is the run's **own** measured span, never an inherited box width. A
    /// paired box supplies it only when the pair is strong *and* the box is no wider than
    /// `span × BOX_SPAN_SLACK`; otherwise the span wins and the box is flagged
    /// [`Self::box_suspicious`]. `None` when nothing is measurable — the run is then refused by
    /// the width gate rather than trusted to a possibly line-sized box.
    pub width: Option<f32>,
    /// True when the run's paired box is far wider than its own span (P2.4.2 T22).
    ///
    /// The 0.4.1 `ence` orphan paired to a line-sized box. Such a pair is flagged, never
    /// merged, and never trusted for its box width. When the span is measurable the run stays
    /// editable (the span is a good quota); when it is not, the group refuses.
    #[serde(default)]
    pub box_suspicious: bool,
    /// The precomputed Stage-1 verdict. `editable` is false for a gate refusal *or* for a
    /// run with no PDFium geometry (nothing to click, nothing to measure).
    pub admissibility: Admissibility,
    /// How the run paired with a rendered box (P2.4.1 T17/T21). `None` means no plausible
    /// box: the run was refused `run-mismatch` naming that reason.
    pub pair_confidence: Option<PairConfidence>,
    /// How many content-stream fragments this run joins (P2.4.1 T18). `1` for an ordinary
    /// single-operand run, `> 1` for a merged run.
    pub fragment_count: usize,
    /// The individual fragments of a merged run, head first, each described exactly as a
    /// single run (P2.4.1 O-Pair-3: the picker expands to this second level). Empty for an
    /// ordinary run, whose own fields already describe its single operand.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fragments: Vec<PageTextRun>,
}

impl PageTextRun {
    /// True for a run the picker may offer as an edit target.
    pub fn editable(&self) -> bool {
        self.admissibility.editable
    }
}

/// How confidently a lopdf run was matched to a rendered box (P2.4.1 §3.3).
///
/// * **Strong** — the origin lies inside the box (within a font-size-relative tolerance) and
///   the box's baseline agrees within 0.5pt.
/// * **Weak** — nearest-only geometry: either the box does not contain the origin or its
///   baseline disagrees. The pair is still offered (geometry is the point), but flagged so
///   the write path knows it is a lower-confidence match.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PairConfidence {
    Strong,
    Weak,
}

/// The tolerance for the "origin lies inside the box" test, in points.
///
/// **Relative to the font size**, never an absolute constant (P2.4.1 T19's rule, applied to
/// pairing too): one page carries 9pt body text and 24pt headings, and a fixed half-point
/// would be far too tight for one and too loose for the other. 0.25em also covers the left
/// side bearing that puts a box's ink to the *right* of the pen origin (measured ~0.08em).
fn origin_tolerance(font_size: f32) -> f32 {
    (0.25 * font_size).max(0.5)
}

/// The baseline agreement required to call a pair strong, in points (P2.4.1 T19/§3.3).
const BASELINE_TOLERANCE: f32 = 0.5;

/// The largest horizontal gap two fragments may have and still merge, as a fraction of the
/// font size (P2.4.1 T19).
///
/// Relative, never an absolute constant: the same PDF carries 9pt body text and 24pt
/// headings, and a fixed quarter-point would either split words in the headings or weld
/// separate words together in the body. A quarter of an em is comfortably tighter than the
/// space glyph (≈0.28em in Helvetica) and comfortably looser than the left side bearing plus a
/// kerning pair.
const MERGE_GAP_EM: f32 = 0.25;

/// The widened gap two fragments may have and still merge **when PDFium's own text shows a
/// real space at that boundary** (P2.4.2 O-Chain-2).
///
/// Word and PowerPoint exports often carry an inter-word space as a *kerning shift* rather
/// than a space glyph: the bytes read `foo` then `bar` with no `0x20` between them, while the
/// rendered line plainly reads `foo bar`. PDFium reconstructs that gap as a space; when it
/// does, the two fragments are one word-space apart and a half-em is the honest bound. When it
/// does not, the quarter-em bound above still applies, so a genuinely large kern is not welded
/// into one run.
const MERGE_GAP_SPACE_EM: f32 = 0.5;

/// The overlap past which two fragments are *not* the same chain, as a fraction of the font
/// size (P2.4.2 O-Chain-2's overlap guard).
///
/// Two fragments on one baseline should never overlap by more than a hair of side bearing.
/// A negative gap beyond this means the run's measured advance overshot its ink — a
/// mis-measure rather than a real adjacency — so the chain breaks there instead of joining two
/// runs whose written bytes would step on each other.
const MERGE_OVERLAP_EM: f32 = 0.05;

/// The size-equality tolerance, in points, for two fragments on one baseline (P2.4.2).
const SIZE_TOLERANCE_PT: f32 = 0.2;

/// The size-equality tolerance as a fraction of the size — the larger of the two rules applies
/// (P2.4.2: `|Δ| ≤ max(0.2pt, 1% of size)`).
const SIZE_TOLERANCE_FRACTION: f32 = 0.01;

/// The largest a paired box may be, relative to the run's own measured span, and still be
/// trusted as the width-gate old width (P2.4.2 T22).
///
/// A box far wider than the span it supposedly measures is a *line* box that a small run
/// claimed — the 0.4.1 `ence` orphan, whose ~14pt tail paired to a ~300pt line and passed a
/// ~110pt replacement. `1.25` leaves room for the ordinary ink/advance difference (a box is
/// ink, an advance is a pen move) while refusing anything line-sized.
const BOX_SPAN_SLACK: f32 = 1.25;

/// The refusal for a run whose width cannot be measured at all (P2.4.2 T22).
///
/// It is a **width-gate** refusal — the gate the user is about to be judged by — so it names
/// `fits-width`, but its reason is the measurement, not the overflow: the font carries no
/// in-file metrics, so the only width on offer is a box that may be a whole line. Trusting it
/// is exactly the 0.4.1 bug, so the safe direction is to refuse and point at the annotation.
const UNMEASURABLE_WIDTH_MESSAGE: &str = "this run's font carries no width metrics to measure \
     the replacement against; add a text annotation instead (gate: fits-width)";

/// Whether two `Tf` sizes are the same fragment's (P2.4.2, relative rule).
///
/// The flat 0.1pt rule of P2.4.1 broke chains over rendering-scale rounding on a large face:
/// a 48pt run measured `48.0` in one operand and `48.05` in the next is the same run, and a
/// tenth of a point would split it. One percent of the size (or a fifth of a point, whichever
/// is larger) tracks the eye instead of the fourth decimal.
fn sizes_match(a: f32, b: f32) -> bool {
    let tolerance = SIZE_TOLERANCE_PT.max(SIZE_TOLERANCE_FRACTION * a.abs().max(b.abs()));
    (a - b).abs() <= tolerance
}

/// The gap below which two fragments on one baseline are joined (P2.4.1 T19 / P2.4.2
/// O-Chain-2).
fn gap_tolerance(font_size: f32, space_at_boundary: bool) -> f32 {
    let em = if space_at_boundary {
        MERGE_GAP_SPACE_EM
    } else {
        MERGE_GAP_EM
    };
    em * font_size
}

/// One lopdf run's match to a rendered box.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Pair {
    rect: usize,
    confidence: PairConfidence,
}

/// Matches one lopdf run to a PDFium box by **geometry** (P2.4.1 T17).
///
/// Text equality is demoted to a tiebreaker (P2.4.1 T17): the two libraries segment text
/// differently — PDFium joins a per-glyph `TJ` array into one run, lopdf splits it into one
/// run per operand — so matching on the reconstructed string fails for exactly the documents
/// this hotfix exists for. Position on the page does not care how either side split the line.
fn pair_run(run: &RawRun, rects: &[crate::editor::runmetrics::PdfiumRun]) -> Option<Pair> {
    let tol = origin_tolerance(run.font_size);

    let mut best: Option<Pair> = None;
    let mut best_key: Option<(bool, f32, f32, bool)> = None;
    for (index, rect) in rects.iter().enumerate() {
        // `contains` uses the box grown by the tolerance, which absorbs the ink offset and a
        // small metric disagreement without reaching a neighbouring line.
        let contains = run.origin.0 >= rect.x0 - tol
            && run.origin.0 <= rect.x1 + tol
            && run.origin.1 >= rect.y0 - tol
            && run.origin.1 <= rect.y1 + tol;
        if !contains {
            continue;
        }
        let baseline_delta = (run.origin.1 - rect.y0).abs();
        let strong = baseline_delta <= BASELINE_TOLERANCE;
        // Prefer strong over weak, then the smallest baseline delta, then the leftmost box —
        // and, per §3 step 2, exact text equality as the *final* tiebreaker when two boxes
        // agree on all of it. Text is only ever a tiebreaker here: it never decides a pair the
        // geometry already separated, which is the whole point of T17.
        let key = (!strong, baseline_delta, rect.x0, run.text != rect.text);
        if best_key.map_or(true, |current| key < current) {
            best_key = Some(key);
            best = Some(Pair {
                rect: index,
                confidence: if strong {
                    PairConfidence::Strong
                } else {
                    PairConfidence::Weak
                },
            });
        }
    }
    if best.is_some() {
        return best;
    }

    // Nothing contains the origin. Fall back to the nearest box, but only when it is
    // *plausibly* the one this run belongs to — a run with no box anywhere near it is the
    // unpaired case T21 names, and guessing would put the run's text on the wrong line.
    let mut nearest: Option<(f32, usize, f32)> = None;
    for (index, rect) in rects.iter().enumerate() {
        let dx = (rect.x0 - run.origin.0)
            .max(run.origin.0 - rect.x1)
            .max(0.0);
        let dy = (rect.y0 - run.origin.1)
            .max(run.origin.1 - rect.y1)
            .max(0.0);
        let distance = dx + dy;
        if nearest.map_or(true, |(best, _, _)| distance < best) {
            nearest = Some((distance, index, rect.x0));
        }
    }
    nearest
        .filter(|(distance, _, _)| *distance <= 2.0 * tol)
        .map(|(_, rect, _)| Pair {
            rect,
            confidence: PairConfidence::Weak,
        })
}

/// True when the reconstructed text of two fragments shows a real space at their seam.
///
/// PDFium joins the operands of a kerned word into its box (T17), so when the two fragments
/// came from **different** PDFium boxes the seam is genuinely a break, not a kern; only a
/// shared box can hide a space. When they share a box, that box's own text is searched for a
/// space — the one signal that turns the widened `0.5em` gap on (P2.4.2 O-Chain-2). The space is
/// looked for as PDFium reports text, not as the operands read, because a producer that emitted
/// a space as kerning has no `0x20` byte to find.
fn space_at_boundary(
    head_index: usize,
    next_index: usize,
    pairs: &[Option<Pair>],
    rects: &[crate::editor::runmetrics::PdfiumRun],
) -> bool {
    let (Some(head_pair), Some(next_pair)) = (pairs[head_index], pairs[next_index]) else {
        return false;
    };
    if head_pair.rect != next_pair.rect {
        return false;
    }
    rects
        .get(head_pair.rect)
        .is_some_and(|rect| rect.text.contains(' '))
}

/// Whether two consecutive fragments may be merged into one run (P2.4.1 T19 / P2.4.2).
///
/// All conditions are required: same font, a size that agrees within the *relative* tolerance
/// ([`sizes_match`]), the same baseline (0.5pt), and a horizontal gap that is
///
/// * at most `0.5em` when PDFium's reconstructed text carries a real space at the seam, else
///   at most `0.25em` ([`gap_tolerance`]); and
/// * no more overlapping than `-0.05em` ([`MERGE_OVERLAP_EM`]) — a larger overlap is a
///   mis-measure, and joining across it would let two fragments' bytes collide.
///
/// The gap is measured from the fragments' own **exact in-file advances** (`advance_exact`,
/// P2.4.2 Phase A) when both have them, falling back to the approximation only when a font
/// offers no metric at all. That is the chain-break fix: the 0.4.1 approximation inflated the
/// head's advance and pushed the seam negative.
fn mergeable(head: &RawRun, next: &RawRun, space: bool) -> bool {
    head.font_key == next.font_key
        && sizes_match(head.font_size, next.font_size)
        && (head.origin.1 - next.origin.1).abs() <= BASELINE_TOLERANCE
        && {
            let advance = head.advance_exact.unwrap_or(head.advance);
            let gap = next.origin.0 - (head.origin.0 + advance);
            gap <= gap_tolerance(head.font_size, space) && gap >= -MERGE_OVERLAP_EM * head.font_size
        }
}

/// A group of consecutive fragments the picker presents as one run (P2.4.1 T18).
struct RunGroup {
    first: usize,
    len: usize,
}

/// Groups consecutive paired fragments into merged runs (P2.4.1 T18/T19 / P2.4.2 T22).
///
/// A group starts at any paired run and extends greedily while the next run is paired with
/// **strong** confidence and satisfies [`mergeable`]. Weak pairs are deliberately left on
/// their own: they are the pairs whose geometry is least certain, and folding two of them
/// into one editable run would make the merged width — the number the width gate refuses on —
/// more speculative than the single-run number it replaces.
///
/// The scan is ordered and strictly consecutive: a run that is not on the same baseline, or a
/// run with no box at all, ends the group. That is what keeps a merge from reaching across a
/// line break or past an address the picker refused.
fn group_runs(
    runs: &[RawRun],
    pairs: &[Option<Pair>],
    rects: &[crate::editor::runmetrics::PdfiumRun],
) -> Vec<RunGroup> {
    let mut groups = Vec::new();
    let mut index = 0;
    while index < runs.len() {
        // An unpaired run is still *reported* — the picker must show that it exists and name
        // why it cannot be edited (T21) — it simply cannot merge with anything.
        let mut len = 1;
        if pairs[index].is_some() {
            while index + len < runs.len() {
                let next = pairs[index + len];
                if next.map(|pair| pair.confidence) != Some(PairConfidence::Strong) {
                    break;
                }
                // O-Chain-2: the widened gap is allowed only when PDFium's reconstructed text
                // has a real space at this seam. A space can only hide inside a *shared* box.
                let space = space_at_boundary(index + len - 1, index + len, pairs, rects);
                if !mergeable(&runs[index + len - 1], &runs[index + len], space) {
                    break;
                }
                len += 1;
            }
        }
        groups.push(RunGroup { first: index, len });
        index += len;
    }
    groups
}

/// The run's own measured span in points, or `None` when its font carries no in-file metrics
/// (P2.4.2 T22).
///
/// The span is measured with the same Stage-2 machinery the walker uses: AFM for a standard-14
/// font, `hmtx` via `ttf-parser` for an embedded program. It is never a box width — that is the
/// whole point of T22 — and never an approximation, because an approximation is exactly what
/// opened the 0.4.1 hole.
fn measured_span(document: &Document, page: u32, run: &RawRun) -> Option<f32> {
    let page_id = page_id(document, page).ok()?;
    let metrics = crate::editor::substitute::FontMetrics::resolve(document, page_id, &run.font_key);
    metrics.advance(&run.text, run.font_size)
}

/// The group's own measured span in points, or `None` when no fragment's span is measurable
/// (P2.4.2 T22).
///
/// A group is a set of fragments laid end to end, so its span is the sum of their spans (their
/// own advances, not the box they happen to share) — for the ordinary single-fragment case this
/// is just that fragment's span. `None` when the group has non-empty text none of which is
/// measurable (a font with no in-file metrics), so the caller refuses rather than reaching for
/// the box.
fn group_span(document: &Document, page: u32, runs: &[RawRun], group: &RunGroup) -> Option<f32> {
    let mut span = 0.0_f32;
    let mut measurable = false;
    for offset in 0..group.len {
        let run = &runs[group.first + offset];
        // An emptied tail fragment (a cleared operand) contributes nothing and needs no metric;
        // its bytes may be unmapped once the head carries the whole replacement.
        if run.text.is_empty() {
            continue;
        }
        span += measured_span(document, page, run)?;
        measurable = true;
    }
    if measurable {
        Some(span)
    } else {
        None
    }
}

/// The union of a group's rendered boxes.
fn union_rect(
    group: &RunGroup,
    pairs: &[Option<Pair>],
    rects: &[crate::editor::runmetrics::PdfiumRun],
) -> Option<crate::editor::geometry::PdfRect> {
    let mut union: Option<crate::editor::geometry::PdfRect> = None;
    for offset in 0..group.len {
        let pair = pairs[group.first + offset]?;
        let rect = rects.get(pair.rect)?;
        let candidate = crate::editor::geometry::PdfRect {
            x0: rect.x0,
            y0: rect.y0,
            x1: rect.x1,
            y1: rect.y1,
        };
        union = Some(match union {
            None => candidate,
            Some(current) => crate::editor::geometry::PdfRect {
                x0: current.x0.min(candidate.x0),
                y0: current.y0.min(candidate.y0),
                x1: current.x1.max(candidate.x1),
                y1: current.y1.max(candidate.y1),
            },
        });
    }
    union
}

/// Describes one run or merged group (P2.4.1 T18/T20 / P2.4.2 T22).
fn describe_group(
    document: &Document,
    page: u32,
    runs: &[RawRun],
    pairs: &[Option<Pair>],
    rects: &[crate::editor::runmetrics::PdfiumRun],
    group: &RunGroup,
) -> Result<PageTextRun, AppError> {
    // T20: every fragment is gated *before* it is merged, and the merged run inherits the
    // first refusal any fragment met. A gate is never weakened by joining fragments.
    let mut fragments = Vec::with_capacity(group.len);
    let mut refused: Option<Admissibility> = None;
    for offset in 0..group.len {
        let run = &runs[group.first + offset];
        let pair = pairs[group.first + offset];
        let described = describe_single(document, page, run, pair, rects)?;
        if !described.admissibility.editable && refused.is_none() {
            refused = Some(described.admissibility.clone());
        }
        fragments.push(described);
    }

    let head = &fragments[0];
    let joined: String = fragments
        .iter()
        .map(|fragment| fragment.text.as_str())
        .collect();
    let rect = union_rect(group, pairs, rects);

    // A refusal anywhere in the group refuses the whole run, naming the same gate and reason
    // as the fragment that failed. When the head paired but a later fragment did not, the
    // merged run has no box either (`union_rect` returns `None`), so it is refused with the
    // same T21 prose.
    let mut admissibility = match refused {
        Some(verdict) => verdict,
        None if rect.is_none() => Admissibility::refused_with(Gate::RunMismatch, UNPAIRED_MESSAGE),
        None => head.admissibility.clone(),
    };

    // P2.4.2 T22: the width quota is the group's own measured span, never the union box's width.
    // A strong pair may supply the box, but only when box ≈ span (the ordinary merged run). A
    // line-sized box is *box-suspicious*: flagged weak, and its quota is its own span — so a
    // replacement too wide for the run refuses quoting that span, not the line. When nothing is
    // measurable the run refuses rather than trusting a possibly line-sized box (safe direction).
    let span = group_span(document, page, runs, group);
    let box_width = rect.map(crate::editor::geometry::PdfRect::width);
    let group_strong =
        pairs[group.first].is_some_and(|pair| pair.confidence == PairConfidence::Strong);
    let (width, box_suspicious) = match resolve_quota(span, box_width, group_strong) {
        Quota::Box(box_width) => (Some(box_width), false),
        Quota::Span { width, suspicious } => (Some(width), suspicious),
        Quota::Unmeasurable => {
            if admissibility.editable {
                admissibility =
                    Admissibility::refused_with(Gate::FitsWidth, UNMEASURABLE_WIDTH_MESSAGE);
            }
            (None, true)
        }
    };

    // The group's confidence is its weakest member. A merged run is only ever assembled from
    // strong pairs, so for a group this is the head's confidence; for an unpaired singleton it
    // is `None`. A box-suspicious pair is reported weak: its box cannot be trusted as a match.
    let mut confidence = Some(PairConfidence::Strong);
    for offset in 0..group.len {
        match pairs[group.first + offset] {
            None => confidence = None,
            Some(pair) if pair.confidence == PairConfidence::Weak => {
                if confidence.is_some() {
                    confidence = Some(PairConfidence::Weak);
                }
            }
            Some(_) => {}
        }
    }
    if box_suspicious && confidence.is_some() {
        confidence = Some(PairConfidence::Weak);
    }

    Ok(PageTextRun {
        run_id: head.run_id.clone(),
        text: joined,
        operator: head.operator.clone(),
        font_key: head.font_key.clone(),
        font_name: head.font_name.clone(),
        font_size: head.font_size,
        rect,
        // O1 (§4) / T22: the width gate measures the run's own span, and the refusal quotes this
        // as the old width. The merged union box supplies it only when box ≈ span.
        width,
        box_suspicious,
        admissibility,
        pair_confidence: confidence,
        fragment_count: group.len,
        // O-Pair-3: only a genuinely merged run carries the second display level; a single
        // run's own fields already describe its operand.
        fragments: if group.len > 1 { fragments } else { Vec::new() },
    })
}

/// Builds the picker's run list for a page, pairing lopdf addresses with PDFium geometry
/// **by position** (P2.4.1 T17).
///
/// The 0.4.0 defect was pairing by decoded-text equality: Word/PowerPoint emit a `Tj` per
/// kerning group or a `TJ` per glyph, so a lopdf "run" is a fragment while a PDFium run is a
/// whole word or line. The texts virtually never matched, and every run was refused. Pairing
/// by the text-space origin the run was shown at is segmentation-independent (T17), and the
/// merge phase (P2.4.1 T18) uses the same pairing to fold fragments back into word-ish runs.
///
/// A run with no plausible rendered box keeps its address and gates but cannot be offered:
/// without geometry the user cannot see what they are picking, and without a measured width
/// the write path cannot evaluate gate (d). It is refused **naming that reason** (T21).
pub fn describe_runs(
    document: &Document,
    page: u32,
    pdfium_runs: &[crate::editor::runmetrics::PdfiumRun],
) -> Result<Vec<PageTextRun>, AppError> {
    let page_id = page_id(document, page)?;
    let runs = textlocate::locate_runs(document, page_id)?;

    // Pair every run independently: several lopdf fragments may legitimately sit inside one
    // PDFium box, which is precisely the fragmentation this phase exists for. Nothing is
    // consumed, so a shared box is not a conflict.
    let pairs: Vec<Option<Pair>> = runs.iter().map(|run| pair_run(run, pdfium_runs)).collect();

    let mut described = Vec::new();
    for group in group_runs(&runs, &pairs, pdfium_runs) {
        // P2.4.2 T22: a merged group whose union box is line-sized for its own span is not a
        // merge — its fragments do not fill the box they share, so the box is a line the group
        // only occupies part of. Split into singletons, each measured by its own span.
        if group.len > 1 && group_box_suspicious(document, page, &runs, &pairs, pdfium_runs, &group)
        {
            for offset in 0..group.len {
                let singleton = RunGroup {
                    first: group.first + offset,
                    len: 1,
                };
                described.push(describe_group(
                    document,
                    page,
                    &runs,
                    &pairs,
                    pdfium_runs,
                    &singleton,
                )?);
            }
            continue;
        }
        described.push(describe_group(
            document,
            page,
            &runs,
            &pairs,
            pdfium_runs,
            &group,
        )?);
    }

    Ok(described)
}

/// The T21 refusal message for an editable run with no rendered box to pair with.
pub const UNPAIRED_MESSAGE: &str = "no matching rendered box on the page; \
     add a text annotation instead (gate: run-mismatch)";
fn describe_single(
    document: &Document,
    page: u32,
    run: &RawRun,
    pair: Option<Pair>,
    rects: &[crate::editor::runmetrics::PdfiumRun],
) -> Result<PageTextRun, AppError> {
    let gates = resolve_gates(document, page, run)?;
    let mut verdict = font_admissibility(&gates);

    let matched = pair.and_then(|pair| rects.get(pair.rect).map(|rect| (pair, rect)));
    if matched.is_none() && verdict.editable {
        // The font gates passed, but there is no box for this run: the address is real while
        // its geometry is not. T21 requires the refusal to *name* that, not merely refuse.
        verdict = Admissibility::refused_with(Gate::RunMismatch, UNPAIRED_MESSAGE);
    }

    Ok(PageTextRun {
        run_id: run.id.clone(),
        text: run.text.clone(),
        operator: run.operator.clone(),
        font_key: String::from_utf8_lossy(&run.font_key).into_owned(),
        font_name: matched
            .map(|(_, rect)| rect.font_name.clone())
            .unwrap_or_default(),
        font_size: matched
            .map(|(_, rect)| rect.font_size)
            .unwrap_or(run.font_size),
        rect: pair.and_then(|pair| rects.get(pair.rect)).map(|rect| {
            crate::editor::geometry::PdfRect {
                x0: rect.x0,
                y0: rect.y0,
                x1: rect.x1,
                y1: rect.y1,
            }
        }),
        // The *box* width is what a display fragment shows. T22 replaces this with the run's
        // own span on the top-level entry (`describe_group`); a fragment nested inside a merged
        // run is display-only and keeps the box, exactly as the overlay draws it.
        width: pair
            .and_then(|pair| rects.get(pair.rect))
            .map(|rect| rect.width),
        box_suspicious: false,
        admissibility: verdict,
        pair_confidence: pair.map(|pair| pair.confidence),
        fragment_count: 1,
        fragments: Vec::new(),
    })
}

/// How a run's width-gate old width was resolved (P2.4.2 T22).
enum Quota {
    /// A strong pair whose box is ≈ the run's own span: the box *is* its ink, so the user's
    /// expectation is measured directly.
    Box(f32),
    /// The run's own measured span. `suspicious` when the paired box was line-sized.
    Span { width: f32, suspicious: bool },
    /// No in-file metric to measure with: the box cannot be trusted, so the run refuses.
    Unmeasurable,
}

/// True when a group's union box is line-sized for the group's own span (P2.4.2 T22).
///
/// A *group* is box-suspicious when its fragments share a box far wider than they tile — the
/// 0.4.1 chain-break orphan, whose only box was the whole line. Per-fragment sharing is normal
/// (a per-glyph `TJ`'s fragments legitimately share the word's box, which they tile); only the
/// whole-group test tells a line box from a word box.
fn group_box_suspicious(
    document: &Document,
    page: u32,
    runs: &[RawRun],
    pairs: &[Option<Pair>],
    rects: &[crate::editor::runmetrics::PdfiumRun],
    group: &RunGroup,
) -> bool {
    let span = group_span(document, page, runs, group);
    let box_width = union_rect(group, pairs, rects).map(crate::editor::geometry::PdfRect::width);
    let strong = pairs[group.first].is_some_and(|pair| pair.confidence == PairConfidence::Strong);
    matches!(
        resolve_quota(span, box_width, strong),
        Quota::Span {
            suspicious: true,
            ..
        }
    )
}

/// Resolves the width-gate old width — the run's own span, never an inherited box width
/// (P2.4.2 T22).
///
/// * A strong pair whose box is no wider than `span × BOX_SPAN_SLACK` supplies the box width:
///   box ≈ span, so the box is the run's own ink.
/// * Otherwise the span wins. A box that is measurably line-sized comes back `suspicious`, so
///   the caller flags the pair and the UI can say so.
/// * With no span at all, a box may be a whole line: [`Quota::Unmeasurable`], and the caller
///   refuses rather than trusting it (the safe direction).
fn resolve_quota(span: Option<f32>, box_width: Option<f32>, strong: bool) -> Quota {
    let box_plausible = strong
        && span.is_some_and(|span| box_width.is_some_and(|box_w| box_w <= span * BOX_SPAN_SLACK));
    if box_plausible {
        return Quota::Box(box_width.expect("box_plausible implies a box width"));
    }
    match span {
        Some(span) => {
            let suspicious = box_width.is_some_and(|box_w| box_w > span * BOX_SPAN_SLACK);
            Quota::Span {
                width: span,
                suspicious,
            }
        }
        None => Quota::Unmeasurable,
    }
}

/// One further fragment of a merged run, as the picker knows it (P2.4.1 T18).
///
/// The head fragment travels as `EditRequest::run_id` + `expected`; every following fragment
/// travels here, with the text the picker showed so the write path can re-validate it (T4).
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FragmentRef {
    pub run_id: RunId,
    pub text: String,
}

/// The request to edit one run (or one merged run, with `fragments` filled).
pub struct EditRequest<'a> {
    /// 0-based page index.
    pub page: u32,
    /// The head run the user picked, by content-stream address (T4).
    pub run_id: RunId,
    /// The head fragment's text the user was shown. Re-validated against the bytes at write
    /// time (T4).
    pub expected: String,
    /// The replacement text.
    pub replacement: String,
    /// The run's measured advance width in points (PDFium, read-only). For a merged run this is
    /// the **union** box's width (O1/§4).
    pub old_width: f32,
    /// The run's font size, so MECH-A can measure the substitute's width itself (T13).
    pub font_size: f32,
    /// The O-ST2-4 toggle: when false, a run whose font cannot set the replacement refuses
    /// (`SUBSTITUTE_REFUSED`) instead of being switched to a standard-14 font.
    pub allow_substitution: bool,
    /// The remaining fragments of a merged run, head first after the head (P2.4.1 T18). Empty
    /// for an ordinary single-operand edit.
    pub fragments: Vec<FragmentRef>,
    pub measure: WidthMeasure<'a>,
}

/// What an edit actually did, so the caller can surface the substitution note (T10) and
/// fold the correct inverse into the undo stack (T5/T16).
#[derive(Debug)]
pub struct EditOutcome {
    pub inverse: ContentOperation,
    /// The substitute's `/BaseFont` when MECH-A fired, `None` for an ordinary edit.
    pub substitution: Option<String>,
    /// The disclosure sentence shown to the user when MECH-A fired (T10).
    pub note: Option<String>,
}

/// Applies a Stage-2 edit, returning the inverse operation for undo (T5).
///
/// Gate order (T11/T13/T14 is load-bearing):
/// 1. identity + font gates (SimpleFont, EncodingRoundTrip, RunMismatch) — as Stage 1;
/// 2. **coverage check** — if the run's own font covers the replacement, keep it (T11);
/// 3. otherwise MECH-A substitute by style-match (or refuse if the toggle is off);
/// 4. encode the replacement through the *final* font's encoding;
/// 5. **width gate with the final font's metrics** (T13): the substitute's AFM widths when
///    MECH-A fires, the caller's measured width otherwise;
/// 6. mutate through lopdf (bytes + `/Fx` swap).
pub fn edit(document: &mut Document, request: &EditRequest<'_>) -> Result<EditOutcome, AppError> {
    let page_id = textlocate::page_id(document, request.page)?;
    let runs = textlocate::locate_runs(document, page_id)?;

    let run = runs
        .iter()
        .find(|run| run.id == request.run_id)
        .ok_or_else(|| {
            AppError::TextRunNotEditable(format!(
                "that text is no longer at this position; reopen the page and try again (gate: {})",
                Gate::EncodingRoundTrip.name()
            ))
        })?;

    // T4: the run at this address must still read as what the user edited.
    if run.text != request.expected {
        return Err(AppError::TextRunMismatch);
    }

    // T4 for the merged run's tail (P2.4.1 T18): the picker's fragment list and the document's
    // must agree here too. The per-fragment byte check happens again in `apply_merged`, against
    // the content actually being written; this earlier check keeps the *addresses* honest, so a
    // stale picker entry cannot name an operand that has since become something else.
    let page_id_for_fragments = page_id;
    let all_runs = textlocate::locate_runs(document, page_id_for_fragments)?;
    for fragment in &request.fragments {
        let current = all_runs
            .iter()
            .find(|run| run.id == fragment.run_id)
            .ok_or(AppError::TextRunMismatch)?;
        if current.text != fragment.text {
            return Err(AppError::TextRunMismatch);
        }
    }

    // T3/T14 (a)/(b): re-run the invariant font gates against the freshly parsed run.
    let gates = resolve_gates(document, request.page, run)?;
    let verdict = font_admissibility(&gates);
    if !verdict.editable {
        return Err(refuse(match verdict.gate.as_deref() {
            Some("encoding-round-trip") => Gate::EncodingRoundTrip,
            _ => Gate::SimpleFont,
        }));
    }

    // Stage 2 (T11): keep the original font when it can set the replacement; otherwise
    // MECH-A picks a standard-14 substitute. This runs BEFORE any mutation.
    let plan = crate::editor::substitute::plan(
        document,
        page_id,
        &run.font_key,
        &gates.font,
        &request.replacement,
        request.allow_substitution,
    )?;

    // Step 4 (design §4): encode the replacement through the encoding that will actually
    // be in force. Keep-original uses the run's own encoding; MECH-A uses WinAnsi, which
    // the substitute declares. In both cases an unmappable character is a named refusal,
    // never a silent drop (`string_to_bytes` would drop it).
    let new_bytes = match &plan {
        crate::editor::substitute::Plan::KeepOriginal => {
            let encoding = {
                let fonts = document.get_page_fonts(page_id).map_err(|error| {
                    AppError::Other(format!("could not read page fonts: {error}"))
                })?;
                fonts
                    .get(&run.font_key)
                    .copied()
                    .and_then(|font| font.get_font_encoding(document).ok())
                    .ok_or_else(|| refuse(Gate::EncodingRoundTrip))?
            };
            let bytes = encoding.string_to_bytes(&request.replacement);
            let round_tripped = encoding
                .bytes_to_string(&bytes)
                .map_err(|_| refuse(Gate::EncodingRoundTrip))?;
            if round_tripped != request.replacement {
                return Err(refuse(Gate::EncodingRoundTrip));
            }
            bytes
        }
        crate::editor::substitute::Plan::Substitute { .. } => {
            crate::editor::afm::winansi_encode(&request.replacement)
                .ok_or(AppError::GlyphsNotCoverable)?
        }
    };

    // T13: the width gate compares the FINAL font's metrics. When MECH-A fires, the final
    // font is the standard-14 substitute and its AFM widths are authoritative; otherwise
    // the caller's PDFium measurement is. `old_width` is unchanged either way (O1).
    let (new_width, substitution, note) = match &plan {
        crate::editor::substitute::Plan::KeepOriginal => {
            ((request.measure)(&request.replacement)?, None, None)
        }
        crate::editor::substitute::Plan::Substitute { base_font, note } => {
            let width = crate::editor::substitute::substitute_width(
                base_font,
                &request.replacement,
                request.font_size,
            )
            .ok_or(AppError::GlyphsNotCoverable)?;
            (width, Some(base_font.clone()), Some(note.clone()))
        }
    };
    if let Some(error) = width_refusal(request.old_width, new_width) {
        return Err(error);
    }

    // Bytes first, then the `/Fx` swap, so a failure in either leaves no half-done state
    // visible. A merged run (P2.4.1 T18) does both in ONE decode -> mutate -> encode pass: the
    // head fragment carries the whole replacement, the rest are cleared to empty, and the swap
    // rides along so bytes and typeface change together or not at all. An ordinary run is the
    // one-fragment case of the same call.
    let mut writes = vec![textlocate::FragmentWrite {
        id: request.run_id.clone(),
        expected: request.expected.clone(),
        new_bytes: new_bytes.clone(),
    }];
    for fragment in &request.fragments {
        writes.push(textlocate::FragmentWrite {
            id: fragment.run_id.clone(),
            expected: fragment.text.clone(),
            // Empty: the run is shown once, not once per fragment. The `TJ` numbers between the
            // operands are left in place (T19).
            new_bytes: Vec::new(),
        });
    }
    let merged = textlocate::MergedWrite {
        fragments: writes,
        substitute: substitution.as_deref(),
    };
    let inverse = textlocate::apply_merged(document, request.page, page_id, &merged)?;

    Ok(EditOutcome {
        inverse,
        substitution,
        note,
    })
}

/// Reverses a text edit from its recorded inverse (T5), re-validating first.
///
/// `document` must currently be in the post-edit state; the inverse's `page` and address
/// identify the run, and `new_bytes` is what should be there now.
///
/// A merged edit (P2.4.1 T18) recorded its remaining fragments in `also` and any further `Tf`
/// swaps in `font_swap.also`; all of them are restored here, so one merged edit is one undo. A
/// stale inverse — one whose recorded bytes are not what the document now holds — refuses
/// rather than replaying an edit the user did not make.
pub fn revert(document: &mut Document, operation: &ContentOperation) -> Result<(), AppError> {
    let page_id = page_id(document, operation.page)?;
    let mut content = textlocate::decoded_content(document, page_id)?;

    // Every write the edit made, head first.
    let writes: Vec<&ContentOperation> = std::iter::once(operation)
        .chain(operation.also.iter())
        .collect();

    for write in &writes {
        let current = content
            .operations
            .get(write.op_index)
            .and_then(|op| string_at(op, &write.operand_path))
            .ok_or(AppError::TextRunMismatch)?;
        if current != write.new_bytes.as_slice() {
            // Something else changed these bytes since; replaying the inverse would undo an
            // edit the user did not make.
            return Err(AppError::TextRunMismatch);
        }
    }

    let encoded = {
        for write in &writes {
            let slot = string_at_mut(
                content
                    .operations
                    .get_mut(write.op_index)
                    .ok_or(AppError::TextRunMismatch)?,
                &write.operand_path,
            )
            .ok_or(AppError::TextRunMismatch)?;
            *slot = write.old_bytes.clone();
        }

        // Undo the font swaps alongside the bytes (Stage 2 T10/T16): leaving a substitute in
        // place while restoring the text would silently keep the changed typeface.
        if let Some(swap) = &operation.font_swap {
            for swap in std::iter::once(swap).chain(swap.also.iter()) {
                let slot = operation_at_mut(&mut content, swap).ok_or(AppError::TextRunMismatch)?;
                *slot = swap.old_name.clone();
            }
        }

        content.encode().map_err(|error| {
            AppError::Other(format!("could not re-encode page content: {error}"))
        })?
    };

    document
        .change_page_content(page_id, encoded)
        .map_err(|error| AppError::Other(format!("could not replace page content: {error}")))
}

/// The mutable resource-name operand a [`FontSwap`] names.
fn operation_at_mut<'a>(
    content: &'a mut lopdf::content::Content<Vec<lopdf::content::Operation>>,
    swap: &crate::editor::textlocate::FontSwap,
) -> Option<&'a mut Vec<u8>> {
    let path = swap.operand_path.as_slice();
    let operation = content.operations.get_mut(swap.op_index)?;
    let mut current = operation.operands.get_mut(*path.first()?)?;
    for index in &path[1..] {
        current = current.as_array_mut().ok()?.get_mut(*index)?;
    }
    match current {
        lopdf::Object::Name(name) => Some(name),
        _ => None,
    }
}

// Local copies of the string accessors, so `revert` does not depend on `textlocate`'s
// private helpers staying public. They are three lines each and identical.
fn string_at<'a>(operation: &'a lopdf::content::Operation, path: &[usize]) -> Option<&'a [u8]> {
    let mut current = operation.operands.get(*path.first()?)?;
    for index in &path[1..] {
        current = current.as_array().ok()?.get(*index)?;
    }
    match current {
        lopdf::Object::String(bytes, _) => Some(bytes),
        _ => None,
    }
}

fn string_at_mut<'a>(
    operation: &'a mut lopdf::content::Operation,
    path: &[usize],
) -> Option<&'a mut Vec<u8>> {
    let mut current = operation.operands.get_mut(*path.first()?)?;
    for index in &path[1..] {
        current = current.as_array_mut().ok()?.get_mut(*index)?;
    }
    match current {
        lopdf::Object::String(bytes, _) => Some(bytes),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::testpdf;

    fn measure_fixed(width: f32) -> impl Fn(&str) -> Result<f32, AppError> {
        move |_| Ok(width)
    }

    fn request<'a>(
        run: &RawRun,
        replacement: &str,
        old_width: f32,
        measure: WidthMeasure<'a>,
    ) -> EditRequest<'a> {
        EditRequest {
            page: 0,
            run_id: run.id.clone(),
            expected: run.text.clone(),
            replacement: replacement.to_string(),
            old_width,
            font_size: 24.0,
            allow_substitution: true,
            fragments: Vec::new(),
            measure,
        }
    }

    /// A test run at a given origin, size and font, for the merge predicates.
    fn test_run(text: &str, x: f32, size: f32, font: &str) -> RawRun {
        let approximation = text.chars().count() as f32 * 0.5 * size;
        RawRun {
            id: RunId {
                op_index: 0,
                operand_path: vec![0],
            },
            bytes: text.as_bytes().to_vec(),
            text: text.to_string(),
            font_key: font.as_bytes().to_vec(),
            operator: "Tj".to_string(),
            origin: (x, 700.0),
            font_size: size,
            advance: approximation,
            // The approximation doubles as the exact metric here, so a unit test that does not
            // care about the metric source behaves as before (P2.4.2 Phase A).
            advance_exact: Some(approximation),
        }
    }

    #[test]
    fn merges_fragments_on_one_baseline_within_a_quarter_em() {
        // "He" then "llo", the second starting exactly where the first ends.
        let head = test_run("He", 72.0, 12.0, "F1");
        let next = test_run("llo", 72.0 + head.advance, 12.0, "F1");
        assert!(mergeable(&head, &next, false));
    }

    #[test]
    fn never_merges_across_a_font_or_a_size() {
        let head = test_run("AB", 72.0, 12.0, "F1");
        // Same position and size, different font: the merged run could not be encoded with
        // one font (T19).
        let other_font = test_run("CD", 72.0 + head.advance, 12.0, "F2");
        assert!(!mergeable(&head, &other_font, false));
        // Same font and position, different size: a heading is not a body word.
        let other_size = test_run("CD", 72.0 + head.advance, 10.0, "F1");
        assert!(!mergeable(&head, &other_size, false));
    }

    #[test]
    fn never_merges_across_a_line_break() {
        let head = test_run("AB", 72.0, 12.0, "F1");
        let mut next = test_run("CD", 72.0 + head.advance, 12.0, "F1");
        next.origin.1 = head.origin.1 + 12.0; // next line
        assert!(!mergeable(&head, &next, false));
    }

    #[test]
    fn merges_body_words_but_not_heading_words_at_the_same_absolute_gap() {
        // The whole reason the threshold is relative: a 3pt gap is inside a 24pt heading's
        // letter-spacing but a whole extra word at 9pt. An absolute constant would get one of
        // these wrong whichever value it picked.
        let body_head = test_run("foo", 72.0, 9.0, "F1");
        let body_next = test_run("bar", 72.0 + body_head.advance + 3.0, 9.0, "F1");
        // 3pt gap vs tolerance 0.25*9 = 2.25pt: too far, the words stay separate.
        assert!(!mergeable(&body_head, &body_next, false));

        let head_head = test_run("Fo", 72.0, 24.0, "F1");
        let head_next = test_run("o", 72.0 + head_head.advance + 3.0, 24.0, "F1");
        // 3pt gap vs tolerance 0.25*24 = 6pt: fine, the heading's glyphs merge.
        assert!(mergeable(&head_head, &head_next, false));
    }

    /// P2.4.2 O-Chain-2: the widened `0.5em` gap applies **only** when PDFium's reconstructed
    /// text carries a real space at the seam; without the space flag it never does.
    #[test]
    fn the_widened_gap_needs_a_real_space_at_the_seam() {
        // A 4pt gap at 12pt: 0.33em, past the 0.25em bound but inside the 0.5em one.
        let head = test_run("foo", 72.0, 12.0, "F1");
        let next = test_run("bar", 72.0 + head.advance + 4.0, 12.0, "F1");
        assert!(
            !mergeable(&head, &next, false),
            "no space at the seam, so the quarter-em bound still applies"
        );
        assert!(
            mergeable(&head, &next, true),
            "PDFium shows a space there, so the seam is one word-space apart"
        );
    }

    /// P2.4.2: a relative size rule, not a flat tenth of a point. A large face rounded to a
    /// different hundredth between two operands is still one run.
    #[test]
    fn size_equality_is_relative_not_a_flat_tenth_of_a_point() {
        assert!(sizes_match(48.0, 48.05), "1% of 48pt tolerates 0.05pt");
        assert!(
            !sizes_match(9.0, 9.3),
            "but 0.3pt at 9pt is a different size"
        );
        // The floor: below the 1% band the flat 0.2pt applies.
        assert!(sizes_match(8.0, 8.15));
        assert!(!sizes_match(8.0, 8.3));
    }

    /// P2.4.2 overlap guard: a negative gap past `-0.05em` is a mis-measure, not a seam, and
    /// must break the chain rather than weld two colliding fragments together.
    #[test]
    fn a_large_overlap_breaks_the_chain() {
        let head = test_run("AB", 72.0, 12.0, "F1");
        // Inside the 0.05em (0.6pt) overlap: still one chain.
        let touching = test_run("CD", 72.0 + head.advance - 0.3, 12.0, "F1");
        assert!(mergeable(&head, &touching, false));
        // Past it: the head's advance overshot, so do not join.
        let overlapping = test_run("CD", 72.0 + head.advance - 2.0, 12.0, "F1");
        assert!(!mergeable(&head, &overlapping, false));
    }

    #[test]
    fn text_equality_only_breaks_a_geometric_tie() {
        use crate::editor::runmetrics::PdfiumRun;
        let box_at = |text: &str, x0: f32| PdfiumRun {
            text: text.to_string(),
            font_name: "Helvetica".to_string(),
            font_size: 12.0,
            x0,
            y0: 700.0,
            x1: x0 + 20.0,
            y1: 712.0,
            width: 20.0,
        };
        // Two boxes that are geometrically indistinguishable at the run's origin: the same
        // baseline, the same left edge, the same delta. Only exact text can separate them, and
        // §3 step 2 says it does — but it is the *last* key, so geometry still wins whenever it
        // can (the next case).
        let run = test_run("He", 72.0, 12.0, "F1");
        let rects = vec![box_at("other", 72.0), box_at("He", 72.0)];
        let pair = pair_run(&run, &rects).expect("a containing box pairs");
        assert_eq!(pair.rect, 1, "exact text breaks the tie");

        // Geometry first: a box whose baseline is nearer wins even though its text differs,
        // because text must never override the position T17 pairs on.
        let mut exact_but_far = box_at("He", 72.0);
        exact_but_far.y0 = 700.2;
        exact_but_far.y1 = 712.2;
        let rects = vec![exact_but_far, box_at("other", 72.0)];
        let pair = pair_run(&run, &rects).expect("a containing box pairs");
        assert_eq!(
            pair.rect, 1,
            "the nearer baseline wins even though its text differs"
        );
    }

    /// Builds one box per run, sized to that run's **own** measured span (P2.4.2 T22), so a
    /// group test that is not about box-vs-span sees ordinary, non-suspicious boxes. Text is
    /// space-free so the widened O-Chain-2 gap never applies.
    fn group_boxes(
        document: &lopdf::Document,
        runs: &[RawRun],
    ) -> Vec<crate::editor::runmetrics::PdfiumRun> {
        runs.iter()
            .map(|run| {
                let span = measured_span(document, 0, run).unwrap_or(0.0);
                crate::editor::runmetrics::PdfiumRun {
                    text: run.text.clone(),
                    font_name: "Helvetica".to_string(),
                    font_size: run.font_size,
                    x0: run.origin.0,
                    y0: 700.0,
                    x1: run.origin.0 + span,
                    y1: 712.0,
                    width: span,
                }
            })
            .collect()
    }

    #[test]
    fn groups_merge_strongly_paired_consecutive_runs_only() {
        let document = testpdf::simple_winansi();
        let head = test_run("He", 72.0, 12.0, "F1");
        // Placed exactly where the head ends, so the seam is a genuine adjacency, not an
        // overlap (P2.4.2's overlap guard).
        let runs = vec![
            head.clone(),
            test_run("llo", 72.0 + head.advance, 12.0, "F1"),
            test_run("far", 200.0, 12.0, "F1"),
        ];
        let strong = |rect: usize| {
            Some(Pair {
                rect,
                confidence: PairConfidence::Strong,
            })
        };
        let boxes = group_boxes(&document, &runs);
        let groups = group_runs(&runs, &[strong(0), strong(0), strong(2)], &boxes);
        // Two groups: "Hello" (2 fragments) and "far" (1), because the gap to "far" is far
        // past a quarter em.
        assert_eq!(groups.len(), 2);
        assert_eq!((groups[0].first, groups[0].len), (0, 2));
        assert_eq!((groups[1].first, groups[1].len), (2, 1));
    }

    #[test]
    fn a_weak_pair_is_never_merged_and_an_unpaired_run_stays_visible() {
        let document = testpdf::simple_winansi();
        let runs = vec![
            test_run("He", 72.0, 12.0, "F1"),
            test_run("llo", 78.0, 12.0, "F1"),
            test_run("solo", 200.0, 12.0, "F1"),
        ];
        let weak = Some(Pair {
            rect: 0,
            confidence: PairConfidence::Weak,
        });
        let boxes = group_boxes(&document, &runs);
        let groups = group_runs(&runs, &[weak, weak, None], &boxes);
        // One group per run: the weak pair is not merged, and the unpaired run is still
        // reported as a singleton so the picker can name its refusal (T21).
        assert_eq!(groups.len(), 3);
        assert!(groups.iter().all(|group| group.len == 1));
    }

    /// P2.4.2 T22: a group whose union box is line-sized for the group's own span is
    /// box-suspicious — the 0.4.1 `ence` orphan against a whole-line box. Two per-glyph
    /// fragments that *tile* their shared word box are not.
    #[test]
    fn a_group_with_a_line_sized_box_is_suspicious() {
        let document = testpdf::simple_winansi();
        let head = test_run("He", 72.0, 12.0, "F1");
        let tail = test_run("llo", 72.0 + head.advance, 12.0, "F1");
        let runs = vec![head, tail];
        let strong = |rect: usize| {
            Some(Pair {
                rect,
                confidence: PairConfidence::Strong,
            })
        };
        let pairs = [strong(0), strong(0)];
        let group = RunGroup { first: 0, len: 2 };

        // A word box the two fragments tile: not suspicious.
        let mut tiled = group_boxes(&document, &runs);
        tiled[0].x1 = 90.0;
        tiled[0].width = 18.0;
        tiled[1].x0 = 72.0;
        tiled[1].x1 = 90.0;
        tiled[1].width = 18.0;
        assert!(!group_box_suspicious(
            &document, 0, &runs, &pairs, &tiled, &group
        ));

        // A line box ten times their span: suspicious — this is the orphan's pair.
        let mut line_sized = tiled.clone();
        line_sized[0].x1 = 72.0 + 200.0;
        line_sized[0].width = 200.0;
        line_sized[1].x1 = 72.0 + 200.0;
        line_sized[1].width = 200.0;
        assert!(group_box_suspicious(
            &document,
            0,
            &runs,
            &pairs,
            &line_sized,
            &group
        ));
    }

    #[test]
    fn edits_a_simple_font_run_when_it_fits() {
        let mut document = testpdf::simple_winansi();
        let run =
            textlocate::locate_runs(&document, page_id(&document, 0).unwrap()).unwrap()[0].clone();
        let measure = measure_fixed(30.0);

        let outcome = edit(&mut document, &request(&run, "BRAVO", 38.7, &measure)).unwrap();

        assert_eq!(outcome.inverse.old_bytes, b"ALPHA");
        assert_eq!(outcome.inverse.new_bytes, b"BRAVO");
        // The run's own font was kept: no substitution, no note (T11/T10).
        assert!(outcome.substitution.is_none());
        assert!(outcome.note.is_none());
    }

    #[test]
    fn refuses_a_cid_font_with_gate_a() {
        let mut document = testpdf::cid_identity_page();
        let run =
            textlocate::locate_runs(&document, page_id(&document, 0).unwrap()).unwrap()[0].clone();
        let measure = measure_fixed(10.0);

        let error = edit(&mut document, &request(&run, "EDIT", 100.0, &measure)).unwrap_err();
        assert_eq!(error.code(), "TEXT_RUN_NOT_EDITABLE");
        assert!(error.to_string().contains("simple-font"), "{error}");
    }

    /// Stage 2 T14: a subset font is no longer refused by gate (c). With no embedded
    /// program to inspect (`subset_font_page` has none), the run is *not* covered, so
    /// MECH-A substitutes Helvetica and the edit succeeds — where Stage 1 refused.
    #[test]
    fn substitutes_a_subset_font_rather_than_refusing_it() {
        let mut document = testpdf::subset_font_page();
        let run =
            textlocate::locate_runs(&document, page_id(&document, 0).unwrap()).unwrap()[0].clone();
        let measure = measure_fixed(10.0);

        let outcome = edit(&mut document, &request(&run, "EDIT", 100.0, &measure)).unwrap();
        assert_eq!(outcome.substitution.as_deref(), Some("Helvetica"));
        assert!(outcome.note.as_deref().unwrap().contains("substituting"));
        assert!(
            outcome.inverse.font_swap.is_some(),
            "the /Tf swap must be recorded"
        );
    }

    /// O-ST2-4: the same run with substitution off refuses `SUBSTITUTE_REFUSED`.
    #[test]
    fn refuses_a_subset_font_when_substitution_is_off() {
        let mut document = testpdf::subset_font_page();
        let run =
            textlocate::locate_runs(&document, page_id(&document, 0).unwrap()).unwrap()[0].clone();
        let measure = measure_fixed(10.0);
        let mut req = request(&run, "EDIT", 100.0, &measure);
        req.allow_substitution = false;

        let error = edit(&mut document, &req).unwrap_err();
        assert_eq!(error.code(), "SUBSTITUTE_REFUSED");
    }

    #[test]
    fn refuses_an_unroundtrippable_encoding_with_gate_b() {
        let mut document = testpdf::roundtrip_fail_page();
        let run =
            textlocate::locate_runs(&document, page_id(&document, 0).unwrap()).unwrap()[0].clone();
        // The single run is the byte that does not round-trip, so its own gate fails and
        // the run is not editable even before the replacement is considered.
        let gates = resolve_gates(&document, 0, &run).unwrap();
        let verdict = font_admissibility(&gates);
        assert!(!verdict.editable);
        assert_eq!(verdict.gate.as_deref(), Some("encoding-round-trip"));

        let measure = measure_fixed(10.0);
        let error = edit(&mut document, &request(&run, "x", 100.0, &measure)).unwrap_err();
        assert_eq!(error.code(), "TEXT_RUN_NOT_EDITABLE");
        assert!(error.to_string().contains("encoding-round-trip"), "{error}");
    }

    #[test]
    fn refuses_too_wide_with_both_measured_widths() {
        let mut document = testpdf::simple_winansi();
        let run =
            textlocate::locate_runs(&document, page_id(&document, 0).unwrap()).unwrap()[0].clone();
        // 38.7pt available, 41.3pt needed: the design's worked example (O1).
        let measure = measure_fixed(41.3);

        let error = edit(&mut document, &request(&run, "ALPHABETIC", 38.7, &measure)).unwrap_err();
        assert_eq!(error.code(), "TEXT_RUN_NOT_EDITABLE");
        let message = error.to_string();
        assert!(
            message.contains("41.3"),
            "must report the needed width: {message}"
        );
        assert!(
            message.contains("38.7"),
            "must report the available width: {message}"
        );
        assert!(message.contains("fits-width"), "{message}");
    }

    #[test]
    fn allows_a_replacement_within_the_rounding_tolerance() {
        // 1% of 100pt is 1pt, so 100.9pt must pass: metric rounding between font
        // programs is real and refusing here would make the feature feel broken.
        assert!(width_refusal(100.0, 100.9).is_none());
        // 101.1pt is past both the 0.5pt floor and the 1% band, so it is refused.
        assert!(width_refusal(100.0, 101.1).is_some());
        // At small widths the 0.5pt floor dominates.
        assert!(width_refusal(10.0, 10.4).is_none());
        assert!(width_refusal(10.0, 10.6).is_some());
    }

    #[test]
    fn refuses_when_the_run_no_longer_matches() {
        let mut document = testpdf::simple_winansi();
        let run =
            textlocate::locate_runs(&document, page_id(&document, 0).unwrap()).unwrap()[0].clone();
        let measure = measure_fixed(30.0);
        let mut req = request(&run, "BRAVO", 100.0, &measure);
        req.expected = "SOMETHING ELSE".to_string();

        let error = edit(&mut document, &req).unwrap_err();
        assert_eq!(error.code(), "TEXT_RUN_MISMATCH");
    }

    #[test]
    fn revert_restores_the_previous_bytes() {
        let mut document = testpdf::simple_winansi();
        let run =
            textlocate::locate_runs(&document, page_id(&document, 0).unwrap()).unwrap()[0].clone();
        let measure = measure_fixed(30.0);

        let outcome = edit(&mut document, &request(&run, "BRAVO", 100.0, &measure)).unwrap();

        revert(&mut document, &outcome.inverse).unwrap();

        let after = textlocate::locate_runs(&document, page_id(&document, 0).unwrap()).unwrap();
        assert_eq!(after[0].text, "ALPHA");
    }

    /// Stage 2 T16: undoing a *substituting* edit restores both the text and the font.
    #[test]
    fn revert_restores_the_font_after_a_substitution() {
        let mut document = testpdf::subset_font_page();
        let run =
            textlocate::locate_runs(&document, page_id(&document, 0).unwrap()).unwrap()[0].clone();
        let measure = measure_fixed(10.0);

        let outcome = edit(&mut document, &request(&run, "EDIT", 100.0, &measure)).unwrap();
        assert!(outcome.inverse.font_swap.is_some());

        // The substitute is now in force; the run reads through WinAnsi and is "EDIT".
        let substituted =
            textlocate::locate_runs(&document, page_id(&document, 0).unwrap()).unwrap();
        assert_eq!(substituted[0].text, "EDIT");

        revert(&mut document, &outcome.inverse).unwrap();

        let restored = textlocate::locate_runs(&document, page_id(&document, 0).unwrap()).unwrap();
        assert_eq!(restored[0].text, "SUBSET");
    }

    #[test]
    fn revert_refuses_when_the_bytes_changed_since() {
        let mut document = testpdf::simple_winansi();
        let run =
            textlocate::locate_runs(&document, page_id(&document, 0).unwrap()).unwrap()[0].clone();
        let measure = measure_fixed(30.0);
        let outcome = edit(&mut document, &request(&run, "BRAVO", 100.0, &measure)).unwrap();

        // Simulate a later, unrelated edit: the inverse's `new_bytes` are no longer there.
        let stale = ContentOperation {
            new_bytes: b"SOMETHING ELSE".to_vec(),
            ..outcome.inverse
        };
        let error = revert(&mut document, &stale).unwrap_err();
        assert_eq!(error.code(), "TEXT_RUN_MISMATCH");
    }
}
