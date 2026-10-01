# GPT Refresh Diagnostics Port Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Extend the opt-in GPT diagnostics with per-slot request-intent source attribution, Trusted Server auction-ID correlation, observed rendered-replacement reporting, and a behavior-preserving publisher refresh observer.

**Architecture:** Replace the store's independent Trusted Server and Prebid pending markers with one identity-keyed, generation-safe request intent containing independently expiring source evidence. Install a standalone diagnostics refresh observer before the existing Prebid wrapper; the shared `TsjsApi.prebidRefreshDispatchInProgress` context makes a nested Prebid refresh Prebid-only, while `adInitRefreshInProgress` continues to exclude internal Trusted Server refreshes. Carry only this branch's string `AuctionRequest.id` through optional `hb_auction_id` winning-bid metadata on initial and page-bids response paths, then expose its opaque value in diagnostics.

**Tech Stack:** Rust 2024, `serde_json`, TypeScript, Vitest, GPT `googletag.cmd`/`PubAdsService`, existing TSJS IIFE bundles.

---

## Scope and branch constraints

- Implement diagnostics only. Do not change auction selection, targeting, markup,
  request timing, callback matching except for the approved response-gated
  `slotOnload` correction, retention limits, or publisher integration behavior.
- This branch has no RC tracing handoff, July handoff, or APS renderer data. Do not import or recreate those concepts; use only `AuctionRequest.id` as the opaque optional `hb_auction_id` diagnostic metadata.
- Preserve the public diagnostics API as fail-open/no-throw. Do not add a public publisher-refresh method.
- Preserve exact `pubads.refresh` behavior: receiver, argument count (including no arguments versus an explicit `undefined`), argument identity, result, synchronous exception, and one original invocation.

## File map

| File                                                                                                              | Responsibility                                                                                                   |
| ----------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------- |
| `crates/trusted-server-js/lib/src/core/types.ts`                                                                  | Public bid and diagnostics request-cycle/API types, including shared Prebid dispatch context.                    |
| `crates/trusted-server-js/lib/src/integrations/gpt_diagnostics/store.ts`                                          | Intent storage, classification, correlation fields, and replacement derivation.                                  |
| `crates/trusted-server-js/lib/src/integrations/gpt_diagnostics/api.ts`                                            | Safe forwarding of the optional auction ID.                                                                      |
| `crates/trusted-server-js/lib/src/integrations/gpt_diagnostics/observer.ts`                                       | GPT listeners plus standalone idempotent publisher refresh observer.                                             |
| `crates/trusted-server-js/lib/src/integrations/gpt_diagnostics/index.ts`                                          | Install the observer before deferred Prebid installation can wrap it.                                            |
| `crates/trusted-server-js/lib/src/integrations/prebid/index.ts`                                                   | Synchronous scoped Prebid diagnostic dispatch context surrounding the delegated refresh.                         |
| `crates/trusted-server-js/lib/src/integrations/gpt/index.ts`                                                      | Forward `bid.hb_auction_id` from `adInit` to diagnostics.                                                        |
| `crates/trusted-server-js/lib/src/integrations/gpt_diagnostics/overlay.ts`                                        | Render source, intent, latency, auction, and replacement facts only when present.                                |
| `crates/trusted-server-js/lib/test/integrations/gpt_diagnostics/{store,api,observer,overlay,index,types}.test.ts` | TDD coverage for store semantics, API/type/export shape, wrapper behavior, installation order, and presentation. |
| `crates/trusted-server-js/lib/test/integrations/{gpt/ad_init,prebid/index}.test.ts`                               | TDD coverage for auction-ID forwarding and Prebid context/refresh transparency.                                  |
| `crates/trusted-server-core/src/publisher.rs`                                                                     | Copy `AuctionRequest.id` into optional winner metadata for initial and page-bids bid maps.                       |
| `docs/guide/integrations/gpt-diagnostics.md`                                                                      | Operator-facing source semantics, privacy, replacement, retention, and API documentation.                        |

### Task 1: Define the exported contracts first

**Files:**

- Modify: `crates/trusted-server-js/lib/src/core/types.ts`
- Modify: `crates/trusted-server-js/lib/test/integrations/gpt_diagnostics/types.test.ts`

