/**
 * The diagnostic boundary catches a render throw and shows the stack (D10).
 *
 * The claim under test is not "a React boundary works" — React's own behaviour — but the
 * three things specific to this component: it catches, it shows a *useful* stack rather
 * than a white window, and it survives a boundary-hostile throw (a non-`Error`, which is
 * what a library doing `throw undefined` produces). The last one matters because a
 * boundary that throws while reporting is a second blank window, which is precisely the
 * symptom it was added to fix.
 */

import { render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { ErrorBoundary } from "./ErrorBoundary";

/** A child that throws during render, which is the only class of throw that blanks the root. */
function Bomb({ thrown }: { thrown: unknown }): never {
  throw thrown;
}

describe("ErrorBoundary", () => {
  it("renders its children when nothing throws", () => {
    render(
      <ErrorBoundary>
        <p>the app</p>
      </ErrorBoundary>,
    );

    expect(screen.getByText("the app")).toBeInTheDocument();
    expect(screen.queryByTestId("render-error-panel")).toBeNull();
  });

  it("shows the error's name, message and stack instead of nothing", () => {
    const onError = vi.fn();
    const thrown = new TypeError("cannot read properties of undefined");
    // A real Error's stack names the file and line; the assertion is that whatever
    // `stack` holds reaches the screen, because that is the fix unit.
    thrown.stack = "TypeError: boom\n    at AnnotationOverlay (AnnotationOverlay.tsx:664:9)";

    render(
      <ErrorBoundary onError={onError}>
        <Bomb thrown={thrown} />
      </ErrorBoundary>,
    );

    const panel = screen.getByTestId("render-error-panel");
    expect(panel).toHaveTextContent("TypeError");
    expect(panel).toHaveTextContent("cannot read properties of undefined");
    expect(panel).toHaveTextContent("AnnotationOverlay.tsx:664:9");
    // The reporter still runs, so a devtools session gets the same information.
    expect(onError).toHaveBeenCalledTimes(1);
  });

  it("reports a thrown value that is not an Error without throwing again", () => {
    const onError = vi.fn();

    render(
      <ErrorBoundary onError={onError}>
        <Bomb thrown={undefined} />
      </ErrorBoundary>,
    );

    // A boundary that threw while describing this would unmount to a white window -- the
    // exact regression. Rendering the panel at all is the assertion.
    expect(screen.getByTestId("render-error-panel")).toHaveTextContent("undefined");
    expect(onError).toHaveBeenCalledTimes(1);
  });

  it("survives a thrown value that cannot be serialised", () => {
    const onError = vi.fn();
    const cyclic: Record<string, unknown> = {};
    cyclic.self = cyclic;

    render(
      <ErrorBoundary onError={onError}>
        <Bomb thrown={cyclic} />
      </ErrorBoundary>,
    );

    // `JSON.stringify` throws on a cycle; the describe step must catch that, not die.
    expect(screen.getByTestId("render-error-panel")).toBeInTheDocument();
    expect(onError).toHaveBeenCalledTimes(1);
  });
});
