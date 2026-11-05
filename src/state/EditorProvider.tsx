import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import type { ReactNode } from "react";
import {
  addAnnotation,
  decryptCopy,
  editPageText,
  getAppState,
  getPageTextRuns,
  listAnnotations,
  onAnnotationChanged,
  removeAnnotation,
  restoreAnnotation,
  revertPageText,
  setAnnotationStatus,
  setAppState,
  updateAnnotation,
} from "../api/invoke";
import type {
  Annotation,
  AnnotationData,
  AnnotationKind,
  AnnotationStatus,
  AppError,
  CommonData,
  NoteData,
  DecryptOutcome,
  PageTextRun,
  TextRunFragmentRef,
  TextRunId,
  ToolStyle,
} from "../api/types";
import { ANNOTATION_KINDS, commonOf, sameTextRunId } from "../api/types";

/**
 * Editing state (docs/P2-DESIGN.md D7/D10).
 *
 * Undo/redo is **session-scoped and per document** (D7): the stack lives here and is
 * never persisted, so a restart starts clean rather than re-hydrating a large stack
 * whose entries may reference annotations the user has since deleted elsewhere.
 *
 * The DB is the source of truth for what annotations exist; the stack only records how
 * to get back to a previous state. Every entry is applied by calling the same commands
 * the tools use, so an undo cannot produce a row the normal path could not.
 */

/** One reversible change. `redo` re-creates what `undo` removed. */
interface StackEntry {
  label: string;
  undo: () => Promise<void>;
  redo: () => Promise<void>;
}

/**
 * Cap on stack depth (D7). Entries close over their own data, so an unbounded stack on a
 * long session is unbounded memory; dropping the oldest is the stated policy.
 */
const MAX_STACK = 100;

/**
 * Where a reply's note icon sits on the page (D-A5).
 *
 * A reply is a note, so it needs a point in PDF space even though the comment pane is where
 * it is read. The top-left corner of the page is inside the media box for every page size and
 * is out of the way of the text a reply is usually about; the icon is deliberately not meant
 * to be found by eye, and the pane is the surface that lists it.
 */
const REPLY_POINT: [number, number] = [8, 8];

