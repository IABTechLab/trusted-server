# Publisher-registered initial GPT delivery implementation plan

Status: [Independently re-reviewed](../reviews/2026-10-27-854-publisher-registered-initial-delivery-rereview.md), with no remaining design findings.
User authorized implementation and a draft PR on 2026-10-27. Implementation,
final V4 local checks, normal browser matrix and independent source review are
complete. Real-GPT staging and deployment remain unapproved. See the
[execution evidence](../reviews/2026-10-27-854-publisher-registered-initial-delivery-validation.md).

Spec: [Publisher-registered initial GPT delivery](../specs/2026-10-27-854-publisher-registered-initial-delivery-design.md).

Worktree: `issue-854-publisher-slot-registration`, based on
`5bc38862512c5c71085b5052bc3e528ee9b9987d`.

## Outcome

Land one opt-in correctness change for #854: a publisher registers its final live
GPT slot and explicitly delegates initial delivery to TS. TS waits for the
current admitted server payload, applies eligible targeting, then displays and
explicitly refreshes that slot once. V1 requires effective initial load already
disabled. Automatic placements retain existing behavior for enabled and disabled
initial loading. No bundle deferral, reservation injection, format-alias shortcut,
or production deployment belongs in this implementation.

This branch starts without the earlier uncommitted CWV tooling. Implement only
the minimal durable regression coverage needed here. Do not copy customer data,
raw captures, operator configuration, temporary artifacts, `node_modules`, or
changes from another worktree. The plan's commands run from this worktree root
unless a command explicitly changes directory.

## Working rules and dependencies

- Tasks 0 through 7 are dependency-ordered. Task 0 proves the test-only provider
  transport before delivery implementation begins. Do not activate the new
  configuration between partial tasks. Task 8 validates the combined code, Task 9 wires CI and
  documentation, and Task 10 governs release evidence.
- Write the relevant failing test before each behavior change. A new API missing
  from old code is one failure signal; also preserve a native-request reproduction
  of the old automatic changed-ID/size sequence so the causal behavior is visible.
- Use focused tests at each completed behavioral unit, broader suites at stable
  integration checkpoints, and the complete project gates before PR handoff.
- One command owner per Cargo target directory, generated browser artifact set,
  fixture server/port and mutable integration state. Reuse current passing evidence
  until relevant inputs change. Do not run timing measurements during builds.
- Stop for design review instead of adding sleeps, new grace periods, polling,
  global GPT policy changes, unknown-size guesses, or request suppression for
  arbitrary publisher calls.
- Read this worktree's `AGENTS.md` and `AGENTS.override.md` first. Scope commits
  to intended files when requested. Do not push, open a PR, deploy or enable a
  production setting without the corresponding approval.

### Shared setup before Task 0

Install this worktree's JS/browser dependencies and build its baseline assets
before the transport preflight. Reuse these outputs in Task 1 while inputs remain
unchanged:

```sh
npm ci --prefix crates/trusted-server-js/lib
npm ci --prefix crates/trusted-server-integration-tests/browser
(cd crates/trusted-server-integration-tests/browser && npx playwright install chromium)
(cd crates/trusted-server-js/lib && node build-all.mjs)
```

These are future implementation commands, not checks run during this documentation
revision. The launcher must build or accept a source-fingerprinted baseline WASM
and the Next.js fixture image through the existing integration build paths.

## Task 0. Prove the test-only provider transport before delivery work

**Create as test infrastructure, then extend in Task 8:**

- `crates/trusted-server-integration-tests/examples/publisher_registration_backends.rs`
- `crates/trusted-server-integration-tests/fixtures/configs/trusted-server.publisher-registration.toml`
- `crates/trusted-server-integration-tests/fixtures/frameworks/nextjs/app/api/fixture-auction/route.ts`
- `scripts/test-publisher-registration.sh`, initially supporting `--transport-smoke`.

The chosen arrangement uses Viceroy's static backend mapping, not a production
HTTP-provider exception or a new TLS trust configuration. The application provider
keeps `https://auction.example.com/api/fixture-auction` and certificate checking.
Only the isolated generated Viceroy configuration maps its predicted backend names
to `http://127.0.0.1:<fixture-port>`. The backend base URL has no API path prefix,
so the provider's original `/api/fixture-auction` path is preserved once.

Source supports the mechanism on the pinned Viceroy 0.17.0:

- `src/session.rs::add_backend` rejects names already present in static or dynamic
  backends, and `backend` resolves the static entry first.
- `src/wiggle_abi/req_impl.rs::register_dynamic_backend` turns that rejection into
  `BackendNameRegistryError`; `src/error.rs::to_fastly_status` maps it to ERROR.
- Fastly SDK 0.12 maps ERROR to `BackendCreationError::NameInUse`. This checkout's
  `crates/trusted-server-adapter-fastly/src/backend.rs::BackendConfig::ensure`
  reuses that existing name, including the matching "already in use" message.
- Viceroy `src/upstream.rs::canonical_uri` and `BackendConnector::call` select the
  configured backend URI and scheme, not the canonical provider URL's transport.
- Viceroy `src/config/backends.rs` parses named static backend `url` entries.

