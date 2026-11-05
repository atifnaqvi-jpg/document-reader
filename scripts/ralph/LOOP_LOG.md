# Ralph loop log

Append-only. One section per iteration. The loop reads the tail before starting, so keep the
most recent iteration last.

---

## Note — 2026-11-01 — the loop had no push step through iteration 17

Recorded as a note rather than edited into the iteration entries above, because those entries
are what each iteration actually observed and rewriting them would make the log agree with a
later understanding instead of with the run.

Through iteration 17 `loop.sh` never pushed. It compared `HEAD` before and after the agent,
ran the gate, checked that `LOOP_LOG.md` was non-empty, and stopped. Publication was the
agent's problem and the agent has no push step either, so every iteration's commit stayed
local until a human pushed it.

Two commits were therefore pushed by hand, not by the loop:

- `4abd4ba` — iteration 16 (R-1b). The loop reported a false red on correct, already-committed
  work (stale pins: bash had parsed 509/599 before the agent's commit moved them to 510/600)
  and exited 1. Pushed during the diagnosis of that defect.
- `714f2a9` — iteration 17 (R-2a). The loop exited 0, but still had no push step. Pushed by
  hand after the run.

A red iteration stranded its commit, and a green one did not publish it either. `loop.sh` now
pushes the agent's commit itself, before the gate, so a red iteration leaves a pushed commit
with a red CI run rather than an invisible local one.

---

## Iteration 0 — setup — green

- Task: harness + ruled-answer doc updates
- Tests: none (documentation and scaffolding only)
- Files: `scripts/ralph/{README,IMPLEMENTATION_PLAN,PROMPT,loop.sh}`, `docs/P2.7-*`
- Gate: not run; no production code touched
- Commit: none yet
- Learned:
  - Repo `8eed510…` is a **Tauri/Rust + React PDF editor**, branch `p2-windows-test`.
  - Build and test need `PDFIUM_LIBRARY_PATH=$PWD/resources/libpdfium.so` from `src-tauri/`.
  - `cargo test` must be run **without** `--features testfixtures` (release command).
  - P2.7 docs were untracked at the start of the programme; they are part of the first commit.
  - Baseline before the loop: 393 Rust tests and 62 vitest tests passing, clippy clean.
  - **Known defect:** rotated text is editable and mis-measured (`advance` is horizontal for a
    vertical run). This is H0 and must be fixed before any new phase.
  - **Known defect:** `decode_text_string` uses Latin-1 for non-UTF-16 strings while claiming
    PDFDocEncoding. This is T-2 and can write a different character than it read.

## Iteration 1 — H0.1/H0.2/H0.4 — green

- Task: refuse rotated/skewed runs instead of measuring them on the wrong axis
- Test: `refuses_a_rotated_run_with_gate_h` (red before: **yes** — verified by deleting the
  gate and watching `assert!(!verdict.editable)` fail), plus
  `a_rotated_text_matrix_is_flagged_on_the_run`, `an_axis_aligned_matrix_is_not_flagged_as_rotated`,
  `a_scaled_axis_aligned_matrix_is_not_flagged_as_rotated`,
  `an_axis_aligned_run_stays_editable`, `a_rotated_run_refuses_as_rotated_not_as_unpaired`
- Files: `src/editor/textlocate.rs`, `src/editor/textedit.rs`, `src/editor/testpdf.rs`
- Gate: cargo test 327 + 72 integration passed, clippy clean (0 warnings), tsc clean,
  vitest 62 passed
- Commit: see `fix(p2.7-hotfix)`
- Learned:
  - `cargo fmt --check` **fails at HEAD** on files this programme never touches. It is not in
    the gate; do not reformat the tree inside a task.
  - `RawRun` construction sites: `textlocate::locate_runs` (real) and `textedit::tests::test_run`
    (helper). Adding a field means touching both.
  - `describe_runs` overwrites the admissibility verdict with `run-mismatch` when no PDFium box
    pairs. A geometry refusal must be decided *before* that, or the user sees the wrong reason.
    Test `a_rotated_run_refuses_as_rotated_not_as_unpaired` pins that ordering.
  - The rotated-text probe from the stock take is now a committed fixture pair
    (`rotated_text` / `unrotated_text`) rather than an example binary — the 0.4.4 lesson.

## Iteration 2 — T-1 (`Ts`) — green

- Task: track text rise and treat it as an ink-baseline offset, never a pen move
- Test: `does_not_merge_across_a_rise_change` (red before: **yes** — reverting `mergeable` to
  compare `origin.1` makes it fail), `text_rise_is_recorded_without_moving_the_pen`,
  `text_rise_is_reset_by_bt`, `merges_two_runs_raised_by_the_same_ts`
- Files: `src/editor/textlocate.rs`, `src/editor/textedit.rs`, `src/editor/testpdf.rs`
- Gate: cargo test 331 + 72 integration passed, clippy 0 warnings
- Learned:
  - `Ts` belongs in the walker's text state and is reset by `BT` like `Tc`/`Tw`/`Tz`.
    It must **not** enter `origin()`: the pen does not move, only the ink does.
  - Three baseline comparisons existed (`pair_run` containment + delta + overshoot,
    `mergeable`, `validate_run_range`, `smallest_containing`); all now use
    `RawRun::baseline() = origin.1 + rise`. Missing any one of them re-introduces the bug
    for a different code path, which is why each is covered.
  - **New finding, not fixed here:** `TextMatrixWalk` does not advance the pen after a plain
    `Tj`/`'`/`"` — only `TJ` advances internally. Consecutive `Tj` operands therefore report
    the same origin x. Pre-existing, independent of `Ts`. Needs its own task; recorded in
    `PENDING` below rather than silently fixed inside this one.

### Iteration 2b — T-1 acceptance test

The plan's stated acceptance (a `Ts 5` run pairs to a box offset by 5pt) was **not** covered by
the first iteration's tests: `does_not_merge_across_a_rise_change` covered merging, not pairing.
Added `a_raised_run_pairs_to_the_box_offset_by_its_rise`. The first version of it passed
against the *old* code too (both boxes contained the pen y), which is the exact failure mode
the prompt's "confirm it fails for the right reason" step exists to catch — the fixture was
rebuilt so the two readings select different boxes, and it now fails with `left: 0, right: 1`
when `ink_y` is reverted to `run.origin.1`.

## Iteration 3 — T-2 (B-6) — green

- Task: decode `ActualText` with the real PDFDocEncoding table, and refuse to rewrite a value
  we could not decode
- Test: `pdfdocencoding_table_matches_the_standard` (all 256 codes, red before: **yes** —
  perturbing one entry fails with `byte 0x85 must decode to Some('–')`);
  `pdfdocencoding_actualtext_decodes_to_the_standard_characters` (red before: **yes** — old
  Latin-1 decoder returned `"ALPHA \u{80} 5\u{a0}"`); `undecodable_actualtext_is_reported_not_rewritten`
  (red before: **yes**, and independently of the decoder — with only the refusal branch removed
  it still fails at `text we could not read must never be overwritten as 'preserved'`)
- Files: `src-tauri/src/editor/textlocate.rs`, `src-tauri/src/editor/testpdf.rs`,
  `src-tauri/examples/make_p24_fixtures.rs`, `src-tauri/tests/textedit_gates.rs`,
  `src-tauri/tests/fixtures/actualtext-{pdfdocencoding,undecodable}.pdf`,
  `docs/P2.7.1-mcid-DESIGN.md`, `docs/P2.7-REVIEW-OPEN-QUESTIONS.md`
- Gate: cargo test 333 lib + 50 gates + 24 other integration = 407 passed, clippy clean,
  tsc clean, vitest 62 passed
- Commit: see `fix(p2.7.1): decode ActualText with real PDFDocEncoding`
- Learned:
  - **The plan's own wording for T-2 was wrong.** It listed `0xA0`–`0xFF` as exception
    ranges; in table D.2 only `0xA0` is exceptional (euro). `0xA1`–`0xFF` *are* Latin-1, and
    `0xAD` is one of the three **undefined** codes (with `0x7F` and `0x9F`), not a soft hyphen.
    Do not build the table from the plan text; build it from the standard.
  - Three independent sources agree (qpdf `QUtil.cc` `pdf_doc_to_unicode`,
    poppler `libpoppler.so` `pdfDocEncodingToUnicode`, pypdf `_codecs/pdfdoc.py`). Extracting
    poppler's table is cheap: search the .so for eight `<I` LE words starting `0x02d8`; the
    table then starts `0x18` entries earlier. That is how the 256-entry expectation was
    cross-checked rather than hand-typed.
  - lopdf's `lopdf::encodings::PDF_DOC_ENCODING` exists but is **not** public API
    (`pub use encodings::{Encoding, encode_utf8, encode_utf16_be}`), so it cannot be reused
    from here; we own a 32-entry exception table instead.
  - **Fixture regeneration side effect:** `scripts/make-p24-fixtures.sh` rewrites every fixture
    including `encrypted-aes.pdf`, whose AES salt is random, so it always shows as modified.
    Revert it with `git checkout --` after regenerating; commit only the new PDFs.
  - Non-string `ActualText` now discloses (`actual_text_preserved = false`) rather than
    erroring the whole edit — an unreadable property must not block the text change.

## Iteration 4 — T-3 (B-7) — green
- Task: copy-on-write for a named `/Properties` entry carrying `ActualText`, plus undo of the
  clone.
- Test: `a_shared_named_property_is_cloned_not_rewritten_in_place` (red before: **yes** — first
  `index out of bounds: len is 0` because the locator saw no named properties at all, then, once
  the reader was fixed, `a clone is exactly how B-7 lets us preserve it` at the
  `actual_text_preserved` assertion). Also `revert_removes_the_property_clone` (red before:
  **yes** — verified by disabling only the cleanup block, which fails at the clone-key
  assertion), and `shared_actualtext_is_reported_not_rewritten` was renamed to
  `shared_actualtext_is_copied_before_it_is_rewritten` because its old assertion encoded the
  pre-B-7 behaviour that this task supersedes.
- Files: `src-tauri/src/editor/textlocate.rs`, `src-tauri/src/editor/textedit.rs`,
  `src-tauri/tests/textedit_gates.rs`, `src-tauri/src/editor/testpdf.rs`,
  `src-tauri/examples/make_p24_fixtures.rs`,
  `src-tauri/tests/fixtures/named-property-shared-actualtext.pdf`,
  `src/api/types.ts`, `docs/P2.7.1-mcid-DESIGN.md`, `scripts/ralph/IMPLEMENTATION_PLAN.md`
- Gate: cargo test 334 lib + 51 gates + 24 other integration = 409 passed, clippy clean,
  tsc clean, vitest 62 passed
- Commit: see `feat(p2.7.1): copy-on-write for named ActualText properties`
- Learned:
  - **Found a real reader bug: `Document::get_page_resources` returns the inline `/Resources`
    dictionary only.** It returns `(Option<&Dictionary>, Vec<ObjectId>)` — the `Option` is the
    page's *own inline* dict, the `Vec` is the referenced one(s), following the `/Parent` chain.
    Every fixture with a named `/Properties` uses an **indirect** `/Resources`
    (`/Resources 4 0 R`), so `MarkedContentStack` resolved no properties at all and
    `named_property_no_actualtext_is_not_a_warning` had been passing **vacuously**: nothing was
    resolved, so nothing warned. Any code path that reads page resources must handle both
    forms; `properties_slot`/`properties_dict`/`properties_mut` in `textlocate.rs` now do, and
    the substitute-font path already did (it uses the `Vec`).
  - The clone is recorded on the inverse as `propertyClone { opIndex, oldName, newName,
    cloneObject }`. `ObjectId` is `(u32, u16)` and serialises as a JSON array, which is fine
    because the frontend round-trips the inverse object verbatim — do not "normalise" it.
  - Undo must remove the clone key **and** the clone object: lopdf writes every entry in
    `document.objects`, reachable or not, so leaving the object would grow the file on every
    edit/undo cycle. Guard the removal on the entry still being a reference to *our* object.
  - `fresh_property_name` caps at 999 suffixes (`/P0_edit999`); past that it returns `None` and
    the edit discloses instead of cloning. That is a deliberate ceiling, not an oversight.
  - Harness note: two long `cargo test` runs were lost to terminal-session restarts (one tmux
    server death, one "restart during tool execution"). Running the command with output
    redirected to a file in the background and polling the file is the reliable pattern here;
    `cargo` output is otherwise truncated by the tool.

## Iteration 5 — T-3b — green

- Task: audit the page-resource readers/writers for the inline-only `/Resources` assumption and
  add an indirect-`/Resources` fixture to each path that needed one.
- Test: `an_inherited_resources_page_reads_the_font_it_actually_uses` (red before: **no** — the
  read path was already correct; it is the control that proves the fixture says what it claims),
  `a_substitute_lands_on_the_page_that_shadows_its_inherited_resources` (red before: **yes** —
  `the substitute must be visible to the page it was installed for, got [("F1",
  "ABCDEF+Shadowed")]`), `a_substitution_on_an_inheriting_page_does_not_reach_its_sibling` (red
  before: **yes** — the sibling reported `[("F1","ABCDEF+Inherited"),("FS1","Helvetica")]`),
  `a_watermark_keeps_the_resources_the_page_inherits` (red before: **yes** — `left: "Helvetica",
  right: "ABCDEF+Inherited"`).
- Files: `src-tauri/src/editor/textlocate.rs`, `src-tauri/src/editor/export.rs`,
  `src-tauri/src/editor/testpdf.rs`, `src-tauri/examples/make_p24_fixtures.rs`,
  `src-tauri/tests/textedit_gates.rs`, `src-tauri/tests/export.rs`,
  `src-tauri/tests/fixtures/indirect-resources-inherited.pdf`,
  `scripts/ralph/IMPLEMENTATION_PLAN.md`
- Gate: cargo test 334 lib + 54 gates + 25 other integration = 413 passed, clippy clean,
  tsc clean, vitest 62 passed
- Commit: see `fix(p2.7.1): resolve inherited and shared page resources before writing`
- Learned:
  - **A page's own `/Resources` *replaces* an inherited one; it is not merged** (ISO 32000-1
    §7.7.3.4). `Document::get_page_fonts` **unions** the page dict with every referenced one up
    the `/Parent` chain, which is right for "can this font be found at all" and **wrong** for
    "does the page resolve the name its content stream writes". Do not use it to answer the second
    question, and do not use it in a test that asserts what a viewer sees.
  - `get_page_resources(...).1` is the list of *referenced* resources objects following
    `/Parent`, page's own first **if indirect**. It is non-empty whenever any ancestor has an
    indirect `/Resources`, which is exactly how the old `ensure_substitute_resource` fell into
    writing to the ancestor.
  - Two silent-wrongness bugs, both from that one assumption, both now fixed:
    substitution on a shadowing page installed `FS1` where the page could not see it (edited text
    stops rendering); `export::watermark` rebuilt `/Resources` from the page's inline dict only
    (dropping inherited fonts/XObjects) and took the name `/F1` over (page text redrawn in
    Helvetica). Watermark's font name is now `WmkFont`.
  - The resolver is one shared helper, `textlocate::writable_resources_mut` (+
    `effective_resources`, `resources_slot`, `reference_count`). Any future page-resource writer
    must go through it rather than reaching for `get_page_resources`.
  - "Shared" is decided by counting references to the resources object across `document.objects`.
    A page whose `/Resources` is an indirect object used once is still writable in place; one used
    twice gets copied. Reference counting is O(objects) and runs once per write, not per run.
  - `Object: From<&[u8]>` does **not** exist; a `/Name` operand needs
    `Object::Name(bytes.to_vec())`. `BTreeMap<Vec<u8>, _>::get` needs `b"..".as_slice()`, not
    `b".."`.
  - Fixture regeneration still rewrites `encrypted-aes.pdf` (random AES salt) — `git checkout --`
    it every time, as the T-2 entry says.

## Iteration 6 — T-4 — green

- Task: bump the release to 0.5.0 across every carrier and ship release notes for it
- Test: `every_release_carrier_reports_the_same_version` (red before: **yes** — `the release
  version is 0.4.5, but package-lock.json root version says 0.2.1, package-lock.json
  packages[""].version says 0.2.1`), `the_shipped_version_has_release_notes` (red before:
  **yes** — `no release notes for 0.5.0`), `the_0_5_0_release_notes_cover_every_change_the_plan_claims`
  (red before: **yes** — the file did not exist; then, once written, it failed on the missing
  copy-on-write mention, so it is not a test that cannot fail)
