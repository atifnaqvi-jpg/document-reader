//! MECH-B: subsetting a bundled face and writing it as a simple `/TrueType` font (P2.7.6).
//!
//! MECH-A sets text no standard-14 face can express by *switching to* one. That covers
//! WinAnsi and stops there. MECH-B covers the rest by cutting the glyphs the replacement
//! needs out of a bundled Liberation face and writing that subset into the document as a new
//! resource.
//!
//! **How a code becomes a glyph.** The content stream carries one-byte codes chosen by
//! [`assign_codes`], not character codes. A viewer resolves such a code in two steps: the
//! font's `/Encoding /Differences` names the character for the code, then the program's
//! `cmap` maps that character to a glyph. So this module does two matching things:
//!
//! * the subset keeps a `(3,1)` Unicode `cmap` from each character to its subset glyph id,
//!   which is what [`crate::editor::textlocate`] writes as `/Differences` names and what lets
//!   `ttf-parser` confirm the subset really maps the characters it claims;
//! * the PDF gets a `/ToUnicode` CMap ([`to_unicode_cmap`]) so copy-paste and search read the
//!   right characters. It governs the text layer; the `cmap` governs layout.
//!
//! The alternative — a symbolic `(3,0)` `cmap` and the `0xF000 + code` convention — was
//! rejected because it makes rendering depend on a viewer convention rather than on the
//! format, and because a symbol table cannot answer "does this subset map U+0100".
//!
//! **The written name.** OFL condition 3 forbids a Modified Version — and deleting glyphs
//! *is* modifying — from presenting `Liberation` as its primary name, so the written name is
//! `/<PREFIX>+<Family>-<Style>` per `resources/fonts/FONT-RECORD.md`. The subset's `name`
//! table keeps IDs 0/13/14 (the notice has to travel with the font) and drops the family
//! names.
//!
//! The faces are compiled in with `include_bytes!` rather than read from `resources/` at
//! runtime: the write path is synchronous, runs with no app handle in tests and in the
//! library layer, and a missing bundle file becomes a build failure instead of a refusal a
//! user meets after shipping.

use std::collections::BTreeMap;

use lopdf::{dictionary, Dictionary, Object, Stream};

/// One bundled face, and the primary name a subset of it may be written under.
pub struct Face {
    /// The committed bytes, so the record and the code cannot disagree about what shipped.
    pub bytes: &'static [u8],
    /// `Family-Style`, e.g. `Sans-BoldItalic`. Never contains the Reserved Font Name.
    pub written_family: &'static str,
    /// The bundled filename, for messages.
    pub file: &'static str,
}

macro_rules! face {
    ($file:literal, $family:literal) => {
        Face {
            bytes: include_bytes!(concat!("../../resources/fonts/", $file)),
            written_family: $family,
            file: $file,
        }
    };
}

/// The style class that selects each face, and the face (R1's bijection).
///
/// The class is `substitute::style_match`'s output — the same string MECH-A would have used —
/// so the family decision is made once for both mechanisms and a document cannot be set in
/// Times by one path and Serif by the other.
static FACES: [(&str, Face); 12] = [
    (
        "Helvetica",
        face!("LiberationSans-Regular.ttf", "Sans-Regular"),
    ),
    (
        "Helvetica-Bold",
        face!("LiberationSans-Bold.ttf", "Sans-Bold"),
    ),
    (
        "Helvetica-Oblique",
        face!("LiberationSans-Italic.ttf", "Sans-Italic"),
    ),
    (
        "Helvetica-BoldOblique",
        face!("LiberationSans-BoldItalic.ttf", "Sans-BoldItalic"),
    ),
    (
        "Times-Roman",
        face!("LiberationSerif-Regular.ttf", "Serif-Regular"),
    ),
    (
        "Times-Bold",
        face!("LiberationSerif-Bold.ttf", "Serif-Bold"),
    ),
    (
        "Times-Italic",
        face!("LiberationSerif-Italic.ttf", "Serif-Italic"),
    ),
    (
        "Times-BoldItalic",
        face!("LiberationSerif-BoldItalic.ttf", "Serif-BoldItalic"),
    ),
    (
        "Courier",
        face!("LiberationMono-Regular.ttf", "Mono-Regular"),
    ),
    (
        "Courier-Bold",
        face!("LiberationMono-Bold.ttf", "Mono-Bold"),
    ),
    (
        "Courier-Oblique",
        face!("LiberationMono-Italic.ttf", "Mono-Italic"),
    ),
    (
        "Courier-BoldOblique",
        face!("LiberationMono-BoldItalic.ttf", "Mono-BoldItalic"),
    ),
];

/// The bundled face for a standard-14 style class, or `None` for a name MECH-A would not have
/// produced either.
pub fn face_for_style(style: &str) -> Option<&'static Face> {
    FACES
        .iter()
        .find(|(class, _)| *class == style)
        .map(|(_, face)| face)
}

/// Every shipped face, for guards that must cover the set rather than a sample.
pub fn faces() -> impl Iterator<Item = (&'static str, &'static Face)> {
    FACES.iter().map(|(class, face)| (*class, face))
}

/// True when a subset of `face` can set `text`.
///
/// This is the coverage *test*, never an assumption (design §1.2). `glyph_index` returning
/// `None` is absence and reads as "not covered"; `Some(GlyphId(0))` is `.notdef`, also not
/// coverage. Both were got wrong in opposite directions earlier in this phase, so both are
/// spelled out.
pub fn covers(face: &Face, text: &str) -> bool {
    let Ok(parsed) = ttf_parser::Face::parse(face.bytes, 0) else {
        return false;
    };
    text.chars().all(|ch| {
        parsed
            .glyph_index(ch)
            .is_some_and(|id| id != ttf_parser::GlyphId(0))
    })
}

/// A face cut down to the glyphs one replacement needs.
#[derive(Debug, Clone, PartialEq)]
pub struct Subset {
    /// The subset program, uncompressed; the caller Flate-compresses the stream.
    pub program: Vec<u8>,
    /// `code → character`, in first-use order. The content stream carries the codes; the
    /// `/Encoding /Differences` and the `/ToUnicode` CMap both state this mapping.
    pub codes: Vec<(u8, char)>,
    /// `/Widths` entries in 1000-unit em, indexed from `first_char`.
    pub widths: Vec<u16>,
    pub first_char: u8,
    /// Advances from the subset's own `hmtx`, for the width gate (T13).
    pub units_per_em: u16,
    /// The written `/BaseFont`, e.g. `KQ3F7A+Sans-Regular`.
    pub base_font: String,
    /// The subset's own bounding box, in font units — what the descriptor must state.
    pub bbox: [i16; 4],
    pub ascent: i16,
    pub descent: i16,
    pub cap_height: i16,
}

impl Subset {
    /// The advance of `text` at `font_size` points, from the **subset's** metrics.
    ///
    /// T13 compares the *final* font's metrics, and the final font here is the subset.
    pub fn advance(&self, text: &str, font_size: f32) -> Option<f32> {
        if self.units_per_em == 0 {
            return None;
        }
        let mut thousandths: f64 = 0.0;
        for ch in text.chars() {
            let index = self.codes.iter().position(|(_, c)| *c == ch)?;
            let width = *self.widths.get(index)?;
            thousandths += f64::from(width);
        }
        Some((thousandths * f64::from(font_size) / 1000.0) as f32)
    }

    /// The bytes the content stream carries for `text`, or `None` if a character was never
    /// assigned a code (which [`subset`] already refused, so this is a programming error).
    pub fn encode(&self, text: &str) -> Option<Vec<u8>> {
        text.chars()
            .map(|ch| {
                self.codes
                    .iter()
                    .find(|(_, c)| *c == ch)
                    .map(|(code, _)| *code)
            })
            .collect()
    }
}

