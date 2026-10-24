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

use std::collections::HashMap;
use lopdf::{content::Operation, Dictionary, Document, Object, ObjectId};

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
    /// Text-space origin `(x, y)` of this operand: the text matrix's translation when the
    /// string showed (P2.4.1 T17). This is the key run pairing uses, because it is
    /// segmentation-independent — two sides may split a line differently and still agree
    /// on where the ink starts. It is **not** a rendered box: only PDFium has those.
    pub origin: (f32, f32),
    /// The `Tf` size in force, in points. The merge tolerances are relative to this
    /// (T19) precisely because one page carries both 9pt body text and 24pt headings.
    pub font_size: f32,
    /// The advance this operand occupies, in points. For a standard-14 font it is the AFM
    /// width; for an embedded font, the exact `hmtx` width (P2.4.2 Phase A); only a font with
    /// no in-file metric at all falls back to an approximation (see [`fragment_advance`]).
    /// Merging uses it to measure the gap to the next fragment; the width gate uses PDFium's
    /// rect instead.
    pub advance: f32,
    /// The advance measured from the font's **own in-file metrics**, or `None` when this font
    /// offers none (P2.4.2 Phase A). Kept next to [`RawRun::advance`] because the width quota
    /// (T22) must be a real measurement, never the approximation — a run whose quota is not
    /// measurable refuses rather than trusting a box.
    pub advance_exact: Option<f32>,
    /// The style class of the font in force (AFM-style name from
    /// [`crate::editor::substitute::style_match`], e.g. `Times-Bold`).
    ///
    /// Two resources with the same class set the same text in the same face, so a seam
    /// between them is not a style change — it is a producer artifact. The Maastricht
    /// brochure switches `F4` (`ABCDEF+TimesNewRomanPS-BoldMT`) to `F5`
    /// (`ABCDEF+TimesNewRomanPSMT`) mid-line purely to change weight; treating the resource
    /// name as identity split one sentence into two pickable runs.
    pub style_class: String,
    /// The marked-content stack in force when this operand was shown, outermost first
    /// (P2.7.1). Empty on an untagged page — which is every predicate's behaviour in 0.4.5.
    pub marked_content: Vec<MarkedContent>,
    /// The text rise (`Ts`) in force, in points (P2.7.1 ruling B-5).
    ///
    /// [`Self::origin`] is the pen position; the ink sits `rise` points above it. Pairing and
    /// merging must compare the **ink** baseline, which is `origin.1 + rise`, or a superscript
    /// pairs to the box below the one it is drawn in.
    pub rise: f32,
    /// True when the text matrix was not axis-aligned (P2.7 hotfix H0).
    ///
    /// [`Self::advance`] is measured along the text axis. For a rotated run that axis is not
    /// horizontal, so the advance is not a width and the `fits-width` gate would compare it
    /// against a horizontal box. Such runs refuse until P2.7.4 measures transforms.
    pub rotated: bool,
}

impl RawRun {
    /// The baseline the run's **ink** sits on: the pen position plus `Ts` (P2.7.1 B-5).
    ///
    /// Every baseline comparison must use this rather than `origin.1`. A run with `Ts 5`
    /// draws 5pt above its pen, so comparing pen positions would place it on the wrong line.
    pub fn baseline(&self) -> f32 {
        self.origin.1 + self.rise
    }

    /// The innermost `MCID` this run sits in, if any.
    ///
    /// The *innermost* one is the boundary that matters: a paragraph (`MCID 5`) containing a
    /// span (`MCID`-less) is still one unit, and an adjacent cell's paragraph has its own
    /// `MCID`. Comparing whole stacks would refuse a merge whenever a producer emits an
    /// extra wrapper on one side only.
    pub fn innermost_mcid(&self) -> Option<i64> {
        self.marked_content
            .iter()
            .rev()
            .find_map(|entry| entry.mcid)
    }

    /// The unit whose boundary a merge must respect (P2.7.1 T27).
    ///
    /// Two things can bound a run, and only one of them is the document's intent:
    ///
    /// * a **block-level structure element**, when the `/ParentTree` resolves the `MCID`
    ///   (`StructureIndex` above). This is the paragraph, list item or table cell that owns the
    ///   text, and it is what a merge must not cross;
    /// * otherwise the **raw `MCID`**, which is finer than a block. Producers that give every
    ///   inline `Span` its own `MCID` land here only when their structure tree is missing or
    ///   unreadable, and the finer boundary is the safe one: it can refuse a merge that a
    ///   reader would allow, never allow one that crosses a cell.
    ///
    /// `None` means the text is outside any marked content, on a page with no tag at all —
    /// the 0.4.5 behaviour, unchanged (T28).
    pub fn merge_scope(&self) -> Option<MergeScope> {
        let innermost = self
            .marked_content
            .iter()
            .rev()
            .find(|entry| entry.mcid.is_some())?;
        Some(match innermost.block {
            Some(block) => MergeScope::Block(block),
            None => MergeScope::Untyped(innermost.mcid),
        })
    }
}

/// What a run's merge boundary actually is (see [`RawRun::merge_scope`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeScope {
    /// A resolved block-level structure element: the coarse, correct boundary.
    Block(ObjectId),
    /// A bare `MCID` whose structure element could not be resolved: the fine, conservative
    /// boundary.
    Untyped(Option<i64>),
}

/// One entry of the marked-content stack at a run's start (P2.7.1).
#[derive(Debug, Clone, PartialEq)]
pub struct MarkedContent {
    /// The tag (`Span`, `P`, `Artifact`, …). Recorded for diagnostics; **never** consulted
    /// for admissibility or merging (T27).
    pub tag: String,
    /// The `MCID` this sequence declared, if it declared one.
    pub mcid: Option<i64>,
    /// The decoded `/ActualText`, when the sequence carried one.
    pub actual_text: Option<String>,
    /// The nearest **block-level** structure element this `MCID` resolves to, when the
    /// document's structure tree names one (see [`StructureIndex`]).
    ///
    /// This, not the raw `MCID`, is the merge boundary. A producer may give every inline
    /// `Span` inside one paragraph its own `MCID` — the Maastricht brochure does, five of them
    /// across one sentence — so comparing `MCID`s would split a plain line of text into
    /// uneditable pieces. The block that owns those spans is one unit, and it is the unit a
    /// reader calls a paragraph.
    pub block: Option<ObjectId>,
}

/// The page's structural view of `MCID`s, used to tell a *paragraph* boundary from an
/// *inline span* boundary (P2.7.1 T27).
///
/// The marked-content stream says which content belongs to which `MCID`; only the structure
/// tree says what those `MCID`s mean. Resolution is page `/StructParents` → the root's
/// `/ParentTree` number tree → an array indexed by `MCID` → a structure element, then up
/// through `/P` to the nearest element that is not inline.
///
/// Failure at **any** step is `None` rather than an error: a document with no structure tree,
/// a malformed tree, or an out-of-range `MCID` falls back to the raw-`MCID` comparison, which
/// can only split more conservatively (T27/T31).
struct StructureIndex<'a> {
    document: &'a Document,
    /// The elements of this page's parent-tree entry, indexed by `MCID`.
    parents: Vec<Object>,
    /// Memoised `/P` walks, so a paragraph with 200 spans costs one traversal.
    blocks: HashMap<ObjectId, Option<ObjectId>>,
}

impl<'a> StructureIndex<'a> {
    fn new(document: &'a Document, page_id: ObjectId) -> Self {
        Self {
            document,
            parents: Self::read_parents(document, page_id),
            blocks: HashMap::new(),
        }
    }

    fn read_parents(document: &Document, page_id: ObjectId) -> Vec<Object> {
        let Some(page) = document
            .get_object(page_id)
            .ok()
            .and_then(|object| object.as_dict().ok())
        else {
            return Vec::new();
        };
        let Some(index) = page.get(b"StructParents").ok().and_then(|o| o.as_i64().ok()) else {
            return Vec::new();
        };
        let Some(root) = document
            .catalog()
            .ok()
            .and_then(|catalog| catalog.get(b"StructTreeRoot").ok().and_then(|root| document.dereference(root).ok().map(|(_, root)| root)))
            .and_then(|root| root.as_dict().ok())
        else {
            return Vec::new();
        };
        let Some(tree) = root.get(b"ParentTree").ok() else {
            return Vec::new();
        };
        number_tree_lookup(document, tree, index)
            .and_then(|entry| {
                // The entry is normally a reference to the per-page array; a page with one
                // `MCID` may inline it.
                document
                    .dereference(&entry)
                    .ok()
                    .map(|(_, resolved)| resolved.clone())
                    .or(Some(entry))
            })
            .map(|entry| match entry {
                Object::Array(items) => items,
                other => vec![other],
            })
            .unwrap_or_default()
    }

    /// The block-level element owning `mcid` on this page.
    fn block_for(&mut self, mcid: i64) -> Option<ObjectId> {
        let entry = self.parents.get(usize::try_from(mcid).ok()?)?;
        let leaf = referenced_element(self.document, entry)?;
        self.block_of(leaf)
    }

