# Prebid Ad-Latency and Auction-Load Optimizations — Design

**Date:** 2026-08-20 (revised 2026-08-22, round 5)
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

- **Client `/auction` → Prebid adapter path:** the `/auction` response serializer does not propagate explicit `nurl`/`burl` to this consumer; win notification is owned by Prebid/GAM rendering. Early or coalesced auctions on this path are targeting-only by construction. Within a merged auction, **cross-caller delivery is accepted behavior** (see Lever A's shared-auction section): every merged unit was requested by _some_ constituent caller. What remains forbidden is delivery of units absent from every constituent call.
- **`tsjs.requestAds` path:** fetch and render are currently coupled. If discovery selects this path for Lever C, fetch must be **split from render** first; an early fetch must never trigger its render half.
- **Server-side notices:** some PBS deployments fire win/billing notices server-side, outside browser control. Any early-auction design must state whether the upstream configuration can bill on auction rather than render; properties where that is true are **excluded** from early auctions until the upstream policy is confirmed render-tied.
- **The PUC render bridge** consumes the server-template `tsjs.bids` path — inactive in this scope; its beacon semantics are not this spec's integrity boundary.
- A prefetched bid that is never consumed expires without firing any beacon.
- Guardrails use **per-path computable signals**; no cross-path "beacons per impression" universal metric is claimed.

## Design

### Lever A — `requestBids` coalescing window (opt-in; load-reduction hypothesis)

**Phase 0 — trace prerequisite (blocks implementation).** Capture a sanitized trace of the production burst calls recording: full option-key set, per-call ad-unit codes and bid entries, effective timeouts, payload sizes, and **callback behavior** — specifically whether callbacks use code-scoped targeting and slot-scoped refresh, or unscoped targeting / bare `pubads.refresh()`. Implementation proceeds only if the observed calls satisfy every admission predicate below **and** the callback discipline is compatible with the shared-auction behavior:

- If the burst is **same-code duplicate auctions** (supported today; the disjoint-code rule refuses to merge them), Lever A as specified reduces nothing — the follow-up is identical-request deduplication as a separate design, or dropping the lever.
- If callbacks perform **bare refreshes**, the first caller's bare refresh consumes the one-shot pending-delivery state for _all_ merged units, and a second caller's subsequent refresh can be classified as an independent refresh and start a **synthetic auction** — potentially cancelling the load reduction. Phase 0 must estimate the net `/auction` effect under the observed callback pattern; if the net is not clearly positive, the lever stops here.

**Objective:** reduce `/auction` request count for bursty, disjoint-unit publisher call patterns. Downstream bidder-call reduction is a **hypothesis to measure**. Explicitly not a first-render latency lever; rollout must verify render latency does not regress.

**Config:** `[integrations.prebid] request_bids_coalesce_ms` — `u32`, default `0`, validated `0..=250`. Injected as `requestBidsCoalesceMs`, omitted when `0`. Default `0` preserves the existing synchronous pass-through path and public observable behavior (the bundle bytes necessarily change when the coalescer ships).

**Coalescing-config lifetime.** The injected coalescing config carries an issue timestamp and a maximum document lifetime (24 h). The shim self-disables coalescing when `now > issuedAt + lifetime`, so long-lived documents quiesce on their own (see Drain).

**Snapshot at enqueue.** The coalescer snapshots each admitted request the way Prebid itself does on entry: shallow-copy the request object and snapshot the ad-unit **array membership and order** (retaining unit references). Later additions of request keys or array push/splice do not retroactively change the issued call; unit-object mutations are caught by dispatch-time revalidation.

**Admission rules.** A call is held only when all of:

- its request object consists solely of `adUnits`, `timeout`, and `bidsBackHandler`, with a non-empty explicit `adUnits` array;
- `timeout` is absent or a finite positive integer, **and** the resulting auction-time budget stays above the solo-dispatch threshold: a call whose effective timeout is less than `window + 150 ms` dispatches solo;
- it is not one of the shim's own synthetic refresh auctions (their GPT watchdog starts when the wrapper returns);
- no ad unit contains a bid entry for a configured client-side bidder;
- ad-unit codes are non-empty strings, **unique within the call**, and **disjoint from every code already pending**;
- structural safety holds: every ad unit is measurable by a **side-effect-free size estimator** that walks own enumerable _data_ properties only. Units carrying accessors, custom `toJSON`, or otherwise unmeasurable values dispatch solo — publisher getter/`toJSON` code must never execute during admission (regression-tested with a stateful `toJSON`);
- batch bounds hold after admission: at most 4 pending calls, at most 32 total ad units, and an estimated unit payload ≤ 160 KiB UTF-8.

**Authoritative size bound (adapter seam).** The estimator above is a cheap pre-filter; the **authoritative** bound lives in the adapter's `buildRequests`, where the final body — including then-current EIDs and context — exists. Mechanics and guarantees:

- **Boundary ownership:** the coalescer assigns each merged segment an **internal auction ID**, passes it through the merged `requestBids` call, and keys constituent boundaries by it in a module-scoped map. `buildRequests` reads the ID from the **auction-scoped `bidderRequest`** it receives (Prebid deep-clones ad units after enrichment, so unit-attached markers are unreliable and are not used); the map entry is deleted at auction end. A test proves no boundary marker leaks into bidder params or the wire payload.
- **EID sanitization:** the adapter deep-copies collected EIDs into plain JSON data (own enumerable data properties only) **once per auction** before any serialization, so stateful getters/`toJSON` in publisher-supplied EID objects are never invoked repeatedly. EIDs that cannot be sanitized this way make the body **unsplittable** (single descriptor).
- **Splitting:** if the final body exceeds 192 KiB (64 KiB margin under the endpoint's 256 KiB limit) and contains multiple constituents, `buildRequests` splits along constituent boundaries into multiple transport descriptors — Prebid dispatches each as a separate HTTP request within the _same_ auction, preserving the one-auction/one-event-stream contract.
- **Narrowed guarantee + singleton rule:** the "no oversized request" guarantee applies to **multi-constituent** descriptors only. A single constituent whose body exceeds the limit (e.g., through large common EIDs/context) cannot be split further and **dispatches as-is — exactly the behavior an oversized solo call has today** (sent, possibly answered 413, resolving as a no-bid auction). Oversized-common-EIDs is an explicit test case.

**Ineligible arrivals — one rule for all classes:** any call failing any admission predicate **first synchronously flushes the pending batch, then dispatches solo**. Nothing overtakes an earlier caller; event order is preserved.

**Deadlines — a reduced auction-time budget, not an absolute deadline.** Prebid starts its auction timer only after request hooks and FPD enrichment, so no wrapper can guarantee completion by `arrival + timeout`; this design shapes the _budget_:

- At enqueue, capture the live `pbjs.getConfig('bidderTimeout')`. If it is missing or not a finite positive integer, calls without an explicit timeout dispatch solo.
- Each call's nominal deadline is `arrival + effectiveTimeout`; queue residence counts against the budget.
- Compatibility: calls merge only while `maxDeadline − minDeadline ≤ 50 ms`; an incompatible arrival flushes first. Later-arriving compatible callers accept the batch's earlier shared budget and shared `timedOut` result — documented behavior.
- A monotonic scheduler flushes at `min(windowEnd, earliestDeadline − 100 ms)` and re-arms if a new caller tightens the earliest deadline.
- The dispatched timeout is `earliestDeadline − now`, floored at **50 ms** — never `0`. The floor is reachable only through timer overshoot, which means the auction runs up to ~50 ms past the nominal deadline — accepted and documented.

**Dispatch-time revalidation and order-preserving eviction.** All predicates are re-checked at dispatch against the live unit objects. Revalidation walks the queue **in arrival order** and dispatches **contiguous eligible segments**, with each invalid call dispatched solo in its queue position: A(valid), B(now-invalid), C(valid) → merged-[A], solo-B, merged-[C]. A synchronous dispatch failure of one segment rejects only that segment's facade promises; **later segments still dispatch in order and settle** (tested).

**Queue lifecycle (reentrancy-safe).** At flush, the pending batch is **atomically detached** from the queue _before_ the underlying `requestBids` is invoked; settlement handlers own only the detached batch and never touch newer queue state. A constituent callback may re-enter `requestBids` and start a new batch while the first settles — the detached-batch rule makes that safe, and the reentrancy test proves the second batch dispatches and settles.

**Dispatch.** One underlying `requestBids` per eligible segment with: the segment's units in arrival order (constituent boundaries attached for the adapter seam); the floored shared budget; and a combined `bidsBackHandler` that:

1. runs Trusted Server bookkeeping for every constituent call first — each call keeps **its own registration ID**, preserving today's per-caller throw-rollback;
2. invokes each caller's original handler in arrival order with callback `this` and the exact three arguments `(bids, timedOut, auctionId)`. When `bids` is an object it is partitioned to the caller's codes; `undefined` cancelled-auction values pass through unaltered. A throwing handler rolls back only its own registration and does not block later handlers; after **all** handlers have run, the **first captured error is thrown synchronously** so Prebid's existing catch-and-log path handles it — observably matching today's lone-call behavior. No global asynchronous rethrow. Facade promises still resolve.

**Shared-auction behavior (documented, accepted).** Partitioned callbacks do not partition Prebid's global auction state: unscoped `setTargetingForGPTAsync()` applies targeting for every returned unit and a bare `pubads.refresh()` can deliver a co-merged caller's units — attributed as publisher delivery because all constituent bookkeeping registers first. Operators enabling the flag accept this; acceptance tests treat cross-caller delivery within a merged auction as permitted.

**Promise semantics.** Every held call returns a facade promise settling with the values described above. Rejection fan-out (synchronous dispatch failure) is per detached segment and is defensive unit coverage — Prebid's public promise is resolve-only, so it cannot be proven against the real artifact. One `auctionInit`/`auctionEnd` event stream per merged segment replaces N — a documented, operator-visible analytics change.

**Coalescing telemetry (required for rollout).** Merged dispatches carry a dedicated, bounded, untrusted metadata object as a **top-level sibling of** `config` in the `/auction` body (never inside the `allowed_context_keys`-filtered context): `coalesced: { group, size, part, parts }` where `group` is an opaque per-logical-merge ID, `size` is the constituent count (`2..=4`), and `part`/`parts` describe split descriptors (`1/1` when unsplit). Semantics: absent = solo/legacy; **logical merges are counted by distinct `group`**, so split descriptors never overcount; malformed or out-of-range values are **dropped as absent, never clamped** into valid observations. Server side: the endpoint parses and validates the object, `AuctionObservationContext` and the auction event summary row carry the fields, the Tinybird datasource gains the columns — **schema deployed before the emitting binary** — and a named rollup (`auction_coalescing_daily`: logical merges, constituent totals, split counts per property/day) plus a dashboard panel constitute the continuous guardrail; a raw column alone does not. Endpoint, telemetry, sink-serialization, schema, and rollup tests are part of the implementation.

### Lever B — GAM preconnect hint (opt-in)

**Config:** `[integrations.gpt] gam_preconnect` — `bool`, default `false`.

When enabled, GPT `head_inserts` emits `<link rel="preconnect" href="https://securepubads.g.doubleclick.net">` **before the GPT bootstrap inserts** (asserted by a transformed-HTML ordering test), without `crossorigin` — ad requests are cookie-credentialed. Browsers may partially perform or skip hints; best-effort by nature.

**Scope of claim:** GPT scripts (including `pubads_impl`) are first-party proxied; the hint can only affect the **first direct ad request**, and the claim is "may reduce" its connection setup.

**Browser coverage (blocking gate):** the flag emits to every browser, and per-engine emission restriction would require request-scoped UA gating (out of scope). Therefore **enablement is blocked until every engine above a 5% traffic share on the property is verified** with engine-appropriate low-level tooling (Chromium: NetLog; WebKit/Firefox: their native network logging), each with defined event predicates, a per-engine sample floor (at least 20 cold-start runs), and per-engine acceptance rules. Current repository browser coverage is Chromium-only, so building this matrix is part of the lever's cost — an unverified material engine blocks the flag; immaterial engines (below 5%) are documented as unverified in the approval.

**Governance (required before any property enables it):**

- The flag is a property-level boolean and GPT head insertion has no request-scoped jurisdiction or consent input; the approval must therefore cover **every jurisdiction served by the deployed configuration**.
- **The approval is a durable artifact** recording: configuration version, jurisdiction inventory and unknown-jurisdiction handling, verified browser matrix, approver, date, expiry, and a re-approval trigger when the served scope changes.
- **Verification protocol:** pinned browser build per engine, fresh profile per run, cold cache and socket pools, capture mode stated explicitly, raw logs treated as sensitive (redacted to the GAM host, retained only for the acceptance window). For Chromium: NetLog with the speculative socket joined to the first ad request via source IDs, using a **versioned parser algorithm with fixtures and one controlled end-to-end test**; equivalent engine-appropriate tooling for others. Current audit tooling has no NetLog surface — building this capture path is part of the lever's implementation cost.
- **Enforceable invariant:** no HTTP request writes on the speculative connection before the normal first GAM ad request — defined as no HTTP/2 or HTTP/3 HEADERS/DATA frames **and** no HTTP/1 request writes. "Normal first GAM request" = the first `securepubads.g.doubleclick.net` ad request initiated by GPT for the document. Verified including GPC-set, CMP-unresolved, and CMP-denied cases; connection-level protocol frames (settings, pings) are inherent to preconnect and permitted.
- Rollback trigger: any observed request write before the normal ad request disables the flag — an immediate-stop conformance failure regardless of performance.

### Lever C — earlier first auction (discovery first; design contingent)

**Status: not implementation-ready.** No design option is selected; discovery produces the inputs for a follow-up spec revision.

**Discovery contract:**

- **Attribute the 3.3 s `/auction` request.** Named hypothesis: `tsjs.requestAds`. If confirmed, acceleration is mis-scoped until fetch is split from render, and the prior question becomes whether an independent non-Prebid auction should be **deduplicated or removed** rather than accelerated.
- **Typed direct-path result.** `requestAds` returns `void` and `sendAuction` collapses network failure, parse failure, and legitimate emptiness into `[]`. A fetch/render split requires a private result type — `{outcome, bids, completedAt}` — preserving the public callback lifecycle.
- **Transport ownership and the Fastly constraint.** The Prebid adapter returns a request descriptor (Prebid core owns that HTTP operation); the repository-owned `sendAuction` is the separate core API. Server-side single-flight at the common auction boundary is the preferred _shape_, **but it currently has no viable Fastly owner**: Fastly application state is rebuilt per request, and the platform abstraction exposes KV/cache/HTTP but no atomic pending-join primitive. **Discovery exit criterion:** prove a cross-instance, atomic, _pending-only_ join with zero retention after settlement on Fastly. If Fastly cannot supply that contract, server-side single-flight is off the table for the pilot and the candidate becomes authenticated client-side coordination or a different architecture. An ordinary persistent-cache lookup is not an acceptable substitute — it _is_ the completed-response reuse this spec forbids.
- **Navigation-scoped reservation (both architectures).** Client- or server-side, sharing requires: an opaque, authenticated, **single-navigation reservation**; exactly one intended early/normal pair per reservation; server revalidation of hidden inputs (HttpOnly EC identity, geo, server-resolved EIDs, headers, provider mode, configuration version) on the joining request; a canonical key derived from the fully normalized provider input plus relevant headers/settings — never a content-only or stable-identity key, which could join different documents or users with identical units and expose a one-shot bid across contexts.
- **One-consumer arbitration before any pending-only rule.** The public `requestAds` path renders every returned creative; a speculative mechanism that discards an unjoined result would silently remove that render. Therefore: either the early request **is** the public call itself (its result keeps its normal consumer — never discarded, render preserved), or the speculative producer is **private** and its results carry no render obligation. Removing or changing the public path's render is only permissible as an explicitly approved breaking migration.
- **Pending-only state machine (for a private speculative producer).** Joining is allowed **only while Pending**. If no waiter attaches before settlement, the private result is **discarded immediately**; a later call starts a fresh auction; the pending-to-settled attachment race is atomic and tested. The measured gap (early ~3.3 s, publisher ~4.8 s) makes settled-before-join the _likely_ case — expected benefit is modest and must be measured before further investment. A full **outcome transition table** is a discovery deliverable: for each of bid / no-bid / consent-denial / failure / timeout / invalidation / attachment-race, define the consumer, deadline, and terminal state.
- **Client-side feasibility gate.** Prebid owns transport and request-scoped bid IDs, and the repository `sendAuction` returns flattened bids, losing raw response and outcome information. Client-side coordination is admissible only after a **real-artifact feasibility proof**: exactly one HTTP request serving both consumers while preserving bid-request IDs, APS admission, callbacks, promises, events, timeout semantics, targeting, and global bid state. If no supported seam exists, authenticated client coordination is also off the table — and Lever C may be infeasible in every architecture, which is an acceptable discovery outcome.
- **Server-side pure-plan seam.** Server request normalization currently inserts a fresh correlation UUID, and the provider contract exposes only a side-effecting `request_bids` — a fingerprint containing correlation randomness never matches, and invoking a provider to learn its exact bytes already contacts the upstream. **Discovery exit criterion:** a side-effect-free `prepare + fingerprint` capability for **every enabled provider and mediator**, or server-side sharing is off the table.
- **Cancellation honesty.** `sendAuction` has no `AbortSignal` and uses `keepalive`; client detach discards the local result but does not prove the server or upstream provider stopped. The design distinguishes **detach/result-discard** from **proven upstream cancellation**, adds generation guards against late targeting/render, and states whether a fresh auction may overlap an invalidated one.
- **Server-seam event semantics** (if a server join is ever built): share only provider/orchestrator execution after each waiter's inputs are normalized; serialize request-specific responses separately; emit one leader auction event plus a joined-waiter event; never replay the leader's correlation data.
- **No completed-response reuse.** The `/auction` response carries no bid lifetime and the adapter stamps a fresh `ttl: 300` at interpretation; reuse would silently renew lifetimes. If ever supported, the response must carry completion time and per-bid expiry, with TTL set to remaining lifetime.
- **Consent parity.** The `/auction` body carries no consent envelope; the server reconstructs consent from cookies and `Sec-GPC`. Discovery must define cookie parity or a validated consent envelope, and `/auction` needs a response signal distinguishing consent-denied from legitimate no-bid; consent-denied results are never shareable.
- **Input ownership tables** per path (TSJS registry generations, Prebid ad-unit generations, navigation generation, render targets); GPT slot targeting is an auction input only if discovery proves it affects request bytes.
- **Billing integrity** per the section above; split fetch from render before any reuse on the `requestAds` path.

## Config-blob compatibility

Integration settings are retained as raw JSON in the pushed blob, so an explicitly configured `0`/`false` is present in the blob; only omitted keys are absent. `PrebidIntegrationConfig` and `GptConfig` do not `deny_unknown_fields`, so older binaries tolerate blobs carrying the new keys. Compatibility tests carry the **non-default values** (`request_bids_coalesce_ms = 50`, `gam_preconnect = true`) through a full blob into the legacy struct shapes, plus present-leaf and absent-leaf environment-overlay tests for **both** fields. The public configuration guide's "unknown TOML keys fail" statement gains an explicit exception note for forward-compatible integration leaves.

**Adapter activation/rollback matrix.** Injected client config is read once per document, so within an adapter a config change affects new documents only — and "new navigation" is not "new document": client-path HTML can be browser-cached for 60 s. Across adapters: Fastly instances are effectively per-request (config push suffices). Axum builds shared state at startup (restart required). Cloudflare holds startup state (redeploy/restart). **Spin parses an embedded example config at build time — a restart cannot activate a changed flag; it requires a source-config change plus rebuild/redeploy (or future runtime config loading).** The pilot rollout is scoped to Fastly.

## Testing

**Vitest (shim), Lever A:**

- two mergeable calls → one underlying `requestBids`, segment units in arrival order, per-caller partitioned maps, handlers in arrival order with preserved `this` and exact `(bids, timedOut, auctionId)`; cancelled-auction `undefined` values pass through unpartitioned;
- bookkeeping-before-callbacks with per-caller registration IDs; a throwing first handler rolls back only its own registration, later handlers run, no facade promise rejects, and the first captured error is **synchronously** thrown after all handlers (asserted to land in Prebid's catch path, matching the existing lone-call regression);
- every ineligible-arrival class flushes the pending batch first, then dispatches solo — ordering asserted;
- order-preserving eviction: merged-[A], solo-B, merged-[C]; **segment-failure isolation:** segment A's underlying dispatch throws synchronously, segments B and C still dispatch in order and settle;
- shared-auction scenario: unscoped `setTargetingForGPTAsync()` plus bare and mixed-slot `refresh()` from the first callback, asserting documented cross-caller delivery, pending-state consumption, synthetic-auction classification, and net `/auction` count;
- reentrancy: a constituent callback enqueues a new batch while the first settles; the detached-batch rule proven — second batch dispatches and settles un-clobbered;
- deadline math: sub-threshold, zero/negative/`NaN`/`Infinity`, missing/invalid captured `bidderTimeout`, mid-hold config change, earlier-deadline re-arm, overshoot floor (never `0`);
- snapshot semantics: post-enqueue request-key additions and array push/splice do not alter the issued call; unit-object mutations caught at revalidation;
- **side-effect-free estimator:** a stateful `toJSON`/getter is never invoked at admission (regression test); accessor-bearing units dispatch solo;
- **adapter split:** `buildRequests` reads boundaries via the internal auction ID from the auction-scoped `bidderRequest` and splits an over-limit merged body along call boundaries (boundary−1/boundary/boundary+1, multibyte content, large EIDs), each descriptor carrying correct `coalesced` group/part metadata; an **oversized single constituent** (large common EIDs) dispatches unsplit as today; **no boundary marker appears in bidder params or the wire payload**; a **stateful EID getter/`toJSON`** is invoked at most once (sanitized copy) or the body is treated as unsplittable;
- coalescing-config lifetime: coalescing self-disables past `issuedAt + lifetime`;
- window `0` / absent config leaves the existing suite untouched.

**Real-artifact coverage (external bundle):** two real calls → one fetch, two thenables, callback-before-promise ordering, partitioned results, a single event sequence, shared timeout/auction-id semantics; a merged body exceeding the limit where `buildRequests` returns multiple descriptors within one auction; a **throwing constituent callback** proving no global error surfaces and all facades resolve; and a merged dispatch where some constituents have **no handler**, proving bookkeeping still runs for them.

**Rust:**

- config defaults and bounds; injected-config serialization omits `requestBidsCoalesceMs` at `0`; injected coalescing config carries `issuedAt` + lifetime;
- `/auction` endpoint parses and validates the `coalesced` object (absent; valid group/size/part; malformed values dropped as absent, never clamped); `AuctionObservationContext` and the event summary row carry the fields; sink serialization includes them; the Tinybird columns, the `auction_coalescing_daily` rollup, and the schema-before-binary ordering are covered;
- `gam_preconnect = true` emits the link without `crossorigin` **before** the GPT bootstrap inserts; `false` emits nothing;
- new-schema → legacy-schema blob test with non-default values; present/absent env-leaf tests for both fields.

**Documentation checklist:** `trusted-server.example.toml`, configuration tables (including the unknown-keys exception note), Prebid and GPT integration guides, environment-overlay leaf behavior for both fields.

**Lever C tests are defined with its design after discovery.**

## Measurement methodology

Acceptance uses a reproducible harness (local Viceroy against the pilot origin, pre-seeded consent, identical scripted scroll) with **randomized, balanced AB/BA pair ordering**, explicit warm/cold connection conditions, and sample sizes derived from pilot variance with **quantile-specific power calculations** (a pilot batch of ≥10 pairs estimates variance; acceptance batches are powered at 80% for the stated effect at the stated quantile, never fewer than 20 pairs). Decisions use **named paired estimators**: median differences via the Hodges–Lehmann estimator with bootstrap 95% confidence intervals; non-inferiority stated per metric and quantile.

**Denominators:** a _run_ is one full harness execution (fresh context); a _navigation_ is one document load; a **candidate burst** is ≥2 `requestBids` calls within the configured window on one navigation (pre-admission); an **eligible burst** is a candidate burst whose calls pass every admission predicate.

Numeric gates:

- **Lever A retains if:** candidate-burst coverage is reported (share of candidate bursts that are eligible — this is the publisher-pattern fact); **merge success among eligible bursts is ≥95%** (implementation health — eligible bursts should merge absent runtime failure, and each failure is diagnosed); total `/auction` requests per navigation drop ≥20% (paired 95% CI excluding zero); and first-non-empty-render non-inferiority holds at p50 and p95 within a 5% margin. Otherwise the flag returns to `0`.
- **Lever B retains if:** the one-sided 95% **lower confidence bound** on cold-start speculative-socket reuse for the first GAM ad request exceeds 70% — the denominator is runs where a first GAM ad request occurred; a run with no speculative socket counts as a reuse failure; parser-indeterminate runs are excluded but invalidate the batch if they exceed 10% — the request-write invariant holds in 100% of runs (any violation = immediate rollback); and the benefit gate passes: the one-sided 95% **lower bound** of the paired setup-time improvement (Hodges–Lehmann) **exceeds the 30 ms minimum worthwhile effect** (a point estimate of 30 ms with a confidence interval reaching near zero does not pass). Median (location) effects use Hodges–Lehmann; **p95 non-inferiority uses a paired bootstrap quantile-difference estimator** with a one-sided upper bound required to stay inside the 5% margin, for first-render (p50, p95) and `load`-event (p50). Socket reuse alone does not retain the flag.
- **B is measured with identical Lever A state in both arms** — the comparison is X vs. X+B, stated in the results.
- **Causality boundary:** the pre-enable paired harness runs are the causal experiment; the production hold checks are **non-causal guardrails** (no contemporaneous control arm) and can trigger rollback but never retention on their own.
- **Hold decision rule:** synthetic checks run **daily** (batches of at least 10 pairs); the retain/rollback decision is a **fixed rule evaluated at day 7** over the accumulated batches (no sequential peeking), with conformance failures acting immediately at any point.

**Live guardrails and their sources:**

| Signal                                    | Source                                                                                                                                        | Type / cadence                     |
| ----------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------- |
| Merged-auction count / drain state        | `coalesced` group counting via the `auction_coalescing_daily` rollup                                                                          | new; continuous                    |
| `/auction` volume per property            | existing server telemetry                                                                                                                     | existing; continuous               |
| Fill/revenue, bid rate, timeout rate      | ad-server / PBS reporting vs. 7-day pre-enable baseline; alert on >5% adverse move; owner: property operator; reporting maturation delay 48 h | existing; daily                    |
| First-render / per-slot latency           | scheduled synthetic harness runs against production                                                                                           | synthetic; daily during holds      |
| Lever B socket reuse + setup time         | scheduled engine-appropriate capture runs                                                                                                     | synthetic; daily during the B hold |
| Consent-denied network activity (Lever B) | scheduled capture runs (GPC / unresolved / denied)                                                                                            | synthetic; daily during the B hold |

**Drain (Lever A):** the shim tracks document age with a **latched monotonic clock** (`performance.now()`-based age, immune to wall-clock skew) and self-disables coalescing once the age exceeds the injected 24 h lifetime. Drain completion is an **additive** bound: config propagation (Fastly: 5 minutes) + 60 s HTML freshness + 24 h document lifetime after the flag returns to `0`. Because auction telemetry is best-effort and can drop rows, zero merged-auction rows alone cannot prove drain: the check requires the `auction_coalescing_daily` rollup to show **zero logical merges while overall `/auction` row volume for the property confirms pipeline liveness** (an ingestion-freshness check). Lever B rollback completion is config propagation + 60 s HTML freshness.

## Rollout

One lever at a time, Fastly pilot only, each independently config-reversible for newly generated documents:

1. Land binary (including the telemetry field and Tinybird schema, deployed schema-first); all flags default off.
2. Lever A Phase 0 trace on the pilot property (including callback-discipline capture). If predicates and net-benefit hold: enable `request_bids_coalesce_ms = 50` alone; hold ≥7 days against the guardrail table; on failure return to `0` and confirm drain per the definition above.
3. With A retained-and-stable or fully drained: enable `gam_preconnect` alone under its governance artifact; **hold ≥7 days** with the daily synthetic reuse/setup and consent-network checks; retain only if the live checks continue to meet the acceptance gates, roll back on any conformance failure immediately.
4. Lever C follows its own spec revision after discovery.
