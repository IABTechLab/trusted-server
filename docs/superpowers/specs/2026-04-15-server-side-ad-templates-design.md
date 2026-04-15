# Server-Side Ad Templates Design

*April 2026*

---

## 1. Problem Statement

Today's display ad pipeline on most publisher sites is structurally sequential
and browser-bound:

1. Page HTML arrives at browser
2. Prebid.js (~300KB) downloads and parses
3. Smart Slots SDK scans the DOM to discover ad placements
4. `addAdUnits()` registers slot definitions
5. Prebid auction fires from the browser (~80–150ms RTT to SSPs)
6. Bids return (~1,000–1,500ms window)
7. GPT `setTargeting()` + `refresh()` fires
8. GAM creative renders

**Total time to ad visible: ~3,100ms.**

The browser is the slowest possible place to run an auction. It must first download and parse
multiple SDKs, scan the DOM to discover what ad slots exist, and then fire SSP requests over
a consumer internet connection with high and variable latency.

Trusted Server sits at the Fastly edge — milliseconds from the user, with data-center-to-data-center
RTT to Prebid Server (~20–30ms vs ~80–150ms from a browser). The server knows, from the request
URL alone, exactly which ad slots are available on any given page. There is no reason to wait for
the browser.

---

## 2. Goal

Enable Trusted Server to:

1. Match an incoming page request URL against a set of pre-configured slot templates
2. Immediately fire the full server-side auction (all providers: PBS, APS, future wrappers) in
   parallel with the origin HTML fetch — before the browser receives a single byte
3. Inject GPT slot definitions into `<head>` so the client can define slots without any SDK
4. Return pre-collected winning bids to the browser's lightweight `/auction` POST before the
   browser would have even finished parsing Prebid.js
5. Eliminate Prebid.js from the client entirely

**Target time to ad visible: ~1,200ms. Net saving: ~2,000ms.**

---

## 3. Non-Goals

- Eliminating client-side GPT / Google Ad Manager — GAM remains in the rendering pipeline
  for Phase 1. The GAM call (`securepubads.g.doubleclick.net`) moves server-side in a future phase.
- Dynamic slot discovery (reading the DOM) — this design commits to pre-defined, URL-matched
  slot templates. Smart Slots' dynamic injection behavior is replaced by server knowledge.
- Changing the `AuctionOrchestrator` internally — the orchestrator already handles parallel
  provider fan-out. This design adds a new trigger point, not new auction logic.

---

## 4. Architecture

### 4.1 New File: `creative-opportunities.toml`

A new config file at the repo root, alongside `trusted-server.toml`. It holds all slot templates:
page pattern matching rules, ad formats, floor prices, and GAM targeting key-values. Bidder-level
params (placement IDs, account IDs) live in Prebid Server stored requests, keyed by slot ID — not
in this file.

Loaded at build time via `include_str!()`, parsed into `Vec<CreativeOpportunitySlot>` at startup.
Ad ops can edit this file independently of server configuration.

`floor_price` is the publisher-owned hard floor per slot — the source of truth for the minimum
acceptable bid price, enforced at the edge before bids reach the ad server. Any bid below the
floor is discarded at the orchestrator level before it enters `__ts_bids`. SSPs may apply their
own dynamic floors independently within their platforms; this floor is the publisher's baseline
that supersedes all other floor logic by virtue of being enforced earliest in the pipeline.

**Schema:**

```toml
[[slot]]
id = "atf_sidebar_ad"
page_patterns = ["/20*/"]
formats = [{ width = 300, height = 250 }]
floor_price = 0.50

[slot.targeting]
pos = "atf"
zone = "atfSidebar"

[[slot]]
id = "below-content-ad"
page_patterns = ["/20*/"]
formats = [{ width = 300, height = 250 }, { width = 728, height = 90 }]
floor_price = 0.25

[slot.targeting]
pos = "btf"
zone = "belowContent"

[[slot]]
id = "ad-homepage-0"
page_patterns = ["/", "/index.html"]
formats = [{ width = 970, height = 250 }, { width = 728, height = 90 }]
floor_price = 1.00

[slot.targeting]
pos = "atf"
zone = "homepage"
slot_index = "0"
```

**Rust type:**

```rust
#[derive(Debug, Clone, serde::Deserialize)]
pub struct CreativeOpportunitySlot {
    pub id: String,
    pub page_patterns: Vec<String>,
    pub formats: Vec<AdFormat>,
    pub floor_price: Option<f64>,
    pub targeting: HashMap<String, serde_json::Value>,
}
```

