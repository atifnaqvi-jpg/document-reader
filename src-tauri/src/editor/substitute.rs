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
//! * **Refusals name the way out** (T3/R4). Four refusals, four remedies, and the remedy is
//!   in the code's name because the next person reads the code, not this comment:
//!   `SUBSTITUTE_REFUSED` (say yes), `GLYPHS_NOT_COVERABLE` (choose a different font),
//!   `SUBSET_BUILD_FAILED` (report it; our bug), and `TYPE0_EMBED_UNIMPLEMENTED` (this build
//!   cannot write a CID subset yet — not a fact about the document or the bundle).
//!
//! Coverage reads the embedded program with `ttf-parser` — the one new dependency, and the
//! only place a font program is parsed. It never writes a font; [`crate::editor::embed`] does.

use lopdf::{Dictionary, Document, Object, ObjectId};

use crate::editor::afm;
use crate::editor::textlocate::FontInfo;

/// The resolved plan for one edit's font.
#[derive(Debug, Clone, PartialEq)]
pub enum Plan {
    KeepOriginal,
    /// MECH-A: a standard-14 reference. Costs no bytes and every viewer has the face.
    Substitute {
        base_font: String,
        note: String,
    },
    /// MECH-B: a subset of a bundled face, written as a new simple `/TrueType` resource.
    ///
    /// Ordered after [`Plan::Substitute`] on purpose (design §2.1): a WinAnsi replacement is
    /// set by a reference every viewer already has, and reaching for a subset first would
    /// bloat every ordinary edit.
    Embed {
        base_font: String,
        subset: crate::editor::embed::Subset,
        note: String,
    },
}

/// The refusal code for a replacement that no available font can set.
pub const GLYPHS_NOT_COVERABLE: &str = "GLYPHS_NOT_COVERABLE";
/// The refusal code for a user who disabled substitution.
pub const SUBSTITUTE_REFUSED: &str = "SUBSTITUTE_REFUSED";
/// MECH-B (P2.7.6): the run is `Type0`, and writing an embedded CID subset is not implemented
/// in this build. Deliberately separate from the other three: it is a capability gap in this
/// binary, not a property of the document, and a later version will remove the code rather
/// than change what it means.
pub const TYPE0_EMBED_UNIMPLEMENTED: &str = "TYPE0_EMBED_UNIMPLEMENTED";

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
    let font = document
        .get_page_fonts(page_id)
        .ok()
        .and_then(|fonts| fonts.get(font_key).copied().cloned());
    plan_with_font(
        document,
        font.as_ref(),
        info,
        replacement,
        allow_substitution,
    )
}

