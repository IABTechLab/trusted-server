# Prebid Ad-Latency and Auction-Load Optimizations — Design

**Date:** 2026-08-20 (revised 2026-08-21)
**Status:** Draft (Lever A gated on a burst-trace prerequisite; Lever C gated on discovery)
**Scope:** Client-side auction properties (server-side ad templates inactive). Measurements in this spec come from a pilot news property; identifying details are kept out of this document per repository policy, and sanitized measurement artifacts live outside the spec.

## Problem

On properties running the client-side auction path (`creative_opportunities.enabled = false`), ad delivery leaves measurable headroom. A three-run instrumented baseline against a local Trusted Server proxying the pilot property's origin (article page, consent resolved, reader-style scrolling) measured:

| Milestone                                                            | Time   |
| -------------------------------------------------------------------- | ------ |
| Prebid bundle + shim installed (deferred head scripts, execute ~DCL) | ~2.1 s |
| `DOMContentLoaded`                                                   | ~2.1 s |
| `window.load`                                                        | ~3.0 s |
| First Trusted Server `/auction` request                              | ~3.3 s |
| Publisher's first `requestBids`                                      | ~4.8 s |
| First non-empty ad render                                            | ~5.0 s |

Observed costs, each owned by a lever below:

1. **The first Trusted Server auction fires ~1.2 s after `DOMContentLoaded`** and did not pass through `pbjs.requestBids`. The leading in-repo hypothesis is the `window.tsjs.requestAds` path, which builds its payload from the TSJS registry, POSTs `/auction` directly, and **immediately renders returned creatives** — fetch and render are coupled there. Lever C's discovery must confirm or refute this before any design commitment.
2. **Publisher `requestBids` bursts issue one `/auction` POST each.** The baseline captured two calls 1 ms apart producing two POSTs 2 ms apart. The POSTs run **concurrently**, so this is a load-reduction hypothesis, not a first-render latency lever. The baseline did **not** record the burst calls' option keys, ad-unit codes, bidder entries, timeouts, or payload sizes — the exact properties that decide merge eligibility — so Lever A carries a trace prerequisite (below).
3. **The first direct GAM ad request pays fresh connection setup** to `securepubads.g.doubleclick.net`. GPT scripts themselves are first-party proxied (the script guard rewrites the cascade), so only the direct ad request path can benefit from a warmed connection.

Out of scope: re-enabling server-side ad templates, GPT lazy-load fetch margins (publisher-coordinated), the publisher's own ad-framework init latency, and server-side auction duration tuning (PBS `tmax`).

## Billing and impression integrity (applies to every lever)

Nothing in this design may create impression, win, or billing signals for ads that never render in a slot a user could see. Because billing signals differ per delivery path, the requirement is stated per path:

- **Client `/auction` → Prebid adapter path:** the `/auction` response serializer does not propagate explicit `nurl`/`burl` to this consumer, and win notification is owned by Prebid/GAM rendering. Early or coalesced auctions on this path are targeting-only by construction; the invariant to preserve is that no lever triggers `pbjs` render or GPT refresh for units the publisher did not ask to render.
- **`tsjs.requestAds` path:** fetch and render are currently coupled. If discovery selects this path for Lever C, fetch must be **split from render** first; an early fetch must never trigger its render half.
- **Server-side notices:** some PBS deployments fire win/billing notices server-side, outside browser control. Any early-auction design must state whether the upstream configuration can bill on auction rather than render; properties where that is true are **excluded** from early auctions until the upstream policy is confirmed render-tied (OpenRTB leaves billing timing exchange-specific).
- **The PUC render bridge** (which fires beacons after posting a creative response, without proof of pixel render) consumes the server-template `tsjs.bids` path — inactive in this scope. It is listed here only to record that its beacon semantics are not the integrity boundary this spec relies on.
- A prefetched bid that is never consumed expires without firing any beacon; coalescing must not cause one caller's handler to render another caller's units (partitioning rule in Lever A, with the global-state caveat below).
- Guardrails use **per-path computable signals** (Lever A: `/auction` count vs. rendered-slot count from the harness; no cross-path "beacons per impression" universal metric is claimed).

## Design

### Lever A — `requestBids` coalescing window (opt-in; load-reduction hypothesis)

