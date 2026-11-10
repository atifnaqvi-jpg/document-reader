//! The trust store D-SG4 leaves to the user, and the chain path built from it.
//!
//! D-SG4 in one sentence: *"The trust store is a local file (or the Windows store) the user has
//! chosen; a certificate that chains to nothing there is reported `unknown`, never `valid`."*
//! This module is that store's deciding half. It answers one question -- **does the signer's
//! certificate chain to an anchor the user imported** -- and it answers only by building a path and
//! checking each signature on it.
//!
//! **Why a path and not a comparison.** The cheapest way to write this is `anchors.contains(signer)`
//! and it is wrong twice over: a leaf that *is* an imported certificate proves nothing about who
//! issued it, and a name that matches an anchor is a string, not a signature. So the check is a
//! walk: find an anchor whose subject issued the current certificate, verify that anchor's key
//! produced the signature on the certificate, and repeat up to a bounded depth. Only a path whose
//! every edge verifies reaches `anchored`.
//!
//! **Where the cryptography lives.** Not here. `x509-cert` parses but does **not** verify
//! signatures, so checking an edge needs `rsa`, which design K25 confines to `cms.rs` and
//! `verify.rs`. This module therefore states what it needs as a [`LinkVerifier`] trait and the
//! caller supplies an implementation. The payoff is the one K25 exists for: every branch below is
//! provable on Linux with no key, no clock and no platform.
//!
//! **What this module does not claim.** Not that the signer is who the subject says; not that the
//! anchor itself is trustworthy beyond the user having chosen it; and not that a certificate is
//! unexpired -- [`Chain`] reports the validity window it saw and the caller decides, because
//! "expired at the reader's clock" is a fact the badge does not express today.
//!
//! The three honest negatives this file exists to keep honest:
//!
//! * an empty anchor set anchors nothing, so a self-signed signer with no anchor is **`unknown`**;
//! * an anchor that did not issue the signer anchors nothing, even if both are real certificates;
//! * a path whose edge signature does not verify is refused, not truncated to the part that did.

use std::collections::HashSet;

use x509_cert::der::Decode;

/// One certificate reduced to what a path walk needs.
///
/// Parsed once on the way in rather than per comparison: a store of a thousand anchors would
/// otherwise be re-decoded for every chain step, and the parse is the expensive part.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CertFacts {
    /// The exact DER this was parsed from. Kept because the edge verifier needs the bytes that were
    /// signed, not a re-encoding of them.
    pub der: Vec<u8>,
    /// Distinguished name of the issuer, as `x509-cert` renders it.
    pub issuer: String,
    /// Distinguished name of the subject.
    pub subject: String,
    /// Upper-case hex, no separators. Compared only within one issuer, per RFC 5280.
    pub serial_hex: String,
    /// True when `BasicConstraints` says `CA:TRUE`, or when the certificate is its own issuer.
    ///
    /// Self-signed is folded in deliberately: a v1 root predates `BasicConstraints`, and a store
    /// that refused every such anchor would refuse the oldest and most carefully chosen ones.
    pub is_ca: bool,
    /// `pathLenConstraint`, when the certificate states one. A CA may issue at most this many
    /// intermediate CAs beneath it.
    pub path_len: Option<u8>,
    pub not_before_unix: i64,
    pub not_after_unix: i64,
}

impl CertFacts {
    /// Parse one DER certificate into the facts a path walk needs.
    ///
    /// Every failure is reported rather than dropped: an anchor the user imported and this build
    /// cannot read is a fact the store must show, because a silently absent anchor is
    /// indistinguishable from one that was never imported.
    pub fn parse(der: &[u8]) -> Result<Self, TrustError> {
        let certificate = x509_cert::Certificate::from_der(der)
            .map_err(|error| TrustError::Unreadable(error.to_string()))?;
        let tbs = certificate.tbs_certificate();

        let subject = tbs.subject().to_string();
        let issuer = tbs.issuer().to_string();
        // `BasicConstraints` is read through the typed extension rather than by scanning for the
        // OID: a malformed extension then fails here, with a reason, instead of reading as absent
        // and silently demoting a CA to a leaf.
        let basic = tbs
            .get_extension::<x509_cert::ext::pkix::BasicConstraints>()
            .ok()
            .flatten()
            .map(|(_, constraints)| constraints);

        Ok(Self {
            der: der.to_vec(),
            issuer: issuer.clone(),
            is_ca: basic.as_ref().is_some_and(|constraints| constraints.ca) || subject == issuer,
            path_len: basic.and_then(|constraints| constraints.path_len_constraint),
            subject,
            serial_hex: hex::encode_upper(tbs.serial_number().as_bytes()),
            not_before_unix: tbs.validity().not_before.to_unix_duration().as_secs() as i64,
            not_after_unix: tbs.validity().not_after.to_unix_duration().as_secs() as i64,
        })
    }

