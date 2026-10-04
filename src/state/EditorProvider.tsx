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
  getAppState,
  listAnnotations,
  onAnnotationChanged,
  removeAnnotation,
  restoreAnnotation,
  setAppState,
  updateAnnotation,
} from "../api/invoke";
import type {
  Annotation,
  AnnotationData,
  AnnotationKind,
  AppError,
  ToolStyle,
} from "../api/types";
import { ANNOTATION_KINDS } from "../api/types";

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

interface EditorContextValue {
  /** Annotations for the open page, in draw order. */
  annotations: Annotation[];
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
  undo: () => Promise<void>;
  redo: () => Promise<void>;
  clearError: () => void;
}

const EditorContext = createContext<EditorContextValue | null>(null);

/** Style key per tool (D10), matching the `annotation.style.<tool>` scheme. */
export const styleKey = (tool: AnnotationKind) => `annotation.style.${tool}`;

const DEFAULT_STYLES: Record<AnnotationKind, ToolStyle> = {
  highlight: { color: "#fff3b0", opacity: 0.4 },
  underline: { color: "#1f6feb", opacity: 1 },
  strikeout: { color: "#d1242f", opacity: 1 },
  note: { color: "#f5a623", opacity: 1 },
  ink: { color: "#111418", opacity: 1 },
  shape: { color: "#1f6feb", opacity: 1 },
};

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

export function EditorProvider({
  documentId,
  page,
  children,
}: {
  documentId: string | null;
  page: number;
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
    getAppState(styleKey(activeTool))
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

  const value = useMemo<EditorContextValue>(
    () => ({
      // Only the open page's annotations are drawn; the DB row for another page is not
      // the overlay's business.
      annotations: annotations.filter((row) => row.page === page),
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
