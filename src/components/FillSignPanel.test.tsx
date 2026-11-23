/**
 * The Fill & Sign control.
 *
 * The claim under test is not "a button renders". It is that this panel is *reachable and honest*:
 * it turns the placement layer on, it says what a visual signature is NOT before the user acts, and
 * it does not offer a cryptographic verdict the backend cannot back (I-F6).
 *
 * The panel is the only place in `src/` that can turn `fillSignMode` on, so these tests also pin
 * that the feature has a surface at all -- the defect was a backend path with no caller.
 */

import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { FillSignPanel } from "./FillSignPanel";
import { EditorProvider, useEditor } from "../state/EditorProvider";
import { installFakeBackend } from "../test/tauri";
import type { PageTextRun } from "../api/types";

const run: PageTextRun = {
  runId: { opIndex: 1, operandPath: [0] },
  text: "Familienname",
  operator: "Tj",
  fontKey: "F1",
  fontName: "Helvetica",
  fontSize: 6.96,
  rect: { x0: 72, y0: 497, x1: 200, y1: 505 },
  width: 128,
  admissibility: "admissible",
  pairConfidence: "strong",
  fragmentCount: 1,
} as unknown as PageTextRun;

let backend: ReturnType<typeof installFakeBackend>;

beforeEach(() => {
  backend = installFakeBackend();
  backend.on("get_page_text_runs", () => [run]);
  backend.on("get_app_state", () => null);
  backend.on("set_app_state", () => null);
});

afterEach(() => {
  backend.teardown();
});

/** Reports the mode so a test can assert the panel actually moved shared state. */
function ModeProbe() {
  const { fillSignMode, fillSignKind } = useEditor();
  return (
    <p data-testid="mode-probe">
      {fillSignMode ? "on" : "off"}:{fillSignKind}
    </p>
  );
}

function renderPanel(props: Partial<React.ComponentProps<typeof FillSignPanel>> = {}) {
  return render(
    <EditorProvider documentId="doc-1" page={0}>
      <FillSignPanel documentId="doc-1" page={0} {...props} />
      <ModeProbe />
    </EditorProvider>,
  );
}

describe("Fill & Sign is reachable", () => {
  it("starts off, so opening a document does not begin a signature", () => {
    renderPanel();
    // Placing a signature is a deliberate act; a surface already armed on open would make the
    // first stray drag on an arbitrary page a mark in the user's name.
    expect(screen.getByTestId("mode-probe")).toHaveTextContent("off:drawn");
  });

  it("turns the placement surface on when the user asks for it", async () => {
    renderPanel();
    await userEvent.click(screen.getByRole("button", { name: /place a signature/i }));
    expect(screen.getByTestId("mode-probe")).toHaveTextContent("on:drawn");
  });

  it("turns the surface back off, so a mode cannot be entered and never left", async () => {
    // Without this, a toggle that only ever sets `true` passes every other test here: the mode
    // would arm on first click and never disarm, leaving the page unable to take any other tool.
    renderPanel();
    const toggle = screen.getByRole("button", { name: /place a signature/i });

    await userEvent.click(toggle);
    expect(screen.getByTestId("mode-probe")).toHaveTextContent("on:drawn");

    await userEvent.click(screen.getByRole("button", { name: /stop placing/i }));
    expect(screen.getByTestId("mode-probe")).toHaveTextContent("off:drawn");
  });

  it("offers a typed signature and carries the text into shared state", async () => {
    renderPanel();
    await userEvent.click(screen.getByRole("radio", { name: /typed/i }));
    await userEvent.type(screen.getByLabelText(/your name/i), "A. Nachname");

    expect(screen.getByTestId("mode-probe")).toHaveTextContent("off:typed");
    // The surface reads the text from the provider, so a field that did not reach it would place
    // an empty signature or refuse.
    await userEvent.click(screen.getByRole("button", { name: /place a signature/i }));
    expect(screen.getByTestId("mode-probe")).toHaveTextContent("on:typed");
  });

  it("says it needs a document rather than showing a dead control", () => {
    render(
      <EditorProvider documentId={null} page={0}>
        <FillSignPanel documentId={null} page={0} />
      </EditorProvider>,
    );
    expect(screen.getByText(/open a document to sign/i)).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /place a signature/i })).toBeNull();
  });
});

describe("the panel is honest about what it produces", () => {
  it("says a visual signature is not a digital signature, before the user places one", () => {
    renderPanel();
    // D-N2 and I-F6 in the user's words. Asserted before any interaction because that is when it
    // matters: a disclosure shown after the mark is placed cannot inform the decision to place it.
    expect(screen.getByText(/not a digital signature/i)).toBeInTheDocument();
    expect(screen.getByText(/no cryptographic weight/i)).toBeInTheDocument();
  });

  it("never claims the mark is valid, verified or certified", () => {
    renderPanel();
    const text = document.body.textContent ?? "";
    for (const claim of ["valid signature", "verified", "certified", "legally binding"]) {
      expect(text.toLowerCase()).not.toContain(claim);
    }
  });

  it("reports how many lines can be snapped to, so assistance is not a surprise", async () => {
    renderPanel();
    await userEvent.click(screen.getByRole("button", { name: /place a signature/i }));
    expect(await screen.findByText(/1 line can be snapped to/i)).toBeInTheDocument();
  });

  it("says so plainly when nothing can be snapped to, instead of degrading silently", async () => {
    backend.on("get_page_text_runs", () => []);
    renderPanel();
    await userEvent.click(screen.getByRole("button", { name: /place a signature/i }));
    // D-N1's fallback stated as a limitation rather than a failure: placement still works.
    expect(await screen.findByText(/will land where you release/i)).toBeInTheDocument();
  });

  it("excludes a suspicious box from the count, because snapping would be wrong there", async () => {
    backend.on("get_page_text_runs", () => [{ ...run, boxSuspicious: true }]);
    renderPanel();
    await userEvent.click(screen.getByRole("button", { name: /place a signature/i }));
    expect(await screen.findByText(/will land where you release/i)).toBeInTheDocument();
  });
});
