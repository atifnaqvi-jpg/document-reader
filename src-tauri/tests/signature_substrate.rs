//! SCOPE-C's second substrate proof: the **signing shape** on top of `IncrementalDocument`.
//!
//! `incremental_proof.rs` proved that a second revision can be appended and read. That is
//! necessary and not sufficient: signing needs three further properties, and each of them
//! invalidates D-SG2 if it fails, which is why they are proven before any signing code exists.
//!
//! 1. **The `/Contents` gap is sized before the signature exists.** A signature cannot know its
//!    own DER length until it is produced, and it cannot be produced until the bytes it covers
//!    exist. The gap breaks that cycle, so the gap has to be reservable at a fixed width.
//! 2. **Patching the gap moves nothing.** `/ByteRange` describes offsets in the file as written.
//!    If filling in the signature shifted any byte outside the gap, the range would describe a
//!    file that no longer exists and every signature would verify as `modified`.
//! 3. **`/ByteRange` is computable before the signature is.** Its integers depend on where the gap
//!    landed and on the total file length, which is only known after the revision is written. So
//!    the range is reserved at a fixed width too, and patched in place in the same pass.
//!
//! **Measured, not assumed — two facts that shaped the mechanism:**
//!
//! * lopdf writes a name followed by a string with **no** separator (a string is not in
//!   `Writer::need_separator`), so a hex `/Contents` renders as `/Contents<…>`, not
//!   `/Contents <…>`. A byte range assuming a space is off by one and verifies against nothing.
//! * lopdf serialises `Object::Integer` **minimally**, so leading zeros cannot be carried through
//!   the object model: a zero-padded placeholder written as an integer comes back shorter and the
//!   file length changes between the measuring pass and the real pass. Verified here by building
//!   the revision twice and comparing lengths, which failed by exactly the stripped zeros. The
//!   reserved width is therefore obtained by writing a placeholder whose value *already has* the
//!   final digit count, then overwriting the digits as raw bytes.
//!
//! The convention asserted here — byte range 1 ends at the `<` and byte range 2 starts after the
//! `>`, so **both delimiters are excluded** — is the one iText, PDFBox and Acrobat produce. It is
//! stated as a constant below rather than left implicit, because "which bracket is excluded" is
//! the kind of detail that verifies against itself and against nothing else.
//!
//! Not PDFium-gated: every claim here is about bytes and offsets, and §7 requires a Linux-side
//! equivalent for anything asserted about the shipped product. Reopening with `lopdf` is included
//! because a revision that no reader can parse satisfies none of the above.

use lopdf::{dictionary, Document, IncrementalDocument, Object, StringFormat};

/// Reserved signature space, in DER bytes. Real CMS blobs for an RSA-2048 signer with one
/// certificate and the CAdES signed attributes land around 4–6 KiB; 8192 leaves room for a
/// certificate chain without reserving so much that a signed file visibly grows.
///
/// The whole point of the gap is that this number is a *ceiling*: the unused tail stays as zero
/// digits inside the hex string. A signature that overflows it must be refused, not truncated.
const CONTENTS_BYTES: usize = 8192;

/// Width reserved for each variable `/ByteRange` integer, in decimal digits. Ten digits covers a
/// ten-gigabyte file, far beyond anything this product will sign, and the reserved width is what
/// makes the range patchable in place.
const BYTE_RANGE_FIELD_WIDTH: usize = 10;

/// The placeholder value written into each reserved field: `10^10 - 1`, i.e. exactly
/// `BYTE_RANGE_FIELD_WIDTH` digits, so the digit count is right before anything is patched.
const BYTE_RANGE_PLACEHOLDER: i64 = 9_999_999_999;

/// A one-page source document, saved to disk, plus its bytes.
///
/// Deliberately the same shape `incremental_proof.rs` uses: the two files must be proving the
/// same substrate, and a different fixture here would hide a disagreement between them.
fn source_on_disk(name: &str) -> Vec<u8> {
    let mut doc = Document::with_version("1.7");
    let content_id = doc.add_object(lopdf::Stream::new(dictionary! {}, b"BT ET".to_vec()));
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
        "docreader-signature-substrate-{name}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("source.pdf");
    doc.save(&path).expect("save source");
    std::fs::read(&path).expect("read source")
}

