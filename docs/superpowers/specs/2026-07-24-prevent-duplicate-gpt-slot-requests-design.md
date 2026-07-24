# Prevent Duplicate GPT Slot Requests — Design Specification

## Problem

When `tsjs.adInit()` executes before a publisher's framework later calls
`googletag.defineSlot()` for the same placement, TS currently defines and displays a
slot on the outer `-container` element. The publisher subsequently defines and
displays an inner-div slot. These are distinct GPT slots, so they make separate GAM
requests for one visible placement.

The affected paths are deliberately duplicated today:

- `crates/trusted-server-js/lib/src/integrations/gpt/index.ts` is the full bundle
  implementation used after the TSJS bundle loads.
- `crates/trusted-server-core/src/integrations/gpt_bootstrap.js` is the head-injected
  implementation that can make the initial request before the bundle loads.

A fix must keep both implementations in sync.

## Goals

1. A configured placement has at most one initial GPT slot and ad request when TS
   runs before a publisher defines its inner div.
2. Apply TS targeting and the `ts_initial=1` marker before that single initial
   request.
3. Continue reusing a slot that the publisher has already defined.
4. Keep the TS-only fallback: if the publisher never defines the placement, TS still
   displays it and makes exactly one initial request.
5. Preserve `disableInitialLoad()`, SPA targeting cleanup, and the rule that TS does
   not destroy genuinely publisher-owned slots.
6. Keep dynamic div-ID prefix resolution intact.

## Non-goals

- Deduplicating by GAM ad-unit path. Multiple visible placements may validly share a
  path.
- Changing publisher GAM configuration, line items, or refresh policy.
- Delaying the initial TS request while waiting an arbitrary amount of time for
  framework hydration. A time-based grace period cannot distinguish a slow
  publisher-owned slot from a placement that the publisher will never define.
- General interception of unrelated GPT slots.

## Decision: one inner-div slot with late-definition handoff

TS will define its fallback slot on the **actual inner div**, never on its outer
`-container` element. It will record a narrowly scoped handoff claim keyed by that
inner div ID. A `googletag.defineSlot` wrapper then recognizes a later publisher
request for that exact div and returns the existing TS slot rather than invoking
GPT's native `defineSlot` again.

GPT requires a one-to-one slot-to-div relationship and documents that a slot should
be displayed only once. Sharing the initial inner-div slot therefore avoids both the
competing container slot and an invalid duplicate definition.

### Lifecycle

1. **Already publisher-owned** — `getSlots()` finds a slot for the resolved inner
   div. TS applies targeting, records it as publisher-owned, and refreshes it as it
   does today.
2. **No slot yet** — TS defines a slot on the resolved inner div, applies targeting,
   enables services when needed, and displays it. When initial load is disabled, TS
   performs its existing one explicit refresh. TS records this slot as TS-owned and
   handoff-eligible.
3. **Publisher defines later** — the scoped `defineSlot` wrapper sees the recorded
   inner-div claim, returns the existing slot, and transfers ownership: it removes
   the slot from TS's future `destroySlots()` set. The publisher's setup continues
   against that same slot.
4. **Publisher's first request call** — the wrapper suppresses the duplicate
   publisher `display()` call. With `disableInitialLoad()`, it instead suppresses
   only the publisher's first refresh for the transferred slot, because TS has
   already issued the required initial refresh. For a no-argument/global refresh,
   the wrapper must expand `getSlots()`, remove only the one-shot suppressed slots,
   and forward the remaining slots explicitly so unrelated slots still refresh.
5. **Later refreshes and SPA navigation** — after the one-shot suppression is
   consumed, publisher refreshes are untouched. On navigation, TS clears its
   targeting from the shared slot and may reuse it for the next route; it must not
   destroy a slot after ownership has transferred.

The wrapper is not a global deduplicator. It only handles IDs present in TS's
handoff registry and must preserve native `defineSlot`, `display`, and `refresh`
behavior for every other placement.

## Implementation shape

### Shared runtime state

Add a small, serializable `window.tsjs` registry that both initial implementations
can read after the bundle replaces the bootstrap implementation. It is keyed by the
resolved actual div ID and records at least:

- whether TS created the slot and whether ownership has transferred;
- whether one publisher `display()` or initial-load-disabled `refresh()` remains to
  suppress.

Do not rely only on module-local state: the bootstrap can define the initial slot
before `index.ts` is loaded. Look up the live slot by element ID through
`pubads().getSlots()` when a wrapper needs it.

