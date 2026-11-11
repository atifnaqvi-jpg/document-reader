/**
 * P3.2 SCOPE-C: the trust management panel -- what the user has trusted, and how to stop.
 *
 * The backend gained `list_trust_anchors`, `forget_trust_anchor`, `list_trust_crls` and
 * `forget_trust_crl` across two units, and until this component existed none of them had a caller.
 * Commands reachable only from tests are the same defect as a guard that never runs: the store was
 * still, in practice, append-only and unauditable, however well the store functions were tested.
 *
 * **Why removal is given equal footing with import.** D-SG4 makes trust local configuration, and
 * configuration you cannot undo is not configuration. Two specific failures follow from an
 * append-only store, and both are silent:
 *
 * 1. An anchor imported by mistake -- a test root, another organisation's CA, one since
 *    compromised -- keeps contributing to `valid` forever.
 * 2. A revocation list naming the signer keeps demoting the badge forever. The mirror case, a stale
 *    *clean* list, is self-limiting: past `nextUpdate` the backend stops counting it. The
 *    suppressing direction has no such escape, which is why forgetting has to exist.
 *
 * **Staleness is shown, not inferred away.** A CRL past its `nextUpdate` is no longer evidence, and
 * the badge silently changes meaning when that date passes. The panel therefore says `expired`
 * rather than leaving the row looking authoritative. This is the same honesty rule as the badge: a
 * state the user cannot see is a state they will misread.
 *
 * **Dates are rendered, never computed into a claim.** `nextUpdate: null` is `none stated`, which
 * means the list never expires by its own terms -- not "unknown" and not "expired". Folding those
 * three into one blank cell would make the panel disagree with `revocation::decide`.
 */

import { useCallback, useEffect, useState } from "react";
import type { TrustAnchorView, TrustCrlView } from "../api/types";
import {
  forgetTrustAnchor,
  forgetTrustCrl,
  listTrustAnchors,
  listTrustCrls,
  toAppError,
} from "../api/invoke";

type Loaded = { anchors: TrustAnchorView[]; crls: TrustCrlView[] };

type State =
  | { kind: "loading" }
  | { kind: "ready"; loaded: Loaded }
  | { kind: "error"; message: string };

const DAY_MS = 24 * 60 * 60 * 1000;

/** A unix-seconds timestamp as a short local date, or the honest reason there is not one. */
export function describeDate(unixSeconds: number | null): string {
  if (unixSeconds === null) return "none stated";
  // The store's own contract is unix seconds; 0 (or anything non-positive) means the field was
  // absent when the row was written, and rendering 1970 would invent a fact.
  if (unixSeconds <= 0) return "not stated";
  const date = new Date(unixSeconds * 1000);
  if (Number.isNaN(date.getTime())) return "not stated";
  return date.toISOString().slice(0, 10);
}

/** True when a dated row is past `nowMs`. A row with no date never expires by its own terms. */
export function isPast(unixSeconds: number | null, nowMs: number): boolean {
  if (unixSeconds === null || unixSeconds <= 0) return false;
  return unixSeconds * 1000 < nowMs;
}

/**
 * Whole days until a date, for the warning that is meant to prompt an import.
 *
 * Only used for a row that is still current; callers check `isPast` first. Rounded up so a list with
 * twelve hours left reads "1 day", not "0 days", because "0 days" reads as expired.
 */
export function daysUntil(unixSeconds: number, nowMs: number): number {
  return Math.ceil((unixSeconds * 1000 - nowMs) / DAY_MS);
}

function load(): Promise<Loaded> {
  return Promise.all([listTrustAnchors(), listTrustCrls()]).then(([anchors, crls]) => ({
    anchors,
    crls,
  }));
}

