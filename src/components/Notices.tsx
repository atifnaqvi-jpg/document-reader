/**
 * P2.7.6 §4 / C-5 / Q5: the OFL notice for the bundled font payload.
 *
 * The licence requires the notice to reach the user, not just to sit in the installer. This
 * is the surface that does it: reachable from the main menu with no document open, so it is
 * readable before a font is ever used.
 *
 * The text is duplicated here rather than loaded from `src-tauri/resources/fonts` because the
 * webview has no filesystem access to the bundle in a release build. `tests/font_bundle.rs`
 * and `src/components/Notices.test.tsx` both read this file, so removing a required claim
 * fails a test rather than only a review.
 */

import { useEffect, useState } from "react";

import { getMigrationNotice } from "../api/invoke";

const FACES = [
  "Liberation Sans: Regular, Bold, Italic, Bold Italic",
  "Liberation Serif: Regular, Bold, Italic, Bold Italic",
  "Liberation Mono: Regular, Bold, Italic, Bold Italic",
];

export function Notices({ onClose }: { onClose: () => void }) {
  // D-R6: when the bundle identifier changed, the user's library was copied from the old
  // data directory. That happened without them asking, so it is stated here rather than
  // only logged -- a migration nobody can see is indistinguishable from one that failed.
  const [migration, setMigration] = useState<string | null>(null);
  useEffect(() => {
    let live = true;
    // Started inside a resolved promise so a missing IPC boundary (the About surface is
    // renderable in tests and in a plain webview) rejects into the catch instead of throwing
    // synchronously out of the effect.
    Promise.resolve()
      .then(() => getMigrationNotice())
      .then((notice) => {
        if (live) setMigration(notice);
      })
      .catch(() => {
        // The notice is informational; failing to read it must not break the About surface.
      });
    return () => {
      live = false;
    };
  }, []);

  return (
    <div className="notices-backdrop" role="presentation" onClick={onClose}>
      <section
        className="notices-panel"
        role="dialog"
        aria-modal="true"
        aria-labelledby="notices-title"
        onClick={(event) => event.stopPropagation()}
      >
        <header className="notices-header">
          <h2 id="notices-title">About and notices</h2>
          <button type="button" onClick={onClose} aria-label="Close notices">
            ✕
          </button>
        </header>

        <div className="notices-body">
          <p>DocuPro edits PDFs on this machine. It sends nothing over the network.</p>

          {migration ? (
            <p className="notices-migration" role="status">
              {migration}
            </p>
          ) : null}

          <h3>Bundled fonts</h3>
          <p>
            Twelve Liberation faces are bundled so that text containing characters outside
            Windows-1252 can still be edited. They are licensed under the{" "}
            <strong>SIL Open Font License, Version 1.1</strong>, which permits bundling and
            embedding.
          </p>
          <ul>
            {FACES.map((face) => (
              <li key={face}>{face}</li>
            ))}
          </ul>
          <p className="muted">
            Liberation Fonts 2.1.5. Digitized data copyright (c) 2010 Google Corporation.
            Copyright (c) 2012 Red Hat, Inc., with Reserved Font Name Liberation.
          </p>
          <p>
            The full licence ships with the application as{" "}
            <code>resources/fonts/LICENSE-Liberation.txt</code>, beside the font files. A
            per-face record — version, SHA-256 and the notice carried in each font&apos;s name
            table — is in <code>resources/fonts/FONT-RECORD.md</code>.
          </p>
          <p>
            Only a <em>subset</em> of a face is ever written into a PDF, and it is written under
            a name that does not use the reserved name.
          </p>

          <h3>PDF rendering</h3>
          <p>PDFium, used for rendering and text measurement. See its licence in the installer.</p>
        </div>
      </section>
    </div>
  );
}
