//! Page-structure operations: rotate, delete, extract, split, merge, reorder.
//!
//! These use `lopdf`'s page tree directly rather than PDFium's `PdfPages` helper
//! methods. PDFium can do all of it (verified: `copy_page_from_document`,
//! `delete_page_at_index`, `tile_into_new_document` all public), but mixing two
//! writers over one document risks one library's in-memory changes clobbering the
//! other's on save. Since annotations must go through lopdf anyway (P2-D3), the
//! page tree follows the same writer and the PDFium path stays render-only.
//!
//! Rotation is the exception, and it is a real one: rotation is a *view*
//! property (`/Rotate` on the page), not a content rewrite, so it needs no
//! rasterisation and no PDFium involvement at all.

use lopdf::{dictionary, Dictionary, Document, Object, ObjectId};

use crate::model::AppError;

/// Rotates a page clockwise by `degrees`, which must be a multiple of 90.
///
/// `/Rotate` is cumulative and must stay in 0/90/180/270: a viewer is not
/// required to normalise an out-of-range value, and `359` would render
/// unpredictably. Existing rotation is read and advanced, so rotating twice
/// returns to the original rather than snapping to a fixed value.
pub fn rotate_page(
    document: &mut Document,
    page_id: ObjectId,
    degrees: i64,
) -> Result<i64, AppError> {
    let normalised = normalise_rotation(degrees)?;

    let current = current_rotation(document, page_id)?;
    let next = (current + normalised).rem_euclid(360);

    document
        .get_object_mut(page_id)
        .map_err(|error| AppError::Other(format!("page unreadable: {error}")))?
        .as_dict_mut()
        .map_err(|error| AppError::Other(format!("page is not a dictionary: {error}")))?
        .set(b"Rotate", Object::Integer(next));

    Ok(next)
}

pub fn current_rotation(document: &Document, page_id: ObjectId) -> Result<i64, AppError> {
    let page = document
        .get_object(page_id)
        .map_err(|error| AppError::Other(format!("page unreadable: {error}")))?
        .as_dict()
        .map_err(|error| AppError::Other(format!("page is not a dictionary: {error}")))?;

    match page.get(b"Rotate") {
        Ok(object) => Ok(object.as_i64().unwrap_or(0).rem_euclid(360)),
        // Absent /Rotate means upright, which is the normal case.
        Err(_) => Ok(0),
    }
}

/// Rejects anything that is not a quarter turn, rather than writing a value
/// viewers handle inconsistently.
pub fn normalise_rotation(degrees: i64) -> Result<i64, AppError> {
    if degrees % 90 != 0 {
        return Err(AppError::Other(format!(
            "rotation must be a multiple of 90 degrees, got {degrees}"
        )));
    }
    Ok(degrees.rem_euclid(360))
}

/// Deletes pages by 1-based number, as the UI shows them.
///
/// Deletion runs in descending order: deleting page 1 first would renumber every
/// later page, so a request to delete pages {1, 3} would remove what were
/// originally pages 1 and 4.
pub fn delete_pages(document: &mut Document, pages: &[u32]) -> Result<usize, AppError> {
    let total = document.get_pages().len() as u32;
    if pages.len() as u32 >= total {
        return Err(AppError::Other(
            "cannot delete every page in a document".to_string(),
        ));
    }

    let mut unique: Vec<u32> = pages.to_vec();
    unique.sort_unstable();
    unique.dedup();
    unique.reverse();

    let mut removed = 0;
    for number in unique {
        if number == 0 || number > total {
            return Err(AppError::Other(format!(
                "page {number} is out of range (document has {total} pages)"
            )));
        }
        document.delete_pages(&[number]);
        removed += 1;
    }

    Ok(removed)
}