/// A built signing revision, with the offsets a signer patches rather than re-derives.
#[derive(Debug)]
struct SigningRevision {
    bytes: Vec<u8>,
    /// Byte offsets of the `<` and `>` around the hex contents.
    contents_open: usize,
    contents_close: usize,
    /// Byte offset of the first digit of each variable `/ByteRange` field, in order. Field 0 is
    /// the constant `0` and is not reserved.
    field_offsets: [usize; 3],
}

impl SigningRevision {
    /// Patch the three variable fields, zero-padded to the reserved width so the file length is
    /// identical to the placeholder pass.
    fn patch_byte_range(&mut self, values: [i64; 3]) {
        for (value, offset) in values.iter().zip(self.field_offsets) {
            let text = format!("{value:0BYTE_RANGE_FIELD_WIDTH$}");
            assert_eq!(text.len(), BYTE_RANGE_FIELD_WIDTH);
            self.bytes[offset..offset + BYTE_RANGE_FIELD_WIDTH].copy_from_slice(text.as_bytes());
        }
    }

    /// The `/ByteRange` this revision will declare, in the four-member form the format uses.
    fn byte_range(&self) -> [i64; 4] {
        // Convention: the digest covers everything except the `<`, the hex digits and the `>`.
        [
            0,
            (self.contents_open - 1) as i64,
            (self.contents_close + 1) as i64,
            (self.bytes.len() - (self.contents_close + 1)) as i64,
        ]
    }

    /// Fill the reserved gap with `payload`, or refuse.
    ///
    /// Returns `Err` rather than panicking, and deliberately: a panic here would be
    /// indistinguishable from any other bug in the surrounding arithmetic, and the test for
    /// "refused, not truncated" was measured passing for exactly that wrong reason before this
    /// signature was introduced.
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

/// Build the signing revision: source bytes verbatim, then one appended revision carrying a
/// signature dictionary, the widget that makes it a signature to a viewer, and a catalog
/// advertising `/SigFlags`.
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

    // The real signing shape: the widget has to be reachable from the page, and the catalog has
    // to advertise `/SigFlags`. A signature dictionary no page points at is not a signature to
    // any viewer, and a test that only checked the dictionary would pass while shipping that.
    incremental
        .opt_clone_object_to_new_document(page_id)
        .expect("clone the page into the new revision");
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

    // Restrict every search to the appended region: the cloned page also carries a `/Contents`
    // key, and asserting uniqueness is what turns "we searched the right place" into a checked
    // property rather than a hope.
    let appended_start = source_bytes.len();
    let appended = &bytes[appended_start..];

    let contents_marker = b"/Contents<";
    let contents_hits: Vec<usize> = appended
        .windows(contents_marker.len())
        .enumerate()
        .filter(|(_, window)| *window == contents_marker)
        .map(|(index, _)| index)
        .collect();
    assert_eq!(
        contents_hits.len(),
        1,
        "the appended revision must carry exactly one hex /Contents; found {}",
        contents_hits.len()
    );
    let contents_open = appended_start + contents_hits[0] + contents_marker.len();
    let contents_close = contents_open + 2 * CONTENTS_BYTES;
    assert_eq!(bytes[contents_open - 1], b'<', "the gap starts after '<'");
    assert_eq!(
        bytes[contents_close], b'>',
        "the gap holds exactly {CONTENTS_BYTES} bytes"
    );

    // `/ByteRange[` — an array needs no separator after a name, so there is no space here either.
    let range_marker = b"/ByteRange[";
    let range_hits: Vec<usize> = appended
        .windows(range_marker.len())
        .enumerate()
        .filter(|(_, window)| *window == range_marker)
        .map(|(index, _)| index)
        .collect();
    assert_eq!(
        range_hits.len(),
        1,
        "the appended revision must carry exactly one /ByteRange; found {}",
        range_hits.len()
    );
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

fn digest_of_ranges(bytes: &[u8], range: [i64; 4]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(&bytes[range[0] as usize..(range[0] + range[1]) as usize]);
    hasher.update(&bytes[range[2] as usize..(range[2] + range[3]) as usize]);
    hasher.finalize().into()
}

/// The independent oracle I-SG1 asks for: hash everything the gap does not occupy, derived from
/// the gap's own offsets rather than from the four `/ByteRange` integers.
///
/// Without this, the read-back test compares the declared range against a range built by the same
/// convention, so a convention that excluded the wrong bracket would agree with itself and pass.
fn digest_outside_the_gap(bytes: &[u8], contents_open: usize, contents_close: usize) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    // `contents_open` points at the first hex digit, so the `<` is one before it; `contents_close`
    // points at the `>`, which the convention excludes along with the digits.
    hasher.update(&bytes[..contents_open - 1]);
    hasher.update(&bytes[contents_close + 1..]);
    hasher.finalize().into()
}

