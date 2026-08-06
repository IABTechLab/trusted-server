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

**Architecture:** one composition root constructs a core runtime from injected
adapters and services. Integration IIFEs register as release-matched integration
modules, prepare inertly, and activate together behind one synchronous commit
barrier. Rust emits one bounded tagged render-source union and one exact per-slot
auction decision set. Universal Creative 1.17.2 supplies the outer response and
owner-registration channels; the kernel owns control and APS document channels.
Direct and PUC paths settle through the same terminal state machine.

**Tech Stack:** Rust (`error-stack`, `http`, `serde`), lockfile TypeScript, Vitest,
Playwright, GPT/Prebid test adapters, Viceroy, and the existing four runtime
adapters.

---

**Source of truth:**
`docs/superpowers/specs/2026-08-04-aps-render-fix-and-tsjs-resilience-design.md`
revision 27, frozen review SHA
`6ed7fd4bafa31fe3a8112ad03ae5c600954d7568e6fef7ceabea5c9f8f94ab69`. This is the
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

Every task ends with `git status --short`, focused verification, and one intentional
commit before the next task. Stage only the exact paths from that task's **Files**
list that the implementation changed; never use broad staging in a dirty worktree.
Use the task title as the commit subject, normalized to the repository's conventional
`test:`, `feat:`, `refactor:`, or `chore:` prefix. Task 19's coordinated production
switch is one atomic commit; do not split it into deployable half-states.

## Planned source shape

The exact split may be adjusted during implementation only when it preserves these
owners and dependency directions.

```text
crates/trusted-server-js/lib/src/
  kernel/
    identity.ts         navigation-prefix + u64 attempts; 128-bit CSPRNG tickets/nonces
    disposable.ts       owned disposer stack and terminal latch primitives
    integration_registry.ts  release-matched prepare/activate transaction
    runtime.ts          bootstrap ownership and shared Runtime object
    sessions.ts         RuntimeSession and NavigationSession
  adapters/
    googletag.ts        only GPT-global access
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
    gpt/index.ts        thin GPT composition
    prebid/index.ts     thin Prebid composition
  core/
    index.ts            final public API installation
    request.ts          input validation and AuctionBatch entry point
    types.ts            public and wire types
  composition/
    browser.ts          sole construction root for concrete adapters and services
```

Test files mirror source ownership under `crates/trusted-server-js/lib/test/`.
Do not make `kernel/` depend on `adapters/`, `services/`, or `integrations/`.
Only `composition/` may import every layer and construct concrete dependencies.

## Dependency order

```text
descriptor contract
  -> server admission/mediation/projection
  -> renderer endpoint

kernel primitives
  -> sessions + adapters
  -> slot/reservation services
  -> lifecycle + auction batch
  -> GPT/Prebid/direct/fallback migration

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
- Task 8 computes and exposes release metadata but does not emit a required-integration
  manifest or claim production bootstrap ownership;
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
      the `rcjuly-tsjs-manifest-v1` JSON block from the revision-27 spec, enumerate
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
      pre-change baseline. Task 7A performs the intentional package and TypeScript
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
      minimal/reference/maximal gzip and Brotli bytes, Node/npm/TypeScript versions,
      Chromium version, CI machine class, fixture, five warmups, 50 samples, p90
      boot-to-first-display, and forced-GC CDP heap checkpoints. Commit these values to
      `aps-tsjs-prechange.json`; later tasks may compare against it but must not
      regenerate it from the completed implementation.

  Extend `scripts/integration-tests-browser.sh` with
  `TS_BROWSER_FRAMEWORKS=nextjs` and use `npm --prefix ... exec -- playwright` for
  argument-safe invocation. The script remains the clean-checkout fixture builder:
  it builds release Fastly WASM with the integration environment, generates
  Viceroy configuration, builds/loads the framework image, installs browser
  dependencies, and builds both TSJS fixture variants. Run the new performance
  test itself—not only the bundle script—and write its 50-sample/heap output to the
  exact baseline path:

  ```bash
  TS_BROWSER_FRAMEWORKS=nextjs \
  TSJS_PERF_MODE=baseline \
  TSJS_PERF_OUTPUT=crates/trusted-server-js/lib/test/fixtures/performance/aps-tsjs-prechange.json \
    ./scripts/integration-tests-browser.sh \
      tests/shared/tsjs-performance.spec.ts --project=chromium
  node crates/trusted-server-js/lib/scripts/check-bundle-budgets.mjs --baseline-only
  ```

  The integration workflow runs the same focused command on its pinned CI image and
  uploads the resulting JSON. Add required manual input `evidence_id` and include it
  in `run-name`. `dispatch-workflow-run.mjs` validates that the ref is a pushed
  branch/tag, dispatches with a unique evidence id, polls for exactly that run, and
  prints its numeric run id. Record the successful id in the baseline JSON:

  ```bash
  TASK0_REF="$(git branch --show-current)"
  TASK0_SHA="$(git rev-parse HEAD)"
  test -n "$TASK0_REF"
  git fetch origin "$TASK0_REF"
  test "$TASK0_SHA" = "$(git rev-parse "origin/$TASK0_REF")"
  TASK0_EVIDENCE_ID="aps-tsjs-baseline-$TASK0_SHA"
  TASK0_RUN_ID="$(node scripts/dispatch-workflow-run.mjs \
    integration-tests.yml "$TASK0_REF" \
    evidence_id="$TASK0_EVIDENCE_ID")"
  gh run watch "$TASK0_RUN_ID" --exit-status
  test "$TASK0_SHA" = "$(gh run view "$TASK0_RUN_ID" --json headSha --jq .headSha)"
  ```

- [ ] **Step 5: Run:**

  ```bash
  npm --prefix crates/trusted-server-js/lib run typecheck
  npm --prefix crates/trusted-server-js/lib run lint
  node --test crates/trusted-server-js/lib/test/contract/rc-july-adoption.test.mjs
  test -s crates/trusted-server-js/lib/test/fixtures/performance/aps-tsjs-prechange.json
  ```

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
      and `cache` members. Ensure APS has only `ApsRendererV1`; delete alternate APS
      `adm`, `meta`, or debug reconstruction. Align upstream and descriptor bid ids to
      1–64 UTF-8 bytes with no NUL/control. Define shared
      `RENDER_DIMENSION_MIN = 1` and `RENDER_DIMENSION_MAX = 4096`; apply the same
      noninteger/nonpositive versus out-of-range distinction in Rust, TS, generated
      ES5, ADM/cache sources, programmatic sizes, and later DOM construction. No
      validator or adapter may clamp.

- [ ] **Step 5: Rerun the focused commands plus:**

  ```bash
  npm --prefix crates/trusted-server-js/lib run typecheck
  npm --prefix crates/trusted-server-js/lib run lint
  npm --prefix crates/trusted-server-js/lib run check:aps-contract
  node --test crates/trusted-server-js/lib/test/contract/aps-renderer-es5.test.mjs
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

- [ ] **Step 5: Define and test the exact `/auction` winner wire. Standard `bid.id` is the**
      `r1_` renderer reservation; `bid.impid` maps exactly to the server slot; and
      `bid.ext.trusted_server` has only `candidate_id`, `slot_id`, and
      `render_source`. Require the four-way decision/candidate/impid/slot join. Reject
      missing, duplicate, extra, or mismatched bids; APS/cache standard `adm`; and ADM
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
      preserve the exact `BidRenderSourceV1`, `candidateId`, and server-minted renderer
      reservation plus the finite nonnegative selected CPM that later becomes the
      internal `WinnerContext`. Browser projection exposes `rendererReservationId`;
      `/auction` uses the same value as standard OpenRTB `bid.id`. CPM is never copied
      into a render descriptor or capability.

