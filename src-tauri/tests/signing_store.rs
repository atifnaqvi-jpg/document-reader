//! How a certificate store listing is judged (P3.2 D-SG4, D-SG5, I-SG4's `unknown`).
//!
//! These run everywhere, including Linux CI, because the logic they cover lives in
//! `editor::signature::identity` and not behind `cfg(windows)`. The Windows CI job for this
//! programme is a compile check plus a render smoke test; if the filtering and describing sat
//! behind the platform gate as well, the only evidence for a shipped certificate picker would be
//! that it compiled on the machine that has a store.
//!
//! The certificate used here is the DER in `tests/fixtures/signing/signer.pfx`, a throwaway
//! identity generated for this suite (`tests/fixtures/signing/README.md`). It is decoded from the
//! PFX rather than committed a second time as a bare `.der`, so there is one copy of the key
//! material to reason about.
//!
//! What is deliberately **not** tested here: that Win32 returns the right certificates. That needs
//! a real store, it is `winstore.rs`, and no test on this machine can stand in for it. This file
//! proves the half that decides what to do with whatever Win32 hands over.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use document_reader_lib::editor::signature::identity::{
    describe, listing_from, CertificateSummary, Listing, StoreEntry,
};
use document_reader_lib::editor::signature::pfx::import_bytes;
use document_reader_lib::model::AppError;

const SIGNER_PFX: &[u8] = include_bytes!("fixtures/signing/signer.pfx");
const SIGNER_PASSWORD: &str = "correct-horse";

/// The fixture's certificate, as DER.
fn fixture_der() -> Vec<u8> {
    let imported = import_bytes(SIGNER_PFX, SIGNER_PASSWORD).expect("the fixture must open");
    imported.identities[0].chain[0].clone()
}

/// A fixed instant for the validity tests. The fixture is valid 2026-11-08 → 2036-11-05, so a
/// timestamp inside that window keeps the test from depending on the clock. Taken as a constant
/// rather than `now()` for the reason `expired_at` takes its `now` as an argument: a test whose
/// verdict changes with the calendar is not a test.
fn during_fixture_validity() -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(1_800_000_000) // 2027-01-15
}

fn before_fixture_validity() -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(1_600_000_000) // 2020-09-13
}

fn after_fixture_validity() -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(2_200_000_000) // 2039-09-18
}

#[test]
fn a_certificate_describes_its_subject_and_validity() {
    let summary = describe(&fixture_der()).expect("the fixture certificate must parse");
    assert_eq!(
        summary.subject, "C=GB,O=DocReader Test,CN=DocReader Test Signer",
        "the subject must be read from the DER, not assumed"
    );
    assert_eq!(
        summary.issuer, summary.subject,
        "the fixture is self-signed, so issuer and subject agree"
    );
    assert_eq!(
        summary.key_algorithm, "1.2.840.113549.1.1.1",
        "the fixture key is RSA; the OID is what the signer will branch on"
    );
    assert!(
        !summary.serial_hex.is_empty() && summary.serial_hex.chars().all(|c| c.is_ascii_hexdigit()),
        "the serial must be upper-case hex with no separators, got {:?}",
        summary.serial_hex
    );
    assert!(
        summary.not_before < summary.not_after,
        "validity must be ordered; not_before={:?} not_after={:?}",
        summary.not_before,
        summary.not_after
    );
    assert!(
        !summary.has_private_key,
        "describe() reads bytes and cannot know about a key; the caller must set that"
    );
}

