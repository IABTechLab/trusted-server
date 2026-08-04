# APS Render Fix and TSJS Resilience Architecture — Design

- **Status:** revision 9 — closes the eighth review round's mandatory set:
  attempt schema, diagnostic renewal, telemetry/gate model, CSP routing, and
  rollout measurement. Fully self-contained.
- **Date:** 2026-08-04
- **Baseline:** `rc/july` @ `248fe9558` ("Fix APS PUC rendering and collapsed
  GAM shells"). All file:line citations refer to this commit.
- **Inputs:** three code audits; design reviews of revisions 1–8; open issues
  #926, #941, #944, #962, #964, #977, #983, #989, #993; open PR #997.
- **Normative gates:** Appendix A ships with this design; changes require
  reviewed decision records.
- **Adoption stance for the baseline APS fixes (`248fe9558`):** contracts
  adopted (MessageChannel handshake semantics, collapsed-shell remediation,
  consolidated bridge branch), implementations rebuilt inside the target
  architecture; the baseline browser tests pass unmodified as the
  conformance pin.

## 0. Release policy: coordinated hard cutover

- One release: server, TSJS bundles, config, HTML under one **`release_id`**.
  No N/N−1; in-flight clients may fail at cutover — accepted and stated.
- **Exact release matching**; mismatch is a refusal.
- **Config is a release-time, content-verified input.** `format_version`
  exact-match. **`config_hash` = SHA-256 over the exact fetched envelope
  bytes**, bound through compiled release metadata (or a signed manifest
  with an embedded verification key); the binary verifies at startup and
  fails loudly on mismatch. Publish order: blob, then manifest. Rollback =
  redeploy the previous release with its own verified config; binding
  prevalidated.
- Assets: embedded only; hashed pathnames for cache identity; unknown hash →
  `410`, `no-store`.
- **Authenticated sticky affinity.** The router sets `ts-rel` on HTML
  responses — format `r1.<kid>.<exp>.<release>.<cohort>.<assignment_id>.
<sig>`: `kid ^[a-z0-9-]{1,16}$`; canonical decimal `exp` (TTL 24 h);
  `release` from the allowlist; `cohort ∈ {canary, control}`;
  `assignment_id` = 32-hex CSPRNG (the pseudonymous **randomization unit**,
  §8); `sig` = unpadded base64url HMAC-SHA-256 over the domain-separated
  length-prefixed input `"ts-affinity-v1" || u32be-len fields ||
u64be(exp)`; keys owned and rotated by the routing layer (active +
  previous, ≥ 24 h retention); constant-time verification; test vectors
  checked in. Attributes `Secure; HttpOnly; SameSite=Lax; Path=/`. Cache
  keys use the post-validation release label only.
- **State-dependent routing defaults:** during canary, valid tokens route by
  their binding; invalid/expired/forged → control + reissue. **After forward
  cutover ("weight 100%"), the safe default flips:** stale, invalid, or
  control-bound tokens on HTML requests are reassigned to the active
  release; non-HTML requests with unknown or stale tokens route to the
  active release — 100% means 100%, not "except 24 h of old cookies."
  Rollback flips the default back.
- **CSP-report affinity never depends on cookies:** the renderer is
  sandboxed without `allow-same-origin` (`aps/render.ts:4`), so its
  browser-generated reports are cross-origin to the publisher endpoint and
  carry no cookie. Release/cohort identity is encoded in the
  **server-generated report path's `policy_id`** (minted per
  `{release, policy version, cohort}`, registered in the header manifest);
  the router routes `/_ts/csp-reports/<policy_id>` by that registry.
- Beacon and trace-auth transports use `credentials: "same-origin"` so the
  affinity cookie routes them; handlers derive no identity from cookies.
- **Canary/control measurement (closing the empty-control-arm gap):** the
  control pool for Phase-3 statistics runs an **observation-only control
  build** — the baseline plus Phase-2 instrumentation only (trace, beacon,
  probes; zero behavior changes) — so both arms emit comparable sampled
  telemetry. Arms write to **arm-specific datasources**; the canonical
  union view stamps a trusted `deployment_pool` dimension from the write
  identity (token → dataset → arm), making the arm a queryable row
  dimension rather than an authorization side effect.
- Router weight over sticky cohorts is the sole activation primitive; flags
  are in-pool emergency kill switches. Cutover = weight 100% + CDN purge;
  rollback = weight back + re-purge. The affinity acceptance test covers
  HTML, assets, APIs, beacons, and CSP reports (via path identity).

## 1. Problem statement

APS demand is fully integrated server-side, yet APS creatives do not appear
reliably. Four serial fixes (the `bid.meta` carrier, the decoupled shim,
the `hb_adid` fallback, the baseline PUC/collapsed-shell fix) each survived
review; the pattern is the finding: **multiple independent failure points,
most failing silently**, with no client→server signal about which fired.
The TSJS library (56 files, ~11,900 lines, two ~1,800-line monoliths,
duplicated ES5/TS logic, inverted layering, ~100 error-swallowing catches)
is the same problem structurally.

### Non-goals

- No change to the APS OpenRTB endpoint contract (including its deliberate
  absence of `nurl`/`burl`, §G4d).
- No rewrite of the decoupled Prebid.js strategy.
- No backward compatibility (§0); replacement surfaces in §7.4.

## 2. Why APS does not render — evidence

Flows: (a) SSAT via `window.tsjs.bids`; (b) GAM + client `trustedServer`
Prebid adapter; (c) SPA `/_ts/page-bids`; (d) direct `/auction`. Only (d)
renders an APS descriptor without GAM.

### 2.1 Admission

| #   | Failure                                                                                                                        | Where                                            |
| --- | ------------------------------------------------------------------------------------------------------------------------------ | ------------------------------------------------ |
| A1  | A configured `[auction].mediator` discards every direct-provider bid; APS reports `success, bid_count: N`, never wins.         | `orchestrator.rs:412-431`                        |
| A2  | `allow_script_creatives` defaults `false`, dropping every `tagtype: "script"` APS bid; counted but invisible (A4).             | `aps.rs:161`, `:334`, `:793`                     |
| A3  | Strict gates: exact `w`×`h` membership; required `ext.creativeurl`; any top-level `contextual` key rejects the whole response. | `aps.rs:675`, `:763-796`, `:859`                 |
| A4  | Drop reasons reach only `/auction` `ext.orchestrator`; SSAT/page-bids discard them; logs and `ts-debug` exclude them.          | `publisher.rs:1866-1875`, `telemetry.rs:808-826` |

### 2.2 Identity

| #   | Failure                                                                                             | Where                                         |
| --- | --------------------------------------------------------------------------------------------------- | --------------------------------------------- |
| B1  | GAM caps key-values at 40 chars; the raw APS bid id as `hb_adid` can fail the bridge match, no log. | `publisher.rs:3366-3372`, `gpt/index.ts:1695` |
| B2  | Two id universes: SSAT keys on the APS bid id; the client adapter on Prebid's `adId`.               | `publisher.rs:3366`, `prebid/index.ts:982`    |

### 2.3 Render

| #   | Failure                                                                                                                 | Where                                                                 |
| --- | ----------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------- |
| C1  | If GAM never serves the PUC, nothing renders and nothing is recorded; `renderApsCreative` reachable only from flow (d). | `gpt/index.ts:923-1180`, `core/request.ts:59`                         |
| C2  | A renderer endpoint that never answers is a silent 10 s death (opaque iframe cannot read HTTP status).                  | `aps.rs:1247`, `aps/render.ts:30`, `:415-437`                         |
| C3  | SafeFrame breaks slot attribution (top-document iframe walk cannot see nested creative windows).                        | `gpt/index.ts:180-215`                                                |
| C4  | Three hand-maintained schema copies with exact-key rejection: a server field addition blanks every APS ad.              | `types.rs:188-211`, `aps/render.ts:46-63`, `:152-162`, `aps.rs:65-93` |
| C5  | Fixed at baseline `248fe9558`: the duplicate renderer branch was consolidated.                                          | `gpt/index.ts:1729`                                                   |
| C6  | The renderer CSP can kill creatives after "ready" (no `object-src`, workers, `blob:`/`data:` frames).                   | `aps.rs:49`                                                           |
| C7  | Renderer branches record nothing: no trace record, no notifications.                                                    | `gpt/index.ts:1628-1760`                                              |

### 2.4 Observability

Zero client→server reporting; a bid that never painted is byte-identical
server-side to one that painted.

### 2.5 Failure → signal mapping (normative)

The **tester cookie** (non-security, `tester_cookie.rs:3`) gates **debug
content** (`tsjs.boot.debug`, response `ext.trusted_server.debug` — same
class as the existing `ts-debug` comment). The **diagnostic credential**
(§5.3) gates **telemetry volume**. Neither crosses into the other's role.

