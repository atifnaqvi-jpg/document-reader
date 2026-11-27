/**
 * `LibraryPane` removal affordance (D14).
 *
 * Two properties are pinned here, and they pull against each other on purpose:
 *
 *   1. every row has a remove control, reachable by pointer and by keyboard;
 *   2. selecting a row still opens it.
 *
 * The second exists because the cheap way to add a remove control -- putting it inside the
 * row's own button, or making the row a clickable `div` with a nested button -- makes one of the
 * two gestures hijack the other. A test that only asserted the control's presence would pass on
 * a row that no longer opens, so the interaction is exercised rather than the markup.
 *
 * The confirm step is tested for its *content*, not just its presence: the ruling is that the
 * dialog must say the file stays on disk, and a dialog that merely asks "are you sure?" would
 * satisfy a presence check while failing the requirement the user actually stated.
 */

import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { LibraryPane } from "./LibraryPane";
import type { LibraryEntry } from "../api/types";

function entry(overrides: Partial<LibraryEntry> = {}): LibraryEntry {
  return {
    id: "doc-1",
    title: "LEA",
    author: null,
    pages: 3,
    format: "pdf",
    path: "/books/lea.pdf",
    missing: false,
    addedAt: 1,
    lastOpenedAt: 2,
    encrypted: false,
    ...overrides,
  };
}

/** The remove control for one document, found by its accessible name. */
const removeControl = (title: string) =>
  screen.getByRole("button", { name: `Remove ${title} from the library` });

describe("LibraryPane: remove controls", () => {
  it("gives every row a remove control, not just the first", () => {
    const entries = [entry({ id: "a", title: "Alpha" }), entry({ id: "b", title: "Beta" })];
    render(
      <LibraryPane
        entries={entries}
        selectedId={null}
        onSelect={() => {}}
        onForget={() => Promise.resolve()}
      />,
    );

    // Each control is found by the document's own name, so a shared label would fail here --
    // and a shared label is exactly what a screen reader would announce forty times.
    expect(removeControl("Alpha")).toBeInTheDocument();
    expect(removeControl("Beta")).toBeInTheDocument();
    expect(screen.getAllByRole("button")).toHaveLength(4);
  });

  it("is keyboard reachable and labelled with the document it removes", () => {
    render(
      <LibraryPane
        entries={[entry()]}
        selectedId={null}
        onSelect={() => {}}
        onForget={() => Promise.resolve()}
      />,
    );

    const control = removeControl("LEA");
    // A real `button`, so Tab reaches it and Enter/Space activate it without a key handler.
    expect(control.tagName).toBe("BUTTON");
    expect(control).toHaveAttribute("type", "button");
    control.focus();
    expect(control).toHaveFocus();
  });

  it("pressing the remove control does not also select the row", () => {
    const onSelect = vi.fn();
    render(
      <LibraryPane
        entries={[entry()]}
        selectedId={null}
        onSelect={onSelect}
        onForget={() => Promise.resolve()}
      />,
    );

    fireEvent.click(removeControl("LEA"));

    // This is requirement 1 in its sharpest form: the two gestures must be separable. Removing
    // a document the user did not mean to open must not open it first.
    expect(onSelect).not.toHaveBeenCalled();
    // And the click reached the confirm step rather than being swallowed.
    expect(screen.getByRole("dialog")).toBeInTheDocument();
  });

  it("still opens a row when its title button is clicked", () => {
    const onSelect = vi.fn();
    render(
      <LibraryPane
        entries={[entry()]}
        selectedId={null}
        onSelect={onSelect}
        onForget={() => Promise.resolve()}
      />,
    );

    fireEvent.click(screen.getByText("LEA"));

    expect(onSelect).toHaveBeenCalledTimes(1);
    expect(onSelect).toHaveBeenCalledWith(expect.objectContaining({ id: "doc-1" }));
    // Selection must not open the dialog: the two actions are independent.
    expect(screen.queryByRole("dialog")).toBeNull();
  });
});

describe("LibraryPane: the confirm step", () => {
  it("names the document and says the file stays on disk", () => {
    render(
      <LibraryPane
        entries={[entry()]}
        selectedId={null}
        onSelect={() => {}}
        onForget={() => Promise.resolve()}
      />,
    );

    fireEvent.click(removeControl("LEA"));

    const dialog = screen.getByRole("dialog");
    // Named, and the path shown, so the user confirms a specific file.
    expect(dialog).toHaveTextContent("LEA");
    expect(dialog).toHaveTextContent("/books/lea.pdf");
    // The statement the ruling requires, asserted by its meaning rather than exact wording:
    // the words "stays on disk" are the promise, so they are what is checked.
    expect(dialog).toHaveTextContent(/stays on disk/i);
    expect(dialog).toHaveTextContent(/not deleted, moved or changed/i);
    // And the cost, stated rather than hidden.
    expect(dialog).toHaveTextContent(/annotations that have not been saved/i);
  });

  it("warns that a missing file can only be taken off the list", () => {
    render(
      <LibraryPane
        entries={[entry({ missing: true })]}
        selectedId={null}
        onSelect={() => {}}
        onForget={() => Promise.resolve()}
      />,
    );

    fireEvent.click(removeControl("LEA"));

    // Requirement 3 is about the row being removable; this is the same case told to the user,
    // so they are not warned about losing a file that is already gone.
    expect(screen.getByRole("dialog")).toHaveTextContent(/cannot find this file/i);
  });

  it("does nothing until confirmed, and cancelling leaves the row alone", () => {
    const onForget = vi.fn(() => Promise.resolve());
    render(
      <LibraryPane
        entries={[entry()]}
        selectedId={null}
        onSelect={() => {}}
        onForget={onForget}
      />,
    );

    fireEvent.click(removeControl("LEA"));
    // Opening the dialog is not the removal. This is the whole reason the step exists.
    expect(onForget).not.toHaveBeenCalled();

    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));

    expect(onForget).not.toHaveBeenCalled();
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(screen.getByText("LEA")).toBeInTheDocument();
  });

  it("calls back with the document and closes on confirm", async () => {
    const onForget = vi.fn(() => Promise.resolve());
    render(
      <LibraryPane
        entries={[entry(), entry({ id: "doc-2", title: "Other" })]}
        selectedId={null}
        onSelect={() => {}}
        onForget={onForget}
      />,
    );

    fireEvent.click(removeControl("Other"));
    fireEvent.click(screen.getByRole("button", { name: "Remove from library" }));

    expect(onForget).toHaveBeenCalledTimes(1);
    // The document the dialog was opened for, not the first in the list.
    expect(onForget).toHaveBeenCalledWith(expect.objectContaining({ id: "doc-2" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  });

  it("keeps the dialog open and shows the reason when removal fails", async () => {
    const onForget = vi.fn(() =>
      Promise.reject({ code: "OTHER", message: "database is locked" }),
    );
    render(
      <LibraryPane
        entries={[entry()]}
        selectedId={null}
        onSelect={() => {}}
        onForget={onForget}
      />,
    );

    fireEvent.click(removeControl("LEA"));
    fireEvent.click(screen.getByRole("button", { name: "Remove from library" }));

    // A failed removal must not look like a successful one, so the step stays put rather than
    // closing and leaving the user to guess.
    expect(await screen.findByRole("alert")).toHaveTextContent("database is locked");
    // Scoped to the dialog: "LEA" is in the row as well, and the point is that the *dialog*
    // still names it while reporting the failure.
    expect(screen.getByRole("dialog")).toHaveTextContent("LEA");
  });
});
