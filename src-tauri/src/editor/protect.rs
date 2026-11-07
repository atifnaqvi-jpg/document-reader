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

// ---------------------------------------------------------------------------
// P3.2 SCOPE-A: protect on save (design D-PR1..D-PR6)
//
// The P2.4 note above ("re-encryption is a capability that does not exist") was true of
// P2.4 and is superseded here on purpose: P3.2 adds the write direction. What does **not**
// change is the rule that made that note worth writing — encryption happens at exactly one
// place, after every other mutation, and the source file is never a write target (D-PR4).
// ---------------------------------------------------------------------------

/// Which permissions the user granted, one field per real `/P` bit.
///
/// A struct of `bool`s rather than a bitflags value because the UI must show every bit's
/// state explicitly (D-PR3): `Permissions::default()` is `all()`, so any representation
/// that can be "partly set" starts by granting everything and clearing nothing, which
/// ships a document that looks restricted and is not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionsChoice {
    pub print: bool,
    pub modify: bool,
    pub copy: bool,
    pub annotate: bool,
    pub fill_forms: bool,
    pub assemble: bool,
    pub print_high_quality: bool,
}

impl PermissionsChoice {
    /// Every permission granted. An explicit value, not an omitted field.
    pub fn all() -> Self {
        Self {
            print: true,
            modify: true,
            copy: true,
            annotate: true,
            fill_forms: true,
            assemble: true,
            print_high_quality: true,
        }
    }

    /// Maps the user's boxes onto lopdf's bitflags.
    ///
    /// `correct_bits()` inside lopdf force-sets the reserved bits; this function only
    /// decides the seven the user can actually see.
    fn to_lopdf(self) -> lopdf::Permissions {
        use lopdf::Permissions;
        let mut bits = Permissions::empty();
        let mut set = |flag: bool, permission: Permissions| {
            if flag {
                bits |= permission;
            }
        };
        set(self.print, Permissions::PRINTABLE);
        set(self.modify, Permissions::MODIFIABLE);
        set(self.copy, Permissions::COPYABLE);
        set(self.annotate, Permissions::ANNOTABLE);
        set(self.fill_forms, Permissions::FILLABLE);
        set(self.assemble, Permissions::ASSEMBLABLE);
        set(
            self.print_high_quality,
            Permissions::PRINTABLE_IN_HIGH_QUALITY,
        );
        bits
    }
}

/// The result of a successful protection: where it landed and what was verified.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProtectOutcome {
    pub path: String,
    pub pages: u32,
}

/// `"{stem} (protected).pdf"`, then `(protected 2)`, … — the D-PR4 name, alongside the
/// original, never over it. Same shape as [`unprotected_path`] on purpose: both are
/// "a derived copy of this document", and two different conventions would be two things
/// for the user to learn.
pub fn protected_path(
    original: &Path,
    exists: impl Fn(&Path) -> bool,
) -> Result<PathBuf, AppError> {
    sibling_copy_path(original, "protected", exists)
}

/// Shared namer for the `(unprotected)`/`(protected)` families.
fn sibling_copy_path(
    original: &Path,
    label: &str,
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

    let first = parent.join(format!("{stem} ({label}).{extension}"));
    if !exists(&first) {
        return Ok(first);
    }
    for number in 2..=9_999_u32 {
        let candidate = parent.join(format!("{stem} ({label} {number}).{extension}"));
        if !exists(&candidate) {
            return Ok(candidate);
        }
    }
    Err(AppError::Other(format!(
        "reached the limit of {label} copies for this document"
    )))
}