Pinned upstream source is under
[`fastly/Viceroy` v0.17.0](https://github.com/fastly/Viceroy/tree/v0.17.0/src).
This is source evidence, not an executed transport pass. The smoke below is a
hard preflight gate; do not leave the first attempt until Task 8.

- [x] Read the current fixture app config, compile one prebid-server provider and
      its explicit bidder routes, and keep unrelated integrations/providers off.
      Before Task 2, use the existing default automatic slot schema; add the new
      initialization field only after it is supported.
- [x] Implement the native test example using already available integration-test
      dev dependencies. Parse the compiled fixture provider's public endpoint,
      provider ID and timeout fields. Construct its `PlatformBackendSpec` with the
      same endpoint host/443 port semantics, certificate flag, no Host override,
      provider discriminator and first/between-byte timers used by `ProviderPlan`.
      Do not export a production-only private method merely for this helper.
- [x] Use `BackendNamingPolicy::Fastly.canonicalize_transport_timeout_ms` and
      `predict`, not a copied hash/bucketing algorithm. Enumerate and deduplicate
      every reachable positive timer for the fixture auction budget, including the
      configured provider timer and reduced-budget buckets. Map every resulting
      name to the same loopback origin. One hardcoded full-budget name is unsafe
      because provider requests hash their request-local transport budget.
- [x] Add native helper tests for full/reduced/minimum budgets, matching discriminator,
      endpoint path preservation and rendered TOML parsing. Compare predicted names
      to the same shared policy; do not weaken HTTPS validation for those tests.
- [x] Generate the ordinary app-config envelope with `generate-viceroy-config`,
      then append only these `[local_server.backends.<predicted-name>]` entries to
      a dedicated generated output. Leave the normal template, operator config,
      production provider validation and `certificate_check: true` unchanged.
- [x] The loopback Next.js API route records the incoming OpenRTB request and returns
      deterministic legal win/no-bid JSON through the existing prebid-server
      profile. Supply matching imp/seat/bidder identity and dimensions. Do not
      assign `tsjs.bids` in the browser to stand in for provider admission.
- [x] Run `--transport-smoke` against the baseline WASM with this generated config.
      Assert the HTTPS application endpoint compiles, dynamic registration reuses
      the generated static backend, the loopback route receives the actual Rust
      request at the intended path, and its known winner is admitted into the
      actual initial/seam or page-bids response. Assert unexpected backend names
      or external provider attempts fail the smoke.
- [x] Persist fixture/runtime versions, predicted and observed backend names,
      provider request/response facts, winner admission and logs. Prove a reduced
      transport-budget name is covered as well as the full-budget case.
- [x] If the pinned runtime cannot execute this source-supported arrangement,
      stop before Tasks 2 through 7 and return to transport design review. Do not
      change canonical endpoints to HTTP, disable certificate checks, silently
      use handwritten browser bids or mark the later framework gate passed.

**Commands to implement and execute:**

```sh
cargo test -p trusted-server-integration-tests --example publisher_registration_backends
./scripts/test-publisher-registration.sh --transport-smoke
```

**Check:** actual compiled Rust provider routing and admission work against the
local fixture without production validation changes. Full feature/browser proof
remains Tasks 7 and 8. No smoke was run while writing this plan.

## Task 1. Establish a fresh baseline and executable reproductions

**Read:**

- `crates/trusted-server-js/lib/src/integrations/gpt/index.ts`, especially
  `installScheduleInitialAdInit`, `matchingHandoff`, `installTsAdInit`,
  `waitForSlotElements` and `installSpaAuctionHook`.
- `crates/trusted-server-core/src/integrations/gpt_bootstrap.js`.
- `crates/trusted-server-js/lib/src/core/first_impression.ts` and `core/types.ts`.
- `crates/trusted-server-js/lib/src/integrations/prebid/index.ts` refresh and
  publisher first-impression registration paths.
- Existing `browser/initial-render/` runner, fixtures and evidence boundaries.

**Create:**

- `crates/trusted-server-integration-tests/browser/publisher-registration/gpt-fixture.js`
- `crates/trusted-server-integration-tests/browser/publisher-registration/pages.cjs`
- `crates/trusted-server-integration-tests/browser/publisher-registration/pages.test.cjs`
- `crates/trusted-server-integration-tests/browser/publisher-registration/run.cjs`

- [x] Confirm the branch has only the approved planning documents before editing.
- [x] Reuse the shared setup and passing transport preflight. Rebuild only when
      the corresponding source, dependency, configuration or toolchain input changed.

- [x] Run and retain clean-base focused results:

  ```sh
  (cd crates/trusted-server-js/lib && npx vitest run \
    test/integrations/gpt/ad_init.test.ts \
    test/integrations/gpt/gpt_bootstrap.test.ts \
    test/integrations/gpt/schedule_initial_ad_init.test.ts \
    test/integrations/gpt/spa_hook.test.ts)
  node --test crates/trusted-server-integration-tests/browser/initial-render/pages.test.cjs
  node crates/trusted-server-integration-tests/browser/initial-render/run.cjs
  ```

- [x] Capture any baseline failure with command, revision, input hashes, environment,
      failure signature and log. Prior checkout failures and timeouts are leads, not
      proof that this clean branch fails or passes.
- [x] Implement a transport-only GPT fixture, separate from the diagnostics stub.
      Model public definitions, service setup, effective initial-load state, displayed
      registrations, selected refreshes, slots/destruction, SRA behavior, targeting at
      native invocation, and controlled documented events. Never consult TS ownership
      to suppress requests in the stub.
- [x] Include an unrelated defined but unfetched slot. With enabled initial load
      and SRA, the fixture must expose that `display` can fetch it. With disabled
      initial load and an explicit refresh list, it must remain untouched.
- [x] Create two old-mode diagnostic controls: replace/rename the server-rendered
      ID after TS delivery, then define identical, reordered or narrowed formats.
      Record native slots, requests, element objects/IDs and visible creatives.
      Do not mark an old-ID alias as a visible-delivery pass.
- [x] Add escaping tests for all inline fixture data and a pure native-count test
      proving repeated native calls are counted even if TS claims ownership.
- [x] Default the new runner to fix expectations. An explicit `--baseline` mode
      may record the old automatic behavior but must never make failing new-mode
      assertions pass. Use `--implementation runtime|bootstrap|handoff|all` and
      `--out DIR`; output defaults to a temporary directory outside Git.

**Check:** page-construction/native-fixture tests pass; existing ten initial-render
scenarios retain their outcomes. New registration expectations fail because the
API/mode does not exist, not because the fixture secretly deduplicates requests.

## Task 2. Add the per-placement configuration and wire field

**Modify:**

- `crates/trusted-server-core/src/creative_opportunities.rs`
- `crates/trusted-server-core/src/publisher.rs`
- `crates/trusted-server-js/lib/src/core/types.ts`
- Existing Rust slot literals/helpers affected by the added field, located with
  `git grep -n -F 'CreativeOpportunitySlot {' -- crates`.

- [x] Add failing Rust tests for absent/default `auto`, explicit
      `publisher_registered`, invalid enum values, and serde round trips.
- [x] Add a typed enum with `auto` default and skip default serialization. Do not
      represent initialization with multiple independent booleans.
- [x] Add `initialization?: 'auto' | 'publisher_registered'` to the client slot
      contract. Preserve automatic wire output byte-for-byte.
- [x] Update `build_slot_json` once, emitting only a non-default mode. Assert the
      same value in initial-document slots, shared-template seam slots and successful
      `/_ts/page-bids` output. Assert gating/empty responses still omit placements.
- [x] Update every affected Rust struct literal with the default. Keep those
      changes mechanical and do not refactor unrelated auction/provider code.
- [x] Verify the mode does not change auction requests, bid selection, page
      matching, consent or template gating.

**Focused commands:**

```sh
cargo test -p trusted-server-core --lib creative_opportunities
cargo test -p trusted-server-core --lib build_slot_json
cargo test -p trusted-server-core --lib page_bids
cargo fmt --all -- --check
```

Use a repository target-matched alias if a native-only test boundary requires it;
record the exact selected target and result. Run the whole touched publisher test
module if a filter would omit the new initial/shared-template assertions.

**Check:** automatic serialization is unchanged; only explicitly opted-in admitted
placements carry the mode. New Rust tests are executed, not merely compiled.

## Task 3. Implement registration records and exclude automatic delivery

**Create:**

- `crates/trusted-server-js/lib/src/integrations/gpt/publisher_registration.ts`
- `crates/trusted-server-js/lib/test/integrations/gpt/publisher_registration.test.ts`
- `crates/trusted-server-js/lib/test/fixtures/publisher_registration_contract.ts`

**Modify:**

- `crates/trusted-server-js/lib/src/core/types.ts`
- `crates/trusted-server-js/lib/src/integrations/gpt/index.ts`
- `crates/trusted-server-js/lib/test/integrations/gpt/ad_init.test.ts`

- [x] Define the public options, handle/status discriminated types, protocol
      version, and browser-local shared record shape from the spec. Native slot and
      element references must not enter a JSON export.
- [x] Implement registration/cancellation/status and a shared current dispatcher.
      Derive the element from the native slot's exact ID. Capture the exact object,
      element, generation and eligible-format snapshot at the boundary.
- [x] Reject invalid arguments, stale generations, duplicate IDs, conflicting live
      registrations and enabled/unknown initial-load state without native delivery.
      Use supported GPT getters and existing legacy disable observation. Do not infer
      disabled mode from a missing getter, configure it, or add another global wrapper.
- [x] Make set-equivalent repeated eligibility registration return the same logical
      record without rewriting GPT's size order. Test repeated handle cancellation
      and ensure stale handles cannot cancel a replacement record or newer navigation.
      A changed eligible set requires cancel before re-registering, even when the
      native object and element stay the same.
- [x] Add a consumed placement marker that survives physical-element disappearance
      until navigation invalidation. Do not rely only on the existing physical-element
      claim registry, which can legitimately prune replaced elements.
- [x] Skip registered slots in `adInit` before any first-impression claim, fallback
      definition, fallback retry, renderable-work test or refresh. Do not put them in
      `prevGptSlots` or a late-definition handoff. Add zero-call assertions on pages
      containing only registered placeholders and on mixed automatic/registered pages.
- [x] Keep ordinary automatic initialization and enabled-load behavior unchanged.
      Do not alter the old changed-ID matcher as part of this task.

**Focused command:**

```sh
(cd crates/trusted-server-js/lib && npx vitest run \
  test/integrations/gpt/publisher_registration.test.ts \
  test/integrations/gpt/ad_init.test.ts)
```

**Check:** waiting/invalid registrations and registered SSR placeholders produce
zero requests and definitions. Automatic controls continue to request normally.
Delivery itself is added only after admission joins are established next.

## Task 4. Join authoritative initial and SPA payload admission

**Modify:**

- `crates/trusted-server-js/lib/src/integrations/gpt/publisher_registration.ts`
- `crates/trusted-server-js/lib/src/integrations/gpt/index.ts`
- `crates/trusted-server-js/lib/test/integrations/gpt/publisher_registration.test.ts`
- `crates/trusted-server-js/lib/test/integrations/gpt/schedule_initial_ad_init.test.ts`
- `crates/trusted-server-js/lib/test/integrations/gpt/spa_hook.test.ts`

- [x] Write both arrival-order tests: publisher registration before accepted
      payload, and accepted payload before registration. No request is authorized by
      head slot configuration, default empty `tsjs.bids`, or document `load` alone.
- [x] Publish generation-0 settlement only after the existing scheduler accepts
      its first payload. Preserve omitted initialSlots versus explicit `[]`, duplicate
      scheduling latches and stale-SSR rejection.
- [x] On accepted SPA navigation, invalidate old registrations/admission before
      new route work is queued. Capture generation before async work; recheck it after
      fetch and in every queued drain.
- [x] Publish accepted SPA payload before registered slots would wait on automatic
      DOM resolution. Restrict `waitForSlotElements` to automatic placements while
      preserving its behavior for those placements. Test registered-only and mixed
      routes with a never-mounted registered element.
- [x] Cover empty accepted payload, no payload seam, aborted/failed page-bids,
      late old responses, repeated pathnames after another navigation, stale GPT
      command callbacks and cancellation while waiting. None may authorize stale work.
- [x] Coalesce drains through the shared dispatcher and existing foreground/rAF
      behavior. Test a hidden document held until visible. Never use frame count or
      temporary DOM stability as the readiness condition.
- [x] Add waiting-payload and hidden-document breakpoint-change tests. The adapter
      synchronously cancels the old handle, recomputes its own effective eligibility
      and re-registers before delivery resumes. Force the old frame/payload callback
      and old-handle cancellation to run afterward; neither may request, clear nor
      cancel the replacement. Verify one eventual request with the new eligibility.
      Also test a resize after dispatch does not reopen the consumed initial request.
- [x] Recheck element identity, native-slot liveness, path, mode, generation and
      effective disabled-load policy immediately before dispatch. Policy changes while
      waiting cause a no-request rejection, not an implicit setting change.

**Focused command:**

```sh
(cd crates/trusted-server-js/lib && npx vitest run \
  test/integrations/gpt/publisher_registration.test.ts \
  test/integrations/gpt/schedule_initial_ad_init.test.ts \
  test/integrations/gpt/spa_hook.test.ts)
```

**Check:** the join has exactly two owners, accepted payload application and
publisher registration, using the existing navigation generation. No parallel
hydration timer or independent page-bids request mechanism was introduced.

## Task 5. Deliver once with eligible targeting and existing renderer behavior

**Modify:**

- `crates/trusted-server-js/lib/src/core/types.ts` for the internal renderer capability.
- `crates/trusted-server-js/lib/src/integrations/gpt/publisher_registration.ts`
- `crates/trusted-server-js/lib/src/integrations/gpt/index.ts`
- `crates/trusted-server-js/lib/test/integrations/gpt/publisher_registration.test.ts`
- `crates/trusted-server-js/lib/test/integrations/gpt/ad_init.test.ts`
- `crates/trusted-server-js/lib/test/integrations/prebid/index.test.ts` for
  integration assertions, not a refresh-policy rewrite.

- [x] Claim the untouched first impression only at final admission. If an observed
      publisher auction/request won first, return `publisher_owned` without targeting,
      a TS retry lease, a new fallback, or another request.
- [x] Commit dispatch once before native delivery. Reentrant registration cannot
      dispatch again. After any native delivery exception, retain consumed ownership
      and return `failed_after_dispatch`, since an exception does not prove no request.
- [x] Give the shared maps an explicit per-generation writer rule in bootstrap
      and runtime. Automatic initialization replaces only its automatic entries and
      derives/retains registered entries from current registration records. Registered
      dispatch updates only its entries. Include consumed/cancelled-after-dispatch
      records whose slot still needs cleanup/correlation; do not add a second
      independently mutable registered-mapping registry.
- [x] Test registered delivery before automatic frames, the reverse order, a
      registered-only empty automatic pass, and a mixed SPA route. Use a valid final
      native element ID outside the configured prefix to prove exact render lookup
      does not rely on prefix fallback. On the next navigation, assert both owners'
      old targeting keys are cleared, old mappings retired and new mappings retained.
      Include cleanup queued until GPT is ready, using the captured old snapshot.
- [x] Reuse targeting cleanup, diagnostics opportunity and render-bridge
      bookkeeping under that writer rule. Keep publisher-native slots out of
      `prevGptSlots` and TS destruction ownership.
- [x] Publish `tsRenderBridgeVersion === 1` only after the real TS message handler
      is installed. Make repeated installation idempotent without changing legacy
      automatic delivery policy. No bootstrap code may advertise an absent handler.
- [x] Filter only the offered registered-slot candidate using the installed renderer
      capability, winning `w`/`h`, configured formats and the publisher eligibility
      snapshot kept current by cancel/re-register. Preserve reordered equivalent
      sets for comparison without mutating GPT size order.
- [x] Cover valid fitting bid, narrowed incompatible bid, unknown dimensions,
      zero/no-fill bid map, fluid-only/empty finite eligibility and a missing renderer.
      All admitted cases still request GAM once; excluded candidates must have no
      managed TS candidate targeting/correlation. Ordinary GAM fill uses actual
      publisher creative content in the fixture; explicit no-fill is allowed as
      empty and must not be misreported as a failed TS renderer.
- [x] Preserve unrelated publisher targeting and clear stale TS candidate keys for
      this owned delivery only. Do not mutate the server's bid map or affect another
      automatic placement's candidate.
- [x] Ensure the render bridge cannot recover an excluded registered candidate
      merely from the globally retained bid map. Use existing first-impression targeting
      and exact-slot correlation where possible, rather than another eligibility
      registry. Test no response/beacon for a requesting slot that did not receive that
      candidate. Keep existing automatic and APS renderer behavior unchanged.
- [x] Call `display(actualId)` once followed by `refresh([offeredSlot])` once under
      the existing scoped internal/Prebid bypasses, restored with `try/finally`.
      Make no definitions, service-policy changes, global refreshes or one-shot
      publisher-refresh suppressions.
- [x] Assert an unrelated SRA slot remains unfetched. Assert a genuinely later
      publisher refresh still traverses Prebid and requests once with original options.

**Focused command:**

```sh
(cd crates/trusted-server-js/lib && npx vitest run \
  test/integrations/gpt/publisher_registration.test.ts \
  test/integrations/gpt/ad_init.test.ts \
  test/integrations/prebid/index.test.ts)
```

**Check:** native request snapshots show targeting before the single scoped request;
no candidate is fabricated for an incompatible size. A count-only/blank-creative
result is explicitly not sufficient acceptance.

## Task 6. Mirror and prove the bootstrap protocol

**Modify:**

- `crates/trusted-server-core/src/integrations/gpt_bootstrap.js`
- `crates/trusted-server-core/src/integrations/gpt.rs`
- `crates/trusted-server-js/lib/test/integrations/gpt/gpt_bootstrap.test.ts`
- Shared `publisher_registration_contract.ts` and registration tests.

- [x] Implement the same API version, statuses, payload settlement, mode skip,
      exact-object identity, mapping writer rule, size-snapshot cancellation and
      scoped request semantics in the inline implementation. Candidate eligibility
      uses the same capability check, but bootstrap-only execution has no TS bridge.
      Install the API before publisher head scripts can call it.
- [x] Keep its state on the same `window.tsjs` fields. Queued callbacks must resolve
      the current shared dispatcher after the bundle takes over.
- [x] Parameterize the common executable contract for runtime-only, bootstrap-only,
      bootstrap waiting then runtime, and bootstrap already requested then runtime.
- [x] Include runtime network failure, registration before GPT queue drain,
      duplicate runtime installation, payload arriving across implementation handoff,
      cancellation across handoff, old-navigation callbacks and post-dispatch throw.
- [x] Choose ordinary-GAM degradation when the TS renderer capability is absent:
      omit managed TS candidate targeting and render correlation, display/register
      once and explicitly refresh the registered slot once. Do not add a renderer
      to the bootstrap. Record the demand-participation tradeoff in docs and tests.
- [x] With a fitting TS bid present but no TS bridge, prove the fixture's ordinary
      publisher creative is actually rendered. After late runtime installation,
      assert no retroactive candidate targeting, automatic replacement refresh or
      response to the excluded candidate from the retained global bid map. A still
      pending registration may use the renderer only after the actual handler
      installs. Test both sides of that dispatch boundary.
- [x] Add Rust include/head-order checks and execute the actual included JS in
      Vitest. A source substring assertion alone cannot satisfy parity.
- [x] Do not migrate bootstrap bundling just to remove duplication. Share a small
      pure helper only if the current build can already emit it without an added
      blocking dependency; otherwise use the existing deliberate duplication plus
      the exact same black-box contract cases.

**Focused commands:**

```sh
(cd crates/trusted-server-js/lib && npx vitest run \
  test/integrations/gpt/publisher_registration.test.ts \
  test/integrations/gpt/gpt_bootstrap.test.ts \
  test/integrations/gpt/schedule_initial_ad_init.test.ts)
cargo test -p trusted-server-core --lib integrations::gpt
```

**Check:** a pending or consumed bootstrap registration is adopted, never restarted.
Failure to load the normal bundle still leaves one supported GAM request path.

## Task 7. Complete offline browser contracts with real Prebid and creatives

**Modify/create:** the `browser/publisher-registration/` files from Task 1.

**Reuse:** existing actual bundles, external Prebid builder and Universal Creative
handling from `browser/initial-render/`. Avoid importing that runner as a module
with startup side effects; share only small pure page/creative helpers when useful.

- [x] Extend the runner with fixed modes for publisher API registration, both
      payload arrival orders, initial inline/shared-seam representations, bootstrap
      variants, SRA isolation, old-mode controls and explicit later refresh.
- [x] Use real built core/GPT/Prebid, a manifest-verified external Prebid artifact
      and real Universal Creative. Intercept provider/GAM transport for fictional
      origins, with deterministic completion controls and no real bidder contacts.
- [x] Delay publisher mount/registration explicitly, rather than sleeping to pick
      the winner. Assert no TS requests or fallback definitions while only SSR
      placeholders are present, including while an unrelated automatic slot runs.
- [x] Assert one native GPT object and request for each final registered placement,
      request-time targeting, exact returned slot/element identity and actual creative
      content in the final separate-origin frame. Do not accept marker-only delivery
      as proof of the render bridge or an alias to a missing ID as proof of visibility.
- [x] Cover two placements sharing a GAM path independently. Reordered/narrowed
      publisher formats are used before registration; they do not trigger aliasing.
- [x] Model unmount before admission with cancel/destroy/remove ordering. For
      deliberate unmount after a request, assert consumed registration is not reopened
      and record the supported teardown boundary instead of promising preservation.
- [x] Verify real Prebid does not auction the TS initial refresh, does auction a
      subsequent publisher refresh, preserves unrelated targeting/options and leaves
      publisher-first claims alone. Also cover empty/incompatible TS bid delivery.
- [x] Test renderer-unavailable delivery separately from the TS Universal Creative
      path: no TS candidate keys, one ordinary request and actual publisher creative
      content when the fixture supplies a fill. Late bridge installation must not
      request again or answer for the excluded candidate. Explicit no-fill remains
      a valid empty result, not a guaranteed-fill assertion.
- [x] Record commit, dirty inputs, bootstrap and bundle hashes, external Prebid
      manifest, browser version, case/profile, all per-slot native calls, controlled
      network counts, render facts and console errors. Keep native requests distinct
      from network batches. Save traces/screenshots/results even on failed cases;
      startup failures remain fatal, never an allowed diagnostic variant.
- [x] Default fixed assertions are mandatory CI behavior. Do not provide a general
      allow-failures flag for the fix contract. Document missing real-GAM coverage.

**Commands:**

```sh
node --test crates/trusted-server-integration-tests/browser/publisher-registration/pages.test.cjs
node crates/trusted-server-integration-tests/browser/publisher-registration/run.cjs \
  --implementation all --out /tmp/ts-publisher-registration-offline
node crates/trusted-server-integration-tests/browser/initial-render/run.cjs
```

**Check:** old automatic-mode reproduction remains accurately labeled; supported
registered-mode tests pass one-request and visible-delivery assertions. All older
initial-render contracts retain their results.

## Task 8. Verify real framework lifecycle through actual Rust HTML injection

**Create or extend the transport infrastructure already proved in Task 0:**

- `crates/trusted-server-integration-tests/fixtures/frameworks/nextjs/app/publisher-registration/page.tsx`
- `crates/trusted-server-integration-tests/fixtures/frameworks/nextjs/app/publisher-registration/RegisteredSlot.tsx`
- `crates/trusted-server-integration-tests/fixtures/frameworks/nextjs/app/api/fixture-auction/route.ts`
- `crates/trusted-server-integration-tests/fixtures/configs/trusted-server.publisher-registration.toml`
- `crates/trusted-server-integration-tests/browser/tests/nextjs/publisher-registration.spec.ts`
- `crates/trusted-server-integration-tests/browser/playwright.publisher-registration.config.ts`
- `scripts/test-publisher-registration.sh`

**Modify only if needed:** Next.js test-fixture configuration, the browser package
scripts, and small reusable test helpers. Do not enable GPT/auctions globally in
`trusted-server.integration.toml` or change diagnostics stub assumptions.

- [x] Add a fictional React component that owns its element and native GPT slot.
      It performs GPT setup after actual component mount/commit, captures generation
      before queued work, registers before display and does no competing initial
      publisher auction. Make cancellation and destroy-before-removal explicit in
      its parent-controlled teardown path and verify call/DOM ordering.
- [x] Offer deterministic test controls for delayed mount, pre-registration ID
      change, responsive narrowing, pending cancellation and navigation. These are
      fixture controls, not production readiness heuristics. Include a failed/delayed
      hydration case in which no registration occurs and TS must not guess readiness.
- [x] Implement the publisher's own synchronous effective-mapping/visibility
      subscriptions. If eligibility changes while waiting for payload or hidden,
      cancel before replacing the registration snapshot, without waiting for a
      passive React effect. Resize in both pending states and force old callbacks
      to run after the new token exists. Assert one request with current eligible
      targeting, no stale winner, and no post-dispatch resize reopening.
- [x] Exercise registered delivery before and after automatic initialization and
      a registered-only page. Confirm exact render correlation and next-navigation
      targeting cleanup survive all orders, including an ID outside the config prefix.
- [x] Isolate the runtime settings in the new app-config fixture. Select one
      registered slot and one automatic/unrelated control on only the fictional
      fixture route. Enable GPT and the required existing auction profile there.
- [x] Reuse Task 0's deterministic Next.js provider route and its passed transport
      preflight. Keep the application endpoint canonical HTTPS and use the generated
      static-name mappings for every reachable transport budget. Extend legal
      win/no-bid responses for this task's cases without changing production profile
      validation or allowing external provider traffic.
- [x] Generate an isolated Viceroy config by invoking the existing
      `generate-viceroy-config` binary with `--app-config` pointing to the new fixture,
      `--template` the existing Viceroy template, `--origin-url` the fixture origin,
      and a separate output under `target/integration-test-artifacts/publisher-registration/`.
      Append the verified static backend mappings from Task 0's native helper and
      assert the configured fixture port and runtime pin match the passed preflight.
      Do not overwrite the normal integration config or an operator config.
- [x] Use the existing global setup/teardown with `TEST_FRAMEWORK=nextjs` and
      `VICEROY_CONFIG_PATH` for this isolated runtime. Restrict the new Playwright
      configuration to the new spec and guard that spec outside its dedicated run,
      so normal Next.js/WordPress jobs keep their existing settings and expectations.
- [x] The launcher builds the required artifacts/images through existing build
      paths, prepares the custom config and passes its location to Playwright. Reuse
      dependencies where valid; each launched container/process must be cleaned by
      its own recorded ID, not a global browser/container kill.
- [x] Inspect the real transformed response and body seams. Assert the wire mode,
      unchanged script ordering, current admitted slots/bids, gated empty responses,
      successful winner targeting and SPA settlement under actual React transitions.
- [x] Install only the transport GPT stub for this local framework suite. Serve
      the actual configured external Prebid artifact through Playwright routing;
      do not use an inert stub to claim Prebid initialization or consent passed.
- [x] Assert final native slot/element objects, visible iframe creative text, no
      early fallback requests, one intended request after registration, later refresh,
      and correct teardown. Test inline and shared-template admission with explicitly
      selected fixture assembly settings, not just handwritten JS payloads.
- [x] If the framework/runtime cannot supply deterministic local admitted payloads
      without unrelated infrastructure changes, stop and review the test approach.
      Do not replace this gate with manual `tsjs.bids` assignment and call it Rust
      end-to-end coverage.

**Command to implement and document:**

```sh
./scripts/test-publisher-registration.sh
```

The script must expose reuse inputs for prebuilt WASM/config/artifacts so CI can
reuse its prepared outputs instead of rebuilding the workspace twice. Browser
runs using the shared fixture port/state must be serialized.

**Check:** both the real framework lifecycle and actual server-generated injection
are exercised. Offline direct-payload tests alone cannot clear this gate.

## Task 9. Wire CI, document the publisher adapter and run complete checks

**Modify/create:**

- `.github/workflows/integration-tests.yml`
- `crates/trusted-server-integration-tests/browser/package.json`
- `crates/trusted-server-integration-tests/browser/publisher-registration/README.md`
- `docs/guide/integrations/gpt.md`
- `docs/guide/configuration.md`, in its `[creative_opportunities]` slot reference.
- `trusted-server.example.toml` only if a commented fictional opt-in example fits
  the existing structure; never enable the mode by default.

- [x] Add standalone fixture tests and the fixed offline runner after the existing
      build/install steps. Reuse the manifest-verified Prebid artifact when possible.
- [x] Add the dedicated framework step after existing normal browser runs, with
      its own isolated app config and recorded process ownership. Keep the normal
      framework matrix in place.
- [x] Upload new traces, screenshots, request/render records and artifact hashes
      with `if: always()`. A failed assertion or browser startup must fail the job;
      uploaded artifacts are evidence, not an alternative pass condition.
- [x] Document configuration, required disabled-load setup before enableServices,
      native-slot registration, status/cancellation, generation capture, finite-size
      eligibility and pending breakpoint updates, single initial owner,
      service/teardown responsibilities and ordinary-GAM bootstrap degradation.
      State explicitly that the bootstrap gives up TS bid participation when no
      renderer is installed and that late runtime arrival never replaces that ad. Show a complete fictional React integration with explicit
      destroy-before-removal orchestration and no extra initial publisher display.
- [x] Document that missing payload/consent admission is not a permission to start,
      unknown dimensions can omit a TS offer while GAM still fills, and every rejected
      status is not a safe automatic publisher retry signal.
- [x] Document protocol-version and artifact pairing before activation, no mid-page
      ownership switch, rollback of a compatible server/adapter/config pair, and the
      remaining real-GPT gate. Do not promise that this opt-in fixes unmodified sites.

**Focused integration checkpoint:**

```sh
(cd crates/trusted-server-js/lib && npx vitest run \
  test/integrations/gpt/publisher_registration.test.ts \
  test/integrations/gpt/ad_init.test.ts \
  test/integrations/gpt/gpt_bootstrap.test.ts \
  test/integrations/gpt/schedule_initial_ad_init.test.ts \
  test/integrations/gpt/spa_hook.test.ts \
  test/integrations/prebid/index.test.ts)
(cd crates/trusted-server-js/lib && npm run lint && npm run format && node build-all.mjs)
node --test crates/trusted-server-integration-tests/browser/publisher-registration/pages.test.cjs
node crates/trusted-server-integration-tests/browser/publisher-registration/run.cjs \
  --implementation all --out /tmp/ts-publisher-registration-final
./scripts/test-publisher-registration.sh
```

**Before PR handoff, run the complete current gates from `AGENTS.md`:**

```sh
cargo fmt --all -- --check
cargo test-fastly
cargo test-fastly-reuse
cargo test-axum
cargo test-cloudflare
cargo test-spin
cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test parity
cargo test -p trusted-server-core --test template_build_digest
cargo clippy-fastly
cargo clippy-axum
cargo clippy-cloudflare
cargo clippy-cloudflare-wasm
cargo clippy-spin-native
cargo clippy-spin-wasm
cargo clippy-cli
cargo clippy-codegen
cargo clippy -p trusted-server-core --test template_build_digest -- -D warnings
(cd crates/trusted-server-js/lib && npx vitest run && npm run format && node build-all.mjs)
(cd docs && npm ci && npm run format)
docs/node_modules/.bin/prettier --config docs/.prettierrc --check \
  '*.md' '.claude/**/*.md' '.github/**/*.md' 'crates/**/*.md' \
  'scripts/**/*.md' 'tinybird/**/*.md'
scripts/lint-wasm-clock.sh
git diff --check
```

Also run the old initial-render browser runner and existing normal browser matrix
as required by the changed integration workflow. Keep the validation record compact:
command, tested commit/dirty-file fingerprints, environment, elapsed time, outcome
and log. Do not rerun unchanged passing units merely because a different worker
ran them. Reproduce failures on the clean base before classifying them as unrelated;
exclusions remain disclosed coverage gaps, not a green full suite.

**Check:** CI and local evidence refer to the exact combined implementation and
built assets, not an earlier protocol or a different worktree's distribution.

## Task 10. Review and real-GPT staging gate

No deployment is authorized by this plan. Obtain separate staging approval and
publisher-adapter access before this task's external actions.

- [x] Review source, fixtures and documentation together for shared-state drift,
      post-cancel retries, stale generation adoption, hidden broad refresh, accidental
      service-policy changes, candidate resurrection in the bridge, and TS destruction
      of publisher-native slots.
- [ ] Confirm the intended live publisher adapter can actually call the API after
      its final native definition and before its first display/auction. The repository
      example alone does not deploy that integration.
- [ ] Stage the exact reviewed release with its matched HTML bootstrap, selected
      module hashes, publisher adapter and per-placement setting. Retain deployment
      mapping to the source commit. Do not enable the new field on an old artifact.
- [ ] Capture one affected placement plus an unrelated control using real GPT/GAM.
      With the new mode, require zero requests from the SSR placeholder, one initial
      request for the final slot, eligible targeting on that request and a visible
      creative in that same live element when GAM supplies a fill. Include a
      no-TS-bid fill, renderer-unavailable ordinary-GAM fill and pending/hidden
      responsive narrowing case; late renderer installation must not replace the
      initial ordinary fill. Distinguish per-slot request events from SRA HTTP batches.
- [ ] Issue a separately controlled legitimate refresh and verify one additional
      request without affecting the unrelated slot. Verify teardown before removal.
- [ ] Record exact observation windows and errors. If trace tools fail, disclose
      missing performance proof; do not convert a failed trace into CWV measurements.
- [ ] Verify rollback by stopping new opt-in assignments and restoring a compatible
      release pair. Do not rewrite ownership inside already-open documents.
- [x] If real-GPT validation cannot run, keep the live-site fix claim blocked.
      The user may explicitly choose to merge a default-off API/tooling feature first,
      but that is not a production-verified fix or grounds to close all of #854.

## Completion record

Before proposing a PR, append a concise execution record to this plan or a linked
review note with:

- Exact changed behavior and supported publisher lifecycle.
- Config/API/protocol compatibility decisions and any reviewed deviations.
- Old failure evidence and new one-request plus visible-creative evidence.
- Focused, full-suite, framework and real-GPT checks with tested state and logs.
- Any unverified adoption, real-GAM, CI or performance claims.

Tasks remain unchecked until their implementation and verification are complete.
The execution record below follows the original documentation approval.

### 2026-10-27: Authorized; blocked at transport preflight

- The user authorized implementation and opening a draft PR when done. Production
  deployment, activation and real-GPT staging remain outside that authorization.
- Shared setup attempted `npm ci` against this worktree's copied package manifests
  in `/tmp/ts-854-implementation/js-deps`. It failed with `EDQUOT`. Browser dependency
  installation, asset builds and baseline tests were not reached. The incomplete
  installation was removed; the failure log remains at
  `/tmp/ts-854-implementation/evidence/npm-js.log`.
- Task 0's native backend-mapping example, isolated fixture TOML, Next.js auction
  route and transport-smoke launcher are drafted. Scoped Rust formatting, route
  formatting, shell syntax and `git diff --check` pass. They have not been compiled
  or exercised through Viceroy; no transport or provider-admission pass is claimed.
- `/tmp` has filesystem free space but the user's tmpfs quota was exhausted.
  Removing only the failed installation recovered about 26 MB, insufficient for
  builds. The root filesystem also has only about 1.4 GB free. Other projects'
  caches have not been removed. The user has been asked to free roughly 8 GB of
  `/tmp` quota or increase it.
- No runtime feature files changed. No commits, pushes or PR creation occurred.
  Resume shared setup and the actual full/reduced-budget transport preflight
  before starting Tasks 2 through 7.

### 2026-10-27: Quota resolved; baseline passed

With explicit user permission, two inactive Cargo caches were removed, recovering
about 8.2 GB. Shared setup now passes. The clean-base full JS suite passed all
1,211 tests, the focused GPT suites passed 215 tests, and all ten existing
initial-render browser scenarios passed. Task 0 compilation and runtime smoke
are in progress; feature delivery work remains gated on that smoke. Detailed
commands, source fingerprint and evidence paths are in the linked validation
record above.

### 2026-10-27: Implementation and final V4 local verification complete

- The opt-in schema, publisher registration API, authoritative payload join,
  renderer eligibility, shared mapping ownership and bootstrap/runtime protocol
  are implemented. Automatic configuration remains the default. Publishers own
  final native definition, disabled-load setup, eligibility updates and teardown.
- Real-history regressions found and fixed a bootstrap-only stale-admission gap
  and a catch-up retry generation bump. The final common contract preserves the
  original waiting handle/token/native through same-generation retries. The
  bootstrap has no second auction client.
- Final V4 passed 1,490 full JS tests, 794 focused instances, 11 pure fixture tests,
  109 offline browser cases, ten legacy scenarios, 26 actual Rust/Next.js cases
  and all 19 Rust commands. Normal Next.js passed 21 tests and WordPress passed
  11, with only the documented dedicated/other-framework skips.
- Actual build.rs WASM embedding matched all 13 independently built module byte
  sequences. Preparation ran without Viceroy installed in PATH; strict reuse
  captured zero Cargo/Docker builds. Normal CI's distinct nine-user-ID Prebid
  profile was built and verified separately from the feature's pinned profile.
- Four independent review lanes covered the exact 32-file target. There are no
  blocking findings. R1, a repeated test-artifact verification helper, remains
  unchanged as nonblocking maintenance. Source fingerprints stayed unchanged
  throughout final artifact checks and review.
- Two nonfatal pinned-Viceroy downstream receiver task panics appeared in the
  otherwise passing normal Next.js run. They remain unclassified because a
  clean-base reproduction was not performed. The validation record preserves
  the logs and exact expected skips rather than claiming a warning-free run.
- Hosted CI, real GPT/GAM, publisher adoption, production activation and CWV
  validation remain unverified. A draft PR is authorized; deployment is not.
  This is not grounds to close the whole of #854.

The linked validation and combined implementation review contain exact hashes,
commands, durations, retained failures and remaining limits.
