/**
 * P3.2 SCOPE-B: the redaction surface, driven through the real IPC mapping.
 *
 * These tests exist because a fully-built backend that nothing calls is this project's own
 * recurring failure: `sign_save` shipped, tested and invisible for a unit. So the assertions here
 * are about the *wiring*, not about re-testing the editor:
 *
 * * the command names `invoke.ts` sends are the names `lib.rs` registers (`redact_mark`, not
 *   `redactMark`; the mapping is what rots);
 * * the arguments are camelCase on this side and arrive snake_case on the other;
 * * the mark list comes from the backend rather than being invented locally;
 * * applying without marks is impossible from the UI, so the I-RD6 rejection is not the user's
 *   first encounter with the rule.
 *
 * `parseRedactionRect` gets its own coverage because it is the one piece of frontend logic that can
 * silently turn a real mark into an invisible one.
 */

import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { installFakeBackend } from "../test/tauri";
import type { FakeBackend } from "../test/tauri";
import type { RedactionMark } from "../api/types";
import { parseRedactionRect } from "../api/types";
import * as api from "../api/invoke";
import { EditorProvider } from "../state/EditorProvider";
import { RedactPanel } from "./RedactPanel";

function makeMark(overrides: Partial<RedactionMark> = {}): RedactionMark {
  return {
    id: "mark-1",
    documentId: "doc-1",
    page: 0,
    kind: "rect",
    rect: "70,690,200,706",
    pattern: null,
    createdAt: 1_000,
    ...overrides,
  };
}

let backend: FakeBackend;

beforeEach(() => {
  backend = installFakeBackend();
});

afterEach(() => {
  backend.teardown();
});

describe("the redaction commands as invoke.ts sends them", () => {
  it("sends redact_mark with a camelCase documentId and the rect untouched", async () => {
    backend.on("redact_mark", () => makeMark());

    const rect = { x0: 70, y0: 690, x1: 200, y1: 706 };
    await api.redactMark("doc-1", 0, rect);

    const [args] = backend.callsTo("redact_mark");
    expect(args.documentId).toBe("doc-1");
    expect(args.page).toBe(0);
    expect(args.rect).toEqual(rect);
  });

  it("reads marks through redact_marks and returns them as the backend shaped them", async () => {
    backend.on("redact_marks", () => [makeMark(), makeMark({ id: "mark-2", page: 1 })]);

    const marks = await api.redactMarks("doc-1");

    expect(backend.callsTo("redact_marks")[0].documentId).toBe("doc-1");
    expect(marks.map((mark) => mark.id)).toEqual(["mark-1", "mark-2"]);
    expect(marks[1].page).toBe(1);
  });

  it("passes the search pattern through unchanged", async () => {
    backend.on("redact_search", () => [makeMark({ kind: "pattern", pattern: "TOPSECRET" })]);

    const marks = await api.redactSearch("doc-1", "TOPSECRET");

    const [args] = backend.callsTo("redact_search");
    expect(args.documentId).toBe("doc-1");
    expect(args.pattern).toBe("TOPSECRET");
    expect(marks[0].kind).toBe("pattern");
  });

  it("names the apply command redact_apply and surfaces the outcome", async () => {
    backend.on("redact_apply", () => ({
      path: "C:/docs/report (redacted 1).pdf",
      runsRemoved: 2,
      annotationsRemoved: 1,
      imagesRemoved: 0,
      objectsDropped: 3,
      marksCleared: 1,
    }));

    const outcome = await api.redactApply("doc-1");

    expect(backend.callsTo("redact_apply")[0].documentId).toBe("doc-1");
    expect(outcome.path).toContain("redacted 1");
    expect(outcome.runsRemoved).toBe(2);
  });

  it("propagates an apply refusal rather than inventing a result", async () => {
    backend.on("redact_apply", () =>
      Promise.reject({ code: "REDACTION_INCOMPLETE", message: "nothing was left to remove" }),
    );

    await expect(api.redactApply("doc-1")).rejects.toMatchObject({
      code: "REDACTION_INCOMPLETE",
    });
  });
});

