import type { PageTextRun, TextRunId } from "../api/types";
import type { PdfRect } from "../state/geometry";

/**
 * Placement assistance for Fill & Sign (D-N1).
 *
 * ## Why this is a pure module
 *
 * Every decision here is a number, and the numbers came from a measurement that contradicted the
 * design's first guess (`docs/P3.3a-fill-and-sign-DESIGN.md` §3.2.1, §10.3). Keeping them in a
 * component would make the arithmetic this phase's correctness rests on reachable only through a
 * DOM, so it would be tested by proxy or not at all.
 *
 * ## The fact the arithmetic encodes
 *
 * A run's box bottom is **not** its baseline. The box is the *advance* box and extends to the
 * font's descent, so the bottom edge sits below the pen origin by the descender depth. D-N1
 * therefore places a signature at `box bottom + correction`, where the correction is a per-size
 * constant rather than per-run data.
 */

/**
 * The descender depth as a fraction of the em, **derived from this phase's own measurement**
 * rather than chosen.
 *
 * `tests/fillsign_facts.rs` measured `origin_y - box_bottom` on the LEA form: 1.462-1.524pt at
 * 6.96pt and 2.592pt at 12pt. Dividing those by their sizes gives 0.2101, 0.2190 and 0.2160. The
 * value below is the top of that range: taking the deepest observation means the correction never
 * places a mark *above* the line on a run whose box is deeper than the sample. It is consistent
 * with Helvetica's descent (~0.207 em), the face the form's own text uses.
 *
 * The 12pt sample also contained a run reporting **0.000** depth. That run's box already ends on
 * the baseline, and the wire cannot distinguish it from the others, which is why this constant is
 * bounded (see {@link baselineFor}) rather than subtracted blindly.
 */
export const DESCENT_EM = 0.216;

/**
 * The correction may never exceed one em. Beyond that the box is not describing a line of text at
 * this size, and the honest answer is to refuse rather than place a mark a line away.
 */
export const MAX_CORRECTION_EM = 1;

/** Which kind of signature the surface places. */
export type SignatureMode = "drawn" | "typed";

/**
 * A snap target resolved from a text run.
 *
 * `rect` is the band the mark occupies: the run's own horizontal extent, one em tall, sitting on
 * the resolved baseline. `baseline` is where a typed mark's text sits and where a drawn mark's
 * bottom edge lands, so both kinds share one geometry instead of two.
 */
export interface SnapTarget {
  rect: PdfRect;
  baseline: number;
  /** Which run was snapped to, so the surface can name it rather than only show a box. */
  runId: TextRunId;
  text: string;
}

/**
 * Whether a run may be used as a snap target at all.
 *
 * `boxSuspicious` is the wire's own warning (P2.4.2 T22): the paired box is not line-sized for its
 * own span, which is precisely when box geometry does not mean what a placement wants it to mean.
 * Reusing the app's existing distrust is deliberate -- a second heuristic here would let a run the
 * rest of the product rejects silently anchor a signature, and that exact shape put a
 * misalignment into 1.1.0.
 */
export function isUsableSnap(run: PageTextRun): boolean {
  if (!run.rect) return false;
  if (run.boxSuspicious) return false;
  if (!(run.fontSize > 0)) return false;
  // A zero-width box cannot be placed on; a sub-point box is a fragment, not a line.
  return Math.abs(run.rect.x1 - run.rect.x0) >= 1;
}

/**
 * The page-space baseline implied by a run's box, or `null` when the box cannot honestly imply one.
 *
 * Baseline = bottom edge + descender depth. The depth is `DESCENT_EM * fontSize`, capped at one em.
 *
 * Refusals, each deliberate:
 *
 * * a non-positive size has no em to take a fraction of;
 * * a non-finite coordinate is a corrupt run, not a placement;
 * * a box shorter than the correction would put the baseline above the box's own top edge, which
 *   means the box is not a line box at this size.
 */
