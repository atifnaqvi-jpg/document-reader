//! Destructive-redaction oracles (P3.2 I-RD1, I-RD2, I-RD3, I-RD4).
//!
//! The point of these is to check the *file*, not the function that wrote it. I-RD1 asks
//! PDFium — an engine this project did not write — what text the output page shows. I-RD2 and
//! I-RD3 read the raw bytes back, with every stream decompressed, so a run that survives in an
//! object nobody walks is caught even when the page looks clean.
//!
//! The fixture is built here rather than loaded: the defect under test is "which bytes are in
//! the file", and a fixture would hide which run went where.

use lopdf::{dictionary, Document, Object, Stream};
use std::path::PathBuf;

use document_reader_lib::editor::object_gc::dangling_references;
use document_reader_lib::editor::redact::{redact_page, Rect};

const SECRET: &str = "TOPSECRETVALUE";
const KEPT: &str = "KEEPME";

/// A one-page PDF whose content stream draws two text runs at known baselines:
/// the secret at y = 700 and the kept string at y = 650, both at x = 72.
fn fixture() -> Document {
    let mut doc = Document::with_version("1.7");

    let content =
        format!("BT /F1 12 Tf 72 700 Td ({SECRET}) Tj ET\nBT /F1 12 Tf 72 650 Td ({KEPT}) Tj ET\n");

    let content_id = doc.new_object_id();
    doc.objects.insert(
        content_id,
        Stream::new(dictionary! {}, content.into_bytes()).into(),
    );

    let font_id = doc.new_object_id();
    doc.objects.insert(
        font_id,
        Object::Dictionary(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "Helvetica",
        }),
    );

    let pages_id = doc.new_object_id();
    let page_id = doc.new_object_id();
    doc.objects.insert(
        page_id,
        Object::Dictionary(dictionary! {
            "Type" => "Page",
            "Parent" => Object::Reference(pages_id),
            "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
            "Contents" => Object::Reference(content_id),
            "Resources" => dictionary! {
                "Font" => dictionary! { "F1" => Object::Reference(font_id) },
            },
        }),
    );
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

fn write_temp(doc: &mut Document, name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("redact-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    doc.save(&path).unwrap();
    path
}

/// Every decompressed byte of every object, concatenated (I-RD3's raw material).
fn all_object_bytes(doc: &Document) -> Vec<u8> {
    let mut out = Vec::new();
    for object in doc.objects.values() {
        match object {
            Object::Stream(stream) => {
                match stream.decompressed_content() {
                    Ok(bytes) => out.extend_from_slice(&bytes),
                    Err(_) => out.extend_from_slice(&stream.content),
                }
                out.extend_from_slice(format!("{:?}", stream.dict).as_bytes());
            }
            other => out.extend_from_slice(&format!("{other:?}").into_bytes()),
        }
    }
    out
}

fn contains(haystack: &[u8], needle: &str) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle.as_bytes())
}

/// The rectangle covering the secret's run and nothing else.
fn secret_rect() -> Rect {
    // Baseline 700, 12 pt type: the box runs from just under the baseline to just above.
    Rect::new(70.0, 690.0, 200.0, 706.0)
}

#[test]
fn redaction_removes_the_text_from_the_object_graph() {
    let mut doc = fixture();
    assert!(
        contains(&all_object_bytes(&doc), SECRET),
        "fixture sanity: the secret must be in the file's objects before redaction"
    );

    let report = redact_page(&mut doc, 0, &[secret_rect()]).expect("redact");

    assert_eq!(report.runs_removed, 1, "exactly one run must be deleted");
    let after = all_object_bytes(&doc);
    assert!(
        !contains(&after, SECRET),
        "I-RD3: the removed text must appear nowhere in the decompressed object graph. \
         Mutation: paint a black box instead of deleting the operator leaves the string \
         here and fails this."
    );
    assert!(
        contains(&after, KEPT),
        "the run outside the rectangle must survive (D-RD3 is about ambiguity, not about \
         removing the page)"
    );
}

