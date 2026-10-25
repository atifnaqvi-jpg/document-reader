# Implementation plan — consumed by the Ralph loop

Source of truth for what to do next. The loop picks the **first unchecked task** whose
dependencies are met, does it, proves it, and checks it off.

Ruled decisions live in `docs/P2.7-REVIEW-OPEN-QUESTIONS.md` §F. Where this file and that
document disagree, that document wins.

Status legend: `[ ]` todo · `[~]` in progress **or gate-green-but-awaiting-installer confirmation** · `[x]` done · `[!]` blocked (needs a ruling)

Ruling **Q9** (2026-09-20): a phase is `[x]` only when its gate is green **and** the user has
confirmed the installer. Until then it is `[~]`. An untested phase must not be built upon.
Ruling **Q7**: one minor version bump per phase; **1.0.0 is reserved for P2.8**.

---

## H0. Rotation hotfix (B-3 — before any programme phase)

Separate commit. Revertible without touching the programme. Ruled: refuse now, transform later
under P2.7.4.

- [x] **H0.1** Add `Gate::RotatedText` (`rotated-text`) and refuse runs whose text matrix is not
      axis-aligned. Detect from `TextMatrixWalk.tm`: a run is rotated when the matrix has a
      non-zero off-diagonal component (or the horizontal scale vector is not parallel to the
      x-axis) beyond a small tolerance.
      Acceptance: a `Tm` of `[0 1 -1 0 300 100]` yields a run that refuses with
      `rotated-text`; an unrotated page is unchanged.
- [x] **H0.2** Ensure the refusal is *visible* rather than a silent skip: the run is still
      returned (so the picker can show it) and carries the gate name, so the UI can explain it.
      Acceptance: the existing gate-plumbing test pattern covers the new name.
- [x] **H0.3** Record the limitation in the plan (A7 disposition). Done: the gate message
      names the reason, `P2.7-TEXT-EDITING-PARITY-PLAN.md` A7 points at P2.7.4.
- [x] **H0.4** Commit as `fix(p2.7-hotfix): refuse rotated runs instead of measuring them on the
      wrong axis`.

## H1. Close P2.7.1 at 0.5.0

- [x] **T-1 (B-5) `Ts`.** Add `rise` to `TextMatrixWalk`, reset on `BT`, set by `Ts`, expose on
      `RawRun`, and use it in pairing (a raised run sits on a different baseline). Does **not**
      change `advance`.
      Acceptance: a fixture with `Ts 5` pairs to a box offset by 5pt; `advance` is unchanged.
      **Done.** `a_raised_run_pairs_to_the_box_offset_by_its_rise` fails if pairing reads the
      pen instead of the ink (`left: 0, right: 1`), so it is not a test that cannot fail.
- [x] **T-2 (B-6) PDFDocEncoding.** Replace `byte as char` with the real PDFDocEncoding table
      (per ISO 32000-1 Annex D, table D.2 — **not** the "Latin-1 plus 0xA0–0xFF exceptions"
      this line originally claimed, which is wrong: only `0xA0` is exceptional).
      If a byte cannot be decoded, treat the `ActualText` as absent and set
      `actual_text_preserved = false` rather than rewriting a different string.
      Acceptance: a fixture whose `ActualText` contains `0x80` decodes to `•` (bullet), not
      U+0080; an undecodable byte refuses to rewrite.
      **Done.** The table is exhaustive-tested byte by byte (`pdfdocencoding_table_matches_the_standard`),
      cross-checked against qpdf/poppler. Undefined codes are `0x7F`, `0x9F`, `0xAD` — not
      `0xA0`, which is the euro sign. `undecodable_actualtext_is_reported_not_rewritten` fails
      independently of the decoder change if the refusal branch is removed.