    /// Whether this certificate's validity window contains `now_unix`.
    pub fn valid_at(&self, now_unix: i64) -> bool {
        self.not_before_unix <= now_unix && now_unix <= self.not_after_unix
    }
}

/// Why a certificate or a store could not be used.
///
/// Separate from [`crate::model::AppError`] because this module is also called from tests and from
/// the store's own bookkeeping, where a database error variant would be noise. The command layer
/// translates these into user-facing variants; keeping the translation there means one place owns
/// the wording.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrustError {
    /// The DER did not parse as an X.509 certificate.
    Unreadable(String),
    /// The DER did not parse as a CRL.
    CrlUnreadable(String),
    /// No path from the signer to any imported anchor.
    ///
    /// This is **not** an error in the sense of "something went wrong": it is the ordinary answer
    /// for a self-signed signer and for a signer whose CA the user has not imported. It is carried
    /// as a value so the badge can say `unknown` without the command turning it into a failure.
    NoPath,
    /// A path existed but an edge on it did not verify. Distinct from [`TrustError::NoPath`]
    /// because it names a different situation: trust material was present and was wrong.
    BadSignature { at_depth: usize },
    /// The walk hit [`MAX_DEPTH`] without reaching an anchor.
    ///
    /// Reported rather than treated as "no path" because the two have different remedies: this one
    /// says the chain is longer than this build follows, which is a limit of ours.
    TooDeep,
}

impl std::fmt::Display for TrustError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TrustError::Unreadable(why) => write!(formatter, "not a readable certificate: {why}"),
            TrustError::CrlUnreadable(why) => write!(formatter, "not a readable CRL: {why}"),
            TrustError::NoPath => write!(formatter, "no path to an imported trust anchor"),
            TrustError::BadSignature { at_depth } => write!(
                formatter,
                "the certificate at depth {at_depth} was not signed by the one above it"
            ),
            TrustError::TooDeep => write!(
                formatter,
                "the chain is longer than this build follows ({MAX_DEPTH} steps)"
            ),
        }
    }
}

/// How many issuing certificates the walk will follow before giving up.
///
/// Three is chosen against real chains: a leaf, an intermediate, and a root is the ordinary shape
/// of a public hierarchy, and anything longer is either a cross-signing artefact or an attack
/// designed to make a verifier burn time. A bounded walk with a stated limit is honest; an
/// unbounded one is a denial-of-service surface on a file an attacker supplies.
pub const MAX_DEPTH: usize = 3;

/// Verifies that `issuer` signed `subject`.
///
/// The trait exists so this module needs no cryptography: it states the one question the walk asks
/// and lets the caller answer it with the stack K25 confines to `verify.rs`. A test can supply a
/// verifier that answers from a table, which is what makes every branch below provable without a
/// key.
pub trait LinkVerifier {
    /// True when `issuer`'s public key verifies the signature carried on `subject`.
    ///
    /// Both arguments are DER. Returning `false` **must not** mean "could not check": an
    /// unimplemented algorithm has to be reported as a failure to anchor, because treating
    /// "unsupported" as "fine" is precisely the false green this module exists to prevent.
    fn issued(&self, issuer: &CertFacts, subject: &CertFacts) -> bool;
}

/// The chain path a signer's certificate took to an anchor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chain {
    /// The certificates from the signer upward, **excluding** the anchor.
    pub path: Vec<CertFacts>,
    /// The imported anchor the path ended at.
    pub anchor: CertFacts,
}

impl Chain {
    /// How many issuing steps were followed. A leaf signed directly by an anchor is `1`.
    pub fn depth(&self) -> usize {
        self.path.len()
    }

    /// Every certificate on the path, signer first and anchor last.
    pub fn certificates(&self) -> impl Iterator<Item = &CertFacts> {
        self.path.iter().chain(std::iter::once(&self.anchor))
    }

