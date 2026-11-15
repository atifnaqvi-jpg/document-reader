//! Reading and writing AcroForm field values (P3.3, design D-F1..D-F7).
//!
//! # Why this module exists at all
//!
//! PDFium is the reader and the renderer, and it fills text fields correctly. It does **not** fill
//! buttons: `set_checked()` on a checkbox returns `Ok` and changes nothing (§1.2 of the design,
//! measured). A fill surface that called it would show the user a tick, save, and produce a file
//! that still shows an empty box — a false success, which I-F1 forbids.
//!
//! So the *read* and *write* halves of a fill both go through the document's own dictionaries here,
//! and PDFium is not in this path. That is D-F1: PDFium discovers, this module mutates.
//!
//! # What "read" means, and why it is not the PDFium field
//!
//! D-F3 says a button's state names come from `/AP`. `pdfium-render 0.8.37` cannot supply them:
//! its only `/AP` accessor is `FPDFAnnot_GetAP`, which returns the *selected* appearance stream
//! rather than the key set under `/AP /N`. The names are therefore read from the same lopdf
//! document the write lands in — which is stricter than D-F3 asked for, because a name cannot then
//! differ between what was read and what was written.
//!
//! # Preservation (D-F4)
//!
//! Every mutation here is an **incremental update**: the source bytes are kept verbatim as the
//! first revision and one more revision is appended. Nothing before the append point is rewritten,
//! so "everything we did not write is preserved byte for byte" is a property of the mechanism
//! rather than a promise the writer has to keep carefully. `a_fill_preserves_the_source_bytes_verbatim_as_its_first_revision`
//! asserts it on the actual bytes: the output must start with the input.
//!
//! The alternative — load, mutate, re-serialise — would rewrite every object in the file. That
//! passes a test that compares rendered output and destroys signatures, object streams, and any
//! vendor extension a producer emitted in an order we would not reproduce.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use lopdf::{Document, IncrementalDocument, Object, ObjectId};

use crate::model::AppError;

/// One field in the document's form, as this build understands it.
///
/// `states` is empty for a text field and carries the **on** states for a button, in the order the
/// document lists them. `Off` is deliberately absent from `states`: it is the one state every
/// button has (ISO 32000-1 12.7.4.2.3) and offering it as a "choice" the user picks from a list of
/// *values* would present "off" as a value the field can be filled *with*.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FormField {
    /// The fully qualified name (parent names joined with `.`), which is what a caller writes to.
    pub name: String,
    /// The field's `/FT` as the document spells it: `Tx`, `Btn`, `Ch`, `Sig`, or anything else.
    pub field_type: String,
    /// `/Ff`, raw. The UI reads individual bits; this module exposes the number so a flag this
    /// build does not model is still visible to a caller that does.
    pub flags: i64,
    /// The current `/V` as text, or `None` when the document carries no `/V`.
    ///
    /// A name object (`/Yes`) is rendered without its slash, so a caller comparing against
    /// [`states`](Self::states) compares like with like.
    pub value: Option<String>,
    /// The on-states of a button, from `/AP /N` of every widget in the field. Empty otherwise.
    pub states: Vec<String>,
    /// `/Opt` of a choice field, as text. Empty otherwise.
    pub options: Vec<String>,
    /// `/I` of a choice field: the zero-based selected indices. Empty otherwise.
    pub selected_indices: Vec<i64>,
    /// Widget rectangles in PDF points, `[x0, y0, x1, y1]`, one per widget. The UI places its
    /// input controls from these.
    pub rects: Vec<[f32; 4]>,
    /// `/Ff` bit 1. A read-only field is listed (the user must see it) but cannot be filled.
    pub read_only: bool,
    /// `/Ff` bit 17 for a button — the field is a radio group rather than a set of checkboxes.
    pub radio: bool,
}

impl FormField {
    /// Whether this build can write a value into this field.
    ///
    /// `Sig` is excluded because a signature is not a fill; `Tx`, `Btn` and `Ch` are the three
    /// the design covers. A type outside them is still *listed* so the UI can show it as
    /// present-but-unsupported rather than silently omitting a field the user can see in another
    /// viewer.
    pub fn is_fillable(&self) -> bool {
        !self.read_only && matches!(self.field_type.as_str(), "Tx" | "Btn" | "Ch")
    }
}

/// Everything this module needs to know about one field's dictionary, resolved.
///
/// Held separately from [`FormField`] because the *write* needs the object ids and the parent
/// chain, and those are not part of the public shape the frontend sees.
struct FieldLocations {
    /// The field dictionary that carries `/FT`/`/V` — the parent for a radio group, else the
    /// widget itself.
    field_id: ObjectId,
    /// Every widget dictionary of the field, which is where `/AS` must be written.
    widget_ids: Vec<ObjectId>,
}

/// A form whose fields were read successfully, plus where each one lives.
pub struct OpenForm {
    source_bytes: Vec<u8>,
    /// Where the bytes came from, when they came from a file. `enforceable_level` reads `/DocMDP`
    /// through PDFium as a fallback when the catalog's own `/Perms` entry is absent, and that fallback
    /// needs a path. `None` for [`open_bytes`], where the bytes are the whole story.
    source_path: Option<PathBuf>,
    fields: Vec<FormField>,
    locations: BTreeMap<String, FieldLocations>,
}

impl std::fmt::Debug for OpenForm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenForm")
            .field("fields", &self.fields)
            .field("source_len", &self.source_bytes.len())
            .finish_non_exhaustive()
    }
}

impl OpenForm {
    /// The fields, in the order the document's `/Fields` array lists them.
    pub fn fields(&self) -> &[FormField] {
        &self.fields
    }

    /// One field by fully qualified name.
    pub fn field(&self, name: &str) -> Option<&FormField> {
        self.fields.iter().find(|field| field.name == name)
    }
}

/// Reads the document at `path` and enumerates its form fields.
///
/// Refuses an XFA form by name (D-F6): an XFA document's values live in the XML packet, so filling
/// the `/AcroForm` half would produce a file that other viewers show unchanged. An encrypted
/// document is refused by the caller's read-only guard, not here — this function's job is forms.
pub fn open(path: &Path) -> Result<OpenForm, AppError> {
    let bytes = std::fs::read(path)
        .map_err(|error| AppError::Other(format!("could not read PDF: {error}")))?;
    let mut form = open_bytes(bytes)?;
    form.source_path = Some(path.to_path_buf());
    Ok(form)
}

/// [`open`] over bytes already in memory.
pub fn open_bytes(source_bytes: Vec<u8>) -> Result<OpenForm, AppError> {
    let document = Document::load_mem(&source_bytes)
        .map_err(|error| AppError::Other(format!("could not open PDF: {error}")))?;

    if has_xfa(&document)? {
        return Err(AppError::XfaFormUnsupported);
    }

    let mut fields = Vec::new();
    let mut locations = BTreeMap::new();
    collect_fields(&document, None, &mut fields, &mut locations)?;

    Ok(OpenForm {
        source_bytes,
        source_path: None,
        fields,
        locations,
    })
}

/// Whether the catalog's `/AcroForm` carries `/XFA`.
///
/// `/XFA` may be an array of alternating name/stream pairs rather than a single stream, so its
/// presence is what matters and its shape does not: either spelling means the form is dynamic.
fn has_xfa(document: &Document) -> Result<bool, AppError> {
    let Some(form) = document
        .catalog()
        .ok()
        .and_then(|catalog| catalog.get(b"AcroForm").ok())
        .and_then(|form| match form {
            Object::Reference(id) => document.get_object(*id).ok(),
            other => Some(other),
        })
        .and_then(|form| form.as_dict().ok())
    else {
        return Ok(false);
    };
    Ok(form.has(b"XFA"))
}