#[test]
fn the_output_reopens_with_no_dangling_references() {
    let mut doc = fixture();
    redact_page(&mut doc, 0, &[secret_rect()]).expect("redact");

    assert_eq!(
        dangling_references(&doc),
        vec![],
        "I-RD4: every reference from a reachable object must still resolve"
    );

    let path = write_temp(&mut doc, "rd4.pdf");
    let reopened = Document::load(&path).expect("the output must parse");
    assert_eq!(
        reopened.get_pages().len(),
        1,
        "I-RD5: the page count must survive the rewrite"
    );
}

#[test]
fn the_kept_run_keeps_its_position_and_the_page_keeps_its_geometry() {
    let mut doc = fixture();
    redact_page(&mut doc, 0, &[secret_rect()]).expect("redact");

    let page_id = doc.get_pages().values().copied().next().unwrap();
    let page = doc.get_object(page_id).unwrap().as_dict().unwrap();
    let media = page.get(b"MediaBox").unwrap().as_array().unwrap();
    assert_eq!(media.len(), 4, "I-RD5: MediaBox must still be a rectangle");
    for (index, expected) in [0, 0, 612, 792].iter().enumerate() {
        assert_eq!(
            media[index].as_i64().unwrap(),
            *expected,
            "I-RD5: MediaBox corner {index} changed"
        );
    }

    let content = document_reader_lib::editor::textlocate::decoded_content(&doc, page_id).unwrap();
    let shows = content
        .operations
        .iter()
        .filter(|operation| operation.operator == "Tj")
        .count();
    assert_eq!(
        shows, 1,
        "exactly one Tj must remain after removing the other"
    );
}

#[test]
fn an_empty_mark_set_changes_nothing() {
    let mut doc = fixture();
    let before = all_object_bytes(&doc);

    let report = redact_page(&mut doc, 0, &[]).expect("redact");

    assert_eq!(report.runs_removed, 0);
    assert_eq!(
        all_object_bytes(&doc),
        before,
        "I-RD6: applying nothing must be a no-op on the content"
    );
    assert!(contains(&all_object_bytes(&doc), SECRET));
}

#[test]
fn a_mark_beside_the_text_removes_nothing() {
    let mut doc = fixture();
    // Far to the right of both runs: the text is at x = 72, this is x = 400.
    let report =
        redact_page(&mut doc, 0, &[Rect::new(400.0, 640.0, 500.0, 720.0)]).expect("redact");

    assert_eq!(
        report.runs_removed, 0,
        "a mark that touches no run must delete no run"
    );
    let after = all_object_bytes(&doc);
    assert!(contains(&after, SECRET) && contains(&after, KEPT));
}

#[test]
fn a_mark_covering_both_runs_removes_both() {
    let mut doc = fixture();
    let report = redact_page(&mut doc, 0, &[Rect::new(60.0, 600.0, 300.0, 720.0)]).expect("redact");

    assert_eq!(report.runs_removed, 2);
    let after = all_object_bytes(&doc);
    assert!(!contains(&after, SECRET) && !contains(&after, KEPT));
}

#[test]
fn removing_the_highest_numbered_object_leaves_the_xref_consistent() {
    let mut doc = fixture();
    // An orphan holding the secret, numbered above everything else, so the sweep has to drop
    // the document's maximum id and the xref `/Size` has to follow.
    let orphan: (u32, u16) = (9000, 0);
    doc.objects.insert(orphan, Object::string_literal(SECRET));
    doc.max_id = 9000;

    redact_page(&mut doc, 0, &[secret_rect()]).expect("redact");

    assert!(
        !doc.objects.contains_key(&orphan),
        "the orphan must be swept"
    );
    assert!(
        doc.max_id < 9000,
        "max_id must fall to the surviving maximum"
    );
    let path = write_temp(&mut doc, "rd4-xref.pdf");
    Document::load(&path).expect("I-RD4: the xref and object table must agree with the file");
}

// ---- D-RD2: annotations and images ----

use lopdf::dictionary as dict_macro;

