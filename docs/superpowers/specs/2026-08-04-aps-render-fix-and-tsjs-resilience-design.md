# APS Render Fix and TSJS Resilience Architecture — Design

- **Status:** draft for review
- **Date:** 2026-08-04
- **Baseline:** `rc/july` @ `541298695` — the full merged state (everything merged
  from `main` plus every rc-only merge), not just the delta pending against
  `main`.
- **Inputs:** three code audits performed against this baseline (APS end-to-end
  trace, TSJS architecture audit, GPT integration map); open issues #926, #941,
  #944, #962, #964, #977, #983, #989, #993; open PR #997.

---

## 1. Problem statement

APS (Amazon Publisher Services) demand is fully integrated server-side — the
edge server runs the APS OpenRTB auction, wins bids, and ships a typed renderer
descriptor to the page — yet APS creatives still do not appear for real users.
Every previous fix (the `bid.meta` carrier so Prebid does not strip the
descriptor, the decoupled prebid shim, the `hb_adid` fallback to the OpenRTB bid
id) addressed a real defect, and APS still does not render. That pattern — serial
single-cause fixes that each survive review and still do not produce ads — is
itself the finding: the APS pipeline has **multiple independent failure points,
most of which fail silently**, and the client library has **no way to tell the
server (or the operator) which one fired**.

At the same time, the TSJS client library has grown organically to 56 files /
~11,900 lines with two ~1,700-line monoliths, duplicated logic maintained by
hand in two languages, inverted layering, and roughly one hundred `catch` blocks
that discard failures. The APS outage and the library's shape are the same
problem seen from two sides: a delivery pipeline whose failure modes are
invisible and whose components cannot be reasoned about independently.

This design covers both: (a) the specific fixes that make APS render, and (b)
the target architecture that makes TSJS a clean, resilient library so the next
integration does not reproduce this failure class.

### Non-goals

- No change to the APS OpenRTB endpoint contract or Amazon-side configuration.
- No rewrite of Prebid.js integration strategy (the decoupled shim stays).
- No visual/behavioral change for publishers whose pages work today.

---

## 2. Why APS still does not render — the evidence

The audit traced all four delivery flows: (a) SSAT server-side ad template via
`window.tsjs.bids`, (b) GAM + client-side `trustedServer` Prebid adapter, (c)
SPA `/_ts/page-bids` re-auction, (d) direct `/auction` via `tsjs.requestAds`.
Only flow (d) — the demo path nobody runs in production — can render an APS
descriptor without GAM's cooperation.

The failure points, ranked by likelihood and blast radius:

### 2.1 Admission: APS bids are eliminated before they can win

| #   | Failure                                                                                                                                                                                                                                                                        | Where                                            |
| --- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ------------------------------------------------ |
| A1  | **A configured `[auction].mediator` discards every direct-provider bid.** Winners come exclusively from the mediator response; APS bids (with their renderers) are used only as mediator input and reporting. APS shows `status: success, bid_count: N` yet never wins a slot. | `orchestrator.rs:412-431`                        |
| A2  | **`allow_script_creatives` defaults to `false`**, dropping every `tagtype: "script"` APS bid — a large share of TAM demand. The drop is counted but invisible (see A4).                                                                                                        | `aps.rs:141-143`, `:773-778`                     |
| A3  | **Strict per-bid gates**: exact `w`×`h` match against configured formats (a 300×600 answer on a `[[300,250],[728,90]]` slot dies), required `ext.creativeurl`, and any top-level `contextual` key rejects the entire response.                                                 | `aps.rs:657-668`, `:745-778`, `:838-846`         |
| A4  | **Drop reasons never reach an operator on the production paths.** `drop_reasons` counters surface only in `/auction` `ext.orchestrator`; the SSAT and page-bids paths discard them, server logs do not carry them, and the `ts-debug` comment allowlist excludes them.         | `publisher.rs:1866-1875`, `telemetry.rs:808-826` |

### 2.2 Identity: the `hb_adid` contract with GAM is unproven

