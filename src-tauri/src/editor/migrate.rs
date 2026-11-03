//! One-shot migration of the library database across a bundle-identifier change (D-R4/D-R5).
//!
//! `app_data_dir()` resolves to `data_dir()/{bundle_identifier}`, so renaming the identifier
//! moves `library.db`. Without this, an existing install opens an empty library: silent data
//! loss for a phase that is otherwise only a rename.
//!
//! Copy, never move, never delete (D-R5). The old file stays where it is, which makes the
//! operation idempotent and reversible by hand if it ever goes wrong.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// The identifier this app shipped under before the rename. The legacy data directory is
/// `data_dir()/LEGACY_IDENTIFIER`, i.e. the sibling of the current one.
pub const LEGACY_IDENTIFIER: &str = "com.documentreader.app";

const DATABASE: &str = "library.db";

#[derive(Debug, PartialEq, Eq)]
pub enum MigrationOutcome {
    /// The destination already holds a database, or there is nothing to copy. A second launch
    /// takes this branch, which is what makes the migration idempotent (D-R5).
    NotNeeded,
    /// Copied from the legacy directory. The caller surfaces this once (D-R6).
    Migrated { from: PathBuf },
    /// A copy was attempted and failed. The destination is left untouched, so the next launch
    /// retries rather than starting from a half-written file.
    Failed { from: PathBuf, reason: String },
}

impl MigrationOutcome {
    /// The sentence the Notices surface shows, or `None` when there is nothing to say.
    pub fn notice(&self) -> Option<String> {
        match self {
            Self::Migrated { from } => Some(format!(
                "Your library was copied from an earlier installation at {}.",
                from.display()
            )),
            Self::Failed { from, reason } => Some(format!(
                "Your library could not be copied from {} ({reason}). The original is unchanged; \
                 the next launch will try again.",
                from.display()
            )),
            Self::NotNeeded => None,
        }
    }
}

/// Copy `legacy_dir/library.db` into `new_dir` when the destination has none.
///
/// Paths are injected rather than resolved here so the test never touches a real profile.
pub fn migrate_legacy_data_dir(new_dir: &Path, legacy_dir: &Path) -> MigrationOutcome {
    let destination = new_dir.join(DATABASE);
    if destination.exists() {
        return MigrationOutcome::NotNeeded;
    }

    let source = legacy_dir.join(DATABASE);
    if !source.is_file() {
        // Fresh install, or a legacy directory that never held a database.
        return MigrationOutcome::NotNeeded;
    }

    match copy_into_place(&source, &destination) {
        Ok(()) => MigrationOutcome::Migrated {
            from: source.clone(),
        },
        Err(error) => MigrationOutcome::Failed {
            from: source.clone(),
            reason: error.to_string(),
        },
    }
}

/// Copy via a temporary name in the *destination* directory, then rename into place.
///
/// A rename within one directory is atomic, so a crash mid-copy leaves either no database or a
/// complete one -- never a truncated `library.db` that SQLite would open as a valid empty file.
fn copy_into_place(source: &Path, destination: &Path) -> io::Result<()> {
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }

    let staging = destination.with_extension("db.migrating");
    // A leftover staging file from a previous crash is ours to overwrite.
    let _ = fs::remove_file(&staging);

    fs::copy(source, &staging)?;
    fs::rename(&staging, destination)?;
    Ok(())
}

/// Holds the one-time notice for the life of the process. Managed state rather than a log line,
/// because the surface that must show it is the UI (D-R6).
pub struct MigrationNotice(Option<String>);

impl MigrationNotice {
    pub fn new(notice: Option<String>) -> Self {
        Self(notice)
    }

