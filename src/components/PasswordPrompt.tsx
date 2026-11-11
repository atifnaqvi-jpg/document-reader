/**
 * Asks for the password of a protected PDF on open (release defect D1).
 *
 * Separate from `ProtectDialog`, which *writes* a protected copy: this one only collects a secret so
 * an existing file can be read, and the two differ in every part that matters -- no file is written,
 * nothing is remembered beyond the session, and a wrong answer must keep the dialog open rather than
 * close it with an error banner.
 *
 * The messages are fixed here rather than passed in because they are the whole point of the defect:
 * the user saw `PdfiumLibraryInternalError(PasswordError, )`, which names a library internal and
 * suggests nothing. `firstAsk` distinguishes "we have not been told yet" from "that one was wrong",
 * which is the only distinction the user can act on.
 */

import { useState } from "react";

/** What the dialog is asking for. */
export type PasswordAsk = {
  /** The file being opened, shown so a mistargeted click is visible before typing. */
  path: string;
  /** True when a password has already been refused, so the message says so. */
  refused: boolean;
};

export function PasswordPrompt({
  ask,
  busy,
  onSubmit,
  onCancel,
}: {
  ask: PasswordAsk;
  busy: boolean;
  onSubmit: (password: string) => void;
  onCancel: () => void;
}) {
  const [password, setPassword] = useState("");

  return (
    <div className="password-backdrop" role="presentation" onClick={onCancel}>
      <section
        className="password-panel"
        role="dialog"
        aria-modal="true"
        aria-labelledby="password-title"
        onClick={(event) => event.stopPropagation()}
      >
        <h2 id="password-title">Password required</h2>
        <p>
          This document is password-protected. Enter its password to open it.
        </p>
        {/* Named, because "a password was wrong" is useless if the user has several files open. */}
        <p className="muted password-path" title={ask.path}>
          {ask.path}
        </p>

        {ask.refused ? (
          <p className="password-refused" role="alert">
            That password did not open this document. Try again.
          </p>
        ) : null}

        <form
          onSubmit={(event) => {
            event.preventDefault();
            if (password === "" || busy) return;
            onSubmit(password);
          }}
        >
          <label>
            Password
            <input
              type="password"
              autoFocus
              value={password}
              onChange={(event) => setPassword(event.target.value)}
            />
          </label>
          <div className="password-actions">
            <button type="submit" disabled={busy || password === ""}>
              {busy ? "Opening…" : "Open"}
            </button>
            <button type="button" onClick={onCancel} disabled={busy}>
              Cancel
            </button>
          </div>
        </form>
      </section>
    </div>
  );
}