| #   | Failure                                                                                                                                                                                                                                                                                     | Where                                         |
| --- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------- |
| B1  | **GAM key-value values are capped at 40 characters.** The new fallback emits the raw APS OpenRTB bid `id` (a long opaque string) as `hb_adid`. If GAM truncates or rejects it, `%%PATTERN:hb_adid%%` comes back different, the bridge's equality check fails, and it bails **with no log**. | `publisher.rs:3366-3372`, `gpt/index.ts:1613` |
| B2  | **Two id universes for the same bid.** SSAT keys the bridge on the APS bid id; the client-side Prebid adapter keys on Prebid's generated `adId`. A page running both paths registers the same slot under different ids.                                                                     | `publisher.rs:3366`, `prebid/index.ts:982`    |

### 2.3 Render: the client has one narrow happy path and no fallback

| #   | Failure                                                                                                                                                                                                                     | Where                                                     |
| --- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------- |
| C1  | **If GAM never serves the Prebid Universal Creative, nothing renders and nothing is recorded.** The renderer descriptor sits unused in `window.tsjs.bids`; `renderApsCreative` is reachable only from the unused flow (d).  | `gpt/index.ts:854-1107`, `core/request.ts:59`             |
| C2  | **The `/integrations/aps/renderer` route registers only when `[integrations.aps]` is enabled on that origin.** A 404/401 there means the sandboxed iframe waits 10 s and dies silently.                                     | `aps.rs:1188-1245`, `aps/render.ts:404`                   |
| C3  | **SafeFrame breaks slot attribution.** The bridge resolves a message source by walking top-document iframes under the slot div; a nested SafeFrame creative window is invisible to that walk, so the bridge bails silently. | `gpt/index.ts:157-183`, `:1599-1600`                      |
| C4  | **Three hand-maintained copies of the descriptor schema** (Rust struct, TS validator, inline renderer-document validator) with exact-key rejection: any server-side field addition instantly blanks every APS ad.           | `types.rs:188-211`, `aps/render.ts:60-93`, `aps.rs:65-73` |
| C5  | **A dead duplicate renderer branch in the bridge** contains the debug log and dedup logic people would look for while debugging; it can never execute.                                                                      | `gpt/index.ts:1643-1674`                                  |
| C6  | **The renderer CSP may kill creatives after success is reported** (`object-src`, workers, `blob:`/`data:` frames are blocked; `renderer-ready` fires before the creative actually paints).                                  | `aps.rs:49`                                               |
| C7  | **The renderer branches record nothing**: no `recordRender`, no win/billing beacons, no `stampCreativeTrace` — an APS win looks "never rendered" in every trace whether or not it painted.                                  | `gpt/index.ts:1558-1632`                                  |

### 2.4 Observability: the common factor

There is **zero client→server reporting**. Server telemetry marks `is_win=1` at
auction time and goes quiet; a bid that never painted is byte-identical to one
that painted perfectly. Client-side evidence (`window.tsjs.renders`, console
warnings, `data-ts-*` attributes) dies with the tab. This is why "APS still does
not work" has taken weeks instead of a dashboard row saying
`render_fail{renderer_endpoint_404}`.

---

## 3. The GPT reality this design must respect

The GPT integration is a **bootstrap-first hybrid**: the server injects a
495-line ES5 `gpt_bootstrap.js` inline in `<head>` before the TSJS bundle, and
the two coordinate through shared monkeypatch sentinels, with document order
deciding the winner. The audit's key facts:

1. **The bundle's handoff and initial-load code is dead in production.** The
   bootstrap installs its wrappers first and sets the same sentinels the bundle
   checks; ~200 lines of the TypeScript the test suite exercises most heavily
   never run on a real page. A fix landed only in `gpt/index.ts` has no
   production effect.
2. **The slot handoff can alias a publisher's new div to a GPT slot bound to a
   dead element** — and the orphan-recovery watcher built to repair exactly that
   was **lost in the #922 merge** (`0dc9b19a9` resolved `gpt/index.ts` to the rc
   side). `updateRender` (one impression, one row) now has no production
   caller, `__tsRenderGeneration` / `__tsRenderBid` are dead writes, and every
   bridge-served impression double-counts in the trace. Open PR #997 appears to
   be the reworked replacement; restoring this is a correctness prerequisite,
   not a refactor.
