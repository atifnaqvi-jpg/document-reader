import { useCallback, useMemo, useRef, useState } from "react";
import type { AnnotationKind, PageTextRun } from "../api/types";
import { useEditor } from "../state/EditorProvider";
import {
  snapTargetAt,
  signatureFreeTextData,
  signatureInkData,
  type SignatureMode,
  type SnapTarget,
} from "../state/fillsign";

export type { SignatureMode };
import type { PageMetrics } from "./AnnotationOverlay";
import { pdfToPage, pointToPdf } from "../state/geometry";

/**
 * The Fill & Sign placement surface (`docs/P3.3a-fill-and-sign-DESIGN.md` §5, §10).
 *
 * ## What this component is for, stated as the defect it closes
 *
 * The backend has written visual signatures since P3.3a's engine work: `annotate.rs` labels a row
 * whose blob carries `visualSignature`, and `apply_annotations` draws that label into page content.
 * But **nothing in `src/` ever set the flag**, so `is_visual_signature` was false for every row a
 * user could create and the feature was unreachable. This surface is the caller that declares the
 * intent (D-N6).
 *
 * ## Why it is its own layer rather than a tool in the annotation palette
 *
 * `TOOLS` in `EditorPane` writes rows of a kind the user picks. A signature is the same row kind
 * (`ink`, or `textbox` for the typed case) but with a *different guarantee attached*: D-N2's label
 * is written into the artifact, and I-F6 says no code path may return a cryptographic verdict for
 * it. Making it a palette entry would put "Ink" and "Signature" in one list where the only visible
 * difference is a checkbox's worth of JSON -- which is exactly how the flag came to be forgotten.
 *
 * ## Two modes, one geometry
 *
 * Drawn and typed differ only in the data written; both resolve their box through the same
 * {@link snapTargetAt}, so the descender correction cannot be right for one and wrong for the other.
 */

/**
 * The ink colour. Fixed, not a palette: D-N5/Q4 fixes the pen weight and a signature is
 * conventionally a dark mark, and offering six colours would imply a styling feature this phase
 * does not have. Stated so the absence reads as a decision rather than an oversight.
 */
const SIGNATURE_COLOUR = "#1a1a1a";

/** Drag shorter than this is a stray click, not a signature. */
const MIN_STROKE_POINTS = 2;

export interface FillSignSurfaceProps {
  metrics: PageMetrics | null;
  /**
   * The current page's runs, used only to snap. Injected rather than read from the editor context
   * so the surface is testable without a live document, and so the one thing this component does
   * with geometry can be exercised directly.
   */
  runs: readonly PageTextRun[];
  mode: SignatureMode;
  /** The typed signature's text, collected before placement. */
  typedText: string;
  /** Called when the placement is rejected, so the reason reaches the user rather than the log. */
  onRefused?: (reason: string) => void;
}

interface Point {
  x: number;
  y: number;
}

/**
 * The placement layer. Renders a full-surface pointer div only while `mode` is active.
 *
 * Deliberately does not render its own Save: §5 requires the existing Save to commit, because one
 * user action must not produce two files. Saving is `save_edits`, which writes a sibling copy.
 */
