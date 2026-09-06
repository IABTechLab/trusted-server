# GPT Runtime Diagnostics

**Category**: Ad Serving

**Status**: Development
**Type**: Local browser diagnostics

## Overview

GPT Runtime Diagnostics is an opt-in browser console for documented Google
Publisher Tag (GPT) lifecycle callbacks and Trusted Server integration evidence.
It groups observations into per-slot request cycles, shows timings and source-neutral
GAM facts, binds slots to exact DOM elements, and downloads the same allowlisted data
as versioned JSON.

The console reports positive observations, not inferred ownership. A filled result
means only that GPT emitted `slotRenderEnded` with `isEmpty === false`. A Trusted
Server candidate, a PUC markup request, a successfully posted markup response, and a
GPT slot load are separate steps in an evidence ladder.

This feature requires zero publisher-code changes. Activation remains the existing
server integration configuration plus `?ts_console=1`; it does not require new
publisher JavaScript, React, Next.js, DOM, or GAM configuration.

The diagnostics integration is independent of the
[GPT first-party script integration](./gpt.md). Either integration can be enabled
without the other, although Trusted Server creative-progress evidence is available
only for slots served through the existing GPT integration.

## Deployment Configuration

The module is unavailable unless explicitly enabled for the deployment:

```toml
[integrations.gpt_diagnostics]
enabled = true
```

Deployment configuration makes the module available; it does not activate any browser
session. Inactive browser sessions receive no diagnostics module. When activated, the
standalone content-hashed module loads synchronously after the core bundle so it can
install listeners before publisher GPT request code. The standalone static response is
cookie-independent and remains publicly cacheable; active HTML responses are private
and non-storeable.

### Auction correlation token

Enabling the integration has one further server-side effect, beyond module
availability, that does not depend on browser activation. For each server-side auction
that produced winning bids, Trusted Server mints a fresh correlation token and publishes
it as `hb_auction_id` on each winning bid in `window.tsjs.bids`:

```text
ts-auc-2f8c1d5a4b7e4c0f9a3d6b1e8c5f2a7d
```

- The token is generated per auction from a random UUID. It is not derived from the Edge
  Cookie ID, the auction request ID, or any other visitor identifier, and it does not
  repeat across auctions.
- It is **not** a GAM targeting key. Of the Trusted Server bid fields, only `hb_pb`,
  `hb_bidder`, `hb_adid`, `hb_cache_host`, `hb_cache_path`, and `ts_initial` are applied
  as slot targeting alongside the slot's configured targeting, so the token never enters
  the ad request.
- It is absent when the integration is disabled, and absent for any auction that
  produced no winning bids.
- It is published on every document whose auction produced winning bids, including
  documents with no active console session, because the console reads it from the same
  page bid state the GPT integration already consumes.

## Activate or Deactivate a Browser Session

Open a page with one of these exact, case-sensitive query directives:

| Directive          | Effect                          |
| ------------------ | ------------------------------- |
| `ts_console=1`     | Activate this browser session   |
| `ts_console=true`  | Activate this browser session   |
| `ts_console=0`     | Deactivate this browser session |
| `ts_console=false` | Deactivate this browser session |

For example:

```text
https://publisher.example.com/article?ts_console=1
```

Deactivate the same browser session with the matching directive:

```text
https://publisher.example.com/article?ts_console=0
```

An exact directive establishes or clears the host-only, `Secure`, `HttpOnly`,
`SameSite=Lax` `__Host-ts-console` session cookie. The server removes every reserved
`ts_console` pair before origin, cookie, or auction handling, and the response removes
the directive from the visible URL while preserving the path, unrelated query pairs,
and fragment. Activation applies to the same origin across tabs until the browser
session ends or an exact deactivation directive clears it.

Duplicate directives, unrecognized values, and duplicate activation cookies fail
closed for the current response. Active and directive-bearing HTML responses use
`Cache-Control: private, no-store` and omit surrogate cache headers. The cookie is
never forwarded to the publisher origin and is unrelated to `ts-tester`.

