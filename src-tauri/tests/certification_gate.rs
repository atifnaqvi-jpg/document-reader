//! Every mutating command a certification governs actually calls the gate (I-SG5).
//!
//! `tests/sign_edit_policy.rs` proves the *policy*: that P=1/2/3 permit and refuse the right edits on
//! real certified files. It cannot prove the commands honour it. A command that never calls
//! `refuse_if_certification_forbids` is invisible to that test, and this project has already shipped
//! exactly that class of bug once — `signature_badge` held `anchored: false` as a literal while
//! `badge.rs` was fully tested, so `valid` was unreachable in the product on any file, and no test of
//! the deciding functions could notice (see `badge_evidence_source.rs`, which exists for that
//! reason).
//!
//! So this guard reads the source. It is a weaker kind of test than a behavioural one, and it is used
//! deliberately: the alternative is constructing a `tauri::State<Db>` plus an `AppHandle` per command,
//! which would test Tauri's plumbing rather than this decision.
//!
//! Two things it checks, because either alone is defeatable:
//!
//! 1. **Every listed command's body calls the gate.** Deleting one call is the regression.
//! 2. **The gate is called with the right `EditKind`.** A command that checks `Signature` before
//!    annotating compiles, calls the helper, and permits everything — the guard would otherwise pass
//!    on a wiring mistake.
//!
//! The command list is written out rather than discovered by scanning for `#[tauri::command]`.
//! Discovery would silently shrink: a renamed command drops out of the scan and the guard goes green
//! over a file it no longer covers. The list is asserted non-empty and every entry must be found, so
//! a rename fails loudly instead.

use std::path::Path;

fn commands_source() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/commands/mod.rs");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

/// The body of `fn <name>`, from its signature to the closing brace at the same depth.
///
/// Panics when the function is absent, on purpose: a guard that quietly finds nothing is a green pass
/// over nothing, which is the failure mode this whole file is about.
fn body_of(source: &str, name: &str) -> String {
    let signature = format!("fn {name}(");
    let start = source.find(&signature).unwrap_or_else(|| {
        panic!("{name} must exist; if it was renamed, update certification_gate.rs")
    });
    let mut depth = 0isize;
    let mut began = false;
    let mut body = String::new();
    for character in source[start..].chars() {
        match character {
            '{' => {
                depth += 1;
                began = true;
            }
            '}' => {
                depth -= 1;
                if began && depth == 0 {
                    body.push(character);
                    return body;
                }
            }
            _ => {}
        }
        body.push(character);
    }
    panic!("{name} has unbalanced braces; the guard cannot delimit its body")
}