interface EditorContextValue {
  /** Annotations for the open page, in draw order. */
  annotations: Annotation[];
  /**
   * Every annotation in the document, in page order.
   *
   * The comment manager is document-wide (D-A11) while the overlay draws one page. Both read
   * the same `refresh()` result; the pane filters, the overlay scopes. A second query would
   * let the two disagree about the same rows.
   */
  allAnnotations: Annotation[];
  selectedId: string | null;
  activeTool: AnnotationKind | null;
  style: ToolStyle;
  error: AppError | null;
  canUndo: boolean;
  canRedo: boolean;
  setActiveTool: (tool: AnnotationKind | null) => void;
  setStyle: (style: ToolStyle) => void;
  setSelectedId: (id: string | null) => void;
  /** Replaces the whole tool style; persisted per tool (D10). */
  create: (
    page: number,
    kind: AnnotationKind,
    data: AnnotationData,
    text: string | null,
  ) => Promise<void>;
  /** Deletes one annotation, recording it for undo. */
  remove: (annotation: Annotation) => Promise<void>;
  /** Replaces one annotation's text, recording it for undo. */
  updateText: (annotation: Annotation, text: string) => Promise<void>;
  /**
   * Sets or clears one annotation's review status (D-A7), recording it for undo.
   *
   * A status change is an annotation *update*, so it pushes exactly one entry (D-A6):
   * triage decisions must be reversible like any other edit, or a misclick silently loses
   * one.
   */
  setStatus: (annotation: Annotation, status: AnnotationStatus | null) => Promise<void>;
  /**
   * Adds a reply to `parent` (D-A5).
   *
   * A reply is a `note` whose blob names its parent -- the file format's own mechanism
   * (`/IRT`), rather than a separate table with its own index and undo machinery.
   */
  reply: (parent: Annotation, text: string) => Promise<void>;
  /**
   * Applies an in-place content-stream text edit (P2.4/T2) and records the inverse for
   * undo (T5). Returns the path of the new file the edit was written to, or `null` when
   * the backend refused (the refusal is on `error`).
   */
  /**
   * Replaces `run` (and the rest of the current selection, if any) in one write (P2.6 T26).
   * The range comes from `selection`, which this provider owns, so a caller cannot drop it.
   */
  editText: (page: number, run: PageTextRun, replacement: string) => Promise<string | null>;
  /**
   * The substitution disclosure from the most recent successful edit (T10), or `null`.
   * Set when MECH-A switched the run's font; the panel surfaces it as a toast so a
   * changed typeface is never silent.
   */
  substitution: SubstitutionNote | null;
  /** Clears the substitution toast after the user has acknowledged it. */
  clearSubstitution: () => void;
  /**
   * The accessibility notice from the most recent edit, or `null` (P2.7.1 T30).
   *
   * Set when the edited run sat under an `/ActualText` the write could not update: the page
   * reads the new text while the document's accessible text still states the old one.
   */
  actualTextNotice: string | null;
  /** Clears the accessibility notice after the user has acknowledged it. */
  clearActualTextNotice: () => void;
  /**
   * The O-ST2-4 toggle. When off, a run whose font cannot set the replacement refuses
   * (`SUBSTITUTE_REFUSED`) instead of being switched to a standard-14 font. Defaults on.
   */
  allowSubstitution: boolean;
  setAllowSubstitution: (on: boolean) => void;
  /** Decrypts a protected document to a new copy (P2.4/T8). */
  decryptCopyToFile: (password: string) => Promise<DecryptOutcome | null>;
  /**
   * The text runs on the current page, with their Stage-1 verdicts (P2.4 §4). Fetched
   * once per (document, page) while text-edit mode is on.
   */
  pageRuns: PageTextRun[];
  runsLoading: boolean;
  /** Whether the in-viewer text picker is active. */
  textEditMode: boolean;
  setTextEditMode: (on: boolean) => void;
  /** The run the user picked, by reference identity within `pageRuns`. */
  selectedRun: PageTextRun | null;
  /**
   * The whole selection, in reading order, head first (P2.6 T26). A selection is a range of
   * runs, not one run: a shift-click extends it, and replacing it replaces all of them.
   * `[selectedRun]` for an ordinary pick, empty for none.
   */
  selection: PageTextRun[];
  /**
   * Select `run`; with `extend`, extend the current selection from its head to `run`.
   * Extending past a gap the backend would refuse is still allowed here — the refusal is
   * explained by the panel rather than silently swallowed by a picker that appears dead.
   */
  selectRun: (run: PageTextRun | null, extend?: boolean) => void;
  undo: () => Promise<void>;
  redo: () => Promise<void>;
  clearError: () => void;
}

const EditorContext = createContext<EditorContextValue | null>(null);

/**
 * The disclosure produced when MECH-A swapped a run's font (T10). Held as data rather
 * than only a string so the panel can render the substitute and the original distinctly.
 */
export interface SubstitutionNote {
  /** The standard-14 `/BaseFont` the run was switched to. */
  baseFont: string;
  /** The backend's disclosure sentence, shown verbatim. */
  message: string;
}

/** Style key per tool (D10), matching the `annotation.style.<tool>` scheme. */
export const styleKey = (tool: AnnotationKind) => `annotation.style.${tool}`;

/**
 * Defaults for every tool the enum names (D-A4). Total by type, so adding a kind without a
 * default is a compile error rather than an `undefined` reaching the style picker.
 */
const DEFAULT_STYLES: Record<AnnotationKind, ToolStyle> = {
  highlight: { color: "#fff3b0", opacity: 0.4 },
  underline: { color: "#1f6feb", opacity: 1 },
  strikeout: { color: "#d1242f", opacity: 1 },
  squiggly: { color: "#d1242f", opacity: 1 },
  note: { color: "#f5a623", opacity: 1 },
  textbox: { color: "#111418", opacity: 1 },
  callout: { color: "#111418", opacity: 1 },
  typewriter: { color: "#111418", opacity: 1 },
  ink: { color: "#111418", opacity: 1 },
  line: { color: "#d1242f", opacity: 1 },
  arrow: { color: "#d1242f", opacity: 1 },
  rect: { color: "#1f6feb", opacity: 1 },
  oval: { color: "#1f6feb", opacity: 1 },
};