    /// Walks `/P` from a leaf element to the nearest element whose type is not inline.
    fn block_of(&mut self, element: ObjectId) -> Option<ObjectId> {
        if let Some(known) = self.blocks.get(&element) {
            return *known;
        }
        // Guard against a `/P` cycle in a malformed tree: a walk longer than the object
        // count cannot be a real ancestry chain.
        let mut current = Some(element);
        let mut answer = None;
        let mut steps = 0usize;
        while let Some(id) = current {
            steps += 1;
            if steps > self.document.objects.len() + 1 {
                break;
            }
            let Some(dict) = element_dict(self.document, id) else {
                break;
            };
            let role = dict
                .get(b"S")
                .ok()
                .and_then(|object| object.as_name().ok())
                .map(|name| String::from_utf8_lossy(name).into_owned());
            if role.as_deref().is_some_and(is_block_role) {
                answer = Some(id);
                break;
            }
            current = dict.get(b"P").ok().and_then(|parent| match parent {
                Object::Reference(parent) => Some(*parent),
                _ => None,
            });
        }
        self.blocks.insert(element, answer);
        answer
    }
}

/// Structure element roles that own a whole block of text.
///
/// Everything absent from this set is treated as inline (a `Span`, a `Link`, an `Artifact`,
/// or a role mapped through `/RoleMap`, which this walk deliberately does not follow: an
/// unknown role is inline, so the boundary falls back to a coarser one, never to a finer one).
fn is_block_role(role: &str) -> bool {
    matches!(
        role,
        "P" | "H" | "H1" | "H2" | "H3" | "H4" | "H5" | "H6"
            | "L" | "LI" | "LBody" | "Lbl"
            | "Table" | "TR" | "TH" | "TD" | "THead" | "TBody" | "TFoot"
            | "Figure" | "Caption" | "BlockQuote" | "Note" | "TOC" | "TOCI"
            | "Sect" | "Div" | "Art" | "Part" | "Index" | "NonStruct"
    )
}

fn element_dict(document: &Document, id: ObjectId) -> Option<&Dictionary> {
    document
        .get_object(id)
        .ok()
        .and_then(|object| object.as_dict().ok())
}

/// The structure element an `/ParentTree` entry refers to, accepting both the bare reference
/// modern producers write and the array wrapper the same producers use for multi-page keys.
fn referenced_element(document: &Document, entry: &Object) -> Option<ObjectId> {
    match entry {
        Object::Reference(id) => Some(*id),
        Object::Array(items) => items.iter().find_map(|item| match item {
            Object::Reference(id) => Some(*id),
            _ => None,
        }),
        _ => document
            .dereference(entry)
            .ok()
            .and_then(|(_, resolved)| resolved.as_array().ok())
            .and_then(|items| {
                items.iter().find_map(|item| match item {
                    Object::Reference(id) => Some(*id),
                    _ => None,
                })
            }),
    }
}

/// Reads one key out of a `/ParentTree`, which is a number tree (`/Nums` or `/Kids`).
fn number_tree_lookup(document: &Document, node: &Object, key: i64) -> Option<Object> {
    let (_, resolved) = document.dereference(node).ok()?;
    let dict = resolved.as_dict().ok()?;
    if let Ok(kids) = dict.get(b"Kids").and_then(|kids| kids.as_array()) {
        for kid in kids {
            let (_, kid) = document.dereference(kid).ok()?;
            let kid = kid.as_dict().ok()?;
            let limits = kid.get(b"Limits").ok()?.as_array().ok()?;
            let low = limits.first()?.as_i64().ok()?;
            let high = limits.get(1)?.as_i64().ok()?;
            if (low..=high).contains(&key) {
                return number_tree_lookup(document, &Object::Dictionary(kid.clone()), key);
            }
        }
        return None;
    }
    let nums = dict.get(b"Nums").ok()?.as_array().ok()?;
    for pair in nums.chunks_exact(2) {
        if pair[0].as_i64().ok()? == key {
            return Some(pair[1].clone());
        }
    }
    None
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
    // P2.4.1 T17: follow the text object through the whole page so each operand gets the
    // origin it was shown at. Only translation and size are tracked -- enough to pair a run
    // with the box that contains it; skew/rotation stay out of scope (Stage 3).
    let mut walker = TextMatrixWalk::new();
    // P2.7.1: marked content is tracked alongside the text state, in the same single pass.
    let mut marked = MarkedContentStack::new(document, page_id);

    for (op_index, operation) in content.operations.iter().enumerate() {
        walker.step(operation);
        marked.step(operation);

        let paths = text_operand_paths(operation);
        if paths.is_empty() {
            continue;
        }

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
        // P2.4.2 Phase A: the run's advance is measured from the font's own metrics when it has
        // any (AFM for standard-14, `hmtx` for an embedded program), so the merge gap is exact
        // rather than a per-character guess. The same source supplies the width quota (T22).
        let metrics = crate::editor::substitute::FontMetrics::resolve(document, page_id, &font_key);

        // Where each operand showed: an ordinary `Tj` shows at the current matrix origin,
        // but fragments inside one `TJ` array advance along the line as their kerning
        // numbers and glyph widths dictate (T17). Both are needed for merging: a per-glyph
        // `TJ` array is a sequence of adjacent fragments, and only the advancing pen tells
        // them apart from a word break.
        let per_path = tj_fragment_origins(operation, &walker, &metrics, &encoding);
        // T29: an `MCID` changes only at `BDC`/`EMC`, which cannot occur inside a `TJ` array.
        // The snapshot is therefore taken once per operation: every operand of one showing
        // operator belongs to the marked-content unit the operator started in.
        let marked_content = marked.snapshot();

        for path in paths {
            let Some(bytes) = string_at(operation, &path) else {
                continue;
            };
            let Ok(text) = encoding.bytes_to_string(bytes) else {
                continue;
            };
            if text.is_empty() {
                continue;
            }

            let origin = per_path
                .iter()
                .find(|(candidate, _)| **candidate == path)
                .map(|(_, origin)| *origin)
                .unwrap_or_else(|| walker.origin());

            // The exact in-file advance, or `None` when this font offers no metric. The
            // approximation exists only to keep the walk finite; `advance_exact` is what the
            // width quota trusts (T22).
            let advance_exact = metrics.advance(&text, walker.size);

            runs.push(RawRun {
                id: RunId {
                    op_index,
                    operand_path: path,
                },
                bytes: bytes.to_vec(),
                text: text.clone(),
                font_key: font_key.clone(),
                operator: operation.operator.clone(),
                origin,
                font_size: walker.size,
                advance: spaced_advance(&metrics, &text, &walker),
                advance_exact: advance_exact
                    .map(|width| spaced_width(width, &text, &walker)),
                style_class: crate::editor::substitute::style_match(
                    crate::editor::textlocate::font_info(document, page_id, &font_key)
                        .and_then(|info| info.base_font)
                        .as_deref(),
                ),
                marked_content: marked_content.clone(),
                rise: walker.rise(),
                rotated: walker.is_rotated(),
            });
        }
    }

    Ok(runs)
}

/// The origin of every string operand of one operation, advancing the pen across a `TJ`
/// array (P2.4.1 T17).
///
/// A `TJ` array is `[ (string) number (string) ... ]`: each number shifts the pen by
/// `-number/1000 x Tf` before the next string shows, and each string advances it by its own
/// width. Recovering that walk is what lets a per-glyph array be recognised as *adjacent*
/// fragments. For any other operator there is nothing to advance and the caller's matrix
/// origin is the answer.
fn tj_fragment_origins(
    operation: &Operation,
    state: &TextMatrixWalk,
    metrics: &crate::editor::substitute::FontMetrics,
    encoding: &lopdf::Encoding,
) -> Vec<(Vec<usize>, (f32, f32))> {
    if operation.operator != "TJ" {
        return Vec::new();
    }

    let mut origins = Vec::new();
    let mut pen_x = state.origin().0;
    for (index, operand) in operation.operands.iter().enumerate() {
        let Object::Array(items) = operand else {
            continue;
        };
        for (inner, item) in items.iter().enumerate() {
            match item {
                Object::String(bytes, _) => {
                    origins.push((vec![index, inner], (pen_x, state.origin().1)));
                    let width = encoding
                        .bytes_to_string(bytes)
                        .map(|text| spaced_advance(metrics, &text, state))
                        .unwrap_or(0.0);
                    pen_x += width;
                }
                // A kerning number is in thousandths of an em, subtracted from the pen.
                other => {
                    if let Ok(number) = other.as_float() {
                        pen_x -= number / 1000.0 * state.size;
                    }
                }
            }
        }
    }
    origins
}

