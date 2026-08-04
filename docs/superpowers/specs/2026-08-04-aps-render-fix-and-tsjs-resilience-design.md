# APS Render Fix and TSJS Resilience Architecture — Design

- **Status:** revision 2 — reworked after design review (review verdict:
  request changes). The five architectural contracts the review required are
  settled in section 4.
- **Date:** 2026-08-04
- **Baseline:** `rc/july` @ `541298695` — the full merged state (everything
  merged from `main` plus every rc-only merge), not just the delta pending
  against `main`.
- **Inputs:** three code audits performed against this baseline (APS end-to-end
  trace, TSJS architecture audit, GPT integration map); design review of
  revision 1; open issues #926, #941, #944, #962, #964, #977, #983, #989,
  #993; open PR #997.

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
- No behavior change for publishers whose pages work today. Where this design
  must migrate a public surface (section 7.4), it does so behind a bounded
  compatibility window, never by immediate removal.

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
| A3  | **Strict per-bid gates**: exact `w`×`h` membership in the slot's configured formats, required `ext.creativeurl`, and any top-level `contextual` key rejects the entire response.                                                                                               | `aps.rs:657-668`, `:745-778`, `:838-846`         |
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
| C2  | **A renderer endpoint that never answers is a silent 10-second death.** The sandboxed iframe cannot read an HTTP status from its opaque origin; a 404/401/misrouted document simply never posts `renderer-ready`.           | `aps.rs:1188-1245`, `aps/render.ts:384-404`               |
| C3  | **SafeFrame breaks slot attribution.** The bridge resolves a message source by walking top-document iframes under the slot div; a nested SafeFrame creative window is invisible to that walk, so the bridge bails silently. | `gpt/index.ts:157-183`, `:1599-1600`                      |
| C4  | **Three hand-maintained copies of the descriptor schema** (Rust struct, TS validator, inline renderer-document validator) with exact-key rejection: any server-side field addition instantly blanks every APS ad.           | `types.rs:188-211`, `aps/render.ts:60-93`, `aps.rs:65-73` |
| C5  | **A dead duplicate renderer branch in the bridge** contains the debug log and dedup logic people would look for while debugging; it can never execute.                                                                      | `gpt/index.ts:1643-1674`                                  |
| C6  | **The renderer CSP may kill creatives after success is reported** (`object-src`, workers, `blob:`/`data:` frames are blocked; `renderer-ready` fires before the creative actually paints).                                  | `aps.rs:49`                                               |
| C7  | **The renderer branches record nothing**: no `recordRender`, no win/billing beacons, no `stampCreativeTrace` — an APS win looks "never rendered" in every trace whether or not it painted.                                  | `gpt/index.ts:1558-1632`                                  |

### 2.4 Observability: the common factor

There is **zero client→server reporting**. Server telemetry marks `is_win=1` at
auction time and goes quiet; a bid that never painted is byte-identical to one
that painted perfectly. Client-side evidence (`window.tsjs.renders`, console
warnings, `data-ts-*` attributes) dies with the tab.

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

---

## 4. Design gates — the five contracts settled before implementation

The design review identified five architectural contracts every later section
depends on. This revision settles them; changing any of these later reopens the
review.

### G1 — Trace identity and event envelope

**The client-visible auction id must never be ingested.** It is EC-derived by
construction (`publisher.rs:3237`: `ts-{ec_id}` when an EC exists), and server
telemetry already uses a deliberately unrelated fresh UUID
(`telemetry.rs:94-97`), so a join on it is both a privacy violation and
impossible.

Contract:

- A **`trace_id`** is minted client-side per navigation: 128-bit CSPRNG
  (`crypto.getRandomValues`), hex-encoded, held in `PageSession`, never
  persisted, never derived from any identity.
- Every event carries the envelope `{trace_id, nav_gen, render_gen, seq}`.
  `seq` is a per-trace monotonic counter (ordering + deduplication);
  `nav_gen`/`render_gen` scope events to a navigation and a refresh cycle, so a
  batch that spans SPA navigations remains attributable per event, not per
  batch.
- The **server** stamps the same `trace_id` into its own telemetry rows by
  reading it from the beacon, never the reverse: server auction rows keep their
  independent telemetry UUID, and correlation happens only where the client
  reported a `trace_id` alongside the events it observed. Failures with no
  winning bid therefore still have a trace: the trace is client-born, not
  auction-born.
- **Sampling is sticky per trace** (decided once at trace mint, recorded in the
  envelope), never per event — a sampled trace is complete or absent.

### G2 — Render identity: `hb_adid`, the APS token, and the PBS Cache UUID

