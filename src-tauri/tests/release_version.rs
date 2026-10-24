//! The release must not lie about its own version, and a version must not ship nameless.
//!
//! Six copies of the version number exist: `package.json`, the two in `package-lock.json`
//! (root and the root package entry), `src-tauri/tauri.conf.json` (which names the
//! installer), `src-tauri/Cargo.toml`, and `src-tauri/Cargo.lock`. A bump that misses one
//! produces an installer named `0.5.0` that reports `0.4.5` internally, and a stale
//! `Cargo.lock` survives `cargo metadata --no-deps`, so nothing else notices. That is not
//! hypothetical here: `package-lock.json` had drifted to `0.2.1` while everything else was
//! at `0.4.5`. Hence a test rather than a checklist.
//!
//! The second invariant is T-4's own acceptance: the notes exist for the version that is
//! about to ship, and they name what shipped. A release note that omits a change the plan
//! claims is how a "known gap" quietly becomes a surprise.

use std::path::{Path, PathBuf};

use serde_json::Value;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("src-tauri must have a parent directory")
        .to_path_buf()
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

fn json_at(path: &Path) -> Value {
    serde_json::from_str(&read(path))
        .unwrap_or_else(|e| panic!("{} is not valid JSON: {e}", path.display()))
}

fn json_version(path: &Path, pointer: &str) -> String {
    json_at(path)
        .pointer(pointer)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("{} has no string at {pointer}", path.display()))
        .to_string()
}

/// Reads `version` out of the `[package]` table, which is the only `version` key we own in
/// that file (dependency entries carry their own).
fn cargo_toml_version() -> String {
    let mut in_package = false;
    for line in read(&repo_root().join("src-tauri/Cargo.toml")).lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_package = line == "[package]";
            continue;
        }
        if in_package {
            if let Some(rest) = line.strip_prefix("version") {
                return rest
                    .trim_start_matches([' ', '='])
                    .trim_matches('"')
                    .to_string();
            }
        }
    }
    panic!("no `version` key under `[package]` in Cargo.toml");
}

/// Reads the `document-reader` package entry out of `Cargo.lock`. A stale lock keeps the
/// old version here while `cargo metadata --no-deps` stays silent.
fn cargo_lock_version() -> String {
    let text = read(&repo_root().join("src-tauri/Cargo.lock"));
    let mut name: Option<String> = None;
    for line in text.lines() {
        let line = line.trim();
        if line == "[[package]]" {
            name = None;
            continue;
        }
        if let Some(rest) = line.strip_prefix("name = ") {
            name = Some(rest.trim_matches('"').to_string());
            continue;
        }
        if let Some(rest) = line.strip_prefix("version = ") {
            if name.as_deref() == Some("document-reader") {
                return rest.trim_matches('"').to_string();
            }
        }
    }
    panic!("no `[[package]] name = \"document-reader\"` entry in Cargo.lock");
}

#[test]
fn every_release_carrier_reports_the_same_version() {
    let root = repo_root();
    let crate_version = cargo_toml_version();

    let carriers = [
        (
            "package.json",
            json_version(&root.join("package.json"), "/version"),
        ),
        (
            "package-lock.json root version",
            json_version(&root.join("package-lock.json"), "/version"),
        ),
        (
            "package-lock.json packages[\"\"].version",
            json_version(&root.join("package-lock.json"), "/packages//version"),
        ),
        (
            "src-tauri/tauri.conf.json",
            json_version(&root.join("src-tauri/tauri.conf.json"), "/version"),
        ),
        ("src-tauri/Cargo.toml", crate_version.clone()),
        ("src-tauri/Cargo.lock", cargo_lock_version()),
    ];

    let disagreements: Vec<String> = carriers
        .iter()
        .filter(|(_, version)| *version != crate_version)
        .map(|(name, version)| format!("{name} says {version}"))
        .collect();

    assert!(
        disagreements.is_empty(),
        "the release version is {crate_version}, but {} -- an installer built from this \
         tree would be named one version and report another",
        disagreements.join(", ")
    );
}

/// The body of `docs/RELEASE-NOTES.md` under the `## <version>` heading, up to the next
/// `## ` heading.
fn release_section(version: &str) -> Option<String> {
    let text = read(&repo_root().join("docs/RELEASE-NOTES.md"));
    let mut section: Option<String> = None;
    for line in text.lines() {
        if let Some(body) = line.strip_prefix("## ") {
            if body.split_whitespace().next() == Some(version) {
                section = Some(String::new());
                continue;
            }
            if section.is_some() {
                break;
            }
        }
        if let Some(body) = section.as_mut() {
            body.push_str(line);
            body.push('\n');
        }
    }
    section
}

#[test]
fn the_shipped_version_has_release_notes() {
    let version = cargo_toml_version();

    assert!(
        release_section(&version).is_some(),
        "docs/RELEASE-NOTES.md has no `## {version}` section for the version about to ship"
    );
}

/// T-4's acceptance: `Ts`, PDFDocEncoding, copy-on-write and the MCID work all ship in
/// 0.5.0, so all four must be listed. Point-in-time by design -- a later phase writes its
/// own section.
#[test]
fn the_0_5_0_release_notes_cover_every_change_the_plan_claims() {
    let section = release_section("0.5.0").expect("docs/RELEASE-NOTES.md has no 0.5.0 section");
    let lower = section.to_lowercase();

    for (topic, needle) in [
        ("MCID boundaries", "mcid"),
        ("ActualText preservation", "actualtext"),
        ("Ts text rise", "text rise"),
        ("PDFDocEncoding", "pdfdocencoding"),
        ("copy-on-write (B-7)", "copy-on-write"),
    ] {
        assert!(
            lower.contains(needle),
            "the 0.5.0 notes never mention {topic} (looked for {needle:?})"
        );
    }
}