/**
 * The stored style key for a tool. `rect` reads the key its predecessor wrote (D-A4's
 * one-way `shape` alias), so a colour picked before the rename is still picked after it
 * rather than silently reverting to the default.
 */
const styleKeysFor = (tool: AnnotationKind): string[] =>
  tool === "rect" ? [styleKey(tool), "annotation.style.shape"] : [styleKey(tool)];

/** Reads a persisted style, falling back to the built-in default for that tool. */
function parseStyle(raw: string | null, tool: AnnotationKind): ToolStyle {
  const fallback = DEFAULT_STYLES[tool];
  if (!raw) return fallback;
  try {
    const parsed = JSON.parse(raw) as Partial<ToolStyle>;
    if (typeof parsed.color !== "string") return fallback;
    return {
      color: parsed.color,
      opacity: typeof parsed.opacity === "number" ? parsed.opacity : fallback.opacity,
    };
  } catch {
    return fallback;
  }
}

/** Whether two run addresses are the same (P2.6 T26). Kept local so the provider does not
 * import from a component; `sameRun` below is the run-shaped view of it. */
function sameRunId(a: TextRunId, b: TextRunId): boolean {
  return sameTextRunId(a, b);
}

function sameAddress(a: PageTextRun, b: PageTextRun): boolean {
  return sameRunId(a.runId, b.runId);
}

