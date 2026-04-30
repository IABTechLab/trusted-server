# Server-Side Ad Templates Design

_Author · 2026-04-15_

---

## 1. Problem Statement

Today's display ad pipeline on most publisher sites is structurally sequential and
browser-bound:

1. Page HTML arrives at browser
2. Prebid.js (~300KB) downloads and parses
3. Smart Slots SDK scans the DOM to discover ad placements
4. `addAdUnits()` registers slot definitions
5. Prebid auction fires from the browser (~80–150ms RTT to SSPs)
6. Bids return (~1,000–1,500ms window)
7. GPT `setTargeting()` + `refresh()` fires
8. GAM creative renders

**Total time to ad visible: ~3,100ms.**

The browser is the slowest possible place to run an auction. It must first download and
parse multiple SDKs, scan the DOM to discover what ad slots exist, and then fire SSP
requests over a consumer internet connection with high and variable latency.

Trusted Server sits at the Fastly edge — milliseconds from the user, with
data-center-to-data-center RTT to Prebid Server (~20–30ms vs ~80–150ms from a browser).
The server knows, from the request URL alone, exactly which ad slots are available on any
given page. There is no reason to wait for the browser.

---

## 2. Goal

Enable Trusted Server to:

1. Match an incoming page request URL against a set of pre-configured slot templates
2. Immediately fire the full server-side auction (all providers: PBS, APS, future wrappers)
   in parallel with the origin HTML fetch — before the browser receives a single byte
3. Inject GPT slot definitions into `<head>` so the client can define slots without any SDK
4. Inject pre-collected winning bids directly into `<head>` as `window.__ts_bids` — the
   client reads this global directly, bypassing the `/auction` POST entirely for
   URL-matched pages. The `/auction` endpoint is retained as a fallback for pages whose
   URLs do not match any slot template, preserving backward compatibility for publishers
   who have not yet adopted `creative-opportunities.toml`.
5. Eliminate Prebid.js from the client entirely

**Target time to ad visible: ~1,200ms. Net saving: ~2,000ms.**

> **Note:** The latency numbers in this document are modeled estimates based on known
> edge→PBS RTT ranges and typical origin response times. They should be validated with
> production measurements after Phase 1 ships.

---

## 3. Non-Goals

- Eliminating client-side GPT / Google Ad Manager — GAM remains in the rendering
  pipeline for Phase 1. The GAM call (`securepubads.g.doubleclick.net`) moves
  server-side in a future phase (see §9.6).
- Dynamic slot discovery (reading the DOM) — this design commits to pre-defined,
  URL-matched slot templates. Smart Slots' dynamic injection behavior is replaced by
  server knowledge.
- Changing the `AuctionOrchestrator` internally — the orchestrator already handles
  parallel provider fan-out. This design adds a new trigger point, not new auction
  logic.

---

## 4. Architecture

### 4.1 New File: `creative-opportunities.toml`

A new config file at the repo root, alongside `trusted-server.toml`. It holds all slot
templates: page pattern matching rules, ad formats, floor prices, GAM targeting
key-values, and per-provider bidder params. PBS bidder-level params (placement IDs,
account IDs) live in Prebid Server stored requests, keyed by slot ID. APS params are
specified inline per slot under `[slot.providers.aps]`.

Loaded at build time via `include_str!()` and compiled into the WASM binary. Slot
changes require a redeploy; this is intentional (fast reads, no KV overhead, no
per-request cost). A migration path to KV-backed config is tracked in §9.5.

`floor_price` is the publisher-owned hard floor per slot — the source of truth for the
minimum acceptable bid price, enforced at the edge before bids reach the ad server. Any
bid below the floor is discarded at the orchestrator level before it enters `__ts_bids`.
SSPs may apply their own dynamic floors independently within their platforms; this floor
is the publisher's baseline that supersedes all other floor logic by virtue of being
enforced earliest in the pipeline.

#### Top-level config (in `trusted-server.toml`)

```toml
[creative_opportunities]
# GAM network ID used to construct default ad-unit paths.
gam_network_id = "21765378893"

# Optional. Defaults to [auction].timeout_ms if not set.
# Recommended: 500ms (vs client-side 1000–1500ms) due to lower edge→PBS RTT.
# This value gates both the auction deadline and the <head>-boundary hold in
# streaming mode — they share the same deadline (T₀ + auction_timeout_ms).
auction_timeout_ms = 500

# Granularity table for hb_pb price bucket strings.
# Options: "low" | "medium" | "high" | "auto" | "dense" | "custom"
# Defaults to "dense" if not set.
price_granularity = "dense"
```

