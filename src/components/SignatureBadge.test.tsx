/**
 * P3.2 SCOPE-C / D-SG7: the badge renders the backend's four states, and colour follows the
 * backend's boolean rather than the badge string.
 *
 * D-SG7 requires the state to be *rendered*, so a test that only checked the Rust enum would leave
 * the actual obligation unverified. These drive the real `invoke.ts` through the fake IPC boundary,
 * so the command name and argument mapping are exercised too.
 */

import { render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { installFakeBackend } from "../test/tauri";
import type { FakeBackend } from "../test/tauri";
import { SignatureBadgeChip } from "./SignatureBadge";

let backend: FakeBackend | undefined;

afterEach(() => {
  backend?.teardown();
  backend = undefined;
});

function withBadge(badge: unknown) {
  backend = installFakeBackend();
  backend.on("signature_badge", () => badge);
  return backend;
}

describe("the signature badge", () => {
  it("renders `unsigned` for a document with no signature", async () => {
    withBadge({
      badge: "unsigned",
      lines: ["this document carries no signature"],
      isGreen: false,
      certificationLevel: null,
    });
    render(<SignatureBadgeChip documentId="doc-1" />);
    expect(await screen.findByRole("button", { name: "unsigned" })).toBeTruthy();
  });

  it("asks the backend for the document it was given", async () => {
    const fake = withBadge({
      badge: "unsigned",
      lines: [],
      isGreen: false,
      certificationLevel: null,
    });
    render(<SignatureBadgeChip documentId="doc-42" />);
    await waitFor(() => expect(fake.callsTo("signature_badge")).toHaveLength(1));
    expect(fake.callsTo("signature_badge")[0]).toEqual({ documentId: "doc-42" });
  });

  it("renders `unknown` without treating it as an error", async () => {
    // The honest answer offline: intact and readable, but not anchored in any local store.
    withBadge({
      badge: "unknown",
      lines: ["the signature could not be confirmed", "integrity: the signed range was hashed and matched"],
      isGreen: false,
      certificationLevel: null,
    });
    render(<SignatureBadgeChip documentId="doc-1" />);
    const chip = await screen.findByRole("button", { name: "unknown" });
    expect(chip.getAttribute("data-state")).toBe("unknown");
    expect(chip.className).toContain("sig-badge--neutral");
    expect(chip.className).not.toContain("sig-badge--error");
    expect(chip.className).not.toContain("sig-badge--valid");
  });

  it("colours `valid` from isGreen, not from the badge string", async () => {
    // A badge that says "valid" but is not green is the exact false-green this guards: the boolean
    // is the backend's decision and the string is only a label, so a mismatch must not paint green.
    withBadge({
      badge: "valid",
      lines: ["the signature verifies against the bytes as shipped"],
      isGreen: false,
      certificationLevel: null,
    });
    render(<SignatureBadgeChip documentId="doc-1" />);
    const chip = await screen.findByRole("button", { name: "valid" });
    expect(chip.className).not.toContain("sig-badge--valid");
  });

  it("paints green only when the backend says so", async () => {
    withBadge({
      badge: "valid",
      lines: ["the signature verifies against the bytes as shipped"],
      isGreen: true,
      certificationLevel: null,
    });
    render(<SignatureBadgeChip documentId="doc-1" />);
    const chip = await screen.findByRole("button", { name: "valid" });
    expect(chip.className).toContain("sig-badge--valid");
  });

  it("shows the backend's lines verbatim when opened, including what was not checked", async () => {
    const lines = [
      "the signature could not be confirmed",
      "integrity: the signed range was hashed and matched",
      "certificate chain: not anchored in any local trust store",
    ];
    withBadge({ badge: "unknown", lines, isGreen: false, certificationLevel: null });
    render(<SignatureBadgeChip documentId="doc-1" />);
    const chip = await screen.findByRole("button", { name: "unknown" });
    chip.click();
    for (const line of lines) {
      expect(await screen.findByText(line)).toBeTruthy();
    }
  });

  it("reports a failed read as a read failure, never as a verdict", async () => {
    backend = installFakeBackend();
    backend.on("signature_badge", () => {
      throw new Error("disk on fire");
    });
    render(<SignatureBadgeChip documentId="doc-1" />);
    const chip = await screen.findByText(/could not be read/);
    expect(chip.className).toContain("sig-badge--error");
    expect(screen.queryByRole("button", { name: "modified" })).toBeNull();
  });
});

