import { invoke } from "@tauri-apps/api/core";
import type { AppError, Bookmark, DocumentMeta, LibraryEntry, SearchHit } from "./types";

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
