import { createContext, useCallback, useContext, useEffect, useState } from "react";
import type { ReactNode } from "react";
import { getAppState, setAppState } from "../api/invoke";

type Theme = "light" | "dark";

interface ThemeContextValue {
  theme: Theme;
  toggleTheme: () => void;
}

const ThemeContext = createContext<ThemeContextValue>({
  theme: "light",
  toggleTheme: () => {},
});

const STORAGE_KEY = "theme";

export function ThemeProvider({ children }: { children: ReactNode }) {
  const [theme, setTheme] = useState<Theme>(() => {
    // localStorage is the synchronous fast path, so the correct theme is applied before
    // the first paint. The SQLite AppState row is the durable copy and is what the
    // read-back below prefers on a machine where localStorage was cleared.
    const stored = localStorage.getItem(STORAGE_KEY);
    return stored === "dark" ? "dark" : "light";
  });

  // Read the persisted value back (P1-D7). P0 could only write state, so the theme was
  // lost whenever localStorage did not survive; the database is the source of truth.
  useEffect(() => {
    let cancelled = false;
    getAppState(STORAGE_KEY)
      .then((stored) => {
        if (cancelled) return;
        if (stored === "dark" || stored === "light") setTheme(stored);
      })
      .catch(() => {
        // A missing or unreadable row is normal on first run; keep the local value.
      });
    return () => {
      cancelled = true;
    };
  }, []);

  useEffect(() => {
    document.documentElement.dataset.theme = theme;
    localStorage.setItem(STORAGE_KEY, theme);
    void setAppState(STORAGE_KEY, theme).catch(() => {
      // Theme persistence is best-effort; the UI must not break if the DB is unavailable.
    });
  }, [theme]);

  const toggleTheme = useCallback(
    () => setTheme((t) => (t === "light" ? "dark" : "light")),
    [],
  );

  return (
    <ThemeContext.Provider value={{ theme, toggleTheme }}>
      {children}
    </ThemeContext.Provider>
  );
}

export function useTheme() {
  return useContext(ThemeContext);
}
