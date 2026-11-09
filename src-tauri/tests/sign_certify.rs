//! SCOPE-C unit 1: certification (DocMDP) and the second-revision obligation.
//!
//! Two questions, and they are different questions:
//!
//! 1. **Does the second revision leave the first signature intact?** A second signature is an
//!    appended revision. The first signature's `/ByteRange` covers the bytes that existed when it
//!    was written, so after an append that range no longer reaches the end of the file. That is
//!    *coverage* changing, which a reader must report. It is not the blob breaking: the first
//!    signature must still verify cryptographically over the range it declares. Conflating the two
//!    is how a product either cries "tampered" on an honest amendment or hides a real one.
//!    It also must still be *reachable*: a signature whose widget has been orphaned from the page
//!    verifies in a file and shows up in no reader's signature panel.
//!
//! 2. **Is the certification level the one that was asked for?** D-SG6 makes `P=1`/`P=2`/`P=3` an
//!    explicit choice. An accidental `P=1` turns the user's own later annotation into "this
//!    document was modified", so the level has to be read back out of the file rather than trusted
//!    from the argument that wrote it.

#[path = "support/cms_verifier.rs"]
mod cms_verifier;

use cms_verifier::{extract_signature, public_key_from_certificate, verify_independently};

use document_reader_lib::editor::signature::pfx::{import_bytes, ImportedIdentity};
use document_reader_lib::editor::signature::sign::{
    sign_save, sign_save_with, Certification, SignOptions,
};
use lopdf::{dictionary, Document, Object};

const PFX_PASSWORD: &str = "correct-horse";

fn fixture_identity() -> ImportedIdentity {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/signing/signer.pfx");
    let bytes =
        std::fs::read(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    import_bytes(&bytes, PFX_PASSWORD)
        .expect("the fixture PFX opens")
        .identities
        .into_iter()
        .next()
        .expect("the fixture PFX carries one identity")
}

fn fixture_public_key() -> rsa::RsaPublicKey {
    public_key_from_certificate(&fixture_identity().chain[0])
}

fn source_on_disk(name: &str, text: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join(name);

    let mut document = Document::with_version("1.7");
    let pages_id = document.new_object_id();
    let content_id = document.add_object(lopdf::Stream::new(
        dictionary! {},
        format!("BT /F1 24 Tf 72 720 Td ({text}) Tj ET").into_bytes(),
    ));
    let page_id = document.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        "Contents" => content_id,
    });
    document.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => vec![Object::Reference(page_id)],
            "Count" => 1,
        }),
    );
    let catalog_id = document.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    document.trailer.set("Root", catalog_id);
    document.save(&path).expect("save the source");

    (dir, path)
}

/// A source whose page carries an *indirect* `/Annots` array plus one pre-existing text
/// annotation, which is what another tool's output looks like.
///
/// Written by hand because the fixture helper above always emits a direct array, and the branch
/// under test only runs for the indirect spelling. If the writer takes the `_ => Vec::new()` arm
/// instead of resolving the reference, the user's own annotation disappears from the signed file.
fn source_with_indirect_annots(name: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join(name);

    let mut document = Document::with_version("1.7");
    let pages_id = document.new_object_id();
    let content_id = document.add_object(lopdf::Stream::new(
        dictionary! {},
        b"BT /F1 24 Tf 72 720 Td (indirect annots) Tj ET".to_vec(),
    ));
    // The reader's own annotation, which must survive the signing revision.
    let note_id = document.add_object(dictionary! {
        "Type" => "Annot",
        "Subtype" => "Text",
        "Rect" => vec![10.into(), 10.into(), 30.into(), 30.into()],
        "Contents" => Object::string_literal(b"the reader's own note".to_vec()),
    });
    // The array itself is a separate object, and the page points at it.
    let annots_id = document.add_object(Object::Array(vec![Object::Reference(note_id)]));
    let page_id = document.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        "Contents" => content_id,
        "Annots" => annots_id,
    });
    document.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => vec![Object::Reference(page_id)],
            "Count" => 1,
        }),
    );
    let catalog_id = document.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    document.trailer.set("Root", catalog_id);
    document.save(&path).expect("save the source");

    (dir, path)
}

