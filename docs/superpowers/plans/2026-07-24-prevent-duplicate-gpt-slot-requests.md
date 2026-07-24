# Prevent Duplicate GPT Slot Requests — Implementation Plan

> **Status:** Revised after production-like validation found a second request for
> publisher-owned slots when hydration-safe scheduling defers `adInit()`.
>
> **Spec:** `docs/superpowers/specs/2026-07-24-prevent-duplicate-gpt-slot-requests-design.md`

**Goal:** Ensure one GPT slot and one initial request per configured placement when
TS `adInit()` runs before a publisher later defines the placement's inner GPT div.

**Architecture:** TS creates its fallback on the resolved inner div and records a
handoff claim. Narrow, idempotent GPT wrappers also gate a configured publisher
slot's first `display`/`refresh` while the server auction result is unavailable. At
`adInit()`, TS applies targeting to that same publisher slot and replays the held
native request once; it does not issue a second TS refresh. Late-definition handoff
and SPA ownership transfer remain unchanged. The head bootstrap and full TSJS bundle
share this runtime protocol through `window.tsjs`.

**Primary files:**

- `crates/trusted-server-js/lib/src/core/types.ts`
- `crates/trusted-server-js/lib/src/integrations/gpt/index.ts`
- `crates/trusted-server-js/lib/test/integrations/gpt/ad_init.test.ts`
- `crates/trusted-server-js/lib/test/integrations/gpt/index.test.ts`
- `crates/trusted-server-core/src/integrations/gpt_bootstrap.js`
- `crates/trusted-server-core/src/integrations/gpt.rs`

## Preconditions

- [ ] Confirm with the issue owner that the intended late-owner behavior is slot
      handoff (publisher receives the existing inner-div slot), not a hydration-delay
      policy.
- [ ] Capture representative publisher call sequences for normal initial load and
      `disableInitialLoad()` before changing wrappers. The expected sequence is
      `defineSlot` → `addService` → `display`; initial-load-disabled pages additionally
      call `refresh`.
- [ ] Establish an automated fake-GPT request counter: calling native `display` with
      initial load enabled, or native `refresh` with initial load disabled, records a
      request. Assertions must use this counter rather than only `getSlots()`.

## Task 1: Add the shared handoff state and typed GPT wrapper surface

**Files:**

- Modify `crates/trusted-server-js/lib/src/core/types.ts`
- Modify `crates/trusted-server-js/lib/src/integrations/gpt/index.ts`

- [ ] Add `TsjsApi` state for both the div-ID-keyed late-handoff registry and an
      initial publisher-request gate. The gate records held display/refresh IDs and
      a released marker so it applies only once per page load.
- [ ] Add only the minimal optional/internal type surface needed for idempotence
      markers on GPT functions and `pubads`. Do not weaken the public GPT types with
      `any`.
- [ ] Add helper functions in `index.ts` to:
  - find a live GPT slot by exact element ID;
  - register and retrieve a claim;
  - remove a transferred slot from `ts.prevGptSlots`;
  - run an internal TS GPT call behind a short-lived guard;
  - filter a requested refresh list (including no-argument/global refresh) by the
    entries whose one-shot publisher refresh must be suppressed.
- [ ] Keep the registry on `window.tsjs`, not in module scope, so the bootstrap state
      survives bundle loading.

**Focused checks:**

```bash
cd crates/trusted-server-js/lib
npx vitest run test/integrations/gpt/ad_init.test.ts test/integrations/gpt/index.test.ts
```

## Task 2: Install scoped idempotent handoff wrappers

**File:** `crates/trusted-server-js/lib/src/integrations/gpt/index.ts`

- [ ] From the GPT command queue, install wrappers once GPT exposes the real methods.
      Mark the wrapped functions/service so a later `installTsAdInit()` call or the
      bootstrap-to-bundle handoff cannot stack wrappers.
- [ ] `defineSlot` wrapper:
  - pass through TS-internal calls and IDs absent from the registry;
  - for a late publisher call on a claimed inner div, find and return the existing
    slot without calling native `defineSlot`;
  - mark ownership transferred and remove that slot from `prevGptSlots` before
    returning it;
  - log, but do not create a second slot, if publisher arguments differ from the TS
    configuration.
- [ ] `display` wrapper: consume the one permitted post-handoff display; before the
      first `adInit()`, also hold a configured publisher slot's native display.
- [ ] `refresh` wrapper: consume one permitted post-handoff disabled-load refresh;
      before the first `adInit()`, hold configured publisher refreshes and forward
      all unrelated slots explicitly, including a no-argument/global refresh.
- [ ] At initial `adInit()`, apply targeting then replay held native calls; never
      refresh an existing publisher-owned slot that has already requested.
- [ ] Ensure wrapper installation precedes publisher setup and fallback creation.

**Focused checks:**

```bash
cd crates/trusted-server-js/lib
npx vitest run test/integrations/gpt/ad_init.test.ts
```

## Task 3: Change fallback creation to the actual inner div

**File:** `crates/trusted-server-js/lib/src/integrations/gpt/index.ts`

- [ ] Delete the `${actualDivId}-container` fallback selection. When no existing
      publisher slot is found, call `defineSlot` with `actualDivId`.
- [ ] Register the handoff claim immediately after successful TS definition.
- [ ] Keep `display()` for TS-created slots; with initial load disabled, retain the
      single TS `refresh()` that makes the required initial request.