- [ ] **Step 1: Write failing public-type/export fixture assertions.**

  Add a complete V1 snapshot fixture containing `publisher_refresh`, `requestIntentId`, `trustedServerAuctionId`, `opportunityToRequestMs`, `replacedRequestNumber`, `previousRenderToRequestMs`, `previousCreativeId`, and `creativeChanged`; assert omitted optional fields stay absent in the serialized/typed shape.

- [ ] **Step 2: Run the focused type test and confirm it fails.**

  Run: `cd crates/trusted-server-js/lib && npx vitest run test/integrations/gpt_diagnostics/types.test.ts`

  Expected: FAIL because the new union member and cycle fields are missing.

- [ ] **Step 3: Extend the smallest shared contracts.**

  In `core/types.ts`:

  ```ts
  export interface AuctionBidData {
    // existing fields
    hb_auction_id?: string;
  }

  export type GptDiagnosticsRequestPath =
    | 'trusted_server_direct'
    | 'prebid_refresh'
    | 'publisher_refresh'
    | 'competing'
    | 'unattributed';

  export interface GptDiagnosticsRequestCycle {
    // existing fields
    requestIntentId?: number;
    trustedServerAuctionId?: string;
    opportunityToRequestMs?: number;
    replacedRequestNumber?: number;
    previousRenderToRequestMs?: number;
    creativeChanged?: boolean;
    previousCreativeId?: GptDiagnosticsAdManagerIdentity['creativeId'];
    loadObservedBeforeRender?: boolean;
  }

  recordTrustedServerOpportunity?(
    slot: GptDiagnosticsSlotHandle,
    auctionSlotId: string,
    opportunity: GptDiagnosticsTrustedServerOpportunity,
    trustedServerAuctionId?: string
  ): void;
  ```

  Reuse the existing numeric GPT creative-ID type; do not widen it to arbitrary strings. Add optional `prebidRefreshDispatchInProgress?: boolean` to shared `TsjsApi`, rather than a bundle-local window cast, so diagnostics and Prebid IIFEs share one defensive context contract.

- [ ] **Step 4: Run the focused type test and confirm it passes.**

  Run: `cd crates/trusted-server-js/lib && npx vitest run test/integrations/gpt_diagnostics/types.test.ts`

  Expected: PASS.

- [ ] **Step 5: Commit the contract-only change.**

  ```bash
  git add crates/trusted-server-js/lib/src/core/types.ts crates/trusted-server-js/lib/test/integrations/gpt_diagnostics/types.test.ts
  git commit -m "Extend GPT diagnostics request contracts"
  ```

### Task 2: Correct response-gated `slotOnload` attribution, then add request intents

**Files:**

- Modify: `crates/trusted-server-js/lib/src/integrations/gpt_diagnostics/store.ts`
- Modify: `crates/trusted-server-js/lib/test/integrations/gpt_diagnostics/store.test.ts`

- [ ] **Step 1: Write the failing normal early-load regression test.**

  In `store.test.ts`, create exactly one response-bearing request cycle, call `recordSlotOnload` before `recordSlotRenderEnded`, then render it. Assert the unique cycle receives `loadAtMs` and `loadObservedBeforeRender: true`; `durations.renderToLoadMs` is absent; `incompleteSequence` remains `false`; and no `slotOnload` `invalid_event_order` issue exists.

- [ ] **Step 2: Write the failing ambiguity and no-response guard tests.**

  Assert `slotOnload` remains unmatched when no candidate has `responseAtMs`, and remains ambiguous when two compatible response-bearing, not-yet-loaded cycles exist. Keep the existing unmatched/ambiguous coverage equation and do not change it merely to accept early loads.

- [ ] **Step 3: Run the focused early-load tests and confirm they fail.**

  Run: `cd crates/trusted-server-js/lib && npx vitest run test/integrations/gpt_diagnostics/store.test.ts`

  Expected: FAIL because the current matcher requires a render and marks early load as unmatched.

- [ ] **Step 4: Implement the smallest response-gated matcher change.**

  Select only cycles with `responseAtMs !== undefined` and no prior load, whether or not render has occurred. When load precedes render, set `loadObservedBeforeRender: true` without an invalid-event-order issue or incomplete flag. When a later render arrives, preserve that flag and deliberately omit `renderToLoadMs`; retain the existing valid render-before-load duration, and leave ambiguity handling unchanged.

- [ ] **Step 5: Run the focused early-load tests and confirm they pass.**

  Run: `cd crates/trusted-server-js/lib && npx vitest run test/integrations/gpt_diagnostics/store.test.ts`

  Expected: PASS, including normal render-before-load and callback-coverage cases.