/// A source whose catalog points at an *indirect* `/AcroForm` holding one filled-in text field.
fn source_with_indirect_acroform(name: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join(name);

    let mut document = Document::with_version("1.7");
    let pages_id = document.new_object_id();
    let content_id = document.add_object(lopdf::Stream::new(
        dictionary! {},
        b"BT /F1 24 Tf 72 720 Td (acroform) Tj ET".to_vec(),
    ));
    let page_id = document.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        "Contents" => content_id,
    });
    document.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => vec![Object::Reference(page_id)],
            "Count" => 1,
        }),
    );
    // The user's already-filled field, which a second signature must not discard.
    let field_id = document.add_object(dictionary! {
        "Type" => "Annot",
        "Subtype" => "Widget",
        "FT" => "Tx",
        "T" => Object::string_literal(b"customer_name".to_vec()),
        "V" => Object::string_literal(b"Ada Lovelace".to_vec()),
    });
    let form_id = document.add_object(dictionary! {
        "Fields" => vec![Object::Reference(field_id)],
        "SigFlags" => 1,
    });
    let catalog_id = document.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
        "AcroForm" => form_id,
    });
    document.trailer.set("Root", catalog_id);
    document.save(&path).expect("save the source");

    (dir, path)
}

// ---------------------------------------------------------------------------------------------
// Reading a file's signatures back, independently of the code that wrote them.
// ---------------------------------------------------------------------------------------------

/// One signature dictionary as it appears in a file.
struct ReadSignature {
    object_number: u32,
    /// The four `/ByteRange` integers, in order.
    byte_range: [usize; 4],
    /// True when the declared range ends at the last byte of the file.
    reaches_end: bool,
    /// The `/P` of its `/Reference` → `/TransformParams`, when it is a certification signature.
    doc_mdp_p: Option<i64>,
    /// The `/V` of those transform parameters.
    doc_mdp_v: Option<String>,
    /// The `/Contents` blob with its hex padding removed.
    der: Vec<u8>,
    reason: Option<String>,
    location: Option<String>,
}

/// Every signature dictionary in `bytes`, found by walking the document rather than by searching
/// for `/ByteRange`: a search finds only the first, and the whole question here is about the
/// second.
fn read_signatures(bytes: &[u8]) -> Vec<ReadSignature> {
    let document = Document::load_mem(bytes).expect("the written file parses");
    let mut found = Vec::new();

    for (id, object) in document.objects.iter() {
        let Some(dict) = object.as_dict().ok() else {
            continue;
        };
        if !dict.has_type(b"Sig") {
            continue;
        }

        let numbers: Vec<usize> = dict
            .get(b"ByteRange")
            .and_then(Object::as_array)
            .expect("/ByteRange is an array")
            .iter()
            .map(|value| value.as_i64().expect("integer").max(0) as usize)
            .collect();
        assert_eq!(numbers.len(), 4, "a /ByteRange has four integers");

        let der = dict
            .get(b"Contents")
            .and_then(Object::as_str)
            .expect("/Contents is a string")
            .to_vec();
        let der_len = der.len() - der.iter().rev().take_while(|byte| **byte == 0).count();

        let (doc_mdp_p, doc_mdp_v) = match dict.get(b"Reference").and_then(Object::as_array) {
            Ok(references) => {
                let mut level = None;
                let mut version = None;
                for reference in references {
                    let Ok(reference) = reference.as_dict() else {
                        continue;
                    };
                    let is_doc_mdp = reference
                        .get(b"TransformMethod")
                        .and_then(Object::as_name)
                        .is_ok_and(|name| name == b"DocMDP");
                    if !is_doc_mdp {
                        continue;
                    }
                    if let Ok(params) = reference.get(b"TransformParams").and_then(Object::as_dict)
                    {
                        level = params.get(b"P").and_then(Object::as_i64).ok();
                        version = params
                            .get(b"V")
                            .and_then(Object::as_name)
                            .ok()
                            .map(|name| String::from_utf8_lossy(name).to_string());
                    }
                }
                (level, version)
            }
            Err(_) => (None, None),
        };

        let text = |key: &[u8]| {
            dict.get(key)
                .and_then(Object::as_str)
                .ok()
                .map(|value| String::from_utf8_lossy(value).to_string())
        };

        found.push(ReadSignature {
            object_number: id.0,
            reaches_end: numbers[2] + numbers[3] == bytes.len(),
            byte_range: [numbers[0], numbers[1], numbers[2], numbers[3]],
            doc_mdp_p,
            doc_mdp_v,
            der: der[..der_len].to_vec(),
            reason: text(b"Reason"),
            location: text(b"Location"),
        });
    }

    found.sort_by_key(|signature| signature.object_number);
    found
}

