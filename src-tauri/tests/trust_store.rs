//! The trust store, end to end, on real files (P3.2 D-SG4).
//!
//! These tests sign an actual PDF with an actual PKCS#12 that a real CA issued, then ask the
//! shipped path (`store::assess`) what badge the file deserves. The point is not that the deciding
//! functions work -- `badge.rs` and `revocation.rs` already prove that -- but that **the inputs
//! they are given in production are real**: that `anchored` is computed from a verified path to a
//! certificate the user imported, not passed as a constant, and that revocation comes from data
//! that exists rather than from `None`.
//!
//! The single most important test here is
//! [`the_same_signed_file_is_unknown_without_the_anchor_and_valid_with_it`]. It holds the file
//! fixed and changes only the store, so the two badges differ for exactly one reason. Before this
//! unit, the "before" half was unreachable -- the command passed `anchored: false` as a literal --
//! which is why the `valid` state had never been produced by the shipped path on a real file.

use document_reader_lib::editor::signature::badge::{Badge, Integrity};
use document_reader_lib::editor::signature::pfx::import_bytes;
use document_reader_lib::editor::signature::pfx::ImportedIdentity;
use document_reader_lib::editor::signature::revocation::{
    NotCheckedReason, Revocation, RevocationSource,
};
use document_reader_lib::editor::signature::sign::sign_save;
use document_reader_lib::editor::signature::store;
use lopdf::{dictionary, Document};

const PFX_PASSWORD: &str = "correct-horse";

/// Read one fixture by name.
fn fixture(name: &str) -> Vec<u8> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/signing/trust")
        .join(name);
    std::fs::read(&path).unwrap_or_else(|error| panic!("fixture {name}: {error}"))
}

/// The leaf identity: issued by `ca.der`, so a chain to that anchor is buildable.
fn leaf_identity() -> ImportedIdentity {
    import_bytes(&fixture("leaf.pfx"), PFX_PASSWORD)
        .expect("the leaf PFX opens")
        .identities
        .into_iter()
        .next()
        .expect("the leaf PFX carries one identity")
}

