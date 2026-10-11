import { useCallback, useEffect, useState } from "react";
import type { LibraryEntry, PageTextRun } from "../api/types";
import { pdfToPage } from "../state/geometry";
import { useEditor } from "../state/EditorProvider";

/**
 * In-place text editing, Stage 1 (P2.4 §4/T2-T4).
 *
 * Two pieces, and the split is deliberate:
 *
 * * {@link TextRunOverlay} draws the picker **in the viewer**, over the page, so a run is
 *   chosen by clicking the text the user can see. It reuses the annotation overlay's
 *   geometry (`pdfToPage`) rather than inventing a second coordinate path — the same
 *   conversion that places a highlight places a run box, so the two can never disagree.
 * * {@link TextEditPanel} holds the run list, the form and the refusal text, in the editing
 *   pane where the other write actions live.
 *
 * Both read the same state from {@link useEditor}, which is also where the runs are
 * fetched: the provider already knows the document and page, and both surfaces already
 * consume it, so the picker and the form cannot get out of step.
 *
 * The verdict arrives **precomputed** with each run, so a run the write path would refuse
 * is visibly refused before the user types rather than after committing (P2.4 §4). A run
 * that is not editable is still drawn and still clickable — clicking it shows why — because
 * hiding it would leave the user wondering why a word they can read has no target.
 */

/** Rendered-page metrics needed to place a run box, mirroring the annotation overlay's. */
export interface OverlayMetrics {
  width: number;
  height: number;
  pageHeightPoints: number;
  scale: number;
}

/** One run's box, in the viewer's pixel space. */
function runBox(run: PageTextRun, metrics: OverlayMetrics) {
  if (!run.rect) return null;
  const { x, y, width, height } = pdfToPage(
    run.rect,
    metrics.scale,
    metrics.pageHeightPoints,
  );
  // A degenerate box (a space, a clipped run) would be an unhittable few-pixel target;
  // give it the height of its own font so it stays clickable.
  const minimum = Math.max(2, run.fontSize * metrics.scale);
  return {
    x,
    y: height < minimum ? y - (minimum - height) / 2 : y,
    width: Math.max(width, 4),
    height: Math.max(height, minimum),
  };
}

/** Whether two run addresses are the same run. Reference equality is not reliable: the
 * provider re-fetches the list, so the selected object and the listed object differ. */
export function sameRun(a: PageTextRun | null, b: PageTextRun | null): boolean {
  return (
    a !== null &&
    b !== null &&
    a.runId.opIndex === b.runId.opIndex &&
    a.runId.operandPath.join(",") === b.runId.operandPath.join(",")
  );
}

/**
 * The error codes the text-edit path can produce (P2.4 §5).
 *
 * They are listed here because the *panel* is where a text-edit refusal belongs: the user
 * is looking at the run they picked, so the numbers in a width refusal have somewhere to
 * land. Every other failure still falls through to the editing pane's own error region,
 * and the two sets are disjoint so the message is never shown twice.
 */
export const TEXT_EDIT_ERROR_CODES = [
  "TEXT_RUN_NOT_EDITABLE",
  "TEXT_RUN_MISMATCH",
  "CONTENT_TOO_LARGE",
  // Stage 2 (T3/R4): a replacement no available font can set, or one refused because the
  // user turned substitution off. Both belong on the panel, next to the run they concern.
  "GLYPHS_NOT_COVERABLE",
  "SUBSTITUTE_REFUSED",
];

/**
 * The clickable run boxes drawn over the rendered page.
 *
 * Rendered only while text-edit mode is on, so ordinary reading pays no cost and the page
 * is not covered in outlines the user did not ask for.
 */
export function TextRunOverlay({
  metrics,
  runs,
}: {
  metrics: OverlayMetrics | null;
  runs: PageTextRun[];
}) {
  const { selectedRun, selectRun, textEditMode } = useEditor();
  if (!textEditMode || !metrics) return null;

  return (
    <div className="textrun-overlay" aria-label="Editable text on this page">
      {runs.map((run, index) => {
        const box = runBox(run, metrics);
        if (!box) return null;
        return (
          <button
            key={`${run.runId.opIndex}:${run.runId.operandPath.join(".")}:${index}`}
            type="button"
            className={`textrun-box${run.admissibility.editable ? "" : " refused"}${
              sameRun(selectedRun, run) ? " selected" : ""
            }`}
            style={{
              left: box.x,
              top: box.y,
              width: box.width,
              height: box.height,
            }}
            aria-label={`Edit text: ${run.text}${
              run.admissibility.editable ? "" : ` (not editable: ${run.admissibility.gate})`
            }`}
            title={
              run.admissibility.editable
                ? `Edit "${run.text}"`
                : (run.admissibility.message ?? "This text cannot be edited")
            }
            // Refused runs stay clickable: the click selects them and the panel explains
            // the gate. A dead box would be indistinguishable from a broken picker.
            onClick={() => selectRun(run)}
          >
            <span className="textrun-box-label">{run.text}</span>
          </button>
        );
      })}
    </div>
  );
}

/**
 * The form, the run list and the refusal text, in the editing pane.
 *
 * The list duplicates the overlay's targets on purpose: on a densely typeset page the boxes
 * overlap, and a list is how a user picks a run their pointer cannot separate.
 */
