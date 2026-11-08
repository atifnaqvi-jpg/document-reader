//! Reading the Windows certificate store (P3.2 D-SG5).
//!
//! The only module in the signing path that calls into Windows. It does exactly two things: open
//! the user's `MY` store read-only, and copy each certificate's DER out along with whether the
//! store holds a private key for it. Every judgement about those certificates -- which can sign,
//! what to display, what to do with one that will not parse -- lives in `identity.rs`, where a
//! Linux test can reach it.
//!
//! **The handle does not escape.** `with_user_store` takes a closure, mirroring
//! `PasswordStore::with_password`: the `HCERTSTORE` is opened and closed inside one call, so no
//! caller can hold a handle past the close or forget to close it. The alternative shape --
//! `open()` returning a guard type -- was rejected because a guard still moves through the app and
//! the closure makes the lifetime a local property that a reviewer can check in one screen.
//!
//! **Nothing here is trusted.** A certificate read from the store is a certificate the user has
//! installed. Whether it is *trustworthy* is D-SG4's question, answered `unknown` until a local
//! trust store says otherwise; this module has no opinion and no way to express one.

#[cfg(windows)]
pub use windows_impl::{list_user_store, with_user_store};

/// The store could not be read on this platform.
///
/// Linux and macOS have no `MY` store in the Windows sense, and this build's shipping target is
/// Windows (D-SG5). Returning an empty list would be the silent skip this codebase forbids: it
/// would render as "you have no signing certificates", which is false, and the user would go
/// looking for a certificate they already have.
#[cfg(not(windows))]
pub fn with_user_store<T>(
    _f: impl FnOnce(&[super::identity::StoreEntry]) -> T,
) -> Result<T, crate::model::AppError> {
    Err(crate::model::AppError::CertificateStoreUnavailable(
        "the Windows certificate store is not available on this platform".to_string(),
    ))
}

/// Convenience wrapper: read the user's store and describe it.
#[cfg(not(windows))]
pub fn list_user_store() -> Result<super::identity::Listing, crate::model::AppError> {
    with_user_store(super::identity::listing_from)
}

#[cfg(windows)]
mod windows_impl {
    use windows::Win32::Security::Cryptography::{
        CertCloseStore, CertEnumCertificatesInStore, CertGetCertificateContextProperty,
        CertOpenStore, CERT_CONTEXT, CERT_KEY_CONTEXT_PROP_ID, CERT_KEY_PROV_INFO_PROP_ID,
        CERT_NCRYPT_KEY_HANDLE_PROP_ID, CERT_OPEN_STORE_FLAGS, CERT_STORE_PROV_SYSTEM_W,
        CERT_STORE_READONLY_FLAG, CERT_SYSTEM_STORE_CURRENT_USER, HCERTSTORE, X509_ASN_ENCODING,
    };

    use crate::model::AppError;

    use super::super::identity::{listing_from, Listing, StoreEntry};

    /// `L"My"` -- the current user's personal store, where imported signing certificates land.
    ///
    /// Encoded as UTF-16 by hand rather than via a helper so the constant is visible in the source
    /// and cannot drift with a library default.
    const MY_STORE: [u16; 3] = [b'M' as u16, b'y' as u16, 0];

    /// Whether the store holds a private key for this certificate, by **either** mechanism.
    ///
    /// A certificate's key can be referenced through the legacy CryptoAPI provider
    /// (`CERT_KEY_PROV_INFO_PROP_ID`) or through a CNG key handle
    /// (`CERT_NCRYPT_KEY_HANDLE_PROP_ID`). Current Windows tooling writes the CNG form. Testing
    /// only the legacy property would report every modern signer as key-less and hide it from the
    /// picker -- a bug that no compile check and no render test can see.
    unsafe fn has_private_key(context: *const CERT_CONTEXT) -> bool {
        [
            CERT_KEY_PROV_INFO_PROP_ID,
            CERT_KEY_CONTEXT_PROP_ID,
            CERT_NCRYPT_KEY_HANDLE_PROP_ID,
        ]
        .into_iter()
        .any(|property| {
            let mut size: u32 = 0;
            // A zero-byte query is the documented way to ask "does this property exist": it
            // returns success and the required size without writing anything.
            CertGetCertificateContextProperty(context, property, None, &mut size).is_ok()
        })
    }

    /// Run `f` over the raw entries of the current user's personal store.
    ///
    /// The store is opened read-only: this module never adds, deletes or modifies a certificate.
    /// Read-only is also a safety property worth stating -- a bug here cannot damage the user's
    /// store.
    pub fn with_user_store<T>(f: impl FnOnce(&[StoreEntry]) -> T) -> Result<T, AppError> {
        let store: HCERTSTORE = unsafe {
            CertOpenStore(
                CERT_STORE_PROV_SYSTEM_W,
                X509_ASN_ENCODING,
                None,
                CERT_OPEN_STORE_FLAGS(CERT_STORE_READONLY_FLAG.0 | CERT_SYSTEM_STORE_CURRENT_USER),
                Some(MY_STORE.as_ptr() as *const core::ffi::c_void),
            )
        }
        .map_err(|error| {
            AppError::CertificateStoreUnavailable(format!(
                "the personal certificate store could not be opened: {error}"
            ))
        })?;

        // Every path out of this function must close the store. The collection runs to completion
        // before the close, rather than returning early from inside the loop, so there is exactly
        // one close and no branch can skip it.
        let entries = unsafe { collect_all(store) };
        // The close result is dropped on purpose: `CertCloseStore` frees the handle and reports
        // failure only for a handle that was already invalid, which the read above would have
        // caught. Reporting it would turn a successful listing into an error over state nobody can
        // act on, and `CERT_CLOSE_STORE_CHECK_FLAG` -- the flag that would make it informative --
        // cannot succeed here because the enumerated contexts share the store.
        let _ = unsafe { CertCloseStore(Some(store), 0) };

        Ok(f(entries.as_slice()))
    }

    /// Walk the store, copying out what is needed before the handle closes.
    ///
    /// `CertEnumCertificatesInStore` both advances and frees the previous context when it is given
    /// one, so the loop does not free contexts itself. Copying the DER inside the loop is what
    /// makes the result independent of the store's lifetime.
    unsafe fn collect_all(store: HCERTSTORE) -> Vec<StoreEntry> {
        let mut entries = Vec::new();
        let mut context = CertEnumCertificatesInStore(store, None);
        while !context.is_null() {
            let certificate = &*context;
            let der = std::slice::from_raw_parts(
                certificate.pbCertEncoded,
                certificate.cbCertEncoded as usize,
            )
            .to_vec();
            entries.push(StoreEntry {
                der,
                has_private_key: has_private_key(context),
            });
            context = CertEnumCertificatesInStore(store, Some(context));
        }
        entries
    }

    /// Read the user's store and describe it.
    pub fn list_user_store() -> Result<Listing, AppError> {
        with_user_store(listing_from)
    }
}