- [x] **T-3 (B-7) Copy-on-write for named `/Properties`.** Clone the named entry, repoint the
      `BDC`, rewrite the clone. Fall back to disclosure only if the clone cannot be made.
      Acceptance: two `BDC`s sharing `/P0` — editing one updates only that one; the other's
      `ActualText` is byte-identical after save.
      **Done.** `a_shared_named_property_is_cloned_not_rewritten_in_place` proves it in memory
      and again after `Document::save` + reload; `revert_removes_the_property_clone` proves undo
      takes the clone out (red if the cleanup block is disabled). Fresh key is `<name>_edit<N>`;
      the clone rides on the inverse as `propertyClone`, so undo repoints the `BDC`, drops the
      key and drops the object — an edit-then-undo leaves no orphan.
      **Reader bug found and fixed:** `Document::get_page_resources` returns the dictionary only
      for an *inline* `/Resources`, so on the usual file (indirect `/Resources`) no named
      `/ActualText` was visible to the locator at all. Reader and writer now share
      `page_properties_dict`.
- [x] **T-3b Regression fixture gap.** The P2.7.1 suite never exercised a page whose
      `/Resources` is an **indirect object** — the form real producers write — which is how the
      reader bug above stayed invisible. Audit the other page-resource readers (font lookup,
      `ensure_substitute_resource`, XObject lookup) for the same inline-only assumption and add
      an indirect-`/Resources` variant of an existing fixture to each.
      **Done.** New fixture `indirect-resources-inherited.pdf`: `/Resources` on the `/Pages`
      node, page 1 shadowing `/F1` with an inline dict, pages 2–3 inheriting one shared object.
      Audit result: the *read* paths (`get_page_fonts`, `FontMetrics::resolve`,
      `embedded_font_program`, `simple_widths`, `properties_slot`) all handle both forms; **two
      writers did not** and are fixed here:
      (1) `ensure_substitute_resource` wrote into `get_page_resources().1.first()`, i.e. the
      first *referenced* resources object up the `/Parent` chain. On a shadowing page that is
      the ancestor's dict, which the page cannot resolve — the edited run named `FS1` and
      stopped rendering; on an inheriting page it mutated an object two pages read (B-7
      violation). It now resolves the slot (`writable_resources_mut`) and gives the page a
      private copy of what it inherits.
      (2) `export::watermark` read only the page's *inline* `/Resources`, so on an inheriting
      page it built a fresh dict holding just its own font — dropping every font and XObject the
      page resolved — and reused the name `/F1`, redrawing the page's own text in Helvetica. It
      now uses the same resolver and its own name, `WmkFont`.
- [x] **T-4** Version bump to 0.5.0 (`Cargo.toml`, `package.json`, `tauri.conf.json`), release
      notes listing `Ts`, PDFDocEncoding, copy-on-write, and the MCID work.
      **Done.** Six carriers agree (`Cargo.toml`, `Cargo.lock`, `package.json`, both
      `package-lock.json` entries, `tauri.conf.json`) and `docs/RELEASE-NOTES.md` carries the
      0.5.0 section. `src-tauri/tests/release_version.rs` is the guard, because the checklist
      had already failed silently: `package-lock.json` was still at **0.2.1** while every other
      carrier said 0.4.5, which builds an installer named one version that reports another.
      `the_0_5_0_release_notes_cover_every_change_the_plan_claims` fails if any of the four
      shipped changes goes unmentioned.
- [x] **T-4b Release tag does not trigger the installer.** `.github/workflows/release-windows.yml`
      triggers on tags matching `v*`; both real releases were tagged `version-0.4.4`, which does
      **not** match. The 0.4.4 build fired only because the tag pointed at the branch head that
      was pushed in the same event; a `version-*` tag on any other commit gets no build and no
      release asset. Fix the glob (or adopt `v*` tags) before tagging 0.5.0.
      **Done — but the diagnosis above was wrong, and the fix is the opposite one.** `v*` *does*
      match `version-0.4.4`: GitHub's filter syntax says `*` matches any run of characters, and
      its own table spells out that `v2*` "matches branch and tag names that start with `v2`"
      (workflow-syntax § patterns-to-match-branches-and-tags). The 0.4.4 evidence agrees twice:
      the workflow has no `branches:` trigger, so its `event=push` run can only have come from
      the tag push, and the installer asset was published, which needs the ref to start with
      `refs/tags/v`. What was actually broken is that `v*` is too **wide** — it matches any tag
      starting with the letter v, and the attach guard (`refs/tags/v`) then publishes a release
      for each, so a stray `verify-ci` tag ships an installer named after itself. Trigger is now
      `v[0-9]*` + `version-[0-9]*` (both conventions, nothing else); the attach guard is
      `refs/tags/` (tag vs branch only — GitHub expressions cannot test the tag *name*), so the
      trigger globs are the single place that decides which tags are releases.
      `src-tauri/tests/release_workflow.rs` pins trigger, guard and the glob matcher itself.
