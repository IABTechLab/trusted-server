# Fire initial `adInit` after Next.js hydration chunks execute (not `window.load`)

**Date:** 2026-07-24
**Status:** Design — finalized after review (await async hydration chunks); pending approval
**Refs:** builds on PR #945 (`fix/react418-hydration-safety`, open), issue #938, investigation from issue #958

## Background — the original problem (issue #958)

The work began as issue #958: _"Investigate refresh too quickly after SSAT with client-side auction
(timing issue with GAM)."_ The concern: a Trusted Server slot receives server-side auction targeting
(SSAT), then shortly after a client-side auction refreshes the same slot — a suspected GAM
timing / throttling problem.

A live investigation — the dev proxy against a real Next.js App Router publisher, driven with
headless Chrome and cross-checked against the real-browser Google Publisher Console — reframed the
problem. The "refresh too quickly" symptom is two separable behaviors:

1. A subsequent client-side refresh clears `ts_initial` and re-auctions the slot. This is the
   intended **single-shot SSAT** design, not a defect.
2. SSAT itself is delivered **very late** (gated on `window.load`), so it lands close to — or on top
   of — the publisher's ongoing refresh cadence.

The root cause worth fixing is (2): SSAT is late. That is what this spec addresses. The refresh
behavior in (1) is left as-is.

## Update (post-deployment live test): pivot from chunk-await to `__next_f`

The chunk-await approach described in the sections below was implemented, deployed, and
**live-tested — and it does not work.** On the real publisher the gate never fired early and always
fell back to `window.load` (~43s, no improvement). Measured cause: the async `/_next/static/chunks/`
scripts finish **before** they can be observed — their `load` events have already fired (so late
listeners never hear them), their `PerformanceResourceTiming` entries are evicted (only 1–15 of 24
present), and ~9 of 24 are phantom prefetch/duplicate tags that never complete — so "all chunks
done" is never detectable.

**The gate was changed to the App Router runtime signal instead:** poll for `window.__next_f.push`
being replaced by the RSC runtime (~9s on the tested publisher, vs ~40s for `window.load`), then the
double `requestAnimationFrame`; `window.load` stays as the can't-hang fallback and the non-Next
path. This is reliable and early where chunk-await was neither.

**Safety is still pending the #418 A/B.** `__next_f.push` replaced means "RSC runtime started
hydrating," not provably "hydration committed" — the double-rAF is the margin, and the #418 count on
a live A/B is the proof. That A/B is currently blocked by a separate rc/july bug
(`elementId.startsWith`, PR #966's `configuredSlotForElementId`, since reverted on #966) that crashes
the ad stack; the fix is to pick up #966's revert in the deploy, then run the A/B.

The sections below document the superseded chunk-await approach and why it was chosen — kept for the
record. The **current** implementation is the `__next_f` gate.

## Problem

PR #945 makes the `</body>` bids bootstrap defer `window.tsjs.adInit()` until after React
hydration, to stop a **#418 hydration mismatch** on Next.js App Router publishers. adInit
mutates the publisher's ad-slot subtrees (GPT `defineSlot` on the `-container` wrapper); running
it synchronously at body-parse time lands that mutation inside React's hydration window.

PR #945 gates the deferred call on the **`window.load`** event (then a double
`requestAnimationFrame`). `window.load` waits for _every_ subresource — images, trackers,
third-party ad/analytics scripts — so its timing is dominated by page weight, not by hydration.

- On the light publisher #945 tested, `load` fired at **~3.7s** — the first targeted GPT request
  carried `ts_initial=1`, so this looked fine.
- On a **heavy** App Router publisher (a live site with a large third-party payload), the same
  gate fires **~52s** after navigation. The Trusted Server ad slots sit **empty** the whole time,
  then adInit fills them — correctly, but ~50 seconds late.

### Measured evidence

Real browser, heavy App Router publisher, from frozen navigation timing:

| Marker                                                  | Time   |
| ------------------------------------------------------- | ------ |
| document received (`responseEnd`)                       | ~2.8s  |
| **`DOMContentLoaded` (`domContentLoadedEventEnd`)**     | ~3.1s  |
| **`window.load` (`loadEventEnd`)** — where adInit fires | ~52.7s |

Independently reproduced through the dev proxy (headless): adInit fired **25ms after**
`loadEventEnd`, confirming the gate is `window.load`; `DOMContentLoaded` was ~50–90s earlier on
the same loads.

The server-side bids are SSR-injected into the HTML, so they are present at first byte (~2.8s).
Nothing blocks adInit except the gate. The Trusted Server slot's first GPT request already carries
`ts_initial=1` (adindex 0) — ordering is correct; the slot is simply requested ~50s too late.

## Investigation method

