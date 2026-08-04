# APS Render Fix and TSJS Resilience Architecture — Design

- **Status:** revision 3 — reworked after the second design review round.
- **Date:** 2026-08-04
- **Baseline:** `rc/july` @ `541298695` — the full merged state (everything
  merged from `main` plus every rc-only merge), not just the delta pending
  against `main`.
- **Inputs:** three code audits performed against this baseline (APS end-to-end
  trace, TSJS architecture audit, GPT integration map); design reviews of
  revisions 1 and 2; open issues #926, #941, #944, #962, #964, #977, #983,
  #989, #993; open PR #997.
- **Terminology:** the per-refresh counter is `refresh_gen` everywhere in this
  document (revision 2 used `render_gen` in some places; that name is
  retired).

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
   never run on a real page.
2. **The slot handoff can alias a publisher's new div to a GPT slot bound to a
   dead element** — and the orphan-recovery watcher built to repair exactly that
   was **lost in the #922 merge** (`0dc9b19a9` resolved `gpt/index.ts` to the rc
   side). `updateRender` now has no production caller,
   `__tsRenderGeneration` / `__tsRenderBid` are dead writes, and every
   bridge-served impression double-counts in the trace. Open PR #997 appears to
   be the reworked replacement.
3. **TS refreshes never pass `changeCorrelator: false`**, silently changing
   roadblock, competitive-exclusion, and frequency-capping behavior.
4. **`enableSingleRequest()` is called blind** after the publisher's own
   `enableServices()` has almost always run.
5. Responsive resolution is a DOM-element-selection ladder; ambiguity silently
   skips the slot for the whole pass.
6. Three independent wrappers on `pubads().refresh` coordinate via
   window-global booleans that async wrappers observe already reset.
7. **GPT refresh is asynchronous and offers no request-cancellation
   primitive** (`gpt/index.ts:1080`): once a refresh is issued, a response can
   arrive arbitrarily late. Any fallback design must treat an issued GPT
   request as uncancelable.

---

## 4. Design gates — the five contracts

### G1 — Trace identity and correlation

**The client-visible auction id must never be ingested.** It is EC-derived by
construction (`publisher.rs:3237`: `ts-{ec_id}` when an EC exists). Server
telemetry uses an independent fresh UUID (`telemetry.rs:94-97`) — and, decisive
for the design: **initial-HTML auction telemetry is emitted before page
JavaScript exists** (`telemetry.rs:148`, `publisher.rs:2452`), so a
client-minted trace can never retroactively join it.

Contract — correlation is minted by whoever acts first:

- **Initial navigation (`nav_gen = 0`):** the **server** mints a
  `trace_id` — 128-bit CSPRNG, hex (`^[0-9a-f]{32}$`), derived from nothing —
  per HTML response, writes it into that response's auction telemetry rows at
  emit time, and injects it into the page as a `tsjs` boot field alongside the
  sampling decision. The client's `NavigationSession` adopts it. It is a new
  value, never `AuctionRequest.id`.
- **SPA navigations (`nav_gen > 0`):** the **client** mints the `trace_id` for
  the navigation and sends it in the `/_ts/page-bids` request body; the server
  records it contemporaneously in that auction's telemetry rows. No
  retroactive joining anywhere.
- **Envelope:** every event carries
  `{trace_id, sampled, nav_gen, refresh_gen, seq}` — per event, not per batch,
  so a transport batch may span navigations without ambiguity. `seq` is a
  per-trace monotonic counter for ordering and deduplication.
- **Sampling** is decided once per trace (server-decided for `nav_gen 0`,
  client-decided from the injected rate for later navigations) and recorded in
  the envelope; a sampled trace is complete or absent.

### G2 — Render identity: `hb_adid`, the APS token, and the PBS Cache UUID

**The PBS Cache contract stays untouched.** `hb_adid` deliberately prefers the
Prebid Cache UUID (`publisher.rs:3355`), and both the emitted cache coordinates
and the bridge's cache fetch assume `?uuid=<hb_adid>` (`publisher.rs:3450`,
`gpt/index.ts:1700`).

Contract:

- Bids with a cache id: `hb_adid` = cache UUID, exactly as today. Bids with
  markup and no cache id: existing fallback chain, exactly as today.
- **Renderer-only bids (APS): `hb_adid` = a server-minted render token**,
  format `^[a-z0-9]{12}$` (12 chars exactly, 36¹² ≈ 4.7 × 10¹⁸ values),
  CSPRNG-generated. Collision handling is honest about its scope: retry on
  collision **within the minting auction** (the only scope the server can
  check without storage); cross-auction uniqueness is **probabilistic**, with
  the birthday bound documented (at 10⁶ live tokens the collision probability
  is ~10⁻⁷) and made harmless by scoping: the client registry keys tokens per
  `(trace_id, nav_gen)`, so a cross-page collision cannot cross wires.
- Token lifecycle: TTL **15 minutes** from mint, one-time consumption in the
  bridge registry, invalidated by navigation.
- The client-side Prebid adapter path keeps Prebid's generated `adId`; both
  paths register into one bridge registry keyed by whichever id that path
  observes.
