//! The CI gate is release infrastructure too, and it fails in the quietest way possible: by
//! simply not running.
//!
//! That is not hypothetical here. `on.push.branches` was `[main, master]`, but all of P2.7.x
//! and the CID phase were developed on the long-lived `p2-windows-test` branch, which that
//! filter never matched. The only workflow that ran there was the Windows release build —
//! `cargo test` plus the PDFium assertion, but no `cargo fmt`, no `clippy`, no `vitest` — so
//! three weeks of unformatted code accumulated while every reported "gates green" referred to
//! someone's laptop. A gate that does not run is not green, and nothing about a green check on
//! a different branch tells you that.
//!
//! The second invariant is that the toolchain is pinned. rustfmt is allowed to change its
//! output between releases; with `@stable` the format gate's verdict depends on the date
//! rather than the code, so it can go red on a day nobody edited anything — or reformat a
//! release branch mid-flight. The pin has to agree between CI and `rust-toolchain.toml`, or a
//! local `cargo fmt --check` proves nothing about what CI will say.

use std::path::PathBuf;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("src-tauri must have a parent directory")
        .to_path_buf()
}

fn ci_workflow() -> String {
    let path = repo_root().join(".github/workflows/ci.yml");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

fn unquote(s: &str) -> String {
    s.trim().trim_matches(['"', '\'']).to_string()
}

/// The keys directly under each `on.<event>` block at indent 4, and the event names
/// themselves. An event with no child keys (`push:`) is a valid, unrestricted trigger and
/// yields just the event name — which is the shape the gate must have.
fn push_trigger(text: &str) -> (Vec<String>, Vec<String>) {
    let mut events = Vec::new();
    let mut keys = Vec::new();
    let mut top = String::new();
    let mut sub = String::new();
    for raw in text.lines() {
        let line = raw.split('#').next().unwrap_or("");
        if line.trim().is_empty() {
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        let item = line.trim();
        match indent {
            0 => {
                top = item.trim_end_matches(':').to_string();
                sub.clear();
            }
            2 => {
                sub = item.trim_end_matches(':').to_string();
                if top == "on" {
                    events.push(sub.clone());
                }
            }
            4 if top == "on" && sub == "push" => {
                // The key, before any inline value: `branches: [main, master]` must yield
                // `branches`, or the restriction check silently passes.
                keys.push(
                    item.split(':')
                        .next()
                        .unwrap_or("")
                        .trim_end_matches(':')
                        .to_string(),
                );
            }
            _ => {}
        }
    }
    (events, keys)
}

/// The `with.toolchain:` value of each `dtolnay/rust-toolchain` step.
fn toolchain_pins(text: &str) -> Vec<String> {
    let mut pins = Vec::new();
    let mut lines = text.lines().peekable();
    while let Some(raw) = lines.next() {
        let line = raw.split('#').next().unwrap_or("");
        let trimmed = line.trim();
        let Some(idx) = trimmed.find("dtolnay/rust-toolchain@") else {
            continue;
        };
        // The action ref itself, e.g. `@master` or `@stable`.
        pins.push(unquote(&trimmed[idx + "dtolnay/rust-toolchain@".len()..]));
        // Then `with.toolchain:` among the following indented lines.
        while let Some(next) = lines.peek() {
            let next = next.split('#').next().unwrap_or("");
            let next_trimmed = next.trim();
            if next_trimmed.is_empty() {
                lines.next();
                continue;
            }
            if next_trimmed.starts_with("- ") {
                break;
            }
            lines.next();
            if let Some(value) = next_trimmed.strip_prefix("toolchain:") {
                pins.push(unquote(value));
            }
        }
    }
    pins
}

fn rust_toolchain_file() -> String {
    let path = repo_root().join("rust-toolchain.toml");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

#[test]
fn the_gate_runs_on_every_branch() {
    let text = ci_workflow();
    let (events, keys) = push_trigger(&text);
    assert!(
        events.contains(&"push".to_string()),
        "the workflow has no `on.push` trigger at all: {events:?}"
    );
    assert!(
        !keys.contains(&"branches".to_string()) && !keys.contains(&"branches-ignore".to_string()),
        "`on.push` restricts branches ({keys:?}). Every branch that can ship must be gated; \
         a long-lived feature branch is exactly the case this restriction hid"
    );
}

#[test]
fn the_toolchain_is_pinned_and_the_pin_agrees() {
    let ci = ci_workflow();
    let pins = toolchain_pins(&ci);
    assert!(
        !pins.is_empty(),
        "no dtolnay/rust-toolchain step found; the pin check would pass vacuously"
    );
    assert!(
        !pins.iter().any(|p| p == "stable"),
        "CI uses a floating `stable` toolchain ({pins:?}); rustfmt output may then change \
         without any code change"
    );

    let declared = rust_toolchain_file();
    let channel = declared
        .lines()
        .map(|l| l.split('#').next().unwrap_or(""))
        .find_map(|l| l.trim().strip_prefix("channel"))
        .map(|rest| unquote(rest.trim_start_matches([' ', '=', ':'])))
        .expect("rust-toolchain.toml has no `channel`");

    assert!(
        pins.contains(&channel),
        "CI pins {pins:?} but rust-toolchain.toml pins {channel:?}; a local \
         `cargo fmt --check` would then prove nothing about CI"
    );
}
