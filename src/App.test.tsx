/**
 * `App` renders. That is the whole claim, and before this file nothing made it (D10).
 *
 * ## Why this is worth a file of its own
 *
 * Every other frontend test mounts a component or a provider. None mounted `App`, so the
 * one component that composes the whole shell — providers, toolbar, library, viewer, the
 * error banner — had no test that would fail if it could not render. `App.select.test.ts`
 * is named for `App` but imports only a pure helper from it, and says so.
 *
 * The gap mattered: a user reported that placing a text box blanked the window, and the
 * suite was green throughout. A blank window is a render throw that unmounted the root, so
 * "does `App` render at all, with nothing to show" is the cheapest possible test of the one
 * symptom the user could actually see.
 *
 * ## What makes this a real render, and what it deliberately does not assert
 *
 * The backend is the project's fake IPC boundary, so `invoke.ts`'s argument mapping runs
 * for real and only the process boundary is faked. The library is empty, which is the
 * first-run shape and needs no document.
 *
 * There is no viewer without a document, so this does not reach `ViewerPane` — the crash
 * path itself. It cannot: reproducing that needs PDFium under WebView2, which jsdom does not
 * have. `ErrorBoundary.test.tsx` covers the catch; this covers the mount. Neither is a
 * substitute for the boundary printing a real stack in the field.
 */

import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import App from "./App";
import { Root } from "./root";
import { installFakeBackend, type FakeBackend } from "./test/tauri";
import type { LibraryEntry } from "./api/types";

let backend: FakeBackend | null = null;

afterEach(() => {
  backend?.teardown();
  backend = null;
});

/** The library read every mount performs, so `App` can settle without a real database. */
function mountingBackend(): FakeBackend {
  const fake = installFakeBackend();
  fake.on("list_library", () => []);
  fake.on("get_app_state", () => null);
  fake.on("set_app_state", () => null);
  // The window is the source of truth for fullscreen (D11), so `ReadingMode` reads it on
  // mount. Unhandled, that read rejects and the app renders its own `FULLSCREEN_UNAVAILABLE`
  // banner -- a real error path, but not the one under test here, and the banner's
  // `role="alert"` would be mistaken for the boundary's panel.
  fake.on("plugin:window|is_fullscreen", () => false);
  return fake;
}

describe("App", () => {
  it("renders the shell with an empty library", async () => {
    backend = mountingBackend();

    const { container } = render(<App />);

    // The toolbar is outside every pane that a document would populate, so its presence is
    // the signal that the shell mounted rather than a pane that happens to render first.
    expect(await screen.findByText("DocuPro")).toBeInTheDocument();
    expect(container.querySelector(".toolbar")).not.toBeNull();
  });

  it("asks the backend for the library exactly once on mount", async () => {
    backend = mountingBackend();

    render(<App />);

    // One read per mount is the behaviour; a second would mean an effect re-firing on a
    // dependency that changes identity every render, which is how a render loop starts.
    await waitFor(() => expect(backend!.callsTo("list_library").length).toBeGreaterThan(0));
    expect(backend!.callsTo("list_library").length).toBe(1);
  });

  it("mounts through the production root, boundary included", async () => {
    backend = mountingBackend();

    // `Root` is what `main.tsx` renders, so this is the tree the user's window gets --
    // StrictMode, boundary and all. Mounting `App` directly would leave the outermost
    // wrapper untested, and the wrapper is the D10 instrument.
    render(<Root />);

    expect(await screen.findByText("DocuPro")).toBeInTheDocument();
    // The boundary's panel must be absent: it only appears on a throw. Identified by its own
    // id, not by `role="alert"` -- the app's error banner carries that role too.
    expect(screen.queryByTestId("render-error-panel")).toBeNull();
  });
});

/**
 * D13: opening a document from the library must not blank the window. This is the whole of
 * the 1.4.0 blocker, exercised through the tree the user's window actually mounts.
 *
 * ## What this reproduces that the pane-level guard cannot
 *
 * The pane-level tests in `EditorPane.test.tsx` drive the null-to-document transition directly.
 * This one goes through `Root` (StrictMode, boundary, providers, library, viewer) and *clicks a
 * library row*, which is the user's own gesture. It matters because the click is what makes
 * `App` hand a real entry down while the pane is already mounted in its empty state -- and
 * because a throw here would be caught by the boundary, turning a crash into a rendered error
 * panel rather than a test failure. Asserting the panel is absent is therefore the assertion
 * that the open path did not throw.
 *
 * ## Two documents, neither with annotations
 *
 * The report first came from one document, then from a second with different content, which
 * ruled out "the annotation list is the trigger". Both cases are mounted here with an empty
 * annotation list, so the test states the narrowed claim directly: the fault is in the
 * document-open path every PDF shares, not in data that only some documents have.
 */

describe("App: opening any document does not blank the window (D13)", () => {
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

  /** A mounted shell whose library holds two flat PDFs, and no annotations anywhere. */
  function shellWithTwoDocuments(): FakeBackend {
    const fake = installFakeBackend();
    fake.on("list_library", () => [pdf("lea", "LEA"), pdf("maastricht", "Maastricht")]);
    fake.on("get_app_state", () => null);
    fake.on("set_app_state", () => null);
    fake.on("plugin:window|is_fullscreen", () => false);
    // Every document-open read the viewer and provider perform on mount.
    fake.on("list_annotations", () => []);
    fake.on("list_form_fields", () => []);
    fake.on("get_document_page_count", () => 3);
    fake.on("get_document_page_render", () => ({
      page: 0,
      width: 612,
      height: 792,
      pngBase64: "",
    }));
    return fake;
  }

  it.each([
    ["the first document", "LEA"],
    ["a second document with different content", "Maastricht"],
  ])("opens %s without a render error", async (_label, title) => {
    backend = shellWithTwoDocuments();

    render(<Root />);

    // The row exists, so the click below is a real library interaction and not a no-op.
    const row = await screen.findByText(title);

    // `fireEvent` rather than `userEvent`: the throw happens synchronously during the state
    // update the click schedules, and a synchronous dispatch keeps the failure attributed to
    // this call rather than to whichever await settles next.
    fireEvent.click(row);

    // The boundary only renders its panel on a throw. Its absence is the assertion that the
    // open path did not blow up -- and it is checked by test id, because the app's own error
    // banner also carries `role="alert"`.
    await waitFor(() => expect(screen.queryByTestId("render-error-panel")).toBeNull());

    // ...and the reason: the document really opened. Without this the assertion above would
    // pass on a shell that never changed state at all.
    await waitFor(() => expect(screen.getByLabelText("Page")).toBeInTheDocument());
  });
});
