//! MECH-A: standard-14 font substitution (P2.4 Stage 2).
//!
//! Stage 1 refused every run whose font was a subset (`font-subset`) because the embedded
//! program only carries the glyphs the original author used. Stage 2 converts most of those
//! refusals into edits by **switching the run to a standard-14 font**, which every
//! conformant viewer ships and whose WinAnsi coverage is total for Latin-1 text. No font
//! file is parsed, embedded, or licensed (T12/T15); the substitute is a bare
//! `/Type1 /WinAnsiEncoding` dictionary and a handful of AFM width constants.
//!
//! The three rules this module exists to enforce:
//!
//! * **Coverage before substitution** (T11). If the original subset font *actually*
//!   contains every glyph the replacement needs, the original font is kept. Substitution is
//!   the fallback, never the default — the substitute changes the document's appearance.
//! * **Style-match is deterministic** (O-ST2-6). Family comes from name tokens
//!   (`serif`→Times, `mono`/`courier`→Courier, else Helvetica) and weight/slant from
//!   `Bold`/`Italic` tokens. The subset prefix (`ABCDEF+`) is stripped **first**, or a
//!   prefixed name would never match a family token.
//! * **Refusals name the way out** (T3/R4). A replacement with a glyph neither the
//!   original nor any standard-14 font can supply refuses `GLYPHS_NOT_COVERABLE`; a user
//!   who turned substitution off refuses `SUBSTITUTE_REFUSED`. Both suggest the annotation
//!   tool, like every Stage-1 refusal.
//!
//! Coverage reads the embedded program with `ttf-parser` — the one new dependency, and the
//! only place a font program is parsed. It never writes a font.

use lopdf::{Document, Object, ObjectId};

use crate::editor::afm;
use crate::editor::textlocate::FontInfo;

/// The resolved plan for one edit's font: keep the original or substitute.
#[derive(Debug, Clone, PartialEq)]
pub enum Plan {
    KeepOriginal,
    Substitute { base_font: String, note: String },
}

/// The refusal code for a replacement that no available font can set.
pub const GLYPHS_NOT_COVERABLE: &str = "GLYPHS_NOT_COVERABLE";
/// The refusal code for a user who disabled substitution.
pub const SUBSTITUTE_REFUSED: &str = "SUBSTITUTE_REFUSED";

/// Decides whether a run's font may be kept, or must be substituted, for `replacement`.
///
/// Order is load-bearing (T11): the original font is kept whenever it covers the
/// replacement, *before* substitution is considered. `allow_substitution` is the O-ST2-4
/// toggle; when it is off and the original does not cover, the caller refuses
/// `SUBSTITUTE_REFUSED` rather than substituting.
pub fn plan(
    document: &Document,
    page_id: ObjectId,
    font_key: &[u8],
    info: &FontInfo,
    replacement: &str,
    allow_substitution: bool,
) -> Result<Plan, crate::model::AppError> {
    // T11: the original font wins if it can set the text. This is checked even when
    // substitution is allowed, and it is the *first* thing checked.
    if original_covers(document, page_id, font_key, info, replacement) {
        return Ok(Plan::KeepOriginal);
    }

    if !allow_substitution {
        return Err(crate::model::AppError::SubstituteRefused);
    }

    let base_font = style_match(info.base_font.as_deref());
    if !afm::covers(&base_font, replacement) {
        return Err(crate::model::AppError::GlyphsNotCoverable);
    }

    let original = info
        .base_font
        .clone()
        .unwrap_or_else(|| String::from("this run's font"));
    Ok(Plan::Substitute {
        note: format!("edited with {base_font} substituting {original}"),
        base_font,
    })
}

/// True when the run's *own* font can set `text` without a substitution.
///
/// Two cases, and the distinction is the whole of T11:
///
/// * **A standard-14 original** (`/BaseFont /Helvetica` and friends) has no program at
///   all, yet every conformant viewer draws any WinAnsi code for it. Coverage is therefore
///   the AFM table: an in-WinAnsi replacement is covered, exactly as Stage 1 assumed, and
///   the font is kept.
/// * **Any other original** (an embedded subset) is covered only when its program maps
///   every character — read with `ttf-parser`, never written. A font with no program here
///   is *not* trusted (a non-embedded subset), so the standard-14 substitute answers it.
fn original_covers(
    document: &Document,
    page_id: ObjectId,
    font_key: &[u8],
    info: &FontInfo,
    text: &str,
) -> bool {
    if let Some(base_font) = info.base_font.as_deref() {
        let stripped = strip_subset_prefix(base_font);
        if afm::widths_for(stripped).is_some() {
            return afm::covers(stripped, text);
        }
    }

    let Some(program) = embedded_font_program(document, page_id, font_key) else {
        return false;
    };
    let Ok(face) = ttf_parser::Face::parse(&program, 0) else {
        return false;
    };
    text.chars().all(|ch| face.glyph_index(ch).is_some())
}

