//! Deciding whether a signed PDF still says what its signer signed (P3.2, I-SG1 / I-SG4).
//!
//! **This is the production half the badge was missing.** Until this module existed, the only
//! verifier in the tree lived in `tests/support/cms_verifier.rs`, which meant a `valid` badge had
//! no way to reach `Integrity::Intact` and D-SG7's four states were three states and a promise.
//! Nothing here is new cryptography: `rsa`, `sha2`, `cms`, `der` and `x509-cert` were already direct
//! dependencies of the signer, so verification adds no crate and no compiled C.
//!
//! **No new crates, and that is a deliberate constraint.** A verifier that needed a different
//! crypto stack from the signer would be a second dependency surface to audit, on the one code
//! path where a mistake is invisible to the user. Reusing the signer's stack means one set of
//! versions to keep current.
//!
//! What this module proves, and what it refuses to claim:
//!
//! * **It proves** that the bytes in `/ByteRange` hash to the `messageDigest` inside the signature,
//!   and that the RSA signature covers those attributes. A changed byte anywhere in the covered
//!   range therefore changes the answer -- which is exactly the case I-SG4 asks for.
//! * **It does not claim** the certificate is trustworthy. That is `anchored`, which D-SG4 defines
//!   as local configuration only, and which this module is handed rather than decides.
//! * **It does not claim** the signature is the only revision. A valid signature followed by an
//!   appended revision is reported as such; the badge maps that to `modified` (D-SG7).
//!
//! The two limits are structural rather than aspirational: `Integrity` has no variant that means
//! "trusted", and [`inspect`] takes `anchored` as an argument.
//!
//! ## Why the DER walk is hand-written
//!
//! `cms 0.2.3` would decode most of this, but its `SignerInfo.signed_attrs` is an `Attributes` newtype
//! that does not retain the bytes of the `[0]` wrapper, and RFC 5652 §5.4 requires the *re-tagged
//! `SET OF`* encoding to be what the key verifies. Re-encoding a decoded value would compare against
//! a byte string we produced rather than the one the signer signed, and a re-encode that differed
//! by one length byte would fail on a good signature or -- worse -- succeed on a rewritten one.
//! Walking the original bytes keeps the verifier's input and the signer's output the same bytes.

use lopdf::{Document, Object};
use rsa::pkcs1v15::{Signature, VerifyingKey};
use rsa::pkcs8::DecodePublicKey;
use rsa::signature::Verifier;
use rsa::RsaPublicKey;
use sha2::{Digest, Sha256};
use x509_cert_cms::der::Decode;

use crate::model::AppError;

use super::badge::Integrity;

/// The DER tag numbers this walk needs to tell apart.
const TAG_OCTET_STRING: u8 = 0x04;
const TAG_OID: u8 = 0x06;
const TAG_SEQUENCE: u8 = 0x30;
const TAG_SET: u8 = 0x31;
/// `[0]` context-specific, constructed: the `SignedData` wrapper and, later, `signedAttrs`.
const TAG_CONTEXT_0: u8 = 0xA0;

/// `1.2.840.113549.1.7.2` -- `id-signedData`.
const OID_SIGNED_DATA: &[u8] = &[0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x07, 0x02];
/// `1.2.840.113549.1.9.4` -- the `messageDigest` signed attribute.
const OID_MESSAGE_DIGEST: &[u8] = &[0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x09, 0x04];
/// `2.16.840.1.101.3.4.2.1` -- SHA-256.
const OID_SHA256: &[u8] = &[0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x01];
/// `1.2.840.113549.1.1.1` -- `rsaEncryption`, the DigestInfo-carrying form the signer writes.
const OID_RSA_ENCRYPTION: &[u8] = &[0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x01, 0x01];
/// `1.2.840.113549.1.1.11` -- `sha256WithRSAEncryption`.
const OID_SHA256_WITH_RSA: &[u8] = &[0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x01, 0x0B];