- [ ] **Step 2: Add failing browser tests proving every TS-owned APS, ADM, and cache PUC source**
      uses the `r1_` reservation byte-for-byte as GAM `hb_adid`. For Trusted Server
      Prebid, replace the generated TS bid `adId` with that same reservation before
      targeting; keep native Prebid `adId` untouched. Preserve PBS Cache UUID only as
      `renderSource.cacheId` and the exact fetch query binding, never as bridge
      authority. Add negative tests for truncation, fallback to upstream/cache ids, and
      native-bid mutation.

- [ ] **Step 3: Add Rust generation tests for `r1_` plus 22 unpadded base64url characters from**
      16 CSPRNG bytes, response-local uniqueness, eight collision retries, and
      `identity_generation_failed`. Make projection choose identity from a tagged
      enum/path decision, not an `or_else` chain. Reject invalid targeting before
      serialization; never truncate.

- [ ] **Step 4: Add the immutable `CacheFetchPolicyV1` boot contract. When cache rendering is**
      enabled, project the trusted configured HTTPS base URL at
      `tsjs.boot.cachePolicy`, validate/freeze it before integration-module preparation, and build
      `fetchUrl` server-side with exactly one canonical `uuid` query. Test credentials,
      query, fragment, origin/port/path mismatches, duplicate query keys, missing policy,
      and configuration mutation after the navigation snapshot. This is cache render
      correctness only; do not add a cache subsystem or runner caching.

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

### Task 5: Serve the static renderer and live APS runner proxy with adapter parity

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

- [ ] **Step 1: Write failing route and exact renderer-policy tests.**

  Cover enabled `GET /integrations/aps/renderer/v1` and
  `GET /integrations/aps/runner.js`; APS-disabled local `404 no-store`; local
  negative `404 no-store` for `/integrations/aps/runner/v1.js`, unknown renderer
  versions, and malformed family paths; `405` plus `Allow: GET`; and proof that no
  reserved path reaches publisher auth, EC, or fallback. Assert renderer body bytes,
  the exact ordered sandbox tokens, the exact CSP from spec §3.6, exact content type,
  immutable cache policy, `nosniff`, and referrer policy. Assert the deliberate
  absence of `X-Frame-Options` and CSP `frame-ancestors`.

- [ ] **Step 2: Run the new focused tests and prove they fail.**

  ```bash
  cargo test-fastly integrations::aps
  cargo test-axum --test routes
  cargo test-cloudflare --test routes
  cargo test-spin --test routes
  ```

  Expected: the live runner route/raw proxy policy and exact renderer headers are not
  yet implemented on every adapter.

- [ ] **Step 3: Define the bounded raw-proxy platform contract.**

  Add a dedicated request/response policy in `platform/http.rs` and adapter
  implementations that:
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

  Cloudflare must preserve `web_sys::Request.method()` before workers-rs conversion
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

- [ ] **Step 4: Write and pass the complete actual-adapter proxy corpus.**

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

- [ ] **Step 5: Implement the reserved dispatcher and live proxy response.**

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

- [ ] **Step 6: Implement and test the static renderer contract.**

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

- [ ] **Step 7: Add the hermetic fictional runner fixture.**

  Author a minimal local fixture that implements only the documented event and
  queue/resolve/reject behavior. Assert it is neither a copy, transformation, nor
  derivative of APS bytes. Use it for deterministic success, rejection, script-load,
  callback-silence, nested-iframe, and duplicate-callback tests. The fixture is not
  served as a production fallback and cannot be included in release bundles.

- [ ] **Step 8: Run the full route, transport, parity, and browser checks.**

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

- [ ] **Step 9: Commit the transport and renderer slice.**

  ```bash
  git add \
    crates/trusted-server-core/src/integrations/aps.rs \
    crates/trusted-server-core/src/integrations/registry.rs \
    crates/trusted-server-core/src/platform/http.rs \
    crates/trusted-server-core/src/platform/test_support.rs \
    crates/trusted-server-core/src/platform/types.rs \
    crates/trusted-server-adapter-fastly/src/app.rs \
    crates/trusted-server-adapter-fastly/src/platform.rs \
    crates/trusted-server-adapter-fastly/Cargo.toml \
    crates/trusted-server-adapter-axum/src/app.rs \
    crates/trusted-server-adapter-axum/src/platform.rs \
    crates/trusted-server-adapter-axum/tests/routes.rs \
    crates/trusted-server-adapter-cloudflare/src/app.rs \
    crates/trusted-server-adapter-cloudflare/src/platform.rs \
    crates/trusted-server-adapter-cloudflare/Cargo.toml \
    crates/trusted-server-adapter-cloudflare/tests/routes.rs \
    crates/trusted-server-adapter-cloudflare/wrangler.aps-runner-proxy.toml \
    crates/trusted-server-adapter-spin/src/app.rs \
    crates/trusted-server-adapter-spin/src/platform.rs \
    crates/trusted-server-adapter-spin/Cargo.toml \
    crates/trusted-server-adapter-spin/tests/routes.rs \
    crates/trusted-server-integration-tests/Cargo.toml \
    crates/trusted-server-integration-tests/fixtures/configs/spin-aps-runner-proxy.toml \
    crates/trusted-server-integration-tests/fixtures/configs/viceroy-aps-runner-proxy-template.toml \
    crates/trusted-server-integration-tests/tests/aps_runner_proxy.rs \
    crates/trusted-server-integration-tests/tests/common/aps_runner_upstream.rs \
    crates/trusted-server-integration-tests/tests/common/mod.rs \
    crates/trusted-server-integration-tests/tests/environments/spin.rs \
    crates/trusted-server-integration-tests/tests/environments/mod.rs \
    crates/trusted-server-integration-tests/tests/environments/cloudflare.rs \
    crates/trusted-server-integration-tests/tests/environments/fastly.rs \
    crates/trusted-server-integration-tests/tests/parity.rs \
    crates/trusted-server-integration-tests/browser/tests/shared/aps-renderer.spec.ts \
    crates/trusted-server-integration-tests/browser/fixtures/fictional-aps-runner.js \
    scripts/integration-tests-aps-runner-proxy.sh \
    scripts/integration-tests-browser.sh \
    .github/workflows/integration-tests.yml
  git commit -m "feat(aps): proxy the live creative runner safely"
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
      composition root that alone imports concrete adapters/services. Kernel files
      accept interfaces and never construct downstream objects. At this task's end
      production remains behaviorally unchanged, while new kernel/service files cannot
      touch GPT or Prebid globals.

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
      capacity 16, prepare throw/async rejection/abort, detached continuation,
      activation throw, duplicate `afterCommit`, shared deadline abort, and late
      continuation.

- [ ] **Step 2: Implement `DisposableStack` without relying on a browser proposal unavailable at**
      the configured target. It must be synchronous at ownership boundaries; async side
      work can observe its signal but cannot delay terminal disposal.

- [ ] **Step 3: Implement the exact `BootManifestV1` contract and release-internal**
      `_registerIntegration({id,release,prepare})` collection. All manifest entries are
      required. Registration executes no module code. In manifest order, core awaits
      only each returned preparation Promise; preparation may validate frozen config,
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

### Task 7A: Upgrade the TSJS package and TypeScript toolchain

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
  git commit -m "chore(tsjs): upgrade the package and TypeScript toolchain"
  ```

  Add only compatibility files that actually changed to the explicit staging list;
  do not use broad staging.