export function TrustPanel({ nowMs = Date.now() }: { nowMs?: number }) {
  const [state, setState] = useState<State>({ kind: "loading" });
  const [busy, setBusy] = useState<string | null>(null);
  const [notice, setNotice] = useState<{ kind: "ok" | "error"; message: string } | null>(null);

  const refresh = useCallback(() => {
    let live = true;
    // Wrapped so a webview with no IPC boundary rejects into `.catch` rather than throwing
    // synchronously while rendering.
    Promise.resolve()
      .then(load)
      .then((loaded) => {
        if (live) setState({ kind: "ready", loaded });
      })
      .catch((error: unknown) => {
        if (live) setState({ kind: "error", message: toAppError(error).message });
      });
    return () => {
      live = false;
    };
  }, []);

  useEffect(() => refresh(), [refresh]);

  const remove = useCallback(
    async (kind: "anchor" | "crl", identity: string) => {
      setBusy(identity);
      setNotice(null);
      try {
        const removed =
          kind === "anchor"
            ? await forgetTrustAnchor(identity)
            : await forgetTrustCrl(identity);
        // The backend reports whether a row actually went. Saying "removed" when it did not would
        // leave the user believing a list no longer decides their badge.
        setNotice(
          removed
            ? { kind: "ok", message: "Removed. The badge will be re-decided without it." }
            : { kind: "error", message: "Nothing was removed: that entry is already gone." },
        );
        refresh();
      } catch (error: unknown) {
        setNotice({ kind: "error", message: toAppError(error).message });
      } finally {
        setBusy(null);
      }
    },
    [refresh],
  );

  return (
    <fieldset className="trust-panel">
      <legend>Your trust store</legend>
      <p className="trust-panel__hint">
        A signature can only be shown as valid against an issuer you have chosen to trust, and a
        revocation list for it. Both are files you supply; nothing is downloaded.
      </p>

      {state.kind === "loading" ? <p role="status">Reading the trust store…</p> : null}

      {state.kind === "error" ? (
        <p className="trust-panel__error" role="alert">
          {state.message}
        </p>
      ) : null}

      {state.kind === "ready" ? (
        <>
          <section aria-labelledby="trust-anchors-heading">
            <h3 id="trust-anchors-heading">Trusted issuers</h3>
            {state.loaded.anchors.length === 0 ? (
              <p className="trust-panel__empty">
                No issuers are trusted yet, so every signature will read as unknown until you
                import one.
              </p>
            ) : (
              <ul className="trust-panel__list">
                {state.loaded.anchors.map((anchor) => {
                  const expired = isPast(anchor.notAfter, nowMs);
                  return (
                    <li key={anchor.derBase64} className="trust-panel__row">
                      <span className="trust-panel__name">{anchor.subject}</span>
                      <span
                        className={
                          expired ? "trust-panel__date trust-panel__date--expired" : "trust-panel__date"
                        }
                      >
                        {expired ? "expired " : "expires "}
                        {describeDate(anchor.notAfter)}
                      </span>
                      <button
                        type="button"
                        onClick={() => void remove("anchor", anchor.derBase64)}
                        disabled={busy !== null}
                      >
                        {busy === anchor.derBase64 ? "Removing…" : `Stop trusting ${anchor.subject}`}
                      </button>
                    </li>
                  );
                })}
              </ul>
            )}
          </section>

          <section aria-labelledby="trust-crls-heading">
            <h3 id="trust-crls-heading">Revocation lists</h3>
            {state.loaded.crls.length === 0 ? (
              <p className="trust-panel__empty">
                No revocation lists are imported. Without one, a signature can be shown as intact
                but its revocation status stays unknown.
              </p>
            ) : (
              <ul className="trust-panel__list">
                {state.loaded.crls.map((crl) => {
                  const expired = isPast(crl.nextUpdate, nowMs);
                  const remaining =
                    crl.nextUpdate !== null && !expired ? daysUntil(crl.nextUpdate, nowMs) : null;
                  return (
                    <li key={crl.derBase64} className="trust-panel__row">
                      <span className="trust-panel__name">{crl.issuer}</span>
                      <span
                        className={
                          expired ? "trust-panel__date trust-panel__date--expired" : "trust-panel__date"
                        }
                      >
                        {expired
                          ? `expired ${describeDate(crl.nextUpdate)} — no longer counts as evidence`
                          : crl.nextUpdate === null
                            ? "no expiry stated — treated as current"
                            : `current until ${describeDate(crl.nextUpdate)}`}
                      </span>
                      {remaining !== null && remaining <= 30 ? (
                        <span className="trust-panel__warn" role="status">
                          {remaining} day{remaining === 1 ? "" : "s"} left; import a newer list when
                          you can.
                        </span>
                      ) : null}
                      <button
                        type="button"
                        onClick={() => void remove("crl", crl.derBase64)}
                        disabled={busy !== null}
                      >
                        {busy === crl.derBase64 ? "Removing…" : `Forget list from ${crl.issuer}`}
                      </button>
                    </li>
                  );
                })}
              </ul>
            )}
          </section>
        </>
      ) : null}

      {notice ? (
        <p
          className={
            notice.kind === "ok" ? "trust-panel__ok" : "trust-panel__error"
          }
          role={notice.kind === "ok" ? "status" : "alert"}
        >
          {notice.message}
        </p>
      ) : null}
    </fieldset>
  );
}