#[test]
fn the_contents_gap_is_sized_before_the_signature_exists() {
    // The circularity D-SG2 has to break: the signature covers the bytes, but the bytes contain
    // the signature. A fixed-width reserved gap is the only way out, so its width must be
    // reachable and exact.
    let source = source_on_disk("gap");
    let revision = build_signing_revision(&source);

    assert_eq!(
        revision.contents_close - revision.contents_open,
        2 * CONTENTS_BYTES,
        "a hex string of {CONTENTS_BYTES} bytes occupies exactly {} characters",
        2 * CONTENTS_BYTES
    );
    assert!(
        revision.bytes[revision.contents_open..revision.contents_close]
            .iter()
            .all(|byte| *byte == b'0'),
        "the reserved gap must be a placeholder that is obviously not a signature"
    );
    assert!(
        revision.bytes.starts_with(&source),
        "the signing revision must not disturb the source bytes it is appended to"
    );
}

#[test]
fn patching_the_byte_range_does_not_change_the_file_length() {
    // The fact that shaped the mechanism: writing the range through the object model strips
    // leading zeros, so the digits have to be overwritten as raw bytes. If the length changed,
    // every offset measured before the patch would describe a different file.
    let source = source_on_disk("range-width");
    let mut revision = build_signing_revision(&source);
    let before_len = revision.bytes.len();
    let before = revision.bytes.clone();

    let range = revision.byte_range();
    revision.patch_byte_range([range[1], range[2], range[3]]);

    assert_eq!(
        revision.bytes.len(),
        before_len,
        "patching the reserved range must not resize the file"
    );

    let changed: Vec<usize> = before
        .iter()
        .zip(revision.bytes.iter())
        .enumerate()
        .filter(|(_, (old, new))| old != new)
        .map(|(index, _)| index)
        .collect();
    assert!(!changed.is_empty(), "the patch changed nothing");
    for index in changed {
        assert!(
            revision
                .field_offsets
                .iter()
                .any(|offset| index >= *offset && index < offset + BYTE_RANGE_FIELD_WIDTH),
            "byte {index} changed outside the reserved range fields {:?}",
            revision.field_offsets
        );
    }
}

#[test]
fn zero_padded_byte_range_fields_read_back_as_the_values_they_denote() {
    // Leading zeros are legal in a PDF integer, but "legal" and "parsed by the library we ship"
    // are different claims. The library is asked, and the values are then checked against the
    // file they claim to describe.
    let source = source_on_disk("padding");
    let mut revision = build_signing_revision(&source);
    let range = revision.byte_range();
    revision.patch_byte_range([range[1], range[2], range[3]]);

    let reopened = Document::load_mem(&revision.bytes).expect("the signed revision must parse");
    let signature = reopened
        .objects
        .values()
        .find_map(|object| {
            let dict = object.as_dict().ok()?;
            (dict.get(b"Type").and_then(Object::as_name).ok() == Some(b"Sig".as_slice()))
                .then_some(dict)
        })
        .expect("the signature dictionary must be findable");

    let values: Vec<i64> = signature
        .get(b"ByteRange")
        .and_then(Object::as_array)
        .expect("the signature must declare /ByteRange")
        .iter()
        .map(|value| {
            value
                .as_i64()
                .expect("a byte range member must be an integer")
        })
        .collect();
    assert_eq!(
        values,
        range.to_vec(),
        "the zero-padded fields must read back as exactly the values they were meant to denote"
    );

    assert_eq!(
        values[0], 0,
        "range one starts at the beginning of the file"
    );
    assert_eq!(
        values[0] + values[1],
        (revision.contents_open - 1) as i64,
        "range one must end at the '<'"
    );
    assert_eq!(
        values[2],
        (revision.contents_close + 1) as i64,
        "range two must start after the '>'"
    );
    assert_eq!(
        values[2] + values[3],
        revision.bytes.len() as i64,
        "range two must reach the end of the file"
    );
}

