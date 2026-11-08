//! D-RD5's closed list, one test per class.
//!
//! Each class gets its own test because a single "sanitize removed 7 things" assertion lets a
//! broken class hide behind six working ones. The negative direction is tested too: a
//! document with none of these classes must come back with an all-zero report, so a pass that
//! clears everything unconditionally is caught rather than looking thorough.

use lopdf::{dictionary, Document, Object, Stream};

use document_reader_lib::editor::sanitize::sanitize;

/// A one-page document with nothing sanitize should touch.
fn clean() -> Document {
    let mut doc = Document::with_version("1.7");
    let content_id = doc.add_object(Stream::new(dictionary! {}, b"BT ET".to_vec()));
    let pages_id = doc.new_object_id();
    let page_id = doc.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => Object::Reference(pages_id),
        "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        "Contents" => Object::Reference(content_id),
    });
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => vec![Object::Reference(page_id)],
            "Count" => 1,
        }),
    );
    let catalog_id = doc.new_object_id();
    doc.objects.insert(
        catalog_id,
        Object::Dictionary(dictionary! {
            "Type" => "Catalog",
            "Pages" => Object::Reference(pages_id),
        }),
    );
    doc.trailer.set("Root", Object::Reference(catalog_id));
    doc
}

fn catalog_id(doc: &Document) -> lopdf::ObjectId {
    doc.trailer
        .get(b"Root")
        .and_then(Object::as_reference)
        .expect("a catalog")
}

#[test]
fn a_clean_document_reports_nothing_removed() {
    // The direction that catches an over-eager wipe: if sanitize cleared things
    // unconditionally, every count here would be non-zero and the report would be a lie.
    let mut doc = clean();
    let report = sanitize(&mut doc).expect("sanitize");

    assert!(
        report.is_empty(),
        "a document with no metadata must report nothing removed, got {report:?}"
    );
}

#[test]
fn info_metadata_is_dropped() {
    let mut doc = clean();
    let info_id = doc.add_object(dictionary! {
        "Author" => Object::string_literal("A Person"),
        "Producer" => Object::string_literal("DocuPro"),
    });
    doc.trailer.set("Info", Object::Reference(info_id));

    let report = sanitize(&mut doc).expect("sanitize");

    assert!(report.info_dropped, "D-RD5: /Info must be dropped");
    assert!(
        doc.trailer.get(b"Info").is_err(),
        "the trailer must no longer point at /Info"
    );
    let bytes = format!("{:?}", doc.objects);
    assert!(
        !bytes.contains("A Person"),
        "the author string must not survive in any object"
    );
}

#[test]
fn xmp_metadata_is_dropped() {
    let mut doc = clean();
    let metadata_id = doc.add_object(Stream::new(
        dictionary! { "Type" => "Metadata", "Subtype" => "XML" },
        b"<x:xmpmeta>hidden</x:xmpmeta>".to_vec(),
    ));
    let catalog = catalog_id(&doc);
    doc.get_object_mut(catalog)
        .and_then(Object::as_dict_mut)
        .unwrap()
        .set("Metadata", Object::Reference(metadata_id));

    let report = sanitize(&mut doc).expect("sanitize");

    assert!(report.xmp_dropped, "D-RD5: XMP /Metadata must be dropped");
    let bytes = format!("{:?}", doc.objects);
    assert!(!bytes.contains("hidden"), "the XMP body must not survive");
}

#[test]
fn outlines_are_dropped_and_counted() {
    let mut doc = clean();
    let catalog = catalog_id(&doc);

    // Two items: "Chapter" with a child "Section".
    let section_id = doc.add_object(dictionary! { "Title" => Object::string_literal("Section") });
    let chapter_id = doc.add_object(dictionary! {
        "Title" => Object::string_literal("Chapter"),
        "First" => Object::Reference(section_id),
    });
    let outlines_id = doc.add_object(dictionary! {
        "Type" => "Outlines",
        "First" => Object::Reference(chapter_id),
        "Count" => 2,
    });
    doc.get_object_mut(catalog)
        .and_then(Object::as_dict_mut)
        .unwrap()
        .set("Outlines", Object::Reference(outlines_id));

    let report = sanitize(&mut doc).expect("sanitize");

    assert_eq!(
        report.outlines_removed, 2,
        "D-RD5: both the chapter and its child must be counted"
    );
    assert!(doc
        .get_object(catalog)
        .and_then(Object::as_dict)
        .map(|catalog| catalog.get(b"Outlines").is_err())
        .unwrap_or(false));
}

