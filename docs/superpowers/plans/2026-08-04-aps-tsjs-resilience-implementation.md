# APS Render Fix and TSJS Resilience Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use
> `superpowers:subagent-driven-development` (recommended) or
> `superpowers:executing-plans` to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking. Use
> `superpowers:test-driven-development` for each behavior change and
> `superpowers:verification-before-completion` before claiming a phase or the plan
> complete.

**Goal:** make APS render deterministically across SSAT, Trusted Server Prebid,
page-bids, direct auction, and fallback paths while replacing TSJS's duplicated
global state with one bounded, lifecycle-owned runtime.

**Architecture:** one minimal core entry constructs the sole runtime and
closure-private capability broker. Provider IIFEs register their owned adapters and
services; production core never imports or inlines every concrete layer. One parser-
blocking, server-composed critical artifact commits that runtime and the exact
modules needed for first display; catalogued later-only IIFEs join the same broker
after the protected first-display paint gate.
Rust emits one bounded tagged APS/ADM render-source contract and one exact per-slot
auction decision set, while the existing PBS Cache path remains a black-box
regression surface. Universal Creative 1.17.2 supplies the outer response and
owner-registration channels; the kernel owns control and APS document channels.
Direct and PUC APS/ADM paths settle through the same terminal state machine.

**Tech Stack:** Rust (`error-stack`, `http`, `serde`), lockfile TypeScript, Vitest,
Playwright, GPT/Prebid test adapters, Viceroy, and the existing four runtime
adapters.

---

**Source of truth:**
`docs/superpowers/specs/2026-08-04-aps-render-fix-and-tsjs-resilience-design.md`
revision 33, frozen review SHA
`958a79ccbbfe70c24a1529f6a4a469e15217cf5da799342a13d122fee4cdc99e`. This is the
only implementation-plan document for the work. APS render
and the runtime architecture are one coupled cutover: neither subsystem is useful or
safe to release independently, so they remain in this one plan.

## Scope guardrails

- Do not change analytics, persistence, billing, experimentation, or deployment
  routing. Those are separate specifications.
- Do not add compatibility aliases, a runtime selector, or dual old/new protocols.
  New construction is reachable only from test code until the coordinated switch.
- Keep unrelated publisher integrations behaviorally unchanged. Their only planned
  changes are thin integration-module registration and shared-runtime access.
- Follow `CLAUDE.md`: use adapter-specific Cargo aliases, `error-stack`, `log`, and
  descriptive `expect("should ...")`; never use bare `cargo test --workspace`.
- Every task follows red → green → refactor: add the narrow failing test, run it to
  prove the failure, implement the minimum complete contract, rerun focused tests,
  then run the task's regression commands.
- Do not create additional design or plan files. Implementation fixtures, schemas,
  tests, and workflow edits listed below are implementation artifacts, not extra
  plans.
- Never check APS runner bytes, a runner version/digest/license/metadata record, SRI,
  an updater, or an offline runner fallback into source, tests, evidence, or release
  artifacts. The only positive runner route is the live fixed-target proxy at
  `/integrations/aps/runner.js`; `/integrations/aps/runner/v1.js` is negative-only.
  This plan defines no runner cache behavior.
- Do not introduce a cache redesign. Preserve the pinned PBS Cache behavior through
  a thin `pbs_cache` carrier plus its pre/post-cutover black-box corpus; do not add a
  cache policy, URL/response contract, direct-cache path, reservation identity,
  deadline, error taxonomy, or price-authority rule.
- GPT, the APS runner, PUC, and other upstream bytes remain live external
  dependencies and are never bundled or copied into TSJS source/output/fixtures.
  The baseline's separately generated pure Prebid.js 10.26.0 artifact remains the
  explicit exception: it is independently built and purity-scanned, contains no
  TSJS behavior, and is never concatenated into a TSJS runtime artifact.

Every task ends with `git status --short`, focused verification, and one intentional
commit before the next task. Stage only the exact paths from that task's **Files**
list that the implementation changed; never use broad staging in a dirty worktree.
Use a descriptive sentence-case, imperative commit subject with no semantic prefix,
as required by `CLAUDE.md`. Task 19's coordinated production switch is one atomic
commit; do not split it into deployable half-states.

## Planned source shape

The exact split may be adjusted during implementation only when it preserves these
owners and dependency directions.

```text
crates/trusted-server-js/lib/src/
  kernel/
    identity.ts         navigation-prefix + u64 attempts; 128-bit CSPRNG tickets/nonces
    disposable.ts       owned disposer stack and terminal latch primitives
    integration_registry.ts  exact ABI/phase/release registration and capability broker
    phase_loader.ts      first-display/paint gate and authenticated deferred loading
    release_catalog.ts  canonical 20-module order, predicates, capabilities, and caps
    diagnostics.ts      bounded core-only snapshot ingress; no module subscriptions
    runtime.ts          bootstrap ownership and shared Runtime object
    sessions.ts         RuntimeSession and NavigationSession
  adapters/
    googletag.ts        only GPT-global access; diagnostic slot/cycle identity mint
    prebid.ts           only Prebid-global access
    messaging.ts        exact/versioned global and MessagePort envelopes
  services/
    context.ts          runtime-owned auction-context contributors
    slots.ts            SlotRecord indexes, request intents, physical cycles
    projections.ts      immutable navigation projection admission/commit
    targeting.ts        owner-aware GPT targeting journal and restoration
    reservations.ts     live renderer capabilities, WinnerContext, tombstones
    render.ts           RenderAttempt state machine and path drivers
    auction_batch.ts    shared fetch and child cancellation
  integrations/
    aps/render.ts       descriptor validation and static-renderer client
    gpt/{index,later}.ts        initial-display owner and later-only slice
    prebid/{index,later}.ts     initial PUC/admission owner and later refresh slice
    gpt/diagnostics_facts.ts  GPT-owned bounded object-identity fact stream
    gpt_diagnostics/{data_api,presentation}.ts  data-only API + deferred UI owners
    */lifecycle.ts      catalogued later-only Osano/Permutive/Sourcepoint slices
  core/
    index.ts            final public API installation
    bootstrap_controller.ts  generated minimal queue/watchdog/fallback controller
    trace.ts            trace reducer/store, public API, private presentation attach
    types.ts            public and wire types
  composition/
    browser.ts          production core entry; no deferred/test imports
    browser_test.ts     test-only concrete composition and injection seams
```

Test files mirror source ownership under `crates/trusted-server-js/lib/test/`.
Do not make `kernel/` depend on `adapters/`, `services/`, or `integrations/`.
Only test-only `composition/browser_test.ts` may import every concrete layer and
construct all dependencies. Production `composition/browser.ts` is the minimal core
entry and may import only kernel, immutable boot/projection contracts, queue/logger,
and the minimum direct-public-API setup; catalogued provider IIFEs own and register
their adapter/service implementations.

## Dependency order

```text
descriptor contract
  -> server admission/mediation/projection
  -> renderer endpoint

kernel primitives
  -> release catalog + exact artifact transport
  -> protected paint gate + authenticated deferred loading
  -> sessions + adapters + capability broker
  -> slot/reservation services
  -> lifecycle + auction batch
  -> GPT/Prebid/direct/fallback migration
  -> critical/deferred product slicing

server path + browser path
  -> hermetic browser matrix
  -> real-GAM conformance
  -> legacy deletion and hard cutover
```

## Coordinated-switch rule

Tasks 1–18 build contracts, pure serializers/parsers, services, integration modules,
and test-only composition without switching a shipped page onto an incompatible
half-state. In particular:

- Task 3 creates canonical projection/response serializers and parsers exercised
  directly in tests; production `/auction`, initial HTML, and page-bids stay on their
  current shapes until Task 19;
- Task 5 builds the versioned handler and exercises adapter parity through test-only
  registries while production dispatch remains unchanged until Task 19;
- Tasks 8A–8C compute the phase-aware release catalog/metadata, exact static
  transport, controller/fallback, capability broker, and deferred loader but do not
  claim production HTML/bootstrap ownership;
- Tasks 16–18 produce tested integration-module preparation/activation and composition without changing the
  shipped entry-point side effects.

Task 19 is the single coordinated production switch: initial HTML/page-bids, core,
GPT, Prebid, APS, fallback, and every enabled integration begin using the new
projection, manifest, versioned renderer, and integration-module runtime together. No runtime
flag, deployment router, or operator-visible old/new selector is introduced. Task
22 removes the now-unreachable old surfaces before any release candidate is built.
Every task's regression suite therefore remains green in task order.

## Phase 0 — pin the contract and baseline

### Task 0: Establish the focused baseline and scope checks

**Files:**

- Modify: `crates/trusted-server-js/lib/package.json`
- Modify: `crates/trusted-server-js/lib/tsconfig.json`
- Modify: `crates/trusted-server-js/lib/build-all.mjs`
- Create: `crates/trusted-server-js/lib/scripts/check-bundle-budgets.mjs`
- Create: `crates/trusted-server-js/lib/test/fixtures/performance/aps-tsjs-prechange.json`
- Create: `crates/trusted-server-integration-tests/browser/tests/shared/tsjs-performance.spec.ts`
- Modify: `scripts/integration-tests-browser.sh`
- Create: `scripts/dispatch-workflow-run.mjs`
- Create: `crates/trusted-server-js/lib/scripts/check-rc-july-adoption.mjs`
- Create: `crates/trusted-server-js/lib/test/contract/rc-july-adoption.test.mjs`
- Modify: `.github/workflows/test.yml`
- Modify: `.github/workflows/integration-tests.yml`
- Test: existing Rust, Vitest, and APS Playwright suites

- [ ] **Step 1: Write and run the failing executable adoption-manifest test.** Extract
      the `rcjuly-tsjs-manifest-v1` JSON block from the revision-33 spec, enumerate
      every `includeRoot` and exact file at
      `905984e62a0858c53d9f0ff6dd3a1bf190cf311d` with `git ls-tree`, and fail for an
      unmapped pinned file, a `lib/src` file mapped only to `RCJ-QUAL-01`, a dead
      mapping, or a manifest/ledger id mismatch. Pin the expected audit result at 144
      files, 38 mapping rows, and 23 ledger/manifest ids so a moving baseline cannot
      enter implementation silently:

  ```bash
  node --test crates/trusted-server-js/lib/test/contract/rc-july-adoption.test.mjs
  ```

  Expected before the script exists: FAIL. Expected after the minimal extractor and
  checker: PASS with zero unmapped/dead/gap arrays. Add the same command to CI and run
  it before every phase exit.

- [ ] **Step 2: Record baseline results before behavior changes:**

  ```bash
  cargo test-fastly
  cargo test-axum
  cargo test-cloudflare
  cargo test-spin
  npm --prefix crates/trusted-server-js/lib ci
  npm --prefix crates/trusted-server-js/lib test
  npm --prefix crates/trusted-server-js/lib run build
  ```

  Any pre-existing failure is recorded in the execution notes; it is not silently
  attributed to this work.

- [ ] **Step 3: Keep the lockfile-resolved compiler unchanged only while capturing the**
      pre-change baseline. Task 0A performs the intentional package and TypeScript
      upgrade before the remaining TSJS runtime is built. Add the checked-in
      `typecheck` script:

  ```json
  "typecheck": "tsc -p tsconfig.json --noEmit"
  ```

  Enable `noUncheckedIndexedAccess`, `exactOptionalPropertyTypes`,
  `verbatimModuleSyntax`, `noImplicitOverride`, and
  `useUnknownInCatchVariables`. Add the CI invocation with the lockfile-resolved
  compiler. Add both `typecheck` and the existing architectural `lint` command to
  CI.

- [ ] **Step 4: Before behavior changes, make bundle measurement deterministic and record the**
      minimal/reference/maximal raw, gzip, and Brotli bytes, Node/npm/TypeScript versions,
      Chromium version, CI machine class, fixture, five warmups, 50 samples, p90
      boot-to-first-display, and forced-GC CDP heap checkpoints. Commit these values to
      `aps-tsjs-prechange.json`; later tasks may compare against it but must not
      regenerate it from the completed implementation.

  For this pre-change capture, define the sets exactly as minimal `[core]`, reference
  `[core, creative, gpt, prebid, datadome]`, and maximal core plus every built
  integration. Record 23,317 / 8,687 / 7,686; 113,756 / 35,163 / 26,051; and
  187,224 / 53,799 / 37,790 bytes respectively. These original fields are immutable
  historical measurements of the old artifact membership. After the split they are
  printed as deltas only: they are not byte ceilings for the different post-split
  semantic sets. Task 18E appends one separately identified role-correct capture to
  this same JSON without changing these fields. Expose the comparator as
  `npm run check:bundle` and run it in the TypeScript CI job immediately after
  `npm run build`; before Task 18E it validates inventory/graph integrity and reports
  historical deltas, and after Task 18E it also enforces the role-correct ceilings.
  A generated metrics file that is not consumed by CI is not a gate.

  Extend `scripts/integration-tests-browser.sh` with
  `TS_BROWSER_FRAMEWORKS=nextjs` and use `npm --prefix ... exec -- playwright` for
  argument-safe invocation. The script remains the clean-checkout fixture builder:
  it builds release Fastly WASM with the integration environment, generates
  Viceroy configuration, builds/loads the framework image, installs browser
  dependencies, and builds both TSJS fixture variants. Run the new performance
  test itself—not only the bundle script—and write its 50-sample/heap output to the
  exact baseline path:

  ```bash
  TASK0_EVIDENCE_ID="aps-tsjs-baseline-$(git rev-parse --short=12 HEAD)-$(date -u +%Y%m%dT%H%M%SZ)"
  TS_BROWSER_FRAMEWORKS=nextjs \
  TSJS_PERF_MODE=baseline \
  TSJS_PERF_EVIDENCE_ID="$TASK0_EVIDENCE_ID" \
  TSJS_PERF_OUTPUT=crates/trusted-server-js/lib/test/fixtures/performance/aps-tsjs-prechange.json \
    ./scripts/integration-tests-browser.sh \
      tests/shared/tsjs-performance.spec.ts --project=chromium
  node crates/trusted-server-js/lib/scripts/check-bundle-budgets.mjs --baseline-only
  ```

  The integration workflow runs the same focused command on its pinned CI image and
  uploads the resulting JSON. Add required manual input `evidence_id` and include it
  in `run-name`. Map that input directly into the focused job as
  `TSJS_PERF_EVIDENCE_ID: ${{ inputs.evidence_id }}` and upload the resulting baseline
  JSON as `aps-tsjs-baseline-${{ github.run_id }}`. The performance test writes that
  exact environment value into top-level string field `evidenceId`.
  `dispatch-workflow-run.mjs` validates that the ref is a pushed
  branch/tag, dispatches with a unique evidence id, polls for exactly that run, and
  prints its numeric run id. Record the unique evidence id—not the later GitHub
  numeric run id—in the baseline JSON so the checked-in capture and remote workflow
  artifact can be joined without a self-referential post-commit edit.

- [ ] **Step 5: Run:**

  ```bash
  npm --prefix crates/trusted-server-js/lib run typecheck
  npm --prefix crates/trusted-server-js/lib run lint
  node --test crates/trusted-server-js/lib/test/contract/rc-july-adoption.test.mjs
  test -s crates/trusted-server-js/lib/test/fixtures/performance/aps-tsjs-prechange.json
  jq -e '.evidenceId | type == "string" and length > 0' \
    crates/trusted-server-js/lib/test/fixtures/performance/aps-tsjs-prechange.json
  git diff --check
  ```

- [ ] **Step 6: Commit and push the complete Task 0 gate before dispatching it.** Stage
      only the Task 0 files, create the baseline-gate commit, push the current branch,
      and then dispatch the workflow from that exact pushed SHA using the evidence id
      already recorded in the baseline. The numeric run id remains external workflow
      evidence; do not edit or recommit the baseline after the run:

  ```bash
  git add \
    crates/trusted-server-js/lib/package.json \
    crates/trusted-server-js/lib/tsconfig.json \
    crates/trusted-server-js/lib/build-all.mjs \
    crates/trusted-server-js/lib/scripts/check-bundle-budgets.mjs \
    crates/trusted-server-js/lib/test/fixtures/performance/aps-tsjs-prechange.json \
    crates/trusted-server-integration-tests/browser/tests/shared/tsjs-performance.spec.ts \
    scripts/integration-tests-browser.sh \
    scripts/dispatch-workflow-run.mjs \
    crates/trusted-server-js/lib/scripts/check-rc-july-adoption.mjs \
    crates/trusted-server-js/lib/test/contract/rc-july-adoption.test.mjs \
    .github/workflows/test.yml \
    .github/workflows/integration-tests.yml
  git diff --cached --check
  git commit -m "Pin the APS TSJS baseline gates"
  TASK0_REF="$(git branch --show-current)"
  git push origin "$TASK0_REF"
  TASK0_SHA="$(git rev-parse HEAD)"
  TASK0_EVIDENCE_ID="$(jq -er .evidenceId \
    crates/trusted-server-js/lib/test/fixtures/performance/aps-tsjs-prechange.json)"
  test -n "$TASK0_REF"
  git fetch origin "$TASK0_REF"
  test "$TASK0_SHA" = "$(git rev-parse "origin/$TASK0_REF")"
  TASK0_RUN_ID="$(node scripts/dispatch-workflow-run.mjs \
    integration-tests.yml "$TASK0_REF" \
    evidence_id="$TASK0_EVIDENCE_ID")"
  gh run watch "$TASK0_RUN_ID" --exit-status
  test "$TASK0_SHA" = "$(gh run view "$TASK0_RUN_ID" --json headSha --jq .headSha)"
  TASK0_REMOTE_EVIDENCE_DIR="$(mktemp -d)"
  gh run download "$TASK0_RUN_ID" \
    --name "aps-tsjs-baseline-$TASK0_RUN_ID" \
    --dir "$TASK0_REMOTE_EVIDENCE_DIR"
  jq -e --arg evidence "$TASK0_EVIDENCE_ID" \
    '.evidenceId == $evidence' \
    "$TASK0_REMOTE_EVIDENCE_DIR/aps-tsjs-prechange.json"
  ```

### Task 0A: Upgrade the TSJS package and TypeScript toolchain

**Files:**

- Modify: `crates/trusted-server-js/lib/package.json`
- Modify: `crates/trusted-server-js/lib/package-lock.json`
- Modify: `crates/trusted-server-js/lib/eslint.config.js`
- Modify if required by an actual compatibility failure:
  `crates/trusted-server-js/lib/tsconfig.json`
- Modify if required by an actual compatibility failure:
  `crates/trusted-server-js/lib/vitest.config.ts`
- Modify only the exact TSJS source/test/build files that a new compiler or tool
  correctly rejects; do not mix in behavior changes or unrelated refactors

- [ ] **Step 1: Capture an executable package-compatibility inventory before changing the**
      lockfile. Run `npm outdated --json`, query direct-package peer/engine ranges,
      and record the current Node/npm/compiler versions. Select the newest stable,
      mutually compatible direct toolchain supported by the repository-pinned Node
      major. Upgrade TypeScript to the newest stable release supported by the newest
      `typescript-eslint`; do not install a newer TypeScript that its parser explicitly
      excludes. Keep `@types/node` on the repository's pinned Node major.

  `prebid.js` is the one explicit package exception: pin it to exactly `10.26.0`, as
  required by this design's external pure-Prebid artifact contract. Do not use this
  task to adopt Prebid 11 or change the selected Prebid modules. If the latest ESLint
  major is incompatible with the unmaintained `eslint-plugin-import`, migrate the
  existing import rules to the maintained compatible `eslint-plugin-import-x` rather
  than holding the rest of the lint toolchain back. Remove redundant direct
  `@typescript-eslint/parser`/plugin declarations when the used `typescript-eslint`
  package already owns those exact dependencies.

- [ ] **Step 2: Update direct package ranges and regenerate `package-lock.json` through npm.**
      Do not hand-edit lock entries. Require a peer-clean `npm ls --all` and a second
      clean `npm ci` from the generated lock. Treat invalid, missing, or extraneous
      nodes as failures. Do not run `npm audit fix --force`; audit findings that remain
      solely behind the mandated Prebid pin are reported, not silently solved by
      violating the artifact contract.

- [ ] **Step 3: Make only compatibility edits proven necessary by the upgraded tools.** Keep
      all strict compiler flags and architecture rules enabled. A new compiler/linter
      diagnostic gets a source fix or a documented, narrow configuration correction;
      it is not suppressed globally. Production bundle entry points, bundle ids,
      integration behavior, the APS runner policy, and the Prebid module set must not
      change in this task.

- [ ] **Step 4: Verify the upgraded toolchain and every shipped artifact:**

  ```bash
  npm --prefix crates/trusted-server-js/lib ci
  npm --prefix crates/trusted-server-js/lib ls --all
  npm --prefix crates/trusted-server-js/lib run typecheck
  npm --prefix crates/trusted-server-js/lib run lint
  npm --prefix crates/trusted-server-js/lib run format
  npm --prefix crates/trusted-server-js/lib test -- --run
  npm --prefix crates/trusted-server-js/lib run build
  node --test crates/trusted-server-js/lib/test/contract/rc-july-adoption.test.mjs
  ```

  Also print the resolved Node, npm, TypeScript, ESLint, Vite, Vitest, and jsdom
  versions into the task verification evidence. Re-run the external Prebid artifact
  integration test and prove both `package.json` and the lockfile resolve Prebid
  `10.26.0` exactly.

- [ ] **Step 5: Commit the toolchain upgrade as its own rollback boundary.**

  ```bash
  git add \
    crates/trusted-server-js/lib/package.json \
    crates/trusted-server-js/lib/package-lock.json \
    crates/trusted-server-js/lib/eslint.config.js \
    crates/trusted-server-js/lib/tsconfig.json \
    crates/trusted-server-js/lib/vitest.config.ts
  git commit -m "Upgrade the package and TypeScript toolchain"
  ```

  Add only compatibility files that actually changed to the explicit staging list;
  do not use broad staging.

### Task 1: Make the APS descriptor a cross-language executable contract

**Files:**

- Modify: `crates/trusted-server-core/src/auction/types.rs`
- Modify: `crates/trusted-server-core/src/auction/formats.rs`
- Modify: `crates/trusted-server-core/src/integrations/aps.rs`
- Modify: `crates/trusted-server-js/lib/src/core/types.ts`
- Modify: `crates/trusted-server-js/lib/src/integrations/aps/render.ts`
- Modify: `crates/trusted-server-js/lib/test/fixtures/aps-renderer-v1.json`
- Create: `crates/trusted-server-js/lib/test/fixtures/aps-renderer-v1-corpus.json`
- Create: `crates/trusted-server-js/lib/test/fixtures/aps-renderer-v1.schema.json`
- Create: `scripts/generate-aps-renderer-contract.mjs`
- Create: `crates/trusted-server-core/src/integrations/generated/aps_renderer_validator_v1.js`
- Create: `crates/trusted-server-js/lib/src/integrations/aps/generated/renderer_validator_v1.ts`
- Create: `crates/trusted-server-js/lib/test/contract/aps-renderer-es5.test.mjs`
- Modify: `crates/trusted-server-js/lib/package.json`
- Modify: `crates/trusted-server-js/lib/test/integrations/aps/render.test.ts`

- [ ] **Step 1: Add failing corpus tests in Rust and Vitest for:**
  - required versus optional exact keys;
  - UTF-8 byte limits, not JavaScript character counts;
  - exact numeric/integral dimensions at 0/1/4096/4097 and distinct
    `invalid_dimensions` versus `dimensions_out_of_range` results;
  - HTTPS URL, credentials, publisher-origin rejection, and URL byte limit;
  - canonical standard base64 and decoded 256 KiB limit;
  - UTF-8 decode failure and malformed JSON;
  - exactly one seat/bid and exact nested keys;
  - duplicated-field disagreement;
  - nonfinite, negative, and wrong-type price;
  - unknown descriptor version/type/tag type.

- [ ] **Step 2: Run the focused tests and confirm at least one new adversarial vector fails in**
      each implementation:

  ```bash
  cargo test-fastly aps_renderer
  npm --prefix crates/trusted-server-js/lib test -- --run test/integrations/aps/render.test.ts
  ```

- [ ] **Step 3: Add `generate:aps-contract` and `check:aps-contract` scripts. The generator reads**
      the neutral schema/corpus and writes both named generated validator files. The
      check runs the generator in staleness mode and fails on any diff. The Node test
      executes the exact ES5 file embedded by `aps.rs` in a `vm` for every corpus
      vector; it is not allowed to substitute the TypeScript validator.

- [ ] **Step 4: Implement `BidRenderSourceV1`/the equivalent Rust enum with exactly `aps`, `adm`,**
      and baseline `pbs_cache` members. Ensure APS has only `ApsRendererV1`; delete alternate APS
      `adm`, `meta`, or debug reconstruction. Align upstream and descriptor bid ids to
      1–64 UTF-8 bytes with no NUL/control. Define shared
      `RENDER_DIMENSION_MIN = 1` and `RENDER_DIMENSION_MAX = 4096`; apply the same
      noninteger/nonpositive versus out-of-range distinction in Rust, TS, generated
      ES5, APS/ADM sources, programmatic sizes, and later DOM construction. The
      baseline cache carrier does not add a new dimension validator. No
      validator or adapter may clamp.

- [ ] **Step 5: Rerun the focused commands plus:**

  ```bash
  npm --prefix crates/trusted-server-js/lib run typecheck
  npm --prefix crates/trusted-server-js/lib run lint
  npm --prefix crates/trusted-server-js/lib run check:aps-contract
  node --test crates/trusted-server-js/lib/test/contract/aps-renderer-es5.test.mjs
  ```

- [ ] **Step 6: Stage and commit the generated cross-language contract atomically:**

  ```bash
  git status --short
  git add \
    crates/trusted-server-core/src/auction/types.rs \
    crates/trusted-server-core/src/auction/formats.rs \
    crates/trusted-server-core/src/integrations/aps.rs \
    crates/trusted-server-js/lib/src/core/types.ts \
    crates/trusted-server-js/lib/src/integrations/aps/render.ts \
    crates/trusted-server-js/lib/test/fixtures/aps-renderer-v1.json \
    crates/trusted-server-js/lib/test/fixtures/aps-renderer-v1-corpus.json \
    crates/trusted-server-js/lib/test/fixtures/aps-renderer-v1.schema.json \
    scripts/generate-aps-renderer-contract.mjs \
    crates/trusted-server-core/src/integrations/generated/aps_renderer_validator_v1.js \
    crates/trusted-server-js/lib/src/integrations/aps/generated/renderer_validator_v1.ts \
    crates/trusted-server-js/lib/test/contract/aps-renderer-es5.test.mjs \
    crates/trusted-server-js/lib/package.json \
    crates/trusted-server-js/lib/test/integrations/aps/render.test.ts
  git diff --cached --check
  git commit -m "Define the APS renderer contract"
  ```

### Phase 0 exit

- The baseline is known.
- Strict TypeScript is a checked-in CI command.
- Rust, TypeScript, and embedded ES5 agree on every APS descriptor vector.
- No external observability or experiment artifact is planned or created.

## Phase 1 — make the server APS path deterministic

### Task 2: Harden APS admission and typed drop reasons

**Files:**

- Modify: `crates/trusted-server-core/src/integrations/aps.rs`
- Modify: `crates/trusted-server-core/src/auction/provider.rs`
- Modify: `crates/trusted-server-core/src/auction/types.rs`
- Modify: `crates/trusted-server-core/src/auction/formats.rs`
- Modify: `crates/trusted-server-core/src/auction/orchestrator.rs`

- [ ] **Step 1: Add failing tests for missing, duplicate, and >64-byte upstream bid ids;**
      malformed contextual data isolated to a bid when safe; missing/invalid
      `creativeurl`; invalid `tagtype`; non-HTTPS/self-origin URL; exact 1–4096
      dimension membership and reason mapping; default-off and enabled script
      creatives; invalid/nonfinite price;
      and one invalid sibling beside one valid bid.

- [ ] **Step 2: Replace stringly ad-hoc reasons with one typed APS/auction drop-reason enum used**
      exhaustively by provider response parsing and publisher debug projection. Do not
      add a persistence or external-event failure reason.

- [ ] **Step 3: Validate per bid before constructing `ApsRendererV1`. Preserve the exact accepted**
      AAX bid in the encoded projection and cross-check it against the descriptor.

- [ ] **Step 4: Treat a provider currency that violates the existing auction/provider contract as**
      `invalid_provider_response`. Do not introduce a currency-specific public failure,
      add or change auction configuration, add currency requirements, or alter non-APS
      behavior. A slot with zero eligible providers is exactly
      `failed{reason:'slot_not_eligible'}`.

- [ ] **Step 5: Run:**

  ```bash
  cargo test-fastly integrations::aps
  cargo test-fastly auction
  cargo test-axum
  ```

- [ ] **Step 6: Stage and commit the APS admission hardening:**

  ```bash
  git status --short
  git add \
    crates/trusted-server-core/src/integrations/aps.rs \
    crates/trusted-server-core/src/auction/provider.rs \
    crates/trusted-server-core/src/auction/types.rs \
    crates/trusted-server-core/src/auction/formats.rs \
    crates/trusted-server-core/src/auction/orchestrator.rs
  git diff --cached --check
  git commit -m "Harden APS bid admission"
  ```

### Task 3: Make per-slot outcomes explicit and mediation provenance-safe

**Files:**

- Modify: `crates/trusted-server-core/src/auction/orchestrator.rs`
- Modify: `crates/trusted-server-core/src/auction/types.rs`
- Modify: `crates/trusted-server-core/src/auction/formats.rs`
- Modify: `crates/trusted-server-core/src/auction/provider.rs`
- Modify: `crates/trusted-server-core/src/auction/endpoints.rs`
- Modify: `crates/trusted-server-core/src/integrations/adserver_mock.rs`
- Modify: `crates/trusted-server-core/src/publisher.rs`
- Modify: `crates/trusted-server-js/lib/src/core/types.ts`
- Modify: `crates/trusted-server-js/lib/src/core/auction.ts`
- Modify: `crates/trusted-server-js/lib/test/core/auction.test.ts`

- [ ] **Step 1: Add failing server and TS parser tests proving every requested slot receives**
      exactly one ordered `winner | no_bid | failed` decision. Cover provider launch,
      transport, timeout, HTTP, parse, per-bid validation, mediation failure, selected
      winner projection failure, one provider failure beside another provider winner,
      all-provider no-bid, partial slots, duplicate/extra/missing decisions, and the
      exact closed failure priority from the spec, including the direct
      `identity_generation_failed` wire result.

  Add exact `BrowserAuctionProjectionV1` boundary cases: 0/256/257 results and bids;
  auction id `^[A-Za-z0-9._:-]{1,128}$`, exact 12-character base64url candidate,
  provider `^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$`, upstream id 1–64 UTF-8 bytes,
  slot 1–256 UTF-8 bytes with no NUL/control, exact unique `r1_` reservation, finite
  nonnegative CPM, literal `USD`; targeting 31/32/33 entries, exact
  `[A-Za-z0-9_]{1,20}` keys at 19/20/21 characters,
  39/40/41-scalar and 159/160/161-byte values, control/unpaired-surrogate rejection,
  reserved `hb_adid`, duplicate joins, and accessors/prototypes. Measure canonical
  schema-order/request-order/lexically-sorted-targeting JSON just below/at/above
  `8 * 1024 * 1024` bytes.

- [ ] **Step 2: Normalize every provider response to exactly one internal `ProviderSlotOutcome`**
      per dispatched slot. Produce `AuctionDecisionSetV1` once in the orchestrator.
      Add pure serializers/parsers for `/auction`
      `ext.trusted_server.slot_results` and `BrowserAuctionProjectionV1`, exercised by
      direct unit tests while preserving the current internal return API. Do not yet
      wire them into production `/auction`, initial HTML, or page-bids; their exact
      coordinated switch occurs in Task 19. A winner must join exactly one projected
      bid by exact slot plus candidate id; no-bid/failed joins none.

  When a complete canonical projection exceeds 8 MiB, transactionally convert every
  winner decision to `failed{reason:'winner_not_renderable'}`, emit zero projected TS
  winner bids, retain existing no-bid/failed decisions in request order, and remove
  the corresponding `/auction` TS `seatbid` entries. Never choose a first-fit subset
  or emit a partial projection. Prove the reduced result is bounded and deterministic
  under response-order permutations.

- [ ] **Step 3: Introduce internal provenance `(provider_name, upstream_bid_id)` and a**
      response-unique opaque 12-character base64url `candidate_id` from 9 CSPRNG bytes;
      test eight response-local collision retries and terminal `internal_error`. Store
      candidates before mediation; put the id only at
      `ext.trusted_server.candidate_id`; require the mock/configured mediator to echo
      exactly one known id. Take only the selected price/selection metadata from
      mediation and restore every render, identity, dimension, currency, and
      notification field from the stored source candidate. Reject missing/unknown/
      duplicate echoes, substitutions, and mediator-native render sources.

- [ ] **Step 4: Preserve the repository's configured mediator selection and timeout fallback.**
      Preserve direct highest-CPM selection, adding only the deterministic
      `(provider_name, upstream_bid_id)` tie break. Add response-order permutation tests
      and prove opaque ids/arrival order never break ties.

- [ ] **Step 5: Define and test the exact `/auction` winner wire. For APS/ADM, standard `bid.id` is the**
      `r1_` renderer reservation; baseline `pbs_cache` retains its native cache id;
      `bid.impid` maps exactly to the server slot; and
      `bid.ext.trusted_server` has only `candidate_id`, `slot_id`, and
      `render_source`. Require the four-way decision/candidate/impid/slot join. Reject
      missing, duplicate, extra, or mismatched bids; APS/`pbs_cache` standard `adm`; and ADM
      disagreement between standard `adm` and `render_source.adm`. TSJS renders only
      the tagged source and never reconstructs it from standard fields.

  The producer and parser must both deny unknown keys and enforce the same projection
  field/count/byte grammars before any slot, reservation, targeting, or bid mutation.
  Boot maps malformed/oversized input to `abi_mismatch`; page-bids/direct admission
  maps it to `invalid_response` with no partial state.

- [ ] **Step 6: Do not modify `auction_config_types.rs`, `auction/config.rs`, settings, TOML, or**
      auction documentation unless compilation reveals an existing type reference that
      must be renamed for the tagged render-source union. This task adds no new
      `winner_selection`, currency, or mediator-fallback requirement.

- [ ] **Step 7: Run:**

  ```bash
  cargo test-fastly orchestrator
  cargo test-fastly formats
  cargo test-fastly publisher
  cargo test-axum
  npm --prefix crates/trusted-server-js/lib test -- --run test/core/auction.test.ts
  ```

- [ ] **Step 8: Stage and commit the complete per-slot decision contract:**

  ```bash
  git status --short
  git add \
    crates/trusted-server-core/src/auction/orchestrator.rs \
    crates/trusted-server-core/src/auction/types.rs \
    crates/trusted-server-core/src/auction/formats.rs \
    crates/trusted-server-core/src/auction/provider.rs \
    crates/trusted-server-core/src/auction/endpoints.rs \
    crates/trusted-server-core/src/integrations/adserver_mock.rs \
    crates/trusted-server-core/src/publisher.rs \
    crates/trusted-server-js/lib/src/core/types.ts \
    crates/trusted-server-js/lib/src/core/auction.ts \
    crates/trusted-server-js/lib/test/core/auction.test.ts
  git diff --cached --check
  git commit -m "Project complete auction slot decisions"
  ```

### Task 4: Project one tagged render source and exact renderer identity

**Files:**

- Modify: `crates/trusted-server-core/src/publisher.rs`
- Modify: `crates/trusted-server-core/src/auction/formats.rs`
- Modify: `crates/trusted-server-core/src/auction/types.rs`
- Modify: `crates/trusted-server-core/src/integrations/gpt.rs`
- Modify: `crates/trusted-server-js/lib/src/core/types.ts`
- Modify: `crates/trusted-server-js/lib/src/core/config.ts`
- Modify: `crates/trusted-server-js/lib/test/integrations/gpt/ad_init.test.ts`
- Modify: `crates/trusted-server-js/lib/test/integrations/prebid/index.test.ts`

- [ ] **Step 1: Add failing server tests for SSAT boot bids, page-bids, and `/auction`. Each must**
      preserve the discriminated `BidRenderSourceV1` and `candidateId`. APS/ADM carries
      one server-minted renderer reservation plus the finite nonnegative selected CPM
      that later becomes the internal `WinnerContext`; browser projection exposes
      `rendererReservationId` and `/auction` uses it as standard OpenRTB `bid.id`.
      Baseline `pbs_cache` carries only `cacheId/cacheHost/cachePath`, has no
      reservation, and retains its native id. CPM is never copied into an APS/ADM
      render descriptor or capability.

- [ ] **Step 2: Add failing browser tests proving every TS-owned APS and ADM PUC source**
      uses the `r1_` reservation byte-for-byte as GAM `hb_adid`. For Trusted Server
      Prebid, replace the generated TS bid `adId` with that same reservation before
      targeting; keep native Prebid `adId` untouched. Preserve PBS Cache UUID as the
      existing cache transport/targeting identity only, never as APS/ADM bridge
      authority. Add negative tests for truncation, APS/ADM fallback to upstream or
      cache ids, and native-bid mutation.

- [ ] **Step 3: Add Rust generation tests for `r1_` plus 22 unpadded base64url characters from**
      16 CSPRNG bytes, response-local uniqueness, eight collision retries, and
      `identity_generation_failed`. Make projection choose identity from a tagged
      enum/path decision, not an `or_else` chain. Reject invalid targeting before
      serialization; never truncate.

- [ ] **Step 4: Pin cache as a black-box non-regression surface.** Add only the
      discriminated `BaselinePbsCacheSourceV1` carrier required to preserve existing
      `hb_adid`, `hb_cache_host`, and `hb_cache_path` handoff. Test deliberate
      ADM-over-cache precedence, cache-only projection, native UUID preservation, and
      absence from the reservation registry. Do not add `CacheFetchPolicyV1`, a
      browser fetch URL, response validator, direct-cache path, new deadline/error,
      dimensions, or price-expansion rule.

- [ ] **Step 5: Limit this task to wire projection and pre-mutation validation. Attempt-owned**
      compare-and-restore targeting cleanup is implemented only after `RenderAttempt`
      exists in Tasks 13 and 16.

  This task does not publish the new initial/page-bid projection or mutate a live
  Prebid/GPT bid. Those production mutations begin atomically in Task 19 after the
  reservation store and integration-module runtime exist.

- [ ] **Step 6: Run:**

  ```bash
  cargo test-fastly publisher
  npm --prefix crates/trusted-server-js/lib test -- --run test/integrations/gpt/ad_init.test.ts test/integrations/prebid/index.test.ts
  ```

- [ ] **Step 7: Stage and commit the tagged render-source projection:**

  ```bash
  git status --short
  git add \
    crates/trusted-server-core/src/publisher.rs \
    crates/trusted-server-core/src/auction/formats.rs \
    crates/trusted-server-core/src/auction/types.rs \
    crates/trusted-server-core/src/integrations/gpt.rs \
    crates/trusted-server-js/lib/src/core/types.ts \
    crates/trusted-server-js/lib/src/core/config.ts \
    crates/trusted-server-js/lib/test/integrations/gpt/ad_init.test.ts \
    crates/trusted-server-js/lib/test/integrations/prebid/index.test.ts
  git diff --cached --check
  git commit -m "Project tagged auction render sources"
  ```

