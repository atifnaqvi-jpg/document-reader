//! Encrypted-PDF read-only default and the explicit "Decrypt a copy" path
//! (P2.4 T7/T8/T9/T10).
//!
//! The policy has three parts, and each is a separate function so none can leak into
//! another:
//!
//! * **Detect** ([`is_encrypted`]) — from the trailer's `/Encrypt`, not from whether a
//!   prompt was survived. A PDF with an empty user password opens without one and is
//!   still protected.
//! * **Refuse** ([`read_only_guard`]) — the single gate every edit command calls. It never
//!   prompts; it returns [`AppError::EncryptedReadOnly`] and the user decides whether to
//!   decrypt a copy.
//! * **Decrypt a copy** ([`decrypt_copy`]) — the only path to an editable file. It never
//!   touches the original and never re-encrypts.
//!
//! T9 is structural: lopdf's `Document::encrypt` is never called anywhere in this file,
//! and the decrypted copy is written after the `/Encrypt` trailer entry is removed, with
//! `encryption_state` cleared, so `save_to` cannot re-encrypt it. Re-encryption
//! is not a capability that is disabled — it is one that does not exist.

use std::path::{Path, PathBuf};

use lopdf::Document;

use crate::model::AppError;

/// True when a PDF's trailer declares `/Encrypt` (P2.4 T7).
///
/// Reads the trailer directly rather than trusting the provider's report: a PDF whose
/// user password is empty loads and renders fine, so "it opened" says nothing about
/// whether it is protected.
pub fn is_encrypted(path: &Path) -> Result<bool, AppError> {
    let bytes = std::fs::read(path)
        .map_err(|error| AppError::Other(format!("could not read {}: {error}", path.display())))?;
    is_encrypted_bytes(&bytes)
}

/// [`is_encrypted`] for bytes already in hand.
pub fn is_encrypted_bytes(bytes: &[u8]) -> Result<bool, AppError> {
    // `load_metadata` reports `encrypted` without needing a password and without failing
    // on one, which is exactly what detection needs.
    let metadata = Document::load_metadata_mem(bytes)
        .map_err(|error| AppError::Other(format!("could not read PDF: {error}")))?;
    Ok(metadata.encrypted)
}

/// The single guard every editing command runs first (P2.4 T7).
///
/// Refuses with [`AppError::EncryptedReadOnly`] for a protected document and does nothing
/// else — no prompt, no decryption attempt, no partial edit. The user's next step is the
/// explicit "Decrypt a copy" action, not a password dialog they did not ask for.
pub fn read_only_guard(path: &Path) -> Result<(), AppError> {
    if is_encrypted(path)? {
        return Err(AppError::EncryptedReadOnly);
    }
    Ok(())
}

/// The path a decrypted copy should be written to, with the collision guard from T8/O2.
///
/// `"{stem} (unprotected).pdf"` first, then `(unprotected 2)`, `(3)`, … taking the first
/// free name. A decrypted copy must never clobber an existing file: the user may have
/// edited the previous one, and silently replacing it would destroy their work.
pub fn unprotected_path(
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

    let first = parent.join(format!("{stem} (unprotected).{extension}"));
    if !exists(&first) {
        return Ok(first);
    }
    for number in 2..=9_999_u32 {
        let candidate = parent.join(format!("{stem} (unprotected {number}).{extension}"));
        if !exists(&candidate) {
            return Ok(candidate);
        }
    }
    Err(AppError::Other(
        "reached the limit of decrypted copies for this document".to_string(),
    ))
}

/// The result of a successful decryption: where it landed and what was verified.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DecryptOutcome {
    pub path: String,
    pub pages: u32,
}

/// Loads a protected PDF and returns a document whose `/Encrypt` has been removed and
/// whose objects are decrypted (P2.4 T8), refusing on a wrong password (T8/F7).
///
/// Split from [`decrypt_copy`] so the byte-level verification in the caller is testable
/// without touching the filesystem.
pub fn decrypt_document(path: &Path, password: &str) -> Result<Document, AppError> {
    let document = Document::load_with_password(path, password).map_err(|error| match error {
        lopdf::Error::InvalidPassword => AppError::WrongPassword,
        other => AppError::Other(format!("could not decrypt PDF: {other}")),
    })?;

    strip_encryption(document)
}

