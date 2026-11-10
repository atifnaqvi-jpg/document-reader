/**
 * P3.2 SCOPE-C: the Sign dialog -- identity source, D-SG6 reason/location, explicit level.
 *
 * This is the surface that makes signing possible. Before it, `sign_document` was reachable only
 * from code, so the product could not sign a document at all.
 *
 * **Two identity sources, one of which works today.** The backend signs from a PKCS#12 file, and the
 * dialog asks for one. The Windows certificate store can be *listed*
 * (`list_signing_certificates`) but the signer has no way to use a store key yet -- it takes a path
 * and a password, not a key handle. The store section therefore renders as an explicit "not yet
 * usable here" rather than as a selectable option, because a radio button a user can pick and which
 * then fails is worse than a sentence that says so up front. The alternative -- passing the
 * certificate's DER hex as if it were a path -- type-checks and fails only at runtime, which is the
 * defect this layout exists to prevent.
 *
 * **The level is chosen, never implied (D-SG6).** An ordinary approval signature is the default and
 * certification is an explicit opt-in, because certification *restricts* later editing. An implicit
 * `P=1` would mark the user's own subsequent annotation as tampering on their own file. So the
 * approval option sends no level at all rather than a `0`.
 *
 * **The password is not retained.** It lives in this component's state for the duration of the call
 * and is cleared as soon as the call returns, whether it worked or not: a rejected password must not
 * be sitting in a field for the next attempt to submit by accident.
 */

import { useCallback, useEffect, useState } from "react";
import { open as openFileDialog } from "@tauri-apps/plugin-dialog";
import type { SigningCertificates, SigningCertificate } from "../api/types";
import { listSigningCertificates, signDocument, toAppError } from "../api/invoke";

/** Certification levels, with the restriction each one imposes (D-SG6, ISO 32000-1 12.8.2.2). */
export const CERTIFICATION_LEVELS = [
  {
    level: 1,
    label: "No changes permitted (P=1)",
    detail: "Nobody, including you, may fill a form, annotate or edit afterwards.",
  },
  {
    level: 2,
    label: "Form filling and signing (P=2)",
    detail: "Forms may be filled and further signatures added. Annotating is not permitted.",
  },
  {
    level: 3,
    label: "Also annotating (P=3)",
    detail: "Forms, signatures and annotations may be added. Editing content is not permitted.",
  },
] as const;

type Store =
  | { kind: "loading" }
  | { kind: "ready"; certificates: SigningCertificate[]; skipped: number }
  | { kind: "unavailable"; message: string };

type Result = { kind: "idle" } | { kind: "signed"; path: string } | { kind: "error"; message: string };

