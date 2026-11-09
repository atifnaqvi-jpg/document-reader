//! SCOPE-C unit 4: sign a real document over the `/ByteRange` gap, then verify the result
//! against an implementation that did not produce it.
//!
//! The substrate tests (`signature_substrate.rs`) proved the *container*: that a `/Contents` gap
//! and a `/ByteRange` can be reserved, patched in place, and patched without moving a byte. This
//! file proves the *signature*: that the bytes patched into that gap are a detached CMS signature
//! over exactly the bytes the container declared, and that a second implementation agrees.
//!
//! **Why the verification here is written by hand.** I-SG1 asks for an implementation other than
//! our verifier. If this file called `cms` to parse what `cms` wrote, a builder that dropped a
//! `messageDigest` attribute would agree with itself and pass — which is precisely the failure
//! measured in `docs/P3.2-FACTS-cms-signing.md` F2. So [`independent_verifier`] walks the DER
//! itself: it locates the `SignedAttributes` SET, re-encodes it with the SET tag DER requires for
//! signing, and verifies the RSA PKCS#1 v1.5 signature over that encoding with `rsa`'s verifier.
//! `rsa`'s *verifier* shares no code with `cms`'s *builder*, which is the axis that matters.
//!
//! **Every proof reads back out of the written file.** The claims are about a file on disk, so
//! each one re-reads the bytes it just wrote and re-derives offsets, digests and the signature
//! from those bytes. A test that verified an in-memory buffer would pass while the writer emitted
//! something else.

use der_cms::{Decode, Encode};
use lopdf::{dictionary, Document, IncrementalDocument, Object, StringFormat};
use sha2::{Digest, Sha256};

#[path = "support/cms_verifier.rs"]
mod cms_verifier;

use cms_verifier::{
    extract_signature, independent_verifier, oid_matches, public_key_from_certificate,
    verify_independently, OID_RSA_ENCRYPTION, OID_SHA256, OID_SHA256_WITH_RSA,
};

/// The fixture signer's public key, from the certificate its blob ships.
fn fixture_public_key() -> rsa::RsaPublicKey {
    let identity = fixture_identity();
    public_key_from_certificate(&identity.chain[0])
}

use document_reader_lib::editor::signature::pfx::{import_bytes, ImportedIdentity};

use document_reader_lib::editor::signature::cms::{
    seal_revision, sign_detached, ReservedGap, BYTE_RANGE_FIELD_WIDTH, CONTENTS_BYTES,
};

/// The placeholder written into each reserved field before patching: `10^10 - 1`, i.e. exactly
/// `BYTE_RANGE_FIELD_WIDTH` digits, so the digit count is right before anything is patched.
const BYTE_RANGE_PLACEHOLDER: i64 = 9_999_999_999;

/// The fixture password, recorded in `tests/fixtures/signing/README.md`.
const PFX_PASSWORD: &str = "correct-horse";

