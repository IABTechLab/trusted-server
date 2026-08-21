# APS Render Fix and TSJS Resilience Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use
> `superpowers:subagent-driven-development` (recommended) or
> `superpowers:executing-plans` to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking. Use
> `superpowers:test-driven-development` for every behavior change and
> `superpowers:verification-before-completion` before claiming a task, phase, or
> plan complete.

**Goal:** Finish the APS render fix and resilient TSJS hard cutover on the exact
`rc/202608` base, preserving all affected rc behavior while closing every remaining
revision-43 contract gap.

**Architecture:** Keep the already-landed single-runtime/first-display architecture
and complete it rather than replaying its historical implementation. The browser
receives one server-sealed canonical JSON transport, synchronously validates its
critical release relationships, and freezes one immutable boot snapshot; no-agent pages synchronously prepare,
activate, and commit the only persistent runtime, while eligible projected pages use
the bounded first-display owner and atomic post-paint takeover. APS rendering moves
to a Trusted Server-owned top-page mount with independent bootstrap and renderer
nonces, nested TS-authored data documents, an exact MessageChannel envelope, and one
committed-artifact lifecycle for direct and PUC paths.

**Tech Stack:** Rust 1.95 (`serde`, `error-stack`, `http`), TypeScript 6, Vitest,
Node test runner, esbuild, Playwright, the four runtime adapters, GitHub Actions, and
checked-in shell/Node CI scripts.

---

## Authority, current state, and execution rules

The sole design authority is
`docs/superpowers/specs/2026-08-04-aps-render-fix-and-tsjs-resilience-design.md`
revision 43. This is the sole implementation plan for that design.

The implementation branch is `feature/aps-tsjs-resilience-rc202608`. Its first
parent is the fetched `origin/rc/202608` commit
`f0825604ec6740111e99dd8a178e3b880e7d772b`; the previously reviewed feature history
at `ecd78a9d11680deece4d4ec13f84be04fdae6b0d` is its second parent through merge
commit `95b562ea820268d6f16da08863dfa9f71076e4d2`. That integrated history already
implements the descriptor/projection contract, proxy route, core runtime, first-
display split, integration catalog, hard cutover, package upgrade, and most
verification surfaces. Those commits are implementation history, not evidence that
revision 43 is complete.

The final Task 17 refresh fetched and integrated `origin/rc/202608` at
`d4cd2cc823718d64ae73bcb068e5eab03ecd901a` on 2026-08-21. Its rc-owned package and
operator-document changes are retained. Its raw-global GAM-attribution transport is
superseded by the typed immutable GPT configuration carrier and sole parser-time GPT
owner, and its mutable publisher-native APS experiment is superseded by the single
Trusted Server renderer owner required by revision 43.

`origin/rc/202608` is the behavior, API, dependency, CI, and performance baseline.
Do not merge `main` separately. Before final verification, fetch the release branch;
if it advanced, merge its exact tip and repeat the overlap audit and complete gates.
The PR targets `rc/202608`.

The retired historical snapshot named by the design is concept evidence only. It is
never fetched, merged, rebased, cherry-picked, built, or used as a performance
baseline. EdgeZero, CLI, SSAT debug, admin, response-cache, analytics, persistence,
billing, experiments, and deployment routing receive no feature work. Preserve rc
changes in those areas if a merge overlaps them.

Hard-cutover rules are non-negotiable:

- no backward-compatible alias, dual runtime, protocol fallback, mutable generic
  config surface, or old route/API;
- no vendored, pinned, cached, hashed, or offline APS runner, GPT, or PUC bytes;
- the APS runner remains the live fixed-target proxy at
  `/integrations/aps/runner.js` and receives no Trusted Server successful-response
  cache requirement;
- the separately built `prebid.js@10.26.0` artifact remains the only explicitly
  isolated external artifact and contains no TSJS behavior;
- no DynamoDB, Tinybird, new cache project, or analytics requirement;
- preserve the rc PBS Cache behavior as a GPT-owned black-box regression surface;
- no new design or plan document. Schemas, fixtures, scripts, tests, and evidence
  are implementation artifacts and may remain separate files;
- workflow logic beyond simple command invocation lives in checked-in script files,
  not inline shell, JavaScript, or Python embedded in Actions YAML.

For every production change, follow strict RED -> GREEN -> REFACTOR:

1. add the smallest focused failing test;
2. run it and confirm it fails for the intended missing behavior, not setup;
3. implement the minimum complete contract;
4. rerun the focused test and adjacent regressions;
5. run formatting for touched languages;
6. inspect `git diff --check` and `git status --short`;
7. commit only the task's paths with a sentence-case imperative subject and no
   semantic prefix.

If a planned RED test already passes on the reconciled branch, inspect the actual
owner and adversarial boundaries. Record the requirement as verified in this plan,
do not manufacture a failure, and make no production edit for that requirement.

Never run bare `cargo test --workspace`. Use the adapter aliases from `CLAUDE.md`.

## Remaining source ownership

```text
Rust boot/config owner
  crates/trusted-server-core/src/tsjs.rs
  crates/trusted-server-core/src/integrations/registry.rs
  crates/trusted-server-core/src/html_processor.rs
  crates/trusted-server-core/src/integrations/*.rs

Immutable boot and runtime transaction
  crates/trusted-server-js/lib/src/core/{bootstrap,types}.ts
  crates/trusted-server-js/lib/src/kernel/{integration_registry,runtime,phase_loader,release_catalog}.ts
  crates/trusted-server-js/lib/src/shared/{first_display_contracts,first_display_handoff}.ts
  crates/trusted-server-js/lib/src/first_display/**

APS top-mount protocol
  crates/trusted-server-core/src/integrations/aps.rs
  scripts/generate-aps-renderer-contract.mjs
  crates/trusted-server-js/lib/src/core/contracts/aps_renderer.ts
  crates/trusted-server-js/lib/src/adapters/messaging.ts
  crates/trusted-server-js/lib/src/services/{render,slots,puc_bridge}.ts
  crates/trusted-server-js/lib/src/kernel/contracts/puc_dynamic_owner.ts
  crates/trusted-server-js/lib/src/integrations/aps/{render,module}.ts
  crates/trusted-server-js/lib/src/first_display/{render_bridge,leaf/aps_protocol,slices/aps}.ts

Rc behavior preservation
  crates/trusted-server-core/src/{creative_opportunities,publisher,settings}.rs
  crates/trusted-server-core/src/integrations/{gpt,gpt_diagnostics,prebid}.rs
  crates/trusted-server-js/lib/src/integrations/{gpt,gpt_diagnostics,prebid}/**
  docs/guide/integrations/{aps,gpt,gpt-diagnostics,prebid}.md

Release proof
  crates/trusted-server-js/lib/scripts/**
  scripts/ci/**
  scripts/validate-tsjs-performance-evidence.mjs
  crates/trusted-server-integration-tests/browser/**
  .github/workflows/{test,tsjs-performance-gate,aps-real-gam}.yml
```

## Phase 0 — Prove the rc baseline and identify actual gaps

### Task 0: Add the schema-bumped rc-baseline concept audit

**Files:**

- Modify: `crates/trusted-server-js/lib/test/fixtures/contracts/current-main-concept-audit.json`
- Modify: `crates/trusted-server-js/lib/scripts/check-retired-concept-audit.mjs`
- Modify: `crates/trusted-server-js/lib/test/contract/retired-concept-audit.test.mjs`
- Modify: `crates/trusted-server-js/lib/package.json`
- Modify: `.github/workflows/test.yml`

- [ ] **Step 1: Add failing checker tests for two immutable evidence layers.** Require
      a schema-versioned root containing the untouched historical-main rows and one
      `rcBaseline` object bound to the exact rc SHA. Require exactly 23 rc rows with
      exact keys `id`, `baselineSha`, `classification`, `ownerPaths`, `testPath`,
      `command`, `result`, and `disposition`; final classifications are only
      `baseline-owned` or `implementation-gap`. Reject missing fields, stale SHA,
      duplicate ids, proof-pending/coverage-gap, retired-source commands, or mutation
      of historical evidence.

- [ ] **Step 2: Run the RED proof.**

  ```bash
  npm --prefix crates/trusted-server-js/lib test -- --run test/contract/retired-concept-audit.test.mjs
  ```

  Expected: FAIL because the fixture/checker has only the historical-main layer.

- [ ] **Step 3: Implement the versioned fixture and checker.** Preserve every
      historical row byte-for-byte as provenance. Add the rc layer, make the checker
      print both layers, and make only a complete passing rc layer satisfy phase
      exit. Keep the existing in-spec 144-path inventory digest authoritative.

- [ ] **Step 4: Classify every ledger row against the untouched rc baseline.** Use a
      detached worktree at the recorded SHA. For each row, identify the baseline
      owner and run the exact focused contract. A setup failure remains proof-pending;
      author a test-only proof before classifying a gap. Do not copy production code
      from historical evidence.

- [ ] **Step 5: Put CI orchestration in a script.** If the workflow needs more than
      `npm run check:concept-audit`, add or modify a checked-in `.mjs` or `.sh` file
      and have Actions invoke it. Do not place an inline program in the workflow.

- [ ] **Step 6: Run the GREEN proof and integrity checks.**

  ```bash
  npm --prefix crates/trusted-server-js/lib run check:concept-audit
  npm --prefix crates/trusted-server-js/lib test -- --run test/contract/retired-concept-audit.test.mjs
  git diff --check
  ```

  Expected: PASS; all 23 rc rows are final and reproducible.

- [ ] **Step 7: Commit.**

  ```bash
  git add crates/trusted-server-js/lib/test/fixtures/contracts/current-main-concept-audit.json crates/trusted-server-js/lib/scripts/check-retired-concept-audit.mjs crates/trusted-server-js/lib/test/contract/retired-concept-audit.test.mjs crates/trusted-server-js/lib/package.json .github/workflows/test.yml
  git commit -m "Classify TSJS concepts against the rc baseline"
  ```

### Task 1: Reconcile the overlapping rc changes before new behavior

**Files:**

- Modify only if a demonstrated gap exists: rc-overlapping files named by the Task
  0 owner paths
- Test: `crates/trusted-server-core/src/publisher.rs`
- Test: `crates/trusted-server-core/src/integrations/gpt.rs`
- Test: `crates/trusted-server-core/src/integrations/gpt_diagnostics.rs`
- Test: `crates/trusted-server-js/lib/test/integrations/gpt/module.test.ts`
- Test: `crates/trusted-server-js/lib/test/integrations/gpt_diagnostics/`
- Test: `crates/trusted-server-js/lib/test/integrations/datadome/`

- [ ] **Step 1: Run the rc overlap contracts before editing.** Cover #1008 template
      enablement/cache policy, #1013 C2/ESI assembly, #1025/#1032 diagnostics, #1034
      GAM attribution, #992 DataDome exclusions, and all existing adapter-parity
      contracts. Record owner and pass/gap in Task 0's rc rows.

- [ ] **Step 2: Resolve demonstrated gaps only.** The design supersedes #1008 only
      by making `creative_opportunities.enabled` required. Preserve C2/ESI,
      diagnostics, GAM attribution, DataDome, CLI/admin/SSAT debug/response-cache,
      and EdgeZero behavior without broad refactoring.

- [ ] **Step 3: Run focused Rust and JS regressions.**

  ```bash
  cargo test --package trusted-server-core --target "$(rustc -vV | sed -n 's/^host: //p')" publisher
  cargo test --package trusted-server-core --target "$(rustc -vV | sed -n 's/^host: //p')" integrations::gpt
  cargo test --package trusted-server-core --target "$(rustc -vV | sed -n 's/^host: //p')" integrations::gpt_diagnostics
  npm --prefix crates/trusted-server-js/lib test -- --run test/integrations/gpt/module.test.ts test/integrations/gpt_diagnostics test/integrations/datadome
  ```

  Expected: PASS. If no gap required a production edit, mark the task verified and
  do not create an empty commit.