- [x] **T-5** Commit the P2.7.1 body (code + fixtures + docs) as one feature commit, separate
      from H0.
      **Done — but the body's *code* was already committed** (2cb2f2f, 64ede4c, d187849, 31d523b,
      647b436, cd8321e), so the remaining work was the part no task had ever checked: whether the
      body's documents describe the code they ship. Three did not.
      (1) `P2.7.1-mcid-DESIGN.md` §4 cited `page_properties_dict`, a helper that does not exist;
      the resolver is `properties_slot`. (2) §8's acceptance table named
      `shared_actualtext_is_reported_not_rewritten`, which ruling B-7 **renamed** to
      `shared_actualtext_is_copied_before_it_is_rewritten` — and the rename took the T30
      *inline* multi-operand case with it, so an invariant the contract claims had no test.
      Restored as a real test on a new `actualtext-shared-inline.pdf` fixture (two `Tj` in one
      `/Span`); it is not vacuous — deleting the `span_is_this_operation` check fails it.
      (3) `P2.7-STOCK-TAKE.md` still reported `Ts` as absent and A7 as silently wrong after T-1
      and H0 shipped, and still listed "commit P2.7.1" as outstanding; `P2.7-TEXT-EDITING-PARITY-PLAN.md`
      still called A2/A3 **Missing** and dated its status to "uncommitted P2.7.1". All corrected
      against 0.5.0.
      `src-tauri/tests/docs_citations.rs` is the guard: it resolves every backticked snake_case
      citation in the design doc and stock take against the tree, checks §8 names running tests,
      and cross-checks the stock take's `Ts` claim against the walker. Deliberately excludes
      `P2.7-REVIEW-OPEN-QUESTIONS.md`, whose §A *quotes* stale names as the defects to fix.
- [x] **T-6 Extend the doc guard to the remaining phase docs.** `P2.7-REVIEW-OPEN-QUESTIONS.md`
      and `P2.7-TEXT-EDITING-PARITY-PLAN.md` are not covered by `docs_citations.rs`. The blocker is
      structural, not effort: the review doc quotes superseded identifiers *on purpose*, so a naive
      citation check would flag the record of the bug as the bug. Separate quoted-defect context
      from live claims (e.g. a `> Defect (fixed):` marker the guard skips), then add both docs.
      **Done.** `~~strikethrough~~` is the marker (the review already used it, and it reads as
      retraction to a human). `docs_citations.rs` now covers all four programme docs, and splits
      every citation into *live* (must resolve) and *retracted* (audit record, exempt). Four
      things had to change for the extension to be honest:
      (1) the guard **excludes itself** from the source set — its own header quotes the superseded
      `t27_char_spacing_is_in_the_pen_walk`, so without that it would have passed on its own
      prose; (2) §A's rows had no disposition at all, so the audit read as six live defects —
      each row now opens with **Fixed**/**Closed**/**Open** and the retracted wording is struck,
      enforced by `every_recorded_defect_states_its_disposition`; (3) two `file.rs:NNN` pointers
      had silently drifted (`textlocate.rs:901` is now `.first()`, `:919` is a struct field), so
      both are symbol citations now and `governed_docs_point_at_symbols_not_line_numbers` keeps
      them that way; (4) §E still promised `docs/P2.7-ADOBE-PARITY-PLAN.md`, a file B-1 renamed
      away — now `governed_docs_cite_only_paths_that_exist` (paths resolve against either the
      repo root or the crate root; a bare `x.rs` only has to name a real stem). Non-vacuity
      proved by mutation: unstriking A-2's stale name, adding a bogus plan citation, dropping
      A-6's marker, unstriking the stale §E path and renaming a file stem each fail exactly one
      test.