## What the Console Shows

The panel opens expanded after document startup and provides filters for All,
Visible, Filled, Empty, Pending/Incomplete, and Unbound/Ambiguous slots.

Each request cycle can show:

- The observed request path, request-intent ID, and direct Trusted Server opportunity.
- Opaque Trusted Server auction-ID correlation and opportunity-to-request latency when available.
- Observed replacement of an earlier retained filled render, including GPT creative-ID transitions.
- Requesting, Response received, Filled, Empty, or Rendered (fill unknown) GPT
  lifecycle state.
- The creative request and successful response timestamps as independent facts.
- Safe, deduplicated creative-bridge failure categories.
- Source-neutral GAM response class and identifiers reported by GPT.
- GPT slot-onload, impression-viewable, and visibility observations.
- Non-negative request-to-response, response-to-render, render-to-load, and
  render-to-viewable durations.
- GPT-reported rendered size, a separately labelled observed outer slot box when safely bound, backfill, and slot-content-change facts.
- Current DOM binding status and viewport intersection.

Elapsed time alone never changes a pending GPT request to Incomplete. Incomplete
sequence appears only when an observed callback proves a missing or invalid earlier
step. When `slotRenderEnded` omits `isEmpty`, the result stays Rendered (fill unknown),
and `responseClass` remains absent. `unclassified_non_empty` requires an explicit
`isEmpty === false` observation.

## Request Paths

Request-path labels describe integration paths observed immediately before one GPT
`slotRequested` callback. They do not identify a bidder winner or the owner of the
actual network request.

| Request path            | Meaning                                                                                                          |
| ----------------------- | ---------------------------------------------------------------------------------------------------------------- |
| `trusted_server_direct` | Only the `adInit` observation was consumed by the request.                                                       |
| `prebid_refresh`        | Only the installed Prebid refresh path was observed, including a `pubads.refresh` Prebid consumed and delegated. |
| `publisher_refresh`     | The request crossed the `pubads.refresh` boundary without Prebid consuming it.                                   |
| `competing`             | Two or more observed paths contributed evidence; competition or overwrite is possible, but unproven.             |
| `unattributed`          | No observed intent was consumed; diagnostics do not infer a path from timing, element IDs, or targeting names.   |

Each source in a per-slot request intent lives for five seconds and is consumed once.
Sources expire independently: re-observing one source cannot extend another source's
window. Their expiry means the observation was too old to associate, not that either
path did or did not own a later request. Because documented GPT callbacks expose no request token,
`competing` is a warning about possible competition, not a conclusion about which
values were sent or selected.

The publisher-refresh observer delegates exactly once with the original receiver,
arguments, result, and synchronous throw. A `refresh()` call that omits its slot
list, or passes `null` or `undefined` for it, refreshes every slot; the observer
reads GPT's current slot list for diagnostics only. A stale refresh function
reference captured before installation bypasses that boundary and remains
`unattributed`. Prebid and the Trusted Server `adInit` refresh each set a scoped,
synchronous diagnostics context while delegating their own refresh, so a nested
`pubads.refresh` is not mislabeled `publisher_refresh` or `competing`. Both clear that
context even when the delegated refresh throws, and the Prebid wrapper restores the
exact prior value. Diagnostics never suppresses or changes a GPT request.

For a direct observation, the optional opaque auction ID is retained only after
trimming to a non-empty value no longer than 256 UTF-8 bytes. No auction payload,
targeting map, bid price, markup, network body, or stack trace is exported. The
reported opportunity-to-request duration is browser-observed only and is omitted for
invalid or negative timing.

When a non-empty render follows a later request for the same retained GPT slot, the
console can report the most recent earlier non-empty render it replaces. This is an
observed callback relationship, not proof that pixels changed. GPT-provided creative
IDs are compared only when both cycles provide IDs; `slotContentChanged` remains a
separate GPT fact.

For the direct path, `adInit` records one opportunity:

| Opportunity              | Meaning                                                                                                                    |
| ------------------------ | -------------------------------------------------------------------------------------------------------------------------- |
| `renderable_candidate`   | Bid targeting was applied with a non-empty ad ID and inline markup or complete PBS Cache coordinates.                      |
| `unrenderable_candidate` | Bid targeting was applied, but the current bridge lacked the complete ID/render-source combination needed to serve markup. |
| `no_candidate`           | `adInit` explicitly observed no direct Trusted Server bid targeting for that configured slot.                              |

An absent opportunity is displayed as unknown. It must not be converted into a
negative demand-source conclusion.

## Trusted Server Evidence Ladder

The console keeps these observations independent and ordered:

1. `adInit` observed a direct opportunity and applied any candidate targeting.
2. A PUC `Prebid Request` passed the exact message-source, slot, and Trusted Server
   ad-ID ownership checks. This is selection evidence.
3. The bridge obtained markup and `port.postMessage` returned without throwing. This
   confirms that a markup response was sent to the requesting PUC.
4. GPT later emitted `slotOnload` for the correlated slot.

Each step proves only itself. In particular, a successful response post does not prove
that the PUC consumed the response, and `slotOnload` is a GPT slot fact. Pixel-level
proof would require a controlled creative acknowledgement after the inner markup runs;
that acknowledgement is outside the zero-publisher-change design.

The derived `delivery` value uses these evidence-safe meanings:

| Delivery state                 | Panel wording                                                                                                                                               |
| ------------------------------ | ----------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `trusted_server_response_sent` | Trusted Server selected; markup response sent to PUC                                                                                                        |
| `trusted_server_selected`      | Trusted Server selected; no markup response confirmed                                                                                                       |
| `candidate_unconfirmed`        | Trusted Server candidate unconfirmed — another GAM result or a creative/bridge failure is possible                                                          |
| `no_candidate`                 | adInit observed no direct Trusted Server candidate for this request                                                                                         |
| `unknown`                      | Delivery status unknown — required GPT or direct-candidate evidence was not observed                                                                        |
| `pending`                      | Waiting for Trusted Server creative evidence                                                                                                                |
| `not_applicable`               | No delivery conclusion is displayed before render or for an explicitly empty result, provided no Trusted Server creative evidence was stamped on the cycle. |

For an explicit non-empty candidate, diagnostics wait five seconds from
`slotRenderEnded` for positive creative evidence. If no matched PUC request arrives,
the state becomes `candidate_unconfirmed`. Possible explanations include a different
GAM result, targeting overwrite, PUC configuration or ID mismatch, and bridge failure;
the missing request does not select among them. A late positive observation inside the
30-second attempt window upgrades the state.

### Creative-bridge failures

A matched creative attempt can report these safe, non-terminal categories:

| Failure                 | Observed at the bridge                                                                         |
| ----------------------- | ---------------------------------------------------------------------------------------------- |
| `missing_render_source` | The bid carried neither inline markup nor a complete PBS Cache host and path.                  |
| `cache_fetch_failed`    | The PBS Cache fetch was rejected, failed while reading the body, or returned a non-OK status.  |
| `invalid_cache_payload` | The cache response was read but held no renderable creative, so nothing was posted.            |
| `response_post_failed`  | `port.postMessage` threw while posting markup, on either the inline or the cached-markup path. |

Failures are deduplicated and retain first-observed order. Detailed URLs, cache IDs,
payloads, markup, and error objects remain only in existing operational logging and do
not enter diagnostics.

### Source-neutral GAM facts

`slotRenderEnded` may expose line item, order, advertiser, creative, yield-group, and
company IDs. The console displays those values as GAM-reported identifiers only; it
does not map an ID to Trusted Server, Prebid, or any other demand source. Response
classes are similarly limited to GPT facts:

| Response class           | Meaning                                                       |
| ------------------------ | ------------------------------------------------------------- |
| `empty`                  | GPT explicitly reported an empty result.                      |
| `backfill`               | GPT reported a non-empty backfill result.                     |
| `reservation`            | GPT reported a non-empty result with reservation identifiers. |
| `unclassified_non_empty` | GPT explicitly reported non-empty without classifying IDs.    |

