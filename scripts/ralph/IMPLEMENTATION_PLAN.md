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

## H2. P2.7.3 XObjects → 0.6.0 — `[~]` gate green, installer confirmation **unrecorded** (Q9)

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
      **A6 therefore stays `Missing` in both places** (`P2.7-STOCK-TAKE.md` and the parity
      plan), and the reason is now specific rather than general: no 0.6.0 installer
      confirmation exists in this repository to point at. This is not a claim that the phase is
      unproven — its gate is green and its X2/X3 acceptance tests run — it is that Q9 names a
      human step and nothing recorded that the step happened. Promoting A6 on "0.6.0 shipped"
      would be exactly the inference Q9 exists to prevent.
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
      **Phase status: `[~]`** — gate green; Q9's installer confirmation is **not recorded
      anywhere in this repository**, and the phase cannot be closed on inference.
      Q9 requires all three of: a green gate; a named installer; and the user having run it.
      0.6.0 has the first. It has no recorded artefact id, no recorded SHA-256, and no
      recorded confirmation date. `docs/RELEASE-NOTES.md` §0.6.0 does record its acceptance
      evidence for 0.8.0 (run `40158689217`, artifact confirmed by the user 2026-10-25) and
      0.9.0 (run `40719287682`, artifact `12830611552`) — so the convention exists and 0.6.0
      is the one release that predates it. Three later phases (0.8.0, 0.8.1, 0.9.0) have been
      built on top of it, which Q9 forbids, so this is a live breach rather than a stale
      label. **It stays `[~]` until the user states the 0.6.0 installer was run.** Recording
      that a phase shipped is not the confirmation Q9 asks for, and this row must not be
      closed by the fact that it is three releases old.

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
- [x] **P2.7.4 Transforms → 0.8.1** (Q7 said 0.7.0; 0.8.0 published first, so 0.7.0 would
  be a semver downgrade) — Design doc frozen (`P2.7.4-transform-DESIGN.md`, committed
  `f566b58`). **Closed:** both items done, published at `v0.8.1`, and the installer is
  recorded as hand-confirmed in `docs/RELEASE-NOTES.md` §0.8.1 — artifact `12618531386` from
  run `40244727545` on `cb8693e`, 7,824,005 bytes, SHA-256 `7a201486…be0a17`. Q9 is satisfied
  on the record, so this row closes rather than sitting open.
  - [x] **Item 1, composition.** `RawRun::page_origin` (CTM applied to the text-space pen),
        `page_scale`, `advance_axis`, `up_axis`; `pair_run`, `locate_span`'s anchor and
        `assign_object_spans` now compare in page space. Text-space lengths are carried into
        page space in both `group_span`'s metric fallback and the write path's width gate.
        Translate/scale/flip runs are editable end-to-end; rotation still refuses, unchanged.
        Three mutations verified (revert composition → 27 failures; unscale the fallback and
        the write path → the two new tests fail).
  - [x] **Item 2, unblock rotation.** Replace the `rotated-text` refusal with correct
        measurement once 1 has landed. **Done**, in the 0.8.1 train (`a582abd` measured text
        in its own frame, `509ef24` rewrote the rotation tests, `2ddbb4f` added the acceptance
        proof). The refusal is gone: `a_rotated_run_is_editable_with_its_own_advance_as_quota`
        (`textedit.rs:3801`), `a_run_rotated_by_the_graphics_matrix_is_editable` (`:3843`), and
        `a_rotated_replacement_saves_and_its_inverse_restores_the_bytes`
        (`tests/textedit_gates.rs:2939`) assert the replacement case, not the refusal.
      H0/T-7 ship the refusal; this phase replaces it with measurement along the run's own
      advance axis. Measurement found a prerequisite defect: `RawRun::origin` is text-space and
      never composed with the CTM, so `translated_by_graphics_matrix` refuses as `run-mismatch`
      end-to-end while `a_run_translated_by_the_graphics_matrix_stays_editable` asserts it is
      editable by testing only the font gates (third vacuous guard in this codebase).
      Includes the rotation hotfix lineage (H0, T-7). Started 2026-10-25.
- [x] **P2.7.5 CID/Identity-H → 0.8.0** — **Unblocked 2026-10-25** by `docs/P2.7.5-cid-DESIGN.md`
      (202 lines, committed `f566b58`), which states C-4 explicitly as required. **Shipped in
      0.8.0**; gate and installer both recorded in `docs/RELEASE-NOTES.md` §0.8.0 (gate: CI run
      `40157718290` on `2199903`; installer: run `40158689217`, 7,827,785 bytes, hand-tested and
      accepted by the user 2026-10-25).
- [~] **P2.7.6 Embedding (≡ Stage 2 MECH-B) → 0.9.0** — **Unblocked 2026-10-26** by
      `docs/P2.7.6-embed-DESIGN.md` (390 lines, committed `3011108`), which carries C-5 as ruled
      (bundle Liberation + the OFL notice in the same task). **Shipped in 0.9.0.** Stays `[~]`,
      not `[x]`: `docs/RELEASE-NOTES.md` §0.9.0 is an explicitly **unpublished draft** whose own
      *Hand-test provenance* says the hand-tested installer (`40719287682`, artifact
      `12830611552`) was built from `dd8efce` and the tree has since moved 16 commits, including
      executable changes on the PDFium picker path (`44c9457`, `3ea86f5`) and the inline
      `/Resources` fix (`f9c75d5`). Q9 is unmet by the notes' own account, so this row is
      gate-green-awaiting-confirmation, which is exactly what `[~]` means.
