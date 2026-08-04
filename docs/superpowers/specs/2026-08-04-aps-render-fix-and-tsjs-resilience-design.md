# APS Render Fix and TSJS Resilience Architecture — Design

- **Status:** revision 5 — rewritten for the coordinated hard-cutover policy
  adopted in the fourth review round, and made fully self-contained (no
  contract is defined by reference to an earlier revision).
- **Date:** 2026-08-04
- **Baseline:** `rc/july` @ `541298695` — the full merged state.
- **Inputs:** three code audits against this baseline; design reviews of
  revisions 1–4; open issues #926, #941, #944, #962, #964, #977, #983, #989,
  #993; open PR #997.

## 0. Release policy: coordinated hard cutover

This design targets a **single coordinated release**. Explicitly:

- Server, TSJS bundles, config format, and page HTML ship together as one
  release with one **release id** (`release_id`: the git tag / build hash).
- **No N/N−1 support.** Old pages, old bundles, old config blobs, old
  globals, and old URLs may stop working at cutover. In-flight clients (pages
  loaded before the switch) may fail; this is accepted and stated, not
  mitigated.
- **Exact release matching only.** The kernel, every service, every plugin,
  and the install manifest carry the same `release_id`; any mismatch is a
  refusal, never a negotiation. There are no version ranges.
- **Config format version:** the config blob gains a top-level
  `format_version`; the binary rejects any other value. No
  omit-default-for-rollback serialization rules; rollback means redeploying
  the previous release with its own config.
- **Assets:** binaries embed only their own release's artifacts. Hashed
  pathnames are kept **for cache identity only** (correct caching and
  dedup), not for retention: an unknown hash answers `410 Gone`,
  `Cache-Control: no-store`. No shared artifact store — no separate
  requirement for one was established.
- **Cutover runbook (normative):** (1) deploy the release to all instances
  dark (flagged off); (2) verify instance health and config
  `format_version`; (3) switch traffic atomically at the routing/CDN layer;
  (4) purge the CDN of prior HTML and assets; (5) monitor the Phase gates
  (§8); rollback = switch traffic back to the previous release and re-purge.

Everything that revisions 3–4 built for mixed-version tolerance is removed:
no ABI version ranges, no retained-artifact storage, no legacy query-hash
sunset, no dual-name global window, no adoption gates.

## 1. Problem statement

APS demand is fully integrated server-side — the edge runs the APS OpenRTB
auction, wins bids, and ships a typed renderer descriptor to the page — yet
APS creatives do not appear for real users. Serial single-cause fixes (the
`bid.meta` carrier, the decoupled prebid shim, the `hb_adid` fallback) each
survived review and still did not produce ads. That pattern is the finding:
the APS pipeline has **multiple independent failure points, most of which
fail silently**, and the client cannot tell the server which one fired.

The TSJS library (56 files, ~11,900 lines, two ~1,700-line monoliths,
duplicated ES5/TS logic, inverted layering, ~100 error-swallowing `catch`
blocks) is the same problem structurally. This design fixes APS delivery and
rebuilds TSJS so the next integration cannot reproduce this failure class.

### Non-goals

- No change to the APS OpenRTB endpoint contract or Amazon-side configuration
  (including its deliberate absence of `nurl`/`burl`, §G4d).
- No rewrite of the decoupled Prebid.js strategy.
- **No backward compatibility** (§0). Publisher-visible surfaces change at
  cutover; the replacement shapes are in §7.4.

## 2. Why APS does not render — evidence

Flows: (a) SSAT via `window.tsjs.bids`; (b) GAM + client `trustedServer`
Prebid adapter; (c) SPA `/_ts/page-bids`; (d) direct `/auction`
`tsjs.requestAds`. Only (d) — unused in production — renders an APS
descriptor without GAM.

### 2.1 Admission

| #   | Failure                                                                                                                                                              | Where                                            |
| --- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------ |
| A1  | A configured `[auction].mediator` discards every direct-provider bid; winners come only from the mediator response. APS reports `success, bid_count: N`, never wins. | `orchestrator.rs:412-431`                        |
| A2  | `allow_script_creatives` defaults `false`, dropping every `tagtype: "script"` APS bid; the drop is counted but invisible (A4).                                       | `aps.rs:141-143`, `:773-778`                     |
| A3  | Strict gates: exact `w`×`h` membership; required `ext.creativeurl`; any top-level `contextual` key rejects the whole response.                                       | `aps.rs:657-668`, `:745-778`, `:838-846`         |
| A4  | Drop reasons reach only `/auction` `ext.orchestrator`; SSAT/page-bids discard them; logs and the `ts-debug` allowlist exclude them.                                  | `publisher.rs:1866-1875`, `telemetry.rs:808-826` |

### 2.2 Identity

| #   | Failure                                                                                                                | Where                                         |
| --- | ---------------------------------------------------------------------------------------------------------------------- | --------------------------------------------- |
| B1  | GAM caps key-value values at 40 chars; the raw APS bid id as `hb_adid` can fail the bridge equality check with no log. | `publisher.rs:3366-3372`, `gpt/index.ts:1613` |
| B2  | Two id universes: SSAT keys on the APS bid id, the client adapter on Prebid's generated `adId`.                        | `publisher.rs:3366`, `prebid/index.ts:982`    |

### 2.3 Render

| #   | Failure                                                                                                                    | Where                                                     |
| --- | -------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------- |
| C1  | If GAM never serves the PUC, nothing renders and nothing is recorded; `renderApsCreative` is reachable only from flow (d). | `gpt/index.ts:854-1107`, `core/request.ts:59`             |
| C2  | A renderer endpoint that never answers is a silent 10 s death (opaque iframe cannot read HTTP status).                     | `aps.rs:1188-1245`, `aps/render.ts:384-404`               |
| C3  | SafeFrame breaks slot attribution (top-document iframe walk cannot see nested creative windows).                           | `gpt/index.ts:157-183`, `:1599-1600`                      |
| C4  | Three hand-maintained schema copies with exact-key rejection: a server field addition blanks every APS ad.                 | `types.rs:188-211`, `aps/render.ts:60-93`, `aps.rs:65-73` |
| C5  | A dead duplicate renderer branch holds the debug log and dedup logic; it can never execute.                                | `gpt/index.ts:1643-1674`                                  |
| C6  | The renderer CSP can kill creatives after "ready" (no `object-src`, workers, `blob:`/`data:` frames).                      | `aps.rs:49`                                               |
| C7  | Renderer branches record nothing: no trace record, no notifications.                                                       | `gpt/index.ts:1558-1632`                                  |

### 2.4 Observability

Zero client→server reporting. Server telemetry marks `is_win=1` at auction
time; a bid that never painted is byte-identical to one that painted.

### 2.5 Failure → signal mapping (normative)

Every section-2 failure maps to a distinct observable **failure class** (not
a claim to distinguish unknowable root causes). The operator query for each
row is the §5.6 canonical join filtered by that row's event/reason or
counter; the console column is what diagnostic mode mirrors on-page:

