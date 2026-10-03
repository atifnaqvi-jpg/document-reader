import "./theme/tokens.css";
import "./App.css";

import { useCallback, useEffect, useRef, useState } from "react";
import { open as openFileDialog } from "@tauri-apps/plugin-dialog";
import { listLibrary, openDocument, toAppError } from "./api/invoke";
import type { AppError, LibraryEntry } from "./api/types";
import { EditorPane } from "./components/EditorPane";
import { ErrorBanner, LibraryPane } from "./components/LibraryPane";
import { IndexingIndicator, SearchPane } from "./components/SearchPane";
import { Toolbar } from "./components/Toolbar";
import { ViewerPane } from "./components/ViewerPane";
import type { JumpRequest, ViewerHandle } from "./components/ViewerPane";
import type { NavigationTarget } from "./components/Menu";
import { EditorProvider } from "./state/EditorProvider";
import { ReadingModeProvider, useReadingMode } from "./state/ReadingMode";
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
  // Which page the viewer is showing, reported up only so the EditorProvider can scope
  // its annotation list. The viewer remains the owner of its own page state.
  const [visiblePage, setVisiblePage] = useState(0);
  const { isFullscreen, chromeHidden, error: fullscreenError, toggleFullscreen, toggleChrome } =
    useReadingMode();

  // The menu drives page/zoom through the viewer's own handle rather than by lifting
  // viewer state into App (P1.9 D6).
  const viewerRef = useRef<ViewerHandle | null>(null);

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
    // A hit inside a note on another page has to move the viewer too, so annotation
    // hits navigate exactly like text hits (D5c).
    setJump((previous) => ({ page, nonce: (previous?.nonce ?? 0) + 1 }));
  }, []);

  const handleNavigate = useCallback((where: NavigationTarget) => {
    const viewer = viewerRef.current;
    if (!viewer) return;
    if (where === "first") viewer.goToFirst();
    else if (where === "last") viewer.goToLast();
    else if (where === "previous") viewer.step(-1);
    else viewer.step(1);
  }, []);

  // A bookmark click reuses the search-hit path, so the viewer's clamping and nonce
  // handling stay in one place.
  const handleOpenBookmark = useCallback(
    (page: number) => {
      if (!selected) return;
      setJump((previous) => ({ page, nonce: (previous?.nonce ?? 0) + 1 }));
    },
    [selected],
  );

  const getCurrentPage = useCallback(() => viewerRef.current?.getCurrentPage() ?? 0, []);

  return (
    <div className="app">
      <Toolbar
        busy={busy}
        onOpen={handleOpen}
        entry={selected}
        isFullscreen={isFullscreen}
        chromeHidden={chromeHidden}
        onToggleFullscreen={toggleFullscreen}
        onToggleChrome={toggleChrome}
        onNavigate={handleNavigate}
        onOpenBookmark={handleOpenBookmark}
        getCurrentPage={getCurrentPage}
      />
      <ErrorBanner error={error ?? fullscreenError} />
      <main className={`workspace${chromeHidden ? " chrome-hidden" : ""}`}>
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
          {/* The provider sits above both the palette and the viewer: the palette sets
              the tool, the viewer's overlay draws with it, and the provider needs the
              page the viewer is on. */}
          <EditorProvider documentId={selected?.id ?? null} page={visiblePage}>
            <EditorPane entry={selected} />
            <ViewerPane
              ref={viewerRef}
              entry={selected}
              jump={jump}
              onPageChange={setVisiblePage}
            />
          </EditorProvider>
        </section>
      </main>
    </div>
  );
}

export default function App() {
  return (
    <ThemeProvider>
      <ReadingModeProvider>
        <Workspace />
      </ReadingModeProvider>
    </ThemeProvider>
  );
}