/// The bytes of the run font's embedded program, if it has one.
///
/// A `/FontDescriptor /FontFile2` (TrueType) or `/FontFile3` (OpenType) stream is the only
/// place an embedded simple font keeps its glyphs. `/FontFile` (Type 1) is not a TrueType
/// program and `ttf-parser` cannot read it, so it is deliberately not returned.
pub(crate) fn embedded_font_program(
    document: &Document,
    page_id: ObjectId,
    font_key: &[u8],
) -> Option<Vec<u8>> {
    let fonts = document.get_page_fonts(page_id).ok()?;
    let font = fonts.get(font_key).copied()?;

    let descriptor = font
        .get(b"FontDescriptor")
        .ok()
        .and_then(|object| deref_object(document, object))
        .and_then(|object| object.as_dict().ok())?;

    for key in [b"FontFile2".as_slice(), b"FontFile3".as_slice()] {
        if let Ok(stream) = descriptor.get(key) {
            if let Some(bytes) = stream_bytes(document, stream) {
                return Some(bytes);
            }
        }
    }
    None
}

/// Follows a reference to the object it names, leaving an inline object as-is.
fn deref_object<'a>(document: &'a Document, object: &'a Object) -> Option<&'a Object> {
    match object {
        Object::Reference(id) => document.get_object(*id).ok(),
        other => Some(other),
    }
}

/// A stream object's bytes, following a reference if needed.
///
/// `Stream::content` is already decompressed by lopdf when the filter is known (Flate for
/// a typical `FontFile2`); an uncompressed program passes through unchanged. A filter
/// lopdf cannot decode leaves the bytes opaque, so the coverage check degrades to "not
/// covered" — a refusal, never a panic.
fn stream_bytes(document: &Document, object: &Object) -> Option<Vec<u8>> {
    let stream = match object {
        Object::Reference(id) => document.get_object(*id).ok()?.as_stream().ok()?,
        other => other.as_stream().ok()?,
    };
    Some(stream.content.clone())
}

/// Strips a subset prefix from a `/BaseFont` name.
///
/// The convention is six uppercase letters, then `+` (e.g. `ABCDEF+TimesNewRoman`). The
/// prefix must go *before* style matching or the leading letters would be read as part of
/// the family name — the bug this function's test pins.
pub fn strip_subset_prefix(name: &str) -> &str {
    let bytes = name.as_bytes();
    if bytes.len() > 7 && bytes[6] == b'+' && bytes[..6].iter().all(u8::is_ascii_uppercase) {
        return &name[7..];
    }
    name
}

/// The standard-14 `/BaseFont` that best matches an original font name (O-ST2-6).
///
/// Deterministic and disclosed: the design's documented token table, not a "closest
/// match" mystery map. Family by substring — `serif`/`times`→Times, `mono`/`courier`→
/// Courier, else Helvetica — then a `-Bold`/`-Italic`/`-BoldItalic` suffix from the
/// weight/slant tokens. The subset prefix is stripped **first** ([`strip_subset_prefix`]),
/// or the six prefix letters would be scanned as part of the name.
pub fn style_match(base_font: Option<&str>) -> String {
    let name = base_font.map(strip_subset_prefix).unwrap_or("");
    let lower = name.to_ascii_lowercase();

    let family = if lower.contains("serif") || lower.contains("times") {
        "Times"
    } else if lower.contains("mono") || lower.contains("courier") {
        "Courier"
    } else {
        "Helvetica"
    };

    let bold = lower.contains("bold") || lower.contains("black") || lower.contains("heavy");
    let italic = lower.contains("italic") || lower.contains("oblique");

    match (family, bold, italic) {
        ("Times", false, false) => "Times-Roman",
        ("Times", true, false) => "Times-Bold",
        ("Times", false, true) => "Times-Italic",
        ("Times", _, _) => "Times-BoldItalic",
        ("Courier", false, false) => "Courier",
        ("Courier", true, false) => "Courier-Bold",
        ("Courier", false, true) => "Courier-Oblique",
        ("Courier", _, _) => "Courier-BoldOblique",
        ("Helvetica", false, false) => "Helvetica",
        ("Helvetica", true, false) => "Helvetica-Bold",
        ("Helvetica", false, true) => "Helvetica-Oblique",
        _ => "Helvetica-BoldOblique",
    }
    .to_string()
}

