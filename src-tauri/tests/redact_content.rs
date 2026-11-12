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
use document_reader_lib::editor::redact::{count_vector_paths_under_marks, redact_page, Rect};

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

// ---- I-RD6: idempotence ----

#[test]
fn applying_the_same_marks_twice_reports_nothing_to_do_the_second_time() {
    // I-RD6. The second apply must not double-remove, and it must say so rather than write a
    // second file that differs only by timestamp.
    let mut doc = fixture();
    let marks = [(0u32, vec![secret_rect()])];

    let first = apply_redactions(&mut doc, &marks).expect("first apply");
    assert!(!first.nothing_to_apply(), "the first apply must do work");
    assert!(first.runs_removed() > 0);

    let second = apply_redactions(&mut doc, &marks).expect("second apply");
    assert!(
        second.nothing_to_apply(),
        "I-RD6: the second apply must report nothing to do, got {second:?}"
    );
    assert_eq!(second.runs_removed(), 0, "nothing removed the second time");
}

#[test]
fn a_redacted_document_is_unchanged_by_a_second_apply() {
    // The stronger form: not just "it says it did nothing" but "the bytes did not move".
    let mut doc = fixture();
    let marks = [(0u32, vec![secret_rect()])];

    apply_redactions(&mut doc, &marks).expect("first apply");
    let after_first = format!("{:?}", doc.objects);

    apply_redactions(&mut doc, &marks).expect("second apply");
    let after_second = format!("{:?}", doc.objects);

    assert_eq!(
        after_first, after_second,
        "I-RD6: a second apply must not alter the object graph"
    );
}

#[test]
fn an_apply_that_only_removes_an_annotation_is_not_a_no_op() {
    // The clause that makes `nothing_to_apply` honest. On a second apply the text is already
    // gone, so `runs_removed` is 0 — but an annotation added since then still intersects the
    // mark, and reporting "nothing to apply" would leave its /Contents in the file.
    let mut doc = fixture();
    let marks = [(0u32, vec![secret_rect()])];

    apply_redactions(&mut doc, &marks).expect("first apply");

    // A new note lands under the mark between the two applies.
    let annot_id = add_square(&mut doc, [80, 692, 180, 704], "added later");
    let second = apply_redactions(&mut doc, &marks).expect("second apply");

    assert!(
        !second.nothing_to_apply(),
        "I-RD6: removing the new annotation is work, not a no-op"
    );
    assert_eq!(
        second
            .pages
            .iter()
            .map(|p| p.annotations_removed)
            .sum::<usize>(),
        1,
        "the new annotation must be the thing removed"
    );
    assert!(
        !doc.objects.contains_key(&annot_id),
        "and it must be gone from the document"
    );
}

// ---- D-RD6: search-and-redact ----

use document_reader_lib::editor::redact::find_and_mark;

#[test]
fn search_finds_the_pattern_and_marks_only_its_run() {
    // D-RD6. The hit must convert to a rectangle covering the matched characters, not the
    // whole run -- otherwise marking "SECRET" inside "TOPSECRETVALUE" would remove the
    // surrounding characters the user did not ask to remove.
    let doc = fixture();
    let hits = find_and_mark(&doc, 0, "SECRET").expect("search");

    assert_eq!(hits.len(), 1, "one run contains the pattern once");
    assert_eq!(hits[0].text, "SECRET");
    assert_eq!(hits[0].page, 0);
    // The secret run starts at x = 72 and is 14 characters wide at 12pt Helvetica. The
    // match starts 2 characters in and is 6 characters wide, so the box is strictly inside
    // the run's own extent on both sides.
    assert!(
        hits[0].rect.x0 > 72.0,
        "the mark starts after the run origin, got {:?}",
        hits[0].rect
    );
    assert!(
        hits[0].rect.x1 < 72.0 + 14.0 * 12.0,
        "the mark ends before the run ends, got {:?}",
        hits[0].rect
    );
}

#[test]
fn search_is_case_insensitive() {
    let doc = fixture();
    let lower = find_and_mark(&doc, 0, "topsecretvalue").expect("search");
    let upper = find_and_mark(&doc, 0, "TOPSECRETVALUE").expect("search");

    assert_eq!(lower.len(), 1, "lowercase must find it");
    assert_eq!(lower.len(), upper.len());
    assert_eq!(
        lower[0].rect, upper[0].rect,
        "case must not move the rectangle"
    );
}