    /// The earliest expiry on the path, which is when the chain as a whole stops being valid.
    ///
    /// The minimum rather than the anchor's own date: a chain is only good for as long as its
    /// shortest-lived member, and reporting the root's decade-long window would overstate it.
    pub fn expires_at(&self) -> i64 {
        self.certificates()
            .map(|certificate| certificate.not_after_unix)
            .min()
            .unwrap_or(i64::MAX)
    }

    /// Whether every certificate on the path is inside its validity window at `now_unix`.
    ///
    /// Reported rather than enforced by [`build_chain`], because the two are different questions
    /// and D-SG7 has no state for "anchored but expired". A caller that ignores this is choosing to
    /// accept an expired chain, and that choice is visible here rather than hidden in the walk.
    pub fn valid_at(&self, now_unix: i64) -> bool {
        self.certificates()
            .all(|certificate| certificate.valid_at(now_unix))
    }
}

/// Build a path from `signer` to one of `anchors`, verifying every edge.
///
/// `intermediates` are the extra certificates the signature carried (a CMS `SignedData` may hold a
/// whole chain). They are candidates only: a certificate is used as an issuer solely when the one
/// below it names it as issuer *and* the edge signature verifies, so a blob full of unrelated
/// certificates cannot shorten the walk.
///
/// The search is breadth-first over depth and returns the **shortest** verified path, which is the
/// one a reader should be shown and the one that is hardest to extend with a planted certificate.
pub fn build_chain(
    signer: &CertFacts,
    intermediates: &[CertFacts],
    anchors: &[CertFacts],
    verifier: &dyn LinkVerifier,
) -> Result<Chain, TrustError> {
    // A signer that is itself an imported anchor is anchored without an issuing step. This is the
    // self-signed-root-as-signer case, and it still requires the anchor to be one the user chose.
    if let Some(anchor) = anchors.iter().find(|anchor| anchor.der == signer.der) {
        return Ok(Chain {
            path: Vec::new(),
            anchor: anchor.clone(),
        });
    }

    // Depth 1..=MAX_DEPTH. Each round extends every live path by one verified edge.
    let mut frontier: Vec<Vec<CertFacts>> = vec![vec![signer.clone()]];
    for depth in 1..=MAX_DEPTH {
        let mut next: Vec<Vec<CertFacts>> = Vec::new();
        for path in &frontier {
            let current = path.last().expect("a frontier path is never empty");
            // Candidate issuers: imported anchors first, then the signature's own certificates.
            // Anchors first is not an optimisation -- it is what makes the shortest honest path
            // be found before a longer one built from attacker-supplied intermediates.
            for candidate in anchors.iter().chain(intermediates.iter()) {
                if path.iter().any(|seen| seen.der == candidate.der) {
                    // A cycle. Following it would loop until MAX_DEPTH and then report a chain
                    // whose every edge verified, which is exactly the shape a hostile blob takes.
                    continue;
                }
                if candidate.subject != current.issuer {
                    continue;
                }
                if !candidate.is_ca {
                    // Naming a non-CA as issuer is a malformed chain, not a near miss. Refusing it
                    // here stops a leaf from vouching for another leaf.
                    continue;
                }
                if !verifier.issued(candidate, current) {
                    return Err(TrustError::BadSignature { at_depth: depth });
                }
                // `pathLenConstraint` bounds the intermediates beneath a CA. `path.len()` counts
                // the certificates already below this candidate, excluding the signer itself.
                if let Some(limit) = candidate.path_len {
                    let below = path.len().saturating_sub(1);
                    if below > usize::from(limit) {
                        continue;
                    }
                }

                let mut extended = path.clone();
                extended.push(candidate.clone());

                if anchors.iter().any(|anchor| anchor.der == candidate.der) {
                    let anchor = extended.pop().expect("just pushed");
                    return Ok(Chain {
                        path: extended,
                        anchor,
                    });
                }
                next.push(extended);
            }
        }
        if next.is_empty() {
            return Err(TrustError::NoPath);
        }
        frontier = next;
    }
    Err(TrustError::TooDeep)
}

/// Parse a set of DER certificates, keeping the ones that would not parse.
///
/// The unreadable ones are returned rather than dropped, for the same reason
/// `identity::listing_from` keeps them: an anchor that vanishes from the user's own store with no
/// explanation leaves them with nothing to act on.
pub fn parse_all(ders: &[Vec<u8>]) -> (Vec<CertFacts>, Vec<TrustError>) {
    let mut parsed = Vec::with_capacity(ders.len());
    let mut failed = Vec::new();
    for der in ders {
        match CertFacts::parse(der) {
            Ok(facts) => parsed.push(facts),
            Err(error) => failed.push(error),
        }
    }
    (parsed, failed)
}