- Regression tests: non-APS cache-backed bids keep byte-identical `hb_adid`
  and cache coordinates.

### G3 — Runtime ABI: how code shares state under the IIFE build

Every entry point is built as a self-contained IIFE with dynamic imports
inlined (`build-all.mjs:46`), and the server concatenates already-closed IIFEs
(`bundle.rs:23`) — a module `import` never shares state across bundles. Live
proof: `core/context.ts:11` holds a private context-provider `Map` while
`permutive/index.ts:102` registers into its own copy.

Contract — **a versioned registration ABI on `window.tsjs._internal`**:

- The kernel ships **only** in `tsjs-core` (always first in the concatenated
  unified bundle) and publishes `tsjs._internal = { abi: 1, registry }` exactly
  once, guarded by a window-level sentinel.
- **Construction ownership:** the kernel constructs and registers the core
  service instances (event bus, beacon queue, session objects, slot registry,
  render state machine) during its own boot; integrations construct only
  integration-scoped services and register them during their `install()`.
- All **stateful** services are reached only through
  `tsjs._internal.registry.get(name, minVersion)` at call time. Pure stateless
  helpers may be imported and inlined freely.
- `abi` majors are checked at lookup; a mismatch is a logged, telemetered
  refusal, not a silent no-op. Mixed-version delivery (old deferred bundle,
  new core) is a tested scenario, not an accident.
- The single-module-graph build is recorded as the successor option behind the
  same ABI surface.

### G4 — Render lifecycle: cycles, acknowledgements, and honest states

Four sub-contracts, each fixing a hole the reviews identified.

**G4a — GPT request-cycle protocol.** `slotRenderEnded` identifies a slot, not
a request, and the bridge currently reads live `window.tsjs.bids`
(`gpt/index.ts:1606`) while the generation snapshots are dead writes
(`gpt/index.ts:1085`). Contract: every TS-issued `display()`/`refresh()` opens
a **cycle** `(slot, refresh_gen)` pushed onto a per-slot pending-cycle queue;
`slotRequested` confirms it; GPT fires slot events in order per slot, so
`slotRenderEnded` is attributed to the oldest confirmed pending cycle for the
slot. Each bridge token and each render attempt binds to exactly one cycle. If
attribution is ambiguous (overlapping cycles the queue cannot separate, or an
event with no pending cycle), the state machine for that slot **fails closed**:
no fallback, `render_fail{cycle_unattributable}`, console warning.

**G4b — Acknowledgement path.** Today the renderer document posts ready only
to its immediate parent (`aps.rs:105`); in the PUC path that parent is the
nested renderer frame, which resolves a local promise (`render.ts:423`) the
top-level kernel cannot observe — and callbacks currently fire right after the
bridge posts its response (`gpt/index.ts:1572`, `:1620`). Contract: the bridge
response carries a **per-attempt CSPRNG acknowledgement nonce**; the dynamic
renderer posts versioned `render_accepted` / `render_failed{reason}` messages
**to the kernel** (top window), carrying the nonce; the kernel validates
source ownership, nonce, token, `nav_gen`, and `refresh_gen` before any state
transition or callback. This protocol is pinned by tests for all three flows:
SSAT, client-Prebid, and nested SafeFrame.

**G4c — Honest observation names.** The browser cannot see inside an opaque
APS frame, and the iframe's geometry is assigned by our own renderer — it
proves nothing about content. A nonempty `slotRenderEnded` proves GAM
delivered a creative container, not that the nested runner painted. The state
machine therefore records observations under accurate names —
`gam_nonempty`, `gam_empty`, `renderer_document_loaded`, `runner_loaded`,
`runner_failed` — and **APS attempts terminate at `render_accepted`**
(= authenticated `runner_loaded` ack) unless Amazon provides a real completion
acknowledgement. `render_confirmed` exists only for paths with same-origin
observable content (inline adm frames TS itself writes); it is never derived
from geometry or from PUC container delivery. Tests: accepted-but-blank, and
nonempty-`slotRenderEnded`-before-bridge-claim.

**G4d — `nurl`/`burl` are separate business events.** OpenRTB 2.6
distinguishes them: `nurl` is the win notice (implies neither delivery nor
billability); `burl` is the billable-event notice under exchange policy.
Today both fire together (`gpt/index.ts:459`). Contract — independent,
idempotent transitions:

1. winner selection → fire `nurl`;
2. `render_accepted` (authenticated) → fire `burl` — this is the **declared
   commercial policy** for APS given no paint acknowledgement exists, recorded
   here explicitly rather than implied;
3. terminal failure after acceptance → no un-firing; the row is labeled
   `billed_then_failed` so the policy's cost is measurable.

**G4e — Fallback trigger.** GPT offers no cancellation (section 3.7), so a
timeout can race a late fill that arrives after a fallback has rendered and
billed. Contract: the opt-in direct fallback
(`[auction].client_render_fallback = "renderer"`) renders **only after an
explicit terminal empty event for the bound cycle** (`gam_empty` from G4a
attribution). A timeout emits diagnostics (`render_fail{bridge_claim_timeout}`)
and **never renders**. For publisher-owned (adopted) slots, fallback is
disabled entirely. TS-owned-slot timeout rendering is admitted only as a
possible future extension that must first destroy the slot to retire the
request, and is out of scope here.

