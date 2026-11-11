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
import { TrustPanel } from "./TrustPanel";
import { open as openFileDialog } from "@tauri-apps/plugin-dialog";
import type { SigningCertificates, SigningCertificate, TrustImport } from "../api/types";
import {
  importTrustAnchor,
  importTrustCrl,
  listSigningCertificates,
  signDocument,
  toAppError,
} from "../api/invoke";

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
  onTrustChanged,
}: {
  documentId: string;
  onClose: () => void;
  /**
   * Called when the trust store changed, so the badge re-reads it.
   *
   * Separate from `onSigned` because nothing was signed: the badge's inputs changed, not the file.
   * Sharing one callback would make "a file was written" and "a certificate was imported" read the
   * same at every call site, and the two have different consequences.
   */
  onTrustChanged: () => void;
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

      {/* Both imports sit here rather than on a separate settings screen: they are the answer to
          "why is this signature unknown?", which is the question this dialog exists to answer. */}
      <TrustImportSection onChanged={onTrustChanged} />
      {/* Below the imports, because it lists what the imports produced. The panel is what makes
          those imports reversible: an append-only store lets a mistyped CA or a stale revocation
          list decide this badge forever, with no way for the user to say otherwise. */}
      <TrustPanel />

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
 * Import a trust anchor or a CRL, which is what makes `valid` reachable at all (P3.2 D-SG4).
 *
 * **Why this belongs in the Sign dialog.** `valid` requires a verified path to an anchor *the user
 * imported*, so before this surface existed the strongest badge the product could ever show was
 * `unknown`, on every file, no matter how clean the signature was. The fix is not a new screen: the
 * user is already here, looking at whether their signature can be checked, and the answer to "why is
 * this unknown?" is "because nothing has been imported". Putting the import next to the question it
 * answers is what makes the badge reachable without teaching anyone a second workflow.
 *
 * **Both files are needed, and the wording says so.** An anchor says which issuer to check; a CRL is
 * its current answer about revocations. Importing only the anchor leaves the revocation check
 * unanswerable, which reads as `unknown` rather than as `valid` -- correct, and confusing unless
 * said out loud, which is why both buttons sit under one heading.
 *
 * **The outcome is reported, not assumed.** `added: false` is not an error: re-importing a
 * certificate the store already holds is ordinary, and saying "already trusted" is different from
 * saying "trusted" for a second time. Collapsing the two would let a user believe they had made two
 * trust decisions when they made one.
 */
function TrustImportSection({ onChanged }: { onChanged: () => void }) {
  const [busy, setBusy] = useState<null | "anchor" | "crl">(null);
  const [outcome, setOutcome] = useState<null | { kind: "ok" | "error"; message: string }>(null);

  const describe = (what: string, imported: TrustImport) =>
    imported.added
      ? `${what} trusted: ${imported.subject}`
      : `${imported.subject} was already trusted; nothing changed`;

  const pick = useCallback(
    async (which: "anchor" | "crl") => {
      setBusy(which);
      setOutcome(null);
      try {
        const picked = await openFileDialog({
          multiple: false,
          directory: false,
          // DER and PEM are both accepted by the backend; filtering to one would make the other
          // look like an unsupported file for a reason the user cannot see.
          filters: [{ name: which === "anchor" ? "Certificate" : "Revocation list", extensions: ["der", "cer", "crt", "pem", "crl"] }],
        });
        if (typeof picked !== "string") return;
        const imported =
          which === "anchor" ? await importTrustAnchor(picked) : await importTrustCrl(picked);
        setOutcome({ kind: "ok", message: describe(which === "anchor" ? "Certificate" : "Revocation list", imported) });
        // The badge depends on this store, so a successful import has to re-read it -- otherwise the
        // badge keeps saying `unknown` after the very action that could make it `valid`.
        onChanged();
      } catch (error: unknown) {
        setOutcome({ kind: "error", message: toAppError(error).message });
      } finally {
        setBusy(null);
      }
    },
    [onChanged],
  );

  return (
    <fieldset className="sign-dialog__trust">
      <legend>Trust</legend>
      <p className="sign-dialog__hint">
        A signature can only be shown as valid against an issuer you have chosen to trust, and its
        revocation list. Both are files you supply; nothing is downloaded.
      </p>
      <div className="sign-dialog__trust-actions">
        <button type="button" onClick={() => void pick("anchor")} disabled={busy !== null}>
          {busy === "anchor" ? "Importing…" : "Import certificate…"}
        </button>
        <button type="button" onClick={() => void pick("crl")} disabled={busy !== null}>
          {busy === "crl" ? "Importing…" : "Import revocation list…"}
        </button>
      </div>
      {outcome ? (
        <p
          className={outcome.kind === "ok" ? "sign-dialog__ok" : "sign-dialog__error"}
          role={outcome.kind === "ok" ? "status" : "alert"}
        >
          {outcome.message}
        </p>
      ) : null}
    </fieldset>
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
          // The dialog stays open: importing a certificate is usually not the last thing a user
          // wants to do here, and closing on success would hide the badge they just made reachable.
          onTrustChanged={onSigned}
        />
      ) : null}
    </span>
  );
}