#[test]
fn a_signature_larger_than_the_reserved_gap_is_refused_not_truncated() {
    // D-SG2 sizes the placeholder before writing, so the failure mode to design against is a
    // blob that outgrows it. Truncating would produce a file whose /Contents is not the
    // signature the signer computed: the document would look signed and verify as modified,
    // which is the worst of both. Refusing is the only honest outcome.
    let source = source_on_disk("overrun");
    let mut revision = build_signing_revision(&source);
    let before = revision.bytes.clone();

    let oversized = vec![0x5A; CONTENTS_BYTES + 1];
    let outcome = revision.patch_contents(&oversized);

    // Matched on the message, not merely on "an error happened". The first version of this test
    // caught a panic and passed -- and then passed again with the refusal deleted, because the
    // arithmetic underflowed and panicked instead. An error is only evidence of *this* refusal if
    // it says so.
    let message = outcome.expect_err("a signature larger than the gap must be refused");
    assert!(
        message.contains("does not fit"),
        "the refusal must name the overrun, not report some other failure: {message}"
    );
    assert_eq!(
        revision.bytes, before,
        "a refused signature must leave the revision untouched, not partially written"
    );
}

#[test]
fn the_declared_byte_range_covers_exactly_what_an_independent_oracle_hashes() {
    // I-SG1's shape, applied to the substrate: the range a verifier will use must agree with a
    // digest taken over "everything the gap does not occupy", where that second computation never
    // reads `/ByteRange` at all. A convention that excluded the wrong bracket would agree with
    // itself on the read-back test above and disagree here, which is the point of asking twice.
    let source = source_on_disk("oracle");
    let mut revision = build_signing_revision(&source);
    let range = revision.byte_range();
    revision.patch_byte_range([range[1], range[2], range[3]]);

    let payload: Vec<u8> = (0..2048u32).map(|n| (n % 97) as u8).collect();
    revision.patch_contents(&payload).expect("the payload fits");

    assert_eq!(
        digest_of_ranges(&revision.bytes, range),
        digest_outside_the_gap(
            &revision.bytes,
            revision.contents_open,
            revision.contents_close
        ),
        "the two computations must agree: one from the declared /ByteRange, the other from the \
         gap the writer actually left. If they disagree, one of them is off by a bracket"
    );
}

#[test]
fn the_independent_oracle_notices_a_one_byte_shift_in_the_covered_region() {
    // The oracle above is only worth its lines if it can fail. This perturbs a covered byte and
    // asserts both computations move together -- i.e. neither is insensitive to the bytes it
    // claims to cover. Without this, an oracle that hashed nothing would "agree" perfectly.
    let source = source_on_disk("oracle-negative");
    let mut revision = build_signing_revision(&source);
    let range = revision.byte_range();
    revision.patch_byte_range([range[1], range[2], range[3]]);
    revision
        .patch_contents(b"payload")
        .expect("the payload fits");

    let before_declared = digest_of_ranges(&revision.bytes, range);
    let before_oracle = digest_outside_the_gap(
        &revision.bytes,
        revision.contents_open,
        revision.contents_close,
    );

    // One byte early in the source, which lies inside range one for any legitimate convention.
    revision.bytes[10] ^= 0xff;

    let after_declared = digest_of_ranges(&revision.bytes, range);
    let after_oracle = digest_outside_the_gap(
        &revision.bytes,
        revision.contents_open,
        revision.contents_close,
    );
    assert_ne!(
        before_declared, after_declared,
        "the declared-range digest must be sensitive to a covered byte"
    );
    assert_ne!(
        before_oracle, after_oracle,
        "the independent digest must be sensitive to a covered byte"
    );
    assert_eq!(
        after_declared, after_oracle,
        "both computations must still agree after the perturbation"
    );
}