/// Removes the trailer's `/Encrypt` and clears the remembered encryption state, so a
/// subsequent save writes a genuinely unprotected file (T8/T9).
fn strip_encryption(mut document: Document) -> Result<Document, AppError> {
    document.trailer.remove(b"Encrypt");
    // `encryption_state` is what an *incremental* save would use to re-encrypt appended
    // objects. Clearing it here is the structural guarantee that no save path can put
    // encryption back (T9).
    document.encryption_state = None;
    Ok(document)
}

/// Decrypts `original` to a new, unprotected copy (P2.4 T8).
///
/// Sequence, all of it required:
/// 1. refuse when there is nothing to decrypt;
/// 2. load with the password, mapping a bad password to [`AppError::WrongPassword`];
/// 3. strip `/Encrypt`;
/// 4. pick the first free `(unprotected …)` name;
/// 5. write through the D2 atomic temp-file-and-rename discipline;
/// 6. byte-verify the result — it must open **without** a password, and its page count
///    and extracted text must match the decrypted original;
/// 7. on any failure, delete the temp/copy and refuse.
///
/// The original file is opened read-only and is never a write target, so it stays
/// byte-identical on disk. Password bytes live only for this call (T10): they are not
/// stored on the returned outcome and not logged.
pub fn decrypt_copy(original: &Path, password: &str) -> Result<DecryptOutcome, AppError> {
    if !is_encrypted(original)? {
        return Err(AppError::Other(
            "this document is not password-protected".to_string(),
        ));
    }

    let decrypted = decrypt_document(original, password)?;

    let page_count = decrypted.get_pages().len() as u32;
    let expected_text = extract_all_text(&decrypted, page_count);

    let mut to_save = decrypted.clone();
    to_save.renumber_objects();
    to_save.compress();
    let mut bytes = Vec::new();
    to_save
        .save_to(&mut bytes)
        .map_err(|error| AppError::Other(format!("could not write decrypted PDF: {error}")))?;

    let target = unprotected_path(original, |candidate| candidate.exists())?;

    let write = || -> Result<(), AppError> {
        crate::editor::save::write_atomically(&target, &bytes)?;
        verify_copy(&target, page_count, &expected_text)
    };

    if let Err(error) = write() {
        // Never leave a half-verified decrypted copy behind: a file that does not open
        // is worse than no file, because the user will try to use it.
        let _ = std::fs::remove_file(&target);
        return Err(error);
    }

    Ok(DecryptOutcome {
        path: target.to_string_lossy().to_string(),
        pages: page_count,
    })
}

/// Byte-verifies a decrypted copy: it must open without a password, have the same page
/// count, and yield the same text as the decrypted original (T8).
fn verify_copy(path: &Path, expected_pages: u32, expected_text: &str) -> Result<(), AppError> {
    let bytes = std::fs::read(path)
        .map_err(|error| AppError::Other(format!("could not read the copy back: {error}")))?;

    if is_encrypted_bytes(&bytes)? {
        return Err(AppError::Other(
            "the decrypted copy still declares encryption".to_string(),
        ));
    }

    // The real test: no password supplied, and it loads.
    let reloaded = Document::load_mem(&bytes).map_err(|error| {
        AppError::Other(format!(
            "the decrypted copy does not open without a password: {error}"
        ))
    })?;

    let pages = reloaded.get_pages().len() as u32;
    if pages != expected_pages {
        return Err(AppError::Other(format!(
            "the decrypted copy has {pages} pages, expected {expected_pages}"
        )));
    }

    let text = extract_all_text(&reloaded, pages);
    if text != expected_text {
        return Err(AppError::Other(
            "the decrypted copy's text does not match the original".to_string(),
        ));
    }

    Ok(())
}