/// Walks `/AcroForm /Fields`, descending through `/Kids` and joining names with `.`.
///
/// `inherited` carries the parent's `/T` segments so a kid's qualified name is built without a
/// second pass, and also which `/FT` applies: a field's type may sit on an ancestor (ISO 32000-1
/// 12.7.3.1), which is how a radio group's `/FT /Btn` reaches its widgets.
fn collect_fields(
    document: &Document,
    inherited: Option<Inherited>,
    fields: &mut Vec<FormField>,
    locations: &mut BTreeMap<String, FieldLocations>,
) -> Result<(), AppError> {
    let start = match inherited {
        None => {
            let catalog = document
                .catalog()
                .map_err(|error| AppError::Other(format!("this PDF has no catalog: {error}")))?;
            let form = catalog
                .get(b"AcroForm")
                .ok()
                .and_then(|form| match form {
                    Object::Reference(id) => document.get_object(*id).ok(),
                    other => Some(other),
                })
                .and_then(|form| form.as_dict().ok());
            let Some(form) = form else {
                // A document with no form is not an error: it simply has no fields to fill.
                return Ok(());
            };
            match form.get(b"Fields").and_then(Object::as_array) {
                Ok(entries) => entries.clone(),
                Err(_) => return Ok(()),
            }
        }
        Some(ref inherited) => inherited.children.clone(),
    };

    for entry in start {
        let Object::Reference(id) = entry else {
            continue;
        };
        let Some(dictionary) = document.get_object(id).ok().and_then(|o| o.as_dict().ok()) else {
            continue;
        };

        let inherited = Inherited {
            name: inherited
                .as_ref()
                .map(|parent| parent.name.clone())
                .unwrap_or_default(),
            field_type: dictionary
                .get(b"FT")
                .ok()
                .and_then(|ft| ft.as_name().ok())
                .map(|ft| String::from_utf8_lossy(ft).to_string())
                .or_else(|| {
                    inherited
                        .as_ref()
                        .and_then(|parent| parent.field_type.clone())
                }),
            flags: dictionary
                .get(b"Ff")
                .ok()
                .and_then(|ff| ff.as_i64().ok())
                .or_else(|| inherited.as_ref().and_then(|parent| parent.flags)),
            children: Vec::new(),
        };

        // The qualified name: a segment with no `/T` contributes nothing, which is how a widget
        // kid of a named parent ends up with the parent's name rather than an empty segment.
        let mut name = inherited.name.clone();
        if let Ok(title) = dictionary.get(b"T").and_then(|t| t.as_str()) {
            let segment = decode_text_string(title);
            if !segment.is_empty() {
                if !name.is_empty() {
                    name.push('.');
                }
                name.push_str(&segment);
            }
        }

        let kids: Vec<Object> = dictionary
            .get(b"Kids")
            .and_then(Object::as_array)
            .cloned()
            .unwrap_or_default();

        // Whether this dictionary is itself a widget: an annotation with a `/Subtype` and a
        // `/Rect`. A field may be merged with its single widget (the common case), in which case
        // it is both the field and the widget and there is nothing to descend into.
        let is_widget = dictionary
            .get(b"Subtype")
            .and_then(|subtype| subtype.as_name())
            .map(|subtype| subtype == b"Widget")
            .unwrap_or(false)
            || dictionary.has(b"Rect");

        if !kids.is_empty() {
            // Descend. `Kids` may mix widgets (radio group kids) with real sub-fields (a
            // hierarchical name), and only the recursion can tell them apart.
            let child_inherited = Inherited {
                name: name.clone(),
                field_type: inherited.field_type.clone(),
                flags: inherited.flags,
                children: kids,
            };
            let before = fields.len();
            collect_fields(document, Some(child_inherited), fields, locations)?;
            if fields.len() == before && !name.is_empty() {
                // A parent whose kids produced nothing is itself a field with no widget this
                // build can place; listing it would put a nameless box in the UI.
                continue;
            }
            continue;
        }

        if name.is_empty() {
            continue;
        }

        let field = describe_field(document, id, &name, &inherited, is_widget);
        let widget_ids = if is_widget { vec![id] } else { Vec::new() };

        // A field is identified by its fully qualified name, so two dictionaries carrying the same
        // name are the same field. This is not a tidiness rule: a radio group's parent holds `/FT`,
        // `/Ff` and `/V` while each kid holds one option's `/AP /N`, so the recursion reaches the
        // option names through dictionaries that all inherit the parent's name. Pushing them as
        // separate fields lists the group once per kid and shows exactly one option -- the bug the
        // radio test caught.
        //
        // `field_id` stays the dictionary that carries `/FT`, because that is where `/V` is
        // written; the widgets are where `/AS` goes, and there are several of those.
        let locations_entry = locations
            .entry(name.clone())
            .or_insert_with(|| FieldLocations {
                field_id: id,
                widget_ids: Vec::new(),
            });
        let field_id = locations_entry.field_id;
        for widget in widget_ids {
            if !locations_entry.widget_ids.contains(&widget) {
                locations_entry.widget_ids.push(widget);
            }
        }

        match fields.iter_mut().find(|existing| existing.name == name) {
            Some(existing) => merge_field(existing, field),
            None => fields.push(field),
        }

        // `/V` is written to the dictionary that carries `/FT`. A radio group's kids inherit `/FT`
        // from their parent and carry only `/AP`, so the parent's id must win even though the kids
        // were reached later; taking the first id that *has* `/FT` is the rule that gets both
        // cases right, because a kid never has one.
        let carries_type = document
            .get_object(id)
            .ok()
            .and_then(|object| object.as_dict().ok())
            .map(|dictionary| dictionary.has(b"FT"))
            .unwrap_or(false);
        let previous_carries_type = document
            .get_object(field_id)
            .ok()
            .and_then(|object| object.as_dict().ok())
            .map(|dictionary| dictionary.has(b"FT"))
            .unwrap_or(false);
        if carries_type && !previous_carries_type {
            if let Some(entry) = locations.get_mut(&name) {
                entry.field_id = id;
            }
        }
    }

    Ok(())
}

/// What a child inherits from its ancestors while [`collect_fields`] descends.
struct Inherited {
    name: String,
    field_type: Option<String>,
    flags: Option<i64>,
    children: Vec<Object>,
}

/// Folds a second view of the same field into the first.
///
/// Two dictionaries share a name only when they are parts of one field (a parent and its widgets),
/// so every disagreement here is a *layer*, not a conflict: the parent's `/V` and the kid's states
/// are both wanted, and neither replaces the other. States and rects accumulate; the layer reached
/// first already carries `/V`, which is why it is kept when both have one.
fn merge_field(into: &mut FormField, from: FormField) {
    for state in from.states {
        if !into.states.contains(&state) {
            into.states.push(state);
        }
    }
    for rect in from.rects {
        if !into.rects.contains(&rect) {
            into.rects.push(rect);
        }
    }
    for option in from.options {
        if !into.options.contains(&option) {
            into.options.push(option);
        }
    }
    if into.value.is_none() {
        into.value = from.value;
    }
    if into.selected_indices.is_empty() {
        into.selected_indices = from.selected_indices;
    }
    into.read_only |= from.read_only;
    into.radio |= from.radio;
    if into.field_type == "unknown" {
        into.field_type = from.field_type;
    }
    into.flags |= from.flags;
}

/// Builds the public [`FormField`] for one dictionary.
fn describe_field(
    document: &Document,
    id: ObjectId,
    name: &str,
    inherited: &Inherited,
    is_widget: bool,
) -> FormField {
    let dictionary = document
        .get_object(id)
        .ok()
        .and_then(|object| object.as_dict().ok());

    let field_type = inherited
        .field_type
        .clone()
        .unwrap_or_else(|| "unknown".to_string());
    let flags = inherited.flags.unwrap_or(0);

    let value = dictionary
        .and_then(|dictionary| dictionary.get(b"V").ok())
        .and_then(object_text);

    let mut states = Vec::new();
    let mut rects = Vec::new();
    if field_type == "Btn" {
        // Read the on-states from `/AP /N`, of every widget in the field.
        let widgets: Vec<ObjectId> = if is_widget {
            vec![id]
        } else {
            document
                .get_object(id)
                .ok()
                .and_then(|object| object.as_dict().ok())
                .and_then(|dictionary| dictionary.get(b"Kids").and_then(Object::as_array).ok())
                .map(|kids| {
                    kids.iter()
                        .filter_map(|kid| kid.as_reference().ok())
                        .collect()
                })
                .unwrap_or_default()
        };
        for widget in widgets {
            let Some(widget_dict) = document
                .get_object(widget)
                .ok()
                .and_then(|object| object.as_dict().ok())
            else {
                continue;
            };
            if let Some(rect) = widget_dict.get(b"Rect").ok().and_then(rect_of) {
                rects.push(rect);
            }
            for state in appearance_states(widget_dict) {
                if state != "Off" && !states.contains(&state) {
                    states.push(state);
                }
            }
        }
    } else if is_widget {
        if let Some(rect) = dictionary
            .and_then(|dictionary| dictionary.get(b"Rect").ok())
            .and_then(rect_of)
        {
            rects.push(rect);
        }
    }

    let options: Vec<String> = dictionary
        .and_then(|dictionary| dictionary.get(b"Opt").ok())
        .and_then(|opt| opt.as_array().ok())
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| match entry {
                    // `/Opt` entries may be `(text)` or an array of `[export value, display text]`;
                    // the display text is what a user picks from.
                    Object::Array(pair) => pair.last().and_then(object_text),
                    other => object_text(other),
                })
                .collect()
        })
        .unwrap_or_default();

    let selected_indices: Vec<i64> = dictionary
        .and_then(|dictionary| dictionary.get(b"I").ok())
        .and_then(|indices| indices.as_array().ok())
        .map(|indices| {
            indices
                .iter()
                .filter_map(|index| index.as_i64().ok())
                .collect()
        })
        .unwrap_or_default();

    FormField {
        name: name.to_string(),
        field_type,
        flags,
        value,
        states,
        options,
        selected_indices,
        rects,
        read_only: flags & 1 != 0,
        radio: flags & 32768 != 0,
    }
}

