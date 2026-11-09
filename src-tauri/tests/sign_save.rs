//! SCOPE-C: sign through the **real save path** and verify the file it produced.
//!
//! `sign_pdf.rs` proves the signature given a revision the test built itself. This file proves the
//! thing the product actually does: `sign_save` reads a document off disk, appends a signing
//! revision, writes a new file and verifies it. Nothing here constructs a revision by hand, so a
//! bug in the production builder cannot hide behind an equivalent bug in a test helper.
//!
//! The claims are I-SG2 (the original revision is byte-identical) and D-SG2a (the source is never
//! modified, the output is a new path), plus the negative the design asks for: a tampered byte is
//! detected. Every assertion re-reads the written file from disk.

#[path = "support/cms_verifier.rs"]
mod cms_verifier;

use cms_verifier::{extract_signature, public_key_from_certificate, verify_independently};

use document_reader_lib::editor::signature::pfx::{import_bytes, ImportedIdentity};
use document_reader_lib::editor::signature::sign::{sign_save, signed_path};
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

/// A one-page document on disk, returned with the directory that owns it.
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

// ---------------------------------------------------------------------------------------------
// The happy path, proved through the shipped entry point.
// ---------------------------------------------------------------------------------------------

#[test]
fn signing_through_the_save_path_produces_a_file_that_verifies() {
    let (_dir, source) = source_on_disk("report.pdf", "P3.2 signing");

    let outcome = sign_save(&source, &fixture_identity()).expect("sign the document");

    let written = std::fs::read(&outcome.path).expect("read the signed file");
    let (der, covered) = extract_signature(&written);

    let checked = verify_independently(&der, &covered, &fixture_public_key())
        .expect("the shipped path's signature verifies against an independent implementation");

    // The digest the caller is handed must be the digest of the content that was actually signed:
    // a caller that displays it would otherwise show a hash of something else.
    assert_eq!(
        checked,
        outcome.content_digest.to_vec(),
        "the returned digest is not the messageDigest the signature carries"
    );
}

#[test]
fn the_signed_file_starts_with_the_source_bytes_verbatim() {
    let (_dir, source) = source_on_disk("report.pdf", "P3.2 I-SG2");
    let before = std::fs::read(&source).expect("read the source");

    let outcome = sign_save(&source, &fixture_identity()).expect("sign the document");
    let written = std::fs::read(&outcome.path).expect("read the signed file");

    // I-SG2: the first range is the source, byte for byte. This is what makes the signature a
    // statement about the user's file rather than about a file we rewrote.
    let first_range = outcome.byte_range[1] as usize;
    assert!(
        first_range >= before.len(),
        "the first byte range ({first_range}) does not even cover the source ({} bytes)",
        before.len()
    );
    assert_eq!(
        &written[..before.len()],
        before.as_slice(),
        "the signed file does not begin with the source's bytes"
    );
}

#[test]
fn the_source_file_is_never_modified_and_the_output_is_a_new_path() {
    let (_dir, source) = source_on_disk("report.pdf", "P3.2 D-SG2a");
    let before = std::fs::read(&source).expect("read the source");

    let outcome = sign_save(&source, &fixture_identity()).expect("sign the document");

    assert_ne!(
        std::path::Path::new(&outcome.path),
        source.as_path(),
        "signing must write a new path, never over the original"
    );
    assert_eq!(
        std::fs::read(&source).expect("re-read the source"),
        before,
        "the source file changed; D-SG2a says it never does"
    );
    assert_eq!(
        std::fs::read(&outcome.path).expect("the signed copy exists"),
        std::fs::read(&outcome.path).expect("the signed copy exists again"),
        "the signed copy is not stable"
    );
}