    pub fn get(&self) -> Option<String> {
        self.0.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "document-reader-migrate-{name}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    /// D-R4: the destination starts empty and must end up holding the legacy bytes.
    #[test]
    fn a_moved_database_is_copied_before_it_is_opened() {
        let root = scratch("copy");
        let legacy = root.join("com.documentreader.app");
        let current = root.join("com.docupro.app");
        fs::create_dir_all(&legacy).expect("legacy dir");
        fs::create_dir_all(&current).expect("current dir");
        fs::write(legacy.join(DATABASE), b"the user's library").expect("write legacy db");

        let outcome = migrate_legacy_data_dir(&current, &legacy);

        assert_eq!(
            outcome,
            MigrationOutcome::Migrated {
                from: legacy.join(DATABASE)
            },
            "an existing legacy library must be copied, not ignored"
        );
        assert_eq!(
            fs::read(current.join(DATABASE)).expect("destination db"),
            b"the user's library",
            "the copied bytes must be the legacy bytes"
        );
        fs::remove_dir_all(&root).ok();
    }

    /// D-R5: copy, never move. The legacy file survives so a failed launch is recoverable.
    #[test]
    fn the_legacy_database_is_left_in_place() {
        let root = scratch("leave");
        let legacy = root.join("com.documentreader.app");
        let current = root.join("com.docupro.app");
        fs::create_dir_all(&legacy).expect("legacy dir");
        fs::write(legacy.join(DATABASE), b"keep me").expect("write legacy db");

        let outcome = migrate_legacy_data_dir(&current, &legacy);

        assert!(matches!(outcome, MigrationOutcome::Migrated { .. }));
        assert_eq!(
            fs::read(legacy.join(DATABASE)).expect("legacy db still there"),
            b"keep me",
            "migration copies; deleting the source would make a failed copy unrecoverable"
        );
        fs::remove_dir_all(&root).ok();
    }

    /// D-R5: a second launch must not overwrite a library the user has since edited.
    #[test]
    fn a_second_launch_does_not_overwrite_the_migrated_library() {
        let root = scratch("idempotent");
        let legacy = root.join("com.documentreader.app");
        let current = root.join("com.docupro.app");
        fs::create_dir_all(&legacy).expect("legacy dir");
        fs::create_dir_all(&current).expect("current dir");
        fs::write(legacy.join(DATABASE), b"old").expect("write legacy db");

        assert!(matches!(
            migrate_legacy_data_dir(&current, &legacy),
            MigrationOutcome::Migrated { .. }
        ));
        fs::write(current.join(DATABASE), b"old plus a week of edits").expect("edit destination");

        let second = migrate_legacy_data_dir(&current, &legacy);

        assert_eq!(second, MigrationOutcome::NotNeeded);
        assert_eq!(
            fs::read(current.join(DATABASE)).expect("destination db"),
            b"old plus a week of edits",
            "the second launch must treat an existing destination as already migrated"
        );
        fs::remove_dir_all(&root).ok();
    }

    /// A fresh install has no legacy directory; that is a no-op, not a failure.
    #[test]
    fn a_fresh_install_reports_nothing_to_do() {
        let root = scratch("fresh");
        let current = root.join("com.docupro.app");

        assert_eq!(
            migrate_legacy_data_dir(&current, &root.join("com.documentreader.app")),
            MigrationOutcome::NotNeeded
        );
        assert!(
            !current.join(DATABASE).exists(),
            "a no-op must not create an empty database"
        );
        fs::remove_dir_all(&root).ok();
    }

    /// D-R6: only a real migration produces a notice. Silence is the correct answer otherwise.
    #[test]
    fn only_a_migration_produces_a_notice() {
        let root = scratch("notice");
        let legacy = root.join("com.documentreader.app");
        let current = root.join("com.docupro.app");
        fs::create_dir_all(&legacy).expect("legacy dir");
        fs::write(legacy.join(DATABASE), b"x").expect("write legacy db");

        assert_eq!(MigrationOutcome::NotNeeded.notice(), None);
        let migrated = migrate_legacy_data_dir(&current, &legacy);
        let notice = migrated.notice().expect("a migration must be visible once");
        assert!(
            notice.contains("com.documentreader.app"),
            "the notice must name where the library came from, got {notice:?}"
        );
        fs::remove_dir_all(&root).ok();
    }

    /// The helper is given paths; it must not invent a legacy location of its own.
    #[test]
    fn the_legacy_identifier_names_the_pre_rename_data_directory() {
        assert_eq!(LEGACY_IDENTIFIER, "com.documentreader.app");
    }
}