/// Encrypts `source` to a new sibling file (P3.2 SCOPE-A; design D-PR1/D-PR2/D-PR4).
///
/// The order is the invariant (**I-PR3**), and it is this function's whole shape:
///
/// 1. refuse if the source is already encrypted — layering encryption is not a request
///    anyone means to make, and it would hide which password opens the file;
/// 2. load, renumber, compress — every mutation;
/// 3. `encrypt` **last**, because lopdf drops the file encryption key once it returns and
///    any object written afterwards would go out in the clear under an `/Encrypt` that
///    claims otherwise;
/// 4. serialise, write through the D2 atomic temp-and-rename discipline;
/// 5. verify the result by **reopening it with the password** and comparing page count
///    and text; on failure, remove the copy and refuse.
///
/// Steps 1–3 mean there is no code path that mutates a document after encryption, because
/// nothing follows `encrypt` but serialisation.
pub fn protect_save(
    source: &Path,
    user_password: &str,
    owner_password: &str,
    permissions: PermissionsChoice,
) -> Result<ProtectOutcome, AppError> {
    use lopdf::encryption::crypt_filters::{Aes128CryptFilter, CryptFilter};
    use lopdf::{EncryptionState, EncryptionVersion, Object};
    use std::collections::BTreeMap;
    use std::sync::Arc;

    if user_password.is_empty() {
        // An empty user password is a real PDF state, but it is not what a "protect with a
        // password" dialog means, and accepting it silently produces a file that opens
        // without prompting while claiming to be protected.
        return Err(AppError::Other(
            "a protection password cannot be empty".to_string(),
        ));
    }

    if is_encrypted(source)? {
        return Err(AppError::Other(
            "this document is already password-protected".to_string(),
        ));
    }

    let mut document = Document::load(source)
        .map_err(|error| AppError::Other(format!("could not open PDF: {error}")))?;

    let page_count = document.get_pages().len() as u32;
    let expected_text = extract_all_text(&document, page_count);

    // Encrypted files need a `/ID` in the trailer; without one lopdf refuses with
    // `MissingFileID`. Generated deterministically from the content-free parts we have so
    // the same input yields the same id, which keeps the round-trip test stable.
    if !document.trailer.has(b"ID") {
        let id = Object::string_literal(b"docupro-protected".to_vec());
        document
            .trailer
            .set("ID", Object::Array(vec![id.clone(), id]));
    }

    document.renumber_objects();
    document.compress();

    // --- Encrypt, and then do nothing else but serialise (D-PR1 / I-PR3). ---
    let mut filters: BTreeMap<Vec<u8>, Arc<dyn CryptFilter>> = BTreeMap::new();
    filters.insert(b"StdCF".to_vec(), Arc::new(Aes128CryptFilter));

    let state: EncryptionState = EncryptionVersion::V4 {
        document: &document,
        encrypt_metadata: true,
        crypt_filters: filters,
        stream_filter: b"StdCF".to_vec(),
        string_filter: b"StdCF".to_vec(),
        owner_password,
        user_password,
        permissions: permissions.to_lopdf(),
    }
    .try_into()
    .map_err(|error| AppError::Other(format!("could not build encryption state: {error}")))?;

    document
        .encrypt(&state)
        .map_err(|error| AppError::Other(format!("could not encrypt the document: {error}")))?;

    let mut bytes = Vec::new();
    document
        .save_to(&mut bytes)
        .map_err(|error| AppError::Other(format!("could not write the protected PDF: {error}")))?;

    let target = protected_path(source, |candidate| candidate.exists())?;

    let write = || -> Result<(), AppError> {
        crate::editor::save::write_atomically(&target, &bytes)?;
        verify_protected(&target, user_password, page_count, &expected_text)
    };

    if let Err(error) = write() {
        let _ = std::fs::remove_file(&target);
        return Err(error);
    }

    Ok(ProtectOutcome {
        path: target.to_string_lossy().to_string(),
        pages: page_count,
    })
}

/// Reopens the protected copy with `password` and checks it still says what it said.
///
/// This is I-PR1 verified from the write side. Deliberately reopens **from disk** rather
/// than trusting `bytes`: the failure this catches is a file that cannot be opened, and an
/// in-memory check cannot see that.
fn verify_protected(
    path: &Path,
    password: &str,
    expected_pages: u32,
    expected_text: &str,
) -> Result<(), AppError> {
    let reopened = Document::load_with_password(path, password).map_err(|error| {
        AppError::Other(format!(
            "the protected copy does not reopen with its own password: {error}"
        ))
    })?;

    let pages = reopened.get_pages().len() as u32;
    if pages != expected_pages {
        return Err(AppError::Other(format!(
            "the protected copy has {pages} pages, expected {expected_pages}"
        )));
    }

    // lopdf leaves the document decrypted once a password was accepted, so text extraction
    // works directly. A wrong content comparison here means the encryption round-trip lost
    // or corrupted objects.
    let text = extract_all_text(&reopened, pages);
    if text != expected_text {
        return Err(AppError::Other(
            "the protected copy's text does not match the original".to_string(),
        ));
    }

    Ok(())
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
