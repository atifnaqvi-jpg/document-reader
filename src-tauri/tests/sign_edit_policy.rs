//! I-SG5: a certification level is a **policy**, not a label.
//!
//! The design's own test for I-SG5 is: *"make a permitted/disallowed edit per level and assert the
//! badge."* This file does exactly that, at the level the shipped code decides at.
//!
//! Two halves, and they fail for different reasons:
//!
//! 1. **The policy** (in-process, no file): `editpolicy::permits` is a total function of a level and
//!    an edit kind, so the complete normative matrix is asserted without a PDF. That is where the
//!    table itself is proved.
//! 2. **The oracle** (real signed files): for each level, one edit the level permits and one it
//!    forbids, each followed by a badge read on a document really certified at that level. This is
//!    the half that would catch a policy that is right in isolation and wired to the wrong edit —
//!    the failure mode a pure-function matrix cannot see.
//!
//! ## What the oracle can and cannot assert, and why
//!
//! The permitted edit asserts the *enforcement* answer directly: the command-layer helper
//! (`editpolicy::check`) returns `Ok`, so the edit proceeds.
//!
//! The forbidden edit asserts two things: `check` refuses it with the level named, **and** the file
//! reads as `modified`. The second is not redundant. It makes the level mean something to a third
//! party: our refusal is a courtesy to the user, while the badge going `modified` is what any other
//! reader reports. A test that only asserted our own refusal could pass while the level was
//! decorative in the file.
//!
//! The disallowed edit applied here is an appended revision — the honest, structural form of "someone
//! changed this after certification", and what a reader detects as `modified`. It does not need the
//! annotation or redaction writers, which are separate units. The commands that *would* make such an
//! edit are gated on the level read from the file; `tests/certification_gate.rs` proves each one
//! actually calls the gate.

use document_reader_lib::editor::signature::badge::Badge;
use document_reader_lib::editor::signature::editpolicy::{
    check, permits, CertificationLevel, EditKind, Verdict,
};
use document_reader_lib::editor::signature::pfx::{import_bytes, ImportedIdentity};
use document_reader_lib::editor::signature::sign::{sign_save_with, Certification, SignOptions};
use document_reader_lib::editor::signature::{store, verify};
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

/// A minimal one-page document to certify.
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
        "Resources" => dictionary! {
            "Font" => dictionary! {
                "F1" => dictionary! {
                    "Type" => "Font",
                    "Subtype" => "Type1",
                    "BaseFont" => "Helvetica",
                },
            },
        },
    });
    document.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => vec![Object::Reference(page_id)],
            "Count" => 1,
        }),
    );
    let catalog_id = document.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    document.trailer.set("Root", catalog_id);
    document.save(&path).expect("the source PDF is written");
    (dir, path)
}

/// Certify a fresh document at `level` and return the bytes that were written.
fn certified(level: i64) -> (tempfile::TempDir, Vec<u8>) {
    let (dir, source) = source_on_disk("certified.pdf", "I-SG5");
    let outcome = sign_save_with(
        &source,
        &fixture_identity(),
        SignOptions {
            certification: Some(Certification::doc_mdp(level).expect("1..=3")),
            ..SignOptions::default()
        },
    )
    .expect("certify the document");
    let written = std::fs::read(&outcome.path).expect("read the certified file");
    (dir, written)
}

/// Append a revision to `written`: the structural shape of an edit made after signing.
///
/// A real incremental update, not bytes glued to the tail. A tail append would move the file's length
/// without changing anything a reader applies, which would prove less. `IncrementalDocument::save_to`
/// writes a genuine second cross-reference section over a modified object, which is what any editor
/// (ours or another vendor's) produces.
fn append_revision(written: &[u8]) -> Vec<u8> {
    let previous = Document::load_mem(written).expect("the certified file parses");

    // Both ids come from the *previous* document: `IncrementalDocument::new_document` starts empty
    // (it holds only objects explicitly cloned into it), so asking it for its pages finds none.
    let page_id = previous
        .get_pages()
        .values()
        .copied()
        .next()
        .expect("one page");
    let contents_id = previous
        .get_dictionary(page_id)
        .expect("page dictionary")
        .get(b"Contents")
        .and_then(Object::as_reference)
        .expect("the page has a content stream");

    let mut incremental = lopdf::IncrementalDocument::create_from(written.to_vec(), previous);
    incremental
        .opt_clone_object_to_new_document(contents_id)
        .expect("clone the content stream into the new revision");
    let replacement = lopdf::Stream::new(
        dictionary! {},
        b"BT /F1 24 Tf 72 720 Td (I-SG5, changed) Tj ET".to_vec(),
    );
    incremental
        .new_document
        .objects
        .insert(contents_id, replacement.into());

    // The same writer `sign.rs` uses for its signing revision: it appends a second cross-reference
    // section after the original bytes, which is exactly the shape `later_revision` reads.
    let mut out = Vec::new();
    incremental.save_to(&mut out).expect("append the revision");
    out
}

