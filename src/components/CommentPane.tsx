import { useMemo, useState } from "react";
import type { Annotation, AnnotationStatus } from "../api/types";
import { ANNOTATION_STATUSES, commonOf } from "../api/types";
import { useEditor } from "../state/EditorProvider";

/**
 * The comment manager (D-A11).
 *
 * A **stateless view over the DB**: it holds only sort and filter, never a cached copy of
 * the annotation list. That is the whole reason it cannot disagree with the page -- a cached
 * list is the failure mode, and the page is the surface a user checks it against.
 *
 * Replies (D-A5) are `note`s whose blob names a parent. They are rendered nested under the
 * annotation they answer, not as rows of their own: a thread is one thing to read, and a
 * flat list would scatter it across the sort order.
 */

/** The columns a user can sort by (D-A11). */
type SortKey = "page" | "author" | "date" | "type" | "status";

export interface CommentPaneProps {
  onJumpToPage: (page: number) => void;
}

/** How a reply is indented, in em, so a deep chain stays readable. */
const REPLY_INDENT_EM = 1.5;

/** The label shown for each kind in the pane, matching the palette's names. */
const KIND_LABELS: Record<string, string> = {
  highlight: "Highlight",
  underline: "Underline",
  strikeout: "Strike out",
  squiggly: "Squiggly",
  note: "Note",
  textbox: "Text box",
  callout: "Callout",
  typewriter: "Typewriter",
  ink: "Ink",
  line: "Line",
  arrow: "Arrow",
  rect: "Rectangle",
  oval: "Oval",
};

interface Thread {
  root: Annotation;
  /** Every reply in the document, keyed by the annotation it answers. */
  nested: Map<string, Annotation[]>;
}

/**
 * Groups a flat row list into threads.
 *
 * A reply whose parent is not in `rows` is treated as a root rather than dropped (D-A5 §5):
 * the parent may be on a page this document no longer has, and hiding the user's text is
 * worse than showing it un-threaded. Cycles cannot occur from the UI -- a reply is always
 * created after its parent -- but a hand-edited blob could contain one, so the walk is
 * bounded by the number of rows.
 */
export function threadAnnotations(rows: Annotation[]): Thread[] {
  const byParent = new Map<string, Annotation[]>();
  const ids = new Set(rows.map((row) => row.id));
  for (const row of rows) {
    const parent = commonOf(row).parent;
    if (parent === undefined) continue;
    const bucket = byParent.get(parent) ?? [];
    bucket.push(row);
    byParent.set(parent, bucket);
  }

  const isReply = (row: Annotation) => {
    const parent = commonOf(row).parent;
    return parent !== undefined && ids.has(parent) && parent !== row.id;
  };

  const roots = rows.filter((row) => !isReply(row));
  return roots.map((root) => ({ root, nested: byParent }));
}

