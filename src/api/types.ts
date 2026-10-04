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
  /** Where the hit came from (P2-D5): the document's own text, or an annotation. */
  origin: SearchOrigin;
  /** The annotation a hit came from, so the UI can select it. `null` for text hits. */
  annotationId: string | null;
}

/**
 * Mirrors `model::SearchOrigin`. A term can appear both in the book and in a note the
 * user wrote; the UI labels the two differently rather than presenting them as one list.
 */
export type SearchOrigin = "document" | "annotation";

/** Mirrors `model::AnnotationKind` (P2-D4). */
export type AnnotationKind =
  | "highlight"
  | "underline"
  | "strikeout"
  | "note"
  | "ink"
  | "shape";

/**
 * Geometry of an annotation, stored as schema-versioned JSON (P2-D4) so a new tool
 * can ship without a table migration. `v` must be checked before reading the rest.
 */
export interface HighlightData {
  v: 1;
  rects: [number, number, number, number][];
  color?: string;
  opacity?: number;
  text?: null;
}

export interface NoteData {
  v: 1;
  point: [number, number];
  color?: string;
}

export interface InkData {
  v: 1;
  strokes: [number, number][][];
  color?: string;
}

export interface ShapeData {
  v: 1;
  rect: [number, number, number, number];
  color?: string;
}

export type AnnotationData = HighlightData | NoteData | InkData | ShapeData;

/** Mirrors `model::Annotation` (P2-D4). `text` is denormalised for the FTS index (D5). */
export interface Annotation {
  id: string;
  documentId: string;
  page: number;
  kind: AnnotationKind;
  /** The JSON blob; parse with {@link parseAnnotationData} rather than casting. */
  data: string;
  text: string | null;
  createdAt: number;
  modifiedAt: number;
}

/** Per-tool style remembered across sessions (P2-D10). */
export interface ToolStyle {
  color: string;
  opacity: number;
}

/** The tool styles the UI needs defaults for; must list every {@link AnnotationKind}. */
export const ANNOTATION_KINDS: AnnotationKind[] = [
  "highlight",
  "underline",
  "strikeout",
  "note",
  "ink",
  "shape",
];

/**
 * Parses a stored blob, rejecting anything newer than this build understands.
 *
 * Returning `null` rather than a partially-filled object is deliberate: a `v:2` blob
 * may have moved the geometry fields, and guessing would place a mark in the wrong
 * place instead of simply not drawing it.
 */
export function parseAnnotationData(annotation: Annotation): AnnotationData | null {
  let parsed: unknown;
  try {
    parsed = JSON.parse(annotation.data);
  } catch {
    return null;
  }
  if (typeof parsed !== "object" || parsed === null) return null;
  const version = (parsed as { v?: unknown }).v;
  if (version !== 1) return null;
  return parsed as AnnotationData;
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

export type IndexState = "running" | "done" | "failed";

/** Mirrors `index::IndexStatus` (P1-D6). Read back after a remount, not just on event. */
export interface IndexStatus {
  state: IndexState;
  done: number;
  total: number;
}

/** Payload of the `index://progress` event. */
export interface IndexProgress {
  docId: string;
  done: number;
  total: number;
}

export interface IndexSummary {
  pagesIndexed: number;
  pagesSkipped: number;
  cancelled: boolean;
}

/** Event name emitted by the backend while indexing (P1-D6). */
export const INDEX_PROGRESS_EVENT = "index://progress";

/**
 * Payload of the `annotation://changed` event (P2-D11).
 *
 * Carries a document id only, by design: the event says something changed, and listeners
 * re-read the DB for the actual state. Treating it as a source of truth would let a
 * dropped or coalesced event desynchronise a pane.
 */
export interface AnnotationChanged {
  documentId: string;
}

/** Event name emitted, debounced 200ms, after a document's annotations change (P2-D11). */
export const ANNOTATION_CHANGED_EVENT = "annotation://changed";
