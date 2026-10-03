import { useCallback, useRef, useState } from "react";
import type { PointerEvent as ReactPointerEvent } from "react";
import type {
  Annotation,
  AnnotationData,
  AnnotationKind,
  HighlightData,
  InkData,
  NoteData,
  ShapeData,
} from "../api/types";
import { parseAnnotationData } from "../api/types";
import { useEditor } from "../state/EditorProvider";
import { dragToRect, isDrawable, pageToPdf, pdfToPage, pointToPdf } from "../state/geometry";
import type { UiRect } from "../state/geometry";
import { NOTE_ICON_POINTS } from "../state/geometry";

/**
 * Draws saved annotations over the rendered page and captures new ones (P2-D8).
 *
 * The overlay is a sibling of the page `<img>`, sized from the *rendered* pixel
 * dimensions rather than from PDF points: the render already accounts for zoom and DPI,
 * so positioning in rendered pixels is what keeps the marks aligned with the pixels the
 * user is looking at. Conversion to PDF points happens only when an annotation is saved.
 *
 * Annotations are drawn as live DOM rather than stamped into the render. PDFium draws
 * the file's own annotations, but not ones that exist only in the DB yet -- and D3
 * forbids baking marks into the image, because a stamp cannot be selected or removed.
 */

/** Rendered-page metrics needed to convert between UI pixels and PDF points. */
export interface PageMetrics {
  /** Zero-based page being edited; every annotation created here belongs to it. */
  pageNumber: number;
  /** Rendered width/height in pixels, from the backend. */
  width: number;
  height: number;
  /** Unrotated page height in PDF points. */
  pageHeightPoints: number;
  /** Zoom scale, so UI pixels can be converted back to points. */
  scale: number;
}

/** How a tool is captured. Determines what the pointer gesture produces. */
const DRAG_TOOLS: AnnotationKind[] = ["highlight", "underline", "strikeout", "shape", "ink"];

export function AnnotationOverlay({
  metrics,
  annotations,
}: {
  metrics: PageMetrics | null;
  annotations: Annotation[];
}) {
  const { activeTool, style, create, selectedId, setSelectedId } = useEditor();
  const [draft, setDraft] = useState<UiRect | null>(null);
  const [inkPoints, setInkPoints] = useState<{ x: number; y: number }[]>([]);
  const surfaceRef = useRef<HTMLDivElement | null>(null);

  /** Pointer position relative to the overlay, which is what all geometry assumes. */
  const localPoint = useCallback((event: ReactPointerEvent) => {
    const bounds = surfaceRef.current?.getBoundingClientRect();
    if (!bounds) return { x: 0, y: 0 };
    return { x: event.clientX - bounds.left, y: event.clientY - bounds.top };
  }, []);

  const start = useRef<{ x: number; y: number } | null>(null);

  const handlePointerDown = useCallback(
    (event: ReactPointerEvent) => {
      if (!activeTool || !metrics) return;
      // A note is a single click, not a drag: its blob stores a point.
      const point = localPoint(event);

      if (activeTool === "note") {
        const [x, y] = pointToPdf(point.x, point.y, metrics.scale, metrics.pageHeightPoints);
        const data: NoteData = { v: 1, point: [x, y], color: style.color };
        void create(metrics.pageNumber, "note", data, null);
        return;
      }

      if (!DRAG_TOOLS.includes(activeTool)) return;
      (event.target as HTMLElement).setPointerCapture(event.pointerId);
      start.current = point;
      if (activeTool === "ink") setInkPoints([point]);
      else setDraft({ x: point.x, y: point.y, width: 0, height: 0 });
    },
    [activeTool, metrics, localPoint, create, style.color],
  );

  const handlePointerMove = useCallback(
    (event: ReactPointerEvent) => {
      if (!activeTool || !metrics || !start.current) return;
      const point = localPoint(event);
      if (activeTool === "ink") setInkPoints((previous) => [...previous, point]);
      else setDraft(dragToRect(start.current!, point));
    },
    [activeTool, metrics, localPoint],
  );

  const finish = useCallback(
    async (event: ReactPointerEvent) => {
      if (!activeTool || !metrics || !start.current) return;
      const origin = start.current;
      start.current = null;
      const point = localPoint(event);

      if (activeTool === "ink") {
        const points = [...inkPoints, point];
        setInkPoints([]);
        // Fewer than two points is a stray tap; a one-vertex stroke has nothing to draw.
        if (points.length < 2) return;
        const strokes = [
          points.map((p) => pointToPdf(p.x, p.y, metrics.scale, metrics.pageHeightPoints)),
        ];
        const data: InkData = { v: 1, strokes, color: style.color };
        await create(metrics.pageNumber, "ink", data, null);
        return;
      }

      const rect = dragToRect(origin, point);
      setDraft(null);
      if (!isDrawable(rect, metrics.scale)) return;

      // Each tool has its own blob shape (D4). One object carrying every field would
      // work at runtime and rot the moment a tool's geometry changes, so the branch
      // is explicit.
      const pdf = pageRectToPdf(rect, metrics);
      const data: AnnotationData =
        activeTool === "shape"
          ? { v: 1, rect: pdf, color: style.color }
          : {
              v: 1,
              rects: [pdf],
              color: style.color,
              opacity: style.opacity,
            };

      await create(metrics.pageNumber, activeTool, data, null);
    },
    [activeTool, metrics, localPoint, inkPoints, create, style],
  );

  if (!metrics) return null;

  return (
    <div
      // Keying on the page drops any in-flight draft when the page turns. Without it a
      // half-drawn highlight would survive the turn and be committed to the next page.
      key={metrics.pageNumber}
      ref={surfaceRef}
      className={`annotation-overlay${activeTool ? " drawing" : ""}`}
      style={{ width: metrics.width, height: metrics.height }}
      onPointerDown={handlePointerDown}
      onPointerMove={handlePointerMove}
      onPointerUp={finish}
      onPointerCancel={() => {
        start.current = null;
        setDraft(null);
        setInkPoints([]);
      }}
    >
      {annotations.map((annotation) => (
        <AnnotationMark
          key={annotation.id}
          annotation={annotation}
          metrics={metrics}
          selected={annotation.id === selectedId}
          onSelect={() => setSelectedId(annotation.id === selectedId ? null : annotation.id)}
        />
      ))}

      {draft ? (
        <div
          className="annotation-draft"
          style={{
            left: draft.x,
            top: draft.y,
            width: draft.width,
            height: draft.height,
            borderColor: style.color,
          }}
        />
      ) : null}

      {inkPoints.length > 1 ? (
        <svg className="annotation-draft-ink" width={metrics.width} height={metrics.height}>
          <polyline
            points={inkPoints.map((p) => `${p.x},${p.y}`).join(" ")}
            fill="none"
            stroke={style.color}
            strokeWidth={2}
          />
        </svg>
      ) : null}
    </div>
  );
}

