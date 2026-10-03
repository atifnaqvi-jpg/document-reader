import { useEffect, useRef, useState } from "react";
import type { ReactNode } from "react";
import { addBookmark, listBookmarks, removeBookmark, toAppError } from "../api/invoke";
import type { AppError, Bookmark, LibraryEntry } from "../api/types";

/**
 * The top-bar menu (docs/P1.9-DESIGN.md D5).
 *
 * One button opening one panel of labelled groups, rather than a row of dropdown roots:
 * the toolbar has to survive a 700px window, and a single root keeps focus handling to
 * one place (D9).
 *
 * Only items that already work are present. Nothing here is a placeholder for a
 * feature that does not exist yet.
 */

interface Props {
  busy: boolean;
  entry: LibraryEntry | null;
  onOpen: () => void;
  isFullscreen: boolean;
  chromeHidden: boolean;
  onToggleFullscreen: () => void;
  onToggleChrome: () => void;
  /** Move the viewer; resolved against the viewer handle by the caller. */
  onNavigate: (where: NavigationTarget) => void;
  onOpenBookmark: (page: number) => void;
  /** Zero-based current page, read at the moment "Add bookmark" is chosen. */
  getCurrentPage: () => number;
}

export type NavigationTarget = "first" | "previous" | "next" | "last";