/// The width a fragment occupies, used to walk *within* a `TJ` array and to measure the gap
/// between two fragments.
///
/// A standard-14 font answers exactly from the AFM tables. An embedded font answers from its
/// own `hmtx` via [`crate::editor::substitute::FontMetrics`] (P2.4.2 Phase A) — the same in-file
/// metrics the width quota uses, so a chain is never broken by a fabricated width. Only a font
/// with **no** in-file metric at all falls back to the per-character approximation; that number
/// then decides nothing but whether two fragments sit shoulder to shoulder on one line, and the
/// width gate -- the thing correctness depends on -- uses the run's measured span, never this.
pub fn fragment_advance(
    metrics: &crate::editor::substitute::FontMetrics,
    text: &str,
    font_size: f32,
) -> f32 {
    metrics
        .advance(text, font_size)
        .unwrap_or_else(|| text.chars().count() as f32 * 0.5 * font_size)
}

/// A fragment's horizontal advance including the text state's spacing and scaling (P2.7.1).
///
/// ISO 32000-1 9.4.4: each glyph moves the pen by
/// `(w0/1000 × Tf + Tc + Tw_if_space) × Tz/100`. `fragment_advance` gives the `w0` term;
/// this adds the other two. Before P2.7.1 they were ignored, so a metrics-less font with
/// character spacing drifted by exactly `Tc` per glyph — the same class of defect the T24
/// grid walk was built to survive, reintroduced on the fallback path.
fn spaced_advance(
    metrics: &crate::editor::substitute::FontMetrics,
    text: &str,
    state: &TextMatrixWalk,
) -> f32 {
    let Some(width) = metrics.advance(text, state.size) else {
        return fragment_advance(metrics, text, state.size) * state.horizontal_scale / 100.0;
    };
    spaced_width(width, text, state)
}

/// The spacing terms alone, applied to an already-measured glyph width.
///
/// `Tw` counts single-byte code 32; a multi-byte code that happens to *decode* to a space is
/// not word-spaced (ISO 32000-1 9.4.4), which is why this counts the bytes rather than the
/// characters.
fn spaced_width(width: f32, text: &str, state: &TextMatrixWalk) -> f32 {
    let glyphs = text.chars().count() as f32;
    let spaces = text.bytes().filter(|byte| *byte == b' ').count() as f32;
    let spacing = state.char_spacing * glyphs + state.word_spacing * spaces;
    (width + spacing) * state.horizontal_scale / 100.0
}

/// The identity text matrix `[a b c d e f]`.
const IDENTITY: [f32; 6] = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

/// Multiplies two text matrices, `m x n`, in the PDF convention (ISO 32000-1 9.4.2).
///
/// The only components this walker needs downstream are the translation terms (`e`, `f`),
/// but a full multiply keeps `Td`/`T*` correct even when a preceding `Tm` scaled or rotated
/// the frame: `e' = tx.a + ty.c + e`, not merely `tx + e`.
fn matrix_mul(m: [f32; 6], n: [f32; 6]) -> [f32; 6] {
    [
        m[0] * n[0] + m[1] * n[2],
        m[0] * n[1] + m[1] * n[3],
        m[2] * n[0] + m[3] * n[2],
        m[2] * n[1] + m[3] * n[3],
        m[4] * n[0] + m[5] * n[2] + n[4],
        m[4] * n[1] + m[5] * n[3] + n[5],
    ]
}

/// A translation `[1 0 0 1 tx ty]`, the matrix `Td`/`TD`/`T*` prepend.
fn translation(tx: f32, ty: f32) -> [f32; 6] {
    [1.0, 0.0, 0.0, 1.0, tx, ty]
}

/// Reads the six numbers of a `Tm` operand list, if it has them.
fn matrix_operands(operands: &[Object]) -> Option<[f32; 6]> {
    if operands.len() < 6 {
        return None;
    }
    let mut m = [0.0_f32; 6];
    for (slot, operand) in m.iter_mut().zip(operands) {
        *slot = operand.as_float().ok()?;
    }
    Some(m)
}

/// Reads a `Td`/`TD` operand pair.
fn translate_operands(operands: &[Object]) -> Option<(f32, f32)> {
    if operands.len() < 2 {
        return None;
    }
    Some((operands[0].as_float().ok()?, operands[1].as_float().ok()?))
}

/// The text state a run's origin is read from (P2.4.1 #3 step 1).
///
/// A deliberately *partial* implementation of the PDF text machinery: `BT`, `Tm`, `Td`,
/// `TD`, `T*`, `TL` and `Tf` size. It tracks the text matrix's translation and the font
/// size, which is everything pairing and the font-size-relative merge tolerances need.
/// `q`/`Q` graphics-state save/restore and CTM concatenation are **not** modelled -- the
/// same honest boundary Stage 1 drew for `font_in_force`.
#[derive(Debug, Clone)]
struct TextMatrixWalk {
    tm: [f32; 6],
    tlm: [f32; 6],
    leading: f32,
    size: f32,
    /// `Tc`: extra space added after every glyph, in unscaled text units (P2.7.1).
    char_spacing: f32,
    /// `Tw`: extra space added after every single-byte code 32 (P2.7.1).
    word_spacing: f32,
    /// `Tz`: horizontal scaling as a percentage, 100 = unscaled (P2.7.1).
    horizontal_scale: f32,
    /// `Ts`: baseline rise in points. Moves the ink, never the pen (P2.7.1 B-5).
    rise: f32,
}

impl TextMatrixWalk {
    fn new() -> Self {
        TextMatrixWalk {
            tm: IDENTITY,
            tlm: IDENTITY,
            leading: 0.0,
            size: 0.0,
            char_spacing: 0.0,
            word_spacing: 0.0,
            horizontal_scale: 100.0,
            rise: 0.0,
        }
    }

    /// Advances the state by one operation, before that operation's own text is read.
    ///
    /// A text-showing operator is a no-op here on purpose: `Tj` does not move the origin the
    /// *next* operand shows at, because the next operand re-sets it with `Td`/`Tm` (or the
    /// previous line's leading with `T*`). Advancing on the shown glyph widths would be an
    /// advance-width computation this walker does not own.
    fn step(&mut self, operation: &Operation) {
        match operation.operator.as_str() {
            "BT" => {
                self.tm = IDENTITY;
                self.tlm = IDENTITY;
                // ISO 32000-1 9.4.1: `BT` resets the text state parameters it can.
                self.char_spacing = 0.0;
                self.word_spacing = 0.0;
                self.horizontal_scale = 100.0;
                self.rise = 0.0;
            }
            "Tm" => {
                if let Some(matrix) = matrix_operands(&operation.operands) {
                    self.tm = matrix;
                    self.tlm = matrix;
                }
            }
            "Td" => {
                if let Some((tx, ty)) = translate_operands(&operation.operands) {
                    self.tlm = matrix_mul(translation(tx, ty), self.tlm);
                    self.tm = self.tlm;
                }
            }
            "TD" => {
                if let Some((tx, ty)) = translate_operands(&operation.operands) {
                    // `TD` sets the leading to `-ty` as well as translating (9.4.3).
                    self.leading = -ty;
                    self.tlm = matrix_mul(translation(tx, ty), self.tlm);
                    self.tm = self.tlm;
                }
            }
            "T*" => {
                self.tlm = matrix_mul(translation(0.0, -self.leading), self.tlm);
                self.tm = self.tlm;
            }
            "TL" => {
                if let Some(operand) = operation.operands.first() {
                    if let Ok(value) = operand.as_float() {
                        self.leading = value;
                    }
                }
            }
            "Tf" => {
                if let Some(operand) = operation.operands.get(1) {
                    if let Ok(value) = operand.as_float() {
                        self.size = value;
                    }
                }
            }
            "Tc" => {
                if let Some(value) = operation.operands.first().and_then(|o| o.as_float().ok()) {
                    self.char_spacing = value;
                }
            }
            "Tw" => {
                if let Some(value) = operation.operands.first().and_then(|o| o.as_float().ok()) {
                    self.word_spacing = value;
                }
            }
            "Tz" => {
                if let Some(value) = operation.operands.first().and_then(|o| o.as_float().ok()) {
                    self.horizontal_scale = value;
                }
            }
            "Ts" => {
                if let Some(value) = operation.operands.first().and_then(|o| o.as_float().ok()) {
                    self.rise = value;
                }
            }
            // `'` and `"` both perform a `T*` (move to the next line) before showing.
            "'" | "\"" => {
                self.tlm = matrix_mul(translation(0.0, -self.leading), self.tlm);
                self.tm = self.tlm;
            }
            _ => {}
        }
    }

    /// The translation component of the current text matrix.
    fn origin(&self) -> (f32, f32) {
        (self.tm[4], self.tm[5])
    }

    /// The text rise (`Ts`) in force, in points.
    ///
    /// `Ts` moves the *baseline* of the drawn glyphs without moving the pen, so it must never
    /// enter [`Self::origin`] or any advance computation. It is recorded separately and added
    /// by the run so pairing can compare the line the ink actually sits on (P2.7.1 ruling B-5).
    fn rise(&self) -> f32 {
        self.rise
    }

