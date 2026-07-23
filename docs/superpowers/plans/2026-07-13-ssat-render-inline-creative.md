# SSAT Inline Creative Rendering — Implementation Plan

> **Status: Superseded — completed with revisions.** The feature shipped on
> `feat/ssat-render-inline-creative`. The implementation followed this plan's
> core (always include the winning `adm`; bridge renders it locally; cache is the
> fallback; gate the GAM-bypass on `debug_bid`) but extended it beyond what the
> task list below captures:
>
> - a dedicated foreign-origin rewriter, `rewrite_inline_creative_html` (absolute
>   URLs, no tsjs injection), replacing the `rewrite_creative_html` calls named
>   below;
> - inline URLs built from the **request origin** (`scheme://host:port`) threaded
>   through `write_bids_to_state`/`build_bid_map`, not `publisher.domain`;
> - winning creative dimensions (`w`/`h`) emitted in the bid map and used to size
>   the render;
> - `${AUCTION_PRICE}` expanded from the winning CPM before sanitize/rewrite/sign;
> - a structured PBS Cache decoder (`parseCachedBid`) preserving cached dimensions
>   and price.
>
> The authoritative final description is the design's "Implementation
> reconciliation" section: `docs/superpowers/specs/2026-07-13-ssat-render-inline-creative-design.md`.
> The unchecked task list below is retained as historical record; it is **not** an
> accurate map of the shipped code.

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Render SSAT-winning creatives from the copy trusted-server already holds (no render-time PBS Cache round trip), while keeping GAM in the loop.

**Architecture:** Always include the winning `adm` in `window.tsjs.bids` so the existing `installTsRenderBridge` serves it locally when GAM's Prebid Universal Creative fires. Keep `hb_cache_*` as the fallback for an _absent_ `adm`. Keep the verbose `debug_bid` blob behind the testing flag, and gate the GAM-bypass path (`injectAdmIntoSlot`) on the per-bid `debug_bid` field — no global flag, no `TsjsApi` change.

**Tech Stack:** Rust (`trusted-server-core`, wasm32-wasip1 via Viceroy), TypeScript (`trusted-server-js`, vitest).

**Spec:** `docs/superpowers/specs/2026-07-13-ssat-render-inline-creative-design.md`

---

## File Structure

| File                                                                 | Responsibility           | Change                                                                              |
| -------------------------------------------------------------------- | ------------------------ | ----------------------------------------------------------------------------------- |
| `crates/trusted-server-core/src/publisher.rs`                        | Build `window.tsjs.bids` | Always insert `adm`; rename `include_adm`→`include_debug_bid` (gates only the blob) |
| `crates/trusted-server-js/lib/src/integrations/gpt/index.ts`         | Client render paths      | Gate `injectAdmIntoSlot` on `bid.adm && bid.debug_bid`                              |
| `crates/trusted-server-js/lib/test/integrations/gpt/ad_init.test.ts` | JS tests                 | Rename "debug adm"→"inline/local adm"; add bypass-gate test                         |

---

## Task 1: `build_bid_map` — always include `adm`, gate only `debug_bid`

**Files:**

- Modify: `crates/trusted-server-core/src/publisher.rs` (`build_bid_map` ~1933; `write_bids_to_state` ~846; callers)
- Test: same file, `#[cfg(test)] mod tests`

- [ ] **Step 1: Write failing test** — `adm` present for a winner regardless of the debug flag; `debug_bid` only when the flag is set.

```rust
#[test]
fn build_bid_map_always_includes_adm_and_gates_debug_bid() {
    let mut winning = std::collections::HashMap::new();
    // Reuse the existing `make_bid(slot_id, price, bidder, ad_id, nurl, burl)`
    // helper (~:3969); it sets `creative: None`, so set the creative here.
    let mut bid = make_bid("ad-header-0", 1.50, "kargo", "abc123", "https://ssp/win", "https://ssp/bill");
    bid.creative = Some("<div>x</div>".to_string());
    winning.insert("ad-header-0".to_string(), bid);
    // include_debug_bid = false (production)
    let map = build_bid_map(&winning, PriceGranularity::Dense, false);
    let slot = map["ad-header-0"]
        .as_object()
        .expect("should contain an object for the winning slot");
    assert_eq!(
        slot["adm"],
        serde_json::json!("<div>x</div>"),
        "should include creative markup for local rendering"
    );
    assert!(
        !slot.contains_key("debug_bid"),
        "should omit the debug_bid blob when the testing flag is off"
    );
}
```

