//! Detached CMS signature construction (P3.2 SCOPE-C, D-SG1, D-SG2, I-SG1).
//!
//! This module turns an [`ImportedIdentity`] and the byte string a signature must cover into a
//! DER-encoded CMS `SignedData` with `eContent` absent — the shape `adbe.pkcs7.detached` denotes.
//!
//! **The single fact that matters here** (`docs/P3.2-FACTS-cms-signing.md`, F2): a detached
//! signature needs the content digest passed *explicitly*. `cms`'s builder will happily accept a
//! `None` digest and emit well-formed DER; OpenSSL then reports
//! `error reading messagedigest attribute`, because no `messageDigest` signed attribute was
//! produced. That failure is invisible to every check that only re-parses what we wrote, so this
//! module computes the digest itself and never permits `None`.
//!
//! **What this module does not decide.** Whether the certificate is trusted is D-SG4. What
//! certification level the document claims is D-SG6. What badge a reader shows is D-SG7. This
//! module answers one question: given a key and a byte string, what is the detached signature.
//!
//! **Dependency boundary (K25).** The types crossing this edge are `&[u8]` of DER and `Vec<u8>` of
//! DER. `x509-cert 0.2`, `der 0.7` and `rsa 0.9` live only inside this file; the rest of the
//! crate sees none of them. `tests/signing_surface.rs` asserts the crate-name side of that.

use cms::builder::{SignedDataBuilder, SignerInfoBuilder};
use cms::cert::{CertificateChoices, IssuerAndSerialNumber};
use cms::signed_data::{EncapsulatedContentInfo, SignerIdentifier};
use der_cms::{Decode, Encode};
use rsa::pkcs1::DecodeRsaPrivateKey;
use rsa::pkcs1v15::{Signature, SigningKey};
use rsa::pkcs8::DecodePrivateKey;
use rsa::RsaPrivateKey;
use sha2::{Digest, Sha256};
use spki_cms::AlgorithmIdentifierOwned;
use x509_cert_cms::Certificate;

use crate::editor::signature::pfx::ImportedIdentity;
use crate::model::AppError;

/// Reserved signature space, in DER bytes.
///
/// The gap must be sized before the signature exists, because a signature cannot be produced until
/// the bytes it covers exist and those bytes include the gap. 8192 holds an RSA-2048/SHA-256 CMS
/// with a small chain with margin (measured: 1411 bytes for one certificate, ~1565 from OpenSSL
/// with signed attributes); see `docs/P3.2-FACTS-cms-signing.md` F3. A signature that overflows it
/// is refused, never truncated.
pub const CONTENTS_BYTES: usize = 8192;

/// Width reserved for each variable `/ByteRange` integer, in decimal digits.
///
/// Ten digits covers a ten-gigabyte file. The width is reserved because lopdf serialises integers
/// minimally (design K18b), so a zero-padded placeholder cannot survive the object model and the
/// digits have to be overwritten as raw bytes.
pub const BYTE_RANGE_FIELD_WIDTH: usize = 10;

/// Digest algorithm for every signature this build produces.
///
/// SHA-256 only. PDF 2.0 deprecates SHA-1 for signing, and offering a choice the product has no
/// reason to make is how a weak default gets picked. A verifier that needs to *read* a foreign
/// SHA-1 signature is a separate concern and is not this module.
const DIGEST: AlgorithmIdentifierOwned = AlgorithmIdentifierOwned {
    oid: const_oid::db::rfc5912::ID_SHA_256,
    parameters: None,
};

/// A detached CMS signature, and the digest of what it covers.
///
/// The digest is returned rather than recomputed by the caller. A caller that hashed the content
/// again would be hashing something it believes is the same bytes; the point of returning what
/// this module actually signed is that the two cannot drift.
pub struct DetachedSignature {
    /// DER-encoded CMS `ContentInfo`, ready to be hex-encoded into `/Contents`.
    pub der: Vec<u8>,
    /// SHA-256 of `content`, computed from the same slice that was signed.
    pub content_digest: [u8; 32],
}

