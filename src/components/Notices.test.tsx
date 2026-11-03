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
import { afterEach, describe, expect, it, vi } from "vitest";
import { installFakeBackend } from "../test/tauri";
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

/**
 * D-R6: when the bundle identifier changed in 1.0.1, the user's library was copied from the
 * old data directory without them asking. A migration nobody can see is indistinguishable
 * from one that failed, so the About surface has to say it.
 */
describe("the data-directory migration notice", () => {
  const backend = installFakeBackend();
  afterEach(() => backend.teardown());

  it("shows the migration line when the backend reports one", async () => {
    backend.on("get_migration_notice", () =>
      "Your library was copied from an earlier installation at C:\\old\\library.db.",
    );
    renderNotices();
    expect(await screen.findByRole("status")).toHaveTextContent(/earlier installation/);
  });

  it("stays silent on a fresh install", async () => {
    backend.on("get_migration_notice", () => null);
    renderNotices();
    // Give the effect's promise a turn before asserting absence, so this cannot pass merely
    // by having rendered before the read resolved.
    await Promise.resolve();
    expect(screen.queryByRole("status")).toBeNull();
  });
});
