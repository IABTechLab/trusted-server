# SSAT: render the winning creative inline (no PBS Cache round trip)

**Date:** 2026-07-13
**Status:** Implemented — see "Implementation reconciliation" below for how the
shipped code diverged from the original design.
**Branch:** `feat/ssat-render-inline-creative`

## Implementation reconciliation (final)

The core design below is accurate; the shipped implementation refined it in four
ways. Where the sections that follow still say `rewrite_creative_html`, the
foreign-origin render context (see below) means the code uses
`rewrite_inline_creative_html`.

1. **Dedicated inline rewriter — `rewrite_inline_creative_html`.** The inline
   `adm` is rendered by the Prebid Universal Creative inside GAM's iframe
   (`f.srcdoc = d.ad`), a **foreign origin**. Root-relative `/first-party/…`
   URLs would resolve against GAM and 404, so the inline rewriter emits
   **absolute** first-party URLs and does **not** inject the `tsjs` bundle (its
   only job — safeguarding click URLs — is moot once URLs are absolute, and the
   bundle is pure weight in a creative iframe). `rewrite_creative_html` (root-
   relative, tsjs-injecting) remains the `/auction` same-origin path.
2. **Absolute URLs use the request origin, not `publisher.domain`.**
   `rewrite_inline_creative_html` takes a `base_origin` derived from the trusted
   request (`scheme://host`, host including any port). `publisher.domain` cannot
   carry a port and may differ from the subdomain serving the request; it is only
   the fallback when the request origin is unknown. Threaded through
   `write_bids_to_state`/`build_bid_map`; SPA page-bids derives it from
   `RequestInfo`.
3. **Render metadata in the bid map.** `build_bid_map` emits the winning
   creative's `w`/`h`; the bridge sizes the inline (and cache-fallback) render
   from those, falling back to the first configured slot format only when absent.
   `${AUCTION_PRICE}` is expanded from the exact winning CPM **before**
   sanitize/rewrite/sign, so the clearing price — not an encoded macro — is what
   gets signed into proxy/click URLs.
4. **Cache fallback decodes a structured bid.** `parseCachedBid` (replacing the
   adm-only `extractCachedAdm`) retains the cached creative's dimensions and
   price, so the fallback sizes correctly and expands `${AUCTION_PRICE}` from the
   cached price. Firing a cached win-notification URL is deferred pending a real
   PBS Cache payload to verify the field and dedup contract.

## Problem

On the server-side auction (SSAT / streaming path), when GAM picks the
trusted-server header-bid line item, the winning creative is fetched at render
time from PBS Cache:

```
https://<hb_cache_host><hb_cache_path>?uuid=<hb_adid>
```

This is an extra network round trip _after_ the GAM call, even though
trusted-server already holds the winning creative markup (`bid.creative`) from
the server-side auction it just ran. The client-side `/auction` flow never does
this — Prebid.js renders the winner from the copy it already has in the browser.

## Goal

Make SSAT render the winning creative **from the copy it already holds**, the
same way the client does — eliminating the render-time PBS Cache round trip —
while keeping GAM in the loop (the header bid still competes against GAM's own
demand via `hb_pb`).

Non-goal: bypassing GAM. SSAT winners must still compete in GAM; we only remove
the round trip that happens _after_ GAM has already picked the TS line item.

## Current flow (verified in code)

1. `build_bid_map` ([publisher.rs:1933]) writes `window.tsjs.bids[slot]` with
   `hb_pb`, `hb_bidder`, `hb_adid`, `hb_cache_host`, `hb_cache_path`. The raw
   `adm` (creative) and a verbose `debug_bid` blob are included **only** when the
   current `include_adm` param is set — today wired to
   `settings.debug.inject_adm_for_testing`.
2. `build_bids_script` ([publisher.rs:2018]) serializes the bid map and runs it
   through `html_escape_for_script` (escapes `<`/`>`/`&` and `U+2028/2029`),
   embedding it as `JSON.parse("…")`.
3. GAM's Prebid line item (matched by `hb_pb`) serves the Prebid Universal
   Creative (PUC), which `postMessage`s `"Prebid Request"`.
4. `installTsRenderBridge` ([gpt/index.ts:839]) intercepts and either:
   - serves `matchedBid.adm` **directly** when present (no round trip), then
     fires win/billing beacons, or
   - **fetches from PBS Cache** using `hb_cache_host`/`hb_cache_path` (the round
     trip we want to remove).
