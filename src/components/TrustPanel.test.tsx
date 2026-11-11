/**
 * P3.2 SCOPE-C: the trust panel reaches the shipped commands, and removal is real.
 *
 * These drive `src/api/invoke.ts` through the fake IPC boundary rather than mocking it away, so the
 * command names and the camelCase argument mapping are exercised too -- the layer that silently rots
 * when Rust's snake_case parameters meet a TypeScript object literal.
 *
 * Three tests carry most of the weight, and each guards a failure that would otherwise be invisible:
 *
 * 1. **The identity sent to `forget_trust_*` is the one the listing returned.** The store identifies
 *    rows by base64 DER. A panel that sent a subject name, or a truncated hash, would look correct
 *    and remove nothing, and the user's badge would keep being decided by an entry they had asked to
 *    drop.
 * 2. **A stale CRL says it is stale.** Past `nextUpdate` a clean list stops counting, so the badge
 *    can change meaning on a date nobody touched. If the panel showed the date neutrally, the user
 *    would keep trusting a list the backend had already stopped believing.
 * 3. **`nextUpdate: null` is "no expiry stated", not "expired" and not a blank.** Those are three
 *    different facts and only one of them agrees with `revocation::decide`, which treats a list that
 *    states no expiry as current.
 */

import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { installFakeBackend } from "../test/tauri";
import type { FakeBackend } from "../test/tauri";
import { TrustPanel, daysUntil, describeDate, isPast } from "./TrustPanel";

let backend: FakeBackend | undefined;

afterEach(() => {
  backend?.teardown();
  backend = undefined;
  vi.restoreAllMocks();
});

/** 2026-01-01T00:00:00Z, so every date in these tests is fixed rather than relative to today. */
const NOW_MS = 1_767_225_600_000;
const NOW_S = NOW_MS / 1000;
const DAY = 86_400;

function anchor(overrides: Record<string, unknown> = {}) {
  return {
    derBase64: "MIIB-anchor-der",
    subject: "CN=Test Root CA",
    notAfter: NOW_S + 365 * DAY,
    addedAt: NOW_S - DAY,
    ...overrides,
  };
}

function crl(overrides: Record<string, unknown> = {}) {
  return {
    derBase64: "MIIB-crl-der",
    issuer: "CN=Test Root CA",
    thisUpdate: NOW_S - DAY,
    nextUpdate: NOW_S + 30 * DAY,
    ...overrides,
  };
}

function withStore(anchors: unknown[], crls: unknown[]) {
  backend = installFakeBackend();
  backend.on("list_trust_anchors", () => anchors);
  backend.on("list_trust_crls", () => crls);
  return backend;
}

