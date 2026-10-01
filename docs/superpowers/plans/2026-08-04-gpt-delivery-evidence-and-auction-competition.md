# GPT Delivery Evidence and Auction Competition Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use
> superpowers:subagent-driven-development (recommended) or
> superpowers:executing-plans to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** Correct and extend the existing GPT diagnostics attribution branch so
one request cycle shows direct-SSAT versus Prebid path evidence, GAM facts, PUC
selection, markup-response progress, and honest ambiguity without publisher
code changes.

**Architecture:** Evolve the branch in place. Preserve its documented GPT event
observer, normalized GAM identifiers, response classification, bounded lifecycle
store, browser export, overlay, and badges. Replace only the positive-only
candidate/single-claim layer with generation-safe path markers and an opaque,
bounded creative-attempt state machine; instrument the existing `adInit`,
render-bridge, and Prebid-refresh boundaries without changing delivery order.

**Tech Stack:** TypeScript, Vitest/jsdom, GPT event/slot object identity,
Prebid.js refresh wrapper, browser `MessagePort`, Prettier, ESLint, Vite build,
VitePress docs.

**Spec:**
`docs/superpowers/specs/2026-08-04-gpt-delivery-evidence-and-auction-competition-design.md`

---

## Existing Branch Treatment

Do not revert commit `7fa4d4cb`. Retain these parts:

- `GptDiagnosticsAdManagerIdentity` and documented GAM ID normalization;
- `GptDiagnosticsResponseClass`, with the one correction that an omitted
  `isEmpty` must not become `unclassified_non_empty`;
- all six GPT listeners and callback coverage accounting;
- exact current-DOM iframe-source plus slot/ad-ID bridge ownership checks;
- inline/cache rendering, size handling, macro expansion, beacon behavior, and
  in-flight cache deduplication;
- bounded GPT slots/cycles/callback issues, exact DOM binding, browser-local
  export, overlay mounting, and badge positioning;
- the existing `adInitRefreshInProgress` bypass and Prebid refresh behavior.

Delete rather than alias these unshipped branch-only contracts:

- `recordTrustedServerCandidate`;
- `recordTrustedServerClaim`;
- `trustedServerCandidate` and `trustedServerClaimAtMs`;
- delivery values `trusted_server` and `other_demand`.

No Rust, React/Next.js, publisher JavaScript, publisher markup, or GAM
configuration file changes belong in this plan.

### Atomic migration rule

Tasks 1-6 replace one unshipped public contract and must land as one atomic
implementation commit. Do not commit their intermediate checkpoints: after the
types/API change, the old store and GPT callers are temporarily stale; after the
store change, the old GPT/UI tests are temporarily stale. Adding compatibility
aliases would preserve the misleading semantics this work removes. Run each
focused RED/GREEN checkpoint while developing, then run the combined affected
suite and commit only at the end of Task 6, after every caller and presentation
has migrated.

## File Map

| File                                                                             | Action | Responsibility                                                           |
| -------------------------------------------------------------------------------- | ------ | ------------------------------------------------------------------------ |
| `crates/trusted-server-js/lib/src/core/types.ts`                                 | Modify | Additive V1 evidence types and safe writer API                           |
| `crates/trusted-server-js/lib/src/integrations/gpt_diagnostics/api.ts`           | Modify | No-throw writer delegation and detached V1 export                        |
| `crates/trusted-server-js/lib/src/integrations/gpt_diagnostics/store.ts`         | Modify | Path markers, delivery derivation, creative attempts, attribution issues |
| `crates/trusted-server-js/lib/src/integrations/gpt/index.ts`                     | Modify | Opportunity classification and staged render-bridge evidence             |
| `crates/trusted-server-js/lib/src/integrations/prebid/index.ts`                  | Modify | Prebid-path marker at controlled GPT refresh calls                       |
| `crates/trusted-server-js/lib/src/integrations/gpt_diagnostics/overlay.ts`       | Modify | Evidence-safe detailed presentation                                      |
| `crates/trusted-server-js/lib/src/integrations/gpt_diagnostics/badges.ts`        | Modify | Compact evidence-safe labels                                             |
| `docs/guide/integrations/gpt-diagnostics.md`                                     | Modify | Evidence ladder, limits, request paths, troubleshooting                  |
| `crates/trusted-server-js/lib/test/integrations/gpt_diagnostics/types.test.ts`   | Modify | V1 compatibility and new type surface                                    |
| `crates/trusted-server-js/lib/test/integrations/gpt_diagnostics/api.test.ts`     | Modify | Writer safety, export allowlist, deep detachment                         |
| `crates/trusted-server-js/lib/test/integrations/gpt_diagnostics/store.test.ts`   | Modify | Deterministic state-machine coverage                                     |
| `crates/trusted-server-js/lib/test/integrations/gpt/ad_init.test.ts`             | Modify | adInit and render-bridge evidence/failure coverage                       |
| `crates/trusted-server-js/lib/test/integrations/prebid/index.test.ts`            | Modify | Refresh-path attribution and non-interference                            |
| `crates/trusted-server-js/lib/test/integrations/gpt_diagnostics/overlay.test.ts` | Modify | Required wording and issue counts                                        |
| `crates/trusted-server-js/lib/test/integrations/gpt_diagnostics/badges.test.ts`  | Modify | Compact labels without negative winner inference                         |

