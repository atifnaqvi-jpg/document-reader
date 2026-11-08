//! Signing (P3.2 SCOPE-C). Nothing here claims trust; every judgement about a certificate is
//! D-SG4's separate question.

pub mod pfx;

pub use pfx::{import_bytes, import_with_store, ImportedIdentity, ImportedPfx};