Install idempotent markers on the wrapped GPT functions/services so the bootstrap and
bundle do not stack wrappers. Each wrapper must retain and call the original bound
function for non-claimed slots. Internal TS calls need a short-lived guard so the
wrappers do not mistake TS's own `defineSlot`, `display`, or `refresh` for a
publisher handoff.

### Full bundle

In `crates/trusted-server-js/lib/src/integrations/gpt/index.ts`:

- Replace the container fallback with `actualDivId`.
- Add the typed handoff-registry state to `TsjsApi` in
  `crates/trusted-server-js/lib/src/core/types.ts`.
- Install the idempotent `defineSlot`, `display`, and `pubads().refresh` handoff
  wrappers from the GPT command queue before `adInit()` can create a fallback slot.
- When a late publisher definition is aliased to the existing slot, remove it from
  `prevGptSlots` and mark it transferred before returning it.
- Keep targeting cleanup keyed by the real inner div. Remove the old dual
  inner/container mappings because the slot element ID is now the inner div.

### Head bootstrap

Mirror the same ownership registry and wrappers in
`crates/trusted-server-core/src/integrations/gpt_bootstrap.js`. The bootstrap must
leave the registry and idempotence markers in `window.tsjs` so the full bundle adopts
rather than re-wraps or reclaims the initial slot.

This duplication is intentional for now: the head bootstrap is needed to apply
server-side targeting before the normal bundle becomes available. The regression
suite must exercise both implementations' observable contract.

## Compatibility rules and risks

| Risk                                                                             | Mitigation                                                                                                                                                                                                                                                                                                               |
| -------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| Publisher passes a different ad-unit path or sizes in its late `defineSlot` call | Return the existing claimed slot but log a diagnostic. Do not define a second slot. Treat the TS configuration and publisher configuration mismatch as an integration error to resolve separately.                                                                                                                       |
| Publisher invokes global `refresh()` after `disableInitialLoad()`                | Filter the one-shot claimed slot from the expanded slot list and refresh all remaining slots. A no-argument refresh must not be silently dropped.                                                                                                                                                                        |
| Publisher calls a legitimate refresh without an initial display                  | The one-shot suppression is consumed only immediately after a successful late handoff. Document and test the standard publisher sequence (`defineSlot` → `addService` → `display`, with `refresh` when initial load is disabled). Escalate unusual publisher lifecycle requirements rather than adding a time heuristic. |
| Publisher-owned slot is destroyed on SPA navigation                              | Transfer ownership synchronously in the `defineSlot` wrapper and remove the slot from `prevGptSlots`.                                                                                                                                                                                                                    |
| Bootstrap and bundle diverge                                                     | Give both paths the same black-box regression cases; retain a Rust source-contract assertion for bootstrap-specific sentinels.                                                                                                                                                                                           |
| A framework creates the inner element only after `adInit()`                      | TS still skips an absent element, as it does today; when the publisher owns that later-created slot it will not be duplicated. Supplying TS targeting to such a slot is a separate readiness problem, not part of this duplicate-request fix.                                                                            |

## Acceptance criteria

- A late `defineSlot(innerDiv)` aliases the already-created inner-div TS slot; native
  `defineSlot` is not called a second time for that placement.
- Request instrumentation records one initial request for the placement in normal and
  initial-load-disabled modes.
- The late publisher `display()` (and its first initial-load-disabled refresh) cannot
  create a second request, while unrelated slots retain their normal calls.
- Existing publisher slots are still reused and receive TS targeting.
- A slot that no publisher claims is displayed and requested once by TS.
- A transferred slot is absent from TS's SPA `destroySlots()` argument; targeting is
  still cleared and reapplied correctly on the next route.
- Dynamic resolved div IDs work without constructing a CSS selector from the ID.
- Bootstrap and bundle paths pass the same ownership/request assertions.

## Validation

1. Add focused Vitest lifecycle tests with a fake GPT that records native
   `defineSlot`, `display`, `refresh`, and synthetic request events.
2. Run the focused GPT test files, then the full TSJS Vitest suite and formatter.
3. Run the target-matched Rust test suite so the included bootstrap and its source
   assertions compile and pass.
4. In a controlled browser capture, verify that one configured header and one
   configured fixed placement each produce one initial slot request, while a distinct
   in-content placement remains independently requestable.
