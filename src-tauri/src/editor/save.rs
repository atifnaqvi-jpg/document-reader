//! Versioned, never-clobbering save paths (P2-D2).
//!
//! DECISIONS §6 says documents are stored **in place**, and P2 does not repeal
//! that: the user's original file is read in place and writing to it is an
//! explicit, separately-confirmed action. Every ordinary save produces a *new*
//! file next to the original, named `"{stem} (edited N).pdf"`, so an edit can
//! never destroy the source document.
//!
//! Saving also goes through a temp file and a rename. A half-written PDF is worse
//! than no save at all: PDFium will open it far enough to show a broken page, and
//! the user's previous good version is already gone. `rename` within a directory
//! is atomic on NTFS, so the target is either the old file or the complete new one.

use std::path::{Path, PathBuf};

use crate::model::AppError;

/// Builds the next available edit filename for `original`.
///
/// `N` starts at 1 and takes the first unused name rather than counting existing
/// files: if the user deletes `edited 2` and saves again, reusing the number is
/// what they expect, and a count-based scheme would skip to a confusingly high N.
pub fn next_edited_path(
    original: &Path,
    exists: impl Fn(&Path) -> bool,
) -> Result<PathBuf, AppError> {
    let parent = original
        .parent()
        .ok_or_else(|| AppError::Other("document has no parent directory".to_string()))?;
    let stem = original
        .file_stem()
        .and_then(|stem| stem.to_str())
        .ok_or_else(|| AppError::Other("document name is not valid utf-8".to_string()))?;
    let extension = original
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or("pdf");

    for number in 1..=MAX_EDIT_VERSIONS {
        let candidate = parent.join(format!("{stem} (edited {number}).{extension}"));
        if !exists(&candidate) {
            return Ok(candidate);
        }
    }

    Err(AppError::Other(format!(
        "reached the limit of {MAX_EDIT_VERSIONS} edited copies for this document"
    )))
}

/// Upper bound on versions, so a runaway loop cannot generate unbounded filenames.
const MAX_EDIT_VERSIONS: u32 = 9_999;

/// Writes `bytes` to `target` atomically: full write to a sibling temp file,
/// flush, then rename over the target.
///
/// The temp file must be a sibling, not in the system temp directory: `rename` is
/// only atomic within one filesystem, and `C:\Temp` is frequently a different
/// volume from the user's documents folder, which would silently degrade to a
/// copy-then-delete and reintroduce the partial-write window.
pub fn write_atomically(target: &Path, bytes: &[u8]) -> Result<(), AppError> {
    let parent = target
        .parent()
        .ok_or_else(|| AppError::Other("target has no parent directory".to_string()))?;
    std::fs::create_dir_all(parent).map_err(|error| AppError::Other(error.to_string()))?;

    let file_name = target
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| AppError::Other("target name is not valid utf-8".to_string()))?;
    let temp = parent.join(format!(".{file_name}.{}.tmp", uuid::Uuid::new_v4()));

    let write_result = (|| -> std::io::Result<()> {
        use std::io::Write;
        let mut file = std::fs::File::create(&temp)?;
        file.write_all(bytes)?;
        // Flush to the filesystem before the rename: on a crash, a renamed file
        // with unwritten buffers is exactly the corruption this function exists
        // to prevent.
        file.sync_all()?;
        Ok(())
    })();

    if let Err(error) = write_result {
        let _ = std::fs::remove_file(&temp);
        return Err(AppError::Other(format!("could not write file: {error}")));
    }

    if let Err(error) = std::fs::rename(&temp, target) {
        let _ = std::fs::remove_file(&temp);
        return Err(AppError::Other(format!(
            "could not replace {}: {error}",
            target.display()
        )));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_save_uses_version_one() {
        let path = next_edited_path(Path::new("C:/docs/report.pdf"), |_| false).unwrap();
        assert_eq!(path, PathBuf::from("C:/docs/report (edited 1).pdf"));
    }

    #[test]
    fn skips_to_the_first_free_version() {
        let path = next_edited_path(Path::new("C:/docs/report.pdf"), |candidate| {
            candidate.file_name().unwrap() == "report (edited 1).pdf"
        })
        .unwrap();
        assert_eq!(path, PathBuf::from("C:/docs/report (edited 2).pdf"));
    }

    #[test]
    fn reuses_a_number_after_a_file_is_deleted() {
        // Only version 1 and 3 exist, so 2 is the answer even though 3 exists.
        let path = next_edited_path(Path::new("C:/docs/report.pdf"), |candidate| {
            let name = candidate.file_name().unwrap().to_str().unwrap();
            name == "report (edited 1).pdf" || name == "report (edited 3).pdf"
        })
        .unwrap();
        assert_eq!(path, PathBuf::from("C:/docs/report (edited 2).pdf"));
    }

    #[test]
    fn keeps_the_original_extension_and_directory() {
        let path = next_edited_path(Path::new("C:/a/b/notes.PDF"), |_| false).unwrap();
        // Extension case is preserved so the file still associates with viewers.
        assert_eq!(path, PathBuf::from("C:/a/b/notes (edited 1).PDF"));
    }

    #[test]
    fn edited_name_never_equals_the_original() {
        // The whole point of D2.
        let original = Path::new("C:/docs/report.pdf");
        let path = next_edited_path(original, |_| false).unwrap();
        assert_ne!(path, original);
    }

    #[test]
    fn gives_up_instead_of_looping_forever() {
        let error = next_edited_path(Path::new("C:/docs/report.pdf"), |_| true).unwrap_err();
        assert!(error.to_string().contains("limit"));
    }

    #[test]
    fn atomic_write_replaces_existing_content() {
        let dir = std::env::temp_dir().join(format!("dr-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("out.pdf");

        write_atomically(&target, b"first").unwrap();
        write_atomically(&target, b"second").unwrap();

        assert_eq!(std::fs::read(&target).unwrap(), b"second");
        // No temp files should be left behind.
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "temp file leaked: {leftovers:?}");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn atomic_write_creates_missing_directories() {
        let dir = std::env::temp_dir().join(format!("dr-test-{}", uuid::Uuid::new_v4()));
        let target = dir.join("nested").join("out.pdf");
        write_atomically(&target, b"data").unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"data");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn atomic_write_leaves_no_partial_file_on_failure() {
        // A directory as the target makes the rename fail; the temp file must not
        // survive as debris next to the user's documents.
        let dir = std::env::temp_dir().join(format!("dr-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("target.pdf")).unwrap();

        let result = write_atomically(&dir.join("target.pdf"), b"data");
        assert!(result.is_err());

        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "temp file leaked: {leftovers:?}");

        std::fs::remove_dir_all(&dir).ok();
    }
}
