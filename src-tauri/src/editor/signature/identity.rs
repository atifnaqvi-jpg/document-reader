//! Describing certificates as the signer needs them (P3.2 D-SG4, D-SG5).
//!
//! Two halves of the Windows-store feature exist, and they are split along the line where the
//! evidence changes. `winstore.rs` owns the Win32 calls that read the store; it is `cfg(windows)`
//! and a Linux run cannot execute a line of it. This module owns everything the signer actually
//! *decides* -- which certificates are usable, what to show for each, and what to do with one that
//! will not parse -- and it is deliberately platform-neutral so the Linux gate can prove it.
//!
//! The split is not tidiness. The Windows CI job is a compile check plus a render smoke test; it
//! runs no certificate logic. If the filtering and describing lived behind `cfg(windows)` too, the
//! only thing standing between a wrong answer and a shipped installer would be "it compiled".
//!
//! **Nothing here says a certificate is trustworthy.** `has_private_key` means the store holds a
//! key reference for it, which is a fact about the store, not about the key's provenance. Trust is
//! D-SG4's separate question and is answered `unknown` until a local trust store says otherwise.

use std::time::SystemTime;

use x509_cert::der::Decode;

use crate::model::AppError;

/// One certificate as the platform collector found it: its DER bytes, and whether the store holds
/// a private key for it.
///
/// DER rather than a platform handle, for the same reason `pfx.rs` hands out DER (K25): the type
/// that crosses out of a platform-specific module should carry no platform-specific power. This
/// one is additionally `cfg`-free, so a Linux test can construct one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoreEntry {
    pub der: Vec<u8>,
    /// True when the store holds either a CryptoAPI key provider or a CNG key handle for this
    /// certificate. The Windows collector checks **both** properties; checking only the legacy
    /// one would silently hide every modern CNG-backed signer, which is a plausible outcome for a
    /// user who imported their certificate with a current tool.
    pub has_private_key: bool,
}

/// What the signer needs to know about a certificate, in our own terms.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CertificateSummary {
    /// The DER this was parsed from, kept so the signer can hand the exact bytes onward.
    pub der: Vec<u8>,
    /// Subject distinguished name, RFC 4514-ish, as `x509-cert` renders it.
    pub subject: String,
    pub issuer: String,
    /// Upper-case hex, no separators.
    pub serial_hex: String,
    /// The signature/public-key algorithm OID from the certificate's `SubjectPublicKeyInfo`.
    pub key_algorithm: String,
    pub not_before: SystemTime,
    pub not_after: SystemTime,
    pub has_private_key: bool,
}

impl CertificateSummary {
    /// Whether the validity window has closed at `now`.
    ///
    /// Takes `now` as an argument rather than reading the clock: an expired certificate is a
    /// property of (certificate, instant) and a test that has to wait a decade for an answer is
    /// not a test. The caller passes `SystemTime::now()` in production.
    pub fn expired_at(&self, now: SystemTime) -> bool {
        now > self.not_after
    }

    /// Whether the validity window has not opened yet at `now`. The mirror of `expired_at`, and
    /// kept separate because it is the same user-visible trap in the other direction: a
    /// certificate that cannot yet sign will fail at sign time with a message about dates.
    pub fn not_yet_valid_at(&self, now: SystemTime) -> bool {
        now < self.not_before
    }

    /// Whether this certificate can stand as a signing identity at `now`.
    ///
    /// Both halves matter and neither is a guess: without a private key it cannot sign at all,
    /// and outside its validity window a signature it produced would verify only as
    /// `unknown`/`modified` at the reader's end.
    pub fn can_sign_at(&self, now: SystemTime) -> bool {
        self.has_private_key && !self.expired_at(now) && !self.not_yet_valid_at(now)
    }

    /// A one-line label for a picker: subject plus, when they differ, the issuer.
    ///
    /// A self-signed certificate says the same thing twice; repeating it wastes the width the
    /// subject already needs.
    pub fn label(&self) -> String {
        if self.subject == self.issuer {
            self.subject.clone()
        } else {
            format!("{} (issued by {})", self.subject, self.issuer)
        }
    }
}

/// A certificate the collector returned that we could not describe, with the reason kept.
///
/// It is reported rather than dropped. A user whose signing certificate vanished from the picker
/// with no explanation has no next step, and "the list is short today" is indistinguishable from
/// "your certificate is unusable". The count and the reasons go to the UI.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Skipped {
    /// Hex of the first bytes of the DER, so an unparseable entry is still identifiable in a
    /// support report without dumping a whole certificate into a log line.
    pub der_prefix_hex: String,
    pub reason: String,
}

/// Everything a listing produced.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Listing {
    /// Every certificate we could describe, **including** ones without a private key. They are
    /// kept rather than filtered because the UI can say "no key" while a silent omission says
    /// nothing at all.
    pub certificates: Vec<CertificateSummary>,
    /// Certificates we could not parse, with the reason.
    pub skipped: Vec<Skipped>,
}

impl Listing {
    /// The certificates that can actually sign at `now`, in a stable order.
    ///
    /// Ordered by subject then serial so the picker does not reshuffle between openings: the
    /// Windows enumeration order is not documented as stable, and a list that reorders under the
    /// pointer is how the wrong certificate gets picked.
    pub fn signable_at(&self, now: SystemTime) -> Vec<&CertificateSummary> {
        let mut usable: Vec<&CertificateSummary> = self
            .certificates
            .iter()
            .filter(|certificate| certificate.can_sign_at(now))
            .collect();
        usable.sort_by(|a, b| {
            a.subject
                .cmp(&b.subject)
                .then_with(|| a.serial_hex.cmp(&b.serial_hex))
        });
        usable
    }

    /// True when the store was read successfully and held nothing at all.
    ///
    /// Distinct from "nothing can sign", which may still be a non-empty list of key-less
    /// certificates. The two need different sentences in the UI.
    pub fn is_empty(&self) -> bool {
        self.certificates.is_empty() && self.skipped.is_empty()
    }
}

/// Describe one DER certificate.
pub fn describe(der: &[u8]) -> Result<CertificateSummary, AppError> {
    let certificate = x509_cert::Certificate::from_der(der).map_err(|error| {
        AppError::CertificateUnreadable(format!("not a DER certificate: {error}"))
    })?;
    let tbs = certificate.tbs_certificate();
    let validity = tbs.validity();
    Ok(CertificateSummary {
        der: der.to_vec(),
        subject: tbs.subject().to_string(),
        issuer: tbs.issuer().to_string(),
        serial_hex: hex::encode_upper(tbs.serial_number().as_bytes()),
        key_algorithm: tbs.subject_public_key_info().algorithm.oid.to_string(),
        not_before: validity.not_before.to_system_time(),
        not_after: validity.not_after.to_system_time(),
        has_private_key: false,
    })
}

/// Turn the collector's raw entries into a [`Listing`].
///
/// A certificate that will not parse is recorded in `skipped` and does not stop the listing: one
/// unreadable entry in a store of hundreds must not hide the rest, and it must not be swallowed
/// either.
pub fn listing_from(entries: &[StoreEntry]) -> Listing {
    let mut listing = Listing::default();
    for entry in entries {
        match describe(&entry.der) {
            Ok(mut summary) => {
                summary.has_private_key = entry.has_private_key;
                listing.certificates.push(summary);
            }
            Err(error) => listing.skipped.push(Skipped {
                der_prefix_hex: hex::encode_upper(&entry.der[..entry.der.len().min(8)]),
                reason: error.to_string(),
            }),
        }
    }
    listing
}