| Failure | Client event/reason (§5.1)                   | Server counter/row (§5.6)              | Console      |
| ------- | -------------------------------------------- | -------------------------------------- | ------------ |
| A1      | —                                            | selection report `mediator_superseded` | startup warn |
| A2      | —                                            | `bid_drop{script_rendering_disabled}`  | startup warn |
| A3      | —                                            | `bid_drop{invalid_dimensions, w, h}`   | warn         |
| A4      | — (fixed by §5.6 itself)                     | `bid_drop` rows exist on all paths     | `ts-debug`   |
| B1/B2   | `bridge_request{matched: false}`             | join via `trace_id`                    | warn         |
| C1      | `gam_empty` then no `bridge_request`         | join via `trace_id`                    | warn         |
| C2      | `render_fail{renderer_document_no_load}`     | renderer route counters                | warn         |
| C3      | `render_fail{bridge_id_mismatch}`            | join via `trace_id`                    | warn         |
| C4      | `render_fail{descriptor_invalid}`            | schema corpus CI                       | warn         |
| C5      | — (branch deleted)                           | —                                      | —            |
| C6      | `runner_failed` + CSP report buckets         | CSP aggregate counters                 | warn         |
| C7      | renderer branch emits the full §5.1 sequence | join via `trace_id`                    | debug/warn   |

## 3. The GPT reality this design must respect

1. Bootstrap-first hybrid: server-injected ES5 `gpt_bootstrap.js` wins the
   sentinel race; the bundle's handoff/initial-load code is dead in
   production.
2. The #922 merge loss: orphan recovery and `updateRender` are gone
   (`0dc9b19a9`); `__tsRenderGeneration`/`__tsRenderBid` are dead writes;
   bridge impressions double-count. PR #997 is the apparent replacement.
3. TS refreshes never pass `changeCorrelator: false`.
4. `enableSingleRequest()` is called blind after publisher `enableServices()`.
5. Responsive resolution ambiguity silently skips slots.
6. Three independent `pubads().refresh` wrappers coordinate via window-global
   booleans.
7. **GPT has no request cancellation, no documented per-refresh identity, no
   overlapping-completion order.** `slotRenderEnded` means creative code was
   injected, not that resources loaded. The April 2025 `responseIdentifier`
   identifies the ad **response** — usable for response dedup/drain, not for
   attributing which caller initiated a request.
8. **With initial load disabled, `display()` creates no request** — the
   subsequent `refresh()` does (`gpt/index.ts:1059`, `ad_init.test.ts:1201`).
   Any cycle protocol must model physical requests, not API calls.
9. The bundle's `slotRenderEnded` registration is gated behind
   `!ts.servicesEnabled` (`gpt/index.ts:1017`); G4a needs unconditional early
   subscription.

## 4. Design gates

### G1 — Trace identity and correlation

The client-visible auction id is EC-derived (`publisher.rs:3237`) and is
never ingested. Initial-HTML auction telemetry is emitted before page JS
exists (`telemetry.rs:148`, `publisher.rs:2452`), so correlation is minted by
whoever acts first:

- **Initial navigation (`nav_gen 0`):** the server mints `trace_id` (128-bit
  CSPRNG, `^[0-9a-f]{32}$`), writes it into that response's auction rows
  (§5.6 schema), and injects it into `tsjs.boot` with a **signed trace
  authorization** (§5.3). It is never `AuctionRequest.id`.
- **Cache privacy invariant:** traces/authorizations are injected only into
  responses that ran a per-request auction; such HTML is
  `Cache-Control: private, no-store` with no validators. Enforced by
  construction and by test.
- **SPA navigations:** `/_ts/page-bids` stays GET; the client mints
  `trace_id` and sends it in a validated `X-TSJS-Trace-Id` header (the
  request already carries a non-simple TSJS header). The server records it in
  that auction's rows; the JSON response echoes the accepted trace and
  returns its signed authorization.
- **Envelope:** every event carries `{nav_gen, refresh_gen, seq}` inside a
  per-trace group `{trace_id, auth, events[]}` (§5.1). `seq` is per-trace
  monotonic. Transport is best-effort: gaps (loss) and duplicates
  (fetch/pagehide races) are both expected; §5.5 defines dedup.
- **Sampling is server-decided for every trace** — initial via boot, SPA via
  the page-bids response — and carried **inside the signed authorization**
  (§5.3 `mode`). The client never asserts its own sampling; an unsigned or
  missing authorization means the trace group is rejected at ingest.

### G2 — Render identity

- Cache-backed bids: `hb_adid` = the PBS Cache UUID, byte-for-byte as today
  (`publisher.rs:3355`; the PUC fetches `?uuid=<hb_adid>`,
  `publisher.rs:3450`, `gpt/index.ts:1700`). Markup bids without cache ids:
  today's fallback chain unchanged.
- **Renderer-only bids: `hb_adid` = a server-minted render token**,
  `^[a-z0-9]{12}$` exactly, CSPRNG, collision-retried within the minting
  auction. Cross-auction uniqueness is probabilistic (36¹² ≈ 4.7×10¹⁸;
  birthday-bound negligible at realistic volumes) and made harmless by
  registry scoping.
- **Registry scope and bounds:** the client bridge registry keys tokens by
  `(trace_id, nav_gen, refresh_gen)` — refresh auctions within one
  navigation cannot collide, closing the revision-4 gap. Capacity: 64 live
  entries per navigation; at capacity a new registration is refused with
  disposition `registry_full`; unexpired entries are evicted only by
  navigation disposal. Token TTL 15 minutes; one-time consumption.
- The client-Prebid path keeps Prebid's generated `adId`; both paths register
  into the one registry keyed by whichever id that path observes.
- Regression tests: non-APS cache-backed bids byte-identical.

### G3 — Runtime ABI under the IIFE build (exact-release model)

IIFE-per-bundle with inlined imports (`build-all.mjs:46`, `bundle.rs:23`)
means imports never share state across bundles (live defect:
`core/context.ts:11` vs `permutive/index.ts:102`).

- The kernel ships only in `tsjs-core`, publishes
  `tsjs._internal = { release_id, registry }` once (window sentinel), and
  **freezes** `_internal` after boot. The kernel constructs and registers
  core services (event bus, beacon queue, sessions, slot registry, render
  state machine) during boot; integrations register integration-scoped
  services during `install()`.
- **Exact release matching:** every service and plugin registration carries
  `release_id`; `registry.get(name)` succeeds only when the registrant's
  `release_id` equals the kernel's. A mismatch quarantines the registration
  and emits `abi_mismatch` (service) / `bundle_partial` (plugin) with a
  console error. No ranges, no minors, no first-wins tiers — under §0 a
  mismatch is a deployment error to surface, not tolerate.
- Stateful access only through the registry at call time; stateless helpers
  may be imported and inlined. Single-module-graph builds remain the
  recorded successor option behind the same surface.

### G4 — Render lifecycle

**G4a — Physical request-cycle protocol.** Two separated notions:

- **Intent:** a TS `display()`/`refresh()` call (or an observed publisher
  entry) targeting a slot. Intents are classified `ts | publisher` at the
  wrapped entry points. An intent may produce zero physical requests
  (initial-load-disabled `display()`; `refresh()` on a never-displayed
  adopted slot); an intent that produces no `slotRequested` within its bound
  (2 s) expires with disposition `intent_no_request` — diagnostics only.
