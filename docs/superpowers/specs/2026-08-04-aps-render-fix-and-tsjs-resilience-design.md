# APS Render Fix and TSJS Resilience Architecture — Design

- **Status:** revision 7 — reworked after the sixth review round.
- **Date:** 2026-08-04
- **Baseline:** `rc/july` @ `248fe9558` ("Fix APS PUC rendering and collapsed
  GAM shells"). All file:line citations refer to this commit.
- **Inputs:** three code audits; design reviews of revisions 1–6; open issues
  #926, #941, #944, #962, #964, #977, #983, #989, #993; open PR #997.
- **Normative gates:** the initial rollout-gates table ships as
  **Appendix A of this document** (one file, one review surface); changes to
  it require reviewed decision records so thresholds cannot be chosen after
  observing results.
- **Adoption stance for the baseline APS fixes (`248fe9558`):** this design
  adopts their **contracts** — the MessageChannel handshake semantics, the
  collapsed-shell remediation behavior, the consolidated bridge branch — and
  **re-implements them inside the target architecture** (the messaging
  module owns the channel protocol, the render engine owns the resize, the
  rebuilt `render_bridge` module owns the branch). The patch code itself is
  not carried forward through the refactor; the baseline's browser tests are
  retained as the conformance suite that pins the adopted behavior while the
  implementation is replaced.

## 0. Release policy: coordinated hard cutover

One coordinated release:

- Server, TSJS bundles, config, and HTML ship under one **`release_id`**.
  **No N/N−1**; in-flight clients may fail at cutover — accepted and stated.
- **Exact release matching**; config `format_version` exact-match; rollback =
  redeploy the previous release with its own config.
- **Config is a release-time input** under this policy: the config blob and
  binary publish together, so the enabled module vectors are known at
  release publication (this powers §G5 asset materialization).
- Assets: embedded only; hashed pathnames for cache identity; unknown hash
  → `410`, `no-store`.
- **Rollout state machine with release affinity.** A deployment manifest
  binds each pool to immutable `{release_id, config_store, config_key,
config_hash}` (rollback binding prevalidated). The new pool comes up fully
  enabled, reachable only by probes. Canarying uses a **sticky cohort
  token**: the router assigns `ts-rel=<release_id>` on the HTML response,
  routes every subsequent request (assets, APIs, beacons) by it, and cache
  keys include it — router weights alone apply per request and would mix
  pools, so affinity is what makes a canary request **coherent** end to end.
  Router weight over sticky cohorts is the sole activation primitive; flags
  are in-pool emergency kill switches only. Cutover = weight 100% + CDN
  purge; rollback = weight back + re-purge.

## 1. Problem statement

APS demand is fully integrated server-side, yet APS creatives do not appear
reliably. Four serial fixes (the `bid.meta` carrier, the decoupled shim, the
`hb_adid` fallback, the baseline PUC/collapsed-shell fix) each survived
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
adapter; (c) SPA `/_ts/page-bids`; (d) direct `/auction`. Only (d) renders
an APS descriptor without GAM.

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
| C6  | The renderer CSP can kill creatives after "ready".                                                                      | `aps.rs:49`                                                           |
| C7  | Renderer branches record nothing: no trace record, no notifications.                                                    | `gpt/index.ts:1628-1760`                                              |

### 2.4 Observability

Zero client→server reporting; a bid that never painted is byte-identical
server-side to one that painted.

### 2.5 Failure → signal mapping (normative)

Two distinct gates, precisely separated: the **tester cookie** (explicitly
non-security, `tester_cookie.rs:3`) gates **debug content** — the
`tsjs.boot.debug` envelope and the page-bids/`/auction`
`ext.trusted_server.debug` fields, the same sensitivity class as the
existing tester-gated `ts-debug` HTML comment. The **diagnostic credential**
(§5.3) gates **telemetry volume** (unsampled mode). The tester cookie never
affects sampling; the credential never gates mere content.

| Failure | Client event/reason (§5.1)                | Server row/counter (§5.6)             | One-page-load surface           |
| ------- | ----------------------------------------- | ------------------------------------- | ------------------------------- |
| A1      | —                                         | `selection_summary.winner_source`     | `boot.debug` selection summary  |
| A2      | —                                         | `bid_drop{script_rendering_disabled}` | `boot.debug` drop summary       |
| A3      | —                                         | `bid_drop{invalid_dimensions,w,h}`    | `boot.debug` drop summary       |
| A4      | — (fixed by §5.6)                         | `bid_drop` rows on all paths          | `boot.debug` / response `debug` |
| B1/B2   | `bridge_request{matched:false}`           | join via trace                        | console warn                    |
| C1      | `gam_empty` then no `bridge_request`      | join via trace                        | console warn                    |
| C2      | `render_fail{renderer_document_no_load}`  | route counters (`ts_ops_counters`)    | console warn                    |
| C3      | `render_fail{bridge_id_mismatch}`         | join via trace                        | console warn                    |
| C4      | `render_fail{descriptor_invalid}`         | schema corpus CI                      | console warn                    |
| C5      | — (fixed at baseline)                     | —                                     | —                               |
| C6      | `runner_failed` + CSP buckets             | `ts_csp_reports`                      | console warn                    |
| C7      | full §5.1 sequence from renderer branches | join via trace                        | debug/warn                      |

## 3. The GPT and baseline reality

1. Bootstrap-first hybrid; the bundle's handoff/initial-load code is dead in
   production.
2. #922 merge loss (`0dc9b19a9`); PR #997 is the apparent replacement.
3. TS refreshes never pass `changeCorrelator: false`.
4. `enableSingleRequest()` called blind after publisher `enableServices()`.
5. Responsive-resolution ambiguity silently skips slots.
6. Three independent `pubads().refresh` wrappers.
7. **GPT has no cancellation, no per-refresh identity, no completion-order
   guarantee**; `slotRenderEnded` = code injected, not resources loaded;
   `responseIdentifier` identifies responses only.
8. **`display()` under disabled initial load creates no request — for any
   caller.** GPT's behavior is caller-independent (`gpt/index.ts:1175`,
   `ad_init.test.ts:1201-1263`); the G4a rules therefore apply to publisher
   `display()` exactly as to TS `display()`.
