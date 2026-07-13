# SSAT Inline Creative Rendering — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Render SSAT-winning creatives from the copy trusted-server already holds (no render-time PBS Cache round trip), while keeping GAM in the loop.

**Architecture:** Always include the winning `adm` in `window.tsjs.bids` so the existing `installTsRenderBridge` serves it locally when GAM's Prebid Universal Creative fires. Keep `hb_cache_*` as a fallback. Split the render `adm` from the debug-only `debug_bid` blob, and gate the GAM-bypass path (`injectAdmIntoSlot`) behind an explicit `window.tsjs.injectAdmForTesting` flag so production keeps GAM.

**Tech Stack:** Rust (`trusted-server-core`, wasm32-wasip1 via Viceroy), TypeScript (`trusted-server-js`, vitest).

**Spec:** `docs/superpowers/specs/2026-07-13-ssat-render-inline-creative-design.md`

---

## File Structure

| File | Responsibility | Change |
| --- | --- | --- |
| `crates/trusted-server-core/src/publisher.rs` | Build `window.tsjs.bids` + injection | Split `include_adm`→`render_adm`+`debug_bid`; always insert `adm`; emit `injectAdmForTesting` flag |
| `crates/trusted-server-js/lib/src/integrations/gpt/index.ts` | Client render paths | Gate `injectAdmIntoSlot` on the flag |
| `crates/trusted-server-js/lib/test/integrations/gpt/*.test.ts` | JS tests | Bypass-off-without-flag; bridge-serves-local-adm |

---

## Task 1: Split `build_bid_map` flags and always include render `adm`

**Files:**
- Modify: `crates/trusted-server-core/src/publisher.rs` (`build_bid_map` ~1933)
- Test: same file, `#[cfg(test)] mod tests`

- [ ] **Step 1: Write failing test** — production path includes `adm`, excludes `debug_bid`.

```rust
#[test]
fn build_bid_map_includes_adm_for_render_without_debug_bid() {
    let mut winning = std::collections::HashMap::new();
    let mut bid = /* build a Bid with creative Some("<div>x</div>"), price Some(1.0) */;
    winning.insert("ad-header-0".to_string(), bid);
    // render_adm = true (production), debug_bid = false
    let map = build_bid_map(&winning, PriceGranularity::Dense, true, false);
    let slot = map["ad-header-0"].as_object().expect("slot obj");
    assert_eq!(slot["adm"], serde_json::json!("<div>x</div>"), "render adm present");
    assert!(!slot.contains_key("debug_bid"), "debug_bid absent in production");
}
```

- [ ] **Step 2: Run — expect FAIL** (signature mismatch: `build_bid_map` takes 3 args).

Run: `cargo test-fastly build_bid_map_includes_adm_for_render_without_debug_bid`

- [ ] **Step 3: Implement** — change signature and body:

```rust
pub(crate) fn build_bid_map(
    winning_bids: &std::collections::HashMap<String, Bid>,
    granularity: crate::price_bucket::PriceGranularity,
    render_adm: bool,
    debug_bid: bool,
) -> serde_json::Map<String, serde_json::Value> {
    // ... unchanged hb_* / cache / nurl / burl inserts ...
    // Replace the single `if include_adm { adm + debug_bid }` block with:
    if render_adm {
        if let Some(ref adm) = bid.creative {
            obj.insert("adm".to_string(), serde_json::Value::String(adm.clone()));
        }
    }
    if debug_bid {
        obj.insert("debug_bid".to_string(), serde_json::json!({ /* unchanged blob */ }));
    }
    // ...
}
```

- [ ] **Step 4: Run — expect PASS.**

- [ ] **Step 5: Update the 3 call sites** so the crate compiles:
  - `publisher.rs:~853`, `~1241`, `~2390`: pass `render_adm = true, debug_bid = settings.debug.inject_adm_for_testing`.
  - The pure-test caller at `~4043`: `build_bid_map(&winning_bids, PriceGranularity::Dense, false, false)`.

- [ ] **Step 6: Run** `cargo check-fastly` — expect clean.

- [ ] **Step 7: Commit** — `git commit -m "Split build_bid_map render adm from debug_bid blob"`

---

## Task 2: Emit the `injectAdmForTesting` flag with the bids script

**Files:**
- Modify: `crates/trusted-server-core/src/publisher.rs` (`build_bids_script` ~2018 + its caller ~853)

- [ ] **Step 1: Write failing test** — script sets the flag.

```rust
#[test]
fn bids_script_emits_inject_adm_for_testing_flag() {
    let script = build_bids_script(&serde_json::Map::new(), true);
    assert!(script.contains("injectAdmForTesting=true"), "flag emitted: {script}");
    let off = build_bids_script(&serde_json::Map::new(), false);
    assert!(off.contains("injectAdmForTesting=false"), "flag false: {off}");
}
```

- [ ] **Step 2: Run — expect FAIL** (arity).

- [ ] **Step 3: Implement** — add a bool param and emit the flag on `window.tsjs`:

```rust
pub(crate) fn build_bids_script(
    bid_map: &serde_json::Map<String, serde_json::Value>,
    inject_adm_for_testing: bool,
) -> String {
    let json = serde_json::to_string(bid_map).expect("serialize bid map");
    let escaped = html_escape_for_script(&json);
    format!(
        "<script>(window.tsjs=window.tsjs||{{}}).injectAdmForTesting={inject_adm_for_testing};\
         (window.tsjs=window.tsjs||{{}}).bids=JSON.parse(\"{escaped}\");\
         (function(){{var f=window.tsjs.adInit;if(typeof f===\"function\")f();}})();</script>"
    )
}
```

- [ ] **Step 4: Update the caller** at `~853/854` to pass `settings.debug.inject_adm_for_testing`; update the empty-bids helper at `~2033` and any test expectations that pin the old script string.

- [ ] **Step 5: Run — expect PASS.** `cargo test-fastly bids_script_emits_inject_adm_for_testing_flag`

- [ ] **Step 6: Commit** — `git commit -m "Emit injectAdmForTesting flag on window.tsjs with bids"`

---

## Task 3: Escaping regression — hostile `adm` cannot break out of `<script>`

**Files:**
- Modify: `crates/trusted-server-core/src/publisher.rs` tests

- [ ] **Step 1: Write test** — a `</script>` + `U+2028` adm is neutralized in the emitted script.

```rust
#[test]
fn build_bids_script_escapes_hostile_adm() {
    let mut winning = std::collections::HashMap::new();
    let mut bid = /* Bid, price Some(1.0), creative Some("</script><script>alert(1)</script>\u{2028}") */;
    winning.insert("s".to_string(), bid);
    let map = build_bid_map(&winning, PriceGranularity::Dense, true, false);
    let script = build_bids_script(&map, false);
    // Raw </script> must not survive; U+2028 must be unicode-escaped.
    assert!(!script.contains("</script><script>"), "no raw breakout: {script}");
    assert!(!script.contains('\u{2028}'), "U+2028 escaped");
}
```

- [ ] **Step 2: Run — expect PASS** (existing `html_escape_for_script` already handles it — this pins the guarantee).

- [ ] **Step 3: Commit** — `git commit -m "Pin escaping guarantee for inline adm"`

---

## Task 4: Gate the GAM-bypass (`injectAdmIntoSlot`) behind the flag

**Files:**
- Modify: `crates/trusted-server-js/lib/src/integrations/gpt/index.ts:~599`
- Test: `crates/trusted-server-js/lib/test/integrations/gpt/` (add/extend a test file)

- [ ] **Step 1: Write failing vitest** — bypass does NOT fire when the flag is off, even with `bid.adm`.

```ts
// window.tsjs = { bids: { 'ad-header-0': { adm: '<div>x</div>' } }, injectAdmForTesting: false }
// simulate slotRenderEnded for ad-header-0
// assert injectAdmIntoSlot was NOT called (spy) — the bridge handles render instead
```

- [ ] **Step 2: Run — expect FAIL.** `cd crates/trusted-server-js/lib && npx vitest run <file>`

- [ ] **Step 3: Implement** — change the guard:

```ts
if (bid.adm && window.tsjs?.injectAdmForTesting) {
  injectAdmIntoSlot(divId, bid.adm);
}
```

- [ ] **Step 4: Add the companion test** — with `injectAdmForTesting: true`, `injectAdmIntoSlot` IS called.

- [ ] **Step 5: Run — expect PASS.**

- [ ] **Step 6: Commit** — `git commit -m "Gate GAM-bypass adm injection behind injectAdmForTesting flag"`

---

## Task 5: Bridge serves local `adm` (no cache fetch) — confirm/extend

**Files:**
- Test: `crates/trusted-server-js/lib/test/integrations/gpt/` (bridge tests)

- [ ] **Step 1:** Check for an existing `installTsRenderBridge` test. If a "serves local adm without cache fetch" case is missing, add: given a bid with `adm` and `hb_cache_*`, a `"Prebid Request"` for its `hb_adid` is answered from `adm` and **no `fetch`** occurs.
- [ ] **Step 2:** Add the fallback case: bid with `hb_cache_*` but no `adm` → bridge fetches from cache.
- [ ] **Step 3: Run — expect PASS.**
- [ ] **Step 4: Commit** — `git commit -m "Cover bridge local-adm render and cache fallback"`

---

## Task 6: Full verification

- [ ] **Step 1:** `cargo fmt --all -- --check`
- [ ] **Step 2:** `cargo test-fastly && cargo test-axum && cargo test-cloudflare && cargo test-spin`
- [ ] **Step 3:** `cargo clippy-fastly` (and axum/cloudflare/spin per CI gate)
- [ ] **Step 4:** `cd crates/trusted-server-js/lib && npx vitest run && npm run format && node build-all.mjs`
- [ ] **Step 5:** Manual: with `[debug].auction_html_comment` off and inline adm on, load a nav page; confirm the winning creative renders **without** a request to `hb_cache_host` (Network tab) and GAM still received `hb_pb`.
- [ ] **Step 6: Commit** any format fixes.

---

## Notes
- Do NOT remove `hb_cache_host`/`hb_cache_path` — they are the fallback.
- Do NOT ship the `debug_bid` blob in production (Task 1 keeps it behind the flag).
- Page-weight cost (inline creatives, uncacheable response) is accepted per spec; size-capping is out of scope.
