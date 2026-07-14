# Auction Parity Phase 4: Browser Lifecycle and Media Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make direct and Prebid browser auctions complete at the correct time without mutating publisher configuration, while refresh preserves request-scoped units, bidder parameters, targeting, floors, aliases, and original media.

**Architecture:** Normalize publisher ad units into an immutable registry at each request boundary, partition cloned bids into server/client execution plans, and rebuild refresh requests from that registry rather than previously mutated `pbjs.adUnits`. Use abortable auction calls and exactly-once callbacks, preserve provider media capabilities, and make long-lived SPA consent changes only tighten the server-injected decision.

**Tech Stack:** TypeScript, Prebid.js NPM adapter APIs, Fetch/AbortController, GPT, DOM mutation/readiness APIs, Vitest/JSDOM, Rust wire/config validation.

**Reference spec:** `docs/superpowers/specs/2026-07-14-server-client-auction-parity-design.md` section 14, section 17 Phase 4, and section 18.

**Depends on:** Phase 3 canonical browser bid, renderer, and lifecycle contracts.

**Execution workspace:** Continue on `feat/auction-parity-foundation`; do not create a worktree unless the user changes that decision.

---

## File map

### Create

- `crates/trusted-server-js/lib/src/integrations/prebid/ad_unit_registry.ts` — immutable normalized unit/request snapshots and aliases.
- `crates/trusted-server-js/lib/src/integrations/prebid/execution_plan.ts` — pure server/client bidder partition with media capability filtering.
- `crates/trusted-server-js/lib/src/core/consent_decision.ts` — monotonic browser decision state for long-lived pages.
- `crates/trusted-server-js/lib/test/integrations/prebid/ad_unit_registry.test.ts`.
- `crates/trusted-server-js/lib/test/integrations/prebid/execution_plan.test.ts`.
- `crates/trusted-server-js/lib/test/core/consent_decision.test.ts`.

### Modify

- `crates/trusted-server-js/lib/src/core/auction.ts`, `core/request.ts`, `core/registry.ts`, `core/types.ts`, and `core/index.ts`.
- `crates/trusted-server-js/lib/src/integrations/prebid/index.ts` and `user_id_modules.ts`.
- `crates/trusted-server-js/lib/src/integrations/gpt/index.ts` and SPA/refresh tests.
- `crates/trusted-server-core/src/integrations/prebid.rs`, settings/config validation, and injected client config types.
- Rust and JS wire fixtures introduced in Phase 2.

---

### Task 1: Make auction HTTP calls abortable and outcome-aware

**Files:**

- Modify: `crates/trusted-server-js/lib/src/core/auction.ts:150-200`
- Test: `crates/trusted-server-js/lib/test/core/auction.test.ts`

- [ ] **Step 1: Write failing success/no-bid/error/timeout tests**

Use fake timers and mocked fetch to assert distinct outcomes for JSON success, `204`,
admitted non-2xx, malformed response, network failure, and abort timeout. Verify the
timer is always cleared, late fetch completion cannot change the settled result, and
the potentially large auction request never sets `keepalive: true`.

- [ ] **Step 2: Run focused tests and verify RED**

Run:

```bash
cd crates/trusted-server-js/lib
npx vitest run test/core/auction.test.ts
```

Expected: current `sendAuction` has no caller timeout/abort result and flattens failures to an empty array.

- [ ] **Step 3: Implement typed abortable outcomes**

```ts
export type AuctionHttpOutcome =
  | { kind: 'bids'; bids: AuctionBid[] }
  | { kind: 'no-bid' }
  | { kind: 'timeout' }
  | { kind: 'error'; error: unknown }

export async function sendAuction(
  endpoint: string,
  request: AdRequest,
  options: { timeoutMs: number; signal?: AbortSignal }
): Promise<AuctionHttpOutcome>
```

Compose an external signal with a local timeout controller without leaking listeners.
Remove `keepalive: true` from the auction fetch; normal request lifetime plus abort
semantics own navigation behavior, and notice transport is a separate concern.

- [ ] **Step 4: Run focused tests and verify GREEN**

Run the command from Step 2.

Expected: all outcome/abort tests pass.

