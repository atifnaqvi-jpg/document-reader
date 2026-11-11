//! What a certification level permits (I-SG5, D-SG6).
//!
//! A certified document states, in its own bytes, which later edits the certifier allows. PDF names
//! those levels `P=1`, `P=2`, `P=3`; ISO 32000-1 §12.8.2.2 defines them as:
//!
//! | level | permitted after certification |
//! |---|---|
//! | 1 | nothing but further signatures — any other change is a violation |
//! | 2 | form filling, and further signatures |
//! | 3 | form filling, further signatures, **and annotation** |
//!
//! I-SG5 asks for two things, and this module is both: the level must be **read back as written**
//! (`verify::certification_level`), and an edit must be judged **against the level** (`permits`). The
//! judgement is a total function of a level and an edit kind, which is why it lives here as plain
//! data rather than inside a command: every branch is then provable in a test with no PDF, no
//! certificate and no platform.
//!
//! ## Why the policy exists at all
//!
//! A certification signature that is displayed and then ignored is a badge, not a control. The
//! level's whole purpose is that the *editor* is what breaks it — our annotation tool and our
//! redaction tool are exactly the operations `P=1` and `P=2` forbid. Enforcing only in the viewer
//! would let a user certify at `P=1` and then annotate their own document, after which every reader
//! reports the document as modified. That is a false accusation produced by this program against the
//! user's own file, and the user could not tell it from real tampering.
//!
//! So the refusal happens at the command layer, before anything is written, and it is a real error
//! with a reason ([`crate::model::AppError::CertificationRefusesEdit`]) rather than a silent no-op: a
//! button that does nothing when pressed is indistinguishable from a broken build.

use crate::model::AppError;

/// One thing an edit command can do to a document.
///
/// Deliberately coarse. The axis that matters is the one PDF's levels distinguish, so this enum has
/// exactly the members the normative table names and nothing more: a finer split would invent
/// distinctions the specification does not make, and a test asserting them would be asserting our
/// invention rather than the format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditKind {
    /// Adding, editing or removing an annotation (highlights, notes, stamps, ink).
    Annotation,
    /// Filling in a form field that already exists.
    FormFill,
    /// Applying a redaction. Redaction rewrites the content stream, so it is a content edit by
    /// nature and no level permits it, `P=3` included.
    Redaction,
    /// Changing the document's content: editing existing text, adding or removing pages, rotating,
    /// watermarking. What `P=1` and `P=2` both refuse.
    Content,
    /// Adding a further signature. Permitted at every level: ISO 32000-1 allows a document to gather
    /// approval signatures after certification, which is much of the point of certifying it.
    Signature,
}

impl EditKind {
    /// A short noun phrase for the error message. Not UI prose, which the frontend owns.
    pub fn code(self) -> &'static str {
        match self {
            EditKind::Annotation => "annotation",
            EditKind::FormFill => "form filling",
            EditKind::Redaction => "redaction",
            EditKind::Content => "content edit",
            EditKind::Signature => "a signature",
        }
    }
}

/// The certification level a document declares, or the absence of one.
///
/// `None` is not "unknown": it is the ordinary case of a document nobody has certified, where every
/// edit is permitted. Kept distinct from `Some(level)` because collapsing the two would either refuse
/// every edit on an uncertified file or permit every edit on a certified one, and only one of those
/// mistakes is visible in testing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CertificationLevel {
    /// `P=1`: no changes of any kind after certification, beyond adding signatures.
    NoChanges,
    /// `P=2`: form filling and further signatures.
    FormFillAndSignatures,
    /// `P=3`: form filling, further signatures and annotations.
    AnnotationsToo,
}

impl CertificationLevel {
    /// The `/P` value this level is written as.
    pub fn number(self) -> u8 {
        match self {
            CertificationLevel::NoChanges => 1,
            CertificationLevel::FormFillAndSignatures => 2,
            CertificationLevel::AnnotationsToo => 3,
        }
    }

    /// The level a `/P` value means, or `None` for a value the specification does not define.
    ///
    /// A reader that mapped an undefined value to a permissive default would answer a malformed claim
    /// with silence, so an unrecognised `/P` maps to nothing and [`permits`] then treats the document
    /// as uncertified. That is the same answer a reader gives a document with no `/Perms` at all, and
    /// it is why the read-back in [`super::verify`] reports only `1..=3`.
    pub fn from_number(number: u8) -> Option<Self> {
        match number {
            1 => Some(CertificationLevel::NoChanges),
            2 => Some(CertificationLevel::FormFillAndSignatures),
            3 => Some(CertificationLevel::AnnotationsToo),
            _ => None,
        }
    }
}

/// What a specific edit would do to a document at some level.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// The edit is inside what the certification permits.
    Permitted,
    /// The edit would violate the certification, and is refused.
    Refused,
}

/// Whether `level` permits `kind`.
///
/// The whole of I-SG5's second half. `None` means the document declares no certification, so every
/// edit is permitted — the state almost every opened file is in.
pub fn permits(level: Option<CertificationLevel>, kind: EditKind) -> Verdict {
    let Some(level) = level else {
        return Verdict::Permitted;
    };

    // A further signature is allowed at every level, `P=1` included. Stated once because it is the
    // one row of the table that is the same everywhere, rather than repeated in three arms.
    if kind == EditKind::Signature {
        return Verdict::Permitted;
    }

    let permitted = match level {
        CertificationLevel::NoChanges => false,
        // `P=2`'s "form filling" is filling a field that already exists, not rewriting the page, so
        // redaction stays refused even though it also touches a form.
        CertificationLevel::FormFillAndSignatures => kind == EditKind::FormFill,
        CertificationLevel::AnnotationsToo => {
            matches!(kind, EditKind::FormFill | EditKind::Annotation)
        }
    };

    if permitted {
        Verdict::Permitted
    } else {
        Verdict::Refused
    }
}

