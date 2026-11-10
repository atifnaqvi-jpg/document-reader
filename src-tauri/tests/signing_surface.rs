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

/// Whether `line` names the package `crate_name` as a path prefix.
///
/// A plain substring test is wrong here and was wrong for as long as every CMS crate lived in one
/// file: `"cms::"` occurs inside `x509_cert_cms::`, `der_cms::` and `spki_cms::`, so asking whether
/// `cms` is used answered yes for the aliased generations. The match therefore requires that the
/// character before the name is not part of an identifier -- no `_x509_cert_` before the `cms`.
fn names_crate(line: &str, crate_name: &str) -> bool {
    let needle = format!("{crate_name}::");
    let bytes = line.as_bytes();
    line.match_indices(&needle).any(|(at, _)| {
        at == 0
            || !matches!(
                bytes[at - 1],
                b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_'
            )
    })
}

/// K25 for the CMS stack: `cms`, `rsa`, `x509-cert 0.2` and `der 0.7` are confined to the two files
/// that form the CMS boundary.
///
/// The signer sits on a second `der`/`x509-cert` generation than the rest of the crate (design
/// K24 predicted exactly this). The boundary is honoured only if no CMS-crate type escapes those
/// files: a `der 0.7` value handed to `der 0.8` code does not compile, but the reverse mistake --
/// threading an `x509-cert 0.3` certificate into the CMS builder -- is caught here rather than at
/// review.
///
/// **Two files, not one.** `cms.rs` builds a signature and `verify.rs` reads one back. Design §4
/// plans verification as its own module for I-SG1..I-SG5, and D-SG7's badges cannot report
/// `Integrity` at all unless production can compute it, so the verifier cannot live in a test file.
/// What must stay confined is the *generation boundary*, and both files are on the far side of it:
/// `x509-cert 0.2`'s `subject_public_key_info` field is an `spki 0.7` type, so a verifier that reads a
/// certificate's key is unavoidably a `der 0.7` consumer.
///
/// Widening this list is a real cost and is bounded by the second half of the test: neither file may
/// publicly expose a CMS-generation type. That is what stops the boundary creeping outward one
/// return type at a time.
///
/// `sha2` is deliberately absent from this list: the crate already uses both generations, and a
/// digest has no cross-generation identity problem.
#[test]
fn the_cms_crates_are_confined_to_the_cms_and_verify_modules() {
    // Per-crate expectations rather than one list. The two boundary files do not import the same
    // packages, and a shared list would either hide a leak or fail on a correct tree: `verify.rs`
    // reaches `der 0.7` through `x509_cert_cms::der` (that crate's own re-export) rather than the
    // `der_cms` alias, and reads no `spki_cms` type by name.
    //
    // `cms` itself is expected only in `cms.rs`: reading a signature does not need the builder, and
    // if `verify.rs` ever starts using it that is a design change worth seeing in this diff.
    let expected: [(&str, &[&str]); 5] = [
        ("cms", &["cms.rs"]),
        ("rsa", &["cms.rs", "verify.rs"]),
        ("x509_cert_cms", &["cms.rs", "verify.rs"]),
        ("der_cms", &["cms.rs"]),
        ("spki_cms", &["cms.rs"]),
    ];

    for (crate_name, allowed) in expected {
        let mut users = Vec::new();
        for (path, text) in signing_sources() {
            for line in text.lines() {
                if line.trim_start().starts_with("//") {
                    continue;
                }
                // Match `use cms::`, `cms::builder::...` and the aliased package names, but not
                // our own module that happens to share the name: `crate::editor::signature::cms`
                // and `super::cms` are this crate's signer, not the `cms` package. Without this
                // the guard fired on every sibling module that used the signer.
                let mut own = line.to_string();
                for prefix in ["signature::cms::", "super::cms::", "self::cms::"] {
                    own = own.replace(prefix, "");
                }
                if names_crate(&own, crate_name) {
                    users.push(path.file_name().unwrap().to_string_lossy().to_string());
                    break;
                }
            }
        }
        users.sort();
        users.dedup();
        let allowed: Vec<String> = allowed.iter().map(|name| (*name).to_string()).collect();
        assert_eq!(
            users, allowed,
            "{crate_name} must be confined to {allowed:?} (design K25); found it in {users:?}"
        );
    }
}

