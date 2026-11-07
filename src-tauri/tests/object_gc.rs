//! Reachability GC and xref integrity (P3.2 D-RD4, I-RD4).
//!
//! These build documents in memory rather than loading fixtures: the defect being guarded
//! against is *graph shape*, and a fixture would hide which shape it had. Each test names
//! the mutation that must make it fail.

use lopdf::{dictionary, Document, Object, ObjectId};

use document_reader_lib::editor::object_gc::{
    dangling_references, reachable_ids, sweep_unreachable,
};

/// Catalog → Pages → one page, plus `extras` objects that the caller decides how to wire.
fn document_with(objects: Vec<(u32, Object)>) -> Document {
    let mut doc = Document::with_version("1.7");

    let pages_id = doc.new_object_id();
    let page_id = doc.new_object_id();
    let catalog_id = doc.new_object_id();

    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => vec![Object::Reference(page_id)],
            "Count" => 1,
        }),
    );
    doc.objects.insert(
        page_id,
        Object::Dictionary(dictionary! {
            "Type" => "Page",
            "Parent" => Object::Reference(pages_id),
            "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        }),
    );
    doc.objects.insert(
        catalog_id,
        Object::Dictionary(dictionary! {
            "Type" => "Catalog",
            "Pages" => Object::Reference(pages_id),
        }),
    );

    doc.trailer.set("Root", Object::Reference(catalog_id));
    for (id, object) in objects {
        doc.objects.insert((id, 0), object);
    }

    doc
}

#[test]
fn an_unreferenced_object_is_dropped_and_reported() {
    let mut doc = document_with(vec![
        (900, Object::string_literal("orphaned secret")),
        (901, Object::string_literal("also orphaned")),
    ]);

    let removed = sweep_unreachable(&mut doc).expect("sweep");

    assert_eq!(
        removed,
        vec![(900, 0), (901, 0)],
        "both orphans must be dropped and named; a sweep that removes nothing \
         (mutation: return early before the removal loop) must fail here"
    );
    assert!(!doc.objects.contains_key(&(900, 0)));
    assert!(
        doc.objects.contains_key(&(1, 0)),
        "the catalog must survive"
    );
}

#[test]
fn an_object_reachable_only_through_a_live_object_survives() {
    let mut doc = document_with(vec![]);
    let page_id = doc.get_pages().values().copied().next().expect("a page");

    // Reachable: page -> Contents -> 910.
    let content_id: ObjectId = (910, 0);
    doc.objects.insert(
        content_id,
        lopdf::Stream::new(dictionary! {}, b"BT ET".to_vec()).into(),
    );
    doc.objects
        .get_mut(&page_id)
        .and_then(|object| object.as_dict_mut().ok())
        .expect("page dict")
        .set("Contents", Object::Reference(content_id));

    let removed = sweep_unreachable(&mut doc).expect("sweep");

    assert!(
        removed.is_empty(),
        "a reachable content stream must not be dropped (mutation: root the sweep at the \
         catalog only, forgetting the trailer's other roots, is not enough to fail this; \
         mutation: seed the worklist with nothing must fail). Removed: {removed:?}"
    );
}

#[test]
fn an_object_reachable_only_through_a_dropped_object_is_dropped_too() {
    // The two-step leak: 920 is referenced, but only by 921, which is itself an orphan.
    // A one-level sweep that deletes the named object and stops leaves 920 on disk.
    let mut doc = document_with(vec![
        (920, Object::string_literal("leaked through an orphan")),
        (
            921,
            Object::Dictionary(dictionary! {
                "Ref" => Object::Reference((920, 0)),
            }),
        ),
    ]);

    let removed = sweep_unreachable(&mut doc).expect("sweep");

    assert_eq!(
        removed,
        vec![(920, 0), (921, 0)],
        "transitive orphans must both go; a single-level sweep (mutation: remove only \
         objects whose direct referrer set is empty) leaves 920 behind"
    );
}

#[test]
fn a_shared_object_referenced_by_two_pages_survives_until_both_let_go() {
    let mut doc = Document::with_version("1.7");
    let pages_id = doc.new_object_id();
    let first = doc.new_object_id();
    let second = doc.new_object_id();
    let shared: ObjectId = (930, 0);

    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => vec![Object::Reference(first), Object::Reference(second)],
            "Count" => 2,
        }),
    );
    for page in [first, second] {
        doc.objects.insert(
            page,
            Object::Dictionary(dictionary! {
                "Type" => "Page",
                "Parent" => Object::Reference(pages_id),
                "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
                "Contents" => Object::Reference(shared),
            }),
        );
    }
    let catalog = doc.new_object_id();
    doc.objects.insert(
        catalog,
        Object::Dictionary(
            dictionary! { "Type" => "Catalog", "Pages" => Object::Reference(pages_id) },
        ),
    );
    doc.trailer.set("Root", Object::Reference(catalog));

    doc.objects.insert(
        shared,
        lopdf::Stream::new(dictionary! {}, b"shared".to_vec()).into(),
    );

    assert!(reachable_ids(&doc).contains(&shared));
    assert!(
        sweep_unreachable(&mut doc).expect("sweep").is_empty(),
        "an object shared by two pages is still referenced and must survive \n         (removing it is the D-RD3 over-removal direction)"
    );

    // Now let both pages go: only then is it garbage.
    for page in [first, second] {
        doc.objects
            .get_mut(&page)
            .and_then(|object| object.as_dict_mut().ok())
            .expect("page dict")
            .remove(b"Contents");
    }
    assert!(
        sweep_unreachable(&mut doc)
            .expect("sweep")
            .contains(&shared),
        "once no page references it, the shared object must be collected"
    );
}