/// Moves a page from one position to another, both 1-based.
pub fn move_page(document: &mut Document, from: u32, to: u32) -> Result<(), AppError> {
    let pages = document.get_pages();
    let total = pages.len() as u32;
    if from == 0 || from > total || to == 0 || to > total {
        return Err(AppError::Other(format!(
            "page move out of range: {from} -> {to} of {total}"
        )));
    }
    if from == to {
        return Ok(());
    }

    // Rebuild the /Kids array in the requested order. Reordering in place would
    // mean swapping references *and* updating /Parent links; rebuilding from the
    // final order keeps every page's parent correct by construction.
    let mut order: Vec<ObjectId> = pages.values().copied().collect();
    let moved = order.remove((from - 1) as usize);
    order.insert((to - 1) as usize, moved);

    let kids: Vec<Object> = order.iter().map(|id| Object::Reference(*id)).collect();
    let root_pages_id = root_pages_id(document)?;
    document
        .get_object_mut(root_pages_id)
        .map_err(|error| AppError::Other(format!("page tree unreadable: {error}")))?
        .as_dict_mut()
        .map_err(|error| AppError::Other(format!("page tree is not a dictionary: {error}")))?
        .set(b"Kids", Object::Array(kids));

    document.renumber_objects();
    Ok(())
}

/// Builds a new document from the selected pages, preserving their order.
pub fn extract_pages(source: &Document, pages: &[u32]) -> Result<Document, AppError> {
    if pages.is_empty() {
        return Err(AppError::Other(
            "no pages selected for extraction".to_string(),
        ));
    }

    let total = source.get_pages().len() as u32;
    for number in pages {
        if *number == 0 || *number > total {
            return Err(AppError::Other(format!(
                "page {number} is out of range (document has {total} pages)"
            )));
        }
    }

    let mut target = Document::with_version("1.4");
    let pages_id = target.new_object_id();

    let mut kids: Vec<Object> = Vec::with_capacity(pages.len());
    for number in pages {
        let page_id = copy_page_into(source, &mut target, *number, pages_id)?;
        kids.push(Object::Reference(page_id));
    }

    target.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => Object::Array(kids),
            "Count" => pages.len() as i64,
        }),
    );
    let catalog_id = target.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    target.trailer.set("Root", catalog_id);
    target.renumber_objects();

    Ok(target)
}

/// Splits a document into two at `at` (1-based): pages `1..at` and `at..end`.
pub fn split_at(document: &Document, at: u32) -> Result<(Document, Document), AppError> {
    let total = document.get_pages().len() as u32;
    if at == 0 || at > total {
        return Err(AppError::Other(format!(
            "split point {at} is out of range (document has {total} pages)"
        )));
    }
    if at == 1 || at > total {
        return Err(AppError::Other(format!(
            "split at {at} would leave an empty document"
        )));
    }

    let first: Vec<u32> = (1..at).collect();
    let second: Vec<u32> = (at..=total).collect();
    Ok((
        extract_pages(document, &first)?,
        extract_pages(document, &second)?,
    ))
}

/// Appends every page of `other` to `document`, returning the new page count.
pub fn merge(document: &mut Document, other: &Document) -> Result<usize, AppError> {
    let other_pages = other.get_pages();
    if other_pages.is_empty() {
        return Err(AppError::Other("nothing to merge".to_string()));
    }

    let root_pages_id = root_pages_id(document)?;
    let mut kids: Vec<Object> = document
        .get_object(root_pages_id)
        .and_then(|object| object.as_dict())
        .and_then(|pages| pages.get(b"Kids"))
        .and_then(Object::as_array)
        .cloned()
        .map_err(|error| AppError::Other(format!("page tree unreadable: {error}")))?;

    // Copy straight from `other` into `document`, parented to `document`'s own
    // page tree. An earlier draft staged through a third document, which left the
    // staged pages pointing at a /Pages node that was never inserted -- every
    // copied page had a dangling /Parent.
    for number in other_pages.keys() {
        let copied = copy_page_into(other, document, *number, root_pages_id)?;
        kids.push(Object::Reference(copied));
    }

    let count = kids.len() as i64;
    let pages = document
        .get_object_mut(root_pages_id)
        .map_err(|error| AppError::Other(format!("page tree unreadable: {error}")))?
        .as_dict_mut()
        .map_err(|error| AppError::Other(format!("page tree is not a dictionary: {error}")))?;
    pages.set(b"Kids", Object::Array(kids));
    pages.set(b"Count", Object::Integer(count));

    document.renumber_objects();
    Ok(count as usize)
}