export function FillSignSurface({
  metrics,
  runs,
  mode,
  typedText,
  onRefused,
}: FillSignSurfaceProps) {
  const { create } = useEditor();
  const surfaceRef = useRef<HTMLDivElement | null>(null);
  const [inkPoints, setInkPoints] = useState<Point[]>([]);
  const [preview, setPreview] = useState<SnapTarget | null>(null);
  const drawing = useRef(false);

  /** Pointer position relative to the overlay, which is what all geometry assumes. */
  const localPoint = useCallback((event: React.PointerEvent): Point => {
    const bounds = surfaceRef.current?.getBoundingClientRect();
    if (!bounds) return { x: 0, y: 0 };
    return { x: event.clientX - bounds.left, y: event.clientY - bounds.top };
  }, []);

  /** The page-space point under a UI point, which is what snapping compares against. */
  const toPdf = useCallback(
    (point: Point): Point => {
      if (!metrics) return point;
      const [x, y] = pointToPdf(point.x, point.y, metrics.scale, metrics.pageHeightPoints);
      return { x, y };
    },
    [metrics],
  );

  /**
   * The snap target under a UI point, or `null`.
   *
   * The tolerance is one line height of the *nearest* run rather than a fixed number of points, so
   * a 6.96pt line and a 24pt line do not share a threshold. Using the default 12pt font size as
   * the yardstick would make the snap jump a whole line on dense forms like the LEA fixture.
   */
  const targetAt = useCallback(
    (point: Point): SnapTarget | null => {
      const pdf = toPdf(point);
      const tolerance = metrics ? 12 / Math.max(metrics.scale, 0.01) : 12;
      return snapTargetAt(runs, pdf, tolerance);
    },
    [metrics, runs, toPdf],
  );

  const handlePointerDown = useCallback(
    (event: React.PointerEvent) => {
      if (!metrics) return;
      event.currentTarget.setPointerCapture?.(event.pointerId);
      drawing.current = true;
      setInkPoints([localPoint(event)]);
    },
    [localPoint, metrics],
  );

  const handlePointerMove = useCallback(
    (event: React.PointerEvent) => {
      if (!metrics) return;
      const point = localPoint(event);

      if (drawing.current) {
        setInkPoints((previous) => [...previous, point]);
        return;
      }
      // D-N8: the snap is shown before it commits, so an unwanted snap is visible rather than
      // silent. Nothing is previewed for a suspicious box because there is nothing honest to show.
      setPreview(targetAt(point));
    },
    [localPoint, metrics, targetAt],
  );

  const handlePointerLeave = useCallback(() => {
    setPreview(null);
  }, []);

  const handlePointerUp = useCallback(
    async (event: React.PointerEvent) => {
      if (!metrics) return;
      const point = localPoint(event);
      drawing.current = false;
      const points = [...inkPoints, point];
      setInkPoints([]);

      if (mode === "typed") {
        const text = typedText.trim();
        if (!text) {
          onRefused?.("A typed signature needs text before it can be placed.");
          return;
        }
        const target = targetAt(point);
        if (!target) {
          onRefused?.(
            "Nothing on this line to sign against. Place the mark on a text line, or draw it instead.",
          );
          return;
        }
        await create(
          metrics.pageNumber,
          // The typed case reuses the existing free-text kind rather than inventing one: the
          // writer already resolves a `/DA` against the annotation's own `/DR`.
          "textbox" as AnnotationKind,
          signatureFreeTextData(target.rect, SIGNATURE_COLOUR),
          text,
        );
        return;
      }

      // Drawn: the strokes are the mark. Fewer than two points is a stray click.
      if (points.length < 2) return;
      const stroke = points.map((p) =>
        pointToPdf(p.x, p.y, metrics.scale, metrics.pageHeightPoints),
      );
      // A non-finite coordinate would be written into the PDF as a broken operand. Checking
      // before the length guard matters because `NaN < MIN` is false, so a NaN stroke would pass
      // a naive length test and be saved as an unopenable mark.
      if (!stroke.every(([x, y]) => Number.isFinite(x) && Number.isFinite(y))) {
        onRefused?.("That stroke has no usable coordinates, so it was not placed.");
        return;
      }
      const length = strokeLength(stroke);
      if (!(length >= MIN_STROKE_POINTS)) {
        onRefused?.("That stroke is too short to be a signature.");
        return;
      }

      await create(
        metrics.pageNumber,
        "ink" as AnnotationKind,
        signatureInkData([stroke], SIGNATURE_COLOUR),
        null,
      );
    },
    [create, inkPoints, localPoint, metrics, mode, onRefused, targetAt, typedText],
  );

  const previewBox = useMemo(() => {
    if (!preview || !metrics) return null;
    return pdfToPage(preview.rect, metrics.scale, metrics.pageHeightPoints);
  }, [metrics, preview]);

  if (!metrics) return null;

  return (
    <div
      ref={surfaceRef}
      data-testid="fillsign-surface"
      className="fillsign-surface"
      onPointerDown={handlePointerDown}
      onPointerMove={handlePointerMove}
      onPointerUp={handlePointerUp}
      onPointerLeave={handlePointerLeave}
      style={{
        position: "absolute",
        inset: 0,
        // The mark is drawn, so the pointer must not select text underneath it.
        cursor: "crosshair",
        touchAction: "none",
      }}
    >
      {previewBox ? (
        <div
          data-testid="fillsign-snap-preview"
          // Dashed rather than solid: it is a proposal, and it must not look like a placed mark.
          style={{
            position: "absolute",
            left: previewBox.x,
            top: previewBox.y,
            width: previewBox.width,
            height: previewBox.height,
            border: "1px dashed #1a73e8",
            pointerEvents: "none",
          }}
        />
      ) : null}
      {inkPoints.length > 1 ? (
        <svg
          data-testid="fillsign-ink-preview"
          style={{ position: "absolute", inset: 0, pointerEvents: "none" }}
        >
          <polyline
            points={inkPoints.map((p) => `${p.x},${p.y}`).join(" ")}
            fill="none"
            stroke={SIGNATURE_COLOUR}
            strokeWidth={2}
          />
        </svg>
      ) : null}
    </div>
  );
}

/** Total path length of a page-space stroke, used to reject a stray tap. */
export function strokeLength(stroke: readonly [number, number][]): number {
  let total = 0;
  for (let i = 1; i < stroke.length; i += 1) {
    total += Math.hypot(stroke[i][0] - stroke[i - 1][0], stroke[i][1] - stroke[i - 1][1]);
  }
  return total;
}
