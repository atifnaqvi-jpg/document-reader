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

use lopdf::{content::Operation, Dictionary, Document, Object, ObjectId};
use std::collections::HashMap;

use crate::editor::geometry::{Point2, RunFrame};
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
    /// The Form XObjects followed to reach the operation (P2.7.3 §3), outermost first.
    ///
    /// Empty — and absent from the payload — for the page's own content stream, so every
    /// 0.5.0 address keeps its meaning.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub xobject_path: Vec<XObjectStep>,
}

/// One step of descent into a page's Form XObjects (P2.7.3 §2.2).
///
/// The **name** is the address: `renumber_objects` rewrites every object id on save, so an id
/// recorded here would name a different object when the inverse is replayed. `op_index` does
/// not resolve the form — the name does — but the grouped inverse needs it to repoint the
/// `Do`, and it is re-validated before use (§7).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct XObjectStep {
    /// The `/XObject` entry's name in the enclosing resource dictionary (`X1`).
    pub name: Vec<u8>,
    /// Index of the `Do` operation in the enclosing content stream that drew it.
    pub op_index: usize,
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
    /// True when the write path could draw an arbitrary replacement in this fragment's font
    /// (P2.7.5).
    ///
    /// Merging needs this, and not merely a matching `style_class`: a merged write clears every
    /// non-head fragment and draws the whole replacement through the head's font. Two
    /// fragments can share a style class and still not be one write target — `F4`/`F5` are both
    /// Times, both simple, so either can draw the replacement; `F3` (TrueType) and `F6`
    /// (Type0/Identity-H) are both Monaco and neither can stand in for the other.
    pub composable: bool,
    /// The face this fragment is actually drawn in — the `/BaseFont` with its subset prefix
    /// stripped, or the resource key when the font names none.
    ///
    /// `style_class` is deliberately coarse: it exists so a producer's `F4`→`F5` weight swap
    /// mid-line does not split a sentence. Being that coarse, it also calls `ArialMT`,
    /// `ABCDEE+Wingdings-Regular` and `ABCDEE+Monaco` all `Helvetica`. Merging on it alone
    /// would draw one face's text in another's glyphs and delete the rest (§1.1(a)). Two
    /// fragments are one run only if they are the same face *and* the same style.
    pub face: String,
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
    /// The horizontal scaling (`Tz`) in force as a **factor**, `1.0` for `Tz 100` (P2.8 T-8b).
    ///
    /// `origin`, [`Self::advance`] and [`Self::advance_exact`] are already *scaled* text space:
    /// every glyph advance and every `TJ` kern on the line carries `Tz/100` (ISO 32000-1 9.4.3,
    /// 9.4.4), so the walk that produced them applied it. `advance` is scaled but the font's own
    /// `w0` sum is not, so a consumer that needs the unscaled width — or that must multiply a
    /// *new* string's unscaled AFM width into scaled text space, which the reflow does — has no
    /// other place to get the factor from.
    ///
    /// Before this field existed the reflow measured replacements unscaled and reported a
    /// `Tz 100` position for a scaled page: measured at 136.704 against the correct 104.352 on
    /// `kerned-line-tz.pdf` (`Tz 50`).
    pub horizontal_scale: f32,
    /// The run's origin in **page space**, i.e. the text-space origin composed with the CTM
    /// (P2.7.4).
    ///
    /// [`Self::origin`] is the text-space pen position: it is what `Td`/`T*`/`TJ` arithmetic
    /// advances, and comparing one run's `origin` to another's cancels any transform, so those
    /// comparisons stay in text space. But PDFium reports ink in page space, so every
    /// comparison *across the lopdf/PDFium boundary* must use this. Before it existed,
    /// `pair_run` compared a text-space point to a page-space box, and any text under a
    /// non-identity `cm` — scale, translation, flip, not just rotation — failed to pair and was
    /// refused `run-mismatch`.
    pub page_origin: (f32, f32),

    /// The composed matrix's length scale **along the advance axis**, `|(m0, m1)|` (P2.7.4).
    ///
    /// [`Self::advance`] is a **text-space** length and a PDFium span is **page space**, so the
    /// width gate must convert one side through this factor. It is deliberately the advance
    /// axis alone: a width is measured along that axis, so that axis's length is the whole
    /// conversion, whether or not the up axis scales equally.
    ///
    /// This used to require a *uniform* scale and report `None` otherwise, which refused every
    /// sheared and every anisotropically scaled run at write time while the picker still
    /// offered it as editable (measured, A7 skew half). `None` now means only that the axis
    /// has collapsed to a point, where there is nothing to scale.
    pub page_scale: Option<f32>,

    /// The run's text direction in page space (P2.7.4).
    ///
    /// For an untransformed page this is [`RunFrame::PAGE`], so every projection written in
    /// terms of it reduces to the coordinate the pre-phase code read directly. It exists so a
    /// rise, a span, a span window or a seam can be measured along the axis the text actually
    /// advances on rather than assumed horizontal.
    pub frame: RunFrame,
    /// Set on the **placeholder** run standing for a `Do` this walk refused to descend
    /// (P2.7.3 Q1); `None` for every ordinary run.
    ///
    /// A refusal with no run to attach to would be invisible, and the text inside the form is
    /// precisely what was refused. The placeholder carries the reason instead, so the picker
    /// can say why the text it can see is not offered.
    pub xobject_refusal: Option<XObjectRefusal>,
}

/// Why a `Do` was not descended into (P2.7.3 §4.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XObjectRefusal {
    /// Descending would put the path one step past [`MAX_XOBJECT_DEPTH`].
    Depth,
    /// The form is already on the current path.
    Cycle,
    /// PDFium reported no placement matrix for the form, so its children cannot be mapped into
    /// page space. Emitting them with a child-space box would pair them to the wrong place.
    Unlocatable,
}

impl XObjectRefusal {
    /// The sentence the picker shows, naming the reason and the way out.
    pub fn message(self) -> &'static str {
        match self {
            XObjectRefusal::Depth => {
                "this text sits in forms nested too deeply to edit; select a narrower range, or \
                 use the annotation tools to add a note"
            }
            XObjectRefusal::Cycle => {
                "this form draws itself, so its text cannot be walked; select a narrower range, or \
                 use the annotation tools to add a note"
            }
            XObjectRefusal::Unlocatable => {
                "this text sits in a form the renderer cannot place on the page; select a \
                 narrower range, or use the annotation tools to add a note"
            }
        }
    }
}

impl RawRun {
    /// The baseline the run's **ink** sits on: the pen position plus `Ts` (P2.7.1 B-5).
    ///
    /// Every baseline comparison must use this rather than `origin.1`. A run with `Ts 5`
    /// draws 5pt above its pen, so comparing pen positions would place it on the wrong line.
    pub fn baseline(&self) -> f32 {
        self.origin.1 + self.rise
    }

    /// The **page-space** ink baseline, for comparing against PDFium (P2.7.4).
    ///
    /// The transform moves the ink along its own "up" direction, so `Ts` cannot simply be
    /// added to `page_origin.1`; the rise must be carried through the matrix like any other
    /// text-space offset. For the identity CTM this equals [`Self::baseline`].
    pub fn page_baseline(&self) -> f32 {
        let origin = Point2::new(self.page_origin.0, self.page_origin.1);
        self.frame.across(origin) + self.rise
    }