- [ ] **Step 6: Add failing classification and consumption tests.**

  Cover TS-only, Prebid-only, publisher-only, every two-source combination, all three sources, repeated same-source recording, no evidence, and one-shot consumption. Assert the consumed intent ID is strictly increasing and only present when evidence existed.

- [ ] **Step 7: Run the focused intent classification test and confirm it fails.**

  Run: `cd crates/trusted-server-js/lib && npx vitest run test/integrations/gpt_diagnostics/store.test.ts`

  Expected: FAIL because marker-only storage cannot represent publisher evidence or a single consumed intent ID.

- [ ] **Step 8: Implement minimal intent classification and one-shot consumption.**

  Add the intent map and recording methods described below, consume the complete intent in `slotRequested`, and classify only from its source set. Do not implement expiry metadata until the next red test is in place.

- [ ] **Step 9: Run the focused intent classification test and confirm it passes.**

  Run: `cd crates/trusted-server-js/lib && npx vitest run test/integrations/gpt_diagnostics/store.test.ts`

  Expected: PASS.

- [ ] **Step 10: Add failing expiry/metadata tests.**

  Use injected `now` and `defer` to prove: each source has an independent five-second timer; expiry of one source retains another; stale deferred generations cannot remove a replacement or unrelated source; a final expiry deletes the intent; repeated TS evidence replaces timestamp/opportunity/auction ID; and an omitted replacement auction ID clears a stale one. Test trimmed non-empty UTF-8 values up to 256 bytes are retained; empty, whitespace-only, non-string, and over-256-byte IDs are omitted without discarding TS evidence.

- [ ] **Step 11: Run the focused expiry/metadata test and confirm it fails.**

  Run: `cd crates/trusted-server-js/lib && npx vitest run test/integrations/gpt_diagnostics/store.test.ts`

  Expected: FAIL because the first intent implementation does not yet perform independent generation-safe expiry or auction-ID normalization.

- [ ] **Step 12: Implement independent expiry and Trusted Server metadata.**

  Replace `pendingTrustedServerOpportunities` and `pendingPrebidRefreshes` with a single `WeakMap<object, PendingRequestIntent>` and diagnostic-only monotonic `nextRequestIntentId`. Keep Trusted Server slot association for the existing creative-attempt flow, but record its next-request source into the intent. Add an internal-only `recordPublisherRefresh(slots)` used by the observer.

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

  On each recording, create or augment one intent without resetting other sources; increment only that source generation, schedule only that source expiry, and compare both intent identity and generation before deletion. Derive `opportunityToRequestMs` only from valid non-negative Trusted Server observation time. Do not call GPT or publisher code from store recording.

- [ ] **Step 13: Run the focused expiry/metadata test and confirm it passes.**

  Run: `cd crates/trusted-server-js/lib && npx vitest run test/integrations/gpt_diagnostics/store.test.ts`

  Expected: PASS.

- [ ] **Step 14: Add failing replacement tests.**

  Build cycles through the public store methods. Verify a non-empty current render selects the most recent earlier retained non-empty render, stores its request number and valid render-to-current-request duration, carries prior creative ID, reports `creativeChanged: true`/`false` only with two GPT creative IDs, skips current/previous empty cycles, and creates no relation after the prior cycle is evicted.

- [ ] **Step 15: Run the focused replacement test and confirm it fails.**

  Run: `cd crates/trusted-server-js/lib && npx vitest run test/integrations/gpt_diagnostics/store.test.ts`

  Expected: FAIL because current cycles do not yet report replacement facts.

- [ ] **Step 16: Implement replacement derivation at current non-empty render.**

  When `recordSlotRenderEnded` receives `isEmpty === false`, scan the already retained earlier cycles in reverse request order. Never maintain separate history or mutate an earlier cycle. Set fields only for a prior non-empty `renderAtMs`; calculate `previousRenderToRequestMs` from that prior render to this cycle's `requestedAtMs` using the existing valid-duration helper.

- [ ] **Step 17: Run the focused replacement test and confirm it passes.**

  Run: `cd crates/trusted-server-js/lib && npx vitest run test/integrations/gpt_diagnostics/store.test.ts`

  Expected: PASS, including existing retention and creative-attempt cases.

