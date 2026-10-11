//! Locating text runs in a page's content stream via `lopdf` (P2.4 T4).
//!
//! This module is the **authoritative** way to address a piece of text on a page. A
//! run is identified by its position in the *decoded content stream* — the index of the
//! text-showing operation and the path to the string operand *inside* it — never by
//! PDFium's reconstructed text. PDFium applies `/ToUnicode` maps, splits objects and
//! joins ligatures; a string that reads `fi` in the UI is very often `\x0C` plus a
//! `/Differences` entry in the bytes. Editing by PDFium's view would therefore write
//! into the wrong bytes (or invent bytes the font cannot encode), so PDFium's text is
//! used for the picker's *display* only, and matching at write time goes through here.
//!
//! Nothing in this module writes. Mutation lives in [`crate::editor::textedit`], which
//! calls back into these helpers so the parse that a run's identity came from is the
//! same parse that is re-validated immediately before the edit lands.

use lopdf::{content::Operation, Document, Object, ObjectId};

use crate::model::AppError;

/// Bounds a page's decompressed content (P2.4 T3e / F3).
///
/// A page content stream is normally a few kilobytes. Even a heavily designed page is
/// under a megabyte. 8 MiB is two orders of magnitude of headroom while still refusing
/// a decompression bomb — a small stream that inflates without limit — instead of
/// allocating it. Exceeding this is a refusal ([`AppError::ContentTooLarge`]), not a
/// best-effort attempt.
pub const CONTENT_LIMIT: usize = 8 * 1024 * 1024;

/// Address of one string operand within a page's decoded content.
///
/// `operand_path` is a path rather than a single index because a `TJ` array holds
/// several strings: the array is operand 0 of the operation and the strings are its
/// elements. A run's identity must survive re-encoding the whole stream, so the path has
/// to name the exact nesting the user picked.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunId {
    /// Index of the text-showing operation in the decoded content.
    pub op_index: usize,
    /// Path from that operation's operands down to the string object. Empty would mean
    /// the operand itself, which is never a string for the operators handled here.
    pub operand_path: Vec<usize>,
}

/// A text run as recovered from the content bytes.
#[derive(Debug, Clone, PartialEq)]
pub struct RawRun {
    pub id: RunId,
    /// The raw bytes as they appear in the stream (a PDF string's contents), which is
    /// what an edit actually replaces.
    pub bytes: Vec<u8>,
    /// The bytes decoded through the document's *own* font encoding. This is the string
    /// shown to the user and the value re-checked at write time (T4).
    pub text: String,
    /// Resource name of the font the run draws with (`F1`), for the font gates.
    pub font_key: Vec<u8>,
    /// The operator that showed the text (`Tj`, `TJ`, `'`, `"`).
    pub operator: String,
}

impl RawRun {
    /// The string a run's identity is re-validated against at write time.
    pub fn signature(&self) -> &str {
        &self.text
    }
}

/// Text-showing operators (ISO 32000-1 9.4.3) and where each keeps its string.
///
/// `Tj` and `'` and `"` each take one string; `TJ` takes an array of strings and
/// positioning numbers. Only the strings are runs.
fn text_operand_paths(operation: &Operation) -> Vec<Vec<usize>> {
    match operation.operator.as_str() {
        "Tj" | "'" | "\"" => {
            // `"` is `aw ac string "`, so the string is the *last* operand, not the
            // first. Getting this wrong would edit a number.
            if operation.operands.last().is_some_and(is_string) {
                vec![vec![operation.operands.len() - 1]]
            } else {
                Vec::new()
            }
        }
        "TJ" => {
            let mut paths = Vec::new();
            for (index, operand) in operation.operands.iter().enumerate() {
                if let Object::Array(items) = operand {
                    for (inner, item) in items.iter().enumerate() {
                        if is_string(item) {
                            paths.push(vec![index, inner]);
                        }
                    }
                }
            }
            paths
        }
        _ => Vec::new(),
    }
}

fn is_string(object: &Object) -> bool {
    matches!(object, Object::String(_, _))
}

