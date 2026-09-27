import "./theme/tokens.css";
import "./App.css";

import { useCallback, useEffect, useState } from "react";
import { open as openFileDialog } from "@tauri-apps/plugin-dialog";
import { listLibrary, openDocument, toAppError } from "./api/invoke";
import type { AppError, LibraryEntry } from "./api/types";
import { ErrorBanner, LibraryPane } from "./components/LibraryPane";
import { Toolbar } from "./components/Toolbar";
import { ViewerPane } from "./components/ViewerPane";
import { ThemeProvider } from "./theme/ThemeProvider";

const OPEN_FILTERS = [
  { name: "Documents", extensions: ["pdf", "txt", "md"] },
];

function Workspace() {
  const [entries, setEntries] = useState<LibraryEntry[]>([]);
  const [selected, setSelected] = useState<LibraryEntry | null>(null);
  const [error, setError] = useState<AppError | null>(null);
  const [busy, setBusy] = useState(false);

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
      setSelected(list.find((e) => e.id === meta.id) ?? null);
    } catch (e) {
      setError(toAppError(e));
    } finally {
      setBusy(false);
    }
  }, [refreshLibrary]);

  return (
    <div className="app">
      <Toolbar busy={busy} onOpen={handleOpen} />
      <ErrorBanner error={error} />
      <main className="workspace">
        <aside className="sidebar">
          <LibraryPane
            entries={entries}
            selectedId={selected?.id ?? null}
            onSelect={setSelected}
          />
        </aside>
        <section className="content">
          <ViewerPane entry={selected} />
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