- [ ] **Step 4: Re-verify the already-landed direct-development toolchain upgrade.**
      Confirm the lockfile uses the newest stable mutually compatible TypeScript,
      `typescript-eslint`, ESLint, Vitest, Vite, esbuild, Prettier, jsdom, and Node
      declarations supported by the repository-pinned Node major. Keep
      `prebid.js@10.26.0` exact. Require a clean install, peer-clean dependency tree,
      strict typecheck, lint, tests, build, and external-Prebid verification; update
      package files test-first only if the current lockfile no longer satisfies that
      contract.

  ```bash
  npm --prefix crates/trusted-server-js/lib ci
  npm --prefix crates/trusted-server-js/lib ls --all
  npm --prefix crates/trusted-server-js/lib run typecheck
  npm --prefix crates/trusted-server-js/lib run lint
  npm --prefix crates/trusted-server-js/lib test
  npm --prefix crates/trusted-server-js/lib run build:prebid-external
  ```

## Phase 1 — One immutable boot snapshot and one parser-time runtime

### Task 2: Emit the exact typed integration-config carrier

**Files:**

- Modify: `crates/trusted-server-core/src/tsjs.rs`
- Modify: `crates/trusted-server-core/src/integrations/registry.rs`
- Modify: `crates/trusted-server-core/src/html_processor.rs`
- Modify as needed for explicit browser projections:
  `crates/trusted-server-core/src/integrations/{aps,datadome,didomi,google_tag_manager,gpt,lockr,osano,permutive,prebid,sourcepoint,testlight}.rs`
- Test: colocated Rust tests in the files above

- [ ] **Step 1: Add failing serialization/admission tests.** Require
      `IntegrationConfigsV1 {version:1,entries}` with the exact ordered id union
      `aps, datadome, didomi, google_tag_manager, gpt, lockr, osano, permutive,
prebid, sourcepoint, testlight`. Emit each enabled product once, omit disabled
      products, emit APS `{}` when enabled, and reject a manifest/config predicate
      mismatch. Prove secrets, credentials, server endpoints, cookies, auth headers,
      and private policy have no browser projection type.

- [ ] **Step 2: Run the RED Rust tests.**

  ```bash
  cargo test --package trusted-server-core --target "$(rustc -vV | sed -n 's/^host: //p')" tsjs::tests
  cargo test --package trusted-server-core --target "$(rustc -vV | sed -n 's/^host: //p')" integrations::registry::tests
  ```

  Expected: FAIL because `TsjsBootV1` has no `integrations` field.

- [ ] **Step 3: Implement explicit typed projections.** Build config from existing
      typed rc settings during registry construction, not request-time reflection.
      Serialize once inside `TsjsBootV1`; do not create integration-config script
      tags or `window.__tsjs_*` globals. Keep creative and diagnostics in their
      dedicated typed boot fields.

- [ ] **Step 4: Enforce server-side caps and exactness.** Reject more than 11
      entries, wrong order/duplicates, invalid JSON values, depth above 16, more than
      4,096 values, strings/keys above 4,096 UTF-8 bytes, one entry above 65,536
      canonical bytes, or the carrier above 524,288 bytes.

- [ ] **Step 5: Run GREEN and adapter-facing Rust regressions.**

  ```bash
  cargo test --package trusted-server-core --target "$(rustc -vV | sed -n 's/^host: //p')" tsjs::tests
  cargo test --package trusted-server-core --target "$(rustc -vV | sed -n 's/^host: //p')" integrations::registry::tests
  cargo test-cloudflare
  cargo test-spin
  cargo fmt --all -- --check
  ```

  Expected: PASS.

- [ ] **Step 6: Commit.**

  ```bash
  git add crates/trusted-server-core/src/tsjs.rs crates/trusted-server-core/src/integrations/registry.rs crates/trusted-server-core/src/html_processor.rs crates/trusted-server-core/src/integrations
  git commit -m "Add typed TSJS integration boot configuration"
  ```

### Task 3: Validate, copy, freeze, and attenuate boot configuration

**Files:**

- Modify: `crates/trusted-server-js/lib/src/core/types.ts`
- Modify: `crates/trusted-server-js/lib/src/core/bootstrap.ts`
- Modify: `crates/trusted-server-js/lib/src/kernel/release_catalog.ts`
- Modify: `crates/trusted-server-js/lib/src/kernel/integration_registry.ts`
- Modify: `crates/trusted-server-js/lib/src/kernel/runtime.ts`
- Modify: `crates/trusted-server-js/lib/src/shared/first_display_contracts.ts`
- Modify: `crates/trusted-server-js/lib/src/shared/first_display_handoff.ts`
- Modify: `crates/trusted-server-js/lib/src/first_display/agent.ts`
- Modify: `crates/trusted-server-js/lib/src/first_display/slices/*.ts`
- Modify: `crates/trusted-server-js/lib/src/integrations/*/module.ts`
- Modify: `crates/trusted-server-js/lib/src/integrations/{osano,permutive,sourcepoint}/*.ts`
- Test: `crates/trusted-server-js/lib/test/core/bootstrap.test.ts`
- Test: `crates/trusted-server-js/lib/test/kernel/{integration_registry,runtime,release_catalog}.test.ts`
- Test: `crates/trusted-server-js/lib/test/first_display/{agent,handoff,slices}.test.ts`
- Test: integration module tests under `crates/trusted-server-js/lib/test/integrations/`

- [ ] **Step 1: Add failing exact-shape and hostile-object tests.** At the complete
      runtime/public object-form boundaries, cover all caps from Task 2 plus
      accessors, symbols, sparse arrays, custom prototypes, cycles, repeated aliases,
      non-finite numbers, UTF-8 boundaries, throwing traps, and copy/freeze failure.
      Require validation before effects and `abi_mismatch` on failure. Task 15A
      separately hard-cuts the production inline wire to canonical JSON text.

- [ ] **Step 2: Add failing immutable-snapshot tests.** Mutate/replace the public boot
      target after bootstrap capture and prove the parsed lexical payload is never
      reread. Agent,
      prepared runtime, deferred modules, and final `tsjs.boot` must all observe the
      same recursively frozen copy and canonical config digest. Handoff carries the
      digest, never a recopy of config.

- [ ] **Step 3: Add failing attenuation tests.** Each catalog module receives only
      its product's exact config, or the dedicated creative/diagnostics field, or
      `undefined` when catalogued as none. A module cannot read the complete map.
      Later modules share the same product value, not a second entry.

- [ ] **Step 4: Run RED.**

  ```bash
  npm --prefix crates/trusted-server-js/lib test -- --run test/core/bootstrap.test.ts test/kernel/integration_registry.test.ts test/kernel/runtime.test.ts test/kernel/release_catalog.test.ts test/first_display/agent.test.ts test/first_display/handoff.test.ts test/first_display/slices.test.ts
  ```

- [ ] **Step 5: Implement admission and catalog binding.** At object-form boundaries,
      use own property descriptors only, copy into ordinary objects/arrays,
      recursively freeze, retain the closure snapshot, calculate the canonical
      digest, and pass only the catalog-declared value to first-display slices and
      integration preparation. Delete raw per-integration bootstrap globals and
      activation attributes. Task 15A later removes this full validator graph from
      the trusted production lexical transport without weakening these boundaries.

- [ ] **Step 6: Add exact typed validators in each product module.** Unknown or
      missing fields and manifest/config inclusion mismatches are `abi_mismatch`;
      there are no silent browser-data defaults.

- [ ] **Step 7: Run GREEN plus all module tests.**

  ```bash
  npm --prefix crates/trusted-server-js/lib test -- --run test/core/bootstrap.test.ts test/kernel test/first_display test/integrations
  npm --prefix crates/trusted-server-js/lib run typecheck
  npm --prefix crates/trusted-server-js/lib run lint
  ```

- [ ] **Step 8: Commit.**

  ```bash
  git add crates/trusted-server-js/lib/src crates/trusted-server-js/lib/test
  git commit -m "Validate and attenuate immutable TSJS boot configuration"
  ```

### Task 4: Make no-agent takeover preparation synchronous

**Files:**

- Modify: `crates/trusted-server-js/lib/src/kernel/integration_registry.ts`
- Modify: `crates/trusted-server-js/lib/src/kernel/runtime.ts`
- Modify: `crates/trusted-server-js/lib/src/core/bootstrap.ts`
- Modify: every takeover registration in `crates/trusted-server-js/lib/src/integrations/**`
- Modify: `crates/trusted-server-js/lib/src/composition/browser_test.ts`
- Test: `crates/trusted-server-js/lib/test/kernel/{integration_registry,runtime}.test.ts`
- Test: every affected module test
- Test: `crates/trusted-server-integration-tests/browser/tests/shared/tsjs-runtime.spec.ts`

- [ ] **Step 1: Add failing registration-shape tests.** A takeover registration has
      exactly six keys and requires both callable `prepareSync` and `prepare`; a
      deferred registration has exactly five keys and only `prepare`. Reject missing,
      extra, inherited, accessor-backed, wrong-phase, or Promise/thenable-returning
      synchronous entry points.

- [ ] **Step 2: Add failing no-yield ordering tests.** On a no-agent page, assert one
      classic parser-blocking evaluation performs every `prepareSync`, then every
      synchronous `activate`, then kernel commit before publisher parser work,
      microtasks, timers, network, or callbacks. Check the monotonic deadline before
      and after every activation and before commit.

- [ ] **Step 3: Add failing agent-takeover tests.** Post-paint takeover uses only
      `prepare`, may await effect-inert work, and adopts parser-time obligations
      without installing a competing guard/listener/attribution owner.

- [ ] **Step 4: Run RED.**

  ```bash
  npm --prefix crates/trusted-server-js/lib test -- --run test/kernel/integration_registry.test.ts test/kernel/runtime.test.ts test/first_display/takeover.test.ts
  ```

- [ ] **Step 5: Implement the discriminated registration ABI and execution modes.**
      Share pure construction behind the two takeover entry points. Do not allow a
      detached continuation, activation during preparation, or a second activation.
      Keep all request-capable/SDK/network work behind `afterCommit`.

- [ ] **Step 6: Add `prepareSync` to every takeover module.** Preserve module order,
      interfaces, reverse-order rollback, and existing effect ownership. Do not add
      `prepareSync` to deferred lifecycle/later modules.

- [ ] **Step 7: Run GREEN and parser-order browser proof.**

  ```bash
  npm --prefix crates/trusted-server-js/lib test -- --run test/kernel test/integrations test/first_display/takeover.test.ts
  npm --prefix crates/trusted-server-js/lib run typecheck
  npm --prefix crates/trusted-server-integration-tests/browser test -- --project=chromium tests/shared/tsjs-runtime.spec.ts
  ```

- [ ] **Step 8: Commit.**

  ```bash
  git add crates/trusted-server-js/lib/src crates/trusted-server-js/lib/test crates/trusted-server-integration-tests/browser/tests/shared/tsjs-runtime.spec.ts
  git commit -m "Activate no-agent TSJS synchronously"
  ```

## Phase 2 — Harden APS containment and top-page ownership

### Task 5: Hard-cut the renderer route to the v2 materialization bootstrap

**Files:**

- Modify: `crates/trusted-server-js/lib/test/fixtures/aps-renderer-v1.schema.json`
- Modify: `crates/trusted-server-js/lib/test/fixtures/aps-renderer-v1.json`
- Modify: `crates/trusted-server-js/lib/test/fixtures/aps-renderer-v1-corpus.json`
- Modify: `scripts/generate-aps-renderer-contract.mjs`
- Modify generated: `crates/trusted-server-core/src/integrations/generated/aps_renderer_validator_v1.js`
- Delete generated: `crates/trusted-server-core/src/integrations/generated/aps_renderer_bootstrap_v1.html`
- Create generated: `crates/trusted-server-core/src/integrations/generated/aps_renderer_bootstrap_v2.html`
- Modify: `crates/trusted-server-core/src/integrations/aps.rs`
- Modify: `crates/trusted-server-core/src/publisher.rs`
- Modify: `scripts/integration-tests-aps-runner-proxy.sh`
- Modify: `crates/trusted-server-integration-tests/tests/aps_runner_proxy.rs`
- Test: `crates/trusted-server-js/lib/test/contract/aps-renderer-es5.test.mjs`
- Test: colocated APS Rust tests
- Test: `scripts/integration-tests-aps-runner-proxy.sh`