/// Read one DER element: `(tag, contents, total_bytes_consumed)`.
///
/// Returns `None` for anything malformed rather than panicking: this module parses bytes that came
/// off disk, which may be truncated or hostile. A panic here would be a crash on a corrupt file.
fn der_read(bytes: &[u8], at: usize) -> Option<(u8, &[u8], usize)> {
    let tag = *bytes.get(at)?;
    // High-tag-number form is not used by any type in a signature; refusing it avoids inventing an
    // interpretation for a shape we would not correctly handle.
    if tag & 0x1F == 0x1F {
        return None;
    }
    let first = *bytes.get(at + 1)?;
    if first & 0x80 == 0 {
        let length = first as usize;
        let body = bytes.get(at + 2..at + 2 + length)?;
        return Some((tag, body, 2 + length));
    }
    let count = (first & 0x7F) as usize;
    // Long-form lengths above four bytes cannot describe a document this product can hold, and the
    // cap keeps the arithmetic below in range.
    if count == 0 || count > 4 {
        return None;
    }
    let mut length = 0usize;
    for index in 0..count {
        length = (length << 8) | *bytes.get(at + 2 + index)? as usize;
    }
    let start = at + 2 + count;
    let body = bytes.get(start..start + length)?;
    Some((tag, body, start + length - at))
}

/// Every child element of a constructed element's contents.
fn der_children(contents: &[u8]) -> Vec<(u8, &[u8])> {
    let mut children = Vec::new();
    let mut at = 0usize;
    while at < contents.len() {
        match der_read(contents, at) {
            Some((tag, body, consumed)) if consumed > 0 => {
                children.push((tag, body));
                at += consumed;
            }
            _ => break,
        }
    }
    children
}

/// Whether an OID's contents equal `expected`.
fn oid_is(contents: &[u8], expected: &[u8]) -> bool {
    contents == expected
}

/// The parts of a signed file the verification needs, all recovered from the file itself.
///
/// Every field is derived by searching the bytes rather than by trusting an offset the writer
/// recorded. A helper that reused the writer's offsets would pass while the file said something
/// else -- the same reasoning `tests/support/cms_verifier.rs` gives for its own extraction.
struct Sealed {
    /// The DER blob decoded out of the `/Contents` hex gap.
    cms: Vec<u8>,
    /// The bytes the signature covers, rebuilt from the four `/ByteRange` integers.
    covered: Vec<u8>,
    /// The range as written, kept so a caller can report exactly what was and was not covered.
    byte_range: [i64; 4],
}

/// Recover `(cms_der, covered_bytes, byte_range)` from a signed file's own bytes.
fn recover(written: &[u8]) -> Result<Sealed, AppError> {
    let byte_range = find_byte_range(written)?;
    let [start, first, second_start, second_len] = byte_range;

    let (start, first, second_start, second_len) = (
        usize::try_from(start).map_err(|_| {
            AppError::SignatureBuildFailed("/ByteRange has a negative offset".into())
        })?,
        usize::try_from(first).map_err(|_| {
            AppError::SignatureBuildFailed("/ByteRange has a negative length".into())
        })?,
        usize::try_from(second_start).map_err(|_| {
            AppError::SignatureBuildFailed("/ByteRange has a negative offset".into())
        })?,
        usize::try_from(second_len).map_err(|_| {
            AppError::SignatureBuildFailed("/ByteRange has a negative length".into())
        })?,
    );

    let covered_end = start
        .checked_add(first)
        .ok_or_else(|| AppError::SignatureBuildFailed("/ByteRange overflows".into()))?;
    let tail_end = second_start
        .checked_add(second_len)
        .ok_or_else(|| AppError::SignatureBuildFailed("/ByteRange overflows".into()))?;
    if covered_end > written.len() || tail_end > written.len() || covered_end > second_start {
        return Err(AppError::SignatureBuildFailed(
            "the /ByteRange does not describe this file's bytes".into(),
        ));
    }

    let mut covered = Vec::with_capacity(first + second_len);
    covered.extend_from_slice(&written[start..covered_end]);
    covered.extend_from_slice(&written[second_start..tail_end]);

    // The gap between the two ranges is `<` + hex + `>`; the hex is the CMS blob.
    let gap = &written[covered_end..second_start];
    if gap.first() != Some(&b'<') || gap.last() != Some(&b'>') {
        return Err(AppError::SignatureBuildFailed(
            "the excluded region is not the <\u{2026}> contents gap".into(),
        ));
    }
    let hex_bytes = &gap[1..gap.len() - 1];
    let cms = decode_hex(hex_bytes)?;

    Ok(Sealed {
        cms,
        covered,
        byte_range,
    })
}

