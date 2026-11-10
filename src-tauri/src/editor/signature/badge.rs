//! What a signature badge may honestly claim (P3.2 D-SG7), and the certification level it reports
//! (D-SG6).
//!
//! **The badge is a judgement over evidence we already hold, never a lookup.** This module decides
//! and formats nothing platform-specific, so the Linux gate proves every branch of it. Only the
//! reading of a signature dictionary is platform-shaped, and that lives elsewhere.
//!
//! The load-bearing rule is D-SG7's: a green badge that means "we did not check" is worse than no
//! badge. So each of the four states is reachable only from evidence that supports it, and the
//! absence of evidence is `unknown` in its own right rather than a default that reads as a pass.
//!
//! Three honest limits shape this type, and none of them is a stub:
//!
//! * **`valid` needs a local anchor.** D-SG4 makes trust local configuration only; a certificate
//!   that chains to nothing the user chose is `unknown`, never `valid`.
//! * **`valid` needs revocation evidence.** Absent revocation data is listed by D-SG7 under
//!   `unknown`. Offline (R3) that is the common case, so `valid` is deliberately hard to reach.
//! * **A revoked certificate has no state of its own.** The four states cannot express "the bytes
//!   are intact but the signer was revoked"; see [`Detail::revoked`] and the note on
//!   [`Badge::severity`].
//!
//! **This module does not verify a signature cryptographically.** `Integrity` is supplied by the
//! caller, and `Integrity::NotChecked` -- the honest value when nothing computed a digest -- can
//! never produce `valid`.
//!
//! In production that caller is `store::assess`, and the value comes from `verify::verify`, which
//! does recompute the SHA-256 over `/ByteRange` and check the RSA signature over the signed
//! attributes. (An earlier revision of this note said production had no asymmetric verifier at all;
//! that was true when it was written and stopped being true when the verifier landed. The separate
//! independent implementation in `tests/support/cms_verifier.rs` still exists, because I-SG1 asks
//! for one that is *not* ours -- a symmetric bug would otherwise pass both sides.)

use super::revocation::{NotCheckedReason, Revocation, RevocationSource};

/// The four states D-SG7 allows, and nothing else.
///
/// A fifth state would be a claim we cannot support offline. That constraint bites hardest on a
/// revoked certificate: it is neither `valid` nor damaged, and it has no state of its own. Such a
/// signature is reported `unknown` with [`Detail::revoked`] set, which the UI must render as a
/// warning rather than as neutrality.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Badge {
    /// Integrity intact, no later revision, anchored locally, and known not revoked.
    Valid,
    /// We could not establish one of the four facts above. Not a pass.
    Unknown,
    /// Damage we established: the signed bytes changed, or a later revision was appended.
    Modified,
    /// The document carries no signature at all.
    Unsigned,
}

impl Badge {
    /// Relative badness, used only to combine several signatures into one document badge.
    ///
    /// Ordered so that a *fact* outranks an *absence*: `modified` is something we established and
    /// must not be averaged away by a sibling signature we could only call `unknown`.
    fn severity(self) -> u8 {
        match self {
            Badge::Valid => 0,
            Badge::Unknown => 1,
            Badge::Modified => 2,
            // Unreachable from [`decide`]; ranked above `modified` so that a future caller mixing
            // it into a list cannot make the document look better than its parts.
            Badge::Unsigned => 3,
        }
    }

    /// A stable code for the UI and for support reports. Not prose: the UI owns wording, and a
    /// second place that formatted user-facing English would drift from the first.
    pub fn code(self) -> &'static str {
        match self {
            Badge::Valid => "valid",
            Badge::Unknown => "unknown",
            Badge::Modified => "modified",
            Badge::Unsigned => "unsigned",
        }
    }

    /// Whether this state may be rendered as a positive outcome.
    ///
    /// A method rather than a comparison at each call site, for the same reason
    /// `Revocation::supports_valid` is one: the copy that drifts would be the one painting a green
    /// badge for a check that never happened.
    pub fn is_green(self) -> bool {
        matches!(self, Badge::Valid)
    }
}

/// Whether the signed byte range still hashes to what the signature recorded.
///
/// `NotChecked` is its own variant rather than an `Option<bool>` because the three outcomes carry
/// three different sentences and only one of them is good news.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Integrity {
    /// The digest recomputed over `/ByteRange` matched the `messageDigest` in `/Contents`.
    Intact,
    /// It did not match, or the ranges no longer describe this file's bytes.
    Damaged,
    /// Nothing recomputed the digest. Never supports `valid`.
    NotChecked,
}

