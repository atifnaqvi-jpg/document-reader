//! Sanitize, as part of apply rather than a separate opt-in step (P3.2 D-RD5).
//!
//! The scope is a **closed list**, and that is the point. "Remove hidden information" that
//! leaves metadata behind is theatre, and a list that grows as cases are noticed cannot be
//! tested. These are the classes this build claims to remove, and §8 of the design records
//! everything it does not:
//!
//! 1. document `/Info` metadata,
//! 2. XMP metadata (the `/Metadata` stream),
//! 3. outgoing `/Outlines` (bookmarks),
//! 4. `/Names` → `/EmbeddedFiles` attachments,
//! 5. annotation `/Contents` and `/T` outside the redaction rectangles,
//! 6. form-field values,
//! 7. document-level JavaScript actions.
//!
//! Two of those are destructive to content the user can see and use — bookmarks and
//! attachments — which is why they happen inside the same confirmed apply action, and why
//! the confirmation names them rather than saying "hidden information" and hoping.
//!
//! Each class is a separate function returning what it removed, so a test can assert on one
//! class without the others masking it. A single `sanitize` that reported a count would let
//! one broken class hide behind six working ones.

use lopdf::{Dictionary, Document, Object};

use crate::editor::object_gc;
use crate::model::AppError;

/// What one sanitize pass removed, per class of D-RD5.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Sanitized {
    /// True when the trailer's `/Info` dictionary was dropped.
    pub info_dropped: bool,
    /// True when the document catalog's `/Metadata` stream was dropped.
    pub xmp_dropped: bool,
    /// Number of outline items removed with the outline tree.
    pub outlines_removed: usize,
    /// Number of embedded files removed from `/Names`.
    pub embedded_files_removed: usize,
    /// Number of annotation `/Contents` or `/T` values cleared.
    pub annotation_texts_cleared: usize,
    /// Number of form-field values cleared.
    pub form_values_cleared: usize,
    /// Number of document-level JavaScript actions removed.
    pub javascript_removed: usize,
    /// Objects swept afterwards because nothing reaches them any more.
    ///
    /// This is what makes the removal real. `Document::save_internal` writes every object in
    /// `objects` whatever references it, so clearing a `/Info` reference leaves the author
    /// string in the file as a dead object — still readable by anyone with a recovery tool.
    pub objects_dropped: usize,
}

impl Sanitized {
    /// True when nothing was removed — the caller has nothing to report and the file is
    /// unchanged by this pass.
    pub fn is_empty(&self) -> bool {
        *self == Sanitized::default()
    }
}

/// Runs every D-RD5 class over `document`.
pub fn sanitize(document: &mut Document) -> Result<Sanitized, AppError> {
    // One initializer rather than field-by-field assignment: each class reads and writes a
    // different part of the document, so the order is the definition's order and should look
    // like it, not like seven statements that could be reordered.
    Ok(Sanitized {
        info_dropped: drop_info(document),
        xmp_dropped: drop_xmp(document),
        outlines_removed: drop_outlines(document),
        embedded_files_removed: drop_embedded_files(document),
        annotation_texts_cleared: clear_annotation_texts(document),
        form_values_cleared: clear_form_values(document),
        javascript_removed: drop_javascript(document),
        objects_dropped: object_gc::sweep_unreachable(document)?.len(),
    })
}

/// (1) The trailer's `/Info` dictionary — author, title, producer, dates.
fn drop_info(document: &mut Document) -> bool {
    if document.trailer.get(b"Info").is_err() {
        return false;
    }
    document.trailer.remove(b"Info");
    true
}

/// (2) The catalog's `/Metadata` XMP stream.
fn drop_xmp(document: &mut Document) -> bool {
    let Some(catalog) = document
        .trailer
        .get(b"Root")
        .ok()
        .and_then(|root| root.as_reference().ok())
    else {
        return false;
    };
    let Ok(catalog) = document
        .get_object_mut(catalog)
        .and_then(Object::as_dict_mut)
    else {
        return false;
    };
    if catalog.get(b"Metadata").is_err() {
        return false;
    }
    catalog.remove(b"Metadata");
    true
}

/// (3) Outgoing `/Outlines` — the bookmark tree.
///
/// Counted by walking the tree before dropping it, because "the catalog has no `/Outlines`"
/// is also true of a document that never had bookmarks; the count is what tells the two apart.
fn drop_outlines(document: &mut Document) -> usize {
    let Some(catalog_id) = document
        .trailer
        .get(b"Root")
        .ok()
        .and_then(|root| root.as_reference().ok())
    else {
        return 0;
    };
    let outline_root = document
        .get_object(catalog_id)
        .and_then(Object::as_dict)
        .ok()
        .and_then(|catalog| catalog.get(b"Outlines").ok().cloned());
    let Some(outline_root) = outline_root else {
        return 0;
    };
    // The `/Outlines` dictionary is a container, not a bookmark; items start at its `/First`.
    let Some(first) = document
        .dereference(&outline_root)
        .ok()
        .and_then(|(_, root)| root.as_dict().ok())
        .and_then(|root| root.get(b"First").ok().cloned())
    else {
        return 0;
    };

    let count = count_outline_items(document, &first, 0);
    if let Ok(catalog) = document
        .get_object_mut(catalog_id)
        .and_then(Object::as_dict_mut)
    {
        catalog.remove(b"Outlines");
    }
    count
}