    /// True when the text matrix is not axis-aligned (P2.7 hotfix H0).
    ///
    /// The matrix maps text space to user space, so its first column `(a, b)` is where one
    /// unit of text advance points. An unrotated, unskewed run has `b == 0` (and `c == 0`);
    /// a run rotated by 90° has `(0, 1)`. `advance` is measured along the *text* axis, so for
    /// a rotated run it is not a horizontal width, and `fits-width` would compare it against
    /// a horizontal box. Until transforms are measured properly (P2.7.4) such runs must
    /// refuse rather than be measured on the wrong axis.
    fn is_rotated(&self) -> bool {
        const EPSILON: f32 = 1e-4;
        self.tm[1].abs() > EPSILON || self.tm[2].abs() > EPSILON
    }
}

/// Tracks the `BDC`/`BMC`/`EMC` stack across a page's operations (P2.7.1).
///
/// The stack is what turns "two fragments look adjacent" into "two fragments are in the same
/// tagged unit". It is deliberately a *depth* machine, not a structure-tree reader: a
/// producer's malformed sequence must not make a page uneditable (T31), and the tag names are
/// never consulted (T27).
struct MarkedContentStack<'a> {
    document: &'a Document,
    /// `None` for a `BMC` (no properties dict, no `MCID`) so the depth is still balanced.
    stack: Vec<Option<MarkedContent>>,
    /// The page's `/Properties` resource dictionary, for `BDC` with a name operand.
    properties: Option<&'a Dictionary>,
    /// Resolves each `MCID` to its owning block, where the structure tree allows it.
    structure: StructureIndex<'a>,
}

impl<'a> MarkedContentStack<'a> {
    fn new(document: &'a Document, page_id: ObjectId) -> Self {
        let properties = properties_dict(document, page_id);
        Self {
            document,
            stack: Vec::new(),
            properties,
            structure: StructureIndex::new(document, page_id),
        }
    }

    fn step(&mut self, operation: &Operation) {
        match operation.operator.as_str() {
            "BDC" => {
                let entry = self.bdc_entry(operation);
                self.stack.push(entry);
            }
            // `BMC` has no properties: it only affects depth.
            "BMC" => self.stack.push(None),
            // T31: an `EMC` at depth 0 is ignored rather than failing the page.
            "EMC" => {
                self.stack.pop();
            }
            _ => {}
        }
    }

    /// The entries in force, outermost first. `None` slots (a `BMC`) contribute nothing.
    fn snapshot(&self) -> Vec<MarkedContent> {
        self.stack.iter().flatten().cloned().collect()
    }

    fn bdc_entry(&mut self, operation: &Operation) -> Option<MarkedContent> {
        let tag = operation
            .operands
            .first()
            .and_then(|operand| operand.as_name().ok())
            .map(|name| String::from_utf8_lossy(name).into_owned())
            .unwrap_or_default();
        let properties = self.resolve_properties(operation.operands.get(1))?;
        let mcid = properties
            .get(b"MCID")
            .ok()
            .and_then(|object| object.as_i64().ok());
        let actual_text = properties
            .get(b"ActualText")
            .ok()
            .and_then(|object| object.as_str().ok())
            .and_then(decode_text_string);
        let block = mcid.and_then(|mcid| self.structure.block_for(mcid));
        Some(MarkedContent {
            tag,
            mcid,
            actual_text,
            block,
        })
    }

    /// `BDC` operands are `(tag, properties)`; properties is either an inline dictionary or a
    /// name resolving through `/Properties` in the page resources.
    fn resolve_properties(&self, operand: Option<&'a Object>) -> Option<&'a Dictionary> {
        match operand? {
            Object::Dictionary(dict) => Some(dict),
            Object::Name(name) => self
                .properties?
                .get(name)
                .ok()
                .and_then(|object| self.document.dereference(object).ok())
                .map(|(_, object)| object)
                .and_then(|object| object.as_dict().ok()),
            _ => None,
        }
    }
}

/// Decodes a PDF text string as a marked-content property value.
///
/// `ActualText` is required to be UTF-16BE with a `FE FF` byte-order mark when it carries
/// non-ASCII; producers that ignore that write the text in PDFDocEncoding. Both are decoded
/// here, and an undecodable string is treated as absent — a value we cannot read is one we
/// must not rewrite (T30, ruling B-6).
fn decode_text_string(bytes: &[u8]) -> Option<String> {
    if bytes.len() >= 2 && bytes[0] == 0xFE && bytes[1] == 0xFF {
        let body = &bytes[2..];
        if body.len() % 2 != 0 {
            // A trailing odd byte means the string is not valid UTF-16. Dropping it would
            // read a different string than the one on the page.
            return None;
        }
        let units: Vec<u16> = body
            .chunks_exact(2)
            .map(|pair| u16::from_be_bytes([pair[0], pair[1]]))
            .collect();
        return String::from_utf16(&units).ok();
    }
    bytes.iter().map(|byte| pdf_doc_encoding_char(*byte)).collect()
}

/// The character a byte stands for in PDFDocEncoding (ISO 32000-1 Annex D, table D.2).
///
/// Latin-1 is **not** PDFDocEncoding: codes `0x18`–`0x1F` are spacing accents, `0x80`–`0x9E`
/// are typographic punctuation, and `0xA0` is the euro sign rather than a no-break space.
/// Reading them as Latin-1 would let a rewrite write back a different character than it read.
///
/// `None` for the three codes the standard leaves undefined (`0x7F`, `0x9F`, `0xAD`): a
/// string containing one cannot be decoded, so it cannot be preserved either.
fn pdf_doc_encoding_char(byte: u8) -> Option<char> {
    const LOW: [char; 8] = ['\u{02d8}', '\u{02c7}', '\u{02c6}', '\u{02d9}', '\u{02dd}', '\u{02db}', '\u{02da}', '\u{02dc}'];
    const HIGH: [char; 31] = [
        '\u{2022}', '\u{2020}', '\u{2021}', '\u{2026}', '\u{2014}', '\u{2013}', '\u{0192}', '\u{2044}',
        '\u{2039}', '\u{203a}', '\u{2212}', '\u{2030}', '\u{201e}', '\u{201c}', '\u{201d}', '\u{2018}',
        '\u{2019}', '\u{201a}', '\u{2122}', '\u{fb01}', '\u{fb02}', '\u{0141}', '\u{0152}', '\u{0160}',
        '\u{0178}', '\u{017d}', '\u{0131}', '\u{0142}', '\u{0153}', '\u{0161}', '\u{017e}',
    ];
    match byte {
        0x18..=0x1f => Some(LOW[(byte - 0x18) as usize]),
        0x80..=0x9e => Some(HIGH[(byte - 0x80) as usize]),
        0xa0 => Some('\u{20ac}'),
        0x7f | 0x9f | 0xad => None,
        _ => Some(byte as char),
    }
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
        also: Vec::new(),
        property_clone: None,
    })
}

/// One fragment's share of a merged write (P2.4.1 T18/T20).
pub struct FragmentWrite {
    /// The fragment's content-stream address.
    pub id: RunId,
    /// The text this fragment reads right now, re-validated before it is touched (T4).
    pub expected: String,
    /// The bytes to install. The head fragment carries the whole replacement; every
    /// following fragment is cleared to empty so the run is shown once, not N times.
    pub new_bytes: Vec<u8>,
}

/// A merged run's complete write: its fragments, and the font swap MECH-A decided on.
pub struct MergedWrite<'a> {
    /// The fragments, head first, in the order the picker presented them.
    pub fragments: Vec<FragmentWrite>,
    /// The standard-14 `/BaseFont` to switch the run to (MECH-A), or `None` to keep the run's
    /// own font. One substitution per merged run, exactly as Stage 2 does for a single run.
    pub substitute: Option<&'a str>,
    /// The text the user asked for, when there is one. Only `/ActualText` consumes it (P2.7.1):
    /// the property must state the *semantic* text, which the operand bytes are not.
    pub replacement: Option<&'a str>,
}

