//! Edit-copy garbage collection (P2-D12).
//!
//! A "save edits" run writes a new file beside the original. If the user then deletes the
//! last annotation that file was describing, the copy no longer records anything the
//! original does not — so it is stale clutter in their documents folder. D12 allows it to
//! be collected, under two guards.
//!
//! This is the only code in the product that deletes a file the user can see, so the
//! guards deliberately err toward *not* deleting:
//!
//! * **Only names this product generates.** The candidate must be the exact
//!   `"{stem} (edited N).{ext}"` shape for *this* original, in the *same* directory.
//!   A user file that happens to sit next to the document is not a candidate, because
//!   guessing wrong here destroys something unrecoverable.
//! * **Only the exact stem.** `next_edited_path` builds the name from the original's stem
//!   and extension, so anything else (a renamed copy, another document's) is left alone.
//!
//! The "never saved-as" half of D12 needs no separate check: an edit copy existing *is*
//! the record that a save-as happened, and it is the only record either way.

use std::path::{Path, PathBuf};

use crate::model::AppError;

/// Deletes the edit copies belonging to `original`, after its last annotation is gone.
///
/// Returns the paths actually removed, which is what tests assert on. A missing file, or a
/// directory that cannot be read, is not an error: GC is opportunistic tidying and must
/// never fail the annotation delete that triggered it.
pub fn collect_edit_copies(original: &Path) -> Result<Vec<PathBuf>, AppError> {
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

    let entries = match std::fs::read_dir(parent) {
        Ok(entries) => entries,
        // Nothing to collect; the delete that triggered this has already succeeded.
        Err(_) => return Ok(Vec::new()),
    };

    let mut removed = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !is_edit_copy_of(&path, stem, extension) {
            continue;
        }
        if std::fs::remove_file(&path).is_ok() {
            removed.push(path);
        }
    }

    Ok(removed)
}

/// True if `candidate` is one of `"{stem} (edited N).{extension}"` for this original.
///
/// Parsed rather than prefix-matched: `"report (edited 1) notes.pdf"` and
/// `"report (edited x).pdf"` both start with the expected text but are not edit copies, and
/// a prefix check would delete them.
fn is_edit_copy_of(candidate: &Path, stem: &str, extension: &str) -> bool {
    let Some(name) = candidate.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let Some(candidate_extension) = candidate.extension().and_then(|e| e.to_str()) else {
        return false;
    };
    // Compared exactly: `next_edited_path` reuses the original's own extension, so a
    // differently-cased file was not produced by a save.
    if candidate_extension != extension {
        return false;
    }

    let Some(inner) = name
        .strip_prefix(stem)
        .and_then(|rest| rest.strip_suffix(&format!(".{extension}")))
    else {
        return false;
    };
    let Some(number) = inner
        .strip_prefix(" (edited ")
        .and_then(|rest| rest.strip_suffix(')'))
    else {
        return false;
    };

    // Digits only, and no leading zeroes: `(edited 1)` is ours, `(edited 01)` is not.
    !number.is_empty() && number.chars().all(|c| c.is_ascii_digit()) && !number.starts_with('0')
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The predicate is tested directly as well as through the filesystem: its failure
    /// mode is deleting a user's file.
    #[test]
    fn only_the_generated_filename_shape_matches() {
        let stem = "report";
        let extension = "pdf";

        assert!(is_edit_copy_of(
            Path::new("/d/report (edited 1).pdf"),
            stem,
            extension
        ));
        assert!(is_edit_copy_of(
            Path::new("/d/report (edited 12).pdf"),
            stem,
            extension
        ));

        assert!(
            !is_edit_copy_of(Path::new("/d/report.pdf"), stem, extension),
            "the original itself"
        );
        assert!(
            !is_edit_copy_of(Path::new("/d/report (edited 1) notes.pdf"), stem, extension),
            "a trailing word means this is not our file"
        );
        assert!(
            !is_edit_copy_of(Path::new("/d/report (edited x).pdf"), stem, extension),
            "non-numeric version"
        );
        assert!(
            !is_edit_copy_of(Path::new("/d/report (edited 01).pdf"), stem, extension),
            "a leading zero is not a name we generate"
        );
        assert!(
            !is_edit_copy_of(Path::new("/d/report (edited 1).txt"), stem, extension),
            "the extension must match the original's"
        );
        assert!(
            !is_edit_copy_of(Path::new("/d/report final (edited 1).pdf"), stem, extension),
            "a different stem"
        );
    }

    #[test]
    fn a_document_whose_stem_contains_edited_is_not_its_own_copy() {
        // The stem is stripped first, so `report (edited 1).pdf` as an *original* never
        // matches plain `report`. A naive "contains (edited" check would delete it.
        assert!(!is_edit_copy_of(
            Path::new("/d/report (edited 1).pdf"),
            "report (edited 1)",
            "pdf"
        ));
        assert!(is_edit_copy_of(
            Path::new("/d/report (edited 1) (edited 2).pdf"),
            "report (edited 1)",
            "pdf"
        ));
    }
}
