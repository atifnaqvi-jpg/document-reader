import { useCallback, useEffect, useState } from "react";
import { useEditor } from "../state/EditorProvider";
import { getPageTextRuns } from "../api/invoke";
import type { PageTextRun } from "../api/types";

/**
 * The Fill & Sign control (P3.3a §5).
 *
 * ## The gap this closes
 *
 * Before this panel the engine could write a visual signature but no user could reach it: the
 * backend labels a row carrying `visualSignature`, and nothing in `src/` ever set that flag. A
 * control with no surface is not a feature, so this is the surface -- the toggle that turns the
 * placement layer on, and the text field the typed case needs.
 *
 * ## What it deliberately does not do
 *
 * * **No Save here.** §5: one user action must not produce two files, so the existing Save
 *   (`save_edits`, which writes a sibling copy) commits the placement. A second Save would be a
 *   second file-writing path.
 * * **No certificate, no level, no verdict.** That is `SignDialog`'s cryptographic flow. I-F6 says
 *   no code path may return a cryptographic verdict for a visual overlay, so the words "valid",
 *   "verified" and "certified" do not appear here at all.
 * * **No image signature.** Q1 deferred it to its own unit with its own design and hand test.
 */
export function FillSignPanel({
  documentId,
  page,
}: {
  documentId: string | null;
  page: number;
}) {
  const {
    fillSignMode,
    setFillSignMode,
    fillSignKind,
    setFillSignKind,
    fillSignText,
    setFillSignText,
    redactMode,
    textEditMode,
  } = useEditor();

  const [runs, setRuns] = useState<PageTextRun[]>([]);
  const [loading, setLoading] = useState(false);

  /**
   * The page's text runs, for snapping.
   *
   * Fetched only while the mode is on: the runs are the placement assistance, and loading them for
   * a user who is not signing would be a command call per page view for nothing. A failure leaves
   * the mode usable -- placement then lands where the user released, which is D-N1's stated
   * fallback rather than an error.
   */
  useEffect(() => {
    if (!fillSignMode || !documentId) {
      setRuns([]);
      return;
    }
    let cancelled = false;
    setLoading(true);
    getPageTextRuns(documentId, page)
      .then((loaded) => {
        if (!cancelled) setRuns(loaded);
      })
      .catch(() => {
        if (!cancelled) setRuns([]);
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [fillSignMode, documentId, page]);

  const toggle = useCallback(() => {
    setFillSignMode(!fillSignMode);
  }, [fillSignMode, setFillSignMode]);

  if (!documentId) {
    return (
      <div className="fillsign-panel">
        <h3>Fill &amp; Sign</h3>
        <p className="muted">Open a document to sign it.</p>
      </div>
    );
  }

  /** The number of lines the snap can use, so the user knows whether assistance is available. */
  const usable = runs.filter(
    (run) => run.rect && !run.boxSuspicious && run.fontSize > 0,
  ).length;

  return (
    <div className="fillsign-panel">
      <h3>Fill &amp; Sign</h3>
      {/*
        The disclosure, in the UI and before the act rather than only in the saved file. D-N2 puts
        the same words into the page content; a user deciding whether to rely on this should read
        them before placing, not after opening the file elsewhere.
      */}
      <p className="muted">
        A visual signature is a mark drawn or typed on the page. It is not a digital signature: it
        carries no cryptographic weight and proves nothing about who signed the document.
      </p>

      <fieldset className="fillsign-kind">
        <legend>Signature type</legend>
        <label>
          <input
            type="radio"
            name="fillsign-kind"
            checked={fillSignKind === "drawn"}
            onChange={() => setFillSignKind("drawn")}
          />
          Drawn
        </label>
        <label>
          <input
            type="radio"
            name="fillsign-kind"
            checked={fillSignKind === "typed"}
            onChange={() => setFillSignKind("typed")}
          />
          Typed
        </label>
      </fieldset>

      {fillSignKind === "typed" ? (
        <label>
          Your name
          <input
            type="text"
            value={fillSignText}
            placeholder="Type your name"
            onChange={(event) => setFillSignText(event.target.value)}
          />
        </label>
      ) : null}

      <button type="button" onClick={toggle} aria-pressed={fillSignMode}>
        {fillSignMode ? "Stop placing" : "Place a signature"}
      </button>

      {fillSignMode ? (
        <p className="muted">
          {loading
            ? "Measuring the page…"
            : usable > 0
              ? `Move over a line to see where the signature will land, then ${
                  fillSignKind === "typed" ? "click" : "draw"
                }. ${usable} line${usable === 1 ? "" : "s"} can be snapped to.`
              : // Honest about a weaker experience rather than silently degrading: with nothing to
                // snap to the mark still lands, it just lands where the pointer was.
                "No line could be measured on this page, so the signature will land where you " +
                "release rather than on a line."}
        </p>
      ) : null}

      {/*
        The one rule a user can break without noticing: a P=1/P=2 certification forbids annotation,
        and `add_annotation` refuses the placement. Saying so here means the refusal is expected
        rather than a failure the user has to interpret.
      */}
      {redactMode || textEditMode ? (
        <p className="muted">
          Turning this on stops the other page tools, so a drag can only mean one thing.
        </p>
      ) : null}
    </div>
  );
}