/// Adds one square annotation at `rect` to page 0, returning its object id.
fn add_square(doc: &mut Document, rect: [i64; 4], contents: &str) -> lopdf::ObjectId {
    let annot_id = doc.new_object_id();
    doc.objects.insert(
        annot_id,
        Object::Dictionary(dict_macro! {
            "Type" => "Annot",
            "Subtype" => "Square",
            "Rect" => vec![rect[0].into(), rect[1].into(), rect[2].into(), rect[3].into()],
            "Contents" => Object::string_literal(contents),
        }),
    );
    let page_id = doc.get_pages().values().copied().next().unwrap();
    let annots = doc
        .get_object(page_id)
        .and_then(Object::as_dict)
        .and_then(|page| page.get(b"Annots").cloned())
        .unwrap_or(Object::Array(Vec::new()));
    let mut array = match annots {
        Object::Array(array) => array,
        other => vec![other],
    };
    array.push(Object::Reference(annot_id));
    doc.get_object_mut(page_id)
        .and_then(Object::as_dict_mut)
        .unwrap()
        .set("Annots", array);
    annot_id
}

#[test]
fn an_annotation_inside_the_mark_is_deleted_and_its_text_goes_with_it() {
    let mut doc = fixture();
    let annot_id = add_square(&mut doc, [80, 692, 180, 704], "note under the mark");
    assert!(contains(&all_object_bytes(&doc), "note under the mark"));

    let report = redact_page(&mut doc, 0, &[secret_rect()]).expect("redact");

    assert_eq!(
        report.annotations_removed, 1,
        "the intersecting note must go"
    );
    assert!(
        !doc.objects.contains_key(&annot_id)
            || !contains(&all_object_bytes(&doc), "note under the mark"),
        "D-RD2: the annotation's /Contents must not survive in the file"
    );
}

#[test]
fn an_annotation_outside_the_mark_survives() {
    let mut doc = fixture();
    add_square(&mut doc, [400, 400, 500, 500], "far from the mark");

    let report = redact_page(&mut doc, 0, &[secret_rect()]).expect("redact");

    assert_eq!(
        report.annotations_removed, 0,
        "an annotation outside every mark must be kept (over-removal of metadata is a \
         different failure from over-removal of content)"
    );
    assert!(contains(&all_object_bytes(&doc), "far from the mark"));
}

#[test]
fn an_image_drawn_under_the_mark_is_removed_whole() {
    let mut doc = fixture();
    let page_id = doc.get_pages().values().copied().next().unwrap();

    // A 2x2 image drawn over the secret's area via `cm`.
    let image_id = doc.new_object_id();
    doc.objects.insert(
        image_id,
        Stream::new(
            dict_macro! {
                "Type" => "XObject",
                "Subtype" => "Image",
                "Width" => 2,
                "Height" => 2,
                "ColorSpace" => "DeviceGray",
                "BitsPerComponent" => 8,
            },
            vec![0u8; 4],
        )
        .into(),
    );
    let resources = doc
        .get_object_mut(page_id)
        .and_then(Object::as_dict_mut)
        .unwrap()
        .get_mut(b"Resources")
        .unwrap()
        .as_dict_mut()
        .unwrap();
    resources.set(
        "XObject",
        dict_macro! { "Im1" => Object::Reference(image_id) },
    );

    // Append the `Do` after the text, scaled to 100x100 at (72, 690).
    let content_id = doc
        .get_object(page_id)
        .and_then(Object::as_dict)
        .and_then(|page| page.get(b"Contents"))
        .and_then(Object::as_reference)
        .unwrap();
    let stream = doc
        .get_object_mut(content_id)
        .unwrap()
        .as_stream_mut()
        .unwrap();
    let mut bytes = stream.content.clone();
    bytes.extend_from_slice(b"\nq 100 0 0 100 72 690 cm /Im1 Do Q\n");
    stream.set_content(bytes);

    let report = redact_page(&mut doc, 0, &[secret_rect()]).expect("redact");

    assert_eq!(
        report.images_removed, 1,
        "D-RD7: the intersecting image goes whole"
    );
    let content = document_reader_lib::editor::textlocate::decoded_content(&doc, page_id).unwrap();
    assert!(
        !content
            .operations
            .iter()
            .any(|operation| operation.operator == "Do"),
        "the Do that drew the removed image must be gone"
    );
    assert!(
        !doc.objects.contains_key(&image_id),
        "the image object must be swept once nothing references it"
    );
}