**The PBS Cache contract stays untouched.** Today `hb_adid` deliberately
prefers the Prebid Cache UUID (`publisher.rs:3355`), and both the emitted cache
coordinates and the bridge's cache fetch assume `?uuid=<hb_adid>`
(`publisher.rs:3450`, `gpt/index.ts:1700`) — that is the Prebid Universal
Creative's documented contract. Revision 1's "token for every SSAT bid" would
have broken every cache-backed render and is withdrawn.

Contract:

- Bids with a cache id: `hb_adid` = cache UUID, exactly as today.
- Bids with markup and no cache id: `hb_adid` = existing fallback chain
  (`ad_id`, then bid id), exactly as today.
- **Renderer-only bids (APS): `hb_adid` = a server-minted render token**,
  format `^[a-z0-9]{12}$` (12 chars exactly), CSPRNG-generated with collision
  retry within the auction, unique across slots and auctions, one-time
  consumption in the bridge registry, TTL-bounded. Twelve characters sit well
  inside GAM's documented 40-character targeting-value limit.
- The client-side Prebid adapter path keeps Prebid's generated `adId` (that is
  Prebid's own contract); both paths register into one bridge registry keyed by
  whichever id that path will observe.
- Regression tests: non-APS cache-backed bids keep byte-identical `hb_adid`
  and cache coordinates; the token property test asserts `^[a-z0-9]{12}$`,
  uniqueness, one-time consumption, and TTL expiry.

### G3 — Runtime ABI: how code shares state under the IIFE build

Every entry point is built as a self-contained IIFE with dynamic imports
inlined (`build-all.mjs:46`), and the server concatenates already-closed IIFEs
(`bundle.rs:23`). **A module `import` therefore never shares state across
bundles** — an `aps/index.ts` alone would just mint another private copy. This
is already a live defect beyond APS: `core/context.ts:11` holds a private
context-provider `Map` while `permutive/index.ts:102` registers into its own
independently bundled copy.

Contract — **a versioned registration ABI on `window.tsjs._internal`**:

- The kernel ships **only** in `tsjs-core` (always first in the concatenated
  unified bundle) and publishes `tsjs._internal = { abi: 1, registry }` exactly
  once, guarded by a window-level sentinel.
- All **stateful** services (slot registry, render state machine, APS renderer
  registry, context providers, event bus, beacon queue) exist once, owned by
  the kernel, and are reached **only** through
  `tsjs._internal.registry.get(name, minVersion)` at call time — never through
  imported module state. Pure stateless helpers may still be imported and
  inlined freely.
- Deferred bundles (prebid) and later scripts interact through the same ABI;
  `abi` majors are checked at lookup and a mismatch is a logged, telemetered
  refusal, not a silent no-op.
- The alternative (single module graph / shared chunks emitted by the build and
  loaded as real modules) is recorded as the long-term option; the ABI is
  chosen now because it works under today's concatenation and deferred
  loading without changing the delivery pipeline.
- This gate precedes and unblocks: the event bus, the slot registry, the
  install registry, `PageSession`, the messaging service, and fixes the
  context-provider split as its first proof.

### G4 — Exactly-once render state machine, and honest success semantics

**Absence of a bridge request is not proof GAM failed** — GAM may legitimately
serve a different, higher-priority creative without ever invoking the TS
bridge. And **`renderer-ready` means Amazon's runner script loaded, not that an
ad painted** (`aps.rs:105`); the current direct renderer returns `true`
immediately while the real outcome settles asynchronously for up to ten
seconds (`render.ts:318`).

Contract:

- One render state machine per placement instance, keyed by
  `(trace_id, nav_gen, slot, refresh_gen, render_token)`, with exactly-once
  terminal transition. States:
  `targeting_set → bridge_claimed | gam_filled_other | fallback_running →
render_accepted → render_confirmed | render_failed(reason)`.
- **The fallback never replaces a nonempty GAM render.** Any bridge claim or
  nonempty `slotRenderEnded` cancels a pending fallback; an **empty**
  `slotRenderEnded` may trigger it; navigation, refresh, handoff, and slot
  destruction invalidate stale attempts.
- The renderer API becomes awaitable with cancellation and an explicit
  terminal reason; no fire-and-forget `true`.
