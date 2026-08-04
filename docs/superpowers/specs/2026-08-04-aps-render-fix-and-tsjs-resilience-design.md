# APS Render Fix and TSJS Resilience Architecture — Design

- **Status:** revision 4 — reworked after the third design review round.
- **Date:** 2026-08-04
- **Baseline:** `rc/july` @ `541298695` — the full merged state (everything
  merged from `main` plus every rc-only merge), not just the delta pending
  against `main`.
- **Inputs:** three code audits performed against this baseline; design
  reviews of revisions 1–3; open issues #926, #941, #944, #962, #964, #977,
  #983, #989, #993; open PR #997.
- **Terminology:** the per-refresh counter is `refresh_gen` everywhere. The
  event previously named `render_confirmed` is removed (see G4c).

---

## 1. Problem statement

APS (Amazon Publisher Services) demand is fully integrated server-side — the
edge server runs the APS OpenRTB auction, wins bids, and ships a typed renderer
descriptor to the page — yet APS creatives still do not appear for real users.
Every previous fix (the `bid.meta` carrier, the decoupled prebid shim, the
`hb_adid` fallback) addressed a real defect, and APS still does not render.
That pattern is itself the finding: the APS pipeline has **multiple independent
failure points, most of which fail silently**, and the client library has **no
way to tell the server (or the operator) which one fired**.

At the same time, the TSJS client library has grown organically to 56 files /
~11,900 lines with two ~1,700-line monoliths, duplicated logic maintained by
hand in two languages, inverted layering, and roughly one hundred `catch`
blocks that discard failures. The APS outage and the library's shape are the
same problem seen from two sides.

This design covers both: (a) the specific fixes that make APS render, and (b)
the target architecture that makes TSJS a clean, resilient library.

### Non-goals

- No change to the APS OpenRTB endpoint contract or Amazon-side configuration
  (including its deliberate absence of `nurl`/`burl` — see G4d).
- No rewrite of Prebid.js integration strategy (the decoupled shim stays).
- No behavior change for publishers whose pages work today. Public-surface
  migrations happen behind a bounded compatibility window (7.4), never by
  immediate removal.

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

There is **zero client→server reporting**. Server telemetry marks `is_win=1`
at auction time and goes quiet; a bid that never painted is byte-identical to
one that painted perfectly. Client-side evidence dies with the tab.

---

## 3. The GPT reality this design must respect

1. **Bootstrap-first hybrid:** the server injects a 495-line ES5
   `gpt_bootstrap.js` before the bundle; shared monkeypatch sentinels mean the
   bundle's handoff and initial-load code is dead in production.
2. **The #922 merge loss:** orphan-slot recovery and `updateRender` enrichment
   are gone (`0dc9b19a9`); `__tsRenderGeneration`/`__tsRenderBid` are dead
   writes; bridge-served impressions double-count. PR #997 appears to be the
   reworked replacement.
3. **TS refreshes never pass `changeCorrelator: false`.**
4. **`enableSingleRequest()` is called blind** after the publisher's own
   `enableServices()` has almost always run.
5. Responsive resolution is a DOM-element-selection ladder; ambiguity silently
   skips the slot.
6. Three independent wrappers on `pubads().refresh` coordinate via
   window-global booleans.
7. **GPT offers no request cancellation** (`gpt/index.ts:1080`), and its event
   contract identifies only the slot: Google documents no per-refresh
   identifier and no completion-order guarantee for overlapping requests, and
   `slotRenderEnded` means creative code was injected, not that its resources
   loaded. Publisher code can refresh an adopted slot while a TS cycle is
   pending. Any attribution scheme must survive all of that (G4a).
8. **The bundle's `slotRenderEnded` registration is gated behind
   `!ts.servicesEnabled`** (`gpt/index.ts:1017`), so bootstrap-first or
   services-enabled pages can miss the listener entirely — G4a requires
   unconditional early subscription.

---

## 4. Design gates — the five contracts

### G1 — Trace identity and correlation

**The client-visible auction id must never be ingested** (EC-derived:
`publisher.rs:3237`). **Initial-HTML auction telemetry is emitted before page
JavaScript exists** (`telemetry.rs:148`, `publisher.rs:2452`), so correlation
is minted by whoever acts first:

- **Initial navigation (`nav_gen = 0`):** the server mints `trace_id`
  (128-bit CSPRNG, `^[0-9a-f]{32}$`), writes it into that response's auction
  telemetry rows at emit time (a new nullable `trace_id` column on the
  existing auction datasource; the independent telemetry `auction_id` UUID
  remains and remains separate), and injects it into the page as a `tsjs`
  boot field with the sampling decision and, when the tester gate is active,
  the diagnostic capability (5.3).