/// The codes a subset uses for `text`: first-use order, starting at 1.
///
/// Code 0 is left unused. A leading `\0` in a PDF string is legal but some extractors treat it
/// as padding, and a replacement short enough to fit a simple font has no need to save a code.
fn assign_codes(text: &str) -> Result<Vec<(u8, char)>, String> {
    let mut codes: Vec<(u8, char)> = Vec::new();
    for ch in text.chars() {
        if codes.iter().any(|(_, c)| *c == ch) {
            continue;
        }
        let code = u8::try_from(codes.len() + 1)
            .map_err(|_| "the replacement has more than 255 distinct characters".to_string())?;
        codes.push((code, ch));
    }
    Ok(codes)
}

/// Cuts `face` down to the glyphs in `text`, or says why it could not.
///
/// Every `Err` here is a fact about *our* builder, not about the shipped faces: coverage was
/// already checked by [`covers`]. The caller maps that to `SUBSET_BUILD_FAILED`, which names a
/// different remedy from `GLYPHS_NOT_COVERABLE` (§2.4).
pub fn subset(face: &Face, text: &str) -> Result<Subset, String> {
    let codes = assign_codes(text)?;
    let parsed = ttf_parser::Face::parse(face.bytes, 0)
        .map_err(|e| format!("bundled face {} did not parse: {e}", face.file))?;
    let units_per_em = parsed.units_per_em();
    if units_per_em == 0 {
        return Err(format!(
            "bundled face {} declares no units-per-em",
            face.file
        ));
    }
    let source = SourceTables::parse(face.bytes)?;

    // Source glyph ids, `.notdef` first: an unmapped code must still have a glyph to point at.
    let mut glyphs: Vec<u16> = vec![0];
    let mut wanted: Vec<(u8, u16)> = Vec::new();
    for (code, ch) in &codes {
        let id = parsed
            .glyph_index(*ch)
            .ok_or_else(|| format!("{} has no glyph for U+{:04X}", face.file, *ch as u32))?;
        let id = id.0;
        if !glyphs.contains(&id) {
            glyphs.push(id);
        }
        wanted.push((*code, id));
    }

    // Closure over composites: a component that is not in the subset would draw nothing.
    let mut queue = glyphs.clone();
    while let Some(id) = queue.pop() {
        for component in source.components(source.glyph_data(id).unwrap_or(&[])) {
            if !glyphs.contains(&component) {
                glyphs.push(component);
                queue.push(component);
            }
        }
    }
    glyphs.sort_unstable();

    // Subset ids: `.notdef` stays 0 and everything else is renumbered in sorted source order,
    // so the same input twice yields the same program.
    let new_id = |old: u16| -> Result<u16, String> {
        glyphs
            .iter()
            .position(|g| *g == old)
            .map(|index| index as u16)
            .ok_or_else(|| format!("composite component {old} is not in the subset"))
    };

    let mut glyf = Vec::new();
    let mut loca: Vec<u32> = vec![0];
    let mut bbox = BBox::default();
    for old in &glyphs {
        let data = source.glyph_data(*old).unwrap_or(&[]);
        let data = if *old == 0 {
            Vec::new()
        } else {
            source.rewrite_composite(data, &new_id)?
        };
        bbox.include(&data);
        glyf.extend_from_slice(&data);
        while glyf.len() % 4 != 0 {
            glyf.push(0);
        }
        loca.push(glyf.len() as u32);
    }

    let mut hmtx = Vec::new();
    for old in &glyphs {
        let advance = parsed
            .glyph_hor_advance(ttf_parser::GlyphId(*old))
            .unwrap_or(0);
        let bearing = parsed
            .glyph_hor_side_bearing(ttf_parser::GlyphId(*old))
            .unwrap_or(0);
        hmtx.extend_from_slice(&advance.to_be_bytes());
        hmtx.extend_from_slice(&bearing.to_be_bytes());
    }

    // `/Widths` is indexed by **code**, so it is built from `wanted`, not from the glyph list.
    let mut widths = Vec::new();
    for (_, old) in &wanted {
        let advance = parsed
            .glyph_hor_advance(ttf_parser::GlyphId(*old))
            .unwrap_or(0);
        widths.push(scale_to_1000(advance, units_per_em));
    }

    let mut chars_to_glyph: BTreeMap<char, u16> = BTreeMap::new();
    for (code, ch) in &codes {
        let old = wanted
            .iter()
            .find(|(c, _)| c == code)
            .map(|(_, glyph)| *glyph)
            .ok_or("internal: a coded character has no glyph")?;
        chars_to_glyph.insert(*ch, new_id(old)?);
    }

    let first_char = codes.first().map(|(code, _)| *code).unwrap_or(1);
    let prefix = subset_prefix(face, &codes, &glyphs);
    let base_font = format!("{prefix}+{}", face.written_family);

    let max_advance = glyphs
        .iter()
        .filter_map(|id| parsed.glyph_hor_advance(ttf_parser::GlyphId(*id)))
        .max()
        .unwrap_or(0);
    let program = assemble(Assemble {
        source: &source,
        parsed: &parsed,
        num_glyphs: u16::try_from(glyphs.len())
            .map_err(|_| "the subset has more than 65535 glyphs".to_string())?,
        max_advance,
        glyf: &glyf,
        loca: &loca,
        hmtx: &hmtx,
        bbox,
        chars_to_glyph: &chars_to_glyph,
    })?;

    Ok(Subset {
        program,
        codes,
        widths,
        first_char,
        units_per_em,
        base_font,
        bbox: [bbox.x_min, bbox.y_min, bbox.x_max, bbox.y_max],
        ascent: parsed.ascender(),
        descent: parsed.descender(),
        cap_height: parsed.capital_height().unwrap_or(parsed.ascender()),
    })
}

/// A face cut down to the glyphs one replacement needs, written as a **CID** program.
///
/// The simple-font [`Subset`] cannot serve a `/Type0` run: its `codes` are one byte, its
/// `/Widths` is indexed by code, and `embed::subset` numbers glyphs from 1 in first-use order.
/// A Type0 font needs two-byte CIDs, a `/W` array, and — under `/CIDToGIDMap /Identity` — a
/// program whose glyph ids *are* the document's CIDs.
///
/// That last requirement is R5, and it is why this rebuild cannot reuse `subset`'s numbering.
/// [`subset_at_cids`] takes the assignment as an argument instead of inventing one.
#[derive(Debug, Clone, PartialEq)]
pub struct CidSubset {
    /// The subset program, uncompressed. Glyph slots are the document's CID space: a CID the
    /// rebuild keeps has its outline at that id, and every other slot is empty.
    pub program: Vec<u8>,
    /// `CID → character`, in ascending CID order: the correspondence the written program and
    /// its `/ToUnicode` both state.
    pub assignments: Vec<(u16, char)>,
    /// The written `/BaseFont`, e.g. `KQ3F7A+Sans-Regular`.
    pub base_font: String,
    pub units_per_em: u16,
    /// The subset's own bounding box, in font units.
    pub bbox: [i16; 4],
    pub ascent: i16,
    pub descent: i16,
    pub cap_height: i16,
}

impl CidSubset {
    /// The two-byte CID for `ch`, or `None` when the subset does not carry it.
    pub fn encode(&self, text: &str) -> Option<Vec<u8>> {
        text.chars()
            .map(|ch| {
                self.assignments
                    .iter()
                    .find(|(_, c)| *c == ch)
                    .map(|(cid, _)| *cid)
            })
            .collect::<Option<Vec<u16>>>()
            .map(|cids| cids.iter().flat_map(|cid| cid.to_be_bytes()).collect())
    }