5. A separate consumer, `injectAdmIntoSlot` ([gpt/index.ts:599]), fires on
   `if (bid.adm)` and **replaces the GAM creative directly** — a GAM _bypass_.
   Its "testing only" status is a comment, not an actual gate.

## Design

### 1. Always include the render `adm`; keep `debug_bid` gated

`build_bid_map`:

- **Always** insert `adm` for a winner when present — there is no runtime reason
  to withhold it, so it is not parameterized. The creative is first run through
  the **same sanitize/rewrite boundary as the `/auction` path** (`auction::formats`):
  `creative::sanitize_creative_html` then `creative::rewrite_creative_html`.
  `sanitize_creative_html` also enforces the 1 MiB `MAX_CREATIVE_SIZE` cap,
  returning empty for oversized or unparseable markup — in which case `adm` is
  omitted and the bridge falls back to the PBS Cache coordinates. `build_bid_map`
  therefore takes `&Settings` (needed by `rewrite_creative_html`).
- Insert the verbose `debug_bid` blob **only** when the testing flag is set. The
  single `include_adm` param becomes `include_debug_bid`. (The `debug_bid` blob
  still carries the raw, un-sanitized creative for diagnostics; only the
  client-facing `adm` is sanitized.)

`hb_cache_host`/`hb_cache_path` remain inserted unconditionally.

### 2. Bridge renders local `adm`; cache is the fallback for an _absent_ `adm`

`installTsRenderBridge` already prefers `matchedBid.adm` and falls back to PBS
Cache. Once `adm` is present in production, the local render becomes the default
and the round trip disappears.

**Cache payload decode:** PBS Cache (`returnCreative=false`) returns the cached
bid as a **JSON object**, and the Prebid Universal Creative's own cache path
`JSON.parse`s it and renders `bidObject.adm` — not the raw response body. The
fallback therefore parses the cache response and extracts the string `adm`
(`extractCachedAdm`), with raw-markup compatibility for caches that return the
creative directly, and declines to render (no `"Prebid Response"`, no beacons)
when the payload has no usable `adm`. It does **not** forward a serialized bid
document to the PUC.

**Fallback scope (corrected):** the bridge posts the markup to the PUC and
returns; it receives **no render-success signal**. So the PBS Cache fallback
fires only when the inline `adm` is **absent or empty** — _not_ when `adm` is
present but fails to render. Render failures after `adm` is supplied are not
currently detectable and do not trigger fallback.

### 3. Gate the GAM-bypass on `bid.debug_bid` (no new global flag)

`injectAdmIntoSlot` must not fire in production merely because `adm` is now
present. Rather than introduce a global `window.tsjs` flag (which goes stale
across SPA navigations — an empty initial response would pin it `false`), gate
the bypass on the per-bid `debug_bid` field, which is already present **iff**
`inject_adm_for_testing` is on:

```ts
if (bid.adm && bid.debug_bid) {
  injectAdmIntoSlot(divId, bid.adm)
}
```

This works for both initial and SPA auction responses, needs no `TsjsApi`
change, and keeps production `adm` on the bridge-only (keep-GAM) path.

### 4. Security

Two layers, both provided directly by trusted-server:

1. **Creative-processing boundary (server-side).** The inline `adm` runs through
   the same `sanitize_creative_html` → `rewrite_creative_html` pass as the
   `/auction` path before it enters the bid map. Sanitization strips executable
   markup (`<script>`, `on*` handlers, `javascript:`/dangerous `data:` URIs) and
   enforces the 1 MiB `MAX_CREATIVE_SIZE` cap; rewriting proxies creative URLs to
   first-party endpoints. Pinned by a hostile-`adm` (script/handler/`javascript:`
   stripped) and an oversized-`adm` (omitted) regression test.
2. **Script-context escaping.** The (now sanitized) `adm` is part of the bid-map
   JSON that passes through `html_escape_for_script` + `JSON.parse("…")`, which
   neutralizes any residual `</script>` breakout and `U+2028/2029`. Pinned by a
   line-separator escaping test.

Frame isolation of the rendered creative is **not** guaranteed by TS on the
bridge path: `injectAdmIntoSlot` sets `sandbox=ADM_IFRAME_SANDBOX`, but the
bridge renderer hands `adm` to the PUC-provided `mkFrame`, which TS neither sets
nor verifies a sandbox on. Bridge isolation therefore depends on the Prebid
Universal Creative implementation, not on TS.