- [ ] **Step 1: Add failing renderer response tests.** The v2 HTTP response is an
      immutable descriptor-free materialization bootstrap with exact CSP, content
      type, cache, nosniff, no-referrer, no X-Frame-Options, and exact initial
      sandbox. It accepts only its `b1_` fragment and one canonical v2 policy
      configuration from the exact parent. It embeds TS-authored templates, the ES5
      validator, and the fixed runner-proxy path, but no concrete descriptor value,
      vendor bytes, publisher DOM authority, or pre-navigation network operation.

- [ ] **Step 2: Add failing route/security tests.** APS-enabled final publisher
      responses append an independent `Content-Security-Policy: frame-ancestors
'self'`; operator CSP cannot replace it. Disabled APS, wrong methods, unknown
      versions, and the abandoned versioned runner route remain exact local failures
      and never fall through.

- [ ] **Step 3: Run RED.**

  ```bash
  npm --prefix crates/trusted-server-js/lib run check:aps-contract
  node --test crates/trusted-server-js/lib/test/contract/aps-renderer-es5.test.mjs
  cargo test --package trusted-server-core --target "$(rustc -vV | sed -n 's/^host: //p')" integrations::aps::tests
  ```

- [ ] **Step 4: Update the single renderer generator and generated contract.** Keep
      the schema's `x-*` semantic markers documentary; generator code remains the
      enforcement authority. Generate deterministic ES5-compatible v2 bootstrap
      bytes, reuse the shared corpus across Rust, TypeScript, and Node tests, delete
      the v1 body, and serve v1 as an unknown local `404` with no compatibility path.

- [ ] **Step 5: Preserve the live runner proxy exactly.** Keep its fixed target,
      credential/referrer stripping, duplicate-header evidence, identity encoding,
      8 MiB cap, exact media types, five-second total transport deadline, unchanged
      successful bytes, local empty 502 failures, and four-adapter parity. Do not
      store runner bytes or add a success cache header.

- [ ] **Step 6: Make the Cloudflare/Wrangler readiness proof exact.** Before the
      adversarial corpus starts, require two consecutive
      `PROPFIND /integrations/aps/renderer/v2` responses with the complete local 405
      contract, including `Allow: GET` and `Cache-Control: no-store`. Any other
      result resets the consecutive count. Implement this in the actual adapter
      harness `crates/trusted-server-integration-tests/tests/aps_runner_proxy.rs`;
      the shell wrapper only builds/selects the runtime. Keep the corpus's own later,
      independent PROPFIND assertion; do not change production request handling.

- [ ] **Step 7: Run GREEN across generator and adapters.**

  ```bash
  npm --prefix crates/trusted-server-js/lib run generate:aps-contract
  npm --prefix crates/trusted-server-js/lib run check:aps-contract
  node --test crates/trusted-server-js/lib/test/contract/aps-renderer-es5.test.mjs
  cargo test-fastly integrations::aps::tests
  cargo test --package trusted-server-core --target "$(rustc -vV | sed -n 's/^host: //p')" integrations::aps::tests
  cargo test-cloudflare
  cargo test-spin
  ./scripts/integration-tests-aps-runner-proxy.sh --runtime axum
  ./scripts/integration-tests-aps-runner-proxy.sh --runtime fastly
  ./scripts/integration-tests-aps-runner-proxy.sh --runtime cloudflare
  ./scripts/integration-tests-aps-runner-proxy.sh --runtime spin
  ```

- [ ] **Step 8: Commit.**

  ```bash
  git add crates/trusted-server-js/lib/test/fixtures/aps-renderer-v1.schema.json crates/trusted-server-js/lib/test/fixtures/aps-renderer-v1.json crates/trusted-server-js/lib/test/fixtures/aps-renderer-v1-corpus.json scripts/generate-aps-renderer-contract.mjs scripts/integration-tests-aps-runner-proxy.sh crates/trusted-server-integration-tests/tests/aps_runner_proxy.rs crates/trusted-server-core/src/integrations/generated/aps_renderer_validator_v1.js crates/trusted-server-core/src/integrations/generated/aps_renderer_bootstrap_v2.html crates/trusted-server-core/src/integrations/generated/aps_renderer_bootstrap_v1.html crates/trusted-server-core/src/integrations/aps.rs crates/trusted-server-core/src/publisher.rs
  git commit -m "Hard-cut APS rendering to the v2 bootstrap"
  ```

### Task 6: Materialize the exact APS data documents after first action

**Files:**

- Modify: `crates/trusted-server-js/lib/src/shared/aps_documents.ts`
- Modify: `scripts/generate-aps-renderer-contract.mjs`
- Modify generated: `crates/trusted-server-core/src/integrations/generated/aps_renderer_bootstrap_v2.html`
- Modify: `crates/trusted-server-js/lib/src/core/contracts/aps_renderer.ts`
- Modify: `crates/trusted-server-js/lib/src/adapters/messaging.ts`
- Modify: `crates/trusted-server-js/lib/src/integrations/aps/render.ts`
- Modify: `crates/trusted-server-js/lib/src/first_display/leaf/aps_protocol.ts`
- Test: `crates/trusted-server-js/lib/test/integrations/aps/documents.test.ts`
- Test: `crates/trusted-server-js/lib/test/integrations/aps/render.test.ts`
- Test: `crates/trusted-server-js/lib/test/adapters/messaging.test.ts`
- Test: `crates/trusted-server-js/lib/test/first_display/slices.test.ts`

- [ ] **Step 1: Add failing template tests.** Have the v2 server bootstrap generate
      detached documents only after it receives the first-action policy message, using
      independent `b1_` and `n1_` nonces, exact origin validation, exact permanent
      sandbox order, sentinel-once substitution, context escaping, and separate
      65,536-byte pre-encoding caps. The outer document contains only origins,
      nonces, sandbox, and encoded inner template; it contains no descriptor or
      lifecycle authority.

- [ ] **Step 2: Add failing conditional-CSP tests.** For iframe creatives, only the
      Trusted Server origin is an external script source at both inheritance levels.
      For explicitly enabled script creatives, add exactly the validated creative
      HTTPS origin to `script-src` at both levels. Never emit the union policy for an
      iframe creative. The publisher origin cannot be a creative frame target.

- [ ] **Step 3: Add failing channel tests.** Outer accepts the exact inner
      `WindowProxy`/`n1_` readiness once, then transfers port1 inward and port2 to the
      checked top parent. Inner accepts only port1 from its exact outer parent and
      then one exact `ApsDocumentEnvelopeV1 {version,nonce,publisherOrigin,renderer}`.
      Reject unknown keys, wrong prototypes, accessors, wrong ports/sources/nonces,
      replays, and oversized messages before effects.

- [ ] **Step 4: Run RED.**

  ```bash
  npm --prefix crates/trusted-server-js/lib test -- --run test/integrations/aps/documents.test.ts test/integrations/aps/render.test.ts test/adapters/messaging.test.ts test/first_display/slices.test.ts
  ```

- [ ] **Step 5: Implement build-time document materialization and exact parsers.**
      Keep the templates and ES5 validator as generator/test authorities, emit them
      into the server-owned v2 response, and remove them from both first-display and
      persistent production TSJS graphs. The top page sends only exact nonces,
      creative origin, and tag type; no descriptor or generated URL crosses that
      channel. The inner
      clears its fragment, queues the APS event with one-shot resolve/reject, loads
      only the same-origin proxy with anonymous CORS/no-referrer, treats script load
      as progress, and sends completion/failure only on callback settlement.

- [ ] **Step 6: Run GREEN, typecheck, lint, and architecture checks.**

  ```bash
  npm --prefix crates/trusted-server-js/lib test -- --run test/integrations/aps test/adapters/messaging.test.ts test/first_display/slices.test.ts
  npm --prefix crates/trusted-server-js/lib run typecheck
  npm --prefix crates/trusted-server-js/lib run lint
  npm --prefix crates/trusted-server-js/lib run check:architecture
  ```

- [ ] **Step 7: Commit.**

  ```bash
  git add crates/trusted-server-js/lib/src/shared/aps_documents.ts crates/trusted-server-js/lib/src/integrations/aps crates/trusted-server-js/lib/src/core/contracts/aps_renderer.ts crates/trusted-server-js/lib/src/adapters/messaging.ts crates/trusted-server-js/lib/src/first_display crates/trusted-server-js/lib/scripts/check-bundle-budgets.mjs scripts/generate-aps-renderer-contract.mjs crates/trusted-server-core/src/integrations/generated/aps_renderer_bootstrap_v2.html crates/trusted-server-js/lib/test
  git commit -m "Materialize APS documents after first action"
  ```

### Task 7: Mount direct APS through the three-phase top-page protocol

**Files:**

- Modify: `crates/trusted-server-js/lib/src/integrations/aps/render.ts`
- Modify: `crates/trusted-server-js/lib/src/services/render.ts`
- Modify: `crates/trusted-server-js/lib/src/first_display/render_bridge.ts`
- Modify: `crates/trusted-server-js/lib/src/kernel/identity.ts`
- Test: `crates/trusted-server-js/lib/test/integrations/aps/render.test.ts`
- Test: `crates/trusted-server-js/lib/test/services/render.test.ts`
- Test: `crates/trusted-server-js/lib/test/first_display/render_bridge.test.ts`
- Test: `crates/trusted-server-js/lib/test/kernel/identity.test.ts`

- [ ] **Step 1: Add failing nonce-registry tests.** `b1_` and `n1_` are distinct
      CSPRNG roles with two independent live-only registries capped at 256 entries,
      eight collision retries, one-use phase checks, and no cross-role acceptance.
      Cover independent 255/256/257 boundaries and collision exhaustion. Neither
      registry creates a tombstone or expiry entry: attempt disposal removes both
      live entries, the bootstrap listener, and the renderer channel, while exact
      source/port/attempt/generation checks make late work inert.

- [ ] **Step 2: Add failing direct-mount state-machine tests.** Create only the
      bootstrap iframe initially. Require exact node/parent/src/WindowProxy/
      generation/nonce checks before sandbox mutation and the v2 policy-only
      configuration message, then exact materialized outer
      readiness and one port before sending the envelope. Enforce one-second
      insertion, one shared three-second document deadline, and the kernel-owned
      ten-second completion deadline beginning at document acceptance.

- [ ] **Step 3: Add failing outcome/disposal tests.** Completion alone accepts;
      runner load is progress. Map script/CORS/proxy load to `runner_no_load`, reject
      to `runner_failed`, malformed/timeout to the specified reason. Abort,
      supersession, navigation, port error, and late/replayed messages settle once,
      close channels, clear timers, and remove only the pending mount.

- [ ] **Step 4: Run RED.**

  ```bash
  npm --prefix crates/trusted-server-js/lib test -- --run test/kernel/identity.test.ts test/integrations/aps/render.test.ts test/services/render.test.ts test/first_display/render_bridge.test.ts
  ```

- [ ] **Step 5: Implement the shared top-page mount service for direct APS.** Keep
      descriptor validation before DOM mutation, permanent sandbox after bootstrap
      readiness, policy-only server materialization after first action, exact winning
      size/layout, and one terminal latch. Direct mount is an ordinary child and does
      not acquire overlay styling.

- [ ] **Step 6: Run GREEN and adjacent direct-ADM regressions.**

  ```bash
  npm --prefix crates/trusted-server-js/lib test -- --run test/kernel/identity.test.ts test/integrations/aps test/services/render.test.ts test/first_display/render_bridge.test.ts test/first_display/adm_render_bridge.test.ts
  ```

- [ ] **Step 7: Commit.**

  ```bash
  git add crates/trusted-server-js/lib/src/integrations/aps crates/trusted-server-js/lib/src/services/render.ts crates/trusted-server-js/lib/src/first_display/render_bridge.ts crates/trusted-server-js/lib/src/kernel/identity.ts crates/trusted-server-js/lib/test
  git commit -m "Mount direct APS through the top-page owner"
  ```

### Task 8: Cut PUC APS over to protocol v4 top mounts

**Files:**