/// The content a signature covers, rebuilt from the file's own declared range.
fn covered_content(bytes: &[u8], range: [usize; 4]) -> Vec<u8> {
    let [start, first, second_start, second_len] = range;
    let mut covered = Vec::with_capacity(first + second_len);
    covered.extend_from_slice(&bytes[start..start + first]);
    covered.extend_from_slice(&bytes[second_start..second_start + second_len]);
    covered
}

/// Every widget reachable from a page's `/Annots`, as `(page subtype, /V)`.
fn widgets_reachable_from_pages(bytes: &[u8]) -> Vec<(String, u32)> {
    let document = Document::load_mem(bytes).expect("parse");
    let mut reachable = Vec::new();
    for (_, object) in document.objects.iter() {
        let Ok(page) = object.as_dict() else { continue };
        if !page.has_type(b"Page") {
            continue;
        }
        let Ok(annots) = page.get(b"Annots").and_then(Object::as_array) else {
            continue;
        };
        for annot in annots {
            let Ok(id) = annot.as_reference() else {
                continue;
            };
            let Ok(dict) = document.get_dictionary(id) else {
                continue;
            };
            let subtype = dict
                .get(b"Subtype")
                .and_then(Object::as_name)
                .map(|name| String::from_utf8_lossy(name).to_string())
                .unwrap_or_default();
            let value = dict
                .get(b"V")
                .and_then(Object::as_reference)
                .map(|reference| reference.0)
                .unwrap_or(0);
            reachable.push((subtype, value));
        }
    }
    reachable
}

// ---------------------------------------------------------------------------------------------
// The second-revision obligation.
// ---------------------------------------------------------------------------------------------

/// Signing a document that already carries a signature must not orphan that signature.
///
/// Measured defect: the first version of `build_signing_revision` *set* `Annots` and `AcroForm` to
/// the new widget, which is correct for the first signature and destroys the first one for the
/// second. The file still verifies — the blob is untouched — and the first signature appears in no
/// reader's panel, because nothing points at its widget any more.
#[test]
fn a_second_signature_leaves_the_first_signature_reachable() {
    let (_dir, source) = source_on_disk("report.pdf", "P3.2 two signatures");
    let identity = fixture_identity();

    let first = sign_save(&source, &identity).expect("first signature");
    let first_path = std::path::PathBuf::from(&first.path);

    let second = sign_save(&first_path, &identity).expect("second signature");
    let written = std::fs::read(&second.path).expect("read the twice-signed file");

    let signatures = read_signatures(&written);
    assert_eq!(
        signatures.len(),
        2,
        "two signing revisions must leave two signature dictionaries"
    );

    let widgets = widgets_reachable_from_pages(&written);
    for signature in &signatures {
        assert!(
            widgets
                .iter()
                .any(|(subtype, value)| subtype == "Widget" && *value == signature.object_number),
            "signature object {} has no widget reachable from any page; a reader cannot show it. \
             widgets found: {widgets:?}",
            signature.object_number
        );
    }
}

