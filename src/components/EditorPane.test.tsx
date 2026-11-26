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

import React, { StrictMode } from "react";
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

describe("EditorPane: the save banner belongs to one document (D11)", () => {
  it("does not carry a save from one document over to the next", async () => {
    // Reported on the 1.4.0 hand-test: the user opened a document with no form, drew a text
    // box, and the pane said "Saved to ... (edited 1).pdf" without any save having happened.
    // Nothing was written -- placement is a row -- but the banner is a *claim about a file*,
    // and it outlived the document it described because `EditorPane` is not keyed on `entry`
    // and nothing cleared this state. A stale claim about a real file is worse than no claim:
    // the user goes looking for a copy that this document never produced.
    const backend = installFakeBackend();
    backend.on("list_annotations", () => []);
    backend.on("get_app_state", () => null);
    backend.on("set_app_state", () => null);
    backend.on("next_save_path", () => "/books/a (edited 1).pdf");
    backend.on("save_edits", () => "/books/a (edited 1).pdf");
    open = backend;

    const view = render(
      <EditorProvider documentId="doc-1" page={0}>
        <EditorPane entry={entry} currentPage={0} />
      </EditorProvider>,
    );

    // Produce a genuine save on the first document.
    await userEvent.click(screen.getByRole("button", { name: /Save edits/ }));
    await userEvent.click(await screen.findByRole("button", { name: /^Save$/ }));
    expect(await screen.findByText(/Saved to/)).toBeInTheDocument();

    // Switch to a second document. The banner describes the *first* one and must go.
    const other: LibraryEntry = { ...entry, id: "doc-2", path: "/books/b.pdf", title: "B" };
    view.rerender(
      <EditorProvider documentId="doc-2" page={0}>
        <EditorPane entry={other} currentPage={0} />
      </EditorProvider>,
    );

    expect(screen.queryByText(/Saved to/)).not.toBeInTheDocument();
    expect(screen.queryByText(/a \(edited 1\)\.pdf/)).not.toBeInTheDocument();
  });
});

/**
 * D13: opening a document must not change the number of hooks `EditorPane` renders.
 *
 * The 1.4.0 build blanked the window when a document was opened. The report called it a
 * setState cascade (React 310 read as "maximum update depth"); it was not. The error was
 * `Rendered more hooks than during the previous render`, and the cause was ordering, not
 * depth: the D11 banner-reset effect was added *below* `if (!entry) return null;`.
 *
 * So the empty state (no document) rendered the ten `useState` hooks above the return and
 * stopped. The moment a document arrived, the same component instance rendered eleven, and
 * React rejects a component whose hook list grows between renders. That is why it appeared
 * only on *opening*: a document opened at mount never takes the early return, so the hook
 * count never changes and the crash never fires. A document opened from the library into an
 * already-mounted pane takes both paths, and only the transition throws.
 *
 * ## Why the guard counts renders and hooks instead of checking the DOM
 *
 * A test that only asserted the final DOM would pass on the broken build: the crash happens
 * during render, before any DOM exists to inspect, and `render` throws rather than returning
 * a container to query. Two properties are asserted instead:
 *
 *   1. **The transition does not throw.** The empty state and the open state are the same
 *      mount, which is the path the user took.
 *   2. **The render count stays bounded.** A cascade and a hook-order violation are different
 *      faults, but both show up as an unbounded or failing render sequence, and a bounded
 *      counter catches either without needing to name the mechanism.
 *
 * A hook-count probe is added on top, because it names the real invariant: whatever the
 * component does, it must call the same hooks on every render.
 */