**Phase 0 — trace prerequisite (blocks implementation).** Capture a sanitized trace of the production burst calls: full option-key set, per-call ad-unit codes and bid entries, effective timeouts, projected payload sizes, and callback behavior. Implementation proceeds only if the observed calls satisfy every admission predicate below. If the burst turns out to be **same-code duplicate auctions** (which the shim supports today and the disjoint-code rule deliberately refuses to merge), Lever A as specified reduces nothing; the follow-up decision is then identical-request deduplication as a separate design, or dropping the lever.

**Objective:** reduce `/auction` request count for bursty, disjoint-unit publisher call patterns. Downstream bidder-call reduction is a **hypothesis to measure**, not a claim: one PBS request with multiple impressions does not guarantee every PBS bidder adapter issues fewer HTTP calls. Explicitly not a first-render latency lever; rollout must verify render latency does not regress.

**Config:** `[integrations.prebid] request_bids_coalesce_ms` — `u32`, default `0`, validated `0..=250`. Injected as `requestBidsCoalesceMs`, omitted when `0`. Default `0` preserves current behavior byte-for-byte.

**Admission rules.** A call is held only when all of:

- its request object consists solely of `adUnits`, `timeout`, and `bidsBackHandler`, with a non-empty explicit `adUnits` array;
- `timeout` is absent or a finite positive integer (zero, negative, `NaN`, `Infinity`, or non-number values dispatch solo unchanged);
- it is not one of the shim's own synthetic refresh auctions (their GPT watchdog starts when the wrapper returns);
- no ad unit contains a bid entry for a configured client-side bidder;
- ad-unit codes are non-empty strings, **unique within the call**, and **disjoint from every code already pending** (the `/auction` payload builder collapses duplicate codes, keeping the first unit's media types);
- batch bounds hold after admission: at most 4 pending calls, at most 32 total ad units, and a projected serialized payload of at most 192 KiB UTF-8 (64 KiB safety margin under the endpoint's 256 KiB limit).

**Ineligible arrivals — one rule for all of them:** any call that fails any admission predicate (extra option keys, synthetic refresh, client-side bidders, invalid timeout, code collision, size overflow, serialization failure) **first synchronously flushes the pending batch, then dispatches solo**. Nothing ever overtakes an earlier caller; event order is preserved.

**Deadlines.**

- At enqueue, capture the live `pbjs.getConfig('bidderTimeout')`; a call without a timeout uses that captured value for deadline math.
- Each call's deadline is absolute: `arrival + effectiveTimeout`. Queue residence counts against it.
- Calls merge only when absolute deadlines agree within a **50 ms tolerance**; an incompatible deadline flushes the queue first. Later-arriving compatible callers accept the batch's earlier shared deadline and the shared `timedOut` result — documented behavior.
- A monotonic scheduler flushes at `min(windowEnd, earliestDeadline − 100 ms safety margin)` and re-arms if a new caller tightens the earliest deadline.
- The dispatched timeout is `earliestDeadline − now`, floored at **50 ms**; it is never `0` or negative (Prebid evaluates `timeout || bidderTimeout`, so `0` would silently restore the full global timeout). If the event loop wakes past a deadline (timer throttling, long tasks), dispatch immediately with the floor.

**Dispatch-time revalidation.** The shim mutates publisher ad-unit objects in place, and publishers can mutate them further while a call is held; the adapter also builds the final payload (including then-current EIDs) only at dispatch. Every admission predicate — option keys, codes, bidders, bounds, projected serialized size — is therefore **re-checked at dispatch** against the live objects. A call that no longer qualifies is evicted from the batch and dispatched solo (current behavior); values that fail serialization (cycles, throwing getters/`toJSON`) are treated the same way.

**Dispatch.** One underlying `requestBids` with: the union of ad units in arrival order; the floored shared deadline; and a combined `bidsBackHandler` that:

1. runs Trusted Server bookkeeping for every constituent call first — each call keeps **its own registration ID**, so the existing throw-rollback semantics are preserved per caller;
2. invokes each caller's original handler in arrival order with callback `this` and the exact three arguments `(bids, timedOut, auctionId)`, with `bids` partitioned to that caller's codes and `timedOut`/`auctionId` shared. A throwing handler rolls back **only its own** registration (matching today's single-call behavior) and does not block later handlers.

**Global-state caveat (documented behavior change).** Partitioned callbacks do not partition Prebid's global auction state: all merged bids belong to one auction, so a publisher callback that calls `pbjs.setTargetingForGPTAsync()` without codes applies targeting for every returned unit, and a bare `pubads.refresh()` can deliver another caller's units — all constituent calls' bookkeeping is registered before callbacks precisely so such a refresh is attributed as publisher delivery for every affected unit. Operators enabling the flag accept this shared-auction visibility; the test plan exercises unscoped targeting plus bare and mixed-slot refreshes from the first callback.

**Promise and result semantics.** Every held call returns a promise settling with `{bids (partitioned), timedOut (shared), auctionId (shared)}`. A synchronous dispatch failure rejects all pending promises with that error; asynchronous rejection of the underlying promise fans out to all held promises; the queue resets in a `finally`. One `auctionInit`/`auctionEnd` event stream replaces N — a documented, operator-visible analytics change.

### Lever B — GAM preconnect hint (opt-in)

**Config:** `[integrations.gpt] gam_preconnect` — `bool`, default `false`.

When enabled, GPT `head_inserts` emits `<link rel="preconnect" href="https://securepubads.g.doubleclick.net">` **before the GPT bootstrap inserts** (asserted by a transformed-HTML ordering test), without `crossorigin` — ad requests are cookie-credentialed, and the HTML preconnect algorithm keeps credentialed and anonymous connections distinct. Browsers may partially perform or skip hints; best-effort by nature.

**Scope of claim:** GPT scripts (including `pubads_impl`) are first-party proxied by the script guard; the hint can only affect the **first direct ad request**, and the claim is "may reduce" its connection setup.

**Governance (required before any property enables it):**

- A per-property, per-jurisdiction approval that pre-consent DNS/TCP/TLS/SNI contact with Google is permitted, with a named approver — this hint fires at head-start on pages that may never issue an ad request.
- Verification uses browser **net logging**, not HAR alone: prove connection reuse by the first ad request under the credentialed mode, and prove **zero HTTP request bytes** are transmitted during speculation, including with GPC set and with the CMP unresolved or denied.
- Rollback trigger: any observed HTTP request on the speculative connection before the normal ad request disables the flag.

**Config-surface note:** environment overrides cannot create absent GPT config leaves today (documented in the GPT guide); enabling per-property therefore goes through the pushed app-config blob, and the implementation updates `trusted-server.example.toml`, the configuration tables, and the GPT/Prebid guides.

### Lever C — earlier first auction (discovery first; design contingent)

**Status: not implementation-ready.**

**Phase 1 — discovery (required):**

- Attribute the 3.3 s `/auction` request precisely. **Named hypothesis:** the `tsjs.requestAds` path (posts `/auction` directly from the TSJS registry and immediately renders returned creatives). If confirmed, both Phase 2 options below are mis-scoped as written: moving `requestAds` earlier moves **rendering** earlier too, and its registry-built payload is not interchangeable with a later Prebid-adapter auction. The design must then first split fetch from render, and decide whether an independent non-Prebid auction on the page should be deduplicated or removed rather than accelerated.
- Identify the actual transport owner for any "adapter transport cache": today the Prebid adapter returns a request descriptor and **Prebid core owns the HTTP operation**; the repository-owned `sendAuction` belongs to the separate core API. A cache needs a named interception point in one of those owners.
- Determine when a valid first auction's inputs exist: consent (see below), GPT slot set with live sizes/targeting, Prebid EIDs (collected at adapter request time), publisher bidder params.

**Consent and equivalence model (server-owned inputs included):**

- CMP readiness in the browser is not the state the server consumes: the `/auction` body carries no consent envelope; the server reconstructs consent from cookies and `Sec-GPC`. A CMP can report granted while its cookie is absent or stale, so an early request could receive a consent-denied no-bid. Discovery must define how CMP readiness becomes the exact server-visible state — **cookie parity or an explicit validated consent envelope** — before any early dispatch.
- `/auction` returns HTTP 200 for both consent-denied and legitimate no-bid; a distinguishing response signal is required, and consent-denied responses are never reusable.
- Request equivalence is **not** body-byte equality: the server consumes EC identity, `ts-eids` fallback, KV-resolved EIDs, geo, IP, user agent, page identity, consent policy, and server configuration outside the body. Completed-response reuse is rejected unless the server provides a bounded, server-owned cache token covering those inputs. In-flight sharing ("no duplicate auctions": the normal path awaits the early request's promise) applies only while the equivalence snapshot — including consent and identity — is unchanged; a consent or identity change while pending invalidates/aborts the early request and runs a fresh normal one.
- Cache entries (if any) carry: transformed-request signature, consent fingerprint, navigation generation, creation time, expiry, one-shot consumed state.
- Billing integrity per the section above; on the `requestAds` path, split fetch from render before any reuse.

## Config-blob compatibility

Integration settings are retained as raw JSON in the pushed blob (`IntegrationSettings` flattens into a `HashMap`), so an explicitly configured `0`/`false` is present in the blob; only omitted keys are absent. `PrebidIntegrationConfig` and `GptConfig` do not `deny_unknown_fields`, so older binaries tolerate blobs carrying the new keys — no clear-before-rollback step. Testing includes a new-schema blob parsed by the legacy struct shape.

**Runtime note:** pages read the injected Prebid config once at load; a config rollback affects new navigations, not already-open pages.

## Testing

**Vitest (shim), Lever A:**

- two mergeable calls → one underlying `requestBids`, union units, per-caller partitioned maps, handlers in arrival order with preserved `this` and exact `(bids, timedOut, auctionId)` arguments;
- bookkeeping-before-callbacks with **per-caller registration IDs**: a throwing first handler rolls back only its own registration, later handlers still run, and the existing throw-rollback regression test still passes;
- every ineligible-arrival class (extra keys, synthetic refresh, client-side bidders, invalid timeout, duplicate/colliding codes, oversized payload, serialization failure) flushes the pending batch first, then dispatches solo — ordering asserted;
- global-state scenario: first callback performs unscoped `setTargetingForGPTAsync()` plus bare and mixed-slot `refresh()`; asserted against the documented shared-auction behavior;
- deadline math: timeout shorter than the window, zero/negative/`NaN` timeouts dispatch solo, `bidderTimeout` captured at enqueue survives a mid-hold config change, later caller with earlier deadline re-arms the scheduler, timer overshoot dispatches with the 50 ms floor (never `0`);
- dispatch-time revalidation: in-place mutation during the hold (code change, added client bidder, size growth) evicts to solo dispatch; cyclic/getter/`toJSON` failures evict to solo;
- promise semantics: async rejection fan-out, queue reset in `finally`, window `0`/absent config leaves the existing suite untouched;
- callback reentrancy: a handler calling `requestBids` during the combined callback.

**Real-artifact coverage (external bundle):** drive two real calls and prove one fetch, two thenables, callback-before-promise ordering, partitioned results, a single event sequence, shared timeout/auction-id semantics, and rejection fan-out. (The unit mock returns `undefined` and the current artifact test discards the return value; neither may be the only promise coverage.)

**Rust:**

- config defaults and bounds; injected-config serialization omits `requestBidsCoalesceMs` at `0`;
- `gam_preconnect = true` emits the link without `crossorigin` and **before** the GPT bootstrap inserts (transformed-HTML ordering test); `false` emits nothing;
- new-schema → legacy-schema blob compatibility test.

**Documentation checklist:** `trusted-server.example.toml`, configuration tables, Prebid and GPT integration guides, environment-overlay behavior note for GPT leaves.

**Lever C tests are defined with its design after discovery.**

## Measurement methodology

The three-run baseline motivates the work but does not gate it. Acceptance uses a reproducible harness (local Viceroy against the pilot origin, pre-seeded consent, identical scripted scroll) with **alternating paired control/treatment arms** on the same machine and network, explicit warm/cold connection conditions, ≥10 pairs per comparison, reporting p50/p95 and dispersion.

Numeric gates:

- **Lever A retains if:** ≥30% of observed eligible bursts merge (per the Phase 0 trace definition), total `/auction` requests per page drop ≥20%, and first-non-empty-render p95 regresses <5% vs. paired control. Otherwise the flag returns to `0`.
- **Lever B retains if:** net logs show connection reuse on the first direct GAM ad request in ≥70% of cold-start runs with zero pre-ad-request HTTP bytes; rollback on any integrity violation regardless of performance.
- Server telemetry currently labels all POSTs `auction_api` with no experiment-arm or navigation join key, so acceptance is **harness-based**; live guardrails during rollout are property-level trends (fill/revenue, bid rate, timeout rate, client-bidder traffic, duplicate-auction rate, per-slot render latency, consent-denied network activity) with a minimum 7-day hold per lever.

## Rollout

One lever at a time, each independently config-reversible (new navigations only, per the runtime note):

1. Land binary; all flags default off — zero behavior change.
2. Lever A Phase 0 trace on the pilot property. If predicates hold: enable `request_bids_coalesce_ms = 50` alone; hold ≥7 days against the guardrails; on failure return to `0` and **drain** (confirm zero merged auctions) before any further lever.
3. With A either retained-and-stable or fully drained to `0`: enable `gam_preconnect` alone under its governance contract.
4. Lever C follows its own spec revision after discovery.