### 4.2 URL Pattern Matching

At request time, TS matches the request path against each slot's `page_patterns`. Patterns are
glob-style strings:

- `/20*/` — matches all date-prefixed article paths (e.g., `/2024/01/my-article/`)
- `/` — matches the homepage exactly
- `/index.html` — exact match

Multiple slots can match a single URL. All matching slots are collected and fed into a single
auction as separate impressions. Pattern matching is purely in-memory against the pre-parsed
config — sub-millisecond.

### 4.3 Auction Trigger

When slots are matched, TS immediately calls `AuctionOrchestrator::run_auction()` with the
matched slots converted to `AdSlot` objects. This happens at request receipt time — in parallel
with the origin fetch.

The orchestrator's existing behaviour is unchanged:
- All providers (PBS, APS, any configured wrappers) are dispatched simultaneously
- Per-provider timeout budgets are enforced from the remaining auction deadline
- Floor price filtering, bid unification, and winning bid selection are applied as today
- PBS resolves bidder params from its stored requests by slot ID — no bidder params travel
  through TS or the browser

**On NextJS 14 (buffered mode):** TS must buffer the full origin response before forwarding.
This gives the auction the entire origin response time (~150–400ms typical) to run before
any HTML is forwarded. In practice, bids are often collected before origin even responds.

**On NextJS 16 (streaming mode):** TS streams HTML chunks to the browser immediately. The
auction runs in parallel. Bid injection into `<head>` must complete before the `</head>` tag
is forwarded. If the auction has not returned by the time `</head>` is encountered, TS waits
up to the remaining auction budget, then flushes with whatever bids have arrived (partial
results) or no targeting if timed out. Content after `</head>` is never held.

### 4.4 Head Injection

TS injects two separate `<script>` blocks into `<head>`:

**First injection — `window.__ts_ad_slots`** — emitted immediately from config, before the
origin fetch even returns. No auction needed. Available to GPT the moment the browser parses `<head>`:

```json
[
  {
    "id": "atf_sidebar_ad",
    "formats": [[300, 250]],
    "targeting": { "pos": "atf", "zone": "atfSidebar" }
  },
  {
    "id": "below-content-ad",
    "formats": [[300, 250], [728, 90]],
    "targeting": { "pos": "btf", "zone": "belowContent" }
  }
]
```

**Second injection — `window.__ts_bids`** — injected once auction results are available, just
before `</head>`. Keyed by slot ID. The client reads this directly — no `/auction` POST needed:

```json
{
  "atf_sidebar_ad": { "hb_pb": "2.50", "hb_bidder": "kargo", "hb_adid": "abc123" },
  "below-content-ad": { "hb_pb": "1.00", "hb_bidder": "appnexus", "hb_adid": "def456" }
}
```

If a slot receives no bid above floor, its entry is omitted from `__ts_bids`. The client
treats absence as no pre-set targeting for that slot — GPT fires without bid targeting,
GAM falls back to its standard auction.

### 4.5 Client Residual

Prebid.js is eliminated. The client-side ad bootstrap is replaced by a small inline script
(~20 lines) that reads `__ts_ad_slots` and `__ts_bids` and drives GPT directly:

```javascript
window.__tsAdInit = function() {
  var slots = window.__ts_ad_slots || [];
  var bids = window.__ts_bids || {};
  googletag.cmd.push(function() {
    slots.forEach(function(slot) {
      var gptSlot = googletag.defineSlot(slot.id, slot.formats, slot.id)
        .addService(googletag.pubads());
      // Apply static targeting from config
      Object.entries(slot.targeting).forEach(function([k, v]) {
        gptSlot.setTargeting(k, v);
      });
      // Apply pre-won bid targeting if available
      var bidTargeting = bids[slot.id] || {};
      Object.entries(bidTargeting).forEach(function([k, v]) {
        gptSlot.setTargeting(k, v);
      });
    });
    googletag.pubads().enableSingleRequest();
    googletag.enableServices();
    googletag.pubads().refresh();
  });
};
```

This script is part of the `tsjs-gpt` integration bundle, injected by TS into every matching
page response alongside the existing GPT integration.

---

## 5. Request-Time Sequence

