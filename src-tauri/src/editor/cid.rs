//! Reading and writing CID-keyed (Type0) text (P2.7.5, closes C-4).
//!
//! A Type0 run's bytes are **codes**, not character codes: two bytes each under Identity-H,
//! naming a CID that `/CIDToGIDMap` turns into a glyph id. Editing one means turning the
//! replacement's characters into such codes, and there are two ways to do that with
//! different guarantees:
//!
//! 1. **Reverse `/ToUnicode`.** The document states which character each code draws, so a
//!    code found this way renders *and means* what the user typed. Tried first, always.
//! 2. **Program `cmap` → glyph id**, valid only under `/CIDToGIDMap /Identity`, where a CID
//!    *is* a glyph id. This reaches text the CMap never named (a Monaco subset that only
//!    maps the en dash, say) at the cost of trusting the program's glyph order.
//!
//! Route 1 before route 2 is load-bearing, not a tiebreak: a Wingdings bullet is code
//! `<001F>` in the document, but the program's `cmap` may map U+F0E0 to a different glyph id,
//! and taking that would silently change which picture is drawn.
//!
//! The CMap is parsed here rather than through `lopdf::Encoding`: lopdf keeps its
//! `encodings` module private, and a reverse lookup that drops characters silently (as
//! `string_to_bytes` does) is exactly what an edit path must not use.

use lopdf::{Dictionary, Document, Object, ObjectId};

use crate::editor::substitute::embedded_program;
use crate::editor::textlocate::{FontInfo, XObjectStep};

/// Why a CID replacement cannot be encoded.
#[derive(Debug, Clone, PartialEq)]
pub enum CidRefusal {
    /// No `/ToUnicode`, so the bytes cannot be read back and the run is not editable.
    Unreadable,
    /// A character that neither the CMap nor the program can express.
    Uncoverable(char),
}

impl CidRefusal {
    /// The sentence the user sees. Naming the character matters: "cannot be encoded" without
    /// saying which character is not actionable.
    pub fn message(&self) -> String {
        match self {
            CidRefusal::Unreadable => {
                "this text uses a CID/Type0 font that publishes no usable /ToUnicode, so it \
                 cannot be read back reliably"
                    .to_string()
            }
            CidRefusal::Uncoverable(ch) => format!(
                "this text uses a CID/Type0 font whose embedded program has no glyph for {ch:?}"
            ),
        }
    }
}

/// A `/ToUnicode` CMap, reduced to what editing needs: both directions for two-byte codes.
#[derive(Debug, Default, Clone)]
pub struct CodeMap {
    /// (code, character), code length two bytes.
    forward: Vec<(u16, char)>,
    /// The same pairs reversed, first writer winning for a repeated character.
    reverse: Vec<(char, u16)>,
}

impl CodeMap {
    /// Parses the `beginbfchar` / `beginbfrange` sections of a `/ToUnicode` stream.
    ///
    /// Only two-byte source codes are read: a Type0 font under Identity-H writes two-byte
    /// codes, and a one-byte entry would belong to a different codec. A section that cannot
    /// be read is skipped rather than failing the whole map, so one malformed entry cannot
    /// cost the user every other character.
    pub fn parse(bytes: &[u8]) -> Option<CodeMap> {
        let text = String::from_utf8_lossy(bytes);
        let mut map = CodeMap::default();

        for section in text.split("beginbfchar").skip(1) {
            for (source, target) in hex_pairs(section) {
                map.insert(source, target);
            }
        }

        for section in text.split("beginbfrange").skip(1) {
            for (low, high, target, targets) in hex_ranges(section) {
                if let Some(list) = targets {
                    for (offset, target) in list.into_iter().enumerate() {
                        if let Some(code) = low.checked_add(offset as u16) {
                            map.insert(code, target);
                        }
                    }
                    continue;
                }
                let Some(target) = target else { continue };
                for code in low..=high {
                    let Some(ch) = shift(target, code - low) else {
                        continue;
                    };
                    map.insert(code, ch);
                }
            }
        }

        if map.forward.is_empty() {
            None
        } else {
            Some(map)
        }
    }

    fn insert(&mut self, code: u16, ch: char) {
        if self.forward.iter().any(|(existing, _)| *existing == code) {
            return;
        }
        self.forward.push((code, ch));
        if !self.reverse.iter().any(|(existing, _)| *existing == ch) {
            self.reverse.push((ch, code));
        }
    }

