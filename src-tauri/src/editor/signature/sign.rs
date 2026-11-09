//! Sign a document through the real save path (P3.2 SCOPE-C, design D-SG2 / D-SG2a).
//!
//! The shape of this module is the invariant:
//!
//! 1. read the source bytes and refuse anything that is not a plain, unencrypted PDF;
//! 2. **append** a signing revision with `lopdf::IncrementalDocument`, so the source bytes become a
//!    byte-identical prefix of the output (I-SG2). PDFium cannot write incrementally (K17b), so
//!    this is the only writer that can produce a signable file at all;
//! 3. locate the reserved `/Contents` gap and the three `/ByteRange` fields in the appended bytes;
//! 4. hand them to [`seal_revision`], which owns the ordering and the refusal to truncate;
//! 5. write to a **new** path with the D2 atomic temp-and-rename discipline;
//! 6. reopen **from disk** and verify the signature there.
//!
//! Step 5 is the D-SG2a exception, written out: the source file is never modified and the output is
//! a new path, exactly as every other save behaves. The only reason the output begins with the
//! source's bytes is that a signature over anything else would be a signature over a different
//! document.
//!
//! Step 6 reopens from disk rather than trusting the bytes in hand, because the failure worth
//! catching is a *file* that does not verify, and an in-memory check cannot see that.

use std::path::{Path, PathBuf};

use lopdf::{dictionary, Document, IncrementalDocument, Object, StringFormat};

use crate::editor::save::write_atomically;
use crate::editor::signature::cms::{
    seal_revision, ReservedGap, BYTE_RANGE_FIELD_WIDTH, CONTENTS_BYTES,
};
use crate::editor::signature::pfx::ImportedIdentity;
use crate::model::AppError;

/// The digit string reserved in each `/ByteRange` field before the real value is known.
///
/// Ten digits because [`BYTE_RANGE_FIELD_WIDTH`] is ten: the field's width cannot change after
/// serialisation, so a value that does not fit is refused rather than written wider. A one-gigabyte
/// document is `1_073_741_824`, still ten digits, so the limit is out of reach for this product and
/// the refusal exists as a guard rather than an everyday path.
const BYTE_RANGE_PLACEHOLDER: i64 = 9_999_999_999;

/// A signed copy: the new path, plus what was signed and where it went.
pub struct SignOutcome {
    pub path: String,
    /// The four integers actually written into the file's `/ByteRange`.
    pub byte_range: [i64; 4],
    /// SHA-256 of the bytes the signature covers, for the caller to display or compare.
    pub content_digest: [u8; 32],
}

/// `"{stem} (signed).pdf"`, then `(signed 2)`, … — a name alongside the original, never over it.
pub fn signed_path(original: &Path, exists: impl Fn(&Path) -> bool) -> Result<PathBuf, AppError> {
    let parent = original
        .parent()
        .ok_or_else(|| AppError::Other("document has no parent directory".to_string()))?;
    let stem = original
        .file_stem()
        .and_then(|stem| stem.to_str())
        .ok_or_else(|| AppError::Other("document name is not valid utf-8".to_string()))?;
    let extension = original
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or("pdf");

    let first = parent.join(format!("{stem} (signed).{extension}"));
    if !exists(&first) {
        return Ok(first);
    }
    for number in 2..=9_999_u32 {
        let candidate = parent.join(format!("{stem} (signed {number}).{extension}"));
        if !exists(&candidate) {
            return Ok(candidate);
        }
    }
    Err(AppError::Other(
        "reached the limit of signed copies for this document".to_string(),
    ))
}

/// Sign `source` into a new sibling file.
///
/// A certify signature (DocMDP) is deliberately **not** produced here: design D-SG6 makes the
/// permission level an explicit choice shown before signing, so a caller that has not asked for a
/// level must not get one by default.
pub fn sign_save(source: &Path, identity: &ImportedIdentity) -> Result<SignOutcome, AppError> {
    if crate::editor::protect::is_encrypted(source)? {
        // Signing an encrypted document needs its password and produces a file whose encryption
        // must be re-established around the appended revision. Neither is in this unit, and
        // appending to an encrypted file without the key writes cleartext objects under an
        // `/Encrypt` that still claims otherwise.
        return Err(AppError::Other(
            "this document is password-protected; signing it is not supported yet".to_string(),
        ));
    }

    let source_bytes = std::fs::read(source)
        .map_err(|error| AppError::Other(format!("could not read PDF: {error}")))?;

    let revision = build_signing_revision(&source_bytes)?;

    let gap = locate_reserved_gap(&revision.bytes, source_bytes.len())?;
    let sealed = seal_revision(&revision.bytes, gap, identity)?;

    let target = signed_path(source, |candidate| candidate.exists())?;

    write_then_verify(&target, &sealed.bytes, |written| {
        verify_signed(written, &source_bytes, sealed.byte_range)
    })?;

    Ok(SignOutcome {
        path: target.to_string_lossy().to_string(),
        byte_range: sealed.byte_range,
        content_digest: sealed.content_digest,
    })
}

