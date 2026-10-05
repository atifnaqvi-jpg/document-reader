/**
 * Geometry tests.
 *
 * The first block mirrors `src-tauri/src/editor/geometry.rs` case for case. Those two
 * modules exist to agree: the overlay draws from the TS conversion and the writer places
 * the mark from the Rust one, so a divergence puts the mark the user sees somewhere other
 * than the mark in the exported PDF. The Rust side was tested; this side was trusted.
 *
 * The mark block covers the offset rules that used to live in `AnnotationOverlay`'s JSX.
 * jsdom has no layout engine, so testing those through the DOM would only ever have
 * observed zeroes -- as pure functions they are checkable, including the highlight bug.
 */

import { describe, expect, it } from "vitest";
import {
  MARK_RULE_THICKNESS,
  NOTE_ICON_POINTS,
  dragToRect,
  isDrawable,
  markHeight,
  markOffset,
  noteAnchorRect,
  pageToPdf,
  pdfToPage,
  pointToPdf,
} from "../state/geometry";
import type { PdfRect } from "../state/geometry";

/** A4-ish page in points, matching the Rust fixture. */
const PAGE_H = 842;

describe("geometry: parity with src-tauri/src/editor/geometry.rs", () => {
  it("maps the top of the page to a high PDF y", () => {
    // Rust: top_of_the_page_maps_to_a_high_pdf_y
    const rect = pageToPdf(0, 10, 100, 30, 1, PAGE_H);
    expect(rect.y1).toBeCloseTo(PAGE_H - 10, 3);
    expect(rect.y0).toBeCloseTo(PAGE_H - 30, 3);
    expect(rect.y0).toBeLessThan(rect.y1);
  });

  it("maps the bottom of the page to a low PDF y", () => {
    // Rust: bottom_of_the_page_maps_to_a_low_pdf_y
    const rect = pageToPdf(0, PAGE_H - 30, 100, PAGE_H - 10, 1, PAGE_H);
    expect(rect.y0).toBeLessThan(30);
    expect(rect.y1).toBeLessThan(30.001);
  });

  it("divides zoom out", () => {
    // Rust: zoom_is_divided_out -- the same visual mark at 2x has double the pixel
    // coordinates but must produce the same page-space rect.
    const at1x = pageToPdf(20, 40, 120, 60, 1, PAGE_H);
    const at2x = pageToPdf(40, 80, 240, 120, 2, PAGE_H);
    expect(at2x).toEqual(at1x);
  });

  it("round-trips through both spaces at every scale", () => {
    // Rust: round_trips_through_both_spaces
    const original: PdfRect = { x0: 40, y0: 100, x1: 300, y1: 140 };
    for (const scale of [0.5, 1, 1.5, 3]) {
      const ui = pdfToPage(original, scale, PAGE_H);
      const back = pageToPdf(ui.x, ui.y, ui.x + ui.width, ui.y + ui.height, scale, PAGE_H);
      expect(back.x0, `x0 at scale ${scale}`).toBeCloseTo(original.x0, 3);
      expect(back.y0, `y0 at scale ${scale}`).toBeCloseTo(original.y0, 3);
      expect(back.x1, `x1 at scale ${scale}`).toBeCloseTo(original.x1, 3);
      expect(back.y1, `y1 at scale ${scale}`).toBeCloseTo(original.y1, 3);
    }
  });

  it("normalises a rect dragged up and to the left", () => {
    // Rust: reversed_input_rect_is_normalised
    const rect = pageToPdf(300, 140, 40, 100, 1, PAGE_H);
    expect(rect.x0).toBeLessThan(rect.x1);
    expect(rect.y0).toBeLessThan(rect.y1);
    expect(rect.x0).toBeCloseTo(40, 3);
    expect(rect.x1).toBeCloseTo(300, 3);
  });

  it("keeps a point and a zero-size rect in agreement", () => {
    const [px, py] = pointToPdf(50, 60, 2, PAGE_H);
    const rect = pageToPdf(50, 60, 50, 60, 2, PAGE_H);
    expect(px).toBeCloseTo(rect.x0, 3);
    expect(py).toBeCloseTo(rect.y0, 3);
  });
});