/** One saved annotation, positioned from its stored blob. */
function AnnotationMark({
  annotation,
  metrics,
  selected,
  onSelect,
}: {
  annotation: Annotation;
  metrics: PageMetrics;
  selected: boolean;
  onSelect: () => void;
}) {
  // A blob from a newer build is not drawn rather than guessed at, which is the same
  // rule the Rust write path applies.
  const data = parseAnnotationData(annotation);
  if (!data) return null;

  const colour = "color" in data && data.color ? data.color : "#f5a623";
  const className = `annotation-mark kind-${annotation.kind}${selected ? " selected" : ""}`;

  if (annotation.kind === "highlight" || annotation.kind === "underline" || annotation.kind === "strikeout") {
    const rects = (data as HighlightData).rects ?? [];
    const opacity = (data as HighlightData).opacity ?? 0.4;
    return (
      <button
        type="button"
        className={className}
        onClick={onSelect}
        aria-label={`${annotation.kind} annotation`}
        style={{ pointerEvents: "auto" }}
      >
        {rects.map((rect, index) => {
          const box = pdfToPage(
            { x0: rect[0], y0: rect[1], x1: rect[2], y1: rect[3] },
            metrics.scale,
            metrics.pageHeightPoints,
          );
          return (
            <span
              key={index}
              className="annotation-rect"
              style={{
                left: box.x,
                top: box.y,
                width: box.width,
                // Underline and strikeout render as a rule, not a fill: a filled box
                // over the text would hide it, which is not what those tools mean.
                height: annotation.kind === "highlight" ? box.height : 2,
                marginTop:
                  annotation.kind === "strikeout" ? Math.max(0, box.height / 2) : box.height - 2,
                background: colour,
                opacity,
              }}
            />
          );
        })}
      </button>
    );
  }

  if (annotation.kind === "ink") {
    const strokes = (data as InkData).strokes ?? [];
    // Ink is drawn from its vertices, so the mark's own scale conversion is per point.
    return (
      <button type="button" className={className} onClick={onSelect} aria-label="ink annotation">
        <svg width={metrics.width} height={metrics.height}>
          {strokes.map((stroke, index) => (
            <polyline
              key={index}
              points={stroke
                .map(([x, y]) => {
                  const box = pdfToPage(
                    { x0: x, y0: y, x1: x, y1: y },
                    metrics.scale,
                    metrics.pageHeightPoints,
                  );
                  return `${box.x},${box.y}`;
                })
                .join(" ")}
              fill="none"
              stroke={colour}
              strokeWidth={2}
            />
          ))}
        </svg>
      </button>
    );
  }

  // Note and shape both anchor on a rect; a note also shows its text in a tooltip.
  const rect =
    annotation.kind === "shape"
      ? (data as ShapeData).rect
      : noteAnchorRect(data as { point?: [number, number] });
  if (!rect) return null;
  const box = pdfToPage(
    { x0: rect[0], y0: rect[1], x1: rect[2], y1: rect[3] },
    metrics.scale,
    metrics.pageHeightPoints,
  );
  return (
    <button
      type="button"
      className={className}
      onClick={onSelect}
      title={annotation.text ?? undefined}
      aria-label={annotation.kind === "note" ? "note annotation" : "shape annotation"}
      style={{
        left: box.x,
        top: box.y,
        width: Math.max(box.width, 12),
        height: Math.max(box.height, 12),
        borderColor: colour,
        pointerEvents: "auto",
      }}
    />
  );
}

/**
 * A note stores a point; the writer expands it up and right in PDF space. The y flip
 * makes "up" the direction of decreasing UI y, so the anchor point is the box's *bottom*
 * in UI terms -- the same `[x, y, x + size, y + size]` in both spaces.
 */
function noteAnchorRect(
  data: { point?: [number, number] },
): [number, number, number, number] | null {
  if (!data.point) return null;
  const [x, y] = data.point;
  return [x, y, x + NOTE_ICON_POINTS, y + NOTE_ICON_POINTS];
}

/** Rect in UI pixels (top-left origin) -> PDF points, via the shared conversion. */
function pageRectToPdf(rect: UiRect, metrics: PageMetrics): [number, number, number, number] {
  const pdf = pageToPdf(
    rect.x,
    rect.y,
    rect.x + rect.width,
    rect.y + rect.height,
    metrics.scale,
    metrics.pageHeightPoints,
  );
  return [pdf.x0, pdf.y0, pdf.x1, pdf.y1];
}