    /// The advance of `text` at `font_size` points, from the **written** program's `hmtx`.
    ///
    /// Read from the program rather than from the bundled face on purpose: T13 compares the
    /// final font's metrics, and `/W` must state the same numbers, so both derive from the
    /// bytes in the file.
    pub fn advance(&self, text: &str, font_size: f32) -> Option<f32> {
        if self.units_per_em == 0 {
            return None;
        }
        let face = ttf_parser::Face::parse(&self.program, 0).ok()?;
        let mut thousandths: f64 = 0.0;
        for ch in text.chars() {
            let (cid, _) = *self.assignments.iter().find(|(_, c)| *c == ch)?;
            let advance = face.glyph_hor_advance(ttf_parser::GlyphId(cid))?;
            thousandths += f64::from(scale_to_1000(advance, face.units_per_em()));
        }
        Some((thousandths * f64::from(font_size) / 1000.0) as f32)
    }

    /// The `/W` entries R1 requires: one `[cid [w1 w2 …]]` pair per contiguous CID run.
    ///
    /// Never the `[cfirst clast w]` range form, and never CID 0. The range form would state one
    /// width for a span the subset measures individually, so it can only be right by luck.
    pub fn width_entries(&self) -> Vec<(u16, Vec<u16>)> {
        let face = match ttf_parser::Face::parse(&self.program, 0) {
            Ok(face) => face,
            Err(_) => return Vec::new(),
        };
        let upem = face.units_per_em();
        let mut out: Vec<(u16, Vec<u16>)> = Vec::new();
        for (cid, _) in &self.assignments {
            if *cid == 0 {
                continue;
            }
            let width = face
                .glyph_hor_advance(ttf_parser::GlyphId(*cid))
                .map(|advance| scale_to_1000(advance, upem))
                .unwrap_or(0);
            match out.last_mut() {
                Some((first, widths)) if *first + widths.len() as u16 == *cid => {
                    widths.push(width);
                }
                _ => out.push((*cid, vec![width])),
            }
        }
        out
    }

    /// `/DW` (R1): the advance of the first assigned CID, in thousandths of an em.
    ///
    /// Written **always**. A CID the `/W` array does not name must still measure; naming
    /// neither would make the file unmeasurable by the same reader that wrote it, which is
    /// the state `cid.rs::CidFont::advance` refuses to guess from.
    pub fn default_width(&self) -> u16 {
        let Some(face) = ttf_parser::Face::parse(&self.program, 0).ok() else {
            return 0;
        };
        let upem = face.units_per_em();
        self.assignments
            .iter()
            .find(|(cid, _)| *cid != 0)
            .and_then(|(cid, _)| face.glyph_hor_advance(ttf_parser::GlyphId(*cid)))
            .map(|advance| scale_to_1000(advance, upem))
            .unwrap_or(0)
    }
}

/// Rebuilds `face` as a CID-keyed program with the glyphs placed **at the given CIDs** (R5).
///
/// `placements` is `(cid, character)`; `face` supplies the outlines. The id comes from the
/// document, the outline from the bundle, and the two are unrelated numbers — this function's
/// whole job is to put the bundled outline down at the document's id instead of assigning a
/// new one. A rebuild that numbered the glyphs from 1 would produce a perfectly valid font in
/// which every retained glyph has moved, which is the silent corruption R2's `/Identity`
/// depends on not happening.
///
/// `source_gid` maps a character to the glyph id it must take in the *source* program. That is
/// what makes `/Identity` true rather than asserted: after this call, the written program's
/// `cmap` resolves every retained character to the same id the source did.
pub fn subset_at_cids(
    face: &Face,
    placements: &[(u16, char)],
    retained: &dyn Fn(char) -> Option<u16>,
) -> Result<CidSubset, String> {
    if placements.is_empty() {
        return Err("a CID subset needs at least one glyph".into());
    }
    let parsed = ttf_parser::Face::parse(face.bytes, 0)
        .map_err(|e| format!("bundled face {} did not parse: {e}", face.file))?;
    let units_per_em = parsed.units_per_em();
    if units_per_em == 0 {
        return Err(format!(
            "bundled face {} declares no units-per-em",
            face.file
        ));
    }
    let source = SourceTables::parse(face.bytes)?;

    // `slot -> outline`, where the slot *is* the document's CID. Missing slots stay empty,
    // which is R5's "leaving an empty slot where the source had a glyph this subset drops".
    let highest = placements.iter().map(|(cid, _)| *cid).max().unwrap_or(0);
    if highest == u16::MAX {
        return Err("the document's CIDs leave no room for a new glyph at 65535".into());
    }
    let mut slot_count = usize::from(highest) + 1;
    let mut outlines: Vec<Option<Vec<u8>>> = vec![None; slot_count];
    let mut assignments: Vec<(u16, char)> = placements.to_vec();
    assignments.sort_unstable_by_key(|(cid, _)| *cid);

    // The outline always comes from the **bundle**. R5 is exact about which number comes from
    // where: "the retained glyph's *outlines* come from the bundled face; its *id* comes from
    // the document." So glyph data is addressed by the bundle's own glyph id for the character,
    // and only the slot it is copied into is the document's CID. Confusing the two indexes
    // yields a program of empty slots — the bundle's glyph 1 is not `A` — which parses, draws
    // nothing, and measures zero.
    let mut slot_source: Vec<Option<u16>> = vec![None; slot_count];
    for (cid, ch) in &assignments {
        if let Some(source_gid) = retained(*ch) {
            if source_gid != *cid {
                return Err(format!(
                    "{ch:?} draws glyph {source_gid} in the source program but the document \
                     draws it at CID {cid}; /Identity would be a false claim and a stream \
                     /CIDToGIDMap is not what this writer produces"
                ));
            }
        }
        let gid = parsed
            .glyph_index(*ch)
            .map(|glyph| glyph.0)
            .ok_or_else(|| format!("the bundle has no glyph for {ch:?}"))?;
        if outlines[usize::from(*cid)].is_some() {
            return Err(format!("CID {cid} is assigned twice"));
        }
        outlines[usize::from(*cid)] = Some(source.glyph_data(gid).unwrap_or(&[]).to_vec());
        slot_source[usize::from(*cid)] = Some(gid);
    }

    // A composite names its components by *bundle* id, and those ids cannot be kept: under
    // `/Identity` a glyph id is a document CID, and the document never used them. Each component
    // gets a fresh slot above every CID in use, and the composite's records are rewritten to
    // name those slots.
    let mut component_slot: Vec<(u16, u16)> = Vec::new();
    for (cid, _) in &assignments {
        let Some(Some(data)) = outlines.get(usize::from(*cid)).cloned() else {
            continue;
        };
        for component in source.components(&data) {
            if component_slot
                .iter()
                .any(|(bundle, _)| *bundle == component)
            {
                continue;
            }
            // A component the document already draws is *that* CID. `Å` is `A` plus a ring, and
            // `A` is usually a live CID, so handing it a fresh slot would ship a second copy of
            // an outline at an id no run and no `/W` entry names — `/Identity` gives one glyph
            // id one meaning, and the duplicate would drift from CID 1 the moment either is
            // rewritten. Reuse the CID.
            if let Some((live, _)) = assignments
                .iter()
                .find(|(_, ch)| parsed.glyph_index(*ch).map(|g| g.0) == Some(component))
            {
                component_slot.push((component, *live));
                continue;
            }
            let slot = u16::try_from(slot_count).map_err(|_| {
                "the CID subset needs more slots than a Type0 can address".to_string()
            })?;
            slot_count += 1;
            outlines.push(Some(source.glyph_data(component).unwrap_or(&[]).to_vec()));
            slot_source.push(Some(component));
            component_slot.push((component, slot));
        }
    }

    // `loca`/`glyf` for every slot, empty ones included.
    let remap = |id: u16| -> Result<u16, String> {
        component_slot
            .iter()
            .find(|(bundle, _)| *bundle == id)
            .map(|(_, slot)| *slot)
            .ok_or_else(|| format!("composite names component {id}, which was never collected"))
    };
    let mut glyf = Vec::new();
    let mut loca: Vec<u32> = vec![0];
    let mut bbox = BBox::default();
    for outline in &outlines {
        // An empty slot is a real zero-length glyph, not a gap a reader must skip: `loca`
        // names it, and `/Identity` may point a CID at it, which draws nothing and parses.
        let data = match outline {
            None => Vec::new(),
            Some(data) => source.rewrite_composite(data, &remap)?,
        };
        bbox.include(&data);
        glyf.extend_from_slice(&data);
        while glyf.len() % 4 != 0 {
            glyf.push(0);
        }
        loca.push(glyf.len() as u32);
    }

    // `hmtx` describes the outlines actually written, slot by slot, so `/W`, `hhea` and the
    // width gate all state the same numbers the page will be drawn with.
    let mut hmtx = Vec::new();
    let mut max_advance = 0u16;
    for source_id in &slot_source {
        let (advance, bearing) = match source_id {
            Some(id) => (
                parsed
                    .glyph_hor_advance(ttf_parser::GlyphId(*id))
                    .unwrap_or(0),
                parsed
                    .glyph_hor_side_bearing(ttf_parser::GlyphId(*id))
                    .unwrap_or(0),
            ),
            None => (0, 0),
        };
        max_advance = max_advance.max(advance);
        hmtx.extend_from_slice(&advance.to_be_bytes());
        hmtx.extend_from_slice(&bearing.to_be_bytes());
    }

    let chars_to_glyph: BTreeMap<char, u16> =
        assignments.iter().map(|(cid, ch)| (*ch, *cid)).collect();
    let prefix = cid_subset_prefix(face, &assignments, highest);
    let base_font = format!("{prefix}+{}", face.written_family);
    let program = assemble(Assemble {
        source: &source,
        parsed: &parsed,
        num_glyphs: u16::try_from(slot_count)
            .map_err(|_| "the CID subset has more than 65535 glyph slots".to_string())?,
        max_advance,
        glyf: &glyf,
        loca: &loca,
        hmtx: &hmtx,
        bbox,
        chars_to_glyph: &chars_to_glyph,
    })?;

    // The program must answer for what was claimed, and this is the one place that can check
    // it before the bytes leave the builder. `ttf-parser` parses what was just assembled, so a
    // disagreement between the tables and the `cmap` is caught here rather than by a renderer.
    let written = ttf_parser::Face::parse(&program, 0)
        .map_err(|e| format!("the CID subset did not parse back: {e}"))?;
    for (cid, ch) in &assignments {
        let actual = written.glyph_index(*ch).map(|glyph| glyph.0);
        if actual != Some(*cid) {
            return Err(format!(
                "the written program resolves {ch:?} to {actual:?} instead of CID {cid}"
            ));
        }
    }

    Ok(CidSubset {
        program,
        assignments,
        base_font,
        units_per_em,
        bbox: [bbox.x_min, bbox.y_min, bbox.x_max, bbox.y_max],
        ascent: parsed.ascender(),
        descent: parsed.descender(),
        cap_height: parsed.capital_height().unwrap_or(parsed.ascender()),
    })
}