/// The appended signing revision: a `/Sig` dictionary, a widget annotating it, a page that carries
/// the widget and a catalog that carries the `/AcroForm`.
///
/// Every object is added to the **new** document half of the incremental pair and every object it
/// references is cloned into that half first, because a revision may not reference revision 1's
/// object numbers as if they were rewritten — it must copy them forward (`/Prev` still points at
/// the original xref). Getting this wrong produces a file that opens and shows a blank page.
pub(crate) struct SigningRevision {
    pub bytes: Vec<u8>,
}

fn build_signing_revision(source_bytes: &[u8]) -> Result<SigningRevision, AppError> {
    let previous = Document::load_mem(source_bytes)
        .map_err(|error| AppError::Other(format!("could not open PDF: {error}")))?;

    let page_id = *previous
        .get_pages()
        .values()
        .next()
        .ok_or_else(|| AppError::Other("this document has no pages to sign".to_string()))?;
    let catalog = previous
        .trailer
        .get(b"Root")
        .and_then(Object::as_reference)
        .map_err(|_| AppError::Other("this document has no catalog".to_string()))?;

    let mut incremental = IncrementalDocument::create_from(source_bytes.to_vec(), previous);

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
        // A zero-size rectangle is the standard shape for an invisible signature. A visible
        // appearance stream is a separate feature and inventing one here would put a mark on the
        // user's page that they did not place.
        "Rect" => vec![0.into(), 0.into(), 0.into(), 0.into()],
        "V" => Object::Reference(signature_id),
    });

    incremental
        .opt_clone_object_to_new_document(page_id)
        .map_err(|error| AppError::Other(format!("could not copy the page: {error}")))?;
    incremental
        .new_document
        .get_object_mut(page_id)
        .and_then(Object::as_dict_mut)
        .map_err(|_| AppError::Other("the page is not a dictionary".to_string()))?
        .set("Annots", Object::Array(vec![Object::Reference(annot_id)]));

    incremental
        .opt_clone_object_to_new_document(catalog)
        .map_err(|error| AppError::Other(format!("could not copy the catalog: {error}")))?;
    incremental
        .new_document
        .get_object_mut(catalog)
        .and_then(Object::as_dict_mut)
        .map_err(|_| AppError::Other("the catalog is not a dictionary".to_string()))?
        // `SigFlags 3` says "this document contains a signature and the fields are append-only",
        // which is what tells a reader that a later revision is an amendment rather than damage.
        .set(
            "AcroForm",
            dictionary! {
                "SigFlags" => 3,
                "Fields" => vec![Object::Reference(annot_id)],
            },
        );

    let mut bytes = Vec::new();
    incremental.save_to(&mut bytes).map_err(|error| {
        AppError::Other(format!("could not append the signing revision: {error}"))
    })?;

    Ok(SigningRevision { bytes })
}

/// Find the reserved regions **in the appended bytes only**.
///
/// Searching the whole file would find an earlier signature's gap first, and signing the wrong gap
/// produces a file whose `/ByteRange` verifies against nothing. The search is therefore anchored at
/// `source_len` and every marker must occur exactly once after it; two occurrences mean the
/// revision is not the shape this module built, so it is refused rather than guessed at.
pub(crate) fn locate_reserved_gap(
    bytes: &[u8],
    source_len: usize,
) -> Result<ReservedGap, AppError> {
    if source_len == 0 || source_len >= bytes.len() {
        return Err(AppError::SignatureBuildFailed(
            "the appended revision is empty, so there is nothing to sign".into(),
        ));
    }
    let appended = &bytes[source_len..];

    let contents_open = unique_marker(appended, b"/Contents<")?
        .checked_add(source_len)
        .and_then(|at| at.checked_add(b"/Contents<".len()))
        .ok_or_else(|| AppError::SignatureBuildFailed("the /Contents offset overflowed".into()))?;
    let contents_close = contents_open + 2 * CONTENTS_BYTES;
    if contents_close > bytes.len() {
        return Err(AppError::SignatureBuildFailed(
            "the reserved /Contents gap runs past the end of the file".into(),
        ));
    }

    let range_start = unique_marker(appended, b"/ByteRange[")? + source_len + b"/ByteRange[".len();
    let placeholder = format!("{BYTE_RANGE_PLACEHOLDER}");
    let mut field_offsets = [0usize; 3];
    let mut cursor = range_start;
    for (found, slot) in field_offsets.iter_mut().enumerate() {
        let hit = bytes[cursor..]
            .windows(placeholder.len())
            .position(|window| window == placeholder.as_bytes())
            .ok_or_else(|| {
                AppError::SignatureBuildFailed(format!(
                    "reserved byte-range field {found} is missing from the appended revision"
                ))
            })?;
        *slot = cursor + hit;
        cursor = *slot + BYTE_RANGE_FIELD_WIDTH;
    }

    Ok(ReservedGap {
        contents_open,
        contents_close,
        field_offsets,
    })
}

