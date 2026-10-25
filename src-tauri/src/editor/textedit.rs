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

use crate::editor::runmetrics::{CharSpan, PdfiumCharGrid};
use crate::editor::textlocate::{
    self, is_simple_font, page_id, ContentOperation, FontInfo, RawRun, RunId, XObjectRefusal,
};
use crate::model::AppError;

/// The Stage-1 admissibility gates (T3), each naming the reason it can refuse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gate {
    /// (a) a font the write path cannot read back — neither simple nor a readable Type0.
    FontEncoding,
    /// (b) `encode_text(decode_text(bytes)) == bytes` for the run.
    EncodingRoundTrip,
    /// (c) the font is not subset-flagged (`ABCDEF+`, partial glyphs).
    NotSubset,
    /// (d) the replacement fits the run's advance width at its font size.
    FitsWidth,
    /// (f) the content-stream run no longer matches the run the user picked (T4).
    RunMismatch,
    /// (g) the selected runs are not one contiguous, same-style span on one baseline
    /// (P2.6 T26).
    SelectionRange,
    /// (h) the run's text matrix is rotated or skewed, so its advance is not a horizontal
    /// width and the width gate would measure the wrong axis (P2.7 hotfix H0).
    RotatedText,
    /// (i) the text sits in a Form XObject this walk refused to enter: nested past the cap, or
    /// drawing itself (P2.7.3 Q1). Carries no text, so there is nothing to rewrite.
    NestedXObject,
}