/// Deterministic six-letter prefix for a CID subset.
///
/// Separate from [`subset_prefix`] because the inputs differ: CIDs are two bytes and the
/// highest slot matters, not the glyph count. The prefix must still be six uppercase letters —
/// `install_embedded_resource` derives the resource name from it, and the OFL rule that
/// `/BaseFont` never carries the Reserved Font Name keys off the same shape.
fn cid_subset_prefix(face: &Face, assignments: &[(u16, char)], highest: u16) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in face
        .written_family
        .bytes()
        .chain(assignments.iter().flat_map(|(cid, _)| cid.to_be_bytes()))
        .chain(highest.to_be_bytes())
    {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ";
    (0..6)
        .map(|index| alphabet[((hash >> (index * 5)) % 26) as usize] as char)
        .collect()
}

/// Deterministic six-letter uppercase prefix from the subset's own content.
///
/// Two edits in one file must not collide (a PDF names them apart), and the same input twice
/// must not produce two names for the same bytes. FNV-1a over the face name and the written
/// characters is enough for that: this is a name, not a security boundary.
fn subset_prefix(face: &Face, codes: &[(u8, char)], glyphs: &[u16]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in face
        .written_family
        .bytes()
        .chain(codes.iter().flat_map(|(_, ch)| (*ch as u32).to_le_bytes()))
        .chain(glyphs.iter().flat_map(|id| id.to_le_bytes()))
    {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ";
    (0..6)
        .map(|index| alphabet[((hash >> (index * 5)) % 26) as usize] as char)
        .collect()
}

fn scale_to_1000(value: u16, units_per_em: u16) -> u16 {
    ((u32::from(value) * 1000) / u32::from(units_per_em)) as u16
}

/// The source tables the subsetter copies from, addressed by tag.
struct SourceTables<'a> {
    data: &'a [u8],
    tables: BTreeMap<[u8; 4], (usize, usize)>,
    long_loca: bool,
}

impl<'a> SourceTables<'a> {
    fn parse(data: &'a [u8]) -> Result<Self, String> {
        if data.len() < 12 {
            return Err("truncated sfnt header".into());
        }
        let count = usize::from(be_u16(data, 4)?);
        let mut tables = BTreeMap::new();
        for index in 0..count {
            let base = 12 + index * 16;
            let tag: [u8; 4] = data
                .get(base..base + 4)
                .ok_or("truncated table directory")?
                .try_into()
                .unwrap();
            let offset = be_u32(data, base + 8)? as usize;
            let length = be_u32(data, base + 12)? as usize;
            if offset
                .checked_add(length)
                .is_none_or(|end| end > data.len())
            {
                return Err(format!(
                    "table {} extends past the file",
                    String::from_utf8_lossy(&tag)
                ));
            }
            tables.insert(tag, (offset, length));
        }
        let head = *tables.get(b"head").ok_or("no head table")?;
        let long_loca = be_i16(data, head.0 + 50)? != 0;
        Ok(Self {
            data,
            tables,
            long_loca,
        })
    }

    fn table(&self, tag: &[u8; 4]) -> Option<&'a [u8]> {
        let (offset, length) = *self.tables.get(tag)?;
        self.data.get(offset..offset + length)
    }

    /// The `glyf` bytes for one *source* glyph id.
    fn glyph_data(&self, id: u16) -> Option<&'a [u8]> {
        let loca = self.table(b"loca")?;
        let glyf = self.table(b"glyf")?;
        let at = usize::from(id);
        let (start, end) = if self.long_loca {
            (
                be_u32(loca, at * 4).ok()? as usize,
                be_u32(loca, at * 4 + 4).ok()? as usize,
            )
        } else {
            (
                usize::from(be_u16(loca, at * 2).ok()?) * 2,
                usize::from(be_u16(loca, at * 2 + 2).ok()?) * 2,
            )
        };
        if end <= start {
            return Some(&[]);
        }
        glyf.get(start..end)
    }

    /// The component glyph ids a composite glyph names, in order.
    fn components(&self, data: &[u8]) -> Vec<u16> {
        let mut out = Vec::new();
        for_each_component(data, |_, glyph| out.push(glyph));
        out
    }

    /// Rewrites a composite glyph's component ids from source ids to subset ids.
    fn rewrite_composite(
        &self,
        data: &[u8],
        new_id: &dyn Fn(u16) -> Result<u16, String>,
    ) -> Result<Vec<u8>, String> {
        if data.len() < 10 || i16::from_be_bytes([data[0], data[1]]) >= 0 {
            return Ok(data.to_vec());
        }
        let mut out = data[..10].to_vec();
        let mut rest = &data[10..];
        loop {
            if rest.len() < 4 {
                return Err("truncated composite glyph".into());
            }
            let flags = u16::from_be_bytes([rest[0], rest[1]]);
            let old = u16::from_be_bytes([rest[2], rest[3]]);
            let mut record = rest[..4].to_vec();
            record[2..4].copy_from_slice(&new_id(old)?.to_be_bytes());
            let advance = component_size(flags);
            if rest.len() < advance {
                return Err("truncated composite glyph record".into());
            }
            out.extend_from_slice(&record);
            out.extend_from_slice(&rest[4..advance]);
            rest = &rest[advance..];
            if flags & 0x0020 == 0 {
                break;
            }
        }
        // Trailing instruction bytes belong to the glyph and are copied verbatim.
        out.extend_from_slice(rest);
        Ok(out)
    }
}