### Task 8: Implement bootstrap ownership and the single runtime registry, dormant

**Files:**

- Create: `crates/trusted-server-js/lib/src/kernel/runtime.ts`
- Create: `crates/trusted-server-js/lib/test/kernel/runtime.test.ts`
- Modify: `crates/trusted-server-js/lib/src/composition/browser.ts`
- Modify: `crates/trusted-server-js/lib/test/composition/browser.test.ts`
- Modify: `crates/trusted-server-js/lib/src/core/index.ts`
- Modify: `crates/trusted-server-js/lib/src/core/queue.ts`
- Modify: `crates/trusted-server-js/lib/src/core/log.ts`
- Modify: `crates/trusted-server-js/lib/src/core/global.d.ts`
- Create: `crates/trusted-server-js/lib/test/core/queue.test.ts`
- Create: `crates/trusted-server-js/lib/test/core/log.test.ts`
- Create: `crates/trusted-server-js/lib/src/integrations/gpt/bootstrap_fallback.ts`
- Modify: `crates/trusted-server-js/lib/test/integrations/gpt/gpt_bootstrap.test.ts`
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
      listener/timer/port/iframe construction after the fallback commits.

  Exercise the queue boundary as a real Array: pushes before/during/at activation and
  commit, retained ingress references, snapshot-versus-forward exactly once, nested
  push ordering, `this === tsjs`, throw isolation, and non-callables. The final queue
  has `length:0`, its own immediate `push`, and is frozen; native/borrowed mutators,
  index/length assignment, deletion, and property definition in strict/sloppy callers
  cannot retain work or change length.

- [ ] **Step 2: Implement `unclaimed → installing → kernel` and**
      `installing → failed → fallback`. Start the only ten-second watchdog immediately
      before core injection; it covers registration, preparation, and activation for
      every required integration. Combine the timer with `performance.now()`/the
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
  `{version:1,auction:{version:1,auctionId:'fallback',results:[]},bids:[]}` plus
  creative/diagnostics disabled safe defaults. Known members resolve with the boot failure, unknown ids
  resolve `slot_unresolved`, aborted known members cancel, and an omitted empty
  projection resolves `slots:[]`. No valid call remains pending.

- [ ] **Step 3: Make `build-all.mjs` emit all bundles with one fixed sentinel, compute 64**
      lowercase SHA-256 hex over the canonical ordered ids plus sentinel-normalized
      bytes, replace exactly one sentinel per bundle, and verify none remains. Embed
      that release id in every bundle. Write exact generated
      `crates/trusted-server-js/dist/tsjs-release-v1.json` with
      `{version:1,releaseId,bundles:[{id,file}]}` in canonical bundle order; add
      `npm run --silent print:release-id` to validate that file and print only its
      64-hex id. Extend `build.rs` generated metadata to include
      the sentinel-normalized all-bundle `release_id`; expose it through `bundle.rs` and
      the crate API beside bundle bytes/content hashes. Add Rust tests proving generated
      metadata and every bundle carry the same id. Implement the pure
      `BootManifestV1` serializer with exactly the enabled unique integration ids in actual
      injection order and `required:true`, but do not emit it into production HTML yet.
      Test changed logical bytes, reordered integrations, sentinel multiplicity, wrong
      release, missing integration, and server/bundle disagreement.

- [ ] **Step 4: Install one `Runtime`; the composition root keeps the mutable service**
      registry and owner tokens in its closure, and integrations obtain frozen
      interfaces only through exact-release preparation/activation contexts.
      `tsjs._internal` is non-enumerable frozen status data only—never the registry.

  In test-only composition, activate the capture-phase bridge recognizer as the first
  reversible core effect, followed by correctness GPT listeners and prepared modules.
  Production bootstrap and manifest emission remain unchanged until Task 19.

- [ ] **Step 5: Generate the proposed queue/boot-flags fallback artifact from**
      `bootstrap_fallback.ts` and test its bytes and terminal behavior at every
      checkpoint. Task 19 performs the one production replacement and adds the final
      embedded staleness assertion; there is never a hand-maintained second fallback
      implementation.

- [ ] **Step 6: Run:**

  ```bash
  npm --prefix crates/trusted-server-js/lib test -- --run test/kernel/runtime.test.ts test/integrations/gpt/gpt_bootstrap.test.ts
  cargo test-fastly release_id
  cargo test-fastly
  npm --prefix crates/trusted-server-js/lib run build
  ```

### Task 9: Add runtime and navigation sessions

**Files:**

- Create: `crates/trusted-server-js/lib/src/kernel/sessions.ts`
- Create: `crates/trusted-server-js/lib/src/kernel/identity.ts`
- Create: `crates/trusted-server-js/lib/test/kernel/sessions.test.ts`
- Create: `crates/trusted-server-js/lib/test/kernel/identity.test.ts`
- Create: `crates/trusted-server-js/lib/src/services/projections.ts`
- Create: `crates/trusted-server-js/lib/test/services/projections.test.ts`
- Create: `crates/trusted-server-js/lib/src/services/context.ts`
- Create: `crates/trusted-server-js/lib/test/services/context.test.ts`
- Modify: `crates/trusted-server-js/lib/src/kernel/runtime.ts`
- Modify: `crates/trusted-server-js/lib/src/composition/browser.ts`
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
  services constructed in `composition/browser.ts` and disposed by the runtime
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

  Keep the issuer injectable only through test composition. Production composition
  always uses browser Web Crypto; no `Math.random`, counter, timestamp, publisher
  input, or compatibility-form parser may mint a capability.

- [ ] **Step 4: Run:**

  ```bash
  npm --prefix crates/trusted-server-js/lib test -- --run test/kernel/sessions.test.ts
  npm --prefix crates/trusted-server-js/lib test -- --run test/kernel/identity.test.ts
  npm --prefix crates/trusted-server-js/lib test -- --run test/services/projections.test.ts
  npm --prefix crates/trusted-server-js/lib test -- --run test/services/context.test.ts
  npm --prefix crates/trusted-server-js/lib run typecheck
  ```

### Task 10: Implement bounded adapters and readiness queues

**Files:**

- Modify: `crates/trusted-server-js/lib/src/adapters/googletag.ts`
- Modify: `crates/trusted-server-js/lib/src/adapters/prebid.ts`
- Create: `crates/trusted-server-js/lib/test/adapters/googletag.test.ts`
- Create: `crates/trusted-server-js/lib/test/adapters/prebid.test.ts`
- Modify: `crates/trusted-server-js/lib/src/adapters/messaging.ts`
- Create: `crates/trusted-server-js/lib/test/adapters/messaging.test.ts`
- Modify: `crates/trusted-server-js/lib/src/composition/browser.ts`
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
- Modify: `crates/trusted-server-js/lib/src/composition/browser.ts`
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

### Task 12: Implement the bounded renderer reservation store

**Files:**

- Create: `crates/trusted-server-js/lib/src/services/reservations.ts`
- Create: `crates/trusted-server-js/lib/test/services/reservations.test.ts`
- Modify: `crates/trusted-server-js/lib/src/kernel/sessions.ts`
- Modify: `crates/trusted-server-js/lib/src/integrations/aps/render.ts`
- Modify: `crates/trusted-server-js/lib/src/composition/browser.ts`
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