#### `creative-opportunities.toml` schema

```toml
[[slot]]
id = "atf_sidebar_ad"
# Optional. Defaults to "/{gam_network_id}/{id}".
# Override for non-standard GAM ad-unit paths.
gam_unit_path = "/21765378893/publisher/atf-sidebar"
# Optional. DOM container element ID. Defaults to slot id.
div_id = "div-atf-sidebar"
page_patterns = ["/20**"]
formats = [{ width = 300, height = 250 }]
floor_price = 0.50

[slot.targeting]
pos = "atf"
zone = "atfSidebar"

[slot.providers.aps]
slot_id = "aps-slot-atf-sidebar"

[[slot]]
id = "below-content-ad"
page_patterns = ["/20**"]
formats = [{ width = 300, height = 250 }, { width = 728, height = 90 }]
floor_price = 0.25

[slot.targeting]
pos = "btf"
zone = "belowContent"

[slot.providers.aps]
slot_id = "aps-slot-below-content"

[[slot]]
id = "ad-homepage-0"
page_patterns = ["/", "/index.html"]
formats = [{ width = 970, height = 250 }, { width = 728, height = 90 }]
floor_price = 1.00

[slot.targeting]
pos = "atf"
zone = "homepage"
slot_index = "0"

[slot.providers.aps]
slot_id = "aps-slot-homepage-0"
```

#### Rust types

```rust
#[derive(Debug, Clone, serde::Deserialize)]
pub struct CreativeOpportunitiesConfig {
    pub gam_network_id: String,
    #[serde(default = "default_auction_timeout_ms")]
    pub auction_timeout_ms: Option<u32>,
    #[serde(default = "PriceGranularity::dense")]
    pub price_granularity: PriceGranularity,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct CreativeOpportunitySlot {
    pub id: String,
    pub gam_unit_path: Option<String>,   // defaults to /{gam_network_id}/{id}
    pub div_id: Option<String>,           // defaults to id
    pub page_patterns: Vec<String>,
    pub formats: Vec<CreativeOpportunityFormat>,
    pub floor_price: Option<f64>,
    #[serde(default)]
    pub targeting: HashMap<String, String>, // strings only — validated at startup
    #[serde(default)]
    pub providers: SlotProviders,
}

/// Separate from auction::AdFormat so media_type can default to Banner
/// without requiring it in the TOML. Converted to AdFormat at auction time.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct CreativeOpportunityFormat {
    pub width: u32,
    pub height: u32,
    #[serde(default = "MediaType::banner")]
    pub media_type: MediaType,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct SlotProviders {
    pub aps: Option<ApsSlotParams>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct ApsSlotParams {
    pub slot_id: String,
}
```

> **Targeting value types:** `targeting` values are `String`-only (not
> `serde_json::Value`). GPT's `setTargeting()` only accepts `string | string[]`;
> non-string values are silently dropped by the browser. Validated at startup — a
> non-string targeting value is a startup error.

> **Slot ID validation:** Slot IDs are validated at startup against a strict allowlist
> (`[A-Za-z0-9_-]+`). IDs outside this set fail startup. This prevents XSS via
> crafted IDs appearing in the injected `<script>` block.

### 4.2 URL Pattern Matching

At request time, TS matches the request path against each slot's `page_patterns`.
Patterns use the `glob` crate (WASM-compatible):

- `/20**` — matches all date-prefixed article paths (e.g., `/2024/01/my-article/`). Note:
  `**` matches across path separators; `*` stops at `/`. Use `**` for multi-segment
  patterns.
- `/` — matches the homepage exactly
- `/index.html` — exact match

Multiple slots can match a single URL. All matching slots are collected and fed into a
single auction as separate impressions. Pattern matching is purely in-memory against the
pre-parsed config — sub-millisecond. The pattern-match cost is O(slots × patterns);
startup logs the compiled pattern count.

### 4.3 Auction Trigger

#### Async restructuring of the publisher path

The current `handle_publisher_request` is a synchronous `fn`. Running the auction in
parallel with the origin fetch requires:

1. Converting `handle_publisher_request` to `async fn`
2. Switching the origin fetch from blocking `.send()` to `.send_async()` (returns
   `PlatformPendingRequest`)
3. Adding `orchestrator: &AuctionOrchestrator` as a parameter
4. Awaiting both the auction future and the pending origin request (via the platform's
   `select` / join primitive)

This is an explicit structural change to the publisher request path. It is listed in §7
as a required migration, not a minor modification.

#### Consent and EC ID

Before firing the auction, TS reads consent signals and the EC ID from the incoming page
request — the same pipeline already executed in `handle_publisher_request` (EC ID at
line 483, consent at lines 497–508). These are forwarded into `AuctionRequest` exactly
as they are today from the client POST path.

Consent gating:

- If consent is **absent or denied** (no TCF consent string, or purpose 1 not consented):
  the auction is not fired. `__ts_bids` is omitted from the page. GPT falls back to its
  own auction. This is treated as a first-class edge case in §8.
- **Mid-page consent revocation** is out of scope for Phase 1; bids already injected into
  `<head>` remain. Phase 2 will address consent event propagation.

EC ID behavior at the new trigger is identical to the existing path — generated or read
from cookie, forwarded in the `AuctionRequest`.

#### Auction execution

When slots are matched and consent is present, TS calls
`AuctionOrchestrator::run_auction()` with the matched slots converted to `AdSlot`
objects. This happens at request receipt time — in parallel with the origin fetch via
`send_async()`.

The orchestrator's existing behaviour is unchanged:

- All providers (PBS, APS) are dispatched simultaneously
- Per-provider timeout budgets are enforced from the remaining auction deadline
  (`creative_opportunities.auction_timeout_ms`, falling back to `[auction].timeout_ms`)
- Floor price filtering, bid unification, and winning bid selection are applied as today
- PBS resolves bidder params from its stored requests by slot ID
- APS bidder params are read from `[slot.providers.aps]` in `creative-opportunities.toml`

**On NextJS 14 (buffered mode):** TS awaits both the origin response and the auction
future. The origin response is buffered at the edge until both complete (or the auction
deadline fires). The HTML processor is then constructed with bid results already
resolved, captured in the element-handler closure — no async state needed inside
`lol_html`. TTFB is deferred until the later of origin response or auction deadline; this
is an accepted tradeoff against the revenue upside.

**On NextJS 16 (streaming mode):** TS streams HTML chunks to the browser immediately.
The auction runs concurrently. To inject `__ts_bids` before `</head>`, the HTML
processor registers an `el.on_end_tag()` handler on the `</head>` element. If the auction
has not resolved by the time that handler fires, TS waits up to the remaining budget
(same `auction_timeout_ms` deadline), then flushes with whatever bids have arrived.
Content after `</head>` is never held. This requires a new `on_end_tag` injection
primitive in `html_processor.rs`; see §7.

### 4.4 Head Injection

TS injects two separate `<script>` blocks into `<head>`:

**First injection — `window.__ts_ad_slots`** — emitted at the `<head>` opening tag,
immediately from config. No auction needed. Available to GPT the moment the browser
parses `<head>`. Owned by the `gpt` integration head injector (not `prebid.rs`):

```json
[
  {
    "id": "atf_sidebar_ad",
    "gam_unit_path": "/21765378893/publisher/atf-sidebar",
    "div_id": "div-atf-sidebar",
    "formats": [[300, 250]],
    "targeting": { "pos": "atf", "zone": "atfSidebar" }
  },
  {
    "id": "below-content-ad",
    "gam_unit_path": "/21765378893/below-content-ad",
    "div_id": "below-content-ad",
    "formats": [
      [300, 250],
      [728, 90]
    ],
    "targeting": { "pos": "btf", "zone": "belowContent" }
  }
]
```

**Second injection — `window.__ts_bids`** — injected just before `</head>` once auction
results are available. Keyed by slot ID. The client reads this directly — no `/auction`
POST needed for matched pages:

```json
{
  "atf_sidebar_ad": {
    "hb_pb": "2.50",
    "hb_bidder": "kargo",
    "hb_adid": "abc123",
    "burl": "https://ssp.example/billing?id=abc123"
  },
  "below-content-ad": {
    "hb_pb": "1.00",
    "hb_bidder": "appnexus",
    "hb_adid": "def456",
    "burl": "https://appnexus.example/billing?id=def456"
  }
}
```