/// Build a detached CMS signature over `content`.
///
/// `content` is the byte string a PDF `/ByteRange` denotes: the file with the `/Contents` gap
/// removed, which for a signed PDF is `prefix || suffix` (F1). It is passed as one slice rather
/// than as two ranges because a caller that concatenates correctly at sign time and incorrectly at
/// verify time is exactly the bug that has no symptom until a third-party reader rejects the file.
pub fn sign_detached(
    identity: &ImportedIdentity,
    content: &[u8],
) -> Result<DetachedSignature, AppError> {
    let certificate = entity_certificate(identity)?;
    let private_key = private_key(identity)?;

    let content_digest: [u8; 32] = Sha256::digest(content).into();

    let signer_identifier = SignerIdentifier::IssuerAndSerialNumber(IssuerAndSerialNumber {
        issuer: certificate.tbs_certificate.issuer.clone(),
        serial_number: certificate.tbs_certificate.serial_number.clone(),
    });

    let content_info = EncapsulatedContentInfo {
        econtent_type: const_oid::db::rfc5911::ID_DATA,
        // `None` is what makes this detached: the content is not carried in the blob.
        econtent: None,
    };

    let signer = SigningKey::<Sha256>::new(private_key);

    // The `Some(..)` is load-bearing. See the module comment: `None` produces DER that parses and
    // verifies against nothing.
    let signer_info = SignerInfoBuilder::new(
        &signer,
        signer_identifier,
        DIGEST,
        &content_info,
        Some(content_digest.as_slice()),
    )
    .map_err(|error| AppError::SignatureBuildFailed(error.to_string()))?;

    let mut builder = SignedDataBuilder::new(&content_info);
    builder
        .add_digest_algorithm(DIGEST)
        .map_err(|error| AppError::SignatureBuildFailed(error.to_string()))?;

    for der in &identity.chain {
        let cert = Certificate::from_der(der).map_err(|error| {
            AppError::CertificateUnreadable(format!(
                "chain certificate in {} could not be read: {error}",
                identity.alias
            ))
        })?;
        // `add_certificate` refuses a duplicate; a chain that repeats a certificate is not an
        // error the user can act on, so a repeat is skipped rather than surfaced.
        if builder
            .add_certificate(CertificateChoices::Certificate(cert))
            .is_err()
        {
            continue;
        }
    }

    let signed_data = builder
        .add_signer_info::<SigningKey<Sha256>, Signature>(signer_info)
        .map_err(|error| AppError::SignatureBuildFailed(error.to_string()))?
        .build()
        .map_err(|error| AppError::SignatureBuildFailed(error.to_string()))?;

    let der = signed_data
        .to_der()
        .map_err(|error| AppError::SignatureBuildFailed(error.to_string()))?;

    Ok(DetachedSignature {
        der,
        content_digest,
    })
}

/// Where a signing revision reserved its two patchable regions.
///
/// Offsets into the revision bytes as written, produced by whoever built the revision. They are
/// passed in rather than searched for here: a search for `/Contents<` would find the wrong one in
/// a document that already carries a signature, and a signer that picks the wrong gap produces a
/// file whose `/ByteRange` verifies against nothing.
#[derive(Debug, Clone, Copy)]
pub struct ReservedGap {
    /// Byte offset of the first hex digit after `<`.
    pub contents_open: usize,
    /// Byte offset of the `>`, i.e. one past the last hex digit.
    pub contents_close: usize,
    /// Byte offset of the first digit of each variable `/ByteRange` integer, in order.
    pub field_offsets: [usize; 3],
}

/// A sealed revision: the finished bytes and what was signed.
pub struct SealedRevision {
    pub bytes: Vec<u8>,
    pub content_digest: [u8; 32],
    /// The four `/ByteRange` integers actually written into the file.
    pub byte_range: [i64; 4],
}