/// The on-state key names under `/AP /N`.
///
/// `/N` is either a single stream (one appearance, no states — a text field or a push button) or a
/// dictionary whose keys are the state names. Only the dictionary form has states, so a stream
/// yields none, which is the honest answer: a push button has no value to write.
fn appearance_states(widget: &lopdf::Dictionary) -> Vec<String> {
    let Some(normal) = widget
        .get(b"AP")
        .and_then(Object::as_dict)
        .ok()
        .and_then(|ap| ap.get(b"N").ok())
    else {
        return Vec::new();
    };
    match normal {
        Object::Reference(_) => Vec::new(), // a single stream: no states to enumerate
        Object::Dictionary(states) => states
            .iter()
            .map(|(key, _)| String::from_utf8_lossy(key).to_string())
            .collect(),
        _ => Vec::new(),
    }
}

/// A `/Rect` as `[x0, y0, x1, y1]` in points, normalised so `x0 <= x1` and `y0 <= y1`.
///
/// A rectangle is allowed to be written either way round, and a producer that writes it
/// top-to-bottom produces a rect whose components look negative to anyone who assumes order. The
/// UI draws an outline from these, so normalising here is cheaper than in every consumer.
fn rect_of(object: &Object) -> Option<[f32; 4]> {
    let values = object.as_array().ok()?;
    if values.len() != 4 {
        return None;
    }
    let mut numbers = [0.0f32; 4];
    for (slot, value) in numbers.iter_mut().zip(values) {
        *slot = value.as_float().ok()?;
    }
    Some([
        numbers[0].min(numbers[2]),
        numbers[1].min(numbers[3]),
        numbers[0].max(numbers[2]),
        numbers[1].max(numbers[3]),
    ])
}

/// The text of a string or name object.
///
/// A name loses its leading slash because `/V /Yes` and `/V (Yes)` are the same claim about the
/// field's value, and a caller comparing against [`FormField::states`] must not have to know which
/// spelling the producer chose.
fn object_text(object: &Object) -> Option<String> {
    match object {
        Object::String(bytes, _) => Some(decode_text_string(bytes)),
        Object::Name(bytes) => Some(String::from_utf8_lossy(bytes).to_string()),
        _ => None,
    }
}

/// Decodes a PDF text string, honouring the UTF-16BE BOM that a producer uses for non-ASCII.
///
/// ISO 32000-1 7.9.2: a string is PDFDocEncoding unless it starts with `FE FF`, which makes it
/// big-endian UTF-16. Returning the raw bytes for the non-BOM case is deliberate — PDFDocEncoding
/// agrees with Latin-1 in the ASCII range, which is every field name this build writes, and
/// guessing further would invent characters for bytes we cannot map.
fn decode_text_string(bytes: &[u8]) -> String {
    if bytes.starts_with(&[0xFE, 0xFF]) {
        let units: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|pair| u16::from_be_bytes([pair[0], pair[1]]))
            .collect();
        String::from_utf16_lossy(&units)
    } else {
        String::from_utf8_lossy(bytes).to_string()
    }
}

/// One value the caller wants written into one field.
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FillValue {
    /// The fully qualified field name, as [`FormField::name`] reports it.
    pub name: String,
    /// For `/Tx`, the text. For `/Btn`, the on-state name. For `/Ch`, the option text.
    pub value: String,
}

/// The result of a successful fill.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FillOutcome {
    /// Where the filled copy was written.
    pub path: String,
    /// How many fields were actually written.
    pub filled: usize,
}

/// Writes `values` into the form and saves an incrementally-appended copy at `target`.
///
/// `exists` is the caller's collision probe, so this module does not touch the filesystem to
/// decide a name — the same discipline `save::next_edited_path` uses.
///
/// # The order of operations, and why
///
/// 1. **Every value is validated against the document before anything is written.** A fill that
///    half-succeeded would leave the caller with a file changed in ways they did not ask for, and
///    I-F2 forbids writing a name or value the document does not carry. So an unknown field name, a
///    read-only field, or a value outside a field's legal set refuses the whole fill and no file is
///    produced.
/// 2. **The source is preserved as revision one.** The write is an incremental append (D-F4), so
///    every byte before the append is the original's.
/// 3. **The result is re-opened and each field read back** (I-F1). A write that reports success and
///    did not stick is the specific failure this phase exists to prevent, and PDFium's
///    `set_checked` returning `Ok` while changing nothing is the measured precedent for it.
pub fn fill_to<F>(
    source: &Path,
    target: &Path,
    values: &[FillValue],
    exists: F,
) -> Result<FillOutcome, AppError>
where
    F: Fn(&Path) -> bool,
{
    let mut form = open(source)?;
    form.fill_and_save(target, values, exists)
}

impl OpenForm {
    /// Validates, writes, and verifies. See [`fill_to`] for the order and why.
    fn fill_and_save<F>(
        &mut self,
        target: &Path,
        values: &[FillValue],
        exists: F,
    ) -> Result<FillOutcome, AppError>
    where
        F: Fn(&Path) -> bool,
    {
        if values.is_empty() {
            return Err(AppError::Other(
                "no form values were supplied, so there is nothing to fill".to_string(),
            ));
        }

        // 0. The certification gate, before anything else touches the document. The 2026-11-14
        //    ruling permits form fill at `P=2` and `P=3` and refuses it at `P=1`, so a certified
        //    document is not refused wholesale -- only the level that forbids filling is. Called
        //    first rather than last because the refusal has to name the level before the user has
        //    had a file written, and because a `P=1` document must not be validated against either:
        //    the answer is no regardless of what the values are.
        let level = crate::editor::signature::verify::enforceable_level(
            &self.source_bytes,
            self.source_path.as_deref().unwrap_or_else(|| Path::new("")),
        );
        crate::editor::signature::editpolicy::check(
            level,
            crate::editor::signature::editpolicy::EditKind::FormFill,
        )?;

        // 1. Validate everything first, so a refusal leaves no half-written file.
        let mut writes = Vec::new();
        for value in values {
            writes.push(self.validate(value)?);
        }

        // 2. Append one revision. `create_from` keeps the source bytes verbatim as revision one.
        let bytes = self.append_revision(&writes)?;

        // 3. Re-open the *saved bytes* and read each field back. Verifying against `self.document`
        //    would prove nothing: it is the in-memory document the write was built in, so a bug that
        //    wrote to the wrong object id would still read back correctly there. The claim being
        //    made is about the file, so the check is made on the file.
        let readback = open_bytes(bytes.clone())?;
        verify_writes(&readback, &writes)?;

        let final_path = if exists(target) {
            crate::editor::save::next_edited_path(target, exists)?
        } else {
            target.to_path_buf()
        };
        crate::editor::save::write_atomically(&final_path, &bytes)?;

        Ok(FillOutcome {
            path: final_path.to_string_lossy().to_string(),
            filled: writes.len(),
        })
    }