- **Cache-privacy invariant:** a trace or capability is injected **only** into
  responses that ran a per-request auction, and any trace-bearing HTML MUST be
  shared-cache-ineligible: `Cache-Control: private, no-store` and no
  validators that could revalidate a shared copy. This is enforced by
  construction (the injection site is the auction-bearing render path) and by
  test. A shared-cached page would have no per-visitor auction to correlate
  anyway; the invariant makes that alignment explicit.
- **SPA navigations (`nav_gen > 0`):** `/_ts/page-bids` **stays GET** (it is
  GET in `publisher.rs:3815` and the client issues GET at
  `gpt/index.ts:1152`; a browser GET cannot carry a body). The client mints
  the `trace_id` and sends it in a validated **`X-TSJS-Trace-Id`** request
  header — the request already carries a non-simple TSJS header, so CORS
  preflight behavior is unchanged. The server records it contemporaneously in
  that auction's telemetry rows and the JSON response **echoes the accepted
  trace id** and returns a capability bound to it when the tester gate is
  active.
- **Envelope:** every event carries
  `{trace_id, sampled, nav_gen, refresh_gen, seq}`; `seq` is per-trace
  monotonic.
- **Sampling is trace-sticky** (decided once per trace; server-decided for
  `nav_gen 0`, client-decided from the injected rate afterwards). Transport is
  best-effort, so a sampled trace may still arrive partial; partiality is
  detectable via `seq` gaps and is not treated as a contract violation.

### G2 — Render identity: `hb_adid`, the APS token, and the PBS Cache UUID

Unchanged from revision 3 (review-accepted): cache-backed bids keep the cache
UUID as `hb_adid` byte-for-byte; renderer-only bids get a server-minted token
`^[a-z0-9]{12}$` (CSPRNG; in-auction collision retry; cross-auction uniqueness
probabilistic with the birthday bound documented; harmless via per-
`(trace_id, nav_gen)` registry scoping); TTL 15 minutes; one-time consumption;
client-Prebid keeps Prebid's `adId`; non-APS cache-path regression tests.

### G3 — Runtime ABI: how code shares state under the IIFE build

IIFE-per-bundle with inlined imports (`build-all.mjs:46`, `bundle.rs:23`)
means module imports never share state across bundles (live proof:
`core/context.ts:11` vs `permutive/index.ts:102`).

Contract — a versioned registration ABI on `window.tsjs._internal`:

- Kernel ships only in `tsjs-core`, publishes
  `tsjs._internal = { abi: 1, registry }` once (window sentinel). The kernel
  constructs and registers core service instances during boot; integrations
  register integration-scoped services during `install()`.
- **Version semantics (settling the mixed-version gap):** every service
  registers with `(major, minor)`. `registry.get(name, {major, minMinor})`
  succeeds iff an implementation with the same `major` and `minor ≥ minMinor`
  is registered. The install manifest's plugin versions are **ranges with the
  same semantics** (required major, minimum minor), not exact pins.
  An incompatible service registration is **quarantined** — recorded, not
  installed — and surfaced as `abi_mismatch`; an incompatible plugin as
  `bundle_partial`. **First-wins applies only among compatible
  registrations.** The old-deferred-bundle + new-core scenario therefore has a
  deterministic verdict: the old plugin either satisfies the manifest range
  and runs, or is quarantined loudly.
- Stateful services only via the registry at call time; stateless helpers may
  be imported and inlined. Single-module-graph builds remain the successor
  option behind the same surface.

### G4 — Render lifecycle

**G4a — Request-cycle protocol (no ordering assumptions).** GPT documents no
per-refresh identity and no completion-order guarantee, so the protocol
assumes neither:

- Every observable request initiation is classified `ts | publisher`: TS's own
  `display()`/`refresh()` calls open TS cycles; the wrapped publisher
  entry-points and `slotRequested` events that match no TS cycle are recorded
  as publisher-initiated.
- **TS serializes itself to at most one outstanding cycle per slot** — a new
  TS refresh for a slot with a pending cycle waits or supersedes explicitly;
  it is never concurrently pending.
- With ≤1 TS cycle outstanding, a `slotRenderEnded` is attributable iff no
  untracked or publisher-initiated request overlaps it. **Any overlap marks
  the slot `cycle_unattributable` and fails closed** (no fallback, no state
  transition, console warning + disposition).