#[test]
fn each_level_permits_its_own_edit_and_forbids_the_next_wider_one() {
    // The three rows of ISO 32000-1 §12.8.2.2, as the *widest* kind each level allows. Naming the
    // permitted edit per level and taking the forbidden one to be the next kind wider is what makes
    // the pairs meaningful: `P=1` permits only signatures, so its forbidden edit is an annotation;
    // `P=2` permits form filling but not annotation; `P=3` permits annotation but not content.
    let cases = [
        (1_i64, EditKind::Signature, EditKind::Annotation),
        (2, EditKind::FormFill, EditKind::Annotation),
        (3, EditKind::Annotation, EditKind::Content),
    ];

    for (number, permitted_edit, forbidden_edit) in cases {
        let (_dir, written) = certified(number);
        let level = verify::certification_level(&written)
            .unwrap_or_else(|| panic!("P={number} must read back as a level"));

        assert_eq!(
            level.number(),
            number as u8,
            "the file must read back as the level that was written"
        );

        // (1) The permitted edit proceeds.
        assert_eq!(
            permits(Some(level), permitted_edit),
            Verdict::Permitted,
            "P={number} must permit {permitted_edit:?}"
        );
        assert!(
            check(Some(level), permitted_edit).is_ok(),
            "P={number} must not refuse {permitted_edit:?}"
        );

        // (2) The forbidden edit is refused, and the refusal names the level so the user can act.
        assert_eq!(
            permits(Some(level), forbidden_edit),
            Verdict::Refused,
            "P={number} must refuse {forbidden_edit:?}"
        );
        let refusal =
            check(Some(level), forbidden_edit).expect_err("the forbidden edit must be refused");
        assert!(
            refusal.to_string().contains(&format!("P={number}")),
            "the refusal must name the level; got {refusal}"
        );

        // (3) If the forbidden edit is made anyway, the badge reports it. This is the assertion that
        // makes the level mean something to a reader that never runs our code.
        let changed = append_revision(&written);
        let connection = rusqlite::Connection::open_in_memory().expect("in-memory db");
        document_reader_lib::db::schema::apply(&connection).expect("schema applies");
        // Between the fixture's validity window, so nothing here turns on clock drift.
        let assessment = store::assess(&connection, &changed, 1_800_000_000)
            .expect("the changed file parses")
            .expect("the changed file still carries a signature");
        assert_eq!(
            assessment.certification_level,
            Some(number as u8),
            "the level must survive the appended revision: it is the document's claim, not the \
             newest signature's"
        );
        assert_eq!(
            assessment.badge,
            Badge::Modified,
            "a revision appended after a P={number} certification must read as `modified`, never \
             `valid`"
        );
    }
}

/// The forbidden edit does not disturb the bytes the signature covers.
///
/// This separates "the file was changed after certification" from "the signature is broken". Both
/// produce a non-green badge, and conflating them is how a product accuses an honest amendment of
/// tampering.
#[test]
fn the_post_certification_revision_leaves_the_signed_bytes_intact() {
    let (_dir, written) = certified(1);
    let changed = append_revision(&written);

    assert!(
        changed.len() > written.len(),
        "the revision must add bytes, or this proves nothing"
    );
    assert_eq!(
        &changed[..written.len()],
        &written[..],
        "the appended revision must leave every original byte untouched; I-SG2 depends on it"
    );
    assert_ne!(
        changed, written,
        "the two files must actually differ, or the badge read elsewhere is measuring nothing"
    );
}

/// An uncertified document has no level, and every edit is permitted.
///
/// The `None` arm is the state almost every file is in. Getting it wrong refuses edits on documents
/// nobody certified — the failure a user reports as "the editor is broken", and one no certification
/// test would otherwise touch.
#[test]
fn an_uncertified_document_reports_no_level_and_permits_everything() {
    let (_dir, source) = source_on_disk("plain.pdf", "no certification");
    let written = std::fs::read(&source).expect("read the source");

    assert_eq!(
        verify::certification_level(&written),
        None,
        "a document with no /Perms /DocMDP must report no level"
    );
    for kind in [
        EditKind::Annotation,
        EditKind::FormFill,
        EditKind::Redaction,
        EditKind::Content,
        EditKind::Signature,
    ] {
        assert!(
            check(None, kind).is_ok(),
            "an uncertified document must permit {kind:?}"
        );
    }
}

/// An approval signature declares no level, so it restricts nothing.
///
/// The dangerous default is an implicit `P=1`: a user signing to say "I read this" would find their
/// own next annotation refused and their file reported as modified. D-SG6 makes certification an
/// explicit choice, and this is the test that it stays explicit at the enforcement layer.
#[test]
fn an_approval_signature_restricts_nothing() {
    let (_dir, source) = source_on_disk("letter.pdf", "approval only");
    let outcome = sign_save_with(
        &source,
        &fixture_identity(),
        SignOptions {
            certification: None,
            ..SignOptions::default()
        },
    )
    .expect("sign without certifying");
    let written = std::fs::read(&outcome.path).expect("read the signed file");

    assert_eq!(
        verify::certification_level(&written),
        None,
        "an approval signature must not read back as any certification level"
    );
    assert!(
        check(None, EditKind::Annotation).is_ok(),
        "an approval-signed document must still accept annotations"
    );
}

/// The level comes from the bytes, so two different levels cannot read back as one.
///
/// A function that always returned the same answer would satisfy "P=n reads back as n" for one `n`
/// and nothing else. This asserts the read-back distinguishes all three.
#[test]
fn the_three_levels_are_distinguishable() {
    let levels: Vec<Option<CertificationLevel>> = [1_i64, 2, 3]
        .into_iter()
        .map(|number| {
            let (_dir, written) = certified(number);
            verify::certification_level(&written)
        })
        .collect();

    assert_eq!(
        levels,
        vec![
            Some(CertificationLevel::NoChanges),
            Some(CertificationLevel::FormFillAndSignatures),
            Some(CertificationLevel::AnnotationsToo),
        ],
        "each /P value must map to its own level"
    );

    for (index, level) in levels.iter().enumerate() {
        assert_eq!(
            level.expect("a level").number(),
            index as u8 + 1,
            "the level at index {index} must be P={}",
            index + 1
        );
    }
}
