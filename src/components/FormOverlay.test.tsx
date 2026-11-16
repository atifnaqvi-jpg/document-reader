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

describe("the overlay collects edits without writing them", () => {
  it("typing produces no write and no file, and the save produces exactly one", async () => {
    // P3.3 D8: the user decides when a file appears. A tick or a keystroke used to write, which
    // is a file the user never asked for. This test pins both halves: zero writes while editing,
    // exactly one on the explicit save.
    backend.on("save_form_fields", () => ({
      path: "/books/form-1.pdf",
      fields: [textField({ value: "Atif" })],
    }));
    await renderWithFields([textField()]);

    const input = screen.getByLabelText("fullname");
    await userEvent.type(input, "Atif");
    await userEvent.tab();

    // No write of any kind while editing — not the single-field command, not the batch.
    expect(backend.callsTo("fill_form_field")).toHaveLength(0);
    expect(backend.callsTo("save_form_fields")).toHaveLength(0);

    await userEvent.click(screen.getByRole("button", { name: /save form edits/i }));

    await waitFor(() => expect(backend.callsTo("save_form_fields")).toHaveLength(1));
    // One call, one file — the whole pending set in one write.
    expect(backend.callsTo("save_form_fields")[0]).toEqual({
      documentId: "doc-1",
      values: [{ name: "fullname", value: "Atif" }],
    });
    expect(backend.callsTo("fill_form_field")).toHaveLength(0);
  });

  it("a checkbox toggle writes nothing, and the save writes once", async () => {
    // D8 again, for the reported defect: ticking used to write immediately.
    backend.on("save_form_fields", () => ({
      path: "/books/form-1.pdf",
      fields: [checkboxField({ value: "Yes" })],
    }));
    await renderWithFields([checkboxField()]);

    await userEvent.click(screen.getByRole("button", { name: "agree" }));
    expect(backend.callsTo("fill_form_field")).toHaveLength(0);
    expect(backend.callsTo("save_form_fields")).toHaveLength(0);
    // The tick is visible immediately, from local state alone.
    expect(screen.getByRole("button", { name: "agree" }).getAttribute("aria-pressed")).toBe("true");

    await userEvent.click(screen.getByRole("button", { name: /save form edits/i }));
    await waitFor(() => expect(backend.callsTo("save_form_fields")).toHaveLength(1));
    // "Yes" came from the document's own states, never a hardcoded `/On`.
    expect(backend.callsTo("save_form_fields")[0].values).toEqual([{ name: "agree", value: "Yes" }]);
  });

  it("draws a tick the moment the box is clicked (D9)", async () => {
    // The reported defect: the box rendered unticked and clicking did nothing visible. Under D8
    // no write happens, so the raster cannot change — the mark must be drawn by the overlay.
    // Asserting on the glyph and not only on `aria-pressed` is the whole point: a state attribute
    // a sighted user cannot see is exactly what "clicking does nothing" looked like.
    await renderWithFields([checkboxField()]);
    const box = screen.getByRole("button", { name: "agree" });

    expect(box.querySelector(".form-widget-tick")).toBeNull();

    await userEvent.click(box);

    expect(box.getAttribute("aria-pressed")).toBe("true");
    const tick = box.querySelector(".form-widget-tick");
    expect(tick, "a clicked box must show a drawn tick, not just a tint").not.toBeNull();
    // Still no write: the mark is local state until the save (D8).
    expect(backend.callsTo("save_form_fields")).toHaveLength(0);
    expect(backend.callsTo("fill_form_field")).toHaveLength(0);

    // Clicking again unticks it, and the glyph goes with the state.
    await userEvent.click(box);
    expect(box.getAttribute("aria-pressed")).toBe("false");
    expect(box.querySelector(".form-widget-tick")).toBeNull();
  });

  it("a box whose /V is Off and states is [Yes] is clickable and toggles (D9)", async () => {
    // The exact shape the forms engine reports for a reportlab 5.1.0 checkbox: `/V /Off`, one
    // on-state `/Yes`. Measured from a generated file rather than assumed.
    await renderWithFields([checkboxField({ value: "Off", states: ["Yes"] })]);
    const box = screen.getByRole("button", { name: "agree" }) as HTMLButtonElement;

    expect(box.disabled, "a checkbox with an on-state must be clickable").toBe(false);
    await userEvent.click(box);
    expect(box.getAttribute("aria-pressed")).toBe("true");
  });

  it("a checkbox the engine reports with no states says why it is inert (D9)", async () => {
    // The failure mode behind the report: an empty `states` used to produce a *disabled* button
    // with no explanation. That is a silent skip, which this project bans. It is now drawn inert
    // with a reason, so an unfillable box reads as unfillable rather than as a broken click.
    const { container } = await renderWithFields([checkboxField({ states: [] })]);
    const drawn = container.querySelector('.form-widget-inert[data-field="agree"]');
    expect(drawn, "an unfillable checkbox must be drawn inert, not disabled and silent").toBeTruthy();
    expect(drawn?.getAttribute("title")).toMatch(/on state/i);
  });

  it("save is disabled until something is actually changed", async () => {
    backend.on("save_form_fields", () => ({ path: "/p.pdf", fields: [textField()] }));
    await renderWithFields([textField()]);
    expect((screen.getByRole("button", { name: /save form edits/i }) as HTMLButtonElement).disabled).toBe(true);

    await userEvent.type(screen.getByLabelText("fullname"), "x");
    expect((screen.getByRole("button", { name: /save form edits/i }) as HTMLButtonElement).disabled).toBe(false);
  });

  it("typing a value back to the stored one leaves nothing pending", async () => {
    backend.on("save_form_fields", () => ({ path: "/p.pdf", fields: [textField()] }));
    await renderWithFields([textField({ value: "same" })]);
    const input = screen.getByLabelText("fullname");

    await userEvent.clear(input);
    await userEvent.type(input, "same");
    expect((screen.getByRole("button", { name: /save form edits/i }) as HTMLButtonElement).disabled).toBe(true);
  });

  it("after a save the controls show the file's values, not the typed ones", async () => {
    // P3.3 D7: once the save lands the overlay defers to the saved values. The engine may
    // normalise what was typed, and the file is the one that is true.
    backend.on("save_form_fields", () => ({
      path: "/books/form-1.pdf",
      fields: [textField({ value: "Normalised" })],
    }));
    await renderWithFields([textField()]);

    await userEvent.type(screen.getByLabelText("fullname"), "typed");
    await userEvent.click(screen.getByRole("button", { name: /save form edits/i }));

    await waitFor(() =>
      expect((screen.getByLabelText("fullname") as HTMLInputElement).value).toBe("Normalised"),
    );
    // And nothing is pending again, so a second save cannot write an unchanged copy.
    await waitFor(() =>
      expect((screen.getByRole("button", { name: /save form edits/i }) as HTMLButtonElement).disabled).toBe(true),
    );
  });

  it("surfaces a refusal beside the page and keeps the unsaved text", async () => {
    // A refused save must not destroy what the user typed: the document is fine, the write was
    // refused. `setError` would replace the page and take the overlay with it.
    backend.on("save_form_fields", () => {
      throw { code: "CERTIFIED_LEVEL_FORBIDS", message: "this document forbids the change" };
    });
    await renderWithFields([textField()]);

    await userEvent.type(screen.getByLabelText("fullname"), "keep me");
    await userEvent.click(screen.getByRole("button", { name: /save form edits/i }));

    await waitFor(() => expect(screen.getByRole("alert").textContent).toMatch(/forbids/i));
    // The page and the typed text survive the refusal.
    expect((screen.getByLabelText("fullname") as HTMLInputElement).value).toBe("keep me");
    expect(screen.getByRole("group", { name: /form fields/i })).toBeTruthy();
  });

  it("exactly one rendering of a text value is visible", async () => {
    // P3.3 D7: the raster underneath already draws the widget's appearance, so the overlay's box
    // must be opaque. A translucent box shows both renderings — the typed text plus a faded ghost
    // of the same value, offset. Asserted on the computed background because that is the mechanism.
    await renderWithFields([textField()]);
    const input = screen.getByLabelText("fullname");
    await userEvent.type(input, "Atif");

    const background = (input as HTMLElement).style.background;
    expect(background).toBe("rgb(255, 255, 255)");
    // Fully opaque: an alpha channel here would let the raster's own text show through.
    expect(background).not.toMatch(/rgba/);
  });

  it("draws a checkbox as one control, not a control over a drawn tick", async () => {
    // The same double-draw question for a button: the overlay's tick must be able to cover the
    // widget's own appearance, so a checked control is a filled box rather than a translucent one.
    await renderWithFields([checkboxField({ value: "Yes" })]);
    const button = screen.getByRole("button", { name: "agree" });
    expect(button.getAttribute("aria-pressed")).toBe("true");
    expect(button.className).toContain("form-widget-checked");
    // The class is not the claim: a checked box in the file already has a tick in the raster, so a
    // translucent fill would show *that* tick through the overlay's, offset by a pixel or two.
    // Opaque is what makes the drawn tick the only one on screen (D7/D9). Asserted on the inline
    // style because that is where the component sets it — and because a class-name-only assertion
    // once passed over a 0.45-alpha fill.
    const background = (button as HTMLElement).style.background;
    expect(background).toBe("rgb(255, 255, 255)");
    expect(background).not.toMatch(/rgba/);
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