/// Sign a small PDF with `identity` and return the written bytes.
fn signed_with(identity: &ImportedIdentity, text: &str) -> (tempfile::TempDir, Vec<u8>) {
    let dir = tempfile::tempdir().expect("temp dir");
    let source = dir.path().join("unsigned.pdf");

    let mut document = Document::with_version("1.7");
    let pages_id = document.new_object_id();
    let content_id = document.add_object(lopdf::Stream::new(
        dictionary! {},
        format!("BT /F1 24 Tf 72 720 Td ({text}) Tj ET").into_bytes(),
    ));
    let page_id = document.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "Contents" => content_id,
    });
    document.objects.insert(
        pages_id,
        lopdf::Object::Dictionary(dictionary! {
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
    document.save(&source).expect("the source PDF is written");

    let outcome = sign_save(&source, identity).expect("signing succeeds");
    let written = std::fs::read(&outcome.path).expect("read the signed file");
    (dir, written)
}

/// An in-memory database with this tree's schema applied.
fn store_db() -> rusqlite::Connection {
    let connection = rusqlite::Connection::open_in_memory().expect("in-memory db");
    document_reader_lib::db::schema::apply(&connection).expect("schema applies");
    connection
}

#[test]
fn the_same_signed_file_is_unknown_without_the_anchor_and_valid_with_it() {
    // One file, signed once. Only the store changes between the two assessments.
    let (_dir, written) = signed_with(&leaf_identity(), "one file, two answers");
    let connection = store_db();
    // Between the fixture's own validity window, so nothing here is decided by clock drift.
    let now = 1_800_000_000;

    let before = store::assess(&connection, &written, now)
        .expect("the file parses")
        .expect("the file carries a signature");

    assert_eq!(
        before.integrity,
        Integrity::Intact,
        "the signature must verify before trust is even considered, or the two halves of this \
         test are measuring different things"
    );
    assert!(!before.anchored, "nothing was imported");
    assert_eq!(
        before.badge,
        Badge::Unknown,
        "an intact signature with no anchor is `unknown`; D-SG4 forbids calling it `valid`"
    );
    assert!(
        before.reason.contains("no trust anchors are imported"),
        "the UI must be able to say why it does not know; got {:?}",
        before.reason
    );

    // Now import the CA that issued the signer's certificate, and a clean CRL that covers it:
    // the two things a user does to move a signature from "unknown" to "valid".
    let anchor = store::add_anchor(&connection, &fixture("ca.der"), now).expect("anchor imports");
    assert_eq!(anchor.subject, "C=GB,O=DocReader Test,CN=DocReader Test CA");
    store::add_crl(&connection, &fixture("crl-empty.der"), now).expect("crl imports");

    let after = store::assess(&connection, &written, now)
        .expect("the file parses")
        .expect("the file carries a signature");

    assert!(
        after.anchored,
        "the signer's certificate was issued by the imported anchor, so a verified path exists"
    );
    assert_eq!(
        after.chain_depth,
        Some(1),
        "the leaf was signed directly by the anchor"
    );
    assert_eq!(
        after.badge,
        Badge::Valid,
        "same bytes, same signature, an anchor the user chose and a current clean list: this is \
         the state that was unreachable while `anchored` was a constant"
    );
}

#[test]
fn an_anchor_alone_is_not_enough_for_a_valid_badge() {
    // The distinction D-SG4 exists to make. Anchoring proves *who* signed; it says nothing about
    // whether that certificate has since been revoked. With no revocation data the honest badge is
    // `unknown`, and this test pins that anchoring cannot be traded for a green badge.
    let (_dir, written) = signed_with(&leaf_identity(), "anchor only");
    let connection = store_db();
    let now = 1_800_000_000;
    store::add_anchor(&connection, &fixture("ca.der"), now).expect("anchor imports");

    let assessment = store::assess(&connection, &written, now)
        .expect("the file parses")
        .expect("the file carries a signature");

    assert!(assessment.anchored, "the chain does reach the anchor");
    assert_eq!(
        assessment.revocation,
        Revocation::NotChecked {
            reason: NotCheckedReason::NoData
        }
    );
    assert_eq!(
        assessment.badge,
        Badge::Unknown,
        "an anchored signature with unchecked revocation is `unknown`, not `valid`"
    );
}

#[test]
fn importing_the_wrong_anchor_leaves_the_file_unknown() {
    // Both certificates are real and both parse. Only the signature edge is missing.
    let (_dir, written) = signed_with(&leaf_identity(), "wrong anchor");
    let connection = store_db();
    let now = 1_800_000_000;

    store::add_anchor(&connection, &fixture("other-ca.der"), now).expect("anchor imports");

    let assessment = store::assess(&connection, &written, now)
        .expect("the file parses")
        .expect("the file carries a signature");

    assert!(
        !assessment.anchored,
        "an unrelated CA did not issue this signer"
    );
    assert_eq!(assessment.badge, Badge::Unknown);
    assert!(
        assessment
            .reason
            .contains("does not chain to any imported anchor"),
        "the reason must distinguish this from an empty store; got {:?}",
        assessment.reason
    );
}

#[test]
fn an_imported_anchor_that_is_the_signer_itself_anchors_without_a_chain() {
    // The self-signed case: the user imports exactly the certificate the file was signed with.
    // This is the ordinary way a user trusts their own key.
    let identity = leaf_identity();
    let (_dir, written) = signed_with(&identity, "self anchored");
    let connection = store_db();
    let now = 1_800_000_000;

    // The signer's own certificate, from the chain the PFX carried.
    let signer_der = identity
        .chain
        .first()
        .expect("the identity has a certificate");
    store::add_anchor(&connection, signer_der, now).expect("anchor imports");
    store::add_crl(&connection, &fixture("crl-empty.der"), now).expect("crl imports");

    let assessment = store::assess(&connection, &written, now)
        .expect("the file parses")
        .expect("the file carries a signature");

    assert!(assessment.anchored);
    assert_eq!(
        assessment.chain_depth,
        Some(0),
        "a certificate that is its own anchor needs no issuing step"
    );
    assert_eq!(assessment.badge, Badge::Valid);
}

#[test]
fn a_tampered_document_is_modified_even_with_the_anchor_imported() {
    // Trust must not outrank damage. This is the ordering `badge::decide` already proves; here it
    // is proved through the shipped path, on a real file, with a real anchor.
    let (_dir, mut written) = signed_with(&leaf_identity(), "tamper me");
    let connection = store_db();
    let now = 1_800_000_000;
    store::add_anchor(&connection, &fixture("ca.der"), now).expect("anchor imports");

    // Flip one byte of the visible text. A structural byte would break parsing instead, which
    // would prove nothing about the digest: the file must still be a readable PDF after the change.
    let needle = b"tamper me";
    let at = written
        .windows(needle.len())
        .position(|window| window == needle)
        .expect("the text is in the file");
    written[at] = b'T';

    let assessment = store::assess(&connection, &written, now)
        .expect("the tampered file still parses")
        .expect("the signature dictionary is still there");

    assert_eq!(assessment.integrity, Integrity::Damaged);
    assert_eq!(
        assessment.badge,
        Badge::Modified,
        "damage outranks trust; a valid anchor must not launder a changed byte"
    );
}

/// An otherwise-`valid` file stops being `valid` when something is appended after the signature.
///
/// This is the combination the existing tests each held still but never crossed. `sign_verify` shows
/// that appended bytes are reported as a later revision -- but with `anchored: false`, so the badge
/// ends up `unknown` there whether or not `later_revision` was consulted. The test above shows
/// damage outranks trust, but it *changes a signed byte*, which sets `Damaged` and short-circuits the
/// same `if` in `badge::decide`. So in every test that existed, a `store::assess` that dropped
/// `later_revision` on the way in would still have passed all of them.
///
/// Here the file is anchored, the CRL is current and clean, and the signed range is untouched: the
/// only thing between this file and a green badge is the appendix. That makes `valid` observable
/// immediately before the append and `modified` immediately after, on one file, with one variable.
#[test]
fn an_anchored_clean_signature_stops_being_valid_when_a_revision_is_appended() {
    let (_dir, written) = signed_with(&leaf_identity(), "appendix target");
    let connection = store_db();
    let now = 1_800_000_000;
    store::add_anchor(&connection, &fixture("ca.der"), now).expect("anchor imports");
    store::add_crl(&connection, &fixture("crl-empty.der"), now).expect("crl imports");

    // The control. Without this half, the assertion below would pass on a file that was never
    // `valid` in the first place, and the test would prove nothing about the append.
    let before = store::assess(&connection, &written, now)
        .expect("the signed file parses")
        .expect("the file carries a signature");
    assert_eq!(
        before.badge,
        Badge::Valid,
        "the control must start green, or the append is not what changed the badge"
    );

    // An ordinary incremental update -- the shape of "someone saved this document again", which is
    // what actually happens to a signed file in the wild. It appends bytes and leaves the signed
    // range exactly as it was.
    let mut appended = written.clone();
    appended.extend_from_slice(
        b"\n% appended after signing: an ordinary incremental update, not part of any signature\n",
    );

    let after = store::assess(&connection, &appended, now)
        .expect("the appended file still parses")
        .expect("the signature dictionary is still there");

    assert_eq!(
        after.integrity,
        Integrity::Intact,
        "the signed range was not touched, so integrity must still verify: this is precisely why \
         `later_revision` cannot be folded into it"
    );
    assert!(
        after.anchored,
        "the appended bytes do not disturb the chain, so the anchor still holds"
    );
    assert_eq!(
        after.badge,
        Badge::Modified,
        "an anchored, cryptographically intact signature over a file that has since grown is \
         `modified`, not `valid`: the file the reader holds is no longer the file that was signed"
    );
}

#[test]
fn revocation_with_no_data_anywhere_is_not_checked_rather_than_clean() {
    // The offline default, through the shipped path. `NotChecked` is what makes the badge
    // `unknown` even when anchoring succeeded -- silence is not a pass.
    let (_dir, written) = signed_with(&leaf_identity(), "no crl");
    let connection = store_db();
    let now = 1_800_000_000;
    store::add_anchor(&connection, &fixture("ca.der"), now).expect("anchor imports");

    let assessment = store::assess(&connection, &written, now)
        .expect("the file parses")
        .expect("the file carries a signature");

    assert!(assessment.anchored);
    assert_eq!(
        assessment.revocation,
        Revocation::NotChecked {
            reason: NotCheckedReason::NoData
        },
        "nothing supplied revocation data, so nothing may claim the certificate is clean"
    );
}

#[test]
fn an_imported_clean_crl_is_read_as_not_revoked_from_the_imported_store() {
    // The positive revocation path: a CRL the user imported covers the signer and does not name it.
    let (_dir, written) = signed_with(&leaf_identity(), "clean crl");
    let connection = store_db();
    let now = 1_800_000_000;
    store::add_anchor(&connection, &fixture("ca.der"), now).expect("anchor imports");
    store::add_crl(&connection, &fixture("crl-empty.der"), now).expect("crl imports");

    let assessment = store::assess(&connection, &written, now)
        .expect("the file parses")
        .expect("the file carries a signature");

    assert_eq!(
        assessment.revocation,
        Revocation::NotRevoked {
            source: RevocationSource::ImportedStore
        },
        "the user's own list must be attributed to the imported store, not to the document"
    );
    assert_eq!(assessment.badge, Badge::Valid);
}

#[test]
fn an_imported_crl_that_names_the_signer_blocks_the_valid_badge() {
    // The negative that matters most: a revoked certificate must never read `valid`, even though
    // its signature is intact and its chain anchors.
    let (_dir, written) = signed_with(&leaf_identity(), "revoked");
    let connection = store_db();
    let now = 1_800_000_000;
    store::add_anchor(&connection, &fixture("ca.der"), now).expect("anchor imports");
    store::add_crl(&connection, &fixture("crl-revoked.der"), now).expect("crl imports");

    let assessment = store::assess(&connection, &written, now)
        .expect("the file parses")
        .expect("the file carries a signature");

    assert_eq!(
        assessment.integrity,
        Integrity::Intact,
        "the signature is still good; revocation is a separate finding"
    );
    assert!(assessment.anchored, "and the chain still anchors");
    assert_eq!(
        assessment.revocation,
        Revocation::Revoked {
            source: RevocationSource::ImportedStore
        }
    );
    assert_eq!(
        assessment.badge,
        Badge::Unknown,
        "a revoked certificate cannot be `valid`; D-SG4 names this as the strongest refusal"
    );
}

#[test]
fn an_unsigned_document_has_no_assessment_at_all() {
    // `None` rather than a fabricated `Unsigned`: the badge for "no signature" is decided
    // elsewhere, and inventing a reading here would be a second source of truth.
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("plain.pdf");
    let mut document = Document::with_version("1.7");
    let pages_id = document.new_object_id();
    document.objects.insert(
        pages_id,
        lopdf::Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => Vec::<lopdf::Object>::new(),
            "Count" => 0,
        }),
    );
    let catalog_id = document.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    document.trailer.set("Root", catalog_id);
    document.save(&path).expect("the PDF is written");
    let written = std::fs::read(&path).expect("read the PDF");

    let connection = store_db();
    assert_eq!(
        store::assess(&connection, &written, 1_800_000_000).expect("the file parses"),
        None,
        "a document with no signature dictionary has no signature to assess"
    );
}