describe("the trust panel", () => {
  it("lists a trusted issuer so the user can see what they trusted", async () => {
    withStore([anchor()], []);
    render(<TrustPanel nowMs={NOW_MS} />);
    expect(await screen.findByText("CN=Test Root CA")).toBeTruthy();
  });

  it("says an empty store is why a signature reads unknown", async () => {
    // The empty state is the one a new user always meets, and "nothing here" would not explain the
    // badge they are looking at.
    withStore([], []);
    render(<TrustPanel nowMs={NOW_MS} />);
    expect(await screen.findByText(/every signature will read as unknown/)).toBeTruthy();
  });

  it("reports an unreadable store as an error, not as an empty store", async () => {
    // "There is nothing" and "I could not look" are different facts and only one is reassuring.
    backend = installFakeBackend();
    backend.on("list_trust_anchors", () => {
      throw "the trust store is locked";
    });
    backend.on("list_trust_crls", () => {
      throw "the trust store is locked";
    });
    render(<TrustPanel nowMs={NOW_MS} />);
    expect(await screen.findByRole("alert")).toBeTruthy();
    expect(screen.queryByText(/every signature will read as unknown/)).toBeNull();
  });

  it("sends the base64 identity the listing returned, not the subject", async () => {
    // The store keys on base64 DER. Sending the subject would look right and remove nothing.
    const store = withStore([anchor()], []);
    let received: Record<string, unknown> | undefined;
    store.on("forget_trust_anchor", (args) => {
      received = args;
      return true;
    });
    render(<TrustPanel nowMs={NOW_MS} />);
    await userEvent.click(await screen.findByRole("button", { name: /Stop trusting/ }));
    await waitFor(() => expect(received).toBeDefined());
    expect(received?.derBase64).toBe("MIIB-anchor-der");
  });

  it("removes a revocation list by its own identity", async () => {
    const store = withStore([], [crl()]);
    let received: Record<string, unknown> | undefined;
    store.on("forget_trust_crl", (args) => {
      received = args;
      return true;
    });
    render(<TrustPanel nowMs={NOW_MS} />);
    await userEvent.click(await screen.findByRole("button", { name: /Forget list/ }));
    await waitFor(() => expect(received).toBeDefined());
    expect(received?.derBase64).toBe("MIIB-crl-der");
  });

  it("does not claim a removal that the backend says did not happen", async () => {
    // `forget_*` reports whether a row actually went. Reporting success regardless would leave the
    // user believing an entry no longer decides their badge.
    const store = withStore([anchor()], []);
    store.on("forget_trust_anchor", () => false);
    render(<TrustPanel nowMs={NOW_MS} />);
    await userEvent.click(await screen.findByRole("button", { name: /Stop trusting/ }));
    expect(await screen.findByText(/Nothing was removed/)).toBeTruthy();
    expect(screen.queryByText(/Removed\./)).toBeNull();
  });

  it("marks a revocation list past nextUpdate as no longer evidence", async () => {
    // The badge can change meaning on this date without anyone touching the file.
    withStore([], [crl({ nextUpdate: NOW_S - DAY })]);
    render(<TrustPanel nowMs={NOW_MS} />);
    expect(await screen.findByText(/no longer counts as evidence/)).toBeTruthy();
  });

  it("treats a list with no stated nextUpdate as current, not expired", async () => {
    // `revocation::decide` treats a list with no `nextUpdate` as never expiring by its own terms.
    // Rendering it as expired would make the panel contradict the verdict it sits beside.
    withStore([], [crl({ nextUpdate: null })]);
    render(<TrustPanel nowMs={NOW_MS} />);
    expect(await screen.findByText(/no expiry stated/)).toBeTruthy();
    expect(screen.queryByText(/no longer counts as evidence/)).toBeNull();
  });

  it("warns before a current list expires rather than after", async () => {
    withStore([], [crl({ nextUpdate: NOW_S + 10 * DAY })]);
    render(<TrustPanel nowMs={NOW_MS} />);
    expect(await screen.findByText(/10 days left/)).toBeTruthy();
  });

  it("marks an expired anchor so it is not read as a current endorsement", async () => {
    withStore([anchor({ notAfter: NOW_S - DAY })], []);
    render(<TrustPanel nowMs={NOW_MS} />);
    expect(await screen.findByText(/expired/)).toBeTruthy();
  });
});

describe("the date helpers", () => {
  it("renders a missing date honestly instead of inventing 1970", () => {
    expect(describeDate(0)).toBe("not stated");
    expect(describeDate(null)).toBe("none stated");
  });

  it("does not call an undated row expired", () => {
    // A null `nextUpdate` means never expires by its own terms; treating it as past would mark every
    // such list stale and silently discard the evidence it carries.
    expect(isPast(null, NOW_MS)).toBe(false);
    expect(isPast(0, NOW_MS)).toBe(false);
    expect(isPast(NOW_S - DAY, NOW_MS)).toBe(true);
    expect(isPast(NOW_S + DAY, NOW_MS)).toBe(false);
  });

  it("rounds a part-day remainder up, so twelve hours is not reported as zero days", () => {
    expect(daysUntil(NOW_S + DAY / 2, NOW_MS)).toBe(1);
    expect(daysUntil(NOW_S + 3 * DAY, NOW_MS)).toBe(3);
  });
});