/// A page whose `/Annots` is an *indirect* array keeps that array's contents.
///
/// Another tool's output commonly spells `/Annots` as a reference to a separate array object. The
/// writer has to resolve it before appending, or the reader's own annotations vanish from a file
/// they just signed. The failure is silent: the signature is valid, the page just lost its notes.
#[test]
fn an_indirect_annots_array_is_resolved_and_its_annotations_survive() {
    let (_dir, source) = source_with_indirect_annots("annotated.pdf");
    let outcome = sign_save(&source, &fixture_identity()).expect("sign");
    let written = std::fs::read(&outcome.path).expect("read the signed file");

    let document = Document::load_mem(&written).expect("the written file parses");
    let page_id = *document.get_pages().values().next().expect("a page");
    let page = document
        .get_object(page_id)
        .and_then(Object::as_dict)
        .expect("the page survives the revision")
        .clone();

    // `/Annots` may itself still be indirect after the merge, so resolve both spellings.
    let annots = match page.get(b"Annots").expect("the page still has /Annots") {
        Object::Reference(id) => document
            .get_object(*id)
            .and_then(Object::as_array)
            .expect("the array resolves")
            .clone(),
        Object::Array(items) => items.clone(),
        other => panic!("/Annots is neither an array nor a reference: {other:?}"),
    };

    let subtypes: Vec<String> = annots
        .iter()
        .filter_map(|item| match item {
            Object::Reference(id) => document.get_object(*id).ok(),
            direct => Some(direct),
        })
        .filter_map(|object| object.as_dict().ok())
        .filter_map(|dict| dict.get(b"Subtype").and_then(Object::as_name).ok())
        .map(|name| String::from_utf8_lossy(name).to_string())
        .collect();

    assert!(
        subtypes.iter().any(|subtype| subtype == "Text"),
        "the reader's own text annotation was dropped by the signing revision; kept: {subtypes:?}"
    );
    assert!(
        subtypes.iter().any(|subtype| subtype == "Widget"),
        "the new signature widget was not added to the existing array; kept: {subtypes:?}"
    );
}

/// An indirect `/AcroForm` and the fields inside it survive a signing revision.
///
/// The same trap as `/Annots`, in a second place: the form object is in the previous revision, so a
/// lookup in the document being built finds nothing and the `unwrap_or_default()` turns that into an
/// empty field list. The signature is valid and the user's filled-in form is gone.
#[test]
fn an_indirect_acroform_keeps_the_fields_that_were_already_filled_in() {
    let (_dir, source) = source_with_indirect_acroform("form.pdf");
    let outcome = sign_save(&source, &fixture_identity()).expect("sign");
    let written = std::fs::read(&outcome.path).expect("read");

    let document = Document::load_mem(&written).expect("parse");
    let catalog_id = document
        .trailer
        .get(b"Root")
        .and_then(Object::as_reference)
        .expect("a catalog");
    let catalog = document
        .get_object(catalog_id)
        .and_then(Object::as_dict)
        .expect("the catalog is a dictionary");

    let form = match catalog.get(b"AcroForm").expect("the form survives") {
        Object::Reference(id) => document
            .get_object(*id)
            .and_then(Object::as_dict)
            .expect("the form resolves"),
        Object::Dictionary(dict) => dict,
        other => panic!("/AcroForm is neither a dictionary nor a reference: {other:?}"),
    };

    let field_objects = form
        .get(b"Fields")
        .and_then(Object::as_array)
        .expect("the field list survives");

    // The signature widget has no `/T` of its own, so identify the entries by what they are rather
    // than by name: the user's field is the one carrying a `/T`.
    let named: Vec<String> = field_objects
        .iter()
        .filter_map(|item| match item {
            Object::Reference(id) => document.get_object(*id).ok(),
            direct => Some(direct),
        })
        .filter_map(|object| object.as_dict().ok())
        .filter_map(|dict| dict.get(b"T").and_then(Object::as_str).ok())
        .map(|name| String::from_utf8_lossy(name).to_string())
        .collect();
    let signature_fields = field_objects
        .iter()
        .filter_map(|item| match item {
            Object::Reference(id) => document.get_object(*id).ok(),
            direct => Some(direct),
        })
        .filter_map(|object| object.as_dict().ok())
        .filter(|dict| {
            dict.get(b"FT")
                .and_then(Object::as_name)
                .is_ok_and(|ft| ft == b"Sig")
        })
        .count();

    assert!(
        named.iter().any(|name| name == "customer_name"),
        "the user's filled-in field was dropped by the signing revision; kept: {named:?}"
    );
    assert_eq!(
        signature_fields, 1,
        "the new signature field must join the existing one, not replace it; kept: {named:?}"
    );
}

