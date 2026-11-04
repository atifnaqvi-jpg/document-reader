/**
 * The comment manager (D-A11/D-A7/D-A5), driven through a real `EditorProvider` and the
 * real `src/api/invoke.ts`, so the pane is exercised against the same command table the
 * app uses rather than against a stub of its own.
 *
 * The pane is a view over the DB, so these tests assert on what the backend was asked to
 * store, not on the pane's internal state: a pane that displayed a change it never wrote
 * is exactly the disagreement with the page that D-A11 exists to prevent.
 */

import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it } from "vitest";
import { CommentPane, threadAnnotations } from "./CommentPane";
import { EditorProvider } from "../state/EditorProvider";
import type { Annotation } from "../api/types";
import { installFakeBackend, makeAnnotation } from "../test/tauri";
import type { FakeBackend } from "../test/tauri";

let open: FakeBackend | null = null;

afterEach(() => {
  open?.teardown();
  open = null;
});

/** Renders the pane inside a provider backed by an in-memory table. */
function mountPane(initial: Annotation[], page = 0) {
  const backend = installFakeBackend();
  let table = initial.map((row) => ({ ...row }));
  let counter = initial.length;
  const jumps: number[] = [];

  backend.on("list_annotations", () => table.map((row) => ({ ...row })));
  backend.on("add_annotation", (args) => {
    counter += 1;
    const row = makeAnnotation({
      id: `new-${counter}`,
      page: args.page as number,
      kind: args.kind as Annotation["kind"],
      data: args.data as string,
      text: (args.text as string | null) ?? null,
    });
    table.push(row);
    return row;
  });
  backend.on("set_annotation_status", (args) => {
    table = table.map((row) => {
      if (row.id !== args.annotationId) return row;
      const blob = JSON.parse(row.data) as Record<string, unknown>;
      if (args.status === null) delete blob.status;
      else blob.status = args.status;
      return { ...row, data: JSON.stringify(blob) };
    });
    return table.find((row) => row.id === args.annotationId)!;
  });
  backend.on("remove_annotation", (args) => {
    table = table.filter((candidate) => candidate.id !== args.annotationId);
    return null;
  });
  backend.on("get_app_state", () => null);
  backend.on("set_app_state", () => null);

  render(
    <EditorProvider documentId="doc-1" page={page}>
      <CommentPane onJumpToPage={(page) => jumps.push(page)} />
    </EditorProvider>,
  );

  open = backend;
  return { backend, rows: () => table, jumps };
}

/** Waits for the pane's first `list_annotations` round trip to render. */
async function waitForRows(text: string) {
  await waitFor(() => expect(screen.getByText(text)).toBeTruthy());
}

describe("CommentPane: lists every annotation in the document (D-A11)", () => {
  it("shows rows from pages other than the one being viewed", async () => {
    // The pane is document-wide; the overlay draws one page. A pane that showed only the
    // current page would be a second copy of the viewer, not a comment manager.
    mountPane([
      makeAnnotation({ page: 0, text: "on the first page" }),
      makeAnnotation({ page: 4, text: "on the fifth page" }),
    ]);

    await waitForRows("on the first page");
    expect(screen.getByText("on the fifth page")).toBeTruthy();
  });

  it("jumps to the page a comment names", async () => {
    const { jumps } = mountPane([makeAnnotation({ page: 6, text: "far away" })]);
    await waitForRows("far away");

    await userEvent.click(screen.getByTitle("Go to page 7"));
    expect(jumps).toEqual([6]);
  });
});

describe("CommentPane: sorting (D-A11)", () => {
  it("sorts by page and reverses", async () => {
    mountPane([
      makeAnnotation({ page: 5, text: "later" }),
      makeAnnotation({ page: 1, text: "earlier" }),
    ]);
    await waitForRows("later");

    const texts = () =>
      screen.getAllByText(/^(later|earlier)$/).map((node) => node.textContent);
    expect(texts()).toEqual(["earlier", "later"]);

    await userEvent.click(screen.getByLabelText("Descending"));
    expect(texts()).toEqual(["later", "earlier"]);
  });

  it("sorts by status with untriaged last", async () => {
    // "No status" is not a status: an untriaged row sorts after the four real ones rather
    // than before them, so a reviewer sees their triage first.
    mountPane([
      makeAnnotation({ text: "untriaged" }),
      makeAnnotation({
        text: "completed one",
        data: JSON.stringify({ v: 2, point: [1, 1], status: "completed" }),
      }),
      makeAnnotation({
        text: "accepted one",
        data: JSON.stringify({ v: 2, point: [1, 1], status: "accepted" }),
      }),
    ]);
    await waitForRows("untriaged");

    await userEvent.selectOptions(screen.getByLabelText("Sort"), "status");
    const texts = screen
      .getAllByText(/^(untriaged|completed one|accepted one)$/)
      .map((node) => node.textContent);
    expect(texts).toEqual(["accepted one", "completed one", "untriaged"]);
  });
});