    /// The run's page-space pen position.
    pub fn page_point(&self) -> Point2 {
        Point2::new(self.page_origin.0, self.page_origin.1)
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
        let Some(index) = page
            .get(b"StructParents")
            .ok()
            .and_then(|o| o.as_i64().ok())
        else {
            return Vec::new();
        };
        let Some(root) = document
            .catalog()
            .ok()
            .and_then(|catalog| {
                catalog
                    .get(b"StructTreeRoot")
                    .ok()
                    .and_then(|root| document.dereference(root).ok().map(|(_, root)| root))
            })
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
        "P" | "H"
            | "H1"
            | "H2"
            | "H3"
            | "H4"
            | "H5"
            | "H6"
            | "L"
            | "LI"
            | "LBody"
            | "Lbl"
            | "Table"
            | "TR"
            | "TH"
            | "TD"
            | "THead"
            | "TBody"
            | "TFoot"
            | "Figure"
            | "Caption"
            | "BlockQuote"
            | "Note"
            | "TOC"
            | "TOCI"
            | "Sect"
            | "Div"
            | "Art"
            | "Part"
            | "Index"
            | "NonStruct"
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

/// The nesting cap for Form XObjects (P2.7.3 Q1). The page is depth 0, so a path of length 8
/// is editable and a ninth step is refused.
pub const MAX_XOBJECT_DEPTH: usize = 8;

/// The resources in force at one level of the descent, and how that level was reached.
///
/// A form's own `/Resources` **replaces** the enclosing one only for the keys it defines
/// (ISO 32000-1 §8.10); the enclosing dictionary answers the rest. Resolving a font against
/// the page in every case would decode a form's `/F9` through the page's unrelated `/F1` —
/// the same class of bug T-3b fixed for named `/Properties`.
struct ResourceLevel<'a> {
    /// The dictionaries in force, outermost first. A name resolves through the innermost
    /// dictionary that defines it, falling back to the enclosing ones (ISO 32000-1 §8.10).
    dictionaries: Vec<&'a Dictionary>,
    /// The `Do` steps that reached this level.
    xobject_path: Vec<XObjectStep>,
    /// The form ids on this path, for cycle detection.
    form_ids: Vec<ObjectId>,
}

impl<'a> ResourceLevel<'a> {
    /// An entry of the `/XObject` dictionary in force.
    fn xobject(&self, document: &'a Document, name: &[u8]) -> Option<&'a Object> {
        self.dictionaries.iter().rev().find_map(|level| {
            let entry = level.get(b"XObject").ok()?;
            let dictionary = document.dereference(entry).ok()?.1.as_dict().ok()?;
            dictionary.get(name).ok()
        })
    }

    /// The font a `/Fx Tf` in this level's content names.
    fn font(&self, document: &'a Document, name: &[u8]) -> Option<&'a Dictionary> {
        self.dictionaries.iter().rev().find_map(|level| {
            let entry = level.get(b"Font").ok()?;
            let dictionary = document.dereference(entry).ok()?.1.as_dict().ok()?;
            document
                .dereference(dictionary.get(name).ok()?)
                .ok()?
                .1
                .as_dict()
                .ok()
        })
    }

    /// The form stream a `Do` names, with its object id, or `None` when the name is not a
    /// form — an image, or a reference that does not resolve. Neither holds text, so neither
    /// is a refusal (§4.1).
    fn form(&self, document: &'a Document, name: &[u8]) -> Option<(&'a lopdf::Stream, ObjectId)> {
        let (id, object) = document.dereference(self.xobject(document, name)?).ok()?;
        let stream = object.as_stream().ok()?;
        if stream.dict.get(b"Subtype").ok()?.as_name().ok()? != b"Form" {
            return None;
        }
        Some((stream, id?))
    }

    /// The level a `Do` of `name` descends into, or why it must not be descended.
    fn descend(&self, document: &'a Document, name: &[u8], op_index: usize) -> Descend<'a> {
        let Some((stream, form_id)) = self.form(document, name) else {
            return Descend::Skip;
        };
        if self.xobject_path.len() >= MAX_XOBJECT_DEPTH {
            return Descend::Refuse(XObjectRefusal::Depth);
        }
        if self.form_ids.contains(&form_id) {
            return Descend::Refuse(XObjectRefusal::Cycle);
        }
        let mut xobject_path = self.xobject_path.clone();
        xobject_path.push(XObjectStep {
            name: name.to_vec(),
            op_index,
        });
        // Per-key fallback (ISO 32000-1 §8.10): the form's own dictionary answers the names it
        // defines, the enclosing one answers the rest. A form written without `/Resources` is
        // common in real files and would otherwise resolve no font at all.
        let mut dictionaries = self.dictionaries.clone();
        if let Some(own) = stream
            .dict
            .get(b"Resources")
            .ok()
            .and_then(|object| document.dereference(object).ok())
            .and_then(|(_, object)| object.as_dict().ok())
        {
            dictionaries.push(own);
        }
        Descend::Level(ResourceLevel {
            dictionaries,
            xobject_path,
            form_ids: self.form_ids.iter().copied().chain([form_id]).collect(),
        })
    }
}

/// What `ResourceLevel::descend` found.
enum Descend<'a> {
    /// A form to walk into.
    Level(ResourceLevel<'a>),
    /// Not a form, or not resolvable: nothing to locate inside, and not a refusal.
    Skip,
    /// A form this walk must not enter. A placeholder run reports it.
    Refuse(XObjectRefusal),
}

/// Every text run on a page, in content order, **including** the text drawn by Form XObjects.
///
/// A run whose font cannot be resolved or whose bytes cannot be decoded through the font's
/// encoding is **dropped** rather than returned with a guessed string: an unreadable run
/// cannot be shown to the user or matched later, so offering it would only promise an edit the
/// write path has to refuse.
///
/// Forms are walked level by level (P2.7.3 Q1), because stopping at one level leaves text in
/// nested forms invisible — the silent failure this gate exists to remove. A `Do` the walk may
/// not enter still yields a placeholder, so a refusal is visible rather than absent.
pub fn locate_runs(document: &Document, page_id: ObjectId) -> Result<Vec<RawRun>, AppError> {
    let content = decoded_content(document, page_id)?;
    let page_fonts = document
        .get_page_fonts(page_id)
        .map_err(|error| AppError::Other(format!("could not read page fonts: {error}")))?;
    let page_resources = effective_resources(document, page_id);
    let mut root_dictionaries = Vec::new();
    if let Some(resources) = page_resources.as_ref() {
        root_dictionaries.push(resources);
    }

    let mut runs = Vec::new();
    let mut walker = TextMatrixWalk::new();
    let mut marked = MarkedContentStack::new(document, page_id);
    walk_level(
        document,
        &page_fonts,
        &content.operations,
        &ResourceLevel {
            dictionaries: root_dictionaries,
            xobject_path: Vec::new(),
            form_ids: Vec::new(),
        },
        &mut walker,
        &mut marked,
        page_id,
        &mut runs,
    );
    Ok(runs)
}

/// Walks one content stream, appending its runs and descending the forms it draws.
///
/// The text state is per level: a form is a separate content stream, and the renderer starts
/// its `Tm`/`Tf`/CTM from the graphics state it was invoked in, not from the page's. The
/// walker and marked-content stack are therefore created fresh for each form rather than
/// shared, which keeps every measurement identical to drawing the same text on its own page.
#[allow(clippy::too_many_arguments)]
fn walk_level<'a>(
    document: &'a Document,
    page_fonts: &std::collections::BTreeMap<Vec<u8>, &'a Dictionary>,
    operations: &[Operation],
    level: &ResourceLevel<'a>,
    walker: &mut TextMatrixWalk,
    marked: &mut MarkedContentStack<'a>,
    page_id: ObjectId,
    runs: &mut Vec<RawRun>,
) {
    for (op_index, operation) in operations.iter().enumerate() {
        walker.step(operation);
        marked.step(operation);

        if operation.operator == "Do" {
            let Some(name) = operation.operands.first().and_then(|o| o.as_name().ok()) else {
                continue;
            };
            match level.descend(document, name, op_index) {
                Descend::Level(child) => {
                    let Some((stream, _)) = level.form(document, name) else {
                        continue;
                    };
                    // A form whose stream will not decode is skipped rather than failing the
                    // page: the rest of the page is still readable and editable.
                    let Ok(content) = decode_stream(stream) else {
                        continue;
                    };
                    let mut form_walker = TextMatrixWalk::new();
                    let mut form_marked = MarkedContentStack::new(document, page_id);
                    walk_level(
                        document,
                        page_fonts,
                        &content.operations,
                        &child,
                        &mut form_walker,
                        &mut form_marked,
                        page_id,
                        runs,
                    );
                }
                Descend::Refuse(reason) => {
                    runs.push(placeholder_run(op_index, name, level, reason));
                }
                Descend::Skip => {}
            }
            continue;
        }

        let paths = text_operand_paths(operation);
        if paths.is_empty() {
            continue;
        }

        // The font in force is the last `/Fx Tf` before this operation, resolved through the
        // resources **of this level**. A run with no resolvable font cannot be decoded, and
        // its encoding gates are unknowable.
        let Some(font_key) = font_in_force(operations, op_index) else {
            continue;
        };
        let font = level
            .font(document, &font_key)
            .or_else(|| page_fonts.get(&font_key).copied());
        let Some(font) = font else {
            continue;
        };
        // `/ToUnicode` first (P2.7.6): an embedded subset writes `uniXXXX` `/Differences`
        // names, which lopdf cannot resolve, and its silent fallback to StandardEncoding would
        // make the picker show the wrong characters for text this build itself wrote. `None`
        // means the font is unreadable and its runs are skipped, as before.
        // A Type0 run's codes are two bytes, so it decodes through lopdf's Identity-H reader
        // (which follows `/ToUnicode`); a simple font decodes through its own `/ToUnicode`
        // first, because `uniXXXX` `/Differences` names are outside lopdf's glyph list.
        let decode = |bytes: &[u8]| decode_run_bytes(document, font, bytes);
        // P2.4.2 Phase A: the run's advance comes from the font's own metrics when it has any,
        // so the merge gap is exact rather than a per-character guess.
        let metrics = crate::editor::substitute::FontMetrics::resolve_with(
            document,
            font,
            font.get(b"BaseFont")
                .ok()
                .and_then(|object| object.as_name().ok())
                .map(|name| String::from_utf8_lossy(name).into_owned())
                .as_deref(),
        );

        // Where each operand showed: an ordinary `Tj` shows at the current matrix origin, but
        // fragments inside one `TJ` array advance along the line as their kerning numbers and
        // glyph widths dictate (T17).
        let per_path = tj_fragment_origins(operation, walker, &metrics, &decode);
        // T29: an `MCID` changes only at `BDC`/`EMC`, which cannot occur inside a `TJ` array,
        // so the snapshot is taken once per operation.
        let marked_content = marked.snapshot();

        for path in paths {
            let Some(bytes) = string_at(operation, &path) else {
                continue;
            };
            let Some(text) = decode(bytes) else {
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
            let advance_exact = metrics.advance(&text, walker.size);
            // P2.7.4: the same origin, carried into page space. `origin` stays in text space
            // because `Td`/`TJ` arithmetic and every run-to-run comparison live there; this is
            // only for comparing against PDFium, which knows nothing of text space.
            let page_origin = walker.to_page(origin.0, origin.1);
            let TextAxes {
                advance: advance_axis,
                up: up_axis,
                scale: page_scale,
                mirrored,
                degenerate,
            } = walker.axes();

            runs.push(RawRun {
                id: RunId {
                    op_index,
                    operand_path: path,
                    xobject_path: level.xobject_path.clone(),
                },
                bytes: bytes.to_vec(),
                text: text.clone(),
                font_key: font_key.clone(),
                operator: operation.operator.clone(),
                origin,
                page_origin,
                page_scale,
                frame: RunFrame::with_matrix(advance_axis, up_axis, mirrored, degenerate),
                font_size: walker.size,
                advance: spaced_advance(&metrics, &text, walker),
                advance_exact: advance_exact.map(|width| spaced_width(width, &text, walker)),
                composable: crate::editor::cid::CidFont::resolve(document, font).is_none(),
                face: face_name(font),
                style_class: crate::editor::substitute::style_match(
                    font.get(b"BaseFont")
                        .ok()
                        .and_then(|object| object.as_name().ok())
                        .map(|name| String::from_utf8_lossy(name).into_owned())
                        .as_deref(),
                ),
                marked_content: marked_content.clone(),
                rise: walker.rise(),
                horizontal_scale: walker.horizontal_scale(),
                xobject_refusal: None,
            });
        }
    }
}

/// The face a fragment is drawn in, for the merge rule (P2.7.5 §3.3).
///
/// Subset prefixes are stripped so the prefix, not the face, does not split a run: two
/// resources naming `ABCDEE+Monaco` and `FGHIJK+Monaco` are the same face. A font that names
/// no face falls back to an empty string, which then only merges with another unnamed font —
/// honest, because such a font has no identity to compare.
fn face_name(font: &Dictionary) -> String {
    font.get(b"BaseFont")
        .ok()
        .and_then(|object| object.as_name().ok())
        .map(|name| {
            let name = String::from_utf8_lossy(name);
            crate::editor::substitute::strip_subset_prefix(&name).to_owned()
        })
        .unwrap_or_default()
}

/// Append a run that stands in for an operator the locator refused to walk (P2.7.3 X1).
///
/// It carries no box and cannot be edited — its `RunId` names a name operand, which fails
/// every write validation — but it exists so the user is told why the text inside is missing
/// rather than seeing a form behave as if it were empty.
fn placeholder_run(
    op_index: usize,
    name: &[u8],
    level: &ResourceLevel<'_>,
    reason: XObjectRefusal,
) -> RawRun {
    RawRun {
        id: RunId {
            op_index,
            operand_path: vec![0],
            xobject_path: level.xobject_path.clone(),
        },
        bytes: name.to_vec(),
        text: format!("/{}", String::from_utf8_lossy(name)),
        font_key: Vec::new(),
        operator: "Do".to_string(),
        origin: (0.0, 0.0),
        page_origin: (0.0, 0.0),
        page_scale: Some(1.0),
        frame: RunFrame::PAGE,
        font_size: 0.0,
        advance: 0.0,
        advance_exact: None,
        composable: false,
        face: String::new(),
        style_class: String::new(),
        marked_content: Vec::new(),
        rise: 0.0,
        horizontal_scale: 1.0,
        xobject_refusal: Some(reason),
    }
}

/// Decodes one Form XObject's content stream, bounded by the same cap as a page (T3e).
fn decode_stream(
    stream: &lopdf::Stream,
) -> Result<lopdf::content::Content<Vec<Operation>>, AppError> {
    let bytes = stream
        .decompressed_content_with_limit(CONTENT_LIMIT)
        .map_err(|error| AppError::Other(format!("could not read form content: {error}")))?;
    lopdf::content::Content::decode(&bytes)
        .map_err(|error| AppError::Other(format!("could not parse form content: {error}")))
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
    decode: &dyn Fn(&[u8]) -> Option<String>,
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
                    let width = decode(bytes)
                        .map(|text| spaced_advance(metrics, &text, state))
                        .unwrap_or(0.0);
                    pen_x += width;
                }
                // A kerning number is in thousandths of an em, subtracted from the pen, and
                // then scaled by `Tz/100` along with every other advance on the line
                // (ISO 32000-1 9.4.3 for the array adjustment, 9.4.4 for the text-state
                // scaling). Omitting the factor is arithmetically invisible at `Tz 100` and
                // 3 pt wrong at `Tz 50` — see `spaced_width`, which applies it to the other
                // two terms (T-8).
                other => {
                    if let Ok(number) = other.as_float() {
                        pen_x -= number / 1000.0 * state.size * state.horizontal_scale / 100.0;
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
///
/// The CTM (`q`/`cm`/`Q`) is tracked **only** to decide whether a run is rotated (P2.7 T-7):
/// the renderer draws text with `tm x ctm`, so a rotation carried by `cm` with an identity
/// `Tm` is still rotated text. Only the CTM is saved and restored here -- the text-state
/// parameters (`Tc`/`Tw`/`Tz`/`Ts`/`TL`/`Tf`), which are also graphics state, are reset by
/// `BT` and not by `q`/`Q` in this walker. The CTM is never folded into [`Self::origin`]:
/// that is the same honest boundary Stage 1 drew for `font_in_force`.
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
    /// The current transformation matrix (`cm`, ISO 32000-1 8.3.4), for rotation detection.
    ctm: [f32; 6],
    /// CTM saved by `q`, restored by `Q`. A stack, not a single slot: `q` nests.
    ctm_stack: Vec<[f32; 6]>,
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
            ctm: IDENTITY,
            ctm_stack: Vec::new(),
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
            "q" => self.ctm_stack.push(self.ctm),
            // An unmatched `Q` must not invent a CTM: a malformed stream would otherwise be
            // read as a transform the renderer never applied.
            "Q" => {
                if let Some(saved) = self.ctm_stack.pop() {
                    self.ctm = saved;
                }
            }
            "cm" => {
                if let Some(matrix) = matrix_operands(&operation.operands) {
                    self.ctm = matrix_mul(matrix, self.ctm);
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

    /// The composed text-to-page matrix, `tm x ctm` (P2.7.4).
    ///
    /// This is the matrix the renderer actually draws through. Everything that crosses into
    /// page space — the composed origin, the advance axis, the scale factor — is derived from
    /// it, so all of them agree with each other and with PDFium by construction.
    fn composed(&self) -> [f32; 6] {
        matrix_mul(self.tm, self.ctm)
    }

    /// A text-space point carried into page space by the **CTM**.
    ///
    /// Deliberately *not* the composed matrix: [`Self::origin`] is `tm`'s translation column,
    /// so it is already the point `(0, 0)` mapped through `Tm`. Composing it again with `tm`
    /// applies that translation twice — which reads on an untransformed page as an origin
    /// displaced by exactly its own coordinates. `Tm`'s scaling reaches the page through
    /// [`Self::axes`], which is where the full product belongs.
    fn to_page(&self, x: f32, y: f32) -> (f32, f32) {
        let m = self.ctm;
        (x * m[0] + y * m[2] + m[4], x * m[1] + y * m[3] + m[5])
    }

    /// Unit vectors of the composed matrix's advance and "up" axes, in page space, plus its
    /// uniform scale factor (P2.7.4).
    ///
    /// `scale` is `None` when the two axes scale differently, because then no single number
    /// converts a text-space length to a page-space length and the width gate has nothing
    /// honest to compare.
    ///
    /// The third value is the sign of the determinant of the matrix the **glyphs are drawn
    /// through**, which is `tm x ctm` times the horizontal scale `Tz`. `Tz` is a scalar, so it
    /// never reaches `advance` or `up`; computing handedness from the axes alone therefore
    /// calls PDFlib's `1 0 0 -1 0 841 cm` with `Tz -100` mirrored, although it renders upright
    /// (measured against poppler, four variants). A degenerate `Tz` has no handedness.
    fn axes(&self) -> TextAxes {
        let m = self.composed();
        let unit = |x: f32, y: f32| {
            let length = (x * x + y * y).sqrt();
            if length > f32::EPSILON {
                (x / length, y / length)
            } else {
                // A degenerate matrix collapses text to a point; there is no axis. Report the
                // untransformed one so callers divide by something, and let `scale` be `None`
                // so nothing is measured through it.
                (1.0, 0.0)
            }
        };
        let advance = unit(m[0], m[1]);
        let up = unit(m[2], m[3]);
        let advance_scale = (m[0] * m[0] + m[1] * m[1]).sqrt();
        // A width is measured *along the advance axis* and converted page-ward by that axis's
        // length alone, so this is the factor a length conversion needs. Whether the up axis
        // scales equally is a different question: it decides whether a box's extent can be
        // trusted as a *height*, never whether an advance can be converted. Requiring
        // uniformity here refused every sheared or anisotropically scaled run — A7's skew half.
        let scale = if advance_scale > f32::EPSILON {
            Some(advance_scale)
        } else {
            None
        };
        let determinant = m[0] * m[3] - m[1] * m[2];
        // `Tz` scales the advance only, so it multiplies the determinant without changing
        // either axis's direction. Zero collapses the pen, so there is no handedness to state.
        let degenerate = determinant.abs() <= f32::EPSILON || self.horizontal_scale == 0.0;
        let mirrored = !degenerate && determinant * self.horizontal_scale < 0.0;
        TextAxes {
            advance,
            up,
            scale,
            mirrored,
            degenerate,
        }
    }

    /// The text rise (`Ts`) in force, in points.
    ///
    /// `Ts` moves the *baseline* of the drawn glyphs without moving the pen, so it must never
    /// enter [`Self::origin`] or any advance computation. It is recorded separately and added
    /// by the run so pairing can compare the line the ink actually sits on (P2.7.1 ruling B-5).
    fn rise(&self) -> f32 {
        self.rise
    }

    /// The horizontal scaling (`Tz`) in force, as a factor rather than a percentage.
    ///
    /// Converted here rather than at each caller so no consumer can apply `Tz` as if it were
    /// `100`-based: `origin`, `advance` and `advance_exact` on a run are already scaled, and the
    /// reflow needs the same factor to scale a *new* string's unscaled width into that space
    /// (P2.8 T-8b).
    fn horizontal_scale(&self) -> f32 {
        self.horizontal_scale / 100.0
    }
}

/// The axes, page scale and handedness one text-matrix state resolves to (see `axes`).
struct TextAxes {
    advance: (f32, f32),
    up: (f32, f32),
    scale: Option<f32>,
    mirrored: bool,
    degenerate: bool,
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
        if !body.len().is_multiple_of(2) {
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
    bytes
        .iter()
        .map(|byte| pdf_doc_encoding_char(*byte))
        .collect()
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
    const LOW: [char; 8] = [
        '\u{02d8}', '\u{02c7}', '\u{02c6}', '\u{02d9}', '\u{02dd}', '\u{02db}', '\u{02da}',
        '\u{02dc}',
    ];
    const HIGH: [char; 31] = [
        '\u{2022}', '\u{2020}', '\u{2021}', '\u{2026}', '\u{2014}', '\u{2013}', '\u{0192}',
        '\u{2044}', '\u{2039}', '\u{203a}', '\u{2212}', '\u{2030}', '\u{201e}', '\u{201c}',
        '\u{201d}', '\u{2018}', '\u{2019}', '\u{201a}', '\u{2122}', '\u{fb01}', '\u{fb02}',
        '\u{0141}', '\u{0152}', '\u{0160}', '\u{0178}', '\u{017d}', '\u{0131}', '\u{0142}',
        '\u{0153}', '\u{0161}', '\u{017e}',
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

/// The font the page resolves for `font_key`, for measurement (P2.7.3 X1).
pub fn font_dict<'a>(
    document: &'a Document,
    page_id: ObjectId,
    font_key: &[u8],
) -> Option<&'a Dictionary> {
    document
        .get_page_fonts(page_id)
        .ok()?
        .get(font_key)
        .copied()
}

/// The resource dictionaries in force along a run's `xobject_path`, outermost first.
///
/// A level contributes its **own** `/Resources` only when it declares one; a level that
/// declares none contributes nothing, so the names it writes resolve through the levels above
/// it (ISO 32000-1 §8.10). This is the reading [`ResourceLevel`] walks for discovery, without
/// borrowing the document, so the write path can hold the same view while it mutates.
pub(crate) fn resource_stack(
    document: &Document,
    page_id: ObjectId,
    path: &[XObjectStep],
) -> Vec<Dictionary> {
    let mut stack = Vec::new();
    if let Some(resources) = effective_resources(document, page_id) {
        stack.push(resources);
    }
    for step in path {
        let Some(form) = form_by_name(document, &stack, &step.name) else {
            break;
        };
        let own = form
            .dict
            .get(b"Resources")
            .ok()
            .and_then(|object| document.dereference(object).ok())
            .and_then(|(_, object)| object.as_dict().ok())
            .cloned();
        if let Some(own) = own {
            stack.push(own);
        }
    }
    stack
}

/// The form a resource name resolves to, through the resource dictionaries in force.
fn form_by_name<'a>(
    document: &'a Document,
    stack: &'a [Dictionary],
    name: &[u8],
) -> Option<&'a lopdf::Stream> {
    stack.iter().rev().find_map(|level| {
        let xobjects = level.get(b"XObject").ok()?;
        let xobjects = document.dereference(xobjects).ok()?.1.as_dict().ok()?;
        let object = xobjects.get(name).ok()?;
        document.dereference(object).ok()?.1.as_stream().ok()
    })
}

/// The font a resource name resolves to **at a run's addressing level** (P2.7.3 §6).
///
/// A form routinely declares its own `/Font` and the page does not; resolving through the page
/// would refuse text whose face is perfectly readable, and would decode it through an
/// unrelated face when the page happens to reuse the same name.
pub(crate) fn font_dictionary_at(
    document: &Document,
    page_id: ObjectId,
    path: &[XObjectStep],
    key: &[u8],
) -> Option<Dictionary> {
    let stack = resource_stack(document, page_id, path);
    stack.iter().rev().find_map(|level| {
        let fonts = level.get(b"Font").ok()?;
        let fonts = document.dereference(fonts).ok()?.1.as_dict().ok()?;
        document
            .dereference(fonts.get(key).ok()?)
            .ok()?
            .1
            .as_dict()
            .ok()
            .cloned()
    })
}

/// The same resolution, reduced to the fields the gates read.
pub fn font_info_at(
    document: &Document,
    page_id: ObjectId,
    path: &[XObjectStep],
    key: &[u8],
) -> Option<FontInfo> {
    let font = font_dictionary_at(document, page_id, path, key)?;
    let read = |key: &[u8]| {
        font.get(key)
            .ok()
            .and_then(|object| object.as_name().ok())
            .map(|name| String::from_utf8_lossy(name).into_owned())
    };
    Some(FontInfo {
        key: key.to_vec(),
        base_font: read(b"BaseFont"),
        subtype: read(b"Subtype"),
    })
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

/// Decodes a run's bytes the way the run itself is read.
///
/// Every site that turns bytes back into text must agree with the picker, or an edit refuses
/// with `TEXT_RUN_MISMATCH` on bytes nobody changed. Two code widths live here: a simple font
/// is one byte per code and prefers its own `/ToUnicode` (MECH-B writes `uniXXXX`
/// `/Differences` names lopdf cannot resolve); a Type0 font is two bytes per code and is read
/// by lopdf's Identity-H machinery, which follows `/ToUnicode`.
pub(crate) fn decode_run_bytes(
    document: &Document,
    font: &Dictionary,
    bytes: &[u8],
) -> Option<String> {
    if !crate::editor::substitute::is_type0(font) {
        if let Some(decoded) = crate::editor::substitute::decode_simple(document, font, bytes) {
            return Some(decoded);
        }
    }
    font.get_font_encoding(document)
        .ok()?
        .bytes_to_string(bytes)
        .ok()
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
    // `/ToUnicode` first: an embedded subset (MECH-B) carries `uniXXXX` `/Differences` names,
    // which lopdf's glyph list does not know, and its fallback to StandardEncoding would decode
    // the inverse's own bytes as the wrong text and refuse a valid undo.
    let current_text =
        decode_run_bytes(document, font, &old_bytes).ok_or(AppError::TextRunMismatch)?;
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
        xobject_path: Vec::new(),
        xobject_clones: Vec::new(),
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
    /// MECH-B: the subset to install as a new font resource and switch the run to.
    ///
    /// Not an `Option` beside [`MergedWrite::substitute`]: those two spell "which face does
    /// this run end up in", and two optional fields let a caller name both, or neither while
    /// meaning one. The enum makes the choice total.
    pub embed: Option<&'a crate::editor::embed::Subset>,
    /// P2.7.7: the CID subset to install, when the run's font is CID-keyed. Distinct from
    /// `embed` because the two write different objects and different content bytes.
    pub embed_cid: Option<&'a crate::editor::embed::CidSubset>,
    /// Which face the run must end up in. `Keep` covers an ordinary edit and a run whose own
    /// font already sets the replacement; the two mechanisms are exclusive here, not by
    /// convention.
    pub face: FaceWrite,
    /// The text the user asked for, when there is one. Only `/ActualText` consumes it (P2.7.1):
    /// the property must state the *semantic* text, which the operand bytes are not.
    pub replacement: Option<&'a str>,
    /// The content stream being written. Empty is the page; a non-empty path is the Form
    /// XObject clone the edit was copied into (P2.7.3 X2).
    pub level: Vec<XObjectStep>,
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
    let level = &write.level;
    let mut content = read_level(document, page_id, level)?;

    // Every name a fragment reads resolves through the resources **in force at this level**
    // (P2.7.3 §6). The page's table may not name the resource at all, and where it does reuse
    // the name it is a different face — decoding through it would corrupt the comparison.
    let fonts: Vec<Dictionary> = write
        .fragments
        .iter()
        .map(|fragment| {
            let font_key = font_in_force(&content.operations, fragment.id.op_index)
                .ok_or(AppError::TextRunMismatch)?;
            font_dictionary_at(document, page_id, level, &font_key).ok_or(AppError::TextRunMismatch)
        })
        .collect::<Result<_, _>>()?;

    // Validate and collect every original string before mutating anything, so a mismatch on
    // the last fragment leaves the stream untouched rather than half rewritten.
    let mut originals = Vec::with_capacity(write.fragments.len());
    for (fragment, font) in write.fragments.iter().zip(fonts.iter()) {
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
        let current = decode_run_bytes(document, font, &old).ok_or(AppError::TextRunMismatch)?;
        if current != fragment.expected {
            return Err(AppError::TextRunMismatch);
        }
        originals.push(old);
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
    match write.face {
        FaceWrite::Keep => {}
        FaceWrite::Substitute => {
            let Some(base_font) = write.substitute else {
                return Err(AppError::Other(
                    "a substituted run was written without the face to substitute".into(),
                ));
            };
            ensure_substitute_resource(document, page_id, level, base_font)?;
            swap_fonts(&mut content, &write.fragments, SUBSTITUTE_NAME, &mut swaps)?;
        }
        FaceWrite::EmbedCid => {
            let Some(subset) = write.embed_cid else {
                return Err(AppError::Other(
                    "an embedded CID run was written without the subset to install".into(),
                ));
            };
            let (name, _id) = install_embedded_cid_resource(document, page_id, level, subset)?;
            swap_fonts(&mut content, &write.fragments, &name, &mut swaps)?;
        }
        FaceWrite::Embed => {
            let Some(subset) = write.embed else {
                return Err(AppError::Other(
                    "an embedded run was written without the subset to install".into(),
                ));
            };
            // A subset costs bytes, so it gets its own resource name rather than sharing `FS1`
            // with MECH-A: two different subsets are two different faces, and reusing one name
            // would make the second edit silently repaint the first.
            let (name, _id) = install_embedded_resource(document, page_id, level, subset)?;
            swap_fonts(&mut content, &write.fragments, &name, &mut swaps)?;
        }
    }

    // P2.7.1 T30: a run inside `/Span <</ActualText …>>` has semantic text that must not be
    // left reading the old string. The rewrite rides in this same pass, so one undo restores
    // both the operand and the property. `false` means the span is shared or the properties
    // are a shared resource: the edit lands, but the caller must tell the user.
    //
    // P2.7.3 Q3: a clone owns its `/Resources`, so its `/Properties` — inline or referenced —
    // is this edit's to write. On the page path the inherited/shared resolution still applies.
    let named = level_properties(document, page_id, level);
    let (actual_text_preserved, property_clone) = rewrite_actual_text(
        document,
        page_id,
        level,
        &mut content.operations,
        head.id.op_index,
        write.replacement,
        &named,
    )?;

    write_level(document, page_id, level, &content)?;
    prune_unused_embedded_fonts(document, page_id, level, &content.operations);

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
            xobject_path: level.clone(),
            xobject_clones: Vec::new(),
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
            xobject_path: level.clone(),
            xobject_clones: Vec::new(),
        },
        actual_text_preserved,
    ))
}

/// The `/Properties` entries in force at one content level.
///
/// A form's own `/Resources` supplies the keys it defines and the enclosing level supplies the
/// rest (ISO 32000-1 §8.10), so the walk goes outermost-in and lets an inner level win.
fn level_properties(
    document: &Document,
    page_id: ObjectId,
    level: &[XObjectStep],
) -> HashMap<Vec<u8>, Dictionary> {
    if level.is_empty() {
        return page_properties(document, page_id);
    }
    let mut merged = HashMap::new();
    for resources in resource_stack(document, page_id, level) {
        let Some(properties) = resources
            .get(b"Properties")
            .ok()
            .and_then(|entry| document.dereference(entry).ok())
            .and_then(|(_, object)| object.as_dict().ok())
        else {
            continue;
        };
        for (name, value) in properties.iter() {
            let Some(entry) = document
                .dereference(value)
                .ok()
                .and_then(|(_, object)| object.as_dict().ok())
            else {
                continue;
            };
            merged.insert(name.clone(), entry.clone());
        }
    }
    merged
}

/// Mutable access to the `/Properties` dictionary a write at one level may modify.
///
/// A clone is given its private resources at exactly this point (design §5 step 2, Q3): writing
/// into a resources dictionary the original form still shares would recreate B-7 one level down,
/// and creating the copy any earlier would leave an unused object behind on an edit that never
/// touched the resources.
fn properties_mut_for_level<'a>(
    document: &'a mut Document,
    page_id: ObjectId,
    level: &[XObjectStep],
) -> Result<&'a mut Dictionary, AppError> {
    if level.is_empty() {
        return properties_mut(document, page_id)
            .ok_or_else(|| AppError::Other("the page has no /Properties to write".to_string()));
    }
    let form_id = form_id_at(document, page_id, level)?;
    let resources = private_form_resources(document, form_id)?;
    if !resources.has(b"Properties") {
        resources.set(b"Properties", Object::Dictionary(Dictionary::new()));
    }
    resources
        .get_mut(b"Properties")
        .ok()
        .and_then(|entry| entry.as_dict_mut().ok())
        .ok_or_else(|| AppError::Other("the clone has no /Properties to write".to_string()))
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
    level: &[XObjectStep],
    operations: &mut [Operation],
    op_index: usize,
    replacement: Option<&str>,
    named: &HashMap<Vec<u8>, Dictionary>,
) -> Result<(bool, Option<PropertyClone>), AppError> {
    let Some(bdc_index) = enclosing_bdc(operations, op_index) else {
        return Ok((true, None));
    };
    let operation = operations.get(bdc_index).ok_or(AppError::TextRunMismatch)?;

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
        level,
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
    level: &[XObjectStep],
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
        let properties = properties_mut_for_level(document, page_id, level).ok()?;
        fresh_property_name(properties, original_name)?
    };
    let clone_object = document.add_object(clone);
    properties_mut_for_level(document, page_id, level)
        .ok()?
        .set(new_name.clone(), Object::Reference(clone_object));
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

/// Where the page's `/Properties` dictionary actually lives.
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

/// One level's copy of a Form XObject, recorded so undo can take it back out (P2.7.3 §7).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct XObjectClone {
    /// Enclosing level: empty for the page, else the path to the container.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parent_path: Vec<XObjectStep>,
    /// The `Do` the edit repointed, and the names before/after.
    pub do_index: usize,
    pub old_name: Vec<u8>,
    pub new_name: Vec<u8>,
    /// The clone object, removed on undo so no orphan accumulates.
    pub clone_object: ObjectId,
    /// The private `/Resources` installed on the clone, if the edit made one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cloned_resources: Option<ObjectId>,
}

/// Clones every level of `path`, outermost first, and repoints each enclosing `Do` at its clone
/// (P2.7.3 §5, ruling Q2).
///
/// The **draw** is the edit unit: cloning the outermost form makes the copy reachable only from
/// the `Do` that was clicked, so every other draw of the same form — on this page or another —
/// keeps the original bytes. Cloning an inner level alone would leave the outer form shared, and
/// the edit would leak into the outer form's other draws.
///
/// Nothing is mutated until that step's form, name and `Do` all resolve, and a name is only ever
/// added to a resources dictionary the clone owns (Q3/§5.1) — so a form or a page that shares one
/// resources object is never written through.
pub fn clone_draw(
    document: &mut Document,
    page_id: ObjectId,
    path: &[XObjectStep],
) -> Result<Vec<XObjectClone>, AppError> {
    let mut clones = Vec::with_capacity(path.len());
    let mut parent_path: Vec<XObjectStep> = Vec::new();
    for step in path {
        let stream = {
            let stack = resource_stack(document, page_id, &parent_path);
            resolve_form(document, &stack, &step.name)
                .ok_or(AppError::TextRunMismatch)?
                .clone()
        };
        let fresh = {
            let existing = enclosing_xobject_names(document, page_id, &parent_path)?;
            fresh_xobject_name(&existing, &step.name)
        };

        let clone_object = document.add_object(Object::Stream(stream));
        install_xobject_name(document, page_id, &parent_path, &fresh, clone_object)?;
        repoint_do(
            document,
            page_id,
            &parent_path,
            step.op_index,
            &step.name,
            &fresh,
        )?;

        clones.push(XObjectClone {
            parent_path: parent_path.clone(),
            do_index: step.op_index,
            old_name: step.name.clone(),
            new_name: fresh.clone(),
            clone_object,
            cloned_resources: None,
        });
        parent_path.push(XObjectStep {
            name: fresh,
            op_index: step.op_index,
        });
    }
    Ok(clones)
}

/// The form a `Do` operand names, through the resources in force at one level.
fn resolve_form<'a>(
    document: &'a Document,
    stack: &'a [Dictionary],
    name: &[u8],
) -> Option<&'a lopdf::Stream> {
    stack.iter().rev().find_map(|level| {
        let entry = level.get(b"XObject").ok()?;
        let xobjects = document.dereference(entry).ok()?.1.as_dict().ok()?;
        let object = xobjects.get(name).ok()?;
        let (_, resolved) = document.dereference(object).ok()?;
        let stream = resolved.as_stream().ok()?;
        let subtype = stream.dict.get(b"Subtype").ok()?.as_name().ok()?;
        (subtype == b"Form").then_some(stream)
    })
}