```
t=0ms     GET ts.publisher.com/article arrives at Fastly edge

t=1ms     URL matched against creative-opportunities.toml
          Slots matched: [atf_sidebar_ad, below-content-ad, section_ad]

t=2ms     AuctionOrchestrator.run_auction() called
          PBS + APS dispatched in parallel
          Edge→PBS RTT: ~20–30ms

t=2ms     Origin fetch dispatched in parallel

t=150ms   Origin HTML arrives at edge (NextJS 14: buffered)

t=502ms   Auction timeout fires (500ms budget)
          Winning bids collected

t=502ms   <head> injection assembled:
          - window.__ts_ad_slots  (from config, available at t=1ms)
          - window.__ts_bids      (from auction results)

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

t=1222ms  Creative sub-resources + paint

          AD VISIBLE ~1200ms
```

---

## 6. Performance Summary

| Stage | Client-side today | With TS templates | Saving |
|---|---|---|---|
| Script load chain | ~700ms | ~40ms (tsjs only) | -660ms |
| Script parse/JIT | ~280ms | ~10ms | -270ms |
| Sequential SDK hops | ~200ms | 0 | -200ms |
| Auction window | ~1,500ms | ~500ms | -1,000ms |
| GAM + creative | ~570ms | ~570ms | — |
| **Total** | **~3,250ms** | **~1,200ms** | **~2,000ms** |

Auction RTT improvement: browser fires SSP requests at 80–150ms RTT; edge fires at 20–30ms.
Auction timeout can drop from 1,000–1,500ms to 500ms while still collecting more complete
results, because edge→PBS latency is ~5–7x lower.

---

## 7. Implementation Scope

### New

- `creative-opportunities.toml` — slot template config file
- `crates/trusted-server-core/src/creative_opportunities.rs` — config types, TOML parsing,
  URL pattern matching, slot-to-`AdSlot` conversion
- `build.rs` update — `include_str!()` for `creative-opportunities.toml`
- Request handler modification — match slots at request receipt, trigger orchestrator immediately,
  hold result for head injection
- `tsjs-gpt` integration update — `__tsAdInit` bootstrap replaces Prebid.js ad unit setup

### Modified

- `crates/trusted-server-core/src/integrations/prebid.rs` head injector — emit
  `window.__ts_ad_slots` from matched slots
- `crates/trusted-server-core/src/html_processor.rs` — inject `window.__ts_bids` once auction
  results are available, before `</head>`
- `trusted-server.toml` — add `creative_opportunities_path` config key pointing to the new file

### Unchanged

- `AuctionOrchestrator` — no internal changes; new call site only
- PBS stored request configuration — bidder params remain in PBS, keyed by slot ID
- GAM line item configuration — targeting key-values pass through unchanged

---

## 8. Edge Cases

**No slots match the URL** — auction is not fired. Head injection emits neither global. GPT
bootstrap detects empty `__ts_ad_slots` and skips initialization. Page loads normally with no
ad stack.

**Auction times out with partial results** — `__ts_bids` is populated with whatever bids arrived
before the deadline. Slots with no bid omitted. GPT fires without pre-set targeting for those slots;
GAM falls back to its own auction.

**Auction times out with zero results** — `__ts_bids` is an empty object `{}`. All slots fire
GAM without bid targeting. No revenue impact beyond the timeout scenario itself (same as today's
fallback).

**Origin is slow (NextJS 14, buffered)** — auction has more time; results more likely to be
complete. No change to streaming behavior.

**NextJS 16 streaming** — TS must flush `<head>` before `</head>` tag passes through. If auction
not yet complete, TS waits up to `auction_timeout_ms` from the config, then flushes. Content
streaming resumes immediately after `</head>` regardless of bid state.

**`creative-opportunities.toml` missing or malformed** — startup fails with a clear error.
No silent degradation.

---

## 9. Open Questions

1. **URL pattern coverage** — does `/20*/` cover all article paths, or are there
   non-date-prefixed article URLs? Publisher to confirm.
2. **PBS stored request setup** — slot IDs in `creative-opportunities.toml` must have
   corresponding stored requests configured in the publisher's PBS instance before this goes live.
3. **Homepage slot count** — the example shows slots 0 and 1. Are there slots 2–5 following
   the same pattern? Slot IDs and count to be confirmed with ad ops.
4. **Auction timeout for server-side trigger** — current `[integrations.prebid].timeout_ms`
   is 1,000ms. Recommend reducing to 500ms for server-side triggered auctions given the
   lower edge→PBS RTT. Separate config key or override on the new trigger path?
5. **`tsjs-gpt` bootstrap delivery** — the `__tsAdInit` script needs to fire after GPT.js
   loads. Confirm injection order with the existing GPT integration head injection.