- Event taxonomy replaces the single `render_ok`:
  - `bridge_response_sent` — the bridge handed a payload to the PUC;
  - `render_accepted` — the renderer document loaded Amazon's runner
    (today's "ready");
  - `render_confirmed` — observed evidence of a paint (nonempty
    `slotRenderEnded` for GAM fills; for the sandboxed renderer, iframe
    load + non-collapsed geometry where observable).
- **Billing/win callbacks fire no earlier than `render_accepted`, and
  `render_confirmed` is the only state reported as success.** Where APS
  provides no paint acknowledgement, the design says so plainly: telemetry can
  distinguish "runner loaded" from "nothing loaded," but **cannot distinguish
  a painted ad from a blank one** inside the opaque frame — rows terminate at
  `render_accepted` and are labeled as such, not upgraded.

### G5 — Deployment contracts

- **Config rollback:** every new auction/config field is default-valued and
  **omitted from serialization at its default** (`AuctionConfig` denies
  unknown fields, `auction_config_types.rs:7`), so blobs written by a new
  binary remain readable by the previous one unless an operator opts in.
- **Ingest routing:** the beacon route exists in **all four adapters**
  (Fastly, Axum, Cloudflare, Spin) as an early, EC-free, filter-free route.
  Only Fastly has a real telemetry sink today; the no-sink behavior elsewhere
  is "accept, count, drop" — defined, not accidental.
- **Storage:** a new dedicated datasource for client events (the existing
  auction datasource cannot hold the shape without migration); schema,
  retention, and sampling documented with the datasource definition.
- **Asset content-addressing is fixed, not assumed.** Script URLs embed a
  content hash but the handler ignores the query and serves current registry
  bytes from the path alone (`tsjs.rs:3`, `publisher.rs:294`) — during a
  rolling deploy an old `?v=A` request can receive bytes `B` and cache them
  under `A`. The handler must validate the requested hash and serve the
  matching immutable artifact (or answer with the current hash's redirect);
  rolling-deploy cache tests pin this.
- **Phase ordering** follows section 9; every phase ships behind a feature
  flag with canary thresholds and rollback criteria.

---

## 5. Workstream 1 — Observability

### 5.1 Client disposition beacon

A kernel module batches disposition events to `POST /_ts/client-events`:

```
{ v: 1, trace: {trace_id, sampled}, events: [
  { seq, nav_gen, render_gen, t: "bid_received",         slot, bidder, source }
  { seq, nav_gen, render_gen, t: "targeting_set",        slot, hbAdid }
  { seq, nav_gen, render_gen, t: "bridge_request",       slot, adId, matched }
  { seq, nav_gen, render_gen, t: "bridge_response_sent", slot, source }
  { seq, nav_gen, render_gen, t: "render_attempt",       slot, source }
  { seq, nav_gen, render_gen, t: "render_accepted",      slot, source }
  { seq, nav_gen, render_gen, t: "render_confirmed",     slot, source }
  { seq, nav_gen, render_gen, t: "render_fail",          slot, source, reason }
] }
```

- **Transport:** `fetch(..., {keepalive: true, credentials: "omit"})` is the
  primary transport, because `sendBeacon` always sends credentials and its
  `true` return only means "queued," not "received." `sendBeacon` remains the
  documented last-resort fallback on `pagehide` where keepalive is
  unavailable, and the ingest handler ignores credentials in all cases.
- **Batching:** flush on `visibilitychange`/`pagehide` and every 5 s; each
  event self-describes its navigation via the envelope, so batches spanning
  SPA navigations stay attributable.
- **Reasons are a closed enum**, structurally serialized (never interpolated
  into log lines): `renderer_no_ready`, `descriptor_invalid`,
  `bridge_id_mismatch`, `gam_empty`, `gam_filled_other`, `no_render_source`,
  `slot_unresolved`, `gpt_absent`, `pbjs_absent`, `bundle_partial`,
  `fallback_cancelled`, `timeout`. `renderer_no_ready` (not
  `renderer_endpoint_404`) is deliberate: the opaque iframe cannot read an
  HTTP status, so the observable fact is "no ready message before timeout."

### 5.2 Ingest contract

- Registered in all four adapters before auth/EC/filters (G5); same-origin
  enforced via `Origin`/`Sec-Fetch-Site` checks; hard caps on body bytes,
  event count, and string lengths **enforced before parsing or logging**;
  malformed rows dropped and counted; per-IP rate limiting; responds
  `204 Cache-Control: no-store`; touches no KV and mints no identity.
- Server logs a bounded, structured summary per batch; the Fastly sink writes
  to the new datasource (G5); other adapters count-and-drop until a sink
  exists.

### 5.3 Two modes, honestly separated

A sampled, best-effort beacon **cannot** guarantee a complete diagnosis from
one page load — so the design stops claiming it:

- **Production telemetry:** sticky-sampled traces, SLO "a delivery failure
  mode occurring on ≥ N% of impressions is visible in the datasource within
  one hour."
- **Diagnostic mode:** explicitly enabled (tester cookie / query flag),
  unsampled, full event stream plus console mirroring — this is the "one page
  load tells you which failure fired" tool.

### 5.4 Server-side drop-reason surfacing

- Emit a bounded structured summary **whenever any bid is dropped** (not only
  when zero survive): per-slot reason counts, capped.
- Add `drop_reasons` to auction telemetry rows.
- The initial-HTML `ts-debug` comment gains the drop summary; `/_ts/page-bids`
  returns JSON and **cannot carry an HTML comment**, so it gains a gated
  structured `debug` field instead, enabled by the same tester gate.
- Startup validation warnings: APS enabled while `allow_script_creatives =
false` ("script-type APS demand will be dropped"); any direct provider
  configured alongside a mediator without the merge strategy of 6.1 ("provider
  bids cannot win as configured").

---

## 6. Workstream 2 — APS delivery fixes

### 6.1 Mediation: opt-in merge, `mediator_only` stays the default

The configured contract today is explicit — the mediator is the final
decision-maker (`auction_config_types.rs:48`) — and changing that default
silently would be an economic breaking change with a rollback hazard. So:

- `[auction].winner_selection = "mediator_only"` (default, today's behavior,
  now explicit) or `"merge_highest_cpm"` (opt-in). The field is omitted from
  serialized blobs at its default (G5).
- `merge_highest_cpm` semantics, defined up front: comparison in the auction's
  decoded CPM currency; slot floors apply to both populations; a bid present
  in both (a provider bid the mediator also returned) counts once, by
  provenance `mediator`; deals outrank open bids regardless of CPM; ties break
  to the mediator; a mediator timeout degrades to direct-provider selection
  (today's short-circuit) and is reported as such.
- **One candidate-selection helper serves both mediation lifecycles** — the
  ordinary/page-bids path (`orchestrator.rs:412-431`) and the initial-SSAT
  split dispatch/collect path (`orchestrator.rs:1320`) — with tests for each.
- A **selection report** (`winner_source`, `mediator_superseded` counts) is
  emitted separately from the delivery-conversion drop reasons, so "lost the
  merge" is never conflated with "failed to serialize."

### 6.2 Dimensions: exact membership stays; flexibility is operator-declared

Revision 1's containment rule ("never larger on either axis") would still
reject its own motivating example (a 300×600 answer on a 300×250/728×90 slot)
while admitting pathological 1×1 sizes — withdrawn. Instead:

- Exact size membership (`aps.rs:657-668`) remains the default; it is
  consistent with discrete GAM slot formats.
- An operator may declare flexibility per slot:
  `accept_sizes = [[300, 600], …]` (an explicit allow-list of additional
  creative sizes, with documentation that GAM line items must accept them) —
  no inference, no aspect heuristics in v1. Admitted alternate sizes set
  `hb_size` targeting (set and cleared with the other `hb_*` keys) and the
  served size is reported in the selection report.

### 6.3 Script creatives

Keep the secure default (`allow_script_creatives = false`) but make the
consequence loud (5.4) and document the enablement path for TAM-heavy
publishers. The renderer sandbox already isolates script tag types; this is a
policy toggle, not new machinery.

### 6.4 Render identity

Implemented exactly as G2: cache UUID untouched, render token only for
renderer-only bids, one registry, token property tests
(`^[a-z0-9]{12}$`, CSPRNG, collision retry, TTL, one-time consumption,
cross-slot/auction uniqueness), and a non-APS cache-path regression test.

### 6.5 Fallback rendering

Implemented exactly as G4: opt-in
(`[auction].client_render_fallback = "renderer"`), driven by the render state
machine, triggered only by an **empty** GAM render or a bridge-claim timeout
with no fill evidence, cancelled by any bridge claim or nonempty render,
invalidated by navigation/refresh/destruction, and reported with the G4 event
taxonomy. The direct renderer is converted to an awaitable API first; the
fallback lands only after that conversion.

### 6.6 Renderer endpoint

- Document the topology: within one deployment the renderer route and the APS
  provider share the same config gate (`aps.rs:1224`, `:1244`), so "server
  emits descriptors but lacks the route" is a **cross-deployment or stale-CDN
  problem**, and a 401 most plausibly comes from broad `[[handlers]]` auth
  patterns matching `/integrations/*` before route dispatch.
- Therefore: validate at startup that no configured auth handler pattern
  covers `/integrations/aps/renderer`; make the static renderer document
  config-independent **iff** the deployment serves multiple origins from one
  config (recorded as an open question with the operator); version the
  renderer document and define its cache headers.
- The client reports `renderer_no_ready` (5.1) — no status probe is added,
  because a probe would violate the no-new-critical-path-request budget.
- CSP audit (C6) unchanged from revision 1: extend `APS_RENDERER_CSP` only
  with sources observed in real Amazon traffic, each justified in a comment
  and covered by the browser spec.

### 6.7 One descriptor schema — structural generation, semantic validators kept

- The wire truth is the **tagged enum** `BidRenderer` (the `type: "aps"`
  discriminator lives there, not on `ApsRendererV1` — `types.rs:188-211`), so
  the generated schema is the full tagged envelope.
- Generation lives in a **separate wire-schema crate** (or a host-side
  xtask) — it cannot live in `trusted-server-js`'s build because core already
  depends on that crate (`Cargo.toml:45`) and the reverse edge would be a
  cycle. Generated TS artifacts are checked in; CI fails on staleness.
- Generation covers **structure only** (fields, types, discriminator,
  version). The semantic security checks stay hand-written on both sides:
  URL/origin policy, canonical base64, length bounds, the exact one-bid
  envelope projection, cross-field equality. **Unknown-field tolerance applies
  only to the outer versioned descriptor; the decoded AAX envelope remains an
  exact projection** so `adm`, notification URLs, or sibling fields can never
  slip through unexamined.
- A shared corpus — positive cases plus an adversarial set (extra fields,
  wrong versions, oversized payloads, URL smuggling, non-canonical base64) —
  runs through the Rust validator, the TS validator, and the inline renderer
  document in CI.

### 6.8 Bridge hardening

- **Ownership proof stays source-first.** The bridge continues to resolve the
  message source to a slot and only then compares that slot's expected id
  (`gpt/index.ts:1599` order) — a MessageChannel port plus a token is not
  ownership proof, because an inbound port has no pre-established slot
  identity and the token is visible in `window.tsjs.bids`. For SafeFrame,
  source resolution is extended to walk nested browsing contexts via
  `window.frames` containment checks rather than DOM `querySelectorAll` only;
  where the source is unresolvable the bridge refuses (with
  `bridge_id_mismatch`) instead of trusting the token.
- Adversarial tests are part of the contract: wrong-slot tokens, nested
  foreign frames, replayed and duplicated messages, stolen tokens, and
  previous-navigation tokens — not only the positive SafeFrame case.
- Blanket top-of-listener hygiene: parse and ownership-check before any
  branch logic; new branches inherit protection.
- Delete the dead duplicate renderer branch (C5); move its dedup and debug log
  into the live branch.
- Renderer branches emit the same trace records as the adm and cache branches,
  under G4's event taxonomy and billing rules (C7).

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
- Stateful services are reached through the G3 registration ABI, never through
  imported module state; stateless helpers may be imported and inlined.
- This dissolves today's inversions: `core/auction.ts` and `core/request.ts`
  importing `integrations/aps/render`, `gpt` and `prebid` importing `aps`, and
  `prebid` owning the GPT refresh wrapper.

### 7.2 Adapters: explicit absence, without giving up on late loaders

Every external global is wrapped once with a state machine
`present | pending | timed_out`, a queue for `pending`, and per-operation
bounds. `timed_out` is **not terminal**: publishers legitimately lazy-load
GPT, Prebid, and CMPs, so a later arrival transitions the adapter to `present`
and drains what is still valid; individual queued operations carry their own
timeouts and expire with a disposition reason rather than the adapter
permanently disabling itself.

### 7.3 Slot registry service

One registry owns all slot knowledge: publisher-defined vs TS-defined,
adoption, handoff claims, responsive element resolution, refresh generation,
targeting-key history — keyed by `WeakMap<googletag.Slot, SlotRecord>` plus a
div-id index, owned by the kernel via the G3 ABI. Expando properties on live
GPT objects are eliminated. The GPT integration feeds events in and executes
registry decisions; the prebid refresh handler consumes the same registry.

### 7.4 Global namespace policy — with a compatibility window

One owned global, `window.tsjs`, public API versioned, coordination state
under `tsjs._internal` (G3). But the migration must not break published
contracts:

- **Inventory first:** every current global is classified public
  (`globalThis.tscreative`, `tsCreativeConfig` — documented, settable
  pre-load) or private (`__tsjs_*` flags, expandos, sentinels).
- **Public globals get a bounded dual-read/write window:** old and new names
  both work for a stated deprecation period, with pre-init compatibility tests
  (config set before the bundle loads must keep working); removal is its own
  later, announced change.
- Private globals migrate immediately: per-slot expandos
  (`__tsRenderGeneration`, `__tsRenderBid` — dead writes today, delete now)
  into `SlotRecord`; function-object sentinels into a kernel-held `WeakSet`;
  boot flags into the `window.tsjs = window.tsjs || {cmd: []}` pattern.

### 7.5 Messaging module

All `postMessage` traffic goes through one module: versioned envelopes, message
name constants (today `'Prebid Request'` appears as a bare literal at six
sites, and the APS handshake exists in three hand-synced copies), source
validation per 6.8, and one audit point.

### 7.6 Plugin lifecycle

`install()` replaces import-time side effects, with the semantics the review
demanded:

- `tsjs.definePlugin(id, version, install, dispose)` registers; the kernel
  resolves install requests against registrations, so **a deferred bundle that
  registers after `tsjs.install([...])` was requested is installed on
  arrival** (pending-install queue), bounded by a missing-module timeout that
  emits `bundle_partial`.
- Per-plugin exception isolation: a plugin that throws during install is
  quarantined and reported; it cannot halt the bundle (today `didomi` can
  throw during module evaluation and stop everything after it).
- Duplicate registration of the same `(id, version)` is a no-op; a different
  version for a registered id follows a declared policy (first-wins + loud
  telemetry).
- A `PageSession` object owns an **enumerable** set of listeners, timers,
  observers, and slot records — registered at creation, disposed on
  navigation. A `WeakMap` alone cannot dispose anything; the owned-set is the
  disposal inventory.
- Error policy: no empty `catch` — every catch handles, logs with context, or
  emits a disposition reason. The auction fetch gets a timeout +
  `AbortController`, and `requestAds` surfaces failure to its caller.
- **Console logging is retained, not replaced.** The beacon is additive: every
  issue-surfacing condition keeps (or gains) a `log.warn` debuggable from an
  open DevTools console. Existing warnings survive verbatim or strengthened;
  failure paths currently at `debug` (invisible at the default `warn` level —
  the creative `dynamic_src_guard` and click-guard rejection paths) are
  promoted to `warn` when they indicate a delivery or security-relevant
  failure; every `render_fail` / dependency-timeout disposition emits a paired
  `warn` carrying the same reason code, so console and beacon tell one story.

### 7.7 The bootstrap problem

`gpt_bootstrap.js` duplicates ~400 lines of the hardest logic (handoff,
initial-load detection, hydration deferral) in hand-written ES5, always wins
the sentinel race, and has one live divergence (its simpler `adInit` can run
first and permanently suppress the bundle's `slotRenderEnded` listener).

Target: shrink the inline bootstrap to a queue-and-flags stub (create
`googletag.cmd` interception points, record early publisher calls, expose the
enable flag), with the bundle replaying recorded calls on install. This is
**not a pure move** — replay changes observable ordering — so it ships behind
its own flag with the browser specs extended to cover replay timing, and the
no-bundle fallback ("ads still render if the bundle fails," pinned by
`gpt.rs:1174-1179`) is **generated from the same TypeScript source** at build
time, never hand-maintained.

### 7.8 GPT correctness fixes carried with the restructure

- Restore the #922 orphan-slot recovery and `updateRender` enrichment (verify
  against open PR #997; land whichever is canonical).
- Pass `changeCorrelator: false` on TS-initiated refreshes; correlator
  behavior becomes a documented, configurable decision.
- `enableSingleRequest()` only when GPT services are not already enabled;
  otherwise adopt the publisher's mode and record it.
- Ambiguous responsive resolution emits `render_fail{slot_unresolved}` in
  addition to its console warning.

### 7.9 Decomposition targets

| Today                                | Target                                                                                                             |
| ------------------------------------ | ------------------------------------------------------------------------------------------------------------------ |
| `gpt/index.ts` (1777 LOC, 20 jobs)   | `slot_resolution`, `handoff`, `initial_load`, `ad_init`, `render_bridge`, `spa_navigation`, `beacons` + thin index |
| `prebid/index.ts` (1671 LOC)         | adapter, shim, refresh handler (moves onto slot registry), eids, diagnostics                                       |
| `gpt/script_guard.ts` (634 LOC)      | folded onto the 170-LOC `shared/script_guard.ts` factory the other six integrations already use                    |
| `core/trace.ts` (record model + UI)  | `services/trace` (model) + `integrations/trace_overlay` (UI)                                                       |
| `core/global.d.ts` (`pbjs: TsjsApi`) | real Prebid types; `TsjsApi` split into public API vs internal coordination state                                  |

### 7.10 Performance

Client-side, the design is a net speedup with enforced budgets:

- **Smaller synchronous bundle:** script-guard consolidation, single APS
  module (via the ABI), dead-code deletion, trace-overlay extraction.
- **Fewer repeated DOM walks:** slot resolution once per navigation in the
  registry.
- **Bounded waits instead of blind ones:** the 10 s silent renderer timeout
  and forever-queued GPT cases become short, telemetered timeouts.
- **Budgets in CI, precisely specified:** per-bundle byte sizes measured raw,
  gzip, and Brotli for an exact named module set, compared against a
  checked-in baseline artifact with a stated tolerance; the browser-spec
  timing assertion (bids-script-to-first-`display()`) runs N times and gates
  on a percentile, not a single sample.

Server-side (new in this revision): each injected page currently concatenates
and hashes the full immediate bundle, and the asset request concatenates it
again (`bundle.rs:51`). Precompute bundle bytes + hash per registry module set
(they change only at deploy/config time), and benchmark server CPU/heap before
and after.

### 7.11 Toolchain and dependency currency

- **TypeScript to latest stable** (library pins `^5.5.4` while vite 7 /
  vitest 4 / typescript-eslint 8 are current), adopting
  `noUncheckedIndexedAccess`, `exactOptionalPropertyTypes`,
  `verbatimModuleSyntax` — these directly serve killing the `as unknown as`
  escapes and the wrong `global.d.ts` declaration.
- **Dev toolchain to latest stable** (eslint + plugins, prettier, jsdom,
  `@playwright/test`, `@types/node` aligned to the pinned Node), each bump its
  own mechanical CI-gated PR with changelog review — this library
  monkeypatches `fetch`, `sendBeacon`, and DOM prototypes, so jsdom and
  Playwright behavior changes are real risks.
- **`prebid.js` is excluded from casual bumps:** the runtime Prebid is the
  external R2 bundle locked by manifest hash and SRI; upgrading it is its own
  coordinated deploy. The npm pin and the deployed bundle version stay
  documented together so tests exercise the version production runs.
- **Standing policy:** monthly dependency review; no migration phase starts
  more than one minor behind latest stable.

---

## 8. Migration plan

Reordered so every phase's prerequisites precede it; each phase ships behind a
feature flag with canary thresholds and rollback criteria.

- **Phase 0 — Contracts and toolchain.** Settle G1–G5 in code-adjacent docs;
  toolchain upgrades (7.11); the trace envelope + beacon + four-adapter ingest
  (accept-count-drop outside Fastly) behind a flag; server drop-reason
  surfacing (5.4); reason codes on today's silent returns; delete the dead
  expando writes. No runtime behavior change for pages with the flag off.
- **Phase 1 — Runtime ABI + APS admission/identity.** G3 kernel registry in
  `tsjs-core` (context-provider fix is the proof); wire-schema crate + shared
  corpus (6.7); mediation selection helper + opt-in merge (6.1); render token
  for renderer-only bids (6.4); renderer-endpoint startup validation (6.6);
  bridge hardening minus fallback (6.8). APS renders after this phase wherever
  GAM line items and configuration permit — the no-GAM fallback is explicitly
  Phase 2.
- **Phase 2 — Render state machine + GPT correctness.** Minimal `SlotRecord`
  core (just enough for the state machine keys; full registry lands in
  Phase 3); awaitable renderer conversion; the exactly-once fallback (6.5,
  G4); restore #922/#997 attribution and orphan recovery; correlator and SRA
  fixes (7.8).
- **Phase 3 — Structure.** Full layering + boundary lint, plugin lifecycle +
  `PageSession` (7.6), adapters (7.2), full slot registry (7.3), messaging
  module (7.5), namespace migration with its compatibility window (7.4),
  asset content-addressing fix + server bundle precompute (G5, 7.10).
- **Phase 4 — Decomposition.** File splits (7.9), script-guard consolidation,
  bootstrap shrink behind its own flag with replay-timing specs (7.7), and
  the end of the public-global compatibility window.

---

## 9. Test acceptance matrix

Blocking CI is hermetic (the deterministic PUC/message harness); a separate
staged smoke suite covers real GAM line items and is release-gating, not
PR-gating.

| Area             | Must cover                                                                                                                                                                      |
| ---------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Mediation        | both lifecycles (ordinary + SSAT split dispatch/collect); ties, floors, deals, duplicate demand; `mediator_only` default preserved; rollback blob round-trip (omitted defaults) |
| Cache identity   | non-APS cache-backed bids: byte-identical `hb_adid` + cache coordinates (regression); PUC `?uuid=` fetch path                                                                   |
| Render token     | `^[a-z0-9]{12}$`; CSPRNG source; collision retry; TTL; one-time consumption; cross-slot/auction uniqueness                                                                      |
| Fallback         | races vs bridge claim; nonempty-GAM protection; repeated refresh; SPA navigation cancellation; slot destruction; exactly-once terminal transition                               |
| Render semantics | no billing after runner-load failure; `render_accepted` vs `render_confirmed` labeling; opaque-frame honesty (no painted/blank claim)                                           |
| Bridge security  | wrong-slot tokens; nested foreign frames; replayed + duplicate messages; stolen tokens; previous-navigation tokens; SafeFrame positive case                                     |
| Beacon           | trace joins; ordering/dedup via `(trace_id, nav_gen, seq)`; batching across navigations; loss tolerance; ingest abuse (oversize, malformed, cross-origin); all-adapter routing  |
| Schema           | generated-artifact staleness; adversarial corpus through Rust + TS + inline validator; outer-tolerance vs exact AAX projection                                                  |
| Runtime ABI      | one kernel instance under concatenation; deferred registration after install request; per-plugin failure isolation; abi-version mismatch refusal                                |
| Lifecycle        | late-loaded GPT/pbjs (`timed_out → present`); `PageSession` disposal inventory; pre-init `tsCreativeConfig` compatibility; dual-name global window                              |
| Delivery         | immutable-cache behavior across a simulated rolling deploy; deterministic raw/gzip/Brotli bundle budgets vs baseline artifact                                                   |
| Observability    | drop-reason summary on any partial drop; page-bids structured `debug` field gating; diagnostic mode unsampled completeness                                                      |

---

## 10. Alternatives considered

1. **Keep patching APS point-failures without telemetry.** Rejected: three
   consecutive correct fixes have not produced ads; without disposition data
   the next fix is another guess.
2. **Direct-render APS always (skip GAM/PUC).** Simplest render path, but
   changes GAM reporting/pacing semantics unilaterally; kept as the opt-in,
   state-machine-guarded fallback instead.
3. **Single module graph / shared chunks instead of the registration ABI.**
   Cleaner long-term, but changes the delivery pipeline (chunk loading) now;
   recorded as the successor option behind the same ABI surface.
4. **Full library rewrite in one branch.** Rejected: the browser-spec safety
   net is thin in exactly the areas being changed.
5. **Drop the ES5 bootstrap entirely.** Loses the pinned "ads render if the
   bundle fails" guarantee; the generated-fallback approach keeps it without
   dual maintenance.

## 11. Risks

- **Merge strategy misconfiguration** changes auction economics; mitigated by
  keeping `mediator_only` the default, the selection report, and omitted
  serialization at defaults.
- **Beacon abuse/volume:** bounded by pre-parse caps, origin checks, rate
  limits, sticky sampling, and closed enums.
- **ABI freeze risk:** `tsjs._internal.registry` becomes load-bearing;
  versioned from day one, majors checked at lookup.
- **Bootstrap replay** changes observable ordering; own flag, replay-timing
  specs, staged rollout.
- **Schema generation** adds a build step; checked-in artifacts + staleness CI.

## 12. Success criteria

1. APS creatives render on a reference page in each configured flow (SSAT,
   Prebid adapter, page-bids), proven hermetically in CI and by the staged
   smoke suite against real GAM line items.
2. Every failure point in section 2 maps to a distinct observable signal, and
   **diagnostic mode** yields the failing reason from one page load; production
   telemetry meets the stated SLO (5.3).
3. `eslint` boundary rules pass with zero exceptions; no integration imports
   another integration; stateful sharing goes through the versioned ABI.
4. No file in `src/` exceeds ~500 lines; `gpt_bootstrap.js` is a stub or
   generated.
5. Trace counts are per-impression (no double counting), and orphaned-slot
   recovery is covered by a non-vacuous test.
6. The only TSJS-owned global is `window.tsjs` (public globals only inside
   their announced compatibility window); no expandos on GPT slots, GPT
   functions, or `pbjs`.
7. Per-bundle raw/gzip/Brotli sizes are at or below the checked-in baseline
   within stated tolerance, and the percentile-based
   bids-script-to-first-`display()` assertion does not regress; server-side
   per-request bundle concatenation/hashing is precomputed.
8. No existing warning is lost: every issue-surfacing condition logs at `warn`
   or above with the same reason code the beacon carries.
9. TypeScript and the dev toolchain are on latest stable with the new
   strictness flags; `prebid.js`'s npm pin matches the documented deployed
   bundle version; the monthly review policy is in CI docs.
10. Rolling-deploy cache tests pass: a `?v=A` request never caches bytes other
    than `A`.

## 13. Open questions

1. Is a mediator configured in the affected production deployment? (Decides
   whether A1 is the primary cause or a latent one.)
2. What share of live APS demand is `tagtype: "script"`?
3. Should `client_render_fallback` ever become default-on for publishers
   without GAM line items for `hb_bidder=aps`?
4. Is PR #997 the intended restoration of the lost #922 attribution core, or
   should the original be re-merged?
5. Does any deployment serve descriptors for an origin whose config disables
   APS (decides whether the renderer route becomes config-independent, 6.6)?
6. Datasource naming/retention for client events, and whether Axum/Cloudflare/
   Spin get real sinks or keep accept-count-drop.