/// Offset of the single occurrence of `marker` in `haystack`, or a refusal.
fn unique_marker(haystack: &[u8], marker: &[u8]) -> Result<usize, AppError> {
    let mut hits = haystack
        .windows(marker.len())
        .enumerate()
        .filter(|(_, window)| *window == marker)
        .map(|(index, _)| index);
    let first = hits.next().ok_or_else(|| {
        AppError::SignatureBuildFailed(format!(
            "the appended revision has no `{}`",
            String::from_utf8_lossy(marker)
        ))
    })?;
    if let Some(second) = hits.next() {
        return Err(AppError::SignatureBuildFailed(format!(
            "the appended revision has `{}` at {first} and {second}; the signing revision is \
             malformed and signing either would be a guess",
            String::from_utf8_lossy(marker)
        )));
    }
    Ok(first)
}

/// Write `bytes` to `target`, verify it, and remove it again if verification fails.
///
/// The removal is not tidiness: a signed copy that does not verify looks authoritative and would be
/// the one the user emails someone. Leaving it behind after reporting an error is the worst of both
/// outcomes, so the two steps are bound together here rather than left to each caller to remember.
fn write_then_verify(
    target: &Path,
    bytes: &[u8],
    verify: impl FnOnce(&Path) -> Result<(), AppError>,
) -> Result<(), AppError> {
    write_atomically(target, bytes)?;
    if let Err(error) = verify(target) {
        let _ = std::fs::remove_file(target);
        return Err(error);
    }
    Ok(())
}

/// Reopen the written file and check it is what it claims to be.
///
/// This is the write-side half of I-SG2 and D-SG2a, checked **on disk**:
///
/// * the source bytes are a byte-identical prefix (the signature means nothing otherwise);
/// * the `/ByteRange` in the file is the range that was sealed, and it excludes only the gap;
/// * every byte outside the declared ranges is exactly the `<` and `>` delimiters, so no
///   unaccounted region slipped into the file;
/// * the whole file has the length the ranges imply.
///
/// The cryptographic check is not duplicated here: `cms.rs`'s own verifier and the independent DER
/// walk in the integration tests cover it, and a second implementation living in production would
/// be a third thing to keep correct.
pub(crate) fn verify_signed(
    path: &Path,
    source_bytes: &[u8],
    expected_range: [i64; 4],
) -> Result<(), AppError> {
    let written = std::fs::read(path)
        .map_err(|error| AppError::Other(format!("could not re-read PDF: {error}")))?;

    if written.len() <= source_bytes.len() {
        return Err(AppError::Other(
            "the signed file is not longer than the source, so no revision was appended"
                .to_string(),
        ));
    }
    if &written[..source_bytes.len()] != source_bytes {
        return Err(AppError::Other(
            "the signed file does not begin with the source's bytes, so the signature would cover \
             a different document"
                .to_string(),
        ));
    }

    let observed = read_byte_range(&written, source_bytes.len())?;
    if observed != expected_range {
        return Err(AppError::Other(format!(
            "the /ByteRange written into the file ({observed:?}) is not the range that was signed \
             ({expected_range:?})"
        )));
    }

    let [start, first, second_start, second_len] = observed;
    let (start, first, second_start, second_len) = (
        start as usize,
        first as usize,
        second_start as usize,
        second_len as usize,
    );

    let excluded_start = start + first;
    let excluded_end = second_start;
    if second_start + second_len != written.len() {
        return Err(AppError::Other(
            "the declared range does not reach the end of the file, so bytes are unsigned"
                .to_string(),
        ));
    }
    if excluded_end <= excluded_start || excluded_end - excluded_start < 2 {
        return Err(AppError::Other("the excluded gap is too small".to_string()));
    }
    if written[excluded_start] != b'<' || written[excluded_end - 1] != b'>' {
        return Err(AppError::Other(
            "the excluded region is not the <…> contents gap".to_string(),
        ));
    }

    Ok(())
}

