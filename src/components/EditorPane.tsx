import { useState } from "react";
import { nextSavePath, saveEdits, toAppError } from "../api/invoke";
import type { AnnotationKind, AppError, LibraryEntry } from "../api/types";
import { useEditor } from "../state/EditorProvider";

/**
 * Editing tools (docs/P2-DESIGN.md D9/D10).
 *
 * The palette drives {@link useEditor}, which owns the active tool and the per-tool
 * style. Nothing here talks to the backend directly except "save edits", because a
 * tool's whole job is to set state the overlay reads.
 */

const TOOLS: { kind: AnnotationKind; label: string; hint: string }[] = [
  { kind: "highlight", label: "Highlight", hint: "Drag over text" },
  { kind: "underline", label: "Underline", hint: "Drag under text" },
  { kind: "strikeout", label: "Strike out", hint: "Drag over text" },
  { kind: "note", label: "Note", hint: "Click to place, then type" },
  { kind: "ink", label: "Ink", hint: "Drag to draw" },
  { kind: "shape", label: "Box", hint: "Drag to draw a box" },
];

/** Colours offered per tool; a full picker would be its own change, not a P2 need. */
const COLOURS = ["#fff3b0", "#a8e6a1", "#9fd8ff", "#ffb3c1", "#f5a623", "#d1242f"];

export function EditorPane({ entry }: { entry: LibraryEntry | null }) {
  const {
    activeTool,
    setActiveTool,
    style,
    setStyle,
    canUndo,
    canRedo,
    undo,
    redo,
    error,
    clearError,
  } = useEditor();

  const [saveError, setSaveError] = useState<AppError | null>(null);
  const [saving, setSaving] = useState(false);
  const [pendingPath, setPendingPath] = useState<string | null>(null);
  const [savedPath, setSavedPath] = useState<string | null>(null);

  if (!entry) return null;

  /**
   * Shows the filename before writing it (D2).
   *
   * The path is resolved on the Rust side, and the confirm step exists because "save"
   * that silently writes `... (edited 3).pdf` is surprising the first time. The original
   * is never the target, so this dialog has no "overwrite" variant.
   */
  const beginSave = async () => {
    setSaveError(null);
    setSavedPath(null);
    try {
      setPendingPath(await nextSavePath(entry.id));
    } catch (e) {
      setSaveError(toAppError(e));
    }
  };

  const confirmSave = async () => {
    setSaving(true);
    setSaveError(null);
    try {
      setSavedPath(await saveEdits(entry.id));
      setPendingPath(null);
    } catch (e) {
      setSaveError(toAppError(e));
    } finally {
      setSaving(false);
    }
  };

  return (
    <section className="editor-pane" aria-label="Editing tools">
      <div className="editor-tools">
        {TOOLS.map((tool) => (
          <button
            key={tool.kind}
            type="button"
            title={tool.hint}
            aria-pressed={activeTool === tool.kind}
            className={activeTool === tool.kind ? "active" : undefined}
            // Clicking the active tool turns it off, which is how a user stops drawing
            // and goes back to reading without hunting for a "select" tool.
            onClick={() => setActiveTool(activeTool === tool.kind ? null : tool.kind)}
          >
            {tool.label}
          </button>
        ))}
      </div>

      {activeTool ? (
        <div className="editor-style">
          <label>
            Colour
            <select
              value={style.color}
              onChange={(event) => setStyle({ ...style, color: event.target.value })}
            >
              {COLOURS.map((colour) => (
                <option key={colour} value={colour}>
                  {colour}
                </option>
              ))}
            </select>
          </label>
          <label>
            Opacity
            <input
              type="range"
              min={0.1}
              max={1}
              step={0.1}
              value={style.opacity}
              onChange={(event) => setStyle({ ...style, opacity: Number(event.target.value) })}
            />
          </label>
        </div>
      ) : null}

      <div className="editor-actions">
        <button type="button" onClick={undo} disabled={!canUndo}>
          Undo
        </button>
        <button type="button" onClick={redo} disabled={!canRedo}>
          Redo
        </button>
        <button type="button" onClick={beginSave} disabled={saving}>
          Save edits…
        </button>
      </div>

      {pendingPath ? (
        <div className="editor-confirm" role="dialog" aria-label="Confirm save">
          <p>
            Save edits as <span className="path">{pendingPath}</span>?
          </p>
          <p className="muted">Your original file will not be changed.</p>
          <div>
            <button type="button" onClick={confirmSave} disabled={saving}>
              {saving ? "Saving…" : "Save"}
            </button>
            <button type="button" onClick={() => setPendingPath(null)} disabled={saving}>
              Cancel
            </button>
          </div>
        </div>
      ) : null}

      {savedPath ? (
        <p className="editor-saved" role="status">
          Saved to <span className="path">{savedPath}</span>
        </p>
      ) : null}

      {saveError ?? error ? (
        <div className="editor-error" role="alert">
          <p>{(saveError ?? error)!.message}</p>
          <button
            type="button"
            onClick={() => {
              setSaveError(null);
              clearError();
            }}
          >
            Dismiss
          </button>
        </div>
      ) : null}
    </section>
  );
}