/// Everything the badge is decided from, gathered by the caller.
///
/// Plain data on purpose: the decision is then a total function of the struct, which is what makes
/// each branch provable in a test with no certificate, no file and no platform.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SignatureEvidence {
    pub integrity: Integrity,
    /// True when a revision was appended *after* the one this signature covers.
    ///
    /// D-SG7 counts this as `modified`. The bytes the signature covers are untouched, so it is not
    /// damage to the signed range -- but the file the reader holds is no longer the file that was
    /// signed, and calling that `valid` would be the false green.
    pub later_revision: bool,
    /// True when the signer's chain anchors in a local trust store the user chose (D-SG4).
    pub anchored: bool,
    /// The revocation finding, from `revocation::decide`. Never fetched (R3).
    pub revocation: Revocation,
    /// The declared DocMDP level, `Some(1..=3)` for a certification signature, `None` for an
    /// ordinary approval signature (D-SG6).
    pub certification_level: Option<u8>,
}

/// Decide one signature's badge.
///
/// The order of the checks is the precedence, and it is deliberate:
///
/// 1. **damage** and **a later revision** are facts we established, so they outrank everything;
/// 2. **unchecked integrity** is an absence, and an absence cannot be better than `unknown`;
/// 3. only then does trust and revocation decide between `valid` and `unknown`.
pub fn decide(evidence: &SignatureEvidence) -> Badge {
    if evidence.integrity == Integrity::Damaged || evidence.later_revision {
        return Badge::Modified;
    }
    if evidence.integrity == Integrity::NotChecked {
        return Badge::Unknown;
    }
    if !evidence.anchored {
        return Badge::Unknown;
    }
    if !evidence.revocation.supports_valid() {
        return Badge::Unknown;
    }
    Badge::Valid
}

/// Combine every signature in a document into the one badge the status area shows.
///
/// The worst wins. A document with one intact signature and one modified signature is modified: the
/// question the badge answers is "can I rely on what this file says", and one damaged signature
/// answers it no regardless of its neighbours.
pub fn decide_document(signatures: &[SignatureEvidence]) -> Badge {
    if signatures.is_empty() {
        return Badge::Unsigned;
    }
    let mut worst = Badge::Valid;
    for evidence in signatures {
        let badge = decide(evidence);
        if badge.severity() > worst.severity() {
            worst = badge;
        }
    }
    worst
}

/// What was and was not checked, for the detail view D-SG7 requires behind the badge.
///
/// Kept separate from [`Badge`] because the badge is a summary and this is the evidence: the design
/// asks that clicking the badge shows the difference between "checked and fine" and "never
/// checked", and a summary alone cannot express it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Detail {
    pub badge: Badge,
    /// True only when a digest was actually recomputed.
    pub integrity_checked: bool,
    pub chain_anchored: bool,
    /// The revocation finding *with its provenance*, not a flattened boolean.
    ///
    /// This was a `bool` and that lost two things the design asks to be shown. §6 requires the
    /// detail view to say "revocation: not checked (offline)" -- with a boolean, "not checked" and
    /// "checked and clean" are both `false`, so the one sentence that stops a neutral badge being
    /// read as a quiet pass could not be rendered at all. It also discarded
    /// [`RevocationSource`], which `is_self_asserted` exists specifically to distinguish: a CRL the
    /// signer put in their own document is not the same evidence as one the user imported.
    ///
    /// Carried whole rather than re-derived so the detail cannot disagree with the decision above it.
    pub revocation: Revocation,
    pub certification_level: Option<u8>,
}

/// Build the detail view, deriving the badge rather than taking it, so the two cannot disagree.
pub fn explain(evidence: &SignatureEvidence) -> Detail {
    Detail {
        badge: decide(evidence),
        integrity_checked: evidence.integrity != Integrity::NotChecked,
        chain_anchored: evidence.anchored,
        revocation: evidence.revocation,
        certification_level: evidence.certification_level,
    }
}

impl Detail {
    /// Whether the signer was found on a revocation list.
    ///
    /// A method rather than a stored `bool`, because a field and the `revocation` it summarises
    /// could drift apart. Callers that only need the yes/no can still ask for it.
    pub fn revoked(&self) -> bool {
        matches!(self.revocation, Revocation::Revoked { source: _ })
    }