- Files: `src-tauri/tests/release_version.rs` (new), `docs/RELEASE-NOTES.md` (new),
  `package.json`, `package-lock.json`, `src-tauri/{Cargo.toml,Cargo.lock,tauri.conf.json}`,
  `docs/P2.7.1-mcid-DESIGN.md`, `docs/P2.7-TEXT-EDITING-PARITY-PLAN.md`,
  `scripts/ralph/IMPLEMENTATION_PLAN.md`
- Gate: cargo test 334 lib + 1 + 6 + 10 + 2 + 3 + 3 + 3 + 54 integration = 416 passed, clippy
  clean (0 warnings), tsc clean, vitest 62 passed
- Commit: see `chore(p2.7.1): release 0.5.0`
- Learned:
  - **The version carriers had already drifted and nothing noticed.** `package-lock.json` was
    still at **0.2.1** while `package.json`, `tauri.conf.json`, `Cargo.toml` and `Cargo.lock`
    all said 0.4.5. Nothing reads that file at build time, so no test or build ever failed:
    the only symptom would have been an installer whose internal version disagrees with its
    filename. Six carriers now move together and a test pins it, which retires the
    "bump five files" checklist as the only guard.
  - `Cargo.lock` does **not** follow `Cargo.toml` by itself: `cargo metadata --no-deps` leaves
    it alone and `cargo check` refreshes it. Editing it by hand (the `[[package]] name =
    "document-reader"` entry, not the first `version =` in the file) is what the bump commits
    before this one did too.
  - Release notes are one file with one `## <version>` section per release, not
    `RELEASE-<version>.md`: the test looks up the version from `Cargo.toml`, so a forgotten
    section fails the build while a stale section costs nothing.
  - Docs that "list the version bump as a prerequisite" become wrong the moment the bump lands:
    P2.7.1 §7 still said `Ts` was unimplemented and the parity plan still called rotation
    "silently mis-measured". Both corrected here, because a design doc that contradicts a
    shipped release is the same class of defect as the stale `ActualText`.
  - **New task found (T-4b, unchecked):** the release workflow triggers on tags matching `v*`,
    but every real release was tagged `version-0.4.4`. 0.4.4 built only because the tag pointed
    at the branch head pushed in the same event. Tagging 0.5.0 `version-0.5.0` on a commit that
    is not the branch head produces **no installer**. Fix the glob before the release tag.

## Iteration 7 — T-4b — green

- Task: fix the release workflow so a release tag both triggers the Windows installer and gets
  the installer attached — T-4b, restated after its stated diagnosis turned out to be false.
- Test: `a_tag_that_is_not_a_release_does_not_trigger_the_installer` (red before: **yes** —
  `the trigger globs ["v*"] also match non-release tags ["verify-ci", "vendor-drop", "vnext",
  "version-notes"]`), `no_tag_outside_the_release_conventions_publishes_a_release` (red before:
  **yes** — `` `if: startsWith(github.ref, 'refs/tags/v')` publishes a GitHub Release for these
  non-release tags ``). Two companion tests were green before and stay green as controls:
  `the_trigger_accepts_every_release_tag_convention` and
  `every_tag_that_triggers_the_build_also_gets_the_installer_attached` — they are the ones that
  would fail if I narrowed the trigger past `version-0.4.4`, so they are not decoration.
- Files: `.github/workflows/release-windows.yml`, `src-tauri/tests/release_workflow.rs` (new),
  `scripts/ralph/IMPLEMENTATION_PLAN.md`
- Gate: cargo test 422 passed (334 lib + 82 integration, incl. 6 new), clippy clean, tsc clean,
  vitest 62 passed
- Commit: see `fix(ci): trigger and publish only on real release tags`
- Learned:
  - **T-4b's premise was wrong: `v*` DOES match `version-0.4.4`.** GitHub filter patterns say
    `*` matches any run of characters, and the docs' own table states `v2*` "matches branch and
    tag names that start with `v2`" (workflow-syntax § patterns-to-match-branches-and-tags).
    The prior iteration inferred "no trigger" from the tag name alone and never checked the
    glob semantics. Proof from repo evidence, no API needed: `release-windows.yml` has **no**
    `branches:` trigger, so its `event=push` run 38997489826 can only have come from the tag
    push; and the installer asset *was* published, which requires `refs/tags/version-0.4.4` to
    satisfy `startsWith(github.ref, 'refs/tags/v')` at the attach step. Both facts say the tag
    matched.
  - **So the real defect is the opposite: `v*` is too wide.** It matches every tag starting with
    the letter v, and because the attach step guarded on `refs/tags/v` too, any stray tag
    (`verify-ci`, `vnext`, `vendor-drop`) both spent a Windows build *and* published a GitHub
    Release named after itself. Trigger is now `v[0-9]*` + `version-[0-9]*`; both conventions
    still work, nothing else does.
  - GitHub expressions cannot test a tag's *name* (no regex, no `contains`-on-refname that
    distinguishes shapes cleanly), so the attach guard cannot re-implement the trigger. It is
    now `startsWith(github.ref, 'refs/tags/')` — tag vs branch only — and **the trigger globs
    are the single place that decides what counts as a release.** Do not "harden" the guard with
    `contains('.')`: `vnext.1` passes that.
  - The test reads the workflow as text (no YAML crate is a dependency, and adding one for a
    three-level block is not worth it). `tag_globs` is indentation-driven, so **re-indenting
    `on:`/`push:`/`tags:` will silently yield zero globs** — the test asserts the list is
    non-empty for that reason.
  - `the_glob_matcher_follows_the_documented_semantics` exists because the matcher is test
    infrastructure: a matcher returning `false` for everything would make the non-release test
    pass "by the fix", one returning `true` would pass it while proving nothing. It caught its
    own bug on first run (`*` was crossing `/`), which is exactly the class of error the test is
    for.
  - **`cargo fmt --check` is already red at HEAD** in 11 untouched files (testpdf.rs, export.rs,
    textlocate.rs, textedit.rs, session.rs, substitute.rs, protect.rs, tests/export.rs,
    tests/textedit_gates.rs, examples/make_p24_fixtures.rs). It is not in the loop gate; do not
    "fix" it as a drive-by. Format only files you add (`rustfmt --edition 2021 <file>`).
  - Release tags to use going forward: either `v<version>` or `version-<version>`; both are
    accepted and both publish. `README.md` documents `v0.1.0`, the two most recent releases used
    `version-*` — either is now correct, so no doc change was needed.

## Iteration 8 — T-5 — green
- Task: make the P2.7.1 body's docs describe the committed code (T-5 was "commit the body";
  the code was already committed, so the real remaining work was doc truth).
- Test: `src-tauri/tests/docs_citations.rs` (new, 3 tests).
  - `docs_cite_only_identifiers_that_exist` — red before: yes. It reported 7 unresolved
    identifiers in `P2.7.1-mcid-DESIGN.md`, the real one being `page_properties_dict` (the
    resolver is `properties_slot`); the other six resolved once the guard's own token filter was
    corrected.
  - `the_design_doc_acceptance_table_names_tests_that_exist` — red before: yes:
    `shared_actualtext_is_reported_not_rewritten` did not exist.
  - `the_stock_take_agrees_with_the_code_on_text_rise` — red before: yes, after correcting an
    inverted assertion (the first draft passed against the stale doc, which is the failure mode
    this task exists to catch).
- Test: `shared_actualtext_is_reported_not_rewritten` in `textedit_gates.rs` — **not** red on
  the code. The T30 inline multi-operand behaviour was already correct; what was missing was
  the test. Proved it is not vacuous by deleting the `span_is_this_operation` check: the test
  then fails at the `actual_text_preserved` assertion, and passes again when restored.
- Files: `src-tauri/tests/docs_citations.rs` (new), `src-tauri/tests/textedit_gates.rs`,
  `src-tauri/src/editor/testpdf.rs`, `src-tauri/examples/make_p24_fixtures.rs`,
  `src-tauri/tests/fixtures/actualtext-shared-inline.pdf` (new),
  `docs/P2.7.1-mcid-DESIGN.md`, `docs/P2.7-STOCK-TAKE.md`,
  `docs/P2.7-TEXT-EDITING-PARITY-PLAN.md`, `scripts/ralph/IMPLEMENTATION_PLAN.md`.
- Gate: cargo test 426 passed (334 lib + 92 integration), clippy clean, tsc clean,
  vitest 62 passed.
- Commit: `74333ab`.
- Learned:
  - **The rename was the defect, not just the doc.** Ruling B-7 renamed
    `shared_actualtext_is_reported_not_rewritten` → `shared_actualtext_is_copied_before_it_is_rewritten`
    and rewrote its *body* for the named-`/Properties` case. That is a case **replacement**, not
    an addition: the T30 inline multi-operand invariant lost its only test and nobody noticed,
    because the old name disappeared with it. When a ruling renames a test, re-read the body —
    a rename that changes the fixture is a deletion wearing a rename's clothes.
  - The new fixture is `actualtext-shared-inline.pdf`: **two** `Tj` in one inline `/Span`, whose
    single `ActualText` describes both. Distinct from `actualtext-named-property.pdf` (named
    entry, B-7 copy-on-write) and from `actualtext-span.pdf` (one operand, must rewrite). The
    three-way split is the whole T30 contract.
  - **`is_none_or` is Rust 1.82; the crate's MSRV is 1.77** (`rust-version` in `Cargo.toml`).
    Clippy's `incompatible_msrv` catches it, and clippy gates `--all-targets`, so integration
    tests are covered. Use `map_or(true, …)` in test code.
  - `cargo clippy` printed `Finished` with a stale fingerprint after `touch`; deleting
    `target/debug/.fingerprint/*docs_citations*` forced the real recheck. Do not trust a
    0.4s "clean" clippy run after editing a file.
  - The stock take contradicts itself in two places at once (a "Done" row and a
    "Not done"/"Missing" bullet for the same item). A guard that checks only one of them can
    pass while the doc still misleads — `docs_citations.rs` therefore asserts on the *pair*
    (walker handles rise ⇒ no doc line may claim rise is absent).
  - `docs/P2.7-REVIEW-OPEN-QUESTIONS.md` **cannot** be cited-checked naively: its §A quotes
    superseded identifiers deliberately, as the record of what was wrong. That is T-6, and it
    needs a marker distinguishing quoted defects from live claims.

## Iteration 9 — T-6 — green
- Task: extend `docs_citations.rs` to `P2.7-REVIEW-OPEN-QUESTIONS.md` and
  `P2.7-TEXT-EDITING-PARITY-PLAN.md`, separating quoted-defect context from live claims.
- Test: `docs_citations.rs`, 4 new/changed checks.
  - `docs_cite_only_identifiers_that_exist` (extended to all four docs) — red before: **yes**:
    `docs/P2.7-REVIEW-OPEN-QUESTIONS.md: t27_char_spacing_is_in_the_pen_walk`.
  - `every_recorded_defect_states_its_disposition` (new) — red before: **yes**, on five rows
    (A-1, A-2, A-3, A-5, A-6), none of which said whether its defect was fixed.
  - `governed_docs_cite_only_paths_that_exist` (new) — red before: **yes**:
    `docs/P2.7-REVIEW-OPEN-QUESTIONS.md: docs/P2.7-ADOBE-PARITY-PLAN.md`, a file ruling B-1
    renamed away.
  - `governed_docs_point_at_symbols_not_line_numbers` (new) — red before: **yes** (two drifted
    pointers: `textlocate.rs:901` now lands on `.first()`, `:919` on a struct field).
  - `a_struck_citation_is_retracted_but_an_unstruck_one_is_live` (new) — the exemption's own
    unit test, including the unclosed-`~~` case. Green on first run by design; it is the control
    that stops the exemption from silently growing to cover everything.
- Files: `src-tauri/tests/docs_citations.rs`, `docs/P2.7-REVIEW-OPEN-QUESTIONS.md`,
  `docs/P2.7-STOCK-TAKE.md`, `scripts/ralph/IMPLEMENTATION_PLAN.md`.
- Gate: cargo test 430 passed (334 lib + 96 integration), clippy clean, tsc clean, vitest 62
  passed.
- Commit: `966431a`.
- Learned:
  - **The marker was already in the file.** The task proposed inventing `> Defect (fixed):`;
    the audit already used `~~strikethrough~~` for retraction and it renders as retraction to a
    human, so reusing it beat adding a second convention. The rule is now: *struck = the record,
    unstruck = a claim*. Use `~~…~~` when retiring any text in these docs, or the guard will
    treat the record of the bug as the bug.
  - **A checker must not be able to satisfy itself.** `rust_sources()` now excludes
    `docs_citations.rs`. Its own module header quotes the superseded
    `t27_char_spacing_is_in_the_pen_walk`; with the guard in its own source set that name
    "existed in the tree" and the citation test passed while checking nothing. Any guard that
    scans source text has this hole — exclude the scanner.
  - **Extending coverage found two defect classes the task did not name.** Line-number citations
    (`file.rs:901`) rot on every edit; both existing ones had already drifted onto unrelated
    code. They are symbol citations now. And §E promised a doc path that a ruling had renamed
    away. Both are the same failure as a stale test name: plausible, unverifiable, wrong.
  - **§A was the real defect, not just its citations.** Five of six rows were frozen in the
    present tense ("Claims `Ts` is tracked") years after v1.1/v1.2 fixed them. A defect register
    without a disposition column is indistinguishable from a live bug list. Rows now open with
    **Fixed**/**Closed**/**Open**; A-6 (release cadence) is genuinely **Open** — that is a
    programme question, not something this task may settle.
  - Resolve doc paths against **both** the repo root and the crate root: crate-level design docs
    write `examples/make_p24_fixtures.rs`, which is correct and would otherwise read as broken.
  - Non-vacuity was proved by mutation, five ways: unstrike A-2's stale name, add a bogus plan
    citation, drop A-6's marker, unstrike the §E path, rename a cited file stem. Each fails
    exactly one test. Do this before committing a guard — the previous iteration's lesson was
    that a stale-fingerprint clippy run and a self-satisfying scan look identical to green.
  - Next unblocked task is **none**: every remaining item (P2.7.3–P2.7.6, P2.8) is `[!]` blocked
    on an unwritten design doc or an unruled decision (C-4/C-5/C-6). T-6 was the last todo.

## Iteration 10 — T-7 — green

- Task: widen the H0 rotation refusal to rotation carried by `cm` (the CTM), not only `Tm`
- Test: `a_graphics_matrix_rotation_is_flagged_on_the_run` (red before: **yes** — a `cm` of
  `[0 1 -1 0 300 100]` with an identity `Tm` produced `rotated=false`, `advance=114.67`),
  `refuses_a_run_rotated_by_the_graphics_matrix` (red before: **yes**, same cause),
  `a_rotation_restored_by_q_does_not_flag_the_run` (green before and after by design — it is
  the control that fails if `Q` does not restore), plus
  `a_translating_graphics_matrix_is_not_flagged_as_rotated` as the "did not over-refuse" control.
- Files: `src-tauri/src/editor/textlocate.rs`, `src-tauri/src/editor/textedit.rs`,
  `src-tauri/src/editor/testpdf.rs`, `docs/P2.7-STOCK-TAKE.md`,
  `docs/P2.7-TEXT-EDITING-PARITY-PLAN.md`, `docs/RELEASE-NOTES.md`,
  `scripts/ralph/IMPLEMENTATION_PLAN.md`
- Gate: cargo test 435 passed (339 lib + 96 integration), clippy clean, tsc clean,
  vitest 62 passed