## Components changed

| Unit                                            | Change                                                                                                                                                                                                                                                                                     |
| ----------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `build_bid_map` (Rust)                          | Expand `${AUCTION_PRICE}`, then sanitize + `rewrite_inline_creative_html` `bid.creative` (1 MiB cap) before inserting `adm`; omit when rejected. Emit winning `w`/`h`. Takes `&Settings` + `request_origin`. Rename `include_adm` → `include_debug_bid`, gating only the `debug_bid` blob. |
| `creative.rs`                                   | `rewrite_inline_creative_html(&Settings, base_origin, markup)` (absolute URLs, no tsjs); `expand_auction_price_macro`.                                                                                                                                                                     |
| `build_bid_map` / `write_bids_to_state` callers | Thread `&Settings` + the request origin (`scheme://host`); pass `include_debug_bid = inject_adm_for_testing`.                                                                                                                                                                              |
| `gpt/index.ts` `installTsRenderBridge`          | Resolve the bid by requesting slot; size from `matchedBid.w`/`h`; decode PBS Cache JSON into a structured bid (`parseCachedBid`) preserving dims + price, expanding `${AUCTION_PRICE}`; decline when absent.                                                                               |
| `gpt/index.ts` `injectAdmIntoSlot` call site    | Gate on `bid.adm && bid.debug_bid`.                                                                                                                                                                                                                                                        |
| `core/types.ts` `AuctionBidData`                | Add `w`/`h` render dimensions.                                                                                                                                                                                                                                                             |
| bridge/`ad_init` tests (JS)                     | Rename "debug adm" → "inline/local adm"; realistic `returnCreative=false` cache payload + malformed/raw-markup coverage; slot/dimension/origin/macro cases.                                                                                                                                |

No `build_bids_script` change, no `window.tsjs` flag, no `TsjsApi` change.

## Data flow (after)

```
SSAT auction → winner (bid.creative held) → build_bid_map sanitizes+rewrites → inserts adm
  → build_bids_script (html_escape_for_script) → window.tsjs.bids
  → hb_pb targeting → GAM competes
      ├ GAM picks TS line item → PUC "Prebid Request"
      │     → bridge replies with inline adm  → RENDER (no round trip) + beacons
      │     → (adm absent) → PBS Cache fetch → decode JSON → extract adm → RENDER (fallback)
      └ GAM has higher demand → GAM serves its own creative
```

## Precondition

This changes only the render bridge's _data source_ — local `adm` vs a PBS Cache
fetch — **when GAM's Prebid line item already serves the PUC**. It does not
change GAM competition, nor whether the PUC fires. A publisher without Prebid
line items in GAM sees no behavioral change.

## Testing

- **Rust**: `build_bid_map` includes a sanitized `adm` for winners on every path;
  a hostile `adm` has its `<script>`/`on*`/`javascript:` stripped; an oversized
  (> 1 MiB) `adm` is omitted (bridge falls back to cache); `debug_bid` present
  only under the testing flag; `U+2028/2029` in `adm` is escaped so
  `build_bids_script` output stays inside the `<script>`.
- **JS (vitest)**: `injectAdmIntoSlot` fires only when `bid.debug_bid` is present;
  the PBS Cache fallback decodes a realistic `returnCreative=false` JSON payload
  and forwards its `adm`, renders a non-JSON body as raw markup, and declines a
  payload with no `adm`; existing bridge coverage (local-adm-without-cache-fetch,
  `nurl`/`burl` beacons, dedup) is retained with terminology updated to
  "inline/local adm".

## Risks / accepted costs

- **Page weight**: every SSAT navigation response now carries winners' creatives
  inline (~3–8 KB each), making that response larger and per-auction
  (uncacheable). Accepted as the cost of client-parity rendering.
- **Renderability**: some creatives may assume a PBS Cache/PUC context. Because
  render failure after the bridge supplies `adm` is **not detectable**, such a
  creative will fail to render rather than fall back to cache. Mitigation is
  operational (validate creatives) — automatic fallback on render failure is out
  of scope.

## Out of scope

- A tunable inline-size threshold below the 1 MiB sanitize cap (the cap already
  omits oversized creatives and defers them to the cache path).
- Detecting render failure to trigger cache fallback.
- Bypassing GAM for SSAT winners.
- Changing the client-side `/auction` render path.
