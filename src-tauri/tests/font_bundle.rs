//! P2.7.6 (docs/P2.7.6-embed-DESIGN.md §4, R1): the bundled font payload and its record.
//!
//! These guards are deliberately built on **parsed bytes**, not on substrings. The record is
//! a markdown table, so a `contains` check would be satisfiable by a stray occurrence; the
//! hashes are therefore extracted per row and compared against a fresh SHA-256 of the
//! committed file, and the licence facts come out of each face's own `name` table via
//! `ttf-parser`.
//!
//! What each guard prevents:
//!
//! - a face silently dropped, swapped, truncated or added to `resources/fonts`
//! - the Narrow faces creeping back in, which `style_match` can never select (R1)
//! - a face shipped without the OFL notice or licence identifier in its name table
//! - the reserved font name being written as the primary name, which OFL condition 3
//!   forbids for a Modified Version (see FONT-RECORD.md)
//! - `LICENSE-Liberation.txt` removed, or the record losing a face it claims

use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn fonts_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources/fonts")
}

fn record() -> String {
    let path = fonts_dir().join("FONT-RECORD.md");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

/// Every `| \`File.ttf\` | bytes | \`hash\` | ... |` row in the record's audit table.
fn recorded_faces(record: &str) -> BTreeMap<String, (u64, String)> {
    let mut faces = BTreeMap::new();
    for line in record.lines() {
        let Some(rest) = line.strip_prefix("| `Liberation") else {
            continue;
        };
        let cells: Vec<&str> = rest
            .trim_end_matches('|')
            .split('|')
            .map(|c| c.trim().trim_matches('`'))
            .collect();
        // cells[0] is the tail of the filename after the stripped prefix.
        let name = format!("Liberation{}", cells[0]);
        let bytes: u64 = cells[1]
            .replace(',', "")
            .parse()
            .unwrap_or_else(|e| panic!("record row for {name} has no byte count: {e}"));
        let hash = cells[2].to_string();
        assert!(
            faces.insert(name.clone(), (bytes, hash)).is_none(),
            "the record lists {name} twice"
        );
    }
    faces
}

fn ttf_files() -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(fonts_dir())
        .unwrap_or_else(|e| panic!("cannot list {}: {e}", fonts_dir().display()))
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "ttf"))
        .collect();
    files.sort();
    files
}

fn name_record(face: &ttf_parser::Face<'_>, id: u16) -> String {
    face.names()
        .into_iter()
        .filter(|n| n.name_id == id && n.is_unicode())
        .filter_map(|n| n.to_string())
        .collect::<Vec<_>>()
        .join(" | ")
}

/// The directory's contents and the record agree exactly, and every hash matches the bytes.
///
/// This is the guard that makes the provenance claim in FONT-RECORD.md real: the record can
/// name a hash, but only re-hashing proves the committed file is the audited one.
#[test]
fn every_bundled_face_matches_its_recorded_hash() {
    let record = record();
    let recorded = recorded_faces(&record);
    let on_disk = ttf_files();

    assert_eq!(
        recorded.len(),
        12,
        "the record must list the 12 shipped faces, found {}",
        recorded.len()
    );
    assert_eq!(
        on_disk.len(),
        recorded.len(),
        "resources/fonts has {} .ttf files but the record lists {}; a face was added or \
         dropped without the record being updated",
        on_disk.len(),
        recorded.len()
    );

    let mut total = 0u64;
    for path in &on_disk {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .expect("font filename is not UTF-8")
            .to_string();
        let (recorded_bytes, recorded_hash) = recorded
            .get(&name)
            .unwrap_or_else(|| panic!("{name} is bundled but absent from FONT-RECORD.md"));
        let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("cannot read {name}: {e}"));
        assert_eq!(
            bytes.len() as u64,
            *recorded_bytes,
            "{name} is {} bytes; the record says {recorded_bytes}",
            bytes.len()
        );
        assert_eq!(
            sha256_hex(&bytes),
            *recorded_hash,
            "{name} does not hash to the recorded value; the shipped bytes are not the \
             audited ones"
        );
        total += bytes.len() as u64;
    }
    assert_eq!(
        total, 4_360_440,
        "the payload total changed; R1 froze it at 4,360,440 bytes"
    );
    assert!(
        record.contains("4,360,440"),
        "FONT-RECORD.md must state the frozen total"
    );
}