- [x] **U-1 Multi-level undo / grouped transactions (C-6).** In scope, **before P2.8**, built
      once. Design the transaction as a group over `ContentOperation`, `FontSwap`,
      `PropertyClone` and `XObjectClone` so a future phase adds a leg rather than a mechanism.
      Also spans page operations and annotations.
      Acceptance: one XObject edit touching stream + `/Resources` + `/Properties` is **one**
      undo; **acceptances (a) and (c) delivered by X3; acceptance (b) is U-2**; the inverse
      refuses rather than partially applying when any leg fails validation.
      **Closed 2026-10-31. The mechanism is TS, not Rust, and the earlier note in this row
      named it wrongly — corrected below.**
      Where undo actually lives: `src/state/EditorProvider.tsx`, a per-document
      `{undo: StackEntry[], redo: StackEntry[]}` map (`:214`, ref at `:235`) written only by
      `push` (`:364`). One `push` is one visible undo, which is the grouping this row asked
      for: there is no per-leg undo to group because a leg is a closure inside one
      `StackEntry`, not a separate stack entry.
      (a) `undo_restores_the_do_and_drops_the_clone` (`textedit.rs:4784`) and
      `undo_leaves_the_other_draw_untouched` (`textedit.rs:5056`) — one stream + `/Resources` +
      `/Properties` edit is one undo, and the other draw is untouched. Wired end to end at
      `EditorProvider.tsx:551` (T5), whose entry carries the whole `ContentOperation` inverse.
      (c) `undo_after_a_substitution_leaves_no_orphan` (`textedit.rs:4993`) — `revert`
      (`textedit.rs:2515`) validates every `Do` leg before mutating any of them, so a stale leg
      refuses the whole undo. Confirmed in code rather than inferred: the check precedes the
      write in one pass.
      Annotations are covered by three separate `push` sites — create (`:399`), delete (`:432`,
      label `Delete ${kind}`), and text edit (`:462`, restoring the previous text rather than
      deleting the note).
      **What this row's "also spans page operations" clause does NOT have, and that is a
      separate defect, not a reason to keep U-1 open:** page operations do not record undo at
      all. `rotatePage` / `deletePages` reach the UI through `runDerived` in
      `src/components/EditorPane.tsx:113`, which takes a `label` and discards it (`void label`,
      `:125`) and never calls `push`. So rotate and delete pages are **not undoable today**,
      despite this row's clause saying they are. Filed as **U-3** in
      `docs/P2.7-TEXT-EDITING-PARITY-PLAN.md` under C-6 rather than here, because it is
      page-operation coverage and not transaction grouping — the grouping mechanism U-1
      specifies does not exist for pages to use, and adding it is a different task with a
      different test.
      Two further facts worth recording so the next reader is not misled by the old note:
      `src-tauri/src/editor/undo.rs` (`UndoStack`, `UndoEntry { label, bytes }`,
      `push(label, before: Vec<u8>, after: Vec<u8>)`) is **dead code** — referenced nowhere
      outside its own tests, and not on any edit path. And `EditorProvider` is a TSX component,
      not a Rust type; the earlier claim that `pages.rs` and `annotate.rs` route through it was
      wrong, and neither file records undo.