- [ ] **Step 18: Commit the store behavior.**

  ```bash
  git add crates/trusted-server-js/lib/src/integrations/gpt_diagnostics/store.ts crates/trusted-server-js/lib/test/integrations/gpt_diagnostics/store.test.ts
  git commit -m "Correlate GPT request intents and replacements"
  ```

### Task 3: Safely expose the optional Trusted Server auction ID

**Files:**

- Modify: `crates/trusted-server-js/lib/src/integrations/gpt_diagnostics/api.ts`
- Modify: `crates/trusted-server-js/lib/test/integrations/gpt_diagnostics/api.test.ts`
- Modify: `crates/trusted-server-js/lib/src/integrations/gpt/index.ts`
- Modify: `crates/trusted-server-js/lib/test/integrations/gpt/ad_init.test.ts`

- [ ] **Step 1: Write failing API forwarding/no-throw tests.**

  Assert the controller forwards four arguments unchanged to the store, and that a throwing store remains a no-op to the caller. Update fake store interfaces to reflect the optional fourth argument.

- [ ] **Step 2: Write failing `adInit` tests.**

  Give an `AuctionBidData` fixture `hb_auction_id: 'auction-123'` and assert `recordTrustedServerOpportunity(slot, auctionSlotId, opportunity, 'auction-123')`. Retain the three-argument expectation when the metadata is absent, and retain delivery behavior when diagnostics throws.

- [ ] **Step 3: Run the focused API and ad-init tests and confirm they fail.**

  Run: `cd crates/trusted-server-js/lib && npx vitest run test/integrations/gpt_diagnostics/api.test.ts test/integrations/gpt/ad_init.test.ts`

  Expected: FAIL because the optional parameter is not forwarded.

- [ ] **Step 4: Implement pass-through only.**

  Update API/store interfaces and the API callback to accept/pass `trustedServerAuctionId`. In GPT `adInit`, conditionally call the diagnostics method with four arguments only when `bid.hb_auction_id` is present; otherwise preserve the existing three-argument call (do not append an explicit `undefined`). Keep the existing `try/catch` boundary and do no client-side auction-ID validation beyond the store's diagnostic normalization.

- [ ] **Step 5: Run the focused tests and confirm they pass.**

  Run: `cd crates/trusted-server-js/lib && npx vitest run test/integrations/gpt_diagnostics/api.test.ts test/integrations/gpt/ad_init.test.ts`

  Expected: PASS.

- [ ] **Step 6: Commit the API and GPT forwarding change.**

  ```bash
  git add crates/trusted-server-js/lib/src/integrations/gpt_diagnostics/api.ts crates/trusted-server-js/lib/src/integrations/gpt/index.ts crates/trusted-server-js/lib/test/integrations/gpt_diagnostics/api.test.ts crates/trusted-server-js/lib/test/integrations/gpt/ad_init.test.ts
  git commit -m "Forward auction IDs to GPT diagnostics"
  ```

### Task 4: Add the standalone fail-open publisher refresh observer

**Files:**

- Modify: `crates/trusted-server-js/lib/src/integrations/gpt_diagnostics/observer.ts`
- Modify: `crates/trusted-server-js/lib/src/integrations/gpt_diagnostics/index.ts`
- Modify: `crates/trusted-server-js/lib/test/integrations/gpt_diagnostics/observer.test.ts`
- Modify: `crates/trusted-server-js/lib/test/integrations/gpt_diagnostics/index.test.ts`

- [ ] **Step 1: Add failing observer tests for explicit and bare refreshes.**

  Extend the controlled PubAds fake with `refresh` and `getSlots`. Assert an external explicit call records exactly its valid object identities; a bare call obtains exactly the concrete valid `getSlots()` identities; malformed values and throwing `getSlots` record nothing but delegate. Assert repeated `install()` does not double-wrap.

- [ ] **Step 2: Add failing transparency/fail-open tests.**

  Parameterize zero arguments, one explicit `undefined`, and two arguments. For each, assert original receiver and slot/options identities, result, synchronous throw, and exactly-one invocation are preserved. Add throwing/malformed diagnostics-store and context-read cases that still delegate exactly once.

- [ ] **Step 3: Run the focused observer/runtime tests and confirm they fail.**

  Run: `cd crates/trusted-server-js/lib && npx vitest run test/integrations/gpt_diagnostics/observer.test.ts test/integrations/gpt_diagnostics/index.test.ts`

  Expected: FAIL because no refresh observer is installed.