/// The measured width of `text` in a standard-14 font at `font_size` points.
///
/// The width gate (T13) uses this for the FINAL font when MECH-A substitutes, so the gate
/// compares the substitute's metrics — not the original's — exactly as O1 requires.
pub fn substitute_width(base_font: &str, text: &str, font_size: f32) -> Option<f32> {
    afm::text_width(base_font, text, font_size)
}

/// A font's *in-file* metric source, for exact advance measurement (P2.4.2 Phase A / O-ST2-2).
///
/// The 0.4.1 merge predicate measured fragment gaps with `fragment_advance`, which for a
/// non-standard font with no AFM table fell back to `chars * 0.5em`. That over-estimate — real
/// glyphs in a proportional face average closer to `0.5em` only for the narrowest letters —
/// inflated `head.advance`, pushed the next fragment's computed gap negative, and **broke the
/// merge chain** at exactly the boundary the user's `Sci|ence` line showed. The fix is the one
/// the design froze: read the width out of the font file the run already carries.
///
/// The variants are ordered by authority, and [`FontMetrics::advance`] walks them in that order:
///
/// * [`FontMetrics::Afm`] — a standard-14 font answers from the pinned AFM tables. No program
///   involved; the standard-14 metric *is* the constant table.
/// * [`FontMetrics::TrueType`] — an embedded simple font's program, parsed with `ttf-parser`
///   (the same parser [`plan`] already uses for coverage — reused, never a second parse path).
/// * [`FontMetrics::Nominal`] — no program could be read or parsed. This is **not** a silent
///   approximation: [`FontMetrics::advance`] returns `None`, and the caller treats that as "no
///   metric" rather than a width (T22's refuse-when-nothing-is-measurable direction).
pub enum FontMetrics {
    /// A standard-14 font measured from the pinned AFM tables.
    Afm { base_font: String },
    /// An embedded font measured from its own `hmtx`, via the program `ttf-parser` reads.
    TrueType { program: Vec<u8> },
    /// No usable metric source: the caller will be handed `None` rather than a guess.
    Nominal,
}

impl FontMetrics {
    /// Resolves the best available metric source for one run font.
    ///
    /// The AFM table is tried first because a standard-14 run has no program at all; an
    /// embedded subset with a real `FontFile2`/`FontFile3` is next. A run whose font is
    /// neither (a non-embedded, non-standard-14 face) gets [`FontMetrics::Nominal`].
    pub fn resolve(document: &Document, page_id: ObjectId, font_key: &[u8]) -> FontMetrics {
        if let Some(info) = crate::editor::textlocate::font_info(document, page_id, font_key) {
            if let Some(base_font) = info.base_font.as_deref() {
                let stripped = strip_subset_prefix(base_font);
                if afm::widths_for(stripped).is_some() {
                    return FontMetrics::Afm {
                        base_font: stripped.to_string(),
                    };
                }
            }
        }
        if let Some(program) = embedded_font_program(document, page_id, font_key) {
            return FontMetrics::TrueType { program };
        }
        FontMetrics::Nominal
    }

    /// The exact advance width of `text` in points, or `None` when no in-file metric exists.
    ///
    /// `None` is load-bearing (T22): callers must *refuse* rather than substitute a width, so a
    /// run that cannot be measured is never quoted a quota the renderer will disagree with.
    pub fn advance(&self, text: &str, font_size: f32) -> Option<f32> {
        match self {
            FontMetrics::Afm { base_font } => afm::text_width(base_font, text, font_size),
            FontMetrics::TrueType { program } => true_type_advance(program, text, font_size),
            FontMetrics::Nominal => None,
        }
    }
}