- [!] **P2.8 Line-level reflow → 1.0.0** — **Design-doc blocker cleared 2026-10-31** by:
      `docs/P2.8-reflow-DESIGN.md`, 547 lines, frozen at `39d2b26` (the 460-line figure
      sometimes quoted is that commit's diff stat, not the file). Invariants live in
      `docs/P2.7-TEXT-EDITING-PARITY-PLAN.md` §4, not in this file, which has no `## 4`.
      **Still `[!]`, because `[!]` means "blocked (needs a ruling)" and two rulings are open.
      It is no longer blocked on a missing design doc.** What blocks it now, in order:
      **§7.2 — RESOLVED 2026-10-31 by R10**, so it is no longer a blocker. The prefix includes
      the kern for a run kern-positioned inside one `TJ` array; a run whose position also
      depends on a preceding `Tj` or a mid-array `Td`/`TD` is refused. Ruling it exposed a
      reader defect (`R10.1`) that is now T-8 below. Answering it also retired the risk that the
      guard (`9471313`) had been built on an unruled assumption: the rule is now stated, so the
      guard's expected positions can be checked against it rather than assumed to match.
      **§7.3 — the R5/R8 refusal strings.** Still undecided in the doc. R5's gate exists
      (`R5StraddlesLevels`). R8's does **not**: the refusal was implemented, no test could
      make it fire, and it was deleted rather than shipped unguarded — a `TJ` array cannot
      express a `Tf`/`BDC`/`Ts` between two strings, so the states R8 rejects are unreachable
      from a single array. §2.3.2 of the doc records this. §7.3 must therefore be reworded
      before it is ruled: the question is not "what string for R8" but "what, if anything, R8
      refuses on a path that exists."
      **The undo leg.** U-1 acceptance (b): `ReflowOperation` is recorded but not replayable.
      Needed for a user-facing undo of a reflow; **not** needed for case (a)'s write, which is
      landed. Tracked as U-2 below.

## H4. P2.8 case (a) → 1.0.0 — task list

Do these in order. Each is one iteration and one commit. **None is ticked**, and none may be
ticked on the ground that the suite was already green: where a test partly exists, the entry
names the **new assertion** and it must be shown **RED** against `b260966` (current HEAD) before
any implementation. A red run at `39d2b26` is also acceptable for entries whose subject predates
`b260966`. If an assertion cannot be made red, say so in `LOOP_LOG.md` and either tighten it or
record it as equivalent — do not tick the box.

Rulings this section obeys: Q7 (1.0.0 is reserved for P2.8), Q9 (no phase `[x]` without a
confirmed installer), C-2 (line-level only), and the parity plan's §4 invariants.

- [x] **R-1a Implement R10: the kerned `measured prefix`.** §7.2 was **ruled on 2026-10-31**
      (R10, `docs/P2.8-reflow-DESIGN.md`), so this is code, not a ruling. The rule: a run
      kern-positioned inside one `TJ` array has the kern in its prefix.
      Acceptance test first: `a_kerned_prefix_includes_the_kern`.
      Change the prefix arithmetic in `reflow_tj_array` so the kern is included. **No move and no
      new fixture** — this entry is arithmetic only.
      **NEW ASSERTION:** that the prefix `reflow_tj_array` computes for the kern-positioned
      fragment of `kerned-line.pdf` equals line origin + that fragment's ink x offset, i.e. that
      the kern is in it. No existing test states which side of §7.2 the code is on.
      **On the fixture, because the topology looks wrong and is not:** `kerned-line.pdf` has
      **no separate trailing run** on the target's baseline — line 1 is one `TJ` array whose kern
      sits *between* its own fragments, and line 2 is a second line reached by `Td`. That is
      **fine for this entry and fatal for a move**: R-1a asserts arithmetic on a prefix, which
      needs a known-correct prefix, not a run to displace. The move is R-4's job, against R-3's
      fixture. Do not build a fixture here.
      **Depends on T-8 only for a `Tz != 100` assertion. Proceed at `Tz 100`:** until the reader
      applies `Tz` to the kern, a kerned fixture at `Tz 50` measures 3 pt wrong. At `Tz 100` the
      two agree.
      **Partly exists — must be shown RED.** `a_kerned_lines_glyphs_are_where_iso_9_4_3_puts_them`
      (`9471313`) asserts ISO 9.4.3 *placement* against PDFium's grid and never calls the reflow
      path, so it is not evidence; the R10.2 mutation shows it cannot observe the reader either.
      **Done — one iteration, commit `feat(p2.8): include the TJ kern in the measured prefix`.**
      RED confirmed before the change: `moved_to` = 130.704 against the fixture's true tail ink at
      136.704, i.e. short by exactly the 6 pt kern. Mutation-verified afterwards by replacing the
      kern term in `reflow.rs` with `0.0`: `scripts/mutate.sh` reported **CAUGHT** (exit 0), and
      restoring the term returned the suite to green.
      **The finding this task existed to produce:** the kern belongs in `moved_to` but **not** in
      the `Td` offset — the number stays in the tail's `TJ` array and is applied there. Writing
      "the prefix" once in both places applied it twice, measured at 142.704. Recorded as **R10.3**
      in the design doc, because the two quantities are now deliberately unequal and a reader who
      "simplifies" them back into one will be wrong by the kern.

- [x] **R-1b Refuse a kern the walk cannot attribute.** Same ruling's other half: a run whose
      position also depends on a preceding `Tj`, or on a `Td`/`TD` issued after the array began,
      is **refused** rather than measured. **Depends on R-1a** for the arithmetic it declines to
      compute without — a refusal that guards arithmetic which does not exist yet guards nothing.
      Acceptance test first: `a_kern_that_spans_a_tj_or_mid_array_td_refuses`.
      **NEW ASSERTION:** that a run positioned by a `Tj` before it, or by a mid-array `Td`, is
      refused with the R10b arm rather than measured. **Synthetic run pair, no new fixture** — the
      states are unreachable from the committed fixtures by construction, which is exactly why
      they must be built as runs rather than found as files.
      **Done — one iteration, commit `feat(p2.8): refuse a prefix the TJ walk cannot attribute`.**
      RED confirmed before the change: with the check removed both shapes fell through to
      `not-one-array`, i.e. nothing refused the unattributed prefix. Mutation-verified afterwards
      by gating the check behind `false &&`: `scripts/mutate.sh` reported **CAUGHT** (exit 0), and
      restoring it returned the suite to green.
      **Both halves of the ruling are one property of the run set, not two.** No operator can
      appear inside a `TJ` array (its elements are strings and numbers only), so a "mid-array
      `Td`" is not expressible; its reachable form is a **second array on the same baseline,
      started by its own `Td`**. And a preceding `Tj` leaves no trace in the reported origin,
      because `TextMatrixWalk::step` is a no-op for a show operator. Both reduce to *every
      same-baseline run must come from the target's own `op_index`*, which is the check.
      Recorded as **R10.4** in the design doc, with the ruling's own wording left unchanged
      because it describes the property and not the constructible shape.
      **A `--test NAME` argument is not a filter.** `scripts/mutate.sh --test
      a_kern_that_spans…` exits 3 (`MEASUREMENT_ERROR`, no summary line) because `--test` names
      an integration target and the test is in the lib. The harness did its job — it refused to
      read "no summary line" as "no failure" — and the correct invocation is `--lib NAME`.

- [x] **R-2a The R8b fixture.** `two-mcid-one-baseline.pdf`: page content with two `BDC`/`EMC`
      units on one baseline, the edit target in the second, a run from the first lying further
      left. Operators: `BDC` `/P` `/MCID 0`, `Tm 72 700`, `Tj` a short run, `EMC`; then `BDC`
      `/P` `/MCID 1`, `Tm 200 700` (same `y`), `TJ` of two strings, `EMC`. Both units are `/P` so
      the scopes differ by `Block`, not merely by `Untyped`. Expected ink before any edit: unit-0
      run at `x=72`, target at `x=200`, tail at `200 + advance(target)`.
      Acceptance test first: `the_r8b_fixture_has_two_units_on_one_baseline`.
      **NEW ASSERTION:** none about reflow yet. The fixture's own guard asserts, **on the parsed
      operator list rather than on bytes**, that two `BDC`/`EMC` pairs exist, that their `/MCID`
      values differ, and that the two runs' baselines are equal. A byte-level assertion would
      pass on a stream that is textually right and semantically wrong.
      **Done — commit `feat(p2.8): commit the two-MCID one-baseline fixture (R8b)`.** The
      measured document is `HI` at `(72, 700)` in `Untyped`-free scope, and `AB`/`CD` at
      `(200, 700)`/`(232.016, 700)`; all three resolve to `MergeScope::Block`, because the
      builder writes a real `/StructTreeRoot` → `/ParentTree` → per-page array of two `/P`
      elements. **The structure tree is load-bearing, not decoration**: without it both units
      fall back to `MergeScope::Untyped(Some(mcid))`, which is a *finer* boundary and would let
      R-2b pass on the conservative reading while the coarse one — the one the doc names — was
      never exercised. The guard asserts `Block` explicitly for that reason.
      Mutation: changing both `/P` elements to `/Span` (an inline role) fails the `Block`
      assertion; restoring them passes. Deleting the fixture file fails the guard's own load.
      Two other mutations were tried and discarded as non-mutations: giving both `BDC`s the same
      `/MCID` fails the scope assertion, and the kern-less `[(AB) (CD)]` array was chosen over
      `[(AB) -250 (CD)]` so a later `Tz` bug (T-8) cannot move the tail's expected ink.

- [x] **R-2b Refuse a line whose runs leave its merge scope (R8b).** §7.3 was **ruled on
      2026-10-31**, so this is code. `reflow_tj_array` took `line_origin` as the minimum
      `origin.0` over every run on the baseline, and `on_baseline` filters on `baseline()`
      (`origin.1 + rise`) alone, with **no scope term**. A same-baseline run in another `MCID`,
      further left, makes `line_origin` too small and the `Td` offset correspondingly too large.
      **Depends on R-2a** for its fixture.
      Acceptance test first: `a_same_baseline_run_in_another_mcid_does_not_dislocate_the_tail`.
      **NEW ASSERTION:** that the edit refuses with `R8b-scope-left-of-line` rather than computing
      a wrong `line_origin`.
      **Done.** `ReflowRefusal::R8bScopeLeftOfLine` (`R8b-scope-left-of-line`); `line_origin` and
      `line_right` both come from `in_scope` (same-baseline runs with the target's
      `merge_scope()`), and a run outside it lying left of the line start refuses.
      Two things were found rather than assumed, and both are recorded in design §R8b.2:
      (1) **R8b had to move before R10b or it is dead code.** R10b refuses *any* same-baseline run
      at a different `op_index`, and no constructible two-unit line can avoid one (a `BDC`/`EMC`
      pair is an operator pair; a `TJ` array's elements are strings and numbers only, so one array
      cannot hold two scopes — the invariant `one_tj_array_cannot_carry_two_tagged_units` asserts).
      Measured: with the check after R10b the fixture refused `R10b-unattributed-kern`.
      (2) **R10b then had to be scoped to `in_scope`**, or R8b's `line_right` exclusion is dead
      code in the other direction: a right-hand neighbour would be refused as unattributable.
      The new `a_run_outside_the_scope_does_not_set_the_line_extent` is what makes that half
      observable, and it is written against the committed fixture **plus one injected run**
      because a wholly synthetic pair never reaches R4 — an empty document fails at the content
      read first, so the synthetic version of the test was mutation-SURVIVED.
      Both refusal arms and the extent exclusion were mutation-verified in the guarded file
      (`scripts/mutate.sh --lib`): each mutant caught, each restore green.
- [x] **R-2c Pin R5's user-visible string, and give R5 a real-content fixture.** **New work
      discovered 2026-10-31 while ruling §7.3, not a rename of anything.** Two gaps, named:
      **(i) the message is unasserted** — no test states the string a user reads for R5;
      `ReflowRefusal::message()` is pinned nowhere, and
      `a_line_that_crosses_content_levels_refuses_under_r5` asserts only `name()`, the internal
      identifier. The same gap now exists for R8b's message, which R-2b's tests also leave
      unasserted — R-2c's `message` guard should cover every variant, not just R5.
      **(ii) R5 has never fired on real content** — `xobject-single.pdf`'s page draws only
      `/X1 Do`, so no committed fixture puts page text and form text on one baseline; the gate is
      proven synthetically only, against a run pair built in the test.
      Both are defects in a gate **this phase relies on** rather than deferred inventory, which
      is why they stay in P2.8.
      Acceptance test first: `r5_reports_its_own_message_and_refuses_real_content`.
      **NEW ASSERTION:** that the R5 message is the exact sentence, and that a committed
      `page-and-form-one-line.pdf` (page text at a baseline, plus a form drawing text at the same
      baseline, page text starting left) refuses with `R5-straddles-levels` through `locate_runs`
      rather than through synthesized runs. R-2a's fixture cannot serve: it differs in
      `merge_scope`, while R5 fires on `xobject_path` — different term, different refusal.
      **Done 2026-11-01.** `page-and-form-one-line.pdf` committed (page `PAGE` at x=72, form
      `FORM` at x=200, both on baseline 700) and generated by `page_and_form_one_line()` in
      `testpdf.rs`. `every_reflow_refusal_names_itself_and_its_remedy` pins all seven variants,
      reading the variant list from the module's own `enum` body so an eighth refusal cannot
      arrive unpinned, and `the_variant_reader_sees_its_own_source_and_not_a_neighbouring_enum`
      feeds that reader a crafted sample including the defect shape. Counts 513/603 -> 516/606,
      ignored unchanged at 90, so no CI `#[ignore]` pin moved.

- [x] **R-H1 Assert the fixture tree holds no untracked files.** **New work discovered
      2026-11-01.** `750f5fb` committed 36 test-generated copies because `git add -A` swept them
      in, and nothing noticed for two commits: the concurrency tests create them with
      `std::fs::copy` beside a real fixture, they are byte-identical to an input, and their
      cleanup is `let _ = std::fs::remove_file(...)` on success paths only, so a panicking run
      leaves them and the suite still reports green. `.gitignore` covers two known prefixes, which
      is a backstop for two names, not a guarantee. No test enumerates `tests/fixtures/`; the
      `read_dir` calls in `ci_workflow.rs`, `docs_citations.rs` and `font_bundle.rs` walk `src/`,
      `docs/` and the font directory respectively.
      Acceptance test first: `the_fixture_tree_holds_no_untracked_files`.
      **NEW ASSERTION:** that every regular file under `tests/fixtures/` is reported by
      `git ls-files --error-unmatch`, and that the enumeration saw at least the committed fixture
      count, so an empty or unreadable directory cannot pass vacuously.
      **Form — a Rust test in `src-tauri/tests/`, not a shell step.** The suite already shells out
      to `git` (`the_font_payload_is_not_gitignored` runs `git check-ignore`, `release_version.rs` runs
      `git tag`), so there is a precedent and no new dependency. A gate step would be the wrong
      place: the gate asserts counts and build health, while this is a property of the working
      tree that must fail the same way locally, in CI, and in any editor's test run. Keeping it in
      the suite also means it is subject to the same mutation discipline as every other guard.
      **Dirty-tree behaviour — the deliberate part.** The test must ignore *modified* tracked
      files and flag only *untracked* ones (`git ls-files --others --exclude-standard`), because
      mid-iteration a legitimately edited tracked file is normal and must not fail the gate. The
      hazard is specifically a file that git does not know about, which is what a stray copy is.
      Stating it the other way round would make the test fire on every iteration.
      **Mutation that proves it:** copy `tests/fixtures/kerned-line.pdf` to
      `tests/fixtures/kerned-line-stray.pdf` while *not* matching either gitignored prefix, run
      the test, confirm it fails naming that path, then delete it and confirm green. A second
      mutation renames the stray to `sample-99.pdf`, confirms the gitignore backstop hides it, and
      records that the test is not a substitute for staging by explicit path.
      **Landed 2026-11-01 (iteration 21)** once `429540b` cleared the version-invariant red
      (**R-H3**, resolved by moving the carriers to 0.9.1). The test is
      `src-tauri/tests/fixture_tree.rs` -- `the_fixture_tree_holds_no_untracked_files` plus
      `the_untracked_reader_splits_on_nul_and_not_on_newlines`. Rewritten, not re-applied:
      iteration 20's copy was never committed and no copy survived (see `67c9655`).
      Mutation evidence, and the one place the harness could not certify it: **an untracked
      file is not a `git diff`**, so `scripts/mutate.sh` cannot see this mutant at all. Its
      freshness witness demands the mutated path be modified relative to `HEAD`, and the two
      ways to satisfy that both destroy the defect -- `git add -N` makes the stray *tracked*
      (measured: the guard then reports nothing and the harness reports `SURVIVED`), and editing
      a tracked neighbour leaves the stray itself undiffed. The catch was therefore measured
      with a direct `cargo test` pair instead: with `kerned-line-stray.pdf` present the guard
      failed naming that exact path; removing it passed. Counts 516/606 -> **518/608**, ignored
      unchanged at 90.

- [x] **R-H3 The carriers are stale now that `v0.9.0` is tagged — RESOLVED by a version bump.** **Discovered
      2026-11-01 (iteration 20), pre-existing and unrelated to that iteration's task.**
      `the_tree_version_is_ahead_of_every_published_tag` (`src-tauri/tests/release_version.rs:178`)
      **fails at `94d2d34` on a clean worktree**, so the local gate has been red since `v0.9.0`
      was created on `6caaedd` at 04:26 today. It is not a defect in the guard: the guard is
      doing exactly what `05a257f` built it for. The tree carries 30 commits past `v0.9.0`, all
      six carriers still say **0.9.0**, and HEAD is not the tagged commit, so `0.9.0 > 0.9.0` is
      false. Measured, both shapes, `--no-fail-fast`: plain **515 passed / 90 ignored / 1
      failed**, `--include-ignored` **605 passed / 0 ignored / 1 failed** — exactly the pinned
      516/90 and 606/0 minus the one guard.
      **Why this is `[!]` and not a fix.** Every available number is spoken for or forbidden:
      **1.0.0** is P2.8's and may not land before a confirmed installer (Q7, R-7, and this
      loop's standing instruction); **0.9.0** is taken by the tag; **0.9.1** is a *development*
      version, not a phase's, and taking it decides the cadence question **A-6**
      (`docs/P2.7-REVIEW-OPEN-QUESTIONS.md:29`), which is marked **Open** while Q7 rules only
      the per-phase mapping. It would also need a `## 0.9.1` section in
      `docs/RELEASE-NOTES.md`, and that section must carry the "rust 1.90 minimum" and "rust
      1.91 verified" lines `the_release_notes_name_the_toolchain_the_build_was_verified_on`
      demands — i.e. it would state that a build nobody has run was verified, which is the
      class of claim this whole guard set exists to stop.
      **What the ruling has to decide:** whether an in-flight phase carries a patch bump
      (`0.9.1`, with a release-notes section written as *unverified*), whether the carriers
      hold the last released number until the phase's own bump (`0.9.0`, which requires
      relaxing the guard for an untagged tree between a tag and the next phase bump), or
      something else. Do not answer it here.
      **Scope check, done rather than assumed:** CI is **not** red. `ci.yml` uses bare
      `actions/checkout@v4`, which fetches no tags; reproduced in a shallow clone of this repo,
      `git tag --list` returned **0** and the guard's `git tag --list v*` finds nothing and
      skips. The failure is local-only, which is why it went unnoticed — the tag was created at
      04:26:26, commit `324c131` landed 14 seconds before it, and the two commits after it
      (`03e231d`, `94d2d34`) are docs-only — and why the local gate and CI now disagree in the
      direction that matters (local red, CI green).
      **Resolved 2026-11-01 in `429540b`** (the ruling taken was: an in-flight phase carries a
      patch bump). All six carriers moved to **0.9.1**, with a `## 0.9.1` release-notes section
      that says in its own words that it is a development version, that no build was hand-run
      and none published. It names `rust 1.90` (the MSRV) and `rust 1.91.0` (the toolchain the
      local gate ran) in the adjacent-line shape
      `the_release_notes_name_the_toolchain_the_build_was_verified_on` parses. A `-dev` suffix
      was measured and is not available: `parse_semver` splits on `.` and requires three plain
      numbers. The guard is green again and **unchanged** -- the tree moved, not the invariant.
      Cadence question **A-6** stays Open for the next phase, but it no longer blocks the loop.