- **Cycle (outstanding physical request):** opened **only by
  `slotRequested`**, matched to the oldest unexpired TS intent for that slot,
  else classified publisher-initiated. SRA batching yields one `slotRequested`
  per slot per batch — one cycle each, all matched to the intents of the
  batch call. A cycle closes on its `slotRenderEnded` (matched by slot; GPT's
  `responseIdentifier`, where present, deduplicates responses during drain —
  it never attributes initiation).
- **Serialization:** TS keeps at most one outstanding TS cycle per slot. A TS
  intent arriving while a TS cycle is outstanding **queues** (bounded: 1
  queued replacement; further intents coalesce into it).
- **Attribution:** a `slotRenderEnded` is attributable iff exactly one
  TS cycle is outstanding for the slot and no publisher-initiated or
  untracked request overlaps it. Any overlap → the slot enters
  **quarantine**: `cycle_unattributable`, fail closed (no fallback, no state
  transition), and the drain rule applies.
- **Drain/re-arm (supersession and SPA):** physical cycle state lives in the
  **RuntimeSession-owned slot record**, not the NavigationSession — adopted
  slots outlive navigations. On navigation or supersession, outstanding
  cycles are marked stale; their late events are **matched and discarded**
  with disposition `stale_navigation` (never misattributed); a quarantined or
  stale slot re-arms only after every outstanding request/render pair has
  drained (or its 60 s drain bound elapses, which keeps the slot
  fallback-ineligible for that navigation). Queued TS intents dispatch only
  after re-arm. A timeout never makes an old event disappear — drain-by-match
  does.
- CI exercises the protocol on the deterministic harness; a **release-gating
  real-GAM overlap test** (publisher refresh racing a TS cycle;
  initial-load-disabled cycle formation) validates it against actual GPT.

**G4b — Acknowledgement protocol.** The renderer document currently posts
"ready" only to its immediate parent (`aps.rs:105`); in the PUC path the
top-level kernel cannot observe it, and callbacks fire on send
(`gpt/index.ts:1572`, `:1620`). Contract: the bridge response embeds a
**per-attempt 128-bit CSPRNG acknowledgement nonce**; the renderer document
posts versioned `{t: "render_accepted" | "render_failed", nonce, reason?}`
to the top window; the kernel validates, in order: source ownership (§6.8
walk), nonce equality, token binding, `nav_gen`, `refresh_gen` — all five —
before any state transition or notification. Pinned by tests for SSAT,
client-Prebid, and nested SafeFrame flows, including stale and replayed
acks.

**G4c — Honest observations.** Inline-adm frames are sandboxed `srcdoc`
without `allow-same-origin` (`gpt/index.ts:358`) — opaque origins; geometry
proves nothing. Observations: `gam_nonempty`, `gam_empty`,
`renderer_document_loaded`, `runner_loaded`, `runner_failed`,
`adm_document_loaded`. Every path terminates at `render_accepted`
(authenticated per G4b where the renderer protocol exists;
`adm_document_loaded` stands in for adm frames). **No observation claims
paint**; there is no `render_confirmed`. A future trusted completion ack
(OQ6) may add a new state under a new name.

**G4d — Win/billing notifications.** APS intentionally carries neither
`nurl` nor `burl` (`aps.rs:812`; the minimized AAX envelope excludes them;
the integration guide documents no generic APS beacons). APS billing lives in
the Amazon runner lifecycle; unchanged.

For carrying paths (PBS and other OpenRTB providers), **bind is defined per
flow and is never selection or targeting** (targeting-only firing is
explicitly prevented today, `ad_init.test.ts:1824`):

- PUC/GAM flow: bind = an owned, slot-and-ad-id-matched bridge claim.
- Direct `/auction` flow: bind = validated render start (slot resolved,
  descriptor/markup validated, attempt created).
- Fallback flow: bind = attributed `gam_empty`, immediately before the
  fallback render starts.

`nurl` fires at bind; `burl` at `render_accepted`; both attempt-scoped
(idempotency key `(trace_id, nav_gen, slot, refresh_gen, hb_adid)`), fired at
most once, via `sendBeacon`/`no-cors fetch`, no retries. Terminal failure
after acceptance → `billed_then_failed` label; no un-firing.

**G4e — Fallback trigger.** Opt-in
(`[auction].client_render_fallback = "renderer"`). Renders only after a
terminal `gam_empty` **unambiguously attributed to a TS cycle** (G4a) —
ownership does not gate it (adopted slots are the common path and are
eligible); publisher-initiated or unattributable cycles never trigger it;
timeouts never render. The direct renderer is converted to an awaitable API
with cancellation and terminal reasons before the fallback lands.

**G4f — Direct `/auction` lifecycle.** The non-GPT path
(`core/request.ts:52`) gets the same discipline: a `RenderAttempt` keyed
`(trace_id, nav_gen, refresh_gen, slot)` where `refresh_gen` increments per
`requestAds` invocation for the same slot within a navigation; exactly-once
terminal state; G4b acknowledgement validation; G4d direct-flow bind;
cancellation and disposal on navigation; the same §5.1 event sequence. "Every
configured flow" in the success criteria includes this one.

### G5 — Deployment contracts (hard cutover)

- **Config:** top-level `format_version`; exact match required; mismatch is a
  startup error. No default-omission rollback rules.
- **Assets:** hash-in-pathname (`/static/tsjs/<hash>/<name>.js`) for cache
  identity; binaries serve only embedded current-release artifacts; unknown
  hash → `410 Gone`, `no-store`; `Cache-Control: immutable` on exact matches.
  Concatenations precomputed per **ordered module-ID vector** at build time.
  The cutover runbook (§0) owns HTML/asset consistency; the CDN purge step is
  what retires old references.
- **Internal route isolation (all adapters):** the renderer, client-events,
  and CSP-report route families (a) dispatch **before** auth, EC setup, and
  publisher/integration filters (today the renderer can traverse EC setup and
  pre-route filters, `app.rs:709` in the Fastly adapter); (b) reserve **all
  methods and all version prefixes** locally — unsupported method →
  deterministic `405` with `Allow` and `no-store`; unknown version → `404`
  `no-store`; never the publisher fall-through some adapters use today
  (`adapter-spin app.rs:804`); (c) never forward bodies, cookies, or
  authorization headers to publisher origins; (d) compare origins as
  normalized scheme + host + port, not host-only.
- **Ingest routing:** client-events in all four adapters; Fastly has the real
  sink; others accept-count-drop by contract (OQ5).
- **Storage:** §5.6 schemas deploy and validate **before** any writer
  enables.

## 5. Observability

### 5.1 Wire payload

```
{ v: 1, traces: [
  { trace_id, auth,                      // auth: signed authorization (§5.3)
    events: [
      { nav_gen, refresh_gen, seq,
        t: "bid_received" | "targeting_set" | "bridge_request" |
           "bridge_response_sent" | "render_attempt" | "render_accepted" |
           "render_fail" | "gam_nonempty" | "gam_empty" |
           "renderer_document_loaded" | "runner_loaded" | "runner_failed" |
           "adm_document_loaded" | "fallback_start",
        slot,        // configured slot id if in the injected set, else "s<ordinal>"
        id_kind,     // "cache_uuid" | "render_token" | "prebid_adid" | "bid_id" | "none"
        matched,     // bridge_request only
        source,      // "renderer" | "adm" | "pbs-cache" | "gam"
        reason }     // render_fail only
    ] }
] }
```

