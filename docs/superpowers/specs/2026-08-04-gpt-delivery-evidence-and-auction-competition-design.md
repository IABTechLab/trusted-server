# GPT Delivery Evidence and Auction Competition

**Date:** 2026-08-04  
**Status:** Proposed  
**Scope:** Opt-in GPT diagnostics only; no publisher-code changes

## Relationship to the Existing Diagnostics Design

This design is a corrective extension to
`2026-07-28-gpt-runtime-diagnostics-overlay-design.md`. The existing overlay
correctly observes GPT lifecycle callbacks and Ad Manager identifiers. The
delivery-attribution branch adds Trusted Server candidate and creative-bridge
signals, but two of its conclusions are stronger than the evidence permits:

- A Prebid Universal Creative (PUC) markup request proves that the Trusted
  Server creative was selected and invoked. It does not prove that markup was
  returned or rendered.
- A filled slot that does not request markup does not prove that other demand
  won. It can also mean that the Trusted Server line item won but its creative,
  message, identifier, or render source failed.

This extension replaces those inferences with an evidence ladder and adds
request-path attribution at integration-owned refresh boundaries.

## Decision

Extend the existing browser-local GPT diagnostics with three independent
dimensions for every GPT request cycle:

1. **Request path:** whether the request followed the direct Trusted Server
   `adInit` path, the Prebid-managed refresh path, both paths before the same
   request, or an uninstrumented path.
2. **Ad Manager result:** GPT's empty/fill result and source-agnostic Ad Manager
   identifiers, as already observed.
3. **Trusted Server creative progress:** candidate applied, PUC markup request
   observed, markup response successfully posted, and GPT slot load observed.

The diagnostics must report only positive observations. In particular, the
absence of a PUC request after a short observation window becomes
`candidate_unconfirmed`, not `other_demand`.

The feature remains activated only by the existing server configuration plus
`?ts_console=true`. It changes neither auction timing nor delivery behavior and
requires no publisher JavaScript, React, Next.js, DOM, or GAM configuration
change.

## Goals

- Determine whether direct SSAT targeting was applied before a specific GPT
  request.
- Determine whether the existing Prebid refresh integration reached the same
  request and therefore may have competed with or replaced direct targeting.
- Determine whether GAM returned an empty or filled result and expose the GAM
  identifiers already supplied by GPT.
- Positively identify when the Trusted Server PUC asked for its markup.
- Separately identify when the bridge successfully posted a markup response.
- Preserve ambiguous and missing evidence instead of guessing a winner.
- Keep all diagnostics browser-local, bounded, opt-in, and safe to export.
- Preserve zero publisher-code change.

## Non-goals

- Pixel-level proof that an ad was visible or visually correct inside a
  cross-origin creative iframe.
- Proof that a particular GAM line item belongs to Trusted Server based only on
  its line-item ID. Diagnostics has no publisher-specific allowlist.
- Attribution of arbitrary publisher `display()` or `refresh()` calls that do
  not pass through an integration-owned boundary.
- Capturing bid prices, ad IDs, targeting values, creative markup, auction
  payloads, or network response bodies.
- Changing auction order, hydration behavior, GPT configuration, Prebid
  timeouts, or creative behavior.
- Adding a callback to publisher code or changing the publisher's PUC template.

## Evidence Boundaries

The UI and export must use the following meanings exactly.

| Observation                               | What it proves                                                                                       | What it does not prove                                             |
| ----------------------------------------- | ---------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------ |
| Direct opportunity recorded               | `adInit` observed whether server-auction targeting existed and, for a candidate, applied it          | GAM requested the slot or selected that targeting                  |
| Prebid refresh recorded                   | The installed Prebid refresh path invoked GPT for this slot                                          | Which bidder won the Prebid auction                                |
| Both paths recorded                       | Both integration paths touched the slot before one GPT request; competition or overwrite is possible | Which targeting values were present on the wire                    |
| `slotRenderEnded`, non-empty              | GAM filled the slot                                                                                  | Which demand source supplied the creative                          |
| GAM identifiers present                   | GPT reported those IDs for the filled result                                                         | That the IDs belong to Trusted Server without a configured mapping |
| Matched PUC `Prebid Request`              | A PUC under the correlated slot requested the exact Trusted Server bid ID                            | That the bridge returned markup or that the browser rendered it    |
| `Prebid Response` posted without throwing | The bridge submitted Trusted Server markup to the requesting MessagePort                             | That PUC consumed it or that pixels appeared                       |
| `slotOnload`                              | GPT reported that the slot's creative iframe loaded                                                  | Inner creative execution, viewability, or visual correctness       |
| No PUC request within the window          | No matching request was observed                                                                     | That other demand won                                              |

