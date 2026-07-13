# SSAT: render the winning creative inline (no PBS Cache round trip)

**Date:** 2026-07-13
**Status:** Design approved, pending spec review
**Branch:** `feat/ssat-render-inline-creative`

## Problem

On the server-side auction (SSAT / streaming path), when GAM picks the
trusted-server header-bid line item, the winning creative is fetched at render
time from PBS Cache:

```
https://<hb_cache_host><hb_cache_path>?uuid=<hb_adid>
```

This is an extra network round trip *after* the GAM call, even though
trusted-server already holds the winning creative markup (`bid.creative`) from
the server-side auction it just ran. The client-side `/auction` flow never does
this — Prebid.js renders the winner from the copy it already has in the browser.

## Goal

Make SSAT render the winning creative **from the copy it already holds**, the
same way the client does — eliminating the render-time PBS Cache round trip —
while keeping GAM in the loop (the header bid still competes against GAM's own
demand via `hb_pb`).

Non-goal: bypassing GAM. SSAT winners must still compete in GAM; we only remove
the round trip that happens *after* GAM has already picked the TS line item.

## Current flow (verified in code)

1. `build_bid_map` ([publisher.rs:1933]) writes `window.tsjs.bids[slot]` with
   `hb_pb`, `hb_bidder`, `hb_adid`, `hb_cache_host`, `hb_cache_path`. The raw
   `adm` (creative) is included **only** when `include_adm` is set — today wired
   to `settings.debug.inject_adm_for_testing`.
2. `build_bids_script` ([publisher.rs:2018]) serializes the bid map and runs it
   through `html_escape_for_script` (escapes `<`/`>`/`&` and `U+2028/2029`),
   embedding it as `JSON.parse("…")`.
3. GAM's Prebid line item (matched by `hb_pb`) serves the Prebid Universal
   Creative, which `postMessage`s `"Prebid Request"`.
4. `installTsRenderBridge` ([gpt/index.ts:839]) intercepts and either:
   - serves `matchedBid.adm` **directly** when present (no round trip), or
   - **fetches from PBS Cache** using `hb_cache_host`/`hb_cache_path` (the round
     trip we want to remove).

A separate consumer, `injectAdmIntoSlot` ([gpt/index.ts:599]), fires on
`if (bid.adm)` and **replaces the GAM creative directly** — a GAM *bypass*. Its
"testing only" status is a comment, not an actual gate.

## Design

### 1. Always include the render `adm` for SSAT winners

Split the single `include_adm` parameter, which currently bundles two unrelated
things, into two independent signals:

- `render_adm` (production, **always on** for SSAT winners) — inserts the `adm`
  field so the render bridge can serve it locally.
- `debug_bid` (testing only, gated on `inject_adm_for_testing`) — the verbose
  `debug_bid` blob at [publisher.rs:1987] stays behind the testing flag and is
  **not** shipped in production.

`hb_cache_host`/`hb_cache_path` remain inserted unconditionally, so the cache
fetch stays available as a fallback.

### 2. Bridge renders local `adm`; cache is the fallback

No change to `installTsRenderBridge` logic is required: it already prefers
`matchedBid.adm` and falls back to PBS Cache. Once `adm` is present in
production, the local render becomes the default and the round trip disappears.
If `adm` is ever absent or unrenderable, the existing cache path still runs —
zero-risk regression.

### 3. Decouple the GAM-bypass path

`injectAdmIntoSlot` must **not** fire in production just because `adm` is now
present. Gate it behind an explicit injected config flag (e.g.
`injectAdmForTesting`) read by the JS, so:

- production inline-`adm` feeds **only** the render bridge → GAM stays in the
  loop, and
- the direct GAM-replace path remains available for testing behind the flag.

The flag is surfaced to the JS the same way other tsjs config is injected.

### 4. Security

No new escaping code. The `adm` string is part of the bid-map JSON that already
passes through `html_escape_for_script` + `JSON.parse("…")`, which neutralizes
`</script>` breakout and `U+2028/2029`. Requirement: a regression test proving a
hostile `adm` containing `</script>` (and `U+2028/2029`) cannot break out of the
injected `<script>` context. The render itself stays sandboxed in an iframe
(`srcdoc`/`src`) inside `injectAdmIntoSlot`/the bridge renderer.

## Components changed

| Unit | Change |
| --- | --- |
| `build_bid_map` (Rust) | Split `include_adm` → `render_adm` (always) + `debug_bid` (testing). Always insert `adm` for winners. |
| `build_bid_map` callers | Pass `render_adm = true`; `debug_bid = inject_adm_for_testing`. |
| tsjs config injection (Rust→JS) | Surface `injectAdmForTesting` flag. |
| `gpt/index.ts` `injectAdmIntoSlot` call site | Gate on the injected `injectAdmForTesting` flag, not bare `bid.adm`. |

## Data flow (after)

```
SSAT auction → winner (bid.creative held) → build_bid_map inserts adm
  → build_bids_script (html_escape_for_script) → window.tsjs.bids
  → hb_pb targeting → GAM competes
      ├ GAM picks TS line item → PUC "Prebid Request"
      │     → bridge replies with local adm  → RENDER (no round trip)
      │     → (adm absent) → PBS Cache fetch  → RENDER (fallback)
      └ GAM has higher demand → GAM serves its own creative
```

## Testing

- **Rust**: `build_bid_map` includes `adm` for winners on the production path;
  `debug_bid` present only under the testing flag; a hostile `</script>` /
  `U+2028/2029` `adm` is escaped so `build_bids_script` output stays inside the
  `<script>`.
- **JS (vitest)**: bridge serves local `adm` without a cache fetch when present;
  falls back to cache when `adm` absent; `injectAdmIntoSlot` does **not** fire
  without the `injectAdmForTesting` flag.

## Precondition

This changes only the render bridge's *data source* — local `adm` vs a PBS Cache
fetch — **when GAM's Prebid line item already serves the Prebid Universal
Creative**. It does not change GAM competition, nor whether the PUC fires. A
publisher without Prebid line items in GAM sees no behavioral change (same as
the current cache path).

## Risks / accepted costs

- **Page weight**: every SSAT navigation response now carries winners' creatives
  inline (~3–8 KB each), making that response larger and per-auction
  (uncacheable). Accepted as the cost of client-parity rendering. A future
  size-cap (inline small, cache large) is out of scope here.
- **Renderability**: some creatives may assume a PBS Cache/PUC context. The cache
  fallback covers any that don't render inline.

## Out of scope

- Size-capping / conditional inline vs cache.
- Bypassing GAM for SSAT winners.
- Changing the client-side `/auction` render path.