export function SignDialog({
  documentId,
  onClose,
  onSigned,
}: {
  documentId: string;
  onClose: () => void;
  /** Called once a file has actually been written, so the badge can re-read it. */
  onSigned: () => void;
}) {
  const [store, setStore] = useState<Store>({ kind: "loading" });
  const [pfxPath, setPfxPath] = useState<string | null>(null);
  const [password, setPassword] = useState("");
  // `null` is an approval signature. Deliberately not `0` and not a level: absence is a meaningful
  // choice here, and a truthy check would quietly turn it into `P=1`.
  const [certificationLevel, setCertificationLevel] = useState<number | null>(null);
  const [reason, setReason] = useState("");
  const [location, setLocation] = useState("");
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<Result>({ kind: "idle" });

  useEffect(() => {
    let live = true;
    Promise.resolve()
      .then(() => listSigningCertificates())
      .then((answer: SigningCertificates) => {
        if (!live) return;
        setStore({
          kind: "ready",
          certificates: answer.certificates,
          skipped: answer.skipped.length,
        });
      })
      .catch((error: unknown) => {
        if (!live) return;
        // A platform without a Windows store says exactly that. That is an answer, not a fault, and
        // it is reported as its own sentence rather than as "no certificates found".
        setStore({ kind: "unavailable", message: toAppError(error).message });
      });
    return () => {
      live = false;
    };
  }, []);

  const pickPfx = useCallback(async () => {
    // Wrapped because the dialog plugin is absent in a plain webview (tests, dev browser); the
    // failure is reported rather than thrown out of a React event handler.
    try {
      const picked = await openFileDialog({
        multiple: false,
        directory: false,
        filters: [{ name: "PKCS#12", extensions: ["pfx", "p12"] }],
      });
      if (typeof picked === "string") {
        setPfxPath(picked);
        setResult({ kind: "idle" });
      }
    } catch (error: unknown) {
      setResult({ kind: "error", message: toAppError(error).message });
    }
  }, []);

  const ready = pfxPath !== null && password !== "";

  const submit = useCallback(async () => {
    if (!pfxPath) return;
    setBusy(true);
    setResult({ kind: "idle" });
    try {
      const outcome = await signDocument({
        documentId,
        pfxPath,
        pfxPassword: password,
        certificationLevel: certificationLevel ?? undefined,
        // Blank is not absent: an all-whitespace field is a reason the user did not give, so it is
        // omitted rather than written into the signature as a space.
        reason: reason.trim() === "" ? undefined : reason.trim(),
        location: location.trim() === "" ? undefined : location.trim(),
      });
      setResult({ kind: "signed", path: outcome.path });
      onSigned();
    } catch (error: unknown) {
      setResult({ kind: "error", message: toAppError(error).message });
    } finally {
      setPassword("");
      setBusy(false);
    }
  }, [certificationLevel, documentId, location, onSigned, password, pfxPath, reason]);

  return (
    <div className="sign-dialog" role="dialog" aria-label="Sign document">
      <h2>Sign document</h2>
      <p className="muted">
        The signature is written to a new file beside the original. The original is not changed.
      </p>

      <fieldset>
        <legend>Certificate file</legend>
        <button type="button" onClick={() => void pickPfx()}>
          Choose a PKCS#12 file…
        </button>
        {pfxPath ? (
          <p className="path" role="status">
            {pfxPath}
          </p>
        ) : (
          <p className="muted">No file chosen. A `.pfx` or `.p12` with its private key is required.</p>
        )}
        <label>
          Password for the private key
          <input
            type="password"
            value={password}
            autoFocus
            onChange={(event) => setPassword(event.target.value)}
          />
        </label>
      </fieldset>

      {/*
        The store listing is shown so a user can confirm their certificate is installed, and marked
        plainly as not yet usable for signing. See the module comment: the signer needs a key
        handle, and passing DER hex as a path would fail only at runtime.
      */}
      <fieldset>
        <legend>Windows certificate store</legend>
        {store.kind === "loading" ? <p role="status">Reading certificates…</p> : null}
        {store.kind === "unavailable" ? <p className="muted">Not available: {store.message}</p> : null}
        {store.kind === "ready" && store.certificates.length === 0 ? (
          <p className="muted">No certificate with a private key is installed for this user.</p>
        ) : null}
        {store.kind === "ready" && store.certificates.length > 0 ? (
          <>
            <ul className="sign-dialog__certificates">
              {store.certificates.map((entry) => (
                <li key={entry.derHex}>
                  {entry.subject}
                  {entry.expired ? <span className="muted"> — expired</span> : null}
                  {!entry.canSign && !entry.expired ? (
                    <span className="muted"> — no private key</span>
                  ) : null}
                </li>
              ))}
            </ul>
            <p className="muted">
              Signing with a store certificate is not available yet. Choose the certificate's
              `.pfx` file above to sign.
            </p>
          </>
        ) : null}
        {store.kind === "ready" && store.skipped > 0 ? (
          <p className="muted">
            {store.skipped} certificate(s) in the store could not be read and are not listed.
          </p>
        ) : null}
      </fieldset>

      <fieldset>
        <legend>Type of signature</legend>
        <label>
          <input
            type="radio"
            name="certification-level"
            checked={certificationLevel === null}
            onChange={() => setCertificationLevel(null)}
          />
          Approval signature
        </label>
        <p className="muted">Records that you signed. Does not restrict later editing.</p>
        {CERTIFICATION_LEVELS.map((option) => (
          <div key={option.level}>
            <label>
              <input
                type="radio"
                name="certification-level"
                checked={certificationLevel === option.level}
                onChange={() => setCertificationLevel(option.level)}
              />
              {option.label}
            </label>
            <p className="muted">{option.detail}</p>
          </div>
        ))}
        <p className="muted">
          Certification must be the first signature on a document. On a file that already carries
          one, the app will refuse rather than add a second certification.
        </p>
      </fieldset>

      <label>
        Reason (optional)
        <input type="text" value={reason} onChange={(event) => setReason(event.target.value)} />
      </label>
      <label>
        Location (optional)
        <input type="text" value={location} onChange={(event) => setLocation(event.target.value)} />
      </label>

      {result.kind === "signed" ? (
        <p className="sign-dialog__ok" role="status">
          Signed. The new file is <span className="path">{result.path}</span>.
        </p>
      ) : null}
      {result.kind === "error" ? (
        <p className="sign-dialog__error" role="alert">
          {result.message}
        </p>
      ) : null}

      <div className="sign-dialog__actions">
        <button type="button" onClick={() => void submit()} disabled={busy || !ready}>
          {busy ? "Signing…" : "Sign"}
        </button>
        <button
          type="button"
          onClick={() => {
            setPassword("");
            onClose();
          }}
          disabled={busy}
        >
          Cancel
        </button>
      </div>
    </div>
  );
}

/**
 * The Sign button and its dialog, kept together so the trigger and the surface cannot drift apart.
 *
 * `signedTick` is passed through to remount the badge's reader after a successful sign. Without it
 * the badge keeps showing the verdict it read before signing, which would be `unsigned` on a
 * document the user had just signed -- correct at the moment it was read, and wrong by the time the
 * user looked at it.
 */
export function SignAction({
  documentId,
  signedTick,
  onSigned,
}: {
  documentId: string;
  signedTick: number;
  onSigned: () => void;
}) {
  const [open, setOpen] = useState(false);
  return (
    <span className="sign-action" key={signedTick}>
      <button type="button" onClick={() => setOpen(true)}>
        Sign…
      </button>
      {open ? (
        <SignDialog
          documentId={documentId}
          onClose={() => setOpen(false)}
          onSigned={() => {
            setOpen(false);
            onSigned();
          }}
        />
      ) : null}
    </span>
  );
}
