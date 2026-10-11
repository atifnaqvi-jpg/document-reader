/**
 * P2.4 text editing, driven through the real `src/api/invoke.ts` and the fake IPC boundary.
 *
 * Four things are pinned here, and each one is a promise the design document makes to the
 * user rather than an implementation detail:
 *
 * 1. The picker lists a page's runs and an edit really travels to `edit_page_text`.
 * 2. A replacement that overflows the run's width is **refused**, and the refusal reports
 *    both measured widths and points at the annotation tool (T3d/O1). A refusal the user
 *    cannot act on is the failure Q2 exists to prevent.
 * 3. A protected document shows the calm read-only notice and offers no picker at all
 *    (T7) — it does not offer an edit the backend would always reject.
 * 4. Removing protection is confirmed before it happens and only then calls
 *    `decrypt_copy` (T8).
 *
 * The page raster is a fixed stub: nothing asserted here depends on pixels, only on the
 * commands that cross the wire.
 */

import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it } from "vitest";
import { EditorPane } from "./EditorPane";
import { ViewerPane } from "./ViewerPane";
import { EditorProvider } from "../state/EditorProvider";
import type { LibraryEntry, PageTextRun } from "../api/types";
import { installFakeBackend } from "../test/tauri";
import type { FakeBackend } from "../test/tauri";

const RENDERED = { bytes: "AAAA", width: 612, height: 792 };

const entry: LibraryEntry = {
  id: "doc-1",
  title: "A Book",
  author: null,
  pages: 3,
  format: "pdf",
  path: "/books/a.pdf",
  missing: false,
  addedAt: 1,
  lastOpenedAt: 2,
  encrypted: false,
};

/** One run as the backend describes it: an address, a width, and a verdict. */
function makeRun(overrides: Partial<PageTextRun> = {}): PageTextRun {
  return {
    runId: { opIndex: 7, operandPath: [1] },
    text: "hello",
    operator: "Tj",
    fontKey: "F1",
    fontName: "Helvetica",
    fontSize: 12,
    rect: { x0: 72, y0: 600, x1: 200, y1: 612 },
    width: 120,
    admissibility: { editable: true, gate: null, message: null },
    ...overrides,
  };
}

/**
 * Mounts the whole editing surface for one document.
 *
 * Both panes are rendered for real: the picker's state lives in the provider, so splitting
 * them would test a wiring that does not exist.
 */
function mountEditor(overrides: Partial<LibraryEntry> = {}) {
  const backend = installFakeBackend();
  backend.on("list_annotations", () => []);
  backend.on("get_app_state", () => null);
  backend.on("set_app_state", () => null);
  backend.on("get_document_page_render", () => RENDERED);
  backend.on("get_document_page_count", () => 3);

  const merged = { ...entry, ...overrides };
  const view = render(
    <EditorProvider documentId={merged.id} page={0}>
      <EditorPane entry={merged} currentPage={0} />
      <ViewerPane entry={merged} />
    </EditorProvider>,
  );
  return { backend, view, entry: merged };
}

/** Turns the picker on, which is what fetches the runs. */
async function startEditing() {
  await screen.findByRole("button", { name: "Edit text on this page" });
  await userEvent.click(screen.getByRole("button", { name: "Edit text on this page" }));
}

let open: FakeBackend | null = null;

afterEach(() => {
  open?.teardown();
  open = null;
});