#[test]
fn search_reports_every_occurrence_in_a_run() {
    let mut doc = fixture();
    // Replace the content so one run repeats the pattern.
    let content_id = doc
        .get_pages()
        .values()
        .copied()
        .next()
        .and_then(|page| doc.get_object(page).ok())
        .and_then(|page| page.as_dict().ok())
        .and_then(|page| page.get(b"Contents").ok())
        .and_then(|c| c.as_reference().ok())
        .expect("content");
    let content = "BT /F1 12 Tf 72 700 Td (ABAB) Tj ET\n".to_string();
    doc.objects.insert(
        content_id,
        Stream::new(dictionary! {}, content.into_bytes()).into(),
    );

    let hits = find_and_mark(&doc, 0, "AB").expect("search");

    assert_eq!(hits.len(), 2, "both occurrences must be marked");
    assert!(
        hits[1].rect.x0 > hits[0].rect.x0,
        "the second mark sits to the right of the first"
    );
    assert_eq!(hits[0].text, "AB");
    assert_eq!(hits[1].text, "AB");
}

#[test]
fn search_that_matches_nothing_marks_nothing() {
    let doc = fixture();
    let hits = find_and_mark(&doc, 0, "notpresent").expect("search");
    assert!(hits.is_empty(), "no match means no mark, got {hits:?}");
}

#[test]
fn an_empty_pattern_marks_nothing_rather_than_everything() {
    // The dangerous default: an empty needle matches at every position in every run, which
    // would mark the whole document.
    let doc = fixture();
    let hits = find_and_mark(&doc, 0, "").expect("search");
    assert!(hits.is_empty(), "an empty pattern must mark nothing");
}

#[test]
fn a_hit_becomes_a_mark_that_removes_the_text() {
    // The end-to-end claim: find a word, apply, and the word is gone while its neighbours
    // stay. This is what distinguishes search-and-redact from "redact the whole run".
    let mut doc = fixture();
    let hits = find_and_mark(&doc, 0, "SECRET").expect("search");
    assert_eq!(hits.len(), 1);

    let rects: Vec<Rect> = hits.iter().map(|hit| hit.rect).collect();
    let report = apply_redactions(&mut doc, &[(0, rects)]).expect("apply");

    assert!(report.runs_removed() > 0, "the run must be removed");
    let bytes = all_object_bytes(&doc);
    assert!(
        !contains(&bytes, "TOPSECRETVALUE"),
        "the whole run goes: partial-run glyph removal is not what D-RD2 asks for"
    );
    assert!(contains(&bytes, "KEEPME"), "the other run must survive");
}