`gpt_diagnostics/observer.ts` and its tests should remain unchanged unless a
compile-only import adjustment is required.

Run every command block from the repository root unless the block starts with
an explicit `cd`; start the next block from the repository root again.

---

### Task 1: Replace the Public Attribution Contract and Make Writers Safe

**Files:**

- Modify: `crates/trusted-server-js/lib/src/core/types.ts:109-258`
- Modify: `crates/trusted-server-js/lib/src/integrations/gpt_diagnostics/api.ts:1-120`
- Test: `crates/trusted-server-js/lib/test/integrations/gpt_diagnostics/types.test.ts`
- Test: `crates/trusted-server-js/lib/test/integrations/gpt_diagnostics/api.test.ts`

- [ ] **Step 1: Write failing public-type compatibility tests.**

Keep the existing base-shaped `GptDiagnosticsExportV1` object with none of the
new optional fields. Add a full evidence-shaped cycle and attribution issue:

```ts
const evidenceCycle: GptDiagnosticsRequestCycle = {
  requestNumber: 2,
  durations: {},
  incompleteSequence: false,
  requestPath: 'competing',
  trustedServerOpportunity: 'renderable_candidate',
  trustedServerCreativeRequestAtMs: 20,
  trustedServerCreativeResponseAtMs: 25,
  trustedServerCreativeFailures: ['cache_fetch_failed'],
  delivery: 'trusted_server_response_sent',
}

const issue: GptDiagnosticsAttributionIssue = {
  reason: 'creative_attempt_expired',
  timestampMs: 30,
}
```

Assert that version 1 still accepts the old object and that neither type exposes
prices, targeting, bid IDs, markup, cache URLs, or payloads.

- [ ] **Step 2: Write failing API tests.**

Use a small fake `ApiStore` to assert all five writer calls delegate exact
arguments, the creative-request call returns its numeric attempt ID, and every
throwing writer is swallowed. A throwing creative-request writer must return
`undefined`.

Extend the snapshot test to require detached copies of:

- `attributionIssues`;
- `trustedServerCreativeFailures`;
- `adManager.yieldGroupIds` and `adManager.companyIds`;
- `metadata.droppedAttributionIssues`.

- [ ] **Step 3: Run the focused tests and confirm RED.**

```bash
cd crates/trusted-server-js/lib
npx tsc --noEmit \
  --skipLibCheck \
  --target ES2022 \
  --module ESNext \
  --moduleResolution Bundler \
  --lib ES2022,DOM,DOM.Iterable \
  --types vitest/globals \
  test/integrations/gpt_diagnostics/types.test.ts
npx vitest run \
  test/integrations/gpt_diagnostics/types.test.ts \
  test/integrations/gpt_diagnostics/api.test.ts
```

Expected: the narrow `tsc` command reports missing evidence types and Vitest
reports the missing writer API. Do not substitute repository-wide `tsc`; it has
unrelated baseline failures.

- [ ] **Step 4: Replace the branch-only public types.**

Add these unions and fields to `core/types.ts`:

```ts
export type GptDiagnosticsRequestPath =
  | 'trusted_server_direct'
  | 'prebid_refresh'
  | 'competing'
  | 'unattributed'

export type GptDiagnosticsTrustedServerOpportunity =
  | 'renderable_candidate'
  | 'unrenderable_candidate'
  | 'no_candidate'

export type GptDiagnosticsCreativeFailure =
  | 'missing_render_source'
  | 'cache_fetch_failed'
  | 'invalid_cache_payload'
  | 'response_post_failed'

export type GptDiagnosticsDelivery =
  | 'trusted_server_response_sent'
  | 'trusted_server_selected'
  | 'candidate_unconfirmed'
  | 'no_candidate'
  | 'unknown'
  | 'pending'
  | 'not_applicable'
```

Replace the candidate/claim fields on `GptDiagnosticsRequestCycle` with optional
`requestPath`, `trustedServerOpportunity`, the two creative timestamps, and a
creative-failure array. Define the spec's eight-value attribution-issue reason
union, including `creative_attempt_capacity`. Add optional `attributionIssues` and
`droppedAttributionIssues` fields to the public V1 shape so existing V1 object
literals remain source-compatible; current snapshots will always emit them.

Replace the two old `GptDiagnosticsApi` writers with:

```ts
recordTrustedServerOpportunity(slot, auctionSlotId, opportunity): void;
recordPrebidRefresh(slots): void;
recordTrustedServerCreativeRequest(auctionSlotId): number | undefined;
recordTrustedServerCreativeResponse(attemptId): void;
recordTrustedServerCreativeFailure(attemptId, reason): void;
```

- [ ] **Step 5: Implement no-throw API delegation and detached export.**

Use narrow safety helpers in `api.ts`:

```ts
function safelyRecord(action: () => void): void {
  try {
    action()
  } catch {
    // Diagnostics must not alter delivery.
  }
}

function safelyCreateAttempt(
  action: () => number | undefined
): number | undefined {
  try {
    return action()
  } catch {
    return undefined
  }
}
```

Delegate only through those helpers. Explicitly copy nested identity arrays,
failure arrays, and attribution issues in `snapshot()`; do not rely on a shallow
`{ ...cycle }` for nested values.

- [ ] **Step 6: Run the focused tests and confirm GREEN.**

Run the Step 3 commands. Expected: narrow type-check and both test files pass.

- [ ] **Step 7: Record the checkpoint without committing.**

Confirm only the Task 1 focused tests pass. Continue directly to Task 2 under
the atomic migration rule; do not add temporary store adapters or commit yet.

---

### Task 2: Attribute Request Paths and Derive Delivery Without Guessing

**Files:**

- Modify: `crates/trusted-server-js/lib/src/integrations/gpt_diagnostics/store.ts:1-205,285-380,466-500`
- Test: `crates/trusted-server-js/lib/test/integrations/gpt_diagnostics/store.test.ts`

- [ ] **Step 1: Replace the old candidate/verdict tests with failing table tests.**

Cover:

- direct, Prebid, competing, and unattributed paths;
- all three opportunity values;
- one-shot marker consumption;
- five-second marker expiry at `age >= 5_000`;
- a delayed old expiry callback not deleting a newer generation;
- explicit non-empty candidates moving `pending` → `candidate_unconfirmed` from
  `slotRenderEnded + 5_000`;
- explicit `no_candidate`, no opportunity (`unknown`), omitted `isEmpty`
  (`unknown`), empty and pre-render (`not_applicable`);
- no branch returning or displaying `other_demand`.

- [ ] **Step 2: Run the store tests and confirm RED.**

```bash
cd crates/trusted-server-js/lib
npx vitest run test/integrations/gpt_diagnostics/store.test.ts
```

Expected: failures on removed candidate/claim semantics and missing path markers.

- [ ] **Step 3: Add generation-safe one-shot path markers.**

Add constants and internal shapes:

```ts
export const REQUEST_PATH_ATTRIBUTION_WINDOW_MS = 5_000
export const TRUSTED_SERVER_ATTRIBUTION_WINDOW_MS = 5_000

interface PendingMarker<T> {
  generation: number
  recordedAtMs: number
  value: T
}
```

Replace `pendingCandidates: WeakSet` with two `WeakMap<object, PendingMarker<...>>`
instances. `recordTrustedServerOpportunity` must:

1. runtime-validate the opportunity enum;
2. refresh the bounded auction-slot → GPT-slot association;
3. create one direct marker with a monotonically increasing generation;
4. schedule expiry that deletes only when the stored generation still matches.

