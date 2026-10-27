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

/// The version in the tree must not be one a published tag already owns.
///
/// `the_shipped_version_has_release_notes` only asks whether a section exists. It therefore
/// passed for weeks while the tree said `0.8.1`, carried commits that `v0.8.1` did not
/// contain, and matched the *published* 0.8.1 section whose content described a different
/// tree. That is a guard satisfied by a coincidence: the notes were green because the number
/// was stale, not because the tree matched the notes.
///
/// The property that actually matters is: if `HEAD` is not the commit a published tag points
/// at, then the carrier version must be ahead of every published tag. A tree one commit past
/// `v0.9.0` with carriers still at `0.9.0` re-creates the defect, so this is checked against
/// tags rather than against the single case that was found.
///
/// Skipped when git or the tag is unavailable (a source tarball has no tags); it must not fail
/// for a reason unrelated to the property.
#[test]
fn the_tree_version_is_ahead_of_every_published_tag() {
    let root = repo_root();
    let tag = std::process::Command::new("git")
        .args(["describe", "--tags", "--exact-match", "HEAD"])
        .current_dir(&root)
        .output();

    let Ok(tag) = tag else {
        eprintln!("git unavailable; skipping the published-tag comparison");
        return;
    };
    if tag.status.success() {
        // HEAD *is* a tagged release: the carriers are expected to name it.
        return;
    }

    let listed = std::process::Command::new("git")
        .args(["tag", "--list", "v*"])
        .current_dir(&root)
        .output()
        .expect("git tag --list must run");
    // Explicit shape, not "whatever strip_prefix('v') leaves". The list also carries
    // `version-0.4.4`, which survived only because `ersion-0.4.4` happened to fail semver
    // parsing. Incidental protection changes meaning when circumstances change: a tag named
    // `v1.0.0-rc1` or `version-9.9.9` must be *declared* not-a-release here, not excluded by
    // a parse accident downstream.
    let mut published: Vec<(u64, u64, u64)> = String::from_utf8_lossy(&listed.stdout)
        .lines()
        .filter_map(published_semver_tag)
        .collect();
    if published.is_empty() {
        eprintln!("no version tags found; skipping");
        return;
    }
    published.sort_unstable();
    let highest = *published.last().expect("non-empty");

    let current = parse_semver(&cargo_toml_version())
        .unwrap_or_else(|| panic!("Cargo.toml version is not plain semver: three numbers"));

    assert!(
        current > highest,
        "HEAD is untagged but the carriers say {}.{}.{}, which is not ahead of the highest \
         published tag v{}.{}.{}; an installer built here would claim a version whose \
         published artifact does not contain these commits, and the release-notes guard would \
         pass against the published section",
        current.0,
        current.1,
        current.2,
        highest.0,
        highest.1,
        highest.2
    );
}