3. **TS refreshes never pass `changeCorrelator: false`**, so every TS-driven
   refresh starts a new GAM page-view correlator — silently changing roadblock,
   competitive-exclusion, and frequency-capping behavior.
4. **`enableSingleRequest()` is called blind** after the publisher's own
   `enableServices()` has almost always run (post-#945 deferral), so SRA intent
   is asserted but not real — and on pages where TS wins the race, it forces
   SRA onto publishers who chose otherwise.
5. Responsive resolution is a DOM-element-selection ladder (not GPT size
   mapping); ambiguity silently skips the slot for the whole pass.
6. Three independent wrappers on `pubads().refresh` (bootstrap, bundle, prebid)
   coordinate via window-global booleans that async wrappers observe already
   reset.

Any APS fix that adds more targeting keys, more refresh calls, or more
postMessage traffic has to land inside this reality, which is why the design
couples the APS fix to the library restructuring instead of adding a sixth
patch to the pile.

---

## 4. Design overview

Three workstreams, ordered by dependency:

1. **See the failures** — client→server disposition telemetry plus surfacing
   the server's existing drop counters. Without this, every subsequent fix is
   another blind patch.
2. **Fix APS delivery** — admission, identity, render chain, schema, and
   bridge fixes, each verifiable by the new telemetry and by contract tests.
3. **Restructure TSJS** — the kernel/adapters/services architecture that makes
   the fixes durable and the next integration cheap.

---

## 5. Workstream 1 — Observability first

### 5.1 Client disposition beacon

A new kernel module batches disposition events and posts them to a new
`POST /_ts/client-events` ingest route:

```
{ v: 1, page: {auctionId, navGen}, events: [
  { t: "bid_received",   slot, bidder, source }
  { t: "targeting_set",  slot, hbAdid }
  { t: "bridge_request", slot, adId, matched: bool }
  { t: "render_attempt", slot, source: "renderer"|"adm"|"pbs-cache" }
  { t: "render_ok",      slot, source }
  { t: "render_fail",    slot, source, reason }   // reason is a closed enum
] }
```

- Transport: `navigator.sendBeacon` with `fetch` keepalive fallback; batched
  (flush on `visibilitychange`/`pagehide` and every 5 s); capped payload.
- Server side: a bounded, sampled log/telemetry row per event class, joining on
  the auction id the server already logs. No KV writes, no PII, no cookies.
- The existing `recordRender` funnel becomes a producer for this beacon, so the
  in-page trace overlay and the server see the same stream.

`reason` enums are the contract: `renderer_endpoint_404`,
`renderer_ready_timeout`, `descriptor_invalid`, `bridge_id_mismatch`,
`gam_empty`, `no_render_source`, `slot_unresolved`, `gpt_absent`, and so on.
Every silent `return` found by the audit gets a reason code.

### 5.2 Surface the server's own drop counters

- Log `drop_reasons` at `warn` when an APS response yields zero admitted bids.
- Add `drop_reasons` to auction telemetry rows and to the `ts-debug` comment
  allowlist (SSAT and page-bids paths).
- Startup validation warning when `[integrations.aps]` is enabled while
  `allow_script_creatives = false`: "script-type APS demand will be dropped."
- Startup validation warning when APS (or any direct provider) is configured
  alongside a mediator, until Workstream 2 makes that combination meaningful.

**Exit criterion:** an operator can answer "which of the failure points in
section 2 is firing on this page" from server logs alone, with one page load.

---

## 6. Workstream 2 — APS delivery fixes

### 6.1 Admission

- **Mediated auctions must not discard direct-provider winners (A1).** New
  winner-merge policy: after the mediator responds, direct-provider bids
  compete per slot by decoded CPM against mediator bids under a configurable
  strategy: `mediator_only` (today's behavior, explicit), `merge_highest_cpm`
  (new default). The delivery report gains
  `dropped_winner_reasons["mediator_superseded"]` so the loser is visible.
- **Dimension tolerance (A3).** Replace exact `w`×`h` equality with a
  containment rule: an APS bid is admitted when its size fits within any
  configured format for the slot (never larger on either axis); the served size
  is reported in targeting. Exact match stays preferred when available.
- **Script creatives (A2).** Keep the secure default (`false`) but make the
  consequence loud (5.2) and document the enablement path for TAM-heavy
  publishers. The renderer sandbox already isolates script tag types; this is a
  policy toggle, not new machinery.

### 6.2 Render identity: one short token

Introduce a server-generated **render token** — 12 chars, `[a-z0-9]`, unique per
(auction, slot) — emitted as `hb_adid` for every SSAT bid and used as the key in
every registry and bridge branch:

- Well inside GAM's 40-char value limit and charset rules (B1).
- The bid map carries `{ hb_adid: token, bid_id, renderer, … }`; the bridge
  matches on the token; billing/win URLs keep using the real bid id.
- The client-side Prebid adapter path keeps Prebid's generated `adId` (that
  contract is Prebid's own), but registration for both paths lands in **one**
  registry keyed by whichever token the path will observe (B2).
- Property test: every emitted `hb_adid` matches `^[a-z0-9-]{1,40}$`.

### 6.3 Render source chain with a GAM-claim timeout

A winning bid becomes an ordered list of render sources:
`renderer → inline adm → pbs-cache`. The render engine walks the chain, emitting
`render_attempt` / `render_ok` / `render_fail{reason}` per step.

For flow (a)/(c), add the missing fallback (C1): when targeting was set for a
slot and **no bridge request arrives within N seconds of `slotRenderEnded`
(empty) or within M seconds of refresh**, and the config opts in
(`[auction].client_render_fallback = "renderer"`), render the descriptor
directly into the slot container via the existing `renderApsCreative` path. The
fallback is opt-in because it changes GAM reporting semantics; the beacon makes
the "GAM never asked" case visible either way.

### 6.4 One descriptor schema

The Rust `ApsRendererV1` struct becomes the single source of truth:

- `build.rs` (or a checked-in generation step) exports JSON Schema from the
  serde model; the TS types and validators in `aps/render.ts` and the inline
  renderer-document validator are **generated** from it.
- Validation becomes versioned-envelope tolerant: known fields validated
  strictly, unknown fields ignored, `version` gates behavior (C4).
- A conformance test round-trips a Rust-serialized descriptor through the TS
  validator and the renderer-document validator in CI.

### 6.5 Renderer endpoint availability

- Register the `/integrations/aps/renderer` route whenever the server can emit
  renderer bids (auction-level concern), not only when the APS integration is
  enabled on the serving origin (C2).
- The renderer iframe failure path (10 s timeout, load error) emits
  `render_fail{renderer_endpoint_404 | renderer_ready_timeout}` instead of
  dying silently.
- CSP audit (C6): extend `APS_RENDERER_CSP` with the minimum additional sources
  observed in real Amazon creative traffic (candidates: `frame-src data: blob:`,
  `worker-src blob:`), each addition justified in a comment and covered by the
  browser spec.

### 6.6 Bridge hardening

- Delete the dead duplicate renderer branch (C5) and move its dedup +
  debug-log into the live branch.
- Renderer branches call the same `fireWinBillingBeacons` +
  `recordGptBridgeRender` as the adm and cache branches (C7).
- Blanket source validation at the top of the bridge listener: parse and
  ownership-check before any branch logic; new branches inherit protection.
- SafeFrame-aware attribution (C3): resolve the slot by the MessageChannel port
  and the `hb_adid` token first (the token is already unique per slot), using
  the DOM walk only as a fallback.

### 6.7 Tests that pin the contract

1. Browser spec for flow (a): real GPT + PUC handshake driven from
   `window.tsjs.bids` with a renderer-only bid (the region the dead code hid).
2. Mediator + APS orchestration test asserting `merge_highest_cpm` admits the
   APS winner and `mediator_only` reports the drop.
3. `build_bid_map` tests: renderer emission, token-form `hb_adid`, adm and
   cache-coordinate suppression for renderer bids.
4. Cross-schema conformance (6.4).
5. Page-bids JSON carries `renderer`; SPA hook delivers it to the bridge.

---

## 7. Workstream 3 — TSJS target architecture

### 7.1 Layering

```
kernel/          boot, config, command queue, event bus, log, telemetry beacon
adapters/        googletag.ts, pbjs.ts, messaging.ts   ← the ONLY window.* access
services/        slots (registry+handoff), auction client, render engine, consent
integrations/    gpt, prebid, aps, creative, datadome, …  (plugins over services)
```

Rules, enforced by an eslint boundary rule in CI (`import/no-restricted-paths`):

- `kernel` imports nothing above it; `adapters` import kernel only; `services`
  import kernel + adapters; `integrations` import kernel + services, **never
  each other**.
- This dissolves today's inversions: `core/auction.ts` and `core/request.ts`
  importing `integrations/aps/render`, `gpt` and `prebid` importing `aps`, and
  `prebid` owning the GPT refresh wrapper.
- `aps/` gains a real module boundary (an `index.ts`), ending the triple
  inlining that gives three bundles three private copies of the frame-tracking
  WeakMaps (today two paths can each mount a live APS iframe on one container
  without seeing each other's cancel bookkeeping).

### 7.2 Adapters: explicit absence

Every external global is wrapped once with a tri-state
(`present | pending | absent`), a queue for `pending`, and a resolution
timeout that emits telemetry on `absent`. No other file touches
`window.googletag` / `window.pbjs`. This converts today's silent hangs (GPT
stub whose `cmd` never drains, `adInit` bare-returning without googletag) into
recorded, reasoned outcomes.

### 7.3 Slot registry service

One registry owns all slot knowledge: publisher-defined vs TS-defined,
adoption, handoff claims, responsive element resolution, refresh generation,
targeting-key history — keyed by `WeakMap<googletag.Slot, SlotRecord>` plus a
div-id index. Expando properties on live GPT objects are eliminated. The GPT
integration feeds events in and executes registry decisions; the prebid refresh
handler consumes the same registry instead of re-deriving slot resolution.

### 7.4 Global namespace policy: everything under `window.tsjs`

Today the library sprawls across the window: ten-plus `window.__tsjs_*` /
`window.__ts*` flags, `globalThis.tscreative` / `tsCreativeConfig`, a
symbol-keyed dispatcher, and expando properties stamped onto foreign objects
(`__tsPushed` on GPT's command queue, `__tsSlotHandoffPatched` on wrapped
functions, `__tsRenderGeneration` / `__tsRenderBid` on live GPT slot objects,
sentinels on `pbjs`). The policy going forward:

- **One owned global: `window.tsjs`**, split internally into `tsjs` (public,
  versioned API) and `tsjs._internal` (coordination state, explicitly not a
  contract). Server-injected boot flags (`__tsjs_gpt_enabled`,
  `__tsjs_slim_prebid_url`, bundle manifests) become fields the boot script
  sets via the same command-queue pattern (`window.tsjs = window.tsjs ||
{cmd: []}`), so early inline scripts and the bundle share one namespace.
- **No expandos on objects we do not own.** Per-slot state
  (`__tsRenderGeneration`, `__tsRenderBid`) moves into the slot registry's
  `WeakMap<googletag.Slot, SlotRecord>`; wrap-idempotence sentinels on foreign
  functions are replaced by a kernel-held `WeakSet` of wrapped targets.
- **Immediate cleanup, independent of the refactor:** `__tsRenderGeneration`
  and `__tsRenderBid` are dead writes on this baseline — written at
  `gpt/index.ts:1086-1091`, read by nothing (their consumer was lost in the
  #922 merge). Delete the writes now; when Phase 2 restores attribution, the
  captured bid/generation lives in `SlotRecord`.
- Third-party globals (`googletag`, `pbjs`, CMP APIs) are read only through
  adapters (7.2); integration-owned config globals (`didomiConfig`,
  `permutive.config`) are written only inside that integration's adapter
  boundary.

### 7.5 Messaging module

All `postMessage` traffic goes through one module: versioned envelopes, message
name constants (today `'Prebid Request'` appears as a bare literal at six
sites, and the APS handshake exists in three hand-synced copies), source and —
where origins are non-opaque — origin validation, and one audit point.

### 7.6 Lifecycle discipline

- **`install()` entry points instead of import-time side effects.** The server
  boot script calls `tsjs.install(['gpt', 'prebid', …])`; modules stop
  self-executing at module bottom. This kills the double-injection class
  (today: `beacon_guard` double-wraps `window.fetch` unrecoverably, a second
  `creative` copy silently disarms `setConfig`, `didomi` can throw during
  module evaluation and halt the concatenated bundle).
- One shared window-level install sentinel helper (the pattern
  `gpt_diagnostics` already got right), applied to every integration.
- A `PageSession` object owns all per-page mutable state; SPA navigation
  disposes and recreates it (fixing the leaked observers, listeners, and
  unbounded maps the audit enumerated).
- Error policy: no empty `catch` — every catch either handles, logs with
  context, or emits a disposition reason. The auction fetch gets a timeout +
  `AbortController`, and `requestAds` surfaces failure to its caller.
- **Console logging is retained, not replaced.** The disposition beacon is
  additive: every condition that surfaces an issue keeps (or gains) a
  `log.warn` with enough context to debug from an open DevTools console,
  because the console is the tool available on a publisher's page when no
  server access exists. Concretely: existing warnings survive the refactor
  verbatim or strengthened; failure paths currently logged at `debug` — which
  is invisible at the default `warn` level (for example the creative
  `dynamic_src_guard` and click-guard rejection paths) — are promoted to
  `warn` when they indicate a delivery or security-relevant failure; and every
  new `render_fail` / `absent`-dependency disposition emits a paired `warn`
  carrying the same reason code, so console and beacon tell one story.

### 7.7 The bootstrap problem

`gpt_bootstrap.js` duplicates ~400 lines of the hardest logic (handoff,
initial-load detection, hydration deferral) in hand-written ES5, always wins
the sentinel race, and has one live divergence (its simpler `adInit` can run
first and permanently suppress the bundle's `slotRenderEnded` listener).

Target: shrink the inline bootstrap to a **queue-and-flags stub only** (create
`googletag.cmd` interception points, record early publisher calls, expose
`__tsjs_gpt_enabled`), and move all behavior into the bundle, which replays the
recorded early calls on install. If a no-bundle fallback must keep rendering
ads (today's pinned behavior), that fallback is **generated from the same
TypeScript source** at build time, never hand-maintained.

### 7.8 GPT correctness fixes carried with the restructure

- Restore the #922 orphan-slot recovery and `updateRender` enrichment (verify
  against open PR #997; land whichever is canonical) — fixes the dead-element
  handoff alias (section 3.2) and trace double-counting.
- Pass `changeCorrelator: false` on TS-initiated refreshes; make correlator
  behavior a documented, configurable decision.
- `enableSingleRequest()` only when GPT services are not already enabled;
  otherwise adopt the publisher's mode and record it.
- Ambiguous responsive resolution emits `render_fail{slot_unresolved}` instead
  of only a console warning.

### 7.9 Decomposition targets

| Today                                | Target                                                                                                             |
| ------------------------------------ | ------------------------------------------------------------------------------------------------------------------ |
| `gpt/index.ts` (1777 LOC, 20 jobs)   | `slot_resolution`, `handoff`, `initial_load`, `ad_init`, `render_bridge`, `spa_navigation`, `beacons` + thin index |
| `prebid/index.ts` (1671 LOC)         | adapter, shim, refresh handler (moves onto slot registry), eids, diagnostics                                       |
| `gpt/script_guard.ts` (634 LOC)      | folded onto the 170-LOC `shared/script_guard.ts` factory the other six integrations already use                    |
| `core/trace.ts` (record model + UI)  | `services/trace` (model) + `integrations/trace_overlay` (UI)                                                       |
| `core/global.d.ts` (`pbjs: TsjsApi`) | real Prebid types; `TsjsApi` split into public API vs internal coordination state                                  |

---

### 7.10 Performance

Resilience must not cost speed; several parts of this design make the library
faster, and the rest are held to explicit budgets:

**Where the design is a speedup:**

- **Smaller synchronous bundle.** Consolidating `gpt/script_guard.ts` (634
  lines) onto the shared factory, un-inlining `aps/render.ts` from three
  bundles into one, deleting the dead bridge branch and dead expando writes,
  and splitting the trace overlay UI out of `core` all shrink the head-blocking
  `tsjs-unified.js`. Target: measurably smaller than today's bundle, tracked in
  CI (size report per PR).
- **Fewer repeated DOM walks.** Today slot resolution
  (`findSlotElementByDivId`'s five-step ladder, iframe walks in the bridge,
  prebid's independent re-derivation) runs per feature per pass. The slot
  registry resolves once per slot per navigation and everyone reads the record.
- **Bounded waits instead of blind ones.** The 10 s silent renderer timeout and
  the "queued forever on a GPT that never loads" cases become short, telemetered
  timeouts with fallbacks — failures surface in hundreds of milliseconds, and
  the render-source chain moves to the next source instead of waiting.

**Where the design must not regress, and how that is enforced:**

- **Ad request timing is untouched.** The critical path (bids script →
  targeting → display/refresh) gains no network calls and no awaits; adapter
  indirection is one property read and a queue check.
- **Telemetry is off the critical path by construction.** `sendBeacon` /
  keepalive fetch, batched, flushed on `visibilitychange` — never awaited by
  render code, capped in size and event count.
- **No new long tasks.** The kernel boots synchronously in microseconds
  (queue + registry creation); integration `install()` bodies do what their
  import-time footers do today, just at a controlled moment.
- **Budgets in CI:** bundle byte size per module, and a browser-spec assertion
  that time-from-bids-script-to-first-`display()` on the reference page does
  not regress against the recorded baseline.

### 7.11 Toolchain and dependency currency

The refactor starts from a current toolchain rather than dragging old versions
through it:

- **TypeScript to latest stable.** The library pins `typescript ^5.5.4` while
  the rest of the stack (vite 7, vitest 4, typescript-eslint 8) is current;
  upgrade TS first and adopt the newer strictness the refactor wants anyway
  (`noUncheckedIndexedAccess`, `exactOptionalPropertyTypes`,
  `verbatimModuleSyntax`) — these directly serve the typing goals in 7.9
  (killing `as unknown as` escapes and the wrong `global.d.ts` declaration).
- **Dev toolchain to latest stable:** eslint (+ plugins), prettier, jsdom,
  `@playwright/test` in the browser-test package, and `@types/node` aligned to
  the pinned Node in `.tool-versions`. Each bump lands as its own mechanical PR
  gated by the full CI matrix, with changelog review — this library
  monkeypatches globals (`fetch`, `sendBeacon`, DOM prototypes), so jsdom and
  Playwright behavior changes are real risks, not formalities.
- **`prebid.js` is deliberately excluded from casual bumps.** The runtime
  Prebid is the external R2 bundle, version-locked by manifest hash and SRI;
  the npm `prebid.js` dependency exists for tests and type
  references. Upgrading Prebid is its own coordinated deploy (bundle + config
  sha + server), per the decoupled-shim process — the spec only requires that
  the npm pin and the deployed bundle version stay documented together so
  tests exercise the version production runs.
- **Standing policy:** dependencies are reviewed on a monthly cadence and
  before each phase of this migration begins; a phase never starts on a
  toolchain more than one minor behind latest stable. Version floors live in
  `package.json` (exact or caret pins as today) and CI runs on the pinned
  Node/npm from `.tool-versions`.

## 8. Migration plan (phased, each phase independently shippable)

- **Phase 0 — Observability and toolchain.** Beacon + ingest route + server
  drop-reason surfacing + reason codes on today's silent returns. Toolchain
  currency (7.11): TypeScript and dev-dependency upgrades land here, before
  any structural change, so every later phase type-checks against the compiler
  it will ship with. No runtime behavior change.
- **Phase 1 — APS correctness.** Sections 6.1, 6.2, 6.5, 6.6 (admission,
  token, endpoint, bridge). Verified by Phase 0 data and the new tests. This
  phase alone should make APS render wherever configuration permits.
- **Phase 2 — GPT correctness.** Restore #922 attribution/orphan recovery
  (with #997), correlator, SRA guard. Render-source chain + opt-in direct
  fallback (6.3).
- **Phase 3 — Structure.** Layering + boundary lint, `install()` lifecycle,
  adapters, slot registry, messaging module, schema generation (6.4).
- **Phase 4 — Decomposition.** File splits, script-guard consolidation,
  bootstrap shrink. Pure moves under the existing vitest + browser specs,
  landed one module per PR.

Each phase gates on: all existing CI (Rust + JS + browser specs) green, plus
its own new tests; no phase depends on a later one.

---

## 9. Alternatives considered

1. **Keep patching APS point-failures without telemetry.** Rejected: three
   consecutive correct fixes have not produced ads; without disposition data
   the next fix is another guess.
2. **Direct-render APS always (skip GAM/PUC).** Simplest render path, but
   changes GAM reporting/pacing semantics unilaterally; kept as the opt-in
   fallback (6.3) instead.
3. **Full library rewrite in one branch.** Rejected: the browser-spec safety
   net is thin in exactly the areas being changed; phased extraction under
   tests is slower but survivable.
4. **Drop the ES5 bootstrap entirely (bundle-only).** Cleanest, but loses the
   pinned "ads still render if the bundle fails" guarantee; the
   generated-fallback approach (7.6) keeps that guarantee without the dual
   maintenance.

---

## 10. Risks

- **Mediator merge policy (6.1)** changes auction economics where a mediator is
  configured; mitigated by the explicit `mediator_only` strategy and the
  delivery-report visibility.
- **Beacon volume**: bounded by batching, sampling, and closed enums; the
  ingest route is fire-and-forget and cannot block rendering.
- **Schema generation** adds a build step; mitigated by checking generated
  artifacts into the tree and diffing them in CI.
- **Bootstrap shrink** touches the most load-order-sensitive code in the
  product; it is deliberately last (Phase 4) and behind the browser specs.

## 11. Success criteria

1. APS creatives render on a reference page in each configured flow (SSAT,
   Prebid adapter, page-bids), proven by browser specs and by disposition
   telemetry from a staged deployment.
2. Every failure point in section 2 maps to a distinct, observable signal
   (server log, telemetry row, or beacon reason).
3. `eslint` boundary rules pass with zero exceptions; no integration imports
   another integration; `core`/`kernel` imports no integration.
4. No file in `src/` exceeds ~500 lines; `gpt_bootstrap.js` is a stub or
   generated.
5. Trace counts are per-impression (no double counting), and orphaned-slot
   recovery is covered by a non-vacuous test.
6. The only TSJS-owned global is `window.tsjs`; no expando properties on GPT
   slots, GPT functions, or `pbjs`; the dead `__tsRenderGeneration` /
   `__tsRenderBid` writes are gone.
7. The synchronous bundle is no larger than today's (target: smaller), and the
   reference-page time-from-bids-script-to-first-`display()` does not regress.
8. No existing warning is lost: every issue-surfacing condition logs at `warn`
   or above in the console, with the same reason code the beacon carries.
9. TypeScript and the dev toolchain are on latest stable (with the new
   strictness flags enabled), `prebid.js`'s npm pin matches the documented
   deployed bundle version, and the monthly review policy is in CI docs.

## 12. Open questions

1. Is a mediator configured in the affected production deployment? (Decides
   whether A1 is the primary cause or a latent one.)
2. What share of live APS demand is `tagtype: "script"`? (Decides how urgent
   the `allow_script_creatives` enablement guidance is.)
3. Should the direct-render fallback (6.3) ever become default-on for
   publishers without GAM line items for `hb_bidder=aps`?
4. Is PR #997 the intended restoration of the lost #922 attribution core, or
   should the original be re-merged?
5. Beacon endpoint naming and retention: `/_ts/client-events` vs folding into
   the existing telemetry namespace.