It must not mutate an already-open request cycle. That current behavior can
mark both the open cycle and the next cycle.

`recordPrebidRefresh` installs equivalent markers for each valid slot object.
`recordSlotRequested` time-checks and consumes both markers even when a deferred
expiry callback has not run, then records:

```ts
const requestPath =
  direct && prebid
    ? 'competing'
    : direct
      ? 'trusted_server_direct'
      : prebid
        ? 'prebid_refresh'
        : 'unattributed'
```

Only a consumed direct marker supplies `trustedServerOpportunity`.

- [ ] **Step 4: Implement evidence-only response and delivery derivation.**

Correct `responseClass()` first:

```ts
if (cycle.renderAtMs === undefined) return undefined
if (cycle.isEmpty === true) return 'empty'
if (cycle.isEmpty !== false) return undefined
if (cycle.isBackfill === true) return 'backfill'
```

Derive delivery in the approved order:

```ts
if (cycle.trustedServerCreativeResponseAtMs !== undefined)
  return 'trusted_server_response_sent'
if (cycle.trustedServerCreativeRequestAtMs !== undefined)
  return 'trusted_server_selected'
if (cycle.renderAtMs === undefined || cycle.isEmpty === true)
  return 'not_applicable'
if (cycle.isEmpty !== false) return 'unknown'
if (cycle.trustedServerOpportunity === 'no_candidate') return 'no_candidate'
if (cycle.trustedServerOpportunity === undefined) return 'unknown'
return nowMs - cycle.renderAtMs >= TRUSTED_SERVER_ATTRIBUTION_WINDOW_MS
  ? 'candidate_unconfirmed'
  : 'pending'
```

Schedule only one diagnostic re-notification at the observation boundary, and
only for an explicit non-empty render with a renderable or unrenderable
candidate. The timer must never trigger GPT or Prebid work.

- [ ] **Step 5: Run the store tests and confirm GREEN.**

Run the Step 2 command. Expected: all existing lifecycle tests and the new path/
delivery tables pass.

- [ ] **Step 6: Record the checkpoint without committing.**

Continue directly to Task 3. The store's creative writers and the migrated GPT
caller do not exist yet, so this is intentionally not a commit boundary.

---

### Task 3: Correlate Creative Requests and Responses with Bounded Attempts

**Files:**

- Modify: `crates/trusted-server-js/lib/src/integrations/gpt_diagnostics/store.ts:55-90,180-285,295-545,600-630`
- Test: `crates/trusted-server-js/lib/test/integrations/gpt_diagnostics/store.test.ts`

- [ ] **Step 1: Add failing creative-attempt and issue tests.**

Use deterministic `now`, `defer`, and fake GPT slot objects to cover:

- matched request → `trusted_server_selected` → response-sent;
- a creative request/response arriving after candidate timeout upgrading
  `candidate_unconfirmed` rather than being rejected as negative evidence;
- duplicate/retry returning the same live ID without replacing first timestamp;
- unique, first-observed failure ordering, then in-window success;
- an async response remaining on its originating cycle after a newer refresh;
- initial pre-render provisional attachment;
- ambiguous pre-render request when an earlier non-empty cycle remains;
- render with omitted `isEmpty` accepting positive PUC evidence;
- explicit empty rejection and provisional-then-empty inconsistency;
- cycle admission outside 30 seconds;
- missing auction-slot association → `creative_request_without_slot`;
- known slot association without a compatible request cycle →
  `creative_request_without_cycle`;
- attempt expiry with no replacement ID;
- unknown, expired, and evicted attempt IDs;
- request-cycle and slot-LRU eviction invalidation;
- oldest tombstone reclamation at 128 records;
- all-128-live capacity rejection;
- attribution-issue cap/drop metadata without changing the GPT callback coverage
  equation;
- detached failure arrays, attribution issues, `yieldGroupIds`, and `companyIds`.

- [ ] **Step 2: Run the store test and confirm RED.**

Run Task 2 Step 2. Expected: failures for the missing attempt API and issue store.

- [ ] **Step 3: Add the bounded attribution-issue collection.**

Add `MAX_ATTRIBUTION_ISSUES = 128`, a dedicated array, and
`metadata.droppedAttributionIssues`. `addAttributionIssue()` must accept optional
slot identity and never manufacture runtime slot `0`. It must not call
`addIssue()`, increment callback coverage, or change `droppedCallbacks`.

