# Ralph loop log

Append-only. One section per iteration. The loop reads the tail before starting, so keep the
most recent iteration last.

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
- Commit: see below.
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