/// A certificate is only offered as a signing identity when the store holds its key.
///
/// This is the guard for the picker's central filter. The Windows store holds hundreds of roots
/// and intermediates with no key; offering them as signable would let the user pick one and fail
/// later, at sign time, with a message about a missing key. The mutant that removes the
/// `has_private_key` condition from `can_sign_at` must fail this test.
#[test]
fn a_certificate_without_a_private_key_cannot_sign() {
    let mut summary = describe(&fixture_der()).expect("parse");
    summary.has_private_key = false;
    assert!(
        !summary.can_sign_at(during_fixture_validity()),
        "a certificate with no key in the store must not be offered as a signing identity"
    );

    summary.has_private_key = true;
    assert!(
        summary.can_sign_at(during_fixture_validity()),
        "with a key and inside its validity window the same certificate must be signable, or \
         the test above passes for the wrong reason"
    );
}

/// A certificate outside its validity window is not offered, in either direction.
#[test]
fn an_expired_or_not_yet_valid_certificate_cannot_sign() {
    let mut summary = describe(&fixture_der()).expect("parse");
    summary.has_private_key = true;

    assert!(
        summary.expired_at(after_fixture_validity()),
        "the fixture must read as expired after its not_after, or this test proves nothing"
    );
    assert!(!summary.expired_at(during_fixture_validity()));
    assert!(
        !summary.can_sign_at(after_fixture_validity()),
        "an expired certificate must not be offered: its signature would not stand up"
    );

    assert!(summary.not_yet_valid_at(before_fixture_validity()));
    assert!(!summary.not_yet_valid_at(during_fixture_validity()));
    assert!(
        !summary.can_sign_at(before_fixture_validity()),
        "a certificate that has not started must not be offered either"
    );
}

/// The picker lists what can sign, in an order that does not reshuffle between openings.
///
/// Windows does not document the enumeration order of a certificate store as stable. A list that
/// reorders under the pointer is how the wrong certificate gets picked, so the order is ours:
/// subject, then serial.
#[test]
fn signable_certificates_are_listed_in_a_stable_order() {
    let signer = fixture_der();
    let mut summary = describe(&signer).expect("parse");
    // `describe` reads bytes and reports no key; the store is what knows. Both entries must be
    // signable or the ordering assertion below would pass on an empty list.
    summary.has_private_key = true;
    let now = during_fixture_validity();

    // Two entries with the same subject and different serials: the tie must break on serial.
    let mut second = summary.clone();
    second.serial_hex = "00FF".to_string();
    let mut first = summary.clone();
    first.serial_hex = "0001".to_string();
    first.subject = "A=first".to_string();
    second.subject = "A=first".to_string();

    let listing = Listing {
        certificates: vec![second.clone(), first.clone()],
        skipped: Vec::new(),
    };

    let ordered = listing.signable_at(now);
    assert_eq!(ordered.len(), 2, "both must be signable at {now:?}");
    assert_eq!(
        ordered[0].serial_hex, "0001",
        "equal subjects must break the tie on serial so the order is total"
    );
    assert_eq!(ordered[1].serial_hex, "00FF");

    // Reversing the input must not change the output: the sort is a function of the data.
    let reversed = Listing {
        certificates: vec![first, second],
        skipped: Vec::new(),
    };
    let ordered_again = reversed.signable_at(now);
    let serials: Vec<&str> = ordered_again
        .iter()
        .map(|c| c.serial_hex.as_str())
        .collect();
    assert_eq!(
        serials,
        vec!["0001", "00FF"],
        "the listing order must not depend on the order the store enumerated in"
    );
}

/// The listing keeps certificates that cannot sign, instead of filtering them away.
///
/// A key-less certificate is a fact ("this store holds this certificate, but not its key") that
/// the UI can state. Dropping it silently would leave the user with a shorter list and no reason,
/// which is indistinguishable from the certificate never having been installed.
#[test]
fn a_certificate_without_a_key_is_kept_and_excluded_from_signing() {
    let listing = listing_from(&[StoreEntry {
        der: fixture_der(),
        has_private_key: false,
    }]);

    assert_eq!(
        listing.certificates.len(),
        1,
        "the certificate must still be described; the key is a separate fact"
    );
    assert!(
        listing.signable_at(during_fixture_validity()).is_empty(),
        "it must not be offered as signable"
    );
    assert!(
        !listing.is_empty(),
        "a store holding a key-less certificate is not an empty store, and the UI must be able \
         to tell those two apart"
    );
}