    /// One line per fact, in the order a reader should weigh them.
    ///
    /// Prose lives here rather than in [`Badge::code`] because these sentences are the *explanation*
    /// the design asks for, not a machine-readable state.
    pub fn lines(&self) -> Vec<&'static str> {
        let mut lines = Vec::with_capacity(4);
        lines.push(match self.badge {
            Badge::Unsigned => "this document carries no signature",
            Badge::Modified => "the signed bytes have changed since signing",
            Badge::Valid => "the signature verifies against the bytes as shipped",
            Badge::Unknown => "the signature could not be confirmed",
        });
        lines.push(match self.integrity_checked {
            true => "integrity: the signed range was hashed and matched",
            false => "integrity: not checked",
        });
        lines.push(match self.chain_anchored {
            true => "certificate chain: anchored in a local trust store",
            false => "certificate chain: not anchored in any local trust store",
        });
        // D-SG7/§6: revocation gets a line *whatever* the answer, including the answer "we did not
        // check". A line that appears only for `Revoked` leaves the common offline case silent, and
        // silence beside a neutral badge reads as a quiet pass. `Revocation` already carries the
        // provenance, so the evidence is stated as well as the verdict.
        lines.push(match self.revocation {
            Revocation::Revoked { source } => match source {
                RevocationSource::Document => {
                    "revocation: REVOKED by a list embedded in this document"
                }
                RevocationSource::ImportedStore => "revocation: REVOKED by a list you imported",
            },
            Revocation::NotRevoked { source } => match source {
                RevocationSource::Document => {
                    "revocation: not revoked, per a list embedded in this document (the signer's \
                     own claim, not an authority you chose)"
                }
                RevocationSource::ImportedStore => {
                    "revocation: not revoked, per a list you imported"
                }
            },
            // The three reasons are different operational problems and the UI can act on each, so
            // they are not collapsed into one "could not check".
            Revocation::NotChecked { reason } => match reason {
                NotCheckedReason::NoData => "revocation: not checked (offline, no data)",
                NotCheckedReason::Expired => {
                    "revocation: not checked (the revocation data had expired)"
                }
                NotCheckedReason::DoesNotApply => {
                    "revocation: not checked (the revocation data names a different certificate)"
                }
            },
        });
        if let Some(level) = self.certification_level {
            lines.push(match level {
                1 => "certification: no changes permitted after this signature",
                2 => "certification: form filling and further signatures permitted",
                _ => "certification: annotations permitted as well",
            });
        }
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::signature::revocation::{NotCheckedReason, RevocationSource};

    /// Evidence that is as good as it gets offline: intact, anchored, known not revoked.
    fn good() -> SignatureEvidence {
        SignatureEvidence {
            integrity: Integrity::Intact,
            later_revision: false,
            anchored: true,
            revocation: Revocation::NotRevoked {
                source: RevocationSource::ImportedStore,
            },
            certification_level: None,
        }
    }

    #[test]
    fn fully_evidenced_signature_is_the_only_valid_one() {
        assert_eq!(decide(&good()), Badge::Valid);
        assert!(decide(&good()).is_green());
    }

    #[test]
    fn no_signature_is_unsigned_not_valid() {
        assert_eq!(decide_document(&[]), Badge::Unsigned);
        assert!(!Badge::Unsigned.is_green());
    }

    #[test]
    fn damaged_bytes_are_modified() {
        let mut evidence = good();
        evidence.integrity = Integrity::Damaged;
        assert_eq!(decide(&evidence), Badge::Modified);
    }

    #[test]
    fn a_later_revision_is_modified_even_with_the_signed_range_intact() {
        // The signed bytes are untouched, so a digest check passes; the file is still not the file
        // that was signed. This is the case that a naive "does it hash" test gets wrong.
        let mut evidence = good();
        evidence.later_revision = true;
        assert_eq!(decide(&evidence), Badge::Modified);
    }

    #[test]
    fn unchecked_integrity_is_unknown_and_never_valid() {
        let mut evidence = good();
        evidence.integrity = Integrity::NotChecked;
        assert_eq!(
            decide(&evidence),
            Badge::Unknown,
            "an absence of a digest cannot support a green badge"
        );
    }

    #[test]
    fn an_unanchored_chain_is_unknown() {
        let mut evidence = good();
        evidence.anchored = false;
        assert_eq!(decide(&evidence), Badge::Unknown);
    }

    #[test]
    fn absent_revocation_data_is_unknown_not_valid() {
        let mut evidence = good();
        evidence.revocation = Revocation::NotChecked {
            reason: NotCheckedReason::NoData,
        };
        assert_eq!(
            decide(&evidence),
            Badge::Unknown,
            "D-SG7: offline silence is unknown"
        );
    }

    #[test]
    fn a_revoked_signer_never_yields_valid_and_is_flagged_in_the_detail() {
        let mut evidence = good();
        evidence.revocation = Revocation::Revoked {
            source: RevocationSource::ImportedStore,
        };
        assert_ne!(decide(&evidence), Badge::Valid);
        let detail = explain(&evidence);
        assert!(
            detail.revoked(),
            "the detail must carry the finding the badge cannot express"
        );
        assert_eq!(
            detail.revocation,
            Revocation::Revoked {
                source: RevocationSource::ImportedStore
            },
            "the detail must keep the provenance too: 'revoked by a list I imported' and 'revoked \
             by a list the signer embedded' are different claims, and §6 asks for that to show"
        );
        assert!(
            detail.lines().iter().any(|line| line.contains("REVOKED")),
            "a revoked signer must be stated, not left to be inferred from a neutral badge"
        );
    }

