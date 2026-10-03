import { useTheme } from "../theme/ThemeProvider";
import type { LibraryEntry } from "../api/types";
import { Menu } from "./Menu";
import type { NavigationTarget } from "./Menu";

interface Props {
  busy: boolean;
  onOpen: () => void;
  entry: LibraryEntry | null;
  isFullscreen: boolean;
  chromeHidden: boolean;
  onToggleFullscreen: () => void;
  onToggleChrome: () => void;
  onNavigate: (where: NavigationTarget) => void;
  onOpenBookmark: (page: number) => void;
  getCurrentPage: () => number;
}

export function Toolbar({
  busy,
  onOpen,
  entry,
  isFullscreen,
  chromeHidden,
  onToggleFullscreen,
  onToggleChrome,
  onNavigate,
  onOpenBookmark,
  getCurrentPage,
}: Props) {
  const { theme, toggleTheme } = useTheme();

  return (
    <header className="toolbar">
      <div className="toolbar-left">
        <span className="app-name">Document Reader</span>
        <Menu
          busy={busy}
          entry={entry}
          onOpen={onOpen}
          isFullscreen={isFullscreen}
          chromeHidden={chromeHidden}
          onToggleFullscreen={onToggleFullscreen}
          onToggleChrome={onToggleChrome}
          onNavigate={onNavigate}
          onOpenBookmark={onOpenBookmark}
          getCurrentPage={getCurrentPage}
        />
        <button type="button" onClick={onOpen} disabled={busy}>
          {busy ? "Opening…" : "Open…"}
        </button>
      </div>
      <div className="toolbar-right">
        <button type="button" onClick={toggleTheme} title="Toggle theme">
          {theme === "dark" ? "☀ Light" : "☾ Dark"}
        </button>
        {/* A direct entry point next to the menu entry: both flip the same state (D7). */}
        <button
          type="button"
          onClick={onToggleFullscreen}
          title={isFullscreen ? "Exit fullscreen (F11)" : "Enter fullscreen (F11)"}
          aria-pressed={isFullscreen}
        >
          {isFullscreen ? "⛗ Exit fullscreen" : "⛶ Fullscreen"}
        </button>
      </div>
    </header>
  );
}