### Task 5: Serve the static renderer and live APS runner proxy in three green checkpoints

Task 5 is an umbrella only. Execute and review three independent red-to-green
commits: 5A defines the common reserved-family and raw-proxy contract, 5B implements
and attests the four adapter transports, and 5C implements the static renderer plus
its fictional browser fixture. The combined inventory below is not authorization to
collapse those checkpoints or carry unverified behavior between them.

**Files:**

- Modify: `crates/trusted-server-core/src/integrations/aps.rs`
- Modify: `crates/trusted-server-core/src/integrations/mod.rs`
- Modify: `crates/trusted-server-core/src/integrations/registry.rs`
- Modify: `crates/trusted-server-core/src/platform/http.rs`
- Modify: `crates/trusted-server-core/src/platform/mod.rs`
- Modify: `crates/trusted-server-core/src/platform/test_support.rs`
- Modify: `crates/trusted-server-core/src/platform/types.rs`
- Modify: `crates/trusted-server-adapter-fastly/src/app.rs`
- Modify: `crates/trusted-server-adapter-fastly/src/main.rs`
- Modify: `crates/trusted-server-adapter-fastly/src/middleware.rs`
- Modify: `crates/trusted-server-adapter-fastly/src/platform.rs`
- Modify: `crates/trusted-server-adapter-fastly/Cargo.toml`
- Modify: `crates/trusted-server-adapter-axum/src/app.rs`
- Modify: `crates/trusted-server-adapter-axum/src/main.rs`
- Modify: `crates/trusted-server-adapter-axum/src/middleware.rs`
- Modify: `crates/trusted-server-adapter-axum/src/platform.rs`
- Modify: `crates/trusted-server-adapter-axum/tests/routes.rs`
- Modify: `crates/trusted-server-adapter-cloudflare/src/app.rs`
- Modify: `crates/trusted-server-adapter-cloudflare/src/lib.rs`
- Modify: `crates/trusted-server-adapter-cloudflare/src/middleware.rs`
- Modify: `crates/trusted-server-adapter-cloudflare/src/platform.rs`
- Modify: `crates/trusted-server-adapter-cloudflare/Cargo.toml`
- Modify: `crates/trusted-server-adapter-cloudflare/build.sh`
- Modify: `crates/trusted-server-adapter-cloudflare/tests/routes.rs`
- Create: `crates/trusted-server-adapter-cloudflare/wrangler.aps-runner-proxy.toml`
- Modify: `crates/trusted-server-adapter-spin/src/app.rs`
- Modify: `crates/trusted-server-adapter-spin/src/lib.rs`
- Modify: `crates/trusted-server-adapter-spin/src/middleware.rs`
- Modify: `crates/trusted-server-adapter-spin/src/platform.rs`
- Modify: `crates/trusted-server-adapter-spin/Cargo.toml`
- Modify: `crates/trusted-server-adapter-spin/tests/routes.rs`
- Create: `crates/trusted-server-integration-tests/fixtures/configs/spin-aps-runner-proxy.toml`
- Create: `crates/trusted-server-integration-tests/fixtures/configs/cloudflare-aps-runner-proxy-fixture.toml`
- Create: `crates/trusted-server-integration-tests/fixtures/cloudflare/aps-runner-proxy-service.js`
- Modify: `crates/trusted-server-integration-tests/Cargo.toml`
- Modify: `crates/trusted-server-integration-tests/README.md`
- Create: `crates/trusted-server-integration-tests/tests/aps_runner_proxy.rs`
- Create: `crates/trusted-server-integration-tests/tests/common/aps_runner_upstream.rs`
- Modify: `crates/trusted-server-integration-tests/tests/common/mod.rs`
- Modify: `crates/trusted-server-integration-tests/tests/common/runtime.rs`
- Modify: `crates/trusted-server-integration-tests/tests/environments/axum.rs`
- Create: `crates/trusted-server-integration-tests/tests/environments/spin.rs`
- Modify: `crates/trusted-server-integration-tests/tests/environments/mod.rs`
- Modify: `crates/trusted-server-integration-tests/tests/environments/cloudflare.rs`
- Modify: `crates/trusted-server-integration-tests/tests/environments/fastly.rs`
- Modify: `crates/trusted-server-integration-tests/tests/parity.rs`
- Modify: `crates/trusted-server-integration-tests/browser/tests/shared/aps-renderer.spec.ts`
- Create: `crates/trusted-server-integration-tests/browser/fixtures/fictional-aps-runner.js`
- Create: `scripts/integration-tests-aps-runner-proxy.sh`
- Modify: `scripts/integration-tests-browser.sh`
- Modify: `scripts/integration-tests.sh`
- Modify: `.github/workflows/integration-tests.yml`
- Modify: `.tool-versions`
- Modify: `Cargo.lock`
- Modify: `CLAUDE.md`
- Modify: `docs/guide/error-reference.md`
- Modify: `docs/guide/getting-started.md`
- Modify: `docs/guide/testing.md`

#### Task 5A: Define and test the common reserved-route and raw-proxy contract

**Task 5A files:**

- `crates/trusted-server-core/src/integrations/aps.rs`
- `crates/trusted-server-core/src/integrations/mod.rs`
- `crates/trusted-server-core/src/integrations/registry.rs`
- `crates/trusted-server-core/src/platform/http.rs`
- `crates/trusted-server-core/src/platform/mod.rs`
- `crates/trusted-server-core/src/platform/test_support.rs`
- `crates/trusted-server-core/src/platform/types.rs`

- [ ] **Step A1: Write failing reserved-family and raw-proxy contract tests.**

  In `trusted-server-core` only, cover reserved-family classification and the
  `ApsV1Integration`/platform test-support contract with a fake transport. Assert the
  exact upstream target/request evidence, five-second dispatch-through-final-byte
  policy, cancellation, body cap, closed response grammar, replacement headers, and
  empty non-leaking failures. Do not add or run real-adapter route tests in 5A;
  adapter dispatch and method behavior belong to 5B, and static renderer bytes/policy
  belong to 5C.

- [ ] **Step A2: Run the new focused tests and prove they fail.**

  ```bash
  cargo test-fastly integrations::aps
  ```

  Expected: the bounded raw-proxy policy/evidence contract and fake response
  validation are not implemented; no adapter suite has been changed.

- [ ] **Step A3: Define the bounded raw-proxy platform contract.**

  Add a dedicated request/response policy in `platform/http.rs` and core test-support
  contract that requires adapter implementations to:
  - sends only credential-free `GET` to the compile-time fixed URL
    `https://client.aps.amazon-adsystem.com/prebid-creative.js` with
    `Accept-Encoding: identity`, no forwarded browser/publisher headers, no referrer,
    and redirect following disabled;
  - exposes `ProxyResponseEvidenceV1`: status plus every occurrence of the three
    security-relevant headers when the runtime preserves pairs, or the exact visibly
    combined value when it does not. Combined values are never split and every
    comma/list form fails the singleton grammar; erased/ambiguous evidence is
    `unavailable`, never reconstructed;
  - returns a bounded stream or bounded buffer with a five-second monotonic deadline
    from dispatch through the final body byte, cancellation on timeout/overflow, and
    generation-inert late continuations; and
  - uses the common `APS_RUNNER_MAX_RESPONSE_BYTES = 8 MiB` cap.

  Task 5B's Cloudflare implementation must preserve `web_sys::Request.method()` before workers-rs conversion
  and restore it at the reserved pre-router boundary because workers-rs maps extension
  methods such as `PROPFIND` to `GET`. It must also inspect the initial Workers headers
  before its generic adapter strips encoding/length; concatenated duplicates remain
  visibly combined and fail.
  Spin must bypass `spin_sdk::http::send`, call the WASI HTTP outgoing handler with
  supported request options, and poll both response and body stream against a
  monotonic-clock total deadline. Axum and Fastly must enforce the same deadline,
  evidence, redirect, encoding, and cap contract instead of their generic client
  defaults. If a runtime cannot supply the required evidence or cancellation
  behavior, APS cannot be enabled there and the release is blocked.

- [ ] **Step A4: Make the common contract and core fakes green, then commit before adapter**
      transport work. This checkpoint contains only reserved-family dispatch,
      request/response evidence types, bounded policy, core validation, and test
      support; it does not claim actual-runtime parity or renderer behavior.

  ```bash
  cargo test-fastly integrations::aps
  cargo fmt --all -- --check
  git add crates/trusted-server-core/src/integrations/aps.rs crates/trusted-server-core/src/integrations/mod.rs crates/trusted-server-core/src/integrations/registry.rs crates/trusted-server-core/src/platform/http.rs crates/trusted-server-core/src/platform/mod.rs crates/trusted-server-core/src/platform/test_support.rs crates/trusted-server-core/src/platform/types.rs
  git commit -m "Define the bounded APS runner proxy contract"
  ```

#### Task 5B: Implement and attest all four actual adapter transports

**Task 5B files:**

- `crates/trusted-server-core/src/integrations/aps.rs`
- `crates/trusted-server-core/src/integrations/registry.rs`
- `crates/trusted-server-adapter-fastly/Cargo.toml`
- `crates/trusted-server-adapter-fastly/src/app.rs`
- `crates/trusted-server-adapter-fastly/src/main.rs`
- `crates/trusted-server-adapter-fastly/src/middleware.rs`
- `crates/trusted-server-adapter-fastly/src/platform.rs`
- `crates/trusted-server-adapter-axum/src/app.rs`
- `crates/trusted-server-adapter-axum/src/main.rs`
- `crates/trusted-server-adapter-axum/src/middleware.rs`
- `crates/trusted-server-adapter-axum/src/platform.rs`
- `crates/trusted-server-adapter-axum/tests/routes.rs`
- `crates/trusted-server-adapter-cloudflare/Cargo.toml`
- `crates/trusted-server-adapter-cloudflare/build.sh`
- `crates/trusted-server-adapter-cloudflare/src/app.rs`
- `crates/trusted-server-adapter-cloudflare/src/lib.rs`
- `crates/trusted-server-adapter-cloudflare/src/middleware.rs`
- `crates/trusted-server-adapter-cloudflare/src/platform.rs`
- `crates/trusted-server-adapter-cloudflare/tests/routes.rs`
- `crates/trusted-server-adapter-cloudflare/wrangler.aps-runner-proxy.toml`
- `crates/trusted-server-adapter-spin/Cargo.toml`
- `crates/trusted-server-adapter-spin/src/app.rs`
- `crates/trusted-server-adapter-spin/src/lib.rs`
- `crates/trusted-server-adapter-spin/src/middleware.rs`
- `crates/trusted-server-adapter-spin/src/platform.rs`
- `crates/trusted-server-adapter-spin/tests/routes.rs`
- `crates/trusted-server-integration-tests/fixtures/configs/spin-aps-runner-proxy.toml`
- `crates/trusted-server-integration-tests/fixtures/configs/cloudflare-aps-runner-proxy-fixture.toml`
- `crates/trusted-server-integration-tests/fixtures/cloudflare/aps-runner-proxy-service.js`
- `crates/trusted-server-integration-tests/Cargo.toml`
- `crates/trusted-server-integration-tests/README.md`
- `crates/trusted-server-integration-tests/tests/aps_runner_proxy.rs`
- `crates/trusted-server-integration-tests/tests/common/aps_runner_upstream.rs`
- `crates/trusted-server-integration-tests/tests/common/mod.rs`
- `crates/trusted-server-integration-tests/tests/common/runtime.rs`
- `crates/trusted-server-integration-tests/tests/environments/axum.rs`
- `crates/trusted-server-integration-tests/tests/environments/spin.rs`
- `crates/trusted-server-integration-tests/tests/environments/mod.rs`
- `crates/trusted-server-integration-tests/tests/environments/cloudflare.rs`
- `crates/trusted-server-integration-tests/tests/environments/fastly.rs`
- `crates/trusted-server-integration-tests/tests/parity.rs`
- `scripts/integration-tests-aps-runner-proxy.sh`
- `scripts/integration-tests-browser.sh`
- `scripts/integration-tests.sh`
- `.github/workflows/integration-tests.yml`
- `.tool-versions`
- `Cargo.lock`
- `CLAUDE.md`

- [ ] **Step B1: Write the failing actual-adapter route and proxy corpus.** Cover enabled
      `GET /integrations/aps/runner.js`; APS-disabled local `404 no-store`; negative
      `/integrations/aps/runner/v1.js` and malformed family paths; `405` plus
      `Allow: GET`; and proof that no reserved path reaches publisher auth, EC, or
      fallback through Fastly, Axum, Cloudflare, or Spin.

  Drive each real transport boundary—including Cloudflare and Spin wasm and full
  Fastly routes—against a controlled fictional upstream. Cover status other than
  200; redirects; stall and slow-drip total deadlines; late data after cancellation;
  absent/duplicate/malformed/mismatched/over-limit `Content-Length`;
  absent/identity/listed/other `Content-Encoding`; missing/duplicate/parameterized/
  rejected `Content-Type`; invalid UTF-8; exactly-at and one-byte-over 8 MiB bodies;
  buffered and streamed overflow; byte-preserving success; stripped upstream
  cookies/headers; and empty non-leaking `502 no-store` failures. Do not inject an
  already-normalized core response in place of an adapter transport.

  Build a hermetic `aps_runner_proxy` runtime test, not a core fake. Its private
  control socket selects the next fictional upstream response out-of-band; browser
  requests cannot choose a scenario or target. The request entering core always
  carries the compile-time APS logical URL. An integration-test-only platform
  resolver maps only that exact logical origin to the loopback fixture below target
  validation and immediately above the real adapter transport. Fastly uses a
  generated static Viceroy backend with exact 4,000 ms first-byte and 250 ms
  between-bytes timeouts. That static selection is simulator-only: production keeps
  using Fastly's dynamic backend API with the same transport policy. Cloudflare uses
  a workerd service binding in the dedicated Wrangler manifest, Spin uses the
  dedicated Spin manifest, and Axum uses its real bounded client. Production
  constructors expose no resolver/override, and release-build absence tests fail if
  the integration feature, fixture address, or service binding is enabled or
  embedded.

  `scripts/integration-tests-aps-runner-proxy.sh` builds the actual Fastly
  `wasm32-wasip1`, Cloudflare `wasm32-unknown-unknown`, and Spin `wasm32-wasip1`
  artifacts with that integration-only transport seam; launches Viceroy,
  `wrangler dev`/workerd, and `spin up`; waits for readiness; runs the identical
  `aps_runner_proxy` corpus against each runtime with `--test-threads=1`; and always
  terminates process groups. The test asserts that the request entering each raw
  adapter transport still carries the compile-time logical URL and authority of the
  fixed APS target. Where the runtime supports a backend/resolver authority override,
  the fixture also asserts the wire `Host` is the APS host. A runtime-owned transport
  such as Spin/Wasmtime that forbids guest `Host` writes must not synthesize one: its
  integration-only seam instead validates the exact logical APS URL immediately
  before lowering to the private loopback URI, carries a test-only logical-target
  attestation to the fixture, and proves that neither an inbound request nor any
  production configuration can select the loopback target. The corpus also proves the
  fixture is loopback-only and that actual runtime header normalization,
  cancellation/resource drop, streaming cap, and dispatch-through-final-byte clock
  produce the expected response.

  The Fastly corpus requires Viceroy `0.19.0`. Diagnostics showed that its dynamic
  backend path did not interrupt a mid-body stall even with the configured
  between-bytes timeout, while a generated static backend with the exact 4,000/250 ms
  policy did. The corpus therefore uses that feature-only static backend; production
  continues to use Fastly's dynamic backend API and the same policy. This is a pin
  and transport seam for the local Fastly simulator only; it is not an APS runner
  pin, and no APS runner version, digest, or body enters the repository.

- [ ] **Step B2: Run the new adapter route/corpus tests and prove they fail before implementation.**

  ```bash
  cargo test-fastly
  cargo test-axum --test routes
  cargo test-cloudflare --test routes
  cargo test-spin --test routes
  cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test parity
  ./scripts/integration-tests-aps-runner-proxy.sh --runtime axum
  ./scripts/integration-tests-aps-runner-proxy.sh --runtime fastly
  ./scripts/integration-tests-aps-runner-proxy.sh --runtime cloudflare
  ./scripts/integration-tests-aps-runner-proxy.sh --runtime spin
  ```

  Expected: each runtime is missing its raw transport and/or reserved live-runner
  dispatch; no Task 5C static-renderer assertion is part of this red gate.

- [ ] **Step B3: Implement each actual adapter transport, the reserved dispatcher, and the live**
      **proxy response.**

  Register the family ahead of auth/EC/fallback through one explicit test-only
  registry constructor used by unit tests and the dedicated integration artifacts.
  `scripts/integration-tests-browser.sh` accepts `TS_TEST_APS_V1=1` only to build that
  non-release artifact; ordinary production dispatch stays unchanged until Task 19.
  CI/release absence checks reject the integration feature/sentinel in a production
  bundle, so this cannot become a hidden dual route.
  Accept only status 200, the exact closed content-type/encoding/content-length
  grammars from spec §3.6, a body within 8 MiB, and exact UTF-8 bytes. Relay accepted
  bytes unchanged while replacing all headers with exactly the specified
  JavaScript content type, wildcard CORS, cross-origin CORP, `nosniff`, and
  no-referrer policy. Every upstream or validation failure returns a local empty
  `502 no-store`, with no vendor body or descriptor/capability data in logs.

- [ ] **Step B4: Run and commit adapter transport parity before adding the static renderer.**

  ```bash
  cargo test-fastly
  cargo test-axum
  cargo test-cloudflare
  cargo test-spin
  cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test parity
  ./scripts/integration-tests-aps-runner-proxy.sh --runtime axum
  ./scripts/integration-tests-aps-runner-proxy.sh --runtime fastly
  ./scripts/integration-tests-aps-runner-proxy.sh --runtime cloudflare
  ./scripts/integration-tests-aps-runner-proxy.sh --runtime spin
  git add crates/trusted-server-core/src/integrations/aps.rs crates/trusted-server-core/src/integrations/registry.rs
  git add crates/trusted-server-adapter-fastly/Cargo.toml crates/trusted-server-adapter-fastly/src/app.rs crates/trusted-server-adapter-fastly/src/main.rs crates/trusted-server-adapter-fastly/src/middleware.rs crates/trusted-server-adapter-fastly/src/platform.rs
  git add crates/trusted-server-adapter-axum/src/app.rs crates/trusted-server-adapter-axum/src/main.rs crates/trusted-server-adapter-axum/src/middleware.rs crates/trusted-server-adapter-axum/src/platform.rs crates/trusted-server-adapter-axum/tests/routes.rs
  git add crates/trusted-server-adapter-cloudflare/Cargo.toml crates/trusted-server-adapter-cloudflare/build.sh crates/trusted-server-adapter-cloudflare/src/app.rs crates/trusted-server-adapter-cloudflare/src/lib.rs crates/trusted-server-adapter-cloudflare/src/middleware.rs crates/trusted-server-adapter-cloudflare/src/platform.rs crates/trusted-server-adapter-cloudflare/tests/routes.rs crates/trusted-server-adapter-cloudflare/wrangler.aps-runner-proxy.toml
  git add crates/trusted-server-adapter-spin/Cargo.toml crates/trusted-server-adapter-spin/src/app.rs crates/trusted-server-adapter-spin/src/lib.rs crates/trusted-server-adapter-spin/src/middleware.rs crates/trusted-server-adapter-spin/src/platform.rs crates/trusted-server-adapter-spin/tests/routes.rs
  git add crates/trusted-server-integration-tests/fixtures/configs/spin-aps-runner-proxy.toml crates/trusted-server-integration-tests/fixtures/configs/cloudflare-aps-runner-proxy-fixture.toml crates/trusted-server-integration-tests/fixtures/cloudflare/aps-runner-proxy-service.js crates/trusted-server-integration-tests/Cargo.toml crates/trusted-server-integration-tests/README.md
  git add crates/trusted-server-integration-tests/tests/aps_runner_proxy.rs crates/trusted-server-integration-tests/tests/common/aps_runner_upstream.rs crates/trusted-server-integration-tests/tests/common/mod.rs crates/trusted-server-integration-tests/tests/common/runtime.rs crates/trusted-server-integration-tests/tests/environments/axum.rs crates/trusted-server-integration-tests/tests/environments/spin.rs crates/trusted-server-integration-tests/tests/environments/mod.rs crates/trusted-server-integration-tests/tests/environments/cloudflare.rs crates/trusted-server-integration-tests/tests/environments/fastly.rs crates/trusted-server-integration-tests/tests/parity.rs
  git add scripts/integration-tests-aps-runner-proxy.sh scripts/integration-tests-browser.sh scripts/integration-tests.sh .github/workflows/integration-tests.yml .tool-versions Cargo.lock CLAUDE.md
  git commit -m "Implement APS runner proxy parity"
  ```

#### Task 5C: Implement the static renderer and fictional browser fixture

**Task 5C files:**

- `crates/trusted-server-core/src/integrations/aps.rs`
- `crates/trusted-server-core/src/integrations/registry.rs`
- `crates/trusted-server-adapter-fastly/src/app.rs`
- `crates/trusted-server-adapter-axum/tests/routes.rs`
- `crates/trusted-server-adapter-cloudflare/tests/routes.rs`
- `crates/trusted-server-adapter-spin/tests/routes.rs`
- `crates/trusted-server-integration-tests/tests/parity.rs`
- `crates/trusted-server-integration-tests/browser/tests/shared/aps-renderer.spec.ts`
- `crates/trusted-server-integration-tests/browser/fixtures/fictional-aps-runner.js`
- `docs/guide/error-reference.md`
- `docs/guide/getting-started.md`
- `docs/guide/testing.md`

- [ ] **Step C1: Write failing static-renderer route and policy tests.** Cover
      `/integrations/aps/renderer/v1`, disabled and unknown-version local
      `404 no-store`, malformed family paths, `405` plus `Allow: GET`, and proof the
      route cannot reach publisher auth, EC, or fallback. Assert exact body bytes,
      ordered sandbox tokens, CSP, content type, immutable cache policy, `nosniff`,
      referrer policy, and deliberate absence of `X-Frame-Options` and CSP
      `frame-ancestors`.

- [ ] **Step C2: Run the static-renderer tests and prove they fail before implementation.**

  ```bash
  cargo test-fastly integrations::aps
  cargo test-axum --test routes
  cargo test-cloudflare --test routes
  cargo test-spin --test routes
  cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test parity
  npm --prefix crates/trusted-server-js/lib run check:aps-contract
  node --test crates/trusted-server-js/lib/test/contract/aps-renderer-es5.test.mjs
  TS_TEST_APS_V1=1 TS_BROWSER_FRAMEWORKS=nextjs TS_BROWSER_PROJECTS=chromium \
    ./scripts/integration-tests-browser.sh \
      tests/shared/aps-renderer.spec.ts --project=chromium
  ```

  Expected: the versioned renderer route/body/policy or renderer-document behavior
  is missing while the already-committed live proxy remains green.

- [ ] **Step C3: Implement and test the static renderer contract.**

  The renderer validates/clears the fragment nonce, accepts one exact source-bound
  parent port, validates the descriptor and kernel-captured publisher origin, and
  resolves only its own absolute `/integrations/aps/runner.js`. Before loading the
  script, queue the exact `prebid/creative/render` event with one-shot
  `resolve`/`reject`. Set only `crossOrigin='anonymous'` and
  `referrerPolicy='no-referrer'`; do not set integrity/SRI. Script `load` is
  nonterminal progress. Proxy/CORS/script-load failure reports `runner_no_load`;
  callback rejection reports `runner_failed`; callback success reports completion.
  The renderer starts no completion timer—the kernel owns the only ten-second timer
  from document acceptance. Mutable APS callback correctness is an accepted external
  trust dependency, not a fact TS can derive from script load or body inspection.

- [ ] **Step C4: Add the hermetic fictional runner fixture.**

  Author a minimal local fixture that implements only the documented event and
  queue/resolve/reject behavior. Assert it is neither a copy, transformation, nor
  derivative of APS bytes. Use it for deterministic success, rejection, script-load,
  callback-silence, nested-iframe, and duplicate-callback tests. The fixture is not
  served as a production fallback and cannot be included in release bundles.

- [ ] **Step C5: Run the full route, transport, parity, and browser checks.**

  ```bash
  cargo test-fastly
  cargo test-axum
  cargo test-cloudflare
  cargo test-spin
  cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test parity
  ./scripts/integration-tests-aps-runner-proxy.sh --runtime axum
  ./scripts/integration-tests-aps-runner-proxy.sh --runtime fastly
  ./scripts/integration-tests-aps-runner-proxy.sh --runtime cloudflare
  ./scripts/integration-tests-aps-runner-proxy.sh --runtime spin
  TS_TEST_APS_V1=1 TS_BROWSER_FRAMEWORKS=nextjs TS_BROWSER_PROJECTS=chromium \
    ./scripts/integration-tests-browser.sh \
      tests/shared/aps-renderer.spec.ts --project=chromium
  ```

- [ ] **Step C6: Commit only the static renderer and fictional browser fixture after C1-C5**
      are green. Adapter transport files must already be clean from Task 5B.

  ```bash
  git add crates/trusted-server-core/src/integrations/aps.rs crates/trusted-server-core/src/integrations/registry.rs crates/trusted-server-adapter-fastly/src/app.rs crates/trusted-server-adapter-axum/tests/routes.rs crates/trusted-server-adapter-cloudflare/tests/routes.rs crates/trusted-server-adapter-spin/tests/routes.rs crates/trusted-server-integration-tests/tests/parity.rs crates/trusted-server-integration-tests/browser/tests/shared/aps-renderer.spec.ts crates/trusted-server-integration-tests/browser/fixtures/fictional-aps-runner.js docs/guide/error-reference.md docs/guide/getting-started.md docs/guide/testing.md
  git commit -m "Implement the static APS renderer"
  ```

### Phase 1 exit

- Valid APS bids survive mediation and projection without identity loss.
- Invalid bids fail with exact local reasons.
- Test-only adapter registries prove the same secure static renderer and live runner
  proxy behavior through every actual transport without activating a dual production
  route.
- Existing non-APS auction and publisher tests remain green.

## Phase 2 — build one TSJS runtime

### Task 6: Enforce layering and external-global ownership

**Files:**

- Modify: `crates/trusted-server-js/lib/eslint.config.js`
- Modify: `crates/trusted-server-js/lib/package-lock.json`
- Modify: `crates/trusted-server-js/lib/package.json`
- Modify: `crates/trusted-server-js/lib/vitest.config.ts`
- Create: `crates/trusted-server-js/lib/eslint-rules/no-adtech-globals.js`
- Create: `crates/trusted-server-js/lib/test/eslint/no-adtech-globals.test.mjs`
- Create: `crates/trusted-server-js/lib/src/adapters/googletag.ts`
- Create: `crates/trusted-server-js/lib/src/adapters/prebid.ts`
- Create: `crates/trusted-server-js/lib/src/adapters/messaging.ts`
- Create: `crates/trusted-server-js/lib/src/composition/browser.ts`
- Create: `crates/trusted-server-js/lib/src/composition/browser_test.ts`
- Create: `crates/trusted-server-js/lib/test/composition/browser.test.ts`

- [ ] **Step 1: Add failing lint-rule tests for direct and aliased access through `window`,**
      `globalThis`, and `self`, plus aliases of `googletag`/`pbjs`. Add allowed cases
      inside adapters and kernel `window.tsjs`/messaging code.

- [ ] **Step 2: Configure `import/no-restricted-paths` for the dependency direction in the**
      source-shape diagram. Resolve imports with the package's TypeScript bundler
      semantics so explicit `.js` specifiers cannot bypass `.ts` boundaries. Add a
      narrow, enumerated temporary allowlist for current
      production files that still violate the target (`core/auction.ts`,
      `core/request.ts`, GPT/Prebid integration files, and the diagnostics observer
      found by the executable lint inventory).
      New files receive no exemption. Check the allowlist into the lint test and make
      Task 22 fail if any entry remains.

- [ ] **Step 3: Add adapter interfaces and no-op/fake constructors before moving behavior. Add a**
      test-only composition root that alone may import all concrete adapters/services.
      Production `composition/browser.ts` remains the minimal core entry, and future
      provider IIFEs register their owned implementations. Kernel files accept
      interfaces and never construct downstream objects. At this task's end
      production remains behaviorally unchanged, while new kernel/service files cannot
      touch GPT or Prebid globals.

  Add the production-metafile assertion now: `browser.ts` may import only the minimal
  core graph, while `browser_test.ts` is excluded from every production entry and may
  import concrete fakes/providers. Keep that assertion green in every later task.

  The messaging interface must support synchronous capture-phase listener
  installation. Reserve that hook as the first reversible core activation so the
  final core can install the TS capability recognizer before integration-module
  activation and before TS-owned GPT/Prebid injection, while leaving no listener live
  during asynchronous preparation.

- [ ] **Step 4: Ensure `.github/workflows/format.yml` continues to run lint and**
      `.github/workflows/test.yml` runs typecheck. Enforcement begins with the narrow
      allowlist and becomes repository-clean in Task 22.

- [ ] **Step 5: Run:**

  ```bash
  node --test crates/trusted-server-js/lib/test/eslint/no-adtech-globals.test.mjs
  npm --prefix crates/trusted-server-js/lib run lint
  npm --prefix crates/trusted-server-js/lib run typecheck
  git status --short
  git add crates/trusted-server-js/lib/eslint.config.js crates/trusted-server-js/lib/package.json crates/trusted-server-js/lib/package-lock.json crates/trusted-server-js/lib/vitest.config.ts crates/trusted-server-js/lib/eslint-rules/no-adtech-globals.js crates/trusted-server-js/lib/test/eslint/no-adtech-globals.test.mjs
  git add crates/trusted-server-js/lib/src/adapters/googletag.ts crates/trusted-server-js/lib/src/adapters/prebid.ts crates/trusted-server-js/lib/src/adapters/messaging.ts crates/trusted-server-js/lib/src/composition/browser.ts crates/trusted-server-js/lib/src/composition/browser_test.ts crates/trusted-server-js/lib/test/composition/browser.test.ts
  git commit -m "Enforce TSJS layering and global ownership"
  ```

### Task 7: Implement disposal, terminal latch, and transactional integration modules

**Files:**

- Create: `crates/trusted-server-js/lib/src/kernel/disposable.ts`
- Create: `crates/trusted-server-js/lib/src/kernel/integration_registry.ts`
- Create: `crates/trusted-server-js/lib/test/kernel/disposable.test.ts`
- Create: `crates/trusted-server-js/lib/test/kernel/integration_registry.test.ts`
- Modify: `crates/trusted-server-js/lib/src/core/types.ts`

- [ ] **Step 1: Write failing fake-timer tests for reverse-order exactly-once disposal, disposer**
      failure isolation, registration after disposal, first-terminal-wins, duplicate
      integration id, malformed/unknown manifest id, wrong 64-hex release, pending
      catalog-derived 14-critical/20-total capacity, prepare throw/async rejection/abort, detached continuation,
      activation throw, duplicate `afterCommit`, shared deadline abort, and late
      continuation.

- [ ] **Step 2: Implement `DisposableStack` without relying on a browser proposal unavailable at**
      the configured target. It must be synchronous at ownership boundaries; async side
      work can observe its signal but cannot delay terminal disposal.

- [ ] **Step 3: Implement the exact phase-aware `BootManifestV1` contract and release-internal**
      `_registerIntegration({abi:1,id,phase,releaseId,prepare})` collection. Critical
      entries participate in the atomic bootstrap; deferred entries have only the
      fixed `first_display_or_idle` trigger and independent later transactions.
      Registration executes no module code. In critical manifest order, core awaits
      each returned preparation Promise. A deferred module prepares independently
      only after its own accepted registration plus load checkpoint and never awaits
      a sibling. Preparation may validate frozen config,
      acquire interfaces, allocate inert private data, and register private-memory
      disposers, but cannot touch globals/DOM, attach wrappers/listeners, inject/load,
      start timers/network, invoke publisher code/stateful adapters, or detach work.
      Quarantine wrong-release, unexpected, duplicate, and post-fallback bundles
      before calling `prepare`.

- [ ] **Step 4: Implement the synchronous activation barrier.** A prepared module
      returns one synchronous `activate(ctx)`. Activation may install only
      compare-restorable wrappers/listeners/observers/subscriptions, registering each
      disposer before mutation. It may stage at most one `afterCommit` callback and
      cannot yield, inject/load, start timers/network, drain a queue, or invoke
      publisher callbacks. Activate in manifest order; on throw, unwind every
      activated/prepared module in reverse order before fallback. Check the shared
      monotonic deadline before and after every activation and immediately before
      handoff: 9,999 ms may commit; 10,000/10,001 ms must unwind even if the timer task
      has not run. Document/test that a permanently nonreturning same-thread function
      is unpreemptable. A second `afterCommit` registration throws and becomes
      `bundle_partial`.

- [ ] **Step 5: Test post-commit behavior.** After all activations, publish the full
      API, invoke staged callbacks in manifest order, then drain preload work. An
      `afterCommit` throw disposes only that module, records a bounded local runtime
      failure, and cannot roll back the kernel or create fallback. Race publisher GPT
      calls and script/creative DOM activity before, during, and after a later module
      failure; prove preparation is inert, activation is same-task, and no wrapper,
      guard, script, request, listener, or timer survives a fallback commit.

- [ ] **Step 6: Expose only frozen interfaces; keep mutable maps and owner tokens in closures.**

- [ ] **Step 7: Run:**

  ```bash
  npm --prefix crates/trusted-server-js/lib test -- --run test/kernel/disposable.test.ts test/kernel/integration_registry.test.ts
  npm --prefix crates/trusted-server-js/lib run typecheck
  ```

- [ ] **Step 8: Stage and commit the transactional registry primitives:**

  ```bash
  git status --short
  git add \
    crates/trusted-server-js/lib/src/kernel/disposable.ts \
    crates/trusted-server-js/lib/src/kernel/integration_registry.ts \
    crates/trusted-server-js/lib/test/kernel/disposable.test.ts \
    crates/trusted-server-js/lib/test/kernel/integration_registry.test.ts \
    crates/trusted-server-js/lib/src/core/types.ts
  git diff --cached --check
  git commit -m "Add transactional TSJS module ownership"
  ```

### Task 8A: Implement the minimal bootstrap controller and terminal fallback, dormant

**Files:**

- Create: `crates/trusted-server-js/lib/src/kernel/runtime.ts`
- Create: `crates/trusted-server-js/lib/test/kernel/runtime.test.ts`
- Modify: `crates/trusted-server-js/lib/src/composition/browser.ts`
- Modify: `crates/trusted-server-js/lib/src/composition/browser_test.ts`
- Modify: `crates/trusted-server-js/lib/test/composition/browser.test.ts`
- Modify: `crates/trusted-server-js/lib/src/core/index.ts`
- Modify: `crates/trusted-server-js/lib/src/core/queue.ts`
- Modify: `crates/trusted-server-js/lib/src/core/log.ts`
- Modify: `crates/trusted-server-js/lib/src/core/global.d.ts`
- Create: `crates/trusted-server-js/lib/test/core/queue.test.ts`
- Create: `crates/trusted-server-js/lib/test/core/log.test.ts`
- Create: `crates/trusted-server-js/lib/src/core/bootstrap_controller.ts`
- Create: `crates/trusted-server-js/lib/test/core/bootstrap_controller.test.ts`
- Delete after migrating its tests: `crates/trusted-server-js/lib/src/integrations/gpt/bootstrap_fallback.ts`
- Modify: `crates/trusted-server-core/src/integrations/gpt.rs`
- Modify: `crates/trusted-server-core/src/tsjs.rs`
- Modify: `crates/trusted-server-core/src/publisher.rs`
- Modify: `crates/trusted-server-js/lib/build-all.mjs`
- Modify: `crates/trusted-server-js/lib/package.json`
- Create: `crates/trusted-server-js/lib/scripts/print-release-id.mjs`
- Modify: `crates/trusted-server-js/build.rs`
- Modify: `crates/trusted-server-js/src/bundle.rs`
- Modify: `crates/trusted-server-js/src/lib.rs`

- [ ] **Step 1: Add failing tests for field-wise initialization, existing publisher queue/config,**
      two core loads, invalid manifests, wrong/duplicate/unknown integrations,
      prepare/activate/after-commit failure at every checkpoint, a missing or hung
      integration, the shared watchdog and monotonic activation checks,
      owner-generation mismatch, late continuation after fallback, bundle after
      fallback, and exactly-one committed owner. At each failure checkpoint assert the
      exact immutable `abi_mismatch | bundle_partial` reason; FIFO draining despite a
      throwing callback; queued and later `requestAds`; already-aborted signals; late
      integration refusal without invoking module code; and zero runtime/service/adapter/
      listener/timer/port/iframe construction after the fallback commits. The
      bootstrap owns only queue ingress, immutable boot/manifest inputs, generation,
      the ten-second critical watchdog, the registration sink, the
      `tsjs:bids-script` mark, and fallback; assert it has no adapter, service,
      renderer, DOM scan, upstream loader, or publisher-callback execution path.

  Add exact critical-tag authentication cases. The server emits one parser-inserted
  `script#trustedserver-js` immediately after the controller. The controller resolves
  root-relative `criticalSrc` exactly once against its trusted document origin,
  never `document.baseURI`, requires same origin plus exact
  path/query round-trip, and compares that canonical absolute value to the unique
  element's resolved `src`. Missing/duplicate id, publisher-created replacement,
  detached node, changed `src`, base-element influence, origin/path/query mismatch,
  and any redirected local route fail the critical transaction as `abi_mismatch`;
  no module registration or fallback runtime construction may occur first.

  For an ordinary document the trusted origin is the captured exact HTTP(S)
  `window.location.origin`. Add the sandboxed-creative exception in the existing
  bootstrap/core-owned fallback boundary: only when the document origin is `"null"`,
  accept the exact own non-enumerable/non-configurable/non-writable data stamp
  `window.__tsCreativeOrigin` written before bidder markup. Reject missing,
  inherited, accessor-backed, mutable, enumerable, credentialed, and non-origin
  stamps. Use this same helper for critical-source capture and registry
  current-script ownership; move the shared failure-reason type to that boundary so
  the registry imports it without a fallback/registry module cycle. Do not create a
  new uncaptured production-source ownership entry.

  Exercise the queue boundary as a real Array: pushes before/during/at activation and
  commit, retained ingress references, snapshot-versus-forward exactly once, nested
  push ordering, `this === tsjs`, throw isolation, and non-callables. The final queue
  has `length:0`, its own immediate `push`, and is frozen; native/borrowed mutators,
  index/length assignment, deletion, and property definition in strict/sloppy callers
  cannot retain work or change length.

