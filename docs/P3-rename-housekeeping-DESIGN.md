# Rename housekeeping — product name and bundle identifier → **docu PRO**

Status: **DESIGN — frozen pending user sign-off. No code written.** Governed by the stage gate in
`docs/P3-PLAN.md` §4: design, freeze decisions, sign-off, then implementation. This doc covers
the **Rename housekeeping** row of `docs/P3-PLAN.md` §1, ruling **R5** in `docs/DECISIONS.md` §9,
target version **1.0.1** (reserved by the tree). Requirements source: the report at
`docs/reference/acrobat/acrobat-reader-feature-extraction-report.md` — this phase is
branding housekeeping and claims **nothing** from the report's capability catalog; the report
sections it does not cite are not applicable, and that is stated rather than left ambiguous.

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
| **N2** | The NSIS bundler names the artifact `{productName}_{version}_x64-setup.exe`; the published 1.0.0 asset was `Document.Reader_1.0.0_x64-setup.exe` — a **period**, not a space, because GitHub rewrites spaces in asset names. The bytes on disk carry the space. | The local filename and the published asset name differ by one character. Any guard must compare the right one; `docs/RELEASE-NOTES.md` §1.0.0 already records both. |
| **N3** | `tauri-utils 2.10.0` (its config module): the identifier "must contain only alphanumeric characters (A-Z, a-z, and 0-9), hyphens (-), and periods (.)", and "must be unique across applications since it is used in system configurations like the bundle ID and path to the webview data directory". | A space is not legal in an identifier. `docu PRO` cannot appear in the identifier verbatim; the identifier needs its own spelling. |
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
| **D-R1** | The product/display name is exactly **docu PRO** — lower-case `docu`, one space, upper-case `PRO`, as the user wrote it in R5. It is not title-cased to `Docu PRO`, not `DocuPro`, not `docu pro`, and not expanded. | R5 is the ruling and the spelling is the user's. A name is the one string where "improving" it is a defect. |
| **D-R2** | `productName` and `identifier` change **together in one atomic commit**. No commit may contain one without the other. | N4: a partial change is not a cosmetic slip. `productName` alone renames the installer and install directory; `identifier` alone moves the data directory, leaving an install that looks unchanged and has lost its library. The pair is the change. |
| **D-R3** | The bundle identifier becomes **`com.docupro.app`**. Proposed for approval; the user may replace it and D-R3 is rewritten before implementation. | Reverse-DNS, legal under N3 (alphanumerics and periods only; no space, which N3 forbids), and carries no vendor domain we do not own. It is deliberately **not** `com.documentreader.app`, because an identifier that survives the rename defeats the point of recording the upgrade consequence. **See D-R4 for why changing it costs a migration.** |
| **D-R4** | The identifier change **moves the app data directory** (N4). The phase therefore ships a **one-shot migration**: at startup, if the new `app_data_dir()` holds no `library.db` and the old `%APPDATA%\com.documentreader.app\library.db` exists, copy it across *before* opening the database. The old directory is left in place, untouched. | Without this, an existing hand-test install silently opens an empty library — data loss from the user's point of view, and the worst possible outcome for a housekeeping phase. Leaving the old copy means the migration is idempotent and reversible by hand if it ever goes wrong; deleting the source would make a failed copy unrecoverable. |
| **D-R5** | Migration is **copy, never move, never delete**, and it is **skipped silently when the new directory already has a database** — a second launch must not overwrite a library the user has since edited. | Idempotency is the only property that makes a startup migration safe across crashes. "Already migrated" and "fresh install" must reach the same no-op. |
| **D-R6** | The migrated state is **visible, once**, in the Notices/About surface — a dated line naming the old database path. Not a modal, not a blocking dialog. | The user's library moved without them asking. Rule 5 (silent skips are banned) applies to silent *successes* too; a migration the user cannot see is indistinguishable from one that did not happen. |
| **D-R7** | The git repository name, the git remote, the crate name `document-reader`, and the npm package name `document-reader` are **NOT renamed** in this phase, and not without a separate explicit user ruling. | N6: none affects the runtime or the user. Renaming the remote would break every existing clone, every CI trigger and every recorded commit reference in these documents — a large, irreversible-in-practice change for zero product effect. Called out as a decision rather than an omission so a later reader does not "finish the job". |
| **D-R8** | The installer filename follows `productName` automatically (N2) and is **not** overridden. Its expected form is recorded in the release notes as `docu PRO_1.0.1_x64-setup.exe` on disk and `docu.PRO_1.0.1_x64-setup.exe` as published. | Overriding the artifact name would decouple two things Tauri keeps consistent for free, and would make the release notes' provenance table describe a name no future build produces. |
| **D-R9** | Window title, `index.html` title, the Toolbar label, and the Notices privacy sentence all become **docu PRO**, and the Notices sentence is reworded so it still reads as a sentence — `docu PRO edits PDFs on this machine.` The README heading and the install-directory path follow. | N7/N8. The sentence is prose; a blind token replace of a name inside a sentence is how "PRO edits PDFs" ships. The README path is a factual claim (N8) and is the one occurrence whose staleness would mislead. |

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
| The published asset name differs from the disk name | D-R8 records both; a guard compares the disk form, which is the one a build produces |
| Someone "finishes" the rename by moving the remote | D-R7 states the boundary; a later reader finds the decision, not an omission |
| `docu PRO` contains a space where an identifier is required | N3: the identifier has its own spelling (D-R3); the space lives only in display strings |

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

## 7. Open for sign-off

1. **The identifier `com.docupro.app` (D-R3)** — approve, or supply a replacement. It is the
   Windows upgrade identity and the data-directory name; changing it after release repeats
   the migration cost.
2. **D-R4's migration** — confirm that copying the old database across is wanted, rather than
   accepting a fresh empty library on upgrade. The design recommends the copy; the alternative
   is legitimate for a hand-test-only install base and cheaper, but it loses the library.
3. **D-R7's boundary** — confirm the repository and crate names stay `document-reader`.
