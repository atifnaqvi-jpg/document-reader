import { useCallback, useEffect, useRef, useState } from "react";
import type { CSSProperties, PointerEvent as ReactPointerEvent } from "react";
import type {
  Annotation,
  AnnotationData,
  AnnotationKind,
  HighlightData,
  InkData,
  NoteData,
  ShapeData,
  LineData,
} from "../api/types";
import { parseAnnotationData } from "../api/types";
import { useEditor } from "../state/EditorProvider";
import {
  FREE_TEXT_INSET_POINTS,
  FREE_TEXT_LEADING,
  FREE_TEXT_SIZE_POINTS,
  MARK_RULE_THICKNESS,
  dragToRect,
  isDrawable,
  markHeight,
  markOffset,
  squigglyPolyline,
  noteAnchorRect as noteAnchorRectPoints,
  pageToPdf,
  pdfToPage,
  pointToPdf,
} from "../state/geometry";
import type { SweepKind, UiRect } from "../state/geometry";
import type { FreeTextData } from "../api/types";

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
const DRAG_TOOLS: AnnotationKind[] = [
  "highlight",
  "underline",
  "strikeout",
  "squiggly",
  "rect",
  "oval",
  "ink",
  "line",
  "arrow",
];

/** Shortest line worth writing, in PDF points. Below this a drag is a click. */
const MIN_LINE_POINTS = 2;

/** The tools whose value is typed text: they sweep a box and then ask for content. */
const FREE_TEXT_TOOLS: AnnotationKind[] = ["textbox", "callout", "typewriter"];