/// Reads the string object a path points at.
fn string_at<'a>(operation: &'a Operation, path: &[usize]) -> Option<&'a [u8]> {
    let mut current = operation.operands.get(*path.first()?)?;
    for index in &path[1..] {
        current = current.as_array().ok()?.get(*index)?;
    }
    match current {
        Object::String(bytes, _) => Some(bytes),
        _ => None,
    }
}

/// The mutable string object a path points at.
fn string_at_mut<'a>(operation: &'a mut Operation, path: &[usize]) -> Option<&'a mut Vec<u8>> {
    let mut current = operation.operands.get_mut(*path.first()?)?;
    for index in &path[1..] {
        current = current.as_array_mut().ok()?.get_mut(*index)?;
    }
    match current {
        Object::String(bytes, _) => Some(bytes),
        _ => None,
    }
}

/// Resolves a 0-based page index to its page object id.
pub fn page_id(document: &Document, page: u32) -> Result<ObjectId, AppError> {
    let pages = document.get_pages();
    pages
        .get(&(page + 1))
        .copied()
        .ok_or_else(|| AppError::Other(format!("page {} is out of range", page + 1)))
}

/// Decodes a page's content, refusing beyond the cap (T3e).
pub fn decoded_content(
    document: &Document,
    page_id: ObjectId,
) -> Result<lopdf::content::Content<Vec<Operation>>, AppError> {
    let bytes = document
        .get_page_content_with_limit(page_id, CONTENT_LIMIT)
        .map_err(|error| match error {
            lopdf::Error::Decompress(lopdf::DecompressError::MemoryLimitExceeded { .. }) => {
                AppError::ContentTooLarge
            }
            other => AppError::Other(format!("could not read page content: {other}")),
        })?;

    lopdf::content::Content::decode(&bytes)
        .map_err(|error| AppError::Other(format!("could not parse page content: {error}")))
}

/// Every text run on a page, in content order.
///
/// A run whose font cannot be resolved or whose bytes cannot be decoded through the
/// font's encoding is **dropped** rather than returned with a guessed string: an
/// unreadable run cannot be shown to the user or matched later, so offering it would
/// only promise an edit the write path has to refuse.
pub fn locate_runs(document: &Document, page_id: ObjectId) -> Result<Vec<RawRun>, AppError> {
    let content = decoded_content(document, page_id)?;
    let fonts = document
        .get_page_fonts(page_id)
        .map_err(|error| AppError::Other(format!("could not read page fonts: {error}")))?;

    let mut runs = Vec::new();

    for (op_index, operation) in content.operations.iter().enumerate() {
        for path in text_operand_paths(operation) {
            let Some(bytes) = string_at(operation, &path) else {
                continue;
            };

            // The font in force is the last `/Fx Tf` before this operation. A run with
            // no font cannot be decoded, and its encoding gates are unknowable.
            let Some(font_key) = font_in_force(&content.operations, op_index) else {
                continue;
            };
            let Some(font) = fonts.get(&font_key).copied() else {
                continue;
            };
            let Ok(encoding) = font.get_font_encoding(document) else {
                continue;
            };
            let Ok(text) = encoding.bytes_to_string(bytes) else {
                continue;
            };
            if text.is_empty() {
                continue;
            }

            runs.push(RawRun {
                id: RunId {
                    op_index,
                    operand_path: path,
                },
                bytes: bytes.to_vec(),
                text,
                font_key: font_key.clone(),
                operator: operation.operator.clone(),
            });
        }
    }

    Ok(runs)
}

/// The font resource name in force at `op_index`, i.e. the last `Tf` at or before it.
///
/// Deliberately a scan over the decoded operations rather than a full text state
/// machine: `Tf` inside a form XObject or a nested `q`/`Q` is out of scope for Stage 1,
/// and the gates refuse anything whose font this simple walk cannot resolve.
fn font_in_force(operations: &[Operation], op_index: usize) -> Option<Vec<u8>> {
    font_in_force_with_index(operations, op_index).map(|(_, name)| name)
}