- [ ] **Step 4: Implement one observer-owned wrapper in the existing `googletag.cmd` callback.**

  Expand the observer's narrow PubAds type only as needed. After six listener registrations, install an idempotent refresh wrapper around the currently installed `pubads.refresh`, isolated in its own `try/catch` so failure cannot affect listener installation. Use `function (...args)` plus `Reflect.apply(originalRefresh, this, args)` to preserve call shape. Before delegation, derive slots only for diagnostics; skip publisher intent if `window.tsjs?.adInitRefreshInProgress` or the Prebid dispatch-context reader says active; then delegate once regardless of diagnostics errors.

  Do not add GPT listeners, invoke GPT, alter `refresh` options, or use slot IDs/targeting to infer a source. Keep any wrapper sentinel private to the observer and compatible with the Prebid wrapper installed later.

- [ ] **Step 5: Install the diagnostics observer before Prebid can wrap it.**

  Preserve runtime idempotency in `index.ts`; ensure the observer queues through diagnostics' existing `googletag.cmd` setup, with no coupling to RC, July handoff, APS, or slot-handoff code.

- [ ] **Step 6: Run the focused observer/runtime tests and confirm they pass.**

  Run: `cd crates/trusted-server-js/lib && npx vitest run test/integrations/gpt_diagnostics/observer.test.ts test/integrations/gpt_diagnostics/index.test.ts`

  Expected: PASS.

- [ ] **Step 7: Commit the standalone observer.**

  ```bash
  git add crates/trusted-server-js/lib/src/integrations/gpt_diagnostics/{observer,index}.ts crates/trusted-server-js/lib/test/integrations/gpt_diagnostics/{observer,index}.test.ts
  git commit -m "Observe publisher GPT refresh intent"
  ```

### Task 5: Make nested Prebid refreshes Prebid-only with synchronous context

**Files:**

- Modify: `crates/trusted-server-js/lib/src/integrations/prebid/index.ts`
- Modify: `crates/trusted-server-js/lib/test/integrations/prebid/index.test.ts`
- Modify: `crates/trusted-server-js/lib/test/integrations/gpt_diagnostics/observer.test.ts`

- [ ] **Step 1: Write failing nested-wrapper integration tests.**

  Install the diagnostics observer first, then `installRefreshHandler`. Assert Prebid's delegated explicit and bare refreshes record Prebid intent and do not record publisher intent merely from nesting; `adInitRefreshInProgress` records neither. Assert context restoration after return and synchronous throw, including a throwing context accessor.

- [ ] **Step 2: Run targeted Prebid and observer tests and confirm they fail.**

  Run: `cd crates/trusted-server-js/lib && npx vitest run test/integrations/prebid/index.test.ts test/integrations/gpt_diagnostics/observer.test.ts`

  Expected: FAIL because the observer sees the nested call as publisher-originated.

- [ ] **Step 3: Add a minimal synchronous diagnostic dispatch context.**

  Use the shared `window.tsjs?.prebidRefreshDispatchInProgress` declared on `TsjsApi`, not a private module variable or bundle-specific cast. Set it only around each actual `originalRefresh` delegation for which `recordPrebidRefreshForDiagnostics(targetSlots)` ran; defensively read any prior value, restore that exact value (or remove the property if it was absent) in `finally`, including on synchronous throw. The observer defensively reads this same optional property. Context setup/read/restore failures must not change Prebid auction timing, GPT call count, targeting, watchdog behavior, return, or thrown error.

- [ ] **Step 4: Run targeted Prebid and observer tests and confirm they pass.**

  Run: `cd crates/trusted-server-js/lib && npx vitest run test/integrations/prebid/index.test.ts test/integrations/gpt_diagnostics/observer.test.ts`

  Expected: PASS.

- [ ] **Step 5: Commit the Prebid context change.**

  ```bash
  git add crates/trusted-server-js/lib/src/integrations/prebid/index.ts crates/trusted-server-js/lib/test/integrations/prebid/index.test.ts crates/trusted-server-js/lib/test/integrations/gpt_diagnostics/observer.test.ts
  git commit -m "Keep Prebid diagnostics refreshes distinct"
  ```

### Task 6: Carry `AuctionRequest.id` in branch-native winning-bid metadata

**Files:**

- Modify: `crates/trusted-server-core/src/publisher.rs`
- Test: `crates/trusted-server-core/src/publisher.rs` (`creative_opportunities_tests` and `page_bids_no_match_tests`)