- Modify: `crates/trusted-server-js/lib/src/kernel/contracts/puc_dynamic_owner.ts`
- Modify: `crates/trusted-server-js/lib/src/core/puc_shell.ts`
- Modify: `crates/trusted-server-js/lib/src/adapters/messaging.ts`
- Modify: `crates/trusted-server-js/lib/src/services/puc_bridge.ts`
- Modify: `crates/trusted-server-js/lib/src/services/slots.ts`
- Modify: `crates/trusted-server-js/lib/src/services/render.ts`
- Modify: `crates/trusted-server-js/lib/src/integrations/gpt/module.ts`
- Modify: `crates/trusted-server-js/lib/src/first_display/{render_bridge,leaf/aps_protocol}.ts`
- Modify: `crates/trusted-server-js/lib/src/composition/browser_test.ts`
- Test: corresponding unit tests
- Test: `crates/trusted-server-integration-tests/browser/tests/shared/aps-puc-lifecycle.spec.ts`

- [ ] **Step 1: Add failing v4 protocol tests.** The PUC outer response contains
      only renderer version `4`, the TS dynamic owner, and ready/refused owner
      metadata. Remove `rendererUrl`, descriptor, ADM, nonce, and old APS-start
      payloads. The dynamic owner performs only helper registration, the 20-second
      control watchdog, and Promise settlement; it never creates an APS iframe.

- [ ] **Step 2: Add failing exact top-slot binding tests.** The authenticated PUC
      `WindowProxy` is never mapped to DOM. Resolve the reservation's exact
      `SlotRecord`, physical GPT object, active cycle, generation, uniquely connected
      top-page slot element, and binding epoch. Revalidate before insertion and
      commit; missing, ambiguous, replaced, disconnected, stale, or multiply claimed
      binding fails `slot_unresolved` without a fallback locator.

- [ ] **Step 3: Add failing overlay tests.** Append one absolute hidden child inside
      the exact slot host. Compare-own `position:relative` only when computed position
      is static. Keep GAM/PUC/SafeFrame content connected. Document acceptance and
      runner load stay hidden; only valid render completion reveals and commits the
      overlay. Failure removes only TS state and compare-restores only still-owned
      styles.

- [ ] **Step 4: Add failing owner-control tests.** Kernel sends only
      `TS APS Top Mount Started` informational control data, then exactly one final
      `TS Owner Settled`. Abort remains attempt-owned. Malformed data, messageerror,
      silent channel, watchdog, disposal, or replay rejects once; settlement-post
      throw cannot alter kernel outcome. Keep PUC ADM on its existing exact owner
      protocol.

- [ ] **Step 5: Run RED.**

  ```bash
  npm --prefix crates/trusted-server-js/lib test -- --run test/kernel/contracts/puc_dynamic_owner.test.ts test/services/puc_bridge.test.ts test/services/slots.test.ts test/services/render.test.ts test/integrations/gpt/module.test.ts test/first_display/render_bridge.test.ts test/adapters/messaging.test.ts
  ```

- [ ] **Step 6: Implement v4 and delete old protocol fields.** Do not retain an alias
      or parser branch for the prior renderer version. Do not vendor PUC or add its
      bytes to fixtures; use only the locally authored public-contract harness.

- [ ] **Step 7: Run GREEN including browser lifecycle.**

  ```bash
  npm --prefix crates/trusted-server-js/lib test -- --run test/kernel/contracts/puc_dynamic_owner.test.ts test/services test/integrations/gpt/module.test.ts test/first_display test/adapters/messaging.test.ts
  npm --prefix crates/trusted-server-integration-tests/browser test -- --project=chromium tests/shared/aps-puc-lifecycle.spec.ts
  ```

- [ ] **Step 8: Commit.**

  ```bash
  git add crates/trusted-server-js/lib/src crates/trusted-server-js/lib/test crates/trusted-server-integration-tests/browser/tests/shared/aps-puc-lifecycle.spec.ts
  git commit -m "Move PUC APS rendering to top-page mounts"
  ```

### Task 9: Give committed APS overlays one exact retirement owner

**Files:**

- Modify: `crates/trusted-server-js/lib/src/adapters/googletag.ts`
- Modify: `crates/trusted-server-js/lib/src/services/slots.ts`
- Modify: `crates/trusted-server-js/lib/src/services/render.ts`
- Modify: `crates/trusted-server-js/lib/src/integrations/gpt/{module,later}.ts`
- Modify: `crates/trusted-server-js/lib/src/first_display/adapters/googletag.ts`
- Modify: `crates/trusted-server-js/lib/src/first_display/{agent,render_bridge}.ts`
- Test: corresponding unit tests
- Test: `crates/trusted-server-integration-tests/browser/tests/shared/aps-puc-lifecycle.spec.ts`
- Test: `crates/trusted-server-integration-tests/browser/tests/nextjs/navigation.spec.ts`

- [ ] **Step 1: Add failing cycle-retirement tests.** The earliest sole GPT
      `slotRequested` listener classifies origin before later listeners. Publisher,
      competing, or ambiguous cycles synchronously retire the old overlay before the
      callback returns. An exactly attributable TS replacement retains the old
      artifact until the new one commits; a failed replacement keeps it visible.

- [ ] **Step 2: Add failing destruction tests.** Wrap publisher-originated
      `destroySlots` without changing receiver, arguments, order, return, or throw.
      Snapshot exact live objects before forwarding and retire only after successful
      return; omitted args cover all live objects. False/throw keeps connected
      artifacts intact.

- [ ] **Step 3: Add failing DOM-integrity tests.** Disconnect/replacement of host,
      removal/reparenting of the exact overlay, navigation, or exact artifact
      replacement retires once. Remove the TS node wherever it moved, compare-restore
      only owned style/targeting, never traverse/remove publisher children, destroy
      publisher GPT objects, or repeat PUC settlement.

- [ ] **Step 4: Run RED, implement the shared exact-once latch, and rerun GREEN.**

  ```bash
  npm --prefix crates/trusted-server-js/lib test -- --run test/adapters/googletag.test.ts test/services/slots.test.ts test/services/render.test.ts test/integrations/gpt/module.test.ts test/integrations/gpt/later.test.ts test/first_display/gpt_adapter.test.ts test/first_display/render_bridge.test.ts
  ```

- [ ] **Step 5: Run browser refresh/navigation proof.**

  ```bash
  npm --prefix crates/trusted-server-integration-tests/browser test -- --project=chromium tests/shared/aps-puc-lifecycle.spec.ts tests/nextjs/navigation.spec.ts
  ```

- [ ] **Step 6: Commit.**

  ```bash
  git add crates/trusted-server-js/lib/src crates/trusted-server-js/lib/test crates/trusted-server-integration-tests/browser/tests/shared/aps-puc-lifecycle.spec.ts crates/trusted-server-integration-tests/browser/tests/nextjs/navigation.spec.ts
  git commit -m "Retire committed APS overlays exactly once"
  ```

## Phase 3 — Preserve rc behavior behind the final owners

### Task 10: Make creative-opportunity enablement an explicit hard-cutover switch

**Files:**

- Modify: `crates/trusted-server-core/src/creative_opportunities.rs`
- Modify: `crates/trusted-server-core/src/settings.rs`
- Modify: `crates/trusted-server-core/src/publisher.rs`
- Modify: `crates/trusted-server-core/src/auction/endpoints.rs`
- Modify: `trusted-server.example.toml`
- Modify: `docs/guide/configuration.md`
- Modify: inline configuration fixtures in the Rust owners above containing
  `[creative_opportunities]`; this worktree has no separate fixture file
- Test: colocated Rust tests

- [ ] **Step 1: Add failing config tests.** A present table requires an explicit
      boolean `enabled`; omission fails. Absence of the table remains valid. Disabled
      configuration still parses and validates every slot/cache/assembly field.

- [ ] **Step 2: Add failing behavior tests.** `enabled=false` suppresses publisher
      matching, initial projection, automatic auction, slot injection, SPA init, and
      page-bids delivery; page-bids returns the exact empty result. Direct
      `POST /auction` remains live under ordinary auction gates.

- [ ] **Step 3: Add failing cache-policy tests.** A successful inactive publisher
      HTML GET receives `max-age=60` unless origin cache control contains `private`
      or `no-store` case-insensitively. Preserve validators and surrogate/CDN policy
      on inactive output; keep private/no-store and validator stripping for injected
      per-navigation state. Non-HTML, failures, and direct auction are unchanged.

- [ ] **Step 4: Run RED, implement the required field/gates, update every fixture,
      update the official configuration guide and every fixture, and run GREEN.**
      The guide's active and inactive examples must state `enabled` explicitly and
      must not describe an empty slot list as the implicit enablement switch.

  ```bash
  cargo test --package trusted-server-core --target "$(rustc -vV | sed -n 's/^host: //p')" creative_opportunities
  cargo test --package trusted-server-core --target "$(rustc -vV | sed -n 's/^host: //p')" publisher
  cargo test --package trusted-server-core --target "$(rustc -vV | sed -n 's/^host: //p')" auction::endpoints
  cargo test-fastly creative_opportunities
  cargo fmt --all -- --check
  ```

- [ ] **Step 5: Commit.**

  ```bash
  git add crates/trusted-server-core/src/creative_opportunities.rs crates/trusted-server-core/src/settings.rs crates/trusted-server-core/src/publisher.rs crates/trusted-server-core/src/auction/endpoints.rs trusted-server.example.toml docs/guide/configuration.md
  git commit -m "Require explicit creative opportunity enablement"
  ```

### Task 11: Preserve GPT diagnostics as a self-contained public contract

**Files:**

- Modify: `crates/trusted-server-js/lib/src/core/types.ts`
- Modify: `crates/trusted-server-js/lib/src/integrations/gpt/diagnostics_facts.ts`
- Modify: `crates/trusted-server-js/lib/src/integrations/gpt_diagnostics/{api,data_api,store,slot_size_observer,badges,overlay,module}.ts`
- Modify: `crates/trusted-server-core/src/integrations/gpt_diagnostics.rs`
- Modify: `crates/trusted-server-core/src/publisher.rs`
- Modify: `docs/guide/integrations/gpt-diagnostics.md`
- Test: `crates/trusted-server-js/lib/test/integrations/gpt_diagnostics/`
- Test: `crates/trusted-server-integration-tests/browser/tests/nextjs/gpt-diagnostics.spec.ts`

- [ ] **Step 1: Add failing public-type and data tests.** The design file is the
      complete authority; public types do not import an older diagnostics spec.
      Preserve exact callbacks, binding, request path, identities, response class,
      issues, counters, privacy, caps, copied/frozen exports, and asynchronous
      one-entry latest-snapshot notifications.

- [ ] **Step 2: Add failing three-size tests.** Keep
      `requestedSlotSizes` (copied/frozen, max 16 integer 1..4096 tuples), GPT
      `size`, and fractional/nonnegative `observedSlotSize` distinct in store,
      snapshot, badges, overlay, and export. Publisher/native requests never inherit
      stale requested evidence. Cover refresh/rebind/navigation/observer/disposal
      races and no-ResizeObserver fallback.

- [ ] **Step 3: Add failing `ts_console` session tests.** Exact case-sensitive
      `1|true` enables and `0|false` disables on eligible GET navigation; duplicates
      or unknown values fail closed. Strip the reserved directive before publisher
      handling, keep the HttpOnly host-only session, and expose no query/referrer/
      cookies/targeting/creative data.

- [ ] **Step 4: Run RED, implement demonstrated gaps, and run GREEN.**

  ```bash
  npm --prefix crates/trusted-server-js/lib test -- --run test/integrations/gpt_diagnostics test/integrations/gpt/diagnostics_facts.test.ts
  cargo test --package trusted-server-core --target "$(rustc -vV | sed -n 's/^host: //p')" integrations::gpt_diagnostics
  cargo test --package trusted-server-core --target "$(rustc -vV | sed -n 's/^host: //p')" publisher
  npm --prefix crates/trusted-server-integration-tests/browser test -- --project=chromium tests/nextjs/gpt-diagnostics.spec.ts
  ```

- [ ] **Step 5: Commit only if production changed.**

  ```bash
  git add crates/trusted-server-js/lib/src/core/types.ts crates/trusted-server-js/lib/src/integrations/gpt crates/trusted-server-js/lib/src/integrations/gpt_diagnostics crates/trusted-server-js/lib/test/integrations/gpt crates/trusted-server-js/lib/test/integrations/gpt_diagnostics crates/trusted-server-core/src/integrations/gpt_diagnostics.rs crates/trusted-server-core/src/publisher.rs crates/trusted-server-integration-tests/browser/tests/nextjs/gpt-diagnostics.spec.ts docs/guide/integrations/gpt-diagnostics.md
  git commit -m "Preserve GPT diagnostics through the runtime cutover"
  ```

