import { useCallback, useEffect, useState } from "react";
import type { LibraryEntry, PageTextRun } from "../api/types";
import { sameTextRunId, textRunIdKey } from "../api/types";
import type { PdfRect } from "../state/geometry";
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
  return a !== null && b !== null && sameTextRunId(a.runId, b.runId);
}

/** Whether `outer` fully covers `inner` in PDF points, edges included. */
function covers(outer: PdfRect, inner: PdfRect): boolean {
  return (
    outer.x0 <= inner.x0 &&
    outer.y0 <= inner.y0 &&
    outer.x1 >= inner.x1 &&
    outer.y1 >= inner.y1
  );
}

/**
 * The runs that should receive a click on the page (P2.4.2 O-Chain-3).
 *
 * The picker draws one box per top-level run, so a click on the page must resolve to the run
 * the user can see is there. A **box-suspicious** run (T22) is the exception: it claimed a box
 * it does not fill — the 0.4.1 `ence` orphan against a whole **line** — so its line-sized box
 * must not swallow clicks meant for the run that really occupies that line. When an honest
 * run's box covers a suspicious run's, the suspicious one is reached only from the panel list
 * (fragment-level targeting after explicit expand, O-Pair-3) and takes no page click.
 *
 * A run with no box is never shadowed — it has nothing to be covered by, and the panel is the
 * only way to select it (T21).
 */
