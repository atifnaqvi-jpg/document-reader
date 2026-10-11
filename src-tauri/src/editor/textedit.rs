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
    pub width: Option<f32>,
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
    let mut best_key: Option<(bool, f32, f32)> = None;
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
        // Prefer strong over weak, then the smallest baseline delta, then the leftmost box.
        let key = (!strong, baseline_delta, rect.x0);
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

    let mut described = Vec::with_capacity(runs.len());
    for (index, run) in runs.iter().enumerate() {
        described.push(describe_single(
            document,
            page,
            run,
            pairs[index],
            pdfium_runs,
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
        width: pair
            .and_then(|pair| rects.get(pair.rect))
            .map(|rect| rect.width),
        admissibility: verdict,
        pair_confidence: pair.map(|pair| pair.confidence),
        fragment_count: 1,
        fragments: Vec::new(),
    })
}

/// The request to edit one run.
pub struct EditRequest<'a> {
    /// 0-based page index.
    pub page: u32,
    /// The run the user picked, by content-stream address (T4).
    pub run_id: RunId,
    /// The text the user was shown. Re-validated against the bytes at write time (T4).
    pub expected: String,
    /// The replacement text.
    pub replacement: String,
    /// The run's measured advance width in points (PDFium, read-only).
    pub old_width: f32,
    /// The run's font size, so MECH-A can measure the substitute's width itself (T13).
    pub font_size: f32,
    /// The O-ST2-4 toggle: when false, a run whose font cannot set the replacement refuses
    /// (`SUBSTITUTE_REFUSED`) instead of being switched to a standard-14 font.
    pub allow_substitution: bool,
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

    // Byte replacement first, then the `/Fx` swap, so a failure in either leaves no
    // half-done state visible: both mutate the same decoded content before it is written.
    let mut inverse = textlocate::apply_text(
        document,
        request.page,
        page_id,
        &request.run_id,
        &request.expected,
        &new_bytes,
    )?;

    if substitution.is_some() {
        let base_font = substitution.clone().unwrap_or_default();
        let swap = textlocate::apply_font_swap(
            document,
            request.page,
            page_id,
            &request.run_id,
            &base_font,
        )?;
        inverse.font_swap = Some(swap);
    }

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
pub fn revert(document: &mut Document, operation: &ContentOperation) -> Result<(), AppError> {
    let page_id = page_id(document, operation.page)?;
    let mut content = textlocate::decoded_content(document, page_id)?;

    let operation_at = content
        .operations
        .get(operation.op_index)
        .ok_or(AppError::TextRunMismatch)?;
    let current =
        string_at(operation_at, &operation.operand_path).ok_or(AppError::TextRunMismatch)?;
    if current != operation.new_bytes.as_slice() {
        // Something else changed these bytes since; replaying the inverse would undo an
        // edit the user did not make.
        return Err(AppError::TextRunMismatch);
    }

    let encoded = {
        let slot = string_at_mut(
            content
                .operations
                .get_mut(operation.op_index)
                .ok_or(AppError::TextRunMismatch)?,
            &operation.operand_path,
        )
        .ok_or(AppError::TextRunMismatch)?;
        *slot = operation.old_bytes.clone();

        // Undo the font swap alongside the bytes (Stage 2 T10/T16): leaving the substitute
        // in place while restoring the text would silently keep the changed typeface.
        if let Some(swap) = &operation.font_swap {
            let slot = operation_at_mut(&mut content, swap).ok_or(AppError::TextRunMismatch)?;
            *slot = swap.old_name.clone();
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
            measure,
        }
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