#[test]
fn an_image_far_from_every_mark_survives() {
    let mut doc = fixture();
    let page_id = doc.get_pages().values().copied().next().unwrap();
    let image_id = doc.new_object_id();
    doc.objects.insert(
        image_id,
        Stream::new(
            dict_macro! {
                "Type" => "XObject",
                "Subtype" => "Image",
                "Width" => 2,
                "Height" => 2,
                "ColorSpace" => "DeviceGray",
                "BitsPerComponent" => 8,
            },
            vec![0u8; 4],
        )
        .into(),
    );
    doc.get_object_mut(page_id)
        .and_then(Object::as_dict_mut)
        .unwrap()
        .get_mut(b"Resources")
        .unwrap()
        .as_dict_mut()
        .unwrap()
        .set(
            "XObject",
            dict_macro! { "Im1" => Object::Reference(image_id) },
        );

    let content_id = doc
        .get_object(page_id)
        .and_then(Object::as_dict)
        .and_then(|page| page.get(b"Contents"))
        .and_then(Object::as_reference)
        .unwrap();
    let stream = doc
        .get_object_mut(content_id)
        .unwrap()
        .as_stream_mut()
        .unwrap();
    let mut bytes = stream.content.clone();
    bytes.extend_from_slice(b"\nq 50 0 0 50 400 400 cm /Im1 Do Q\n");
    stream.set_content(bytes);

    let report = redact_page(&mut doc, 0, &[secret_rect()]).expect("redact");

    assert_eq!(
        report.images_removed, 0,
        "an image outside every mark must stay"
    );
    assert!(doc.objects.contains_key(&image_id));
}

#[test]
fn an_annotation_whose_rect_cannot_be_read_is_removed() {
    // D-RD3's fail-safe direction. A `/Rect` that is missing, malformed or not a number means
    // the extent is unknown, and an annotation of unknown extent might sit under the mark.
    // Keeping it is under-removal -- a silent disclosure -- which the phase forbids.
    let mut doc = fixture();
    let annot_id = doc.new_object_id();
    doc.objects.insert(
        annot_id,
        Object::Dictionary(dict_macro! {
            "Type" => "Annot",
            "Subtype" => "Square",
            // Three numbers, not four: not a rectangle.
            "Rect" => vec![80.into(), 692.into(), 180.into()],
            "Contents" => Object::string_literal("malformed rect note"),
        }),
    );
    let page_id = doc.get_pages().values().copied().next().unwrap();
    doc.get_object_mut(page_id)
        .and_then(Object::as_dict_mut)
        .unwrap()
        .set("Annots", vec![Object::Reference(annot_id)]);

    let report = redact_page(&mut doc, 0, &[secret_rect()]).expect("redact");

    assert_eq!(
        report.annotations_removed, 1,
        "D-RD3: an annotation whose box cannot be read must be removed, not kept"
    );
    assert!(
        !contains(&all_object_bytes(&doc), "malformed rect note"),
        "its /Contents must not survive in the file"
    );
}

// ---- I-RD7: the marks must not survive as marks ----

use document_reader_lib::editor::redact::apply_redactions;

#[test]
fn a_pre_existing_redact_annotation_is_stripped() {
    // Another tool's redaction mark carries the rectangle it covers in /QuadPoints, so it
    // tells a viewer exactly what was removed. I-RD7: the output contains no /Redact.
    let mut doc = fixture();
    let annot_id = doc.new_object_id();
    doc.objects.insert(
        annot_id,
        Object::Dictionary(dict_macro! {
            "Type" => "Annot",
            "Subtype" => "Redact",
            "Rect" => vec![72.into(), 690.into(), 200.into(), 706.into()],
            "QuadPoints" => vec![72.into(), 706.into(), 200.into(), 706.into()],
        }),
    );
    let page_id = doc.get_pages().values().copied().next().unwrap();
    doc.get_object_mut(page_id)
        .and_then(Object::as_dict_mut)
        .unwrap()
        .set("Annots", vec![Object::Reference(annot_id)]);

    let report = apply_redactions(&mut doc, &[(0, vec![secret_rect()])]).expect("apply");

    assert_eq!(
        report
            .pages
            .iter()
            .map(|p| p.redact_annotations_stripped)
            .sum::<usize>(),
        1,
        "I-RD7: the /Redact annotation must be stripped"
    );
    let bytes = format!("{:?}", doc.objects);
    assert!(
        !bytes.contains("Redact"),
        "I-RD7: no /Redact subtype may remain anywhere in the file"
    );
}

