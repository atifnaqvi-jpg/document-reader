//! The trust store's persistence, and the one function that turns a signed file into a badge.
//!
//! [`assess`] is deliberately the only place the inputs `badge::decide` needs are gathered. Before
//! it existed, `src/commands/mod.rs` passed `anchored: false` and `revocation::decide(None, None, 0)`
//! as constants, which made two of D-SG7's four states unreachable in production however good the
//! deciding logic was. A single seam means the command and the end-to-end test call the same code,
//! so a passing test is evidence about the shipped path rather than about a copy of it.
//!
//! **Nothing here touches the network.** D-SG4's rule is one sentence and this file is where it is
//! kept: anchors come from the user's own store, revocation data comes from the document or from a
//! list the user imported, and a CRL this build has never seen is `unknown` rather than fetched.
//!
//! Base64 in the database rather than a BLOB column: every other table here stores TEXT, and one
//! BLOB column would need its own row-mapping path for no gain. The encoding is not a security
//! measure and is not treated as one.

use rusqlite::{params, Connection, OptionalExtension};

use crate::model::AppError;

use super::badge::{self, Badge, Integrity, SignatureEvidence};
use super::revocation::{self, Revocation};
use super::trust::{self, CertFacts};
use super::verify::{self, CertificateLinkVerifier, SealedReading};

/// One anchor as the user sees it in a list.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AnchorRow {
    /// Base64 of the DER, which is also the row's identity.
    pub der_base64: String,
    pub subject: String,
    pub not_after: i64,
    pub added_at: i64,
}

/// What [`assess`] concluded, plus the evidence a UI needs to explain it.
///
/// The badge alone is not enough to render honestly: D-SG7 requires that `unknown` be a distinct
/// state, and "unknown because nothing is imported" reads very differently from "unknown because a
/// CRL is stale". `reason` is what lets the UI say which.
#[derive(Clone, Debug, PartialEq)]
pub struct Assessment {
    pub badge: Badge,
    pub integrity: Integrity,
    /// True only when a verified path reached an imported anchor.
    pub anchored: bool,
    /// The chain that was built, when one was.
    pub chain_depth: Option<usize>,
    pub revocation: Revocation,
    /// The declared DocMDP level, when this is a certification signature.
    pub certification_level: Option<u8>,
    /// The badge's own explanatory lines, from `badge::explain`.
    ///
    /// Carried alongside the badge so the command does not have to rebuild the evidence it just
    /// decomposed. The evidence is rebuilt here rather than returned, which keeps `SignatureEvidence`
    /// out of this module's public surface.
    pub lines: Vec<&'static str>,
    /// A short, user-facing explanation of the *least* certain input.
    ///
    /// Empty when everything is known and good. Never a code: this is rendered directly.
    pub reason: String,
}

/// Store one anchor the user chose.
///
/// The DER is parsed before it is stored, so an unreadable certificate is refused at the boundary
/// with a reason the user can act on, rather than being saved and silently failing to anchor
/// anything later.
pub fn add_anchor(
    connection: &Connection,
    der: &[u8],
    now_unix: i64,
) -> Result<AnchorRow, AppError> {
    let facts = CertFacts::parse(der)
        .map_err(|error| AppError::CertificateUnreadable(error.to_string()))?;
    let encoded = base64_encode(der);
    connection.execute(
        "INSERT INTO TrustAnchor (der, subject, not_after, added_at) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(der) DO NOTHING",
        params![encoded, facts.subject, facts.not_after_unix, now_unix],
    )?;
    Ok(AnchorRow {
        der_base64: encoded,
        subject: facts.subject,
        not_after: facts.not_after_unix,
        added_at: now_unix,
    })
}

