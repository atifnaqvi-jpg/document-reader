//! What we know about whether a certificate was revoked (P3.2 D-SG4, R3).
//!
//! **The answer is allowed to be "we did not check", and that is the common answer.** R3 rules
//! offline verification in and TSA mode out: no network call is made anywhere in the sign or
//! verify path. Revocation information therefore comes from exactly two places -- the document
//! itself (an embedded CRL or DSS), or a store the user explicitly imported. Nothing here fetches,
//! and there is no argument you can pass to make it.
//!
//! The dangerous default is the one that needs no code: treating "chains to a certificate we like"
//! as "not revoked". Those are different claims, and the second one requires evidence we do not
//! have offline. So the type carries the *provenance* of the answer alongside the answer, and the
//! absence of evidence is its own variant rather than a default that reads as a pass.
//!
//! This module is deliberately platform-neutral and free of I/O. It decides; it does not look
//! anything up. The Linux gate can prove every branch of it, which is the same split `identity.rs`
//! uses for the certificate store (K19): only the platform read is Windows-only, never the
//! judgement layered on top of it.

/// Where a revocation answer came from, because the source is part of the claim.
///
/// A "good" from an imported CRL is a real check against a list someone chose. A "good" from the
/// document's own DSS is a statement the *signer* made about their own signature, which is worth
/// recording but is not the same evidence. Collapsing the two would make an attacker-supplied
/// document able to assert its own non-revocation with no marker to show for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RevocationSource {
    /// A CRL or OCSP response embedded in the document being verified.
    Document,
    /// A store the user explicitly imported or configured.
    ImportedStore,
}

impl RevocationSource {
    /// A short stable label for a support report or a badge tooltip. Not localised here: the UI
    /// owns wording, and a code path that formatted user-facing English would be a second place
    /// to keep consistent.
    pub fn label(self) -> &'static str {
        match self {
            RevocationSource::Document => "document",
            RevocationSource::ImportedStore => "imported store",
        }
    }

    /// True when the evidence was authored by the signer rather than by an authority the user
    /// chose. The UI must not present these two with equal weight, so the distinction is a method
    /// rather than a comment.
    pub fn is_self_asserted(self) -> bool {
        matches!(self, RevocationSource::Document)
    }
}

/// The revocation finding, with its provenance.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Revocation {
    /// Revoked, per the named source.
    Revoked { source: RevocationSource },
    /// Not revoked at a stated time, per the named source. The time is when the CRL or response
    /// was produced, not when we read it: a CRL from a year ago still says only what was true a
    /// year ago, and that difference is exactly what a reader needs to judge it.
    NotRevoked { source: RevocationSource },
    /// No usable revocation data was available. **This is not a pass.** It is the honest answer
    /// offline, and D-SG7 renders it as `unknown` rather than folding it into a green badge.
    NotChecked { reason: NotCheckedReason },
}

/// Why no revocation answer was available. Kept as separate variants because the UI can say
/// something useful about each, and because "we have no data" and "the data we have is unusable"
/// are different operational problems for the user.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotCheckedReason {
    /// Neither the document nor an imported store carried any revocation data for this
    /// certificate. The expected state for an ordinary offline machine.
    NoData,
    /// Data was present but had expired at the moment of checking. A caller must not treat this as
    /// `NotRevoked`: an expired CRL asserts nothing about today.
    Expired,
    /// Data was present but names a different certificate or issuer, so it does not apply.
    DoesNotApply,
}

impl Revocation {
    /// The badge this finding supports (D-SG7). `NotChecked` maps to `unknown`, never to a pass.
    ///
    /// Written as a method on the finding rather than at each call site so the mapping exists in
    /// exactly one place: a second copy would drift, and the one that drifted would be the one
    /// that rendered a green badge for a check that never happened.
    pub fn supports_valid(self) -> bool {
        matches!(self, Revocation::NotRevoked { .. })
    }

    /// True when we can honestly say the certificate is *known* not to be revoked.
    ///
    /// Deliberately false for `NotChecked`. A caller that wants "should I show a warning" gets the
    /// conservative answer without having to remember the distinction.
    pub fn is_known_good(self) -> bool {
        self.supports_valid()
    }
}

/// Decide the revocation finding from evidence that is already in hand.
///
/// There is no third source and no fetch: the two parameters are the document's embedded data and
/// the imported store's data, and the function cannot reach anything else. That is the whole of
/// D-SG4's enforcement on the deciding side -- the compile-surface ban in `clippy.toml` covers the
/// possibility of someone adding a third.
pub fn decide(
    document_data: Option<RevocationEvidence>,
    imported_data: Option<RevocationEvidence>,
    now_unix: i64,
) -> Revocation {
    // The imported store wins when both have something to say. Rationale: the user chose it, and a
    // document asserting its own signer's non-revocation is the weaker evidence (see
    // `RevocationSource::is_self_asserted`). If the imported data is expired or inapplicable we
    // fall through to the document's rather than discarding both, and record which one answered.
    let imported = imported_data.map(|evidence| (evidence, RevocationSource::ImportedStore));
    let document = document_data.map(|evidence| (evidence, RevocationSource::Document));

    let mut reason = NotCheckedReason::NoData;
    for (evidence, source) in [imported, document].into_iter().flatten() {
        match evidence {
            RevocationEvidence::Revoked => return Revocation::Revoked { source },
            RevocationEvidence::NotRevoked { produced_at_unix } if produced_at_unix <= now_unix => {
                return Revocation::NotRevoked { source }
            }
            // A CRL with a production time in the future is not usable evidence: either the clock
            // is wrong or the file is not what it claims, and neither lets us assert anything.
            RevocationEvidence::NotRevoked { .. } => {
                reason = NotCheckedReason::Expired;
            }
            RevocationEvidence::Expired => reason = NotCheckedReason::Expired,
            RevocationEvidence::DoesNotApply => reason = NotCheckedReason::DoesNotApply,
        }
    }

    Revocation::NotChecked { reason }
}

