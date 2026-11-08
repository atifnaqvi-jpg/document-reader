//! SCOPE-C's substrate proof: `lopdf::IncrementalDocument` can append a revision.
//!
//! The design calls this "the likeliest failure" (§11) and step 1 of §10 requires it before any
//! SCOPE-C code. It is cheap and it invalidates D-SG2 wholesale if it fails, so it is proven
//! here rather than assumed.
//!
//! Why this is not PDFium-gated: §7 requires every Windows assertion to have a Linux-side
//! equivalent, and this claim is about bytes and xref structure, not rendering. Reopening with
//! `lopdf` is the weaker half of the proof; `incremental_pdfium.rs` carries the viewer half.
//!
//! The property that matters for signing is **byte-identical prefix**. A `/ByteRange` covers an
//! offset range of the file as written, so if appending displaced or rewrote any earlier byte,
//! the range would no longer describe what the signer hashed. `prefix_is_byte_identical` is the
//! assertion the whole of D-SG2 rests on.

use lopdf::{dictionary, Document, Object, Stream};

/// A one-page source document, saved to disk, plus its bytes.
fn source_on_disk(name: &str) -> (std::path::PathBuf, Vec<u8>) {
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

    let dir = std::env::temp_dir().join(format!(
        "docreader-incremental-{name}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("source.pdf");
    doc.save(&path).expect("save source");
    let bytes = std::fs::read(&path).expect("read source");
    (path, bytes)
}

#[test]
fn the_appended_output_starts_with_the_source_bytes_verbatim() {
    // D-SG2's premise. If this fails, a signature cannot cover the user's original bytes and
    // the whole shape of SCOPE-C is wrong.
    use lopdf::IncrementalDocument;

    let (_path, source_bytes) = source_on_disk("prefix");
    let prev = Document::load_mem(&source_bytes).expect("reload the source");
    let mut incremental = IncrementalDocument::create_from(source_bytes.clone(), prev);

    // Append one object and one catalog key: the minimum a signature revision does.
    let new_id = incremental
        .new_document
        .add_object(Object::string_literal("appended"));
    let catalog = incremental
        .new_document
        .trailer
        .get(b"Root")
        .and_then(Object::as_reference)
        .expect("catalog");
    incremental
        .opt_clone_object_to_new_document(catalog)
        .expect("clone catalog");
    let catalog_len = incremental
        .new_document
        .get_object(catalog)
        .and_then(Object::as_dict)
        .map(|dict| dict.len())
        .expect("catalog dict");
    let _ = catalog_len;
    let _ = new_id;

    let mut out = Vec::new();
    incremental.save_to(&mut out).expect("save incrementally");

    assert!(
        out.len() > source_bytes.len(),
        "the append must make the file longer: source {} vs output {}",
        source_bytes.len(),
        out.len()
    );
    assert!(
        out.starts_with(&source_bytes),
        "D-SG2: the output must begin with the source bytes byte-for-byte; \
         a /ByteRange over those offsets would otherwise be meaningless"
    );
}

#[test]
fn the_appended_object_is_visible_after_reopening() {
    use lopdf::IncrementalDocument;

    let (_path, source_bytes) = source_on_disk("visible");
    let prev = Document::load_mem(&source_bytes).expect("reload");
    let root = prev
        .trailer
        .get(b"Root")
        .and_then(Object::as_reference)
        .unwrap();
    let mut incremental = IncrementalDocument::create_from(source_bytes, prev);

    let new_id = incremental
        .new_document
        .add_object(Object::string_literal("SIGNATURE_HERE"));
    incremental
        .new_document
        .objects
        .insert(new_id, Object::string_literal("SIGNATURE_HERE"));

    let mut out = Vec::new();
    incremental.save_to(&mut out).expect("save");

    let reopened = Document::load_mem(&out).expect("reopen the appended file");
    let found = reopened
        .get_object(new_id)
        .expect("the appended object must resolve");
    assert_eq!(
        found.as_str().ok(),
        Some(b"SIGNATURE_HERE".as_slice()),
        "the appended object must read back"
    );
    // The original structure must still be there: an append that broke the catalog would make
    // the file unusable even though the new object resolves.
    assert!(
        reopened.get_object(root).is_ok(),
        "the original catalog must still resolve"
    );
}

#[test]
fn the_new_revision_chains_to_the_previous_one_with_prev() {
    // ISO 32000-1 §7.5.6: an incremental trailer carries /Prev pointing at the previous
    // cross-reference section. Without it a reader sees a one-revision file that is missing
    // every object the new revision did not rewrite.
    //
    // Asserted on the **bytes**, not on `Document::load_mem(...).trailer`. lopdf's reader
    // merges the xref chain and drops `/Prev` from the reassembled trailer, so reading it back
    // through lopdf reports absence even when the file is correct. The bytes are what a viewer
    // (and PDFium) actually parses, so they are what the invariant is about.
    use lopdf::IncrementalDocument;

    let (_path, source_bytes) = source_on_disk("prev");
    let prev = Document::load_mem(&source_bytes).expect("reload");
    let prev_xref_start = prev.xref_start;
    assert!(
        prev_xref_start > 0,
        "the source must have a real xref offset, or /Prev would be meaningless"
    );

    let mut incremental = IncrementalDocument::create_from(source_bytes.clone(), prev);
    incremental
        .new_document
        .add_object(Object::string_literal("x"));

    let mut out = Vec::new();
    incremental.save_to(&mut out).expect("save");

    // Only the appended region may carry the new /Prev; searching the whole file would also
    // match the original trailer's own keys.
    let appended = &out[source_bytes.len()..];
    let needle = format!("/Prev {prev_xref_start}");
    assert!(
        appended
            .windows(needle.len())
            .any(|window| window == needle.as_bytes()),
        "the appended revision must declare /Prev {prev_xref_start}; appended region was: {}",
        String::from_utf8_lossy(appended)
    );
}

#[test]
fn appending_a_second_revision_on_top_of_the_first_works() {
    // "Second revision" in full: revision 1 is the source, revision 2 is the first append, and
    // this adds revision 3. Signing then annotating-with-a-signature is exactly this shape, and
    // a chain that only works once would be found much later.
    use lopdf::IncrementalDocument;

    let (_path, source_bytes) = source_on_disk("chain");

    // Revision 2.
    let prev = Document::load_mem(&source_bytes).expect("reload");
    let mut first = IncrementalDocument::create_from(source_bytes.clone(), prev);
    let first_id = first
        .new_document
        .add_object(Object::string_literal("REV2"));
    let mut after_first = Vec::new();
    first.save_to(&mut after_first).expect("save revision 2");

    assert!(
        after_first.starts_with(&source_bytes),
        "revision 2 keeps the prefix"
    );

    // Revision 3.
    let prev = Document::load_mem(&after_first).expect("reload revision 2");
    let mut second = IncrementalDocument::create_from(after_first.clone(), prev);
    let second_id = second
        .new_document
        .add_object(Object::string_literal("REV3"));
    let mut after_second = Vec::new();
    second.save_to(&mut after_second).expect("save revision 3");

    assert!(
        after_second.starts_with(&after_first),
        "revision 3 must preserve revision 2's bytes, which already contain revision 1's"
    );

    let reopened = Document::load_mem(&after_second).expect("reopen the 3-revision file");
    assert_eq!(
        reopened.get_object(first_id).and_then(Object::as_str).ok(),
        Some(b"REV2".as_slice()),
        "the first appended object must survive two more revisions"
    );
    assert_eq!(
        reopened.get_object(second_id).and_then(Object::as_str).ok(),
        Some(b"REV3".as_slice()),
        "the second appended object must resolve"
    );
}

#[test]
fn the_source_file_on_disk_is_never_modified() {
    // D-SG2a: the source is never modified; the signed output is a new path. This asserts the
    // filesystem, not the API — an incremental save that wrote back over its own input would
    // satisfy every other test in this file.
    use lopdf::IncrementalDocument;

    let (path, source_bytes) = source_on_disk("untouched");
    let prev = Document::load_mem(&source_bytes).expect("reload");
    let mut incremental = IncrementalDocument::create_from(source_bytes.clone(), prev);
    incremental
        .new_document
        .add_object(Object::string_literal("append"));

    let out_path = path.with_file_name("signed.pdf");
    incremental.save(&out_path).expect("save to a new path");

    let after = std::fs::read(&path).expect("re-read the source");
    assert_eq!(
        after, source_bytes,
        "D-SG2a: the source file must be byte-identical after producing the signed path"
    );
    assert_ne!(
        std::fs::read(&out_path).expect("read output"),
        source_bytes,
        "and the output must be a different file"
    );
}

#[test]
fn an_appended_object_can_overwrite_an_existing_one_without_touching_revision_one() {
    // A signature revision rewrites the catalog (to add /AcroForm or /Perms). That is an
    // object id the previous revision already defines, so the append must shadow it. The old
    // bytes stay in the file — which is correct and is why I-RD3-style scans over a *signed*
    // file must expect revision 1 to still contain whatever revision 1 contained.
    use lopdf::IncrementalDocument;

    let (_path, source_bytes) = source_on_disk("shadow");
    let prev = Document::load_mem(&source_bytes).expect("reload");
    let catalog = prev
        .trailer
        .get(b"Root")
        .and_then(Object::as_reference)
        .unwrap();
    let mut incremental = IncrementalDocument::create_from(source_bytes.clone(), prev);

    incremental
        .opt_clone_object_to_new_document(catalog)
        .expect("clone the catalog into the new revision");
    incremental
        .new_document
        .get_object_mut(catalog)
        .and_then(Object::as_dict_mut)
        .expect("catalog dict")
        .set("Signed", Object::string_literal("yes"));

    let mut out = Vec::new();
    incremental.save_to(&mut out).expect("save");

    let reopened = Document::load_mem(&out).expect("reopen");
    let dict = reopened
        .get_object(catalog)
        .and_then(Object::as_dict)
        .expect("catalog");
    assert_eq!(
        dict.get(b"Signed").ok(),
        Some(&Object::string_literal("yes")),
        "the shadowing revision must win when the file is read"
    );
    assert!(
        dict.get(b"Pages").is_ok(),
        "and the rewritten catalog must still carry what it had"
    );
}