### G5 — Deployment contracts

- **Config rollback:** every new auction/config field is default-valued and
  omitted from serialization at its default (`auction_config_types.rs:7`
  denies unknown fields), so blobs written by a new binary remain readable by
  the previous one unless an operator opts in.
- **Asset identity is path-based and retained.** A query hash the handler
  ignores (`tsjs.rs:3`, `publisher.rs:294`) is not content addressing, and
  redirecting an old hash to current bytes just executes new code under old
  HTML, bootstrap flags, and ABI expectations. Contract: the content hash
  moves into the **pathname** (`/static/tsjs/<hash>/<name>.js`); the server
  serves through a hash→bytes manifest that **retains prior artifacts** beyond
  the maximum HTML cache lifetime plus the deferred-load window (retention
  floor: 7 days); `Cache-Control: immutable` only on exact hash matches;
  unknown hashes answer `410 Gone` with `no-store` — never a redirect to
  different bytes. Precomputed concatenations are keyed by the **ordered
  module-ID vector** (order affects side effects), not the set.
- **Ingest routing:** the beacon route exists in all four adapters (Fastly,
  Axum, Cloudflare, Spin) as an early, EC-free, filter-free route. Only Fastly
  has a real sink today; the others accept-count-drop by explicit contract.
- **Storage:** a new dedicated datasource named `ts_client_events`; retention
  **30 days**; production sampling default **10%** (operator-tunable);
  schema versioned with the event enum.
- **Phase ordering** is section 8's; each phase ships behind a feature flag
  with the named canary thresholds and rollback criteria in section 8.

---

## 5. Workstream 1 — Observability

### 5.1 Event payload — minimized by design

High-cardinality identifiers stay out of the beacon: no raw `hb_adid`, no raw
Prebid `adId`, no free-form slot strings.

```
{ v: 1, events: [
  { trace_id, sampled, nav_gen, refresh_gen, seq,
    t: "bid_received" | "targeting_set" | "bridge_request" |
       "bridge_response_sent" | "render_attempt" | "render_accepted" |
       "render_confirmed" | "render_fail",
    slot,          // configured slot id if in the injected slot set, else "s<ordinal>"
    id_kind,       // "cache_uuid" | "render_token" | "prebid_adid" | "bid_id" | "none"
    matched,       // bridge_request only: token/id equality result
    source,        // "renderer" | "adm" | "pbs-cache" | "gam"
    reason }       // render_fail only: closed enum below
] }
```

- Every stored string is either a member of a server-known allowlist (slot ids
  from the injected config, enum members) or a bounded ordinal — nothing free
  .form is persisted.
- Reason enum: `renderer_document_no_load`, `runner_no_load`, `runner_failed`,
  `descriptor_invalid`, `bridge_id_mismatch`, `cycle_unattributable`,
  `bridge_claim_timeout`, `gam_empty`, `no_render_source`, `slot_unresolved`,
  `gpt_absent`, `pbjs_absent`, `bundle_partial`, `fallback_cancelled`,
  `abi_mismatch`. (`renderer_no_ready` from revision 2 is split by the G4b/6.6
  protocol into document-load vs runner-load failures.)

### 5.2 Transport

`fetch(..., {keepalive: true, credentials: "omit"})` primary; `sendBeacon` as
the documented last-resort `pagehide` fallback (credentialed by platform
design, and its `true` means queued, not received — the handler ignores
credentials either way). Flush on `visibilitychange`/`pagehide` and every 5 s.

### 5.3 Ingest wire contract (numeric, complete)

- Route: `POST /_ts/client-events`, registered in all four adapters before
  auth/EC/filters. Content type: `application/json` only (no
  `Content-Encoding`; compressed bodies rejected). Responds
  `204 Cache-Control: no-store`.
- Limits enforced **before parse or log**: body ≤ **16 KiB**; ≤ **64** events
  per batch; any string field ≤ **64** chars; `trace_id` must match
  `^[0-9a-f]{32}$`; `nav_gen`/`refresh_gen`/`seq` are integers in
  `[0, 2³¹)`. Violation → `204` (accepted-and-dropped) + abuse counter; the
  endpoint never echoes input.
- Same-origin enforcement: `Sec-Fetch-Site: same-origin` when present;
  otherwise `Origin` must match the serving host; **absent both → reject**
  (drop-and-count). All strings are structurally serialized (never
  interpolated into log lines).
- Client IP for rate limiting is derived per adapter from its documented
  trusted source (Fastly: the platform client IP; Axum: configured trusted
  proxy header; Cloudflare/Spin: platform equivalents). Rate limiting uses the
  platform limiter where one exists (Fastly); portable adapters ship a
  best-effort in-memory limiter and the policy is **fail-open with an abuse
  counter** (dropping telemetry must never block ad delivery).
