# Design Spec: One integrations crate holds every module, in a folder for its type and vendor

**Status:** Proposed, 2026-10-26. For a new row at the top of the stack, row 56, which folds every module crate into one crate, as IAB Tech Lab asked at the Touchbase of 2026-10-23 and as decided by 51Degrees after the Touchbase. No pull request exists for the row yet.
**Author:** 51Degrees (contributed), for Tech Lab review
**Related specs:** `2026-10-14-no-modules-crate-design.md`, `2026-10-19-module-scripts-in-crates-design.md`, `2026-10-12-loading-cache-design.md`, `2026-10-15-asset-cache-design.md`, `2026-08-27-integration-provider-seam-design.md`, and IAB Tech Lab's `2026-09-17-split-integrations-crates-design.md` on [IABTechLab/trusted-server#1194](https://github.com/IABTechLab/trusted-server/pull/1194)
**Related PRs:** [IABTechLab/trusted-server#1194](https://github.com/IABTechLab/trusted-server/pull/1194) (Aram Grigoryan, draft), [IABTechLab/trusted-server#1423](https://github.com/IABTechLab/trusted-server/pull/1423) (row 55, the top of the stack below this row). [IABTechLab/trusted-server#1342](https://github.com/IABTechLab/trusted-server/pull/1342) carries every design document of the stack
**Issues:** [IABTechLab/trusted-server#777](https://github.com/IABTechLab/trusted-server/issues/777) lists the stack
**Last updated:** 2026-10-26

> **Why this spec exists.** The stack gives each module a crate of its own, 32 crates under `crates/<type>/<vendor>/`. At the Touchbase of 2026-10-23, IAB Tech Lab asked for those folders to be kept inside one crate, as Tech Lab's own design puts every integration in one crate, and 51Degrees agreed.

Paths, counts and line numbers are those of row 55 at `5be114773` unless a row or a commit is named.

## 1. Problem

Every module outside core is a crate of its own at `crates/<type>/<vendor>/`, whose package is `trusted-server-<type>-<vendor>`. There are 32. Each manifest names the module's maintainers in `[package.metadata.maintainers]`, being 6 `project owned`, 22 `seeking vendor owner` and 4 `vendor owned`, all four by 51Degrees, being the loading cache, the client hints middleware, the Model Terms signal and the code the 51Degrees modules share. The asset cache module of row 34 (`2026-10-15-asset-cache-design.md`) adds a 33rd, vendor owned by 51Degrees.

The crates are named one by one in many places. The workspace manifest lists each as a member, and each but the seam probe as a dependency. The seam probe, `crates/testing/iabtl`, is a fixture crate through which tests drive core's extension points from outside core. Each adapter's manifest names between 28 and 31 of the crates, the `ts` tool's names 23, and the permissions inspector under `tools/permissions-inspector/wasm`, a workspace of its own, names four. Each adapter's `shipped_module_builders()` and the tool's `linked_builders()` name each builder by its crate, six aliases in `.cargo/config.toml` list the packages, and two steps of `.github/workflows/test.yml` name a crate or a path below `crates/<type>/<vendor>/`.

IAB Tech Lab's design, `2026-09-17-split-integrations-crates-design.md` on the draft IABTechLab/trusted-server#1194, puts every concrete integration in one crate, `trusted-server-integrations`, each in a flat folder `src/integrations/<id>/`, and every integration's browser code in a second crate, `trusted-server-integrations-js`. It sets per-vendor crates aside because they "would multiply dependency, compatibility, release, and governance surfaces without a current external consumer". At the Touchbase of 2026-10-23, IAB Tech Lab asked for one crate that keeps the `<type>/<vendor>` folders, and 51Degrees agreed. As decided by 51Degrees after the Touchbase, one crate holds every module, its own crates and the asset cache module included, every file 51Degrees puts in the repository takes the repository's license, Apache-2.0, and the 51Degrees loading cache keeps depending on the package `fiftyone-caching`.

## 2. Design

### 2.1 The crate and its folders

Every module crate moves into one crate, `crates/trusted-server-integrations`, package `trusted-server-integrations`, the name #1194 gives it. Each crate's folder becomes a folder of the new crate's `src`, so `crates/<type>/<vendor>/` becomes `crates/trusted-server-integrations/src/<type>/<vendor>/`, and what was in the crate's own `src/` moves up into that folder, as the table below shows.

```text
crates/trusted-server-integrations/
  Cargo.toml
  src/
    lib.rs                   declares each type folder
    cmp/
      mod.rs                 declares each vendor folder of the type
      inmobi/
        mod.rs               was crates/cmp/inmobi/src/lib.rs
        maintainers.toml     was the manifest's maintainers table
        fixtures/            was crates/cmp/inmobi/src/fixtures/
        tests/               InMobi's node tests, as before
      didomi/
        mod.rs
        maintainers.toml
        js/src/didomi/       the module's browser script, as row 55 placed it
        js/test/didomi/
    loading-cache/
      mod.rs
      51degrees/
        mod.rs
        layer.rs
        stores.rs
        tests.rs
        maintainers.toml
```

| In a module crate                           | In the integrations crate                                                                                               |
| ------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------- |
| `src/lib.rs`                                | `src/<type>/<vendor>/mod.rs`                                                                                            |
| Every other file under `src/`               | The same name beside `mod.rs`, so `src/fixtures/` is `fixtures/`                                                        |
| `js/`, `tests/`, `project/` and `README.md` | The same, in the folder                                                                                                 |
| `.gitattributes`, in 13 of the folders      | The same, with its patterns read from the folder, so `src/fixtures/page-change.*.html` is `fixtures/page-change.*.html` |
| `[package.metadata.maintainers]`            | `maintainers.toml` (section 2.4)                                                                                        |
| The dependencies                            | The integrations crate's manifest, each once, apart from one tokio feature, placed as below                             |
| The rest of the manifest                    | The workspace's edition, license, version and `publish`, with `[lints]` and `doctest = false` as below                  |

All 32 manifests set `[lints] workspace = true`, which a crate has to opt into, and 31 set `doctest = false`, so the integrations crate sets both. The seam probe's manifest is the one without `doctest = false`, and its one doctest (`crates/testing/iabtl/src/lib.rs` lines 839 to 843) names its crate, `trusted_server_testing_iabtl`, so that doctest becomes a unit test in the probe's folder, where it neither fails for the missing crate nor stops running under `doctest = false`.

The 51Degrees loading cache's tests ask tokio for `rt-multi-thread` (`crates/loading-cache/51degrees/Cargo.toml` line 27). tokio 1.52.3 refuses that feature on every WebAssembly target unless `tokio_unstable` is set (its `src/lib.rs` lines 466 to 478), nothing in the repository sets it, and `test-fastly` and `clippy-fastly` build the integrations crate's tests for `wasm32-wasip1`. The feature therefore goes in a table that only a native build reads, and the tests that use threads run on the host only, behind `#[cfg(not(target_arch = "wasm32"))]`.

```toml
[dev-dependencies]
futures = { workspace = true }
tokio = { workspace = true, features = ["rt", "macros", "time"] }

[target.'cfg(not(target_arch = "wasm32"))'.dev-dependencies]
tokio = { workspace = true, features = ["rt-multi-thread"] }
```

The placeholder `crates/edgecookie/README.md`, a type folder with no crate in it yet, moves to `src/edgecookie/README.md`. Each type folder gains a `mod.rs` that declares its vendor folders, and the crate's `lib.rs` declares the type folders. Each file moves once, keeping its content apart from the changes this design lists, so Git follows the moves as renames and `git log --follow` and blame keep each file's history.

Paths inside the files follow. `include_str!` takes a path from the file that holds it, so a fixture that moves with its source needs no change. The 12 folders whose page change test names its recorded page by a path under `env!("CARGO_MANIFEST_DIR")` name `/src/<type>/<vendor>/fixtures/` where they named `/src/fixtures/`, because the manifest folder is now the integrations crate's. A `use` of another module crate, such as the client hints middleware's `trusted_server_shared_51degrees`, becomes a path in the crate, `crate::shared::fiftyone_degrees`. Some paths climb out of a crate's own `src/` or folder, and each of them changes with the move.

- The seam probe's `include_str!("../js/probe.js")` (`crates/testing/iabtl/src/lib.rs` line 97) becomes `include_str!("js/probe.js")`, because `mod.rs` sits beside `js/`.
- The APS module's tests read the renderer fixture of `trusted-server-js` with `include_str!("../../../trusted-server-js/lib/test/fixtures/aps-renderer-v1.json")` (`crates/auction/aps/src/lib.rs` lines 2641 and 4532), which takes one more `../`, because the file sits one folder deeper below `crates/`.
- InMobi's node tests read `../src/gpp_stub.js` and `../src/tcf_stub.js` (line 9 of each file under `crates/cmp/inmobi/tests/`), which become `../gpp_stub.js` and `../tcf_stub.js`.
- The Google ad tag's script tests find `gpt_bootstrap.js` at `../../../src/gpt_bootstrap.js` (`crates/ad-tag/google/js/test/fixtures/paths.ts` line 14), which becomes `../../../gpt_bootstrap.js`.
- Three `README.md` files link the guides by a relative path, `crates/middleware/51degrees/README.md` line 18, `crates/shared/51degrees/README.md` line 25 and `crates/analytics/tinybird/project/README.md` lines 27 and 28, and each link takes two more `../`.

Section 3 lists the files outside the crate that name a path in a module's folder.

### 2.2 Rust names for the folders

A folder name is what the settings use, and Rust cannot use every folder name as a module name. Each folder keeps its name on disk, and its Rust module takes a name under two rules.

1. A `-` becomes `_`, so `permission-signal` is `permission_signal` and `us-privacy` is `us_privacy`.
2. The one folder name that starts with a digit, `51degrees`, is `fiftyone_degrees`, the way 51Degrees' own published Rust packages write the number, `fiftyone-caching` among them.

A name that is a Rust keyword would take a raw name, such as `r#type`, and no folder today is one. The `#[path]` attribute ties each module to its folder.

```rust
// src/lib.rs
#[path = "permission-signal/mod.rs"]
pub mod permission_signal;

// src/permission-signal/mod.rs
#[path = "us-privacy/mod.rs"]
pub mod us_privacy;
```

| Folder                             | Rust path in the crate            |
| ---------------------------------- | --------------------------------- |
| `src/cmp/inmobi`                   | `cmp::inmobi`                     |
| `src/permission-signal/us-privacy` | `permission_signal::us_privacy`   |
| `src/tag/google-tag-manager`       | `tag::google_tag_manager`         |
| `src/auction/prebid-server`        | `auction::prebid_server`          |
| `src/loading-cache/51degrees`      | `loading_cache::fiftyone_degrees` |
| `src/asset-cache/51degrees`        | `asset_cache::fiftyone_degrees`   |
| `src/shared/51degrees`             | `shared::fiftyone_degrees`        |

Other crates write the Rust names in more places than `use` lines and builder calls. Each adapter's `shipped_signal_modules()` makes the permission signal modules by their types (Axum `app.rs` line 160, Cloudflare line 432, Fastly line 426 and Spin line 383), and the Fastly adapter uses the Fastly device and geo types, `FastlyHostSignals` and `FastlyDeviceModule` (its `lib.rs` line 42 and `app.rs` line 149) and `FastlyPlatformGeo` (its `platform.rs` line 668). The tool's `pbs inspect` command reads the Prebid Server module's `MODULE`, and its tests read Prebid's `PrebidIntegrationConfig`. The permissions inspector makes four permission signal modules, and the Axum adapter's tests name constants such as `MODULE` of the Osano module and `COOKIE_MTM_PREF` of the Model Terms signal. A deployment's code writes `trusted_server_integrations::cmp::inmobi::builder()` where it wrote `trusted_server_cmp_inmobi::builder()`.

### 2.3 Module names do not change

A module's name is still `<type>.<vendor>`, with an entry after it where a folder supplies several, such as `ad-tag.google.diagnostics`, so no settings document changes and every section, table and `[[fetch]]` or `[[serve]]` entry reads as before.

Today `trusted_server_core::module_name!()` derives a name from Cargo's `CARGO_MANIFEST_DIR`, which after the fold is the integrations crate's folder for every module, so the macro can no longer tell two folders apart. At row 55 it is called in 24 tests, the 20 named `module_constant_is_the_crate_folder`, `module_constant_is_the_crate_folder_with_the_entry` in the client hints middleware, `module_constant_names_the_crate_folder_and_the_module_within_it` in the Google ad tag's diagnostics, `the_loader_middleware_is_named_after_the_module` in the Google ad tag and `the_tag_middleware_is_named_after_the_module` in DataDome, and in 8 functions that work a name out when the module runs, being `name()` in each of the five permission signal crates, `vendor()` in the 51Degrees shared code and `module_name()` and `reader_middleware_name()` in the seam probe. These 24 tests sit in 21 of the 32 crates, and the other 11 have no test of their name.

In the integrations crate every name is a constant written in its folder's `mod.rs`, as most already are (`pub const MODULE: &str = "cmp.inmobi";`), and the 8 functions return constants. One test checks every constant against its folder (section 2.5) and refuses a call of `module_name!` anywhere in the crate. Core keeps the macro for a crate outside the repository, such as a deployment's own, whose name still comes from its folder. Deriving the names from `file!()` was set aside, because the path `file!()` gives depends on how Cargo and rustc were invoked, for example whether the crate is a member of the workspace being built and whether the build remaps source paths, so a name derived from it could differ from one build to the next.

The builders give the registry a source label for its messages, such as the one that names two builders registering one id. 24 places in 22 of the crates pass `env!("CARGO_PKG_NAME")`, which after the fold would give every builder `trusted-server-integrations`, and two builders write their package name as text, the Google tag's `SOURCE` (`crates/tag/google-tag/src/lib.rs` line 69) and the seam probe's `SEAM_PROBE_SOURCE` (`crates/testing/iabtl/src/lib.rs` line 83). Each folder writes its own label instead, `trusted-server-integrations/<type>/<vendor>`, so the messages still name the folder. Section 4 says what a reader sees change.

### 2.4 Maintainers

Each folder holds a `maintainers.toml` with the two values its manifest's table holds today, and the comment beside the table where it has one, with "vendor crate" and "crate" in the comment written "module", as InMobi's below shows (its manifest's comment, lines 27 to 29, says "this vendor crate" and "this crate").

```toml
# The visible owner of this module, the way Prebid.js requires a named
# maintainer of every adapter. InMobi has not yet adopted this module, so the
# Trusted Server maintainers own it until a vendor owner steps up.
owner = "Trusted Server maintainers"
status = "seeking vendor owner"
```

The counts stay as they are, 6 `project owned`, 22 `seeking vendor owner` and 4 `vendor owned`, with the asset cache module of row 34 a fifth `vendor owned`, by 51Degrees. A folder whose vendor takes ownership changes its own file, and a vendor that wants a crate of its own later takes its one folder, which holds everything the module has.

The test `every_crate_outside_core_declares_its_maintainers` in `crates/trusted-server-adapter-axum/tests/shipped_modules.rs` (line 548) reads the manifests under `crates/<type>/`. It gives way to `every_folder_declares_its_maintainers` in the integrations crate (section 2.5).

### 2.5 The folder tests

Two tests in the integrations crate read the crate's own `src` folder, and replace the 24 name tests and the maintainers test.

1. **`every_folder_declares_its_maintainers`** reads each `src/<type>/<vendor>/maintainers.toml` with the `toml` crate, a workspace dependency, and fails for a folder without the file, an empty `owner`, or a `status` that is not `vendor owned`, `seeking vendor owner` or `project owned`, naming the folder.
2. **`every_folder_names_its_modules_after_itself`** compares the folders on disk with a list in the test of each folder and the names it supplies, and fails for a folder the list leaves out or a listed folder that is gone. Each name must be the folder's `<type>.<vendor>`, that name with an entry after it, such as `ad-tag.google.diagnostics`, or the same with the type left off, as `51degrees.client_hints` is written under `[middleware]`. The one other name the test allows is `loading-cache.iabtl`, the seam probe's test cache, and it allows that name for the probe's folder alone. The test also fails if any file in the crate calls `module_name!`, and it checks each folder's source label.

Both are host tests, behind `#[cfg(not(target_arch = "wasm32"))]`, because they read the crate's folders, and the crate's tests also run as WebAssembly under Viceroy, Fastly's local runtime, where there is no file system. Row 55 met the same limit in its discovery tests and changed them to work on folders held in memory (`5be114773`, `2026-10-19-module-scripts-in-crates-design.md`), which these two tests cannot do, because the folders on disk are what they check. Before the row is committed, each test is run once with one folder's file or constant broken, to show that it fails and names the folder.

The list in the second test is the same kind of check as the catalog completeness test that #1194 describes for its flat folders, which fails when a folder is missing from the catalog or a catalog entry has no folder.

### 2.6 Browser scripts stay in each folder

A module's browser script stays where row 55 put it, in its folder's `js/src/<module>/`, with its tests in `js/test/<module>/`, now under `crates/trusted-server-integrations/src/<type>/<vendor>/`. `trusted-server-js` still builds every bundle with its one Node project in `crates/trusted-server-js/lib` and embeds them, so the bundles' names, the URLs they are served at and their hashes do not change, and each of the 14 bundles is the same, byte for byte, as on row 55.

What finds the scripts follows the move. Each place that walks `crates/<type>/<crate>/js/` walks `crates/trusted-server-integrations/src/<type>/<vendor>/js/` instead.

- `moduleSourceRoots` in `crates/trusted-server-js/lib/build-module-options.mjs`, `expected_module_ids` in `crates/trusted-server-js/build/bundle_set.rs`, and `crate_script_folders` in `crates/trusted-server-js/build.rs`, with the folders `build.rs` watches.
- The patterns `*/*/js/...` in `crates/eslint.config.js`, `crates/.prettierrc.json`, `crates/tsconfig.json`, `lib/vitest.config.ts` and the `lint:crates`, `lint:fix`, `format` and `format:write` scripts of `lib/package.json`, which become `trusted-server-integrations/src/*/*/js/...`. The ignore list in `crates/eslint.config.js` (line 8) lets each folder level back in with a negation of its own (`'!*/'`, `'!*/*/'`, `'!*/*/js/'`), so it takes one negation for each level of the new path, from `'!trusted-server-integrations/'` down to `'!trusted-server-integrations/src/*/*/js/'`, ahead of the two for `js/src/**` and `js/test/**`.
- `lib/tsconfig.json`, whose `include` names `../../*/*/js` and whose `@tsjs/module/*` list names each folder's `js/src`, which the test of that list checks against `moduleSourceRoots()`.

InMobi's `node --test` files move with their folder, and the step of `.github/workflows/test.yml` that runs `crates/*/*/tests/*.test.mjs` runs `crates/trusted-server-integrations/src/*/*/tests/*.test.mjs`.

**Why the scripts are not a crate of their own.** #1194 moves every integration's browser code into a second crate, `trusted-server-integrations-js`. This design keeps each script in its module's folder, for four reasons.

1. Whoever maintains a module works in one folder, which holds its Rust, its script and the script's tests, its fixtures and its `maintainers.toml`, so one file names the owner of all of them. A second crate would put the script under a folder whose owner is not the module's.
2. #1194's reason for a browser crate is to take vendor sources out of the neutral browser crate, because "leaving vendor sources in the neutral crate preserves the ownership inversion". Row 55 already moved the ten modules' scripts that could move out of `trusted-server-js` into the folders that supply them, which a second crate would also do. It left vendor code in `trusted-server-js` that #1194's browser crate would take, being the APS renderer, which the shared browser core imports (`lib/src/core/auction.ts` line 5 and `lib/src/core/request.ts` line 2), and Prebid's code, and the shared browser core still names vendors (`2026-10-19-module-scripts-in-crates-design.md`). This row leaves that code where it is, so for the APS renderer and Prebid the inversion #1194 describes remains, until the browser core stops importing the renderer, as #1194's rule 4 asks, and Prebid's code has a folder of its own.
3. #1194's requirement of one Node toolchain, one lockfile and one set of tool settings is met already. Row 55 builds every module's script with the one Node project in `crates/trusted-server-js/lib`, and finds the scripts in the module folders.
4. #1194 asks for each integration to have one Rust folder and, where it has browser code, one browser folder of the same name. Here the two are the same folder.

What row 55 left in `trusted-server-js` stays there, being the APS renderer, which the shared browser core imports, Prebid's shim, which imports the `prebid.js` package that a script outside `lib` cannot reach, and Prebid's external bundle build, which the `ts` tool runs from `lib`.

### 2.7 Licenses

Every file in the integrations crate takes the repository's license, Apache-2.0, the `license` of `[workspace.package]` and the `LICENSE` file at the root, as every crate under `crates/` does at row 55, where each of the 42 manifests takes `license = { workspace = true }`. Two files under `crates/` carry license text of their own, and this row touches neither. `crates/trusted-server-js/lib/src/core/styles/normalize.css` has its MIT header, as IAB Tech Lab's main has it, and `crates/trusted-server-openrtb/proto/openrtb.proto` gives IAB Tech Lab's copyright under the Apache License 2.0 at line 16. The search below, for the license of the 51Degrees package and for license markers, prints nothing on row 55 and must print nothing on row 56.

```text
git grep -n -i -E 'EUPL|European Union Public|SPDX-License-Identifier' -- crates
```

The package `fiftyone-caching` 4.5.8, which the 51Degrees loading cache folder uses, gives its license as `EUPL-1.2` in its manifest and on crates.io. It is a dependency, so none of its files is in the repository, and only that folder uses it, behind the feature of section 2.8.

### 2.8 Cargo features

Three folders use a dependency that not every build can or should carry, one holds a module that no stock build links, and one is used only by tests. A stock build is an adapter or the `ts` tool built from the repository as it ships, with the builders that its own list, its stock list, names. Each of the five folders is compiled behind a feature named after the folder, `<type>-<vendor>`, and the feature gates the code in the folder, never the folder's name constants or its source label, so the tests of section 2.5 check every folder whatever features are on.

| Feature                   | What it compiles                                                           | Who turns it on                                                                                                                                |
| ------------------------- | -------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------- |
| `device-fastly`           | The Fastly device module, with the Fastly SDK, which calls the Fastly host | The Fastly adapter                                                                                                                             |
| `geo-fastly`              | The Fastly geo module, with the Fastly SDK                                 | The Fastly adapter                                                                                                                             |
| `loading-cache-51degrees` | The 51Degrees loading cache, with `fiftyone-caching`                       | No stock adapter and not the `ts` tool. A deployment that links the module (`2026-10-12-loading-cache-design.md`)                              |
| `asset-cache-51degrees`   | The 51Degrees asset cache module                                           | No stock adapter and not the `ts` tool. A deployment that links the module, beside a loading cache module (`2026-10-15-asset-cache-design.md`) |
| `testing-iabtl`           | The seam probe                                                             | The Axum and Fastly adapters' dev-dependencies, the only links to the probe at row 55, so no adapter build compiles it                         |

The Fastly SDK's functions are imports that only the Fastly host and Viceroy supply. `fastly-sys` 0.12.1 declares them at its `src/lib.rs` line 298 and guards no other target, so the two Fastly features belong to the builds for `wasm32-wasip1` that the Fastly adapter and its aliases make.

```toml
[features]
default = []
loading-cache-51degrees = ["dep:fiftyone-caching"]
asset-cache-51degrees = []

[dependencies]
fiftyone-caching = { workspace = true, optional = true }
```

So `cargo tree -p <adapter> -e normal --target <target>` finds no `fiftyone-caching` for any of the four adapters, nor for the `ts` tool, and finds the Fastly SDK under the integrations crate only for the Fastly adapter. The aliases in `.cargo/config.toml` name `-p trusted-server-integrations` where they named each module package. `clippy-fastly`, which builds for `wasm32-wasip1`, lints with every feature, as its `--all-features` already gives. The Fastly adapter's dependencies turn on the two Fastly features in `build-fastly`, `check-fastly` and `test-fastly`, and its dev-dependencies turn on `testing-iabtl` in `test-fastly`. The three also name `asset-cache-51degrees`, so that the asset cache module still builds and checks for `wasm32-wasip1` and its tests still run under Viceroy, as `2026-10-15-asset-cache-design.md` has the four Fastly aliases list the module's crate on row 34. None of the three names `loading-cache-51degrees`, as no Fastly alias lists the 51Degrees loading cache at row 55. The native aliases, `test-axum` and `clippy-axum`, turn on `loading-cache-51degrees`, `asset-cache-51degrees` and `testing-iabtl` and leave the two Fastly features to the Fastly aliases, so `clippy-axum` names the features it wants where it passes `--all-features` today. With `testing-iabtl` on, `test-axum` runs the seam probe's tests, so the step of `.github/workflows/test.yml` that tests the probe's package on the host (lines 133 to 139) goes.

The two features the module crates have today go. `test-utils` of the OpenRTB auction protocol served the APS and Prebid Server crates' tests, and `test-utils` of the 51Degrees shared code served the client hints middleware's tests, and those tests are now in the same crate, so they use the helpers under `#[cfg(test)]`. A test that holds an answer between two calls, as tests of the 51Degrees shared code and the client hints middleware do with the 51Degrees loading cache today, either uses core's `MemoryLoadingCache` or runs only with `loading-cache-51degrees` on, which the native test alias turns on.

The asset cache module needs no dependency the other folders lack. It sits behind `asset-cache-51degrees` because no stock build links it, as the 51Degrees loading cache sits behind its own feature.

### 2.9 What a deployment outside the repository changes

A deployment that builds on the repository through a path to a checkout of it replaces each module package it names with one dependency on the integrations crate, and turns on the features of the folders it links that have one.

```toml
# Before
[dependencies]
trusted-server-adapter-fastly = { path = "trusted-server/crates/trusted-server-adapter-fastly" }
trusted-server-cmp-inmobi = { path = "trusted-server/crates/cmp/inmobi" }
trusted-server-loading-cache-51degrees = { path = "trusted-server/crates/loading-cache/51degrees" }

# After
[dependencies]
trusted-server-adapter-fastly = { path = "trusted-server/crates/trusted-server-adapter-fastly" }
trusted-server-integrations = { path = "trusted-server/crates/trusted-server-integrations", features = ["loading-cache-51degrees"] }
```

Its code changes each `use` and builder call from the module's package to its path in the crate, by the names of section 2.2, so `trusted_server_loading_cache_51degrees::builder()` is `trusted_server_integrations::loading_cache::fiftyone_degrees::builder()`. Its settings documents do not change, because no module name does. A crate of its own that supplies modules keeps its package and its name, because `module_name!()` still names such a crate by its folder, and it hands its builders to the adapter as before. A git dependency changes in the same way, with `git` and `rev` where the example writes `path`.

### 2.10 Why one new row at the top

The crates were made in eleven rows and are folded in one, row 56, above row 55.

| Row | Crates it made                                                                                               |
| --- | ------------------------------------------------------------------------------------------------------------ |
| 2   | `device/fastly`, `geo/fastly`                                                                                |
| 3   | The five under `permission-signal/`                                                                          |
| 7   | Sixteen, from `ad-server/mock` to `testing/testlight`, as the integration seam moved each vendor out of core |
| 18  | `cmp/inmobi`                                                                                                 |
| 20  | `ads-txt/sellers`                                                                                            |
| 24  | `loading-cache/51degrees`                                                                                    |
| 30  | `tag/google-tag`                                                                                             |
| 33  | `headers/geo`                                                                                                |
| 44  | `middleware/51degrees`, `shared/51degrees`                                                                   |
| 46  | `analytics/tinybird`                                                                                         |
| 48  | `shared/aws`                                                                                                 |

Row 43 renamed three of them, `ad-server/mock`, `ads-txt/sellers` and `testing/seam-probe`, to `iabtl`, and row 34 makes the asset cache module. Folding in each of those rows was set aside for four reasons.

1. **Each fold would travel through the rows above it.** A fold in row 2 changes every row above it that touches a moved file, and 31 of the 48 rows above row 7 change a file in a module crate's folder, so the moves would be merged up through as many as 52 rows, each merge meeting the next row's own changes to the moved files. One fold at the top moves each file once, from the place the rows below leave it.
2. **The rows below stay as reviewed.** 41 of the other design documents in IABTechLab/trusted-server#1342 name a path under `crates/<type>/<vendor>/`, as do each row's description and code. A fold in each row would change all of them in rows already reviewed. With one fold at the top, each row's design describes the tree that row has, and this design gives the map from the old paths to the new.
3. **The fold changes how modules are built and not what they do, and one row shows it.** Its review asks one question, whether anything but the paths and the labels of section 4 changed, which this row answers by the same tests passing, the 14 bundles keeping their hashes and every module keeping its name. Row 56 is measured beside row 55, with the same tests, the hashes of the 14 bundles, every module name and the time per request, and the results go in the description of row 56's pull request.
4. **Tech Lab's own move fits in its place.** #1194 also moves the crate boundary in a milestone of its own that preserves behavior, ahead of any change to the configuration. A row at the top can be replaced by Tech Lab's move, or by a later form of this one, without a change to the rows below it.

### 2.11 Beside IAB Tech Lab's split design

| What                        | #1194                                                                                                           | This design                                                                                                      |
| --------------------------- | --------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------- |
| The crate                   | `trusted-server-integrations`                                                                                   | The same                                                                                                         |
| The folders                 | `src/integrations/<id>/`, one level                                                                             | `src/<type>/<vendor>/`, as Tech Lab asked on 2026-10-23                                                          |
| Browser code                | A second crate, `trusted-server-integrations-js`                                                                | In each module's folder, built by `trusted-server-js` (section 2.6)                                              |
| Module names and settings   | One ordered `[integrations]` inventory, in a milestone after the crate move                                     | Unchanged `<type>.<vendor>` names and the module sections the stack has                                          |
| Owners                      | No maintainer record                                                                                            | `maintainers.toml` in each folder                                                                                |
| A crate outside the project | "External vendor crate injection or adapter-supplied registration" is a non-goal                                | A deployment's own crates still hand builders to the adapters, through `run_with` or `routes_with_registrations` |
| Finding every folder        | A static catalog in `lib.rs` and a test that the catalog and the folders agree                                  | Each adapter's own list (row 43) and the folder test of section 2.5                                              |
| Where vendor names live     | Only the integrations crate, which puts the catalog together, as no adapter imports an integration (its rule 6) | Also five lists written by hand, each adapter's `shipped_module_builders()` and the tool's `linked_builders()`   |

Rule 6 of #1194 reads "No adapter reconstructs a concrete catalog or imports `aps`, `prebid`, or another integration module directly." Here each adapter's `shipped_signal_modules()` and the permissions inspector also name the permission signal modules, and section 2.2 lists the other places outside the crate that name a module.

## 3. What else changes in the repository

1. The workspace `Cargo.toml` lists one member and one dependency where it listed each module crate, and `Cargo.lock` follows.
2. Each adapter's `Cargo.toml` and the `ts` tool's names `trusted-server-integrations`, with the features of section 2.8, and each `shipped_module_builders()`, `shipped_signal_modules()` and `linked_builders()` names each builder or module by its path in the crate, in the same order, as do the other places section 2.2 lists.
3. The permissions inspector, `tools/permissions-inspector/wasm`, names the integrations crate where it named four permission signal crates. Its build for `wasm32-unknown-unknown` then compiles every folder that needs no feature. The Cloudflare adapter's build for that target compiles each of those folders' crates at row 55, the 51Degrees shared code through the client hints middleware and the others because it names them.
4. In `.cargo/config.toml` the aliases change as section 2.8 says. In `.github/workflows/test.yml` the step that runs InMobi's node tests changes as section 2.6 says, and the step that tests the seam probe's package on the host goes, as section 2.8 says.
5. `crates/fastly.toml` goes. It is the Viceroy manifest for crates three folders deep, because the runner in `.cargo/config.toml` starts Viceroy with `-C ../../fastly.toml` from the crate's folder. From `crates/trusted-server-integrations`, two folders deep, that path reaches the root `fastly.toml`, and nothing else uses `crates/fastly.toml`.
6. `crates/trusted-server-integration-tests/browser/initial-render/run.cjs` reads the Google ad tag's `gpt_bootstrap.js` by its path (lines 70, 166 and 477), which becomes `crates/trusted-server-integrations/src/ad-tag/google/gpt_bootstrap.js`. `.github/workflows/integration-tests.yml` runs that script (line 211) and needs no change.
7. The doc comments in core that call a module's name a crate's path under `crates`, on `IntegrationBuilder::with_module_name` and `with_cache_features`, on `IntegrationRegistrationBuilder::with_geo_module` and at the head of `module_name.rs`, say a folder's path under the integrations crate's `src` as well. The same goes for the documentation of a permission signal's name in `permission_signal/mod.rs` (line 195) and of a middleware's name in `middleware.rs` (line 521), and for `permission_signal/README.md`, whose line 232 names `module_name!()` and whose line 256 says that five crates ship under `crates/permission-signal/`. A test comment in `publisher.rs` (line 7143) names the Google ad tag's `crates/ad-tag/google/js/src/gpt/index.ts`. No code in core changes.
8. The 20 guides under `docs/guide` that name a crate path or package, among them the integration guide, the onboarding guide and `docs/guide/integrations/inmobi.md`, the contributor documents that name one, the sample permission policy `config/permissions/sample.yaml`, whose comment at line 514 names `crates/permission-signal/tcf`, and `CHANGELOG.md`, which says what a deployment changes (section 2.9).

## 4. What is not in this change

- **A change of behavior.** No module, middleware, route or setting behaves differently, so there is no consent or privacy effect, and the stock lists hold the same builders in the same order. Two things a reader sees do change with the paths. The source label each builder gives the registry becomes `trusted-server-integrations/<type>/<vendor>` (section 2.3), so the message for two builders registering one id names that label, and the Axum test that expects `trusted-server-identity-lockr` in it (`crates/trusted-server-adapter-axum/tests/routes.rs` line 781) changes with it. Every module's log target becomes its path in the crate, such as `trusted_server_integrations::cmp::inmobi`, and the Fastly adapter's logger prints only the last part of a target (`target_label` in `crates/trusted-server-adapter-fastly/src/logging.rs` lines 11 to 17). On Fastly a line logged in a folder's `mod.rs` therefore carries only the last part of the folder's Rust path, so the three `iabtl` folders log as `iabtl`, the 51Degrees folders as `fiftyone_degrees` and the two Fastly folders as `fastly`, where each crate logged under its own name.
- **A change to module names or settings.** No section, table or entry changes.
- **IAB Tech Lab's `[integrations]` inventory.** #1194 changes the configuration in a milestone of its own, and this row does not.
- **A crate for browser code.** Section 2.6 says why.
- **A crate for each vendor.** A vendor that takes ownership of its module may later ask for one, and its folder is what such a crate would hold.

## 5. Acceptance

1. Every test alias passes on row 56. The native aliases run the tests they ran on row 55, apart from the 24 name tests and the maintainers test that section 2.5 replaces, and they also run the seam probe's tests, which the workflow step this row removes ran on row 55. `test-fastly` also runs, under Viceroy, the tests of eight folders it did not list on row 55, being the five permission signals, the 51Degrees shared code, the client hints middleware and the seam probe, because those folders are now in a crate it tests. The ninth it did not list, the 51Degrees loading cache, stays out, because its feature is off there. A test among them that cannot run under Viceroy is marked for the host, such as the 51Degrees shared code's test that asks from two threads at once (`crates/shared/51degrees/src/client.rs` line 706).
2. Each of the 14 bundles `build-all.mjs` writes has the same SHA-256 as on row 55.
3. `every_folder_declares_its_maintainers` and `every_folder_names_its_modules_after_itself` pass, and each fails, naming the folder, when one folder's `maintainers.toml` is removed or one name constant is changed.
4. `cargo tree -e normal` finds no `fiftyone-caching` for any adapter, on its own target, or for the `ts` tool, and finds the Fastly SDK under the integrations crate only for the Fastly adapter.
5. The license search of section 2.7 prints nothing.
6. `git log --follow` on a moved file, such as `crates/trusted-server-integrations/src/cmp/inmobi/mod.rs`, shows the file's history from before the move.
7. No settings document, fixture or example in the repository changes, apart from the paths that sections 2.1 and 3 list and paths in comments.

## 6. Open questions

These are for the reviewer, and this design decides none of them.

1. **The Rust name of `51degrees`.** `fiftyone_degrees` writes the number the way 51Degrees' own packages do. Another rule, such as a leading `_` (`_51degrees`) or the vendor's own short form (`fiftyone`), would serve, and the choice reaches every deployment's `use` lines.
2. **The seam probe.** It folds into `src/testing/iabtl/` behind `testing-iabtl`. Kept as a crate of its own instead, it would go on proving that a crate beside the integrations crate, as a deployment's own is, plugs into the adapters. Which do reviewers prefer?
3. **Where the folder tests live.** They sit in the integrations crate, as host tests. The maintainers test sits today in the Axum adapter's tests, which run only on the host. Should they stay together there?
4. **Which crate embeds the bundles.** `trusted-server-js` still embeds every bundle, the vendor ones included, as on row 55, and core depends on `trusted-server-js`, so core's build carries every vendor's browser script. Should the integrations crate embed its folders' bundles itself, as #1194 has its browser crate do?
5. **A rebuild after one vendor file changes.** A change to one folder's file rebuilds the whole integrations crate, where today it rebuilds that module's crate and what depends on it. Is that an acceptable cost for module authors, or should a folder whose vendor works on it often be extracted into a crate of its own?
6. **Features for every folder.** Only the five folders of section 2.8 have a feature. Should every folder have one, so that a deployment compiles only the modules it links?
7. **One stock list.** The four adapters and the tool each name every builder in a list written by hand, and `2026-10-14-no-modules-crate-design.md` says that no test checks that the five lists agree. In #1194 the integrations crate composes the catalog and no adapter imports an integration directly. Should the integrations crate supply one stock list that the adapters and the tool take, with a deployment that links a different set still writing its own?
8. **Log labels on Fastly.** After the fold, folders that share a vendor share a log label on Fastly (section 4). Should each folder give its log lines a target of its own, or should the Fastly logger print more of the target?

## Revision record

| Date       | Change                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                            |
| ---------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 2026-10-26 | First draft.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                      |
| 2026-10-26 | Fixes from review round 1. tokio's `rt-multi-thread`, which the loading cache's tests ask for, goes in a table that only native builds read, with the threaded tests on the host. The paths that climb out of a crate's folder are listed with their fixes, and `run.cjs`, `crates/fastly.toml`, the seam probe's workflow step and the core documents that name a crate's path join section 3. The browser scripts' second reason says that the APS renderer and Prebid's code stay in `trusted-server-js`, where #1194 would move them. Row 56's own measurement goes in its pull request. The manifest's lint and doctest settings, the source labels written as text, the log labels on Fastly, the places the Rust names are used, the rule for `51degrees`, the probe's one name of another type, the Fastly SDK's host imports, the files with license text of their own, the count of rows a fold would travel and where vendor names live beside #1194 are stated. Open questions 4 and 5 are reworded and 7 and 8 added. The Touchbase of 2026-10-23 says what Tech Lab asked, and that 51Degrees decided, and the seam probe and Viceroy are defined where first used. |
| 2026-10-26 | Fixes from review round 2. The split design's pull request names its author, Aram Grigoryan. The account of the Touchbase gives Tech Lab's request as the record has it, and places the decisions of 51Degrees after the Touchbase. "Stock build" is defined where first used, the split design's integrations crate is the one that puts the catalog together, and a feature gates neither a folder's name constants nor its source label. `build-fastly`, `check-fastly` and `test-fastly` name `asset-cache-51degrees` and none of them names `loading-cache-51degrees`, each `maintainers.toml` comment says "module" where the manifest says "crate", and the sample permission policy joins section 3.                                                                                                                                                                                                                                                                                                                                                                                                                                                                      |

## Review log

1. **Round 1.** A review in a fresh session, against `split/55-scripts-in-crates`. Issues found, three of them blocking. The three were that one manifest would carry the loading cache's test dependency on tokio's `rt-multi-thread`, which tokio refuses to build for WebAssembly, so `test-fastly` and `clippy-fastly` would stop, that three paths climb out of a crate's folder and break after the move, and that the design said a separate browser crate would move no more vendor code out, when the APS renderer and Prebid's code stay in `trusted-server-js`, where IABTechLab/trusted-server#1194 would move them. This revision puts that dependency in a table that only native builds read, with the threaded tests on the host, lists every path that climbs out of a folder with its fix, and says where the APS renderer and Prebid's code stay. It also states the manifest's lint and doctest settings, the source labels written as text, the log labels on Fastly and where vendor names live beside IABTechLab/trusted-server#1194, and leaves row 56's own measurement to that row's pull request.
2. **Round 2.** A review in a fresh session, checking the round 1 fixes against `split/55-scripts-in-crates`. Issues found, none blocking. This revision names the author of Tech Lab's split design, Aram Grigoryan, gives Tech Lab's request at the Touchbase as the record has it and places the decisions of 51Degrees after the Touchbase, defines stock build where first used, and calls the split design's integrations crate the crate that puts the catalog together. It also says that a feature gates neither a folder's name constants nor its source label, that `build-fastly`, `check-fastly` and `test-fastly` name `asset-cache-51degrees` and none of them names `loading-cache-51degrees`, and adds the sample permission policy to section 3.