/// Strip `//` comments, so a guard cannot be satisfied by prose.
///
/// This is not hypothetical: the I-SG4 guard was satisfied by a doc comment that happened to contain
/// the string it looked for. Every source guard here reads code-only text.
fn code_only(body: &str) -> String {
    body.lines()
        .map(|line| match line.find("//") {
            Some(at) => &line[..at],
            None => line,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Every call to the gate in `body`, returned as the text from the call to its terminating `;`.
///
/// Needed because "calls the gate" is not the property that matters — "calls it and propagates the
/// refusal" is. `let _ = refuse_if_certification_forbids(...)` compiles, names the helper, and is a
/// silent no-op to the user, which is precisely the shape this project bans.
fn gate_calls(body: &str) -> Vec<String> {
    const CALL: &str = "refuse_if_certification_forbids(";
    let mut calls = Vec::new();
    let mut from = 0usize;
    while let Some(found) = body[from..].find(CALL) {
        let start = from + found;
        let end = body[start..]
            .find(';')
            .map(|at| start + at + 1)
            .unwrap_or(body.len());
        calls.push(body[start..end].to_string());
        from = end;
    }
    calls
}

/// Every command that must refuse a forbidden edit, with the kind it must enforce.
///
/// Derived from ISO 32000-1 §12.8.2.2 rather than from the code: annotation commands enforce
/// `Annotation`, page and content surgery enforce `Content`, and baking annotations into the file
/// enforces `Annotation` because that is what the resulting file change is.
const GOVERNED: &[(&str, &str)] = &[
    // Annotation changes. `P=1` and `P=2` refuse these; `P=3` permits them.
    ("add_annotation", "Annotation"),
    ("update_annotation", "Annotation"),
    ("set_annotation_status", "Annotation"),
    // Deletion is the same change to the signed document's annots array as creation, and the gate for
    // it lives in `remove_annotation_and_gc`, which owns the write.
    ("remove_annotation_and_gc", "Annotation"),
    // Baking the annotation layer into the original file is the same edit, applied destructively.
    ("overwrite_edits", "Annotation"),
    // Content surgery. Refused at every level, `P=3` included.
    ("rotate_page", "Content"),
    ("delete_pages", "Content"),
    ("move_page", "Content"),
    ("add_watermark", "Content"),
    ("edit_page_text", "Content"),
    ("revert_page_text", "Content"),
];

#[test]
fn every_governed_command_calls_the_certification_gate() {
    let source = commands_source();

    for (command, kind) in GOVERNED {
        let body = code_only(&body_of(&source, command));

        let calls = gate_calls(&body);
        assert!(
            !calls.is_empty(),
            "{command} must call refuse_if_certification_forbids before it writes; without it a \
             certified document can be edited past its own certification"
        );
        assert!(
            body.contains(&format!("EditKind::{kind}")),
            "{command} must enforce EditKind::{kind}; calling the gate with a different kind \
             compiles and permits everything"
        );

        // Every call must propagate its refusal. A gate whose result is discarded is worse than no
        // gate: the source still names it, so a review sees it, while the user sees the edit proceed.
        for call in &calls {
            assert!(
                call.trim_end().ends_with("?;"),
                "{command} must propagate the gate's refusal with `?`; this call discards it and \
                 the edit proceeds: {call}"
            );
        }
    }
}

/// The gate is not called from anywhere the list does not name.
///
/// The forward direction ("every listed command is gated") cannot notice the list *shrinking*: delete
/// an entry and its command stops being checked while the test still passes. `!GOVERNED.is_empty()`
/// would not help either — it is a compile-time constant that can never be false, which clippy
/// correctly flagged as a guard that cannot fail. The real inverse is to scan the source for every
/// gate call, find the function each one lives in, and require that function to be in the list. Then
/// dropping an entry, or adding a gated command the list omits, both fail.
#[test]
fn no_command_calls_the_gate_without_being_listed() {
    let source = commands_source();

    // Every `fn` in the file, as (name, byte offset of its name), so a call site can be attributed to
    // the innermost enclosing function. A hand-rolled scan rather than a parser: the functions in this
    // file are top-level, and the guard only needs to know which function precedes a call.
    //
    // Anchoring on the `fn` keyword rather than on the qualifiers is what makes this correct for
    // `fn`, `pub fn`, `async fn` and `pub async fn` alike. An earlier version matched `"\npub fn "`
    // and `"\nasync fn "` by prefix and read the name from the start of the match, which yielded
    // `"pub"` as a function name -- a guard that misattributes call sites is exactly the class of bug
    // this file exists to catch.
    let mut functions: Vec<(&str, usize)> = Vec::new();
    for (offset, _) in source.match_indices("fn ") {
        // `fn ` preceded by an identifier character is part of a name (`my_fn(`), not a definition.
        let mid_identifier = offset > 0
            && source[..offset]
                .chars()
                .next_back()
                .is_some_and(|c| c == '_' || c.is_alphanumeric());
        if mid_identifier {
            continue;
        }
        let rest = &source[offset + 3..];
        let length = rest
            .find(|c: char| !c.is_alphanumeric() && c != '_')
            .unwrap_or(rest.len());
        let name = &rest[..length];
        if !name.is_empty() {
            functions.push((name, offset));
        }
    }
    functions.sort_by_key(|(_, offset)| *offset);

    let mut callers: Vec<&str> = Vec::new();
    for (offset, _) in source.match_indices("refuse_if_certification_forbids(") {
        let enclosing = functions
            .iter()
            .filter(|(_, at)| *at < offset)
            .next_back()
            .map(|(name, _)| *name)
            .unwrap_or_else(|| panic!("a gate call at byte {offset} is outside any function"));
        // Skip the definition itself.
        if enclosing == "refuse_if_certification_forbids" {
            continue;
        }
        callers.push(enclosing);
    }

    callers.sort_unstable();
    callers.dedup();

    assert!(
        !callers.is_empty(),
        "no command calls the gate at all; the enforcement is gone"
    );

    let listed: Vec<&str> = GOVERNED.iter().map(|(name, _)| *name).collect();
    for caller in &callers {
        assert!(
            listed.contains(caller),
            "`{caller}` calls the certification gate but is not in GOVERNED; add it with the \
             EditKind it enforces, or the forward guard stops covering it"
        );
    }
}

/// The gate is called *before* the mutation, not after it.
///
/// Ordering is the property that makes a refusal safe. A command that writes and then checks leaves
/// the user's file changed by an edit it reports as refused, which is worse than either outcome alone
/// and is invisible to any assertion on the returned error.
///
/// Checked structurally: the gate's call must appear before the first line that mutates the database
/// or the document in each command. The anchors are per command because "the mutation" is a different
/// call in each one, and guessing at it would make the guard assert a coincidence.
#[test]
fn the_gate_runs_before_the_mutation_in_each_command() {
    let source = commands_source();

    // For each command, the marker of its first write. The gate must come earlier in the body.
    let first_write: &[(&str, &str)] = &[
        ("add_annotation", "annotations::add("),
        ("update_annotation", "annotations::update("),
        ("set_annotation_status", "annotations::set_status("),
        ("remove_annotation_and_gc", "annotations::remove("),
        ("overwrite_edits", "write_atomically("),
        ("rotate_page", "editor::pages::rotate"),
        ("delete_pages", "editor::pages::delete_pages("),
        ("move_page", "editor::pages::move_page("),
        ("add_watermark", "editor::export::watermark("),
        ("edit_page_text", "editor::textedit::edit("),
        ("revert_page_text", "editor::textedit::revert("),
    ];

    for (command, write) in first_write {
        let body = code_only(&body_of(&source, command));
        let gate = body
            .find("refuse_if_certification_forbids(")
            .unwrap_or_else(|| {
                panic!("{command} does not call the gate; the companion test names the omission")
            });
        let mutation = body.find(write).unwrap_or_else(|| {
            panic!(
                "{command} no longer contains `{write}`; if the write path was renamed, re-anchor \
                 this guard rather than deleting the entry"
            )
        });
        assert!(
            gate < mutation,
            "{command} calls the gate after `{write}`; a refusal there leaves the file changed"
        );
    }
}

/// The gate reads the level from the file and returns a real error, rather than a silent no-op.
///
/// Three properties, each of which has been a real bug somewhere in this project's history:
/// the level comes from the bytes (not from a caller argument), the refusal is an `Err` (not a
/// logged warning), and an unreadable file is reported rather than treated as uncertified.
#[test]
fn the_gate_itself_is_shaped_as_an_error_and_reads_the_file() {
    let source = commands_source();
    let body = code_only(&body_of(&source, "refuse_if_certification_forbids"));

    assert!(
        body.contains("std::fs::read("),
        "the gate must read the document's bytes; a level from anywhere else is not the file's claim"
    );
    assert!(
        body.contains("enforceable_level("),
        "the gate must read the level through the shared resolver, not invent its own"
    );
    assert!(
        body.contains("editpolicy::check("),
        "the gate must return the policy's error rather than approximating the decision"
    );
    assert!(
        !body.contains("let _ ="),
        "the gate must not discard its result; a logged refusal is a silent no-op to the user"
    );
    assert!(
        body.contains("?;") || body.contains(")?"),
        "the gate must propagate the refusal with `?` so the command aborts before writing"
    );
}
