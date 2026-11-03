# Rename housekeeping — product name and bundle identifier → **DocuPro**

Status: **DESIGN — APPROVED 2026-11-03, implementation in progress.** Governed by the stage gate in
`docs/P3-PLAN.md` §4: design, freeze decisions, sign-off, then implementation. This doc covers
the **Rename housekeeping** row of `docs/P3-PLAN.md` §1, ruling **R5** in `docs/DECISIONS.md` §9,
target version **1.0.1** (reserved by the tree). Requirements source: the report at
`docs/reference/acrobat/acrobat-reader-feature-extraction-report.md` — this phase is
branding housekeeping and claims **nothing** from the report's capability catalog; the report
sections it does not cite are not applicable, and that is stated rather than left ambiguous.

> **Name ruling, 2026-11-03.** R5 originally read *docu PRO*; the user re-ruled the name on
> 2026-11-03 as **DocuPro**, one word, exactly that spelling, with nothing appended — no
> platform word, no qualifier. An intermediate name (`DocuProOpenClaw`) was carried in one
> relay and **withdrawn by the user before any commit landed**; it never reached the tree, and
> no file in this repository has ever contained it. D-R1 records the ruling; this note exists
> so a later reader who finds the withdrawn name in a chat log knows it was never in force.

> **What this phase is not.** It is not a product rename of the codebase, the repository, the
> crate, or the npm package. R5 names the *product*; the tree keeps the identifiers that
> would force a churn of paths, imports, CI caches and git remotes with no user-visible
> benefit. See D-R7 — the repository name is explicitly **not** renamed without a separate
> ruling.

## 1. Verified capability facts (the ground the design stands on)

Checked against the tree at `96e7c2a` (version 1.0.1), `tauri 2.12.0` and `tauri-utils 2.10.0`
sources. Each fact names how it was checked, because "the app is called Document Reader" was
itself only a guess until it was read off the config.