/// Every anchor the user has imported, newest first.
pub fn list_anchors(connection: &Connection) -> Result<Vec<AnchorRow>, AppError> {
    let mut statement = connection.prepare(
        "SELECT der, subject, not_after, added_at FROM TrustAnchor ORDER BY added_at DESC, subject",
    )?;
    let rows = statement.query_map([], |row| {
        Ok(AnchorRow {
            der_base64: row.get(0)?,
            subject: row.get(1)?,
            not_after: row.get(2)?,
            added_at: row.get(3)?,
        })
    })?;
    let mut anchors = Vec::new();
    for row in rows {
        anchors.push(row?);
    }
    Ok(anchors)
}

/// Remove one anchor by its base64 DER. Returns whether a row was actually deleted.
pub fn remove_anchor(connection: &Connection, der_base64: &str) -> Result<bool, AppError> {
    let removed = connection.execute(
        "DELETE FROM TrustAnchor WHERE der = ?1",
        params![der_base64],
    )?;
    Ok(removed > 0)
}

/// Every anchor's parsed certificates, for the path walk.
///
/// Unreadable rows are skipped rather than failing the whole assessment:
/// [`store_anchor_count`] still counts them, so a store that has an unreadable entry does not
/// silently look empty. A row that will not parse is a store inconsistency, not a user action.
fn anchor_facts(connection: &Connection) -> Result<Vec<CertFacts>, AppError> {
    let mut statement = connection.prepare("SELECT der FROM TrustAnchor")?;
    let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
    let mut anchors = Vec::new();
    for row in rows {
        let encoded: String = row?;
        let Some(der) = base64_decode(&encoded) else {
            continue;
        };
        if let Ok(facts) = CertFacts::parse(&der) {
            anchors.push(facts);
        }
    }
    Ok(trust::dedup(anchors))
}

/// How many anchors are stored, including ones that would not parse.
fn store_anchor_count(connection: &Connection) -> Result<i64, AppError> {
    Ok(connection.query_row("SELECT count(*) FROM TrustAnchor", [], |row| row.get(0))?)
}

/// Store one CRL the user imported.
pub fn add_crl(connection: &Connection, der: &[u8], now_unix: i64) -> Result<(), AppError> {
    use x509_cert::der::Decode;

    let list: x509_cert::crl::CertificateList = x509_cert::crl::CertificateList::from_der(der)
        .map_err(|error| AppError::CertificateUnreadable(format!("not a CRL: {error}")))?;
    let tbs = &list.tbs_cert_list;
    connection.execute(
        "INSERT INTO TrustCrl (der, issuer, this_update, next_update, added_at)
         VALUES (?1, ?2, ?3, ?4, ?5) ON CONFLICT(der) DO NOTHING",
        params![
            base64_encode(der),
            tbs.issuer.to_string(),
            tbs.this_update.to_unix_duration().as_secs() as i64,
            tbs.next_update
                .map(|time| time.to_unix_duration().as_secs() as i64),
            now_unix,
        ],
    )?;
    Ok(())
}

/// Every imported CRL's DER.
fn imported_crls(connection: &Connection) -> Result<Vec<Vec<u8>>, AppError> {
    let mut statement = connection.prepare("SELECT der FROM TrustCrl")?;
    let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
    let mut crls = Vec::new();
    for row in rows {
        let encoded: String = row?;
        if let Some(der) = base64_decode(&encoded) {
            crls.push(der);
        }
    }
    Ok(crls)
}

