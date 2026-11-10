//! Every `#[tauri::command]` is registered with `generate_handler!`.
//!
//! A command that is written, tested and never registered is invisible: `invoke` fails at run time
//! with "command not found", and nothing in the suite notices because nothing calls the command
//! table. `sign_save` sat in that state -- the signer was fully built and no UI could reach it --
//! and the reason it was not caught is that the only test of the signer called it directly rather
//! than through the surface the product actually uses.
//!
//! This compares the two lists as sets. A source scan rather than a compile-time check because
//! Tauri's registration is a macro over a list of paths: a missing path is not a type error.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn src_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

/// Command function names declared with `#[tauri::command]`, from the `fn` on a following line.
fn declared_commands() -> BTreeSet<String> {
    let text = read(&src_root().join("commands/mod.rs"));
    let mut names = BTreeSet::new();
    let mut pending = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("//") {
            continue;
        }
        if trimmed == "#[tauri::command]" {
            pending = true;
            continue;
        }
        if pending {
            // Other attributes may sit between `#[tauri::command]` and the `fn`:
            // `edit_page_text` has `#[allow(clippy::too_many_arguments)]` there. A scan that
            // required the function on the very next line silently missed it, and the guard then
            // accused the registration list of drift instead of itself of a parse bug.
            if trimmed.starts_with("#[") {
                continue;
            }
            if let Some(rest) = trimmed
                .strip_prefix("pub async fn ")
                .or_else(|| trimmed.strip_prefix("pub fn "))
            {
                let name = rest.split(['(', '<']).next().unwrap_or("").trim();
                if !name.is_empty() {
                    names.insert(name.to_string());
                }
            }
            pending = false;
        }
    }
    names
}

/// Names in the `generate_handler!` list in `lib.rs`.
fn registered_commands() -> BTreeSet<String> {
    let text = read(&src_root().join("lib.rs"));
    let mut names = BTreeSet::new();
    let mut inside = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("//") {
            continue;
        }
        // The macro is `tauri::generate_handler![`, not `generate_handler![`; matching the shorter
        // name found nothing and the guard reported an empty table rather than a parse failure.
        if trimmed.contains("generate_handler![") {
            inside = true;
            continue;
        }
        if inside {
            if trimmed.starts_with("])") {
                break;
            }
            if let Some(name) = trimmed.strip_prefix("commands::") {
                let name = name.trim_end_matches(',').trim();
                if !name.is_empty() {
                    names.insert(name.to_string());
                }
            }
        }
    }
    names
}

/// The guard fired on an empty list once is worth nothing: it must see a populated table before it
/// is allowed to claim anything. A parse that silently matched nothing would otherwise pass.
#[test]
fn the_command_tables_are_not_empty() {
    let declared = declared_commands();
    let registered = registered_commands();
    assert!(
        declared.len() > 20,
        "the `#[tauri::command]` scan found only {} commands; it is probably not parsing",
        declared.len()
    );
    assert!(
        registered.len() > 20,
        "the registration scan found only {} commands; it is probably not parsing",
        registered.len()
    );
    assert!(
        registered.contains("signature_badge"),
        "the scan did not see the command this unit added, so it cannot vouch for the rest"
    );
}

#[test]
fn every_declared_command_is_registered() {
    let declared = declared_commands();
    let registered = registered_commands();
    let missing: Vec<&String> = declared.difference(&registered).collect();
    assert!(
        missing.is_empty(),
        "these commands are declared but never registered with `generate_handler!`, so no UI can \
         invoke them: {missing:?}"
    );
}

/// The reverse direction, and its honest limits.
///
/// A phantom *name* cannot survive the compiler -- `commands::nosuch` is an unresolved path -- so
/// this direction is not a guard against someone adding a name that does not exist. Its value is
/// scanning drift: if a command is declared in a form `declared_commands` cannot see, this fires and
/// says so. That is not hypothetical. While this file was being written the scan missed
/// `edit_page_text` (an `#[allow]` attribute sat between the attribute and the `fn`) and, earlier,
/// every command at all (the macro is `tauri::generate_handler![`, not `generate_handler![`). Both
/// were caught here, as "registered but not declared", rather than by the first guard.
///
/// Known limit: the declaration scan reads `commands/mod.rs` only. A command moved to a new file
/// under `commands/` would be reported as undeclared. That is the failure this test wants loud.
#[test]
fn every_registered_command_is_declared() {
    let declared = declared_commands();
    let registered = registered_commands();
    let unknown: Vec<&String> = registered.difference(&declared).collect();
    assert!(
        unknown.is_empty(),
        "these names are registered but no `#[tauri::command]` in commands/mod.rs declares them, \
         which means the list has drifted from the code: {unknown:?}"
    );
}

/// The text from `marker` up to the first `end`, or a panic naming the stale marker.
///
/// Panics rather than returning an empty slice: a guard that silently checked nothing would pass on
/// any code at all, which is the vacuous green AGENTS.md forbids.
fn bounded_body(text: &str, marker: &str, end: &str) -> String {
    let start = text
        .find(marker)
        .unwrap_or_else(|| panic!("no `{marker}` in commands/mod.rs; re-anchor this guard"));
    let rest = &text[start..];
    let stop = rest
        .find(end)
        .unwrap_or_else(|| panic!("no `{end}` after `{marker}`; this guard's bound is stale"));
    rest[..stop].to_string()
}

/// Strip `//` comments so a guard cannot be satisfied by prose that merely names the identifier.
fn code_only(body: &str) -> String {
    body.lines()
        .map(|line| match line.find("//") {
            Some(at) => &line[..at],
            None => line,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// D-SG7's four states are the whole vocabulary of the badge, and `is_green` is true for exactly
/// one of them. Checked here rather than in the badge's own tests because the failure this guards
/// against is a *fifth* state being added to the command's serialised surface.
#[test]
fn the_badge_command_reports_only_the_four_documented_states() {
    let text = read(&src_root().join("commands/mod.rs"));
    // The mapping moved out of the command into `badge_for` so a test can call it; the badge must
    // still be produced in exactly one place, so both halves are checked here rather than neither.
    let command = bounded_body(&text, "pub async fn signature_badge", "\n}\n");
    let mapping = bounded_body(&text, "pub fn badge_for(", "\n}\n");

    for (name, body) in [("signature_badge", &command), ("badge_for", &mapping)] {
        for forbidden in ["Badge::Valid", "\"valid\""] {
            assert!(
                !body.contains(forbidden),
                "`{name}` must not construct a badge itself; the deciding functions are the only \
                 place that decides, and a second copy is how a green badge outlives the check \
                 behind it"
            );
        }
    }

    assert!(
        command.contains("badge_for("),
        "the command must report through `badge_for`, which owns the wording for the unsigned case"
    );
    // Comments are stripped before this check. Without that, the doc comment above the branch --
    // which names `badge::unsigned()` while explaining why it is used -- satisfies the guard all on
    // its own, and reverting the code to the fabricated-evidence shape leaves it green. Measured:
    // that mutant survived until the prose was excluded.
    assert!(
        code_only(&mapping).contains("badge::unsigned()"),
        "the unsigned-document branch must come from `badge::unsigned()`, which derives the badge \
         from `decide_document(&[])`. Rebuilding a fabricated evidence struct here is what made an \
         unsigned file render `unknown`."
    );
}