- [ ] **Step 2: Run — expect FAIL** (signature is 3-arg with `include_adm` semantics; adm currently gated off).

Run: `cargo test-fastly build_bid_map_always_includes_adm_and_gates_debug_bid`

- [ ] **Step 3: Implement** — rename the param and split the gating:

```rust
pub(crate) fn build_bid_map(
    winning_bids: &std::collections::HashMap<String, Bid>,
    granularity: crate::price_bucket::PriceGranularity,
    include_debug_bid: bool,
) -> serde_json::Map<String, serde_json::Value> {
    // ... unchanged hb_* / cache / nurl / burl inserts ...
    // Always include the creative for local rendering.
    if let Some(ref adm) = bid.creative {
        obj.insert("adm".to_string(), serde_json::Value::String(adm.clone()));
    }
    // Verbose debug blob only under the testing flag.
    if include_debug_bid {
        obj.insert("debug_bid".to_string(), serde_json::json!({ /* unchanged blob */ }));
    }
    // ...
}
```

- [ ] **Step 4: Run — expect PASS.**

- [ ] **Step 5: Update call sites** so the crate compiles (real call graph):
  - `write_bids_to_state` (`~846`): rename its `inject_adm: bool` param to `include_debug_bid`; call `build_bid_map(winning_bids, price_granularity, include_debug_bid)` (`~853`). `build_bids_script` is **unchanged**.
  - Callers `~695` / `~1241`: no change (already pass `settings.debug.inject_adm_for_testing`).
  - `handle_page_bids` (`~2390`): `build_bid_map(&winning_bids, co_config.price_granularity, settings.debug.inject_adm_for_testing)`.
  - Pure-test caller (`~4043`): `build_bid_map(&winning_bids, PriceGranularity::Dense, false)`.

- [ ] **Step 6: Update any test that asserted "no adm" on the production path** — production now always carries `adm`; adjust expectations to the new behavior.

- [ ] **Step 7: Run** `cargo check-fastly` — expect clean.

- [ ] **Step 8: Commit** — `git commit -m "Always include adm in bid map; gate only debug_bid blob"`

---

## Task 2: Escaping regression — hostile `adm` cannot break out of `<script>`

**Files:**

- Modify: `crates/trusted-server-core/src/publisher.rs` tests

- [ ] **Step 1: Write test** — a `</script>` + `U+2028` adm is neutralized in the emitted script.

```rust
#[test]
fn build_bids_script_escapes_hostile_adm() {
    let mut winning = std::collections::HashMap::new();
    let mut bid = make_bid("s", 1.50, "kargo", "abc123", "https://ssp/win", "https://ssp/bill");
    // Both line/paragraph separators — the spec promises escaping for each.
    bid.creative = Some("</script><script>alert(1)</script>\u{2028}\u{2029}".to_string());
    winning.insert("s".to_string(), bid);
    let map = build_bid_map(&winning, PriceGranularity::Dense, false);
    let script = build_bids_script(&map);
    assert!(
        !script.contains("</script><script>"),
        "should not let a hostile adm break out of the script context"
    );
    assert!(
        !script.contains('\u{2028}') && !script.contains('\u{2029}'),
        "should unicode-escape both U+2028 and U+2029 in the adm"
    );
}
```

- [ ] **Step 2: Run — expect PASS** (existing `html_escape_for_script` already handles it — this pins the guarantee).

Run: `cargo test-fastly build_bids_script_escapes_hostile_adm`

- [ ] **Step 3: Commit** — `git commit -m "Pin script-context escaping guarantee for inline adm"`

---

## Task 3: Gate the GAM-bypass (`injectAdmIntoSlot`) on `bid.debug_bid`

**Files:**

- Modify: `crates/trusted-server-js/lib/src/integrations/gpt/index.ts:~599`
- Test: `crates/trusted-server-js/lib/test/integrations/gpt/ad_init.test.ts`