- [ ] **Step 1: Add failing initial-document and page-bids Rust tests.**

  Construct a deterministic `AuctionRequest` ID and one winning bid. Assert the initial generated `tsjs.bids` payload and the `/_ts/page-bids` JSON response contain `hb_auction_id` only on the winning bid, equal to that request ID. Assert no entry appears when there is no winning bid; do not assert or add targeting effects.

- [ ] **Step 2: Run the focused publisher tests and confirm they fail.**

  Run: `cargo test-fastly publisher::tests::creative_opportunities_tests -- --nocapture && cargo test-fastly publisher::tests::page_bids_no_match_tests -- --nocapture`

  Expected: FAIL because `build_bid_map` has no request-ID input or `hb_auction_id` output.

- [ ] **Step 3: Thread only the optional opaque ID through the two existing response paths.**

  Change `build_bid_map` and `write_bids_to_state` to accept `Option<&str>` (or `Option<&AuctionRequest>` and borrow its `String` ID at the final call) because `AuctionRequest.id` is already a `String`. Insert `"hb_auction_id": Value::String(auction_id.to_owned())` only while serializing an existing winning-bid object. Pass the same current request ID from the initial collection flow and `handle_page_bids`; do not derive a second ID, add it to `Bid`, expose any request payload, or touch APS/RC/JULY data.

- [ ] **Step 4: Run focused Rust tests and confirm they pass.**

  Run: `cargo test-fastly publisher::tests::creative_opportunities_tests -- --nocapture && cargo test-fastly publisher::tests::page_bids_no_match_tests -- --nocapture`

  Expected: PASS.

- [ ] **Step 5: Commit the metadata-only Rust change.**

  ```bash
  git add crates/trusted-server-core/src/publisher.rs
  git commit -m "Expose auction ID in diagnostic bid metadata"
  ```

### Task 7: Present and document observed facts without overclaiming

**Files:**

- Modify: `crates/trusted-server-js/lib/src/integrations/gpt_diagnostics/overlay.ts`
- Modify: `crates/trusted-server-js/lib/test/integrations/gpt_diagnostics/overlay.test.ts`
- Modify: `docs/guide/integrations/gpt-diagnostics.md`

- [ ] **Step 1: Add failing overlay tests.**

  Assert labels for Publisher refresh and all existing paths. For populated facts, assert exactly: `Request intent: 7`, `Trusted Server auction: auction-123`, `Opportunity → request 24 ms`, `Replaced rendered request 1 after 6048 ms`, and creative changed/unchanged wording. Assert missing/one-sided creative IDs omit comparison, and `slotContentChanged` remains independently visible.

- [ ] **Step 2: Run the focused overlay test and confirm it fails.**

  Run: `cd crates/trusted-server-js/lib && npx vitest run test/integrations/gpt_diagnostics/overlay.test.ts`

  Expected: FAIL because publisher/replacement/correlation facts are not rendered.

- [ ] **Step 3: Implement conditional factual labels.**

  Add the `publisher_refresh` request-path label and append only defined, valid fields. Use existing millisecond formatting; never render `unknown changed`, infer a source, or reinterpret `slotContentChanged` as a creative transition.

- [ ] **Step 4: Update the guide in the same commit.**

  Replace marker wording with request-intent semantics and the three-source classification table. Document independent five-second source expiry, opaque bounded auction-ID validation, opportunity latency, publisher boundary limits, and shared Prebid dispatch-context behavior. Document the deployed `slotOnload` correction precisely: a unique response-bearing cycle can observe load before render; it records `loadObservedBeforeRender`, omits `renderToLoadMs`, and is neither incomplete nor an invalid event order; overlapping response-bearing candidates remain ambiguous. Document observed replacement/creative semantics and export privacy. Retain explicit caveats: no source inference, no winner proof, no payload/markup/targeting capture, no behavior change, and bypassed stale refresh references remain unattributed.

- [ ] **Step 5: Run focused overlay test and docs formatting.**

  Run: `cd crates/trusted-server-js/lib && npx vitest run test/integrations/gpt_diagnostics/overlay.test.ts && cd ../../../docs && npm run format`

  Expected: PASS; docs formatter completes successfully.

