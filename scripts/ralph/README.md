# Ralph loop — text-editing parity programme

Working state for the Ralph loop that drives P2.7 and beyond.

> Each iteration: read the plan, pick the next unchecked task, implement it, run the full gate,
> commit only when green, update the plan, repeat. No iteration may claim a task done without
> evidence in the log.

## Files

| File | Role |
|---|---|
| `IMPLEMENTATION_PLAN.md` | The task list the loop consumes. Rewritten as tasks complete. |
| `PROMPT.md` | The per-iteration prompt handed to a fresh agent. |
| `loop.sh` | The runner: invokes the agent per iteration, enforces the gate, writes the log. |
| `LOOP_LOG.md` | Append-only record: iteration, task, files touched, gate result, commit. |
| `../../docs/P2.7-REVIEW-OPEN-QUESTIONS.md` | Ruled decisions the loop must obey (B-1…C-8). |

## Gate

A task is **done** only when all of these pass **in the same iteration**:

```
cd src-tauri && PDFIUM_LIBRARY_PATH=$PWD/resources/libpdfium.so cargo test
cd src-tauri && PDFIUM_LIBRARY_PATH=$PWD/resources/libpdfium.so cargo clippy --all-targets
npx tsc --noEmit
npx vitest run
```

`clippy` must add no new warning. `cargo test` runs **without** `--features testfixtures`,
because that is the release command (P2.7.1 §11).

`cargo fmt --check` is **not** in the gate: it already fails at `2cb2f2f` across files the
programme never touches (`protect.rs`, `session.rs`, `substitute.rs`), so the repo is not
rustfmt-clean. Reformatting the tree inside a task would bury the change in unrelated diff.
If the project wants fmt enforced, that is its own commit.

## Rules the loop must not break

1. **One task per iteration.** One task, one commit.
2. **Never weaken a gate** (`fits-width`, `encoding-round-trip`, `run-mismatch`,
   `selection-range`, `simple-font`, `not-subset`). A task that needs to is a design change and
   stops the loop.
3. **Never edit a shared PDF object in place** (B-7). Copy-on-write or refuse.
4. **Never claim a task done on green tests alone** — the task's own acceptance test must exist
   and fail before the change, or the log must explain why it could not.
5. **Silent wrongness outranks a refusal.** If a change makes the app refuse less but behave
   incorrectly, revert it.
6. **Stop and ask** on anything in `P2.7-REVIEW-OPEN-QUESTIONS.md` marked "not ruled".

## Running

```bash
./scripts/ralph/loop.sh --max-iterations 12
```

The runner is deliberately dumb: it does not decide what to work on. The agent reads
`IMPLEMENTATION_PLAN.md` and `LOOP_LOG.md`, so the plan — not the script — stays the source of
truth.