- [ ] **Step 5: Commit abortable auction transport**

```bash
git add crates/trusted-server-js/lib/src/core/auction.ts crates/trusted-server-js/lib/test/core/auction.test.ts
git commit -m "Make browser auctions abortable"
```

### Task 2: Fix `requestAds` completion and callback semantics

**Files:**

- Modify: `crates/trusted-server-js/lib/src/core/request.ts:25-80`
- Test: `crates/trusted-server-js/lib/test/core/request.test.ts`

- [ ] **Step 1: Write failing callback tests**

Freeze both supported public signatures as callback-with-no-arguments:
`requestAds(callback, options?)` and
`requestAds({ bidsBackHandler: callback, timeout? })`. Assert the callback is not
synchronous, receives no arguments, fires once after render attempts complete, fires
once on no-bid/error/timeout, respects the caller timeout, isolates callback
exceptions, and does not reintroduce keepalive through the `requestAds` wrapper.

- [ ] **Step 2: Run request tests and verify RED**

Run:

```bash
cd crates/trusted-server-js/lib
npx vitest run test/core/request.test.ts
```

Expected: current callback fires synchronously before the HTTP request finishes.

- [ ] **Step 3: Await the full auction/render lifecycle**

Keep the public function returning `void` and both callback types as `() => void`. Run
one internal async operation and invoke a no-argument, `once`-wrapped callback in
`finally`. Await all per-bid render promises with `Promise.allSettled`.

- [ ] **Step 4: Run request tests and verify GREEN**

Run the command from Step 2.

Expected: completion and exactly-once tests pass.

- [ ] **Step 5: Commit callback correction**

```bash
git add crates/trusted-server-js/lib/src/core/request.ts crates/trusted-server-js/lib/test/core/request.test.ts
git commit -m "Complete requestAds callbacks after auction"
```

### Task 3: Add an immutable normalized ad-unit registry

**Files:**

- Create: `crates/trusted-server-js/lib/src/integrations/prebid/ad_unit_registry.ts`
- Modify: `crates/trusted-server-js/lib/src/core/registry.ts`
- Test: `crates/trusted-server-js/lib/test/integrations/prebid/ad_unit_registry.test.ts`

- [ ] **Step 1: Write failing normalization/immutability tests**

Normalize global and request-scoped units with banner/video/native media, sizes, floors, targeting, custom zone, labels, ortb2-allowed context, and bidder params. Freeze or clone the output and prove later mutation of publisher input does not change the snapshot; also prove normalization never mutates input.
For each snapshot, test every alias source explicitly: Prebid code, GPT element ID,
injected slot ID, configured creative ID, and div ID. Each alias must resolve only
within its own `AuctionScopeId`.

- [ ] **Step 2: Run registry tests and verify RED**

Run:

```bash
cd crates/trusted-server-js/lib
npx vitest run test/integrations/prebid/ad_unit_registry.test.ts
```

Expected: the registry module does not exist and current code reads/mutates live `pbjs.adUnits`.

- [ ] **Step 3: Implement normalized unit and alias types**

```ts
export interface NormalizedAdUnit {
  readonly code: string
  readonly aliases: readonly string[]
  readonly mediaTypes: Readonly<NormalizedMediaTypes>
  readonly floor?: number
  readonly targeting: Readonly<Record<string, TargetingValue>>
  readonly bids: readonly NormalizedBid[]
  readonly context: Readonly<Record<string, ContextValue>>
}

export class AdUnitRegistry {
  capture(units: readonly unknown[], scope: AuctionScope): AuctionSnapshot
  resolve(scopeId: AuctionScopeId, alias: string): NormalizedAdUnit | undefined
}
```

`AuctionSnapshot` contains its immutable `scopeId` and units. Alias indexes are nested
under that scope ID, never global last-write-wins maps. Use structured cloning
functions, not JSON stringify/parse, so undefined and typed values are handled
deliberately.

- [ ] **Step 4: Run registry tests and verify GREEN**

Run the command from Step 2.

Expected: immutability, request-scope, and alias tests pass.

- [ ] **Step 5: Commit normalized registry**