export function TextEditPanel({
  entry,
  page,
}: {
  entry: LibraryEntry;
  page: number;
}) {
  const {
    editText,
    error,
    clearError,
    textEditMode,
    setTextEditMode,
    pageRuns,
    runsLoading,
    selectedRun,
    selectRun,
    substitution,
    clearSubstitution,
    allowSubstitution,
    setAllowSubstitution,
  } = useEditor();
  const [replacement, setReplacement] = useState("");
  const [savedPath, setSavedPath] = useState<string | null>(null);

  // Track the selection into the box, so picking a run by clicking it on the page fills
  // the form exactly as picking it from the list does.
  useEffect(() => {
    setReplacement(selectedRun?.text ?? "");
    setSavedPath(null);
  }, [selectedRun]);

  const apply = useCallback(async () => {
    if (!selectedRun) return;
    setSavedPath(null);
    clearSubstitution();
    const path = await editText(page, selectedRun, replacement);
    if (path) setSavedPath(path);
  }, [editText, page, selectedRun, replacement, clearSubstitution]);

  // A protected document refuses every edit (P2.4 T7), so the picker is not offered at
  // all: turning it on would promise an edit the backend will always reject.
  if (entry.encrypted) {
    return (
      <fieldset className="editor-textedit">
        <legend>Edit text</legend>
        <p className="muted">
          This document is password-protected. Use{" "}
          <em>Decrypt a copy&hellip;</em> in the editing pane; the original stays protected
          and read-only.
        </p>
      </fieldset>
    );
  }

  const editable = pageRuns.filter((run) => run.admissibility.editable);

  return (
    <fieldset className="editor-textedit">
      <legend>Edit text (page {page + 1})</legend>

      <button
        type="button"
        aria-pressed={textEditMode}
        onClick={() => {
          setTextEditMode(!textEditMode);
          clearError();
        }}
      >
        {textEditMode ? "Stop editing text" : "Edit text on this page"}
      </button>

      {textEditMode ? (
        <>
          {runsLoading ? <p className="muted">Finding text on this page…</p> : null}

          {!runsLoading && pageRuns.length === 0 ? (
            <p className="muted">No editable text on this page.</p>
          ) : null}

          {pageRuns.length > 0 ? (
            <ul className="textrun-list" aria-label="Text runs on this page">
              {pageRuns.map((run, index) => (
                <li key={`${run.runId.opIndex}:${run.runId.operandPath.join(".")}:${index}`}>
                  <button
                    type="button"
                    className={run.admissibility.editable ? undefined : "refused"}
                    aria-pressed={sameRun(selectedRun, run)}
                    title={
                      run.admissibility.editable
                        ? `Edit "${run.text}" (${run.fontName})`
                        : (run.admissibility.message ?? "This text cannot be edited")
                    }
                    onClick={() => {
                      selectRun(run);
                      clearError();
                    }}
                  >
                    <span className="textrun-text">{run.text}</span>
                    <span className="textrun-font muted">
                      {run.fontName || run.fontKey}
                      {run.fontSize ? ` ${run.fontSize}pt` : ""}
                    </span>
                    {!run.admissibility.editable ? (
                      <span className="textrun-gate">{run.admissibility.gate}</span>
                    ) : null}
                  </button>
                </li>
              ))}
            </ul>
          ) : null}

          {pageRuns.length > 0 && editable.length === 0 ? (
            <p className="muted">
              None of this page&rsquo;s text can be edited in place. Add a text annotation
              instead.
            </p>
          ) : null}

          {selectedRun ? (
            <div className="textedit-form">
              {!selectedRun.admissibility.editable ? (
                <p className="textedit-refusal" role="status">
                  {selectedRun.admissibility.message ??
                    "This text cannot be edited in place."}
                </p>
              ) : null}
              <label>
                Replace <span className="path">{selectedRun.text}</span> with
                <input
                  type="text"
                  value={replacement}
                  disabled={!selectedRun.admissibility.editable}
                  onChange={(event) => setReplacement(event.target.value)}
                />
              </label>
              <button
                type="button"
                onClick={apply}
                disabled={
                  !selectedRun.admissibility.editable ||
                  replacement === selectedRun.text
                }
              >
                Apply text edit
              </button>
              <label className="textedit-substitute-toggle">
                <input
                  type="checkbox"
                  checked={allowSubstitution}
                  onChange={(event) => setAllowSubstitution(event.target.checked)}
                />
                Substitute a standard font when this run&rsquo;s font cannot set the text
              </label>
              <p className="muted">Writes a new file. The original is never changed.</p>
            </div>
          ) : null}
        </>
      ) : null}

      {savedPath ? (
        <p className="editor-saved" role="status">
          Saved to <span className="path">{savedPath}</span>
        </p>
      ) : null}

      {/* T10: the typeface changed, so say so. The backend's disclosure sentence is shown
          verbatim — a toast that paraphrased it could hide which font the run now uses. */}
      {substitution ? (
        <div className="textedit-substitution" role="status">
          <span>{substitution.message}</span>
          <button
            type="button"
            aria-label="Dismiss font substitution notice"
            onClick={clearSubstitution}
          >
            Dismiss
          </button>
        </div>
      ) : null}

      {/* The refusal quotes the backend's message verbatim: it names the violated gate and,
          for the width gate, both measured widths (T3d), so it is never paraphrased here. */}
      {error ? (
        <p className="textedit-error" role="alert">
          {error.message}
        </p>
      ) : null}
    </fieldset>
  );
}

/**
 * The calm read-only banner for a password-protected document (P2.4 T7).
 *
 * Calm is the requirement: the document opened and reads normally, and the only thing the
 * user cannot do is edit it in place. The banner states that and names the explicit way
 * out — no prompt, no alarm, nothing modal.
 */
export function EncryptedBanner({ entry }: { entry: LibraryEntry | null }) {
  if (!entry?.encrypted) return null;
  return (
    <div className="encrypted-banner" role="status">
      <strong>Password-protected</strong>
      <span>
        Opened read-only. Use <em>Decrypt a copy…</em> in the editing pane to make an
        editable copy; the original stays protected.
      </span>
    </div>
  );
}