export function CommentPane({ onJumpToPage }: CommentPaneProps) {
  // The provider exposes only the current page's annotations, which is what the overlay
  // draws. The pane is document-wide (D-A11), so it asks the DB through the same provider.
  const { allAnnotations, setStatus, reply, selectedId, setSelectedId } = useEditor();
  const [sort, setSort] = useState<SortKey>("page");
  const [descending, setDescending] = useState(false);
  const [filterStatus, setFilterStatus] = useState<AnnotationStatus | "any" | "none">("any");
  const [filterKind, setFilterKind] = useState<string>("any");
  const [text, setText] = useState("");
  const [replyingTo, setReplyingTo] = useState<string | null>(null);
  const [draft, setDraft] = useState("");

  const filtered = useMemo(() => {
    const needle = text.trim().toLowerCase();
    return allAnnotations.filter((row) => {
      if (filterKind !== "any" && row.kind !== filterKind) return false;
      const status = commonOf(row).status;
      if (filterStatus === "none" && status !== undefined) return false;
      if (
        filterStatus !== "any" &&
        filterStatus !== "none" &&
        status !== filterStatus
      ) {
        return false;
      }
      if (needle === "") return true;
      return (row.text ?? "").toLowerCase().includes(needle);
    });
  }, [allAnnotations, filterKind, filterStatus, text]);

  const threads = useMemo(() => {
    const groups = threadAnnotations(filtered);
    const statusRank = (row: Annotation) => {
      const status = commonOf(row).status;
      return status === undefined ? ANNOTATION_STATUSES.length : ANNOTATION_STATUSES.indexOf(status);
    };
    const compare = (a: Annotation, b: Annotation): number => {
      switch (sort) {
        case "page":
          return a.page - b.page;
        case "author":
          // No author field is stored on an annotation yet (P2-D4), so both sides are 0 and
          // the sort is stable rather than pretending to order by something absent.
          return 0;
        case "date":
          return a.modifiedAt - b.modifiedAt;
        case "type":
          return a.kind.localeCompare(b.kind);
        case "status":
          return statusRank(a) - statusRank(b);
      }
    };
    const sorted = [...groups].sort((left, right) => {
      const result = compare(left.root, right.root);
      // A tie falls back to page, then creation order, so the list does not reshuffle
      // between renders for equal keys.
      if (result !== 0) return descending ? -result : result;
      const byPage = left.root.page - right.root.page;
      if (byPage !== 0) return descending ? -byPage : byPage;
      return left.root.createdAt - right.root.createdAt;
    });
    return sorted;
  }, [filtered, sort, descending]);

  const kinds = useMemo(
    () => [...new Set(allAnnotations.map((row) => row.kind))].sort(),
    [allAnnotations],
  );

  const submitReply = (parent: Annotation) => {
    const body = draft.trim();
    if (body === "") return;
    void reply(parent, body);
    setDraft("");
    setReplyingTo(null);
  };

  const renderRow = (row: Annotation, depth: number) => {
    const status = commonOf(row).status;
    return (
      <li
        key={row.id}
        className={`comment-row${selectedId === row.id ? " selected" : ""}`}
        style={{ marginLeft: `${depth * REPLY_INDENT_EM}em` }}
        onClick={() => setSelectedId(row.id)}
      >
        <div className="comment-meta">
          <button
            type="button"
            className="comment-jump"
            onClick={() => onJumpToPage(row.page)}
            title={`Go to page ${row.page + 1}`}
          >
            p{row.page + 1}
          </button>
          <span className="comment-kind">{KIND_LABELS[row.kind] ?? row.kind}</span>
          <span className="muted">{new Date(row.modifiedAt * 1000).toLocaleDateString()}</span>
        </div>
        {row.text ? <p className="comment-text">{row.text}</p> : null}
        <div className="comment-actions">
          <label>
            <span className="visually-hidden">Status for {KIND_LABELS[row.kind] ?? row.kind}</span>
            <select
              value={status ?? ""}
              onChange={(event) => {
                const value = event.target.value;
                void setStatus(row, value === "" ? null : (value as AnnotationStatus));
              }}
            >
              <option value="">No status</option>
              {ANNOTATION_STATUSES.map((candidate) => (
                <option key={candidate} value={candidate}>
                  {candidate}
                </option>
              ))}
            </select>
          </label>
          <button type="button" onClick={() => setReplyingTo(row.id)}>
            Reply
          </button>
        </div>
        {replyingTo === row.id ? (
          <form
            className="comment-reply"
            onSubmit={(event) => {
              event.preventDefault();
              submitReply(row);
            }}
          >
            <input
              aria-label={`Reply to ${KIND_LABELS[row.kind] ?? row.kind} on page ${row.page + 1}`}
              value={draft}
              onChange={(event) => setDraft(event.target.value)}
              placeholder="Reply…"
            />
            <button type="submit" disabled={draft.trim() === ""}>
              Send
            </button>
          </form>
        ) : null}
      </li>
    );
  };

  /** Renders a root and everything answering it, bounded so a cyclic blob cannot hang. */
  const renderThread = (thread: Thread) => {
    const rendered: JSX.Element[] = [];
    const walk = (row: Annotation, depth: number) => {
      if (depth > 8) return;
      rendered.push(renderRow(row, depth));
      for (const child of thread.nested.get(row.id) ?? []) {
        if (child.id === row.id) continue;
        walk(child, depth + 1);
      }
    };
    walk(thread.root, 0);
    return rendered;
  };

  return (
    <section className="comment-pane" aria-label="Comments">
      <h2>Comments</h2>
      <div className="comment-controls">
        <label>
          Sort
          <select value={sort} onChange={(event) => setSort(event.target.value as SortKey)}>
            <option value="page">Page</option>
            <option value="date">Date</option>
            <option value="type">Type</option>
            <option value="status">Status</option>
            <option value="author">Author</option>
          </select>
        </label>
        <label>
          <input
            type="checkbox"
            checked={descending}
            onChange={(event) => setDescending(event.target.checked)}
          />
          Descending
        </label>
        <label>
          Status
          <select
            value={filterStatus}
            onChange={(event) =>
              setFilterStatus(event.target.value as AnnotationStatus | "any" | "none")
            }
          >
            <option value="any">Any</option>
            <option value="none">None</option>
            {ANNOTATION_STATUSES.map((candidate) => (
              <option key={candidate} value={candidate}>
                {candidate}
              </option>
            ))}
          </select>
        </label>
        <label>
          Type
          <select value={filterKind} onChange={(event) => setFilterKind(event.target.value)}>
            <option value="any">Any</option>
            {kinds.map((kind) => (
              <option key={kind} value={kind}>
                {KIND_LABELS[kind] ?? kind}
              </option>
            ))}
          </select>
        </label>
        <label>
          Search
          <input value={text} onChange={(event) => setText(event.target.value)} />
        </label>
      </div>
      {allAnnotations.length === 0 ? (
        <p className="muted">No comments in this document yet.</p>
      ) : filtered.length === 0 ? (
        <p className="muted">No comment matches these filters.</p>
      ) : (
        <ul className="comment-list">{threads.flatMap(renderThread)}</ul>
      )}
    </section>
  );
}