/// R1: a face ships only if it owns a style class no other shipped face owns.
///
/// Measured at freeze: the four Sans Narrow faces collapse onto the Sans classes, so
/// `style_match` can never address them. This asserts the consequence at the payload level,
/// so a Narrow face cannot be added back on the grounds that it "looks like" a small change.
#[test]
fn the_narrow_faces_are_not_bundled() {
    let narrow: Vec<String> = ttf_files()
        .iter()
        .filter_map(|p| p.file_name().and_then(|n| n.to_str()).map(str::to_string))
        .filter(|n| n.to_ascii_lowercase().contains("narrow"))
        .collect();
    assert!(
        narrow.is_empty(),
        "Narrow faces are unreachable through style_match and must not ship: {narrow:?}"
    );

    let families: Vec<(String, String)> = ttf_files()
        .iter()
        .map(|p| {
            let bytes = std::fs::read(p).expect("readable face");
            let face = ttf_parser::Face::parse(&bytes, 0)
                .unwrap_or_else(|e| panic!("{} is not a parseable TTF: {e}", p.display()));
            (name_record(&face, 1), name_record(&face, 2))
        })
        .collect();

    let mut classes = std::collections::BTreeSet::new();
    for (family, subfamily) in &families {
        let class = format!("{family}/{subfamily}");
        assert!(
            classes.insert(class.clone()),
            "two bundled faces present the same family/subfamily {class:?}; under R1 only \
             the face that owns a class may ship"
        );
    }
    assert_eq!(classes.len(), 12, "expected 12 distinct style classes");
}

/// OFL condition 2: the notice and the licence identifier must be *in* each face, not only
/// in a file beside it. This is what makes the bundled copy self-describing.
#[test]
fn every_face_carries_the_ofl_notice_in_its_name_table() {
    for path in ttf_files() {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let bytes = std::fs::read(&path).expect("readable face");
        let face = ttf_parser::Face::parse(&bytes, 0)
            .unwrap_or_else(|e| panic!("{name} is not a parseable TTF: {e}"));

        let copyright = name_record(&face, 0);
        let licence = name_record(&face, 13);
        let licence_url = name_record(&face, 14);

        assert!(
            copyright.contains("Red Hat") && copyright.contains("Google"),
            "{name} name ID 0 lost the copyright holders: {copyright:?}"
        );
        assert!(
            licence.contains("SIL Open Font License") && licence.contains("1.1"),
            "{name} name ID 13 must identify OFL 1.1: {licence:?}"
        );
        assert!(
            licence_url.contains("scripts.sil.org/OFL"),
            "{name} name ID 14 must point at the OFL: {licence_url:?}"
        );
    }
}

/// OFL condition 3: a Modified Version may not use the reserved name as its primary name.
///
/// A subset is a Modified Version (it deletes glyphs), so the name written into a PDF must
/// not be `Liberation`. The table in FONT-RECORD.md is the mapping the write path uses; this
/// guard keeps the mapping and the faces in step, and keeps the tokens free of the RFN.
#[test]
fn written_font_names_avoid_the_reserved_font_name() {
    let record = record();
    let section = record
        .split("| Style class | Bundled face | Written family token |")
        .nth(1)
        .expect("FONT-RECORD.md must keep the written-name table");

    let mut rows = 0;
    for line in section.lines() {
        if !line.starts_with("| ") || line.starts_with("| Style class") || line.starts_with("|---")
        {
            continue;
        }
        let cells: Vec<&str> = line.trim_matches('|').split('|').map(str::trim).collect();
        if cells.len() != 3 {
            continue;
        }
        let (face, token) = (cells[1], cells[2].trim_matches('`'));
        let face_file = if face.ends_with(".ttf") {
            face.to_string()
        } else {
            format!("{face}.ttf")
        };
        assert!(
            fonts_dir().join(&face_file).is_file(),
            "the written-name table names {face}, which is not bundled"
        );
        assert!(
            !token.to_ascii_lowercase().contains("liberation"),
            "the written family token {token:?} for {face} uses the Reserved Font Name; a \
             subset is a Modified Version (OFL condition 3)"
        );
        rows += 1;
    }
    assert_eq!(rows, 12, "the written-name table must cover all 12 faces");

    assert!(
        record.contains("Reserved Font Name"),
        "FONT-RECORD.md must record the RFN ruling, not just the table"
    );
}

/// The licence text travels with the binaries. Deleting it must fail a test, not just a
/// review.
#[test]
fn the_licence_ships_beside_the_faces() {
    let path = fonts_dir().join("LICENSE-Liberation.txt");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("the bundled licence is missing ({}): {e}", path.display()));
    for needle in [
        "SIL OPEN FONT LICENSE",
        "Reserved Font Name Liberation",
        "Copyright (c) 2012 Red Hat, Inc.",
    ] {
        assert!(
            text.contains(needle),
            "the bundled licence no longer contains {needle:?}"
        );
    }
}