export function Menu({
  busy,
  entry,
  onOpen,
  isFullscreen,
  chromeHidden,
  onToggleFullscreen,
  onToggleChrome,
  onNavigate,
  onOpenBookmark,
  getCurrentPage,
}: Props) {
  const [open, setOpen] = useState(false);
  const [bookmarks, setBookmarks] = useState<Bookmark[]>([]);
  const [error, setError] = useState<AppError | null>(null);
  const rootRef = useRef<HTMLDivElement | null>(null);
  const buttonRef = useRef<HTMLButtonElement | null>(null);

  // Close on outside click and on Escape. Escape is also how fullscreen is left, but
  // ReadingModeProvider only acts on it when the menu is not the focus of attention —
  // closing the menu is the narrower, expected meaning.
  useEffect(() => {
    if (!open) return;

    const onPointerDown = (event: MouseEvent) => {
      if (rootRef.current && !rootRef.current.contains(event.target as Node)) {
        setOpen(false);
      }
    };
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        // Claim the key so ReadingModeProvider does not also exit fullscreen: closing the
        // menu is the narrower, expected meaning.
        event.preventDefault();
        event.stopPropagation();
        setOpen(false);
        buttonRef.current?.focus();
      }
    };
    document.addEventListener("mousedown", onPointerDown);
    // Capture so the menu closes before fullscreen handling sees Escape.
    window.addEventListener("keydown", onKey, true);
    return () => {
      document.removeEventListener("mousedown", onPointerDown);
      window.removeEventListener("keydown", onKey, true);
    };
  }, [open]);

  // Bookmarks are per-document and can change while the menu is closed (a document is
  // replaced by a re-import), so they are refetched on open rather than cached.
  useEffect(() => {
    if (!open) return;
    if (!entry) {
      setBookmarks([]);
      setError(null);
      return;
    }
    let cancelled = false;
    listBookmarks(entry.id)
      .then((list) => {
        if (!cancelled) setBookmarks(list);
      })
      .catch((cause: unknown) => {
        if (!cancelled) setError(toAppError(cause));
      });
    return () => {
      cancelled = true;
    };
  }, [open, entry]);

  const handleAddBookmark = () => {
    if (!entry) return;
    const page = getCurrentPage();
    addBookmark(entry.id, page, null, null)
      .then(() => listBookmarks(entry.id))
      .then(setBookmarks)
      .then(() => setOpen(false))
      .catch((cause: unknown) => setError(toAppError(cause)));
  };

  const handleRemoveBookmark = (bookmarkId: string) => {
    removeBookmark(bookmarkId)
      .then(() => (entry ? listBookmarks(entry.id) : Promise.resolve([])))
      .then(setBookmarks)
      .catch((cause: unknown) => setError(toAppError(cause)));
  };

  return (
    <div className="menu-root" ref={rootRef}>
      <button
        type="button"
        ref={buttonRef}
        className="menu-button"
        aria-haspopup="menu"
        aria-expanded={open}
        onClick={() => setOpen((value) => !value)}
      >
        ☰ Menu
      </button>

      {open ? (
        <div className="menu-panel" role="menu" aria-label="Main menu">
          <p className="menu-group">File</p>
          <button
            type="button"
            role="menuitem"
            disabled={busy}
            onClick={() => {
              setOpen(false);
              onOpen();
            }}
          >
            {busy ? "Opening…" : "Open…"}
          </button>

          <p className="menu-group">View</p>
          <button
            type="button"
            role="menuitem"
            onClick={() => {
              setOpen(false);
              onToggleFullscreen();
            }}
          >
            {isFullscreen ? "Exit fullscreen" : "Enter fullscreen"}
            <span className="menu-hint">F11</span>
          </button>
          <button
            type="button"
            role="menuitem"
            onClick={() => {
              setOpen(false);
              onToggleChrome();
            }}
          >
            {chromeHidden ? "Show side panels" : "Hide side panels"}
          </button>

          <p className="menu-group">Go</p>
          <MenuItem
            disabled={!entry}
            onClick={() => runNavigation("first")}
            hint="Home"
          >
            First page
          </MenuItem>
          <MenuItem
            disabled={!entry}
            onClick={() => runNavigation("previous")}
            hint="←"
          >
            Previous page
          </MenuItem>
          <MenuItem disabled={!entry} onClick={() => runNavigation("next")} hint="→">
            Next page
          </MenuItem>
          <MenuItem disabled={!entry} onClick={() => runNavigation("last")} hint="End">
            Last page
          </MenuItem>

          <p className="menu-group">Bookmarks</p>
          <MenuItem disabled={!entry} onClick={handleAddBookmark}>
            Add bookmark (page {getCurrentPage() + 1})
          </MenuItem>
          {entry ? (
            bookmarks.length > 0 ? (
              <ul className="menu-bookmarks">
                {bookmarks.map((bookmark) => (
                  <li key={bookmark.id}>
                    <button
                      type="button"
                      role="menuitem"
                      className="menu-bookmark"
                      onClick={() => {
                        setOpen(false);
                        // A bookmark made before page metadata was known has no page yet;
                        // there is nowhere to navigate to, so leave the reader put.
                        if (bookmark.page !== null) onOpenBookmark(bookmark.page);
                      }}
                    >
                      {bookmark.page !== null ? `Page ${bookmark.page + 1}` : "No page"}
                      {bookmark.label ? ` — ${bookmark.label}` : ""}
                    </button>
                    <button
                      type="button"
                      className="menu-bookmark-remove"
                      aria-label={`Remove bookmark${bookmark.page !== null ? ` on page ${bookmark.page + 1}` : ""}`}
                      onClick={() => handleRemoveBookmark(bookmark.id)}
                    >
                      ✕
                    </button>
                  </li>
                ))}
              </ul>
            ) : (
              <p className="menu-empty muted">No bookmarks yet.</p>
            )
          ) : (
            <p className="menu-empty muted">Open a document to bookmark pages.</p>
          )}

          {error ? (
            <p className="menu-error" role="alert">
              {error.code}: {error.message}
            </p>
          ) : null}
        </div>
      ) : null}
    </div>
  );

  function runNavigation(where: NavigationTarget) {
    setOpen(false);
    onNavigate(where);
  }
}

/** A menu row with an optional accelerator hint on the right. */
function MenuItem({
  children,
  onClick,
  disabled,
  hint,
}: {
  children: ReactNode;
  onClick: () => void;
  disabled?: boolean;
  hint?: string;
}) {
  return (
    <button type="button" role="menuitem" disabled={disabled} onClick={onClick}>
      {children}
      {hint ? <span className="menu-hint">{hint}</span> : null}
    </button>
  );
}
