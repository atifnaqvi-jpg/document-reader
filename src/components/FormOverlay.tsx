import { useCallback, useRef, useState } from "react";
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
  /** Writes a field. Resolves `true` when the engine accepted the fill. */
  onFill: (field: string, value: string) => Promise<boolean>;
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
  return null;
}

export function FormOverlay({ metrics, fields, onFill }: FormOverlayProps) {
  // The in-progress string per field. Text lives here until blur: a write per keystroke would
  // produce a new file per character under the P2-D2 versioned-save discipline (design §10.3).
  const [drafts, setDrafts] = useState<Record<string, string>>({});
  const [busy, setBusy] = useState<Record<string, boolean>>({});
  const [refused, setRefused] = useState<Record<string, string>>({});
  // The last value each field was committed with, so a blur that re-fires after a commit does
  // not write the same value again.
  const committed = useRef<Record<string, string>>({});

  const commit = useCallback(
    async (field: FormField, value: string) => {
      if (!isFillable(field)) return;
      if (committed.current[field.name] === value) return;
      setBusy((current) => ({ ...current, [field.name]: true }));
      let accepted = false;
      try {
        accepted = await onFill(field.name, value);
      } finally {
        setBusy((current) => ({ ...current, [field.name]: false }));
      }
      if (accepted) {
        committed.current[field.name] = value;
        setRefused((current) => {
          const next = { ...current };
          delete next[field.name];
          return next;
        });
        // Drop the draft only once the value is in the file: clearing it first would blank the
        // box the user is looking at if the write then failed.
        setDrafts((current) => {
          const next = { ...current };
          delete next[field.name];
          return next;
        });
      } else {
        setRefused((current) => ({ ...current, [field.name]: "This field could not be changed." }));
      }
    },
    [onFill],
  );

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
          const target = buttonFillValue(field);
          const checked = buttonIsSet(field);
          return (
            <button
              key={widget.key}
              type="button"
              className={`form-widget form-widget-button${checked ? " form-widget-checked" : ""}`}
              style={box}
              data-field={field.name}
              aria-label={field.name}
              aria-pressed={checked}
              disabled={busy[field.name] === true || target === null}
              onClick={() => {
                // Toggling off needs the `Off` ruling (design §10.6), so a set button is inert
                // rather than silently re-writing the same value.
                if (target === null || checked) return;
                void commit(field, target);
              }}
            />
          );
        }

        const value = drafts[field.name] ?? field.value ?? "";
        return (
          <input
            key={widget.key}
            type="text"
            className="form-widget form-widget-text"
            style={box}
            data-field={field.name}
            aria-label={field.name}
            aria-invalid={refused[field.name] ? true : undefined}
            value={value}
            disabled={busy[field.name] === true}
            onChange={(event: ChangeEvent<HTMLInputElement>) =>
              setDrafts((current) => ({ ...current, [field.name]: event.target.value }))
            }
            onBlur={(event) => void commit(field, event.target.value)}
            onKeyDown={(event) => {
              // Enter commits without leaving the field, so a form can be filled without
              // clicking away between fields.
              if (event.key === "Enter") void commit(field, event.currentTarget.value);
            }}
          />
        );
      })}
    </div>
  );
}