#[test]
fn a_self_referencing_outline_does_not_hang() {
    // A malformed `/Next` that points at its own item would recurse forever without a bound.
    // Sanitize that hangs is a denial of service on the user's own file.
    let mut doc = clean();
    let catalog = catalog_id(&doc);
    let item_id = doc.new_object_id();
    doc.objects.insert(
        item_id,
        Object::Dictionary(dictionary! {
            "Title" => Object::string_literal("Loop"),
            "Next" => Object::Reference(item_id),
        }),
    );
    let outlines_id = doc.add_object(dictionary! {
        "Type" => "Outlines",
        "First" => Object::Reference(item_id),
    });
    doc.get_object_mut(catalog)
        .and_then(Object::as_dict_mut)
        .unwrap()
        .set("Outlines", Object::Reference(outlines_id));

    let report = sanitize(&mut doc).expect("sanitize must terminate");

    assert!(
        report.outlines_removed > 0,
        "the loop must be counted once and stopped, got {report:?}"
    );
}

#[test]
fn embedded_files_are_dropped() {
    let mut doc = clean();
    let catalog = catalog_id(&doc);

    let filespec_id = doc.add_object(dictionary! {
        "Type" => "Filespec",
        "F" => Object::string_literal("payload.bin"),
    });
    let tree_id = doc.add_object(dictionary! {
        "Names" => vec![
            Object::string_literal("payload.bin"),
            Object::Reference(filespec_id),
        ],
    });
    let names_id = doc.add_object(dictionary! {
        "EmbeddedFiles" => Object::Reference(tree_id),
    });
    doc.get_object_mut(catalog)
        .and_then(Object::as_dict_mut)
        .unwrap()
        .set("Names", Object::Reference(names_id));

    let report = sanitize(&mut doc).expect("sanitize");

    assert_eq!(
        report.embedded_files_removed, 1,
        "D-RD5: the embedded file must be counted and dropped"
    );
    let names = doc
        .get_object(names_id)
        .and_then(Object::as_dict)
        .expect("the /Names dictionary still exists");
    assert!(
        names.get(b"EmbeddedFiles").is_err(),
        "the attachment tree must be gone"
    );
}

#[test]
fn annotation_text_outside_a_mark_is_cleared() {
    let mut doc = clean();
    let page_id = doc.get_pages().values().copied().next().unwrap();
    let annot_id = doc.add_object(dictionary! {
        "Type" => "Annot",
        "Subtype" => "Text",
        "Rect" => vec![10.into(), 10.into(), 20.into(), 20.into()],
        "Contents" => Object::string_literal("a secret remark"),
        "T" => Object::string_literal("Reviewer Name"),
    });
    doc.get_object_mut(page_id)
        .and_then(Object::as_dict_mut)
        .unwrap()
        .set("Annots", vec![Object::Reference(annot_id)]);

    let report = sanitize(&mut doc).expect("sanitize");

    assert_eq!(
        report.annotation_texts_cleared, 2,
        "both /Contents and /T must be cleared"
    );
    let bytes = format!("{:?}", doc.objects);
    assert!(!bytes.contains("a secret remark"), "/Contents must be gone");
    assert!(!bytes.contains("Reviewer Name"), "/T must be gone");
}

#[test]
fn form_field_values_are_cleared() {
    let mut doc = clean();
    let catalog = catalog_id(&doc);

    let field_id = doc.add_object(dictionary! {
        "Type" => "Annot",
        "Subtype" => "Widget",
        "T" => Object::string_literal("ssn"),
        "V" => Object::string_literal("123-45-6789"),
        "Rect" => vec![0.into(), 0.into(), 10.into(), 10.into()],
    });
    let acro_id = doc.add_object(dictionary! {
        "Fields" => vec![Object::Reference(field_id)],
    });
    doc.get_object_mut(catalog)
        .and_then(Object::as_dict_mut)
        .unwrap()
        .set("AcroForm", Object::Reference(acro_id));

    let report = sanitize(&mut doc).expect("sanitize");

    assert_eq!(report.form_values_cleared, 1, "D-RD5: the /V must go");
    let bytes = format!("{:?}", doc.objects);
    assert!(
        !bytes.contains("123-45-6789"),
        "the field value must not survive anywhere in the file"
    );
}

#[test]
fn document_javascript_is_dropped() {
    let mut doc = clean();
    let catalog = catalog_id(&doc);

    let action_id = doc.add_object(dictionary! {
        "Type" => "Action",
        "S" => "JavaScript",
        "JS" => Object::string_literal("app.alert('tracked')"),
    });
    doc.get_object_mut(catalog)
        .and_then(Object::as_dict_mut)
        .unwrap()
        .set("OpenAction", Object::Reference(action_id));

    let report = sanitize(&mut doc).expect("sanitize");

    assert_eq!(
        report.javascript_removed, 1,
        "D-RD5: the document JavaScript must be dropped"
    );
    let bytes = format!("{:?}", doc.objects);
    assert!(
        !bytes.contains("app.alert"),
        "the script source must not survive"
    );
}