- **Diagnostic mode is a server-injected capability, not a query flag.** The
  tester gate (cookie) is evaluated server-side at HTML render; the page
  receives a short-lived signed capability token (HMAC over
  `trace_id + expiry`, ≤ 15 minutes) which the client echoes in the batch.
  The ingest handler verifies the signature — this works with
  `credentials: "omit"` because the capability travels in the payload, and a
  public query flag alone can never switch a session to unsampled.

### 5.4 Two modes, honestly separated

- **Production telemetry:** sticky-sampled (default 10%), SLO: **a delivery
  failure mode affecting ≥ 1% of impressions is visible in `ts_client_events`
  within one hour**.
- **Diagnostic mode:** capability-gated, unsampled, full event stream plus
  console mirroring — the "one page load names the failing reason" tool.

### 5.5 Server-side drop-reason surfacing

- Emit a bounded structured summary **whenever any bid is dropped** (per-slot
  reason counts, capped), not only when zero survive.
- Add `drop_reasons` to auction telemetry rows; add the drop summary to the
  initial-HTML `ts-debug` comment; `/_ts/page-bids` (JSON) gains a gated
  structured `debug` field under the same tester gate.
- Startup validation warnings: APS enabled while
  `allow_script_creatives = false`; any direct provider configured alongside a
  mediator without the 6.1 merge strategy.

---

## 6. Workstream 2 — APS delivery fixes

### 6.1 Mediation: opt-in merge, `mediator_only` stays the default

- `[auction].winner_selection = "mediator_only"` (default, today's behavior,
  now explicit) or `"merge_highest_cpm"` (opt-in); omitted from serialized
  blobs at the default (G5).
- `merge_highest_cpm` semantics: comparison in decoded CPM; **currency
  mismatch is a rejection** (the mismatched bid is dropped with a selection
  reason; no conversion in v1); slot floors apply to both populations; ties
  break to the mediator; a mediator timeout degrades to direct-provider
  selection and is reported.
- **Deduplication key:** the server constructs the mediator's input, so it
  records provenance at forwarding time — `(provider_name, upstream_bid_id)`
  per candidate — and carries a provenance map keyed by the id it sent.
  A mediator bid whose id maps back to a forwarded candidate counts once, as
  provenance `mediator`. A mediator bid whose id was **transformed beyond the
  map** is treated as distinct mediator demand (documented limitation).
- **Deal priority is out of scope for v1.** The internal `Bid`
  (`types.rs:231`) carries no deal identity; inventing a priority rule the
  model cannot express would be fiction. Extending the bid model with
  `deal_id`/deal type and a deal-first rule is recorded as follow-up work;
  until then deals compete by CPM like everything else and the limitation is
  documented in the config reference.
- One candidate-selection helper serves **both** mediation lifecycles
  (ordinary/page-bids, `orchestrator.rs:412-431`; initial-SSAT split
  dispatch/collect, `orchestrator.rs:1320`), with tests for each.
- A **selection report** (`winner_source`, `mediator_superseded`,
  `currency_rejected`, dedup hits) is emitted separately from
  delivery-conversion drop reasons.

### 6.2 Dimensions: the contract is "request what you accept"

Revision 2's operator `accept_sizes` allow-list is withdrawn on the review's
sharper observation: if an alternate size is acceptable, it belongs in the
slot's **requested formats** — APS should be asked for it. Accepting an
unrequested response size would conceal an upstream protocol violation.

- Exact size membership (`aps.rs:657-668`) remains the admission rule,
  unchanged.
- The fix is configuration plus visibility: the drop summary (5.5) names the
  rejected size per slot (`invalid_dimensions{300x600}`), so an operator sees
  exactly which format to add to the slot's `formats` if they want that
  demand. Documentation gains a "sizing your slots for APS" section.
- No `hb_size` key, no admission relaxation, no new config.

### 6.3 Script creatives

Keep the secure default (`allow_script_creatives = false`) but make the
consequence loud (5.5) and document the enablement path for TAM-heavy
publishers.

### 6.4 Render identity

Implemented exactly as G2 (token scope, format, TTL, per-navigation registry
keying, one-time consumption, cache-path regression tests).

### 6.5 Fallback rendering

Implemented exactly as G4e: renders only on an attributed terminal
`gam_empty`; timeouts are diagnostics-only; disabled for adopted slots. The
direct renderer is converted to an awaitable API with cancellation and a
terminal reason first; the fallback lands only after that conversion.

### 6.6 Renderer endpoint — unconditional, versioned, observable

Topology is resolved now rather than left conditional:

- **The static renderer document route registers unconditionally in every
  adapter.** It contains no configuration, no secrets, and validates its input
  client-side; serving it cannot leak anything, and conditional registration
  is exactly what created the silent cross-deployment failure class. (The APS
  _provider_ stays config-gated; only the static document is unconditional.)
- The document is **versioned in its path**
  (`/integrations/aps/renderer/v1`) and served `Cache-Control: no-store`;
  descriptor compatibility across N/N−1 is guaranteed by the outer-tolerant
  validation of 6.7. The client pins the version it targets.
- Startup validation fails loudly if any configured auth handler pattern
  covers `/integrations/aps/renderer`.