/// Decode the `/Contents` hex, stopping at the DER's own length.
///
/// The gap is zero-padded to a reserved size, so the decoded buffer is longer than the signature.
/// Trimming trailing zeros would be wrong -- a valid signature may end in `0x00`, which is the bug
/// that made this suite flaky once already -- so the padding is skipped by reading the DER header
/// and taking exactly the length it declares.
fn decode_hex(hex_bytes: &[u8]) -> Result<Vec<u8>, AppError> {
    let mut decoded = Vec::with_capacity(hex_bytes.len() / 2);
    let mut nibble: Option<u8> = None;
    for byte in hex_bytes {
        let value = match byte {
            b'0'..=b'9' => byte - b'0',
            b'a'..=b'f' => byte - b'a' + 10,
            b'A'..=b'F' => byte - b'A' + 10,
            // Whitespace is legal between hex digits in a PDF string.
            b' ' | b'\n' | b'\r' | b'\t' => continue,
            _ => {
                return Err(AppError::SignatureBuildFailed(
                    "/Contents is not hexadecimal".into(),
                ))
            }
        };
        match nibble.take() {
            Some(high) => decoded.push((high << 4) | value),
            None => nibble = Some(value),
        }
    }
    if decoded.is_empty() {
        return Err(AppError::SignatureBuildFailed("/Contents is empty".into()));
    }

    // Take exactly what the outermost DER element declares. A file whose declared length exceeds
    // the decoded bytes is truncated and must not be treated as a shorter, valid signature.
    let (_, _, consumed) = der_read(&decoded, 0).ok_or_else(|| {
        AppError::SignatureBuildFailed("/Contents does not begin with a DER element".into())
    })?;
    decoded.truncate(consumed);
    Ok(decoded)
}

/// Find the four integers of the `/ByteRange` belonging to the **last** signature.
///
/// Searched from the end: a document that has been signed twice carries two `/ByteRange` arrays, and
/// the badge is about the newest signature. Reading the first one would report on a revision the
/// user did not just add.
fn find_byte_range(written: &[u8]) -> Result<[i64; 4], AppError> {
    const MARKER: &[u8] = b"/ByteRange[";
    let at = written
        .windows(MARKER.len())
        .rposition(|window| window == MARKER)
        .ok_or_else(|| AppError::SignatureBuildFailed("the file has no /ByteRange".into()))?
        + MARKER.len();
    let tail = written
        .get(at..(at + 96).min(written.len()))
        .and_then(|slice| std::str::from_utf8(slice).ok())
        .ok_or_else(|| AppError::SignatureBuildFailed("/ByteRange is not ASCII".into()))?;
    let end = tail
        .find(']')
        .ok_or_else(|| AppError::SignatureBuildFailed("/ByteRange is not closed".into()))?;
    let parts: Vec<&str> = tail[..end].split_whitespace().collect();
    if parts.len() != 4 {
        return Err(AppError::SignatureBuildFailed(format!(
            "/ByteRange has {} integers, expected four",
            parts.len()
        )));
    }
    let mut values = [0i64; 4];
    for (slot, part) in values.iter_mut().zip(parts) {
        *slot = part
            .trim_end_matches(',')
            .parse()
            .map_err(|_| AppError::SignatureBuildFailed(format!("/ByteRange holds `{part}`")))?;
    }
    Ok(values)
}

/// The `messageDigest` and the RSA signature, plus the certificate that should carry the key.
struct Reading {
    message_digest: Vec<u8>,
    signature: Vec<u8>,
    /// `SignedAttributes` contents re-tagged as the `SET OF` DER that was signed.
    signed_attributes: Vec<u8>,
    digest_oid: Vec<u8>,
    signature_oid: Vec<u8>,
    /// The first X.509 certificate carried in the blob.
    certificate: Option<Vec<u8>>,
}