- [x] **T-7 Widen the H0 rotation refusal to `cm`.** H0 read rotation from `Tm` alone, but the
      renderer draws with `tm x ctm`. A page that rotates with `cm` and leaves `Tm` at the
      identity — a common producer shape, and the shape the app's own `watermark` writes — was
      still offered as editable with a horizontal `advance`: editable *and* mis-measured, the
      exact failure H0 exists to remove.
      Acceptance: a `cm` of `[0 1 -1 0 300 100]` with an identity `Tm` yields a run flagged
      rotated and refused with `rotated-text` on both the picker and write paths; a translating
      `cm` stays editable; a rotation undone by `Q` is not flagged.
      **Done.** `TextMatrixWalk` now tracks the CTM with a `q`/`Q` stack (only the CTM — the
      text-state parameters are reset by `BT`, not by `Q`, and stay as they were).
      `is_rotated` reads the composed `matrix_mul(tm, ctm)`. The CTM is still not folded into
      `origin`/`advance`, so this is strictly a refusal-widening: no measurement changed and no
      gate was relaxed. Non-vacuity proved by mutation — reverting `is_rotated` to `tm` alone
      fails `a_graphics_matrix_rotation_is_flagged_on_the_run` and
      `refuses_a_run_rotated_by_the_graphics_matrix`; dropping the `Q` restore fails
      `a_rotation_restored_by_q_does_not_flag_the_run`.

## H2. P2.7.3 XObjects → 0.6.0 — `[~]` gate green, awaiting installer (Q9)

Design: `docs/P2.7.3-xobject-DESIGN.md` (frozen v1.0). Rulings: Q1 nesting/cycles, Q2 **per
draw**, Q3 private `/Resources` only when touched, Q6 grouped inverse from the start, B-7.
Do the tasks in order; each is one iteration and one commit.

- [x] **X1 Address model + discovery.** Commit `cdce29d`. `RunId.xobject_path` + TS mirror
      (empty = page, so 0.5.0 payloads and inverses keep their meaning); `locate_runs` descends
      `Do` → `/Subtype /Form` with per-key resource fallback; `Gate::NestedXObject`
      (`nested-xobject`) with a message naming depth or cycle and the way out; `mcid` dropped.
      Gate: `locates_runs_inside_a_form_xobject` (text `ALPHA`, non-empty path),
      `a_run_inside_a_nested_form_is_located` (depth 2 — distinguishes the level-by-level
      reading of Q1 from a one-level walk), `a_cycle_refuses_with_nested_xobject_and_is_visible`,
      `depth_nine_refuses_with_nested_xobject` (8 editable, 9 refused),
      `editing_a_placeholder_is_refused_with_nested_xobject`,
      `the_nested_xobject_gate_outranks_run_mismatch`. Mutation of `MAX_XOBJECT_DEPTH` to 1
      fails the depth-2 and depth-9 tests.
