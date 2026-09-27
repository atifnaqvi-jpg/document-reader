import { useTheme } from "../theme/ThemeProvider";

interface Props {
  busy: boolean;
  onOpen: () => void;
}

export function Toolbar({ busy, onOpen }: Props) {
  const { theme, toggleTheme } = useTheme();

  return (
    <header className="toolbar">
      <div className="toolbar-left">
        <span className="app-name">Document Reader</span>
        <button type="button" onClick={onOpen} disabled={busy}>
          {busy ? "Opening…" : "Open…"}
        </button>
      </div>
      <div className="toolbar-right">
        <button type="button" onClick={toggleTheme} title="Toggle theme">
          {theme === "dark" ? "☀ Light" : "☾ Dark"}
        </button>
      </div>
    </header>
  );
}