    #[test]
    fn every_revocation_answer_gets_a_line_including_we_did_not_check() {
        // The design's own wording: clicking the badge must be able to show "revocation: not
        // checked (offline)". This is the sentence that stops a neutral `unknown` badge from being
        // read as a quiet pass, and it was unrenderable while the finding was a `bool` -- "not
        // checked" and "checked, clean" were both `false`, so no line could tell them apart.
        let cases = [
            Revocation::Revoked {
                source: RevocationSource::Document,
            },
            Revocation::Revoked {
                source: RevocationSource::ImportedStore,
            },
            Revocation::NotRevoked {
                source: RevocationSource::Document,
            },
            Revocation::NotRevoked {
                source: RevocationSource::ImportedStore,
            },
            Revocation::NotChecked {
                reason: NotCheckedReason::NoData,
            },
            Revocation::NotChecked {
                reason: NotCheckedReason::Expired,
            },
            Revocation::NotChecked {
                reason: NotCheckedReason::DoesNotApply,
            },
        ];

        let mut seen: Vec<&'static str> = Vec::new();
        for revocation in cases {
            let mut evidence = good();
            evidence.revocation = revocation;
            let detail = explain(&evidence);
            let line = detail
                .lines()
                .into_iter()
                .find(|line| line.starts_with("revocation:"))
                .unwrap_or_else(|| panic!("no revocation line for {revocation:?}"));
            assert!(
                !seen.contains(&line),
                "two different findings rendered the same sentence ({line:?}); the detail would \\
                 then say nothing about which evidence it rests on"
            );
            seen.push(line);
            // Provenance must be legible: the signer's own embedded list is not an authority the
            // user chose, and `is_self_asserted` exists only if something surfaces the difference.
            if let Revocation::NotRevoked { source } | Revocation::Revoked { source } = revocation {
                if source.is_self_asserted() {
                    assert!(
                        line.contains("embedded in this document"),
                        "a signer-asserted finding must say so; got {line:?}"
                    );
                } else {
                    assert!(
                        line.contains("imported"),
                        "a user-imported finding must say so; got {line:?}"
                    );
                }
            }
        }
        assert_eq!(seen.len(), 7, "every finding has its own sentence");
    }

    #[test]
    fn damage_outranks_an_absence_of_trust() {
        // Both facts are present. The established one wins: reporting `unknown` here would hide
        // that we know the bytes changed.
        let mut evidence = good();
        evidence.integrity = Integrity::Damaged;
        evidence.anchored = false;
        assert_eq!(decide(&evidence), Badge::Modified);
    }

    #[test]
    fn the_document_badge_is_the_worst_of_its_parts() {
        let mut damaged = good();
        damaged.integrity = Integrity::Damaged;
        assert_eq!(
            decide_document(&[good(), damaged]),
            Badge::Modified,
            "one damaged signature answers the document's reliability on its own"
        );
        assert_eq!(decide_document(&[good(), good()]), Badge::Valid);
    }

    #[test]
    fn the_detail_cannot_disagree_with_the_badge() {
        // `explain` derives the badge rather than accepting it; this pins that shape so a future
        // refactor cannot pass a pre-computed badge in.
        for evidence in [
            good(),
            SignatureEvidence {
                integrity: Integrity::Damaged,
                ..good()
            },
            SignatureEvidence {
                anchored: false,
                ..good()
            },
            SignatureEvidence {
                integrity: Integrity::NotChecked,
                ..good()
            },
        ] {
            assert_eq!(explain(&evidence).badge, decide(&evidence));
        }
    }

    #[test]
    fn the_certification_level_is_reported_as_written() {
        let mut evidence = good();
        evidence.certification_level = Some(1);
        let detail = explain(&evidence);
        assert_eq!(detail.certification_level, Some(1));
        assert!(
            detail
                .lines()
                .iter()
                .any(|line| line.contains("no changes permitted")),
            "D-SG6: the level must be shown in words, not as a bare number"
        );
    }

    #[test]
    fn badge_codes_are_stable() {
        // The UI and support reports key off these strings; a rename is a silent break.
        assert_eq!(Badge::Valid.code(), "valid");
        assert_eq!(Badge::Unknown.code(), "unknown");
        assert_eq!(Badge::Modified.code(), "modified");
        assert_eq!(Badge::Unsigned.code(), "unsigned");
    }
}
