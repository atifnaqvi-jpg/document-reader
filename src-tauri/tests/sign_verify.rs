//! SCOPE-C Unit 3: the **production** verifier proves a real signature, and a tampered byte
//! changes the answer (I-SG1, I-SG4).
//!
//! Until this file existed, the only verifier in the tree was `tests/support/cms_verifier.rs`, which
//! is test support: nothing the app shipped could compute `Integrity`. The badge therefore had no
//! honest route to `valid`, and the four states D-SG7 defines were three states and an aspiration.
//!
//! This test signs a real document through `sign_save`, reads the file back from disk, and feeds it
//! to `verify::inspect` -- the same function the app will call. It then flips one byte inside the
//! covered range and requires the answer to change.
//!
//! **The tamper must land inside `/ByteRange`.** Flipping a byte in the appended objects, or in the
//! gap, would prove nothing: those bytes are outside what the signature covers, and a verifier that
//! reported `Damaged` for them would be wrong. The test rewrites a byte of the *original* content
//! stream, which is always inside the first range.

use document_reader_lib::editor::signature::badge::Integrity;
use document_reader_lib::editor::signature::pfx::{import_bytes, ImportedIdentity};
use document_reader_lib::editor::signature::sign::sign_save;
use document_reader_lib::editor::signature::verify::{inspect, newest_byte_range};
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
            "Kids" => vec![page_id.into()],
            "Count" => 1,
        }),
    );
    let catalog_id = document.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    document.trailer.set("Root", catalog_id);
    document.save(&path).expect("write the source");
    (dir, path)
}

/// Sign a fresh document and return the written bytes for the verifier to chew on.
fn signed_bytes(text: &str) -> (tempfile::TempDir, Vec<u8>) {
    let (dir, source) = source_on_disk("unsigned.pdf", text);
    let identity = fixture_identity();
    let outcome = sign_save(&source, &identity).expect("signing succeeds");
    let written = std::fs::read(&outcome.path).expect("read the signed file");
    (dir, written)
}

#[test]
fn the_ships_verifier_accepts_a_signature_the_product_wrote() {
    let (_dir, written) = signed_bytes("verify me");

    let reading = inspect(&written)
        .expect("the signed file parses")
        .expect("the file carries a signature dictionary");

    assert_eq!(
        reading.integrity,
        Integrity::Intact,
        "the production verifier must accept a signature this product produced; if it cannot, \
         no badge can ever honestly read `valid`"
    );
}

#[test]
fn a_byte_changed_inside_the_byte_range_makes_the_verifier_say_damaged() {
    let (_dir, written) = signed_bytes("tamper target");

    let range = newest_byte_range(&written).expect("a /ByteRange");
    assert!(range[1] > 0, "the first range must be non-empty");

    // Rewrite one byte inside the covered range. A digit in the content stream is a byte whose
    // change alters the bytes without altering the structure lopdf needs to re-read the file.
    let mut tampered = written.clone();
    let needle = b"tamper target";
    let at = tampered
        .windows(needle.len())
        .position(|window| window == needle)
        .expect("the text is inside the first range");
    assert!(
        (at as i64) < range[1],
        "the tamper must land inside the covered range, not in the appended revision"
    );
    tampered[at] = b'T';

    let reading = inspect(&tampered)
        .expect("the tampered file still parses")
        .expect("the signature dictionary is still there");

    assert_eq!(
        reading.integrity,
        Integrity::Damaged,
        "I-SG4: a changed byte inside /ByteRange must not verify"
    );
}

#[test]
fn an_unsigned_document_reports_no_signature_rather_than_a_badge() {
    let (_dir, source) = source_on_disk("plain.pdf", "no signature here");
    let bytes = std::fs::read(&source).expect("read the source");

    assert_eq!(
        inspect(&bytes).expect("an ordinary PDF parses"),
        None,
        "an unsigned file has no signature to badge; `Unsigned` is decided by absence, not by \
         a failed verification"
    );
}

/// The total length of the outermost DER element, read independently of the production code.
///
/// Written here on purpose rather than imported: a test that measured the blob with the
/// verifier's own reader would agree with a bug in it.
fn der_total_length(bytes: &[u8]) -> usize {
    let first = bytes[1];
    if first & 0x80 == 0 {
        return 2 + first as usize;
    }
    let count = (first & 0x7F) as usize;
    let mut length = 0usize;
    for index in 0..count {
        length = (length << 8) | bytes[2 + index] as usize;
    }
    2 + count + length
}