Every G4c observation is a **wire event** in this enum (internal state
transitions map to them one-to-one; nothing observable is state-only).
`gam_empty` additionally appears as a `render_fail` reason when it is the
terminal outcome of an attributed attempt.

The `t` enum now **contains every G4c observation**, so Phase-3 stage rates
(renderer-document load rate, runner load/failure, GAM fill) are queryable.
Reason enum (closed): `renderer_document_no_load`, `runner_no_load`,
`runner_failed`, `descriptor_invalid`, `invalid_dimensions`,
`dimensions_out_of_range`, `bridge_id_mismatch`, `cycle_unattributable`,
`intent_no_request`, `stale_navigation`, `bridge_claim_timeout`, `gam_empty`,
`no_render_source`, `slot_unresolved`, `gpt_absent`, `pbjs_absent`,
`bundle_partial`, `fallback_cancelled`, `abi_mismatch`, `registry_full`.

### 5.2 Transport

`fetch(..., {keepalive: true, credentials: "omit"})` primary; `pagehide`
fallback `navigator.sendBeacon(url, new Blob([json], {type:
"application/json"}))`. Flush every 5 s and on `visibilitychange`/`pagehide`.
**Client queue bound:** 256 events; overflow drops oldest, increments a
counter, and the final flushed batch carries one `render_fail{...}`-class
overflow marker event so truncation is visible. Duplicates from
fetch/pagehide races are expected and handled at the sink (§5.5).

### 5.3 Signed trace authorization

Format `v1.<kid>.<exp>.<mode>.<sig>`:

- `kid`: key id; **active and previous keys** live in the platform secret
  store; keys are ≥ 256-bit CSPRNG values; rotation = introduce new key as
  active, demote, retire. **Missing-key startup behavior:** if the beacon is
  enabled and no signing key resolves at startup, startup fails loudly
  (config error) — traces are never issued unsigned.
- `exp`: unix epoch seconds; verifier allows ±60 s skew; maximum future
  15 minutes from issuance.
- `mode`: `sampled` | `diagnostic`. Sampling is server-decided (G1);
  diagnostic is a distinct authenticated mode gated by the tester cookie at
  issuance — not an overloaded "sampling off" bit.
- `sig`: HMAC-SHA-256 over the **domain-separated, length-prefixed** input
  `"ts-trace-auth-v1" || len(origin) || origin || len(trace_id) || trace_id
|| len(mode) || mode || u64(exp)`, where `origin` is the externally visible
  scheme+host+port. Constant-time comparison.
- Ingest verifies per trace group; a missing key id, expired, future-dated,
  or invalid signature → that **group** is dropped-and-counted (other groups
  in the batch survive). An unsigned `sampled` claim does not exist in the
  wire format, so it cannot be asserted.

### 5.4 Ingest contract

- `POST /_ts/client-events`; `Content-Type: application/json` only; no
  `Content-Encoding`; responds `204`, `no-store`; never echoes input.
- Pre-parse limits: body ≤ 16 KiB; ≤ 64 events; strings ≤ 64 chars;
  `trace_id ^[0-9a-f]{32}$`; integers in `[0, 2³¹)`.
- Same-origin: `Sec-Fetch-Site: same-origin` when present, else normalized
  `Origin` equality; absent both → drop-and-count.
- **Rate limiting (numeric, fail-closed for telemetry):** token bucket per
  client address, **10 requests/min, burst 20**; limiter map ≤ 65,536
  entries, entry TTL 10 min, LRU eviction; limiter unavailable/errored →
  drop early with `204` (count only). Trusted client address per adapter:
  Fastly — platform client IP; Axum — rightmost `X-Forwarded-For` entry
  beyond required `trusted_proxy_hops` (absent config → socket peer only);
  Cloudflare — `CF-Connecting-IP`; Spin — platform client address.

### 5.5 Sink and deduplication

- Stable event key `(publisher, trace_id, seq)`; deduplication at the sink
  or query layer (Tinybird: latest-write or `GROUP BY` on the key), covering
  fetch/pagehide double-delivery.
- The Fastly sink is fire-and-forget after dispatch (`tinybird.rs:153`) and
  cannot observe downstream schema/auth rejection — therefore
  **datasource-side freshness monitoring is mandatory** (§8 gates alarm on
  ingestion lag and row-rejection metrics from the datasource side).

### 5.6 Physical schemas (deployed before writers)

- **New datasource `ts_client_events`** (flattened rows):
  `ts (DateTime64), publisher (LowCardinality String), release_id (String),
trace_id (FixedString 32), mode (Enum sampled|diagnostic), nav_gen UInt32,
refresh_gen UInt32, seq UInt32, event (Enum §5.1), slot (String ≤64),
id_kind (Enum), matched (UInt8), source (Enum), reason (Enum §5.1)`.
  Sorting key `(publisher, ts, trace_id, seq)`; 30-day TTL; its own ingest
  token, configured via new `TinybirdSettings` fields
  (`client_events_dataset`, `client_events_token_secret`) — today's settings
  configure only the auction dataset (`settings.rs:1752`). Sink batch cap:
  512 rows per dispatch (matching the auction sink). Startup validation:
  when client events are enabled, the dataset name and token secret must
  resolve or startup fails. Sink-unavailable behavior at runtime:
  accept-count-drop (ingest still answers `204`). Canonical join:
  `ts_client_events` ⋈ auction rows on `(publisher, trace_id)`; dashboards
  and alerts are owned by the release owner and defined with the datasource.
- **Auction rows** (`AuctionEventRow`, `telemetry.rs:262`, and
  `auction_events_raw.datasource`): add nullable `trace_id (FixedString 32)`
  and `mode`; add a bounded **`bid_drop` row type**
  `{provider, slot, reason (Enum), width UInt16?, height UInt16?, count
UInt32}` with per-auction row cap 32 and an `overflow` bucket row.
- APS parsing returns a **structured drop observation**
  `{reason, slot, width?, height?}` instead of a bare reason string
  (`aps.rs:722`); dimensions above 8192 use `dimensions_out_of_range` with
  dimensions omitted, never clamped.

### 5.7 Modes and SLOs

- **Production (sink-backed only):** server-decided 10% sampling.
  Two separated objectives: **pipeline availability** — ingestion freshness
  ≤ 5 min and datasource rejection rate < 0.1%, alarmed on breach (fails
  during sink outages, by design); **failure detection** — a failure mode
  affecting ≥ 1% of sampled render attempts is visible within one hour,
  evaluated only at ≥ 10,000 sampled render attempts/hour.
- **Diagnostic:** authenticated mode (§5.3), unsampled, full stream +
  console mirroring; one page load names the failing class.

### 5.8 Server-side drop surfacing