#[test]
fn the_independent_oracle_ignores_a_change_inside_the_gap() {
    // The complement: bytes the signature will occupy are exactly the ones the digest must not
    // cover. If the oracle hashed the gap too, a real signer could never produce a matching
    // digest, and the failure would look like "every signature is modified".
    let source = source_on_disk("oracle-gap");
    let mut revision = build_signing_revision(&source);
    let range = revision.byte_range();
    revision.patch_byte_range([range[1], range[2], range[3]]);

    let before = digest_outside_the_gap(
        &revision.bytes,
        revision.contents_open,
        revision.contents_close,
    );
    revision
        .patch_contents(&vec![0xAB; 3000])
        .expect("the payload fits");
    let after = digest_outside_the_gap(
        &revision.bytes,
        revision.contents_open,
        revision.contents_close,
    );

    assert_eq!(
        before, after,
        "a change inside the reserved gap must not move a digest that excludes it; the signature \
         is written there after the digest exists"
    );
    assert_eq!(
        digest_of_ranges(&revision.bytes, range),
        after,
        "the declared range must also exclude the gap"
    );
}

#[test]
fn patching_the_gap_changes_nothing_the_byte_range_covers() {
    // This is the assertion the whole of signing rests on. A verifier recomputes the digest over
    // the two ranges; if writing the signature into the gap altered a single covered byte, the
    // recomputed digest would not match the one the signer signed, and every signed document
    // would report `modified`.
    let source = source_on_disk("patch");
    let mut revision = build_signing_revision(&source);
    let range = revision.byte_range();
    revision.patch_byte_range([range[1], range[2], range[3]]);

    let covered_before = digest_of_ranges(&revision.bytes, range);
    let before = revision.bytes.clone();

    // A payload whose length is not a round number, so a bug that round-tripped through a
    // fixed-size buffer would show up as a shift rather than as a matching digest.
    let payload: Vec<u8> = (0..4099u32).map(|n| (n % 251) as u8).collect();
    revision.patch_contents(&payload).expect("the payload fits");

    assert_eq!(
        revision.bytes.len(),
        before.len(),
        "patching must not change the file length; a verifier re-reads offsets from the file"
    );
    assert_eq!(
        digest_of_ranges(&revision.bytes, range),
        covered_before,
        "the covered bytes must be untouched by the signature being written into the gap"
    );

    let mut changed = 0;
    for (index, (old, new)) in before.iter().zip(revision.bytes.iter()).enumerate() {
        if old != new {
            changed += 1;
            assert!(
                index >= revision.contents_open && index < revision.contents_close,
                "byte {index} changed but lies outside the gap {}..{}; a /ByteRange computed \
                 before this write would no longer describe the file",
                revision.contents_open,
                revision.contents_close
            );
        }
    }
    assert!(
        changed > 0,
        "nothing changed, so this test proved nothing about the patch"
    );
}

#[test]
fn the_patched_revision_still_opens_and_resolves_its_signature() {
    // D-SG2's output has to be a usable PDF, not merely a byte range that verifies. A revision
    // whose annotation is unreachable from the page is not a signature to any viewer.
    let source = source_on_disk("reopen");
    let mut revision = build_signing_revision(&source);
    let range = revision.byte_range();
    revision.patch_byte_range([range[1], range[2], range[3]]);
    revision
        .patch_contents(b"not a real signature")
        .expect("the payload fits");

    let reopened = Document::load_mem(&revision.bytes).expect("the patched revision must parse");
    assert_eq!(
        reopened.get_pages().len(),
        1,
        "the page must survive the append"
    );

    let page_id = *reopened.get_pages().values().next().expect("one page");
    let annots = reopened
        .get_object(page_id)
        .and_then(Object::as_dict)
        .and_then(|page| page.get(b"Annots"))
        .and_then(Object::as_array)
        .expect("the widget must still be reachable from the page");
    assert_eq!(annots.len(), 1);

    let annot = reopened
        .get_object(annots[0].as_reference().expect("an indirect reference"))
        .and_then(Object::as_dict)
        .expect("the annotation must resolve");
    let signature = annot
        .get(b"V")
        .and_then(Object::as_reference)
        .and_then(|id| reopened.get_object(id))
        .and_then(Object::as_dict)
        .expect("the signature dictionary must resolve through the annotation");
    let contents = signature
        .get(b"Contents")
        .and_then(Object::as_str)
        .expect("/Contents must read back as a string");
    assert_eq!(
        contents.len(),
        CONTENTS_BYTES,
        "the reserved width must survive the round trip, or the next patch would shift the file"
    );
    assert!(
        contents.starts_with(b"not a real signature"),
        "the patched bytes must be what reads back"
    );
}