/// Sign a built revision in place and return the finished file.
///
/// **The order in this function is the whole point, and it is not the obvious one.** The
/// `/ByteRange` integers sit inside the region the signature covers — they describe the file, and
/// the file's own bytes include them. So:
///
/// 1. compute the range from the reserved offsets,
/// 2. **patch the range first**,
/// 3. hash what the file now is,
/// 4. sign that,
/// 5. patch the signature into `/Contents`, which is the one region excluded from the range.
///
/// Signing before patching the range produces a signature over digits that then change, and the
/// failure is silent: the file is well formed, the blob parses, and only a verifier that
/// recomputes from the finished bytes notices. Measured — this function exists because the first
/// version of the test got the order wrong and failed exactly that way.
pub fn seal_revision(
    revision_bytes: &[u8],
    gap: ReservedGap,
    identity: &ImportedIdentity,
) -> Result<SealedRevision, AppError> {
    let mut bytes = revision_bytes.to_vec();

    let byte_range = [
        0i64,
        (gap.contents_open - 1) as i64,
        (gap.contents_close + 1) as i64,
        (bytes.len() - (gap.contents_close + 1)) as i64,
    ];

    for (value, offset) in [byte_range[1], byte_range[2], byte_range[3]]
        .iter()
        .zip(gap.field_offsets)
    {
        let text = format!("{value:0width$}", width = BYTE_RANGE_FIELD_WIDTH);
        if text.len() != BYTE_RANGE_FIELD_WIDTH {
            return Err(AppError::SignatureBuildFailed(format!(
                "the declared byte range {value} does not fit the {BYTE_RANGE_FIELD_WIDTH} digit field"
            )));
        }
        let end = offset + BYTE_RANGE_FIELD_WIDTH;
        if end > bytes.len() {
            return Err(AppError::SignatureBuildFailed(
                "a reserved byte-range field lies outside the revision".into(),
            ));
        }
        bytes[offset..end].copy_from_slice(text.as_bytes());
    }

    // Hashing happens *after* the range is patched, which is the whole point of the ordering.
    let content = signed_content(&bytes, gap)?;
    let signature = sign_detached(identity, &content)?;

    let hex = encode_into_gap(&signature.der, gap)?;
    bytes[gap.contents_open..gap.contents_close].copy_from_slice(hex.as_bytes());

    Ok(SealedRevision {
        bytes,
        content_digest: signature.content_digest,
        byte_range,
    })
}

/// Hex-encode `der` into a reserved gap of exactly `2 * CONTENTS_BYTES` characters.
///
/// Split out from [`seal_revision`] so the refusal can be tested with a payload that is obviously
/// too large, without having to build a signature that big. Mutation testing found the original
/// inline version's guard unproven: replacing the refusal with a silent truncation left every test
/// green, because the only overflow test exercised a copy of this logic inside the test file.
///
/// **A truncated signature is worse than no signature.** It is a well-formed DER prefix that a
/// reader will try to parse, and a partly-written `/Contents` in a file whose `/ByteRange` claims
/// to cover a signature is the shape of a forgery gone wrong. Refuse.
fn encode_into_gap(der: &[u8], gap: ReservedGap) -> Result<String, AppError> {
    let hex_len = gap.contents_close - gap.contents_open;
    if hex_len != 2 * CONTENTS_BYTES {
        return Err(AppError::SignatureBuildFailed(format!(
            "the reserved /Contents gap is {hex_len} characters, expected {}",
            2 * CONTENTS_BYTES
        )));
    }
    if der.len() > CONTENTS_BYTES {
        return Err(AppError::SignatureBuildFailed(format!(
            "a {} byte signature does not fit the {CONTENTS_BYTES} byte reserved gap; \
             it is refused rather than truncated, because a truncated signature looks valid",
            der.len()
        )));
    }

    let mut hex = String::with_capacity(hex_len);
    for byte in der {
        hex.push_str(&format!("{byte:02X}"));
    }
    // The unused tail stays as zeros. A reader locates the real DER by parsing, so the padding is
    // not part of it; truncating the gap instead would move every following byte and invalidate
    // the very offsets just written.
    hex.push_str(&"0".repeat(2 * (CONTENTS_BYTES - der.len())));
    debug_assert_eq!(hex.len(), hex_len);
    Ok(hex)
}

