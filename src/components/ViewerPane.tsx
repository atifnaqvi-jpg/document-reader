import type { LibraryEntry } from "../api/types";

export function ViewerPane({ entry }: { entry: LibraryEntry | null }) {
  if (!entry) {
    return (
      <div className="pane-empty">
        <p>No document open.</p>
        <p className="muted">Select a document from the library, or open a new file.</p>
      </div>
    );
  }

  return (
    <div className="viewer">
      <header className="viewer-header">
        <h1>{entry.title ?? "(untitled)"}</h1>
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
            <dt>Pages</dt>
            <dd>{entry.pages ?? "—"}</dd>
          </div>
          <div>
            <dt>Path</dt>
            <dd className="path">{entry.path}</dd>
          </div>
        </dl>
      </header>
      <div className="viewer-canvas">
        <p className="muted">
          Page rendering arrives in P1 (native PDFium). P0 shows document metadata only.
        </p>
      </div>
    </div>
  );
}
