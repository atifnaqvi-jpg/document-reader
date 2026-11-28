/**
 * D15: the colour picker could not produce black.
 *
 * The picker held six highlight pastels and nothing else, while the text tools defaulted to
 * `#111418` -- so the one colour people fill forms in was the one colour the control could not
 * express. That had a second, quieter effect: a `<select>` whose `value` matches no `<option>`
 * shows some *other* entry, so the control was silently reporting a colour the tool would not
 * draw in.
 *
 * Two guards, and they fail for different reasons:
 *
 *   1. the palette contains black, and black is the *same* value `DEFAULT_STYLES` uses -- so the
 *      two cannot drift apart again. Comparing against `DEFAULT_STYLES` rather than restating
 *      `"#111418"` is the point: a hardcoded copy would still pass after someone changed the
 *      default.
 *   2. the rendered control's displayed value equals the active tool's colour, for every tool
 *      that has one. That is the assertion the phantom selection would have failed.
 */

import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { EditorPane, COLOURS, colourOptions } from "./EditorPane";
import { DEFAULT_STYLES, EditorProvider } from "../state/EditorProvider";
import type { AnnotationKind, LibraryEntry } from "../api/types";
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

/** The tool button whose label the palette belongs to. */
const TOOL_LABELS: Record<string, string> = {
  highlight: "Highlight",
  underline: "Underline",
  strikeout: "Strike out",
  squiggly: "Squiggly",
  note: "Note",
  textbox: "Text box",
  callout: "Callout",
  typewriter: "Typewriter",
  ink: "Ink",
  line: "Line",
  arrow: "Arrow",
  rect: "Box",
  oval: "Oval",
};

/** The colour `<select>`, found by its label so a layout change cannot retarget it. */
const colourSelect = () => screen.getByLabelText("Colour") as HTMLSelectElement;

let open: { teardown(): void } | null = null;

afterEach(() => {
  open?.teardown();
  open = null;
});

function renderPane(appState: Record<string, string> = {}) {
  const backend = installFakeBackend();
  backend.on("list_annotations", () => []);
  backend.on("get_app_state", (args: unknown) => {
    const { key } = args as { key: string };
    return appState[key] ?? null;
  });
  backend.on("set_app_state", () => null);

  const view = render(
    <EditorProvider documentId="doc-1" page={0}>
      <EditorPane entry={entry} currentPage={0} />
    </EditorProvider>,
  );
  open = backend;
  return view;
}

describe("D15: the palette can produce black", () => {
  it("offers Black, using exactly the value the text tools default to", () => {
    const black = COLOURS.find((colour) => colour.label === "Black");

    // Present at all -- this is the reported defect.
    expect(black).toBeDefined();
    // And it is the *same* value the text tools draw in. Read from DEFAULT_STYLES rather than
    // restated, so changing the default without touching the palette fails here instead of
    // quietly reintroducing the phantom selection.
    for (const kind of ["textbox", "callout", "typewriter"] as AnnotationKind[]) {
      expect(black!.value).toBe(DEFAULT_STYLES[kind].color);
    }
  });

  it("puts Black first, because it is the common case", () => {
    expect(COLOURS[0].label).toBe("Black");
  });

  it("keeps every pastel the highlighter needs", () => {
    const labels = COLOURS.map((colour) => colour.label);
    // Adding black must not cost the palette its actual job. Named individually so removing one
    // to make room is a failure rather than a silent palette change.
    for (const pastel of ["Yellow", "Green", "Blue", "Pink", "Orange", "Red"]) {
      expect(labels).toContain(pastel);
    }
    expect(COLOURS).toHaveLength(7);
  });

  it("can display every colour any tool actually draws in", () => {
    // The general form of the original bug: a tool whose default the control cannot show is a
    // tool whose picker lies. Checked for *every* kind, so a future default that misses the
    // palette fails here rather than at the user.
    //
    // Deliberately `colourOptions` rather than `COLOURS`: three vector tools default to
    // `#1f6feb`, which is not one of the highlight pastels and should not become one. The
    // requirement is that the control *reflects* the colour in force, not that every default is
    // a named swatch -- `colourOptions` appends the unknown value so the shown label is exact.
    for (const [kind, style] of Object.entries(DEFAULT_STYLES)) {
      const shown = colourOptions(style.color);
      expect(
        shown.some((option) => option.value.toLowerCase() === style.color.toLowerCase()),
        `${kind} draws ${style.color} but the control cannot display it`,
      ).toBe(true);
    }
  });

  it("appends an unlisted default rather than misreporting it", () => {
    // `#1f6feb` is underline/rect/oval's default and is not a highlight pastel. It must be shown
    // as itself; swapping in the pastel "Blue" (#9fd8ff) would label the control with a colour
    // the tool does not draw in -- the original defect, in a different row.
    const shown = colourOptions("#1f6feb");
    expect(shown[shown.length - 1]).toEqual({ value: "#1f6feb", label: "#1f6feb" });
    // And it must not have replaced the pastel that already had that name.
    expect(shown.find((option) => option.label === "Blue")?.value).toBe("#9fd8ff");
  });
});

describe("D15: the control shows the colour that will be drawn", () => {
  it("shows the active tool's colour for every tool, never a phantom", async () => {
    renderPane();

    for (const [kind, label] of Object.entries(TOOL_LABELS)) {
      fireEvent.click(screen.getByRole("button", { name: label }));

      // The style loads asynchronously, so wait for the value rather than reading the first
      // paint -- a transient phantom during loading is still a phantom the user can see.
      await waitFor(() => {
        const select = colourSelect();
        expect(
          select.value,
          `${label} shows ${select.value} but draws ${DEFAULT_STYLES[kind as AnnotationKind].color}`,
        ).toBe(DEFAULT_STYLES[kind as AnnotationKind].color);
      });

      // And the shown option is a real option, not an empty selection. A `<select>` with a
      // value matching no option reports `value === ""` in some engines and the first option in
      // others; either way the label would not name the colour in force.
      const select = colourSelect();
      expect(select.selectedOptions).toHaveLength(1);
      expect(select.selectedOptions[0].value).toBe(select.value);
    }
  });

  it("names the current colour in words, so the closed control is readable", async () => {
    renderPane();

    fireEvent.click(screen.getByRole("button", { name: "Text box" }));

    await waitFor(() => expect(colourSelect().value).toBe(DEFAULT_STYLES.textbox.color));
    // The closed native control shows the option's text, which is why Black had to have a label
    // rather than a hex string.
    expect(colourSelect().selectedOptions[0].textContent).toBe("Black");
  });

  it("still reports a stored colour the palette does not contain", async () => {
    // A style written by an older build. Falling back would change what is drawn to match a
    // label; the requirement is the other direction -- show what will actually be used.
    renderPane({ "annotation.style.textbox": JSON.stringify({ color: "#00ff00", opacity: 1 }) });

    fireEvent.click(screen.getByRole("button", { name: "Text box" }));

    await waitFor(() => expect(colourSelect().value).toBe("#00ff00"));
    // Shown as its own value rather than silently swapped for a pastel that is not in force.
    expect(colourSelect().selectedOptions[0].textContent).toBe("#00ff00");
  });
});