/// Applies a merged run's edit in **one** decode -> mutate -> encode -> replace pass
/// (P2.4.1 T18/T19/T20).
///
/// One pass is the whole point. Writing each fragment as its own edit would re-encode the
/// page N times, and a failure between passes would leave a document holding the replacement
/// *and* the fragments it replaced — the text counted twice. The font swap rides along in the
/// same pass for the same reason: bytes and typeface must change together or not at all.
///
/// The `TJ` positioning numbers between the operands are deliberately untouched (T19): the pen
/// advances that placed the glyphs do not depend on how many of them still carry text.
///
/// The inverse restores each fragment's original bytes to its own operand path and puts the
/// original font name back, so one merged edit is one undo entry (T18).
pub fn apply_merged(
    document: &mut Document,
    page_index: u32,
    page_id: ObjectId,
    write: &MergedWrite<'_>,
) -> Result<(ContentOperation, bool), AppError> {
    let head = write.fragments.first().ok_or(AppError::TextRunMismatch)?;
    let mut content = decoded_content(document, page_id)?;

    // Validate and collect every original string before mutating anything, so a mismatch on
    // the last fragment leaves the page untouched rather than half rewritten.
    let mut originals = Vec::with_capacity(write.fragments.len());
    {
        let fonts = document
            .get_page_fonts(page_id)
            .map_err(|error| AppError::Other(format!("could not read page fonts: {error}")))?;
        for fragment in &write.fragments {
            let operation = content
                .operations
                .get(fragment.id.op_index)
                .ok_or(AppError::TextRunMismatch)?;
            let old = string_at(operation, &fragment.id.operand_path)
                .ok_or(AppError::TextRunMismatch)?
                .to_vec();
            // T4, per fragment: the bytes at this address must still read as what the user was
            // shown. A merged run is re-validated fragment by fragment, so a document that
            // changed under the picker refuses instead of writing the wrong operand.
            let font_key = font_in_force(&content.operations, fragment.id.op_index)
                .ok_or(AppError::TextRunMismatch)?;
            let font = fonts
                .get(&font_key)
                .copied()
                .ok_or(AppError::TextRunMismatch)?;
            let encoding = font
                .get_font_encoding(document)
                .map_err(|_| AppError::TextRunMismatch)?;
            let current = encoding
                .bytes_to_string(&old)
                .map_err(|_| AppError::TextRunMismatch)?;
            if current != fragment.expected {
                return Err(AppError::TextRunMismatch);
            }
            originals.push(old);
        }
    }

    for fragment in &write.fragments {
        let operation = content
            .operations
            .get_mut(fragment.id.op_index)
            .ok_or(AppError::TextRunMismatch)?;
        let slot =
            string_at_mut(operation, &fragment.id.operand_path).ok_or(AppError::TextRunMismatch)?;
        *slot = fragment.new_bytes.clone();
    }

    // The font swap, in the same pass. Every governing `Tf` the merged fragments read through
    // is switched (a document may repeat `Tf F1 12` between fragments); each swap is recorded
    // so undo restores the original name exactly where it was read.
    let mut swaps: Vec<FontSwap> = Vec::new();
    if let Some(base_font) = write.substitute {
        ensure_substitute_resource(document, page_id, base_font)?;
        let mut seen: Vec<usize> = Vec::new();
        for fragment in &write.fragments {
            let (tf_index, old_name) =
                font_in_force_with_index(&content.operations, fragment.id.op_index)
                    .ok_or(AppError::TextRunMismatch)?;
            if seen.contains(&tf_index) {
                continue;
            }
            seen.push(tf_index);
            let operation = content
                .operations
                .get_mut(tf_index)
                .ok_or(AppError::TextRunMismatch)?;
            let slot = operation
                .operands
                .first_mut()
                .ok_or(AppError::TextRunMismatch)?;
            *slot = Object::Name(SUBSTITUTE_NAME.to_vec());
            swaps.push(FontSwap {
                op_index: tf_index,
                operand_path: vec![0],
                old_name,
                new_name: SUBSTITUTE_NAME.to_vec(),
                also: Vec::new(),
            });
        }
    }

    // P2.7.1 T30: a run inside `/Span <</ActualText …>>` has semantic text that must not be
    // left reading the old string. The rewrite rides in this same pass, so one undo restores
    // both the operand and the property. `false` means the span is shared or the properties
    // are a shared resource: the edit lands, but the caller must tell the user.
    let named = page_properties(document, page_id);
    let (actual_text_preserved, property_clone) = rewrite_actual_text(
        document,
        page_id,
        &mut content.operations,
        head.id.op_index,
        write.replacement,
        &named,
    )?;

    let encoded = content
        .encode()
        .map_err(|error| AppError::Other(format!("could not re-encode page content: {error}")))?;

    // F2: the whole page becomes one stream (D2 keeps the original file's layout).
    document
        .change_page_content(page_id, encoded)
        .map_err(|error| AppError::Other(format!("could not replace page content: {error}")))?;

    let also: Vec<ContentOperation> = write
        .fragments
        .iter()
        .zip(originals.iter())
        .skip(1)
        .map(|(fragment, old)| ContentOperation {
            page: page_index,
            op_index: fragment.id.op_index,
            operand_path: fragment.id.operand_path.clone(),
            old_bytes: old.clone(),
            new_bytes: fragment.new_bytes.clone(),
            font_swap: None,
            also: Vec::new(),
            property_clone: None,
        })
        .collect();

    let font_swap = swaps.split_first().map(|(first, rest)| FontSwap {
        op_index: first.op_index,
        operand_path: first.operand_path.clone(),
        old_name: first.old_name.clone(),
        new_name: first.new_name.clone(),
        also: rest
            .iter()
            .map(|swap| FontSwap {
                op_index: swap.op_index,
                operand_path: swap.operand_path.clone(),
                old_name: swap.old_name.clone(),
                new_name: swap.new_name.clone(),
                also: Vec::new(),
            })
            .collect(),
    });

    Ok((
        ContentOperation {
            page: page_index,
            op_index: head.id.op_index,
            operand_path: head.id.operand_path.clone(),
            old_bytes: originals[0].clone(),
            new_bytes: head.new_bytes.clone(),
            font_swap,
            also,
            property_clone,
        },
        actual_text_preserved,
    ))
}

/// Rewrites the `/ActualText` of the innermost `BDC` governing one showing operation
/// (P2.7.1 T30).
///
/// Returns `true` when there was nothing to update or the update happened; `false` when an
/// `ActualText` exists but cannot be safely rewritten. Three cases are refused rather than
/// guessed at, because each would silently change text the user did not edit:
///
/// - the enclosing `BDC` holds another showing operation (the span's text is the concatenation
///   of both, so the replacement is only part of it);
/// - the existing `ActualText` does not decode (ruling B-6): rewriting text we could not read
///   would silently replace an unknown value;
/// - the new bytes are not decodable text, so there is nothing meaningful to write.
///
/// A **named** `/Properties` entry is rewritten by copy-on-write (ruling B-7): the entry is
/// cloned under a fresh name, this `BDC` is repointed at the clone, and only the clone is
/// edited. Nothing shared is mutated, so every other sequence naming the original keeps its
/// own `ActualText`. The clone is reported so undo can remove it again.
///
/// A malformed stream (no `EMC` for a `BDC`) simply leaves the scan at depth > 0; the span is
/// then treated as open to the end of the stream, which is the conservative reading.
fn rewrite_actual_text(
    document: &mut Document,
    page_id: ObjectId,
    operations: &mut [Operation],
    op_index: usize,
    replacement: Option<&str>,
    named: &HashMap<Vec<u8>, Dictionary>,
) -> Result<(bool, Option<PropertyClone>), AppError> {
    let Some(bdc_index) = enclosing_bdc(operations, op_index) else {
        return Ok((true, None));
    };
    let operation = operations
        .get(bdc_index)
        .ok_or(AppError::TextRunMismatch)?;

    // A name operand resolves through `/Properties`. It cannot be mutated in place without
    // knowing whether another sequence shares it, but it must still be *read*: a named bundle
    // with no `ActualText` is the common case and must not produce a warning.
    let named_entry = match operation.operands.get(1) {
        Some(Object::Dictionary(_)) => None,
        Some(Object::Name(name)) => Some(name.clone()),
        _ => return Ok((true, None)),
    };
    let properties = match &named_entry {
        Some(name) => named.get(name).ok_or(AppError::TextRunMismatch)?,
        None => match operation.operands.get(1) {
            Some(Object::Dictionary(properties)) => properties,
            _ => return Ok((true, None)),
        },
    };
    if properties.get(b"ActualText").is_err() {
        return Ok((true, None));
    }
    // Ruling B-6: we may only claim preservation for text we could actually read. An
    // `ActualText` that does not decode (an undefined PDFDocEncoding code, a malformed UTF-16
    // run, or a value that is not a string) is text we never understood, so overwriting it
    // would destroy a value we cannot reproduce.
    let existing = properties
        .get(b"ActualText")
        .ok()
        .and_then(|object| object.as_str().ok());
    if existing.and_then(decode_text_string).is_none() {
        return Ok((false, None));
    }
    if !span_is_this_operation(operations, bdc_index, op_index) {
        return Ok((false, None));
    }

    // Without the replacement string there is no semantic text to write. `MergedWrite` always
    // supplies it for an edit; a caller that does not is treated as "cannot preserve".
    let Some(text) = replacement else {
        return Ok((false, None));
    };
    let mut encoded = vec![0xFE, 0xFF];
    encoded.extend(text.encode_utf16().flat_map(|unit| unit.to_be_bytes()));

    let Some(original_name) = named_entry else {
        let operation = operations
            .get_mut(bdc_index)
            .ok_or(AppError::TextRunMismatch)?;
        if let Some(Object::Dictionary(properties)) = operation.operands.get_mut(1) {
            properties.set(
                "ActualText",
                Object::String(encoded, lopdf::StringFormat::Literal),
            );
        }
        return Ok((true, None));
    };

    let source = properties.clone();
    let Some(clone) = clone_named_property(
        document,
        page_id,
        bdc_index,
        &original_name,
        &source,
        encoded,
    ) else {
        // B-7: no clone, no rewrite. Disclose rather than mutate the shared entry.
        return Ok((false, None));
    };
    let operation = operations
        .get_mut(bdc_index)
        .ok_or(AppError::TextRunMismatch)?;
    *operation
        .operands
        .get_mut(1)
        .ok_or(AppError::TextRunMismatch)? = Object::Name(clone.new_name.clone());
    Ok((true, Some(clone)))
}