/// Sums the `hmtx` advances of `text` in an embedded TrueType program.
///
/// A character the program does not map (no `cmap` entry) makes the whole measurement `None`:
/// the run's own font genuinely cannot set that text, so an advance for it would be fiction.
/// Widths are font units scaled by `font_size / unitsPerEm`, exactly as a viewer lays them out.
fn true_type_advance(program: &[u8], text: &str, font_size: f32) -> Option<f32> {
    let face = ttf_parser::Face::parse(program, 0).ok()?;
    let units_per_em = f32::from(face.units_per_em());
    if units_per_em <= 0.0 {
        return None;
    }
    let mut units: f32 = 0.0;
    for ch in text.chars() {
        let glyph = face.glyph_index(ch)?;
        let advance = face.glyph_hor_advance(glyph)?;
        units += f32::from(advance);
    }
    Some(units * font_size / units_per_em)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::testpdf;
    use crate::editor::textlocate;

    #[test]
    fn the_subset_prefix_is_stripped_before_matching() {
        assert_eq!(strip_subset_prefix("ABCDEF+TimesNewRoman"), "TimesNewRoman");
        assert_eq!(strip_subset_prefix("Helvetica"), "Helvetica");
        // Lowercase letters are not a subset prefix; nothing is stripped.
        assert_eq!(strip_subset_prefix("abcdef+X"), "abcdef+X");
        // Too short to carry a prefix.
        assert_eq!(strip_subset_prefix("AB+X"), "AB+X");
    }

    /// The bug the requirement calls out: a subset-prefixed serif name must still match
    /// the serif family. Without stripping, `ABCDEF+Times...` would classify as Helvetica.
    #[test]
    fn subset_prefixed_names_still_match_their_family() {
        assert_eq!(style_match(Some("ABCDEF+TimesNewRoman")), "Times-Roman");
        assert_eq!(style_match(Some("ABCDEF+TimesNewRoman-Bold")), "Times-Bold");
        assert_eq!(style_match(Some("ABCDEF+CourierNew")), "Courier");
        assert_eq!(
            style_match(Some("ABCDEF+Courier-BoldOblique")),
            "Courier-BoldOblique"
        );
        assert_eq!(style_match(Some("ABCDEF+DejaVuSerif")), "Times-Roman");
        assert_eq!(style_match(Some("ABCDEF+Arial")), "Helvetica");
        assert_eq!(style_match(Some("ABCDEF+Arial-Bold")), "Helvetica-Bold");
        assert_eq!(
            style_match(Some("ABCDEF+Arial-Italic")),
            "Helvetica-Oblique"
        );
    }

    #[test]
    fn style_match_reads_weight_and_slant_tokens() {
        assert_eq!(
            style_match(Some("TimesNewRomanPS-BoldItalicMT")),
            "Times-BoldItalic"
        );
        assert_eq!(
            style_match(Some("HelveticaNeue-Oblique")),
            "Helvetica-Oblique"
        );
        assert_eq!(style_match(Some("LiberationMono")), "Courier");
        assert_eq!(style_match(None), "Helvetica");
    }

    /// T11: a subset font that *does* contain the replacement's glyphs keeps the original.
    #[test]
    fn a_subset_that_covers_the_replacement_keeps_the_original_font() {
        let glyphs: Vec<char> = "HELLOWORLD".chars().collect();
        let document = testpdf::embedded_subset_page("HELLO", &glyphs);
        let page_id = textlocate::page_id(&document, 0).unwrap();
        let run = &textlocate::locate_runs(&document, page_id).unwrap()[0];
        let info = textlocate::font_info(&document, page_id, &run.font_key).unwrap();

        // "WOOD" uses only glyphs the program has.
        let plan = plan(&document, page_id, &run.font_key, &info, "WOOD", true).unwrap();
        assert_eq!(plan, Plan::KeepOriginal);
    }

    /// T11: a subset that lacks a needed glyph substitutes rather than keeping the font.
    #[test]
    fn a_subset_missing_a_glyph_substitutes() {
        let glyphs: Vec<char> = "HELLO".chars().collect();
        let document = testpdf::embedded_subset_page("HELLO", &glyphs);
        let page_id = textlocate::page_id(&document, 0).unwrap();
        let run = &textlocate::locate_runs(&document, page_id).unwrap()[0];
        let info = textlocate::font_info(&document, page_id, &run.font_key).unwrap();

        // "HELP" needs P, which the program does not map; and the name has no family
        // token, so the substitute is Helvetica.
        match plan(&document, page_id, &run.font_key, &info, "HELP", true).unwrap() {
            Plan::Substitute { base_font, note } => {
                assert_eq!(base_font, "Helvetica");
                assert!(note.contains("Helvetica"), "{note}");
                assert!(note.contains("ABCDEF+FixtureSubset"), "{note}");
            }
            other => panic!("expected a substitution, got {other:?}"),
        }
    }

    /// O-ST2-4: with the toggle off and the original unable to cover, refuse.
    #[test]
    fn substitution_off_refuses_rather_than_substituting() {
        let glyphs: Vec<char> = "HELLO".chars().collect();
        let document = testpdf::embedded_subset_page("HELLO", &glyphs);
        let page_id = textlocate::page_id(&document, 0).unwrap();
        let run = &textlocate::locate_runs(&document, page_id).unwrap()[0];
        let info = textlocate::font_info(&document, page_id, &run.font_key).unwrap();

        let error = plan(&document, page_id, &run.font_key, &info, "HELP", false).unwrap_err();
        assert_eq!(error.code(), "SUBSTITUTE_REFUSED");
        assert!(
            error.to_string().contains("add a text annotation"),
            "{error}"
        );
    }

    /// O-ST2-2 / P2.4.2 Phase A: the metric source is the font file, not an approximation. A
    /// standard-14 font answers from the AFM tables; an embedded subset answers from its own
    /// `hmtx`; a font with neither is `Nominal` and its `advance` is `None` (never a guess).
    #[test]
    fn font_metrics_come_from_the_font_file_never_an_approximation() {
        // A standard-14 font: AFM, exact, and `Sci` at 12pt is Helvetica's 3-glyph width.
        let helvetica = testpdf::simple_winansi();
        let page_id = textlocate::page_id(&helvetica, 0).unwrap();
        let metrics = FontMetrics::resolve(&helvetica, page_id, b"F1");
        match &metrics {
            FontMetrics::Afm { base_font } => assert_eq!(base_font, "Helvetica"),
            _ => panic!("expected AFM metrics, got a different source"),
        }
        let afm_width = metrics
            .advance("Sci", 12.0)
            .expect("AFM text is measurable");
        // Helvetica S=667, c=500, i=222 /1000 em, at 12pt.
        let expected = (667.0 + 500.0 + 222.0) * 12.0 / 1000.0;
        assert!(
            (afm_width - expected).abs() < 0.01,
            "{afm_width} vs {expected}"
        );

        // An embedded subset: hmtx, and a `None` for a character the program does not map.
        let glyphs: Vec<char> = "Sci".chars().collect();
        let subset = testpdf::embedded_subset_page("Sci", &glyphs);
        let page_id = textlocate::page_id(&subset, 0).unwrap();
        let metrics = FontMetrics::resolve(&subset, page_id, b"F1");
        match &metrics {
            FontMetrics::TrueType { .. } => {}
            _ => panic!("an embedded program must be measured from its own hmtx"),
        }
        // The fixture program's advance is 600/1000 em per glyph, at 24pt the default run size.
        let hmtx_width = metrics
            .advance("Sci", 24.0)
            .expect("mapped glyphs are measurable");
        let expected = 3.0 * 600.0 * 24.0 / 1000.0;
        assert!(
            (hmtx_width - expected).abs() < 0.01,
            "{hmtx_width} vs {expected}"
        );
        assert!(
            metrics.advance("Z", 24.0).is_none(),
            "an unmapped glyph has no advance, so the caller refuses rather than guessing"
        );
    }

    /// MECH-B's absence: a glyph outside WinAnsi refuses with `GLYPHS_NOT_COVERABLE`.
    #[test]
    fn a_non_winansi_glyph_is_not_coverable() {
        let glyphs: Vec<char> = "HI".chars().collect();
        let document = testpdf::embedded_subset_page("HI", &glyphs);
        let page_id = textlocate::page_id(&document, 0).unwrap();
        let run = &textlocate::locate_runs(&document, page_id).unwrap()[0];
        let info = textlocate::font_info(&document, page_id, &run.font_key).unwrap();

        // U+2192 (→) has no WinAnsi code.
        let error = plan(&document, page_id, &run.font_key, &info, "H\u{2192}", true).unwrap_err();
        assert_eq!(error.code(), "GLYPHS_NOT_COVERABLE");
        assert!(
            error.to_string().contains("add a text annotation"),
            "{error}"
        );
    }
}