- Commit: `eb4af51`.
- Learned:
  - **No unchecked task existed; the plan's "blocked" list was correct, and that was the
    signal to audit a shipped refusal rather than to declare BLOCKED.** Every remaining phase
    needs an unwritten design doc or an unruled decision (C-3/C-4/C-5/C-6), and inventing one
    would be deciding something marked "not ruled". So the iteration checked whether the
    *shipped* H0 contract held: it says "refuse runs whose text matrix is not axis-aligned",
    and it did not.
  - **H0 read `tm` in isolation; the renderer draws with `tm x ctm`.** A content stream that
    rotates with `cm` and leaves `Tm` at the identity still draws vertical text, and H0 called
    it editable with a horizontal `advance` — the precise editable-and-wrong failure H0 exists
    to remove. This is not hypothetical: the app's own `export::watermark` emits exactly this
    shape (`cm` rotation, `BT`/`Tf`/`Td`, no `Tm`), so a watermarked-then-reopened PDF was
    offering its mark as editable.
  - Detection is now `matrix_mul(tm, ctm)` with a `q`/`Q` CTM stack. Keep the boundary the
    module header states: only the **CTM** is saved/restored, and it never enters `origin` or
    `advance`. `q`/`Q` also save text state in the spec, but this walker resets text state on
    `BT` only, unchanged — do not "fix" that as a drive-by; it would move pen positions and
    change every pairing tolerance.
  - **The control tests are the load-bearing ones here.** Widening a refusal can over-refuse,
    and an over-refusing detector would make a rotated-text test pass while breaking ordinary
    pages. `a_translating_graphics_matrix_is_not_flagged_as_rotated` and
    `a_rotation_restored_by_q_does_not_flag_the_run` are what stop that.
  - Non-vacuity proved by two mutations: reverting `is_rotated` to read `tm` alone fails both
    new "is flagged" tests; making `Q` pop without restoring fails only the `Q` control.
  - Unmatched `Q` deliberately leaves the CTM alone rather than resetting to identity: a
    malformed stream must not be read as a transform the renderer never applied.
  - Checked the real-bytes fixture with a one-off probe (`real-maastricht-brochure.pdf`, 31
    streams): every text show has an axis-aligned, unit-scale CTM. So the widening changes
    nothing on the documents we ship, which is why the whole suite stayed green — the gap was
    only reachable through `cm`-rotated producers. The probe was deleted; `git status` clean.
  - **Next unblocked task is still none.** Remaining: P2.7.3 (needs `P2.7.3-xobject-DESIGN.md`
    + a nesting-depth ruling), P2.7.4 (design doc), P2.7.5 (design doc + C-4), P2.7.6 (C-5),
    P2.8 (design doc + C-6 undo infrastructure).

## Iteration 11 — 2026-09-20 — P2.7.3 X1 (address model + Form XObject discovery)

- Task: X1 of P2.7.3. `RunId`/`PageTextRun` gain `xobject_path`; `locate_runs` descends
  `Do` into `/Subtype /Form`; `nested-xobject` gate; drop `PageTextRun.mcid`.
- Files: `src-tauri/src/editor/{textlocate,textedit,testpdf,substitute}.rs`,
  `src-tauri/tests/textedit_gates.rs`, `src/api/types.ts`,
  `src/components/{TextEdit.tsx,TextEdit.test.tsx}`, `src/state/EditorProvider.tsx`,
  `docs/P2.7-STOCK-TAKE.md`, `scripts/ralph/{loop.sh,run-iteration.sh}`.
- Gate: cargo test 351 lib + 60 integration passed, clippy clean, tsc clean, vitest 63 passed.
- Commit: `cdce29d`.
- Learned:
  - **The iteration hit the 200-step ceiling after the gate was already green, and the loop
    reported that as "nothing was completed" and exited 0.** Two harness defects, both fixed
    here: `loop.sh` discarded `run-iteration.sh`'s non-zero exit (a truncated conversation
    looks exactly like a BLOCKED one), and the "no commit" arm accepted a dirty tree. A
    truncated iteration must fail loudly — otherwise the next iteration starts on top of
    half-finished work and the log loses the reason. Step ceiling raised to 400, and the
    truncation path is covered by a stub-agent test.
  - **`opIndex + operandPath` stopped being a unique address the moment a form could be drawn
    at the same op index as a page run.** The TS identity helpers (`textRunIdKey`,
    `sameTextRunId`) and three React `key`s all used it, so a page run and a form run could
    collide in a list and in selection. The address now includes the XObject names in order.
  - **The depth cap is what pins Q1's reading.** Cutting `MAX_XOBJECT_DEPTH` from 8 to 1 must
    fail `depth_nine_refuses_with_nested_xobject` *and* `a_run_inside_a_nested_form_is_located`
    — the second is the test that distinguishes "descend a level at a time" from a literal
    one-level walk. Both were confirmed to fail under the mutation.
  - **A placeholder must be refused by the write path, not only hidden by the UI.** A `Do`
    placeholder carries no text operand; `edit()` refuses it with `nested-xobject` on its own,
    so a request that skips the picker cannot rewrite a name operand.
  - Per-key resource inheritance, not whole-dictionary: a form that omits `/Font` still sees
    the parent's fonts, but a form that defines `/Font F9` must win. Resolving the page's
    dictionary wholesale would decode a form's text through an unrelated font and corrupt
    `advance`.
  - Next: X2 (copy-on-write per draw). The design's warning stands — `change_content_stream`,
    never `change_page_content`; address by resource name, because `renumber_objects`
    invalidates ids on save.

## Iteration 12 — 2026-10-24 — P2.7.3 X2 + X3 (copy-on-write per draw, grouped inverse)

- Task: X2 and X3 of P2.7.3. The write path copies the **draw** the user clicked, writes the
  clone's own stream, and records a grouped inverse that repoints the `Do` and drops the clone.
- Test: `editing_one_draw_leaves_the_other_draw_byte_identical`,
  `editing_a_shared_form_on_one_page_leaves_the_other_page_untouched`,
  `a_nested_run_is_editable_and_only_its_draw_changes`,
  `a_substituted_font_gets_a_private_resources_dict`,
  `editing_a_form_run_writes_the_form_and_not_the_page_stream`,
  `undo_restores_the_do_and_drops_the_clone`, `undo_leaves_the_other_draw_untouched`,
  `undo_after_a_substitution_leaves_no_orphan` (red before: yes — all eight panicked on X1's
  `nested-xobject` refusal, then on the missing level plumbing).
- Files: `src-tauri/src/editor/{textlocate,textedit,testpdf,substitute}.rs`, `src/api/types.ts`.
- Gate: cargo test 358 lib + 101 integration/other passed, clippy clean, tsc clean, vitest 63.
- Commit: `9320e86`.
- Learned:
  - **A Form XObject is a `Stream`, not a `Dictionary`.** `document.get_dictionary(form_id)`
    fails with `expected type Dictionary but found type Stream` even though the object *has*
    a dictionary. Every form-key read/write goes through `form_dictionary` /
    `form_dictionary_mut`, which unwrap `stream.dict`. This is the same class as the
    `change_page_content` trap: the API is shaped for page objects.
  - **`ResourceStack` has to be read *before* the write, not after.** `install_xobject_name`
    reads the enclosing `/XObject` map through `enclosing_resources`, then writes the modified
    map through `writable_resources_at`. Doing it in one borrow also silently re-copied the
    resources dictionary twice, leaving one orphan per clone.
  - **`cloned_resources` must be observed, not predicted.** Q3 says the private `/Resources` is
    created *only when the edit touches it* — and the decision is made inside
    `properties_mut_for_level` / `ensure_substitute_resource`, after the clone exists. Predicting
    it in `clone_draw` records either a copy that never happened (undo then deletes a
    dictionary still in use) or nothing at all (undo leaks one object per cycle). The inverse
    now compares the clone's `/Resources` id against the original's after `apply_merged`.
  - **Delete the clone before its resources.** `object_is_referenced` is a real reference scan;
    while the clone object exists it still points at the private dictionary, so removing the
    dictionary first looks like deleting an in-use object and leaves both behind.
  - **`revert` was page-bound in three places**, not one: the stream it decoded, the stream it
    wrote (both became `read_level`/`write_level`), and the `/Properties` cleanup (needs
    `properties_mut`, which is a page-only path by design — a form-level clone is dropped with
    the clone instead). The old `change_page_content` call had to go: it targets a page
    dictionary, so an undo of a form edit would have silently done nothing.
  - **A test that asserts "the page stream is untouched" is wrong for this feature.** Repointing
    the `Do` *is* a page-stream write; the invariant is "no page **text operand** changed".
  - Non-vacuity, both directions: `clone_draw` returning `Ok(Vec::new())` fails all three
    "leaves untouched" tests, the nested test, and all three undo tests; deleting the `Do`
    restore fails both `undo_restores_the_do_and_drops_the_clone` and
    `undo_leaves_the_other_draw_untouched`.
  - Next: X4 — commit the `.pdf` fixtures through `make_p24_fixtures.rs`, bump to 0.6.0 (three
    version strings + `RELEASE-NOTES.md`), and leave parity row A6 at `Missing` until the
    installer is confirmed (Q9); the phase closes as `[~]`, never `[x]`.

## Iteration 13 — 2026-10-24 — P2.7.3 X4 (committed fixtures + 0.6.0) — green

- Task: commit the seven P2.7.3 §8 Form XObject fixtures, prove them from disk, bump six
  version carriers to 0.6.0, write the 0.6.0 release notes.
- Test: `every_fixture_the_design_doc_names_is_committed`,
  `the_single_and_nested_fixtures_locate_their_text_at_the_documented_depth`,
  `the_two_draw_and_two_page_fixtures_repeat_the_same_form`,
  `the_cycle_and_depth_fixtures_refuse_visibly_at_the_documented_boundary`,
  `the_shared_resources_fixture_gives_both_forms_one_indirect_dictionary`,
  `editing_the_committed_two_draw_fixture_saves_a_reloadable_clone`,
  `the_0_6_0_release_notes_cover_every_change_the_plan_claims` (red before: **yes** — all six
  fixture tests panicked with `IO error` because no `.pdf` existed; the release-notes test
  failed with "has no 0.6.0 section").
- Files: `src-tauri/tests/xobject_fixtures.rs` (new), `src-tauri/examples/make_p24_fixtures.rs`,
  `src-tauri/tests/fixtures/xobject-{single,two-draws,two-pages,nested,cycle,depth-9,shared-resources}.pdf`
  (new), `src-tauri/tests/{release_version,docs_citations}.rs`, the six version carriers,
  `docs/RELEASE-NOTES.md`, `docs/P2.7-STOCK-TAKE.md`, `scripts/ralph/IMPLEMENTATION_PLAN.md`.
- Gate: cargo test 461 passed, clippy clean (0 warnings), tsc clean, vitest 63 passed.
- Commit: `feat(p2.7.3): commit the xobject fixtures and release 0.6.0` — one commit; its sha is
  not recorded here because writing it into the log amends the commit that contains the log
  (each amend re-hashes). Earlier iterations' shas in this file are pre-amend and may dangle.
- Learned:
  - **Fixtures are per-phase artefacts, and a test that builds the shape in memory does not
    commit the shape.** X1–X3 covered every mechanism with in-memory builders and left the
    `.pdf` files ungenerated, so §8's fixture table described files that did not exist. Fixed
    by generating them and by asserting them **from disk** in a new integration test; the
    guard compares the §8 table against the fixture directory, so it fails if either drifts.
  - **`scripts/make-p24-fixtures.sh` rewrites `encrypted-aes.pdf` non-deterministically** (AES
    IV/key). Running it always dirties that one file; `git checkout --` it before committing.
    The seven new fixtures *are* deterministic — verified by regenerating and `md5sum -c`.
  - **The §8 test-name guard only read the P2.7.1 doc.** The P2.7.3 doc names ten tests and
    nothing checked them; the guard now takes a (doc, §8 heading, section terminator) triple.
    Both docs use a different §8 heading (`## 8. Acceptance tests` vs `## 8. Tests`), so the
    heading is data, not a constant.
  - Do **not** add all four/five docs to `GOVERNED_DOCS` casually: adding
    `P2.7.3-xobject-DESIGN.md` fails `governed_docs_cite_only_paths_that_exist` on its bare
    `make_p24_fixtures.rs` citation (an `examples/` file, not a stem the source scan sees).
    That is a real doc fix, but it is a separate task — reverted here rather than smuggled in.
  - Row **A6 stays `Missing`**: Q9 makes a phase `[x]` only when the installer is confirmed.
    Phase H2 is `[~]`, and the stock take now says *why* row A6 is still Missing instead of
    leaving a stale "Tf in a nested Do is out of scope" (which shipped in X1).
  - **Next is `D-1`**, which this iteration added: govern `P2.7.3-xobject-DESIGN.md` with the
    citation guard. It is doc-and-guard only, so Q9 (do not build on an unconfirmed phase)
    does not apply to it. Everything else is blocked: P2.7.4/P2.7.5/P2.7.6 are `[!]` on a
    design doc and P2.8 is `[!]` on both a design doc and U-1, and **U-1 itself must wait** —
    H2 is `[~]` until the 0.6.0 installer is confirmed, and Q9 forbids building on it.

## Iteration 14 — 2026-10-24 — D-1 (govern the P2.7.3 design doc) — green

- Task: D-1 — put `docs/P2.7.3-xobject-DESIGN.md` under the citation guard, and make the guard
  able to resolve a bare `x.rs` citation to a file in `examples/`.
- Test: `governed_docs_cite_only_paths_that_exist` (red before: **yes** — with the doc added to
  `GOVERNED_DOCS` it reported exactly one missing path — the design doc's bare
  `make_p24_fixtures.rs` — which is the failure the task predicted). Added
  `a_bare_stem_from_the_examples_directory_resolves`
  to pin the widened stem scan; it fails (40 stems, no `make_p24_fixtures.rs`) if `examples/` is
  dropped from `crate_stems`.
- Files: `src-tauri/tests/docs_citations.rs`, `docs/P2.7.3-xobject-DESIGN.md`,
  `scripts/ralph/IMPLEMENTATION_PLAN.md`.
- Gate: cargo test 358 lib + 104 integration/other passed (8 in docs_citations), clippy clean,
  tsc clean, vitest 63 passed.
- Commit: `docs(p2.7): govern the xobject design doc with the citation guard`
- Learned:
  - **Widening an exemption is not the same fix as widening a scan.** The task wording said
    "widen the stem set to `examples/`", but the set was not a constant: it was derived from
    `rust_sources()`, which only walks `src/` and `tests/`. So the honest change is a new
    `crate_stems()` walker over the three roots; loosening the *check* (e.g. accepting any bare
    stem) would have made a typo'd citation pass.
  - **Listing a doc in `GOVERNED_DOCS` proves nothing on its own** — all five guarded docs could
    cite only resolvable tokens and the addition would be untested. Mutation is the proof: a
    bogus path fails the path test, and renaming a live `locate_runs` mention to
    `locate_runs_all_levels` fails the identifier test, so this doc's prose is genuinely read.
  - The design doc had **no** `file.rs:NNN` pointers (`governed_docs_point_at_symbols_not_line_numbers`
    passed unmodified) and its §8 table was already checked by X4; the only defect was the bare
    generator citation, i.e. the doc was in better shape than the other four when they were added.
  - Next: **nothing in H3 is startable.** D-1 was the last unblocked task. P2.7.4/P2.7.5/P2.7.6
    are `[!]` on a design doc, P2.8 is `[!]` on a design doc and U-1, and U-1 must wait for the
    0.6.0 installer (Q9 — H2 is `[~]`, and an unconfirmed phase must not be built upon).
    The loop should stop and ask the user to confirm the 0.6.0 installer, or to rule a design
    doc for the next phase.

## Iteration 15 — 2026-10-31 — R-1a (R10: the kerned measured prefix) — green

- Task: R-1a — include the `TJ` kern in the prefix `reflow_tj_array` computes, so the reported
  tail origin is where the ink lands rather than where the arithmetic intended it to land.
- Test: `a_kerned_prefix_includes_the_kern` (red before: **yes** — `moved_to` = 130.704 against
  the fixture's true tail ink at 136.704, short by exactly the 6 pt kern, with a precondition
  assertion that `CD` really is kern-positioned so the test cannot pass on a kern-less fixture).
- Files: `src-tauri/src/editor/reflow.rs`, `docs/P2.8-reflow-DESIGN.md` (new §R10.3),
  `.github/workflows/ci.yml`, `src-tauri/tests/ci_workflow.rs`, `scripts/ralph/loop.sh`,
  `scripts/ralph/PROMPT.md`, `scripts/ralph/IMPLEMENTATION_PLAN.md`.
- Gate: cargo test **509 passed / 90 ignored**, AND `--include-ignored` **599 passed / 0 ignored**,
  clippy clean (0 warnings), tsc clean, vitest **69 passed**. Both counts moved by exactly one and
  both were re-measured, not incremented: the new test is `#[ignore]`d because it compares against
  PDFium ink, which takes ignored 89→90 and the include-ignored total 598→599 while `passed`
  without PDFium stays 509. The `ne 89` pin in `ci.yml`, its mirror in `tests/ci_workflow.rs` and
  the three ralph pins were updated in the same commit.
- Commit: see `git log` (the log is in the commit it describes, so recording its own sha here
  would amend it).