- **Two-stage acknowledgement (with G4b):** the document first posts an
  authenticated `document_loaded` (proving route + auth + CSP allowed the
  document itself), then the separate runner-load result. This splits the old
  blind timeout into `renderer_document_no_load` (route/auth/stale-CDN/network)
  vs `runner_no_load` / `runner_failed` (Amazon script or CSP) — distinct
  signals, as the success criteria require.
- Server-side: route status/version counters (requests, unknown-version,
  auth-blocked) join the telemetry rows.
- **CSP changes ship report-only first** (`Content-Security-Policy-Report-Only`
  canary with a bounded report endpoint), then enforce; each added source is
  justified in a comment and covered by the browser spec. Tests cover broad
  auth patterns, stale versions, and CSP failures on all adapters.

### 6.7 One descriptor schema — structural generation, semantic validators kept

- The wire truth is the tagged enum `BidRenderer` (discriminator lives there,
  not on `ApsRendererV1` — `types.rs:188-211`); the generated schema is the
  full tagged envelope.
- Generation lives in a **separate wire-schema crate** (or host-side xtask) —
  core already depends on `trusted-server-js` (`Cargo.toml:45`), so the
  reverse edge would be a cycle. Generated TS artifacts are checked in; CI
  fails on staleness.
- Generation covers structure only; the semantic security checks stay
  hand-written on both sides (URL/origin policy, canonical base64, length
  bounds, the exact one-bid envelope projection, cross-field equality).
  **Unknown-field tolerance applies only to the outer versioned descriptor;
  the decoded AAX envelope remains an exact projection.**
- A shared positive + adversarial corpus runs through the Rust validator, the
  TS validator, and the inline renderer document in CI.

### 6.8 Bridge hardening

- **Ownership proof stays source-first**, and the SafeFrame extension is
  bounded: the kernel maintains a map of known slot-root `WindowProxy` objects
  (the iframes GPT created under each slot element); on a message, it walks
  the **sender's own parent chain** (`event.source.parent`, …) up to depth
  **5**, looking for a known root — it never enumerates or recursively scans
  an attacker-controllable frame tree. Unresolvable source → refuse with
  `bridge_id_mismatch`.
- Adversarial tests: wrong-slot tokens, nested foreign frames, replayed and
  duplicated messages, stolen tokens, previous-navigation tokens, plus the
  positive SafeFrame case.
- Blanket top-of-listener hygiene: parse and ownership-check before branch
  logic.
- Delete the dead duplicate renderer branch (C5); renderer branches emit the
  same trace records as adm/cache branches under G4's taxonomy and the G4d
  `nurl`/`burl` split (C7).

---

## 7. Workstream 3 — TSJS target architecture

### 7.1 Layering

```
kernel/          boot, config, queue, event bus, log, beacon, sessions
adapters/        googletag.ts, pbjs.ts, messaging.ts   ← the ONLY window.* access
services/        slots (registry+handoff), auction client, render engine, consent
integrations/    gpt, prebid, aps, creative, datadome, …  (plugins over services)
```

Boundary lint in CI (`import/no-restricted-paths`): `kernel` imports nothing
above it; `adapters` import kernel only; `services` import kernel + adapters;
`integrations` import kernel + services, never each other. Stateful services
via the G3 ABI only.

### 7.2 Adapters: explicit absence, without giving up on late loaders

`present | pending | timed_out` per external global; `timed_out` is
non-terminal (late GPT/pbjs/CMP arrival transitions to `present` and drains
what is still valid); individual queued operations carry their own timeouts
and expire with a disposition reason.

### 7.3 Slot registry service

One registry owns slot knowledge (publisher- vs TS-defined, adoption, handoff
claims, responsive resolution, pending request cycles per G4a, targeting-key
history), keyed by `WeakMap<googletag.Slot, SlotRecord>` plus a div-id index,
kernel-owned via the ABI. Expandos on live GPT objects are eliminated.

### 7.4 Global namespace policy — with a compatibility window

- One owned global, `window.tsjs`; public API versioned; coordination state
  under `tsjs._internal` (G3). **The public queue keeps its existing name:
  `window.tsjs.que`** (`types.ts:259`, drained at `core/index.ts:25`) —
  revision 2's `cmd` was an error; renaming a public surface silently would
  violate this very section.
- Inventory first: every current global classified public
  (`globalThis.tscreative`, `tsCreativeConfig`, `tsjs.que`) or private
  (`__tsjs_*` flags, expandos, sentinels).
- **Public globals: dual-read/write for a bounded window — two release
  cycles, minimum 60 days — ending only after an adoption gate: beacon-observed
  old-name usage below 0.1% of traces for 14 consecutive days.** Pre-init
  compatibility tests pin that config set before the bundle loads keeps
  working.
- Private globals migrate immediately: dead expando writes deleted now; slot
  state into `SlotRecord`; function sentinels into a kernel `WeakSet`; boot
  flags into `tsjs` boot fields.
- `requestAds` keeps its void signature; failure surfacing arrives as a **new
  versioned async API** (`tsjs.requestAdsAsync(...): Promise<RequestAdsResult>`)
  rather than changing the existing contract.

### 7.5 Messaging module