- [x] **R-G1 Assert the gate's command list against `ci.yml`.** **New work discovered
      2026-11-01.** `scripts/ralph/gate.sh` and `.github/workflows/ci.yml` are two hand-kept
      lists of the same checks. They have already drifted twice: fmt was in CI and absent from
      the gate until `144f65c` (so `714f2a9` passed the gate and failed CI), and clippy ran
      without `-D warnings` in the gate until this commit, which is the same failure in a
      smaller size — a warning passes locally and fails CI. Nothing detects the next drift.
      Acceptance test first: `the_gate_runs_the_checks_ci_requires`.
      **NEW ASSERTION:** that every `run:` command under `ci.yml`'s `rust` and `frontend` jobs
      appears in `gate.sh`, matched on the command text after collapsing whitespace, and that
      the set is non-empty so a parse failure cannot pass vacuously.
      **Is that comparison meaningful?** Not as a literal string match, for three measured
      reasons. (i) The two jobs run on different platforms with different setups — CI needs
      `apt-get install libwebkit2gtk-4.1-dev …` before anything compiles; the gate runs on a
      machine where those are already installed, and replicating the install would be wrong,
      not missing. (ii) Some CI steps have no local analogue at all: `cargo check --target
      x86_64-pc-windows-msvc`, `scripts/fetch-pdfium.ps1`, and the three PDFium-on-Windows
      tests cannot run on this aarch64 box, and `--include-ignored` here uses a different
      library that CI is explicitly told never to fetch. (iii) CI's `Tests` step is a shell
      script with count floors embedded, not a command the gate could contain; the gate
      expresses the same intent as two pinned shapes.
      **What is worth asserting instead, weaker but real:** a *declared* correspondence rather
      than a discovered one. Each check the gate does not run is listed in `gate.sh` with the
      reason it is out of scope (platform, or fetched binary), and the test asserts that every
      `ci.yml` `run:` line is either present in `gate.sh` or named in that exemption list. A
      new CI step then fails the test until someone decides which it is, which is the property
      that matters: the drift becomes a decision instead of an omission. The parse is over
      `ci.yml`'s step names and `run:` bodies only — asserting anything about *behaviour*
      equivalence between the two is not possible from text.
      **Not to be fixed here:** `npm ci` and `npm run build` stay missing from the gate. They
      belong to the frontend install/build path and are out of scope for this phase; this task
      records them as exemptions when it lands, it does not add them.
      **Landed.** `src-tauri/tests/gate_parity.rs` (`the_gate_runs_the_checks_ci_requires`, plus
      `the_parity_check_reports_what_it_is_given` for the parser) over a `>>> ci-exemptions` block
      in `gate.sh` holding 10 entries. Every `run:` body in all three jobs is parsed; the
      `windows-check` job's steps were kept rather than excluded, because they are the ones with
      no local analogue and are exactly the entries an allow-list is for. Three assertions the
      plan did not ask for: an exemption naming a command that no longer exists in `ci.yml`
      fails (an excuse cannot outlive its step), an exemption whose `covers:` command is no
      longer run by the gate fails (deleting a check cannot hide behind an old excuse), and no
      exemption may be a prefix of another (an allow-list must name checks, not wildcards).
      `fmt` and `clippy -D warnings` are additionally pinned by name and forbidden from being
      exempted, so the two historical drifts cannot be re-blessed as decisions. The `executed`
      floor is the measured value (2), not headroom.