- Learned:
  - **The kern belongs in the reported prefix and NOT in the `Td` offset.** The `TJ` number stays
    in the tail's array and is applied there when the tail shows. Writing `line_origin + new_width
    + kern` into both put it in twice and was measured at 142.704 against the correct 136.704 —
    a test-visible error, not a reasoning one. Recorded as R10.3 in the design doc; the two
    quantities are deliberately unequal and must not be "simplified" back together.
  - **`shift` stays the growth alone.** The tail's old position carried the same kern, so
    `shift = moved_to - (old origin + old_width + kern) == new_width - old_width`. The three
    relations are asserted separately so a future collapse fails on a number.
  - Mutation-verified in the guarded file, per the standing rule: replacing the kern term with
    `0.0` in `reflow.rs` → `scripts/mutate.sh` exit 0 **CAUGHT**; restored → green. The ci.yml pin
    mutation was likewise applied to `ci.yml` itself (`ne 90`→`ne 91`), failed `ci_workflow`, and
    was restored.
  - **R-1b is now the next startable task** and is *not* done: `reflow_tj_array` still accepts
    every `TJ` array whose target has a later fragment, so a run positioned by a preceding `Tj`
    or a mid-array `Td` is measured rather than refused. The design doc now says so in R10.3.

## Iteration 16 — 2026-11-01 — R-1b (refuse a prefix the TJ walk cannot attribute) — green

- Task: R-1b — refuse a run whose baseline prefix is set by an operator outside its own `TJ`
  array, rather than measure a prefix the array walk never saw.
- Test: `a_kern_that_spans_a_tj_or_mid_array_td_refuses` (red before: **yes** — with the check
  removed, both shapes fell through the R6 arm to `not-one-array`, i.e. the unattributed prefix
  was measured; the test reported `left: "not-one-array", right: "R10b-unattributed-kern"`).
- Files: `src-tauri/src/editor/reflow.rs`, `docs/P2.8-reflow-DESIGN.md` (§R10.4 new, R10.3 scope
  corrected), `.github/workflows/ci.yml`, `src-tauri/tests/ci_workflow.rs`, `scripts/ralph/loop.sh`,
  `scripts/ralph/PROMPT.md`, `scripts/ralph/IMPLEMENTATION_PLAN.md`, `scripts/ralph/LOOP_LOG.md`.
- Gate: cargo test **510 passed / 90 ignored**, AND `--include-ignored` **600 passed / 0 ignored**,
  clippy clean (0 warnings), tsc clean, vitest **69 passed**. Both pins moved by exactly one and
  were re-measured rather than incremented: the new test is *not* `#[ignore]`d (it builds runs and
  needs no PDFium), so plain `passed` 509→510 and the include-ignored total 599→600 while the
  ignored count stays 90. The `-lt 509` floor in `ci.yml`, its mirror in `tests/ci_workflow.rs` and
  the two ralph pins were updated in the same commit.
- Commit: see `git log` (the log is in the commit it describes, so recording its own sha here
  would amend it).
- Learned:
  - **The ruling's two shapes are one property of the run set.** The plan and R10 both name "a
    preceding `Tj`" and "a mid-array `Td`". No operator can occur inside a `TJ` array (elements are
    strings and numbers only — the same fact that removed R8's refusal in §2.3.2), so the second
    shape's only constructible form is *a second array on the same baseline, started by its own
    `Td`*. Both reduce to "every same-baseline run must share the target's `op_index`", and that is
    the check — one arm, not two.
  - **The check belongs in the run set, not in the array walk.** `TextMatrixWalk::step` is a
    deliberate no-op for a show operator, so after `Tj (WXYZ)` the next array's fragments are
    reported at the *line matrix* origin while the ink starts an advance further right. A check
    placed inside `tj_fragment_origins` would be validating a value that is already wrong.
  - **Refuse rather than correct**, for the same reason as R8b.1: `moved_to` reduces to
    `run.origin.0 + new_width` whatever `line_origin` is, so a wrong `line_origin` produces a
    record that agrees with itself and disagrees only with the ink.
  - **`scripts/mutate.sh --test NAME` is not a test filter.** It exits 3 (`MEASUREMENT_ERROR`, no
    summary line) because `--test` selects an integration *target*, and this test is in the lib.
    The harness behaved correctly — it refused to read an absent summary as an absent failure —
    and the right invocation is `--lib NAME`. Worth remembering: two of the ralph pins live in
    `scripts/` and `.github/`, so a mutation of `ci.yml` is `MUTATE_RUNTIME_READ` territory only
    when the guard reads it at run time; `tests/ci_workflow.rs` reads it with `fs::read_to_string`
    inside the test body, which is the runtime-read class, and the rebuild check still applies
    because the *guard binary* is Rust. Both directions were run: `-lt 510`→`511` in `ci.yml`
    failed the guard; restoring `510` passed it.
  - Next startable: **R-2a** (the R8b fixture `two-mcid-one-baseline.pdf`), then R-2b, R-2c, R-3.
    R-4 (the A10 move) is where T-8's `Tz` defect starts to matter, and T-8 remains unstarted.

## Iteration 17 — 2026-11-01 — R-2a (the R8b fixture) — green

- Task: R-2a — commit `two-mcid-one-baseline.pdf`, two `/P` tagged units on one baseline with the
  edit target in the second, and guard the fixture's own shape on the parsed operator list.
- Test: `the_r8b_fixture_has_two_units_on_one_baseline` (red before: **yes**, three ways, and all
  three were run rather than reasoned about: (1) with the file absent it fails on its own load;
  (2) with both `BDC`s given `MCID 0` it fails `assert_ne!` on the two scope values; (3) with the
  two `/P` structure elements rewritten to `/Span` it fails the `Block` assertion at
  `reflow.rs:609`. Restoring each returned it to green.)
- Files: `src-tauri/src/editor/testpdf.rs`, `src-tauri/examples/make_p24_fixtures.rs`,
  `src-tauri/src/editor/reflow.rs`, `src-tauri/tests/fixtures/two-mcid-one-baseline.pdf`,
  `docs/P2.8-reflow-DESIGN.md` (§7 item 3 records the fixture landing),
  `.github/workflows/ci.yml`, `src-tauri/tests/ci_workflow.rs`, `scripts/ralph/gate.sh`,
  `scripts/ralph/PROMPT.md`, `scripts/ralph/IMPLEMENTATION_PLAN.md`.
- Gate: cargo test **511 passed / 90 ignored**, AND `--include-ignored` **601 passed / 0 ignored**,
  clippy clean (0 warnings), tsc clean, vitest **69 passed**. Both cargo counts moved by exactly
  one and were re-measured, not incremented: the new test parses a committed fixture with no
  PDFium, so plain `passed` 510→511 and include-ignored 600→601 while `ignored` stays 90. The
  `-lt 510` floor in `ci.yml`, its mirror in `tests/ci_workflow.rs` and the two ralph pins were
  updated in the same commit, and the floor mutation (`511`→`512` in `ci.yml`) was run: the guard
  failed, restoring `511` passed.
- Commit: `feat(p2.8): commit the two-MCID one-baseline fixture (R8b)`
- Learned:
  - **The structure tree is load-bearing, not decoration.** The plan says the units must differ by
    `Block`, "not merely by `Untyped`". A page with two `BDC`s and no `/StructTreeRoot` still gives
    two different `merge_scope()`s — but both are `Untyped(Some(mcid))`, the *finer, conservative*
    boundary. R-2b written against that fixture would pass on the fallback reading while the coarse
    reading the doc names was never exercised. So the builder writes a real
    `/StructTreeRoot` → `/ParentTree` (`Nums` → per-page array of two `/P` elements) and the guard
    asserts `MergeScope::Block` explicitly. `/P` is itself a block role, so the `/P` walk stops on
    the element instead of climbing.
  - **No kern in the fixture's `TJ` array, deliberately.** The first draft carried
    `[(AB) -250 (CD)]` — the `kerned-line.pdf` shape — which would have made R-4's expected tail
    ink depend on T-8's missing `Tz` factor once anything set `Tz != 100`. `[(AB) (CD)]` keeps the
    tail at `200 + advance(AB)` with no second term to inherit a reader bug.
  - **The fixture guard's assertion had to be on the parsed operations, not the bytes.** The plan
    says so and it is right for a concrete reason: a `BDC` closed in the wrong place re-encodes to
    the same tokens in a different order, so a `contains` check would pass on a stream whose
    semantics are wrong. The guard parses with `decoded_content`, counts `BDC`/`EMC`, and compares
    the `/MCID` operands.
  - The generator rewrites every fixture, and `encrypted-aes.pdf` is **not** byte-stable (new
    salt/IV each run), so it shows up dirty after every regeneration. `git checkout --` it before
    staging — do not commit it, and do not "fix" the nondeterminism here.
  - Next startable: **R-2b** (`R8b-scope-left-of-line`), now unblocked with its fixture; then R-2c.

## Iteration 18 — 2026-11-01 — R-2b (R8b: refuse a line whose origin another unit owns) — green

- Task: R-2b — compute `line_origin` and `line_right` only from the runs in the target's
  `merge_scope`, and refuse with `R8b-scope-left-of-line` when a same-baseline run in another
  tagged unit lies left of this line's start.
- Test: `a_same_baseline_run_in_another_mcid_does_not_dislocate_the_tail` (red before: **yes** —
  against the committed fixture it reported `R10b-unattributed-kern` where R8b was expected, which
  is the ordering finding below, not the intended refusal). Second guard
  `a_run_outside_the_scope_does_not_set_the_line_extent` pins the other half of the ruling.
- Files: `src-tauri/src/editor/reflow.rs`, `docs/P2.8-reflow-DESIGN.md` (§R8b.2 new, §7 item 3),
  `src-tauri/src/editor/testpdf.rs` (fmt only), `.github/workflows/ci.yml`,
  `src-tauri/tests/ci_workflow.rs`, `scripts/ralph/gate.sh`, `scripts/ralph/PROMPT.md`,
  `scripts/ralph/IMPLEMENTATION_PLAN.md`, `scripts/ralph/LOOP_LOG.md`.
- Gate: cargo test **513 passed / 90 ignored**, AND `--include-ignored` **603 passed / 0 ignored**,
  clippy clean (0 warnings), tsc clean, vitest **69 passed**. Both cargo counts moved by exactly
  two and were re-measured, not incremented: both new guards parse the committed fixture and need
  no PDFium, so `passed` 511→513 and include-ignored 601→603 while `ignored` stays 90. The
  `-lt 511` floor in `ci.yml`, its mirror in `tests/ci_workflow.rs` and the two ralph pins (both
  shapes) were updated in the same commit; the floor mutation (`513`→`514` in `ci.yml`) failed the
  guard and restoring `513` passed.
- Commit: see `git log` (the log is in the commit it describes, so recording its own sha here
  would amend it).