### Task 12: Move GAM attribution into the sole GPT owner

**Files:**

- Modify: `crates/trusted-server-core/src/integrations/gpt.rs`
- Modify: `crates/trusted-server-core/src/tsjs.rs`
- Modify: `crates/trusted-server-js/lib/src/integrations/gpt/module.ts`
- Modify: `crates/trusted-server-js/lib/src/first_display/slices/gpt.ts`
- Delete obsolete raw-bootstrap transport from `crates/trusted-server-core/src/publisher.rs` if present
- Test: `crates/trusted-server-js/lib/test/integrations/gpt/module.test.ts`
- Test: `crates/trusted-server-js/lib/test/first_display/slices.test.ts`
- Test: colocated Rust tests

- [ ] **Step 1: Add failing order/idempotence tests.** The typed GPT boot config
      carries `gamAttributionEnabled`, default false. Enabled pages enqueue exactly
      one `googletag.setConfig({targeting:{ts:'true'}})` in parser-time activation
      before publisher GPT commands, including preexisting `adInit`. No-agent uses
      synchronous activation; agent takeover adopts the already-installed obligation
      without a second call.

- [ ] **Step 2: Add failing lifecycle tests.** Attribution persists through initial,
      lazy, publisher-owned, refresh, Prebid refresh, and SPA requests. Neither
      refresh nor cleanup clears page-level `ts`; targeting API absence/throw is
      isolated. Disabled mode has zero side effects.

- [ ] **Step 3: Run RED, implement one owner, and delete the raw global flag,
      activation attribute, and duplicate fallback path.**

  ```bash
  cargo test --package trusted-server-core --target "$(rustc -vV | sed -n 's/^host: //p')" integrations::gpt::tests
  npm --prefix crates/trusted-server-js/lib test -- --run test/integrations/gpt/module.test.ts test/first_display/slices.test.ts test/integrations/prebid/later.test.ts
  ```

- [ ] **Step 4: Commit.**

  ```bash
  git add crates/trusted-server-core/src/integrations/gpt.rs crates/trusted-server-core/src/tsjs.rs crates/trusted-server-core/src/publisher.rs crates/trusted-server-js/lib/src/integrations/gpt/module.ts crates/trusted-server-js/lib/src/first_display/slices/gpt.ts crates/trusted-server-js/lib/test
  git commit -m "Make GPT own parser-time GAM attribution"
  ```

### Task 13: Lock the remaining rc GPT, Prebid, cache, and integration behavior

**Files:**

- Modify only for demonstrated gaps: integration modules and focused tests under
  `crates/trusted-server-js/lib/src/integrations/` and
  `crates/trusted-server-js/lib/test/integrations/`
- Test: `crates/trusted-server-js/lib/test/integrations/gpt/module.test.ts`
- Test: `crates/trusted-server-js/lib/test/integrations/prebid/{module,later}.test.ts`
- Test: all remaining integration suites

- [ ] **Step 1: Add or run the named RCJ-GPT-04 collapsed-shell unit step.** Resize
      only the exact authenticated, connected, ordinary collapsed iframe after a
      successful TS PUC response. Do not require same-origin access: the source
      identity, width/height attributes, computed dimensions, current attempt, and
      ordinary non-fixed/non-sticky shell checks are the authority. Reject anchor/
      fixed/sticky/disconnected/already-expanded/unrelated frames and preserve
      publisher styles outside the narrow guard.

- [ ] **Step 2: Add or run the RCJ-PREBID-04 focused step.** Rebuild the Prebid
      refresh GAM-path exclusion in `prebid_later`; prove no initial-admission
      ownership, no native-bid interference, and no duplicate refresh.

- [ ] **Step 3: Preserve PBS Cache as a black box.** Run exact rc request, parse,
      macro, PUC response, failure reason, and collapsed-resize fixtures. The generic
      mutable `setConfig/getConfig` runtime API stays deleted by the hard cutover;
      cache parity must not retain or reintroduce it. Do not route cache IDs into
      APS/ADM reservations or add new cache policy, deadline, schema, or endpoint.

- [ ] **Step 4: Run the complete remaining-integration parity suite.** Cover
      DataDome, Didomi, GTM, Lockr, Osano, Permutive, Sourcepoint, Testlight, Creative,
      disposal, maximal catalog order, and throw isolation.

  ```bash
  npm --prefix crates/trusted-server-js/lib test -- --run test/integrations
  npm --prefix crates/trusted-server-js/lib run typecheck
  npm --prefix crates/trusted-server-js/lib run lint
  ```

- [ ] **Step 5: Commit only demonstrated gaps.** Use a sentence-case subject naming
      the behavior actually changed; do not create an empty parity commit.

## Phase 4 — Browser, security, build, and release evidence

### Task 14: Prove the four-level APS browser contract

**Files:**

- Modify: `crates/trusted-server-integration-tests/browser/fixtures/fictional-aps-runner.js`
- Modify: `crates/trusted-server-integration-tests/browser/helpers/{gpt-stub,gam-test-network,tsjs-fixture}.ts`
- Modify: `crates/trusted-server-integration-tests/browser/tests/shared/{aps-renderer,aps-puc-lifecycle,tsjs-runtime}.spec.ts`
- Modify: `crates/trusted-server-integration-tests/browser/tests/nextjs/navigation.spec.ts`
- Modify: `docs/guide/integrations/aps.md`

- [ ] **Step 1: Extend the locally authored fictional harness.** Implement only the
      public queue/resolve/reject behavior; never copy or store vendor bytes. Expose
      the exact PUC release value `1.17.2` in protected test API metadata without
      vendoring PUC.

- [ ] **Step 2: Add Chromium/Firefox/WebKit containment tests.** Assert top slot ->
      bootstrap/outer mount -> inner renderer -> descendant creative ancestry,
      independent nonces, opaque data origins, exact sandbox/CSP pairs, no publisher
      regain, no PUC/GAM/SafeFrame ancestor, exact ports/sources, and no descriptor in
      bootstrap/outer/PUC payloads.

- [ ] **Step 3: Add the required script-creative enforcement cases in every
      browser.** Prove an iframe-policy document refuses external creative script,
      an explicitly enabled same-origin script creative executes under the exact
      script policy, and a cross-origin redirect or final script origin is refused.
      Keep script creatives default-off and verify that enabling one origin never
      broadens iframe creatives or another origin.

- [ ] **Step 4: Add layout and visibility tests.** At every boundary fixture size,
      all four levels have exact width/height/client/scroll dimensions, zero default
      margins/overflow, and no clipping. The overlay stays hidden through acceptance
      and runner load and becomes visible only on completion.

- [ ] **Step 5: Add adversarial lifecycle tests.** Cover wrong source/origin/nonce/
      port, replay, duplicate keys, malformed UTF-8 boundaries, timeout, runner
      silence/reject/load-only, abort, navigation, host replacement, node movement,
      publisher/competing/ambiguous cycles, successful/failed destroy, TS replacement
      success/failure, and exact-once settlement/disposal.

- [ ] **Step 6: Run the hermetic browser matrix.**

  ```bash
  npm --prefix crates/trusted-server-integration-tests/browser test -- tests/shared/aps-renderer.spec.ts tests/shared/aps-puc-lifecycle.spec.ts tests/shared/tsjs-runtime.spec.ts tests/nextjs/navigation.spec.ts --project=chromium --project=firefox --project=webkit
  ```

- [ ] **Step 7: Update operator docs.** State that APS browser routes are anonymous
      reserved routes, platform shielding owns rate limits, runner bytes are live/
      unversioned/unvendored/unpinned/uncached by Trusted Server, successful proxy
      responses have no TS cache promise, APS can be disabled for containment, and
      PUC 1.17.2 is configured outside this repository.

- [ ] **Step 8: Commit.**

  ```bash
  git add crates/trusted-server-integration-tests/browser docs/guide/integrations/aps.md
  git commit -m "Prove APS top-mount containment in browsers"
  ```

### Task 15: Retarget bundle and performance gates to the exact rc PR base

**Files:**

- Modify: `crates/trusted-server-js/lib/scripts/check-bundle-budgets.mjs`
- Modify: `crates/trusted-server-js/lib/test/build/release-v1.test.mjs`
- Modify: `crates/trusted-server-js/lib/test/fixtures/performance/aps-tsjs-prechange.json` only by appending the new candidate layer; never rewrite historical subtrees
- Modify: `scripts/validate-tsjs-performance-evidence.mjs`
- Modify: `scripts/ci/tsjs-performance.sh`
- Modify: `scripts/ci/aps-tsjs-evidence.mjs`
- Create: `scripts/ci/dispatch-aps-tsjs-gate.mjs`
- Create: `scripts/ci/dispatch-aps-tsjs-gate.test.mjs`
- Modify: `crates/trusted-server-integration-tests/browser/tests/shared/tsjs-performance.spec.ts`
- Modify: `crates/trusted-server-integration-tests/browser/playwright.performance.config.ts`
- Modify: `.github/workflows/tsjs-performance-gate.yml`
- Modify: `.github/workflows/test.yml`

- [ ] **Step 1: Add failing schema/base tests.** Replace moving-main inputs with an
      exact `baseSha`/`baseline` identity. On PRs use `pull_request.base.sha`; manual
      and called runs require the same explicit input. Verify the commit is 40 lower-
      hex characters and reachable from fetched `origin/rc/202608`. Never derive a
      fallback from candidate HEAD or a moving branch name.
      Add a required `base_sha` input to manual/called workflow entrypoints and bind
      PR runs directly to `pull_request.base.sha` as `TSJS_PERF_BASE_SHA`. Bind
      `TSJS_PERF_HEAD_SHA` to `pull_request.head.sha`; neither writer nor validator
      may read `GITHUB_SHA`, which names the synthetic PR merge commit.

- [ ] **Step 2: Add failing legacy/phase-aware baseline-loader tests.** A detached
      worktree builds the real artifact model at the exact base: legacy core+
      creative+GPT where applicable, or release-v1 controller/artifacts after
      cutover. Compare the same semantic first-action interval without demanding
      candidate-only metadata from a legacy base.

- [ ] **Step 3: Enforce all absolute and relative gates.** Keep immutable historical
      and remediation captures report-only. Pin these independent raw/gzip/Brotli
      ceilings exactly: bootstrap `48,000/16,000/14,000`; every permitted
      first-display mask `90,000/30,000/26,000`; persistent reference
      `524,288/163,840/131,072`; maximal non-bootstrap total
      `1,048,576/327,680/262,144`. Require and name the minimal
      `[first_display]`, GPT reference
      `[first_display,creative_initial,gpt_initial,prebid_initial,datadome_initial]`,
      and APS
      `[first_display,render_owner_initial,aps_initial,creative_initial,gpt_initial]`
      masks, plus the largest admitted mask per encoding. Enforce GPT reference
      semantic transfer no-growth in every encoding. Separately require both GPT and
      APS candidate first-action p90 and pre-action raw/gzip/Brotli transfer to be at
      most the exact rc base × 1.10.

      Pin the APS absolute table: bids-script→first GPT action p90 ≤900 ms; first
      action→accepted completion p90 ≤1,500 ms; accepted completion→paint p90 ≤250
      ms; bids-script→paint p90 ≤2,500 ms; forced-GC post-paint used size ≤3,145,728
      bytes; post-takeover/queue-drain used size ≤3,932,160 bytes. Both variants in
      the paired GPT case remain below 4,194,304 bytes at boot, first render, refresh,
      and SPA navigation. Require real User Timing marks and no persistent/deferred request,
      preload, prepare, or execution before paint.
      The production source graph must prove that neither first-display nor
      persistent artifacts reach `shared/aps_documents.ts` or the document-form ES5
      validator; those authorities exist only in the generator and contract tests.
      The mandatory APS + creative + GPT first-display mask must pass the unchanged
      90,000/30,000/26,000 raw/gzip/Brotli ceilings.