/// The payload must actually be committable.
///
/// `resources/.gitignore` ignores `*` so that fetched PDFium binaries stay out of the repo. A
/// face can therefore sit in the working tree, pass every other guard here, and still never be
/// committed or built — the tests would run against files git was silently dropping. This
/// caught exactly that on the first attempt at this phase, so it is checked rather than
/// remembered.
#[test]
fn the_font_payload_is_not_gitignored() {
    let mut checked = 0;
    for path in ttf_files() {
        let output = std::process::Command::new("git")
            .arg("check-ignore")
            .arg("-q")
            .arg(&path)
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .status()
            .expect("git must be available to verify the payload is committable");
        assert!(
            !output.success(),
            "{} is ignored by git; it would pass the guards here and never ship",
            path.display()
        );
        checked += 1;
    }
    assert_eq!(checked, 12, "expected to check all 12 faces");

    for name in ["LICENSE-Liberation.txt", "FONT-RECORD.md"] {
        let output = std::process::Command::new("git")
            .arg("check-ignore")
            .arg("-q")
            .arg(fonts_dir().join(name))
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .status()
            .expect("git must be available");
        assert!(
            !output.success(),
            "fonts/{name} is ignored by git and would not ship"
        );
    }
}

/// Every face and the licence reach the installer, on the platform that ships one.
///
/// The Windows installer is the only shipping artifact, and `tauri.windows.conf.json`
/// overrides `bundle.resources` wholesale. A font payload declared only in the base config
/// could therefore be absent from the one build users install — the notice would be in git
/// and on the website, but not in the app. Both configs are checked by parsed key, not by
/// substring, so a renamed target or a dropped licence file fails.
#[test]
fn the_bundle_payload_includes_the_faces_and_the_licence() {
    for config in ["tauri.conf.json", "tauri.windows.conf.json"] {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(config);
        let raw = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
        let parsed: serde_json::Value = serde_json::from_str(&raw)
            .unwrap_or_else(|e| panic!("{config} is not valid JSON: {e}"));
        let resources = parsed["bundle"]["resources"]
            .as_object()
            .unwrap_or_else(|| panic!("{config} declares no bundle.resources"));

        assert!(
            resources
                .keys()
                .any(|k| k.starts_with("resources/fonts/") && k.ends_with(".ttf")),
            "{config} does not bundle the font faces; the write path would have no program"
        );
        assert!(
            resources
                .keys()
                .any(|k| k.ends_with("LICENSE-Liberation.txt")),
            "{config} does not bundle the OFL licence; condition 2 requires it to travel with \
             the fonts"
        );
        assert!(
            resources.keys().any(|k| k.ends_with("FONT-RECORD.md")),
            "{config} does not bundle the per-face record"
        );
    }
}

/// The record must be reachable from the design doc that requires it, so a reader can get
/// from the ruling to the evidence without guessing a path.
#[test]
fn the_design_doc_points_at_this_record() {
    let design = Path::new(env!("CARGO_MANIFEST_DIR")).join("../docs/P2.7.6-embed-DESIGN.md");
    let text = std::fs::read_to_string(&design)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", design.display()));
    assert!(
        text.contains("resources/fonts/FONT-RECORD.md"),
        "docs/P2.7.6-embed-DESIGN.md §4 must cite resources/fonts/FONT-RECORD.md"
    );
}

/// §4 obligation 2, from the Rust side: the About surface names the licence and the record.
///
/// `src/components/Notices.test.tsx` checks the rendered text; this checks the source, so the
/// notice cannot be dropped by deleting the component's test at the same time, and a Rust-only
/// gate still notices.
#[test]
fn the_about_surface_names_the_font_and_its_licence() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../src/components/Notices.tsx");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("the notices surface is missing ({}): {e}", path.display()));
    for needle in [
        "Liberation",
        "SIL Open Font License",
        "Version 1.1",
        "Red Hat",
        "Reserved Font Name Liberation",
        "LICENSE-Liberation.txt",
        "FONT-RECORD.md",
    ] {
        assert!(
            text.contains(needle),
            "src/components/Notices.tsx no longer states {needle:?}"
        );
    }

    let menu = Path::new(env!("CARGO_MANIFEST_DIR")).join("../src/components/Menu.tsx");
    let menu = std::fs::read_to_string(&menu)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", menu.display()));
    assert!(
        menu.contains("About and notices"),
        "the notices surface must be reachable from the menu, or the notice is not delivered"
    );
}
