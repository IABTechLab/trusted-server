# Split Integrations into Dedicated Crates Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Move all concrete Rust and browser integrations into two statically linked crates, then make one ordered `[integrations]` configuration the operator inventory.

**Architecture:** Milestone 1 introduces neutral core contracts, one Rust composition root, and separately owned browser artifacts while retaining the current operator/storage schema and behavior through one temporary normalizer. Milestone 2 replaces only the source schema and ordering contract, adds stored schema 2 with explicit order sidecars and a read-only schema-1 decoder, and cuts over each adapter only after its rollback gate passes.

**Tech Stack:** Rust 2024, Cargo workspace, EdgeZero, Fastly/Axum/Cloudflare/Spin adapters, TOML 1.1, serde, Node 24, TypeScript, Vite, Vitest, Playwright.

**Spec:** `docs/superpowers/specs/2026-09-17-split-integrations-crates-design.md`

**Tracking:** [Issue #1237](https://github.com/IABTechLab/trusted-server/issues/1237), [draft design PR #1194](https://github.com/IABTechLab/trusted-server/pull/1194).

---

## Execution rules and entry gate

This is an implementation plan, not permission to start extraction from the spec worktree. As checked on 2026-10-06, draft PR #1194 is at `c6c34cdcd`, remote `main` is `7f610c0fc`, and the spec checkout still pins EdgeZero v0.0.8. The final implementation baseline is **not** either of those commits. Start a fresh implementation worktree from the post-prerequisite `origin/main`; do not merge the spec branch's old source tree into it. Re-read `AGENTS.md` there.

Before Task 1, prove that independent fixes for #1098, #1196, #1198, #1199, #1200, #1202, #1203, #1204, and #1208 have landed with their focused tests. #1197 is closed, but the broader set-valued serialization audit remains an evidence gate. Confirm #1206/#1207 are resolved by #1208; #1201 belongs to milestone 2; #1205's hidden-first-phase remedy is rejected. Confirm the reviewed EdgeZero typed-config hook and environment-selector revision is available before Task 8. If any requirement is absent, stop that dependent task and land the prerequisite separately. Do not absorb a baseline bug fix into a rename commit. Record the exact `origin/main`, EdgeZero revision, test results, and output-golden hashes in the first implementation PR.

Use the same public API manifest for every extraction commit. Every new/widened/removed symbol must have an owner, direct consumer, stability class, and (if transitional) removal step; an unlisted symbol needs a spec amendment. One active Rust catalog and one browser manifest exist at every revision; a transitional delegation replaces, rather than duplicates, an owner. No per-vendor crate, plugin ABI, runtime loading, second operator inventory, or unrelated audit-code reorganization.

For each task below: write the named failing test or guard first; run it and record the expected failure; make the smallest change; run the focused target-matched command and the neighboring regression set; inspect `git diff --check`; commit one independently reviewable change using the repository's sentence-case imperative convention. The exact path list is based on the reviewed checkout and must be rechecked against the recorded post-prerequisite baseline before editing.

## File and ownership map

| Owner                | Create or change                                                                                                                                                                                                                            | Responsibility                                                                                                                                                                                                    |
| -------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Neutral Rust         | `crates/trusted-server-core/src/integration/{mod,registry,claims,browser_assets,processing}.rs`; `auction/{plan,profile,provider,orchestrator}.rs`; `config_payload.rs`, `settings_data.rs`, `publisher.rs`, `html_processor.rs`, `tsjs.rs` | Typed contracts, plan compilation/execution, verified envelopes, neutral processing and browser-asset consumption; no concrete imports. Keep files split by responsibility rather than growing one registry file. |
| Rust application     | `crates/trusted-server-integrations/{Cargo.toml,src/lib.rs}`, `src/{catalog,composition,config,legacy_config,source_config,validation,secret_metadata}.rs`, `src/integrations/<id>/mod.rs`                                                  | Static definitions, concrete implementations, source views, schema conversion, validation, and final composition. `source_config` is host-feature-gated.                                                          |
| Neutral browser      | `crates/trusted-server-js/{build.rs,src/bundle.rs}`, `lib/src/core/`, `lib/src/shared/`, `lib/package.json`, `lib/{build-all.mjs,tsconfig.json,vitest.config.ts,eslint.config.js}`                                                          | One versioned runtime facade, core IIFE/finalizer, canonical Node project, neutral owner-private artifacts.                                                                                                       |
| Integration browser  | `crates/trusted-server-integrations-js/{Cargo.toml,build.rs,src/lib.rs}`, `lib/src/integrations/`, `lib/test/integrations/`                                                                                                                 | Integration IIFEs, inline programs, owned fixtures/tests, typed asset catalog and private artifacts.                                                                                                              |
| App edges            | `crates/trusted-server-adapter-{fastly,axum,cloudflare,spin}/src/app.rs`, Fastly `src/main.rs`, `crates/trusted-server-cli/src/{app_config,run,prebid_bundle}.rs`                                                                           | Exactly one composition call per loaded config; source/deploy command views and target gates.                                                                                                                     |
| Cross-cutting checks | `Cargo.toml`, `.cargo/config.toml`, `.github/workflows/{format,test,integration-tests}.yml`, `crates/trusted-server-integration-tests/`, `scripts/`, `docs/guide/`, `trusted-server.example.toml`                                           | Target-matched gates, real embedded-artifact tests, parity, rollout evidence, docs and stale-path guard.                                                                                                          |

The names in the new crates are responsibility boundaries, not a requirement to create empty placeholder modules. Create each file when its first consumer and test arrive. Keep the existing Next.js nested implementation and fixtures together; leave neutral `ec/prebid_eids.rs` in core.

## Milestone 1 — crate boundary with schema-one behavior

### Task 1: Freeze the corrected baseline and public surface

**Files:** Create `crates/trusted-server-integration-tests/tests/integration_split_baseline.rs` and a reviewed output-golden directory under `crates/trusted-server-integration-tests/fixtures/integration_split_baseline/`; create `scripts/check-integration-api-delta.sh` and its checked-in manifest; modify only baseline-test selectors in `.github/workflows/integration-tests.yml`.

- [ ] Add differential cases for every current Rust definition and browser module, globally interleaved APS/Prebid/standard provider IDs, deferred Prebid, GPT diagnostics, Next.js/GTM/RSC, #1135, routes, trusted script attributes, CLI views, and relevant failure classifications. Store actual corrected-baseline output bytes and separate artifact hashes; do not hand-author expected runtime output.
- [ ] Run the harness against recorded post-prerequisite `main` twice. Expected: byte-identical behavior goldens; artifact hashes may be recorded separately. Prove each golden fails after a deliberate test-only mutation to its observed output, then restore the mutation.
- [ ] Capture the current public core symbol snapshot and write an explicit delta manifest for the spec's three classes. Test that an undeclared added/removed public item fails the guard, then restore the canary.
- [ ] Verify deterministic serialization by repeatedly parsing a fixture with at least two values in every set-valued field and comparing envelope SHA and template identity. If it fails, stop for the standalone prerequisite fix.
- [ ] Commit the baseline evidence and its exact commit/toolchain/EdgeZero pins. Do not proceed if a prerequisite output defect is still present.

### Task 2: Introduce neutral registration, claims, and plan inputs

**Files:** Create `crates/trusted-server-core/src/integration/{mod,registry,claims}.rs`; modify `crates/trusted-server-core/src/{lib.rs,integrations/registry.rs,integrations/mod.rs,auction/plan.rs,auction/profile.rs,auction/provider.rs,auction/mod.rs}`; test in those modules.

- [ ] Write compile/runtime tests for `IntegrationRegistry::from_registrations`, capability-local order, duplicate/reserved route and script claims, and a flat ordinal-bearing provider sequence independent of lookup-map order. Expected initially: missing contracts or wrong order.
- [ ] Add `IntegrationDeclaration` and only the typed capabilities consumed by current implementations; make the registry accept registrations rather than constructing APS/Prebid or calling `builders()`. Keep the old fixed builder table as the sole schema-one source until the new composition root owns it.
- [ ] Introduce neutral profile and prepared exchange-or-skip contracts, a consuming bound response parser, renderer descriptors, transport-header view, and mediator registration. Remove vendor enum/downcast/constructor knowledge from core without changing OpenRTB wire bytes or the #1159 skip/admission matrix.
- [ ] Run `cargo test-fastly`, `cargo test-axum`, and the baseline differential harness. Expected: schema-one provider order and externally visible local labels match the golden. Run the API-delta guard and commit the neutral seam separately from any source moves.

### Task 3: Move lifecycle decisions behind neutral processing requirements

**Files:** Create `crates/trusted-server-core/src/integration/processing.rs`; modify `crates/trusted-server-core/src/{html_processor,publisher,cookies,cache_policy,response_privacy}.rs` and `crates/trusted-server-adapter-{fastly,axum,cloudflare,spin}/src/app.rs` plus Fastly `src/main.rs`.

- [ ] Write failing matrices for DataDome origin/body/privacy requirements, GPT diagnostics preparation/finalization and cache vetoes, reserved Cookie/query normalization even when diagnostics is absent, and request-local mutable document state. Include every adapter's existing fallback and health behavior.
- [ ] Add narrow neutral per-request requirements and document-state factories. Make core apply requirements and fixed parser phases without importing `datadome` or `gpt_diagnostics`; keep current concrete implementations in their original owner until their individual move.
- [ ] Move fixed GPT-diagnostics input cleanup to the neutral ingress boundary. Preserve lenient Cookie handling and all-response privacy before cache/origin selection; integration requirements may only restrict baseline cache decisions.
- [ ] Run focused core module tests, `cargo test-fastly`, `cargo test-axum`, `cargo test-cloudflare`, `cargo test-spin`, and API/differential guards. Commit the lifecycle seam before moving DataDome or GPT diagnostics.

### Task 4: Establish the one browser build lease and two-root toolchain

**Files:** Create `scripts/with-browser-lease.mjs` and its tests; modify `crates/trusted-server-js/lib/{package.json,tsconfig.json,vitest.config.ts,eslint.config.js,build-all.mjs}`, `.github/workflows/{format,test,integration-tests}.yml`, `Cargo.toml`, `.cargo/config.toml`.

- [ ] Add failing lease tests for outer and authenticated nested invocations, absent/wrong nonce, holder PID reuse, a live orphan child, stale recovery, and direct tool invocation. A nested `Cargo build → runner → npm → runner → Node` path must complete without deadlock.
- [ ] Implement one atomic-directory lease rooted at the canonical Node project. Keep the lock while the complete child process scope lives. Fail closed when ownership cannot be proven dead; no per-crate locks. Put `npm ci`, Vite, Vitest, TypeScript, ESLint, Prettier, Cargo browser builds, and Prebid bundle behind this runner.
- [ ] Configure the canonical package at `crates/trusted-server-js/lib` to resolve both source roots and `prebid.js` exports. Add production two-root `typecheck`, isolated failing canary, explicit test/lint/format globs, and exact GPT-bootstrap legacy formatter/linter exceptions. Keep one lockfile.
- [ ] Run runner unit tests and wrapped `npm run typecheck`, `npm run test`, `npm run lint`, `npm run format`. Expected: both roots are selected and the canary fails only in its isolated negative test. Commit tooling before moving browser files.

### Task 5: Define the versioned browser facade and neutral APS/queue seams

**Files:** Create focused facade/IDL/ABI snapshot and tests under `crates/trusted-server-js/lib/src/core/` and `lib/test/core/`; modify `lib/src/core/{index,auction,request,queue,registry}.ts`, `lib/src/shared/dom_insertion_dispatcher.ts`, `lib/src/integrations/{aps,gpt,prebid,permutive,testlight}/` only where required by the seam.

- [ ] Write production-artifact tests proving one `window.tsjs`/first-impression service, Permutive context and log state across IIFEs, APS renderer registration without a core APS import, GPT pre-core adoption, Prebid deferred version mismatch rejection, and idempotent listener/queue behavior. Pin exact ABI IDL, generated record bytes, immutable published snapshot, and mismatch-before-initializer guards.
- [ ] Move shared state and stateful APIs behind a versioned facade; keep type-only cross-root imports and the one declared neutral bootstrap-safe GPT input. Do not duplicate core state inside IIFEs or change the #1191 first-impression algorithm.
- [ ] Replace numeric/lexical DOM-handler priority with synchronous registration order. Derive immediate-only DOM need from the dedicated accessor import; reject deferred use. Install the CSP-compatible neutral external queue finalizer after fixed synchronous post-unified assets, preserving success/failure fallback paths.
- [ ] Run wrapped browser Vitest, production typecheck, facade ABI/artifact tests, and the corrected #1196/#1199/initial-render golden cases. Commit the facade and dispatcher before relocating any browser owner.

### Task 6: Split browser artifact ownership

**Files:** Create `crates/trusted-server-integrations-js/{Cargo.toml,build.rs,src/lib.rs}` and the empty Rust crate shell `crates/trusted-server-integrations/{Cargo.toml,src/lib.rs}`; create owner-specific build modules under `crates/trusted-server-js/lib/`; modify `crates/trusted-server-js/{build.rs,src/bundle.rs}`, `Cargo.toml`, `.cargo/config.toml`.

- [ ] Write failing clean/incremental/concurrent build tests: changing one integration source changes only its owner manifest/hash; changing the neutral first-impression input changes GPT's bootstrap input; stale or partial output and missing npm fail. Discovery includes every `index.ts` regardless of immediate, deferred, or standalone load mode.
- [ ] Build neutral core/finalizer and integration IIFEs/assets into separate private Cargo `$OUT_DIR` targets. Generate typed module IDs and exact-byte/hash catalogs; validate complete artifact-affecting input manifests, `TSJS_SKIP_BUILD` freshness, and Cargo rerun environment inputs. No shared `dist` scan or copy.
- [ ] Keep one explicit temporary browser manifest pointing to unmoved source paths, then replace each mapping in Task 11; do not copy an unused duplicate source tree. Create both new Cargo members and update target-matched aliases in this change.
- [ ] Run wrapped build/test/typecheck and owner-manifest tests, then `cargo check-fastly`, `cargo check-axum`, `cargo check-cloudflare`, `cargo check-spin`. Commit the build split; Task 9 exports exact composed bytes for browser harnesses after the composition root exists.

### Task 7: Make neutral core consume composed browser assets

**Files:** Create `crates/trusted-server-core/src/integration/browser_assets.rs`; modify `crates/trusted-server-core/src/{publisher,tsjs,html_processor}.rs` and `crates/trusted-server-js/src/bundle.rs`; test in core, integration tests, and browser harnesses.

- [ ] Write failing tests for core/creative/immediate/fixed synchronous/finalizer/deferred/standalone order, exact static bytes and hashes, stale `?v=` response behavior, trusted attributes, APS immediacy, and cache fingerprint invalidation when an inline or external asset changes.
- [ ] Introduce `CompiledBrowserAsset` and `BrowserDocumentAssets` as neutral byte/hash/order inputs. Add and test injection-capable core serving, script-tag, and HTML APIs while the existing production caller remains the sole active asset source until Task 9's atomic composition switch. Core must never link `trusted-server-integrations-js` or look up a concrete module by unchecked string.
- [ ] Implement the spec's unambiguous document fingerprint and pure composition-digest function with an injected full artifact build ID. Pin raw-byte, length-framing, attribute, asset-order, and secret-reference vectors. Keep the corrected #1198 cache key active until Task 9 assembles the full adapter ID; do not replace it with a core-only digest.
- [ ] Run focused core tests, browser artifact vectors, template-cache smoke, and the baseline differential harness. Commit this neutral consumption boundary before adapter composition changes.

### Task 8: Add the Rust composition root while retaining schema one

**Files:** Fill `crates/trusted-server-integrations/src/lib.rs`; create `src/{catalog,composition,config,legacy_config,source_config,validation,secret_metadata}.rs`; modify `crates/trusted-server-core/src/{config,config_payload,settings_data,integrations/registry}.rs`, `Cargo.toml`, `.cargo/config.toml`, `Cargo.lock`.

- [ ] Before edits, verify the reviewed EdgeZero `run_*_typed_with_hooks` revision is available and repin all `edgezero-*` dependencies together. Add tests proving one exact config read/value across pre-pass, overlay, validation, serialization, diff, and push, including source replacement and `--no-env`. If the extension is absent, stop; do not substitute filesystem snapshots.
- [ ] Write failing catalog completeness and schema-one differential tests. The catalog initially has one entry per current definition, each delegating to the old owner through an explicit transitional export. One legacy converter preserves omitted-default behavior, fixed builder sequence, globally lexical flat provider order, implicit APS, current local external labels, and baseline unknown-ID acceptance.
- [ ] Move `TrustedServerAppConfig`, catalog-owned secret metadata/validation, inactive integration-secret preprocessing, and source/partial/validated views outward. Keep only neutral envelope/chunk verification, global settings, global inactive-secret preprocessing, secret primitives, and plan compilation in core. Add `CompositionAttempt` with the exact settings-capture stage specified in the design.
- [ ] Reuse the prerequisite canonical generator from core build support to emit labeled raw component digest records for core, OpenRTB, both browser embedding crates, and the integrations crate. Each crate records its own authoritative source/build inputs and forwards direct runtime workspace dependency records; conflicting duplicate labels fail. Do not read another crate's `$OUT_DIR`.
- [ ] Make the new root produce one settings/plan/orchestrator/registry/assets/target/digest composition in focused tests, without activating a second production catalog. Leave deletion of core's `builders()`/concrete `with_plan` path to Task 9's atomic consumer switch. Add neutral core test stubs behind `test-utils`; do not create a dev-dependency cycle.
- [ ] Run `cargo test -p trusted-server-integrations`, `cargo test-fastly`, `cargo clippy-fastly`, API/differential guards, and `cargo tree -p trusted-server-core` to prove no reverse dependency. Commit the inert composition root separately from owner moves.

### Task 9: Rewire every adapter, loader, and CLI command to that root

**Files:** Modify four adapter `src/app.rs` files, Fastly `src/main.rs`, `crates/trusted-server-cli/src/{app_config,run,prebid_bundle}.rs`, `src/commands/{config/ad_templates,audit/ad_templates,audit/generate/validate}.rs`, `crates/trusted-server-integration-tests/src/bin/generate-viceroy-config.rs`, `tests/common/config.rs`, and their tests; create `crates/trusted-server-integration-tests/src/bin/export-browser-artifacts.rs`; modify `browser/tests/shared/aps-renderer.spec.ts`, `browser/initial-render/run.cjs`, `scripts/{integration-tests-browser,template-cache-local-test}.sh`.

- [ ] Write failing tests for each adapter's verified-byte source, config location, missing/unpropagated versus corrupt data, Fastly JA4/settings-only failed-startup path, and unchanged adapter health/status matrix. Add a test that no adapter reconstructs plan, registry, browser assets, or digest.
- [ ] Replace direct core config/catalog imports with the integrations facade. Keep one resolved config location and one verified byte source; pass the staged composition result to existing route construction. Fastly alone changes post-store-open missing root/chunk to transient 503, never partial decode; early store-open and JA4 failures stay 500.
- [ ] Give each adapter its own component record, collect the transitive runtime workspace records after static linking, and compute the spec's artifact-specific `build_id`. Add a target-matched `cargo metadata` graph test for each adapter that rejects missing/extra/conflicting labels and a perturbation test for adapter, OpenRTB, integration, browser, compiler/flag, and root-workspace inputs. Only then switch the publisher template key to the full composition digest while preserving its existing URL, host, scheme, cookie, Vary, and schema dimensions.
- [ ] Extract each adapter's fixed route registrations into a checked method/path/pattern manifest and derive the corresponding reserved claims from it. Add a parity test comparing actual router registration, precedence, and fallthrough against claims, including Fastly-only routes, Cloudflare's publisher fallback, and all cache-purge methods. Reject an integration claim that collides with any fixed route before startup.
- [ ] Route CLI read-only commands through `SourceConfigView`, recovery mutators through bounded `PartialSourceConfigView<T>`, and deploy commands through `ValidatedSourceConfig` plus selected-target validation. Add repeatable `config validate --adapter` while preserving EdgeZero `--strict`. Move Prebid typed module requirements out of CLI schema; retain atomic, non-disclosing staging writes.
- [ ] In one atomic review unit, switch the CLI, all adapters, Viceroy generator, and parity helper to the facade/storage serializer and remove core's old concrete `builders()`/`with_plan` and asset lookup paths. Run CLI, all four adapter tests, Viceroy parity, failed-startup cases, and differential goldens. No revision may have two active catalogs or loaders.
- [ ] Add the host-only test exporter now that composition exists: for an explicit fixture, emit exactly its embedded bytes, hashes, IDs, and load phases to a fresh directory. Point out-of-Cargo browser harnesses at its manifest, verify every hash, and remove newest-file/shared-`dist` selection. Run exporter and browser tests before committing this separate tooling change.

### Task 10: Move the fifteen concrete Rust owners and their tests

**Files:** Move `crates/trusted-server-core/src/integrations/{adserver_mock,aps,datadome,didomi,google_tag_manager,gpt,gpt_diagnostics,js_asset_proxy,lockr,nextjs,osano,permutive,prebid,sourcepoint,testlight}` into same-named directories under `crates/trusted-server-integrations/src/integrations/`; add `openrtb/mod.rs`; modify both crates' `lib.rs`, catalog, test support, and migration guards.

- [ ] For each ordinary owner, add a failing catalog/behavior test, move its code and fixtures with a rename-focused diff, remove exactly its transitional export, run its focused tests and API guard, then commit. Suggested independent batches: simple page owners; Next.js/GTM/script claims; DataDome/GPT diagnostics after Task 3; APS/Prebid after Task 2; `adserver_mock` after mediator contracts.
- [ ] Add the built-in Rust-only `openrtb` definition for the current standard profile. End with exactly sixteen catalog entries, one directory per Rust definition, and no concrete import in core. JavaScript-only `creative` remains a fixed prelude, not a Rust definition.
- [ ] Move integration-owned test fixtures/helpers outward; replace core tests needing the real catalog with neutral registrations and an integration-owned production-catalog test factory. Move the complete Fastly-SDK guard over moved sources, including nested Next.js, without weakening neutral core checks.
- [ ] After every batch run `cargo test -p trusted-server-integrations`, `cargo test-fastly`, `cargo clippy-fastly`, completeness/API/path guards, and the differential harness. At the end run native/WASM target checks and prove the transitional Rust-export set is empty.

### Task 11: Move every concrete browser owner, inline program, and test

**Files:** Move `crates/trusted-server-js/lib/src/integrations/` and `lib/test/integrations/` into `crates/trusted-server-integrations-js/lib/`; move integration-owned fixtures from `lib/test/fixtures/` case by case; move `crates/trusted-server-core/src/integrations/gpt_bootstrap.js` and integration-owned executable inline programs to their browser owner; modify `lib/build-all.mjs`, `lib/build-prebid-external.mjs`, `crates/trusted-server-cli/src/prebid_bundle.rs`, and owned Rust includes.

- [ ] For each owner, first add a failing wrapped Vitest/artifact/load-order case, move source and tests with a rename-focused diff, remove its legacy manifest path, run wrapped build/typecheck/lint/format/test and the exporter-based production-artifact check, then commit. Keep owner-specific fixtures with their owner; leave cross-adapter Playwright/parity in the integration-test crate.
- [ ] Move APS renderer document and GPT bootstrap with their existing CSP/header behavior; audit Sourcepoint, GPT diagnostics, DataDome, Didomi, GPT, Prebid, and Sourcepoint executable inline inputs. Browser core retains no concrete APS import and Rust retains no handwritten production integration browser algorithm.
- [ ] Make `ts prebid bundle` resolve one canonical package root and all sibling source realpaths in the same checkout, preserve package exports and managed User ID/analytics selection, and prove a two-worktree test cannot use a compile-time path from another checkout.
- [ ] Run wrapped JS gates, APS/Prebid/GPT production artifact tests, Playwright initial-render and shared suites, all adapter parity tests, and the differential harness. Prove every moved test is selected, the legacy browser manifest path set is empty, and no duplicate unused source tree remains.

### Task 12: Exit milestone 1 only with schema-one parity

**Files:** Modify `.cargo/config.toml`, `.github/workflows/{format,test,integration-tests}.yml`, `AGENTS.md`, relevant `.claude/agents/` and `.claude/commands/`, `scripts/`, `docs/guide/`, crate READMEs, and `crates/trusted-server-core/src/migration_guards.rs`; create the stale-path/selected-test guard under `scripts/`.

- [ ] Add a repository guard for live references to old concrete Rust and browser paths, including constructed paths and test selections; allowlist historical docs only. Update every explicit Fastly alias package list and a dedicated host integrations-crate test/clippy gate. Keep Fastly as sole default member and preserve CLI/codegen host gates.
- [ ] Prove the catalog has sixteen definitions, browser discovery includes deferred/standalone modules, core's dependency graph has no integrations crate, adapters/CLI do not import vendor modules, no transitional API/path remains, and all schema-one differential outputs match except the spec's explicit milestone-one corrections.
- [ ] Run every repository gate listed in `AGENTS.md`: Rust format, all target-matched clippy aliases and tests, host CLI/codegen gates, native/WASM checks/builds, cross-adapter parity, wrapped JS build/typecheck/lint/format/Vitest/Playwright, documentation snippets/VitePress and docs format. Record exact commands, exit codes, and intentional behavior deltas.
- [ ] Merge milestone 1 only as a complete crate boundary. Do not activate schema 2 or schema-2 operator source in this PR.

## Milestone 2 — ordered source, storage, and rollout

### Task 13: Add ordered source parsing and strong identities behind a fence

**Files:** Create `crates/trusted-server-integrations/src/{source_config,provider_ids}.rs` (host feature for source parsing); modify `src/{config,validation}.rs`, `crates/trusted-server-core/src/{auction_config_types,auction/plan}.rs`, workspace `Cargo.toml` and `Cargo.lock`.

- [ ] Add failing fixtures for nonlexical parent and provider order, descendant-before-parent, shorthand parent, missing `enabled`, disabled retained settings, unknown IDs/fields, old/new syntax mixtures, local/qualified ID grammar, and overlays that try to create/remove/reorder tables.
- [ ] Upgrade the direct `toml_edit` to the selected TOML-1.1 generation and enable direct `toml/preserve_order` only in the host `source-config` feature used by normal CLI/install graphs. Parse source structure before the EdgeZero typed overlay; check typed order equals the pre-pass order. Keep WASM runtime graphs free of host pre-pass code.
- [ ] Implement the new `[integrations.<id>]` parser as an internal, test-only candidate path with explicit parent `enabled` and nested providers. Normal validate/diff/push still use only schema-one source, and no new-source candidate can serialize or reach a remote write yet. Add `IntegrationId`, `LocalProviderId`, `QualifiedProviderId`, and distinct `ExternalProviderLabel` with exact serde/Display and collision tests.
- [ ] Run host source-parser, CLI/install dependency-graph, WASM target, and parity tests. Assert an ordinary command rejects new-source syntax while the fence is active. Commit the inert parser/identity work before switching storage or runtime order.

### Task 14: Add schema-2 storage DTO and the dual stored reader

**Files:** Create `crates/trusted-server-integrations/src/{stored_config,legacy_config}.rs`; modify `src/{config,composition,secret_metadata}.rs`, `crates/trusted-server-core/src/config_payload.rs`, CLI and integration-test serializer callers.

- [ ] Write failing TOML → typed → envelope → runtime round trips with permuted JSON map members, sidecar missing/duplicate/extra IDs, every stored integration `enabled` (including `false`), default omission, disabled source-shaped values, and unchanged DataDome secret object paths.
- [ ] Serialize schema 2 as object maps plus `integration_order` and local `provider_order` sidecars; always emit and require parent `enabled`. Validate the sidecar/map bijection at source-validation, serialization, and runtime read. Runtime order must never come from JSON map iteration.
- [ ] Keep one schema-one converter that preserves current unknown-ID/default acceptance, implicit APS, globally lexical flat provider sequence, and local external labels. Reject unknown schema marker values before secret resolution; prove archived rollback binaries reject schema-2 root markers.
- [ ] Keep the schema-2 writer and dual reader reachable only from tests/internal candidate validation until Tasks 15–17 have migrated CLI consumers, activated schema-2 ordering, and installed adapter write fences. Normal CLI commands must still reject new-source syntax or refuse its remote write; add an explicit negative test after this commit. One reviewed final cutover commit removes that fence only after Task 17's no-write gates pass.
- [ ] Add bidirectional release-compatibility corpus coverage: candidate writer → oldest rollback/deployed/candidate readers and frozen deployed/live schema-2 bytes → candidate reader, including generated leaf/variant coverage. Run serializer, secret, adapter-loader, and source parity tests; commit the storage boundary separately from order activation.

### Task 15: Migrate CLI source consumers and provide a safe migration command

**Files:** Modify `crates/trusted-server-cli/src/{app_config,run,prebid_bundle}.rs`, `src/commands/config/{mod,ad_templates}.rs`, `src/commands/audit/{ad_templates,generate/validate}.rs`; create `src/commands/config/migrate.rs`; modify `crates/trusted-server-integration-tests/{src/bin/generate-viceroy-config.rs,tests/common/config.rs}`.

- [ ] Write failing CLI cases for one exact source read under concurrent replacement, source/pre-pass/overlay parity, stale environment path names, selected-target diff/push/validate, read-only overlay views, bounded recovery edits, and non-disclosing errors. Confirm `--strict` remains EdgeZero's manifest check.
- [ ] Implement `ts config migrate --dry-run` as a local-only, comment/permission-preserving legacy-source reader that emits a validated schema-2 candidate. Report explicit default normalization, every provider/hook/browser reorder, renamed overlays, unknown tables, and manual decisions; require per-ID unknown discard, explicit noninteractive reorder acceptance, and unchanged-file check before an interactive write.
- [ ] Implement the candidate-path Prebid bundle, ad-template, audit, and fixture-generator changes behind the same source cutover fence as Tasks 13–14. Before Task 18, ordinary commands retain their schema-one behavior and inventory, while explicit candidate/migrate tests exercise the new paths. At final cutover, Prebid requires an existing explicit parent and edits only descendants, preserving file mode and staged hash/SRI behavior; update help/rule counts and exact-message/docs snippets in that same review unit.
- [ ] Run CLI command tests, actual `cargo install --path crates/trusted-server-cli --locked` in a temporary install root, Viceroy fixture generation/parity, and secret-sentinel tests. Commit all source consumers before any schema-2 remote write is enabled.

### Task 16: Switch Rust and browser execution to declaration order

**Files:** Modify `crates/trusted-server-core/src/{integration/registry.rs,auction/plan.rs,auction/orchestrator.rs,auction/provider.rs,publisher.rs}`, `crates/trusted-server-integrations/src/{composition,catalog}.rs`, browser dispatcher and integration IIFEs, and relevant tests.

- [ ] Write failing tests for each comparable hook phase, immediate/deferred browser lists, provider launch/response/mediator-input/tie order, remaining deadline before each launch, plan-order recovery after failure, and ordered diagnostics. Include globally interleaved schema-one provider IDs to prove old order remains unchanged under the dual reader. Cover known-disabled bidder suppression without client-side fallback and disabled-mediator local ranking.
- [ ] Add a stored-schema-2 catalog-drift test: widen one script-source claim and one integration-owned native route in a candidate catalog after a blob is stored. Runtime must stay available; the exact-URL asset-policy clamp restores the original first-party path for an enabled asset or removes a blocked asset, and asset-wins handling drops only the newly colliding integration route. It emits redacted sampled diagnostics. Fixed-route or unrelated native/native conflicts still fail composition.
- [ ] Feed ordinal-bearing neutral plan and registration inputs from schema-2 sidecars. Use definition-local order only within a single integration. Remove numeric/lexical browser handler sorting; keep fixed core/creative, diagnostics, finalizer, and deferred lifecycle phases explicit.
- [ ] Validate complete script-source and route claims before execution, including parent-disabled pruning and policy-consistent final chain outcome; do not give `js_asset_proxy` an invisible first phase. Keep corrected #1199/#1208 behavior identical for schema one. Use qualified external labels only for schema 2.
- [ ] Run ordering, auction, browser, route, cache, and differential tests on both schema versions and every adapter. Commit the intentional ordering change only with all dependent consumers and diagnostics ready.

### Task 17: Fence writes and implement adapter-specific rollout tooling

**Files:** Modify `crates/trusted-server-cli/src/run.rs` and config command modules; create focused copy/export/rollout-record helpers under `crates/trusted-server-cli/src/commands/config/`; modify Fastly, Cloudflare, Spin, Axum config-loader tests and `docs/guide/` rollout guidance.

- [ ] Write failing no-mutation tests for schema-2 push to an unverified Fastly tuple, Cloudflare KV, and unproven remote Spin. Add a Fastly `config gc` test that refuses both protected physical stores and any missing/stale accepted-pair record before EdgeZero can sweep.
- [ ] Implement checked-in Fastly generation copy using explicit source/destination service-version/store/root-key tuples, chunk-first/root-last exact-byte copy and full readback hash/length verification. Record accepted active/rollback tuples only after both candidates and POP-visible probes pass; do not mutate currently selected immutable roots during paired edits.
- [ ] Add `ts config validate-stored-catalog --inventory <deployment-inventory> --exports <ephemeral-directory>`. In a test-controlled temporary root, export every authoritative live environment's exact verified envelope and adapter/location metadata, prove inventory/export bijection, then run candidate pure route/script claims without resolving secret values. Refuse missing/tampered exports or any degraded conflict; log only redacted IDs and hashes. The command never deletes its input; release-pipeline cleanup removes only its own nonce-marked, non-symlink temporary directory.
- [ ] For every candidate release, decode each exported live schema-one envelope with both deployed and candidate readers under identical secret resolution and compare normalized settings, activation, hook/browser/route/provider order, external labels, and errors. A mismatch or missing environment blocks promotion even if the archived baseline fixture passes. Run the bidirectional same-schema-2 corpus gate from Task 14 over all live schema-2 exports too.
- [ ] Implement protected Cloudflare `export-binding` with exact selected outer property/envelope bytes and no stdout/overwrite. Keep old Worker code + schema-one binding paired for rollback. Reject remote Spin schema-2 push until versioned export/restore/selection drill exists; allow local Axum migration. No generic force flag bypass.
- [ ] Add authenticated unsampled unique-probe schema/artifact/digest observation and a settings-dependent response predicate without a new public status route. Block activation if live-environment inventory/export bijection, candidate claim validation, version/binding readback, or rollback isolation is unproven. After explicit rollback-window closure, GC may resume only with verified absence of protected version bindings.
- [ ] Run Fastly tuple/GC/503 tests, Cloudflare binding-fallback tests, Spin no-write tests, Axum local migration, live-export/candidate corpus tests, and documented staged rollback drills. Commit tooling and runbook together; do not perform a production cutover as part of the implementation PR.

### Task 18: Exit milestone 2 and define later cleanup

**Files:** Modify `trusted-server.example.toml`, `docs/guide/{configuration,auction-orchestration,integration-guide}.md`, `docs/guide/integrations/{aps,prebid}.md`, `AGENTS.md`, browser/CI scripts and repository path guards; retain `legacy_config.rs`.

- [ ] After Tasks 13–17 pass, make one final source/write activation change in the same review unit as the operator guidance below: activate the new parser and all Prebid/ad-template/audit/fixture command paths together; ordinary commands accept only new `[integrations]` source, serialize schema 2, and call the installed per-adapter push fence. Assert old source, mixed source, and an unverified destination fail before any remote operation. The read-only schema-one blob decoder remains active.
- [ ] Convert examples, operator guides, environment names, CLI diagnostics, dashboards/telemetry migration notes, template-cache smoke, and documentation snippets to the single ordered inventory and qualified schema-2 labels. Pin plain-language explanation that TOML parent order controls comparable hooks and nested-provider priority.
- [ ] Run every milestone-one repository gate again, plus dual-schema, migrate, same-schema release-compatibility, exporter, candidate-live-inventory, and adapter rollback suites. Review exact diff against milestone-one behavior goldens; every intentional change must be identified in the spec.
- [ ] Merge the dual reader and CLI write fences before any schema-2 data is pushed. Keep the schema-one stored decoder and archived writer for the rollback window. Remove the decoder only in a later separately reviewed release after every production adapter, including Spin, has completed its rollback drill.

## Review checkpoints

1. **Neutral Rust contracts:** after Tasks 1–3, confirm the new API delta is closed and corrected schema-one behavior is unchanged.
2. **Browser boundary:** after Tasks 4–7, confirm exact embedded bytes are used by runtime and out-of-Cargo tests; no private state copy or shared-`dist` race exists.
3. **Concrete extraction:** after Tasks 8–12, confirm two crates own all concrete sources, every consumer uses one composition root, transitional sets are empty, and full schema-one gates pass. This is the first merge milestone.
4. **Ordered config and rollout:** after Tasks 13–18, confirm source/store/runtime order, labels, adapter write fences, and rollback evidence. This is the second merge milestone.

At each checkpoint, perform a self-review against all acceptance criteria in the spec, inspect the dependency graph and public API diff, and request an independent code review. Treat a new capability or public symbol outside the reviewed manifest as a design question, not an opportunistic extraction shortcut.