/// The byte string a signature covers: everything except `<`, the hex digits and `>`.
fn signed_content(bytes: &[u8], gap: ReservedGap) -> Result<Vec<u8>, AppError> {
    if gap.contents_open == 0 || gap.contents_close + 1 > bytes.len() {
        return Err(AppError::SignatureBuildFailed(
            "the reserved gap lies outside the revision".into(),
        ));
    }
    if bytes[gap.contents_open - 1] != b'<' || bytes[gap.contents_close] != b'>' {
        return Err(AppError::SignatureBuildFailed(
            "the reserved gap is not delimited by '<' and '>'".into(),
        ));
    }
    let mut content = Vec::with_capacity(bytes.len());
    content.extend_from_slice(&bytes[..gap.contents_open - 1]);
    content.extend_from_slice(&bytes[gap.contents_close + 1..]);
    Ok(content)
}

/// The entity certificate — `chain[0]`, which `ImportedIdentity` documents as the leaf.
///
/// The key and the certificate are not proved to belong together here. They arrived linked from
/// the archive under `Strict` policy, and re-deriving that link needs the key's public half — see
/// `the_signer_identifier_matches_the_entity_certificate` for the part that is checked.
fn entity_certificate(identity: &ImportedIdentity) -> Result<Certificate, AppError> {
    let der = identity.chain.first().ok_or_else(|| {
        AppError::CertificateUnreadable(format!(
            "identity {} carries no certificate, so nothing identifies the signer",
            identity.alias
        ))
    })?;
    Certificate::from_der(der).map_err(|error| {
        AppError::CertificateUnreadable(format!(
            "entity certificate in {} could not be read: {error}",
            identity.alias
        ))
    })
}