/// `<base>_edit<N>` for the smallest `N` the enclosing `/XObject` dictionary does not hold.
///
/// Mirrors [`fresh_property_name`]: the name is the address, so a fresh one must be derivable
/// from the address alone, without trusting an object id (`renumber_objects` rewrites those).
fn fresh_xobject_name(existing: &[Vec<u8>], base: &[u8]) -> Vec<u8> {
    (1..=999)
        .map(|suffix| {
            let mut candidate = base.to_vec();
            candidate.extend_from_slice(b"_edit");
            candidate.extend_from_slice(suffix.to_string().as_bytes());
            candidate
        })
        .find(|candidate| !existing.iter().any(|name| name == candidate))
        .expect("999 fresh names is more than a draw needs")
}

/// The resource names the enclosing `/XObject` dictionary declares.
fn enclosing_xobject_names(
    document: &Document,
    page_id: ObjectId,
    parent_path: &[XObjectStep],
) -> Result<Vec<Vec<u8>>, AppError> {
    let Some(resources) = enclosing_resources(document, page_id, parent_path) else {
        return Ok(Vec::new());
    };
    let Some(xobjects) = resources
        .get(b"XObject")
        .ok()
        .and_then(|entry| document.dereference(entry).ok())
        .and_then(|(_, object)| object.as_dict().ok())
    else {
        return Ok(Vec::new());
    };
    Ok(xobjects.iter().map(|(name, _)| name.clone()).collect())
}

