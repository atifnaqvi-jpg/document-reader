import "./theme/tokens.css";
import "./App.css";

import { useCallback, useEffect, useState } from "react";
import { open as openFileDialog } from "@tauri-apps/plugin-dialog";
import { listLibrary, openDocument, toAppError } from "./api/invoke";
import type { AppError, LibraryEntry } from "./api/types";
import { ErrorBanner, LibraryPane } from "./components/LibraryPane";
import { IndexingIndicator, SearchPane } from "./components/SearchPane";
import { Toolbar } from "./components/Toolbar";
import { ViewerPane } from "./components/ViewerPane";
import type { JumpRequest } from "./components/ViewerPane";
import { ThemeProvider } from "./theme/ThemeProvider";

const OPEN_FILTERS = [
  { name: "Documents", extensions: ["pdf", "txt", "md"] },
];

function Workspace() {
  const [entries, setEntries] = useState<LibraryEntry[]>([]);
  const [selected, setSelected] = useState<LibraryEntry | null>(null);
  const [error, setError] = useState<AppError | null>(null);
  const [busy, setBusy] = useState(false);
  const [jump, setJump] = useState<JumpRequest | null>(null);

  const refreshLibrary = useCallback(async () => {
    const list = await listLibrary();
    setEntries(list);
    return list;
  }, []);

  useEffect(() => {
    refreshLibrary().catch((e) => setError(toAppError(e)));
  }, [refreshLibrary]);

  const handleOpen = useCallback(async () => {
    setError(null);
    try {
      const picked = await openFileDialog({ multiple: false, filters: OPEN_FILTERS });
      if (typeof picked !== "string") return;

      setBusy(true);
      const meta = await openDocument(picked);
      const list = await refreshLibrary();
      // Opening a document starts from its first page, not wherever the previous
      // document was left, so any pending jump is dropped.
      setJump(null);
      setSelected(list.find((e) => e.id === meta.id) ?? null);
    } catch (e) {
      setError(toAppError(e));
    } finally {
      setBusy(false);
    }
  }, [refreshLibrary]);

  const handleSelect = useCallback((entry: LibraryEntry) => {
    setJump(null);
    setSelected(entry);
  }, []);

  // A search hit selects a document and asks the viewer for a page. The nonce must change
  // even when the page does not, so clicking the same hit twice re-navigates.
  const handleOpenHit = useCallback((entry: LibraryEntry, page: number) => {
    setSelected(entry);
    setJump((previous) => ({ page, nonce: (previous?.nonce ?? 0) + 1 }));
  }, []);

  return (
    <div className="app">
      <Toolbar busy={busy} onOpen={handleOpen} />
      <ErrorBanner error={error} />
      <main className="workspace">
        <aside className="sidebar">
          <LibraryPane
            entries={entries}
            selectedId={selected?.id ?? null}
            onSelect={handleSelect}
          />
        </aside>
        <aside className="search-sidebar">
          <SearchPane entries={entries} onOpenHit={handleOpenHit} />
        </aside>
        <section className="content">
          <IndexingIndicator docId={selected?.id ?? null} />
          <ViewerPane entry={selected} jump={jump} />
        </section>
      </main>
    </div>
  );
}

export default function App() {
  return (
    <ThemeProvider>
      <Workspace />
    </ThemeProvider>
  );
}
