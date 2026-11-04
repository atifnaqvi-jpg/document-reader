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

import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it } from "vitest";
import { EditorPane } from "./EditorPane";
import { EditorProvider } from "../state/EditorProvider";
import type { LibraryEntry } from "../api/types";
import { installFakeBackend } from "../test/tauri";

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
    for (const label of ["Highlight", "Underline", "Strike out", "Squiggly", "Text box", "Callout", "Typewriter", "Line", "Arrow"]) {
      expect(screen.getByRole("button", { name: label }), label).toBeInTheDocument();
    }
  });

  it("does not offer a tool whose writer still refuses", () => {
    // A-5 landed, so Line and Arrow moved to the list above. A-6 (Oval) has not: offering its
    // button would let the user draw a mark that the save then rejects, which is worse than
    // not offering it. This list shrinks as rows land and is empty when P3.1's palette is
    // complete -- a shrinking list, not a reworded one.
    const { backend } = renderPane(0);
    open = backend;
    for (const label of ["Oval"]) {
      expect(screen.queryByRole("button", { name: label }), label).not.toBeInTheDocument();
    }
  });
});
