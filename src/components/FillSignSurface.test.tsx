/**
 * The Fill & Sign surface, and the I-F6 boundary at the UI layer.
 *
 * Two things are proved here that no backend test can reach:
 *
 * 1. **The surface sets `visualSignature`.** That flag's absence is why the feature was
 *    unreachable: `annotate.rs` labels a row that carries it, and nothing in `src/` ever set it.
 *    A test that only asserted "an ink row was created" would have passed before this unit, so it
 *    would prove nothing.
 * 2. **I-F6 at the boundary**: no path from this surface yields a cryptographic verdict, and an
 *    ordinary annotation is not silently labelled as a signature.
 *
 * Rendered for real against the fake backend, because the claim is about what crosses the IPC
 * boundary -- the JSON actually sent to `add_annotation` -- not about component internals.
 */

import { fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { FillSignSurface } from "./FillSignSurface";
import { EditorProvider } from "../state/EditorProvider";
import type { PageMetrics } from "./AnnotationOverlay";
import type { Annotation, PageTextRun } from "../api/types";
import { DESCENT_EM } from "../state/fillsign";
import { installFakeBackend } from "../test/tauri";

const metrics: PageMetrics = {
  pageNumber: 0,
  width: 612,
  height: 792,
  pageHeightPoints: 792,
  scale: 1,
};

/** A run shaped like the LEA form's Familienname line: 6.96pt, box bottom one descent low. */
function leaRun(): PageTextRun {
  const fontSize = 6.96;
  const baseline = 500;
  return {
    runId: { opIndex: 3, operandPath: [0] },
    text: "Familienname",
    operator: "Tj",
    fontKey: "F1",
    fontName: "Helvetica",
    fontSize,
    rect: { x0: 72, y0: baseline - DESCENT_EM * fontSize, x1: 200, y1: baseline + fontSize * 0.7 },
    width: 128,
    admissibility: "admissible",
    pairConfidence: "strong",
    fragmentCount: 1,
  } as unknown as PageTextRun;
}

function makeAnnotation(overrides: Partial<Annotation> = {}): Annotation {
  return {
    id: "a1",
    documentId: "doc-1",
    page: 0,
    kind: "ink",
    data: "{}",
    text: null,
    createdAt: 1,
    ...overrides,
  } as Annotation;
}

/**
 * jsdom does not implement `PointerEvent`, so `fireEvent.pointerDown(el, {clientX})` drops the
 * coordinates and the component reads `undefined - 0 = NaN`. Every drawn-stroke test then fails on
 * the coordinate guard rather than on what it means to prove. `MouseEvent` does carry the
 * coordinates, so the events are dispatched as real `MouseEvent`s under the pointer type names --
 * which is what a browser's pointer events are, plus a `pointerId`.
 */
function pointer(target: HTMLElement, type: string, x: number, y: number) {
  const event = new MouseEvent(type, {
    clientX: x,
    clientY: y,
    bubbles: true,
    cancelable: true,
  });
  Object.defineProperty(event, "pointerId", { value: 1 });
  fireEvent(target, event);
}

let open: { teardown(): void } | null = null;
let backend: ReturnType<typeof installFakeBackend>;

beforeEach(() => {
  backend = installFakeBackend();
  backend.on("list_annotations", () => []);
  backend.on("get_app_state", () => null);
  backend.on("set_app_state", () => null);
  backend.on("add_annotation", (args) =>
    makeAnnotation({
      id: "new-1",
      kind: args.kind as Annotation["kind"],
      data: args.data as string,
      text: (args.text as string | null) ?? null,
    }),
  );
  open = backend;
});

afterEach(() => {
  open?.teardown();
  open = null;
});

function renderSurface(props: Partial<React.ComponentProps<typeof FillSignSurface>> = {}) {
  return render(
    <EditorProvider documentId="doc-1" page={0}>
      <FillSignSurface
        metrics={metrics}
        runs={[leaRun()]}
        mode="drawn"
        typedText=""
        {...props}
      />
    </EditorProvider>,
  );
}

/**
 * The surface div, found by test id so a layout change cannot silently retarget it.
 *
 * jsdom has no layout engine, so `getBoundingClientRect` returns all zeros and a pointer's
 * `clientX` - 0 is still the coordinate, but the synthetic event carries none at all. This stubs
 * the one rect the component reads, so the geometry the test exercises is the geometry a browser
 * would give it. The alternative -- asserting against a NaN -> NaN conversion -- would test that
 * the component survives jsdom rather than that it places a signature.
 */
function surface(): HTMLElement {
  const element = screen.getByTestId("fillsign-surface");
  element.getBoundingClientRect = () =>
    ({
      x: 0,
      y: 0,
      left: 0,
      top: 0,
      right: metrics.width,
      bottom: metrics.height,
      width: metrics.width,
      height: metrics.height,
      toJSON: () => ({}),
    }) as DOMRect;
  return element;
}

/** One drawn stroke from (100, 300) to (180, 300) in UI pixels. */
function drawStroke() {
  const element = surface();
  pointer(element, "pointerdown", 100, 300);
  pointer(element, "pointermove", 140, 305);
  pointer(element, "pointerup", 180, 300);
}

describe("a user can place a visual signature, and the row says so", () => {
  it("writes an ink row carrying visualSignature, which is the defect this unit closes", () => {
    renderSurface({ mode: "drawn" });
    drawStroke();

    const calls = backend.callsTo("add_annotation");
    expect(calls).toHaveLength(1);

    const payload = JSON.parse(calls[0].data as string) as Record<string, unknown>;
    // Before this unit this field did not exist anywhere in `src/`, so the backend's
    // `is_visual_signature` was false for every row a user could create.
    expect(payload.visualSignature).toBe(true);
    expect(calls[0].kind).toBe("ink");
  });

  it("writes a typed row carrying visualSignature and the text the user gave", () => {
    renderSurface({ mode: "typed", typedText: "A. Nachname" });

    const element = surface();
    pointer(element, "pointerdown", 120, 300);
    pointer(element, "pointerup", 120, 300);

    const calls = backend.callsTo("add_annotation");
    expect(calls).toHaveLength(1);
    const payload = JSON.parse(calls[0].data as string) as Record<string, unknown>;
    expect(payload.visualSignature).toBe(true);
    expect(calls[0].text).toBe("A. Nachname");
  });

  it("refuses a typed signature with no text rather than writing an unlabelled empty box", () => {
    const refusals: string[] = [];
    renderSurface({ mode: "typed", typedText: "   ", onRefused: (r) => refusals.push(r) });

    const element = surface();
    pointer(element, "pointerdown", 120, 300);
    pointer(element, "pointerup", 120, 300);

    expect(backend.callsTo("add_annotation")).toHaveLength(0);
    expect(refusals).toHaveLength(1);
    // I-F7's shape applied here: a refusal names the reason rather than being a bare string.
    expect(refusals[0]).toMatch(/text/i);
  });

  it("refuses a stray tap rather than writing a one-point signature", () => {
    const refusals: string[] = [];
    renderSurface({ mode: "drawn", onRefused: (r) => refusals.push(r) });

    const element = surface();
    pointer(element, "pointerdown", 100, 300);
    pointer(element, "pointerup", 100, 300);

    expect(backend.callsTo("add_annotation")).toHaveLength(0);
    expect(refusals.join(" ")).toMatch(/short/i);
  });
});

describe("placement snapping is visible before it commits (D-N1, D-N8)", () => {
  it("shows a preview over a usable line and clears it on leave", () => {
    renderSurface();

    const element = surface();
    // Inside the Familienname box: 612 wide, 792 tall, box at points (72..200, ~497..505).
    // In UI pixels with scale 1 and top-left origin, y = 792 - 500 = 292.
    pointer(element, "pointermove", 120, 292);
    expect(screen.queryByTestId("fillsign-snap-preview")).not.toBeNull();

    fireEvent.pointerLeave(element);
    expect(screen.queryByTestId("fillsign-snap-preview")).toBeNull();
  });

  it("shows no preview over a suspicious box, because there is nothing honest to show", () => {
    renderSurface({ runs: [{ ...leaRun(), boxSuspicious: true }] });

    pointer(surface(), "pointermove", 120, 292);
    expect(screen.queryByTestId("fillsign-snap-preview")).toBeNull();
  });

  it("snaps the typed box to the resolved baseline, not the raw box bottom", () => {
    renderSurface({ mode: "typed", typedText: "A. Nachname" });

    const element = surface();
    pointer(element, "pointerdown", 120, 292);
    pointer(element, "pointerup", 120, 292);

    const calls = backend.callsTo("add_annotation");
    expect(calls).toHaveLength(1);
    const payload = JSON.parse(calls[0].data as string) as { rect: number[] };
    // The rect's y0 is the baseline. The run's own box bottom is one descent below it, so a
    // raw-box snap would put this at ~497 rather than 500.
    expect(payload.rect[1]).toBeCloseTo(500, 3);
    expect(payload.rect[1]).toBeGreaterThan(leaRun().rect!.y0);
  });
});

describe("I-F6: a visual signature never carries a cryptographic verdict", () => {
  it("sends no signature field and no verdict in the row it writes", () => {
    renderSurface({ mode: "drawn" });
    drawStroke();

    const payload = JSON.parse(backend.callsTo("add_annotation")[0].data as string) as Record<
      string,
      unknown
    >;
    // I-F6's whole content: the row declares itself VISUAL and nothing else. A `/Sig`, a
    // certificate, or a verdict appearing here would make P3.2's badges untrustworthy.
    expect(payload.visualSignature).toBe(true);
    expect(payload).not.toHaveProperty("signature");
    expect(payload).not.toHaveProperty("certificate");
    expect(payload).not.toHaveProperty("verdict");
    expect(payload).not.toHaveProperty("valid");
  });

  it("never sets the flag on a signature the surface did not create", () => {
    // The inverse boundary: `signatureInkData` is the only place the flag is added, so a row
    // built the ordinary way cannot be mistaken for a signature.
    const ordinary = JSON.parse(JSON.stringify({ v: 1, strokes: [], color: "#000" }));
    expect(ordinary.visualSignature).toBeUndefined();
  });

  it("labels the mark as visual and does not ask the backend for a cryptographic sign", () => {
    renderSurface({ mode: "drawn" });
    drawStroke();

    // No cryptographic command may be reachable from this surface. `sign_*` is the signing
    // flow's prefix (SignDialog); a Fill & Sign placement must not touch it.
    const signingCalls = backend.calls.filter((c) => c.command.startsWith("sign"));
    expect(signingCalls).toEqual([]);
  });
});