- [ ] **Step 4: Keep CI logic in scripts.** Workflow YAML may set immutable inputs,
      install toolchains, and invoke checked-in scripts. Put Git ancestry validation,
      worktree creation, build selection, sampling, and evidence validation in
      `.sh`/`.mjs` files. Rename step labels from current-main to rc baseline.

      Add a checked-in operator dispatch helper with exact `performance` and
      `real-gam` subcommands. Before dispatch it requires a clean worktree and proves
      the named remote branch resolves to local HEAD. It generates a unique bounded
      evidence id from gate kind, HEAD, UTC time, and random suffix; records the
      dispatch start; invokes `gh workflow run`; polls for exactly one run matching
      workflow, exact evidence id, exact head SHA, and creation time; watches that
      run; downloads the exact expected artifact; and invokes the owning validator.
      `real-gam` derives `release_id` from the clean local artifact and requires only
      the operator-owned rollback artifact id locally. The protected workflow—not
      local shell—compares that release id with its environment-owned expected value.
      Unit-test command construction, uniqueness, zero/multiple/stale-run rejection,
      remote-HEAD mismatch, artifact naming, and validator invocation with an
      injected fake command runner.

      The performance job is fixed to Chromium `145.0.7632.6`, the
      `github-hosted:ubuntu-24.04` runner class,
      `tsjs-baseline-paired-network-v3` (150 ms, 200,000 bytes/s down, 93,750 bytes/s
      up, zero loss), five warmups and 50 measured samples per variant, alternating
      baseline/candidate order, a 40-minute browser budget, a 50-minute job budget,
      exactly one post-sample candidate lifecycle observation, and separate paired
      heap contexts. Add focused validator tests for every literal and reject an
      environment override.

- [ ] **Step 5: Run local self-tests and prove the intentional pre-remediation
      RED.** Validator/schema/dispatch unit tests and the build must be GREEN. The
      exact semantic-transfer and APS admission checks against the untouched
      candidate are expected RED for the recorded revision-43 bytes/timing; preserve
      that failure as the gate contract. Do not weaken membership, a threshold, or a
      fixture to make Task 15 green. Tasks 15A/15B are the remediation, and Task 15B
      Step 7 is the first required full acceptance GREEN.

  ```bash
  node scripts/validate-tsjs-performance-evidence.mjs --self-test
  node --test scripts/ci/dispatch-aps-tsjs-gate.test.mjs
  npm --prefix crates/trusted-server-js/lib run build
  npm --prefix crates/trusted-server-js/lib run check:bundle
  npm --prefix crates/trusted-server-js/lib run test:release
  npm --prefix crates/trusted-server-js/lib run check:architecture
  ```

- [ ] **Step 6: Commit the gate and dispatch contract before any remote run.**

  ```bash
  git add crates/trusted-server-js/lib/scripts/check-bundle-budgets.mjs crates/trusted-server-js/lib/test/build/release-v1.test.mjs crates/trusted-server-js/lib/test/fixtures/performance/aps-tsjs-prechange.json scripts/validate-tsjs-performance-evidence.mjs scripts/ci/tsjs-performance.sh scripts/ci/aps-tsjs-evidence.mjs scripts/ci/dispatch-aps-tsjs-gate.mjs scripts/ci/dispatch-aps-tsjs-gate.test.mjs crates/trusted-server-integration-tests/browser/tests/shared/tsjs-performance.spec.ts crates/trusted-server-integration-tests/browser/playwright.performance.config.ts .github/workflows/tsjs-performance-gate.yml .github/workflows/test.yml
  git commit -m "Compare TSJS performance with the exact rc base"
  ```

### Task 15A: Replace the production boot object graph with a sealed JSON transport

**Files:**

- Create: `crates/trusted-server-js/lib/src/core/contracts/server_boot_transport.ts`
- Modify: `crates/trusted-server-core/src/tsjs.rs`
- Modify: `crates/trusted-server-js/lib/src/core/bootstrap.ts`
- Modify: `crates/trusted-server-js/lib/src/core/contracts/boot.ts`
- Modify: `crates/trusted-server-js/lib/src/kernel/runtime.ts`
- Modify: `crates/trusted-server-js/lib/src/kernel/release_catalog.ts`
- Modify: `crates/trusted-server-js/lib/src/shared/first_display_handoff.ts`
- Modify: `crates/trusted-server-js/lib/src/shared/takeover.ts`
- Modify: `crates/trusted-server-js/lib/build-all.mjs`
- Modify: `crates/trusted-server-js/lib/scripts/check-bundle-budgets.mjs`
- Test: `crates/trusted-server-js/lib/test/core/bootstrap.test.ts`
- Test: `crates/trusted-server-js/lib/test/kernel/runtime.test.ts`
- Test: `crates/trusted-server-js/lib/test/first_display/handoff.test.ts`
- Test: `crates/trusted-server-js/lib/test/first_display/takeover.test.ts`
- Test: `crates/trusted-server-js/lib/test/build/generated-fallback.test.mjs`
- Test: `crates/trusted-server-js/lib/test/build/release-v1.test.mjs`
- Test: colocated `crates/trusted-server-core/src/tsjs.rs` tests

- [ ] **Step 1: Add the RED production-transport tests.** Require Rust to emit one
      inline lexical JSON string containing exact
      `{version:1,boot,integrity,outline}` and no
      object-form `__TSJS_SERVER_BOOT_INPUT_V1__`, raw boot global, compatibility
      alias, or executable source value. Parse the emitted fragment in the test and
      prove exact release, manifest, projection, carrier, creative, diagnostics,
      always-present boot-integrity digests, the exact
      `outline:null | TakeoverOutlineV1` union and non-null equality, escaping, and 10
      MiB decoded-size behavior.

- [ ] **Step 2: Add the RED compact-parser and graph tests.** Define the wished-for
      `snapshotServerBootTransportV1(payload, releaseId)` contract. A canonical valid
      string returns one recursively frozen fresh snapshot; malformed JSON, wrong or
      extra root keys, release mismatch, bad first-display URL/mask/slices, bad
      runtime URL, missing/malformed boot integrity, outline parity/count/digest form
      or integrity mismatch, invalid dedicated booleans, or an oversized string
      returns `undefined` before effects. A direct-runtime snapshot has
      `outline:null` but retains the integrity value. Add RED runtime/handoff tests
      proving direct boot recomputes both digests, and takeover requires recomputed
      boot = integrity = outline = handoff. Projection and integration-config
      mismatches independently produce `abi_mismatch` before preparation,
      activation, or publication; a valid carrier succeeds on both paths. The release metafile
      must prove `bootstrap` no longer reaches `core/contracts/boot.ts`,
      `core/contracts/auction_projection.ts`, or
      `core/contracts/integration_configs.ts`.

      The generated fallback tests require the exact safe manifest, empty
      `integrations:{version:1,entries:[]}`, empty fallback auction projection,
      disabled creative/diagnostics defaults, omitted `requestAds()` → `{slots:[]}`,
      explicit valid id → `slot_unresolved`, already-aborted explicit id →
      `caller_aborted`, same frozen reason on `addAdUnits` refusal/internal state,
      both `initialDisplayCommitted` values, exact FIFO queue drain, accepted-DOM
      preservation, no pending call, no artifact retry, and both
      `abi_mismatch`/`bundle_partial` classifications. Fallback never needs the full
      projection validator.

- [ ] **Step 3: Run RED and inspect the expected failures.**

  ```bash
  cargo test --package trusted-server-core --target "$(rustc -vV | sed -n 's/^host: //p')" tsjs::tests
  npm --prefix crates/trusted-server-js/lib test -- --run test/core/bootstrap.test.ts test/kernel/runtime.test.ts test/first_display/handoff.test.ts test/first_display/takeover.test.ts
  node --test crates/trusted-server-js/lib/test/build/generated-fallback.test.mjs
  npm --prefix crates/trusted-server-js/lib run test:release
  ```

  Expected: the Rust shape still emits the object graph, the compact parser is
  absent, and the bootstrap metafile still reaches the full validators.

- [ ] **Step 4: Implement the minimal sealed transport.** Canonicalize and validate
      in Rust exactly as today, serialize the payload once, escape it as a JS string,
      and keep `window.tsjs` target acquisition outside the JSON. In the compact
      parser, use the parsed fresh tree, perform only revision-43 critical
      release/manifest/integrity/outline checks, recursively freeze, and return one
      snapshot.
      Do not accept object form or move executable code into the payload.

- [ ] **Step 5: Rewire only the production bootstrap.** Import the compact parser,
      replace the generic config lookup with an exact scan over the already-frozen
      ordered entries, and retain the full `snapshotTsjsBootV1` validators for core,
      public, and test boundaries. First-display batch and slice validators remain
      authoritative before their effects. Persistent core recomputes the canonical
      projection and integration-config SHA-256 digests before direct use or handoff
      adoption and compares them with the always-present integrity value; the
      controller validates only exact digest form and outline/integrity relations.

- [ ] **Step 6: Run GREEN and adjacent hard-cutover checks.**

  ```bash
  cargo test --package trusted-server-core --target "$(rustc -vV | sed -n 's/^host: //p')" tsjs::tests
  npm --prefix crates/trusted-server-js/lib test -- --run test/core/bootstrap.test.ts test/kernel/runtime.test.ts test/first_display
  npm --prefix crates/trusted-server-js/lib run typecheck
  npm --prefix crates/trusted-server-js/lib run lint
  npm --prefix crates/trusted-server-js/lib run build
  npm --prefix crates/trusted-server-js/lib run test:release
  npm --prefix crates/trusted-server-js/lib run check:hard-cutover-absence
  ```

- [ ] **Step 7: Measure rather than infer transfer improvement.** Read the generated
      bootstrap raw/gzip/Brotli values and its complete source-owner list. If the
      graph exclusions pass but the served GPT semantic interval cannot fit beneath
      the exact rc baseline after including its real inline payload, stop and revisit
      the architecture instead of adding mangling or changing membership.

- [ ] **Step 8: Commit.**

  ```bash
  git add crates/trusted-server-core/src/tsjs.rs crates/trusted-server-js/lib/src/core/bootstrap.ts crates/trusted-server-js/lib/src/core/contracts/server_boot_transport.ts crates/trusted-server-js/lib/src/core/contracts/boot.ts crates/trusted-server-js/lib/src/kernel/runtime.ts crates/trusted-server-js/lib/src/kernel/release_catalog.ts crates/trusted-server-js/lib/src/shared/first_display_handoff.ts crates/trusted-server-js/lib/src/shared/takeover.ts crates/trusted-server-js/lib/build-all.mjs crates/trusted-server-js/lib/scripts/check-bundle-budgets.mjs crates/trusted-server-js/lib/test/core/bootstrap.test.ts crates/trusted-server-js/lib/test/kernel/runtime.test.ts crates/trusted-server-js/lib/test/first_display/handoff.test.ts crates/trusted-server-js/lib/test/first_display/takeover.test.ts crates/trusted-server-js/lib/test/build/generated-fallback.test.mjs crates/trusted-server-js/lib/test/build/release-v1.test.mjs
  git commit -m "Seal the production TSJS boot transport"
  ```

### Task 15B: Add the shared render-owner slice and thin the APS slice

**Files:**

- Create: `crates/trusted-server-js/lib/src/first_display/render_journal.ts`
- Create: `crates/trusted-server-js/lib/src/first_display/slices/render_owner.ts`
- Modify: `crates/trusted-server-core/src/tsjs.rs`
- Modify: `crates/trusted-server-js/lib/src/first_display/agent.ts`
- Modify: `crates/trusted-server-js/lib/src/first_display/adm_render_bridge.ts`
- Modify: `crates/trusted-server-js/lib/src/first_display/render_bridge.ts`
- Modify: `crates/trusted-server-js/lib/src/first_display/leaf/aps_protocol.ts`
- Modify: `crates/trusted-server-js/lib/src/first_display/slices/aps.ts`
- Modify: `crates/trusted-server-js/lib/src/core/contracts/server_boot_transport.ts`
- Modify: `crates/trusted-server-js/lib/src/kernel/contracts/puc_dynamic_owner.ts`
- Modify: `crates/trusted-server-js/lib/src/kernel/release_catalog.ts`
- Modify: `crates/trusted-server-js/lib/src/shared/first_display_transaction.ts`
- Modify: `crates/trusted-server-js/lib/src/shared/first_display_registration.ts`
- Modify: `crates/trusted-server-js/lib/src/shared/first_display_contracts.ts`
- Modify: `crates/trusted-server-js/lib/build-all.mjs`
- Modify: `crates/trusted-server-js/lib/scripts/check-bundle-budgets.mjs`
- Test: `crates/trusted-server-js/lib/test/first_display/{agent,adm_render_bridge,render_bridge,slices}.test.ts`
- Test: `crates/trusted-server-js/lib/test/first_display/transaction.test.ts`
- Test: `crates/trusted-server-js/lib/test/first_display/contracts.test.ts`
- Test: `crates/trusted-server-js/lib/test/kernel/release_catalog.test.ts`
- Test: `crates/trusted-server-js/lib/test/services/puc_bridge.test.ts`
- Test: `crates/trusted-server-js/lib/test/build/release-v1.test.mjs`
- Test: `crates/trusted-server-integration-tests/browser/tests/shared/{aps-renderer,aps-puc-lifecycle,tsjs-runtime}.spec.ts`