`hb_pb` is computed using the **dense** granularity table (publisher-configurable via
`price_granularity` in `[creative_opportunities]`). The key set is `hb_pb`, `hb_bidder`,
`hb_adid`, and `burl` — matching GAM standard Prebid targeting keys. `burl` is included
so the client can fire it from the `slotRenderEnded` event (see §4.5).

If a slot receives no bid above floor, its entry is omitted from `__ts_bids`. The client
treats absence as no pre-set targeting for that slot — GPT fires without bid targeting,
GAM falls back to its standard auction.

> **Security:** All string values in `__ts_bids` are JSON-serialized via `serde_json`
> and HTML-attribute-escaped before insertion into the `<script>` block. The injection
> wrapper is always `<script>window.__ts_bids = JSON.parse(ESCAPED_JSON);</script>`, not
> raw string interpolation.

> **Cache contract:** Any response with `__ts_bids` injected is per-user data and must
> not be cached. TS sets `Cache-Control: private, no-store` on the response before
> forwarding, overriding any conflicting cache headers from the publisher origin.
> `Surrogate-Control` and `Fastly-Surrogate-Control` are also stripped.

### 4.5 Win Notifications

Win notification responsibilities are split by where the truth lives:

**`nurl` (SSP win event) — fired server-side.** When the orchestrator selects a winning
bid, TS fires a fire-and-forget background HTTP request to `nurl` from the edge
(edge→SSP RTT ~20–30ms, no auction-path latency cost). A per-integration switch
(`[integrations.prebid].fire_nurl_at_edge`, default `true`) handles cases where the PBS
deployment already fires win events internally to avoid double-firing. APS win
notification follows its own spec.

**`burl` (billing event) — fired client-side.** `burl` is embedded per slot in
`__ts_bids` (see §4.4). The `__tsAdInit` script registers a GPT `slotRenderEnded`
listener after defining slots. On render: if `!event.isEmpty` and
`event.slot.getTargeting('hb_adid')[0] === bidData.hb_adid`, the client fires `burl`
via `navigator.sendBeacon`. This confirms both that the ad rendered and that our specific
Prebid bid (not a direct deal or backfill) won the GAM line item match.

### 4.6 Client Residual

Prebid.js is eliminated. The client-side ad bootstrap is replaced by a small inline
script (~30 lines) that reads `__ts_ad_slots` and `__ts_bids`, drives GPT directly, and
handles billing notifications:

```javascript
window.__tsAdInit = function () {
  var slots = window.__ts_ad_slots || []
  var bids = window.__ts_bids || {}
  googletag.cmd.push(function () {
    slots.forEach(function (slot) {
      var gptSlot = googletag
        .defineSlot(slot.gam_unit_path, slot.formats, slot.div_id)
        .addService(googletag.pubads())
      // Apply static targeting from config
      Object.entries(slot.targeting).forEach(function ([k, v]) {
        gptSlot.setTargeting(k, v)
      })
      // Apply pre-won bid targeting if available
      var bidData = bids[slot.id] || {}
      ;['hb_pb', 'hb_bidder', 'hb_adid'].forEach(function (key) {
        if (bidData[key]) gptSlot.setTargeting(key, bidData[key])
      })
    })
    googletag.pubads().enableSingleRequest()
    googletag.enableServices()
    // Fire burl on confirmed render
    googletag.pubads().addEventListener('slotRenderEnded', function (event) {
      var slotId = event.slot.getSlotElementId()
      var bidData = bids[slotId] || {}
      if (
        !event.isEmpty &&
        bidData.burl &&
        event.slot.getTargeting('hb_adid')[0] === bidData.hb_adid
      ) {
        navigator.sendBeacon(bidData.burl)
      }
    })
    googletag.pubads().refresh()
  })
}
```

This script is part of the existing `gpt` integration bundle
(`crates/js/lib/src/integrations/gpt/index.ts`), extending the existing GPT shim.
Injected via the `gpt` head injector alongside `window.__ts_ad_slots`.

---

## 5. Request-Time Sequence

