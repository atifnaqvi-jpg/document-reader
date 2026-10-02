import { useEffect, useState } from "react";
import { search, toAppError } from "../api/invoke";
import type { AppError, LibraryEntry, SearchHit } from "../api/types";
import { useIndexStatus } from "../hooks/useIndexStatus";

interface Props {
  /** Used to label hits: the index stores ids, the UI shows titles. */
  entries: LibraryEntry[];
  onOpenHit: (entry: LibraryEntry, page: number) => void;
}

/** Debounce for the search box: typing should not fire a query per keystroke. */
const SEARCH_DEBOUNCE_MS = 200;
const RESULT_LIMIT = 50;

export function SearchPane({ entries, onOpenHit }: Props) {
  const [query, setQuery] = useState("");
  const [hits, setHits] = useState<SearchHit[]>([]);
  const [error, setError] = useState<AppError | null>(null);
  const [searching, setSearching] = useState(false);

  useEffect(() => {
    const trimmed = query.trim();
    if (trimmed === "") {
      setHits([]);
      setError(null);
      return;
    }

    let cancelled = false;
    setSearching(true);
    const timer = window.setTimeout(() => {
      search(trimmed, RESULT_LIMIT)
        .then((results) => {
          if (!cancelled) setHits(results);
        })
        .catch((e) => {
          if (!cancelled) setError(toAppError(e));
        })
        .finally(() => {
          if (!cancelled) setSearching(false);
        });
    }, SEARCH_DEBOUNCE_MS);

    // Cancelling the timer on every keystroke is what makes this a debounce rather than
    // a request per character.
    return () => {
      cancelled = true;
      window.clearTimeout(timer);
    };
  }, [query]);

  const titleFor = (documentId: string) => {
    const entry = entries.find((candidate) => candidate.id === documentId);
    return entry?.title ?? "(untitled)";
  };

  return (
    <div className="search-pane">
      <label className="search-box">
        <span className="visually-hidden">Search documents</span>
        <input
          type="search"
          value={query}
          placeholder="Search text…"
          onChange={(event) => setQuery(event.target.value)}
        />
      </label>

      {error ? (
        <p className="muted" role="alert">
          {error.message}
        </p>
      ) : null}

      {query.trim() !== "" && !searching && hits.length === 0 && !error ? (
        <p className="muted">
          No matches. Text is indexed when a document is opened, so a document added
          before now may still be indexing.
        </p>
      ) : null}

      {hits.length > 0 ? (
        <ul className="search-results">
          {hits.map((hit, index) => (
            <li key={`${hit.documentId}:${hit.page}:${index}`}>
              <button
                type="button"
                onClick={() => {
                  const entry = entries.find((c) => c.id === hit.documentId);
                  if (entry) onOpenHit(entry, hit.page);
                }}
              >
                <span className="search-title">{titleFor(hit.documentId)}</span>
                <span className="search-page">p. {hit.page + 1}</span>
                {/* The snippet is FTS5's own output: matches are wrapped in [ ] by the
                    backend's snippet() call, so it is rendered as text, never as HTML. */}
                <span className="search-snippet">{hit.snippet}</span>
              </button>
            </li>
          ))}
        </ul>
      ) : null}
    </div>
  );
}

/** Progress and failure state for the selected document's index (P1-D6). */
export function IndexingIndicator({ docId }: { docId: string | null }) {
  const { status, error, retrying, retry } = useIndexStatus(docId);

  if (!docId || (!status && !error)) return null;

  if (error) {
    return (
      <div className="index-status failed" role="alert">
        <span>Indexing failed.</span>
        <button type="button" onClick={retry} disabled={retrying}>
          {retrying ? "Retrying…" : "Retry"}
        </button>
      </div>
    );
  }

  if (status?.state === "failed") {
    return (
      <div className="index-status failed" role="alert">
        <span>Indexing failed; search will miss this document.</span>
        <button type="button" onClick={retry} disabled={retrying}>
          {retrying ? "Retrying…" : "Retry"}
        </button>
      </div>
    );
  }

  if (status?.state !== "running") return null;

  // `total` is 0 until the page count is known, so an indeterminate label is shown
  // rather than a nonsensical "0 / 0".
  const label =
    status.total > 0
      ? `Indexing ${status.done} / ${status.total}`
      : "Indexing…";

  return (
    <div className="index-status running" role="status">
      <span>{label}</span>
      {status.total > 0 ? (
        <progress value={status.done} max={status.total} />
      ) : null}
    </div>
  );
}
