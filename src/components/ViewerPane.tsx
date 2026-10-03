import { forwardRef, useCallback, useEffect, useImperativeHandle, useRef, useState } from "react";
import {
  getDocumentPageCount,
  getDocumentPageRender,
  pageImageSrc,
  toAppError,
} from "../api/invoke";
import type { AppError, LibraryEntry, RenderedPage } from "../api/types";

/** Zoom levels offered by the viewer. Scale multiplies the PDF's nominal 96 DPI. */
const ZOOM_LEVELS = [0.5, 0.75, 1, 1.5, 2, 3] as const;
const DEFAULT_ZOOM = 2;

/** A request to move the viewer to a page. The nonce makes an identical request (same
 * page, same document — e.g. clicking the same search hit twice) retrigger the effect. */
export interface JumpRequest {
  page: number;
  nonce: number;
}

/**
 * Commands the viewer exposes to the rest of the app (P1.9 D6). The menu lives in App
 * while the page and zoom state live here, so the menu drives the viewer through this
 * handle rather than the state being lifted — the P1 architecture keeps viewer state
 * local on purpose.
 */
export interface ViewerHandle {
  step: (delta: number) => void;
  goToFirst: () => void;
  goToLast: () => void;
  zoomIn: () => void;
  zoomOut: () => void;
  /** Current page, zero-based, for "bookmark this page". */
  getCurrentPage: () => number;
}