- [ ] **Step 1: Add RED catalog, selection, and source-ownership tests.** Add
      `render_owner_initial` as catalog order 2 and shift the remaining optional
      slices by one. Enumerate exactly 3,584 reachable 14-bit masks. Select the slice
      exactly for an ADM or APS reservation; require it for `aps_initial`; omit it
      from no-bid/nonrendering masks. It owns one frozen source-neutral render-journal
      interface for attempt collection, terminal latching, reservation/ticket state,
      v4 PUC owner control, committed-artifact identity, handoff capture, detachment,
      and exact-once retirement. `aps_initial` composes that interface and has no
      second full journal/bridge implementation. The only APS-specific branch admitted
      to the shared owner is the exact authority-free informational
      `TS APS Top Mount Started` control message after APS kernel/top-mount start;
      non-APS masks cannot reach APS descriptors, URLs, nonce registries,
      renderer-document parsing, mount code, or top-mount policy. ADM-only masks still
      receive the self-contained v4 owner. Assert exact catalog order 1–14 and the
      implications APS ⇒ render owner ⇒ GPT, ADM ⇒ render owner without APS, and
      no-bid/nonrendering ⇒ no render owner.

- [ ] **Step 2: Add RED parity tests before extracting code.** Run the same journal
      corpus against ADM-only, APS-only, and mixed APS/ADM batches. Cover accepted,
      failed, cancelled, empty-GAM fallback, replacement success/failure, moved or
      mutated nodes, navigation, handoff, post-detach reentrancy, overlay-lease
      transfer, targeting restoration, and final retirement. The new tests must fail
      only because a shared base journal is not yet exposed.

- [ ] **Step 3: Add RED compact-owner tests.** Keep the exact successful/refused v4
      outer shapes and the complete current adversarial corpus, while requiring the
      checked-in dynamic owner to remain self-contained and contain no `fetch`,
      dynamic import, script insertion, blob/worker loader, URL, descriptor, or APS
      DOM authority. Exercise registration timeout/refusal/replay, port errors,
      nested publisher callbacks, ADM insertion, APS informational start, settlement,
      and Promise behavior through the actual serialized program. The release/static-
      route test must also prove `render_owner_initial` is concatenated into the one
      served mask body, has no independently routable/requested production asset, and
      can create no script, preload, fetch, dynamic-import, worker, or blob request
      before paint.

- [ ] **Step 4: Run RED.**

  ```bash
  npm --prefix crates/trusted-server-js/lib test -- --run test/first_display/agent.test.ts test/first_display/adm_render_bridge.test.ts test/first_display/render_bridge.test.ts test/first_display/slices.test.ts test/first_display/transaction.test.ts test/first_display/contracts.test.ts test/kernel/release_catalog.test.ts test/services/puc_bridge.test.ts test/build/release-v1.test.mjs
  ```

- [ ] **Step 5: Extract one render-owner slice and journal.** Replace—not wrap—the
      common ADM/APS maps, reservation/ticket state, v4 PUC dispatcher/owner control,
      terminal latch, committed artifact journal, mutation notification, handoff/
      detach state, timers, and retirement bookkeeping. Expose only the exact frozen
      operations the APS protocol needs through the release-private slice host. Keep
      APS descriptor parsing, nonce registries, document parser, mount policy, and
      overlay behavior in `aps_initial`.

- [ ] **Step 6: Compact the dynamic owner in place.** Deduplicate its exact record,
      event/port, timer, and terminal helpers without removing checks or changing a
      message. Preserve the single self-contained `renderer` string and the existing
      64 KiB/72 KiB caps; do not load another asset and do not rely on cross-artifact
      property mangling.

- [ ] **Step 7: Run GREEN, build, and inspect the actual APS mask.**

  ```bash
  npm --prefix crates/trusted-server-js/lib test -- --run test/first_display test/services/puc_bridge.test.ts test/integrations/aps test/services/render.test.ts test/build/release-v1.test.mjs
  npm --prefix crates/trusted-server-js/lib run typecheck
  npm --prefix crates/trusted-server-js/lib run lint
  npm --prefix crates/trusted-server-js/lib run build
  npm --prefix crates/trusted-server-js/lib run check:bundle
  node scripts/validate-tsjs-performance-evidence.mjs --self-test
  node --test scripts/ci/dispatch-aps-tsjs-gate.test.mjs
  npm --prefix crates/trusted-server-js/lib run test:release
  npm --prefix crates/trusted-server-js/lib run check:architecture
  ```

  Expected: every focused behavior is unchanged, the production graph has one
  render journal, and the exact mandatory APS mask remains admitted. If the combined
  inline-plus-mask semantic bytes still exceed the rc × 1.10 allowance, stop and
  revisit the approved split rather than weakening a gate.

- [ ] **Step 8: Run the three-browser focused lifecycle proof.** In
      `tsjs-runtime.spec.ts`, assert exactly one parser-blocking first-display TSJS
      request before paint, no separately routed render-owner asset, and no script,
      preload, fetch, dynamic-import, worker, or blob request from that owner.

  ```bash
  npm --prefix crates/trusted-server-integration-tests/browser test -- tests/shared/aps-renderer.spec.ts tests/shared/aps-puc-lifecycle.spec.ts tests/shared/tsjs-runtime.spec.ts --project=chromium --project=firefox --project=webkit
  ```

- [ ] **Step 9: Commit.**

  ```bash
  git add crates/trusted-server-core/src/tsjs.rs crates/trusted-server-js/lib/src/first_display crates/trusted-server-js/lib/src/core/contracts/server_boot_transport.ts crates/trusted-server-js/lib/src/kernel/contracts/puc_dynamic_owner.ts crates/trusted-server-js/lib/src/kernel/release_catalog.ts crates/trusted-server-js/lib/src/shared/first_display_transaction.ts crates/trusted-server-js/lib/src/shared/first_display_registration.ts crates/trusted-server-js/lib/src/shared/first_display_contracts.ts crates/trusted-server-js/lib/build-all.mjs crates/trusted-server-js/lib/scripts/check-bundle-budgets.mjs crates/trusted-server-js/lib/test/first_display crates/trusted-server-js/lib/test/kernel/release_catalog.test.ts crates/trusted-server-js/lib/test/services/puc_bridge.test.ts crates/trusted-server-js/lib/test/build/release-v1.test.mjs crates/trusted-server-integration-tests/browser/tests/shared/aps-renderer.spec.ts crates/trusted-server-integration-tests/browser/tests/shared/aps-puc-lifecycle.spec.ts crates/trusted-server-integration-tests/browser/tests/shared/tsjs-runtime.spec.ts
  git commit -m "Thin the APS first-display owner"
  ```

### Task 16: Enforce hard-cutover absence and supply-chain boundaries

**Files:**

- Modify: `crates/trusted-server-js/lib/scripts/check-hard-cutover-absence.mjs`
- Modify: `crates/trusted-server-js/lib/test/eslint/no-adtech-globals.test.mjs`
- Modify: `crates/trusted-server-js/lib/eslint-rules/no-adtech-globals.js`
- Modify: `crates/trusted-server-js/lib/test/build/release-v1.test.mjs`
- Modify: `scripts/ci/aps-tsjs-quality.sh`
- Modify: `.github/workflows/test.yml`

- [ ] **Step 1: Add failing absence tests.** Reject old renderer routes/version,
      `rendererUrl` in v4 messages, old APS Start payloads, raw integration globals,
      activation attributes, `TsjsApiV1`, compatibility aliases, second runtimes,
      old public render/config APIs, and legacy globals/event names named in design
      section 5.4.

- [ ] **Step 2: Add failing vendor-byte scans.** Reject APS runner, GPT, or PUC
      bytes, checksums, distributable artifacts, or stored vendor bodies in source,
      dist, fixtures, or evidence. Permit only the required protected conformance
      metadata `pucRelease: '1.17.2'`; it is not vendored code or a checksum. Keep the
      pure lockfile-pinned Prebid artifact isolated and verify it contains no
      auction/render behavior owned by TSJS.

- [ ] **Step 3: Document static-analysis blind spots.** Note that dynamically
      computed global keys and function-returned roots are not fully visible to the
      ESLint rule; retain bundle/source scans and browser ownership tests as the
      defense-in-depth layers.

- [ ] **Step 4: Run RED, implement scripts/rules, and run GREEN.**

  ```bash
  npm --prefix crates/trusted-server-js/lib run check:hard-cutover-absence
  npm --prefix crates/trusted-server-js/lib run test:architecture
  npm --prefix crates/trusted-server-js/lib run lint
  npm --prefix crates/trusted-server-js/lib run test:release
  bash scripts/ci/aps-tsjs-quality.sh
  ```

- [ ] **Step 5: Commit.**

  ```bash
  git add crates/trusted-server-js/lib/scripts/check-hard-cutover-absence.mjs crates/trusted-server-js/lib/test/eslint/no-adtech-globals.test.mjs crates/trusted-server-js/lib/eslint-rules/no-adtech-globals.js crates/trusted-server-js/lib/test/build/release-v1.test.mjs scripts/ci/aps-tsjs-quality.sh .github/workflows/test.yml
  git commit -m "Enforce final TSJS cutover boundaries"
  ```

## Phase 5 — Final rc refresh, verification, and PR

### Task 17: Refresh `rc/202608` and rerun the overlap audit

**Files:**

- Modify only files conflicted by an advanced rc tip
- Modify: `crates/trusted-server-js/lib/test/fixtures/contracts/current-main-concept-audit.json` rc layer
- Modify: performance evidence inputs if the exact PR base changes

- [ ] **Step 1: Require a clean, pushed task boundary, then fetch the release branch.**

  ```bash
  test -z "$(git status --porcelain)"
  git fetch origin rc/202608
  git merge --no-ff --no-commit origin/rc/202608
  ```

  If already an ancestor, abort the empty merge and record the no-op. If advanced,
  resolve conflicts in favor of revision 43 where it explicitly supersedes rc and
  otherwise in favor of rc. Do not merge `main` separately.