All `postMessage` traffic through one module: versioned envelopes, message
name constants, the G4b acknowledgement nonces, source validation per 6.8, one
audit point.

### 7.6 Plugin lifecycle and session model

- **Activation:** Rust owns integration selection today and continues to — the
  server injects a **versioned install manifest** (enabled plugin ids +
  expected versions, in injection order) into the pre-core `tsjs.que`. The
  kernel executes the manifest on boot; nobody else calls install in
  production (the API remains callable for tests).
- `tsjs.definePlugin(id, version, install, dispose)`: synchronous `install`
  by default; a plugin may return a promise, but anything `adInit` depends on
  (gpt, prebid shim registration) must complete synchronously and is listed as
  such in the manifest. Late registration after the manifest requested the id
  installs on arrival (pending-install queue) bounded by a missing-module
  timeout emitting `bundle_partial`. A stale async completion (arriving after
  its `RuntimeSession` was disposed) is discarded. Duplicate `(id, version)`
  is a no-op; a different version for a registered id: first-wins + loud
  telemetry. Disposal runs in reverse install order.
- **Session model, split as the review required:**
  - `RuntimeSession` (page lifetime): bridge listener, history hook, pbjs
    subscriptions, adapters, beacon queue.
  - `NavigationSession` (per SPA navigation): `trace_id`, render attempts,
    slot aliases, targeting history, navigation-scoped timers/observers.
  - `RenderAttempt` (per G4a cycle): state machine instance, ack nonce.
    Each owns an **enumerable** disposal inventory; navigation disposes
    `NavigationSession` children only.
- Per-plugin exception isolation: a plugin that throws during install is
  quarantined and reported; it cannot halt the bundle.
- Error policy: no empty `catch`; every catch handles, logs with context, or
  emits a disposition reason. The auction fetch gets timeout +
  `AbortController`.
- **Console logging is retained, not replaced.** Every issue-surfacing
  condition keeps (or gains) a `log.warn` debuggable from DevTools; existing
  warnings survive verbatim or strengthened; `debug`-level failure paths that
  indicate delivery or security-relevant failures are promoted to `warn`;
  every `render_fail`/dependency-timeout disposition emits a paired `warn`
  with the same reason code.

### 7.7 The bootstrap problem

Target: shrink the inline `gpt_bootstrap.js` to a queue-and-flags stub with
the bundle replaying recorded calls on install. This is **not a pure move** —
replay changes observable ordering — so it ships behind its own flag with
browser specs extended to cover replay timing, and the no-bundle fallback
("ads still render if the bundle fails", pinned by `gpt.rs:1174-1179`) is
**generated from the same TypeScript source** at build time.

### 7.8 GPT correctness fixes carried with the restructure

- Restore the #922 orphan-slot recovery and `updateRender` enrichment (verify
  against open PR #997; land whichever is canonical).
- `changeCorrelator: false` on TS-initiated refreshes; correlator behavior
  becomes a documented, configurable decision.
- `enableSingleRequest()` only when GPT services are not already enabled.
- Ambiguous responsive resolution emits `render_fail{slot_unresolved}`
  alongside its console warning.

### 7.9 Decomposition targets

| Today                                | Target                                                                                                             |
| ------------------------------------ | ------------------------------------------------------------------------------------------------------------------ |
| `gpt/index.ts` (1777 LOC, 20 jobs)   | `slot_resolution`, `handoff`, `initial_load`, `ad_init`, `render_bridge`, `spa_navigation`, `beacons` + thin index |
| `prebid/index.ts` (1671 LOC)         | adapter, shim, refresh handler (moves onto slot registry), eids, diagnostics                                       |
| `gpt/script_guard.ts` (634 LOC)      | folded onto the 170-LOC `shared/script_guard.ts` factory                                                           |
| `core/trace.ts` (record model + UI)  | `services/trace` (model) + `integrations/trace_overlay` (UI)                                                       |
| `core/global.d.ts` (`pbjs: TsjsApi`) | real Prebid types; `TsjsApi` split into public API vs internal coordination state                                  |

### 7.10 Performance

Client-side speedups with enforced budgets: smaller synchronous bundle
(script-guard consolidation, single APS module via the ABI, dead-code
deletion, trace-overlay extraction); slot resolution once per navigation;
bounded telemetered waits. Budgets, concretely: per-bundle raw/gzip/Brotli
bytes for the exact ordered module vector of the reference config, compared
to a checked-in baseline with **+5% tolerance**; the
bids-script-to-first-`display()` browser assertion runs **20 iterations** and
gates on **p90**. Server-side: precompute bundle bytes + hash per ordered
module vector (deploy/config-time), benchmark server CPU/heap before/after.

### 7.11 Toolchain and dependency currency

- **TypeScript:** the manifest floor is `^5.5.4` but the lockfile already
  resolves **5.9.3** (`package-lock.json`), so the code compiles on 5.9 today.
  Action: raise the manifest floor to the resolved 5.9 line (making the floor
  honest), then evaluate the next major as its own gated PR; adopt
  `noUncheckedIndexedAccess`, `exactOptionalPropertyTypes`,
  `verbatimModuleSyntax`.
