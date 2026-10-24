# Implementation plan — consumed by the Ralph loop

Source of truth for what to do next. The loop picks the **first unchecked task** whose
dependencies are met, does it, proves it, and checks it off.

Ruled decisions live in `docs/P2.7-REVIEW-OPEN-QUESTIONS.md` §F. Where this file and that
document disagree, that document wins.

Status legend: `[ ]` todo · `[~]` in progress · `[x]` done · `[!]` blocked (needs a ruling)

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
- [ ] **T-4** Version bump to 0.5.0 (`Cargo.toml`, `package.json`, `tauri.conf.json`), release
      notes listing `Ts`, PDFDocEncoding, copy-on-write, and the MCID work.
- [ ] **T-5** Commit the P2.7.1 body (code + fixtures + docs) as one feature commit, separate
      from H0.

## H2. Next phase (order fixed by B-3 and the plan's own rule)

- [!] **P2.7.3 XObjects** — copy-on-write per B-7. **Blocked on:** design doc
      (`P2.7.3-xobject-DESIGN.md`) not written. Needs a ruling on nesting depth.
- [!] **P2.7.4 Transforms** — **Blocked on:** design doc. H0 ships the refusal; this phase
      replaces the refusal with correct measurement.
- [!] **P2.7.5 CID/Identity-H** — **Blocked on:** design doc + the C-4 ceiling decision.
- [!] **P2.7.6 Embedding (≡ Stage 2 MECH-B)** — **Blocked on:** the C-5 font-bundling decision.
- [!] **P2.8 Line-level reflow** — **Blocked on:** design doc + C-6 multi-level undo
      infrastructure. Invariants are already fixed (see the plan §4).

## Loop hygiene (do every iteration)

- [ ] Update `LOOP_LOG.md` with the iteration record.
- [ ] Remove any probe/example file created for investigation.
- [ ] `git status` must show only the intended files before committing.
