/**
 * D16: the opacity slider was live for thirteen tools and effective for one.
 *
 * `HighlightData` is the only annotation blob with an `opacity` field. `commitFreeText` builds
 * `{ v: 1, rect, color }`, the rect/oval branch does the same, and `annotate.rs` hardcodes
 * `opacity: 1.0` for rect and note -- so dragging the slider did nothing for any of them. The
 * control moved, which made it look effective; that is the same class of dishonesty as a stale
 * save banner.
 *
 * The fix hides the control where the value would go nowhere. Two things these guards pin:
 *
 *   1. Present for a kind that stores opacity, absent for kinds that do not -- named explicitly,
 *      so a future kind cannot silently inherit a live slider. If someone later gives `textbox`
 *      an opacity field, this test fails and forces a deliberate edit.
 *   2. The highlight path still carries opacity all the way into the stored data, so hiding the
 *      control elsewhere did not cost the one path that worked.
 */

import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { EditorPane, storesOpacity } from "./EditorPane";
import { EditorProvider } from "../state/EditorProvider";
import type { AnnotationKind, LibraryEntry } from "../api/types";
import { installFakeBackend } from "../test/tauri";
import { AnnotationOverlay } from "./AnnotationOverlay";

const entry: LibraryEntry = {
  id: "doc-1",
  title: "A Book",
  author: null,
  pages: 300,
  format: "pdf",
  path: "/books/a.pdf",
  missing: false,
  addedAt: 1,
  lastOpenedAt: 2,
  encrypted: false,
};

const TOOL_LABELS: Record<AnnotationKind, string> = {
  highlight: "Highlight",
  underline: "Underline",
  strikeout: "Strike out",
  squiggly: "Squiggly",
  note: "Note",
  textbox: "Text box",
  callout: "Callout",
  typewriter: "Typewriter",
  ink: "Ink",
  line: "Line",
  arrow: "Arrow",
  rect: "Box",
  oval: "Oval",
};

/**
 * The kinds whose blob carries opacity, written out rather than read from the component.
 *
 * Deliberately a second, independent statement of the same fact: if the component's list drifts,
 * these two disagree and the test says so. Deriving the expectation from the implementation
 * would make the assertion vacuous.
 */
const CARRIES_OPACITY: AnnotationKind[] = ["highlight", "underline", "strikeout", "squiggly"];

/** Kinds whose blob demonstrably has no opacity field. */
const NO_OPACITY: AnnotationKind[] = [
  "note",
  "textbox",
  "callout",
  "typewriter",
  "ink",
  "line",
  "arrow",
  "rect",
  "oval",
];

let open: { teardown(): void } | null = null;

afterEach(() => {
  open?.teardown();
  open = null;
});

function renderPane(appState: Record<string, string> = {}) {
  const backend = installFakeBackend();
  backend.on("list_annotations", () => []);
  backend.on("get_app_state", (args: unknown) => {
    const { key } = args as { key: string };
    return appState[key] ?? null;
  });
  backend.on("set_app_state", () => null);
  // The pane asks for redaction marks on mount; an unhandled command surfaces as an editor
  // error and blocks drawing, which would make the drag below fail for an unrelated reason.
  backend.on("redact_marks", () => []);
  backend.on("add_annotation", () => ({
    id: "a1",
    documentId: "doc-1",
    page: 0,
    kind: "highlight",
    data: "{}",
    text: null,
    createdAt: 1,
    modifiedAt: 1,
  }));

  render(
    <EditorProvider documentId="doc-1" page={0}>
      <EditorPane entry={entry} currentPage={0} />
      <AnnotationOverlay
        metrics={{ pageNumber: 0, width: 612, height: 792, pageHeightPoints: 792, scale: 1 }}
        annotations={[]}
      />
    </EditorProvider>,
  );
  open = backend;
  return backend;
}

const opacitySlider = () => screen.queryByLabelText("Opacity");