/// The index of the governing `Tf` and the resource name it sets.
///
/// The index matters for MECH-A: the substitute is installed by rewriting that exact
/// `Tf`, so the swap can be recorded and reverted at the same address the run read.
fn font_in_force_with_index(operations: &[Operation], op_index: usize) -> Option<(usize, Vec<u8>)> {
    operations[..=op_index]
        .iter()
        .enumerate()
        .rev()
        .find(|(_, operation)| operation.operator == "Tf")
        .and_then(|(index, operation)| {
            operation
                .operands
                .first()
                .and_then(|operand| operand.as_name().ok())
                .map(|name| (index, name.to_vec()))
        })
}

/// The properties a run's font must have for Stage 1, read from the font dictionary.
#[derive(Debug, Clone, PartialEq)]
pub struct FontInfo {
    pub key: Vec<u8>,
    pub base_font: Option<String>,
    pub subtype: Option<String>,
}

/// Reads the font dictionary's `/Subtype` and `/BaseFont`.
pub fn font_info(document: &Document, page_id: ObjectId, font_key: &[u8]) -> Option<FontInfo> {
    let fonts = document.get_page_fonts(page_id).ok()?;
    let font = fonts.get(font_key).copied()?;
    let read = |key: &[u8]| {
        font.get(key)
            .ok()
            .and_then(|object| object.as_name().ok())
            .map(|name| String::from_utf8_lossy(name).into_owned())
    };
    Some(FontInfo {
        key: font_key.to_vec(),
        base_font: read(b"BaseFont"),
        subtype: read(b"Subtype"),
    })
}

/// True when a base font name carries a subset prefix: six uppercase letters then `+`
/// (e.g. `ABCDEF+Helvetica`), the convention PDF producers use for an embedded subset.
///
/// A subset font contains only the glyphs the original document used, so a replacement
/// string using any other glyph would render as `.notdef` — a row of boxes that looks
/// like file corruption. Stage 1 refuses it (T3c) rather than shipping that.
pub fn is_subset_font(base_font: Option<&str>) -> bool {
    let Some(name) = base_font else {
        return false;
    };
    let bytes = name.as_bytes();
    bytes.len() > 7 && bytes[6] == b'+' && bytes[..6].iter().all(u8::is_ascii_uppercase)
}

/// True when the font is CID-keyed (Type0 / Identity-H), which Stage 1 cannot edit.
///
/// CID text is a sequence of multi-byte glyph ids, not character codes: replacing a
/// string means re-mapping glyphs, which needs the embedded program. Refused up front
/// (T3a) so the refusal names the real reason.
pub fn is_cid_font(info: &FontInfo) -> bool {
    let subtype_is_type0 = info
        .subtype
        .as_deref()
        .is_some_and(|subtype| subtype.eq_ignore_ascii_case("Type0"));
    let name_is_cid = info
        .base_font
        .as_deref()
        .is_some_and(|name| name.to_ascii_uppercase().contains("IDENTITY-"));
    subtype_is_type0 || name_is_cid
}

/// True for the simple font subtypes Stage 1 can rewrite: Type1, TrueType, MMType1.
pub fn is_simple_font(info: &FontInfo) -> bool {
    match info.subtype.as_deref() {
        Some(subtype) => matches!(
            subtype.to_ascii_lowercase().as_str(),
            "type1" | "truetype" | "mmtype1"
        ),
        // No `/Subtype` is not evidence of a simple font; refuse rather than guess.
        None => false,
    }
}