- [ ] **Step 1: Write failing vitest — observable behavior (not a spy).** `injectAdmIntoSlot` is module-private, so assert its _effect_ on the DOM:

```ts
// Setup:
//   bids['ad-header-0'] = { adm: '<iframe src="https://cdn.example/creative.html"></iframe>' }  // NO debug_bid
//   place an existing GAM iframe (src="about:blank") in the slot div
//   capture the slotRenderEnded listener, fire it for 'ad-header-0'
// Assert (production): the GAM iframe src stays 'about:blank'
//   — the bypass did not fire; the render bridge handles it.
```

- [ ] **Step 2: Run — expect FAIL.** `cd crates/trusted-server-js/lib && npx vitest run ad_init`

- [ ] **Step 3: Implement** — change the guard (`ts` is the local `window.tsjs`):

```ts
// Direct GAM replacement is a testing-only bypass. `debug_bid` is present only
// when inject_adm_for_testing is on, so it doubles as the per-bid gate — no
// global flag needed, and it is correct across SPA auction responses.
if (bid.adm && bid.debug_bid) {
  injectAdmIntoSlot(divId, bid.adm)
}
```

- [ ] **Step 4: Add companion test (testing mode)** — same setup but with `bid.debug_bid` present. Fire `slotRenderEnded` → assert the slot iframe's `src` **changes to** the creative URL (`https://cdn.example/creative.html`), proving `injectAdmIntoSlot` ran.

- [ ] **Step 5: Run — expect PASS.**

- [ ] **Step 6: Commit** — `git commit -m "Gate GAM-bypass adm injection on per-bid debug_bid"`

---

## Task 4: Reconcile existing bridge tests (no duplicates)

**Files:**

- Modify: `crates/trusted-server-js/lib/test/integrations/gpt/ad_init.test.ts`

`ad_init.test.ts` already covers: PBS Cache fetch when `adm` absent; local `adm`
response without a cache fetch; `nurl`/`burl` on the local path; cache-fetch
concurrency + beacon dedup. Do **not** duplicate them.

- [ ] **Step 1:** Rename "debug adm" terminology → "inline/local adm" in the existing bridge tests.
- [ ] **Step 2:** Confirm the local-`adm` test fixtures carry **both** `hb_cache_*` coordinates **and** inline `adm`, proving the bridge prefers local `adm` even when cache coords are present (the production shape).
- [ ] **Step 3: Run — expect PASS.** `cd crates/trusted-server-js/lib && npx vitest run ad_init`
- [ ] **Step 4: Commit** — `git commit -m "Rename debug-adm test terminology to inline/local adm"`

---

## Task 5: Full verification

- [ ] **Step 1:** `cargo fmt --all -- --check`
- [ ] **Step 2:** `cargo test-fastly && cargo test-axum && cargo test-cloudflare && cargo test-spin`
- [ ] **Step 3:** Clippy — exact CI-gate commands:
  ```
  cargo clippy-fastly
  cargo clippy-axum
  cargo clippy-cloudflare
  cargo clippy-cloudflare-wasm
  cargo clippy-spin-native
  cargo clippy-spin-wasm
  ```
- [ ] **Step 4:** `cd crates/trusted-server-js/lib && npx vitest run && npm run format && node build-all.mjs`
- [ ] **Step 5:** Docs format (these spec/plan docs changed): `cd docs && npm run format`
- [ ] **Step 6:** Manual: with `[debug].auction_html_comment` off, load a nav page; confirm the winning creative renders **without** a request to `hb_cache_host` (Network tab) and GAM still received `hb_pb`.
- [ ] **Step 7: Commit** any format fixes.

---

## Notes

- Do NOT remove `hb_cache_host`/`hb_cache_path` — they are the fallback for an **absent** `adm`. Render failure _after_ `adm` is supplied is not detectable and does not fall back (spec Risks).
- Do NOT ship the `debug_bid` blob in production (Task 1 keeps it behind the flag).
- No global `window.tsjs` flag, no `TsjsApi` change — the bypass gate is the per-bid `debug_bid`.
- Page-weight cost (inline creatives, uncacheable response) accepted per spec; size-capping out of scope.
- **Precondition:** only changes the bridge's data source when GAM's Prebid line item already serves the PUC — no change to GAM competition or whether the PUC fires.
