//! Edit-copy garbage collection — **disabled on purpose** (P2-D12).
//!
//! ## Why this module no longer deletes anything
//!
//! This used to delete `"{stem} (edited N).{ext}"` beside the original once the document's last
//! annotation was removed, on the reasoning that the copy had stopped recording anything the
//! original did not. The user overruled that on 2026-11-25, and the ruling is right:
//!
//! > A saved PDF vanishing because the last annotation was removed is data loss from the user's
//! > point of view, however it is implemented.
//!
//! The reasoning that "the copy is now redundant" is sound about *content* and wrong about
//! *things*. A file that appeared in the user's documents folder, that an explicit save announced
//! by name, and that they may have since renamed, copied elsewhere, or attached to an email, is a
//! thing they own. Deleting it because an unrelated action (removing a mark) made it redundant
//! destroys an artifact the user was told had been created. The coupling is the defect: a delete
//! nobody asked for, triggered by an edit to a different object.
//!
//! ## What a collector would be allowed to remove
//!
//! Only app-owned scratch that was *never offered to the user as a saved document*. This product
//! writes no such file: `save_edits` names its output and reports it to the UI as the saved path,
//! so every copy this module could have seen is user-visible. There is therefore nothing to
//! collect, and the honest implementation of "restrict it to app-owned temporary paths" is an
//! empty set rather than a narrower predicate.
//!
//! The seam is kept — `collect_edit_copies_if_unannotated` still runs after the last annotation
//! is deleted — so that if scratch space is ever introduced there is one place to register it.
//! Keeping the call site also keeps the *reason* visible at the point where the deletion used to
//! happen, rather than leaving a silent absence that a later reader would have to archaeologise.

use std::path::{Path, PathBuf};

use crate::model::AppError;

/// Returns the edit copies that may be deleted. Always empty: see the module docs.
///
/// The signature is unchanged so the call site and its tests keep compiling, and so a future
/// app-owned scratch area can be added here without touching the wiring.
pub fn collect_edit_copies(_original: &Path) -> Result<Vec<PathBuf>, AppError> {
    Ok(Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_copy_is_ever_a_collection_candidate() {
        // Not a tautology in intent: it pins the *policy*. If someone reintroduces a deletion
        // rule here, this fails and the reason in the module docs has to be argued with first.
        let removed = collect_edit_copies(Path::new("/documents/report.pdf")).unwrap();
        assert!(
            removed.is_empty(),
            "the collector must not name a file: every `(edited N)` copy is a document the user \
             was told had been saved (P2-D12 ruling, 2026-11-25)"
        );
    }
}
