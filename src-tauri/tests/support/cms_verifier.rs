//! The independent CMS verifier, shared by the tests that prove a signature.
//!
//! I-SG1 asks for a check written by something other than the code that produced the blob. This
//! walks the DER itself and verifies with `rsa`'s *verifier*, which shares no code with `cms`'s
//! *builder* — the axis that matters, because a builder that omitted the `messageDigest` attribute
//! would otherwise agree with itself.
//!
//! It lives here rather than in one test file because two test binaries need it: the substrate
//! proof (`sign_pdf.rs`) and the save-path proof (`sign_save.rs`). A second copy is not a second
//! opinion — it is the same opinion twice, and the first copy would decay while the second passed.
//!
//! Included with `#[path = "support/cms_verifier.rs"] mod cms_verifier;` so each test binary
//! compiles the single source. `cargo test` does not build a `tests/support/` directory as its own
//! crate, so the shared file has no `main` and no `#[test]` of its own.

use der_cms::{Decode, Encode};
use rsa::pkcs1v15::{Signature, VerifyingKey};
use rsa::pkcs8::DecodePublicKey;
use rsa::signature::Verifier;
use rsa::RsaPublicKey;
use sha2::{Digest, Sha256};

/// Minimal DER reader. Returns `(tag, contents, total_consumed)`.
pub fn der_read(bytes: &[u8], at: usize) -> Option<(u8, &[u8], usize)> {
    let tag = *bytes.get(at)?;
    let first = *bytes.get(at + 1)?;
    let (length, header) = if first & 0x80 == 0 {
        (first as usize, 2usize)
    } else {
        let count = (first & 0x7F) as usize;
        if count == 0 || count > 4 {
            return None;
        }
        let mut value = 0usize;
        for index in 0..count {
            value = (value << 8) | *bytes.get(at + 2 + index)? as usize;
        }
        (value, 2 + count)
    };
    let body = at + header;
    let end = body.checked_add(length)?;
    Some((tag, bytes.get(body..end)?, header + length))
}

/// Iterate the children of a constructed value's contents.
pub fn der_children(contents: &[u8]) -> Vec<(u8, &[u8])> {
    let mut out = Vec::new();
    let mut at = 0usize;
    while at < contents.len() {
        match der_read(contents, at) {
            Some((tag, body, consumed)) => {
                out.push((tag, body));
                at += consumed;
            }
            None => break,
        }
    }
    out
}

pub fn oid_matches(contents: &[u8], expected: &[u8]) -> bool {
    contents == expected
}

/// `1.2.840.113549.1.1.11` (`sha256WithRSAEncryption`) — what `cms 0.2.3` actually emits as the
/// `SignerInfo.signatureAlgorithm`, measured rather than assumed.
///
/// Measured 2026-11-08: the builder writes the combined `sha256WithRSAEncryption` OID, **not** bare
/// `rsaEncryption` with the digest named only in `digestAlgorithm`. Both are legal CMS, so a
/// verifier that hardcodes one form rejects valid signatures from the other. This test accepts
/// either and names which it saw.
pub const OID_SHA256_WITH_RSA: &[u8] = &[0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x01, 0x0B];
/// `1.2.840.113549.1.1.1` (`rsaEncryption`) — the digest-agnostic form, also accepted.
pub const OID_RSA_ENCRYPTION: &[u8] = &[0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x01, 0x01];
/// `1.2.840.113549.1.7.1` (data) — the type of the *encapsulated* content.
pub const OID_DATA: &[u8] = &[0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x07, 0x01];
/// `1.2.840.113549.1.7.2` (signedData) — the type of the outer `ContentInfo`.
///
/// Not `id-data`: the outer OID says "this is a SignedData", and the inner
/// `encapContentInfo.eContentType` says "the content it signs is data". Conflating the two is the
/// kind of off-by-one-OID mistake that makes a verifier accept the wrong thing.
pub const OID_SIGNED_DATA: &[u8] = &[0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x07, 0x02];
/// `2.16.840.1.101.3.4.2.1` (sha256).
pub const OID_SHA256: &[u8] = &[0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x01];

