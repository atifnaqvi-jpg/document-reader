import { useEffect, useMemo, useRef, useState } from "react";
import type { ChangeEvent } from "react";
import type { FormField } from "../api/types";
import type { PageMetrics } from "./AnnotationOverlay";

/**
 * Puts an interactive control over each form widget on the rendered page (P3.3 D6).
 *
 * Before this existed the engine filled fields correctly and nothing in the app could reach it:
 * PDFium draws widget *appearances* like any other page content, so the form looked interactive
 * and was inert. This layer is the missing one — a sibling of the page `<img>`, sized in rendered
 * pixels exactly as `AnnotationOverlay` is, so a control lands on the widget the user can see.
 *
 * Positioning is in **rendered pixels**, not PDF points: the render already accounts for zoom, so
 * rendered pixels are the space the image is in. A widget rect is `[x0, y0, x1, y1]` in points with
 * y measured up from the page bottom, so y is flipped here — the same conversion `pdfToPage`
 * performs, written out because a widget rect is four numbers rather than a `PdfRect`.
 *
 * A widget whose page the document does not state is **not** drawn. Guessing page 0 would put an
 * input box over unrelated content; an absent box is the honest failure.
 */
export interface FormOverlayProps {
  metrics: PageMetrics;
  /** Every field of the document; this component picks the widgets on `metrics.pageNumber`. */
  fields: FormField[];
  /**
   * Reports the values the user has changed but not yet saved (P3.3 D8).
   *
   * The overlay never writes. It reports what is pending so the surface can offer a Save, and the
   * save is the only thing that produces a file — the P2 rule that the user decides when anything
   * is written. An empty array means "nothing pending".
   */
  onPendingChange?: (values: { name: string; value: string }[]) => void;
  /**
   * Changes to this value mean "a save completed", so drafts are dropped and the values shown are
   * the file's own (P3.3 D7/D8).
   */
  savedAt?: number;
}

/** One widget of one field, placed in rendered pixels. */
export interface PlacedWidget {
  field: FormField;
  key: string;
  left: number;
  top: number;
  width: number;
  height: number;
}

/**
 * Whether this build can write this field — the same predicate as `FormField::is_fillable`.
 *
 * Kept in step with the Rust by intent, not derivation: it is a *fast path* for drawing, and the
 * engine's own refusal is the guard. If the two ever disagree, the user gets a control that
 * refuses with `FORM_FIELD_READ_ONLY` rather than a wrong value, which is the safe direction.
 */
export function isFillable(field: FormField): boolean {
  return !field.readOnly && ["Tx", "Btn", "Ch"].includes(field.fieldType);
}

/** Places the widgets that belong to the page `metrics` describes. */
export function placeWidgets(fields: FormField[], metrics: PageMetrics): PlacedWidget[] {
  const safeScale = metrics.scale > 0 ? metrics.scale : 1;
  const placed: PlacedWidget[] = [];
  for (const field of fields) {
    field.rects.forEach((rect, index) => {
      const page = field.rectPages?.[index] ?? null;
      // The page gate. `null` is "the document does not say", which is not page 0.
      if (page !== metrics.pageNumber) return;
      const [x0, y0, x1, y1] = rect;
      placed.push({
        field,
        key: `${field.name}#${index}`,
        left: Math.min(x0, x1) * safeScale,
        top: (metrics.pageHeightPoints - Math.max(y0, y1)) * safeScale,
        width: Math.abs(x1 - x0) * safeScale,
        height: Math.abs(y1 - y0) * safeScale,
      });
    });
  }
  return placed;
}

/**
 * The value a click on this button should write, or `null` when the field offers no on-state.
 *
 * Taken from the document's own `states`, never a hardcoded `/Yes` or `/On` (D-F3, design §10.4).
 * The engine re-derives the spelling from `/AP /N`, so this picks *which* option the user means,
 * not how it is spelled in the file.
 */
export function buttonFillValue(field: FormField): string | null {
  return field.states.length > 0 ? field.states[0] : null;
}

/** Whether a button reads as set, given its `/V`. */
export function buttonIsSet(field: FormField): boolean {
  return field.value !== null && field.states.includes(field.value);
}

/**
 * Why a widget is drawn inert, or `null` when it is interactive.
 *
 * Separate from `isFillable` so the title can say *which* reason applies: "read-only" and "this
 * build does not fill dropdowns" are different messages to a user staring at a box that does
 * nothing.
 */
export function inertReason(field: FormField): string | null {
  if (field.readOnly) return "Read-only field";
  if (!isFillable(field)) return `Unsupported field type: ${field.fieldType}`;
  if (field.fieldType === "Ch") return "Dropdown and list fields are not fillable in this build";
  // A button with no on-state names is a push button, or a checkbox whose `/AP /N` this build
  // could not read. Either way there is no value a click could write, so the box is inert — and
  // it says so. Reporting it as merely disabled made an unreadable checkbox indistinguishable
  // from a click that did nothing (P3.3 D9).
  if (field.fieldType === "Btn" && field.states.length === 0) {
    return "This button has no on state in the file, so it cannot be ticked here";
  }
  return null;
}