- [ ] **Step 2: Produce an overlap inventory before committing.** For every
      conflict or rc change touching TSJS, APS, publisher HTML, diagnostics,
      integration config, build, or CI, name the final owner and focused test. Keep
      EdgeZero and other excluded features unchanged.

  Final refresh inventory for `d4cd2cc823718d64ae73bcb068e5eab03ecd901a`:

  | Rc overlap                                                   | Final disposition and owner                                                                                                                                                                                                                              | Focused proof                                                                                                        |
  | ------------------------------------------------------------ | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------- |
  | APS provider, renderer, browser types, and guide             | Revision 43 supersedes the publisher-native experiment. `integrations/aps.rs`, `integrations/aps/render.ts`, and the shared render services retain one TS-owned renderer and the no-second-auction invariant.                                            | APS Rust tests, `test/integrations/aps/render.test.ts`, GPT/PUC ownership tests, hard-cutover scan                   |
  | APS creative-frame scrollbar fix                             | Preserve the concept in the stronger proxy-owned documents: both document roots suppress overflow and the inner creative iframe is borderless, block-level, and full-size. No publisher-origin runner path is retained.                                  | APS document corpus, renderer unit suite, three-engine browser matrix                                                |
  | Initial GPT scheduling one-shot and handoff latch            | Supersede both mutable `scheduleInitialAdInit` implementations. The authenticated first-display registration transaction accepts one exact ordered batch, deletes registration ingress before activation, and transfers one frozen handoff exactly once. | first-display transaction, bootstrap, agent, takeover, and handoff suites                                            |
  | GPT diagnostics presentation review fixes                    | Preserve target-window scheduling, cancellable ownership, bounded badge size lists, shared size formatting, rounded observed boxes, destroyed-state guards, and record-currentness checks inside the data-only diagnostics owner.                        | GPT diagnostics binding, badge, overlay, slot-size, store, and public-type suites                                    |
  | Shared-template delivery of initial work                     | Preserve rc's reader-specific delivery result while replacing its legacy scheduler call with the immutable boot projection and first-display agent transaction.                                                                                          | publisher/template assembly byte-identity tests, boot contract tests, first-display projection tests                 |
  | Structural ad-template slot generation                       | Preserve rc's structural TOML update, contiguous provider tables, line-ending safeguards, and unmanaged-field check; add the required `enabled = true` only when the generator creates a hard-cutover section.                                           | focused `slot_toml` and CLI configuration suites                                                                     |
  | GAM cohort attribution                                       | Preserve the configuration and reporting concept; supersede rc raw globals and the activation attribute with the frozen GPT config carrier and sole parser-time GPT owner.                                                                               | GPT Rust config tests, `tsjs.rs` selection tests, first-display GPT adapter/slice tests, persistent GPT module tests |
  | GPT diagnostics correlation guide                            | Preserve the rc correlation evidence, rewritten to the immutable initial projection and navigation-internal page-bids projection.                                                                                                                        | GPT diagnostics store/API/data/overlay suites and docs format/build                                                  |
  | Retired GPT bootstrap, request runtime, and legacy GPT tests | Keep the revision-43 deletions; no second runtime or compatibility file returns.                                                                                                                                                                         | hard-cutover scan, architecture test, release graph test                                                             |
  | Docs TypeScript-ESLint update                                | Preserve rc package and lockfile ownership.                                                                                                                                                                                                              | docs install, lint, format, and build                                                                                |
  | GPT operator wording and example config                      | Preserve rc's provider-neutral environment-overlay wording; do not add EdgeZero feature work.                                                                                                                                                            | docs format/build and config tests                                                                                   |
  | Rc GAM review plan/spec artifacts                            | Preserve as release-branch documentation; they do not override revision 43's TSJS architecture.                                                                                                                                                          | docs format/build and plan-integrity scan                                                                            |

- [ ] **Step 3: Update every rc-baseline row and rerun its exact proof.** No stale
      SHA or prior pass satisfies the gate.

- [ ] **Step 4: Run concept, package, and focused overlap checks.**

  ```bash
  npm --prefix crates/trusted-server-js/lib run check:concept-audit
  npm --prefix crates/trusted-server-js/lib run typecheck
  npm --prefix crates/trusted-server-js/lib test -- --run test/integrations test/kernel test/first_display
  cargo test --package trusted-server-core --target "$(rustc -vV | sed -n 's/^host: //p')" publisher
  cargo test --package trusted-server-core --target "$(rustc -vV | sed -n 's/^host: //p')" integrations
  ```

- [ ] **Step 5: Commit an actual rc integration only when the tip advanced.**
      Stage each resolved conflict and refreshed audit/evidence file by its literal
      path after reviewing `git status --short`; never use a broad path or placeholder
      command. Then commit with subject `Integrate the latest rc baseline`.

### Task 18: Run the complete verification matrix

**Files:** no intended production changes; fix failures test-first in the owning task
and rerun this task from the start.

- [ ] **Step 1: Verify formatting and repository integrity.**

  ```bash
  cargo fmt --all -- --check
  npm --prefix crates/trusted-server-js/lib run format
  npm --prefix docs run format
  git diff --check
  ```

- [ ] **Step 2: Verify TypeScript build, types, lint, contracts, and bundles.**

  ```bash
  npm --prefix crates/trusted-server-js/lib ci
  npm --prefix crates/trusted-server-js/lib ls --all
  npm --prefix crates/trusted-server-js/lib run typecheck
  npm --prefix crates/trusted-server-js/lib run lint
  npm --prefix crates/trusted-server-js/lib test
  npm --prefix crates/trusted-server-js/lib run build
  npm --prefix crates/trusted-server-js/lib run build:prebid-external
  npm --prefix crates/trusted-server-js/lib run check:aps-contract
  npm --prefix crates/trusted-server-js/lib run check:concept-audit
  npm --prefix crates/trusted-server-js/lib run check:hard-cutover-absence
  npm --prefix crates/trusted-server-js/lib run check:bundle
  npm --prefix crates/trusted-server-js/lib run test:release
  ```

- [ ] **Step 3: Verify Rust and every runtime adapter.**

  ```bash
  cargo test-fastly
  cargo test-axum
  cargo test-cloudflare
  cargo test-spin
  cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test parity
  ./scripts/test-cli.sh
  ```

- [ ] **Step 4: Verify all target-matched clippy gates.**

  ```bash
  cargo clippy-fastly
  cargo clippy-axum
  cargo clippy-cloudflare
  cargo clippy-cloudflare-wasm
  cargo clippy-spin-native
  cargo clippy-spin-wasm
  ```

- [ ] **Step 5: Verify APS proxy transport parity.**

  ```bash
  ./scripts/integration-tests-aps-runner-proxy.sh --runtime axum
  ./scripts/integration-tests-aps-runner-proxy.sh --runtime fastly
  ./scripts/integration-tests-aps-runner-proxy.sh --runtime cloudflare
  ./scripts/integration-tests-aps-runner-proxy.sh --runtime spin
  ```

- [ ] **Step 6: Verify the hermetic browser matrix in all three engines.**

  ```bash
  bash scripts/ci/aps-tsjs-cutover.sh install-browsers
  bash scripts/ci/aps-tsjs-cutover.sh run-browser
  ```

- [ ] **Step 7: Push the exact locally verified candidate before remote gates.**
      Require a clean worktree, confirm `origin/rc/202608` is the audited base, push
      local HEAD to the named feature branch, and prove the remote ref equals local
      HEAD. Any later code, workflow, script, evidence-validator, or rc-integration
      change invalidates the remote evidence and restarts Task 18 from Step 1.

  ```bash
  test -z "$(git status --porcelain)"
  git push origin HEAD:feature/aps-tsjs-resilience-rc202608
  test "$(git rev-parse HEAD)" = "$(git ls-remote origin refs/heads/feature/aps-tsjs-resilience-rc202608 | awk '{print $1}')"
  ```

- [ ] **Step 8: Verify paired rc performance evidence on that exact remote HEAD.**
      Preserve Chromium 145.0.7632.6, Ubuntu 24.04 class, the fixed network profile,
      five warmups, 50 samples per variant, alternating order, one post-sample
      lifecycle observation, and paired heap contexts. The helper generates and
      records a unique evidence id, deterministically selects only the new run,
      watches it, downloads its one immutable artifact, and validates exact evidence,
      head, base, and mode bindings:

  ```bash
  node scripts/ci/dispatch-aps-tsjs-gate.mjs performance --ref feature/aps-tsjs-resilience-rc202608 --base-sha "$(git rev-parse origin/rc/202608)" --mode postswitch --output-dir target/aps-tsjs-final-performance
  ```

  Validation must finish and evidence must be written even when a soft Playwright
  assertion fails; fix the owner, commit, repeat all local verification, push, and
  repeat the complete sample rather than selectively rerunning rows.

- [ ] **Step 9: Verify real-GAM conformance.** This gate runs only in the protected
      `aps-real-gam` environment. Require a clean pushed commit, exact release id,
      helper-generated unique evidence id, and operator-supplied immutable rollback
      artifact id:

  ```bash
  test -n "${APS_REAL_GAM_PREVIOUS_ARTIFACT_ID:-}"
  node scripts/ci/dispatch-aps-tsjs-gate.mjs real-gam --ref feature/aps-tsjs-resilience-rc202608 --previous-artifact-id "${APS_REAL_GAM_PREVIOUS_ARTIFACT_ID}" --output-dir target/aps-tsjs-final-real-gam
  ```

  The helper derives the dispatched release id from the clean local artifact; the
  protected workflow compares it to `TS_REAL_GAM_EXPECTED_RELEASE_ID` supplied only
  by the GitHub environment. Require the downloaded scrubbed attestation manifest to
  name the exact evidence, release, rollback artifact, commit, successful conclusion,
  and workflow run. The workflow invokes `bash scripts/ci/aps-real-gam.sh
validate-inputs` and `run`; it tests PUC 1.17.2 in Chromium, Firefox, and WebKit.
  This is the only release proof allowed to depend on the live proxied APS runner;
  no fetched bytes are committed.

- [ ] **Step 10: Inspect final state.**

  ```bash
  git status --short --branch
  git log --oneline --decorate origin/rc/202608..HEAD
  ```

  Expected: clean worktree, all required commits present, no unrelated changes.

### Task 19: Request final review, push, and update the replacement PR

**Files:** no new implementation files.

- [ ] **Step 1: Use `superpowers:requesting-code-review`.** Review the complete diff
      against the exact PR base, design revision 43, this plan, the rc adoption
      ledger, security protocol, lifecycle ownership, no-vendoring boundary, and
      verification evidence. Resolve every blocking finding test-first and rerun the
      affected task plus Task 18.

- [ ] **Step 2: Use `superpowers:verification-before-completion`.** Confirm fresh
      command output rather than relying on earlier runs.

- [ ] **Step 3: Push the branch and ensure the PR base is `rc/202608`.** Use the
      GitHub publishing workflow; do not retarget to `main`. The PR description must
      identify the exact rc base, hard cutover, no-vendoring/proxy model, no cache or
      analytics expansion, named rc behaviors preserved, test matrix, performance
      evidence, and any external real-GAM prerequisite.

- [ ] **Step 4: Confirm GitHub checks.** If a check fails, inspect the actual GitHub
      job/log with the available repository GitHub workflow, reproduce locally where
      practical, fix the owner test-first, push, and wait for the replacement run.

- [ ] **Step 5: Use `superpowers:finishing-a-development-branch`.** Present the
      verified PR and branch state. Do not claim implementation complete while any
      required check, real-GAM gate, exact-base comparison, review finding, or
      uncommitted change remains.

## Completion checklist

- [ ] Design revision 43 and this one plan agree on `rc/202608` authority.
- [ ] All 23 concept rows have fresh final rc-baseline classifications.
- [ ] No backward-compatibility path or second runtime remains.
- [ ] No APS runner, GPT, or PUC bytes are vendored, pinned, cached, or stored.
- [ ] The live runner proxy remains fixed-target, bounded, five-second, and
      equivalent across all four adapters.
- [ ] `TsjsBootV1` contains one exact frozen ordered integration-config carrier;
      modules receive attenuated values only.
- [ ] Production boot uses one server-sealed canonical JSON lexical transport;
      bootstrap does not bundle the full object-form projection/config validators.
- [ ] `ServerBootIntegrityV1` is always present; direct runtime and takeover both
      recompute projection/config digests before effects, and takeover requires exact
      integrity/outline/handoff equality.
- [ ] No-agent preparation/activation/commit is synchronous and parser-blocking.
- [ ] ADM and APS reservations select one `render_owner_initial` slice with the
      source-neutral journal and compact self-contained PUC owner; `aps_initial`
      owns only APS-specific descriptor/nonce/mount behavior.
- [ ] APS uses top mount -> bootstrap -> outer data -> inner data -> creative with
      independent `b1_`/`n1_` nonces and exact conditional CSP.
- [ ] PUC protocol v4 owns registration/Promise settlement only, not APS DOM.
- [ ] Committed overlays retire exactly once on every required lifecycle boundary.
- [ ] `creative_opportunities.enabled` is required and preserves direct auction plus
      the exact inactive HTML policy.
- [ ] GPT diagnostics keep requested, GPT-fill, and observed sizes distinct.
- [ ] GAM attribution has one typed parser-time GPT owner and no raw global path.
- [ ] Rc C2/ESI, DataDome, PBS Cache, GPT, Prebid, creative, and remaining
      integrations pass unchanged-behavior contracts.
- [ ] Package, TypeScript, Prebid isolation, architecture, bundle, and hard-cutover
      gates pass.
- [ ] Hermetic three-browser, four-adapter proxy, paired rc performance, and
      protected real-GAM gates pass.
- [ ] Branch is clean, pushed, reviewed, and the PR targets `rc/202608`.
