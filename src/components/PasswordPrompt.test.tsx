/**
 * The prompt has to (a) never show a PDFium internal, (b) say plainly that the last password was
 * wrong, and (c) keep the dialog open on a refusal so the retry is one gesture.
 *
 * Reported as: "failed to open PDF: PdfiumLibraryInternalError(PasswordError, )" with no prompt
 * anywhere. These assert the *wording and controls*, because that banner is the defect.
 */

import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import { PasswordPrompt } from "./PasswordPrompt";

function setup(overrides: { refused?: boolean; busy?: boolean } = {}) {
  const onSubmit = vi.fn();
  const onCancel = vi.fn();
  const utils = render(
    <PasswordPrompt
      ask={{ path: "C:/docs/protected.pdf", refused: overrides.refused ?? false }}
      busy={overrides.busy ?? false}
      onSubmit={onSubmit}
      onCancel={onCancel}
    />,
  );
  return { ...utils, onSubmit, onCancel };
}

describe("PasswordPrompt (D1)", () => {
  it("never mentions a PDFium internal", () => {
    const { container } = setup({ refused: true });
    expect(container.textContent).not.toMatch(/Pdfium/i);
    expect(container.textContent).not.toMatch(/PasswordError/);
  });

  it("names the file so a mistargeted open is visible before typing", () => {
    setup();
    expect(screen.getByTitle("C:/docs/protected.pdf")).toBeTruthy();
  });

  it("says the password was wrong only after a refusal", async () => {
    const first = setup();
    expect(first.queryByRole("alert")).toBeNull();
    first.unmount();

    setup({ refused: true });
    expect(screen.getByRole("alert").textContent).toMatch(/did not open this document/i);
  });

  it("submits the typed password", async () => {
    const { onSubmit } = setup();
    await userEvent.type(screen.getByLabelText("Password"), "secret");
    await userEvent.click(screen.getByRole("button", { name: "Open" }));
    expect(onSubmit).toHaveBeenCalledWith("secret");
  });

  it("refuses an empty password rather than submitting one", async () => {
    const { onSubmit } = setup();
    const open = screen.getByRole("button", { name: "Open" }) as HTMLButtonElement;
    expect(open.disabled).toBe(true);
    await userEvent.click(open);
    expect(onSubmit).not.toHaveBeenCalled();
  });

  it("cancels without submitting", async () => {
    const { onCancel, onSubmit } = setup();
    await userEvent.click(screen.getByRole("button", { name: "Cancel" }));
    expect(onCancel).toHaveBeenCalled();
    expect(onSubmit).not.toHaveBeenCalled();
  });

  it("does not submit twice while a request is in flight", async () => {
    const { onSubmit } = setup({ busy: true });
    await userEvent.type(screen.getByLabelText("Password"), "secret");
    await userEvent.click(screen.getByRole("button", { name: /Opening/ }));
    expect(onSubmit).not.toHaveBeenCalled();
  });
});
