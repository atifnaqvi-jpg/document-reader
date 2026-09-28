// Mirrors src-tauri/src/model/mod.rs. Keep in sync manually — see docs/P0-DESIGN.md §7.

export type Format = "pdf" | "txt" | "md";

export interface DocumentMeta {
  id: string;
  title: string | null;
  author: string | null;
  pages: number | null;
  format: Format;
}

export interface LibraryEntry extends DocumentMeta {
  path: string;
  missing: boolean;
  addedAt: number;
  lastOpenedAt: number | null;
}

export interface Bookmark {
  id: string;
  documentId: string;
  page: number | null;
  offset: number | null;
  label: string | null;
  createdAt: number;
}

export interface SearchHit {
  documentId: string;
  page: number;
  snippet: string;
}

/**
 * A rendered page (P1-D2). `bytes` is base64-encoded PNG as serialized by Tauri;
 * `width`/`height` are the rendered pixel dimensions, so the viewer can reserve the
 * right space before the image decodes and avoid a layout jump on page turn.
 */
export interface RenderedPage {
  bytes: string;
  width: number;
  height: number;
}

export interface AppError {
  code: string;
  message: string;
}