/// What the independent verifier read out of a CMS blob.
pub struct CmsReading {
    /// True when `eContent` is absent, which is what `adbe.pkcs7.detached` means.
    pub detached: bool,
    /// `(oid, signature_bytes)` for the first signer, and the raw `SignedAttributes` contents.
    pub signature_bytes: Vec<u8>,
    pub signed_attributes: Vec<u8>,
    pub digest_oid: Vec<u8>,
    pub signature_oid: Vec<u8>,
    /// The `messageDigest` attribute's value, if present.
    pub message_digest: Option<Vec<u8>>,
    /// Each certificate carried in `SignedData.certificates`, re-encoded as DER.
    ///
    /// Collected rather than merely counted: a blob whose `sid` names a certificate it does not
    /// carry is unattributable, and the serial number alone cannot prove inclusion because the
    /// serial also appears inside the `SignerIdentifier`. A mutation that dropped the entity
    /// certificate from the blob survived an assertion built on the serial alone.
    pub certificates: Vec<Vec<u8>>,
}

/// Parse a detached CMS `ContentInfo` by hand.
///
/// Layout (RFC 5652 §5): `ContentInfo{ contentType, [0] SignedData }`,
/// `SignedData{ version, digestAlgorithms SET, encapContentInfo, [0] certificates, signerInfos SET }`,
/// `SignerInfo{ version, sid, digestAlgorithm, [0] signedAttrs, signatureAlgorithm, OCTET STRING }`.
pub fn independent_verifier(der: &[u8]) -> CmsReading {
    let (tag, content_info, _) = der_read(der, 0).expect("ContentInfo SEQUENCE");
    assert_eq!(tag, 0x30, "ContentInfo must be a SEQUENCE");

    let top = der_children(content_info);
    let content_type = top.first().expect("contentType").1;
    assert!(
        oid_matches(content_type, OID_SIGNED_DATA),
        "the outer contentType must be id-signedData, not id-data"
    );

    let (wrapped_tag, signed_data_body, _) = {
        let mut at = 0usize;
        let first = der_read(content_info, at).expect("contentType");
        at += first.2;
        let second = der_read(content_info, at).expect("[0] SignedData");
        second
    };
    assert_eq!(wrapped_tag, 0xA0, "SignedData must be [0] explicit");
    let (sd_tag, sd_contents, _) = der_read(signed_data_body, 0).expect("SignedData SEQUENCE");
    assert_eq!(sd_tag, 0x30);

    let sd = der_children(sd_contents);
    // version(0), digestAlgorithms(1), encapContentInfo(2), then optional [0]/[1], then signerInfos.
    let encap = sd.get(2).expect("encapContentInfo").1;
    let encap_fields = der_children(encap);
    let econtent_type = encap_fields.first().expect("eContentType").1;
    assert!(
        oid_matches(econtent_type, OID_DATA),
        "eContentType must be id-data"
    );
    // A detached signature carries no [0] eContent.
    let detached = !encap_fields.iter().any(|(tag, _)| *tag == 0xA0);

    let mut reading = CmsReading {
        detached,
        signature_bytes: Vec::new(),
        signed_attributes: Vec::new(),
        digest_oid: Vec::new(),
        signature_oid: Vec::new(),
        message_digest: None,
        certificates: Vec::new(),
    };

    // `certificates [0] IMPLICIT` is optional; when present it is an unordered SET of
    // CertificateChoices. Only the SEQUENCE alternative (plain X.509) is expected here.
    for (tag, body) in &sd {
        if *tag != 0xA0 {
            continue;
        }
        let mut at = 0usize;
        while at < body.len() {
            match der_read(body, at) {
                Some((child_tag, _, consumed)) => {
                    if child_tag == 0x30 {
                        let full = &body[at..at + consumed];
                        let certificate = x509_cert_cms::Certificate::from_der(full)
                            .expect("parse a carried cert");
                        reading
                            .certificates
                            .push(certificate.to_der().expect("re-encode the carried cert"));
                    }
                    at += consumed;
                }
                None => break,
            }
        }
    }

    let signer_infos = sd
        .iter()
        .rev()
        .find(|(tag, _)| *tag == 0x31)
        .expect("signerInfos SET")
        .1;
    let (si_tag, si_contents, _) = der_read(signer_infos, 0).expect("SignerInfo SEQUENCE");
    assert_eq!(si_tag, 0x30);
    let si = der_children(si_contents);

    // version(0), sid(1), digestAlgorithm(2), [0] signedAttrs(3), signatureAlgorithm, signature.
    let digest_algorithm = si.get(2).expect("digestAlgorithm").1;
    reading.digest_oid = der_children(digest_algorithm)
        .first()
        .expect("digest OID")
        .1
        .to_vec();

    let signed_attrs = si
        .iter()
        .find(|(tag, _)| *tag == 0xA0)
        .expect("[0] signedAttrs must be present: without it nothing binds the content");
    reading.signed_attributes = signed_attrs.1.to_vec();

    // The last two children are signatureAlgorithm and the signature OCTET STRING.
    let signature_algorithm = si[si.len() - 2].1;
    reading.signature_oid = der_children(signature_algorithm)
        .first()
        .expect("signature OID")
        .1
        .to_vec();
    let signature = si[si.len() - 1].1;
    reading.signature_bytes = signature.to_vec();

    // Walk the signed attributes for the messageDigest value.
    for (tag, body) in der_children(signed_attrs.1) {
        assert_eq!(tag, 0x30, "each Attribute is a SEQUENCE");
        let fields = der_children(body);
        let attr_type = fields.first().expect("attr type").1;
        // 1.2.840.113549.1.9.4 = messageDigest
        if oid_matches(
            attr_type,
            &[0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x09, 0x04],
        ) {
            let values = fields.get(1).expect("attr values").1;
            let (octet_tag, octet, _) = der_read(values, 0).expect("messageDigest OCTET STRING");
            assert_eq!(octet_tag, 0x04);
            reading.message_digest = Some(octet.to_vec());
        }
    }

    reading
}