- [ ] **Step 2: Implement `unclaimed → installing → kernel` and**
      `installing → failed → fallback`. Start the only ten-second watchdog immediately
      before core injection; it covers registration, preparation, and activation for
      every enabled critical integration only. Combine the timer with `performance.now()`/the
      injected monotonic clock checks before/after every activation and before
      handoff. Abort completes synchronous reverse-order unwind before fallback.
      Deferred bundles after fallback are rejected and cannot replace that generation.
      Exercise this through the test-only composition harness; do not yet replace the
      production bootstrap.

  Normalize `tsjs.que` to an actual ingress Array with a writable-false,
  configurable-true property during preparation. After successful activation (or
  after unwind for fallback), perform the spec §5.3 six-step synchronous handoff:
  construct/freeze the final real Array, snapshot callable own data entries, clear
  ingress and replace its `push` with a forwarder, redefine the public property
  non-writable/non-configurable with all committed fields, run kernel `afterCommit`
  callbacks, then drain the snapshot. No task/microtask may split the steps.

  The fallback is one terminal non-rendering shell, never a reduced runtime. Before
  draining the queue it installs the final `requestAds` validator, immediate queue,
  permanently refusing `_registerIntegration`, exact `version:'1.0.0'`/`releaseId`,
  validating-then-refusing `addAdUnits`, local logger, immutable safe boot value, and
  frozen non-enumerable `_internal` fallback record. It
  constructs no runtime, registry, adapter, bridge, timer, listener, port, or
  iframe. Batch membership comes only from exact server slot ids in the immutable
  boot auction projection. Retain it only when the full §3.1–3.2 shape, grammar,
  256-slot, dimension, and 8 MiB bounds pass; otherwise substitute exactly
  `{version:1,auction:{version:1,auctionId:'fallback',results:[]},slots:[],bids:[]}`
  plus creative/diagnostics disabled safe defaults. Assert that this replacement has
  exactly those four own keys and that each nested object/array has only its specified
  own keys. Known members resolve with the boot failure, unknown ids resolve
  `slot_unresolved`, aborted known members cancel, and an omitted empty projection
  resolves `slots:[]`. No valid call remains pending.

- [ ] **Step 3: Make `build-all.mjs` emit bootstrap, core, and every production module with one fixed sentinel, compute 64**
      lowercase SHA-256 hex over a length-framed canonical ordered inventory tuple
      for every artifact containing its exact `id`, `role`, `phase`, fixed trigger,
      and sentinel-normalized bytes. The byte preimage is ASCII
      `tsjs-release-v1\0`, then an unsigned 64-bit big-endian artifact count, then—per
      catalog-ordered artifact—exactly five fields in order: UTF-8 `id`, UTF-8
      `role`, UTF-8 `phase`, UTF-8 `trigger`, and raw sentinel-normalized bundle
      bytes. Prefix every field with its unsigned 64-bit big-endian byte length;
      encode a non-applicable phase or trigger as a zero-length field. Hash no file
      path, host separator, JSON rendering, platform newline, or final-byte digest.
      Replace exactly one sentinel per bundle and verify none remains. Independently
      test id-only, role-only,
      phase/trigger-only, order-only, and byte-only changes, plus framing ambiguity;
      every one must change `releaseId`. Embed
      that release id in every bundle. Write exact generated
      `crates/trusted-server-js/dist/tsjs-release-v1.json` with each artifact's exact
      `{id,role,phase,trigger,inputs,outputs,file,bytes,hash}` in canonical order; add
      `npm run --silent print:release-id` to validate that file and print only its
      64-hex id. Extend `build.rs` generated metadata to include
      the sentinel-normalized all-bundle `release_id`; expose it through `bundle.rs` and
      the crate API beside bundle bytes/content hashes. Add Rust tests proving generated
      metadata and every bundle carry the same id. Implement the pure exact
      `BootManifestV1` serializer as
      `{version:1,releaseId,criticalSrc,integrations:[{id,phase:'critical'} | {id,phase:'deferred',trigger:'first_display_or_idle',src}]}`
      in canonical injection order, but do not emit it into production HTML yet.
      Test changed logical bytes, role/phase/order changes, sentinel multiplicity,
      wrong release, missing integration, bad `criticalSrc`, and server/bundle
      disagreement.

- [ ] **Step 4: Install one `Runtime`; its closure keeps the mutable capability broker**
      and owner tokens, provider IIFEs register owned implementations, and consumers
      obtain frozen interfaces only through exact-release preparation/activation
      contexts. Only the test composition may construct every concrete provider.
      `tsjs._internal` is non-enumerable frozen status data only—never the registry.

  In test-only composition, activate the capture-phase bridge recognizer as the first
  reversible core effect, followed by correctness GPT listeners and prepared modules.
  Production bootstrap and manifest emission remain unchanged until Task 19.

  Implement the critical-tag lookup/capture before arming its load/error handlers,
  store that exact node identity for every critical registration, and reject a
  different `document.currentScript` or node that no longer matches the authenticated
  id/source. Pair these browser tests with Task 8B's no-redirect local transport test;
  do not infer redirect safety from `script.src` alone.

- [ ] **Step 5: Generate the inline controller and fallback from one TypeScript source**
      and test their bytes and terminal behavior at every checkpoint. Delete the old
      behavior-bearing `gpt_bootstrap.js` at the production switch. Pin the generated
      artifact with a staleness assertion. Report 19,101 raw / 5,468 gzip / 4,632
      Brotli and their former 5% figures as historical old-artifact evidence only;
      Task 18E captures and gates the generated post-split bootstrap artifact with the
      role-correct formula. There is never a hand-maintained second fallback or a
      degraded GPT runtime.

- [ ] **Step 6: Run:**

  ```bash
  npm --prefix crates/trusted-server-js/lib test -- --run test/kernel/runtime.test.ts test/core/bootstrap_controller.test.ts
  cargo test-fastly release_id
  cargo test-fastly
  npm --prefix crates/trusted-server-js/lib run build
  ```

- [ ] **Step 7: Stage and commit the dormant controller, fallback, and release identity**
      together. Include the migrated fallback deletion so no behavior-bearing orphan
      remains outside the commit:

  ```bash
  git status --short
  git add \
    crates/trusted-server-js/lib/src/kernel/runtime.ts \
    crates/trusted-server-js/lib/test/kernel/runtime.test.ts \
    crates/trusted-server-js/lib/src/composition/browser.ts \
    crates/trusted-server-js/lib/src/composition/browser_test.ts \
    crates/trusted-server-js/lib/test/composition/browser.test.ts \
    crates/trusted-server-js/lib/src/core/index.ts \
    crates/trusted-server-js/lib/src/core/queue.ts \
    crates/trusted-server-js/lib/src/core/log.ts \
    crates/trusted-server-js/lib/src/core/global.d.ts \
    crates/trusted-server-js/lib/test/core/queue.test.ts \
    crates/trusted-server-js/lib/test/core/log.test.ts \
    crates/trusted-server-js/lib/src/core/bootstrap_controller.ts \
    crates/trusted-server-js/lib/test/core/bootstrap_controller.test.ts \
    crates/trusted-server-js/lib/src/integrations/gpt/bootstrap_fallback.ts \
    crates/trusted-server-core/src/integrations/gpt.rs \
    crates/trusted-server-core/src/tsjs.rs \
    crates/trusted-server-core/src/publisher.rs \
    crates/trusted-server-js/lib/build-all.mjs \
    crates/trusted-server-js/lib/package.json \
    crates/trusted-server-js/lib/scripts/print-release-id.mjs \
    crates/trusted-server-js/build.rs \
    crates/trusted-server-js/src/bundle.rs \
    crates/trusted-server-js/src/lib.rs
  git diff --cached --check
  git commit -m "Add the terminal TSJS bootstrap controller"
  ```

### Task 8B: Build the canonical release catalog and exact TSJS artifact transport

**Files:**

- Create: `crates/trusted-server-js/lib/src/kernel/release_catalog.ts`
- Create: `crates/trusted-server-js/lib/test/kernel/release_catalog.test.ts`
- Modify: `crates/trusted-server-js/lib/build-all.mjs`
- Modify: `crates/trusted-server-js/lib/test/build/release-v1.test.mjs`
- Modify: `crates/trusted-server-js/src/bundle.rs`
- Modify: `crates/trusted-server-js/src/lib.rs`
- Modify: `crates/trusted-server-core/src/tsjs.rs`
- Modify: `crates/trusted-server-core/src/publisher.rs`
- Modify: `crates/trusted-server-core/src/html_processor.rs`
- Modify: `crates/trusted-server-adapter-fastly/src/app.rs`
- Modify: `crates/trusted-server-adapter-axum/src/app.rs`
- Modify: `crates/trusted-server-adapter-cloudflare/src/app.rs`
- Modify: `crates/trusted-server-adapter-spin/src/app.rs`
- Modify: `crates/trusted-server-adapter-fastly/src/app.rs` test module
- Modify: `crates/trusted-server-adapter-axum/tests/routes.rs`
- Modify: `crates/trusted-server-adapter-cloudflare/tests/routes.rs`
- Modify: `crates/trusted-server-adapter-spin/tests/routes.rs`

- [ ] **Step 1: Write the failing catalog and inventory tests.** Encode the 20 rows
      and exact order from spec §5.2, including all inclusion predicates, provided and
      consumed capability keys, phase/trigger, and named obligation. Assert
      `MAX_CRITICAL_MODULES = 14`, `MAX_MANIFEST_MODULES = 20`, boundary cases
      13/14/15 and 19/20/21, exactly one provider per non-kernel capability,
      provider-before-consumer order, no deferred provider, no cycle, no unknown
      production artifact, and no phase override. The mandatory minimal critical set
      is `[core, render_runtime]`; the reference set is
      `[core, render_runtime, creative, gpt, prebid, datadome]`.

  Assert the exact diagnostics edges rather than a generic bus relationship:
  `render_runtime` provides both `trace.v1` and the distinct
  `trace.presentation.v1`; `gpt_diagnostics` consumes only `runtime.v1` and
  `gpt.events.v1`; `diagnostics_presentation` alone consumes
  `trace.presentation.v1`, plus `gpt_diag.v1` iff GPT diagnostics is active. Its
  inclusion predicate is exactly
  `renderTraceOverlay || diagnostics.gpt.active`. Prove `attachPresentation` is
  absent from every `trace.v1` consumer projection and that no integration-module
  diagnostics subscription capability or capacity is generated.

- [ ] **Step 2: Run the new catalog/release tests and verify they fail** because the
      existing generated inventory has no roles/phases/capabilities and still uses
      the old all-required/deferred convention.

  ```bash
  npm --prefix crates/trusted-server-js/lib test -- --run test/kernel/release_catalog.test.ts
  npm --prefix crates/trusted-server-js/lib run test:release
  ```

- [ ] **Step 3: Implement the catalog as the single build/server/browser authority.**
      Generate bootstrap, core, 14 possible critical modules, and six deferred slices
      from it. Reject test/fake/no-op outputs, missing or multiply counted artifacts,
      undeclared capability edges, a deferred source in a critical metafile, and any
      server list that is not the catalog-filtered order. Derive manifest capacities
      from the catalog rather than a hand-written 16. Generate no internal diagnostics
      subscriber constant: the core ingress has no module subscription surface, while
      public trace/GPT subscriber limits remain owned by their respective data APIs.

- [ ] **Step 4: Write failing Rust and adapter tests for exact static transport.** Cover
      only `GET /static/tsjs=tsjs-unified.min.js?v=<criticalHash>` and exact enabled
      deferred paths; one `v`, no extra query; lowercase 64-hex SHA-256 over exact
      uncompressed response bytes; current catalog membership; `200` JavaScript MIME
      plus `nosniff`; existing conditional `304`; and local `404 no-store` for wrong
      method, aliases, unknown/disabled ids, malformed/stale hash, or extra query.
      Assert no redirect and no publisher-origin fallthrough on all four adapters.

- [ ] **Step 5: Implement hash-validating unified/deferred handlers and manifest
      emission.** `criticalSrc` names core plus critical IIFEs in catalog order with
      the exact `;\n` separator. A successful request returns only bytes embedded in
      the active binary; do not add N/N-1 storage, a cache redesign, or a source-map/
      upstream fallback. Preserve the existing strong-ETag/static-cache behavior.

- [ ] **Step 6: Run the focused release and four-adapter route gates, then commit.**

  ```bash
  npm --prefix crates/trusted-server-js/lib run build
  npm --prefix crates/trusted-server-js/lib run test:release
  cargo test-fastly tsjs
  cargo test-axum tsjs
  cargo test-cloudflare tsjs
  cargo test-spin tsjs
  git status --short
  git add crates/trusted-server-js/lib/src/kernel/release_catalog.ts crates/trusted-server-js/lib/test/kernel/release_catalog.test.ts crates/trusted-server-js/lib/build-all.mjs crates/trusted-server-js/lib/test/build/release-v1.test.mjs crates/trusted-server-js/src/bundle.rs crates/trusted-server-js/src/lib.rs crates/trusted-server-core/src/tsjs.rs crates/trusted-server-core/src/publisher.rs crates/trusted-server-core/src/html_processor.rs
  git add crates/trusted-server-adapter-fastly/src/app.rs crates/trusted-server-adapter-axum/src/app.rs crates/trusted-server-adapter-axum/tests/routes.rs crates/trusted-server-adapter-cloudflare/src/app.rs crates/trusted-server-adapter-cloudflare/tests/routes.rs crates/trusted-server-adapter-spin/src/app.rs crates/trusted-server-adapter-spin/tests/routes.rs
  git commit -m "Define the phase-aware TSJS release catalog"
  ```

### Task 8C: Add the protected paint gate and authenticated deferred loader

**Files:**

- Create: `crates/trusted-server-js/lib/src/kernel/phase_loader.ts`
- Create: `crates/trusted-server-js/lib/test/kernel/phase_loader.test.ts`
- Modify: `crates/trusted-server-js/lib/src/kernel/integration_registry.ts`
- Modify: `crates/trusted-server-js/lib/test/kernel/integration_registry.test.ts`
- Modify: `crates/trusted-server-js/lib/src/kernel/runtime.ts`
- Modify: `crates/trusted-server-js/lib/test/kernel/runtime.test.ts`
- Create: `crates/trusted-server-js/lib/src/kernel/sessions.ts`
- Create: `crates/trusted-server-js/lib/test/kernel/sessions.test.ts`
- Modify: `crates/trusted-server-js/lib/src/core/global.d.ts`

- [ ] **Step 1: Write failing unit tests for the exact registrar ABI.** Accept only a
      non-null plain object with exactly five own enumerable data properties
      `{abi:1,id,phase,releaseId,prepare}`. Reject accessors, inherited/unknown/missing
      keys, custom prototypes, wrong literals/types, unknown/cross-phase ids,
      duplicate/late calls, the wrong core-created element, and a different
      `document.currentScript`. Stage declared frozen provider interfaces after
      provider preparation, make them effect-inert until activation, and expose no
      broker/readiness object through public or `_internal` surfaces.

- [ ] **Step 2: Write failing scheduler tests for the only phase gate.** At kernel
      commit arm a 10,000 ms attempt-creation guard on every page. The first attempt-
      creating batch becomes immutable; cancel the guard and wait without another
      cutoff for every member's terminal latch. After terminal/no-attempt, require two
      owned animation frames when visible, or visibility plus two frames versus a
      2,000 ms hidden timeout. Only then schedule `requestIdleCallback({timeout:2000})`
      or the post-gate 50 ms fallback. Assert no request/preload/prepare/evaluation
      precedes `tsjs:first-display-paint`, including 9,999/10,000/10,001 ms edges and
      immediate programmatic direct first display. Treat a first attempt created
      after the 10,000 ms no-attempt release as correct but explicitly outside the
      no-contention guarantee.

- [ ] **Step 3: Implement independent deferred transactions and their two clocks.**
      After the common paint/idle gate, initiate every included classic script in
      manifest order with `async=true` without awaiting a sibling; require exact load
      plus exactly one accepted registration before that module's
      prepare/activate/afterCommit. Each caller keeps
      its original enqueue deadline while the shared module gets one independent
      ten-second trigger-to-commit deadline. One caller expiry cannot abort the
      background/shared load, and one hung/failed module cannot delay another's fetch
      or consume its deadline. Implement the exact state chain and unavailable reasons
      including `policy_blocked`; the catalog forbids deferred-to-deferred capability
      edges, and a deferred failure never selects fallback or a second runtime.

- [ ] **Step 4: Add and implement CSP/Trusted Types fixtures.** Copy only the exact
      critical script's nonempty `nonce` IDL value. Create at most one closure-private
      `trusted-server#tsjs-v1` policy whose `createScriptURL` accepts only frozen exact
      canonical absolute URLs derived once from the validated root-relative manifest
      against captured `window.location.origin`, never `document.baseURI`. Use that
      absolute set for tag/currentScript checks, policy output, and post-assignment
      comparison. If policy creation fails, allow raw assignment only
      through non-enforcement or an existing publisher default policy, then compare
      the resolved element URL exactly before insertion. Cover same-origin policy,
      nonce-only, `strict-dynamic`, allowed/rejected named policy, exact-preserving and
      mutating defaults, throws, absent nonce, disallowed URL, node replacement, and
      full policy block. Reserve `policy_blocked` for synchronous Trusted Types or
      pre-insertion URL failure; CSP nonce/source rejection after insertion is
      `load_error`, and removal/replacement that loses authenticated identity is
      `registration_rejected` unless load failure wins. Never install a
      `SecurityPolicyViolationEvent` listener, rewrite CSP, or try `setAttribute`, dynamic import,
      blob/data URLs, `eval`, another policy name, or a remote fallback.

- [ ] **Step 5: Prove disposal ownership.** Full runtime disposal cancels readiness,
      idle/frame/timer/load handlers, uncommitted nodes, and late registration. SPA
      disposal cancels only navigation-owned waiters/state; it does not abort the
      runtime-owned module transaction or remove a document-runtime provider needed
      by the next navigation. Exercise reversed disposal/load/registration/timeout
      orderings and prove the sole adapter/listener/registry identities survive an
      isolated deferred failure.

- [ ] **Step 6: Run the focused kernel gate, then commit.**

  ```bash
  npm --prefix crates/trusted-server-js/lib test -- --run test/kernel/phase_loader.test.ts test/kernel/integration_registry.test.ts test/kernel/runtime.test.ts test/kernel/sessions.test.ts
  npm --prefix crates/trusted-server-js/lib run lint
  npm --prefix crates/trusted-server-js/lib run typecheck
  git status --short
  git add crates/trusted-server-js/lib/src/kernel/phase_loader.ts crates/trusted-server-js/lib/test/kernel/phase_loader.test.ts crates/trusted-server-js/lib/src/kernel/integration_registry.ts crates/trusted-server-js/lib/test/kernel/integration_registry.test.ts crates/trusted-server-js/lib/src/kernel/runtime.ts crates/trusted-server-js/lib/test/kernel/runtime.test.ts crates/trusted-server-js/lib/src/kernel/sessions.ts crates/trusted-server-js/lib/test/kernel/sessions.test.ts crates/trusted-server-js/lib/src/core/global.d.ts
  git commit -m "Load later TSJS modules after first-display paint"
  ```

### Task 9: Add runtime and navigation sessions

**Files:**

- Modify: `crates/trusted-server-js/lib/src/kernel/sessions.ts`
- Create: `crates/trusted-server-js/lib/src/kernel/identity.ts`
- Modify: `crates/trusted-server-js/lib/test/kernel/sessions.test.ts`
- Create: `crates/trusted-server-js/lib/test/kernel/identity.test.ts`
- Create: `crates/trusted-server-js/lib/src/services/projections.ts`
- Create: `crates/trusted-server-js/lib/test/services/projections.test.ts`
- Create: `crates/trusted-server-js/lib/src/services/context.ts`
- Create: `crates/trusted-server-js/lib/test/services/context.test.ts`
- Modify: `crates/trusted-server-js/lib/src/kernel/runtime.ts`
- Modify: `crates/trusted-server-js/lib/src/composition/browser_test.ts`
- Modify: `crates/trusted-server-js/lib/test/composition/browser.test.ts`

- [ ] **Step 1: Add failing tests for RuntimeSession singleton ownership, navigation replacement,**
      reverse-order disposal inventory, late old-generation callbacks, same DOM id on a
      new navigation, timer/listener/port cleanup, and double disposal.

  Add deterministic-crypto issuer tests proving one `NavigationSession` obtains an
  eight-byte CSPRNG prefix and combines it with one big-endian unsigned 64-bit ordinal
  stored as two u32 words. Each `a1_` encodes those 16 bytes as 22 unpadded base64url
  characters; ordinal increments exactly once, never wraps, and needs no issued-id
  history. Unavailable/throwing crypto or ordinal exhaustion fails
  `identity_generation_failed` before creating work. Separately prove `t1_` and `n1_`
  encode 16 fresh CSPRNG bytes; their eight-draw collision/capacity behavior belongs
  to Tasks 14 and 13. Never pass raw bytes or issued identities to logging/debug
  callbacks.

- [ ] **Step 2: Implement explicit owner APIs:**
  - `RuntimeSession`: injected adapter interfaces and owner/disposer scopes; it
    does not import concrete adapters or services;
  - `NavigationSession`: aliases, intents, targeting ownership, batches, attempts,
    and one internal immutable current auction projection;
  - `AuctionContextRegistry`: runtime-owned, manifest-bounded contributor callbacks
    registered with owner-scoped disposers; it snapshots contributions for one batch,
    preserves manifest registration order and later-key precedence, freezes the
    result, and isolates/logs one contributor throw without retaining its values;
  - child scopes for `AuctionBatch` and `RenderAttempt`.

  Concrete slot maps, physical cycles, reservations, and the bridge listener are
  services constructed only in `composition/browser_test.ts` for pre-switch tests and disposed by the runtime
  scopes through interfaces.

  Seed only the initial session from recursively frozen
  `tsjs.boot.auctionProjection`; never mutate boot. A new SPA session begins with no
  projection. Its page-bids controller deep-copies/freezes one exact current-
  generation `BrowserAuctionProjectionV1`, reserves all projected slots against the
  shared 256 cap (including already-admitted programmatic units), and commits slots +
  projection atomically. Stale/duplicate/malformed/over-cap responses commit nothing
  and never retain prior-navigation data.

- [ ] **Step 3: Every callback captures an owner generation and verifies it before mutation.**
      Provide test-only inventory snapshots; do not expose mutable production state.

  Keep the issuer injectable only through test composition. The production runtime
  provider always uses browser Web Crypto; no `Math.random`, counter, timestamp, publisher
  input, or compatibility-form parser may mint a capability.

- [ ] **Step 4: Run:**

  ```bash
  npm --prefix crates/trusted-server-js/lib test -- --run test/kernel/sessions.test.ts
  npm --prefix crates/trusted-server-js/lib test -- --run test/kernel/identity.test.ts
  npm --prefix crates/trusted-server-js/lib test -- --run test/services/projections.test.ts
  npm --prefix crates/trusted-server-js/lib test -- --run test/services/context.test.ts
  npm --prefix crates/trusted-server-js/lib run typecheck
  ```

- [ ] **Step 5: Stage and commit the session, identity, projection, and context owners:**

  ```bash
  git status --short
  git add \
    crates/trusted-server-js/lib/src/kernel/sessions.ts \
    crates/trusted-server-js/lib/src/kernel/identity.ts \
    crates/trusted-server-js/lib/test/kernel/sessions.test.ts \
    crates/trusted-server-js/lib/test/kernel/identity.test.ts \
    crates/trusted-server-js/lib/src/services/projections.ts \
    crates/trusted-server-js/lib/test/services/projections.test.ts \
    crates/trusted-server-js/lib/src/services/context.ts \
    crates/trusted-server-js/lib/test/services/context.test.ts \
    crates/trusted-server-js/lib/src/kernel/runtime.ts \
    crates/trusted-server-js/lib/src/composition/browser_test.ts \
    crates/trusted-server-js/lib/test/composition/browser.test.ts
  git diff --cached --check
  git commit -m "Add TSJS runtime and navigation ownership"
  ```

### Task 10: Implement bounded adapters and readiness queues

**Files:**

- Modify: `crates/trusted-server-js/lib/src/adapters/googletag.ts`
- Modify: `crates/trusted-server-js/lib/src/adapters/prebid.ts`
- Create: `crates/trusted-server-js/lib/test/adapters/googletag.test.ts`
- Create: `crates/trusted-server-js/lib/test/adapters/prebid.test.ts`
- Modify: `crates/trusted-server-js/lib/src/adapters/messaging.ts`
- Create: `crates/trusted-server-js/lib/test/adapters/messaging.test.ts`
- Modify: `crates/trusted-server-js/lib/src/composition/browser_test.ts`
- Modify: `crates/trusted-server-js/lib/test/composition/browser.test.ts`

- [ ] **Step 1: Add failing tests for `present | pending | timed_out | incompatible`, late external readiness,**
      per-operation timeout/removal, abort/disposal, queue capacity, command throw,
      callback throw, exact message keys/version, zero/one/two ports, and port closure.
      Race readiness immediately before/at/after the exact deadline.

- [ ] **Step 2: Move global access behind injected adapter methods. `timed_out` describes one**
      operation, not a permanent library state; `incompatible` describes only the
      currently bound external object/stamp and later replacement may succeed. Each adapter
      readiness queue holds at most 64 operations. Overflow fails the new operation
      synchronously with `external_queue_full`; timed-out/aborted entries are removed
      immediately and readiness drains live entries FIFO. Each operation expires with
      `external_ready_timeout` exactly ten seconds from enqueue, independent of
      `requestAds.timeoutMs` and the auction-fetch deadline. Dispatch versus expiry
      races through one latch.

- [ ] **Step 3: Centralize all message names, exact-shape guards, safe port extraction, and port**
      disposal in the messaging adapter. Do not put reservation or lifecycle policy in
      the adapter.

- [ ] **Step 4: Run:**

  ```bash
  npm --prefix crates/trusted-server-js/lib test -- --run test/adapters
  npm --prefix crates/trusted-server-js/lib run lint
  npm --prefix crates/trusted-server-js/lib run typecheck
  ```

- [ ] **Step 5: Stage and commit the bounded external adapters:**

  ```bash
  git status --short
  git add \
    crates/trusted-server-js/lib/src/adapters/googletag.ts \
    crates/trusted-server-js/lib/src/adapters/prebid.ts \
    crates/trusted-server-js/lib/test/adapters/googletag.test.ts \
    crates/trusted-server-js/lib/test/adapters/prebid.test.ts \
    crates/trusted-server-js/lib/src/adapters/messaging.ts \
    crates/trusted-server-js/lib/test/adapters/messaging.test.ts \
    crates/trusted-server-js/lib/src/composition/browser_test.ts \
    crates/trusted-server-js/lib/test/composition/browser.test.ts
  git diff --cached --check
  git commit -m "Bound TSJS external adapter readiness"
  ```

### Phase 2 exit

- One runtime and registry exist across separately built IIFEs.
- Bootstrap/fallback ownership is transactional and generation-safe.
- Sessions and adapters own every external global and disposal boundary.
- No APS/GPT/Prebid production behavior has yet been switched without tests.

## Phase 3 — move slot, auction, and render state into services

### Task 11: Implement the slot registry and physical GPT cycle model

**Files:**

- Create: `crates/trusted-server-js/lib/src/services/slots.ts`
- Create: `crates/trusted-server-js/lib/test/services/slots.test.ts`
- Create: `crates/trusted-server-js/lib/src/services/targeting.ts`
- Create: `crates/trusted-server-js/lib/test/services/targeting.test.ts`
- Modify: `crates/trusted-server-js/lib/src/kernel/sessions.ts`
- Modify: `crates/trusted-server-js/lib/src/adapters/googletag.ts`
- Modify: `crates/trusted-server-js/lib/src/composition/browser_test.ts`
- Modify: `crates/trusted-server-js/lib/test/composition/browser.test.ts`

- [ ] **Step 1: Add failing tests for exact nonempty server slot ids at the 256-UTF-8-byte**
      boundary with NUL/control rejection, combined 255/256/257 server plus
      programmatic records, ad-unit codes, DOM aliases, alias
      collision, GPT object identity, SRA, one active/one queued replacement, TS versus
      publisher intent, `display()` under disabled initial load, same/opposite-class
      supersession, ambiguous overlap, duplicate response identifiers, no timeout
      re-arm, safe destroy/redefine, and navigation disposal. For disabled initial load,
      prove TS `display()` only registers and exactly one
      `refresh([slot],{changeCorrelator:false})` owns the intent; refresh unavailability/
      throw fails `gpt_request_failed`, while publisher display stays publisher-owned.
      Include old completion
      after navigation and before/after the replacement completion on the same DOM id.
      Race `slotRequested` immediately before/at/after three seconds and
      `slotRenderEnded` immediately before/at/after ten seconds from request start.

  Cover transactional destroy/redefine at every recovery site: throw/false destroy,
  failed replacement definition, stale generation after define, and proof that no
  second physical GPT slot or binding appears. Destroy failure retires/quarantines
  the old identity and makes later TS work fail `gpt_request_failed` until publisher
  destruction or reload.

- [ ] **Step 2: Implement `SlotRecord`, navigation-local registration ordinals, and indexes.**
      Reject missing/empty/over-limit server ids and exact server-id collisions before
      indexing. Ad-unit and DOM alias resolution must produce exactly one record or
      `slot_unresolved`; never normalize server identity or choose first registration.
      Enforce one `MAX_ACTIVE_SLOT_RECORDS = 256` transaction across server projection
      and later programmatic registration; navigation disposal releases all records.

- [ ] **Step 3: Record intent before the adapter operation. Open a physical cycle only on**
      `slotRequested`; close on `slotRenderEnded`; attribute only when exactly one live
      compatible TS intent exists. Fail ambiguous TS ownership instead of guessing.
      A request-capable GPT call with no `slotRequested` by three seconds fails
      `gpt_request_timeout`; because no attributable cycle exists, immediately
      invoke one adapter transaction that marks the old TS object retired, requires
      successful `destroySlots([old])`, and only then defines/binds a replacement; or
      place a publisher-owned object in permanent
      page-lifetime quarantine until explicit publisher destruction/reload. Future GPT
      events never release that request-timeout quarantine. An opened cycle with no
      completion by ten seconds fails `gpt_completion_timeout`; only its matching real
      completion may drain that exact cycle. Neither timeout starts fallback.

- [ ] **Step 4: Expose narrow operations to integrations: register/adopt slot, record intent,**
      handle GPT event, own/clear targeting, resolve exact slot, dispose navigation.

- [ ] **Step 5: On navigation disposal, destroy/redefine only a TS-owned GPT slot object and keep**
      the retired object in the `WeakMap` until late completion drains. Define a
      replacement only when a current navigation needs it and the transactional
      destroy succeeded. Quarantine an
      open publisher-owned object until its real `slotRenderEnded`, publisher
      destruction, or reload; new TS work fails `slot_quarantined`. No timeout or
      navigation token re-arms a physical cycle.

- [ ] **Step 6: Run:**

  ```bash
  npm --prefix crates/trusted-server-js/lib test -- --run test/services/slots.test.ts
  npm --prefix crates/trusted-server-js/lib test -- --run test/services/targeting.test.ts
  npm --prefix crates/trusted-server-js/lib run typecheck
  ```

- [ ] **Step 7: Stage and commit the slot and physical-cycle service:**

  ```bash
  git status --short
  git add \
    crates/trusted-server-js/lib/src/services/slots.ts \
    crates/trusted-server-js/lib/test/services/slots.test.ts \
    crates/trusted-server-js/lib/src/services/targeting.ts \
    crates/trusted-server-js/lib/test/services/targeting.test.ts \
    crates/trusted-server-js/lib/src/kernel/sessions.ts \
    crates/trusted-server-js/lib/src/adapters/googletag.ts \
    crates/trusted-server-js/lib/src/composition/browser_test.ts \
    crates/trusted-server-js/lib/test/composition/browser.test.ts
  git diff --cached --check
  git commit -m "Model physical GPT slot cycles"
  ```

### Task 12: Implement the bounded renderer reservation store

**Files:**

- Create: `crates/trusted-server-js/lib/src/services/reservations.ts`
- Create: `crates/trusted-server-js/lib/test/services/reservations.test.ts`
- Modify: `crates/trusted-server-js/lib/src/kernel/sessions.ts`
- Modify: `crates/trusted-server-js/lib/src/integrations/aps/render.ts`
- Modify: `crates/trusted-server-js/lib/src/composition/browser_test.ts`
- Modify: `crates/trusted-server-js/lib/test/composition/browser.test.ts`

- [ ] **Step 1: Add failing tests for `r1_` reservation validation, exact identity lookup, duplicate**
      insertion, atomic claim, duplicate simultaneous claim, consumed/stale/disposed
      tombstones, the fixed 15-minute render boundary, the ten-second Prebid admission
      lease, atomic selection promotion to a new 15-minute render expiry, unselected/
      aborted/selection-timeout tombstoning only through the original lease, union
      capacity 320, suppression/refusal and contract-failure tombstoning of a PUC claim
      against a pre-selection lease, refusal at capacity, no live eviction, and a late
      request for the oldest unexpired id. Cover immutable finite/nonnegative
      `WinnerContext{selectedCpm}`, exact Prebid-bid CPM equality, context preservation
      across promotion and projection replacement, transfer into the attempt before
      consumption, and context/source deletion from tombstones. Renderer-id generation/collision retry
      belongs to Rust Task 4, not this browser store.

- [ ] **Step 2: Move `apsPrebidRenderers` and consumed-id maps out of public globals into the**
      runtime-owned service. Entries carry exact slot, tagged render source, navigation
      generation, fixed expiry, state, attempt binding, and immutable winner context
      while live. Source
      `WindowProxy` is intentionally absent until the first valid PUC claim acquires
      it. Consumption transfers the render source/context to the exact attempt before
      replacing the entry. It never extends expiry; tombstones remain through original
      expiry with only id/expiry/state/minimum suppression metadata.

- [ ] **Step 3: Use one reservation type for every TS-owned APS and ADM PUC source.**
      Validate the supplied server id but never generate it in the browser. Cache UUID
      remains baseline `pbs_cache` transport state and never enters this store;
      upstream bid id remains provenance and native Prebid ids remain native. Reject
      an `r1_` collision against any live/tombstoned entry; lookup recognizes a TS id
      before detailed validation.

- [ ] **Step 4: Run:**

  ```bash
  npm --prefix crates/trusted-server-js/lib test -- --run test/services/reservations.test.ts test/integrations/aps/render.test.ts
  ```

- [ ] **Step 5: Stage and commit the bounded reservation store:**

  ```bash
  git status --short
  git add \
    crates/trusted-server-js/lib/src/services/reservations.ts \
    crates/trusted-server-js/lib/test/services/reservations.test.ts \
    crates/trusted-server-js/lib/src/kernel/sessions.ts \
    crates/trusted-server-js/lib/src/integrations/aps/render.ts \
    crates/trusted-server-js/lib/src/composition/browser_test.ts \
    crates/trusted-server-js/lib/test/composition/browser.test.ts
  git diff --cached --check
  git commit -m "Add bounded renderer reservations"
  ```

### Task 13: Implement the RenderAttempt state machine and direct paths

Task 13 is an umbrella only. Execute 13A attempt/artifact ownership, 13B direct APS,
and 13C direct ADM plus path-independent lifecycle as three separate
red-to-green-to-commit checkpoints. Do not stage a later checkpoint with an earlier
one.

**Files:**

- Create: `crates/trusted-server-js/lib/src/services/render.ts`
- Create: `crates/trusted-server-js/lib/test/services/render.test.ts`
- Modify: `crates/trusted-server-js/lib/src/integrations/aps/render.ts`
- Modify: `crates/trusted-server-js/lib/src/core/render.ts`
- Modify: `crates/trusted-server-js/lib/src/composition/browser_test.ts`
- Modify: `crates/trusted-server-js/lib/test/composition/browser.test.ts`

- [ ] **Step 13A.1: Add failing state-table tests for every valid transition and every invalid/replay**
      transition. Race success, failure, timeout, caller abort, supersession, and
      navigation disposal through one terminal latch. Add accepted-artifact promotion
      races and prove terminal attempt disposal cannot remove a committed render. At
      construction assert one exact navigation-unique `a1_` attempt id; fallback child
      ids are distinct and bind their exact parent id. Test navigation-prefix failure,
      ordinal exhaustion, disposal, and that neither ids nor issuer bytes reach logs.
      Treat `created -> no_bid` as the sole `no_bid` transition for an exact parsed
      server no-winner decision; every later no-bid transition is invalid.

- [ ] **Step 13A.2: Run the focused attempt/artifact tests and require RED.**

  ```bash
  npm --prefix crates/trusted-server-js/lib test -- --run test/services/render.test.ts test/core/render.test.ts
  ```

  Expected: FAIL only in the new transition/artifact cases before implementation.

- [ ] **Step 13A.3: Implement path-independent `RenderAttempt` ownership: state, exact slot,**
      generation, exact `a1_` id, optional parent attempt id, tagged render source,
      immutable `WinnerContext{selectedCpm}`, timers, ports, iframe, terminal result,
      and disposer. Direct winner admission constructs the context from the exact
      validated joined server winner; a PUC claim receives the same context from the
      consumed reservation by presenting the exact one-shot frozen claim result;
      the result exposes neither source nor context, and those values are never
      accepted as independently swappable inputs. Accept that claim only through the
      branded reservation service while the service,
      original attempt/navigation, and fixed reservation expiry remain live.
      Obtain the id only from the NavigationSession issuer before registering work; an
      issuance failure settles `identity_generation_failed` without DOM/global
      mutation. Add `SlotOperation` above attempts so a primary and optional fallback
      retain immutable child results while the operation exposes one final result and
      `path`.
      Transition methods—not callers—create and clear deadlines. Before acceptance,
      atomically detach committed iframe/targeting/physical-slot metadata into one
      slot/navigation-owned `CommittedRenderArtifact`; the attempt disposer removes
      only uncommitted resources. On replacement, promote the new artifact, dispose
      the prior artifact before publishing the new one, and rebase targeting ownership
      without clearing the newer generation. Direct
      iframe artifacts remove their DOM; PUC artifacts defer DOM ownership to GPT and
      follow TS-owned destroy/redefine versus publisher-owned metadata-only rules.
      Require synchronous exact-once artifact disposal, reject Promise/thenable
      disposers, reject republication once disposal starts, and require private
      provenance for artifact stores, attempts, and fallback children.
      On every post-issuance construction rejection, best-effort dispose the exact
      captured attempt scope and prove the session permits an immediate same-slot
      retry.

- [ ] **Step 13A.4: Run the focused attempt/artifact tests GREEN and commit.**

  ```bash
  npm --prefix crates/trusted-server-js/lib test -- --run test/services/render.test.ts test/core/render.test.ts
  git add crates/trusted-server-js/lib/src/services/render.ts crates/trusted-server-js/lib/test/services/render.test.ts crates/trusted-server-js/lib/src/core/render.ts crates/trusted-server-js/lib/src/composition/browser_test.ts crates/trusted-server-js/lib/test/composition/browser.test.ts
  git commit -m "Implement render-attempt artifact ownership"
  ```

