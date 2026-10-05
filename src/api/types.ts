// Mirrors src-tauri/src/model/mod.rs. Keep in sync manually — see docs/P0-DESIGN.md §7.

import type { PdfRect } from "../state/geometry";

export type Format = "pdf" | "txt" | "md";

export interface DocumentMeta {
  id: string;
  title: string | null;
  author: string | null;
  pages: number | null;
  format: Format;
  /**
   * The file declares `/Encrypt` (P2.4 T7). A protected document still opens and reads;
   * every edit refuses with `ENCRYPTED_READ_ONLY` until the user makes a copy with
   * "Decrypt a copy".
   */
  encrypted: boolean;
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

// --- P2.4: content-stream text editing -----------------------------------------

/**
 * Address of one string operand inside a page's decoded content stream (P2.4 T4).
 *
 * This — not the visible text — is what identifies a run. The visible text is
 * re-validated against it at write time, and a disagreement refuses with
 * `TEXT_RUN_MISMATCH` rather than editing the wrong bytes.
 */
export interface TextRunId {
  opIndex: number;
  operandPath: number[];
}

/** The Stage-1 verdict for one run, precomputed by the backend (P2.4 §4/T3). */
export interface TextAdmissibility {
  editable: boolean;
  /** Name of the first violated gate, e.g. `fits-width`; `null` when editable. */
  gate: string | null;
  /**
   * A calm sentence naming the gate. For the width gate it quotes both measured widths,
   * so a refusal is actionable rather than mysterious.
   */
  message: string | null;
}

/** One text run as the picker sees it (mirrors `editor::textedit::PageTextRun`). */
export interface PageTextRun {
  runId: TextRunId;
  text: string;
  operator: string;
  fontKey: string;
  fontName: string;
  fontSize: number;
  /** Box in PDF points (bottom-left origin); `null` when PDFium could not match it. */
  rect: PdfRect | null;
  /** Advance width in points, the `oldWidth` the width gate compares against. */
  width: number | null;
  admissibility: TextAdmissibility;
}

/** The inverse of a text edit, for the session undo stack (P2.4 T5). */
export interface ContentOperation {
  page: number;
  opIndex: number;
  operandPath: number[];
  oldBytes: number[];
  newBytes: number[];
}

export interface TextEditResult {
  /** The new file the edit was written to. Never the original (D2). */
  path: string;
  inverse: ContentOperation;
  text: string;
}

export interface DecryptOutcome {
  path: string;
  pages: number;
}

/** Error code a protected document returns from every edit command (P2.4 T7). */
export const ENCRYPTED_READ_ONLY = "ENCRYPTED_READ_ONLY";
/** Error code when a supplied password does not unlock the document (P2.4 T8). */
export const WRONG_PASSWORD = "WRONG_PASSWORD";
