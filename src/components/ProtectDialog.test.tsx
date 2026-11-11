/**
 * P3.2 SCOPE-A: the Protect dialog actually calls `protect_document`, with every bit explicit.
 *
 * The defect these tests exist for is not "the dialog looks wrong". It is that `protect_document` was
 * registered, tested and **unreachable** -- so the suite proved things about code the product could
 * not run. A test that only checked the dialog renders would leave that possible, so the first test
 * below asserts the invocation itself.
 *
 * The D-PR3 tests are the sharp ones. lopdf's `Permissions::default()` is `all()`, so a dialog that
 * inherited a default and rendered it would look *identical* to this one while shipping a document
 * granting every ability it appears to withhold. Nothing about the rendering distinguishes them;
 * only the payload does. So the payload is what is asserted.
 */

import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import {
  DEFAULT_PERMISSIONS,
  PERMISSIONS,
  ProtectAction,
  ProtectDialog,
} from "./ProtectDialog";

vi.mock("../api/invoke", () => ({
  protectDocument: vi.fn(),
  toAppError: (error: unknown) => ({
    message: error instanceof Error ? error.message : String(error),
  }),
}));

const { protectDocument } = await import("../api/invoke");
const protectMock = vi.mocked(protectDocument);

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

/** Fill the three password fields so the dialog is submittable. */
function fillPasswords(user = "open-me", owner = "own-me") {
  fireEvent.change(screen.getByLabelText(/password to open/i), { target: { value: user } });
  fireEvent.change(screen.getByLabelText(/repeat the password/i), { target: { value: user } });
  fireEvent.change(screen.getByLabelText(/owner password/i), { target: { value: owner } });
}

const protectButton = () => screen.getByRole("button", { name: /^protect$/i });