    /// Checks one requested value against this document, or refuses.
    fn validate(&self, value: &FillValue) -> Result<PlannedWrite, AppError> {
        let field = self
            .field(&value.name)
            .ok_or_else(|| AppError::FormFieldNotFound {
                name: value.name.clone(),
            })?;

        if field.read_only {
            return Err(AppError::FormFieldReadOnly {
                name: value.name.clone(),
            });
        }

        let locations =
            self.locations
                .get(&value.name)
                .ok_or_else(|| AppError::FormFieldNotFound {
                    name: value.name.clone(),
                })?;

        match field.field_type.as_str() {
            "Tx" => Ok(PlannedWrite {
                name: value.name.clone(),
                kind: WriteKind::Text(value.value.clone()),
                expected: Expected::Text(value.value.clone()),
                field_id: locations.field_id,
                widget_ids: locations.widget_ids.clone(),
            }),
            "Btn" => {
                // D-F3/I-F2: the legal states are the ones the document's `/AP /N` names. `Off` is
                // always writable even though it is not listed, because clearing a checkbox is a
                // legitimate fill and its state name is fixed by the format rather than chosen.
                let legal =
                    value.value == "Off" || field.states.iter().any(|state| state == &value.value);
                if !legal {
                    let mut allowed = vec!["Off".to_string()];
                    allowed.extend(field.states.iter().cloned());
                    return Err(AppError::FormFieldValueNotAllowed {
                        value: value.value.clone(),
                        allowed: allowed.join(", "),
                    });
                }
                Ok(PlannedWrite {
                    name: value.name.clone(),
                    kind: WriteKind::Button(value.value.clone()),
                    expected: Expected::Button(value.value.clone()),
                    field_id: locations.field_id,
                    widget_ids: locations.widget_ids.clone(),
                })
            }
            "Ch" => {
                // A combo may be edited (`/Ff` bit 19), in which case a value outside `/Opt` is
                // legal; otherwise the option list is the whole legal set.
                let editable = field.flags & 262_144 != 0;
                let index = field
                    .options
                    .iter()
                    .position(|option| option == &value.value);
                if index.is_none() && !editable {
                    return Err(AppError::FormFieldValueNotAllowed {
                        value: value.value.clone(),
                        allowed: field.options.join(", "),
                    });
                }
                Ok(PlannedWrite {
                    name: value.name.clone(),
                    kind: WriteKind::Choice {
                        value: value.value.clone(),
                        index: index.map(|index| index as i64),
                    },
                    expected: Expected::Text(value.value.clone()),
                    field_id: locations.field_id,
                    widget_ids: locations.widget_ids.clone(),
                })
            }
            other => Err(AppError::Other(format!(
                "the field \"{}\" is of type /{other}, which this build cannot fill",
                value.name
            ))),
        }
    }

    /// Builds the appended revision and serialises the source plus that revision.
    fn append_revision(&self, writes: &[PlannedWrite]) -> Result<Vec<u8>, AppError> {
        let previous = Document::load_mem(&self.source_bytes)
            .map_err(|error| AppError::Other(format!("could not open PDF: {error}")))?;
        let mut incremental = IncrementalDocument::create_from(self.source_bytes.clone(), previous);

        // The catalog must be staged before any field, because `/NeedAppearances` lands on it and a
        // field's own parent chain is reached through it. Staging twice is a no-op (`opt_clone`
        // checks), so the order is safe to state in one place.
        let catalog_id = incremental
            .get_prev_documents()
            .trailer
            .get(b"Root")
            .and_then(Object::as_reference)
            .map_err(|_| AppError::Other("this document has no catalog".to_string()))?;

        for write in writes {
            incremental
                .opt_clone_object_to_new_document(write.field_id)
                .map_err(|error| {
                    AppError::Other(format!("could not stage the field dictionary: {error}"))
                })?;
            for widget in &write.widget_ids {
                incremental
                    .opt_clone_object_to_new_document(*widget)
                    .map_err(|error| {
                        AppError::Other(format!("could not stage the widget dictionary: {error}"))
                    })?;
            }

            // The field dictionary carries `/V`; the widgets carry `/AS`. They are the same object
            // for a merged field, which is why both are set through the same lookup rather than
            // assuming two distinct ids.
            let field_dictionary = incremental
                .new_document
                .get_object_mut(write.field_id)
                .and_then(Object::as_dict_mut)
                .map_err(|_| AppError::Other("the field is not a dictionary".to_string()))?;
            write.apply_to_field(field_dictionary);

            for widget in &write.widget_ids {
                let widget_dictionary = incremental
                    .new_document
                    .get_object_mut(*widget)
                    .and_then(Object::as_dict_mut)
                    .map_err(|_| AppError::Other("the widget is not a dictionary".to_string()))?;
                write.apply_to_widget(widget_dictionary);
            }
        }

        set_need_appearances(&mut incremental, catalog_id)?;

        let mut bytes = Vec::new();
        incremental.save_to(&mut bytes).map_err(|error| {
            AppError::Other(format!("could not append the fill revision: {error}"))
        })?;
        Ok(bytes)
    }
}

/// Sets `/AcroForm /NeedAppearances true` in the appended revision.
///
/// `/NeedAppearances` tells a viewer to regenerate the appearances itself. It is written because
/// our `/AP` update is a *choice* of the existing streams, not a redraw: a text field's appearance
/// stream still draws the old value, so a viewer that trusted `/AP` verbatim would show stale ink.
/// Setting the flag is the format's way of saying "recompute these", and it is what makes the tick
/// appear without this build shipping a text layout engine.
fn set_need_appearances(
    incremental: &mut IncrementalDocument,
    catalog_id: ObjectId,
) -> Result<(), AppError> {
    incremental
        .opt_clone_object_to_new_document(catalog_id)
        .map_err(|error| AppError::Other(format!("could not stage the catalog: {error}")))?;

    // An `/AcroForm` may be a direct dictionary or a reference. A referenced form lives in the
    // previous revision, so writing through the reference would edit an object this revision does
    // not contain and the change would be dropped on save. Copy it forward and write it inline.
    let previous = incremental.get_prev_documents();
    let previous_form = previous
        .get_object(catalog_id)
        .ok()
        .and_then(|catalog| catalog.as_dict().ok())
        .and_then(|catalog| catalog.get(b"AcroForm").ok())
        .and_then(|form| match form {
            Object::Reference(id) => previous
                .get_object(*id)
                .ok()
                .and_then(|form| form.as_dict().ok())
                .cloned(),
            other => other.as_dict().ok().cloned(),
        })
        .unwrap_or_default();

    let mut form = previous_form;
    form.set("NeedAppearances", Object::Boolean(true));

    let catalog = incremental
        .new_document
        .get_object_mut(catalog_id)
        .and_then(Object::as_dict_mut)
        .map_err(|_| AppError::Other("the catalog is not a dictionary".to_string()))?;
    catalog.set("AcroForm", Object::Dictionary(form));
    Ok(())
}

/// I-F1: every write must be visible in a re-read of the saved bytes, or the fill fails.
///
/// A free function rather than a loop inside [`OpenForm::fill_and_save`] so the refusal can be
/// tested against a readback that deliberately disagrees with a write. Inline, the only way to reach
/// this branch would be a writer bug, so the guard would be untestable and its absence invisible —
/// which is exactly how PDFium's `set_checked` shipped returning `Ok` over an unchanged file.
fn verify_writes(readback: &OpenForm, writes: &[PlannedWrite]) -> Result<(), AppError> {
    for write in writes {
        let Some(field) = readback.field(&write.name) else {
            return Err(AppError::FormFillDidNotPersist {
                name: write.name.clone(),
                found: "no such field".to_string(),
            });
        };
        if !write.satisfied_by(field) {
            return Err(AppError::FormFillDidNotPersist {
                name: write.name.clone(),
                found: field
                    .value
                    .clone()
                    .map(|value| format!("{value:?}"))
                    .unwrap_or_else(|| "no value".to_string()),
            });
        }
    }
    Ok(())
}

/// A validated write, ready to be applied.
struct PlannedWrite {
    name: String,
    kind: WriteKind,
    expected: Expected,
    field_id: ObjectId,
    widget_ids: Vec<ObjectId>,
}

enum WriteKind {
    Text(String),
    Button(String),
    Choice { value: String, index: Option<i64> },
}

/// What a read-back has to show for the write to count as persisted.
enum Expected {
    Text(String),
    Button(String),
}