- [ ] **Step 4: Add opaque attempt bookkeeping outside exported cycles.**

Use constants and private records equivalent to:

```ts
export const CREATIVE_ATTEMPT_WINDOW_MS = 30_000
export const MAX_CREATIVE_ATTEMPTS = 128

type AttemptStatus = 'live' | 'completed' | 'expired' | 'evicted'

interface CreativeAttemptRecord {
  id: number
  cycle?: MutableRequestCycle
  runtimeSlotNumber?: number
  slotElementId?: string
  requestedAtMs: number
  expiresAtMs: number
  provisionalBeforeRender: boolean
  status: AttemptStatus
}
```

Keep `cycle → attempt ID` in a `WeakMap`; never add the attempt ID or status to
the exported cycle object. Before admission at capacity, remove the oldest
completed/expired/evicted record. If all records are live, add
`creative_attempt_capacity` and return `undefined`.

- [ ] **Step 5: Implement creative-request admission exactly.**

`recordTrustedServerCreativeRequest(auctionSlotId)` must:

1. expire due live attempts;
2. resolve the bounded auction-slot association and greatest request-number
   cycle;
3. reuse that cycle's existing live attempt before checking new-admission age;
4. return `undefined` for a completed attempt and issue/no-replace for an
   expired or evicted attempt;
5. reject a new attempt when request-cycle age is `> 30_000`;
6. reject an explicitly empty cycle;
7. accept a rendered cycle with `isEmpty !== true`;
8. accept a pre-render cycle only when no earlier retained explicit non-empty
   cycle is still inside 30 seconds;
9. write only the first creative-request timestamp and return the opaque ID.

If a cycle already has `trustedServerCreativeResponseAtMs`, return `undefined`
even when its completed tombstone has been reclaimed. If all attempt records are
live, retain the first positive creative-request timestamp, add the capacity
issue, and return no ID; a later duplicate may retry first-time admission only
because that cycle has never received an attempt ID.

Runtime-validate all JavaScript-callable enum inputs.

- [ ] **Step 6: Implement response, failure, expiry, and eviction transitions.**

- `recordTrustedServerCreativeResponse(id)` sets the first response timestamp
  and completes only a live, unexpired attempt.
- `recordTrustedServerCreativeFailure(id, reason)` validates the four-value
  allowlist and appends each reason once while the attempt remains live.
- Failure is non-terminal; an in-window success may still complete it.
- Completed repeats are idempotent no-ops.
- At attempt age `>= 30_000`, late mutation adds `creative_attempt_expired`.
- Request-cycle shift and slot eviction mark related live attempts evicted before
  removing their cycle.
- A provisional request followed by explicit empty retains the positive request
  timestamp but adds `creative_request_on_empty_cycle`.

Copy failure and nested GAM-ID arrays in `copyCycle()`, and copy attribution
issues in `snapshot()`.

- [ ] **Step 7: Run store, type, and API tests and confirm GREEN.**

```bash
cd crates/trusted-server-js/lib
npx vitest run \
  test/integrations/gpt_diagnostics/store.test.ts \
  test/integrations/gpt_diagnostics/types.test.ts \
  test/integrations/gpt_diagnostics/api.test.ts
```

- [ ] **Step 8: Record the checkpoint without committing.**

Continue directly to Task 4. The new core contract is internally implemented,
but `gpt/index.ts`, Prebid, overlay, badges, and their tests still need migration.

---

### Task 4: Emit Opportunity and Creative-progress Evidence from GPT Integration

**Files:**

- Modify: `crates/trusted-server-js/lib/src/integrations/gpt/index.ts:25-40,650-670,1117-1268`
- Test: `crates/trusted-server-js/lib/test/integrations/gpt/ad_init.test.ts:115-205,1420-1660`

- [ ] **Step 1: Replace the two existing attribution tests with failing staged tests.**

For `adInit`, assert:

- bid targeting plus `hb_adid` and inline `adm` → `renderable_candidate`;
- bid targeting plus `hb_adid` and complete cache coordinates →
  `renderable_candidate`;
- some bid targeting but no usable ID/source → `unrenderable_candidate`;
- no non-empty bid-targeting value → explicit `no_candidate`;
- a throwing diagnostics stub does not stop targeting or the GPT display/refresh.

For the bridge, assert:

- exact-owned request returns/retains the diagnostic attempt ID;
- inline and cache response evidence occurs only after successful `postMessage`;
- missing source, cache HTTP/network failure, invalid payload, and thrown
  `postMessage` record exactly one corresponding safe failure;
- a cache `postMessage` failure is not also classified as a cache-fetch failure;
- another slot/ad ID emits no Trusted Server evidence;
- diagnostics absent or throwing leaves propagation, rendered response, and
  beacon behavior unchanged.

- [ ] **Step 2: Run the GPT integration tests and confirm RED.**

```bash
cd crates/trusted-server-js/lib
npx vitest run test/integrations/gpt/ad_init.test.ts
```

- [ ] **Step 3: Classify every resolved adInit opportunity.**

Add a pure helper using only booleans:

```ts
function trustedServerOpportunity(
  bid: AuctionBidData
): GptDiagnosticsTrustedServerOpportunity {
  const hasBidTargeting = TS_BID_TARGETING_KEYS.some((key) => {
    const value = bid[key]
    return typeof value === 'string' && value.length > 0
  })
  if (!hasBidTargeting) return 'no_candidate'

  const hasAdId = typeof bid.hb_adid === 'string' && bid.hb_adid.length > 0
  const hasInline = typeof bid.adm === 'string' && bid.adm.length > 0
  const hasCache = Boolean(bid.hb_cache_host && bid.hb_cache_path)
  return hasAdId && (hasInline || hasCache)
    ? 'renderable_candidate'
    : 'unrenderable_candidate'
}
```

At the existing call position after targeting, report an opportunity for every
resolved GPT slot, including no-candidate slots. Pass only slot identity,
auction-slot ID, and the enum. Wrap the external diagnostics invocation in
`try/catch`; optional chaining alone does not protect ads from a throwing stub.

- [ ] **Step 4: Split bridge selection from response evidence.**

After the existing iframe-source and exact slot/ad-ID checks:

```ts
const attemptId = safelyCreateDiagnosticsAttempt(slotId)
```

Record `missing_render_source` when neither inline markup nor complete cache
coordinates exist. On inline and cache success, call `port.postMessage()` first,
then safely record the response, then retain the existing beacon/log order.

Catch `postMessage` separately and record only `response_post_failed`. Keep cache
HTTP/network rejection in the outer promise catch as `cache_fetch_failed`, and
record `invalid_cache_payload` on a successful response that cannot produce
`adm`. Diagnostics calls must never throw through the bridge.

- [ ] **Step 5: Run GPT tests and confirm GREEN.**

Run the Step 2 command. Expected: all existing bridge behavior plus staged
diagnostic evidence passes.

- [ ] **Step 6: Record the checkpoint without committing.**

Continue under the atomic migration rule. Do not retain the old candidate/claim
methods merely to create an intermediate commit.

---

### Task 5: Mark Prebid-managed GPT Requests at the Existing Refresh Boundary

**Files:**

- Modify: `crates/trusted-server-js/lib/src/integrations/prebid/index.ts:1171-1315`
- Test: `crates/trusted-server-js/lib/test/integrations/prebid/index.test.ts:991-1547,2000-2760`

- [ ] **Step 1: Add failing refresh-path tests.**

Assert that `recordPrebidRefresh` receives the exact slot-object list immediately
before `originalRefresh` for:

- publisher-delivery refresh without a new auction;
- completed synthetic refresh;
- mixed SRA refresh, including delivery and independent slots;
- synthetic timeout fallback;
- caught synchronous auction failure.

Assert exactly one mark and one GPT refresh when a late callback follows a
timeout. Assert no mark for:

- `adInitRefreshInProgress` bypass;
- empty/invalid passthrough;
- a path that has not reached `originalRefresh` yet.

Use a throwing diagnostics stub to prove the GPT refresh still executes.

- [ ] **Step 2: Run Prebid tests and confirm RED.**

```bash
cd crates/trusted-server-js/lib
npx vitest run test/integrations/prebid/index.test.ts
```

- [ ] **Step 3: Add a narrow safe marker helper.**

```ts
function recordPrebidRefreshForDiagnostics(slots: RefreshGptSlot[]): void {
  try {
    window.tsjs?.gptDiagnostics?.recordPrebidRefresh?.(slots)
  } catch {
    // Diagnostics must not suppress the GAM request.
  }
}
```