/// Assess one signed document against the user's trust store.
///
/// The order is fixed and each step is a refusal rather than a fallback:
///
/// 1. `inspect` establishes integrity and lifts the signer, chain and CRLs out of the file.
/// 2. The path walk establishes `anchored` -- or does not, and says why.
/// 3. Revocation evidence is gathered from the document and from imported CRLs, and `revocation`
///    decides it, so the policy stays in the one place it is tested.
/// 4. `badge::decide` turns those three into one of D-SG7's four states.
///
/// **`anchored` is never inferred from a name.** If there is no signer certificate, or no anchor
/// that issues it, `anchored` is `false` and the badged state for an intact signature is `unknown`.
/// That is the answer D-SG4 requires, and it is what the end-to-end test asserts.
pub fn assess(
    connection: &Connection,
    written: &[u8],
    now_unix: i64,
) -> Result<Option<Assessment>, AppError> {
    let Some(reading) = verify::inspect(written)? else {
        return Ok(None);
    };

    // (2) The path. A signing certificate that is absent or unreadable anchors nothing: the
    // signature may still be intact, but "intact" and "trusted" are different claims.
    let anchors = anchor_facts(connection)?;
    let store_anchor_count = store_anchor_count(connection)?;
    let mut anchored = false;
    let mut chain_depth = None;
    let mut reason = String::new();

    match reading.signer.as_deref() {
        None => {
            reason = "the signature does not carry its signing certificate".to_string();
        }
        Some(signer_der) => match CertFacts::parse(signer_der) {
            Err(error) => {
                reason = format!("the signing certificate could not be read ({error})");
            }
            Ok(signer) => {
                // The document's own certificates are candidates for issuers. They are not
                // trusted -- the walk still has to reach a *user-imported* anchor -- but a chain
                // that shipped with the signature is exactly the material a path needs.
                let (embedded, _unreadable) = trust::parse_all(&reading.embedded_chain);
                match trust::build_chain(&signer, &embedded, &anchors, &CertificateLinkVerifier) {
                    Ok(chain) => {
                        anchored = true;
                        chain_depth = Some(chain.depth());
                        if !chain.valid_at(now_unix) {
                            // Reported, not enforced: D-SG7 has no "anchored but expired" state,
                            // and inventing one here would be a fifth badge.
                            reason =
                                "the chain is anchored, but a certificate has expired".to_string();
                        }
                    }
                    Err(trust::TrustError::NoPath) if store_anchor_count == 0 => {
                        reason = "no trust anchors are imported, so this signature cannot be \
                                  confirmed"
                            .to_string();
                    }
                    Err(trust::TrustError::NoPath) if anchors.is_empty() => {
                        // Rows exist but none parsed. Saying "nothing is imported" here would be
                        // false, and it would hide a broken store behind a plausible sentence.
                        reason = "the import store has entries but none could be read, so this \
                                  signature cannot be confirmed"
                            .to_string();
                    }
                    Err(trust::TrustError::NoPath) => {
                        reason = "the signing certificate does not chain to any imported anchor"
                            .to_string();
                    }
                    Err(error) => {
                        reason = error.to_string();
                    }
                }
            }
        },
    }

    // (3) Revocation. A revoked anchor is worse than an unanchored one, so this runs even when
    // anchoring failed: a revocation the reader can see is information regardless.
    let revocation = revocation_for(connection, &reading, now_unix)?;

    // (4) The decision, unchanged from where it is already proved. `badge::decide` takes the whole
    // evidence struct rather than four loose arguments, so adding an input later is a compile error
    // here rather than a silently missing field.
    let evidence = SignatureEvidence {
        integrity: reading.integrity,
        later_revision: reading.later_revision,
        anchored,
        revocation,
        certification_level: reading.certification_level,
    };
    let detail = badge::explain(&evidence);

    if anchored && reason.is_empty() && revocation.supports_valid() {
        // Nothing to explain: the badge says it.
        reason.clear();
    }

    Ok(Some(Assessment {
        badge: detail.badge,
        integrity: reading.integrity,
        anchored,
        chain_depth,
        revocation,
        certification_level: reading.certification_level,
        lines: detail.lines(),
        reason,
    }))
}

