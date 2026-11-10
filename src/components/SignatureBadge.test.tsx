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

  it("renders `modified` as a warning, the one state the whole check exists for", async () => {
    // D-SG7 names four states and this one was the only one with no render test: `unsigned` and
    // `unknown` were covered, `valid` was covered by the two colour tests, and `modified` -- the
    // verdict that tells a user their signed document changed after signing -- was never rendered by
    // any test. It is also the state the `later_revision` measurement exists to produce, so a
    // regression there would have had no frontend net under it.
    withBadge({
      badge: "modified",
      lines: [
        "the document changed after it was signed",
        "integrity: the signed range was hashed and did not match",
      ],
      isGreen: false,
      certificationLevel: null,
    });
    render(<SignatureBadgeChip documentId="doc-1" />);
    const chip = await screen.findByRole("button", { name: "modified" });
    expect(chip.getAttribute("data-state")).toBe("modified");
    // Alarming, and never green: a changed signed document is the one thing the badge must not
    // soften.
    expect(chip.className).toContain("sig-badge--neutral");
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

  it("renders the reason after the lines, so the user learns why the badge is not better", async () => {
    // `unknown` is the state this field exists for: the badge cannot say more, and the reason is
    // the only thing that tells the user an anchor import would help. Without it the honest
    // `unknown` is indistinguishable from a broken verifier.
    const lines = ["the signature could not be confirmed"];
    const reason = "no trust anchors are imported, so this signature cannot be confirmed";
    withBadge({ badge: "unknown", lines, reason, isGreen: false, certificationLevel: null });
    render(<SignatureBadgeChip documentId="doc-1" />);
    const chip = await screen.findByRole("button", { name: "unknown" });
    chip.click();
    expect(await screen.findByText(reason)).toBeTruthy();
    // And the fixed lines are still there: the reason adds to them, never replaces them.
    expect(await screen.findByText(lines[0])).toBeTruthy();
  });

  it("renders nothing extra when the reason is empty or absent", async () => {
    withBadge({
      badge: "valid",
      lines: ["the signature verifies against the bytes as shipped"],
      reason: "",
      isGreen: true,
      certificationLevel: null,
    });
    const { container } = render(<SignatureBadgeChip documentId="doc-1" />);
    const chip = await screen.findByRole("button", { name: "valid" });
    chip.click();
    await screen.findByText("the signature verifies against the bytes as shipped");
    expect(container.querySelector(".sig-badge__reason")).toBeNull();
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

