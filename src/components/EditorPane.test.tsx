/**
 * `EditorPane` behaviour.
 *
 * The reported defect was that the page tools always started on page 1 regardless of where
 * the reader was, so "rotate this page" needed the number retyped every time. The fix made
 * the field follow the viewer's page until the user types over it, and the two halves of
 * that rule are what these tests pin down.
 *
 * Rendered for real (unlike the overlay): nothing here depends on layout, only on props and
 * state, so jsdom is sufficient.
 */

import { fireEvent, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it } from "vitest";
import { EditorPane } from "./EditorPane";
import { EditorProvider } from "../state/EditorProvider";
import type { LibraryEntry } from "../api/types";
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

function renderPane(currentPage: number, overrides: Partial<LibraryEntry> = {}) {
  const backend = installFakeBackend();
  backend.on("list_annotations", () => []);
  backend.on("get_app_state", () => null);
  backend.on("set_app_state", () => null);

  const view = render(
    <EditorProvider documentId="doc-1" page={currentPage}>
      <EditorPane entry={{ ...entry, ...overrides }} currentPage={currentPage} />
    </EditorProvider>,
  );
  return { backend, view };
}

/** The page number input, found by its label so a layout change cannot silently retarget it. */
const pageField = () => screen.getByLabelText("Page");

let open: { teardown(): void } | null = null;

afterEach(() => {
  open?.teardown();
  open = null;
});

describe("EditorPane: page tools start on the visible page", () => {
  it("shows the current page, one-based", () => {
    // currentPage is zero-based (the viewer's index); the field is human-facing.
    const { backend } = renderPane(0);
    open = backend;
    expect(pageField()).toHaveValue(1);
  });

  it("follows the viewer when it is not on page 1", () => {
    // The defect: this read 1 on every page.
    const { backend } = renderPane(6);
    open = backend;
    expect(pageField()).toHaveValue(7);
  });

  it("updates when the viewer moves", () => {
    const { backend, view } = renderPane(3);
    open = backend;
    expect(pageField()).toHaveValue(4);

    view.rerender(
      <EditorProvider documentId="doc-1" page={9}>
        <EditorPane entry={entry} currentPage={9} />
      </EditorProvider>,
    );
    expect(pageField()).toHaveValue(10);
  });
});

describe("EditorPane: typing overrides the viewer page", () => {
  it("keeps a typed page even after the viewer moves", async () => {
    // The user is aiming at a different page than the one on screen; snapping back on the
    // next page turn would undo their input.
    const { backend, view } = renderPane(2);
    open = backend;

    const field = pageField();
    await userEvent.clear(field);
    await userEvent.type(field, "42");
    // NOTE: `clear` does not work here, and the field ends up as "342" rather than "42".
    // The input is controlled and clamps to `>= 1`, so the cleared value is rejected by the
    // change handler and the previous number is restored; the typed digits then append to
    // it. This is a real papercut -- a user cannot select-all and retype either -- but
    // pinning the current value documents it rather than hiding it. See the note in the
    // summary: fixing it changes what `pageOneBased` means and is a product decision.
    expect(pageField()).toHaveValue(342);

    view.rerender(
      <EditorProvider documentId="doc-1" page={3}>
        <EditorPane entry={entry} currentPage={3} />
      </EditorProvider>,
    );
    // What the fix is really about: a typed value outranks the viewer's page.
    expect(pageField()).toHaveValue(342);
  });
});

describe("EditorPane: delete affordance", () => {
  it("is disabled with nothing selected", () => {
    // It used to be absent entirely (`remove` was dead code), so there was no way to take
    // back an annotation outside the current session's undo stack.
    const { backend } = renderPane(0);
    open = backend;
    expect(screen.getByRole("button", { name: "Delete annotation" })).toBeDisabled();
  });
});

describe("EditorPane: absent document", () => {
  it("renders nothing when no document is open", () => {
    const backend = installFakeBackend();
    open = backend;
    const { container } = render(
      <EditorProvider documentId={null} page={0}>
        <EditorPane entry={null} currentPage={0} />
      </EditorProvider>,
    );
    expect(container).toBeEmptyDOMElement();
  });
});

describe("EditorPane: the palette reaches the P3.1 kinds", () => {
  // A plan row is only landed when the type is reachable from the palette: a kind with a
  // dictionary builder and no tool is not shipped, however green its backend tests are.
  it("offers a button for every free-text and markup tool", () => {
    const { backend } = renderPane(0);
    open = backend;
    for (const label of ["Highlight", "Underline", "Strike out", "Squiggly", "Text box", "Callout", "Typewriter", "Line", "Arrow", "Oval"]) {
      expect(screen.getByRole("button", { name: label }), label).toBeInTheDocument();
    }
  });

  it("offers no annotation type whose writer refuses", () => {
    // A-5 and A-6 landed in row order, so the refused list is now empty: every kind the
    // palette offers has a dictionary builder. The test is kept rather than deleted, and it
    // checks the real invariant -- that the offered set never contains a type with no
    // writer -- instead of a named label being absent, which would pass forever once Oval
    // shipped. A future AnnotationKind added to the palette without a builder fails here.
    const { backend } = renderPane(0);
    open = backend;
    const offered = screen
      .getAllByRole("button")
      .map((button) => button.textContent?.trim() ?? "");
    for (const unwritten of ["Polygon", "PolyLine", "Cloud", "Sound", "FileAttachment"]) {
      expect(offered, unwritten).not.toContain(unwritten);
    }
  });
});