Bounded structured summary whenever any bid is dropped; `bid_drop` rows
(§5.6); drop summary in the initial-HTML `ts-debug` comment; page-bids gains
a tester-gated structured `debug` field. Startup warnings: APS +
`allow_script_creatives = false`; mediator + direct providers without an
explicit `winner_selection` (§6.1 makes that a hard error).

## 6. APS delivery fixes

### 6.1 Mediation: complete inline algorithm

Current mediation cannot support merging: it forwards no stable candidate id;
restores fields via a lossy last-write-wins `(provider, slot, bidder)` index
(`adserver_mock.rs:95`); breaks equal-price ties by response arrival order
(`orchestrator.rs:827`); and assigns parsed Prebid bids USD without
validating response currency (`prebid.rs:2318`). The algorithm below replaces
that, identically in the synchronous and split dispatch/collect paths, via
one shared candidate-selection helper.

1. **Candidate registration.** Every direct-provider bid admitted by parsing
   becomes a candidate with a **server-minted candidate id** (`c` +
   11-char CSPRNG, unique per auction). The full candidate (renderer, cache
   coordinates, notification URLs, currency, provenance
   `(provider, upstream_bid_id)`) is stored by candidate id. Winners are
   selected **by candidate id** and their fields read from the stored
   candidate — the lossy index is deleted.
2. **Currency.** The auction has one configured currency. A provider response
   that declares another currency, or a path that cannot prove its currency
   (the Prebid parse point must validate, not assume USD), rejects that bid
   at parse with `bid_drop{currency_mismatch}`. No conversion.
3. **Mediator exchange.** Forwarded candidates carry their candidate id; the
   mediator is required (wire contract, including `adserver_mock`) to echo it
   on any bid derived from a forwarded candidate. A mediator bid **without**
   an echoed id is mediator-native. A mediator bid with an id that does not
   resolve → **the slot fails closed** for merging
   (`mediation_provenance_invalid`: mediator-native bids for that slot still
   compete; unresolvable forwarded claims are discarded and counted — a
   warning alone is insufficient).
4. **Floors.** Slot floors filter both populations before selection.
5. **Dedup.** A mediator bid that echoes candidate id X removes direct
   candidate X from the pool (it is the same demand, provenance `mediator`).
6. **Selection.** Per slot, the winner is the maximum under the **total
   deterministic order**: decoded CPM desc → provenance rank (mediator
   before direct) → provider name asc → candidate id asc. Response arrival
   order can never matter.
7. **Strategy config.** `[auction].winner_selection` is **required whenever a
   mediator and direct providers coexist** — startup error if absent (no
   silent default; §0 removes the compatibility rationale for one):
   `mediator_only` (mediator bids only, direct providers are signal) or
   `merge_highest_cpm` (the algorithm above). A mediator timeout degrades to
   direct-only selection and is reported.
8. **Reporting.** A selection report per auction: `winner_source`,
   `mediator_superseded`, `currency_mismatch`, `dedup_hits`,
   `mediation_provenance_invalid` — separate from delivery `bid_drop` rows.

Deal priority remains out of scope: the `Bid` model carries no deal identity
(`types.rs:231`); a rule the model cannot express would be fiction. Recorded
as follow-up requiring a bid-model extension.

### 6.2 Dimensions

Exact size membership stays (`aps.rs:657-668`). The fix is visibility
(structured `bid_drop{invalid_dimensions, w, h}`, §5.6) plus documentation
("sizing your slots for APS"): if a size is acceptable, request it in the
slot's `formats` — accepting unrequested sizes would conceal an upstream
protocol violation.

### 6.3 Script creatives

`allow_script_creatives` stays default-`false` (defensible sandbox posture);
the consequence becomes loud (§5.8) and the enablement path documented.

### 6.4 Render identity

As G2, including the `(trace_id, nav_gen, refresh_gen)` registry scope and
capacity rules.

### 6.5 Fallback

As G4e/G4a; awaitable renderer first; attribution-gated; timeouts never
render.

### 6.6 Renderer endpoint

- The static renderer document route registers **unconditionally in every
  adapter** (the APS provider stays config-gated); startup validation fails
  if an auth handler pattern covers it; §G5 route-isolation rules apply
  (early dispatch, all methods reserved, no publisher fall-through).
- Path `/integrations/aps/renderer/v1`, embedded in the binary, served
  `Cache-Control: immutable` (its bytes change only by shipping `/v2` in a
  new release; §0's purge retires the old). Unknown versions → `404`
  `no-store`.
- **Two-stage acknowledgement:** authenticated `document_loaded` (proves
  route + auth + document CSP), then the runner-load result — splitting
  `renderer_document_no_load` from `runner_no_load`/`runner_failed`.
- Server route counters (requests, unknown-version, auth-blocked) are
  **aggregate only** — the document request carries no trace (the nonce
  rides the URL fragment and never reaches the server).
- **CSP rollout that cannot false-pass.** Three distinct uses, each with its
  own instrument: **discovery** uses the **currently enforced** policy with
  reporting attached (report-only alone cannot reveal what the enforced
  policy already blocks); **tightening** candidates run report-only;
  **relaxation** candidates are tested in a small enforced cohort under a
  short-lived canary document version, gated on runner acceptance rate, CSP
  violation rate, render-failure rate, and a kill switch that reverts the
  cohort to the frozen policy. Once frozen, a new immutable `/v2` ships with
  the final enforced headers. Reports: dedicated `POST /_ts/csp-reports`
  accepting **both** `application/csp-report` (legacy) and
  `application/reports+json` (Reporting API), each with its own payload
  validator; §5.4 caps and rate-limit rules; **origin rules account for the
  renderer's opaque sandbox** (reports may carry `null` origin — validated
  by document URL / policy version instead of the beacon's same-origin
  rule); unused report fields are **discarded before logging or
  aggregation**; stored as aggregate counters with blocked sources bucketed
  into `https-host (allowlisted) | data | blob | inline | eval | other` (no
  arbitrary host labels — cardinality abuse is otherwise trivial). Browser
  coverage for opaque-renderer reports runs on **Chromium, Firefox, and
  WebKit** (CI is Chromium-only today, `playwright.config.ts:16`; the matrix
  extends for this suite).

### 6.7 One descriptor schema — generation covers all three implementations

- Wire truth: the tagged `BidRenderer` envelope (discriminator on the enum,
  `types.rs:188-211`).
- A wire-schema crate/xtask (separate from `trusted-server-js`; core already
  depends on that crate, `Cargo.toml:45`) generates: the JSON-Schema
  artifact, the **TS structural parser**, the **ES5-compatible inline
  validator fragment** embedded in the renderer document, and shared
  fixtures — all checked in with staleness CI. Only environment-specific
  semantic checks (URL/origin policy, canonical base64, length bounds, the
  exact one-bid AAX projection, cross-field equality) stay handwritten.
- Tolerance only on the outer versioned descriptor; the decoded AAX envelope
  remains an exact projection. A shared positive + adversarial corpus runs
  through the Rust validator, the generated TS parser, and the generated
  inline fragment in CI.

### 6.8 Bridge hardening

Processing order (normative — preserves the existing stolen-capability
defense that suppresses propagation before source validation,
`gpt/index.ts:1547`):