- **Dev toolchain to latest stable** (eslint + plugins, prettier, jsdom,
  `@playwright/test`, `@types/node` aligned to the pinned Node), each bump its
  own mechanical CI-gated PR with changelog review — this library monkeypatches
  `fetch`, `sendBeacon`, and DOM prototypes, so jsdom/Playwright behavior
  changes are real risks.
- **`prebid.js` is excluded from casual bumps** (runtime Prebid is the
  manifest-locked external R2 bundle); the npm pin and deployed bundle version
  stay documented together.
- **Standing policy:** monthly dependency review; no migration phase starts
  more than one minor behind latest stable.

---

## 8. Migration plan

Ordered so every phase's prerequisites precede it (the review's required
order). Every phase ships behind a feature flag; shared canary criteria: no
increase in `render_fail` rate beyond **+0.5% absolute** on canary traffic
over 24 h, no new console errors in the browser-spec run, rollback = flag off
(phases 0–2 are additive; later phases keep dual paths until their gate).

- **Phase 0 — Asset identity, contracts, toolchain.** Path-based immutable
  asset identity + manifest retention + rolling-deploy tests (G5) — this lands
  **before** anything makes ABI compatibility load-bearing. Toolchain floors
  (7.11). Contracts G1–G5 recorded as code-adjacent docs. Delete the dead
  expando writes. Server drop-reason surfacing (5.5) — server-only, no client
  dependency.
- **Phase 1 — Kernel ABI and sessions.** Minimal kernel in `tsjs-core`
  publishing the versioned registry (G3); `RuntimeSession` /
  `NavigationSession` scopes (7.6); context-provider fix as the ABI proof;
  install manifest injection.
- **Phase 2 — Trace and beacon.** Server correlation nonce for `nav_gen 0` +
  page-bids trace echo (G1); beacon service on the Phase-1 kernel; ingest
  route in all four adapters (5.3); diagnostic capability; `ts_client_events`
  datasource.
- **Phase 3 — APS delivery.** Wire-schema crate + corpus (6.7); mediation
  helper + opt-in merge (6.1); render token (6.4); unconditional versioned
  renderer route + two-stage ack (6.6, G4b); bridge hardening (6.8); GPT
  request-cycle protocol + render state machine + awaitable renderer +
  `nurl`/`burl` split (G4a–G4d); the opt-in fallback (G4e); restore #922/#997
  attribution; correlator and SRA fixes (7.8). APS renders after this phase
  wherever GAM line items and configuration permit.
- **Phase 4 — Structure.** Full layering + boundary lint, plugin lifecycle
  completion, adapters, full slot registry, messaging module, namespace
  migration window (7.4), server bundle precompute (7.10).
- **Phase 5 — Decomposition.** File splits (7.9), script-guard consolidation,
  bootstrap shrink behind its own flag with replay-timing specs (7.7), end of
  the public-global compatibility window (gated per 7.4).

---

## 9. Test acceptance matrix

Blocking CI is hermetic (the deterministic PUC/message harness); a separate
staged smoke suite covers real GAM line items and is release-gating, not
PR-gating.

| Area              | Must cover                                                                                                                                                                                                                    |
| ----------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Mediation         | both lifecycles; ties, floors, currency rejection; duplicate demand via provenance map; transformed mediator ids; `mediator_only` default preserved; rollback blob round-trip (omitted defaults)                              |
| Cache identity    | non-APS cache-backed bids byte-identical (`hb_adid` + coordinates); PUC `?uuid=` fetch path                                                                                                                                   |
| Render token      | `^[a-z0-9]{12}$`; CSPRNG source; in-auction collision retry; 15-min TTL; one-time consumption; per-`(trace, nav_gen)` registry scoping                                                                                        |
| Request cycles    | pending-cycle attribution; overlapping refreshes; late `slotRenderEnded`; nonempty-before-bridge-claim; `cycle_unattributable` fail-closed                                                                                    |
| Ack protocol      | nonce validation (source, token, nav_gen, refresh_gen); SSAT + client-Prebid + nested SafeFrame flows; stale/replayed acks                                                                                                    |
| Render semantics  | no `burl` before authenticated `render_accepted`; `nurl` at selection independent of delivery; `billed_then_failed` labeling; accepted-but-blank; no `render_confirmed` from geometry or PUC container                        |
| Fallback          | renders only on attributed `gam_empty`; timeout is diagnostics-only; adopted-slot fallback disabled; SPA cancellation; slot destruction; exactly-once terminal transition                                                     |
| Bridge security   | wrong-slot tokens; nested foreign frames; replay + duplicates; stolen tokens; previous-navigation tokens; bounded parent-chain walk (depth 5); SafeFrame positive case                                                        |
| Beacon            | initial-nav server nonce join; page-bids trace echo; per-event envelope across navigation-spanning batches; `(trace_id, nav_gen, seq)` dedup; ingest abuse (oversize, malformed, cross-origin, absent Origin); capability sig |
| Schema            | generated-artifact staleness; adversarial corpus through Rust + TS + inline validator; outer-tolerance vs exact AAX projection                                                                                                |
| Runtime ABI       | one kernel under concatenation; deferred registration after manifest request; per-plugin failure isolation; **mixed-version delivery (old deferred bundle + new core)**; abi-mismatch refusal                                 |
| Lifecycle         | late-loaded GPT/pbjs (`timed_out → present`); RuntimeSession vs NavigationSession disposal inventories; stale async install completion; pre-init `tsCreativeConfig` and `tsjs.que` compatibility; dual-name global window     |
| Delivery          | rolling-deploy simulation (old HTML + retained old assets); unknown hash → 410 `no-store`; immutable only on exact match; ordered-module-vector keying; deterministic raw/gzip/Brotli budgets vs baseline                     |
| Renderer endpoint | route present in all four adapters; auth-pattern startup failure; stale version behavior; CSP report-only canary; `document_loaded` vs runner-result split                                                                    |
| Adapter parity    | ingest + renderer routes + drop-reason surfacing behave equivalently on Fastly/Viceroy vs Axum vs Cloudflare vs Spin                                                                                                          |
| Policy            | script-creative startup warning; `invalid_dimensions{WxH}` drop naming; page-bids `debug` field gating; diagnostic-mode unsampled completeness                                                                                |

