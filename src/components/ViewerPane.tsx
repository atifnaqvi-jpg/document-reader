import { useCallback, useEffect, useRef, useState } from "react";
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

export function ViewerPane({ entry }: { entry: LibraryEntry | null }) {
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
}
