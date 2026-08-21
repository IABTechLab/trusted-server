# Prebid Ad-Latency and Auction-Load Optimizations — Design

**Date:** 2026-08-20
**Status:** Draft (returned from review; Lever C requires a discovery phase before implementation)
**Scope:** Client-side auction properties (server-side ad templates inactive)

## Problem

On properties running the client-side auction path (`creative_opportunities.enabled = false`), ad delivery leaves measurable headroom. A three-run instrumented baseline against a local Trusted Server proxying the production autoblog origin (article page, consent resolved, reader-style scrolling) measured:

| Milestone                                                            | Time   |
| -------------------------------------------------------------------- | ------ |
| Prebid bundle + shim installed (deferred head scripts, execute ~DCL) | ~2.1 s |
| `DOMContentLoaded`                                                   | ~2.1 s |
| `window.load`                                                        | ~3.0 s |
| First Trusted Server `/auction` request                              | ~3.3 s |
| Publisher's first `requestBids`                                      | ~4.8 s |
| First non-empty ad render                                            | ~5.0 s |

Observed costs, each owned by a different lever below:

1. **The first Trusted Server auction fires ~1.2 s after `DOMContentLoaded`.** The exact trigger of the observed 3.3 s `/auction` request is not yet attributed (it did not pass through `pbjs.requestBids`, and `gpt.slim_prebid_url` is not configured on the measured property). Lever C starts with a discovery task to attribute it precisely.
2. **Publisher `requestBids` bursts issue one `/auction` POST each.** The baseline captured two calls 1 ms apart producing two POSTs 2 ms apart. The POSTs run **concurrently**, so this is a server-load and bidder-QPS cost, not a first-render latency cost. Lever A is therefore a load/cost lever, not a latency lever.
3. **The first direct GAM ad request pays fresh connection setup** to `securepubads.g.doubleclick.net`. GPT scripts themselves are first-party proxied (the script guard rewrites the cascade), so only the direct ad request path can benefit from a warmed connection.

Out of scope: re-enabling server-side ad templates, GPT lazy-load fetch margins (publisher-coordinated), the publisher's own ad-framework init latency, and server-side auction duration tuning (PBS `tmax`).

## Billing and impression integrity (applies to every lever)

Nothing in this design may create impression, win, or billing signals for ads that never render in a slot a user could see:

- Prefetched or early-dispatched auctions are **targeting-only**: they produce bids, never renders. `nurl`/`burl` and any render-bridge activity remain tied to an actual GAM render of the slot, exactly as today.
- A prefetched bid that is never consumed expires without firing any beacon.
- Coalescing must not cause a caller's handler to render or fire beacons for another caller's ad units (see the partitioning rule in Lever A).
- Rollout guardrails (below) monitor duplicate-auction rate and beacon counts per rendered impression so any integrity regression is visible immediately.

## Design

### Lever A — `requestBids` coalescing window (opt-in; load/cost reduction)

**Objective:** reduce `/auction` request count and upstream bidder QPS for bursty publisher call patterns. Explicitly **not** claimed to reduce first-render latency; the hold can only add up to the window duration for the earliest caller, and rollout must verify render latency does not regress.

**Config:** `[integrations.prebid] request_bids_coalesce_ms` — `u32`, default `0`, validated `0..=250`. Injected into `window.__tsjs_prebid` as `requestBidsCoalesceMs`, omitted when `0`. Default `0` preserves current behavior byte-for-byte.

**Admission (merge-safety) rules.** A call is held only when all of:

- its request object consists solely of `adUnits`, `timeout`, and `bidsBackHandler`, with a non-empty explicit `adUnits` array (any other key — `ortb2`, `labels`, `adUnitCodes`, `ttlBuffer`, `auctionId`, … — flushes the pending queue synchronously, then dispatches solo, preserving call order);
- it is **not** one of the shim's own synthetic refresh auctions (their GPT watchdog deadline starts when the wrapper returns; holding them races the watchdog and can discard valid bids);
- none of its ad units contains a bid entry for a configured client-side bidder (merging would merge those bidders' native auctions too, changing their request shape and analytics; if operators later want that, it is a separate, explicit decision);
- every held ad-unit `code` is non-empty and **disjoint** from the codes already pending (the `/auction` payload builder collapses duplicate codes, keeping the first unit's media types — a merged duplicate would produce a hybrid auction neither caller requested);
- the projected serialized payload of the merged request stays under a bound with safety margin (the endpoint rejects bodies over 256 KiB; bound pending calls, total ad units, and projected bytes — flush before admitting a call that would exceed any bound).

**Deadlines.** Each call's effective deadline is absolute: queue residence counts against it. A held call with timeout `T` arriving at `t0` must reach Prebid with an adjusted timeout of `T − (dispatch − t0)`. Calls merge only when their absolute deadlines are within a compatibility tolerance; an incompatible deadline flushes the queue. A call without a timeout uses the configured default for this computation.

**Dispatch.** One underlying `requestBids` with: the union of ad units in arrival order; the minimum adjusted deadline; and a combined `bidsBackHandler` that:

1. first runs **all** Trusted Server bookkeeping for every constituent call (pending-publisher-bid registration, EID cookie sync) — before any publisher callback runs, so a publisher callback that immediately calls `pubads.refresh()` cannot observe a constituent call whose bookkeeping has not happened;
2. then invokes each caller's original handler in arrival order, passing a bid map **partitioned to that caller's ad-unit codes**, with throws isolated per handler.

**Promise and result semantics.** Prebid 10's `requestBids` returns a promise resolving `{bids, timedOut, auctionId}`. Every held call returns a promise that settles when the merged auction settles, with `bids` partitioned to the caller's codes and the shared `auctionId`. The shared auction id and merged event stream (one `auctionInit`/`auctionEnd` for N calls) are documented operator-visible changes; analytics consumers on the property see merged auctions. If the underlying dispatch throws synchronously, every pending promise rejects with that error and the queue resets.

### Lever B — GAM preconnect hint (opt-in)

**Config:** `[integrations.gpt] gam_preconnect` — `bool`, default `false`.

When enabled, GPT `head_inserts` emits `<link rel="preconnect" href="https://securepubads.g.doubleclick.net">` at head-start.

**Corrections from review, reflected in scope and claims:**

- This opens a connection to Google at head-start, **before consent resolution and on pages that may never issue an ad request**. That is a deliberate per-property privacy decision — hence config-gated, default off, for operators whose consent posture permits it.
- GPT scripts (including `pubads_impl`) are first-party proxied by the script guard; the hint can only help the **first direct ad request**. The claim is "may reduce" its connection setup; verification requires a HAR demonstrating connection reuse under the chosen credential mode.
- Credential mode matters: ad requests are cookie-credentialed, so the hint is emitted **without** `crossorigin` (credentialed preconnect). Browsers may partially perform or skip hints; this lever is best-effort by nature.

### Lever C — earlier first auction (discovery first; design contingent)

**Status: not implementation-ready.** Two review findings block a concrete design: the previously described lifecycle seam does not exist, and a raw `/auction` response cannot be "stashed and consumed" — the adapter maps responses back to Prebid bids via the requesting auction's Prebid-generated bid-request IDs.

**Phase 1 — discovery (required before any design commitment):**

- Attribute the observed 3.3 s first `/auction` request precisely (it bypassed `pbjs.requestBids`; `slim_prebid_url` is unset on the measured property). Identify the triggering module, its gate (`load` listener, GPT event, publisher call), and what state it needs.
- Determine at what point the inputs a valid first auction needs are actually available: consent readiness (see below), GPT slot set with live sizes/targeting, Prebid EIDs (collected at adapter request time), and publisher bidder params.

**Phase 2 — design options to evaluate against discovery output:**

1. **Transport-level cache behind the real `requestBids` lifecycle:** the adapter's transport layer may reuse an in-flight or completed `/auction` HTTP exchange when — and only when — the newly transformed request is byte-identical in its auction-relevant signature. Cache entries carry: exact transformed-request signature, consent fingerprint, navigation generation, creation time, expiry, and one-shot consumed state. The response must expose a signal distinguishing consent-denied no-bid from legitimate no-bid (`/auction` currently returns HTTP 200 for both), and consent-denied responses are never cacheable.
2. **Classified full early auction:** run the real Prebid auction earlier and accept that scripts, events, consent modules, identity work, and native client-side bidders all move earlier. Honest but larger; interacts with hydration-window rendering (React #418, ad-container gating in #969) because earlier auctions pull GPT refresh earlier.

**Hard requirements for either option:**

- **Consent readiness is a concrete API contract, not an assumption:** the trigger must consume the CMP signal (GPP `signalStatus: ready` / TCF `tcloaded`/`tcstring`, USP response) with a defined timeout and default action, and record the consent fingerprint used. The GPT bundle currently has no consent gate (it is documented as a future hook), so this gate must be built, not referenced.
- **No duplicate auctions:** an unresolved early request is **awaited** by the normal path (share the promise), never replaced by a second request. Cover pending→fallback→late-success, SPA navigation, and slot-destruction cases; a navigation or slot change invalidates the entry.
- **Billing integrity:** per the integrity section — early responses are targeting data only.

## Config-blob compatibility

Integration settings are retained as raw JSON in the pushed blob (`IntegrationSettings` flattens into a `HashMap`), so an explicitly configured `0`/`false` **is** present in the blob; only omitted keys are absent. `PrebidIntegrationConfig` and `GptConfig` do not `deny_unknown_fields`, so older binaries tolerate blobs carrying the new keys — no clear-before-rollback step is required. Testing includes a new-schema blob parsed by the legacy struct shape to lock this in.

## Testing

**Vitest (shim), Lever A:**

- two mergeable calls in the window → one underlying `requestBids`, union ad units, per-caller partitioned bid maps, handlers in arrival order;
- bookkeeping-before-callbacks: constituent-call registration observable before the first publisher callback runs;
- non-mergeable option keys flush then dispatch solo, preserving order;
- synthetic refresh auctions are never held (watchdog interplay covered with fake timers);
- calls containing client-side bidder entries are never held;
- duplicate/conflicting ad-unit codes flush before admission; payload-bound boundary flushes;
- unequal and absent timeouts: absolute-deadline adjustment, queue time counted, incompatible deadlines flush;
- throwing first handler does not block later handlers; synchronous dispatch failure rejects all pending promises and resets the queue;
- held-call promise resolves `{bids (partitioned), timedOut, auctionId (shared)}`;
- window `0` / absent config: existing suite passes untouched;
- callback reentrancy: a handler calling `requestBids` during the combined callback dispatches correctly.

**Real-artifact coverage:** the external-bundle integration test asserts the wrapper's promise return against real Prebid (the current test discards the return value; the unit mock returns `undefined` and must not be the only coverage).

**Rust:**

- config defaults (`request_bids_coalesce_ms = 0`, `gam_preconnect = false`) and the `0..=250` bound;
- injected-config serialization omits `requestBidsCoalesceMs` at `0`;
- GPT `head_inserts` includes the preconnect link only when `gam_preconnect = true`, without `crossorigin`;
- new-schema → legacy-schema blob compatibility test.

**Lever C tests are defined with its design after discovery** (consent denial/change, EID readiness, navigation/slot destruction, late completion, cache one-shot semantics).

## Measurement methodology

The three-run baseline motivates the work but does not gate it. Acceptance runs use: ≥10 runs per arm on the same machine and network, local Viceroy against the production origin with pre-seeded consent and an identical scripted scroll; medians compared, with a regression limit on first non-empty render (no worse than baseline median + 5%) and the target metric per lever (Lever A: `/auction` request count and burst dedup; Lever B: HAR-verified connection reuse on the first direct GAM request; Lever C: first-auction dispatch time).

## Rollout

One lever at a time, each independently config-reversible:

1. Land binary; all flags default off — zero behavior change.
2. Enable `request_bids_coalesce_ms` (50 ms) alone on the autoblog tester property. Guardrails: fill/revenue, bid rate, timeout rate, client-side bidder traffic (must be unchanged), duplicate-auction rate, beacons per rendered impression, per-slot render latency.
3. After A stabilizes, enable `gam_preconnect` alone; verify via HAR and consent-denied network activity monitoring (no pre-consent regressions beyond the documented connection).
4. Lever C follows its own spec revision after discovery.
