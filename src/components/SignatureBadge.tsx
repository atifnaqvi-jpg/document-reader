/**
 * P3.2 SCOPE-C / D-SG7: the rendered signature badge.
 *
 * D-SG7 is explicit that the four states must be *rendered*, not merely computed: "`unknown` is the
 * correct answer for offline verification and must be the *rendered* badge, not an error." A
 * verifier nobody displays satisfies nothing, which is why this component exists and why the
 * previous unit's `badge.rs` alone was not enough.
 *
 * Two rules shape the markup:
 *
 * 1. Colour comes from `isGreen`, a boolean the backend computes, never from comparing the badge
 *    string. Comparing strings here would put the judgement in two places, and the copy that drifts
 *    is the one that turns a badge green.
 * 2. The detail lines are the backend's own wording. Nothing here rewrites or reorders them, so the
 *    "what was not checked" sentence cannot be dropped by a UI change.
 *
 * `unknown` renders with a neutral, not an alarming, tone. It is the honest answer on a machine with
 * no trust store, and styling it as an error would teach users to dismiss it -- which is exactly how
 * a real `modified` gets ignored.
 */

import { useCallback, useEffect, useState } from "react";
import type { SignatureBadge } from "../api/types";
import { getSignatureBadge } from "../api/invoke";

/** The state before the first answer arrives. Not `unknown`: nothing has been asked yet. */
type Pending = { kind: "pending" };

export function SignatureBadgeChip({ documentId }: { documentId: string }) {
  const [state, setState] = useState<SignatureBadge | Pending | { kind: "error"; message: string }>({
    kind: "pending",
  });
  const [open, setOpen] = useState(false);

  const load = useCallback(() => {
    let live = true;
    // Wrapped so a missing IPC boundary rejects into `.catch` rather than throwing synchronously
    // out of the effect. The badge is renderable in tests and in a plain webview.
    Promise.resolve()
      .then(() => getSignatureBadge(documentId))
      .then((badge) => {
        if (!live) return;
        setState(badge);
      })
      .catch((error: unknown) => {
        if (!live) return;
        setState({ kind: "error", message: String(error) });
      });
    return () => {
      live = false;
    };
  }, [documentId]);

  useEffect(() => load(), [load]);

  if ("kind" in state && state.kind === "pending") {
    return null;
  }
  if ("kind" in state && state.kind === "error") {
    // A failed *read* is not a verdict on the signature. Saying `modified` here would accuse the
    // document of something we did not check, so the failure is reported as itself.
    return (
      <span className="sig-badge sig-badge--error" title={state.message}>
        signature: could not be read
      </span>
    );
  }

  const badge = state as SignatureBadge;
  // The green class comes from the boolean alone. Interpolating `badge.badge` into it was a real
  // false green: a response saying `valid` with `isGreen: false` still got `--valid`, which is the
  // one thing D-SG7 forbids. The state string stays available for styling and for `data-state`,
  // but it cannot paint the badge green.
  const modifier = badge.isGreen ? "sig-badge--valid" : "sig-badge--neutral";

  return (
    <span className="sig-badge-wrap">
      <button
        type="button"
        className={`sig-badge ${modifier}`}
        data-state={badge.badge}
        aria-expanded={open}
        onClick={() => setOpen((value) => !value)}
      >
        {badge.badge}
      </button>
      {open && (
        <ul className="sig-badge__detail" role="list">
          {badge.lines.map((line) => (
            <li key={line}>{line}</li>
          ))}
        </ul>
      )}
    </span>
  );
}