9. `slotRenderEnded` registration gated behind `!ts.servicesEnabled`
   (`gpt/index.ts:1091`); G4a needs unconditional early subscription.
10. Baseline fix `248fe9558`: MessageChannel APS-PUC handshake
    (`aps.rs:65-125`, `aps/render.ts:415-437`; reply still terminates inside
    the PUC frame), collapsed-shell resize (`gpt/index.ts:217`), C5
    consolidated, real-PUC browser test added.
11. The bridge keeps consumed-id tombstones for security
    (`gpt/index.ts:1527`); G2 preserves that across navigations.
12. The tester cookie is not a security control (`tester_cookie.rs:3`).
13. **Fastly constructs application state per request** (`app.rs:146`) — no
    cross-request in-memory state may be assumed on that adapter.

## 4. Design gates

### G1 — Trace identity, sampling, and correlation

- Client-visible auction id (EC-derived, `publisher.rs:3237`) is never
  ingested. Initial-HTML telemetry precedes page JS, so correlation is
  minted by whoever acts first: server for `nav_gen 0` (trace + signed
  authorization in `tsjs.boot`); client afterwards, via `X-TSJS-Trace-Id`
  on page-bids (GET) and on the `/auction` POST, echoed back with the
  signed authorization (`ext.trusted_server.trace = {trace_id, auth,
auction_id}` on JSON/OpenRTB responses).
- **Sampling is a deterministic keyed function**, not a coin flip:
  `mode = sampled iff HMAC(sampling_key, trace_id) < sample_rate` — so
  concurrent requests presenting the same trace always derive the same
  mode, and first issuance needs no shared state (Fastly is per-request,
  §3.13).
- **Cross-tier join key (closing the row-multiplication gap):** the server
  echoes its telemetry `auction_id` to the client (boot / response
  extensions); the client stamps `auction_id` on every event of attempts
  born from that auction. Server rows additionally carry `release_id`.
  Canonical joins run on `(publisher_domain, trace_id, auction_id, slot,
refresh_gen)` at attempt grain; canary/control comparison uses the
  server-side `release_id`.
- Cache-privacy invariant: traces/authorizations only in per-request
  auction-bearing responses; such HTML is `private, no-store`.
- Envelope: per-trace groups `{trace_id, auth, events[]}`; events carry
  `{nav_gen, refresh_gen, seq, flow, auction_id?}`. **`flow` is a closed
  field** `ssat | prebid | page_bids | direct | fallback` set by the
  attempt owner — the per-flow funnels in the gates table depend on it.
- Traces are navigation-scoped; attempt counts key on
  `(trace_id, nav_gen, refresh_gen, slot)`.

### G2 — Render identity

- Cache-backed bids: `hb_adid` = PBS Cache UUID byte-for-byte. Markup bids:
  existing fallback chain. Renderer-only bids: server-minted token
  `^[a-z0-9]{12}$`, CSPRNG, in-auction collision retry, TTL 15 min,
  one-time consumption.
- **Registry + tombstones, one capacity, no unexpired eviction:** the
  bridge-reservation store holds live registrations and tombstones
  (consumed / stale / navigation-disposed ids) in one bounded structure —
  **capacity 320 for the union**; expired entries are pruned; **unexpired
  entries are never evicted**; when the union is at capacity, **new
  registration is refused** with `registry_full`. A late prior-navigation
  bridge request therefore always meets suppression until its id's original
  TTL passes (preserving `gpt/index.ts:1527`). Test: >320 registrations,
  then a late request for the oldest unexpired id.
- Client-Prebid keeps Prebid's `adId`; one store serves both paths.

### G3 — Runtime ABI (exact-release)

- Kernel only in `tsjs-core`; publishes
  `tsjs._internal = {release_id, registry}`; frozen after boot; core
  services constructed at boot; plugins register via the object-form API
  (§7.6) whose `release` is a build-generated constant; `registry.get`
  succeeds only on `release_id` equality; mismatch quarantines
  (`abi_mismatch`/`bundle_partial`) with a console error.
- Boundary enforcement: `import/no-restricted-paths` for layering plus
  **`no-restricted-properties`/`no-restricted-syntax`** rules covering
  `window`, `globalThis`, `self`, and local aliases for `googletag`/`pbjs`
  access outside `adapters/` (`no-restricted-globals` cannot catch member
  expressions). Adapters are the only access to **external ad-tech
  globals**; the kernel and messaging module necessarily touch
  `window.tsjs`, listeners, and `postMessage`.

### G4 — Render lifecycle

**G4a — Physical request cycles.**

- Intents recorded for both classes (`ts | publisher`) in one causal queue.
  **Any `display()` issued while initial load is disabled is retired at
  issuance regardless of caller** (GPT's no-request behavior is
  caller-independent, §3.8) — a stale publisher `display()` intent can no
  more poison a later TS `refresh()` match than the reverse. Hindsight
  zero-request intents (`refresh()` on a never-displayed slot) expire at
  2 s with `intent_no_request`; while one is pending, any opposite-class
  intent makes the next `slotRequested` ambiguous → quarantine. The
  ambiguity rule is symmetric.
- Cycles open only on `slotRequested`, matched to the causal queue head;
  SRA yields one per slot per batch; cycles close on `slotRenderEnded`;
  `responseIdentifier` dedups during drain.
- One outstanding TS cycle per slot; one queued replacement (coalescing).
- Attribution requires exactly one outstanding TS cycle and no overlap;
  otherwise quarantine (`cycle_unattributable`), fail closed.
- **No timeout re-arm.** Re-arm only on count-based drain, safe TS-owned
  destroy/redefine, or page end. Unissued intents are NavigationSession
  children; physical cycle/drain state is RuntimeSession.