/// The error a refused edit returns, naming the edit and the level that forbids it.
///
/// Built here rather than at each call site so the sentence cannot drift between commands, and so a
/// test can assert the reason without reaching into a command's body.
pub fn refusal(level: CertificationLevel, kind: EditKind) -> AppError {
    AppError::CertificationRefusesEdit {
        edit: kind.code(),
        level: level.number(),
    }
}

/// Enforce [`permits`] at a command boundary, returning the refusal when the edit is forbidden.
///
/// Every mutating command a certification governs calls this before it writes anything. Returning
/// `Err` *before* the mutation is what makes the refusal safe: a command that wrote first and checked
/// afterwards would leave the user's file changed by an edit it then reported as refused.
pub fn check(level: Option<CertificationLevel>, kind: EditKind) -> Result<(), AppError> {
    match permits(level, kind) {
        Verdict::Permitted => Ok(()),
        Verdict::Refused => Err(refusal(
            // `permits` refuses only when a level was declared, so the `expect` cannot fire; spelling
            // it out keeps the refusal's sentence honest rather than defaulting to a made-up level.
            level.expect("a refusal requires a declared level"),
            kind,
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_KINDS: [EditKind; 5] = [
        EditKind::Annotation,
        EditKind::FormFill,
        EditKind::Redaction,
        EditKind::Content,
        EditKind::Signature,
    ];

    const ALL_LEVELS: [CertificationLevel; 3] = [
        CertificationLevel::NoChanges,
        CertificationLevel::FormFillAndSignatures,
        CertificationLevel::AnnotationsToo,
    ];

    /// The complete normative matrix, written out by hand from ISO 32000-1 §12.8.2.2.
    ///
    /// Exhaustive on purpose: every (level, kind) pair appears, so a change to `permits` that keeps
    /// one row right cannot pass. A table-driven test would have to restate the table to build
    /// itself; this states the expected verdict literally, which is the point.
    #[test]
    fn the_permitted_edit_matrix_matches_the_specification() {
        use CertificationLevel::*;
        use EditKind::*;

        let expected = [
            (NoChanges, Annotation, false),
            (NoChanges, FormFill, false),
            (NoChanges, Redaction, false),
            (NoChanges, Content, false),
            (NoChanges, Signature, true),
            (FormFillAndSignatures, Annotation, false),
            (FormFillAndSignatures, FormFill, true),
            (FormFillAndSignatures, Redaction, false),
            (FormFillAndSignatures, Content, false),
            (FormFillAndSignatures, Signature, true),
            (AnnotationsToo, Annotation, true),
            (AnnotationsToo, FormFill, true),
            (AnnotationsToo, Redaction, false),
            (AnnotationsToo, Content, false),
            (AnnotationsToo, Signature, true),
        ];

        // Every pair is covered. Asserted rather than assumed, so deleting a row from the literal
        // above is a failure rather than a silently smaller matrix.
        assert_eq!(
            expected.len(),
            ALL_LEVELS.len() * ALL_KINDS.len(),
            "the matrix must cover every (level, kind) pair"
        );

        for (level, kind, want) in expected {
            assert_eq!(
                permits(Some(level), kind),
                if want {
                    Verdict::Permitted
                } else {
                    Verdict::Refused
                },
                "P={} with {kind:?}: expected permitted={want}",
                level.number()
            );
        }
    }

    /// An uncertified document permits everything.
    ///
    /// Distinct from the matrix because it is the `None` arm: getting it wrong refuses edits on files
    /// nobody certified, which a user would report as "the editor is broken".
    #[test]
    fn an_uncertified_document_permits_every_edit() {
        for kind in ALL_KINDS {
            assert_eq!(
                permits(None, kind),
                Verdict::Permitted,
                "no certification must permit {kind:?}"
            );
            assert!(
                check(None, kind).is_ok(),
                "no certification must not produce an error for {kind:?}"
            );
        }
    }

    /// The levels round-trip through their `/P` numbers, and undefined numbers map to nothing.
    #[test]
    fn levels_round_trip_through_their_numbers() {
        use CertificationLevel::*;
        for level in [NoChanges, FormFillAndSignatures, AnnotationsToo] {
            assert_eq!(
                CertificationLevel::from_number(level.number()),
                Some(level),
                "P={} must round-trip",
                level.number()
            );
        }
        for undefined in [0u8, 4, 5, 255] {
            assert_eq!(
                CertificationLevel::from_number(undefined),
                None,
                "/P {undefined} is undefined and must not become a level"
            );
        }
    }

    /// A refusal names the edit and the level, so the user can tell why it happened.
    #[test]
    fn a_refusal_states_the_edit_and_the_level() {
        let error = refusal(CertificationLevel::NoChanges, EditKind::Annotation);
        let message = error.to_string();
        assert!(
            message.contains("annotation"),
            "the refusal must name the edit: {message}"
        );
        assert!(
            message.contains("P=1"),
            "the refusal must name the level: {message}"
        );
        assert_eq!(error.code(), "CERTIFICATION_REFUSES_EDIT");
    }

    /// `check` refuses exactly the pairs the matrix refuses: it is the predicate plus an error, and a
    /// divergence between them would mean the enforcement layer decides by a different rule.
    #[test]
    fn check_refuses_exactly_what_the_predicate_refuses() {
        for kind in ALL_KINDS {
            for level in ALL_LEVELS {
                assert_eq!(
                    permits(Some(level), kind) == Verdict::Refused,
                    check(Some(level), kind).is_err(),
                    "check and permits disagree at P={} for {kind:?}",
                    level.number()
                );
            }
        }
    }
}
