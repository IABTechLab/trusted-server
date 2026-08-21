# Prebid Ad-Latency and Auction-Load Optimizations — Design

**Date:** 2026-08-20 (revised 2026-08-21, round 3)
**Status:** Draft (Lever A gated on a burst-trace prerequisite; Lever C gated on discovery)
**Scope:** Client-side auction properties (server-side ad templates inactive). Measurements come from a pilot news property; identifying details stay out of this document per repository policy, and sanitized measurement artifacts live outside the spec. The pilot rollout is scoped to the Fastly adapter (see the activation matrix).

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
2. **Publisher `requestBids` bursts issue one `/auction` POST each.** The baseline captured two calls 1 ms apart producing two POSTs 2 ms apart. The POSTs run **concurrently**, so this is a load-reduction hypothesis, not a first-render latency lever. The baseline did **not** record the burst calls' option keys, ad-unit codes, bidder entries, timeouts, payload sizes, or callback behavior — the properties that decide merge eligibility — so Lever A carries a trace prerequisite (below).
3. **The first direct GAM ad request pays fresh connection setup** to `securepubads.g.doubleclick.net`. GPT scripts themselves are first-party proxied (the script guard rewrites the cascade), so only the direct ad request path can benefit from a warmed connection.

Out of scope: re-enabling server-side ad templates, GPT lazy-load fetch margins (publisher-coordinated), the publisher's own ad-framework init latency, and server-side auction duration tuning (PBS `tmax`).

## Billing and impression integrity (applies to every lever)

Nothing in this design may create impression, win, or billing signals for ad units that **no caller requested to auction**. Signals differ per delivery path:

- **Client `/auction` → Prebid adapter path:** the `/auction` response serializer does not propagate explicit `nurl`/`burl` to this consumer; win notification is owned by Prebid/GAM rendering. Early or coalesced auctions on this path are targeting-only by construction. Within a merged auction, **cross-caller delivery is accepted behavior** (see Lever A's shared-auction section): every merged unit was requested by _some_ constituent caller, and a publisher callback using unscoped targeting or a bare refresh may deliver units from a co-merged caller. What remains forbidden is delivery of units absent from every constituent call.
- **`tsjs.requestAds` path:** fetch and render are currently coupled. If discovery selects this path for Lever C, fetch must be **split from render** first; an early fetch must never trigger its render half.
- **Server-side notices:** some PBS deployments fire win/billing notices server-side, outside browser control. Any early-auction design must state whether the upstream configuration can bill on auction rather than render; properties where that is true are **excluded** from early auctions until the upstream policy is confirmed render-tied (OpenRTB leaves billing timing exchange-specific).
- **The PUC render bridge** (which fires beacons after posting a creative response, without proof of pixel render) consumes the server-template `tsjs.bids` path — inactive in this scope; its beacon semantics are not this spec's integrity boundary.
- A prefetched bid that is never consumed expires without firing any beacon.
- Guardrails use **per-path computable signals** (Lever A: `/auction` count vs. rendered-slot count from the harness); no cross-path "beacons per impression" universal metric is claimed.

## Design

### Lever A — `requestBids` coalescing window (opt-in; load-reduction hypothesis)

**Phase 0 — trace prerequisite (blocks implementation).** Capture a sanitized trace of the production burst calls recording: full option-key set, per-call ad-unit codes and bid entries, effective timeouts, projected payload sizes, and **callback behavior** — specifically whether callbacks use code-scoped targeting and slot-scoped refresh, or unscoped targeting / bare `pubads.refresh()`. Implementation proceeds only if the observed calls satisfy every admission predicate below **and** the callback discipline is compatible with the shared-auction behavior:

- If the burst is **same-code duplicate auctions** (supported today; the disjoint-code rule refuses to merge them), Lever A as specified reduces nothing — the follow-up is identical-request deduplication as a separate design, or dropping the lever.
- If callbacks perform **bare refreshes**, note the interaction: the first caller's bare refresh consumes the one-shot pending-delivery state for _all_ merged units, and a second caller's subsequent common refresh pattern can then be classified as an independent refresh and start a **synthetic auction** — potentially cancelling the load reduction. Phase 0 must estimate the net `/auction` effect under the observed callback pattern; if the net is not clearly positive, the lever stops here.

**Objective:** reduce `/auction` request count for bursty, disjoint-unit publisher call patterns. Downstream bidder-call reduction is a **hypothesis to measure**: one PBS request with multiple impressions does not guarantee every PBS bidder adapter issues fewer HTTP calls. Explicitly not a first-render latency lever; rollout must verify render latency does not regress.

**Config:** `[integrations.prebid] request_bids_coalesce_ms` — `u32`, default `0`, validated `0..=250`. Injected as `requestBidsCoalesceMs`, omitted when `0`. Default `0` preserves current behavior byte-for-byte.

**Snapshot at enqueue.** The coalescer snapshots each admitted request the way Prebid itself does on entry: shallow-copy the request object and snapshot the ad-unit **array membership and order** (retaining unit references). Later additions of request keys or array push/splice do not retroactively change the issued call; unit-object mutations are caught by dispatch-time revalidation.

**Admission rules.** A call is held only when all of:

- its request object consists solely of `adUnits`, `timeout`, and `bidsBackHandler`, with a non-empty explicit `adUnits` array;
- `timeout` is absent or a finite positive integer, **and** the resulting auction-time budget stays above the solo-dispatch threshold: a call whose effective timeout is less than `window + 150 ms` dispatches solo (holding it would consume its budget; see Deadlines);
- it is not one of the shim's own synthetic refresh auctions (their GPT watchdog starts when the wrapper returns);
- no ad unit contains a bid entry for a configured client-side bidder;
- ad-unit codes are non-empty strings, **unique within the call**, and **disjoint from every code already pending** (the `/auction` payload builder collapses duplicate codes, keeping the first unit's media types);
- batch bounds hold after admission: at most 4 pending calls, at most 32 total ad units, and a projected payload within the size budget below.

**Payload size budget.** "Projected serialized payload" is defined as the UTF-8 byte length of `JSON.stringify` applied to the `/auction`-shaped body built from the snapshotted units at admission, re-computed at dispatch. Budget: projected units ≤ **160 KiB**, plus a **32 KiB reserved allowance** for adapter-added inputs (EIDs, context config — which the shim does not bound), leaving ≥ 64 KiB margin under the endpoint's 256 KiB limit. This is a best-effort bound, not a guarantee: if the server still rejects the merged body with 413, the coalescer **re-dispatches each constituent call solo, once** — no caller silently fails because of merging. Serialization failures (cycles, throwing getters/`toJSON`) at admission or dispatch evict that call to solo dispatch.

**Ineligible arrivals — one rule for all classes:** any call failing any admission predicate (extra option keys, synthetic refresh, client-side bidders, invalid or sub-threshold timeout, code collision, size overflow, serialization failure) **first synchronously flushes the pending batch, then dispatches solo**. Nothing overtakes an earlier caller; event order is preserved.

**Deadlines — a reduced auction-time budget, not an absolute deadline.** Prebid starts its auction timer only after request hooks and FPD enrichment, so no wrapper can guarantee completion by `arrival + timeout`; this design shapes the _budget_ instead:

- At enqueue, capture the live `pbjs.getConfig('bidderTimeout')`. If it is missing or not a finite positive integer, calls without an explicit timeout dispatch solo (no deadline math is possible for them).
- Each call's nominal deadline is `arrival + effectiveTimeout`; queue residence counts against the budget.
- Compatibility: calls merge only while `maxDeadline − minDeadline ≤ 50 ms`; an incompatible arrival flushes first. Later-arriving compatible callers accept the batch's earlier shared budget and shared `timedOut` result — documented behavior.
- A monotonic scheduler flushes at `min(windowEnd, earliestDeadline − 100 ms)` and re-arms if a new caller tightens the earliest deadline.
- The dispatched timeout is `earliestDeadline − now`, floored at **50 ms** — never `0` (Prebid's `timeout || bidderTimeout` would silently restore the global default). The floor is reachable only through timer overshoot (throttling, long tasks), because sub-threshold budgets were never admitted; overshoot means the auction runs up to ~50 ms past the nominal deadline, which is accepted and documented.

**Dispatch-time revalidation and order-preserving eviction.** All predicates are re-checked at dispatch against the live unit objects (the shim mutates units in place and publishers can too). Revalidation walks the queue **in arrival order** and dispatches **contiguous eligible segments**, with each invalid call dispatched solo in its queue position: for A(valid), B(now-invalid), C(valid), the dispatch order is merged-[A], solo-B, merged-[C] — never a reordering. (A single-segment queue with one invalid member yields exactly today's per-call behavior for that member.)

**Queue lifecycle (reentrancy-safe).** At flush, the pending batch is **atomically detached** from the queue _before_ the underlying `requestBids` is invoked; settlement handlers own only the detached batch and never touch newer queue state. Prebid invokes `bidsBackHandler` before resolving its public promise, so a constituent callback may re-enter `requestBids` and start a new batch while the first is settling — the detached-batch rule makes that safe, and the reentrancy test must prove the second batch dispatches and settles.

**Dispatch.** One underlying `requestBids` per eligible segment with: the segment's units in arrival order; the floored shared budget; and a combined `bidsBackHandler` that:

1. runs Trusted Server bookkeeping for every constituent call first — each call keeps **its own registration ID**, preserving today's per-caller throw-rollback;
2. invokes each caller's original handler in arrival order with callback `this` and the exact three arguments `(bids, timedOut, auctionId)`. When `bids` is an object it is partitioned to the caller's codes; when Prebid supplies `undefined` (cancelled auction), `undefined` is passed through unaltered — as are `timedOut`/`auctionId`. A throwing handler rolls back only its own registration and does not block later handlers; the exception is reported by asynchronous rethrow (matching a lone call's observable behavior) and **does not reject any caller's facade promise**.

**Shared-auction behavior (documented, accepted).** Partitioned callbacks do not partition Prebid's global auction state: merged bids belong to one auction, so unscoped `setTargetingForGPTAsync()` applies targeting for every returned unit and a bare `pubads.refresh()` can deliver a co-merged caller's units — attributed as publisher delivery because all constituent bookkeeping registers first. Operators enabling the flag accept this; the integrity section and acceptance tests treat cross-caller delivery within a merged auction as permitted.

**Promise semantics.** Every held call returns a facade promise settling with the values described above. A synchronous dispatch failure rejects that segment's facade promises and resets only the detached segment. Prebid's public promise is resolve-only, so rejection fan-out is defensive unit/mock coverage — it cannot be proven against the real artifact without replacing the API under test. One `auctionInit`/`auctionEnd` event stream replaces N per merged segment — a documented, operator-visible analytics change.

### Lever B — GAM preconnect hint (opt-in)

**Config:** `[integrations.gpt] gam_preconnect` — `bool`, default `false`.

When enabled, GPT `head_inserts` emits `<link rel="preconnect" href="https://securepubads.g.doubleclick.net">` **before the GPT bootstrap inserts** (asserted by a transformed-HTML ordering test), without `crossorigin` — ad requests are cookie-credentialed, and the HTML preconnect algorithm keeps credentialed and anonymous connections distinct. Browsers may partially perform or skip hints; best-effort by nature.

**Scope of claim:** GPT scripts (including `pubads_impl`) are first-party proxied by the script guard; the hint can only affect the **first direct ad request**, and the claim is "may reduce" its connection setup.

**Governance (required before any property enables it):**

- The flag is a property-level boolean and GPT head insertion has no request-scoped jurisdiction or consent input; therefore the approval must cover **every jurisdiction served by the deployed configuration**, with a named approver. (Per-jurisdiction selectivity would require request-scoped consent/geo input to head insertion — out of scope here and stated as such.)
- **Verification protocol (NetLog):** pinned browser version, fresh profile per run, cache and socket pools cold, `--log-net-log` capture with sensitive data excluded, H2/H3/QUIC classified, and the speculative socket joined to the first GAM request via NetLog source IDs. Raw logs are redacted (no cookies/URLs beyond the GAM host) and retained only for the acceptance window.
- **Enforceable invariant:** no HTTP request HEADERS/DATA frames on the speculative connection before the normal GAM ad request. (Connection-level protocol frames — settings, pings — are inherent to preconnect and permitted.) Verified including GPC-set, CMP-unresolved, and CMP-denied cases.
- Rollback trigger: any observed request frames before the normal ad request disables the flag.

### Lever C — earlier first auction (discovery first; design contingent)

**Status: not implementation-ready.** No design option is selected in this revision; discovery below produces the inputs for a follow-up spec revision that will select one.

**Discovery contract:**

- **Attribute the 3.3 s `/auction` request.** Named hypothesis: `tsjs.requestAds` (posts `/auction` directly from the TSJS registry, invokes its callback, renders asynchronously). If confirmed, acceleration is mis-scoped until fetch is split from render, and the prior question becomes whether an independent non-Prebid auction on the page should be **deduplicated or removed** rather than accelerated.
- **Typed direct-path result.** `requestAds` returns `void` and `sendAuction` collapses network failure, parse failure, and legitimate emptiness into `[]`. A fetch/render split requires a private result type — e.g. `{outcome, bids, completedAt}` — distinguishing those cases while preserving the public callback lifecycle.
- **Transport ownership.** The Prebid adapter returns a request descriptor and **Prebid core owns that HTTP operation**; the repository-owned `sendAuction` belongs to the separate core API. Any cache/single-flight needs a named owner at one of those seams — and **server-side single-flight at the common auction boundary is the preferred shape**, because the browser cannot observe the HttpOnly EC identity, server-side KV/geo/identity-graph state, or configuration changes that determine auction equivalence. If a client-side mechanism is chosen anyway, it requires a short-lived, opaque, authenticated, navigation-bound server-issued token that reveals no stable identity and carries capability/configuration versioning.
- **No completed-response reuse initially.** The `/auction` response carries no bid lifetime and the adapter stamps a fresh `ttl: 300` at interpretation time, so reusing an old response silently renews its apparent lifetime. Reuse (if ever supported) requires completion time and per-bid expiry in the response, with TTL set to remaining lifetime. Until then, only **in-flight sharing** is on the table: the normal path awaits the early request's promise, valid only while the equivalence snapshot (consent, identity, navigation) is unchanged; any change invalidates/aborts and a fresh normal request runs.
- **Consent parity.** CMP readiness in the browser is not the state the server consumes: the `/auction` body carries no consent envelope; the server reconstructs consent from cookies and `Sec-GPC`. Discovery must define cookie parity or a validated consent envelope, and `/auction` needs a response signal distinguishing consent-denied from legitimate no-bid (both are HTTP 200 today); consent-denied results are never shareable.
- **Input ownership tables.** Producer/consumer/invalidation tables per path: TSJS registry generations, Prebid ad-unit generations, navigation generation, render targets. GPT slot targeting is listed as an auction input only if discovery proves it affects request bytes.
- **Billing integrity** per the section above; on the `requestAds` path, split fetch from render before any reuse.

## Config-blob compatibility

Integration settings are retained as raw JSON in the pushed blob (`IntegrationSettings` flattens into a `HashMap`), so an explicitly configured `0`/`false` is present in the blob; only omitted keys are absent. `PrebidIntegrationConfig` and `GptConfig` do not `deny_unknown_fields`, so older binaries tolerate blobs carrying the new keys — no clear-before-rollback step. Testing includes a new-schema blob parsed by the legacy struct shape.

**Adapter activation/rollback matrix.** Injected client config is read once per document, so _within_ an adapter a config change affects new navigations only. Across adapters, activation differs: Fastly instances are effectively per-request (config push suffices); Axum builds shared state at startup, and Cloudflare and Spin similarly hold startup state (config change requires restart/redeploy). **The pilot rollout is scoped to Fastly**; the other adapters inherit the flags but their activation path is restart-based and out of the pilot's scope.

## Testing

**Vitest (shim), Lever A:**

- two mergeable calls → one underlying `requestBids`, segment units in arrival order, per-caller partitioned maps, handlers in arrival order with preserved `this` and exact `(bids, timedOut, auctionId)` arguments; cancelled-auction `undefined` values passed through unpartitioned;
- bookkeeping-before-callbacks with per-caller registration IDs; a throwing first handler rolls back only its own registration, later handlers run, no facade promise rejects, the exception surfaces via async rethrow, and the existing throw-rollback regression test still passes;
- every ineligible-arrival class flushes the pending batch first, then dispatches solo — ordering asserted;
- **order-preserving eviction:** middle-call mutation during the hold produces merged-[A], solo-B, merged-[C] dispatch and event order;
- shared-auction scenario: first callback performs unscoped `setTargetingForGPTAsync()` plus bare and mixed-slot `refresh()`; asserts the documented cross-caller delivery, pending-state consumption, and any resulting synthetic-auction classification (net `/auction` count asserted);
- **reentrancy:** a constituent callback enqueues a new batch while the first settles; the detached-batch rule is proven — the second batch dispatches and settles, and the first batch's cleanup does not clear it or its timer;
- deadline math: sub-threshold timeout dispatches solo; zero/negative/`NaN`/`Infinity` dispatch solo; missing/invalid captured `bidderTimeout` sends timeout-less calls solo; `bidderTimeout` captured at enqueue survives a mid-hold config change; a later caller with an earlier deadline re-arms the scheduler; timer overshoot dispatches with the 50 ms floor (never `0`), documented as budget overrun;
- snapshot semantics: post-enqueue request-key additions and ad-unit array push/splice do not alter the issued call; unit-object mutations are caught at revalidation;
- payload budget: boundary−1 / boundary / boundary+1 projections, multibyte (UTF-8) content, oversized EID allowance behavior, and the 413 → re-dispatch-solo-once fallback;
- async rejection fan-out and `finally`-scoped cleanup of the detached segment (defensive unit coverage; not provable against the real artifact);
- window `0` / absent config leaves the existing suite untouched.

**Real-artifact coverage (external bundle):** drive two real calls and prove one fetch, two thenables, callback-before-promise ordering, partitioned results, a single event sequence, and shared timeout/auction-id semantics. (Rejection fan-out stays in unit coverage; the real public promise is resolve-only.)

**Rust:**

- config defaults and bounds; injected-config serialization omits `requestBidsCoalesceMs` at `0`;
- `gam_preconnect = true` emits the link without `crossorigin` and **before** the GPT bootstrap inserts (transformed-HTML ordering test); `false` emits nothing;
- new-schema → legacy-schema blob compatibility test.

**Documentation checklist:** `trusted-server.example.toml`, configuration tables, Prebid and GPT integration guides, and environment-overlay leaf-creation behavior for **both** new fields (Prebid and GPT).

**Lever C tests are defined with its design after discovery.**

## Measurement methodology

The three-run baseline motivates the work but does not gate it. Acceptance uses a reproducible harness (local Viceroy against the pilot origin, pre-seeded consent, identical scripted scroll) with **randomized, balanced AB/BA pair ordering** on the same machine and network, explicit warm/cold connection conditions, and a sample size derived from pilot variance (a pilot batch of ≥10 pairs estimates variance; the acceptance batch is sized for 80% power on the stated effect, and is never smaller than 20 pairs). Decisions use **paired confidence bounds**, reporting p50/p95 with dispersion.

**Denominators, defined:** a _run_ is one full harness execution (fresh context); a _navigation_ is one document load within a run; an _eligible burst_ is a set of ≥2 `requestBids` calls within the configured window on one navigation that satisfy every admission predicate per the Phase 0 trace definition.

Numeric gates:

- **Lever A retains if:** ≥30% of eligible bursts merge, total `/auction` requests per navigation drop ≥20% (paired 95% CI excluding zero), **and** first-non-empty-render p95 shows non-inferiority within a 5% margin vs. paired control. Otherwise the flag returns to `0`.
- **Lever B retains if:** NetLog shows the speculative socket serving the first direct GAM ad request in ≥70% of cold-start pairs, the request-frame invariant holds in 100% of runs (any violation is an immediate rollback regardless of performance), **and** a real benefit gate passes: paired median first-GAM-request setup-time improvement with a 95% CI excluding zero, plus first-render and page-load non-inferiority (5% margin). Socket reuse alone does not retain the flag.
- **B is measured with identical Lever A state in both arms** (whatever A's disposition is at that point — the comparison is X vs. X+B, stated explicitly in the results).

**Live guardrails and their sources.** Server telemetry today has no experiment arm, navigation ID, or coalesced-count signal, so live monitoring is explicitly split:

| Signal                                    | Source                                                                                                                              | Type                              |
| ----------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------- | --------------------------------- |
| Merged-auction count / drain state        | new `coalesced` count field the shim adds to the `/auction` request `config` object; surfaced in server auction telemetry           | new, required for Lever A rollout |
| `/auction` volume per property            | existing server telemetry                                                                                                           | existing                          |
| Fill/revenue, bid rate, timeout rate      | ad-server / PBS reporting (property-level trend vs. 7-day pre-enable baseline, alert on >5% adverse move; owner: property operator) | existing, coarse                  |
| First-render / per-slot latency           | sampled synthetic checks (scheduled harness runs against production), not RUM                                                       | synthetic                         |
| Consent-denied network activity (Lever B) | scheduled NetLog synthetic checks                                                                                                   | synthetic                         |

**Drain definition (Lever A):** after setting the flag to `0`, drain is complete when the `coalesced` signal reports zero merged auctions for a period covering the maximum expected open-document lifetime (48 h), since already-open documents keep their read-once config.

## Rollout

One lever at a time, Fastly pilot only, each independently config-reversible for new navigations:

1. Land binary; all flags default off — zero behavior change.
2. Lever A Phase 0 trace on the pilot property (including callback-discipline capture). If predicates and net-benefit hold: enable `request_bids_coalesce_ms = 50` alone; hold ≥7 days against the guardrail table; on failure return to `0` and confirm drain per the definition above.
3. With A either retained-and-stable or fully drained: enable `gam_preconnect` alone under its governance contract, measured against the then-current A state.
4. Lever C follows its own spec revision after discovery.