/// Gather revocation evidence for the signer and let `revocation` decide.
fn revocation_for(
    connection: &Connection,
    reading: &SealedReading,
    now_unix: i64,
) -> Result<Revocation, AppError> {
    // The signer is the certificate whose serial a CRL would name. Without it there is nothing to
    // look up, and `decide(None, ...)` is the honest call -- an uncheckable certificate is not a
    // clean one.
    let Some(signer_der) = reading.signer.as_deref() else {
        return Ok(revocation::decide(None, None, now_unix));
    };
    let Ok(signer) = CertFacts::parse(signer_der) else {
        return Ok(revocation::decide(None, None, now_unix));
    };

    // Document data first: it travels with the file and needs no import.
    let document_evidence = trust::best_crl_evidence(&reading.embedded_crls, &signer, now_unix);

    // Then the lists the user imported. A CRL a user imported is worth more than one the document
    // shipped, which is `decide`'s own distinction and the reason the two are passed separately.
    let imported = imported_crls(connection)?;
    let imported_evidence = trust::best_crl_evidence(&imported, &signer, now_unix);

    Ok(revocation::decide(
        document_evidence,
        imported_evidence,
        now_unix,
    ))
}

/// Minimal standard base64, so the store needs no crate for it.
///
/// Written out rather than pulled in because the tree has no base64 dependency and adding one for
/// two hundred lines of arithmetic would be the larger change. The alphabet is the standard one and
/// padding is preserved, so the value is also readable with any external tool.
mod base64 {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    pub fn encode(input: &[u8]) -> String {
        let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
        for chunk in input.chunks(3) {
            let byte = [
                chunk[0],
                *chunk.get(1).unwrap_or(&0),
                *chunk.get(2).unwrap_or(&0),
            ];
            let triple =
                (u32::from(byte[0]) << 16) | (u32::from(byte[1]) << 8) | u32::from(byte[2]);
            for position in 0..4 {
                if position <= chunk.len() {
                    let index = (triple >> (18 - position * 6)) & 0x3F;
                    out.push(ALPHABET[index as usize] as char);
                } else {
                    out.push('=');
                }
            }
        }
        out
    }

    pub fn decode(input: &str) -> Option<Vec<u8>> {
        let mut values = Vec::with_capacity(input.len());
        for byte in input.bytes() {
            match byte {
                b'A'..=b'Z' => values.push(byte - b'A'),
                b'a'..=b'z' => values.push(byte - b'a' + 26),
                b'0'..=b'9' => values.push(byte - b'0' + 52),
                b'+' => values.push(62),
                b'/' => values.push(63),
                b'\n' | b'\r' => continue,
                // Padding is counted from the tail below; a `=` anywhere else is malformed.
                b'=' => break,
                _ => return None,
            }
        }
        // Each `=` in the final quantum stands for a byte that was never there. Emitting three
        // bytes regardless would append garbage to every decoded value whose input was padded,
        // which is every DER blob whose length is not a multiple of three.
        let padding = input.bytes().rev().take_while(|byte| *byte == b'=').count();
        if padding > 2 {
            return None;
        }
        // The *total* quantum length includes the padding, so the multiple-of-four check has to
        // count it; the values vector stops at the first `=`.
        if (values.len() + padding) % 4 != 0 {
            return None;
        }
        while values.len() % 4 != 0 {
            values.push(0);
        }
        let mut out = Vec::with_capacity(values.len() / 4 * 3);
        let quanta = values.len() / 4;
        for (index, chunk) in values.chunks(4).enumerate() {
            let triple = (u32::from(chunk[0]) << 18)
                | (u32::from(chunk[1]) << 12)
                | (u32::from(chunk[2]) << 6)
                | u32::from(chunk[3]);
            let keep = if index + 1 == quanta { 3 - padding } else { 3 };
            for byte in 0..keep {
                out.push((triple >> (16 - byte * 8)) as u8);
            }
        }
        Some(out)
    }
}

use base64::{decode as base64_decode, encode as base64_encode};

/// Whether one anchor is present, by its base64 DER. Used by tests and the UI.
pub fn has_anchor(connection: &Connection, der_base64: &str) -> Result<bool, AppError> {
    let found: Option<i64> = connection
        .query_row(
            "SELECT 1 FROM TrustAnchor WHERE der = ?1",
            params![der_base64],
            |row| row.get(0),
        )
        .optional()?;
    Ok(found.is_some())
}
