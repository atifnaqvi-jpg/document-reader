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
  | "squiggly"
  | "note"
  | "textbox"
  | "callout"
  | "typewriter"
  | "ink"
  | "line"
  | "arrow"
  | "rect"
  | "oval";

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

/**
 * A free-text box. `rect` is the box and the annotation's `text` is what goes in it, so the
 * text lives in one place rather than being duplicated into the blob.
 *
 * `callout` is the leader line's target point and is present only for a callout: the writer
 * refuses a callout without one rather than drawing a box, because a box is a different mark.
 */
export interface FreeTextData {
  v: 1;
  rect: [number, number, number, number];
  color?: string;
  callout?: [number, number];
}

/**
 * A line or arrow. The endpoints are the mark, so they are stored rather than a rect: a rect
 * cannot say which two corners the line runs between, and a diagonal and its bounding box are
 * different marks.
 */
export interface LineData {
  v: 1;
  from: [number, number];
  to: [number, number];
  color?: string;
}

/**
 * A review status (D-A7). The four the report lists, plus `null` for "no status" -- which is
 * a real answer, not a fifth value: an annotation nobody has triaged is not `accepted`.
 */
export type AnnotationStatus =
  | "accepted"
  | "rejected"
  | "completed"
  | "cancelled";

/** The statuses in report order, for rendering the picker without repeating the list. */
export const ANNOTATION_STATUSES: AnnotationStatus[] = [
  "accepted",
  "rejected",
  "completed",
  "cancelled",
];

/**
 * Keys every annotation blob can carry, independent of its geometry (D-A13).
 *
 * Read with {@link commonOf} rather than cast: a blob is `unknown` until parsed, and a pane
 * that assumed these keys were there would show `undefined` as a status.
 */
export interface CommonData {
  /** Absent means untriaged. */
  status?: AnnotationStatus;
  /** The annotation this one replies to (D-A5). Absent means it is a root. */
  parent?: string;
}

export type AnnotationData =
  | HighlightData
  | NoteData
  | InkData
  | ShapeData
  | FreeTextData
  | LineData;

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
  "squiggly",
  "note",
  "textbox",
  "callout",
  "typewriter",
  "ink",
  "line",
  "arrow",
  "rect",
  "oval",
];

/**
 * Parses a stored blob, rejecting anything newer than this build understands.
 *
 * Returning `null` rather than a partially-filled object is deliberate: a `v:2` blob
 * may have moved the geometry fields, and guessing would place a mark in the wrong
 * place instead of simply not drawing it.
 */
export function parseAnnotationData(annotation: Annotation): AnnotationData | null {
  const parsed = parseBlob(annotation);
  if (parsed === null) return null;
  return parsed as unknown as AnnotationData;
}

/** The parsed blob at any version this build accepts, or `null`. */
function parseBlob(annotation: Annotation): Record<string, unknown> | null {
  let parsed: unknown;
  try {
    parsed = JSON.parse(annotation.data);
  } catch {
    return null;
  }
  if (typeof parsed !== "object" || parsed === null) return null;
  const version = (parsed as { v?: unknown }).v;
  // v2 adds `status` and `parent` (D-A13) and is otherwise v1's shapes, so both versions
  // parse. A v3 from a newer build is refused, because its geometry may have moved.
  if (version !== 1 && version !== 2) return null;
  return parsed as Record<string, unknown>;
}

/**
 * The review keys of an annotation, whatever its geometry (D-A7/D-A5).
 *
 * A malformed or future blob yields no status and no parent rather than throwing: the
 * comment pane's job is to list what is there, and one unreadable row must not blank it.
 */
export function commonOf(annotation: Annotation): CommonData {
  const parsed = parseBlob(annotation);
  if (parsed === null) return {};
  const status = parsed.status;
  const parent = parsed.parent;
  return {
    status:
      typeof status === "string" && (ANNOTATION_STATUSES as string[]).includes(status)
        ? (status as AnnotationStatus)
        : undefined,
    parent: typeof parent === "string" && parent.length > 0 ? parent : undefined,
  };
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
  /**
   * The Form XObjects followed to reach this operand, outermost first (P2.7.3 §3).
   *
   * Empty or absent means the page's own content stream, so every address written before
   * P2.7.3 keeps its meaning. The step's `name` — a resource name, never an object id —
   * is what identifies the form: object ids are renumbered on save (Q2/B-7).
   */
  xobjectPath?: XObjectStep[];
}