/**
 * jsdom does not implement `PointerEvent`, so `fireEvent.pointerDown(el, {clientX})` silently drops
 * the coordinates and the overlay reads `undefined - 0 = NaN`. `MouseEvent` does carry them, so the
 * events are dispatched as real `MouseEvent`s under the pointer type names. Same helper as
 * `FillSignSurface.test.tsx`, for the same reason.
 */
function pointer(target: HTMLElement, type: string, x: number, y: number) {
  const event = new MouseEvent(type, { clientX: x, clientY: y, bubbles: true, cancelable: true });
  Object.defineProperty(event, "pointerId", { value: 1 });
  fireEvent(target, event);
}

describe("D16: the opacity control is only where opacity is stored", () => {
  it("names the kinds that carry opacity, and the kind that does not", () => {
    // The capability itself, stated positively for the kind that works and negatively for the
    // kind the report was about.
    expect(storesOpacity("highlight")).toBe(true);
    expect(storesOpacity("textbox")).toBe(false);
    // No tool selected: nothing to style, so no slider.
    expect(storesOpacity(null)).toBe(false);

    // Every kind is covered by one of the two lists, so adding a fourteenth kind to
    // AnnotationKind without deciding here is a failure rather than an omission.
    const covered = [...CARRIES_OPACITY, ...NO_OPACITY].sort();
    expect(covered).toEqual(Object.keys(TOOL_LABELS).sort());
  });

  it("shows the control for highlight and hides it for every kind without the field", async () => {
    renderPane();

    for (const kind of CARRIES_OPACITY) {
      fireEvent.click(screen.getByRole("button", { name: TOOL_LABELS[kind] }));
      // The style loads asynchronously; wait for the panel, then require the slider.
      await waitFor(() => expect(screen.getByLabelText("Colour")).toBeTruthy());
      expect(opacitySlider(), `${kind} stores opacity and must offer the control`).not.toBeNull();
    }

    for (const kind of NO_OPACITY) {
      fireEvent.click(screen.getByRole("button", { name: TOOL_LABELS[kind] }));
      await waitFor(() => expect(screen.getByLabelText("Colour")).toBeTruthy());
      // Not merely disabled: a disabled slider still reads as "this could apply if...", so the
      // honest control is no control. The assertion is absence.
      expect(
        opacitySlider(),
        `${kind} has no opacity field, so the slider must not be shown`,
      ).toBeNull();
    }
  });

  it("does not show the control when no tool is active", () => {
    renderPane();
    expect(screen.queryByLabelText("Colour")).toBeNull();
    expect(opacitySlider()).toBeNull();
  });
});

describe("D16: the highlight path still stores what the slider sets", () => {
  it("carries a chosen opacity into the annotation data", async () => {
    const backend = renderPane();

    fireEvent.click(screen.getByRole("button", { name: "Highlight" }));
    await waitFor(() => expect(opacitySlider()).not.toBeNull());

    // 0.4 is the highlight default; 0.7 is a value the slider can reach and a change the stored
    // blob must reflect. Picking a non-default is the point -- asserting 0.4 would pass even if
    // the value were hardcoded.
    fireEvent.change(opacitySlider()!, { target: { value: "0.7" } });

    // Draw a highlight across the page. The overlay is the only element that receives these.
    const overlay = document.querySelector(".annotation-overlay") as HTMLElement;
    expect(overlay).not.toBeNull();
    pointer(overlay, "pointerdown", 20, 20);
    pointer(overlay, "pointermove", 120, 40);
    pointer(overlay, "pointerup", 120, 40);

    await waitFor(() => {
      const calls = backend.callsTo("add_annotation");
      expect(calls).toHaveLength(1);
      // The wire carries the blob as a JSON *string*, so it must be parsed to read a field --
      // asserting on the raw arg would compare against a string and pass on `undefined`.
      const args = calls[0] as unknown as { kind: string; data: string };
      expect(args.kind).toBe("highlight");
      const data = JSON.parse(args.data) as { opacity?: number; color?: string };
      // The value the user dragged to, not the default.
      expect(data.opacity).toBe(0.7);
      // And the colour still rides along, so the fix did not displace the neighbouring field.
      expect(typeof data.color).toBe("string");
    });
  });
});