| # | Fact | Consequence |
|---|------|-------------|
| **N1** | `src-tauri/tauri.conf.json` carries the name in **four** places: `productName: "Document Reader"`, `identifier: "com.documentreader.app"`, `app.windows[0].title: "Document Reader"`, and (indirectly) the installer filename Tauri derives from `productName`. | Four edits, one commit; a partial edit yields an installer named one thing and a window titled another. |
| **N2** | The NSIS bundler names the artifact `{productName}_{version}_x64-setup.exe`. The published 1.0.0 asset was `Document.Reader_1.0.0_x64-setup.exe` while the disk file carried a **space** (`Document Reader_1.0.0_x64-setup.exe`), because GitHub rewrites spaces in asset names. | The disk and published names differ **only when `productName` contains a space**. `DocuPro` does not, so from 1.0.1 the two forms coincide exactly — see D-R8, whose earlier two-form rule this fact retires. |
| **N3** | `tauri-utils 2.10.0` (its config module): the identifier "must contain only alphanumeric characters (A-Z, a-z, and 0-9), hyphens (-), and periods (.)", and "must be unique across applications since it is used in system configurations like the bundle ID and path to the webview data directory". | Under the current ruling this is a **legal** value: `DocuPro` has no space and is pure alphanumeric, so the identifier constraint no longer forces anything. `com.docupro.app` is therefore chosen for **lower-case convention and upgrade identity**, not because the constraint demands it. Stated plainly because the earlier draft of this fact justified the identifier by a space that no longer exists. |
| **N4** | `tauri 2.12.0` (its path module's desktop implementation): `app_data_dir()` resolves to `data_dir()/{bundle_identifier}`. There is no `appDirectoriesOverride` in this config and no migration code anywhere in `src-tauri/src/`. | **Changing the identifier moves the database.** The library at `%APPDATA%\com.documentreader.app\library.db` becomes unreachable at `%APPDATA%\<new-id>\library.db`. This is the phase's one real correctness obligation, and it is not a cosmetic concern. |
| **N5** | `src-tauri/src/lib.rs` joins `app_data_dir()` with `library.db` in `setup()`, and the path is not user-configurable. | The move has exactly one destination and one source; a one-shot migration is a bounded, testable operation rather than a search problem. |
| **N6** | The crate name (`src-tauri/Cargo.toml` `name = "document-reader"`), the npm name (`package.json` `"name": "document-reader"`), and the repository name are **independent** of `productName` and of the identifier. None is read at runtime to build a path or a title. | They may stay. Renaming them is optional churn, and R5 does not ask for it. |
| **N7** | The user-facing name also appears in `index.html` `<title>`, `src/components/Toolbar.tsx` (`app-name` span), and `src/components/Notices.tsx` (the privacy sentence). | Three frontend strings; the Notices one is a **sentence**, not a label, so it needs a grammatical substitution rather than a token replace. |
| **N8** | `README.md` carries the name in its `# Document Reader` heading and in a troubleshooting path — `$env:LOCALAPPDATA\Programs\Document Reader\pdfium.dll`. The latter is the **install directory**, which NSIS derives from `productName`. | The README path is a factual claim about where the installer puts files; it must change with `productName` or the README becomes wrong. This is the only occurrence in the tree that would actively mislead a user if missed. |
| **N9** | No Rust or TS test asserts the current product name. The only test mentioning it is a **doc comment** in `src-tauri/tests/release_version.rs` (`Document Reader_0.8.1_x64-setup.exe`), which is historical narrative about a past release, not a live assertion. | The rename breaks no guard, so no guard currently protects it. That is a reason to add one (§5), not a reason to relax. |
| **N10** | `src-tauri/tests/release_version.rs` reads the version out of `tauri.conf.json`; it does not read `productName`. | A guard that ties the six carriers together exists for the *version*, and an equivalent must be added for the *name* — otherwise the four N1 sites can drift exactly the way the version carriers could before that guard existed. |

## 2. Frozen decisions (rename housekeeping)

Sign-off on this table authorises implementation. D-R1 through D-R9 are the phase's contract.

| ID | Decision | Rationale |
|----|----------|-----------|
| **D-R1** | The product/display name is exactly **DocuPro** — capital `D`, capital `P`, one word, no space, no hyphen, and nothing appended: not `docu PRO`, not `Docu Pro`, not `DocuProOpenClaw`, and not qualified by a platform or edition word. | The user re-ruled the name on 2026-11-03 and stated the spelling explicitly, including "no platform word or anything else appended". A name is the one string where "improving" it is a defect, and an appended qualifier is the specific failure the ruling names. |
| **D-R2** | `productName` and `identifier` change **together in one atomic commit**. No commit may contain one without the other. | N4: a partial change is not a cosmetic slip. `productName` alone renames the installer and install directory; `identifier` alone moves the data directory, leaving an install that looks unchanged and has lost its library. The pair is the change. |
| **D-R3** | The bundle identifier becomes **`com.docupro.app`**. Approved by the user on 2026-11-03. | Reverse-DNS, legal under N3, and derived from the ruling name so the two cannot drift apart. It is deliberately **not** `com.documentreader.app`, because an identifier that survives the rename defeats the point of recording the upgrade consequence. Lower-case is convention for reverse-DNS identifiers rather than a constraint, since N3 would accept `com.DocuPro.app` — recorded because the earlier draft wrongly justified the lower case by a space constraint that no longer applies. **See D-R4 for why changing it costs a migration.** |
| **D-R4** | The identifier change **moves the app data directory** (N4). The phase therefore ships a **one-shot migration**: at startup, if the new `app_data_dir()` holds no `library.db` and the old `%APPDATA%\com.documentreader.app\library.db` exists, copy it across *before* opening the database. The old directory is left in place, untouched. | Without this, an existing hand-test install silently opens an empty library — data loss from the user's point of view, and the worst possible outcome for a housekeeping phase. Leaving the old copy means the migration is idempotent and reversible by hand if it ever goes wrong; deleting the source would make a failed copy unrecoverable. |
| **D-R5** | Migration is **copy, never move, never delete**, and it is **skipped silently when the new directory already has a database** — a second launch must not overwrite a library the user has since edited. | Idempotency is the only property that makes a startup migration safe across crashes. "Already migrated" and "fresh install" must reach the same no-op. |
| **D-R6** | The migrated state is **visible, once**, in the Notices/About surface — a dated line naming the old database path. Not a modal, not a blocking dialog. | The user's library moved without them asking. Rule 5 (silent skips are banned) applies to silent *successes* too; a migration the user cannot see is indistinguishable from one that did not happen. |
| **D-R7** | The git repository name, the git remote, the crate name `document-reader`, and the npm package name `document-reader` are **NOT renamed** in this phase, and not without a separate explicit user ruling. | N6: none affects the runtime or the user. Renaming the remote would break every existing clone, every CI trigger and every recorded commit reference in these documents — a large, irreversible-in-practice change for zero product effect. Called out as a decision rather than an omission so a later reader does not "finish the job". |
| **D-R8** | The installer filename follows `productName` automatically (N2) and is **not** overridden. Its expected form is **`DocuPro_1.0.1_x64-setup.exe`** on disk **and** as published — one name, not two. | Overriding the artifact name would decouple two things Tauri keeps consistent for free, and would make the release notes' provenance table describe a name no future build produces. N2's space-rewrite rule applied only to a name containing a space; `DocuPro` has none, so the earlier two-form rule in this row is retired rather than carried forward as folklore. |
| **D-R9** | Window title, `index.html` title, the Toolbar label, and the Notices privacy sentence all become **DocuPro**, and the Notices sentence reads `DocuPro edits PDFs on this machine.` The README heading and the install-directory path follow. | N7/N8. The sentence is still prose and is still checked as a sentence, but the one-word name removes the specific hazard the earlier draft had to guard — there is no leading lower-case token that a blind replace could strand. The README path is a factual claim (N8) and is the one occurrence whose staleness would mislead. |

## 3. What changes, file by file

| File | Change | Guard |
|------|--------|-------|
| `src-tauri/tauri.conf.json` | `productName`, `identifier`, `app.windows[0].title` | new name guard (§5) |
| `src-tauri/src/lib.rs` | startup migration before `Db::open` (D-R4/D-R5) | **a_moved_database_is_copied_before_it_is_opened** (planned) |
| new migration module in `src-tauri/src/editor/` | new **migrate_legacy_data_dir** (planned) helper, path-injectable so it is testable without `%APPDATA%` | fixtures in a temp dir |
| `src/components/Toolbar.tsx` | `app-name` label | vitest |
| `src/components/Notices.tsx` | privacy sentence (D-R9) + one-time migration line (D-R6) | vitest |
| `index.html` | `<title>` | name guard |
| `README.md` | heading + install-directory path | name guard |

**One commit.** D-R2 makes the config pair indivisible; the frontend and README strings ride
with it so no commit in the history shows a half-renamed product.

## 4. Risks

| Risk | Mitigation |
|------|-----------|
| Existing install opens an empty library | D-R4 migration + a test that fails without it (the phase's single most important test) |
| Migration overwrites a newer library on second launch | D-R5: skip when the destination exists; tested by running it twice |
| Migration half-completes on a crash | Copy to a temp name then rename; the source is never deleted, so a retry converges |
| Four name sites drift out of sync | name guard (§5), mirroring the version-carrier guard that exists for exactly this reason |
| The published asset name differs from the disk name | D-R8: under `DocuPro` the two forms coincide (N2), so there is one name to check and one name to record |
| Someone "finishes" the rename by moving the remote | D-R7 states the boundary; a later reader finds the decision, not an omission |
| A later reader reintroduces a qualifier, e.g. a platform word | D-R1 names the failure explicitly; the name guard (§5) compares against `productName`, so any appended word fails at the config, not at review |

## 5. New guard (acceptance)

**the_product_name_is_stated_once_and_agrees_everywhere** (planned) — reads `productName` from
`src-tauri/tauri.conf.json`, then asserts that the window title, `index.html` `<title>`, the
Toolbar label and the README heading all carry exactly that string, and that `identifier`
matches the reverse-DNS shape in N3 (alphanumerics, hyphens, periods only). Mutation: change
any one site to `Docu PRO` and the guard must fail naming that site (N10 — no such guard
exists today, which is why the four sites could drift).

The migration test **a_moved_database_is_copied_before_it_is_opened** (planned) runs against two temp
directories, so it never touches a real profile. Mutation: delete the copy call and it must
fail with the empty-library symptom (D-R4), not with a compile error.

## 6. Definition of done

1. One commit carrying `productName` + `identifier` + title + three frontend strings + README
   (D-R2, D-R9).
2. Migration test green and mutation-verified; second-run idempotency covered (D-R5).
3. Name guard green and mutation-verified (§5).
4. `cargo fmt --check`, clippy deny-warnings, both `cargo test` shapes, `tsc`, vitest — all green,
   counts recorded in the commit message.
5. `docs/RELEASE-NOTES.md` gains its `## 1.0.1` content **before** the bump lands (§4 of the
   programme protocol), including the migration and the accepted reinstall consequence.
6. Installer built by build-only dispatch, **user hand-confirms**, then tag — the recorded
   0.9.0/Q9 order. The user must confirm the Add/Remove double-entry and the migrated library.

## 7. Sign-off record

**Approved 2026-11-03.** Both freeze tables were signed off: this phase's D-R1 through D-R9
(including the copy-never-move migration machinery, D-R4/D-R5) and the P3.1 tables D-A1 through
D-A10 in `docs/P3.1-annotations-DESIGN.md`.

The three questions this section previously carried are resolved rather than dropped:

1. **The identifier (D-R3)** — approved as `com.docupro.app` under the re-ruled name. It is the
   Windows upgrade identity and the data-directory name; changing it after release repeats the
   migration cost.
2. **The migration (D-R4)** — approved. The copy ships; the "accept an empty library" alternative
   was considered and rejected because it loses the user's library.
3. **The boundary (D-R7)** — approved. The repository, remote, crate and npm names stay
   `document-reader` until a separate ruling says otherwise.

**One consequence still to be confirmed by hand, not by CI:** the upgrade installs over
`Document Reader` as a *different* application. Add/Remove Programs will list both until the old
entry is removed, and the user accepted that consequence in `docs/DECISIONS.md` §9 item 5. The
hand-test checklist in §6 item 6 asks for it to be observed, not assumed.
