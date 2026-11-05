/**
 * The search pane's hit list (P2-D5/D5c, P3.1 D-A12).
 *
 * Unlike the comment pane, this one does not need a provider: the pane is a pure view over
 * `search()` plus a callback, and the callback is exactly what D-A12 is about -- a hit must
 * resolve to *page and annotation*, not just page.
 *
 * The backend is faked at the IPC boundary through the shared helper, so the pane is driven
 * through the real `src/api/invoke.ts`.
 */

import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it } from "vitest";
import { SearchPane } from "./SearchPane";
import type { LibraryEntry, SearchHit } from "../api/types";
import { installFakeBackend } from "../test/tauri";
import type { FakeBackend } from "../test/tauri";

let open: FakeBackend | null = null;

afterEach(() => {
  open?.teardown();
  open = null;
});

const ENTRY: LibraryEntry = {
  id: "doc-1",
  title: "A book",
  path: "C:/a.pdf",
  format: "pdf",
  addedAt: 0,
  missing: false,
  lastOpenedAt: null,
  pages: 10,
  author: null,
  encrypted: false,
};

function mount(hits: SearchHit[]) {
  const backend = installFakeBackend();
  const opened: { page: number; annotationId: string | null }[] = [];
  backend.on("search", () => hits);
  render(
    <SearchPane
      entries={[ENTRY]}
      onOpenHit={(entry, page, annotationId) => {
        opened.push({ page, annotationId });
        void entry;
      }}
    />,
  );
  return { backend, opened };
}

describe("SearchPane", () => {
  it("resolves an annotation hit to both its page and its annotation id", async () => {
    // D-A12: the hit carries `annotation_id` so the click can select the row, not just
    // scroll to the page. Dropping the id here would still look right -- the viewer would
    // jump to the correct page -- which is exactly why it needs an assertion rather than a
    // read of the click handler.
    const { opened } = mount([
      {
        documentId: "doc-1",
        page: 4,
        snippet: "a [needle] here",
        origin: "annotation",
        annotationId: "ann-7",
      },
    ]);

    await userEvent.type(screen.getByRole("searchbox"), "needle");
    const hit = await screen.findByRole("button", { name: /needle/ });
    await userEvent.click(hit);

    await waitFor(() => expect(opened).toHaveLength(1));
    expect(opened[0]).toEqual({ page: 4, annotationId: "ann-7" });
  });

  it("passes no annotation id for a document hit", async () => {
    // The null case is asserted too: a document hit must not acquire an id, or the viewer
    // would try to select an annotation that does not exist.
    const { opened } = mount([
      {
        documentId: "doc-1",
        page: 2,
        snippet: "the [needle] is here",
        origin: "document",
        annotationId: null,
      },
    ]);

    await userEvent.type(screen.getByRole("searchbox"), "needle");
    await userEvent.click(await screen.findByRole("button", { name: /needle/ }));

    await waitFor(() => expect(opened).toHaveLength(1));
    expect(opened[0]).toEqual({ page: 2, annotationId: null });
  });
});