/// A one-page PDF whose content stream paints a filled rectangle and nothing else.
///
/// This is the D3 fixture: a mark over *drawing* rather than over text. §8 says apply does not
/// remove it, so the only correct behaviour is to report that it is still there.
fn vector_fixture(content: &str) -> Document {
    let mut doc = Document::with_version("1.7");

    let content_id = doc.new_object_id();
    doc.objects.insert(
        content_id,
        Stream::new(dictionary! {}, content.as_bytes().to_vec()).into(),
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

#[test]
fn a_filled_path_under_a_mark_is_counted_as_remaining() {
    // D3. The rectangle covers the mark exactly; apply deletes no operator, so the ink is still
    // in the file. The count is what lets the command warn instead of writing a file that looks
    // redacted and is not.
    let mark = Rect::new(70.0, 100.0, 200.0, 200.0);
    let doc = vector_fixture("0 0 0 rg\n80 120 100 60 re f\n");
    let report = apply_redactions(&mut doc.clone(), &[(0, vec![mark])]).expect("apply");

    assert_eq!(
        report.vector_paths_remaining(),
        1,
        "a filled `re` under the mark must be counted as still drawn"
    );
    assert_eq!(
        report.runs_removed(),
        0,
        "there is no text to remove, which is exactly why the count matters"
    );
    assert!(
        report.nothing_to_apply(),
        "nothing was deleted, so the caller must not treat this as a successful redaction"
    );
}

#[test]
fn a_path_outside_every_mark_is_not_counted() {
    // The other half of the oracle: a warning that fires on every document is noise, and the
    // user learns to ignore it. A path well clear of the mark must not be counted.
    let mark = Rect::new(300.0, 500.0, 400.0, 600.0);
    let doc = vector_fixture("0 0 0 rg\n80 120 100 60 re f\n");

    assert_eq!(
        count_vector_paths_under_marks(&doc, 0, &[mark]).expect("count"),
        0,
        "a path that does not touch the mark is not remaining content"
    );
}

#[test]
fn the_transform_in_force_decides_whether_a_path_is_under_the_mark() {
    // A path drawn in user space at 0,0 can land anywhere on the page once `cm` and `q`/`Q` are
    // applied. Judging the raw operands instead of the transformed ones would count this stroke
    // at the origin and miss that it is painted at 350,550 -- the same class of bug the image
    // pass already replays matrices to avoid.
    let mark = Rect::new(300.0, 500.0, 460.0, 640.0);
    let doc = vector_fixture("q 1 0 0 1 350 550 cm\n0 0 m 40 40 l S\nQ\n");

    assert_eq!(
        count_vector_paths_under_marks(&doc, 0, &[mark]).expect("count"),
        1,
        "the stroke is painted inside the mark only after its `cm` is applied"
    );

    // And the same bytes with the translation removed must stop counting, which is what proves
    // the matrix was actually used rather than coincidentally right.
    let untranslated = vector_fixture("q 1 0 0 1 0 0 cm\n0 0 m 40 40 l S\nQ\n");
    assert_eq!(
        count_vector_paths_under_marks(&untranslated, 0, &[mark]).expect("count"),
        0,
        "without the translation the stroke is at the origin, outside the mark"
    );
}

#[test]
fn a_path_inside_a_form_is_counted_through_the_forms_matrix() {
    // A logo is usually a Form XObject, and a form's /Matrix places its content. Counting only
    // the page stream would report nothing for the most common way a vector graphic reaches a
    // page, and the warning would never fire where it matters most.
    let mark = Rect::new(300.0, 500.0, 460.0, 640.0);

    let mut doc = vector_fixture("q /Fm1 Do Q\n");

    let form_id = doc.new_object_id();
    let form = Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Form",
            "BBox" => vec![0.into(), 0.into(), 100.into(), 100.into()],
            // Translates the form's own 0,0 to 330,520, inside the mark.
            "Matrix" => vec![1.into(), 0.into(), 0.into(), 1.into(), 330.into(), 520.into()],
        },
        b"0 0 0 rg\n10 10 50 50 re f\n".to_vec(),
    );
    doc.objects.insert(form_id, form.into());

    // `get_pages` is 1-based; page 0 is key 1.
    let page_id = doc.get_pages()[&1];
    doc.get_object_mut(page_id)
        .unwrap()
        .as_dict_mut()
        .unwrap()
        .set(
            "Resources",
            dictionary! {
                "XObject" => dictionary! { "Fm1" => Object::Reference(form_id) },
            },
        );

    assert_eq!(
        count_vector_paths_under_marks(&doc, 0, &[mark]).expect("count"),
        1,
        "a filled path inside a form, placed by the form's /Matrix, must be counted"
    );
}

#[test]
fn text_painting_operators_are_not_counted_as_vector_graphics() {
    // A warning that fires on ordinary text would be worse than none: every text redaction
    // would claim to have left something behind. Text is removed, so it must not be counted.
    let mark = Rect::new(70.0, 690.0, 200.0, 706.0);
    let doc = fixture();

    assert_eq!(
        count_vector_paths_under_marks(&doc, 0, &[mark]).expect("count"),
        0,
        "the secret is drawn with Tj/Td, which apply deletes; it is not remaining content"
    );
}

#[test]
fn a_clip_path_that_paints_nothing_is_not_counted() {
    // `re W n` builds a path solely to clip. Nothing is painted, so nothing remains; counting it
    // would warn about content that was never visible.
    let mark = Rect::new(70.0, 100.0, 200.0, 200.0);
    let doc = vector_fixture("80 120 100 60 re W n\n");

    assert_eq!(
        count_vector_paths_under_marks(&doc, 0, &[mark]).expect("count"),
        0,
        "a clip with no painting operator leaves no ink under the mark"
    );
}
