import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { getIndexStatus, indexDocument, toAppError } from "../api/invoke";
import { INDEX_PROGRESS_EVENT } from "../api/types";
import type { AppError, IndexProgress, IndexStatus } from "../api/types";

/**
 * A document's indexing status (P1-D6).
 *
 * Two sources are needed, because neither alone is sufficient:
 *
 * * The `index://progress` event is how a live run reports itself, but it is fire-and-
 *   forget — anything emitted while this pane was unmounted is gone.
 * * `get_index_status` is the read-back that survives a remount, but it is a snapshot.
 *
 * So the event drives updates and the read-back seeds them. The listener is registered
 * before the read-back is requested, so a batch landing in between is not lost: the
 * status is at worst one event behind, never permanently stale.
 */
export function useIndexStatus(docId: string | null) {
  const [status, setStatus] = useState<IndexStatus | null>(null);
  const [error, setError] = useState<AppError | null>(null);
  const [retrying, setRetrying] = useState(false);

  useEffect(() => {
    setStatus(null);
    setError(null);
    if (!docId) return;

    let cancelled = false;
    const unlisten = listen<IndexProgress>(INDEX_PROGRESS_EVENT, (event) => {
      if (cancelled || event.payload.docId !== docId) return;
      setStatus({
        state: "running",
        done: event.payload.done,
        total: event.payload.total,
      });
    });

    // Read-back after subscribing: indexing starts at import, so by the time the viewer
    // mounts the work may already be in progress or finished.
    getIndexStatus(docId)
      .then((stored) => {
        if (cancelled) return;
        // A live event is fresher than the snapshot, so only fill in the gaps.
        setStatus((current) => current ?? stored);
      })
      .catch((e) => {
        if (!cancelled) setError(toAppError(e));
      });

    return () => {
      cancelled = true;
      void unlisten.then((stop) => stop());
    };
  }, [docId]);

  const retry = async () => {
    if (!docId) return;
    setRetrying(true);
    setError(null);
    try {
      await indexDocument(docId);
    } catch (e) {
      setError(toAppError(e));
    } finally {
      setRetrying(false);
    }
  };

  return { status, error, retrying, retry };
}

/**
 * Percentage complete, or `null` when not running. `total` can be 0 while a run is still
 * resolving the page count, in which case there is no meaningful fraction to show.
 */
export function indexPercent(status: IndexStatus | null): number | null {
  if (!status || status.state !== "running") return null;
  if (status.total <= 0) return null;
  return Math.min(100, Math.round((status.done / status.total) * 100));
}