fn component_size(flags: u16) -> usize {
    4 + if flags & 0x0001 != 0 { 4 } else { 2 }
        + if flags & 0x0008 != 0 {
            2
        } else if flags & 0x0040 != 0 {
            4
        } else if flags & 0x0080 != 0 {
            8
        } else {
            0
        }
}

fn for_each_component(data: &[u8], mut visit: impl FnMut(u16, u16)) {
    if data.len() < 10 || i16::from_be_bytes([data[0], data[1]]) >= 0 {
        return;
    }
    let mut rest = &data[10..];
    loop {
        if rest.len() < 4 {
            return;
        }
        let flags = u16::from_be_bytes([rest[0], rest[1]]);
        let glyph = u16::from_be_bytes([rest[2], rest[3]]);
        visit(flags, glyph);
        let advance = component_size(flags);
        if rest.len() < advance {
            return;
        }
        rest = &rest[advance..];
        if flags & 0x0020 == 0 {
            return;
        }
    }
}

#[derive(Clone, Copy, Default)]
struct BBox {
    x_min: i16,
    y_min: i16,
    x_max: i16,
    y_max: i16,
    seen: bool,
}

impl BBox {
    fn include(&mut self, glyph: &[u8]) {
        if glyph.len() < 10 {
            return;
        }
        let read = |at: usize| i16::from_be_bytes([glyph[at], glyph[at + 1]]);
        let (x_min, y_min, x_max, y_max) = (read(2), read(4), read(6), read(8));
        if !self.seen {
            *self = Self {
                x_min,
                y_min,
                x_max,
                y_max,
                seen: true,
            };
            return;
        }
        self.x_min = self.x_min.min(x_min);
        self.y_min = self.y_min.min(y_min);
        self.x_max = self.x_max.max(x_max);
        self.y_max = self.y_max.max(y_max);
    }
}

struct Assemble<'a> {
    source: &'a SourceTables<'a>,
    parsed: &'a ttf_parser::Face<'a>,
    /// Glyph slots the written program has, which for the CID rebuild is the highest placed
    /// id plus one and *not* the number of glyphs kept: R5 leaves gaps.
    num_glyphs: u16,
    /// The largest advance in the subset, for `hhea`.
    max_advance: u16,
    glyf: &'a [u8],
    loca: &'a [u32],
    hmtx: &'a [u8],
    bbox: BBox,
    chars_to_glyph: &'a BTreeMap<char, u16>,
}

/// Writes the subset sfnt from the tables above.
///
/// Tables are emitted in tag order with 4-byte padding, as the format expects. Hinting
/// (`fpgm`/`prep`/`cvt `), layout (`GSUB`/`GPOS`/`GDEF`), `kern`, `DSIG` and vertical metrics
/// are deliberately not carried over (design §2.3): unused at screen sizes, and where subset
/// bugs concentrate.
fn assemble(input: Assemble<'_>) -> Result<Vec<u8>, String> {
    let Assemble {
        source,
        parsed,
        num_glyphs,
        max_advance,
        glyf,
        loca,
        hmtx,
        bbox,
        chars_to_glyph,
    } = input;

    let long_loca = glyf.len() > usize::from(u16::MAX);
    let loca_bytes: Vec<u8> = if long_loca {
        loca.iter().flat_map(|value| value.to_be_bytes()).collect()
    } else {
        loca.iter()
            .flat_map(|value| ((*value / 2) as u16).to_be_bytes())
            .collect()
    };

    let bbox = if bbox.seen {
        bbox
    } else {
        let rect = parsed.global_bounding_box();
        BBox {
            x_min: rect.x_min,
            y_min: rect.y_min,
            x_max: rect.x_max,
            y_max: rect.y_max,
            seen: true,
        }
    };
    let head = head_table(source, &bbox, if long_loca { 1 } else { 0 })?;
    let hhea = hhea_table(source, max_advance, num_glyphs)?;
    let maxp = maxp_table(num_glyphs, source);
    let cmap = unicode_cmap(chars_to_glyph);
    let name = name_table(parsed)?;
    let post = post_v3();
    let os2 = source
        .table(b"OS/2")
        .map(<[u8]>::to_vec)
        .unwrap_or_default();

    let mut tables: Vec<([u8; 4], Vec<u8>)> = vec![
        (*b"OS/2", os2),
        (*b"cmap", cmap),
        (*b"glyf", glyf.to_vec()),
        (*b"head", head),
        (*b"hhea", hhea),
        (*b"hmtx", hmtx.to_vec()),
        (*b"loca", loca_bytes),
        (*b"maxp", maxp),
        (*b"name", name),
        (*b"post", post),
    ];
    tables.retain(|(_, data)| !data.is_empty());
    tables.sort_by(|a, b| a.0.cmp(&b.0));

    let count = tables.len() as u16;
    // `searchRange` is 16 x the largest power of two that is <= numTables, not the next power
    // of two: for 10 tables that is 8 (128), not 16 (256).
    let power = largest_power_of_two(count);
    let search_range = 16 * power;
    let entry_selector = u16::from(power.trailing_zeros() as u8);

    let mut out = Vec::new();
    out.extend_from_slice(&0x0001_0000u32.to_be_bytes());
    out.extend_from_slice(&count.to_be_bytes());
    out.extend_from_slice(&search_range.to_be_bytes());
    out.extend_from_slice(&entry_selector.to_be_bytes());
    out.extend_from_slice(&(count * 16 - search_range).to_be_bytes());

    let mut offset = 12 + 16 * tables.len();
    let mut body = Vec::new();
    for (tag, data) in &tables {
        out.extend_from_slice(tag);
        out.extend_from_slice(&checksum(data).to_be_bytes());
        out.extend_from_slice(&(offset as u32).to_be_bytes());
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        body.extend_from_slice(data);
        while body.len() % 4 != 0 {
            body.push(0);
        }
        offset += data.len() + (4 - data.len() % 4) % 4;
    }
    out.extend_from_slice(&body);
    Ok(out)
}

fn head_table(
    source: &SourceTables<'_>,
    bbox: &BBox,
    index_to_loc: i16,
) -> Result<Vec<u8>, String> {
    let mut head = source
        .table(b"head")
        .ok_or("no head table")?
        .get(..54)
        .ok_or("truncated head")?
        .to_vec();
    // `checkSumAdjustment` is recomputed by any consumer that cares, and 0 is the format's
    // "not computed" value; a stale copied value would be a claim about the wrong bytes.
    head[8..12].copy_from_slice(&0u32.to_be_bytes());
    head[36..38].copy_from_slice(&bbox.x_min.to_be_bytes());
    head[38..40].copy_from_slice(&bbox.y_min.to_be_bytes());
    head[40..42].copy_from_slice(&bbox.x_max.to_be_bytes());
    head[42..44].copy_from_slice(&bbox.y_max.to_be_bytes());
    head[50..52].copy_from_slice(&index_to_loc.to_be_bytes());
    Ok(head)
}

fn hhea_table(
    source: &SourceTables<'_>,
    max_advance: u16,
    metrics: u16,
) -> Result<Vec<u8>, String> {
    let mut hhea = source
        .table(b"hhea")
        .ok_or("no hhea table")?
        .get(..36)
        .ok_or("truncated hhea")?
        .to_vec();
    hhea[10..12].copy_from_slice(&max_advance.to_be_bytes());
    // `numberOfHMetrics` must describe the `hmtx` actually written, which has one long metric
    // per subset glyph. Copying the source's count would tell a reader to look past the table.
    hhea[34..36].copy_from_slice(&metrics.to_be_bytes());
    Ok(hhea)
}