- [x] **R-3 The A10 fixture.** Landed 2026-11-01 (`reflow_line` in `testpdf.rs`).
      `reflow-line.pdf` and `reflow-line-near-edge.pdf`, guarded by
      `every_fixture_the_reflow_design_names_is_committed`, `the_reflow_fixture_carries_the_a10_topology`
      and `the_near_edge_variant_has_almost_no_room_left` in `tests/reflow_fixtures.rs`, plus
      `the_committed_reflow_fixtures_are_the_builders_own_bytes` for byte stability.
      Counts moved 520 -> 526 and 610 -> 616 (**measured, not incremented**) across all four
      pin carriers. Two departures from the task text, both deliberate: (a) the fixture table
      lives in its own `### 6.2` section rather than reusing the XObject doc's guard, because
      that guard parses a different document and would have needed a second path anyway; (b)
      the near-edge variant is a **second file**, not a second page, so R-5 can assert "the
      stream is byte-unchanged" of the whole file.
      Original text follows.

      `two_run_line()` cannot carry A10 `two_run_line()` cannot carry A10 (§6.2: no second line, no `TJ`
      seam, nothing near the content box). Add a builder beside `too_wide_page()` in
      `testpdf.rs`, wired into `make_p24_fixtures.rs` with one line.
      Acceptance test first: `every_fixture_the_design_doc_names_is_committed` — extend the §6.2
      list so the new fixture is named and must exist on disk.
      **NEW ASSERTION:** that the A10 fixture is committed and byte-stable, and that it carries
      two lines, a `TJ` seam and a near-edge variant. `kerned-line.pdf` / `kerned-line-tz.pdf`
      (`6809678`) are the *guard's* fixtures: two lines and a seam, **no near-edge variant and no
      replacement-width case**, so they are a different topology.
      **Partly exists — must be shown RED.** The committed-fixture guard exists (`X4`); the
      clause naming this fixture does not.