    /// The character a two-byte code draws.
    pub fn character(&self, code: u16) -> Option<char> {
        self.forward
            .iter()
            .find(|(candidate, _)| *candidate == code)
            .map(|(_, ch)| *ch)
    }

    /// The code that draws `ch`.
    pub fn code(&self, ch: char) -> Option<u16> {
        self.reverse
            .iter()
            .find(|(candidate, _)| *candidate == ch)
            .map(|(_, code)| *code)
    }
}

/// One Type0 font, resolved far enough to decode, encode, and measure its runs.
pub struct CidFont {
    /// Route 1 and decoding. `None` makes the font unreadable.
    cmap: Option<CodeMap>,
    /// Decompressed program, for route 2. `None` when unembedded or unparseable.
    program: Option<Vec<u8>>,
    /// `/CIDToGIDMap /Identity`: a CID can be used directly as a glyph id.
    identity_cid_to_gid: bool,
    /// `/DW`: the advance for every CID the `/W` table does not name.
    default_width: Option<f32>,
    /// `/W`, flattened to (cid, width) pairs in thousandths of an em.
    widths: Vec<(u16, f32)>,
}

impl CidFont {
    /// Reads a Type0 font dictionary. `None` when this is not a Type0 font.
    ///
    /// The metrics live on the descendant (`/DescendantFonts[0]`); a Type0 without one still
    /// resolves, because its `/ToUnicode` alone can decode and encode — it just has no widths
    /// and the width gate will refuse rather than guess.
    pub fn resolve(document: &Document, font: &Dictionary) -> Option<CidFont> {
        let subtype = font
            .get(b"Subtype")
            .ok()
            .and_then(|object| object.as_name().ok())?;
        if !subtype.eq_ignore_ascii_case(b"Type0") {
            return None;
        }

        let cmap = font
            .get(b"ToUnicode")
            .ok()
            .and_then(|object| stream_content(document, object))
            .and_then(|bytes| CodeMap::parse(&bytes));

        let descendant = font
            .get(b"DescendantFonts")
            .ok()
            .and_then(|object| deref(document, object))
            .and_then(|object| object.as_array().ok())
            .and_then(|array| array.first())
            .and_then(|object| deref(document, object))
            .and_then(|object| object.as_dict().ok());

        let Some(descendant) = descendant else {
            return Some(CidFont {
                cmap,
                program: None,
                identity_cid_to_gid: false,
                default_width: None,
                widths: Vec::new(),
            });
        };

        let program = embedded_program(document, descendant)
            .filter(|program| ttf_parser::Face::parse(program, 0).is_ok());

        let identity_cid_to_gid = descendant
            .get(b"CIDToGIDMap")
            .ok()
            .and_then(|object| object.as_name().ok())
            .is_some_and(|name| name.eq_ignore_ascii_case(b"Identity"));

        let default_width = descendant
            .get(b"DW")
            .ok()
            .and_then(|object| object.as_i64().ok())
            .map(|value| value as f32);

        Some(CidFont {
            cmap,
            program,
            identity_cid_to_gid,
            default_width,
            widths: width_table(document, descendant).unwrap_or_default(),
        })
    }

    /// True when the run's bytes can be read back, which is what makes it editable at all.
    pub fn is_readable(&self) -> bool {
        self.cmap.is_some()
    }

    /// True when every character of `text` can be expressed.
    pub fn covers(&self, text: &str) -> bool {
        self.encode(text).is_ok()
    }

    /// Turns `text` into the two-byte codes the font draws, route 1 before route 2.
    pub fn encode(&self, text: &str) -> Result<Vec<u8>, CidRefusal> {
        if !self.is_readable() {
            return Err(CidRefusal::Unreadable);
        }
        let face = self.program.as_deref().and_then(parse_face);
        let mut out = Vec::with_capacity(text.chars().count() * 2);
        for ch in text.chars() {
            let code = self
                .code_for_char(ch)
                .or_else(|| match (&face, self.identity_cid_to_gid) {
                    (Some(face), true) => face.glyph_index(ch).map(|glyph| glyph.0),
                    _ => None,
                })
                .ok_or(CidRefusal::Uncoverable(ch))?;
            out.extend_from_slice(&code.to_be_bytes());
        }
        Ok(out)
    }