#[test]
fn the_sweep_reports_no_dangling_references_afterwards() {
    let mut doc = document_with(vec![
        (940, Object::string_literal("orphan pointing at nothing")),
        (
            941,
            Object::Dictionary(dictionary! {
                "Missing" => Object::Reference((999, 0)),
            }),
        ),
    ]);

    // Before the sweep the orphan's dangling reference is invisible: 941 is unreachable,
    // and only reachable objects are checked.
    assert!(
        dangling_references(&doc).is_empty(),
        "an unreachable object's broken reference is not a defect"
    );

    sweep_unreachable(&mut doc).expect("sweep");

    assert_eq!(
        dangling_references(&doc),
        vec![],
        "after the sweep every reachable reference must resolve (I-RD4)"
    );
}

#[test]
fn a_dangling_reference_in_a_live_object_is_reported_with_both_ends() {
    let mut doc = document_with(vec![]);
    let page_id = doc.get_pages().values().copied().next().expect("a page");
    doc.objects
        .get_mut(&page_id)
        .and_then(|object| object.as_dict_mut().ok())
        .expect("page dict")
        .set("Annots", Object::Reference((999, 0)));

    assert_eq!(
        dangling_references(&doc),
        vec![(page_id, (999, 0))],
        "the detector must name the referrer as well as the missing target; \
         mutation: return an empty vec unconditionally must fail this"
    );
}

#[test]
fn max_id_and_size_follow_the_sweep() {
    let mut doc = document_with(vec![(5000, Object::string_literal("high orphan"))]);
    // Inserting into `objects` directly does not advance `max_id`; a parsed document has it
    // set from the file's /Size, so the fixture has to do the same.
    doc.max_id = 5000;
    assert_eq!(doc.max_id, 5000, "fixture sanity: highest id is the orphan");

    sweep_unreachable(&mut doc).expect("sweep");

    assert!(
        doc.max_id < 5000,
        "max_id must shrink to the real maximum (was 5000, now {}); leaving it high makes \
         /Size disagree with the object table",
        doc.max_id
    );
    assert_eq!(
        doc.trailer.get(b"Size").and_then(Object::as_i64).ok(),
        Some(i64::from(doc.max_id + 1)),
        "the trailer's /Size must equal max_id + 1 after the sweep"
    );
}