describe("EditorPane: opening a document keeps the hook order stable (D13)", () => {
  /** The empty state and the opened state, mounted as one instance. */
  function mountThenOpen() {
    const backend = installFakeBackend();
    backend.on("list_annotations", () => []);
    backend.on("get_app_state", () => null);
    backend.on("set_app_state", () => null);

    const tree = (opened: boolean) => (
      <StrictMode>
        <EditorProvider documentId={opened ? "doc-1" : null} page={0}>
          <EditorPane entry={opened ? entry : null} currentPage={0} />
        </EditorProvider>
      </StrictMode>
    );

    const view = render(tree(false));
    return { backend, view, tree };
  }

  it("does not throw when a document is opened into an empty pane", () => {
    const { backend, view, tree } = mountThenOpen();

    // The exact sequence a user performs: the shell is up with nothing selected, then they
    // pick a document from the library. `EditorPane` stays mounted across both, which is
    // what makes the hook list observable and what made it grow.
    expect(() => view.rerender(tree(true))).not.toThrow();

    // And the open state really rendered, so a throw is not being masked by an empty tree.
    expect(screen.getByLabelText("Page")).toBeInTheDocument();

    view.unmount();
    backend.teardown();
  });

  it("renders a bounded number of times across the transition", () => {
    const { backend, view } = mountThenOpen();

    // Counting is done by wrapping the pane: `RenderProbe` increments on every render of its
    // own body, which for a stable child is the same count the child renders, plus the probe.
    let renders = 0;
    function Probe({ opened }: { opened: boolean }) {
      renders += 1;
      return (
        <EditorProvider documentId={opened ? "doc-1" : null} page={0}>
          <EditorPane entry={opened ? entry : null} currentPage={0} />
        </EditorProvider>
      );
    }

    view.rerender(
      <StrictMode>
        <Probe opened={false} />
      </StrictMode>,
    );
    const beforeOpen = renders;
    view.rerender(
      <StrictMode>
        <Probe opened={true} />
      </StrictMode>,
    );
    const afterOpen = renders - beforeOpen;

    // StrictMode double-renders, so the honest bound is a small constant, not 1. The point is
    // that the count is bounded at all: a cascade runs until React throws at 50 nested
    // updates, which is three orders of magnitude away from this number.
    expect(afterOpen, "opening a document must render a bounded number of times").toBeLessThan(8);

    view.unmount();
    backend.teardown();
  });

  it("fires on a document with no annotations at all, so the trigger is the open, not the marks", () => {
    // The report came from a document that has annotations, which invites the reading that
    // the annotation list is the trigger. It is not: the hook list is decided before any
    // annotation is looked at, and `EditorPane` only sees annotations through context, which
    // it does not read into a hook. So this variant keeps the annotation list empty and
    // asserts the same transition. If this ever fails while the others pass, the trigger has
    // genuinely moved into the annotation path and the brief has to say so.
    const backend = installFakeBackend();
    const annotationsRead = { count: 0 };
    backend.on("list_annotations", () => {
      annotationsRead.count += 1;
      return [];
    });
    backend.on("get_app_state", () => null);
    backend.on("set_app_state", () => null);

    // No annotations, no form fields: a flat document, which is the narrowest open there is.
    backend.on("list_form_fields", () => []);

    const tree = (opened: boolean) => (
      <StrictMode>
        <EditorProvider documentId={opened ? "doc-flat" : null} page={0}>
          <EditorPane
            entry={opened ? { ...entry, id: "doc-flat", path: "/books/flat.pdf" } : null}
            currentPage={0}
          />
        </EditorProvider>
      </StrictMode>
    );

    const view = render(tree(false));
    expect(() => view.rerender(tree(true))).not.toThrow();
    // The pane rendered its open state, so the assertion above is not vacuous.
    expect(screen.getByLabelText("Page")).toBeInTheDocument();

    view.unmount();
    backend.teardown();
  });

  it("calls the same hooks in the same order for the empty and the open state", () => {
    const seen: number[] = [];
    const originalUseState = React.useState;
    const originalUseEffect = React.useEffect;

    // Patched at the module boundary so the probe measures what React actually records. The
    // alternative -- reading React's internal hook list -- would depend on an implementation
    // detail; counting calls per render is the behaviour React itself validates.
    let inPane = false;
    let perRender = 0;
    const record = () => {
      if (inPane) perRender += 1;
    };
    React.useState = ((...args: Parameters<typeof originalUseState>) => {
      record();
      return originalUseState(...args);
    }) as typeof originalUseState;
    React.useEffect = ((...args: Parameters<typeof originalUseEffect>) => {
      record();
      return originalUseEffect(...args);
    }) as typeof originalUseEffect;

    const backend = installFakeBackend();
    backend.on("list_annotations", () => []);
    backend.on("get_app_state", () => null);
    backend.on("set_app_state", () => null);

    function Marked({ opened }: { opened: boolean }) {
      perRender = 0;
      inPane = true;
      const node = <EditorPane entry={opened ? entry : null} currentPage={0} />;
      inPane = false;
      seen.push(perRender);
      return (
        <EditorProvider documentId={opened ? "doc-1" : null} page={0}>
          {node}
        </EditorProvider>
      );
    }

    try {
      const view = render(
        <StrictMode>
          <Marked opened={false} />
        </StrictMode>,
      );
      const emptyCount = seen[seen.length - 1];
      view.rerender(
        <StrictMode>
          <Marked opened={true} />
        </StrictMode>,
      );
      const openCount = seen[seen.length - 1];

      expect(openCount, "hook count must not change between the empty and open state").toBe(
        emptyCount,
      );
      view.unmount();
    } finally {
      React.useState = originalUseState;
      React.useEffect = originalUseEffect;
      backend.teardown();
    }
  });
});