/// The resources dictionary in force at one level of the descent, if it declares one.
fn enclosing_resources(
    document: &Document,
    page_id: ObjectId,
    parent_path: &[XObjectStep],
) -> Option<Dictionary> {
    if parent_path.is_empty() {
        return effective_resources(document, page_id);
    }
    let parent_path_len = parent_path.len();
    let stack = resource_stack(document, page_id, &parent_path[..parent_path_len - 1]);
    resolve_form(document, &stack, &parent_path[parent_path_len - 1].name)?
        .dict
        .get(b"Resources")
        .ok()
        .and_then(|object| document.dereference(object).ok())
        .and_then(|(_, object)| object.as_dict().ok())
        .cloned()
}

/// Installs a fresh `/XObject` name at one level, giving that level a private resources
/// dictionary first (ruling B-7 / Q3).
fn install_xobject_name(
    document: &mut Document,
    page_id: ObjectId,
    parent_path: &[XObjectStep],
    name: &[u8],
    clone_object: ObjectId,
) -> Result<(), AppError> {
    // Read the current map from the enclosing level, then write it back through the level's
    // writable resources. `/XObject` is routinely an indirect object, and writing the name
    // through the reference would mutate a dictionary other readers may share.
    let mut xobjects = enclosing_resources(document, page_id, parent_path)
        .and_then(|resources| {
            let entry = resources.get(b"XObject").ok()?;
            document
                .dereference(entry)
                .ok()
                .and_then(|(_, object)| object.as_dict().ok())
                .cloned()
        })
        .unwrap_or_default();
    xobjects.set(name, Object::Reference(clone_object));
    writable_resources_at(document, page_id, parent_path)?
        .set(b"XObject", Object::Dictionary(xobjects));
    Ok(())
}

/// The resources dictionary a write at one level may mutate, private to that level.
///
/// The page's own dictionary is handled by [`writable_resources_mut`], which copies what the
/// page inherits or shares. A form level has no such inheritance to resolve — the *level* is the
/// form — so its clone is given a private copy of its `/Resources` (or a new dictionary) the
/// first time an edit touches it (Q3).
pub(crate) fn writable_resources_at<'a>(
    document: &'a mut Document,
    page_id: ObjectId,
    parent_path: &[XObjectStep],
) -> Result<&'a mut Dictionary, AppError> {
    if parent_path.is_empty() {
        return writable_resources_mut(document, page_id);
    }
    let form_id = form_id_at(document, page_id, parent_path)?;
    private_form_resources(document, form_id)
}

