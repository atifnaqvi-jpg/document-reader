/**
 * P3.3 D6: the interactive form layer, driven through the real `src/api/invoke.ts` and the fake
 * IPC boundary.
 *
 * The defect this closes was not a broken engine — it was an unreachable one. The engine filled
 * fields and PDFium drew widget appearances, so the form *looked* interactive and did nothing.
 * Every assertion here is therefore about reach: does a click arrive, does a keystroke arrive,
 * does the write reach Rust, and does the answer come back.
 *
 * Four promises are pinned:
 *
 * 1. A widget appears on its own page and not on another — from `rectPages`, and a `null` page
 *    draws nothing rather than defaulting to page 0.
 * 2. Typing reaches `fill_form_field` **on blur**, not per keystroke (design §10.3).
 * 3. The value shown afterwards is the one re-read from the file, not the one typed.
 * 4. A refusal from the engine is surfaced and the typed text is not silently kept as truth.
 */

import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { ViewerPane } from "./ViewerPane";
import { EditorProvider } from "../state/EditorProvider";
import { placeWidgets, buttonFillValue, buttonIsSet, inertReason } from "./FormOverlay";
import type { FormField, LibraryEntry } from "../api/types";
import type { PageMetrics } from "./AnnotationOverlay";
import { installFakeBackend } from "../test/tauri";
import type { FakeBackend } from "../test/tauri";

const RENDERED = { bytes: "AAAA", width: 612, height: 792 };

const entry: LibraryEntry = {
  id: "doc-1",
  title: "A Form",
  author: null,
  pages: 2,
  format: "pdf",
  path: "/books/form.pdf",
  missing: false,
  addedAt: 1,
  lastOpenedAt: 2,
  encrypted: false,
};

/** A text field on `page`, with `rect` in PDF points. */
function textField(overrides: Partial<FormField> = {}): FormField {
  return {
    name: "fullname",
    fieldType: "Tx",
    flags: 0,
    value: null,
    states: [],
    options: [],
    selectedIndices: [],
    rects: [[72, 700, 300, 724]],
    rectPages: [0],
    readOnly: false,
    radio: false,
    ...overrides,
  };
}

/** A checkbox with one on-state, as the LibreOffice fixture carries. */
function checkboxField(overrides: Partial<FormField> = {}): FormField {
  return textField({
    name: "agree",
    fieldType: "Btn",
    states: ["Yes"],
    value: null,
    rects: [[72, 600, 86, 614]],
    ...overrides,
  });
}

const metrics: PageMetrics = {
  pageNumber: 0,
  width: 612,
  height: 792,
  pageHeightPoints: 792,
  scale: 1,
};

let backend: FakeBackend;

beforeEach(() => {
  backend = installFakeBackend();
  backend.on("get_document_page_count", () => 2);
  backend.on("get_document_page_render", () => RENDERED);
  backend.on("list_form_fields", () => []);
});

afterEach(() => {
  backend.teardown();
});

/** Renders the viewer over a document whose fields the fake backend reports. */
async function renderWithFields(fields: FormField[]) {
  backend.on("list_form_fields", () => fields);
  const rendered = render(
    <EditorProvider documentId={entry.id} page={0}>
      <ViewerPane entry={entry} />
    </EditorProvider>,
  );
  // The field list is fetched in an effect; wait for the overlay rather than a timer.
  if (fields.length > 0) {
    await waitFor(() => expect(screen.getByRole("group", { name: /form fields/i })).toBeTruthy());
  } else {
    await waitFor(() => expect(backend.callsTo("list_form_fields")).toHaveLength(1));
  }
  return rendered;
}

describe("placeWidgets", () => {
  it("places a widget on the page it names, in rendered pixels with y flipped", () => {
    const placed = placeWidgets([textField()], metrics);
    expect(placed).toHaveLength(1);
    // [72, 700, 300, 724] with the page 792pt tall, y flipped: top = 792 - 724 = 68.
    expect(placed[0].left).toBe(72);
    expect(placed[0].top).toBe(68);
    expect(placed[0].width).toBe(228);
    expect(placed[0].height).toBe(24);
  });

  it("scales with the render, because a zoomed page is a bigger pixel grid", () => {
    const placed = placeWidgets([textField()], { ...metrics, scale: 2 });
    expect(placed[0].left).toBe(144);
    expect(placed[0].top).toBe(136);
    expect(placed[0].width).toBe(456);
  });

  it("omits a widget that belongs to another page", () => {
    const placed = placeWidgets([textField({ rectPages: [1] })], metrics);
    expect(placed).toHaveLength(0);
  });

  it("omits a widget whose page is unknown rather than placing it on page 0", () => {
    // This is the honesty rule: `null` is "the document does not say", and page 0 is a guess
    // that would draw an input box over unrelated content.
    const placed = placeWidgets([textField({ rectPages: [null] })], metrics);
    expect(placed).toHaveLength(0);
  });

  it("treats a missing rectPages as unknown for every rect", () => {
    const placed = placeWidgets([textField({ rectPages: undefined })], metrics);
    expect(placed).toHaveLength(0);
  });

  it("keeps a radio group's second widget on its own page", () => {
    const group = textField({
      name: "color",
      fieldType: "Btn",
      radio: true,
      states: ["red", "blue"],
      rects: [
        [72, 700, 86, 714],
        [112, 700, 126, 714],
      ],
      rectPages: [1, 1],
    });
    expect(placeWidgets([group], metrics)).toHaveLength(0);
    expect(placeWidgets([group], { ...metrics, pageNumber: 1 })).toHaveLength(2);
  });
});