```bash
git add crates/trusted-server-js/lib/src/integrations/prebid/ad_unit_registry.ts crates/trusted-server-js/lib/src/core/registry.ts crates/trusted-server-js/lib/test/integrations/prebid/ad_unit_registry.test.ts
git commit -m "Add immutable Prebid ad unit registry"
```

### Task 4: Partition server/client bidders without mutating publisher units

**Files:**

- Create: `crates/trusted-server-js/lib/src/integrations/prebid/execution_plan.ts`
- Modify: `crates/trusted-server-js/lib/src/integrations/prebid/index.ts:487-690`
- Test: `crates/trusted-server-js/lib/test/integrations/prebid/execution_plan.test.ts`
- Test: existing Prebid index tests

- [ ] **Step 1: Write failing partition tests**

Cover server-only, client-only, mixed units, inline bidder parameters, duplicate bidder declarations, overlapping server/client configuration, and repeated `requestBids` calls. Assert original units are byte/deep-equal before and after and neither side executes the same bidder twice.

- [ ] **Step 2: Run partition tests and verify RED**

Run:

```bash
cd crates/trusted-server-js/lib
npx vitest run test/integrations/prebid/execution_plan.test.ts test/integrations/prebid/index.test.ts
```

Expected: current shim filters and appends bids directly on publisher ad units.

- [ ] **Step 3: Build a pure execution plan from snapshots**

```ts
export interface AuctionExecutionPlan {
  readonly nativeUnits: readonly PrebidAdUnit[]
  readonly trustedServerUnits: readonly PrebidAdUnit[]
}

export function buildExecutionPlan(
  units: readonly NormalizedAdUnit[],
  config: BidderRoutingConfig
): AuctionExecutionPlan
```

Generate cloned Prebid units for the original adapter call and cloned `trustedServer` units containing the captured per-bidder map. Treat overlap as an injected configuration error, not last-write-wins behavior.

- [ ] **Step 4: Run partition tests and verify GREEN**

Run the command from Step 2.

Expected: partition and no-mutation tests pass across repeated calls.

- [ ] **Step 5: Commit non-mutating bidder folding**

```bash
git add crates/trusted-server-js/lib/src/integrations/prebid/execution_plan.ts crates/trusted-server-js/lib/src/integrations/prebid/index.ts crates/trusted-server-js/lib/test/integrations/prebid
git commit -m "Partition Prebid bidders without mutation"
```

### Task 5: Reconstruct refresh from the immutable request snapshot

**Files:**

- Modify: `crates/trusted-server-js/lib/src/integrations/prebid/index.ts:730-815`
- Modify: `crates/trusted-server-js/lib/src/integrations/gpt/index.ts:680-800`
- Modify: `crates/trusted-server-js/lib/src/integrations/prebid/ad_unit_registry.ts`
- Test: Prebid refresh and GPT SPA tests

- [ ] **Step 1: Write failing refresh tests**

Cover a request-scoped unit absent from global `pbjs.adUnits`, two scopes reusing the
same alias, repeated refresh, aliased DOM/GPT IDs, preserved inline params, floor,
targeting, zone, and context. Assert refresh resolves `(scopeId, alias)` to the original
snapshot even after a newer scope captures that alias and publisher/global arrays
change.

- [ ] **Step 2: Run refresh tests and verify RED**

Run:

```bash
cd crates/trusted-server-js/lib
npx vitest run test/integrations/prebid/index.test.ts test/integrations/gpt/spa_hook.test.ts
```

Expected: current lookup loses at least request-scoped data or depends on previously mutated units.

- [ ] **Step 3: Make refresh resolve aliases into the captured auction scope**

Carry the captured `AuctionScopeId` with the request/page-bids lifecycle and call
`registry.resolve(scopeId, alias)` during refresh. Retire unscoped lookup and fallback
reconstruction from already-folded `pbjs.adUnits` once migration tests pass.

- [ ] **Step 4: Run refresh tests and verify GREEN**

Run the command from Step 2.

Expected: request-scoped and repeated-refresh fixtures pass.

- [ ] **Step 5: Commit snapshot-based refresh**