/// A named `/Properties` entry cloned for one edit (ruling B-7), recorded so undo can take
/// the clone back out again.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PropertyClone {
    /// Index of the `BDC` whose properties operand now names the clone.
    pub op_index: usize,
    /// The name the `BDC` used before the edit.
    pub old_name: Vec<u8>,
    /// The fresh name this edit created and pointed the `BDC` at.
    pub new_name: Vec<u8>,
    /// The clone object itself, removed on undo so the file does not accumulate orphans.
    pub clone_object: ObjectId,
}

/// Clones a named `/Properties` entry under a fresh name, installs it in the page's
/// `/Properties` dictionary and stores the new `ActualText` in the clone. Returns `None` when
/// the clone cannot be made, which the caller reports as "not preserved" (B-7).
fn clone_named_property(
    document: &mut Document,
    page_id: ObjectId,
    bdc_index: usize,
    original_name: &[u8],
    source: &Dictionary,
    actual_text: Vec<u8>,
) -> Option<PropertyClone> {
    let mut clone = source.clone();
    clone.set(
        "ActualText",
        Object::String(actual_text, lopdf::StringFormat::Literal),
    );
    let new_name = {
        let properties = properties_mut(document, page_id)?;
        fresh_property_name(properties, original_name)?
    };
    let clone_object = document.add_object(clone);
    properties_mut(document, page_id)?.set(new_name.clone(), Object::Reference(clone_object));
    Some(PropertyClone {
        op_index: bdc_index,
        old_name: original_name.to_vec(),
        new_name,
        clone_object,
    })
}

/// `<base>_edit<N>` for the smallest `N` not already taken in `properties`.
fn fresh_property_name(properties: &Dictionary, base: &[u8]) -> Option<Vec<u8>> {
    (1..=999).find_map(|suffix| {
        let mut candidate = base.to_vec();
        candidate.extend_from_slice(b"_edit");
        candidate.extend_from_slice(suffix.to_string().as_bytes());
        (!properties.has(&candidate)).then_some(candidate)
    })
}

/// Where a page's `/Properties` dictionary actually lives.
///
/// `/Properties` can hang off an inline `/Resources` dict, off an indirect `/Resources` object
/// (what real producers write), or be an indirect object itself; `/Resources` may also be
/// inherited from the page tree. `Document::get_page_resources` reports the inline dictionary
/// and the referenced ones separately, so both have to be considered.
enum PropertiesSlot {
    /// `/Properties` is inline inside the page's inline `/Resources`.
    Page,
    /// `/Properties` is inline inside the resources object `id` (possibly an inherited one).
    Resources(ObjectId),
    /// `/Properties` is itself an indirect object.
    Object(ObjectId),
}

fn properties_slot(document: &Document, page_id: ObjectId) -> Option<PropertiesSlot> {
    let page = document.get_dictionary(page_id).ok()?;
    if let Some(resources) = page.get(b"Resources").ok().and_then(|r| r.as_dict().ok()) {
        match resources.get(b"Properties").ok()? {
            Object::Reference(properties) => return Some(PropertiesSlot::Object(*properties)),
            Object::Dictionary(_) => return Some(PropertiesSlot::Page),
            _ => return None,
        }
    }
    let (_, resource_ids) = document.get_page_resources(page_id).ok()?;
    resource_ids.iter().find_map(|id| {
        let resources = document.get_dictionary(*id).ok()?;
        match resources.get(b"Properties").ok()? {
            Object::Reference(properties) => Some(PropertiesSlot::Object(*properties)),
            Object::Dictionary(_) => Some(PropertiesSlot::Resources(*id)),
            _ => None,
        }
    })
}

/// The page's `/Properties` dictionary, resolved the same way the write path resolves it.
fn properties_dict(document: &Document, page_id: ObjectId) -> Option<&Dictionary> {
    match properties_slot(document, page_id)? {
        PropertiesSlot::Object(id) => document.get_dictionary(id).ok(),
        PropertiesSlot::Resources(id) => document
            .get_dictionary(id)
            .ok()?
            .get(b"Properties")
            .ok()?
            .as_dict()
            .ok(),
        PropertiesSlot::Page => document
            .get_dictionary(page_id)
            .ok()?
            .get(b"Resources")
            .ok()?
            .as_dict()
            .ok()?
            .get(b"Properties")
            .ok()?
            .as_dict()
            .ok(),
    }
}