#[test]
fn signing_twice_does_not_overwrite_the_first_signed_copy() {
    let (_dir, source) = source_on_disk("report.pdf", "P3.2 naming");
    let identity = fixture_identity();

    let first = sign_save(&source, &identity).expect("first signature");
    let first_bytes = std::fs::read(&first.path).expect("read the first copy");

    let second = sign_save(&source, &identity).expect("second signature");
    assert_ne!(
        first.path, second.path,
        "the second sign reused the first name"
    );
    assert_eq!(
        std::fs::read(&first.path).expect("the first copy survives"),
        first_bytes,
        "the first signed copy was modified by the second"
    );
}

// ---------------------------------------------------------------------------------------------
// The negatives. A signature that verifies everything is not evidence; these are the cases where
// the answer must be "no".
// ---------------------------------------------------------------------------------------------

#[test]
fn a_tampered_byte_in_the_signed_content_is_detected() {
    let (_dir, source) = source_on_disk("report.pdf", "P3.2 tamper");
    let outcome = sign_save(&source, &fixture_identity()).expect("sign the document");
    let written = std::fs::read(&outcome.path).expect("read the signed file");
    let (der, covered) = extract_signature(&written);

    verify_independently(&der, &covered, &fixture_public_key()).expect("the honest file verifies");

    // Flip one byte of the content the signature covers, well inside the first range and away from
    // any structure the DER walk needs. The signature must not survive it.
    let mut tampered = covered.clone();
    let at = tampered.len() / 2;
    tampered[at] ^= 0x01;

    let refused = verify_independently(&der, &tampered, &fixture_public_key());
    assert!(
        refused.is_err(),
        "a signature verified over content with a flipped byte at {at}"
    );
}

#[test]
fn a_signature_over_one_document_does_not_verify_against_another() {
    let (_dir_a, first) = source_on_disk("first.pdf", "P3.2 document A");
    let (_dir_b, second) = source_on_disk("second.pdf", "P3.2 document B");

    let signed = sign_save(&first, &fixture_identity()).expect("sign the first document");
    let written = std::fs::read(&signed.path).expect("read the signed file");
    let (der, _covered) = extract_signature(&written);

    let other = std::fs::read(&second).expect("read the second document");
    let refused = verify_independently(&der, &other, &fixture_public_key());
    assert!(
        refused.is_err(),
        "the signature over document A verified against document B"
    );
}

#[test]
fn an_encrypted_document_is_refused_rather_than_signed() {
    let (_dir, source) = source_on_disk("secret.pdf", "P3.2 encrypted");

    // Protect it through the shipped protect path, so this is a genuinely encrypted file rather
    // than a hand-set flag.
    let protected = document_reader_lib::editor::protect::protect_save(
        &source,
        "user-password",
        "owner-password",
        document_reader_lib::editor::protect::PermissionsChoice::all(),
    )
    .expect("protect the document");

    let error = sign_save(std::path::Path::new(&protected.path), &fixture_identity())
        .err()
        .expect("signing an encrypted document must be refused");
    assert!(
        error.to_string().contains("password-protected"),
        "the refusal does not explain itself: {error}"
    );
}

// ---------------------------------------------------------------------------------------------
// The namer, tested directly because the file-system half cannot reach every branch.
// ---------------------------------------------------------------------------------------------

#[test]
fn the_signed_name_is_alongside_the_original_and_takes_the_first_free_slot() {
    use std::path::{Path, PathBuf};

    assert_eq!(
        signed_path(Path::new("C:/docs/report.pdf"), |_| false).unwrap(),
        PathBuf::from("C:/docs/report (signed).pdf")
    );
    // Reuse a freed number rather than counting, which is what the user expects after a delete.
    assert_eq!(
        signed_path(Path::new("C:/docs/report.pdf"), |candidate| {
            candidate.ends_with("report (signed).pdf")
        })
        .unwrap(),
        PathBuf::from("C:/docs/report (signed 2).pdf")
    );
    assert_eq!(
        signed_path(Path::new("C:/docs/notes"), |_| false).unwrap(),
        PathBuf::from("C:/docs/notes (signed).pdf"),
        "a document without an extension still gets .pdf"
    );
}
