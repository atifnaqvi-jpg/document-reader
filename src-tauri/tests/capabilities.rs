//! P1.9 (docs/P1.9-DESIGN.md D10): the fullscreen capability must be granted.
//!
//! `core:window:set_fullscreen` is **not** part of Tauri's `core:window:default` set
//! (tauri's build.rs declares it as `false`, unlike `is_fullscreen` which defaults to
//! `true`). Without an explicit grant the frontend's `setFullscreen` call is rejected at
//! runtime and the UI would silently do nothing — a failure mode no Rust test would
//! otherwise notice, because fullscreen is entirely a frontend concern.
//!
//! Asserting on the capability file keeps that from regressing if someone tidies the
//! permissions list.

use std::path::PathBuf;

fn capability_permissions() -> Vec<String> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("capabilities/default.json");
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    let parsed: serde_json::Value = serde_json::from_str(&raw)
        .unwrap_or_else(|e| panic!("{} is not valid JSON: {e}", path.display()));
    parsed["permissions"]
        .as_array()
        .unwrap_or_else(|| panic!("{} has no `permissions` array", path.display()))
        .iter()
        .map(|value| {
            value
                .as_str()
                .unwrap_or_else(|| panic!("non-string entry in the permissions array"))
                .to_string()
        })
        .collect()
}

/// The window may be put into fullscreen from the frontend.
#[test]
fn fullscreen_is_permitted() {
    let permissions = capability_permissions();
    assert!(
        permissions
            .iter()
            .any(|p| p == "core:window:allow-set-fullscreen"),
        "core:window:allow-set-fullscreen is missing from capabilities/default.json, so \
         the fullscreen toggle will be denied at runtime. Permissions: {permissions:?}"
    );
}