Call it only:

1. immediately before `originalRefresh` when `independentSlots.length === 0`;
2. inside `completeRefresh`, after optional targeting and immediately before the
   one `originalRefresh`.

Pass all `targetSlots`, not only `independentSlots`, so mixed SRA requests have
one accurate path marker per GPT slot. Do not touch the adInit bypass or invalid
passthrough branches.

- [ ] **Step 4: Run Prebid tests and confirm GREEN.**

Run the Step 2 command. Expected: all current refresh behavior and new marker
ordering tests pass.

- [ ] **Step 5: Record the checkpoint without committing.**

Continue to Task 6 so the presentation and documentation migrate before the
atomic implementation commit.

---

### Task 6: Present and Document the Evidence Ladder

**Files:**

- Modify: `crates/trusted-server-js/lib/src/integrations/gpt_diagnostics/overlay.ts:115-210,450-480`
- Modify: `crates/trusted-server-js/lib/src/integrations/gpt_diagnostics/badges.ts:76-105`
- Modify: `docs/guide/integrations/gpt-diagnostics.md:9-105,200-235`
- Test: `crates/trusted-server-js/lib/test/integrations/gpt_diagnostics/overlay.test.ts:100-145`
- Test: `crates/trusted-server-js/lib/test/integrations/gpt_diagnostics/badges.test.ts:118-205`

- [ ] **Step 1: Rewrite the overclaiming UI tests to fail safely.**

Require the exact delivery wording from the spec and facts for:

- direct, Prebid, competing, and unattributed request paths;
- renderable, unrenderable, explicit-no-candidate, and unknown opportunities;
- selected, response-sent, pending, candidate-unconfirmed, no-candidate,
  unknown, and not-applicable states;
- all four safe bridge failures;
- GPT slot onload as an observed fact, never pixel proof;
- attribution-issue count separate from callback-issue count.

Explicitly reject these strings everywhere:

```text
creative rendered
other demand won
no Trusted Server creative ran
```

Update badge expectations to compact labels such as `TS response sent`,
`TS selected`, `TS candidate`, `TS unconfirmed`, and `Competing paths`; do not
label a GAM line-item ID as “other demand.”

- [ ] **Step 2: Run UI tests and confirm RED.**

```bash
cd crates/trusted-server-js/lib
npx vitest run \
  test/integrations/gpt_diagnostics/overlay.test.ts \
  test/integrations/gpt_diagnostics/badges.test.ts
```

- [ ] **Step 3: Implement evidence-safe overlay and badge facts.**

Keep GAM IDs source-neutral. Show request path, opportunity, creative request,
response, deduplicated failures, and delivery as independent lines. If a TS
response and `slotOnload` are both present, say “GPT slot onload observed”; do
not say the inner creative rendered or became visible.

Change the summary to report both bounded issue collections, for example:

```ts
const summaryText = `${slots} slots · ${callbackIssues} callback issues · ${attributionIssues} attribution issues`
```

- [ ] **Step 4: Rewrite operator documentation.**

Document:

- zero publisher-code change and existing `?ts_console=true` activation;
- request-path meanings and the “possible overwrite, not proven winner” limit;
- candidate → PUC request → response sent → GPT onload evidence ladder;
- why absence becomes `candidate_unconfirmed` or `unknown`, never other demand;
- five-second path/delivery windows, 30-second attempt lifetime, and bounds;
- safe failure enums and exported attribution issues;
- no raw targeting, bid IDs, prices, markup, cache URLs, or upload;
- no pixel-level proof without a controlled creative acknowledgement.

- [ ] **Step 5: Run focused UI and docs formatting checks.**

```bash
cd crates/trusted-server-js/lib
npx vitest run \
  test/integrations/gpt_diagnostics/overlay.test.ts \
  test/integrations/gpt_diagnostics/badges.test.ts
npm run format

cd ../../../docs
npm run format
```

Expected: tests and both formatting checks pass.

- [ ] **Step 6: Run the complete affected suite at the atomic boundary.**

```bash
cd crates/trusted-server-js/lib
npx tsc --noEmit \
  --skipLibCheck \
  --target ES2022 \
  --module ESNext \
  --moduleResolution Bundler \
  --lib ES2022,DOM,DOM.Iterable \
  --types vitest/globals \
  test/integrations/gpt_diagnostics/types.test.ts
npx vitest run \
  test/integrations/gpt_diagnostics \
  test/integrations/gpt/ad_init.test.ts \
  test/integrations/prebid/index.test.ts
npm run lint
npm run format
```

