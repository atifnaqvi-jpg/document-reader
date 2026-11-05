/**
 * The free-text preview must draw at the size the file will draw (A-11).
 *
 * Reported against 1.1.0: "the text written in the note box is misaligned so that it gets
 * hidden". Two independent causes were measured; this file covers the live one.
 *
 * The stylesheet set `font-size: 12px` on the free-text mark, and the writer sets 12 *points*.
 * Points become pixels through zoom, so the preview only agreed with the file at scale 1. At
 * zoom 2 the on-screen text was half the size the file would draw, and at a box sized for one
 * line the preview fitted text the file would overflow -- the text sat at a different place
 * in the box than the page would put it.
 *
 * jsdom has no layout engine, so these assert the *inputs* to layout: the inline style the
 * component computes. That is the same reasoning as `geometry.test.ts` -- the numbers the
 * browser is asked to lay out are checkable even where the layout is not.
 */

import { render } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { AnnotationOverlay } from "./AnnotationOverlay";
import type { PageMetrics } from "./AnnotationOverlay";
import { FREE_TEXT_INSET_POINTS, FREE_TEXT_LEADING, FREE_TEXT_SIZE_POINTS } from "../state/geometry";
import type { Annotation } from "../api/types";
import { EditorProvider } from "../state/EditorProvider";
import { installFakeBackend, makeAnnotation } from "../test/tauri";

/** Rendered-page metrics for a 200x60pt box at `scale`, as the viewer would report them. */
function metricsAt(scale: number): PageMetrics {
  const widthPoints = 612;
  const heightPoints = 792;
  return {
    pageNumber: 0,
    width: widthPoints * scale,
    height: heightPoints * scale,
    pageHeightPoints: heightPoints,
    scale,
  };
}

/** A textbox at the top-left of the page, 200pt wide and 60pt tall. */
function textbox(): Annotation {
  return makeAnnotation({
    id: "ft-1",
    page: 0,
    kind: "textbox",
    data: JSON.stringify({ v: 1, rect: [0, 732, 200, 792], color: "#000000" }),
    text: "first line\nsecond line",
  });
}

/** Renders the overlay and returns the free-text mark element. */
function renderMark(scale: number) {
  const backend = installFakeBackend();
  backend.on("list_annotations", () => []);
  backend.on("get_app_state", () => null);
  backend.on("set_app_state", () => null);

  const { container, unmount } = render(
    <EditorProvider documentId="doc-1" page={0}>
      <AnnotationOverlay metrics={metricsAt(scale)} annotations={[textbox()]} />
    </EditorProvider>,
  );
  const mark = container.querySelector(".kind-free-text") as HTMLElement | null;
  expect(mark, "the free-text mark must render").toBeTruthy();
  backend.teardown();
  return { mark: mark!, unmount };
}

describe("free-text preview: draws at the size the file draws (A-11)", () => {
  it("scales the font with zoom, exactly as the writer's points do", () => {
    for (const scale of [0.5, 1, 2, 3]) {
      const { mark, unmount } = renderMark(scale);
      const expected = `${FREE_TEXT_SIZE_POINTS * scale}px`;
      // A fixed 12px passes at scale 1 and is wrong at every other zoom, which is why the
      // original defect survived a suite that only ever rendered at one zoom.
      expect(mark.style.fontSize, `font size at scale ${scale}`).toBe(expected);
      unmount();
    }
  });

  it("scales the leading with the font, not independently", () => {
    const { mark, unmount } = renderMark(2);
    const fontSize = parseFloat(mark.style.fontSize);
    const expected = fontSize * FREE_TEXT_LEADING;
    expect(parseFloat(mark.style.lineHeight)).toBeCloseTo(expected, 3);
    unmount();
  });

  it("insets by the writer's points, not by a fixed pixel padding", () => {
    const { mark, unmount } = renderMark(2);
    expect(parseFloat(mark.style.padding)).toBeCloseTo(FREE_TEXT_INSET_POINTS * 2, 3);
    unmount();
  });

  it("keeps the box at the rect's size so the frame matches the saved annotation", () => {
    const { mark, unmount } = renderMark(2);
    // 200x60pt at scale 2 is 400x120px, which is what the file's /Rect describes.
    expect(mark.style.width).toBe("400px");
    expect(mark.style.height).toBe("120px");
    unmount();
  });
});