- [ ] **Step 6: Commit presentation and documentation.**

  ```bash
  git add crates/trusted-server-js/lib/src/integrations/gpt_diagnostics/overlay.ts crates/trusted-server-js/lib/test/integrations/gpt_diagnostics/overlay.test.ts docs/guide/integrations/gpt-diagnostics.md
  git commit -m "Document GPT refresh diagnostics evidence"
  ```

### Task 8: Run focused and full verification before handoff

**Files:**

- Verify only; no production changes.

- [ ] **Step 1: Run all touched JavaScript diagnostics and integration tests.**

  Run:

  ```bash
  cd crates/trusted-server-js/lib && npx vitest run \
    test/integrations/gpt_diagnostics/store.test.ts \
    test/integrations/gpt_diagnostics/api.test.ts \
    test/integrations/gpt_diagnostics/observer.test.ts \
    test/integrations/gpt_diagnostics/index.test.ts \
    test/integrations/gpt_diagnostics/overlay.test.ts \
    test/integrations/gpt_diagnostics/types.test.ts \
    test/integrations/gpt/ad_init.test.ts \
    test/integrations/prebid/index.test.ts \
    test/prebid-artifact-integration.test.mjs
  ```

  Expected: PASS.

- [ ] **Step 2: Run focused Rust coverage for both response paths.**

  Run: `cargo test-fastly publisher::tests::creative_opportunities_tests -- --nocapture && cargo test-fastly publisher::tests::page_bids_no_match_tests -- --nocapture`

  Expected: PASS.

- [ ] **Step 3: Run the complete CI-equivalent verification matrix.**

  Run:

  ```bash
  cd crates/trusted-server-js/lib && npx vitest run && npm run lint && npm run format && node build-all.mjs
  cd ../../..
  cargo fmt --all -- --check
  cargo build-axum
  cargo build --package trusted-server-adapter-fastly --release --target wasm32-wasip1
  cargo test-fastly
  cargo test-axum
  cargo test-cloudflare
  cargo test-spin
  cargo check -p trusted-server-adapter-cloudflare
  cargo check -p trusted-server-adapter-spin
  cargo check-spin
  TRUSTED_SERVER__PUBLISHER__ORIGIN_URL=http://127.0.0.1:8080 \
  TRUSTED_SERVER__PUBLISHER__PROXY_SECRET=integration-test-proxy-secret \
  TRUSTED_SERVER__EC__PASSPHRASE=integration-test-ec-secret-padded-32 \
  TRUSTED_SERVER__PROXY__CERTIFICATE_CHECK=false \
  cargo build --package trusted-server-adapter-spin --target wasm32-wasip1 --features spin --release
  cargo bench -p trusted-server-core --bench html_processor_bench -- --test
  ./scripts/test-cli.sh
  cargo test --package trusted-server-openrtb-codegen --target "$(rustc -vV | sed -n 's/host: //p')"
  cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test parity
  cargo fmt --manifest-path crates/trusted-server-integration-tests/Cargo.toml -- --check
  cargo clippy --manifest-path crates/trusted-server-integration-tests/Cargo.toml --all-targets -- -D warnings
  cargo clippy-fastly
  cargo clippy-axum
  cargo clippy-cloudflare
  cargo clippy-cloudflare-wasm
  cargo clippy-spin-native
  cargo clippy-spin-wasm
  cargo clippy --manifest-path crates/trusted-server-cli/Cargo.toml --target "$(rustc -vV | sed -n 's/host: //p')" --all-targets --all-features -- -D warnings
  cargo clippy --package trusted-server-openrtb-codegen --target "$(rustc -vV | sed -n 's/host: //p')" --all-targets -- -D warnings
  cargo check-fastly && cargo check-axum && cargo check-cloudflare
  cd docs && npm run lint && npm run format
  ```

  Expected: every command exits 0. Do not use bare `cargo test --workspace` or a host-target workspace build. The four Spin release-build variables above are the CI fixture overrides required to create a bootable production-style artifact; they are test values, not deployment configuration.

- [ ] **Step 4: Inspect the final diff and prepare the intended diagnostics changes for review.**

  Run: `git diff --check && git status --short && git diff -- docs/guide/integrations/gpt-diagnostics.md crates/trusted-server-core/src/publisher.rs crates/trusted-server-js/lib/src`

  Expected: no whitespace errors; no unrelated files, RC/JULY handoff, APS renderer, targeting, selection, or delivery changes. Do not create an unconditional extra final commit: the focused task commits already own their exact files.
