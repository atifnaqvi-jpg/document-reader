/**
 * PDF <-> UI coordinate conversion, mirroring `src-tauri/src/editor/geometry.rs`.
 *
 * Kept as its own module for the same reason the Rust side has one: mixing the two
 * spaces is the classic PDF bug, and a single pair of functions is the only way to
 * guarantee the overlay draws exactly where the annotation will be written.
 *
 * PDF space: origin bottom-left, +y up, points (1/72"), unrotated page box.
 * UI space: origin top-left, +y down, CSS pixels, scaled by the viewer's zoom.
 */

/** A rectangle in PDF points. Normalised by the Rust side; callers here assume x0<x1. */
export interface PdfRect {
  x0: number;
  y0: number;
  x1: number;
  y1: number;
}

/** A rectangle in UI pixels (top-left origin). */
export interface UiRect {
  x: number;
  y: number;
  width: number;
  height: number;
}

/** Below this a rect is a dead click rather than a mark; mirrors `MIN_DIMENSION_POINTS`. */
export const MIN_DIMENSION_POINTS = 0.5;

/**
 * Side of a note's icon square, in PDF points; mirrors the 20-point icon the writer
 * builds in `editor/lopdf_write.rs`. A note stores one anchor point that the writer
 * expands up and right into this square, so the overlay must draw the same square --
 * otherwise the mark the user sees and the annotation written into the PDF disagree
 * about where the icon is.
 */
export const NOTE_ICON_POINTS = 20;

/**
 * Font size a free-text box is written at, in points; mirrors `FREE_TEXT_SIZE` in
 * `editor/annotate/appearance.rs`.
 *
 * The preview must multiply this by zoom. A fixed pixel size in the stylesheet made the
 * on-screen box a different annotation from the saved one at every zoom but 1.
 */
export const FREE_TEXT_SIZE_POINTS = 12;

/** Leading between lines, as a multiple of the font size; mirrors the writer's. */
export const FREE_TEXT_LEADING = 1.2;

/** Left/top inset of the text inside a free-text box, in points; mirrors the writer's. */
export const FREE_TEXT_INSET_POINTS = 2;

/**
 * UI pixels -> PDF points. `pageHeightPoints` is the unrotated page height.
 *
 * The y flip is `pageHeight - y` rather than `-y`: annotations are positioned within
 * the page box, so a bare negation would place everything off-page.
 */
export function pageToPdf(
  x0: number,
  y0: number,
  x1: number,
  y1: number,
  scale: number,
  pageHeightPoints: number,
): PdfRect {
  // A zero/negative scale would produce an infinite rect; fall back to 1:1 rather than
  // writing an unrenderable annotation.
  const safeScale = scale > 0 ? scale : 1;
  const toPoints = (value: number) => value / safeScale;
  const rect = {
    x0: toPoints(x0),
    y0: pageHeightPoints - toPoints(y1),
    x1: toPoints(x1),
    y1: pageHeightPoints - toPoints(y0),
  };
  return {
    x0: Math.min(rect.x0, rect.x1),
    y0: Math.min(rect.y0, rect.y1),
    x1: Math.max(rect.x0, rect.x1),
    y1: Math.max(rect.y0, rect.y1),
  };
}

/** PDF points -> UI pixels. Inverse of {@link pageToPdf}. */
export function pdfToPage(
  rect: PdfRect,
  scale: number,
  pageHeightPoints: number,
): UiRect {
  const safeScale = scale > 0 ? scale : 1;
  const x0 = Math.min(rect.x0, rect.x1);
  const y0 = Math.min(rect.y0, rect.y1);
  const x1 = Math.max(rect.x0, rect.x1);
  const y1 = Math.max(rect.y0, rect.y1);
  return {
    x: x0 * safeScale,
    y: (pageHeightPoints - y1) * safeScale,
    width: (x1 - x0) * safeScale,
    height: (y1 - y0) * safeScale,
  };
}

/** A single point (note anchors, ink vertices) from UI pixels to PDF points. */
export function pointToPdf(
  x: number,
  y: number,
  scale: number,
  pageHeightPoints: number,
): [number, number] {
  const safeScale = scale > 0 ? scale : 1;
  return [x / safeScale, pageHeightPoints - y / safeScale];
}

/**
 * Normalises a drag into a rect, so dragging up-left produces the same shape as
 * dragging down-right instead of a negative-size box.
 */