/// Replaces one run's bytes in a freshly decoded copy of the page, returning the
/// `ContentOperation` inverse for undo (T5).
///
/// Re-decoding instead of reusing the content the run was located in is the point: the
/// bytes on disk may have changed since the user picked the run, and the inverse must
/// describe the document as it is *now*. `expected` is the text the user saw; a
/// disagreement is a refusal (T4), never an overwrite in the wrong place.
pub fn apply_text(
    document: &mut Document,
    page_index: u32,
    page_id: ObjectId,
    id: &RunId,
    expected: &str,
    new_bytes: &[u8],
) -> Result<ContentOperation, AppError> {
    let mut content = decoded_content(document, page_id)?;

    // Everything that reads the content immutably happens before the mutable borrow of
    // the target operation: `font_in_force` scans `content.operations`, and holding that
    // borrow across `operations.get_mut` is what the borrow checker refuses.
    let old_bytes = content
        .operations
        .get(id.op_index)
        .and_then(|operation| string_at(operation, &id.operand_path))
        .ok_or(AppError::TextRunMismatch)?
        .to_vec();

    let fonts = document
        .get_page_fonts(page_id)
        .map_err(|error| AppError::Other(format!("could not read page fonts: {error}")))?;
    let font_key =
        font_in_force(&content.operations, id.op_index).ok_or(AppError::TextRunMismatch)?;
    let font = fonts
        .get(&font_key)
        .copied()
        .ok_or(AppError::TextRunMismatch)?;
    let encoding = font
        .get_font_encoding(document)
        .map_err(|_| AppError::TextRunMismatch)?;
    let current_text = encoding
        .bytes_to_string(&old_bytes)
        .map_err(|_| AppError::TextRunMismatch)?;
    if current_text != expected {
        return Err(AppError::TextRunMismatch);
    }

    let operation = content
        .operations
        .get_mut(id.op_index)
        .ok_or(AppError::TextRunMismatch)?;
    let slot = string_at_mut(operation, &id.operand_path).ok_or(AppError::TextRunMismatch)?;
    *slot = new_bytes.to_vec();

    let encoded = content
        .encode()
        .map_err(|error| AppError::Other(format!("could not re-encode page content: {error}")))?;

    // F2: the whole page becomes one stream. Legal, and the D2 copy semantics mean the
    // original file keeps its own stream layout.
    document
        .change_page_content(page_id, encoded)
        .map_err(|error| AppError::Other(format!("could not replace page content: {error}")))?;

    Ok(ContentOperation {
        page: page_index,
        op_index: id.op_index,
        operand_path: id.operand_path.clone(),
        old_bytes,
        new_bytes: new_bytes.to_vec(),
        font_swap: None,
    })
}

/// Installs a standard-14 substitute font as a resource on `page_id` and rewrites the
/// run's governing `Tf` to name it, returning the [`FontSwap`] for undo (P2.4 Stage 2
/// T10/T15).
///
/// The substitute dictionary is a document-local, standard-14 `/Type1` entry with
/// `/Encoding /WinAnsiEncoding` — no font stream, no CMap, nothing that needs a program
/// file the viewer must have (T15). The resource *name* is the same for every substitute
/// on a page (`FS1`), so a second substitution on the page reuses one dictionary rather
/// than accumulating one per edit; that is also what keeps the resource dictionary
/// bounded on a long session.
pub fn apply_font_swap(
    document: &mut Document,
    page_index: u32,
    page_id: ObjectId,
    id: &RunId,
    base_font: &str,
) -> Result<FontSwap, AppError> {
    let _ = (page_index, base_font);
    ensure_substitute_resource(document, page_id, base_font)?;

    let mut content = decoded_content(document, page_id)?;
    let (tf_index, old_name) = font_in_force_with_index(&content.operations, id.op_index)
        .ok_or(AppError::TextRunMismatch)?;

    // `Tf` is `name size Tf`; operand 0 is the resource name. Rewriting it is the whole
    // swap: the substitute declares `/WinAnsiEncoding`, the encoding the run's bytes were
    // already decoded through, so no byte remapping is needed for MECH-A.
    {
        let operation = content
            .operations
            .get_mut(tf_index)
            .ok_or(AppError::TextRunMismatch)?;
        let slot = operation
            .operands
            .first_mut()
            .ok_or(AppError::TextRunMismatch)?;
        *slot = Object::Name(SUBSTITUTE_NAME.to_vec());
    }

    let encoded = content
        .encode()
        .map_err(|error| AppError::Other(format!("could not re-encode page content: {error}")))?;
    document
        .change_page_content(page_id, encoded)
        .map_err(|error| AppError::Other(format!("could not replace page content: {error}")))?;

    Ok(FontSwap {
        op_index: tf_index,
        operand_path: vec![0],
        old_name,
        new_name: SUBSTITUTE_NAME.to_vec(),
    })
}