/// Parse the four integers of the `/ByteRange` in the appended revision.
///
/// Searched from `from` rather than from byte zero: a document that already carries a signature has
/// an earlier `/ByteRange`, and reading that one would compare this revision's range against the
/// previous signer's.
fn read_byte_range(bytes: &[u8], from: usize) -> Result<[i64; 4], AppError> {
    let marker = b"/ByteRange[";
    let at = bytes
        .get(from..)
        .unwrap_or_default()
        .windows(marker.len())
        .position(|window| window == marker)
        .ok_or_else(|| AppError::Other("the signed file has no /ByteRange".to_string()))?
        + from
        + marker.len();
    let tail = std::str::from_utf8(&bytes[at..(at + 96).min(bytes.len())])
        .map_err(|_| AppError::Other("/ByteRange is not ASCII".to_string()))?;
    let end = tail
        .find(']')
        .ok_or_else(|| AppError::Other("/ByteRange is not closed".to_string()))?;
    let mut values = [0i64; 4];
    let parts: Vec<&str> = tail[..end].split_whitespace().collect();
    if parts.len() != 4 {
        return Err(AppError::Other(format!(
            "/ByteRange has {} integers, expected four",
            parts.len()
        )));
    }
    for (slot, part) in values.iter_mut().zip(parts) {
        *slot = part
            .trim_end_matches(',')
            .parse()
            .map_err(|_| AppError::Other(format!("/ByteRange holds `{part}`, not a number")))?;
    }
    Ok(values)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal well-formed signed-looking file: a "source" prefix, a gap, a trailer.
    ///
    /// The point is the *shape* `verify_signed` inspects, not a real signature, so the bytes can be
    /// synthetic. Testing the guard directly is deliberate: a mutation that disables it cannot be
    /// caught by signing a document, because a correct builder never produces a file the guard
    /// rejects. Leaving it proven only through the happy path is how a guard rots.
    struct Fake {
        /// Held only so the temporary directory outlives the file inside it.
        _dir: tempfile::TempDir,
        source: Vec<u8>,
        path: std::path::PathBuf,
        bytes: Vec<u8>,
        range: [i64; 4],
    }

    fn fake() -> Fake {
        let source = b"%PDF-1.7\nsource revision\n".to_vec();
        let mut bytes = source.clone();

        let gap_start = bytes.len();
        bytes.extend_from_slice(b"<");
        bytes.extend(std::iter::repeat_n(b'0', 32));
        bytes.extend_from_slice(b">");
        let after_gap = bytes.len();

        // Fixed-width fields, exactly as the real builder reserves them: the value is patched in
        // afterwards, so writing the range does not change the offsets it describes.
        const W: usize = 10;
        const PLACEHOLDER: &str = "9999999999";
        let range_start = bytes.len();
        bytes.extend_from_slice(
            format!("/ByteRange[{PLACEHOLDER} {PLACEHOLDER} {PLACEHOLDER} {PLACEHOLDER}]\n")
                .as_bytes(),
        );
        bytes.extend_from_slice(b"trailer\n");

        let range = [
            0i64,
            gap_start as i64,
            after_gap as i64,
            (bytes.len() - after_gap) as i64,
        ];
        // All four slots are patched: the leading zero included, because the placeholder in slot 0
        // is not a zero and a guard that reads the file would see it.
        for (index, value) in range.iter().enumerate() {
            let at = range_start + "/ByteRange[".len() + index * (W + 1);
            bytes[at..at + W].copy_from_slice(format!("{value:0W$}").as_bytes());
        }

        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("signed.pdf");
        std::fs::write(&path, &bytes).expect("write");

        Fake {
            _dir: dir,
            source,
            path,
            bytes,
            range,
        }
    }

    #[test]
    fn a_file_that_fails_verification_is_removed_again() {
        let dir = tempfile::tempdir().expect("temp dir");
        let target = dir.path().join("signed.pdf");

        let error = write_then_verify(&target, b"candidate bytes", |_| {
            Err(AppError::Other("verification failed".to_string()))
        })
        .expect_err("the failure must be reported");

        assert!(error.to_string().contains("verification failed"));
        assert!(
            !target.exists(),
            "an unverified signed copy was left on disk; it looks authoritative and is not"
        );
    }

    #[test]
    fn a_file_that_passes_verification_is_kept() {
        let dir = tempfile::tempdir().expect("temp dir");
        let target = dir.path().join("signed.pdf");

        write_then_verify(&target, b"candidate bytes", |written| {
            assert_eq!(std::fs::read(written).unwrap(), b"candidate bytes");
            Ok(())
        })
        .expect("a passing verification keeps the file");

        assert!(target.exists(), "the verified copy must survive");
    }

    #[test]
    fn a_correctly_shaped_file_passes() {
        let fake = fake();
        verify_signed(&fake.path, &fake.source, fake.range)
            .expect("the hand-built file is exactly the shape the guard accepts");
    }

    #[test]
    fn a_file_that_does_not_start_with_the_source_is_refused() {
        let fake = fake();
        let mut altered = fake.bytes.clone();
        altered[0] = b'X';
        std::fs::write(&fake.path, &altered).expect("write");

        let error = verify_signed(&fake.path, &fake.source, fake.range)
            .expect_err("a file whose prefix differs must be refused");
        assert!(
            error.to_string().contains("does not begin with the source"),
            "unexpected refusal: {error}"
        );
    }

    #[test]
    fn a_declared_range_that_differs_from_the_written_one_is_refused() {
        let fake = fake();
        let mut wrong = fake.range;
        wrong[1] -= 1;

        let error = verify_signed(&fake.path, &fake.source, wrong)
            .expect_err("the guard must compare against the range that was signed");
        assert!(
            error
                .to_string()
                .contains("is not the range that was signed"),
            "unexpected refusal: {error}"
        );
    }

    #[test]
    fn a_range_that_stops_short_of_the_end_is_refused() {
        let fake = fake();
        // The short value must be in the *file* as well as in `expected`, or the range comparison
        // fires first and this test would be asserting the wrong guard.
        let mut short = fake.range;
        short[3] -= 1;
        let mut bytes = fake.bytes.clone();
        let field = find_nth_field(&bytes, 3);
        bytes[field..field + 10].copy_from_slice(format!("{:010}", short[3]).as_bytes());
        std::fs::write(&fake.path, &bytes).expect("write");

        let error = verify_signed(&fake.path, &fake.source, short)
            .expect_err("unsigned trailing bytes must be refused");
        assert!(
            error.to_string().contains("does not reach the end"),
            "unexpected refusal: {error}"
        );
    }

    /// Offset of the `n`th ten-digit field of the file's `/ByteRange`.
    fn find_nth_field(bytes: &[u8], n: usize) -> usize {
        let marker = b"/ByteRange[";
        let start = bytes
            .windows(marker.len())
            .position(|window| window == marker)
            .expect("a /ByteRange")
            + marker.len();
        start + n * 11
    }

    #[test]
    fn an_excluded_region_without_delimiters_is_refused() {
        let fake = fake();
        let mut bytes = fake.bytes.clone();
        let open = (fake.range[1]) as usize;
        bytes[open] = b' ';
        std::fs::write(&fake.path, &bytes).expect("write");

        let error = verify_signed(&fake.path, &fake.source, fake.range)
            .expect_err("a gap that is not <…> must be refused");
        assert!(
            error.to_string().contains("not the"),
            "unexpected refusal: {error}"
        );
    }

    #[test]
    fn a_file_no_longer_than_the_source_is_refused() {
        let fake = fake();
        std::fs::write(&fake.path, &fake.source).expect("write");

        let error = verify_signed(&fake.path, &fake.source, fake.range)
            .expect_err("a file with no appended revision must be refused");
        assert!(
            error.to_string().contains("no revision was appended"),
            "unexpected refusal: {error}"
        );
    }

    #[test]
    fn the_gap_locator_refuses_a_revision_with_two_contents_markers() {
        // Two `/Contents<` in the appended region means the revision is not the shape this module
        // builds, so signing either one would be a guess.
        let mut bytes = b"prefix".to_vec();
        bytes.extend_from_slice(b"/Contents<aaaa/Contents<bbbb");
        let error = locate_reserved_gap(&bytes, b"prefix".len())
            .expect_err("an ambiguous gap must be refused");
        assert!(
            error.to_string().contains("malformed"),
            "unexpected refusal: {error}"
        );
    }

    #[test]
    fn the_gap_locator_refuses_an_empty_revision() {
        let bytes = b"only the source".to_vec();
        let error = locate_reserved_gap(&bytes, bytes.len())
            .expect_err("an empty appended region must be refused");
        assert!(
            error.to_string().contains("nothing to sign"),
            "unexpected refusal: {error}"
        );
    }
}