export function EditorProvider({
  documentId,
  page,
  selectRequest,
  children,
}: {
  documentId: string | null;
  page: number;
  /**
   * A jump from outside the provider -- a search hit naming an annotation (D-A12). The
   * `nonce` makes the same id selectable twice: clicking the same hit again must move the
   * selection back to it after the user has selected something else, and a bare id would
   * not change state the second time.
   */
  selectRequest?: { id: string; nonce: number } | null;
  children: ReactNode;
}) {
  const [annotations, setAnnotations] = useState<Annotation[]>([]);
  const [activeTool, setActiveTool] = useState<AnnotationKind | null>(null);
  const [style, setStyleState] = useState<ToolStyle>(DEFAULT_STYLES.highlight);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [error, setError] = useState<AppError | null>(null);
  const [stacks, setStacks] = useState<{ undo: StackEntry[]; redo: StackEntry[] }>({
    undo: [],
    redo: [],
  });
  // P2.4: the in-viewer text picker. `pageRuns` is the current page's run list with its
  // Stage-1 verdicts; it is fetched rather than derived, because the run addresses come
  // from lopdf and the geometry from PDFium.
  const [pageRuns, setPageRuns] = useState<PageTextRun[]>([]);
  const [runsLoading, setRunsLoading] = useState(false);
  const [textEditMode, setTextEditMode] = useState(false);
  const [selectedRun, setSelectedRun] = useState<PageTextRun | null>(null);
  const [selection, setSelection] = useState<PageTextRun[]>([]);
  // Stage 2: the O-ST2-4 toggle (default on) and the T10 disclosure from the last edit.
  const [allowSubstitution, setAllowSubstitution] = useState(true);
  const [substitution, setSubstitution] = useState<SubstitutionNote | null>(null);
  const [actualTextNotice, setActualTextNotice] = useState<string | null>(null);

  // Per-document stacks (D7). Kept in one map keyed by document rather than one stack
  // reset on switch, so moving between documents does not destroy either one's history.
  // The map is only written by `push`/`undo`/`redo` (which mirror into `stacks`), never
  // during render: a render-time write would store the pre-update value and lose an entry.
  const stacksRef = useRef(new Map<string, { undo: StackEntry[]; redo: StackEntry[] }>());
  const docKey = documentId ?? "";
  const stacksForDoc = stacksRef.current.get(docKey);

  /** Reads annotations for a page from the DB -- the source of truth, not local edits. */
  const refresh = useCallback(async () => {
    if (!documentId) {
      setAnnotations([]);
      return;
    }
    try {
      const rows = await listAnnotations(documentId);
      setAnnotations(rows);
    } catch (e) {
      setError(e as AppError);
    }
  }, [documentId]);

  // Switching documents shows that document's own history, not the previous one's.
  useEffect(() => {
    setSelectedId(null);
    setStacks(stacksRef.current.get(docKey) ?? { undo: [], redo: [] });
    void refresh();
  }, [docKey, refresh]);

  // A search hit naming an annotation selects it (P3.1 D-A12). Declared after the
  // document-change effect above so it wins: opening a hit for a *different* document sets
  // the selection in the same pass, and the reset to null would otherwise land last and
  // clear the very row the user clicked.
  useEffect(() => {
    if (!selectRequest) return;
    setSelectedId(selectRequest.id);
  }, [selectRequest]);

  // Leaving text-edit mode, switching page or document drops the run selection: the run
  // addresses belong to one page of one document, and carrying one across would offer an
  // edit whose target no longer exists.
  useEffect(() => {
    setSelectedRun(null);
  }, [docKey, page, textEditMode]);

  // Text runs for the current page (P2.4 §4). Fetched only while the picker is on: the
  // extraction walks the content stream and measures every run against PDFium, which is
  // real work the reader should not pay for while merely reading.
  //
  // A refused run list is not an error banner: the picker is still useful (it shows what
  // is there and why it cannot be edited), so the failure clears the list instead of
  // raising. The backend's refusal messages reach the user through the edit path.
  useEffect(() => {
    if (!textEditMode || !documentId) {
      setPageRuns([]);
      setRunsLoading(false);
      return;
    }
    let cancelled = false;
    setRunsLoading(true);
    setError(null);
    getPageTextRuns(documentId, page)
      .then((runs) => {
        if (!cancelled) setPageRuns(runs);
      })
      .catch((e: unknown) => {
        if (!cancelled) {
          setPageRuns([]);
          setError(e as AppError);
        }
      })
      .finally(() => {
        if (!cancelled) setRunsLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [textEditMode, documentId, page]);

  // Cross-surface sync (P2-D11). An annotation changed somewhere else -- an undo restored
  // by another pane, a future multi-window edit -- and this pane re-reads the DB rather
  // than trusting the event's payload, which only names the document.
  //
  // Skipped for other documents: the event is process-wide, so without this guard a change
  // in document B would make the open document A re-query for nothing.
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let cancelled = false;

    onAnnotationChanged((change) => {
      if (change.documentId === docKey) void refresh();
    })
      .then((fn) => {
        if (cancelled) fn();
        else unlisten = fn;
      })
      .catch(() => {
        // No event bridge (e.g. a test renderer) must not break editing.
      });

    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [docKey, refresh]);

  // `canUndo`/`canRedo` read the map so they are correct even on the render immediately
  // after a document switch, when `stacks` still holds the outgoing document's history.
  const canUndo = (stacksForDoc ?? stacks).undo.length > 0;
  const canRedo = (stacksForDoc ?? stacks).redo.length > 0;


  // The tool style is per tool and persisted (D10). Loading it when the tool changes
  // means a colour picked last session is already selected when that tool is picked.
  useEffect(() => {
    if (!activeTool) return;
    let cancelled = false;
    const [key, legacyKey] = styleKeysFor(activeTool);
    getAppState(key)
      .then((raw) => (raw === null && legacyKey ? getAppState(legacyKey) : raw))
      .then((raw) => {
        if (!cancelled) setStyleState(parseStyle(raw, activeTool));
      })
      .catch(() => {
        // A missing key is the normal first-run case and already yields the default;
        // a read failure is not worth interrupting the user over.
      });
    return () => {
      cancelled = true;
    };
  }, [activeTool]);

  const setStyle = useCallback(
    (next: ToolStyle) => {
      setStyleState(next);
      if (!activeTool) return;
      // Fire-and-forget: the style is a preference, and failing to save it must not
      // block the annotation the user is about to draw with it.
      setAppState(styleKey(activeTool), JSON.stringify(next)).catch(() => {});
    },
    [activeTool],
  );

  /** Pushes an entry, dropping the oldest at the cap (D7). Pushing clears the redo side. */
  const push = useCallback(
    (key: string, entry: StackEntry) => {
      const current = stacksRef.current.get(key) ?? { undo: [], redo: [] };
      const undo = [...current.undo, entry];
      // Drop from the front when over the cap: those are the oldest actions, and they
      // are the ones a user is least likely to want to walk back to.
      while (undo.length > MAX_STACK) undo.shift();
      const next = { undo, redo: [] };
      stacksRef.current.set(key, next);
      if (key === docKey) setStacks(next);
    },
    [docKey],
  );

  const create = useCallback(
    async (
      targetPage: number,
      kind: AnnotationKind,
      data: AnnotationData,
      text: string | null,
    ) => {
      if (!documentId) return;
      try {
        const created = await addAnnotation(
          documentId,
          targetPage,
          kind,
          JSON.stringify(data),
          text,
        );
        await refresh();
        // Re-creating after an undo mints a *new* id, so the entries must track the
        // current one rather than close over `created.id`: otherwise the second undo of
        // the same action would try to delete an id that no longer exists.
        let currentId = created.id;
        push(docKey, {
          label: `Add ${kind}`,
          undo: async () => {
            await removeAnnotation(currentId);
            await refresh();
          },
          redo: async () => {
            // Rebuilt from the captured blob, not the original gesture: the row is gone
            // after undo, so this entry is the only record of what to put back.
            const again = await addAnnotation(
              documentId,
              targetPage,
              kind,
              created.data,
              created.text,
            );
            currentId = again.id;
            await refresh();
          },
        });
      } catch (e) {
        setError(e as AppError);
      }
    },
    [documentId, docKey, refresh, push],
  );

  const remove = useCallback(
    async (annotation: Annotation) => {
      if (!documentId) return;
      try {
        await removeAnnotation(annotation.id);
        await refresh();
        push(docKey, {
          label: `Delete ${annotation.kind}`,
          undo: async () => {
            // Restored with its original id and timestamps, so anything referencing it
            // (a search hit, a selection) still resolves. `add_annotation` would mint a
            // new id and quietly break those references.
            await restoreAnnotation(annotation);
            await refresh();
          },
          redo: async () => {
            await removeAnnotation(annotation.id);
            await refresh();
          },
        });
      } catch (e) {
        setError(e as AppError);
      }
    },
    [documentId, docKey, refresh, push],
  );

  const updateText = useCallback(
    async (annotation: Annotation, text: string) => {
      try {
        await updateAnnotation(annotation.id, annotation.data, text);
        await refresh();
        // Undo restores the *previous* text rather than deleting the note, because editing
        // text is a change to something that still exists. The entry closes over the old
        // value, which is the only record of it once the update lands.
        const previous = annotation.text;
        push(docKey, {
          label: `Edit ${annotation.kind} text`,
          undo: async () => {
            await updateAnnotation(annotation.id, annotation.data, previous);
            await refresh();
          },
          redo: async () => {
            await updateAnnotation(annotation.id, annotation.data, text);
            await refresh();
          },
        });
      } catch (e) {
        setError(e as AppError);
      }
    },
    [docKey, refresh, push],
  );

  const setStatus = useCallback(
    async (annotation: Annotation, status: AnnotationStatus | null) => {
      try {
        const previous = commonOf(annotation).status ?? null;
        await setAnnotationStatus(annotation.id, status);
        await refresh();
        // One entry per gesture (D-A6). Undo restores the *previous* status, including
        // "none": clearing back to untriaged has to be reachable too.
        push(docKey, {
          label: `Status ${annotation.kind}`,
          undo: async () => {
            await setAnnotationStatus(annotation.id, previous);
            await refresh();
          },
          redo: async () => {
            await setAnnotationStatus(annotation.id, status);
            await refresh();
          },
        });
      } catch (e) {
        setError(e as AppError);
      }
    },
    [docKey, refresh, push],
  );

  const reply = useCallback(
    async (parent: Annotation, text: string) => {
      if (!documentId) return;
      try {
        // A reply is anchored on its parent's page: a reply that landed on page 0 because
        // nobody passed the page would appear in the pane and nowhere near what it answers.
        const page = parent.page;
        const data: NoteData & CommonData = {
          v: 1,
          point: REPLY_POINT,
          parent: parent.id,
        };
        const created = await addAnnotation(
          documentId,
          page,
          "note",
          JSON.stringify(data),
          text,
        );
        await refresh();
        let currentId = created.id;
        push(docKey, {
          label: `Reply to ${parent.kind}`,
          undo: async () => {
            await removeAnnotation(currentId);
            await refresh();
          },
          redo: async () => {
            const again = await addAnnotation(
              documentId,
              page,
              "note",
              created.data,
              created.text,
            );
            currentId = again.id;
            await refresh();
          },
        });
      } catch (e) {
        setError(e as AppError);
      }
    },
    [documentId, docKey, refresh, push],
  );

  const editText = useCallback(
    async (
      targetPage: number,
      run: PageTextRun,
      replacement: string,
    ): Promise<string | null> => {
      if (!documentId) return null;
      // A merged run's tail fragments travel with the edit so the backend can clear them in
      // the same pass (P2.4.1 T18). An ordinary run sends none.
      const fragments: TextRunFragmentRef[] = (run.fragments ?? [])
        // The head fragment `run` itself already carries; only the tail travels here (T18).
        .filter((fragment) => !sameRunId(fragment.runId, run.runId))
        .map((fragment) => ({
          runId: fragment.runId,
          text: fragment.text,
        }));
      // A range run that is itself merged contributes its own tail too: the backend writes
      // one replacement into the selection's head and clears every other fragment (T26).
      // The selection is the range; the head is `run`, so the rest are cleared. Derived here
      // rather than passed in: a caller that forgot the argument must not write a one-run
      // edit while the UI shows three runs selected.
      const range = selection.filter((selected) => !sameAddress(selected, run));
      const rangeFragments: TextRunFragmentRef[] = range.flatMap((selected) =>
        (selected.fragments ?? [])
          .filter((fragment) => !sameRunId(fragment.runId, selected.runId))
          .map((fragment) => ({ runId: fragment.runId, text: fragment.text })),
      );
      try {
        const result = await editPageText(
          documentId,
          targetPage,
          run.runId,
          // The head *operand's* bytes, not the run's display text: a merged run's `text`
          // joins its fragments and, since T26 item 4, may show a kern seam as a word space
          // the bytes do not contain. The backend re-validates this against the content
          // stream, so sending the display text would refuse every merged kern edit.
          run.fragments?.[0]?.text ?? run.text,
          replacement,
          run.width ?? 0,
          run.fontName,
          run.fontSize,
          allowSubstitution,
          [...fragments, ...rangeFragments],
          // A range entry names the run's *head fragment*: that is the operand the write
          // clears. A merged run's remaining fragments travel in `fragments` above.
          range.map((selected) => ({
            runId: selected.fragments?.[0]?.runId ?? selected.runId,
            // Same rule as the head: range entries are re-validated by their operand bytes.
            text: selected.fragments?.[0]?.text ?? selected.text,
          })),
        );
        // T10: a substitution is never silent. Carry the backend's disclosure into the
        // toast, and clear it again on any edit that keeps the original font.
        setSubstitution(
          result.substitution
            ? { baseFont: result.substitution, message: result.note ?? "" }
            : null,
        );
        // P2.7.1 T30: an `/ActualText` we could not rewrite is a silent divergence between
        // what the page shows and what the document says it shows. Say so.
        setActualTextNotice(
          result.actualTextPreserved === false
            ? "This text is tagged with accessibility text (ActualText) that could not be " +
              "updated; screen readers will still read the previous wording."
            : null,
        );
        // T5: the inverse is a ContentOperation, so undo re-applies the previous bytes to
        // the file this edit wrote. Redo replays the edit on *that* file, so each entry
        // tracks the path its own inverse describes: an edit made after this one would
        // otherwise be reverted from the wrong bytes and refused as a mismatch.
        let currentPath = result.path;
        push(docKey, {
          label: "Edit text",
          undo: async () => {
            currentPath = await revertPageText(documentId, currentPath, result.inverse);
          },
          redo: async () => {
            currentPath = await editPageText(
              documentId,
              targetPage,
              run.runId,
              run.text,
              replacement,
              run.width ?? 0,
              run.fontName,
              run.fontSize,
              allowSubstitution,
              fragments,
            ).then((again) => again.path);
          },
        });
        return result.path;
      } catch (e) {
        setError(e as AppError);
        return null;
      }
    },
    [documentId, docKey, push, allowSubstitution, selection],
  );

  const clearSubstitution = useCallback(() => setSubstitution(null), []);
  const clearActualTextNotice = useCallback(() => setActualTextNotice(null), []);

  const decryptCopyToFile = useCallback(
    async (password: string): Promise<DecryptOutcome | null> => {
      if (!documentId) return null;
      try {
        return await decryptCopy(documentId, password);
      } catch (e) {
        setError(e as AppError);
        return null;
      }
    },
    [documentId],
  );

  const undo = useCallback(async () => {
    const current = stacksRef.current.get(docKey) ?? { undo: [], redo: [] };
    const entry = current.undo[current.undo.length - 1];
    if (!entry) return;
    try {
      await entry.undo();
      const next = {
        undo: current.undo.slice(0, -1),
        redo: [...current.redo, entry],
      };
      stacksRef.current.set(docKey, next);
      setStacks(next);
    } catch (e) {
      setError(e as AppError);
    }
  }, [docKey]);

  const redo = useCallback(async () => {
    const current = stacksRef.current.get(docKey) ?? { undo: [], redo: [] };
    const entry = current.redo[current.redo.length - 1];
    if (!entry) return;
    try {
      await entry.redo();
      const next = {
        undo: [...current.undo, entry],
        redo: current.redo.slice(0, -1),
      };
      stacksRef.current.set(docKey, next);
      setStacks(next);
    } catch (e) {
      setError(e as AppError);
    }
  }, [docKey]);

  const clearError = useCallback(() => setError(null), []);

  /**
   * Picking a run replaces any pending refusal (P2.4 T3/Q2): the previous gate message
   * referred to a different run, and showing it against the new selection would name the
   * wrong reason.
   */
  // A page change drops the selection: the runs it names are not on the new page, and a
  // stale head would let the box edit text the user is no longer looking at.
  useEffect(() => {
    setSelectedRun(null);
    setSelection([]);
  }, [page]);

  const selectRun = useCallback((run: PageTextRun | null, extend = false) => {
    // A shift-click extends the range but keeps the *head*: the replacement is written into
    // the head's operand, so moving it to the shift-click target would type into the end of
    // the selection instead of where the user started.
    if (!extend) setSelectedRun(run);
    setError(null);
    // P2.6 T26: a selection is a range of runs. A plain click is a one-run range; a
    // shift-click takes every run between the current head and `run` in `pageRuns` order,
    // which is reading order, so the backend's contiguity check speaks for itself.
    setSelection((current) => {
      if (!run) return [];
      if (!extend || current.length === 0) return [run];
      const from = pageRuns.findIndex((candidate) => sameAddress(candidate, current[0]));
      const to = pageRuns.findIndex((candidate) => sameAddress(candidate, run));
      if (from < 0 || to < 0) return [run];
      const [start, end] = from <= to ? [from, to] : [to, from];
      return pageRuns.slice(start, end + 1);
    });
  }, [pageRuns]);

  const value = useMemo<EditorContextValue>(
    () => ({
      // Only the open page's annotations are drawn; the DB row for another page is not
      // the overlay's business.
      annotations: annotations.filter((row) => row.page === page),
      allAnnotations: annotations,
      selectedId,
      activeTool,
      style,
      error,
      canUndo,
      canRedo,
      setActiveTool,
      setStyle,
      setSelectedId,
      create,
      remove,
      updateText,
      setStatus,
      reply,
      editText,
      substitution,
      clearSubstitution,
      actualTextNotice,
      clearActualTextNotice,
      allowSubstitution,
      setAllowSubstitution,
      decryptCopyToFile,
      pageRuns,
      runsLoading,
      textEditMode,
      setTextEditMode,
      selectedRun,
      selection,
      selectRun,
      undo,
      redo,
      clearError,
    }),
    [
      annotations,
      page,
      selectedId,
      activeTool,
      style,
      error,
      canUndo,
      canRedo,
      setStyle,
      create,
      remove,
      updateText,
      editText,
      substitution,
      clearSubstitution,
      actualTextNotice,
      clearActualTextNotice,
      allowSubstitution,
      decryptCopyToFile,
      pageRuns,
      runsLoading,
      textEditMode,
      selectedRun,
      selection,
      selectRun,
      undo,
      redo,
      clearError,
    ],
  );

  return <EditorContext.Provider value={value}>{children}</EditorContext.Provider>;
}

export function useEditor(): EditorContextValue {
  const context = useContext(EditorContext);
  if (!context) throw new Error("useEditor must be used inside an EditorProvider");
  return context;
}

/** Re-exported so the tool palette and the provider cannot disagree on the tool list. */
export { ANNOTATION_KINDS };