- [ ] **Step 13B.1: Add failing direct-APS tests.** Cover renderer-nonce live-registry
      behavior at 255/256/257 entries, eight collision draws, crypto failure, exact
      source/port/attempt/generation binding, disposal reuse, and no tombstone/history
      set. Capacity maps `capability_registry_full`; exhausted draws map
      `identity_generation_failed`. Cover every direct-path behavior enumerated in
      13B.3, including exact sandbox token order, omitted privileges, absolute
      URL/nonce construction, attributes/CSS, referrer policy, detached creation, and
      single append.

- [ ] **Step 13B.2: Run the direct-APS tests and require RED.**

  ```bash
  npm --prefix crates/trusted-server-js/lib test -- --run test/integrations/aps/render.test.ts test/services/render.test.ts
  ```

  Expected: FAIL only in the new nonce/channel/sandbox cases before implementation.

- [ ] **Step 13B.3: Implement the direct-APS path:**
  - validate descriptor before DOM mutation;
  - mint one exact `n1_` nonce from the 16-byte Web Crypto issuer and create the
    inner channel immediately before insertion; bind the nonce to the exact attempt,
    generation, renderer `contentWindow`, and retained port;
  - put the nonce in the fragment and transfer an envelope containing the
    kernel-captured publisher origin;
  - use captured native document, tree, event, attribute, source, and removal
    authorities to create one fresh iframe in the exact publisher document; never
    accept a publisher-supplied connected or detached frame, forged `contentWindow`,
    lying `src`, or hostile cleanup method;
  - bind to the exact iframe browsing-context `contentWindow` and transferred port,
    fail detectable node removal/replacement or `src` mutation, and preserve the
    explicit §4.4 ancestor-navigation trust boundary: an opaque active `Document`
    cannot be attested after undetectable `contentWindow.location` navigation, so no
    test or release evidence may claim otherwise;
  - atomically consume the nonce on the first valid document acceptance and
    invalidate it on failure, supersession, navigation, or disposal; duplicate,
    wrong-source, stale, or late use is inert and nonce values are never logged;
  - start the document deadline at iframe insertion and fail
    `renderer_document_no_load` at three seconds;
  - make the kernel the sole owner of the ten-second APS-completion deadline,
    starting only at document acceptance; the static renderer owns no competing
    timer;
  - map proxy/CORS/script-load failure to `runner_no_load`, callback rejection or
    ten-second callback silence to `runner_failed`, treat `runner_loaded` only as
    progress, and accept only `TS APS Render Completed`; remove on failure/cancel.

  Use one shared renderer-iframe constructor and constant for this direct path and
  Task 14's PUC owner. It must serialize the sandbox tokens exactly as
  `allow-forms allow-pointer-lock allow-popups allow-popups-to-escape-sandbox allow-scripts allow-top-navigation-by-user-activation`,
  omit `allow-same-origin` and every unlisted privilege, set
  `referrerPolicy='no-referrer'`, and use the captured current-generation absolute
  `/integrations/aps/renderer/v1` URL with no query plus exactly one `#n1_…` fragment.
  Set the validated integral 1–4096 dimensions as width/height attributes and CSS
  pixels, with block layout, zero border/margin, hidden overflow, no scrolling,
  title `Ad content`, and aria-label `Advertisement`.

- [ ] **Step 13B.4: Run the direct-APS tests GREEN and commit.**

  ```bash
  npm --prefix crates/trusted-server-js/lib test -- --run test/integrations/aps/render.test.ts test/services/render.test.ts
  git add crates/trusted-server-js/lib/src/integrations/aps/render.ts crates/trusted-server-js/lib/test/integrations/aps/render.test.ts crates/trusted-server-js/lib/src/services/render.ts crates/trusted-server-js/lib/test/services/render.test.ts
  git commit -m "Implement the direct APS render path"
  ```

- [ ] **Step 13C.1: Add failing direct-ADM and lifecycle tests.** Cover every
      constructor, cache non-regression, kill-switch, notification, and lifecycle
      behavior enumerated in 13C.3-13C.4.

- [ ] **Step 13C.2: Run the ADM/lifecycle tests and require RED.**

  ```bash
  npm --prefix crates/trusted-server-js/lib test -- --run test/services/render.test.ts test/core/render.test.ts test/integrations/aps/render.test.ts
  ```

  Expected: FAIL only in the new ADM/lifecycle cases before implementation.

- [ ] **Step 13C.3: Implement the shared ADM constructor.**
      Use the exact ordered sandbox, no-referrer policy, integral 1–4096 source dimensions,
      CSS sizing, zero border/margin, hidden overflow, display, scrolling, title, and
      aria attributes from spec §4.5. Create the iframe detached, install one-shot
      handlers/disposal first, assign exactly one complete `srcdoc`, and append once;
      never append empty or set `src`. Accept only the exact current pending frame's
      intended `srcdoc` load while its generation/latch remain current. Initial
      `about:blank`, pre-assignment, removed/replaced frame, stale generation,
      post-disposal, duplicate load, error, and five-second timeout cannot accept and
      map to `adm_document_no_load`.

  Do not route `pbs_cache` through `RenderAttempt`, this ADM constructor, or a new
  direct path. The GPT integration retains the pinned baseline cache implementation
  behind only the runtime generation/disposal boundary. Its existing request, parse,
  macro, PUC response, collapsed-resize, failure, and stale-navigation behavior is
  proven by a black-box pre/post corpus; no new cache inputs, outputs, identity,
  deadlines, or error reasons are implemented here.

- [ ] **Step 13C.4: Finish render lifecycle behavior behind the test-only composition before any**
      **production switch.** Snapshot render-relevant configuration when the attempt is
      created, then re-check the navigation generation and the already-snapshotted
      kill-switch state immediately before the earliest irreversible action: bridge
      response, DOM insertion, or an existing non-APS notification. Prove a later
      mutation cannot change an in-flight attempt and that an already-loaded page sees
      configuration changes only through an existing response path; add no polling,
      push channel, or event ingestion.

  Route existing non-APS `nurl`/`burl` behavior through the accepted terminal
  transition so each notification initiates at most once and never blocks or changes
  the render result. Add the explicit negative assertion that APS neither has nor
  synthesizes either URL. Keep all remote side effects outside terminal correctness.

- [ ] **Step 13C.5: Run the ADM/lifecycle tests GREEN and commit.**

  ```bash
  npm --prefix crates/trusted-server-js/lib test -- --run test/services/render.test.ts test/core/render.test.ts test/integrations/aps/render.test.ts
  git add crates/trusted-server-js/lib/src/services/render.ts crates/trusted-server-js/lib/test/services/render.test.ts crates/trusted-server-js/lib/src/integrations/aps/render.ts crates/trusted-server-js/lib/test/integrations/aps/render.test.ts crates/trusted-server-js/lib/src/core/render.ts crates/trusted-server-js/lib/src/composition/browser_test.ts crates/trusted-server-js/lib/test/composition/browser.test.ts
  git commit -m "Implement direct ADM and render lifecycle"
  ```

### Task 14: Implement Universal Creative claim and owner-control channels

Task 14 is an umbrella only. Execute 14A claim recognition/two-condition join, 14B
lifecycle-ticket owner registration, and 14C APS/ADM owner control and settlement as
three separate red-to-green-to-commit checkpoints. Do not stage a later checkpoint
with an earlier one.

**Files:**

- Modify: `crates/trusted-server-js/lib/src/services/render.ts`
- Modify: `crates/trusted-server-js/lib/src/services/reservations.ts`
- Modify: `crates/trusted-server-js/lib/src/adapters/messaging.ts`
- Create: `crates/trusted-server-js/lib/src/services/puc_bridge.ts`
- Create: `crates/trusted-server-js/lib/test/services/puc_bridge.test.ts`
- Modify: `crates/trusted-server-js/lib/src/integrations/aps/render.ts`
- Modify: `crates/trusted-server-js/lib/test/integrations/aps/render.test.ts`
- Modify: `crates/trusted-server-js/lib/test/integrations/gpt/ad_init.test.ts`
- Modify: `crates/trusted-server-js/lib/src/composition/browser_test.ts`
- Modify: `crates/trusted-server-js/lib/test/composition/browser.test.ts`

- [ ] **Step 14A.1: Build a locally authored PUC contract harness without copying or vendoring PUC**
      **bytes.** Keep it limited to the public `prebidMessenger`, dynamic-renderer,
      and `h.sendMessage` behavior exercised by this protocol. The external GAM
      configuration selects PUC 1.17.2 and never `latest`; the real-GAM gate, not a
      repository artifact, validates that release. Add only the failing
      claim-recognition and join tests for the exact JSON string
      `{message:"Prebid Request",adId,adServerDomain}`, object/extended shapes,
      zero/two ports, native id, live/tombstoned TS id, duplicate simultaneous claim,
      replay, prior navigation, SafeFrame-shaped nesting, outer post failure, and every
      ordering of early claim, nonempty/empty GAM, navigation, supersession, claim
      deadline, and GPT-cycle deadline. Owner-registration and control/settlement
      cases belong only to 14B and 14C respectively.

- [ ] **Step 14A.2: Run the claim/join tests and require RED.**

  ```bash
  npm --prefix crates/trusted-server-js/lib test -- --run test/services/puc_bridge.test.ts test/integrations/gpt/ad_init.test.ts
  ```

  Expected: FAIL only in the new recognizer/join cases before implementation.

- [ ] **Step 14A.3: Install exactly one capture-phase bridge dispatcher synchronously as the first**
      reversible core activation, before integration-module activation and any
      TS-owned GPT/Prebid injection; asynchronous preparation leaves no listener. It
      multiplexes only `Prebid Request` and
      `TS Render Owner Register`; no attempt installs another global listener. First
      perform side-effect-free minimal extraction of own data
      `message`/`adId`/optional `lifecycleTicket` from JSON strings or clone-safe plain
      objects without invoking accessors. For the request branch, lookup before exact
      parsing or port checks. Leave non-TS ids to native Prebid; for live/tombstoned TS
      ids call `stopImmediatePropagation()` immediately, including object/extended
      shapes and zero/two ports, then exact-parse and generically refuse/close invalid
      requests. The first valid claim acquires
      `MessageEvent.source` as the authoritative PUC `WindowProxy`; never precompute or
      walk the SafeFrame ancestry.

- [ ] **Step 14A.4: Implement the two-condition join. An early claim buffers only source plus outer**
      response port and discloses no render data. A nonempty GAM result starts the
      three-second claim timer when claim is absent. Empty/disposal/supersession closes
      the port and tombstones. When both are present, atomically revalidate/consume,
      mint an exact `t1_` ticket from 16 Web Crypto CSPRNG bytes through the Task 9
      issuer, with at most eight total draws and fixed three-second TTL, and
      reply with the exact ready outer schema. Bind it to the exact attempt id,
      reservation id, PUC source, and navigation generation; issuance failure settles
      `identity_generation_failed` before any ready response. Refusals use the exact
      generic refused schema.
      The outer response carries only owner kind/ticket plus the checked-in dynamic
      owner, never ADM, APS descriptor, nonce, or final document port.

  Store live tickets plus tombstones in one runtime-owned capacity-320 registry.
  Prune expired entries first, never evict an unexpired entry, preserve the original
  three-second expiry on consumption/disposal, and map capacity to
  `capability_registry_full` and eight-draw collision exhaustion to
  `identity_generation_failed` before exposing any usable capability.

  Bound a claim-first path by the GPT request-start/completion deadlines from Task
  11 and the attempt deadline. Test boundary races and prove only an attributable
  completion-timeout event drains its exact physical cycle; request-timeout events
  never release publisher quarantine. Neither can revive the claim or start
  fallback.

- [ ] **Step 14A.5: Run the claim/join tests GREEN and commit.**

  ```bash
  npm --prefix crates/trusted-server-js/lib test -- --run test/services/puc_bridge.test.ts test/integrations/gpt/ad_init.test.ts
  git add crates/trusted-server-js/lib/src/services/puc_bridge.ts crates/trusted-server-js/lib/test/services/puc_bridge.test.ts crates/trusted-server-js/lib/src/services/reservations.ts crates/trusted-server-js/lib/src/adapters/messaging.ts crates/trusted-server-js/lib/test/integrations/gpt/ad_init.test.ts
  git commit -m "Implement Universal Creative claim joining"
  ```

- [ ] **Step 14B.1: Add failing owner-registration tests.** Require PUC's supplied
      `h.sendMessage('TS Render Owner Register',{version:1,lifecycleTicket},callback)`
      and forbid global `postMessage`. Assert the kernel sees the original captured PUC
      source, exact auto-added `adId/message` keys, and one helper-created response
      port. Test exact registered/refused responses, ticket TTL/atomic consumption,
      wrong source, stale generation, replay, zero/two response ports, the owner's
      three-second watchdog, helper disposer, and late response.

- [ ] **Step 14B.2: Run the owner-registration tests and require RED.**

  ```bash
  npm --prefix crates/trusted-server-js/lib test -- --run test/services/puc_bridge.test.ts
  ```

  Expected: FAIL only in the new ticket/registration cases before implementation.

- [ ] **Step 14B.3: Implement owner registration.** Call the exact supplied
      `h.sendMessage` form from 14B.1 and never global `postMessage`.

  Receive registration through the same dispatcher. Minimally lookup the ticket
  map first; ignore unknown tickets, but suppress live/tombstoned TS tickets before
  exact source/adId/attempt/generation/shape/one-port checks. Refuse and close
  recognized invalid/replayed registration, keep ticket tombstones through their
  original TTL, and prove attempt disposal removes ticket state without removing
  the runtime dispatcher. Atomically consume the first valid use, invalidate on
  timeout/failure/supersession/navigation/disposal, make duplicate/stale/late uses
  inert, and prove ticket values and issuer bytes never reach logs.

- [ ] **Step 14B.4: Run the owner-registration tests GREEN and commit.**

  ```bash
  npm --prefix crates/trusted-server-js/lib test -- --run test/services/puc_bridge.test.ts
  git add crates/trusted-server-js/lib/src/services/puc_bridge.ts crates/trusted-server-js/lib/test/services/puc_bridge.test.ts crates/trusted-server-js/lib/src/services/reservations.ts crates/trusted-server-js/lib/src/adapters/messaging.ts
  git commit -m "Authenticate Universal Creative owners"
  ```

- [ ] **Step 14C.1: Add failing owner-control, settlement, and parser-corpus tests.**
      Require one retained and one transferred owner-control endpoint in
      `TS Render Owner Registered`; exact APS/ADM start shapes and port counts; and
      exact-shape tests for every start, insertion, document progress, render
      completion/failure, ADM load/failure, and final owner settlement message and for
      every wrong/extra key or port count. `OwnerSettlementV1` cancellation includes
      exactly `caller_aborted | superseded | navigation_disposed`; every terminal
      RenderOutcome is therefore encodable after registration. Add the complete
      bounds/prototype/encoding corpus described in 14C.5 before implementation.

- [ ] **Step 14C.2: Run the owner-control tests and require RED.**

  ```bash
  npm --prefix crates/trusted-server-js/lib test -- --run test/services/puc_bridge.test.ts test/services/render.test.ts test/integrations/gpt/ad_init.test.ts
  ```

  Expected: FAIL only in the new owner-control/settlement/parser cases before
  implementation.

- [ ] **Step 14C.3: Implement the owner-control channel and exact start shapes.** On
      registration the kernel creates the channel, keeps one endpoint, and transfers
      exactly one endpoint in `TS Render Owner Registered`. For APS it sends exact
      `TS APS Start` with the descriptor/envelope plus exactly one renderer-document
      port; for ADM it sends exact `TS ADM Start` and no port.

- [ ] **Step 14C.4: For APS, make the owner create exactly one iframe by reusing Task 13's exact shared renderer-iframe constructor and immutable sandbox**
      constant, meet the one-second insertion deadline, and leave document and
      completion timing to the kernel anchors from Task 13. For ADM, call the
      exact shared detached-iframe constructor from Task 13; report `TS Owner Inserted`
      only after its one append, then report only the intended load/failure. Initial
      blank, replacement, removal, duplicate, stale, disposed, error, and timeout races
      cannot resolve the PUC Promise. Resolve/reject that Promise only from the kernel's
      final settlement. The remote owner—not the kernel—owns that iframe: accepted
      settlement promotes it, removes temporary handlers, closes the port, and
      resolves once; failed/cancelled settlement removes it, removes handlers, closes,
      and rejects once. Arm one fail-closed 20-second settlement/channel watchdog when
      registration accepts the control port, before start; never rearm it on start.
      Malformed control, `messageerror`, local disposal, silent loss, or expiry runs
      remote cleanup but cannot report acceptance to the kernel. Direct-path iframe
      cleanup remains kernel-owned. The kernel owns all terminal decisions; bidder ADM
      receives no capability. Prove each channel creator/retained/transferred endpoint
      is closed by success, refusal, timeout, cancellation, and navigation tests.

  Include caller abort before/after registration, insertion, and document acceptance;
  lost/closed control channel before start and after insertion; settlement-post
  throw; cleanup at 19,999/20,000/20,001 ms; and proof accepted remote DOM remains
  while every uncommitted failure removes exactly its owned iframe and settles the
  PUC Promise once. Reassert Task 13's exact sandbox token order, omitted privileges,
  absolute renderer URL plus nonce, dimensions, and no-referrer policy through the
  remote-owner path; do not create a PUC-specific constructor or constant.

- [ ] **Step 14C.5: Implement and run the shared protocol corpus through both global and port parsers. Enforce the**
      4,096-byte inbound JSON cap before parse; exact 25-character capabilities;
      field-specific 256/2,048/4,096-byte limits; safe generations; 64 KiB dynamic
      owner and 72 KiB successful outer-response limits; exact keys/prototypes; and
      boundary-minus-one/boundary/boundary-plus-one multibyte, duplicate-key, malformed
      encoding, accessor, and exact 1/4096 dimension cases.

- [ ] **Step 14C.6: Run the owner-control tests GREEN and commit.**

  ```bash
  npm --prefix crates/trusted-server-js/lib test -- --run test/services/puc_bridge.test.ts test/services/render.test.ts test/integrations/gpt/ad_init.test.ts
  git add crates/trusted-server-js/lib/src/services/puc_bridge.ts crates/trusted-server-js/lib/test/services/puc_bridge.test.ts crates/trusted-server-js/lib/src/services/render.ts crates/trusted-server-js/lib/src/services/reservations.ts crates/trusted-server-js/lib/src/adapters/messaging.ts crates/trusted-server-js/lib/src/integrations/aps/render.ts crates/trusted-server-js/lib/test/integrations/aps/render.test.ts crates/trusted-server-js/lib/test/integrations/gpt/ad_init.test.ts crates/trusted-server-js/lib/src/composition/browser_test.ts crates/trusted-server-js/lib/test/composition/browser.test.ts
  git commit -m "Implement Universal Creative render ownership"
  ```

### Task 15: Implement the exact public API, programmatic registration, and `requestAds`

**Files:**

- Create: `crates/trusted-server-js/lib/src/services/auction_batch.ts`
- Create: `crates/trusted-server-js/lib/test/services/auction_batch.test.ts`
- Modify: `crates/trusted-server-js/lib/src/services/context.ts`
- Modify: `crates/trusted-server-js/lib/test/services/context.test.ts`
- Modify: `crates/trusted-server-js/lib/src/core/request.ts`
- Modify: `crates/trusted-server-js/lib/src/core/auction.ts`
- Modify: `crates/trusted-server-js/lib/src/core/index.ts`
- Modify: `crates/trusted-server-js/lib/src/core/registry.ts`
- Modify: `crates/trusted-server-js/lib/src/core/log.ts`
- Modify: `crates/trusted-server-js/lib/src/core/queue.ts`
- Modify: `crates/trusted-server-js/lib/src/core/types.ts`
- Modify: `crates/trusted-server-js/lib/src/core/global.d.ts`
- Modify: `crates/trusted-server-js/lib/src/index.ts`
- Modify: `crates/trusted-server-js/lib/test/core/request.test.ts`
- Modify: `crates/trusted-server-js/lib/test/core/auction.test.ts`
- Modify: `crates/trusted-server-js/lib/test/core/registry.test.ts`
- Modify: `crates/trusted-server-js/lib/test/core/log.test.ts`
- Modify: `crates/trusted-server-js/lib/test/core/queue.test.ts`
- Modify: `crates/trusted-server-js/lib/src/composition/browser_test.ts`
- Modify: `crates/trusted-server-js/lib/test/composition/browser.test.ts`

- [ ] **Step 1: Add failing input tests for omitted/undefined options, null/array/non-object,**
      foreign/accessor/unknown properties, invalid/duplicate/too-many slots, timeout
      bounds, AbortSignal brand, exact server-slot-id selection, an unknown id beside a
      valid sibling, ad-unit/DOM alias collisions, and input-order output. Prove an
      omitted-slot call synchronously snapshots current NavigationSession registrations
      by ordinal and excludes a later registration. Prove an omitted timeout uses
      exactly 10,000 ms for only the shared auction fetch; readiness and renderer/GPT
      deadlines neither inherit nor reset it.

- [ ] **Step 2: Add failing public-surface and registration tests.** Assert the exact
      kernel/fallback `TsjsApi` own properties, `version:'1.0.0'`, release equality,
      recursively frozen `TsjsBootV1`, non-enumerable frozen status-only `_internal`,
      permanently refusing `_registerIntegration`, diagnostics present only on kernel,
      and no compatibility/placeholder/mutable-config aliases. Keep numeric `V1` only
      on actually serialized boot/wire schemas; public helpers are `TsjsApi`,
      `TsjsCommandQueue`, `TsjsLog`, and `TsjsDiagnostics`. Update the package barrel
      to export only the final public names and remove old `AdUnit`/API aliases.

  Cover `addAdUnits` one/array, empty/257 units, unknown/accessor/prototype fields,
  duplicate/colliding codes, malformed media/bids/params, encoded 256 KiB auction
  body cap, 63/64/65-byte bidder names, integral dimensions at 0/1/4096/4097 with
  exact `invalid_dimensions | dimensions_out_of_range`, and combined registry totals
  255/256/257. Registration is synchronous, all-or-nothing, navigation-scoped, and
  assigns ordinals after server slots. Fallback fully validates then throws exact
  `TsjsUnavailableError` without constructing a registry.

  Assert logger default level/methods, all valid levels, invalid-level throw without
  mutation, bounded output, and missing/throwing console methods. Reuse Task 8's
  frozen real-Array queue tests as the final public queue contract.

- [ ] **Step 3: Implement `addAdUnits` before live mutation.** Validate the complete
      input in deterministic field order, reserve capacity for the whole call, then
      transactionally register exact programmatic `code` values into the same
      navigation slot service. A code is a public registered slot id but never a GPT
      path/DOM alias; successful units participate only in later direct-auction
      snapshots and do not define/display/target GPT.

- [ ] **Step 4: Replace the void/callback API with the exact Promise contract. Interpret every**
      explicit entry only as an exact case-sensitive registered slot id—server or
      programmatic—never a GPT ad-unit path, DOM id, or alias. Preserve explicit input order or omitted snapshot order in
      the results and always return the exact server id. Input errors reject before
      creating attempts. Unknown explicit ids resolve `slot_unresolved` without
      blocking valid siblings. After attempt creation, operational/render failures
      resolve as typed per-slot results.

- [ ] **Step 5: Implement one `AuctionBatch` per fetch. Test partial overlap across concurrent**
      calls, one child superseded, all children superseded, already-aborted caller,
      later abort, response deadline, reversed response order, invalid response,
      missing/duplicate/extra slot decisions, winner-to-bid join mismatch, server
      failed decisions, and navigation disposal. Parse the exact standard `bid.id` /
      `impid` plus exact three-key `bid.ext.trusted_server` contract and require the
      decision/candidate/impid/slot join. Reject standard `adm` on APS/cache and any ADM
      mismatch; never infer a render source from standard bid fields.

  Snapshot the runtime-owned auction-context contributors exactly once before request
  serialization. Contributors are invoked in manifest registration order; later keys
  retain the baseline precedence, one throw is locally logged/isolated, and module or
  runtime disposal removes the contributor before the next batch. Navigation changes
  do not duplicate or discard a document-scoped contributor. No integration imports
  or mutates a module-global provider map.

- [ ] **Step 6: Preserve error distinctions:**
      `auction_timeout | network_error | http_error | invalid_response`. After parsing,
      consume the server's exact per-slot decisions; only an explicit `no_bid` decision
      is no-bid. Never infer no-bid from an absent/malformed sibling.

- [ ] **Step 7: Exercise the exact `TsjsApi` through test-only composition. Keep the shipped public**
      entry point unchanged until Task 19; Task 22 deletes the old callback/void overload
      and declarations rather than keeping an alias.

- [ ] **Step 8: Run:**

  ```bash
  npm --prefix crates/trusted-server-js/lib test -- --run test/services/auction_batch.test.ts test/core/request.test.ts test/core/auction.test.ts test/core/index.test.ts
  npm --prefix crates/trusted-server-js/lib test -- --run test/core/registry.test.ts test/core/log.test.ts test/core/queue.test.ts
  npm --prefix crates/trusted-server-js/lib run typecheck
  ```

- [ ] **Step 9: Stage and commit the exact public API and auction-batch behavior:**

  ```bash
  git status --short
  git add \
    crates/trusted-server-js/lib/src/services/auction_batch.ts \
    crates/trusted-server-js/lib/test/services/auction_batch.test.ts \
    crates/trusted-server-js/lib/src/services/context.ts \
    crates/trusted-server-js/lib/test/services/context.test.ts \
    crates/trusted-server-js/lib/src/core/request.ts \
    crates/trusted-server-js/lib/src/core/auction.ts \
    crates/trusted-server-js/lib/src/core/index.ts \
    crates/trusted-server-js/lib/src/core/registry.ts \
    crates/trusted-server-js/lib/src/core/log.ts \
    crates/trusted-server-js/lib/src/core/queue.ts \
    crates/trusted-server-js/lib/src/core/types.ts \
    crates/trusted-server-js/lib/src/core/global.d.ts \
    crates/trusted-server-js/lib/src/index.ts \
    crates/trusted-server-js/lib/test/core/request.test.ts \
    crates/trusted-server-js/lib/test/core/auction.test.ts \
    crates/trusted-server-js/lib/test/core/registry.test.ts \
    crates/trusted-server-js/lib/test/core/log.test.ts \
    crates/trusted-server-js/lib/test/core/queue.test.ts \
    crates/trusted-server-js/lib/src/composition/browser_test.ts \
    crates/trusted-server-js/lib/test/composition/browser.test.ts
  git diff --cached --check
  git commit -m "Define the exact TSJS public API"
  ```

### Phase 3 exit

- Slot/cycle attribution, reservations, direct/PUC lifecycles, and auction batches
  are service-owned and independently tested.
- Every created attempt has one terminal result under adversarial ordering.
- The new test-only public contract has no compatibility aliases; the shipped API
  remains unchanged until the coordinated Task 19 switch.

## Phase 4 — migrate integrations and remove duplicate state

### Task 16: Prepare the GPT integration module over adapters, slots, and render services

**Files:**

- Modify: `crates/trusted-server-js/lib/src/integrations/gpt/index.ts`
- Create: `crates/trusted-server-js/lib/src/integrations/gpt/module.ts`
- Modify: `crates/trusted-server-js/lib/test/integrations/gpt/index.test.ts`
- Modify: `crates/trusted-server-js/lib/test/integrations/gpt/ad_init.test.ts`
- Modify: `crates/trusted-server-js/lib/test/integrations/gpt/schedule_initial_ad_init.test.ts`
- Modify: `crates/trusted-server-js/lib/test/integrations/gpt/spa_hook.test.ts`
- Modify: `crates/trusted-server-js/lib/src/integrations/gpt/script_guard.ts`
- Modify: `crates/trusted-server-js/lib/src/shared/script_guard.ts`
- Modify: `crates/trusted-server-js/lib/src/adapters/googletag.ts`
- Modify: `crates/trusted-server-js/lib/test/adapters/googletag.test.ts`
- Modify: `crates/trusted-server-js/lib/src/services/slots.ts`
- Modify: `crates/trusted-server-js/lib/test/services/slots.test.ts`
- Modify: `crates/trusted-server-js/lib/src/services/targeting.ts`
- Modify: `crates/trusted-server-js/lib/test/services/targeting.test.ts`
- Modify: `crates/trusted-server-js/lib/src/services/render.ts`
- Modify: `crates/trusted-server-js/lib/test/services/render.test.ts`
- Modify: `crates/trusted-server-js/lib/src/composition/browser_test.ts`
- Modify: `crates/trusted-server-js/lib/test/composition/browser.test.ts`
- Modify: `crates/trusted-server-core/src/publisher.rs`

- [ ] **Step 1: Add or preserve failing tests for early unconditional GPT subscriptions,**
      publisher services already enabled, SRA, disabled initial load, one refresh path,
      `changeCorrelator:false`, responsive ambiguity, collapsed-shell guard, targeting
      ownership, page-bids, SPA replacement, retired TS-owned slot objects,
      publisher-owned cycle quarantine/drain, old completion races, and direct
      publisher GPT activity. Include external-readiness, request-start, and completion
      deadline boundaries and accepted-artifact replacement/navigation ownership.
      Add the exact handoff corpus: exact and hydration-renamed late `defineSlot`,
      mismatch/ambiguity, publisher duplicate `display`, explicit/global refresh with
      disabled initial load, unrelated slot/options preservation, ownership transfer
      racing navigation, and proof that publisher work never starts TS fallback.

  Add deterministic fake-timer DOM-reconciliation tests at 249/250 ms and
  4,999/5,000 ms for first/final pass success, ambiguity, expiry-versus-commit latch,
  ownership transfer during a pass, one and two successful rebinds, immediate
  `reconciliation_capacity` on a third disconnect, and navigation disposal. Exercise
  request-timeout, completion-timeout, navigation, and reconciliation through the
  same destroy/redefine transaction. A throwing/false `destroySlots([old])` retires
  and quarantines the old identity, defines no second physical slot, and returns
  `gpt_request_failed`; a failed replacement leaves the slot unbound. A stale
  generation destroys any just-created TS replacement and cannot bind it. Prove no
  path destroys a publisher-owned slot.

- [ ] **Step 2: Extract a GPT integration module used by test-only composition.** Its
      `prepare(ctx)` reads only validated frozen boot data and creates closures; it
      performs no GPT/global/DOM/script/timer/listener mutation. Its synchronous
      `activate(ctx)` installs every reversible adapter interception and registers
      disposers before mutation. Script injection or other irreversible startup is
      staged in the module's single `afterCommit` callback. Move every GPT-global
      call into `GoogletagAdapter` and all slot/cycle state into `slots.ts`, while
      retaining the shipped entry-point behavior until Task 19.

- [ ] **Step 3: Route APS and ADM winners to `RenderAttempt`/PUC bridge. Remove duplicate renderer**
      branches, slot expandos, local consumed-id maps, and independent refresh wrappers.

- [ ] **Step 4: Rebuild and unit-test the RCJ-GPT-04 collapsed-shell resize in the attempt-owned**
      PUC success path. Resize only after the authenticated current attempt posts its
      response, and only when the exact connected source iframe and its immediate
      ordinary wrapper both remain collapsed to at most 1x1. Require finite positive
      winning dimensions; reject anchors, unrelated frames, detached/replaced frames,
      expanded dimensions, and fixed/sticky shells. Assert one guarded resize of only
      those two nodes, plus inert replay, stale-attempt, navigation, and failure paths
      in `test/integrations/gpt/ad_init.test.ts` and `test/services/render.test.ts`.

- [ ] **Step 5: Implement the owner-and-value targeting journal in `services/targeting.ts`.**
      Keep one closure-private stack per physical GPT slot/key. Each TS write pushes a
      distinct frame containing its owner id, exact installed string, and predecessor
      value/owner—even when the string is unchanged. The GPT adapter observes the live
      slot's `setTargeting`, per-key `clearTargeting`, and clear-all calls. A private
      reentrancy marker distinguishes TS calls; every publisher-originated mutation
      invalidates the affected restoration chain before forwarding, including a
      same-value write, without changing arguments, return value, throw behavior, or
      ordering.

  Before a TS write, compare the actual GPT value to the current frame and discard a
  stale chain rather than overwriting publisher state. Cleanup writes only when the
  disposing frame is the current owner and the actual value still equals its
  installed string. Removing a non-top frame performs no GPT call and rebases the
  successor to the removed predecessor. Acceptance promotes frames into the
  `CommittedRenderArtifact`; disposal of an older accepted artifact uses the same
  non-top rebase. Test two equal-string generations under newer success, failure,
  supersession, and older-artifact disposal, plus publisher different-value and
  same-value set, per-key clear, clear-all, wrapper replacement, and throws at every
  cleanup point. Never blind-clear.

  Implement the ordered publication transaction: validate source/slot/reservation,
  register the live reservation, expose exact `hb_adid` and other targeting, record
  GPT intent, then invoke the request. Any intervening failure tombstones the
  reservation, compare-restores targeting, and settles. Prove a fast creative
  request always finds the store entry.

- [ ] **Step 6: Fold integration-specific script-guard mechanics onto the shared factory while**
      keeping GPT configuration in its integration. Implement one runtime-owned
      `MutationObserver` per `NavigationSession`, 250 ms debounce, 5,000 ms monotonic
      window, one final boundary pass, the two-success cap, exact physical-object
      quarantine, and complete timer/candidate/reference disposal. Successful handoff
      cancels reconciliation and transfers cleanup ownership synchronously.

- [ ] **Step 7: Add the attributable-empty-cycle fallback corpus and implementation before the**
      **switch.** Prove fallback begins only after an attributable TS-owned empty GAM
      cycle; the primary child settles before fallback starts; publisher, ambiguous,
      quarantined, timeout, and stale cases do not fall back; both child histories
      remain immutable; and `SlotOperation` publishes exactly one final result with
      `path:'fallback'` when the fallback child runs. Exercise this through the GPT
      adapter, slot service, render service, and test-only browser composition; do not
      wire a shipped entry point yet.

- [ ] **Step 8: Implement and test the prospective real performance marks before the switch.**
      Make the generated bootstrap controller execute
      `performance.mark('tsjs:bids-script')` immediately before the critical head
      sequence but leave its production call site unchanged. In the shared render/
      GPT owners, implement exactly-once `performance.mark('tsjs:first-display')`
      immediately before the first protected TS-owned GPT display/refresh or direct
      iframe insertion. Record `tsjs:first-display-terminal` when that attempt settles
      and `tsjs:first-display-paint` only after the complete immutable protected batch
      settles and Task 8C's two-frame/hidden paint gate passes. A page with no attempt
      emits none of the display marks. Exercise all four through test-only composition,
      including replay, publisher/non-authoritative display, direct display, sibling
      batch completion, post-window attempts, stale generation, and
      missing/throwing Performance API cases, and assert
      `performance.measure('tsjs:boot-to-first-display', 'tsjs:bids-script', 'tsjs:first-display')`
      uses those exact marks. `window.__tsjsPerf` is baseline-only scaffolding and is
      rejected by the prospective post-switch test.

- [ ] **Step 9: Run the entire GPT suite and prospective boot-mark tests, not only new files:**

  ```bash
  npm --prefix crates/trusted-server-js/lib test -- --run test/integrations/gpt
  npm --prefix crates/trusted-server-js/lib test -- --run test/adapters/googletag.test.ts test/services/slots.test.ts test/services/targeting.test.ts test/services/render.test.ts
  npm --prefix crates/trusted-server-js/lib test -- --run test/composition/browser.test.ts
  cargo test-fastly bids_script_performance_mark
  npm --prefix crates/trusted-server-js/lib run lint
  npm --prefix crates/trusted-server-js/lib run typecheck
  ```

- [ ] **Step 10: Commit the green GPT module as its own rollback boundary.**

  ```bash
  git add crates/trusted-server-js/lib/src/integrations/gpt/index.ts crates/trusted-server-js/lib/src/integrations/gpt/module.ts crates/trusted-server-js/lib/src/integrations/gpt/script_guard.ts
  git add crates/trusted-server-js/lib/test/integrations/gpt/index.test.ts crates/trusted-server-js/lib/test/integrations/gpt/ad_init.test.ts crates/trusted-server-js/lib/test/integrations/gpt/schedule_initial_ad_init.test.ts crates/trusted-server-js/lib/test/integrations/gpt/spa_hook.test.ts
  git add crates/trusted-server-js/lib/src/adapters/googletag.ts crates/trusted-server-js/lib/test/adapters/googletag.test.ts crates/trusted-server-js/lib/src/services/slots.ts crates/trusted-server-js/lib/test/services/slots.test.ts crates/trusted-server-js/lib/src/services/targeting.ts crates/trusted-server-js/lib/test/services/targeting.test.ts crates/trusted-server-js/lib/src/services/render.ts crates/trusted-server-js/lib/test/services/render.test.ts
  git add crates/trusted-server-js/lib/src/shared/script_guard.ts crates/trusted-server-js/lib/src/composition/browser_test.ts crates/trusted-server-js/lib/test/composition/browser.test.ts crates/trusted-server-core/src/publisher.rs
  git commit -m "Prepare the GPT integration module"
  ```

### Task 17: Prepare Prebid and APS registration on the shared runtime

**Files:**

- Modify: `crates/trusted-server-js/lib/src/integrations/prebid/index.ts`
- Create: `crates/trusted-server-js/lib/src/integrations/prebid/module.ts`
- Modify: `crates/trusted-server-js/lib/test/integrations/prebid/index.test.ts`
- Modify: `crates/trusted-server-js/lib/src/integrations/prebid/prebid_modules/aliases.d.ts`
- Modify: `crates/trusted-server-js/lib/src/integrations/prebid/prebid_modules/liveIntentIdSystem.ts`
- Modify: `crates/trusted-server-js/lib/src/integrations/prebid/user_id_modules.json`
- Modify: `crates/trusted-server-js/lib/src/integrations/prebid/user_id_modules.ts`
- Modify: `crates/trusted-server-js/lib/test/integrations/prebid/user_id_modules.test.ts`
- Modify: `crates/trusted-server-js/lib/src/adapters/prebid.ts`
- Modify: `crates/trusted-server-js/lib/test/adapters/prebid.test.ts`
- Modify: `crates/trusted-server-js/lib/src/integrations/aps/render.ts`
- Modify: `crates/trusted-server-js/lib/test/integrations/aps/render.test.ts`
- Modify: `crates/trusted-server-js/lib/src/core/types.ts`
- Modify: `crates/trusted-server-js/lib/src/composition/browser_test.ts`
- Modify: `crates/trusted-server-js/lib/test/composition/browser.test.ts`
- Modify: `crates/trusted-server-js/lib/build-prebid-external.mjs`
- Modify: `crates/trusted-server-js/lib/test/build-prebid-external.test.mjs`
- Modify: `crates/trusted-server-js/lib/test/prebid-artifact-integration.test.mjs`