/// Parse the identity's private key.
///
/// Both encodings are tried because `p12-keystore` reports whatever the archive carried, and a
/// PFX may hold either a bare PKCS#1 `RSAPrivateKey` or a PKCS#8 `PrivateKeyInfo`. Failing to
/// parse is reported with the alias only — never with key bytes, which is why the error string is
/// built from a constant plus the alias.
fn private_key(identity: &ImportedIdentity) -> Result<RsaPrivateKey, AppError> {
    if let Ok(key) = RsaPrivateKey::from_pkcs8_der(&identity.key_der) {
        return Ok(key);
    }
    RsaPrivateKey::from_pkcs1_der(&identity.key_der).map_err(|_| {
        AppError::SignatureBuildFailed(format!(
            "the private key for {} is not a key this build can use",
            identity.alias
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A gap of the size a real revision reserves, positioned arbitrarily inside a buffer.
    fn a_gap() -> ReservedGap {
        ReservedGap {
            contents_open: 100,
            contents_close: 100 + 2 * CONTENTS_BYTES,
            field_offsets: [0, 11, 22],
        }
    }

    #[test]
    fn the_digest_constant_is_sha256() {
        // A one-line guard on a constant that silently changes meaning: `ID_SHA_1` here would make
        // every signature this build produces one PDF 2.0 deprecates.
        assert_eq!(DIGEST.oid, const_oid::db::rfc5912::ID_SHA_256);
    }

    #[test]
    fn a_signature_that_fits_is_encoded_uppercase_and_zero_padded() {
        let hex = encode_into_gap(&[0xDE, 0xAD, 0xBE, 0xEF], a_gap()).expect("four bytes fit");
        assert_eq!(hex.len(), 2 * CONTENTS_BYTES, "the gap is filled exactly");
        assert!(hex.starts_with("DEADBEEF"), "uppercase hex, no separators");
        assert!(
            hex[8..].bytes().all(|byte| byte == b'0'),
            "the unused tail must be zeros, never another character"
        );
    }

    #[test]
    fn a_signature_one_byte_too_large_is_refused_not_truncated() {
        // The boundary, both sides. Off-by-one here is the difference between a signature and a
        // DER prefix a reader will try to parse.
        let exactly_full = vec![0xAB; CONTENTS_BYTES];
        let hex = encode_into_gap(&exactly_full, a_gap()).expect("exactly the gap must fit");
        assert_eq!(hex.len(), 2 * CONTENTS_BYTES);
        assert!(
            !hex[2..].contains('0'),
            "a full gap has no padding, so no zero character may appear"
        );

        let one_too_many = vec![0xAB; CONTENTS_BYTES + 1];
        let refused = encode_into_gap(&one_too_many, a_gap()).expect_err("must refuse");
        let message = refused.to_string();
        assert!(
            message.contains("does not fit") && message.contains("refused rather than truncated"),
            "the refusal must say it will not truncate: {message}"
        );
    }

    #[test]
    fn a_gap_of_the_wrong_width_is_refused() {
        // A revision built with a different `CONTENTS_BYTES` than this module compiles with would
        // otherwise write a hex string of a different length into a slice of the wrong size, which
        // panics deep in a copy rather than reporting the mismatch.
        let wrong = ReservedGap {
            contents_open: 100,
            contents_close: 100 + 2 * CONTENTS_BYTES - 2,
            field_offsets: [0, 11, 22],
        };
        let refused = encode_into_gap(&[0x01], wrong).expect_err("must refuse");
        assert!(
            refused.to_string().contains("reserved /Contents gap"),
            "the refusal must name the gap width: {refused}"
        );
    }

    #[test]
    fn the_signed_content_omits_both_delimiters_and_only_those() {
        // A concrete file: `AB<gap>C` must sign exactly `ABC`. Getting this wrong by one byte --
        // including a delimiter, or dropping a byte of the suffix -- produces a signature that is
        // internally consistent and rejected by every reader. Measured against iText/PDFBox/
        // Acrobat convention in `docs/P3.2-FACTS-cms-signing.md` F1.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"AB<");
        bytes.extend_from_slice(&vec![b'0'; 2 * CONTENTS_BYTES]);
        bytes.extend_from_slice(b">C");
        let gap = ReservedGap {
            contents_open: 3,
            contents_close: 3 + 2 * CONTENTS_BYTES,
            field_offsets: [0, 11, 22],
        };
        assert_eq!(
            signed_content(&bytes, gap).expect("delimited"),
            b"ABC".to_vec(),
            "exactly the prefix and suffix, with both delimiters and the gap removed"
        );
    }

    #[test]
    fn an_undelimited_gap_is_refused() {
        // The offsets come from the revision builder. If they ever name a region that is not
        // `<hex>`, signing would cover the wrong bytes and the resulting file would look fine.
        // Mutation testing found this guard reachable-but-unproven.
        // Long enough that the bounds check passes and only the delimiter check can fire.
        let bytes = vec![b'x'; 2 * CONTENTS_BYTES + 40];
        let gap = ReservedGap {
            contents_open: 10,
            contents_close: 10 + 2 * CONTENTS_BYTES,
            field_offsets: [0, 11, 22],
        };
        let refused = signed_content(&bytes, gap).expect_err("must refuse");
        assert!(
            refused.to_string().contains("delimited"),
            "the refusal must say the gap is not delimited: {refused}"
        );
    }

    #[test]
    fn a_gap_past_the_end_of_the_buffer_is_refused() {
        let bytes = b"short".to_vec();
        let gap = ReservedGap {
            contents_open: 100,
            contents_close: 100 + 2 * CONTENTS_BYTES,
            field_offsets: [0, 11, 22],
        };
        let refused = signed_content(&bytes, gap).expect_err("must refuse");
        assert!(
            refused.to_string().contains("outside the revision"),
            "the refusal must say the gap is outside the file: {refused}"
        );
    }

    #[test]
    fn a_gap_at_offset_zero_is_refused() {
        // `contents_open - 1` underflows if zero is ever passed, which is a panic rather than an
        // error in a release build. Refuse before touching it.
        let bytes = vec![b'<'; 8];
        let gap = ReservedGap {
            contents_open: 0,
            contents_close: 8,
            field_offsets: [0, 11, 22],
        };
        let refused = signed_content(&bytes, gap).expect_err("must refuse");
        assert!(
            refused.to_string().contains("outside the revision"),
            "a zero offset must be refused before it can underflow: {refused}"
        );
    }
}