- [ ] **Step 3: Use one reservation type for every TS-owned APS, ADM, and cache PUC source.**
      Validate the supplied server id but never generate it in the browser. Cache UUID
      remains only `cacheId` transport state; upstream bid id remains provenance;
      native Prebid ids never enter this store. Reject an id collision against any
      live/tombstoned entry; lookup recognizes a TS id before detailed validation.

- [ ] **Step 4: Run:**

  ```bash
  npm --prefix crates/trusted-server-js/lib test -- --run test/services/reservations.test.ts test/integrations/aps/render.test.ts
  ```

### Task 13: Implement the RenderAttempt state machine and direct paths

**Files:**

- Create: `crates/trusted-server-js/lib/src/services/render.ts`
- Create: `crates/trusted-server-js/lib/test/services/render.test.ts`
- Modify: `crates/trusted-server-js/lib/src/integrations/aps/render.ts`
- Modify: `crates/trusted-server-js/lib/src/core/render.ts`
- Modify: `crates/trusted-server-js/lib/src/composition/browser.ts`
- Modify: `crates/trusted-server-js/lib/test/composition/browser.test.ts`

- [ ] **Step 1: Add failing state-table tests for every valid transition and every invalid/replay**
      transition. Race success, failure, timeout, caller abort, supersession, and
      navigation disposal through one terminal latch. Add accepted-artifact promotion
      races and prove terminal attempt disposal cannot remove a committed render. At
      construction assert one exact navigation-unique `a1_` attempt id; fallback child
      ids are distinct and bind their exact parent id. Test navigation-prefix failure,
      ordinal exhaustion, disposal, and that neither ids nor issuer bytes reach logs.

  Add renderer-nonce live-registry tests at 255/256/257 entries, eight collision
  draws, crypto failure, exact source/port/attempt/generation binding, disposal reuse,
  and no tombstone/history set. Capacity maps `capability_registry_full`; exhausted
  draws map `identity_generation_failed`.

- [ ] **Step 2: Implement path-independent `RenderAttempt` ownership: state, exact slot,**
      generation, exact `a1_` id, optional parent attempt id, tagged render source,
      immutable `WinnerContext{selectedCpm}`, timers, ports, iframe, terminal result,
      and disposer. Direct winner admission constructs the context from the exact
      validated joined server winner; a PUC claim receives the same context from the
      consumed reservation.
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

- [ ] **Step 3: Implement direct APS:**
  - validate descriptor before DOM mutation;
  - mint one exact `n1_` nonce from the 16-byte Web Crypto issuer and create the
    inner channel immediately before insertion; bind the nonce to the exact attempt,
    generation, renderer `contentWindow`, and retained port;
  - put the nonce in the fragment and transfer an envelope containing the
    kernel-captured publisher origin;
  - bind to the exact iframe `contentWindow` and transferred port;
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

- [ ] **Step 4: Implement direct ADM through one shared constructor used later by the PUC owner.**
      Use the exact ordered sandbox, no-referrer policy, integral 1–4096 source dimensions,
      CSS sizing, zero border/margin, hidden overflow, display, scrolling, title, and
      aria attributes from spec §4.5. Create the iframe detached, install one-shot
      handlers/disposal first, assign exactly one complete `srcdoc`, and append once;
      never append empty or set `src`. Accept only the exact current pending frame's
      intended `srcdoc` load while its generation/latch remain current. Initial
      `about:blank`, pre-assignment, removed/replaced frame, stale generation,
      post-disposal, duplicate load, error, and five-second timeout cannot accept and
      map to `adm_document_no_load`.

  Implement cache as a preceding bounded fetch. Require a frozen valid
  `CacheFetchPolicyV1`; require `baseUrl`/`fetchUrl` at the 4,096-byte boundary and
  the server-built HTTPS URL to have the exact base origin/port/path and exactly
  one canonical `uuid` query equal to `cacheId`; use `redirect:'error'`, omit
  credentials/referrer, require CORS and a
  successful status, and enforce five seconds and 512 KiB. Parse only a JSON object
  with required own bounded nonempty `adm`; optional `w`/`h` must appear together,
  stay in 1–4096, and match; optional finite nonnegative `price` is ignored; unknown OpenRTB bid
  keys are ignored; raw bodies, aliases, arrays/primitives, and wrappers fail.
  Expand only the exact `${AUCTION_PRICE}` token as
  `String(attempt.winnerContext.selectedCpm)`; leave `${AUCTION_PRICE:B64}` untouched
  and never read response `price`, current projection, targeting, or a later winner.
  Test delayed PUC cache after projection replacement, Prebid lease promotion, and a
  separate direct-cache context, plus URL/query/redirect/body/shape/macro cases, all
  three typed cache failures, and proof none becomes `no_bid`.

- [ ] **Step 5: Keep all remote side effects outside terminal correctness. APS has no synthetic**
      notification.

- [ ] **Step 6: Run:**

  ```bash
  npm --prefix crates/trusted-server-js/lib test -- --run test/services/render.test.ts test/core/render.test.ts test/integrations/aps/render.test.ts
  ```

### Task 14: Implement Universal Creative claim and owner-control channels

**Files:**

- Modify: `crates/trusted-server-js/lib/src/services/render.ts`
- Modify: `crates/trusted-server-js/lib/src/services/reservations.ts`
- Modify: `crates/trusted-server-js/lib/src/adapters/messaging.ts`
- Create: `crates/trusted-server-js/lib/src/services/puc_bridge.ts`
- Create: `crates/trusted-server-js/lib/test/services/puc_bridge.test.ts`
- Modify: `crates/trusted-server-js/lib/src/integrations/aps/render.ts`
- Modify: `crates/trusted-server-js/lib/test/integrations/aps/render.test.ts`
- Modify: `crates/trusted-server-js/lib/test/integrations/gpt/ad_init.test.ts`
- Modify: `crates/trusted-server-js/lib/src/composition/browser.ts`
- Modify: `crates/trusted-server-js/lib/test/composition/browser.test.ts`
- Create: `crates/trusted-server-integration-tests/browser/fixtures/prebid-universal-creative-1.17.2.js`
- Create: `crates/trusted-server-integration-tests/browser/fixtures/prebid-universal-creative-1.17.2.sha256`

- [ ] **Step 1: Vendor the exact supported PUC 1.17.2 artifact and checksum for hermetic tests;**
      the GAM template pins the same version and never `latest`. Add failing tests for
      the exact JSON string
      `{message:"Prebid Request",adId,adServerDomain}`, object/extended shapes,
      zero/two ports, native id, live/tombstoned TS id, duplicate simultaneous claim,
      replay, prior navigation, SafeFrame-shaped nesting, outer post failure, and every
      ordering of early claim, nonempty/empty GAM, navigation, supersession, claim
      deadline, and GPT-cycle deadline. Include caller abort before/after registration,
      insertion, and document acceptance; lost/closed control channel before start and
      after insertion; settlement-post throw; remote cleanup at 19,999/20,000/20,001
      ms; and proof accepted remote DOM remains while every uncommitted failure removes
      exactly its owned iframe and settles the PUC Promise once.

- [ ] **Step 2: Install exactly one capture-phase bridge dispatcher synchronously as the first**
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

- [ ] **Step 3: Implement the two-condition join. An early claim buffers only source plus outer**
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