describe("ProtectDialog", () => {
  it("calls protect_document, so the command finally has a caller", async () => {
    protectMock.mockResolvedValue({ path: "/tmp/report (protected).pdf", pages: 3 });
    render(<ProtectDialog documentId="doc-1" onClose={() => {}} />);

    fillPasswords();
    fireEvent.click(protectButton());

    await waitFor(() => expect(protectMock).toHaveBeenCalledTimes(1));
    expect(protectMock).toHaveBeenCalledWith(
      "doc-1",
      "open-me",
      "own-me",
      DEFAULT_PERMISSIONS,
    );
  });

  it("sends all seven permissions explicitly, none omitted", async () => {
    protectMock.mockResolvedValue({ path: "/tmp/x.pdf", pages: 1 });
    render(<ProtectDialog documentId="doc-1" onClose={() => {}} />);

    fillPasswords();
    fireEvent.click(protectButton());
    await waitFor(() => expect(protectMock).toHaveBeenCalled());

    const permissions = protectMock.mock.calls[0][3];
    // Every `/P` bit the backend knows about must be present. A missing field would deserialize to
    // `false` in Rust and silently deny an ability the user left ticked -- the mirror of the D-PR3
    // failure and just as invisible.
    expect(Object.keys(permissions).sort()).toEqual(
      [
        "annotate",
        "assemble",
        "copy",
        "fillForms",
        "modify",
        "print",
        "printHighQuality",
      ].sort(),
    );
    // And every one of them is actually ticked on screen, matching what is sent. A dialog whose
    // boxes disagree with its payload is the "looks restricted, grants everything" shape.
    expect(PERMISSIONS).toHaveLength(7);
    for (const permission of PERMISSIONS) {
      const box = screen.getByRole("checkbox", { name: permission.label });
      expect(box).toBeChecked();
      expect(permissions[permission.key]).toBe(true);
    }
  });

  it("unticking a box removes exactly that bit and nothing else", async () => {
    protectMock.mockResolvedValue({ path: "/tmp/x.pdf", pages: 1 });
    render(<ProtectDialog documentId="doc-1" onClose={() => {}} />);

    fillPasswords();
    // Two ticks, not one. With a single untick, a toggle that rebuilt the struct from the
    // all-true default produces the *same* answer and the test silently proves nothing -- that
    // mutant survived until this line was added. Two untickings make the difference visible: any
    // toggle that writes more than the one key it was given re-grants the other box.
    fireEvent.click(screen.getByRole("checkbox", { name: "Copy text and images" }));
    fireEvent.click(screen.getByRole("checkbox", { name: "Change the contents" }));
    fireEvent.click(protectButton());
    await waitFor(() => expect(protectMock).toHaveBeenCalled());

    const permissions = protectMock.mock.calls[0][3];
    expect(permissions.copy).toBe(false);
    expect(permissions.modify).toBe(false);
    // The other five are untouched. A toggle that wrote the whole struct would fail here, and the
    // resulting file would deny abilities the user never chose to remove.
    expect(permissions).toEqual({ ...DEFAULT_PERMISSIONS, copy: false, modify: false });
  });

  it("will not submit while the two opening passwords differ", async () => {
    render(<ProtectDialog documentId="doc-1" onClose={() => {}} />);

    fireEvent.change(screen.getByLabelText(/password to open/i), { target: { value: "a" } });
    fireEvent.change(screen.getByLabelText(/repeat the password/i), { target: { value: "b" } });
    fireEvent.change(screen.getByLabelText(/owner password/i), { target: { value: "c" } });

    expect(protectButton()).toBeDisabled();
    expect(screen.getByText(/do not match/i)).toBeTruthy();
    fireEvent.click(protectButton());
    expect(protectMock).not.toHaveBeenCalled();
  });

  it("refuses an owner password equal to the opening password, and says why", async () => {
    render(<ProtectDialog documentId="doc-1" onClose={() => {}} />);

    // Equal passwords make every permission above advisory: whoever can open the file can also lift
    // its restrictions. Accepting this silently would produce a document whose checkboxes are
    // decoration, which is worse than not offering them.
    fillPasswords("same", "same");

    expect(protectButton()).toBeDisabled();
    expect(screen.getByText(/must differ/i)).toBeTruthy();
    fireEvent.click(protectButton());
    expect(protectMock).not.toHaveBeenCalled();
  });

  it("requires an owner password rather than defaulting it", async () => {
    render(<ProtectDialog documentId="doc-1" onClose={() => {}} />);

    fireEvent.change(screen.getByLabelText(/password to open/i), { target: { value: "a" } });
    fireEvent.change(screen.getByLabelText(/repeat the password/i), { target: { value: "a" } });

    expect(protectButton()).toBeDisabled();
    expect(screen.getByText(/Set an owner password/i)).toBeTruthy();
  });

  it("shows where the copy landed, and says the original was not touched", async () => {
    protectMock.mockResolvedValue({ path: "/papers/report (protected).pdf", pages: 3 });
    render(<ProtectDialog documentId="doc-1" onClose={() => {}} />);

    fillPasswords();
    fireEvent.click(protectButton());

    await waitFor(() =>
      expect(screen.getByText("/papers/report (protected).pdf")).toBeTruthy(),
    );
    expect(screen.getByText(/reopened with the password/i)).toBeTruthy();
    // Stated before the click as well as after: D-PR4 writes a sibling, and a user who expects their
    // original to change will look for the change and find none.
    expect(screen.getByText(/never modified/i)).toBeTruthy();
  });

  it("reports the backend's own sentence when protection fails", async () => {
    // `protect_save` verifies by reopening the output; a failure there is a real refusal with a
    // specific reason. Rewording it in the UI would be a second place for those reasons to drift.
    protectMock.mockRejectedValue(new Error("the written file did not reopen with its password"));
    render(<ProtectDialog documentId="doc-1" onClose={() => {}} />);

    fillPasswords();
    fireEvent.click(protectButton());

    await waitFor(() =>
      expect(screen.getByRole("alert").textContent).toMatch(/did not reopen/i),
    );
  });

  it("clears the passwords after a successful protect", async () => {
    protectMock.mockResolvedValue({ path: "/tmp/x.pdf", pages: 1 });
    render(<ProtectDialog documentId="doc-1" onClose={() => {}} />);

    fillPasswords();
    fireEvent.click(protectButton());
    await waitFor(() => expect(screen.getByText("/tmp/x.pdf")).toBeTruthy());

    // Not retained: the copy has its password, and leaving them in state would let a second click
    // protect again with a password the user has stopped looking at.
    expect(screen.getByLabelText(/password to open/i)).toHaveValue("");
    expect(screen.getByLabelText(/owner password/i)).toHaveValue("");
  });

  it("states the output convention before the user commits", () => {
    render(<ProtectDialog documentId="doc-1" onClose={() => {}} />);
    expect(screen.getByText(/writes a new file beside the original/i)).toBeTruthy();
  });

  it("the toolbar trigger opens the dialog", () => {
    render(<ProtectAction documentId="doc-1" />);
    expect(screen.queryByText(/Protect with password/i)).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: /protect…/i }));
    expect(screen.getByText(/Protect with password/i)).toBeTruthy();
  });
});