describe("P2.4 picker: runs are listed and a chosen run edits", () => {
  it("lists the page's runs and fires edit_page_text with the run's address", async () => {
    const run = makeRun();
    const mounted = mountEditor();
    open = mounted.backend;
    mounted.backend.on("get_page_text_runs", () => [run]);
    mounted.backend.on("edit_page_text", () => ({
      path: "/books/a (edited 1).pdf",
      inverse: { page: 0, opIndex: 7, operandPath: [1], oldBytes: [104], newBytes: [106] },
      text: "jello",
    }));

    await startEditing();

    // The run appears in the panel's list *and* as a clickable box over the page: two
    // surfaces, one source of truth.
    const list = await screen.findByRole("list", { name: "Text runs on this page" });
    const listed = within(list).getByRole("button", { name: /hello/ });
    expect(screen.getAllByRole("button", { name: /^Edit text: hello/ })).toHaveLength(1);

    await userEvent.click(listed);
    const field = await screen.findByLabelText(/Replace hello with/);
    await userEvent.clear(field);
    await userEvent.type(field, "jello");

    await userEvent.click(screen.getByRole("button", { name: "Apply text edit" }));

    await waitFor(() => expect(mounted.backend.callsTo("edit_page_text")).toHaveLength(1));
    const call = mounted.backend.callsTo("edit_page_text")[0]!;
    expect(call.documentId).toBe("doc-1");
    expect(call.page).toBe(0);
    expect(call.runId).toEqual({ opIndex: 7, operandPath: [1] });
    // The text the user was shown travels with the request, so the backend can refuse a
    // target that changed underneath them (T4) rather than editing the wrong bytes.
    expect(call.expected).toBe("hello");
    expect(call.replacement).toBe("jello");
    // The measured width travels too: the width gate must compare against the same metric
    // the picker showed (T3d).
    expect(call.oldWidth).toBe(120);
    expect(call.fontName).toBe("Helvetica");
    expect(call.fontSize).toBe(12);
    // O-ST2-4: the toggle defaults on, so the backend is free to substitute.
    expect(call.allowSubstitution).toBe(true);
    await screen.findByText(/a \(edited 1\)\.pdf/);
  });
});

describe("P2.4 refusal: an overflow is refused with both measured widths", () => {
  it("shows the backend's refusal verbatim and writes nothing", async () => {
    const run = makeRun();
    const mounted = mountEditor();
    open = mounted.backend;
    mounted.backend.on("get_page_text_runs", () => [run]);

    // The tolerance is the backend's rule -- refuse only past
    // `old_width + max(0.5pt, 1% of old_width)` (O1/T3d) -- and the frontend must not
    // second-guess it with arithmetic of its own. What the frontend owes the user is
    // **both** measured widths, unedited, next to the suggested alternative.
    const oldWidth = run.width!;
    const newWidth = 152.3;
    const refusal =
      `the replacement needs ${newWidth.toFixed(1)}pt, available ${oldWidth.toFixed(1)}pt; ` +
      `add a text annotation instead (gate: fits-width)`;
    mounted.backend.on("edit_page_text", () => {
      throw { code: "TEXT_RUN_NOT_EDITABLE", message: refusal };
    });

    await startEditing();
    const list = await screen.findByRole("list", { name: "Text runs on this page" });
    await userEvent.click(within(list).getByRole("button", { name: /hello/ }));
    const field = await screen.findByLabelText(/Replace hello with/);
    await userEvent.clear(field);
    await userEvent.type(field, "a very much longer string");
    await userEvent.click(screen.getByRole("button", { name: "Apply text edit" }));

    const alerts = await screen.findAllByRole("alert");
    // Exactly one surface shows it. Two copies of the same refusal would be a UI defect of
    // its own, and it is what caught the pane and the panel both rendering the error.
    expect(alerts).toHaveLength(1);
    // Verbatim, so both measured widths survive to the user.
    expect(alerts[0]!.textContent).toBe(refusal);
    expect(alerts[0]!.textContent).toContain("152.3pt");
    expect(alerts[0]!.textContent).toContain("120.0pt");

    // The request was made -- and refused, so nothing was written.
    expect(mounted.backend.callsTo("edit_page_text")).toHaveLength(1);
    expect(screen.queryByText(/Saved to/)).toBeNull();
  });

  it("refuses a too-wide run before the user types, quoting both widths", async () => {
    // P2.4 §4: "refusal reasons are pre-computed, not discovered after typing". A run the
    // write path would refuse carries its verdict with it, so the picker greys it out and
    // the panel shows the reason -- which for the width gate means both measured widths.
    const refusal =
      "the replacement needs 240.0pt, available 118.5pt; add a text annotation instead " +
      "(gate: fits-width)";
    const run = makeRun({
      admissibility: { editable: false, gate: "fits-width", message: refusal },
    });
    const mounted = mountEditor();
    open = mounted.backend;
    mounted.backend.on("get_page_text_runs", () => [run]);
    mounted.backend.on("edit_page_text", () => {
      throw new Error("a refused run must never reach the backend");
    });

    await startEditing();
    const list = await screen.findByRole("list", { name: "Text runs on this page" });
    // The gate badge names the violated gate on the listed run itself.
    expect(within(list).getByText("fits-width")).toBeTruthy();

    await userEvent.click(within(list).getByRole("button", { name: /hello/ }));
    const status = await screen.findByText(new RegExp("240.0pt"));
    expect(status).toHaveTextContent("118.5pt");

    // And the form is not offered: the replacement box and the apply button are disabled,
    // so the edit cannot even be attempted.
    expect(screen.getByLabelText(/Replace hello with/)).toBeDisabled();
    expect(screen.getByRole("button", { name: "Apply text edit" })).toBeDisabled();
    expect(mounted.backend.callsTo("edit_page_text")).toHaveLength(0);
  });
});

