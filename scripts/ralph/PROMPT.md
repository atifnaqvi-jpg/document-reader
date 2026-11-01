# Ralph loop — per-iteration prompt

You are one iteration of a loop working on the document-reader repo. You have **no memory of
previous iterations**; everything you need is in the files named below. Read them before acting.

## Read first

1. `scripts/ralph/IMPLEMENTATION_PLAN.md` — the task list. The first unchecked, unblocked task
   is yours.
2. `scripts/ralph/LOOP_LOG.md` — what earlier iterations did, and what they learned.
3. `docs/P2.7-REVIEW-OPEN-QUESTIONS.md` §F **and §F.2** — the **ruled** decisions. These are
   binding. §F.2 (Q1–Q9) covers XObject nesting/per-draw copy, the CID ceiling, font
   bundling, multi-level undo, version cadence, the stop condition and per-phase gating.
4. `docs/P2.7-TEXT-EDITING-PARITY-PLAN.md` — scope and sequencing.
5. The design doc for the phase you are touching (e.g. `docs/P2.7.1-mcid-DESIGN.md`).

## Do

1. State, in one line, which task you are doing.
2. **Write the failing test first.** The acceptance test named in the task, or a better one.
   Run it and confirm it fails for the right reason. If it cannot be written yet, say why and
   stop — do not proceed on tests that already pass.
3. Implement the smallest change that makes it pass. No drive-by refactors.
4. Run the full gate. All five commands must pass, and the two `cargo test` shapes are
   **separate** — running only the first one silently skips the 90 PDFium-backed ink tests,
   which are the only tests that compare the renderer to the ink:

   ```
   cd src-tauri && PDFIUM_LIBRARY_PATH=$PWD/resources/libpdfium.so cargo test                          # 513 passed, 90 ignored
   cd src-tauri && PDFIUM_LIBRARY_PATH=$PWD/resources/libpdfium.so cargo test -- --include-ignored     # 603 passed, 0 ignored
   cd src-tauri && PDFIUM_LIBRARY_PATH=$PWD/resources/libpdfium.so cargo clippy --all-targets
   npx tsc --noEmit
   npx vitest run
   ```

   The counts are **pins**. If either moves, stop and report it as a count change, not as a
   green suite — a test that stopped running is not a test that passes.

5. If the gate is red, fix it. If it cannot be fixed within this task's scope, revert your
   change, record the failure in `LOOP_LOG.md`, and leave the task unchecked. Do not commit red.
6. Update `IMPLEMENTATION_PLAN.md` (tick the task, add any newly discovered task).
7. Append to `LOOP_LOG.md`:

   ```
   ## Iteration N — <task id> — <verdict>
   - Task: <one line>
   - Test: <name> (red before: yes/no — if no, explain)
   - Files: <paths>
   - Gate: cargo test <n> passed / <n> ignored, AND --include-ignored <n> passed / <n> ignored,
     clippy clean, tsc clean, vitest <n> passed (both shapes, both counts — one count is not a gate)
   - Commit: <sha or "none">
   - Learned: <anything the next iteration must know>
   ```

8. Commit — **only if the gate is green and the log is updated**. One task, one commit.
   Message format follows the repo: `fix(p2.7): …`, `feat(p2.7): …`, `docs(p2.7): …`.

## Never

- Never weaken an admissibility gate (`fits-width`, `encoding-round-trip`, `run-mismatch`,
  `selection-range`, `simple-font`, `not-subset`, `nested-xobject`) to make a test pass.
- Never edit a shared PDF object in place. Copy-on-write or refuse (ruling B-7).
- Never copy a Form XObject **per page** — the edit unit is the **draw** (ruling Q2).
- Never address an XObject by `ObjectId` in a persisted address or inverse: `renumber_objects`
  invalidates ids on save. Use the resource **name**.
- Never use `change_page_content` to rewrite a Form XObject's stream: it addresses a *page*
  dictionary (`Err(ObjectType)` on a stream), and its silent arm returns `Ok(())` without
  writing when `/Contents` is neither a reference nor an array. Use `change_content_stream`.
- Never edit the original document on disk (D-2). Edits land in the edited copy.
- Never mark a task done because the suite was already green.
- Never leave a probe/example file behind.
- Never decide something marked "not ruled" in `P2.7-REVIEW-OPEN-QUESTIONS.md`. Mark the task
  `[!]` and stop.
- Never mark a **phase** closed (`[x]`) on a green gate alone: a phase is `[x]` only when the
  user has confirmed the installer (ruling Q9). Gate-green-not-yet-confirmed is `[~]`.
- Never bump to **1.0.0** — it is reserved for P2.8 (ruling Q7).
- Never commit `target/`, `node_modules/`, or `artifacts/`.

## Finish

Output exactly:

```
ITERATION <N> | TASK <id> | GATE <green|red> | COMMIT <sha|none> | NEXT <task id or BLOCKED>
```
