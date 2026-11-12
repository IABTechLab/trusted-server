# Design Spec: A module's browser script lives in the crate that supplies it

**Status:** Proposed, 2026-10-19. Implemented as the draft pull request [IABTechLab/trusted-server#1423](https://github.com/IABTechLab/trusted-server/pull/1423), row 55 of the stack, above [IABTechLab/trusted-server#1412](https://github.com/IABTechLab/trusted-server/pull/1412), which serves asset routes on every adapter, for Tech Lab review. [IABTechLab/trusted-server#1342](https://github.com/IABTechLab/trusted-server/pull/1342) carries every design document of the stack in one place.
**Author:** 51Degrees (contributed), for Tech Lab review
**Related specs:** [integration-provider-seam](https://github.com/IABTechLab/trusted-server/blob/feature/6-integration-seam/docs/superpowers/specs/2026-08-27-integration-provider-seam-design.md), [page-middleware](https://github.com/IABTechLab/trusted-server/blob/feature/6-integration-seam/docs/superpowers/specs/2026-10-12-page-middleware-design.md), [no-modules-crate](https://github.com/IABTechLab/trusted-server/blob/feature/6-integration-seam/docs/superpowers/specs/2026-10-14-no-modules-crate-design.md), and the design documents of the stack in IABTechLab/trusted-server#1342
**Related PRs:** [IABTechLab/trusted-server#1423](https://github.com/IABTechLab/trusted-server/pull/1423). [IABTechLab/trusted-server#1348](https://github.com/IABTechLab/trusted-server/pull/1348), which moves every vendor module's Rust out of core into `crates/<type>/<vendor>/`
**Issues:** none
**Last updated:** 2026-10-20

> **Why this spec exists.** Each vendor module's Rust lives in its own crate, `crates/<type>/<vendor>/`, but its browser script, the script's tests and some of their test data live in `crates/trusted-server-js/lib/src/integrations/<module>/` and `lib/test/integrations/<module>/`, a folder that belongs to no vendor. Whoever maintains a module works in two places, and the shared browser crate holds every vendor's code. This change moves ten modules' scripts and tests into the nine crates that supply them, makes the build, the tests, the lint and the format checks, and editors, find them there, and moves the Tinybird project into the Tinybird crate. The 14 built bundles are byte for byte the same as before.

## 1. Problem

`build-all.mjs` builds a bundle for every folder in `lib/src/integrations/` that holds an `index.ts`, and `expected_module_ids` in `crates/trusted-server-js/build/bundle_set.rs` lists the same folders, so the build script embeds exactly the bundles the Node build wrote. Vitest, ESLint, Prettier and TypeScript are all configured for `crates/trusted-server-js/lib` and see no file outside it. A module's script reaches the shared browser code by relative paths such as `../../core/log`, and its tests reach the script by paths such as `../../../src/integrations/lockr/script_guard`.

So a module crate such as `crates/identity/lockr` holds the module's Rust, its settings and its page-change fixtures, while the script that runs in the reader's browser and that script's tests sit in another crate, under a folder shared by every vendor. Three tests in the shared crate also use vendor scripts. The shared DOM insertion dispatcher's test imports five vendors' script guards, the Prebid shim's test imports the GPT and GPT diagnostics scripts, and the IIFE state test builds the Permutive bundle. The GPT script imports the APS renderer.

The Tinybird module has the same split. Its Rust is in `crates/analytics/tinybird`, and the Tinybird project it sends to, its datasources, pipes, fixtures and tests, is in a `tinybird/` folder at the repository root.

## 2. Design

### 2.1 Where a module's script lives

A crate that supplies a browser module keeps the script in `js/src/<module>/`, with `index.ts` as the bundle's entry, and the script's tests in `js/test/<module>/`. Test data only that crate's tests use goes in `js/test/fixtures/`. The module's name is its folder's name, as before, so one crate can supply more than one module.

| Module               | Crate                            |
| -------------------- | -------------------------------- |
| `datadome`           | `crates/bot-protection/datadome` |
| `didomi`             | `crates/cmp/didomi`              |
| `google_tag_manager` | `crates/tag/google-tag-manager`  |
| `gpt`                | `crates/ad-tag/google`           |
| `gpt_diagnostics`    | `crates/ad-tag/google`           |
| `lockr`              | `crates/identity/lockr`          |
| `osano`              | `crates/cmp/osano`               |
| `permutive`          | `crates/audience/permutive`      |
| `sourcepoint`        | `crates/cmp/sourcepoint`         |
| `testlight`          | `crates/testing/testlight`       |

Git detects each move as a rename, because each moved file keeps most of its content, so `git log --follow` and blame keep each file's history. The GPT tests' helper that locates `gpt_bootstrap.js`, `test/fixtures/paths.ts`, moves into `crates/ad-tag/google/js/test/fixtures/`, because only the GPT tests use it.

A vendor crate can now hold browser code in three ways, and this change adds only the first.

- **Module scripts**, in `js/src/<module>/` and `js/test/<module>/`, which the shared build bundles and which the checks in 2.4 cover.
- **Inline page scripts** in `src/*.js`, such as `crates/ad-tag/google/src/gpt_bootstrap.js` and InMobi's `gpp_stub.js` and `tcf_stub.js`, which the crate's Rust embeds into the page. InMobi's are tested by `node --test` in the last step of the Vitest workflow job, as before.
- **A carried script** directly in `js/`, such as `crates/testing/iabtl/js/probe.js`, which the crate embeds with `include_str!` and hands to the registry as a `CarriedJsModule` with its SHA-256.

So a folder named `js/` holds either module scripts, in `js/src` and `js/test`, or a carried script directly in `js/`.

### 2.2 How the build finds them

`moduleSourceDirs()` in `lib/build-module-options.mjs` lists every module's source folder by name, `lib/src/integrations/<name>` for the modules that stay in the shared crate and `crates/<type>/<crate>/js/src/<name>` for the rest. A name found in two places is an error, because bundles and imports are keyed by name, and so is a module named `core`, which is the shared core's bundle. A link whose target is gone counts as no folder. `build-all.mjs` builds a bundle for each listed folder whose `index.ts` is a file.

`expected_module_ids` in `build/bundle_set.rs` applies the same rule from the Rust side, given the crates folder as well as `lib/src`, so `build.rs` still embeds exactly the bundles the Node build wrote. Both sides have tests of the module list, a name found twice and a module named `core`, in `build/bundle_set.rs` and `lib/test/module-discovery.test.mjs`, and the JavaScript tests also walk folders in a scratch tree. The Rust tests check `module_ids`, which takes the folders the walk found, because the crate's tests also run as WebAssembly under Viceroy, where there is no file system. Bundles are still named `tsjs-<module>.js`, so the Rust API, the served URLs and every content hash are unchanged.

`build.rs` asks Cargo to rerun it when `lib/build-module-options.mjs` or any existing `crates/<type>/<crate>/js/src` folder changes, and `js/test` as well when `TSJS_TEST=1` runs the tests. It names only folders that exist, because Cargo reruns a build script on every build while a watched path is missing. A crate's first `js/src` folder is therefore picked up by the next build that runs for another reason, which `crates/trusted-server-js/README.md` says, with touching `lib/package.json` as the way to force it.

### 2.3 How scripts and tests import

`lib/build-module-options.mjs` defines `@tsjs/core`, `@tsjs/shared` and `@tsjs/module/<module>` for the build and for Vitest, Vitest's config adds `@tsjs/test`, and `tsconfig.json` maps the same names for TypeScript.

- `@tsjs/core/...` and `@tsjs/shared/...` reach the shared browser code from a script in any crate.
- `@tsjs/module/<module>/...` reaches another module's code by the module's name rather than by the folder it lives in. The GPT script uses it for the APS renderer, and the dispatcher and Prebid tests use it for the vendor scripts they exercise, so an import of a crate script's code by another module can be found by searching for `@tsjs/module/`. Modules kept in `lib` still import each other by relative path, as the Prebid shim imports the APS renderer, and the IIFE state test finds the Permutive bundle's entry through `moduleSourceDirs()`.
- `@tsjs/test/...` reaches test data that more than one module's tests share, which is `test/fixtures/aps-renderer-v1.json`, used by the core, APS, GPT and Prebid tests.

TypeScript's `paths` cannot match a folder by pattern, so `tsconfig.json` lists each crate's `js/src` folder under `@tsjs/module/*`, and a test checks that the list matches what `moduleSourceRoots()` finds, in the same order. A test imports its own module's script by a short relative path within the crate, for example `../../src/lockr/script_guard`.

A script in a crate cannot import an npm package. Vite and TypeScript look for a package by walking up from the importing file, and this crate's `node_modules` is not above any other crate. None of the moved scripts imports a package, and a script that needs one stays in `lib` for now, which is one reason the Prebid shim, which imports `prebid.js`, has not moved.

### 2.4 How the checks and editors find them

Vitest's `test.dir` is the crates folder, with `trusted-server-js/lib/test/**/*.test.{ts,mjs}` and `*/*/js/test/**/*.test.{ts,mjs}` as the test files, and `server.fs.allow` lets it read the crates folder. The type gate, which checks the GPT diagnostics export contract with `expectTypeOf`, covers `*/*/js/test/**/types.test.ts`, because that test moved with the GPT diagnostics script. `lib/tsconfig.json` includes `../../*/*/js`, maps the `@tsjs/...` names, and maps `vitest` into this crate's `node_modules`, which a test in another crate cannot find by walking up from its own folder.

ESLint's flat config ignores files outside the folder it runs from, and Prettier and TypeScript look for their settings by walking up from the file they read, so the crates folder holds three small files. `crates/eslint.config.js` applies `lib/eslint.config.js` to `*/*/js/src` and `*/*/js/test` and ignores everything else under the crates folder. ESLint 9 finds it when it runs from the crates folder, as `npm run lint` runs it, and an editor uses it only when it runs ESLint from there with `lib`'s copy of ESLint. `crates/.prettierrc.json` gives files under those folders, and the three files themselves, `lib/.prettierrc.json`'s options and sets nothing for any other file, and a test checks the options are the same. `crates/tsconfig.json` extends `lib/tsconfig.json` for those folders and the shared core's `global.d.ts`, so an editor checks a crate script as `lib`'s TypeScript run does. Vite also reads it when it compiles a crate script, so `build.rs` watches it.

`npm run lint` lints `lib` as before and then runs ESLint from the crates folder over `*/*/js/{src,test}/**/*.{ts,tsx,js,mjs}`, and `npm run format` checks those folders, each without `--config`, so the command line and an editor apply the same settings. The ESLint config treats `@tsjs/` imports as internal, orders the import groups built-in, external, internal, parent, sibling, index, and gives a crate's `js/test` `.mjs` scripts Node's globals as it gives `lib`'s. The CI jobs still run `npm run lint`, `npm run format`, `npm run build` and `npm test` in `crates/trusted-server-js/lib`, so no workflow changes for the scripts.

The lint and format checks did not cover inline page scripts or carried scripts before this change, and they still do not.

### 2.5 The Tinybird project

`tinybird/` moves to `crates/analytics/tinybird/project/`. Its README's links to the telemetry and configuration guides follow the move, the crate's README points at the project, the telemetry and configuration guides' links follow it, the Markdown format list in `.github/workflows/format.yml` and `AGENTS.md` drops `tinybird/**/*.md`, which `crates/**/*.md` now covers, and the changelog says where the project now is, because an operator runs `tb` from that folder. Because the project is now under `crates/`, a merge that changes only the project starts the documentation proposal workflow, `.github/workflows/docs-proposal.yml`, which runs on changes under `crates/**`.

### 2.6 Text that named the old places

Comments and pages that named a moved script by its old path now name the new one, in `crates/ad-tag/google/src/lib.rs`, `crates/cmp/sourcepoint/src/lib.rs`, `crates/trusted-server-core/src/publisher.rs`, the Didomi test and the GPT guide. The old path also appeared in three comment lines in `crates/ad-tag/google/src/gpt_bootstrap.js`, which the GPT module inlines into each page it changes, so readers receive those three comment lines with the new path, and the module's recorded page-change fixture changes with them. That is the only change a reader receives.

The `tsjs` guide said there are 12 integration source modules and 13 bundles. There are 13 and 14, because it missed `ec_client_fixed`, and it now says so, with where a vendor module's script lives. `AGENTS.md`, the onboarding guide, the error reference's entry for an invalid bundle set and the changelog name both places a module's script can be.

## 3. What is not in this change

- **APS's renderer**, `lib/src/integrations/aps/render.ts`, stays in the shared crate, because the shared browser core imports it in `core/auction.ts` and `core/request.ts` and it is compiled into the `core` bundle. It moves when APS rendering leaves the browser core.
- **Prebid**, its shim, its external bundle build `build-prebid-external.mjs` with its tests, and the Prebid code of the `ts prebid client` command in `crates/trusted-server-cli/src/prebid_bundle.rs`, moves together in a later change. The shim imports `prebid.js`, which a script in a crate cannot (2.3), and the CLI runs the bundle build from `lib` with `lib`'s `node_modules`.
- **The shared browser core still names vendors**, GPT, Prebid, APS, Lockr, DataDome and Google Tag Manager in `lib/src/core` and `lib/src/shared`. Taking them out is a later change, as [#1348](https://github.com/IABTechLab/trusted-server/pull/1348) and the pull requests above it did for the Rust core.
- **`creative` and `ec_client_fixed`** are core's own browser features and stay in the shared crate.
- **Vendor fixtures in the integration test crate** and **vendor pages in the docs site** stay, because the integration tests and the docs site build from their own folders.

## 4. Acceptance

- Each of the 14 bundles `build-all.mjs` writes has the same SHA-256 as on row 54.
- `npm run lint` and `npm run format` pass, and a planted unused variable in a crate's script makes `npm run lint` fail.
- Every Vitest file passes, including the discovery tests and the checks of the Prettier options and the `@tsjs/module` list, and the type gate covers the moved contract test, which a planted false `expectTypeOf` assertion makes fail. On Windows the two `build-prebid-external` tests that create symlinks fail, as they do on row 54, because Windows refuses symlinks without the privilege.
- `tsc --noEmit -p tsconfig.json` in `lib` reports no error that it does not report on row 54.
- `cargo test` and `cargo clippy -- -D warnings` pass for `trusted-server-js`, including its discovery tests, and for `trusted-server-ad-tag-google`, whose recorded fixture changed, and `cargo fmt --check` passes.
- The repository's workflows pass on the pull request.

## Revision record

| Date       | Change                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                    |
| ---------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 2026-10-19 | First draft.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                              |
| 2026-10-19 | Fixes from review round 1. TypeScript's `@tsjs/module/*` list and its test, the configuration guide's Tinybird link, the editor settings in the crates folder and their test, the npm package rule, `build.rs` watching `build-module-options.mjs`, discovery refusing `core` and passing over broken links on both sides with JavaScript tests, no unmatched-pattern pass for ESLint, `.mjs` crate tests, the three kinds of browser code a crate can hold, the changelog, `AGENTS.md` and the onboarding guide, and wording throughout. |
| 2026-10-19 | Corrections from review round 2. The `@tsjs/module` search claim, `crates/tsconfig.json` taking in the shared `global.d.ts`, how ESLint finds `crates/eslint.config.js`, `build.rs` watching `crates/tsconfig.json`, the documentation proposal workflow a Tinybird change starts, and Rust discovery passing over a module root that is a file.                                                                                                                                                                                          |
| 2026-10-20 | Names its pull request, IABTechLab/trusted-server#1423.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                   |
| 2026-10-20 | The Rust discovery tests check `module_ids` without a file system, because the WebAssembly test run under Viceroy has none.                                                                                                                                                                                                                                                                                                                                                                                                               |

## Review log

1. **Round 1, 2026-10-19.** A review in a fresh session, against row 55's branch as first committed, before it was rewritten and pushed. Issues found, two of them blocking. TypeScript had no mapping for `@tsjs/module/*`, so the GPT script was no longer type-checked against the APS renderer while this design said TypeScript used the names, and the configuration guide still linked the old Tinybird README. Of the others, editors found no settings for crate scripts, a script in a crate cannot import an npm package and the design did not say so, `build.rs` did not watch `build-module-options.mjs`, the crate lint run passed an empty match, the JavaScript and Rust discovery differed on broken links, an `index.ts` folder and a module named `core`, crate `.mjs` tests were neither run nor given Node's globals, and there was no changelog entry. This revision fixes each of them.
2. **Round 2, 2026-10-19.** A lighter review in a fresh session, against this page at `b89557183`, of the round 1 fixes and the new text. No blocking issues. One round 1 item was only partly fixed, the claim that every import of one module by another uses `@tsjs/module/`, which the Prebid shim's relative import of the APS renderer contradicted. It also found that `crates/tsconfig.json` left out the shared `global.d.ts`, that ESLint 9 finds its config from the folder it runs in rather than from the file, that the build reads `crates/tsconfig.json` without `build.rs` watching it, that Rust discovery failed on a module root that is a file where JavaScript passed over it, that the new configuration files and `.mjs` test were outside the format check, and that a Tinybird change now starts the documentation proposal workflow. This revision fixes each of them, and the design joins the specs pull request when the row is pushed.