- [ ] **Step 1: Add failing tests for the server `r1_` reservation replacing a Trusted Server**
      Prebid bid's generated `adId` before targeting, exact equality with `hb_adid`,
      exact ad-unit ownership, invalid descriptor, duplicate response, registration
      capacity, navigation disposal, native non-TS bid/id preservation, and lifecycle
      success/failure. Pin the exact content-addressed external artifact built from
      lockfile-resolved Prebid.js 10.26.0 and its admission behavior: `not_admitted`
      produces no bid/event/targeting state; throw and partial publication fail closed
      at runtime; and all three map to the exact admission/contract failure reasons.
      Cover selected versus losing TS bids, native winners, multiple ad units, exact
      auction-id ownership, navigation/auction abort, missing/late `auctionEnd`, event
      listener ordering before publisher callbacks, ten-second admission lease, and
      capacity release. Prove a PUC request before selection is suppressed/refused,
      tombstones the suppress-only lease, and maps to `prebid_contract_violation`. A
      dependency-version change requires deliberate fixture review.

- [ ] **Step 2: Extract a release-matched Prebid integration module used by test-only**
      composition. `prepare(ctx)` is inert; `activate(ctx)` synchronously installs
      only reversible adapter state and contributes at most one `afterCommit`
      callback. Use the Prebid adapter for every global call and the RuntimeSession
      reservation service for APS and ADM PUC entries. Baseline `pbs_cache` stays in
      the GPT integration and never enters this reservation service. The adapter binds one
      exact `pbjs` object plus its own recursively frozen artifact-stamp identity and
      rechecks both on every operation. Missing or invalid bindings report
      `incompatible` only for that readiness operation; later whole-object replacement
      can satisfy later work. Keep shipped entry-point behavior unchanged until Task 19.

- [ ] **Step 3: Remove `tsjs.apsPrebidRenderers`, Prebid function sentinels, and direct imports of**
      GPT integration internals. Preserve eids and unrelated Prebid behavior through
      existing tests.

- [ ] **Step 4: Keep `aps/render.ts` responsible for descriptor/client renderer mechanics only;**
      registry and lifecycle state live in services.

  Prepare and validate the complete bid, register the reservation, replace the TS
  `adId`, then call the version-pinned adapter's single
  `admitTrustedBid(preparedBid): admitted | not_admitted` boundary as the only
  irreversible action. Atomic `not_admitted` and throw tombstone and settle
  `prebid_admission_failed`; detected partial publication tombstones/suppresses,
  settles `prebid_contract_violation`, and blocks the artifact gate. Never alter
  native Prebid `adId` values.

  `admitted` enters `awaiting_prebid_selection` on a ten-second lease, not a render
  attempt. The adapter's early synchronous `auctionEnd` listener queries exact
  auction/ad-unit winners before publisher targeting callbacks, promotes only the
  selected TS id and its immutable `WinnerContext` into a new 15-minute render
  reservation/attempt, and tombstones all losing TS ids only through their original
  short lease. Missing auction end records
  `prebid_selection_timeout`; navigation/auction abort clears the admitted set. No
  losing bid remains live for 15 minutes.

- [ ] **Step 5: Rebuild and unit-test RCJ-PREBID-04 through one Prebid refresh policy over the**
      GPT adapter. Literal, case-sensitive configured GAM-path suffixes remove only
      eligible matches from the synthetic Prebid auction; missing, non-string, or
      throwing `getAdUnitPath()` fails open. Clear stale TS/Prebid targeting from every
      target, while the full original slot list and exact options continue to GPT.
      Cover global, explicit, mixed, all-excluded, no-exclusion, and fail-open cases in
      `test/integrations/prebid/index.test.ts`, `test/adapters/googletag.test.ts`, and
      `test/integrations/gpt/ad_init.test.ts` before rebuilding the external artifact.

  Treat RCJ-PREBID-04 as its own named red-to-green checkpoint. Run the three focused
  unit files before implementation and require the new cases to fail for refresh-list
  or stale-targeting behavior; rerun them after implementation, rebuild the external
  Prebid artifact, and rerun its purity/integration contract so the rebuild cannot
  silently reintroduce refresh behavior:

  ```bash
  npm --prefix crates/trusted-server-js/lib test -- --run \
    test/integrations/prebid/index.test.ts \
    test/adapters/googletag.test.ts \
    test/integrations/gpt/ad_init.test.ts
  npm --prefix crates/trusted-server-js/lib run build:prebid-external
  node --test \
    crates/trusted-server-js/lib/test/build-prebid-external.test.mjs \
    crates/trusted-server-js/lib/test/prebid-artifact-integration.test.mjs
  ```

- [ ] **Step 6: Make the external artifact independently correct and pure.** Build exactly
      lockfile-resolved Prebid.js 10.26.0 with no TS auction, admission, render,
      targeting, or refresh behavior. The first wrapper statement arms an independent
      5,000 ms queue-drain watchdog before stamp inspection or module factories. It
      calls the then-current real object's idempotent `processQueue()` at most once per
      wrapper so every publisher callback runs exactly once even when the TS module is
      absent or incompatible.

  Expose only one own, non-enumerable, non-writable, non-configurable
  `__trustedServerArtifactV1` data property containing the exact recursively frozen
  `ExternalPrebidArtifactV1`. Same-release/content duplicates reuse the object without
  re-executing factories; a different valid release refuses the new wrapper without
  disturbing the working object. Absent, accessor, inherited, malformed, and hostile
  non-configurable descriptors never throw or stop publisher Prebid; they only make
  TS readiness incompatible and emit at most one bounded warning. Verify all manifest
  sort/uniqueness/UTF-8/count/alias/config/EID bounds and exact version 10.26.0.

  Emit exactly one 64-zero release sentinel, hash the sentinel-normalized JavaScript,
  replace it with the lowercase SHA-256 artifact release id, and separately compute
  the final-byte SHA-256/SRI. Assert no sentinel remains and do not require the
  artifact release id to equal the TSJS release id. Black-box tests bind and recheck
  the exact `pbjs` plus stamp identities, cover late valid replacement, and prove the
  external artifact contains no TS behavior or `window.__tsjs_*` handshake.

- [ ] **Step 7: Run:**

  ```bash
  npm --prefix crates/trusted-server-js/lib test -- --run test/integrations/prebid test/integrations/aps
  npm --prefix crates/trusted-server-js/lib run build:prebid-external
  node --test crates/trusted-server-js/lib/test/prebid-artifact-integration.test.mjs
  ```

- [ ] **Step 8: Commit the green Prebid/APS module as its own rollback boundary.**

  ```bash
  git add crates/trusted-server-js/lib/src/integrations/prebid/index.ts crates/trusted-server-js/lib/src/integrations/prebid/module.ts crates/trusted-server-js/lib/test/integrations/prebid/index.test.ts crates/trusted-server-js/lib/src/integrations/prebid/prebid_modules/aliases.d.ts crates/trusted-server-js/lib/src/integrations/prebid/prebid_modules/liveIntentIdSystem.ts crates/trusted-server-js/lib/src/integrations/prebid/user_id_modules.json crates/trusted-server-js/lib/src/integrations/prebid/user_id_modules.ts crates/trusted-server-js/lib/test/integrations/prebid/user_id_modules.test.ts
  git add crates/trusted-server-js/lib/src/adapters/prebid.ts crates/trusted-server-js/lib/test/adapters/prebid.test.ts crates/trusted-server-js/lib/src/integrations/aps/render.ts crates/trusted-server-js/lib/test/integrations/aps/render.test.ts crates/trusted-server-js/lib/src/core/types.ts crates/trusted-server-js/lib/src/composition/browser_test.ts crates/trusted-server-js/lib/test/composition/browser.test.ts
  git add crates/trusted-server-js/lib/build-prebid-external.mjs crates/trusted-server-js/lib/test/build-prebid-external.test.mjs crates/trusted-server-js/lib/test/prebid-artifact-integration.test.mjs
  git commit -m "Prepare Prebid and APS integration modules"
  ```

### Task 18: Prepare creative, diagnostics, and remaining integration modules

Task 18 is an umbrella only. Execute the detailed 18A creative, 18B diagnostics, 18C
remaining-integration, and 18D phase-slice sections below as four independently
reviewed red-to-green commits. The shared inventory is not authorization to stage
them as one implementation change.

**Files:**

- Modify: `crates/trusted-server-js/lib/src/integrations/creative/index.ts`
- Modify: `crates/trusted-server-js/lib/src/integrations/creative/click.ts`
- Modify: `crates/trusted-server-js/lib/src/integrations/creative/dynamic_src_guard.ts`
- Modify: `crates/trusted-server-js/lib/src/integrations/creative/iframe.ts`
- Modify: `crates/trusted-server-js/lib/src/integrations/creative/image.ts`
- Modify: `crates/trusted-server-js/lib/src/integrations/creative/proxy_sign.ts`
- Modify: `crates/trusted-server-js/lib/src/integrations/datadome/index.ts`
- Modify: `crates/trusted-server-js/lib/src/integrations/datadome/script_guard.ts`
- Modify: `crates/trusted-server-js/lib/src/integrations/didomi/index.ts`
- Modify: `crates/trusted-server-js/lib/src/integrations/google_tag_manager/index.ts`
- Modify: `crates/trusted-server-js/lib/src/integrations/google_tag_manager/script_guard.ts`
- Modify: `crates/trusted-server-js/lib/src/integrations/gpt_diagnostics/index.ts`
- Modify: `crates/trusted-server-js/lib/src/integrations/gpt_diagnostics/api.ts`
- Create: `crates/trusted-server-js/lib/src/integrations/gpt_diagnostics/data_api.ts`
- Create: `crates/trusted-server-js/lib/src/integrations/gpt_diagnostics/presentation.ts`
- Modify: `crates/trusted-server-js/lib/src/integrations/gpt_diagnostics/badges.ts`
- Modify: `crates/trusted-server-js/lib/src/integrations/gpt_diagnostics/binding.ts`
- Modify: `crates/trusted-server-js/lib/src/integrations/gpt_diagnostics/observer.ts`
- Modify: `crates/trusted-server-js/lib/src/integrations/gpt_diagnostics/overlay.ts`
- Modify: `crates/trusted-server-js/lib/src/integrations/gpt_diagnostics/store.ts`
- Modify: `crates/trusted-server-js/lib/src/integrations/lockr/index.ts`
- Modify: `crates/trusted-server-js/lib/src/integrations/lockr/script_guard.ts`
- Modify: `crates/trusted-server-js/lib/src/integrations/osano/index.ts`
- Modify: `crates/trusted-server-js/lib/src/integrations/permutive/index.ts`
- Modify: `crates/trusted-server-js/lib/src/integrations/permutive/script_guard.ts`
- Modify: `crates/trusted-server-js/lib/src/integrations/permutive/segments.ts`
- Modify: `crates/trusted-server-js/lib/src/integrations/sourcepoint/index.ts`
- Modify: `crates/trusted-server-js/lib/src/integrations/sourcepoint/script_guard.ts`
- Modify: `crates/trusted-server-js/lib/src/integrations/testlight/index.ts`
- Modify: `crates/trusted-server-js/lib/src/core/trace.ts`
- Modify: `crates/trusted-server-js/lib/test/core/trace_runtime.test.ts`
- Create: `crates/trusted-server-js/lib/src/kernel/diagnostics.ts`
- Create: `crates/trusted-server-js/lib/test/kernel/diagnostics.test.ts`
- Create: `crates/trusted-server-js/lib/src/integrations/gpt/diagnostics_facts.ts`
- Create: `crates/trusted-server-js/lib/test/integrations/gpt/diagnostics_facts.test.ts`
- Modify: `crates/trusted-server-js/lib/src/services/context.ts`
- Modify: `crates/trusted-server-js/lib/test/services/context.test.ts`
- Modify: `crates/trusted-server-js/lib/src/shared/async.ts`
- Modify: `crates/trusted-server-js/lib/src/shared/beacon_guard.ts`
- Modify: `crates/trusted-server-js/lib/src/shared/dom_insertion_dispatcher.ts`
- Modify: `crates/trusted-server-js/lib/src/shared/globals.ts`
- Modify: `crates/trusted-server-js/lib/src/shared/origin.ts`
- Modify: `crates/trusted-server-js/lib/src/shared/scheduler.ts`
- Modify: `crates/trusted-server-js/lib/src/shared/script_guard.ts`
- Modify: `crates/trusted-server-js/lib/test/shared/async.test.ts`
- Modify: `crates/trusted-server-js/lib/test/shared/beacon_guard.test.ts`
- Modify: `crates/trusted-server-js/lib/test/shared/dom_insertion_dispatcher.test.ts`
- Modify: `crates/trusted-server-js/lib/test/shared/scheduler.test.ts`
- Modify: `crates/trusted-server-js/lib/test/integrations/creative/click.test.ts`
- Modify: `crates/trusted-server-js/lib/test/integrations/creative/helpers.ts`
- Modify: `crates/trusted-server-js/lib/test/integrations/creative/iframe.test.ts`
- Modify: `crates/trusted-server-js/lib/test/integrations/creative/image.test.ts`
- Modify: `crates/trusted-server-js/lib/test/integrations/creative/proxy_sign.test.ts`
- Modify: `crates/trusted-server-js/lib/test/integrations/datadome/script_guard.test.ts`
- Modify: `crates/trusted-server-js/lib/test/integrations/didomi/index.test.ts`
- Modify: `crates/trusted-server-js/lib/test/integrations/google_tag_manager/script_guard.test.ts`
- Modify: `crates/trusted-server-js/lib/test/integrations/gpt_diagnostics/index.test.ts`
- Modify: `crates/trusted-server-js/lib/test/integrations/gpt_diagnostics/api.test.ts`
- Create: `crates/trusted-server-js/lib/test/integrations/gpt_diagnostics/data_api.test.ts`
- Create: `crates/trusted-server-js/lib/test/integrations/gpt_diagnostics/presentation.test.ts`
- Modify: `crates/trusted-server-js/lib/test/integrations/gpt_diagnostics/badges.test.ts`
- Modify: `crates/trusted-server-js/lib/test/integrations/gpt_diagnostics/binding.test.ts`
- Modify: `crates/trusted-server-js/lib/test/integrations/gpt_diagnostics/bootstrap.test.ts`
- Modify: `crates/trusted-server-js/lib/test/integrations/gpt_diagnostics/observer.test.ts`
- Modify: `crates/trusted-server-js/lib/test/integrations/gpt_diagnostics/overlay.test.ts`
- Modify: `crates/trusted-server-js/lib/test/integrations/gpt_diagnostics/store.test.ts`
- Modify: `crates/trusted-server-js/lib/test/integrations/gpt_diagnostics/types.test.ts`
- Modify: `crates/trusted-server-js/lib/test/integrations/lockr/script_guard.test.ts`
- Modify: `crates/trusted-server-js/lib/test/integrations/osano/index.test.ts`
- Modify: `crates/trusted-server-js/lib/test/integrations/permutive/segments.test.ts`
- Modify: `crates/trusted-server-js/lib/test/integrations/sourcepoint/index.test.ts`
- Modify: `crates/trusted-server-js/lib/test/integrations/sourcepoint/script_guard.test.ts`
- Create: `crates/trusted-server-js/lib/test/integrations/testlight/index.test.ts`
- Modify: `crates/trusted-server-js/lib/test/build/release-v1.test.mjs`
- Modify: `crates/trusted-server-js/lib/build-all.mjs`
- Modify: `crates/trusted-server-core/src/publisher.rs`
- Modify: `crates/trusted-server-core/src/trace_cookie.rs`
- Modify: `crates/trusted-server-core/src/integrations/datadome.rs`
- Modify: `crates/trusted-server-core/src/integrations/datadome/protection.rs`
- Modify: `crates/trusted-server-core/src/integrations/datadome/protection_scope.rs`
- Modify: `crates/trusted-server-core/src/integrations/didomi.rs`
- Modify: `crates/trusted-server-core/src/integrations/google_tag_manager.rs`
- Modify: `crates/trusted-server-core/src/integrations/gpt_diagnostics.rs`
- Modify: `crates/trusted-server-core/src/integrations/gpt_diagnostics_bootstrap.js`
- Modify: `crates/trusted-server-core/src/integrations/lockr.rs`
- Modify: `crates/trusted-server-core/src/integrations/mod.rs`
- Modify: `crates/trusted-server-core/src/integrations/osano.rs`
- Modify: `crates/trusted-server-core/src/integrations/permutive.rs`
- Modify: `crates/trusted-server-core/src/integrations/sourcepoint.rs`
- Modify: `crates/trusted-server-core/src/integrations/testlight.rs`
- Modify: `crates/trusted-server-js/lib/src/composition/browser_test.ts`
- Modify: `crates/trusted-server-js/lib/test/composition/browser.test.ts`

#### Task 18A: Rebuild creative as one independently green integration module

**Task 18A files:**

- `crates/trusted-server-js/lib/src/integrations/creative/index.ts`
- `crates/trusted-server-js/lib/src/integrations/creative/click.ts`
- `crates/trusted-server-js/lib/src/integrations/creative/dynamic_src_guard.ts`
- `crates/trusted-server-js/lib/src/integrations/creative/iframe.ts`
- `crates/trusted-server-js/lib/src/integrations/creative/image.ts`
- `crates/trusted-server-js/lib/src/integrations/creative/proxy_sign.ts`
- `crates/trusted-server-js/lib/src/shared/async.ts`
- `crates/trusted-server-js/lib/src/shared/dom_insertion_dispatcher.ts`
- `crates/trusted-server-js/lib/src/shared/origin.ts`
- `crates/trusted-server-js/lib/src/shared/scheduler.ts`
- `crates/trusted-server-js/lib/src/shared/script_guard.ts`
- `crates/trusted-server-js/lib/test/integrations/creative/click.test.ts`
- `crates/trusted-server-js/lib/test/integrations/creative/iframe.test.ts`
- `crates/trusted-server-js/lib/test/integrations/creative/image.test.ts`
- `crates/trusted-server-js/lib/test/integrations/creative/proxy_sign.test.ts`
- `crates/trusted-server-js/lib/test/integrations/creative/helpers.ts`
- `crates/trusted-server-js/lib/test/shared/async.test.ts`
- `crates/trusted-server-js/lib/test/shared/dom_insertion_dispatcher.test.ts`
- `crates/trusted-server-js/lib/test/shared/scheduler.test.ts`
- `crates/trusted-server-js/lib/src/composition/browser_test.ts`
- `crates/trusted-server-js/lib/test/composition/browser.test.ts`

- [ ] **Step A1: Add the failing creative-only composition and lifecycle corpus.** Cover
      boot validation, guard enablement combinations, automatic scans, wrapper and
      observer ownership, hostile callbacks, startup rollback, disposal, and every
      existing click/image/iframe/proxy-sign behavior. Run creative alone and inside
      a manifest composition without modifying any other integration.

- [ ] **Step A2: Run the creative slice before implementation and prove the new composition**
      **cases fail for the missing module lifecycle.**

  ```bash
  npm --prefix crates/trusted-server-js/lib test -- --run test/integrations/creative test/composition/browser.test.ts
  npm --prefix crates/trusted-server-js/lib test -- --run test/shared/async.test.ts test/shared/dom_insertion_dispatcher.test.ts test/shared/scheduler.test.ts
  ```

- [ ] **Step A3: Convert only creative into a thin integration module.** Its
      `_registerIntegration({abi:1,id,phase:'critical',releaseId,prepare})` call is pure registration;
      `prepare(ctx)` is inert and Promise-returning; `activate(ctx)` is synchronous,
      pre-registers disposal before every reversible mutation, and contributes at
      most one `afterCommit` callback. Keep shipped entry-point side effects unchanged
      until Task 19.

- [ ] **Step A4: Rebuild creative startup around the exact frozen `CreativeBootV1`.** Validate
      the complete plain-object shape, defaults, disabled/manifest mismatch, unknown
      keys, accessors, prototypes, and literals before preparation. Activation installs
      the click guard when `clickGuard` is true and dynamic image/iframe guards when
      `renderGuard` is true, with no baseline DOM rewrite. A still-loading document
      receives one owned `DOMContentLoaded` rescan; an already-ready document stages
      that scan in the module's single `afterCommit`. Disabled creative and
      enabled-with-both-guards-false perform zero wrappers, observers, listeners,
      scans, or DOM mutation. Disposal compare-restores only the exact installed
      wrapper and clears owned DOM state once.

  Preserve sanitization as opt-in/default-off and rewriting as its independent
  existing policy across direct, SSAT, cache, and auction paths. Cover dynamic-node
  guards, sandbox attributes, font/CORS/body/base behavior, opaque-origin click
  recovery through `/first-party/proxy-rebuild`, validated absolute HTTP(S), and
  rejection of credentials, malformed values, and non-network schemes. For each
  rewritten independent creative document that needs a guard, emit the complete
  document-local boot controller plus exactly one authenticated critical tag for
  core + `render_runtime` + `creative`, with no other critical or deferred module.
  Cover body and body-less insertion, exact tag identity/source/manifest/release,
  the immutable opaque-origin stamp, and zero TSJS injection when creative is
  disabled or both guards are false. Delete the
  mutable/install creative globals only in Task 22.

- [ ] **Step A5: Run and commit the creative slice before diagnostics or other modules.**

  ```bash
  npm --prefix crates/trusted-server-js/lib test -- --run test/integrations/creative test/composition/browser.test.ts
  npm --prefix crates/trusted-server-js/lib test -- --run test/shared/async.test.ts test/shared/dom_insertion_dispatcher.test.ts test/shared/scheduler.test.ts
  npm --prefix crates/trusted-server-js/lib run lint
  npm --prefix crates/trusted-server-js/lib run typecheck
  git add crates/trusted-server-js/lib/src/integrations/creative/index.ts crates/trusted-server-js/lib/src/integrations/creative/click.ts crates/trusted-server-js/lib/src/integrations/creative/dynamic_src_guard.ts crates/trusted-server-js/lib/src/integrations/creative/iframe.ts crates/trusted-server-js/lib/src/integrations/creative/image.ts crates/trusted-server-js/lib/src/integrations/creative/proxy_sign.ts
  git add crates/trusted-server-js/lib/test/integrations/creative/click.test.ts crates/trusted-server-js/lib/test/integrations/creative/helpers.ts crates/trusted-server-js/lib/test/integrations/creative/iframe.test.ts crates/trusted-server-js/lib/test/integrations/creative/image.test.ts crates/trusted-server-js/lib/test/integrations/creative/proxy_sign.test.ts
  git add crates/trusted-server-js/lib/src/shared/async.ts crates/trusted-server-js/lib/src/shared/dom_insertion_dispatcher.ts crates/trusted-server-js/lib/src/shared/origin.ts crates/trusted-server-js/lib/src/shared/scheduler.ts crates/trusted-server-js/lib/src/shared/script_guard.ts crates/trusted-server-js/lib/test/shared/async.test.ts crates/trusted-server-js/lib/test/shared/dom_insertion_dispatcher.test.ts crates/trusted-server-js/lib/test/shared/scheduler.test.ts crates/trusted-server-js/lib/src/composition/browser_test.ts crates/trusted-server-js/lib/test/composition/browser.test.ts
  git commit -m "Prepare the creative integration module"
  ```

#### Task 18B: Rebuild diagnostics transport, producers, and consumers

**Task 18B files:**

Unlabelled paths inherit their create/modify status from the Task 18 umbrella;
`gpt_diagnostics/module.ts` is additionally created here before Task 18D modifies it.

- `crates/trusted-server-core/src/publisher.rs`
- `crates/trusted-server-core/src/trace_cookie.rs`
- `crates/trusted-server-core/src/integrations/gpt_diagnostics.rs`
- `crates/trusted-server-core/src/integrations/gpt_diagnostics_bootstrap.js`
- `crates/trusted-server-js/lib/src/core/trace.ts`
- `crates/trusted-server-js/lib/test/core/trace_runtime.test.ts`
- `crates/trusted-server-js/lib/src/kernel/diagnostics.ts`
- `crates/trusted-server-js/lib/test/kernel/diagnostics.test.ts`
- `crates/trusted-server-js/lib/src/kernel/release_catalog.ts`
- `crates/trusted-server-js/lib/test/kernel/release_catalog.test.ts`
- `crates/trusted-server-js/build.rs`
- `crates/trusted-server-js/src/bundle.rs`
- `crates/trusted-server-js/src/lib.rs`
- `crates/trusted-server-js/lib/build-all.mjs`
- `crates/trusted-server-js/lib/src/services/render.ts`
- `crates/trusted-server-js/lib/test/services/render.test.ts`
- `crates/trusted-server-js/lib/src/services/slots.ts`
- `crates/trusted-server-js/lib/test/services/slots.test.ts`
- `crates/trusted-server-js/lib/src/adapters/googletag.ts`
- `crates/trusted-server-js/lib/test/adapters/googletag.test.ts`
- `crates/trusted-server-js/lib/src/integrations/gpt/diagnostics_facts.ts`
- `crates/trusted-server-js/lib/test/integrations/gpt/diagnostics_facts.test.ts`
- `crates/trusted-server-js/lib/src/integrations/gpt/module.ts`
- `crates/trusted-server-js/lib/test/integrations/gpt/module.test.ts`
- `crates/trusted-server-js/lib/src/integrations/render_runtime/module.ts`
- `crates/trusted-server-js/lib/test/integrations/render_runtime/module.test.ts`
- `crates/trusted-server-js/lib/src/integrations/gpt_diagnostics/index.ts`
- Create: `crates/trusted-server-js/lib/src/integrations/gpt_diagnostics/module.ts`
- `crates/trusted-server-js/lib/src/integrations/gpt_diagnostics/api.ts`
- Create: `crates/trusted-server-js/lib/src/integrations/gpt_diagnostics/data_api.ts`
- Create: `crates/trusted-server-js/lib/src/integrations/gpt_diagnostics/presentation.ts`
- `crates/trusted-server-js/lib/src/integrations/gpt_diagnostics/badges.ts`
- `crates/trusted-server-js/lib/src/integrations/gpt_diagnostics/binding.ts`
- `crates/trusted-server-js/lib/src/integrations/gpt_diagnostics/observer.ts`
- `crates/trusted-server-js/lib/src/integrations/gpt_diagnostics/overlay.ts`
- `crates/trusted-server-js/lib/src/integrations/gpt_diagnostics/store.ts`
- `crates/trusted-server-js/lib/test/integrations/gpt_diagnostics/index.test.ts`
- `crates/trusted-server-js/lib/test/integrations/gpt_diagnostics/api.test.ts`
- Create: `crates/trusted-server-js/lib/test/integrations/gpt_diagnostics/data_api.test.ts`
- Create: `crates/trusted-server-js/lib/test/integrations/gpt_diagnostics/presentation.test.ts`
- `crates/trusted-server-js/lib/test/integrations/gpt_diagnostics/module.test.ts`
- `crates/trusted-server-js/lib/test/integrations/gpt_diagnostics/badges.test.ts`
- `crates/trusted-server-js/lib/test/integrations/gpt_diagnostics/binding.test.ts`
- `crates/trusted-server-js/lib/test/integrations/gpt_diagnostics/bootstrap.test.ts`
- `crates/trusted-server-js/lib/test/integrations/gpt_diagnostics/observer.test.ts`
- `crates/trusted-server-js/lib/test/integrations/gpt_diagnostics/overlay.test.ts`
- `crates/trusted-server-js/lib/test/integrations/gpt_diagnostics/store.test.ts`
- `crates/trusted-server-js/lib/test/integrations/gpt_diagnostics/types.test.ts`
- `crates/trusted-server-js/lib/src/composition/browser_test.ts`
- `crates/trusted-server-js/lib/src/composition/browser_test_gpt_diagnostics.ts`
- `crates/trusted-server-js/lib/test/composition/browser.test.ts`

- [ ] **Step B1: Add the failing diagnostics slice before implementation.** Cover the
      closure-private core ingress with exact frozen `{publish,dispose}` own surface
      and explicit absence of `subscribe`, consumer ids, capacity, queue, scheduler,
      timer, and overflow hooks. Add RED boundary cases for total nodes 511/512/513,
      depth 15/16/17, 127/128/129-byte property names, 4,095/4,096/4,097-byte string
      values, multibyte UTF-8, ordinary/null-prototype records, and dense arrays.
      Reject sparse/extra-property arrays, accessors, symbols, functions, `undefined`,
      bigint, non-finite numbers, custom prototypes, aliases/cycles, hostile traps,
      and injected copy/freeze failure without reducer entry. Prove accepted values
      are fresh deeply frozen copies, reducer/reporter throws still return `true`,
      dispose is idempotent, and retained stale-runtime publishers return `false`.

  Add RED capability tests proving `trace.v1` has no `attachPresentation`, only
  `diagnostics_presentation` receives the distinct `trace.presentation.v1`, and
  `gpt_diagnostics` consumes only `runtime.v1` plus `gpt.events.v1`. Cover private
  factory/controls/source exact shapes; non-callable, reentrant, duplicate, malformed,
  and missing-listener failure; first-listener preservation; unsubscribe/resubscribe;
  initial replay before live delivery; coalescing; detach/owner disposal; retained
  empty snapshots; late callbacks; and no use of the public 32-subscriber capacity.

  Add RED GPT projection tests for canonical `gt1_` tokens, per-object cycle ordinals,
  exact `{token,cycleOrdinal}` joins, object-token separation on `gpt.events.v1`,
  same-object refreshes, handoff, replacement, collision/exhaustion, the ten-cycle
  adapter ledger, `unknownPriorCycle`, the 256-entry core binding map, history-only
  late enrichment, and ambiguous callback omission before/after the next cycle's
  start/completion. Add the existing render-trace, GPT-fact, producer-ordering,
  inactive-zero-effect, composition, and `ts_console` request-pipeline cases.

- [ ] **Step B2: Run the diagnostics slice and prove it fails at the missing core ingress,**
      **producer wiring, and server session mechanics.**

  ```bash
  cargo test-fastly trace_cookie
  cargo test-fastly ts_console
  npm --prefix crates/trusted-server-js/lib test -- --run test/kernel/diagnostics.test.ts test/kernel/release_catalog.test.ts test/core/trace_runtime.test.ts test/services/render.test.ts test/services/slots.test.ts test/adapters/googletag.test.ts test/integrations/gpt/diagnostics_facts.test.ts test/integrations/gpt/module.test.ts test/integrations/render_runtime/module.test.ts test/integrations/gpt_diagnostics test/composition/browser.test.ts
  ```

- [ ] **Step B3: Implement the core-only ingress and exact public trace surface.**
      Replace the internal bus with a closure-private runtime ingress in
      `kernel/diagnostics.ts`; its exact frozen facade contains only `publish` and
      `dispose` and is never reachable from `window.tsjs`, `TsjsApi`, diagnostics
      snapshots, or publisher callbacks. Delete integration ids, manifest admission,
      subscriptions, pending observations, schedulers/timers, overflow handling, and
      their capacity constants from TypeScript, generated Rust metadata, `bundle.rs`,
      and crate exports.

  Implement the spec §5.8 data-tree copier with these exact constants and no
  producer-owned references:

  ```ts
  const MAX_DIAGNOSTICS_OBSERVATION_DEPTH = 16
  const MAX_DIAGNOSTICS_OBSERVATION_NODES = 512
  const MAX_DIAGNOSTICS_PROPERTY_NAME_BYTES = 128
  const MAX_DIAGNOSTICS_STRING_BYTES = 4096
  ```

  Count the root and every property/element value as a node; use own data descriptors,
  null-prototype output records, local dense arrays, UTF-8 byte counts, one global
  seen-set that rejects both cycles and repeated references, and deep freeze.
  `publish` never throws: malformed, over-
  bound, copy/freeze-failed, disposed, or stale-owner input returns `false` before the
  reducer. Accepted input invokes the core reducer exactly once synchronously;
  reducer/reporter failure is isolated and still returns `true`. `dispose` clears
  retained callbacks and makes every old facade inert.

  `tsjs.diagnostics.renderTrace` exposes only frozen `current()`, `history()`, and
  `subscribe()`. Keep current state keyed by exact slot and capped by the 256-slot
  navigation registry; prune on disposal. Keep document-runtime history at 200, one
  row per physical impression, monotonic `count`/global `seq`, immutable `at`, and
  non-weakening enrichment. Remove stale DOM stamp fields/badges on update and preserve
  bounded overlay/export failure isolation.

  Commit correctness state before public delivery. Capture subscriber ids and enqueue
  frozen full records asynchronously in a 200-entry FIFO keyed by `seq`; same-sequence
  enrichment replaces the pending record and captured ids without reordering. One
  owned zero-delay task drains FIFO. Enforce callable-before-capacity validation,
  32-live-subscriber cap, idempotent unsubscribe, unsubscribe-before-delivery,
  registration-during-dispatch, callback throw isolation, and 199/200/201 overflow.
  Emit no `CustomEvent`, mutable trace global, or compatibility alias.

  In `render_runtime`, provide two different exact broker values. `trace.v1` retains
  only correctness record/enrich/prune/diagnostics plus ingress publication.
  `trace.presentation.v1` contains only this method and is granted only to the
  deferred presentation module:

  ```ts
  interface RenderTracePresentationSourceV1 {
    current(): Readonly<Record<string, Readonly<RenderTraceRecord>>>
    history(): readonly Readonly<RenderTraceRecord>[]
    subscribe(listener: () => void): () => void
  }

  interface TracePresentationCapabilityV1 {
    attachPresentation(
      factory: (
        source: Readonly<RenderTracePresentationSourceV1>
      ) => Readonly<{ dispose(): void }>
    ): () => void
  }
  ```

  Implement callable-before-state checks, one live attachment and listener,
  state-preserving duplicate failure, synchronous initial snapshot, post-commit
  zero-delay coalescing, rollback/retry, exact frozen controls, idempotent
  unsubscribe/detach/dispose, generation invalidation, cancelled tasks, and inert
  retained references. After detach, retained `current()`/`history()` return frozen
  empty snapshots and retained `subscribe()` rejects. Presentation never consumes a
  public trace subscriber slot and its failure cannot affect the trace store.

- [ ] **Step B4: Preserve GPT diagnostics through the adapter event stream.** Validate exact
      `DiagnosticsBootV1` plus manifest activation before any listener/buffer exists.
      When active, core owns the six documented GPT observations before TS requests,
      buffers 512 raw facts until module activation, then replays and releases the
      buffer. When inactive, require zero diagnostics-added listeners, DOM, timers,
      observers, API, storage, or network work beyond the two correctness listeners.
      Preserve exact physical-slot binding/replacement, per-slot monotonic request
      numbers, callback truth/timing, frozen exports, Shadow DOM overlay, badges, SPA,
      privacy, and non-interference.

  The critical module consumes only `runtime.v1` and `gpt.events.v1`; remove every
  `trace.v1` lookup and ingress subscription. GPT owns the bounded early fact buffer
  in `integrations/gpt/diagnostics_facts.ts`. Its facts retain the frozen opaque
  per-physical-object token because this direct stream does not cross the generic
  ingress. Deferred `gpt_diagnostics/presentation.ts` attaches separately to
  `gpt_diag.v1` for GPT UI and `trace.presentation.v1` for trace UI; neither
  presentation path is imported by production core.

  Bound the store to 64 slot objects, ten cycles per slot, and 128 callback issues.
  Expose only `tsjs.diagnostics.gpt`, with `snapshot()` plus the shared 32-subscriber
  limit. Public delivery uses a separate one-entry latest-snapshot notifier on one
  owned zero-delay task; 0/1/2-update coalescing, captured ids, unsubscribe/disposal,
  slow/throwing listeners, and callback-stack isolation are executable tests. No
  storage, upload, old flag, runtime expando, or `tsjs.gptDiagnostics` alias remains
  after Task 22.

- [ ] **Step B5: Rebuild and unit-test the server-owned `ts_console` browser-session mechanics.**
      On eligible GET document navigations, accept exactly one case-sensitive
      `ts_console=1|true` enable directive or `0|false` disable directive;
      duplicate, conflicting, empty, or unknown values fail closed for that response.
      Strip every reserved pair before publisher/origin/cookie/auction handling,
      preserve all unrelated path/query/fragment data, and set or clear only the
      host-only `Secure`, `HttpOnly`, `SameSite=Lax` session cookie. Assert same-origin
      tab/session behavior, disabled-by-default behavior, and that frozen
      `DiagnosticsBootV1.gpt.active` is the only browser-visible activation result.
      Write request-pipeline tests named with `ts_console` before implementation and
      prove they fail for directive stripping, method/document eligibility,
      unrelated URL preservation, exact `Set-Cookie`, clearing, and boot-emitter
      activation. These assertions must exercise `publisher.rs`, not only the
      isolated trace-cookie parser.

- [ ] **Step B6: Wire every diagnostics producer explicitly after its correctness commit.**
      `RenderAttempt` offers a data-tree candidate to ingress only after terminal or
      accepted-artifact state commits; ingress owns validation/copy/freeze. The sole
      GPT adapter publishes its six object-identity facts to `gpt.events.v1` only after
      adapter bookkeeping commits. The GPT integration separately projects an
      ingress-safe trace fact; neither path calls public subscribers inline or can
      delay, reject, retry, or mutate rendering/GPT behavior.

  Mint `GptSlotTokenV1` in the adapter as `gt1_` plus the canonical lower-case base-36
  runtime ordinal 1..4,294,967,295 (11 bytes maximum), stable for one physical object
  in the adapter `WeakMap`, copied to `SlotRecord.traceToken`, and never reused in the
  runtime. Keep the direct `gpt.events.v1` opaque object token independent. Mint a
  separate per-object `GptTraceCycleOrdinalV1` 1..4,294,967,295 only for an
  unambiguous §2.4 physical request. Retain at most ten cycle records per object and
  set `unknownPriorCycle` when pruning; exhaustion/collision/ambiguity disables only
  the affected trace projection and reports locally without changing GPT delivery.

  Project the exact data identity
  `slot:{token,cycleOrdinal,elementId?}`. A non-request fact receives an ordinal only
  from the lifecycle owner's exact cycle handle or one uniquely eligible retained
  cycle—never from newest timing, element id, or physical token alone. The trace
  reducer keys its 256-entry impression binding map by the compound pair and stores
  `{slotId,navigationGeneration,baselineSeq?,historySeq?,state}`. It never evicts an
  open binding; it prunes completed/retired oldest-first. Unresolved, stale,
  over-capacity, pruned, or multi-cycle-ambiguous facts are diagnostics-only drops.
  A uniquely matched retired fact may enrich only its retained old history row and
  can never create/rebind/mutate new current state.

  Test token/cycle zero, boundary, overflow, collision, stability, handoff,
  replacement, per-object exhaustion, 9/10/11 cycle pruning, 255/256/257 core map,
  same-object consecutive refreshes, and every old response/render/onload/viewability/
  visibility callback ordering before/after the next start and completion. Also test
  inactive zero-effects, producer/reducer/reporter throw isolation, event ordering,
  enrichment replacement, GPT fact-buffer release, navigation/runtime disposal, and
  absence of `CustomEvent`, mutable globals, ingress subscribers, or a second GPT
  listener set.