/// The `/Resources` object a form in a path points at, if it points at an indirect one.
///
/// Used to tell "the clone's dictionary is now its own" from "the clone still shares the
/// original's" after a write: the write path creates the private copy only when the edit
/// actually needs it (Q3), so the record has to be observed, not predicted.
pub(crate) fn form_resources_id(
    document: &Document,
    page_id: ObjectId,
    path: &[XObjectStep],
) -> Option<ObjectId> {
    let form_id = form_id_at(document, page_id, path).ok()?;
    form_dictionary(document, form_id)?
        .get(b"Resources")
        .ok()?
        .as_reference()
        .ok()
}

/// The object id of the form a (clone) path names at its innermost level.
pub(crate) fn form_id_at(
    document: &Document,
    page_id: ObjectId,
    path: &[XObjectStep],
) -> Result<ObjectId, AppError> {
    let (last, parent) = path.split_last().ok_or(AppError::TextRunMismatch)?;
    let stack = resource_stack(document, page_id, parent);
    let entry = stack
        .iter()
        .rev()
        .find_map(|level| {
            let xobjects = level.get(b"XObject").ok()?;
            document
                .dereference(xobjects)
                .ok()?
                .1
                .as_dict()
                .ok()?
                .get(&last.name)
                .ok()
        })
        .ok_or(AppError::TextRunMismatch)?;
    entry.as_reference().map_err(|_| AppError::TextRunMismatch)
}

/// True when more than one dictionary in the document names `resources_id` as its
/// `/Resources`, or when nothing does.
///
/// A clone starts out sharing its original's `/Resources` object, so "is it mine" has to be
/// answered by who points at it, not by whether it is indirect — the same reasoning §5.1 uses
/// for pages.
fn resources_is_shared(document: &Document, resources_id: ObjectId) -> bool {
    resources_holders(document, resources_id) != 1
}

/// How many dictionaries name `resources_id` as their `/Resources`.
fn resources_holders(document: &Document, resources_id: ObjectId) -> usize {
    document
        .objects
        .values()
        .filter_map(|object| object.as_dict().ok())
        .filter(|dict| {
            dict.get(b"Resources")
                .ok()
                .and_then(|resources| resources.as_reference().ok())
                == Some(resources_id)
        })
        .count()
}

/// Gives a form a private `/Resources` object, returning it for writing.
///
/// Called only when an edit touches the form's resources — a font substitution or a named
/// `/Properties` clone (Q3). The copy is unconditional at that point rather than share-counted
/// on the *entry*: a resources dictionary is small, and failing to notice a second reader would
/// recreate B-7 one level down. Idempotent, so two legs of one edit do not leave an orphan.
///
/// The source dictionary is whichever shape the form declares. A form may write `/Resources`
/// as an indirect reference (what real producers do, and what
/// `xobject-shared-resources.pdf` models) or **inline**, which is legal and is what every
/// other §8 fixture does. Both are copied whole: the font substitution replaces one `/Font`
/// entry, and `/XObject`, `/Properties`, `/ExtGState` and every other key survive verbatim.
/// Copying only the reference form and falling back to an empty dictionary silently dropped
/// those siblings from the clone, leaving `Do` operations pointing at names the private
/// dictionary no longer defined.
pub(crate) fn private_form_resources(
    document: &mut Document,
    form_id: ObjectId,
) -> Result<&mut Dictionary, AppError> {
    let declared = form_dictionary(document, form_id).and_then(|form| form.get(b"Resources").ok());
    let referenced = declared.and_then(|object| object.as_reference().ok());
    if let Some(resources_id) = referenced {
        if !resources_is_shared(document, resources_id) {
            return document
                .get_dictionary_mut(resources_id)
                .map_err(|error| AppError::Other(format!("form resources unreadable: {error}")));
        }
    }
    let copy = match (referenced, declared) {
        (Some(resources_id), _) => document
            .get_dictionary(resources_id)
            .map_err(|error| AppError::Other(format!("form resources unreadable: {error}")))?
            .clone(),
        (None, Some(Object::Dictionary(inline))) => inline.clone(),
        (None, Some(_)) => {
            return Err(AppError::Other(
                "form resources are neither a reference nor a dictionary".to_string(),
            ))
        }
        (None, None) => Dictionary::new(),
    };
    let resources_id = document.add_object(copy);
    form_dictionary_mut(document, form_id)
        .ok_or_else(|| AppError::Other("form object unreadable".to_string()))?
        .set(b"Resources", Object::Reference(resources_id));
    document
        .get_dictionary_mut(resources_id)
        .map_err(|error| AppError::Other(format!("form resources unreadable: {error}")))
}

/// A form's dictionary — the `/Subtype /Form` object is a **stream**, so its keys live on
/// `stream.dict`, not on the object itself.
pub(crate) fn form_dictionary(document: &Document, form_id: ObjectId) -> Option<&Dictionary> {
    document
        .get_object(form_id)
        .ok()
        .and_then(|object| match object {
            Object::Stream(stream) => Some(&stream.dict),
            other => other.as_dict().ok(),
        })
}

fn form_dictionary_mut(document: &mut Document, form_id: ObjectId) -> Option<&mut Dictionary> {
    document
        .get_object_mut(form_id)
        .ok()
        .and_then(|object| match object {
            Object::Stream(stream) => Some(&mut stream.dict),
            other => other.as_dict_mut().ok(),
        })
}

/// Removes a `/XObject` name at one level, **without** copying the dictionary.
///
/// Undo removes a name the edit itself installed, so it must not re-run the private-copy rule:
/// that would leave a fresh orphan on every undo. [`install_xobject_name`] writes the map inline
/// in the level's own `/Resources`, so the entry is reachable by a plain lookup.
pub(crate) fn remove_xobject_name(
    document: &mut Document,
    page_id: ObjectId,
    parent_path: &[XObjectStep],
    name: &[u8],
    expected: ObjectId,
) {
    let Some(xobjects) = level_resources_mut(document, page_id, parent_path)
        .and_then(|resources| resources.get_mut(b"XObject").ok())
        .and_then(|entry| entry.as_dict_mut().ok())
    else {
        return;
    };
    if xobjects
        .get(name)
        .ok()
        .and_then(|entry| entry.as_reference().ok())
        == Some(expected)
    {
        xobjects.remove(name);
    }
}

/// Mutable access to one level's own resources dictionary, inline or indirect, without copying.
fn level_resources_mut<'a>(
    document: &'a mut Document,
    page_id: ObjectId,
    parent_path: &[XObjectStep],
) -> Option<&'a mut Dictionary> {
    let holder = if parent_path.is_empty() {
        page_id
    } else {
        form_id_at(document, page_id, parent_path).ok()?
    };
    let referenced = document
        .get_dictionary(holder)
        .ok()?
        .get(b"Resources")
        .ok()?
        .as_reference()
        .ok();
    match referenced {
        Some(resources_id) => document.get_dictionary_mut(resources_id).ok(),
        None => document
            .get_dictionary_mut(holder)
            .ok()?
            .get_mut(b"Resources")
            .ok()?
            .as_dict_mut()
            .ok(),
    }
}

/// True when any object in the document still references `id`.
pub(crate) fn object_is_referenced(document: &Document, id: ObjectId) -> bool {
    fn scan(object: &Object, id: ObjectId) -> bool {
        match object {
            Object::Reference(candidate) => *candidate == id,
            Object::Array(items) => items.iter().any(|item| scan(item, id)),
            Object::Dictionary(dict) => dict.iter().any(|(_, value)| scan(value, id)),
            Object::Stream(stream) => stream.dict.iter().any(|(_, value)| scan(value, id)),
            _ => false,
        }
    }
    document
        .objects
        .iter()
        .any(|(key, object)| *key != id && scan(object, id))
        || document.trailer.iter().any(|(_, value)| scan(value, id))
}

/// Points one level's `Do` operand at the clone's name.
fn repoint_do(
    document: &mut Document,
    page_id: ObjectId,
    parent_path: &[XObjectStep],
    do_index: usize,
    old_name: &[u8],
    new_name: &[u8],
) -> Result<(), AppError> {
    let mut content = read_level(document, page_id, parent_path)?;
    let operation = content
        .operations
        .get_mut(do_index)
        .ok_or(AppError::TextRunMismatch)?;
    if operation.operator != "Do"
        || operation.operands.first().and_then(|o| o.as_name().ok()) != Some(old_name)
    {
        return Err(AppError::TextRunMismatch);
    }
    *operation
        .operands
        .first_mut()
        .ok_or(AppError::TextRunMismatch)? = Object::Name(new_name.to_vec());
    write_level(document, page_id, parent_path, &content)
}

/// One level's decoded content: the page's own stream, or a form clone's.
pub(crate) fn read_level(
    document: &Document,
    page_id: ObjectId,
    path: &[XObjectStep],
) -> Result<lopdf::content::Content<Vec<Operation>>, AppError> {
    if path.is_empty() {
        return decoded_content(document, page_id);
    }
    let form_id = form_id_at(document, page_id, path)?;
    let stream = document
        .get_object(form_id)
        .ok()
        .and_then(|object| object.as_stream().ok())
        .ok_or(AppError::TextRunMismatch)?;
    decode_stream(stream)
}