GPT populates its source-agnostic line item and creative IDs for reservation and
line-item backfill alike, so they classify as `reservation` only when GPT also
reported the render as explicitly non-backfill. On their own they remain
`unclassified_non_empty` rather than becoming an unsupported conclusion.

Identifiers are retained only as positive whole numbers, and the yield-group and company
lists keep at most eight IDs each. GPT reports these fields only for reservation and
backfill ads served by PubAdsService, so an absent identifier is a fact about the render
rather than a gap in observation.

## Attribution Issues and Callback Coverage

Creative-correlation problems are exported separately from GPT callback issues. The
eight attribution issue reasons are:

- `creative_request_without_slot`
- `creative_request_without_cycle`
- `creative_request_ambiguous_cycle`
- `creative_request_on_empty_cycle`
- `creative_attempt_capacity`
- `creative_attempt_unknown`
- `creative_attempt_expired`
- `creative_attempt_evicted`

The panel summary reports slot, callback-issue, and attribution-issue counts
separately. Attribution issues never increment callback coverage.

Coverage remains independent for each documented GPT callback:

```text
observed = matched + unmatched + ambiguous
```

Unmatched callbacks have no compatible retained request cycle. Ambiguous callbacks
have more than one compatible cycle, such as overlapping refreshes. A uniquely
correlated out-of-order callback remains matched and also records an
`invalid_event_order` callback issue. Coverage describes callback correlation, not
fill rate or revenue.

A unique response-bearing cycle can observe `slotOnload` before
`slotRenderEnded`. Diagnostics records `loadObservedBeforeRender`, deliberately omits
`renderToLoadMs`, and does not mark that normal ordering incomplete or invalid.
Overlapping response-bearing candidates remain ambiguous.

## Correlation, Slot Binding, and Badges

GPT slot object identity is the only correlation key. Exact DOM element IDs are used
only to bind a retained GPT slot to its current element. Dynamic Next.js suffixes and
configuration prefixes such as `ad-fixed_bottom-0` are display facts, not correlation
keys, and diagnostics never use prefix matching to join request cycles.

A binding is valid only when one connected DOM element has the exact GPT slot element
ID and one retained GPT slot claims that ID. Prefixes, container IDs, and
likely-looking elements are never guessed.

A concise viewport badge appears only when a slot:

- Has at least one observed request.
- Has a unique, connected exact binding.
- Has a non-zero rectangle intersecting the viewport.

A badge summarizes the slot's most recent request cycle: the GPT result (Filled, Empty,
Rendered (fill unknown), or Pending), a short delivery label, a `Competing paths`
marker when the request path is `competing`, the rendered size, and the request-to-
response, response-to-render, and render-to-viewable durations that are available. It
adds `Incomplete sequence` when a callback proved a missing or invalid earlier step.

Badge delivery labels are the same derived states the panel and export report, shortened
to fit:

| Delivery state                 | Badge label             |
| ------------------------------ | ----------------------- |
| `trusted_server_response_sent` | TS response sent        |
| `trusted_server_selected`      | TS selected             |
| `pending`                      | TS candidate (pending)  |
| `candidate_unconfirmed`        | TS unconfirmed          |
| `no_candidate`                 | No TS candidate         |
| `unknown`                      | Delivery unknown        |
| `not_applicable`               | No delivery label shown |

The badge never re-derives delivery from raw timestamps; it labels the state the store
already resolved, so a badge cannot disagree with the panel or the export.

Missing elements and duplicate DOM or GPT slot IDs remain visible in the panel as
Unbound or Ambiguous and receive no badge. If DOM uniqueness cannot be verified
because selector support is unavailable or throws, the export reports
`dom_uniqueness_unverifiable`. Framework replacement of an element with a new unique
element using the same exact ID is rebound automatically.

