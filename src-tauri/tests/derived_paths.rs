//! Naming rules for files the page/export tools write.
//!
//! Two invariants, both about not destroying the user's work:
//!
//! * A derived file never overwrites an existing one, at any count.
//! * Derived names stay out of the `(edited N)` namespace, which the D12 GC claims as its
//!   own and may delete. A rotated copy named `report (edited 1).pdf` would be a file the
//!   app could silently remove.

use std::fs;

use document_reader_lib::commands::next_derived_path;

fn dir_with(files: &[&str]) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let original = dir.path().join("report.pdf");
    fs::write(&original, b"%PDF-1.7\n").unwrap();
    for name in files {
        fs::write(dir.path().join(name), b"existing").unwrap();
    }
    (dir, original)
}

#[test]
fn the_first_derived_name_is_used_when_nothing_is_taken() {
    let (dir, original) = dir_with(&[]);

    let target = next_derived_path(&original, "rotated").unwrap();

    assert_eq!(target.file_name().unwrap(), "report (rotated 1).pdf");
    assert_eq!(target.parent().unwrap(), dir.path());
    assert!(!target.exists(), "the returned path must be free to write");
}

#[test]
fn an_existing_derived_file_is_never_clobbered() {
    let (_dir, original) = dir_with(&["report (rotated 1).pdf", "report (rotated 2).pdf"]);

    let target = next_derived_path(&original, "rotated").unwrap();

    assert_eq!(
        target.file_name().unwrap(),
        "report (rotated 3).pdf",
        "the next number must skip the files already on disk"
    );
}

#[test]
fn derived_names_do_not_collide_with_the_gc_namespace() {
    // `(edited N)` belongs to the D12 GC. Borrowing it would make a rotated copy eligible
    // for deletion when the last annotation goes away.
    let (_dir, original) = dir_with(&["report (edited 1).pdf"]);

    let target = next_derived_path(&original, "rotated").unwrap();

    assert_eq!(target.file_name().unwrap(), "report (rotated 1).pdf");
    assert!(
        !target
            .file_name()
            .unwrap()
            .to_string_lossy()
            .contains("edited"),
        "derived files must not be GC candidates"
    );
}

#[test]
fn labels_containing_spaces_are_kept_readable() {
    let (_dir, original) = dir_with(&[]);

    let target = next_derived_path(&original, "part 1").unwrap();

    assert_eq!(target.file_name().unwrap(), "report (part 1 1).pdf");
}

#[test]
fn a_derived_file_lands_beside_the_document_not_in_the_working_directory() {
    // Derived files go next to the original, which is where the user is looking. A path
    // built relative to the process working directory would scatter copies.
    let (dir, original) = dir_with(&[]);

    let target = next_derived_path(&original, "extracted").unwrap();

    assert_eq!(target.parent().unwrap(), dir.path());
}

#[test]
fn the_original_extension_is_preserved() {
    // A PDF copied to `.PDF` must not come back as `.pdf`, which on a case-sensitive
    // filesystem is a second, differently-named file.
    let (dir, _) = dir_with(&[]);
    let upper = dir.path().join("report.PDF");
    fs::write(&upper, b"%PDF-1.7\n").unwrap();

    let target = next_derived_path(&upper, "rotated").unwrap();

    assert_eq!(target.extension().unwrap(), "PDF");
}