/// Deduplicate a set of certificates by their DER, preserving the first-seen order.
///
/// Importing the same anchor twice is an ordinary thing to do, and a duplicate would otherwise let
/// the walk consider two identical candidates at one depth.
pub fn dedup(certificates: Vec<CertFacts>) -> Vec<CertFacts> {
    let mut seen = HashSet::new();
    certificates
        .into_iter()
        .filter(|certificate| seen.insert(certificate.der.clone()))
        .collect()
}

// ---------------------------------------------------------------------------------------------
// Revocation: a CRL reduced to the one fact `revocation::decide` asks for
// ---------------------------------------------------------------------------------------------

/// Turn one DER CRL into `RevocationEvidence` for `certificate`, at `now_unix`.
///
/// This is the parser `revocation.rs` was written to be fed by: it produces the
/// [`super::revocation::RevocationEvidence`] the deciding function already handles, and decides
/// nothing itself. Keeping the split means the parser can be wrong about a *field* without being
/// wrong about the *policy*, and the policy is the half that has already been proved.
///
/// Four outcomes, and every one of them is a claim about this CRL and this certificate:
///
/// * `Revoked` -- the certificate's serial is on the list;
/// * `NotRevoked { produced_at_unix }` -- the list covers this issuer and does not name it;
/// * `Expired` -- the list's `nextUpdate` has passed, so it asserts nothing about today;
/// * `DoesNotApply` -- the list is for a different issuer, or covers only a subset of the serial
///   space that excludes this certificate (an indirect or partitioned CRL).
///
/// **The issuer check is not optional.** A CRL signed by an unrelated CA that happens not to name
/// this serial is not evidence of anything, and treating it as `NotRevoked` would let any document
/// ship a fresh empty CRL for a CA it made up and clear itself.
pub fn crl_evidence(
    crl_der: &[u8],
    certificate: &CertFacts,
    now_unix: i64,
) -> Result<super::revocation::RevocationEvidence, TrustError> {
    use x509_cert::crl::CertificateList;

    // Annotated because two generations of `x509-cert` are in the graph (K24): without the type,
    // inference cannot tell the `0.3` `CertificateList` from a `0.2` one this file must not use.
    let list: x509_cert::crl::CertificateList = CertificateList::from_der(crl_der)
        .map_err(|error| TrustError::CrlUnreadable(error.to_string()))?;
    let tbs = &list.tbs_cert_list;

    // (1) Does this list speak about this certificate's issuer? `Name` compares as the DER of the
    // whole RDNSequence, which is the identity RFC 5280 defines -- comparing rendered strings would
    // treat two differently-encoded but equal names as different issuers.
    if tbs.issuer.to_string() != certificate.issuer {
        return Ok(super::revocation::RevocationEvidence::DoesNotApply);
    }

    // (2) An indirect or partitioned CRL covers only part of the serial space. `TbsCertList` has no
    // typed extension accessor (unlike `TbsCertificate`), so the OID is matched directly. A CRL that
    // restricts its scope is reported as not applying rather than assumed to cover everything,
    // because assuming would turn a partial list into a clean bill of health for the rest.
    const OID_ISSUING_DISTRIBUTION_POINT: x509_cert::der::asn1::ObjectIdentifier =
        x509_cert::der::asn1::ObjectIdentifier::new_unwrap("2.5.29.28");
    if tbs.crl_extensions.as_deref().is_some_and(|extensions| {
        extensions
            .iter()
            .any(|extension| extension.extn_id == OID_ISSUING_DISTRIBUTION_POINT)
    }) {
        return Ok(super::revocation::RevocationEvidence::DoesNotApply);
    }

    // (3) Is the list still current? A CRL past its `nextUpdate` asserts nothing about today, so
    // it can only support `Expired`. A hit below still wins even on an expired list: a revoked
    // certificate must not be cleared by staleness.
    if let Some(revoked) = &tbs.revoked_certificates {
        let named = revoked.iter().any(|entry| {
            hex::encode_upper(entry.serial_number.as_bytes()) == certificate.serial_hex
        });
        if named {
            return Ok(super::revocation::RevocationEvidence::Revoked);
        }
    }

    let produced_at_unix = tbs.this_update.to_unix_duration().as_secs() as i64;
    match tbs.next_update {
        // `next_update` past `now` means the list is current.
        Some(next) if next.to_unix_duration().as_secs() as i64 >= now_unix => {
            Ok(super::revocation::RevocationEvidence::NotRevoked { produced_at_unix })
        }
        Some(_) => Ok(super::revocation::RevocationEvidence::Expired),
        // A CRL with no `nextUpdate` never expires by its own statement. RFC 5280 makes it optional,
        // and treating "no expiry stated" as "expired" would discard every such list.
        None => Ok(super::revocation::RevocationEvidence::NotRevoked { produced_at_unix }),
    }
}

