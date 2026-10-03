import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useRef,
  useState,
} from "react";
import type { ReactNode } from "react";
import {
  getAppState,
  isWindowFullscreen,
  onWindowResized,
  setAppState,
  setWindowFullscreen,
} from "../api/invoke";
import type { AppError } from "../api/types";

/**
 * Fullscreen reading mode (docs/P1.9-DESIGN.md D1).
 *
 * Two flags, one user-facing toggle:
 * - `isFullscreen` — the OS window is in fullscreen. Its source of truth is the window
 *   itself, not this state; see the reconciliation below.
 * - `chromeHidden` — "focus mode": the library/search panes and the metadata block are
 *   hidden, leaving the page and a slim toolbar. Independent of fullscreen, so it can be
 *   toggled on its own.
 *
 * Entering fullscreen also hides the chrome, remembering what it was; leaving restores
 * that remembered value, so a user who already had the panes hidden does not get them
 * back by pressing Escape or F11 (D4).
 */

interface ReadingModeContextValue {
  isFullscreen: boolean;
  chromeHidden: boolean;
  /** Set when a fullscreen request fails (D11), so the UI can explain itself. */
  error: AppError | null;
  toggleFullscreen: () => void;
  toggleChrome: () => void;
}

const ReadingModeContext = createContext<ReadingModeContextValue>({
  isFullscreen: false,
  chromeHidden: false,
  error: null,
  toggleFullscreen: () => {},
  toggleChrome: () => {},
});

/** AppState key for the persisted chrome preference (D3). */
const CHROME_KEY = "reading.chromeHidden";

export function ReadingModeProvider({ children }: { children: ReactNode }) {
  const [isFullscreen, setIsFullscreen] = useState(false);
  const [chromeHidden, setChromeHidden] = useState(false);
  const [error, setError] = useState<AppError | null>(null);

  // What `chromeHidden` was before fullscreen hid it, so exiting can put it back (D4).
  // `undefined` means "fullscreen is not what hid the chrome" — either we are not in
  // fullscreen, or the user toggled the chrome by hand while in it.
  const chromeBeforeFullscreen = useRef<boolean | undefined>(undefined);

  const restoreChrome = useCallback(() => {
    const previous = chromeBeforeFullscreen.current;
    chromeBeforeFullscreen.current = undefined;
    // Restore only when fullscreen is what hid the chrome; a manual toggle wins.
    if (previous !== undefined) setChromeHidden(previous);
  }, []);

  // The window is the source of truth for fullscreen (D11). Read it back after every
  // change and on resize, because the OS can leave fullscreen without asking us.
  const syncFullscreen = useCallback(() => {
    isWindowFullscreen()
      .then((current) => {
        setIsFullscreen((previous) => {
          if (previous === current) return previous;
          // The state and the window diverged; drop any stale snapshot so a later exit
          // does not restore chrome from an outdated run.
          if (!current) chromeBeforeFullscreen.current = undefined;
          return current;
        });
      })
      .catch((cause: unknown) => {
        // Reading the flag can only fail if the window or the permission is wrong; the
        // window's real state is then unknown, so leave the UI as it is rather than guess.
        setError(toReadingError(cause));
      });
  }, []);

  useEffect(() => {
    syncFullscreen();
  }, [syncFullscreen]);

  // Persisted chrome preference (D3). The AppState row is read back on mount, matching
  // ThemeProvider's durable-copy approach.
  const [chromeReady, setChromeReady] = useState(false);
  useEffect(() => {
    let cancelled = false;
    getAppState(CHROME_KEY)
      .then((stored) => {
        if (cancelled) return;
        if (stored === "true" || stored === "false") {
          setChromeHidden(stored === "true");
        }
      })
      .catch(() => {
        // Never set is normal on first run; keep the default.
      })
      .finally(() => {
        if (!cancelled) setChromeReady(true);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  useEffect(() => {
    // Wait for the read-back so the write below cannot overwrite a stored "true" with the
    // default before it arrives.
    if (!chromeReady) return;
    // The hide that entering fullscreen performs is transient by design (D3): quitting
    // while in fullscreen must not leave the next launch with its side panels hidden and
    // no fullscreen to explain why. A live snapshot is exactly that transient case; a
    // user-initiated toggle clears it, so those still persist.
    if (chromeBeforeFullscreen.current !== undefined) return;
    void setAppState(CHROME_KEY, String(chromeHidden)).catch(() => {
      // Persistence is best-effort; reading must not break if the DB is unavailable.
    });
  }, [chromeHidden, chromeReady]);

  // Notice the OS leaving fullscreen behind our back (e.g. the user uses the window's own
  // restore control). Cheap, and it keeps the toggle from lying.
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let cancelled = false;
    onWindowResized(() => syncFullscreen())
      .then((fn) => {
        if (cancelled) fn();
        else unlisten = fn;
      })
      .catch(() => {
        // Not fatal: the toggle still reconciles on every use.
      });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [syncFullscreen]);

  const toggleFullscreen = useCallback(() => {
    // Resolve against the window, not the render-scope flag, so a rapid double press
    // cannot act on a stale value: the value passed to the window API and the value we
    // expect back must come from the same read.
    isWindowFullscreen()
      .then((currently) => {
        const next = !currently;
        return setWindowFullscreen(next).then(() => next);
      })
      .then((next) =>
        // Confirm what the window actually did (D11): if the OS refused, the UI must not
        // claim a mode the window is not in, and the chrome must stay as it is.
        isWindowFullscreen().then((actual) => ({ next, actual })),
      )
      .then(({ next, actual }) => {
        setError(null);
        setIsFullscreen(actual);
        if (actual !== next) return;
        if (next) {
          chromeBeforeFullscreen.current = chromeHidden;
          setChromeHidden(true);
        } else {
          restoreChrome();
        }
      })
      .catch((cause: unknown) => setError(toReadingError(cause)));
  }, [chromeHidden, restoreChrome]);

  const toggleChrome = useCallback(() => {
    // Toggling the chrome by hand means fullscreen is no longer what controls it, so the
    // snapshot must not override that choice when fullscreen is left later (D4).
    chromeBeforeFullscreen.current = undefined;
    setChromeHidden((hidden) => !hidden);
  }, []);

  // Escape leaves fullscreen, and F11 toggles it — the keys every PDF reader uses.
  // Registered here rather than in the viewer so they work no matter where focus sits.
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "F11") {
        event.preventDefault();
        toggleFullscreen();
      } else if (event.key === "Escape" && isFullscreen) {
        // A component that consumes Escape (an open menu) sets this flag, so closing the
        // menu does not also drop the user out of fullscreen.
        if (event.defaultPrevented) return;
        event.preventDefault();
        toggleFullscreen();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [isFullscreen, toggleFullscreen]);

  return (
    <ReadingModeContext.Provider
      value={{ isFullscreen, chromeHidden, error, toggleFullscreen, toggleChrome }}
    >
      {children}
    </ReadingModeContext.Provider>
  );
}

export function useReadingMode() {
  return useContext(ReadingModeContext);
}

/** Normalises a window-API rejection into the app's error shape (D11). */
function toReadingError(cause: unknown): AppError {
  return {
    code: "FULLSCREEN_UNAVAILABLE",
    message:
      "Fullscreen mode could not be changed. " +
      (cause instanceof Error ? cause.message : String(cause)),
  };
}
