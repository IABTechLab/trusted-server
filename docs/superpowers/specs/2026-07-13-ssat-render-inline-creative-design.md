# SSAT: render the winning creative inline (no PBS Cache round trip)

**Date:** 2026-07-13
**Status:** Design revised after review, pending final approval
**Branch:** `feat/ssat-render-inline-creative`

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

- **Always** insert `adm` (from `bid.creative`) for a winner when present —
  there is no runtime reason to withhold it, so it is not parameterized.
- Insert the verbose `debug_bid` blob **only** when the testing flag is set. The
  single `include_adm` param becomes `include_debug_bid`.

`hb_cache_host`/`hb_cache_path` remain inserted unconditionally.

### 2. Bridge renders local `adm`; cache is the fallback for an _absent_ `adm`

`installTsRenderBridge` already prefers `matchedBid.adm` and falls back to PBS
Cache. Once `adm` is present in production, the local render becomes the default
and the round trip disappears.

**Fallback scope (corrected):** the bridge posts the markup to the PUC and
returns; it receives **no render-success signal**. So the PBS Cache fallback
fires only when `adm` is **absent or empty** — _not_ when `adm` is present but
fails to render. Render failures after `adm` is supplied are not currently
detectable and do not trigger fallback.

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

No new escaping code. The `adm` string is part of the bid-map JSON that already
passes through `html_escape_for_script` + `JSON.parse("…")`, which neutralizes
`</script>` breakout and `U+2028/2029`. **This is the guarantee trusted-server
directly provides**, pinned by a hostile-`adm` regression test.

Frame isolation of the rendered creative is **not** guaranteed by TS on the
bridge path: `injectAdmIntoSlot` sets `sandbox=ADM_IFRAME_SANDBOX`, but the
bridge renderer hands `adm` to the PUC-provided `mkFrame`, which TS neither sets
nor verifies a sandbox on. Bridge isolation therefore depends on the Prebid
Universal Creative implementation, not on TS.

## Components changed

| Unit                                         | Change                                                                                                                           |
| -------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------- |
| `build_bid_map` (Rust)                       | Always insert `adm` when `bid.creative` is `Some`. Rename `include_adm` → `include_debug_bid`, gating only the `debug_bid` blob. |
| `build_bid_map` callers                      | Pass `include_debug_bid = inject_adm_for_testing`.                                                                               |
| `gpt/index.ts` `injectAdmIntoSlot` call site | Gate on `bid.adm && bid.debug_bid`.                                                                                              |
| bridge/`ad_init` tests (JS)                  | Rename "debug adm" → "inline/local adm"; confirm existing coverage.                                                              |

No `build_bids_script` change, no `window.tsjs` flag, no `TsjsApi` change.

## Data flow (after)

```
SSAT auction → winner (bid.creative held) → build_bid_map inserts adm
  → build_bids_script (html_escape_for_script) → window.tsjs.bids
  → hb_pb targeting → GAM competes
      ├ GAM picks TS line item → PUC "Prebid Request"
      │     → bridge replies with local adm  → RENDER (no round trip) + beacons
      │     → (adm absent) → PBS Cache fetch  → RENDER (fallback)
      └ GAM has higher demand → GAM serves its own creative
```

## Precondition

This changes only the render bridge's _data source_ — local `adm` vs a PBS Cache
fetch — **when GAM's Prebid line item already serves the PUC**. It does not
change GAM competition, nor whether the PUC fires. A publisher without Prebid
line items in GAM sees no behavioral change.

## Testing

- **Rust**: `build_bid_map` includes `adm` for winners on every path; `debug_bid`
  present only under the testing flag; a hostile `</script>` / `U+2028/2029`
  `adm` is escaped so `build_bids_script` output stays inside the `<script>`.
- **JS (vitest)**: `injectAdmIntoSlot` fires only when `bid.debug_bid` is present;
  existing `ad_init.test.ts` bridge coverage (local-adm-without-cache-fetch,
  cache-fetch-when-adm-absent, `nurl`/`burl` beacons, dedup) is retained with
  terminology updated to "inline/local adm" — no duplicate tests.

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

- Size-capping / conditional inline vs cache.
- Detecting render failure to trigger cache fallback.
- Bypassing GAM for SSAT winners.
- Changing the client-side `/auction` render path.