/// Walk a detached CMS `ContentInfo` to the signer's parts.
fn read_cms(der: &[u8]) -> Result<Reading, AppError> {
    let fail =
        |why: &str| AppError::SignatureBuildFailed(format!("the signature is not readable: {why}"));

    let (tag, content_info, _) = der_read(der, 0).ok_or_else(|| fail("truncated ContentInfo"))?;
    if tag != TAG_SEQUENCE {
        return Err(fail("ContentInfo is not a SEQUENCE"));
    }

    let top = der_children(content_info);
    if !top
        .first()
        .is_some_and(|(tag, body)| *tag == TAG_OID && oid_is(body, OID_SIGNED_DATA))
    {
        return Err(fail("the outer contentType is not id-signedData"));
    }
    let (wrapped_tag, wrapped) = top
        .get(1)
        .map(|(tag, body)| (*tag, *body))
        .ok_or_else(|| fail("no [0] SignedData"))?;
    if wrapped_tag != TAG_CONTEXT_0 {
        return Err(fail("SignedData is not [0] explicit"));
    }
    let (sd_tag, sd_contents, _) =
        der_read(wrapped, 0).ok_or_else(|| fail("truncated SignedData"))?;
    if sd_tag != TAG_SEQUENCE {
        return Err(fail("SignedData is not a SEQUENCE"));
    }
    let sd = der_children(sd_contents);

    // version(0), digestAlgorithms(1), encapContentInfo(2), [0] certificates?, [1] crls?, signerInfos
    let encap = sd.get(2).ok_or_else(|| fail("no encapContentInfo"))?.1;
    let encap_fields = der_children(encap);
    if !encap_fields.first().is_some_and(|(tag, body)| {
        *tag == TAG_OID
            && oid_is(
                body,
                &[0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x07, 0x01],
            )
    }) {
        return Err(fail("eContentType is not id-data"));
    }
    if encap_fields.iter().any(|(tag, _)| *tag == TAG_CONTEXT_0) {
        return Err(fail("the signature is not detached: it carries eContent"));
    }

    // The first plain SEQUENCE inside `[0] certificates` is an X.509 certificate.
    let mut certificate = None;
    if let Some((_, body)) = sd.iter().find(|(tag, _)| *tag == TAG_CONTEXT_0) {
        let mut at = 0usize;
        while at < body.len() {
            match der_read(body, at) {
                Some((child_tag, _, consumed)) if consumed > 0 => {
                    if child_tag == TAG_SEQUENCE && certificate.is_none() {
                        certificate = Some(body[at..at + consumed].to_vec());
                    }
                    at += consumed;
                }
                _ => break,
            }
        }
    }

    let signer_infos = sd
        .iter()
        .rev()
        .find(|(tag, _)| *tag == TAG_SET)
        .ok_or_else(|| fail("no signerInfos SET"))?
        .1;
    let (si_tag, si_contents, _) =
        der_read(signer_infos, 0).ok_or_else(|| fail("truncated SignerInfo"))?;
    if si_tag != TAG_SEQUENCE {
        return Err(fail("SignerInfo is not a SEQUENCE"));
    }
    let si = der_children(si_contents);

    let digest_oid = si
        .get(2)
        .and_then(|(_, body)| der_children(body).first().map(|(_, oid)| oid.to_vec()))
        .ok_or_else(|| fail("no digestAlgorithm"))?;

    let signed_attrs = si
        .iter()
        .find(|(tag, _)| *tag == TAG_CONTEXT_0)
        .ok_or_else(|| fail("no signed attributes, so nothing binds the content"))?
        .1;

    if si.len() < 2 {
        return Err(fail("SignerInfo has no signature"));
    }
    let signature_oid = der_children(si[si.len() - 2].1)
        .first()
        .map(|(_, oid)| oid.to_vec())
        .ok_or_else(|| fail("no signatureAlgorithm"))?;
    let signature = si[si.len() - 1].1.to_vec();

    let mut message_digest = None;
    for (tag, body) in der_children(signed_attrs) {
        if tag != TAG_SEQUENCE {
            continue;
        }
        let fields = der_children(body);
        let Some((attr_tag, attr_type)) = fields.first() else {
            continue;
        };
        if *attr_tag != TAG_OID || !oid_is(attr_type, OID_MESSAGE_DIGEST) {
            continue;
        }
        if let Some((_, values)) = fields.get(1) {
            if let Some((TAG_OCTET_STRING, octets, _)) = der_read(values, 0) {
                message_digest = Some(octets.to_vec());
            }
        }
    }
    let message_digest = message_digest
        .ok_or_else(|| fail("no messageDigest attribute, so the signature binds nothing"))?;

    Ok(Reading {
        message_digest,
        signature,
        signed_attributes: retag_as_set(signed_attrs),
        digest_oid,
        signature_oid,
        certificate,
    })
}