export function dragToRect(
  start: { x: number; y: number },
  end: { x: number; y: number },
): UiRect {
  return {
    x: Math.min(start.x, end.x),
    y: Math.min(start.y, end.y),
    width: Math.abs(end.x - start.x),
    height: Math.abs(end.y - start.y),
  };
}

/** Whether a UI rect is big enough to be a real mark rather than a stray click. */
export function isDrawable(rect: UiRect, scale: number): boolean {
  const safeScale = scale > 0 ? scale : 1;
  const min = MIN_DIMENSION_POINTS * safeScale;
  return rect.width >= min && rect.height >= min;
}

/**
 * The tools that sweep out a rectangle over text. Ink, notes and shapes carry their own
 * geometry and are not covered by the two functions below.
 */
export type SweepKind = "highlight" | "underline" | "strikeout" | "squiggly";

/** Thickness of the rule underline and strikeout draw, in UI pixels. */
export const MARK_RULE_THICKNESS = 2;

/** Peak deviation of the squiggly wave from its centre line, in UI pixels. */
export const SQUIGGLY_AMPLITUDE = 2;

/** Horizontal distance between squiggly crests, in UI pixels. */
export const SQUIGGLY_WAVELENGTH = 6;

/**
 * How tall a sweep mark is drawn inside its rect, in UI pixels.
 *
 * A highlight fills the swept area; underline and strikeout are rules *within* it, so
 * they stay a fixed thickness however tall the user's drag was.
 */
export function markHeight(kind: SweepKind, boxHeight: number): number {
  // A squiggly is a wave inside the box, so it needs vertical room to oscillate. It gets a
  // band at the bottom of the swept area rather than the full height: the wave marks the
  // baseline of the text, the same place an underline goes.
  if (kind === "squiggly") return SQUIGGLY_AMPLITUDE * 2 + MARK_RULE_THICKNESS;
  return kind === "highlight" ? boxHeight : MARK_RULE_THICKNESS;
}

/**
 * How far a sweep mark is pushed down inside its rect, in UI pixels.
 *
 * These three must agree with where the Rust writer puts the mark, so the drawn overlay
 * and the exported PDF show the same thing.
 *
 * An underline sits at the bottom of the swept area, a strikeout across its middle, and a
 * highlight is not offset at all. Letting the highlight share the rule tools' offset was a
 * real defect: it inherited `height - thickness`, which moved the fill down by its own
 * height so it landed below the text the user had swept.
 *
 * Kept here, away from the JSX, because a correct offset is arithmetic and can be checked
 * without a browser -- jsdom has no layout engine, so a test that asserted on rendered CSS
 * could not have caught that bug.
 */
export function markOffset(kind: SweepKind, boxHeight: number): number {
  if (kind === "highlight") return 0;
  if (kind === "squiggly") {
    return Math.max(0, boxHeight - markHeight("squiggly", boxHeight));
  }
  if (kind === "strikeout") return Math.max(0, boxHeight / 2 - MARK_RULE_THICKNESS / 2);
  return Math.max(0, boxHeight - MARK_RULE_THICKNESS);
}

/**
 * The polyline a squiggly draws, in UI pixels relative to its own box.
 *
 * A squiggly is a wave, and every other sweep kind is either a fill or a straight rule, so
 * this is the one mark that cannot be expressed as a styled `div`. Generated here rather
 * than in JSX so the shape is arithmetic a test can assert on -- jsdom has no layout, so a
 * rendered-SVG test could not tell a wave from a straight line.
 */
export function squigglyPolyline(width: number, height: number): string {
  if (width <= 0 || height <= 0) return "";
  const mid = height / 2;
  const points: string[] = [];
  const steps = Math.max(2, Math.round(width / SQUIGGLY_WAVELENGTH) * 2);
  for (let step = 0; step <= steps; step += 1) {
    const x = (width * step) / steps;
    // Alternate above and below the centre: crest at odd steps, trough at even ones.
    const y = step % 2 === 0 ? mid - SQUIGGLY_AMPLITUDE : mid + SQUIGGLY_AMPLITUDE;
    points.push(`${x},${y}`);
  }
  return points.join(" ");
}

/**
 * Expands a note's stored anchor point into the square the writer builds. A note is one
 * point on disk and a square on the page, so this is the only place that conversion lives.
 */
export function noteAnchorRect(point: [number, number] | undefined): PdfRect | null {
  if (!point) return null;
  const [x, y] = point;
  return { x0: x, y0: y, x1: x + NOTE_ICON_POINTS, y1: y + NOTE_ICON_POINTS };
}