- Learned:
  - **A rule that is correct can still be unreachable, and that looks identical to done.** R8b
    written after R10b passes its own test only because R10b fires first with a *different*
    message; the acceptance assertion is what surfaced it, because it named the rule it wanted.
    No constructible two-unit line can reach R8b second: a `BDC`/`EMC` pair is an operator pair
    and a `TJ` array's elements are strings and numbers only, so one array cannot hold two scopes
    (the invariant `one_tj_array_cannot_carry_two_tagged_units` already asserts exactly this).
    Both rules refuse, so the ordering decides *which remedy is named*, and R10b's message names
    the wrong one for the R8b shape. Recorded as design §R8b.2 so a future reader does not
    "simplify" the two checks together.
  - **Moving R8b first was only half the fix: R10b then had to be scoped too.** R10b on
    `on_baseline` refuses a right-hand neighbour as unattributable, which makes R8b's `line_right`
    exclusion dead code — mutating `line_right` back to the full set changed nothing observable.
    Scoping R10b to the same `in_scope` set is what makes the ruling's "or `line_right`" clause
    real rather than commented.
  - **A test can pass for the shape it does not reach.** The first version of the extent test was
    synthetic (an empty `Document`), and it was mutation-**SURVIVED**: the extent check sits after
    the document read, so an empty page fails at `decoded_content` and never reaches R4. The test
    that works is the committed fixture plus one injected run — and the injected run must carry a
    *distinct* `block` id, because `MergeScope::Block` compares `ObjectId`s and the first draft
    reused the fixture's own `(6, 0)`, so the "foreign" run was in the same scope.
  - Mutation discipline held both ways, in the guarded file: deleting the R8b arm → CAUGHT;
    `line_right` back to `on_baseline` → CAUGHT (after the test was fixed); and the over-broad
    variant (any out-of-scope run refuses) → CAUGHT, which is what proves the refusal is not
    simply "refuse every two-unit line". Each restore returned the suite to green.
  - **The local gate was not a superset of CI's.** `cargo fmt --check` runs in CI and not in
    `scripts/ralph/gate.sh`, and the tree at `f01c792` was already fmt-dirty in `testpdf.rs` from
    R-2a. This iteration's `cargo fmt` cleaned it as a side effect; the gap is filed as **R-H2**
    rather than fixed here, because adding a gate step is its own task with its own guard.
  - Next startable: **R-2c** (pin R5's message, give R5 a real-content fixture). It now also owns
    R8b's message, which this iteration deliberately left unasserted — the task text says so.

## Iteration 19 — 2026-11-01 — R-2c (pin R5's message; give R5 real content) — green

- Task: R-2c — assert the exact sentence a user reads for R5 (and pin every other refusal's
  identifier and message with it), and commit a fixture where page text and form text share one
  baseline so R5 fires through `locate_runs` rather than only on runs built in a test.
- Test: `r5_reports_its_own_message_and_refuses_real_content` (red before: **yes** — the fixture
  did not exist, `Document::load` reported `NotFound` at the load line, so the test failed for
  the missing fixture rather than for the assertion it is about). Two supporting guards:
  `every_reflow_refusal_names_itself_and_its_remedy` (pins all seven variants) and
  `the_variant_reader_sees_its_own_source_and_not_a_neighbouring_enum` (crafted sample, as the
  parser-helper rule requires).
- Files: `src-tauri/src/editor/reflow.rs`, `src-tauri/src/editor/testpdf.rs`,
  `src-tauri/examples/make_p24_fixtures.rs`,
  `src-tauri/tests/fixtures/page-and-form-one-line.pdf` (new, 788 bytes),
  `docs/P2.8-reflow-DESIGN.md` (§7 item 3), `.github/workflows/ci.yml`,
  `src-tauri/tests/ci_workflow.rs`, `scripts/ralph/{gate.sh,PROMPT.md,IMPLEMENTATION_PLAN.md,LOOP_LOG.md}`.
- Gate: cargo test **516 passed / 90 ignored**, AND `--include-ignored` **606 passed / 0 ignored**,
  clippy clean (0 warnings), tsc clean, vitest **69 passed**, `cargo fmt --check` clean. Both
  counts moved by exactly three and were **measured, not incremented**: 513→516 and 603→606,
  `ignored` unchanged at 90, because all three new guards read a committed `.pdf`/`.rs` with
  `std::fs` and none opens a document through PDFium. The `-lt 513` floor in `ci.yml`, its mirror
  in `tests/ci_workflow.rs` and both ralph pins were updated in the same commit; the floor
  mutation (`516`→`517` in `ci.yml`) failed the mirror guard and restoring `516` passed.
- Commit: see `git log` (this entry is in the commit it describes).
- Learned:
  - **A refusal's message was entirely unasserted, and `name()` looked like coverage.** Both are
    exhaustive `match`es over the enum, so a new variant is *forced* to supply both — but nothing
    forced a *test* to exist, so rewording a sentence a user reads was a silent change. The pin
    now reads the variant list out of the module's own `enum` body via `include_str!`, so the
    hand-maintained table cannot go stale in the direction that matters: an eighth variant added
    to the enum and forgotten in the table fails the guard.
  - **The source reader needed its own crafted sample, and the first draft was a rehearsal.**
    A trim-and-`strip_suffix(',')` line reader "handled" a struct variant only because the sample
    was written on one line; given a body across three lines it would report the *fields* as
    variants. Rewritten to track brace depth, and the sample now carries a multi-line struct
    variant, an attribute, both comment kinds and a second `enum` after the block.
  - **Mutation discipline, four times, each in the guarded file and each a pair (red, then
    restored green):** (1) append `(MUTANT)` to R5's `message()` arm → CAUGHT; (2) add an eighth
    variant `MutantProbe` with both match arms → CAUGHT by the table, which is the stale-table
    direction; (3) `if false &&` in front of the straddle check → CAUGHT, so the gate itself is
    what the test measures and not just its message; (4) rewrite the fixture's `200 700 Td` to
    `200 600 Td` → CAUGHT, failing at the baseline precondition with `700 vs 600`. The fixture
    mutation was verified to have landed (`cmp -l` shows one differing offset, length unchanged
    at 788), and the harness only accepted it once the new file was `git add -N`'d: an untracked
    file is "unmodified relative to HEAD" to `git diff`, so the run was correctly refused as a
    MEASUREMENT ERROR first. That refusal is the harness working, not a bug.
  - **Deleting a fixture is not a harness-testable mutation.** `MUTATE_RUNTIME_READ` on a
    nonexistent path exits 4 by design; the missing-fixture failure was measured with a direct
    `cargo test` instead, and what it proved is only that the test cannot pass vacuously — the
    preconditions inside the test are what catch an off-baseline or single-level fixture.
  - `scripts/ralph/gate.sh` refuses to run while it has uncommitted changes (the pins must be the
    committed ones), so the pin edits have to be committed before the gate is invoked. Run the
    five non-pin steps first, commit, then the gate.
  - Next startable: **R-H1** (assert the fixture tree holds no untracked files), then **R-3**
    (the A10 fixture) and **R-4** (row A10's guard).

## Note — 2026-11-01 — the branch carried a red CI from `94d2d34`, and it was not this loop's

Recorded because the loop treats CI health as a precondition, and that reading was wrong.

`94d2d34` is a **documentation-only** commit: `docs/RELEASE-NOTES.md` and
`docs/P2.7.6-embed-DESIGN.md`, no source file. Its CI run `41278018858` failed at Windows
step 10 (*Assert the PDFium tests actually ran*) with `exit code: 0xc0000005,
STATUS_ACCESS_VIOLATION`. `324c131` passed the same job twenty minutes earlier.

That is now **two timestamped reproductions** — `40908435884` attempt 1 (`3942c8c`, aborted
after 354 of 393 tests; a same-SHA re-run passed) and `41278018858` — of the known
intermittent teardown fault. It is **not caused by any code in this branch**: a docs-only
commit cannot introduce a memory fault, and the earlier reproduction passed on a re-run of
identical bytes. No fix is attempted here.

Consequence for the loop: **a red Windows job is not by itself evidence that an iteration's
code is bad.** The local gate cannot see this fault at all — this box has no Windows and no
`--include-ignored` on the MSVC target — so the loop will happily pass a commit that CI then
fails on this. Distinguishing the two means reading the failing step: `STATUS_ACCESS_VIOLATION`
at step 10 is this defect; any other red belongs to the iteration. See `AGENTS.md`, *Known
open defect: the Windows suite crashes intermittently*.

---

## Iteration 20 — 2026-11-01 — R-H1 (no untracked fixture files) — **blocked**

- Task: R-H1 — assert that nothing lives under `src-tauri/tests/fixtures/` that git does not
  track, so a concurrency test's leftover copy (or a stray swept in by `git add -A`) fails the
  suite instead of sitting in the tree looking like an input.
- Test: `the_fixture_tree_holds_no_untracked_files` (red before: **yes**, in three measured
  steps, and the first two were reader defects rather than tree defects — see *Learned*).
  Supporting guard `the_untracked_reader_splits_on_nul_and_not_on_newlines`.
- Files written, run, and then **reverted**: `src-tauri/tests/fixture_tree.rs` (new, two tests).
  Files committed: `scripts/ralph/IMPLEMENTATION_PLAN.md`, `scripts/ralph/LOOP_LOG.md`.
- Gate: **RED — pre-existing, outside this task.** `the_tree_version_is_ahead_of_every_published_tag`
  (`src-tauri/tests/release_version.rs:178`) fails at `94d2d34`, reproduced on a clean
  `git worktree` at HEAD with my new test file removed. Measured `--no-fail-fast`:
  plain **515 passed / 90 ignored / 1 failed**, `--include-ignored` **605 passed / 0 ignored /
  1 failed**. Both are exactly the pins (516/90 and 606/0) minus the one failing guard, so the
  failure is one test and not a count move. `cargo fmt --check`, clippy `-D warnings`, tsc and
  vitest are all clean with the new test in place; the new test itself passes.
- Commit: docs only (plan + log). R-H1's code is **not** committed, because the prompt is
  explicit that a red gate is never committed and that the change is reverted and recorded.
- Learned:
  - **The cause, and why it is not mine to fix.** `v0.9.0` (a tag object on `6caaedd`) was
    created at 04:26:26 today, *after* iteration 19's green gate. The tree is 30 commits past
    that target and all six carriers still say `0.9.0`, so the guard's `current > highest` is
    `0.9.0 > 0.9.0` = false. The guard is working as designed; the carriers are stale. Filed
    as **R-H3** in the plan, marked `[!]`, because every candidate number is spoken for or
    forbidden: 1.0.0 belongs to P2.8 (Q7/R-7) and may not land before a confirmed installer;
    0.9.0 is the tag; and 0.9.1 is a *development* version which would decide the cadence
    question **A-6**, marked **Open**. Bumping to 0.9.1 would also require a `## 0.9.1`
    release-notes section that the toolchain guard forces to say "verified" about a build
    nobody has run — the exact vacuous claim `05a257f` added that guard to prevent.
  - **CI is green on this, so the local gate is the only witness.** `ci.yml` uses bare
    `actions/checkout@v4`, which fetches no tags. Reproduced in a shallow clone: `git tag
    --list` returns 0 entries, the guard's `git tag --list v*` finds nothing, and it returns
    early. So this is a local-only divergence between gate and CI, in the direction that makes
    correct work look red — the mirror image of the `714f2a9` drift this project already
    recorded (local green, CI red).
  - **Two reader defects were caught by the guard's own floor assertions, not by review.**
    (1) The first draft split `git ls-files` output on NUL without passing `-z`, so 54 paths
    arrived newline-separated and the NUL splitter reported **1** entry. (2) The second draft
    appended `-z` *after* `--`, where git reads it as a pathspec; same symptom, same one-entry
    result. Both were reported as `git reported only 1 tracked files under tests/fixtures/,
    expected at least 54` — the count floor is what turned a silent misread into a failure.
    This is the "assert the precondition" rule doing real work.
  - **Mutations, both in the guarded tree and both a pair (red, then restored green).**
    (1) `cp kerned-line.pdf kerned-line-stray.pdf` — an un-ignored name — failed with the path
    named: `untracked files under tests/fixtures/: ["src-tauri/tests/fixtures/kerned-line-stray.pdf"]`.
    `cmp` confirmed the copy was byte-identical, i.e. a mutation that landed. (2) renaming it
    to `sample-99.pdf` left the test **green**, which is the documented limit of the guard and
    the reason the assertion message says so: `--exclude-standard` hides anything matching
    `.gitignore`, so the two known prefixes are a backstop, not a substitute for staging by
    explicit path. Both strays deleted; `ls | wc -l` back to 54.
  - **Why the test checks untracked and never modified.** Mid-iteration a tracked fixture is
    routinely edited and regenerated by `examples/make_p24_fixtures`; failing on that would
    make the guard fire every iteration and teach the next agent to disable it. The hazard is a
    file git has never heard of. `git ls-files --others --exclude-standard` is the exact query.
  - **Not a substitute for the rule:** this guard cannot see a stray whose name matches the
    two ignored prefixes, and it cannot see an ignored tracked-adjacent file. `AGENTS.md`'s
    "never `git add -A`, stage by explicit path" remains the primary control.
  - **R-H1's implementation was never committed and no copy of it survives.** An earlier
    revision of this entry said the test was "written but uncommitted" and should be
    "re-applied verbatim". That is false and is corrected here. `2ffeccd` touched only
    `IMPLEMENTATION_PLAN.md` and `LOOP_LOG.md`; `FIXTURE_FLOOR` appears nowhere under
    `src-tauri/src` or `src-tauri/tests` at that commit or at any commit on this branch, and
    the working tree held no uncommitted Rust when the iteration ended. The test must be
    **rewritten**, not re-applied. What survives is the design description above -- the
    untracked-vs-modified distinction, the `--exclude-standard` limit, the 54-file floor
    (52 `.pdf` + `README.md` + `sample.txt`) -- and the fact that its own floor assertions
    caught two real defects while it existed.
  - **R-H3 is filed for the version-invariant blocker**, not for R-H1. `v0.9.0` now points at
    `6caaedd`, and `the_tree_version_is_ahead_of_every_published_tag` requires the carriers to
    exceed the highest published tag; they say `0.9.0`, so the test fails on this branch by
    construction. That red is a property of tagging without moving the tree version, not a
    defect in R-H1.
  - Next startable: **nothing** until R-H3 is ruled. R-H1 cannot be committed under a red gate
    and will be rewritten once it is unblocked.

---

## Iteration 21 — 2026-11-01 — R-H1 (no untracked fixture files) — green

- Task: R-H1 — assert that nothing under `src-tauri/tests/fixtures/` exists which git does not
  track, so a concurrency test's leftover copy (or a stray swept in by `git add -A`) fails the
  suite instead of sitting in the tree and looking exactly like an input.
- Test: `the_fixture_tree_holds_no_untracked_files` (red before: **yes** — a
  `kerned-line-stray.pdf` copy in the fixtures directory made it fail naming that exact path,
  then removing the file made it pass). Supporting guard
  `the_untracked_reader_splits_on_nul_and_not_on_newlines` (crafted samples, including both
  defect shapes: a path with an embedded newline, and a newline-separated listing read by a
  NUL splitter).
- Files: `src-tauri/tests/fixture_tree.rs` (new, two tests), `.github/workflows/ci.yml`,
  `src-tauri/tests/ci_workflow.rs`, `scripts/ralph/{gate.sh,PROMPT.md,IMPLEMENTATION_PLAN.md,LOOP_LOG.md}`.
- Gate: cargo test **518 passed / 90 ignored**, AND `--include-ignored` **608 passed / 0
  ignored**, clippy clean (`-D warnings`), tsc clean, vitest **69 passed**, `cargo fmt --check`
  clean. Both counts moved by exactly two and were **measured, not incremented**: 516→518 and
  606→608, `ignored` unchanged at 90, because both new guards call `git`/`std::fs` on the
  working tree and neither opens a document through PDFium. The `-lt 516` floor in `ci.yml`, its
  mirror in `tests/ci_workflow.rs` and both ralph pins moved in the same commit.
- Commit: see `git log` (this entry is in the commit it describes).
- Learned:
  - **An untracked file is not a `git diff`, and that makes this mutant the one shape the
    harness cannot certify.** `scripts/mutate.sh` establishes freshness for a runtime-read
    target by requiring the mutated path to differ from `HEAD`. Here the mutant *is* a file git
    has never seen, so it can never differ from `HEAD`, and both ways of manufacturing a diff
    destroy the defect: `git add -N` makes the stray **tracked**, after which
    `git ls-files --others` reports nothing and the harness returns `SURVIVED` (measured —
    exit 1), and editing a tracked neighbour leaves the stray itself undiffed (also measured:
    the harness correctly refused with "the runtime-read target is not modified relative to
    HEAD", exit 4). The catch was therefore measured with a direct `cargo test` pair rather
    than through the harness, and this entry is the record of why. Do not read the absence of
    a mutation-log CAUGHT record for this test as an unproven guard; read it as a harness
    bound, and check the pair by hand if it is ever doubted.
  - **`MUTATE_RUNTIME_READ` is repo-root-relative but `cd`s into `src-tauri`.** The first
    harness run failed with "names a path that does not exist" because the value was given as
    `tests/fixtures/...`; the same string resolves under `src-tauri/` after the `cd`. Both
    `MUTATE_RUNTIME_READ` and `MUTATE_TARGET` take a path from the repository root.
  - **The floor is what makes the NUL reader safe, and the reader's own test says so.** A
    splitter that is correct cannot distinguish "no untracked files" from "the query returned
    nothing because the pathspec was wrong"; only a count floor on the *tracked* side can. So
    the guard asserts both: `on_disk >= 54` **and** `tracked >= 54`. Iteration 20 measured a
    draft that passed `-z` after `--` (where git reads it as a pathspec) and reported 54 paths
    as **1**; the floor turned that silent misread into a named failure both times.
  - **Mutated the guarded file, not the guard.** `ci.yml`'s floor was moved 518→517 (and
    separately 518→519) with `git diff --stat` naming `.github/workflows/ci.yml` in both runs;
    `the_linux_job_pins_the_measured_passed_floor` failed both times, and restoring 518 passed.
    Both directions matter here because 517 also sits *below* a floor — this pin is an
    equality-shaped `contains`, so it is direction-free, unlike a bare `-lt` comparison.
  - **`cargo fmt --check` caught the new file before CI did.** The first draft was not
    rustfmt-clean (one wrapped `read_dir` call and one long `git(...)` call). That is exactly
    the drift **R-H2** records, and it is why fmt is first in the gate: it is the cheapest
    step and the one whose absence let `714f2a9` pass locally and fail CI.
  - **The guard cannot see an ignored name, by construction and on purpose.** Renaming the
    stray to `sample-99.pdf` leaves the test **green**, because `--exclude-standard` hides
    anything matching `.gitignore`. That is recorded in the assertion message itself so the
    next reader does not mistake a pass for coverage of those two prefixes. `AGENTS.md`'s
    "never `git add -A`, stage by explicit path" remains the primary control.
  - **R-H3 is closed and was not mine to close, so it is recorded rather than re-decided.**
    It was resolved by `429540b` before this iteration started, by moving all six carriers to
    0.9.1 with a release-notes section that says plainly it is unverified and unpublished.
    Measured at the start of this iteration on the untouched tree: plain **516 passed / 90
    ignored / 0 failed**, `--include-ignored` **608 / 0 / 0** after the change — i.e. the
    version guard is green again with the guard text unchanged. The plan now marks it `[x]`.
  - Next startable: **R-3** (the A10 fixture) and then **R-4** (row A10's guard). T-8 is
    flagged in the plan as worth landing **before R-4 starts asserting positions**, because a
    reflow that moves a kern-positioned tail would inherit its 3 pt error at `Tz 50`.

---

## Iteration 22 — 2026-11-01 — R-G1 (gate/CI parity) — green

- Task: R-G1 — assert that every `run:` command in `.github/workflows/ci.yml` is either executed
  by `scripts/ralph/gate.sh` or named in it with a reason, so the next drift between the two is a
  decision instead of an omission.
- Test: `the_gate_runs_the_checks_ci_requires` (red before: **yes** — `gate.sh` had no exemption
  block, so it failed at `parsed 0 exemptions … An unparsed block excuses nothing`, and before
  that at the literal-match assertion in the same test). Supporting guard
  `the_parity_check_reports_what_it_is_given`, which feeds the parser a clean sample and four
  defect shapes and requires a finding for each.
- Files: `src-tauri/tests/gate_parity.rs` (new, two tests), `scripts/ralph/gate.sh` (the
  `>>> ci-exemptions` block, 10 entries), `.github/workflows/ci.yml`, `src-tauri/tests/ci_workflow.rs`,
  `scripts/ralph/{PROMPT.md,IMPLEMENTATION_PLAN.md,LOOP_LOG.md}`.
- Gate: cargo test **520 passed / 90 ignored**, AND `--include-ignored` **610 passed / 0 ignored**,
  clippy clean (`-D warnings`), tsc clean, vitest **69 passed**, `cargo fmt --check` clean. Both
  counts moved by two and were **measured, not incremented**: 518→520 and 608→610, ignored
  unchanged at 90. Both new guards read `ci.yml` and `gate.sh` with `std::fs` and need no PDFium.
- Commit: see `git log` (this entry is in the commit it describes).
- Learned:
  - **The plan's acceptance test asked for a literal comparison, and the plan itself argued it
    down in the same paragraph.** A literal match reports 10 failures on a correct tree, because
    `sudo apt-get update`, `npm ci`, the pwsh fetch, `cargo check --target …-msvc` and the three
    Windows PDFium steps have no local analogue. What landed is the *declared* correspondence the
    plan named as "weaker but real", and the `windows-check` job was kept in the parse rather than
    filtered out — its steps are precisely the ones an allow-list exists to record.
  - **An allow-list entry can be made to satisfy its own excuse, so two cross-checks were added
    that the plan did not ask for.** (1) An exemption naming a command `ci.yml` no longer holds
    fails, so an excuse cannot outlive its step. (2) An exemption whose `covers:` command the gate
    no longer runs fails, so deleting `npx tsc --noEmit` cannot hide behind the `npm run build`
    entry. (3) No exemption may be a prefix of another, so `cargo test` as an entry cannot stand in
    for `cargo test --test memory …`. Each has its own craft-sample arm.
  - **Message lines had to be excluded from the "commands the gate runs" text.** `gate.sh` says
    `log "gate: cargo fmt --check"` before every check. A `contains`-based comparison against the
    whole file would let deleting the `cargo fmt --check` run leave the message behind and still
    pass — the documented "a value that is still satisfied by the defect" shape. `gate_commands`
    drops `log`/`fail`/`printf`/`echo`/`set` lines as well as comments.
  - **Three mutations, all in the file the guard reads, all a pair (red, then restored green).**
    (1) `( cd src-tauri && cargo fmt --check ) || status=1` deleted from `gate.sh` → CAUGHT via
    `scripts/mutate.sh` with `MUTATE_RUNTIME_READ=scripts/ralph/gate.sh`, exit 0.
    (2) A new step `run: cargo miri test` added to `ci.yml` → CAUGHT, exit 0, with
    `MUTATE_RUNTIME_READ=.github/workflows/ci.yml`. (3) `npx tsc --noEmit || status=1` deleted from
    `gate.sh` → CAUGHT, exit 0, which is the `covers:` cross-check firing rather than the step
    comparison. `git diff --stat` named the guarded file in all three runs.
  - **`gate.sh` refuses to run on a dirty pin file**, so the pin move has to be committed before
    the gate is invoked (this is in iteration 20's entry as well and was true again here).
  - **The `executed` floor is the measured value, not headroom.** Two CI commands are run literally
    by the gate (`cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`); `cargo test` is
    covered but written differently (`assert_shape` passes `cargo test "$@"`). A `>= 3` floor failed
    against a correct tree, so it is `>= 2` with the number in the message.
  - Next startable: **R-3** (the A10 fixture), then **R-4**. **T-8** is still worth landing before
    R-4 asserts positions: a reflow that moves a kern-positioned tail inherits its 3 pt error at
    `Tz 50`.

## Iteration 23 — 2026-11-01 — R-3 (the A10 fixture) — green
- Task: Commit the two fixtures row A10 needs, plus the guards that keep them honest.
- Test: `every_fixture_the_reflow_design_names_is_committed` (red before: yes — it panicked with
  `§6.2 names fixtures that are not committed: ["reflow-line.pdf", "reflow-line-near-edge.pdf"]`),
  `the_reflow_fixture_carries_the_a10_topology` (red: `IO error` loading the absent file),
  `the_near_edge_variant_has_almost_no_room_left` (red, same reason),
  `the_fixture_table_parser_sees_its_own_source_and_the_defect_shape`,
  `the_committed_reflow_fixtures_are_the_builders_own_bytes` (red: `reflow-line.pdf is not
  committed: No such file or directory` after the two files were moved aside),
  `the_near_edge_variant_is_not_the_ordinary_one`.
- Files: `src-tauri/src/editor/testpdf.rs` (`reflow_line(near_edge)`, plus a `#[cfg(test)]` module),
  `src-tauri/examples/make_p24_fixtures.rs`, `src-tauri/tests/reflow_fixtures.rs` (new),
  `src-tauri/tests/fixtures/reflow-line.pdf`, `src-tauri/tests/fixtures/reflow-line-near-edge.pdf`,
  `src-tauri/tests/fixtures/README.md`, `docs/P2.8-reflow-DESIGN.md` (§6.2 table plus three status
  rows), `.github/workflows/ci.yml`, `src-tauri/tests/ci_workflow.rs`, `scripts/ralph/gate.sh`,
  `scripts/ralph/PROMPT.md`.
- Gate: cargo test 526 passed / 90 ignored, AND --include-ignored 616 passed / 0 ignored,
  clippy clean, tsc clean, vitest 69 passed (both shapes, both counts — one count is not a gate).
  The old pins were 520/610; the +6 is **measured**, not incremented, and all four carriers moved
  in the same commit.
- Commit: see `git log` (this entry is in the commit it describes).
- Learned:
  - **The fixture topology, measured.** Line 1 at `Tf 24` Helvetica is `ALPHA ` 3557 + `BETA`
    2612 + kern −120 = 6289 thousandths = **150.936 pt**. At x=72 it ends at 222.936, leaving
    389.064 pt of room; at x=452 it ends at 602.936, leaving **9.064 pt**. The 452 was chosen as
    the largest integer x that leaves a positive remainder under 10 pt — 461.064 is an exact fit,
    so 462 would have overflowed and made the file describe an already-broken document.
  - **Why a second file, not a second page.** R-5's assertion is "the stream is byte-unchanged
    after the refusal". With two pages in one file that sentence means "page 0's stream", which a
    partial write elsewhere would satisfy. A separate file makes it "the file on disk", which is
    the property the row actually names.
  - **`every_fixture_the_design_doc_names_is_committed` was not reused.** The existing guard of
    that name in `xobject_fixtures.rs` parses `P2.7.2`'s table; §6.2 is a different section of a
    different document. The name is shared deliberately (the plan named it) but the parser is a
    second one, and it got its own craft-sample test because a parser that returns `[]` would
    make the committed-file check pass over nothing — the "absence of evidence" shape.
  - **Three mutations, the guarded file each time, each a pair.**
    (1) `Tm` → `Td` for line 2 in the builder, then regenerated: `the_reflow_fixture_carries_the_a10_topology`
    FAILED with `["BT","Tf","Td","TJ","Td","TJ","ET"]` and `left: 2, right: 1`. `cmp -l` against
    the pre-mutation blob was non-empty (618 bytes vs 626, differing at offset 211), so the
    mutant landed. Restored: `cmp` identical.
    (2) near-edge x 452 → 400, then regenerated: `the_near_edge_variant_has_almost_no_room_left`
    FAILED with `61.063965 pt left`. `cmp -l` non-empty (offsets 236/237). Restored: identical.
    (3) dropped the `-120` seam, then regenerated: `the_reflow_fixture_carries_the_a10_topology`
    FAILED at the seam assertion, `found [(ALPHA ), (BETA)]`. Restored: identical.
    (4) the doc guard: changed the §6.2 table to name `reflow-line-edge-variant.pdf`;
    `every_fixture_the_reflow_design_names_is_committed` FAILED naming that file. Restored green.
    (5) the pin: `ci.yml` `-lt 526` → `-lt 527`;
    `the_linux_job_pins_the_measured_passed_floor` FAILED. Restored green.
  - **The regenerated tree is not reproducible for one file.** `make-p24-fixtures.sh` rewrites
    `encrypted-aes.pdf` with a fresh salt/IV every run; `git checkout --` it after every
    regeneration, never commit it. The two new fixtures *are* byte-stable, which is exactly what
    `the_committed_reflow_fixtures_are_the_builders_own_bytes` now pins.
  - **Untracked fixtures fail a *different* guard first.** `tests/fixture_tree.rs` asserts nothing
    under `tests/fixtures/` is untracked, so the new files must be `git add`ed before the full
    suite can be green — the plain `cargo test` count is meaningless until they are staged.
  - Next startable: **R-4** (row A10's guard — the move itself), then **R-5** (the near-edge
    refusal). Both now have their fixture. T-8 (`Tz`-scaled kern) is still worth landing before
    R-4 asserts positions.

## Iteration 24 — 2026-11-01 — T-8 (the reader's kern term) — green

- Task: T-8 — give `tj_fragment_origins`'s kern term the `Tz/100` factor that `spaced_width`
  already applies to glyph advance and spacing, and prove it against the ink rather than against
  its own arithmetic.
- Test: `the_reader_puts_a_kerned_tail_where_pdfium_drew_it_at_tz_50` (red before: **yes** —
  "the reader puts `CD` at x=94.008 … PDFium drew it at 91.008 — off by 3.000 pt, 60x the 0.05 pt
  tolerance", exactly the `250/1000 x 24 x 0.5` the plan predicted) and
  `the_kern_term_carries_the_horizontal_scale` (red before: yes, at `85.008` against the expected
  `91.008` while the mutant was in place; note its first draft had the sign inverted and was red
  for that reason, which is why it was re-measured rather than assumed fixed).
- Files: `src-tauri/src/editor/textlocate.rs` (the factor, two tests),
  `docs/P2.8-reflow-DESIGN.md` (R10.1/R10.2 status), `.github/workflows/ci.yml` (both pins plus
  the comment that records why), `src-tauri/tests/ci_workflow.rs` (both pin assertions),
  `scripts/ralph/{gate.sh,PROMPT.md,IMPLEMENTATION_PLAN.md,LOOP_LOG.md}` (all four count carriers).
- Gate: cargo test **527 passed / 91 ignored**, AND `--include-ignored` **618 passed / 0
  ignored**, clippy clean (`-D warnings`), tsc clean, vitest 69 passed, `cargo fmt --check` clean.
  Both counts moved by the measured amount: 526→527 and 616→618, ignored 90→91. **Measured, not
  incremented** — `--no-fail-fast` with libtest's own totals summed.
- Commit: see `git log` (this entry is in the commit it describes).
- Learned:
  - **The plan's own measurement reproduced to the digit.** 94.008 against 91.008 is 3.000 pt,
    and `(-250)/1000 x 24 x (1 - 0.5) = 3.000`. That is worth recording because it means the
    assertion was written from a number someone else had already measured, and it still had to
    be run to know the *test* could see it — the two are different claims.
  - **Two guards, not one, and the reason is the CI asymmetry.** The acceptance test the plan
    names is `#[ignore]`d, so Linux never runs it; the defect is in the reader, which every
    consumer of `tj_fragment_origins` inherits, so a property covered only on Windows would be
    a coverage hole on the branch everyone runs. The hand-built sibling needs no library and
    keeps it in the plain suite. Both were mutation-verified with the factor deleted from
    `tj_fragment_origins` (the guarded file — `git diff --stat` named it), each caught, each a
    pair: restored, both pass.
  - **The reflow has the same omission, and fixing it was the wrong call here.** `reflow.rs`
    computes `kern_after_target` unscaled and its comment says so in as many words ("Everything
    here is unscaled text space: `Tz` does not enter"). That comment was true only while the
    reader shared the assumption. Measured after the reader fix, `reflow_tj_array` on
    `kerned-line-tz.pdf` with `AB -> ABab` still reports `moved_to = 136.704` — the `Tz 100`
    answer, on a `Tz 50` page. Filed as **T-8b** in the plan rather than fixed: R-4 is at
    `Tz 100`, nothing yet asserts a reflow position at any other scaling, and the change would
    move a number with nothing watching it. `shift` stays correct at `Tz != 100` by accident —
    it subtracts two quantities that carry the same unscaled kern — which is exactly the kind of
    agreement that stops being true when the other half is fixed.
  - **`scripts/mutate.sh` refused the second mutant run with exit 4, correctly.** The first
    mutation run had left the mutant compiled into the binary, so the PDFium test's second run
    executed a stale build. `touch`ing the file and re-running produced a real CAUGHT. Without
    exit 4 that green-adjacent summary would have been read as a survivor.
  - **The `-lt` floor was mutated upward as well as downward.** `527 -> 528` caught; `91 -> 90`
    caught. A floor only catches values below it, so a survivor at 528 would have been a
    direction-blind guard passed off as coverage (AGENTS.md's rule about equality vs floor).
  - **`cargo fmt --check` caught the new assertion before CI did**, in the exact shape R-H2
    records: the wrapped `let expected = …` was not rustfmt-clean. This is the third iteration
    in a row where fmt-first was load-bearing.
  - Next startable: **R-4** (row A10's guard — the move itself), then **R-5**. R-4 no longer has
    T-8 as a predecessor. **T-8b** and **R-H2** remain open and are not blocking R-4.

## Iteration 26 — 2026-11-01 — R-4 (row A10's move guard) — green

- Task: R-4 — write row A10's move assertions against R-3's committed fixture, so the row's
  "the rest of the line moves and nothing else" half exists as a test rather than a contract.
- Test: `a_wider_replacement_moves_the_rest_of_the_line_and_nothing_else` (red before: **yes** —
  transplanted onto `b260966` in a throwaway worktree it failed with "the write reports its tail
  at 184.056; the reader puts it at 186.936", short by exactly the 2.880 pt seam kern that R-1a
  later added to the prefix).
- Files: `src-tauri/src/editor/reflow.rs` (the test, 199 lines, plus `afm::text_width` for the two
  measured widths), `docs/P2.8-reflow-DESIGN.md` (status line, §0.1 acceptance row, §6.1 test
  cell, a new R-4 paragraph in §6.2), `.github/workflows/ci.yml` (ignored pin 91→92 plus the
  comment recording why), `src-tauri/tests/ci_workflow.rs` (the `ne 92` assertion and its
  explanation), `scripts/ralph/{gate.sh,PROMPT.md,IMPLEMENTATION_PLAN.md,LOOP_LOG.md}`.
- Gate: cargo test **527 passed / 92 ignored**, AND `--include-ignored` **619 passed / 0
  ignored**, clippy clean (`-D warnings`), tsc clean, vitest **69 passed**, `cargo fmt --check`
  clean. Both shapes measured with `--no-fail-fast` and libtest's totals summed. The +1 is
  **measured, not incremented**: the new test is `#[ignore]`d (it reads PDFium ink), so plain
  `passed` is unchanged at 527 while `ignored` 91→92 and `--include-ignored` 618→619.
- Commit: see `git log` (this entry is in the commit it describes).
- Learned:
  - **The fixture's line origin cannot be read from a glyph.** The first draft derived the
    expected tail position from `at(ink, 'A', 700.0)`, and it failed at 160.248 against 162.912
    because `A` occurs twice on the fixture (`ALPHA ` and `BETA`'s `A`) — `at` correctly refuses
    to guess, but the *expectation* had picked the wrong one. The fix is to take the line origin
    from the reader (`target.origin.0`) and assert `L`'s own offset from it, which is the number
    that cannot be ambiguous.
  - **The replacement must not contain letters the tail carries.** `ALPHABET ` would put a
    second `B`, `E` and `T` on the page, and an ink oracle that looks a character up then reads
    whichever it finds. `ALPHAab ` keeps every tracked glyph unique.
  - **Three mutations, guarded file each time, each a pair.** (1) `tail_pen` from `old_width`
    instead of `new_width` → CAUGHT. (2) The `escape_literal(replacement)` write deleted (the
    `if let` arm became `let _ = &bytes`) → CAUGHT, failing on the read-back assertion with
    `["ALPHA ", "BETA", "WXYZ"]`. (3) The splice range widened by one (`..op_index + 2`) so the
    write drops the operator after the array → CAUGHT, and it reported itself usefully as "got
    `('W', 249.6, 700.0)`" — the untouched line had been dragged onto line 1. `git diff --stat`
    named `src-tauri/src/editor/reflow.rs` in all three runs; restoring returned green each time.
  - **The mutation harness refused my first invocation, correctly.** `scripts/mutate.sh` prepends
    `cargo test --release`; passing `--manifest-path src-tauri/Cargo.toml` *and* `--lib` made the
    invocation malformed and it exited 3 (`no_summary`). The right form is `--lib -- <pattern>`,
    as iteration 25 recorded for `--test`. This is the second iteration in a row where a
    `cargo`-argument mistake looked like a measurement.
  - **The pin guard lives in `the_linux_job_does_not_force_pdfium_tests_without_a_library`, not
    `the_windows_job_asks_for_ignored_tests_and_counts_them`.** Mutating `ci.yml`'s `-ne 92` and
    running the *Windows* guard reported SURVIVED — a real coverage finding about my own aim,
    not about the guard. Re-running the Linux guard with the same mutant was CAUGHT, and so was
    the upward mutant (`-ne 93`), so the pin is direction-free as an equality should be.
  - **The design doc's §6.1 test cell was a lie the citation guard could not see.** It read "to be
    written with the guard (§5)" — a promise, not a citation, so `docs_citations.rs` passed.
    Naming the test is what makes it checkable; the paragraph in §6.2 records the red-before
    number and the three mutations so the next reader does not re-measure them.
  - **Row A10 stays `Missing` and the phase stays `[!]`.** R-5 (the refusal half) is not written,
    R-6 has no caller in the edit path, R-7 is forbidden without a confirmed installer (Q7/Q9).
    This iteration moves one row and closes nothing.
  - Next startable: **R-5** (the near-edge refusal, `fits-width`, stream byte-unchanged), then
    **R-6** (wire `reflow_tj_array` into the edit path). **T-8b** and **R-H2** remain open and
    block neither.

## Iteration 27 — 2026-11-01 — R-5 (row A10's refusal case) — green

- Task: R-5 — write row A10's refusal half against R-3's near-edge fixture: the refusal must come
  from *this page's content box* through the `fits-width` gate, and the stream must be
  byte-unchanged afterwards.
- Test: `a_replacement_too_wide_for_the_content_box_refuses_and_writes_nothing` (red before: **yes**
  — the behaviour did not exist; measured on the fixture, today's `reflow_tj_array` with the box's
  right edge as a limit refuses as `R4-wrap` with a message about wrapping, and there was no
  resolver at all, so `content_box` returned nothing). Two more guards:
  `the_content_box_is_the_crop_box_defaulting_to_the_media_box_through_the_page_tree` and
  `a_page_without_a_readable_box_refuses_instead_of_inventing_one` (both new code, so red before by
  absence rather than by assertion).
- Files: `src-tauri/src/editor/reflow.rs` (`reflow_line_within_content_box`, `content_box`,
  `inherited_box`, `parse_box`, `ReflowRefused::measured`, three tests),
  `src-tauri/src/editor/textlocate.rs` (`MAX_PARENT_DEPTH` now `pub(crate)`, one reason noted),
  `docs/P2.8-reflow-DESIGN.md` (status, §6.1 test cell, an R-5 paragraph in §6.2),
  `.github/workflows/ci.yml` (the `-lt` floor 527 -> 530 with the reason), `src-tauri/tests/
  ci_workflow.rs` (the matching assertion), `scripts/ralph/{gate.sh,PROMPT.md,
  IMPLEMENTATION_PLAN.md,LOOP_LOG.md}` (all four count carriers).
- Gate: cargo test **530 passed / 92 ignored**, AND `--include-ignored` **622 passed / 0 ignored**,
  clippy clean (`-D warnings`), tsc clean, vitest **69 passed**, `cargo fmt --check` clean. Both
  counts moved by the measured amount: 527 -> 530 and 619 -> 622; **measured, not incremented**
  (`--no-fail-fast`, summing libtest's own totals). `ignored` is unchanged at 92 because all
  three new tests call code rather than PDFium.
- Commit: see `git log` (this entry is in the commit it describes).
- Learned:
  - **The gate's own parity guard is what made "which file holds the pin" answerable, and the
    answer moved.** `ci.yml`'s floor is a `-lt`, so it is direction-blind by construction: only a
    *lowering* is caught by the property it claims, and a raising is caught only because the guard
    is an equality on the literal. Both directions were mutated (530 -> 529, 530 -> 531) and both
    were CAUGHT by `the_linux_job_pins_the_measured_passed_floor`. That is the AGENTS.md rule about
    floors applied rather than quoted: a floor under the measured count would pass on a lost test,
    so the number must equal the measurement, and `ignored` must stay put when no test is ignored.
  - **A message is part of a refusal, so the widths the decision compared must travel with it.**
    The first shape re-measured the two numbers at the seam with `FontMetrics::resolve`, which is a
    *second* measurement of the same quantity — the same class of defect as the doc comment that
    claims one thing while the code does another. `ReflowRefused` now carries `(needed, available)`
    from the point of the comparison, and a mutation that swapped `available` for the run's own
    advance (`old_width`) was CAUGHT. That number is exactly what R3 says changes.
  - **The malformed-box case is the trap the obvious implementation walks into.** Writing
    `inherited_box` to return `Option` — the signature the rest of this module uses — makes a
    present-but-unreadable `/CropBox` indistinguishable from an absent one, so the resolver falls
    through to the `/MediaBox` and silently widens the region text may enter. It is now
    `Result<Option<_>>` and the mutation that restored the fall-through was CAUGHT by the second
    assertion (the message names `CropBox`). §3.2 step 3 refuses to *invent* a box; this is the same
    refusal one level down, and it needed its own test because the "no box anywhere" test cannot
    see it.
  - **`export.rs`'s `media_box` is a real reuse hazard and the A4 case is what makes it visible.**
    It has a US-Letter fallback and does not walk the `/Parent` chain, so on an inherited A4 box it
    answers 612. The resolver's guard uses an inherited **595 x 842** box for this reason: a
    resolver that reused it would fail there, not on `reflow-line.pdf`, whose box is Letter either
    way. §3.3 says this in prose; the test is what makes the prose checkable.
  - **Six mutations, all in `src-tauri/src/editor/reflow.rs`, all CAUGHT and all restored green**:
    `/MediaBox` before `/CropBox`; the `/Parent` walk deleted; the malformed fall-through; the box
    replaced by `f32::INFINITY` as the limit; `available` reported as `old_width`; and
    `.normalised()` dropped. `git diff --stat` named the guarded file in each run. The first three
    are resolver properties and the last three are properties of the wrapper — which is also why
    the resolver got its own tests: the wrapper's guard cannot observe which box was chosen, only
    that *a* limit was applied.
  - **`scripts/mutate.sh` needed `--lib` for the unit binary and nothing else.** Every invocation
    was `--lib <test-name>`; passing `--manifest-path` as well (iteration 26's mistake) makes it
    exit 3. The pin guards were run with `--test ci_workflow <name>` and
    `MUTATE_RUNTIME_READ=.github/workflows/ci.yml`, which is the runtime-read class: `Compiling`
    is neither required nor evidence there.
  - **`MAX_PARENT_DEPTH` was `const` and private to `textlocate`; it is now `pub(crate)`.** Two
    independent "how deep is a page tree" bounds would be two answers to one question, and the
    content-box resolver walks the same chain for the same reason.
  - **Row A10 stays `Missing` and the phase stays `[~]`.** R-6 has no caller in the edit path and
    R-7 is forbidden without a confirmed installer (Q7/Q9). This iteration moves the refusal half
    and closes nothing.
  - Next startable: **R-6** (wire `reflow_tj_array`/`reflow_line_within_content_box` into the edit
    path, with D-2 and an inverse restoring the exact pre-edit bytes), then **U-2**. **T-8b** and
    **R-H2** remain open and block neither.

## Iteration 28 — 2026-11-02 — T-8b (the reflow's own `Tz`) — green

- **Task:** `reflow_tj_array` measured both its replacement width and its `TJ` kern term in
  unscaled text space, so on a `Tz != 100` page it reported a tail at the `Tz 100` answer.
- **Test:** `a_reflows_reported_tail_origin_matches_the_ink_at_tz_50` (`#[ignore]`d) and its
  no-library sibling `the_reflows_tail_position_carries_the_horizontal_scale`. **Red before:
  yes** — the plain sibling reported **136.704** against 104.352; the ignored sibling failed at
  the *ink* assertion (`C` drawn at **133.704** against 104.352), so the write really did land
  the tail where the wrong number said, not merely report wrongly.
- **Files:** `src-tauri/src/editor/reflow.rs`, `src-tauri/src/editor/textlocate.rs`
  (`RawRun::horizontal_scale` + walker accessor), `src-tauri/src/editor/textedit.rs` (test
  literal), `src-tauri/tests/ci_workflow.rs`, `.github/workflows/ci.yml`, `scripts/ralph/gate.sh`,
  `scripts/ralph/PROMPT.md`, `scripts/ralph/IMPLEMENTATION_PLAN.md`,
  `docs/P2.8-reflow-DESIGN.md`.
- **Gate:** `cargo test` **531 passed / 93 ignored**, AND `--include-ignored` **624 passed / 0
  ignored**, clippy `-D warnings` clean, `cargo fmt --check` clean, tsc clean, vitest **69
  passed**. Counts measured with `--no-fail-fast` and summed from libtest's own totals, then set
  in all four carriers in this commit.
- **Commit:** see `git log` (this entry is in the commit it describes). `scripts/ralph/gate.sh`
  confirmed green after the pin move: 531/93 plain, 624/0 ignored, fmt, clippy, tsc, vitest 69.
- **Learned:**
  - **The task text named half the defect.** It said the kern term omitted `Tz`; the *replacement
    measurement* omitted it too, and that is the larger half — 29.352 pt against 3.000. Fixing
    only `kern_after_target` would have left the acceptance test red at 133.704 and read as "the
    fix did not work". When a task names one arithmetic omission in a function, check every other
    `* Tf` in the same function before writing the fix.
  - **`shift` was correct for the wrong reason and is now correct for the right one.** It
    subtracted `run.origin.0 + old_width + kern` from `moved_to`, and before the fix both sides
    carried the same unscaled kern, so the difference was right by cancellation. The plain guard
    asserts `shift == scaled_growth` independently, so the two halves cannot cancel into a pass
    again.
  - **A `Tz 0` page reaches `measure_scaled` as `scale == 0.0`, not as a `None` metrics answer.**
    Returning `Some(0.0)` there would satisfy `new_width <= old_width` and refuse as **R1**, whose
    message names the wrong remedy ("not wider") for a page whose advance axis has collapsed.
    `measure_scaled` returns `None` in that case instead, which routes to the existing
    `NotOneArray` refusal rather than inventing a gate this module does not own — `DegenerateText`
    is `textedit`'s. No test drives this today; recorded rather than papered over.
  - **Mutation evidence, all in `reflow.rs` (`git diff` named it each time, `cmp -l` confirmed
    the bytes changed):** dropping `* scale` from the width (CAUGHT by the plain sibling), from
    the kern term (CAUGHT), and the width again against the `#[ignore]`d sibling (CAUGHT). All
    restored and re-run green — each is a pair, not a single red run.
  - **The `ci.yml` pins were mutated in the guarded file, not the guard.** The ignored pin is an
    equality, so it was mutated **both** ways (**93 -> 92** and **93 -> 94**); the `passed` floor
    was mutated **down** to 530. All three were CAUGHT by
    `the_linux_job_does_not_force_pdfium_tests_without_a_library` — the Linux guard, *not*
    `the_windows_job_asks_for_ignored_tests_and_counts_them`, which reports a false SURVIVED for
    this pin (the memory note from `ddd9615`). Restored: 8/8 green.
  - **`scripts/ralph/gate.sh` refuses to run while it is dirty**, so the pins had to be *edited*
    here and the six commands run individually first. They all passed; `gate.sh`'s own run is the
    post-commit confirmation, not a pre-commit substitute.
  - Next startable: **R-6** — still the only path to closing row A10, and it still needs the
    §7.4 inverse-record ruling before it can be written test-first (a `ReflowOperation` that undo
    replays must have a persisted shape, and U-2 is blocked on the same ruling). **R-H2** is
    mechanically already satisfied — `gate.sh` runs `cargo fmt --check` at line 115 and
    `gate_parity.rs` asserts it cannot be exempted — but its task text still says `[ ]` and its
    named test `the_gate_runs_every_check_ci_runs` does not exist under that name; left for the
    user to close rather than checked on a name that does not match.

## Iteration 29 — 2026-11-02 — R-7 (row A10 `Missing → Have`, tag `v1.0.0`) — green, phase closed
- Task: promote A10 on the strength of a hand-tested installer, tag the confirmed commit, publish
  as a full release, and move the dev tree to 1.0.1 so the tree-ahead-of-tag invariant holds.
- **Release authority was granted by the user**, who hand-ran the installer built from dispatch
  `41461007177` (`artifact 13149701933`, `Document Reader_1.0.0_x64-setup.exe`, 10,375,381 bytes,
  `af18fca8…c57b9`) and reported all checks pass, including the A10 move case observed live.

### What was done, in order
1. **Tag target verified before creating it.** `41461007177` ran at `1e6bfd8`, not at HEAD. Tagged
   `1e6bfd8`, per the 0.9.0 provenance rule. The two docs-only commits between it and HEAD
   (`7d3c661`, `e64c805`) are recorded **in the tag message** as the staleness delta, with the
   reason they cannot change the artifact: neither touches `src-tauri/` or `src/`, and the crate's
   only `include_bytes!` is the font payload. Eliding this would have made "tag the confirmed
   tree" and "tag HEAD" indistinguishable from outside.
2. **The acceptance test did not exist, so it was written.** `grep` found
   `the_parity_matrix_promotes_a10_only_with_a_recorded_installer` only in the plan that named it.
   Added to `src-tauri/tests/release_version.rs`: a promoted A10 row must name the dispatch, the
   artefact id and the tag, and the dispatch id must also appear in `RELEASE-NOTES.md`.
3. **Two mutants, both CAUGHT, both restored:**
   - dispatch id stripped from a promoted row → *"A10 is promoted to `Have`, but its row does not
     name 41461007177"*;
   - dispatch id removed from the notes while the row kept it → *"the hand-test provenance would
     exist only in the matrix cell that depends on it"*. This is the load-bearing half: without
     it a promoted row citing a build nobody wrote down is unfalsifiable from the tree.
4. **The guard cannot know a human ran the installer** — that evidence is on the user's machine.
   It refuses the *shape* where a row is promoted with no provenance recorded, which is the
   failure mode that survives review. Said plainly in the doc comment so it is not mistaken for
   more than it is.
5. **Counts moved +1 in both shapes: 524/93 · 617/0 → 525/93 · 618/0.** Ignored pin unchanged at
   93, which is the check that no PDFium-backed test changed shape. Four carriers updated:
   `gate.sh`, `ci.yml` (floor `-lt 525` and the comment), `ci_workflow.rs` (the assertion string
   and its message), `PROMPT.md`. A fifth carrier I deliberately did **not** touch: the counts
   table in `RELEASE-NOTES.md` documents the *shipped 1.0.0 build*, measured before this test
   existed. Rewriting it would falsify the record of what was hand-tested.
6. **`gate.sh` refused to run while it was dirty** — its own guard ("the pins would not be the
   committed ones"). Counts were therefore measured and the pins edited first, committed, then
   gated. Same sequence as iteration 28; it is the script's design, not an accident.
7. **1.0.1 bump**, driven by an observed red rather than a prediction:
   `the_tree_version_is_ahead_of_every_published_tag` failed the moment `v1.0.0` existed, saying
   the carriers `1.0.0` is "not ahead of the highest published tag v1.0.0". All six carriers moved
   (`Cargo.toml`, `package.json` ×2 in `package-lock.json`, `tauri.conf.json`, `Cargo.lock`). The
   dependency version strings that merely look like ours — `miniz_oxide 0.9.1`, `0.9.12` — were
   left alone by line-addressed edits, then checked.
8. **`## 1.0.1` section added**, mirroring the `## 0.9.1` precedent: development version, no
   build, reserved for R5. The R5 citation is `docs/DECISIONS.md` **§9 item 5** — there is no
   "§9.5" heading in that file, and `docs/P3-PLAN.md` already schedules R5 as 1.0.1.

### Row state
- **A10 → `Have`** in `docs/P2.7-TEXT-EDITING-PARITY-PLAN.md` with the dispatch, artefact and tag
  in the cell; in `docs/P2.7-STOCK-TAKE.md` as a dated *supersession* rather than a silent
  rewrite, because that file is a dated audit and what it said at 0.6.0 was true then.
- **Overflow UX filed, not fixed.** The `fits-width` refusal quoted both widths correctly when
  the box had no room; the user judged the behaviour right and the *presentation* worth a future
  row. Recorded as such; nothing in P2.8 waits on it.
- **P2.8 phase row → `[x]`**, both former blockers (§7.2, §7.3) recorded resolved and the undo leg
  recorded as landed (U-2). The H4 list is fully ticked; its governing instruction is kept
  verbatim under a `Closed` line so the next phase list inherits it.
- `docs/P2.8-reflow-DESIGN.md` status → **shipped**, with the tag and the hand-test date.

### Verdicts
- **CI, tag `v1.0.0` @ `1e6bfd8`:** run `41508308720` — success (all three jobs).
- **Release `v1.0.0`:** workflow run `41508308763`, triggered by the tag push, publishing as a
  full release (`prerelease: false`, guarded by `a_tag_publishes_a_full_release_since_1_0_0`).
- **Gate:** `525 passed / 93 ignored` and `618 / 0`, reconciling (525 + 93 = 618).

### Next
Nothing in P2.8. **Halted here by instruction.** P2.8 closes with this turn; the next briefing
names what follows. 1.0.1 is reserved for the R5 rename and no P3 work may take that number.

---

## Iteration — 2026-11-03 — R5 rename RELEASED as `v1.0.1`; tree to 1.0.2; halt

Release-authority turn. Scope was release bookkeeping only: tick the rename row, cut the tag at
the confirmed commit, verify the published release, bump the dev tree, halt. **No P3.1 row was
touched.**

### What the tag points at, and why

`v1.0.1` → **`330a4ad`**, not `HEAD`. `HEAD` was `dd1370e`, eight commits ahead. The rule is the
0.9.0 provenance rule: the release asset must be built from exactly the source the tag points
at, because the whole point of "tag the confirmed tree" is that the confirmed bytes and the tag
are the same claim.

The verified chain, each link checked rather than assumed:

- dispatch `41651299040` (workflow_dispatch, build-only) ran at `330a4ad` — `success`
- artifact `13237103980` `document-reader-windows-installer` contains
  `DocuPro_1.0.1_x64-setup.exe`, 10,359,989 bytes,
  sha256 `b7cbe7eb11c9c72052c1c25ec5cb20f04934d58e406bb1d5c8c2a769decab03d`
- the filename and product name are the ones the user hand-tested — DocuPro, one word
- CI at `330a4ad` was `41651291111` `success` before the tag was cut
- `git rev-parse v1.0.1^{commit}` == `330a4ad…aa19`, compared as full SHAs, not prefixes

**Staleness delta recorded in the tag message, not elided:** `c41eb48`, `927c89d`, `a7a189c`,
`7615a1d`, `479d1b4`, `dd1370e`, plus `d74af2f` and `66334d3` — all P3.1 annotation work, none in
the artifact. The user hand-tested a build that predates them, so promoting HEAD would have
tagged a tree nobody ran.

### The row

R5 had **no tickable row anywhere**: `docs/P3-PLAN.md` §1 is a prose table with no tick column,
and `IMPLEMENTATION_PLAN.md` tracked it only inside a P2.8 entry as a forward reference. Added
`## H4b. R5 rename housekeeping → 1.0.1` with the row ticked and the hand-test evidence inline —
dispatch, artifact, installer name/size/sha256, tag, and both guard names. Also:
`docs/P3-PLAN.md` §1 row marked ✅ with the evidence, and the design doc status moved
`DESIGN — APPROVED, implementation in progress` → **`SHIPPED 2026-11-03`**.

The tick rests on rule 1, not on the gate: the gate was already green when the installer was
dispatched. What closes the row is a human running the installer, and the four checks the user
reported — name, Add/Remove double entry, migration notice, library survival — are the ones the
design's §6 item 6 names.

### The tree bump

1.0.1 → **1.0.2** across all six carriers: `package.json`, two in `package-lock.json` (root and
the `packages[""]` entry), `src-tauri/tauri.conf.json`, `src-tauri/Cargo.toml`,
`src-tauri/Cargo.lock`. Then `## 1.0.2` written into `docs/RELEASE-NOTES.md` mirroring the
`0.9.1`/`1.0.1` precedent.

This bump is **forced, not cosmetic**, and the guard states why: `v1.0.1` now exists and `HEAD`
is untagged, so `the_tree_version_is_ahead_of_every_published_tag` requires the carriers to name
something strictly higher. Left at 1.0.1 it is unsatisfiable by construction for every commit
after the tag — the same failure it reported the moment `v1.0.0` was pushed.

**1.0.2 is RESERVED for the first user-facing P3.1 release and for nothing else.** Written into
the notes with both reasons: a patch bump is what any small intervening change would reach for,
and it would spend the number the first installable P3.1 artefact should carry. If the phase
schedule holds at 1.1.0 the number is simply skipped.

### Verdicts
- **Gate at 1.0.2:** `553 passed / 93 ignored` and `646 / 0`, reconciling (553 + 93 = 646);
  fmt, clippy, tsc, vitest 80 green.
- **Tag `v1.0.1` @ `330a4ad`:** pushed. Release run `41685774608` and CI `41685774595` both
  dispatched by the tag push.
- **Release asset:** `DocuPro_1.0.1_x64-setup.exe`, published, 10,360,210 bytes. Release
  `v1.0.1` is `draft: false`, `prerelease: false`, published 2026-11-03T18:04:33Z.
- **Rebuild delta found and recorded, not waved past.** The published asset is **not** the bytes
  the user tested: the tag run rebuilds rather than re-attaching the dispatched artifact, so it
  is 221 bytes larger, sha256 `d940a12f…79da73` against the tested `b7cbe7eb…decab03d`. Same
  source commit, same `productName`, same `1.0.1` resource — NSIS non-determinism. `v1.0.0` has
  the identical delta (published `de78e8f3`, recorded `af18fca8`), so this is a standing property
  of the pipeline, not a defect introduced here. Written into the release notes because a
  mismatched sha256 reads as tampering until it is explained.

### Next
**HALTED by instruction.** P3.1 continuation — A-4 (ink `/AP`) onward — gets its own pulse.
Carried forward from A-3: A-4 should expect the same `/DR` requirement if its appearance names a
font, and the palette tests pin `Line`/`Arrow`/`Oval` as *absent*, so A-5 will turn one red by
design.

## P3.1 A-4 → A-6 (drawing rows), 2026-11-04

### Landed
- `3a36f34` **A-4 ink `/AP`** — CI `41833314510` green, no skipped steps.
- `89e1a1f` **A-5 line + arrow** — CI `41836449605` green.
- `3105e8f` **A-6 oval** — CI `41842746779` green, no skipped steps.
- `4d13e77` A-5 follow-up (`/LE` = `OpenArrow`) + plan rows ticked — CI `41843950200` green.
- `90c128e` test hygiene (panic-safe temp cleanup) — CI `41846674278` green.

### The two defects this pulse found that no test was going to find
1. **`/LE` was `Arrow`.** Not a member of Table 179's closed value set, so every viewer would
   have ignored it and drawn a plain line — the exact failure the key exists to prevent, in the
   row whose whole point is the arrowhead. The test asserted whatever the writer wrote, which
   cannot catch a wrong constant; it was found by re-reading the table against the plan row.
   The test now asserts the spec's spelling.
2. **Oval was reachable in the palette before it was drawable.** It was added to
   `EditorPane`'s list without a `DRAG_TOOLS` entry or a blob branch, so the gesture was never
   captured and, once captured, would have written a `rects` blob the shape writer rejects.

### Mutation record (honest, including the survivors)
- A-4: 4 mutants, all caught *after* re-aiming. Two survived initially because `S`→`f` and a
  widened `/BBox` hit **free text's** builders — the strings are shared, so the mutation landed
  in the wrong function. A third survived for a real reason: the test asserted the substring
  `"40"`, which page-space `640` also contains, so it passed on the bug it existed to catch.
- A-5: M8 (structural flatten skip removed) **SURVIVED and is left surviving** — `appearance_for`
  returns `None` for line, so the drawn-guard excludes it anyway. The skip is kept as defence in
  depth and the plan row says it is not claimed as protection. M6/M7/M9/M10/M11 caught.
- A-6: M13 (kappa → 1.0) **survived the first test** — quadrant points and curve count do not
  distinguish an ellipse from a bulged quadrilateral. Test now pins the operator line; re-run
  caught. M12 caught.
- Temp guard: the `Drop`-deletes-nothing mutant **survived** against the real test (cleanup is
  invisible to it), which is why `the_temp_workspace_removes_itself_when_it_goes_out_of_scope`
  exists. Re-run caught.

### Known, not fixed
- Five other temp-dir sites (`src/library.rs`, `src/provider/text.rs`, `tests/export.rs`,
  `tests/support/mod.rs`, `tests/type0_row6.rs`) leak one pid-named directory per run. Bounded,
  unlike the per-failure leak just fixed, so left for their own change.
- An intermittent `STATUS_ACCESS_VIOLATION` in the Windows PDFium suite remains undiagnosed
  (same-SHA re-runs pass).

### One red gate, unexplained
A single gate run reported `gate red` with no failing step in its log; six subsequent full runs
and five direct suite runs were green. I could not reproduce it and did not find a cause, so it
is recorded as unexplained rather than attributed to a flake. My first read of that run was
wrong in a way worth noting: `... | tail -3; echo $?` reports *tail's* status, so `EXIT=0` said
nothing about the gate.

### Next
**A-7 comment pane** (statuses, `/State`, one undo entry per status change) per
`IMPLEMENTATION_PLAN.md` H5.

---

## P3.1 A-7 → A-9 (XFDF parity), 2026-11-04 — green, four commits pushed

Rows landed: A-7 (`1ef587a`), A-8 (`1ef587a`), A-9 (`1ef78d3`) and the A-9 save-dialog
follow-up (`d5404b8`). All four ticked in `IMPLEMENTATION_PLAN.md` with their evidence.
A-10 checked and **left unticked** — see below.

### The one that mattered: D-A10 was never implemented

`skipped_existing` was on the import report struct and was never incremented. A second import
wrote duplicate rows and reported a clean run. That is the exact defect D-A10 exists to
forbid, sitting behind a field name that reads as if it were wired.

It was found by writing the row's NEW ASSERTION, not by reading the code. The lesson is not
"review harder"; it is that a struct field with the right name is not evidence that anything
writes to it, and an assertion phrased as *a second import adds zero rows* finds that in one
run where a code read did not.

Three more bugs fell out of the same assertions, all invisible to a compile check: quick-xml
delivers an entity as its own event, so treating text nodes as the whole value silently
dropped `&amp;`-style content from every note; trimming text nodes then ate the spaces either
side of an entity; and XFDF `State` values were compared against the blob's lowercase spelling,
so any status from a real file was dropped. A fourth: `line` geometry was read from a file but
never written to one.

### Mutation record (honest, including the non-appliers)

Fifteen mutants, M26–M40, all caught. But three of them *initially reported a verdict against
unchanged code* — a blank-line mismatch and two Python `SyntaxError`s meant the substitution
never landed, and the harness dutifully reported SURVIVED (or "caught" by a test that happened
to already fail). Each was re-run until the edit was confirmed present in the file before the
verdict was believed.

This is the A-4/A-6 lesson in a new dress. There, the mutant applied but landed in the *wrong
shared function*; here it did not apply at all. Both produce a number that looks like a
measurement and is not. The habit that catches both: assert the substitution landed
(`assert old in s`, then read the file back) and re-run.

The save-dialog row's mutant — the cancellation check removed — was caught, and that row's
test (`writes nothing and claims nothing when the save dialog is cancelled`) exists because the
old shape reported "Exported 2" whether or not a file was written.

### A-10 left unticked, on purpose

`index_text` has no kind filter, so a `FreeText` row is indexed exactly as a `Note` row is, and
`annotation_hits_are_tagged_and_carry_their_id` proves the hit carries page and id. The
*behaviour* therefore looks right.

The assertion as written names textbox, callout and typewriter, and no test indexes any of
them — the only hit test inserts `AnnotationKind::Note`. Ticking on "the path is shared so it
must work" is precisely the reasoning that produced the D-A10 defect one row earlier. Recorded
in the plan row instead, with what is proven and what is not.

### Process failure this iteration: the work was not pushed

Four commits sat local after the row work, with no CI run against any of them, because I halted
for review without pushing. The per-row rule is push-then-watch, and a halt is not an exception
to it — a halt with unpushed work means the next person inherits untested commits and no
verdicts.

Corrected: pushed `3c4c8a4..d5404b8`, run `41931156511`, all three jobs green —
Frontend `success`, Rust (fmt, clippy, test) `success`, Windows compile check + render smoke
`success`. The Windows job's own guard reports `PDFium tests ran: 701 passed, 0 ignored`.

**701 vs the local 702 is expected and is not a silent skip.** The difference is exactly one
test, `row1_the_embedded_subset_renders_through_poppler`, gated `#[cfg(unix)]` because poppler's
CLI is a Unix install here. It is absent from the Windows binary by static `cfg`, which is why
`--include-ignored` cannot turn it into a failure there — a static platform gate, not a skip
arm that a green suite could hide behind. Checked by counting the `cfg` attributes (there is
exactly one) rather than by assuming.

### Brief handling — the lesson to carry forward

The conversation's INITIAL message is a **2026-10-29 state header**, not the current brief. It
was stale by roughly four days and forty commits: it named `3942c8c` as HEAD and `f9c75d5` as
unpushed (both already ancestors of origin), claimed the newest tag was `v0.8.1` when `v1.0.1`
existed, described the shipped 0.9.0 notes as a draft, listed a T1 deletion whose symbol no
longer exists in the tree, and named P2.7.7 as next when it had already landed.

**The current brief is always the NEWEST user pulse.** When context compaction re-surfaces
the initial message, it arrives looking authoritative and will drive task selection toward
work that is already done. The discipline that worked: verify every claim against git reality
before acting on any of it — `git log`, `git status`, `git rev-parse origin/<branch>`,
`git merge-base --is-ancestor <sha> origin/<branch>` (a SHA existing does not mean it is
unpushed), `git for-each-ref --sort=-creatordate refs/tags` (not `--sort=-v:refname`), and the
GitHub runs API for CI. Treat the newest pulse as authoritative; treat the header as a
historical document.

### Next

A-10 (one test per text-bearing type, or reword its assertion), then P3.1's remaining rows.
**Version ruling is PENDING at the user level**: 1.0.2 is reserved, the plan table says 1.1.0
for P3.1. No bump and no tag until the user answers — the release-prep pulse follows that answer.

---

## P3.1 A-10 (search parity, D-A12) — green, P3.1 task list complete

Landed `4e36346`, CI `41939678096` (Frontend success, Rust success, Windows success). A-10 was
the last unchecked row in H5; with it ticked the P3.1 task list is complete.

### The row was not a formality, and my 2026-11-04 reading of it was wrong

Yesterday I checked A-10, decided the behaviour "looks right" because `index_text` has no kind
filter, and left it unticked for want of tests. The tests found two defects that reading could
not have:

1. **`snippet(AnnotationSearch, 2, …)` read the wrong column.** `AnnotationSearch` is
   `(annotation_id, document_id, page, content)`; `content` is column **3**, not 2. So every
   annotation hit in the product displayed its *page number* as its snippet — non-empty,
   plausible-looking, and not the matched text. The document table is a column shorter, which
   is why the number 2 was right there and wrong here.
2. **The UI dropped `annotationId`.** `onOpenHit(entry, hit.page)` — a hit inside a note
   jumped to the correct page and selected nothing. Half of D-A12, and it looked complete
   because the page was right.

Both are the A-9 shape again: the visible half working hides the other half not. Recorded
because the 2026-11-04 note asserted the opposite, and a future reader should not trust it.

### Assertion 1 is also its own guardrail

The kind list is written out (`Note`, three `FreeTextMode`s, a commented highlight) rather than
generated from `AnnotationKind::as_str()`. A loop over the enum would pass today and pass again
when a fourteenth kind arrives with no indexing path, because it would silently join the loop.
Enumerating means a new kind lands here as a decision to make.

### The cross-document ordering bug, and why only one test catches it

Opening a search hit changes document *and* asks for a selection in the same render. The
provider already had an effect that resets `selectedId` to null on document change. Effects run
in declaration order, so the new selection effect must be declared *after* the reset, or the
reset lands last and clears the row the user just clicked.

The reordered mutant is caught by exactly one of the two provider tests — the cross-document
one. A single-document test stays green through it. This is the same lesson as A-4's shared
string builders and A-6's `kappa`: a mutant only proves something if a test exercises the
configuration where the two orders differ.

### Mutation record

| Mutant | Outcome |
|---|---|
| `snippet(AnnotationSearch, 3` → `2` | CAUGHT (2 tests) |
| `onOpenHit(…, hit.annotationId)` → `null` | CAUGHT (1 test) |
| selection effect body gutted | CAUGHT (2 tests) |
| selection effect moved before document reset | CAUGHT (1 test, cross-document only) |
| document hit returns `previous` instead of `null` | CAUGHT (1 test) |

Each substitution was asserted present in the file before the verdict was read. The Rust
mutant ran through `scripts/mutate.sh`; the four frontend ones are vitest failures, where the
harness's compile-step hazards do not apply but the "did the edit land" check still does.

### Gate

Plain **611** passed / 93 ignored (pins moved 609→611). `--include-ignored` **704** / 0 (pins
moved 702→704; the unchanged 93 is pinned too, so a suite that started skipping fails instead
of passing). vitest **113** across 8 files. `cargo fmt --check` and clippy `-D warnings` clean.
Windows CI reports **703** passed, 0 ignored — local 704 minus the one `#[cfg(unix)]` poppler
test, with both new Rust tests named `ok` in the Windows log rather than assumed.

One correction during the work: my first commit message said vitest 115. The real count is 113,
and it was amended. A test count in the record that does not match the run is the same class of
error as an unpushed commit — the artifact disagrees with the claim.

### Next

P3.1's task list is complete. Phase close needs the installer hand-test (rule 1), which is not
mine to do. Release version is a user decision and stays pending: the plan table names 1.1.0
for P3.1, while 1.0.2 is reserved and never dispatched.