The behavior was observed on a live Next.js App Router publisher **without deploying any change**, by
routing the publisher through the Trusted Server edge locally and instrumenting the browser.

**1. Local production-hostname proxy.** `ts dev proxy` (macOS MITM proxy) mapped the publisher host
to the Trusted Server edge host with `--rewrite-host`, injected the publisher's basic-auth
credentials, and listened on loopback. Matched requests are MITM'd and Host-rewritten so the browser
loads the real page with Trusted Server active; unmatched hosts (GAM / `doubleclick.net`) are
blind-tunnelled through untouched so the ad stack runs normally. Credentials and the DataDome cookie
were supplied out of band and used **in memory only** — never written to any file, test, or the
repository.

**2. Controllable browser.** A Playwright-driven Chromium (system Chrome channel) pointed at the
proxy with `ignoreHTTPSErrors` (dev CA), seeded the DataDome cookie on the context, and navigated to
the page with `?ts_console=1`. Real navigations were used (`Sec-Fetch-Mode: navigate`), which the
server-side auction path requires.

**3. Instrumentation captured per load:**

- Every GAM ad request (`/gampad/ads`): the query parsed into per-slot rows from `prev_scp`
  (`adindex`, `adzone`, `ts_initial`, APS `amznbid`, Prebid `hb_*`), plus `dids`, `correlator`,
  `idt`, and the response status / body length / fill.
- `googletag.pubads().refresh()` wrapped to log each call's slots and the `adInitRefreshInProgress`
  flag; `pbjs.requestBids()` wrapped to log each client-side auction.