- [x] **R-4 Row A10's guard: the move itself.** Write the A10 assertions against R-3's fixture.
      Acceptance test first: `a_wider_replacement_moves_the_rest_of_the_line_and_nothing_else`.
      **NEW ASSERTION:** after a write of a *wider* replacement, every run on the edited line has
      moved by exactly the measured width of the replacement, every run on every other line has
      unmoved ink, **and** the edited run's extracted text equals the requested string.
      `a_hand_built_td_moves_line_one_and_leaves_line_two_alone` (`9471313`) is the closest and
      is **not** this: it moves a hand-built `Td` with **no replacement text**, so it asserts
      nothing about growth, nothing about the run's extracted text, and nothing about the
      measured-width relation. This is the entry the plan's A10 row is actually waiting on.
      **Partly exists — must be shown RED against `9471313`/`b260966`.**
      **Done 2026-11-01.** The guard edits `ALPHA ` to `ALPHAab ` on `reflow-line.pdf` and asserts
      four properties on PDFium's per-glyph ink: the head of the line did not move, the tail
      moved by exactly the measured growth, the other line did not move, and the edited run
      reads back as the requested string with its tail's `origin.0` equal to `moved_to` **and**
      to `line origin + new width + kern` (R10.3's three-way relation, asserted separately).
      The untouched-line half is stated twice — once in ink and once over every run's
      `page_origin` on the reloaded document — so a tail glyph that happened to land right
      cannot stand in for the whole line.
      **RED confirmed** by transplanting the test onto `b260966` in a throwaway worktree: it
      failed with `the write reports its tail at 184.056; the reader puts it at 186.936`, short
      by the 2.880 pt seam kern that R-1a later put into the prefix. Three mutations of the
      guarded file were each CAUGHT via `scripts/mutate.sh --lib` and each restored green:
      `old_width` for `new_width` in the tail offset, the replacement text not written, and the
      splice taking one operator too many.
      **The replacement avoids `B`/`E`/`T` on purpose** — widening into letters the tail already
      carries duplicates the tracked glyph and an ink oracle then reads the wrong one. Counts
      moved 91→92 ignored and 618→619 `--include-ignored`, plain `passed` unchanged at 527;
      **measured, not incremented**, across all four pin carriers.
      Row A10 stays `Missing`: R-5 is not written and Q9 needs a confirmed installer.

- [x] **R-5 Row A10's refusal case.** Same fixture, near-edge variant.
      Acceptance test first: `a_replacement_too_wide_for_the_content_box_refuses_and_writes_nothing`.
      **NEW ASSERTION:** the refusal goes through the content box's `fits-width` path (§4 R3) —
      not R4's caller-supplied `limit_x` — **and** the stream is byte-unchanged after the
      refusal. `a_replacement_past_the_limit_refuses_under_r4` (`b260966`) asserts R4 with a
      `limit_x` handed in by the caller, and never inspects the content stream afterwards, so it
      covers neither half of this.
      **Done 2026-11-01.** `reflow_line_within_content_box` resolves the box from the document
      (`content_box`: `/CropBox`, defaulting to `/MediaBox`, both through the `/Parent` chain,
      normalised — §3) and passes its right edge to `reflow_tj_array`, which keeps its
      caller-supplied limit for the gate tests. R4's refusal is translated to the `fits-width`
      gate, carrying the two widths the decision compared: R3 says the change is the message, not
      the gate, so the *needed* width is the growth and the *available* width is the line's own
      room. Measured on the near-edge fixture: 9.064 pt of room against a 38.688 pt growth, so a
      resolver that returned a wider box could not pass by rounding. Two further guards are on
      `content_box` itself — the four selection properties (`/CropBox` first, `/MediaBox` default,
      inheritance through the page tree for **both**, normalisation) and the two refusal shapes
      (no box anywhere; a box present but unreadable, which must error rather than fall through
      to the next attribute). Six mutations of `src-tauri/src/editor/reflow.rs` were each CAUGHT
      via `scripts/mutate.sh --lib` and each restored green; the `ci.yml` floor was mutated both
      ways and both were caught. Counts moved 527 -> 530 plain and 619 -> 622
      `--include-ignored`, ignored unchanged at 92 — **measured, not incremented**, across all
      four pin carriers. Row A10 stays `Missing`: R-6 has no caller, R-7 needs a confirmed
      installer (Q9).

- [ ] **R-6 Give `reflow_tj_array` a caller in the edit path.** Today it is reachable only from
      tests — §5.3 accepts that the *guard* has no caller, but the reflow itself must be wired
      before A10 can pass on a real document.
      Acceptance test first: `a_reflow_edit_reaches_the_document_and_its_inverse_restores_it`.
      **NEW ASSERTION:** the reflow write goes through the same path a user edit takes (so the
      edited copy, not the original, is what changes — D-2), and its inverse restores the exact
      pre-edit bytes. No existing test invokes `reflow_tj_array` outside its own module.

- [ ] **U-2 Replay `ReflowOperation`.** U-1 acceptance (b), the only criterion X3 did not deliver.
      Depends on the §7.4 ruling (share the persisted record, or a sibling leg).
      Acceptance test first: `a_reflow_shaped_multi_operand_edit_is_one_undo`.
      **NEW ASSERTION:** one undo restores the pre-reflow stream after a write that inserted a
      `Td`, as a single entry — i.e. the `TJ` + `Td` + `TJ` vector is restored in one step, not
      one step per operator. `undo_restores_the_do_and_drops_the_clone` covers a *different*
      kind of grouped write (`Do` + clone), so it is not evidence for this.

- [ ] **R-7 Row A10 `Missing → Have`, and the 1.0.0 bump.** Last, and only after a confirmed
      installer (Q9). Bumping to 1.0.0 before that is forbidden by Q7's intent even though Q7
      assigns P2.8 to 1.0.0, because a version string names a build a human has run.
      Acceptance test first: `the_parity_matrix_promotes_a10_only_with_a_recorded_installer` —
      a docs test asserting the matrix row and a recorded artefact id move together.
      **NEW ASSERTION:** that A10 cannot be promoted without a named installer artefact in the
      release notes, mirroring the guard `release_version.rs` already applies to version carriers.

- [x] **T-8 Reader's kern term omitted `Tz`.** Landed 2026-11-01. `tj_fragment_origins`
computes the kern as `pen_x -= number / 1000.0 * state.size` with no
`horizontal_scale / 100` factor, while `spaced_width` applies that factor to advance and
spacing. So on any page at `Tz != 100` every run after a kern is misplaced. Found while
ruling R10 and **deliberately not fixed there** (`docs/P2.8-reflow-DESIGN.md` R10.1).
Measured, not argued, on `kerned-line-tz.pdf` at `Tz 50` through `locate_runs`: the reader
puts `CD` at x=**94.008** where PDFium drew it at x=**91.008** — off by **3.000 pt**,
which is `250/1000 x 24 x 0.5`. At `Tz 100` the two agree, which is why the suite is green.
Acceptance test first: the T-8 assertion below, which does not exist yet.
**NEW ASSERTION:** that `locate_runs` on `kerned-line-tz.pdf` reports the second `TJ`
fragment within 0.05 pt of its PDFium glyph origin. **No existing test compares the reader
to the ink at a non-100 `Tz`.** `a_kerned_lines_glyphs_are_where_iso_9_4_3_puts_them_at_tz_50`
looks like it does and does not: it never calls `tj_fragment_origins`, it compares a
hand-computed expectation against PDFium's grid. Verified by mutation — replacing the kern
term with `pen_x -= 0.0` leaves **both** kerned-line tests green at `Tz 50` and `Tz 100`,
so neither can observe the reader (`R10.2` of the design doc). The fixture, the constants
and the expectation all exist; only the comparison against the reader is missing.
The assertion is RED today by construction: the measured gap is 3.000 pt against a 0.05 pt
tolerance, 60x it. Fix is one factor in one expression. **Blocks nothing in H4**: R-1..R-7
are reflow-side, and a reflow that moved a kern-positioned tail would inherit this error
(3 pt at `Tz 50`, 0 at `Tz 100`) — which is the reason to land T-8 **before R-4 starts
asserting positions**, not before R-1..R-3.

      **Done.** The reader now reads `pen_x -= number / 1000.0 * state.size *
      state.horizontal_scale / 100.0`, and two guards hold it:
      `the_reader_puts_a_kerned_tail_where_pdfium_drew_it_at_tz_50` (the acceptance test, red
      before at the measured 94.008 against PDFium's 91.008) and
      `the_kern_term_carries_the_horizontal_scale` (a hand-built text state, so the property is
      also covered by the plain suite — the PDFium one is `#[ignore]`d and absent from Linux CI).
      Both were mutation-verified with the factor removed, and both fail. Counts moved
      526 -> 527 and 616 -> 618, ignored 90 -> 91, **measured across all four carriers**.
      Two facts measured while landing this, both recorded rather than fixed here:
      (a) the reflow's own kern term (`reflow.rs`, `kern_after_target`) also omits the factor and
      its comment says so — it is now a *separate* defect, filed as **T-8b** below, because
      fixing it changes `moved_to` for `Tz != 100` pages and no R-4 guard exists yet to hold it;
      (b) at `Tz 50` the reflow still reports `moved_to = 136.704` for `ABab`, which is correct
      only because `shift` compares two quantities that carry the same unscaled kern.

- [ ] **T-8b Reflow's kern term omits `Tz` too.** Discovered 2026-11-01 while landing T-8.
      `reflow_tj_array` computes `kern_after_target` as `number / 1000.0 * run.font_size` with no
      `horizontal_scale / 100` factor, and its own comment records the omission ("Everything here
      is unscaled text space: `Tz` does not enter"). That was true only while the reader shared
      the assumption; the reader is now scaled, so on a `Tz != 100` page `moved_to` is off by
      `n/1000 x Tf x (1 - Tz/100)` — 3 pt at `Tz 50`.
      Measured, not argued: `reflow_tj_array` on `kerned-line-tz.pdf` with `AB -> ABab` reports
      `moved_to = 136.704`, the `Tz 100` answer, on a page at `Tz 50`.
      Acceptance test first: `a_reflows_reported_tail_origin_matches_the_ink_at_tz_50` — compare
      `applied.moved_to` against PDFium's glyph origin on `kerned-line-tz.pdf`, with the
      `Tz 100` reading asserted different first so a fixture that lost its `Tz` cannot pass.
      **NEW ASSERTION:** that `moved_to`, not merely `shift`, is the quantity compared to ink.
      `a_kerned_prefix_includes_the_kern` compares `moved_to` to a *hand-computed* expectation
      and would go on agreeing with itself. Not blocking R-4, which is at `Tz 100`.

- [ ] **R-H2 Add `cargo fmt --check` to the gate.** **New work discovered 2026-11-01.** The
      gate script runs `cargo test` (both shapes), `cargo clippy`, `tsc` and `vitest`, and CI runs
      `cargo fmt --check`; the *local* gate does not. Measured: after R-2a landed, the committed
      tree at `f01c792` was **not** `cargo fmt`-clean (`cargo fmt --check` named two `Tm`
      operator vectors in `testpdf.rs`), and nothing in the loop reported it. R-2b's own run
      formatted them as a side effect, which is how the drift was found rather than avoided.
      A gate that is a strict subset of CI's is a gate that can report green on a tree CI rejects.
      Acceptance test first: `the_gate_runs_every_check_ci_runs` — compare the `run:` commands in
      `ci.yml` against the commands in `scripts/ralph/gate.sh` and fail on a check present in one
      and absent from the other, with an explicit allow-list for steps that cannot run locally.
      **NEW ASSERTION:** that the comparison is by command, not by step name, and that the
      allow-list is non-empty-and-claimed rather than a wildcard.

## Loop hygiene (do every iteration)

- [ ] Update `LOOP_LOG.md` with the iteration record.
- [ ] Remove any probe/example file created for investigation.
- [ ] `git status` must show only the intended files before committing.