- [ ] **Step 4: In the hidden dynamic renderer call PUC's supplied**
      `h.sendMessage('TS Render Owner Register',{version:1,lifecycleTicket},callback)`;
      never global `postMessage`. Assert the kernel sees the original captured PUC
      source, exact auto-added `adId/message` keys, and one helper-created response
      port. Test exact registered/refused responses, ticket TTL/atomic consumption,
      wrong source, stale generation, replay, zero/two response ports, the owner's
      three-second watchdog, helper disposer, and late response.

  Receive registration through the same dispatcher. Minimally lookup the ticket
  map first; ignore unknown tickets, but suppress live/tombstoned TS tickets before
  exact source/adId/attempt/generation/shape/one-port checks. Refuse and close
  recognized invalid/replayed registration, keep ticket tombstones through their
  original TTL, and prove attempt disposal removes ticket state without removing
  the runtime dispatcher. Atomically consume the first valid use, invalidate on
  timeout/failure/supersession/navigation/disposal, make duplicate/stale/late uses
  inert, and prove ticket values and issuer bytes never reach logs.

- [ ] **Step 5: On registration the kernel creates the owner-control channel, keeps one endpoint,**
      and transfers exactly one endpoint in `TS Render Owner Registered`. For APS it
      then sends exact `TS APS Start` with the descriptor/envelope plus exactly one
      renderer-document port. For ADM it sends exact `TS ADM Start` and no port. Add
      exact-shape tests for every start, insertion, document progress, render
      completion/failure, ADM load/failure, and final owner settlement message and for
      every wrong/extra key or port count. `OwnerSettlementV1` cancellation includes
      exactly `caller_aborted | superseded | navigation_disposed`; every terminal
      RenderOutcome is therefore encodable after registration.

- [ ] **Step 6: For APS, make the owner create exactly one iframe using the immutable renderer**
      sandbox constant, meet the one-second insertion deadline, and leave document and
      completion timing to the kernel anchors from Task 13. For ADM/cache, call the
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

- [ ] **Step 7: Run the shared protocol corpus through both global and port parsers. Enforce the**
      4,096-byte inbound JSON cap before parse; exact 25-character capabilities;
      field-specific 256/2,048/4,096-byte limits; safe generations; 64 KiB dynamic
      owner and 72 KiB successful outer-response limits; exact keys/prototypes; and
      boundary-minus-one/boundary/boundary-plus-one multibyte, duplicate-key, malformed
      encoding, accessor, and exact 1/4096 dimension cases.