| Failure | Client event/reason (§5.1)                           | Server row/counter (§5.6)             | One-page-load surface           |
| ------- | ---------------------------------------------------- | ------------------------------------- | ------------------------------- |
| A1      | —                                                    | `selection_summary.winner_source`     | `boot.debug` selection summary  |
| A2      | —                                                    | `bid_drop{script_rendering_disabled}` | `boot.debug` drop summary       |
| A3      | —                                                    | `bid_drop{invalid_dimensions,w,h}`    | `boot.debug` drop summary       |
| A4      | — (fixed by §5.6)                                    | `bid_drop` rows on all paths          | `boot.debug` / response `debug` |
| B1/B2   | `bridge_request{matched:false}` (§6.8)               | join via trace                        | console warn                    |
| C1      | `gam_empty` then no `bridge_request`                 | join via trace                        | console warn                    |
| C2      | `render_terminal{failed, renderer_document_no_load}` | `ts_ops_counters`                     | console warn                    |
| C3      | `render_terminal{failed, bridge_id_mismatch}`        | join via trace                        | console warn                    |
| C4      | `render_terminal{failed, descriptor_invalid}`        | schema corpus CI                      | console warn                    |
| C5      | — (fixed at baseline)                                | —                                     | —                               |
| C6      | `runner_failed` + CSP buckets                        | `ts_csp_reports`                      | console warn                    |
| C7      | full §5.1 sequence from renderer branches            | join via trace                        | debug/warn                      |

## 3. The GPT and baseline reality

1. Bootstrap-first hybrid; the bundle's handoff/initial-load code is dead
   in production.
2. #922 merge loss (`0dc9b19a9`); PR #997 is the apparent replacement.
3. TS refreshes never pass `changeCorrelator: false`.
4. `enableSingleRequest()` called blind after publisher `enableServices()`.
5. Responsive-resolution ambiguity silently skips slots.
6. Three independent `pubads().refresh` wrappers.
7. GPT has no cancellation, no per-refresh identity, no completion-order
   guarantee; `slotRenderEnded` = code injected; `responseIdentifier`
   identifies responses only.
8. `display()` under disabled initial load creates no request — for any
   caller (`gpt/index.ts:1175`, `ad_init.test.ts:1201-1263`).
9. `slotRenderEnded` registration gated behind `!ts.servicesEnabled`
   (`gpt/index.ts:1091`).
10. Baseline `248fe9558`: MessageChannel APS-PUC handshake
    (`aps.rs:65-125`, `aps/render.ts:415-437`), collapsed-shell resize
    (`gpt/index.ts:217`), C5 consolidated, real-PUC browser test.
11. The bridge keeps consumed-id tombstones (`gpt/index.ts:1527`).
12. The tester cookie is not a security control (`tester_cookie.rs:3`).
13. Fastly constructs application state per request (`app.rs:146`); its
    platform counter is a 60 s window with separate lookup/increment
    (`rate_limiter.rs:40`).
14. The baseline auction client collapses every failure into an empty
    array (`core/auction.ts:185-224`).
15. A single `/auction` request can carry several slots; concurrent calls
    race today (`request.ts:31`) and share one fetch.

## 4. Design gates

### G1 — Trace identity, attempts, sampling, correlation

- EC-derived auction ids (`publisher.rs:3237`) are never ingested.
  Initial-HTML telemetry precedes page JS (`telemetry.rs:148`,
  `publisher.rs:2452`); correlation is minted by whoever acts first: the
  server for `nav_gen 0` (trace + signed authorization in `tsjs.boot`);
  the client afterwards via `X-TSJS-Trace-Id` on page-bids (GET) and the
  `/auction` POST, echoed back with
  `ext.trusted_server.trace = {trace_id, auth, auction_id}`.
- **Attempt identity (closing the parent/child collision):** every render
  attempt mints a client-side **`attempt_id`** (8-char `[a-z0-9]`,
  CSPRNG, unique per trace) and carries nullable **`parent_attempt_id`**
  (fallback children reference their parent). The tuple
  `(trace_id, nav_gen, refresh_gen, slot)` remains a **grouping key
  only**; `ts_render_attempts_v` keys by `attempt_id`. Exactly one
  terminal event per `attempt_id` (G4c) is a tested invariant.
- **Deterministic keyed sampling:** first 8 bytes of
  `HMAC-SHA-256(sampling_key, trace_id)` as u64 BE; `sampled` iff
  `u64 < floor(sample_rate × 2⁶⁴)`; `sample_rate` finite in `[0, 1]`.
- **Cross-tier join:** equality key = the server telemetry
  **`auction_id`** (UUID; the client column is `Nullable(UUID)` and
  ingest validates canonical UUID syntax). Generations are client-side
  only. **Join grains are explicit:** auction-level joins hit the one
  summary row per `auction_id`; slot-level joins use
  `auction_id + slot + row_kind`; bid-level joins are opt-in for
  bid-grained analyses. Raw attempt × auction-row joins are forbidden
  (row multiplication).
- Cache-privacy invariant: traces/authorizations only in per-request
  auction-bearing responses; such HTML is `private, no-store`.
- Envelope: per-trace groups `{trace_id, auth, events[]}`; events carry
  `{nav_gen, refresh_gen, seq, flow, attempt_id?, parent_attempt_id?,
auction_id?, t_rel_ms?}`. **`t_rel_ms`** is a bounded monotonic
  duration (`performance.now()` truncated to u32 ms, relative to
  navigation start) — the latency gates' basis; `received_at` is ingest
  time and is never used as event time. `flow` is closed:
  `ssat | prebid | page_bids | direct | fallback | system`.
- Traces are navigation-scoped; attempt counts key on `attempt_id`.

### G2 — Render identity

- Cache-backed bids: `hb_adid` = PBS Cache UUID byte-for-byte
  (`publisher.rs:3355`, `gpt/index.ts:1772`). Markup bids: existing
  fallback chain. Renderer-only bids: server-minted token
  `^[a-z0-9]{12}$`, CSPRNG, in-auction collision retry, TTL 15 min,
  one-time consumption.
- **Reservation store:** live registrations + tombstones (consumed /
  stale / disposed) share one structure, **union capacity 320**; expired
  entries pruned; **unexpired entries never evicted**; at capacity, new
  registration refused with `registry_full`. Late prior-navigation
  requests always meet suppression until original TTL (preserving
  `gpt/index.ts:1527`). Test: >320 registrations, late oldest-id request.
- Client-Prebid keeps Prebid's `adId`; one store serves both paths.
  Non-APS cache-path byte-identity regression tests.

### G3 — Runtime ABI (exact-release)

- IIFE-per-bundle with inlined imports (`build-all.mjs:46`,
  `bundle.rs:23`); imports never share state (defect:
  `core/context.ts:11` vs `permutive/index.ts:102`). Kernel only in
  `tsjs-core`; `tsjs._internal = {release_id, registry}` frozen after
  boot; core services constructed at boot; plugins via
  `definePlugin({id, release, install})` with build-generated `release`;
  `registry.get` succeeds only on equality; mismatch quarantines
  (`abi_mismatch`/`bundle_partial`) loudly.
- **Boundary enforcement:** `import/no-restricted-paths` for layering,
  plus a **custom scope-aware ESLint rule** for external-global access —
  standard `no-restricted-properties`/`no-restricted-syntax` cannot
  follow arbitrary aliasing, so the custom rule tracks member access to
  `googletag`/`pbjs` through `window`/`globalThis`/`self` **and
  same-file const aliases**; anything cleverer (cross-module smuggling)
  is caught by review, and the claim is scoped to exactly that. Adapters
  are the only access to external ad-tech globals; kernel/messaging
  necessarily touch `window.tsjs` and `postMessage`.

### G4 — Render lifecycle

**G4a — Physical request cycles.** Intents (both classes, one causal
queue): any `display()` under disabled initial load is retired at
issuance regardless of caller; hindsight zero-request intents expire at
2 s with `intent_no_request`; **any later request-capable intent — same
class or opposite — supersedes a pending uncertain intent**, and if the
uncertain one could still be in flight, the next `slotRequested` is
ambiguous → quarantine. Cycles open only on `slotRequested` (causal
head; SRA = one per slot per batch) and close on `slotRenderEnded`
(`responseIdentifier` dedups drain). One outstanding TS cycle per slot;
one queued replacement. Attribution requires exactly one outstanding TS
cycle and no overlap; otherwise `cycle_unattributable`, fail closed. **No
timeout re-arm** — re-arm only on count-based drain, safe TS-owned
destroy/redefine, or page end; unissued intents are NavigationSession
children; physical state is RuntimeSession. Deterministic-harness CI +
the release-gating real-GAM suite (Appendix A.3).

**G4b — Acknowledgement, per render path.** Nonces are per-attempt
128-bit CSPRNG values; the kernel validates source ownership (§6.8),
nonce, token, `nav_gen`, `refresh_gen` before transitions or
notifications; navigation/supersession invalidates; late acks →
`stale_navigation`. Deadlines: document 3 s, runner 10 s, adm 5 s.

1. **APS-PUC** (baseline transport): MessageChannel into the renderer
   document (`ports.length` checks, exact-key replies, one-shot latch,
   port close); the document posts authenticated
   `renderer_document_loaded` then the accepted/failed result to the top
   window.
