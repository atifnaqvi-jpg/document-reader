//! The product name is stated in several places, and they must agree.
//!
//! D-R9 renamed the app to `DocuPro` in the config, the window title, the `index.html` title,
//! the Toolbar label, the Notices sentence and the README heading. Before this file, **no
//! test asserted the product name at all** (D-R1's N9): a tree could ship an installer named
//! one thing and a window titled another with every gate green.
//!
//! The version has had a guard of this shape since 0.6.0, for the same reason. This is the
//! name's.
//!
//! Deliberately **not** asserted here: the published asset name. GitHub rewrites spaces in
//! asset names, and `DocuPro` has none, so the disk and published forms coincide (D-R8).
//! Nothing in the tree can see the published form anyway; that is a fact about the release
//! page, recorded in `docs/RELEASE-NOTES.md`.

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

fn product_name() -> String {
    let path = repo_root().join("src-tauri/tauri.conf.json");
    let config: Value = serde_json::from_str(&read(&path))
        .unwrap_or_else(|e| panic!("{} is not valid JSON: {e}", path.display()));
    config
        .pointer("/productName")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("{} has no string at /productName", path.display()))
        .to_string()
}

#[test]
fn the_product_name_is_stated_once_and_agrees_everywhere() {
    let name = product_name();
    let root = repo_root();

    let window_title = {
        let config: Value = serde_json::from_str(&read(&root.join("src-tauri/tauri.conf.json")))
            .expect("tauri.conf.json parses");
        config
            .pointer("/app/windows/0/title")
            .and_then(Value::as_str)
            .expect("a window title")
            .to_string()
    };
    assert_eq!(
        window_title, name,
        "the window title and `productName` disagree: a user sees one name in the title bar \
         and an installer named the other"
    );

    let expected_title = format!("<title>{name}</title>");
    assert!(
        read(&root.join("index.html")).contains(&expected_title),
        "index.html does not carry `{expected_title}`"
    );

    let toolbar = read(&root.join("src/components/Toolbar.tsx"));
    let expected_label = format!(">{name}<");
    assert!(
        toolbar.contains(&expected_label),
        "the Toolbar `app-name` label does not carry `{name}`"
    );

    assert!(
        read(&root.join("src/components/Notices.tsx")).contains(&format!("{name} edits PDFs")),
        "the Notices privacy sentence does not name `{name}` as the subject of its own sentence"
    );

    let readme = read(&root.join("README.md"));
    assert!(
        readme.contains(&format!("# {name}")),
        "the README heading does not name `{name}`"
    );
    // N8: the install directory is derived from `productName`, so a README that still names
    // the old one tells the user to look in a folder the installer never creates.
    assert!(
        readme.contains(&format!("\\Programs\\{name}\\")),
        "the README's pdfium.dll troubleshooting path does not match the install directory \
         NSIS derives from `productName`"
    );
}

#[test]
fn the_bundle_identifier_is_reverse_dns_and_tracks_the_name() {
    let root = repo_root();
    let config: Value = serde_json::from_str(&read(&root.join("src-tauri/tauri.conf.json")))
        .expect("tauri.conf.json parses");
    let identifier = config
        .pointer("/identifier")
        .and_then(Value::as_str)
        .expect("an identifier")
        .to_string();

    // N3: only alphanumerics, hyphens and periods. Checked rather than trusted, because an
    // identifier that Tauri rejects fails at bundle time, long after `cargo test` is green.
    assert!(
        identifier
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-'),
        "`{identifier}` contains a character Tauri's identifier rule forbids"
    );
    assert!(
        identifier.contains('.') && !identifier.starts_with('.') && !identifier.ends_with('.'),
        "`{identifier}` is not reverse-DNS: it needs a separator and must not be one"
    );

    // D-R3: derived from the ruling name so the two cannot drift. Lower case because that is
    // reverse-DNS convention, which N3 permits but does not require. The `.app` segment is
    // D-R3's own shape, so it is spelled out here rather than inferred.
    let expected = format!("com.{}.app", product_name().to_lowercase());
    assert_eq!(
        identifier, expected,
        "the identifier no longer tracks `productName`: the rename would ship a name and an \
         upgrade identity that disagree"
    );
}