#[test]
fn apply_runs_sanitize_and_the_result_has_no_metadata() {
    // D-RD5 says sanitize is part of apply, not a separate opt-in step. If a caller could
    // apply without sanitizing, the "remove hidden information" claim would be opt-in and
    // skippable, which is what the decision forbids.
    let mut doc = fixture();
    let info_id = doc.add_object(dict_macro! {
        "Author" => Object::string_literal("A Person"),
    });
    doc.trailer.set("Info", Object::Reference(info_id));

    let report = apply_redactions(&mut doc, &[(0, vec![secret_rect()])]).expect("apply");

    assert!(
        report.sanitized.info_dropped,
        "D-RD5: apply must have run sanitize"
    );
    let bytes = format!("{:?}", doc.objects);
    assert!(
        !bytes.contains("A Person"),
        "the author must not survive the apply"
    );
}

#[test]
fn apply_removes_text_and_sanitizes_in_one_pass() {
    let mut doc = fixture();
    let info_id = doc.add_object(dict_macro! {
        "Author" => Object::string_literal("Hidden Author"),
    });
    doc.trailer.set("Info", Object::Reference(info_id));

    let report = apply_redactions(&mut doc, &[(0, vec![secret_rect()])]).expect("apply");

    assert!(report.runs_removed() > 0, "the text run must be removed");
    assert!(report.sanitized.info_dropped, "and /Info dropped");
    let bytes = all_object_bytes(&doc);
    assert!(!contains(&bytes, "TOPSECRET"), "the text must be gone");
    assert!(
        !contains(&bytes, "Hidden Author"),
        "the metadata must be gone"
    );
}

#[test]
fn apply_with_no_marks_still_sanitizes_but_removes_no_content() {
    // An empty mark set is a legal call (the panel's Apply with nothing marked). It must not
    // remove content, but sanitize is part of apply, so it still runs.
    let mut doc = fixture();
    let info_id = doc.add_object(dict_macro! {
        "Author" => Object::string_literal("Hidden Author"),
    });
    doc.trailer.set("Info", Object::Reference(info_id));

    let report = apply_redactions(&mut doc, &[]).expect("apply");

    assert_eq!(
        report.runs_removed(),
        0,
        "no marks means no content removal"
    );
    assert!(report.pages.is_empty(), "no page had marks");
    assert!(report.sanitized.info_dropped, "sanitize still runs");
    assert!(contains(&all_object_bytes(&doc), "TOPSECRET"));
}

#[test]
fn stripping_redact_marks_keeps_other_annotations() {
    // The over-removal direction of I-RD7. Strip only `/Subtype /Redact`: a highlight or a
    // comment is the user's own content, and deleting it while claiming to remove hidden
    // information would be a different failure, not a safer one.
    use document_reader_lib::editor::redact::strip_redact_annotations;

    let mut doc = fixture();
    let redact_id = doc.new_object_id();
    doc.objects.insert(
        redact_id,
        Object::Dictionary(dict_macro! {
            "Type" => "Annot",
            "Subtype" => "Redact",
            "Rect" => vec![72.into(), 690.into(), 200.into(), 706.into()],
        }),
    );
    let highlight_id = doc.new_object_id();
    doc.objects.insert(
        highlight_id,
        Object::Dictionary(dict_macro! {
            "Type" => "Annot",
            "Subtype" => "Highlight",
            "Rect" => vec![72.into(), 640.into(), 200.into(), 656.into()],
        }),
    );
    let page_id = doc.get_pages().values().copied().next().unwrap();
    doc.get_object_mut(page_id)
        .and_then(Object::as_dict_mut)
        .unwrap()
        .set(
            "Annots",
            vec![
                Object::Reference(redact_id),
                Object::Reference(highlight_id),
            ],
        );

    let stripped = strip_redact_annotations(&mut doc).expect("strip");

    assert_eq!(stripped, 1, "only the /Redact annotation goes");
    assert!(
        doc.objects.contains_key(&highlight_id),
        "a highlight is the user's content and must survive"
    );
    let annots = doc
        .get_object(page_id)
        .and_then(Object::as_dict)
        .and_then(|page| page.get(b"Annots"))
        .and_then(Object::as_array)
        .expect("the surviving annotation stays in /Annots");
    assert_eq!(annots.len(), 1, "exactly one annotation remains");
}