describe("CommentPane: filters (D-A11)", () => {
  it("filters by status, including the untriaged case", async () => {
    mountPane([
      makeAnnotation({ text: "plain" }),
      makeAnnotation({
        text: "rejected one",
        data: JSON.stringify({ v: 2, point: [1, 1], status: "rejected" }),
      }),
    ]);
    await waitForRows("plain");

    await userEvent.selectOptions(screen.getByLabelText("Status"), "none");
    expect(screen.getByText("plain")).toBeTruthy();
    expect(screen.queryByText("rejected one")).toBeNull();

    await userEvent.selectOptions(screen.getByLabelText("Status"), "rejected");
    expect(screen.queryByText("plain")).toBeNull();
    expect(screen.getByText("rejected one")).toBeTruthy();
  });

  it("filters by type", async () => {
    mountPane([
      makeAnnotation({ text: "a highlight", kind: "highlight" }),
      makeAnnotation({ text: "an ink stroke", kind: "ink" }),
    ]);
    await waitForRows("a highlight");

    await userEvent.selectOptions(screen.getByLabelText("Type"), "ink");
    expect(screen.queryByText("a highlight")).toBeNull();
    expect(screen.getByText("an ink stroke")).toBeTruthy();
  });

  it("filters by text, case-insensitively", async () => {
    mountPane([
      makeAnnotation({ text: "Mentions Reflow" }),
      makeAnnotation({ text: "something else" }),
    ]);
    await waitForRows("Mentions Reflow");

    await userEvent.type(screen.getByLabelText("Search"), "reflow");
    expect(screen.getByText("Mentions Reflow")).toBeTruthy();
    expect(screen.queryByText("something else")).toBeNull();
  });

  it("says so when nothing matches rather than showing an empty list", async () => {
    mountPane([makeAnnotation({ text: "only one" })]);
    await waitForRows("only one");

    await userEvent.type(screen.getByLabelText("Search"), "no such text");
    expect(screen.getByText("No comment matches these filters.")).toBeTruthy();
  });
});

describe("CommentPane: editing a status writes through (D-A7)", () => {
  it("sends the chosen status and shows it after the refresh", async () => {
    const { backend, rows } = mountPane([makeAnnotation({ text: "triage me" })]);
    await waitForRows("triage me");

    const row = screen.getByText("triage me").closest("li") as HTMLElement;
    await userEvent.selectOptions(within(row).getByRole("combobox"), "accepted");

    await waitFor(() =>
      expect(JSON.parse(rows()[0]!.data).status).toBe("accepted"),
    );
    expect(backend.callsTo("set_annotation_status")[0]!.status).toBe("accepted");
  });

  it("clears a status back to untriaged", async () => {
    const { rows } = mountPane([
      makeAnnotation({
        text: "already triaged",
        data: JSON.stringify({ v: 2, point: [1, 1], status: "cancelled" }),
      }),
    ]);
    await waitForRows("already triaged");

    const row = screen.getByText("already triaged").closest("li") as HTMLElement;
    await userEvent.selectOptions(within(row).getByRole("combobox"), "");

    await waitFor(() => expect(JSON.parse(rows()[0]!.data).status).toBeUndefined());
  });
});

describe("CommentPane: replies render as threads (D-A5)", () => {
  it("nests a reply under the annotation it answers", async () => {
    const parent = makeAnnotation({ text: "the original" });
    const reply = makeAnnotation({
      text: "the answer",
      data: JSON.stringify({ v: 2, point: [1, 1], parent: parent.id }),
    });
    mountPane([parent, reply]);
    await waitForRows("the original");

    const rows = screen.getAllByRole("listitem");
    expect(rows).toHaveLength(2);
    // Indentation is the visual thread; the order asserts the reply follows its parent.
    expect(rows[0]!.textContent).toContain("the original");
    expect(rows[1]!.textContent).toContain("the answer");
    expect(rows[1]!.style.marginLeft).not.toBe("");
  });

  it("keeps a reply visible when its parent is not in the list", async () => {
    // D-A5 §5: a reply must not vanish because its parent is gone. Hiding the user's text
    // is worse than showing it at the root.
    mountPane([
      makeAnnotation({
        text: "orphaned answer",
        data: JSON.stringify({ v: 2, point: [1, 1], parent: "not-in-this-document" }),
      }),
    ]);

    await waitForRows("orphaned answer");
  });

  it("adds a reply to the thread when the Reply box is used", async () => {
    const { rows } = mountPane([makeAnnotation({ text: "the original" })]);
    await waitForRows("the original");

    const row = screen.getByText("the original").closest("li") as HTMLElement;
    await userEvent.click(within(row).getByText("Reply"));
    await userEvent.type(within(row).getByRole("textbox"), "an answer");
    await userEvent.click(within(row).getByText("Send"));

    await waitFor(() => expect(rows()).toHaveLength(2));
    const stored = rows()[1]!;
    expect(stored.text).toBe("an answer");
    expect(JSON.parse(stored.data).parent).toBe(rows()[0]!.id);
  });
});

describe("threadAnnotations", () => {
  it("does not loop on a blob whose parent points at itself", () => {
    // A hand-edited blob can contain a cycle. The grouping must terminate: a `while` that
    // walked parents would hang the pane, which is a whole-window freeze, not a bad row.
    const self = makeAnnotation({ text: "self" });
    self.data = JSON.stringify({ v: 2, point: [1, 1], parent: self.id });

    const threads = threadAnnotations([self]);
    expect(threads).toHaveLength(1);
    expect(threads[0]!.root.id).toBe(self.id);
  });

  it("groups a reply under the row whose id it names", () => {
    const parent = makeAnnotation({ text: "parent" });
    const child = makeAnnotation({
      text: "child",
      data: JSON.stringify({ v: 2, point: [1, 1], parent: parent.id }),
    });

    const threads = threadAnnotations([parent, child]);
    expect(threads).toHaveLength(1);
    expect(threads[0]!.root.id).toBe(parent.id);
    expect(threads[0]!.nested.get(parent.id)?.map((row) => row.id)).toEqual([child.id]);
  });
});