```bash
git add crates/trusted-server-js/lib/src/integrations/prebid crates/trusted-server-js/lib/src/integrations/gpt/index.ts crates/trusted-server-js/lib/test/integrations
git commit -m "Rebuild refresh from immutable ad units"
```

### Task 6: Preserve native/video media and enforce server capabilities

**Files:**

- Modify: `crates/trusted-server-js/lib/src/integrations/prebid/execution_plan.ts`
- Modify: `crates/trusted-server-js/lib/src/core/auction.ts`
- Modify: injected config in `crates/trusted-server-core/src/integrations/prebid.rs`
- Test: execution-plan and index tests

- [ ] **Step 1: Write failing media tests**

Use units containing banner + video, native-only, video-only, and multi-format client bidders. Assert banner-only Trusted Server receives only supported formats while native/client adapters retain original media objects; no refresh coerces media into banner sizes.

- [ ] **Step 2: Run media tests and verify RED**

Run:

```bash
cd crates/trusted-server-js/lib
npx vitest run test/integrations/prebid/execution_plan.test.ts
```

Expected: current builder supports banner only and refresh can reconstruct media incompletely.

- [ ] **Step 3: Filter by injected provider capabilities**

Inject a versioned capability object from Rust. The execution plan may omit unsupported formats from the Trusted Server clone but must never alter the native clone. A unit with no supported server format gets no `trustedServer` bid.

- [ ] **Step 4: Run media tests and verify GREEN**

Run the command from Step 2.

Expected: all mixed-media fixtures preserve the original native request.

- [ ] **Step 5: Commit media-preserving routing**

```bash
git add crates/trusted-server-js/lib/src/integrations/prebid crates/trusted-server-js/lib/src/core/auction.ts crates/trusted-server-core/src/integrations/prebid.rs crates/trusted-server-js/lib/test/integrations/prebid
git commit -m "Preserve media across bidder routing"
```

### Task 7: Bound per-slot readiness without blocking unrelated slots

**Files:**

- Modify: `crates/trusted-server-js/lib/src/integrations/gpt/index.ts`
- Modify: `crates/trusted-server-js/lib/src/shared/scheduler.ts`
- Test: `crates/trusted-server-js/lib/test/integrations/gpt/spa_hook.test.ts`

- [ ] **Step 1: Write failing readiness tests**

Simulate one immediately available slot, one delayed slot, and one missing slot. Assert the available slot auctions/renders immediately, the delayed slot runs within its own deadline, and the missing slot times out without delaying either.

- [ ] **Step 2: Run SPA tests and verify RED**

Run:

```bash
cd crates/trusted-server-js/lib
npx vitest run test/integrations/gpt/spa_hook.test.ts
```

Expected: current shared readiness wait delays unrelated slots in at least one fixture.

- [ ] **Step 3: Implement per-slot readiness promises**

Use one bounded observer/poll promise per canonical slot alias set and `Promise.allSettled`; never use a single all-slots gate.

- [ ] **Step 4: Run SPA tests and verify GREEN**

Run the command from Step 2.

Expected: independent timing assertions pass.

- [ ] **Step 5: Commit per-slot readiness**

```bash
git add crates/trusted-server-js/lib/src/integrations/gpt/index.ts crates/trusted-server-js/lib/src/shared/scheduler.ts crates/trusted-server-js/lib/test/integrations/gpt/spa_hook.test.ts
git commit -m "Isolate SPA slot readiness waits"
```

### Task 8: Make long-lived consent decisions monotonic and fail closed

**Files:**

- Create: `crates/trusted-server-js/lib/src/core/consent_decision.ts`
- Modify: `crates/trusted-server-js/lib/src/integrations/prebid/index.ts`
- Modify: `crates/trusted-server-js/lib/src/integrations/prebid/user_id_modules.ts`
- Modify: `crates/trusted-server-js/lib/src/integrations/osano/index.ts`
- Modify: `crates/trusted-server-js/lib/src/integrations/sourcepoint/index.ts`
- Test: `crates/trusted-server-js/lib/test/core/consent_decision.test.ts`
- Test: `crates/trusted-server-js/lib/test/integrations/prebid/index.test.ts`
- Test: `crates/trusted-server-js/lib/test/integrations/osano/index.test.ts`
- Test: `crates/trusted-server-js/lib/test/integrations/sourcepoint/index.test.ts`