#[test]
fn anchors_persist_and_remove_by_their_own_identity() {
    let connection = store_db();
    let now = 1_800_000_000;

    let first = store::add_anchor(&connection, &fixture("ca.der"), now).expect("anchor imports");
    let second =
        store::add_anchor(&connection, &fixture("other-ca.der"), now).expect("and another");

    let listed = store::list_anchors(&connection).expect("list");
    assert_eq!(listed.len(), 2, "both anchors are listed");

    // Importing the same anchor twice must not duplicate it.
    store::add_anchor(&connection, &fixture("ca.der"), now).expect("re-import is a no-op");
    assert_eq!(store::list_anchors(&connection).expect("list").len(), 2);

    assert!(store::remove_anchor(&connection, &first.der_base64).expect("remove"));
    assert!(
        !store::remove_anchor(&connection, &first.der_base64).expect("remove again"),
        "removing an absent anchor reports that nothing was removed"
    );
    assert!(store::has_anchor(&connection, &second.der_base64).expect("has"));
    assert_eq!(store::list_anchors(&connection).expect("list").len(), 1);
}

#[test]
fn an_unreadable_anchor_is_refused_at_import_rather_than_stored() {
    // A certificate that will not parse would silently never anchor anything; refusing it here
    // gives the user a sentence instead of a mystery.
    let connection = store_db();
    let error = store::add_anchor(&connection, b"this is not a certificate", 1_800_000_000)
        .expect_err("garbage must not import");
    assert!(
        format!("{error}").contains("could not be read"),
        "the refusal must name the reason; got {error}"
    );
    assert_eq!(
        store::list_anchors(&connection).expect("list").len(),
        0,
        "and nothing may be left behind"
    );
}