The final limitation is deliberate. Without either a publisher-specific GAM
line-item mapping or an acknowledgement emitted by the creative after it writes
the returned markup, negative winner attribution is not possible. Neither is
compatible with the zero-publisher-change requirement for this iteration.

## Request-path Model

Add a request-cycle field with these values:

```ts
type GptDiagnosticsRequestPath =
  | 'trusted_server_direct'
  | 'prebid_refresh'
  | 'competing'
  | 'unattributed'
```

- `trusted_server_direct`: only a one-shot opportunity marker from `adInit` was
  present when GPT emitted `slotRequested`.
- `prebid_refresh`: only a one-shot marker from the installed Prebid refresh
  path was present.
- `competing`: both markers were present. This is the diagnostic signal for the
  hydration/client-auction race: `adInit` observed the opportunity, but a
  Prebid-managed delivery also reached the slot before GPT opened the observed
  request cycle. If the opportunity is a candidate, targeting competition or
  overwrite is possible; the path alone does not prove it occurred.
- `unattributed`: neither marker was present. Diagnostics must not infer an
  owner from timing, element IDs, or targeting-key names.

These labels describe observed integration paths, not a bidder winner.

### Direct marker and observed opportunity

`adInit` records an opportunity for every configured slot it actually resolves
to a GPT slot. Unlike a positive-only candidate marker, this records an
explicit `no_candidate` observation when the server supplied no GAM bid
targeting. A real candidate is recorded whenever the server auction supplied
at least one non-empty `TS_BID_TARGETING_KEYS` field, not only when `hb_adid` is
present:

```ts
type GptDiagnosticsTrustedServerOpportunity =
  | 'renderable_candidate'
  | 'unrenderable_candidate'
  | 'no_candidate';

recordTrustedServerOpportunity(
  slot: GptDiagnosticsSlotHandle,
  auctionSlotId: string,
  opportunity: GptDiagnosticsTrustedServerOpportunity
): void;
```

`renderable_candidate` requires a non-empty `hb_adid` and either inline `adm`
or both PBS Cache coordinates. `unrenderable_candidate` means bid targeting was
applied but that condition was not met. These enums expose no bid data and let
diagnostics distinguish “adInit observed no direct bid” from “direct bid
applied, but the current bridge could not serve it.”

The direct-path marker carries the observed opportunity. It is consumed once by
`slotRequested`, using GPT slot object identity. Its resulting request-cycle
fact is:

```ts
trustedServerOpportunity?: GptDiagnosticsTrustedServerOpportunity;
```

The value means “observed by adInit before this request,” not “confirmed on the
network request.” A `competing` path explicitly warns that Prebid may have
replaced the targeting before GPT sent the request.

### Prebid marker

Add an internal diagnostics API method:

```ts
recordPrebidRefresh(slots: GptDiagnosticsSlotHandle[]): void;
```

The existing `installRefreshHandler` calls it immediately before
`originalRefresh` in the two paths that it controls:

- when a publisher Prebid auction is being delivered without a new auction;
- when the synthetic refresh auction completes or its watchdog falls back.

It must not mark:

- `adInit`'s `adInitRefreshInProgress` bypass;
- invalid or unresolved passthrough refreshes;
- an invocation that never reaches `originalRefresh`.

The store keeps only one-shot, object-identity markers. Both the direct marker
and the Prebid marker expire
`REQUEST_PATH_ATTRIBUTION_WINDOW_MS = 5_000` after they are recorded. Each
expiry callback includes the marker generation and may remove only that exact
generation, so it cannot erase a newer observation for the same slot. A marker
is otherwise consumed by the next matching `slotRequested`. Expiration prevents
a no-op display or refresh from attributing an unrelated later request. The
bounded auction-slot-to-GPT-slot association used by the creative bridge is
separate and is not removed when a request-path marker expires.

Because GPT exposes no request token, the UI describes request path as an
observed integration path, not an exact network-request owner. If a marker
expires before `slotRequested`, the cycle remains `unattributed` and its direct
opportunity is unknown; expiration never becomes negative evidence.

## Trusted Server Creative-progress Model

Replace the existing single “claim” timestamp with two timestamps and bounded,
deduplicated safe failure classifications:

```ts
trustedServerCreativeRequestAtMs?: number;
trustedServerCreativeResponseAtMs?: number;
trustedServerCreativeFailures?: GptDiagnosticsCreativeFailure[];
```

