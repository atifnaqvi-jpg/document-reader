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

- [ ] **R-1a Implement R10: the kerned `measured prefix`.** §7.2 was **ruled on 2026-10-31**
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

- [ ] **R-1b Refuse a kern the walk cannot attribute.** Same ruling's other half: a run whose
      position also depends on a preceding `Tj`, or on a `Td`/`TD` issued after the array began,
      is **refused** rather than measured. **Depends on R-1a** for the arithmetic it declines to
      compute without — a refusal that guards arithmetic which does not exist yet guards nothing.
      Acceptance test first: `a_kern_that_spans_a_tj_or_mid_array_td_refuses`.
      **NEW ASSERTION:** that a run positioned by a `Tj` before it, or by a mid-array `Td`, is
      refused with the R10b arm rather than measured. **Synthetic run pair, no new fixture** — the
      states are unreachable from the committed fixtures by construction, which is exactly why
      they must be built as runs rather than found as files.
      **Nothing exists — this is new code.** No refusal arm, no enum variant, no test.

- [ ] **R-2a The R8b fixture.** `two-mcid-one-baseline.pdf`: page content with two `BDC`/`EMC`
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
- [ ] **R-2b Refuse a line whose runs leave its merge scope (R8b).** §7.3 was **ruled on
      2026-10-31**, so this is code. `reflow_tj_array` takes `line_origin` as the minimum
      `origin.0` over every run on the baseline, and `on_baseline` filters on `baseline()`
      (`origin.1 + rise`) alone, with **no scope term**. A same-baseline run in another `MCID`,
      further left, makes `line_origin` too small and the `Td` offset correspondingly too large.
      **Depends on R-2a** for its fixture.
      Acceptance test first: `a_same_baseline_run_in_another_mcid_does_not_dislocate_the_tail`.
      **NEW ASSERTION:** that the edit refuses with `R8b-scope-left-of-line` rather than computing
      a wrong `line_origin`. **Nothing exists** — no variant, no gate, no test. Red against R-2a's
      fixture because `on_baseline` carries no tag term.
      The reason it must refuse rather than correct the arithmetic is recorded as R8b.1 in the
      design doc: `line_origin + prefix` cancels to `run.origin.0 + new_width`, so the record
      would report the intended position while the ink sat elsewhere.
- [ ] **R-2c Pin R5's user-visible string, and give R5 a real-content fixture.** **New work
      discovered 2026-10-31 while ruling §7.3, not a rename of anything.** Two gaps, named:
      **(i) the message is unasserted** — no test states the string a user reads for R5;
      `message()` (`reflow.rs:67`) is pinned nowhere, and `a_line_that_crosses_content_levels_
      refuses_under_r5` asserts only `name()`, the internal identifier.
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

- [ ] **R-3 The A10 fixture.** `two_run_line()` cannot carry A10 (§6.2: no second line, no `TJ`
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

- [ ] **R-4 Row A10's guard: the move itself.** Write the A10 assertions against R-3's fixture.
      Acceptance test first: `a_wider_replacement_moves_the_rest_of_the_line_and_nothing_else`.
      **NEW ASSERTION:** after a write of a *wider* replacement, every run on the edited line has
      moved by exactly the measured width of the replacement, every run on every other line has
      unmoved ink, **and** the edited run's extracted text equals the requested string.
      `a_hand_built_td_moves_line_one_and_leaves_line_two_alone` (`9471313`) is the closest and
      is **not** this: it moves a hand-built `Td` with **no replacement text**, so it asserts
      nothing about growth, nothing about the run's extracted text, and nothing about the
      measured-width relation. This is the entry the plan's A10 row is actually waiting on.
      **Partly exists — must be shown RED against `9471313`/`b260966`.**

- [ ] **R-5 Row A10's refusal case.** Same fixture, near-edge variant.
      Acceptance test first: `a_replacement_too_wide_for_the_content_box_refuses_and_writes_nothing`.
      **NEW ASSERTION:** the refusal goes through the content box's `fits-width` path (§4 R3) —
      not R4's caller-supplied `limit_x` — **and** the stream is byte-unchanged after the
      refusal. `a_replacement_past_the_limit_refuses_under_r4` (`b260966`) asserts R4 with a
      `limit_x` handed in by the caller, and never inspects the content stream afterwards, so it
      covers neither half of this.

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

- [ ] **T-8 Reader's kern term omits `Tz`.** `tj_fragment_origins`
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

## Loop hygiene (do every iteration)

- [ ] Update `LOOP_LOG.md` with the iteration record.
- [ ] Remove any probe/example file created for investigation.
- [ ] `git status` must show only the intended files before committing.