describe("geometry: scale guards", () => {
  it("falls back to 1:1 rather than emitting an infinite rect", () => {
    expect(pageToPdf(0, 0, 10, 10, 0, PAGE_H)).toEqual(pageToPdf(0, 0, 10, 10, 1, PAGE_H));
    expect(pageToPdf(0, 0, 10, 10, -2, PAGE_H)).toEqual(pageToPdf(0, 0, 10, 10, 1, PAGE_H));
  });
});

describe("geometry: dragToRect", () => {
  it("produces the same shape whichever way the drag went", () => {
    const downRight = dragToRect({ x: 10, y: 20 }, { x: 60, y: 80 });
    const upLeft = dragToRect({ x: 60, y: 80 }, { x: 10, y: 20 });
    expect(upLeft).toEqual(downRight);
    expect(downRight).toEqual({ x: 10, y: 20, width: 50, height: 60 });
  });
});

describe("geometry: isDrawable", () => {
  it("rejects a stray click but accepts a real sweep", () => {
    const tiny = dragToRect({ x: 5, y: 5 }, { x: 5.1, y: 5.1 });
    const real = dragToRect({ x: 5, y: 5 }, { x: 60, y: 40 });
    expect(isDrawable(tiny, 1)).toBe(false);
    expect(isDrawable(real, 1)).toBe(true);
  });

  it("scales the minimum with zoom, so the threshold is constant on screen", () => {
    // The floor is MIN_DIMENSION_POINTS in *page* terms, and a UI rect of n pixels is n/scale
    // points. So a 1px drag is 1pt at 1x but only 1/3pt at 3x -- zooming in makes a tiny
    // physical drag cover less page, and it must eventually stop counting as a sweep.
    const rect = dragToRect({ x: 0, y: 0 }, { x: 1, y: 1 });
    expect(isDrawable(rect, 1)).toBe(true); // 1pt
    expect(isDrawable(rect, 2)).toBe(true); // 0.5pt, exactly at the floor
    expect(isDrawable(rect, 3)).toBe(false); // 0.33pt, below it
  });
});

describe("geometry: sweep marks", () => {
  // Regression cover for the reported defect: a highlight inherited the rule tools'
  // `height - thickness` offset, so it was drawn one full height below the swept text.
  it("does not offset a highlight", () => {
    for (const height of [1, 12, 30, 120]) {
      expect(markOffset("highlight", height), `height ${height}`).toBe(0);
    }
  });

  it("fills the swept area with a highlight", () => {
    expect(markHeight("highlight", 37)).toBe(37);
  });

  it("puts an underline at the bottom of the swept area", () => {
    expect(markOffset("underline", 30)).toBe(30 - MARK_RULE_THICKNESS);
    // The rule must finish inside its box, or it bleeds onto the line below.
    expect(markOffset("underline", 30) + markHeight("underline", 30)).toBeLessThanOrEqual(30);
  });

  it("puts a strikeout across the middle of the swept area", () => {
    const height = 40;
    const top = markOffset("strikeout", height);
    // Centred: equal space above and below the rule.
    expect(top).toBeCloseTo((height - MARK_RULE_THICKNESS) / 2, 3);
    expect(height - (top + MARK_RULE_THICKNESS)).toBeCloseTo(top, 3);
  });

  it("keeps the rule tools a fixed thickness however tall the drag was", () => {
    // A rule that scaled with the drag would be unusable on a tall selection.
    expect(markHeight("underline", 200)).toBe(MARK_RULE_THICKNESS);
    expect(markHeight("strikeout", 4)).toBe(MARK_RULE_THICKNESS);
  });

  it("never draws a mark outside its box, even for a very short drag", () => {
    for (const kind of ["highlight", "underline", "strikeout"] as const) {
      for (const height of [0, 0.5, 1, 2, 3]) {
        const top = markOffset(kind, height);
        const drawn = markHeight(kind, height);
        expect(top, `${kind} at height ${height}`).toBeGreaterThanOrEqual(0);
        expect(top + drawn, `${kind} at height ${height}`).toBeLessThanOrEqual(
          Math.max(height, drawn),
        );
      }
    }
  });
});

describe("geometry: note anchors", () => {
  it("expands the stored point into the writer's square", () => {
    expect(noteAnchorRect([10, 20])).toEqual({
      x0: 10,
      y0: 20,
      x1: 10 + NOTE_ICON_POINTS,
      y1: 20 + NOTE_ICON_POINTS,
    });
  });

  it("returns null rather than guessing when the point is absent", () => {
    expect(noteAnchorRect(undefined)).toBeNull();
  });
});