/// The resource name every MECH-A substitute is installed under.
///
/// One name per document, not per substitute: a page that substitutes twice reuses one
/// dictionary rather than accumulating one entry per edit, which keeps the resource
/// dictionary bounded. On a document whose pages *share* one `/Resources` object (the
/// synthetic fixtures do), a later substitution to a different family replaces the
/// earlier one's dictionary — acceptable for MECH-A, whose substitutes are all
/// standard-14 and all WinAnsi-encoded.
const SUBSTITUTE_NAME: &[u8] = b"FS1";

/// Ensures the page's `/Font` resources map [`SUBSTITUTE_NAME`] to a standard-14
/// dictionary for `base_font` (P2.4 Stage 2 T15).
///
/// Standard-14 needs no program: the entry is a document-local `/Type1` dictionary with
/// `/Encoding /WinAnsiEncoding`. No stream, no CMap. The page's `/Resources` may be an
/// indirect reference or an inline dictionary; both are handled.
fn ensure_substitute_resource(
    document: &mut Document,
    page_id: ObjectId,
    base_font: &str,
) -> Result<ObjectId, AppError> {
    let mut font_dictionary = lopdf::Dictionary::new();
    font_dictionary.set("Type", Object::Name(b"Font".to_vec()));
    font_dictionary.set("Subtype", Object::Name(b"Type1".to_vec()));
    font_dictionary.set("BaseFont", Object::Name(base_font.as_bytes().to_vec()));
    font_dictionary.set("Encoding", Object::Name(b"WinAnsiEncoding".to_vec()));
    let substitute_id = document.add_object(font_dictionary);

    // `get_page_resources` walks the page and its `/Parent` chain; the first reference it
    // reports is the page's own `/Resources` when that is indirect. An inline `/Resources`
    // (or an absent one) reports nothing and is handled on the page dictionary itself.
    let (_, resource_ids) = document
        .get_page_resources(page_id)
        .map_err(|error| AppError::Other(format!("could not read page resources: {error}")))?;

    if let Some(resources_id) = resource_ids.first().copied() {
        let resources = document
            .get_object_mut(resources_id)
            .map_err(|error| AppError::Other(format!("page resources unreadable: {error}")))?
            .as_dict_mut()
            .map_err(|error| {
                AppError::Other(format!("page resources not a dictionary: {error}"))
            })?;
        set_font_entry(resources, substitute_id)?;
    } else {
        let page = document
            .get_object_mut(page_id)
            .map_err(|error| AppError::Other(format!("page object unreadable: {error}")))?
            .as_dict_mut()
            .map_err(|error| AppError::Other(format!("page object not a dictionary: {error}")))?;
        if !page.has(b"Resources") {
            page.set(b"Resources", Object::Dictionary(lopdf::Dictionary::new()));
        }
        let resources = page
            .get_mut(b"Resources")
            .and_then(Object::as_dict_mut)
            .map_err(|_| {
                AppError::Other(
                    "page /Resources is an indirect reference MECH-A cannot rewrite".to_string(),
                )
            })?;
        set_font_entry(resources, substitute_id)?;
    }

    Ok(substitute_id)
}

/// Sets `/Font /FS1` in a resources dictionary, creating `/Font` when absent.
fn set_font_entry(
    resources: &mut lopdf::Dictionary,
    substitute_id: ObjectId,
) -> Result<(), AppError> {
    if !resources.has(b"Font") {
        resources.set(b"Font", Object::Dictionary(lopdf::Dictionary::new()));
    }
    let fonts = resources
        .get_mut(b"Font")
        .and_then(Object::as_dict_mut)
        .map_err(|_| {
            AppError::Other(
                "page /Font map is an indirect form MECH-A does not rewrite".to_string(),
            )
        })?;
    fonts.set(SUBSTITUTE_NAME, Object::Reference(substitute_id));
    Ok(())
}

/// The inverse of one text edit (T5). Applying `old_bytes` where `new_bytes` is now is
/// the exact undo; no document re-hydration is needed.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContentOperation {
    /// 0-based page the run lives on.
    pub page: u32,
    pub op_index: usize,
    pub operand_path: Vec<usize>,
    pub old_bytes: Vec<u8>,
    pub new_bytes: Vec<u8>,
    /// The font-resource swap MECH-A performed, when the edit substituted a font
    /// (P2.4 Stage 2 T10/T15). `None` for an ordinary Stage-1 edit. Reverted together
    /// with the bytes so an undo restores the original font as well as the text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font_swap: Option<FontSwap>,
}