/// The largest power of two that is less than or equal to `value` (1 for 0).
fn largest_power_of_two(value: u16) -> u16 {
    if value == 0 {
        return 1;
    }
    1 << (15 - value.leading_zeros() as u16)
}

fn maxp_table(num_glyphs: u16, source: &SourceTables<'_>) -> Vec<u8> {
    // Version 1.0 with the source's maxima: no glyph is added, and the source's maxima bound
    // this face.
    let mut maxp = vec![0u8; 32];
    maxp[0..4].copy_from_slice(&0x0001_0000u32.to_be_bytes());
    maxp[4..6].copy_from_slice(&num_glyphs.to_be_bytes());
    if let Some(original) = source.table(b"maxp").filter(|table| table.len() >= 32) {
        maxp[6..32].copy_from_slice(&original[6..32]);
    }
    maxp
}

/// A `(3,1)` Windows Unicode `cmap` (format 4) from character to subset glyph id.
///
/// Format 4 is deliberate: it is the BMP subtable every viewer reads, and a simple PDF font
/// addresses only BMP characters by construction. Each segment uses a `glyphIdArray` rather
/// than `idDelta`, because characters are coded in first-use order while glyph ids are sorted,
/// so adjacent characters do not have adjacent glyph ids.
fn unicode_cmap(chars_to_glyph: &BTreeMap<char, u16>) -> Vec<u8> {
    let mut points: Vec<u16> = chars_to_glyph
        .keys()
        .filter(|ch| u32::from(**ch) <= u32::from(u16::MAX))
        .map(|ch| *ch as u32 as u16)
        .collect();
    points.sort_unstable();
    points.dedup();

    let mut segments: Vec<(u16, u16)> = Vec::new();
    for point in points {
        match segments.last_mut() {
            Some((_, end)) if *end != u16::MAX && *end + 1 == point => *end = point,
            _ => segments.push((point, point)),
        }
    }
    // The 0xFFFF terminator. If a real character already owns that code point, a zero-length
    // segment after it would be malformed; the format allows the terminator to be merged, so
    // only add it when absent.
    if segments.last().map(|(_, end)| *end) != Some(u16::MAX) {
        segments.push((0xFFFF, 0xFFFF));
    }

    let seg_count = segments.len() as u16;
    let power = largest_power_of_two(seg_count);
    let search_range = 2 * power;
    let entry_selector = u16::from(power.trailing_zeros() as u8);

    let mut subtable = Vec::new();
    subtable.extend_from_slice(&4u16.to_be_bytes());
    let length_at = subtable.len();
    subtable.extend_from_slice(&0u16.to_be_bytes());
    subtable.extend_from_slice(&0u16.to_be_bytes());
    subtable.extend_from_slice(&(seg_count * 2).to_be_bytes());
    subtable.extend_from_slice(&search_range.to_be_bytes());
    subtable.extend_from_slice(&entry_selector.to_be_bytes());
    subtable.extend_from_slice(&(seg_count * 2 - search_range).to_be_bytes());

    for (_, end) in &segments {
        subtable.extend_from_slice(&end.to_be_bytes());
    }
    subtable.extend_from_slice(&0u16.to_be_bytes()); // reservedPad
    for (start, _) in &segments {
        subtable.extend_from_slice(&start.to_be_bytes());
    }
    for _ in &segments {
        // Glyph ids are all below 0x8000, so a zero delta with a glyphIdArray is exact;
        // a non-zero delta would also have to be applied modulo 65536, which is less clear.
        subtable.extend_from_slice(&0u16.to_be_bytes());
    }
    let offsets_at = subtable.len();
    for _ in &segments {
        subtable.extend_from_slice(&0u16.to_be_bytes());
    }

    // `idRangeOffset[i]` is measured from the *element itself*, not from the start of the
    // array: glyph = *(A_i + idRangeOffset[i] + 2*(c - startCode[i])). The glyph array begins
    // just past the last offset, so the first entry of segment `i` is
    // `2*segCount - 2*i + 2*(entries before this segment)` bytes ahead of `A_i`.
    let mut entries_before = 0u16;
    for (index, (start, end)) in segments.iter().enumerate() {
        let at = offsets_at + index * 2;
        let offset = 2 * seg_count - 2 * index as u16 + entries_before;
        subtable[at..at + 2].copy_from_slice(&offset.to_be_bytes());
        for point in *start..=*end {
            let glyph = char::from_u32(u32::from(point))
                .and_then(|ch| chars_to_glyph.get(&ch).copied())
                .unwrap_or(0);
            subtable.extend_from_slice(&glyph.to_be_bytes());
        }
        entries_before = entries_before.saturating_add(2 * (end - start + 1));
    }

    let length = subtable.len() as u16;
    subtable[length_at..length_at + 2].copy_from_slice(&length.to_be_bytes());

    let mut cmap = Vec::new();
    cmap.extend_from_slice(&0u16.to_be_bytes());
    cmap.extend_from_slice(&1u16.to_be_bytes());
    cmap.extend_from_slice(&3u16.to_be_bytes());
    cmap.extend_from_slice(&1u16.to_be_bytes());
    cmap.extend_from_slice(&12u32.to_be_bytes());
    cmap.extend_from_slice(&subtable);
    cmap
}

/// The `/ToUnicode` CMap for a CID subset: two-byte source codes (R5).
///
/// A one-byte `codespacerange` here would be a different codec from the font it describes, and
/// a reader would split every CID in half. The `/CIDSystemInfo` **inside this stream** is
/// `UCS` because its destinations are Unicode code points — a different object and a different
/// claim from the `CIDFontType2`'s triple, which is why R3 forbids copying one into the other.
pub fn cid_to_unicode_cmap(assignments: &[(u16, char)]) -> Vec<u8> {
    let mut out = String::from(
        "/CIDInit /ProcSet findresource begin\n\
12 dict begin\nbegincmap\n\
/CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n\
/CMapName /Adobe-Identity-UCS def\n/CMapType 2 def\n\
1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n",
    );
    for chunk in assignments.chunks(100) {
        out.push_str(&format!("{} beginbfchar\n", chunk.len()));
        for (cid, ch) in chunk {
            let mut units = [0u16; 2];
            let target: String = ch
                .encode_utf16(&mut units)
                .iter()
                .map(|unit| format!("{unit:04X}"))
                .collect();
            out.push_str(&format!("<{cid:04X}> <{target}>\n"));
        }
        out.push_str("endbfchar\n");
    }
    out.push_str("endcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n");
    out.into_bytes()
}

/// The `/W` array, in R1's form: explicit `[cid [w1 w2 …]]` pairs, one list per contiguous run.
///
/// The `[cfirst clast w]` range form is deliberately never emitted. It only pays when many
/// consecutive codes share one width; our CIDs are consecutive *and individually measured*, so
/// a range would equalise advances the program distinguishes — a font that parses and lays out
/// wrongly, which is the silent-wrongness shape.
pub fn width_array(subset: &CidSubset) -> Object {
    let mut array: Vec<Object> = Vec::new();
    for (first, widths) in subset.width_entries() {
        array.push(Object::Integer(i64::from(first)));
        array.push(
            widths
                .into_iter()
                .map(|width| Object::Integer(i64::from(width)))
                .collect::<Vec<Object>>()
                .into(),
        );
    }
    Object::Array(array)
}

