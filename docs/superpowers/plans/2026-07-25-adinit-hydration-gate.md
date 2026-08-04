# adInit Hydration-Chunk Gate Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the initial `window.tsjs.adInit()` fire as soon as the Next.js App Router hydration chunks have executed (a few seconds) instead of on `window.load` (~52s on heavy publishers), without reintroducing the React #418 hydration mismatch.

**Architecture:** One Rust function, `build_bids_script` in `crates/trusted-server-core/src/publisher.rs`, emits an inline `<script>` that defers `adInit`. This plan replaces its `window.load` gate (from PR #945) with a hybrid gate: wait for the async `/_next/static/chunks/` scripts to finish (via `PerformanceResourceTiming` + `load`/`error`), then a double `requestAnimationFrame`, then `adInit`; keep `window.load` as an unconditional can't-hang fallback and as the unchanged path for non-Next publishers. No new config, no new files.

**Tech Stack:** Rust (edge core, `wasm32-wasip1` via Viceroy for tests), an inline browser JS string emitted from a Rust `format!`.

**Spec:** `docs/superpowers/specs/2026-07-24-adinit-hydration-gate-design.md`

> **UPDATE (post-live-test): chunk-await was replaced by a `__next_f` runtime-signal gate.**
> The chunk-await mechanism in Tasks 1–2 below was implemented and deployed, then failed on a live
> publisher — it never fired early and always fell back to `window.load` (chunk completion is not
> reliably observable: already-fired `load` events, evicted resource-timing entries, phantom prefetch
> tags). The gate now polls `window.__next_f.push` being replaced by the RSC runtime (~9s vs ~40s),
> then double-rAF, with `window.load` fallback. Tests assert `__next_f` / `setInterval` /
> `clearInterval` instead of the chunk selectors. See the spec's "Update" section. Safety is pending
> the #418 A/B, which is blocked by a separate rc/july bug (`elementId.startsWith`, PR #966, since
> reverted on #966 — adopt that revert in the deploy). Tasks below are kept for the record.
>
> **The gate also moved out of Rust.** The "Architecture" and "File Structure" sections below name
> `crates/trusted-server-core/src/publisher.rs::build_bids_script` as the only production change;
> that is no longer where the gate lives. `build_bids_script` now only calls
> `window.tsjs.scheduleInitialAdInit()`, and the gate itself is
> `installScheduleInitialAdInit` in `crates/trusted-server-js/lib/src/integrations/gpt/index.ts`,
> covered by `crates/trusted-server-js/lib/test/integrations/gpt/schedule_initial_ad_init.test.ts`
> (executable Vitest lifecycle assertions, not string matches on emitted Rust output). Edit the TS
> module, not the Rust `format!`.

---

## Preconditions

- **Base branch:** PR #945 (`fix/react418-hydration-safety`) must be merged to `main` first — it introduces the `window.load` gate this plan transforms. Branch off updated `main`.
  - If #945 has **not** merged when you execute, stack this branch on `fix/react418-hydration-safety` instead, and retarget the PR to `main` after #945 lands.
- **Verify the starting point exists** before editing: `build_bids_script` must contain `window.addEventListener(\"load\"`. If it instead calls `adInit` synchronously (pre-#945 `main`), STOP — the base is wrong.
- **Commits require user approval** (project rule): the commit steps below are real, but ask before running each `git commit`. Stage with explicit paths, never `git add -A`, never stage `ts.toml`.

## File Structure

- Modify: `crates/trusted-server-core/src/publisher.rs`
  - `build_bids_script` (the emitted `<script>` + its doc comment) — the only production change.
  - `build_empty_bids_script` delegates to `build_bids_script`, so it inherits the gate automatically — do not touch it.
- Modify (tests, same file, `#[cfg(test)]` module):
  - `bids_script_defers_ad_init_until_after_hydration` — extend for the chunk-await structure.
  - `bids_script_is_xss_safe` — unchanged, but it is the load-bearing guard that the emitted script has no `<` / `>`. Must stay green.
  - `bids_script_calls_ad_init_without_retry_timer` — unchanged, stays green (no `setTimeout`).

---

## Task 1: Extend the deferral test for the chunk-await gate (RED)

**Files:**

- Test: `crates/trusted-server-core/src/publisher.rs` — `bids_script_defers_ad_init_until_after_hydration`

- [ ] **Step 1: Add the new assertions to the existing test**

Keep every existing assertion (the `window.load` fallback is retained) and add the chunk-await ones. The test body becomes:

```rust
#[test]
fn bids_script_defers_ad_init_until_after_hydration() {
    let mut map = serde_json::Map::new();
    map.insert("atf".to_string(), serde_json::json!({"hb_pb": "1.00"}));

    let script = build_bids_script(&map);

    // Retained from PR #945: deferral, window.load fallback, route guard,
    // qualified globals, no retry timer.
    assert!(
        script.contains("requestAnimationFrame"),
        "should defer adInit to a post-hydration animation frame"
    );
    assert!(
        script.contains("\"load\""),
        "should keep window.load as the can't-hang fallback"
    );
    assert!(
        !script.contains("setTimeout"),
        "should not retry adInit on a timer"
    );
    assert!(
        script.contains("window.tsjs.adInit"),
        "should still hand off bids to adInit"
    );
    assert!(
        script.contains("window.requestAnimationFrame"),
        "should qualify requestAnimationFrame on window"
    );
    assert!(
        script.contains("window.addEventListener"),
        "should qualify addEventListener on window"
    );
    assert!(
        script.contains("location.pathname"),
        "should keep the SPA route guard"
    );

    // New: wait for the async Next.js hydration chunks before adInit.
    assert!(
        script.contains("/_next/static/chunks/"),
        "should gate on the Next.js hydration chunks"
    );
    assert!(
        script.contains("getEntriesByType"),
        "should pre-clear already-finished chunks via resource timing"
    );
    assert!(
        script.contains("DOMContentLoaded"),
        "should start the chunk check at DOMContentLoaded"
    );
    assert!(
        script.contains("forEach"),
        "should iterate with forEach (no for-loop; keeps the script free of < and >)"
    );
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test-fastly bids_script_defers_ad_init_until_after_hydration`
Expected: **FAIL** — the current (#945) script has no `/_next/static/chunks/`, `getEntriesByType`, `DOMContentLoaded`, or `forEach`.

---

## Task 2: Implement the hybrid gate (GREEN)

**Files:**

- Modify: `crates/trusted-server-core/src/publisher.rs` — `build_bids_script`

- [ ] **Step 1: Replace the doc comment**

Replace the existing `// adInit() defines GPT slots …` comment block above the `format!` with:

```rust
    // adInit() defines GPT slots on the publisher's `-container` wrappers, mutating
    // those ad-slot subtrees. Running it before React hydration commits trips a #418
    // hydration mismatch on Next.js App Router. Defer it until hydration has run.
    //
    // The App Router loads its hydration chunks as `async` scripts, which do NOT
    // block DOMContentLoaded — so DCL alone can precede hydration. `window.load` is
    // safe (it awaits async scripts) but also waits for every image/tracker, which is
    // ~52s on heavy pages. So: at DOMContentLoaded, wait for the async
    // `/_next/static/chunks/` scripts to finish (PerformanceResourceTiming for the
    // ones already done, load/error for the rest), then a double requestAnimationFrame,
    // then adInit. window.load stays as an unconditional can't-hang fallback and as the
    // unchanged path for non-Next publishers (no chunks matched). A `fired` flag makes
    // whichever path completes first win exactly once.
    //
    // The route guard captures the route this run was scheduled for and no-ops if an
    // SPA navigation changed it (a deferred adInit must not run against a newer route).
    //
    // IMPORTANT: the emitted script must contain no `<` or `>` (see
    // `bids_script_is_xss_safe`; only the JSON payload is HTML-escaped). Iterate with
    // `forEach` / `Array.prototype.forEach.call`, never a `for (i = 0; i < n; i++)` loop.
```

- [ ] **Step 2: Replace the `format!` body**

Replace the entire `format!( … )` with:

```rust
    format!(
        "<script>(window.tsjs=window.tsjs||{{}}).bids=JSON.parse(\"{}\");\
(function(){{\
var p=location.pathname+location.search;\
var f=function(){{\
if(location.pathname+location.search!==p)return;\
var a=window.tsjs.adInit;if(typeof a===\"function\")a();}};\
var d=function(){{window.requestAnimationFrame(function(){{window.requestAnimationFrame(f);}});}};\
if(document.readyState===\"complete\"){{d();return;}}\
var fired=false;\
var t=function(){{if(fired)return;fired=true;d();}};\
window.addEventListener(\"load\",t,{{once:true}});\
var c=function(){{\
var s=document.querySelectorAll('script[src*=\"/_next/static/chunks/\"]');\
if(s.length===0)return;\
var n=s.length;var done={{}};\
var es=(window.performance&&window.performance.getEntriesByType)?window.performance.getEntriesByType(\"resource\"):[];\
es.forEach(function(e){{done[e.name]=true;}});\
var dec=function(){{if(--n===0)t();}};\
Array.prototype.forEach.call(s,function(x){{\
if(x.src&&done[x.src])dec();\
else{{x.addEventListener(\"load\",dec,{{once:true}});x.addEventListener(\"error\",dec,{{once:true}});}}\
}});\
}};\
if(document.readyState===\"loading\")document.addEventListener(\"DOMContentLoaded\",c,{{once:true}});\
else c();\
}})();</script>",
        escaped
    )
```

This emits (with the JSON payload spliced in):

```js
;(window.tsjs = window.tsjs || {}).bids = JSON.parse('…')
;(function () {
  var p = location.pathname + location.search
  var f = function () {
    if (location.pathname + location.search !== p) return
    var a = window.tsjs.adInit
    if (typeof a === 'function') a()
  }
  var d = function () {
    window.requestAnimationFrame(function () {
      window.requestAnimationFrame(f)
    })
  }
  if (document.readyState === 'complete') {
    d()
    return
  }
  var fired = false
  var t = function () {
    if (fired) return
    fired = true
    d()
  }
  window.addEventListener('load', t, { once: true })
  var c = function () {
    var s = document.querySelectorAll('script[src*="/_next/static/chunks/"]')
    if (s.length === 0) return
    var n = s.length
    var done = {}
    var es =
      window.performance && window.performance.getEntriesByType
        ? window.performance.getEntriesByType('resource')
        : []
    es.forEach(function (e) {
      done[e.name] = true
    })
    var dec = function () {
      if (--n === 0) t()
    }
    Array.prototype.forEach.call(s, function (x) {
      if (x.src && done[x.src]) dec()
      else {
        x.addEventListener('load', dec, { once: true })
        x.addEventListener('error', dec, { once: true })
      }
    })
  }
  if (document.readyState === 'loading')
    document.addEventListener('DOMContentLoaded', c, { once: true })
  else c()
})()
```

- [ ] **Step 3: Run the deferral test to verify it passes**

Run: `cargo test-fastly bids_script_defers_ad_init_until_after_hydration`
Expected: **PASS**.

- [ ] **Step 4: Run the XSS guard (must stay green)**

Run: `cargo test-fastly bids_script_is_xss_safe`
Expected: **PASS** — proves the emitted script has no `<` / `>`. If it fails, a `for`-loop `<` slipped in; convert it to `forEach`.

---

## Task 3: Full verification

- [ ] **Step 1: Full core suite under Viceroy**

`build_bids_script` is a format-emitting function, and Viceroy aborts on the first panic (hiding later failures), so run the whole suite, not just the three tests.

Run: `cargo test-fastly`
Expected: all pass (baseline was 1646 passing on the #945 branch; expect the same plus the extended assertions).

- [ ] **Step 2: Format + lint**

Run: `cargo fmt --all -- --check`
Run: `cargo clippy-fastly`
Expected: clean (no changes / no warnings). No JS-bundle rebuild is needed — this is a Rust-emitted inline string.

---

## Task 4: Live acceptance — the #418 A/B (blocking, manual)

Unit tests cannot prove the gate is still post-hydration. Re-run PR #945's own measurement.

- [ ] **Step 1: Reproduce on a live App Router publisher via the dev proxy**

Start the proxy (macOS), mapping the publisher host to the edge, with basic auth injected and a valid DataDome cookie in the browser (credentials out of band, never committed). Drive the page with the tester cookie that toggles Trusted Server, per the method in the spec.

- [ ] **Step 2: Count React #418 with TS active**

Expected: **#418 count = 0** with TS active (matching #945's "pure publisher" baseline). Any #418 means the gate fired before hydration on that publisher — do not roll out; fall the design back toward `window.load`.

- [ ] **Step 3: Confirm timing + targeting**

Expected: the first targeted GPT request carries `ts_initial=1` (adindex 0) and fires when the hydration chunks finish (a few seconds), not at `window.load` (~52s). `servicesEnabled: true`, TS container slot defined, ads render.

---

## Task 5: Commit and open the PR

- [ ] **Step 1: Stage the explicit paths (ask before committing)**

```bash
git add crates/trusted-server-core/src/publisher.rs
git add docs/superpowers/specs/2026-07-24-adinit-hydration-gate-design.md
git add docs/superpowers/plans/2026-07-25-adinit-hydration-gate.md
git status --short   # confirm ts.toml is NOT staged
```

- [ ] **Step 2: Commit**

```bash
git commit -m "Gate initial adInit on Next.js hydration chunks instead of window load"
```

- [ ] **Step 3: Open the stacked follow-up PR to `main`** (after #945 has merged), summarizing the load→chunk-await change and attaching the #418 A/B evidence from Task 4.

---

## Acceptance criteria (recap)

1. #418 A/B on a live App Router page: **count = 0** with TS active (Task 4).
2. First targeted GPT request carries `ts_initial=1` and fires ~seconds, not ~52s.
3. `cargo test-fastly`, `cargo fmt --all -- --check`, `cargo clippy-fastly` all clean.
4. `bids_script_is_xss_safe` green (no `<` / `>` in the emitted script).