#[test]
fn an_imported_crl_is_stored_by_its_own_identity() {
    let connection = store_db();
    let now = 1_800_000_000;
    store::add_crl(&connection, &fixture("crl-empty.der"), now).expect("crl imports");
    // Re-importing is idempotent, because the DER is the key.
    store::add_crl(&connection, &fixture("crl-empty.der"), now).expect("re-import is a no-op");
    assert!(store::add_crl(&connection, b"not a crl", now).is_err());
}

// --- The import surface the product actually calls (SCOPE-C unit 4) --------------------------
//
// The tests above call `store::add_anchor` directly. That is what the end-to-end test had to do
// before this unit existed, and it is why `valid` was unreachable in the shipped product: the store
// worked, and nothing reached it. These tests go through `commands::import_anchor_bytes`, which is
// the function the `import_trust_anchor` command calls after reading the file, so the path from "a
// user picks a certificate" to "a signature can now be `valid`" is covered end to end.

use document_reader_lib::commands::{
    import_anchor_bytes, import_crl_bytes, read_import_file_within,
};

/// Encode `der` as PEM, so the import tests exercise the shape users actually have.
fn pem(label: &str, der: &[u8]) -> Vec<u8> {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut digits = String::new();
    for chunk in der.chunks(3) {
        let byte = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let triple = (u32::from(byte[0]) << 16) | (u32::from(byte[1]) << 8) | u32::from(byte[2]);
        for position in 0..4 {
            if position <= chunk.len() {
                digits.push(ALPHABET[((triple >> (18 - position * 6)) & 0x3F) as usize] as char);
            } else {
                digits.push('=');
            }
        }
    }
    let mut text = format!("-----BEGIN {label}-----\n");
    for chunk in digits.as_bytes().chunks(64) {
        text.push_str(&String::from_utf8_lossy(chunk));
        text.push('\n');
    }
    text.push_str(&format!("-----END {label}-----\n"));
    text.into_bytes()
}

