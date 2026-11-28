import { useEffect, useState } from "react";
import {
  addWatermark,
  deletePages,
  exportFlattened,
  extractPages,
  nextSavePath,
  overwriteEdits,
  rotatePage,
  saveEdits,
  splitDocument,
  toAppError,
} from "../api/invoke";
import type { AnnotationKind, AppError, LibraryEntry } from "../api/types";
import { useEditor } from "../state/EditorProvider";
import { TEXT_EDIT_ERROR_CODES, TextEditPanel } from "./TextEdit";

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
  { kind: "squiggly", label: "Squiggly", hint: "Drag under text" },
  { kind: "note", label: "Note", hint: "Click to place, then type" },
  { kind: "textbox", label: "Text box", hint: "Drag a box, then type" },
  { kind: "callout", label: "Callout", hint: "Drag a box, then drag from it to point" },
  { kind: "typewriter", label: "Typewriter", hint: "Drag a box, then type" },
  { kind: "ink", label: "Ink", hint: "Drag to draw" },
  { kind: "line", label: "Line", hint: "Drag from one end to the other" },
  { kind: "arrow", label: "Arrow", hint: "Drag from tail to tip" },
  { kind: "rect", label: "Box", hint: "Drag to draw a box" },
  { kind: "oval", label: "Oval", hint: "Drag a box" },
];

/**
 * The tools whose stored blob actually carries `opacity` (D16).
 *
 * These four are the *sweep* tools and they share one blob shape, `HighlightData`, which has the
 * field and whose commit writes `style.opacity` into it. `annotate.rs` groups the same four
 * together and passes the value to `annotation_dictionary`, which sets `/CA` on the annotation
 * dictionary -- so the number reaches the file, not just the database.
 *
 * Every other kind is different: `commitFreeText` and the rect/oval branch build
 * `{ v, rect, color }` with no opacity at all, and the backend hardcodes `1.0` for rect and note.
 * For those the slider moved and changed nothing -- the same class of lie as a stale save banner.
 *
 * Listed explicitly rather than derived as "everything except the others": adding a kind must not
 * silently inherit a live slider. Text transparency is its own unit, not a side effect of this
 * one: it needs the field, a commit that carries it, and an `/ca` in the appearance stream that
 * the preview matches.
 */
const STORES_OPACITY: AnnotationKind[] = ["highlight", "underline", "strikeout", "squiggly"];

/** Whether the opacity control applies to `kind`. Exported so a guard names the kinds (D16). */
export function storesOpacity(kind: AnnotationKind | null): boolean {
  return kind !== null && STORES_OPACITY.includes(kind);
}

/**
 * Colours offered per tool; a full picker would be its own change, not a P2 need.
 *
 * Named, because the label is what the user reads. A raw `#fff3b0` in a native Windows `<select>` is
 * a hex string, not a colour: the closed control shows only that text, so the palette read as six
 * codes nobody could choose between. `value` stays the hex, which is what the annotation dict stores.
 *
 * Black comes first and matters most (D15): filling in a form is the job for which the text tools
 * exist, and form text is black. Its value is the same near-black `DEFAULT_STYLES` gives
 * `textbox`, `callout` and `typewriter`, and a guard asserts the two stay equal -- before this,
 * the picker held no option matching the default, so the control displayed a value it could not
 * offer and the user could not get back to the colour the tool actually drew in.
 *
 * The pastels stay. Highlighting and form-filling are different jobs and both are wanted; adding
 * black must not cost the palette its highlighter.
 */
const DEFAULT_TEXT_COLOUR = "#111418";

/** The palette itself, exported so a guard can compare it with `DEFAULT_STYLES` (D15). */
export const COLOURS: { value: string; label: string }[] = [
  { value: DEFAULT_TEXT_COLOUR, label: "Black" },
  { value: "#fff3b0", label: "Yellow" },
  { value: "#a8e6a1", label: "Green" },
  { value: "#9fd8ff", label: "Blue" },
  { value: "#ffb3c1", label: "Pink" },
  { value: "#f5a623", label: "Orange" },
  { value: "#d1242f", label: "Red" },
];

/**
 * The colours to render for `current`, which is not necessarily in `COLOURS`.
 *
 * A colour can come from somewhere other than this list: a style persisted by an older build, or
 * a value written by hand into the app state. A `<select>` whose `value` matches no `<option>`
 * does not render an empty box -- it shows a *different* option than the one in force, or none,
 * so the user reads a colour the tool will not draw in. Rather than fall back (which would change
 * what is drawn to match a lie), the unknown colour is appended and labelled by its own hex, so
 * the control always displays exactly the value that will be used.
 */
export function colourOptions(current: string): { value: string; label: string }[] {
  const known =
    current !== "" &&
    COLOURS.some((colour) => colour.value.toLowerCase() === current.toLowerCase());
  if (known) return COLOURS;
  return [...COLOURS, { value: current, label: current }];
}