/// The `/AcroForm` field list must accumulate too, for the same reason.
#[test]
fn a_second_signature_keeps_both_signatures_in_the_acroform_field_list() {
    let (_dir, source) = source_on_disk("report.pdf", "P3.2 form fields");
    let identity = fixture_identity();

    let first = sign_save(&source, &identity).expect("first signature");
    let second = sign_save(std::path::Path::new(&first.path), &identity).expect("second signature");
    let written = std::fs::read(&second.path).expect("read");

    let document = Document::load_mem(&written).expect("parse");
    let catalog = document
        .trailer
        .get(b"Root")
        .and_then(Object::as_reference)
        .expect("catalog");
    let form = document
        .get_dictionary(catalog)
        .expect("catalog dictionary")
        .get(b"AcroForm")
        .and_then(Object::as_dict)
        .expect("/AcroForm is a dictionary");

    let fields = form
        .get(b"Fields")
        .and_then(Object::as_array)
        .expect("/Fields is an array");
    assert_eq!(
        fields.len(),
        2,
        "both signature fields must remain in /AcroForm/Fields, not just the newest"
    );
    assert_eq!(
        form.get(b"SigFlags").and_then(Object::as_i64).unwrap_or(0) & 3,
        3,
        "SigFlags must keep both SignaturesExist and AppendOnly set"
    );
}

/// The first signature still verifies over the range it declares, even though that range no longer
/// reaches the end of the file.
///
/// This is the distinction the badge work depends on: coverage changed, the blob did not.
#[test]
fn the_first_signature_still_verifies_after_the_second_is_appended() {
    let (_dir, source) = source_on_disk("report.pdf", "P3.2 first survives");
    let identity = fixture_identity();

    let first = sign_save(&source, &identity).expect("first signature");
    let second = sign_save(std::path::Path::new(&first.path), &identity).expect("second signature");
    let written = std::fs::read(&second.path).expect("read");

    let signatures = read_signatures(&written);
    let first_signature = signatures
        .first()
        .expect("the first signature is still present");

    verify_independently(
        &first_signature.der,
        &covered_content(&written, first_signature.byte_range),
        &fixture_public_key(),
    )
    .expect("the first signature must still verify over the range it declares");

    assert!(
        !first_signature.reaches_end,
        "the first signature should no longer reach the end of the file; if it does, no revision \
         was appended and this test proves nothing"
    );

    let second_signature = signatures.last().expect("the second signature");
    verify_independently(
        &second_signature.der,
        &covered_content(&written, second_signature.byte_range),
        &fixture_public_key(),
    )
    .expect("the second signature must verify over its own range");
    assert!(
        second_signature.reaches_end,
        "the newest signature must cover through the end of the file"
    );
}

// ---------------------------------------------------------------------------------------------
// Certification (D-SG6).
// ---------------------------------------------------------------------------------------------

/// `P=1`, `P=2` and `P=3` each read back as the level that was written (I-SG5).
#[test]
fn each_certification_level_reads_back_as_the_level_that_was_written() {
    for level in [1_i64, 2, 3] {
        let (_dir, source) = source_on_disk("contract.pdf", "P3.2 certify");
        let certification = Certification::doc_mdp(level).expect("1, 2 and 3 are the legal levels");

        let outcome = sign_save_with(
            &source,
            &fixture_identity(),
            SignOptions {
                certification: Some(certification),
                ..SignOptions::default()
            },
        )
        .expect("certify the document");

        let written = std::fs::read(&outcome.path).expect("read the certified file");
        let signatures = read_signatures(&written);
        assert_eq!(signatures.len(), 1, "one certification signature");
        let signature = &signatures[0];

        assert_eq!(
            signature.doc_mdp_p,
            Some(level),
            "a P={level} certification read back as {:?}",
            signature.doc_mdp_p
        );
        assert_eq!(
            signature.doc_mdp_v.as_deref(),
            Some("1.2"),
            "the transform parameters must declare the DocMDP version"
        );

        // The catalog must point at this signature, or the level is decoration a reader ignores.
        let document = Document::load_mem(&written).expect("parse");
        let catalog = document
            .trailer
            .get(b"Root")
            .and_then(Object::as_reference)
            .expect("catalog");
        let perms = document
            .get_dictionary(catalog)
            .expect("catalog dictionary")
            .get(b"Perms")
            .and_then(Object::as_dict)
            .expect("/Perms is a dictionary");
        let doc_mdp = perms
            .get(b"DocMDP")
            .and_then(Object::as_reference)
            .expect("/Perms must name the certification signature");
        assert_eq!(
            doc_mdp.0, signature.object_number,
            "the catalog's /Perms /DocMDP must name the certification signature object"
        );
    }
}