- [ ] **Step 8: Run:**

  ```bash
  npm --prefix crates/trusted-server-js/lib test -- --run test/services/puc_bridge.test.ts test/services/render.test.ts test/integrations/gpt/ad_init.test.ts
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
- Modify: `crates/trusted-server-js/lib/src/composition/browser.ts`
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
- Modify: `crates/trusted-server-js/lib/src/composition/browser.ts`
- Modify: `crates/trusted-server-js/lib/test/composition/browser.test.ts`

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

- [ ] **Step 4: Implement the owner-and-value targeting journal in `services/targeting.ts`.**
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

- [ ] **Step 5: Fold integration-specific script-guard mechanics onto the shared factory while**
      keeping GPT configuration in its integration. Implement one runtime-owned
      `MutationObserver` per `NavigationSession`, 250 ms debounce, 5,000 ms monotonic
      window, one final boundary pass, the two-success cap, exact physical-object
      quarantine, and complete timer/candidate/reference disposal. Successful handoff
      cancels reconciliation and transfers cleanup ownership synchronously.

- [ ] **Step 6: Run the entire GPT suite, not only new files:**

  ```bash
  npm --prefix crates/trusted-server-js/lib test -- --run test/integrations/gpt
  npm --prefix crates/trusted-server-js/lib test -- --run test/adapters/googletag.test.ts test/services/slots.test.ts test/services/targeting.test.ts
  npm --prefix crates/trusted-server-js/lib run lint
  npm --prefix crates/trusted-server-js/lib run typecheck
  ```

### Task 17: Prepare Prebid and APS registration on the shared runtime

**Files:**

- Modify: `crates/trusted-server-js/lib/src/integrations/prebid/index.ts`
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
- Modify: `crates/trusted-server-js/lib/src/composition/browser.ts`
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
      reservation service for APS, ADM, and cache PUC entries. The adapter binds one
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

- [ ] **Step 5: Make the external artifact independently correct and pure.** Build exactly
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

- [ ] **Step 6: Run:**

  ```bash
  npm --prefix crates/trusted-server-js/lib test -- --run test/integrations/prebid test/integrations/aps
  npm --prefix crates/trusted-server-js/lib run build:prebid-external
  npm --prefix crates/trusted-server-js/lib test -- --run test/prebid-artifact-integration.test.mjs
  ```

### Task 18: Prepare creative, diagnostics, and remaining integration modules

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
- Modify: `crates/trusted-server-js/lib/test/core/trace.test.ts`
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
- Modify: `crates/trusted-server-js/lib/src/composition/browser.ts`
- Modify: `crates/trusted-server-js/lib/test/composition/browser.test.ts`

- [ ] **Step 1: Add a maximal-bundle failing smoke test that loads core followed by every**
      server-declared integration in manifest order and asserts one runtime, no unknown
      integration id, no duplicate activation, exact reverse-order disposal, and no
      leaked timer/listener/wrapper/observer after disposal. Run every module alone
      and in the maximal manifest with missing globals, readiness/timeouts, malformed
      config/consent/storage, matcher false positives, callback throws, startup
      failure, and cross-integration isolation.

- [ ] **Step 2: Convert every remaining capability into a thin integration module.** Each
      `_registerIntegration({id,release,prepare})` call is pure registration;
      `prepare(ctx)` is inert and Promise-returning; the returned `activate(ctx)` is
      synchronous, registers a disposer before each reversible mutation, and uses at
      most one staged `afterCommit` callback for irreversible work. Exercise all
      modules through the same manifest-ordered test composition. Preserve existing
      feature behavior and integration-owned matchers/configuration; shared helpers
      must not broaden matching, reorder startup, stack interception, or retain work
      after disposal. Do not change shipped entry-point side effects until Task 19.

- [ ] **Step 3: Rebuild creative startup around the exact frozen `CreativeBootV1`.** Validate
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
  rejection of credentials, malformed values, and non-network schemes. Delete the
  mutable/install creative globals only in Task 22.

- [ ] **Step 4: Move render tracing to the kernel diagnostics bus and exact public surface.**
      `tsjs.diagnostics.renderTrace` exposes only frozen `current()`, `history()`, and
      `subscribe()`. Keep current state keyed by exact slot and capped by the 256-slot
      navigation registry; prune on disposal. Keep document-runtime history at 200,
      one row per physical impression, monotonic `count`/global `seq`, immutable `at`,
      and non-weakening enrichment. Remove stale DOM stamp fields/badges on update and
      preserve bounded overlay/export failure isolation.

  Commit correctness state before public delivery. Capture subscriber ids and enqueue
  frozen full records asynchronously in a 200-entry FIFO keyed by `seq`; same-sequence
  enrichment replaces the pending record and captured ids without reordering. One
  owned zero-delay task drains FIFO. Enforce callable-before-capacity validation,
  32-live-subscriber cap, idempotent unsubscribe, unsubscribe-before-delivery,
  registration-during-dispatch, callback throw isolation, and 199/200/201 overflow.
  Emit no `CustomEvent`, mutable trace global, or compatibility alias.

- [ ] **Step 5: Preserve GPT diagnostics through the adapter event stream.** Validate exact
      `DiagnosticsBootV1` plus manifest activation before any listener/buffer exists.
      When active, core owns the six documented GPT observations before TS requests,
      buffers 512 raw facts until module activation, then replays and releases the
      buffer. When inactive, require zero diagnostics-added listeners, DOM, timers,
      observers, API, storage, or network work beyond the two correctness listeners.
      Preserve exact physical-slot binding/replacement, per-slot monotonic request
      numbers, callback truth/timing, frozen exports, Shadow DOM overlay, badges, SPA,
      privacy, and non-interference.

  Bound the store to 64 slot objects, ten cycles per slot, and 128 callback issues.
  Expose only `tsjs.diagnostics.gpt`, with `snapshot()` plus the shared 32-subscriber
  limit. Public delivery uses a separate one-entry latest-snapshot notifier on one
  owned zero-delay task; 0/1/2-update coalescing, captured ids, unsubscribe/disposal,
  slow/throwing listeners, and callback-stack isolation are executable tests. No
  storage, upload, old flag, runtime expando, or `tsjs.gptDiagnostics` alias remains
  after Task 22.

- [ ] **Step 6: Preserve each remaining `rc/july` integration corpus exactly.** Cover DataDome
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

- [ ] **Step 7: Generate and test the prospective manifest member list/order from the exact**
      enabled bundle list. Embed the same release id in core and every integration
      IIFE. Add failures for integration before core, unknown/missing/duplicate member,
      malformed/unsorted/oversized manifest, wrong release, preparation or activation
      failure, duplicate `afterCommit`, and the 16-member/10-second transaction limits.
      Production manifest emission starts only in Task 19.

- [ ] **Step 8: Run:**

  ```bash
  npm --prefix crates/trusted-server-js/lib test
  npm --prefix crates/trusted-server-js/lib run build
  npm --prefix crates/trusted-server-js/lib run lint
  npm --prefix crates/trusted-server-js/lib run typecheck
  cargo test-fastly publisher
  ```

### Task 19: Complete lifecycle behavior and perform the coordinated production switch

**Files:**

- Modify: `crates/trusted-server-js/lib/src/services/render.ts`
- Modify: `crates/trusted-server-js/lib/src/services/slots.ts`
- Modify: `crates/trusted-server-js/lib/src/services/projections.ts`
- Modify: `crates/trusted-server-js/lib/src/services/targeting.ts`
- Modify: `crates/trusted-server-js/lib/src/services/reservations.ts`
- Modify: `crates/trusted-server-js/lib/src/services/auction_batch.ts`
- Modify: `crates/trusted-server-js/lib/src/services/context.ts`
- Modify: `crates/trusted-server-js/lib/src/kernel/integration_registry.ts`
- Modify: `crates/trusted-server-js/lib/src/kernel/runtime.ts`
- Modify: `crates/trusted-server-js/lib/src/kernel/sessions.ts`
- Modify: `crates/trusted-server-js/lib/src/adapters/googletag.ts`
- Modify: `crates/trusted-server-js/lib/src/adapters/prebid.ts`
- Modify: `crates/trusted-server-js/lib/src/adapters/messaging.ts`
- Modify: `crates/trusted-server-js/lib/src/core/config.ts`
- Modify: `crates/trusted-server-js/lib/src/core/global.d.ts`
- Modify: `crates/trusted-server-js/lib/src/core/log.ts`
- Modify: `crates/trusted-server-js/lib/src/core/queue.ts`
- Modify: `crates/trusted-server-js/lib/src/core/registry.ts`
- Modify: `crates/trusted-server-js/lib/src/core/trace.ts`
- Modify: `crates/trusted-server-js/lib/src/core/types.ts`
- Modify: `crates/trusted-server-js/lib/src/core/request.ts`
- Modify: `crates/trusted-server-js/lib/src/core/auction.ts`
- Modify: `crates/trusted-server-js/lib/src/core/index.ts`
- Modify: `crates/trusted-server-js/lib/src/composition/browser.ts`
- Modify: `crates/trusted-server-js/lib/test/composition/browser.test.ts`
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
- Modify: `crates/trusted-server-core/src/integrations/gpt_bootstrap.js`
- Modify: `crates/trusted-server-core/src/publisher.rs`
- Modify: `crates/trusted-server-core/src/tsjs.rs`
- Modify: `crates/trusted-server-core/src/auction/endpoints.rs`
- Modify: `crates/trusted-server-core/src/auction/formats.rs`
- Modify: `crates/trusted-server-core/src/integrations/registry.rs`
- Modify: `crates/trusted-server-adapter-fastly/src/app.rs`
- Modify: `crates/trusted-server-adapter-axum/src/app.rs`
- Modify: `crates/trusted-server-adapter-cloudflare/src/app.rs`
- Modify: `crates/trusted-server-adapter-spin/src/app.rs`
- Modify: `crates/trusted-server-integration-tests/tests/parity.rs`
- Modify: `crates/trusted-server-core/src/html_processor.rs`
- Modify: `crates/trusted-server-core/src/integrations/prebid.rs`
- Modify: `crates/trusted-server-core/src/integrations/didomi.rs`
- Modify: `crates/trusted-server-core/src/integrations/sourcepoint.rs`
- Modify: `crates/trusted-server-core/src/integrations/gpt.rs`
- Modify: `crates/trusted-server-core/src/integrations/gpt_diagnostics.rs`
- Modify: `crates/trusted-server-core/src/integrations/gpt_diagnostics_bootstrap.js`
- Modify: `crates/trusted-server-js/lib/build-prebid-external.mjs`
- Modify: `crates/trusted-server-js/lib/build-all.mjs`
- Modify: `crates/trusted-server-js/lib/test/core/index.test.ts`
- Modify: `crates/trusted-server-js/lib/test/core/request.test.ts`
- Modify: `crates/trusted-server-js/lib/test/core/auction.test.ts`
- Modify: `crates/trusted-server-js/lib/test/kernel/runtime.test.ts`
- Modify: `crates/trusted-server-js/lib/test/services/render.test.ts`
- Modify: `crates/trusted-server-js/lib/test/services/slots.test.ts`
- Modify: `crates/trusted-server-js/lib/test/services/projections.test.ts`
- Modify: `crates/trusted-server-js/lib/test/services/targeting.test.ts`
- Modify: `crates/trusted-server-js/lib/test/services/reservations.test.ts`
- Modify: `crates/trusted-server-js/lib/test/services/auction_batch.test.ts`
- Modify: `crates/trusted-server-js/lib/test/services/context.test.ts`
- Modify: `crates/trusted-server-js/lib/test/core/queue.test.ts`
- Modify: `crates/trusted-server-js/lib/test/core/registry.test.ts`
- Modify: `crates/trusted-server-js/lib/test/core/log.test.ts`
- Modify: `crates/trusted-server-js/lib/test/core/trace.test.ts`
- Modify: `crates/trusted-server-js/lib/test/adapters/googletag.test.ts`
- Modify: `crates/trusted-server-js/lib/test/adapters/prebid.test.ts`
- Modify: `crates/trusted-server-js/lib/test/adapters/messaging.test.ts`
- Modify: `crates/trusted-server-js/lib/test/integrations/gpt/gpt_bootstrap.test.ts`
- Modify: `crates/trusted-server-js/lib/test/integrations/gpt/ad_init.test.ts`
- Modify: `crates/trusted-server-js/lib/test/integrations/gpt/index.test.ts`
- Modify: `crates/trusted-server-js/lib/test/integrations/prebid/index.test.ts`
- Modify: `crates/trusted-server-js/lib/test/integrations/aps/render.test.ts`

- [ ] **Step 1: Add failing tests proving fallback begins only after an attributable TS-owned**
      empty GAM cycle; the primary child settles before fallback starts; publisher,
      ambiguous, quarantined, timeout, and stale cases do not fall back; both child
      histories remain immutable; and `SlotOperation` publishes exactly one final
      result with `path:'fallback'` when the child runs.

- [ ] **Step 2: Snapshot render-relevant configuration at attempt creation. Re-check generation**
      and existing kill-switch state immediately before the earliest irreversible
      action (bridge response, DOM insertion, or an existing non-APS notification).

- [ ] **Step 3: Preserve existing non-APS `nurl`/`burl` behavior but route it through the attempt**
      terminal transition so it initiates once and never blocks. Add an assertion that
      APS never synthesizes either URL.

- [ ] **Step 4: Test already-loaded-page limits honestly: configuration changes reach a page only**
      through an existing response path; do not add polling, push, or event ingestion.

- [ ] **Step 5: Atomically activate the new production surface in one task and one commit:**
  - `/auction` emits/parses only the exact decision-set/tagged-source wire, and
    initial HTML/page-bids emit only `tsjs.boot.auctionProjection`;
  - the immutable initial projection seeds the first `NavigationSession`; every SPA
    page-bids response validates and commits only to the replacement session's
    internal projection and never mutates recursively frozen `tsjs.boot`;
  - projection parsing enforces the exact 256-array/member, identifier, targeting,
    currency/CPM, reservation, dimension, and canonical 8 MiB bounds before mutation;
    an over-cap projection converts every otherwise winning decision to
    `winner_not_renderable`, emits no projected bid, and omits the corresponding
    `/auction` TS seatbid;
  - the server emits exact frozen `TsjsBootV1`, `CreativeBootV1`,
    `DiagnosticsBootV1`, and `BootManifestV1` before core from generated release
    metadata, after validating every integration config and manifest relationship;
  - core inertly prepares every required integration in manifest order while no
    bridge/listener/global mutation is live. Only after all Promises resolve does the
    same-task synchronous activation barrier install the capture bridge as its first
    reversible core effect, install correctness GPT listeners, and activate modules
    in order with monotonic pre/post-call and pre-handoff checks. Failure rolls back
    every reversible effect; success commits the complete `TsjsApi`, runs staged
    `afterCommit` callbacks in manifest order, and drains the preload queue;
  - the preload queue handoff uses the exact real-Array algorithm: capture ingress,
    install the fixed installing descriptor, snapshot, forward retained ingress
    pushes, install the frozen final actual Array with own immediate `push` and
    `length:0`, publish the complete API, run `afterCommit`, then drain snapshot plus
    forwarded work exactly once. Native/borrowed mutators and retained references
    cannot retain entries or create a second runtime;
  - the kernel surface is exactly `TsjsApi` with semantic `version`, exact
    `releaseId`, immutable `boot`, real `que`, `addAdUnits`, Promise `requestAds`,
    local `log`, diagnostics, `_registerIntegration`, and frozen status-only
    `_internal`. Fallback exposes its exact smaller own surface, validates then refuses
    `addAdUnits`, settles known slots with the committed fallback reason, drains the
    queue once, and creates no runtime/adapters/listeners/timers/DOM work;
  - `addAdUnits` transactionally validates and registers programmatic direct-auction
    slots against the same combined 256-slot cap, exact identifier/bidder/dimension
    grammar, and collision indexes. Omitted-slot `requestAds` snapshots server and
    programmatic registrations in ordinal order; later registrations cannot enter an
    in-flight snapshot;
  - GPT, Prebid, APS, creative, diagnostics, all remaining integrations, Promise
    `requestAds`, versioned APS renderer client, and generated bootstrap/fallback
    switch together on the shared sessions/services and terminal latches;
  - the external publisher artifact switches as independently useful pure Prebid.js
    10.26.0 with its own watchdog and frozen artifact stamp; TS admission, render,
    refresh, targeting, and release matching remain only in the separate Prebid
    integration module;
  - all adapters atomically register only the versioned static renderer and
    unversioned live `/integrations/aps/runner.js` proxy; the abandoned
    `/integrations/aps/runner/v1.js` and unversioned renderer are local negative
    routes;
  - every Rust/JS integration config emitter moves its existing values from
    scattered `window.__tsjs_*` globals into its exact `tsjs.boot.*` member before
    the corresponding integration prepares; no integration loses configuration;
  - accepted artifacts, `WinnerContext`, targeting journals, renderer reservations,
    GPT physical-object reconciliation, and navigation ownership use the shared
    services; and
  - render trace and GPT diagnostics commit only after correctness transitions and
    expose their exact bounded asynchronous frozen APIs. Creative guards auto-install
    from frozen boot configuration and both-false guards have zero DOM side effects.

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

- [ ] **Step 6: Run:**

  ```bash
  npm --prefix crates/trusted-server-js/lib test -- --run test/services test/core test/integrations/gpt
  npm --prefix crates/trusted-server-js/lib test -- --run test/integrations/prebid test/integrations/aps test/kernel
  npm --prefix crates/trusted-server-js/lib run build
  cargo test-fastly
  cargo test-axum
  cargo test-cloudflare
  cargo test-spin
  cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test parity
  ```

### Phase 4 exit

- GPT, Prebid, APS, and all integration entry points use one kernel/integration-module
  surface.
- The old registries, sentinels, expandos, refresh wrappers, and bridge branches are
  gone.
- All Vitest and production-bundle tests pass.

## Phase 5 — browser conformance, deletion, and release readiness

### Task 20: Build the hermetic browser race matrix

**Files:**

- Modify: `crates/trusted-server-integration-tests/browser/playwright.config.ts`
- Modify: `crates/trusted-server-integration-tests/browser/tests/shared/aps-renderer.spec.ts`
- Create: `crates/trusted-server-integration-tests/browser/tests/shared/aps-puc-lifecycle.spec.ts`
- Create: `crates/trusted-server-integration-tests/browser/tests/shared/tsjs-runtime.spec.ts`
- Modify: `crates/trusted-server-integration-tests/browser/tests/shared/creative-sandbox.spec.ts`
- Modify: `crates/trusted-server-integration-tests/browser/tests/nextjs/gpt-diagnostics.spec.ts`
- Modify: `crates/trusted-server-integration-tests/browser/tests/nextjs/navigation.spec.ts`
- Modify: `crates/trusted-server-integration-tests/browser/helpers/gpt-stub.ts`
- Modify: `crates/trusted-server-integration-tests/browser/helpers/infra.ts`
- Modify: `crates/trusted-server-integration-tests/browser/helpers/state.ts`
- Modify: `crates/trusted-server-integration-tests/browser/fixtures/fictional-aps-runner.js`
- Modify: `crates/trusted-server-integration-tests/browser/fixtures/prebid-universal-creative-1.17.2.js`
- Modify: `scripts/integration-tests-browser.sh`
- Modify: `.github/workflows/integration-tests.yml`

- [ ] **Step 1: Extend Playwright projects to Chromium, Firefox, and WebKit for the focused APS**
      conformance files. Keep the broader existing suite's browser matrix unchanged
      unless runtime permits expansion.

  Extend the clean-checkout browser script's `TS_BROWSER_PROJECTS` input so it
  installs the selected engines and forwards Playwright arguments with
  `npm --prefix ... exec -- playwright`; it must retain the release-WASM, Viceroy
  config, Docker image, npm install, and TSJS fixture preparation from Task 0.

- [ ] **Step 2: Create deterministic local GPT and locally authored fictional APS-runner**
      success/failure fixtures; run the vendored exact PUC 1.17.2 artifact for the
      creative path. The fictional runner must not copy, transform, derive from, or
      archive APS runner bytes and must never be packaged as a production fallback. Do
      not replace PUC's `prebidMessenger`, `runDynamicRenderer`, or `h.sendMessage`
      behavior and do not mock the kernel/services under test.

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
  - APS/ADM/cache documents: renderer load/error/removal/replacement, runner
    acknowledgement/failure/timeout, exact 1/4096 dimensions in Rust/TS/embedded ES5/
    cache/PUC DOM, ADM initial `about:blank` versus intended `srcdoc`, and proof that
    only the current intended navigation can accept. Cache expansion uses only
    `String(attempt.winnerContext.selectedCpm)` and leaves `${AUCTION_PRICE:B64}`
    untouched;
  - runtime/bootstrap: prepare reject/abort, activation throw at every checkpoint,
    9,999/10,000/10,001 ms boundaries, duplicate `afterCommit`, 15/16 member capacity,
    late continuation after fallback, publisher work during startup, exact same-task
    rollback, full/fallback `TsjsApi` own surfaces, malformed boot, actual-Array queue
    swap/retained references/native mutators/nested pushes/callback throws, and missing
    main bundle after server projection;
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
      direct ADM/cache, attributable empty fallback, SRA, refresh, SPA navigation, and
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

### Task 22: Delete final legacy surfaces and enforce absence

**Files:**

- Modify: `crates/trusted-server-js/lib/package.json`
- Modify: `crates/trusted-server-js/lib/eslint.config.js`
- Create: `crates/trusted-server-js/lib/scripts/check-architecture.mjs`
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
- Modify: `crates/trusted-server-core/src/integrations/gpt_bootstrap.js`
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
- Modify: `docs/guide/integrations/aps.md`
- Modify: `docs/guide/auction-orchestration.md`
- Modify: `docs/guide/configuration.md`
- Modify: `docs/guide/creative-processing.md`
- Modify: `docs/guide/integration-guide.md`
- Modify: `docs/guide/integrations/prebid.md`
- Modify: `docs/guide/integrations/didomi.md`

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
  generated Prebid external artifact. For every former integration configuration
  emitter/consumer, separately assert the exact immutable `tsjs.boot.*` replacement
  in server output, integration consumers, fixtures, and current guides; deleting an
  emitter without migrating its value is a test failure.

  Scope the executable search to shipped source, current guides, tests, scripts,
  and workflows; exclude historical `docs/superpowers` designs/plans because this
  work does not rewrite separate completed specifications. The enumerated file list
  above is the current baseline hit inventory and must be updated if Task 0 finds
  another in-scope hit.

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

- [ ] **Step 4: Add an executable absence script to `package.json` and CI, build every**
      integration combination used by server fixtures, and rerun all TS and adapter
      route tests.

### Task 23: Add deterministic bundle, browser-time, and retained-heap gates

**Files:**

- Modify: `crates/trusted-server-js/lib/build-all.mjs`
- Modify: `crates/trusted-server-js/lib/scripts/check-bundle-budgets.mjs`
- Read: `crates/trusted-server-js/lib/test/fixtures/performance/aps-tsjs-prechange.json`
- Modify: `crates/trusted-server-integration-tests/browser/tests/shared/tsjs-performance.spec.ts`
- Modify: `.github/workflows/test.yml`
- Modify: `.github/workflows/integration-tests.yml`

- [ ] **Step 1: Consume, but do not regenerate, the pre-change artifact captured in Task 0. Fail**
      minimal/reference/maximal deterministic gzip or Brotli growth above 5% unless a
      separate review explicitly updates the baseline.

- [ ] **Step 2: On the pinned Chromium/CI-machine/fixture, measure boot-to-first-display p90 after**
      five warmups and 50 samples and require ≤1.10× the Task 0 baseline. Do not rerun
      selectively to turn a failed sample into a pass.

- [ ] **Step 3: Through Chromium CDP, collect garbage then record retained heap after boot, first**
      render, refresh, and SPA navigation; gate each checkpoint at ≤1.10×. Firefox and
      WebKit remain correctness-only and do not emit synthetic heap equivalents.

- [ ] **Step 4: Keep these gates separate from render correctness: performance cannot convert a**
      failed conformance test to a pass.

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

- [ ] **Step 4: Run three clean-checkout workflows for the exact release commit. Add**
      `workflow_dispatch` with required `evidence_id` and `release_id` inputs to
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
  verify every `headSha`, conclusion, and release-id attestation:

  ```bash
  RELEASE_REF="$(git branch --show-current)"
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