/** One step of descent into a Form XObject (P2.7.3 §2.2). */
export interface XObjectStep {
  /** The `/XObject` entry's name in the enclosing resource dictionary (`X1`). */
  name: number[];
  /** Index of the `Do` in the enclosing content stream that drew it. */
  opIndex: number;
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

/**
 * How confidently a run was matched to a rendered box (P2.4.1 T17/§3.3).
 *
 * `strong` — the run's origin lies inside the box and its baseline agrees; `weak` —
 * nearest-only geometry. `null` means no plausible box, and the run was refused.
 */
export type PairConfidence = "strong" | "weak";

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
  /**
   * True when the run's paired box is line-sized for its own span (P2.4.2 T22).
   *
   * The 0.4.1 chain-break orphan claimed the whole line's box. The pair is flagged, and the
   * picker says so; `width` above is then the run's own span, never the line.
   */
  boxSuspicious?: boolean;
  admissibility: TextAdmissibility;
  /** How the run paired with a box (P2.4.1 T17/T21); `null` when unpaired. */
  pairConfidence: PairConfidence | null;
  /** How many content-stream fragments the run joins (P2.4.1 T18); `1` for an ordinary run. */
  fragmentCount: number;
  /** The fragments of a merged run, head first (P2.4.1 O-Pair-3). Absent for a single run. */
  fragments?: PageTextRun[];
  /** Whether the run's `/ActualText`, if any, could be updated with the edit (P2.7.1). */
  actualTextPreserved?: boolean;
}

/**
 * One tail fragment of a merged run, as the write path needs it (P2.4.1 T18).
 *
 * Carries the text the picker showed so the backend re-validates the fragment against the
 * document (T4) before clearing it; a stale entry refuses rather than blanking the wrong run.
 */
export interface TextRunFragmentRef {
  runId: TextRunId;
  text: string;
}

/** The inverse of a text edit, for the session undo stack (P2.4 T5). */
export interface ContentOperation {
  page: number;
  opIndex: number;
  operandPath: number[];
  oldBytes: number[];
  newBytes: number[];
  /**
   * The `/Properties` clone a named `ActualText` rewrite created (P2.7.1 ruling B-7). Undo
   * needs it to repoint the `BDC` at its original name and drop the clone, so it must round-trip
   * unchanged.
   */
  propertyClone?: {
    opIndex: number;
    oldName: number[];
    newName: number[];
    cloneObject: [number, number];
  } | null;
  /**
   * The content stream this edit wrote (P2.7.3 Q2). Empty/absent is the page's own stream, so
   * every 0.5.0 inverse keeps its meaning; a non-empty path names the Form XObject clone.
   */
  xobjectPath?: XObjectStep[];
  /**
   * The draws this edit copied, outermost first (P2.7.3 Q6). Undo restores each `Do` and drops
   * the clone, so one edit remains one undo entry.
   */
  xobjectClones?: XObjectClone[];
  /**
   * Ruling A (2026-11-02): this leg replaced the operation **vector** at `opIndex` rather than
   * one operand's bytes. A reflow splits one `TJ` into `TJ` + `Td` + `TJ`, so it changes the
   * operator count and has no `operandPath` to address. Both streams are encoded with the same
   * encoder `write_level` writes; `oldBytes`/`newBytes` are empty on such a leg.
   *
   * Absent for every byte-level edit, which is the degenerate one-element case.
   */
  ops?: OpVector | null;
}

/** The two encoded operation vectors a reflow's inverse records (ruling A). */
export interface OpVector {
  oldStream: number[];
  newStream: number[];
}

/** One Form XObject copy an edit made (P2.7.3 §7). */
export interface XObjectClone {
  /** The enclosing level: empty/absent for the page, else the path to the container. */
  parentPath?: XObjectStep[];
  /** The `Do` the edit repointed, and the names before and after. */
  doIndex: number;
  oldName: number[];
  newName: number[];
  /** The clone object, removed on undo so no orphan accumulates. */
  cloneObject: [number, number];
  /** The private `/Resources` installed on the clone, if the edit made one. */
  clonedResources?: [number, number] | null;
}

export interface TextEditResult {
  /** The new file the edit was written to. Never the original (D2). */
  path: string;
  inverse: ContentOperation;
  text: string;
  /** The standard-14 `/BaseFont` MECH-A substituted, when it fired (T10). */
  substitution?: string | null;
  /** The disclosure sentence shown when MECH-A fired (T10). `null`/absent otherwise. */
  note?: string | null;
  /**
   * False when the run sat under an `/ActualText` the edit could not update: the page shows
   * the new text but the document's accessible text still states the old one.
   */
  actualTextPreserved?: boolean;
}

export interface DecryptOutcome {
  path: string;
  pages: number;
}

