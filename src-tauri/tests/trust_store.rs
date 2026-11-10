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