fn parse_semver(text: &str) -> Option<(u64, u64, u64)> {
    let mut parts = text.trim().split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

/// A tag names a published release only when it is exactly `v` followed by three plain
/// numeric components.
///
/// The strictness is the point. The repository also carries `version-0.4.4`, which the old
/// `strip_prefix('v')` + parse chain excluded by accident: `ersion-0.4.4` is not semver, so
/// it fell out at the parse step rather than being declared not-a-release. That works until
/// someone adds a tag whose remainder happens to parse, at which point a non-release becomes
/// a comparison floor. Requiring the shape up front makes the exclusion intentional.
fn published_semver_tag(tag: &str) -> Option<(u64, u64, u64)> {
    let digits = tag.trim().strip_prefix('v')?;
    if digits.matches('.').count() != 2 {
        return None;
    }
    if !digits
        .split('.')
        .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
    {
        return None;
    }
    parse_semver(digits)
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

/// The programme boundary is P2.8, not P2.7.6: the deferred full-parity inventory is
/// produced **after reflow ships**, and the plan must say so where the next reader looks —
/// in the boundary note and in the sequencing note after the phase table.
///
/// Why a test: this is a pure documentation claim with no compiler behind it, and the two
/// plausible readings ("close P2.7.6, then inventory" vs "close 1.0.0, then inventory")
/// differ by a whole phase. A reader who takes the wrong one starts the inventory early,
/// which is the scope error ruling B-1 exists to prevent.
///
/// The assertion is on the *sentence*, not on the file: `contains("P2.8")` anywhere would
/// pass on the phase table alone, which has named P2.8 since the plan was frozen. That is
/// the vacuous-guard shape this programme has already caught seven times, so the notes are
/// isolated first and only then searched.
#[test]
fn the_plan_defers_the_full_parity_inventory_to_after_p2_8() {
    let plan = read(&repo_root().join("docs/P2.7-TEXT-EDITING-PARITY-PLAN.md"));

    let boundary = plan
        .lines()
        .skip_while(|line| !line.contains("**Programme boundary (B-1"))
        .take_while(|line| line.trim_start().starts_with('>'))
        .collect::<Vec<_>>()
        .join(" ");
    let boundary = boundary.to_lowercase();
    assert!(
        !boundary.is_empty(),
        "the plan has no '**Programme boundary (B-1' note; the deferral point is unstated"
    );

    // The deferral-point *sentence*, not the whole note. The note names P2.8 more than once
    // ("after P2.8", then "produced once, after P2.8 ships"), so a contains over the block
    // survives flipping the sentence that actually states the boundary. Mutation-verified:
    // this exact mutation passed the whole-block version.
    let deferral = boundary
        .split("the deferral point is")
        .nth(1)
        .map(|tail| tail.split(". ").next().unwrap_or(tail))
        .unwrap_or("")
        .trim_start_matches(['*', ' ']);
    assert!(
        deferral.starts_with("after p2.8") && deferral.contains("not after p2.7.6"),
        "the deferral point must read 'after P2.8 ... not after P2.7.6'; sentence: {deferral:?}"
    );

    for needle in ["0.9.0", "1.0.0", "must not be produced"] {
        assert!(
            boundary.contains(needle),
            "the programme-boundary note never says {needle:?}: {boundary}"
        );
    }

    let after_table = plan
        .lines()
        .skip_while(|line| !line.contains("**What comes after which"))
        .take_while(|line| !line.trim().is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let after_table = after_table.to_lowercase();
    assert!(
        after_table.contains("after p2.8") && after_table.contains("last phase before reflow"),
        "the sequencing note after the phase table must say P2.7.6 is the last phase before \
         reflow and the inventory follows P2.8: {after_table}"
    );

    // Same claim in the section that records the ruling, so the two cannot drift.
    let questions = read(&repo_root().join("docs/P2.7-REVIEW-OPEN-QUESTIONS.md"));
    let b1 = questions
        .lines()
        .find(|line| line.contains("| **B-1** |"))
        .unwrap_or_else(|| panic!("P2.7-REVIEW-OPEN-QUESTIONS.md has no B-1 row"));
    let b1_lower = b1.to_lowercase();
    assert!(
        b1_lower.contains("after p2.8") && b1_lower.contains("full-parity inventory"),
        "the B-1 ruling row must place the full-parity inventory after P2.8: {b1}"
    );
}

/// The tag predicate is fed the shapes that actually occur, including the one that was
/// previously excluded by accident.
///
/// `version-0.4.4` is the real tag in this repository. It must be rejected because it is not
/// a release tag, not because `ersion-0.4.4` fails to parse -- those are different reasons
/// and only one of them survives the next tag someone adds.
#[test]
fn only_a_bare_v_triple_names_a_published_release() {
    assert_eq!(published_semver_tag("v0.8.1"), Some((0, 8, 1)));
    assert_eq!(published_semver_tag("v0.6.0"), Some((0, 6, 0)));
    assert_eq!(published_semver_tag("  v1.2.3  "), Some((1, 2, 3)));

    for rejected in [
        "version-0.4.4",  // the real tag in this repo: not a release, by shape
        "v1.0.0-rc1",     // pre-release: not a published release for this guard
        "v1.0",           // two components
        "v1.0.0.0",       // four components
        "v1..0",          // empty component
        "v",              // no components
        "1.2.3",          // no prefix
        "V1.2.3",         // wrong case
        "release-v1.2.3", // prefix elsewhere
        // The two that actually distinguish this predicate from the old chain. Rust's
        // integer parser accepts a leading space and a leading `+`, so `strip_prefix('v')`
        // followed by `parse()` treated both of these as published releases. They are the
        // cases that make this test a test; without them every entry above is rejected by
        // the old code too, for a different reason, and the mutant survives.
        "v 1.2.3",
        "v+1.2.3",
    ] {
        assert_eq!(
            published_semver_tag(rejected),
            None,
            "{rejected:?} must be declared not-a-release by shape, not excluded downstream"
        );
    }
}
