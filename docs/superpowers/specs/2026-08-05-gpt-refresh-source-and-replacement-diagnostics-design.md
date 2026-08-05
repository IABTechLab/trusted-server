# GPT Refresh Source and Replacement Diagnostics

**Date:** 2026-08-05  
**Status:** Proposed  
**Scope:** Opt-in GPT diagnostics only; zero publisher-code changes

## Relationship to Existing Diagnostics

This design extends
`2026-08-04-gpt-delivery-evidence-and-auction-competition-design.md`. The
existing implementation distinguishes Trusted Server direct requests, Prebid
refreshes, competing markers, and unattributed GPT requests. Deployed Autoblog
evidence confirmed that a late Trusted Server request can replace an already
rendered publisher ad, but the diagnostics do not yet identify ordinary
publisher refreshes or summarize the replacement relationship.

The change remains observational. It must not suppress, delay, reorder, add, or
remove GPT requests; change targeting; alter auction timeouts; or require
publisher JavaScript, framework, DOM, Prebid, or GAM changes.

## Decision

Represent every integration-observed GPT request trigger as a short-lived,
per-slot request intent. The next matching `slotRequested` consumes the current
intent evidence and records its source and correlation facts on that request
cycle.

The diagnostics will additionally relate a newly rendered request to the most
recent earlier filled render for the same GPT slot. This makes an observed
replacement explicit and reports the time between the previous render and the
next request, plus a creative-ID transition when GPT exposes both IDs.

The implementation will not infer a refresh source from targeting values,
elapsed time, request number, element IDs, or network parameters. An invocation
that bypasses all installed integration boundaries remains `unattributed`.

## Goals

- Distinguish Trusted Server direct, Prebid-managed, and observed publisher
  refresh requests.
- Preserve the existing `competing` classification when more than one observed
  path contributes to the same request.
- Correlate a Trusted Server request with its server auction ID when supplied by
  the bid response.
- Report opportunity-to-GPT-request latency.
- Report when a later filled render replaces an already-rendered filled cycle.
- Report previous-render-to-next-request latency and GAM creative-ID changes.
- Keep exported data bounded, browser-local, free of bid payloads and creative
  markup, and safe when diagnostics code is absent or malformed.
- Preserve zero publisher-code changes and identical delivery behavior.

## Non-goals

- Preventing the late Trusted Server refresh or changing hydration/ad-init
  behavior.
- Deciding whether a publisher refresh should be suppressed.
- Inferring an auction winner from targeting or timing.
- Capturing prices, targeting maps, markup, network bodies, or stack traces.
- Proving visual correctness inside a cross-origin creative iframe.
- Attributing calls made through a refresh function reference captured before
  the Trusted Server wrappers were installed.
- Changing callback matching, `slotOnload` ambiguity handling, or the existing
  ten-cycle-per-slot retention limit.
- Adding server processing timestamps. This iteration correlates the opaque
  server auction ID and measures only browser-observed intervals.

## Evidence Model

### Request intents

Replace the two independent pending-marker concepts with one per-slot request
intent record that can accumulate source evidence before GPT emits
`slotRequested`:

```ts
type GptDiagnosticsRequestIntentSource =
  | 'trusted_server_direct'
  | 'prebid_refresh'
  | 'publisher_refresh'

interface PendingRequestIntent {
  intentId: number
  sources: Map<GptDiagnosticsRequestIntentSource, PendingSourceEvidence>
}

interface PendingSourceEvidence {
  generation: number
  observedAtMs: number
  trustedServerOpportunity?: GptDiagnosticsTrustedServerOpportunity
  trustedServerAuctionId?: string
}
```

The concrete implementation may use an equivalent internal representation,
but the following semantics are required:

- Storage is keyed by GPT slot object identity.
- Recording another source for the same pending intent adds independently
  timestamped evidence without discarding existing evidence or resetting its
  expiry.
- Re-recording the same source replaces that source's timestamp, generation,
  and metadata and restarts only that source's expiry. For Trusted Server, the
  new opportunity and optional auction ID replace the prior values; an absent
  auction ID clears a prior auction ID rather than carrying stale correlation.
  Re-recording remains idempotent for classification.
- Each intent has a monotonically increasing diagnostic-only ID.
- The intent is consumed once by the next matching `slotRequested`.
- Each source expires independently after the existing five-second request-path
  attribution window. Expiry removes only that source. The intent is removed
  when it is consumed or when its final source expires.
- Deferred source expiry is generation-safe and cannot delete a newer
  observation of that source or any other source.
- An expired or absent intent produces an `unattributed` request.

The public diagnostic API remains fail-closed and no-throw. It adds an observed
publisher-refresh method and allows the Trusted Server opportunity method to
carry an optional auction ID:

```ts
recordTrustedServerOpportunity(
  slot: GptDiagnosticsSlotHandle,
  auctionSlotId: string,
  opportunity: GptDiagnosticsTrustedServerOpportunity,
  trustedServerAuctionId?: string
): void;

recordPrebidRefresh(slots: GptDiagnosticsSlotHandle[]): void;
recordPublisherRefresh(slots: GptDiagnosticsSlotHandle[]): void;
```

A valid slot handle is any non-null object identity, matching the existing
WeakMap contract; GPT getter methods remain optional and throwing getters are
handled by the existing safe-read paths. Empty required identifiers and
unavailable diagnostics objects are ignored.

An optional auction ID is accepted only when it is a string whose trimmed value
is non-empty and no more than 256 UTF-8 bytes. The trimmed value is retained.
Invalid values are omitted without dropping the otherwise-valid Trusted Server
intent. The auction ID is opaque correlation data; no auction payload is
retained. The GPT `adInit` call site explicitly forwards `bid.hb_auction_id` to
the diagnostic opportunity method.

### Source classification

Extend the request-path union:

```ts
type GptDiagnosticsRequestPath =
  | 'trusted_server_direct'
  | 'prebid_refresh'
  | 'publisher_refresh'
  | 'competing'
  | 'unattributed'
```

Classification is based only on consumed intent sources:

| Observed sources             | Request path            |
| ---------------------------- | ----------------------- |
| Trusted Server only          | `trusted_server_direct` |
| Prebid only                  | `prebid_refresh`        |
| Publisher only               | `publisher_refresh`     |
| Any two or all three sources | `competing`             |
| None                         | `unattributed`          |

`publisher_refresh` means the request passed through the installed publisher
refresh boundary. It does not mean publisher code was the original caller if
another unobserved wrapper invoked that boundary.

### Wrapper ownership and behavioral identity

The late-slot-handoff refresh wrapper is the common boundary for refresh calls
that reach the currently installed GPT API. Before delegating an external call,
it records publisher intent for the concrete slots it will actually pass to its
wrapped function.

Prebid already wraps that boundary. To avoid labeling a Prebid-delivered
request as both publisher and Prebid merely because the wrappers are nested,
Prebid sets a synchronous, exception-safe diagnostic dispatch context only
while it delegates a refresh for which it also records Prebid intent. The
handoff wrapper skips publisher intent when that context is active. Trusted
Server's internal `adInit` refresh continues to use its existing internal
context and is likewise not marked as publisher initiated.

Prebid passthroughs that do not record Prebid evidence leave the dispatch
context inactive. This includes unresolved or invalid slot lists and paths with
no auction or delivery work. Those calls can therefore be observed as publisher
refreshes by the handoff boundary. Delivery-only refreshes, successful auction
completion, watchdog completion, and caught auction-failure completion pair
their Prebid marker and dispatch context around the same delegated call.

The context is metadata only. It must be restored in `finally`, must not escape
the synchronous refresh delegation, and must not influence slot selection,
targeting, callbacks, timeouts, or whether the wrapped refresh runs.

The required installation order is the existing production order: the GPT
late-handoff wrapper is installed first and the Prebid refresh wrapper captures
it later, so Prebid is the outer wrapper when publisher code calls the current
`pubads().refresh`. Installation code must preserve and test this invariant.
Both installers remain idempotent. If the invariant cannot be established, the
implementation must stop and revise this design rather than ship a known false
`competing` classification.

For a bare external `refresh()`, the handoff wrapper resolves the same concrete
slot list it already uses for suppression filtering and records only the slots
that are actually delegated. When no concrete slot list can be resolved, it
delegates exactly as before and records no intent.

If publisher code retained and invokes an older refresh reference that bypasses
the installed boundary, no publisher intent is observable. Its GPT request is
honestly retained as `unattributed`.

## Request-cycle Correlation Fields

Add the following optional exported fields:

```ts
interface GptDiagnosticsRequestCycle {
  requestIntentId?: number
  trustedServerAuctionId?: string
  opportunityToRequestMs?: number
  replacedRequestNumber?: number
  previousRenderToRequestMs?: number
  creativeChanged?: boolean
  previousCreativeId?: string | number
}
```

`requestIntentId` is present only when an observed intent was consumed.
`trustedServerAuctionId` and `opportunityToRequestMs` are present only when the
consumed intent included Trusted Server evidence. The duration is calculated
from the Trusted Server opportunity observation to `slotRequested`; invalid or
negative durations are omitted.

The type of creative IDs must match the existing GPT Ad Manager identity type.
If that type is narrower than the illustrative union above, reuse the existing
type rather than widening it.

## Replacement Detection

Replacement is evaluated when a current cycle receives a non-empty
`slotRenderEnded` callback. Search earlier retained cycles for the most recent
cycle that also has a non-empty render. If one exists:

- set `replacedRequestNumber` to that earlier cycle's request number;
- set `previousRenderToRequestMs` when the previous render and current request
  timestamps form a valid non-negative duration;
- copy the previous creative ID when GPT supplied it;
- set `creativeChanged` only when both cycles expose creative IDs.

This is an observed slot-render replacement relationship, not proof that the
pixels visibly changed. `slotContentChanged` remains GPT's independent statement
and is displayed alongside it. Empty current renders do not count as a rendered
replacement. If the earlier rendered cycle has already been evicted, no
replacement relationship is invented.

The diagnostics must not mutate an older cycle after it is evicted and must not
retain extra unbounded history solely for replacement detection.

## Overlay and Export

The overlay uses concise factual labels:

- `Request path: Publisher refresh`
- `Request intent: 7`
- `Trusted Server auction: <opaque ID>`
- `Opportunity → request 24 ms`
- `Replaced rendered request 1 after 6048 ms`
- `Creative changed 138563319574 → 138562551425`
- `Creative unchanged <ID>` when both IDs match

When either creative ID is unavailable, omit the creative comparison instead
of displaying `unknown changed`. Existing `slotContentChanged` output remains.
The JSON snapshot/export includes the new optional fields without changing its
activation, redaction, or bounded-retention behavior.

## Failure and Safety Behavior

- Diagnostics calls remain wrapped so failures cannot interrupt ad mapping or
  refresh delivery.
- Intent recording never calls GPT, Prebid, or publisher callbacks.
- Wrapper dispatch context is restored even when the delegated refresh throws.
- Marker expiry and notification scheduling remain bounded.
- Invalid auction IDs are omitted without dropping otherwise-valid TS intent.
- Per-source expiry removes no fresh or unrelated source evidence.
- Missing previous render facts produce absent replacement fields, not errors.
- No source classification is inferred after intent expiry.

## Test Strategy

### Store tests

- TS-only, Prebid-only, publisher-only, two-source, and three-source
  classification.
- Repeated same-source recording remains single-source.
- One-shot consumption, expiry, and generation-safe replacement.
- Independent per-source timestamps and expiry, including partial expiry of a
  multi-source intent.
- Repeated TS evidence replaces opportunity, timestamp, and auction-ID metadata
  and clears a stale auction ID when the replacement omits it.
- Intent IDs increase and correlate to the correct request.
- TS auction ID and opportunity-to-request duration propagation.
- Invalid optional auction ID omission.
- Replacement against the most recent earlier non-empty render.
- Empty current or previous render does not create a replacement relationship.
- Creative changed, unchanged, and one-sided/missing-ID cases.
- Evicted history does not create fabricated replacement facts.

### Integration tests

- External explicit and bare refreshes record publisher intent for exactly the
  delegated slots.
- Prebid delegation records Prebid intent but not nested publisher intent.
- Invalid/unresolved, empty-target, and fully excluded Prebid passthroughs leave
  dispatch context inactive and remain publisher-observable.
- Delivery-only, auction-completion, watchdog, and caught-failure completion
  paths pair Prebid intent and context around exactly one delegation.
- `adInit` internal refresh records TS intent but not publisher intent.
- Context cleanup occurs when delegated refresh throws.
- The real installer sequence establishes handoff-inside-Prebid ordering, and
  repeated installer calls preserve that order without double wrapping.
- Existing slot filtering, suppression, argument forwarding, and call counts
  remain unchanged.

### API, overlay, and type tests

- Safe API forwarding and malformed-input no-op behavior.
- Exhaustive path-label coverage including publisher refresh.
- Conditional display of intent, auction, latency, and replacement facts.
- Snapshot/export shape includes only available optional facts.

## Acceptance Criteria

- A normal refresh passing through the installed publisher boundary is labeled
  `publisher_refresh`.
- A Prebid-delivered refresh remains `prebid_refresh`, not `competing`, solely
  because of wrapper nesting.
- A Trusted Server direct request reports its auction ID when available and its
  opportunity-to-request delay.
- A late filled request after an earlier filled render explicitly identifies
  the prior request and elapsed delay.
- Creative transitions are reported only from GPT-provided identifiers.
- Calls that bypass observed boundaries remain `unattributed`.
- Delegated slot-array identity and contents, `undefined` versus explicit bare
  refresh arguments, options-object identity, call count, and synchronous throw
  propagation are preserved exactly.
- Existing handoff suppression state transitions, Prebid auction/watchdog
  timing, targeting operations, and GPT/Prebid callback counts are unchanged;
  diagnostics adds no GPT or Prebid callback.
- The production installer sequence and idempotent reinstallation keep
  Prebid-controlled requests Prebid-only rather than false `competing`.
- All existing JS tests plus new store, API, GPT, Prebid, overlay, and type tests
  pass, followed by repository lint, formatting, build, and required Rust
  verification commands.