impl PlannedWrite {
    /// `/V` on the field dictionary.
    fn apply_to_field(&self, dictionary: &mut lopdf::Dictionary) {
        match &self.kind {
            WriteKind::Text(text) | WriteKind::Choice { value: text, .. } => {
                // `Object::string_literal`, not `Object::from(&str)`: `From<String>` produces a
                // *name*, so the obvious spelling writes `/V /hello` instead of `/V (hello)`.
                dictionary.set("V", Object::string_literal(text.as_bytes()));
            }
            WriteKind::Button(state) => {
                // `/V /Off` and `/V /Yes` are names, not strings — the format says so, and a reader
                // comparing names to `/AP` keys would not match a string.
                dictionary.set("V", Object::Name(state.as_bytes().to_vec()));
            }
        }
        if let WriteKind::Choice { index, .. } = &self.kind {
            // `/I` and `/V` have to agree: a viewer shows the one its own logic prefers, so a
            // multi-select list with a fresh `/V` and a stale `/I` highlights the old row.
            match index {
                Some(index) => dictionary.set("I", Object::Array(vec![Object::Integer(*index)])),
                None => {
                    dictionary.remove(b"I");
                }
            }
        }
    }

    /// `/AS` on the widget dictionary, for a button.
    ///
    /// A text field's `/AS` is not a thing: `/AS` selects an appearance stream's state, which only a
    /// button has. Writing it there would add a key no reader consults.
    ///
    /// A widget takes the requested state **only when its own `/AP /N` draws it**; otherwise it is
    /// set `Off`. A radio group is why. Its kids each name one option (`/red`, `/blue`), so writing
    /// `/AS /blue` to every kid tells the red kid to show a stream its `/AP` does not contain — the
    /// measured result is a group with the tick under both options. `Off` is always a legal `/AS`
    /// because every button's appearance dictionary carries an `/Off` entry.
    fn apply_to_widget(&self, dictionary: &mut lopdf::Dictionary) {
        let WriteKind::Button(state) = &self.kind else {
            return;
        };
        let drawable = state == "Off" || appearance_states(dictionary).iter().any(|s| s == state);
        let written = if drawable { state.as_str() } else { "Off" };
        dictionary.set("AS", Object::Name(written.as_bytes().to_vec()));
    }