export function FormOverlay({ metrics, fields, onPendingChange, savedAt = 0 }: FormOverlayProps) {
  // Text the user has typed but not saved. Nothing here reaches the engine: the overlay collects
  // edits in memory and the surface writes them once, on an explicit save (P3.3 D8, P2-D2).
  const [drafts, setDrafts] = useState<Record<string, string>>({});
  // Buttons the user has toggled but not saved, keyed by field name. Local for the same reason.
  const [toggled, setToggled] = useState<Record<string, boolean>>({});

  // A completed save clears the in-memory edits so the controls show the file's own values. The
  // file is the truth; holding a stale draft over it would show a value the file may not contain.
  const lastSaved = useRef(savedAt);
  useEffect(() => {
    if (savedAt === lastSaved.current) return;
    lastSaved.current = savedAt;
    setDrafts({});
    setToggled({});
  }, [savedAt]);

  // Report pending edits upward whenever they change. The values are the *unsaved* ones only:
  // a field the user returned to its stored value is not pending.
  const pending = useMemo(() => {
    const values: { name: string; value: string }[] = [];
    for (const [name, value] of Object.entries(drafts)) {
      const field = fields.find((candidate) => candidate.name === name);
      if (field && value !== (field.value ?? "")) values.push({ name, value });
    }
    for (const [name, isSet] of Object.entries(toggled)) {
      const field = fields.find((candidate) => candidate.name === name);
      if (!field) continue;
      const target = buttonFillValue(field);
      if (target === null) continue;
      const stored = buttonIsSet(field);
      if (isSet !== stored) values.push({ name, value: isSet ? target : "Off" });
    }
    return values;
  }, [drafts, toggled, fields]);

  const onPendingChangeRef = useRef(onPendingChange);
  onPendingChangeRef.current = onPendingChange;
  useEffect(() => {
    onPendingChangeRef.current?.(pending);
  }, [pending]);

  const widgets = placeWidgets(fields, metrics);

  return (
    <div
      className="form-overlay"
      style={{ position: "absolute", inset: 0, width: metrics.width, height: metrics.height }}
      role="group"
      aria-label="Form fields"
    >
      {widgets.map((widget) => {
        const { field } = widget;
        const box = {
          position: "absolute" as const,
          left: widget.left,
          top: widget.top,
          width: widget.width,
          height: widget.height,
        };
        const inert = inertReason(field);

        // A field this build cannot write is drawn and inert (D-F7): the user must see it, and a
        // click must not open an editor that would then fail.
        if (inert !== null) {
          return (
            <div
              key={widget.key}
              className="form-widget form-widget-inert"
              style={box}
              data-field={field.name}
              title={inert}
              aria-readonly="true"
            />
          );
        }

        if (field.fieldType === "Btn") {
          // Local intent wins over the stored value while unsaved (P3.3 D8): a click changes what
          // the user *wants*, and only a save changes the file.
          const checked = toggled[field.name] ?? buttonIsSet(field);
          return (
            <button
              key={widget.key}
              type="button"
              className={`form-widget form-widget-button${checked ? " form-widget-checked" : ""}`}
              // Opaque on purpose (P3.3 D7), for the same reason the text box is: the raster
              // underneath already draws a checked box's own tick, and a translucent control would
              // show that mark through the one drawn here — two ticks, slightly offset. Set inline
              // rather than in CSS so the opacity is a property of this component, which is what
              // the D7 test asserts.
              style={{ ...box, background: "#fff" }}
              data-field={field.name}
              aria-label={field.name}
              aria-pressed={checked}
              onClick={() => setToggled((current) => ({ ...current, [field.name]: !checked }))}
            >
              {/* The tick has to be *drawn*, not implied (P3.3 D9). Nothing is written until the
                  user saves, so the page raster keeps showing the widget's own unticked
                  appearance; a tinted box alone reads as "the click did nothing", because the mark
                  the user is looking for never appears. This glyph is the only feedback a click
                  can have before the save, so it is a real mark rather than a colour change. */}
              {checked ? <span className="form-widget-tick" aria-hidden="true" /> : null}
            </button>
          );
        }

        const value = drafts[field.name] ?? field.value ?? "";
        return (
          <input
            key={widget.key}
            type="text"
            className="form-widget form-widget-text"
            // Opaque on purpose (P3.3 D7). The raster underneath already draws this widget's own
            // appearance, and a translucent control lets that text show through as a ghost behind
            // the one the user is typing — two renderings of the same value, slightly offset. An
            // opaque box means exactly one string is visible: this one.
            style={{ ...box, background: "#fff" }}
            data-field={field.name}
            aria-label={field.name}
            value={value}
            onChange={(event: ChangeEvent<HTMLInputElement>) =>
              setDrafts((current) => ({ ...current, [field.name]: event.target.value }))
            }
          />
        );
      })}
    </div>
  );
}