When Trusted Server associates a GPT slot with its next request, diagnostics retains
`requestedSlotSizes`: the configured `AuctionSlot.formats` list Trusted Server supplied
to GPT when it defined the slot. A reused slot keeps those definition-time formats even
if a later SPA auction has different configured formats, because GPT still holds the
original sizes. The retained value is a bounded validated copy of the complete
configured list, not an inferred responsive size or a claim about the final selected
size. It is omitted for publisher and otherwise unknown request paths where Trusted
Server did not supply formats.

For an explicitly filled render, diagnostics can also retain `observedSlotSize`: the
most recently sampled outer CSS box of the uniquely bound, connected slot element.
This is measured after `slotRenderEnded`. When `ResizeObserver` is available, it is
updated while that same request cycle is latest for the GPT slot. If the element later
becomes unbound or ambiguous, the last successful sample remains as request-cycle
evidence while the binding status reports the current DOM state. It is displayed
separately from `size`, which remains the exact GPT-reported `slotRenderEnded.size`
fill-size fact. The panel labels the three separate facts as requested slot sizes,
GPT-reported fill size, and observed outer slot box; the badge abbreviates them as
`Req`, `Fill`, and `Box`. The observed box may differ from GPT's reported size (for
example, a flexible APS creative can report `1×1` while its allocated outer slot box
is larger). The measurement describes publisher-page layout, not universal internal
creative-pixel dimensions. A collapsed or hidden bound element can report `0×0`, which records the
page layout state rather than an invalid measurement. Empty, unbound, missing, or
ambiguous slots do not report an observed box; delayed measurements from an older cycle
are rejected after a refresh.

Cross-origin and SafeFrame boundaries prevent diagnostics from inspecting iframe
content or altering the APS sandbox, so this field cannot prove the inner creative's
pixels.

Badges and the panel live in a closed Shadow DOM. Diagnostics do not add attributes,
classes, or inline styles to publisher slot elements.

## Presentation Lifecycle

- **Collapse** reduces the panel while preserving capture.
- **Close** dismisses the presentation for the current document.
- External removal by hydration or DOM reconciliation triggers a debounced remount.
- Live re-renders preserve open request-history disclosures and panel scroll position.
- Explicit Close or `hide()` prevents remount until `show()` is called.
- Capture continues while the panel is hidden.

The visual host mounts only after the document is complete and two animation frames
have elapsed. GPT callback capture and integration evidence can begin earlier.

## Browser API

When active, the integration exposes a read-only operator API. It has exactly the
five methods below; the evidence writers Trusted Server's own integration modules
use live on a separate internal channel (`window.tsjs.gptDiagnosticsRecorder`) that
is not part of this contract and is not supported for operator use.

```js
const diagnostics = window.tsjs.gptDiagnostics

diagnostics.snapshot()
diagnostics.export()
const unsubscribe = diagnostics.subscribe((snapshot) => {
  console.log(snapshot.version, snapshot.slots.length)
})
diagnostics.hide()
diagnostics.show()
unsubscribe()
```

| Method                | Semantics                                                                                            |
| --------------------- | ---------------------------------------------------------------------------------------------------- |
| `snapshot()`          | Returns a fresh V1 snapshot from current store and binding facts                                     |
| `export()`            | Downloads the current V1 snapshot as local JSON; no upload occurs                                    |
| `subscribe(listener)` | Delivers fresh snapshots after coalesced data or binding changes and returns an unsubscribe function |
| `hide()`              | Dismisses presentation without stopping capture                                                      |
| `show()`              | Clears dismissal and remounts presentation without resetting data                                    |

## V1 Export, Storage, and Privacy

The allowlisted export contains:

- `version: 1` and an ISO `capturedAt` timestamp.
- Current page origin and pathname, excluding query parameters and fragments.
- Retained slots, binding facts, visibility, and request cycles.
- `requestedSlotSizes` when Trusted Server supplied configured formats for that exact
  request, plus GPT-reported fill `size` and an optional observed outer `observedSlotSize`.
- Request path, request intent ID, opportunity, creative-progress timestamps, and
  safe failure enums.
