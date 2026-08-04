# APS Render Fix and TSJS Resilience Architecture — Design

- **Status:** revision 8 — reworked after the seventh review round and made
  fully self-contained: no contract in this document is defined by reference
  to an earlier revision.
- **Date:** 2026-08-04
- **Baseline:** `rc/july` @ `248fe9558` ("Fix APS PUC rendering and collapsed
  GAM shells"). All file:line citations refer to this commit.
- **Inputs:** three code audits; design reviews of revisions 1–7; open issues
  #926, #941, #944, #962, #964, #977, #983, #989, #993; open PR #997.
- **Normative gates:** the initial rollout-gates table is **Appendix A**;
  changes require reviewed decision records so thresholds cannot be chosen
  after observing results.
- **Adoption stance for the baseline APS fixes (`248fe9558`):** this design
  adopts their **contracts** — the MessageChannel handshake semantics, the
  collapsed-shell remediation behavior, the consolidated bridge branch — and
  **re-implements them inside the target architecture** (the messaging module
  owns the channel protocol, the render engine owns the resize, the rebuilt
  `render_bridge` module owns the branch). The patch code is not carried
  forward; the baseline's browser tests are retained unmodified as the
  conformance suite pinning the adopted behavior.

## 0. Release policy: coordinated hard cutover

One coordinated release:

- Server, TSJS bundles, config, and HTML ship under one **`release_id`**
  (git tag / build hash). **No N/N−1**; in-flight clients may fail at
  cutover — accepted and stated, not mitigated.
- **Exact release matching**: kernel, services, plugins, and the install
  manifest carry the same `release_id`; mismatch is a refusal.
- **Config is a release-time, content-verified input.** The config blob gains
  a top-level `format_version` (exact match required). Publish order: blob
  first, deployment manifest second. The manifest binds each pool to
  immutable `{release_id, config_store, config_key, config_hash}`, and the
  binary **verifies the loaded blob's hash against the manifest at
  startup** — a mismatch is a startup failure, so a config overwrite cannot
  mutate a supposedly immutable release or invalidate pre-materialized asset
  vectors. Rollback = redeploy the previous release with its own verified
  config; the rollback binding is prevalidated.
- **Assets:** binaries embed only their release's artifacts; hashed pathnames
  exist for cache identity; unknown hash → `410 Gone`, `no-store`.
- **Rollout state machine with authenticated release affinity.** The new
  pool comes up fully enabled, reachable only by probes. Canarying uses a
  **sticky, opaque, authenticated cohort token**: the router sets `ts-rel`
  on the HTML response — an HMAC-signed opaque value binding
  `{publisher_host, release_id, cohort, exp}` (attributes: `Secure;
HttpOnly; SameSite=Lax; Path=/`; TTL 24 h) — and routes every subsequent
  request by the **validated** token; invalid, expired, forged, or
  non-allowlisted tokens route to control and are reissued; tokens for
  retired releases are reassigned on next HTML response after rollback.
  Cache keys use the post-validation release label (bounded cardinality;
  raw cookie values never key caches). A plain readable release id would
  let any visitor opt into the dark pool and would hand cache-key
  cardinality to attackers — hence opaque and authenticated. Because
  affinity rides a cookie, **the beacon and CSP-report transports use
  `credentials: "same-origin"`** (not `omit`): the cookie exists for the
  routing layer only; application handlers still derive no identity from
  it. Router weight over sticky cohorts is the sole activation primitive;
  flags are in-pool emergency kill switches. Cutover = weight 100% + CDN
  purge; rollback = weight back + re-purge. The affinity acceptance test
  covers HTML, assets, APIs, **beacons, and CSP reports**.
- **Canary/control discrimination is infrastructure-attributed:** each pool
  writes telemetry with **pool-specific datasource tokens**, so cohort
  attribution comes from the write identity, not from in-row fields the
  control binary (the baseline) does not emit; in-row `release_id` from the
  new pool is secondary confirmation.

## 1. Problem statement

APS demand is fully integrated server-side — the edge runs the APS OpenRTB
auction, wins bids, and ships a typed renderer descriptor — yet APS
creatives do not appear reliably. Four serial fixes (the `bid.meta`
carrier, the decoupled shim, the `hb_adid` fallback, the baseline
PUC/collapsed-shell fix) each survived review; the pattern is the finding:
**multiple independent failure points, most failing silently**, with no
client→server signal about which fired. The TSJS library (56 files,
~11,900 lines, two ~1,800-line monoliths, duplicated ES5/TS logic,
inverted layering, ~100 error-swallowing catches) is the same problem
structurally.

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

Two gates, precisely separated: the **tester cookie** (explicitly
non-security, `tester_cookie.rs:3`) gates **debug content** — the
`tsjs.boot.debug` envelope and the page-bids/`/auction`
`ext.trusted_server.debug` fields, the same sensitivity class as the
existing tester-gated `ts-debug` comment. The **diagnostic credential**
(§5.3) gates **telemetry volume**. The cookie never affects sampling; the
credential never gates mere content.

| Failure | Client event/reason (§5.1)                | Server row/counter (§5.6)             | One-page-load surface           |
| ------- | ----------------------------------------- | ------------------------------------- | ------------------------------- |
| A1      | —                                         | `selection_summary.winner_source`     | `boot.debug` selection summary  |
| A2      | —                                         | `bid_drop{script_rendering_disabled}` | `boot.debug` drop summary       |
| A3      | —                                         | `bid_drop{invalid_dimensions,w,h}`    | `boot.debug` drop summary       |
| A4      | — (fixed by §5.6)                         | `bid_drop` rows on all paths          | `boot.debug` / response `debug` |
| B1/B2   | `bridge_request{matched:false}`           | join via trace (§G1)                  | console warn                    |
| C1      | `gam_empty` then no `bridge_request`      | join via trace                        | console warn                    |
| C2      | `render_fail{renderer_document_no_load}`  | `ts_ops_counters`                     | console warn                    |
| C3      | `render_fail{bridge_id_mismatch}`         | join via trace                        | console warn                    |
| C4      | `render_fail{descriptor_invalid}`         | schema corpus CI                      | console warn                    |
| C5      | — (fixed at baseline)                     | —                                     | —                               |
| C6      | `runner_failed` + CSP buckets             | `ts_csp_reports`                      | console warn                    |
| C7      | full §5.1 sequence from renderer branches | join via trace                        | debug/warn                      |

## 3. The GPT and baseline reality

1. Bootstrap-first hybrid; the bundle's handoff/initial-load code is dead
   in production.
2. #922 merge loss (`0dc9b19a9`); PR #997 is the apparent replacement.
3. TS refreshes never pass `changeCorrelator: false`.
4. `enableSingleRequest()` called blind after publisher `enableServices()`.
5. Responsive-resolution ambiguity silently skips slots.
6. Three independent `pubads().refresh` wrappers.
7. **GPT has no cancellation, no per-refresh identity, no completion-order
   guarantee**; `slotRenderEnded` = code injected, not resources loaded;
   `responseIdentifier` identifies responses only.
8. **`display()` under disabled initial load creates no request — for any
   caller** (`gpt/index.ts:1175`, `ad_init.test.ts:1201-1263`); GPT's
   behavior is caller-independent.
9. `slotRenderEnded` registration gated behind `!ts.servicesEnabled`
   (`gpt/index.ts:1091`); G4a needs unconditional early subscription.
10. Baseline fix `248fe9558`: MessageChannel APS-PUC handshake
    (`aps.rs:65-125`, `aps/render.ts:415-437`; the reply still terminates
    inside the PUC frame), collapsed-shell resize (`gpt/index.ts:217`), C5
    consolidated, real-PUC browser test added.
11. The bridge keeps consumed-id tombstones for security
    (`gpt/index.ts:1527`).
12. The tester cookie is not a security control (`tester_cookie.rs:3`).
13. **Fastly constructs application state per request** (`app.rs:146`);
    its platform rate counter is a 60 s fixed window with separate
    lookup/increment (`rate_limiter.rs:40`).
14. The baseline auction client collapses every failure into an empty
    array (`core/auction.ts:185-224`): absent fetch, timeout, network
    error, non-2xx, wrong content type, malformed body, and a genuine
    zero-bid auction are indistinguishable to callers.

## 4. Design gates

### G1 — Trace identity, sampling, correlation

- The client-visible auction id is EC-derived (`publisher.rs:3237`) and
  never ingested. Initial-HTML telemetry precedes page JS
  (`telemetry.rs:148`, `publisher.rs:2452`), so correlation is minted by
  whoever acts first: the **server** for `nav_gen 0` (trace + signed
  authorization in `tsjs.boot`); the **client** afterwards via
  `X-TSJS-Trace-Id` on page-bids (GET) and the `/auction` POST, echoed
  back with the authorization and the server's telemetry auction id:
  `ext.trusted_server.trace = {trace_id, auth, auction_id}`.
- **Deterministic keyed sampling, numerically exact:** take the first
  8 bytes of `HMAC-SHA-256(sampling_key, trace_id)` as a big-endian u64;
  `mode = sampled` iff `u64 < floor(sample_rate × 2⁶⁴)`; `sample_rate`
  must be finite and in `[0, 1]` (validated at load; 0 → nothing sampled,
  1 → everything). Concurrent requests for one trace always derive the
  same mode with no shared state (§3.13).
- **Cross-tier join:** the equality key is the globally unique
  **`auction_id`** (server-minted telemetry UUID), echoed to the client
  and stamped on every event of attempts born from that auction.
  Generations (`nav_gen`, `refresh_gen`) exist **client-side only**, for
  attempt aggregation — auction rows do not carry them. Canonical join:
  `(publisher_domain, trace_id, auction_id)`, attempt grain added from
  client events.
- **Cache-privacy invariant:** traces/authorizations only in per-request
  auction-bearing responses; such HTML is `private, no-store`, no
  validators; by construction and by test.
- Envelope: per-trace groups `{trace_id, auth, events[]}`; events carry
  `{nav_gen, refresh_gen, seq, flow, auction_id?}`. **`flow`** is closed:
  `ssat | prebid | page_bids | direct | fallback | system` — `system` for
  heartbeat and overflow events, which have no render flow; the generated
  per-event validity matrix (§5.1) says which events may carry which
  flows.
- Traces are navigation-scoped; attempt counts key on
  `(trace_id, nav_gen, refresh_gen, slot)`.

### G2 — Render identity

- Cache-backed bids: `hb_adid` = PBS Cache UUID byte-for-byte
  (`publisher.rs:3355`; the PUC fetches `?uuid=`, `gpt/index.ts:1772`).
  Markup bids: existing fallback chain. Renderer-only bids: server-minted
  token `^[a-z0-9]{12}$`, CSPRNG, in-auction collision retry,
  cross-auction uniqueness probabilistic (36¹² ≈ 4.7×10¹⁸) and harmless
  via scoping; TTL 15 min; one-time consumption.
- **Reservation store — one capacity, no unexpired eviction:** live
  registrations and tombstones (consumed / stale / navigation-disposed
  ids) share one bounded structure, **union capacity 320**; expired
  entries are pruned; **unexpired entries are never evicted**; at
  capacity, new registration is refused with `registry_full`. A late
  prior-navigation bridge request always meets suppression until its id's
  original TTL passes (preserving `gpt/index.ts:1527`). Test: >320
  registrations, then a late request for the oldest unexpired id.
- The client-Prebid path keeps Prebid's generated `adId`; one store
  serves both paths. Non-APS cache-path byte-identity regression tests.

### G3 — Runtime ABI under the IIFE build (exact-release)

Every entry point is a self-contained IIFE with inlined imports
(`build-all.mjs:46`, `bundle.rs:23`) — imports never share state across
bundles (live defect: `core/context.ts:11` vs `permutive/index.ts:102`).

- The kernel ships only in `tsjs-core`, publishes
  `tsjs._internal = {release_id, registry}` once (window sentinel),
  freezes `_internal` after boot, and constructs/registers core services
  (event bus, beacon queue, sessions, slot registry, render engine)
  during boot; integrations register integration-scoped services during
  `install()`.
- **Exact release matching:** every registration carries `release_id`
  (plugins via the §7.6 object API whose `release` is a build-generated
  constant); `registry.get(name)` succeeds only on equality; mismatch
  quarantines (`abi_mismatch` service / `bundle_partial` plugin) with a
  console error.
- Stateful access only via the registry at call time; stateless helpers
  may inline.
- **Boundary enforcement:** `import/no-restricted-paths` for layering
  **plus** `no-restricted-properties`/`no-restricted-syntax` rules
  catching member-expression access to `googletag`/`pbjs` through
  `window`, `globalThis`, `self`, and local aliases outside `adapters/`
  (`no-restricted-globals` cannot catch member expressions). Adapters are
  the only access to **external ad-tech globals**; kernel and messaging
  necessarily touch `window.tsjs`, listeners, and `postMessage`.

### G4 — Render lifecycle

**G4a — Physical request cycles.**

- **Intents, both classes, one causal queue.** Every observable
  initiation — TS and wrapped publisher `display()`/`refresh()` — records
  an intent in causal order, classified `ts | publisher`. Any
  `display()` issued while initial load is disabled is **retired at
  issuance regardless of caller** (GPT is caller-independent, §3.8) — it
  never enters the matcher. Hindsight zero-request intents (`refresh()`
  on a never-displayed slot) expire at 2 s with `intent_no_request`;
  **any later request-capable intent — same class or opposite —
  supersedes a pending uncertain intent immediately**, and if the
  uncertain intent's request could still legitimately be in flight
  (within its 2 s bound), the next `slotRequested` is ambiguous and the
  slot quarantines. A stale no-op `refresh()` can therefore never steal
  a later `display()`'s request in either direction, TS→TS,
  publisher→publisher, or across classes.
- **Cycles** open only on `slotRequested`, matched to the causal queue
  head; SRA batching yields one per slot per batch; cycles close on
  `slotRenderEnded`; `responseIdentifier` deduplicates responses during
  drain (it never attributes initiation).
- **Serialization:** at most one outstanding TS cycle per slot; one
  queued TS replacement (later intents coalesce).
- **Attribution:** a `slotRenderEnded` is attributable iff exactly one TS
  cycle is outstanding and no publisher/untracked request overlaps;
  otherwise quarantine (`cycle_unattributable`), fail closed.
- **No timeout re-arm.** Physical cycle/drain state lives in the
  RuntimeSession slot record; unissued intents are NavigationSession
  children (cancelled by navigation disposal). A quarantined or stale
  slot re-arms only on count-based drain, safe TS-owned
  destroy/redefine, or page end. Timeouts emit diagnostics and never
  restore attribution. Late stale events are matched and discarded
  (`stale_navigation`).
- Deterministic-harness CI plus the release-gating real-GAM suite
  (topologies enumerated in Appendix A.3).

**G4b — Acknowledgement, per render path.** Four normative sequences;
each names its nonce producer, transport, authenticated acceptance
observation, cancellation, and deadlines (document 3 s, runner 10 s,
adm 5 s). All nonces are per-attempt 128-bit CSPRNG values minted by the
attempt owner; the kernel validates, in order: source ownership (§6.8
walk), nonce, token, `nav_gen`, `refresh_gen` — before any transition or
notification. Navigation/supersession invalidates the nonce; late acks →
`stale_navigation`.

1. **APS-PUC** (baseline transport): bridge mints the nonce;
   MessageChannel into the renderer document (`ports.length` checks,
   exact-key replies, one-shot `accepted` latch, port close —
   `aps.rs:65-125`, `aps/render.ts:415-437`); the document posts
   authenticated `renderer_document_loaded` then
   `render_accepted | render_failed{reason}` to the top window.
2. **Generic ADM/cache-PUC:** the display renderer creates the sandboxed
   adm frame with an injected reporter snippet that posts authenticated
   `adm_document_loaded{nonce}` on document load; acceptance = that
   message (the baseline merely appends an iframe with no observation).
3. **Direct APS** (`renderApsCreative`): the kernel is the frame parent;
   the baseline parent-postMessage branch (`ports.length === 0`) is
   already kernel-observed; same three messages, same validation.
4. **Direct ADM/cache:** as (2) with the kernel as parent.

**G4c — Honest observations; one terminal state.** Inline-adm frames are
sandboxed `srcdoc` without `allow-same-origin` (`gpt/index.ts:510`) —
opaque; geometry proves nothing. Observations: `gam_nonempty`,
`gam_empty`, `gam_collapsed{action: resized | guarded, reason?}`
(observation and remediation separate), `renderer_document_loaded`,
`runner_loaded`, `runner_failed`, `adm_document_loaded`. **An attempt has
exactly one terminal state: `accepted | failed{reason} | no_bid |
cancelled`.** Post-acceptance runner failure is an observation only —
there is **no** `billing_outcome` event: no path has an honest producer
for a post-accept billing-failure claim (APS is excluded from
notifications and opaque frames offer no authenticated post-accept
signal), so the design does not pretend otherwise. No observation claims
paint; there is no `render_confirmed`. The baseline resize
(`gpt/index.ts:217`) is a sanctioned, guarded exception to the
no-foreign-DOM-mutation rule (authenticated source frame only; wrapper
only when both dimensions ≤ 1 px; anchor-ad and fixed/sticky guards).

**G4d — Notifications.** APS carries neither `nurl` nor `burl`
(`aps.rs:839`; the AAX envelope excludes them; the integration guide
documents no generic APS beacons) — excluded entirely; the Amazon runner
lifecycle is unchanged. For carrying paths (PBS and other OpenRTB
providers):

- Bind per flow, never selection or targeting (`ad_init.test.ts:1824`):
  PUC — an owned, slot-and-ad-id-matched bridge claim; direct —
  validated render start (the server must preserve and macro-expand
  `nurl`/`burl` in `/auction` responses, `formats.rs:423` omits them;
  the client must parse and https-validate them, `core/auction.ts:43`
  drops them); fallback — attributed `gam_empty` immediately before
  fallback render.
- `nurl` at bind; `burl` at `accepted`; **no retries**; idempotency key
  `(trace_id, nav_gen, refresh_gen, slot, id_kind, id_value)` with the
  normalized economic identity `(id_kind, id_value)` (direct attempts
  without `hb_adid` use `bid_id`).
- **Observability without client cryptography:** the server mints an
  opaque **`notif_id`** (12-char token, same generator as G2) per
  notification-carrying bid and delivers it with the bid; every dispatch
  emits `notification_sent{kind: nurl | burl, notif_id, result:
queued | failed}`. The browser computes no hashes (a client-held HMAC
  key would break the pseudonymization boundary; a rotating token would
  break stability across a gate window).
- **The duplicate-`burl` invariant is proven hermetically and
  reconciled externally, not "proven" by lossy telemetry:** hermetic
  tests pin exactly-once dispatch logic; production
  `notification_sent` duplicates are a **detection alarm** (any
  observed duplicate is a red gate); absence-of-duplicates is
  established by billing reconciliation (GAM/SSP reports vs server-side
  win counts) because sampled, best-effort telemetry cannot prove a
  zero.

**G4e — Fallback.** Opt-in
(`[auction].client_render_fallback = "renderer"`); renders only after a
terminal `gam_empty` unambiguously attributed to a TS cycle; ownership
does not gate it; publisher-initiated or unattributable cycles never
trigger it; timeouts never render.

**G4f — Direct `/auction` lifecycle.** `RenderAttempt` keyed
`(trace_id, nav_gen, refresh_gen, slot)`; per-slot **latest-wins with
cancellation** (concurrent calls cancel the older attempt;
`request.ts:31` races today); generation checks before every DOM/beacon
effect; G4b sequences 3/4; G4d direct binds; navigation disposal; one
terminal state. **The auction client returns a discriminated result** —
`{ok: bids[]} | {error: "auction_timeout" | "network_error" |
"http_error" | "invalid_response"}` — replacing the baseline's
everything-is-an-empty-array collapse (§3.14); only a successfully
parsed response with no winner maps to `no_bid`. Public API:
`tsjs.requestAds(options): Promise<RequestAdsResult>`,
`RequestAdsResult = {traceId, slots: [{slot, outcome: "rendered" |
"no_bid" | "failed" | "cancelled", reason?}]}`, settling when every slot
attempt is terminal. Reversed-response tests required.
**Fallback identity:** fallback is a **child attempt** — new
`RenderAttempt`, `flow = fallback`, carrying `parent_flow` (the
originating flow); the terminal `gam_empty` belongs to the parent
attempt under the parent's flow; the canonical view links parent and
child on `(trace_id, nav_gen, slot, refresh_gen)`.

**G4g — Mid-attempt configuration and the commit point.** An attempt
snapshots configuration at creation. **Commit = the earliest
irreversible action** — the first of: notification dispatch (`nurl` at
bind), `bridge_response_sent`, or first DOM insertion. The
generation/kill-switch check runs **immediately before each** of those;
an attempt past commit runs to its terminal state; dispatched
notifications are never recalled. (Revision 7 put commit after the
`nurl` side effect; that ordering error is corrected.)

### G5 — Deployment contracts

- Config `format_version` + manifest hash verification (§0).
- **Assets pre-materialized at release publication:** config is a
  release-time input, so validated module vectors are known when the
  release is built; concatenated bytes + hashes are produced then and
  embedded; serving is lookup-only on every adapter (Fastly is
  per-request, §3.13, so construction-time caching would be
  meaningless). Unknown vector = release-build error; unknown hash =
  `410 no-store`; exact match = `public, max-age=31536000, immutable`.
- **Internal route families — four:** renderer, client-events,
  CSP-report, `/_ts/trace-auth`. All dispatch before auth/EC/publisher/
  integration filters (Fastly today runs EC setup and pre-route filters
  first, `app.rs:709`); all methods and version prefixes reserved
  locally (405 + `Allow` + `no-store`; unknown version 404 `no-store`;
  never the publisher fall-through of `adapter-spin app.rs:804`); no
  body/cookie/authorization forwarding. **Origin policy is per family**,
  not universal: client-events and trace-auth require strict normalized
  same-origin (scheme+host+port); the CSP route admits opaque/`null`
  origins and authenticates by server-selected path identity plus abuse
  limits; the renderer document is a public GET validated by
  version/path only (it is loaded from sandboxed opaque contexts —
  browser-origin authentication is impossible there by design).
- Ingest routes exist in all four adapters; Fastly has real sinks;
  others accept-count-drop by contract (DR-5).
- §5.6 schemas deploy and validate before writers enable.

## 5. Observability

### 5.1 Wire payload and per-event field matrix

```
{ v: 1, traces: [ { trace_id, auth, events: [
  { nav_gen, refresh_gen, seq, flow, auction_id?, t, ...fields } ] } ] }
```

| `t`                        | fields                                        | allowed `flow`          |
| -------------------------- | --------------------------------------------- | ----------------------- |
| `bid_received`             | slot, id_kind, source                         | render flows            |
| `targeting_set`            | slot, id_kind                                 | render flows            |
| `bridge_request`           | slot, id_kind, matched                        | ssat, prebid, page_bids |
| `bridge_response_sent`     | slot, source                                  | ssat, prebid, page_bids |
| `render_attempt`           | slot, source                                  | render flows            |
| `render_accepted`          | slot, source                                  | render flows            |
| `render_fail`              | slot, reason, source?                         | render flows            |
| `gam_nonempty`             | slot                                          | ssat, prebid, page_bids |
| `gam_empty`                | slot                                          | ssat, prebid, page_bids |
| `gam_collapsed`            | slot, action (`resized`\|`guarded`), reason?  | ssat, prebid, page_bids |
| `renderer_document_loaded` | slot                                          | render flows            |
| `runner_loaded`            | slot                                          | render flows            |
| `runner_failed`            | slot, reason                                  | render flows            |
| `adm_document_loaded`      | slot                                          | render flows            |
| `fallback_start`           | slot, parent_flow                             | fallback                |
| `notification_sent`        | slot, kind (`nurl`\|`burl`), notif_id, result | render flows            |
| `client_queue_overflow`    | dropped (count)                               | system                  |
| `heartbeat`                | probe_id, expected_seq                        | system                  |

"Render flows" = `ssat | prebid | page_bids | direct | fallback`.
`source` on `render_fail` is **nullable**: absent for pre-source reasons
(`gpt_absent`, `pbjs_absent`, `slot_unresolved`, `intent_no_request`,
`abi_mismatch`, `registry_full`, `bundle_partial`); required otherwise.
The per-event/per-reason validity matrix is a generated artifact (§6.7).
Reason enum (closed): `renderer_document_no_load`, `runner_no_load`,
`runner_failed`, `descriptor_invalid`, `invalid_dimensions`,
`dimensions_out_of_range`, `bridge_id_mismatch`, `cycle_unattributable`,
`intent_no_request`, `stale_navigation`, `bridge_claim_timeout`,
`gam_empty`, `no_render_source`, `slot_unresolved`, `gpt_absent`,
`pbjs_absent`, `bundle_partial`, `fallback_cancelled`, `abi_mismatch`,
`registry_full`, `currency_mismatch`, `auction_timeout`,
`network_error`, `http_error`, `invalid_response`,
`adm_document_no_load`. No client timestamp; the server stamps
`received_at`; ordering within a trace is `seq`.

### 5.2 Transport and overflow

`fetch(..., {keepalive: true, credentials: "same-origin"})` primary (§0
affinity; the handler still derives no identity from cookies);
`pagehide` fallback `navigator.sendBeacon(url, new Blob([json], {type:
"application/json"}))`. Flush every 5 s and on
`visibilitychange`/`pagehide`. Queue bound 256 events. **Overflow never
enqueues into the full queue:** an out-of-band saturating counter
accumulates drops and one coalesced `client_queue_overflow{dropped}` is
materialized into the next flush.

### 5.3 Signed trace authorization

Format `v1.<kid>.<exp>.<mode>.<sig>`; the `auth` field has its own
ingest bound of **256 bytes** (every other string keeps the 64-char
cap — a 43-char unpadded-base64url signature cannot fit 64 with its
prefix fields).

- `kid`: `^[a-z0-9-]{1,16}$`; active + previous keys in the platform
  secret store; keys ≥ 256-bit CSPRNG; previous keys retained ≥ 24 h
  (≫ max token lifetime + skew). Missing key with the feature enabled →
  startup/first-use failure, never silent.
- `exp`: canonical decimal unix seconds (no sign, no leading zeros);
  ±60 s skew; ≤ 15 min future.
- `mode`: `sampled | unsampled | diagnostic | probe`. **`unsampled` is
  the signed discard decision:** the client neither enqueues nor
  transmits for it, and ingest rejects any group carrying it. `probe`
  marks synthetic monitors (server-issued to probe runners); probe
  traffic is never sampled out and is excluded from product metrics by
  mode.
- `sig`: unpadded base64url of HMAC-SHA-256 (43 chars) over the
  domain-separated, length-prefixed input `"ts-trace-auth-v1" ||
u32be(len(origin)) || origin || u32be(len(trace_id)) || trace_id ||
u32be(len(mode)) || mode || u64be(exp)`, strings UTF-8, `origin` =
  externally visible scheme+host+port. Constant-time comparison.
- **Renewal preserves mode by verification, not trust:**
  `GET /_ts/trace-auth` presents the current still-valid token in
  `X-TSJS-Trace-Auth` (plus the trace header); the server verifies and
  re-signs the same `trace_id` and `mode` with fresh `exp`. **The only
  mode transition that exists is the diagnostic upgrade, a distinct
  operation:** `POST /_ts/trace-auth/upgrade` presenting the current
  token **and** a valid diagnostic credential; it re-signs with
  `mode = diagnostic` and `exp = min(now + 15 min, credential expiry)`.
  Plain renewal never changes mode. Renewal after expiry fails; the
  client stops transmitting and counts locally.
- **Diagnostic credential:** issued `POST
/_ts/admin/diagnostic-credential` under the existing admin
  authentication (CSRF: same-origin + custom header), format
  `d1.<kid>.<exp>.<origin-hash>.<sig>`, absolute expiry ≤ 60 min,
  origin-bound, **replayable short-lived bearer by design** (bounded by
  expiry + origin binding; stated, not implied). **Exposure-minimized
  transport:** the operator opens the page with `#tsdiag=<credential>`;
  the synchronous bootstrap reads it, **immediately clears the fragment
  via `history.replaceState`**, holds the credential **in memory only**
  (RuntimeSession — never `sessionStorage`, which page scripts can
  read), and exchanges it via the upgrade operation as soon as the trace
  exists. Because initial HTML cannot see the fragment, `nav_gen 0`
  starts `sampled | unsampled`; **when a pending `#tsdiag` fragment is
  detected, the client buffers events locally without transmission
  (bounded 256) until the upgrade resolves**, then flushes under
  diagnostic mode — one-page-load diagnostic completeness holds without
  delaying rendering. Forgery, wrong-origin, and replay-past-expiry
  tests required. Validation is stateless HMAC — all four adapters.
- **Lazy cached initialization** applies to every secret-backed
  component (trace-auth keys, diagnostic keys, sampling key, sinks):
  first-use resolution with a cached result on request-bound platforms;
  failure with the feature enabled is that feature's loud error path.

### 5.4 Ingest and rate limiting

- `POST /_ts/client-events`: `application/json` only; no
  `Content-Encoding`; responds `204`, `no-store`; never echoes input.
  Pre-parse limits: body ≤ 16 KiB; ≤ 64 events; strings ≤ 64 chars
  (`auth` ≤ 256 bytes); `trace_id ^[0-9a-f]{32}$`; integers `[0, 2³¹)`;
  width/height `[0, 8192]`. Violations → drop-and-count with `204`.
- Same-origin (client-events, trace-auth): `Sec-Fetch-Site:
same-origin` when present, else normalized `Origin` equality; absent
  both → drop-and-count.
- **Rate limiting — adapter abstraction with declared semantics:**
  trait `ClientEventLimiter`, key namespace per route family; intent
  10 req/min, burst 20 per client address. Axum: real in-process token
  bucket, map ≤ 65,536 entries; Cloudflare/Spin: per-isolate/instance
  best-effort, ≤ 4,096 entries; entry TTL 10 min with cleanup on access
  plus periodic sweep — **capacity pressure rejects unseen identities,
  but expired entries are always reclaimable, so saturation is bounded,
  not permanent**; missing client address → shared `unknown` bucket at
  1 req/min. Fastly: the platform 60 s fixed-window counter at limit 20
  as a documented approximation; because its lookup and increment are
  separate operations (§3.13), **overshoot under a synchronized burst
  is bounded only by in-flight concurrency, and no numeric multiple is
  claimed** — the synchronized-burst test (> 40 concurrent) documents
  observed behavior, and a penalty-box follow-up is recorded if
  observed overshoot is operationally unacceptable. Limiter
  unavailable/errored → drop early with `204`. Trusted client address:
  Fastly platform client IP; Axum rightmost `X-Forwarded-For` entry
  after skipping exactly `trusted_proxy_hops` (absent config → socket
  peer only); Cloudflare `CF-Connecting-IP`; Spin platform address.
  `/_ts/trace-auth` and `/_ts/csp-reports/<policy-id>` carry their own
  buckets with the same intent.

### 5.5 Sinks, canonical views, per-sink monitoring

- Stable event key `(publisher_domain, trace_id, seq)`. Canonical views:
  `ts_client_events_v` (dedup: latest `received_at` per key) and
  `ts_render_attempts_v` (attempt grain per G1). Dashboards and alerts
  query canonical views only; raw-to-raw joins are forbidden (row
  multiplication).
- The Fastly sink is fire-and-forget after dispatch (`tinybird.rs:153`)
  and cannot see downstream rejection — **each datasource gets its own
  synthetic probe and freshness/loss query, per adapter write path**:
  client-events via `heartbeat` events (mode `probe`,
  `expected_seq` gaps = loss, lag = freshness); CSP via probe reports
  to a reserved `policy_id = probe`; ops via a probe counter. A green
  client-events heartbeat says nothing about the CSP or ops
  credentials — hence three probes. Alert owner: release owner's
  on-call.

### 5.6 Physical schemas (deployed before writers)

- **`ts_client_events`**: `received_at DateTime64, publisher_domain
LowCardinality(String), release_id String, trace_id FixedString(32),
mode Enum(sampled|diagnostic|probe), nav_gen UInt32, refresh_gen
UInt32, seq UInt32, flow Enum(ssat|prebid|page_bids|direct|fallback|
system), auction_id Nullable(FixedString(36)), event Enum(§5.1), slot
Nullable(String), id_kind Nullable(Enum), matched Nullable(UInt8),
source Nullable(Enum), reason Nullable(Enum), action Nullable(Enum),
parent_flow Nullable(Enum), kind Nullable(Enum), notif_id
Nullable(FixedString(12)), result Nullable(Enum), dropped
Nullable(UInt32), probe_id Nullable(String), expected_seq
Nullable(UInt32)`. Sorting key `(publisher_domain, received_at,
trace_id, seq)`; TTL 30 days; own ingest token; sink batch cap 512;
  startup validation of dataset + token when enabled;
  sink-unavailable → accept-count-drop.
- **Auction rows** (`telemetry.rs:262`, `auction_events_raw.datasource`):
  add nullable `trace_id`, `mode`, `release_id`. Two added row types
  with an explicit **`row_kind Enum(slot | totals | overflow)`** so
  totals/overflow rows are valid instances (no publisher-controlled
  sentinel strings; inapplicable fields nullable):
  - `bid_drop {row_kind, provider Nullable(LowCardinality(String)) —
NULL on overflow, slot Nullable(String), reason
Enum(AuctionDropReason), width Nullable(UInt16), height
Nullable(UInt16), count UInt32}` — cap 32 slot-rows/auction plus one
    overflow row whose `count` = **actual dropped bids**, not compacted
    rows;
  - `selection_summary {row_kind, slot Nullable(String) — NULL on
totals, winner_source Nullable(Enum(mediator|direct|none)) — NULL on
totals, winner_provider Nullable(String) — NULL when winner_source
≠ a winner, candidates_direct UInt16, candidates_mediator UInt16,
dedup_hits UInt16, currency_rejected UInt16, provenance_invalid
UInt16, mediator_superseded UInt16}` — cap 8 slot-rows plus one
    totals row that always survives truncation. Counters saturate at
    `0xFFFF`/`0xFFFFFFFF`.
  - **`AuctionDropReason` (closed, exhaustive over baseline
    producers):** `script_rendering_disabled, invalid_dimensions,
dimensions_out_of_range, missing_render_source,
invalid_creative_url, unsupported_tagtype,
render_payload_too_large, unexpected_response_shape,
currency_mismatch, floor_rejected, provenance_invalid,
duplicate_demand, missing_bid_id, duplicate_bid_id, unknown_impid,
invalid_price, unsupported_media_type, creative_id_too_large,
empty_seatbid, renderer_extension_serialization_failed,
no_render_source, lost_to_higher_bid, overflow` — covering the
    outcomes emitted at `aps.rs:740-929` and `formats.rs:408-419`; a
    **compile-time exhaustiveness test maps every producer to the
    enum**.
- **`ts_csp_reports`**: `received_at DateTime64, publisher_domain
LowCardinality(String), release_id String, policy_id
LowCardinality(String), cohort LowCardinality(String),
directive_bucket Enum(script|style|frame|img|connect|font|media|
worker|other), source_bucket Enum(https_host_allowlisted|data|blob|
inline|eval|other), count UInt32`; sorting key `(publisher_domain,
received_at, policy_id)`; TTL 30 days; settings
  `[telemetry.csp_reports] enabled, api_host, dataset, token_secret,
secret_store`. Ingest: pre-buffer body ≤ 8 KiB, ≤ 10 reports/request,
  strings ≤ 256, nesting ≤ 4, both media types
  (`application/csp-report`, `application/reports+json`) with separate
  validators, unused fields discarded before logging, own limiter
  bucket.
- **`ts_ops_counters`**: `received_at DateTime64, publisher_domain
LowCardinality(String), release_id String, counter
Enum(renderer_requests|renderer_unknown_version|renderer_auth_blocked|
ingest_accepted|ingest_dropped|ingest_rate_limited|abuse_flagged|
probe), value UInt64`; sorting key `(publisher_domain, received_at,
counter)`; TTL 90 days; settings `[telemetry.ops_counters]` (same
  shape).
- **Sink plumbing:** one generic multi-target Tinybird sink trait; the
  `RuntimeServices` (`platform/types.rs:158`) gains handles for
  client-events, CSP, and ops targets beside the auction sink; each
  target has its own dataset + token settings as above.
- **Settings:** `[telemetry.client_events] collection_enabled,
sink_enabled, sample_rate, api_host, dataset, token_secret,
secret_store, max_body_bytes`; `[telemetry.trace_auth] secret_store,
active_kid, previous_kids, sampling_key_secret`;
  `[telemetry.diagnostic] secret_store, active_kid`.
- APS parsing returns structured drop observations
  `{reason, slot, width?, height?}` (`aps.rs:722` loses slot/values
  today); >8192 → `dimensions_out_of_range` with dimensions omitted.

### 5.7 Modes and SLIs

Production (sink-backed): deterministic keyed sampling (default 0.10).
SLIs: **pipeline availability** — per-sink probe freshness ≤ 5 min and
probe loss < 0.1% (fails during sink outages, alarmed); **failure
detection** — a failure mode affecting ≥ 1% of sampled render attempts
visible within one hour, at ≥ 10,000 sampled attempts/hour. Diagnostic:
credential-gated, unsampled, full stream + console mirroring + debug
envelopes (§2.5).

### 5.8 Server-side drop surfacing

Bounded structured summary whenever any bid is dropped; `bid_drop` and
`selection_summary` rows (§5.6); the initial-HTML `ts-debug` comment
carries the drop summary; page-bids and `/auction` carry the
tester-gated structured `debug` field. Startup warnings: APS +
`allow_script_creatives = false`; mediator + direct providers without
an explicit `winner_selection` (§6.1 hard error).

## 6. APS delivery fixes

### 6.1 Mediation — total order, no fictional identities

Baseline defects: no forwarded candidate id; lossy last-write-wins
`(provider, slot, bidder)` field restoration (`adserver_mock.rs:95`);
arrival-order equal-price ties (`orchestrator.rs:827`); Prebid assumes
USD (`prebid.rs:2433`); APS stamps USD (`aps.rs:475`); no configured
currency. Replacement, identical in the synchronous and split
dispatch/collect paths via one shared helper:

1. **Currency.** Required `[auction].currency` (ISO 4217). Every
   provider parse validates its response currency; contract-implied
   currencies are validated as implied — **APS enabled with a non-USD
   configured currency is a startup error**, not a silent all-drop.
   Mismatch → `bid_drop{currency_mismatch}`.
2. **Candidate identity.** `source_candidate_id` =
   `(provider_name, upstream_bid_id)`; the upstream id is **required,
   ≤ 64 chars, and unique per provider response — bids missing an id or
   duplicating one are rejected** (`bid_drop{missing_bid_id |
duplicate_bid_id}`). No fingerprint fallback: a fingerprint over a
   partial field set can merge economically distinct demand, and
   OpenRTB requires bid ids — rejection is honest. `candidate_id` (wire
   echo) is CSPRNG with in-auction collision retry and is never an
   ordering key. Mediator-native bids get identities the same way from
   the mediator's response.
3. **Mediator exchange.** Forwarded candidates carry
   `ext.trusted_server.candidate_id`; the mediator echoes it (contract
   for every mediator, `adserver_mock` included). Echoed id resolves →
   the forwarded candidate, provenance `mediator`: **price is
   authoritative from the mediator; every render-source and
   notification field comes from the stored candidate; deal fields are
   out of scope entirely** (no deal identity exists in the model). A
   mediator bid whose any render-source field differs from the stored
   candidate is reclassified mediator-native. An unresolvable echoed id
   → that bid is discarded and counted (`provenance_invalid`);
   mediator-native bids **and direct candidates remain eligible** —
   fail-closed applies to the invalid claim, not the slot.
4. Floors filter both populations; echoed candidates remove their
   direct twins (`dedup_hits`).
5. **Selection order (total, intrinsic):** decoded CPM desc →
   provenance rank (mediator first) → `source_candidate_id` asc.
   Arrival order can never matter.
6. **Strategy** required when mediator + direct providers coexist
   (startup error if absent): `mediator_only` (timeout → no winners
   unless `mediator_timeout_fallback = "direct"`) or
   `merge_highest_cpm` (timeout → direct-only, reported).
7. Reporting: `selection_summary` slot rows + totals row (§5.6).

### 6.2 Dimensions

Exact size membership stays (`aps.rs:675`). The fix is visibility
(`bid_drop{invalid_dimensions, w, h}`) plus documentation ("request the
sizes you accept"); accepting unrequested sizes would conceal an
upstream protocol violation.

### 6.3 Script creatives

`allow_script_creatives` stays default-`false`; the consequence is loud
(§5.8). **DR-2's output is a deployment decision:** enable with explicit
sandbox/security approval, or accept a quantified maximum
excluded-demand share and gate Phase 3 on it.

### 6.4 Render identity

As specified in G2 (token format, registry-union capacity, tombstones,
cache-path byte identity).

### 6.5 Fallback

As specified in G4e/G4a/G4f (attribution-gated child attempt; awaitable
renderer conversion precedes it; timeouts never render).

### 6.6 Renderer endpoint

- The static renderer document route registers **unconditionally in
  every adapter** (the APS provider stays config-gated); startup
  validation fails if an auth handler pattern covers it; §G5 isolation
  rules apply.
- Path `/integrations/aps/renderer/v1`, embedded, served
  `Cache-Control: public, max-age=31536000, immutable`; canary versions
  `no-store` (or bounded below cohort lifetime); a **checked-in header
  manifest per renderer version** freezes headers (CSP included) with
  the bytes — a version's headers never change after publication.
  Unknown versions → 404 `no-store`.
- Three-message acknowledgement per G4b sequence 1.
- Server route counters are aggregate (`ts_ops_counters`) — the nonce
  rides the URL fragment and never reaches the server.
- **CSP rollout, three instruments:** discovery on the **currently
  enforced** policy with reporting attached (report-only alone cannot
  reveal what the enforced policy already blocks); **tightening** via
  report-only; **relaxation** via a small enforced cohort on a
  short-lived canary version, gated on runner acceptance rate, CSP
  violation rate, and render-failure rate, with a kill switch; once
  frozen, a new immutable `/v2` ships. **CSP reports are advisory** —
  for opaque-origin reports the body-supplied document URL and policy
  version are forgeable, so policy identity is encoded in the
  **server-selected report path** (`/_ts/csp-reports/<policy-id>`);
  bucketed aggregation only (§5.6 buckets, global and per-cohort caps);
  never a sole automatic rollback signal. Browser capture on Chromium,
  Firefox, and WebKit (CI is Chromium-only today,
  `playwright.config.ts:16`; the matrix extends for this suite).

### 6.7 One descriptor schema

Wire truth is the tagged `BidRenderer` envelope (discriminator on the
enum, `types.rs:188-211`). A wire-schema crate/xtask (separate from
`trusted-server-js` — core already depends on it, `Cargo.toml:45`, so
the reverse edge would cycle) generates: the JSON-Schema artifact, the
TS structural parser, the ES5 inline validator fragment, the §5.1
per-event/per-reason validity matrix, and shared fixtures — checked in,
staleness-gated. Semantic checks stay handwritten on both sides
(URL/origin policy, canonical base64, length bounds, the exact one-bid
AAX projection, cross-field equality). Unknown-field tolerance applies
only to the outer versioned descriptor; the decoded AAX envelope
remains an exact projection. A shared positive + adversarial corpus
(extra fields, wrong versions, oversized payloads, URL smuggling,
non-canonical base64) runs through the Rust validator, the generated TS
parser, and the generated inline fragment in CI.

### 6.8 Bridge hardening

Processing order (normative; preserves the baseline defense that stops
propagation before source validation, `gpt/index.ts:1584-1637`):

1. parse `e.data` (bare catch → return);
2. identify a TS-reserved ad id — **live registry or tombstone** (G2);
3. if TS-reserved: `stopImmediatePropagation()` before any validation —
   a rejected foreign frame must not be answerable by Prebid's native
   handler either;
4. validate source ownership via the bounded walk: known slot-root
   `WindowProxy` map, the sender's own parent chain
   (`event.source.parent`, …) to depth 5 — never scanning an
   attacker-controllable frame tree;
5. validate nonce, token, `nav_gen`, `refresh_gen` (G4b);
6. respond, or refuse with `bridge_id_mismatch`.

Non-TS ad ids are untouched. The stolen-token browser test asserts
**neither TS nor the native Prebid listener responds**; listener
registration order has a real-browser assertion. Renderer branches emit
the full §5.1 sequence with G4d notifications only on carrying paths.

## 7. TSJS target architecture

### 7.1 Layering

```
kernel/          boot, config, queue, event bus, log, beacon, sessions
adapters/        googletag.ts, pbjs.ts, messaging.ts   ← only access to external ad-tech globals
services/        slots (registry+handoff), auction client, render engine, consent
integrations/    gpt, prebid, aps, creative, datadome, …
```

Enforced by the two G3 lint rule families. Dissolves the audited
inversions (`core/auction.ts`/`core/request.ts` →
`integrations/aps/render`; `gpt`/`prebid` → `aps`; `prebid` owning the
GPT refresh wrapper). Kernel imports nothing above it; adapters import
kernel only; services import kernel + adapters; integrations import
kernel + services, never each other; stateful services via the G3
registry only.

### 7.2 Adapters

Per external global: `present | pending | timed_out`; `timed_out` is
non-terminal (late GPT/pbjs/CMP arrival transitions to `present` and
drains what is still valid); queued operations carry their own timeouts
and expire with disposition reasons.

### 7.3 Slot registry service

Kernel-owned; `WeakMap<googletag.Slot, SlotRecord>` + div-id index;
ownership (ts/publisher/adopted), adoption, handoff claims, responsive
resolution, the G4a causal intent queue (NavigationSession for unissued
intents) and cycle/drain state (RuntimeSession), targeting-key history.
No expandos on GPT objects (`__tsRenderGeneration`/`__tsRenderBid`
deleted).

### 7.4 Final global surface (hard cutover)

| Legacy surface (removed at cutover)     | Final shape                                                           |
| --------------------------------------- | --------------------------------------------------------------------- |
| `window.tsjs.que`                       | `window.tsjs.que` — unchanged, the one public queue                   |
| `globalThis.tscreative`                 | `tsjs.creative.*`                                                     |
| `globalThis.tsCreativeConfig`           | `tsjs.boot.creative`                                                  |
| `requestAds` (void)                     | one async `tsjs.requestAds(options): Promise<RequestAdsResult>` (G4f) |
| `window.__tsjs_*` flags, config globals | `tsjs.boot.*`                                                         |
| install manifest                        | `tsjs.boot.manifest` (`{release_id, plugins: [{id, order}]}`)         |
| expandos / function sentinels           | `SlotRecord` fields / kernel `WeakSet`                                |
| `tsjs._internal`                        | kernel registry (G3), frozen after boot                               |
| (new, public)                           | `tsjs.definePlugin({id, release, install})`                           |

**Bootstrap correctness and transactional ownership:** every
server-injected initializer creates the container **idempotently,
field-wise** — `window.tsjs ||= {}; tsjs.que ||= []; tsjs.boot ||= {}`
(the ad-slot script's `window.tsjs = {}` at `publisher.rs:3665` is
fixed). Ownership states: `unclaimed → installing → kernel | fallback`,
with an **owner generation** counter. The kernel installs wrappers
**inert** and flips them live at a single commit point; a throw before
commit runs the shared unwind inventory and marks `failed`. **The
watchdog path is race-free:** on a 10 s stuck `installing`, the
watchdog aborts the owner-generation-scoped `AbortController`, runs the
same shared unwind inventory to completion, and only then atomically
transitions `failed → fallback`; **every late kernel continuation and
disposer validates the owner generation** and self-discards on
mismatch — a resumed async installation can neither overwrite fallback
wrappers nor perform a stale commit. A bundle arriving after fallback
committed defers for the page (`bundle_partial`). Tests: throws
injected after each boot checkpoint **and** hung checkpoints that
resume after fallback claims ownership.

### 7.5 Messaging module

All `postMessage` through one module: versioned envelopes, name
constants (the `'Prebid Request'` literal appears at six sites today;
the APS handshake existed in three copies), G4b nonces, §6.8
validation. The minimal module (envelope + constants + validators used
by the bridge) lands in Phase 1; full call-site migration in Phase 4.

### 7.6 Plugin lifecycle — transactional — and sessions

`tsjs.definePlugin({id, release, install})` — object form; `release` is
the build-generated `release_id` constant; **there is no plugin-level
`dispose` hook** — disposal is exclusively `ctx.onDispose`
registrations, which have exactly-once reverse-order semantics
(revision 7's optional `dispose?` had no defined ordering and is
removed). `install(ctx): void | Promise<void>`:

- `ctx.signal` (aborted on quarantine/disposal); synchronous
  `ctx.onDispose(fn)`; effects registered as they are made;
  reverse-order unwind on throw/reject/abort; per-disposer exception
  isolation; a disposer registered after disposal is invoked
  immediately; pending late registrations capacity 16, bound 10 s →
  `bundle_partial`; release mismatch quarantines before `install`.
- Sessions: `RuntimeSession` (page lifetime: bridge listener +
  reservation store, history hook, pbjs subscriptions, adapters, beacon
  queue, physical slot cycle/drain state, in-memory diagnostic
  credential); `NavigationSession` (per navigation: trace +
  authorization + renewal timer, render attempts, slot aliases,
  unissued intents, targeting history); `RenderAttempt` (per G4a cycle
  / G4f attempt). Each owns an enumerable disposal inventory;
  navigation disposes NavigationSession children only.
- Error policy: no empty `catch` — handle, log with context, or emit a
  disposition. The auction fetch gains timeout + `AbortController` and
  the G4f discriminated result.
- **Console logging retained, not replaced:** every issue-surfacing
  condition keeps or gains a `log.warn` carrying the same reason code
  as its beacon event; `debug`-level delivery/security failures are
  promoted to `warn`.

### 7.7 Bootstrap

`gpt_bootstrap.js` (495 ES5 lines duplicating handoff/initial-load/
hydration logic, with the live `servicesEnabled` divergence) shrinks to
a queue-and-flags stub; the bundle replays recorded early calls on
install (browser specs cover replay timing); the no-bundle fallback
("ads render if the bundle fails", pinned by `gpt.rs:1174-1179`) is
**generated from the same TypeScript source** at build time, activated
per §7.4's transactional rules.

### 7.8 GPT correctness carried with the restructure

Unconditional early `slotRequested`/`slotRenderEnded` subscription
(replacing the `!servicesEnabled` gate, `gpt/index.ts:1091`; idempotent
recording); restore the #922 orphan-slot recovery and `updateRender`
enrichment (DR-3 decides #997 vs re-merge); `changeCorrelator: false`
on TS-initiated refreshes (configurable); `enableSingleRequest()` only
when GPT services are not already enabled; ambiguous responsive
resolution emits `render_fail{slot_unresolved}` alongside its console
warning.

### 7.9 Decomposition targets

| Today                                | Target                                                                                                             |
| ------------------------------------ | ------------------------------------------------------------------------------------------------------------------ |
| `gpt/index.ts` (~1850 LOC)           | `slot_resolution`, `handoff`, `initial_load`, `ad_init`, `render_bridge`, `spa_navigation`, `beacons` + thin index |
| `prebid/index.ts` (1671 LOC)         | adapter, shim, refresh handler (onto the slot registry), eids, diagnostics                                         |
| `gpt/script_guard.ts` (634 LOC)      | folded onto the 170-LOC `shared/script_guard.ts` factory                                                           |
| `core/trace.ts` (model + UI)         | `services/trace` (model) + `integrations/trace_overlay` (UI)                                                       |
| `core/global.d.ts` (`pbjs: TsjsApi`) | real Prebid types; `TsjsApi` split public vs internal                                                              |

### 7.10 Performance (reproducible)

- **Dedicated workflow** pinned `runs-on: ubuntu-24.04` (browser CI is
  `ubuntu-latest` today, `integration-tests.yml:155`) inside a pinned
  container image digest; browser = the lockfile-resolved
  `@playwright/test` build with its browser revision recorded in the
  baseline artifact (the manifest is a caret range,
  `browser/package.json:10` — lockfile + recorded revision are
  authoritative); compressors pinned by version in the container;
  deterministic flags (`gzip -9 -n`, `brotli -q 11`).
- **Module vectors enumerated:** minimal = `[core]`; reference =
  `[core, creative, gpt, prebid, datadome]`; maximal = all 13
  discovered modules. Budgets: raw/gzip/Brotli per bundle per vector vs
  checked-in baselines (`perf/baselines/*.json`; updates are reviewed
  diffs recording image/browser/tool versions; a baseline update is
  invalid if any pinned component differs); +5% bytes.
- **Browser timing:** marks `performance.mark("tsjs:bids-script")`
  (emitted by the injected bids script) to
  `performance.mark("tsjs:first-display")` (emitted by the adapter
  wrapper at the first `display()`/`refresh()` dispatch); reference
  fixture page; warm HTTP cache; all resources local; 5 warm-ups
  discarded, 50 samples; p90 = nearest-rank; gate p90 ≤ baseline ×
  1.10; inconclusive (3-run agreement worse than 5%) → one rerun, then
  fail. **Maximal-vector peak JS heap ≤ baseline × 1.10.**
- **Server benchmark:** the G5 lookup path; 100 warm-ups, 1,000
  iterations; median and p90; one-sided ≤ baseline × 1.10; 3
  consecutive runs within 5% or inconclusive (rerun, never pass).

### 7.11 Toolchain

TypeScript floor to the resolved 5.9 line (lockfile resolves 5.9.3
under the stale `^5.5.4` manifest). **Release-gating flags:** `strict`,
`noUncheckedIndexedAccess`, `exactOptionalPropertyTypes`,
`verbatimModuleSyntax`, `noImplicitOverride`,
`useUnknownInCatchVariables`. **Gate command (checked-in npm script
`typecheck`):**

```
cd crates/trusted-server-js/lib && npx --no-install tsc -p tsconfig.json --noEmit
```

(runs where the pinned compiler is installed; `--no-install` guarantees
the lockfile-resolved binary). Dev toolchain bumps as individual
CI-gated PRs with changelog review (this library monkeypatches
`fetch`/`sendBeacon`/DOM prototypes); `prebid.js` excluded from casual
bumps (runtime Prebid is the manifest-locked external bundle; npm pin
and deployed bundle version documented together); monthly review.

## 8. Rollout

Single-release state machine per §0 (authenticated sticky-cohort
affinity; infrastructure-attributed cohorts). The normative gates table
is Appendix A; every gate names a **checked-in artifact** (versioned
Tinybird pipe under `tinybird/pipes/`, script under `scripts/gates/`,
or workflow under `.github/workflows/`) — prose never substitutes for
an executable reference. Threshold changes require reviewed decision
records.

**Phase 0 decision records** (owner, evidence, deadline, explicit
go/no-go): DR-1 mediator presence (gates §6.1's Phase-3 scope); DR-2
script creatives — a **deployment decision** (§6.3); DR-3 #997 vs
re-merge (#922 restoration path); DR-4 mediator candidate-id echo owner
and timeline (`merge_highest_cpm` is config-blocked until delivered);
DR-5 non-Fastly sinks (splits Phase-2 gates).

- **Phase 0 — Identity, schemas, toolchain, decisions.** Release-time
  asset materialization; `format_version` + config-hash verification;
  §5.6 schemas deployed writer-off; toolchain floors; dead expando
  deletion; §5.8 drop surfacing; the five DRs; the gate artifacts
  themselves (pipes/scripts/workflows).
- **Phase 1 — Kernel, sessions, minimal messaging, cycle registry,
  transactional bootstrap ownership.**
- **Phase 2 — Trace + beacon.** Issuance on all three paths + renewal +
  diagnostic upgrade + probe mode; four-adapter ingest incl.
  `/_ts/trace-auth`; per-sink probes. Gates split per DR-5: HTTP parity
  (all adapters) vs persistence (sink-backed).
- **Phase 3 — APS delivery.** Schema crate + corpus; §6.1 with required
  `winner_selection` + `[auction].currency`; render token + reservation
  store; renderer route + three-message ack + CSP report route; §6.8;
  G4a–G4g; `notification_sent` with server-minted `notif_id`; fallback;
  DR-3 restoration; correlator + SRA fixes.
- **Phase 4 — Structure.** Full layering + both lint families; plugin
  lifecycle; adapters; full slot registry; full messaging migration;
  final namespace; four-flow behavioral parity.
- **Phase 5 — Decomposition + cutover.** File splits; script-guard
  consolidation; bootstrap stub + generated fallback (error/hang/
  arbitration tests); four-flow parity rerun; **the full Phase-3
  statistical canary/control gates and the real-GAM suite repeat on the
  exact immutable release candidate** before router weight rises beyond
  the low-weight canary; then weight-up, purge, 24 h monitored window.

**Statistical method (normative for A.1 production gates):**
populations are **sampled traces only** — diagnostic traffic is
operator-selected and failure-enriched, so it is reported separately
and never enters a statistical gate. Assignment unit = the sticky
cohort token (browser session); cohorts randomized at HTML request;
stratification by publisher and slot. Non-inferiority gates use
**one-sided 95% confidence bounds on the relative difference**
(canary/control − 1 ≥ −2% for fill and billing-per-1,000-attempts;
canary/control − 1 ≤ +2% for p95 latency); improvements always pass.
Floors are **per flow per arm** (Appendix A); flows that cannot reach
their floor in the window (direct, fallback at low adoption) are gated
hermetically and by the real-GAM suite instead of statistically — a
rare flow never permanently blocks rollout, and a statistical gate that
cannot reach its floor is **inconclusive** (extend once, then Hold).
`cycle_unattributable` is divided by **all TS request cycles that were
candidates for attribution** — the failures live in their own
denominator. Missing telemetry counts as failure. Billing reconciliation
(§G4d) runs alongside as the authoritative duplicate check.

## 9. Test acceptance matrix

Hermetic CI blocks PRs; the real-GAM suite is release-gating per
Appendix A.3.

| Area              | Must cover                                                                                                                                                                                                                                                                                                                                      |
| ----------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Request cycles    | intent-vs-request; disabled-initial-load `display()` retired for **any caller**; publisher-display → TS-refresh and TS-noop-refresh → TS-display supersession (same-class stealing); SRA; `intent_no_request`; overlap quarantine; no timeout re-arm; stale discard; real-GAM overlap                                                           |
| Ack protocol      | all four G4b sequences incl. the adm reporter; three-message APS sequence; five-field validation; SSAT/client-Prebid/SafeFrame; stale/replayed acks; acks after navigation disposal; per-path deadlines                                                                                                                                         |
| Bridge security   | propagation stopped before validation; neither TS nor native Prebid responds to stolen ids; prior-navigation ids suppressed via the reservation store after disposal; bounded parent-chain walk; listener order (real browser)                                                                                                                  |
| Render semantics  | binds per flow (never targeting); `burl` at accepted; attempt idempotency; `notification_sent{notif_id}` per dispatch; duplicate-detection alarm; accepted-but-blank honesty; `gam_collapsed{action}` emission + guarded resize                                                                                                                 |
| Direct `/auction` | trace header + `ext.trusted_server.trace` echo; discriminated auction-client errors (timeout/network/http/invalid vs `no_bid`); per-slot latest-wins with reversed responses; generation checks; `RequestAdsResult` settlement; server preserves + expands `nurl`/`burl`, client validates                                                      |
| Fallback          | child-attempt identity with `parent_flow`; renders only on attributed parent `gam_empty`; publisher-initiated never; timeout never renders; SPA cancellation; kill-switch pre-commit cancellation (commit = earliest irreversible action)                                                                                                       |
| Mediation         | required-unique upstream ids (`missing_bid_id`/`duplicate_bid_id` rejection — no fingerprint); `candidate_id` echo; arrival-order shuffle invariance; authoritative-field rules (repricing kept; any render-source difference → native); provenance fail-closed scope; strategy-specific timeouts; both lifecycles; APS + non-USD startup error |
| Render token      | format/CSPRNG/retry/TTL/one-time; `(trace, nav_gen, refresh_gen)` scope; union capacity 320 with `registry_full`; >320 then late oldest-id suppressed                                                                                                                                                                                           |
| Trace auth        | auth ≤ 256 B; encoding vectors (kid charset, canonical exp, u32/u64 BE prefixes, unpadded base64url); expiry/skew/max-future; renewal preserves mode via token presentation; renewal-after-expiry fails closed; previous-key retention; deterministic sampling (same trace → same mode concurrently; exact u64 threshold algorithm)             |
| Diagnostic        | credential issuance under admin auth + CSRF; fragment cleared via `replaceState`; in-memory-only storage; upgrade as the sole mode transition; auth `exp` capped at credential expiry; pre-upgrade local buffering then diagnostic flush; forgery/wrong-origin/replay-past-expiry                                                               |
| Trace-auth route  | four-adapter parity; wrong-method 405; dispatch before filters; no forwarding; own limiter bucket                                                                                                                                                                                                                                               |
| Affinity          | opaque token validation (forged/expired/retired → control + reissue); coherence for HTML, assets, APIs, **beacons, CSP reports**; cache-key normalization; rollback reassignment                                                                                                                                                                |
| Join keys         | `auction_id` echo on all three paths; attempt-grain join uniqueness under repeated same-slot auctions; infrastructure cohort attribution (per-pool tokens)                                                                                                                                                                                      |
| Funnels           | `flow` set per path incl. `system`; per-flow expected-stage conformance; heartbeat/overflow excluded from render denominators                                                                                                                                                                                                                   |
| Beacon            | joins on all three issuance paths; per-trace grouping; seq gaps; duplicate fetch/pagehide deduped in the canonical view; overflow coalescing without recursion; ingest abuse incl. absent Origin; sendBeacon Blob type; `credentials: same-origin` with identity-free handling                                                                  |
| Ingest/limits     | per-adapter limiter semantics as declared; TTL reclaim under saturation; unknown-address bucket; Fastly synchronized-burst behavior documented (> 40 concurrent); XFF hop selection; fail-closed 204                                                                                                                                            |
| Internal routes   | wrong-method 405 + `Allow` + `no-store` on every adapter; unknown version 404; no publisher fall-through; dispatch before auth/EC/filters; no forwarding; per-family origin policies                                                                                                                                                            |
| CSP               | both media types with separate validators; opaque/null-origin admission; policy-id path identity (forged body URL/version ignored); bucketed aggregation with caps; three-browser capture; per-version frozen header manifest                                                                                                                   |
| Schema            | staleness; adversarial corpus through Rust + generated TS + generated inline fragment; outer tolerance vs exact AAX projection; generated validity matrix; **compile-time exhaustiveness of `AuctionDropReason` over all producers**                                                                                                            |
| Runtime ABI       | one kernel under concatenation; exact-release verdicts; late registration; failure isolation; object-form `definePlugin` release check                                                                                                                                                                                                          |
| Plugins           | partial-install unwind; async rejection; abort while pending; disposer-after-disposal; per-disposer isolation                                                                                                                                                                                                                                   |
| Bootstrap         | field-wise idempotent init (ad-slot script no longer clobbers); inert-install + commit flip; throw after each checkpoint unwinds; **hung checkpoint resuming after fallback self-discards via owner generation**; fallback-then-late-bundle deferral                                                                                            |
| Lifecycle         | `timed_out → present`; session disposal inventories; unissued intents cancelled by navigation disposal; boot container consume/freeze/delete; final-namespace smoke (`tsjs.que`, `tsjs.creative`, async `requestAds`, `definePlugin`)                                                                                                           |
| Delivery          | unknown hash 410 `no-store`; exact-match immutable with full directive; release-time vector materialization (unlisted vector = build error); config-hash verification failure; cutover rehearsal (weight, purge, rollback)                                                                                                                      |
| Sinks/monitoring  | per-datasource probes (client-events heartbeat, CSP probe policy-id, ops probe counter) per adapter write path; canonical-views-only enforcement (raw-join multiplication test); `publisher_domain` naming                                                                                                                                      |
| Failure injection | Amazon runner redirect/hang/CSP block/script error → distinct §5.1 outcomes; EC/filter failure before renderer dispatch                                                                                                                                                                                                                         |
| Adapter parity    | ingest, CSP-report, trace-auth, renderer routes and drop surfacing equivalent across Fastly/Viceroy, Axum, Cloudflare, Spin                                                                                                                                                                                                                     |
| Policy            | script-creative warning; `invalid_dimensions` w/h; `dimensions_out_of_range` unclamped; `boot.debug` + response `debug` gating; diagnostic completeness per §2.5; kill-switch snapshot semantics                                                                                                                                                |
| Perf              | marks present; three vector contents; heap budget; inconclusive-rerun policy; pinned-environment baseline validity                                                                                                                                                                                                                              |
| Lint              | member-expression access to `googletag`/`pbjs` via `window`/`globalThis`/`self`/aliases caught outside adapters                                                                                                                                                                                                                                 |

## 10. Alternatives considered

1. Patching APS point-failures without telemetry — rejected: four
   correct fixes have not produced reliable ads.
2. Always direct-render APS (skip GAM/PUC) — rejected: changes GAM
   reporting/pacing unilaterally; kept as the attributed-`gam_empty`
   fallback.
3. Single module graph / shared chunks now — rejected for this release:
   changes the delivery pipeline while everything else changes;
   successor option behind the same registry surface.
4. Full rewrite in one branch without phases — rejected: the
   browser-spec safety net is thinnest exactly where behavior changes.
5. Dropping the ES5 bootstrap — rejected: loses the pinned no-bundle
   guarantee; the generated fallback keeps it.
6. Timeout-triggered fallback rendering — rejected: GPT requests cannot
   be cancelled; timeout racing a late fill can double-render and
   double-bill.
7. Timeout-based quarantine re-arm — rejected (recreates the stale-event
   bug).
8. N/N−1 compatibility machinery — removed by the §0 policy decision.
9. Client-computed notification hashes — rejected (no key without
   breaking the pseudonymization boundary); server-minted `notif_id`.
10. Fingerprint identities for id-less bids — rejected (can merge
    distinct demand); rejection with closed reasons instead.
11. Plain readable cohort cookie — rejected (dark-pool opt-in +
    cache-cardinality abuse); opaque authenticated token.
12. `billing_outcome` event — removed (no honest producer exists).

## 11. Risks

- Hard-cutover blast radius — accepted by policy; bounded by the §0
  runbook (probes, low-weight canary, 24 h window, weight-back
  rollback).
- Sticky-cohort routing is new infrastructure the cutover depends on —
  Phase 0 work; its coherence test is release-gating.
- Mediator wire-contract change (`candidate_id` echo) — DR-4 gates
  `merge_highest_cpm`; config validation enforces the block.
- Notification triggers become a published contract for PBS-path
  demand — changing them later is a breaking change for SSP reporting.
- Required `[auction].currency` and `winner_selection` (mediated
  deployments) are a deliberate startup-error class under §0.
- Beacon abuse — pre-parse caps, per-family origin policies, fail-closed
  numeric limits, signed modes, credentialed diagnostics.
- Registry/limiter memory — explicit capacities, TTL reclamation,
  reject-at-capacity; Fastly overshoot documented, not claimed.
- CSP data is advisory — never a sole rollback signal.
- Sink blindness — per-datasource probes with datasource-side queries.
- ABI freeze — `tsjs._internal.registry` is load-bearing; exact-release
  verdicts are the contract.
- Schema generation — checked-in artifacts + staleness CI.

## 12. Success criteria

1. APS creatives render in each configured flow (SSAT, client-Prebid,
   page-bids, direct), hermetically and in the release-gating real-GAM
   suite per Appendix A.3's enumerated topologies.
2. Every §2 failure maps to its §2.5 signal; diagnostic mode names the
   failing class from one page load (including A1–A4 via the debug
   envelopes, and including an initially-unsampled page via pre-upgrade
   buffering); §5.7 SLIs hold on sink-backed deployments.
3. Both lint families pass with zero exceptions; stateful sharing only
   via the registry; exact-release mismatches quarantine loudly.
4. No `src/` file exceeds ~500 lines; `gpt_bootstrap.js` is a stub or
   generated.
5. Attempt counts key on `(trace_id, nav_gen, refresh_gen, slot)`;
   traces stay navigation-scoped; no double counting; orphan recovery
   has a non-vacuous test; G4a holds including caller-independent
   retirement and same-class supersession.
6. The only TSJS-owned global is `window.tsjs` with the §7.4 final
   shape; no expandos; legacy names gone at cutover.
7. §7.10 budgets hold on the dedicated pinned workflow.
8. No existing warning lost; issue-surfacing conditions log `warn`+
   with the beacon's reason code.
9. TypeScript floor matches resolved 5.9 with the §7.11 flags via the
   checked-in `typecheck` script; `prebid.js` pin documented with the
   deployed bundle.
10. `nurl`/`burl` fire only on carrying paths at their G4d binds,
    attempt-scoped and idempotent; APS fires neither; hermetic
    exactly-once tests pass; production duplicates alarm via
    `notification_sent` and reconcile to zero via billing reports.
11. Trace-bearing responses are `private, no-store`; authorizations
    are per-trace, signed, mode-carrying, renewal-preserving, with
    diagnostic upgrade as the sole authenticated mode transition;
    unsampled traces transmit nothing.
12. The cutover runbook rehearsed (weight switch, purge, rollback);
    config-hash verification enforced.
13. The Phase-3 statistical and real-GAM gates pass on the exact
    immutable Phase-5 release candidate before weight-up.
14. The Appendix A gates table shipped with this design; every change
    carries a reviewed decision record.
15. The baseline APS fix behaviors are re-implemented in the target
    architecture with the baseline browser tests passing unmodified.

## 13. Open questions

Only one remains outside the decision records: does Amazon expose any
creative-completion acknowledgement that could add a
post-`render_accepted` state under a new name (future enhancement)?

---

## Appendix A — Normative rollout gates (initial values)

Owners are roles: **RO** = release owner, **QA** = QA owner, **OPS** =
release owner's on-call. Assignment unit = the authenticated sticky
cohort token (§0), randomized at HTML request, stratified by publisher
and slot. Statistical gates use **sampled traces only** (§8 method);
diagnostic traffic is reported separately. All production queries run
against canonical views via **checked-in versioned pipes**
(`tinybird/pipes/gate_<name>.pipe`); probe/parity suites are checked-in
scripts (`scripts/gates/<name>.sh`) or workflows. "Hold" = router
weight frozen; "Rollback" = weight to previous release + re-purge.
Changing any row requires a reviewed decision record.

### A.1 Phase gates

| Phase | Gate                          | Artifact (checked in)                                      | Denominator                                         | Floor          | Threshold                                                                              | Window | Owner | Action   |
| ----- | ----------------------------- | ---------------------------------------------------------- | --------------------------------------------------- | -------------- | -------------------------------------------------------------------------------------- | ------ | ----- | -------- |
| 0     | Dark-pool health              | `scripts/gates/probe-pool.sh`                              | probe requests                                      | 1,000          | 100% expected responses                                                                | 24 h   | OPS   | Hold     |
| 0     | Schema validation             | `scripts/gates/schema-writes.sh` (deterministic writes)    | synthetic rows                                      | 10,000         | **0 rejections** (writes are deterministic)                                            | 24 h   | RO    | Hold     |
| 0     | Asset identity                | `scripts/gates/asset-probe.sh`                             | probed hashes                                       | all            | 0 misses / 0 wrong-status                                                              | once   | QA    | Hold     |
| 0     | Config binding                | `scripts/gates/config-hash.sh`                             | pools                                               | all            | manifest hash verified on every pool                                                   | once   | OPS   | Hold     |
| 1     | ABI cleanliness               | `gate_abi.pipe` (probe pages)                              | probe page loads                                    | 1,000          | 0 `abi_mismatch`/`bundle_partial`                                                      | 24 h   | QA    | Hold     |
| 1     | Bootstrap ownership           | hermetic suite `bootstrap-ownership.spec`                  | checkpoints incl. hung-resume                       | all            | 100% unwind/self-discard                                                               | CI     | QA    | Hold     |
| 2     | Ingest HTTP parity            | `scripts/gates/ingest-parity.sh` (4 adapters, 4 families)  | parity cases                                        | all            | 100%                                                                                   | CI     | QA    | Hold     |
| 2     | Persistence (sink-backed)     | `gate_ingest.pipe`                                         | probe batches                                       | 10,000 events  | acceptance ≥ 99%; dedup exactly-once                                                   | 24 h   | OPS   | Hold     |
| 2     | Per-sink probes               | `gate_probes.pipe` (3 datasources × adapters)              | probe writes per sink                               | 1,000 each     | lag ≤ 5 min; loss < 0.1%                                                               | 24 h   | OPS   | Hold     |
| 3     | Funnel: ssat/prebid/page_bids | `gate_funnel.pipe` per flow                                | eligible APS wins (sampled traces), per flow-arm    | 10,000 each    | per A.2                                                                                | 24 h   | RO    | Rollback |
| 3     | Funnel: direct/fallback       | hermetic + real-GAM rows (A.3) — not statistical           | suite cases                                         | all            | 100%                                                                                   | CI+RG  | QA    | Hold     |
| 3     | Attribution soundness         | `gate_cycles.pipe`                                         | **all TS request cycles candidate for attribution** | 10,000         | `cycle_unattributable` < 0.5%                                                          | 24 h   | RO    | Rollback |
| 3     | GAM fill                      | `gate_fill.pipe`                                           | cohort ad requests per arm                          | 10,000         | one-sided 95% CB: rel. diff ≥ −2%                                                      | 24 h   | RO    | Rollback |
| 3     | Latency                       | `gate_latency.pipe`                                        | cohort attempts per arm                             | 10,000         | one-sided 95% CB: p95 rel. diff ≤ +2%                                                  | 24 h   | RO    | Rollback |
| 3     | Billing                       | `gate_billing.pipe` + GAM report reconciliation            | attempts per arm (per-1,000 normalization)          | 100,000 or 7 d | one-sided 95% CB: rel. diff ≥ −2%                                                      | window | RO    | Rollback |
| 3     | Duplicate `burl` alarm        | `gate_dup_notif.pipe` (detection) + billing reconciliation | burl dispatches                                     | 1,000          | 0 observed duplicates; reconciliation clean                                            | 24 h   | RO    | Rollback |
| 4     | Layering + leaks              | lint CI + `disposal-inventory.spec`                        | —                                                   | —              | 0 exceptions / 0 leaks                                                                 | CI     | QA    | Hold     |
| 4     | Four-flow parity              | `flow-parity.spec` (hermetic)                              | parity cases                                        | all            | 100%                                                                                   | CI     | QA    | Hold     |
| 5     | Parity rerun + budgets        | `flow-parity.spec`; `perf.yml`                             | —                                                   | —              | 100% / within §7.10 tolerances                                                         | CI     | QA    | Hold     |
| 5     | RC re-canary                  | repeat all Phase-3 rows on the immutable RC                | as Phase 3                                          | as Phase 3     | as Phase 3                                                                             | 24 h   | RO    | Rollback |
| 5     | Cutover monitor               | `gate_slis.pipe`                                           | production traffic                                  | —              | probe lag ≤ 5 min; probe loss < 0.1%; `render_fail` rate ≤ pre-cutover canary + 0.5 pt | 24 h   | OPS   | Rollback |

Low-volume handling: a statistical gate that cannot reach its floor in
its window is inconclusive — extend once; a second inconclusive is a
Hold, never a pass.

### A.2 Expected stages per flow (Phase 3 funnel)

Denominators are named per stage; document/runner rates apply wherever
the renderer document participates.

| Flow      | Expected sequence                                                                                    | Stage thresholds (each vs its named denominator)                                                                                          |
| --------- | ---------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------- |
| ssat      | `targeting_set → bridge_request → bridge_response_sent → renderer_document_loaded → render_accepted` | each ≥ 95% of prior; `renderer_document_loaded`/`bridge_response_sent` ≥ 99%; `runner_failed`+timeouts ≤ 1% of `renderer_document_loaded` |
| prebid    | same as ssat (keyed by Prebid `adId`)                                                                | same                                                                                                                                      |
| page_bids | same as ssat (after SPA navigation)                                                                  | same                                                                                                                                      |
| direct    | `render_attempt → renderer_document_loaded → render_accepted`                                        | document ≥ 99% of attempts; accepted ≥ 95% of attempts (hermetic/real-GAM gate, not statistical)                                          |
| fallback  | parent `gam_empty` → child `fallback_start → renderer_document_loaded → render_accepted`             | accepted ≥ 95% of `fallback_start` (hermetic/real-GAM gate, not statistical)                                                              |

### A.3 Real-GAM suite (operational row)

| Field              | Value                                                                                                                     |
| ------------------ | ------------------------------------------------------------------------------------------------------------------------- |
| Workflow           | `.github/workflows/real-gam-release.yml` (manual dispatch, release-gating; created in Phase 0)                            |
| Topologies         | one per A.2 flow, plus publisher-overlap and disabled-initial-load formation (G4a), plus the same-class supersession case |
| Browsers           | Chromium, Firefox, WebKit (CSP/opaque-origin rows); Chromium (funnel rows)                                                |
| Fixture            | dedicated GAM test network + line items targeting `hb_bidder=aps`; fixture doc in repo                                    |
| Account/credential | owner recorded in the Phase-0 DR (operator-held; never in repo)                                                           |
| Command            | `npx playwright test --config real-gam.config.ts` from the browser test package                                           |
| Artifact           | Playwright HTML report + trace zips as workflow artifacts, retained 90 days                                               |
| Retry policy       | one automatic retry per flaky-tagged spec; failures after retry are gate failures                                         |
| Approval evidence  | green workflow run URL linked in the release checklist, signed off by RO                                                  |
