//! Session-scoped password memory for the decrypted-copy flow (P2.4 T10).
//!
//! T10 is a small rule with a large failure mode: passwords live **in memory, for the
//! session only** — never on disk, never in SQLite, never in a log or an event — and are
//! **cleared** when the document closes, not merely dropped.
//!
//! "Not merely dropped" is the reason this is a type rather than a `String` in the
//! command. A plain `String` handed to a closure would be deallocated but its bytes could
//! still be sitting in freed heap; zeroising on clear is the difference between "the
//! password is not reachable" and "the password is still in the process's memory".
//!
//! The store is keyed by document **path**, not by the library id: the id is stable across
//! a re-import, and a password must not silently follow a document to different bytes.
//! Closing a document (or a re-import, which replaces its bytes) drops the entry.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

/// An in-memory password that zeroises its bytes when cleared.
///
/// `Debug` is deliberately *not* derived: a password must never be formatted into a log
/// line or an error message by accident, and a redacting impl is the only way to be sure.
#[derive(Clone)]
pub struct SessionPassword(String);

impl SessionPassword {
    pub fn new(password: impl Into<String>) -> Self {
        SessionPassword(password.into())
    }

    /// Borrows the password for a single operation.
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Overwrites the bytes before dropping the string.
    pub fn wipe(&mut self) {
        // SAFETY: `as_bytes_mut` on a `String` we uniquely own, which is valid UTF-8 by
        // construction. Zeroing preserves UTF-8 validity (NUL is a valid code point), so
        // the `String` is never left in an invalid state.
        unsafe {
            self.0.as_bytes_mut().fill(0);
        }
        self.0.clear();
    }
}

impl Drop for SessionPassword {
    fn drop(&mut self) {
        self.wipe();
    }
}

impl std::fmt::Debug for SessionPassword {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never print the password, even under `{:?}`.
        f.write_str("SessionPassword(<redacted>)")
    }
}

/// The process-wide password store (T10). One per app, held in Tauri managed state.
#[derive(Default)]
pub struct PasswordStore {
    /// `Arc` rather than a bare `SessionPassword` so a *reader* can keep the bytes alive for as
    /// long as the document it unlocked is open. `PdfDocument` borrows the password string, so a
    /// value that vanished at the end of the lookup would leave that borrow dangling; the Arc ties
    /// the two lifetimes together without copying the secret.
    entries: Mutex<HashMap<String, Arc<SessionPassword>>>,
}

impl PasswordStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Remembers a password for a document path, replacing any previous one.
    pub fn remember(&self, path: &str, password: SessionPassword) {
        self.remember_shared(path, Arc::new(password));
    }

    /// [`remember`](Self::remember) for a password already shared with a reader.
    pub fn remember_shared(&self, path: &str, password: Arc<SessionPassword>) {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(previous) = entries.insert(path.to_string(), password) {
            // The replaced entry is dropped, which zeroises it -- unless a reader still holds a
            // clone, in which case it is zeroised when that document closes. Either way the store
            // no longer serves the old password.
            drop(previous);
        }
    }

    /// A shared handle to the password for `path`, if one is held.
    ///
    /// The only reason this exists beside [`with_password`](Self::with_password): a reader needs
    /// the bytes to outlive the lookup, and a closure cannot hand back a borrow of its own argument.
    pub fn get(&self, path: &str) -> Option<Arc<SessionPassword>> {
        self.entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(path)
            .cloned()
    }

    /// Runs `f` with the remembered password for `path`, if any.
    ///
    /// A closure rather than a `get() -> Option<&SessionPassword>` because the lock must
    /// not escape this function: handing out a reference would let a caller hold the
    /// password, which is exactly the lifetime T10 wants bounded.
    pub fn with_password<T>(&self, path: &str, f: impl FnOnce(&str) -> T) -> Option<T> {
        let entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        entries.get(path).map(|password| f(password.expose()))
    }

    /// Whether a password is currently held for a path.
    pub fn has(&self, path: &str) -> bool {
        let entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        entries.contains_key(path)
    }

    /// Forgets a path's password, zeroising it (T10: "cleared on document close").
    ///
    /// Returns whether an entry was present, so a caller can assert the close actually
    /// cleared something.
    pub fn forget(&self, path: &str) -> bool {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        match entries.remove(path) {
            // `Arc::into_inner` succeeds only when no reader still holds the password, which is the
            // usual case by the time a document closes. When a handle does still hold one, dropping
            // it zeroises there instead -- the store's job is to stop serving it, and it does.
            Some(password) => {
                if let Some(mut owned) = Arc::into_inner(password) {
                    owned.wipe();
                }
                true
            }
            None => false,
        }
    }

    /// Forgets every password. Used on app exit and by tests.
    pub fn clear(&self) {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        for (_, password) in entries.drain() {
            if let Some(mut owned) = Arc::into_inner(password) {
                owned.wipe();
            }
        }
    }

    /// Number of remembered passwords (diagnostics/tests).
    pub fn len(&self) -> usize {
        self.entries.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// The process-wide store, so read paths can find a remembered password.
///
/// One instance, not two: `lib.rs` manages *this* `Arc`, and the PDF provider reads through it. A
/// second `PasswordStore` would make "remembered" and "available to open the file" different
/// questions, which is precisely the failure this fixes -- a document the app could decrypt but
/// still could not render.
///
/// Returning an `Arc` rather than a `&'static` keeps `PasswordStore` usable as Tauri managed state
/// while leaving the interior mutability (`Mutex`) to enforce T10's rules.
pub fn shared() -> Arc<PasswordStore> {
    static SHARED: OnceLock<Arc<PasswordStore>> = OnceLock::new();
    Arc::clone(SHARED.get_or_init(|| Arc::new(PasswordStore::new())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remembers_and_exposes_a_password() {
        let store = PasswordStore::new();
        store.remember("C:/docs/a.pdf", SessionPassword::new("secret"));

        assert_eq!(
            store.with_password("C:/docs/a.pdf", |password| password.to_string()),
            Some("secret".to_string())
        );
    }

    #[test]
    fn has_no_password_for_an_unknown_path() {
        let store = PasswordStore::new();
        assert!(!store.has("C:/docs/other.pdf"));
        assert!(store.with_password("C:/docs/other.pdf", |_| ()).is_none());
    }

    #[test]
    fn forgetting_a_password_removes_it() {
        let store = PasswordStore::new();
        store.remember("C:/docs/a.pdf", SessionPassword::new("secret"));

        assert!(store.forget("C:/docs/a.pdf"));
        assert!(!store.has("C:/docs/a.pdf"));
        // Forgetting again reports nothing was there, rather than pretending to clear.
        assert!(!store.forget("C:/docs/a.pdf"));
    }

    #[test]
    fn clearing_removes_every_entry() {
        let store = PasswordStore::new();
        store.remember("a", SessionPassword::new("one"));
        store.remember("b", SessionPassword::new("two"));

        store.clear();

        assert!(store.is_empty());
    }

    #[test]
    fn a_password_is_never_printed_in_debug_output() {
        let password = SessionPassword::new("hunter2");
        let rendered = format!("{password:?}");
        assert!(
            !rendered.contains("hunter2"),
            "debug leaked the password: {rendered}"
        );
        assert!(rendered.contains("redacted"));
    }

    #[test]
    fn remembering_again_replaces_the_previous_password() {
        let store = PasswordStore::new();
        store.remember("a", SessionPassword::new("first"));
        store.remember("a", SessionPassword::new("second"));

        assert_eq!(store.len(), 1);
        assert_eq!(
            store.with_password("a", |password| password.to_string()),
            Some("second".to_string())
        );
    }
}