/// Revocation data reduced to the one fact this module needs, so the deciding logic never touches a
/// CRL parser.
///
/// The parsers are a separate question and a separate landing; keeping them out of here is what
/// lets every branch of the decision be proved on Linux with no fixture certificate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RevocationEvidence {
    /// The list names this certificate as revoked.
    Revoked,
    /// The list covers this certificate and does not name it, as of `produced_at_unix`.
    NotRevoked { produced_at_unix: i64 },
    /// The data exists but its validity window has passed.
    Expired,
    /// The data names a different certificate or issuer.
    DoesNotApply,
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_700_000_000;

    #[test]
    fn no_data_anywhere_is_not_checked_rather_than_not_revoked() {
        // The whole point of D-SG4: silence must not read as a pass.
        let finding = decide(None, None, NOW);
        assert_eq!(
            finding,
            Revocation::NotChecked {
                reason: NotCheckedReason::NoData
            }
        );
        assert!(
            !finding.supports_valid(),
            "the absence of revocation data must never support a valid badge"
        );
    }

    #[test]
    fn imported_evidence_is_preferred_over_the_documents_own_claim() {
        let finding = decide(
            Some(RevocationEvidence::NotRevoked {
                produced_at_unix: NOW - 10,
            }),
            Some(RevocationEvidence::NotRevoked {
                produced_at_unix: NOW - 10,
            }),
            NOW,
        );
        assert_eq!(
            finding,
            Revocation::NotRevoked {
                source: RevocationSource::ImportedStore
            },
            "the store the user chose must outrank the document's self-assertion"
        );
    }

    #[test]
    fn a_documents_own_evidence_is_marked_as_self_asserted() {
        let finding = decide(
            Some(RevocationEvidence::NotRevoked {
                produced_at_unix: NOW - 10,
            }),
            None,
            NOW,
        );
        match finding {
            Revocation::NotRevoked { source } => {
                assert!(
                    source.is_self_asserted(),
                    "document evidence is the signer's own claim"
                );
            }
            other => panic!("expected NotRevoked from the document, got {other:?}"),
        }
    }

    #[test]
    fn revoked_from_either_source_is_revoked() {
        for source in [RevocationSource::Document, RevocationSource::ImportedStore] {
            let document =
                (source == RevocationSource::Document).then_some(RevocationEvidence::Revoked);
            let imported =
                (source == RevocationSource::ImportedStore).then_some(RevocationEvidence::Revoked);
            assert_eq!(
                decide(document, imported, NOW),
                Revocation::Revoked { source },
                "a revoked finding must survive from {source:?}"
            );
        }
    }

    #[test]
    fn expired_imported_evidence_falls_through_to_the_documents() {
        let finding = decide(
            Some(RevocationEvidence::NotRevoked {
                produced_at_unix: NOW - 10,
            }),
            Some(RevocationEvidence::Expired),
            NOW,
        );
        assert_eq!(
            finding,
            Revocation::NotRevoked {
                source: RevocationSource::Document
            },
            "an expired list must not discard usable document evidence"
        );
    }

    #[test]
    fn expired_evidence_alone_reports_expired_not_not_revoked() {
        let finding = decide(None, Some(RevocationEvidence::Expired), NOW);
        assert_eq!(
            finding,
            Revocation::NotChecked {
                reason: NotCheckedReason::Expired
            },
            "an expired CRL asserts nothing about today"
        );
    }

    #[test]
    fn evidence_produced_in_the_future_is_not_usable() {
        // A production time after "now" means a wrong clock or a file that is not what it claims.
        let finding = decide(
            None,
            Some(RevocationEvidence::NotRevoked {
                produced_at_unix: NOW + 1,
            }),
            NOW,
        );
        assert_eq!(
            finding,
            Revocation::NotChecked {
                reason: NotCheckedReason::Expired
            }
        );
    }

    #[test]
    fn inapplicable_evidence_is_reported_as_such() {
        let finding = decide(None, Some(RevocationEvidence::DoesNotApply), NOW);
        assert_eq!(
            finding,
            Revocation::NotChecked {
                reason: NotCheckedReason::DoesNotApply
            }
        );
    }

    #[test]
    fn only_not_revoked_supports_a_valid_badge() {
        assert!(Revocation::NotRevoked {
            source: RevocationSource::Document
        }
        .supports_valid());
        assert!(!Revocation::Revoked {
            source: RevocationSource::ImportedStore
        }
        .supports_valid());
        assert!(!Revocation::NotChecked {
            reason: NotCheckedReason::NoData
        }
        .supports_valid());
    }
}