impl Gate {
    /// Stable identifier used in messages and the frontend, so a refusal can be
    /// asserted on and, later, translated without matching prose.
    pub fn name(self) -> &'static str {
        match self {
            Gate::FontEncoding => "font-encoding",
            Gate::EncodingRoundTrip => "encoding-round-trip",
            Gate::NotSubset => "not-subset",
            Gate::FitsWidth => "fits-width",
            Gate::RunMismatch => "run-mismatch",
            Gate::SelectionRange => "selection-range",
            Gate::RotatedText => "rotated-text",
            Gate::NestedXObject => "nested-xobject",
        }
    }

    /// A short, calm sentence naming the gate and the way out.
    pub fn message(self) -> &'static str {
        match self {
            Gate::FontEncoding => {
                "this text uses a CID/Type0 font that publishes no /ToUnicode, so it cannot \
                 be read back reliably; select a narrower range, or use the annotation tools \
                 to add a note"
            }
            Gate::EncodingRoundTrip => {
                "this run's font encoding cannot reproduce its own bytes, so an edit \
                 could corrupt it; select a narrower range, or use the annotation tools to \
                 add a note"
            }
            Gate::SelectionRange => {
                "these runs are not one unbroken line of the same typeface, so they cannot \
                 be replaced together; select a single run instead"
            }
            Gate::NotSubset => {
                "this run's font is an embedded subset with only the glyphs the \
                 original used; select a narrower range, or use the annotation tools to \
                 add a note"
            }
            Gate::FitsWidth => "the replacement is wider than the space available",
            Gate::RunMismatch => {
                "this text is not addressable in the page's content stream (or it changed \
                 since it was selected); select a narrower range, or use the annotation \
                 tools to add a note"
            }
            Gate::RotatedText => {
                "this text is rotated or skewed, so its width cannot yet be measured reliably; \
                 select a narrower range, or use the annotation tools to add a note"
            }
            Gate::NestedXObject => {
                "this text sits in a form nested too deeply to edit, or in a form that draws \
                 itself; select a narrower range, or use the annotation tools to add a \
                 note"
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
    /// True when the run's text matrix was not axis-aligned (P2.7 hotfix H0).
    pub rotated: bool,
    /// Why this run is a Form XObject refusal placeholder (P2.7.3 X1), if it is one.
    pub nested_xobject: Option<XObjectRefusal>,
    /// A CID/Type0 font's `/ToUnicode` exists, so its codes can be read back (P2.8).
    /// Meaningless for a simple font.
    pub cid_readable: bool,
}

/// Resolves the font gates for a run.
///
/// An unresolvable font is not an error here: it is simply not editable, and the
/// caller reports that as a gate refusal. Only a genuinely unreadable document (no
/// page, no content) surfaces as an error.
pub fn resolve_gates(document: &Document, page: u32, run: &RawRun) -> Result<RunGates, AppError> {
    let page_id = page_id(document, page)?;
    // A placeholder run has no font — there is no text — and the refusal it carries is the
    // answer, so `nested-xobject` outranks every font gate including `simple-font` (X1).
    if run.xobject_refusal.is_some() {
        return Ok(RunGates {
            font: FontInfo {
                key: run.font_key.clone(),
                base_font: None,
                subtype: None,
            },
            round_trips: false,
            rotated: false,
            nested_xobject: run.xobject_refusal,
            cid_readable: false,
        });
    }
    // P2.7.3 §6: the run's font is resolved at the level the run is addressed in. A form
    // usually declares its own `/Font` and the page may not name that resource at all.
    let font = textlocate::font_info_at(document, page_id, &run.id.xobject_path, &run.font_key)
        .ok_or_else(|| {
            AppError::TextRunNotEditable(format!(
                "this run's font ({}) could not be resolved; select a narrower range, or use the \
                 annotation tools to add a note (gate: {})",
                String::from_utf8_lossy(&run.font_key),
                Gate::FontEncoding.name()
            ))
        })?;

    let round_trips =
        textlocate::font_dictionary_at(document, page_id, &run.id.xobject_path, &run.font_key)
            .map(|font| {
                font.get_font_encoding(document)
                    .map(|encoding| {
                        let text = encoding.bytes_to_string(&run.bytes).unwrap_or_default();
                        encoding.string_to_bytes(&text) == run.bytes
                    })
                    .unwrap_or(false)
            })
            .unwrap_or(false);

    // P2.8: a Type0 run's bytes are codes, so its own round-trip is decided by the CID
    // codec rather than by lopdf's one-byte encoding machinery.
    let cid_readable =
        crate::editor::cid::resolve_at(document, page_id, &run.id.xobject_path, &run.font_key)
            .map(|cid| cid.is_readable())
            .unwrap_or(false);

    Ok(RunGates {
        font,
        round_trips,
        rotated: run.rotated,
        nested_xobject: run.xobject_refusal,
        cid_readable,
    })
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
    // A rotated run is refused before anything else: its `advance` is not a horizontal width,
    // so no downstream measurement (width gate, PDFium pairing) can be trusted for it.
    // Font capability is irrelevant when the geometry is the thing that cannot be read.
    if gates.rotated {
        return Admissibility::refused(Gate::RotatedText);
    }
    // A placeholder is not text: there is no operand to rewrite, so the only honest answer is
    // that the text inside that form is out of reach (P2.7.3 Q1/§4.2).
    if let Some(reason) = gates.nested_xobject {
        return Admissibility::refused_with(Gate::NestedXObject, reason.message());
    }
    // (a) P2.8: a simple font is written through its own one-byte encoding; a Type0 is
    // written as two-byte codes through its `/ToUnicode` and (under Identity) its program.
    // Anything else cannot be read back at all, which is the only thing this gate refuses.
    let simple = is_simple_font(&gates.font);
    if !simple && !gates.cid_readable {
        return Admissibility::refused(Gate::FontEncoding);
    }
    // A simple font's round-trip is a prerequisite here. A CID run has no one-byte
    // round-trip to check: its bytes are codes, and the codec answers coverage per
    // replacement character when the write path encodes it.
    if simple && !gates.round_trips {
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
             select a narrower range, or use the annotation tools to add a note \
             (gate: {})",
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
    /// False when the run sits under an `/ActualText` that could not be updated with the edit
    /// (P2.7.1 T30), so the UI can warn that the document's accessible text still reads the
    /// old string. Always true for an untagged run.
    pub actual_text_preserved: bool,
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
     the replacement against; select a narrower range, or use the annotation tools to add a \
     note (gate: fits-width)";

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
    /// The run's own text as found in PDFium's **character grid**, and the span its characters
    /// occupy (P2.4.3 T23a). `None` when the text could not be located on the paired baseline —
    /// the walk is then unavailable and the quota falls through to the font metrics (T23d).
    ///
    /// This is the measurement the P2.4.3 field report needed: it works for a font whose program
    /// carries no readable metrics, because it is read from the *rendered* page.
    span: Option<CharSpan>,
    /// True when this box is the **smallest** box containing the run's origin (P2.4.3 T23c's
    /// clause 3). Only such a box may stand in for an unmeasurable span: a larger box that also
    /// contains the origin is a line the run only occupies part of.
    smallest_containing: bool,
}

/// The tolerance with which a grid character's pen origin may precede the run's text-space
/// origin and still be taken as that run's first character (P2.4.3 T23a).
///
/// Deliberately far tighter than [`origin_tolerance`], which exists to absorb a *box's* ink
/// offset. Here the two numbers are the same quantity measured twice — the text-space origin
/// lopdf computes from `Tm` and the pen origin PDFium reports — so only float rounding
/// separates them. Loosening this to the pairing tolerance would let a narrow glyph re-anchor
/// its neighbour's character as its own (an `l` advances only 2.66pt at 12pt, while the pairing
/// tolerance is 3pt), which reads as a large *overlap* at the seam and breaks a chain that is
/// genuinely whole. When the two origins disagree by more than this, the walk simply finds
/// nothing and the span falls back to the font metrics — the safe direction.
const ANCHOR_TOLERANCE: f32 = 0.1;

/// The baseline band a run's characters must sit in to count as that run's (P2.4.3 T23a).
///
/// Centred on the paired box's baseline and widened by the same font-size-relative tolerance
/// pairing uses, so the walk stays on one line: half a point would be too tight for a large
/// face and too loose for a small one, exactly as [`origin_tolerance`] argues for pairing.
fn grid_band(rect: &crate::editor::runmetrics::PdfiumRun, font_size: f32) -> (f32, f32) {
    let tol = origin_tolerance(font_size);
    (rect.y0 - tol, rect.y0 + tol)
}

/// Finds a run's text in PDFium's character grid, anchored at the run's own text-space origin
/// on the paired box's baseline (P2.4.3 T23a).
///
/// The anchor is what keeps a repeated word from measuring the *other* occurrence: the walk
/// starts at the first grid character at or after the run's origin, and every matched character
/// must also lie inside the paired box's x-window and the baseline band. Both anchors are
/// needed — the box window alone cannot separate `l` from the `l` beside it in one word, and the
/// origin alone cannot tell a run's line from the one above it.
fn locate_span(
    run: &RawRun,
    rect: &crate::editor::runmetrics::PdfiumRun,
    grid: &PdfiumCharGrid,
) -> Option<CharSpan> {
    let band = grid_band(rect, run.font_size);
    let window_left = rect.x0 - origin_tolerance(run.font_size);
    let anchor = window_left.max(run.origin.0 - ANCHOR_TOLERANCE);
    let from = grid
        .chars
        .iter()
        .position(|ch| (band.0..=band.1).contains(&ch.origin_y) && ch.origin_x >= anchor)
        .unwrap_or(0);
    grid.locate(&run.text, from, band, window_left)
}

/// Reads one baseline per box, once, for a whole page (P2.4.4 T24).
///
/// [`pair_run`] is called once per lopdf run, and each call compares against every box. Deriving
/// the box's rendered baseline from the character grid is a scan of the grid, so it is done once
/// here and handed in rather than recomputed per run.
fn box_baselines(
    rects: &[crate::editor::runmetrics::PdfiumRun],
    grid: &PdfiumCharGrid,
) -> Vec<f32> {
    rects
        .iter()
        .map(|rect| rendered_baseline(rect, grid))
        .collect()
}

/// Matches one lopdf run to a PDFium box by **geometry** (P2.4.1 T17), and walks its text in
/// the character grid for its own measured span (P2.4.3 T23a).
///
/// Text equality is demoted to a tiebreaker (P2.4.1 T17): the two libraries segment text
/// differently — PDFium joins a per-glyph `TJ` array into one run, lopdf splits it into one
/// run per operand — so matching on the reconstructed string fails for exactly the documents
/// this hotfix exists for. Position on the page does not care how either side split the line.
///
/// `baselines` carries each box's **rendered** baseline (P2.4.4 T24), read from the grid rather
/// than assumed to be the box's bottom edge, so a line with a descender still pairs strong.
fn pair_run(
    run: &RawRun,
    rects: &[crate::editor::runmetrics::PdfiumRun],
    baselines: &[f32],
    grid: &PdfiumCharGrid,
) -> Option<Pair> {
    let tol = origin_tolerance(run.font_size);
    // Every vertical comparison uses the **ink** baseline, not the pen: `Ts` moves the drawn
    // glyphs without moving the pen, so a superscript's pen sits below its own box (B-5).
    let ink_y = run.baseline();

    let mut best: Option<Pair> = None;
    let mut best_key: Option<(bool, f32, f32, f32, bool)> = None;
    for (index, rect) in rects.iter().enumerate() {
        // `contains` uses the box grown by the tolerance, which absorbs the ink offset and a
        // small metric disagreement without reaching a neighbouring line.
        let contains = run.origin.0 >= rect.x0 - tol
            && run.origin.0 <= rect.x1 + tol
            && ink_y >= rect.y0 - tol
            && ink_y <= rect.y1 + tol;
        if !contains {
            continue;
        }
        let baseline = baselines.get(index).copied().unwrap_or(rect.y0);
        // `baselines` is the *rendered* baseline from the grid, so it already includes `Ts`.
        // Comparing the pen position instead would make every raised run a weak pair (B-5).
        let baseline_delta = (ink_y - baseline).abs();
        let strong = baseline_delta <= BASELINE_TOLERANCE;
        // Prefer strong over weak, then the smallest baseline delta, then the box the origin
        // sits deepest inside — and, per §3 step 2, exact text equality as the *final*
        // tiebreaker when two boxes agree on all of it. Text is only ever a tiebreaker here: it
        // never decides a pair the geometry already separated, which is the whole point of T17.
        //
        // Depth is measured as how far the origin falls outside the box (0 when inside), not as
        // the leftmost edge. The tolerance above deliberately reaches past a box's border, so at
        // a seam two boxes can both contain the origin, and leftmost then hands the run to the
        // *previous* box: `O` of `Overview` opens 0.48pt left of the ink of its own box and
        // 3.19pt past the bold box the line's earlier operation drew, which split the whole
        // tail of the line into singletons. Only when both boxes contain the origin equally
        // (a word box inside a line box) does the leftmost edge still decide, as before.
        let overshoot = (rect.x0 - run.origin.0)
            .max(run.origin.0 - rect.x1)
            .max(rect.y0 - ink_y)
            .max(ink_y - rect.y1)
            .max(0.0);
        let key = (
            !strong,
            baseline_delta,
            overshoot,
            rect.x0,
            run.text != rect.text,
        );
        if best_key.map_or(true, |current| key < current) {
            best_key = Some(key);
            best = Some(Pair {
                rect: index,
                confidence: if strong {
                    PairConfidence::Strong
                } else {
                    PairConfidence::Weak
                },
                span: locate_span(run, rect, grid),
                smallest_containing: smallest_containing(run, index, rects, tol),
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
        let dy = (rect.y0 - ink_y).max(ink_y - rect.y1).max(0.0);
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
            span: rects
                .get(rect)
                .and_then(|box_| locate_span(run, box_, grid)),
            smallest_containing: smallest_containing(run, rect, rects, tol),
        })
}

/// Locates one PDFium text object's **character window** in the grid (P2.4.4 T24).
///
/// The window is the contiguous range of grid indices whose non-generated characters spell the
/// object's own text, searched inside the object's box — the same baseline band and x-window
/// pairing uses ([`PdfiumCharGrid::locate`]). It is the coordinate system T24 assigns fragments
/// in: a fragment's true span is the union of the window's characters its decoded text occupies,
/// never the drifting text-space pen.
///
/// `locate` normalises whitespace on both sides, so a trailing space the object actually drew is
/// trimmed from its match. The window therefore extends forward over any further non-generated
/// character still inside the box (the pen has not left it) — that is where the trailing space
/// lives, and a fragment that ends in a space must be able to claim it.
fn object_window(
    rect: &crate::editor::runmetrics::PdfiumRun,
    grid: &PdfiumCharGrid,
) -> Option<(usize, usize)> {
    let tol = origin_tolerance(rect.font_size);
    let band = (rect.y0 - tol, rect.y1 + tol);
    let span = grid.locate(&rect.text, 0, band, rect.x0 - tol)?;
    let mut last = span.last;
    while let Some(next) = grid.chars.get(last + 1) {
        if next.generated || next.unicode.is_none() {
            break;
        }
        if !(band.0..=band.1).contains(&next.origin_y) || next.x0 > rect.x1 + tol {
            break;
        }
        last += 1;
    }
    Some((span.first, last))
}

/// Assigns every fragment the union of its object's characters, by character alignment (T24).
///
/// Each lopdf fragment is one operand of a `TJ`/`Tj` the producer emitted, so its decoded text is
/// a slice of the object's text: within one object the fragments are consecutive and their texts
/// concatenate to PDFium's reconstruction. The walk therefore takes the object's window and hands
/// each fragment, in order, exactly as many characters as its decoded text has — the same ToUnicode
/// CMap decoded both sides, so the lengths agree — and measures the fragment's span as the union of
/// those characters' advance boxes.
///
/// This is the T24 fix: the previous span anchored the walk on the run's **text-space origin**,
/// which a metrics-less font drifts left cumulatively, so a long fragment matched the wrong grid
/// characters. Aligning on the object's own characters removes the drifting quantity entirely.
///
/// PDFium-generated characters (a synthesised space at a kern seam, a `\r\n`) occupy grid slots but
/// no operand's bytes, so they are skipped and never assigned. A fragment whose characters do not
/// match the grid's in order — the two CMaps disagree for that operand — keeps the T23a
/// `locate_span` result: the safe direction, and no behaviour change for the mismatched fragment.
/// The cursor advances past the characters the attempt consumed either way, so a single mismatch
/// cannot knock every later fragment out of alignment.
fn assign_object_spans(
    runs: &[RawRun],
    pairs: &mut [Option<Pair>],
    rects: &[crate::editor::runmetrics::PdfiumRun],
    grid: &PdfiumCharGrid,
) {
    for (rect_index, rect) in rects.iter().enumerate() {
        let members: Vec<usize> = (0..runs.len())
            .filter(|index| pairs[*index].is_some_and(|pair| pair.rect == rect_index))
            .collect();
        if members.is_empty() {
            continue;
        }
        let Some((first, last)) = object_window(rect, grid) else {
            continue;
        };

        // The window's assignable slots: characters that occupy bytes (generated ones do not).
        let slots: Vec<usize> = (first..=last)
            .filter(|index| {
                let ch = &grid.chars[*index];
                ch.unicode.is_some() && !ch.generated
            })
            .collect();

        let mut cursor = 0_usize;
        for &run_index in &members {
            let expected: Vec<char> = runs[run_index].text.chars().collect();
            if expected.is_empty() || cursor + expected.len() > slots.len() {
                continue;
            }
            let assigned = &slots[cursor..cursor + expected.len()];

            let identified = assigned
                .iter()
                .zip(expected.iter())
                .all(|(index, ch)| grid.chars[*index].unicode == Some(*ch));
            if !identified {
                // The fragment's text is not at this cursor: it was drawn before this box
                // opened (an operand straddling from the preceding box), so it consumes none
                // of *this* box's characters. Advancing anyway would shift every later member
                // by its length and leave them unmeasured — the Maastricht `Fundamental `
                // bug, where one straddling `ce. ` hid the whole next operand.
                continue;
            }
            cursor += expected.len();
            let x0 = assigned
                .iter()
                .map(|index| grid.chars[*index].x0)
                .fold(f32::INFINITY, f32::min);
            let x1 = assigned
                .iter()
                .map(|index| grid.chars[*index].x1)
                .fold(f32::NEG_INFINITY, f32::max);
            if let Some(pair) = pairs[run_index].as_mut() {
                pair.span = Some(CharSpan {
                    first: assigned[0],
                    last: *assigned.last().expect("expected is non-empty"),
                    x0,
                    x1,
                });
            }
        }
    }
}

/// Re-derives spans for operations whose per-box alignment came out out-of-order (P2.5 T25b).
///
/// T24 aligns a box's members against that box's character window, which is exact only while
/// every operand of the box's operation stayed inside it. On a metrics-less face the producer
/// emits one `TJ` whose operands run past their box into the next one on the line, so a
/// line-sized box's window can also hold characters the operation never drew; an operand then
/// takes a span from the wrong part of the line. `Fundamental ` went unmeasured that way, and
/// the `concepts of pro` pick ended at the wrong glyph.
///
/// The operation is the reliable unit: its operands are consecutive in the content stream
/// *and* on the page, so their concatenated text locates in the grid as one contiguous run.
/// Only operations that are straddled are rewritten — their per-box spans are either missing
/// or not strictly increasing — so every ordinary line keeps T24's exact per-box result.
fn resync_straddled_op_spans(
    runs: &[RawRun],
    pairs: &mut [Option<Pair>],
    rects: &[crate::editor::runmetrics::PdfiumRun],
    grid: &PdfiumCharGrid,
) {
    let mut index = 0_usize;
    while index < runs.len() {
        let op_index = runs[index].id.op_index;
        let mut end = index;
        while end < runs.len() && runs[end].id.op_index == op_index {
            end += 1;
        }
        // The test below is the whole gate. Having metrics does not make T24's per-box
        // assignment exact: a `TJ` operand that runs past its box can still be matched to a
        // later box's characters — the Maastricht `o` of `operating` took the span of an `o`
        // 94pt to its left even though its font published `/Widths`.
        if end - index < 2 {
            index = end;
            continue;
        }
        let ordered = runs[index..end]
            .iter()
            .zip(&pairs[index..end])
            .all(|(_, pair)| match pair {
                None => true,
                Some(pair) => match pair.span {
                    None => false,
                    Some(span) => span.x1 >= span.x0,
                },
            });
        let monotonic = pairs[index..end]
            .iter()
            .filter_map(|pair| pair.and_then(|pair| pair.span))
            .zip(
                pairs[index..end]
                    .iter()
                    .filter_map(|pair| pair.and_then(|pair| pair.span))
                    .skip(1),
            )
            .all(|(head, next)| next.x0 >= head.x0);
        if ordered && monotonic {
            index = end;
            continue;
        }

        let text: String = runs[index..end]
            .iter()
            .map(|run| run.text.as_str())
            .collect();
        let Some(anchor) = (index..end).find(|run_index| pairs[*run_index].is_some()) else {
            index = end;
            continue;
        };
        let Some((rect, font_size)) = pairs[anchor]
            .and_then(|pair| rects.get(pair.rect))
            .map(|rect| (rect, runs[anchor].font_size))
        else {
            index = end;
            continue;
        };
        if text.trim().is_empty() {
            index = end;
            continue;
        }
        let band = grid_band(rect, font_size);
        // The window opens where the operation starts drawing, not where its box starts: a
        // line-sized box reaches back to the line's margin, and locating from there would find
        // an earlier occurrence of the same text. A leading whitespace operand has no ink to
        // place it, so the first inked operand anchors.
        let Some(op_left) = (index..end)
            .find(|run_index| {
                !runs[*run_index].text.trim().is_empty()
                    && runs[*run_index].origin.0 >= rect.x0 - origin_tolerance(font_size)
            })
            .map(|run_index| runs[run_index].origin.0)
        else {
            index = end;
            continue;
        };
        let window_left = op_left - origin_tolerance(font_size);
        let from = grid
            .chars
            .iter()
            .position(|ch| (band.0..=band.1).contains(&ch.origin_y) && ch.origin_x >= window_left)
            .unwrap_or(0);
        let Some(span) = grid.locate(&text, from, band, window_left) else {
            index = end;
            continue;
        };

        // Generated characters (a synthesised seam space, a line break) occupy grid slots but
        // no operand's bytes, so they are skipped — the same rule T24 applies.
        let mut slots: Vec<usize> = (span.first..=span.last)
            .filter(|slot| {
                let ch = &grid.chars[*slot];
                ch.unicode.is_some() && !ch.generated
            })
            .collect();
        // `locate` trims the needle's trailing whitespace, so an operation ending in a space
        // operand locates only through its last visible glyph. That operand still needs its
        // slot or the group's width falls back to the line-sized box.
        let trailing = text
            .chars()
            .rev()
            .take_while(|ch| ch.is_whitespace())
            .count();
        let total: usize = runs[index..end]
            .iter()
            .map(|run| run.text.chars().count())
            .sum();
        let (tail_lo, tail_hi) = grid_band(rect, font_size * 2.0);
        let mut next = span.last + 1;
        let mut added = 0;
        while added < trailing.min(total.saturating_sub(slots.len())) && next < grid.chars.len() {
            let ch = &grid.chars[next];
            if !(tail_lo..=tail_hi).contains(&ch.origin_y) {
                break;
            }
            if ch.unicode.is_some() && !ch.generated {
                slots.push(next);
                added += 1;
            }
            next += 1;
        }

        let mut cursor = 0_usize;
        for run_index in index..end {
            let expected: Vec<char> = runs[run_index].text.chars().collect();
            if expected.is_empty() || cursor + expected.len() > slots.len() {
                continue;
            }
            let assigned = &slots[cursor..cursor + expected.len()];
            let identified = assigned
                .iter()
                .zip(expected.iter())
                .all(|(slot, ch)| grid.chars[*slot].unicode == Some(*ch));
            if !identified {
                continue;
            }
            cursor += expected.len();
            let x0 = assigned
                .iter()
                .map(|slot| grid.chars[*slot].x0)
                .fold(f32::INFINITY, f32::min);
            let x1 = assigned
                .iter()
                .map(|slot| grid.chars[*slot].x1)
                .fold(f32::NEG_INFINITY, f32::max);
            if let Some(pair) = pairs[run_index].as_mut() {
                pair.span = Some(CharSpan {
                    first: assigned[0],
                    last: *assigned.last().expect("expected is non-empty"),
                    x0,
                    x1,
                });
            }
        }

        index = end;
    }
}

/// The baseline PDFium rendered a box's text on, read from the character grid (P2.4.4 T24).
///
/// A box's `y0` is its **bottom edge**, and for any line containing a descender (`y`, `g`, `p`)
/// that edge sits *below* the baseline by the descender depth — `3.39pt` on the Maastricht line
/// at 15pt, because `versity` has a `y`. The baseline a pair must agree with is not that edge:
/// it is the pen origin PDFium reports for the line's characters, and every character on one
/// baseline shares it exactly (P2.4.3 T23a's grid). Reading it from the grid is the same
/// rendered-oracle rule T23/T24 use for spans.
///
/// Before this, `rect.y0` stood in for the baseline, which is exact only for a descender-free
/// line. On a line with a descender every fragment paired *weak* — the baseline "disagreement"
/// was the descender — and [`group_runs`] merges strong pairs only, so the line could not merge
/// however exact its spans were. When the grid carries nothing in the box's window the bottom
/// edge still stands in, so an in-memory test with an empty grid behaves exactly as before.
fn rendered_baseline(rect: &crate::editor::runmetrics::PdfiumRun, grid: &PdfiumCharGrid) -> f32 {
    let tol = origin_tolerance(rect.font_size);
    // The modal pen origin of the box's own characters. Characters from the line above or below
    // sit in a different band and are excluded, so one line yields one baseline.
    let mut counts: Vec<(f32, usize)> = Vec::new();
    for ch in &grid.chars {
        if ch.generated || ch.unicode.is_none() {
            continue;
        }
        if ch.x0 < rect.x0 - tol || ch.x0 > rect.x1 + tol {
            continue;
        }
        if !(rect.y0 - tol..=rect.y1 + tol).contains(&ch.origin_y) {
            continue;
        }
        match counts
            .iter_mut()
            .find(|(value, _)| (*value - ch.origin_y).abs() < 0.01)
        {
            Some((_, count)) => *count += 1,
            None => counts.push((ch.origin_y, 1)),
        }
    }
    counts
        .into_iter()
        .reduce(|best, current| {
            if current.1 > best.1 || (current.1 == best.1 && current.0 < best.0) {
                current
            } else {
                best
            }
        })
        .map(|(value, _)| value)
        .unwrap_or(rect.y0)
}

/// True when `index` is the **smallest** box that contains the run's text-space origin.
///
/// T23c's clause 3 trusts a box only when it is the smallest one containing the run: on a line
/// of several boxes, the one a run's origin falls in and that no other containing box undercuts
/// is the run's own, while a box that several others nest inside is a line. Ties (two boxes the
/// same size) resolve to the first, which is the leftmost by input order.
fn smallest_containing(
    run: &RawRun,
    index: usize,
    rects: &[crate::editor::runmetrics::PdfiumRun],
    tol: f32,
) -> bool {
    let Some(candidate) = rects.get(index) else {
        return false;
    };
    let width = candidate.x1 - candidate.x0;
    !rects.iter().enumerate().any(|(other_index, other)| {
        other_index != index
            && run.origin.0 >= other.x0 - tol
            && run.origin.0 <= other.x1 + tol
            && run.baseline() >= other.y0 - tol
            && run.baseline() <= other.y1 + tol
            && (other.x1 - other.x0) < width
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

/// Whether a kerning seam between two fragments is a **word space** (P2.6 T26 item 4).
///
/// `space_at_boundary` can only see a space inside a *shared* box, which is exactly the case a
/// kern seam is not: PDFium reconstructs the space as a **generated** character precisely
/// because no single run drew one. This reads the character grid instead — the equivalent of
/// Acrobat's `ExplicitSpaceChars` reading-order repair.
///
/// A seam is a word space when the grid holds a generated space in the one slot between the
/// head's last character and the next fragment's first, and that generated character carries no
/// advance of its own. PDFium reports a kern-reconstructed space that way when it renders the
/// page as `foo bar`; `-400` and `-600` kerns are indistinguishable in the content stream, so
/// the renderer's reconstruction is the only evidence there is.
///
/// Several things this deliberately does **not** treat as a word space:
/// * a generated `\r\n` — a line break, not a space between words;
/// * a generated space with a non-zero advance — PDFium keeps a kern seam at zero advance, so
///   a positive advance would be a space it laid out itself, and inserting one would double it;
/// * a *narrow* seam. A compressed face whose glyphs advance unevenly also makes PDFium emit
///   generated spaces between letters of one word (`m o n th`); the word-space signal is the
///   gap size, so the seam must be at least a quarter em, the same bound O-Chain-2 uses for a
///   kerned word break;
/// * a seam either operand already brackets with whitespace, which would otherwise double it.
fn seam_is_word_space(
    head_index: usize,
    next_index: usize,
    runs: &[RawRun],
    pairs: &[Option<Pair>],
    grid: &crate::editor::runmetrics::PdfiumCharGrid,
) -> bool {
    let (Some(head_span), Some(next_span)) = (
        pairs[head_index].and_then(|pair| pair.span),
        pairs[next_index].and_then(|pair| pair.span),
    ) else {
        return false;
    };
    if next_span.first <= head_span.last {
        return false;
    }
    // Only the slots strictly between the two spans can hold the seam. Requiring exactly one
    // keeps a run of generated characters from being collapsed into a single space.
    let between = &grid.chars[head_span.last + 1..next_span.first];
    if !(between.len() == 1
        && between[0].generated
        && between[0].unicode == Some(' ')
        && (between[0].x1 - between[0].x0).abs() < 0.01)
    {
        return false;
    }
    if runs[head_index].text.ends_with(char::is_whitespace)
        || runs[next_index].text.starts_with(char::is_whitespace)
    {
        return false;
    }
    // The seam's advance is the gap between the two measured spans.
    let seam = next_span.x0 - head_span.x1;
    let em = runs[head_index].font_size.max(f32::EPSILON);
    seam >= 0.25 * em
}

/// The gap between two consecutive fragments in points, measured the most exact way available
/// (P2.4.3 T23b).
///
/// * **Char-grid** — when both fragments were located in PDFium's character grid on this page,
///   the gap is `next.x0 − head.x1`, read off the rendered page. This is exact for *every* font
///   class, including the PowerPoint subsets whose program carries no metrics, and it replaces
///   the approximation wherever it is available.
/// * **In-file metrics** — otherwise the head's own exact advance (`advance_exact`, P2.4.2
///   Phase A) when the font has one.
/// * **Approximation** — last resort, exactly as P2.4.2 left it.
fn measured_gap(
    head: &RawRun,
    next: &RawRun,
    head_span: Option<CharSpan>,
    next_span: Option<CharSpan>,
) -> f32 {
    match (head_span, next_span) {
        (Some(head_span), Some(next_span)) => next_span.x0 - head_span.x1,
        _ => {
            let advance = head.advance_exact.unwrap_or(head.advance);
            next.origin.0 - (head.origin.0 + advance)
        }
    }
}

/// Whether two consecutive fragments may be merged into one run (P2.4.1 T19 / P2.4.2 / P2.4.3).
///
/// All conditions are required: same font, a size that agrees within the *relative* tolerance
/// ([`sizes_match`]), the same baseline (0.5pt), and a horizontal gap that is
///
/// * at most `0.5em` when PDFium's reconstructed text carries a real space at the seam, else
///   at most `0.25em` ([`gap_tolerance`]); and
/// * no more overlapping than `-0.05em` ([`MERGE_OVERLAP_EM`]) — a larger overlap is a
///   mis-measure, and joining across it would let two fragments' bytes collide.
///
/// The gap itself comes from [`measured_gap`]: char-grid first (P2.4.3 T23b), then the exact
/// in-file advance, then the approximation. This is the chain-break fix in both senses: the
/// 0.4.1 approximation inflated the head's advance and pushed the seam negative, and the
/// approximation was the *only* source for a font with no metrics — which is why the Maastricht
/// line could not remerge even after P2.4.2.
fn mergeable(
    head: &RawRun,
    next: &RawRun,
    space: bool,
    head_span: Option<CharSpan>,
    next_span: Option<CharSpan>,
) -> bool {
    head.style_class == next.style_class
        // P2.8 §1.1(a): the same face, not merely the same style. `style_match` is coarse on
        // purpose (a producer's `F4`→`F5` weight swap must not split a line), so it also calls
        // `ArialMT`, `ABCDEE+Monaco` and `ABCDEE+Wingdings-Regular` all `Helvetica`. A merged
        // write draws everything through the head's font, so merging across faces would render
        // one face's text in another's glyphs and delete the rest.
        && head.face == next.face
        // P2.8: a merged write puts the whole replacement in the head fragment and clears the
        // rest, drawing all of it through the head's font. So a fragment whose font cannot draw
        // arbitrary text is not a write target, whatever other fragments share its style — the
        // Maastricht line's `F3` Monaco and `F6` Monaco/Identity-H are one style class and two
        // different codecs, and the Wingdings `F7` icon is a third.
        && head.composable
        && next.composable
        // P2.7.3 X1: a placeholder is not text, and two of them are not one run. Nor may a
        // run inside a form merge with one on the page: they have different addresses, so a
        // single write could not clear both.
        && head.xobject_refusal.is_none()
        && next.xobject_refusal.is_none()
        && head.id.xobject_path == next.id.xobject_path
        // P2.7.1 T27/T28: a tagged unit is a boundary the document itself drew. Two fragments
        // in different `MCID`s are not one editable run, however perfectly their geometry
        // abuts -- a table cell beside its neighbour, a caption under its figure. Untagged
        // pages compare `None == None` and are unchanged.
        && head.merge_scope() == next.merge_scope()
        && sizes_match(head.font_size, next.font_size)
        // P2.7.1 B-5: `Ts` moves the ink without moving the pen, so the baseline a merge
        // compares is `origin.1 + rise`. Two runs with different rises are on different lines
        // even when their pens agree.
        && (head.baseline() - next.baseline()).abs() <= BASELINE_TOLERANCE
        && {
            let gap = measured_gap(head, next, head_span, next_span);
            gap <= gap_tolerance(head.font_size, space) && gap >= -MERGE_OVERLAP_EM * head.font_size
                // P2.6 T26 reading order: a `TJ` kern moves the pen without appearing in
                // `/Widths`, so the measured seam shows a *negative* gap that is not a
                // mis-measure — the Maastricht `T`/`est` seam is exactly its `-1.227pt` kern.
                // When the grid also places the next glyph in the very next slot, the two are
                // one laid-out line. Bounded by the same window as a positive gap, so a real
                // overlap still refuses, and never applied to a positive gap, where the
                // classic window is already the whole answer.
                || (gap < 0.0
                    && gap >= -gap_tolerance(head.font_size, space)
                    && reads_on_from_grid(head_span, next_span))
                || {
                    // P2.5 T25: both fragments already carry char-grid spans (P2.4.4 T24) and
                    // sit on one exact baseline — the grid re-adjudicated this seam when it
                    // refit the fragments with per-character advances. Re-measuring the seam
                    // with the font's own (absent) metrics returns a phantom gap far outside
                    // any window — the Maastricht `an` bug measured 55pt between two glyphs
                    // PDFium drew 6.5pt apart — so the remaining question is whether the chain
                    // still runs rightward: the next span must not sit left of the head's.
                    // Only a metrics-less seam takes this exit: a measurable seam is already
                    // adjudicated by the classic window above (the Maastricht font publishes
                    // no metrics; the T22 orphan's embedded subset does, and its deliberate
                    // kern keeps its protection).
                    // P2.6 T26: bounded by one em. The escape exists for a grid that
                    // mis-measures a seam, not for a seam that is genuinely far apart — the
                    // bullet column of a list sits ~1.5em left of its text, and joining it
                    // would make the marker part of the user's sentence.
                    head.advance_exact.is_none()
                        && match (head_span, next_span) {
                            (Some(head_span), Some(next_span)) => {
                                next_span.x0 >= head_span.x0
                                    && next_span.x0 - head_span.x1 <= head.font_size
                            }
                            _ => false,
                        }
                }
        }
}

/// True when the character grid places the next fragment's first glyph immediately after the
/// head's last one — at most one slot between them, which is the generated space PDFium
/// inserts at a kern (P2.6 T26).
fn reads_on_from_grid(head_span: Option<CharSpan>, next_span: Option<CharSpan>) -> bool {
    match (head_span, next_span) {
        (Some(head_span), Some(next_span)) => {
            next_span.first > head_span.first
                && next_span.first <= head_span.last + 2
                && next_span.x0 >= head_span.x0
                && next_span.x1 > head_span.x1
        }
        _ => false,
    }
}

/// A group of consecutive fragments the picker presents as one run (P2.4.1 T18).
struct RunGroup {
    first: usize,
    len: usize,
    /// For each seam after the head, whether PDFium reconstructed a **word space** there
    /// (P2.6 T26 item 4). `len - 1` entries, indexed by the fragment to the left of the seam.
    /// Empty means "no seam carries a space", the ordinary case.
    seams: Vec<bool>,
}

impl RunGroup {
    fn new(first: usize, len: usize) -> Self {
        Self {
            first,
            len,
            seams: Vec::new(),
        }
    }
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
    grid: &crate::editor::runmetrics::PdfiumCharGrid,
) -> Vec<RunGroup> {
    let mut groups = Vec::new();
    let mut index = 0;
    while index < runs.len() {
        // An unpaired run is still *reported* — the picker must show that it exists and name
        // why it cannot be edited (T21) — it simply cannot merge with anything.
        let mut len = 1;
        let mut seams: Vec<bool> = Vec::new();
        if pairs[index].is_some() {
            while index + len < runs.len() {
                let next = pairs[index + len];
                let head_span = pairs[index + len - 1].and_then(|pair| pair.span);
                let ready = match next {
                    Some(pair) => match pair.confidence {
                        PairConfidence::Strong => true,
                        // P2.5 T25: a fragment the grid paired only `Weak`ly may still extend
                        // the chain when the grid's own evidence is complete — it located the
                        // fragment's text inside its smallest box, contiguously after the
                        // head's span. On a metrics-less font this is the *only* pairing the
                        // tail of a line gets (the Maastricht `an`), and refusing it strands
                        // the user's text as an editable singleton mid-line.
                        PairConfidence::Weak => {
                            // The T22 orphan — a tail the grid paired to a line-sized box
                            // when the font *has* metrics — keeps O1's protection: its Weak
                            // demotion is the refusal the width gate stands on, so a
                            // measurable head never extends onto it. On a metrics-less font
                            // (no font metrics at all) the grid chain is the only measurement
                            // there is, and refusing it strands the user's text (T25).
                            runs[index + len - 1].advance_exact.is_none()
                                && pair.smallest_containing
                                && match (head_span, pair.span) {
                                    (Some(head_span), Some(span)) => {
                                        span.first > head_span.first
                                            && (head_span.last + 1..=head_span.last + 2)
                                                .contains(&span.first)
                                    }
                                    _ => false,
                                }
                        }
                    },
                    None => false,
                };
                if !ready {
                    break;
                }
                // O-Chain-2: the widened gap is allowed only when PDFium's reconstructed text
                // has a real space at this seam. That space hides in a *shared* box for a
                // producer that drew it as a `0x20`; at a kern seam there is no shared box, so
                // the grid's generated space is the equivalent evidence (T26 item 4).
                // Two distinct questions, and keeping them apart matters. `widened` says the
                // seam may use the wide gap window (O-Chain-2); `word_space` says the page
                // shows two words here, which also earns a separator and the seam's width.
                // The shared-box predicate is true all over an ordinary line -- one PDFium box
                // per line contains a space somewhere -- so it must never add a separator.
                let widened = space_at_boundary(index + len - 1, index + len, pairs, rects);
                let word_space =
                    seam_is_word_space(index + len - 1, index + len, runs, pairs, grid);
                // P2.4.3 T23b: the gap is char-grid measured when both spans are available.
                let head_span = pairs[index + len - 1].and_then(|pair| pair.span);
                let next_span = next.and_then(|pair| pair.span);
                if !mergeable(
                    &runs[index + len - 1],
                    &runs[index + len],
                    widened || word_space,
                    head_span,
                    next_span,
                ) {
                    break;
                }
                seams.push(word_space);
                len += 1;
            }
        }
        groups.push(RunGroup {
            first: index,
            len,
            seams,
        });
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
/// (P2.4.2 T22 / P2.4.3 T23a).
///
/// A group is a set of fragments laid end to end, so its span is the sum of their spans (their
/// own advances, not the box they happen to share) — for the ordinary single-fragment case this
/// is just that fragment's span.
///
/// **Char-grid first** (P2.4.3 T23a): each fragment's span is the union of its own characters'
/// advance boxes, read off the rendered page. That sum is measured for every font class,
/// including the embedded subsets whose program carries no metrics — which is the Maastricht
/// case that previously had no measurable span at all. Only when some non-empty fragment could
/// not be located in the grid do the per-fragment **in-file** metrics take over, and only when a
/// fragment has neither does the group report no span, exactly as P2.4.2 did (T23d).
///
/// The union of the group's *box* is never used: a box may be a whole line the group only
/// occupies part of, which is the 0.4.1 hole.
///
/// True when the group's union box is wide because **the group's own chain** left fragments
/// behind in it (P2.5 T25).
///
/// A metrics-less line keeps its `Strong` prefix together while the grid paired its tail only
/// weakly (or not at all); the renderer still drew that tail in the box, so the box outruns
/// the group by exactly the text still to come from the same operation. The fragments after
/// the group — up to the end of the operation, stopping at the next `Strong`-paired fragment
/// (a chain that stopped on merit is the classic T22 case and keeps its refusal) — identify
/// the case: when each is unpaired or a `Weak` pair, the box is the line the group belongs to,
/// not a neighbour's ink.
fn shared_box_is_own_chain(runs: &[RawRun], pairs: &[Option<Pair>], group: &RunGroup) -> bool {
    let last = group.first + group.len;
    let own_tail: Vec<_> = runs[last..]
        .iter()
        .zip(pairs[last..].iter())
        .take_while(|(run, _)| run.id.op_index == runs[group.first].id.op_index)
        .collect();
    // A group the operation *ends* with has no tail to explain a line-sized box: whatever the
    // box is, it is not the group's chain continuing — the suspicion stands (T22 orphan).
    if own_tail.is_empty() {
        return false;
    }
    own_tail.iter().all(|(_, pair)| match pair {
        None => true,
        Some(pair) => pair.confidence == PairConfidence::Weak,
    })
}

fn group_span(
    document: &Document,
    page: u32,
    runs: &[RawRun],
    pairs: &[Option<Pair>],
    group: &RunGroup,
) -> Option<f32> {
    let mut grid_span = 0.0_f32;
    let mut grid_measurable = false;
    let mut grid_complete = true;

    let mut metric_span = 0.0_f32;
    let mut metric_complete = true;
    let mut metric_measurable = false;

    for offset in 0..group.len {
        let run = &runs[group.first + offset];
        // An emptied tail fragment (a cleared operand) contributes nothing and needs no metric;
        // its bytes may be unmapped once the head carries the whole replacement.
        if run.text.is_empty() {
            continue;
        }
        match pairs[group.first + offset].and_then(|pair| pair.span) {
            Some(span) => {
                grid_span += span.width();
                // P2.6 T26 item 4: consecutive spans abut, so summing them drops the seam
                // itself. When PDFium reconstructed a word space at this seam, the page's own
                // advance across it is real width the quota must include -- otherwise the gate
                // measures a narrower line than the reader sees and accepts a replacement the
                // page cannot hold.
                if offset > 0 && group.seams.get(offset - 1).copied().unwrap_or(false) {
                    if let Some(head) = pairs[group.first + offset - 1].and_then(|pair| pair.span) {
                        grid_span += span.x0 - head.x1;
                    }
                }
                grid_measurable = true;
            }
            None => grid_complete = false,
        }

        // The in-file metric is only needed in the fallback, but is cheap and is also what the
        // suspicion test compares against, so it is resolved for every non-empty fragment. A
        // single fragment with no metric makes the *sum* unusable, so the flag is only ever
        // cleared, never re-set by a later fragment (which would leave a partial sum).
        match measured_span(document, page, run) {
            Some(width) => {
                metric_span += width;
                metric_measurable = true;
            }
            None => metric_complete = false,
        }
    }

    if grid_measurable && grid_complete {
        return Some(grid_span);
    }
    if metric_measurable && metric_complete {
        return Some(metric_span);
    }
    None
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
    // P2.6 T26 item 4: a seam PDFium reconstructed as a word space is shown as one. The
    // fragments' bytes have no `0x20` there (the space lived in the kern), but the page reads
    // as two words, and the picker must agree with the page. Only the display text gains the
    // separator -- `EditRequest::expected` stays the operands' bytes, because that is what the
    // write path will find in the content stream and re-validate.
    let joined: String = fragments
        .iter()
        .enumerate()
        .map(|(offset, fragment)| {
            if offset > 0 && group.seams.get(offset - 1).copied().unwrap_or(false) {
                format!(" {}", fragment.text)
            } else {
                fragment.text.clone()
            }
        })
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

    // P2.4.2 T22 / P2.4.3 T23c: the width quota walks a preference order — the char-grid span,
    // then the font-metric span, then the box for a strong pair whose box is ≈ the span it
    // measures (the ordinary merged run), then refusal. Nothing is weakened: a box is only ever
    // trusted when it agrees with a real span, so a box that may be a whole line never supplies
    // the number the gate compares against. A measurably line-sized box is *box-suspicious*:
    // flagged weak, its quota its own span, so a replacement too wide for the run refuses quoting
    // that span rather than the line.
    let span = group_span(document, page, runs, pairs, group);
    let head_pair = pairs[group.first];
    let box_width = if head_pair.is_some_and(|pair| pair.confidence == PairConfidence::Strong)
        && shared_box_is_own_chain(runs, pairs, group)
    {
        // P2.5 T25: the union box is the group's own line even though the renderer parked tail
        // fragments the merge gates left outside on top of it. Feeding the line-sized union to
        // the quota would flag the merged run suspicious (and demote it to weak) for ink that
        // is not a neighbouring line's — so the box never enters the quota comparison.
        None
    } else {
        rect.map(crate::editor::geometry::PdfRect::width)
    };
    let group_strong = head_pair.is_some_and(|pair| pair.confidence == PairConfidence::Strong);
    let smallest_containing = head_pair.is_some_and(|pair| pair.smallest_containing);
    let (width, box_suspicious) =
        match resolve_quota(span, box_width, smallest_containing, group_strong) {
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
        actual_text_preserved: true,
    })
}

/// Builds the picker's run list for a page, pairing lopdf addresses with PDFium geometry
/// **by position** (P2.4.1 T17) and walking each run in PDFium's character grid for its own
/// measured span (P2.4.3 T23a).
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
    grid: &crate::editor::runmetrics::PdfiumCharGrid,
) -> Result<Vec<PageTextRun>, AppError> {
    let page_id = page_id(document, page)?;
    let runs = textlocate::locate_runs(document, page_id)?;

    // Pair every run independently: several lopdf fragments may legitimately sit inside one
    // PDFium box, which is precisely the fragmentation this phase exists for. Nothing is
    // consumed, so a shared box is not a conflict.
    let baselines = box_baselines(pdfium_runs, grid);
    let mut pairs: Vec<Option<Pair>> = runs
        .iter()
        .map(|run| pair_run(run, pdfium_runs, &baselines, grid))
        .collect();

    // P2.4.4 T24: re-derive each fragment's span from the characters *it* occupies inside its
    // paired object, by character alignment. This replaces the drifting text-space anchor for
    // every fragment whose object window can be located and whose characters match; a fragment
    // that fails either check keeps the T23a `locate_span` result. Only `Pair::span` changes —
    // every predicate that consumes it is untouched.
    assign_object_spans(&runs, &mut pairs, pdfium_runs, grid);

    // P2.5 T25b: a line-sized box's character window can hold more than the operation that
    // drew into it, so the per-box walk may hand an operand characters that sit elsewhere on
    // the line. Operands of one operation are drawn consecutively, so their spans must run
    // rightward; when they do not, the operation's own located window is the authority.
    resync_straddled_op_spans(&runs, &mut pairs, pdfium_runs, grid);

    let mut described = Vec::new();
    for group in group_runs(&runs, &pairs, pdfium_runs, grid) {
        // P2.4.2 T22: a merged group whose union box is line-sized for its own span is not a
        // merge — its fragments do not fill the box they share, so the box is a line the group
        // only occupies part of. Split into singletons, each measured by its own span.
        if group.len > 1
            && group_box_suspicious(document, page, &runs, &pairs, pdfium_runs, &group)
            && !shared_box_is_own_chain(&runs, &pairs, &group)
        {
            for offset in 0..group.len {
                let singleton = RunGroup::new(group.first + offset, 1);
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
     select a narrower range, or use the annotation tools to add a note \
     (gate: run-mismatch)";
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
        actual_text_preserved: true,
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
    let span = group_span(document, page, runs, pairs, group);
    let box_width = union_rect(group, pairs, rects).map(crate::editor::geometry::PdfRect::width);
    let head_pair = pairs[group.first];
    let strong = head_pair.is_some_and(|pair| pair.confidence == PairConfidence::Strong);
    let smallest_containing = head_pair.is_some_and(|pair| pair.smallest_containing);
    matches!(
        resolve_quota(span, box_width, smallest_containing, strong),
        Quota::Span {
            suspicious: true,
            ..
        }
    )
}

/// Resolves the width-gate old width from the best available source (P2.4.2 T22 / P2.4.3 T23c).
///
/// The preference order is exactly T23c's:
///
/// 1. **`span`** — the char-grid span, else the font-metric span (the caller's `group_span`
///    preference, T23a/T23d). Always the quota when it exists.
/// 2. **`box`** — the strongest pair's box, supplied only when the group has **no** span *and*
///    the box is the **smallest** containing box (`smallest_containing`), and is measurably
///    no wider than a strong pair's own ink would allow. This is the case T23c names: a strong
///    pair whose box is the smallest containing one restores the 0.4.1 `course`/`online` path
///    for a font the renderer drew but the file cannot measure.
/// 3. **refuse** — [`Quota::Unmeasurable`], and the caller refuses rather than trusting a box
///    that may be a whole line (the safe direction, unchanged from P2.4.2).
///
/// A line-sized box never reaches clause 2: it fails `smallest_containing` whenever a smaller
/// box also contains the run, and the `BOX_SPAN_SLACK` guard rejects the rest. A box is only
/// ever `suspicious` when a span exists to compare against, which is why the flag is `false` in
/// the box-quota branch.
///
/// `box_measure` is the same measurement the suspicion test uses, passed as a number so the
/// branch does not recompute the font's metrics.
fn resolve_quota(
    span: Option<f32>,
    box_width: Option<f32>,
    smallest_containing: bool,
    strong: bool,
) -> Quota {
    match span {
        Some(span) => {
            let suspicious = box_width.is_some_and(|box_w| box_w > span * BOX_SPAN_SLACK);
            Quota::Span {
                width: span,
                suspicious,
            }
        }
        None => match box_width {
            Some(box_width) if strong && smallest_containing => Quota::Box(box_width),
            _ => Quota::Unmeasurable,
        },
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
    /// Further whole runs the user selected alongside the head (P2.6 T26). Empty for an
    /// ordinary pick. Their fragments (if they are merged runs) must be in `fragments`.
    pub range: Vec<FragmentRef>,
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
    /// False when the run sat under an `/ActualText` that could not be updated (P2.7.1 T30).
    pub actual_text_preserved: bool,
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

    // P2.6 T26: a selection is a range of runs. Validate it as one span before anything else
    // is measured, so a mixed-style or non-contiguous selection refuses with its own reason
    // instead of being written as a single run's replacement.
    let range_writes = validate_run_range(&all_runs, run, request)?;

    // T3/T14 (a)/(b): re-run the invariant font gates against the freshly parsed run.
    let gates = resolve_gates(document, request.page, run)?;
    let verdict = font_admissibility(&gates);
    if !verdict.editable {
        return Err(refuse(match verdict.gate.as_deref() {
            Some("encoding-round-trip") => Gate::EncodingRoundTrip,
            Some("rotated-text") => Gate::RotatedText,
            Some("nested-xobject") => Gate::NestedXObject,
            _ => Gate::FontEncoding,
        }));
    }

    // P2.7.3 X2 (Q2/Q6): a run inside a form is edited by copying the **draw** the user
    // clicked and writing the copy's stream. The page's own stream is never touched, so an
    // `op_index` from the form's stream can never be applied to the page's (`change_page_content`
    // would land it on an unrelated operation). Cloning happens before anything is measured, so
    // a refusal leaves the document exactly as it was.
    let clones = if run.id.xobject_path.is_empty() {
        Vec::new()
    } else {
        textlocate::clone_draw(document, page_id, &run.id.xobject_path)?
    };
    let level: Vec<textlocate::XObjectStep> = clones
        .iter()
        .map(|clone| textlocate::XObjectStep {
            name: clone.new_name.clone(),
            op_index: clone.do_index,
        })
        .collect();

    // Stage 2 (T11): keep the original font when it can set the replacement; otherwise
    // MECH-A picks a standard-14 substitute. This runs BEFORE any mutation.
    //
    // P2.7.3 §6: the font is resolved at the run's own level, not the page's. A form usually
    // declares its own `/Font`, and the page may not name that resource at all — the page
    // lookup would report "no program" and substitute a face for text the original sets.
    let font = textlocate::font_dictionary_at(document, page_id, &level, &run.font_key)
        .ok_or_else(|| refuse(Gate::FontEncoding))?;
    let plan = crate::editor::substitute::plan_with_font(
        document,
        Some(&font),
        &gates.font,
        &request.replacement,
        request.allow_substitution,
    )?;

    // Step 4 (design §4): encode the replacement through the encoding that will actually
    // be in force. Keep-original uses the run's own encoding; MECH-A uses WinAnsi, which
    // the substitute declares. In both cases an unmappable character is a named refusal,
    // never a silent drop (`string_to_bytes` would drop it).
    let (new_bytes, cid) = match &plan {
        crate::editor::substitute::Plan::KeepOriginal => {
            // P2.8: a Type0 run is written as two-byte codes. Encoding it through lopdf's
            // one-byte machinery would treat each byte of the replacement as a code and drop
            // whatever does not map — the silent-truncation failure this arm exists to avoid.
            if let Some(cid) = crate::editor::cid::CidFont::resolve(document, &font) {
                let bytes = cid
                    .encode(&request.replacement)
                    .map_err(|refusal| AppError::TextRunNotEditable(refusal.message()))?;
                (bytes, Some(cid))
            } else {
                let encoding = font
                    .get_font_encoding(document)
                    .map_err(|_| refuse(Gate::EncodingRoundTrip))?;
                let bytes = encoding.string_to_bytes(&request.replacement);
                let round_tripped = encoding
                    .bytes_to_string(&bytes)
                    .map_err(|_| refuse(Gate::EncodingRoundTrip))?;
                if round_tripped != request.replacement {
                    return Err(refuse(Gate::EncodingRoundTrip));
                }
                (bytes, None)
            }
        }
        crate::editor::substitute::Plan::Substitute { .. } => (
            crate::editor::afm::winansi_encode(&request.replacement)
                .ok_or(AppError::GlyphsNotCoverable)?,
            None,
        ),
    };

    // T13: the width gate compares the FINAL font's metrics. When MECH-A fires, the final
    // font is the standard-14 substitute and its AFM widths are authoritative; otherwise
    // the caller's PDFium measurement is. `old_width` is unchanged either way (O1).
    let (new_width, substitution, note) = match &plan {
        crate::editor::substitute::Plan::KeepOriginal => {
            // P2.8: a CID run's advance comes from its own `/W` then `/DW`, which is what the
            // renderer uses — PDFium's box width is the union of a merged selection and is not
            // comparable to one replacement string.
            let width = match &cid {
                Some(cid) => cid
                    .advance(&new_bytes, request.font_size)
                    .ok_or(AppError::GlyphsNotCoverable)?,
                None => (request.measure)(&request.replacement)?,
            };
            (width, None, None)
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
    for fragment in range_writes {
        writes.push(fragment);
    }
    let merged = textlocate::MergedWrite {
        fragments: writes,
        substitute: substitution.as_deref(),
        replacement: Some(&request.replacement),
        level: level.clone(),
    };
    let (mut inverse, actual_text_preserved) =
        textlocate::apply_merged(document, request.page, page_id, &merged)?;
    let mut clones = clones;
    for clone in &mut clones {
        // The private `/Resources` is created lazily by the write path, and only when the edit
        // needed it (Q3). Comparing the clone against the form it came from records exactly
        // what was made; predicting it here would record a copy that never happened.
        let sibling = textlocate::XObjectStep {
            name: clone.old_name.clone(),
            op_index: clone.do_index,
        };
        let copied = textlocate::XObjectStep {
            name: clone.new_name.clone(),
            op_index: clone.do_index,
        };
        let before = textlocate::form_resources_id(
            document,
            page_id,
            &[clone.parent_path.clone(), vec![sibling]].concat(),
        );
        let after = textlocate::form_resources_id(
            document,
            page_id,
            &[clone.parent_path.clone(), vec![copied]].concat(),
        );
        if after.is_some() && after != before {
            clone.cloned_resources = after;
        }
    }
    inverse.xobject_clones = clones;

    Ok(EditOutcome {
        inverse,
        substitution,
        note,
        actual_text_preserved,
    })
}

/// Turns a selected **range of runs** into the tail writes that clear it, or refuses
/// `selection-range` (P2.6 T26).
///
/// The range is one span when it is consecutive in the content stream, one style at one
/// size, on one baseline, and running rightward. Those are the same conditions [`mergeable`]
/// asks of two fragments, applied to whole runs; a range that fails any of them is a
/// multi-line or mixed-typeface selection, and replacing it with one run's text would silently
/// delete text the user meant to keep.
///
/// A range run's own fragments are already in `request.fragments` when it is a merged run, so
/// clearing the range run's head here is enough: its tail fragments were cleared with the
/// head's merged tail.
fn validate_run_range(
    all_runs: &[textlocate::RawRun],
    head: &textlocate::RawRun,
    request: &EditRequest<'_>,
) -> Result<Vec<textlocate::FragmentWrite>, AppError> {
    if request.range.is_empty() {
        return Ok(Vec::new());
    }
    let mut previous = head;
    let mut writes = Vec::with_capacity(request.range.len());
    for reference in &request.range {
        let next = all_runs
            .iter()
            .find(|run| run.id == reference.run_id)
            .ok_or(AppError::TextRunMismatch)?;
        if next.text != reference.text {
            return Err(AppError::TextRunMismatch);
        }
        // P2.7.3 X1: "consecutive" means consecutive *in one stream*. Two runs in different
        // forms are not one span however their indices read, and one write could not clear
        // both.
        let same_stream = next.id.xobject_path == previous.id.xobject_path;
        let consecutive = if next.id.op_index == previous.id.op_index {
            next.id.operand_path > previous.id.operand_path
        } else {
            next.id.op_index > previous.id.op_index
        };
        let same_style = next.style_class == previous.style_class
            && sizes_match(next.font_size, previous.font_size);
        let same_baseline = (next.baseline() - previous.baseline()).abs() <= BASELINE_TOLERANCE;
        let rightward = next.origin.0 >= previous.origin.0;
        if !(same_stream && consecutive && same_style && same_baseline && rightward) {
            return Err(refuse(Gate::SelectionRange));
        }
        writes.push(textlocate::FragmentWrite {
            id: next.id.clone(),
            expected: next.text.clone(),
            new_bytes: Vec::new(),
        });
        previous = next;
    }
    Ok(writes)
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
///
/// A form edit (P2.7.3 Q6) restores its operand bytes in the **clone's** stream, repoints every
/// `Do` it changed back at the original name, and drops the clones — one pass, one undo entry.
pub fn revert(document: &mut Document, operation: &ContentOperation) -> Result<(), AppError> {
    let page_id = page_id(document, operation.page)?;
    let level = &operation.xobject_path;
    let mut content = textlocate::read_level(document, page_id, level)?;

    // Every write the edit made, head first.
    let writes: Vec<&ContentOperation> = std::iter::once(operation)
        .chain(operation.also.iter())
        .collect();

    // Q6, leg one: each `Do` must still name the clone this edit installed. Checking before
    // any mutation means a `Do` someone else has since repointed refuses the whole undo rather
    // than half reverting it and repointing a draw the user never edited.
    for clone in &operation.xobject_clones {
        let enclosing = textlocate::read_level(document, page_id, &clone.parent_path)?;
        let operation_at_do = enclosing
            .operations
            .get(clone.do_index)
            .ok_or(AppError::TextRunMismatch)?;
        if operation_at_do.operator != "Do"
            || operation_at_do
                .operands
                .first()
                .and_then(|o| o.as_name().ok())
                != Some(clone.new_name.as_slice())
        {
            return Err(AppError::TextRunMismatch);
        }
    }

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

    {
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

        // Undo a `/Properties` clone (P2.7.1 B-7): the `BDC` goes back to the name it used
        // before the edit. A `BDC` that no longer names the clone means something else edited
        // it since, and repointing it would undo an edit the user did not make.
        if let Some(clone) = &operation.property_clone {
            let bdc = content
                .operations
                .get_mut(clone.op_index)
                .ok_or(AppError::TextRunMismatch)?;
            if bdc.operator != "BDC"
                || bdc.operands.get(1).and_then(|name| name.as_name().ok())
                    != Some(clone.new_name.as_slice())
            {
                return Err(AppError::TextRunMismatch);
            }
            *bdc.operands.get_mut(1).ok_or(AppError::TextRunMismatch)? =
                lopdf::Object::Name(clone.old_name.clone());
        }
    }

    textlocate::write_level(document, page_id, level, &content)?;

    // The `/Properties` clone is now unreferenced; drop it so an undo leaves the file as it was
    // rather than accumulating one orphan per edit-then-undo cycle.
    if let Some(clone) = &operation.property_clone {
        if let Some(properties) = textlocate::properties_mut(document, page_id) {
            let is_our_clone = properties
                .get(&clone.new_name)
                .ok()
                .and_then(|entry| entry.as_reference().ok())
                == Some(clone.clone_object);
            if is_our_clone {
                properties.remove(&clone.new_name);
                document.objects.remove(&clone.clone_object);
            }
        }
    }

    // Q6, leg two: put each `Do` back, then drop what the edit created, so edit-then-undo
    // leaves the file as it was rather than one orphan per cycle.
    for clone in &operation.xobject_clones {
        let mut enclosing = textlocate::read_level(document, page_id, &clone.parent_path)?;
        let operation_at_do = enclosing
            .operations
            .get_mut(clone.do_index)
            .ok_or(AppError::TextRunMismatch)?;
        *operation_at_do
            .operands
            .first_mut()
            .ok_or(AppError::TextRunMismatch)? = lopdf::Object::Name(clone.old_name.clone());
        textlocate::write_level(document, page_id, &clone.parent_path, &enclosing)?;

        textlocate::remove_xobject_name(
            document,
            page_id,
            &clone.parent_path,
            &clone.new_name,
            clone.clone_object,
        );
        // The clone object before its resources: while the clone exists it still points at the
        // private dictionary, so removing the dictionary first would leave it looking in use.
        if !textlocate::object_is_referenced(document, clone.clone_object) {
            document.objects.remove(&clone.clone_object);
        }
        if let Some(cloned) = clone.cloned_resources {
            if !textlocate::object_is_referenced(document, cloned) {
                document.objects.remove(&cloned);
            }
        }
    }

    Ok(())
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
            range: Vec::new(),
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
                xobject_path: Vec::new(),
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
            composable: true,
            // Tests name the font by its style class; the face is the same name, so a test
            // that wants "same style, different face" sets `face` directly.
            face: font.to_string(),
            style_class: font.to_string(),
            marked_content: Vec::new(),
            rise: 0.0,
            rotated: false,
            xobject_refusal: None,
        }
    }

    #[test]
    fn merges_fragments_on_one_baseline_within_a_quarter_em() {
        // "He" then "llo", the second starting exactly where the first ends.
        let head = test_run("He", 72.0, 12.0, "F1");
        let next = test_run("llo", 72.0 + head.advance, 12.0, "F1");
        assert!(mergeable(&head, &next, false, None, None));
    }

    /// P2.7.1 B-5: `Ts` is a merge boundary. A raised operand sits on a different ink baseline
    /// than the text it follows, so merging them would write the superscript into the body run.
    #[test]
    fn does_not_merge_across_a_rise_change() {
        let head = test_run("E", 72.0, 12.0, "F1");
        let mut next = test_run("2", 72.0 + head.advance, 12.0, "F1");
        // Same pen baseline, different ink: exactly what `Ts 5` produces.
        next.rise = 5.0;
        assert_eq!(
            next.origin.1, head.origin.1,
            "the fixture must share a pen baseline"
        );
        assert!(!mergeable(&head, &next, false, None, None));
    }

    /// The control: adding the same rise to *both* operands keeps them on one baseline, so the
    /// gate above cannot be passing merely because `rise` is non-zero.
    #[test]
    fn merges_two_runs_raised_by_the_same_ts() {
        let mut head = test_run("He", 72.0, 12.0, "F1");
        head.rise = 5.0;
        let mut next = test_run("llo", 72.0 + head.advance, 12.0, "F1");
        next.rise = 5.0;
        assert!(mergeable(&head, &next, false, None, None));
    }

    /// P2.6 T26: two *resources* that name the same face are one style, so a producer's
    /// mid-word resource switch does not split the line. The Maastricht brochure does exactly
    /// this (`F4`/`F5`, both Times at 14.064pt).
    ///
    /// P2.8: this survives only because both are *composable* — a simple font can draw an
    /// arbitrary replacement, so either resource can carry the merged write.
    #[test]
    fn merges_across_resources_that_name_the_same_face() {
        let head = test_run("Scien", 72.0, 12.0, "Times-Roman");
        let next = test_run("tific", 72.0 + head.advance, 12.0, "Times-Roman");
        let mut other_resource = next.clone();
        other_resource.font_key = b"F9".to_vec();
        assert!(mergeable(&head, &other_resource, false, None, None));
    }

    /// P2.8 §1.1(a): same style class, different face. `ArialMT` and `ABCDEE+Monaco` are both
    /// `Helvetica` to `style_match`, but a merged write through either would delete the other's
    /// glyphs. They must stay two runs.
    #[test]
    fn never_merges_two_faces_that_share_a_style_class() {
        let head = test_run("Each hour", 72.0, 12.0, "Helvetica");
        let mut next = test_run(" of lecture", 72.0 + head.advance, 12.0, "Helvetica");
        next.face = "Monaco".to_string();
        assert_eq!(head.style_class, next.style_class);
        assert!(!mergeable(&head, &next, false, None, None));
    }

    /// P2.8: a fragment whose font cannot draw the replacement is not a merge target, even
    /// when its style class matches. The Maastricht line's Monaco is split across `F3`
    /// (`TrueType`, editable) and `F6` (`Type0/Identity-H`, CID codes); a merged write through
    /// `F3`'s encoding would emit the second fragment's Unicode as unrelated glyphs.
    #[test]
    fn never_merges_a_fragment_whose_font_cannot_be_rewritten() {
        let head = test_run("Each hour", 72.0, 12.0, "Monaco");
        let mut next = test_run(" of lecture", 72.0 + head.advance, 12.0, "Monaco");
        next.composable = false;
        assert!(!mergeable(&head, &next, false, None, None));
    }

    /// A real style change still splits: bold is not roman, and a serif is not a sans.
    #[test]
    fn never_merges_across_a_style_change() {
        let head = test_run("Scien", 72.0, 12.0, "Times-Roman");
        let bold = test_run("tific", 72.0 + head.advance, 12.0, "Times-Bold");
        assert!(!mergeable(&head, &bold, false, None, None));
        let sans = test_run("tific", 72.0 + head.advance, 12.0, "Helvetica");
        assert!(!mergeable(&head, &sans, false, None, None));
    }

    #[test]
    fn never_merges_across_a_font_or_a_size() {
        let head = test_run("AB", 72.0, 12.0, "F1");
        // Same position and size, different font: the merged run could not be encoded with
        // one font (T19).
        let other_font = test_run("CD", 72.0 + head.advance, 12.0, "F2");
        assert!(!mergeable(&head, &other_font, false, None, None));
        // Same font and position, different size: a heading is not a body word.
        let other_size = test_run("CD", 72.0 + head.advance, 10.0, "F1");
        assert!(!mergeable(&head, &other_size, false, None, None));
    }

    #[test]
    fn never_merges_across_a_line_break() {
        let head = test_run("AB", 72.0, 12.0, "F1");
        let mut next = test_run("CD", 72.0 + head.advance, 12.0, "F1");
        next.origin.1 = head.origin.1 + 12.0; // next line
        assert!(!mergeable(&head, &next, false, None, None));
    }

    #[test]
    fn merges_body_words_but_not_heading_words_at_the_same_absolute_gap() {
        // The whole reason the threshold is relative: a 3pt gap is inside a 24pt heading's
        // letter-spacing but a whole extra word at 9pt. An absolute constant would get one of
        // these wrong whichever value it picked.
        let body_head = test_run("foo", 72.0, 9.0, "F1");
        let body_next = test_run("bar", 72.0 + body_head.advance + 3.0, 9.0, "F1");
        // 3pt gap vs tolerance 0.25*9 = 2.25pt: too far, the words stay separate.
        assert!(!mergeable(&body_head, &body_next, false, None, None));

        let head_head = test_run("Fo", 72.0, 24.0, "F1");
        let head_next = test_run("o", 72.0 + head_head.advance + 3.0, 24.0, "F1");
        // 3pt gap vs tolerance 0.25*24 = 6pt: fine, the heading's glyphs merge.
        assert!(mergeable(&head_head, &head_next, false, None, None));
    }

    /// P2.4.2 O-Chain-2: the widened `0.5em` gap applies **only** when PDFium's reconstructed
    /// text carries a real space at the seam; without the space flag it never does.
    #[test]
    fn the_widened_gap_needs_a_real_space_at_the_seam() {
        // A 4pt gap at 12pt: 0.33em, past the 0.25em bound but inside the 0.5em one.
        let head = test_run("foo", 72.0, 12.0, "F1");
        let next = test_run("bar", 72.0 + head.advance + 4.0, 12.0, "F1");
        assert!(
            !mergeable(&head, &next, false, None, None),
            "no space at the seam, so the quarter-em bound still applies"
        );
        assert!(
            mergeable(&head, &next, true, None, None),
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
        assert!(mergeable(&head, &touching, false, None, None));
        // Past it: the head's advance overshot, so do not join.
        let overlapping = test_run("CD", 72.0 + head.advance - 2.0, 12.0, "F1");
        assert!(!mergeable(&head, &overlapping, false, None, None));
    }

    /// The T-1 acceptance from the plan: a run with `Ts 5` must pair to the box that is
    /// 5pt above its pen.
    ///
    /// The fixture is built so the two readings disagree: the body box ends exactly at the
    /// pen (`695..700`), the raised box starts exactly at the ink (`705..712`). Measuring the
    /// pen therefore pairs downward, and measuring the ink pairs upward — the boxes cannot
    /// both match, which is what makes this test able to fail.
    #[test]
    fn a_raised_run_pairs_to_the_box_offset_by_its_rise() {
        use crate::editor::runmetrics::PdfiumRun;
        let grid = crate::editor::runmetrics::PdfiumCharGrid {
            text: "HIGH".to_string(),
            chars: Vec::new(),
        };

        let mut run = test_run("HIGH", 72.0, 12.0, "F1");
        run.rise = 5.0;
        run.origin.1 = 700.0; // pen at 700, ink at 705

        let body = PdfiumRun {
            text: "BASE".to_string(),
            font_name: "Helvetica".to_string(),
            font_size: 12.0,
            x0: 72.0,
            x1: 92.0,
            y0: 688.0,
            y1: 700.0,
            width: 20.0,
        };
        let raised = PdfiumRun {
            y0: 705.0,
            y1: 717.0,
            ..body.clone()
        };
        let rects = vec![body, raised];
        // Rendered baselines: the body sits on 700, the raised glyphs on 705.
        let pair = pair_run(&run, &rects, &[700.0, 705.0], &grid).expect("pairs");
        assert_eq!(
            pair.rect, 1,
            "the run's ink sits at 705, so it belongs to the raised box, not the body"
        );
        assert_eq!(pair.confidence, PairConfidence::Strong);
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
        let grid = crate::editor::runmetrics::PdfiumCharGrid {
            text: String::new(),
            chars: Vec::new(),
        };
        let pair = pair_run(&run, &rects, &box_baselines(&rects, &grid), &grid)
            .expect("a containing box pairs");
        assert_eq!(pair.rect, 1, "exact text breaks the tie");

        // Geometry first: a box whose baseline is nearer wins even though its text differs,
        // because text must never override the position T17 pairs on.
        let mut exact_but_far = box_at("He", 72.0);
        exact_but_far.y0 = 700.2;
        exact_but_far.y1 = 712.2;
        let rects = vec![exact_but_far, box_at("other", 72.0)];
        let pair = pair_run(&run, &rects, &box_baselines(&rects, &grid), &grid)
            .expect("a containing box pairs");
        assert_eq!(
            pair.rect, 1,
            "the nearer baseline wins even though its text differs"
        );
    }

    /// Builds one grid character at `x` with the given advance width, on `baseline` (P2.4.4).
    fn grid_char(
        ch: char,
        x: f32,
        width: f32,
        baseline: f32,
        generated: bool,
    ) -> crate::editor::runmetrics::PdfiumChar {
        crate::editor::runmetrics::PdfiumChar {
            unicode: Some(ch),
            generated,
            x0: x,
            y0: baseline - 3.0,
            x1: x + width,
            y1: baseline + 9.0,
            ink_x0: x,
            ink_y0: baseline,
            ink_x1: x + width,
            ink_y1: baseline + 9.0,
            origin_x: x,
            origin_y: baseline,
            font_size: 12.0,
        }
    }

    /// A PDFium box for the pairing tests, at the module level so a T24 test can use it too.
    fn test_box(text: &str, x0: f32) -> crate::editor::runmetrics::PdfiumRun {
        crate::editor::runmetrics::PdfiumRun {
            text: text.to_string(),
            font_name: "Helvetica".to_string(),
            font_size: 12.0,
            x0,
            y0: 700.0,
            x1: x0 + 20.0,
            y1: 712.0,
            width: 20.0,
        }
    }

    /// A box whose bottom edge sits below its baseline by a descender depth — the Maastricht
    /// shape, where `versity`'s `y` pushes `y0` 3.39pt under the pen origin.
    #[test]
    fn rendered_baseline_reads_the_grid_pen_not_the_box_bottom() {
        let grid = crate::editor::runmetrics::PdfiumCharGrid {
            text: "y".to_string(),
            chars: vec![grid_char('y', 72.0, 7.2, 700.0, false)],
        };
        let mut rect = test_box("y", 72.0);
        // The box bottom is 3.4pt below the baseline (a descender), so `rect.y0` alone would
        // *never* equal the run's origin and every pair would be weak.
        rect.y0 = 696.6;
        rect.y1 = 712.0;
        rect.x1 = 79.2;
        assert_eq!(
            rendered_baseline(&rect, &grid),
            700.0,
            "the baseline comes from the characters, not the descender-depth bottom edge"
        );

        // With no grid in the box's window the bottom edge still stands in, so an in-memory
        // test with an empty grid is unchanged.
        let empty = crate::editor::runmetrics::PdfiumCharGrid {
            text: String::new(),
            chars: Vec::new(),
        };
        assert_eq!(rendered_baseline(&rect, &empty), 696.6);
    }

    /// A descender line's pair is strong **only** because the baseline is read from the grid.
    #[test]
    fn a_descender_line_pairs_strongly_once_the_baseline_comes_from_the_grid() {
        let grid = crate::editor::runmetrics::PdfiumCharGrid {
            text: "y".to_string(),
            chars: vec![grid_char('y', 72.0, 7.2, 700.0, false)],
        };
        let run = test_run("y", 72.0, 12.0, "F1");
        let mut rect = test_box("y", 72.0);
        rect.y0 = 696.6; // 3.4pt descender depth
        rect.y1 = 712.0;
        rect.x1 = 79.2;
        let rects = vec![rect];
        let pair = pair_run(&run, &rects, &box_baselines(&rects, &grid), &grid)
            .expect("the box contains the origin");
        assert_eq!(
            pair.confidence,
            PairConfidence::Strong,
            "the grid baseline is the run's origin"
        );
    }

    /// T24: fragments inside one object take their spans by **character alignment**, skipping
    /// the generated seam space, and a mis-identified fragment is left on its fallback span.
    #[test]
    fn assign_object_spans_aligns_characters_and_skips_generated_ones() {
        // Grid: "Data| (generated) |Sci" in a 0.6em face at 15pt (9pt a glyph), on the baseline.
        let mut chars = Vec::new();
        let mut x = 46.0_f32;
        for ch in "Data".chars() {
            chars.push(grid_char(ch, x, 9.0_f32, 700.0, false));
            x += 9.0;
        }
        // The kern seam PDFium synthesises: zero width, occupies a slot, no operand owns it.
        chars.push(grid_char(' ', x, 0.0, 700.0, true));
        for ch in "Sci".chars() {
            chars.push(grid_char(ch, x, 9.0_f32, 700.0, false));
            x += 9.0;
        }
        let grid = crate::editor::runmetrics::PdfiumCharGrid {
            text: "Data Sci".to_string(),
            chars,
        };

        let head = test_run("Data", 46.0, 15.0, "F1");
        let tail = test_run("Sci", 85.0, 15.0, "F1");
        let runs = vec![head, tail];
        let mut rect = test_box("Data Sci", 46.0);
        rect.font_size = 15.0;
        rect.x1 = 109.0;
        let rects = vec![rect];
        let pair = Pair {
            rect: 0,
            confidence: PairConfidence::Strong,
            span: None,
            smallest_containing: true,
        };
        let mut pairs = vec![Some(pair), Some(pair)];

        assign_object_spans(&runs, &mut pairs, &rects, &grid);

        let head_span = pairs[0].unwrap().span.expect("head span assigned");
        let tail_span = pairs[1].unwrap().span.expect("tail span assigned");
        assert_eq!(
            (head_span.first, head_span.last),
            (0, 3),
            "Data keeps its own four characters"
        );
        assert_eq!(
            (tail_span.first, tail_span.last),
            (5, 7),
            "Sci starts after the generated seam (index 4), not on it"
        );
        assert_eq!(head_span.x0, 46.0);
        assert_eq!(head_span.x1, 82.0, "Data is 4 x 9pt");
        assert_eq!(tail_span.x0, 82.0, "Sci renders from the seam onward");
        assert_eq!(tail_span.x1, 109.0, "Sci is 3 x 9pt");

        // A fragment whose characters do not match the window keeps its fallback span: give the
        // tail text that is not in the grid and confirm its span is left untouched.
        let stray = test_run("Xyz", 85.0, 15.0, "F1");
        let runs = vec![runs[0].clone(), stray];
        let fallback = CharSpan {
            first: 0,
            last: 0,
            x0: 1.0,
            x1: 2.0,
        };
        let mut pairs = vec![
            Some(Pair {
                span: Some(fallback),
                ..pair
            }),
            Some(Pair {
                span: Some(fallback),
                ..pair
            }),
        ];
        assign_object_spans(&runs, &mut pairs, &rects, &grid);
        assert_eq!(
            pairs[1].unwrap().span,
            Some(fallback),
            "a char-identity mismatch falls back, never mis-assigns"
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
                span: None,
                smallest_containing: true,
            })
        };
        let boxes = group_boxes(&document, &runs);
        let groups = group_runs(
            &runs,
            &[strong(0), strong(0), strong(2)],
            &boxes,
            &empty_grid(),
        );
        // Two groups: "Hello" (2 fragments) and "far" (1), because the gap to "far" is far
        // past a quarter em.
        assert_eq!(groups.len(), 2);
        assert_eq!((groups[0].first, groups[0].len), (0, 2));
        assert_eq!((groups[1].first, groups[1].len), (2, 1));
    }

    /// A grid with no characters: the seam predicate then never sees a generated space, so a
    /// unit test that is not about kern seams keeps the pre-item-4 behaviour.
    fn empty_grid() -> crate::editor::runmetrics::PdfiumCharGrid {
        crate::editor::runmetrics::PdfiumCharGrid {
            text: String::new(),
            chars: Vec::new(),
        }
    }

    /// P2.6 T26 item 4: the kerned word break. `Some -400 Word` at 15pt puts a zero-advance
    /// generated space between the two operands and advances the pen 6pt across the seam.
    #[test]
    fn a_kerned_word_break_reads_as_a_space() {
        let runs = vec![
            test_run("Some", 46.0, 15.0, "F1"),
            test_run("Word", 91.0, 15.0, "F1"),
        ];
        let span = |first, last, x0, x1| {
            Some(Pair {
                rect: 0,
                confidence: PairConfidence::Strong,
                span: Some(CharSpan {
                    first,
                    last,
                    x0,
                    x1,
                }),
                smallest_containing: true,
            })
        };
        let pairs = [span(0, 3, 46.0, 85.18), span(5, 8, 91.18, 127.01)];
        let grid = crate::editor::runmetrics::PdfiumCharGrid {
            text: "Some Word".to_string(),
            chars: vec![
                grid_char('S', 46.0, 10.01, 712.0, false),
                grid_char('o', 56.01, 8.34, 712.0, false),
                grid_char('m', 64.35, 12.49, 712.0, false),
                grid_char('e', 76.84, 8.34, 712.0, false),
                grid_char(' ', 91.18, 0.00, 712.0, true),
                grid_char('W', 91.18, 14.16, 712.0, false),
                grid_char('o', 105.34, 8.34, 712.0, false),
                grid_char('r', 113.68, 5.00, 712.0, false),
                grid_char('d', 118.68, 8.33, 712.0, false),
            ],
        };
        assert!(seam_is_word_space(0, 1, &runs, &pairs, &grid));
    }

    /// The counter-cases the predicate exists to reject. Each is a variant of the kern seam,
    /// so a change that widens the test is caught here rather than on a real document.
    #[test]
    fn a_seam_is_a_word_space_only_at_a_wide_generated_gap() {
        let runs = vec![
            test_run("Some", 46.0, 15.0, "F1"),
            test_run("Word", 91.0, 15.0, "F1"),
        ];
        let pair = |span: CharSpan| {
            Some(Pair {
                rect: 0,
                confidence: PairConfidence::Strong,
                span: Some(span),
                smallest_containing: true,
            })
        };
        let head = CharSpan {
            first: 0,
            last: 3,
            x0: 46.0,
            x1: 85.18,
        };
        let tail = CharSpan {
            first: 5,
            last: 8,
            x0: 91.18,
            x1: 127.01,
        };
        let grid = |between: crate::editor::runmetrics::PdfiumChar| {
            crate::editor::runmetrics::PdfiumCharGrid {
                text: String::new(),
                chars: vec![
                    grid_char('S', 46.0, 10.01, 712.0, false),
                    grid_char('o', 56.01, 8.34, 712.0, false),
                    grid_char('m', 64.35, 12.49, 712.0, false),
                    grid_char('e', 76.84, 8.34, 712.0, false),
                    between,
                    grid_char('W', 91.18, 14.16, 712.0, false),
                ],
            }
        };

        // The seam itself: a zero-advance generated space, 6pt wide.
        assert!(seam_is_word_space(
            0,
            1,
            &runs,
            &[pair(head), pair(tail)],
            &grid(grid_char(' ', 91.18, 0.00, 712.0, true))
        ));

        // A line break is not a word space.
        assert!(!seam_is_word_space(
            0,
            1,
            &runs,
            &[pair(head), pair(tail)],
            &grid(grid_char('\r', 91.18, 0.00, 712.0, true))
        ));

        // A space PDFium *laid out* (non-zero advance) is already a real glyph; inserting one
        // would double it.
        assert!(!seam_is_word_space(
            0,
            1,
            &runs,
            &[pair(head), pair(tail)],
            &grid(grid_char(' ', 85.18, 6.00, 712.0, true))
        ));

        // A narrow seam: the compressed-face artifact (`m o n th`), not a word break.
        let narrow_tail = CharSpan {
            first: 5,
            last: 8,
            x0: 86.18,
            x1: 122.01,
        };
        assert!(!seam_is_word_space(
            0,
            1,
            &runs,
            &[pair(head), pair(narrow_tail)],
            &grid(grid_char(' ', 86.18, 0.00, 712.0, true))
        ));

        // Bytes that already carry the whitespace: the separator is not doubled.
        let padded = vec![
            test_run("Some ", 46.0, 15.0, "F1"),
            test_run("Word", 91.0, 15.0, "F1"),
        ];
        assert!(!seam_is_word_space(
            0,
            1,
            &padded,
            &[pair(head), pair(tail)],
            &grid(grid_char(' ', 91.18, 0.00, 712.0, true))
        ));
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
            span: None,
            smallest_containing: true,
        });
        let boxes = group_boxes(&document, &runs);
        let groups = group_runs(&runs, &[weak, weak, None], &boxes, &empty_grid());
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
                span: None,
                smallest_containing: true,
            })
        };
        let pairs = [strong(0), strong(0)];
        let group = RunGroup::new(0, 2);

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

    /// P2.4.3 T23b: the gap between two fragments is read from PDFium's character grid when
    /// both spans are available — exact for every font class, including one with no metrics.
    #[test]
    fn the_gap_is_the_char_grid_span_when_both_spans_are_available() {
        use crate::editor::runmetrics::CharSpan;
        let head = test_run("DataSci", 72.0, 12.0, "F1");
        // Placed so the *approximation* would call this a wide gap: the head measures 42pt
        // (0.5em/glyph) where its rendered span is 50.4pt.
        let next = test_run("ence", 122.4, 12.0, "F1");
        let head_span = CharSpan {
            first: 0,
            last: 6,
            x0: 72.0,
            x1: 72.0 + 50.4,
        };
        let next_span = CharSpan {
            first: 7,
            last: 10,
            x0: 122.4,
            x1: 122.4 + 24.0,
        };
        // Char-grid: gap = 0, the chain is whole.
        assert!(
            mergeable(&head, &next, false, Some(head_span), Some(next_span)),
            "the rendered seam is a zero gap"
        );
        // Without the grid the approximation inflates the head and breaks the chain — the
        // exact 0.4.2 field failure this test pins the cure for.
        assert!(
            !mergeable(&head, &next, false, None, None),
            "the approximation breaks the same seam"
        );
    }

    /// P2.4.3 T23a: a span from the grid also supplies the width quota, replacing both the
    /// approximation and the unmeasurable refusal.
    #[test]
    fn the_char_grid_span_is_the_width_quota() {
        // A span with a box the group fills: the quota is the span, not the box.
        match resolve_quota(Some(84.4), Some(84.4), true, true) {
            Quota::Span { width, suspicious } => {
                assert_eq!(width, 84.4);
                assert!(!suspicious);
            }
            _ => panic!("a span is always the quota"),
        }
    }

    /// P2.4.3 T23c: the full preference order, one branch per arm.
    #[test]
    fn the_width_quota_follows_the_documented_preference_order() {
        // (1) the span wins whenever it exists, even for a strong pair with a plausible box —
        // T22's rule, preserved: the box never overrides a real span.
        match resolve_quota(Some(20.0), Some(20.5), true, true) {
            Quota::Span { width, .. } => assert_eq!(width, 20.0),
            _ => panic!("the span is clause 1"),
        }

        // A span the box dwarfs is suspicious: the box was a line, the quota is the span.
        match resolve_quota(Some(24.0), Some(127.0), true, true) {
            Quota::Span { width, suspicious } => {
                assert_eq!(width, 24.0);
                assert!(
                    suspicious,
                    "a line-sized box against a 24pt span is suspicious"
                );
            }
            _ => panic!("a span is clause 1"),
        }

        // (2) no span, a strong pair whose box is the smallest containing one: the box is the
        // run's own ink, so it supplies the quota (T23c's restored `course`/`online` case).
        match resolve_quota(None, Some(38.0), true, true) {
            Quota::Box(width) => assert_eq!(width, 38.0),
            _ => panic!("clause 2 supplies the box"),
        }

        // …but not when the box is not the smallest one containing the run: it may be a line.
        match resolve_quota(None, Some(38.0), false, true) {
            Quota::Unmeasurable => {}
            _ => panic!("a non-smallest box must not supply the quota"),
        }

        // …and not from a weak pair, whose box is suspect for that reason too.
        match resolve_quota(None, Some(38.0), true, false) {
            Quota::Unmeasurable => {}
            _ => panic!("a weak pair must not supply the quota"),
        }

        // (3) nothing measurable: refuse (T23d), unchanged from P2.4.2.
        match resolve_quota(None, None, false, false) {
            Quota::Unmeasurable => {}
            _ => panic!("with no measurement the run refuses"),
        }
    }

    /// P2.4.3 T23c: the "smallest containing box" test that clause 2 rests on.
    #[test]
    fn only_the_smallest_containing_box_may_supply_the_quota() {
        use crate::editor::runmetrics::PdfiumRun;
        let box_at = |x0: f32, x1: f32| PdfiumRun {
            text: "x".to_string(),
            font_name: "ABCDEE+Monaco".to_string(),
            font_size: 12.0,
            x0,
            y0: 700.0,
            x1,
            y1: 712.0,
            width: x1 - x0,
        };
        let run = test_run("DataSci", 72.0, 12.0, "F1");
        let tol = origin_tolerance(run.font_size);

        // One box only: it is trivially the smallest.
        let rects = vec![box_at(72.0, 122.4)];
        assert!(smallest_containing(&run, 0, &rects, tol));

        // A word box and a line box both containing the run: the word box is the smallest, so
        // the orphan's line box (index 1) is *not*.
        let rects = vec![box_at(72.0, 122.4), box_at(72.0, 500.0)];
        assert!(
            smallest_containing(&run, 0, &rects, tol),
            "the word box is the smallest containing box"
        );
        assert!(
            !smallest_containing(&run, 1, &rects, tol),
            "the line box is not: only a larger one contains the run"
        );
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

    /// P2.8: a Type0 run *without* `/ToUnicode` is still refused at gate (a) — its codes
    /// cannot be read back, so no replacement can be shown to be what the user typed. The
    /// fixture has no CMap, which is now the only thing this gate refuses.
    #[test]
    fn refuses_a_cid_font_without_tounicode() {
        let mut document = testpdf::cid_identity_page();
        let run =
            textlocate::locate_runs(&document, page_id(&document, 0).unwrap()).unwrap()[0].clone();
        let measure = measure_fixed(10.0);

        let error = edit(&mut document, &request(&run, "EDIT", 100.0, &measure)).unwrap_err();
        assert_eq!(error.code(), "TEXT_RUN_NOT_EDITABLE");
        assert!(error.to_string().contains("font-encoding"), "{error}");
    }

    /// P2.8: the positive case the old gate made impossible — a Type0 run whose font carries a
    /// `/ToUnicode` is editable, and the replacement is written as two-byte codes.
    #[test]
    fn edits_a_cid_font_through_its_tounicode() {
        let mut document = testpdf::cid_tounicode_page();
        let run =
            textlocate::locate_runs(&document, page_id(&document, 0).unwrap()).unwrap()[0].clone();
        assert!(font_admissibility(&resolve_gates(&document, 0, &run).unwrap()).editable);

        let measure = measure_fixed(10.0);
        let outcome = edit(&mut document, &request(&run, "BC", 100.0, &measure)).unwrap();
        assert!(outcome.substitution.is_none());

        let after = textlocate::locate_runs(&document, page_id(&document, 0).unwrap()).unwrap();
        assert_eq!(after[0].text, "BC");
        assert_eq!(after[0].bytes, vec![0x00, 0x02, 0x00, 0x03]);
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

    /// P2.7 hotfix H0: a rotated run was previously **editable**, with `advance` measured
    /// along the text axis and `fits-width` comparing it to a horizontal box. The write path
    /// must now refuse it, naming the geometry rather than the font.
    #[test]
    fn refuses_a_rotated_run_with_gate_h() {
        let mut document = testpdf::rotated_text();
        let run =
            textlocate::locate_runs(&document, page_id(&document, 0).unwrap()).unwrap()[0].clone();
        assert!(
            run.rotated,
            "fixture must be rotated for this test to mean anything"
        );

        let gates = resolve_gates(&document, 0, &run).unwrap();
        let verdict = font_admissibility(&gates);
        assert!(!verdict.editable);
        assert_eq!(verdict.gate.as_deref(), Some("rotated-text"));

        // The write path re-validates independently, so a caller that skipped the picker
        // still refuses.
        let measure = measure_fixed(10.0);
        let error = edit(&mut document, &request(&run, "X", 100.0, &measure)).unwrap_err();
        assert_eq!(error.code(), "TEXT_RUN_NOT_EDITABLE");
        assert!(error.to_string().contains("rotated-text"), "{error}");
    }

    /// H0.2: the refusal must reach the *picker*, not only the write path. A rotated run with
    /// no PDFium box would otherwise refuse `run-mismatch` — the wrong reason — because the
    /// unpaired check overwrites the verdict. Geometry must win.
    #[test]
    fn a_rotated_run_refuses_as_rotated_not_as_unpaired() {
        let document = testpdf::rotated_text();
        // No PDFium runs are supplied, which is the shape that used to overwrite the verdict.
        let described = describe_runs(
            &document,
            0,
            &[],
            &crate::editor::runmetrics::PdfiumCharGrid {
                text: String::new(),
                chars: Vec::new(),
            },
        )
        .unwrap();

        assert_eq!(described.len(), 1);
        assert!(!described[0].admissibility.editable);
        assert_eq!(
            described[0].admissibility.gate.as_deref(),
            Some("rotated-text"),
            "a rotated run must refuse for its geometry, not for a missing box"
        );
    }

    /// P2.7 T-7: the same refusal must cover a run rotated by `cm` rather than `Tm`.
    /// H0 read the text matrix alone, so this run was offered as editable with a horizontal
    /// `advance` while the renderer drew it vertically — editable *and* mis-measured.
    #[test]
    fn refuses_a_run_rotated_by_the_graphics_matrix() {
        let mut document = testpdf::rotated_by_graphics_matrix();
        let run =
            textlocate::locate_runs(&document, page_id(&document, 0).unwrap()).unwrap()[0].clone();
        assert!(
            run.rotated,
            "fixture must be rotated for this test to mean anything"
        );

        let gates = resolve_gates(&document, 0, &run).unwrap();
        assert_eq!(
            font_admissibility(&gates).gate.as_deref(),
            Some("rotated-text")
        );

        // The picker path, which is where a user would otherwise be offered the edit.
        let described = describe_runs(
            &document,
            0,
            &[],
            &crate::editor::runmetrics::PdfiumCharGrid {
                text: String::new(),
                chars: Vec::new(),
            },
        )
        .unwrap();
        assert_eq!(described.len(), 1);
        assert_eq!(
            described[0].admissibility.gate.as_deref(),
            Some("rotated-text")
        );

        let measure = measure_fixed(10.0);
        let error = edit(&mut document, &request(&run, "X", 100.0, &measure)).unwrap_err();
        assert!(error.to_string().contains("rotated-text"), "{error}");
    }

    /// The control for the test above: a `cm` that only translates leaves the run editable,
    /// so the wider detection did not start refusing ordinary pages.
    #[test]
    fn a_run_translated_by_the_graphics_matrix_stays_editable() {
        let document = testpdf::translated_by_graphics_matrix();
        let run =
            textlocate::locate_runs(&document, page_id(&document, 0).unwrap()).unwrap()[0].clone();
        assert!(!run.rotated);
        let gates = resolve_gates(&document, 0, &run).unwrap();
        assert!(font_admissibility(&gates).editable);
    }

    /// The control for the refusal above: the same text in an axis-aligned matrix stays
    /// editable, so H0 did not make ordinary pages refuse.
    #[test]
    fn an_axis_aligned_run_stays_editable() {
        let document = testpdf::unrotated_text();
        let run =
            textlocate::locate_runs(&document, page_id(&document, 0).unwrap()).unwrap()[0].clone();
        assert!(!run.rotated);

        let gates = resolve_gates(&document, 0, &run).unwrap();
        assert!(font_admissibility(&gates).editable);
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

    /// P2.6 T26: a selection is a range of whole runs, validated as one span.
    #[test]
    fn a_run_range_is_validated_as_one_contiguous_same_style_span() -> Result<(), String> {
        let measure = measure_fixed(30.0);
        let head = test_run("Alpha", 10.0, 12.0, "Times");
        let mut middle = test_run("Beta", 40.0, 12.0, "Times");
        middle.id.op_index = 1;
        let mut tail = test_run("Gamma", 60.0, 12.0, "Times");
        tail.id.op_index = 2;
        let all = vec![head.clone(), middle.clone(), tail.clone()];
        let references = |runs: &[RawRun]| {
            runs.iter()
                .map(|run| FragmentRef {
                    run_id: run.id.clone(),
                    text: run.text.clone(),
                })
                .collect::<Vec<_>>()
        };

        let mut req = request(&head, "Alpha Beta", 100.0, &measure);
        req.range = references(&[middle.clone(), tail.clone()]);
        let writes = match validate_run_range(&all, &head, &req) {
            Ok(writes) => writes,
            Err(error) => return Err(error.to_string()),
        };
        assert_eq!(writes.len(), 2);
        assert_eq!(writes[0].id, middle.id);
        assert!(
            writes.iter().all(|write| write.new_bytes.is_empty()),
            "range runs are cleared, not rewritten"
        );

        // A different typeface anywhere in the range is a mixed selection. The range
        // reference resolves against the document's runs, so the document must hold the
        // different-style run — that is what makes the check meaningful.
        let mut other = tail.clone();
        other.style_class = "Helvetica".to_string();
        let mixed = vec![head.clone(), middle.clone(), other.clone()];
        let mut req = request(&head, "Alpha", 100.0, &measure);
        req.range = references(&[middle.clone(), other.clone()]);
        match validate_run_range(&mixed, &head, &req) {
            Err(error) => {
                assert_eq!(error.code(), "TEXT_RUN_NOT_EDITABLE");
                assert!(error.to_string().contains("selection-range"), "{error}");
            }
            Ok(_) => return Err("a mixed-typeface range must be refused".to_string()),
        }

        // So is a run that moved to another baseline (a wrapped line).
        let mut wrapped = tail.clone();
        wrapped.origin.1 -= 14.0;
        let shifted = vec![head.clone(), middle.clone(), wrapped.clone()];
        let mut req = request(&head, "Alpha", 100.0, &measure);
        req.range = references(&[wrapped]);
        assert!(validate_run_range(&shifted, &head, &req).is_err());

        // And so is an out-of-order or stale reference.
        let mut req = request(&head, "Alpha", 100.0, &measure);
        req.range = references(&[tail.clone(), middle.clone()]);
        assert!(validate_run_range(&all, &head, &req).is_err());

        let mut req = request(&head, "Alpha", 100.0, &measure);
        req.range = vec![FragmentRef {
            run_id: middle.id.clone(),
            text: "not what is there".to_string(),
        }];
        match validate_run_range(&all, &head, &req) {
            Err(error) => assert_eq!(error.code(), "TEXT_RUN_MISMATCH"),
            Ok(_) => return Err("a stale range reference must be refused".to_string()),
        }
        Ok(())
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

    /// B-7: undo takes the clone out again. After an edit of a named `ActualText` and its
    /// revert, the `BDC` names its original entry, the clone key is gone, the clone object is
    /// gone, and the shared entry never changed.
    #[test]
    fn revert_removes_the_property_clone() {
        let mut document = testpdf::named_property_shared_actualtext();
        let run =
            textlocate::locate_runs(&document, page_id(&document, 0).unwrap()).unwrap()[0].clone();
        let measure = measure_fixed(70.0);
        let outcome = edit(&mut document, &request(&run, "OMEGA", 100.0, &measure)).unwrap();

        let clone = outcome
            .inverse
            .property_clone
            .clone()
            .expect("a named ActualText is rewritten through a clone");
        assert!(outcome.actual_text_preserved);

        revert(&mut document, &outcome.inverse).unwrap();

        let page = page_id(&document, 0).unwrap();
        let after = textlocate::decoded_content(&document, page).unwrap();
        let names: Vec<&[u8]> = after
            .operations
            .iter()
            .filter(|operation| operation.operator == "BDC")
            .filter_map(|operation| operation.operands.get(1))
            .filter_map(|operand| operand.as_name().ok())
            .collect();
        assert_eq!(
            names,
            vec![&b"P0"[..], &b"P0"[..]],
            "both BDCs name /P0 again"
        );

        {
            let properties = textlocate::properties_mut(&mut document, page).unwrap();
            assert!(
                !properties.has(&clone.new_name),
                "the clone key is removed, so the file does not accumulate orphans"
            );
            assert!(properties.has(b"P0"), "the shared entry is still there");
        }
        assert!(
            !document.objects.contains_key(&clone.clone_object),
            "the clone object itself is dropped"
        );

        // And the run still reads its original semantic text.
        let runs = textlocate::locate_runs(&document, page).unwrap();
        assert_eq!(
            runs[0].marked_content[0].actual_text.as_deref(),
            Some("ALPHA")
        );
    }

    /// P2.7.3 X1: `nested-xobject` outranks `run-mismatch`. A placeholder has no PDFium box, so
    /// the generic unpaired arm would mask the real reason — the same precedence bug H0 avoided
    /// for `rotated-text`.
    #[test]
    fn the_nested_xobject_gate_outranks_run_mismatch() {
        let document = testpdf::xobject_cycle();
        let placeholder = {
            let page = page_id(&document, 0).unwrap();
            textlocate::locate_runs(&document, page)
                .unwrap()
                .into_iter()
                .find(|run| run.operator == "Do")
                .expect("the cycle placeholder")
        };
        // No PDFium box exists for the placeholder, so the unpaired arm is in play.
        let described = describe_single(&document, 0, &placeholder, None, &[]).unwrap();
        assert_eq!(
            described.admissibility.gate.as_deref(),
            Some("nested-xobject"),
            "the traversal refusal must be the reason the user sees, not `run-mismatch`"
        );
        assert!(!described.admissibility.editable);
        assert_eq!(described.rect, None);
        assert_eq!(described.width, None);
    }

    /// A run inside a form must not merge with a run drawn directly on the page, however well
    /// their geometry abuts: one write cannot clear both, because they are in different streams.
    #[test]
    fn a_form_run_does_not_merge_with_a_page_run() {
        let head = test_run("He", 72.0, 12.0, "F1");
        let mut next = test_run("llo", 72.0 + head.advance, 12.0, "F1");
        next.id.xobject_path = vec![textlocate::XObjectStep {
            name: b"X1".to_vec(),
            op_index: 1,
        }];
        assert!(
            !mergeable(&head, &next, false, None, None),
            "different addressing levels are different runs"
        );
    }

    /// Two placeholders are never one run, even at the same origin and size: they carry no
    /// text, and merging them would invent an editable address out of two refusals.
    #[test]
    fn two_placeholders_never_merge() {
        let head = test_run("/X1", 72.0, 12.0, "");
        let next = test_run("/X1", 72.0, 12.0, "");
        let mut head = head;
        head.xobject_refusal = Some(XObjectRefusal::Cycle);
        let mut next = next;
        next.xobject_refusal = Some(XObjectRefusal::Cycle);
        assert!(!mergeable(&head, &next, false, None, None));
    }

    /// P2.7.3 X2 replaces X1's refusal: a form-addressed run is now written — but only into the
    /// **form**. The page path indexes the page stream by `op_index`, which is the *form's*
    /// offset, so writing there would land the bytes in an unrelated page operation.
    ///
    /// The page stream does change in exactly one place: the `Do` that is repointed at the
    /// clone. Every text operand on it must still be what it was.
    #[test]
    fn editing_a_form_run_writes_the_form_and_not_the_page_stream() {
        let mut document = testpdf::xobject_single();
        let page = page_id(&document, 0).unwrap();
        let form_run = textlocate::locate_runs(&document, page)
            .unwrap()
            .into_iter()
            .find(|run| run.text == "ALPHA")
            .expect("the form's run is located");
        assert_eq!(form_run.id.xobject_path.len(), 1);

        // The page's own text, before and after: the fixture has none, which is the point —
        // an `op_index` from the form must not address anything here.
        let page_text = |document: &lopdf::Document| -> Vec<Vec<u8>> {
            textlocate::decoded_content(document, page)
                .unwrap()
                .operations
                .iter()
                .filter(|operation| matches!(operation.operator.as_str(), "Tj" | "TJ" | "'" | "\""))
                .filter_map(|operation| operation.operands.first())
                .filter_map(|operand| operand.as_str().ok().map(<[u8]>::to_vec))
                .collect()
        };
        assert!(page_text(&document).is_empty());

        edit(
            &mut document,
            &request(&form_run, "BRAVO", 60.0, &measure_fixed(60.0)),
        )
        .unwrap();

        assert!(
            page_text(&document).is_empty(),
            "no text operand appears on the page: the bytes went to the form"
        );
        assert_eq!(page_draws(&document, page), vec!["X1_edit1".to_string()]);
        assert_eq!(
            form_text(&document, page, &["X1_edit1"]).as_deref(),
            Some("BRAVO")
        );
    }

    // --- P2.7.3 X2: copy-on-write per draw ------------------------------------

    /// The `Do` operands a page draws, in order, as resource names.
    fn page_draws(document: &lopdf::Document, page: lopdf::ObjectId) -> Vec<String> {
        textlocate::decoded_content(document, page)
            .unwrap()
            .operations
            .iter()
            .filter(|operation| operation.operator == "Do")
            .filter_map(|operation| operation.operands.first())
            .filter_map(|operand| operand.as_name().ok())
            .map(|name| String::from_utf8_lossy(name).into_owned())
            .collect()
    }

    /// The text a form draws, reached by the resource names that get to it.
    fn form_text(
        document: &lopdf::Document,
        page: lopdf::ObjectId,
        path: &[&str],
    ) -> Option<String> {
        let operations = testpdf::form_operations(document, page, path)?;
        operations
            .iter()
            .find(|operation| operation.operator == "Tj")
            .and_then(|operation| operation.operands.first())
            .and_then(|operand| operand.as_str().ok())
            .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
    }

    /// Q2: the edit unit is the **draw**. Editing the instance the user clicked must change
    /// that instance and nothing else, so the other draw of the same form has to keep its own
    /// stream — byte for byte, not merely render the same.
    #[test]
    fn editing_one_draw_leaves_the_other_draw_byte_identical() {
        let mut document = testpdf::xobject_two_draws();
        let page = page_id(&document, 0).unwrap();

        let before: Vec<u8> = testpdf::form_stream(&document, page, &["X1"])
            .expect("the fixture draws X1")
            .decompressed_content()
            .unwrap();

        // Pick the second draw: "the other one" is then the first, and an in-place edit would
        // rewrite the stream both draws read. The second `Do` sits after the `cm`.
        let run = textlocate::locate_runs(&document, page)
            .unwrap()
            .into_iter()
            .filter(|run| run.text == "ALPHA")
            .max_by_key(|run| run.id.xobject_path[0].op_index)
            .expect("both draws are located");
        assert_eq!(run.id.xobject_path[0].op_index, 2);

        edit(
            &mut document,
            &request(&run, "BRAVO", 60.0, &measure_fixed(60.0)),
        )
        .unwrap();

        assert_eq!(
            page_draws(&document, page),
            vec!["X1".to_string(), "X1_edit1".to_string()],
            "only the clicked draw is repointed at the clone"
        );
        let after: Vec<u8> = testpdf::form_stream(&document, page, &["X1"])
            .expect("the original form is still there")
            .decompressed_content()
            .unwrap();
        assert_eq!(after, before, "the other draw's form is byte-identical");
        assert_eq!(
            form_text(&document, page, &["X1"]).as_deref(),
            Some("ALPHA")
        );
        assert_eq!(
            form_text(&document, page, &["X1_edit1"]).as_deref(),
            Some("BRAVO"),
            "the clicked draw reads the replacement"
        );
    }

    /// Q2 again, one level out: a form drawn on two pages must be copied for the page that was
    /// edited. §5.1 adds the second half — the pages share one indirect `/Resources`, so adding
    /// the clone's name must not be a write into an object the other page reads.
    #[test]
    fn editing_a_shared_form_on_one_page_leaves_the_other_page_untouched() {
        let mut document = testpdf::xobject_two_pages();
        let first = page_id(&document, 0).unwrap();
        let second = page_id(&document, 1).unwrap();

        let run = textlocate::locate_runs(&document, first)
            .unwrap()
            .into_iter()
            .find(|run| run.text == "ALPHA")
            .expect("page one's form run is located");
        edit(
            &mut document,
            &request(&run, "BRAVO", 60.0, &measure_fixed(60.0)),
        )
        .unwrap();

        assert_eq!(page_draws(&document, first), vec!["X1_edit1".to_string()]);
        assert_eq!(
            page_draws(&document, second),
            vec!["X1".to_string()],
            "page two still draws the original form"
        );
        assert_eq!(
            form_text(&document, second, &["X1"]).as_deref(),
            Some("ALPHA")
        );
        // The clone's name lives somewhere page one alone reads: page two's resources object
        // is not the one page one now names, so a rendering-neutral mutation is still a write
        // to another page's object (B-7).
        assert_ne!(
            resources_reference(&document, first),
            resources_reference(&document, second),
            "the edited page must not keep sharing its /Resources object"
        );
        assert_eq!(form_text(&document, second, &["X1_edit1"]).as_deref(), None);
    }

    /// The `/Resources` reference a page dictionary holds, as written.
    fn resources_reference(
        document: &lopdf::Document,
        page: lopdf::ObjectId,
    ) -> Option<lopdf::ObjectId> {
        document
            .get_dictionary(page)
            .ok()?
            .get(b"Resources")
            .ok()?
            .as_reference()
            .ok()
    }

    /// §2.1: a nested path is copied at **every** level, outermost first. Copying only the
    /// inner form would leave the outer one shared with the sibling draw that reached it.
    #[test]
    fn a_nested_run_is_editable_and_only_its_draw_changes() {
        let mut document = testpdf::xobject_nested();
        let page = page_id(&document, 0).unwrap();

        let run = textlocate::locate_runs(&document, page)
            .unwrap()
            .into_iter()
            .find(|run| run.text == "ALPHA")
            .expect("the nested run is located");
        assert_eq!(run.id.xobject_path.len(), 2);

        edit(
            &mut document,
            &request(&run, "BRAVO", 60.0, &measure_fixed(60.0)),
        )
        .unwrap();

        assert_eq!(page_draws(&document, page), vec!["X1_edit1".to_string()]);
        assert_eq!(
            form_text(&document, page, &["X1"]).as_deref(),
            None,
            "the original X1 draws X2, not text"
        );
        // The clone of X1 draws a clone of X2, and only that inner clone carries the edit.
        let outer =
            testpdf::form_operations(&document, page, &["X1_edit1"]).expect("the outer clone");
        let inner_names: Vec<String> = outer
            .iter()
            .filter(|operation| operation.operator == "Do")
            .filter_map(|operation| operation.operands.first())
            .filter_map(|operand| operand.as_name().ok())
            .map(|name| String::from_utf8_lossy(name).into_owned())
            .collect();
        assert_eq!(inner_names, vec!["X2_edit1".to_string()]);
        assert_eq!(
            form_text(&document, page, &["X2"]).as_deref(),
            Some("ALPHA")
        );
        assert_eq!(
            form_text(&document, page, &["X1_edit1", "X2_edit1"]).as_deref(),
            Some("BRAVO")
        );
    }

    /// Q3: the stream is copied always, `/Resources` only when the edit touches it — here a
    /// MECH-A substitution. The substitute must land in the **clone's** private dictionary, so
    /// the shared one another form reads is unchanged.
    #[test]
    fn a_substituted_font_gets_a_private_resources_dict() {
        let mut document = testpdf::xobject_shared_resources();
        let page = page_id(&document, 0).unwrap();

        let run = textlocate::locate_runs(&document, page)
            .unwrap()
            .into_iter()
            .find(|run| run.text == "ALPHA")
            .expect("X1's run is located");
        // 84pt is the substitute's width for BRAVO at 24pt, so the gate is not what this test
        // is about; the resources dictionary is.
        edit(
            &mut document,
            &request(&run, "BRAVO", 100.0, &measure_fixed(100.0)),
        )
        .unwrap();

        // The clone declares its own /Resources, and it is not the object X2 still reads.
        let clone = testpdf::form_stream(&document, page, &["X1_edit1"]).expect("the clone");
        let clone_resources = clone
            .dict
            .get(b"Resources")
            .ok()
            .and_then(|object| object.as_reference().ok())
            .expect("the clone has a private /Resources object");
        let original = testpdf::form_stream(&document, page, &["X2"]).expect("X2");
        let original_resources = original
            .dict
            .get(b"Resources")
            .ok()
            .and_then(|object| object.as_reference().ok())
            .expect("X2 shares one /Resources object");
        assert_ne!(clone_resources, original_resources);

        let fonts = document
            .get_dictionary(clone_resources)
            .unwrap()
            .get(b"Font")
            .ok()
            .and_then(|object| object.as_dict().ok())
            .expect("the private copy keeps the form's /Font map");
        assert!(
            fonts.has(b"FS1"),
            "the substitute is installed in the clone"
        );

        let shared = document
            .get_dictionary(original_resources)
            .unwrap()
            .get(b"Font")
            .ok()
            .and_then(|object| object.as_dict().ok())
            .expect("the shared /Font map");
        assert!(
            !shared.has(b"FS1"),
            "the shared dictionary another form reads gains nothing"
        );
    }

    /// Q6: one edit is one undo, and undo leaves **no orphan**. The `Do` goes back to the
    /// original name, the clone name leaves the resources dictionary, and the clone object is
    /// gone — so edit-then-undo returns the file to what it was, not to a grown version of it.
    #[test]
    fn undo_restores_the_do_and_drops_the_clone() {
        let mut document = testpdf::xobject_single();
        let page = page_id(&document, 0).unwrap();
        let objects_before = document.objects.len();

        let run = textlocate::locate_runs(&document, page)
            .unwrap()
            .into_iter()
            .find(|run| run.text == "ALPHA")
            .expect("the form's run is located");
        let outcome = edit(
            &mut document,
            &request(&run, "BRAVO", 60.0, &measure_fixed(60.0)),
        )
        .unwrap();
        assert_eq!(page_draws(&document, page), vec!["X1_edit1".to_string()]);

        revert(&mut document, &outcome.inverse).unwrap();

        assert_eq!(
            page_draws(&document, page),
            vec!["X1".to_string()],
            "the Do names the original form again"
        );
        assert_eq!(
            form_text(&document, page, &["X1"]).as_deref(),
            Some("ALPHA")
        );
        assert_eq!(
            form_text(&document, page, &["X1_edit1"]),
            None,
            "the clone name is gone from the resources"
        );
        assert_eq!(
            document.objects.len(),
            objects_before,
            "no clone object is left behind"
        );
    }

    /// Q6/Q3: an undo after a **substitution** must remove the private `/Resources` too. The
    /// write path creates that dictionary lazily, so the inverse has to record what was actually
    /// made — recording nothing would leak one resources object per substitute-then-undo.
    #[test]
    fn undo_after_a_substitution_leaves_no_orphan() {
        let mut document = testpdf::xobject_shared_resources();
        let page = page_id(&document, 0).unwrap();
        let before: std::collections::HashSet<lopdf::ObjectId> =
            document.objects.keys().copied().collect();
        let run = textlocate::locate_runs(&document, page)
            .unwrap()
            .into_iter()
            .find(|run| run.text == "ALPHA")
            .expect("the form's run is located");
        let outcome = edit(
            &mut document,
            &request(&run, "BRAVO", 100.0, &measure_fixed(100.0)),
        )
        .unwrap();
        let clone = outcome
            .inverse
            .xobject_clones
            .first()
            .expect("the edit copied the draw");
        assert!(
            clone.cloned_resources.is_some(),
            "a substitution gives the clone its own /Resources"
        );
        let private = clone.cloned_resources.expect("recorded above");
        assert_ne!(
            testpdf::form_stream(&document, page, &["X1_edit1"])
                .and_then(|form| form.dict.get(b"Resources").ok())
                .and_then(|entry| entry.as_reference().ok()),
            testpdf::form_stream(&document, page, &["X1"])
                .and_then(|form| form.dict.get(b"Resources").ok())
                .and_then(|entry| entry.as_reference().ok()),
            "the clone does not keep sharing the original's dictionary"
        );

        revert(&mut document, &outcome.inverse).unwrap();

        assert!(
            !document.objects.contains_key(&private),
            "the private /Resources object is dropped on undo"
        );
        assert!(
            !document.objects.contains_key(&clone.clone_object),
            "the clone object is dropped on undo"
        );
        // The standard-14 substitute dictionary stays, exactly as it does after a page edit's
        // undo: `/FS1` is reused across a page's substitutions, so it is shared state bounded
        // at one object, not an orphan this edit created.
        let leftovers: Vec<&lopdf::ObjectId> = document
            .objects
            .keys()
            .filter(|id| !before.contains(*id))
            .collect();
        assert_eq!(
            leftovers.len(),
            1,
            "only the page's shared substitute dictionary remains: {leftovers:?}"
        );
    }

    /// Q6/Q2: the inverse is draw-scoped. Undo must restore the draw it edited and leave the
    /// other draw — which was never touched — exactly as the edit left it.
    #[test]
    fn undo_leaves_the_other_draw_untouched() {
        let mut document = testpdf::xobject_two_draws();
        let page = page_id(&document, 0).unwrap();
        let other_before = testpdf::form_stream(&document, page, &["X1"])
            .expect("the fixture draws X1")
            .decompressed_content()
            .unwrap();

        let run = textlocate::locate_runs(&document, page)
            .unwrap()
            .into_iter()
            .filter(|run| run.text == "ALPHA")
            .max_by_key(|run| run.id.xobject_path[0].op_index)
            .expect("both draws are located");
        let outcome = edit(
            &mut document,
            &request(&run, "BRAVO", 60.0, &measure_fixed(60.0)),
        )
        .unwrap();
        revert(&mut document, &outcome.inverse).unwrap();

        assert_eq!(
            page_draws(&document, page),
            vec!["X1".to_string(), "X1".to_string()],
            "both draws name the original form again"
        );
        let other_after = testpdf::form_stream(&document, page, &["X1"])
            .expect("the original form is still there")
            .decompressed_content()
            .unwrap();
        assert_eq!(other_after, other_before, "the untouched draw is unchanged");
    }

    /// The placeholder's own `Do` is not text either: even if the UI somehow sent it, the
    /// refusal names the traversal gate rather than a font or encoding failure.
    #[test]
    fn editing_a_placeholder_is_refused_with_nested_xobject() {
        let mut document = testpdf::xobject_cycle();
        let page = page_id(&document, 0).unwrap();
        let placeholder = textlocate::locate_runs(&document, page)
            .unwrap()
            .into_iter()
            .find(|run| run.operator == "Do")
            .unwrap();
        let error = edit(
            &mut document,
            &request(&placeholder, "X", 10.0, &measure_fixed(10.0)),
        )
        .expect_err("a placeholder is not text");
        assert!(
            matches!(&error, AppError::TextRunNotEditable(message) if message.contains("nested-xobject")),
            "expected a nested-xobject refusal, got {error:?}"
        );
    }

    /// P2.7.3 X1: a selection may not span two addressing levels. Two runs can sit at
    /// consecutive `op_index` values, in one style, on one baseline, running rightward — and
    /// still be in different streams, where one write could never clear both.
    #[test]
    fn a_selection_may_not_span_two_streams() {
        let head = test_run("ALPHA", 72.0, 24.0, "F1");
        let mut tail = test_run("BETA", 72.0 + head.advance, 24.0, "F1");
        tail.id.op_index = head.id.op_index + 1;
        tail.id.xobject_path = vec![textlocate::XObjectStep {
            name: b"X1".to_vec(),
            op_index: 1,
        }];

        // The same geometry in one stream does *not* refuse: the stream check is the only
        // difference between this call and the refusing one below.
        let mut one_stream = tail.clone();
        one_stream.id.xobject_path.clear();
        let one_stream_all = vec![head.clone(), one_stream.clone()];
        let one_stream_request = EditRequest {
            page: 0,
            run_id: head.id.clone(),
            expected: head.text.clone(),
            replacement: "X".into(),
            old_width: 60.0,
            font_size: 24.0,
            allow_substitution: true,
            fragments: Vec::new(),
            range: vec![FragmentRef {
                run_id: one_stream.id.clone(),
                text: one_stream.text.clone(),
            }],
            measure: &measure_fixed(60.0),
        };
        assert!(
            validate_run_range(&one_stream_all, &head, &one_stream_request).is_ok(),
            "in one stream the same selection is a legitimate range"
        );

        let all = vec![head.clone(), tail.clone()];
        let request = EditRequest {
            range: vec![FragmentRef {
                run_id: tail.id.clone(),
                text: tail.text.clone(),
            }],
            ..one_stream_request
        };
        let Err(error) = validate_run_range(&all, &head, &request) else {
            panic!("a cross-stream selection must refuse");
        };
        assert_eq!(error.code(), "TEXT_RUN_NOT_EDITABLE");
        assert!(
            format!("{error:?}").contains("selection-range"),
            "expected selection-range, got {error:?}"
        );
    }
}
