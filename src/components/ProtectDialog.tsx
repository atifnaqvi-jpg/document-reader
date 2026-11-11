/**
 * P3.2 SCOPE-A: the Protect dialog -- the surface `protect_document` never had.
 *
 * The backend has been complete and tested for two units: `protect_save` generates keys, writes the
 * copy, reopens it with the password to prove it opens, and refuses to touch the source (D-PR4).
 * `protect_document` wraps it and is registered in `lib.rs`. Nothing in `src/` called it, so no user
 * could protect a document, and every test of the write path was evidence about code the product
 * could not run. This is the same defect the trust store had, and it is worth naming plainly: a
 * command with no caller is not a feature, whatever its coverage says.
 *
 * **D-PR3 is the whole reason this file is shaped the way it is.** `Permissions::default()` in lopdf
 * is `all()`, and an "advanced" UI that starts from a default and lets the user clear boxes ships a
 * document that *looks* restricted and is not. So:
 *
 * 1. There is no "allow everything" shortcut and no omitted field. Every one of the seven bits is a
 *    visible checkbox with its own label, and all seven are always sent.
 * 2. The initial state is stated explicitly below rather than inherited from a default, and it is
 *    the *permissive* one -- because a protection dialog that starts by silently removing abilities
 *    is the mirror failure, and it is worse: the user gets a file they cannot use and no record of
 *    why. Starting permissive and making every restriction a deliberate untick means the resulting
 *    `/P` reflects choices the user actually made.
 *
 * **Two passwords, and why the second is not optional.** The user password opens the file; the
 * owner password governs the permissions. If they are equal, anyone who can open the document can
 * also lift its restrictions, which makes every box above advisory. So the dialog requires an owner
 * password distinct from the user password, and says why rather than silently accepting a weak
 * choice. `protect_save` is handed both; the user password is not retained (the copy has just been
 * given one, so a later edit prompts through the existing path).
 *
 * **The output file is named before the click, not after.** D-PR4 makes protect write a *sibling*
 * copy, and the consistency rule carried from P3.1 is that a destructive action states its output
 * before performing it. The dialog shows the `(protected).pdf` convention up front so nobody expects
 * their original to change.
 */

import { useCallback, useState } from "react";
import type { PermissionsChoice, ProtectOutcome } from "../api/types";
import { protectDocument, toAppError } from "../api/invoke";

/**
 * The seven permissions, in the order a person thinks about them.
 *
 * `key` is the wire field, so a typo here is a compile error rather than a silently-ignored box.
 * Each `detail` says what the permission actually lets a reader do -- "modify" alone does not tell
 * anyone whether it covers annotating.
 */
export const PERMISSIONS = [
  {
    key: "print",
    label: "Print",
    detail: "Send the document to a printer or a print-to-PDF driver.",
  },
  {
    key: "printHighQuality",
    label: "Print at high resolution",
    detail: "Print a faithful copy rather than a degraded one.",
  },
  {
    key: "modify",
    label: "Change the contents",
    detail: "Edit, reflow or rewrite the text and images.",
  },
  {
    key: "copy",
    label: "Copy text and images",
    detail: "Extract content, for the clipboard or for another program.",
  },
  {
    key: "annotate",
    label: "Add comments and annotations",
    detail: "Highlight, comment, draw, or place stamps.",
  },
  {
    key: "fillForms",
    label: "Fill in forms",
    detail: "Type into form fields, and sign where a signature field exists.",
  },
  {
    key: "assemble",
    label: "Insert, rotate or delete pages",
    detail: "Change the page order or the set of pages.",
  },
] as const satisfies ReadonlyArray<{
  key: keyof PermissionsChoice;
  label: string;
  detail: string;
}>;

/**
 * The opening state: every permission granted.
 *
 * Exported so a test can assert the *stated* default rather than reading it out of the render, and
 * so the reason above is attached to the value it explains.
 */
export const DEFAULT_PERMISSIONS: PermissionsChoice = {
  print: true,
  printHighQuality: true,
  modify: true,
  copy: true,
  annotate: true,
  fillForms: true,
  assemble: true,
};

type Result =
  | { kind: "idle" }
  | { kind: "protected"; outcome: ProtectOutcome }
  | { kind: "error"; message: string };