- [ ] Simplify `divToSlotId` and `prevSlotTargetingKeys` to the actual inner div;
      remove only mappings that existed exclusively for the container fallback.
- [ ] On SPA navigation, destroy only claims that remain TS-owned. A transferred
      claim must participate in stale-targeting cleanup but never be passed to
      `destroySlots()`.
- [ ] Retain exact match then prefix-based dynamic-ID lookup; do not interpolate
      publisher-provided IDs into CSS selectors.

## Task 4: Add request-level regression coverage for the full bundle

**Files:**

- Modify `crates/trusted-server-js/lib/test/integrations/gpt/ad_init.test.ts`
- Modify `crates/trusted-server-js/lib/test/integrations/gpt/index.test.ts` if the
  shared wrapper setup belongs there

- [ ] Introduce a reusable fake GPT fixture that models slots by element ID and
      records native `defineSlot`, `display`, `refresh`, and request events. Its
      `getSlots()` result must update when a slot is defined so the test cannot pass by
      asserting a stale static array.
- [ ] Add a failing regression test for the critical sequence:
  1. TS finds the inner div and runs `adInit()` before publisher setup;
  2. TS defines/displays the inner div and makes one request;
  3. publisher calls `defineSlot(innerDiv).addService(...); display(innerDiv)`;
  4. assert native `defineSlot` was called once, there is one slot, and there is one
     request.
- [ ] Add the same sequence with `disableInitialLoad()`: TS display plus its refresh
      makes one request; the publisher's first refresh cannot make a second request.
- [ ] Add a no-argument publisher refresh test containing an unrelated slot. Assert
      the claimed slot is suppressed once and the unrelated slot is refreshed.
- [ ] Add publisher-owned tests proving TS holds normal and disabled-load initial
      requests, applies targeting, and replays exactly one native request. Also prove
      an already-requested publisher slot is not refreshed again.
- [ ] Add a no-publisher test proving TS still creates, displays, and requests its
      inner-div slot exactly once.
- [ ] Add a SPA handoff test: after late publisher claim, the next `adInit()` does not
      destroy the transferred slot, clears old TS keys, and reapplies current-route
      targeting.
- [ ] Retain or extend the dynamic prefix-ID test to prove a resolved runtime ID is
      the handoff key.

## Task 5: Mirror the runtime protocol in the head bootstrap

**Files:**

- Modify `crates/trusted-server-core/src/integrations/gpt_bootstrap.js`
- Modify `crates/trusted-server-core/src/integrations/gpt.rs`

- [ ] Port the same initial-request gate, actual-inner-div fallback, registry names,
      lifecycle flags, and idempotence markers to the plain-JavaScript bootstrap.
- [ ] Use the existing bootstrap `window.tsjs` properties exactly so `index.ts` can
      adopt the initial claim after the bundle loads.
- [ ] Ensure its internal definition/display/refresh calls use the same guards as the
      bundle; bootstrap must not transfer or suppress its own operations.
- [ ] Extend the `gpt.rs` head-insert tests to assert that the bootstrap contains the
      inner-div handoff protocol and no longer contains the container fallback.
- [ ] Add an executable bootstrap behavior test if practical by evaluating the
      injected script against the same fake GPT fixture. If the test setup cannot execute
      the included asset without duplication, record that limitation and keep the Rust
      source-contract assertion plus identical bundle lifecycle tests as the minimum
      coverage.

## Task 6: Validate, inspect, and ship

- [ ] Run focused request-level tests:

  ```bash
  cd crates/trusted-server-js/lib
  npx vitest run test/integrations/gpt/ad_init.test.ts test/integrations/gpt/index.test.ts
  ```

- [ ] Run all TSJS tests and formatting:

  ```bash
  cd crates/trusted-server-js/lib
  npx vitest run
  npm run format
  ```

- [ ] Run the target-matched Rust tests that cover the embedded bootstrap, followed by
      project formatting and linting:

  ```bash
  cargo test-axum
  cargo fmt --all -- --check
  cargo clippy-fastly && cargo clippy-axum && cargo clippy-cloudflare
  ```

- [ ] Before PR handoff, run the full required CI gates from `CLAUDE.md`, including
      Fastly, Axum, Cloudflare, Spin, integration parity, JS build/tests/format, and docs
      format.
- [ ] Review the diff specifically for bootstrap/bundle protocol drift and for any
      use of container IDs in GPT slot creation.
- [ ] In a controlled production-like browser capture with the hydration-safe
      deferred `adInit()` path, verify one targeted initial request for each affected
      visible placement and independently verify an unrelated placement remains
      requestable.
- [ ] Update issue #944 with the ownership-handoff decision, test evidence, and
      browser-capture result.

## Stop conditions

Stop and return to design review instead of adding heuristics if any of these occur:

- A publisher relies on a late `defineSlot` with materially different path or size
  arguments and cannot accept the existing TS slot.
- The publisher's first initial-load-disabled refresh cannot be identified without
  suppressing unrelated legitimate refreshes.
- A cross-bundle bootstrap handoff requires module-local identity that cannot be
  represented safely through `window.tsjs`.
- Browser validation shows a second request despite native `defineSlot`/`display`/
  `refresh` suppression; capture the GPT event ordering before choosing another
  strategy.
