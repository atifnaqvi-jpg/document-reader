//! I-PR3: nothing may be written between `encrypt()` and `save_to()`.
//!
//! This is a **source-level** guard, and it has to be, because the defect it prevents is
//! invisible to a runtime test of the file's *content*. Measured in this tree, not assumed:
//! appending one object after `encrypt()` produces an output in which that object sits in
//! the clear (a probe string was present verbatim in the bytes) while the document's
//! `/Encrypt` dictionary claims every stream is encrypted. A reader trusts that claim; a
//! recovery tool reads the plaintext. Neither a page-count check nor a text round-trip
//! notices, because the leaked object is reachable from nowhere and says nothing.
//!
//! lopdf makes this hazard easy to hit and impossible to detect afterwards: `encrypt()`
//! takes `&mut Document` and returns `Ok(())`, then drops the file encryption key. Every
//! later `add_object` is silently unencrypted. So the rule is about *statement order*, and
//! statement order is what this guard reads.
//!
//! Why a line-number-free AST-free text check is adequate here: the function is short and
//! has exactly one `encrypt` call and one `save_to` call. Reading the bytes between them is
//! crude, but it is exact for the shape it checks, and it fails closed when the shape
//! changes enough to confuse it.

use std::path::PathBuf;

fn protect_source() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/editor/protect.rs");
    std::fs::read_to_string(&path).expect("the protect module must be readable")
}

/// Everything between `document.encrypt(` and the `save_to` that follows it.
///
/// Panics if either marker is missing, so renaming the call cannot silently disable the
/// guard — a guard that stops guarding is worse than no guard, because the suite stays
/// green.
fn between_encrypt_and_save(source: &str) -> String {
    let encrypt_at = source
        .find("document\n        .encrypt(&state)")
        .or_else(|| source.find(".encrypt(&state)"))
        .expect("I-PR3 guard: no `.encrypt(&state)` call found; if it moved, update this guard");

    let after = &source[encrypt_at..];
    let save_at = after
        .find(".save_to(")
        .expect("I-PR3 guard: no `.save_to(` after `.encrypt`; if it moved, update this guard");

    after[..save_at].to_string()
}

/// The only statements allowed between `encrypt` and `save_to` are the error mapping and
/// the buffer declaration.
#[test]
fn nothing_is_written_between_encryption_and_the_save() {
    let source = protect_source();
    let gap = between_encrypt_and_save(&source);

    for forbidden in [
        "add_object",
        "objects.insert",
        "objects.remove",
        "renumber_objects",
        "compress(",
        "delete_object",
        "trailer.set",
        "trailer.remove",
    ] {
        assert!(
            !gap.contains(forbidden),
            "`{forbidden}` runs after `encrypt()` and before `save_to()`. lopdf has already \
             dropped the file encryption key, so anything this writes goes out in the clear \
             under an `/Encrypt` that claims otherwise (design K6 / I-PR3). Move it above the \
             `encrypt()` call.\n\ncode between them:\n{gap}"
        );
    }
}

/// `encrypt()` must appear exactly once: a second call, or a stray call in a helper, would
/// mean the ordering argument above covers only one of them.
#[test]
fn the_product_encrypts_in_exactly_one_place() {
    let source = protect_source();
    let calls = source.matches(".encrypt(&state)").count();
    assert_eq!(
        calls, 1,
        "expected exactly one `encrypt(&state)` call in the protect module, found {calls}; the \
         I-PR3 ordering guard reasons about the one it can see"
    );
}

/// The guard must actually be able to see its region. If both markers were missing it would
/// panic rather than pass, and this test proves the markers still parse.
#[test]
fn the_guard_finds_a_non_empty_region() {
    let source = protect_source();
    let gap = between_encrypt_and_save(&source);
    assert!(
        !gap.trim().is_empty(),
        "the region between encrypt() and save_to() is empty, so this guard is checking nothing"
    );
}
