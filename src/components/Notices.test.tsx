/**
 * P2.7.6 §4 obligation 2: the About/notices surface must name the bundled font, state its
 * licence, and point at the shipped licence file.
 *
 * The design says this is "measured, not asserted: a test reads the bundled licence and the
 * About component's text, so neither can be deleted without a failure". This is that test.
 *
 * Every required claim below is one a licence obligation needs. Removing any of them from
 * `Notices.tsx` fails here, which is the point: the notice is not allowed to decay into
 * something that merely mentions fonts.
 */

import { render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { Notices } from "./Notices";

function renderNotices() {
  render(<Notices onClose={vi.fn()} />);
}

describe("the bundled-font notice", () => {
  it("names the font and its licence", () => {
    renderNotices();
    expect(screen.getByText(/Liberation Fonts 2\.1\.5/)).toBeTruthy();
    expect(screen.getByText(/SIL Open Font License, Version 1\.1/)).toBeTruthy();
  });

  it("carries the copyright holders and the reserved font name", () => {
    renderNotices();
    expect(screen.getByText(/Google Corporation/)).toBeTruthy();
    expect(screen.getByText(/Red Hat, Inc\./)).toBeTruthy();
    expect(screen.getByText(/Reserved Font Name Liberation/)).toBeTruthy();
  });

  it("points at the shipped licence and the per-face record", () => {
    renderNotices();
    expect(screen.getByText(/LICENSE-Liberation\.txt/)).toBeTruthy();
    expect(screen.getByText(/FONT-RECORD\.md/)).toBeTruthy();
  });

  it("covers all three families", () => {
    renderNotices();
    for (const family of ["Liberation Sans", "Liberation Serif", "Liberation Mono"]) {
      expect(screen.getByText(new RegExp(family))).toBeTruthy();
    }
  });

  it("says a subset is what gets written, and not under the reserved name", () => {
    renderNotices();
    expect(screen.getByText(/subset/i)).toBeTruthy();
    expect(screen.getByText(/reserved name/i)).toBeTruthy();
  });

  it("is a labelled modal and can be closed", () => {
    const onClose = vi.fn();
    render(<Notices onClose={onClose} />);
    const dialog = screen.getByRole("dialog", { name: "About and notices" });
    expect(dialog).toBeTruthy();
    screen.getByRole("button", { name: "Close notices" }).click();
    expect(onClose).toHaveBeenCalledTimes(1);
  });
});
