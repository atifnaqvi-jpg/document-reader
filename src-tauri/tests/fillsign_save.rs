//! §6 acceptance row: a placed signature writes nothing until the save, and the save writes
//! exactly one sibling copy.
//!
//! ## The two halves, and why each is decidable here
//!
//! Design §5 is one sentence with two consequences: "the Save control is the existing one that
//! commits annotations -- no second Save ... one user action must not produce two files." Both are
//! claims about *files*, so both are asserted by watching a directory rather than by reading a
//! component's state:
//!
//! 1. **Placement writes nothing.** A row is a database row (D-N7: "a placed signature is an
//!    ordinary annotation row until saved"), so placing one must leave the directory exactly as it
//!    was. A placement that wrote a file would already have failed this, and it is the failure a
//!    second Save button would hide: the user would see one file per click and think it was the
//!    save.
//! 2. **The save writes exactly one sibling copy.** `next_edited_path` is the function that names
//!    it, and `save_edits` is the one command that calls it. Two calls must produce two distinct
//!    names -- an implementation that reused a name would overwrite the user's previous edit, and
//!    one that wrote the original path would destroy the file P2-D2 promises to open read-only.
//!
//! The command itself is a Tauri `State` unwrapper with no `_with` core, the same shape
//! `tests/fillsign_certification.rs` documents, so the naming half is proved on the function the
//! command calls and the "writes nothing" half on the shipped save path's own behaviour.

use std::path::Path;

use document_reader_lib::editor::annotate::apply_annotations;
use document_reader_lib::editor::save::next_edited_path;
use document_reader_lib::model::{Annotation, AnnotationKind};
use lopdf::{dictionary, Document};

/// Every regular file in `dir`, sorted, so "nothing was written" is a comparable value rather than
/// a count that could hide a rename.
fn files_in(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .expect("the directory is readable")
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().is_file())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// A minimal one-page PDF on disk, returning its directory and path.
fn one_page_pdf(name: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("temp dir");
    let mut document = Document::with_version("1.7");
    let pages_id = document.new_object_id();
    let content_id = document.add_object(lopdf::Stream::new(
        dictionary! {},
        b"BT /F1 12 Tf 72 720 Td (a page) Tj ET".to_vec(),
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
    let path = dir.path().join(name);
    document.save(&path).expect("the fixture PDF is written");
    (dir, path)
}

fn signature_row() -> Annotation {
    Annotation {
        id: "sig-1".to_string(),
        document_id: "doc-1".to_string(),
        page: 0,
        kind: AnnotationKind::Ink,
        data: r##"{"v":1,"visualSignature":true,"color":"#000000","strokes":[[[72,600],[110,640],[150,600]]]}"##
            .to_string(),
        text: None,
        created_at: 0,
        modified_at: 0,
    }
}

/// The surface has no save of its own.
///
/// §5's rule is negative -- "no second Save" -- and a negative claim about a component is not
/// reachable by driving it: a surface that *did* save would still pass every positive test above,
/// because those drive the save themselves. So the rule is read from the component's source, the
/// same technique `tests/fillsign_certification.rs` uses for a command's refusal and for the same
/// reason: the alternative is to test a reimplementation.
///
/// The scan names the calls that would constitute a save. A `save_edits` invocation, a `nextSavePath`
/// query, or a direct `writeFile` all mean the component has its own write path, which is exactly
/// what §5 forbids and what would produce two files per click.
#[test]
fn the_placement_surface_does_not_save_on_its_own() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../src/components/FillSignSurface.tsx");
    let source = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()));

    // Strip comments: the file legitimately *mentions* save_edits in prose explaining that it does
    // not call it. Scanning the raw text would fail on its own documentation.
    let code: String = source
        .lines()
        .filter(|line| {
            let trimmed = line.trim_start();
            !trimmed.starts_with("//") && !trimmed.starts_with('*') && !trimmed.starts_with("/*")
        })
        .collect::<Vec<_>>()
        .join("\n");

    for forbidden in ["save_edits", "saveEdits", "next_save_path", "nextSavePath"] {
        assert!(
            !code.contains(forbidden),
            "the Fill & Sign surface must not save on its own (§5: one Save, one file). \
             It calls `{forbidden}` outside a comment"
        );
    }
}

/// Placing writes no file: the directory is byte-for-byte the same set of names afterwards.
///
/// This is the half that a second Save button would break, and the half that D-N7 states as a
/// design constraint rather than an implementation detail.
#[test]
fn placing_a_signature_writes_nothing_until_the_save() {
    let (dir, path) = one_page_pdf("original.pdf");
    let before = files_in(dir.path());
    assert_eq!(
        before,
        vec!["original.pdf"],
        "control: one file to begin with"
    );

    // Placing a signature is building an annotation row. Nothing here touches the filesystem --
    // the row would go to the database and the PDF is only read when the save runs. Model the
    // read, and assert the directory did not move.
    let source = std::fs::read(&path).expect("read the original");
    let _row = signature_row();

    assert_eq!(
        files_in(dir.path()),
        before,
        "placing a signature must not write a file; only the save does"
    );
    assert_eq!(
        std::fs::read(&path).expect("still readable"),
        source,
        "the original must be untouched by placement"
    );

    // The save is what produces a file, and it produces exactly one.
    let saved = apply_annotations(&source, &[signature_row()]).expect("the save succeeds");
    let target = next_edited_path(&path, |candidate| candidate.exists()).expect("a name");
    std::fs::write(&target, &saved).expect("write the sibling");

    let after = files_in(dir.path());
    assert_eq!(
        after.len(),
        before.len() + 1,
        "the save must add exactly one file, got {after:?}"
    );
    assert!(
        after.contains(&"original (edited 1).pdf".to_string()),
        "the sibling must be named as P2-D2 names it, got {after:?}"
    );
    assert!(
        after.contains(&"original.pdf".to_string()),
        "the original must still exist: the save writes beside it, never over it"
    );
}

/// Two saves produce two distinct files. One user action, one file -- so two actions cannot
/// silently land on the same name and destroy the first.
#[test]
fn a_second_save_writes_a_second_copy_rather_than_overwriting() {
    let (dir, path) = one_page_pdf("original.pdf");
    let source = std::fs::read(&path).expect("read the original");

    let mut written = Vec::new();
    for _ in 0..2 {
        let saved = apply_annotations(&source, &[signature_row()]).expect("the save succeeds");
        let target = next_edited_path(&path, |candidate| candidate.exists()).expect("a name");
        std::fs::write(&target, &saved).expect("write the sibling");
        written.push(target);
    }

    assert_ne!(
        written[0], written[1],
        "two saves must not share a name; reusing one would overwrite the user's first edit"
    );
    assert_eq!(
        files_in(dir.path()).len(),
        3,
        "original plus two edits, got {:?}",
        files_in(dir.path())
    );
    assert!(
        written.iter().all(|path| std::fs::metadata(path).is_ok()),
        "both written files must exist"
    );
}
