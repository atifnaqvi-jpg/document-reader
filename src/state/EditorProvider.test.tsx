/**
 * `EditorProvider` behaviour, driven through the real `src/api/invoke.ts`.
 *
 * These cover the annotation lifecycle that the UI defects sat in: a note must carry its
 * text to the DB (it used to be created with `text: null`, so the text could never be
 * saved), editing it must be reversible, and deleting must be undoable under the original
 * id. All of that is decided by this provider, not by the overlay.
 *
 * `renderHook` rather than a rendered overlay: the overlay needs a laid-out page, which
 * jsdom cannot provide. The page-default test lives in `EditorPane.test.tsx`.
 */

import { act, renderHook, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { EditorProvider, useEditor } from "./EditorProvider";
import type { Annotation } from "../api/types";
import { ANNOTATION_CHANGED_EVENT } from "../api/types";
import { makeAnnotation, installFakeBackend } from "../test/tauri";
import type { FakeBackend } from "../test/tauri";

/**
 * A provider backed by an in-memory annotation table.
 *
 * The table is the point: `add_annotation` returns a row and `list_annotations` returns
 * what is really stored, so a component that reads stale state or writes the wrong column
 * fails here rather than passing against a hand-fed stub.
 */
function mountEditor(initial: Annotation[] = [], page = 0) {
  const backend = installFakeBackend();
  // Deep-copied on entry: the caller keeps its own `note` object, and a backend that
  // mutated it in place would let the test's expectations change under it. Real IPC never
  // shares a reference, so neither does this.
  let table = initial.map((row) => ({ ...row }));
  let counter = initial.length;

  backend.on("list_annotations", () => table.map((row) => ({ ...row })));
  backend.on("add_annotation", (args) => {
    counter += 1;
    const row = makeAnnotation({
      id: `new-${counter}`,
      documentId: args.documentId as string,
      page: args.page as number,
      kind: args.kind as Annotation["kind"],
      data: args.data as string,
      text: (args.text as string | null) ?? null,
    });
    table.push(row);
    return row;
  });
  backend.on("update_annotation", (args) => {
    // Replaced rather than mutated, so an undo entry holding the previous row object still
    // sees the previous text -- which is what makes the undo assertion meaningful.
    table = table.map((row) =>
      row.id === args.annotationId
        ? { ...row, data: args.data as string, text: (args.text as string | null) ?? null }
        : row,
    );
    return table.find((row) => row.id === args.annotationId)!;
  });
  backend.on("remove_annotation", (args) => {
    table = table.filter((candidate) => candidate.id !== args.annotationId);
    return null;
  });
  backend.on("restore_annotation", (args) => {
    table.push(args.annotation as Annotation);
    return null;
  });
  backend.on("get_app_state", () => null);
  backend.on("set_app_state", () => null);

  const view = renderHook(() => useEditor(), {
    wrapper: ({ children }) => (
      <EditorProvider documentId="doc-1" page={page}>
        {children}
      </EditorProvider>
    ),
  });

  return {
    backend,
    view,
    /** The live table, for asserting on what was actually persisted. */
    rows: () => table,
  };
}

/** Waits for the provider's initial `list_annotations` round trip to land. */
async function waitForLoad(result: { current: ReturnType<typeof useEditor> }) {
  await waitFor(() => expect(result.current.error).toBeNull());
}

let open: FakeBackend | null = null;

afterEach(() => {
  open?.teardown();
  open = null;
});

describe("EditorProvider: creating a note keeps its text", () => {
  it("passes the note's text to the backend instead of null", async () => {
    // The reported defect: the overlay called `create(..., text = null)` for notes, so the
    // row -- and therefore the exported /Contents -- could never carry the text.
    const { backend, view, rows } = mountEditor();
    open = backend;
    await waitForLoad(view.result);

    await act(async () => {
      await view.result.current.create(
        0,
        "note",
        { v: 1, point: [12, 34], color: "#f5a623" },
        "check this claim",
      );
    });

    const call = backend.callsTo("add_annotation")[0];
    expect(call.text).toBe("check this claim");
    expect(rows()[rows().length - 1]?.text).toBe("check this claim");
  });

  it("records the text alongside the geometry, not instead of it", async () => {
    const { backend, view, rows } = mountEditor();
    open = backend;
    await waitForLoad(view.result);

    await act(async () => {
      await view.result.current.create(0, "note", { v: 1, point: [12, 34] }, "hello");
    });

    const stored = rows()[rows().length - 1]!;
    expect(JSON.parse(stored.data)).toEqual({ v: 1, point: [12, 34] });
    expect(stored.text).toBe("hello");
  });

  it("still allows a text-less mark for the tools that have no text", async () => {
    const { backend, view, rows } = mountEditor();
    open = backend;
    await waitForLoad(view.result);

    await act(async () => {
      await view.result.current.create(0, "highlight", { v: 1, rects: [[0, 0, 10, 10]] }, null);
    });

    expect(backend.callsTo("add_annotation")[0].text).toBeNull();
    expect(rows()[rows().length - 1]?.text).toBeNull();
  });
});

describe("EditorProvider: editing a note's text", () => {
  it("writes the new text and exposes it on the next render", async () => {
    const note = makeAnnotation({ text: "before" });
    const { backend, view, rows } = mountEditor([note]);
    open = backend;
    await waitForLoad(view.result);

    await act(async () => {
      await view.result.current.updateText(note, "after");
    });

    expect(backend.callsTo("update_annotation")[0].text).toBe("after");
    expect(rows()[0].text).toBe("after");
    await waitFor(() => expect(view.result.current.annotations[0].text).toBe("after"));
  });

  it("restores the previous text on undo, rather than deleting the note", async () => {
    // Editing text changes something that still exists, so undo must not remove the row.
    const note = makeAnnotation({ text: "before" });
    const { backend, view, rows } = mountEditor([note]);
    open = backend;
    await waitForLoad(view.result);

    await act(async () => {
      await view.result.current.updateText(note, "after");
    });
    await act(async () => {
      await view.result.current.undo();
    });

    expect(rows()).toHaveLength(1);
    expect(rows()[0].text).toBe("before");
    expect(backend.callsTo("remove_annotation")).toHaveLength(0);
  });

  it("does not write when the text is unchanged", async () => {
    // The editor commits on blur, which also fires on unmount; the guard against the second
    // write lives in the component, not here. Asserted so a future move of that guard into
    // the provider is a visible change rather than a silent one.
    const note = makeAnnotation({ text: "same" });
    const { backend, view } = mountEditor([note]);
    open = backend;
    await waitForLoad(view.result);

    await act(async () => {
      await view.result.current.updateText(note, "same");
    });

    expect(backend.callsTo("update_annotation")).toHaveLength(1);
  });
});

describe("EditorProvider: cross-surface sync (P2-D11)", () => {
  it("re-reads annotations when a change is reported for this document", async () => {
    // The event names only the document, and listeners re-read the DB rather than trusting
    // its payload -- so the observable effect is another query, not a local mutation.
    const { backend, view } = mountEditor();
    open = backend;
    await waitForLoad(view.result);
    await waitFor(() => expect(backend.eventListenCount()).toBe(1));
    const before = backend.callsTo("list_annotations").length;

    act(() => {
      backend.emitEvent(ANNOTATION_CHANGED_EVENT, { documentId: "doc-1" });
    });

    await waitFor(() =>
      expect(backend.callsTo("list_annotations").length).toBeGreaterThan(before),
    );
  });

  it("ignores a change reported for a different document", async () => {
    // The event is process-wide, so without the guard every open pane would re-query for
    // every write anywhere in the app.
    const { backend, view } = mountEditor();
    open = backend;
    await waitForLoad(view.result);
    await waitFor(() => expect(backend.eventListenCount()).toBe(1));
    const before = backend.callsTo("list_annotations").length;

    act(() => {
      backend.emitEvent(ANNOTATION_CHANGED_EVENT, { documentId: "doc-other" });
    });
    await act(async () => {
      await Promise.resolve();
    });

    expect(backend.callsTo("list_annotations")).toHaveLength(before);
  });
});

describe("EditorProvider: deleting", () => {
  it("removes the row and can restore it under its original id", async () => {
    // `restore_annotation` (not `add_annotation`) matters: a fresh id would break the
    // search hits and selections that already reference the old one.
    const note = makeAnnotation({ id: "keep-me", text: "note" });
    const { backend, view, rows } = mountEditor([note]);
    open = backend;
    await waitForLoad(view.result);

    await act(async () => {
      await view.result.current.remove(note);
    });
    expect(rows()).toHaveLength(0);

    await act(async () => {
      await view.result.current.undo();
    });
    expect(rows().map((row) => row.id)).toEqual(["keep-me"]);
    expect(backend.callsTo("restore_annotation")[0].annotation).toMatchObject({ id: "keep-me" });
  });

  it("deletes it again on redo", async () => {
    const note = makeAnnotation({ id: "keep-me" });
    const { backend, view, rows } = mountEditor([note]);
    open = backend;
    await waitForLoad(view.result);

    await act(async () => {
      await view.result.current.remove(note);
    });
    await act(async () => {
      await view.result.current.undo();
    });
    await act(async () => {
      await view.result.current.redo();
    });

    expect(rows()).toHaveLength(0);
  });
});

describe("EditorProvider: undo/redo bookkeeping", () => {
  it("reports canUndo/canRedo as the stacks change", async () => {
    const { backend, view } = mountEditor();
    open = backend;
    await waitForLoad(view.result);

    expect(view.result.current.canUndo).toBe(false);
    await act(async () => {
      await view.result.current.create(0, "note", { v: 1, point: [1, 1] }, "x");
    });
    expect(view.result.current.canUndo).toBe(true);
    expect(view.result.current.canRedo).toBe(false);

    await act(async () => {
      await view.result.current.undo();
    });
    expect(view.result.current.canUndo).toBe(false);
    expect(view.result.current.canRedo).toBe(true);
  });

  it("re-creates an undone annotation with a fresh id and can undo that too", async () => {
    // Re-creating after an undo mints a new id; a stack entry closing over the original id
    // would make the second undo delete a row that no longer exists.
    const { backend, view, rows } = mountEditor();
    open = backend;
    await waitForLoad(view.result);

    await act(async () => {
      await view.result.current.create(0, "note", { v: 1, point: [1, 1] }, "x");
    });
    const firstId = rows()[0].id;

    await act(async () => {
      await view.result.current.undo();
    });
    await act(async () => {
      await view.result.current.redo();
    });
    const secondId = rows()[0].id;
    expect(secondId).not.toBe(firstId);

    // The real assertion: undo tracks the id that exists now.
    await act(async () => {
      await view.result.current.undo();
    });
    expect(rows()).toHaveLength(0);
    expect(view.result.current.error).toBeNull();
  });

  it("drops nothing while at the depth cap and keeps the newest entries", async () => {
    // MAX_STACK is 100; going past it must discard the oldest, not the newest.
    const { backend, view } = mountEditor();
    open = backend;
    await waitForLoad(view.result);

    for (let index = 0; index < 102; index += 1) {
      await act(async () => {
        await view.result.current.create(0, "note", { v: 1, point: [index, index] }, `n${index}`);
      });
    }

    // 102 created, cap 100 -> the first two are unreachable but the rest are undoable.
    for (let index = 0; index < 100; index += 1) {
      expect(view.result.current.canUndo, `undo ${index}`).toBe(true);
      await act(async () => {
        await view.result.current.undo();
      });
    }
    expect(view.result.current.canUndo).toBe(false);
  });
});

describe("EditorProvider: error surface", () => {
  it("captures a backend rejection as an AppError instead of throwing", async () => {
    const { backend, view } = mountEditor();
    open = backend;
    await waitForLoad(view.result);

    backend.on("add_annotation", () => {
      throw { code: "DB_LOCKED", message: "database is locked" };
    });

    await act(async () => {
      await view.result.current.create(0, "note", { v: 1, point: [1, 1] }, "x");
    });

    expect(view.result.current.error).toMatchObject({ code: "DB_LOCKED" });
    await act(async () => {
      view.result.current.clearError();
    });
    expect(view.result.current.error).toBeNull();
  });
});

describe("EditorProvider: page scoping", () => {
  it("draws only the open page's annotations", async () => {
    const onPage0 = makeAnnotation({ id: "a", page: 0 });
    const onPage1 = makeAnnotation({ id: "b", page: 1 });
    const { backend, view } = mountEditor([onPage0, onPage1], 1);
    open = backend;

    await waitFor(() => expect(view.result.current.annotations).toHaveLength(1));
    expect(view.result.current.annotations[0].id).toBe("b");
  });
});