/// The same decision, from a font dictionary resolved at the level the run is addressed in
/// (P2.7.3 X2).
///
/// A run inside a form usually names a font the **page** does not: [`plan`] would resolve no
/// program and substitute a standard-14 face for text the original sets perfectly. The caller
/// has the dictionary already — it read it to decode the bytes — so it passes it here.
pub fn plan_with_font(
    document: &Document,
    font: Option<&lopdf::Dictionary>,
    info: &FontInfo,
    replacement: &str,
    allow_substitution: bool,
) -> Result<Plan, crate::model::AppError> {
    // T11: the original font wins if it can set the text. This is checked even when
    // substitution is allowed, and it is the *first* thing checked.
    if original_covers(document, font, info, replacement) {
        return Ok(Plan::KeepOriginal);
    }

    if !allow_substitution {
        return Err(crate::model::AppError::SubstituteRefused);
    }

    let original = info
        .base_font
        .clone()
        .unwrap_or_else(|| String::from("this run's font"));

    // MECH-A first: a standard-14 reference is the cheap path and stays cheap (R4).
    let base_font = style_match(info.base_font.as_deref());
    if afm::covers(&base_font, replacement) {
        return Ok(Plan::Substitute {
            note: format!("edited with {base_font} substituting {original}"),
            base_font,
        });
    }

    // MECH-B. Each step names a different fact with a different remedy, ordered from the most
    // permanent fact to the most transient:
    //
    // 1. no bundled face covering a character is permanent within the bundle, and stays true
    //    even after the Type0 writer lands, so it is checked first;
    // 2. a Type0 run would need a CID writer this build does not have -- our gap, temporary;
    // 3. a face that has the glyph but fails to subset is our defect.
    let Some(face) = crate::editor::embed::face_for_style(&base_font) else {
        return Err(crate::model::AppError::GlyphsNotCoverable);
    };
    if !crate::editor::embed::covers(face, replacement) {
        return Err(crate::model::AppError::GlyphsNotCoverable);
    }
    if info
        .subtype
        .as_deref()
        .is_some_and(|subtype| subtype.eq_ignore_ascii_case("type0"))
    {
        return Err(crate::model::AppError::Type0EmbedUnimplemented);
    }
    let subset = crate::editor::embed::subset(face, replacement)
        .map_err(crate::model::AppError::SubsetBuildFailed)?;

    Ok(Plan::Embed {
        // The prefixed name, exactly as written to `/BaseFont`: the note is what a user would
        // quote in a bug report, and "Sans-Regular" is not in the file.
        note: format!(
            "edited with an embedded subset of {} substituting {original}",
            subset.base_font
        ),
        base_font: subset.base_font.clone(),
        subset,
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
    font: Option<&lopdf::Dictionary>,
    info: &FontInfo,
    text: &str,
) -> bool {
    if let Some(base_font) = info.base_font.as_deref() {
        let stripped = strip_subset_prefix(base_font);
        if afm::widths_for(stripped).is_some() {
            return afm::covers(stripped, text);
        }
    }

    // P2.7.5: a Type0 font keeps its glyphs on the descendant, not on the root dictionary that
    // `embedded_program` reads, so coverage is the CID codec's answer: `/ToUnicode` first,
    // then (under `/Identity`) the program's own `cmap`.
    if let Some(font) = font {
        if let Some(cid) = crate::editor::cid::CidFont::resolve(document, font) {
            return cid.covers(text);
        }
    }

    let Some(program) = font.and_then(|font| embedded_program(document, font)) else {
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
pub(crate) fn embedded_program(document: &Document, font: &lopdf::Dictionary) -> Option<Vec<u8>> {
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

/// A stream object's bytes, following a reference if needed and **decompressing** them.
///
/// `Stream::content` is the raw stream body: for a typical Flate `FontFile2` that is still
/// compressed, and `ttf-parser` cannot read it. Reading it raw is not a corner case — it was
/// the only case for every real PDF, so coverage always fell through to "not covered" and
/// T11 substituted a font the original could have set (P2.7.5 §1.1b). An uncompressed program
/// decompresses to itself. A filter lopdf cannot decode leaves the bytes opaque, so the
/// coverage check degrades to "not covered" — a refusal, never a panic.
fn stream_bytes(document: &Document, object: &Object) -> Option<Vec<u8>> {
    let mut stream = match object {
        Object::Reference(id) => document.get_object(*id).ok()?.as_stream().ok()?.clone(),
        other => other.as_stream().ok()?.clone(),
    };
    // A stream with no filter is already plain; `decompress` would only clone it.
    if !stream.dict.get(b"Filter").map(|_| true).unwrap_or(false) {
        return Some(stream.content);
    }
    stream.decompress().ok().map(|_| stream.content)
}

/// Decodes one-byte `bytes` through a simple font's **own `/ToUnicode`** when it has one.
///
/// lopdf resolves a font's `/Encoding /Differences` by looking each glyph name up in its own
/// Adobe glyph list. That list has no `uniXXXX` convention, so a `/Differences` written by
/// [`crate::editor::embed`] makes `get_font_encoding` fail — and it then falls back to
/// StandardEncoding, silently. Reading our own embedded subsets through that fallback would
/// decode code `0x01` as `A` rather than U+0100, which breaks undo (the inverse is validated
/// against decoded text) and re-editing.
///
/// `/ToUnicode` is the authority here, and not only for our writer: the PDF format makes it
/// the semantic mapping of a code, so preferring it is the general rule rather than a
/// workaround for one producer. Returns `None` when the font has no `/ToUnicode`, or when the
/// bytes contain a code it does not name — refusing to guess is the direction every other gate
/// in this module takes.
pub fn decode_simple(document: &Document, font: &Dictionary, bytes: &[u8]) -> Option<String> {
    // The guard belongs here, in the one-byte reader, not in each caller: a Type0 CMap maps
    // two-byte codes, and splitting them can produce a plausible string from the wrong bytes
    // (a `<00> <0041>` entry turns code `0002` into `AA`). Two-byte decoding belongs to
    // `cid::CidFont`.
    if is_type0(font) {
        return None;
    }
    if let Some(map) = to_unicode_map(document, font) {
        let mut out = String::new();
        for byte in bytes {
            out.push(map.character(u16::from(*byte))?);
        }
        return Some(out);
    }
    // No `/ToUnicode`: fall back to the font's `/Encoding`, which is what every pre-P2.7.6
    // simple font relies on and what lopdf reads correctly for a name-based `/Differences`.
    font.get_font_encoding(document)
        .ok()?
        .bytes_to_string(bytes)
        .ok()
}

/// Encodes `text` through a simple font's `/ToUnicode`, the inverse of [`decode_simple`].
///
/// A character the font's CMap does not name is `None`, never a dropped character: writing
/// fewer bytes than the user asked for while reporting success is the silent-corruption class
/// this whole path exists to avoid. Without a `/ToUnicode` this defers to lopdf's encoding,
/// whose `string_to_bytes` may drop characters, so the caller still has to round-trip.
pub fn encode_simple(document: &Document, font: &Dictionary, text: &str) -> Option<Vec<u8>> {
    let Some(map) = to_unicode_map(document, font) else {
        let encoding = font.get_font_encoding(document).ok()?;
        return Some(encoding.string_to_bytes(text));
    };
    let mut out = Vec::with_capacity(text.len());
    for ch in text.chars() {
        let code = map.code(ch)?;
        out.push(u8::try_from(code).ok()?);
    }
    Some(out)
}

/// True for a composite (CID) font, whose codes are more than one byte.
pub fn is_type0(font: &Dictionary) -> bool {
    font.get(b"Subtype")
        .and_then(Object::as_name)
        .is_ok_and(|subtype| subtype.eq_ignore_ascii_case(b"Type0"))
}
/// True when the font carries a readable `/ToUnicode`.
///
/// The condition is "we have a CMap", not "we are the ones who wrote it": a producer's CMap is
/// just as authoritative, and a font whose CMap fails to parse falls back to `/Encoding`.
pub fn has_to_unicode(document: &Document, font: &Dictionary) -> bool {
    to_unicode_map(document, font).is_some()
}

/// Parses a font's `/ToUnicode` stream with the CMap reader P2.7.5 already has.
///
/// One-byte and two-byte codes are both `<hex>` tokens, and [`crate::editor::cid::CodeMap`]
/// parses any width; a simple font only ever asks about codes below 256.
fn to_unicode_map(document: &Document, font: &Dictionary) -> Option<crate::editor::cid::CodeMap> {
    let stream = font
        .get(b"ToUnicode")
        .ok()
        .and_then(|object| deref_object(document, object))
        .and_then(|object| object.as_stream().ok())?;
    let content = match stream.decompressed_content() {
        Ok(content) => content,
        Err(_) => stream.content.clone(),
    };
    crate::editor::cid::CodeMap::parse(&content)
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
    /// A simple font measured from the `/Widths` array the PDF itself carries, indexed from
    /// `/FirstChar`. This is the only source for a *non-embedded* face (the Maastricht
    /// brochure's Times/Arial), and it is the same array a viewer uses to lay the run out.
    SimpleWidths { first_char: u16, widths: Vec<u16> },
    /// A CID/Type0 font measured from `/W` then `/DW` (P2.7.5). The widths are per **cid**, so
    /// measurement encodes the text first — the same codes the write path will install.
    Cid { font: crate::editor::cid::CidFont },
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
        let Some(font) = crate::editor::textlocate::font_dict(document, page_id, font_key) else {
            return FontMetrics::Nominal;
        };
        let base_font = font
            .get(b"BaseFont")
            .ok()
            .and_then(|object| object.as_name().ok())
            .map(|name| String::from_utf8_lossy(name).into_owned());
        FontMetrics::resolve_with(document, font, base_font.as_deref())
    }

    /// The same choice, from a font dictionary already resolved at the level that declares it
    /// (P2.7.3 X1).
    ///
    /// A run inside a Form XObject must be measured through **that form's** `/Font` entry: the
    /// page's table may not name the resource at all, and `get_page_fonts` would answer with a
    /// different face under the same name.
    pub fn resolve_with(
        document: &Document,
        font: &lopdf::Dictionary,
        base_font: Option<&str>,
    ) -> FontMetrics {
        if let Some(base_font) = base_font {
            let stripped = strip_subset_prefix(base_font);
            if afm::widths_for(stripped).is_some() {
                return FontMetrics::Afm {
                    base_font: stripped.to_string(),
                };
            }
        }
        if let Some(cid) = crate::editor::cid::CidFont::resolve(document, font) {
            return FontMetrics::Cid { font: cid };
        }
        if let Some(program) = embedded_program(document, font) {
            return FontMetrics::TrueType { program };
        }
        // A non-embedded face still publishes its advances in `/Widths`. Reading them is the
        // difference between an exact seam and a phantom one: the Maastricht Times/Arial
        // resources are in this class, and before this they resolved to `Nominal`.
        if let Some((first_char, widths)) = widths_of(document, font) {
            return FontMetrics::SimpleWidths { first_char, widths };
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
            FontMetrics::SimpleWidths { first_char, widths } => {
                simple_widths_advance(*first_char, widths, text, font_size)
            }
            FontMetrics::Cid { font } => font
                .encode(text)
                .ok()
                .and_then(|codes| font.advance(&codes, font_size)),
            FontMetrics::Nominal => None,
        }
    }
}

/// Reads a PDF number that may have been written as an integer or a real.
fn number(object: &Object) -> Option<f32> {
    match object {
        Object::Integer(value) => Some(*value as f32),
        Object::Real(value) => Some(*value),
        _ => None,
    }
}

/// The `/Widths` of an already-resolved font dictionary.
///
/// Returns `None` when it has no `/Widths` (the Maastricht `F2`/`F9` Arial resources are in
/// that class, and they must stay unmeasurable rather than be guessed at).
fn widths_of(document: &Document, font: &lopdf::Dictionary) -> Option<(u16, Vec<u16>)> {
    // `/Widths` is routinely written as an indirect array, and `font.get` hands back the
    // reference rather than the array; resolve it before reading.
    let resolve = |object: &Object| -> Option<Object> {
        match object {
            Object::Reference(id) => document.get_object(*id).ok().cloned(),
            other => Some(other.clone()),
        }
    };
    let first_char = number(&resolve(font.get(b"FirstChar").ok()?)?)?;
    let array = resolve(font.get(b"Widths").ok()?)?;
    let array = array.as_array().ok()?;
    let widths = array
        .iter()
        .map(|width| number(width).map(|w| w.round() as u16))
        .collect::<Option<Vec<u16>>>()?;
    if widths.is_empty() {
        return None;
    }
    Some((first_char.round() as u16, widths))
}

/// Sums the published `/Widths` of `text` in a simple font.
///
/// A code outside the `/FirstChar` range, or one whose published width is zero, makes the
/// whole measurement `None`: the font dictionary does not describe that glyph, so an advance
/// for it would be fiction — the same contract [`true_type_advance`] holds for a missing
/// `cmap` entry.
fn simple_widths_advance(
    first_char: u16,
    widths: &[u16],
    text: &str,
    font_size: f32,
) -> Option<f32> {
    let mut thousandths: f32 = 0.0;
    for code in text.bytes() {
        let index = (code as u16).checked_sub(first_char)? as usize;
        let width = *widths.get(index)?;
        if width == 0 {
            return None;
        }
        thousandths += f32::from(width);
    }
    Some(thousandths * font_size / 1000.0)
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
        assert!(error.to_string().contains("annotation"), "{error}");
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

    /// The regression `/Widths` support exists for: a *non-embedded* face that publishes its
    /// advances must measure exactly instead of resolving to `Nominal`.
    #[test]
    fn a_non_embedded_font_is_measured_from_its_widths() {
        let document = testpdf::simple_font_widths_page();
        let page_id = textlocate::page_id(&document, 0).unwrap();
        let run = &textlocate::locate_runs(&document, page_id).unwrap()[0];
        let metrics = FontMetrics::resolve(&document, page_id, &run.font_key);

        assert!(
            matches!(metrics, FontMetrics::SimpleWidths { .. }),
            "a non-embedded face with /Widths must resolve to SimpleWidths, not Nominal"
        );
        // The fixture publishes 500 for every code in 32..=126.
        let width = metrics
            .advance("Test", 10.0)
            .expect("published widths are usable");
        assert!(
            (width - 4.0 * 500.0 * 10.0 / 1000.0).abs() < 0.001,
            "{width}"
        );
        assert!(
            metrics.advance("\u{2192}", 10.0).is_none(),
            "a code outside the published range has no advance"
        );
    }

    /// A font with no `/Widths` must stay unmeasurable, so the caller keeps refusing rather
    /// than substituting a guess (T22).
    #[test]
    fn a_font_without_widths_stays_nominal() {
        let document = testpdf::no_widths_page();
        let page_id = textlocate::page_id(&document, 0).unwrap();
        let run = &textlocate::locate_runs(&document, page_id).unwrap()[0];
        let metrics = FontMetrics::resolve(&document, page_id, &run.font_key);
        assert!(matches!(metrics, FontMetrics::Nominal));
        assert!(metrics.advance("x", 10.0).is_none());
    }

    /// MECH-B: a non-WinAnsi glyph the bundle covers is **embedded**, not refused.
    ///
    /// This test used to assert `GLYPHS_NOT_COVERABLE` for U+2192, which was true only while
    /// MECH-B did not exist. Arrows are in every bundled face, so the refusal would now be
    /// wrong; what the old test really protected was "no silent drop", and that is checked
    /// here instead: the character must be in the subset's own code map.
    #[test]
    fn a_non_winansi_glyph_the_bundle_covers_is_embedded() {
        let glyphs: Vec<char> = "HI".chars().collect();
        let document = testpdf::embedded_subset_page("HI", &glyphs);
        let page_id = textlocate::page_id(&document, 0).unwrap();
        let run = &textlocate::locate_runs(&document, page_id).unwrap()[0];
        let info = textlocate::font_info(&document, page_id, &run.font_key).unwrap();

        let plan = plan(&document, page_id, &run.font_key, &info, "H\u{2192}", true)
            .expect("U+2192 is covered by the bundled faces");
        let Plan::Embed { subset, .. } = plan else {
            panic!("a WinAnsi code exists for neither character, so MECH-A cannot fire");
        };
        assert!(
            subset
                .codes
                .iter()
                .any(|(_, character)| *character == '\u{2192}'),
            "the arrow must have a code of its own, not be dropped or folded"
        );
    }

    /// MECH-B refuses a glyph no bundled face has — the permanent gap, with its own code.
    ///
    /// U+1F600 is in no bundled face, and U+FE0F is a variation selector, which a simple font
    /// cannot address at all.
    #[test]
    fn a_glyph_no_bundled_face_has_is_not_coverable() {
        let glyphs: Vec<char> = "HI".chars().collect();
        let document = testpdf::embedded_subset_page("HI", &glyphs);
        let page_id = textlocate::page_id(&document, 0).unwrap();
        let run = &textlocate::locate_runs(&document, page_id).unwrap()[0];
        let info = textlocate::font_info(&document, page_id, &run.font_key).unwrap();

        let error = plan(&document, page_id, &run.font_key, &info, "H\u{1F600}", true)
            .expect_err("no bundled face has an emoji");
        assert_eq!(error.code(), "GLYPHS_NOT_COVERABLE", "{error}");
    }
}
