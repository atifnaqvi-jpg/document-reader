/**
 * The search-hit -> selection branch (P3.1 D-A12).
 *
 * Pure function, no DOM: `App` itself needs a viewer and a PDF backend, and the assertion
 * that matters here is about the branch, not about the render. The nonce behaviour is
 * asserted because a stable object would leave the effect in `EditorProvider` unrun when the
 * user clicks the same hit twice.
 */

import { describe, expect, it } from "vitest";
import { nextSelectRequest } from "./App";

describe("nextSelectRequest", () => {
  it("asks for the annotation a hit names", () => {
    expect(nextSelectRequest("ann-7", null)).toEqual({ id: "ann-7", nonce: 1 });
  });

  it("advances the nonce so the same hit can be clicked twice", () => {
    const first = nextSelectRequest("ann-7", null);
    const second = nextSelectRequest("ann-7", first);
    expect(second).toEqual({ id: "ann-7", nonce: 2 });
    expect(second).not.toBe(first);
  });

  it("clears the selection for a document hit", () => {
    // A document hit carries no id. Leaving the previous selection standing would highlight
    // an annotation the user did not click.
    expect(nextSelectRequest(null, { id: "ann-7", nonce: 3 })).toBeNull();
  });

  it("stays cleared when a second document hit follows", () => {
    expect(nextSelectRequest(null, null)).toBeNull();
  });
});