/// Re-encode `SignedAttributes` as DER requires for signing: the tag changes from `[0]` to `SET`.
///
/// This is the detail that makes hand-verification possible and is *not* a detail our builder
/// controls — RFC 5652 §5.4 signs the DER encoding of the SET OF, not of the implicitly-tagged
/// form that appears in the message.
pub fn signed_attributes_for_signing(contents: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(contents.len() + 6);
    out.push(0x31);
    // Re-derive the length header at the same width the original used, so the encoding we verify
    // is byte-identical to what the signer signed.
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

/// Verify a detached CMS blob against `content` using `rsa`'s verifier and our own DER walk.
///
/// Returns the `messageDigest` it checked, so a caller can assert on the relationship between the
/// two digests rather than trusting that the check happened.
pub fn verify_independently(
    der: &[u8],
    content: &[u8],
    public_key: &RsaPublicKey,
) -> Result<Vec<u8>, String> {
    let reading = independent_verifier(der);

    if !reading.detached {
        return Err("the blob is not detached: it carries eContent".into());
    }
    if !oid_matches(&reading.digest_oid, OID_SHA256) {
        return Err(format!("unexpected digest OID {:02X?}", reading.digest_oid));
    }
    let signature_algorithm_ok = oid_matches(&reading.signature_oid, OID_SHA256_WITH_RSA)
        || oid_matches(&reading.signature_oid, OID_RSA_ENCRYPTION);
    if !signature_algorithm_ok {
        return Err(format!(
            "unexpected signature OID {:02X?}",
            reading.signature_oid
        ));
    }

    let message_digest = reading
        .message_digest
        .ok_or("no messageDigest signed attribute: the signature binds nothing")?;

    // (1) The attribute must be the digest of the content we were handed.
    let actual = Sha256::digest(content);
    if message_digest.as_slice() != actual.as_slice() {
        return Err(format!(
            "messageDigest {} does not match sha256(content) {}",
            hex(&message_digest),
            hex(&actual)
        ));
    }

    // (2) The RSA signature must verify over the signed attributes as DER.
    let to_verify = signed_attributes_for_signing(&reading.signed_attributes);
    VerifyingKey::<Sha256>::new(public_key.clone())
        .verify(
            &to_verify,
            &Signature::try_from(reading.signature_bytes.as_slice())
                .map_err(|error| format!("signature is malformed: {error}"))?,
        )
        .map_err(|error| format!("RSA verification failed: {error}"))?;

    Ok(message_digest)
}

/// A signer's public key, read from the certificate the blob ships.
///
/// Read out of the certificate rather than out of the private key, so the verification depends on
/// the certificate the blob actually carries. A signature that verified against a key recovered
/// from the private key would prove the arithmetic and not the claim.
pub fn public_key_from_certificate(certificate_der: &[u8]) -> RsaPublicKey {
    let certificate =
        x509_cert_cms::Certificate::from_der(certificate_der).expect("parse the certificate");
    // Re-encode the SubjectPublicKeyInfo and parse it with `rsa`'s own SPKI decoder rather than
    // converting between the two `x509-cert`/`spki` generations by type, which they do not share.
    let spki_der = certificate
        .tbs_certificate
        .subject_public_key_info
        .to_der()
        .expect("re-encode the SubjectPublicKeyInfo");
    RsaPublicKey::from_public_key_der(&spki_der).expect("the certificate's public key is RSA")
}

/// Recover `(signature_der, covered_content)` from a finished signed file's own bytes.
///
/// Everything is re-derived from the file: the gap is located by searching, the hex is decoded, and
/// the covered content is rebuilt from the declared `/ByteRange` integers. A helper that trusted
/// offsets from the code that wrote the file would pass while the file said something else.
pub fn extract_signature(written: &[u8]) -> (Vec<u8>, Vec<u8>) {
    let marker = b"/ByteRange[";
    let at = written
        .windows(marker.len())
        .position(|window| window == marker)
        .expect("/ByteRange present")
        + marker.len();
    let tail = std::str::from_utf8(&written[at..at + 64]).expect("ASCII digits");
    let end = tail.find(']').expect("array closes");
    let numbers: Vec<usize> = tail[..end]
        .split_whitespace()
        .map(|part| part.trim_end_matches(',').parse::<usize>().unwrap())
        .collect();
    assert_eq!(numbers.len(), 4, "a /ByteRange has four integers");
    let (start, first, second_start, second_len) = (numbers[0], numbers[1], numbers[2], numbers[3]);

    let mut covered = Vec::new();
    covered.extend_from_slice(&written[start..start + first]);
    covered.extend_from_slice(&written[second_start..second_start + second_len]);

    let gap_start = start + first + 1; // past the '<'
    let gap_end = second_start - 1; // the '>'
    let hex_gap = std::str::from_utf8(&written[gap_start..gap_end]).expect("hex is ASCII");
    (decode_contents_gap(hex_gap.as_bytes()), covered)
}

/// Decode the `/Contents` hex gap down to the exact DER it holds.
///
/// The real DER length comes from its own header, not from trailing zeros. The gap is zero-padded
/// to a fixed size, but a signature may legitimately *end* in `0x00`, and a DER SEQUENCE ends with
/// the last byte of its last element, so nothing about a zero says "padding". Stripping every
/// trailing zero truncated such a signature by one byte and the parse then failed at
/// "ContentInfo SEQUENCE" on a signature that was in fact well formed: measured at roughly one run
/// in ten. Parse first, then trust the declared length.
fn decode_contents_gap(hex_gap: &[u8]) -> Vec<u8> {
    let mut decoded = Vec::with_capacity(hex_gap.len() / 2);
    for pair in hex_gap.chunks(2) {
        decoded.push(u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).expect("hex"));
    }
    let (_, _, der_len) =
        der_read(&decoded, 0).expect("the /Contents gap must begin with a DER value");
    decoded.truncate(der_len);
    decoded
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod gap_tests {
    use super::*;

    /// A 4-byte DER SEQUENCE whose last content byte is `0x00`, padded with zeros as the real
    /// `/Contents` gap is. `30 02 01 00` is the shortest shape where the zero is *data*, not
    /// padding: a strict DER encoder is free to emit these exact bytes.
    fn der_ending_in_zero() -> Vec<u8> {
        let mut gap = vec![0x30, 0x02, 0x01, 0x00];
        gap.extend(std::iter::repeat_n(0, 12));
        gap
    }

    fn as_hex(bytes: &[u8]) -> Vec<u8> {
        bytes
            .iter()
            .flat_map(|b| format!("{b:02X}").into_bytes())
            .collect()
    }

    /// The regression this pins: a signature that legitimately ends in `0x00` must not lose it.
    ///
    /// The old implementation computed the DER length by trimming trailing zeros, which is
    /// indistinguishable from trimming padding and so truncated this input to three bytes. That
    /// surfaced as an intermittent failure of `a_signature_over_one_document_does_not_verify_
    /// against_another`, roughly one run in ten, because whether the RSA signature happened to end
    /// in `0x00` was luck. A real signature only hits the bad case about one run in six, so this
    /// test states the case directly and fails every time the strip comes back.
    #[test]
    fn a_contents_gap_ending_in_a_zero_keeps_its_byte() {
        let gap = der_ending_in_zero();
        let decoded = decode_contents_gap(&as_hex(&gap));
        assert_eq!(
            decoded,
            vec![0x30, 0x02, 0x01, 0x00],
            "the declared DER length is 4 bytes; the trailing padding must not be trimmed from \
             inside it"
        );
    }

    /// And the padding after the DER is still dropped, or the caller would get the whole gap.
    #[test]
    fn padding_after_the_declared_length_is_dropped() {
        let decoded = decode_contents_gap(&as_hex(&der_ending_in_zero()));
        assert_eq!(
            decoded.len(),
            4,
            "the twelve padding zeros must not survive"
        );
    }
}