- [ ] **Step B7: Run and commit diagnostics transport, producer, and consumer wiring as one**
      independently green slice.

  ```bash
  cargo test-fastly trace_cookie
  cargo test-fastly ts_console
  npm --prefix crates/trusted-server-js/lib test -- --run test/kernel/diagnostics.test.ts test/kernel/release_catalog.test.ts test/core/trace_runtime.test.ts test/services/render.test.ts test/services/slots.test.ts test/adapters/googletag.test.ts
  npm --prefix crates/trusted-server-js/lib test -- --run test/integrations/gpt/diagnostics_facts.test.ts test/integrations/gpt/module.test.ts test/integrations/render_runtime/module.test.ts test/integrations/gpt_diagnostics
  npm --prefix crates/trusted-server-js/lib test -- --run test/composition/browser.test.ts test/composition/maximal-runtime.test.ts
  npm --prefix crates/trusted-server-js/lib run build
  npm --prefix crates/trusted-server-js/lib run test:release
  npm --prefix crates/trusted-server-js/lib run lint
  npm --prefix crates/trusted-server-js/lib run typecheck
  git add crates/trusted-server-core/src/publisher.rs crates/trusted-server-core/src/trace_cookie.rs crates/trusted-server-core/src/integrations/gpt_diagnostics.rs crates/trusted-server-core/src/integrations/gpt_diagnostics_bootstrap.js
  git add crates/trusted-server-js/lib/src/kernel/diagnostics.ts crates/trusted-server-js/lib/test/kernel/diagnostics.test.ts crates/trusted-server-js/lib/src/kernel/release_catalog.ts crates/trusted-server-js/lib/test/kernel/release_catalog.test.ts crates/trusted-server-js/build.rs crates/trusted-server-js/src/bundle.rs crates/trusted-server-js/src/lib.rs crates/trusted-server-js/lib/build-all.mjs
  git add crates/trusted-server-js/lib/src/core/trace.ts crates/trusted-server-js/lib/test/core/trace_runtime.test.ts crates/trusted-server-js/lib/src/services/render.ts crates/trusted-server-js/lib/test/services/render.test.ts crates/trusted-server-js/lib/src/services/slots.ts crates/trusted-server-js/lib/test/services/slots.test.ts crates/trusted-server-js/lib/src/adapters/googletag.ts crates/trusted-server-js/lib/test/adapters/googletag.test.ts
  git add crates/trusted-server-js/lib/src/integrations/gpt/diagnostics_facts.ts crates/trusted-server-js/lib/test/integrations/gpt/diagnostics_facts.test.ts crates/trusted-server-js/lib/src/integrations/gpt/module.ts crates/trusted-server-js/lib/test/integrations/gpt/module.test.ts crates/trusted-server-js/lib/src/integrations/render_runtime/module.ts crates/trusted-server-js/lib/test/integrations/render_runtime/module.test.ts
  git add crates/trusted-server-js/lib/src/integrations/gpt_diagnostics/index.ts crates/trusted-server-js/lib/src/integrations/gpt_diagnostics/module.ts crates/trusted-server-js/lib/src/integrations/gpt_diagnostics/api.ts crates/trusted-server-js/lib/src/integrations/gpt_diagnostics/data_api.ts crates/trusted-server-js/lib/src/integrations/gpt_diagnostics/presentation.ts crates/trusted-server-js/lib/src/integrations/gpt_diagnostics/badges.ts crates/trusted-server-js/lib/src/integrations/gpt_diagnostics/binding.ts crates/trusted-server-js/lib/src/integrations/gpt_diagnostics/observer.ts crates/trusted-server-js/lib/src/integrations/gpt_diagnostics/overlay.ts crates/trusted-server-js/lib/src/integrations/gpt_diagnostics/store.ts
  git add crates/trusted-server-js/lib/test/integrations/gpt_diagnostics/index.test.ts crates/trusted-server-js/lib/test/integrations/gpt_diagnostics/api.test.ts crates/trusted-server-js/lib/test/integrations/gpt_diagnostics/data_api.test.ts crates/trusted-server-js/lib/test/integrations/gpt_diagnostics/presentation.test.ts crates/trusted-server-js/lib/test/integrations/gpt_diagnostics/module.test.ts crates/trusted-server-js/lib/test/integrations/gpt_diagnostics/badges.test.ts crates/trusted-server-js/lib/test/integrations/gpt_diagnostics/binding.test.ts crates/trusted-server-js/lib/test/integrations/gpt_diagnostics/bootstrap.test.ts crates/trusted-server-js/lib/test/integrations/gpt_diagnostics/observer.test.ts crates/trusted-server-js/lib/test/integrations/gpt_diagnostics/overlay.test.ts crates/trusted-server-js/lib/test/integrations/gpt_diagnostics/store.test.ts crates/trusted-server-js/lib/test/integrations/gpt_diagnostics/types.test.ts
  git add crates/trusted-server-js/lib/src/composition/browser_test.ts crates/trusted-server-js/lib/src/composition/browser_test_gpt_diagnostics.ts crates/trusted-server-js/lib/test/composition/browser.test.ts
  git commit -m "Make diagnostics ingress core-only"
  ```

#### Task 18C: Migrate the remaining integrations and maximal manifest

**Task 18C files:**

Unlabelled paths inherit their status from the Task 18 umbrella; the three explicitly
labelled `module.ts` paths are created here before Task 18D modifies them.

- `crates/trusted-server-js/lib/src/integrations/datadome/index.ts`
- `crates/trusted-server-js/lib/src/integrations/datadome/script_guard.ts`
- `crates/trusted-server-js/lib/src/integrations/didomi/index.ts`
- `crates/trusted-server-js/lib/src/integrations/google_tag_manager/index.ts`
- `crates/trusted-server-js/lib/src/integrations/google_tag_manager/script_guard.ts`
- `crates/trusted-server-js/lib/src/integrations/lockr/index.ts`
- `crates/trusted-server-js/lib/src/integrations/lockr/script_guard.ts`
- `crates/trusted-server-js/lib/src/integrations/osano/index.ts`
- Create: `crates/trusted-server-js/lib/src/integrations/osano/module.ts`
- `crates/trusted-server-js/lib/src/integrations/permutive/index.ts`
- Create: `crates/trusted-server-js/lib/src/integrations/permutive/module.ts`
- `crates/trusted-server-js/lib/src/integrations/permutive/script_guard.ts`
- `crates/trusted-server-js/lib/src/integrations/permutive/segments.ts`
- `crates/trusted-server-js/lib/src/integrations/sourcepoint/index.ts`
- Create: `crates/trusted-server-js/lib/src/integrations/sourcepoint/module.ts`
- `crates/trusted-server-js/lib/src/integrations/sourcepoint/script_guard.ts`
- `crates/trusted-server-js/lib/src/integrations/testlight/index.ts`
- `crates/trusted-server-js/lib/test/integrations/datadome/script_guard.test.ts`
- `crates/trusted-server-js/lib/test/integrations/didomi/index.test.ts`
- `crates/trusted-server-js/lib/test/integrations/google_tag_manager/script_guard.test.ts`
- `crates/trusted-server-js/lib/test/integrations/lockr/script_guard.test.ts`
- `crates/trusted-server-js/lib/test/integrations/osano/index.test.ts`
- `crates/trusted-server-js/lib/test/integrations/permutive/segments.test.ts`
- `crates/trusted-server-js/lib/test/integrations/sourcepoint/index.test.ts`
- `crates/trusted-server-js/lib/test/integrations/sourcepoint/script_guard.test.ts`
- `crates/trusted-server-js/lib/test/integrations/testlight/index.test.ts`
- `crates/trusted-server-js/lib/test/build/release-v1.test.mjs`
- `crates/trusted-server-js/lib/src/services/context.ts`
- `crates/trusted-server-js/lib/test/services/context.test.ts`
- `crates/trusted-server-js/lib/src/shared/beacon_guard.ts`
- `crates/trusted-server-js/lib/src/shared/globals.ts`
- `crates/trusted-server-js/lib/src/shared/script_guard.ts`
- `crates/trusted-server-js/lib/test/shared/beacon_guard.test.ts`
- `crates/trusted-server-js/lib/src/composition/browser_test.ts`
- `crates/trusted-server-js/lib/test/composition/browser.test.ts`
- `crates/trusted-server-js/lib/build-all.mjs`
- `crates/trusted-server-core/src/integrations/datadome.rs`
- `crates/trusted-server-core/src/integrations/datadome/protection.rs`
- `crates/trusted-server-core/src/integrations/datadome/protection_scope.rs`
- `crates/trusted-server-core/src/integrations/didomi.rs`
- `crates/trusted-server-core/src/integrations/google_tag_manager.rs`
- `crates/trusted-server-core/src/integrations/lockr.rs`
- `crates/trusted-server-core/src/integrations/osano.rs`
- `crates/trusted-server-core/src/integrations/permutive.rs`
- `crates/trusted-server-core/src/integrations/sourcepoint.rs`
- `crates/trusted-server-core/src/integrations/testlight.rs`
- `crates/trusted-server-core/src/integrations/mod.rs`

- [ ] **Step C1: Add the failing remaining-integration and maximal-manifest corpus.** Preserve
      each `rc/july` integration behavior exactly. Cover DataDome
      script/preload path rewriting; Didomi absolute SDK path without config clobber;
      GTM script/preload and GA beacon/fetch rewriting; Lockr bounded readiness and API
      host; Osano USP/GPP/TCF marker ownership and lifecycle; Permutive bounded
      readiness/API host and at-most-100 normalized segments; Sourcepoint optional SDK
      plus GPP storage/marker lifecycle; and Testlight preexisting/later callbacks,
      invalid entries, and throw isolation. Run unchanged pre-cutover fixtures beside
      module-composed fixtures. Permutive and any other auction-context contributor
      register only through the injected runtime service during activation and remove
      that contribution through the pre-registered disposer; no import-time global
      provider survives failed activation or module/runtime disposal, and SPA
      navigation does not register a duplicate.

  Load core followed by every server-declared integration in manifest order and
  assert one runtime, no unknown id, no duplicate activation, exact reverse-order
  disposal, and no leaked timer, listener, wrapper, observer, context provider, or
  queued continuation. Run each module alone and in the maximal manifest with missing
  globals, timeout, malformed config/consent/storage, matcher false positives,
  callback throws, startup failure, and cross-integration isolation.

- [ ] **Step C2: Run the complete new corpus before conversion and prove the module lifecycle**
      **and maximal-manifest cases fail.**

  ```bash
  npm --prefix crates/trusted-server-js/lib test -- --run test/integrations/datadome test/integrations/didomi test/integrations/google_tag_manager test/integrations/lockr test/integrations/osano test/integrations/permutive test/integrations/sourcepoint test/integrations/testlight
  npm --prefix crates/trusted-server-js/lib test -- --run test/services/context.test.ts test/shared/beacon_guard.test.ts test/composition/browser.test.ts
  npm --prefix crates/trusted-server-js/lib run build
  npm --prefix crates/trusted-server-js/lib run test:release
  cargo test-fastly publisher
  ```

- [ ] **Step C3: Convert only the remaining integrations into thin modules.** Each
      `_registerIntegration({abi:1,id,phase,releaseId,prepare})` call is pure registration;
      `prepare(ctx)` is inert and Promise-returning; `activate(ctx)` is synchronous,
      pre-registers disposal before reversible mutation, and contributes at most one
      `afterCommit`. Shared helpers must preserve each integration's exact matcher,
      startup order, failure isolation, and disposal semantics.

- [ ] **Step C4: Generate and test the prospective manifest member list/order from the exact**
      enabled bundle list. Embed the same release id in core and every integration
      IIFE. Add failures for integration before core, unknown/missing/duplicate member,
      malformed/unsorted/oversized manifest, wrong release, preparation or activation
      failure, duplicate `afterCommit`, and catalog-derived 13/14/15 critical plus
      19/20/21 total capacity. Deferred entries do not share the critical watchdog.
      Production manifest emission starts only in Task 19.

- [ ] **Step C5: Run the complete remaining-integration and maximal-manifest gate:**

  ```bash
  npm --prefix crates/trusted-server-js/lib test
  npm --prefix crates/trusted-server-js/lib run build
  npm --prefix crates/trusted-server-js/lib run lint
  npm --prefix crates/trusted-server-js/lib run typecheck
  cargo test-fastly publisher
  ```

- [ ] **Step C6: Commit the remaining integrations only after C1-C5 are green.** Stage the
      remaining integration directories, their shared helpers/tests, composition,
      build manifest, and exact Rust config emitters; do not fold creative or
      diagnostics changes from Tasks 18A/18B into this commit.

  ```bash
  git add crates/trusted-server-js/lib/src/integrations/datadome/index.ts crates/trusted-server-js/lib/src/integrations/datadome/script_guard.ts crates/trusted-server-js/lib/src/integrations/didomi/index.ts crates/trusted-server-js/lib/src/integrations/google_tag_manager/index.ts crates/trusted-server-js/lib/src/integrations/google_tag_manager/script_guard.ts
  git add crates/trusted-server-js/lib/src/integrations/lockr/index.ts crates/trusted-server-js/lib/src/integrations/lockr/script_guard.ts crates/trusted-server-js/lib/src/integrations/osano/index.ts crates/trusted-server-js/lib/src/integrations/osano/module.ts crates/trusted-server-js/lib/src/integrations/permutive/index.ts crates/trusted-server-js/lib/src/integrations/permutive/module.ts crates/trusted-server-js/lib/src/integrations/permutive/script_guard.ts crates/trusted-server-js/lib/src/integrations/permutive/segments.ts
  git add crates/trusted-server-js/lib/src/integrations/sourcepoint/index.ts crates/trusted-server-js/lib/src/integrations/sourcepoint/module.ts crates/trusted-server-js/lib/src/integrations/sourcepoint/script_guard.ts crates/trusted-server-js/lib/src/integrations/testlight/index.ts
  git add crates/trusted-server-js/lib/test/integrations/datadome/script_guard.test.ts crates/trusted-server-js/lib/test/integrations/didomi/index.test.ts crates/trusted-server-js/lib/test/integrations/google_tag_manager/script_guard.test.ts crates/trusted-server-js/lib/test/integrations/lockr/script_guard.test.ts crates/trusted-server-js/lib/test/integrations/osano/index.test.ts crates/trusted-server-js/lib/test/integrations/permutive/segments.test.ts
  git add crates/trusted-server-js/lib/test/integrations/sourcepoint/index.test.ts crates/trusted-server-js/lib/test/integrations/sourcepoint/script_guard.test.ts crates/trusted-server-js/lib/test/integrations/testlight/index.test.ts
  git add crates/trusted-server-js/lib/src/services/context.ts crates/trusted-server-js/lib/test/services/context.test.ts crates/trusted-server-js/lib/src/shared/beacon_guard.ts crates/trusted-server-js/lib/src/shared/globals.ts crates/trusted-server-js/lib/src/shared/script_guard.ts crates/trusted-server-js/lib/test/shared/beacon_guard.test.ts crates/trusted-server-js/lib/src/composition/browser_test.ts crates/trusted-server-js/lib/test/composition/browser.test.ts crates/trusted-server-js/lib/test/build/release-v1.test.mjs crates/trusted-server-js/lib/build-all.mjs
  git add crates/trusted-server-core/src/integrations/datadome.rs crates/trusted-server-core/src/integrations/datadome/protection.rs crates/trusted-server-core/src/integrations/datadome/protection_scope.rs crates/trusted-server-core/src/integrations/didomi.rs crates/trusted-server-core/src/integrations/google_tag_manager.rs crates/trusted-server-core/src/integrations/lockr.rs crates/trusted-server-core/src/integrations/osano.rs crates/trusted-server-core/src/integrations/permutive.rs crates/trusted-server-core/src/integrations/sourcepoint.rs crates/trusted-server-core/src/integrations/testlight.rs crates/trusted-server-core/src/integrations/mod.rs
  git commit -m "Prepare the remaining integration modules"
  ```

#### Task 18D: Extract the canonical critical and later-only product slices

**Task 18D files:**

- Create: `crates/trusted-server-js/lib/src/integrations/render_runtime/index.ts`
- Create: `crates/trusted-server-js/lib/src/integrations/render_runtime/module.ts`
- Create: `crates/trusted-server-js/lib/test/integrations/render_runtime/module.test.ts`
- Create: `crates/trusted-server-js/lib/src/integrations/aps/index.ts`
- Create: `crates/trusted-server-js/lib/src/integrations/aps/module.ts`
- Create: `crates/trusted-server-js/lib/src/integrations/gpt/later.ts`
- Create: `crates/trusted-server-js/lib/src/integrations/prebid/later.ts`
- Modify: `crates/trusted-server-js/lib/src/integrations/gpt_diagnostics/presentation.ts`
- Create: `crates/trusted-server-js/lib/src/integrations/osano/lifecycle.ts`
- Create: `crates/trusted-server-js/lib/src/integrations/permutive/lifecycle.ts`
- Create: `crates/trusted-server-js/lib/src/integrations/sourcepoint/lifecycle.ts`
- Create: `crates/trusted-server-js/lib/test/integrations/phase-slices.test.ts`
- Modify: `crates/trusted-server-js/lib/src/integrations/{gpt,prebid,gpt_diagnostics,osano,permutive,sourcepoint}/module.ts`
- Modify: `crates/trusted-server-js/lib/src/composition/browser.ts`
- Modify: `crates/trusted-server-js/lib/src/composition/browser_test.ts`
- Modify: `crates/trusted-server-js/lib/test/composition/browser.test.ts`
- Modify: `crates/trusted-server-js/lib/test/composition/maximal-runtime.test.ts`
- Modify: `crates/trusted-server-js/lib/build-all.mjs`
- Create: `crates/trusted-server-js/lib/scripts/check-architecture.mjs`
- Modify: `crates/trusted-server-js/lib/scripts/check-bundle-budgets.mjs`

- [ ] **Step D1: Write failing phase-membership and behavior tests.** Map all spec
      §5.2 catalog rows to these exact artifacts: critical `render_runtime`, `aps`,
      `creative`, `datadome`, `didomi`, `google_tag_manager`, `gpt`,
      `gpt_diagnostics`, `lockr`, `osano_consent`, `permutive_context`,
      `sourcepoint_consent`, `prebid`, `testlight`; deferred
      `diagnostics_presentation`, `gpt_later`, `osano_lifecycle`,
      `permutive_lifecycle`, `prebid_later`, `sourcepoint_lifecycle`. Assert each
      inclusion predicate, capability edge, named critical/later obligation, build
      role, and exactly-once maximal-inventory membership.

  Assert `render_runtime` provides distinct `trace.v1` and
  `trace.presentation.v1`; only `diagnostics_presentation` consumes the latter;
  `gpt_diagnostics` consumes only `runtime.v1` and `gpt.events.v1`; and presentation
  inclusion is exactly `renderTraceOverlay || diagnostics.gpt.active`. The build and
  broker must deny `attachPresentation` to APS, GPT, `gpt_later`, and every public
  facade rather than relying on naming or consumer convention.

- [ ] **Step D2: Add failing production-metafile tests.** The core entry may import
      only kernel, boot/projection contracts, queue/logger, and minimum direct public-
      API setup. Reject every deferred integration/service/UI path, test/no-op/fake
      seam, and `*ForTest` export. Reject a consumer artifact that inlines its
      provider, a second adapter/runtime/listener owner, or any GPT/Prebid global
      access outside the adapters.

- [ ] **Step D3: Run the new tests and record the expected failures and current
      critical/reference/maximal size deltas.** Do not regenerate the immutable
      baseline.

  ```bash
  npm --prefix crates/trusted-server-js/lib test -- --run test/integrations/phase-slices.test.ts test/composition/browser.test.ts test/composition/maximal-runtime.test.ts
  npm --prefix crates/trusted-server-js/lib run test:architecture
  npm --prefix crates/trusted-server-js/lib run check:bundle
  ```

- [ ] **Step D4: Extract the mandatory critical providers and product obligations.**
      `render_runtime` alone provides slots/auction/render/messages/trace/direct over
      kernel `runtime.v1`, with presentation authority attenuated into the separate
      `trace.presentation.v1` broker value. `aps`, GPT, diagnostics, consent/context,
      and all guard modules consume only catalogued frozen interfaces. Keep creative critical only
      for `enabled && (clickGuard || renderGuard)`. Prebid is always critical when
      enabled and retains external-artifact readiness, bidder aliases, user-ID/EIDs,
      publisher queue behavior, initial auction, and TS bid/PUC admission. It remains
      a separately generated pure external Prebid artifact plus TS-owned adapter—not
      vendored bytes.

- [ ] **Step D5: Move only proven later behavior to the six deferred slices.** Move
      diagnostics DOM/UI/export behind the already-tested private trace/GPT
      presentation attachments; GPT post-first-display refresh/navigation/later
      reconciliation; Osano retry/event/focus/visibility/clear; Permutive later SDK/
      segment refresh; Prebid synthetic refresh plus RCJ-PREBID-04 GAM-path exclusion;
      and Sourcepoint later retry/visibility/focus/update/safe-clear. Keep initial GPT
      handoff/hydration/reconciliation critical. A deferred slice consumes only
      critical providers, creates no adapter/runtime, and preserves exact disposer
      ownership. The baseline PBS Cache implementation stays inside the GPT product
      behind generation/disposal only and receives no new protocol.

- [ ] **Step D6: Prove parity and independent deferred loading.** Run every `RCJ-*`
      fixture against the final owner, including EID/user-ID behavior before first
      display and RCJ-PREBID-04 after the gate. In the maximal runtime, start all six
      deferred transactions after the shared paint/idle gate; block/fail each one in
      turn and prove every sibling starts immediately with its own deadline and the
      one runtime/adapter/listener identities remain unchanged.

- [ ] **Step D7: Make the production import and inventory gates green, report the
      historical size deltas, then commit and push.** Minimal critical means `[core,render_runtime]`; reference
      critical means `[core,render_runtime,creative,gpt,prebid,datadome]`; maximal
      TSJS total counts core and every production integration module exactly once.
      The release inventory also counts bootstrap exactly once under its separate
      bootstrap role/budget; it is not added to maximal TSJS total. Splitting never
      excuses later maximal-total growth. At this pre-capture checkpoint,
      `check:bundle` must validate the immutable historical subtree, semantic
      membership, graph integrity, and artifact accounting, print the old-membership
      values as deltas, and report `roleCorrectStatus: "pending-capture"`; it must not
      compare the new semantic sets to the old membership's byte ceilings. Task 18E
      closes that temporary pre-production state before Task 19.

  ```bash
  npm --prefix crates/trusted-server-js/lib test
  npm --prefix crates/trusted-server-js/lib run build
  npm --prefix crates/trusted-server-js/lib run test:release
  npm --prefix crates/trusted-server-js/lib run test:architecture
  npm --prefix crates/trusted-server-js/lib run check:bundle
  npm --prefix crates/trusted-server-js/lib run lint
  npm --prefix crates/trusted-server-js/lib run typecheck
  git status --short
  git add crates/trusted-server-js/lib/src/integrations/render_runtime/index.ts crates/trusted-server-js/lib/src/integrations/render_runtime/module.ts crates/trusted-server-js/lib/test/integrations/render_runtime/module.test.ts crates/trusted-server-js/lib/src/integrations/aps/index.ts crates/trusted-server-js/lib/src/integrations/aps/module.ts
  git add crates/trusted-server-js/lib/src/integrations/gpt/later.ts crates/trusted-server-js/lib/src/integrations/prebid/later.ts crates/trusted-server-js/lib/src/integrations/gpt_diagnostics/presentation.ts crates/trusted-server-js/lib/src/integrations/osano/lifecycle.ts crates/trusted-server-js/lib/src/integrations/permutive/lifecycle.ts crates/trusted-server-js/lib/src/integrations/sourcepoint/lifecycle.ts
  git add crates/trusted-server-js/lib/src/integrations/gpt/module.ts crates/trusted-server-js/lib/src/integrations/prebid/module.ts crates/trusted-server-js/lib/src/integrations/gpt_diagnostics/module.ts crates/trusted-server-js/lib/src/integrations/osano/module.ts crates/trusted-server-js/lib/src/integrations/permutive/module.ts crates/trusted-server-js/lib/src/integrations/sourcepoint/module.ts
  git add crates/trusted-server-js/lib/src/composition/browser.ts crates/trusted-server-js/lib/src/composition/browser_test.ts crates/trusted-server-js/lib/test/integrations/phase-slices.test.ts crates/trusted-server-js/lib/test/composition/browser.test.ts crates/trusted-server-js/lib/test/composition/maximal-runtime.test.ts
  git add crates/trusted-server-js/lib/build-all.mjs crates/trusted-server-js/lib/scripts/check-architecture.mjs crates/trusted-server-js/lib/scripts/check-bundle-budgets.mjs
  git commit -m "Split TSJS around the protected first display"
  git push origin "$(git branch --show-current)"
  ```

#### Task 18E: Resolve current-base conflicts and freeze role-correct transfer budgets

This is an integration and measurement checkpoint, not production wiring. The PR
continues to target `rc/july`. Resolve `main` first because the user explicitly
requested a clean merge against it, then resolve the actual PR base. Do not use a
blanket `ours` or `theirs` strategy: preserve upstream fixes and the revision-33
runtime/APS contracts file-by-file.

**Task 18E files:**

- Modify as required: files conflicted by merging `origin/main`
- Modify as required: files conflicted by merging `origin/rc/july`
- Modify: `crates/trusted-server-js/lib/test/fixtures/performance/aps-tsjs-prechange.json`
- Modify: `crates/trusted-server-js/lib/scripts/check-bundle-budgets.mjs`
- Create: `crates/trusted-server-js/lib/scripts/bundle-metrics.mjs`
- Modify: `crates/trusted-server-js/lib/test/build/release-v1.test.mjs`
- Modify: `crates/trusted-server-integration-tests/browser/tests/shared/tsjs-performance.spec.ts`
- Create: `scripts/validate-tsjs-performance-evidence.mjs`
- Create: `.github/workflows/tsjs-performance-gate.yml`
- Modify: `.github/workflows/test.yml`
- Modify: `.github/workflows/integration-tests.yml`

- [ ] **Step E1: Prove Task 18D is committed, pushed, and clean before integrating.**

  ```bash
  test -z "$(git status --porcelain)"
  TASK18D_SHA="$(git rev-parse HEAD)"
  git fetch origin "$(git branch --show-current)" main rc/july
  test "$TASK18D_SHA" = "$(git rev-parse "origin/$(git branch --show-current)")"
  ```

- [ ] **Step E2: Merge current `origin/main` and resolve every conflict
      deliberately.** For each conflict, compare the merge base, upstream side, and
      Task 18D side. Retain upstream security/build/test fixes while preserving one
      runtime, the 14-critical/6-deferred catalog, live APS proxying, external GPT/
      Prebid bytes, and the hard-cutover contract. `git merge` may report conflicts;
      resolve them before running any test, stage each resolved path explicitly, and
      assert there is no unresolved, unstaged, or untracked residue. If `main` is
      already contained, verify the no-op instead of manufacturing an empty commit.
      Then run the focused suites for every conflicted subsystem plus the full JS
      gate before completing the merge:

  ```bash
  MAIN_SHA="$(git rev-parse origin/main)"
  git merge --no-ff --no-commit origin/main
  # If Git reports conflicts: inspect all three stages, edit deliberately, and
  # git add each exact resolved path before continuing.
  test -z "$(git ls-files -u)"
  git diff --quiet
  test -z "$(git ls-files --others --exclude-standard)"
  git diff --check
  git diff --cached --check
  npm --prefix crates/trusted-server-js/lib test
  npm --prefix crates/trusted-server-js/lib run build
  npm --prefix crates/trusted-server-js/lib run test:release
  npm --prefix crates/trusted-server-js/lib run test:architecture
  npm --prefix crates/trusted-server-js/lib run lint
  npm --prefix crates/trusted-server-js/lib run typecheck
  cargo test-fastly
  cargo test-axum
  cargo test-cloudflare
  cargo test-spin
  test -z "$(git ls-files -u)"
  git diff --check
  git diff --cached --check
  git status --short
  if git rev-parse -q --verify MERGE_HEAD >/dev/null; then
    git commit -m "Merge current main into the TSJS cutover"
  fi
  git merge-base --is-ancestor "$MAIN_SHA" HEAD
  ```

- [ ] **Step E3: Merge current `origin/rc/july`, resolve the PR-base conflicts with
      the same three-way discipline, and rerun the complete E2 verification.** Also
      run the adoption ledger because `rc/july` is its source branch. As in E2,
      resolve and stage before testing and commit only when `MERGE_HEAD` exists. List
      and run the complete verification explicitly; do not treat “same as E2” as an
      executable gate. Push only when the worktree is clean:

  ```bash
  RC_JULY_SHA="$(git rev-parse origin/rc/july)"
  git merge --no-ff --no-commit origin/rc/july
  # If Git reports conflicts: inspect all three stages, edit deliberately, and
  # git add each exact resolved path before continuing.
  test -z "$(git ls-files -u)"
  git diff --quiet
  test -z "$(git ls-files --others --exclude-standard)"
  git diff --check
  git diff --cached --check
  node --test crates/trusted-server-js/lib/test/contract/rc-july-adoption.test.mjs
  npm --prefix crates/trusted-server-js/lib test
  npm --prefix crates/trusted-server-js/lib run build
  npm --prefix crates/trusted-server-js/lib run test:release
  npm --prefix crates/trusted-server-js/lib run test:architecture
  npm --prefix crates/trusted-server-js/lib run lint
  npm --prefix crates/trusted-server-js/lib run typecheck
  cargo test-fastly
  cargo test-axum
  cargo test-cloudflare
  cargo test-spin
  test -z "$(git ls-files -u)"
  git diff --check
  git diff --cached --check
  if git rev-parse -q --verify MERGE_HEAD >/dev/null; then
    git commit -m "Merge current rc/july into the TSJS cutover"
  fi
  git merge-base --is-ancestor "$RC_JULY_SHA" HEAD
  test -z "$(git status --porcelain)"
  git push origin "$(git branch --show-current)"
  ```

- [ ] **Step E4: From that exact clean, pushed, post-conflict parent commit, build
      once and append the role-correct capture to the existing JSON.** Never rewrite
      any original top-level field. Add `roleCorrectTransfer` containing the capture
      ref/SHA; exact Node/npm/TypeScript/Vite/esbuild identities (or the immutable
      package-lock digest that covers Vite/esbuild); gzip/Brotli implementation,
      version, and parameters; release id; the canonical release inventory with every
      artifact's exact `id`, `role`, `phase`, `trigger`, `inputs`, `outputs`, `file`,
      `bytes`, and `hash`; and exact raw/gzip/Brotli values for:
  - bootstrap: the generated controller/fallback exactly once;
  - minimal: `[core,render_runtime]`;
  - reference: `[core,render_runtime,creative,gpt,prebid,datadome]`;
  - maximal: core plus all twenty catalogued modules, excluding bootstrap.

  Run exactly:

  ```bash
  CAPTURE_REF="$(git branch --show-current)"
  CAPTURE_SHA="$(git rev-parse HEAD)"
  git fetch origin "$CAPTURE_REF"
  test "$CAPTURE_SHA" = "$(git rev-parse "origin/$CAPTURE_REF")"
  test -z "$(git status --porcelain)"
  npm --prefix crates/trusted-server-js/lib run build
  npm --prefix crates/trusted-server-js/lib run test:release
  ```

  Recompute every generated artifact hash from its current bytes and assert the
  release id, inventory, and metrics agree before copying only those generated values
  into the new object with `apply_patch`. Do not add a recapture command or mutable
  update mode to the comparator.

- [ ] **Step E5: Test-drive the permanent comparator.** Pin one canonical digest over
      every original top-level field except the newly appended
      `roleCorrectTransfer`; this protects source, environment, sampling, bundles,
      performance samples/ceilings, and evidence linkage together. Validate every
      role-correct evidence field and exact semantic membership. At the freeze point,
      prove the capture's release id, inventory, artifact hashes, and bytes equal the
      exact clean capture parent. For bootstrap/minimal/reference/maximal and each
      size kind, enforce `current <= ceil(captured * 1.05)`. Preserve all graph-
      integrity blockers: no missing/unclassified/multiply counted artifact, omitted
      maximal module, critical-to-deferred reachability, provider inlining, duplicate
      adapter/runtime/listener owner, or production test/fake/no-op seam. Add negative
      mutations for every original subtree and each forbidden edge. Add a fractional
      boundary fixture proving exactly `ceil(captured * 1.05)` passes and
      `ceil(captured * 1.05) + 1` fails; this must reject a `floor` implementation.
      Extract deterministic inventory-set aggregation and raw/gzip/Brotli measurement
      into pure `scripts/bundle-metrics.mjs`. The build calls that helper; Task 19 may
      change production entry wiring in `build-all.mjs`, but cannot alter the frozen
      measurement helper or comparator.

  ```bash
  npm --prefix crates/trusted-server-js/lib test -- --run test/build/release-v1.test.mjs
  npm --prefix crates/trusted-server-js/lib run build
  npm --prefix crates/trusted-server-js/lib run check:bundle
  npm --prefix crates/trusted-server-js/lib run test:release
  npm --prefix crates/trusted-server-js/lib run test:architecture
  npm --prefix crates/trusted-server-js/lib run lint
  npm --prefix crates/trusted-server-js/lib run typecheck
  git diff --check
  ```

- [ ] **Step E6: Commit only the frozen capture/comparator checkpoint, push it, and
      re-run `check:bundle` from a clean tree.** The capture's source SHA intentionally
      names this commit's clean parent; its release id and artifact hashes must match
      the files measured at that parent. Any future recapture or membership/formula
      change requires a separate reviewed design.

  ```bash
  git add \
    crates/trusted-server-js/lib/test/fixtures/performance/aps-tsjs-prechange.json \
    crates/trusted-server-js/lib/scripts/check-bundle-budgets.mjs \
    crates/trusted-server-js/lib/scripts/bundle-metrics.mjs \
    crates/trusted-server-js/lib/test/build/release-v1.test.mjs \
    .github/workflows/test.yml
  git diff --cached --check
  git commit -m "Freeze role-correct TSJS transfer budgets"
  git push origin "$(git branch --show-current)"
  test -z "$(git status --porcelain)"
  npm --prefix crates/trusted-server-js/lib run build
  npm --prefix crates/trusted-server-js/lib run check:bundle
  ```

- [ ] **Step E7: Implement and run the real browser-time, retained-heap, and request-
      ordering gate before production wiring.** The browser fixture must exercise the
      generated server controller, real critical artifact, one runtime, and real
      first-display adapter through test-only prospective routes/composition; it may
      not switch any production emitter or use `window.__tsjsPerf`. On Chromium
      145.0.7632.6, `github-hosted:ubuntu-24.04`, and fixture
      `tsjs-generated-loopback-paired-v2`, use exactly five warmups and 50 samples per
      variant, require the real `tsjs:bids-script` and `tsjs:first-display` marks,
      alternate a frozen reference and current variant in one runner process, and
      enforce current p90 ≤ reference p90 × 1.10 plus p90 ≤100 ms on both variants
      without selective reruns.

  **Corrected-instrument amendment (2026-08-12).** The earlier E7 run used
  Playwright request interception and is invalid evidence: `page.route()` /
  `route.fulfill()` did not measure a real browser-to-server transport. The corrected
  instrument serves the exact generated controller document and exact built critical
  and deferred bytes from one in-process `node:http` server bound to an ephemeral
  `127.0.0.1` port for the Playwright test. Auction POST and page-bids use that same
  loopback origin; delayed `gpt_later`, request counts, resource timing, mark timing,
  manual heap lifecycle, and cleanup remain part of the instrument. Its fixture
  provenance is `tsjs-generated-loopback-paired-v2`; the unchanged generator remains
  `generated-server-v1`.

  This transport and provenance correction invalidates commit
  `a7a9bab36c4eee6bc180e9206ca6c6879303d37a` as the frozen E7 instrument, along
  with its run, evidence tuple, and any instrument tag. The workflow must extract
  non-empty repository pins with valid quoting and verify Node.js `v24.12.0`, npm
  `11.6.2`, and Rust `1.95.0` before measurement. The first corrected pinned capture
  at `62421ee44c62f24534ea8782a46dfa5bfbcea950` (run `31598415675`) measured p90
  30.5 ms, but the unchanged artifact/instrument measured 40.8 ms in run
  `31599101515`. That host-load variance invalidates the attempted 33.6 ms absolute
  ceiling. Use commit `62421ee44c62f24534ea8782a46dfa5bfbcea950` as the immutable
  detached reference and pair it against current in the same job, alternating order
  for each warmup/sample. Freeze the 1.10 ratio and 100 ms per-side hard ceiling; do
  not infer or raise either from a later run. Record and review any capture-derived
  heap constant change before tagging; after that checkpoint, the corrected
  instrument bytes, reference SHA, ratio, hard ceiling, and heap thresholds are
  immutable and the normal pre-switch validation/tag sequence below applies.

  In one separate fresh Chromium context per variant, run the real fixture once. At
  each heap checkpoint, send `HeapProfiler.collectGarbage` exactly once and
  immediately read the single `Runtime.getHeapUsage.usedSize`. The obsolete
  synthetic-fixture absolute ceilings are retired: the corrected fixture first
  reached this assertion in run `31600763735` and measured 1,620,848 bytes after
  first render. Enforce current ≤ frozen reference × 1.10 and a 4 MiB hard ceiling
  for each side at boot, first render, refresh, and SPA navigation. Also assert
  exactly one critical TSJS request; no deferred request,
  preload, preparation, or execution before `tsjs:first-display-paint`; independent
  deferred starts after the gate; and no head-of-line blocking. Performance cannot
  waive a correctness failure.

  Put the entire measurement job—pinned runner/image, fixture preparation, commands,
  environment, and named artifact upload—in standalone
  `.github/workflows/tsjs-performance-gate.yml`, with `workflow_dispatch` and reusable
  `workflow_call` inputs for evidence id and `preswitch | postswitch` mode. Other
  workflows may call this file but cannot duplicate or redefine its measurement
  steps.

  ```bash
  TS_BROWSER_FRAMEWORKS=nextjs \
  TS_BROWSER_PROJECTS=chromium \
  TSJS_PERF_MODE=gate \
  TSJS_PERF_OUTPUT=crates/trusted-server-integration-tests/browser/test-results/tsjs-performance-preswitch.json \
    ./scripts/integration-tests-browser.sh \
      tests/shared/tsjs-performance.spec.ts --project=chromium
  npm --prefix crates/trusted-server-js/lib run build
  npm --prefix crates/trusted-server-js/lib run check:bundle
  ```

  Stage only the performance test and workflow wiring, commit and push, then dispatch
  the focused integration workflow for that exact pushed SHA with a unique evidence
  id and wait for its named pre-switch performance job to succeed. Upload the non-
  baseline result as immutable pre-switch evidence; never overwrite the Task 0
  fixture or Task 18E capture. Any pre-Task19 change to a production artifact or its
  build inputs invalidates the role-correct capture and must receive separate design
  review; E7 changes only the browser gate and workflow wiring.

  ```bash
  git add \
    crates/trusted-server-integration-tests/browser/tests/shared/tsjs-performance.spec.ts \
    scripts/validate-tsjs-performance-evidence.mjs \
    .github/workflows/tsjs-performance-gate.yml \
    .github/workflows/test.yml \
    .github/workflows/integration-tests.yml
  git diff --cached --check
  git commit -m "Gate TSJS first-display performance before cutover"
  git push origin "$(git branch --show-current)"
  test -z "$(git status --porcelain)"
  PRESWITCH_REF="$(git branch --show-current)"
  PRESWITCH_SHA="$(git rev-parse HEAD)"
  PRESWITCH_EVIDENCE_ID="aps-tsjs-preswitch-${PRESWITCH_SHA}"
  PRESWITCH_RUN_ID="$(node scripts/dispatch-workflow-run.mjs \
    tsjs-performance-gate.yml \
    "$PRESWITCH_REF" \
    evidence_id="$PRESWITCH_EVIDENCE_ID" \
    mode=preswitch)"
  gh run watch "$PRESWITCH_RUN_ID" --exit-status
  PRESWITCH_EVIDENCE_DIR="$(mktemp -d)"
  gh run download "$PRESWITCH_RUN_ID" \
    --name "tsjs-performance-$PRESWITCH_EVIDENCE_ID" \
    --dir "$PRESWITCH_EVIDENCE_DIR"
  node scripts/validate-tsjs-performance-evidence.mjs \
    --file "$PRESWITCH_EVIDENCE_DIR/tsjs-performance-preswitch.json" \
    --evidence-id "$PRESWITCH_EVIDENCE_ID" \
    --head-sha "$PRESWITCH_SHA" \
    --mode preswitch
  PRESWITCH_TAG="tsjs-performance-instrument-${PRESWITCH_SHA}"
  test -z "$(git ls-remote --tags origin "refs/tags/$PRESWITCH_TAG")"
  git tag -a "$PRESWITCH_TAG" "$PRESWITCH_SHA" \
    -m "Freeze validated TSJS performance instrument"
  git push origin "refs/tags/$PRESWITCH_TAG"
  test "$PRESWITCH_SHA" = "$(git rev-parse "$PRESWITCH_TAG^{commit}")"
  ```

  The validator rejects a wrong evidence id/head/reference SHA, environment or
  fixture drift, anything other than five warmups/50 samples per variant or
  alternating pair order, missing real marks, relative/hard p90 or heap overflow, a
  failed correctness/load-order assertion, or an incomplete result. Record the
  immutable `{sha,tag,evidenceId,runId,artifactName}` tuple in the PR execution
  evidence, not in either baseline object. The annotated tag is created only after
  the evidence validates and is never moved, deleted, or force-pushed; its SHA is the
  sole later byte-identity baseline.