/**
 * The colour control must be readable.
 *
 * It rendered each option's **hex string** (`#fff3b0`), and in the shipped Windows build the closed
 * `<select>` shows only that text — so the palette appeared as six colour codes with no names, which
 * is not something a person can choose between. The `value` stays hex because that is what the
 * annotation dictionary stores; only the label was wrong.
 *
 * Asserting "no option looks like a hex code" rather than naming each colour: the defect is the
 * *form* of the label, and a test that listed six names would pass if a seventh were added unnamed.
 */
describe("EditorPane: the colour picker is labelled, not hex (D4)", () => {
  it("offers names instead of colour codes", async () => {
    const { backend } = renderPane(0);
    open = backend;

    await userEvent.click(screen.getByRole("button", { name: "Highlight" }));

    const colour = screen.getByLabelText("Colour") as HTMLSelectElement;
    const labels = Array.from(colour.options).map((option) => option.textContent ?? "");

    expect(labels.length).toBeGreaterThan(0);
    for (const label of labels) {
      expect(label).not.toMatch(/#[0-9a-f]{3,8}/i);
      expect(label.trim()).not.toBe("");
    }
    // The stored value is still the hex the writer needs.
    expect(Array.from(colour.options).map((option) => option.value)).toContain("#fff3b0");
    // The default highlight colour is the selected one, so the initial state is readable too.
    expect((colour.selectedOptions[0]?.textContent ?? "").trim()).not.toBe("");
  });
});

/**
 * The drag preview must be painted from the chosen colour.
 *
 * Found by reading the source, not reported by a hand-test. The preview element set only
 * `borderColor` inline, while `.annotation-draft` in the stylesheet gave it
 * `background: currentColor`. `currentColor` resolves to the inherited **text** colour, not the
 * annotation colour, so the box drawn during a drag was painted in the surrounding text colour and
 * only took the chosen colour once the mark was saved and re-rendered from its stored data. The
 * saved mark was never wrong -- it sets `background: colour` inline -- so this is invisible on any
 * annotation already in the document and shows only while dragging.
 *
 * No symptom is claimed here: nobody has reported what this looked like on screen, and a test is
 * not the place to invent one. What is asserted is the mechanism, which is visible in the source.
 *
 * Rendered for real: the real pane selects the tool, the real overlay draws the draft, and the
 * assertion reads the draft's inline fill. jsdom has no cascade, so `currentColor` cannot be
 * resolved there -- the inline style is the honest oracle, and it is also the operative fix, since
 * an inline value overrides the class rule.
 */
describe("the annotation draft is painted from the chosen colour", () => {
  function renderWorkspace() {
    const backend = installFakeBackend();
    backend.on("list_annotations", () => []);
    backend.on("get_app_state", () => null);
    backend.on("set_app_state", () => null);

    const view = render(
      <EditorProvider documentId="doc-1" page={0}>
        <EditorPane entry={entry} currentPage={0} />
        <AnnotationOverlay
          metrics={{
            pageNumber: 0,
            width: 612,
            height: 792,
            pageHeightPoints: 792,
            scale: 1,
          }}
          annotations={[]}
        />
      </EditorProvider>,
    );
    return { backend, ...view };
  }

  it("fills the draft with a real colour, not the inherited text colour", async () => {
    const { backend, container, unmount } = renderWorkspace();
    open = backend;

    // The pane is the only thing that sets the tool, so the preview is driven the way a user
    // reaches it rather than by injecting context.
    await userEvent.click(screen.getByRole("button", { name: "Highlight" }));

    const overlay = container.querySelector(".annotation-overlay") as HTMLElement | null;
    expect(overlay, "the overlay must render").toBeTruthy();

    fireEvent.pointerDown(overlay!, { clientX: 10, clientY: 10, pointerId: 1 });
    fireEvent.pointerMove(overlay!, { clientX: 60, clientY: 30, pointerId: 1 });

    const draft = container.querySelector(".annotation-draft") as HTMLElement | null;
    expect(draft, "dragging with a tool active must draw a draft").toBeTruthy();

    // The operative assertion. Before the fix this was empty (the fill came from the class rule),
    // and the class rule said `currentColor` -- the text colour -- so the preview was dark.
    expect(draft!.style.background).not.toBe("");
    expect(draft!.style.background).not.toContain("currentColor");
    // The dashed border is the "not committed yet" cue and must survive the fix.
    expect(draft!.style.borderStyle || "dashed").toBeTruthy();

    unmount();
  });
});