    /// Decodes a run's bytes the way the picker does, so both agree on what was read.
    pub fn decode(&self, bytes: &[u8]) -> Option<String> {
        let cmap = self.cmap.as_ref()?;
        // An odd trailing byte is not a code; the run is unreadable rather than invented.
        if !bytes.len().is_multiple_of(2) {
            return None;
        }
        let mut out = String::new();
        for chunk in bytes.chunks_exact(2) {
            let code = u16::from_be_bytes([chunk[0], chunk[1]]);
            out.push(cmap.character(code)?);
        }
        Some(out)
    }

    /// The advance width of `codes` in points, from `/W` then `/DW`.
    ///
    /// `None` when the font names neither, so the caller refuses instead of quoting a width
    /// the renderer will disagree with (the T22 direction).
    pub fn advance(&self, codes: &[u8], font_size: f32) -> Option<f32> {
        let mut thousandths = 0.0_f32;
        for chunk in codes.chunks_exact(2) {
            let cid = u16::from_be_bytes([chunk[0], chunk[1]]);
            let width = self
                .widths
                .iter()
                .find(|(candidate, _)| *candidate == cid)
                .map(|(_, width)| *width)
                .or(self.default_width)?;
            thousandths += width;
        }
        Some(thousandths * font_size / 1000.0)
    }

    fn code_for_char(&self, ch: char) -> Option<u16> {
        self.cmap.as_ref()?.code(ch)
    }
}

fn parse_face(program: &[u8]) -> Option<ttf_parser::Face<'_>> {
    ttf_parser::Face::parse(program, 0).ok()
}

/// A hex string token (`<0084>`) as a code and the character it denotes.
fn hex_string(bytes: &str, start: usize) -> Option<(u16, usize)> {
    let open = bytes[start..].find('<')? + start;
    let close = bytes[open..].find('>')? + open;
    let digits: String = bytes[open + 1..close]
        .chars()
        .filter(|ch| ch.is_ascii_hexdigit())
        .collect();
    let code = u16::from_str_radix(&digits, 16).ok()?;
    Some((code, close + 1))
}

/// `bfchar` entries: `<src> <dst>` pairs, UTF-16BE `dst` reduced to its first character.
fn hex_pairs(section: &str) -> Vec<(u16, char)> {
    let body = section.split("endbfchar").next().unwrap_or("");
    let mut pairs = Vec::new();
    let mut cursor = 0;
    while let Some((source, after)) = hex_string(body, cursor) {
        let Some((target, next)) = hex_string(body, after) else {
            break;
        };
        if let Some(ch) = char::from_u32(u32::from(target)) {
            pairs.push((source, ch));
        }
        cursor = next;
    }
    pairs
}

/// One `bfrange` entry: the code span and the target it maps.
type RangeEntry = (u16, u16, Option<char>, Option<Vec<char>>);

/// `bfrange` entries: `<lo> <hi> <dst>` or `<lo> <hi> [<dst> …]`.
fn hex_ranges(section: &str) -> Vec<RangeEntry> {
    let body = section.split("endbfrange").next().unwrap_or("");
    let mut ranges = Vec::new();
    let mut cursor = 0;
    while let Some((low, after)) = hex_string(body, cursor) {
        let Some((high, after)) = hex_string(body, after) else {
            break;
        };
        let rest = &body[after..];
        let Some(open) = rest.find('[') else {
            let Some((target, next)) = hex_string(body, after) else {
                break;
            };
            ranges.push((low, high, char::from_u32(u32::from(target)), None));
            cursor = next;
            continue;
        };
        let close = rest[open..]
            .find(']')
            .map(|offset| open + offset)
            .unwrap_or(rest.len());
        let list = hex_strings(&rest[open..close]);
        ranges.push((low, high, None, Some(list)));
        cursor = after + close + 1;
    }
    ranges
}

/// Every `<…>` token in a slice, as characters.
fn hex_strings(slice: &str) -> Vec<char> {
    let mut out = Vec::new();
    let mut cursor = 0;
    while let Some((code, next)) = hex_string(slice, cursor) {
        if let Some(ch) = char::from_u32(u32::from(code)) {
            out.push(ch);
        }
        cursor = next;
    }
    out
}

/// Adds `offset` to the last code unit of `target`, as a `bfrange` continuation does.
fn shift(target: char, offset: u16) -> Option<char> {
    char::from_u32(u32::from(target).checked_add(u32::from(offset))?)
}