---

## 10. Alternatives considered

1. **Keep patching APS point-failures without telemetry.** Rejected: three
   consecutive correct fixes have not produced ads.
2. **Direct-render APS always (skip GAM/PUC).** Changes GAM reporting/pacing
   semantics unilaterally; kept as the opt-in, `gam_empty`-gated fallback.
3. **Single module graph / shared chunks instead of the registration ABI.**
   Cleaner long-term; changes the delivery pipeline now; successor option
   behind the same ABI surface.
4. **Full library rewrite in one branch.** Rejected: thin browser-spec safety
   net in exactly the changing areas.
5. **Drop the ES5 bootstrap entirely.** Loses the pinned "ads render if the
   bundle fails" guarantee; generated fallback keeps it.
6. **Timeout-triggered fallback rendering.** Rejected (G4e): GPT requests
   cannot be cancelled, so timeout rendering races late fills; only an
   attributed terminal empty event may trigger rendering.

## 11. Risks

- **Merge strategy misconfiguration:** mitigated by `mediator_only` default,
  the selection report, omitted-default serialization.
- **Beacon abuse/volume:** pre-parse caps, origin checks, per-adapter rate
  limiting, sticky sampling, closed enums, capability-gated diagnostics.
- **ABI freeze risk:** versioned from day one; mixed-version delivery tested.
- **Ack protocol adds a message round-trip before `burl`:** bounded by the
  existing renderer timeout; the `billed_then_failed` label measures the
  policy.
- **Bootstrap replay:** own flag, replay-timing specs, staged rollout.
- **Schema generation:** checked-in artifacts + staleness CI.

## 12. Success criteria

1. APS creatives render on a reference page in each configured flow (SSAT,
   Prebid adapter, page-bids), hermetically in CI and via the staged smoke
   suite.
2. Every failure point in section 2 maps to a distinct observable signal;
   diagnostic mode names the failing reason from one page load; production
   telemetry meets the 5.4 SLO (≥ 1% failure modes visible within one hour).
3. Boundary lint passes with zero exceptions; stateful sharing only via the
   versioned ABI; mixed-version delivery behaves as specified.
4. No file in `src/` exceeds ~500 lines; `gpt_bootstrap.js` is a stub or
   generated.
5. Trace counts are per-impression; orphaned-slot recovery has a non-vacuous
   test; `refresh_gen` attribution follows the G4a cycle protocol.
6. The only TSJS-owned global is `window.tsjs` (public globals only inside
   their announced window, which closes per the 7.4 adoption gate); no
   expandos on GPT slots, GPT functions, or `pbjs`.
7. Bundle budgets (raw/gzip/Brotli, +5% tolerance vs baseline) and the p90
   20-run timing assertion hold; server-side per-request concatenation is
   precomputed.
8. No existing warning is lost; every issue-surfacing condition logs at `warn`
   or above with the beacon's reason code.
9. TypeScript floor matches the resolved 5.9 line with the strictness flags
   on; `prebid.js` npm pin matches the documented deployed bundle; monthly
   review policy in CI docs.
10. Rolling-deploy tests: old HTML always receives its exact old assets during
    the retention window; unknown hashes 410 with `no-store`; immutable
    caching only on exact matches.
11. `nurl` and `burl` fire on their distinct G4d transitions, idempotently,
    and never before their triggering state.

## 13. Open questions

1. Is a mediator configured in the affected production deployment?
2. What share of live APS demand is `tagtype: "script"`?
3. Should `client_render_fallback` ever become default-on for publishers
   without GAM line items for `hb_bidder=aps`?
4. Is PR #997 the intended restoration of the lost #922 attribution core, or
   should the original be re-merged?
5. Do Axum/Cloudflare/Spin get real client-event sinks, or keep
   accept-count-drop? (Datasource `ts_client_events`, 30-day retention, and
   10% default sampling are proposed values pending operator sign-off.)
6. Does Amazon expose any creative-completion acknowledgement we could adopt
   to move APS beyond `render_accepted` (G4c)?