- [ ] **Step 1: Write failing consent-change tests**

Start with server allow then browser/CMP deny, server deny then browser allow, unknown fail-closed jurisdiction, and identity-only withdrawal. Assert decisions can become stricter but never more permissive than the server snapshot; withdrawal stops new auctions/user-ID sync and clears browser EID persistence.

- [ ] **Step 2: Run consent tests and verify RED**

Run:

```bash
cd crates/trusted-server-js/lib
npx vitest run test/core/consent_decision.test.ts test/integrations/prebid/index.test.ts test/integrations/osano/index.test.ts test/integrations/sourcepoint/index.test.ts
```

Expected: no monotonic decision service exists.

- [ ] **Step 3: Implement monotonic decision combination**

```ts
export function combineDecision(
  server: AuctionPermission,
  dynamic: AuctionPermission
): AuctionPermission {
  return {
    auctionAllowed: server.auctionAllowed && dynamic.auctionAllowed,
    identityAllowed: server.identityAllowed && dynamic.identityAllowed,
  }
}
```

Have the existing Osano and Sourcepoint change hooks publish normalized updates to the
decision service, and have Prebid consume that service. Do not add vendor-specific
global polling. CMPs without an established change hook remain bounded by the original
server snapshot rather than gaining permission.

- [ ] **Step 4: Run consent tests and verify GREEN**

Run the command from Step 2.

Expected: no dynamic signal can relax a server denial.

- [ ] **Step 5: Commit dynamic consent tightening**

```bash
git add crates/trusted-server-js/lib/src/core/consent_decision.ts crates/trusted-server-js/lib/src/integrations/prebid crates/trusted-server-js/lib/src/integrations/osano/index.ts crates/trusted-server-js/lib/src/integrations/sourcepoint/index.ts crates/trusted-server-js/lib/test
git commit -m "Tighten browser consent decisions dynamically"
```

### Task 9: Lock browser lifecycle compatibility and run the Phase 4 gate

**Files:**

- Modify: `crates/trusted-server-integration-tests/fixtures/auction/canonical-banner-v2.json`
- Modify: `crates/trusted-server-integration-tests/fixtures/auction/invalid-cases-v2.json`
- Create: `crates/trusted-server-integration-tests/browser/tests/shared/auction-parity.spec.ts`
- Modify: `docs/guide/integrations/prebid.md`
- Modify: `docs/guide/integrations/gpt.md`
- Modify: `docs/guide/auction-orchestration.md`

- [ ] **Step 1: Add full browser fixtures**

Cover direct `requestAds`, initial Prebid request, request-scoped auction, SPA route change, repeated refresh, mixed server/client bidders, native/video preservation, slow/missing slots, timeout, and consent withdrawal.
For one fixed EC fixture, assert initial navigation, SPA page-bids, and refresh each
retain a different fresh auction UUID.

- [ ] **Step 2: Add backwards-compatibility assertions**

Verify both callback signatures still work, bidder alternate-code behavior remains valid, and required migration errors are explicit rather than silent drops.

- [ ] **Step 3: Update public browser lifecycle documentation**

Document asynchronous callback timing, immutable publisher units, request-scoped refresh, media capability split, aliases, timeout tiers, and monotonic consent behavior.

- [ ] **Step 4: Run the complete Phase 4 verification set**

Run the full Rust/adapter gate because injected config changed, then JS build/Vitest/format, browser integration tests, and docs format/build. Every command must exit zero.

- [ ] **Step 5: Commit fixtures and documentation**

```bash
git add crates/trusted-server-js crates/trusted-server-core/src/integrations/prebid.rs crates/trusted-server-integration-tests docs
git commit -m "Lock browser auction lifecycle parity"
```

---

## Phase 4 completion checkpoint

Do not start final telemetry/schema cleanup until callbacks are asynchronous and exactly once, every publisher object remains unmodified, refresh is snapshot-based and request-scope-safe, mixed media remains intact for native bidders, unsupported server media is explicit, one missing slot cannot delay others, and dynamic consent can only tighten the server decision.