- `window.tsjs.adTrace.export()` (the ad-trace machinery from PR #919 / #961): generation-scoped
  GPT request / response / render events with `isEmpty`, used as the authoritative fill signal.
- Navigation timing (`responseEnd`, `domInteractive`, `domContentLoadedEventEnd`, `loadEventEnd`)
  and a perf-marks / `requestIdleCallback` / `window.next` probe for the gate analysis.

**4. Real-browser cross-check.** The same page was opened in a normal browser with
`?google_console=true` (Google Publisher Console) to read per-slot delivery (line item, ad fetch
count, slot-level targeting), and with console one-liners reading the frozen navigation-timing entry
(`performance.getEntriesByType('navigation')[0]`) for `DOMContentLoaded` / `load`. This anchored the
absolute timings: headless-through-proxy inflates `window.load` (~93s headless vs ~52s real), so
structural ordering was taken from the instrumented runs and absolute numbers from the real browser.

**5. Code-level reproduction.** The synchronous-flag / gate behavior was reproduced in the Rust and
Vitest unit suites (the refresh wrapper's targeting-clear, the adInit bypass flag) under the pinned
Node runtime, independent of any live site.

## Investigation summary

### Confirmed (code + live wire + real browser)

- SSAT is the Trusted Server slot's **first** GPT fetch (adindex 0) and carries `ts_initial=1` —
  ordering is correct, the slot is only late.
- adInit fires on **`window.load`**: headless, it fired 25ms after `loadEventEnd`; real browser,
  `loadEventEnd` ~52.7s vs `domContentLoadedEventEnd` ~3.1s on the same load.
- The subsequent client-side refresh (adindex 1) **strips `ts_initial`** and re-auctions — verified
  on the wire via GAM `prev_scp` (adindex 0 has `ts_initial=1`, adindex 1 does not). This is the
  intended single-shot SSAT behavior.
- In every observed run all GAM fetches **filled** (non-empty, rendered); the SSAT impression is
  superseded by the later refresh, which is expected refresh behavior.
- The publisher's own GPT slots are **distinct** from Trusted Server slots (separate div ids and
  fetch timing) — the early pre-SSAT fetch belonged to the publisher, not to a TS slot.
- The `window.load` gate originates in PR #945, chosen deliberately as a conservative
  post-hydration proxy.
- **App Router loads its hydration chunks `async`.** The served HTML has 23 of 24 `/_next/static`
  script tags `async` (0 `defer`), including `main-app`. Because `async` scripts do not block
  `DOMContentLoaded`, DCL can fire before hydration — so bare DCL is **not** a safe gate. (The
  earlier "hydration ≤ DCL" observation was a race artifact: DCL was inflated by other blocking
  scripts, giving the async chunks time to finish first.)
- `requestIdleCallback` fires ~3s — **before** the framework runtime (`window.next` ~17.9s) — so it
  is not a safe hydration proxy. The `Next.js-hydration` performance measure is absent on App Router
  (`performance.getEntriesByName('Next.js-hydration')` returns `[]`); it is a Pages Router signal.

### Still open / not confirmed

- **GAM's internal throttle decision was never directly observed.** No blank / no-fill occurred on
  the SSAT path in any run. A separate publisher-driven double-fetch ~1.6s apart _did_ return an
  empty body, showing GAM can drop closely spaced fetches — but the SSAT-path sub-30s cadence did
  not manifest a drop. The sub-30s double-request is a policy violation / risk, not a proven loss.
- **The sub-second mid-flight clobber** — a refresh landing before `slotRenderEnded`, which would
  wipe SSAT targeting mid-flight — is proven only in a unit test. Live refresh cadence (12–31s) was
  always far longer than render (~1s), so it never triggered in observation.
- **Revenue impact of the strip is unquantified.** The SSAT impression rendered and was viewable
  before being superseded (12–31s later), so no loss was demonstrated.
- **Exact real-browser hydration-commit time was not captured.** Console probes for idle / framework
  markers were run after load and returned "now"; only the frozen `DOMContentLoaded` / `load`
  navigation-timing values are reliable.

## Goal

Fire the initial `adInit` as soon as the Next.js hydration chunks have executed — a few seconds on
the heavy publisher, versus ~52s for `window.load` — and never before hydration. Not synchronous
(reintroduces #418), not bare `DOMContentLoaded` (fires before the async hydration chunks), not
`window.load` (dominated by page weight).

## Non-goals

- `ts_initial` targeting being cleared on a subsequent client-side refresh (single-shot SSAT is
  by design).
- Sub-30s refresh cadence and GAM throttling behavior.
- Server-side auction / page-bids latency.
- Firing before hydration at first byte (~2.8s) via a "hydration-safe adInit" rewrite that only
  touches DOM React does not own — larger change, separate effort.

## Approach

Replace the single `window.load` gate #945 introduced with a **hybrid gate** in
`crates/trusted-server-core/src/publisher.rs::build_bids_script` that waits for the Next.js App
Router hydration chunks to execute — not for all images/trackers — and falls back to `window.load`.

`window.load` is safe because it awaits async scripts too; its only fault is also awaiting images
and trackers (→ ~52s). The hybrid replicates the async-script guarantee for the hydration chunks
alone.

Logic:

1. If `document.readyState === "complete"`, `load` has already fired (hydration long done) →
   double-`rAF` → adInit, return.
2. Otherwise attach an unconditional `window.load` listener as a can't-hang fallback, guarded by a
   `fired` flag (also the unchanged path for non-Next publishers).
3. At `DOMContentLoaded` (or immediately if past it), query
   `document.querySelectorAll('script[src*="/_next/static/chunks/"]')`.
   - If none exist → do nothing; the `window.load` fallback fires (non-Next, safe).
   - If chunks exist → count them; use `performance.getEntriesByType('resource')` to pre-clear the
     ones already finished (match on the resolved `script.src`), and attach `load`/`error` listeners
     to the rest. When the count reaches 0 → fire (via the `fired`-guarded trigger) → double-`rAF` →
     adInit.
4. The `fired` flag makes whichever path completes first win exactly once. #945's SPA route-guard
   (captures `location.pathname + location.search`, no-ops if the route changed) and window-qualified
   globals are retained; still run-once, no retry timer.

**Codebase constraint — no `<` / `>` in the emitted script.** `build_bids_script` only HTML-escapes
the JSON bid payload, not the JS template, and `bids_script_is_xss_safe` asserts the inner script
contains no `<` or `>`. So iteration must use `Array.prototype.forEach.call(nodeList, …)` /
`entries.forEach(…)` — **not** `for (i = 0; i < n; i++)` loops, whose `<` would fail that test and
emit a raw `<` inside `<script>`. (The reviewer's example snippet used `for` loops and would fail
here; this is a required adaptation.)

Illustrative emitted script (final form is the plan's; note the `forEach` and the `===` / `!==` /
`--pending === 0` comparisons — no `<` / `>` anywhere):

```js
;(function () {
  var p = location.pathname + location.search
  var fire = function () {
    if (location.pathname + location.search !== p) return // SPA route-guard
    var a = window.tsjs.adInit
    if (typeof a === 'function') a()
  }
  var raf2 = function () {
    window.requestAnimationFrame(function () {
      window.requestAnimationFrame(fire)
    })
  }
  if (document.readyState === 'complete') {
    raf2()
    return
  }
  var fired = false
  var trigger = function () {
    if (fired) return
    fired = true
    raf2()
  }
  window.addEventListener('load', trigger, { once: true }) // can't-hang fallback
  var check = function () {
    var chunks = document.querySelectorAll(
      'script[src*="/_next/static/chunks/"]'
    )
    if (chunks.length === 0) return // non-Next: let window.load fire
    var pending = chunks.length
    var done = {}
    var entries =
      window.performance && window.performance.getEntriesByType
        ? window.performance.getEntriesByType('resource')
        : []
    entries.forEach(function (e) {
      done[e.name] = true
    })
    var dec = function () {
      if (--pending === 0) trigger()
    }
    Array.prototype.forEach.call(chunks, function (s) {
      if (s.src && done[s.src]) dec()
      else {
        s.addEventListener('load', dec, { once: true })
        s.addEventListener('error', dec, { once: true })
      }
    })
  }
  if (document.readyState === 'loading')
    document.addEventListener('DOMContentLoaded', check, { once: true })
  else check()
})()
```

`build_empty_bids_script` delegates to `build_bids_script`, so the empty-bids path inherits the same
gate; behavior is unchanged apart from timing.

## Why this gate (and not DCL, idle, or a runtime poll)

- **Bare `DOMContentLoaded` is unsafe.** Verified on the live HTML: 23 of 24 `/_next/static` script
  tags are `async` (0 `defer`), including `main-app`. `async` scripts do not block DCL, so DCL can
  fire before the hydration chunks execute → adInit mutates pre-hydration DOM → #418. The earlier
  "hydration ≤ DCL" reading was a race artifact (DCL dragged out by other blocking scripts).
- **`requestIdleCallback`** fires on first idle (~3s) — before the framework runtime — unsafe.
- **`Next.js-hydration` performance measure** is absent on App Router
  (`getEntriesByName('Next.js-hydration')` → `[]`); it is a Pages Router signal. React exposes no
  hydration-complete event.
- **Polling a runtime flag** (`__next_f.push` patched / `__NEXT_DATA__`) detects the runtime
  **booted**, not hydration **committed** → can fire mid-hydration → still #418.
- **Waiting for the async hydration chunks to load, then double-`rAF`** is safe-by-construction: those
  chunks are what hydration runs from, and `window.load` — which the fallback preserves — is defined
  to await them anyway. We only skip the images/trackers that inflate `load` to ~52s.

## Risk and safety

- **Resource entry = downloaded, not executed.** A `PerformanceResourceTiming` entry marks network
  completion; the script executes (and React hydrates) just after. The double-`rAF` covers this gap.
- **`resource` buffer is capped (~250 entries).** On a heavy page an early chunk's entry can be
  evicted; we then attach a `load` listener to a script that already fired, so that chunk never
  decrements. This does not hang — the `window.load` fallback still fires. Worst case = #945's
  behavior (safe, slow).
- **Chunks injected after DCL are not awaited.** The query snapshots at DCL. Initial hydration chunks
  are present in the served HTML (24 observed), so initial hydration is covered; lazy route chunks are
  not hydration. Acceptable.
- **Not a mathematical proof of hydration completion** — streaming/Suspense hydrate incrementally and
  React exposes no completion event. Rollout is therefore gated on the #418 A/B below, not on
  reasoning alone; any observed #418 falls the design back toward `window.load`.

## Acceptance criteria

1. **#418 A/B (blocking).** Re-run PR #945's own measurement — toggle Trusted Server on the same
   live App Router page via the tester cookie — with the hydration-chunk gate, and confirm the React
   **#418 count remains 0** with TS active. This is what proves the gate is still post-hydration;
   unit tests cannot.
2. First targeted GPT request still carries `ts_initial=1` (server-side targeting reaches the first
   impression), and fires when the hydration chunks finish (a few seconds) rather than at
   `window.load`.
3. `servicesEnabled: true`, the TS container slot is defined, and ads render.

## Testing

- Update `bids_script_defers_ad_init_until_after_hydration` to assert the emitted script:
  - contains `/_next/static/chunks/` (the hydration-chunk query),
  - contains `getEntriesByType` (the already-loaded pre-clear),
  - contains `DOMContentLoaded`,
  - **still** contains the `window.load` fallback (`"load"`), `requestAnimationFrame`,
    `window.tsjs.adInit`, the route guard (`location.pathname`), and no `setTimeout`,
  - keeps `window.requestAnimationFrame` / `window.addEventListener` window-qualified.
- `bids_script_is_xss_safe` **must stay green** — this is the load-bearing check that the script uses
  `forEach` and contains no `<` / `>`.
- `bids_script_calls_ad_init_without_retry_timer` stays green (no `setTimeout`).
- Local verification: `cargo test-fastly`, `cargo fmt --all -- --check`, `cargo clippy-fastly`.
- No JS-bundle change (this is a Rust-emitted inline script), so the JS test/build gates are
  unaffected.

## Rollout

Canary a single publisher first, watch the browser console for React #418, then widen. No new
configuration surface is introduced.

## PR strategy

Stacked follow-up: let PR #945 merge as-is (conservative `window.load`, #418 fixed), then open a
separate PR that changes the gate to await the async hydration chunks, branched off `main` once #945
lands. The change is intentionally isolated to `build_bids_script` and its tests so the timing change
has its own review and its own #418 A/B evidence.