#[test]
fn importing_through_the_command_reaches_the_valid_badge_on_a_real_file() {
    // The whole point of the unit, stated as one test: before it, no shipped path could produce
    // `valid`, because nothing could put an anchor in the store.
    let (_dir, written) = signed_with(&leaf_identity(), "reached through the import command");
    let connection = store_db();
    let now = 1_800_000_000;

    let before = store::assess(&connection, &written, now)
        .expect("the file parses")
        .expect("the file carries a signature");
    assert_eq!(
        before.badge,
        Badge::Unknown,
        "precondition: with nothing imported, the file cannot be `valid`"
    );

    // Exactly what the command does, on the bytes it would have read from the chosen file.
    import_anchor_bytes(&connection, &fixture("ca.der"), now).expect("the anchor imports");
    import_crl_bytes(&connection, &fixture("crl-empty.der"), now).expect("the CRL imports");

    let after = store::assess(&connection, &written, now)
        .expect("the file parses")
        .expect("the file carries a signature");
    assert_eq!(
        after.badge,
        Badge::Valid,
        "an anchored, clean, intact signature must reach `valid` through the shipped import path"
    );
}

#[test]
fn the_import_command_reports_what_it_stored_and_whether_it_was_new() {
    let connection = store_db();
    let now = 1_800_000_000;

    let first = import_anchor_bytes(&connection, &fixture("ca.der"), now).expect("imports");
    assert!(first.added, "the first import is an addition");
    assert_eq!(
        first.subject, "C=GB,O=DocReader Test,CN=DocReader Test CA",
        "the caller must be able to show which certificate it now trusts"
    );
    assert!(!first.der_base64.is_empty(), "the row identity is returned");
    // The fixture's own `notAfter` (2036-11-07 16:39:40 UTC), read from the certificate with
    // `openssl x509 -enddate` rather than copied from the README's rounded date.
    assert_eq!(
        first.not_after,
        Some(2_109_688_780),
        "the returned expiry must be the certificate's own, so a UI can warn before it lapses"
    );

    // The second import is a no-op, and must say so: "imported" twice for one anchor would let a
    // user believe they had added a second trust decision.
    let second = import_anchor_bytes(&connection, &fixture("ca.der"), now).expect("re-import ok");
    assert!(!second.added, "the same certificate is not a new anchor");
    assert_eq!(
        store::list_anchors(&connection).expect("list").len(),
        1,
        "and the store must still hold exactly one row"
    );
}

#[test]
fn a_pem_anchor_imports_through_the_command_like_its_der() {
    // The same file, in the encoding a user is most likely to have from a CA's download page.
    let connection = store_db();
    let now = 1_800_000_000;
    let imported = import_anchor_bytes(&connection, &pem("CERTIFICATE", &fixture("ca.der")), now)
        .expect("a PEM certificate imports");
    assert!(imported.added);
    assert_eq!(
        imported.der_base64,
        import_anchor_bytes(&store_db(), &fixture("ca.der"), now)
            .expect("imports")
            .der_base64,
        "PEM and DER of one certificate must produce one identity, or a store would hold both"
    );
}