describe("P2.4 Stage 2: font substitution is disclosed and toggleable", () => {
  it("shows the substitution note verbatim and can dismiss it (T10)", async () => {
    const run = makeRun();
    const mounted = mountEditor();
    open = mounted.backend;
    mounted.backend.on("get_page_text_runs", () => [run]);

    const note = "edited with Helvetica substituting ABCDEF+FixtureSubset";
    mounted.backend.on("edit_page_text", () => ({
      path: "/books/a (edited 1).pdf",
      inverse: { page: 0, opIndex: 7, operandPath: [1], oldBytes: [104], newBytes: [106] },
      text: "jello",
      substitution: "Helvetica",
      note,
    }));

    await startEditing();
    const list = await screen.findByRole("list", { name: "Text runs on this page" });
    await userEvent.click(within(list).getByRole("button", { name: /hello/ }));
    const field = await screen.findByLabelText(/Replace hello with/);
    await userEvent.clear(field);
    await userEvent.type(field, "jello");
    await userEvent.click(screen.getByRole("button", { name: "Apply text edit" }));

    // The disclosure is shown verbatim, so the user learns the typeface changed.
    await screen.findByText(note);
    await userEvent.click(
      screen.getByRole("button", { name: "Dismiss font substitution notice" }),
    );
    await waitFor(() => expect(screen.queryByText(note)).toBeNull());
  });

  it("sends allowSubstitution=false once the toggle is turned off (O-ST2-4)", async () => {
    const run = makeRun();
    const mounted = mountEditor();
    open = mounted.backend;
    mounted.backend.on("get_page_text_runs", () => [run]);
    mounted.backend.on("edit_page_text", () => ({
      path: "/books/a (edited 1).pdf",
      inverse: { page: 0, opIndex: 7, operandPath: [1], oldBytes: [104], newBytes: [106] },
      text: "jello",
      substitution: null,
      note: null,
    }));

    await startEditing();
    const list = await screen.findByRole("list", { name: "Text runs on this page" });
    await userEvent.click(within(list).getByRole("button", { name: /hello/ }));
    const field = await screen.findByLabelText(/Replace hello with/);
    await userEvent.clear(field);
    await userEvent.type(field, "jello");

    const toggle = screen.getByRole("checkbox", {
      name: /Substitute a standard font/,
    }) as HTMLInputElement;
    expect(toggle.checked).toBe(true);
    await userEvent.click(toggle);

    await userEvent.click(screen.getByRole("button", { name: "Apply text edit" }));

    await waitFor(() => expect(mounted.backend.callsTo("edit_page_text")).toHaveLength(1));
    expect(mounted.backend.callsTo("edit_page_text")[0]!.allowSubstitution).toBe(false);
  });

  it("surfaces a SUBSTITUTE_REFUSED refusal on the panel", async () => {
    const run = makeRun();
    const mounted = mountEditor();
    open = mounted.backend;
    mounted.backend.on("get_page_text_runs", () => [run]);
    mounted.backend.on("edit_page_text", () => {
      throw {
        code: "SUBSTITUTE_REFUSED",
        message:
          "this run's font does not contain the glyphs the replacement needs, and " +
          "substitution is turned off; add a text annotation instead",
      };
    });

    await startEditing();
    const list = await screen.findByRole("list", { name: "Text runs on this page" });
    await userEvent.click(within(list).getByRole("button", { name: /hello/ }));
    const field = await screen.findByLabelText(/Replace hello with/);
    await userEvent.clear(field);
    await userEvent.type(field, "jello");
    await userEvent.click(screen.getByRole("button", { name: "Apply text edit" }));

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent(/add a text annotation instead/);
    // It belongs to the panel, not the pane: the pane does not duplicate it.
    expect(screen.getAllByRole("alert")).toHaveLength(1);
  });
});