- [x] **X2 Copy-on-write per draw.** Clone each level along the path, outermost first; install
      each clone under a fresh name (`X1_edit1`, mirroring `fresh_property_name`) and repoint
      the enclosing `Do` at the clone. Copy `/Resources` **only** when the edit will touch it
      (font substitution or named-`/Properties`), and copy the enclosing resources before
      adding a name when they are shared by more than one page. Write the clone's stream with
      **`change_content_stream`, never `change_page_content`** (the latter addresses a page
      dictionary: it errors on a stream and has a silent `Ok(())` arm when `/Contents` is an
      unexpected type).
      Acceptance: `editing_one_draw_leaves_the_other_draw_byte_identical`;
      `editing_a_shared_form_on_one_page_leaves_the_other_page_untouched`;
      `a_nested_run_is_editable_and_only_its_draw_changes`;
      `a_substituted_font_gets_a_private_resources_dict`.
      Non-vacuity: replacing the clone with an in-place edit must fail the two "leaves
      untouched" tests. **Confirmed:** `clone_draw` returning no clones fails all three
      "leaves untouched" tests, `a_nested_run_is_editable_and_only_its_draw_changes`, and all
      three undo tests. Also green: `editing_a_form_run_writes_the_form_and_not_the_page_stream`
      (the page's text operands stay empty; only the `Do` changes).
- [x] **X3 Grouped inverse and undo.** Add `XObjectClone` and
      `ContentOperation.xobject_clones` + `xobject_path`, riding the existing `also` /
      `font_swap.also` / `property_clone` group rather than a new mechanism. Undo re-validates
      each `Do` (operator, operand is the `new_name` this edit wrote) before repointing, then
      restores bytes/font/`ActualText`/`BDC`, repoints `Do` to `old_name`, and removes the
      clone's name and object so edit-then-undo leaves no orphan.
      Acceptance: `undo_restores_the_do_and_drops_the_clone`;
      `undo_leaves_the_other_draw_untouched`; plus
      `undo_after_a_substitution_leaves_no_orphan` (`cloned_resources` is *observed* after the
      write, not predicted: the private dictionary is created lazily by the write path, and
      recording a copy that never happened would delete a dictionary still in use).
      **Confirmed:** deleting the `Do` restore fails both `undo_restores_the_do_and_drops_the_clone`
      and `undo_leaves_the_other_draw_untouched`.
- [x] **X4 Fixtures + version 0.6.0.** Commit `xobject-single`, `xobject-two-draws`,
      `xobject-two-pages`, `xobject-nested`, `xobject-cycle`, `xobject-depth-9`,
      `xobject-shared-resources` through `testpdf.rs` + `make_p24_fixtures.rs`. Bump the three
      version strings and `docs/RELEASE-NOTES.md` to 0.6.0. Update the parity matrix row A6
      `Missing → Have` **only after** the installer is confirmed (Q9); until then row A6 stays
      `Missing` and the task closes on a green gate, with the phase marked `[~]`.
      **Done.** All seven fixtures are committed (751–2135 bytes, regenerated deterministically
      from the existing builders). Six carriers agree at 0.6.0 — `Cargo.toml`, `Cargo.lock`,
      `package.json`, both `package-lock.json` entries, `tauri.conf.json` — and
      `docs/RELEASE-NOTES.md` carries the 0.6.0 section.
      **The gap X1–X3 left was that no test ever read the committed artefacts.** Every
      XObject test built its document in memory, so a fixture the design doc promises but
      nobody generated would have gone unnoticed — the same defect class as a §8 table naming
      a test that does not run. `src-tauri/tests/xobject_fixtures.rs` closes it: five tests
      assert the §8 shape from disk (reachability at depth 1 and 2, the two draws and two
      pages, the cycle and depth-9 refusals reaching the picker as `nested-xobject`, and the
      shared indirect `/Resources` object), plus one that edits and **saves** a committed
      fixture and re-reads the clone — the only form edit in the suite that goes through
      `Document::save`, which is where a resource name `renumber_objects` invalidated would
      show. Mutating either the tree or the doc fails it: removing `xobject-cycle.pdf` fails
      `every_fixture_the_design_doc_names_is_committed`.
      The doc guard now covers the P2.7.3 §8 table too — it named ten tests and checked none;
      substituting a bogus name fails `the_design_doc_acceptance_table_names_tests_that_exist`.
      Row A6 stays `Missing` with the reason recorded (Q9), and `P2.7-STOCK-TAKE.md` is
      re-verified at 0.6.0 rather than still dated to 0.5.0.
      **Phase status: `[~]`** — gate green, installer not yet confirmed.

## H3. Remaining phases

Version targets are ruled (Q7): one minor bump per phase. **1.0.0 is reserved for P2.8** — the
point at which text editing is declared complete. Never reach it by accident.

- [x] **D-1 Govern the P2.7.3 design doc with the citation guard.** `GOVERNED_DOCS` still lists
      only the four P2.7.1-era documents, so `P2.7.3-xobject-DESIGN.md`'s live claims are
      unchecked except for its §8 table (X4 added that). Adding it today fails
      `governed_docs_cite_only_paths_that_exist` on its bare `make_p24_fixtures.rs` citation:
      `cited_paths` only accepts a bare `x.rs` when it names a file stem under `src/` or
      `tests/`, and the generator lives in `examples/`. Fix the citation (write
      `examples/make_p24_fixtures.rs`) and widen the stem set to `examples/` in the same task,
      then add the doc. Blocked on nothing: it is a doc-and-guard task, not a phase build.
      **Done.** `GOVERNED_DOCS` now has five entries, and the stem set comes from a
      `crate_stems()` scan of `src/`, `tests/` **and** `examples/` — the previous set was built
      from `rust_sources()`, whose two roots are `src/` and `tests/`. The doc's citation is now
      `examples/make_p24_fixtures.rs`, the path form the P2.7.1 doc already uses.
      Non-vacuity, three ways: adding a bogus `ghost_generator.rs` fails the path test, renaming
      a live `locate_runs` mention to `locate_runs_all_levels` fails the identifier test (so the
      doc is genuinely *checked*, not merely listed), and a new
      `a_bare_stem_from_the_examples_directory_resolves` fails if the scan drops `examples/` —
      without it, widening the exemption would have "fixed" the same false positive while
      leaving the generator uncheckable.
- [~] **P2.7.4 Transforms → 0.8.1** (Q7 said 0.7.0; 0.8.0 published first, so 0.7.0 would
  be a semver downgrade) — Design doc frozen (`P2.7.4-transform-DESIGN.md`).
  - [x] **Item 1, composition.** `RawRun::page_origin` (CTM applied to the text-space pen),
        `page_scale`, `advance_axis`, `up_axis`; `pair_run`, `locate_span`'s anchor and
        `assign_object_spans` now compare in page space. Text-space lengths are carried into
        page space in both `group_span`'s metric fallback and the write path's width gate.
        Translate/scale/flip runs are editable end-to-end; rotation still refuses, unchanged.
        Three mutations verified (revert composition → 27 failures; unscale the fallback and
        the write path → the two new tests fail).
  - [ ] **Item 2, unblock rotation.** Replace the `rotated-text` refusal with correct
        measurement once 1 has landed.
      H0/T-7 ship the refusal; this phase replaces it with measurement along the run's own
      advance axis. Measurement found a prerequisite defect: `RawRun::origin` is text-space and
      never composed with the CTM, so `translated_by_graphics_matrix` refuses as `run-mismatch`
      end-to-end while `a_run_translated_by_the_graphics_matrix_stays_editable` asserts it is
      editable by testing only the font gates (third vacuous guard in this codebase).
      Includes the rotation hotfix lineage (H0, T-7). Started 2026-10-25.
- [!] **P2.7.5 CID/Identity-H → 0.8.0** — **Blocked on:** design doc. **C-4 is now ruled:**
      refuse today; substitute-with-consent once embedding ships. The design doc must state that
      explicitly.
- [!] **P2.7.6 Embedding (≡ Stage 2 MECH-B) → 0.9.0** — **C-5 is now ruled:** bundle Liberation
      (~350 KB) and add the OFL notice to About/notices in the same task. **Priority-raised by
      C-4:** it is the gate on real-world CID coverage, not a later nicety. Blocked on: design doc.
- [ ] **U-1 Multi-level undo / grouped transactions (C-6).** In scope, **before P2.8**, built
      once. Design the transaction as a group over `ContentOperation`, `FontSwap`,
      `PropertyClone` and `XObjectClone` so a future phase adds a leg rather than a mechanism.
      Also spans page operations and annotations (`EditorProvider.push` is already one visible
      undo per action; the record underneath becomes a group).
      Acceptance: one XObject edit touching stream + `/Resources` + `/Properties` is **one**
      undo; a reflow-shaped multi-operand edit is one undo; the inverse refuses rather than
      partially applying when any leg fails validation.
- [!] **P2.8 Line-level reflow → 1.0.0** — **Blocked on:** design doc, and on U-1. Invariants
      are already fixed (see the plan §4).

## Loop hygiene (do every iteration)

- [ ] Update `LOOP_LOG.md` with the iteration record.
- [ ] Remove any probe/example file created for investigation.
- [ ] `git status` must show only the intended files before committing.
