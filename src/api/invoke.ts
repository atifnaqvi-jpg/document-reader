import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import type {
  Annotation,
  AnnotationChanged,
  AnnotationKind,
  AnnotationStatus,
  AppError,
  Bookmark,
  ContentOperation,
  DecryptOutcome,
  DocumentMeta,
  IndexStatus,
  IndexSummary,
  LibraryEntry,
  PageTextRun,
  RedactionMark,
  RenderedPage,
  SearchHit,
  SignatureBadge,
  SigningCertificates,
  SignOutcome,
  SignRequest,
  TextEditResult,
  TrustImport,
  TextRunFragmentRef,
  TextRunId,
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
 * The one-time data-directory migration notice, or `null` when nothing moved (D-R6).
 *
 * Read on demand rather than pushed, so the notice survives a remount and stays re-readable.
 */
export const getMigrationNotice = () =>
  invoke<string | null>("get_migration_notice");

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

/**
 * Sets or clears one annotation's review status (D-A7).
 *
 * Its own command rather than `updateAnnotation` so the status travels alone: the backend
 * rewrites only the blob's `status` key, and a stale geometry held by the pane cannot ride
 * along and overwrite a newer one.
 */
export const setAnnotationStatus = (
  annotationId: string,
  status: AnnotationStatus | null,
) => invoke<Annotation>("set_annotation_status", { annotationId, status });

export const listAnnotations = (documentId: string) =>
  invoke<Annotation[]>("list_annotations", { documentId });

/** What an XFDF export produced. `unsupported` counts rows that had no XFDF spelling. */
export type XfdfExport = {
  contents: string;
  exported: number;
  unsupported: number;
};

/** What an XFDF import did. `skippedExisting` is D-A10's never-overwrite count. */
export type XfdfImportReport = {
  imported: number;
  skippedExisting: number;
  skippedUnsupported: number;
  skippedMalformed: number;
};

export const exportXfdf = (documentId: string) =>
  invoke<XfdfExport>("export_xfdf", { documentId });

export const importXfdf = (documentId: string, contents: string) =>
  invoke<XfdfImportReport>("import_xfdf", { documentId, contents });

/** Writes an export to the path the user chose in the save dialog. */
export const saveXfdf = (contents: string, path: string) =>
  invoke<null>("save_xfdf", { contents, path });

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
 * Overwrites the original file in place with the document's annotations (P2-D2).
 *
 * A separate binding from `saveEdits` on purpose: the backend exposes it as a differently
 * named command so no path meaning "save" can reach it. Callers must confirm with the user
 * first, since this is the only action that can destroy the source document.
 */
export const overwriteEdits = (documentId: string) =>
  invoke<string>("overwrite_edits", { documentId });

/**
 * Re-inserts a deleted annotation under its original id (P2-D7). Undo needs the whole
 * row back — a fresh id would break anything already holding the old one.
 */
export const restoreAnnotation = (annotation: Annotation) =>
  invoke<void>("restore_annotation", { annotation });

// --- P2: page structure, export, watermark ------------------------------------

/**
 * Rotates a page clockwise and returns its resulting rotation (P2-F3).
 *
 * Returns the new value rather than echoing `degrees` because `/Rotate` accumulates: two
 * 90-degree turns on one page give 180.
 */
export const rotatePage = (documentId: string, page: number, degrees: number) =>
  invoke<RotateResult>("rotate_page", { documentId, page, degrees });

export interface RotateResult {
  /** Where the rotated copy was written; never the original. */
  path: string;
  /** The page's resulting angle, which accumulates across rotations. */
  rotation: number;
}

/** Deletes pages, numbered from 1 as the UI shows them (P2-F3). */
export const deletePages = (documentId: string, pages: number[]) =>
  invoke<string>("delete_pages", { documentId, pages });

/** Moves a page to a new 1-based position (P2-F3 reorder). */
export const movePage = (documentId: string, from: number, to: number) =>
  invoke<string>("move_page", { documentId, from, to });

/** Writes just the selected pages to a new file (P2-F3 extract). */
export const extractPages = (documentId: string, pages: number[]) =>
  invoke<string>("extract_pages", { documentId, pages });

/** Splits the document in two at a 1-based page number, returning both paths (P2-F3). */
export const splitDocument = (documentId: string, at: number) =>
  invoke<string[]>("split_document", { documentId, at });

/**
 * Flattens annotations into a new file, optionally watermarking it (P2-F2/F5).
 *
 * One-way: the result has the marks baked in and no annotation objects, which is the point
 * — it renders correctly in viewers that ignore annotations.
 */
export const exportFlattened = (documentId: string, watermark?: string) =>
  invoke<string>("export_flattened", { documentId, watermark: watermark ?? null });

/** Stamps a watermark across every page, into a new file (P2-F5). */
export const addWatermark = (documentId: string, text: string) =>
  invoke<string>("add_watermark", { documentId, text });

/** Renders one page to a base64 PNG for the clipboard (P2-F2). */
export const exportPagePng = (documentId: string, page: number, scale = 1) =>
  invoke<string>("export_page_png", { documentId, page, scale });

// --- P2.4: content-stream text editing -----------------------------------------

/**
 * The text runs on one page, with the Stage-1 verdict already computed (P2.4 §4).
 *
 * The backend pairs lopdf's run addresses with PDFium's geometry, so the caller can grey
 * out runs the write path would refuse *before* the user types. A run whose
 * `admissibility.editable` is false must not be offered as an edit target.
 */
export const getPageTextRuns = (documentId: string, page: number) =>
  invoke<PageTextRun[]>("get_page_text_runs", { documentId, page });

/**
 * Applies one in-place text edit, writing a **new** file (P2.4 T2).
 *
 * `expected` is the text the user was shown; the backend re-validates it against the
 * content stream and refuses with `TEXT_RUN_MISMATCH` if it changed. `oldWidth` is the
 * run's measured advance width, which the width gate compares the replacement against.
 */
export const editPageText = (
  documentId: string,
  page: number,
  runId: TextRunId,
  /**
   * The head **operand's** bytes, not the run's display text (P2.6 T26 item 4). For a merged
   * run the display text joins its fragments and may show a kern seam as a word space the
   * bytes do not contain; the backend re-validates this against the content stream.
   */
  expected: string,
  replacement: string,
  oldWidth: number,
  fontName: string,
  fontSize: number,
  allowSubstitution: boolean,
  /**
   * The remaining fragments of a merged run, head first after the head (P2.4.1 T18). The
   * backend writes the replacement into the head, clears each of these, and records all of
   * them in ONE inverse, so a merged edit is one atomic pass and one undo. Empty for an
   * ordinary run; each entry is re-validated against the document before anything is written.
   */
  fragments: TextRunFragmentRef[],
  /**
   * Further whole runs selected alongside the head, in reading order (P2.6 T26). The backend
   * replaces the head and clears these in the same atomic write, re-validating that together
   * they are one contiguous, same-style span. Empty for a single-run pick.
   */
  range: TextRunFragmentRef[] = [],
) =>
  invoke<TextEditResult>("edit_page_text", {
    documentId,
    page,
    runId,
    expected,
    replacement,
    oldWidth,
    fontName,
    fontSize,
    // O-ST2-4: the toggle travels explicitly so the backend never guesses the user's
    // intent; the UI defaults it on.
    allowSubstitution,
    fragments,
    range,
  });

/**
 * Reverses a text edit from its recorded inverse, writing another new file (P2.4 T5).
 *
 * `editedPath` is the file the edit wrote, because that is where the bytes the inverse
 * describes currently live. Returns the path of the reverted copy.
 */
export const revertPageText = (
  documentId: string,
  editedPath: string,
  operation: ContentOperation,
) =>
  invoke<string>("revert_page_text", { documentId, editedPath, operation });

/**
 * Decrypts a protected document to a new, unprotected copy (P2.4 T8).
 *
 * The original is never modified. The password is used for this call and kept in memory
 * for the session only (T10) — never on disk, in SQLite, or in an event payload.
 */
export const decryptCopy = (documentId: string, password: string) =>
  invoke<DecryptOutcome>("decrypt_copy", { documentId, password });

/**
 * Lists the signing certificates installed for the current user (P3.2 D-SG5).
 *
 * Rejects on a platform with no Windows certificate store, rather than resolving to an empty list.
 * An empty list would render as "you have no signing certificates", which is a false statement about
 * the user's machine.
 */
export const listSigningCertificates = () =>
  invoke<SigningCertificates>("list_signing_certificates");

/**
 * Signs a document's saved file with a PKCS#12 identity (P3.2 SCOPE-C).
 *
 * The password travels with the request and is not remembered by the app: unlike a document
 * password, it unlocks a private key, so keeping it would let a later action sign as the user
 * without asking. The call writes a sibling `"{stem} (signed).pdf"` and never the original.
 *
 * `sign_document` refuses an encrypted source and an ambiguous PKCS#12 container rather than
 * guessing, so those arrive here as rejected promises with a sentence meant for the user.
 */
export const signDocument = (request: SignRequest) =>
  invoke<SignOutcome>("sign_document", {
    documentId: request.documentId,
    pfxPath: request.pfxPath,
    pfxPassword: request.pfxPassword,
    certificationLevel: request.certificationLevel ?? null,
    reason: request.reason ?? null,
    location: request.location ?? null,
  });

/**
 * Reports the signature badge for a document's saved file (P3.2 SCOPE-C, D-SG7).
 *
 * Reads the file on disk, not the editor buffer: the badge answers what the signature attests to,
 * and unsaved edits are not in the file. A document with no signature returns `unsigned`, not an
 * error -- "there is no signature" is an answer, and an error would render as a fault.
 */
export const getSignatureBadge = (documentId: string) =>
  invoke<SignatureBadge>("signature_badge", { documentId });

/**
 * Imports one trust anchor, so signatures from that CA can reach `valid` (P3.2 D-SG4).
 *
 * This is the write side of the local-only trust store: without it the `valid` state is
 * unreachable on any file, because `anchored` requires a path to an anchor the user imported.
 * The path is a file the user chose; nothing is fetched (D-SG3). DER and PEM are both accepted.
 *
 * The call resolves with what was stored, so the caller can say "trusted" or "already trusted"
 * rather than guessing from the absence of an error.
 */
export const importTrustAnchor = (derPath: string) =>
  invoke<TrustImport>("import_trust_anchor", { derPath });

/**
 * Imports one certificate revocation list (P3.2 D-SG4).
 *
 * Separate from the anchor because it answers a different question: the anchor says which issuer
 * is checked, the CRL says which of its certificates are revoked. A signature needs both for
 * `valid`. The CRL's own `nextUpdate` decides when it stops being evidence; nothing is fetched.
 */
export const importTrustCrl = (derPath: string) =>
  invoke<TrustImport>("import_trust_crl", { derPath });

/** Drops a document's remembered password when it closes (P2.4 T10). */
export const forgetPassword = (path: string) =>
  invoke<void>("forget_password", { path });

// --- P3.2 SCOPE-B: redaction -------------------------------------------------

/**
 * Records one dragged rectangle as a pending redaction mark (P3.2 D-RD1).
 *
 * Writes no file: a mark is working state that lives in the local database, so the document on disk
 * still shows the text until the user applies. `rect` is in PDF page points with the origin at the
 * bottom-left, which is what the overlay's `pointToPdf` produces; the backend re-orders the corners,
 * so a rectangle built from a drag in either direction is safe to send.
 *
 * Refused on a certified document: applying a mark rewrites the content stream, and no certification
 * level permits that (`EditKind::Redaction`). The refusal comes back as an error rather than a silent
 * no-op, so the UI can say why before the user draws more rectangles.
 */
export const redactMark = (
  documentId: string,
  page: number,
  rect: { x0: number; y0: number; x1: number; y1: number },
) => invoke<RedactionMark>("redact_mark", { documentId, page, rect });

/** The document's pending marks, from the database rather than from UI state (P3.2 D-RD1). */
export const redactMarks = (documentId: string) =>
  invoke<RedactionMark[]>("redact_marks", { documentId });

/** Removes one pending mark. Not gated: a mark is our own state and changes no document byte. */
export const redactUnmark = (markId: string) =>
  invoke<boolean>("redact_unmark", { markId });

/**
 * Finds `pattern` and records a mark per hit (P3.2 D-RD6).
 *
 * An empty array means the text genuinely does not occur — that is an answer, not a failure, and the
 * UI says so rather than rendering an error. Throws when a page cannot be searched at all, because
 * reporting a search that silently skipped part of the document would be the under-coverage D-RD3
 * forbids.
 */
export const redactSearch = (documentId: string, pattern: string) =>
  invoke<RedactionMark[]>("redact_search", { documentId, pattern });

/**
 * Applies every pending mark and writes a NEW file (P3.2 D-RD1/D-RD2).
 *
 * The source is never modified: `outcome.path` is a derived sibling, exactly as the page tools
 * behave. Marks are cleared only once the write succeeded, so a failed apply leaves them for a retry.
 *
 * Rejects when there is nothing to remove (I-RD6) — a second apply at the same marks, or marks over
 * content already gone. That is reported rather than producing a second byte-identical file the user
 * cannot tell apart from the redacted one.
 */
export const redactApply = (documentId: string) =>
  invoke<RedactOutcome>("redact_apply", { documentId });

/** What {@link redactApply} did, for the report the user reads afterwards. */
export interface RedactOutcome {
  /** Where the redacted copy was written; never the original. */
  path: string;
  /** Text-showing operations deleted, across every page and every form. */
  runsRemoved: number;
  /** Annotations removed because their box intersected a mark. */
  annotationsRemoved: number;
  /** Images removed whole because they intersected a mark. */
  imagesRemoved: number;
  /** Objects dropped by the reachability sweep afterwards. */
  objectsDropped: number;
  /** Marks cleared from the database. */
  marksCleared: number;
}