/// Copies one page (and everything it references) from `source` into `target`.
///
/// A page's `/Contents` is a reference, and its `/Resources` may reference fonts
/// and XObjects; copying the page dictionary alone would leave those references
/// dangling in the target, producing a page that opens but renders blank.
fn copy_page_into(
    source: &Document,
    target: &mut Document,
    page_number: u32,
    parent_pages_id: ObjectId,
) -> Result<ObjectId, AppError> {
    let source_pages = source.get_pages();
    let source_page_id = *source_pages.get(&page_number).ok_or_else(|| {
        AppError::Other(format!("page {page_number} is not in the source document"))
    })?;

    let source_page = source
        .get_object(source_page_id)
        .map_err(|error| AppError::Other(format!("page unreadable: {error}")))?
        .as_dict()
        .map_err(|error| AppError::Other(format!("page is not a dictionary: {error}")))?
        .clone();

    let mut copied = Dictionary::new();
    for (key, value) in source_page.iter() {
        // /Parent must be repointed at the target's page tree; the source's
        // parent object does not exist here.
        if key == b"Parent" {
            continue;
        }
        copied.set(key.clone(), copy_value(source, target, value)?);
    }
    copied.set(b"Parent", Object::Reference(parent_pages_id));

    Ok(target.add_object(copied))
}

/// Deep-copies an object, remapping references into `target`.
fn copy_value(
    source: &Document,
    target: &mut Document,
    value: &Object,
) -> Result<Object, AppError> {
    Ok(match value {
        Object::Reference(id) => {
            let resolved = source
                .get_object(*id)
                .map_err(|error| AppError::Other(format!("dangling reference: {error}")))?
                .clone();
            let copied = copy_value(source, target, &resolved)?;
            Object::Reference(target.add_object(copied))
        }
        Object::Array(entries) => {
            let mut copied = Vec::with_capacity(entries.len());
            for entry in entries {
                copied.push(copy_value(source, target, entry)?);
            }
            Object::Array(copied)
        }
        Object::Dictionary(dictionary) => {
            let mut copied = Dictionary::new();
            for (key, entry) in dictionary.iter() {
                copied.set(key.clone(), copy_value(source, target, entry)?);
            }
            Object::Dictionary(copied)
        }
        Object::Stream(stream) => {
            let mut copied_dictionary = Dictionary::new();
            for (key, entry) in stream.dict.iter() {
                copied_dictionary.set(key.clone(), copy_value(source, target, entry)?);
            }
            Object::Stream(lopdf::Stream::new(
                copied_dictionary,
                stream.content.clone(),
            ))
        }
        other => other.clone(),
    })
}