/** Error code a protected document returns from every edit command (P2.4 T7). */
export const ENCRYPTED_READ_ONLY = "ENCRYPTED_READ_ONLY";
/** Error code when a supplied password does not unlock the document (P2.4 T8). */
export const WRONG_PASSWORD = "WRONG_PASSWORD";

/**
 * One certificate from the Windows store, as the picker lists it (P3.2 D-SG5).
 *
 * `canSign` is false when the store has no private key for it or the validity window has closed.
 * Such a certificate is listed rather than filtered: an absent row is indistinguishable from "not
 * installed", which sends a user off to import a certificate they already have.
 */
export interface SigningCertificate {
  derHex: string;
  subject: string;
  issuer: string;
  serialHex: string;
  keyAlgorithm: string;
  /** RFC 3339, or a sentence saying the clock is unusable. Never a fabricated date. */
  notBefore: string;
  notAfter: string;
  canSign: boolean;
  expired: boolean;
}

/** A certificate the store held but that could not be described, with the reason kept. */
export interface SkippedCertificate {
  derPrefixHex: string;
  reason: string;
}

/** What `list_signing_certificates` returns. */
export interface SigningCertificates {
  certificates: SigningCertificate[];
  /** Counted and shown so a short list can be told apart from a broken store. */
  skipped: SkippedCertificate[];
}

/**
 * The result of signing: where the signed copy went, and what was signed.
 *
 * `contentDigest` is lower-case hex, not a byte array -- the backend serialises it that way so a
 * person can compare it with a value someone else reported.
 */
export interface SignOutcome {
  /** The sibling copy that was written. The original is never overwritten. */
  path: string;
  /** The four integers written into the file's `/ByteRange`. */
  byteRange: [number, number, number, number];
  /** SHA-256 of the bytes the signature covers, as 64 hex characters. */
  contentDigest: string;
}

/** What the Sign dialog collected, beyond the identity (P3.2, D-SG6). */
export interface SignRequest {
  documentId: string;
  pfxPath: string;
  pfxPassword: string;
  /**
   * A DocMDP certification level, 1-3. Omitted for an approval signature.
   *
   * Absence is not 0 and not "no protection": it is the deliberate default, because an implicit
   * `P=1` would mark every later edit to the user's own file as tampering.
   */
  certificationLevel?: number;
  reason?: string;
  location?: string;
}

/**
 * What an import actually stored (P3.2 D-SG4, `import_trust_anchor` / `import_trust_crl`).
 *
 * `added` is separate from success on purpose. Importing a certificate the store already holds is
 * ordinary -- a user re-imports a CA when they are not sure it took -- and it must not read as
 * either a failure or a second trust decision. The UI says "already trusted" when this is false.
 */
/** One imported trust anchor, as the management panel lists it (P3.2 D-SG4). */
export interface TrustAnchorView {
  /** Base64 DER: the row's identity, and what `forgetTrustAnchor` takes. */
  derBase64: string;
  /** Distinguished name, so the user can recognise which issuer this is. */
  subject: string;
  /** Unix seconds. Past this the anchor is expired and stops being evidence. */
  notAfter: number;
  /** Unix seconds, when it was imported. */
  addedAt: number;
}

/** One imported revocation list, as the management panel lists it (P3.2 D-SG4). */
export interface TrustCrlView {
  /** Base64 DER: the row's identity, and what `forgetTrustCrl` takes. */
  derBase64: string;
  /** The issuer distinguished name the CRL states -- how two lists are told apart. */
  issuer: string;
  /** The CRL's `thisUpdate`, in unix seconds. */
  thisUpdate: number;
  /**
   * Its `nextUpdate`, in unix seconds, or null when it states none.
   *
   * Not decoration: past this date the list stops being evidence, so the panel must show it rather
   * than let a stale list look authoritative.
   */
  nextUpdate: number | null;
}

/**
 * Every `/P` bit the user can choose, one field each (P3.2 D-PR3).
 *
 * A struct of booleans rather than a bitflags value because the dialog must show each bit's state.
 * lopdf's `Permissions::default()` is `all()`, so any representation that can be "partly set" starts
 * by granting everything -- which is how a document ends up looking restricted while granting it all.
 */
export interface PermissionsChoice {
  print: boolean;
  modify: boolean;
  copy: boolean;
  annotate: boolean;
  fillForms: boolean;
  assemble: boolean;
  printHighQuality: boolean;
}

/** What a successful protect produced (P3.2 D-PR4). */
export interface ProtectOutcome {
  /** The sibling copy's full path -- `"{stem} (protected).pdf"`, never the original. */
  path: string;
  /** Page count of the written file, as read back during verification. */
  pages: number;
}