`GptDiagnosticsCreativeFailure` is the four-value union
`missing_render_source | cache_fetch_failed | invalid_cache_payload |
response_post_failed`. The array preserves first-observed order, contains each
enum at most once, and therefore can never contain more than four entries.

The bridge records the request only after all existing ownership checks pass:

- the message is `Prebid Request`;
- it contains an `adId` and a MessagePort;
- the message source resolves to one auction slot;
- that slot's current Trusted Server bid owns the exact `adId`.

The request-recording method returns an opaque, diagnostic-only attempt number:

```ts
recordTrustedServerCreativeRequest(auctionSlotId: string): number | undefined;
recordTrustedServerCreativeResponse(attemptId: number): void;
recordTrustedServerCreativeFailure(
  attemptId: number,
  reason: GptDiagnosticsCreativeFailure
): void;
```

The opaque attempt number ties an asynchronous cache response to the exact
request cycle that initiated it. It avoids attaching a late cache response to a
newer refresh of the same slot. Attempt records are bounded and contain no ad
identifier.

### Attempt correlation and lifecycle

There is at most one diagnostic creative attempt per GPT request cycle and at
most `MAX_CREATIVE_ATTEMPTS = 128` retained attempt records. The store uses the
following rules:

1. Resolve the auction-slot association, then inspect the retained GPT cycle
   with the greatest request number. Reuse of that cycle's existing valid
   attempt is checked first. For admission of a new attempt, a cycle is outside
   creative correlation once more than
   `CREATIVE_ATTEMPT_WINDOW_MS = 30_000` has elapsed since its `slotRequested`
   timestamp.
2. The greatest request cycle is compatible when it has rendered non-empty. It
   is provisionally compatible before `slotRenderEnded` only when no earlier
   retained non-empty cycle is still inside the 30-second window. This preserves
   an initial PUC request that races GPT's render callback, but rejects a request
   during the ambiguous interval between a refresh request and its render event.
   A known-empty cycle is never compatible.
3. The bridge's existing current-DOM iframe-source check and exact slot/ad-ID
   ownership check run before this algorithm. After a later cycle renders, an
   older cycle is not eligible merely because it is still retained.
4. One compatible cycle receives the first request timestamp and a new attempt
   ID. No compatible cycle or the ambiguous pre-render case produces an
   attribution issue and no attempt ID.
5. A duplicate or retry request while that greatest cycle remains current
   returns the same valid attempt ID and does not replace its first request
   timestamp.
6. Failures are observations, not terminal states. They are appended once to
   the bounded failure array. A later successful retry while the attempt ID is
   still valid can set the first response timestamp and upgrade delivery to
   `trusted_server_response_sent`.
7. Repeated response and failure calls for the same valid attempt are
   idempotent.
8. An attempt ID expires 30 seconds after its first request. Expiry invalidates
   future mutation through that ID but does not erase evidence already stored
   on the request cycle. Because there is only one attempt per cycle, a request
   after expiry records `creative_attempt_expired` and returns no replacement
   ID. A cycle that already has a response is complete and likewise returns no
   new attempt.
9. Evicting a request cycle immediately invalidates its attempt IDs. Late calls
   for expired, evicted, or unknown IDs add an attribution issue and never
   attach to another cycle.

Before admitting an attempt at the 128-record cap, the store removes the oldest
completed, expired, or evicted tombstone. It never evicts a live attempt to make
room. If all 128 records are live, admission records
`creative_attempt_capacity` and returns no attempt ID. A successful response
makes an attempt completed; repeat calls through a retained completed record are
idempotent no-ops.

When a pre-render request was provisionally attached, `slotRenderEnded` closes
the check. A non-empty event confirms compatibility. An empty event records an
`creative_request_on_empty_cycle` attribution issue; the positive request fact
is retained, but the inconsistent GPT sequence is visible to the tester.

The bridge records `CreativeResponse` only after `port.postMessage` returns
without throwing:

- immediately after the inline-`adm` response;
- immediately after the decoded PBS Cache response.

It records a safe failure classification when it positively observes a known
failure. Existing log messages retain the detailed operational context. The
diagnostics export contains only the enum, never URLs, cache IDs, markup, or
errors.

## Derived Delivery State

Replace the branch's `trusted_server` and `other_demand` conclusions with:

```ts
type GptDiagnosticsDelivery =
  | 'trusted_server_response_sent'
  | 'trusted_server_selected'
  | 'candidate_unconfirmed'
  | 'no_candidate'
  | 'unknown'
  | 'pending'
  | 'not_applicable'
```

Derive the state in this order:

1. `trusted_server_response_sent` when a creative response timestamp exists.
2. `trusted_server_selected` when a matched PUC request timestamp exists.
3. `not_applicable` before `slotRenderEnded` or for an explicitly empty result.
4. `unknown` when `slotRenderEnded` did not supply an explicit `isEmpty` value.
5. `no_candidate` for a non-empty result whose consumed opportunity is
   explicitly `no_candidate`.
6. `unknown` for a non-empty result with no consumed opportunity observation.
7. `pending` for a non-empty `renderable_candidate` or
   `unrenderable_candidate` still inside the observation window.
8. `candidate_unconfirmed` after that window expires.

`candidate_unconfirmed` means exactly: “Trusted Server applied a candidate,
GAM filled the slot, and no matching PUC request was observed.” Other GAM
demand, a targeting overwrite, a PUC configuration problem, an ID mismatch,
and a bridge problem remain possible explanations.

The delivery observation window is
`TRUSTED_SERVER_ATTRIBUTION_WINDOW_MS = 5_000` and starts at the cycle's
`slotRenderEnded` timestamp. A late positive signal always upgrades the state.
A timeout never blocks or changes ad delivery.

## Correlation and Ambiguity

All slot correlation continues to use GPT slot object identity. Dynamic Next.js
element suffixes and stable configuration prefixes are display-only facts and
must not become correlation keys.

The auction-slot-to-GPT-slot association established by `adInit` remains
bounded. A creative request is attached only to the greatest request-number
cycle when the lifecycle rules above make it compatible. Missing correlation or
the ambiguous pre-render interval creates an attribution issue and returns no
attempt number. The store never selects a cycle by nearest timestamp.

The response/failure methods accept only a valid attempt number created by the
request method. Unknown, expired, or evicted attempt numbers create a bounded
attribution issue and do not mutate a guessed cycle.

### Attribution issues

Creative and request-path problems are not GPT callbacks and must not be added
to `callbackIssues` or its coverage counters. Add a separate, optional v1
export collection:

```ts
interface GptDiagnosticsAttributionIssue {
  reason:
    | 'creative_request_without_slot'
    | 'creative_request_without_cycle'
    | 'creative_request_ambiguous_cycle'
    | 'creative_request_on_empty_cycle'
    | 'creative_attempt_capacity'
    | 'creative_attempt_unknown'
    | 'creative_attempt_expired'
    | 'creative_attempt_evicted'
  timestampMs: number
  runtimeSlotNumber?: number
  slotElementId?: string
}
```

The collection is capped at `MAX_ATTRIBUTION_ISSUES = 128`. Missing slot
identity remains absent rather than using runtime slot `0`. Export metadata
adds `droppedAttributionIssues`; marker expiry by itself is expected cleanup and
does not create an issue. Expired and evicted attempt IDs remain as bounded
status tombstones inside the same 128-record attempt cap, allowing a late call
to report the correct reason; an ID with no retained record is `unknown`.

## UI and Export

For each request cycle, the overlay and exported snapshot show:

- request path;
- the observed direct opportunity (`renderable_candidate`,
  `unrenderable_candidate`, `no_candidate`, or unknown);
- GPT response class and GAM identifiers;
- creative request/response timestamps or durations;
- safe, deduplicated bridge failure classifications, when observed;
- GPT load and viewability facts already captured;
- the derived delivery state.

Required user-facing wording:

| State                          | Wording                                                                                              |
| ------------------------------ | ---------------------------------------------------------------------------------------------------- |
| `trusted_server_response_sent` | “Trusted Server selected; markup response sent to PUC”                                               |
| `trusted_server_selected`      | “Trusted Server selected; no markup response confirmed”                                              |
| `candidate_unconfirmed`        | “Trusted Server candidate unconfirmed — another GAM result or a creative/bridge failure is possible” |
| `no_candidate`                 | “adInit observed no direct Trusted Server candidate for this request”                                |
| `unknown`                      | “Delivery status unknown — required GPT or direct-candidate evidence was not observed”               |
| `pending`                      | “Waiting for Trusted Server creative evidence”                                                       |
| `not_applicable`               | No delivery conclusion                                                                               |

If `slotOnload` is present with a Trusted Server response, the UI may append
“GPT slot onload observed.” It must not say “creative rendered,” “ad visible,”
or “pixels confirmed.”

Badges remain compact, but use the same evidence-safe meanings. The export
retains version 1 because these attribution fields are optional additions that
have not shipped from this branch; no existing version-1 field changes meaning
on the base branch.

## Privacy, Bounds, and Non-interference

- Diagnostics remains disabled unless both the integration configuration and
  `?ts_console=true` activate it.
- Every integration call uses optional chaining and is a no-op when diagnostics
  is inactive.
