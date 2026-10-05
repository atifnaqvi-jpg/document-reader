/**
 * Vitest setup. Runs before every test file.
 *
 * jsdom has no layout engine, so `getBoundingClientRect` returns zeros and
 * `setPointerCapture` does not exist. That is not a gap to paper over here: it is why
 * geometry lives in pure functions (`src/state/geometry.ts`) that tests call directly,
 * rather than in assertions about rendered CSS. Anything that genuinely needs pixels
 * (annotation marks against a real page raster) belongs in the Rust suite against
 * PDFium, not here.
 */

import "@testing-library/jest-dom/vitest";
import { cleanup } from "@testing-library/react";
import { afterEach } from "vitest";

// React Testing Library auto-registers its cleanup only when a global `afterEach` exists.
// This project runs with `globals: false`, so without this the DOM from one test survives
// into the next and queries fail with "found multiple elements" -- a confusing failure that
// looks like a component bug.
afterEach(() => {
  cleanup();
});

// jsdom does not implement pointer capture. `AnnotationOverlay` calls it on pointerdown
// to keep a drag alive outside the element, so without these stubs every drag test would
// throw before reaching an assertion. No-ops rather than assertions: capture is platform
// behaviour, and faking it would only test the fake.
if (!HTMLElement.prototype.setPointerCapture) {
  HTMLElement.prototype.setPointerCapture = () => {};
  HTMLElement.prototype.releasePointerCapture = () => {};
  HTMLElement.prototype.hasPointerCapture = () => false;
}