describe("parseRedactionRect", () => {
  it("reads a stored rectangle into page points", () => {
    expect(parseRedactionRect("70,690,200,706")).toEqual({
      x0: 70,
      y0: 690,
      x1: 200,
      y1: 706,
    });
  });

  it("tolerates spacing around the numbers", () => {
    expect(parseRedactionRect(" 70 , 690 , 200 , 706 ")).toEqual({
      x0: 70,
      y0: 690,
      x1: 200,
      y1: 706,
    });
  });

  it("returns null rather than a rectangle for malformed input", () => {
    // Null rather than zeros: a zero rectangle would draw nothing and, if it ever reached apply,
    // would remove nothing while the UI reported that a redaction had run.
    for (const bad of ["", "1,2,3", "1,2,3,4,5", "a,b,c,d", "1,,2,3", "NaN,0,1,1"]) {
      expect(parseRedactionRect(bad), `input ${JSON.stringify(bad)}`).toBeNull();
    }
  });
});

describe("RedactPanel", () => {
  function renderPanel() {
    return render(
      <EditorProvider documentId="doc-1" page={0} selectRequest={null}>
        <RedactPanel documentId="doc-1" />
      </EditorProvider>,
    );
  }

  it("says marking changes nothing until the document is applied", async () => {
    backend.on("redact_marks", () => []);
    backend.on("list_annotations", () => []);
    backend.on("get_app_state", () => null);

    renderPanel();

    // The copy is an obligation, not decoration: a user who believes marking has protected the text
    // will not apply, and the file still contains it.
    await waitFor(() =>
      expect(
        screen.getByText(/the text is still there, on disk and on screen, until you apply/i),
      ).toBeTruthy(),
    );
  });

  it("lists the backend's marks rather than its own idea of them", async () => {
    backend.on("redact_marks", () => [
      makeMark({ id: "a", page: 0 }),
      makeMark({ id: "b", page: 2, kind: "pattern", pattern: "TOPSECRET" }),
    ]);
    backend.on("list_annotations", () => []);
    backend.on("get_app_state", () => null);

    renderPanel();

    await waitFor(() => expect(screen.getByText(/Page 1/)).toBeTruthy());
    expect(screen.getByText(/Page 3/)).toBeTruthy();
    expect(screen.getByText(/TOPSECRET/)).toBeTruthy();
  });

  it("cannot apply with no marks pending", async () => {
    backend.on("redact_marks", () => []);
    backend.on("list_annotations", () => []);
    backend.on("get_app_state", () => null);

    renderPanel();

    await waitFor(() => {
      const button = screen.getByRole("button", { name: /apply/i });
      expect((button as HTMLButtonElement).disabled).toBe(true);
    });
    // The disable is the point: I-RD6 makes an apply with nothing pending an error, and the UI must
    // not offer a button whose only outcome is that error.
    expect(backend.callsTo("redact_apply")).toHaveLength(0);
  });

  it("reports an empty search as an answer rather than a failure", async () => {
    backend.on("redact_marks", () => []);
    backend.on("redact_search", () => []);
    backend.on("list_annotations", () => []);
    backend.on("get_app_state", () => null);

    renderPanel();

    const input = await screen.findByPlaceholderText(/text to find/i);
    await userEvent.type(input, "absent-word");
    await userEvent.click(screen.getByRole("button", { name: /mark all/i }));

    await waitFor(() =>
      expect(screen.getByText(/does not occur in this document/i)).toBeTruthy(),
    );
  });

  it("reloads the marks and shows the path after applying", async () => {
    let marks = [makeMark()];
    backend.on("redact_marks", () => marks);
    backend.on("list_annotations", () => []);
    backend.on("get_app_state", () => null);
    backend.on("redact_apply", () => {
      marks = [];
      return {
        path: "C:/docs/report (redacted 1).pdf",
        runsRemoved: 1,
        annotationsRemoved: 0,
        imagesRemoved: 0,
        objectsDropped: 0,
        marksCleared: 1,
      };
    });

    renderPanel();

    const apply = await screen.findByRole("button", { name: /apply 1 mark/i });
    await userEvent.click(apply);

    // The whole path is shown, so the assertion matches the real derived name -- "(redacted 1).pdf",
    // with the closing parenthesis. An earlier version looked for "redacted 1.pdf" and failed on a
    // filename that was correct.
    await waitFor(() =>
      expect(
        screen.getByText((_content, element) => element?.tagName === "P" &&
          (element.textContent ?? "").includes("(redacted 1).pdf")),
      ).toBeTruthy(),
    );
    // The list is re-read from the backend, so the cleared mark is gone from the panel too.
    await waitFor(() => expect(screen.queryByText(/Page 1 —/)).toBeNull());
    expect(screen.getByText(/No marks yet\./)).toBeTruthy();
  });
});
