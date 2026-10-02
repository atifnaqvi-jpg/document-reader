import { invoke } from "@tauri-apps/api/core";
import type {
  AppError,
  Bookmark,
  DocumentMeta,
  IndexStatus,
  IndexSummary,
  LibraryEntry,
  RenderedPage,
  SearchHit,
} from "./types";

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
export const getDocumentPageRender = (docId: string, page: number, scale: number) =>
  invoke<RenderedPage>("get_document_page_render", { docId, page, scale });

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