#[test]
fn an_unreadable_import_is_refused_with_a_code_the_ui_can_read() {
    let connection = store_db();
    let now = 1_800_000_000;

    let error = import_anchor_bytes(&connection, b"not a certificate", now)
        .expect_err("garbage must not import");
    assert_eq!(
        error.code(),
        "CERTIFICATE_UNREADABLE",
        "the frontend branches on this code; `OTHER` would render as a defect"
    );
    assert_eq!(
        store::list_anchors(&connection).expect("list").len(),
        0,
        "and nothing may be written for a file that was refused"
    );

    let error =
        import_crl_bytes(&connection, b"not a crl", now).expect_err("garbage must not import");
    assert_eq!(error.code(), "CERTIFICATE_UNREADABLE");
}

#[test]
fn importing_a_crl_reports_the_expiry_that_decides_when_it_stops_being_evidence() {
    // `nextUpdate`, not `thisUpdate`. A CRL whose reported expiry was the day it was issued would
    // look permanently fresh to any caller that used this field, which is the field a UI reads to
    // warn "this revocation list is out of date". The mutant that reported `thisUpdate` survived
    // the rest of this file, so this test exists to be the one that dies.
    let connection = store_db();
    let now = 1_800_000_000;

    let crl = import_crl_bytes(&connection, &fixture("crl-empty.der"), now).expect("CRL imports");
    assert!(crl.added, "the first import is an addition");
    assert_eq!(
        crl.subject, "C=GB,O=DocReader Test,CN=DocReader Test CA",
        "the issuer is reported so the UI can say whose list this is"
    );

    // The fixture's own timestamps, read from the file with `openssl crl -lastupdate -nextupdate`.
    let this_update = 1_794_328_781; // 2026-11-10 16:39:41 UTC
    let next_update = 2_109_688_781; // 2036-11-07 16:39:41 UTC
    assert_eq!(
        crl.not_after,
        Some(next_update),
        "the reported expiry must be `nextUpdate`; `thisUpdate` would present a stale list as fresh"
    );
    assert_ne!(
        crl.not_after,
        Some(this_update),
        "and it must not be `thisUpdate`"
    );

    // Re-importing is a no-op, and says so.
    let again =
        import_crl_bytes(&connection, &fixture("crl-empty.der"), now).expect("re-import ok");
    assert!(!again.added, "the same CRL is not a new import");
}

#[test]
fn importing_a_crl_that_names_the_signer_suppresses_the_valid_badge() {
    // The import surface must not be able to launder a revocation: importing the *revoked* list
    // instead of the clean one has to demote the badge, through the same command a user would use.
    let (_dir, written) = signed_with(&leaf_identity(), "revoked through the import command");
    let connection = store_db();
    let now = 1_800_000_000;
    import_anchor_bytes(&connection, &fixture("ca.der"), now).expect("anchor imports");
    import_crl_bytes(&connection, &fixture("crl-revoked.der"), now).expect("revoked CRL imports");

    let assessment = store::assess(&connection, &written, now)
        .expect("the file parses")
        .expect("the file carries a signature");
    assert_ne!(
        assessment.badge,
        Badge::Valid,
        "a revocation imported by the user must never leave a green badge"
    );
}

#[test]
fn an_import_file_over_the_limit_is_refused_with_its_size_named() {
    // The command's own guard, not the store's: a user who picks a disk image or a bundle archive by
    // mistake must get a sentence rather than a stalled command and a large allocation. The limit is
    // supplied here so the test does not need to create a 16 MiB file to reach the branch.
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("bundle.pem");
    let body = b"not really a bundle, but large enough for the limit below";
    std::fs::write(&path, body).expect("write");
    let path = path.to_str().expect("utf-8 path");

    let error = read_import_file_within(path, 8).expect_err("must refuse");
    let text = format!("{error}");
    assert!(text.contains("larger than the 8 byte limit"), "got {text}");
    assert!(
        text.contains(&format!("{} bytes", body.len())),
        "the refusal must name the size so the user can see it was the wrong file; got {text}"
    );

    // And a file under the limit is read, not refused: the guard must not reject everything.
    assert_eq!(
        read_import_file_within(path, 1024)
            .expect("within the limit")
            .len(),
        body.len()
    );
    // A missing file is a read failure, not a size failure.
    let missing = read_import_file_within("/nonexistent/ca.der", 1024).expect_err("must fail");
    assert!(format!("{missing}").contains("could not read"));
}