/// Re-encode `SignedAttributes` with the `SET OF` tag the signature actually covers.
///
/// RFC 5652 §5.4 signs the DER of the `SET OF`, while the bytes in the message carry the `[0]`
/// implicit tag. Only the tag differs; the length header is rebuilt at the width the original used
/// so the encoding stays byte-identical below the tag.
fn retag_as_set(contents: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(contents.len() + 5);
    out.push(TAG_SET);
    let length = contents.len();
    if length < 0x80 {
        out.push(length as u8);
    } else if length <= 0xFF {
        out.push(0x81);
        out.push(length as u8);
    } else {
        out.push(0x82);
        out.push((length >> 8) as u8);
        out.push((length & 0xFF) as u8);
    }
    out.extend_from_slice(contents);
    out
}

/// Whether the file's catalog still names a `/Perms /DocMDP` entry.
///
/// Used by the badge to report the certification the file *declares*. Parsed with `lopdf` from the
/// bytes rather than read through PDFium: the Windows CI job cannot run PDFium's signature reader,
/// and the Linux gate must be able to prove the same reading.
fn declares_doc_mdp(document: &Document) -> bool {
    let Ok(catalog) = document.catalog() else {
        return false;
    };
    let Ok(perms) = catalog.get(b"Perms") else {
        return false;
    };
    let perms = match perms {
        Object::Reference(id) => match document.get_object(*id) {
            Ok(resolved) => resolved,
            Err(_) => return false,
        },
        other => other,
    };
    perms.as_dict().is_ok_and(|dict| dict.has(b"DocMDP"))
}

/// Verify the newest signature in `written` against the bytes it covers.
///
/// Returns the computed [`Integrity`] and the declared certification level, or `None` when the file
/// carries no signature dictionary at all. The caller combines these with trust and revocation
/// through `badge::decide`.
///
/// `Integrity::Damaged` is the answer for a file that will not parse as well as for a digest
/// mismatch. Both mean the same thing to a reader -- the signature does not attest to these bytes --
/// and the distinction is logged at the call site rather than guessed at here.
pub fn inspect(written: &[u8]) -> Result<Option<SealedReading>, AppError> {
    let document = match Document::load_mem(written) {
        Ok(document) => document,
        Err(error) => {
            // A file whose cross-reference table will not parse is still evidence: if a
            // `/ByteRange` is present, the signature exists but cannot be checked, which is
            // `Damaged` rather than `Unsigned`. Reporting `Unsigned` here would hide the fact that
            // someone signed this document.
            return if written.windows(11).any(|window| window == b"/ByteRange[") {
                Ok(Some(SealedReading {
                    integrity: Integrity::Damaged,
                    revoked_hint: None,
                    certification_level: None,
                    // Knowable from the bytes even though the xref would not parse, and `Damaged`
                    // already outranks it in `decide`, so this is reported for completeness rather
                    // than to change the badge.
                    later_revision: find_byte_range(written)
                        .map(|range| later_revision_after_signing(written, range))
                        .unwrap_or(false),
                }))
            } else {
                Err(AppError::SignatureBuildFailed(format!(
                    "not a readable PDF: {error}"
                )))
            };
        }
    };

    if !document.objects.values().any(|object| {
        object
            .as_dict()
            .ok()
            .is_some_and(|dict| dict.has_type(b"Sig"))
    }) {
        return Ok(None);
    }

    let certification_level = declared_level(&document);
    let sealed = match recover(written) {
        Ok(sealed) => sealed,
        // Signed, but we cannot recover the range: damaged, and say so.
        Err(_) => {
            return Ok(Some(SealedReading {
                integrity: Integrity::Damaged,
                revoked_hint: None,
                certification_level,
                // Unrecoverable range: whether anything was appended cannot be established, and
                // `Damaged` already decides the badge.
                later_revision: false,
            }));
        }
    };

    let integrity = verify(&sealed);
    let later_revision = later_revision_after_signing(written, sealed.byte_range);
    Ok(Some(SealedReading {
        integrity,
        revoked_hint: None,
        certification_level,
        later_revision,
    }))
}