/// An approval signature carries no `/Reference` and no `/Perms`, and that absence is the claim.
///
/// The dangerous default is an implicit `P=1`: it would make the user's own later annotation read
/// as tampering on a file they never certified.
#[test]
fn an_approval_signature_writes_no_certification_at_all() {
    let (_dir, source) = source_on_disk("letter.pdf", "P3.2 approval");
    let outcome = sign_save(&source, &fixture_identity()).expect("sign");
    let written = std::fs::read(&outcome.path).expect("read");

    let signatures = read_signatures(&written);
    assert_eq!(signatures.len(), 1);
    assert_eq!(
        signatures[0].doc_mdp_p, None,
        "an approval signature must not declare a DocMDP level"
    );

    let document = Document::load_mem(&written).expect("parse");
    let catalog = document
        .trailer
        .get(b"Root")
        .and_then(Object::as_reference)
        .expect("catalog");
    assert!(
        !document
            .get_dictionary(catalog)
            .expect("catalog dictionary")
            .has(b"Perms"),
        "an approval signature must not add /Perms to the catalog"
    );
}

/// A level outside 1..=3 is refused rather than written.
#[test]
fn a_certification_level_outside_the_legal_range_is_refused() {
    for illegal in [0_i64, 4, -1] {
        assert!(
            Certification::doc_mdp(illegal).is_err(),
            "P={illegal} must be refused; only 1, 2 and 3 are defined"
        );
    }
}

/// Certification must be the first signature, so certifying an already-signed document is refused.
///
/// ISO 32000-1 §12.8.2.2: a document has at most one certification signature, and it is the first.
/// Writing a second would produce a file whose `/Perms` names a signature that is not the earliest,
/// which readers either ignore or report as invalid — a failure the user cannot see from the UI.
#[test]
fn certifying_an_already_signed_document_is_refused() {
    let (_dir, source) = source_on_disk("contract.pdf", "P3.2 certify twice");
    let identity = fixture_identity();

    let first = sign_save(&source, &identity).expect("first, an approval signature");

    let error = sign_save_with(
        std::path::Path::new(&first.path),
        &identity,
        SignOptions {
            certification: Some(Certification::doc_mdp(2).expect("P=2 is legal")),
            ..SignOptions::default()
        },
    )
    .err()
    .expect("certifying an already-signed document must be refused");

    let message = error.to_string();
    assert!(
        message.contains("certif"),
        "the refusal does not explain that certification must come first: {message}"
    );
}

/// Reason and location are written when given, and absent when not.
#[test]
fn the_reason_and_location_reach_the_signature_dictionary() {
    let (_dir, source) = source_on_disk("letter.pdf", "P3.2 reason");
    let outcome = sign_save_with(
        &source,
        &fixture_identity(),
        SignOptions {
            reason: Some("Approved for release".to_string()),
            location: Some("Manchester".to_string()),
            ..SignOptions::default()
        },
    )
    .expect("sign");

    let written = std::fs::read(&outcome.path).expect("read");
    let signatures = read_signatures(&written);
    assert_eq!(
        signatures[0].reason.as_deref(),
        Some("Approved for release"),
        "the reason the user typed must be in the file"
    );
    assert_eq!(
        signatures[0].location.as_deref(),
        Some("Manchester"),
        "the location the user typed must be in the file"
    );

    let (_dir2, plain) = source_on_disk("plain.pdf", "P3.2 no reason");
    let outcome = sign_save(&plain, &fixture_identity()).expect("sign");
    let written = std::fs::read(&outcome.path).expect("read");
    let signatures = read_signatures(&written);
    assert_eq!(
        signatures[0].reason, None,
        "an unstated reason must be absent, not an empty string"
    );
}