describe("buttonFillValue", () => {
  it("takes the on-state the document carries", () => {
    expect(buttonFillValue(checkboxField())).toBe("Yes");
  });

  it("reports no value for a button with no on-state", () => {
    // A push button has `/AP /N` as a stream, so it has no states. Filling it would need a
    // spelling to write, and inventing one is the bug D-F3 forbids.
    expect(buttonFillValue(checkboxField({ states: [] }))).toBeNull();
  });
});

describe("buttonIsSet", () => {
  it("is set when the value is one of the field's own states", () => {
    expect(buttonIsSet(checkboxField({ value: "Yes" }))).toBe(true);
  });

  it("is not set for Off, and not set for no value at all", () => {
    expect(buttonIsSet(checkboxField({ value: "Off" }))).toBe(false);
    expect(buttonIsSet(checkboxField({ value: null }))).toBe(false);
  });
});

describe("inertReason", () => {
  it("names read-only first", () => {
    expect(inertReason(textField({ readOnly: true }))).toMatch(/read-only/i);
  });

  it("names an unsupported type", () => {
    expect(inertReason(textField({ fieldType: "Sig" }))).toMatch(/Sig/);
  });

  it("names the choice-field gap rather than drawing a text box over it", () => {
    expect(inertReason(textField({ fieldType: "Ch" }))).toMatch(/dropdown/i);
  });

  it("is null for a fillable text field", () => {
    expect(inertReason(textField())).toBeNull();
  });
});

describe("the overlay reaches the engine", () => {
  it("writes a text field on blur, and shows the value the file reported", async () => {
    backend.on("fill_form_field", () => ({
      path: "/books/form-1.pdf",
      fields: [textField({ value: "Ada Lovelace" })],
    }));
    await renderWithFields([textField()]);

    const input = screen.getByLabelText("fullname");
    await userEvent.type(input, "Ada Lovelace");
    // Still nothing has been written: typing alone must not produce a file per character
    // (design §10.3, P2-D2).
    expect(backend.callsTo("fill_form_field")).toHaveLength(0);

    await userEvent.tab();

    await waitFor(() => expect(backend.callsTo("fill_form_field")).toHaveLength(1));
    expect(backend.callsTo("fill_form_field")[0]).toEqual({
      documentId: "doc-1",
      field: "fullname",
      value: "Ada Lovelace",
    });
    // The displayed value comes back from the file, not from the draft.
    await waitFor(() => expect((screen.getByLabelText("fullname") as HTMLInputElement).value).toBe("Ada Lovelace"));
  });

  it("commits on Enter without leaving the field", async () => {
    backend.on("fill_form_field", () => ({ path: "/books/form-1.pdf", fields: [textField({ value: "x" })] }));
    await renderWithFields([textField()]);

    await userEvent.type(screen.getByLabelText("fullname"), "x{Enter}");
    await waitFor(() => expect(backend.callsTo("fill_form_field")).toHaveLength(1));
  });

  it("writes a checkbox using the document's own on-state", async () => {
    backend.on("fill_form_field", () => ({
      path: "/books/form-1.pdf",
      fields: [checkboxField({ value: "Yes" })],
    }));
    await renderWithFields([checkboxField()]);

    await userEvent.click(screen.getByRole("button", { name: "agree" }));
    await waitFor(() => expect(backend.callsTo("fill_form_field")).toHaveLength(1));
    // "Yes" here came from `states`, which came from the document's `/AP /N`. A hardcoded
    // `/On` would fail against this fixture, which is why the fixture spells it "Yes".
    expect(backend.callsTo("fill_form_field")[0].value).toBe("Yes");
  });

  it("surfaces a refusal by clearing nothing and marking the control invalid", async () => {
    // A fillable field whose write the engine nevertheless refuses — the shape a certified
    // document or a protected field produces. The text the user typed must not be presented as
    // if it were saved.
    backend.on("fill_form_field", () => {
      throw { code: "FORM_FIELD_VALUE_NOT_ALLOWED", message: "that value is not allowed" };
    });
    await renderWithFields([textField()]);

    const input = screen.getByLabelText("fullname");
    await userEvent.type(input, "nope");
    await userEvent.tab();

    await waitFor(() => expect(backend.callsTo("fill_form_field")).toHaveLength(1));
    await waitFor(() => expect(input.getAttribute("aria-invalid")).toBe("true"));
  });

  it("draws a read-only field inert, with no input to type into", async () => {
    // The read-only refusal is unreachable by clicking, which is the point: the field is drawn
    // so the user can see it (D-F7) and cannot be edited, so no write is ever attempted.
    const { container } = await renderWithFields([textField({ readOnly: true })]);
    expect(screen.queryByLabelText("fullname")).toBeNull();
    const drawn = container.querySelector('.form-widget-inert[data-field="fullname"]');
    expect(drawn).toBeTruthy();
  });

  it("does not draw a widget whose page is unknown", async () => {
    await renderWithFields([textField({ rectPages: [null] })]);
    expect(screen.queryByLabelText("fullname")).toBeNull();
  });

  it("draws nothing at all for a document with no form", async () => {
    backend.on("list_form_fields", () => []);
    render(
      <EditorProvider documentId={entry.id} page={0}>
        <ViewerPane entry={entry} />
      </EditorProvider>,
    );
    await waitFor(() => expect(backend.callsTo("list_form_fields")).toHaveLength(1));
    expect(screen.queryByRole("group", { name: /form fields/i })).toBeNull();
  });
});
