import { Component } from "react";
import type { ErrorInfo, ReactNode } from "react";

/**
 * Last-resort diagnostic for a render that throws (D10).
 *
 * ## What this is, and what it is not
 *
 * This is **instrumentation, not a design answer.** It exists for exactly one failure the
 * product already had: placing a text box blanked the whole window, and a blank window names
 * nothing. A React render throw that reaches the root unmounts the entire tree, so the user
 * gets white space where the app was, and the stack that would identify the bug is visible
 * only in a devtools console the user is not reading.
 *
 * So the boundary swaps "white" for "the error, its name, and its stack, on screen". That
 * turns an unreproducible report into a fix unit. It is deliberately ugly and deliberately
 * unstyled: it must render when the thing that broke *is* the stylesheet or the theme
 * provider, so every rule here is inline and nothing is imported.
 *
 * ## Why a render throw is the right thing to catch
 *
 * React unmounts the root only for errors thrown during render or in a lifecycle. A throw
 * inside an event handler does **not** blank the app -- React logs it and keeps the tree --
 * so a white window is specifically a render-phase throw, and this is the boundary that sees
 * it. Errors inside promises and timers are likewise outside both this and React's own
 * handling; those surface as a rejected promise, not a blank window, and are out of scope
 * for this instrument.
 *
 * ## Why it stays mounted in the tree
 *
 * `componentDidCatch` also writes to the console, so the same stack reaches a devtools
 * session. Belt and braces: a screenshot of the panel is the expected route (the user is
 * running an installed build with no console), and the console copy costs nothing.
 */

interface ErrorBoundaryState {
  error: Error | null;
  componentStack: string | null;
}

/**
 * Text of an unknown thrown value.
 *
 * A boundary must survive whatever was thrown: `getDerivedStateFromError` can receive a
 * string, a rejected Response, or `undefined` from a library that does `throw undefined`.
 * Calling `error.stack` on those throws again, inside the boundary itself, which produces a
 * second unmount and the blank window this component exists to prevent.
 */
function describe(thrown: unknown): { name: string; message: string; stack: string } {
  if (thrown instanceof Error) {
    return {
      name: thrown.name || "Error",
      message: thrown.message || "(no message)",
      stack: thrown.stack ?? "(no stack was captured for this error)",
    };
  }
  let rendered: string;
  try {
    rendered = typeof thrown === "string" ? thrown : JSON.stringify(thrown) ?? String(thrown);
  } catch {
    // A cyclic or otherwise unserialisable value must not throw here.
    rendered = String(thrown);
  }
  return { name: typeof thrown, message: rendered, stack: "(not an Error; no stack)" };
}

export interface ErrorBoundaryProps {
  children: ReactNode;
  /** Overridden by the tests so a deliberate throw does not pollute the console. */
  onError?: (error: Error, info: ErrorInfo) => void;
}

export class ErrorBoundary extends Component<ErrorBoundaryProps, ErrorBoundaryState> {
  state: ErrorBoundaryState = { error: null, componentStack: null };

  static getDerivedStateFromError(thrown: unknown): ErrorBoundaryState {
    // React requires an Error instance for its own bookkeeping; the panel renders from the
    // raw value, so wrapping is enough and keeps React from rethrowing.
    const error = thrown instanceof Error ? thrown : new Error(describe(thrown).message);
    return { error, componentStack: null };
  }

  componentDidCatch(thrown: unknown, info: ErrorInfo): void {
    this.setState({ componentStack: info.componentStack ?? null });
    const report = this.props.onError;
    if (report) {
      report(thrown instanceof Error ? thrown : new Error(describe(thrown).message), info);
      return;
    }
    console.error("render threw; the boundary caught it", thrown, info.componentStack);
  }

  render(): ReactNode {
    const { error, componentStack } = this.state;
    if (!error) return this.props.children;

    const { name, message, stack } = describe(error);
    const text = [
      `${name}: ${message}`,
      "",
      stack,
      componentStack ? `\n--- React component stack ---${componentStack}` : "",
    ]
      .join("\n")
      .trim();

    return (
      <div
        // `role="alert"` is deliberate: this panel must be announced, not merely shown.
        // `data-testid` carries the identity, because `Alert` is shared with the app's own
        // error banner and a test asserting on the role alone cannot tell them apart.
        role="alert"
        data-testid="render-error-panel"
        style={{
          position: "fixed",
          inset: 0,
          overflow: "auto",
          background: "#1b1f24",
          color: "#e6edf3",
          font: "13px/1.5 ui-monospace, SFMono-Regular, Menlo, Consolas, monospace",
          padding: "24px",
          zIndex: 2147483647,
        }}
      >
        <h1 style={{ margin: "0 0 4px", fontSize: "16px", color: "#ff7b72" }}>
          The window stopped rendering
        </h1>
        <p style={{ margin: "0 0 16px", color: "#8b949e" }}>
          This panel replaces the blank window on purpose. Please screenshot it, or use
          &ldquo;Copy stack&rdquo; — the stack below is what identifies the defect.
        </p>
        <p style={{ margin: "0 0 12px", font: "inherit", fontSize: "14px" }}>
          <strong style={{ color: "#ffa657" }}>{name}</strong>
          {": "}
          {message}
        </p>
        <pre
          style={{
            margin: "0 0 16px",
            padding: "12px",
            background: "#0d1117",
            border: "1px solid #30363d",
            borderRadius: "6px",
            whiteSpace: "pre-wrap",
            wordBreak: "break-word",
            userSelect: "text",
          }}
        >
          {text}
        </pre>
        <button
          type="button"
          style={{
            font: "inherit",
            padding: "6px 12px",
            color: "#e6edf3",
            background: "#21262d",
            border: "1px solid #30363d",
            borderRadius: "6px",
            cursor: "pointer",
          }}
          onClick={() => {
            void navigator.clipboard?.writeText(text).catch(() => {
              // A clipboard refusal is not worth a second failure; the text is selectable.
            });
          }}
        >
          Copy stack
        </button>
      </div>
    );
  }
}
