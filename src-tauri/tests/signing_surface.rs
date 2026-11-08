//! The signing surface's shape (P3.2 D-SG3, D-SG1 as amended, K25).
//!
//! These guards read source text rather than behaviour. That is deliberate for the properties they
//! cover: "no HTTP client is reachable from the signing path" and "the keystore crate's types do not
//! escape its module" are claims about the *import graph*, and the way they go wrong is someone
//! adding one `use` line. A behavioural test cannot see an import that is not yet called; a scan
//! can, and it fails at the moment the line is typed rather than when the code path is first
//! exercised.

use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("the crate lives one level below the repository root")
        .to_path_buf()
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

/// Every `.rs` file under `src/editor/signature/`, so a new file in the module is covered without
/// anyone remembering to add it to this test.
fn signing_sources() -> Vec<(PathBuf, String)> {
    let root = repo_root().join("src-tauri/src/editor/signature");
    let mut found = Vec::new();
    let mut stack = vec![root.clone()];
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("cannot list {}: {e}", dir.display()));
        for entry in entries {
            let path = entry.expect("readable entry").path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                let text = read(&path);
                found.push((path, text));
            }
        }
    }
    assert!(
        !found.is_empty(),
        "the signing module must contain sources; an empty scan would pass every guard below"
    );
    found
}

/// D-SG3: the document hash never leaves the machine.
///
/// A comment saying "no network" is not enforcement. This fails on an HTTP client, a socket type,
/// or a `TcpStream` reaching the signing module, which is the only mechanism by which the hash
/// could be sent anywhere.
#[test]
fn nothing_in_the_signing_module_can_reach_the_network() {
    const FORBIDDEN: [&str; 6] = [
        "reqwest",
        "hyper",
        "ureq",
        "std::net",
        "TcpStream",
        "tokio::net",
    ];
    let mut offenders = Vec::new();
    for (path, text) in signing_sources() {
        for line in text.lines() {
            let code = line.split("//").next().unwrap_or("");
            for needle in FORBIDDEN {
                if code.contains(needle) {
                    offenders.push(format!("{}: {needle} in `{}`", path.display(), line.trim()));
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "the signing path must not be able to make a network call (D-SG3):\n{}",
        offenders.join("\n")
    );
}

/// The signing module must not *depend on* a network crate either, which a `use` scan cannot see.
#[test]
fn the_crate_does_not_depend_on_an_http_client() {
    let manifest = read(&repo_root().join("src-tauri/Cargo.toml"));
    let mut in_dependencies = false;
    let mut offenders = Vec::new();
    for line in manifest.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_dependencies = trimmed == "[dependencies]";
            continue;
        }
        if !in_dependencies || trimmed.starts_with('#') || trimmed.is_empty() {
            continue;
        }
        let name = trimmed.split(['=', ' ']).next().unwrap_or("");
        if ["reqwest", "hyper", "ureq", "isahc", "attohttpc", "curl"]
            .iter()
            .any(|forbidden| name.eq_ignore_ascii_case(forbidden))
        {
            offenders.push(trimmed.to_string());
        }
    }
    assert!(
        offenders.is_empty(),
        "an HTTP client in the dependency block makes D-SG3 unenforceable:\n{}",
        offenders.join("\n")
    );
}

/// K25: `p12-keystore` parsers and decrypts; nothing else. Its types must not escape its module,
/// because the crate sits on a different `der` generation than the signing stack and a type that
/// crossed the boundary would drag `der 0.8` into the signer.
///
/// The check is that the crate name appears in **exactly one** file: the PFX module that documents
/// why it is there. A second appearance means a type or a call has leaked.
#[test]
fn the_keystore_crate_is_named_only_in_the_pfx_module() {
    let mut users = Vec::new();
    for (path, text) in signing_sources() {
        for line in text.lines() {
            // `use p12_keystore::...` and any qualified path, matched on the crate's Rust name.
            if line.contains("p12_keystore") && !line.trim_start().starts_with("//") {
                users.push(path.file_name().unwrap().to_string_lossy().to_string());
                break;
            }
        }
    }
    users.sort();
    users.dedup();
    assert_eq!(
        users,
        vec!["pfx.rs".to_string()],
        "the keystore crate must be confined to the PFX module (design K25); found it in {users:?}"
    );
}

/// K25 for the CMS stack: `cms`, `rsa`, `x509-cert 0.2` and `der 0.7` are confined to `cms.rs`.
///
/// The signer sits on a second `der`/`x509-cert` generation than the rest of the crate (design
/// K24 predicted exactly this). The boundary is honoured only if no CMS-crate type escapes the one
/// file that builds signatures: a `der 0.7` value handed to `der 0.8` code does not compile, but
/// the reverse mistake -- threading an `x509-cert 0.3` certificate into the CMS builder -- is
/// caught here rather than at review.
///
/// `sha2` is deliberately absent from this list: the crate already uses both generations, and a
/// digest has no cross-generation identity problem.
#[test]
fn the_cms_crates_are_confined_to_the_cms_module() {
    for crate_name in ["cms", "rsa", "x509_cert_cms", "der_cms", "spki_cms"] {
        let mut users = Vec::new();
        for (path, text) in signing_sources() {
            for line in text.lines() {
                if line.trim_start().starts_with("//") {
                    continue;
                }
                // Match `use cms::`, `cms::builder::...` and the aliased package names.
                if line.contains(&format!("{crate_name}::")) {
                    users.push(path.file_name().unwrap().to_string_lossy().to_string());
                    break;
                }
            }
        }
        users.sort();
        users.dedup();
        assert_eq!(
            users,
            vec!["cms.rs".to_string()],
            "{crate_name} must be confined to the CMS module (design K25); found it in {users:?}"
        );
    }
}

/// The PFX module's public surface is DER, not the reader's types (design K25).
///
/// Written as a check on the re-export list because that is the exact place the boundary is
/// crossed: `pub use pfx::ImportedPfx` is fine, `pub use p12_keystore::KeyStore` would not be.
#[test]
fn the_signing_module_re_exports_only_its_own_types() {
    let module = read(&repo_root().join("src-tauri/src/editor/signature/mod.rs"));
    for line in module.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("pub use") {
            assert!(
                !trimmed.contains("p12_keystore"),
                "a keystore type must never be re-exported from the signing module: `{trimmed}`"
            );
        }
    }
}

/// The fixture that the PFX tests open is a throwaway identity, and that fact must stay recorded.
///
/// A committed private key is a decision that needs its reason next to it, or the next reader has
/// to guess whether it is a leak. The README is checked rather than trusted because the way this
/// goes wrong is someone deleting "just a test file".
#[test]
fn the_committed_test_key_is_documented_as_a_throwaway() {
    let readme = read(&repo_root().join("src-tauri/tests/fixtures/signing/README.md"));
    assert!(
        readme.contains("correct-horse"),
        "the fixture password must be published in the README, not implied"
    );
    assert!(
        readme.to_lowercase().contains("throwaway"),
        "the README must state that the committed key is a throwaway test identity"
    );
    assert!(
        readme.contains("never be used to sign anything a person relies on"),
        "the README must state the limitation, so a later reader does not treat the fixture as \
         usable key material"
    );
}