/// The byte offset of the hex digit that begins the `/Contents` gap.
///
/// Anchored on `/Contents` rather than on the first `<` in the file: a PDF's dictionaries are
/// written `<<`, so the first `<` is a dictionary opener and searching for it silently measures
/// the wrong region.
fn contents_gap_start(written: &[u8]) -> usize {
    const MARKER: &[u8] = b"/Contents";
    let at = written
        .windows(MARKER.len())
        .rposition(|window| window == MARKER)
        .expect("/Contents present")
        + MARKER.len();
    written[at..]
        .iter()
        .position(|byte| *byte == b'<')
        .map(|offset| at + offset + 1)
        .expect("the contents gap is a hex string")
}

/// Flip one byte of the decoded DER inside the `/Contents` gap.
fn corrupt_contents_byte(written: &[u8], index: usize) -> Vec<u8> {
    let hex_at = contents_gap_start(written) + index * 2;
    let mut out = written.to_vec();
    for offset in 0..2 {
        let at = hex_at + offset;
        out[at] = if out[at] == b'0' { b'1' } else { b'0' };
    }
    out
}

#[test]
fn a_broken_rsa_signature_is_damaged_even_though_the_digest_still_matches() {
    // This is the test that proves the RSA check runs at all. Corrupting a byte of the signature
    // itself leaves the covered range untouched, so `messageDigest` still matches: only the
    // public-key verification can tell this file apart from a good one. Without this test a
    // verifier that compared the digest and then returned `Intact` unconditionally would pass
    // every other test in this file.
    let (_dir, written) = signed_bytes("signature arithmetic");

    let hex = &written[contents_gap_start(&written)..];
    let decoded_length = hex
        .iter()
        .take_while(|byte| byte.is_ascii_hexdigit())
        .count()
        / 2;
    assert!(
        decoded_length > 16,
        "the contents gap holds a certificate and a signature"
    );

    let mut der = Vec::with_capacity(decoded_length);
    for pair in hex.chunks(2).take(decoded_length) {
        let high = (pair[0] as char).to_digit(16).expect("hex") as u8;
        let low = (pair[1] as char).to_digit(16).expect("hex") as u8;
        der.push((high << 4) | low);
    }
    let total = der_total_length(&der);
    // Five bytes from the end of the DER: inside the RSA signature value, past the attributes.
    let tampered = corrupt_contents_byte(&written, total - 5);

    let reading = inspect(&tampered)
        .expect("the tampered file still parses")
        .expect("the signature dictionary is still there");

    assert_eq!(
        reading.integrity,
        Integrity::Damaged,
        "a corrupted RSA signature must not verify, even though the signed range is untouched"
    );
}

#[test]
fn the_newest_signature_is_the_one_that_is_read() {
    // Sign the signed copy: the result carries two `/ByteRange` arrays. Reading the first would
    // report on the older revision, which is not what the badge is about.
    let (_dir, source) = source_on_disk("twice.pdf", "two signatures");
    let identity = fixture_identity();
    let first = sign_save(&source, &identity).expect("first signature");
    let second = sign_save(std::path::Path::new(&first.path), &identity).expect("second signature");

    assert_ne!(
        first.byte_range, second.byte_range,
        "the two revisions must have distinct ranges or this test proves nothing"
    );

    let written = std::fs::read(&second.path).expect("read the twice-signed file");
    assert_eq!(
        newest_byte_range(&written),
        Some(second.byte_range),
        "the verifier must report the newest signature's range, not the first one in the file"
    );
    assert_eq!(
        inspect(&written)
            .expect("parses")
            .expect("signed")
            .integrity,
        Integrity::Intact,
        "the newest revision still verifies"
    );
}

#[test]
fn the_verifier_reports_the_range_it_actually_checked() {
    // A verifier that returned the range it was *told* about rather than the one in the file would
    // pass the happy path and be useless the moment the two disagreed.
    let (_dir, written) = signed_bytes("range check");
    let outcome_range = {
        let (_d, source) = source_on_disk("again.pdf", "range check");
        let identity = fixture_identity();
        sign_save(&source, &identity)
            .expect("signing succeeds")
            .byte_range
    };
    let observed = newest_byte_range(&written).expect("a /ByteRange in the file");
    assert_eq!(
        observed, outcome_range,
        "the file and the outcome must agree"
    );
    assert_eq!(observed[0], 0, "a PDF /ByteRange always starts at zero");
}