/// The other half of K25: neither CMS-boundary file may **publicly** hand out a CMS-crate type.
///
/// Confinement by file would be satisfied by a `pub fn` returning a `der 0.7` value to the rest of
/// the crate, which is the leak the file check is trying to prevent. `pub(super)` is refused along
/// with `pub` because `signature::verify` is reachable from anywhere in the crate through
/// `editor::signature`.
///
/// Return types only, and that is a deliberate limit rather than an oversight: an argument of a
/// CMS type can only be supplied by code already inside the boundary, so it is not an escape.
#[test]
fn no_cms_type_escapes_the_boundary_files() {
    const CMS_CRATES: [&str; 5] = [
        "cms::",
        "rsa::",
        "x509_cert_cms::",
        "der_cms::",
        "spki_cms::",
    ];
    let mut offenders = Vec::new();
    for (path, text) in signing_sources() {
        if !matches!(
            path.file_name().and_then(|name| name.to_str()),
            Some("cms.rs" | "verify.rs")
        ) {
            continue;
        }
        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("//") || !trimmed.starts_with("pub") {
                continue;
            }
            // A `pub struct`/`pub enum`/`pub type` names its own type; only an `fn` hands one out.
            let Some(signature) = trimmed.split("->").nth(1) else {
                continue;
            };
            if CMS_CRATES.iter().any(|name| signature.contains(name)) {
                offenders.push(format!("{}: {trimmed}", path.display()));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "a public function returns a CMS-generation type, which lets the boundary escape:\n{}",
        offenders.join("\n")
    );
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

// ---------------------------------------------------------------------------------------------
// D-SG3 as a *compile-surface* property, not a convention (2026-11-09).
//
// The scan above is a source-text test. It fails when someone runs it, it reads strings and
// comments as if they were code, and it cannot see a type reached through a helper in another
// module. The design calls D-SG3 "a compile-surface property we can test", so the ban also lives
// in `clippy.toml` as `disallowed-types`, which the compiler enforces and which CI turns into a
// build failure with `-D warnings`.
//
// `clippy.toml` cannot be trusted on its own, and this was measured rather than assumed: an
// **unknown type name in `disallowed-types` is silently tolerated**. Clippy does not report a name
// it cannot resolve, so a typo or a crate rename turns an entry into a no-op while the config still
// *looks* like it is banning something. A list of bans that no longer bite is worse than no list,
// because it is recorded as coverage.
//
// So each name is proved by compiling a probe that names it. The proof is a compile that must
// fail, and the failure must mention the lint -- a *different* compile error would mean the probe
// itself is broken and nothing was learned.
// ---------------------------------------------------------------------------------------------

use std::process::Command;

/// Every type the sign or verify path must not be able to name, paired with a probe that uses it.
///
/// Kept here rather than read out of `clippy.toml` on purpose. If this list were parsed from the
/// config, deleting an entry from the config would delete its test, and the guard would shrink
/// silently -- the exact vacuity this file exists to prevent. The two lists are compared below, so
/// they cannot drift apart without a failure.
const BANNED_TYPES: [(&str, &str); 6] = [
    (
        "std::net::TcpStream",
        "let _ = std::net::TcpStream::connect(\"127.0.0.1:1\");",
    ),
    (
        "std::net::TcpListener",
        "let _ = std::net::TcpListener::bind(\"127.0.0.1:0\");",
    ),
    (
        "std::net::UdpSocket",
        "let _ = std::net::UdpSocket::bind(\"127.0.0.1:0\");",
    ),
    (
        "std::net::SocketAddr",
        "let _: std::net::SocketAddr = \"127.0.0.1:0\".parse().unwrap();",
    ),
    (
        "reqwest::Client",
        "let _ = std::any::type_name::<reqwest::Client>();",
    ),
    (
        "reqwest::blocking::Client",
        "let _ = std::any::type_name::<reqwest::blocking::Client>();",
    ),
];

/// The names `clippy.toml` claims to ban, parsed from the config the compiler actually reads.
fn banned_types_in_config() -> Vec<String> {
    let text = read(&repo_root().join("src-tauri/clippy.toml"));
    // `disallowed-types` may be written across lines; collect everything inside the brackets. A
    // bare `contains` on the whole file would also match the prose in the comment that explains
    // the list, and would then pass with an empty array.
    let start = text
        .find("disallowed-types")
        .expect("clippy.toml must declare disallowed-types");
    let body = &text[start..];
    let open = body.find('[').expect("the list is bracketed");
    let close = body[open..].find(']').expect("the list is closed") + open;
    body[open + 1..close]
        .split(',')
        .map(|entry| entry.trim().trim_matches('"').trim().to_string())
        .filter(|entry| !entry.is_empty())
        .collect()
}

/// The two lists must name the same types, or one of them is stale.
#[test]
fn the_compile_surface_ban_and_its_probes_name_the_same_types() {
    let mut config = banned_types_in_config();
    config.sort();
    let mut probed: Vec<String> = BANNED_TYPES
        .iter()
        .map(|(name, _)| name.to_string())
        .collect();
    probed.sort();
    assert_eq!(
        config, probed,
        "clippy.toml's disallowed-types and the probes in this test have drifted. An entry in the \
         config with no probe is a ban nobody has proved still bites; a probe with no entry is a \
         test of nothing."
    );
}

/// Each banned type must be a build failure when named, and the failure must be *this* lint --
/// and the same probe must compile when the ban is removed.
///
/// That pair is the point. A lone "the probe failed" result proves nothing: the first version of
/// this test accepted any failure, and `reqwest::Client` in a throwaway crate fails with `E0433`
/// (unresolved crate), which is not the ban at all. Measured, and it looked like a pass. So each
/// probe is compiled twice against the *same* generated code:
///
/// 1. With `clippy.toml` as shipped, it must fail, and the error must name the lint.
/// 2. With an empty ban list, it must succeed -- which proves the probe is otherwise valid and
///    that the ban is what rejected it.
///
/// A crate that is not a dependency of a throwaway probe is not resolvable at all, so the
/// `reqwest` probes get a three-line stub crate. A real `reqwest` would mean a ~19 s network
/// fetch inside the gate; the stub exercises the identical lint path in under a second.
#[test]
fn every_banned_type_is_rejected_by_the_compiler() {
    let shipped = read(&repo_root().join("src-tauri/clippy.toml"));
    for (name, probe) in BANNED_TYPES {
        let project = tempfile::tempdir().expect("temp dir");
        let dir = project.path();

        // A stub crate when the probe names one. `std::*` needs nothing.
        let dependency = if name.starts_with("reqwest::") {
            let stub = dir.join("reqwest_stub");
            std::fs::create_dir_all(stub.join("src")).expect("stub dirs");
            std::fs::write(
                stub.join("Cargo.toml"),
                // No `[workspace]` table: a second root inside the probe's directory makes cargo
                // refuse with "multiple workspace roots", which the control run catches as a
                // broken probe instead of letting it pass as a caught one.
                "[package]\nname = \"reqwest\"\nversion = \"0.0.0\"\nedition = \"2021\"\n",
            )
            .expect("stub manifest");
            std::fs::write(
                stub.join("src/lib.rs"),
                "pub struct Client;\npub mod blocking { pub struct Client; }\n",
            )
            .expect("stub source");
            "reqwest = { path = \"./reqwest_stub\" }\n"
        } else {
            ""
        };

        std::fs::write(
            dir.join("Cargo.toml"),
            format!(
                "[package]\nname = \"probe\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n                 [dependencies]\n{dependency}\n[workspace]\n"
            ),
        )
        .expect("write manifest");
        std::fs::create_dir(dir.join("src")).expect("src dir");
        std::fs::write(
            dir.join("src/lib.rs"),
            format!("pub fn probe() {{\n    {probe}\n}}\n"),
        )
        .expect("write probe");

        let run = |config: &str| -> (bool, String) {
            std::fs::write(dir.join("clippy.toml"), config).expect("write config");
            let output = Command::new("cargo")
                .args(["clippy", "--quiet", "--", "-D", "warnings"])
                .current_dir(dir)
                .output()
                .expect("cargo clippy must be on PATH; a skip here would read as a pass");
            (
                output.status.success(),
                String::from_utf8_lossy(&output.stderr).to_string(),
            )
        };

        // Control first: with no ban the probe must compile, or the probe is broken and the
        // failing run below would prove nothing.
        let (control_ok, control_err) = run("disallowed-types = []\n");
        assert!(
            control_ok,
            "the probe naming `{name}` does not compile even with the ban removed, so it cannot \
             test the ban. Fix the probe.\nstderr:\n{control_err}"
        );

        let (ok, stderr) = run(&shipped);
        assert!(
            !ok,
            "a probe naming `{name}` compiled against the shipped clippy.toml; the ban is a no-op \
             for it. Clippy silently tolerates type names it cannot resolve, so a rotted entry \
             looks exactly like this."
        );
        assert!(
            stderr.contains("disallowed"),
            "the probe naming `{name}` failed, but not with the disallowed-type lint, so nothing \
             was proved about the ban.\nstderr:\n{stderr}"
        );
    }
}