/// The page's `/Properties` entries, keyed by name, so the write path can read a named
/// `ActualText` without borrowing the document it is about to mutate.
fn page_properties(document: &Document, page_id: ObjectId) -> HashMap<Vec<u8>, Dictionary> {
    properties_dict(document, page_id)
        .map(|properties| {
            properties
                .iter()
                .filter_map(|(name, value)| {
                    document
                        .dereference(value)
                        .ok()
                        .and_then(|(_, resolved)| resolved.as_dict().ok())
                        .map(|dict| (name.clone(), dict.clone()))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Mutable access to the page's `/Properties` dictionary.
///
/// Adding a key here is safe even when pages share one `/Resources` object: the write path only
/// *adds* a clone (ruling B-7) and never mutates an entry a shared reader can see.
pub(crate) fn properties_mut(
    document: &mut Document,
    page_id: ObjectId,
) -> Option<&mut Dictionary> {
    match properties_slot(document, page_id)? {
        PropertiesSlot::Object(id) => document.get_dictionary_mut(id).ok(),
        PropertiesSlot::Resources(id) => document
            .get_dictionary_mut(id)
            .ok()?
            .get_mut(b"Properties")
            .ok()?
            .as_dict_mut()
            .ok(),
        PropertiesSlot::Page => document
            .get_dictionary_mut(page_id)
            .ok()?
            .get_mut(b"Resources")
            .ok()?
            .as_dict_mut()
            .ok()?
            .get_mut(b"Properties")
            .ok()?
            .as_dict_mut()
            .ok(),
    }
}

/// The index of the `BDC` that opened the marked-content sequence containing `op_index`.
fn enclosing_bdc(operations: &[Operation], op_index: usize) -> Option<usize> {
    let mut stack: Vec<usize> = Vec::new();
    for (index, operation) in operations.iter().enumerate().take(op_index) {
        match operation.operator.as_str() {
            "BDC" => stack.push(index),
            "BMC" => stack.push(usize::MAX),
            "EMC" => {
                stack.pop();
            }
            _ => {}
        }
    }
    stack
        .into_iter()
        .rev()
        .find(|index| *index != usize::MAX)
}

/// True when `op_index` is the **only** showing operation between `bdc_index` and its `EMC`.
fn span_is_this_operation(operations: &[Operation], bdc_index: usize, op_index: usize) -> bool {
    let mut depth = 0usize;
    for (index, operation) in operations.iter().enumerate().skip(bdc_index) {
        if index != bdc_index {
            match operation.operator.as_str() {
                "BDC" | "BMC" => depth += 1,
                "EMC" => {
                    if depth == 0 {
                        break;
                    }
                    depth -= 1;
                }
                // A nested sequence's text belongs to the nested span, not to this one.
                _ if depth == 0
                    && index != op_index
                    && !text_operand_paths(operation).is_empty() =>
                {
                    return false;
                }
                _ => {}
            }
        }
    }
    true
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
        also: Vec::new(),
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
/// `/Encoding /WinAnsiEncoding`. No stream, no CMap.
///
/// The entry must land in the dictionary **this page resolves**, and must not be visible to
/// any other page (ruling B-7). Both requirements fail on a page whose `/Resources` is
/// inherited or shared, so such a page is given a private copy first. Writing to the
/// inherited dictionary instead would leave the edited page naming a font it cannot resolve
/// — the text would stop rendering — while editing an object other pages also read.
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

    let resources = writable_resources_mut(document, page_id)?;
    set_font_entry(resources, substitute_id)?;

    Ok(substitute_id)
}

/// The resources dictionary a write to `page_id` must go through, giving the page a private
/// copy of what it inherits when it does not own one.
///
/// Callers must not write into an inherited or shared dictionary: a page's own `/Resources`
/// replaces an inherited one outright, so a write to the ancestor is invisible on the page it
/// was meant for, and a shared object is another page's to keep (ruling B-7).
pub(crate) fn writable_resources_mut(
    document: &mut Document,
    page_id: ObjectId,
) -> Result<&mut lopdf::Dictionary, AppError> {
    match resources_slot(document, page_id) {
        ResourcesSlot::Page => private_page_resources(document, page_id),
        ResourcesSlot::Object(resources_id) => document
            .get_dictionary_mut(resources_id)
            .map_err(|error| AppError::Other(format!("page resources unreadable: {error}"))),
        ResourcesSlot::Shared => {
            let copy = effective_resources(document, page_id).unwrap_or_default();
            let page = document
                .get_object_mut(page_id)
                .map_err(|error| AppError::Other(format!("page object unreadable: {error}")))?
                .as_dict_mut()
                .map_err(|error| {
                    AppError::Other(format!("page object not a dictionary: {error}"))
                })?;
            page.set(b"Resources", Object::Dictionary(copy));
            private_page_resources(document, page_id)
        }
    }
}

/// How far up the `/Parent` chain resource inheritance is followed. PDF specifies no depth
/// limit; the bound only stops a malformed cyclic tree from looping.
const MAX_PARENT_DEPTH: usize = 64;

/// Where the `/Font` map a page must be edited through actually is.
enum ResourcesSlot {
    /// The page carries its own inline `/Resources` dictionary.
    Page,
    /// The page's `/Resources` is an indirect object no other page references.
    Object(ObjectId),
    /// `/Resources` is inherited from the page tree, or its object is referenced by more
    /// than one page. Either way it is not this page's to write into (ruling B-7).
    Shared,
}

/// The page's own `/Resources`, resolved as a viewer resolves it.
///
/// A page's own `/Resources` **replaces** an inherited one outright (ISO 32000-1 §7.7.3.4);
/// `/Font` is not merged across the page tree. `Document::get_page_fonts` unions them, which
/// answers "can this font be found at all" rather than "does this page resolve the name its
/// content stream writes".
pub(crate) fn effective_resources(
    document: &Document,
    page_id: ObjectId,
) -> Option<lopdf::Dictionary> {
    let mut node_id = page_id;
    for _ in 0..MAX_PARENT_DEPTH {
        let node = document.get_dictionary(node_id).ok()?;
        if let Ok(resources) = node.get(b"Resources") {
            let (_, resolved) = document.dereference(resources).ok()?;
            return resolved.as_dict().ok().cloned();
        }
        node_id = node.get(b"Parent").ok()?.as_reference().ok()?;
    }
    None
}

/// How many objects reference `id`, so a write can tell a private resource object from a
/// shared one.
fn reference_count(document: &Document, id: ObjectId) -> usize {
    fn count(object: &Object, id: ObjectId) -> usize {
        match object {
            Object::Reference(reference) => usize::from(*reference == id),
            Object::Array(entries) => entries.iter().map(|entry| count(entry, id)).sum(),
            Object::Dictionary(dictionary) => {
                dictionary.iter().map(|(_, value)| count(value, id)).sum()
            }
            Object::Stream(stream) => {
                stream.dict.iter().map(|(_, value)| count(value, id)).sum()
            }
            _ => 0,
        }
    }
    document.objects.values().map(|object| count(object, id)).sum()
}

/// Classifies the page's `/Resources` for a write.
fn resources_slot(document: &Document, page_id: ObjectId) -> ResourcesSlot {
    let Ok(page) = document.get_dictionary(page_id) else {
        return ResourcesSlot::Shared;
    };
    match page.get(b"Resources") {
        Ok(Object::Dictionary(_)) => ResourcesSlot::Page,
        Ok(resources) => match resources.as_reference() {
            Ok(id) if reference_count(document, id) == 1 => ResourcesSlot::Object(id),
            _ => ResourcesSlot::Shared,
        },
        Err(_) => ResourcesSlot::Shared,
    }
}

/// The page's own inline `/Resources` dictionary, for writing.
fn private_page_resources(
    document: &mut Document,
    page_id: ObjectId,
) -> Result<&mut lopdf::Dictionary, AppError> {
    let page = document
        .get_object_mut(page_id)
        .map_err(|error| AppError::Other(format!("page object unreadable: {error}")))?
        .as_dict_mut()
        .map_err(|error| AppError::Other(format!("page object not a dictionary: {error}")))?;
    if !page.has(b"Resources") {
        page.set(b"Resources", Object::Dictionary(lopdf::Dictionary::new()));
    }
    page.get_mut(b"Resources")
        .and_then(Object::as_dict_mut)
        .map_err(|_| {
            AppError::Other(
                "page /Resources is an indirect reference MECH-A cannot rewrite".to_string(),
            )
        })
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
    /// The rest of a merged run's fragments (P2.4.1 T18). A merged edit writes one operand
    /// with the replacement and clears each following fragment to empty; every one of those
    /// writes is recorded here, so a single undo restores all their original bytes. Empty for
    /// an ordinary single-operand edit.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub also: Vec<ContentOperation>,
    /// The `/Properties` clone a named-`ActualText` rewrite created (ruling B-7), so undo can
    /// repoint the `BDC` at the original name and drop the clone again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub property_clone: Option<PropertyClone>,
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
    /// Further `Tf` sites the same merged edit switched (P2.4.1 T18), when a page repeats
    /// `Tf` between a run's fragments. Empty for the ordinary single-`Tf` case.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub also: Vec<FontSwap>,
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

    /// P2.7 hotfix H0: a rotated text matrix must be *known*, not silently measured as if it
    /// were horizontal. `advance` is measured along the text axis, so for `Tm [0 1 -1 0 …]`
    /// it is a vertical extent — comparing it to a horizontal box is the wrong axis.
    #[test]
    fn a_rotated_text_matrix_is_flagged_on_the_run() {
        let document = testpdf::rotated_text();
        let page = page_id(&document, 0).unwrap();
        let runs = locate_runs(&document, page).unwrap();

        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].text, "ROTATED");
        assert!(runs[0].rotated, "a 90 degree matrix must be flagged rotated");
        // The origin still reads correctly: the first column of the matrix is the advance
        // direction, but `e`/`f` are still the pen position.
        assert_eq!(runs[0].origin, (300.0, 100.0));
    }

    /// The control: the same text and matrix shape without rotation must **not** be flagged,
    /// so the refusal cannot be blamed on the fixture rather than the transform.
    #[test]
    fn an_axis_aligned_matrix_is_not_flagged_as_rotated() {
        let document = testpdf::unrotated_text();
        let page = page_id(&document, 0).unwrap();
        let runs = locate_runs(&document, page).unwrap();

        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].text, "ROTATED");
        assert!(!runs[0].rotated);
    }

    /// A matrix that scales but does not rotate is also not rotated: `Tz`-style horizontal
    /// scaling and `Tm` with `a = 2, b = 0` still advance along the x-axis.
    #[test]
    fn a_scaled_axis_aligned_matrix_is_not_flagged_as_rotated() {
        let mut document = testpdf::blank_page();
        let page = page_id(&document, 0).unwrap();
        let content = Content {
            operations: vec![
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec!["F1".into(), 12.into()]),
                Operation::new(
                    "Tm",
                    vec![2.into(), 0.into(), 0.into(), 2.into(), 72.into(), 700.into()],
                ),
                Operation::new("Tj", vec![Object::string_literal("Scaled")]),
                Operation::new("ET", vec![]),
            ],
        };
        document
            .change_page_content(page, content.encode().unwrap())
            .unwrap();

        let runs = locate_runs(&document, page).unwrap();
        assert_eq!(runs.len(), 1);
        assert!(!runs[0].rotated);
    }

    /// P2.7.1 B-5: `Ts` is recorded on each run, and it does **not** move the pen. The two
    /// operands of `text_rise` share an origin y but differ in ink baseline by the rise.
    #[test]
    fn text_rise_is_recorded_without_moving_the_pen() {
        let document = testpdf::text_rise();
        let page = page_id(&document, 0).unwrap();
        let runs = locate_runs(&document, page).unwrap();

        let base = runs.iter().find(|run| run.text == "BASE ").expect("BASE");
        let high = runs.iter().find(|run| run.text == "HIGH").expect("HIGH");

        assert_eq!(base.rise, 0.0);
        assert_eq!(high.rise, 5.0);
        // `Ts` must not shift the pen vertically. (The pen's *horizontal* advance after a
        // plain `Tj` is not modelled at all -- `TextMatrixWalk` only advances inside `TJ` --
        // so this fixture deliberately says nothing about x. Tracked separately.)
        assert_eq!(high.origin.1, base.origin.1);
        // The ink is what the rest of the programme compares.
        assert!((high.baseline() - (base.baseline() + 5.0)).abs() < 0.001);
    }

    /// `Ts` is reset by `BT`, like every other text-state parameter. A rise that leaked past
    /// `BT` would silently displace the next block's ink.
    #[test]
    fn text_rise_is_reset_by_bt() {
        let mut document = testpdf::blank_page();
        let page = page_id(&document, 0).unwrap();
        let content = Content {
            operations: vec![
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec!["F1".into(), 12.into()]),
                Operation::new("Ts", vec![8.into()]),
                Operation::new("Td", vec![72.into(), 700.into()]),
                Operation::new("Tj", vec![Object::string_literal("Raised")]),
                Operation::new("ET", vec![]),
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec!["F1".into(), 12.into()]),
                Operation::new("Td", vec![72.into(), 600.into()]),
                Operation::new("Tj", vec![Object::string_literal("Flat")]),
                Operation::new("ET", vec![]),
            ],
        };
        document
            .change_page_content(page, content.encode().unwrap())
            .unwrap();

        let runs = locate_runs(&document, page).unwrap();
        let raised = runs.iter().find(|run| run.text == "Raised").expect("Raised");
        let flat = runs.iter().find(|run| run.text == "Flat").expect("Flat");
        assert_eq!(raised.rise, 8.0);
        assert_eq!(flat.rise, 0.0, "BT must reset Ts");
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

    // --- P2.4.1 T17: the text-matrix walker --------------------------------------

    /// The walker follows `Td` cumulatively, and `Tm` resets the frame absolutely.
    #[test]
    fn the_walker_tracks_td_cumulatively_and_tm_absolutely() {
        let mut document = testpdf::blank_page();
        let page = page_id(&document, 0).unwrap();
        let content = Content {
            operations: vec![
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec!["F1".into(), 10.into()]),
                Operation::new("Td", vec![100.into(), 700.into()]),
                Operation::new("Tj", vec![Object::string_literal("one")]),
                // A second `Td` moves *relative to the line matrix*: 100+20, 700-12.
                Operation::new("Td", vec![20.into(), (-12).into()]),
                Operation::new("Tj", vec![Object::string_literal("two")]),
                // `Tm` replaces the frame outright.
                Operation::new(
                    "Tm",
                    vec![
                        1.into(),
                        0.into(),
                        0.into(),
                        1.into(),
                        50.into(),
                        100.into(),
                    ],
                ),
                Operation::new("Tj", vec![Object::string_literal("three")]),
                Operation::new("ET", vec![]),
            ],
        };
        document
            .change_page_content(page, content.encode().unwrap())
            .unwrap();

        let runs = locate_runs(&document, page).unwrap();
        assert_eq!(runs[0].origin, (100.0, 700.0));
        assert_eq!(runs[1].origin, (120.0, 688.0));
        assert_eq!(runs[2].origin, (50.0, 100.0));
        assert_eq!(runs[0].font_size, 10.0);
    }

    /// A `TJ` array advances the pen per fragment, so a per-glyph array's operands land at
    /// successive x positions rather than all on the array's origin.
    #[test]
    fn tj_fragments_get_successive_origins() {
        let document = testpdf::fragmented_tj();
        let page = page_id(&document, 0).unwrap();
        let runs = locate_runs(&document, page).unwrap();
        assert_eq!(runs.len(), 5);

        // The first glyph sits exactly at the `Td`; each later glyph is to its right.
        assert_eq!(runs[0].origin, (72.0, 700.0));
        for pair in runs.windows(2) {
            assert!(
                pair[1].origin.0 > pair[0].origin.0,
                "each fragment must start to the right of the previous: {:?}",
                runs.iter().map(|r| r.origin.0).collect::<Vec<_>>()
            );
            assert_eq!(pair[1].origin.1, pair[0].origin.1, "same baseline");
        }
        // The advance is the AFM width, not zero: that is what makes the gap computable.
        assert!(runs[0].advance > 0.0);
    }

    /// `T*` and `TL` move to the next line by the leading, which is how a real document
    /// lays out successive lines without an absolute `Tm` each time.
    #[test]
    fn the_walker_follows_leading_with_t_star() {
        let mut document = testpdf::blank_page();
        let page = page_id(&document, 0).unwrap();
        let content = Content {
            operations: vec![
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec!["F1".into(), 12.into()]),
                Operation::new("TL", vec![14.into()]),
                Operation::new("Td", vec![0.into(), 700.into()]),
                Operation::new("Tj", vec![Object::string_literal("first")]),
                Operation::new("T*", vec![]),
                Operation::new("Tj", vec![Object::string_literal("second")]),
                Operation::new("ET", vec![]),
            ],
        };
        document
            .change_page_content(page, content.encode().unwrap())
            .unwrap();

        let runs = locate_runs(&document, page).unwrap();
        assert_eq!(runs[0].origin, (0.0, 700.0));
        assert_eq!(runs[1].origin, (0.0, 686.0));
    }

    /// The whole table, not one sample: PDFDocEncoding is Latin-1 **except** for 32 codes, and
    /// getting any one of them wrong is the silent-corruption path ruling B-6 exists to close.
    /// The expected string is ISO 32000-1 Annex D table D.2, cross-checked against qpdf's
    /// `pdf_doc_to_unicode` and poppler's `pdfDocEncodingToUnicode`; `\u{fffd}` marks the three
    /// codes the standard leaves undefined.
    #[test]
    fn pdfdocencoding_table_matches_the_standard() {
        #[rustfmt::skip]
        const EXPECTED: &str = concat!(
            "\u{0}\u{1}\u{2}\u{3}\u{4}\u{5}\u{6}\u{7}",
            "\u{8}\u{9}\u{a}\u{b}\u{c}\u{d}\u{e}\u{f}",
            "\u{10}\u{11}\u{12}\u{13}\u{14}\u{15}\u{16}\u{17}",
            "\u{2d8}\u{2c7}\u{2c6}\u{2d9}\u{2dd}\u{2db}\u{2da}\u{2dc}",
            "\u{20}\u{21}\u{22}\u{23}\u{24}\u{25}\u{26}\u{27}",
            "\u{28}\u{29}\u{2a}\u{2b}\u{2c}\u{2d}\u{2e}\u{2f}",
            "\u{30}\u{31}\u{32}\u{33}\u{34}\u{35}\u{36}\u{37}",
            "\u{38}\u{39}\u{3a}\u{3b}\u{3c}\u{3d}\u{3e}\u{3f}",
            "\u{40}\u{41}\u{42}\u{43}\u{44}\u{45}\u{46}\u{47}",
            "\u{48}\u{49}\u{4a}\u{4b}\u{4c}\u{4d}\u{4e}\u{4f}",
            "\u{50}\u{51}\u{52}\u{53}\u{54}\u{55}\u{56}\u{57}",
            "\u{58}\u{59}\u{5a}\u{5b}\u{5c}\u{5d}\u{5e}\u{5f}",
            "\u{60}\u{61}\u{62}\u{63}\u{64}\u{65}\u{66}\u{67}",
            "\u{68}\u{69}\u{6a}\u{6b}\u{6c}\u{6d}\u{6e}\u{6f}",
            "\u{70}\u{71}\u{72}\u{73}\u{74}\u{75}\u{76}\u{77}",
            "\u{78}\u{79}\u{7a}\u{7b}\u{7c}\u{7d}\u{7e}\u{fffd}",
            "\u{2022}\u{2020}\u{2021}\u{2026}\u{2014}\u{2013}\u{192}\u{2044}",
            "\u{2039}\u{203a}\u{2212}\u{2030}\u{201e}\u{201c}\u{201d}\u{2018}",
            "\u{2019}\u{201a}\u{2122}\u{fb01}\u{fb02}\u{141}\u{152}\u{160}",
            "\u{178}\u{17d}\u{131}\u{142}\u{153}\u{161}\u{17e}\u{fffd}",
            "\u{20ac}\u{a1}\u{a2}\u{a3}\u{a4}\u{a5}\u{a6}\u{a7}",
            "\u{a8}\u{a9}\u{aa}\u{ab}\u{ac}\u{fffd}\u{ae}\u{af}",
            "\u{b0}\u{b1}\u{b2}\u{b3}\u{b4}\u{b5}\u{b6}\u{b7}",
            "\u{b8}\u{b9}\u{ba}\u{bb}\u{bc}\u{bd}\u{be}\u{bf}",
            "\u{c0}\u{c1}\u{c2}\u{c3}\u{c4}\u{c5}\u{c6}\u{c7}",
            "\u{c8}\u{c9}\u{ca}\u{cb}\u{cc}\u{cd}\u{ce}\u{cf}",
            "\u{d0}\u{d1}\u{d2}\u{d3}\u{d4}\u{d5}\u{d6}\u{d7}",
            "\u{d8}\u{d9}\u{da}\u{db}\u{dc}\u{dd}\u{de}\u{df}",
            "\u{e0}\u{e1}\u{e2}\u{e3}\u{e4}\u{e5}\u{e6}\u{e7}",
            "\u{e8}\u{e9}\u{ea}\u{eb}\u{ec}\u{ed}\u{ee}\u{ef}",
            "\u{f0}\u{f1}\u{f2}\u{f3}\u{f4}\u{f5}\u{f6}\u{f7}",
            "\u{f8}\u{f9}\u{fa}\u{fb}\u{fc}\u{fd}\u{fe}\u{ff}",
        );
        let expected: Vec<char> = EXPECTED.chars().collect();
        assert_eq!(expected.len(), 256);
        for byte in 0u16..=255 {
            let want = match expected[byte as usize] {
                '\u{fffd}' => None,
                other => Some(other),
            };
            assert_eq!(
                pdf_doc_encoding_char(byte as u8),
                want,
                "byte 0x{byte:02X} must decode to {want:?}"
            );
        }
    }
}