function isFreeText(kind: AnnotationKind): boolean {
  return FREE_TEXT_TOOLS.includes(kind);
}

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
  // A note is the one tool whose value is its text, so it is captured before the row is
  // written rather than left empty. Held in UI pixels for placement only.
  const [notePrompt, setNotePrompt] = useState<{ x: number; y: number } | null>(null);
  const [noteText, setNoteText] = useState("");
  // A free-text tool sweeps a box, then asks for the text that goes in it. Held like the note
  // prompt because the reason is the same: the text is the annotation's only content, so it
  // must be known before the row is written.
  const [freeTextPrompt, setFreeTextPrompt] = useState<{
    rect: UiRect;
    kind: AnnotationKind;
  } | null>(null);
  const [freeText, setFreeText] = useState("");
  // A callout's leader line needs a target. Set after the text is confirmed, and the next
  // click places it: a callout with no target is a text box, which is a different mark.
  const [awaitingTarget, setAwaitingTarget] = useState<UiRect | null>(null);
  // The callout's text has to outlive the prompt that collected it: the prompt closes first
  // and the leader target is placed afterwards, so the text cannot live in prompt state that
  // is cleared on close.
  const calloutText = useRef("");
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

      // Once the box is drawn and its text confirmed, the next click places the callout's
      // leader target. Handled before the drag path so the click does not start a new box.
      if (awaitingTarget) {
        void commitCallout(point);
        return;
      }

      if (activeTool === "note") {
        // Capture first, write on confirm: a note with no text is a pin with nothing pinned
        // to it, and the DB row is the only place the text can live.
        setNoteText("");
        setNotePrompt(point);
        return;
      }

      if (!DRAG_TOOLS.includes(activeTool) && !isFreeText(activeTool)) return;
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

      // A line's mark IS its two endpoints, so it is not built from a rect: the drag's start
      // and release are the line, and a diagonal's bounding box would be a different mark.
      // The minimum length is measured in UI pixels against the same scale the rect guard
      // uses, so a stray click cannot write a zero-length annotation.
      if (activeTool === "line" || activeTool === "arrow") {
        setDraft(null);
        const from = pointToPdf(origin.x, origin.y, metrics.scale, metrics.pageHeightPoints);
        const to = pointToPdf(point.x, point.y, metrics.scale, metrics.pageHeightPoints);
        const length = Math.hypot(to[0] - from[0], to[1] - from[1]);
        if (length < MIN_LINE_POINTS) return;
        const lineData: LineData = { v: 1, from, to, color: style.color };
        await create(metrics.pageNumber, activeTool, lineData, null);
        return;
      }

      const rect = dragToRect(origin, point);
      setDraft(null);
      if (!isDrawable(rect, metrics.scale)) return;

      // A free-text tool does not write on release: its content is the text, and a box with
      // no text is refused by the writer. The prompt collects it first.
      if (isFreeText(activeTool)) {
        setFreeText("");
        setFreeTextPrompt({ rect, kind: activeTool });
        return;
      }

      // Each tool has its own blob shape (D4). One object carrying every field would
      // work at runtime and rot the moment a tool's geometry changes, so the branch
      // is explicit.
      const pdf = pageRectToPdf(rect, metrics);
      const data: AnnotationData =
        activeTool === "rect" || activeTool === "oval"
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

  /** Writes the free-text row once its text is known. */
  const commitFreeText = useCallback(async () => {
    if (!freeTextPrompt || !metrics) return;
    const text = freeText.trim();
    // Refused rather than written: the writer refuses an empty free text, and an empty frame
    // is a mark the user cannot explain. The button is disabled too, so this is the guard
    // behind the guard.
    if (!text) return;
    const { rect, kind } = freeTextPrompt;
    setFreeTextPrompt(null);
    setFreeText("");

    if (kind === "callout") {
      // A callout is not written yet: its leader target is a second click, so hand the box to
      // that step rather than writing a box now and a callout later.
      calloutText.current = text;
      setAwaitingTarget(rect);
      return;
    }

    const pdf = pageRectToPdf(rect, metrics);
    const data: FreeTextData = { v: 1, rect: pdf, color: style.color };
    await create(metrics.pageNumber, kind, data, text);
  }, [freeTextPrompt, freeText, metrics, create, style.color]);

  /** Writes a callout once its leader target has been placed. */
  const commitCallout = useCallback(
    async (point: { x: number; y: number }) => {
      if (!awaitingTarget || !metrics) return;
      const rect = awaitingTarget;
      const [x, y] = pointToPdf(point.x, point.y, metrics.scale, metrics.pageHeightPoints);
      setAwaitingTarget(null);
      const pdf = pageRectToPdf(rect, metrics);
      const data: FreeTextData = {
        v: 1,
        rect: pdf,
        color: style.color,
        callout: [x, y],
      };
      await create(metrics.pageNumber, "callout", data, calloutText.current);
    },
    [awaitingTarget, metrics, create, style.color],
  );

  /** Commits a note once its text is known, then clears the prompt. */
  const commitNote = useCallback(async () => {
    if (!notePrompt || !metrics) return;
    const text = noteText.trim();
    // An empty note is refused rather than written: it would be an anonymous pin that
    // nothing can distinguish from a stray mark.
    if (!text) return;
    const [x, y] = pointToPdf(
      notePrompt.x,
      notePrompt.y,
      metrics.scale,
      metrics.pageHeightPoints,
    );
    const data: NoteData = { v: 1, point: [x, y], color: style.color };
    setNotePrompt(null);
    setNoteText("");
    await create(metrics.pageNumber, "note", data, text);
  }, [notePrompt, noteText, metrics, create, style.color]);

  // Escape abandons the draft note, matching the menu's Escape handling.
  useEffect(() => {
    if (!notePrompt) return;
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.preventDefault();
        setNotePrompt(null);
        setNoteText("");
      }
    };
    // Capture phase: the overlay sits inside panes that also listen for Escape.
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [notePrompt]);

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

      {/* Positioned where the click landed, so the note is placed and described in one
          gesture rather than two disconnected ones. */}
      {notePrompt ? (
        <div
          className="annotation-note-prompt"
          style={{ left: notePrompt.x, top: notePrompt.y }}
          onPointerDown={(event) => event.stopPropagation()}
        >
          <textarea
            autoFocus
            value={noteText}
            placeholder="Note text"
            onChange={(event) => setNoteText(event.target.value)}
            onKeyDown={(event) => {
              // Enter commits; Shift+Enter is a newline, the convention notes use.
              if (event.key === "Enter" && !event.shiftKey) {
                event.preventDefault();
                void commitNote();
              }
            }}
          />
          <div className="annotation-note-prompt-actions">
            <button type="button" onClick={() => void commitNote()} disabled={!noteText.trim()}>
              Add note
            </button>
            <button
              type="button"
              onClick={() => {
                setNotePrompt(null);
                setNoteText("");
              }}
            >
              Cancel
            </button>
          </div>
        </div>
      ) : null}

      {/* Positioned over the box the user swept, so the text is typed where it will appear. */}
      {freeTextPrompt ? (
        <div
          className="annotation-note-prompt"
          style={{ left: freeTextPrompt.rect.x, top: freeTextPrompt.rect.y }}
          onPointerDown={(event) => event.stopPropagation()}
        >
          <textarea
            autoFocus
            value={freeText}
            placeholder={freeTextPrompt.kind === "callout" ? "Callout text" : "Text"}
            onChange={(event) => setFreeText(event.target.value)}
            onKeyDown={(event) => {
              // Enter commits; Shift+Enter is a newline, the same convention notes use.
              if (event.key === "Enter" && !event.shiftKey) {
                event.preventDefault();
                void commitFreeText();
              }
            }}
          />
          <div className="annotation-note-prompt-actions">
            <button type="button" onClick={() => void commitFreeText()} disabled={!freeText.trim()}>
              {freeTextPrompt.kind === "callout" ? "Next: point at it" : "Add text"}
            </button>
            <button
              type="button"
              onClick={() => {
                setFreeTextPrompt(null);
                setFreeText("");
              }}
            >
              Cancel
            </button>
          </div>
        </div>
      ) : null}

      {/* Tells the user what the next click does. Without it the callout box vanishes and the
          next click appears to do nothing. */}
      {awaitingTarget ? (
        <div className="annotation-note-prompt" style={{ left: awaitingTarget.x, top: awaitingTarget.y }}>
          <span>Click the thing this callout points at.</span>
        </div>
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

  if (
    annotation.kind === "highlight" ||
    annotation.kind === "underline" ||
    annotation.kind === "strikeout" ||
    annotation.kind === "squiggly"
  ) {
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
          const kind = annotation.kind as SweepKind;
          // A squiggly is the one sweep whose shape is not a rectangle: it is a wave, so it
          // is drawn as a polyline rather than a styled span. Rendering it as a span would
          // paint a solid bar, which is a different mark (a highlight, effectively).
          if (kind === "squiggly") {
            const height = markHeight(kind, box.height);
            return (
              <svg
                key={index}
                className="annotation-rect"
                style={{ left: box.x, top: box.y + markOffset(kind, box.height) }}
                width={box.width}
                height={height}
                aria-hidden="true"
              >
                <polyline
                  points={squigglyPolyline(box.width, height)}
                  fill="none"
                  stroke={colour}
                  strokeWidth={MARK_RULE_THICKNESS}
                  opacity={opacity}
                />
              </svg>
            );
          }
          return (
            <span
              key={index}
              className="annotation-rect"
              style={{
                left: box.x,
                top: box.y,
                width: box.width,
                // Rule tools draw a rule within the box; a highlight fills it. The offset
                // and height rules live in `geometry.ts` so they are unit-testable.
                height: markHeight(kind, box.height),
                marginTop: markOffset(kind, box.height),
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

  if (annotation.kind === "line" || annotation.kind === "arrow") {
    const line = data as LineData;
    if (!line.from || !line.to) return null;
    // The endpoint is converted the same way a single point is, so the line lands where the
    // drag happened rather than where a rect's corners would have been.
    const start = pdfToPage(
      { x0: line.from[0], y0: line.from[1], x1: line.from[0], y1: line.from[1] },
      metrics.scale,
      metrics.pageHeightPoints,
    );
    const end = pdfToPage(
      { x0: line.to[0], y0: line.to[1], x1: line.to[0], y1: line.to[1] },
      metrics.scale,
      metrics.pageHeightPoints,
    );
    return (
      <button
        type="button"
        className={className}
        onClick={onSelect}
        aria-label={`${annotation.kind} annotation`}
      >
        <svg width={metrics.width} height={metrics.height}>
          <line
            x1={start.x}
            y1={start.y}
            x2={end.x}
            y2={end.y}
            stroke={colour}
            strokeWidth={2}
            strokeLinecap="round"
            markerEnd={
              annotation.kind === "arrow" ? "url(#annotation-arrowhead)" : undefined
            }
          />
          {annotation.kind === "arrow" && (
            <defs>
              {/* The same head the writer draws into /AP, so the preview and the saved
                  file do not disagree about which end the arrow points at. */}
              <marker
                id="annotation-arrowhead"
                markerWidth="8"
                markerHeight="8"
                refX="7"
                refY="4"
                orient="auto"
              >
                <path d="M0,0 L8,4 L0,8 z" fill={colour} />
              </marker>
            </defs>
          )}
        </svg>
      </button>
    );
  }

  if (annotation.kind === "note") {
    const rect = noteAnchorRect(data as { point?: [number, number] });
    if (!rect) return null;
    const box = pdfToPage(
      { x0: rect[0], y0: rect[1], x1: rect[2], y1: rect[3] },
      metrics.scale,
      metrics.pageHeightPoints,
    );
    // A note is an icon, not an empty frame: without a glyph and a fill of its own it
    // reads as a stray rectangle. Sized in UI pixels so it stays legible at every zoom
    // rather than shrinking with the page.
    const side = Math.max(box.width, 18);
    return (
      <>
        <button
          type="button"
          className={className}
          onClick={onSelect}
          title={annotation.text ?? undefined}
          aria-label="note annotation"
          style={{
            left: box.x,
            top: box.y,
            width: side,
            height: side,
            background: colour,
            borderColor: colour,
            pointerEvents: "auto",
          }}
        >
          <span className="annotation-note-glyph" aria-hidden="true">
            N
          </span>
        </button>
        {/* Selecting a note opens its text for editing: a note's value is its text, and
            without this the only way to fix a typo would be to delete and re-add it. */}
        {selected ? (
          <NoteEditor annotation={annotation} style={{ left: box.x + side, top: box.y }} />
        ) : null}
      </>
    );
  }

  // Free text draws its box and its text. The box is drawn even for a typewriter, whose
  // writer emits no frame: the viewer still needs to show where the text sits while it is
  // selected, or the mark cannot be targeted to edit.
  if (annotation.kind === "textbox" || annotation.kind === "callout" || annotation.kind === "typewriter") {
    const rect = (data as FreeTextData).rect;
    if (!rect) return null;
    const box = pdfToPage(
      { x0: rect[0], y0: rect[1], x1: rect[2], y1: rect[3] },
      metrics.scale,
      metrics.pageHeightPoints,
    );
    // The preview has to draw at the size the file will draw. The writer sets 12pt, so the
    // on-screen font is 12 x zoom: a fixed 12px made the preview a different annotation from
    // the one saved at every zoom except 1, which is why text typed into the box looked
    // displaced once the page was zoomed. Line height and inset are the writer's own
    // constants for the same reason -- the box in the file is padded by 2pt, not 3px.
    const fontSize = FREE_TEXT_SIZE_POINTS * metrics.scale;
    const lineHeight = fontSize * FREE_TEXT_LEADING;
    return (
      <button
        type="button"
        className={`${className} kind-free-text`}
        onClick={onSelect}
        title={annotation.text ?? undefined}
        aria-label={`${annotation.kind} annotation`}
        style={{
          left: box.x,
          top: box.y,
          width: box.width,
          height: box.height,
          borderColor: colour,
          color: colour,
          pointerEvents: "auto",
          fontSize,
          lineHeight: `${lineHeight}px`,
          padding: FREE_TEXT_INSET_POINTS * metrics.scale,
        }}
      >
        <span className="annotation-free-text">{annotation.text}</span>
      </button>
    );
  }

  // A shape anchors on a rect and draws as an empty frame. An oval's frame is the same rect,
  // drawn as an inscribed ellipse -- the same construction the writer puts in /AP, so the
  // preview and the saved page agree about how much of the box the curve fills.
  const rect = (data as ShapeData).rect;
  if (!rect) return null;
  const box = pdfToPage(
    { x0: rect[0], y0: rect[1], x1: rect[2], y1: rect[3] },
    metrics.scale,
    metrics.pageHeightPoints,
  );
  const size = {
    width: Math.max(box.width, 12),
    height: Math.max(box.height, 12),
  };

  if (annotation.kind === "oval") {
    return (
      <button
        type="button"
        className={className}
        onClick={onSelect}
        title={annotation.text ?? undefined}
        aria-label="oval annotation"
        style={{ left: box.x, top: box.y, ...size, pointerEvents: "auto" }}
      >
        <svg width={size.width} height={size.height}>
          <ellipse
            cx={size.width / 2}
            cy={size.height / 2}
            rx={Math.max(size.width / 2 - 1, 0)}
            ry={Math.max(size.height / 2 - 1, 0)}
            fill="none"
            stroke={colour}
            strokeWidth={2}
          />
        </svg>
      </button>
    );
  }

  return (
    <button
      type="button"
      className={className}
      onClick={onSelect}
      title={annotation.text ?? undefined}
      aria-label="shape annotation"
      style={{
        left: box.x,
        top: box.y,
        ...size,
        borderColor: colour,
        pointerEvents: "auto",
      }}
    />
  );
}

/**
 * Edits one note's text in place (P2-D11). Commits on blur or Enter.
 *
 * A local draft is kept rather than writing on every keystroke: each keystroke would
 * otherwise be a DB write, a re-query and an undo entry.
 */
function NoteEditor({
  annotation,
  style,
}: {
  annotation: Annotation;
  style: CSSProperties;
}) {
  const { updateText } = useEditor();
  const [text, setText] = useState(annotation.text ?? "");
  const committed = useRef(false);

  const commit = useCallback(() => {
    // Blur fires on unmount too, so the guard stops a second (identical) write.
    if (committed.current) return;
    committed.current = true;
    const next = text.trim();
    if (next === (annotation.text ?? "").trim()) return;
    void updateText(annotation, next);
  }, [annotation, text, updateText]);

  return (
    <div
      className="annotation-note-editor"
      style={style}
      onPointerDown={(event) => event.stopPropagation()}
    >
      <textarea
        autoFocus
        value={text}
        onChange={(event) => {
          committed.current = false;
          setText(event.target.value);
        }}
        onBlur={commit}
        onKeyDown={(event) => {
          if (event.key === "Enter" && !event.shiftKey) {
            event.preventDefault();
            commit();
          }
          if (event.key === "Escape") {
            event.preventDefault();
            committed.current = true;
            setText(annotation.text ?? "");
          }
        }}
      />
    </div>
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
  const rect = noteAnchorRectPoints(data.point);
  return rect ? [rect.x0, rect.y0, rect.x1, rect.y1] : null;
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