/// `/W`, flattened. Accepts both array forms: `[c [w1 w2 …]]` and `[cfirst clast w]`.
fn width_table(document: &Document, descendant: &Dictionary) -> Option<Vec<(u16, f32)>> {
    let array = descendant
        .get(b"W")
        .ok()
        .and_then(|object| deref(document, object))
        .and_then(|object| object.as_array().ok())?;

    let mut widths = Vec::new();
    let mut index = 0;
    while index < array.len() {
        let first = deref(document, &array[index]).and_then(|object| object.as_i64().ok())?;
        index += 1;
        let next = array
            .get(index)
            .and_then(|object| deref(document, object))?;

        if let Ok(list) = next.as_array() {
            for (offset, width) in list.iter().enumerate() {
                let width = deref(document, width).and_then(|object| object.as_i64().ok())?;
                widths.push((first as u16 + offset as u16, width as f32));
            }
            index += 1;
        } else {
            let last = next.as_i64().ok()?;
            index += 1;
            let width = array
                .get(index)
                .and_then(|object| deref(document, object))
                .and_then(|object| object.as_i64().ok())?;
            index += 1;
            for cid in first..=last {
                widths.push((cid as u16, width as f32));
            }
        }
    }
    Some(widths)
}

/// Follows a reference, leaving an inline object alone.
fn deref<'a>(document: &'a Document, object: &'a Object) -> Option<&'a Object> {
    match object {
        Object::Reference(id) => document.get_object(*id).ok(),
        other => Some(other),
    }
}

/// A stream's decompressed content.
fn stream_content(document: &Document, object: &Object) -> Option<Vec<u8>> {
    let mut stream = deref(document, object)?.as_stream().ok()?.clone();
    if stream.dict.get(b"Filter").is_ok() {
        stream.decompress().ok()?;
    }
    Some(stream.content)
}

/// Whether a font can be edited at all, for the picker's pre-computed verdict.
///
/// A simple font is handled by the existing pipeline. A Type0 is editable when it can be
/// decoded (`/ToUnicode` exists) and re-encoded — which follows from decoding, since route 1
/// then covers every character the CMap names and route 2 covers the rest when Identity.
pub fn admissible(document: &Document, font: &Dictionary, info: &FontInfo) -> bool {
    if crate::editor::textlocate::is_simple_font(info) {
        return true;
    }
    CidFont::resolve(document, font).is_some_and(|cid| cid.is_readable())
}