/// A certification still verifies: DocMDP is metadata around a signature, not a substitute for one.
#[test]
fn a_certified_document_verifies_through_the_independent_verifier() {
    let (_dir, source) = source_on_disk("contract.pdf", "P3.2 certified verify");
    let outcome = sign_save_with(
        &source,
        &fixture_identity(),
        SignOptions {
            certification: Some(Certification::doc_mdp(3).expect("P=3 is legal")),
            ..SignOptions::default()
        },
    )
    .expect("certify");

    let written = std::fs::read(&outcome.path).expect("read");
    let signature = read_signatures(&written).remove(0);
    verify_independently(
        &signature.der,
        &covered_content(&written, signature.byte_range),
        &fixture_public_key(),
    )
    .expect("a certification signature is still a signature");
}

/// The source file is untouched by certification, exactly as for an approval signature (D-SG2a).
#[test]
fn certifying_leaves_the_source_file_byte_identical() {
    let (_dir, source) = source_on_disk("contract.pdf", "P3.2 certify source");
    let before = std::fs::read(&source).expect("read the source before");

    sign_save_with(
        &source,
        &fixture_identity(),
        SignOptions {
            certification: Some(Certification::doc_mdp(2).expect("P=2 is legal")),
            ..SignOptions::default()
        },
    )
    .expect("certify");

    assert_eq!(
        std::fs::read(&source).expect("read the source after"),
        before,
        "certifying must never modify the source document"
    );
}

/// Every signature carries a `/M` in the PDF date format, and it is a claim not a proof.
///
/// Asserted because a signature with no `/M` renders as undated in most readers, and the field is
/// one line to write and one to forget. The format is pinned rather than the value, since the value
/// is the clock.
#[test]
fn every_signature_states_a_signing_time_in_the_pdf_date_format() {
    let (_dir, source) = source_on_disk("letter.pdf", "P3.2 signing time");
    let outcome = sign_save(&source, &fixture_identity()).expect("sign");
    let written = std::fs::read(&outcome.path).expect("read");

    let document = Document::load_mem(&written).expect("parse");
    let stamp = document
        .objects
        .values()
        .filter_map(|object| object.as_dict().ok())
        .find(|dict| dict.has_type(b"Sig"))
        .and_then(|dict| dict.get(b"M").and_then(Object::as_str).ok())
        .map(|value| String::from_utf8_lossy(value).to_string())
        .expect("every signature dictionary must carry /M");

    // Read back from the *object*, not from a byte search: lopdf writes `/M` with no separator
    // after the `/Contents` hex blob (design K18a), so `/M/D:...` is what appears in the file and a
    // search for `"/M "` finds nothing in a correct document. The first version of this test did
    // exactly that and failed on working code.

    // `D:YYYYMMDDHHmmSSZ` -- two characters of prefix, fourteen digits, one `Z`: seventeen.
    assert!(
        stamp.starts_with("D:") && stamp.ends_with('Z') && stamp.len() == 17,
        "the signing time `{stamp}` is not a PDF date string of the form D:YYYYMMDDHHmmSSZ"
    );
    assert!(
        stamp[2..]
            .trim_end_matches('Z')
            .chars()
            .all(|c| c.is_ascii_digit()),
        "the signing time `{stamp}` carries non-digits"
    );
}

/// `extract_signature` finds the first `/ByteRange`; guard the helper's own limitation so a later
/// reader does not mistake it for a whole-file reader.
#[test]
fn the_shared_extractor_only_reads_the_first_signature_and_that_is_recorded_here() {
    let (_dir, source) = source_on_disk("report.pdf", "P3.2 extractor limit");
    let identity = fixture_identity();
    let first = sign_save(&source, &identity).expect("first");
    let second = sign_save(std::path::Path::new(&first.path), &identity).expect("second");
    let written = std::fs::read(&second.path).expect("read");

    let (der, covered) = extract_signature(&written);
    let signatures = read_signatures(&written);
    // The shared helper searches from byte zero, so it reads signature 1. Assert that rather than
    // leaving the next reader to discover it when a second-signature test fails for a good reason.
    assert!(
        verify_independently(&der, &covered, &fixture_public_key()).is_ok(),
        "the shared extractor must still yield a self-consistent (signature, content) pair"
    );
    assert!(
        signatures.len() > 1,
        "this document must carry two signatures for the limitation to be worth recording"
    );
}