/// Counts outline items reachable from `entry`, following `/First` and `/Next` siblings.
///
/// Depth is bounded rather than trusted: a malformed file can make `/Next` reach itself, and
/// a sanitize pass that hangs on a hostile document is a denial of service on the user.
fn count_outline_items(document: &Document, entry: &Object, depth: usize) -> usize {
    const MAX_DEPTH: usize = 64;
    if depth > MAX_DEPTH {
        return 0;
    }
    let Some(item) = document
        .dereference(entry)
        .ok()
        .and_then(|(_, object)| object.as_dict().ok())
    else {
        return 0;
    };

    let mut count = 1;
    if let Ok(first) = item.get(b"First") {
        count += count_outline_items(document, first, depth + 1);
    }
    if let Ok(next) = item.get(b"Next") {
        count += count_outline_items(document, next, depth + 1);
    }
    count
}

/// (4) `/Names` → `/EmbeddedFiles` attachments.
fn drop_embedded_files(document: &mut Document) -> usize {
    let Some(catalog_id) = document
        .trailer
        .get(b"Root")
        .ok()
        .and_then(|root| root.as_reference().ok())
    else {
        return 0;
    };
    let names_id = document
        .get_object(catalog_id)
        .and_then(Object::as_dict)
        .ok()
        .and_then(|catalog| match catalog.get(b"Names").ok() {
            Some(Object::Reference(id)) => Some(*id),
            _ => None,
        });
    let Some(names_id) = names_id else {
        return 0;
    };

    // Counted before the mutable borrow: the name tree's `/Names` holds two entries per file
    // (name, filespec), and dereferencing needs `&document` while `names` holds `&mut`.
    let present = document
        .get_object(names_id)
        .and_then(Object::as_dict)
        .ok()
        .and_then(|names| names.get(b"EmbeddedFiles").ok().cloned());
    let Some(tree) = present else {
        return 0;
    };
    let count = document
        .dereference(&tree)
        .ok()
        .and_then(|(_, tree)| tree.as_dict().ok())
        .and_then(|tree| tree.get(b"Names").ok())
        .and_then(|array| document.dereference(array).ok())
        .and_then(|(_, array)| array.as_array().ok())
        .map(|array| array.len() / 2)
        .unwrap_or(1);

    if let Ok(names) = document
        .get_object_mut(names_id)
        .and_then(Object::as_dict_mut)
    {
        names.remove(b"EmbeddedFiles");
    }
    count
}

/// (5) Annotation `/Contents` and `/T`, on every page.
///
/// Annotations *inside* a mark are already deleted whole by the redaction pass, so anything
/// reached here is outside every rectangle and is cleared rather than deleted: the mark is
/// not where this text is, but the text is still hidden information in the file.
fn clear_annotation_texts(document: &mut Document) -> usize {
    let pages: Vec<_> = document.get_pages().values().copied().collect();
    let mut cleared = 0;

    for page_id in pages {
        let annots = match document.get_object(page_id).and_then(Object::as_dict) {
            Ok(page) => match page.get(b"Annots").ok() {
                Some(Object::Reference(id)) => document
                    .get_object(*id)
                    .and_then(Object::as_array)
                    .cloned()
                    .unwrap_or_default(),
                Some(Object::Array(array)) => array.clone(),
                _ => continue,
            },
            Err(_) => continue,
        };

        for entry in annots {
            let Some(id) = entry.as_reference().ok() else {
                continue;
            };
            let Ok(dict) = document.get_object_mut(id).and_then(Object::as_dict_mut) else {
                continue;
            };
            for key in [b"Contents".as_slice(), b"T".as_slice()] {
                if dict.get(key).is_ok() {
                    dict.remove(key);
                    cleared += 1;
                }
            }
        }
    }

    cleared
}

