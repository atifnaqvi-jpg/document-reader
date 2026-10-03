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
