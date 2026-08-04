# APS Render Fix and TSJS Resilience Architecture — Design

- **Status:** revision 6 — baseline advanced to the APS PUC/collapsed-shell
  fix, and reworked after the fifth review round.
- **Date:** 2026-08-04
- **Baseline:** `rc/july` @ `248fe9558` ("Fix APS PUC rendering and collapsed
  GAM shells") — the full merged state. All file:line citations refer to this
  commit.
- **Inputs:** three code audits; design reviews of revisions 1–5; open issues
  #926, #941, #944, #962, #964, #977, #983, #989, #993; open PR #997.

## 0. Release policy: coordinated hard cutover

This design targets a **single coordinated release**:

- Server, TSJS bundles, config format, and page HTML ship together under one
  **`release_id`** (git tag / build hash). **No N/N−1 support**: old pages,
  bundles, config blobs, globals, and URLs may stop working at cutover;
  in-flight clients may fail. Accepted and stated, not mitigated.
- **Exact release matching only** — kernel, services, plugins, and the
  install manifest carry the same `release_id`; mismatch is a refusal.
- **Config:** top-level `format_version`, exact match required. Rollback =
  redeploy the previous release with its own config.
- **Assets:** binaries embed only their release's artifacts; hashed pathnames
  exist for cache identity only; unknown hash → `410 Gone`, `no-store`.
- **One executable rollout state machine** (resolving the §0/§8 tension the
  review found): a release ships with a **deployment manifest** enumerating
  the complete flag set; the new pool comes up **fully enabled but
  unreachable except by probes**; phase gates (§8) run against probe traffic
  and a **router-weight canary of coherent routed requests** (a request is
  served end-to-end by one pool — HTML, assets, and APIs never mix pools);
  **router weight is the sole activation primitive**; cutover = weight to
  100% + CDN purge; rollback = weight back + re-purge. Flags exist for
  emergency kill switches inside a pool, not as the activation mechanism.

## 1. Problem statement

APS demand is fully integrated server-side — the edge runs the APS OpenRTB
auction, wins bids, and ships a typed renderer descriptor to the page — yet
APS creatives do not appear reliably for real users. Four serial fixes (the
`bid.meta` carrier, the decoupled prebid shim, the `hb_adid` fallback, and
now the baseline's PUC/collapsed-shell fix) each survived review; the pattern
is the finding: the pipeline has **multiple independent failure points, most
of which fail silently**, and the client cannot tell the server which fired.

The TSJS library (56 files, ~11,900 lines, two ~1,800-line monoliths,
duplicated ES5/TS logic, inverted layering, ~100 error-swallowing `catch`
blocks) is the same problem structurally. This design fixes APS delivery and
rebuilds TSJS so the next integration cannot reproduce this failure class.

### Non-goals

- No change to the APS OpenRTB endpoint contract or Amazon-side configuration
  (including its deliberate absence of `nurl`/`burl`, §G4d).
- No rewrite of the decoupled Prebid.js strategy.
- **No backward compatibility** (§0); replacement surfaces are in §7.4.

## 2. Why APS does not render — evidence

Flows: (a) SSAT via `window.tsjs.bids`; (b) GAM + client `trustedServer`
Prebid adapter; (c) SPA `/_ts/page-bids`; (d) direct `/auction`
`tsjs.requestAds`. Only (d) — unused in production — renders an APS
descriptor without GAM.

### 2.1 Admission

| #   | Failure                                                                                                                                                              | Where                                            |
| --- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------ |
| A1  | A configured `[auction].mediator` discards every direct-provider bid; winners come only from the mediator response. APS reports `success, bid_count: N`, never wins. | `orchestrator.rs:412-431`                        |
| A2  | `allow_script_creatives` defaults `false`, dropping every `tagtype: "script"` APS bid; the drop is counted but invisible (A4).                                       | `aps.rs:161`, `:334`, `:793`                     |
| A3  | Strict gates: exact `w`×`h` membership; required `ext.creativeurl`; any top-level `contextual` key rejects the whole response.                                       | `aps.rs:675`, `:763-796`, `:859`                 |
| A4  | Drop reasons reach only `/auction` `ext.orchestrator`; SSAT/page-bids discard them; logs and the `ts-debug` allowlist exclude them.                                  | `publisher.rs:1866-1875`, `telemetry.rs:808-826` |

### 2.2 Identity

| #   | Failure                                                                                                                | Where                                         |
| --- | ---------------------------------------------------------------------------------------------------------------------- | --------------------------------------------- |
| B1  | GAM caps key-value values at 40 chars; the raw APS bid id as `hb_adid` can fail the bridge equality check with no log. | `publisher.rs:3366-3372`, `gpt/index.ts:1695` |
| B2  | Two id universes: SSAT keys on the APS bid id, the client adapter on Prebid's generated `adId`.                        | `publisher.rs:3366`, `prebid/index.ts:982`    |

### 2.3 Render

| #   | Failure                                                                                                                          | Where                                                                 |
| --- | -------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------- |
| C1  | If GAM never serves the PUC, nothing renders and nothing is recorded; `renderApsCreative` is reachable only from flow (d).       | `gpt/index.ts:923-1180`, `core/request.ts:59`                         |
| C2  | A renderer endpoint that never answers is a silent 10 s death (opaque iframe cannot read HTTP status).                           | `aps.rs:1247`, `aps/render.ts:30`, `:415-437`                         |
| C3  | SafeFrame breaks slot attribution (top-document iframe walk cannot see nested creative windows).                                 | `gpt/index.ts:180-215`                                                |
| C4  | Three hand-maintained schema copies with exact-key rejection: a server field addition blanks every APS ad.                       | `types.rs:188-211`, `aps/render.ts:46-63`, `:152-162`, `aps.rs:65-93` |
| C5  | Fixed at baseline `248fe9558`: the duplicate renderer branch was consolidated; the served-through-APS-renderer log is reachable. | `gpt/index.ts:1729`                                                   |
| C6  | The renderer CSP can kill creatives after "ready" (no `object-src`, workers, `blob:`/`data:` frames).                            | `aps.rs:49`                                                           |
| C7  | Renderer branches record nothing: no trace record, no notifications.                                                             | `gpt/index.ts:1628-1760`                                              |

### 2.4 Observability

Zero client→server reporting. Server telemetry marks `is_win=1` at auction
time; a bid that never painted is byte-identical to one that painted.

### 2.5 Failure → signal mapping (normative)

Each failure maps to a distinct observable **failure class** (not a claim to
distinguish unknowable root causes). The operator query for each row is the
§5.6 canonical view filtered by that row's event/reason or counter. So that
**diagnostic mode really can name every class from one page load** — the
review's objection that A1–A4 are server-side-only — the tester gate also
delivers a **`tsjs.boot.debug` envelope** in the initial HTML (selection
summary + drop summary for the initial auction) and the equivalent gated
`ext.trusted_server.debug` field on page-bids and `/auction` responses;
diagnostic mode mirrors these to the console:

| Failure | Client event/reason (§5.1)                   | Server row/counter (§5.6)             | One-page-load surface           |
| ------- | -------------------------------------------- | ------------------------------------- | ------------------------------- |
| A1      | —                                            | `selection_summary.winner_source`     | `boot.debug` selection summary  |
| A2      | —                                            | `bid_drop{script_rendering_disabled}` | `boot.debug` drop summary       |
| A3      | —                                            | `bid_drop{invalid_dimensions, w, h}`  | `boot.debug` drop summary       |
| A4      | — (fixed by §5.6 itself)                     | `bid_drop` rows on all paths          | `boot.debug` / response `debug` |
| B1/B2   | `bridge_request{matched: false}`             | join via trace                        | console warn                    |
| C1      | `gam_empty` then no `bridge_request`         | join via trace                        | console warn                    |
| C2      | `render_fail{renderer_document_no_load}`     | renderer route counters               | console warn                    |
| C3      | `render_fail{bridge_id_mismatch}`            | join via trace                        | console warn                    |
| C4      | `render_fail{descriptor_invalid}`            | schema corpus CI                      | console warn                    |
| C5      | — (fixed at baseline)                        | —                                     | —                               |
| C6      | `runner_failed` + CSP buckets                | CSP aggregate counters                | console warn                    |
| C7      | renderer branch emits the full §5.1 sequence | join via trace                        | debug/warn                      |

## 3. The GPT and baseline reality this design must respect

1. Bootstrap-first hybrid: server-injected ES5 `gpt_bootstrap.js` wins the
   sentinel race; the bundle's handoff/initial-load code is dead in
   production.
2. The #922 merge loss: orphan recovery and `updateRender` are gone
   (`0dc9b19a9`); bridge impressions double-count. PR #997 is the apparent
   replacement.
3. TS refreshes never pass `changeCorrelator: false`.
4. `enableSingleRequest()` is called blind after publisher
   `enableServices()`.
5. Responsive-resolution ambiguity silently skips slots.
6. Three independent `pubads().refresh` wrappers coordinate via
   window-global booleans.
7. **GPT has no request cancellation, no documented per-refresh identity, no
   overlapping-completion order.** `slotRenderEnded` means creative code was
   injected, not that resources loaded. `responseIdentifier` identifies the
   ad **response** — usable for response dedup/drain, never initiation
   attribution.
8. **With initial load disabled, `display()` creates no request** — the
   subsequent `refresh()` does (`gpt/index.ts:1175`,
   `ad_init.test.ts:1201-1263`). Cycle protocols must model physical
   requests.
9. The bundle's `slotRenderEnded` registration is gated behind
   `!ts.servicesEnabled` (`gpt/index.ts:1091`); G4a needs unconditional
   early subscription.
10. **The baseline includes the fourth serial APS fix** (`248fe9558`): (a)
    the renderer handshake moved to a **MessageChannel** on the PUC path —
    port-transferred nonce message, descriptor over the port (no wildcard
    broadcast on that path), exact-key replies, `ports.length` checks, a
    one-shot `accepted` latch, port closing (`aps.rs:65-125`,
    `aps/render.ts:415-437`) — but the reply still terminates inside the PUC
    creative frame, so G4b's kernel-observability gap remains; (b) a
    nonempty GAM render can be a **collapsed 1×1 shell**, remediated by
    `resizeCollapsedCreativeFrame` (`gpt/index.ts:217`) — a guarded style
    mutation of GAM-owned elements; (c) the dead renderer branch was
    consolidated (C5); (d) a real-PUC-topology browser test now exists.
11. The bridge already keeps **consumed-id tombstones** for security
    (`gpt/index.ts:1527`) — G2's registry rules must preserve that property
    across navigations.
12. The current tester cookie is **explicitly not a security control**
    (`tester_cookie.rs:3`) — it cannot gate unsampled telemetry (§5.3).

## 4. Design gates

### G1 — Trace identity and correlation

The client-visible auction id is EC-derived (`publisher.rs:3237`) and never
ingested. Initial-HTML auction telemetry is emitted before page JS exists
(`telemetry.rs:148`, `publisher.rs:2452`), so correlation is minted by
whoever acts first:

- **Initial navigation (`nav_gen 0`):** the server mints `trace_id` (128-bit
  CSPRNG, `^[0-9a-f]{32}$`), writes it into that response's auction rows,
  and injects it into `tsjs.boot` with the signed authorization (§5.3).
- **Cache-privacy invariant:** traces/authorizations are injected only into
  responses that ran a per-request auction; such HTML is
  `Cache-Control: private, no-store`, no validators. By construction + test.
- **SPA navigations:** `/_ts/page-bids` stays GET; the client mints
  `trace_id`, sends it in `X-TSJS-Trace-Id`; the server records it and the
  response echoes the trace + its authorization.
- **Direct `/auction` (closing the G4f gap):** the client sends the same
  `X-TSJS-Trace-Id` header on the POST (`core/auction.ts:190` gains it); the
  server validates, stamps that auction's rows, and echoes
  `ext.trusted_server.trace = {trace_id, auth}` in the OpenRTB response —
  covering pages whose initial HTML ran no auction.
- **Envelope:** every event carries `{nav_gen, refresh_gen, seq}` inside a
  per-trace group `{trace_id, auth, events[]}`. `seq` is per-trace
  monotonic. Gaps (loss) and duplicates (fetch/pagehide races) are expected;
  §5.5 dedups.
- **Sampling is server-decided for every trace** and carried inside the
  signed authorization (§5.3). Traces are navigation-scoped; **impression /
  attempt counts are keyed by `(trace_id, nav_gen, refresh_gen, slot)`**
  (success criterion 5 uses this key, not "per-impression traces").

### G2 — Render identity

- Cache-backed bids: `hb_adid` = PBS Cache UUID byte-for-byte
  (`publisher.rs:3355`; PUC fetches `?uuid=`, `gpt/index.ts:1772`). Markup
  bids: today's fallback chain.
- **Renderer-only bids:** `hb_adid` = server-minted token `^[a-z0-9]{12}$`,
  CSPRNG, collision-retried within the minting auction; cross-auction
  uniqueness probabilistic (36¹²; negligible) and harmless via scoping.
- **Registry:** keyed `(trace_id, nav_gen, refresh_gen)`; capacity 64 live
  entries per navigation (`registry_full` on refusal); TTL 15 min; one-time
  consumption. **Navigation disposal does not erase security state**
  (review's tombstone finding): consumed, stale, and disposed ids move into
  a bounded **RuntimeSession tombstone set** (cap 256, FIFO, entries retained
  until their original TTL) — the bridge's TS-reserved check consults live
  registry **and** tombstones, so a late prior-navigation request is still
  suppressed and refused, never released to native Prebid. This preserves
  the baseline's existing consumed-id tombstone behavior
  (`gpt/index.ts:1527`).
- The client-Prebid path keeps Prebid's `adId`; both paths share the one
  registry. Non-APS cache-path regression tests.

### G3 — Runtime ABI under the IIFE build (exact-release)

IIFE-per-bundle with inlined imports (`build-all.mjs:46`, `bundle.rs:23`)
means imports never share state across bundles (live defect:
`core/context.ts:11` vs `permutive/index.ts:102`).

- Kernel ships only in `tsjs-core`; publishes
  `tsjs._internal = { release_id, registry }` once; freezes after boot;
  constructs and registers core services during boot.
- **Exact release matching:** every registration carries `release_id`
  (plugins via the object-form API, §7.6, whose `release` field is a
  build-generated constant); `registry.get(name)` succeeds only on equality;
  mismatch quarantines with `abi_mismatch` / `bundle_partial` and a console
  error.
- Stateful access only via the registry at call time; stateless helpers may
  inline. Boundary enforcement is **two lint rules**: `import/no-restricted-
paths` for layering **and** `no-restricted-globals` forbidding
  `window.googletag` / `window.pbjs` outside `adapters/` (import paths alone
  cannot enforce §7.1).

### G4 — Render lifecycle

**G4a — Physical request-cycle protocol.**

- **Intents, both classes, one causal queue.** Every observable initiation —
  TS `display()`/`refresh()` and wrapped publisher entries — records an
  intent in causal order, classified `ts | publisher`. A TS `display()`
  issued while initial load is disabled is **known at call time to produce
  no request** and is retired immediately as bookkeeping (it never enters
  the matcher) — closing the review's misattribution case where a publisher
  `refresh()` inside the 2 s window would have been consumed by a stale TS
  intent. TS intents that _may_ produce no request only in hindsight
  (`refresh()` on a never-displayed adopted slot) expire at 2 s with
  `intent_no_request`; **if any publisher intent is recorded for the slot
  while such a TS intent is pending, the next `slotRequested` is ambiguous
  and the slot quarantines** — a zero-request TS intent can never silently
  win FIFO matching. (Exact test in §9.)
- **Cycles:** opened only by `slotRequested`, matched to the head of the
  causal intent queue; SRA batching yields one `slotRequested` per slot per
  batch. A cycle closes on its `slotRenderEnded`; `responseIdentifier`
  deduplicates responses during drain.
- **Serialization:** at most one outstanding TS cycle per slot; one queued
  TS replacement (later intents coalesce).
- **Attribution:** a `slotRenderEnded` is attributable iff exactly one TS
  cycle is outstanding and no publisher/untracked request overlaps.
  Overlap → quarantine (`cycle_unattributable`, fail closed).
- **Drain/re-arm (no timeout re-arm).** Physical cycle and drain state live
  in the RuntimeSession slot record; **unissued intents are
  NavigationSession children** and are cancelled by navigation disposal. A
  quarantined or stale slot re-arms only on: count-based drain (every
  outstanding request/render pair matched), safe TS-owned slot destruction
  and redefinition, or page end. **A timeout emits a diagnostic and never
  restores attribution** — the 60 s bound from revision 5 is removed
  because an old `slotRenderEnded` arriving after re-arm would be
  indistinguishable from a new cycle. Late stale events are matched and
  discarded (`stale_navigation`).
- CI exercises the protocol on the deterministic harness; a release-gating
  **real-GAM overlap test** (publisher refresh racing a TS cycle;
  initial-load-disabled formation) validates it against actual GPT.

**G4b — Acknowledgement protocol (on the baseline port transport).** Since
`248fe9558` the frame pair speaks MessageChannel (parent-postMessage with
`ports.length === 0`, or transferred port with `ports.length === 1`;
exact-key replies; one-shot `accepted` latch; port closed after reply —
`aps.rs:65-125`, `aps/render.ts:415-437`). Adopted as the contract of record
within the frame pair. The kernel-observability gap remains (the PUC-flow
reply resolves inside the creative frame; callbacks fire on send,
`gpt/index.ts:1632-1760`). Contract — **three authenticated messages per
attempt**, each carrying the per-attempt 128-bit CSPRNG nonce from the
bridge response:

1. `renderer_document_loaded` — posted to the top window after the document
   validates the descriptor and nonce (this is §6.6's first stage, which
   revision 5's two-message protocol omitted);
2. the port reply to its frame-pair peer (baseline behavior, unchanged);
3. `render_accepted` / `render_failed{reason}` — posted to the top window.

The kernel validates, in order: source ownership (§6.8 walk), nonce, token,
`nav_gen`, `refresh_gen` — before any state transition or notification. The
one-shot latch + port close mean a re-render is a fresh document instance
with a fresh nonce. Pinned for SSAT, client-Prebid, and nested SafeFrame,
including stale/replayed acks and acks after navigation disposal.

**G4c — Honest observations.** Inline-adm frames are sandboxed `srcdoc`
without `allow-same-origin` (`gpt/index.ts:510`) — opaque; geometry proves
nothing (the shell dimensions are assigned by our own code). Observations:
`gam_nonempty`, `gam_empty`, **`gam_collapsed`** (nonempty render whose
shell computes ≤ 1px — the baseline's discovery), `renderer_document_loaded`,
`runner_loaded`, `runner_failed`, `adm_document_loaded`. Every path
terminates at `render_accepted`; **no observation claims paint**; there is
no `render_confirmed`. The baseline's `resizeCollapsedCreativeFrame`
(`gpt/index.ts:217`) is adopted as a **sanctioned, guarded exception** to
the no-foreign-DOM-mutation rule (authenticated source frame only; wrapper
only when both dimensions ≤ 1px; anchor-ad `ins[data-anchor-status]` and
fixed/sticky guards) and emits `gam_collapsed` when it acts.

**G4d — Win/billing notifications.** APS carries neither `nurl` nor `burl`
by design (`aps.rs:839`; the AAX envelope excludes them; the integration
guide documents no generic APS beacons) — APS billing lives in the Amazon
runner lifecycle, unchanged, and **APS is excluded from everything below**.

For carrying paths (PBS and other OpenRTB providers): bind is per flow and
never selection or targeting (`ad_init.test.ts:1824` pins that):

- GAM/PUC: an owned, slot-and-ad-id-matched bridge claim.
- Direct `/auction`: validated render start. **This requires server and
  client work the current code lacks**: `/auction` response conversion must
  preserve `nurl`/`burl` with server-side macro expansion
  (`formats.rs:423` omits them today) and the client parser must carry and
  https-validate them (`core/auction.ts:43` drops them today).
- Fallback: attributed `gam_empty`, immediately before fallback render.

`nurl` at bind; `burl` at `render_accepted`; attempt-scoped idempotency key
`(trace_id, nav_gen, slot, refresh_gen, hb_adid)`; `sendBeacon`/no-cors
fetch; no retries. Post-acceptance terminal failure emits the dedicated
**`billing_outcome{billed_then_failed}`** event (§5.1) — it is not a
`render_fail` reason.

**G4e — Fallback trigger.** Opt-in
(`[auction].client_render_fallback = "renderer"`); renders only after a
terminal `gam_empty` unambiguously attributed to a TS cycle; ownership does
not gate it; publisher-initiated or unattributable cycles never trigger it;
timeouts never render.

**G4f — Direct `/auction` lifecycle.** The non-GPT path
(`core/request.ts:52`) gets: a `RenderAttempt` keyed
`(trace_id, nav_gen, refresh_gen, slot)` with `refresh_gen` incremented per
`requestAds` invocation touching the slot; **per-slot serialization with
latest-wins cancellation** — concurrent calls for the same slot cancel the
older attempt, and every DOM or beacon side effect re-checks its attempt
generation first, so a reversed-arrival response can never replace a newer
creative or start a second economic lifecycle (`request.ts:31` currently
races); G4b acknowledgement validation; G4d direct-flow binds; disposal on
navigation; exactly-once terminal state; the full §5.1 event sequence.
`tsjs.requestAds(options)` returns
`Promise<RequestAdsResult>` where
`RequestAdsResult = { traceId, slots: Array<{ slot, outcome: "rendered" |
"no_bid" | "failed" | "cancelled", reason? }> }`, settling when every slot
attempt reaches a terminal state. Reversed-response tests required.

### G5 — Deployment contracts

- Config `format_version` exact-match; rollback by redeploy.
- Assets: hash-in-pathname; embedded only; unknown hash 410 `no-store`;
  `Cache-Control: public, max-age=31536000, immutable` on exact matches
  (`immutable` alone carries no lifetime). **Concatenation is materialized
  and cached at application-state construction from the validated
  configured module vector** — not per request (`bundle.rs:23` today), and
  not a build-time-only set, since enabled vectors are runtime
  configuration; an unlisted vector is a startup error.
- Internal route families (renderer, client-events, CSP reports): dispatch
  before auth/EC/publisher/integration filters (Fastly today runs EC setup
  and pre-route filters first, `app.rs:709`); all methods and version
  prefixes reserved locally (405 + `Allow` + `no-store`; unknown version
  404 `no-store`; never the publisher fall-through in `adapter-spin
app.rs:804`); no body/cookie/authorization forwarding; origins compared
  as normalized scheme+host+port.
- Ingest routes exist in all four adapters; Fastly has the real sink; the
  others accept-count-drop (OQ5 drives their gates).
- §5.6 schemas deploy and validate before writers enable.

## 5. Observability

### 5.1 Wire payload and per-event field matrix

```
{ v: 1, traces: [
  { trace_id, auth, events: [ { nav_gen, refresh_gen, seq, t, ...fields } ] }
] }
```

Event types and their fields (closed enums; a field absent from a row is
absent from the wire and NULL in storage):

| `t`                        | fields                               |
| -------------------------- | ------------------------------------ |
| `bid_received`             | slot, id_kind, source                |
| `targeting_set`            | slot, id_kind                        |
| `bridge_request`           | slot, id_kind, matched               |
| `bridge_response_sent`     | slot, source                         |
| `render_attempt`           | slot, source                         |
| `render_accepted`          | slot, source                         |
| `render_fail`              | slot, source, reason                 |
| `gam_nonempty`             | slot                                 |
| `gam_empty`                | slot                                 |
| `gam_collapsed`            | slot                                 |
| `renderer_document_loaded` | slot                                 |
| `runner_loaded`            | slot                                 |
| `runner_failed`            | slot, reason                         |
| `adm_document_loaded`      | slot                                 |
| `fallback_start`           | slot                                 |
| `billing_outcome`          | slot, outcome (`billed_then_failed`) |
| `client_queue_overflow`    | dropped (count)                      |

`slot` is a configured slot id or `s<ordinal>`; `id_kind` ∈
`cache_uuid | render_token | prebid_adid | bid_id | none`; `source` ∈
`renderer | adm | pbs-cache | gam`. Reason enum:
`renderer_document_no_load`, `runner_no_load`, `runner_failed`,
`descriptor_invalid`, `invalid_dimensions`, `dimensions_out_of_range`,
`bridge_id_mismatch`, `cycle_unattributable`, `intent_no_request`,
`stale_navigation`, `bridge_claim_timeout`, `gam_empty`,
`no_render_source`, `slot_unresolved`, `gpt_absent`, `pbjs_absent`,
`bundle_partial`, `fallback_cancelled`, `abi_mismatch`, `registry_full`.
Queue overflow is its own event (`client_queue_overflow`), never a
`render_fail` — failure denominators stay clean. The payload carries **no
client timestamp**; the server stamps `received_at`, and ordering within a
trace is `seq`.

### 5.2 Transport

`fetch(..., {keepalive: true, credentials: "omit"})` primary; `pagehide`
fallback `navigator.sendBeacon(url, new Blob([json], {type:
"application/json"}))`. Flush every 5 s and on `visibilitychange`/
`pagehide`. Client queue bound 256 events; overflow drops oldest and emits
`client_queue_overflow{dropped}`.

### 5.3 Signed trace authorization

Format `v1.<kid>.<exp>.<mode>.<sig>` — **`auth` has its own ingest bound of
256 bytes** (it cannot fit the general 64-char string cap; every other
string keeps 64):

- `kid`: `^[a-z0-9-]{1,16}$`; active + previous keys in the platform secret
  store; keys ≥ 256-bit CSPRNG; **previous keys are retained at least
  24 hours** (≫ max token lifetime + skew); missing key at startup with the
  beacon enabled = startup failure.
- `exp`: canonical decimal unix seconds (no sign, no leading zeros); ±60 s
  skew; max future 15 min.
- `mode`: `sampled | unsampled | diagnostic`. **`unsampled` is the signed
  discard decision** (the review's missing state): the client must not
  enqueue or transmit events for an `unsampled` trace, and ingest rejects
  any group whose token mode is `unsampled`; the decision is sticky for the
  trace (renewals preserve mode). `diagnostic` is a distinct authenticated
  mode — **not** gated by the tester cookie, which is explicitly
  non-security (`tester_cookie.rs:3`); it requires a separate short-lived
  **diagnostic credential** issued behind the existing operator/admin
  authentication (`/_ts/admin` surface): HMAC-signed, bound to publisher
  origin, expiry ≤ 60 min, revoked by key rotation, issuance
  CSRF-protected; forgery/replay tests required. The tester cookie may
  still gate cosmetic overlays; never telemetry volume.
- `sig`: base64url, unpadded, of HMAC-SHA-256 (43 chars) over the
  domain-separated input
  `"ts-trace-auth-v1" || u32be(len(origin)) || origin ||
u32be(len(trace_id)) || trace_id || u32be(len(mode)) || mode ||
u64be(exp)`, all strings UTF-8; constant-time comparison.
- **Renewal for long-lived pages:** before expiry the client calls
  same-origin `GET /_ts/trace-auth` with `X-TSJS-Trace-Id`; the server
  re-signs the **same trace id and mode** with a fresh `exp` (correlation is
  the unchanged trace id). On renewal failure the client stops transmitting
  and counts locally — silent rejection at ingest is thereby a bug, not a
  policy.
- Ingest verifies per trace group; invalid/expired/unknown-kid → group
  dropped-and-counted; other groups survive.

### 5.4 Ingest contract

- `POST /_ts/client-events`; `application/json` only; no
  `Content-Encoding`; `204`, `no-store`; never echoes input.
- Pre-parse limits: body ≤ 16 KiB; ≤ 64 events; strings ≤ 64 chars except
  `auth` ≤ 256 bytes; `trace_id ^[0-9a-f]{32}$`; integers `[0, 2³¹)`.
- Same-origin: `Sec-Fetch-Site: same-origin` when present, else normalized
  `Origin` equality; absent both → drop-and-count.
- **Rate limiting via an adapter abstraction** (the review is right that a
  cross-request in-memory token bucket cannot exist on Fastly, `app.rs:146`):
  trait `ClientEventLimiter` with a declared per-adapter backing and
  semantics — Fastly: the platform edge counter (`rate_limiter.rs:40`),
  fixed 60 s window, limit 20/window (documented approximation of
  10 rpm + burst 20); Axum: real in-process token bucket (10 rpm, burst
  20), map ≤ 65,536 entries; Cloudflare/Spin: per-isolate/per-instance
  best-effort with the same parameters. **At capacity, unseen identities
  are rejected (drop-and-count); active buckets are never evicted by
  churn.** Limiter unavailable/errored → drop early with `204`. Trusted
  client address per adapter: Fastly platform client IP; Axum rightmost
  `X-Forwarded-For` beyond required `trusted_proxy_hops` (absent → socket
  peer only); Cloudflare `CF-Connecting-IP`; Spin platform address.

### 5.5 Sink, canonical views, and monitoring

- Stable event key `(publisher_domain, trace_id, seq)`.
- **One named canonical dedup pipe/view per table** (`ts_client_events_v`:
  latest `received_at` per key; `ts_render_attempts_v`: attempt-grain
  aggregation keyed `(trace_id, nav_gen, refresh_gen, slot)`). **Joins run
  at attempt/slot grain against the views, never raw-to-raw** (a raw join on
  `(publisher_domain, trace_id)` multiplies rows). Dashboards and alerts
  may query only canonical views.
- Field naming matches the existing auction rows: **`publisher_domain`**.
- The Fastly sink is fire-and-forget after dispatch (`tinybird.rs:153`) and
  cannot see downstream rejection — **datasource-side monitoring is
  mandatory**, driven by **sequence-tagged synthetic heartbeats** from a
  probe client (accepted rows cannot reveal rejected rows): heartbeat gaps
  measure rejection; heartbeat lag measures freshness. Alert owner: the
  release owner's on-call.

### 5.6 Physical schemas (deployed before writers)

- **`ts_client_events`**: `received_at DateTime64, publisher_domain
LowCardinality(String), release_id String, trace_id FixedString(32),
mode Enum(sampled|diagnostic), nav_gen UInt32, refresh_gen UInt32,
seq UInt32, event Enum(§5.1), slot Nullable(String), id_kind
Nullable(Enum), matched Nullable(UInt8), source Nullable(Enum), reason
Nullable(Enum), outcome Nullable(Enum), dropped Nullable(UInt32)`.
  Sorting key `(publisher_domain, received_at, trace_id, seq)`; TTL
  30 days; own ingest token; sink batch cap 512 rows; startup validation of
  dataset + token when enabled ("startup" on request-bound platforms such
  as Cloudflare means first-request lazy initialization with a cached
  result); sink-unavailable at runtime → accept-count-drop.
- **Auction rows** (`telemetry.rs:262`, `auction_events_raw.datasource`):
  add nullable `trace_id`, `mode`; add two bounded row types — **`bid_drop`**
  `{provider, slot Nullable, reason Enum, width Nullable(UInt16), height
Nullable(UInt16), count UInt32}` (nullable slot/dimensions for
  response-level failures; cap 32 rows/auction + overflow row) and
  **`selection_summary`** per slot
  `{slot, winner_source Enum(mediator|direct|none), winner_provider,
candidates_direct UInt16, candidates_mediator UInt16, dedup_hits,
currency_rejected, provenance_invalid, mediator_superseded}` (cap 8
  rows/auction + overflow) — §6.1's selection report now has a physical
  home.
- **Settings schema (complete):** `[telemetry.client_events]` `enabled`,
  `sample_rate` (0–1), `dataset`, `token_secret`; `[telemetry.trace_auth]`
  `secret_store`, `active_kid`, `previous_kids = []`; the diagnostic
  credential secret alongside. `RuntimeServices` (`platform/types.rs:158`)
  gains the client-events sink handle next to the auction sink.
- APS parsing returns structured drop observations
  `{reason, slot, width?, height?}` (`aps.rs:722` today loses slot and
  values); >8192 → `dimensions_out_of_range`, dimensions omitted.

### 5.7 Modes and SLIs

- **Production (sink-backed only):** server-decided sampling
  (`sample_rate`, default 0.10; `sampled` vs `unsampled` signed per trace).
  Separated SLIs: **pipeline availability** (heartbeat freshness ≤ 5 min,
  heartbeat loss < 0.1%; fails during sink outages, alarmed); **failure
  detection** (a failure mode affecting ≥ 1% of sampled render attempts
  visible within one hour, evaluated at ≥ 10,000 sampled attempts/hour).
- **Diagnostic:** credential-gated (§5.3), unsampled, full stream, console
  mirroring, plus the `boot.debug` / response-`debug` envelopes (§2.5) — one
  page load names the failing class for every §2 row.

### 5.8 Server-side drop surfacing

Bounded structured summary whenever any bid is dropped; `bid_drop` +
`selection_summary` rows; `ts-debug` comment carries the drop summary;
page-bids and `/auction` carry the gated structured `debug` field. Startup
warnings: APS + `allow_script_creatives = false`; mediator + direct
providers without an explicit `winner_selection` (§6.1 hard error).

## 6. APS delivery fixes

### 6.1 Mediation: complete, arrival-independent algorithm

Current code cannot merge: no forwarded candidate id; lossy
last-write-wins `(provider, slot, bidder)` restoration
(`adserver_mock.rs:95`); arrival-order ties (`orchestrator.rs:827`); Prebid
assumes USD (`prebid.rs:2433`) and APS stamps USD (`aps.rs:475`) with no
configured currency. Replacement, identical in the synchronous and split
dispatch/collect paths via one shared helper:

1. **Currency.** New required field `[auction].currency` (ISO 4217). Every
   provider parse validates its response currency against it (absent
   declaration where the provider contract implies one — APS's USD — is
   validated as that implied value); mismatch → `bid_drop{currency_mismatch}`.
   No conversion.
2. **Candidates.** Every admitted bid — direct **and mediator-native** —
   becomes a candidate. Identity is two-part: `source_candidate_id` =
   the **intrinsic stable key** `(provider_name, upstream_bid_id)` (for
   mediator-native bids: the mediator's provider name and its bid id), and
   `candidate_id` = a server-minted opaque wire id (`c` + 11-char CSPRNG)
   used **only** for the mediator echo — never for ordering, so response
   arrival order cannot influence selection.
3. **Mediator exchange.** Forwarded candidates carry `candidate_id` in the
   named wire extension **`ext.trusted_server.candidate_id`** (contract for
   every mediator implementation, `adserver_mock` included); the mediator
   echoes it on derived bids. Echoed id resolves → the bid is the forwarded
   candidate with provenance `mediator`; **authoritative fields:** price
   and deal fields come from the mediator (repricing is its job);
   render-source fields (renderer, adm, cache coordinates, notification
   URLs) come from the stored candidate — a mediator that returns its own
   `adm` for an echoed candidate is treated as mediator-native demand
   instead. Unresolvable echoed id → the slot **fails closed for merging**
   (`mediation_provenance_invalid`; mediator-native bids for the slot still
   compete; the claim is discarded and counted).
4. **Floors** filter both populations. **Dedup:** an echoed candidate
   removes its direct twin.
5. **Selection order (total, intrinsic):** decoded CPM desc → provenance
   rank (mediator first) → `source_candidate_id` asc. Winner fields are
   read from the stored candidate per rule 3.
6. **Strategy.** `[auction].winner_selection` is **required** whenever a
   mediator and direct providers coexist (startup error if absent):
   `mediator_only` or `merge_highest_cpm`. **Timeout behavior is
   strategy-specific:** under `merge_highest_cpm`, mediator timeout →
   direct-only selection, reported; under `mediator_only`, mediator timeout
   → **no winners** (direct bids stay signal-only) unless
   `mediator_timeout_fallback = "direct"` is explicitly configured.
7. **Reporting:** the `selection_summary` row (§5.6) per slot.

Deal priority stays out of scope (the `Bid` model carries no deal identity;
recorded follow-up).

### 6.2 Dimensions

Exact size membership stays (`aps.rs:675`). Fix is visibility
(`bid_drop{invalid_dimensions, w, h}`) plus documentation: request the
sizes you accept.

### 6.3 Script creatives

Default stays `false`; consequence loud (§5.8); enablement documented.

### 6.4 Render identity

As G2 (including tombstones).

### 6.5 Fallback

As G4e/G4a; awaitable renderer first; attribution-gated; timeouts never
render.

### 6.6 Renderer endpoint

- Route registers unconditionally in every adapter (provider stays
  config-gated); startup validation fails if an auth handler pattern covers
  it; §G5 isolation rules apply.
- Path `/integrations/aps/renderer/v1`, embedded, served
  `Cache-Control: public, max-age=31536000, immutable`; canary versions
  are `no-store` (or bounded below the cohort lifetime); a **checked-in
  header manifest per renderer version** freezes headers (CSP included)
  with the bytes — a version's headers never change after publication,
  resolving the immutable-caching/CSP conflict. Unknown versions → 404
  `no-store`.
- Three-message acknowledgement per G4b (`renderer_document_loaded` is the
  first authenticated envelope).
- Server route counters are aggregate (the nonce rides the URL fragment).
- **CSP rollout:** discovery on the currently enforced policy with
  reporting; tightening via report-only; relaxation via a small enforced
  cohort on a short-lived canary version, gated on runner acceptance, CSP
  violation rate, and render-failure rate, with a kill switch — and **CSP
  reports are advisory**: for opaque-origin reports the body-supplied
  document URL and policy version are forgeable, so **policy identity is
  encoded in a server-selected report path**
  (`POST /_ts/csp-reports/<policy-id>`, ids server-generated per version),
  reports are bucketed into closed effective-directive buckets and
  `https-host (allowlisted) | data | blob | inline | eval | other` source
  buckets with global and per-cohort caps, unused fields discarded before
  logging, and CSP data is **never a sole automatic rollback signal**.
  Physical storage: aggregate counters only. Browser capture on Chromium,
  Firefox, and WebKit (CI is Chromium-only today,
  `playwright.config.ts:16`), both report media types
  (`application/csp-report`, `application/reports+json`) with separate
  validators.

### 6.7 One descriptor schema

Wire truth is the tagged `BidRenderer` envelope (`types.rs:188-211`). A
wire-schema crate/xtask (no `core → js` cycle; core already depends on the
js crate, `Cargo.toml:45`) generates the JSON-Schema artifact, the TS
structural parser, the ES5 inline validator fragment, and shared fixtures —
checked in, staleness-gated. Semantic checks (URL/origin policy, canonical
base64, bounds, exact one-bid AAX projection, cross-field equality) stay
handwritten. Outer-descriptor tolerance only; AAX projection exact. Shared
positive + adversarial corpus runs through all three validators in CI.

### 6.8 Bridge hardening

Order (normative; preserves the baseline defense that suppresses
propagation before source validation, `gpt/index.ts:1584-1637`): parse →
identify TS-reserved id (live registry **or tombstone**, G2) →
`stopImmediatePropagation()` → validate source ownership via the bounded
walk (known slot-root `WindowProxy` map; sender's parent chain to depth 5;
never scanning the frame tree) → validate nonce/token/`nav_gen`/
`refresh_gen` → respond or refuse (`bridge_id_mismatch`). Non-TS ids are
untouched. The stolen-token browser test asserts neither TS nor native
Prebid responds; listener-order has a real-browser assertion. Renderer
branches emit the full §5.1 sequence; notifications only on carrying paths.

## 7. TSJS target architecture

### 7.1 Layering

```
kernel/          boot, config, queue, event bus, log, beacon, sessions
adapters/        googletag.ts, pbjs.ts, messaging.ts   ← the ONLY window.* access
services/        slots (registry+handoff), auction client, render engine, consent
integrations/    gpt, prebid, aps, creative, datadome, …
```

Enforced by the two G3 lint rules. Dissolves the audited inversions
(`core/auction.ts`/`core/request.ts` → `integrations/aps/render`;
`gpt`/`prebid` → `aps`; `prebid` owning the GPT refresh wrapper).

### 7.2 Adapters

`present | pending | timed_out` per external global; `timed_out`
non-terminal; queued operations carry their own timeouts and expire with
disposition reasons.

### 7.3 Slot registry service

Kernel-owned; `WeakMap<googletag.Slot, SlotRecord>` + div-id index;
ownership, adoption, handoff claims, responsive resolution, the G4a causal
intent queue + cycle/drain state (RuntimeSession) and unissued intents
(NavigationSession), targeting history. No expandos
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

**Bootstrap correctness (closing the review's two holes):** every
server-injected initializer creates the container **idempotently and
field-wise** — `window.tsjs ||= {}; tsjs.que ||= []; tsjs.boot ||= {}` —
never only-when-absent (today the first ad-slot script does
`window.tsjs = {}`, `publisher.rs:3665`, which would clobber or starve
later `boot` writes). Kernel boot claims an **atomic owner sentinel**; it
consumes `boot`, deep-freezes the retained copy, and deletes one-shot
secrets. The generated no-bundle fallback (§7.7) activates on bundle
`error` **or** a bounded hang watchdog (10 s without kernel boot); if the
fallback has activated and the bundle later arrives, the bundle **defers
for the rest of the page** (logs + `bundle_partial` disposition) — queue
ownership never changes hands mid-page.

### 7.5 Messaging module

All `postMessage` through one module: versioned envelopes, name constants,
G4b nonces, §6.8 validation. Minimal module lands in Phase 1; full call-site
migration in Phase 4.

### 7.6 Plugin lifecycle — transactional — and sessions

`tsjs.definePlugin({id, release, install, dispose?})` — object form; the
`release` field is the build-generated `release_id` constant (G3 needs it;
revision 5's positional API omitted it). `install(ctx): void |
Promise<void>`:

- `ctx.signal`; synchronous `ctx.onDispose(fn)`; reverse-order unwind on
  throw/reject/abort; per-disposer isolation; disposer registered after
  disposal → invoked immediately; pending late registrations capacity 16,
  bound 10 s → `bundle_partial`; release mismatch quarantines before
  `install`.
- Sessions: `RuntimeSession` (page-lifetime: bridge listener + tombstones,
  history hook, pbjs subscriptions, adapters, beacon queue, physical slot
  cycle/drain state); `NavigationSession` (trace + authorization + renewal
  timer, render attempts, slot aliases, unissued intents, targeting
  history); `RenderAttempt` (per G4a cycle / G4f attempt). Enumerable
  disposal inventories; navigation disposes NavigationSession children
  only.
- No empty `catch`; auction fetch gains timeout + `AbortController`.
- **Console logging retained**: every issue-surfacing condition keeps or
  gains a `log.warn` with the beacon's reason code; `debug`-level
  delivery/security failures promoted to `warn`.

### 7.7 Bootstrap

`gpt_bootstrap.js` shrinks to a queue-and-flags stub; the bundle replays
recorded calls on install (browser specs cover replay timing); the
no-bundle fallback is **generated from the same TypeScript source**, with
the §7.4 activation/arbitration rules.

### 7.8 GPT correctness carried with the restructure

Unconditional early `slotRequested`/`slotRenderEnded` subscription
(replacing the `!servicesEnabled` gate, `gpt/index.ts:1091`; idempotent
recording); restore #922/#997 attribution and orphan recovery;
`changeCorrelator: false` on TS refreshes (configurable);
`enableSingleRequest()` only when services are not already enabled;
ambiguous responsive resolution emits `render_fail{slot_unresolved}`.

### 7.9 Decomposition targets

| Today                                | Target                                                                                                             |
| ------------------------------------ | ------------------------------------------------------------------------------------------------------------------ |
| `gpt/index.ts` (~1850 LOC)           | `slot_resolution`, `handoff`, `initial_load`, `ad_init`, `render_bridge`, `spa_navigation`, `beacons` + thin index |
| `prebid/index.ts` (1671 LOC)         | adapter, shim, refresh handler (onto the slot registry), eids, diagnostics                                         |
| `gpt/script_guard.ts` (634 LOC)      | folded onto the 170-LOC `shared/script_guard.ts` factory                                                           |
| `core/trace.ts` (model + UI)         | `services/trace` + `integrations/trace_overlay`                                                                    |
| `core/global.d.ts` (`pbjs: TsjsApi`) | real Prebid types; `TsjsApi` split public vs internal                                                              |

### 7.10 Performance (reproducible)

- **Dedicated workflow** on a fixed runner (`runs-on: ubuntu-24.04`
  explicitly — browser CI is `ubuntu-latest` today,
  `integration-tests.yml:155` — inside a pinned container image digest);
  browser = the lockfile-resolved `@playwright/test` build with its browser
  revision recorded in the baseline artifact (the manifest is a caret
  range today, `browser/package.json:10` — the lockfile + recorded
  revision are authoritative); compressors pinned by version in the
  container (`gzip -9 -n` for determinism, `brotli -q 11`).
- Bundle budgets: raw/gzip/Brotli for three vectors (minimal, reference,
  maximal) vs checked-in baselines (`perf/baselines/*.json`, updates are
  reviewed diffs recording image/browser/tool versions); +5% bytes.
- Browser timing: 5 warm-ups discarded, 50 samples, p90 ≤ baseline × 1.10.
- **Server benchmark harness (complete):** workload = concatenation +
  hash of the reference vector; 100 warm-up iterations, 1,000 measured;
  statistic = median and p90; one-sided gates ≤ baseline × 1.10; variance
  policy: 3 consecutive runs must agree within 5% or the result is
  inconclusive (rerun, never pass).

### 7.11 Toolchain

TypeScript floor to the resolved 5.9 line; strictness flags on; dev
toolchain bumps as individual CI-gated PRs; `prebid.js` excluded from
casual bumps; monthly review.

## 8. Rollout: phases, decision records, and executable gates

Phases are build milestones inside the §0 single-release model (dark pool →
probe gates → router-weight canary of coherent requests → full weight).

**Phase 0 decision records** (promoted from open questions; each has an
owner, evidence, a deadline, and an explicit go/no-go): DR-1 mediator
presence in the affected deployment (OQ1 — decides whether §6.1 gates
Phase 3 entry); DR-2 script-creative share (OQ2 — decides the §6.3
guidance priority); DR-3 #922 vs #997 (OQ4 — decides the Phase 3 work
item); DR-4 mediator candidate-id echo owner and timeline (OQ7 —
`merge_highest_cpm` is config-blocked until delivered); DR-5 non-Fastly
sink decision (OQ5 — splits Phase 2's gates below).

**Gates are a checked-in table** (`docs/superpowers/specs/rollout-gates.md`,
created in Phase 0) with columns: query/test command, assignment key,
expected positive count, denominator, sample floor, threshold, window,
owner, hold/rollback action. The real-GAM suite's row includes its workflow
name, fixture account and credentials owner, invocation command, artifact
location, retry policy, and required approval evidence. Prose below is the
summary; the table is normative.

- **Phase 0 — Identity, schemas, toolchain, decisions.** Path-hashed
  assets + 410 semantics + construction-time concatenation cache;
  `format_version`; §5.6 schemas deployed writer-off; toolchain floors;
  dead expando writes deleted; §5.8 drop surfacing; the five decision
  records; the gates table itself.
- **Phase 1 — Kernel, sessions, minimal messaging, cycle registry.** G3
  registry; sessions; install manifest; minimal messaging; G4a intent/cycle
  records; unconditional GPT subscriptions.
- **Phase 2 — Trace + beacon.** G1 issuance on all three paths (boot,
  page-bids, `/auction` extension); §5.3 authorization incl. renewal and
  the diagnostic credential; four-adapter ingest; `ts_client_events`
  writers on. Gates split per DR-5: **HTTP parity** (all adapters:
  routing, limits, 204s, method reservations) vs **persistence**
  (sink-backed only: acceptance, dedup-exactly-once, heartbeat freshness).
- **Phase 3 — APS delivery.** Schema crate + corpus; §6.1 with required
  `winner_selection` and `[auction].currency`; render token + tombstones;
  renderer route + three-message ack + CSP report route; §6.8; G4a–G4f
  incl. direct-flow `nurl`/`burl` plumbing; fallback; DR-3's attribution
  restoration; correlator + SRA fixes.
  _Gate (sticky randomized canary/control cohorts, 24 h, ≥ 10,000 sampled
  attempts each; missing telemetry counts as failure):_ **denominator =
  all server-observed eligible APS wins**; per-stage rates gated
  separately — targeting_set/eligible, bridge_request/targeting_set,
  render_accepted/bridge_response_sent, and `cycle_unattributable` rate
  < 0.5% (survivorship is thereby visible, not excluded); GAM fill and p95
  latency as **one-sided non-inferiority** (canary not worse than control
  by > 2%; improvements pass); billing normalized per attempt and per
  thousand attempts vs control; a separate **duplicate-billing invariant**
  (zero double `burl` per idempotency key); real-GAM overlap suite green
  per its table row.
- **Phase 4 — Structure.** Full layering + both lint rules; plugin
  lifecycle; adapters; full slot registry; full messaging migration; final
  namespace.
  _Gate:_ lints zero exceptions; disposal-inventory leak tests; four-flow
  behavioral parity (SSAT, client-Prebid, page-bids, direct).
- **Phase 5 — Decomposition + cutover.** File splits; script-guard
  consolidation; bootstrap stub + generated fallback (with error/hang/
  fallback-arbitration tests); **four-flow parity reruns here** (it changed
  bootstrap behavior after Phase 4's parity — the review's ordering
  point); then the §0 runbook: weight-up, purge, 24 h monitored window,
  weight-back rollback.

## 9. Test acceptance matrix

Hermetic CI blocks PRs; the real-GAM suite is release-gating per its gates
row. New/changed rows this revision are marked •.

| Area              | Must cover                                                                                                                                                                                                                                                                                                                                                      |
| ----------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Request cycles    | intent-vs-request; • disabled-initial-load `display()` retired at issuance, publisher `refresh()` inside 2 s window attributed to publisher (the exact review case); SRA; `intent_no_request`; overlap quarantine; • no timeout re-arm (drain/destroy/page-end only); stale discard; real-GAM overlap                                                           |
| Ack protocol      | three-message sequence (• `renderer_document_loaded` envelope); five-field validation; SSAT/client-Prebid/SafeFrame; stale/replayed; after-disposal acks                                                                                                                                                                                                        |
| Bridge security   | propagation stopped before validation; neither TS nor native Prebid responds to stolen ids; • prior-navigation ids suppressed via RuntimeSession tombstones after NavigationSession disposal; bounded walk; listener order                                                                                                                                      |
| Render semantics  | binds per flow; `burl` at accepted; attempt idempotency; • `billing_outcome{billed_then_failed}` as its own event; accepted-but-blank; • `gam_collapsed` emission + guarded resize (authenticated source only; 1×1 wrapper; anchor/fixed guards)                                                                                                                |
| Direct `/auction` | • trace header + response `ext.trusted_server.trace`; • per-slot latest-wins with reversed responses; • generation check before each DOM/beacon effect; • `RequestAdsResult` settlement; • server preserves + expands `nurl`/`burl`, client validates                                                                                                           |
| Fallback          | attributed `gam_empty` only; publisher-initiated never; timeout never renders; SPA cancellation; flag change mid-attempt                                                                                                                                                                                                                                        |
| Mediation         | • `[auction].currency` required + per-provider validation (Prebid parse, APS implied USD); • `ext.trusted_server.candidate_id` echo; • mediator-native candidates ordered by intrinsic key (arrival-order shuffle test); • authoritative-field rules (repricing kept, adm-swap → native); provenance fail-closed; • strategy-specific timeouts; both lifecycles |
| Render token      | format/CSPRNG/retry/TTL/one-time; `(trace, nav_gen, refresh_gen)` scope; capacity → `registry_full`; • tombstone retention to original TTL, cap 256                                                                                                                                                                                                             |
| Trace auth        | • auth ≤ 256 B bound accepted, 64-char cap for others; • encoding vectors (kid charset, canonical exp, u32be/u64be length prefixes, unpadded base64url); expiry/skew/max-future; • renewal preserves trace + mode; • previous-key retention ≥ 24 h; rotation; per-group rejection                                                                               |
| Sampling modes    | • signed `unsampled`: client transmits nothing, ingest rejects carried events, stickiness across renewal; • diagnostic requires the operator credential — tester cookie alone must fail; forgery/replay of the credential                                                                                                                                       |
| Beacon            | joins on all three issuance paths; per-trace grouping; seq gaps; duplicate fetch/pagehide deduped in the canonical view; • `client_queue_overflow` not in failure denominators; ingest abuse; sendBeacon Blob                                                                                                                                                   |
| Ingest/limits     | • per-adapter limiter semantics as declared (Fastly fixed-window approximation, Axum token bucket); • at-capacity rejects unseen identities, never evicts active; fail-closed 204                                                                                                                                                                               |
| Internal routes   | wrong-method 405 + Allow + no-store on every adapter; unknown version 404; no publisher fall-through; dispatch before auth/EC/filters; no forwarding                                                                                                                                                                                                            |
| CSP               | both media types; opaque/null-origin admission; • policy identity from server-selected report path (forged body URL/version ignored); bucketed aggregation with caps; three-browser capture; • header manifest per version (immutable headers frozen with bytes)                                                                                                |
| Schema            | staleness; adversarial corpus ×3 validators; outer tolerance vs exact AAX                                                                                                                                                                                                                                                                                       |
| Runtime ABI       | one kernel; exact-release verdicts; late registration; failure isolation; • object-form `definePlugin` release check                                                                                                                                                                                                                                            |
| Plugins           | partial-install unwind; async rejection; abort pending; disposer-after-disposal; isolation                                                                                                                                                                                                                                                                      |
| Lifecycle         | `timed_out → present`; session inventories; • unissued intents cancelled by navigation disposal; boot container idempotent field-wise init (• ad-slot script no longer clobbers); • fallback error/hang activation + late-bundle deferral; final-namespace smoke                                                                                                |
| Delivery          | unknown hash 410 no-store; exact-match immutable with full directive; • construction-time vector cache (unlisted vector = startup error); cutover rehearsal                                                                                                                                                                                                     |
| Sink/monitoring   | • sequence-tagged synthetic heartbeats measure rejection + freshness; • canonical views only (raw-join multiplication test); `publisher_domain` naming                                                                                                                                                                                                          |
| Failure injection | Amazon runner redirect/hang/CSP/script error → distinct outcomes; EC/filter failure before renderer dispatch                                                                                                                                                                                                                                                    |
| Adapter parity    | ingest, CSP-report, renderer routes and drop surfacing equivalent across Fastly/Viceroy, Axum, Cloudflare, Spin                                                                                                                                                                                                                                                 |
| Policy            | script-creative warning; `invalid_dimensions` w/h; `dimensions_out_of_range` unclamped; • `boot.debug` + response `debug` gating; diagnostic completeness per §2.5                                                                                                                                                                                              |

## 10. Alternatives considered

1. Patching APS point-failures without telemetry — rejected (four correct
   fixes, still no reliable ads).
2. Always direct-render APS — rejected; kept as the attributed-`gam_empty`
   fallback.
3. Single module graph now — rejected for this release; successor option.
4. Big-bang rewrite without phases — rejected (thin safety net).
5. Dropping the ES5 bootstrap — rejected (loses the no-bundle guarantee);
   generated fallback keeps it.
6. Timeout-triggered fallback rendering — rejected (uncancelable GPT
   requests race late fills).
7. Timeout-based quarantine re-arm (revision 5) — removed: it recreated
   the stale-event bug it claimed to fix.
8. N/N−1 compatibility machinery (revisions 3–4) — removed by the §0
   policy.

## 11. Risks

- Hard-cutover blast radius (accepted; bounded by the §0 runbook).
- Mediator wire-contract change (`candidate_id` echo) — DR-4 gates
  `merge_highest_cpm`.
- Notification triggers become a published contract for PBS-path demand.
- Beacon abuse — capped, origin-checked, fail-closed limited, signed
  modes, credentialed diagnostics.
- Registry/limiter memory — explicit capacities, TTLs, reject-at-capacity.
- CSP data is advisory — never a sole rollback signal.
- Sink blindness — heartbeat-based datasource monitoring.
- `[auction].currency` and `winner_selection` are new required config in
  mediated deployments — a deliberate startup-error class under §0.

## 12. Success criteria

1. APS creatives render in each configured flow (SSAT, client-Prebid,
   page-bids, direct), hermetically and in the release-gating real-GAM
   suite.
2. Every §2 failure maps to its §2.5 signal; **diagnostic mode names the
   failing class from one page load including A1–A4 via the `boot.debug` /
   response-`debug` envelopes**; §5.7 SLIs hold on sink-backed
   deployments.
3. Lints (both rules) zero exceptions; stateful sharing only via the
   registry; exact-release mismatches quarantine loudly.
4. No `src/` file exceeds ~500 lines; `gpt_bootstrap.js` is a stub or
   generated.
5. Attempt counts are keyed `(trace_id, nav_gen, refresh_gen, slot)`
   (traces stay navigation-scoped); no double counting; orphan recovery
   has a non-vacuous test; G4a holds including the no-timeout-re-arm rule.
6. The only TSJS-owned global is `window.tsjs` (§7.4 final shape); no
   expandos; legacy names gone at cutover.
7. §7.10 budgets hold on the dedicated pinned workflow.
8. No existing warning lost; issue-surfacing conditions log `warn`+ with
   the beacon reason code.
9. TypeScript floor matches resolved 5.9; `prebid.js` pin documented with
   the deployed bundle.
10. `nurl`/`burl` only on carrying paths at their G4d binds, idempotent
    per attempt; APS fires neither; duplicate-billing invariant holds.
11. Trace-bearing responses are `private, no-store`; authorizations are
    per-trace, signed, mode-carrying (`sampled|unsampled|diagnostic`),
    renewal-capable; unsampled traces transmit nothing.
12. The cutover runbook rehearsed (weight switch, purge, rollback).

## 13. Open questions

Promoted to Phase 0 decision records: mediator presence (DR-1),
script-creative share (DR-2), #922 vs #997 (DR-3), candidate-id echo owner
(DR-4), non-Fastly sinks (DR-5). Remaining open: does Amazon expose any
creative-completion acknowledgement that could add a post-`render_accepted`
state under a new name (future enhancement)?