```
t=0ms     GET ts.publisher.com/article arrives at Fastly edge

t=1ms     URL matched against creative-opportunities.toml
          Slots matched: [atf_sidebar_ad, below-content-ad, section_ad]
          Consent check: TCF consent present → auction proceeds

t=2ms     AuctionOrchestrator.run_auction() called
          PBS + APS dispatched in parallel via send_async()
          Edge→PBS RTT: ~20–30ms

t=2ms     Origin fetch dispatched via send_async() in parallel

t=2ms     window.__ts_ad_slots script assembled from config (no auction needed)

t=150ms   Origin HTML arrives at edge (NextJS 14: buffered)
          Auction still running; origin response held at edge

t=502ms   Auction deadline fires (500ms budget)
          Winning bids collected; nurl fired as background requests

t=502ms   HtmlProcessorConfig constructed with bid results captured
          <head> injection assembled:
          - window.__ts_ad_slots  (from config, ready at t=2ms)
          - window.__ts_bids      (from auction results; Cache-Control: private, no-store set)

t=502ms   HTML forwarded to browser with injected <head>

t=652ms   HTML arrives at browser (150ms network)
          window.__ts_ad_slots and window.__ts_bids already in <head>
          tsjs bundle tag in <head> (~30KB)

t=682ms   tsjs downloads + executes (30ms, edge-served CDN)
          __tsAdInit() reads __ts_ad_slots + __ts_bids directly
          No /auction POST needed — bids are already in the page

t=702ms   googletag.pubads().refresh() fires

t=822ms   GET /gampad/ads

t=922ms   Creative fetch

t=1222ms  Creative sub-resources + paint; burl fired via slotRenderEnded

          AD VISIBLE ~1200ms
```

---

## 6. Performance Summary

| Stage               | Client-side today | With TS templates | Saving       |
| ------------------- | ----------------- | ----------------- | ------------ |
| Script load chain   | ~700ms            | ~40ms (tsjs only) | -660ms       |
| Script parse/JIT    | ~280ms            | ~10ms             | -270ms       |
| Sequential SDK hops | ~200ms            | 0                 | -200ms       |
| Auction window      | ~1,500ms          | ~500ms            | -1,000ms     |
| GAM + creative      | ~570ms            | ~570ms            | —            |
| TTFB penalty¹       | 0                 | up to +350ms      | -            |
| **Total**           | **~3,250ms**      | **~1,200ms**      | **~2,000ms** |

¹ Buffered mode only: the origin response is held until the auction resolves. For fast
origins (<150ms) and a 500ms auction deadline, TTFB may increase by up to 350ms. This
tradeoff is net-positive on revenue. The streaming mode (NextJS 16) has no TTFB penalty.

Auction RTT improvement: browser fires SSP requests at 80–150ms RTT; edge fires at
20–30ms. Auction timeout can drop from 1,000–1,500ms to 500ms while still collecting
more complete results, because edge→PBS latency is ~5–7x lower.

---

## 7. Implementation Scope

### New

- `creative-opportunities.toml` — slot template config file
- `crates/trusted-server-core/src/creative_opportunities.rs` — config types, TOML
  parsing, URL glob matching, slot-to-`AdSlot` conversion, price bucketing
- `crates/trusted-server-core/build.rs` — `include_str!()` for
  `creative-opportunities.toml`; startup slot-ID validation
- `crates/trusted-server-core/src/price_bucket.rs` — Prebid price granularity tables
  (dense default; publisher-configurable); converts raw CPM `f64` to `hb_pb` string

### Modified

- **`crates/trusted-server-core/src/publisher.rs`** — primary structural change:
  - Convert `handle_publisher_request` from `fn` to `async fn`
  - Switch origin fetch from `.send()` to `.send_async()` (returns
    `PlatformPendingRequest`)
  - Add `orchestrator: &AuctionOrchestrator` parameter
  - Match slots, check consent, fire auction and origin fetch concurrently
  - Await both and construct `HtmlProcessorConfig` with resolved bid results
- **`crates/trusted-server-adapter-fastly/src/main.rs`** — update `route_request` call
  site to `.await` the now-async publisher handler; pass orchestrator reference
- **`crates/trusted-server-core/src/html_processor.rs`** — inject `window.__ts_bids`
  before `</head>` via `el.on_end_tag()` on the `</head>` element; set
  `Cache-Control: private, no-store` header on injection; HTML-escape bid JSON
- **`crates/trusted-server-core/src/integrations/gpt.rs`** — extend head injector to
  emit `window.__ts_ad_slots` from matched slots (not `prebid.rs`); emit `__tsAdInit`
  bootstrap script
- **`crates/js/lib/src/integrations/gpt/index.ts`** — add `__tsAdInit` function and
  `slotRenderEnded` burl-firing logic to the existing GPT shim