2. **Generic ADM/cache-PUC:** **the acceptance observation lives in the
   trusted owner, not the creative document** — the owner observes its
   own iframe's `load`/`error` events and emits `adm_document_loaded`;
   the nonce never enters the bidder realm (an injected reporter would
   hand bidder-controlled code the acceptance credential and let it
   trigger `burl` early — revision 8's reporter is withdrawn).
3. **Direct APS:** the kernel is the frame parent; the baseline
   parent-postMessage branch is already kernel-observed.
4. **Direct ADM/cache:** as (2), owner-observed `load`/`error`.

**G4c — Honest observations; one terminal event.** Observations:
`gam_nonempty`, `gam_empty`, `gam_collapsed{action: resized | guarded}`,
`renderer_document_loaded`, `runner_loaded`, `runner_failed`,
`adm_document_loaded`. **The terminal is one discriminated event:
`render_terminal{outcome: accepted | failed | no_bid | cancelled,
reason?}` — exactly one per `attempt_id`** (replacing separate
accepted/fail events the schema could not reconcile). A parent attempt
whose GAM cycle ends empty emits `render_terminal{failed, gam_empty}`
**before** its fallback child starts. Post-acceptance runner failure is
an observation only (no billing-failure event exists — no honest
producer). No observation claims paint. The baseline resize
(`gpt/index.ts:217`) stays a sanctioned, guarded exception.

**G4d — Notifications.** APS carries neither `nurl` nor `burl`
(`aps.rs:839`) — excluded entirely. For carrying paths: bind per flow
(PUC: owned matched bridge claim; direct: validated render start —
server must preserve + macro-expand the URLs (`formats.rs:423` omits),
client must parse + https-validate (`core/auction.ts:43` drops);
fallback: attributed parent `gam_empty` immediately before child
render). `nurl` at bind; `burl` at `accepted`; idempotency key
`(trace_id, nav_gen, refresh_gen, slot, id_kind, id_value)`.
**Dispatch mechanics (normative):** macros are expanded server-side
only; the client fires
`fetch(url, {method: "GET", mode: "no-cors", credentials: "omit",
redirect: "follow", referrerPolicy: "no-referrer", keepalive: true})`;
on synchronous failure the fallback is a detached `Image()` request; no
retries either way. Every dispatch emits
`notification_sent{kind, notif_id, result: queued | failed}` with the
**server-minted `notif_id`** (12-char token delivered with the bid).
Duplicates: hermetic exactly-once proof + production detection alarm +
billing reconciliation (lossy telemetry cannot prove a zero).

**G4e — Fallback.** Opt-in; child attempt (own `attempt_id`,
`parent_attempt_id`, `flow = fallback`); renders only after the parent's
attributed `render_terminal{failed, gam_empty}`; publisher-initiated or
unattributable never triggers; timeouts never render.

**G4f — Direct `/auction` lifecycle and the AuctionBatch.** A single
`/auction` fetch may serve several slots, so cancellation is
batch-aware: an **`AuctionBatch`** owns the fetch (its `AbortController`)
and the child `RenderAttempt`s. Supersession cancels **children
individually** (`render_terminal{cancelled}`); the fetch aborts only
when every child is dead or the batch times out or its navigation
disposes; every response bid is filtered through the **currently live**
child identity before any effect. Tests: partial overlap, full overlap,
timeout, navigation disposal, reversed responses. The auction client
returns a discriminated result — `{ok: bids[]} | {error:
"auction_timeout" | "network_error" | "http_error" | "invalid_response"}`
(§3.14); only a parsed empty response is `no_bid`. Public API:
`tsjs.requestAds(options): Promise<RequestAdsResult>`,
`RequestAdsResult = {traceId, slots: [{slot, outcome, reason?}]}`,
settling when every child attempt is terminal.

**G4g — Mid-attempt configuration, honestly scoped.** Attempts snapshot
configuration at creation. **Commit = the earliest irreversible action**
(first of: notification dispatch, `bridge_response_sent`, first DOM
insertion), with the generation/kill check immediately before each.
**The kill switch's delivery is page-bound:** already-loaded pages have
no push channel, so live switch state travels only on responses the
page later fetches — page-bids and `/auction` responses carry
`ext.trusted_server.switches`, and new HTML carries current state. The
guarantee is therefore scoped: the switch affects attempts created
after the page received switch state; SSAT attempts on already-loaded
pages are unaffected by design, and the spec says so rather than
implying a live channel that does not exist.

### G5 — Deployment contracts

- Config verification per §0. Assets pre-materialized at release
  publication (config is a release-time input); serving is lookup-only;
  unknown vector = build error; unknown hash = `410 no-store`; exact
  match = `public, max-age=31536000, immutable`.
- **Internal route families — four** (renderer, client-events,
  CSP-report, `/_ts/trace-auth`): dispatch before auth/EC/publisher/
  integration filters (`app.rs:709` orders these wrong today); all
  methods + version prefixes reserved locally (405 + `Allow` +
  `no-store`; unknown version 404 `no-store`; no publisher fall-through,
  `adapter-spin app.rs:804`); no body/cookie/authorization forwarding.
  **Per-family origin policy:** client-events + trace-auth strict
  normalized same-origin; CSP admits opaque/`null` origins with path
  identity + limits; renderer is a public GET validated by version/path.
- Ingest routes in all four adapters; Fastly has real sinks; others
  accept-count-drop (DR-5). §5.6 schemas deploy before writers.

## 5. Observability

### 5.1 Wire payload and field matrix

```
{ v: 1, traces: [ { trace_id, auth, events: [
  { nav_gen, refresh_gen, seq, flow, attempt_id?, parent_attempt_id?,
    auction_id?, t_rel_ms?, t, ...fields } ] } ] }
```

| `t`                        | fields                                        | allowed `flow`          |
| -------------------------- | --------------------------------------------- | ----------------------- |
| `bid_received`             | slot, id_kind, source                         | render flows            |
| `targeting_set`            | slot, id_kind                                 | render flows            |
| `attempt_started`          | slot, source                                  | render flows            |
| `bridge_request`           | slot, id_kind, matched                        | ssat, prebid, page_bids |
| `bridge_response_sent`     | slot, source                                  | ssat, prebid, page_bids |
| `render_terminal`          | slot, outcome, reason?, source?               | render flows            |
| `gam_nonempty`             | slot                                          | ssat, prebid, page_bids |
| `gam_empty`                | slot                                          | ssat, prebid, page_bids |
| `gam_collapsed`            | slot, action (`resized`\|`guarded`), reason?  | ssat, prebid, page_bids |
| `renderer_document_loaded` | slot                                          | render flows            |
| `runner_loaded`            | slot                                          | render flows            |
| `runner_failed`            | slot, reason                                  | render flows            |
| `adm_document_loaded`      | slot                                          | render flows            |
| `fallback_start`           | slot                                          | fallback                |
| `notification_sent`        | slot, kind (`nurl`\|`burl`), notif_id, result | render flows            |
| `client_queue_overflow`    | dropped (count)                               | system                  |
| `heartbeat`                | probe_run_id, expected_seq, adapter, target   | system                  |

Render flows = `ssat | prebid | page_bids | direct | fallback`.
`attempt_started` carries the attempt's `t_rel_ms` baseline; the latency
metric is `render_terminal{accepted}.t_rel_ms −
attempt_started.t_rel_ms` per attempt. `source` on `render_terminal` is
nullable for pre-source reasons (`gpt_absent`, `pbjs_absent`,
`slot_unresolved`, `intent_no_request`, `abi_mismatch`, `registry_full`,
`bundle_partial`); the per-event/per-reason validity matrix is a
generated artifact (§6.7). Reason enum: `renderer_document_no_load`,
`runner_no_load`, `runner_failed`, `descriptor_invalid`,
`invalid_dimensions`, `dimensions_out_of_range`, `bridge_id_mismatch`,
`cycle_unattributable`, `intent_no_request`, `stale_navigation`,
`bridge_claim_timeout`, `gam_empty`, `no_render_source`,
`slot_unresolved`, `gpt_absent`, `pbjs_absent`, `bundle_partial`,
`fallback_cancelled`, `abi_mismatch`, `registry_full`,
`currency_mismatch`, `auction_timeout`, `network_error`, `http_error`,
`invalid_response`, `adm_document_no_load`.

### 5.2 Transport, batching, and budgets (limiter-consistent)

- Batches are capped by **both** 64 events **and** 12 KiB encoded
  payload (headroom under the 16 KiB ingest cap); a flush drains the
  queue as up to **4 sequential batches**.
- Cadence: flush every **10 s** and on `visibilitychange`/`pagehide`.
  Worst-case honest traffic per tab: 6 flushes/min × ≤ 4 batches = ≤ 24
  requests/min transient, typically ≤ 6.
- **Budgets derived from that worst case:** client-side trace budget
  ≤ 8 batches/min sustained (excess coalesces into the next flush);
  ingest per-address budget **60 req/min, burst 120** (≈ 5 active tabs
  plus pagehide bursts). The limiter can no longer reject honest
  steady-state traffic by construction; tests cover sustained
  single-tab, multi-tab (5), diagnostic pre-upgrade buffer flush, and
  pagehide bursts.
- Transport: `fetch(..., {keepalive: true, credentials:
"same-origin"})`; `pagehide` fallback `sendBeacon(url, new
Blob([json], {type: "application/json"}))`. Queue bound 256; overflow
  uses the out-of-band saturating counter + one coalesced
  `client_queue_overflow` in the next flush (never enqueued into a full
  queue).

### 5.3 Signed authorizations

**Trace authorization** `v1.<kid>.<exp>.<mode>[.<dexp>].<sig>` (`auth`
ingest bound 256 bytes; all other strings 64):

- `kid ^[a-z0-9-]{1,16}$`; keys ≥ 256-bit CSPRNG in the secret store;
  previous keys retained ≥ 24 h; missing key with the feature enabled →
  loud first-use failure.
- `exp` canonical decimal; ±60 s skew; ≤ 15 min future. `mode`:
  `sampled | unsampled | diagnostic | probe`. **`dexp` is present iff
  `mode = diagnostic`** — the immutable diagnostic ceiling, signed into
  the token, set at upgrade to the credential's absolute expiry.
  **Every renewal of a diagnostic token re-derives
  `exp = min(now + 15 min, dexp)` and preserves `dexp`; past `dexp`,
  renewal fails** — diagnostic access is bounded by the credential
  forever, not just at upgrade (closing the indefinite-renewal hole).
  Tests: renewal-before-expiry capped, repeated renewal to the ceiling.
- `sig` = unpadded base64url HMAC-SHA-256 over
  `"ts-trace-auth-v1" || u32be(len(origin)) || origin ||
u32be(len(trace_id)) || trace_id || u32be(len(mode)) || mode ||
u64be(exp) [|| u64be(dexp)]`; constant-time compare; per-group
  rejection at ingest. `unsampled` transmits nothing and is rejected if
  carried. **Probe issuance protocol:** the probe runner authenticates
  to `POST /_ts/admin/probe-authorization` (admin auth + CSRF) and
  receives a batch of pre-signed probe-mode tokens tagged
  `probe_run_id`; probe traffic is never sampled out and excluded from
  product metrics by mode.
- Renewal: `GET /_ts/trace-auth` presenting the current still-valid
  token in `X-TSJS-Trace-Auth`; re-signs same trace + mode (+ `dexp`).
  **Diagnostic upgrade** is the sole mode transition:
  `POST /_ts/trace-auth/upgrade` presenting token + credential.

**Diagnostic credential** `d1.<kid>.<exp>.<oh>.<sig>` — full byte-level
spec with vectors: `oh` = first 16 hex chars of SHA-256 of the
externally visible origin (scheme+host+port, UTF-8); `sig` = unpadded
base64url HMAC-SHA-256 over `"ts-diag-cred-v1" || u32be(len(origin)) ||
origin || u64be(exp)`; same kid charset, key strength, rotation, and
≥ 24 h previous-key retention; ±60 s skew; absolute expiry ≤ 60 min;
constant-time compare; replayable short-lived bearer by design (bounded
by expiry + origin). Issued `POST /_ts/admin/diagnostic-credential`
(admin auth, CSRF: same-origin + custom header). Transport: `#tsdiag=`
fragment → read synchronously, cleared via `history.replaceState`, held
in memory only (RuntimeSession); pre-upgrade events buffer locally
(bounded 256) and flush after upgrade. Forgery, wrong-origin,
replay-past-expiry tests.

Lazy cached initialization applies to every secret-backed component;
failure with the feature enabled is that feature's loud error path.

### 5.4 Ingest and rate limiting

- `POST /_ts/client-events`: `application/json` only; no
  `Content-Encoding`; `204`, `no-store`; never echoes input. Pre-parse:
  body ≤ 16 KiB; ≤ 64 events; strings ≤ 64 (`auth` ≤ 256 B);
  `trace_id ^[0-9a-f]{32}$`; `attempt_id ^[a-z0-9]{8}$`; `auction_id`
  canonical UUID; integers `[0, 2³¹)`; `t_rel_ms` u32.
- Same-origin (client-events, trace-auth): `Sec-Fetch-Site:
same-origin` else normalized `Origin` equality; absent both →
  drop-and-count.
- Limiter (per §5.2 budgets): trait `ClientEventLimiter`; Axum real
  token bucket (60/min, burst 120), map ≤ 65,536; Cloudflare/Spin
  best-effort ≤ 4,096/instance; TTL 10 min, cleanup on access + sweep;
  at capacity reject unseen identities (expired always reclaimable);
  unknown address → shared bucket 6/min; Fastly platform 60 s window at
  limit 120 (approximation; overshoot bounded only by in-flight
  concurrency — documented by the synchronized-burst test, no numeric
  multiple claimed). Limiter unavailable → drop early with `204`.
  Trusted address per adapter (Fastly platform IP; Axum rightmost XFF
  after `trusted_proxy_hops`, absent → socket peer; CF
  `CF-Connecting-IP`; Spin platform). Trace-auth and CSP routes carry
  their own buckets (10/min, burst 20).

### 5.5 Sinks, canonical views, per-sink authenticated probes

- Event key `(publisher_domain, trace_id, seq)`; canonical views
  `ts_client_events_v` (dedup) and `ts_render_attempts_v` (**keyed by
  `attempt_id`**); the arm-union views stamp `deployment_pool` from the
  write identity (§0). Dashboards/alerts query canonical views only.
- The Fastly sink is fire-and-forget (`tinybird.rs:153`) —
  **per-datasource authenticated probes**: every probe row carries
  `{probe_run_id, expected_seq, adapter, target}`; client-events via
  `heartbeat` events under probe-mode tokens; CSP via probe reports to a
  **secret-derived probe `policy_id`** (registered like any policy id,
  not guessable — `policy_id = "probe"` would be publicly forgeable);
  ops via an authenticated probe counter write. **Persistence gates are
  scoped to sink-backed adapters** (DR-5); accept-count-drop adapters
  get HTTP-parity gates only. Freshness = probe lag ≤ 5 min; loss =
  `expected_seq` gaps < 0.1%; alert owner: release owner's on-call.
- **Alert-delivery drill:** a synthetic canary page injects a known
  failure class at a known rate; the failure-detection alert must fire
  within one hour — dashboards and alert latency are tested, not
  assumed (Appendix A row).

### 5.6 Physical schemas (deployed before writers)

- **`ts_client_events`**: `received_at DateTime64, publisher_domain
LowCardinality(String), release_id String, deployment_pool
Enum(canary|control), assignment_id Nullable(FixedString(32)),
trace_id FixedString(32), mode Enum(sampled|diagnostic|probe), nav_gen
UInt32, refresh_gen UInt32, seq UInt32, flow
Enum(ssat|prebid|page_bids|direct|fallback|system), attempt_id
Nullable(FixedString(8)), parent_attempt_id Nullable(FixedString(8)),
auction_id Nullable(UUID), t_rel_ms Nullable(UInt32), event Enum(§5.1),
slot Nullable(String), id_kind Nullable(Enum), matched Nullable(UInt8),
source Nullable(Enum), reason Nullable(Enum), outcome
Nullable(Enum(accepted|failed|no_bid|cancelled)), action
Nullable(Enum), kind Nullable(Enum), notif_id Nullable(FixedString(12)),
result Nullable(Enum), dropped Nullable(UInt32), probe_run_id
Nullable(String), expected_seq Nullable(UInt32), adapter
Nullable(Enum), target Nullable(Enum)`. Sorting key `(publisher_domain,
received_at, trace_id, seq)`; TTL 30 days; sink batch cap 512; startup
  validation; sink-unavailable → accept-count-drop.
- **Auction rows** (`telemetry.rs:262`, `auction_events_raw.datasource`):
  add nullable `trace_id`, `mode`, `release_id`;
  `row_kind Enum(slot|totals|overflow)`; `bid_drop {row_kind, provider
Nullable(LowCardinality(String)), slot Nullable(String), reason
Enum(AuctionDropReason), width Nullable(UInt16), height
Nullable(UInt16), count UInt32}` (32 slot-rows + one overflow row whose
  `count` = actual dropped bids); `selection_summary {row_kind, slot
Nullable(String), winner_source Nullable(Enum(mediator|direct|none)),
winner_provider Nullable(String), candidates_direct UInt16,
candidates_mediator UInt16, dedup_hits UInt16, currency_rejected
UInt16, provenance_invalid UInt16, mediator_superseded UInt16}` (8
  slot-rows + one totals row that survives truncation; saturating
  counters `0xFFFF`/`0xFFFFFFFF`).
  - **`AuctionDropReason` (closed, exhaustive over baseline producers,
    one shared typed enum — no string literals):**
    `script_rendering_disabled, invalid_dimensions,
dimensions_out_of_range, missing_render_source, invalid_creative_url,
unsupported_tagtype, render_payload_too_large,
unexpected_response_shape, currency_mismatch, floor_rejected,
provenance_invalid, duplicate_demand, missing_bid_id,
duplicate_bid_id, bid_id_too_large, empty_seatbid,
empty_seatbid_bids, unknown_impid, invalid_price,
unsupported_media_type, creative_id_too_large,
renderer_extension_serialization_failed, no_render_source,
lost_to_higher_bid, overflow` — covering `aps.rs:740-929`
    (incl. `empty_seatbid_bids` at `:875`) and `formats.rs:408-419`; a
    **compile-time exhaustiveness test maps every producer to the enum**.
- **`ts_csp_reports`**: `received_at DateTime64, publisher_domain
LowCardinality(String), release_id String, policy_id
LowCardinality(String), cohort LowCardinality(String), directive_bucket
Enum(script|style|frame|img|connect|font|media|worker|other),
source_bucket Enum(https_host_allowlisted|data|blob|inline|eval|other),
count UInt32`; sorting key `(publisher_domain, received_at, policy_id)`;
  TTL 30 days. Ingest: body ≤ 8 KiB, ≤ 10 reports/request, strings ≤ 256,
  nesting ≤ 4, both media types with separate validators, unused fields
  discarded, own limiter bucket. **Caps with values:** 10,000
  reports/hour/publisher and 1,000/hour/cohort; overflow increments the
  `csp_overflow` ops counter (dropped reports counted, never parsed
  further).
- **`ts_ops_counters`**: `received_at DateTime64, publisher_domain
LowCardinality(String), release_id String, counter
Enum(renderer_requests|renderer_unknown_version|renderer_auth_blocked|
ingest_accepted|ingest_dropped|ingest_rate_limited|abuse_flagged|
csp_overflow|probe), value UInt64`; sorting key `(publisher_domain,
received_at, counter)`; TTL 90 days.
- **Sink plumbing:** one generic multi-target Tinybird sink trait;
  `RuntimeServices` (`platform/types.rs:158`) gains handles for
  client-events, CSP, and ops targets beside the auction sink; each
  target has its own dataset + token settings.
- **Settings:** `[telemetry.client_events] collection_enabled,
sink_enabled, sample_rate, api_host, dataset, token_secret,
secret_store, max_body_bytes`; `[telemetry.csp_reports]` and
  `[telemetry.ops_counters]` (same transport shape);
  `[telemetry.trace_auth] secret_store, active_kid, previous_kids,
sampling_key_secret`; `[telemetry.diagnostic] secret_store,
active_kid`; `[telemetry.probe]` admin-issued run configuration.
- APS parsing returns structured drop observations
  `{reason, slot, width?, height?}` (`aps.rs:722` loses slot/values);
  > 8192 → `dimensions_out_of_range` unclamped.

### 5.7 Modes and SLIs

Production (sink-backed): deterministic sampling (0.10). SLIs: pipeline
availability (per-sink authenticated probe freshness ≤ 5 min, loss
< 0.1%); failure detection (≥ 1% of sampled render attempts visible
within one hour at ≥ 10,000/hour) — **verified by the injected-failure
alert drill**, not only by probe persistence. Diagnostic:
credential-gated, `dexp`-bounded, unsampled, full stream + console
mirroring + debug envelopes.

### 5.8 Server-side drop surfacing

Bounded structured summary whenever any bid is dropped; `bid_drop` +
`selection_summary` rows; `ts-debug` comment; tester-gated structured
`debug` on page-bids and `/auction`. Startup warnings: APS +
`allow_script_creatives = false`; mediator + direct providers without
explicit `winner_selection`.

## 6. APS delivery fixes

### 6.1 Mediation

Eight rules, arrival-independent, one shared helper across both
lifecycles: (1) required `[auction].currency` (APS + non-USD = startup
error; Prebid validates at parse, `prebid.rs:2433`); (2) candidate
identity `source_candidate_id = (provider_name, upstream_bid_id)` with
the upstream id **required, ≤ 64 chars, unique** — missing/duplicate/
oversized → `bid_drop{missing_bid_id | duplicate_bid_id |
bid_id_too_large}`, **no fingerprint fallback**; `candidate_id` (CSPRNG
wire echo, never an ordering key); (3) mediator echoes
`ext.trusted_server.candidate_id` — resolves → forwarded candidate,
provenance `mediator`, **price authoritative from the mediator, every
render-source and notification field from the stored candidate, deal
fields out of scope**; any render-source difference → mediator-native;
unresolvable echo → discarded + counted (`provenance_invalid`),
mediator-native and direct candidates stay eligible; (4) floors both
populations, echoes dedup twins; (5) **total order** decoded CPM desc →
provenance rank (mediator first) → `source_candidate_id` asc; (6)
required `winner_selection` (`mediator_only`: timeout → no winners
unless `mediator_timeout_fallback = "direct"`; `merge_highest_cpm`:
timeout → direct-only); (7) `selection_summary` reporting.

### 6.2 Dimensions

Exact membership (`aps.rs:675`); structured `bid_drop{invalid_dimensions,
w, h}`; "request the sizes you accept."

### 6.3 Script creatives

Default `false`, loud (§5.8); **DR-2 is a deployment decision** (enable
with security approval, or accept a quantified excluded share and gate
Phase 3 on it).

### 6.4 / 6.5

Render identity per G2; fallback per G4e (child attempt).

### 6.6 Renderer endpoint

Unconditional route in every adapter (provider stays config-gated);
startup auth-pattern validation; `/integrations/aps/renderer/v1`
embedded, served `public, max-age=31536000, immutable` with a
checked-in per-version header manifest (headers frozen with bytes);
canary versions `no-store`; unknown version 404 `no-store`;
three-message ack (G4b-1); aggregate route counters in
`ts_ops_counters`; CSP rollout three instruments (enforced discovery /
report-only tightening / enforced-cohort relaxation on a short-lived
canary version, gated on runner acceptance, violation rate, render
failure, with a kill switch); **policy identity in the server-selected
`policy_id` path** (also the release/cohort carrier, §0); bucketed
aggregation only with the §5.6 caps; never a sole rollback signal;
three-browser capture (`playwright.config.ts:16`).

### 6.7 One descriptor schema

Tagged `BidRenderer` envelope (`types.rs:188-211`); wire-schema
crate/xtask (no core↔js cycle, `Cargo.toml:45`) generates JSON-Schema,
TS parser, ES5 inline fragment, the §5.1 validity matrix, and fixtures;
staleness CI; semantic validators handwritten; outer tolerance only;
exact AAX projection; shared positive + adversarial corpus through all
three validators.

### 6.8 Bridge hardening

Order (normative — preserving the baseline stolen-capability defense,
`gpt/index.ts:1584-1637`, **plus the read-only lookup the altered-id
signal needs**):

1. parse `e.data` (bare catch → return);
2. **read-only source→active-slot lookup for every Prebid Request** — if
   the resolved slot expects a different ad id, emit
   `bridge_request{matched: false}` (the B1 signal) **without responding
   and without suppressing native Prebid** (a truncated `hb_adid` is not
   TS-reserved, so suppression would be wrong and the old order could
   never produce the signal);
3. identify a TS-reserved ad id (live registry or tombstone, G2);
4. if TS-reserved: `stopImmediatePropagation()` before validation;
5. validate source ownership (known slot-root `WindowProxy` map;
   sender's parent chain to depth 5; never scanning the frame tree);
6. validate nonce, token, `nav_gen`, `refresh_gen` (G4b);
7. respond, or refuse with `bridge_id_mismatch`.

Non-TS ids are otherwise untouched. Stolen-token test: neither TS nor
native Prebid responds. Listener order: real-browser assertion.

## 7. TSJS target architecture

### 7.1 Layering

```
kernel/          boot, config, queue, event bus, log, beacon, sessions
adapters/        googletag.ts, pbjs.ts, messaging.ts   ← only access to external ad-tech globals
services/        slots (registry+handoff), auction client, render engine, consent
integrations/    gpt, prebid, aps, creative, datadome, …
```

Kernel imports nothing above it; adapters import kernel only; services
import kernel + adapters; integrations import kernel + services, never
each other; stateful services via the G3 registry only; enforced per
G3's two rule families.

### 7.2 Adapters

`present | pending | timed_out` per external global; `timed_out`
non-terminal; queued operations carry their own timeouts and expire with
disposition reasons.

### 7.3 Slot registry service

Kernel-owned; `WeakMap<googletag.Slot, SlotRecord>` + div-id index;
ownership, adoption, handoff claims, responsive resolution, the G4a
causal intent queue (NavigationSession for unissued intents) and
cycle/drain state (RuntimeSession), targeting history. No expandos
(`__tsRenderGeneration`/`__tsRenderBid` deleted).

### 7.4 Final global surface (hard cutover)

| Legacy surface (removed at cutover)     | Final shape                                                           |
| --------------------------------------- | --------------------------------------------------------------------- |
| `window.tsjs.que`                       | `window.tsjs.que` — unchanged                                         |
| `globalThis.tscreative`                 | `tsjs.creative.*`                                                     |
| `globalThis.tsCreativeConfig`           | `tsjs.boot.creative`                                                  |
| `requestAds` (void)                     | one async `tsjs.requestAds(options): Promise<RequestAdsResult>` (G4f) |
| `window.__tsjs_*` flags, config globals | `tsjs.boot.*`                                                         |
| install manifest                        | `tsjs.boot.manifest` (`{release_id, plugins: [{id, order}]}`)         |
| expandos / function sentinels           | `SlotRecord` fields / kernel `WeakSet`                                |
| `tsjs._internal`                        | kernel registry (G3), frozen after boot                               |
| (new, public)                           | `tsjs.definePlugin({id, release, install})`                           |

Bootstrap: field-wise idempotent init (`window.tsjs ||= {}; tsjs.que
||= []; tsjs.boot ||= {}`; `publisher.rs:3665`'s clobber fixed);
transactional ownership `unclaimed → installing → kernel | fallback`
with an owner-generation counter; kernel installs inert and flips at one
commit point; throws unwind to `failed`; the 10 s watchdog aborts the
owner-generation-scoped controller, completes the shared unwind, then
atomically transitions `failed → fallback`; late continuations and
disposers validate the owner generation and self-discard; a bundle
arriving after fallback committed defers (`bundle_partial`). Tests:
throws per checkpoint and hung-resume-after-fallback.

### 7.5 Messaging module

All `postMessage` through one module: versioned envelopes, name
constants, G4b nonces, §6.8 validation. Minimal module in Phase 1; full
migration in Phase 4.

### 7.6 Plugins and sessions

`tsjs.definePlugin({id, release, install})`; **no plugin-level dispose
hook** — `ctx.onDispose` only, exactly-once reverse order.
`install(ctx): void | Promise<void>` with `ctx.signal`, unwind on
throw/reject/abort, per-disposer isolation, disposer-after-disposal
invoked immediately, pending capacity 16 / 10 s → `bundle_partial`,
release mismatch quarantined before install. Sessions: `RuntimeSession`
(bridge listener + reservation store, history hook, pbjs subscriptions,
adapters, beacon queue, cycle/drain state, in-memory diagnostic
credential), `NavigationSession` (trace + auth + renewal timer,
attempts, aliases, unissued intents, targeting history),
`RenderAttempt`/`AuctionBatch`; enumerable disposal inventories. No
empty `catch`; console logging retained (paired `warn` with the beacon
reason; `debug`-level delivery/security failures promoted).

### 7.7 Bootstrap

`gpt_bootstrap.js` shrinks to a queue-and-flags stub; the bundle replays
recorded calls; the no-bundle fallback is generated from the same
TypeScript source (pinned by `gpt.rs:1174-1179`), activated per §7.4.

### 7.8 GPT correctness carried with the restructure

Unconditional early `slotRequested`/`slotRenderEnded` subscription
(replacing `gpt/index.ts:1091`'s gate); restore #922/#997 (DR-3);
`changeCorrelator: false` (configurable); `enableSingleRequest()` only
when services are not already enabled; ambiguous responsive resolution
emits `render_terminal{failed, slot_unresolved}` alongside its warning.

### 7.9 Decomposition targets

| Today                                | Target                                                                                                             |
| ------------------------------------ | ------------------------------------------------------------------------------------------------------------------ |
| `gpt/index.ts` (~1850 LOC)           | `slot_resolution`, `handoff`, `initial_load`, `ad_init`, `render_bridge`, `spa_navigation`, `beacons` + thin index |
| `prebid/index.ts` (1671 LOC)         | adapter, shim, refresh handler (onto the slot registry), eids, diagnostics                                         |
| `gpt/script_guard.ts` (634 LOC)      | folded onto the 170-LOC `shared/script_guard.ts` factory                                                           |
| `core/trace.ts` (model + UI)         | `services/trace` + `integrations/trace_overlay`                                                                    |
| `core/global.d.ts` (`pbjs: TsjsApi`) | real Prebid types; `TsjsApi` split public vs internal                                                              |

### 7.10 Performance (reproducible)

Pinned workflow (`runs-on: ubuntu-24.04` — browser CI is `ubuntu-latest`
today, `integration-tests.yml:155` — inside a pinned container digest);
lockfile-resolved Playwright with recorded browser revision
(`browser/package.json:10` is a caret range); pinned compressors
(`gzip -9 -n`, `brotli -q 11`). Vectors: minimal `[core]`; reference
`[core, creative, gpt, prebid, datadome]`; maximal all 13. Budgets vs
checked-in baselines (+5% bytes; baseline records image/browser/tool
versions and is invalid if any differ). Browser timing:
`performance.mark("tsjs:bids-script")` →
`performance.mark("tsjs:first-display")`; reference fixture; warm cache;
local resources; 5 warm-ups, 50 samples, nearest-rank p90 ≤ baseline ×
1.10; inconclusive (3-run agreement > 5%) → one rerun, then fail. **Peak
JS heap, reproducibly:** via the Playwright CDP session —
`HeapProfiler.collectGarbage` then `Runtime.getHeapUsage`, sampled at
five fixed points (post-boot, post-adInit, post-first-render,
post-refresh, post-SPA-navigation) on the maximal vector; metric = max
sample; gate ≤ baseline × 1.10; same rerun rule. Server benchmark: the
G5 lookup path; 100 warm-ups, 1,000 iterations; median + p90 one-sided
≤ baseline × 1.10; 3-run 5% agreement or inconclusive.

### 7.11 Toolchain

TypeScript floor to the resolved 5.9 line; release-gating flags
`strict`, `noUncheckedIndexedAccess`, `exactOptionalPropertyTypes`,
`verbatimModuleSyntax`, `noImplicitOverride`,
`useUnknownInCatchVariables`; checked-in npm script `typecheck`:

```
cd crates/trusted-server-js/lib && npx --no-install tsc -p tsconfig.json --noEmit
```

Dev toolchain bumps as individual CI-gated PRs; `prebid.js` excluded
from casual bumps; monthly review.

## 8. Rollout

Single-release state machine per §0. **Statistical method:** sampled
traces only (diagnostic reported separately); **randomization unit =
`assignment_id`** (minted inside the affinity token, §0; persisted on
client-event rows), so attempts cluster by session; the estimator is a
**checked-in cluster bootstrap** (`scripts/gates/estimator.py`):
resample assignment ids, 2,000 resamples, fixed seed recorded in the
gate artifact, strata (publisher × slot) weighted by control-arm
traffic share; one-sided 95% confidence bounds on **relative
differences**; each gate is an independent go/no-go (no cross-gate
multiplicity correction — stated). Missing telemetry counts as failure.
Per-flow floors; rare flows (direct, fallback) gate hermetically +
real-GAM, never statistically. `cycle_unattributable` divides by all
attribution-candidate TS cycles. **Billing cohort dimension in external
reports:** TS-driven GAM requests set a `ts_arm=<cohort>` key-value,
making the arm a reportable GAM dimension for reconciliation.

**Phase 0 decision records:** DR-1 mediator presence; DR-2 script
creatives (deployment decision); DR-3 #997 vs re-merge; DR-4
candidate-id echo owner (`merge_highest_cpm` config-blocked until
delivered); DR-5 non-Fastly sinks (splits Phase-2 gates, scopes
persistence gates).

Phases: **0** identity/schemas/toolchain/DRs + gate artifacts
(pipes/scripts/workflows) + observation-only control build definition;
**1** kernel/sessions/minimal messaging/cycle registry/transactional
bootstrap — Phase-1 gates are **hermetic** (in-page counters; beacon
transport does not exist until Phase 2); **2** trace + beacon + renewal

- diagnostic upgrade + probe issuance + four-adapter ingest + per-sink
  probes + the alert drill; **3** APS delivery (schema crate + corpus;
  mediation; render token + reservation store; renderer route +
  three-message ack + CSP route; §6.8; G4a–G4g incl. AuctionBatch;
  `notification_sent`; fallback; DR-3 restoration; correlator + SRA);
  **4** structure (layering, plugins, adapters, registry, messaging,
  namespace; four-flow parity); **5** decomposition + bootstrap stub +
  parity rerun + **full Phase-3 statistical and real-GAM gates repeated
  on the exact immutable RC** (attested per A.3) before weight-up, then
  cutover per §0.

## 9. Test acceptance matrix

Hermetic CI blocks PRs; the real-GAM suite is release-gating (A.3).

| Area             | Coverage                                                                                                                                                                                                                                                                         |
| ---------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Attempts         | `attempt_id` uniqueness; parent/child fallback linkage; exactly one `render_terminal` per attempt; parent `failed{gam_empty}` precedes `fallback_start`                                                                                                                          |
| Terminal model   | discriminated outcomes incl. `no_bid`/`cancelled`; per-reason `source` nullability; validity-matrix conformance                                                                                                                                                                  |
| Request cycles   | disabled-initial-load `display()` retired for any caller; TS-noop-refresh → TS-display and publisher variants (same-class supersession); SRA; `intent_no_request`; overlap quarantine; no timeout re-arm; stale discard; real-GAM overlap                                        |
| Ack per path     | four G4b sequences; APS three-message; owner-observed ADM `load`/`error` with **nonce never in bidder realm**; bidder-synthesized message ignored; early-`burl` attempt fails; per-path deadlines; stale/replayed; after-disposal                                                |
| Bridge signal    | truncated/replaced `hb_adid` → `bridge_request{matched:false}` with no response and native Prebid untouched; stolen TS ids fully suppressed (neither TS nor native responds); listener order                                                                                     |
| AuctionBatch     | multi-slot partial/full supersession; fetch aborts only when all children dead; stale-bid filtering through live child identity; timeout; navigation disposal; reversed responses; discriminated auction-client errors                                                           |
| Diagnostic       | `dexp` ceiling — renewal-before-expiry capped, repeated renewal to ceiling then failure; credential byte-level vectors; fragment clearing; in-memory storage; pre-upgrade buffering then flush; forgery/wrong-origin/replay                                                      |
| Transport/limits | batch caps (64 events AND 12 KiB); ≤ 4 batches/flush; sustained single-tab, 5-tab, diagnostic flush, pagehide burst inside the 60/120 budget; overflow coalescing; Fastly synchronized-burst documented; unknown-address bucket                                                  |
| Affinity         | token vectors (format, rotation, constant-time); state-dependent defaults (canary vs post-cutover reassignment; no stale-cookie stragglers); coherence for HTML/assets/APIs/beacons; **CSP affinity via `policy_id` path, cookie-less renderer reports**                         |
| Arms/measurement | observation-only control build emits comparable sampled telemetry (its zero-behavior-change gated by hermetic parity vs baseline); arm-specific datasources; union view stamps `deployment_pool`; `assignment_id` on rows; cluster-bootstrap fixture; GAM `ts_arm` key           |
| Latency/fill     | `t_rel_ms` monotonic bounds; `attempt_started`→`render_terminal` durations; per-gate numerator/denominator queries (A.1); named source tables/joins                                                                                                                              |
| Joins            | `Nullable(UUID)` type equality; auction-level vs slot-level vs bid-level canonical joins; raw-join multiplication rejected                                                                                                                                                       |
| Probes           | authenticated probe rows (`probe_run_id`/`expected_seq`/`adapter`/`target`) per datasource; secret-derived CSP probe `policy_id` unforgeable; probe issuance protocol; persistence gates scoped to sink-backed adapters                                                          |
| Alerting         | injected-failure drill: alert fires ≤ 1 h                                                                                                                                                                                                                                        |
| Kill switch      | switch state via HTML and response extensions; attempts created after delivery honor it before each irreversible action (incl. before `nurl`); SSAT-on-stale-page exemption documented and tested                                                                                |
| Drop enum        | `empty_seatbid_bids` + `bid_id_too_large` mapped; shared typed enum across producers; compile-time exhaustiveness                                                                                                                                                                |
| RC attestation   | real-GAM workflow consumes the immutable release manifest `{release_id, bundle hashes, binary hash, config_hash, pool}` and emits it in the attested output; gates parameterized by (release, pool, epoch); RC re-canary inherits each row's window                              |
| Config/affinity  | `config_hash` SHA-256 over exact bytes verified at startup (mismatch = fail); affinity HMAC vectors + rotation + constant-time                                                                                                                                                   |
| Heap             | CDP procedure at the five fixed points; GC before sample; rerun rule                                                                                                                                                                                                             |
| Lint             | custom scope-aware rule catches `window`/`globalThis`/`self` member access and same-file aliases outside adapters (claim scoped to these shapes)                                                                                                                                 |
| Notifications    | dispatch mechanics (no-cors GET, no-referrer, keepalive; `Image()` fallback; server-side macro expansion only); `notif_id` emission; duplicate alarm + reconciliation; hermetic exactly-once                                                                                     |
| Mediation        | required-unique bounded upstream ids (`missing`/`duplicate`/`bid_id_too_large`, no fingerprint); `candidate_id` echo; arrival-order shuffle invariance; authoritative-field rules; provenance fail-closed scope; strategy timeouts; both lifecycles; APS + non-USD startup error |
| Render token     | format/CSPRNG/retry/TTL/one-time; scope; union capacity 320 with `registry_full`; >320 then late oldest-id suppressed                                                                                                                                                            |
| Trace auth       | auth ≤ 256 B; encoding vectors; expiry/skew/max-future; renewal preserves mode; renewal-after-expiry fails; previous-key retention; deterministic sampling (exact u64 threshold; same trace → same mode concurrently)                                                            |
| Internal routes  | wrong-method 405 + `Allow` + `no-store`; unknown version 404; no fall-through; dispatch before filters; no forwarding; per-family origin policies                                                                                                                                |
| CSP              | both media types; opaque/null origin; policy-id path identity (forged body ignored); bucket caps + overflow counter; three-browser capture; per-version frozen header manifest                                                                                                   |
| Schema           | staleness; adversarial corpus ×3 validators; outer tolerance vs exact AAX; generated validity matrix                                                                                                                                                                             |
| ABI/plugins/boot | one kernel; exact-release verdicts; object-form release check; partial-install unwind; abort-pending; disposer-after-disposal; hung-resume self-discard; fallback-then-late-bundle deferral                                                                                      |
| Lifecycle        | `timed_out → present`; disposal inventories; unissued intents cancelled by navigation; boot consume/freeze/delete; final-namespace smoke                                                                                                                                         |
| Delivery         | unknown hash 410 `no-store`; exact-match immutable; release-time vector materialization (unlisted = build error); cutover rehearsal                                                                                                                                              |
| Adapter parity   | ingest, CSP-report, trace-auth, renderer routes and drop surfacing equivalent across Fastly/Viceroy, Axum, Cloudflare, Spin                                                                                                                                                      |
| Policy           | script-creative warning; `invalid_dimensions` w/h; `dimensions_out_of_range` unclamped; `boot.debug` + response `debug` gating; diagnostic completeness; kill-switch snapshot semantics                                                                                          |
| Perf             | marks present; three vectors; heap CDP; inconclusive-rerun; pinned-environment baseline validity                                                                                                                                                                                 |

## 10. Alternatives considered

Revision 8's twelve rejections stand (patch-without-telemetry;
always-direct-render; shared chunks now; big-bang rewrite; dropping the
bootstrap; timeout fallback; timeout re-arm; N/N−1 machinery;
client-computed hashes; fingerprint identities; plain cohort cookie;
`billing_outcome`), plus: **13.** injected in-realm ADM reporter —
rejected (hands bidder code the acceptance credential); owner-observed
`load`/`error` instead. **14.** cookie-routed CSP affinity — rejected
(opaque-origin reports carry no cookie); path-identity routing. **15.**
token-identity alone for arm attribution — rejected (authorization is
not a row dimension); arm-specific datasources + stamped union view.
**16.** indefinite diagnostic renewal — rejected; `dexp` ceiling.
**17.** shared attempt identity for parent/child — rejected; distinct
`attempt_id`.

## 11. Risks

Revision 8's register stands (cutover blast radius; sticky-cohort
infrastructure; mediator wire-contract change; published notification
triggers; required-config startup errors; beacon abuse; memory bounds;
advisory CSP; sink blindness; ABI freeze; schema generation), plus: the
**observation-only control build** is new scoped work whose "zero
behavior change" property is itself gated (hermetic parity vs baseline);
and `assignment_id` persistence is pseudonymous but new — bounded by the
24 h token TTL and excluded from any identity join by schema review.

## 12. Success criteria

1. APS creatives render in each configured flow, hermetically and in the
   attested real-GAM suite.
2. Every §2 failure maps to its §2.5 signal; diagnostic mode names the
   failing class from one page load; §5.7 SLIs hold, including the alert
   drill.
3. Both lint families (incl. the custom scope-aware rule) pass; stateful
   sharing only via the registry; exact-release mismatches quarantine
   loudly.
4. No `src/` file exceeds ~500 lines; `gpt_bootstrap.js` is a stub or
   generated.
5. Exactly one `render_terminal` per `attempt_id`; parent/child fallback
   attempts are distinct rows; attempt aggregation keys on `attempt_id`
   with the tuple as grouping only.
6. The only TSJS-owned global is `window.tsjs` (§7.4); no expandos.
7. §7.10 budgets hold, including the CDP heap procedure.
8. No existing warning lost; issue-surfacing conditions log `warn`+ with
   the beacon reason.
9. TypeScript floor and flags via the checked-in `typecheck` script;
   `prebid.js` pin documented.
10. `nurl`/`burl` only on carrying paths at their binds with the
    normative dispatch mechanics; APS fires neither; hermetic
    exactly-once + production alarm + reconciliation.
11. Trace-bearing responses `private, no-store`; authorizations signed,
    mode-carrying, renewal-preserving, diagnostic bounded by `dexp`;
    unsampled transmits nothing.
12. Cutover rehearsed; config-hash verification enforced; post-cutover
    routing defaults flip (no stale-cookie stragglers).
13. Phase-3 statistical and real-GAM gates pass on the attested
    immutable RC before weight-up.
14. Appendix A shipped with this design; changes carry decision records.
15. Baseline APS fix behaviors re-implemented; baseline browser tests
    pass unmodified.

## 13. Open questions

One: does Amazon expose any creative-completion acknowledgement that
could add a post-`accepted` state under a new name (future
enhancement)?

---

## Appendix A — Normative rollout gates (initial values)

Roles: **RO** release owner, **QA** QA owner, **OPS** on-call.
Randomization unit: `assignment_id` (§0). Statistical gates: sampled
traces, cluster bootstrap (§8), canonical views only, checked-in
artifacts (`tinybird/pipes/gate_*.pipe`, `scripts/gates/*.sh`,
`.github/workflows/*`). Every statistical row specifies
numerator/denominator/source; missing rows count as failure. "Hold" =
weight frozen; "Rollback" = weight back + re-purge. Changes require
decision records. Low volume: inconclusive → extend once → Hold.

### A.1 Phase gates

| Phase | Gate                          | Artifact                               | Numerator / denominator (source)                                                         | Floor           | Threshold                                          | Window                    | Owner | Action   |
| ----- | ----------------------------- | -------------------------------------- | ---------------------------------------------------------------------------------------- | --------------- | -------------------------------------------------- | ------------------------- | ----- | -------- |
| 0     | Dark-pool health              | `probe-pool.sh`                        | expected responses / probe requests                                                      | 1,000           | 100%                                               | 24 h                      | OPS   | Hold     |
| 0     | Schema validation             | `schema-writes.sh`                     | accepted / deterministic synthetic rows (all tables)                                     | 10,000          | **0 rejections**                                   | 24 h                      | RO    | Hold     |
| 0     | Asset identity                | `asset-probe.sh`                       | correct status / probed hashes                                                           | all             | 100%                                               | once                      | QA    | Hold     |
| 0     | Config binding                | `config-hash.sh`                       | verified pools / pools                                                                   | all             | 100%                                               | once                      | OPS   | Hold     |
| 1     | Kernel/bootstrap (hermetic)   | `bootstrap-ownership.spec` + counters  | passing cases / cases (no beacon dependency)                                             | all             | 100%                                               | CI                        | QA    | Hold     |
| 2     | Ingest HTTP parity            | `ingest-parity.sh`                     | passing / parity cases (4 adapters × 4 families)                                         | all             | 100%                                               | CI                        | QA    | Hold     |
| 2     | Persistence (sink-backed)     | `gate_ingest.pipe`                     | accepted probe events / sent (probe tokens)                                              | 10,000          | ≥ 99%; dedup exactly-once                          | 24 h                      | OPS   | Hold     |
| 2     | Per-sink authenticated probes | `gate_probes.pipe`                     | on-time probe rows / expected (`probe_run_id×seq`), per datasource × sink-backed adapter | 1,000 each      | lag ≤ 5 min; loss < 0.1%                           | 24 h                      | OPS   | Hold     |
| 2     | Alert drill                   | `alert-drill.sh`                       | alerts ≤ 1 h / injected failure episodes                                                 | 3 episodes      | 100%                                               | 24 h                      | OPS   | Hold     |
| 3     | Funnel ssat/prebid/page_bids  | `gate_funnel.pipe`                     | per A.2 stage pairs (`ts_render_attempts_v` ⋈ slot-level auction rows)                   | 10,000/flow-arm | per A.2                                            | 24 h                      | RO    | Rollback |
| 3     | Direct/fallback conformance   | hermetic + A.3 rows                    | passing / suite cases                                                                    | all             | 100%                                               | CI + RG                   | QA    | Hold     |
| 3     | Attribution soundness         | `gate_cycles.pipe`                     | `cycle_unattributable` / **all attribution-candidate TS cycles**                         | 10,000          | < 0.5%                                             | 24 h                      | RO    | Rollback |
| 3     | GAM fill                      | `gate_fill.pipe`                       | nonempty `slotRenderEnded` / TS request cycles, canary vs control                        | 10,000/arm      | 1-sided 95% CB rel. diff ≥ −2%                     | 24 h                      | RO    | Rollback |
| 3     | Latency                       | `gate_latency.pipe`                    | p95 of (`render_terminal{accepted}.t_rel_ms − attempt_started.t_rel_ms`) per arm         | 10,000/arm      | 1-sided 95% CB rel. diff ≤ +2%                     | 24 h                      | RO    | Rollback |
| 3     | Billing                       | `gate_billing.pipe` + GAM `ts_arm`     | revenue per 1,000 attempts per arm (GAM report ⋈ attempts)                               | 100,000/arm     | 1-sided 95% CB rel. diff ≥ −2%                     | 7 d (+7 d ext.)           | RO    | Rollback |
| 3     | Duplicate `burl` alarm        | `gate_dup_notif.pipe` + reconciliation | duplicate `notification_sent{burl}` per `notif_id` / dispatches; GAM-vs-server deltas    | 1,000           | 0 observed; reconciliation within 1%               | 24 h / billing wnd        | RO    | Rollback |
| 4     | Layering + leaks              | lint CI + `disposal-inventory.spec`    | —                                                                                        | —               | 0 exceptions / 0 leaks                             | CI                        | QA    | Hold     |
| 4     | Four-flow parity              | `flow-parity.spec`                     | passing / parity cases                                                                   | all             | 100%                                               | CI                        | QA    | Hold     |
| 5     | Parity rerun + budgets        | `flow-parity.spec`; `perf.yml`         | —                                                                                        | —               | 100% / §7.10 tolerances                            | CI                        | QA    | Hold     |
| 5     | RC re-canary                  | all Phase-3 rows on the attested RC    | as Phase 3                                                                               | as Phase 3      | as Phase 3                                         | **each row's own window** | RO    | Rollback |
| 5     | Cutover monitor               | `gate_slis.pipe`                       | probe freshness/loss; `render_terminal{failed}` rate vs pre-cutover canary               | —               | lag ≤ 5 min; loss < 0.1%; failed ≤ canary + 0.5 pt | 24 h                      | OPS   | Rollback |

### A.2 Expected stages per flow

| Flow      | Expected sequence                                                                                                           | Stage thresholds (named denominators)                                                                                                             |
| --------- | --------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------- |
| ssat      | `targeting_set → bridge_request → bridge_response_sent → renderer_document_loaded → render_terminal{accepted}`              | `targeting_set`/eligible wins ≥ 98%; each later stage ≥ 95% of prior; document/`bridge_response_sent` ≥ 99%; runner fail+timeout ≤ 1% of document |
| prebid    | same (keyed by Prebid `adId`)                                                                                               | same                                                                                                                                              |
| page_bids | same (post-SPA-navigation)                                                                                                  | same                                                                                                                                              |
| direct    | `attempt_started → renderer_document_loaded → render_terminal{accepted}`                                                    | document ≥ 99% of attempts; accepted ≥ 95%; runner fail+timeout ≤ 1% of document (hermetic + real-GAM)                                            |
| fallback  | parent `render_terminal{failed, gam_empty}` → child `fallback_start → renderer_document_loaded → render_terminal{accepted}` | `fallback_start`/eligible parent `gam_empty` ≥ 99%; accepted ≥ 95% of starts; runner fail+timeout ≤ 1% of document (hermetic + real-GAM)          |

### A.3 Real-GAM suite (attested)

| Field              | Value                                                                                                                                             |
| ------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------- |
| Workflow           | `.github/workflows/real-gam-release.yml` (manual dispatch, release-gating; created in Phase 0)                                                    |
| **Input**          | **the immutable release manifest `{release_id, bundle hashes, binary hash, config_hash, pool}`**                                                  |
| **Output**         | **attested report embedding the manifest** — a green run attests the exact build it exercised, parameterized by (release, pool, deployment epoch) |
| Topologies         | one per A.2 flow; publisher-overlap; disabled-initial-load formation; same-class supersession                                                     |
| Browsers           | Chromium, Firefox, WebKit (CSP/opaque rows); Chromium (funnel rows)                                                                               |
| Fixture            | dedicated GAM test network + line items targeting `hb_bidder=aps`; fixture doc in repo                                                            |
| Account/credential | owner recorded in the Phase-0 DR (operator-held; never in repo)                                                                                   |
| Command            | `npx playwright test --config real-gam.config.ts`                                                                                                 |
| Artifact           | Playwright HTML report + trace zips, retained 90 days                                                                                             |
| Retry policy       | one automatic retry per flaky-tagged spec; failures after retry are gate failures                                                                 |
| Approval evidence  | green attested run URL in the release checklist, signed off by RO                                                                                 |