/// The `/CIDFontType2` descendant of a written `Type0`.
///
/// `/CIDSystemInfo` is `(Adobe) (Identity) (0)` (R3): it names the character collection *our*
/// CIDs are drawn from, and ours is identity-ordered by R2 and R5. `UCS` here would be a false
/// claim — our CIDs are glyph indices, not code points.
pub fn cid_descendant_dictionary(subset: &CidSubset, descriptor_id: lopdf::ObjectId) -> Dictionary {
    let mut descendant = Dictionary::new();
    descendant.set("Type", Object::Name(b"Font".to_vec()));
    descendant.set("Subtype", Object::Name(b"CIDFontType2".to_vec()));
    descendant.set(
        "BaseFont",
        Object::Name(subset.base_font.as_bytes().to_vec()),
    );
    let mut info = Dictionary::new();
    info.set("Registry", Object::string_literal("Adobe"));
    info.set("Ordering", Object::string_literal("Identity"));
    info.set("Supplement", Object::Integer(0));
    descendant.set("CIDSystemInfo", Object::Dictionary(info));
    // R2: `/Identity` is honest only because R5 placed each retained glyph at its own CID.
    descendant.set("CIDToGIDMap", Object::Name(b"Identity".to_vec()));
    descendant.set("DW", Object::Integer(i64::from(subset.default_width())));
    descendant.set("W", width_array(subset));
    descendant.set("FontDescriptor", Object::Reference(descriptor_id));
    descendant
}

/// The `Type0` root of a written CID font.
///
/// `/Encoding /Identity-H` states that two-byte codes *are* the CIDs, which is true here for
/// the same reason `/CIDToGIDMap /Identity` is. The source document's CMap is not copied — its
/// CIDs named the old font's glyphs, and inheriting them would make the new program answer for
/// characters it does not draw.
pub fn cid_font_dictionary(
    subset: &CidSubset,
    descendant_id: lopdf::ObjectId,
    cmap_id: lopdf::ObjectId,
) -> Dictionary {
    let mut font = Dictionary::new();
    font.set("Type", Object::Name(b"Font".to_vec()));
    font.set("Subtype", Object::Name(b"Type0".to_vec()));
    font.set(
        "BaseFont",
        Object::Name(subset.base_font.as_bytes().to_vec()),
    );
    font.set("Encoding", Object::Name(b"Identity-H".to_vec()));
    font.set(
        "DescendantFonts",
        Object::Array(vec![Object::Reference(descendant_id)]),
    );
    font.set("ToUnicode", Object::Reference(cmap_id));
    font
}

/// The `/FontFile2` stream for a CID subset, Flate-compressed.
pub fn cid_program_stream(subset: &CidSubset) -> Stream {
    let mut program = Stream::new(Dictionary::new(), subset.program.clone());
    program
        .dict
        .set("Length1", Object::Integer(subset.program.len() as i64));
    let _ = program.compress();
    program
}

/// The `/ToUnicode` stream for a CID subset, Flate-compressed.
pub fn cid_cmap_stream(subset: &CidSubset) -> Stream {
    let mut cmap = Stream::new(Dictionary::new(), cid_to_unicode_cmap(&subset.assignments));
    let _ = cmap.compress();
    cmap
}

/// The `/FontDescriptor` for a CID subset.
pub fn cid_descriptor_dictionary(subset: &CidSubset, program_id: lopdf::ObjectId) -> Dictionary {
    let mut descriptor = Dictionary::new();
    descriptor.set("Type", Object::Name(b"FontDescriptor".to_vec()));
    descriptor.set(
        "FontName",
        Object::Name(subset.base_font.as_bytes().to_vec()),
    );
    descriptor.set("Flags", Object::Integer(32));
    descriptor.set(
        "FontBBox",
        Object::Array(
            subset
                .bbox
                .iter()
                .map(|value| Object::Integer(i64::from(*value)))
                .collect(),
        ),
    );
    descriptor.set("ItalicAngle", Object::Integer(0));
    descriptor.set("Ascent", Object::Integer(i64::from(subset.ascent)));
    descriptor.set("Descent", Object::Integer(i64::from(subset.descent)));
    descriptor.set("CapHeight", Object::Integer(i64::from(subset.cap_height)));
    descriptor.set("StemV", Object::Integer(80));
    descriptor.set("FontFile2", Object::Reference(program_id));
    descriptor
}

/// A `name` table carrying only what OFL condition 2 requires to travel.
///
/// IDs 0 (copyright), 13 (licence) and 14 (licence URL) are copied from the source face.
/// IDs 1/4/6 are **not**: they name the Reserved Font Name, and this program is a Modified
/// Version. The PDF's `/BaseFont` carries the written name instead.
fn name_table(parsed: &ttf_parser::Face<'_>) -> Result<Vec<u8>, String> {
    let mut records: Vec<(u16, u16, u16, u16, Vec<u8>)> = Vec::new();
    for name in parsed.names() {
        if !matches!(name.name_id, 0 | 13 | 14) || !name.is_unicode() {
            continue;
        }
        let key = (
            platform_id(name.platform_id),
            name.encoding_id,
            name.language_id,
            name.name_id,
        );
        if records
            .iter()
            .any(|(p, e, l, id, _)| (*p, *e, *l, *id) == key)
        {
            continue;
        }
        records.push((key.0, key.1, key.2, key.3, name.name.to_vec()));
    }
    if records.is_empty() {
        return Err("the bundled face carries no copyright/licence records".into());
    }

    let count = records.len() as u16;
    let mut out = Vec::new();
    let mut storage = Vec::new();
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&count.to_be_bytes());
    out.extend_from_slice(&(6 + count * 12).to_be_bytes());
    for (platform, encoding, language, id, data) in &records {
        out.extend_from_slice(&platform.to_be_bytes());
        out.extend_from_slice(&encoding.to_be_bytes());
        out.extend_from_slice(&language.to_be_bytes());
        out.extend_from_slice(&id.to_be_bytes());
        out.extend_from_slice(&(data.len() as u16).to_be_bytes());
        out.extend_from_slice(&(storage.len() as u16).to_be_bytes());
        storage.extend_from_slice(data);
    }
    out.extend_from_slice(&storage);
    Ok(out)
}

/// `ttf-parser`'s platform id as the number the `name` table stores.
fn platform_id(platform: ttf_parser::PlatformId) -> u16 {
    match platform {
        ttf_parser::PlatformId::Unicode => 0,
        ttf_parser::PlatformId::Macintosh => 1,
        ttf_parser::PlatformId::Iso => 2,
        ttf_parser::PlatformId::Windows => 3,
        ttf_parser::PlatformId::Custom => 4,
    }
}

fn post_v3() -> Vec<u8> {
    let mut post = vec![0u8; 32];
    post[0..4].copy_from_slice(&0x0003_0000u32.to_be_bytes());
    post
}

fn checksum(data: &[u8]) -> u32 {
    let mut sum: u32 = 0;
    for chunk in data.chunks(4) {
        let mut word = [0u8; 4];
        word[..chunk.len()].copy_from_slice(chunk);
        sum = sum.wrapping_add(u32::from_be_bytes(word));
    }
    sum
}

fn be_u16(data: &[u8], at: usize) -> Result<u16, String> {
    data.get(at..at + 2)
        .map(|slice| u16::from_be_bytes([slice[0], slice[1]]))
        .ok_or_else(|| format!("truncated read at {at}"))
}

fn be_i16(data: &[u8], at: usize) -> Result<i16, String> {
    be_u16(data, at).map(|value| value as i16)
}

fn be_u32(data: &[u8], at: usize) -> Result<u32, String> {
    data.get(at..at + 4)
        .map(|slice| u32::from_be_bytes([slice[0], slice[1], slice[2], slice[3]]))
        .ok_or_else(|| format!("truncated read at {at}"))
}

