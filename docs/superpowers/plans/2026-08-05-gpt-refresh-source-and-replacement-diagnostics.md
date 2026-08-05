# GPT Refresh Source and Replacement Diagnostics Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Extend opt-in GPT diagnostics to identify observed publisher refreshes, correlate Trusted Server request intent and auction IDs, and report when a later filled request replaces an earlier rendered ad.

**Architecture:** Consolidate short-lived request-source markers into one per-GPT-slot intent containing independently expiring source evidence. Instrument only existing GPT and Prebid refresh boundaries, using synchronous metadata context to avoid false publisher attribution from nested wrappers, then derive replacement facts from retained GPT render cycles. All hooks are no-throw observations and preserve refresh arguments, timing, targeting, suppression, and callback behavior.

**Tech Stack:** TypeScript, GPT event APIs, Prebid integration wrappers, Vitest 4, jsdom, ESLint, Prettier, Vite.

**Design:** `docs/superpowers/specs/2026-08-05-gpt-refresh-source-and-replacement-diagnostics-design.md`

---

## File Map

- `crates/trusted-server-js/lib/src/core/types.ts`: public paths, cycle facts, API, and private dispatch context.
- `crates/trusted-server-js/lib/src/integrations/gpt_diagnostics/store.ts`: intents, expiry, classification, latency, and replacement derivation.
- `crates/trusted-server-js/lib/src/integrations/gpt_diagnostics/api.ts`: safe publisher-intent and auction-ID forwarding.
- `crates/trusted-server-js/lib/src/integrations/gpt_diagnostics/overlay.ts`: new factual labels.
- `crates/trusted-server-js/lib/src/integrations/gpt/index.ts`: publisher refresh observation and TS auction-ID forwarding.
- `crates/trusted-server-js/lib/src/integrations/prebid/index.ts`: nested Prebid dispatch context.
- `crates/trusted-server-js/lib/test/integrations/{gpt_diagnostics,gpt,prebid}`: unit and integration coverage.
- `docs/guide/integrations/gpt-diagnostics.md`: evidence meanings and limitations.

For JS commands use Node 24.12.0 and run from the JS package:

```bash
export PATH=/Users/prk-jr/.nvm/versions/node/v24.12.0/bin:$PATH
cd crates/trusted-server-js/lib
```

## Task 1: Request-intent types, store, and safe API

**Files:**

- Modify: `crates/trusted-server-js/lib/src/core/types.ts`
- Modify: `crates/trusted-server-js/lib/src/integrations/gpt_diagnostics/store.ts`
- Modify: `crates/trusted-server-js/lib/src/integrations/gpt_diagnostics/api.ts`
- Test: `crates/trusted-server-js/lib/test/integrations/gpt_diagnostics/store.test.ts`
- Test: `crates/trusted-server-js/lib/test/integrations/gpt_diagnostics/api.test.ts`
- Test: `crates/trusted-server-js/lib/test/integrations/gpt_diagnostics/types.test.ts`

- [ ] **Step 1: Write failing type and API tests.** Add `publisher_refresh`, `requestIntentId`, `trustedServerAuctionId`, and `opportunityToRequestMs` to a typed request-cycle fixture. Assert exact forwarding of:

```ts
controller.api.recordTrustedServerOpportunity(
  slot,
  'slot-a',
  'renderable_candidate',
  'auction-a'
)
controller.api.recordPublisherRefresh([slot])
```

Retain the tests proving thrown diagnostic methods are swallowed.

- [ ] **Step 2: Write failing store tests.** With a fake clock and captured deferred callbacks, cover TS-only, Prebid-only, publisher-only, every two-source combination, all-three classification, repeated same-source evidence, monotonic intent IDs, one-shot consumption, per-source expiry, partial expiry, generation safety, and stale-source filtering at consumption time. Delay the expiry callback past five seconds, then record a new source: assert the old final source is pruned, cannot make the request `competing`, and the new evidence receives a new intent ID. Verify a repeated TS observation replaces its opportunity/timestamp/auction ID and an absent replacement ID clears the stale one.

Validate the exact auction-ID boundary: surrounding whitespace is trimmed and
the trimmed value exported; exactly 256 UTF-8 bytes is accepted; 257 ASCII bytes
and a multibyte value over 256 UTF-8 bytes are rejected; non-string and blank
values are omitted; every invalid ID still preserves TS intent. Prove latency
starts at TS observation rather than another source, and that negative or
non-finite opportunity-to-request durations are omitted:

```ts
store.recordTrustedServerOpportunity(
  slot,
  'slot-a',
  'renderable_candidate',
  'auction-a'
)
nowMs = 100
store.recordPublisherRefresh([slot])
nowMs = 124
store.recordSlotRequested(slot)
expect(latestCycle(store)).toMatchObject({
  requestPath: 'competing',
  trustedServerAuctionId: 'auction-a',
  opportunityToRequestMs: 124,
})
```

- [ ] **Step 3: Run focused tests and verify RED.**

```bash
npx vitest run test/integrations/gpt_diagnostics/store.test.ts \
  test/integrations/gpt_diagnostics/api.test.ts \
  test/integrations/gpt_diagnostics/types.test.ts
```

Expected: FAIL on missing path, API method/argument, and cycle fields.

- [ ] **Step 4: Add types.** Extend `GptDiagnosticsRequestPath`, `GptDiagnosticsRequestCycle`, and `GptDiagnosticsApi`. Add a documented private `TsjsApi.prebidRefreshDispatchInProgress?: boolean`; production code may use it only to decide diagnostic attribution.

- [ ] **Step 5: Implement per-slot intent storage.** Replace the two marker maps with:

```ts
type RequestIntentSource =
  | 'trusted_server_direct'
  | 'prebid_refresh'
  | 'publisher_refresh'
interface PendingSourceEvidence {
  generation: number
  observedAtMs: number
  trustedServerOpportunity?: GptDiagnosticsTrustedServerOpportunity
  trustedServerAuctionId?: string
}
interface PendingRequestIntent {
  intentId: number
  sources: Map<RequestIntentSource, PendingSourceEvidence>
}
```

Use one `WeakMap<object, PendingRequestIntent>`. Before recording new evidence,
prune every age-expired source synchronously and remove an intent whose final
source expired, even if its timer callback has not run. Create an ID only when
no active intent remains; replace and restart expiry only for the recorded
source; delete the intent only on consumption or final-source expiry; check age
again on consumption; classify one source directly, two or more as `competing`,
and none as `unattributed`. Add a delayed-timer test where the old final source
is older than five seconds when a new source is recorded: stale evidence must
not classify, and the new observation receives a new monotonic intent ID. Keep
the bounded creative association separate. Trim IDs and enforce the exact
256-byte UTF-8 boundary above.

- [ ] **Step 6: Forward API inputs safely.** Extend `ApiStore` and `InstalledGptDiagnosticsApi`; pass the optional auction ID and route `recordPublisherRefresh` through `safelyRecord`.

- [ ] **Step 7: Run the Task 1 tests and verify GREEN.** Expected: all selected tests pass, including existing request-path and creative-attribution cases.

- [ ] **Step 8: Commit.**

```bash
git add crates/trusted-server-js/lib/src/core/types.ts \
  crates/trusted-server-js/lib/src/integrations/gpt_diagnostics/store.ts \
  crates/trusted-server-js/lib/src/integrations/gpt_diagnostics/api.ts \
  crates/trusted-server-js/lib/test/integrations/gpt_diagnostics/{store,api,types}.test.ts
git commit -m "feat: add GPT diagnostic request intents"
```

## Task 2: GPT publisher-refresh and TS auction instrumentation

**Files:**

- Modify: `crates/trusted-server-js/lib/src/integrations/gpt/index.ts`
- Test: `crates/trusted-server-js/lib/test/integrations/gpt/ad_init.test.ts`

- [ ] **Step 1: Write failing GPT boundary tests.** Prove an explicit external refresh records exactly its delegated slots; a bare refresh records resolved slots while preserving an `undefined` native argument when unfiltered; a filtered refresh records/forwards only remaining slots; and a fully suppressed refresh records/calls nothing. Assert exact options identity, slot identity/order, call count, synchronous throw propagation, and invocation order. Missing or throwing diagnostics must not affect delivery. Internal TS and Prebid-context calls must not record publisher intent. Reinstallation must not double-wrap.

- [ ] **Step 2: Write a failing auction-ID test.** Give `adInit` a bid with `hb_auction_id: 'auction-from-server'` and expect:

```ts
expect(recordTrustedServerOpportunity).toHaveBeenCalledWith(
  resolvedSlot,
  auctionSlotId,
  'renderable_candidate',
  'auction-from-server'
)
```