- [ ] **Step 6: Extend the workflows to upload `aps-tsjs-quality-<run-id>`,**
      `aps-tsjs-cutover-<commit-sha>`, and `aps-real-gam-<run-id>` artifacts,
      substituting actual GitHub values. Include exact command logs, sanitized
      Playwright reports/traces, route parity output, corpus/staleness output, bundle/
      performance reports, release id, commit SHA, run id, conclusion, and where
      applicable the prior deployable artifact id. Run the Task 21 pre-upload scrub on
      every browser artifact and fail if APS runner/creative bodies, secrets,
      descriptors, or capabilities are present. GitHub Actions artifacts for those
      three successful runs are the sole evidence location; do not create another
      repository document.

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
2. Rust, TypeScript, embedded ES5, programmatic registration, cache expansion, and
   DOM validation agree on the exact descriptor grammar and 1–4096 dimensions while
   preserving invalid-versus-out-of-range reasons.
3. Initial and SPA projections enforce all grammar/count/UTF-8/8 MiB bounds
   transactionally; SPA state lives only in `NavigationSession`, boot stays frozen,
   and over-cap projections reduce all winners to `winner_not_renderable` without TS
   projected bids or `/auction` seatbids.
4. Server reservations are the sole PUC authority, attempt/ticket/nonce issuers are
   bounded as specified, and every live reservation/attempt retains the immutable
   `WinnerContext` used for cache price expansion instead of response, targeting, or
   current-projection data.
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
10. Render trace and GPT diagnostics expose only the exact bounded frozen asynchronous
    APIs; no correctness callback runs publisher diagnostics code, no legacy event or
    alias remains, inactive GPT diagnostics has zero incremental side effects, and
    creative guards auto-install from exact frozen boot data with both-false zero DOM
    effects.
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
    and clippy targets, Vitest, bundle/artifact builds, Playwright, size, browser-time,
    and retained-heap gates pass in attested clean-checkout quality, integration, and
    real-GAM runs for the exact release SHA and release id.
16. The binary cutover and 24-hour monitor complete or the exact prior immutable
    artifact is restored cleanly; no runtime selector, percentage router, or dual
    protocol is introduced.
17. The emergency APS-disable path stops admission and returns local `404 no-store`
    for both reserved APS routes; binary rollback is never represented as restoring
    mutable upstream runner bytes.
18. No analytics, persistence, billing, experimentation, deployment-routing, or new
    external observability requirement was introduced.
