import { useCallback, useEffect, useState } from "react";
import { redactApply, redactSearch, redactUnmark, toAppError } from "../api/invoke";
import type { RedactOutcome } from "../api/invoke";
import type { AppError } from "../api/types";
import { useEditor } from "../state/EditorProvider";

/**
 * Mark, apply and search-and-redact (P3.2 SCOPE-B, D-RD1/D-RD6).
 *
 * The copy here is deliberately blunt about what applying does, because this is the one operation in
 * the product that is meant to be irreversible. A user who expects an undo has misunderstood the
 * feature, and the interface is the only place that misunderstanding can be corrected before the
 * file is written.
 *
 * Three things it refuses to blur:
 *
 * 1. **Where the output goes.** Apply writes a new file and leaves the original alone; the panel says
 *    so before the button is pressed and names the path afterwards.
 * 2. **That marks are not in the file yet.** A marked document still contains the text, on disk and
 *    on screen. The panel says that in the list rather than implying the mark is protection.
 * 3. **That "nothing to remove" is a real answer.** A second apply at the same marks reports it
 *    instead of producing a second copy that looks the same.
 */
export function RedactPanel({ documentId }: { documentId: string | null }) {
  // The mark list is owned by the provider, so the overlay and this panel cannot disagree about
  // which marks exist. Local state here is only what this component derives from it.
  const { redactionMarks, refreshRedactionMarks, redactMode, setRedactMode } = useEditor();
  const marks = redactionMarks;
  const [pattern, setPattern] = useState("");
  const [error, setError] = useState<AppError | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [outcome, setOutcome] = useState<RedactOutcome | null>(null);
  const [busy, setBusy] = useState(false);

  const load = refreshRedactionMarks;

  // Read from the DB rather than keeping marks in component state: they outlive a restart by design,
  // so the database is the only honest answer to "what is pending". The provider owns the read, so
  // this only clears the per-document report.
  useEffect(() => {
    setError(null);
    setNotice(null);
    setOutcome(null);
  }, [documentId]);

  const removeMark = useCallback(
    async (markId: string) => {
      try {
        setError(null);
        await redactUnmark(markId);
        await load();
      } catch (e) {
        setError(toAppError(e));
      }
    },
    [load],
  );

  const search = useCallback(async () => {
    if (!documentId || pattern.trim() === "") return;
    setBusy(true);
    setError(null);
    setNotice(null);
    try {
      const found = await redactSearch(documentId, pattern);
      // An empty result is an answer, not a failure: say the text does not occur rather than
      // leaving the user to wonder whether the search ran.
      setNotice(
        found.length === 0
          ? `"${pattern}" does not occur in this document, so no marks were added.`
          : `Marked ${found.length} occurrence${found.length === 1 ? "" : "s"} of "${pattern}".`,
      );
      await load();
    } catch (e) {
      setError(toAppError(e));
    } finally {
      setBusy(false);
    }
  }, [documentId, load, pattern]);

  const apply = useCallback(async () => {
    if (!documentId) return;
    setBusy(true);
    setError(null);
    setNotice(null);
    try {
      const result = await redactApply(documentId);
      setOutcome(result);
      await load();
    } catch (e) {
      // A refusal to apply is expected in two ordinary cases -- nothing pending, and marks over
      // content already removed -- so it is shown as a notice with the backend's own wording
      // rather than as a fault banner.
      setError(toAppError(e));
      await load();
    } finally {
      setBusy(false);
    }
  }, [documentId, load]);

  if (!documentId) {
    return (
      <div className="redact-panel">
        <p className="muted">Open a document to redact it.</p>
      </div>
    );
  }

  return (
    <div className="redact-panel">
      <h3>Redact</h3>
      <p className="muted">
        Drag on a page to mark an area. Marking changes nothing in the file: the text is still
        there, on disk and on screen, until you apply.
      </p>

      <button
        type="button"
        onClick={() => setRedactMode(!redactMode)}
        aria-pressed={redactMode}
      >
        {redactMode ? "Stop marking" : "Start marking"}
      </button>
      {redactMode ? <p className="muted">Drag on the page to mark an area.</p> : null}

      <div className="redact-search">
        <label>
          Find text
          <input
            type="text"
            value={pattern}
            placeholder="Text to find"
            onChange={(event) => setPattern(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === "Enter") search();
            }}
          />
        </label>
        <button type="button" onClick={search} disabled={busy || pattern.trim() === ""}>
          Mark all
        </button>
      </div>

      {notice ? <p className="muted">{notice}</p> : null}
      {error ? (
        <p className="redact-error" role="alert">
          {error.message}
        </p>
      ) : null}

      {marks.length === 0 ? (
        <p className="muted">No marks yet.</p>
      ) : (
        <ul className="redact-marks">
          {marks.map((mark) => (
            <li key={mark.id}>
              <span>
                Page {mark.page + 1}
                {mark.pattern ? ` — “${mark.pattern}”` : ""}
              </span>
              <button type="button" onClick={() => removeMark(mark.id)} disabled={busy}>
                Remove
              </button>
            </li>
          ))}
        </ul>
      )}

      <button
        type="button"
        className="redact-apply"
        onClick={apply}
        disabled={busy || marks.length === 0}
      >
        Apply {marks.length} mark{marks.length === 1 ? "" : "s"}
      </button>
      <p className="muted">
        Applying writes a new file beside the original and leaves the original untouched. This
        cannot be undone, and it is why the text it removes does not come back.
      </p>

      {outcome ? (
        <div className="redact-outcome" role="status">
          <p>Wrote {outcome.path}</p>
          <ul>
            <li>{outcome.runsRemoved} text run(s) removed</li>
            <li>{outcome.annotationsRemoved} annotation(s) removed</li>
            <li>{outcome.imagesRemoved} image(s) removed</li>
            <li>{outcome.objectsDropped} object(s) dropped</li>
            <li>{outcome.marksCleared} mark(s) cleared</li>
          </ul>
        </div>
      ) : null}
    </div>
  );
}