- **`crates/trusted-server-core/src/integrations/prebid.rs`** — add
  `fire_nurl_at_edge` config key; add nurl fire-and-forget call in orchestrator result
  handling
- **`trusted-server.toml`** — add `[creative_opportunities]` section
- **`crates/trusted-server-core/src/settings.rs`** — add `CreativeOpportunitiesConfig`
  to `Settings`

### Unchanged

- `AuctionOrchestrator` internals — no changes; new call site only
- PBS stored request configuration — bidder params remain in PBS, keyed by slot ID
- GAM line item configuration — targeting key-values pass through unchanged

---

## 8. Edge Cases

**No slots match the URL** — auction is not fired. Neither global is emitted. The page
loads with no TS ad stack; existing client-side Prebid/GPT flow runs unmodified (for
publishers in dual-mode rollout).

**Consent absent or denied** — auction is not fired. Neither global is emitted.
`Cache-Control: private, no-store` is still set (to prevent caching the consent-negative
response if personalised ads were previously served). Page loads normally; GAM runs its
own auction without Prebid targeting.

**Auction times out with partial results** — `__ts_bids` is populated with whatever bids
arrived before the deadline. Slots with no bid are omitted. GPT fires without pre-set
targeting for those slots; GAM falls back to its own auction for them.

**Auction times out with zero results** — `__ts_bids` is an empty object `{}`. All slots
fire GAM without bid targeting. No revenue impact beyond the timeout scenario itself.

**Origin is slow (NextJS 14, buffered)** — auction has more time; results more likely to
be complete. TTFB impact is bounded by the origin latency, not additive to it.

**NextJS 16 streaming** — `el.on_end_tag()` on `</head>` gates injection. TS waits up to
the remaining `auction_timeout_ms` budget, then flushes. Content after `</head>` is never
held. If the auction resolves before `</head>` is encountered (common case), injection is
zero-latency.

**`creative-opportunities.toml` missing or malformed** — startup fails with a clear
error. No silent degradation.

**Config empty (zero slots)** — treated as "no match" for all URLs; auction never fires.
No error. Useful as a kill-switch: deploying an empty `creative-opportunities.toml`
disables the feature without a code change.

**Slot ID not found in PBS stored requests** — PBS returns a no-bid for that slot. Slot
is omitted from `__ts_bids`. The remaining slots proceed normally.

---

## 9. Open Questions

1. **URL pattern coverage** — does `/20**` cover all article paths, or are there
   non-date-prefixed article URLs? Publisher to confirm.
2. **PBS stored request setup** — slot IDs in `creative-opportunities.toml` must have
   corresponding stored requests configured in the publisher's PBS instance before this
   goes live.
3. **Homepage slot count** — the example shows slots 0 and 1. Are there additional slots
   following the same pattern? Slot IDs and count to be confirmed with ad ops.
4. **Auction timeout** — ✅ Resolved: new dedicated key
   `[creative_opportunities].auction_timeout_ms` with fallback to `[auction].timeout_ms`.
   Per-provider ceilings (`[integrations.prebid].timeout_ms`,
   `[integrations.aps].timeout_ms`) remain unchanged; the orchestrator's existing
   `min(remaining_budget, provider_timeout)` logic applies.
5. **KV-backed config migration path** — Phase 1 ships with `include_str!()` for
   simplicity and cost. When ad ops require live slot edits between deploys, the migration
   path is: load from `services.kv_store()` at request time with a compiled-in fallback.
   Design tracked as a follow-up before Phase 2.
6. **Phase 2 server-side GAM** — The real latency ceiling is the GAM call
   (`securepubads.g.doubleclick.net`). Phase 2 routes the GAM ad request through the edge
   (securepubads proxy + creative bundling), eliminating the last browser→Google hop. The
   Phase 1 architecture is designed to be shape-compatible with this: `__ts_ad_slots`
   gives the edge the full slot inventory it needs to build a server-side GAM request.
7. **`tsjs-gpt` bootstrap delivery** — ✅ Resolved: `__tsAdInit` is part of the existing
   `gpt` integration bundle, not a new integration. Injection order: `window.__ts_ad_slots`
   → existing GPT shim → `__tsAdInit` — all emitted by the `gpt` head injector in a single
   `<script>` block, guaranteeing order before GPT.js loads.