- The per-auction diagnostics token (`trustedServerAuctionId`) and the
  opportunity-to-request duration, when a direct opportunity was observed.
- Replacement facts for a re-rendered slot: `replacedRequestNumber`,
  `previousRenderToRequestMs`, `previousCreativeId`, and `creativeChanged`.
- The derived `delivery` state, `responseClass`, and `loadObservedBeforeRender`.
- Source-neutral GAM identifiers and response classes.
- Non-negative derived durations only.
- Separate callback issues, attribution issues, coverage counters, and retention
  counters.

It does not contain raw targeting, bid IDs, bid prices, bidder identity, creative
markup, cache URLs, cache payloads, cache or bridge error details, cookies, user
identifiers, query strings, or URL fragments. The exported `trustedServerAuctionId`
is the `hb_auction_id` value described in
[Auction correlation token](#auction-correlation-token): minted fresh for each
server-side auction, not derived from the Edge Cookie ID or any other visitor
identifier, and never repeated across auctions, so it cannot be joined back to a
visitor. Diagnostics retain it only after trimming to a non-empty value of at most
256 UTF-8 bytes.

Captured records are memory-only. Diagnostics do not add an upload, diagnostics
network request, `localStorage`, `sessionStorage`, IndexedDB, or other persistence.
`export()` creates only the user-requested local JSON download; it sends nothing to a
server. The `__Host-ts-console` session cookie contains only the activation bit and is
inaccessible to JavaScript.

## Timing and Retention Bounds

- Direct, Prebid, and publisher request-path markers: five seconds, one-shot.
  Expiry is evaluated when the slot is next recorded or requested, so retained
  intents hold no timer.
- Delivery observation after `slotRenderEnded`: five seconds.
- Creative-attempt mutation lifetime: 30 seconds from the first matched request.
- Retained GPT slot objects: 64.
- Retained request cycles per slot: 10.
- Retained callback issues: 128.
- Retained auction-slot-to-GPT-slot associations: 64.
- Requested slot sizes per correlated request: the first 16 configured entries, with invalid entries dropped.
- Retained creative attempts, including status tombstones: 128.
- Retained attribution issues: 128.

The least-recently-active slot is evicted when the slot bound is exceeded. An evicted
GPT slot can re-enter retention only after a future `slotRequested`; request numbers
remain monotonic. The oldest request cycle or issue is removed at its own bound.
Export metadata reports `evictedSlots`, `evictedRequestCycles`, `droppedCallbacks`,
and `droppedAttributionIssues`.

Timers schedule only presentation notification, and at most one is outstanding: the
delivery-evidence boundary re-arms itself from retained cycles instead of queueing
one callback per render, so a refresh burst cannot grow the timer queue. They never
trigger GPT or Prebid work, gate an auction, or delay delivery.

## Troubleshooting

### The API or panel is absent

1. Confirm `[integrations.gpt_diagnostics]` is enabled in the deployed configuration.
2. Activate the browser session with an exact recognized `ts_console` value.
3. Confirm the Trusted Server script bundle loaded successfully.
4. Use `ts_console=false` and then `ts_console=true` on a new document to reset
   browser-session activation explicitly.
5. If the API exists but the panel does not mount, check for a publisher element using
   the reserved ID `trusted-server-gpt-diagnostics`; rename or remove that element and
   reload.

### The panel says Waiting for GPT

GPT was not observed after listener installation. Confirm GPT initializes and
executes queued `googletag.cmd` callbacks. Diagnostics do not create GPT, poll for it,
or patch publisher request behavior.

### Initial callbacks are missing

The integration can observe only callbacks emitted after its listeners execute.
Confirm the Trusted Server bundle precedes publisher GPT request code. The console
reports coverage gaps rather than reconstructing unobserved activity.

### A slot is Unbound

Confirm `slot.getSlotElementId()` returns a non-empty ID and a connected element with
that exact ID exists. Lazy or framework-created elements can bind later without losing
request history.

### A slot has Ambiguous binding

Remove duplicate DOM IDs or ensure only one retained GPT Slot object claims the ID.
Diagnostics intentionally do not choose one candidate.

### Callbacks are Ambiguous

Overlapping requests for the same GPT Slot object cannot be correlated safely because
documented callbacks do not expose a request-cycle identifier. Avoid overlap in
controlled tests, or use the issue record as evidence that correlation was not
possible.

### A refresh is `unattributed` or `competing`

`unattributed` means no request-path evidence was still eligible when GPT emitted
`slotRequested`. Each source's marker lives five seconds and is consumed once, so a
request more than five seconds after the observation, a refresh function reference the
publisher captured before installation, and any path Trusted Server does not observe
all stay `unattributed`. Diagnostics never fill that gap from timing, element IDs, or
targeting names.

`competing` means two or more sources contributed evidence for the same request. It is
a warning that competition or overwrite is possible, not a statement about which values
GPT sent. To narrow it in a controlled test, trigger one path at a time and leave more
than five seconds between refreshes.

### Delivery stays `candidate_unconfirmed`

The cycle rendered explicitly non-empty with a Trusted Server candidate, but no matched
creative markup request arrived within five seconds of `slotRenderEnded`. Read the
cycle's other facts before concluding anything:

- `responseClass` and the GAM identifiers show what Ad Manager reported delivering.
- A creative-bridge failure category on the same cycle shows the bridge was reached and
  failed.
- An attribution issue at the same time shows the request arrived but could not be
  correlated.
- No evidence at all is consistent with a different GAM result, a targeting overwrite,
  and a PUC configuration or ID mismatch alike.

A late positive observation within the 30-second attempt window — measured from the
cycle's GPT request, and only while that cycle is still retained — upgrades the state,
so re-read the panel rather than exporting immediately after render.

### Correlation evidence is missing

Attribution issues record why creative evidence could not be attached. They never
increment callback coverage and never create a delivery claim of their own, though
evidence already stamped on a cycle still resolves that cycle's delivery state:

| Reason                             | What was observed                                                                                                                                                                             |
| ---------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `creative_request_without_slot`    | The markup request carried no auction slot ID, or no retained association mapped it to a GPT slot.                                                                                            |
| `creative_request_without_cycle`   | The slot had no retained request cycle inside the 30-second attempt window, or its most recent cycle was already reported empty.                                                              |
| `creative_request_ambiguous_cycle` | An earlier non-empty cycle for the same slot was still in window before render, so no cycle was chosen.                                                                                       |
| `creative_request_on_empty_cycle`  | GPT later reported the matched cycle empty, so the attempt was dropped and cannot complete; selection or response evidence already stamped on that cycle still appears as its delivery state. |
| `creative_attempt_capacity`        | The 128-attempt bound was reached with every retained attempt still live.                                                                                                                     |
| `creative_attempt_unknown`         | A request, response, or failure referenced an attempt no longer retained.                                                                                                                     |
| `creative_attempt_expired`         | The attempt passed its 30-second lifetime before its response or failure was observed.                                                                                                        |
| `creative_attempt_evicted`         | The attempt's slot or request cycle was dropped first, by a retention bound or by GPT reporting that cycle empty.                                                                             |

Repeated issues on a busy page usually mean retention bounds, not delivery failure.
Reduce refresh overlap or capture a shorter session, then re-read the cycle.

## Limits

The integration observes six documented PubAdsService events, wraps
`pubads.refresh` once, and reads the existing Trusted Server `adInit`, Prebid
refresh, and creative-bridge boundaries. It does not patch publisher `display()`
calls, inspect GPT network payloads, or identify demand ownership from GAM IDs. A
refresh function reference the publisher captured before installation bypasses the
wrapper and stays `unattributed`. It cannot prove inner-iframe execution or visual
correctness without a controlled creative acknowledgement.

## Related

- [Google Publisher Tags Integration](./gpt.md)
- [Integrations Overview](/guide/integrations-overview)
- [Ad Serving](/guide/ad-serving)