export function baselineFor(run: PageTextRun): number | null {
  if (!run.rect) return null;
  if (!(run.fontSize > 0)) return null;
  if (!Number.isFinite(run.rect.y0) || !Number.isFinite(run.rect.y1)) return null;

  const height = run.rect.y1 - run.rect.y0;
  if (!(height > 0)) return null;

  const correction = Math.min(DESCENT_EM * run.fontSize, MAX_CORRECTION_EM * run.fontSize);
  const baseline = run.rect.y0 + correction;

  // The resolved baseline must stay inside the box it came from. If the correction overshoots the
  // top edge, the box is too short to be a line of this size, and refusing beats guessing.
  if (baseline > run.rect.y1) return null;
  return baseline;
}

/**
 * Resolves the snap target whose box contains the point, or the nearest one within `tolerance`.
 *
 * D-N1: snap only when the release point lies within a line height of a run, otherwise the mark
 * lands exactly where the user released. The caller supplies `tolerance` because a 6pt line and a
 * 12pt line must not share one threshold.
 *
 * A point inside a box always wins over a nearer edge of a box it is not inside: being on the line
 * is stronger evidence than being close to it.
 */
export function snapTargetAt(
  runs: readonly PageTextRun[],
  point: { x: number; y: number },
  tolerance: number,
): SnapTarget | null {
  let inside: { run: PageTextRun; distance: number } | null = null;
  let near: { run: PageTextRun; distance: number } | null = null;

  for (const run of runs) {
    if (!isUsableSnap(run)) continue;
    const baseline = baselineFor(run);
    if (baseline === null) continue;

    const rect = run.rect as PdfRect;
    const distance = distanceToRect(point, rect);
    if (distance === 0) {
      if (!inside) inside = { run, distance };
    } else if (distance <= tolerance) {
      if (!near || distance < near.distance) near = { run, distance };
    }
  }

  const chosen = inside ?? near;
  if (!chosen) return null;

  const rect = chosen.run.rect as PdfRect;
  const baseline = baselineFor(chosen.run) as number;

  return {
    rect: {
      x0: rect.x0,
      y0: baseline,
      x1: rect.x1,
      // One em tall, taken from the run's own size so a mark scaled to the line is not a
      // fixed-size sticker.
      y1: baseline + chosen.run.fontSize,
    },
    baseline,
    runId: chosen.run.runId,
    text: chosen.run.text,
  };
}

/** Euclidean distance from a point to a rect; zero inside it. */
function distanceToRect(point: { x: number; y: number }, rect: PdfRect): number {
  const x0 = Math.min(rect.x0, rect.x1);
  const x1 = Math.max(rect.x0, rect.x1);
  const y0 = Math.min(rect.y0, rect.y1);
  const y1 = Math.max(rect.y0, rect.y1);

  const dx = point.x < x0 ? x0 - point.x : point.x > x1 ? point.x - x1 : 0;
  const dy = point.y < y0 ? y0 - point.y : point.y > y1 ? point.y - y1 : 0;
  return Math.hypot(dx, dy);
}

/**
 * The row a placed drawn signature writes (D-N6).
 *
 * The flag is set **here**, by the surface, and never inferred by the backend: a stroke count is
 * not evidence of intent, and guessing wrong either labels an ordinary annotation as a signature
 * or leaves a real signature unlabelled. This flag is the single reason the feature did not exist
 * -- nothing in the app set it, so `is_visual_signature` was false for every user-created row.
 */
export function signatureInkData(
  strokes: [number, number][][],
  color: string,
): { v: 1; strokes: [number, number][][]; color: string; visualSignature: true } {
  return { v: 1, strokes, color, visualSignature: true };
}

/**
 * The typed counterpart of {@link signatureInkData}; `rect` is the band the text sits in.
 *
 * Emits the wire's own shape -- a `[x0, y0, x1, y1]` tuple, matching `FreeTextData.rect`, not the
 * `PdfRect` object the snapping code works in. Converting here rather than at the call site keeps
 * the one place a rectangle changes representation next to the test that pins it.
 */
export function signatureFreeTextData(
  rect: PdfRect,
  color: string,
): { v: 1; rect: [number, number, number, number]; color: string; visualSignature: true } {
  return { v: 1, rect: [rect.x0, rect.y0, rect.x1, rect.y1], color, visualSignature: true };
}
