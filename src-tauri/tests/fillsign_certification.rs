//! §6 acceptance row: a signature placement is refused on a `P=1`/`P=2` document.
//!
//! ## Why the test is shaped this way
//!
//! The row is named `the_certification_gate_refuses_a_p1_and_p2_signature_placement`. A behavioural
//! test would build a certified PDF, call the placement command, and assert the refusal — but
//! `add_annotation` is a Tauri `State` unwrapper with no `_with` core, so there is no driveable
//! entry point from an integration test. Pretending otherwise would mean testing a reimplementation
//! of the command, which proves the reimplementation works and says nothing about the shipped one.
//!
//! So the two halves are proved where each is actually decidable:
//!
//! 1. **The command refuses, before writing, with the right kind.** Read from the shipped source by
//!    the same technique `tests/certification_gate.rs` uses, because that is the only view of the
//!    shipped path available here.
//! 2. **The kind refuses at `P=1` and `P=2` and permits at `P=3`.** Called directly: `check` is a
//!    total function of a level and a kind, so this half needs no PDF at all.
//!
//! Together they give the row's claim: a placement goes through `add_annotation` (design §10.1,
//! D-N3), `add_annotation` refuses before the row exists, and the kind it refuses with is one the
//! policy refuses at exactly the levels the ISO table names. Neither half alone would: a command
//! that refused with the wrong `EditKind` would pass the first and fail the second.

use document_reader_lib::editor::signature::editpolicy::{
    check, permits, CertificationLevel, EditKind, Verdict,
};

fn commands_source() -> String {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/src/commands/mod.rs");
    std::fs::read_to_string(path).expect("commands/mod.rs must be readable")
}

/// The body of `fn name(`, delimited by brace matching.
fn body_of(source: &str, name: &str) -> String {
    let signature = format!("fn {name}(");
    let start = source
        .find(&signature)
        .unwrap_or_else(|| panic!("{name} not found in commands/mod.rs"));
    let mut depth = 0usize;
    let mut body = String::new();
    for character in source[start..].chars() {
        match character {
            '{' => {
                depth += 1;
                if depth == 1 {
                    continue;
                }
            }
            '}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return body;
                }
            }
            _ => {}
        }
        body.push(character);
    }
    panic!("{name} has unbalanced braces")
}

/// Strip `//` comments so the guard cannot be satisfied by prose. A doc comment containing the
/// string it looks for has satisfied a guard in this project before.
fn code_only(body: &str) -> String {
    body.lines()
        .map(|line| match line.find("//") {
            Some(at) => &line[..at],
            None => line,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The command refuses a placement on a certified document, with `EditKind::Annotation`.
///
/// Three properties, not one: the gate is *called*, it is called with the *annotation* kind (not
/// `Signature`, which every level permits, and not `Content`, which would refuse a legitimate
/// `P=3` placement), and the refusal is *before* the row is written so a refusal leaves nothing to
/// clean up.
#[test]
fn the_certification_gate_refuses_a_p1_and_p2_signature_placement() {
    let source = commands_source();
    let body = code_only(&body_of(&source, "add_annotation"));

    assert!(
        body.contains("refuse_if_certification_forbids("),
        "add_annotation must call the certification gate: it is the command Fill & Sign places \
         through (design D-N3), so a placement on a P=1/P=2 document has nowhere else to be refused"
    );
    assert!(
        body.contains("EditKind::Annotation"),
        "the gate must be called with EditKind::Annotation. `EditKind::Signature` is permitted at \
         every level, so using it would let a visual overlay onto a P=1 document -- the exact false \
         accusation of the certifier that I-SG5 exists to prevent"
    );

    // Ordering: the gate's call must precede the first write. `annotations::add` is the write.
    let gate = body
        .find("refuse_if_certification_forbids(")
        .expect("gate call present");
    let write = body
        .find("annotations::add(")
        .expect("add_annotation must write through annotations::add");
    assert!(
        gate < write,
        "the gate must run before the row is written, so a refusal leaves no row in the database \
         for the user to clean up"
    );
}

/// The kind the command refuses with is refused at exactly `P=1` and `P=2`.
#[test]
fn the_annotation_kind_is_refused_at_p1_and_p2_but_permitted_at_p3() {
    // The row's "P=1 and P=2" half. Called directly because `permits` is total: no PDF, no
    // certificate, no platform, and every branch is decided by the table in ISO 32000-1 §12.8.2.2.
    assert_eq!(
        permits(Some(CertificationLevel::NoChanges), EditKind::Annotation),
        Verdict::Refused,
        "P=1 permits nothing but further signatures, so a placement must be refused"
    );
    assert_eq!(
        permits(
            Some(CertificationLevel::FormFillAndSignatures),
            EditKind::Annotation
        ),
        Verdict::Refused,
        "P=2 permits form filling and signatures, not annotation, so a placement must be refused"
    );
    assert_eq!(
        permits(
            Some(CertificationLevel::AnnotationsToo),
            EditKind::Annotation
        ),
        Verdict::Permitted,
        "P=3 permits annotation, so refusing a placement there would break a legitimate use"
    );
    // And the uncertified case, so the refusal is not a blanket ban that happens to pass above.
    assert_eq!(permits(None, EditKind::Annotation), Verdict::Permitted);
}

/// The refusal is a real error with a reason, not a silent no-op.
#[test]
fn a_refused_placement_returns_an_error_naming_annotation() {
    let refusal = check(Some(CertificationLevel::NoChanges), EditKind::Annotation);
    let error = refusal.expect_err("P=1 must refuse an annotation placement");
    let message = format!("{error}");
    // I-F7's shape: a refusal names why. A button that does nothing is indistinguishable from a
    // broken build, which is why the policy returns an error rather than `()`.
    assert!(
        message.to_lowercase().contains("certif"),
        "the refusal must say the document is certified, got: {message}"
    );
    assert!(
        message.contains("annotation"),
        "the refusal must name the refused edit as an annotation, got: {message}"
    );
}
