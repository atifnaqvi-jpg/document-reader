/**
 * D14: removing a document from the library, exercised through the real shell.
 *
 * The pane's own tests cover the affordance and the confirm step. What they cannot cover is
 * the transition the user actually experiences: the open document disappearing out from under
 * the viewer while its panes are mounted. That is where a removal becomes a crash -- every
 * pane below `EditorProvider` reads `selected`, and a null document has to land in the empty
 * state rather than in a throw or a stale banner.
 *
 * Mounted through `Root` (StrictMode, boundary, providers) because a throw would be caught by
 * the boundary and rendered as a panel. Asserting the panel is absent is therefore the
 * assertion that the transition did not throw.
 */

import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { Root } from "./root";
import { installFakeBackend, type FakeBackend } from "./test/tauri";
import type { LibraryEntry } from "./api/types";

let backend: FakeBackend | null = null;

afterEach(() => {
  backend?.teardown();
  backend = null;
});

const pdf = (id: string, title: string): LibraryEntry => ({
  id,
  title,
  author: null,
  pages: 3,
  format: "pdf",
  path: `/books/${id}.pdf`,
  missing: false,
  addedAt: 1,
  lastOpenedAt: 2,
  encrypted: false,
});

/**
 * A shell holding two documents, where `list_library` answers from a mutable list so the
 * removal is observable exactly as the real backend's would be.
 */
function shellWithTwoDocuments(): { fake: FakeBackend; library: LibraryEntry[] } {
  const library = [pdf("lea", "LEA"), pdf("maastricht", "Maastricht")];
  const fake = installFakeBackend();
  fake.on("list_library", () => [...library]);
  fake.on("get_app_state", () => null);
  fake.on("set_app_state", () => null);
  fake.on("plugin:window|is_fullscreen", () => false);
  fake.on("list_annotations", () => []);
  fake.on("list_form_fields", () => []);
  fake.on("get_document_page_count", () => 3);
  fake.on("get_document_page_render", () => ({
    page: 0,
    width: 612,
    height: 792,
    pngBase64: "",
  }));
  // The removal itself: drop the row, and report the id and path the real command returns.
  fake.on("forget_document", (args: unknown) => {
    const { documentId } = args as { documentId: string };
    const index = library.findIndex((e) => e.id === documentId);
    if (index === -1) return null;
    const [forgotten] = library.splice(index, 1);
    return { id: forgotten.id, path: forgotten.path };
  });
  return { fake, library };
}

describe("App: removing a document from the library (D14)", () => {
  it("returns the viewer to the empty state when the open document is removed", async () => {
    const shell = shellWithTwoDocuments();
    backend = shell.fake;

    render(<Root />);

    // Open the document the way a user does, so the viewer and its panes are really mounted.
    fireEvent.click(await screen.findByText("LEA"));
    await waitFor(() => expect(screen.getByLabelText("Page")).toBeInTheDocument());

    // Remove that same document.
    fireEvent.click(screen.getByRole("button", { name: "Remove LEA from the library" }));
    fireEvent.click(screen.getByRole("button", { name: "Remove from library" }));

    // The document's controls are gone, because there is no document...
    await waitFor(() => expect(screen.queryByLabelText("Page")).toBeNull());
    // ...the library row is gone...
    expect(screen.queryByText("LEA")).toBeNull();
    // ...the other document is untouched...
    expect(screen.getByText("Maastricht")).toBeInTheDocument();
    // ...and the removal did not throw. A throw would have been caught by the boundary and
    // rendered as its panel, replacing the entire window.
    expect(screen.queryByTestId("render-error-panel")).toBeNull();
  });

  it("keeps the open document open when a different one is removed", async () => {
    const shell = shellWithTwoDocuments();
    backend = shell.fake;

    render(<Root />);

    fireEvent.click(await screen.findByText("LEA"));
    await waitFor(() => expect(screen.getByLabelText("Page")).toBeInTheDocument());

    // Remove the *other* document. This is the case a naive `setSelected(null)` would break:
    // clearing unconditionally would close the document the user is reading to no purpose.
    fireEvent.click(
      screen.getByRole("button", { name: "Remove Maastricht from the library" }),
    );
    fireEvent.click(screen.getByRole("button", { name: "Remove from library" }));

    await waitFor(() => expect(screen.queryByText("Maastricht")).toBeNull());
    // Still open, still the same document. The page tools only exist for an open document,
    // and its row is still selected -- checked on the button's class rather than by title
    // text, which appears both in the row and in the viewer.
    expect(screen.getByLabelText("Page")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /^LEA/ })).toHaveClass("selected");
    expect(screen.queryByTestId("render-error-panel")).toBeNull();
  });
});