- `slotRenderEnded` is treated as "creative code injected", not "resources
  loaded" — it can confirm delivery, never paint.
- The deterministic PUC/message harness exercises the protocol in CI, and a
  **release-gating real-GAM overlap test** (publisher refresh racing a TS
  cycle) validates the contract against actual GPT, since a FIFO-assuming
  stub proves nothing.

**G4b — Acknowledgement path.** Unchanged from revision 3 (review-accepted):
per-attempt CSPRNG nonce in the bridge response; the dynamic renderer posts
versioned accepted/failed messages to the kernel; the kernel validates source
ownership, nonce, token, `nav_gen`, `refresh_gen` before any transition or
callback; pinned for SSAT, client-Prebid, and nested SafeFrame flows.

**G4c — Honest observations, `render_confirmed` removed.** The inline-adm
frames are sandboxed `srcdoc` documents with `allow-same-origin` deliberately
omitted (`gpt/index.ts:358`) — their origins are opaque, so revision 3's
"same-origin observable" premise was false. The event is removed entirely.
The taxonomy is now: `gam_nonempty`, `gam_empty`, `renderer_document_loaded`,
`runner_loaded`, `runner_failed`, `adm_document_loaded` (the iframe `load`
event for TS-written adm frames — document delivery, not paint). **Every
render path terminates at `render_accepted`** (authenticated per G4b where
the renderer protocol exists; `adm_document_loaded` for adm frames). No
observation claims paint. A future trusted completion acknowledgement (open
question 6) may reintroduce a confirmed state under a new name.

**G4d — Win/billing notifications, scoped to paths that have them.** APS
**intentionally carries neither** `nurl` nor `burl` (`aps.rs:812` sets both
`None`; the minimized AAX envelope excludes notifications; the integration
guide documents that generic win/billing beacons are not fired for APS). APS
billing runs entirely inside the Amazon runner lifecycle, and this design
does not change the APS wire contract.

For bid paths that do carry the URLs (PBS and other OpenRTB providers):

- **Trigger semantics, published explicitly:** `nurl` fires when the render
  attempt binds the bid to a cycle (Trusted Server's selection produced the
  candidate GAM will render — the earliest point at which "win" is
  meaningful for this pipeline); `burl` fires at the attempt's
  `render_accepted`. Both are **attempt-scoped**, keyed by
  `(trace_id, nav_gen, slot, refresh_gen, hb_adid)` as the idempotency key —
  fired at most once per attempt, not page-wide.
- **Owner and mechanics:** the client render pipeline owns firing (as today,
  `gpt/index.ts:459`), via `sendBeacon`/`no-cors fetch`, no retries (a beacon
  either queues or is lost; retrying risks double-billing).
- Terminal failure after acceptance is labeled `billed_then_failed`; no
  un-firing.

**G4e — Fallback trigger.** The opt-in fallback
(`[auction].client_render_fallback = "renderer"`) renders only after a
**terminal `gam_empty` unambiguously attributed to a TS-initiated cycle**
(G4a). Timeouts are diagnostics-only and never render. Revision 3 disabled
fallback for adopted slots entirely, which — as the review noted — excludes
the common production path (pre-existing publisher slots are adopted and
refreshed, `gpt/index.ts:925`). Revised: **ownership does not gate the
fallback; attribution does.** An adopted slot whose attributed TS cycle ends
in `gam_empty` may fall back; any publisher-initiated or unattributable cycle
never triggers it. The success criteria and browser specs cover the adopted
case explicitly.

### G5 — Deployment contracts