- Deterministic-harness CI plus the release-gating real-GAM suite (scope
  enumerated in the gates table: per-flow topologies, expected sequences,
  browsers, fixtures, commands, artifacts, approvals — success criterion 1
  refers to that enumeration).

**G4b — Acknowledgement, specified per render path.** Four normative
sequences; each names its nonce/token producer, transport into the owned
frame, authenticated acceptance observation, cancellation, and separate
document/runner deadlines (document 3 s, runner 10 s, adm 5 s):

1. **APS-PUC** (baseline transport): bridge mints the per-attempt 128-bit
   nonce; MessageChannel into the renderer document (`ports.length`
   checks, exact-key replies, one-shot latch, port close); the document
   posts authenticated `renderer_document_loaded` then
   `render_accepted | render_failed{reason}` **to the top window**; the
   kernel validates source ownership, nonce, token, `nav_gen`,
   `refresh_gen` before transitions or notifications.
2. **Generic ADM/cache-PUC**: the bridge's display renderer creates the
   sandboxed adm frame with an injected reporter snippet; the reporter
   posts authenticated `adm_document_loaded{nonce}` to the top window on
   document load; acceptance = that message (baseline merely appends an
   iframe with no observation).
3. **Direct APS** (`renderApsCreative`): the kernel is the frame parent —
   the baseline parent-postMessage handshake (`ports.length === 0` branch)
   is already kernel-observed; same three messages, same validation.
4. **Direct ADM/cache**: as (2), with the kernel as parent.

Cancellation for all four: navigation/supersession invalidates the nonce;
late acks are discarded with `stale_navigation`.

**G4c — Honest observations; one terminal state.** Observations:
`gam_nonempty`, `gam_empty`, `gam_collapsed{action: resized | guarded,
reason}` (observation and remediation are separate — a guarded anchor/fixed
case is still observed), `renderer_document_loaded`, `runner_loaded`,
`runner_failed`, `adm_document_loaded`. **An attempt has exactly one
terminal state: `accepted | failed{reason} | no_bid | cancelled`.**
Post-acceptance runner failure is an observation plus the
`billing_outcome{billed_then_failed}` event — never a second terminal
transition. No observation claims paint. The baseline resize is a
sanctioned, guarded exception to the no-foreign-DOM-mutation rule.

**G4d — Notifications.** APS carries neither `nurl` nor `burl`
(`aps.rs:839`); excluded entirely. For carrying paths: bind per flow — PUC:
owned, slot-and-ad-id-matched bridge claim; direct: validated render start
(server must preserve + macro-expand `nurl`/`burl` in `/auction`
responses — `formats.rs:423` omits them — and the client must parse and
https-validate them — `core/auction.ts:43` drops them); fallback:
attributed `gam_empty` immediately before render. `nurl` at bind, `burl`
at `accepted`. **Economic identity is the normalized pair
`(id_kind, id_value)`** (direct attempts without `hb_adid` use
`bid_id`), idempotency key
`(trace_id, nav_gen, refresh_gen, slot, id_kind, id_value)`. **Every
dispatch emits `notification_sent{kind: nurl|burl, id_key_hash, result:
queued | failed}`** where `id_key_hash` is a 16-hex truncated HMAC of the
idempotency key — making the duplicate-`burl` invariant observable in
production (the gates table queries zero duplicates per key hash); external
billing reconciliation remains the authoritative backstop. No retries.

**G4e — Fallback.** Opt-in; renders only on a terminal `gam_empty`
unambiguously attributed to a TS cycle; ownership does not gate;
publisher-initiated or unattributable never triggers; timeouts never
render.

**G4f — Direct `/auction` lifecycle.** `RenderAttempt` keyed
`(trace_id, nav_gen, refresh_gen, slot)`; per-slot latest-wins with
cancellation; generation checks before every DOM/beacon effect
(`request.ts:31` races today); G4b sequence 3/4; G4d direct binds;
disposal on navigation; single terminal state;
`tsjs.requestAds(options): Promise<RequestAdsResult>` with
`RequestAdsResult = {traceId, slots: [{slot, outcome: "rendered" |
"no_bid" | "failed" | "cancelled", reason?}]}`.

**G4g — Mid-attempt configuration.** An attempt **snapshots its
configuration at creation**. The in-pool emergency kill switch cancels
attempts that have not yet passed their commit point — defined as
`bridge_response_sent` (PUC flows) or first DOM insertion (direct/fallback
flows); attempts past commit run to their terminal state; dispatched
notifications are never recalled.

### G5 — Deployment contracts

