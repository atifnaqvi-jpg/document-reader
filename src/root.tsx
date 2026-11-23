import { StrictMode } from "react";
import App from "./App";
import { ErrorBoundary } from "./components/ErrorBoundary";

/**
 * The tree the window mounts, kept separate from `main.tsx` so a test can render the real
 * thing. `main.tsx` has a side effect (it creates the root), so it cannot be imported in a
 * test without mounting twice; the tree it renders can.
 *
 * The order is the whole point: `ErrorBoundary` sits **outside** `App` so it survives the
 * throw that unmounts `App`, and **inside** `StrictMode` so development double-rendering is
 * unchanged. A render throw reaching the root otherwise leaves a white window with the stack
 * only in a console the user is not reading (D10).
 */
export function Root() {
  return (
    <StrictMode>
      <ErrorBoundary>
        <App />
      </ErrorBoundary>
    </StrictMode>
  );
}