Cover absent ID and throwing diagnostics without changing refresh behavior.

- [ ] **Step 3: Run filtered GPT tests and verify RED.**

```bash
npx vitest run test/integrations/gpt/ad_init.test.ts \
  -t "publisher refresh diagnostics|auction ID diagnostics"
```

- [ ] **Step 4: Implement fail-closed observation.** Add `recordPublisherRefreshForDiagnostics(slots)` with `try/catch`. In the handoff wrapper, call it only for the exact concrete list immediately about to be delegated. Skip internal TS calls, Prebid-context calls, unresolved bare calls, empty lists, and fully suppressed calls. Do not change explicit-versus-bare arguments or options when no filtering occurs.

- [ ] **Step 5: Forward `bid.hb_auction_id`.** Add it only as the fourth argument to `recordTrustedServerOpportunity` inside the existing diagnostic `try/catch`.

- [ ] **Step 6: Run the complete GPT ad-init test file.**

```bash
npx vitest run test/integrations/gpt/ad_init.test.ts
```

Expected: PASS, including all existing handoff, suppression, APS, cache, and render tests.

- [ ] **Step 7: Commit.**

```bash
git add crates/trusted-server-js/lib/src/integrations/gpt/index.ts \
  crates/trusted-server-js/lib/test/integrations/gpt/ad_init.test.ts
git commit -m "feat: observe publisher GPT refresh intent"
```

## Task 3: Prebid nested-dispatch attribution

**Files:**

- Modify: `crates/trusted-server-js/lib/src/integrations/prebid/index.ts`
- Test: `crates/trusted-server-js/lib/test/integrations/prebid/index.test.ts`
- Test: `crates/trusted-server-js/lib/test/integrations/gpt/ad_init.test.ts`

- [ ] **Step 1: Write failing Prebid context tests.** Have native refresh observe the context. It must be true only for publisher-delivery, completed synthetic auction, timeout watchdog, and caught auction-failure delegation. It remains false for adInit bypass, empty/invalid passthrough, unresolved bare passthrough, fully excluded passthrough, and the wait before auction completion. Preserve all existing exact slots/options/call-count/targeting assertions.

- [ ] **Step 2: Write failing exception/order tests.** A native synchronous throw restores the previous flag and rethrows the same error; a previous true value is restored. The real GPT-then-Prebid install sequence must produce one Prebid marker and zero publisher markers. Reinstalling either integration must preserve order and avoid duplicate markers.

- [ ] **Step 3: Run focused tests and verify RED.**

```bash
npx vitest run test/integrations/prebid/index.test.ts \
  test/integrations/gpt/ad_init.test.ts \
  -t "refresh diagnostics|dispatch context|installer order"
```

- [ ] **Step 4: Implement exception-safe dispatch metadata.** Add:

```ts
function withPrebidRefreshDispatch<T>(callback: () => T): T {
  const ts = window.tsjs
  if (!ts) return callback()
  const previous = ts.prebidRefreshDispatchInProgress
  ts.prebidRefreshDispatchInProgress = true
  try {
    return callback()
  } finally {
    ts.prebidRefreshDispatchInProgress = previous
  }
}
```

Pair this helper with every `recordPrebidRefreshForDiagnostics(targetSlots)` immediately around its corresponding `originalRefresh`. Do not use it on passthrough paths that do not record Prebid intent. Do not move auction callbacks, targeting, timers, or completion guards.

- [ ] **Step 5: Run complete Prebid and GPT tests.**

```bash
npx vitest run test/integrations/prebid/index.test.ts \
  test/integrations/gpt/ad_init.test.ts
```

Expected: PASS with Prebid-only attribution through real wrapper order.

- [ ] **Step 6: Commit.**

```bash
git add crates/trusted-server-js/lib/src/integrations/prebid/index.ts \
  crates/trusted-server-js/lib/test/integrations/prebid/index.test.ts \
  crates/trusted-server-js/lib/test/integrations/gpt/ad_init.test.ts
git commit -m "feat: distinguish nested Prebid refresh dispatch"
```

## Task 4: Rendered replacement derivation and presentation

**Files:**