- **Config rollback:** new config fields are default-valued and omitted from
  serialization at defaults. **Rollback runbook rule:** after an operator has
  opted into a new field, rolling the binary back requires restoring the
  default and pushing the default-compatible blob first (this mirrors the
  project's existing rollback guidance).
- **Asset identity and the artifact source (settling "not realizable"):**
  hash in the pathname; and artifacts are **published to shared immutable
  platform storage (KV/config store) as deploy stage 1, before any HTML
  references them** — binaries serve the current vector from embedded bytes
  (fast path) and everything else by hash lookup in shared storage. This
  answers all four skew cases: new HTML hash `B` reaching an old instance
  (lookup serves `B` from storage), a miss for retained `A` after only `B` is
  embedded (lookup), already-issued legacy query-hash URLs (the legacy path
  keeps serving current bytes with short-TTL, non-immutable caching through a
  documented sunset), and renderer `/v2` reaching a `/v1`-era instance
  (versioned renderer documents are published to the same storage in
  stage 1). Two-stage deployment is the contract: **stage 1 publish
  artifacts, stage 2 roll binaries/HTML.** Both rolling directions and the
  legacy URL are tested. Retention: ≥ 7 days, which must exceed the HTML
  cache lifetime — itself now bounded by contract (auction-bearing HTML is
  `no-store` per G1; any cacheable non-auction HTML referencing tsjs sets
  `max-age ≤ 300`). `Cache-Control: immutable` only on exact hash matches;
  unknown hashes → `410 Gone`, `no-store`. Concatenations are keyed by the
  **ordered module-ID vector**, precomputed in Phase 0 (which owns asset
  identity).
- **Ingest routing:** the beacon route exists in all four adapters as an
  early, EC-free, filter-free route; only Fastly has a real sink; others
  accept-count-drop by explicit contract.
- **Storage:** datasource `ts_client_events`; retention 30 days; production
  sampling 10%. These are **adopted defaults** (operator-tunable), no longer
  open questions; the remaining open question is only whether non-Fastly
  adapters get sinks (OQ5).
- **Phase gates are phase-specific** (section 8) — the render-fail canary
  applies only from Phase 3 onward, because earlier phases don't create that
  metric.

---

## 5. Workstream 1 — Observability

### 5.1 Event payload — minimized, grouped per trace

```
{ v: 1, traces: [
  { trace_id, sampled, capability?,        // capability: only in diagnostic mode
    events: [
      { nav_gen, refresh_gen, seq,
        t: "bid_received" | "targeting_set" | "bridge_request" |
           "bridge_response_sent" | "render_attempt" | "render_accepted" |
           "render_fail",
        slot,       // configured slot id if in the injected set, else "s<ordinal>"
        id_kind,    // "cache_uuid" | "render_token" | "prebid_adid" | "bid_id" | "none"
        matched,    // bridge_request only
        source,     // "renderer" | "adm" | "pbs-cache" | "gam"
        reason,     // render_fail only: closed enum below
        width, height }  // invalid_dimensions context only: bounded ints [0, 8192]
    ] }
] }
```

- **Events are grouped per trace, and the diagnostic capability is a per-trace
  field** — a navigation-spanning batch carries one group per trace, so one
  batch-level capability can never be ambiguous, and an initial-navigation
  capability never authorizes a client-minted SPA trace (that trace's
  capability comes from the page-bids response, G1).
- Reason enum (closed; no interpolation — dimension context travels in the
  bounded numeric fields): `renderer_document_no_load`, `runner_no_load`,
  `runner_failed`, `descriptor_invalid`, `invalid_dimensions`,
  `bridge_id_mismatch`, `cycle_unattributable`, `bridge_claim_timeout`,
  `gam_empty`, `no_render_source`, `slot_unresolved`, `gpt_absent`,
  `pbjs_absent`, `bundle_partial`, `fallback_cancelled`, `abi_mismatch`.

### 5.2 Transport

`fetch(..., {keepalive: true, credentials: "omit"})` primary. The `pagehide`
fallback is `navigator.sendBeacon(url, new Blob([json], {type:
"application/json"}))` — the Blob type satisfies the ingest media-type
contract (a bare string would arrive as text); it is credentialed by platform
design and its `true` means queued, not received; the handler ignores
credentials either way. Flush on `visibilitychange`/`pagehide` and every 5 s.

### 5.3 Ingest wire contract

- `POST /_ts/client-events` in all four adapters, before auth/EC/filters.
  `Content-Type: application/json` only; no `Content-Encoding`. Responds
  `204 Cache-Control: no-store`; never echoes input.
- Pre-parse limits: body ≤ 16 KiB; ≤ 64 events; strings ≤ 64 chars;
  `trace_id ^[0-9a-f]{32}$`; integers in `[0, 2³¹)`; width/height in
  `[0, 8192]`. Violation → drop-and-count with `204`.
- Same-origin: `Sec-Fetch-Site: same-origin` when present, else `Origin`
  matching the serving host; **absent both → drop-and-count**.
- **Rate limiting fails closed for telemetry:** when the limiter denies, or
  when a portable adapter's best-effort limiter is unavailable or errors, the
  request is dropped early with `204` (count only, no parse, no sink). Ad
  delivery is unaffected by construction because this route serves nothing.
- **Trusted client address, per adapter, concretely:** Fastly — the
  platform's client IP API; Axum — the rightmost `X-Forwarded-For` entry
  beyond `trusted_proxy_hops` (a required config value when the beacon is
  enabled; without it, the socket peer address is used and forwarded headers
  are ignored); Cloudflare — `CF-Connecting-IP`; Spin — the platform client
  address. Spoofable headers are never trusted beyond the configured hop
  count.
- **Diagnostic capability:** server-issued, HMAC over
  `trace_id + expiry` (≤ 15 min), delivered via the G1 boot field (initial
  trace) or the page-bids response (SPA traces), echoed per trace group.
  Signature verification is what switches a trace to unsampled — a public
  query flag alone never does.

### 5.4 Two modes, honestly separated

- **Production telemetry** (sink-backed deployments only — today Fastly):
  sticky-sampled (10%). SLO, fully parameterized: a failure mode affecting
  ≥ 1% of **sampled render attempts** is visible in `ts_client_events` within
  one hour, evaluated only when the deployment produced ≥ 10,000 sampled
  render attempts in that hour, with sink ingestion freshness ≤ 5 minutes;
  sink outages pause the SLO clock and are alarmed separately. Non-sink
  adapters are explicitly out of SLO scope, and no global gate depends on
  their beacon data.
- **Diagnostic mode:** capability-gated, unsampled, full stream + console
  mirroring — the "one page load names the failing reason" tool.

### 5.5 Server-side drop-reason surfacing

- Bounded structured summary whenever **any** bid is dropped (per-slot reason
  counts, capped); `drop_reasons` added to auction telemetry rows; drop
  summary in the initial-HTML `ts-debug` comment; `/_ts/page-bids` gains a
  tester-gated structured `debug` field.
- Startup warnings: APS enabled with `allow_script_creatives = false`; direct
  provider configured alongside a mediator without 6.1's merge strategy.

---

## 6. Workstream 2 — APS delivery fixes

### 6.1 Mediation: opt-in merge, `mediator_only` stays the default

As revision 3 (review-accepted), with one tightening: a mediator bid whose id
the provenance map cannot resolve, **for a slot where the same provider had
forwarded candidates**, is counted under a dedicated
`mediator_provenance_unresolved` metric and logged at `warn` — surfacing
possible self-competition instead of silently treating it as distinct demand.
Deal priority remains out of scope (the `Bid` model carries no deal identity;
recorded as follow-up). Currency mismatch remains rejection. One selection
helper serves both mediation lifecycles, with tests for each.

### 6.2 Dimensions: the contract is "request what you accept"

Exact size membership stays. The fix is visibility plus configuration: the
drop summary names the rejected size per slot via
`reason: invalid_dimensions` with bounded numeric `width`/`height` fields
(closed enum preserved), and documentation gains "sizing your slots for APS."

### 6.3 Script creatives

Secure default kept; consequence made loud (5.5); enablement path documented.

### 6.4 Render identity

As G2.

### 6.5 Fallback rendering

As G4e — attribution-gated, not ownership-gated; timeouts never render; the
renderer is converted to an awaitable API with cancellation and terminal
reasons before the fallback lands.

### 6.6 Renderer endpoint — unconditional, versioned, observable

- The static renderer document route registers unconditionally in every
  adapter (the APS provider stays config-gated). Startup validation fails
  loudly if an auth handler pattern covers it.
- **Caching matches immutability:** `/integrations/aps/renderer/v1` is an
  immutable artifact — its bytes change only by shipping `/v2` — so it is
  served with `Cache-Control: immutable` (long max-age), published to shared
  artifact storage in deploy stage 1 like every versioned asset (G5), which
  also answers version-skew (`/v2` requests reaching older instances are
  served from storage). Revision 3's `no-store` contradicted the versioning
  and is corrected.
- Two-stage acknowledgement (G4b): authenticated `document_loaded`, then the
  runner-load result — splitting `renderer_document_no_load` from
  `runner_no_load`/`runner_failed`.
- **Server route counters are aggregate** (requests, unknown-version,
  auth-blocked): the document request carries no trace (the nonce travels in
  the URL fragment, which never reaches the server), so no row-level join is
  claimed.
- **CSP report-only canary, fully specified:** reports go to a dedicated
  `POST /_ts/csp-reports` route (same pre-parse caps and same-origin rules as
  5.3; credentials ignored; rate-limited fail-closed); stored as **aggregate
  counters only** (directive, blocked-origin **host only** — full URLs
  redacted) on sink-backed adapters, count-and-drop elsewhere; enforcement
  follows only after a clean canary window.

### 6.7 One descriptor schema

As revision 3 (review-accepted): tagged-envelope schema generated from a
separate wire-schema crate/xtask; semantic validators hand-written on both
sides; outer-tolerance only, exact AAX projection; shared
positive + adversarial corpus across Rust, TS, and the inline document;
staleness CI.

### 6.8 Bridge hardening

As revision 3 (review-accepted): source-first ownership with the bounded
parent-chain SafeFrame walk (depth 5, known slot-root `WindowProxy` map, no
tree scans); adversarial test set; top-of-listener hygiene; dead branch
deleted; renderer branches emit trace records under G4's taxonomy, with G4d
notifications only where the bid path carries them (never APS).

---

## 7. Workstream 3 — TSJS target architecture

### 7.1 Layering

Kernel / adapters / services / integrations exactly as revision 3, with the
boundary lint in CI. Stateful services via the G3 ABI only.

### 7.2 Adapters

`present | pending | timed_out` with non-terminal `timed_out` (late loaders
transition to `present`); per-operation timeouts with disposition reasons.

### 7.3 Slot registry service

Kernel-owned registry (`WeakMap<googletag.Slot, SlotRecord>` + div-id index)
holding ownership, adoption, handoff claims, responsive resolution, pending
request cycles (G4a), and targeting-key history. No expandos on GPT objects.

### 7.4 Global namespace policy — with a compatibility window

As revision 3 (review-accepted): `window.tsjs` + `tsjs._internal`; the public
queue keeps its real name **`tsjs.que`**; public globals
(`tscreative`, `tsCreativeConfig`, `tsjs.que`) get dual-read/write for two
release cycles / ≥ 60 days, closing only on the adoption gate (old-name usage
< 0.1% of traces for 14 days, measured on sink-backed deployments);
`requestAds` keeps its void signature; `requestAdsAsync` is the new versioned
API; private globals migrate immediately.

### 7.5 Messaging module

All `postMessage` through one module: versioned envelopes, name constants,
G4b nonces, 6.8 source validation. A **minimal** messaging module (envelope +
constants + validation helpers used by the bridge) lands early (Phase 1) so
Phase 3 does not depend on Phase-4 structure; the full migration of every
legacy call site completes in Phase 4.

### 7.6 Plugin lifecycle and session model

As revision 3 (review-accepted), with G3's sharpened version semantics:
manifest versions are ranges (major + minMinor); quarantine on
incompatibility; first-wins only among compatible. Sessions:
`RuntimeSession` / `NavigationSession` / `RenderAttempt` with enumerable
disposal inventories. Error policy: no empty `catch`; auction fetch gets
timeout + `AbortController`. **Console logging retained, not replaced**
(paired `warn` with the beacon's reason code; `debug`-level
delivery/security failures promoted to `warn`).

### 7.7 The bootstrap problem

As revision 3: queue-and-flags stub + bundle replay behind its own flag with
replay-timing specs; the no-bundle fallback generated from the TypeScript
source.

### 7.8 GPT correctness fixes carried with the restructure

- **Unconditional early GPT event subscription:** the `slotRenderEnded`
  (and `slotRequested`) listeners register on the command queue at install,
  no longer gated behind `!ts.servicesEnabled` (`gpt/index.ts:1017`) — G4a
  cannot work on bootstrap-first pages otherwise. Recording is idempotent so
  double-registration cannot double-count.
- Restore #922/#997 attribution and orphan recovery.
- `changeCorrelator: false` on TS-initiated refreshes (configurable).
- `enableSingleRequest()` only when GPT services are not already enabled.
- Ambiguous responsive resolution emits `render_fail{slot_unresolved}`.

### 7.9 Decomposition targets

As revision 3 (gpt/prebid splits, script-guard consolidation, trace model/UI
split, `global.d.ts` fix).

### 7.10 Performance

Budgets tightened per review: per-bundle raw/gzip/Brotli for the exact
ordered module vector vs a checked-in baseline, **+5% byte tolerance**;
browser timing assertion (bids-script-to-first-`display()`) on a **pinned CI
runner class and pinned browser version**, **5 warm-up runs discarded, 50
measured samples, gate on p90 with +10% latency tolerance**; server bench
(precomputed concatenation) gates CPU and heap at **±10% vs baseline** with
pinned tool versions. Precompute lands in Phase 0 with asset identity (G5).

### 7.11 Toolchain and dependency currency

As revision 3 (review-accepted): raise the TypeScript floor to the resolved
5.9 line, then evaluate the next major separately; strictness flags on; dev
toolchain bumps as individual CI-gated PRs; `prebid.js` excluded from casual
bumps; monthly review policy.

---

## 8. Migration plan

Each phase ships behind a feature flag with **phase-specific gates** (the
render-fail canary cannot evaluate phases that predate the metric):

- **Phase 0 — Asset identity, contracts, toolchain.** Two-stage artifact
  publishing (shared immutable storage) + path-based identity + ordered-vector
  precompute + rolling-deploy tests in both directions + legacy-URL test;
  toolchain floors; contracts G1–G5 as code-adjacent docs; delete dead expando
  writes; server drop-reason surfacing.
  _Gate:_ zero unexpected `410`s and zero legacy-URL breakage on canary; asset
  hit/miss counters nominal.
- **Phase 1 — Kernel ABI, sessions, minimal messaging, minimal cycle
  registry.** Versioned registry with `(major, minor)` semantics;
  `RuntimeSession`/`NavigationSession`; install manifest; the minimal
  messaging module (7.5) and the cycle-aware slot-record core (G4a's queue)
  land here so Phase 3 has its dependencies; unconditional early GPT
  subscriptions (7.8).
  _Gate:_ ABI install-success counters clean; zero `abi_mismatch` on canary;
  no listener regression in browser specs.
- **Phase 2 — Trace and beacon.** Server-minted initial trace + page-bids
  `X-TSJS-Trace-Id` echo + capability issuance (G1, 5.3); beacon service;
  four-adapter ingest; `ts_client_events`.
  _Gate:_ ingest acceptance/drop/abuse counters nominal; trace join rate on
  sink-backed canary ≥ 95% of sampled traces.
- **Phase 3 — APS delivery.** Wire-schema crate + corpus; mediation helper +
  opt-in merge; render token; unconditional versioned renderer route +
  two-stage ack; bridge hardening; request-cycle protocol + render state
  machine + awaitable renderer + scoped notifications (G4a–G4d); the opt-in
  fallback (G4e); restore #922/#997; correlator and SRA fixes.
  _Gate:_ `render_fail` rate within +0.5% absolute of pre-flag baseline over
  24 h on canary; fill/latency/billing volume deltas within agreed bounds
  (billing measured against GAM/server-side reporting, not the beacon);
  release-gating real-GAM overlap test green.
- **Phase 4 — Structure.** Full layering + boundary lint; plugin lifecycle
  completion; adapters; full slot registry; full messaging migration;
  namespace window (7.4).
  _Gate:_ boundary lint zero exceptions; disposal-inventory leak tests green.
- **Phase 5 — Decomposition.** File splits; script-guard consolidation;
  bootstrap shrink (own flag, replay-timing specs); compatibility-window
  close (adoption-gated).
  _Gate:_ bundle budgets and timing assertions hold; adoption gate met before
  any removal.

---

## 9. Test acceptance matrix

Blocking CI is hermetic; the staged smoke suite (real GAM line items,
including the G4a overlap test) is release-gating.

| Area              | Must cover                                                                                                                                                                                                                          |
| ----------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Mediation         | both lifecycles; ties, floors, currency rejection; provenance dedup; transformed ids → `mediator_provenance_unresolved`; `mediator_only` default; rollback blob round-trip                                                          |
| Cache identity    | non-APS cache-backed bids byte-identical; PUC `?uuid=` path                                                                                                                                                                         |
| Render token      | format/CSPRNG/in-auction retry/TTL/one-time/per-`(trace, nav_gen)` scoping                                                                                                                                                          |
| Request cycles    | ts-vs-publisher classification; one-outstanding-TS-cycle serialization; publisher refresh overlapping TS cycle → `cycle_unattributable` fail-closed; late events; **real-GAM overlap (release gate)**                               |
| Ack protocol      | nonce validation (source, token, nav_gen, refresh_gen); SSAT + client-Prebid + nested SafeFrame; stale/replayed acks                                                                                                                |
| Render semantics  | notifications only on carrying paths (never APS); `nurl` at bind, `burl` at accepted, attempt-scoped idempotency; `billed_then_failed`; no paint claims (`adm_document_loaded` labeling); accepted-but-blank                        |
| Fallback          | renders only on attributed `gam_empty` (adopted **and** TS-owned); publisher-initiated cycles never trigger; timeout diagnostics-only; SPA cancellation; destruction; exactly-once terminal                                         |
| Bridge security   | wrong-slot/stolen/replayed/prior-navigation tokens; nested foreign frames; bounded parent-chain walk; SafeFrame positive                                                                                                            |
| Beacon            | initial-trace join; page-bids header echo + response trace/capability; per-trace grouping across navigation-spanning batches; `seq`-gap partial traces; ingest abuse incl. absent Origin; capability signature; sendBeacon Blob     |
| Schema            | staleness; adversarial corpus ×3 validators; outer-tolerance vs exact AAX projection                                                                                                                                                |
| Runtime ABI       | one kernel under concatenation; deferred late registration; failure isolation; **mixed-version verdicts (compatible-range install vs quarantine)**; first-wins among compatible only                                                |
| Lifecycle         | `timed_out → present`; session disposal inventories; stale async install; pre-init `tsCreativeConfig` + `tsjs.que`; dual-name window                                                                                                |
| Delivery          | two-stage deploy simulation **both rolling directions**; new-HTML-hash on old instance (storage lookup); retained-hash miss; legacy query-hash sunset path; unknown hash 410 `no-store`; immutable exact-match only; ordered vector |
| Renderer endpoint | route in all adapters; auth-pattern startup failure; `/v1` immutable caching; version-skew via storage; `document_loaded` vs runner split; CSP report route caps/redaction                                                          |
| Adapter parity    | ingest, CSP-report, renderer routes and drop-reason surfacing equivalent across Fastly/Viceroy, Axum, Cloudflare, Spin                                                                                                              |
| Policy            | script-creative warning; `invalid_dimensions` + bounded width/height; page-bids `debug` gating; diagnostic unsampled completeness; trace-bearing HTML `private, no-store` invariant                                                 |

---

## 10. Alternatives considered

Unchanged from revision 3 (patching without telemetry; always-direct-render;
single module graph; big-bang rewrite; dropping the bootstrap;
timeout-triggered fallback — all rejected for the recorded reasons).

## 11. Risks

Revision 3's list, plus: **shared-storage dependency for assets** (stage-1
publish becomes a deploy prerequisite; mitigated by the embedded fast path
for the current vector and deploy-time verification that storage matches the
embedded hashes); **notification-trigger semantics** are now a published
contract for PBS-path demand — changing them later is a breaking change for
SSP reporting expectations.

## 12. Success criteria

1. APS creatives render on a reference page in each configured flow,
   hermetically in CI and via the staged smoke suite (including the real-GAM
   overlap test).
2. Every failure point in section 2 maps to a distinct observable signal;
   diagnostic mode names the failing reason from one page load; production
   telemetry meets the 5.4 SLO on sink-backed deployments.
3. Boundary lint zero exceptions; stateful sharing only via the versioned
   ABI; mixed-version delivery resolves to the G3 verdicts.
4. No file in `src/` exceeds ~500 lines; `gpt_bootstrap.js` is a stub or
   generated.
5. Trace counts are per-impression; orphan recovery has a non-vacuous test;
   cycle attribution follows G4a including the publisher-overlap fail-closed
   rule.
6. The only TSJS-owned global is `window.tsjs` (public globals only inside
   their window, closed by the 7.4 adoption gate); no expandos on GPT slots,
   GPT functions, or `pbjs`.
7. Bundle budgets (+5% bytes) and the pinned-environment p90 timing assertion
   (50 samples, +10% tolerance) hold; server concatenation is precomputed
   within ±10% CPU/heap of baseline.
8. No existing warning is lost; every issue-surfacing condition logs at
   `warn` or above with the beacon's reason code.
9. TypeScript floor matches the resolved 5.9 line with strictness flags on;
   `prebid.js` pin matches the documented deployed bundle; monthly review
   policy in CI docs.
10. Rolling-deploy tests pass in both directions; legacy URLs serve through
    their sunset; unknown hashes 410 `no-store`; immutable only on exact
    match.
11. `nurl`/`burl` fire only on carrying paths, on their G4d transitions,
    attempt-scoped and idempotent; APS fires neither.
12. Trace-bearing responses are `private, no-store` by test; capabilities are
    per-trace and never authorize a trace they were not bound to.

## 13. Open questions

1. Is a mediator configured in the affected production deployment?
2. What share of live APS demand is `tagtype: "script"`?
3. Should `client_render_fallback` ever become default-on for publishers
   without GAM line items for `hb_bidder=aps`?
4. Is PR #997 the intended restoration of the lost #922 attribution core, or
   should the original be re-merged?
5. Do Axum/Cloudflare/Spin get real client-event sinks, or keep
   accept-count-drop?
6. Does Amazon expose any creative-completion acknowledgement that could
   reintroduce a confirmed state (G4c) under a new name?
7. Which shared storage backs stage-1 artifact publishing per platform (KV
   store vs config store vs CDN), and who owns the publish step in the deploy
   pipeline?
