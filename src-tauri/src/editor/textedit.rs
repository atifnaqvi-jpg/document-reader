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
    self, is_cid_font, is_simple_font, is_subset_font, page_id, ContentOperation, FontInfo, RawRun,
    RunId,
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
pub fn resolve_gates(
    document: &Document,
    page: u32,
    run: &RawRun,
) -> Result<RunGates, AppError> {
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
pub fn font_admissibility(gates: &RunGates) -> Admissibility {
    if !is_simple_font(&gates.font) {
        return Admissibility::refused(Gate::SimpleFont);
    }
    if is_cid_font(&gates.font) {
        return Admissibility::refused(Gate::SimpleFont);
    }
    if is_subset_font(gates.font.base_font.as_deref()) {
        return Admissibility::refused(Gate::NotSubset);
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
    /// The decoded text, shown to the user and re-validated at write time.
    pub text: String,
    /// The showing operator (`Tj`, `TJ`, `'`, `"`), for diagnostics.
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
}

/// Builds the picker's run list for a page, pairing lopdf addresses with PDFium geometry.
///
/// `pdfium_runs` is matched to lopdf runs by **decoded text, in order**, consuming each
/// PDFium run once so two runs reading the same string pair up correctly. A lopdf run with
/// no PDFium counterpart keeps its address and gates but gets no rect and is reported as
/// not editable: without geometry the user cannot see what they are picking, and without a
/// measured width the write path cannot evaluate gate (d).
pub fn describe_runs(
    document: &Document,
    page: u32,
    pdfium_runs: &[crate::editor::runmetrics::PdfiumRun],
) -> Result<Vec<PageTextRun>, AppError> {
    let page_id = page_id(document, page)?;
    let runs = textlocate::locate_runs(document, page_id)?;
    let mut used = vec![false; pdfium_runs.len()];

    let mut described = Vec::with_capacity(runs.len());
    for run in &runs {
        let gates = resolve_gates(document, page, run)?;
        let mut verdict = font_admissibility(&gates);

        let matched = pdfium_runs
            .iter()
            .enumerate()
            .find(|(index, candidate)| !used[*index] && candidate.matches_text(&run.text))
            .map(|(index, candidate)| {
                used[index] = true;
                candidate
            });

        if matched.is_none() && verdict.editable {
            // The font gates passed, but the picker has no box for this run — the address
            // is real while its geometry is not, so it stays uneditable rather than
            // becoming a target the user cannot see or that gate (d) cannot measure.
            verdict = Admissibility::refused(Gate::RunMismatch);
        }

        described.push(PageTextRun {
            run_id: run.id.clone(),
            text: run.text.clone(),
            operator: run.operator.clone(),
            font_key: String::from_utf8_lossy(&run.font_key).into_owned(),
            font_name: matched.map(|run| run.font_name.clone()).unwrap_or_default(),
            font_size: matched.map(|run| run.font_size).unwrap_or_default(),
            rect: matched.map(|run| crate::editor::geometry::PdfRect {
                x0: run.x0,
                y0: run.y0,
                x1: run.x1,
                y1: run.y1,
            }),
            width: matched.map(|run| run.width),
            admissibility: verdict,
        });
    }

    Ok(described)
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
    pub measure: WidthMeasure<'a>,
}

/// Applies a Stage-1 edit, returning the inverse operation for undo (T5).
///
/// All gates are enforced here — not just the ones the picker precomputed — because the
/// precomputed verdict is a UX hint and this is the write. The order is deliberate:
/// encode the replacement first (so an unencodable string cannot pass the width gate
/// and then corrupt the file), then measure, then re-validate identity, then mutate.
pub fn edit(
    document: &mut Document,
    request: &EditRequest<'_>,
) -> Result<ContentOperation, AppError> {
    let page_id = textlocate::page_id(document, request.page)?;
    let runs = textlocate::locate_runs(document, page_id)?;

    let run = runs
        .iter()
        .find(|run| run.id == request.run_id)
        .ok_or_else(|| AppError::TextRunNotEditable(
            format!("that text is no longer at this position; reopen the page and try again (gate: {})", Gate::EncodingRoundTrip.name()),
        ))?;

    // T4: the run at this address must still read as what the user edited.
    if run.text != request.expected {
        return Err(AppError::TextRunMismatch);
    }

    // T3 (a)/(c)/(b): re-run the font gates against the freshly parsed run.
    let gates = resolve_gates(document, request.page, run)?;
    let verdict = font_admissibility(&gates);
    if !verdict.editable {
        return Err(refuse(
            match verdict.gate.as_deref() {
                Some("not-subset") => Gate::NotSubset,
                Some("encoding-round-trip") => Gate::EncodingRoundTrip,
                _ => Gate::SimpleFont,
            },
        ));
    }

    // Encode the replacement through the *document's own* font encoding. If a glyph is
    // unmappable, lopdf drops it silently; a length change means the string the user
    // typed cannot be represented, which is a gate (b) refusal, not a partial write.
    let encoding = {
        let fonts = document
            .get_page_fonts(page_id)
            .map_err(|error| AppError::Other(format!("could not read page fonts: {error}")))?;
        fonts
            .get(&run.font_key)
            .copied()
            .and_then(|font| font.get_font_encoding(document).ok())
            .ok_or_else(|| refuse(Gate::EncodingRoundTrip))?
    };
    let new_bytes = encoding.string_to_bytes(&request.replacement);
    let round_tripped = encoding
        .bytes_to_string(&new_bytes)
        .map_err(|_| refuse(Gate::EncodingRoundTrip))?;
    if round_tripped != request.replacement {
        return Err(refuse(Gate::EncodingRoundTrip));
    }

    // T3 (d): the replacement must fit the space the run occupies.
    let new_width = (request.measure)(&request.replacement)?;
    if let Some(error) = width_refusal(request.old_width, new_width) {
        return Err(error);
    }

    textlocate::apply_text(
        document,
        request.page,
        page_id,
        &request.run_id,
        &request.expected,
        &new_bytes,
    )
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
    let current = string_at(operation_at, &operation.operand_path).ok_or(AppError::TextRunMismatch)?;
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
        content
            .encode()
            .map_err(|error| AppError::Other(format!("could not re-encode page content: {error}")))?
    };

    document
        .change_page_content(page_id, encoded)
        .map_err(|error| AppError::Other(format!("could not replace page content: {error}")))
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
            measure,
        }
    }

    #[test]
    fn edits_a_simple_font_run_when_it_fits() {
        let mut document = testpdf::simple_winansi();
        let run = textlocate::locate_runs(&document, page_id(&document, 0).unwrap()).unwrap()[0]
            .clone();
        let measure = measure_fixed(30.0);

        let inverse = edit(&mut document, &request(&run, "BRAVO", 38.7, &measure)).unwrap();

        assert_eq!(inverse.old_bytes, b"ALPHA");
        assert_eq!(inverse.new_bytes, b"BRAVO");
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

    #[test]
    fn refuses_a_subset_font_with_gate_c() {
        let mut document = testpdf::subset_font_page();
        let run =
            textlocate::locate_runs(&document, page_id(&document, 0).unwrap()).unwrap()[0].clone();
        let measure = measure_fixed(10.0);

        let error = edit(&mut document, &request(&run, "EDIT", 100.0, &measure)).unwrap_err();
        assert_eq!(error.code(), "TEXT_RUN_NOT_EDITABLE");
        assert!(error.to_string().contains("not-subset"), "{error}");
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
        let run = textlocate::locate_runs(&document, page_id(&document, 0).unwrap()).unwrap()[0]
            .clone();
        // 38.7pt available, 41.3pt needed: the design's worked example (O1).
        let measure = measure_fixed(41.3);

        let error = edit(&mut document, &request(&run, "ALPHABETIC", 38.7, &measure)).unwrap_err();
        assert_eq!(error.code(), "TEXT_RUN_NOT_EDITABLE");
        let message = error.to_string();
        assert!(message.contains("41.3"), "must report the needed width: {message}");
        assert!(message.contains("38.7"), "must report the available width: {message}");
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
        let run = textlocate::locate_runs(&document, page_id(&document, 0).unwrap()).unwrap()[0]
            .clone();
        let measure = measure_fixed(30.0);
        let mut req = request(&run, "BRAVO", 100.0, &measure);
        req.expected = "SOMETHING ELSE".to_string();

        let error = edit(&mut document, &req).unwrap_err();
        assert_eq!(error.code(), "TEXT_RUN_MISMATCH");
    }

    #[test]
    fn revert_restores_the_previous_bytes() {
        let mut document = testpdf::simple_winansi();
        let run = textlocate::locate_runs(&document, page_id(&document, 0).unwrap()).unwrap()[0]
            .clone();
        let measure = measure_fixed(30.0);

        let inverse = edit(&mut document, &request(&run, "BRAVO", 100.0, &measure)).unwrap();

        revert(&mut document, &inverse).unwrap();

        let after =
            textlocate::locate_runs(&document, page_id(&document, 0).unwrap()).unwrap();
        assert_eq!(after[0].text, "ALPHA");
    }

    #[test]
    fn revert_refuses_when_the_bytes_changed_since() {
        let mut document = testpdf::simple_winansi();
        let run = textlocate::locate_runs(&document, page_id(&document, 0).unwrap()).unwrap()[0]
            .clone();
        let measure = measure_fixed(30.0);
        let inverse = edit(&mut document, &request(&run, "BRAVO", 100.0, &measure)).unwrap();

        // Simulate a later, unrelated edit: the inverse's `new_bytes` are no longer there.
        let stale = ContentOperation {
            new_bytes: b"SOMETHING ELSE".to_vec(),
            ..inverse
        };
        let error = revert(&mut document, &stale).unwrap_err();
        assert_eq!(error.code(), "TEXT_RUN_MISMATCH");
    }
}