/// Reduce a set of CRLs to the single best piece of evidence for `certificate`.
///
/// "Best" is: revoked outranks not-revoked, and a current list outranks an expired one. Ordering
/// lives here rather than in `revocation::decide` because `decide` is already the single place the
/// *policy* is written, and a second policy inside a loop would be the copy that drifts.
pub fn best_crl_evidence(
    crls: &[Vec<u8>],
    certificate: &CertFacts,
    now_unix: i64,
) -> Option<super::revocation::RevocationEvidence> {
    use super::revocation::RevocationEvidence;

    // Ordered so that a larger rank wins: a revocation outranks any clean bill, and a current list
    // outranks an expired one. Writing the order as a number keeps the fold one line and puts the
    // policy in exactly one place, rather than in the shape of a `match`.
    fn rank(evidence: RevocationEvidence) -> u8 {
        match evidence {
            RevocationEvidence::Revoked => 3,
            RevocationEvidence::NotRevoked { .. } => 2,
            RevocationEvidence::DoesNotApply => 1,
            RevocationEvidence::Expired => 0,
        }
    }

    crls.iter()
        // A CRL that will not parse is skipped rather than aborting: one unreadable entry must not
        // hide a usable one beside it.
        .filter_map(|crl| crl_evidence(crl, certificate, now_unix).ok())
        .fold(
            None,
            |best: Option<RevocationEvidence>, evidence| match best {
                Some(previous) if rank(previous) >= rank(evidence) => Some(previous),
                _ => Some(evidence),
            },
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    /// A verifier that answers from a set of `(issuer, subject)` pairs.
    ///
    /// This is not a mock of the cryptography: it is the same contract the production verifier
    /// satisfies, and the walk's correctness does not depend on which implementation answers. The
    /// *real* verifier is proved separately, against real keys, in `verify.rs` and in the
    /// end-to-end test.
    struct Table(Vec<(String, String)>);

    impl LinkVerifier for Table {
        fn issued(&self, issuer: &CertFacts, subject: &CertFacts) -> bool {
            self.0
                .iter()
                .any(|(from, to)| *from == issuer.subject && *to == subject.subject)
        }
    }

    /// Everything verifies, which makes the walk's shape observable without naming keys.
    struct TrustAll;

    impl LinkVerifier for TrustAll {
        fn issued(&self, _issuer: &CertFacts, _subject: &CertFacts) -> bool {
            true
        }
    }

    fn cert(subject: &str, issuer: &str, is_ca: bool) -> CertFacts {
        CertFacts {
            der: format!("der:{subject}:{issuer}").into_bytes(),
            issuer: issuer.to_string(),
            subject: subject.to_string(),
            serial_hex: "01".to_string(),
            is_ca,
            path_len: None,
            not_before_unix: 0,
            not_after_unix: i64::MAX,
        }
    }

    #[test]
    fn a_self_signed_signer_with_no_anchor_has_no_path() {
        // The honest negative the whole unit is built around. This must not be `Ok`.
        let signer = cert("CN=Self", "CN=Self", false);
        assert_eq!(
            build_chain(&signer, &[], &[], &TrustAll),
            Err(TrustError::NoPath),
            "a self-signed certificate is not its own anchor; only the user importing it makes it one"
        );
    }

    #[test]
    fn a_signer_that_is_an_imported_anchor_is_anchored() {
        let signer = cert("CN=Self", "CN=Self", false);
        let chain =
            build_chain(&signer, &[], std::slice::from_ref(&signer), &TrustAll).expect("anchored");
        assert_eq!(chain.depth(), 0, "no issuing step was needed");
        assert_eq!(chain.anchor.der, signer.der);
    }

    #[test]
    fn a_leaf_signed_by_an_imported_ca_is_anchored_at_depth_one() {
        let signer = cert("CN=Leaf", "CN=CA", false);
        let anchor = cert("CN=CA", "CN=CA", true);
        let chain = build_chain(&signer, &[], &[anchor], &TrustAll).expect("anchored");
        assert_eq!(chain.depth(), 1);
        assert_eq!(chain.path[0].subject, "CN=Leaf");
    }

    #[test]
    fn an_unrelated_imported_anchor_does_not_anchor_the_signer() {
        // Both are real certificates. Only the *edge* matters, and it is not there.
        let signer = cert("CN=Leaf", "CN=CA", false);
        let unrelated = cert("CN=Some Other CA", "CN=Some Other CA", true);
        assert_eq!(
            build_chain(&signer, &[], &[unrelated], &TrustAll),
            Err(TrustError::NoPath),
            "importing an anchor the signer did not chain to must not anchor it"
        );
    }

    #[test]
    fn a_name_match_without_a_verified_edge_is_refused() {
        // The failure this test exists for: the issuer *name* matches, so a walk that compared
        // strings would call this anchored. The verifier says the signature is wrong.
        let signer = cert("CN=Leaf", "CN=CA", false);
        let impostor = cert("CN=CA", "CN=CA", true);
        let table = Table(Vec::new());
        assert_eq!(
            build_chain(&signer, &[], &[impostor], &table),
            Err(TrustError::BadSignature { at_depth: 1 }),
            "a matching subject name is not a signature"
        );
    }

    #[test]
    fn a_chain_through_an_intermediate_reaches_the_anchor() {
        let signer = cert("CN=Leaf", "CN=Intermediate", false);
        let intermediate = cert("CN=Intermediate", "CN=Root", true);
        let anchor = cert("CN=Root", "CN=Root", true);
        let chain = build_chain(&signer, &[intermediate], &[anchor], &TrustAll).expect("anchored");
        assert_eq!(chain.depth(), 2);
        let subjects: Vec<&str> = chain
            .certificates()
            .map(|certificate| certificate.subject.as_str())
            .collect();
        assert_eq!(
            subjects,
            vec!["CN=Leaf", "CN=Intermediate", "CN=Root"],
            "the path must be signer-first and anchor-last"
        );
    }

    #[test]
    fn a_non_ca_cannot_issue_a_certificate() {
        let signer = cert("CN=Leaf", "CN=Other Leaf", false);
        let other_leaf = cert("CN=Other Leaf", "CN=Root", false);
        let anchor = cert("CN=Root", "CN=Root", true);
        assert_eq!(
            build_chain(&signer, &[other_leaf], &[anchor], &TrustAll),
            Err(TrustError::NoPath),
            "`CA:FALSE` means it may not sign certificates, so this chain cannot be built"
        );
    }

    #[test]
    fn a_ca_without_basic_constraints_is_still_an_issuer_when_self_signed() {
        // A v1 root predates `BasicConstraints`. Refusing it would refuse the oldest anchors.
        let facts = CertFacts {
            is_ca: true,
            ..cert("CN=Old Root", "CN=Old Root", false)
        };
        assert!(facts.is_ca);
    }

    #[test]
    fn a_path_length_constraint_blocks_a_chain_that_is_too_long() {
        let signer = cert("CN=Leaf", "CN=Intermediate", false);
        let intermediate = CertFacts {
            path_len: Some(0),
            ..cert("CN=Intermediate", "CN=Root", true)
        };
        let anchor = cert("CN=Root", "CN=Root", true);
        // `pathLenConstraint = 0` on the intermediate forbids any CA beneath it; the leaf is not a
        // CA, so one certificate below is allowed and this chain is legal.
        assert!(
            build_chain(
                &signer,
                std::slice::from_ref(&intermediate),
                std::slice::from_ref(&anchor),
                &TrustAll
            )
            .is_ok(),
            "one non-CA below a pathLen=0 CA is permitted"
        );

        // Two levels below it is not.
        let deeper = cert("CN=Leaf", "CN=Deep Intermediate", false);
        let deep_intermediate = cert("CN=Deep Intermediate", "CN=Intermediate", true);
        assert_eq!(
            build_chain(
                &deeper,
                &[deep_intermediate, intermediate],
                &[anchor],
                &TrustAll
            ),
            Err(TrustError::NoPath),
            "pathLen=0 must stop a second CA appearing beneath it"
        );
    }

    #[test]
    fn a_cycle_terminates_instead_of_looping() {
        // Two CAs that each name the other as issuer. A walk without a seen-set spins until the
        // depth cap and can report a chain whose edges all verified.
        let signer = cert("CN=Leaf", "CN=A", false);
        let a = cert("CN=A", "CN=B", true);
        let b = cert("CN=B", "CN=A", true);
        let error = build_chain(&signer, &[a, b], &[], &TrustAll).expect_err("no anchor");
        // `NoPath`, not `TooDeep`: the seen-set refuses to re-enter a certificate, so the frontier
        // empties at depth 2 rather than spinning to the cap. Cutting a cycle early is the stronger
        // result -- it costs nothing and cannot be used to burn depth -- so this pins it.
        assert_eq!(
            error,
            TrustError::NoPath,
            "a cycle must terminate without resolving into a chain"
        );
    }

    #[test]
    fn the_shortest_verified_path_wins() {
        // Both routes reach the same anchor; the direct one must be reported.
        let signer = cert("CN=Leaf", "CN=Root", false);
        let detour = cert("CN=Decoy", "CN=Root", true);
        let anchor = cert("CN=Root", "CN=Root", true);
        let chain = build_chain(&signer, &[detour], &[anchor], &TrustAll).expect("anchored");
        assert_eq!(chain.depth(), 1, "the direct edge is the one to report");
    }

    #[test]
    fn a_chain_too_long_for_the_cap_says_so() {
        let mut intermediates = Vec::new();
        let mut subject = "CN=L0".to_string();
        for depth in 1..=MAX_DEPTH + 1 {
            let issuer = format!("CN=L{depth}");
            intermediates.push(cert(&subject, &issuer, true));
            subject = issuer;
        }
        let signer = cert("CN=Leaf", "CN=L1", false);
        assert_eq!(
            build_chain(&signer, &intermediates, &[], &TrustAll),
            Err(TrustError::TooDeep),
            "hitting the cap is our limit, and it must be named as such rather than as `no path`"
        );
    }

    #[test]
    fn the_chain_expires_with_its_shortest_lived_certificate() {
        let signer = CertFacts {
            not_after_unix: 100,
            ..cert("CN=Leaf", "CN=Root", false)
        };
        let anchor = CertFacts {
            not_after_unix: 10_000,
            ..cert("CN=Root", "CN=Root", true)
        };
        let chain = build_chain(&signer, &[], &[anchor], &TrustAll).expect("anchored");
        assert_eq!(
            chain.expires_at(),
            100,
            "a chain is only good while its shortest-lived member is"
        );
        assert!(chain.valid_at(50));
        assert!(!chain.valid_at(101));
    }

    #[test]
    fn duplicates_are_collapsed_without_reordering() {
        let a = cert("CN=A", "CN=A", true);
        let b = cert("CN=B", "CN=B", true);
        let deduped = dedup(vec![a.clone(), b.clone(), a.clone()]);
        assert_eq!(deduped, vec![a, b]);
    }
}

#[cfg(test)]
mod fixture_tests {
    use super::*;
    use crate::editor::signature::verify::CertificateLinkVerifier;

    fn fixture(name: &str) -> Vec<u8> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/signing/trust")
            .join(name);
        std::fs::read(&path).unwrap_or_else(|error| panic!("fixture {name}: {error}"))
    }

    #[test]
    fn the_real_verifier_accepts_a_ca_that_signed_a_leaf() {
        let ca = CertFacts::parse(&fixture("ca.der")).expect("CA parses");
        let leaf = CertFacts::parse(&fixture("leaf.der")).expect("leaf parses");
        assert!(
            CertificateLinkVerifier.issued(&ca, &leaf),
            "openssl signed this leaf with this CA; a false here means the edge check is broken"
        );
    }

    #[test]
    fn the_real_verifier_rejects_a_leaf_signed_by_a_different_ca() {
        let other = CertFacts::parse(&fixture("other-ca.der")).expect("other CA parses");
        let leaf = CertFacts::parse(&fixture("leaf.der")).expect("leaf parses");
        // The names differ, so the walk would skip it anyway; this pins the verifier itself.
        assert!(
            !CertificateLinkVerifier.issued(&other, &leaf),
            "an unrelated CA's key did not sign this leaf"
        );
    }

    #[test]
    fn the_real_verifier_rejects_a_tampered_leaf_body() {
        // Flip one byte in the leaf's signed content. The signature must stop verifying: this is
        // the difference between checking a signature and comparing two names.
        let ca = CertFacts::parse(&fixture("ca.der")).expect("CA parses");
        let mut leaf_der = fixture("leaf.der");
        // A byte well inside the TBS (the subject CN), never in the outer header.
        let at = leaf_der.len() / 2;
        leaf_der[at] ^= 0x01;
        let leaf = CertFacts {
            der: leaf_der,
            ..CertFacts::parse(&fixture("leaf.der")).expect("leaf parses")
        };
        assert!(
            !CertificateLinkVerifier.issued(&ca, &leaf),
            "one flipped byte must break the edge; passing here means the body is not covered"
        );
    }

    #[test]
    fn a_real_leaf_chains_to_its_real_anchor_at_depth_one() {
        let signer = CertFacts::parse(&fixture("leaf.der")).expect("leaf parses");
        let anchor = CertFacts::parse(&fixture("ca.der")).expect("CA parses");
        let chain = build_chain(&signer, &[], &[anchor], &CertificateLinkVerifier)
            .expect("the real leaf chains to the real CA");
        assert_eq!(chain.depth(), 1);
        assert_eq!(chain.path[0].subject, signer.subject);
    }

    #[test]
    fn a_real_leaf_does_not_chain_to_an_unrelated_real_anchor() {
        // The honest negative, with real certificates rather than a table.
        let signer = CertFacts::parse(&fixture("leaf.der")).expect("leaf parses");
        let unrelated = CertFacts::parse(&fixture("other-ca.der")).expect("other CA parses");
        assert_eq!(
            build_chain(&signer, &[], &[unrelated], &CertificateLinkVerifier),
            Err(TrustError::NoPath),
            "a real anchor that did not issue this leaf must not anchor it"
        );
    }

    #[test]
    fn a_real_crl_that_does_not_name_the_leaf_says_not_revoked() {
        use crate::editor::signature::revocation::RevocationEvidence;
        let leaf = CertFacts::parse(&fixture("leaf.der")).expect("leaf parses");
        // `now` before the printed `nextUpdate` (2036), so the list is current.
        let now = 1_800_000_000;
        assert_eq!(
            crl_evidence(&fixture("crl-empty.der"), &leaf, now),
            Ok(RevocationEvidence::NotRevoked {
                // `thisUpdate` of the generated fixture. Asserted rather than ignored: if the
                // parser stopped reading that field the value would drift and this would fail.
                produced_at_unix: 1_794_328_781
            })
        );
    }

    #[test]
    fn a_real_crl_that_names_the_leaf_says_revoked() {
        use crate::editor::signature::revocation::RevocationEvidence;
        let leaf = CertFacts::parse(&fixture("leaf.der")).expect("leaf parses");
        assert_eq!(
            crl_evidence(&fixture("crl-revoked.der"), &leaf, 1_800_000_000),
            Ok(RevocationEvidence::Revoked),
            "this CRL names the leaf's serial; reporting anything else clears a revoked certificate"
        );
    }

    #[test]
    fn a_crl_for_another_issuer_does_not_apply() {
        use crate::editor::signature::revocation::RevocationEvidence;
        let unrelated = CertFacts::parse(&fixture("other-ca.der")).expect("other CA parses");
        assert_eq!(
            crl_evidence(&fixture("crl-empty.der"), &unrelated, 1_800_000_000),
            Ok(RevocationEvidence::DoesNotApply),
            "a list for a different issuer is not evidence about this certificate"
        );
    }

    #[test]
    fn a_crl_past_its_next_update_is_expired() {
        use crate::editor::signature::revocation::RevocationEvidence;
        let leaf = CertFacts::parse(&fixture("leaf.der")).expect("leaf parses");
        // The fixture's `nextUpdate` is 2036-11-07.
        assert_eq!(
            crl_evidence(&fixture("crl-empty.der"), &leaf, 2_200_000_000),
            Ok(RevocationEvidence::Expired),
            "a stale clean list must not be read as a current clean bill"
        );
        // ... but a revoked entry survives staleness.
        assert_eq!(
            crl_evidence(&fixture("crl-revoked.der"), &leaf, 2_200_000_000),
            Ok(RevocationEvidence::Revoked),
            "expiry must not clear a revocation the list records"
        );
    }

    #[test]
    fn a_garbage_crl_is_unreadable_rather_than_silently_clean() {
        let leaf = CertFacts::parse(&fixture("leaf.der")).expect("leaf parses");
        assert!(matches!(
            crl_evidence(b"not a crl at all", &leaf, 1_800_000_000),
            Err(TrustError::CrlUnreadable(_))
        ));
    }
}