/// Replaces one level's content.
///
/// A form clone addressed by its own object id is written with `change_content_stream`, never
/// `change_page_content`: the latter addresses a *page* dictionary, so on a stream it returns
/// `Err(ObjectType)` and on a dictionary whose `/Contents` is an unexpected type it returns
/// `Ok(())` having written nothing (design §5.4).
pub(crate) fn write_level(
    document: &mut Document,
    page_id: ObjectId,
    path: &[XObjectStep],
    content: &lopdf::content::Content<Vec<Operation>>,
) -> Result<(), AppError> {
    let encoded = content
        .encode()
        .map_err(|error| AppError::Other(format!("could not re-encode content: {error}")))?;
    if path.is_empty() {
        return document
            .change_page_content(page_id, encoded)
            .map_err(|error| AppError::Other(format!("could not replace page content: {error}")));
    }
    let form_id = form_id_at(document, page_id, path)?;
    let is_stream = document
        .get_object(form_id)
        .ok()
        .and_then(|object| object.as_stream().ok())
        .is_some();
    if !is_stream {
        return Err(AppError::Other(format!(
            "form {form_id:?} is not a stream, so its content cannot be written"
        )));
    }
    document.change_content_stream(form_id, encoded);
    Ok(())
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
    stack.into_iter().rev().find(|index| *index != usize::MAX)
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

/// Points every governing `Tf` a merged run reads through at `name`, recording each swap.
///
/// A document may repeat `Tf F1 12` between fragments, so the same `Tf` index can govern two
/// of them; it is swapped once and recorded once.
fn swap_fonts(
    content: &mut lopdf::content::Content<Vec<Operation>>,
    fragments: &[FragmentWrite],
    name: &[u8],
    swaps: &mut Vec<FontSwap>,
) -> Result<(), AppError> {
    let mut seen: Vec<usize> = Vec::new();
    for fragment in fragments {
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
        *slot = Object::Name(name.to_vec());
        swaps.push(FontSwap {
            op_index: tf_index,
            operand_path: vec![0],
            old_name,
            new_name: name.to_vec(),
            also: Vec::new(),
        });
    }
    Ok(())
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
    ensure_substitute_resource(document, page_id, &id.xobject_path, base_font)?;

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

/// Installs a MECH-B subset as its own font resource at this level, returning its resource
/// name and object id.
///
/// The name is derived from the written `/BaseFont` (`F` + the six-letter prefix) rather than
/// being constant. Two subsets in one document are different faces; a constant name would make
/// the second edit repaint text the first edit set, which is the MECH-A trade-off applied to a
/// case where it is not acceptable.
///
/// Every object goes in as an indirect reference: `/FontDescriptor` and `/ToUnicode` are named
/// by the font dictionary, and a viewer follows references, not inline copies.
fn install_embedded_resource(
    document: &mut Document,
    page_id: ObjectId,
    level: &[XObjectStep],
    subset: &crate::editor::embed::Subset,
) -> Result<(Vec<u8>, ObjectId), AppError> {
    let program_id = document.add_object(crate::editor::embed::program_stream(subset));
    let descriptor_id = document.add_object(crate::editor::embed::descriptor_dictionary(
        subset, program_id,
    ));
    let cmap_id = document.add_object(crate::editor::embed::cmap_stream(subset));
    let font_id = document.add_object(crate::editor::embed::font_dictionary(
        subset,
        descriptor_id,
        cmap_id,
    ));

    let prefix = subset
        .base_font
        .split('+')
        .next()
        .filter(|prefix| prefix.len() == 6)
        .ok_or_else(|| AppError::Other("a subset name must carry a six-letter prefix".into()))?;
    let name = format!("F{prefix}").into_bytes();

    let resources = writable_resources_at(document, page_id, level)?;
    if !resources.has(b"Font") {
        resources.set(b"Font", Object::Dictionary(Dictionary::new()));
    }
    let fonts = resources
        .get_mut(b"Font")
        .and_then(Object::as_dict_mut)
        .map_err(|_| {
            AppError::Other("page /Font map is an indirect form this edit does not rewrite".into())
        })?;
    if fonts.has(&name) {
        // The same input twice produces the same prefix and the same bytes, so this is the
        // same face: reuse the entry rather than shadowing it with an identical copy.
        if fonts
            .get(&name)
            .ok()
            .and_then(|object| object.as_reference().ok())
            == Some(font_id)
        {
            return Ok((name, font_id));
        }
    }
    fonts.set(name.clone(), Object::Reference(font_id));
    Ok((name, font_id))
}

/// Installs a `/Type0` → `/CIDFontType2` pair for a CID subset (P2.7.7).
///
/// Five objects, not four: a Type0 root, its descendant, a descriptor, the program and the
/// `/ToUnicode` CMap. The resource name still comes from the subset prefix, so two different
/// CID subsets cannot share one — the same reason the simple installer derives its name that
/// way.
fn install_embedded_cid_resource(
    document: &mut Document,
    page_id: ObjectId,
    level: &[XObjectStep],
    subset: &crate::editor::embed::CidSubset,
) -> Result<(Vec<u8>, ObjectId), AppError> {
    let program_id = document.add_object(crate::editor::embed::cid_program_stream(subset));
    let descriptor_id = document.add_object(crate::editor::embed::cid_descriptor_dictionary(
        subset, program_id,
    ));
    let descendant_id = document.add_object(crate::editor::embed::cid_descendant_dictionary(
        subset,
        descriptor_id,
    ));
    let cmap_id = document.add_object(crate::editor::embed::cid_cmap_stream(subset));
    let font_id = document.add_object(crate::editor::embed::cid_font_dictionary(
        subset,
        descendant_id,
        cmap_id,
    ));

    let prefix = subset
        .base_font
        .split('+')
        .next()
        .filter(|prefix| prefix.len() == 6)
        .ok_or_else(|| AppError::Other("a subset name must carry a six-letter prefix".into()))?;
    let name = format!("F{prefix}").into_bytes();

    let resources = writable_resources_at(document, page_id, level)?;
    if !resources.has(b"Font") {
        resources.set(b"Font", Object::Dictionary(Dictionary::new()));
    }
    let fonts = resources
        .get_mut(b"Font")
        .and_then(Object::as_dict_mut)
        .map_err(|_| {
            AppError::Other("page /Font map is an indirect form this edit does not rewrite".into())
        })?;
    if fonts.has(&name)
        && fonts
            .get(&name)
            .ok()
            .and_then(|object| object.as_reference().ok())
            == Some(font_id)
    {
        return Ok((name, font_id));
    }
    fonts.set(name.clone(), Object::Reference(font_id));
    Ok((name, font_id))
}

/// Which face a write leaves the run in.
///
/// Three states, not two booleans: the writer must be able to tell "keep the run's font" from
/// "the caller forgot to say", and MECH-A from MECH-B.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaceWrite {
    Keep,
    Substitute,
    Embed,
    /// P2.7.7: install a `/Type0` → `/CIDFontType2` pair.
    EmbedCid,
}

/// Drops every embedded subset this level no longer draws with.
///
/// A subset costs four objects and a few kilobytes; without this, each re-edit of the same run
/// leaves the previous subset in `/Font` forever, and edit-then-undo leaves one per cycle. The
/// test is "no `Tf` in this level names it", which is exactly the condition under which the
/// objects cannot affect rendering.
///
/// Only faces this module wrote are dropped: a font is ours when its `/BaseFont` carries a
/// six-letter subset prefix **and** it has a `/FontFile2`. A producer's own subset in the same
/// `/Font` map is left alone — it may be referenced from another content stream, and deleting
/// an object that is still in use is the one unrecoverable mistake here.
pub(crate) fn prune_unused_embedded_fonts(
    document: &mut Document,
    page_id: ObjectId,
    level: &[XObjectStep],
    operations: &[Operation],
) {
    let mut used: Vec<Vec<u8>> = Vec::new();
    for operation in operations {
        if operation.operator == "Tf" {
            if let Some(name) = operation.operands.first().and_then(|o| o.as_name().ok()) {
                if !used.iter().any(|candidate| candidate == name) {
                    used.push(name.to_vec());
                }
            }
        }
    }

    let Some(fonts) = level_resources_mut(document, page_id, level)
        .and_then(|resources| resources.get_mut(b"Font").ok())
        .and_then(|object| object.as_dict_mut().ok())
    else {
        return;
    };
    let stale: Vec<(Vec<u8>, ObjectId)> = fonts
        .iter()
        .filter(|(name, _)| !used.iter().any(|candidate| candidate == *name))
        .filter_map(|(name, value)| Some((name.clone(), value.as_reference().ok()?)))
        .collect();

    for (name, font_id) in stale {
        let Ok(font) = document.get_dictionary(font_id) else {
            continue;
        };
        let ours = font
            .get(b"BaseFont")
            .and_then(Object::as_name)
            .ok()
            .is_some_and(|base| {
                base.len() > 7 && base[6] == b'+' && base[..6].iter().all(u8::is_ascii_uppercase)
            })
            && font.has(b"FontDescriptor");
        if !ours {
            continue;
        }
        let descriptor_id = font
            .get(b"FontDescriptor")
            .and_then(Object::as_reference)
            .ok();
        let cmap_id = font.get(b"ToUnicode").and_then(Object::as_reference).ok();
        let program_id = descriptor_id
            .and_then(|id| document.get_dictionary(id).ok())
            .and_then(|descriptor| {
                descriptor
                    .get(b"FontFile2")
                    .and_then(Object::as_reference)
                    .ok()
            });

        if let Some(resources) = level_resources_mut(document, page_id, level) {
            if let Some(map) = resources
                .get_mut(b"Font")
                .ok()
                .and_then(|o| o.as_dict_mut().ok())
            {
                map.remove(&name);
            }
        }
        for id in [Some(font_id), descriptor_id, cmap_id, program_id]
            .into_iter()
            .flatten()
        {
            if !object_is_referenced(document, id) {
                document.objects.remove(&id);
            }
        }
    }
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
    level: &[XObjectStep],
    base_font: &str,
) -> Result<ObjectId, AppError> {
    let mut font_dictionary = lopdf::Dictionary::new();
    font_dictionary.set("Type", Object::Name(b"Font".to_vec()));
    font_dictionary.set("Subtype", Object::Name(b"Type1".to_vec()));
    font_dictionary.set("BaseFont", Object::Name(base_font.as_bytes().to_vec()));
    font_dictionary.set("Encoding", Object::Name(b"WinAnsiEncoding".to_vec()));
    let substitute_id = document.add_object(font_dictionary);

    let resources = writable_resources_at(document, page_id, level)?;
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
///
/// `pub(crate)` because P2.8's content-box resolver follows the same chain for `/CropBox` and
/// `/MediaBox` (§3.2), and a second bound would be a second answer to "how deep is a page tree".
pub(crate) const MAX_PARENT_DEPTH: usize = 64;

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
            Object::Stream(stream) => stream.dict.iter().map(|(_, value)| count(value, id)).sum(),
            _ => 0,
        }
    }
    document
        .objects
        .values()
        .map(|object| count(object, id))
        .sum()
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
    /// The content stream this operation wrote (P2.7.3 X2). Empty is the page's own stream, so
    /// every 0.5.0 inverse keeps its meaning.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub xobject_path: Vec<XObjectStep>,
    /// The draws this edit copied, outermost first — the other leg of the same transaction
    /// (Q6). Empty at page level; undo restores the `Do` and drops each clone.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub xobject_clones: Vec<XObjectClone>,
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
    /// P2.7.5 §3.3: the face is the `/BaseFont` without its subset prefix, so a producer that
    /// re-subsets mid-document does not split a line into per-resource runs.
    #[test]
    fn a_face_is_named_without_its_subset_prefix() {
        let font = crate::editor::testpdf::dictionary_for_test(&[
            ("Type", "Font"),
            ("Subtype", "TrueType"),
            ("BaseFont", "ABCDEE+Monaco"),
        ]);
        assert_eq!(face_name(&font), "Monaco");

        let unnamed = crate::editor::testpdf::dictionary_for_test(&[("Type", "Font")]);
        assert_eq!(face_name(&unnamed), "", "an unnamed face has no identity");
    }

    use super::*;
    use crate::editor::testpdf;
    use lopdf::content::Content;

    /// A Type0 font's `/ToUnicode` maps two-byte codes, and splitting them can produce a
    /// *plausible* string from the wrong bytes.
    ///
    /// The trap fixture maps code `0x0000` to `A` as well as `0002` to `B`, so a byte-split
    /// reader turns the two-byte code `0002` into `AA`. Without that entry both readers agree,
    /// and the test would pass against the broken one too — which is exactly what happened
    /// before this fixture existed.
    #[test]
    fn a_two_byte_code_is_never_decoded_one_byte_at_a_time() {
        let document = testpdf::cid_trap_cmap_page();
        let page = page_id(&document, 0).unwrap();
        let font = font_dictionary_at(&document, page, &[], b"F1").unwrap();

        assert!(
            crate::editor::substitute::has_to_unicode(&document, &font),
            "the trap CMap must parse, or this test proves nothing"
        );
        assert!(
            crate::editor::substitute::decode_simple(&document, &font, &[0x00, 0x02]).is_none(),
            "the one-byte reader must refuse a Type0 CMap outright, not split its codes"
        );
        assert_eq!(
            decode_run_bytes(&document, &font, &[0x00, 0x02]),
            Some(String::from("B")),
            "the Type0 reader produces one character per two-byte code"
        );
    }

    /// A simple font keeps the one-byte reader, and a font with no `/ToUnicode` keeps lopdf's
    /// `/Encoding`: the fallback is what every pre-P2.7.6 document relies on.
    #[test]
    fn a_simple_font_still_decodes_through_its_encoding() {
        let document = testpdf::simple_winansi();
        let page = page_id(&document, 0).unwrap();
        let font = font_dictionary_at(&document, page, &[], b"F1").unwrap();

        assert_eq!(
            decode_run_bytes(&document, &font, b"HI"),
            Some(String::from("HI"))
        );
    }

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
        assert!(
            !runs[0].frame.is_horizontal(),
            "a 90 degree matrix must give the run a turned frame"
        );
        assert_eq!(
            runs[0].frame.advance,
            crate::editor::geometry::Point2::new(0.0, 1.0)
        );
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
        assert_eq!(runs[0].frame, crate::editor::geometry::RunFrame::PAGE);
    }

    /// P2.7 T-7: H0 read rotation from `Tm` only, so a rotation applied by `cm` — which the
    /// renderer composes with `Tm` — was called editable with a horizontal `advance`. That is
    /// editable *and* wrong, the exact failure H0 exists to remove. Rotation must be detected
    /// in the composed frame.
    #[test]
    fn a_graphics_matrix_rotation_is_flagged_on_the_run() {
        let document = testpdf::rotated_by_graphics_matrix();
        let page = page_id(&document, 0).unwrap();
        let runs = locate_runs(&document, page).unwrap();

        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].text, "ROTATED");
        assert!(
            !runs[0].frame.is_horizontal(),
            "a 90 degree `cm` with an identity `Tm` draws vertical text; it must give the run a turned frame"
        );
    }

    /// The control: a `cm` that only translates is not a rotation. Detection must not refuse
    /// text just because the page brackets it in `q`/`cm`/`Q`.
    #[test]
    fn a_translating_graphics_matrix_is_not_flagged_as_rotated() {
        let document = testpdf::translated_by_graphics_matrix();
        let page = page_id(&document, 0).unwrap();
        let runs = locate_runs(&document, page).unwrap();

        assert_eq!(runs.len(), 1);
        assert_eq!(
            runs[0].frame,
            crate::editor::geometry::RunFrame::PAGE,
            "a pure translation is not a rotation"
        );
    }

    /// The CTM is graphics state: `Q` restores what `q` saved. A rotation undone before the
    /// text is shown leaves an axis-aligned run, so the stack must be modelled rather than
    /// treating any `cm` seen earlier on the page as still in force.
    #[test]
    fn a_rotation_restored_by_q_does_not_flag_the_run() {
        let document = testpdf::rotation_restored_before_showing();
        let page = page_id(&document, 0).unwrap();
        let runs = locate_runs(&document, page).unwrap();

        assert_eq!(runs.len(), 1);
        assert!(
            runs[0].frame.is_horizontal(),
            "`Q` restores the identity, so this run is axis-aligned"
        );
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
                    vec![
                        2.into(),
                        0.into(),
                        0.into(),
                        2.into(),
                        72.into(),
                        700.into(),
                    ],
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
        assert_eq!(runs[0].frame, crate::editor::geometry::RunFrame::PAGE);
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
        let raised = runs
            .iter()
            .find(|run| run.text == "Raised")
            .expect("Raised");
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
            xobject_path: Vec::new(),
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

    // --- P2.7.3 X1: discovery across Form XObjects ------------------------------

    /// The reachability the whole phase exists for: text drawn by a form is text on the page,
    /// and the picker must be able to address it. Before X1 the run was simply absent.
    #[test]
    fn locates_runs_inside_a_form_xobject() {
        let document = testpdf::xobject_single();
        let page = page_id(&document, 0).unwrap();
        let runs = locate_runs(&document, page).unwrap();

        let run = runs
            .iter()
            .find(|run| run.text == "ALPHA")
            .expect("text drawn inside a form is still text on the page");
        assert_eq!(run.id.xobject_path.len(), 1, "one step: the page drew X1");
        assert_eq!(run.id.xobject_path[0].name, b"X1");
        assert_eq!(run.font_key, b"F1");
        // The address is the *form's* stream, not the page's: op 3 is its `Tj`.
        assert_eq!(run.id.op_index, 3);
        assert_eq!(run.id.operand_path, vec![0]);
    }

    /// Q1 is only satisfied by a walk that descends level by level: a one-level walk cannot
    /// see `X2` at all, so this fixture is what distinguishes the two readings of the ruling.
    #[test]
    fn a_run_inside_a_nested_form_is_located() {
        let document = testpdf::xobject_nested();
        let page = page_id(&document, 0).unwrap();
        let runs = locate_runs(&document, page).unwrap();

        let run = runs
            .iter()
            .find(|run| run.text == "ALPHA")
            .expect("a doubly nested form still draws text");
        let names: Vec<&[u8]> = run
            .id
            .xobject_path
            .iter()
            .map(|step| step.name.as_slice())
            .collect();
        assert_eq!(
            names,
            vec![&b"X1"[..], &b"X2"[..]],
            "the full path of names"
        );
    }

    /// A form that draws itself is legal to write and hangs a naive walker. The refusal must
    /// be **visible**: a placeholder run at the `Do` that could not be descended, carrying
    /// `nested-xobject`, because text the picker cannot see is the failure this gate removes.
    #[test]
    fn a_cycle_refuses_with_nested_xobject_and_is_visible() {
        let document = testpdf::xobject_cycle();
        let page = page_id(&document, 0).unwrap();
        let runs = locate_runs(&document, page).unwrap();

        let placeholder = runs
            .iter()
            .find(|run| run.text == "/X1")
            .expect("the undescendable `Do` still reports itself");
        assert_eq!(placeholder.operator, "Do");
        assert_eq!(
            placeholder.xobject_refusal,
            Some(XObjectRefusal::Cycle),
            "the placeholder carries the reason"
        );

        let gates = crate::editor::textedit::resolve_gates(&document, 0, placeholder).unwrap();
        let verdict = crate::editor::textedit::font_admissibility(&gates);
        assert!(!verdict.editable);
        assert_eq!(verdict.gate.as_deref(), Some("nested-xobject"));
        assert!(
            verdict
                .message
                .as_deref()
                .is_some_and(|m| m.contains("itself")),
            "the refusal must name the cycle: {:?}",
            verdict.message
        );
    }

    /// The cap: depth 8 is editable, depth 9 is refused. One fixture carries both halves, so
    /// the boundary is proven rather than the existence of a limit.
    #[test]
    fn depth_nine_refuses_with_nested_xobject() {
        let document = testpdf::xobject_depth_nine();
        let page = page_id(&document, 0).unwrap();
        let runs = locate_runs(&document, page).unwrap();

        let edge = runs
            .iter()
            .find(|run| run.text == "EDGE")
            .expect("depth 8 is within the cap and must be located");
        assert_eq!(edge.id.xobject_path.len(), 8);

        let refused = runs
            .iter()
            .find(|run| run.text == "/X9")
            .expect("the ninth level still reports itself");
        assert_eq!(refused.xobject_refusal, Some(XObjectRefusal::Depth));

        let gates = crate::editor::textedit::resolve_gates(&document, 0, refused).unwrap();
        let verdict = crate::editor::textedit::font_admissibility(&gates);
        assert_eq!(verdict.gate.as_deref(), Some("nested-xobject"));
        assert!(
            verdict
                .message
                .as_deref()
                .is_some_and(|m| m.contains("deeply")),
            "the refusal must name the depth: {:?}",
            verdict.message
        );
    }

    /// The address model: an ordinary page run's `xobjectPath` is absent from the JSON, so a
    /// 0.5.0 client sees exactly the payload it saw before.
    #[test]
    fn a_page_run_serialises_without_an_xobject_path() {
        let json = serde_json::to_value(RunId {
            op_index: 3,
            operand_path: vec![0],
            xobject_path: Vec::new(),
        })
        .unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "opIndex": 3, "operandPath": [0] })
        );

        let json = serde_json::to_value(RunId {
            op_index: 3,
            operand_path: vec![0],
            xobject_path: vec![XObjectStep {
                name: b"X1".to_vec(),
                op_index: 7,
            }],
        })
        .unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "opIndex": 3,
                "operandPath": [0],
                "xobjectPath": [{ "name": [88, 49], "opIndex": 7 }],
            })
        );
    }

    /// A form with no `/Resources` resolves every name through the enclosing level — the
    /// per-key fallback of §4, and the reason a whole-dictionary replacement is wrong.
    #[test]
    fn a_form_without_resources_inherits_fonts_and_children() {
        let document = testpdf::xobject_inherited_resources();
        let page = page_id(&document, 0).unwrap();
        let runs = locate_runs(&document, page).unwrap();

        let run = runs
            .iter()
            .find(|run| run.text == "ALPHA")
            .expect("the inherited child form is still descended");
        assert_eq!(
            run.id.xobject_path.len(),
            2,
            "X1 had no resources; X2 came from the page"
        );
        // The font came from the page's `/Font` dictionary, not from X2's own.
        assert_eq!(run.font_key, b"F1");
        assert_eq!(run.font_size, 24.0);
    }

    // --- P2.8 §5: the expectation reader, proved without reflow ----------------------
    //
    // These tests exist to give the reflow guard an oracle that is **not** the writer's own
    // arithmetic. The rule they establish is the one that matters: expected positions are read
    // from PDFium's ink, never from `runmetrics::measure`, whose `bounds.width()` is a *sum* of
    // advances and kerning -- the same quantity the writer computes. Comparing `measure` to a
    // computed position would be the row 6c defect in a new costume.
    //
    // They live here, in the crate, rather than in `tests/`, for one reason: assertion (c) needs
    // `write_level`, which is `pub(crate)`. Widening it to `pub` would put a content-rewriting
    // primitive on the library's public surface purely for a test. A `#[cfg(test)]` module can
    // reach it as it stands.
    //
    // What is proved here is the *reader*. No reflow code exists yet, and none is used: the
    // "hand-built edit" in (c) inserts a `Td` into the decoded operator list and re-encodes,
    // which is what the design says a reflow will eventually do, minus the decision of *where*.

    /// The kern and `Tz` the fixtures were built with, mirrored here so the expectation is
    /// computed from named constants rather than transcribed numbers.
    use crate::editor::testpdf::{KERNED_LINE_KERN, KERNED_LINE_TZ};

    use std::path::Path;

    /// Helvetica `A`-`D` and `W`-`Z` widths, in thousandths of an em, from the AFM table this
    /// build already ships (`editor::afm`). `A`=667 `B`=667 `C`=722 `D`=722,
    /// `W`=944 `X`=667 `Y`=667 `Z`=611.
    ///
    /// Transcribed rather than looked up on purpose: a guard that asked `afm` for the width
    /// would be asking the writer, which is the whole defect these tests avoid. If the AFM table
    /// ever disagrees with this, that disagreement is itself the finding.
    const ADVANCE_A: f32 = 667.0;
    const ADVANCE_B: f32 = 667.0;
    const ADVANCE_C: f32 = 722.0;
    const ADVANCE_D: f32 = 722.0;

    const FIXTURE_FONT_SIZE: f32 = 24.0;
    const FIXTURE_TD_X: f32 = 72.0;
    /// Line 2's `Tm` translation. See `kerned_line_page`.
    const FIXTURE_LINE2_X: f32 = 72.0;

    /// A glyph's pen origin, in page space, with the character it draws.
    ///
    /// `x`/`y` are PDFium's `origin()`, which is the **pen** origin — the quantity a positioning
    /// operator actually moves — not the loose or ink box, whose left edge carries a side
    /// bearing that varies by glyph.
    #[derive(Debug, Clone, Copy, PartialEq)]
    struct GlyphOrigin {
        char: char,
        x: f32,
        y: f32,
    }

    /// Reads per-glyph pen origins out of one committed fixture.
    ///
    /// This is the expectation reader itself: it opens the file with PDFium, takes the character
    /// grid, and returns each character's origin. It **never** calls `measure` — the whole point
    /// is that its answer comes from what the renderer drew, not from what the writer adds up.
    ///
    /// Generated characters are dropped: PDFium synthesises a line break or a word space at a
    /// kerning seam, and those occupy grid slots without being drawn by any run's bytes. Keeping
    /// them would put a phantom glyph in the middle of every kern.
    fn glyph_origins(path: &Path, page: u32) -> Vec<GlyphOrigin> {
        let grid = crate::provider::DocumentHandles::new()
            .with_document(path, None, |document| {
                crate::editor::runmetrics::char_grid(document, page, path)
            })
            .expect("PDFium char grid extraction failed");

        grid.chars
            .iter()
            .filter(|character| !character.generated)
            .filter_map(|character| {
                character.unicode.map(|glyph| GlyphOrigin {
                    char: glyph,
                    x: character.origin_x,
                    y: character.origin_y,
                })
            })
            .collect()
    }

    /// One `TJ` fragment: its per-character AFM widths and the kern that follows it.
    struct Fragment {
        widths: &'static [f32],
        kern_after: f32,
    }

    /// The pen x of every fragment, from 9.4.3 plus 9.4.4.
    ///
    /// `kern_after` is the raw `TJ` number. It is subtracted, so a negative number widens the
    /// gap — the sign the repo states in `push_tj_array` and the one the fixtures rely on.
    fn expected_fragment_origins(
        start: f32,
        fragments: &[Fragment],
        size: f32,
        tz: f32,
    ) -> Vec<f32> {
        let scale = tz / 100.0;
        let mut pen = 0.0_f32;
        let mut out = Vec::new();
        for fragment in fragments {
            out.push(start + pen * scale);
            let thousandths: f32 = fragment.widths.iter().sum();
            pen += thousandths * size / 1000.0;
            pen -= fragment.kern_after / 1000.0 * size;
        }
        out
    }

    fn kerned_fixture_path(name: &str) -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join(name)
    }

    /// The `AB`-then-`CD` line, as 9.4.3 describes it.
    fn kerned_line_fragments() -> [Fragment; 2] {
        [
            Fragment {
                widths: &[ADVANCE_A, ADVANCE_B],
                kern_after: KERNED_LINE_KERN as f32,
            },
            Fragment {
                widths: &[ADVANCE_C, ADVANCE_D],
                kern_after: 0.0,
            },
        ]
    }

    /// Assertion (a): line 1's glyph origins are where its own operators put them.
    ///
    /// Tolerance is `0.05 pt`. PDFium reports through an `f32` and rounds its own layout, so an
    /// exact comparison would fail on representation rather than on a missing factor. The
    /// distinction that matters is scale: the defect under test moves `C` by `16.5 pt` at
    /// `Tz 50`, which is 330x the tolerance.
    fn assert_line_one_origins(name: &str, tz: f32) {
        let path = kerned_fixture_path(name);
        let glyphs = glyph_origins(&path, 0);

        let expected = expected_fragment_origins(
            FIXTURE_TD_X,
            &kerned_line_fragments(),
            FIXTURE_FONT_SIZE,
            tz,
        );

        let a = glyphs
            .iter()
            .find(|glyph| glyph.char == 'A')
            .unwrap_or_else(|| panic!("{name}: no `A` in the grid: {glyphs:?}"));
        let c = glyphs
            .iter()
            .find(|glyph| glyph.char == 'C')
            .unwrap_or_else(|| panic!("{name}: no `C` in the grid: {glyphs:?}"));

        assert!(
            (a.x - expected[0]).abs() < 0.05,
            "{name}: `A` drew at x={:.3}, 9.4.3 says {:.3}",
            a.x,
            expected[0]
        );
        assert!(
            (c.x - expected[1]).abs() < 0.05,
            "{name}: `C` drew at x={:.3}, 9.4.3 says {:.3} (kern {} at Tz {tz})",
            c.x,
            expected[1],
            KERNED_LINE_KERN
        );
    }

    /// Assertion (b): the control line's glyphs are where its own `Tm` puts them.
    fn assert_line_two_origins(name: &str) {
        let path = kerned_fixture_path(name);
        let glyphs = glyph_origins(&path, 0);

        let w = glyphs
            .iter()
            .find(|glyph| glyph.char == 'W')
            .unwrap_or_else(|| panic!("{name}: no `W` in the grid: {glyphs:?}"));

        assert!(
            (w.x - FIXTURE_LINE2_X).abs() < 0.05,
            "{name}: control `W` drew at x={:.3}, its `Tm` says {:.3}",
            w.x,
            FIXTURE_LINE2_X
        );
    }

    #[test]
    #[ignore = "requires a real PDFium library; `cargo test -- --include-ignored` after fetching it"]
    fn a_kerned_lines_glyphs_are_where_iso_9_4_3_puts_them() {
        assert_line_one_origins("kerned-line.pdf", 100.0);
    }

    /// Assertion (a) at a horizontal scaling that is not 100.
    ///
    /// At `Tz 100` the walk's kern term is correct by accident: it omits the `Tz/100` factor, and
    /// `x / 100 * 100 == x`. This fixture is what makes that omission falsifiable.
    ///
    /// Note what the expectation does **not** do: it does not consult the reader's pen walk. The
    /// `Tz/100` is applied here to a hand-computed text-space sum, so the two implementations
    /// are independent and either can be wrong without the other agreeing.
    #[test]
    #[ignore = "requires a real PDFium library; `cargo test -- --include-ignored` after fetching it"]
    fn a_kerned_lines_glyphs_are_where_iso_9_4_3_puts_them_at_tz_50() {
        assert_line_one_origins("kerned-line-tz.pdf", KERNED_LINE_TZ as f32);
    }

    /// T-8: the reader's own pen against the ink, at a scaling that is not 100.
    ///
    /// R10.2 measured that deleting the walk's kern term outright leaves every existing
    /// kerned-line test green: they compare hand-computed §9.4.3 arithmetic against PDFium's
    /// grid and never ask `tj_fragment_origins`. This is the missing comparison, and it is the
    /// only place the walk's `Tz` factor can fail — at `Tz 100` the factor is arithmetically
    /// invisible (`x / 100 * 100 == x`), which is why the suite was green with it missing.
    #[test]
    #[ignore = "requires a real PDFium library; `cargo test -- --include-ignored` after fetching it"]
    fn the_reader_puts_a_kerned_tail_where_pdfium_drew_it_at_tz_50() {
        let path = kerned_fixture_path("kerned-line-tz.pdf");
        let document = Document::load(&path).expect("fixture loads");
        let page = page_id(&document, 0).expect("page 0");
        let runs = locate_runs(&document, page).expect("runs");
        let cd = runs.iter().find(|run| run.text == "CD").unwrap_or_else(|| {
            panic!(
                "no `CD` run: {:?}",
                runs.iter().map(|run| &run.text).collect::<Vec<_>>()
            )
        });

        let glyphs = glyph_origins(&path, 0);
        let c = glyphs
            .iter()
            .find(|glyph| glyph.char == 'C')
            .unwrap_or_else(|| panic!("no `C` in the grid: {glyphs:?}"));

        let fragments = kerned_line_fragments();
        let scaled = expected_fragment_origins(
            FIXTURE_TD_X,
            &fragments,
            FIXTURE_FONT_SIZE,
            KERNED_LINE_TZ as f32,
        )[1];
        let unscaled =
            expected_fragment_origins(FIXTURE_TD_X, &fragments, FIXTURE_FONT_SIZE, 100.0)[1];

        // Precondition, in R10.2's shape: if the scaling moved `CD` by less than the tolerance,
        // a reader that omitted the factor would pass here exactly as it passes everywhere else.
        assert!(
            (scaled - unscaled).abs() > 0.05,
            "`Tz {KERNED_LINE_TZ}` moves `CD` only {:.3} pt from its un-scaled position, inside \
             the tolerance — this fixture could not tell the two readings apart",
            scaled - unscaled
        );
        assert!(
            (c.x - scaled).abs() < 0.05,
            "PDFium drew `C` at x={:.3}; §9.4.3 at `Tz {KERNED_LINE_TZ}` says {scaled:.3}",
            c.x
        );

        assert!(
            (cd.page_origin.0 - c.x).abs() < 0.05,
            "the reader puts `CD` at x={:.3} (text space {:.3}), PDFium drew it at {:.3} — off \
             by {:.3} pt, {}x the 0.05 pt tolerance",
            cd.page_origin.0,
            cd.origin.0,
            c.x,
            cd.page_origin.0 - c.x,
            (cd.page_origin.0 - c.x).abs() / 0.05
        );
    }

    /// T-8 at unit level: the walk's kern term carries `Tz`.
    ///
    /// The PDFium sibling above is the acceptance test the plan names, but it is `#[ignore]`d and
    /// therefore absent from the Linux job — and this is a defect in the *reader*, which every
    /// consumer of [`tj_fragment_origins`] inherits, not in the reflow. So the same property is
    /// asserted here against a hand-built array and text state, where it is cheap and always
    /// runs.
    ///
    /// The numbers are the fixture's own: `AB` at 24 pt with a `-250` kern, at `Tz 50`. The
    /// factor is falsifiable only because the scaling is not 100 — at `Tz 100` both readings
    /// return the same number, which is why the defect survived until now.
    #[test]
    fn the_kern_term_carries_the_horizontal_scale() {
        let mut state = TextMatrixWalk::new();
        for operation in [
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec!["F1".into(), FIXTURE_FONT_SIZE.into()]),
            Operation::new("Tz", vec![KERNED_LINE_TZ.into()]),
            Operation::new("Td", vec![FIXTURE_TD_X.into(), 700.into()]),
        ] {
            state.step(&operation);
        }
        let array = Operation::new(
            "TJ",
            vec![Object::Array(vec![
                Object::string_literal("AB"),
                Object::Integer(KERNED_LINE_KERN),
                Object::string_literal("CD"),
            ])],
        );
        let metrics = crate::editor::substitute::FontMetrics::resolve_with(
            &Document::new(),
            &Dictionary::new(),
            Some("Helvetica"),
        );
        let decode = |bytes: &[u8]| Some(String::from_utf8_lossy(bytes).into_owned());

        let origins = tj_fragment_origins(&array, &state, &metrics, &decode);
        assert_eq!(origins.len(), 2, "one origin per string in the array");

        // 72 + (667 + 667) x 24/1000 x 0.5, then the kern *subtracted* — so a negative number
        // widens the gap by 250/1000 x 24 x 0.5 = 3.000, giving 91.008.
        let scale = KERNED_LINE_TZ as f32 / 100.0;
        let expected = FIXTURE_TD_X + (ADVANCE_A + ADVANCE_B) * FIXTURE_FONT_SIZE / 1000.0 * scale
            - (KERNED_LINE_KERN as f32 / 1000.0) * FIXTURE_FONT_SIZE * scale;
        assert!(
            (origins[1].1 .0 - expected).abs() < 1e-3,
            "the tail starts at {:.3}; the scaled kern puts it at {expected:.3} — an unscaled \
             kern would report {:.3}",
            origins[1].1 .0,
            expected + (KERNED_LINE_KERN as f32 / 1000.0) * FIXTURE_FONT_SIZE * (1.0 - scale)
        );
    }

    #[test]
    #[ignore = "requires a real PDFium library; `cargo test -- --include-ignored` after fetching it"]
    fn the_control_line_is_where_its_tm_puts_it() {
        assert_line_two_origins("kerned-line.pdf");
    }

    /// Assertion (c): a hand-built operator edit moves line 1 and leaves line 2 alone.
    ///
    /// The edit is exactly what a reflow will eventually build and nothing more: decode the
    /// page's operators, insert one `Td` between the two `TJ` strings — splitting the array so
    /// `CD` is shown from a new pen origin — re-encode through `write_level`, save, and re-read
    /// the ink.
    ///
    /// Why a split rather than a shifted `TJ` number: a kern number displaces the pen inside the
    /// array and cannot move `CD` *rightwards* past its own glyph advances without also changing
    /// the text. A `Td` between two arrays is the general form, and it is the shape §2.2 of the
    /// design says the inverse model must be able to emit.
    ///
    /// The assertions are the two halves of an edit's contract:
    ///   * line 1's `C` moves by exactly the `Td` delta;
    ///   * line 2's `W` does not move at all.
    ///
    /// Both are read from PDFium ink after a round trip through a file, so this also proves the
    /// write path is not silently dropping the second array or the control line's `Tm`.
    #[test]
    #[ignore = "requires a real PDFium library; `cargo test -- --include-ignored` after fetching it"]
    fn a_hand_built_td_moves_line_one_and_leaves_line_two_alone() {
        const SHIFT: f32 = 40.0;

        let source = kerned_fixture_path("kerned-line.pdf");
        let mut document = Document::load(&source).expect("fixture loads");
        let page_id = page_id(&document, 0).expect("page 0");

        let before = glyph_origins(&source, 0);
        let c_before = before.iter().find(|glyph| glyph.char == 'C').expect("C");
        let w_before = before.iter().find(|glyph| glyph.char == 'W').expect("W");

        // The hand-built edit, on the decoded operator list.
        let mut content = read_level(&document, page_id, &[]).expect("page content");
        let array_index = content
            .operations
            .iter()
            .position(|operation| operation.operator == "TJ")
            .expect("a TJ array on line 1");
        let Operation { operands, .. } = content.operations.remove(array_index);
        let Object::Array(items) = operands.into_iter().next().expect("an array operand") else {
            panic!("TJ operand was not an array");
        };
        // Rebuild as two arrays with a `Td` between them. `CD` keeps its kern-implied start: the
        // `Td` supplies the whole displacement, so the kern must not be applied twice.
        let ab = items.first().cloned().expect("AB");
        let cd = items.get(2).cloned().expect("CD");
        // `Td` translates the **line matrix**, not the pen: `tm` and `tlm` are set to
        // `translate(tx,ty) x tlm` (ISO 32000-1 9.4.2). So a `Td` inserted after `AB` discards the
        // advance `AB` just made, and the offset must be measured from the *line* origin, not
        // from the pen. Getting this wrong is exactly how a reflow loses the preceding run's
        // width, so the fixture-level expectation below is derived from the line origin.
        let text_space_cd = expected_fragment_origins(
            FIXTURE_TD_X,
            &kerned_line_fragments(),
            FIXTURE_FONT_SIZE,
            100.0,
        )[1];
        let td_x = (text_space_cd - FIXTURE_TD_X) + SHIFT;

        let rebuilt = vec![
            Operation::new("TJ", vec![Object::Array(vec![ab])]),
            Operation::new("Td", vec![td_x.into(), 0.into()]),
            Operation::new("TJ", vec![Object::Array(vec![cd])]),
        ];
        content.operations.splice(array_index..array_index, rebuilt);
        write_level(&mut document, page_id, &[], &content).expect("write page content");

        let dir = tempfile::tempdir().expect("temp dir");
        let edited = dir.path().join("kerned-line-edited.pdf");
        document.save(&edited).expect("save edited document");

        let after = glyph_origins(&edited, 0);
        let c_after = after.iter().find(|glyph| glyph.char == 'C').expect("C");
        let w_after = after.iter().find(|glyph| glyph.char == 'W').expect("W");

        assert!(
            (c_after.x - (c_before.x + SHIFT)).abs() < 0.05,
            "line 1's `C` moved {:.3} pt, the `Td` asked for {SHIFT}",
            c_after.x - c_before.x
        );
        assert!(
            (w_after.x - w_before.x).abs() < 0.05,
            "line 2's `W` moved {:.3} pt, but nothing edited it",
            w_after.x - w_before.x
        );
    }
}