### Task 19: Perform the coordinated production wiring switch

**Files:**

- Modify: `crates/trusted-server-js/lib/src/core/index.ts`
- Modify: `crates/trusted-server-js/lib/src/composition/browser.ts`
- Modify: `crates/trusted-server-js/lib/src/integrations/gpt/index.ts`
- Modify: `crates/trusted-server-js/lib/src/integrations/prebid/index.ts`
- Modify: `crates/trusted-server-js/lib/src/integrations/creative/index.ts`
- Modify: `crates/trusted-server-js/lib/src/integrations/datadome/index.ts`
- Modify: `crates/trusted-server-js/lib/src/integrations/didomi/index.ts`
- Modify: `crates/trusted-server-js/lib/src/integrations/google_tag_manager/index.ts`
- Modify: `crates/trusted-server-js/lib/src/integrations/gpt_diagnostics/index.ts`
- Modify: `crates/trusted-server-js/lib/src/integrations/lockr/index.ts`
- Modify: `crates/trusted-server-js/lib/src/integrations/osano/index.ts`
- Modify: `crates/trusted-server-js/lib/src/integrations/permutive/index.ts`
- Modify: `crates/trusted-server-js/lib/src/integrations/sourcepoint/index.ts`
- Modify: `crates/trusted-server-js/lib/src/integrations/testlight/index.ts`
- Delete: `crates/trusted-server-core/src/integrations/gpt_bootstrap.js`
- Modify: `crates/trusted-server-core/src/publisher.rs`
- Modify: `crates/trusted-server-core/src/tsjs.rs`
- Modify: `crates/trusted-server-core/src/auction/endpoints.rs`
- Modify: `crates/trusted-server-core/src/auction/formats.rs`
- Modify: `crates/trusted-server-core/src/integrations/registry.rs`
- Modify: `crates/trusted-server-core/src/html_processor.rs`
- Modify: `crates/trusted-server-core/src/integrations/prebid.rs`
- Modify: `crates/trusted-server-core/src/integrations/didomi.rs`
- Modify: `crates/trusted-server-core/src/integrations/sourcepoint.rs`
- Modify: `crates/trusted-server-core/src/integrations/gpt.rs`
- Modify: `crates/trusted-server-core/src/integrations/gpt_diagnostics.rs`
- Modify: `crates/trusted-server-core/src/integrations/gpt_diagnostics_bootstrap.js`
- Modify: `crates/trusted-server-js/lib/build-all.mjs`
- Modify: `crates/trusted-server-adapter-fastly/src/app.rs`
- Modify: `crates/trusted-server-adapter-axum/src/app.rs`
- Modify: `crates/trusted-server-adapter-cloudflare/src/app.rs`
- Modify: `crates/trusted-server-adapter-spin/src/app.rs`

- [ ] **Step 1: Complete the pre-switch checklist with no production-wiring changes staged.**
      The atomic switch is allowed to flip wiring only after every behavior suite
      below is already green against the test-only composition and prospective
      routes/artifacts:
  - render attempt, direct APS, direct ADM, baseline PBS Cache black-box parity, PUC claim/channel, artifact
    store, reservations, slots, targeting, projections, auction batching, context,
    navigation sessions, runtime transaction, queue handoff, and integration registry;
  - GPT including RCJ-GPT-04, Prebid including RCJ-PREBID-04 and the rebuilt pure
    10.26.0 artifact, APS, creative, render trace, GPT diagnostics/`ts_console`, and
    every remaining integration alone and in the maximal manifest;
  - core diagnostics ingress shape/copy/bounds/disposal with no module subscription
    machinery; consumer-specific `trace.presentation.v1` denial and private attach
    lifecycle; GPT physical-slot plus request-cycle projection, repeated same-object
    refresh, ambiguous late-callback omission, and independent object-identity
    `gpt.events.v1` delivery;
  - generated release/fallback/absence contracts, architecture/lint/typecheck, all
    Rust projection/config/route tests, adapter parity, and the four actual-adapter
    runner-proxy corpora; exact critical/deferred hash routes; canonical 20-module
    catalog; protected paint gate; independent deferred deadlines; canonical absolute
    URL authentication; and CSP/Trusted Types cases; and
  - old-surface rejection plus new-surface fixture tests, proving the cutover commit
    contains no new behavior implementation or test repair.

  Keep the immutable historical-reporting and role-correct bundle gates wired and
  green. Task 18D must already have
  removed old-plus-new production membership; do not rebase the pre-change artifact
  or defer a failing critical/maximal budget until after the atomic switch.

  Verify the prospective performance-mark tests from Task 16/18D are already green: the
  unit-tested server fragment names `tsjs:bids-script`, the adapter names exactly one
  authoritative `tsjs:first-display`, the protected attempt records
  `tsjs:first-display-terminal`, the shared paint gate records
  `tsjs:first-display-paint`, and the historical measure uses the first two exact
  start/action marks. Task 19
  may connect only their already-tested production call sites; it may not add or
  repair mark behavior. `window.__tsjsPerf` remains baseline-capture scaffolding and
  cannot satisfy the post-switch gate.

  ```bash
  test -z "$(git status --porcelain)"
  test "$(gh pr view --json baseRefName --jq .baseRefName)" = "rc/july"
  PRESWITCH_REF="$(git branch --show-current)"
  git fetch origin \
    'refs/tags/tsjs-performance-instrument-*:refs/tags/tsjs-performance-instrument-*'
  PRESWITCH_TAGS="$(git tag --list 'tsjs-performance-instrument-*' \
    --merged HEAD)"
  test "$(printf '%s\n' "$PRESWITCH_TAGS" | sed '/^$/d' | wc -l | tr -d ' ')" -eq 1
  PRESWITCH_TAG="$(printf '%s\n' "$PRESWITCH_TAGS" | sed '/^$/d')"
  PRESWITCH_SHA="$(git rev-parse "$PRESWITCH_TAG^{commit}")"
  test "$PRESWITCH_TAG" = "tsjs-performance-instrument-$PRESWITCH_SHA"
  git merge-base --is-ancestor "$PRESWITCH_SHA" HEAD
  PRESWITCH_EVIDENCE_ID="aps-tsjs-preswitch-${PRESWITCH_SHA}"
  PRESWITCH_RUNS="$(gh run list \
    --workflow tsjs-performance-gate.yml \
    --event workflow_dispatch \
    --limit 100 \
    --json databaseId,displayTitle,headSha,conclusion)"
  PRESWITCH_MATCHES="$(jq \
    --arg sha "$PRESWITCH_SHA" \
    --arg evidence "$PRESWITCH_EVIDENCE_ID" \
    '[.[] | select(.headSha == $sha and (.displayTitle | contains($evidence)))]' \
    <<<"$PRESWITCH_RUNS")"
  test "$(jq 'length' <<<"$PRESWITCH_MATCHES")" -eq 1
  PRESWITCH_RUN_ID="$(jq -er '.[0] | select(.conclusion == "success") | .databaseId' \
    <<<"$PRESWITCH_MATCHES")"
  PRESWITCH_EVIDENCE_DIR="$(mktemp -d)"
  gh run download "$PRESWITCH_RUN_ID" \
    --name "tsjs-performance-$PRESWITCH_EVIDENCE_ID" \
    --dir "$PRESWITCH_EVIDENCE_DIR"
  node scripts/validate-tsjs-performance-evidence.mjs \
    --file "$PRESWITCH_EVIDENCE_DIR/tsjs-performance-preswitch.json" \
    --evidence-id "$PRESWITCH_EVIDENCE_ID" \
    --head-sha "$PRESWITCH_SHA" \
    --mode preswitch
  npm --prefix crates/trusted-server-js/lib run build
  npm --prefix crates/trusted-server-js/lib run check:bundle
  npm --prefix crates/trusted-server-js/lib run test:release
  npm --prefix crates/trusted-server-js/lib run test:architecture
  npm --prefix crates/trusted-server-js/lib test -- --run test/core/index.test.ts test/kernel/runtime.test.ts test/composition/browser.test.ts
  npm --prefix crates/trusted-server-js/lib test -- --run \
    test/services test/kernel test/adapters test/core
  npm --prefix crates/trusted-server-js/lib test -- --run \
    test/integrations test/composition
  npm --prefix crates/trusted-server-js/lib run build
  npm --prefix crates/trusted-server-js/lib run lint
  npm --prefix crates/trusted-server-js/lib run typecheck
  cargo test-fastly
  cargo test-axum
  cargo test-cloudflare
  cargo test-spin
  cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test parity
  ./scripts/integration-tests-aps-runner-proxy.sh --runtime axum
  ./scripts/integration-tests-aps-runner-proxy.sh --runtime fastly
  ./scripts/integration-tests-aps-runner-proxy.sh --runtime cloudflare
  ./scripts/integration-tests-aps-runner-proxy.sh --runtime spin
  test "$(gh run view "$PRESWITCH_RUN_ID" --json headSha,conclusion \
    --jq '[.headSha,.conclusion] | @tsv')" = "$PRESWITCH_SHA	success"
  ```

- [ ] **Step 2: Atomically switch production wiring in one task and one commit.** Make no
      validator, state-machine, lifecycle, adapter, or test behavior changes here:
  - point `/auction`, initial HTML, and page-bids production emitters at the
    already-tested exact decision/projection serializers and boot-script fragments,
    including the preimplemented `tsjs:bids-script` mark;
  - carry one exact ordered placement record for every initial/page-bids decision,
    reject missing/extra/out-of-order placement coverage in the browser parser, and
    keep only the direct `/auction` serializer's internal `slots:[]` exception;
  - have the already-tested `render_runtime` and GPT provider modules resolve each
    placement, adopt exactly one existing publisher GPT slot or transactionally
    define/adopt one TS slot, merge static then bid targeting with runtime-owned
    `hb_adid`, and publish both initial and committed SPA winners through the same
    GPT/PUC lifecycle. Cover responsive-prefix ambiguity, stale candidate
    destruction, publisher refresh versus TS display, attributable empty-GAM direct
    fallback, and page-bids alias registration;
  - preserve rc/july SPA route semantics in that composition: pathname-plus-query
    identity across push/replace/pop, same-route suppression, stale-response
    inertness, and rollback to the last committed path after a current failure so an
    identical route can retry;
  - keep production `composition/browser.ts` as the already-tested minimal core graph:
    controller handoff, one kernel/runtime, immutable boot/projection parsing, queue,
    logger, catalog registrar, and minimum direct public API only. Each critical or
    deferred provider IIFE owns and registers its already-tested adapter/service
    implementation through that registrar; production core must not import or inline
    those implementations. Only test-only `composition/browser_test.ts` may import
    and construct all concrete layers. Each thin integration `index.ts` delegates to
    its owned module without retaining a second registry or behavior branch;
  - switch generated release/manifest/config/bootstrap emission and the independently
    built pure Prebid 10.26.0 artifact to those already-tested entry points; emit the
    inline minimal controller followed by the one parser-blocking critical artifact,
    no parser-time deferred tags, and the early live external Prebid tag whenever
    Prebid is enabled; let core independently start catalogued later modules only
    after the protected paint/idle gate; and
  - register the already-tested versioned APS renderer and live unversioned runner
    proxy in the production registry and pre-router entry points of all four adapters,
    preserving exact response headers and the negative routes before auth, generic
    finalization, EC, integration filters, or publisher fallback can run.

  The switch is a hard cutover: add no selector, dual manifest, compatibility alias,
  protocol autodetection, or fallback to old behavior. Delete behavior-bearing
  `gpt_bootstrap.js` in this switch commit; every other old implementation may remain
  physically present only while unreachable, and Task 22 deletes it before release.

  Before enabling the Fastly production route, run the unchanged stall/slow-drip
  deadline cases through a non-production Fastly Compute service and a controlled
  staging-only backend. Viceroy proves the feature artifact and static simulator
  seam, but it does not prove Fastly's production dynamic-backend body-timeout
  behavior. The staging smoke must retain the strict five-second downstream ceiling
  and block cutover on any timeout or late-continuation failure.

  Run old-surface and new-surface fixture tests immediately before the switch, then
  require the entire suite green after it. Do not add a production selector, dual
  manifest, or shape autodetection. The temporarily unused server routes and old
  declarations are deleted in Task 22 before release.

- [ ] **Step 3: Stage only the wiring allowlist, prove the diff contains no behavior/test files,**
      **run the full gate, and commit.** Any required behavior or test repair fails this
      checkpoint and returns to the owning earlier task; do not widen the allowlist.

  ```bash
  git add \
    crates/trusted-server-js/lib/src/core/index.ts \
    crates/trusted-server-js/lib/src/composition/browser.ts \
    crates/trusted-server-js/lib/src/integrations/{gpt,prebid,creative,datadome,didomi,google_tag_manager,gpt_diagnostics,lockr,osano,permutive,sourcepoint,testlight}/index.ts \
    crates/trusted-server-js/lib/build-all.mjs
  git add \
    crates/trusted-server-core/src/integrations/gpt_bootstrap.js \
    crates/trusted-server-core/src/publisher.rs \
    crates/trusted-server-core/src/tsjs.rs \
    crates/trusted-server-core/src/auction/{endpoints,formats}.rs \
    crates/trusted-server-core/src/integrations/{registry,prebid,didomi,sourcepoint,gpt,gpt_diagnostics}.rs \
    crates/trusted-server-core/src/integrations/gpt_diagnostics_bootstrap.js \
    crates/trusted-server-core/src/html_processor.rs
  git add \
    crates/trusted-server-adapter-fastly/src/app.rs \
    crates/trusted-server-adapter-axum/src/app.rs \
    crates/trusted-server-adapter-cloudflare/src/app.rs \
    crates/trusted-server-adapter-spin/src/app.rs
  git diff --name-only --cached | awk '
    /^(crates\/trusted-server-js\/lib\/(src\/core\/index\.ts|src\/composition\/browser\.ts|src\/integrations\/(gpt|prebid|creative|datadome|didomi|google_tag_manager|gpt_diagnostics|lockr|osano|permutive|sourcepoint|testlight)\/index\.ts|build-all\.mjs)|crates\/trusted-server-core\/src\/(integrations\/gpt_bootstrap\.js|publisher\.rs|tsjs\.rs|auction\/(endpoints|formats)\.rs|integrations\/(registry|prebid|didomi|sourcepoint|gpt|gpt_diagnostics)\.rs|integrations\/gpt_diagnostics_bootstrap\.js|html_processor\.rs)|crates\/trusted-server-adapter-(fastly|axum|cloudflare|spin)\/src\/app\.rs)$/ { next }
    { print "unexpected non-wiring path: " $0; bad = 1 }
    END { exit bad ? 1 : 0 }
  '
  git diff --exit-code --cached -- \
    crates/trusted-server-js/lib/src/services \
    crates/trusted-server-js/lib/src/kernel \
    crates/trusted-server-js/lib/src/adapters \
    crates/trusted-server-js/lib/test
  npm --prefix crates/trusted-server-js/lib test -- --run test/services test/core test/integrations/gpt
  npm --prefix crates/trusted-server-js/lib test -- --run test/integrations/prebid test/integrations/aps test/kernel
  npm --prefix crates/trusted-server-js/lib run test:release
  npm --prefix crates/trusted-server-js/lib run test:architecture
  npm --prefix crates/trusted-server-js/lib run build
  npm --prefix crates/trusted-server-js/lib run lint
  npm --prefix crates/trusted-server-js/lib run typecheck
  cargo test-fastly
  cargo test-axum
  cargo test-cloudflare
  cargo test-spin
  cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test parity
  ./scripts/integration-tests-aps-runner-proxy.sh --runtime axum
  ./scripts/integration-tests-aps-runner-proxy.sh --runtime fastly
  ./scripts/integration-tests-aps-runner-proxy.sh --runtime cloudflare
  ./scripts/integration-tests-aps-runner-proxy.sh --runtime spin
  git commit -m "Switch production to the resilient TSJS runtime"
  ```

### Phase 4 exit

- GPT, Prebid, APS, and all integration entry points use one kernel/integration-module
  surface.
- The old registries, sentinels, expandos, refresh wrappers, and bridge branches are
  unreachable behind production wiring; after the switch deletes `gpt_bootstrap.js`,
  Task 22 physically deletes the remaining legacy surfaces.
- All Vitest and production-bundle tests pass.

## Phase 5 — browser conformance, deletion, and release readiness

### Task 20: Build the hermetic browser race matrix

Task 20 is an umbrella only. Execute 20A bootstrap/phase-loader/CSP, 20B render/PUC,
and 20C GPT/Prebid/diagnostics/integration behavior as separate
red-to-green-to-commit checkpoints. Execute 20D as a green re-attestation and CI-
gating checkpoint for Task 5B's already-complete four-adapter transport corpus.
Shared helpers may be extended by later checkpoints, but no checkpoint may stage a
later group's spec files or workflow changes.

**Files:**

- Modify: `crates/trusted-server-integration-tests/browser/playwright.config.ts`
- Modify: `crates/trusted-server-integration-tests/browser/tests/shared/aps-renderer.spec.ts`
- Create: `crates/trusted-server-integration-tests/browser/tests/shared/aps-puc-lifecycle.spec.ts`
- Create: `crates/trusted-server-integration-tests/browser/tests/shared/tsjs-runtime.spec.ts`
- Modify: `crates/trusted-server-integration-tests/browser/tests/shared/creative-sandbox.spec.ts`
- Modify: `crates/trusted-server-integration-tests/browser/tests/nextjs/gpt-diagnostics.spec.ts`
- Modify: `crates/trusted-server-integration-tests/browser/tests/nextjs/navigation.spec.ts`
- Create: `crates/trusted-server-integration-tests/browser/helpers/tsjs-fixture.ts`
- Modify: `crates/trusted-server-integration-tests/browser/helpers/gpt-stub.ts`
- Modify: `crates/trusted-server-integration-tests/browser/helpers/infra.ts`
- Modify: `crates/trusted-server-integration-tests/browser/helpers/state.ts`
- Modify: `crates/trusted-server-integration-tests/browser/fixtures/fictional-aps-runner.js`
- Modify: `scripts/integration-tests-browser.sh`
- Modify: `.github/workflows/integration-tests.yml`

- [ ] **Step 1: Within 20A, extend Playwright projects to Chromium, Firefox, and WebKit for the focused APS**
      conformance files. Keep the broader existing suite's browser matrix unchanged
      unless runtime permits expansion.

  Extend the clean-checkout browser script's `TS_BROWSER_PROJECTS` input so it
  installs the selected engines and forwards Playwright arguments with
  `npm --prefix ... exec -- playwright`; it must retain the release-WASM, Viceroy
  config, Docker image, npm install, and TSJS fixture preparation from Task 0.

- [ ] **Step 2: Within 20B and 20C, create only the deterministic local GPT, PUC-contract, and locally authored**
      **fictional APS-runner success/failure fixtures.** The PUC harness implements
      only the public `prebidMessenger`, `runDynamicRenderer`, and `h.sendMessage`
      behavior required to drive the protocol and contains no copied PUC bytes. The
      fictional runner must not copy, transform, derive from, or archive APS runner
      bytes and must never be packaged as a production fallback. Do not mock the
      kernel/services under test; exercise the actual externally hosted PUC release
      only in the real-GAM pre-production gate. Create and stage each fixture in the
      first checkpoint that consumes it; do not leave 20B/20C fixture changes dirty
      across the 20A commit.

  Build all synthetic critical-runtime pages from one shared helper that reads the
  generated release manifest, concatenates exact core plus selected critical
  artifact bytes with the production separator, hashes those response bytes, emits
  the exact `BootManifestV1`, serves only that canonical content-addressed URL, and
  always uses `script#trustedserver-js`. Do not execute anonymous inline/path
  bundles or hand-maintain abbreviated manifests in browser tests.

- [ ] **Step 3: Implement every spec §7.2 browser-observable race as grouped tables with exact**
      terminal, DOM, targeting, listener, port, timer, and network assertions:
  - PUC claim/join: simultaneous duplicate requests; live/tombstoned/native ids;
    wrong source/slot, altered id, SafeFrame-shaped nesting, claim before/after
    attributable nonempty or empty GAM, navigation/supersession on both sides, and
    replay at tombstone expiry;
  - capability/channel ownership: ticket/nonce capacity and eighth-draw collision,
    zero/one/two transferred ports, registration before/at/after deadline, caller
    abort before/after registration/insertion/document acceptance, channel loss,
    settlement-post throw, owner watchdog versus late response, and exactly one
    `OwnerSettlementV1` plus Promise settlement. The remote owner removes only an
    uncommitted iframe at 20 seconds; accepted DOM survives;
  - APS/ADM documents: renderer load/error/removal/replacement, runner
    acknowledgement/failure/timeout, exact 1/4096 dimensions in Rust/TS/embedded ES5/
    PUC DOM, ADM initial `about:blank` versus intended `srcdoc`, and proof that only
    the current intended navigation can accept. Run the pinned PBS Cache request/
    parse/macro/PUC/collapsed-resize/failure/navigation corpus only as pre/post black-
    box parity, with no new cache path or semantics;
  - runtime/bootstrap: prepare reject/abort, activation throw at every checkpoint,
    9,999/10,000/10,001 ms boundaries, duplicate `afterCommit`, catalog-derived
    13/14/15 critical and 19/20/21 manifest capacity,
    late continuation after fallback, publisher work during startup, exact same-task
    rollback, full/fallback `TsjsApi` own surfaces, malformed boot, actual-Array queue
    swap/retained references/native mutators/nested pushes/callback throws, and
    missing main bundle after server projection. For every fallback commit,
    instrument the real browser surfaces and assert that no second runtime,
    GPT/Prebid/message listener, `MessagePort`, interval/timeout, request, script,
    wrapper, observer, guard, or iframe survives; dispatch late messages, timer
    boundaries, and bundles afterward and prove none can revive rendering or allocate
    replacement state;
  - phase loading: protected programmatic/server first attempt on both sides of the
    10,000 ms startup boundary; complete immutable batch settlement; visible two-
    frame and hidden timeout paths; real `tsjs:first-display-paint`; no earlier
    deferred traffic; independent simultaneous deferred transactions; a hung or
    failed diagnostics module that cannot delay GPT/Prebid later work; caller versus
    module deadlines; SPA-waiter versus runtime-load disposal; exact absolute URL,
    script-node, currentScript, id/phase/release checks; same-origin, nonce-only,
    `strict-dynamic`, and Trusted Types allowed/default/rejected fixtures; synchronous
    `policy_blocked` versus post-insertion `load_error`; and stale static hashes that
    return local `404 no-store` without publisher fallthrough;
  - navigation/projection/API: immutable initial boot versus SPA-owned replacement,
    stale/duplicate/malformed page-bids, exact grammar/count/UTF-8 and 8 MiB all-winner
    reduction, 255/256/257 combined server/programmatic slots, transactional
    `addAdUnits`, explicit and omitted `requestAds` snapshots, unknown/colliding ids,
    concurrent partially overlapping batches, aborts/timeouts, logger/error behavior,
    and attempt-prefix/ordinal exhaustion without issued-id retention;
  - GPT/targeting: readiness, request-start, and completion on both deadline sides;
    SRA ordering, duplicate response ids, old navigation callbacks, handoff and
    disabled-initial-load suppression, DOM reconciliation at 249/250 and
    4,999/5,000 ms, two-success cap, throw/false destroy, no second physical slot,
    publisher ownership, and identical-string targeting generations plus same-value/
    different-value set, per-key clear, clear-all, non-top rebase, and artifact
    promotion/disposal races;
  - Prebid: missing/stub/late/duplicate/older/partial artifacts; exact 10.26.0 and all
    stamp/manifest caps; same-release reuse, different-release refusal, hostile own
    property shapes, watchdog versus late module, `pbjs`/stamp replacement, exact
    prepared-bid admission and non-publication, bidder aliases/user-ID/EID coverage,
    selection/loser/timeout/abort behavior, refresh exclusions, native bid/queue
    survival, and proof that the external artifact has no TS auction/render behavior;
  - creative/diagnostics/integrations: every creative policy/boot/automatic-guard/
    opaque-click case; render-trace ordering/enrichment/200-history/200-pending/32-
    subscriber/async-delivery cases with no event alias; GPT diagnostics 512-fact,
    64-slot, 10-cycle, 128-issue, 32-subscriber, latest-snapshot and inactive-zero-
    effect cases; and every remaining integration alone/maximal with startup failure,
    disposal, matcher, storage/consent, callback, and cross-isolation cases; and
  - transport/protocol: every message/body/count/string boundary at minus-one/exact/
    plus-one with multibyte and malformed encodings, plus the complete runner-proxy
    redirect, media type, encoding, content-length, streamed-size, slow-drip, header-
    stripping, byte-preserving, and empty non-leaking failure corpus through all
    actual adapters, including Cloudflare and Spin wasm evidence.

  Do not implement these groups as one batch. Allocate runtime/bootstrap and phase
  loading to 20A; PUC capability/channel ownership, APS/ADM documents, baseline
  cache parity, and creative sandbox behavior to 20B; navigation/projection/API,
  GPT/targeting, Prebid, diagnostics, and the remaining integrations to 20C; and the
  complete actual-adapter transport/protocol corpus to Task 5B, with only
  re-attestation and CI gating in 20D.

- [ ] **Step 20A: Add bootstrap/phase-loader/CSP cases, observe the focused browser failure, make them green, and commit.**

  ```bash
  # RED, before 20A implementation: expected FAIL in the new runtime/loader/CSP cases.
  TS_BROWSER_FRAMEWORKS=nextjs TS_BROWSER_PROJECTS=chromium,firefox,webkit \
    ./scripts/integration-tests-browser.sh \
      tests/shared/tsjs-runtime.spec.ts \
      --project=chromium --project=firefox --project=webkit
  # GREEN, after 20A implementation: expected PASS.
  TS_BROWSER_FRAMEWORKS=nextjs TS_BROWSER_PROJECTS=chromium,firefox,webkit \
    ./scripts/integration-tests-browser.sh \
      tests/shared/tsjs-runtime.spec.ts \
      --project=chromium --project=firefox --project=webkit
  git add crates/trusted-server-integration-tests/browser/playwright.config.ts crates/trusted-server-integration-tests/browser/tests/shared/tsjs-runtime.spec.ts crates/trusted-server-integration-tests/browser/helpers/infra.ts crates/trusted-server-integration-tests/browser/helpers/state.ts scripts/integration-tests-browser.sh
  git commit -m "Cover TSJS bootstrap and deferred loading in browsers"
  ```

- [ ] **Step 20B: Add render/PUC/creative/cache-parity cases, observe the focused browser failure, make them green, and commit.**

  ```bash
  # RED, before 20B implementation: expected FAIL in the new render/PUC cases.
  TS_BROWSER_FRAMEWORKS=nextjs TS_BROWSER_PROJECTS=chromium,firefox,webkit \
    ./scripts/integration-tests-browser.sh \
      tests/shared/aps-renderer.spec.ts \
      tests/shared/aps-puc-lifecycle.spec.ts \
      tests/shared/creative-sandbox.spec.ts \
      --project=chromium --project=firefox --project=webkit
  # GREEN, after 20B implementation: expected PASS.
  TS_BROWSER_FRAMEWORKS=nextjs TS_BROWSER_PROJECTS=chromium,firefox,webkit \
    ./scripts/integration-tests-browser.sh \
      tests/shared/aps-renderer.spec.ts \
      tests/shared/aps-puc-lifecycle.spec.ts \
      tests/shared/creative-sandbox.spec.ts \
      --project=chromium --project=firefox --project=webkit
  git add crates/trusted-server-integration-tests/browser/tests/shared/aps-renderer.spec.ts crates/trusted-server-integration-tests/browser/tests/shared/aps-puc-lifecycle.spec.ts crates/trusted-server-integration-tests/browser/tests/shared/creative-sandbox.spec.ts crates/trusted-server-integration-tests/browser/fixtures/fictional-aps-runner.js crates/trusted-server-integration-tests/browser/helpers/gpt-stub.ts crates/trusted-server-integration-tests/browser/helpers/infra.ts crates/trusted-server-integration-tests/browser/helpers/state.ts
  git commit -m "Cover APS and creative render races in browsers"
  ```

- [ ] **Step 20C: Add GPT/Prebid/navigation/diagnostics/integration cases, observe the focused browser failure, make them green, and commit.**

  ```bash
  # RED, before 20C implementation: expected FAIL in the new product/navigation cases.
  TS_BROWSER_FRAMEWORKS=nextjs TS_BROWSER_PROJECTS=chromium,firefox,webkit \
    ./scripts/integration-tests-browser.sh \
      tests/shared/tsjs-runtime.spec.ts \
      tests/nextjs/gpt-diagnostics.spec.ts \
      tests/nextjs/navigation.spec.ts \
      --project=chromium --project=firefox --project=webkit
  # GREEN, after 20C implementation: expected PASS.
  TS_BROWSER_FRAMEWORKS=nextjs TS_BROWSER_PROJECTS=chromium,firefox,webkit \
    ./scripts/integration-tests-browser.sh \
      tests/shared/tsjs-runtime.spec.ts \
      tests/nextjs/gpt-diagnostics.spec.ts \
      tests/nextjs/navigation.spec.ts \
      --project=chromium --project=firefox --project=webkit
  git add crates/trusted-server-integration-tests/browser/tests/shared/tsjs-runtime.spec.ts crates/trusted-server-integration-tests/browser/tests/nextjs/gpt-diagnostics.spec.ts crates/trusted-server-integration-tests/browser/tests/nextjs/navigation.spec.ts crates/trusted-server-integration-tests/browser/helpers/gpt-stub.ts crates/trusted-server-integration-tests/browser/helpers/infra.ts crates/trusted-server-integration-tests/browser/helpers/state.ts
  git commit -m "Cover TSJS product lifecycles in browsers"
  ```

- [ ] **Step 20D: Re-attest Task 5B's unchanged complete actual-adapter transport corpus, wire that green gate into CI, and commit.**

  Task 5B owns every transport test/corpus file and its RED/GREEN implementation
  checkpoint. No new transport case or implementation belongs here; if a missing
  case is discovered, return it to Task 5B rather than repairing it in Task 20.

  ```bash
  ./scripts/integration-tests-aps-runner-proxy.sh --runtime axum
  ./scripts/integration-tests-aps-runner-proxy.sh --runtime fastly
  ./scripts/integration-tests-aps-runner-proxy.sh --runtime cloudflare
  ./scripts/integration-tests-aps-runner-proxy.sh --runtime spin
  # Expected: all four unchanged Task 5B corpora PASS before and after CI wiring.
  git add .github/workflows/integration-tests.yml
  git commit -m "Gate APS transport and browser conformance"
  ```

- [ ] **Step 4: Assert DOM/network/lifecycle outcomes directly. The suite must run with no**
      external analytics or persistence service.

- [ ] **Step 5: Run:**

  ```bash
  ./scripts/integration-tests-aps-runner-proxy.sh --runtime axum
  ./scripts/integration-tests-aps-runner-proxy.sh --runtime fastly
  ./scripts/integration-tests-aps-runner-proxy.sh --runtime cloudflare
  ./scripts/integration-tests-aps-runner-proxy.sh --runtime spin
  TS_BROWSER_FRAMEWORKS=nextjs \
  TS_BROWSER_PROJECTS=chromium,firefox,webkit \
    ./scripts/integration-tests-browser.sh \
      tests/shared/aps-renderer.spec.ts \
      tests/shared/aps-puc-lifecycle.spec.ts \
      tests/shared/tsjs-runtime.spec.ts \
      tests/shared/creative-sandbox.spec.ts \
      tests/nextjs/gpt-diagnostics.spec.ts \
      tests/nextjs/navigation.spec.ts \
      --project=chromium --project=firefox --project=webkit
  ```

### Task 21: Add and pass the attested real-GAM test-network suite

**Files:**

- Create: `crates/trusted-server-integration-tests/browser/tests/shared/aps-real-gam.spec.ts`
- Create: `crates/trusted-server-integration-tests/browser/helpers/gam-test-network.ts`
- Create: `crates/trusted-server-integration-tests/browser/playwright.real-gam.config.ts`
- Create: `crates/trusted-server-integration-tests/fixtures/configs/aps-real-gam.template.toml`
- Create: `.github/workflows/aps-real-gam.yml`
- Do not create a second runbook; this plan contains the release gate and commands

- [ ] **Step 1: Add a manually dispatched `aps-real-gam.yml` job using the protected GitHub**
      environment `aps-real-gam` and a required `workflow_dispatch` string input
      `release_id`, plus required `evidence_id` and `previous_artifact_id` inputs used
      only for attestation/rollback evidence. Its exact environment contract is
      `TS_REAL_GAM_PAGE_URL`, `TS_REAL_GAM_AUTH_HEADER`, and
      `TS_REAL_GAM_EXPECTED_RELEASE_ID`; the first two are protected secrets and no
      value is checked in. The template contains fictional placeholders only. This job
      is not added to ordinary PR CI, but a successful run for the exact release id is
      a mandatory cutover artifact.

  The dedicated Playwright config has no local `globalSetup`, `globalTeardown`,
  Viceroy, Docker, or WASM dependency. It runs only the remote real-GAM spec against
  `TS_REAL_GAM_PAGE_URL`; importing the shared local config is forbidden by a test.

- [ ] **Step 2: Cover SSAT APS-PUC, Trusted Server Prebid APS-PUC, page-bids APS-PUC, direct APS,**
      direct ADM plus baseline PBS Cache regression, attributable empty fallback, SRA, refresh, SPA navigation, and
      collapsed-shell resize.

- [ ] **Step 3: Add negative fixtures for wrong id/source, invalid descriptor, no outer claim,**
      no owner registration, no document acknowledgement, and APS runner failure.
      Exercise the live fixed-target proxy and assert route, DOM, nested-iframe, exact
      lifecycle callback, and terminal outcomes. There is no runner digest/version
      check: mutable APS bytes are not TS source or release identity.

- [ ] **Step 4: Capture browser console, GPT events, sanitized network metadata, DOM snapshots,**
      screenshots, and sanitized traces under `test-results/`, `playwright-report/`,
      and `real-gam-evidence/`. Disable response-body/HAR embedding for the APS runner
      and creative resources and strip any such bodies if the browser tool records them
      despite configuration. Before upload, inspect every archive/trace/HAR and fail if
      it contains a runner or creative response body, authorization value, account id,
      descriptor, or lifecycle capability; metadata such as URL/status/timing is
      allowed. Upload one artifact named
      `aps-real-gam-<run-id>` with the actual GitHub run id and 30-day retention.
      Pass/fail comes from
      exact request/DOM/lifecycle assertions.

- [ ] **Step 5: Require a clean pass in Chromium, Firefox, and WebKit before cutover. If the GAM**
      test network itself cannot support a browser, record that as a release blocker
      instead of silently weakening the criterion.

- [ ] **Step 6: Provide and verify the exact manual equivalent from the browser package:**

  ```bash
  TS_REAL_GAM_PAGE_URL=... \
  TS_REAL_GAM_AUTH_HEADER=... \
  TS_REAL_GAM_EXPECTED_RELEASE_ID=... \
  npm --prefix crates/trusted-server-integration-tests/browser exec -- \
    playwright test --config=playwright.real-gam.config.ts \
      tests/shared/aps-real-gam.spec.ts \
      --project=chromium --project=firefox --project=webkit
  ```

- [ ] **Step 7: Stage and commit the protected real-GAM suite and workflow together.**
      Task 24 may then amend and attest the checked-in workflow without referring to
      uncommitted tests, helpers, configuration, or fictional template data:

  ```bash
  git status --short
  git add \
    crates/trusted-server-integration-tests/browser/tests/shared/aps-real-gam.spec.ts \
    crates/trusted-server-integration-tests/browser/helpers/gam-test-network.ts \
    crates/trusted-server-integration-tests/browser/playwright.real-gam.config.ts \
    crates/trusted-server-integration-tests/fixtures/configs/aps-real-gam.template.toml \
    .github/workflows/aps-real-gam.yml
  git diff --cached --check
  git commit -m "Add protected APS real-GAM verification"
  ```

### Task 22: Delete final legacy surfaces and enforce absence

**Files:**

