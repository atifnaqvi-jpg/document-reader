import { useCallback, useState } from "react";
import type { CSSProperties, PointerEvent as ReactPointerEvent } from "react";
import type { RedactionMark } from "../api/types";
import { parseRedactionRect } from "../api/types";
import type { PageMetrics } from "./AnnotationOverlay";
import { dragToRect, isDrawable, pdfToPage, pointToPdf } from "../state/geometry";

/**
 * Draws pending redaction marks and captures new ones (P3.2 D-RD1).
 *
 * A sibling of the page `<img>`, sized in rendered pixels exactly as `AnnotationOverlay` is, so a
 * mark stays on the words the user drew it over at any zoom. Conversion to PDF points happens only
 * at the moment a mark is sent to the backend, which is where the rest of the editor keeps it too.
 *
 * Separate from `AnnotationOverlay` rather than another tool inside it, for a reason the two cannot
 * share: annotations are content that ships inside the PDF and can be selected and removed later,
 * while a redaction mark is working state that must never be written into the file at all (I-RD7).
 * Sharing the component would put both in one list whose removal means two different things.
 */

/** How a pending mark is drawn before it is applied. */
const MARK_FILL = "rgba(180, 20, 20, 0.35)";
const MARK_BORDER = "1px solid rgba(150, 10, 10, 0.9)";

export interface RedactionOverlayProps {
  metrics: PageMetrics;
  /** Pending marks, already filtered to this page by the caller. */
  marks: RedactionMark[];
  /**
   * Called with the dragged rectangle in PDF points. The backend re-orders the corners, so this
   * passes them through as produced rather than normalising twice.
   */
  onMark: (rect: { x0: number; y0: number; x1: number; y1: number }) => void;
  /** Whether a drag should create a mark. False on a certified document, where it would be refused. */
  enabled: boolean;
}

interface Drag {
  startX: number;
  startY: number;
  endX: number;
  endY: number;
}

export function RedactionOverlay({
  metrics,
  marks,
  onMark,
  enabled,
}: RedactionOverlayProps) {
  const [drag, setDrag] = useState<Drag | null>(null);

  const localPoint = useCallback(
    (event: ReactPointerEvent<HTMLDivElement>): { x: number; y: number } => {
      const bounds = event.currentTarget.getBoundingClientRect();
      return { x: event.clientX - bounds.left, y: event.clientY - bounds.top };
    },
    [],
  );

  const handlePointerDown = useCallback(
    (event: ReactPointerEvent<HTMLDivElement>) => {
      if (!enabled) return;
      // Only the primary button drags a mark: a right-click is a context menu, and starting a
      // redaction rectangle from one would be a surprising way to begin a destructive operation.
      if (event.button !== 0) return;
      const point = localPoint(event);
      event.currentTarget.setPointerCapture(event.pointerId);
      setDrag({ startX: point.x, startY: point.y, endX: point.x, endY: point.y });
    },
    [enabled, localPoint],
  );

  const handlePointerMove = useCallback(
    (event: ReactPointerEvent<HTMLDivElement>) => {
      if (!drag) return;
      const point = localPoint(event);
      setDrag((current) =>
        current ? { ...current, endX: point.x, endY: point.y } : current,
      );
    },
    [drag, localPoint],
  );

  const finish = useCallback(
    (event: ReactPointerEvent<HTMLDivElement>) => {
      if (!drag) return;
      if (event.currentTarget.hasPointerCapture(event.pointerId)) {
        event.currentTarget.releasePointerCapture(event.pointerId);
      }
      const { startX, startY, endX, endY } = drag;
      setDrag(null);

      // A click rather than a drag: ignore it. A zero-area mark would be stored, applied and remove
      // nothing while the user believed they had redacted something -- the silent under-removal
      // D-RD3 forbids, reached from the UI side. `isDrawable` is the same threshold the annotation
      // tools use, so "too small to be a mark" means one thing in the product.
      if (!isDrawable(dragToRect({ x: startX, y: startY }, { x: endX, y: endY }), metrics.scale)) {
        return;
      }

      const [ax, ay] = pointToPdf(startX, startY, metrics.scale, metrics.pageHeightPoints);
      const [bx, by] = pointToPdf(endX, endY, metrics.scale, metrics.pageHeightPoints);
      onMark({ x0: ax, y0: ay, x1: bx, y1: by });
    },
    [drag, metrics, onMark],
  );

  return (
    <div
      className="redaction-overlay"
      onPointerDown={handlePointerDown}
      onPointerMove={handlePointerMove}
      onPointerUp={finish}
      onPointerCancel={finish}
      style={{
        position: "absolute",
        inset: 0,
        width: metrics.width,
        height: metrics.height,
        // A crosshair states the tool without a label, matching what the text picker does.
        cursor: enabled ? "crosshair" : "not-allowed",
        // The marks are decoration, but the surface must still receive pointer events.
        touchAction: "none",
      }}
      role="application"
      aria-label="Redaction marks"
    >
      {marks.map((mark) => {
        const rect = parseRedactionRect(mark.rect);
        // A mark the frontend cannot parse is skipped rather than drawn at zero size: the backend
        // refuses to store one, so reaching here means the row was written by something else, and a
        // silently invisible mark in the list is worse than one visibly missing from this page.
        if (!rect) return null;
        // `pdfToPage` normalises the corners and flips y, returning rendered pixels -- the same
        // space this overlay is laid out in, so no further scaling happens here.
        const box = pdfToPage(rect, metrics.scale, metrics.pageHeightPoints);
        const style: CSSProperties = {
          position: "absolute",
          left: box.x,
          top: box.y,
          width: box.width,
          height: box.height,
          background: MARK_FILL,
          border: MARK_BORDER,
          pointerEvents: "none",
        };
        return (
          <div
            key={mark.id}
            className="redaction-mark"
            data-mark-id={mark.id}
            title={mark.pattern ? `Redact: ${mark.pattern}` : undefined}
            style={style}
          />
        );
      })}

      {drag ? (
        <DragRect drag={drag} />
      ) : null}
    </div>
  );
}

/** The rectangle as it is being dragged, before it becomes a mark. */
function DragRect({ drag }: { drag: Drag }) {
  const rect = dragToRect(
    { x: drag.startX, y: drag.startY },
    { x: drag.endX, y: drag.endY },
  );
  return (
    <div
      className="redaction-mark redaction-mark-pending"
      style={{
        position: "absolute",
        left: rect.x,
        top: rect.y,
        width: rect.width,
        height: rect.height,
        background: MARK_FILL,
        border: MARK_BORDER,
        pointerEvents: "none",
      }}
    />
  );
}