/// What `inspect` established about one signed file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SealedReading {
    pub integrity: Integrity,
    /// Reserved for the revocation parser landing; always `None` today so a caller cannot mistake
    /// "no data" for "not revoked". `revocation::decide(None, None, now)` is the honest call and
    /// this field exists to make that explicit rather than to imply a check happened.
    pub revoked_hint: Option<bool>,
    /// The `/P` from the signature's `/Reference -> /TransformParams`, when it declares one.
    pub certification_level: Option<u8>,
    /// True when bytes were appended after the revision this signature covers. D-SG7 reads that as
    /// `modified`: the signed range is intact, but the file is not the file that was signed.
    pub later_revision: bool,
}

/// Read the declared DocMDP level, if the signature declares one.
fn declared_level(document: &Document) -> Option<u8> {
    if !declares_doc_mdp(document) {
        return None;
    }
    let signature = document
        .objects
        .values()
        .find_map(|object| object.as_dict().ok().filter(|dict| dict.has_type(b"Sig")))?;
    let references = signature.get(b"Reference").ok()?.as_array().ok()?;
    for reference in references {
        let reference = match reference {
            Object::Reference(id) => document.get_object(*id).ok()?.as_dict().ok()?,
            other => other.as_dict().ok()?,
        };
        let parameters = match reference.get(b"TransformParams").ok()? {
            Object::Reference(id) => document.get_object(*id).ok()?,
            other => other,
        };
        if let Ok(parameters) = parameters.as_dict() {
            if let Ok(Object::Integer(level)) = parameters.get(b"P") {
                // Only the three defined levels; anything else is a claim we do not recognise and
                // must not report as a certification.
                if (1..=3).contains(level) {
                    return Some(*level as u8);
                }
            }
        }
    }
    None
}

/// The cryptographic check: `messageDigest` against the covered bytes, then RSA over the attributes.
fn verify(sealed: &Sealed) -> Integrity {
    let reading = match read_cms(&sealed.cms) {
        Ok(reading) => reading,
        Err(_) => return Integrity::Damaged,
    };

    if !oid_is(&reading.digest_oid, OID_SHA256) {
        // A digest algorithm we do not implement. Reporting `Damaged` would be a false accusation;
        // `NotChecked` is the honest answer and cannot produce a valid badge.
        return Integrity::NotChecked;
    }
    let signature_algorithm_ok = oid_is(&reading.signature_oid, OID_SHA256_WITH_RSA)
        || oid_is(&reading.signature_oid, OID_RSA_ENCRYPTION);
    if !signature_algorithm_ok {
        return Integrity::NotChecked;
    }

    // (1) The attribute must be the digest of the bytes the range covers.
    let actual = Sha256::digest(&sealed.covered);
    if reading.message_digest.as_slice() != actual.as_slice() {
        return Integrity::Damaged;
    }

    // (2) The RSA signature must verify over the attributes as DER.
    let Some(certificate) = reading.certificate else {
        return Integrity::NotChecked;
    };
    let Some(public_key) = public_key(&certificate) else {
        return Integrity::NotChecked;
    };
    let Ok(signature) = Signature::try_from(reading.signature.as_slice()) else {
        return Integrity::Damaged;
    };
    match VerifyingKey::<Sha256>::new(public_key).verify(&reading.signed_attributes, &signature) {
        Ok(()) => Integrity::Intact,
        Err(_) => Integrity::Damaged,
    }
}

/// The certificate's RSA public key, or `None` when it is not an RSA key we can use.
///
/// Re-encoded through `SubjectPublicKeyInfo` DER rather than converted by type: `x509-cert 0.2`
/// (the `cms` generation) and `spki 0.7` (the `rsa` generation) do not share types, and a manual
/// field copy would be a second parser to keep correct.
fn public_key(certificate_der: &[u8]) -> Option<RsaPublicKey> {
    let certificate = x509_cert_cms::Certificate::from_der(certificate_der).ok()?;
    let spki =
        x509_cert_cms::der::Encode::to_der(&certificate.tbs_certificate.subject_public_key_info)
            .ok()?;
    RsaPublicKey::from_public_key_der(&spki).ok()
}

/// The `/ByteRange` of the newest signature, for a caller that wants to show what was covered.
pub fn newest_byte_range(written: &[u8]) -> Option<[i64; 4]> {
    recover(written).ok().map(|sealed| sealed.byte_range)
}