/// A certificate that will not parse is reported, not dropped and not fatal.
///
/// One unreadable entry in a store of hundreds must neither hide the rest nor vanish: the user
/// whose own certificate is the unreadable one gets a sentence instead of silence.
#[test]
fn an_unreadable_certificate_is_reported_and_does_not_hide_the_others() {
    let listing = listing_from(&[
        StoreEntry {
            der: vec![0xDE, 0xAD, 0xBE, 0xEF],
            has_private_key: true,
        },
        StoreEntry {
            der: fixture_der(),
            has_private_key: true,
        },
    ]);

    assert_eq!(
        listing.skipped.len(),
        1,
        "the malformed entry must be reported, not swallowed"
    );
    assert_eq!(
        listing.certificates.len(),
        1,
        "the readable certificate must survive alongside it"
    );
    assert_eq!(
        listing.skipped[0].der_prefix_hex, "DEADBEEF",
        "the skipped entry must stay identifiable in a support report"
    );
    assert!(
        !listing.skipped[0].reason.is_empty(),
        "a skip with no reason is a silent skip with extra steps"
    );
}

/// `describe` refuses rather than panicking on bytes that are not a certificate.
#[test]
fn garbage_bytes_are_refused_with_a_named_error() {
    let error = describe(&[0x00, 0x01, 0x02]).expect_err("these are not a DER certificate");
    assert!(
        matches!(error, AppError::CertificateUnreadable(_)),
        "expected CertificateUnreadable, got {error:?}"
    );
    assert_eq!(error.code(), "CERTIFICATE_UNREADABLE");
}

/// The fixture's certificate, read straight from bytes, round-trips its DER unchanged.
///
/// The signer hands this DER onward, so the bytes that were described must be the bytes that were
/// parsed -- not a re-encoding that happens to be equivalent.
#[test]
fn the_described_certificate_keeps_its_original_bytes() {
    let der = fixture_der();
    let summary = describe(&der).expect("parse");
    assert_eq!(
        summary.der, der,
        "the summary must carry the exact bytes it was parsed from"
    );
}

/// A self-signed certificate is labelled once, not twice.
#[test]
fn a_self_signed_certificate_is_not_labelled_with_its_own_issuer() {
    let summary: CertificateSummary = describe(&fixture_der()).expect("parse");
    assert_eq!(
        summary.subject, summary.issuer,
        "precondition: the fixture is self-signed"
    );
    assert_eq!(
        summary.label(),
        summary.subject,
        "repeating the issuer when it equals the subject wastes the width the label needs"
    );
}

/// Off Windows, the listing refuses instead of reporting an empty store.
///
/// This is the silent-skip rule with a specific victim: a platform where no store exists would
/// otherwise render as "you have no signing certificates". That sentence is false, and the user
/// would go looking for a certificate they already have. The two outcomes must be different
/// values, not different wording over the same empty list -- so this asserts the error, and the
/// assertion would fail if `with_user_store` were "helpfully" changed to return an empty listing.
#[cfg(not(windows))]
#[test]
fn listing_refuses_off_windows_rather_than_reporting_an_empty_store() {
    let error = document_reader_lib::editor::signature::winstore::list_user_store()
        .expect_err("a platform with no Windows certificate store must refuse, not return empty");
    assert!(
        matches!(error, AppError::CertificateStoreUnavailable(_)),
        "expected CertificateStoreUnavailable, got {error:?}"
    );
    assert_eq!(
        error.code(),
        "CERTIFICATE_STORE_UNAVAILABLE",
        "the frontend switches on this string; it must not drift"
    );
    assert_ne!(
        error.code(),
        "PFX_INVALID",
        "an unavailable platform is not a malformed keystore, and the two must not share a code"
    );
}