    /// Whether a re-read field shows what was written.
    fn satisfied_by(&self, field: &FormField) -> bool {
        match &self.expected {
            Expected::Text(text) => field.value.as_deref() == Some(text.as_str()),
            // A checked button is shown by its `/V`. The `/AS` is what the pixels come from and is
            // asserted separately, by the rendered-pixel gate; `/V` is the claim this reads.
            Expected::Button(state) => field.value.as_deref() == Some(state.as_str()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::testpdf;
    use lopdf::dictionary;

    fn open_fixture(bytes: Vec<u8>) -> OpenForm {
        open_bytes(bytes).expect("the fixture opens")
    }

    fn fixture_bytes(document: Document) -> Vec<u8> {
        testpdf::bytes(&document)
    }

    /// The three button spellings must each be read back as the document wrote them.
    ///
    /// This is the read half of D-F3. A writer that got its state names from a constant would pass
    /// a test against one fixture; asserting all three here is what makes the constant fail.
    #[test]
    fn every_button_spelling_is_read_from_the_documents_ap_dictionary() {
        for (spelling, build) in [
            ("Yes", testpdf::form_checkbox_yes as fn() -> Document),
            ("On", testpdf::form_checkbox_on),
            ("yes", testpdf::form_checkbox_lowercase_yes),
        ] {
            let form = open_fixture(fixture_bytes(build()));
            let field = form
                .field("agree")
                .unwrap_or_else(|| panic!("the {spelling} fixture has a field named agree"));
            assert_eq!(
                field.states,
                vec![spelling.to_string()],
                "the on-state of the {spelling} fixture must be read from /AP /N, not assumed"
            );
            assert_eq!(field.field_type, "Btn");
            assert_eq!(field.value.as_deref(), Some("Off"));
            assert!(!field.radio, "a lone checkbox is not a radio group");
        }
    }

    /// A radio group's states come from its kids, and `/Ff` bit 16 comes from its parent.
    ///
    /// Reading the parent alone yields no states at all — which is exactly the bug this asserts
    /// against, so the test names it.
    #[test]
    fn a_radio_groups_states_are_the_union_of_its_kids_appearances() {
        let form = open_fixture(fixture_bytes(testpdf::form_radio_group()));
        let field = form.field("color").expect("the radio group is named color");
        assert_eq!(field.field_type, "Btn");
        assert!(
            field.radio,
            "/Ff bit 16 on the parent is what makes this a radio group rather than two checkboxes"
        );
        assert_eq!(
            field.states,
            vec!["red".to_string(), "blue".to_string()],
            "each option name comes from its own kid widget's /AP /N"
        );
        assert_eq!(field.rects.len(), 2, "one rect per kid widget");
    }

    /// `Off` is never offered as a value to fill with.
    ///
    /// It is a state the writer has to be able to *write* (to clear a checkbox), but it is not a
    /// choice from a list of the field's values, and mixing the two makes "the value you picked"
    /// indistinguishable from "nothing is selected".
    #[test]
    fn the_off_state_is_not_listed_as_a_fill_value() {
        let form = open_fixture(fixture_bytes(testpdf::form_checkbox_yes()));
        let field = form.field("agree").expect("field");
        assert!(
            !field.states.iter().any(|state| state == "Off"),
            "Off is every button's unselected state, not a value the user picks: {:?}",
            field.states
        );
    }

    #[test]
    fn text_fields_carry_their_type_flags_and_rects() {
        let form = open_fixture(fixture_bytes(testpdf::form_text_fields()));
        let names: Vec<&str> = form.fields().iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, vec!["given", "notes"]);

        let given = form.field("given").expect("given");
        assert_eq!(given.field_type, "Tx");
        assert_eq!(given.value, None, "an empty field carries no /V");
        assert!(given.is_fillable());
        assert_eq!(given.rects.len(), 1);

        let notes = form.field("notes").expect("notes");
        assert_eq!(
            notes.flags & 4096,
            4096,
            "the multi-line fixture must set /Ff bit 13, or the multi-line path is untested"
        );
    }

    #[test]
    fn choice_fields_expose_their_options_and_selected_indices() {
        let form = open_fixture(fixture_bytes(testpdf::form_choice_fields()));

        let pick = form.field("pick").expect("the combo");
        assert_eq!(pick.field_type, "Ch");
        assert_eq!(pick.options, vec!["alpha", "bravo", "charlie"]);
        assert_eq!(pick.value, None, "the combo starts empty");
        assert!(pick.selected_indices.is_empty());

        let many = form.field("many").expect("the list");
        assert_eq!(many.flags & 2_097_152, 2_097_152, "multi-select");
        assert_eq!(many.selected_indices, vec![1]);
        assert_eq!(
            many.value.as_deref(),
            Some("bravo"),
            "/I and /V must agree, or a write that updated one would look correct here"
        );
    }

    #[test]
    fn a_read_only_field_is_listed_but_not_fillable() {
        let form = open_fixture(fixture_bytes(testpdf::form_read_only_field()));
        let locked = form.field("locked").expect("locked");
        assert!(locked.read_only, "/Ff bit 1");
        assert!(
            !locked.is_fillable(),
            "I-F4: a read-only field must be refused, so it must not read as fillable"
        );
    }

    /// D-F6: an XFA form is refused by name rather than half-filled.
    #[test]
    fn an_xfa_form_is_refused_rather_than_partially_filled() {
        let error = open_bytes(fixture_bytes(testpdf::form_with_xfa()))
            .expect_err("an XFA form must not open for filling");
        assert_eq!(
            error.code(),
            "XFA_FORM_UNSUPPORTED",
            "the refusal must name itself, not surface as a generic parse failure"
        );
    }

    /// A document with no form is not an error; it has no fields.
    #[test]
    fn a_document_without_a_form_has_no_fields() {
        let form = open_fixture(fixture_bytes(testpdf::blank_page()));
        assert!(form.fields().is_empty());
    }

    /// I-F1, the phase's first invariant: a fill that reports success has to be readable back.
    ///
    /// Written as a loop over every field type because the failure this guards is not one bug. A
    /// writer can get `/V` right and `/I` wrong, or a button right and a choice wrong, and a single
    /// happy-path test would pass while the other two lie to the user.
    #[test]
    fn every_field_type_reads_back_the_value_that_was_written() {
        type Case = (&'static str, fn() -> Document, &'static str, &'static str);
        let cases: [Case; 4] = [
            ("text", testpdf::form_text_fields, "given", "Ada Lovelace"),
            (
                "multiline",
                testpdf::form_text_fields,
                "notes",
                "line one\nline two",
            ),
            ("checkbox", testpdf::form_checkbox_on, "agree", "On"),
            ("choice", testpdf::form_choice_fields, "pick", "bravo"),
        ];

        for (label, build, field, value) in cases {
            let dir = tempfile::tempdir().expect("temp dir");
            let source = dir.path().join("source.pdf");
            let target = dir.path().join("filled.pdf");
            std::fs::write(&source, testpdf::bytes(&build())).expect("write fixture");

            let outcome = fill_to(
                &source,
                &target,
                &[FillValue {
                    name: field.to_string(),
                    value: value.to_string(),
                }],
                |path| path.exists(),
            )
            .unwrap_or_else(|error| panic!("{label} fill failed: {error}"));

            assert_eq!(outcome.filled, 1, "{label}");
            let readback = open(&std::path::PathBuf::from(&outcome.path))
                .unwrap_or_else(|error| panic!("{label} readback failed: {error}"));
            assert_eq!(
                readback.field(field).and_then(|f| f.value.clone()),
                Some(value.to_string()),
                "{label}: the saved file must carry the value that was written"
            );
        }
    }

    /// D-F4: everything the writer did not write is preserved byte for byte.
    ///
    /// Asserted on the actual bytes rather than on a rendered comparison, because the property is
    /// about the file: a load/mutate/re-serialise writer would pass a render check and still rewrite
    /// every object, destroying signatures and vendor extensions the user never touched. The output
    /// starting with the input verbatim is the whole claim.
    #[test]
    fn a_fill_preserves_the_source_bytes_verbatim_as_its_first_revision() {
        let dir = tempfile::tempdir().expect("temp dir");
        let source = dir.path().join("source.pdf");
        let target = dir.path().join("filled.pdf");
        let source_bytes = testpdf::bytes(&testpdf::form_checkbox_yes());
        std::fs::write(&source, &source_bytes).expect("write fixture");

        let outcome = fill_to(
            &source,
            &target,
            &[FillValue {
                name: "agree".to_string(),
                value: "Yes".to_string(),
            }],
            |path| path.exists(),
        )
        .expect("the fill succeeds");

        let written = std::fs::read(&outcome.path).expect("read the output");
        assert!(
            written.starts_with(&source_bytes),
            "the output must begin with the original file verbatim: {} of {} bytes survived",
            written.len().min(source_bytes.len()),
            source_bytes.len()
        );
        assert!(
            written.len() > source_bytes.len(),
            "an incremental update appends, so the file must have grown"
        );
    }

    /// A second fill of an already-filled file must not lose the first value.
    ///
    /// This is what makes the append genuinely incremental rather than "revision two replaces
    /// revision one": the readback re-reads both revisions, so a lost first write shows up here.
    #[test]
    fn a_second_fill_keeps_the_first_fills_value() {
        let dir = tempfile::tempdir().expect("temp dir");
        let source = dir.path().join("source.pdf");
        let first = dir.path().join("first.pdf");
        let second = dir.path().join("second.pdf");
        std::fs::write(&source, testpdf::bytes(&testpdf::form_text_fields()))
            .expect("write fixture");

        fill_to(
            &source,
            &first,
            &[FillValue {
                name: "given".to_string(),
                value: "Ada".to_string(),
            }],
            |path| path.exists(),
        )
        .expect("first fill");

        fill_to(
            &first,
            &second,
            &[FillValue {
                name: "notes".to_string(),
                value: "second".to_string(),
            }],
            |path| path.exists(),
        )
        .expect("second fill");

        let readback = open(&second).expect("readback");
        assert_eq!(
            readback.field("given").and_then(|f| f.value.clone()),
            Some("Ada".to_string()),
            "the first revision's value must survive the second append"
        );
        assert_eq!(
            readback.field("notes").and_then(|f| f.value.clone()),
            Some("second".to_string())
        );
    }

    /// I-F2: a value outside the field's legal set is refused, and nothing is written.
    #[test]
    fn a_value_the_document_does_not_carry_is_refused_by_name() {
        let dir = tempfile::tempdir().expect("temp dir");
        let source = dir.path().join("source.pdf");
        let target = dir.path().join("filled.pdf");
        std::fs::write(&source, testpdf::bytes(&testpdf::form_checkbox_on()))
            .expect("write fixture");

        let error = fill_to(
            &source,
            &target,
            &[FillValue {
                name: "agree".to_string(),
                value: "Yes".to_string(), // this fixture spells its on-state /On
            }],
            |path| path.exists(),
        )
        .expect_err("a state the /AP dictionary does not name must be refused");

        assert_eq!(error.code(), "FORM_FIELD_VALUE_NOT_ALLOWED");
        assert!(
            error.to_string().contains("On"),
            "the refusal must name the states the document actually has, or the user cannot tell \
             a bug from a typo: {error}"
        );
        assert!(
            !target.exists(),
            "a refusal must leave no file behind: the caller has to be able to retry knowing \
             nothing changed"
        );
    }

    #[test]
    fn an_unknown_field_name_is_refused_and_no_file_is_written() {
        let dir = tempfile::tempdir().expect("temp dir");
        let source = dir.path().join("source.pdf");
        let target = dir.path().join("filled.pdf");
        std::fs::write(&source, testpdf::bytes(&testpdf::form_text_fields()))
            .expect("write fixture");

        let error = fill_to(
            &source,
            &target,
            &[FillValue {
                name: "no_such_field".to_string(),
                value: "x".to_string(),
            }],
            |path| path.exists(),
        )
        .expect_err("I-F2 forbids inventing a field");

        assert_eq!(error.code(), "FORM_FIELD_NOT_FOUND");
        assert!(error.to_string().contains("no_such_field"));
        assert!(!target.exists());
    }

    /// I-F4: a read-only field is refused, and the refusal is distinct from "no such field".
    #[test]
    fn a_read_only_field_is_refused_with_its_own_code() {
        let dir = tempfile::tempdir().expect("temp dir");
        let source = dir.path().join("source.pdf");
        let target = dir.path().join("filled.pdf");
        std::fs::write(&source, testpdf::bytes(&testpdf::form_read_only_field()))
            .expect("write fixture");

        let error = fill_to(
            &source,
            &target,
            &[FillValue {
                name: "locked".to_string(),
                value: "changed".to_string(),
            }],
            |path| path.exists(),
        )
        .expect_err("a read-only field must not be filled");

        assert_eq!(
            error.code(),
            "FORM_FIELD_READ_ONLY",
            "distinct from FORM_FIELD_NOT_FOUND: the field is there, it just cannot be changed"
        );
        assert!(!target.exists());
    }

    /// One bad value in a batch refuses the whole batch, so a fill is all-or-nothing.
    #[test]
    fn one_bad_value_refuses_the_whole_batch_rather_than_writing_the_good_ones() {
        let dir = tempfile::tempdir().expect("temp dir");
        let source = dir.path().join("source.pdf");
        let target = dir.path().join("filled.pdf");
        std::fs::write(&source, testpdf::bytes(&testpdf::form_text_fields()))
            .expect("write fixture");

        let error = fill_to(
            &source,
            &target,
            &[
                FillValue {
                    name: "given".to_string(),
                    value: "Ada".to_string(),
                },
                FillValue {
                    name: "missing".to_string(),
                    value: "x".to_string(),
                },
            ],
            |path| path.exists(),
        )
        .expect_err("the batch contains an unknown field");

        assert_eq!(error.code(), "FORM_FIELD_NOT_FOUND");
        assert!(
            !target.exists(),
            "the valid first value must not have been written: a half-applied fill is a file \
             changed in ways the caller did not ask for"
        );
    }

    /// A choice write has to keep `/I` and `/V` in step.
    ///
    /// The fixture's list starts on option 1 with a matching `/V`. Writing option 2 and updating
    /// only `/V` leaves a viewer that trusts `/I` highlighting "bravo" while the text says
    /// "charlie" — a disagreement the value-only check would not see, so this asserts both.
    #[test]
    fn a_choice_write_updates_the_selected_index_with_the_value() {
        let dir = tempfile::tempdir().expect("temp dir");
        let source = dir.path().join("source.pdf");
        let target = dir.path().join("filled.pdf");
        std::fs::write(&source, testpdf::bytes(&testpdf::form_choice_fields()))
            .expect("write fixture");

        fill_to(
            &source,
            &target,
            &[FillValue {
                name: "many".to_string(),
                value: "charlie".to_string(),
            }],
            |path| path.exists(),
        )
        .expect("the choice write succeeds");

        let readback = open(&target).expect("readback");
        let many = readback.field("many").expect("many");
        assert_eq!(many.value.as_deref(), Some("charlie"));
        assert_eq!(
            many.selected_indices,
            vec![2],
            "/I must move with /V, or a viewer highlights the old row"
        );
    }

    /// A radio group's write must set `/V` on the parent and `/AS` on the matching kid only.
    ///
    /// Setting `/AS` on both kids, or on the wrong one, produces a group where the tick is under
    /// the option the user did not pick.
    #[test]
    fn a_radio_write_sets_the_state_on_the_matching_kid_only() {
        let dir = tempfile::tempdir().expect("temp dir");
        let source = dir.path().join("source.pdf");
        let target = dir.path().join("filled.pdf");
        std::fs::write(&source, testpdf::bytes(&testpdf::form_radio_group()))
            .expect("write fixture");

        fill_to(
            &source,
            &target,
            &[FillValue {
                name: "color".to_string(),
                value: "blue".to_string(),
            }],
            |path| path.exists(),
        )
        .expect("the radio write succeeds");

        let document = Document::load(&target).expect("reopen");
        let mut states = Vec::new();
        for (_, object) in document.objects.iter() {
            if let Ok(dictionary) = object.as_dict() {
                if dictionary
                    .get(b"Subtype")
                    .and_then(Object::as_name)
                    .map(|name| name == b"Widget")
                    .unwrap_or(false)
                {
                    let state = dictionary
                        .get(b"AS")
                        .and_then(Object::as_name)
                        .map(|name| String::from_utf8_lossy(name).to_string())
                        .unwrap_or_else(|_| "<none>".to_string());
                    states.push((dictionary.get(b"Rect").is_ok(), state));
                }
            }
        }
        let blue = states.iter().filter(|(_, state)| state == "blue").count();
        let red = states.iter().filter(|(_, state)| state == "red").count();
        assert_eq!(blue, 1, "exactly the blue kid is on: {states:?}");
        assert_eq!(red, 0, "the red kid must stay Off: {states:?}");
    }

    /// `/NeedAppearances` has to be set on the *saved* file, or a viewer draws stale ink.
    #[test]
    fn a_fill_sets_need_appearances_on_the_saved_file() {
        let dir = tempfile::tempdir().expect("temp dir");
        let source = dir.path().join("source.pdf");
        let target = dir.path().join("filled.pdf");
        std::fs::write(&source, testpdf::bytes(&testpdf::form_text_fields()))
            .expect("write fixture");

        fill_to(
            &source,
            &target,
            &[FillValue {
                name: "given".to_string(),
                value: "Ada".to_string(),
            }],
            |path| path.exists(),
        )
        .expect("fill");

        let document = Document::load(&target).expect("reopen");
        let catalog = document.catalog().expect("catalog");
        let form = catalog
            .get(b"AcroForm")
            .ok()
            .and_then(|form| match form {
                Object::Reference(id) => document.get_object(*id).ok(),
                other => Some(other),
            })
            .and_then(|form| form.as_dict().ok())
            .expect("the form survives the append");

        assert_eq!(
            form.get(b"NeedAppearances").ok(),
            Some(&Object::Boolean(true)),
            "without this a viewer trusts the stale /AP stream and shows the old value"
        );
        assert!(
            form.get(b"Fields").is_ok(),
            "the form's /Fields must survive: replacing /AcroForm wholesale would orphan every field"
        );
    }

    /// I-F1 has to fail loudly when the file does not carry the value — the exact defect that
    /// forced this module into existence.
    ///
    /// Built by handing the verifier a write whose expectation the readback cannot meet, which is
    /// the shape a silently-ignored write takes: the fill "succeeds" and the field still reads the
    /// old value. If this branch ever stops refusing, a user sees "Filled" over a document that did
    /// not change.
    #[test]
    fn a_write_the_saved_file_does_not_carry_is_refused_rather_than_reported_as_filled() {
        let readback = open_fixture(fixture_bytes(testpdf::form_checkbox_on()));
        let field_id = readback
            .locations
            .get("agree")
            .expect("the fixture's field is located")
            .field_id;

        // What the caller asked for versus what the file actually holds. `/On` is the state the
        // fixture draws; asking the verifier to expect `/Yes` is the disagreement under test.
        let write = PlannedWrite {
            name: "agree".to_string(),
            kind: WriteKind::Button("Yes".to_string()),
            expected: Expected::Button("Yes".to_string()),
            field_id,
            widget_ids: vec![field_id],
        };

        let error = verify_writes(&readback, std::slice::from_ref(&write))
            .expect_err("a value the file does not carry must not read back as success");
        assert_eq!(
            error.code(),
            "FORM_FILL_DID_NOT_PERSIST",
            "the refusal must name itself so the UI can say the fill did not stick"
        );
        assert!(
            error.to_string().contains("agree"),
            "the message must name the field: {error}"
        );
        assert!(
            error.to_string().contains("Off"),
            "the message must quote what the field actually reads, so a user can see the file \
             disagreed with the write: {error}"
        );

        // The same write, expecting what the file really holds, must pass. Without this the test
        // above would be satisfied by a verifier that refuses everything.
        let matching = PlannedWrite {
            name: "agree".to_string(),
            kind: WriteKind::Button("Off".to_string()),
            expected: Expected::Button("Off".to_string()),
            field_id,
            widget_ids: vec![field_id],
        };
        verify_writes(&readback, std::slice::from_ref(&matching))
            .expect("a write the file does carry must verify");
    }

    /// A write naming a field the readback does not have is also a non-persist, not a silent pass.
    #[test]
    fn a_write_for_a_field_missing_from_the_saved_file_is_refused() {
        let readback = open_fixture(fixture_bytes(testpdf::form_text_fields()));
        let field_id = readback
            .locations
            .get("given")
            .expect("the fixture's field is located")
            .field_id;
        let write = PlannedWrite {
            name: "vanished".to_string(),
            kind: WriteKind::Text("x".to_string()),
            expected: Expected::Text("x".to_string()),
            field_id,
            widget_ids: vec![field_id],
        };
        let error = verify_writes(&readback, std::slice::from_ref(&write))
            .expect_err("a missing field is not a persisted fill");
        assert_eq!(error.code(), "FORM_FILL_DID_NOT_PERSIST");
    }

    /// A form document certified at `level`, in the shape `verify::declared_level` reads: a catalog
    /// `/Perms /DocMDP` entry, a `/Type /Sig` dictionary, and `/Reference` naming a
    /// `/TransformParams` dictionary whose `/P` is the level.
    ///
    /// Built here rather than added to `testpdf` because only this test needs it, and a fixture
    /// builder nothing else calls is a second thing to keep honest.
    fn certified_form(level: i64) -> Vec<u8> {
        let mut document = testpdf::form_checkbox_yes();
        let parameters = document.add_object(dictionary! { "P" => level });
        let reference = document.add_object(dictionary! {
            "TransformMethod" => Object::Name(b"DocMDP".to_vec()),
            "TransformParams" => Object::Reference(parameters),
        });
        let signature = document.add_object(dictionary! {
            "Type" => Object::Name(b"Sig".to_vec()),
            "Filter" => Object::Name(b"Adobe.PPKLite".to_vec()),
            "Reference" => Object::Array(vec![Object::Reference(reference)]),
            "Contents" => Object::string_literal(vec![0u8; 8]),
        });
        let catalog_id = document
            .trailer
            .get(b"Root")
            .ok()
            .and_then(|root| root.as_reference().ok())
            .expect("the document has a /Root");
        let catalog = document
            .get_object_mut(catalog_id)
            .and_then(|object| object.as_dict_mut())
            .expect("a catalog dictionary");
        catalog.set(
            "Perms",
            dictionary! { "DocMDP" => Object::Reference(signature) },
        );
        testpdf::bytes(&document)
    }

    /// D-F7's ruling, as a test: form fill is permitted on a document certified at `P=2` and `P=3`,
    /// and refused at `P=1` with the level named.
    ///
    /// The `P=1` case is the one that matters, and it is asserted by *code* rather than by message
    /// text so a reworded message cannot make it pass. It also checks no file was produced, because
    /// a gate that refuses after the write has already told the user their fill worked.
    #[test]
    fn the_certification_gate_refuses_a_p1_fill_and_permits_p2_and_p3() {
        for level in [2i64, 3] {
            let source_bytes = certified_form(level);
            // Guard the guard: if `certified_form` stopped producing a readable level, the
            // permissive rows below would pass for the wrong reason.
            assert_eq!(
                crate::editor::signature::verify::certification_level(&source_bytes),
                crate::editor::signature::editpolicy::CertificationLevel::from_number(level as u8),
                "the P={level} fixture must actually declare its level"
            );

            let dir = tempfile::tempdir().expect("temp dir");
            let source = dir.path().join("source.pdf");
            let target = dir.path().join("filled.pdf");
            std::fs::write(&source, &source_bytes).expect("write fixture");

            fill_to(
                &source,
                &target,
                &[FillValue {
                    name: "agree".to_string(),
                    value: "Yes".to_string(),
                }],
                |path| path.exists(),
            )
            .unwrap_or_else(|error| {
                panic!("P={level} permits form fill per the 2026-11-14 ruling, but got: {error}")
            });
            assert!(target.exists(), "P={level} must produce the filled file");
        }

        let source_bytes = certified_form(1);
        assert_eq!(
            crate::editor::signature::verify::certification_level(&source_bytes),
            crate::editor::signature::editpolicy::CertificationLevel::from_number(1),
            "the P=1 fixture must actually declare its level"
        );

        let dir = tempfile::tempdir().expect("temp dir");
        let source = dir.path().join("source.pdf");
        let target = dir.path().join("filled.pdf");
        std::fs::write(&source, &source_bytes).expect("write fixture");

        let error = fill_to(
            &source,
            &target,
            &[FillValue {
                name: "agree".to_string(),
                value: "Yes".to_string(),
            }],
            |path| path.exists(),
        )
        .expect_err("P=1 forbids every change after certification but a further signature");

        assert_eq!(error.code(), "CERTIFICATION_REFUSES_EDIT");
        assert!(
            error.to_string().contains('1'),
            "the refusal must name the level, or the user cannot tell a P=1 refusal from a P=2 one: \
             {error}"
        );
        assert!(
            !target.exists(),
            "a certified refusal must leave no file: the gate runs before the write, and this is how \
             that ordering is proven rather than asserted"
        );

        // Reading is not filling: a P=1 document must still open and list its fields, or the gate
        // has turned a certified document into an unopenable one.
        let form = open(&source).expect("a certified document still opens");
        assert!(
            form.field("agree").is_some(),
            "reading a P=1 document's fields is not an edit and must stay possible"
        );
    }

    /// §5's acceptance sweep: every state spelling the design names, filled end to end, plus the
    /// one real-producer file in the repository.
    ///
    /// This is the row a reviewer reads instead of the eight unit tests. It is deliberately one
    /// test over a table rather than four tests: the claim being made is *uniformity* -- that the
    /// same writer handles `/Yes`, `/On`, `/yes` and LibreOffice's value/state disagreement without
    /// a special case per spelling -- and four separate tests would each pass while a fifth
    /// spelling was handled by a hardcoded branch.
    ///
    /// Each case asserts the three things a fill has to be true of, in the order the invariants
    /// name them: the value persisted (I-F1), the bytes before the append are the original's
    /// (D-F4), and the source file on disk is untouched.
    #[test]
    fn the_acceptance_sweep_fills_every_state_spelling_and_the_real_producer_file() {
        // (label, fixture bytes, field name, the state to write, the state the file must then hold)
        //
        // The third and fourth columns differ for the LibreOffice file and that difference is the
        // point: its `/V` says `Yes` while its `/AP /N` draws `yes`. A writer that took the state
        // name from `/V` would write `/V /Yes` and `/AS /Yes`, and `/AS` would then name a stream
        // that does not exist. Writing `yes` and reading back `yes` proves the name came from where
        // the design says it must.
        let synthetic_yes = fixture_bytes(testpdf::form_checkbox_yes());
        let synthetic_on = fixture_bytes(testpdf::form_checkbox_on());
        let synthetic_lower = fixture_bytes(testpdf::form_checkbox_lowercase_yes());
        let libreoffice = std::fs::read("tests/fixtures/libreoffice-checkbox-form.pdf")
            .expect("the real-producer fixture is committed");

        let cases: [(&str, Vec<u8>, &str, &str, &str); 4] = [
            ("synthetic /Yes", synthetic_yes, "agree", "Yes", "Yes"),
            ("synthetic /On", synthetic_on, "agree", "On", "On"),
            ("synthetic /yes", synthetic_lower, "agree", "yes", "yes"),
            (
                "libreoffice /V Yes with /AP yes",
                libreoffice,
                "agree",
                "yes",
                "yes",
            ),
        ];

        for (label, source_bytes, field, write, expected) in cases {
            let dir = tempfile::tempdir().expect("temp dir");
            let source = dir.path().join("source.pdf");
            let target = dir.path().join("filled.pdf");
            std::fs::write(&source, &source_bytes).expect("write fixture");

            fill_to(
                &source,
                &target,
                &[FillValue {
                    name: field.to_string(),
                    value: write.to_string(),
                }],
                |path| path.exists(),
            )
            .unwrap_or_else(|error| panic!("{label}: fill refused: {error}"));

            // I-F1: the saved file carries the value.
            let readback = open(&target).unwrap_or_else(|error| panic!("{label}: {error}"));
            assert_eq!(
                readback.field(field).and_then(|f| f.value.clone()),
                Some(expected.to_string()),
                "{label}: the saved file must hold {expected:?}"
            );

            // D-F4: the original bytes are revision one, verbatim.
            let written = std::fs::read(&target).expect("read output");
            assert!(
                written.starts_with(&source_bytes),
                "{label}: the fill must append, not rewrite"
            );

            // The source is not the target, and was not touched.
            assert_eq!(
                std::fs::read(&source).expect("read source"),
                source_bytes,
                "{label}: the original file must be unchanged"
            );
        }
    }

    /// The tamper negative: a state name the field's `/AP /N` does not draw is refused even when
    /// another spelling in the same family exists on the file.
    ///
    /// Written against the real-producer file because that is where the trap is sharpest. Its `/V`
    /// says `Yes` and its appearance says `yes`; a writer that accepted `/V`'s spelling would take
    /// `Yes` here and write an `/AS` no stream answers. The refusal must therefore name `yes` as the
    /// legal state, proving the legal set came from `/AP /N` and not from `/V`.
    #[test]
    fn the_tamper_negative_refuses_the_spelling_the_appearance_does_not_draw() {
        let source_bytes = std::fs::read("tests/fixtures/libreoffice-checkbox-form.pdf")
            .expect("the real-producer fixture is committed");
        let form = open_bytes(source_bytes.clone()).expect("the fixture opens");

        let field = form.field("agree").expect("the checkbox is named agree");
        assert_eq!(
            field.value.as_deref(),
            Some("Yes"),
            "the fixture's /V must stay /Yes, or this test stops testing the disagreement"
        );
        assert_eq!(
            field.states,
            vec!["yes".to_string()],
            "the fixture's /AP /N must stay /yes, or this test stops testing the disagreement"
        );

        let dir = tempfile::tempdir().expect("temp dir");
        let source = dir.path().join("source.pdf");
        let target = dir.path().join("filled.pdf");
        std::fs::write(&source, &source_bytes).expect("write fixture");

        let error = fill_to(
            &source,
            &target,
            &[FillValue {
                name: "agree".to_string(),
                // The spelling `/V` uses. Plausible, and wrong: no `/AP /N` key draws it.
                value: "Yes".to_string(),
            }],
            |path| path.exists(),
        )
        .expect_err("the value /V spells is not the state the appearance draws");

        assert_eq!(error.code(), "FORM_FIELD_VALUE_NOT_ALLOWED");
        assert!(
            error.to_string().contains("yes"),
            "the refusal must offer the state the file actually draws, which proves the legal set \
             came from /AP /N rather than /V: {error}"
        );
        assert!(
            !target.exists(),
            "a refusal must leave no file, so the caller can retry knowing nothing changed"
        );

        // And the spelling the appearance does draw is accepted, so this is a distinction the
        // writer makes rather than a blanket refusal of the file.
        fill_to(
            &source,
            &target,
            &[FillValue {
                name: "agree".to_string(),
                value: "yes".to_string(),
            }],
            |path| path.exists(),
        )
        .expect("the state the appearance draws must be accepted");
    }

    #[test]
    fn a_utf16_name_is_decoded_rather_than_left_as_bytes() {
        assert_eq!(decode_text_string(&[0xFE, 0xFF, 0x00, 0x41]), "A");
        assert_eq!(decode_text_string(b"plain"), "plain");
    }
}