export const ViewerPane = forwardRef<ViewerHandle, {
  entry: LibraryEntry | null;
  jump?: JumpRequest | null;
}>(function ViewerPane({ entry, jump }, ref) {

  const [page, setPage] = useState(0);
  const [pageCount, setPageCount] = useState<number | null>(null);
  const [zoom, setZoom] = useState<number>(DEFAULT_ZOOM);
  const [rendered, setRendered] = useState<RenderedPage | null>(null);
  const [error, setError] = useState<AppError | null>(null);
  const [rendering, setRendering] = useState(false);

  // Renders are async and can be superseded by a newer page/zoom request. Tracking the
  // in-flight request lets a stale response be dropped instead of flashing over the
  // current page. The backend caches by (doc, page, scale), so a late result is never
  // wasted, only out of order.
  const requestRef = useRef(0);

  const docId = entry?.id ?? null;
  const jumpPage = jump?.page ?? null;
  const jumpNonce = jump?.nonce ?? 0;

  // Reset per-document state when the selection changes, so the page number of the
  // previous document does not carry into the new one.
  useEffect(() => {
    setPage(0);
    setRendered(null);
    setError(null);
    setPageCount(entry?.pages ?? null);

    if (!docId) return;
    let cancelled = false;
    // The import-time page count can be null, so an authoritative count is fetched
    // from the renderer.
    getDocumentPageCount(docId)
      .then((count) => {
        if (!cancelled) setPageCount(count);
      })
      .catch((e) => {
        if (!cancelled) setError(toAppError(e));
      });
    return () => {
      cancelled = true;
    };
  }, [docId, entry?.pages]);

  // Latest count, readable from the jump effect without making `pageCount` a dependency:
  // including it would re-run the jump whenever the count arrives and yank the user back
  // to the hit page after they had already navigated away.
  const pageCountRef = useRef<number | null>(null);
  pageCountRef.current = pageCount;

  // A search hit asks the viewer to go to a specific page. Clamping happens here rather
  // than at the call site because `pageCount` is only authoritative after the count
  // request resolves, so the caller cannot know the last page. The nonce means clicking
  // the same hit twice still re-fires.
  const lastJumpNonce = useRef(0);
  useEffect(() => {
    if (jumpPage === null || jumpNonce === lastJumpNonce.current) return;
    lastJumpNonce.current = jumpNonce;

    const count = pageCountRef.current;
    const target = count !== null ? Math.min(jumpPage, Math.max(0, count - 1)) : jumpPage;
    setPage(Math.max(0, target));
  }, [jumpPage, jumpNonce]);

  useEffect(() => {
    if (!docId) return;

    const request = ++requestRef.current;
    setRendering(true);
    setError(null);

    getDocumentPageRender(docId, page, zoom)
      .then((result) => {
        if (request !== requestRef.current) return;
        setRendered(result);
      })
      .catch((e) => {
        if (request !== requestRef.current) return;
        setError(toAppError(e));
      })
      .finally(() => {
        if (request === requestRef.current) setRendering(false);
      });
  }, [docId, page, zoom]);

  const step = useCallback(
    (delta: number) => {
      setPage((current) => {
        const next = current + delta;
        if (next < 0) return 0;
        if (pageCount !== null && next > pageCount - 1) return pageCount - 1;
        return next;
      });
    },
    [pageCount],
  );

  const goToFirst = useCallback(() => setPage(0), []);
  const goToLast = useCallback(() => {
    if (pageCount !== null) setPage(Math.max(0, pageCount - 1));
  }, [pageCount]);

  const zoomBy = useCallback((delta: number) => {
    setZoom((current) => {
      const index = ZOOM_LEVELS.indexOf(current as (typeof ZOOM_LEVELS)[number]);
      // An unlisted value (only reachable if the list changes shape) falls back to the
      // nearest step rather than jumping to an end of the range.
      const from = index === -1 ? ZOOM_LEVELS.indexOf(DEFAULT_ZOOM) : index;
      const next = Math.min(Math.max(from + delta, 0), ZOOM_LEVELS.length - 1);
      return ZOOM_LEVELS[next];
    });
  }, []);

  // The page state the menu reads for "bookmark this page", kept in a ref so the handle
  // does not change identity on every page turn.
  const pageRef = useRef(page);
  pageRef.current = page;

  useImperativeHandle(
    ref,
    () => ({
      step,
      goToFirst,
      goToLast,
      zoomIn: () => zoomBy(1),
      zoomOut: () => zoomBy(-1),
      getCurrentPage: () => pageRef.current,
    }),
    [step, goToFirst, goToLast, zoomBy],
  );

  // Keyboard navigation: a reader is a keyboard-first surface, and arrows/PageUp/Down
  // are what people reach for before any on-screen control.
  useEffect(() => {
    if (!docId) return;
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "ArrowRight" || event.key === "PageDown") {
        event.preventDefault();
        step(1);
      } else if (event.key === "ArrowLeft" || event.key === "PageUp") {
        event.preventDefault();
        step(-1);
      } else if (event.key === "Home") {
        event.preventDefault();
        setPage(0);
      } else if (event.key === "End" && pageCount !== null) {
        event.preventDefault();
        setPage(pageCount - 1);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [docId, pageCount, step]);

  if (!entry) {
    return (
      <div className="pane-empty">
        <p>No document open.</p>
        <p className="muted">Select a document from the library, or open a new file.</p>
      </div>
    );
  }

  const lastPage = pageCount !== null ? pageCount - 1 : null;

  return (
    <div className="viewer">
      <header className="viewer-header">
        <h1>{entry.title ?? "(untitled)"}</h1>
        <div className="viewer-controls">
          <button
            type="button"
            onClick={() => step(-1)}
            disabled={page === 0}
            aria-label="Previous page"
          >
            ‹
          </button>
          <span className="page-indicator">
            {pageCount === null ? page + 1 : `${page + 1} / ${pageCount}`}
          </span>
          <button
            type="button"
            onClick={() => step(1)}
            disabled={lastPage !== null && page >= lastPage}
            aria-label="Next page"
          >
            ›
          </button>
          <label className="zoom-control">
            Zoom
            <select value={zoom} onChange={(event) => setZoom(Number(event.target.value))}>
              {ZOOM_LEVELS.map((level) => (
                <option key={level} value={level}>
                  {Math.round(level * 100)}%
                </option>
              ))}
            </select>
          </label>
        </div>
        <dl className="metadata">
          <div>
            <dt>Author</dt>
            <dd>{entry.author ?? "—"}</dd>
          </div>
          <div>
            <dt>Format</dt>
            <dd>{entry.format.toUpperCase()}</dd>
          </div>
          <div>
            <dt>Path</dt>
            <dd className="path">{entry.path}</dd>
          </div>
        </dl>
      </header>

      <div className="viewer-canvas">
        {error ? (
          <div className="render-error" role="alert">
            <p>
              {error.code === "RENDER_UNAVAILABLE"
                ? "This format cannot be displayed as pages."
                : "Could not render this page."}
            </p>
            <p className="muted">{error.message}</p>
          </div>
        ) : null}

        {rendered && !error ? (
          <img
            className="page-image"
            src={pageImageSrc(rendered)}
            // Explicit pixel size from the backend (P1-D2): the container is sized
            // before the PNG decodes, so a page turn does not reflow the viewer.
            width={rendered.width}
            height={rendered.height}
            alt={`Page ${page + 1}`}
          />
        ) : null}

        {rendering ? <p className="muted">Rendering page…</p> : null}
      </div>
    </div>
  );
});