/// The `/ToUnicode` CMap for a subset: chosen code → the character it draws.
///
/// Not optional (design §2.2). Without it, copy-paste and search over edited text read the new
/// bytes as if they were WinAnsi — for U+0100 written as code `0x01` the page renders correctly
/// while the text layer is silently wrong.
pub fn to_unicode_cmap(codes: &[(u8, char)]) -> Vec<u8> {
    let mut out = String::from(
        "/CIDInit /ProcSet findresource begin\n\
12 dict begin\nbegincmap\n\
/CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n\
/CMapName /Adobe-Identity-UCS def\n/CMapType 2 def\n\
1 begincodespacerange\n<00> <FF>\nendcodespacerange\n",
    );
    for chunk in codes.chunks(100) {
        out.push_str(&format!("{} beginbfchar\n", chunk.len()));
        for (code, ch) in chunk {
            let mut units = [0u16; 2];
            let target: String = ch
                .encode_utf16(&mut units)
                .iter()
                .map(|unit| format!("{unit:04X}"))
                .collect();
            out.push_str(&format!("<{code:02X}> <{target}>\n"));
        }
        out.push_str("endbfchar\n");
    }
    out.push_str("endcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n");
    out.into_bytes()
}

/// The `/Encoding /Differences` names for a subset: code → the character's Adobe glyph name.
///
/// `/BaseEncoding` is deliberately absent. A base of `WinAnsiEncoding` would leave every code
/// this subset does *not* name mapped to a WinAnsi character, and a viewer would then resolve
/// those codes through the subset's Unicode `cmap` and draw an unrelated glyph. With no base,
/// only the named codes have a meaning — which is the truth about this program.
pub fn differences_array(codes: &[(u8, char)]) -> Vec<Object> {
    let mut array = Vec::new();
    for (code, ch) in codes {
        array.push(Object::Integer(i64::from(*code)));
        array.push(Object::Name(glyph_name(*ch).into_bytes()));
    }
    array
}

/// The Adobe glyph name for a character, using the `uniXXXX` convention.
///
/// `uniXXXX` is defined by the Adobe Glyph List specification for *any* BMP character, so one
/// rule covers U+0100 as well as U+2192 without a 4000-entry name table. lopdf's
/// `Glyph::from_name` does not parse `uniXXXX` — it matches only its own table — so the
/// round-trip of an edited run is checked in `tests/embed_subset.rs` against the encoding this
/// writer actually produces, not assumed from the crate's behaviour.
pub fn glyph_name(ch: char) -> String {
    format!("uni{:04X}", ch as u32)
}

/// The font objects a subset is written as, before they are added to a document.
///
/// `/Subtype /TrueType`, a real `/FontDescriptor` with `/FontFile2`, `/Widths` from the
/// subset's `hmtx`, `/Encoding /Differences` for the chosen codes, and `/ToUnicode`. Every
/// substitute is self-describing; none of it depends on a face the viewer must already have.
pub fn font_dictionary(
    subset: &Subset,
    descriptor_id: lopdf::ObjectId,
    cmap_id: lopdf::ObjectId,
) -> Dictionary {
    let mut font = Dictionary::new();
    font.set("Type", Object::Name(b"Font".to_vec()));
    font.set("Subtype", Object::Name(b"TrueType".to_vec()));
    font.set(
        "BaseFont",
        Object::Name(subset.base_font.as_bytes().to_vec()),
    );
    font.set("FirstChar", Object::Integer(i64::from(subset.first_char)));
    font.set(
        "LastChar",
        Object::Integer(i64::from(subset.first_char) + subset.widths.len() as i64 - 1),
    );
    font.set(
        "Widths",
        Object::Array(
            subset
                .widths
                .iter()
                .map(|width| Object::Integer(i64::from(*width)))
                .collect(),
        ),
    );
    // No `/BaseEncoding`: see [`differences_array`]. Only the codes this subset names have a
    // meaning, which is the truth about the program.
    font.set(
        "Encoding",
        Object::Dictionary(dictionary! {
            "Type" => "Encoding",
            "Differences" => differences_array(&subset.codes),
        }),
    );
    font.set("FontDescriptor", Object::Reference(descriptor_id));
    font.set("ToUnicode", Object::Reference(cmap_id));
    font
}

/// The `/FontDescriptor` for a subset, naming its program.
pub fn descriptor_dictionary(subset: &Subset, program_id: lopdf::ObjectId) -> Dictionary {
    let mut descriptor = Dictionary::new();
    descriptor.set("Type", Object::Name(b"FontDescriptor".to_vec()));
    descriptor.set(
        "FontName",
        Object::Name(subset.base_font.as_bytes().to_vec()),
    );
    // Bit 6 (non-symbolic) alone: this is a Unicode-cmap TrueType face. Symbolic (bit 3) and
    // non-symbolic are mutually exclusive, and claiming both is a contradiction in the file.
    descriptor.set("Flags", Object::Integer(32));
    descriptor.set(
        "FontBBox",
        Object::Array(
            subset
                .bbox
                .iter()
                .map(|value| Object::Integer(i64::from(*value)))
                .collect(),
        ),
    );
    descriptor.set("ItalicAngle", Object::Integer(0));
    descriptor.set("Ascent", Object::Integer(i64::from(subset.ascent)));
    descriptor.set("Descent", Object::Integer(i64::from(subset.descent)));
    descriptor.set("CapHeight", Object::Integer(i64::from(subset.cap_height)));
    descriptor.set("StemV", Object::Integer(80));
    descriptor.set("FontFile2", Object::Reference(program_id));
    descriptor
}

/// The `/FontFile2` stream for a subset, Flate-compressed.
pub fn program_stream(subset: &Subset) -> Stream {
    let mut program = Stream::new(Dictionary::new(), subset.program.clone());
    program
        .dict
        .set("Length1", Object::Integer(subset.program.len() as i64));
    let _ = program.compress();
    program
}

/// The `/ToUnicode` stream for a subset, Flate-compressed.
pub fn cmap_stream(subset: &Subset) -> Stream {
    let mut cmap = Stream::new(Dictionary::new(), to_unicode_cmap(&subset.codes));
    let _ = cmap.compress();
    cmap
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A copy of `face`'s bytes with one table record's `offset`/`length` replaced.
    ///
    /// `Face` holds `&'static [u8]` because the bundled files are compiled in, so the mutated
    /// copy has to be leaked. That is confined to this test.
    fn face_with_record(face: &Face, tag: &[u8; 4], offset: u32, length: u32) -> &'static [u8] {
        let mut bytes = face.bytes.to_vec();
        let count = usize::from(be_u16(&bytes, 4).unwrap());
        for index in 0..count {
            let base = 12 + index * 16;
            if &bytes[base..base + 4] == tag {
                bytes[base + 8..base + 12].copy_from_slice(&offset.to_be_bytes());
                bytes[base + 12..base + 16].copy_from_slice(&length.to_be_bytes());
                return Box::leak(bytes.into_boxed_slice());
            }
        }
        panic!("face has no {tag:?} table");
    }

    /// A table range that runs past the end of the file is refused by *this* check, and by
    /// no later one.
    ///
    /// The tag is one `parse` never reads (`GSUB`), so nothing downstream can produce the
    /// same message. Asserting only `is_err()` would pass on `table()` returning `None` later
    /// with a different error, which is why this matches the specific refusal.
    ///
    /// Mutation-verified: deleting the `if` (keeping the record) makes this fail, because
    /// `parse` then returns `Ok`.
    ///
    /// The `None` arm of `checked_add` is *not* covered and cannot be: both operands are
    /// `u32` widened to `usize`, so on a 64-bit target the sum cannot wrap, and the arm is
    /// unreachable for every possible input. Measured: offset `u32::MAX - 8` plus length
    /// `0x20` is 2^32 + 23 -- an ordinary `Some(end)` past the file, not an overflow. A test
    /// aiming at that arm would pass whatever the code did, so there is not one.
    #[test]
    fn a_table_record_past_the_end_of_the_file_is_refused() {
        let face = face_for_style("Helvetica").expect("Helvetica maps to a bundled face");
        let corrupt = face_with_record(face, b"GSUB", 0xFFFF_FF00, 0x200);
        let error = match SourceTables::parse(corrupt) {
            Ok(_) => panic!("an out-of-range table must not parse"),
            Err(error) => error,
        };
        assert!(
            error.contains("extends past the file"),
            "the refusal must name the range check, not surface later as a missing table: {error}"
        );
    }
}