/// Resolves the CID codec for a run's font, when it is a Type0 font.
pub fn resolve_at(
    document: &Document,
    page_id: ObjectId,
    path: &[XObjectStep],
    font_key: &[u8],
) -> Option<CidFont> {
    let font = crate::editor::textlocate::font_dictionary_at(document, page_id, path, font_key)?;
    CidFont::resolve(document, &font)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::testpdf;

    fn font_of(document: &Document, key: &str) -> Dictionary {
        let page = crate::editor::textlocate::page_id(document, 0).unwrap();
        crate::editor::textlocate::font_dictionary_at(document, page, &[], key.as_bytes())
            .expect("fixture font")
    }

    /// The fixture's CMap is compressed, so parsing it also proves the stream is
    /// decompressed before it is read.
    #[test]
    fn parses_a_compressed_tounicode_in_both_directions() {
        let document = testpdf::cid_tounicode_page();
        let cid = CidFont::resolve(&document, &font_of(&document, "F1")).unwrap();

        assert!(cid.is_readable());
        assert_eq!(cid.decode(&[0x00, 0x02, 0x00, 0x03]).as_deref(), Some("BC"));
        assert_eq!(cid.encode("BC").unwrap(), vec![0x00, 0x02, 0x00, 0x03]);
    }

    #[test]
    fn a_type0_without_tounicode_is_not_readable() {
        let document = testpdf::cid_identity_page();
        let cid = CidFont::resolve(&document, &font_of(&document, "F1")).unwrap();

        assert!(!cid.is_readable());
        assert!(!cid.covers("CID"));
        assert_eq!(cid.encode("CID"), Err(CidRefusal::Unreadable));
    }

    /// C3: the character is named, because "cannot be encoded" alone does not tell the user
    /// which keystroke to remove.
    #[test]
    fn an_uncovered_character_is_refused_by_name() {
        let document = testpdf::cid_tounicode_page();
        let cid = CidFont::resolve(&document, &font_of(&document, "F1")).unwrap();

        assert_eq!(cid.encode("AZ"), Err(CidRefusal::Uncoverable('Z')));
    }

    /// C6: `/W` wins over `/DW`, and a font naming neither measures nothing.
    #[test]
    fn width_comes_from_w_before_dw() {
        let document = testpdf::cid_tounicode_page();
        let cid = CidFont::resolve(&document, &font_of(&document, "F1")).unwrap();

        // Two cids in `/W` at 600/1000 em, size 10.
        assert_eq!(cid.advance(&[0x00, 0x01, 0x00, 0x02], 10.0), Some(12.0));
    }

    #[test]
    fn a_font_with_no_w_or_dw_has_no_width() {
        let mut document = testpdf::cid_tounicode_page();
        let page = crate::editor::textlocate::page_id(&document, 0).unwrap();
        let font =
            crate::editor::textlocate::font_dictionary_at(&document, page, &[], b"F1").unwrap();
        let descendant = font.get(b"DescendantFonts").unwrap().as_array().unwrap()[0]
            .as_reference()
            .unwrap();
        let descendant = document
            .get_object_mut(descendant)
            .unwrap()
            .as_dict_mut()
            .unwrap();
        descendant.remove(b"W");
        descendant.remove(b"DW");

        let cid = CidFont::resolve(&document, &font_of(&document, "F1")).unwrap();
        assert_eq!(cid.advance(&[0x00, 0x01], 10.0), None);
    }

    /// C5: without `/Identity` a cid is not a glyph id, so route 2 must stay closed. Route 1
    /// still works, because the CMap decides meaning regardless of the mapping.
    #[test]
    fn a_non_identity_cid_to_gid_map_closes_route_two_only() {
        let document = testpdf::cid_tounicode_page();
        let page = crate::editor::textlocate::page_id(&document, 0).unwrap();
        let font =
            crate::editor::textlocate::font_dictionary_at(&document, page, &[], b"F1").unwrap();
        let descendant = font.get(b"DescendantFonts").unwrap().as_array().unwrap()[0]
            .as_reference()
            .unwrap();
        let mut document = document;
        document
            .get_object_mut(descendant)
            .unwrap()
            .as_dict_mut()
            .unwrap()
            .set("CIDToGIDMap", Object::Reference(descendant));
        let stream_id =
            document.add_object(lopdf::Stream::new(lopdf::dictionary! {}, vec![0u8; 512]));
        document
            .get_object_mut(descendant)
            .unwrap()
            .as_dict_mut()
            .unwrap()
            .set("CIDToGIDMap", stream_id);

        let cid = CidFont::resolve(&document, &font_of(&document, "F1")).unwrap();
        assert!(!cid.identity_cid_to_gid);
        assert_eq!(cid.encode("BC"), Ok(vec![0x00, 0x02, 0x00, 0x03]));
        // Route 2 is what would reach 'Z'; it must not be open.
        assert_eq!(cid.encode("Z"), Err(CidRefusal::Uncoverable('Z')));
    }

    /// C7: the program is read from a **compressed** `/FontFile2`. Before `embedded_program`
    /// decompressed, `ttf-parser` never saw a valid face in any real document, so route 2 and
    /// every "does the original font cover this?" answer were silently wrong.
    #[test]
    fn a_compressed_program_covers_its_own_glyphs() {
        let document = testpdf::compressed_program_cid_page(&['A', 'B', 'Z']);
        let cid = CidFont::resolve(&document, &font_of(&document, "F1")).unwrap();

        assert!(
            cid.program.is_some(),
            "the FontFile2 must decompress and parse"
        );
        // The CMap names only A and B; 'Z' is reachable only through the program.
        assert_eq!(cid.decode(&[0x00, 0x01]).as_deref(), Some("A"));
        // Glyph ids are assigned in character order: A=1, B=2, Z=3.
        assert_eq!(cid.encode("Z"), Ok(vec![0x00, 0x03]));
    }

    #[test]
    fn a_simple_font_is_admissible_and_a_type0_is_judged_by_its_cmap() {
        let simple = testpdf::simple_winansi();
        let page = crate::editor::textlocate::page_id(&simple, 0).unwrap();
        let info = crate::editor::textlocate::font_info(&simple, page, b"F1").unwrap();
        let font = font_of(&simple, "F1");
        assert!(admissible(&simple, &font, &info));

        let cid_page = testpdf::cid_identity_page();
        let page = crate::editor::textlocate::page_id(&cid_page, 0).unwrap();
        let info = crate::editor::textlocate::font_info(&cid_page, page, b"F1").unwrap();
        let font = font_of(&cid_page, "F1");
        assert!(!admissible(&cid_page, &font, &info));
    }
}