1. parse `e.data` (bare `catch` → return);
2. identify a TS-reserved ad id (registry lookup);
3. if TS-reserved: `stopImmediatePropagation()` **before** any validation —
   a rejected foreign frame must not be answerable by Prebid's native
   handler either;
4. validate source ownership via the bounded walk: known slot-root
   `WindowProxy` map, sender's own parent chain (`event.source.parent`, …)
   to depth 5 — never scanning an attacker-controllable frame tree;
5. validate nonce, token, `nav_gen`, `refresh_gen` (G4b);
6. respond, or refuse with `bridge_id_mismatch`.

Non-TS ad ids are untouched (no propagation suppression). The stolen-token
browser test asserts **neither TS nor the native Prebid listener responds**;
listener-registration ordering has a real-browser assertion. The dead
duplicate renderer branch (C5) is deleted; renderer branches emit the full
§5.1 sequence with G4d notifications only on carrying paths.

## 7. TSJS target architecture

### 7.1 Layering

```
kernel/          boot, config, queue, event bus, log, beacon, sessions
adapters/        googletag.ts, pbjs.ts, messaging.ts   ← the ONLY window.* access
services/        slots (registry+handoff), auction client, render engine, consent
integrations/    gpt, prebid, aps, creative, datadome, …  (plugins over services)
```

Boundary lint in CI (`import/no-restricted-paths`): kernel imports nothing
above it; adapters import kernel only; services import kernel + adapters;
integrations import kernel + services, never each other. This dissolves the
audited inversions (`core/auction.ts` and `core/request.ts` importing
`integrations/aps/render`; `gpt` and `prebid` importing `aps`; `prebid`
owning the GPT refresh wrapper). Stateful services via the G3 registry only.

### 7.2 Adapters

Per external global: `present | pending | timed_out`, `timed_out`
non-terminal (late loaders transition to `present` and drain what is still
valid); queued operations carry their own timeouts and expire with
disposition reasons.

### 7.3 Slot registry service

Kernel-owned; `WeakMap<googletag.Slot, SlotRecord>` + div-id index; holds
ownership (ts/publisher/adopted), handoff claims, responsive resolution,
G4a intent queue + cycle state (RuntimeSession-scoped), targeting-key
history. No expandos on GPT objects (`__tsRenderGeneration`/`__tsRenderBid`
deleted).

### 7.4 Final global surface (hard cutover — no dual names)

| Legacy surface (removed at cutover)          | Final shape                                                                   |
| -------------------------------------------- | ----------------------------------------------------------------------------- |
| `window.tsjs.que`                            | `window.tsjs.que` — unchanged, the one public queue                           |
| `globalThis.tscreative` (API)                | `tsjs.creative.*` (same methods, namespaced)                                  |
| `globalThis.tsCreativeConfig` (pre-load)     | `tsjs.boot.creative` inside the boot container (below)                        |
| `requestAds` (void) — and rev-4's dual API   | **one** async contract: `tsjs.requestAds(options): Promise<RequestAdsResult>` |
| `window.__tsjs_*` flags, integration configs | `tsjs.boot.*` fields written by server-injected scripts before the bundle     |
| server install manifest                      | `tsjs.boot.manifest` (`{release_id, plugins: [{id, order}]}`)                 |
| expandos / function sentinels                | `SlotRecord` fields / kernel `WeakSet`                                        |
| `tsjs._internal`                             | kernel-owned registry (G3), **frozen after boot**                             |

Boot container lifecycle: pre-core scripts write
`window.tsjs = window.tsjs || {que: [], boot: {}}` fields; the kernel
**consumes `boot` at boot, deep-freezes the retained copy, and deletes
consumed one-shot secrets** (the trace authorization moves into the sealed
NavigationSession). Old pages referencing removed names fail at cutover —
accepted per §0.

### 7.5 Messaging module

All `postMessage` through one module: versioned envelopes, name constants
(the `'Prebid Request'` literal exists at six sites today; the APS handshake
in three copies), G4b nonces, §6.8 validation. The minimal module (envelope +
constants + validators used by the bridge) lands in Phase 1; full call-site
migration completes in Phase 4.

### 7.6 Plugin lifecycle — transactional — and sessions

`tsjs.definePlugin(id, install, dispose?)` with `install(ctx)`:

- `ctx.signal` (aborted on quarantine/disposal); synchronous
  `ctx.onDispose(fn)` registration; effects must be registered as they are
  made.
- **Unwind on failure:** a throw, rejection, or abort triggers automatic
  reverse-order invocation of the disposers registered so far — partial
  installs cannot leak effects. Per-disposer exception isolation (one
  throwing disposer cannot stop the rest).
- A disposer registered (or returned) **after** the owning session was
  disposed is invoked immediately.
- Pending late registrations (manifest requested, bundle not yet evaluated):
  capacity 16, bound 10 s, then `bundle_partial`.
- Release matching per G3: a plugin whose `release_id` differs from the
  kernel's is quarantined before `install` runs.
- Sessions: `RuntimeSession` (page lifetime: bridge listener, history hook,
  pbjs subscriptions, adapters, beacon queue, **slot cycle state**);
  `NavigationSession` (per navigation: trace + authorization, render
  attempts, slot aliases, targeting history); `RenderAttempt` (per G4a cycle
  or G4f attempt). Each owns an enumerable disposal inventory; navigation
  disposes only NavigationSession children.
- Error policy: no empty `catch` — handle, log with context, or emit a
  disposition. The auction fetch gains timeout + `AbortController`.
- **Console logging retained:** every issue-surfacing condition keeps or
  gains a `log.warn` carrying the same reason code as its beacon event;
  `debug`-level delivery/security failures are promoted to `warn`.

### 7.7 Bootstrap

