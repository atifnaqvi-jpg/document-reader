//! Every registered Tauri command must have a caller in the frontend.
//!
//! Twice in two units the same defect shipped: a command, complete and tested, registered in
//! `lib.rs` and invoked from nowhere. `protect_document` (P3.2 SCOPE-A) could write a protected
//! copy and no user could ask it to. `list_trust_anchors` and its three siblings (D-SG4) could
//! read the trust store and nothing displayed the answer. Both looked finished: the Rust suite
//! covered them, `cargo clippy` was quiet, and a command that nothing calls produces no warning,
//! because to the compiler a `tauri::command` is just a `pub fn` that happens to have an attribute.
//!
//! The cost is not that the code was dead. It is that the *tests were evidence about nothing the
//! product could do*. A green suite over an unreachable path reads exactly like a green suite over
//! a working feature, and the difference only surfaces when a user looks for the button and it is
//! not there. That is the shape this file exists to make impossible to ship again.
//!
//! **Why this is a source scan and not a runtime check.** An `invoke("name")` is a string, so
//! nothing in either language links it to the `#[tauri::command]` it names. There is no type to
//! break and no compiler to ask. The scan is therefore over the two literal inventories: the
//! registrations in `lib.rs`'s `generate_handler!`, and the names passed to `invoke` in
//! `src/api/invoke.ts`. Both are the single place each side declares its half of the contract,
//! which is what makes the scan meaningful rather than a grep for a coincidence.
//!
//! **What it deliberately does not assert.** That the *arguments* agree. Serde field names are
//! checked by the TypeScript types and by the commands' own tests; a scan for them here would be a
//! second, weaker copy of those checks. This guard answers one question -- does anything ask for
//! this command at all -- because that is the question that went unanswered twice.

use std::collections::BTreeSet;
use std::path::PathBuf;

fn repo_root() -> PathBuf {
    // `CARGO_MANIFEST_DIR` is `src-tauri`; the frontend sits beside it.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("src-tauri has a parent")
        .to_path_buf()
}

/// Command names registered in `lib.rs`'s `generate_handler![...]`.
///
/// Scoped to that macro rather than scanning the whole file: `lib.rs` also mentions command names in
/// comments, and a name appearing in a comment is not a registration. A guard that counted comments
/// would pass while a command was missing from the handler list -- the opposite failure, and the
/// one that makes a command silently uncallable even when the frontend asks for it.
fn registered_commands(source: &str) -> BTreeSet<String> {
    let start = source
        .find("generate_handler!")
        .expect("lib.rs declares a generate_handler! list");
    let body = &source[start..];
    let open = body.find('[').expect("generate_handler! takes a list");
    let close = body.find(']').expect("generate_handler! list is closed");
    let list = &body[open + 1..close];

    list.split(',')
        .filter_map(|entry| {
            let entry = entry.trim();
            let name = entry.strip_prefix("commands::")?;
            let name = name.trim();
            (!name.is_empty()).then(|| name.to_string())
        })
        .collect()
}

/// Command names the frontend passes to `invoke`.
///
/// Both call shapes are collected: `invoke<T>("name", ..)` and the untyped `invoke("name", ..)`.
/// The regex is deliberately not used -- adding one for two literal shapes would mean accepting a
/// dependency to parse a string, and a hand-rolled scan keeps this test readable to whoever next
/// wonders why a command is listed as uncalled.
fn invoked_commands(source: &str) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    let mut rest = source;

    while let Some(at) = rest.find("invoke") {
        rest = &rest[at + "invoke".len()..];

        // Optional generic argument: `<SignatureBadge>` or `<TrustAnchorView[]>`.
        if rest.trim_start().starts_with('<') {
            let skip = rest.len() - rest.trim_start().len();
            let mut depth = 0usize;
            let mut end = None;
            for (index, character) in rest[skip..].char_indices() {
                match character {
                    '<' => depth += 1,
                    '>' => {
                        depth -= 1;
                        if depth == 0 {
                            end = Some(skip + index + 1);
                            break;
                        }
                    }
                    _ => {}
                }
            }
            let Some(end) = end else { continue };
            rest = &rest[end..];
        }

        let trimmed = rest.trim_start();
        let Some(after_paren) = trimmed.strip_prefix('(') else {
            continue;
        };
        let after_paren = after_paren.trim_start();
        let Some(quote) = after_paren.strip_prefix('"') else {
            continue;
        };
        let Some(end_quote) = quote.find('"') else {
            continue;
        };
        let name = &quote[..end_quote];
        if !name.is_empty() && name.chars().all(|c| c.is_ascii_lowercase() || c == '_') {
            found.insert(name.to_string());
        }
    }

    found
}

#[test]
fn every_registered_command_has_a_frontend_caller() {
    let registered = registered_commands(
        &std::fs::read_to_string(repo_root().join("src-tauri/src/lib.rs")).unwrap(),
    );
    let invoked =
        invoked_commands(&std::fs::read_to_string(repo_root().join("src/api/invoke.ts")).unwrap());

    assert!(
        registered.len() > 40,
        "the registration scan found only {} commands; it is no longer reading generate_handler!, \
         so this guard would pass over an empty set",
        registered.len()
    );
    assert!(
        invoked.len() > 40,
        "the invoke scan found only {} call sites; it is no longer reading invoke.ts",
        invoked.len()
    );

    let uncalled: Vec<&String> = registered.difference(&invoked).collect();
    assert!(
        uncalled.is_empty(),
        "these commands are registered but nothing in src/ calls them: {uncalled:?}\n\
         \n\
         A command with no caller is not a feature: no user can reach it, and every test of it is \
         evidence about code the product cannot run. Either give it a UI (as P3.2 did for the trust \
         store and for protect_document) or delete it. Do not add it to an allowlist -- if it is \
         genuinely not ready to be called, it does not belong in generate_handler! yet."
    );
}

#[test]
fn the_scan_reads_the_two_literal_inventories_it_claims_to() {
    // Positive controls. Without these, a scan that silently stopped matching would report an empty
    // registry and an empty call list, and the guard above would be green having checked nothing --
    // the failure mode `AGENTS.md` calls a green pass wearing a label.
    let registered = registered_commands(
        &std::fs::read_to_string(repo_root().join("src-tauri/src/lib.rs")).unwrap(),
    );
    assert!(
        registered.contains("protect_document"),
        "protect_document must be registered; P3.2 SCOPE-A added it and this test pins that the \
         scan still sees it"
    );
    assert!(registered.contains("list_trust_anchors"));

    let invoked =
        invoked_commands(&std::fs::read_to_string(repo_root().join("src/api/invoke.ts")).unwrap());
    assert!(
        invoked.contains("protect_document"),
        "protectDocument must reach the scanner"
    );

    // And the scanner must handle the typed call shape, which is most of invoke.ts.
    assert!(
        invoked_commands(r#"invoke<SignatureBadge>("get_signature_badge", { id })"#)
            .contains("get_signature_badge"),
        "the scanner must skip a generic argument to find the command name"
    );
    // And must not be fooled by a name that is not a command: the lowercase/underscore filter is
    // what keeps `invoke` used as an ordinary word from being counted.
    assert!(invoked_commands(r#"// invoke("Not_A_Command")"#).is_empty());
}