Expected: the narrow public-contract type-check, every affected suite, lint, and
format pass. This is the first implementation commit boundary.

- [ ] **Step 7: Commit the atomic contract/provider/consumer migration.**

```bash
git add \
  crates/trusted-server-js/lib/src/core/types.ts \
  crates/trusted-server-js/lib/src/integrations/gpt/index.ts \
  crates/trusted-server-js/lib/src/integrations/prebid/index.ts \
  crates/trusted-server-js/lib/src/integrations/gpt_diagnostics/api.ts \
  crates/trusted-server-js/lib/src/integrations/gpt_diagnostics/store.ts \
  crates/trusted-server-js/lib/src/integrations/gpt_diagnostics/overlay.ts \
  crates/trusted-server-js/lib/src/integrations/gpt_diagnostics/badges.ts \
  crates/trusted-server-js/lib/test/integrations/gpt/ad_init.test.ts \
  crates/trusted-server-js/lib/test/integrations/prebid/index.test.ts \
  crates/trusted-server-js/lib/test/integrations/gpt_diagnostics/types.test.ts \
  crates/trusted-server-js/lib/test/integrations/gpt_diagnostics/api.test.ts \
  crates/trusted-server-js/lib/test/integrations/gpt_diagnostics/store.test.ts \
  crates/trusted-server-js/lib/test/integrations/gpt_diagnostics/overlay.test.ts \
  crates/trusted-server-js/lib/test/integrations/gpt_diagnostics/badges.test.ts \
  docs/guide/integrations/gpt-diagnostics.md
git commit -m "Report GPT delivery as an evidence ladder"
```

---

### Task 7: Full Verification and Branch Review

**Files:** No intended source changes. Fix only failures caused by Tasks 1-6.

- [ ] **Step 1: Run focused affected suites together.**

```bash
cd crates/trusted-server-js/lib
npx vitest run \
  test/integrations/gpt_diagnostics \
  test/integrations/gpt/ad_init.test.ts \
  test/integrations/prebid/index.test.ts
```

Expected: all affected suites pass with no unhandled promise rejection.

- [ ] **Step 2: Run the complete JavaScript gates.**

```bash
cd crates/trusted-server-js/lib
npx tsc --noEmit \
  --skipLibCheck \
  --target ES2022 \
  --module ESNext \
  --moduleResolution Bundler \
  --lib ES2022,DOM,DOM.Iterable \
  --types vitest/globals \
  test/integrations/gpt_diagnostics/types.test.ts
npx vitest run
npm run lint
npm run format
node build-all.mjs
```

Expected: all tests, lint, format, and every generated bundle build pass.

- [ ] **Step 3: Run documentation gates.**

```bash
cd docs
npm run format
npm run lint
npm run build
```

Expected: docs format, lint, and site build pass.

- [ ] **Step 4: Run the repository CI gates required by `CLAUDE.md`.**

```bash
cargo fmt --all -- --check
cargo clippy-fastly
cargo clippy-axum
cargo clippy-cloudflare
cargo clippy-cloudflare-wasm
cargo clippy-spin-native
cargo clippy-spin-wasm
cargo test-fastly
cargo test-axum
cargo test-cloudflare
cargo test-spin
cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test parity
```

Expected: every command passes. Do not replace these target-matched aliases with
bare `cargo test --workspace`.

- [ ] **Step 5: Review the final branch delta.**

```bash
git diff --check
git status --short
git diff --stat origin/main...HEAD
git log --oneline origin/main..HEAD
```

Confirm:

- no publisher/GAM/React changes;
- no raw bid or creative data in diagnostics exports;
- no negative winner inference;
- no diagnostics exception can alter a GPT refresh or bridge response;
- the existing GAM identity observer remains intact;
- only expected source, test, plan/spec, and guide files changed.

- [ ] **Step 6: Request code review using `superpowers:requesting-code-review`.**

Resolve any correctness issue, rerun the affected focused suite, then rerun the
relevant final gates.

- [ ] **Step 7: Commit any verification-only corrections.**

Use a narrow message describing the correction; do not squash implementation
history unless requested.