fn fixture_pfx() -> Vec<u8> {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/signing/signer.pfx");
    std::fs::read(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

/// The one identity in the fixture archive.
fn fixture_identity() -> ImportedIdentity {
    let imported = import_bytes(&fixture_pfx(), PFX_PASSWORD).expect("the fixture PFX opens");
    imported
        .identities
        .into_iter()
        .next()
        .expect("the fixture PFX carries one identity")
}

/// A one-page source document on disk, and its bytes.
///
/// Deliberately the same shape `signature_substrate.rs` builds, so the two files cannot drift into
/// proving different substrates.
fn source_on_disk(name: &str) -> (tempfile::TempDir, std::path::PathBuf, Vec<u8>) {
    source_on_disk_with_text(name, "SCOPE-C signature")
}

/// As [`source_on_disk`], with the page text chosen by the caller.
///
/// A second parameter exists so a test that needs two *different* documents actually gets them.
/// Two calls with the same text produced byte-near-identical files, and the assertion meant to
/// prove "a signature over one document does not verify against another" passed the wrong
/// comparison until this was fixed.
fn source_on_disk_with_text(
    name: &str,
    text: &str,
) -> (tempfile::TempDir, std::path::PathBuf, Vec<u8>) {
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

    let bytes = std::fs::read(&path).expect("read the source back");
    (dir, path, bytes)
}

/// A written signing revision with patched offsets, exactly as `signature_substrate.rs` builds it.
struct SigningRevision {
    bytes: Vec<u8>,
    contents_open: usize,
    contents_close: usize,
    field_offsets: [usize; 3],
}

impl SigningRevision {
    fn patch_byte_range(&mut self, values: [i64; 3]) {
        for (value, offset) in values.iter().zip(self.field_offsets) {
            let text = format!("{value:0BYTE_RANGE_FIELD_WIDTH$}");
            assert_eq!(text.len(), BYTE_RANGE_FIELD_WIDTH);
            self.bytes[offset..offset + BYTE_RANGE_FIELD_WIDTH].copy_from_slice(text.as_bytes());
        }
    }

    fn byte_range(&self) -> [i64; 4] {
        [
            0,
            (self.contents_open - 1) as i64,
            (self.contents_close + 1) as i64,
            (self.bytes.len() - (self.contents_close + 1)) as i64,
        ]
    }

    /// The byte string the signature covers: the file with the gap **and both delimiters** removed.
    fn signed_content(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.bytes.len());
        out.extend_from_slice(&self.bytes[..self.contents_open - 1]);
        out.extend_from_slice(&self.bytes[self.contents_close + 1..]);
        out
    }

    fn gap(&self) -> ReservedGap {
        ReservedGap {
            contents_open: self.contents_open,
            contents_close: self.contents_close,
            field_offsets: self.field_offsets,
        }
    }

    fn patch_contents(&mut self, payload: &[u8]) -> Result<(), String> {
        if payload.len() > CONTENTS_BYTES {
            return Err(format!(
                "a {} byte signature does not fit the {} byte reserved gap",
                payload.len(),
                CONTENTS_BYTES
            ));
        }
        let mut hex = String::with_capacity(2 * CONTENTS_BYTES);
        for byte in payload {
            hex.push_str(&format!("{byte:02X}"));
        }
        hex.push_str(&"0".repeat(2 * (CONTENTS_BYTES - payload.len())));
        if hex.len() != self.contents_close - self.contents_open {
            return Err(format!(
                "the encoded signature is {} characters in a {} character gap",
                hex.len(),
                self.contents_close - self.contents_open
            ));
        }
        self.bytes[self.contents_open..self.contents_close].copy_from_slice(hex.as_bytes());
        Ok(())
    }
}

fn build_signing_revision(source_bytes: &[u8]) -> SigningRevision {
    let prev = Document::load_mem(source_bytes).expect("reload the source");
    let page_id = *prev
        .get_pages()
        .values()
        .next()
        .expect("the source has one page");
    let catalog = prev
        .trailer
        .get(b"Root")
        .and_then(Object::as_reference)
        .expect("catalog");

    let mut incremental = IncrementalDocument::create_from(source_bytes.to_vec(), prev);

    let signature_id = incremental.new_document.add_object(dictionary! {
        "Type" => "Sig",
        "Filter" => "Adobe.PPKLite",
        "SubFilter" => "adbe.pkcs7.detached",
        "ByteRange" => vec![
            0.into(),
            BYTE_RANGE_PLACEHOLDER.into(),
            BYTE_RANGE_PLACEHOLDER.into(),
            BYTE_RANGE_PLACEHOLDER.into(),
        ],
        "Contents" => Object::String(vec![0u8; CONTENTS_BYTES], StringFormat::Hexadecimal),
    });
    let annot_id = incremental.new_document.add_object(dictionary! {
        "Type" => "Annot",
        "Subtype" => "Widget",
        "FT" => "Sig",
        "Rect" => vec![0.into(), 0.into(), 0.into(), 0.into()],
        "V" => Object::Reference(signature_id),
    });
    incremental
        .opt_clone_object_to_new_document(page_id)
        .expect("clone the page");
    incremental
        .new_document
        .get_object_mut(page_id)
        .and_then(Object::as_dict_mut)
        .expect("page dict")
        .set("Annots", Object::Array(vec![Object::Reference(annot_id)]));
    incremental
        .opt_clone_object_to_new_document(catalog)
        .expect("clone the catalog");
    incremental
        .new_document
        .get_object_mut(catalog)
        .and_then(Object::as_dict_mut)
        .expect("catalog dict")
        .set(
            "AcroForm",
            dictionary! {
                "SigFlags" => 3,
                "Fields" => vec![Object::Reference(annot_id)],
            },
        );

    let mut bytes = Vec::new();
    incremental
        .save_to(&mut bytes)
        .expect("append the signing revision");

    let appended_start = source_bytes.len();
    let appended = &bytes[appended_start..];

    let contents_marker = b"/Contents<";
    let contents_hits: Vec<usize> = appended
        .windows(contents_marker.len())
        .enumerate()
        .filter(|(_, window)| *window == contents_marker)
        .map(|(index, _)| index)
        .collect();
    assert_eq!(contents_hits.len(), 1, "exactly one hex /Contents");
    let contents_open = appended_start + contents_hits[0] + contents_marker.len();
    let contents_close = contents_open + 2 * CONTENTS_BYTES;

    let range_marker = b"/ByteRange[";
    let range_hits: Vec<usize> = appended
        .windows(range_marker.len())
        .enumerate()
        .filter(|(_, window)| *window == range_marker)
        .map(|(index, _)| index)
        .collect();
    assert_eq!(range_hits.len(), 1, "exactly one /ByteRange");
    let range_start = appended_start + range_hits[0] + range_marker.len();

    let placeholder = format!("{BYTE_RANGE_PLACEHOLDER}");
    let mut field_offsets = [0usize; 3];
    let mut cursor = range_start;
    for (found, slot) in field_offsets.iter_mut().enumerate() {
        let hit = bytes[cursor..]
            .windows(placeholder.len())
            .position(|window| window == placeholder.as_bytes())
            .unwrap_or_else(|| panic!("reserved byte-range field {found} is missing"));
        *slot = cursor + hit;
        cursor += hit + placeholder.len();
    }

    SigningRevision {
        bytes,
        contents_open,
        contents_close,
        field_offsets,
    }
}

// ---------------------------------------------------------------------------------------------
// The independent verifier: DER walked by hand, RSA verified by `rsa`, nothing from `cms`.
// ---------------------------------------------------------------------------------------------

// ---------------------------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------------------------

#[test]
fn a_signed_document_carries_a_detached_cms_over_its_byte_range() {
    let (_dir, _path, source) = source_on_disk("signed-source.pdf");
    let revision = build_signing_revision(&source);

    let sealed = seal_revision(&revision.bytes, revision.gap(), &fixture_identity())
        .expect("the fixture signs the revision");

    // Read back out of the file this test just built, not out of the variables above.
    let out_dir = tempfile::tempdir().unwrap();
    let signed_path = out_dir.path().join("signed.pdf");
    std::fs::write(&signed_path, &sealed.bytes).expect("write the signed file");
    let written = std::fs::read(&signed_path).expect("read the signed file back");

    // Locate the gap again from the bytes, so a stale offset cannot make this pass.
    let marker = b"/Contents<";
    let hits: Vec<usize> = written
        .windows(marker.len())
        .enumerate()
        .filter(|(_, window)| *window == marker)
        .map(|(index, _)| index)
        .collect();
    assert_eq!(
        hits.len(),
        1,
        "exactly one hex /Contents in the written file"
    );
    let open = hits[0] + marker.len();
    let close = open + 2 * CONTENTS_BYTES;
    assert_eq!(written[open - 1], b'<');
    assert_eq!(written[close], b'>');

    // Decode the hex back to DER: this is what a reader does, and it is the only way to prove the
    // gap holds a signature rather than placeholder digits.
    let hex_gap = std::str::from_utf8(&written[open..close]).expect("hex is ASCII");
    let mut decoded = Vec::with_capacity(CONTENTS_BYTES);
    for pair in hex_gap.as_bytes().chunks(2) {
        let text = std::str::from_utf8(pair).unwrap();
        decoded.push(u8::from_str_radix(text, 16).expect("hex digits"));
    }
    let trailing_zeros = decoded.iter().rev().take_while(|byte| **byte == 0).count();
    let der_len = decoded.len() - trailing_zeros;
    let signature_der = &decoded[..der_len];

    assert!(
        der_len > 0 && der_len < CONTENTS_BYTES,
        "the gap must hold a real signature and unused zero padding; found {der_len} bytes"
    );
    // The gap is the signature followed by zero padding; the DER we verify starts with SEQUENCE.
    assert_eq!(
        signature_der[0], 0x30,
        "the gap must begin with a DER SEQUENCE"
    );
    assert!(
        trailing_zeros > 0,
        "the unused tail of the gap must stay as zero padding, not be trimmed"
    );

    // Now the claim that matters: an independent verifier, over the content recomputed from the
    // written file, accepts the signature in the written file.
    let mut recomputed = Vec::with_capacity(written.len());
    recomputed.extend_from_slice(&written[..open - 1]);
    recomputed.extend_from_slice(&written[close + 1..]);
    let checked = verify_independently(signature_der, &recomputed, &fixture_public_key())
        .expect("the independent verifier accepts the written signature");
    assert_eq!(
        checked,
        Sha256::digest(&recomputed).to_vec(),
        "the verified messageDigest is the digest of the content the file declares"
    );
}

#[test]
fn the_declared_byte_range_matches_the_gap_in_the_written_file() {
    let (_dir, _path, source) = source_on_disk("range-source.pdf");
    let revision = build_signing_revision(&source);

    let sealed = seal_revision(&revision.bytes, revision.gap(), &fixture_identity()).unwrap();

    let out_dir = tempfile::tempdir().unwrap();
    let signed_path = out_dir.path().join("range-signed.pdf");
    std::fs::write(&signed_path, &sealed.bytes).unwrap();
    let written = std::fs::read(&signed_path).unwrap();

    // Parse the patched /ByteRange out of the bytes and check it describes this file.
    let marker = b"/ByteRange[";
    let at = written
        .windows(marker.len())
        .position(|window| window == marker)
        .expect("/ByteRange present")
        + marker.len();
    let tail = std::str::from_utf8(&written[at..at + 48]).expect("ASCII digits");
    let end = tail.find(']').expect("array closes");
    let numbers: Vec<usize> = tail[..end]
        .split_whitespace()
        .map(|part| part.trim_end_matches(',').parse::<usize>().unwrap())
        .collect();
    assert_eq!(numbers.len(), 4, "a /ByteRange has four integers");

    let [start, first, second_start, second_len] = [numbers[0], numbers[1], numbers[2], numbers[3]];
    assert_eq!(start, 0, "the first range starts at byte 0");

    // The range `seal_revision` reports must be the range the file carries, all four integers.
    // Without this the leading 0 in the file came from the revision builder and the first element
    // of the returned array was never read — a mutation setting it to 1 survived every test.
    assert_eq!(
        sealed.byte_range,
        [
            start as i64,
            first as i64,
            second_start as i64,
            second_len as i64
        ],
        "the declared range must be the range written into the file"
    );
    assert_eq!(
        second_start + second_len,
        written.len(),
        "the second range must end at end-of-file, otherwise bytes are unsigned"
    );

    // The excluded window must be exactly `<` + hex + `>`.
    let excluded = written.len() - first - second_len;
    assert_eq!(
        excluded,
        2 + 2 * CONTENTS_BYTES,
        "the excluded window must be the two delimiters plus the gap"
    );
    assert_eq!(written[first], b'<', "range 1 ends at the '<'");
    assert_eq!(
        written[second_start - 1],
        b'>',
        "range 2 starts after the '>'"
    );

    // And the signature over the range's own segmentation verifies.
    let mut covered = Vec::new();
    covered.extend_from_slice(&written[start..start + first]);
    covered.extend_from_slice(&written[second_start..second_start + second_len]);

    let hex_gap = std::str::from_utf8(&written[first + 1..second_start - 1]).unwrap();
    let mut decoded = Vec::new();
    for pair in hex_gap.as_bytes().chunks(2) {
        decoded.push(u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap());
    }
    let der_len = decoded.len() - decoded.iter().rev().take_while(|b| **b == 0).count();

    verify_independently(&decoded[..der_len], &covered, &fixture_public_key())
        .expect("the signature verifies over the ranges the file declares");
}

#[test]
fn the_independent_verifier_rejects_content_edited_outside_the_gap() {
    let (_dir, _path, source) = source_on_disk("tamper-source.pdf");
    let revision = build_signing_revision(&source);
    let sealed = seal_revision(&revision.bytes, revision.gap(), &fixture_identity()).unwrap();
    let (signature_der, content) = extract_signature(&sealed.bytes);

    // Sanity: the honest content verifies.
    verify_independently(&signature_der, &content, &fixture_public_key())
        .expect("the honest content verifies");

    // Flip one byte in the region the range covers.
    let mut tampered = content.clone();
    let victim = tampered.len() / 2;
    tampered[victim] ^= 0xFF;

    let refused = verify_independently(&signature_der, &tampered, &fixture_public_key())
        .expect_err("a change outside the gap must be refused");
    assert!(
        refused.contains("messageDigest") || refused.contains("RSA verification"),
        "the refusal must name the digest or the signature, not fail for an unrelated reason: {refused}"
    );
}

#[test]
fn a_signature_over_different_content_does_not_verify() {
    // The strongest negative: a *valid* signature, produced by the same key over a different
    // document. Tampering tests the digest; this tests that the digest is actually consulted.
    let (_dir, _path, first) = source_on_disk_with_text("first.pdf", "the first document");
    let (_dir2, _path2, second) = source_on_disk_with_text("second.pdf", "an entirely other file");
    assert_ne!(first, second, "the two sources must differ");

    let first_revision = build_signing_revision(&first);
    let sealed = seal_revision(
        &first_revision.bytes,
        first_revision.gap(),
        &fixture_identity(),
    )
    .unwrap();
    let (signature_der, first_content) = extract_signature(&sealed.bytes);

    verify_independently(&signature_der, &first_content, &fixture_public_key())
        .expect("its own content verifies");

    let second_revision = build_signing_revision(&second);
    let second_content = second_revision.signed_content();
    assert_ne!(
        first_content, second_content,
        "the two documents must produce different signed content"
    );
    let refused = verify_independently(&signature_der, &second_content, &fixture_public_key())
        .expect_err("a signature over one document must not verify against another");
    assert!(
        refused.contains("messageDigest"),
        "the refusal should be the digest mismatch: {refused}"
    );
}

#[test]
fn the_returned_digest_is_the_digest_of_what_was_signed() {
    let (_dir, _path, source) = source_on_disk("digest-source.pdf");
    let revision = build_signing_revision(&source);
    let sealed = seal_revision(&revision.bytes, revision.gap(), &fixture_identity()).unwrap();
    let (_, content) = extract_signature(&sealed.bytes);

    // The module returns what it signed so the caller cannot hash a different buffer. Check that
    // claim against an independent hash of the same slice.
    assert_eq!(
        sealed.content_digest,
        <[u8; 32]>::from(Sha256::digest(&content)),
        "the returned digest must be sha256 of the finished file's signed content"
    );
}

#[test]
fn a_signature_larger_than_the_gap_is_refused_rather_than_truncated() {
    let (_dir, _path, source) = source_on_disk("overflow-source.pdf");
    let mut revision = build_signing_revision(&source);
    let oversized = vec![0x30u8; CONTENTS_BYTES + 1];
    let refused = revision
        .patch_contents(&oversized)
        .expect_err("an oversized signature must be refused");
    assert!(
        refused.contains("does not fit"),
        "the refusal must say the signature does not fit: {refused}"
    );
    // And nothing was written: a truncated signature is worse than none, because it looks valid.
    let hex_gap =
        std::str::from_utf8(&revision.bytes[revision.contents_open..revision.contents_close])
            .unwrap();
    assert!(
        hex_gap.bytes().all(|byte| byte == b'0'),
        "a refused signature must leave the gap untouched"
    );
}

#[test]
fn the_signer_identifier_matches_the_entity_certificate() {
    // A SignerInfo whose `sid` names a different certificate than the one shipped is a signature a
    // reader cannot attribute. Check the two agree, from the blob rather than from our own code.
    let (_dir, _path, source) = source_on_disk("sid-source.pdf");
    let revision = build_signing_revision(&source);
    let sealed = seal_revision(&revision.bytes, revision.gap(), &fixture_identity()).unwrap();
    let (signature_der, _) = extract_signature(&sealed.bytes);

    let reading = independent_verifier(&signature_der);
    assert!(reading.detached, "adbe.pkcs7.detached carries no eContent");
    assert!(
        reading.message_digest.is_some(),
        "a detached signature without messageDigest verifies against nothing"
    );

    // The entity certificate must be carried *as a certificate*, not merely have its serial
    // number appear somewhere. The first version of this test searched the blob for the serial
    // bytes and passed with the certificate absent, because `sid` contains the same serial —
    // a mutation that dropped the certificate from the blob survived it. Compare DER equality.
    let entity = &fixture_identity().chain[0];
    let entity_der = x509_cert_cms::Certificate::from_der(entity)
        .expect("parse the fixture entity certificate")
        .to_der()
        .expect("re-encode it");
    assert!(
        !reading.certificates.is_empty(),
        "a detached signature must carry the signer's certificate, or nothing can attribute it"
    );
    assert!(
        reading.certificates.contains(&entity_der),
        "the entity certificate must be among the {} carried certificates",
        reading.certificates.len()
    );
}

#[test]
fn every_signature_is_sha256_and_pkcs1v15() {
    let (_dir, _path, source) = source_on_disk("alg-source.pdf");
    let revision = build_signing_revision(&source);
    let sealed = seal_revision(&revision.bytes, revision.gap(), &fixture_identity()).unwrap();
    let (signature_der, _) = extract_signature(&sealed.bytes);

    let reading = independent_verifier(&signature_der);
    assert!(oid_matches(&reading.digest_oid, OID_SHA256), "SHA-256 only");
    assert!(
        oid_matches(&reading.signature_oid, OID_SHA256_WITH_RSA)
            || oid_matches(&reading.signature_oid, OID_RSA_ENCRYPTION),
        "the signature algorithm must be RSA PKCS#1 v1.5; saw {:02X?}",
        reading.signature_oid
    );
}

#[test]
fn the_source_file_is_untouched_by_signing() {
    // D-SG2a: the source is never modified; the signed output is a new path.
    let (_dir, path, source) = source_on_disk("untouched-source.pdf");
    let revision = build_signing_revision(&source);
    let sealed = seal_revision(&revision.bytes, revision.gap(), &fixture_identity()).unwrap();

    let out_dir = tempfile::tempdir().unwrap();
    std::fs::write(out_dir.path().join("out.pdf"), &sealed.bytes).unwrap();

    let after = std::fs::read(&path).expect("re-read the source");
    assert_eq!(
        after, source,
        "the source file must be byte-identical after signing"
    );
}

#[test]
fn the_signed_file_starts_with_the_source_bytes() {
    // I-SG2 at the CMS unit: the signed output begins with the source verbatim, which is what lets
    // the first byte range describe the user's own bytes.
    let (_dir, _path, source) = source_on_disk("prefix-source.pdf");
    let revision = build_signing_revision(&source);
    let sealed = seal_revision(&revision.bytes, revision.gap(), &fixture_identity()).unwrap();

    assert_eq!(
        &sealed.bytes[..source.len()],
        source.as_slice(),
        "the first {} bytes must be the source file, byte for byte",
        source.len()
    );
}

#[test]
fn the_signed_file_still_parses_and_resolves_its_signature() {
    // The container test proves this for a placeholder; this proves it for a real signature. A
    // file that parses but whose signature dictionary is unreachable is not a signed document.
    let (_dir, _path, source) = source_on_disk("resolve-source.pdf");
    let revision = build_signing_revision(&source);
    let sealed = seal_revision(&revision.bytes, revision.gap(), &fixture_identity()).unwrap();

    let out_dir = tempfile::tempdir().unwrap();
    let signed_path = out_dir.path().join("resolve-signed.pdf");
    std::fs::write(&signed_path, &sealed.bytes).unwrap();

    let document = Document::load(&signed_path).expect("the signed file must parse");
    let pages = document.get_pages();
    assert_eq!(pages.len(), 1, "the page survives signing");

    // Walk page -> /Annots -> /V to the signature dictionary, then read its /SubFilter.
    let page_id = *pages.values().next().unwrap();
    let page = document.get_object(page_id).unwrap().as_dict().unwrap();
    let annots = page.get(b"Annots").unwrap().as_array().unwrap();
    assert_eq!(annots.len(), 1, "the page carries one annotation");
    let annot_id = annots[0].as_reference().expect("annotation reference");
    let annot = document.get_object(annot_id).unwrap().as_dict().unwrap();
    assert_eq!(annot.get(b"Subtype").unwrap().as_name().unwrap(), b"Widget");
    assert_eq!(annot.get(b"FT").unwrap().as_name().unwrap(), b"Sig");

    let sig_id = annot.get(b"V").unwrap().as_reference().unwrap();
    let sig = document.get_object(sig_id).unwrap().as_dict().unwrap();
    assert_eq!(
        sig.get(b"SubFilter").unwrap().as_name().unwrap(),
        b"adbe.pkcs7.detached",
        "the SubFilter must denote a detached CMS"
    );

    let contents = sig.get(b"Contents").unwrap().as_str().unwrap();
    assert!(
        contents.len() == CONTENTS_BYTES,
        "the /Contents gap is {} bytes, found {}",
        CONTENTS_BYTES,
        contents.len()
    );
    assert!(
        contents.iter().any(|byte| *byte != 0),
        "the gap must hold signature bytes, not only padding"
    );
}

#[test]
fn signing_before_patching_the_byte_range_is_caught() {
    // Pins the ordering fact that `seal_revision` exists to enforce. Done the wrong way round --
    // hash, sign, *then* write the range digits -- the file is still well formed and the blob still
    // parses; only a verifier that recomputes from the finished bytes notices. If this test ever
    // passes, the oracle has stopped checking and the whole file is decoration.
    let (_dir, _path, source) = source_on_disk("order-source.pdf");
    let mut revision = build_signing_revision(&source);

    // Wrong order: sign the placeholder-digit content first.
    let content_with_placeholders = revision.signed_content();
    let premature = sign_detached(&fixture_identity(), &content_with_placeholders).unwrap();
    revision.patch_contents(&premature.der).unwrap();
    let range = revision.byte_range();
    revision.patch_byte_range([range[1], range[2], range[3]]);

    let (signature_der, covered) = extract_signature(&revision.bytes);
    let refused = verify_independently(&signature_der, &covered, &fixture_public_key())
        .expect_err("a signature made before the range was patched must not verify");
    assert!(
        refused.contains("messageDigest"),
        "the mismatch should be the digest, because the digits changed after hashing: {refused}"
    );

    // And the correct order does verify, so this test cannot pass by the oracle refusing everything.
    let good_revision = build_signing_revision(&source);
    let sealed = seal_revision(
        &good_revision.bytes,
        good_revision.gap(),
        &fixture_identity(),
    )
    .unwrap();
    let (good_der, good_content) = extract_signature(&sealed.bytes);
    verify_independently(&good_der, &good_content, &fixture_public_key())
        .expect("the correct order must verify, or the refusal above proves nothing");
}
