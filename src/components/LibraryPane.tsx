import type { AppError, LibraryEntry } from "../api/types";

interface Props {
  entries: LibraryEntry[];
  selectedId: string | null;
  onSelect: (entry: LibraryEntry) => void;
}

export function LibraryPane({ entries, selectedId, onSelect }: Props) {
  if (entries.length === 0) {
    return (
      <div className="pane-empty">
        <p>Your library is empty.</p>
        <p className="muted">Use “Open…” to add a PDF, TXT, or Markdown file.</p>
      </div>
    );
  }

  return (
    <ul className="library-list">
      {entries.map((entry) => (
        <li key={entry.id}>
          <button
            type="button"
            className={`library-item${entry.id === selectedId ? " selected" : ""}`}
            onClick={() => onSelect(entry)}
          >
            <span className="library-title">{entry.title ?? "(untitled)"}</span>
            <span className="library-meta">
              {entry.format.toUpperCase()}
              {entry.pages != null ? ` · ${entry.pages} pages` : ""}
              {entry.missing ? " · file moved or deleted" : ""}
            </span>
          </button>
        </li>
      ))}
    </ul>
  );
}

export function ErrorBanner({ error }: { error: AppError | null }) {
  if (!error) return null;
  return (
    <div className="error-banner" role="alert">
      <strong>{error.code}</strong>
      <span>{error.message}</span>
    </div>
  );
}