export function clickTargets(runs: PageTextRun[]): PageTextRun[] {
  return runs.filter((run) => {
    if (!run.rect || !run.boxSuspicious) return true;
    const rect = run.rect;
    return !runs.some(
      (other) =>
        other !== run &&
        other.rect !== null &&
        other.rect !== undefined &&
        !other.boxSuspicious &&
        covers(other.rect, rect),
    );
  });
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
  // P2.7.6: a glyph the bundled faces have, but a subset we failed to build. Distinct from
  // GLYPHS_NOT_COVERABLE on purpose -- the remedy is "report it", not "choose a font", and
  // both belong on the panel so the message lands next to the run it concerns.
  "SUBSET_BUILD_FAILED",
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
  const { selectedRun, selection, selectRun, textEditMode } = useEditor();
  if (!textEditMode || !metrics) return null;

  // P2.4.2 O-Chain-3: a box-suspicious run's line-sized box does not take page clicks when an
  // honest run's box covers it — the panel list remains the way to reach it.
  const targets = clickTargets(runs);

  return (
    <div className="textrun-overlay" aria-label="Editable text on this page">
      {targets.map((run, index) => {
        const box = runBox(run, metrics);
        if (!box) return null;
        return (
          <button
            key={`${textRunIdKey(run.runId)}:${index}`}
            type="button"
            className={`textrun-box${run.admissibility.editable ? "" : " refused"}${
              selection.some((picked) => sameRun(picked, run))
                ? sameRun(selectedRun, run)
                  ? " selected"
                  : " selected in-range"
                : ""
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
            // Shift-click extends the selection into a range of runs (P2.6 T26).
            onClick={(event) => selectRun(run, event.shiftKey)}
          >
            <span className="textrun-box-label">{run.text}</span>
          </button>
        );
      })}
    </div>
  );
}

/**
 * One entry in the picker list: a merged run, collapsed by default, expandable to its
 * fragments (P2.4.1 O-Pair-3).
 *
 * The two levels exist because a merged run is usually exactly one logical run — a word
 * broken up by the producer's kerning — and only occasionally a genuinely mixed-geometry
 * line the user needs to pick apart. Showing all five glyphs of every word by default would
 * bury the runs the user came to edit; showing none would hide the rare mixed case.
 */
function RunListItem({
  run,
  selected,
  onSelect,
}: {
  run: PageTextRun;
  selected: boolean;
  onSelect: (run: PageTextRun) => void;
}) {
  const { selectedRun } = useEditor();
  const [expanded, setExpanded] = useState(false);
  const fragments = run.fragments ?? [];
  const merged = run.fragmentCount > 1 && fragments.length > 0;

  const label = (
    <>
      <span className="textrun-text">{run.text}</span>
      <span className="textrun-font muted">
        {run.fontName || run.fontKey}
        {run.fontSize ? ` ${run.fontSize}pt` : ""}
      </span>
      {/* T21: a weak pair is flagged, not hidden. Its box is nearest-only rather than a
          containing rect, so the write path re-checks it — but it is still offered, because
          refusing it would be the 0.4.0 defect again. */}
      {run.pairConfidence === "weak" ? (
        <span className="textrun-weak muted" title="matched to the nearest box, not a containing one">
          nearest box
        </span>
      ) : null}
      {/* P2.4.2 T22: a box-suspicious pair claimed a box far wider than its own span — the
          0.4.1 orphan's line box. It is measured against its own span, and the user is told
          the geometric match is not to be trusted. */}
      {run.boxSuspicious ? (
        <span
          className="textrun-weak muted"
          title="paired to a box far wider than this run's own span; measured against its own span"
        >
          line box
        </span>
      ) : null}
      {/* T10-style disclosure: a merged run says so, and how many fragments it joined, so a
          single click never silently rewrites five operands. */}
      {merged ? (
        <span className="textrun-merged muted">
          combines {run.fragmentCount} fragments of {run.fontName || run.fontKey}
        </span>
      ) : null}
      {!run.admissibility.editable ? (
        <span className="textrun-gate">{run.admissibility.gate}</span>
      ) : null}
    </>
  );

  return (
    <li>
      <div className="textrun-row">
        <button
          type="button"
          className={run.admissibility.editable ? undefined : "refused"}
          aria-pressed={selected}
          title={
            run.admissibility.editable
              ? `Edit "${run.text}" (${run.fontName})`
              : (run.admissibility.message ?? "This text cannot be edited")
          }
          onClick={() => onSelect(run)}
        >
          {label}
        </button>
        {merged ? (
          <button
            type="button"
            className="textrun-expand"
            aria-expanded={expanded}
            aria-label={`${expanded ? "Hide" : "Show"} the ${run.fragmentCount} fragments of "${run.text}"`}
            onClick={() => setExpanded(!expanded)}
          >
            {expanded ? "▾" : "▸"}
          </button>
        ) : null}
      </div>
      {merged && expanded ? (
        <ul className="textrun-fragments" aria-label={`Fragments of "${run.text}"`}>
          {fragments.map((fragment, index) => (
            <li key={`${textRunIdKey(fragment.runId)}:${index}`}>
              <button
                type="button"
                className={fragment.admissibility.editable ? undefined : "refused"}
                aria-pressed={sameRun(selectedRun, fragment)}
                title={
                  fragment.admissibility.editable
                    ? `Edit fragment "${fragment.text}"`
                    : (fragment.admissibility.message ?? "This fragment cannot be edited")
                }
                onClick={() => onSelect(fragment)}
              >
                <span className="textrun-text">{fragment.text || "␣"}</span>
                <span className="textrun-font muted">{fragment.fontName}</span>
              </button>
            </li>
          ))}
        </ul>
      ) : null}
    </li>
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
    selection,
    selectRun,
    substitution,
    clearSubstitution,
    actualTextNotice,
    clearActualTextNotice,
    allowSubstitution,
    setAllowSubstitution,
  } = useEditor();
  const [replacement, setReplacement] = useState("");
  const [savedPath, setSavedPath] = useState<string | null>(null);

  // P2.6 T26: a selection is a range of runs. The box edits the head's text, so extending
  // the selection keeps the typed replacement rather than resetting it; only picking a
  // *different* head refills the form.
  const selectionText = selection.map((run) => run.text).join("");
  const isRange = selection.length > 1;
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
  }, [editText, page, selectedRun, selection, replacement, clearSubstitution]);

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
  // O-Pair-3: merged runs are listed first, then single runs. Stable within each group so the
  // picker does not reshuffle between renders.
  const orderedRuns = [
    ...pageRuns.filter((run) => run.fragmentCount > 1),
    ...pageRuns.filter((run) => run.fragmentCount <= 1),
  ];

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
              {/* O-Pair-3: merged runs first — they are the runs that were broken up and are
                  what the user most often came to fix — then the ordinary single runs. */}
              {orderedRuns.map((run, index) => (
                <RunListItem
                  key={`${textRunIdKey(run.runId)}:${index}`}
                  run={run}
                  selected={sameRun(selectedRun, run)}
                  onSelect={(picked) => {
                    selectRun(picked);
                    clearError();
                  }}
                />
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
                Replace{' '}
                <span className="path">{isRange ? selectionText : selectedRun.text}</span>{' '}
                with
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
                {/* P2.7.5: a CID/Type0 run keeps its own font — the toggle cannot reach it,
                    because a substitute would have to replace the encoding as well as the
                    face. Say so rather than letting the label imply otherwise. */}
                Substitute a standard font when a simple font cannot set the text
                (CID/Type0 runs keep their own font)
              </label>
              {/* T10-style disclosure for the merged case: the edit rewrites several
                  operands at once, so say so before the user applies it. */}
              {selectedRun.fragmentCount > 1 ? (
                <p className="textedit-merged muted">
                  This edit combines {selectedRun.fragmentCount} fragments of{' '}
                  {selectedRun.fontName || selectedRun.fontKey} in one write; undo restores
                  all of them together.
                </p>
              ) : null}
              {/* P2.6 T26: a range of runs is replaced as one span. The backend refuses a
                  range that is not one contiguous, same-style line, so say what was picked
                  and leave the refusal to speak for itself if it does not hold. */}
              {isRange ? (
                <p className="textedit-merged muted">
                  {selection.length} runs are selected and will be replaced together by one
                  write; shift-click extends the selection. Undo restores all of them.
                </p>
              ) : null}
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

      {/* P2.7.1 T30: the page and the document's accessible text now disagree. This is the
          one warning that must not be dismissible-by-ignorance, so it sits beside the
          substitution toast and names the consequence rather than the mechanism. */}
      {actualTextNotice ? (
        <div className="textedit-substitution" role="status">
          <span>{actualTextNotice}</span>
          <button
            type="button"
            aria-label="Dismiss accessibility text notice"
            onClick={clearActualTextNotice}
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
