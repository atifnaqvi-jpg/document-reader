//! `signature_badge` takes its evidence from the trust store, not from literals.
//!
//! `tests/trust_store.rs` proves the *decision* is honest by calling `store::assess` on real signed
//! files. It cannot see the command, and the command is where this project has already shipped a
//! false badge once: `anchored: false` and `revocation::decide(None, None, 0)` were both literals
//! there while `badge.rs` and `revocation.rs` were fully tested. The result was that `valid` was
//! unreachable in the product on any file, which no test of the deciding functions could notice.
//!
//! Restoring those literals compiles, and every other test still passes, because they all call
//! `assess` rather than the command. So the guard has to read the source. That is a weaker kind of
//! test than a behavioural one and it is used here deliberately: the alternative is to build a
//! `tauri::State<Db>` in a test, which would test Tauri rather than this decision.
//!
//! The scan is narrow on purpose -- it names the two literals that were actually wrong, so it does
//! not fire on a legitimate `anchored` mention in a comment or on the unsigned-document branch,
//! which correctly hardcodes `anchored: false` because there is no signature to anchor.

use std::path::Path;

fn commands_source() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/commands/mod.rs");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

/// The body of `signature_badge`, from its `fn` to the closing brace at the same depth.
fn signature_badge_body(source: &str) -> String {
    let start = source
        .find("pub async fn signature_badge(")
        .expect("signature_badge must exist; if it was renamed, update this guard");
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
    body
}

/// Strip `//` comments, so a guard cannot be satisfied by prose.
fn code_only(body: &str) -> String {
    body.lines()
        .map(|line| match line.find("//") {
            Some(at) => &line[..at],
            None => line,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The body of `fn badge_for`, which owns the unsigned-document branch.
fn badge_for_body(source: &str) -> String {
    let start = source
        .find("pub fn badge_for(")
        .expect("badge_for must exist; if it was renamed, update this guard");
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
    body
}

#[test]
fn the_command_assesses_through_the_store_rather_than_hardcoding_inputs() {
    let body = code_only(&signature_badge_body(&commands_source()));
    assert!(
        body.contains("store::assess("),
        "`signature_badge` must call `store::assess`, which is the only function that gathers the \
         anchor set and the revocation evidence. Without it the badge is decided from literals."
    );
}

#[test]
fn the_command_hands_the_store_assessment_to_the_mapping_unmodified() {
    let body = code_only(&signature_badge_body(&commands_source()));
    assert!(
        body.contains("badge_for(assessment)"),
        "`signature_badge` must hand the assessment straight to `badge_for`. Rebuilding a \
         `SignatureEvidence` here is what let an unsigned document render `unknown`, and a guard \
         that only read `badge_for` would not notice the command going its own way again."
    );
}

#[test]
fn the_command_does_not_pass_a_constant_anchored_flag_for_a_signed_file() {
    let body = code_only(&badge_for_body(&commands_source()));
    // The unsigned-document branch legitimately hardcodes `anchored: false`: there is no signature,
    // so nothing can anchor. Everything *after* that branch is about a signed file, so the cut is
    // taken at the `};` that closes the `else` block -- not at the `let`, which would leave the
    // legitimate literal inside the slice and make this guard fire on correct code.
    //
    // The marker is REQUIRED, not optional. An earlier draft returned early when it was absent,
    // which meant a renamed or removed early-return turned this guard into a silent pass. A mutant
    // that replaced the whole tail with the historical literals then survived this guard while
    // failing the other two -- a vacuous pass is exactly what AGENTS.md calls worse than no guard.
    //
    // The branch moved from `signature_badge` to `badge_for` when the mapping was extracted to be
    // callable from a test; this guard moved with it rather than being dropped, and it fired loudly
    // when the anchor went stale instead of passing over an empty slice.
    let after_early_return = match body.find("let Some(assessment) = assessment else") {
        Some(at) => &body[at..],
        None => panic!(
            "`badge_for` no longer has the `let Some(assessment) = assessment else` \
             early-return this guard cuts at. Re-anchor the guard rather than letting it pass \
             without checking anything."
        ),
    };
    let close = after_early_return.find("    };\n").unwrap_or_else(|| {
        panic!(
            "could not find the `}};` closing the unsigned-document branch; this guard's cut \
             point is stale and it would otherwise check the wrong region"
        )
    });
    let tail = &after_early_return[close..];
    assert!(
        !tail.contains("anchored:"),
        "the badge for a signed file must take `anchored` from `store::assess`, not a literal. \
         A hardcoded `false` here makes `valid` unreachable in the product even though every \
         deciding function is correct and tested."
    );
}

#[test]
fn the_command_does_not_decide_revocation_with_empty_inputs() {
    let body = code_only(&signature_badge_body(&commands_source()));
    assert!(
        !body.contains("decide(None, None"),
        "revocation must be decided from evidence `store::assess` gathered. Calling \
         `decide(None, None, ...)` here reports `NotChecked` for every file, which cannot support \
         `valid` and silently overrides whatever the store found."
    );
}
