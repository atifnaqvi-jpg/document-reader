import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import type {
  Annotation,
  AnnotationChanged,
  AnnotationKind,
  AppError,
  Bookmark,
  DocumentMeta,
  IndexStatus,
  IndexSummary,
  LibraryEntry,
  RenderedPage,
  SearchHit,
} from "./types";
import { ANNOTATION_CHANGED_EVENT } from "./types";

export function isAppError(value: unknown): value is AppError {
  return (
    typeof value === "object" &&
    value !== null &&
    "code" in value &&
    "message" in value
  );
}

/** Normalises anything a rejected command can throw into an AppError. */
export function toAppError(value: unknown): AppError {
  if (isAppError(value)) return value;
  return { code: "UNKNOWN", message: String(value) };
}

export const openDocument = (path: string) =>
  invoke<DocumentMeta>("open_document", { path });

export const listLibrary = () => invoke<LibraryEntry[]>("list_library");

export const search = (query: string, limit: number) =>
  invoke<SearchHit[]>("search", { query, limit });

export const addBookmark = (
  documentId: string,
  page: number | null,
  offset: number | null,
  label: string | null,
) => invoke<Bookmark>("add_bookmark", { documentId, page, offset, label });

export const listBookmarks = (documentId: string) =>
  invoke<Bookmark[]>("list_bookmarks", { documentId });

export const removeBookmark = (bookmarkId: string) =>
  invoke<void>("remove_bookmark", { bookmarkId });

export const setAppState = (key: string, value: string) =>
  invoke<void>("set_app_state", { key, value });

/** Reads back state written by {@link setAppState} (P1-D7). `null` when never set. */
export const getAppState = (key: string) =>
  invoke<string | null>("get_app_state", { key });

/**
 * A document's index status (P1-D6). `null` means never indexed. Read after a remount,
 * because the `index://progress` event may have fired while the pane was unmounted.
 */
export const getIndexStatus = (documentId: string) =>
  invoke<IndexStatus | null>("get_index_status", { documentId });

/**
 * Indexes a document and resolves when the run finishes. `open_document` already starts
 * indexing, so this is for an explicit retry; progress normally arrives via the
 * `index://progress` event rather than by awaiting this.
 */
export const indexDocument = (documentId: string) =>
  invoke<IndexSummary>("index_document", { documentId });

/**
 * Renders one page (P1-D2). Expensive on the Rust side — the backend runs it on a
 * blocking worker, so callers should treat concurrent calls as normal and rely on the
 * document handle cache + render cache rather than de-duplicating here.
 */
export const getDocumentPageRender = (
  docId: string,
  page: number,
  scale: number,
  /**
   * P2-D9: `true` composites the annotation layer into the raster (the default view),
   * `false` renders the untouched original. This is a render-time switch, not a DOM
   * hide, so the pixels and the overlay must agree on which view is showing.
   */
  annotations = true,
) =>
  invoke<RenderedPage>("get_document_page_render", { docId, page, scale, annotations });

/** Extracts one page's text layer (feeds indexing/search in P1.6/P1.7). */
export const getDocumentPageText = (docId: string, page: number) =>
  invoke<string>("get_document_page_text", { docId, page });

/**
 * Page count as the renderer sees it. The import-time count can be `null`, so the
 * viewer asks for this when it needs an authoritative bound for navigation.
 */
export const getDocumentPageCount = (docId: string) =>
  invoke<number>("get_document_page_count", { docId });

/** Builds a `data:` URL for a rendered page's base64 PNG. */
export const pageImageSrc = (rendered: RenderedPage) =>
  `data:image/png;base64,${rendered.bytes}`;

/**
 * Whether the OS window is currently fullscreen (P1.9). `is_fullscreen` is part of
 * Tauri's `core:window:default` permission set, so unlike {@link setWindowFullscreen}
 * this needs no extra grant.
 *
 * Read back after toggling rather than trusting local state: the OS can leave
 * fullscreen on its own, and a permission failure would otherwise leave the UI
 * claiming a mode the window is not in.
 */
export const isWindowFullscreen = () => getCurrentWindow().isFullscreen();

/**
 * Puts the OS window in or out of fullscreen (P1.9).
 *
 * Requires `core:window:allow-set-fullscreen`, which is **not** in
 * `core:window:default`; it is granted in `capabilities/default.json` and asserted by
 * `src-tauri/tests/capabilities.rs`. Without the grant this rejects at runtime.
 */
export const setWindowFullscreen = (value: boolean) =>
  getCurrentWindow().setFullscreen(value);

/**
 * Subscribes to window resizes, used to notice the OS leaving fullscreen (P1.9).
 * Resolves to an unlisten function.
 */
export const onWindowResized = (handler: () => void) =>
  getCurrentWindow().onResized(() => handler());

/**
 * Subscribes to debounced annotation changes (P2-D11). Resolves to an unlisten function.
 *
 * The handler receives the document id from the event; listeners must re-read the DB for
 * state rather than inferring it from the payload.
 */
export const onAnnotationChanged = (handler: (change: AnnotationChanged) => void) =>
  listen<AnnotationChanged>(ANNOTATION_CHANGED_EVENT, (event) => handler(event.payload));

// --- P2: editing ---------------------------------------------------------------

/** Creates an annotation; the backend writes its FTS row in the same transaction (D5). */
export const addAnnotation = (
  documentId: string,
  page: number,
  kind: AnnotationKind,
  data: string,
  text: string | null,
) => invoke<Annotation>("add_annotation", { documentId, page, kind, data, text });

export const updateAnnotation = (
  annotationId: string,
  data: string,
  text: string | null,
) => invoke<Annotation>("update_annotation", { annotationId, data, text });

export const listAnnotations = (documentId: string) =>
  invoke<Annotation[]>("list_annotations", { documentId });

/** Only the annotations on one page, which is what the overlay renders. */
export const listPageAnnotations = (documentId: string, page: number) =>
  invoke<Annotation[]>("list_page_annotations", { documentId, page });

export const removeAnnotation = (annotationId: string) =>
  invoke<void>("remove_annotation", { annotationId });

/**
 * The path a "save edits" would write (P2-D2), for showing the filename in a confirm
 * dialog before committing. Resolved on the Rust side so the version number in the name
 * cannot race between two saves.
 */
export const nextSavePath = (documentId: string) =>
  invoke<string>("next_save_path", { documentId });

/**
 * Writes the document's annotations to a new versioned file (P2-D2) and returns that
 * path. The original is never modified by this call.
 */
export const saveEdits = (documentId: string) =>
  invoke<string>("save_edits", { documentId });

/**
 * Re-inserts a deleted annotation under its original id (P2-D7). Undo needs the whole
 * row back — a fresh id would break anything already holding the old one.
 */
export const restoreAnnotation = (annotation: Annotation) =>
  invoke<void>("restore_annotation", { annotation });