- No new global patch is introduced. The implementation adds observations only
  inside the existing `adInit`, Prebid refresh wrapper, GPT observer, and render
  bridge.
- Request-path markers use `WeakMap` slot identity, generation checks, and the
  fixed five-second lifetime.
- Creative attempts have a fixed maximum, a 30-second mutation lifetime, and
  are invalidated on request-cycle eviction.
- No raw targeting values, prices, bid IDs, markup, URLs, payloads, or error
  objects enter the store, overlay, badges, or export.
- Diagnostics does not add a network request, storage write, timer that gates
  delivery, or callback awaited by auction code.

## Failure Handling

- A diagnostics method throwing must never break auction or rendering code.
  The public diagnostics API catches internal failures and returns
  `undefined` for creative-attempt creation.
- A `postMessage` failure retains existing bridge behavior while recording
  `response_post_failed` when possible.
- Cache HTTP/network failure records `cache_fetch_failed`.
- A cache document without usable `adm` records `invalid_cache_payload`.
- A matched request with neither inline markup nor complete cache coordinates
  records `missing_render_source`.
- Missing or ambiguous correlation produces an attribution issue, not a GPT
  callback issue.
- Observation-window and marker-expiration timers only schedule diagnostic
  notification/cleanup and never trigger GPT or Prebid work.

## Testing Strategy

Implementation follows test-driven development.

### Store tests

- direct, Prebid, competing, and unattributed request paths;
- one-shot consumption, generation-safe replacement, and five-second expiry of
  both request-path markers;
- renderable, non-renderable, explicit-no-candidate, and unknown opportunities;
- selected, response-sent, pending, candidate-unconfirmed, no-candidate,
  unknown, and not-applicable delivery states;
- late positive evidence upgrades a timed-out state;
- opaque attempt IDs attach async responses only to their originating cycle;
- pre-render request, duplicate/retry, in-window failure-then-success,
  non-renewable 30-second expiry, and request-cycle eviction behavior;
- `slotRenderEnded` without `isEmpty` remains unknown unless positive creative
  evidence exists;
- unknown, expired, evicted, unmatched, inconsistent-empty, and ambiguous
  attempts create attribution issues without changing GPT callback coverage;
- attempt-cap admission removes an old tombstone but rejects a new attempt when
  all 128 records are live;
- no absence-only path emits an “other demand” conclusion;
- all collections and request cycles remain bounded.

### GPT/adInit and bridge tests

- candidates are recorded for incomplete-but-real server bid targeting;
- renderability is false without an ID or render source;
- inline response is recorded only after successful `postMessage`;
- cache response is recorded only after successful decode and `postMessage`;
- missing source, failed cache, invalid payload, and thrown `postMessage` record
  the safe failure enum;
- messages for another slot or bid produce no Trusted Server evidence;
- diagnostics absent or throwing does not change the bridge result.

### Prebid refresh tests

- publisher-delivery and completed synthetic refresh paths record their exact
  target slots immediately before `originalRefresh`;
- direct `adInit` bypass and invalid passthrough paths do not record Prebid;
- timeout and caught-auction fallback record one Prebid refresh, not two;
- mixed SRA refreshes mark all slots sent in the single GPT request;
- diagnostics absent or throwing does not drop the refresh.

### UI, API, and documentation tests

- overlay and badges use the required evidence-safe wording;
- snapshots export request path, progress, and GAM identifiers without raw bid
  data;
- no UI path claims that missing evidence proves other demand;
- diagnostics setup/cleanup remains idempotent.

## Acceptance Criteria

With diagnostics enabled, a tester can inspect one request cycle and answer:

1. Did the direct SSAT path apply a candidate?
2. Did a Prebid-managed refresh touch the same request cycle?
3. Did GAM fill the slot, and which identifiers did GPT report?
4. Did the correlated Trusted Server PUC request markup?
5. Did the bridge successfully post the markup response?
6. Did GPT later report slot load and viewability events?
7. If evidence stops, what is the last positively observed stage?

The tester cannot incorrectly conclude that other demand won merely because a
PUC request was absent, and no publisher code change is needed to gather the
evidence.

## Rollout

1. Land behind the existing diagnostics configuration and query-parameter
   activation; no new feature flag is required.
2. Deploy to a non-production or RC environment with diagnostics enabled.
3. Capture exports for a direct initial request, a hydration-era publisher
   auction, and a later refresh of the same slot.
4. Compare request paths and creative-progress stages across those cycles.
5. Use the evidence to decide whether the production fix belongs in auction
   ownership, creative delivery, or both. Do not use diagnostics alone to
   change GAM line-item priority.
