import { useState } from "react";
import type { AppError, LibraryEntry } from "../api/types";
import { isAppError } from "../api/invoke";

interface Props {
  entries: LibraryEntry[];
  selectedId: string | null;
  onSelect: (entry: LibraryEntry) => void;
  /**
   * Removes one document from the library. **The file on disk is never touched** (D14).
   *
   * Returns a promise so this pane can show a busy state and keep the confirm step open on
   * failure: a removal that failed must not look like one that succeeded, and the dialog is the
   * only place the reason can be shown against the document it concerns.
   */
  onForget: (entry: LibraryEntry) => Promise<void>;
}

export function LibraryPane({ entries, selectedId, onSelect, onForget }: Props) {
  /** The document the confirm step is open for, if any. */
  const [pending, setPending] = useState<LibraryEntry | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<AppError | null>(null);

  const close = () => {
    if (busy) return;
    setPending(null);
    setError(null);
  };

  if (entries.length === 0) {
    return (
      <div className="pane-empty">
        <p>Your library is empty.</p>
        <p className="muted">Use “Open…” to add a PDF, TXT, or Markdown file.</p>
      </div>
    );
  }

  return (
    <>
      <ul className="library-list">
        {entries.map((entry) => (
          <li key={entry.id} className="library-row">
            <button
              type="button"
              className={`library-item${entry.id === selectedId ? " selected" : ""}`}
              onClick={() => onSelect(entry)}
            >
              <span className="library-title">{entry.title ?? "(untitled)"}</span>
              <span className="library-meta">
                {entry.format.toUpperCase()}
                {entry.pages != null ? ` · ${entry.pages} pages` : ""}
                {entry.missing ? " · file moved or deleted" : ""}
              </span>
            </button>
            {/* A *sibling* of the select button, never a child: a button inside a button is
                invalid HTML and one press would do both things. As a sibling it is also
                reachable by Tab without changing what pressing Enter on the row does, which is
                requirement 1 -- the two actions cannot be confused by pointer or keyboard.
                The label names the document so a screen reader announces which row, rather
                than forty identical "Remove" buttons. */}
            <button
              type="button"
              className="library-remove"
              aria-label={`Remove ${entry.title ?? "(untitled)"} from the library`}
              title="Remove from library (the file stays on disk)"
              onClick={(event) => {
                event.stopPropagation();
                setError(null);
                setPending(entry);
              }}
            >
              ×
            </button>
          </li>
        ))}
      </ul>
      {pending ? (
        <ForgetConfirm
          entry={pending}
          busy={busy}
          error={error}
          onDismissError={() => setError(null)}
          onCancel={close}
          onConfirm={() => {
            setBusy(true);
            setError(null);
            void onForget(pending)
              .then(() => {
                setBusy(false);
                setPending(null);
              })
              .catch((e: unknown) => {
                setBusy(false);
                setError(isAppError(e) ? e : null);
              });
          }}
        />
      ) : null}
    </>
  );
}

/**
 * The confirm step. It has to state two things plainly, because the whole point of the ruling
 * is that "remove" is easily misread as "delete":
 *
 *   1. the file stays on disk, in the place the user put it;
 *   2. anything not yet saved to a file is discarded.
 *
 * The second is not a hedge. Annotations live in the database until a save writes them into a
 * PDF, so a document with unsaved marks genuinely loses them here -- saying so is the price of a
 * confirm step that is not theatre. The document is named, since a generic "are you sure?" would
 * not identify which row is at stake.
 */
function ForgetConfirm({
  entry,
  busy,
  error,
  onDismissError,
  onCancel,
  onConfirm,
}: {
  entry: LibraryEntry;
  busy: boolean;
  error: AppError | null;
  onDismissError: () => void;
  onCancel: () => void;
  onConfirm: () => void;
}) {
  const title = entry.title ?? "(untitled)";

  return (
    <div className="password-backdrop" role="presentation" onClick={onCancel}>
      <section
        className="password-panel"
        role="dialog"
        aria-modal="true"
        aria-labelledby="forget-title"
        onClick={(event) => event.stopPropagation()}
      >
        <h2 id="forget-title">Remove from library?</h2>
        <p>
          <strong>{title}</strong> will no longer appear in DocuPro.
        </p>
        <p className="muted forget-path" title={entry.path}>
          {entry.path}
        </p>
        {/* The file statement is the important one, so it is not the muted note. */}
        <p>The file stays on disk, where it is now. It is not deleted, moved or changed.</p>
        <p className="muted">
          Any annotations that have not been saved to a file will be discarded. You can open the
          file again to add it back to the library.
        </p>
        {entry.missing ? (
          <p className="muted">
            DocuPro cannot find this file, so it can only be taken off the list. Nothing on disk
            is deleted.
          </p>
        ) : null}
        {error ? (
          <p className="error" role="alert">
            {error.message}{" "}
            <button type="button" className="link" onClick={onDismissError}>
              Dismiss
            </button>
          </p>
        ) : null}
        <div className="password-actions">
          <button type="button" className="secondary" onClick={onCancel} disabled={busy}>
            Cancel
          </button>
          <button type="button" onClick={onConfirm} disabled={busy}>
            {busy ? "Removing…" : "Remove from library"}
          </button>
        </div>
      </section>
    </div>
  );
}

export function ErrorBanner({ error }: { error: AppError | null }) {
  if (!error) return null;
  return (
    <div className="error-banner" role="alert">
      <strong>{error.code}</strong>
      <span>{error.message}</span>
    </div>
  );
}