export function EditorPane({
  entry,
  currentPage,
}: {
  entry: LibraryEntry | null;
  /** Zero-based page the viewer is showing, so the page tools start on it. */
  currentPage: number;
}) {
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
    annotations,
    selectedId,
    setSelectedId,
    remove,
    decryptCopyToFile,
  } = useEditor();

  const selected = annotations.find((row) => row.id === selectedId) ?? null;

  const [saveError, setSaveError] = useState<AppError | null>(null);
  const [saving, setSaving] = useState(false);
  const [pendingPath, setPendingPath] = useState<string | null>(null);
  const [savedPath, setSavedPath] = useState<string | null>(null);
  // D2: overwriting the original is its own confirmed flow, never the default and never
  // reachable from "Save edits". The prompt names the file so the user confirms a specific
  // document rather than an abstract "overwrite?".
  const [pendingOverwrite, setPendingOverwrite] = useState(false);
  // P2.4 T8: removing protection is deliberate, so it is a three-step flow -- open the form,
  // read what it will do, then confirm with the password. `pendingDecrypt` holds the typed
  // password only long enough to submit it (T10 keeps it out of persistence and events).
  const [decryptOpen, setDecryptOpen] = useState(false);
  const [decryptPassword, setDecryptPassword] = useState("");
  const [decrypting, setDecrypting] = useState(false);
  // P2-F3/F5: page tools and export are "write a new file" operations, so they report
  // their result through the same savedPath channel as a save and share its busy flag.
  // Follows the visible page by default -- the page the user is looking at is almost
  // always the one they mean, and 1 is right for the first page anyway. Typing in the
  // box overrides it until the viewer moves again.
  const [pageOverride, setPageOverride] = useState<number | null>(null);
  const pageOneBased = pageOverride ?? currentPage + 1;
  const [watermark, setWatermark] = useState("");

  // D11: the status banner is a claim about *one file*. It stops being true when the user
  // switches document, and `EditorPane` is not keyed on the entry, so this state used to
  // survive the switch -- an old path kept hanging over a new document and read as "the box
  // you just drew was saved" when nothing had been written (1.4.0 hand-test). Clearing on the
  // document's identity is the narrowest correct bound: an annotation edit on the *same*
  // document leaves the banner alone, because there it is still true.
  //
  // This effect sits ABOVE the `!entry` return, and must stay there (D13). Below it, the
  // empty state rendered ten hooks and stopped, the open state rendered eleven, and React
  // refused the component with `Rendered more hooks than during the previous render` -- a
  // blank window on the one action the user took. Every hook in a component has to run on
  // every render regardless of which branch the render will take; that is what makes the
  // hook list a stable list rather than a function of state.
  const entryId = entry?.id ?? null;
  useEffect(() => {
    setSavedPath(null);
    setSaveError(null);
    setPendingPath(null);
    setPendingOverwrite(false);
  }, [entryId]);

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

  /**
   * Runs a page/export operation, which always produces a new file rather than touching
   * the original. `label` names the operation for the error message, so a failure
   * identifies which tool the user was using.
   */
  const runDerived = async (label: string, operation: () => Promise<string | string[]>) => {
    setSaving(true);
    setSaveError(null);
    setSavedPath(null);
    try {
      const result = await operation();
      setSavedPath(Array.isArray(result) ? result.join("\n") : result);
    } catch (e) {
      setSaveError(toAppError(e));
    } finally {
      setSaving(false);
    }
    void label;
  };

  const confirmOverwrite = async () => {
    setSaving(true);
    setSaveError(null);
    try {
      setSavedPath(await overwriteEdits(entry.id));
      setPendingOverwrite(false);
    } catch (e) {
      setSaveError(toAppError(e));
    } finally {
      setSaving(false);
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

  /**
   * Removes protection from a copy (P2.4 T8).
   *
   * Only reached after the confirmation has been read, and the password is cleared from
   * component state as soon as the call returns, whether it worked or not: a rejected
   * password must not linger in a field for the next attempt to submit by accident.
   */
  const confirmDecrypt = async () => {
    setDecrypting(true);
    setSaveError(null);
    try {
      const outcome = await decryptCopyToFile(decryptPassword);
      if (outcome) {
        setSavedPath(outcome.path);
        setDecryptOpen(false);
      }
    } finally {
      setDecryptPassword("");
      setDecrypting(false);
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
              {colourOptions(style.color).map((colour) => (
                <option key={colour.value} value={colour.value}>
                  {colour.label}
                </option>
              ))}
            </select>
          </label>
          {/* D16: only rendered where the stored blob has the field. A slider that moves and
              changes nothing is worse than no slider -- see STORES_OPACITY. */}
          {storesOpacity(activeTool) ? (
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
          ) : null}
        </div>
      ) : null}

      <fieldset className="editor-pages">
        <legend>Pages and export</legend>
        <label>
          Page
          <input
            type="number"
            min={1}
            value={pageOneBased}
            onChange={(event) => {
              const next = Number(event.target.value);
              // Clearing the box must not snap it back to the viewer's page mid-edit.
              if (Number.isFinite(next) && next >= 1) setPageOverride(next);
            }}
          />
        </label>
        <button
          type="button"
          disabled={saving}
          onClick={() =>
            runDerived("rotate", async () => {
              const result = await rotatePage(entry.id, pageOneBased, 90);
              return `${result.path} (page now ${result.rotation}°)`;
            })
          }
        >
          Rotate 90°
        </button>
        <button
          type="button"
          disabled={saving}
          onClick={() => runDerived("delete", () => deletePages(entry.id, [pageOneBased]))}
        >
          Delete page
        </button>
        <button
          type="button"
          disabled={saving}
          onClick={() => runDerived("extract", () => extractPages(entry.id, [pageOneBased]))}
        >
          Extract page
        </button>
        <button
          type="button"
          disabled={saving}
          onClick={() => runDerived("split", () => splitDocument(entry.id, pageOneBased))}
        >
          Split here
        </button>
        <button
          type="button"
          disabled={saving}
          onClick={() => runDerived("flatten", () => exportFlattened(entry.id))}
        >
          Export flattened
        </button>
        <label>
          Watermark
          <input
            type="text"
            value={watermark}
            placeholder="DRAFT"
            onChange={(event) => setWatermark(event.target.value)}
          />
        </label>
        <button
          type="button"
          disabled={saving || watermark.trim() === ""}
          onClick={() =>
            runDerived("flatten+watermark", () => exportFlattened(entry.id, watermark))
          }
        >
          Export with watermark
        </button>
        <button
          type="button"
          disabled={saving || watermark.trim() === ""}
          onClick={() => runDerived("watermark", () => addWatermark(entry.id, watermark))}
        >
          Watermark a copy
        </button>
        <p className="muted">These write a new file. The original is never changed.</p>
      </fieldset>

      <div className="editor-actions">
        {/* Without this there is no way to remove a mark once drawn: annotations could be
            created but never taken back, and undo only spans the current session. */}
        <button
          type="button"
          disabled={!selected || saving}
          onClick={() => {
            if (!selected) return;
            setSelectedId(null);
            void remove(selected);
          }}
        >
          Delete annotation
        </button>
        <button type="button" onClick={undo} disabled={!canUndo}>
          Undo
        </button>
        <button type="button" onClick={redo} disabled={!canRedo}>
          Redo
        </button>
        <button type="button" onClick={beginSave} disabled={saving}>
          Save edits…
        </button>
        <button
          type="button"
          onClick={() => {
            setSaveError(null);
            setSavedPath(null);
            setPendingOverwrite(true);
          }}
          disabled={saving}
        >
          Overwrite original…
        </button>
        {/* T8: the only path from a protected document to an editable one. Offered whether
            or not the open document is protected, because the user may be about to open one;
            the confirmation states plainly that protection is dropped on the new copy only. */}
        <button
          type="button"
          title="Save an unprotected copy of a password-protected document"
          onClick={() => {
            setSaveError(null);
            setSavedPath(null);
            setDecryptPassword("");
            setDecryptOpen(true);
          }}
          disabled={decrypting}
        >
          Decrypt a copy…
        </button>
      </div>

      <TextEditPanel entry={entry} page={currentPage} />

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

      {pendingOverwrite ? (
        <div className="editor-confirm danger" role="dialog" aria-label="Confirm overwrite">
          <p>
            Overwrite <span className="path">{entry.path}</span> with your annotations?
          </p>
          <p className="muted">
            This replaces the original file. It cannot be undone from here.
          </p>
          <div>
            <button type="button" onClick={confirmOverwrite} disabled={saving}>
              {saving ? "Overwriting…" : "Overwrite"}
            </button>
            <button
              type="button"
              onClick={() => setPendingOverwrite(false)}
              disabled={saving}
            >
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

      {decryptOpen ? (
        <div className="editor-confirm danger" role="dialog" aria-label="Confirm decrypt">
          <p>
            Save an unprotected copy of <span className="path">{entry.path}</span>?
          </p>
          <p className="muted">
            Protection will be removed on the new copy only. The original file is not
            changed and stays protected. Nothing is re-encrypted.
          </p>
          <label>
            Password
            <input
              type="password"
              value={decryptPassword}
              // Autofocus because the dialog exists to take one value; the field is not
              // persisted anywhere and is cleared as soon as the call returns (T10).
              autoFocus
              onChange={(event) => setDecryptPassword(event.target.value)}
            />
          </label>
          <div>
            <button
              type="button"
              onClick={() => void confirmDecrypt()}
              disabled={decrypting || decryptPassword === ""}
            >
              {decrypting ? "Decrypting…" : "Decrypt a copy"}
            </button>
            <button
              type="button"
              onClick={() => {
                setDecryptPassword("");
                setDecryptOpen(false);
              }}
              disabled={decrypting}
            >
              Cancel
            </button>
          </div>
        </div>
      ) : null}

      {saveError ?? (error && !TEXT_EDIT_ERROR_CODES.includes(error.code) ? error : null) ? (
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