- Modify: `crates/trusted-server-js/lib/src/core/types.ts`
- Modify: `crates/trusted-server-js/lib/src/integrations/gpt_diagnostics/store.ts`
- Modify: `crates/trusted-server-js/lib/src/integrations/gpt_diagnostics/overlay.ts`
- Test: `crates/trusted-server-js/lib/test/integrations/gpt_diagnostics/store.test.ts`
- Test: `crates/trusted-server-js/lib/test/integrations/gpt_diagnostics/overlay.test.ts`
- Test: `crates/trusted-server-js/lib/test/integrations/gpt_diagnostics/types.test.ts`

- [ ] **Step 1: Write failing replacement tests.** The current non-empty render finds the most recent earlier retained non-empty render, skipping empty/unrendered cycles. Assert request number, valid previous-render-to-current-request duration, previous creative ID, and changed/unchanged status. Cover missing one/both IDs, precedence of `creativeId` over `sourceAgnosticCreativeId`, empty current render, negative duration, and evicted history.

- [ ] **Step 2: Write failing overlay/type tests.** Assert exact conditional facts:

```text
Request path: Publisher refresh
Request intent: 7
Trusted Server auction: auction-7
Opportunity → request 24 ms
Replaced rendered request 1 after 6048 ms
Creative changed 138563319574 → 138562551425
```

Equal IDs show `Creative unchanged <ID>`; a missing ID omits the comparison.

- [ ] **Step 3: Run diagnostics tests and verify RED.**

```bash
npx vitest run test/integrations/gpt_diagnostics/store.test.ts \
  test/integrations/gpt_diagnostics/overlay.test.ts \
  test/integrations/gpt_diagnostics/types.test.ts
```

- [ ] **Step 4: Add fields and derive replacements.** Add `replacedRequestNumber?: number`, `previousRenderToRequestMs?: number`, `previousCreativeId?: number`, and `creativeChanged?: boolean`. Before mutating the current cycle in `recordSlotRenderEnded`, find the latest earlier `renderAtMs !== undefined && isEmpty === false`. Only a current `isEmpty === false` records a relationship. Use `validDuration(previous.renderAtMs, cycle.requestedAtMs)` and `creativeId ?? sourceAgnosticCreativeId`; compare only when both IDs exist.

- [ ] **Step 5: Add factual overlay lines.** Extend `requestPathFact`; append optional intent, auction, opportunity latency, replacement, and creative-transition facts using the existing millisecond formatter. Do not infer missing values.

- [ ] **Step 6: Run all diagnostics tests.**

```bash
npx vitest run test/integrations/gpt_diagnostics
```

Expected: PASS across store, API, observer, binding, overlay, badges, and activation.

- [ ] **Step 7: Commit.**

```bash
git add crates/trusted-server-js/lib/src/core/types.ts \
  crates/trusted-server-js/lib/src/integrations/gpt_diagnostics/{store,overlay}.ts \
  crates/trusted-server-js/lib/test/integrations/gpt_diagnostics/{store,overlay,types}.test.ts
git commit -m "feat: report GPT rendered ad replacements"
```

## Task 5: Documentation and full verification

**Files:**

- Modify: `docs/guide/integrations/gpt-diagnostics.md`

- [ ] **Step 1: Update the guide.** Document publisher-boundary meaning, nested Prebid attribution, retained-old-function limitations, correlation-not-winner semantics, rendered replacement versus GPT's `slotContentChanged`, and the absence of delivery changes.

- [ ] **Step 2: Run the full JS suite.**

```bash
npx vitest run
npm run lint
npm run format
npm run build
```

Expected: tests, ESLint, Prettier, and build all pass; build emits every expected module.

Then format-check the separately managed documentation package from the
repository root:

```bash
cd docs && npm run format
```

Expected: the guide and all docs pass Prettier.

- [ ] **Step 3: Run repository CI gates from the repository root.** Run sequentially:

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

Expected: every command exits 0. Never use bare `cargo test --workspace`.

- [ ] **Step 4: Inspect the final behavioral diff.**

```bash
git diff --check
git status --short
git diff --stat HEAD~4..HEAD
```

Confirm new production state affects only diagnostic recording; request, targeting, slot selection, timing, suppression, and creative behavior are unchanged.

- [ ] **Step 5: Commit documentation.**

```bash
git add docs/guide/integrations/gpt-diagnostics.md
git commit -m "docs: explain GPT refresh replacement diagnostics"
```

- [ ] **Step 6: Request final review and re-verify.** Use `@superpowers:requesting-code-review` with the design, plan, diff, and verification evidence. Resolve only verified findings, rerun affected tests, then use `@superpowers:verification-before-completion` before claiming completion.