/// Finds the root `/Pages` node via the catalog.
///
/// Walking the tree is not needed: the catalog's `/Pages` entry *is* the root,
/// and going through it means a document whose page tree is nested still gets
/// the correct node rather than the first `/Pages` encountered.
pub fn root_pages_id(document: &Document) -> Result<ObjectId, AppError> {
    let root = document
        .trailer
        .get(b"Root")
        .and_then(Object::as_reference)
        .map_err(|error| AppError::Other(format!("document has no /Root: {error}")))?;
    let catalog = document
        .get_object(root)
        .map_err(|error| AppError::Other(format!("catalog unreadable: {error}")))?
        .as_dict()
        .map_err(|error| AppError::Other(format!("catalog is not a dictionary: {error}")))?;
    catalog
        .get(b"Pages")
        .and_then(Object::as_reference)
        .map_err(|error| AppError::Other(format!("catalog has no /Pages: {error}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::content::{Content, Operation};
    use lopdf::Stream;

    /// Builds an N-page document where each page's content stream contains its own
    /// number, so tests can prove a page kept its identity through a copy.
    fn multi_page_pdf(pages: u32) -> Document {
        let mut document = Document::with_version("1.4");
        let pages_id = document.new_object_id();
        let font_id = document.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "Courier",
        });
        let resources_id = document.add_object(dictionary! {
            "Font" => dictionary! { "F1" => font_id },
        });

        let mut kids = Vec::new();
        for number in 1..=pages {
            let content = Content {
                operations: vec![
                    Operation::new("BT", vec![]),
                    Operation::new("Tf", vec!["F1".into(), 24.into()]),
                    Operation::new("Td", vec![72.into(), 720.into()]),
                    Operation::new("Tj", vec![Object::string_literal(format!("page {number}"))]),
                    Operation::new("ET", vec![]),
                ],
            };
            let content_id =
                document.add_object(Stream::new(dictionary! {}, content.encode().unwrap()));
            let page_id = document.add_object(dictionary! {
                "Type" => "Page",
                "Parent" => pages_id,
                "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
                "Contents" => content_id,
                "Resources" => resources_id,
            });
            kids.push(Object::Reference(page_id));
        }

        document.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => Object::Array(kids),
                "Count" => pages as i64,
            }),
        );
        let catalog_id = document.add_object(dictionary! {
            "Type" => "Catalog",
            "Pages" => pages_id,
        });
        document.trailer.set("Root", catalog_id);
        document.renumber_objects();
        document
    }

    fn page_count(document: &Document) -> usize {
        document.get_pages().len()
    }

    /// Reads a page's first text-showing operator, i.e. its identity label.
    fn page_label(document: &Document, page_number: u32) -> String {
        let page_id = *document.get_pages().get(&page_number).unwrap();
        let content = document.get_page_content(page_id);
        let text = String::from_utf8_lossy(&content).to_string();
        let start = text.find("(page").expect("label present");
        let rest = &text[start + 1..];
        let end = rest.find(')').unwrap();
        rest[..end].to_string()
    }

    #[test]
    fn rotation_accumulates_and_wraps() {
        let mut document = multi_page_pdf(1);
        let page_id = *document.get_pages().get(&1).unwrap();

        assert_eq!(current_rotation(&document, page_id).unwrap(), 0);
        assert_eq!(rotate_page(&mut document, page_id, 90).unwrap(), 90);
        assert_eq!(rotate_page(&mut document, page_id, 90).unwrap(), 180);
        assert_eq!(rotate_page(&mut document, page_id, 270).unwrap(), 90);
        assert_eq!(rotate_page(&mut document, page_id, -90).unwrap(), 0);
    }

    #[test]
    fn rotation_survives_a_save() {
        let mut document = multi_page_pdf(1);
        let page_id = *document.get_pages().get(&1).unwrap();
        rotate_page(&mut document, page_id, 90).unwrap();

        let mut buffer = Vec::new();
        document.save_to(&mut buffer).unwrap();
        let reloaded = Document::load_mem(&buffer).unwrap();
        let reloaded_page = *reloaded.get_pages().get(&1).unwrap();
        assert_eq!(current_rotation(&reloaded, reloaded_page).unwrap(), 90);
    }

    #[test]
    fn rejects_a_non_quarter_turn() {
        let mut document = multi_page_pdf(1);
        let page_id = *document.get_pages().get(&1).unwrap();
        let error = rotate_page(&mut document, page_id, 45).unwrap_err();
        assert!(error.to_string().contains("multiple of 90"));
    }

    #[test]
    fn delete_removes_the_named_pages_only() {
        let mut document = multi_page_pdf(5);
        // Delete pages 2 and 4: their labels are the proof of identity.
        delete_pages(&mut document, &[2, 4]).unwrap();

        assert_eq!(page_count(&document), 3);
        assert_eq!(page_label(&document, 1), "page 1");
        assert_eq!(page_label(&document, 2), "page 3");
        assert_eq!(page_label(&document, 3), "page 5");
    }

    #[test]
    fn delete_handles_descending_renumbering() {
        // Deleting {1, 2} naively in ascending order would delete page 1, then the
        // page that used to be 3. Both labels must be gone, and page 3 must remain.
        let mut document = multi_page_pdf(4);
        delete_pages(&mut document, &[1, 2]).unwrap();

        assert_eq!(page_count(&document), 2);
        assert_eq!(page_label(&document, 1), "page 3");
        assert_eq!(page_label(&document, 2), "page 4");
    }

    #[test]
    fn delete_rejects_removing_every_page() {
        // A PDF with zero pages is invalid; refusing beats writing a broken file.
        let mut document = multi_page_pdf(2);
        let error = delete_pages(&mut document, &[1, 2]).unwrap_err();
        assert!(error.to_string().contains("every page"));
    }

    #[test]
    fn delete_rejects_out_of_range_pages() {
        let mut document = multi_page_pdf(2);
        let error = delete_pages(&mut document, &[3]).unwrap_err();
        assert!(error.to_string().contains("out of range"));
        // Nothing should have been deleted by a rejected request.
        assert_eq!(page_count(&document), 2);
    }

    #[test]
    fn move_reorders_pages() {
        let mut document = multi_page_pdf(4);
        move_page(&mut document, 1, 3).unwrap();

        assert_eq!(page_label(&document, 1), "page 2");
        assert_eq!(page_label(&document, 2), "page 3");
        assert_eq!(page_label(&document, 3), "page 1");
        assert_eq!(page_label(&document, 4), "page 4");
    }

    #[test]
    fn move_rejects_out_of_range() {
        let mut document = multi_page_pdf(3);
        assert!(move_page(&mut document, 0, 1).is_err());
        assert!(move_page(&mut document, 1, 9).is_err());
    }

    #[test]
    fn extract_keeps_the_requested_pages_and_their_content() {
        let source = multi_page_pdf(5);
        let extracted = extract_pages(&source, &[4, 2]).unwrap();

        assert_eq!(page_count(&extracted), 2);
        // Requested order is preserved, not source order.
        assert_eq!(page_label(&extracted, 1), "page 4");
        assert_eq!(page_label(&extracted, 2), "page 2");
    }

    #[test]
    fn extracted_document_is_self_contained() {
        // If /Contents were copied shallowly, reloading would fail or the page
        // would render blank. Round-tripping proves the object graph came along.
        let source = multi_page_pdf(3);
        let mut extracted = extract_pages(&source, &[2]).unwrap();

        let mut buffer = Vec::new();
        extracted.save_to(&mut buffer).unwrap();
        let reloaded = Document::load_mem(&buffer).unwrap();
        assert_eq!(page_count(&reloaded), 1);
        assert_eq!(page_label(&reloaded, 1), "page 2");
    }

    #[test]
    fn extract_rejects_an_empty_selection() {
        let source = multi_page_pdf(2);
        assert!(extract_pages(&source, &[]).is_err());
    }

    #[test]
    fn split_produces_two_documents_covering_every_page() {
        let source = multi_page_pdf(5);
        let (first, second) = split_at(&source, 3).unwrap();

        assert_eq!(page_count(&first), 2);
        assert_eq!(page_count(&second), 3);
        assert_eq!(page_label(&first, 1), "page 1");
        assert_eq!(page_label(&first, 2), "page 2");
        assert_eq!(page_label(&second, 1), "page 3");
        assert_eq!(page_label(&second, 3), "page 5");
    }

    #[test]
    fn split_rejects_a_point_that_would_empty_a_document() {
        let source = multi_page_pdf(3);
        assert!(split_at(&source, 1).is_err());
        assert!(split_at(&source, 4).is_err());
        assert!(split_at(&source, 0).is_err());
    }

    #[test]
    fn merge_appends_every_page_of_the_second_document() {
        let mut first = multi_page_pdf(2);
        let second = multi_page_pdf(3);
        let total = merge(&mut first, &second).unwrap();

        assert_eq!(total, 5);
        assert_eq!(page_count(&first), 5);
        assert_eq!(page_label(&first, 1), "page 1");
        assert_eq!(page_label(&first, 3), "page 1"); // second document's first page
        assert_eq!(page_label(&first, 5), "page 3");
    }

    #[test]
    fn merged_document_is_self_contained() {
        let mut first = multi_page_pdf(1);
        let second = multi_page_pdf(2);
        merge(&mut first, &second).unwrap();

        let mut buffer = Vec::new();
        first.save_to(&mut buffer).unwrap();
        let reloaded = Document::load_mem(&buffer).unwrap();
        assert_eq!(page_count(&reloaded), 3);
        assert_eq!(page_label(&reloaded, 3), "page 2");
    }

    #[test]
    fn merge_rejects_an_empty_source() {
        let mut first = multi_page_pdf(1);
        let empty = Document::with_version("1.4");
        assert!(merge(&mut first, &empty).is_err());
    }

    #[test]
    fn root_pages_id_reports_a_catalog_less_document() {
        let document = Document::with_version("1.4");
        assert!(root_pages_id(&document).is_err());
    }

    #[test]
    fn split_then_merge_round_trips_page_count() {
        let source = multi_page_pdf(4);
        let (mut first, second) = split_at(&source, 2).unwrap();
        let total = merge(&mut first, &second).unwrap();
        assert_eq!(total, 4);
        assert_eq!(page_label(&first, 1), "page 1");
        assert_eq!(page_label(&first, 4), "page 4");
    }
}