/// Concatenates every page's text through lopdf, for the round-trip comparison.
///
/// Deliberately the *bounded* extractor: a decrypted copy of an untrusted file is still
/// untrusted input.
fn extract_all_text(document: &Document, pages: u32) -> String {
    if pages == 0 {
        return String::new();
    }
    let numbers: Vec<u32> = (1..=pages).collect();
    document
        .extract_text_with_limit(&numbers, crate::editor::textlocate::CONTENT_LIMIT)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::testpdf;

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("dr-unprotect-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn detects_an_encrypted_document() {
        let encrypted = testpdf::bytes(&testpdf::encrypted_aes());
        assert!(is_encrypted_bytes(&encrypted).unwrap());

        let plain = testpdf::bytes(&testpdf::simple_winansi());
        assert!(!is_encrypted_bytes(&plain).unwrap());
    }

    #[test]
    fn read_only_guard_refuses_an_encrypted_document() {
        let dir = temp_dir();
        let path = dir.join("protected.pdf");
        std::fs::write(&path, testpdf::bytes(&testpdf::encrypted_aes())).unwrap();

        let error = read_only_guard(&path).unwrap_err();
        assert_eq!(error.code(), "ENCRYPTED_READ_ONLY");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn read_only_guard_allows_a_plain_document() {
        let dir = temp_dir();
        let path = dir.join("plain.pdf");
        std::fs::write(&path, testpdf::bytes(&testpdf::simple_winansi())).unwrap();
        assert!(read_only_guard(&path).is_ok());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn decrypt_copy_writes_a_working_unprotected_file_and_leaves_the_original_alone() {
        let dir = temp_dir();
        let original = dir.join("secret.pdf");
        let original_bytes = testpdf::bytes(&testpdf::encrypted_aes());
        std::fs::write(&original, &original_bytes).unwrap();

        let outcome = decrypt_copy(&original, "secret").unwrap();

        // The copy opens with no password and has the original's pages and text.
        let copy = Path::new(&outcome.path);
        let copy_bytes = std::fs::read(copy).unwrap();
        assert!(!is_encrypted_bytes(&copy_bytes).unwrap());
        let reloaded = Document::load_mem(&copy_bytes).unwrap();
        assert_eq!(reloaded.get_pages().len(), 3);
        assert!(reloaded.extract_text(&[1]).unwrap().contains("ALPHA"));

        // The original is byte-identical on disk: T8 forbids touching it.
        assert_eq!(std::fs::read(&original).unwrap(), original_bytes);

        // And the name follows the frozen convention.
        assert!(
            copy.file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .contains("(unprotected)"),
            "unexpected name: {outcome:?}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn decrypt_copy_refuses_a_wrong_password_and_writes_nothing() {
        let dir = temp_dir();
        let original = dir.join("secret.pdf");
        std::fs::write(&original, testpdf::bytes(&testpdf::encrypted_aes())).unwrap();

        let error = decrypt_copy(&original, "not-the-password").unwrap_err();
        assert_eq!(error.code(), "WRONG_PASSWORD");

        // No copy was left behind.
        let copies: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().contains("unprotected"))
            .collect();
        assert!(
            copies.is_empty(),
            "a failed decrypt left a copy: {copies:?}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn decrypt_copy_never_overwrites_an_existing_copy() {
        let dir = temp_dir();
        let original = dir.join("secret.pdf");
        std::fs::write(&original, testpdf::bytes(&testpdf::encrypted_aes())).unwrap();

        // Pre-create the first name with sentinel content.
        let first = dir.join("secret (unprotected).pdf");
        std::fs::write(&first, b"do not clobber me").unwrap();

        let outcome = decrypt_copy(&original, "secret").unwrap();

        assert_eq!(std::fs::read(&first).unwrap(), b"do not clobber me");
        assert!(
            outcome.path.contains("(unprotected 2)"),
            "guard should have moved to the next free name: {}",
            outcome.path
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn unprotected_path_takes_the_first_free_name() {
        let original = Path::new("C:/docs/report.pdf");
        assert_eq!(
            unprotected_path(original, |_| false).unwrap(),
            PathBuf::from("C:/docs/report (unprotected).pdf")
        );
        assert_eq!(
            unprotected_path(original, |candidate| {
                candidate.file_name().unwrap() == "report (unprotected).pdf"
            })
            .unwrap(),
            PathBuf::from("C:/docs/report (unprotected 2).pdf")
        );
    }

    #[test]
    fn decrypting_a_plain_document_is_refused() {
        let dir = temp_dir();
        let original = dir.join("plain.pdf");
        std::fs::write(&original, testpdf::bytes(&testpdf::simple_winansi())).unwrap();
        let error = decrypt_copy(&original, "anything").unwrap_err();
        assert_eq!(error.code(), "OTHER");
        std::fs::remove_dir_all(&dir).ok();
    }
}