`gpt_bootstrap.js` (495 ES5 lines duplicating handoff/initial-load/hydration
logic, with the live `servicesEnabled` divergence) shrinks to a
queue-and-flags stub; the bundle replays recorded early calls on install.
Replay changes observable ordering — it ships inside the cutover with
browser specs covering replay timing. The no-bundle fallback ("ads render if
the bundle fails", pinned by `gpt.rs:1174-1179`) is **generated from the
same TypeScript source** at build time.

### 7.8 GPT correctness carried with the restructure

Unconditional early `slotRequested`/`slotRenderEnded` subscription (replacing
the `!servicesEnabled` gate, `gpt/index.ts:1017`; recording idempotent);
restore #922/#997 attribution and orphan recovery; `changeCorrelator: false`
on TS refreshes (configurable); `enableSingleRequest()` only when GPT
services are not already enabled; ambiguous responsive resolution emits
`render_fail{slot_unresolved}` alongside its warning.

### 7.9 Decomposition targets

| Today                                | Target                                                                                                             |
| ------------------------------------ | ------------------------------------------------------------------------------------------------------------------ |
| `gpt/index.ts` (1777 LOC, 20 jobs)   | `slot_resolution`, `handoff`, `initial_load`, `ad_init`, `render_bridge`, `spa_navigation`, `beacons` + thin index |
| `prebid/index.ts` (1671 LOC)         | adapter, shim, refresh handler (onto the slot registry), eids, diagnostics                                         |
| `gpt/script_guard.ts` (634 LOC)      | folded onto the 170-LOC `shared/script_guard.ts` factory                                                           |
| `core/trace.ts` (model + UI)         | `services/trace` (model) + `integrations/trace_overlay` (UI)                                                       |
| `core/global.d.ts` (`pbjs: TsjsApi`) | real Prebid types; `TsjsApi` split public vs internal                                                              |

### 7.10 Performance (reproducible)

- Bundle budgets: raw/gzip/Brotli per bundle for **three module vectors**
  (minimal, reference, maximal), compressors pinned (`gzip -9`,
  `brotli -q 11`), compared to checked-in baseline artifacts
  (`perf/baselines/*.json`); baseline updates are explicit reviewed diffs.
  Tolerance +5% bytes.
- Browser timing (bids-script-to-first-`display()`): named runner image
  `ubuntu-24.04` (the CI image already used by this repository's workflows)
  and the Chromium build bundled with the pinned `@playwright/test` version
  from `package.json`; 5 warm-up runs discarded, 50 samples, gate p90 ≤
  baseline × 1.10. The baseline artifact records image, browser, and tool
  versions alongside the numbers; a baseline update is invalid if any of
  those differ from the pinned set.
- Server (precomputed concatenation): one-sided gates, CPU and heap ≤
  baseline × 1.10; improvements always pass. Tool versions pinned in
  `.tool-versions`.

### 7.11 Toolchain

Raise the TypeScript floor to the resolved 5.9 line (lockfile already
resolves 5.9.3 under the stale `^5.5.4` manifest); adopt
`noUncheckedIndexedAccess`, `exactOptionalPropertyTypes`,
`verbatimModuleSyntax`; dev-toolchain bumps (eslint, prettier, jsdom,
`@playwright/test`, `@types/node`) as individual CI-gated PRs with changelog
review (this library monkeypatches `fetch`/`sendBeacon`/DOM prototypes —
jsdom/Playwright changes are real risks); `prebid.js` excluded from casual
bumps (runtime Prebid is the manifest-locked external bundle; the npm pin
and deployed bundle version documented together); monthly review; no phase
starts more than one minor behind stable.

## 8. Migration plan

Phases are internal build milestones of **one coordinated release** (§0):
each lands behind a flag in the dark deployment; the cutover switches them
on together. Gates are executable — every gate names query, cohort,
denominator, minimum sample, threshold, window, owner (release owner unless
stated), and action (hold cutover / rollback switch):

- **Phase 0 — Identity, schemas, toolchain.** Path-hashed embedded assets +
  410 semantics + ordered-vector precompute; `format_version`; §5.6 schemas
  deployed and validated (writer-off); toolchain floors; dead expando writes
  deleted; §5.8 server drop surfacing.
  _Gate:_ dark-instance health 100%; datasource validation green (rejection
  rate < 0.1% on synthetic writes, freshness ≤ 5 min); asset `410` rate on
  dark probes = 0 for known hashes.
- **Phase 1 — Kernel, sessions, minimal messaging, cycle registry.** G3
  registry (exact release ids); RuntimeSession/NavigationSession; install
  manifest; minimal messaging module; G4a intent/cycle records; unconditional
  GPT subscriptions.
  _Gate:_ browser-spec suite green incl. listener-order assertions; zero
  `abi_mismatch`/`bundle_partial` on dark probes.
- **Phase 2 — Trace + beacon.** Server-minted initial trace + page-bids
  header echo + signed authorizations; beacon service; four-adapter ingest;
  `ts_client_events` writers on.
  _Gate:_ on dark probes: ingest acceptance ≥ 99%, group auth-rejection
  < 0.5%, dedup query returns exactly-once per `(trace, seq)`; freshness
  ≤ 5 min over a 24 h window.
- **Phase 3 — APS delivery.** Schema crate + corpus (6.7); mediation
  algorithm + required `winner_selection` (6.1); render token (G2);
  renderer route + two-stage ack + CSP report route (6.6); bridge order
  (6.8); G4a–G4f state machines, awaitable renderer, scoped notifications,
  fallback; #922/#997 restoration; correlator + SRA fixes.
  _Gate (canary cohort vs simultaneous control cohort, 24 h, minimum 10,000
  sampled attempts each):_ APS `render_accepted` / attributable APS attempts
  ≥ 95%; renderer-document load rate ≥ 99%; runner failure+timeout ≤ 1%; GAM
  fill, p95 latency, and billing volume deltas within ±2% of control
  (billing measured against GAM/server-side reporting, not the beacon);
  real-GAM overlap test green. Action on breach: hold cutover.
- **Phase 4 — Structure.** Full layering + boundary lint; transactional
  plugin lifecycle; adapters; full slot registry; full messaging migration;
  final namespace (7.4).
  _Gate:_ boundary lint zero exceptions; disposal-inventory leak tests
  green; pre-cutover page smoke on the final namespace; **behavioral-parity
  suite green across all four flows** (SSAT, client-Prebid, page-bids,
  direct `/auction`) comparing pre- and post-restructure event sequences on
  the reference page.
- **Phase 5 — Decomposition + cutover.** File splits; script-guard
  consolidation; bootstrap stub + generated fallback; then the §0 runbook.
  _Gate:_ bundle budgets + timing assertions hold; cutover checklist signed
  off; post-switch monitor window 24 h on the §5.7 objectives; rollback =
  traffic switch back.

## 9. Test acceptance matrix

Hermetic CI (deterministic PUC/message harness) blocks PRs; the staged
real-GAM suite is release-gating. Rows added this revision are marked •.

| Area              | Must cover                                                                                                                                                                                                                                                               |
| ----------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| Request cycles    | intent-vs-request separation; • initial-load-disabled cycle formation (display creates no request); SRA batching; `intent_no_request`; publisher overlap → quarantine; • old-navigation completion before and after replacement; drain/re-arm; real-GAM overlap (gate)   |
| Ack protocol      | five-field validation; SSAT/client-Prebid/SafeFrame; stale + replayed acks; • acks after navigation disposal                                                                                                                                                             |
| Bridge security   | • TS-reserved id rejected with propagation stopped — neither TS nor native Prebid responds; wrong-slot/stolen/prior-navigation tokens; bounded parent-chain walk; listener-order real-browser assertion; SafeFrame positive                                              |
| Render semantics  | notifications only on carrying paths (never APS); bind per flow (PUC claim / direct render-start / fallback pre-render); `burl` at `render_accepted`; attempt-scoped idempotency; `billed_then_failed`; accepted-but-blank                                               |
| Direct `/auction` | • full G4f lifecycle: attempt keys, refresh_gen increments, cancellation on navigation, exactly-once terminal, same event sequence                                                                                                                                       |
| Fallback          | only attributed `gam_empty` (adopted + TS-owned); publisher-initiated never; timeout never renders; SPA cancellation; • flag change during an active attempt                                                                                                             |
| Mediation         | • candidate-id echo; • transformed/unresolvable provenance → slot fails closed for merging; • duplicate candidates dedup; • deterministic ties independent of arrival order; currency validation at the Prebid parse point; both lifecycles; required `winner_selection` |
| Render token      | format/CSPRNG/in-auction retry/TTL/one-time; `(trace, nav_gen, refresh_gen)` scoping; • registry capacity → `registry_full`                                                                                                                                              |
| Trace auth        | • HMAC verification: expiry, skew, max-future, missing kid, rotation (previous key), constant-time path; per-group rejection; cache-privacy invariant (`private, no-store`)                                                                                              |
| Beacon            | initial + SPA trace joins; per-trace grouping; seq gaps; • duplicate fetch/pagehide delivery deduped at sink; queue overflow marker; ingest abuse; sendBeacon Blob type                                                                                                  |
| Ingest/limits     | • token-bucket rate + burst; • limiter saturation and address churn; • map capacity/TTL/eviction; fail-closed drop with 204                                                                                                                                              |
| Internal routes   | • wrong-method → 405 + Allow + no-store on every adapter; • unknown version → 404 no-store; • no publisher fall-through; • dispatch before auth/EC/filters; • no body/cookie/authorization forwarding                                                                    |
| CSP               | • both media types with separate validators; • opaque/null-origin renderer reports accepted; • bucketed aggregation only; • Chromium/Firefox/WebKit capture; • enforced-policy discovery vs canary-cohort relaxation                                                     |
| Schema            | staleness; adversarial corpus through Rust + generated TS + generated inline fragment; outer tolerance vs exact AAX projection                                                                                                                                           |
| Runtime ABI       | one kernel under concatenation; exact-release verdicts (match runs, mismatch quarantines); late registration; failure isolation                                                                                                                                          |
| Plugins           | • partial synchronous install unwound in reverse order; • async rejection; • abort while pending; • disposer-after-disposal invoked immediately; per-disposer isolation                                                                                                  |
| Lifecycle         | `timed_out → present`; session disposal inventories; boot container consume/freeze/delete; final-namespace smoke (`tsjs.que`, `tsjs.creative`, async `requestAds`)                                                                                                       |
| Delivery          | unknown hash 410 no-store; immutable on exact match; ordered-vector precompute; cutover runbook rehearsal (switch + purge + rollback switch)                                                                                                                             |
| Sink              | • datasource-side freshness + rejection monitoring (sink is fire-and-forget); • sink auth/schema rejection surfaced by monitor                                                                                                                                           |
| Failure injection | • Amazon runner redirect, network hang, CSP block, script error → distinct §5.1 outcomes                                                                                                                                                                                 |
| Adapter parity    | ingest, CSP-report, renderer routes and drop surfacing equivalent across Fastly/Viceroy, Axum, Cloudflare, Spin                                                                                                                                                          |
| Policy            | script-creative warning; `invalid_dimensions` + bounded w/h; `dimensions_out_of_range` unclamped; page-bids `debug` gating; diagnostic completeness                                                                                                                      |

## 10. Alternatives considered

1. **Patching APS point-failures without telemetry** — rejected: three
   correct fixes produced no ads; the next fix would be another guess.
2. **Always direct-render APS (skip GAM/PUC)** — rejected: unilaterally
   changes GAM reporting/pacing; kept only as the attributed-`gam_empty`
   fallback.
3. **Single module graph / shared chunks now** — rejected for this release:
   changes the delivery pipeline while everything else changes; recorded as
   the successor behind the same registry surface.
4. **Full rewrite in one branch without phases** — rejected: the browser-spec
   safety net is thinnest exactly where behavior changes.
5. **Dropping the ES5 bootstrap** — rejected: loses the pinned no-bundle
   guarantee; generation from TS keeps it without dual maintenance.
6. **Timeout-triggered fallback** — rejected: GPT requests cannot be
   cancelled; a timeout race can double-render and double-bill.
7. **N/N−1 compatibility machinery** (revisions 3–4) — removed by the §0
   policy decision: version ranges, retained artifacts, legacy URLs, and
   dual-name globals deleted in favor of exact release matching and a
   coordinated switch.

## 11. Risks

- **Hard cutover blast radius:** in-flight pages fail at switch; accepted by
  policy (§0); bounded by the purge + 24 h monitored window + traffic-switch
  rollback.
- **Mediator wire-contract change** (candidate-id echo) requires
  coordinating the mediator implementation; until echoed ids exist,
  `merge_highest_cpm` cannot be enabled (config validation enforces this).
- **Notification triggers become a published contract** for PBS-path demand;
  changing them later is a breaking change for SSP reporting.
- **Beacon abuse:** bounded by pre-parse caps, origin checks, fail-closed
  numeric rate limits, server-decided sampling, signed authorizations.
- **Registry/limiter memory:** all client and server maps carry explicit
  capacities, TTLs, and eviction rules (G2, §5.4).
- **CSP relaxation:** enforced-cohort canary + new immutable version prevent
  false-clean canaries; bucketed aggregation prevents cardinality abuse.
- **Sink blindness:** fire-and-forget dispatch is compensated by mandatory
  datasource-side freshness/rejection monitoring.

## 12. Success criteria

1. APS creatives render in each configured flow — SSAT, client-Prebid,
   page-bids, and direct `/auction` (G4f) — hermetically in CI and in the
   release-gating real-GAM suite.
2. Every §2 failure maps to its §2.5 signal; diagnostic mode names the
   failing class from one page load; §5.7 objectives hold on sink-backed
   deployments.
3. Boundary lint zero exceptions; stateful sharing only via the G3 registry;
   exact-release mismatches quarantine loudly.
4. No `src/` file exceeds ~500 lines; `gpt_bootstrap.js` is a stub or
   generated.
5. Trace counts are per-impression; orphan recovery has a non-vacuous test;
   cycle attribution follows G4a (physical requests, publisher-overlap
   quarantine, drain/re-arm).
6. The only TSJS-owned global is `window.tsjs` with the §7.4 final shape; no
   expandos on GPT slots, GPT functions, or `pbjs`; legacy names are gone at
   cutover.
7. §7.10 budgets hold (three vectors, pinned tools, one-sided server gates).
8. No existing warning is lost; every issue-surfacing condition logs `warn`+
   with the beacon's reason code.
9. TypeScript floor matches the resolved 5.9 line with strictness flags on;
   `prebid.js` pin matches the documented deployed bundle.
10. `nurl`/`burl` fire only on carrying paths at their G4d binds,
    attempt-scoped and idempotent; APS fires neither.
11. Trace-bearing responses are `private, no-store` by test; authorizations
    are per-trace, signed, mode-carrying, and never accepted unsigned.
12. The cutover runbook has been rehearsed (switch, purge, rollback switch)
    before the production switch.

## 13. Open questions

1. Is a mediator configured in the affected production deployment?
2. What share of live APS demand is `tagtype: "script"`?
3. Should `client_render_fallback` ever become default-on for publishers
   without GAM line items for `hb_bidder=aps`?
4. Is PR #997 the intended restoration of the lost #922 attribution core, or
   should the original be re-merged?
5. Do Axum/Cloudflare/Spin get real client-event sinks, or keep
   accept-count-drop?
6. Does Amazon expose any creative-completion acknowledgement that could add
   a confirmed state beyond `render_accepted` under a new name?
7. Who implements and owns the mediator-side candidate-id echo (6.1), and on
   what timeline relative to this release?