/// (6) Form-field values: every `/V` in the AcroForm field tree.
fn clear_form_values(document: &mut Document) -> usize {
    let Some(catalog_id) = document
        .trailer
        .get(b"Root")
        .ok()
        .and_then(|root| root.as_reference().ok())
    else {
        return 0;
    };
    let acro_id = document
        .get_object(catalog_id)
        .and_then(Object::as_dict)
        .ok()
        .and_then(|catalog| match catalog.get(b"AcroForm").ok() {
            Some(Object::Reference(id)) => Some(*id),
            _ => None,
        });

    let fields: Vec<Object> = match acro_id.and_then(|id| {
        document
            .get_object(id)
            .and_then(Object::as_dict)
            .ok()
            .and_then(|form| form.get(b"Fields").ok())
            .and_then(|fields| document.dereference(fields).ok())
            .and_then(|(_, fields)| fields.as_array().ok())
            .map(|array| array.to_vec())
    }) {
        Some(fields) => fields,
        None => {
            // An inline AcroForm: `/Fields` sits directly in the catalog.
            match document
                .get_object_mut(catalog_id)
                .and_then(Object::as_dict_mut)
                .ok()
                .and_then(|catalog| catalog.get_mut(b"AcroForm").ok())
                .and_then(|form| form.as_dict_mut().ok())
                .and_then(|form| form.get(b"Fields").ok())
                .and_then(|fields| fields.as_array().ok())
                .map(|array| array.to_vec())
            {
                Some(fields) => fields,
                None => return 0,
            }
        }
    };

    let mut cleared = 0;
    for field in fields {
        cleared += clear_field(document, &field, 0);
    }
    cleared
}

/// Clears `/V` on one field and its kids, bounded in depth for the same reason as outlines.
fn clear_field(document: &mut Document, entry: &Object, depth: usize) -> usize {
    const MAX_DEPTH: usize = 64;
    if depth > MAX_DEPTH {
        return 0;
    }
    let Ok(id) = entry.as_reference() else {
        return 0;
    };
    let kids = document
        .get_object(id)
        .and_then(Object::as_dict)
        .ok()
        .and_then(|field| field.get(b"Kids").ok())
        .and_then(|kids| kids.as_array().ok())
        .map(|kids| kids.to_vec())
        .unwrap_or_default();

    let mut cleared = 0;
    if let Ok(field) = document.get_object_mut(id).and_then(Object::as_dict_mut) {
        if field.get(b"V").is_ok() {
            field.remove(b"V");
            cleared += 1;
        }
    }
    for kid in kids {
        cleared += clear_field(document, &kid, depth + 1);
    }
    cleared
}

/// (7) Document-level JavaScript: the catalog's `/OpenAction` and the `/Names` JavaScript tree.
fn drop_javascript(document: &mut Document) -> usize {
    let Some(catalog_id) = document
        .trailer
        .get(b"Root")
        .ok()
        .and_then(|root| root.as_reference().ok())
    else {
        return 0;
    };

    let mut removed = 0;

    // `/OpenAction` may itself be a JavaScript action or point at one.
    let open_is_js = document
        .get_object(catalog_id)
        .and_then(Object::as_dict)
        .ok()
        .and_then(|catalog| catalog.get(b"OpenAction").ok().cloned())
        .map(|action| is_javascript(document, &action))
        .unwrap_or(false);
    if open_is_js {
        if let Ok(catalog) = document
            .get_object_mut(catalog_id)
            .and_then(Object::as_dict_mut)
        {
            catalog.remove(b"OpenAction");
            removed += 1;
        }
    }

    // The named JavaScript tree: `/Names` → `/JavaScript`.
    let names_id = document
        .get_object(catalog_id)
        .and_then(Object::as_dict)
        .ok()
        .and_then(|catalog| match catalog.get(b"Names").ok() {
            Some(Object::Reference(id)) => Some(*id),
            _ => None,
        });
    if let Some(names_id) = names_id {
        let has_js = document
            .get_object(names_id)
            .and_then(Object::as_dict)
            .ok()
            .and_then(|names| names.get(b"JavaScript").ok())
            .map(|tree| count_name_tree(document, tree))
            .unwrap_or(0);
        if has_js > 0 {
            if let Ok(names) = document
                .get_object_mut(names_id)
                .and_then(Object::as_dict_mut)
            {
                names.remove(b"JavaScript");
            }
            removed += has_js;
        }
    }

    removed
}

/// Counts entries in a name tree's `/Names` array (two per entry).
fn count_name_tree(document: &Document, tree: &Object) -> usize {
    document
        .dereference(tree)
        .ok()
        .and_then(|(_, tree)| tree.as_dict().ok())
        .and_then(|tree| tree.get(b"Names").ok())
        .and_then(|array| document.dereference(array).ok())
        .and_then(|(_, array)| array.as_array().ok())
        .map(|array| array.len() / 2)
        .unwrap_or(1)
}

/// Whether an action dictionary is a JavaScript action (`/S /JavaScript`).
fn is_javascript(document: &Document, action: &Object) -> bool {
    let Ok((_, object)) = document.dereference(action) else {
        return false;
    };
    let Ok(dict) = object.as_dict() else {
        return false;
    };
    matches!(dict.get(b"S").and_then(Object::as_name), Ok(b"JavaScript"))
}

/// The catalog dictionary, for callers that need it without repeating the trailer lookup.
pub fn catalog(document: &Document) -> Option<&Dictionary> {
    let root = document.trailer.get(b"Root").ok()?;
    let id = root.as_reference().ok()?;
    document.get_object(id).ok()?.as_dict().ok()
}