describe("P2.4 encrypted documents are read-only", () => {
  it("shows the banner and offers no picker", async () => {
    const mounted = mountEditor({ encrypted: true });
    open = mounted.backend;
    // Registered, so the assertion below ("no run request was ever made") is about the UI's
    // behaviour rather than about the fake's refusal to answer.
    mounted.backend.on("get_page_text_runs", () => {
      throw { code: "ENCRYPTED_READ_ONLY", message: "opened read-only" };
    });

    const banner = await screen.findByText(/Password-protected/);
    expect(banner).toBeTruthy();
    // The banner is one status region; the picker panel is replaced by its read-only
    // notice, so the only status text on screen is the banner's.
    const status = await screen.findByRole("status");
    expect(status).toHaveTextContent(/read-only/i);
    expect(status).toHaveTextContent(/original stays protected/i);

    // No way in: the toggle is absent, and no run request was ever made.
    expect(screen.queryByRole("button", { name: "Edit text on this page" })).toBeNull();
    expect(mounted.backend.callsTo("get_page_text_runs")).toHaveLength(0);
    // The explicit way out is named, both in the banner and in the panel's notice.
    expect(screen.getAllByText(/Decrypt a copy/).length).toBeGreaterThan(0);
    expect(screen.getByRole("button", { name: /Decrypt a copy/ })).toBeEnabled();
  });
});

describe("P2.4 decrypt-a-copy requires confirmation", () => {
  it("calls decrypt_copy only after the password is confirmed", async () => {
    const mounted = mountEditor({ encrypted: true });
    open = mounted.backend;
    mounted.backend.on("decrypt_copy", () => ({
      path: "/books/a (unprotected).pdf",
      pages: 3,
    }));

    await userEvent.click(screen.getByRole("button", { name: /Decrypt a copy/ }));

    const dialog = await screen.findByRole("dialog", { name: "Confirm decrypt" });
    // The confirmation has to say what the flow does, not merely "are you sure?".
    expect(dialog).toHaveTextContent(/new copy only/i);
    expect(dialog).toHaveTextContent(/original file is not changed/i);

    // Nothing has happened yet, and the confirm is not offered without a password.
    expect(mounted.backend.callsTo("decrypt_copy")).toHaveLength(0);
    const confirm = within(dialog).getByRole("button", { name: "Decrypt a copy" });
    expect(confirm).toBeDisabled();

    await userEvent.type(within(dialog).getByLabelText("Password"), "s3cret");
    await userEvent.click(confirm);

    await waitFor(() => expect(mounted.backend.callsTo("decrypt_copy")).toHaveLength(1));
    const call = mounted.backend.callsTo("decrypt_copy")[0]!;
    expect(call.documentId).toBe("doc-1");
    expect(call.password).toBe("s3cret");
    // The copy's path is reported through the same channel as every other write.
    await screen.findByText(/a \(unprotected\)\.pdf/);
  });
});