/// Whether a revision was appended after the one the newest signature covers.
///
/// A signature's `/ByteRange` is `[0, a, b, c]`: the two pieces it hashed, which between them leave a
/// gap for the signature's own `/Contents` but must otherwise run to the end of the file that was
/// signed. If the file the reader holds is longer than `b + c`, bytes were appended afterwards. That
/// is a **later revision**, and D-SG7 counts it as `modified` even though the signed bytes are
/// untouched -- the file is no longer the file that was signed.
///
/// Deliberately arithmetic on one range, not an object comparison. Parsing both revisions and
/// comparing them would report *what* changed, which this badge does not claim to know, and would
/// fail closed for a file whose older cross-reference table no longer parses. The question the badge
/// answers is only "is anything there that was not signed".
///
/// Known limit, stated rather than papered over: a file *truncated* after signing also ends before
/// the signed tail, and this returns `false` for it. Truncation that leaves the covered range
/// intact does not change the byte count the signature hashed, so the signature still verifies; the
/// missing bytes are past what it attested to. Detection of that case is not implemented.
pub fn later_revision_after_signing(written: &[u8], byte_range: [i64; 4]) -> bool {
    let [_, _, second_start, second_len] = byte_range;
    match (usize::try_from(second_start), usize::try_from(second_len)) {
        (Ok(start), Ok(len)) => match start.checked_add(len) {
            Some(signed_end) => written.len() > signed_end,
            // An offset that overflows `usize` cannot describe a real tail in this file.
            None => false,
        },
        // A negative offset or length is a malformed range, and `recover` already refused it before
        // this could be asked. Reported as "no later revision" rather than guessed either way.
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_truncated_der_element_is_refused_rather_than_panicking() {
        // Length says 200 bytes, 3 are present.
        assert!(der_read(&[TAG_SEQUENCE, 0xC8, 1, 2, 3], 0).is_none());
        assert!(der_read(&[], 0).is_none());
        assert!(der_read(&[TAG_SEQUENCE], 0).is_none());
    }

    #[test]
    fn a_long_form_length_beyond_four_bytes_is_refused() {
        // 0x85 = five length bytes: larger than any document this product can hold.
        assert!(der_read(&[TAG_SEQUENCE, 0x85, 0, 0, 0, 0, 1], 0).is_none());
    }

    #[test]
    fn hex_decoding_ignores_whitespace_and_stops_at_the_declared_length() {
        // A DER SEQUENCE of two bytes, then zero padding: the padding must not enter the blob.
        let padded = b"3002 0101 0000 0000 0000";
        let decoded = decode_hex(padded).expect("decode");
        assert_eq!(decoded, vec![0x30, 0x02, 0x01, 0x01]);
    }

    #[test]
    fn a_signature_whose_der_ends_in_a_zero_byte_is_not_truncated() {
        // The bug that made this suite flaky: trailing-zero trimming ate the last byte of a DER
        // whose contents legitimately ended in 0x00.
        let decoded = decode_hex(b"30020000 0000").expect("decode");
        assert_eq!(
            decoded,
            vec![0x30, 0x02, 0x00, 0x00],
            "a zero-length element and a zero body byte are both legitimate DER"
        );
    }

    #[test]
    fn a_non_hex_contents_gap_is_refused() {
        assert!(decode_hex(b"30ZZ").is_err());
    }

    #[test]
    fn the_byte_range_is_taken_from_the_last_signature() {
        // Two revisions: the second signature's range must win, or the badge reports on a revision
        // the user did not just add.
        let bytes = b".../ByteRange[0 100 200 300].../ByteRange[0 400 500 600]";
        assert_eq!(find_byte_range(bytes).ok(), Some([0, 400, 500, 600]));
    }

    #[test]
    fn a_range_that_runs_past_the_file_is_refused() {
        let written = b"/ByteRange[0 10 20 99999]";
        assert!(
            recover(written).is_err(),
            "a range claiming bytes the file does not have cannot describe it"
        );
    }

    #[test]
    fn retagging_changes_only_the_tag_and_preserves_the_body() {
        let contents = [0x30, 0x03, 0x02, 0x01, 0x05];
        let retagged = retag_as_set(&contents);
        assert_eq!(retagged[0], TAG_SET);
        assert_eq!(&retagged[retagged.len() - contents.len()..], &contents);
    }
}
