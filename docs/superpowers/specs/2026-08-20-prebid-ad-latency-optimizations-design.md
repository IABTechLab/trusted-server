# Prebid Ad-Latency Optimizations — Design

**Date:** 2026-08-20
**Status:** Approved design, pending implementation plan
**Scope:** Client-side auction properties (server-side ad templates inactive)

## Problem

On properties running the client-side auction path (`creative_opportunities.enabled = false`), ads render slowly relative to what the pipeline allows. A three-run instrumented baseline against a local Trusted Server proxying the production autoblog origin (article page, consent resolved, reader-style scrolling) measured:

| Milestone                               | Time   |
| --------------------------------------- | ------ |
| Prebid bundle + shim fully installed    | ~2.1 s |
| `DOMContentLoaded`                      | ~2.1 s |
| `window.load`                           | ~3.0 s |
| First Trusted Server `/auction` request | ~3.3 s |
| Publisher's first `requestBids`         | ~4.8 s |
| First non-empty ad render               | ~5.0 s |

Three structural costs stand out, all in Trusted-Server-owned surfaces:

1. **The first refresh auction waits for `window.load`.** The post-`load` + double-`requestAnimationFrame` defer exists to avoid React hydration mismatches (React #418), but only the DOM application needs that defer — the auction network fetch is hydration-neutral. `DOMContentLoaded` fires ~1.2 s before `load` on this page, and the gap grows on resource-heavy pages.
2. **Publisher `requestBids` bursts pay one `/auction` round trip each.** The baseline captured two `requestBids` calls 1 ms apart producing two `/auction` POSTs 2 ms apart. Each call costs a full round trip plus ~865 ms median (p90 ~1075 ms) of server-side auction time.
3. **The first GAM ad request pays fresh connection setup.** `securepubads.g.doubleclick.net` serves both the `pubads_impl` script and every ad request; no connection is warmed before first use.

Not addressed here (out of scope): re-enabling server-side ad templates, GPT lazy-load fetch margins (publisher-coordinated), the publisher's own ad-framework init latency (~1.8 s after `load` before their first `requestBids`), and server-side auction duration tuning (PBS `tmax`).

## Design

Three independent levers. Two are config-gated and default off, so shipping the binary changes nothing until an operator flips the flag; one is an unconditional resource hint.

### Lever A — `requestBids` coalescing window (opt-in)

**Config:** `[integrations.prebid] request_bids_coalesce_ms` — `u32`, default `0`, validated `0..=500`. Serialized into the injected client config (`window.__tsjs_prebid`) as `requestBidsCoalesceMs`, omitted when `0`.

**Behavior:** The tsjs Prebid shim already wraps `pbjs.requestBids` (bidder injection, snapshot capture, `bidsBackHandler` chaining). With a non-zero window, the wrapper holds a transformed call for up to the window duration and merges every mergeable call that arrives within it into one underlying `requestBids`:

- **Merged request:** union of the pending calls' ad units (in arrival order), maximum of their `timeout`s (absent if none supplied), and a combined `bidsBackHandler` that invokes each pending call's (already-wrapped) handler in arrival order with the merged auction's results. A throwing handler must not prevent later handlers from running.
- **Merge-safety rule:** only calls whose request object consists solely of `adUnits`, `timeout`, and `bidsBackHandler` (with a non-empty explicit `adUnits` array) are held. Any other call — extra keys such as `ortb2` or `labels`, or an implicit global-ad-units call — first flushes the pending queue synchronously, then dispatches solo. This preserves relative call order and never reinterprets options the merge logic does not understand.
- **Default `0`:** the wrapper dispatches immediately, byte-for-byte the current behavior.

**Return value:** held calls return `undefined` from `pbjs.requestBids`. This matches the wrapper's existing contract in practice, and the flag's opt-in nature means an operator enabling it accepts this for their property.

### Lever B — GAM preconnect hint

The GPT integration's `head_inserts` emits, before its bootstrap scripts:

```html
<link
  rel="preconnect"
  href="https://securepubads.g.doubleclick.net"
  crossorigin
/>
```

Unconditional: it is a pure hint with no behavioral effect, and every GPT-enabled property talks to this host. Removes DNS + TCP + TLS setup from the first ad request (and benefits the `pubads_impl` fetch).

### Lever C — first-auction prefetch at `DOMContentLoaded` (opt-in)

**Config:** `[integrations.prebid] prefetch_first_refresh_auction` — `bool`, default `false`. Injected as `prefetchFirstRefreshAuction`, omitted when `false`.

**Behavior:** When enabled, the first refresh auction's `/auction` request is dispatched as soon as all of the following hold, without waiting for `window.load`:

- consent has resolved (the existing consent gate is unchanged),
- the GPT slots that the auction would target have been observed,
- `DOMContentLoaded` has fired.

The response is held in memory and applied at the **unchanged** post-`load` + double-`rAF` application point. DOM work therefore keeps the exact hydration-safety timing the React #418 fix established; only the network round trip moves earlier.

**Fallback:** if the prefetched response has not arrived by the time the application point runs, the path degrades to today's behavior (issue the request then). A prefetch failure is discarded and the normal path retries; no new error surface.

## Config-blob compatibility

Both new fields serialize only at non-default values, matching the repository's rollback discipline: a blob that never sets them is accepted by older binaries, and before rolling a binary back past this feature, an operator must clear the flags and re-push (the same procedure documented for prior `[integrations.prebid]` additions).

## Testing

**Vitest (shim):**

- coalescing merges two mergeable calls in the window into one underlying `requestBids` with the union of ad units, and invokes both handlers in order;
- a call with extra option keys flushes the queue first and dispatches solo, preserving order;
- a throwing first handler does not prevent the second handler from running;
- window `0` / absent config leaves per-call dispatch unchanged (existing suite must pass untouched);
- prefetch: response held and applied at the application point; unresolved prefetch falls back to the current request-then-apply path.

**Rust:**

- config defaults (`request_bids_coalesce_ms = 0`, `prefetch_first_refresh_auction = false`) and the `0..=500` validation bound;
- injected-config serialization omits both fields at their defaults and includes them otherwise;
- GPT `head_inserts` contains the preconnect link.

**End-to-end verification:** re-run the local baseline harness (Viceroy proxying the production origin, pre-seeded consent, identical scroll script) with each flag enabled, comparing against the recorded baseline: time to first non-empty render, `/auction` request count, and burst dedup on the publisher's paired calls.

## Rollout

1. Land binary with defaults off; the preconnect hint is the only immediate change.
2. Enable `request_bids_coalesce_ms` (initially 50 ms) and `prefetch_first_refresh_auction` on the autoblog tester property via config push.
3. Compare live tester metrics with the harness prediction; each flag reverts independently by config push, no binary rollback.
