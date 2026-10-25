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

/// X4's acceptance: 0.6.0 ships P2.7.3, so the notes must say what a user gets and what is
/// still refused. Same point-in-time rule as the 0.5.0 test above.
#[test]
fn the_0_6_0_release_notes_cover_every_change_the_plan_claims() {
    let section = release_section("0.6.0").expect("docs/RELEASE-NOTES.md has no 0.6.0 section");
    let lower = section.to_lowercase();

    for (topic, needle) in [
        ("Form XObject text editing", "form xobject"),
        ("the per-draw edit unit (Q2)", "per draw"),
        ("the private /Resources copy (Q3)", "private"),
        ("the nesting cap and its refusal (Q1)", "nested-xobject"),
    ] {
        assert!(
            lower.contains(needle),
            "the 0.6.0 notes never mention {topic} (looked for {needle:?})"
        );
    }
}

/// C-4's acceptance: 0.8.0 ships P2.7.5, so the notes must name the CID work and the two
/// defects measuring it exposed — a user upgrading needs to know that merging used to discard
/// glyphs and that a substituted font is no longer chosen for no reason.
///
/// The numbering assertion is here because the phase was drafted as "P2.8 / 0.7.0" and the
/// plan reserves both for something else. A notes section that repeats the mis-numbering
/// would keep the contradiction alive in the one document users actually read.
#[test]
fn the_0_8_0_release_notes_cover_every_change_the_plan_claims() {
    let section = release_section("0.8.0").expect("docs/RELEASE-NOTES.md has no 0.8.0 section");
    let lower = section.to_lowercase();

    for (topic, needle) in [
        ("CID / Type0 in-place editing", "type0"),
        ("the /ToUnicode round-trip", "/tounicode"),
        ("the face-based merge rule", "face"),
        ("the compressed-font fix", "compressed"),
        ("the refusal copy fix", "annotation"),
        ("the corrected phase number", "p2.7.5"),
    ] {
        assert!(
            lower.contains(needle),
            "the 0.8.0 notes never mention {topic} (looked for {needle:?})"
        );
    }

    assert!(
        lower.contains("0.6.0") && lower.contains("editable"),
        "the 0.8.0 notes must connect the refusal in 0.6.0 to the fix, since that is the \
         reported symptom users will be checking"
    );

    // Q9 makes the phase's `[x]` conditional on a confirmed installer, and the confirmation
    // exists only as a hand-test. If the notes drop it, nothing else records that 0.8.0 was
    // accepted rather than merely built.
    for (topic, needle) in [
        ("the hand-test acceptance", "hand-tested"),
        ("the installer checksum", "sha-256"),
        ("the pre-release warning", "pre-release"),
    ] {
        assert!(
            lower.contains(needle),
            "the 0.8.0 notes never mention {topic} (looked for {needle:?})"
        );
    }
    assert!(
        section.contains("40157718290") && section.contains("40158689217"),
        "the 0.8.0 notes must name the gate run and the installer build run, or the \
         acceptance claim cannot be checked against anything"
    );
}

/// Q7's corrected map, pinned because the mistake it fixes is a *documentation* mistake that
/// no compiler can see: 0.7.0 was reserved for P2.7.4, then 0.8.0 (P2.7.5) published first,
/// which left the transform phase numbered below a published release. The version was raised
/// to 0.8.1 so published history stays monotonic.
///
/// The map is read **cell by cell** in both documents. `contains` anywhere in a section is
/// not a check: an early draft of this test asserted the row's whole text contained "0.8.1"
/// and passed with the version cell restored to 0.7.0, because the row also names its
/// installer (`Document Reader_0.8.1_x64-setup.exe`). Measured, not assumed -- the mutation
/// was run. The map is the claim, so the map's own cell is what is compared.
fn version_cell(row: &str) -> String {
    let fields: Vec<&str> = row.split('|').collect();
    fields
        .iter()
        .rev()
        .find(|field| !field.trim().is_empty())
        .map(|field| {
            field
                .trim()
                .trim_start_matches("**")
                .split_whitespace()
                .next()
                .unwrap_or("")
                .trim_matches('*')
                .to_string()
        })
        .unwrap_or_else(|| panic!("no version cell in row: {row}"))
}

#[test]
fn the_0_8_1_notes_record_why_the_version_is_not_0_7_0() {
    let section = release_section("0.8.1").expect("docs/RELEASE-NOTES.md has no 0.8.1 section");
    let lower = section.to_lowercase();

    for (topic, needle) in [
        ("the phase it closes", "p2.7.4"),
        ("the superseded 0.7.0 target", "0.7.0"),
        ("the release that published first", "0.8.0"),
        ("the reason it was raised", "semver"),
        ("the divergence itself", "diverged"),
    ] {
        assert!(
            lower.contains(needle),
            "the 0.8.1 notes never mention {topic} (looked for {needle:?})"
        );
    }

    // The corrected map, in the document users read.
    let map: Vec<&str> = section
        .lines()
        .filter(|line| line.starts_with("| P2.7.") || line.starts_with("| P2.8"))
        .collect();
    assert!(
        map.len() >= 5,
        "the 0.8.1 notes must carry the corrected version map; found {} rows",
        map.len()
    );
    let row = |phase: &str| -> String {
        map.iter()
            .find(|line| line.starts_with(&format!("| {phase}")))
            .unwrap_or_else(|| panic!("the 0.8.1 map has no {phase} row"))
            .to_string()
    };
    assert_eq!(
        version_cell(&row("P2.7.4")),
        "0.8.1",
        "the map must name 0.8.1 for P2.7.4 (the release this section is about)"
    );
    assert_eq!(
        version_cell(&row("P2.7.6")),
        "0.9.0",
        "the map must name 0.9.0 for P2.7.6: it is the last minor before 1.0.0, and leaving it \
         at 0.8.x is the wall this correction exists to avoid"
    );
    assert_eq!(
        version_cell(&row("P2.7.5")),
        "0.8.0",
        "the map must keep 0.8.0 attached to P2.7.5 -- the phase that published first is the \
         fact the note has to explain"
    );

    // The same map in the plan, where the phase table is the contract.
    let plan = read(&repo_root().join("docs/P2.7-TEXT-EDITING-PARITY-PLAN.md"));
    let plan_row = |phase: &str| -> String {
        plan.lines()
            .find(|line| line.starts_with(&format!("| {phase} |")))
            .unwrap_or_else(|| panic!("the plan has no {phase} row"))
            .to_string()
    };
    assert!(
        version_cell(&plan_row("P2.7.4")).starts_with("0.8.1"),
        "the plan's P2.7.4 row must name 0.8.1 as the shipped version; cell: {:?}",
        version_cell(&plan_row("P2.7.4"))
    );
    assert_eq!(
        version_cell(&plan_row("P2.7.6")),
        "0.9.0",
        "the plan's P2.7.6 row must target 0.9.0; cell: {:?}",
        version_cell(&plan_row("P2.7.6"))
    );
}