/// One run's switch from its original font resource to a standard-14 substitute.
///
/// Recorded as the `Tf` operation that names the resource, so reverting sets the name
/// back exactly where it was read — no re-derivation, no guess about which `Tf` governed
/// the run.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FontSwap {
    /// Index of the governing `Tf` operation in the decoded content.
    pub op_index: usize,
    /// Path to the resource-name operand inside that `Tf` (always `[0]` for `Tf`).
    pub operand_path: Vec<usize>,
    /// Resource name in force before the edit (`F1`).
    pub old_name: Vec<u8>,
    /// Resource name the edit switched the run to (`FS1`).
    pub new_name: Vec<u8>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::testpdf;
    use lopdf::content::Content;

    #[test]
    fn locates_runs_in_content_order_with_decoded_text() {
        let document = testpdf::simple_winansi();
        let page = page_id(&document, 0).unwrap();
        let runs = locate_runs(&document, page).unwrap();

        let texts: Vec<&str> = runs.iter().map(|run| run.text.as_str()).collect();
        assert_eq!(
            texts,
            vec!["ALPHA", "Document Reader P1 fixture, page one."]
        );
        // Two separate `Tj` operations, each a one-string run. `BT`/`Tf`/`Td` precede
        // the first `Tj`, so its operation index is 3, not 0.
        assert_eq!(runs[0].id.op_index, 3);
        assert_eq!(runs[0].id.operand_path, vec![0]);
        assert_eq!(runs[0].font_key, b"F1");
        assert_eq!(runs[0].operator, "Tj");
    }

    #[test]
    fn reads_text_from_a_tj_array() {
        // `[(Kerned) -250 (text)] TJ` is one operation with two string runs inside a
        // nested array; a single-index address would name the array, not the string.
        let mut document = testpdf::blank_page();
        let page = page_id(&document, 0).unwrap();
        let content = Content {
            operations: vec![
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec!["F1".into(), 12.into()]),
                Operation::new(
                    "TJ",
                    vec![Object::Array(vec![
                        Object::string_literal("Kerned"),
                        Object::Integer(-250),
                        Object::string_literal("text"),
                    ])],
                ),
                Operation::new("ET", vec![]),
            ],
        };
        document
            .change_page_content(page, content.encode().unwrap())
            .unwrap();

        let runs = locate_runs(&document, page).unwrap();
        let texts: Vec<&str> = runs.iter().map(|run| run.text.as_str()).collect();
        assert_eq!(texts, vec!["Kerned", "text"]);
        assert_eq!(runs[0].id.operand_path, vec![0, 0]);
        assert_eq!(runs[1].id.operand_path, vec![0, 2]);
    }

    #[test]
    fn a_double_quote_run_takes_the_last_operand() {
        // `aw ac string "` — the string is the last operand. Reading the first would
        // return the word spacing number and try to edit it as text.
        let mut document = testpdf::blank_page();
        let page = page_id(&document, 0).unwrap();
        let content = Content {
            operations: vec![
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec!["F1".into(), 12.into()]),
                Operation::new(
                    "\"",
                    vec![1.into(), 2.into(), Object::string_literal("spaced")],
                ),
                Operation::new("ET", vec![]),
            ],
        };
        document
            .change_page_content(page, content.encode().unwrap())
            .unwrap();

        let runs = locate_runs(&document, page).unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].text, "spaced");
        assert_eq!(runs[0].id.operand_path, vec![2]);
    }

    #[test]
    fn a_run_with_no_font_in_force_is_not_offered() {
        let mut document = testpdf::blank_page();
        let page = page_id(&document, 0).unwrap();
        let content = Content {
            operations: vec![Operation::new("Tj", vec![Object::string_literal("orphan")])],
        };
        document
            .change_page_content(page, content.encode().unwrap())
            .unwrap();
        // No `Tf` before it, so the encoding (and therefore the gates) are unknowable.
        assert!(locate_runs(&document, page).unwrap().is_empty());
    }

    #[test]
    fn detects_subset_and_cid_fonts() {
        assert!(is_subset_font(Some("ABCDEF+Helvetica")));
        assert!(!is_subset_font(Some("Helvetica")));
        // Lowercase letters are not the six-uppercase prefix convention.
        assert!(!is_subset_font(Some("abcdef+Helvetica")));
        // A `+` that is not in the prefix position is part of the real name.
        assert!(!is_subset_font(Some("Helvetica+Neue")));
        assert!(!is_subset_font(None));

        let cid = FontInfo {
            key: b"F1".to_vec(),
            base_font: Some("Identity-H".to_string()),
            subtype: Some("Type0".to_string()),
        };
        assert!(is_cid_font(&cid));
        assert!(!is_simple_font(&cid));

        let simple = FontInfo {
            key: b"F1".to_vec(),
            base_font: Some("Helvetica".to_string()),
            subtype: Some("Type1".to_string()),
        };
        assert!(!is_cid_font(&simple));
        assert!(is_simple_font(&simple));
    }

    #[test]
    fn apply_text_replaces_only_the_named_run() {
        let mut document = testpdf::simple_winansi();
        let page = page_id(&document, 0).unwrap();
        let runs = locate_runs(&document, page).unwrap();
        let target = runs[0].clone();

        let page_obj = page_id(&document, 0).unwrap();
        let inverse =
            apply_text(&mut document, 0, page_obj, &target.id, "ALPHA", b"BRAVO").unwrap();

        assert_eq!(inverse.old_bytes, b"ALPHA");
        assert_eq!(inverse.new_bytes, b"BRAVO");

        // The edit is visible to a fresh parse, and the other run is untouched.
        let page = page_id(&document, 0).unwrap();
        let after: Vec<String> = locate_runs(&document, page)
            .unwrap()
            .into_iter()
            .map(|run| run.text)
            .collect();
        assert_eq!(after[0], "BRAVO");
        assert_eq!(after[1], "Document Reader P1 fixture, page one.");
    }

    #[test]
    fn apply_text_refuses_when_the_run_no_longer_matches() {
        // T4: the user's `expected` text disagrees with what is at that address, so
        // writing would land the replacement on different text.
        let mut document = testpdf::simple_winansi();
        let page = page_id(&document, 0).unwrap();
        let target = locate_runs(&document, page).unwrap()[0].clone();

        let page_obj = page_id(&document, 0).unwrap();
        let error = apply_text(
            &mut document,
            0,
            page_obj,
            &target.id,
            "NOT WHAT IS THERE",
            b"BRAVO",
        )
        .unwrap_err();
        assert_eq!(error.code(), "TEXT_RUN_MISMATCH");
    }

    #[test]
    fn apply_text_refuses_an_out_of_range_address() {
        let mut document = testpdf::simple_winansi();
        let page = page_id(&document, 0).unwrap();
        let bogus = RunId {
            op_index: 9_999,
            operand_path: vec![0],
        };
        let error = apply_text(&mut document, 0, page, &bogus, "x", b"y").unwrap_err();
        assert_eq!(error.code(), "TEXT_RUN_MISMATCH");
    }

    #[test]
    fn content_beyond_the_cap_is_content_too_large() {
        let document = testpdf::bomb();
        let page = page_id(&document, 0).unwrap();
        let error = locate_runs(&document, page).unwrap_err();
        assert_eq!(error.code(), "CONTENT_TOO_LARGE");
    }

    #[test]
    fn multi_stream_page_runs_are_located_across_the_seam() {
        // F2: `get_page_content` concatenates the page's streams, so runs from the
        // second stream are located exactly like runs from the first.
        let document = testpdf::multi_stream();
        let page = page_id(&document, 0).unwrap();
        let runs = locate_runs(&document, page).unwrap();
        let texts: Vec<&str> = runs.iter().map(|run| run.text.as_str()).collect();
        assert_eq!(texts, vec!["FIRST", "SECOND"]);
    }
}