- Config `format_version`; release-time config (§0).
- **Assets pre-materialized at release publication:** because config ships
  with the release, the validated module vectors are known when the release
  is built — concatenated bytes + hashes are produced then and embedded;
  serving is lookup-only on every adapter (Fastly's per-request state,
  §3.13, makes construction-time caching meaningless there — the previous
  revision's claim is corrected). The §7.10 server benchmark measures the
  lookup path and guards against regression to per-request concatenation.
  Unknown vector = release-build error; unknown hash = `410`, `no-store`;
  exact match = `public, max-age=31536000, immutable`.
- **Internal route families — now four:** renderer, client-events,
  CSP-report, **and `/_ts/trace-auth`** — dispatch before auth/EC/
  publisher/integration filters; all methods reserved locally (405 +
  `Allow` + `no-store`; unknown versions 404 `no-store`; never publisher
  fall-through); no body/cookie/authorization forwarding; normalized
  scheme+host+port origin comparison; each family rate-limited (§5.4) and
  covered by four-adapter parity tests.
- §5.6 schemas deploy and validate before writers enable.

## 5. Observability

### 5.1 Wire payload and field matrix

```
{ v: 1, traces: [ { trace_id, auth, events: [
  { nav_gen, refresh_gen, seq, flow, auction_id?, t, ...fields } ] } ] }
```

| `t`                        | fields (absent = absent on wire, NULL in storage) |
| -------------------------- | ------------------------------------------------- |
| `bid_received`             | slot, id_kind, source                             |
| `targeting_set`            | slot, id_kind                                     |
| `bridge_request`           | slot, id_kind, matched                            |
| `bridge_response_sent`     | slot, source                                      |
| `render_attempt`           | slot, source                                      |
| `render_accepted`          | slot, source                                      |
| `render_fail`              | slot, reason, source?                             |
| `gam_nonempty`             | slot                                              |
| `gam_empty`                | slot                                              |
| `gam_collapsed`            | slot, action (`resized`\|`guarded`), reason?      |
| `renderer_document_loaded` | slot                                              |
| `runner_loaded`            | slot                                              |
| `runner_failed`            | slot, reason                                      |
| `adm_document_loaded`      | slot                                              |
| `fallback_start`           | slot                                              |
| `billing_outcome`          | slot, outcome (`billed_then_failed`)              |
| `notification_sent`        | slot, kind (`nurl`\|`burl`), id_key_hash, result  |
| `client_queue_overflow`    | dropped (count)                                   |
| `heartbeat`                | probe_id, expected_seq                            |

`source` is **nullable on `render_fail`**: absent for pre-source reasons
(`gpt_absent`, `pbjs_absent`, `slot_unresolved`, `intent_no_request`,
`abi_mismatch`, `registry_full`, `bundle_partial`); required for
source-specific reasons — the per-reason validity matrix is part of the
generated schema. Reason enum as revision 6 plus `currency_mismatch`.
**Heartbeats** are sent by identified probes under mode `probe` (§5.3):
excluded from every product metric by mode, never sampled out, and the
canonical freshness/loss query counts `expected_seq` gaps.

### 5.2 Transport and overflow

`fetch keepalive credentials:"omit"` primary; `sendBeacon(url,
new Blob([json], {type: "application/json"}))` on `pagehide`. Queue bound 256. **Overflow never enqueues into the full queue**: an out-of-band
saturating counter accumulates drops, and one coalesced
`client_queue_overflow{dropped}` is materialized into the **next flush**.

### 5.3 Signed trace authorization

`v1.<kid>.<exp>.<mode>.<sig>`; `auth` ingest bound 256 bytes; kid
`^[a-z0-9-]{1,16}$`; keys ≥ 256-bit CSPRNG in the secret store, previous
keys retained ≥ 24 h; canonical decimal `exp`, ±60 s skew, ≤ 15 min future;
`sig` = unpadded base64url HMAC-SHA-256 over the domain-separated
length-prefixed input (revision 6's exact encoding); constant-time compare;
per-group rejection.

- **Modes:** `sampled | unsampled | diagnostic | probe`. `unsampled`
  transmits nothing and is rejected at ingest if carried. `probe` is
  issued only to synthetic monitors (server-side issuance to the probe
  runner) and marks heartbeat traffic.
- **Renewal preserves mode by verification, not trust:** `GET
/_ts/trace-auth` presents the **current signed authorization** in
  `X-TSJS-Trace-Auth` (plus the trace header); the server verifies the
  still-valid token and re-signs the **same trace_id and mode** with fresh
  `exp`. The trace id itself carries no mode, and no adapter may rely on
  cross-request state (§3.13) — the presented token is the state. Renewal
  after expiry fails; the client stops transmitting and counts locally.
- **Diagnostic credential — complete protocol:** issuance `POST
/_ts/admin/diagnostic-credential` under the existing admin
  authentication (CSRF: same-origin + custom header required), response
  `{credential}` where credential = `d1.<kid>.<exp>.<origin-hash>.<sig>`,
  absolute expiry ≤ 60 min, HMAC over the publisher origin + expiry,
  **replayable short-lived bearer by design** (bounded by expiry and
  origin binding; not one-time — stated, not implied). Transport to the
  page: the operator opens the page with a `#tsdiag=<credential>` fragment
  (never sent to any server in a URL); the client stores it in
  `sessionStorage` and presents it in `X-TSJS-Diag` on trace-auth,
  page-bids, and `/auction` requests. The server, seeing a valid
  credential, issues/renews the trace authorization with
  `mode = diagnostic` and **`exp = min(now + 15 min,
credential expiry)`** — a trace authorization never outlives the
  credential. Initial HTML cannot see the fragment, so `nav_gen 0` starts
  `sampled|unsampled` and the client immediately upgrades via trace-auth.
  Validation is stateless HMAC — all four adapters support it. Forgery,
  replay-past-expiry, and wrong-origin tests required. The tester cookie
  remains content-only (§2.5).
- **Lazy cached initialization applies to every secret-backed component**
  (trace-auth keys, diagnostic keys, sampling key, sinks): first-use
  resolution with a cached result on request-bound platforms; resolution
  failure with the feature enabled → the feature's startup/first-use error
  path, never silent.

### 5.4 Ingest and rate limiting

As revision 6 (limits, same-origin, fail-closed drops), with the limiter
contract completed: key namespace per route family; portable maps hold
≤ 65,536 (Axum) / 4,096 (Cloudflare, Spin per-instance) entries with
10-minute entry TTL, cleanup on access plus periodic sweep — **capacity
pressure rejects unseen identities but expired entries are always
reclaimable, so saturation is bounded, not permanent**; missing client
address → a shared `unknown` bucket at 1 request/min; Fastly uses the
platform 60 s window counter at limit 20 with documented overshoot ≤ 2×
under concurrent bursts (`rate_limiter.rs:40` is read-then-increment);
Axum XFF selection = rightmost entry after skipping exactly
`trusted_proxy_hops`. `/_ts/trace-auth` and `/_ts/csp-reports/<policy-id>`
get their own buckets (10/min, burst 20 intent).

### 5.5 Sink, canonical views, monitoring

Stable event key `(publisher_domain, trace_id, seq)`; canonical views
`ts_client_events_v` (dedup) and `ts_render_attempts_v` (attempt grain,
joined on the G1 key including `auction_id`); dashboards/alerts query views
only. Datasource-side monitoring via `heartbeat` events from identified
probes (mode `probe`); freshness = heartbeat lag ≤ 5 min, loss =
`expected_seq` gaps < 0.1%; alert owner: release owner's on-call.

### 5.6 Physical schemas (deployed before writers)

- **`ts_client_events`**: revision 6 columns plus `flow Enum`,
  `auction_id Nullable(FixedString(36))`, `action Nullable(Enum)`,
  `kind Nullable(Enum)`, `id_key_hash Nullable(FixedString(16))`,
  `result Nullable(Enum)`, `probe_id Nullable(String)`,
  `expected_seq Nullable(UInt32)`; `mode Enum(sampled|diagnostic|probe)`.
- **Auction rows**: nullable `trace_id`, `mode`, plus **`release_id`**
  (the server-side cohort discriminator). Two added row types with full
  physical definitions:
  - `bid_drop {provider LowCardinality(String), slot Nullable(String),
reason Enum(AuctionDropReason), width Nullable(UInt16), height
Nullable(UInt16), count UInt32}` — cap 32 rows/auction **plus** one
    overflow row (`reason = overflow`, `count` = dropped-row count; the
    overflow row is not counted against the cap);
  - `selection_summary {slot String, winner_source
Enum(mediator|direct|none), winner_provider Nullable(String) — NULL
exactly when winner_source = none, candidates_direct UInt16,
candidates_mediator UInt16, dedup_hits UInt16, currency_rejected
UInt16, provenance_invalid UInt16, mediator_superseded UInt16}` — cap
    8 rows/auction plus one **auction-level totals row** (slot =
    `_totals`) that always survives truncation, so gate denominators never
    depend on per-slot rows. Counters are saturating UInt with
    `0xFFFF`/`0xFFFFFFFF` as the saturation sentinel.
  - **`AuctionDropReason` (closed, server-side):** `script_rendering_
disabled, invalid_dimensions, dimensions_out_of_range,
missing_render_source, invalid_creative_url, unsupported_tagtype,
render_payload_too_large, unexpected_response_shape,
currency_mismatch, floor_rejected, provenance_invalid,
duplicate_demand, overflow`.
- **`ts_csp_reports`** (aggregate only): `{received_at, release_id,
policy_id, cohort, directive_bucket Enum, source_bucket Enum, count}`;
  30-day TTL. **CSP ingest contract:** pre-buffer body ≤ 8 KiB, ≤ 10
  reports/request, strings ≤ 256, nesting ≤ 4, no `Content-Encoding`, own
  limiter bucket, fire-and-forget dispatch covered by probe heartbeats.
- **`ts_ops_counters`**: `{received_at, release_id, counter Enum, value
UInt64}` — the physical home for renderer-route counters (requests,
  unknown-version, auth-blocked) and limiter/abuse counters.
- **Settings, constructible:** `[telemetry.client_events]`
  `collection_enabled`, `sink_enabled` (collection without a sink =
  accept-count-drop by configuration, not accident), `sample_rate`,
  `api_host`, `dataset`, `token_secret`, `secret_store`,
  `max_body_bytes`; `[telemetry.trace_auth]` `secret_store`,
  `active_kid`, `previous_kids`, `sampling_key_secret`;
  `[telemetry.diagnostic]` `secret_store`, `active_kid`.
  `RuntimeServices` gains the client-events sink handle.
- APS parsing returns structured drop observations (slot + dimensions);
  > 8192 → `dimensions_out_of_range`, dimensions omitted.

### 5.7 Modes and SLIs

Production (sink-backed): deterministic keyed sampling (default 0.10).
SLIs: pipeline availability (heartbeat freshness/loss); failure detection
(≥ 1% of sampled render attempts visible within one hour at ≥ 10,000
sampled attempts/hour). Diagnostic: credential-gated, unsampled, full
stream + console mirroring + debug envelopes (§2.5).

### 5.8 Server-side drop surfacing

As revision 6, with `selection_summary`/`bid_drop` physicalized above.

## 6. APS delivery fixes

### 6.1 Mediation — total order, closed contradictions

1. **Currency.** Required `[auction].currency` (ISO 4217). Providers
   validate response currency at parse; providers with a contract-implied
   currency validate that implication — **APS enabled with a non-USD
   configured currency is a startup error** (`aps.rs:475` stamps USD), not
   a silent all-drop. Prebid's parse must validate rather than assume USD
   (`prebid.rs:2433`). Mismatch → `bid_drop{currency_mismatch}`.
2. **Candidate identity, total by construction.** `source_candidate_id` =
   `(provider_name, upstream_bid_id)` where the upstream id is **required
   bounded (≤ 64 chars) and unique per provider response at admission**;
   a bid missing an id, or duplicating one, receives a deterministic
   **intrinsic fingerprint**: `fp = hex(HMAC(auction_id, provider || slot
|| price_micros || render_source_digest))` — identical fingerprints are
   the same demand and dedup to one candidate. `candidate_id` (wire echo)
   is CSPRNG with in-auction collision retry and is **never** an ordering
   key.
3. **Mediator exchange.** Forwarded candidates carry
   `ext.trusted_server.candidate_id`; the mediator echoes it. Resolution
   rules, stated exhaustively: an echoed id that resolves → the forwarded
   candidate, provenance `mediator`; **price is authoritative from the
   mediator; every render-source and notification field comes from the
   stored candidate; deal fields are out of scope entirely** (no deal
   identity exists in the model — revision 6's "deal fields from mediator"
   is deleted). A mediator bid whose **any** render-source field differs
   from the stored candidate is reclassified mediator-native. An echoed id
   that does **not** resolve → that bid is discarded and counted
   (`provenance_invalid`); **mediator-native bids and direct candidates
   for the slot all remain eligible** — "fails closed" applies to the
   invalid claim, not the slot.
4. Floors filter both populations; echoed candidates remove their direct
   twins.
5. **Selection order (total):** decoded CPM desc → provenance rank
   (mediator first) → `source_candidate_id` asc (fingerprints compare as
   their hex strings). Arrival order can never matter.
6. **Strategy** required when mediator + direct providers coexist:
   `mediator_only` (timeout → no winners unless
   `mediator_timeout_fallback = "direct"`) or `merge_highest_cpm`
   (timeout → direct-only, reported).
7. Reporting: `selection_summary` rows + `_totals` (§5.6).

### 6.2–6.5

As revision 6: exact dimensions with structured drops; script creatives
default-off but loud — **DR-2's output is a deployment decision** (enable
with explicit sandbox/security approval, or accept a quantified maximum
excluded-demand share and gate Phase 3 on it), not a documentation
priority; G2 identity; G4e fallback.

### 6.6 Renderer endpoint

As revision 6 (unconditional route, versioned immutable document with a
checked-in per-version header manifest, three-message ack, aggregate route
counters now physically in `ts_ops_counters`, CSP rollout with
server-selected `policy_id` report paths and closed buckets — ingest
bounds per §5.6).

### 6.7 One descriptor schema

As revision 6 (generated JSON-Schema + TS parser + ES5 inline fragment +
fixtures; semantic validators handwritten; outer tolerance only; the
per-reason `source` validity matrix of §5.1 joins the generated artifacts).

### 6.8 Bridge hardening

As revision 6: parse → identify TS-reserved (live **or tombstoned**, G2) →
`stopImmediatePropagation` → validate source (bounded walk) →
nonce/token/`nav_gen`/`refresh_gen` → respond or refuse. Non-TS ids
untouched. Stolen-token test proves neither TS nor native Prebid responds.

## 7. TSJS target architecture

### 7.1 Layering

As revision 6, with G3's corrected lint mechanics and the adapter-scope
clarification (external ad-tech globals only).

### 7.2 Adapters / 7.3 Slot registry

As revision 6 (registry holds the G4a causal queue; cycle/drain state
RuntimeSession; unissued intents NavigationSession).

### 7.4 Final global surface

Revision 6's table **plus** the row the review found missing:

| Legacy surface | Final shape                                           |
| -------------- | ----------------------------------------------------- |
| (new, public)  | `tsjs.definePlugin({id, release, install, dispose?})` |

Bootstrap: field-wise idempotent init (`window.tsjs ||= {}; tsjs.que ||=
[]; tsjs.boot ||= {}`; the ad-slot script's `window.tsjs = {}` at
`publisher.rs:3665` is fixed); **transactional ownership**: states
`unclaimed → installing → kernel | fallback`. The kernel installs wrappers
**inert** and flips them live at a single commit point; on a throw before
commit it unwinds its registered disposers (the §7.6 machinery applied to
kernel boot itself) and marks `failed`; the 10 s watchdog treats a stuck
`installing` as failed; **fallback claims ownership only from
`unclaimed | failed`** — never beside a committed kernel. A bundle
arriving after fallback committed defers for the page (`bundle_partial`).
Tests: throw injected after each boot checkpoint.

### 7.5 Messaging / 7.6 Plugins and sessions / 7.7 Bootstrap / 7.8 GPT / 7.9 Decomposition

As revision 6 (plugin API object form with `release`; transactional
install; sessions; generated no-bundle fallback; unconditional GPT
subscriptions; decomposition table).

### 7.10 Performance (fully reproducible)

Revision 6's pinned workflow, plus the missing definitions: **module
vectors enumerated** — minimal = `[core]`; reference = `[core, creative,
gpt, prebid, datadome]`; maximal = all 13 discovered modules. **Browser
timing marks**: `performance.mark("tsjs:bids-script")` emitted by the
injected bids script and `performance.mark("tsjs:first-display")` emitted
by the adapter wrapper at the first `display()`/`refresh()` dispatch;
metric = duration between marks on the reference fixture page (the
integration-tests reference page), warm HTTP cache, all resources served
locally (no external network); p90 = nearest-rank over 50 samples;
inconclusive (3-run agreement worse than 5%) → one rerun, then fail.
**Maximal-vector peak JS heap ≤ baseline × 1.10.** Server benchmark: the
G5 lookup path (not concatenation), 100 warm-ups, 1,000 iterations, median
and p90, one-sided ≤ baseline × 1.10.

### 7.11 Toolchain

TypeScript floor to the resolved 5.9 line. **Release-gating flags,
enumerated:** `strict`, `noUncheckedIndexedAccess`,
`exactOptionalPropertyTypes`, `verbatimModuleSyntax`,
`noImplicitOverride`, `useUnknownInCatchVariables`; CI command:
`npx tsc -p crates/trusted-server-js/lib/tsconfig.json --noEmit`. Dev
toolchain bumps as individual CI-gated PRs; `prebid.js` excluded from
casual bumps; monthly review.

## 8. Rollout

Single-release state machine per §0 (sticky-cohort affinity). **The
normative gates table ships with this design as Appendix A**, including
initial thresholds, queries, commands, cohort keys, sample floors, owners,
hold/rollback actions, and the real-GAM suite's operational row. Threshold
changes require reviewed decision records.

Phases (build milestones inside the one release):

- **Phase 0 — Identity, schemas, toolchain, decisions.** Release-time
  asset materialization; `format_version`; §5.6 schemas writer-off;
  toolchain floors; dead expando deletion; drop surfacing; decision
  records DR-1..DR-5 (DR-2 now a deployment decision, §6.2–6.5; DR-4
  gates `merge_highest_cpm`; DR-5 splits Phase-2 gates).
- **Phase 1 — Kernel, sessions, minimal messaging, cycle registry,
  transactional bootstrap ownership.**
- **Phase 2 — Trace + beacon.** All three issuance paths + renewal +
  diagnostic credential + probe mode; four-adapter ingest incl.
  `/_ts/trace-auth` in the isolation family; heartbeats. Gates split:
  HTTP parity (all adapters) vs persistence (sink-backed).
- **Phase 3 — APS delivery.** As revision 6, plus `notification_sent`
  observability and per-flow funnel gating (the `flow` field): expected-
  stage tables per flow live in the gates file; denominator = server-
  observed eligible APS wins **within sampled/diagnostic traces**;
  `cycle_unattributable` gated; one-sided non-inferiority for fill/p95;
  duplicate-`burl` invariant via `notification_sent` key hashes.
- **Phase 4 — Structure.** Layering, plugins, adapters, registry,
  messaging, namespace; four-flow behavioral parity.
- **Phase 5 — Decomposition + cutover.** File splits; bootstrap stub +
  generated fallback (error/hang/arbitration tests); four-flow parity
  rerun; **then the full Phase-3 statistical canary/control gate and the
  real-GAM suite are repeated on the exact immutable release candidate**
  before router weight rises beyond the low-weight canary; then weight-up,
  purge, 24 h monitored window.

## 9. Test acceptance matrix

Revision 6's matrix stands, with these added/changed rows:

| Area             | Added coverage                                                                                                                                                                       |
| ---------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| Request cycles   | publisher `display()` under disabled initial load retired at issuance; publisher-display → TS-refresh attribution test; symmetric ambiguity                                          |
| Trace auth       | renewal presents current token and preserves mode; renewal-after-expiry fails closed; deterministic sampling (same trace → same mode across concurrent requests)                     |
| Diagnostic       | credential issuance under admin auth + CSRF; fragment→sessionStorage transport; upgrade of an initial sampled trace; auth `exp` capped at credential expiry; forgery/wrong-origin    |
| Trace-auth route | four-adapter parity; wrong-method 405; dispatch before filters; no forwarding; own limiter bucket                                                                                    |
| Affinity         | sticky-cohort coherence: new-pool HTML never fetches control-pool assets/APIs (cache-key + routing test); rollback binding prevalidation                                             |
| Join keys        | `auction_id` echo on all three paths; attempt-grain join uniqueness under repeated same-slot auctions; `release_id` cohort attribution                                               |
| Funnels          | `flow` field set per path; per-flow expected-stage conformance (SSAT, prebid, page-bids, direct, fallback)                                                                           |
| Tombstones       | union capacity 320; unexpired never evicted; >320 registrations then late oldest-id request suppressed; `registry_full` on refusal                                                   |
| Ack per path     | all four G4b sequences incl. adm reporter snippet; per-path document/runner deadlines; cancellation on navigation                                                                    |
| Terminal states  | exactly one terminal per attempt; post-accept runner failure emits observation + `billing_outcome`, no second terminal                                                               |
| Notifications    | `notification_sent` emitted per dispatch; duplicate-key-hash query returns zero in canary; direct-flow `(bid_id)` identity; server preserves/expands `nurl`/`burl`; client validates |
| Bootstrap        | inert-install + commit flip; throw after each checkpoint unwinds; watchdog claims only from failed/stuck; fallback-then-late-bundle deferral                                         |
| Overflow         | out-of-band counter; coalesced overflow event in next flush; no recursion at full queue                                                                                              |
| Heartbeats       | probe mode not sampled out; excluded from product denominators; freshness/loss queries from `expected_seq`                                                                           |
| CSP ingest       | pre-buffer caps (8 KiB / 10 reports / 256 chars / depth 4); both media types; opaque origin; policy-id path identity; aggregate schema rows                                          |
| Mediation        | required upstream id bounds; fingerprint fallback determinism under arrival shuffle; duplicate-id dedup; adm-swap reclassification; APS + non-USD startup error                      |
| Limiter          | TTL reclaim under saturation; unknown-address bucket; Fastly overshoot bound; XFF hop selection                                                                                      |
| Perf             | marks present; vector contents; heap budget; inconclusive-rerun policy                                                                                                               |
| Lint             | member-expression access to `googletag`/`pbjs` via `window`/`globalThis`/`self`/aliases caught outside adapters                                                                      |
| Kill switch      | pre-commit attempts cancelled; post-commit attempts run to terminal; snapshot semantics                                                                                              |

## 10. Alternatives / 11. Risks

As revision 6, plus: **rejected** — per-request Fastly concatenation
(replaced by release-time materialization); trusting client-asserted
sampling on renewal (replaced by token-presentation renewal); FIFO
tombstone eviction (replaced by union capacity with refusal). Risk added:
sticky-cohort routing is new infrastructure the cutover depends on — it is
Phase 0 work and its coherence test is release-gating.

## 12. Success criteria

Revision 6's criteria with these corrections: (2) diagnostic completeness
is achieved via the §2.5 gate split (content vs volume); (5) attempt
counts keyed `(trace_id, nav_gen, refresh_gen, slot)`; (10) the
duplicate-`burl` invariant is measured via `notification_sent` key hashes
in production and by hermetic tests, with external billing reconciliation
as backstop; (add 13) the Phase-3 statistical and real-GAM gates pass on
the exact immutable Phase-5 release candidate before weight-up; (add 14)
the Appendix A gates table shipped with this design and every later change
carries a reviewed decision record; (add 15) the baseline APS fix behaviors
are re-implemented in the target architecture with the baseline browser
tests passing unmodified as the conformance pin.

## 13. Open questions

Only one remains outside the decision records: does Amazon expose any
creative-completion acknowledgement that could add a
post-`render_accepted` state under a new name (future enhancement)?

---

## Appendix A — Normative rollout gates (initial values)

Owners are roles: **RO** = release owner, **QA** = QA owner, **OPS** =
release owner's on-call. Assignment key for canary/control =
sticky cohort (`ts-rel`), randomized at HTML request, per §0. All
production queries run against canonical views only (§5.5). "Hold" =
router weight frozen; "Rollback" = weight to previous release + re-purge.
Changing any row requires a reviewed decision record.

### A.1 Phase gates

| Phase | Gate                       | Query / test command                                                  | Denominator                                    | Floor         | Threshold                         | Window | Owner | Action   |
| ----- | -------------------------- | --------------------------------------------------------------------- | ---------------------------------------------- | ------------- | --------------------------------- | ------ | ----- | -------- |
| 0     | Dark-pool health           | probe suite vs dark pool (all four adapters)                          | probe requests                                 | 1,000         | 100% expected responses           | 24 h   | OPS   | Hold     |
| 0     | Schema validation          | synthetic writes to `ts_client_events` + auction rows                 | synthetic rows                                 | 10,000        | rejection < 0.1%                  | 24 h   | RO    | Hold     |
| 0     | Asset identity             | probe: every manifest hash 200-immutable; unknown hash 410 `no-store` | probed hashes                                  | all           | 0 misses / 0 wrong-status         | once   | QA    | Hold     |
| 1     | ABI cleanliness            | probe pages: `abi_mismatch` + `bundle_partial` counters               | probe page loads                               | 1,000         | 0                                 | 24 h   | QA    | Hold     |
| 1     | Bootstrap ownership        | hermetic: throw-after-each-checkpoint suite                           | checkpoints                                    | all           | 100% unwind-to-`failed`           | CI     | QA    | Hold     |
| 2     | Ingest HTTP parity         | parity suite vs all four adapters (routes, 405s, limits, 204s)        | parity cases                                   | all           | 100%                              | CI     | QA    | Hold     |
| 2     | Persistence (sink-backed)  | acceptance ≥ 99%; dedup exactly-once per `(trace, seq)`               | probe batches                                  | 10,000 evts   | as stated                         | 24 h   | OPS   | Hold     |
| 2     | Heartbeat pipeline         | freshness lag; `expected_seq` loss                                    | probe heartbeats                               | 1,000         | lag ≤ 5 min; loss < 0.1%          | 24 h   | OPS   | Hold     |
| 3     | APS funnel (per flow)      | per-`flow` stage rates from `ts_render_attempts_v` (table A.2)        | eligible APS wins in sampled+diagnostic traces | 10,000/cohort | per A.2                           | 24 h   | RO    | Rollback |
| 3     | Attribution soundness      | `cycle_unattributable` rate                                           | attributable-candidate cycles                  | 10,000        | < 0.5%                            | 24 h   | RO    | Rollback |
| 3     | GAM fill (non-inferiority) | canary fill vs control                                                | cohort ad requests                             | 10,000        | canary ≥ control − 2% (one-sided) | 24 h   | RO    | Rollback |
| 3     | Latency (non-inferiority)  | canary p95 bids-to-display vs control                                 | cohort attempts                                | 10,000        | canary ≤ control × 1.02           | 24 h   | RO    | Rollback |
| 3     | Billing                    | GAM/server-side revenue per 1,000 attempts, canary vs control         | cohort attempts                                | 10,000        | canary ≥ control − 2% (one-sided) | 24 h   | RO    | Rollback |
| 3     | Duplicate `burl`           | duplicate `notification_sent{burl}` per `id_key_hash`                 | burl dispatches                                | 1,000         | 0                                 | 24 h   | RO    | Rollback |
| 4     | Layering                   | both lint rules; disposal-inventory leak suite                        | —                                              | —             | 0 exceptions / 0 leaks            | CI     | QA    | Hold     |
| 4     | Four-flow parity           | hermetic parity: SSAT, prebid, page-bids, direct                      | parity cases                                   | all           | 100%                              | CI     | QA    | Hold     |
| 5     | Parity rerun + budgets     | four-flow parity; §7.10 budgets on pinned workflow                    | —                                              | —             | 100% / within tolerance           | CI     | QA    | Hold     |
| 5     | RC re-canary               | repeat all Phase-3 rows on the immutable RC                           | as Phase 3                                     | as Phase 3    | as Phase 3                        | 24 h   | RO    | Rollback |
| 5     | Cutover monitor            | §5.7 SLIs post-weight-up                                              | production traffic                             | —             | SLIs green                        | 24 h   | OPS   | Rollback |

Low-volume handling: a production gate that cannot reach its floor within
its window is **inconclusive** — extend the window once; a second
inconclusive result is a Hold, never a pass.

### A.2 Expected stages per flow (Phase 3 funnel)

| Flow      | Expected sequence                                                                                    | Stage thresholds                                                         |
| --------- | ---------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------ |
| ssat      | `targeting_set → bridge_request → bridge_response_sent → renderer_document_loaded → render_accepted` | each stage ≥ 95% of prior; document-load ≥ 99%; runner fail+timeout ≤ 1% |
| prebid    | same as ssat (keyed by Prebid `adId`)                                                                | same                                                                     |
| page_bids | same as ssat (after SPA navigation)                                                                  | same                                                                     |
| direct    | `render_attempt → renderer_document_loaded → render_accepted` (no bridge stages)                     | document-load ≥ 99%; accepted ≥ 95% of attempts                          |
| fallback  | `gam_empty → fallback_start → renderer_document_loaded → render_accepted`                            | accepted ≥ 95% of fallback starts                                        |

### A.3 Real-GAM suite (operational row)

| Field              | Value                                                                                  |
| ------------------ | -------------------------------------------------------------------------------------- |
| Workflow           | `real-gam-release.yml` (manual dispatch, release-gating; created in Phase 0)           |
| Topologies         | one per flow in A.2, plus publisher-overlap and disabled-initial-load formation (G4a)  |
| Browsers           | Chromium, Firefox, WebKit (CSP/opaque-origin rows); Chromium (funnel rows)             |
| Fixture            | dedicated GAM test network + line items targeting `hb_bidder=aps`; fixture doc in repo |
| Account/credential | owner recorded in the Phase-0 DR (operator-held; never in repo)                        |
| Command            | `npx playwright test --config real-gam.config.ts` from the browser test package        |
| Artifact           | Playwright HTML report + trace zips, uploaded as workflow artifacts, retained 90 days  |
| Retry policy       | one automatic retry per flaky-tagged spec; failures after retry are gate failures      |
| Approval evidence  | green workflow run URL linked in the release checklist, signed off by RO               |