- Modify: `crates/trusted-server-js/lib/package.json`
- Modify: `crates/trusted-server-js/lib/eslint.config.js`
- Modify: `crates/trusted-server-js/lib/scripts/check-architecture.mjs`
- Create: `crates/trusted-server-js/lib/scripts/check-hard-cutover-absence.mjs`
- Modify: `crates/trusted-server-js/lib/src/core/global.d.ts`
- Modify: `crates/trusted-server-js/lib/src/core/index.ts`
- Modify: `crates/trusted-server-js/lib/src/core/types.ts`
- Delete: `crates/trusted-server-js/lib/src/core/context.ts`
- Modify: `crates/trusted-server-js/lib/src/integrations/gpt/index.ts`
- Modify: `crates/trusted-server-js/lib/src/integrations/prebid/index.ts`
- Modify: `crates/trusted-server-js/lib/src/integrations/aps/render.ts`
- Modify: `crates/trusted-server-js/lib/src/integrations/creative/index.ts`
- Modify: `crates/trusted-server-js/lib/src/shared/globals.ts`
- Modify: `crates/trusted-server-js/lib/src/integrations/didomi/index.ts`
- Modify: `crates/trusted-server-js/lib/src/integrations/gpt_diagnostics/index.ts`
- Modify: `crates/trusted-server-js/lib/src/integrations/sourcepoint/index.ts`
- Modify: `crates/trusted-server-js/lib/build-prebid-external.mjs`
- Modify: `crates/trusted-server-js/lib/test/core/request.test.ts`
- Delete: `crates/trusted-server-js/lib/test/core/context.test.ts`
- Modify: `crates/trusted-server-js/lib/test/integrations/aps/render.test.ts`
- Modify: `crates/trusted-server-js/lib/test/integrations/gpt/ad_init.test.ts`
- Modify: `crates/trusted-server-js/lib/test/integrations/gpt/spa_hook.test.ts`
- Modify: `crates/trusted-server-js/lib/test/integrations/prebid/index.test.ts`
- Modify: `crates/trusted-server-js/lib/test/integrations/creative/helpers.ts`
- Modify: `crates/trusted-server-js/lib/test/integrations/didomi/index.test.ts`
- Modify: `crates/trusted-server-js/lib/test/integrations/gpt/index.test.ts`
- Modify: `crates/trusted-server-js/lib/test/integrations/gpt_diagnostics/bootstrap.test.ts`
- Modify: `crates/trusted-server-js/lib/test/integrations/gpt_diagnostics/index.test.ts`
- Modify: `crates/trusted-server-js/lib/test/integrations/sourcepoint/index.test.ts`
- Modify: `crates/trusted-server-js/lib/test/prebid-artifact-integration.test.mjs`
- Modify: `crates/trusted-server-core/src/html_processor.rs`
- Modify: `crates/trusted-server-core/src/integrations/prebid.rs`
- Modify: `crates/trusted-server-core/src/integrations/didomi.rs`
- Modify: `crates/trusted-server-core/src/integrations/sourcepoint.rs`
- Modify: `crates/trusted-server-core/src/integrations/gpt.rs`
- Modify: `crates/trusted-server-core/src/integrations/gpt_diagnostics.rs`
- Modify: `crates/trusted-server-core/src/integrations/gpt_diagnostics_bootstrap.js`
- Modify: `crates/trusted-server-core/src/publisher.rs`
- Modify: `crates/trusted-server-core/src/integrations/aps.rs`
- Modify: `crates/trusted-server-core/src/auth.rs`
- Modify: `crates/trusted-server-adapter-fastly/src/app.rs`
- Modify: `crates/trusted-server-adapter-axum/src/app.rs`
- Modify: `crates/trusted-server-adapter-cloudflare/src/app.rs`
- Modify: `crates/trusted-server-adapter-spin/src/app.rs`
- Modify: `crates/trusted-server-adapter-axum/tests/routes.rs`
- Modify: `crates/trusted-server-adapter-cloudflare/tests/routes.rs`
- Modify: `crates/trusted-server-adapter-spin/tests/routes.rs`
- Modify: `crates/trusted-server-integration-tests/tests/parity.rs`
- Modify: `crates/trusted-server-integration-tests/browser/tests/shared/aps-renderer.spec.ts`
- Modify: `crates/trusted-server-integration-tests/browser/tests/shared/tsjs-performance.spec.ts`
- Modify: `docs/guide/integrations/aps.md`
- Modify: `docs/guide/auction-orchestration.md`
- Modify: `docs/guide/configuration.md`
- Modify: `docs/guide/creative-processing.md`
- Modify: `docs/guide/integration-guide.md`
- Modify: `docs/guide/integrations/prebid.md`
- Modify: `docs/guide/integrations/didomi.md`
- Modify: `.github/workflows/test.yml`

- [ ] **Step 1: Add an absence test/search for:**
  - `globalThis.tscreative`, `globalThis.tsCreativeConfig`, `installGuards`,
    `setConfig`, and `getConfig`;
  - legacy `window.__tsjs_*` runtime flags;
  - callback/void `requestAds`, `renderAdUnit`, `renderAllAdUnits`, mutable generic
    config, `TsjsApiV1`, and every old public declaration/alias;
  - `tsjs.renders`, `renderLog`, `renderSeq`, `tsjs:adRendered`,
    `tsjs.gptDiagnostics`, and old GPT diagnostics flags/expandos;
  - `__tsRenderGeneration` and `__tsRenderBid`;
  - `tsjs.apsPrebidRenderers`;
  - the module-global core context-provider map and integration imports of
    `registerContextProvider`/`collectContext`;
  - integration-owned GPT/Prebid function sentinels;
  - duplicate `Prebid Request` listeners and refresh wrappers;
  - empty catches in migrated paths;
  - `PAGE_BIDS_LEGACY_PATH`, `/__ts/page-bids`, and the JS retry/fallback marker;
  - the unversioned `/integrations/aps/renderer` route;
  - APS `pub_id` deserialization alias and its compatibility documentation;
  - any APS runner asset, copied body, version/digest/metadata/license record,
    updater/downloader, SRI/integrity attribute, generated runner artifact, offline
    fallback, positive `/integrations/aps/runner/v1.js` route, or runner-cache
    requirement;
  - any enabled `TS_TEST_APS_V1`, integration-only upstream resolver, loopback
    fixture address, Wrangler service binding, or Spin/Viceroy proxy-test manifest
    reference in a production build/release artifact;
  - every temporary architectural lint allowlist entry.

  For `window.__tsjs_*`, assert absence in all shipped JavaScript, including the pure
  generated Prebid external artifact. For every former scattered public flag/config
  emitter or consumer represented by exact `TsjsBootV1`, separately assert the
  immutable `tsjs.boot.auctionProjection`, `tsjs.boot.creative`, or
  `tsjs.boot.diagnostics` replacement in server output, consumers, fixtures, and
  current guides; deleting an emitter without migrating its value is a test failure.
  Other product-specific configuration uses the typed integration binding permitted
  by §5.4: the exact transient `_integrationConfig` transport is admitted only before
  commit, validated/snapshotted/recursively frozen into release-matched module
  bindings, and deleted before publishing the kernel or fallback API. It is not a
  public compatibility surface, does not alter exact `TsjsBootV1`, and must not be
  present in the final API or external Prebid artifact.

  Scope the executable search to shipped source, current guides, tests, scripts,
  and workflows; exclude historical `docs/superpowers` designs/plans because this
  work does not rewrite separate completed specifications. The enumerated file list
  above is the current baseline hit inventory and must be updated if Task 0 or the
  Task 22 RED absence run finds another in-scope hit. The performance fixture is one
  such discovered hit: migrate only its legacy API use here; Task 23 still owns its
  measurements and budgets.

- [ ] **Step 2: Delete unreachable old paths, compatibility declarations,**
      branches, and test-only production exports. Keep only `tsjs.que`, `tsjs.boot`,
      the exact `TsjsApi` public surfaces, `_registerIntegration`, and the frozen
      status-only `_internal` surface described by the spec. Do not expose the
      service registry or a second integration-registration name.

- [ ] **Step 3: Make `/__ts/page-bids`, the unversioned renderer path, unknown renderer versions,**
      and `/integrations/aps/runner/v1.js` local unknown-route responses, never aliases.
      Keep only `/_ts/page-bids`, static `/integrations/aps/renderer/v1`, and live
      fixed-target proxy `/integrations/aps/runner.js`. Update route/parity tests and
      APS/configuration guides to those hard-cutover surfaces. The absence test must
      distinguish the required negative `runner/v1.js` assertion from a forbidden
      positive handler.

- [ ] **Step 4: Add and execute the hard-cutover absence gate.** Implement the scoped
      inventory from Step 1 in
      `crates/trusted-server-js/lib/scripts/check-hard-cutover-absence.mjs`, expose it
      as `check:hard-cutover-absence` in `package.json`, and run it in `test.yml`
      after both the normal TSJS build and pure external Prebid artifact build. The
      script exits nonzero on any forbidden source, guide, fixture, generated bundle,
      or workflow hit and prints the exact file, match, and violated rule. Build all
      integration modules, build the external Prebid artifact, run the absence gate,
      and rerun the TS and adapter route/parity suites:

  ```bash
  npm --prefix crates/trusted-server-js/lib run build
  npm --prefix crates/trusted-server-js/lib run build:prebid-external
  npm --prefix crates/trusted-server-js/lib run check:hard-cutover-absence
  npm --prefix crates/trusted-server-js/lib test
  cargo test-fastly
  cargo test-axum
  cargo test-cloudflare
  cargo test-spin
  cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test parity
  git diff --check
  ```

  Stage only the Task 22 files—including deletions—and commit the gate together with
  the hard-cutover cleanup it proves:

  ```bash
  git add \
    crates/trusted-server-js/lib/package.json \
    crates/trusted-server-js/lib/eslint.config.js \
    crates/trusted-server-js/lib/scripts/check-architecture.mjs \
    crates/trusted-server-js/lib/scripts/check-hard-cutover-absence.mjs \
    crates/trusted-server-js/lib/src/core/global.d.ts \
    crates/trusted-server-js/lib/src/core/index.ts \
    crates/trusted-server-js/lib/src/core/types.ts \
    crates/trusted-server-js/lib/src/core/context.ts \
    crates/trusted-server-js/lib/src/integrations/gpt/index.ts \
    crates/trusted-server-js/lib/src/integrations/prebid/index.ts \
    crates/trusted-server-js/lib/src/integrations/aps/render.ts \
    crates/trusted-server-js/lib/src/integrations/creative/index.ts \
    crates/trusted-server-js/lib/src/shared/globals.ts \
    crates/trusted-server-js/lib/src/integrations/didomi/index.ts \
    crates/trusted-server-js/lib/src/integrations/gpt_diagnostics/index.ts \
    crates/trusted-server-js/lib/src/integrations/sourcepoint/index.ts \
    crates/trusted-server-js/lib/build-prebid-external.mjs \
    crates/trusted-server-js/lib/test/core/request.test.ts \
    crates/trusted-server-js/lib/test/core/context.test.ts \
    crates/trusted-server-js/lib/test/integrations/aps/render.test.ts \
    crates/trusted-server-js/lib/test/integrations/gpt/ad_init.test.ts \
    crates/trusted-server-js/lib/test/integrations/gpt/spa_hook.test.ts \
    crates/trusted-server-js/lib/test/integrations/prebid/index.test.ts \
    crates/trusted-server-js/lib/test/integrations/creative/helpers.ts \
    crates/trusted-server-js/lib/test/integrations/didomi/index.test.ts \
    crates/trusted-server-js/lib/test/integrations/gpt/index.test.ts \
    crates/trusted-server-js/lib/test/integrations/gpt_diagnostics/bootstrap.test.ts \
    crates/trusted-server-js/lib/test/integrations/gpt_diagnostics/index.test.ts \
    crates/trusted-server-js/lib/test/integrations/sourcepoint/index.test.ts \
    crates/trusted-server-js/lib/test/prebid-artifact-integration.test.mjs \
    crates/trusted-server-core/src/html_processor.rs \
    crates/trusted-server-core/src/integrations/prebid.rs \
    crates/trusted-server-core/src/integrations/didomi.rs \
    crates/trusted-server-core/src/integrations/sourcepoint.rs \
    crates/trusted-server-core/src/integrations/gpt.rs \
    crates/trusted-server-core/src/integrations/gpt_diagnostics.rs \
    crates/trusted-server-core/src/integrations/gpt_diagnostics_bootstrap.js \
    crates/trusted-server-core/src/publisher.rs \
    crates/trusted-server-core/src/integrations/aps.rs \
    crates/trusted-server-core/src/auth.rs \
    crates/trusted-server-adapter-fastly/src/app.rs \
    crates/trusted-server-adapter-axum/src/app.rs \
    crates/trusted-server-adapter-cloudflare/src/app.rs \
    crates/trusted-server-adapter-spin/src/app.rs \
    crates/trusted-server-adapter-axum/tests/routes.rs \
    crates/trusted-server-adapter-cloudflare/tests/routes.rs \
    crates/trusted-server-adapter-spin/tests/routes.rs \
    crates/trusted-server-integration-tests/tests/parity.rs \
    crates/trusted-server-integration-tests/browser/tests/shared/aps-renderer.spec.ts \
    crates/trusted-server-integration-tests/browser/tests/shared/tsjs-performance.spec.ts \
    docs/guide/integrations/aps.md \
    docs/guide/auction-orchestration.md \
    docs/guide/configuration.md \
    docs/guide/creative-processing.md \
    docs/guide/integration-guide.md \
    docs/guide/integrations/prebid.md \
    docs/guide/integrations/didomi.md \
    .github/workflows/test.yml
  git diff --cached --check
  git commit -m "Enforce hard-cutover surface absence"
  ```

### Task 23: Add deterministic bundle, browser-time, and retained-heap gates

**Files:**

- Read: `crates/trusted-server-js/lib/build-all.mjs`
- Read: `crates/trusted-server-js/lib/scripts/check-bundle-budgets.mjs`
- Read: `crates/trusted-server-js/lib/scripts/bundle-metrics.mjs`
- Read: `crates/trusted-server-js/lib/test/fixtures/performance/aps-tsjs-prechange.json`
- Read: `crates/trusted-server-integration-tests/browser/tests/shared/tsjs-performance.spec.ts`
- Read: `scripts/validate-tsjs-performance-evidence.mjs`
- Read: `.github/workflows/tsjs-performance-gate.yml`

The E5/E7 bundle-metrics helper, comparator, browser test, evidence validator, and
standalone workflow are the frozen measurement instrument. Task 23 changes none of
them. Later general workflow composition may change, but never this standalone job or
its called instrument paths. Any measurement-logic, membership, ceiling, or sampling
change invalidates the pre-switch evidence and must return to Task 18E under separate
design review. Production-only route selection is part of Task 19 and cannot alter
sampling or comparison logic.

- [ ] **Step 1: Consume, but do not regenerate, both immutable subtrees captured in
      Task 0 and Task 18E.** Build the canonical release inventory fresh. Print the
      original `bundles` values and current deltas as report-only historical evidence;
      never apply those old-membership numbers as post-split ceilings. For
      `roleCorrectTransfer`, independently enforce bootstrap, minimal critical,
      reference critical, and maximal total with
      `ceil(capturedBytes * 1.05)` for raw/gzip/Brotli. Revalidate the recorded source
      parent, tool/compression identities, exact semantic membership,
      historical-evidence digest, and release inventory. Validate every current
      artifact hash against its current bytes; do not require current post-switch
      hashes or release id to equal the frozen pre-switch capture. Captured hashes and
      release id are immutable provenance reproducible by checking out the capture
      source SHA, while current transfer bytes alone are compared to the frozen
      ceilings. Reject missing/unclassified/
      multiply counted output, reference reachability to deferred source, provider
      inlining, duplicate adapter/runtime/listener ownership, production test/fake/
      no-op seams, and a maximal inventory that omits any split module. A separate
      reviewed design is required to alter historical evidence, recapture the
      role-correct values, change membership, or change the 5% formula.

- [ ] **Step 2: Rerun the exact pre-switch browser-time gate after the production
      switch.** On Chromium 145.0.7632.6, `github-hosted:ubuntu-24.04`, and fixture
      `tsjs-generated-loopback-paired-v2`, alternate the frozen pre-switch reference
      and post-switch current variant for five warmups and 50 samples per variant.
      Require current p90 ≤ reference p90 × 1.10 and p90 ≤100 ms for each side. Do
      not rerun selectively to turn a failed sample into a pass. The post-switch
      sample reads the real `tsjs:bids-script`
      and `tsjs:first-display` performance marks and the
      `tsjs:boot-to-first-display` measure installed in Task 19; fail if any sample
      falls back to the pre-change `window.__tsjsPerf` placeholder or lacks either
      mark. The fixture must use the production-wired server controller, critical
      artifact, runtime, and first-display adapter path.

- [ ] **Step 3: Rerun the identical Chromium CDP retained-heap protocol after the
      switch.** After the display samples, open one separate fresh context per
      variant and execute the real fixture once. At each checkpoint send
      `HeapProfiler.collectGarbage` exactly once, immediately call
      `Runtime.getHeapUsage`, and compare that single `usedSize` without averaging,
      max selection, or rerun. At boot, first render, refresh, and SPA navigation,
      require current ≤ frozen reference × 1.10 and both variants ≤4 MiB.
      Firefox/WebKit remain correctness-only.

- [ ] **Step 4: Assert the load-time behavior, not only the numbers.** The reference
      page makes exactly one critical TSJS request; no deferred request, preload,
      preparation, or execution occurs before `tsjs:first-display-paint`; all
      included deferred requests start independently after the gate; and a blocked
      first deferred module does not delay another. Keep these gates separate from
      render correctness: performance cannot convert a failed conformance test to a
      pass.

- [ ] **Step 5: Run the gate from a clean checkout through the same fixture-preparation script**
      and write a separate measured artifact; never overwrite the Task 0 baseline:

  ```bash
  TS_BROWSER_FRAMEWORKS=nextjs \
  TS_BROWSER_PROJECTS=chromium \
  TSJS_PERF_MODE=gate \
  TSJS_PERF_OUTPUT=crates/trusted-server-integration-tests/browser/test-results/tsjs-performance-current.json \
    ./scripts/integration-tests-browser.sh \
      tests/shared/tsjs-performance.spec.ts --project=chromium
  node crates/trusted-server-js/lib/scripts/check-bundle-budgets.mjs
  ```

- [ ] **Step 6: Prove the frozen post-switch instrument and its CI run without a gate-
      code commit.** Dispatch the exact E7 workflow for the pushed post-switch SHA,
      wait for success, download the uniquely named artifact, and validate its head
      SHA, evidence id, environment/fixture, sample/heap counts, values, load ordering,
      and pass status with the unchanged E7 validator. `git diff --exit-code` over all
      instrument paths must remain byte-identical to the E7 commit as well as clean in
      the worktree. Push and verify the exact post-switch SHA before dispatch:

  ```bash
  git fetch origin \
    'refs/tags/tsjs-performance-instrument-*:refs/tags/tsjs-performance-instrument-*'
  PRESWITCH_TAGS="$(git tag --list 'tsjs-performance-instrument-*' \
    --merged HEAD)"
  test "$(printf '%s\n' "$PRESWITCH_TAGS" | sed '/^$/d' | wc -l | tr -d ' ')" -eq 1
  PRESWITCH_TAG="$(printf '%s\n' "$PRESWITCH_TAGS" | sed '/^$/d')"
  PRESWITCH_SHA="$(git rev-parse "$PRESWITCH_TAG^{commit}")"
  test "$PRESWITCH_TAG" = "tsjs-performance-instrument-$PRESWITCH_SHA"
  git merge-base --is-ancestor "$PRESWITCH_SHA" HEAD
  git diff --exit-code "$PRESWITCH_SHA" -- \
    crates/trusted-server-js/lib/scripts/bundle-metrics.mjs \
    crates/trusted-server-js/lib/scripts/check-bundle-budgets.mjs \
    crates/trusted-server-integration-tests/browser/tests/shared/tsjs-performance.spec.ts \
    scripts/validate-tsjs-performance-evidence.mjs \
    .github/workflows/tsjs-performance-gate.yml
  test -z "$(git status --porcelain)"
  POSTSWITCH_REF="$(git branch --show-current)"
  POSTSWITCH_SHA="$(git rev-parse HEAD)"
  git push origin "$POSTSWITCH_REF"
  git fetch origin "$POSTSWITCH_REF"
  test "$POSTSWITCH_SHA" = "$(git rev-parse "origin/$POSTSWITCH_REF")"
  POSTSWITCH_EVIDENCE_ID="aps-tsjs-postswitch-${POSTSWITCH_SHA}"
  POSTSWITCH_RELEASE_ID="$(npm --prefix crates/trusted-server-js/lib run --silent print:release-id)"
  POSTSWITCH_RUN_ID="$(node scripts/dispatch-workflow-run.mjs \
    integration-tests.yml \
    "$POSTSWITCH_REF" \
    evidence_id="$POSTSWITCH_EVIDENCE_ID" \
    release_id="$POSTSWITCH_RELEASE_ID" \
    previous_artifact_id=not-applicable-performance-only)"
  gh run watch "$POSTSWITCH_RUN_ID" --exit-status
  POSTSWITCH_EVIDENCE_DIR="$(mktemp -d)"
  gh run download "$POSTSWITCH_RUN_ID" \
    --name "tsjs-performance-$POSTSWITCH_EVIDENCE_ID" \
    --dir "$POSTSWITCH_EVIDENCE_DIR"
  node scripts/validate-tsjs-performance-evidence.mjs \
    --file "$POSTSWITCH_EVIDENCE_DIR/tsjs-performance-postswitch.json" \
    --evidence-id "$POSTSWITCH_EVIDENCE_ID" \
    --head-sha "$POSTSWITCH_SHA" \
    --mode postswitch
  ```

  Dispatch through `integration-tests.yml` while the reusable performance workflow
  exists only on the PR branch: GitHub resolves `workflow_dispatch` entrypoints from
  the default branch, but the wrapper can invoke the exact branch-local reusable
  workflow. The post-switch prefix selects only the performance job; the rollback
  artifact input is required by the wrapper schema but is not consumed by this
  non-deployment evidence run.

### Task 24: Run final repository verification and assemble the cutover evidence

**Files:**

- Modify: `.github/workflows/test.yml`
- Modify: `.github/workflows/integration-tests.yml`
- Modify: `.github/workflows/aps-real-gam.yml`
- Do not create another plan/design/runbook; workflow artifacts are the evidence

- [ ] **Step 1: Run formatting:**

  ```bash
  cargo fmt --all -- --check
  npm --prefix crates/trusted-server-js/lib run format
  npm --prefix docs run format
  ```

- [ ] **Step 2: Run Rust correctness and lint for every adapter:**

  ```bash
  cargo test-fastly
  cargo test-axum
  cargo test-cloudflare
  cargo test-spin
  cargo clippy-fastly
  cargo clippy-axum
  cargo clippy-cloudflare
  cargo clippy-cloudflare-wasm
  cargo clippy-spin-native
  cargo clippy-spin-wasm
  cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test parity
  ./scripts/integration-tests-aps-runner-proxy.sh --runtime axum
  ./scripts/integration-tests-aps-runner-proxy.sh --runtime fastly
  ./scripts/integration-tests-aps-runner-proxy.sh --runtime cloudflare
  ./scripts/integration-tests-aps-runner-proxy.sh --runtime spin
  cargo clippy --manifest-path crates/trusted-server-integration-tests/Cargo.toml --all-targets -- -D warnings
  ```

- [ ] **Step 3: Run TypeScript and bundle verification:**

  ```bash
  npm --prefix crates/trusted-server-js/lib ci
  npm --prefix crates/trusted-server-js/lib run typecheck
  npm --prefix crates/trusted-server-js/lib run lint
  npm --prefix crates/trusted-server-js/lib test
  npm --prefix crates/trusted-server-js/lib run build
  npm --prefix crates/trusted-server-js/lib run build:prebid-external
  npm --prefix crates/trusted-server-js/lib run check:aps-contract
  npm --prefix crates/trusted-server-js/lib run check:architecture
  node --test crates/trusted-server-js/lib/test/contract/rc-july-adoption.test.mjs
  node crates/trusted-server-js/lib/scripts/check-bundle-budgets.mjs
  npm --prefix docs run lint
  npm --prefix docs run build
  ```

- [ ] **Step 4: Configure, commit, and run three clean-checkout workflows for the exact**
      release commit. Add `workflow_dispatch` with required `evidence_id` and
      `release_id` inputs to
      `test.yml`; that workflow executes the complete format/typecheck/lint/Vitest/
      bundle/Rust-test/clippy matrix from steps 1–3 and uploads its command logs plus
      validated release id. The integration workflow owns adapter startup/artifact
      setup and executes the full integration suite plus focused
      Chromium/Firefox/WebKit APS files. The protected real-GAM workflow runs for the
      same ref/release id.

  Add manual `evidence_id`, `release_id`, and `previous_artifact_id` inputs where
  relevant and include the evidence id in each workflow's `run-name`. After the
  checked build, derive `RELEASE_ID` only from Task 8's validated generated
  manifest. For the repository's Fastly production target, the authoritative prior
  immutable artifact is the one active Fastly service version immediately before
  dispatch; query it through the pinned Fastly CLI and require exactly one active
  version. Pass both values explicitly; an empty/ambiguous value is a blocker.
  Dispatch only a pushed branch verified to resolve to `RELEASE_SHA`, capture each
  exact run id through the Task 0 dispatch helper, wait with `--exit-status`, and
  verify every `headSha`, conclusion, and release-id attestation.

  Before committing or dispatching, configure the workflows to upload
  `aps-tsjs-quality-<run-id>`, `aps-tsjs-cutover-<commit-sha>`, and
  `aps-real-gam-<run-id>` artifacts, substituting actual GitHub values. Include exact
  command logs, sanitized Playwright reports/traces, route parity output,
  corpus/staleness output, bundle/performance reports, release id, commit SHA, run
  id, conclusion, and where applicable the prior deployable artifact id. Run the
  Task 21 pre-upload scrub on every browser artifact and fail if APS runner/creative
  bodies, secrets, descriptors, or capabilities are present. Stage and commit all
  three workflow definitions, push that commit, and only then derive and dispatch
  the release SHA:

  ```bash
  git add \
    .github/workflows/test.yml \
    .github/workflows/integration-tests.yml \
    .github/workflows/aps-real-gam.yml
  git diff --cached --check
  git commit -m "Attest the APS TSJS cutover workflows"
  RELEASE_REF="$(git branch --show-current)"
  git push origin "$RELEASE_REF"
  RELEASE_SHA="$(git rev-parse HEAD)"
  RELEASE_ID="$(npm --prefix crates/trusted-server-js/lib run --silent print:release-id)"
  test -n "$RELEASE_REF"
  test -n "$RELEASE_ID"
  test -n "$FASTLY_SERVICE_ID"
  PREVIOUS_FASTLY_VERSION="$(fastly service version list \
    --service-id "$FASTLY_SERVICE_ID" --json | \
    jq -er '[.[] | select(.active == true)] | if length == 1 then .[0].number else error("expected one active Fastly version") end')"
  PREVIOUS_ARTIFACT_ID="fastly-service-version:$PREVIOUS_FASTLY_VERSION"
  git fetch origin "$RELEASE_REF"
  test "$RELEASE_SHA" = "$(git rev-parse "origin/$RELEASE_REF")"
  test -n "$PREVIOUS_ARTIFACT_ID"
  EVIDENCE_ID="aps-tsjs-cutover-$RELEASE_SHA"
  QUALITY_RUN_ID="$(node scripts/dispatch-workflow-run.mjs \
    test.yml "$RELEASE_REF" \
    evidence_id="$EVIDENCE_ID" \
    release_id="$RELEASE_ID")"
  INTEGRATION_RUN_ID="$(node scripts/dispatch-workflow-run.mjs \
    integration-tests.yml "$RELEASE_REF" \
    evidence_id="$EVIDENCE_ID" \
    release_id="$RELEASE_ID" \
    previous_artifact_id="$PREVIOUS_ARTIFACT_ID")"
  REAL_GAM_RUN_ID="$(node scripts/dispatch-workflow-run.mjs \
    aps-real-gam.yml "$RELEASE_REF" \
    evidence_id="$EVIDENCE_ID" \
    release_id="$RELEASE_ID" \
    previous_artifact_id="$PREVIOUS_ARTIFACT_ID")"
  for RUN_ID in "$QUALITY_RUN_ID" "$INTEGRATION_RUN_ID" "$REAL_GAM_RUN_ID"; do
    gh run watch "$RUN_ID" --exit-status
    test "$RELEASE_SHA" = "$(gh run view "$RUN_ID" --json headSha --jq .headSha)"
    test success = "$(gh run view "$RUN_ID" --json conclusion --jq .conclusion)"
  done
  ```

  All three exact runs must conclude `success`; every evidence manifest must report
  the same embedded release id and commit SHA, and integration plus real-GAM
  artifacts must report the same prior artifact id. The dispatch helper or a
  post-download manifest check verifies those fields against `RELEASE_ID`,
  `RELEASE_SHA`, and `PREVIOUS_ARTIFACT_ID`; a run-name alone is not evidence. An
  unavailable protected environment is a blocker.

- [ ] **Step 5: Audit the final diff:**
  - only planned source/test/build/runbook surfaces changed;
  - no old/new compatibility path remains;
  - no new external observability, persistence, billing, or experiment artifact;
  - no descriptor/capability/account/creative payload is logged;
  - no empty catch or unowned timer/listener/port/iframe in migrated paths;
  - every one of the 144 pinned `rc/july` files maps through all 38 live rows to the
    same 23 ledger ids, with no unmapped/dead/gap result;
  - no integration-module preparation performs observable work, no activation yields,
    and no post-fallback callback can revive the kernel;
  - the public surface is `TsjsApi` only; numeric suffixes remain only on serialized
    versioned boot/wire/artifact schemas; and
  - no unrelated integration behavior was refactored.

- [ ] **Step 6: Download and validate the artifacts that Step 4 configured before the**
      release commit. Each artifact contains `evidence-manifest.json`; validate its
      evidence id, release id, commit SHA, run id, conclusion, and applicable prior
      artifact id against the exact dispatched values. The GitHub Actions artifacts
      for those three successful runs are the sole evidence location; do not modify
      a workflow after dispatch and do not create another repository document:

  ```bash
  CUTOVER_EVIDENCE_DIR="$(mktemp -d)"
  gh run download "$QUALITY_RUN_ID" \
    --name "aps-tsjs-quality-$QUALITY_RUN_ID" \
    --dir "$CUTOVER_EVIDENCE_DIR/quality"
  gh run download "$INTEGRATION_RUN_ID" \
    --name "aps-tsjs-cutover-$RELEASE_SHA" \
    --dir "$CUTOVER_EVIDENCE_DIR/integration"
  gh run download "$REAL_GAM_RUN_ID" \
    --name "aps-real-gam-$REAL_GAM_RUN_ID" \
    --dir "$CUTOVER_EVIDENCE_DIR/real-gam"
  jq -e --arg evidence "$EVIDENCE_ID" --arg release "$RELEASE_ID" \
    --arg sha "$RELEASE_SHA" --arg run "$QUALITY_RUN_ID" \
    '.evidenceId == $evidence and .releaseId == $release and
     .commitSha == $sha and .runId == $run and .conclusion == "success"' \
    "$CUTOVER_EVIDENCE_DIR/quality/evidence-manifest.json"
  for ENTRY in "integration:$INTEGRATION_RUN_ID" "real-gam:$REAL_GAM_RUN_ID"; do
    ARTIFACT_KIND="${ENTRY%%:*}"
    ARTIFACT_RUN_ID="${ENTRY#*:}"
    jq -e --arg evidence "$EVIDENCE_ID" --arg release "$RELEASE_ID" \
      --arg sha "$RELEASE_SHA" --arg run "$ARTIFACT_RUN_ID" \
      --arg previous "$PREVIOUS_ARTIFACT_ID" \
      '.evidenceId == $evidence and .releaseId == $release and
       .commitSha == $sha and .runId == $run and .conclusion == "success" and
       .previousArtifactId == $previous' \
      "$CUTOVER_EVIDENCE_DIR/$ARTIFACT_KIND/evidence-manifest.json"
  done
  ```

## Cutover procedure

Use the existing deployment mechanism; this plan adds no router or experiment
infrastructure.

1. Deploy to pre-production and rerun renderer-route, direct, PUC, refresh, SRA, and
   SPA smoke tests.
2. Confirm the final artifact contains only the new runtime/API and that server and
   TSJS bundles belong to the same ordinary release.
3. Hold an exclusive production deployment window from Task 24 evidence capture
   through cutover. Immediately before deployment, re-query the active Fastly
   version with the same exact command and require it to equal the attested
   `PREVIOUS_FASTLY_VERSION`:

   ```bash
   CURRENT_FASTLY_VERSION="$(fastly service version list \
     --service-id "$FASTLY_SERVICE_ID" --json | \
     jq -er '[.[] | select(.active == true)] | if length == 1 then .[0].number else error("expected one active Fastly version") end')"
   test "$CURRENT_FASTLY_VERSION" = "$PREVIOUS_FASTLY_VERSION"
   ```

   Any mismatch blocks deployment and requires fresh evidence for the new prior
   artifact; never roll back to a stale attestation.

4. Perform one binary production cutover. Immediately verify:
   - existing service availability/error/latency health is normal;
   - APS renderer endpoint smoke passes;
   - direct APS and one GAM/PUC APS smoke pass;
   - no CSP/security console regression;
   - no non-APS cache/ADM, native Prebid, refresh, SRA, or SPA regression.

5. Hold before cutover if evidence is missing. After cutover, roll back the complete
   artifact immediately on a TS code, request, CSP/security, or non-APS regression;
   do not add percentage routing or dual-pool infrastructure. For Fastly, rollback
   reactivates the exact attested prior immutable version:

   ```bash
   fastly service version activate \
     --service-id "$FASTLY_SERVICE_ID" \
     --version "$PREVIOUS_FASTLY_VERSION"
   ```

6. Treat a live-runner incident separately because binary rollback cannot restore
   older APS-owned bytes. If the proxied runner is unavailable, incompatible, or
   produces suspect completion behavior, disable `[integrations.aps]` using the
   existing configuration mechanism. Verify that new APS bids are not admitted and
   both reserved APS routes return local `404 no-store` without publisher fallback.
   Keep APS disabled until the controlled Chromium/Firefox/WebKit real-browser
   conformance gate passes again; do not vendor or pin a runner as containment.
7. Monitor existing operational signals for 24 hours and rerun the focused
   real-browser smoke suite.
8. Confirm again that the deployed artifact contains no development selector or
   compatibility path; Task 22 made this a pre-release absence gate.

## Completion criteria

The plan is complete only when:

1. All five render flows pass exact hermetic and real-GAM lifecycle assertions, and
   every created attempt settles exactly once under the mandatory race matrix.
2. Rust, TypeScript, embedded ES5, programmatic registration, APS/ADM rendering, and
   DOM validation agree on the exact descriptor grammar and 1–4096 dimensions while
   preserving invalid-versus-out-of-range reasons; PBS Cache passes only its pinned
   black-box non-regression corpus.
3. Initial and SPA projections enforce all grammar/count/UTF-8/8 MiB bounds
   transactionally; SPA state lives only in `NavigationSession`, boot stays frozen,
   and over-cap projections reduce all winners to `winner_not_renderable` without TS
   projected bids or `/auction` seatbids.
4. APS/ADM server reservations are the sole TS PUC authority, attempt/ticket/nonce issuers are
   bounded as specified, and every live reservation/attempt retains the immutable
   `WinnerContext` from its exact selected winner. Native PBS Cache UUIDs remain
   separate baseline transport identities and never enter the reservation registry.
5. GPT physical-cycle ownership, exact handoff, two-success DOM reconciliation,
   transactional destroy/redefine, and the owner-and-value targeting journal pass all
   publisher-mutation and same-string generation races without a second physical
   slot or blind clear.
6. PUC capture, owner-control, direct/remote iframe cleanup, accepted-artifact
   promotion, and the 20-second owner watchdog obey their exact ownership boundaries;
   native Prebid requests remain untouched.
7. One runtime owns all integration modules, sessions, slots, projections, auction-
   context contributors, reservations, batches, targeting frames, timers, listeners,
   observers, ports, and renderer iframes. Module preparation is inert, activation is
   synchronous and reversible, commit is atomic, and post-commit work cannot expose a
   partial kernel.
8. The kernel and fallback expose their exact `TsjsApi` own surfaces, semantic
   version/release identity, immutable boot, actual-Array queue semantics, logger,
   programmatic registration, Promise `requestAds`, and diagnostics presence. No
   `TsjsApiV1` or placeholder/callback alias exists.
9. The pure external Prebid.js 10.26.0 artifact independently drains publisher work,
   implements the exact frozen artifact stamp and duplicate/conflict rules, binds
   `pbjs` plus stamp identity, and contains no TS auction, admission, render,
   targeting, refresh, global flag, or TSJS release coupling.
10. Core diagnostics ingress admits only exact bounded copied/frozen data trees and
    has no integration subscription machinery. Render trace and GPT diagnostics
    expose only their exact bounded frozen asynchronous APIs; private trace
    presentation is available only through `trace.presentation.v1`; compound GPT
    physical-slot/request-cycle identity cannot conflate refresh impressions; no
    correctness callback runs publisher diagnostics code, no legacy event or alias
    remains, inactive GPT diagnostics has zero incremental side effects, and creative
    guards auto-install from exact frozen boot data with both-false zero DOM effects.
11. Every one of the 144 pinned `rc/july` files maps through 38 live mappings to all
    23 ledger ids, and the complete GPT, Prebid, APS, creative, diagnostics, shared-
    helper, remaining-integration, and browser parity corpora pass alone and in the
    maximal manifest.
12. Static renderer and live fixed-target runner-proxy status, exact headers,
    bounded/deadline behavior, and fail-closed evidence parsing are proven through
    all four actual adapter transports; no APS runner bytes/version/digest/license/
    SRI/updater/fallback/cache requirement exists in TS source or release evidence.
13. Legacy globals, expandos, sentinels, duplicate listeners/wrappers, mutable
    creative/diagnostics surfaces, old routes, old declarations, and compatibility
    shape detection are absent after the hard cutover.
14. Non-APS cache/ADM, native Prebid, publisher GPT, SRA, refresh, SPA, creative,
    notification, and every other integration regression suite passes without an
    unrelated feature rewrite.
15. Format, lint, typecheck, adoption/architecture/absence checks, all adapter tests
    and clippy targets, Vitest, bundle/artifact builds, Playwright, immutable
    historical bundle reporting, role-correct transfer ceilings, browser-time, and
    retained-heap gates pass in attested clean-checkout quality, integration, and
    real-GAM runs for the exact release SHA and release id.
16. The binary cutover and 24-hour monitor complete or the exact prior immutable
    deployable binary is restored cleanly; the active binary retains no N/N-1 TSJS
    routes, runtime selector, percentage router, or dual protocol.
17. The emergency APS-disable path stops admission and returns local `404 no-store`
    for both reserved APS routes; binary rollback is never represented as restoring
    mutable upstream runner bytes.
18. No analytics, persistence, billing, experimentation, deployment-routing, or new
    external observability requirement was introduced.
19. The canonical 14-critical/20-total catalog emits one content-verified critical
    request, protects every attempt created within the ten-second startup window
    through terminal settlement and paint, then starts all included deferred modules
    independently in the same runtime. Exact absolute URL, nonce, CSP, Trusted Types,
    stale-hash, isolated failure, and head-of-line tests pass without vendoring GPT,
    APS runner, PUC, or other upstream bytes.