export function ProtectDialog({
  documentId,
  onClose,
  onProtected,
}: {
  documentId: string;
  onClose: () => void;
  onProtected?: () => void;
}) {
  const [userPassword, setUserPassword] = useState("");
  const [confirm, setConfirm] = useState("");
  const [ownerPassword, setOwnerPassword] = useState("");
  const [permissions, setPermissions] = useState<PermissionsChoice>(DEFAULT_PERMISSIONS);
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<Result>({ kind: "idle" });

  const toggle = useCallback((key: keyof PermissionsChoice) => {
    setPermissions((current) => ({ ...current, [key]: !current[key] }));
  }, []);

  /**
   * Why the Protect button is disabled, in the user's words -- or null when it is enabled.
   *
   * A single function so the button's state and the sentence explaining it cannot disagree. Each
   * refusal names what to do next; "invalid input" would leave the user guessing which field.
   */
  const refusal: string | null =
    userPassword.length === 0
      ? "Set a password to open the document."
      : confirm !== userPassword
        ? "The two opening passwords do not match."
        : ownerPassword.length === 0
          ? "Set an owner password. Without one, anyone who can open the file can also lift its restrictions."
          : ownerPassword === userPassword
            ? "The owner password must differ from the opening password, or its restrictions are advisory."
            : null;

  const submit = useCallback(async () => {
    if (refusal !== null) return;
    setBusy(true);
    setResult({ kind: "idle" });
    try {
      const outcome = await protectDocument(
        documentId,
        userPassword,
        ownerPassword,
        permissions,
      );
      setResult({ kind: "protected", outcome });
      // Not retained: the copy has just been given a password, and a later edit of it prompts
      // through the existing path. Clearing here means a rejected or abandoned attempt leaves
      // nothing in state for the next attempt to submit by accident.
      setUserPassword("");
      setConfirm("");
      setOwnerPassword("");
      onProtected?.();
    } catch (error: unknown) {
      // The backend's sentence, not a stack string: `protect_save` explains which check failed
      // (weak password, unwritable sibling, verification reopen), and rewording it here would be a
      // second place for those to drift.
      setResult({ kind: "error", message: toAppError(error).message });
    } finally {
      setBusy(false);
    }
  }, [documentId, ownerPassword, permissions, refusal, userPassword, onProtected]);

  return (
    <fieldset className="protect-dialog">
      <legend>Protect with password</legend>
      <p className="protect-dialog__hint">
        This writes a new file beside the original. The original is opened read-only and is never
        modified.
      </p>

      <label>
        Password to open
        <input
          type="password"
          value={userPassword}
          onChange={(event) => setUserPassword(event.target.value)}
          autoComplete="new-password"
        />
      </label>
      <label>
        Repeat the password
        <input
          type="password"
          value={confirm}
          onChange={(event) => setConfirm(event.target.value)}
          autoComplete="new-password"
        />
      </label>
      <label>
        Owner password
        <input
          type="password"
          value={ownerPassword}
          onChange={(event) => setOwnerPassword(event.target.value)}
          autoComplete="new-password"
        />
      </label>

      {/* D-PR3: every bit is shown with its current state. No "all"/"none" shortcut, because a
          single control that clears six boxes hides which six were cleared. */}
      <fieldset className="protect-dialog__permissions">
        <legend>What a reader may do</legend>
        {PERMISSIONS.map((permission) => (
          <label key={permission.key} className="protect-dialog__permission">
            <input
              type="checkbox"
              // The visible label wraps the box, so without this the accessible name is
              // "Print Send the document to a printer..." -- and "Print" then matches both print
              // rows, which is ambiguous for a screen reader as well as for a test.
              aria-label={permission.label}
              checked={permissions[permission.key]}
              onChange={() => toggle(permission.key)}
            />
            <span>{permission.label}</span>
            <span className="protect-dialog__permission-detail">{permission.detail}</span>
          </label>
        ))}
      </fieldset>

      {refusal !== null ? (
        <p className="protect-dialog__hint" role="status">
          {refusal}
        </p>
      ) : null}

      <div className="protect-dialog__actions">
        <button type="button" onClick={() => void submit()} disabled={refusal !== null || busy}>
          {busy ? "Protecting…" : "Protect"}
        </button>
        <button type="button" onClick={onClose} disabled={busy}>
          Cancel
        </button>
      </div>

      {result.kind === "protected" ? (
        <p className="protect-dialog__ok" role="status">
          Protected. The new file is <span className="path">{result.outcome.path}</span>, and it was
          reopened with the password to confirm it opens.
        </p>
      ) : null}
      {result.kind === "error" ? (
        <p className="protect-dialog__error" role="alert">
          {result.message}
        </p>
      ) : null}
    </fieldset>
  );
}

/** The toolbar trigger and its dialog, kept together so they cannot drift apart. */
export function ProtectAction({ documentId }: { documentId: string }) {
  const [open, setOpen] = useState(false);
  return (
    <span className="protect-action">
      <button type="button" onClick={() => setOpen(true)}>
        Protect…
      </button>
      {open ? <ProtectDialog documentId={documentId} onClose={() => setOpen(false)} /> : null}
    </span>
  );
}