export interface TrustImport {
  /** Distinguished name of the certificate or the CRL's issuer. */
  subject: string;
  /** Base64 DER, the row's identity: lets the UI show *which* certificate is trusted. */
  derBase64: string;
  /** False when the store already held it. */
  added: boolean;
  /**
   * Unix seconds. The anchor's own expiry, or the CRL's `nextUpdate` when it states one.
   *
   * Null only for a CRL that states no `nextUpdate`, which by its own terms never expires.
   */
  notAfter: number | null;
}

/**
 * The signature badge, as `signature_badge` reports it (P3.2 SCOPE-C, D-SG7).
 *
 * `badge` is one of exactly four states. `isGreen` is separate from the string on purpose: the
 * component colours on the boolean, so adding a fifth state to the union cannot accidentally render
 * green. The backend computes it from evidence; the UI never decides.
 */
export interface SignatureBadge {
  badge: "valid" | "modified" | "unknown" | "unsigned";
  /** One line per thing checked or not checked, already worded by the backend. */
  lines: string[];
  /**
   * Why the badge is not better than it is, or absent/empty when there is nothing to add.
   *
   * Separate from `lines` because its content depends on this document: no anchor imported, a
   * chain that reaches none, a stale list. `lines` is fixed wording the backend can vouch for; this
   * names a runtime condition. The UI shows it after the lines, never instead of them.
   *
   * Optional as well as possibly empty: a payload from a build before this field existed must still
   * render, and treating a missing field as a runtime error would make an older backend look broken.
   */
  reason?: string;
  /** True only for `valid`. The single source of colour. */
  isGreen: boolean;
  /** The declared DocMDP level (1..3) when this is a certification signature. */
  certificationLevel: number | null;
}

/**
 * A stable string for a run address, for React keys and identity checks (P2.7.3 X1).
 *
 * The `opIndex`/`operandPath` pair alone is no longer unique: a page run at op 3 and a run at
 * op 3 inside a form are different runs. The XObject names are part of the key, in order, so a
 * nested run and a page run can never collide in a list, a selection, or a `key` prop.
 */
export function textRunIdKey(runId: TextRunId): string {
  const path = (runId.xobjectPath ?? []).map((step) => step.name.join(".")).join("/");
  return `${path}:${runId.opIndex}:${runId.operandPath.join(".")}`;
}

/** Whether two addresses name the same operand (P2.7.3 X1: including the XObject path). */
export function sameTextRunId(a: TextRunId, b: TextRunId): boolean {
  return textRunIdKey(a) === textRunIdKey(b);
}

/** How a redaction mark came to exist (P3.2 D-RD1/D-RD6). */
export type RedactionKind = "rect" | "pattern";

/**
 * One pending redaction mark (P3.2 D-RD1).
 *
 * Working state, not document content: it lives in the local database and is cleared when apply
 * writes its output. `rect` is the stored `x0,y0,x1,y1` string in PDF points, corner-ordered by the
 * backend — parse it with {@link parseRedactionRect} rather than splitting it in a component, so the
 * one place that knows the format is also the one place a change has to be made.
 */
export interface RedactionMark {
  id: string;
  documentId: string;
  page: number;
  kind: RedactionKind;
  /** `x0,y0,x1,y1`, page points, origin bottom-left. */
  rect: string;
  /** The searched text, for `pattern` marks only. Display data; never re-run at apply time. */
  pattern: string | null;
  createdAt: number;
}

/**
 * Parses a stored mark rectangle into page points.
 *
 * Returns `null` rather than a zero rectangle for anything malformed. A zero rect would render as an
 * invisible mark and, worse, would apply as one that removes nothing while the UI reported success.
 * The backend refuses to store such a value; this is the frontend holding the same line, so a corrupt
 * row cannot turn into a silent no-op in the interface.
 */
export function parseRedactionRect(
  rect: string,
): { x0: number; y0: number; x1: number; y1: number } | null {
  const parts = rect.split(",");
  if (parts.length !== 4) return null;
  // An empty component is rejected explicitly. `Number("")` is `0`, and `0` is finite, so a blank
  // field would otherwise parse as a real coordinate: "1,,2,3" would become a rectangle running from
  // y=0, which is a mark the user never drew -- and one that removes content they meant to keep.
  if (parts.some((part) => part.trim() === "")) return null;
  const [x0, y0, x1, y1] = parts.map((part) => Number(part.trim()));
  if (![x0, y0, x1, y1].every((value) => Number.isFinite(value))) return null;
  return { x0, y0, x1, y1 };
}
