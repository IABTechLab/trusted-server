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
Server candidate, a PUC creative request, a successfully posted creative response,
and a GPT slot load are separate steps in an evidence ladder. A creative response may
carry inline or cached markup, or a validated APS renderer descriptor.

This feature requires zero publisher-code changes. Activation remains the existing
server integration configuration plus `?ts_console=true`; it does not require new
publisher JavaScript, React, Next.js, DOM, or GAM configuration.

Refresh-source attribution and rendered-replacement diagnostics (below) are equally
observational. They do not suppress, delay, reorder, add, or remove GPT requests;
change targeting, slot selection, or auction timeouts; or add a GPT or Prebid
callback. Diagnostics calls are wrapped and fail closed — a missing or throwing
diagnostics implementation cannot interrupt refresh delivery.

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

Deployment configuration only makes the module available. Inactive browser sessions
receive no diagnostics module. When activated, the standalone content-hashed module
loads synchronously after the core bundle so it can install listeners before
publisher GPT request code. The standalone static response is cookie-independent and
remains publicly cacheable; active HTML responses are private and non-storeable.

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
https://publisher.example.com/article?ts_console=true
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

- The observed request path and direct Trusted Server opportunity.
- Requesting, Response received, Filled, Empty, or Rendered (fill unknown) GPT
  lifecycle state.
- The creative request and successful response timestamps as independent facts.
- Safe, deduplicated creative-bridge failure categories.
- Source-neutral GAM response class and identifiers reported by GPT.
- GPT slot-onload, impression-viewable, and visibility observations.
- Non-negative request-to-response, response-to-render, render-to-load, and
  render-to-viewable durations.
- Rendered size, backfill, and slot-content-change facts exposed by GPT.
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

| Request path            | Meaning                                                                                                                               |
| ----------------------- | ------------------------------------------------------------------------------------------------------------------------------------- |
| `trusted_server_direct` | Only the one-shot opportunity marker from `adInit` was consumed by the request.                                                       |
| `prebid_refresh`        | Only the installed Prebid refresh path marked the slots passed to its GPT refresh.                                                    |
| `publisher_refresh`     | Only the installed publisher refresh boundary marked the slots passed to `pubads().refresh`.                                          |
| `competing`             | Two or all three observed sources touched the slot before the request; targeting competition or overwrite is possible, but unproven.  |
| `unattributed`          | No observed source touched the slot before the request; diagnostics do not infer a path from timing, element IDs, or targeting names. |

Each of the three request-intent sources — Trusted Server direct, Prebid refresh, and
publisher refresh — lives for five seconds from its own observation and is consumed
once by the next matching `slotRequested`. Expiry is independent per source: one
source expiring does not remove another still-fresh source recorded for the same
slot, and expiry means that observation was too old to associate, not that the source
did or did not own a later request. Because documented GPT callbacks expose no
request token, `competing` is a warning about possible competition among the observed
sources, not a conclusion about which values were sent or selected.

For the direct path, `adInit` records one opportunity:

| Opportunity              | Meaning                                                                                                                                                                         |
| ------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `renderable_candidate`   | Bid targeting was applied with a non-empty ad ID and either a valid APS descriptor plus usable same-origin renderer endpoint, inline markup, or complete PBS Cache coordinates. |
| `unrenderable_candidate` | Bid targeting was applied, but the current bridge lacked the complete ID/render-source combination needed to send a creative response.                                          |
| `no_candidate`           | `adInit` explicitly observed no direct Trusted Server bid targeting for that configured slot.                                                                                   |

APS renderers use fail-closed precedence. When a bid contains `renderer`, the bridge
requires both a valid APS descriptor and a usable same-origin renderer endpoint. An
invalid descriptor or unavailable endpoint remains unrenderable even if legacy
`adm` or cache fields coexist. Only renderer-absent bids may use inline markup or
complete PBS Cache coordinates.

An absent opportunity is displayed as unknown. It must not be converted into a
negative demand-source conclusion.

### Publisher-boundary meaning

A `publisher_refresh` request path means the request passed through the installed
publisher refresh boundary — the wrapper Trusted Server installs around
`pubads().refresh` to observe refresh calls. It does not prove that publisher code
was the original caller: another unobserved wrapper sitting between publisher code
and that boundary could have invoked it. The label describes what the boundary
observed, not what code initiated the call.

### Nested Prebid attribution

Prebid's own refresh path wraps the same publisher refresh boundary. Without further
bookkeeping, a Prebid-delivered refresh would touch both the Prebid marker and the
publisher boundary and be misreported as `competing` merely because the wrappers are
nested — not because two independent sources actually contended for the slot.

To avoid that false positive, Prebid sets a synchronous, exception-safe dispatch
context only while it delegates a refresh for which it also records its own Prebid
intent. The publisher boundary skips recording publisher intent while that context is
active, so a Prebid-delivered refresh is correctly reported as `prebid_refresh`, not
`competing`, despite the wrappers being nested.

Prebid passthroughs that record no Prebid intent — for example, an invalid or fully
excluded slot list — leave the dispatch context inactive. Those calls still reach the
publisher boundary and can therefore be legitimately observed as publisher refreshes,
because no Prebid evidence was recorded for them.

### Retained old function references

If publisher code captured a reference to `pubads().refresh` before the Trusted
Server wrappers were installed, and later calls that retained reference directly, the
call bypasses every installed boundary. No publisher intent is observable in that
case, and the request is honestly reported as `unattributed` rather than guessed as
`publisher_refresh`. Diagnostics do not attempt to detect or compensate for this case.

### Trusted Server auction correlation

When the consumed intent includes Trusted Server evidence — for `trusted_server_direct`
or a `competing` cycle that includes it — the request cycle can carry two additional
facts:

- `trustedServerAuctionId`: the opaque auction identifier forwarded from the bid
  response (`bid.hb_auction_id`) when `adInit` recorded the opportunity.
- `opportunityToRequestMs`: the browser-observed interval from that Trusted Server
  opportunity to the GPT `slotRequested` callback that consumed it.

Both fields are informational correlation data, not proof of an auction outcome.
`trustedServerAuctionId` does not identify an auction winner, and diagnostics do not
infer a winner from targeting, timing, or this ID. `opportunityToRequestMs` is a
browser-observed interval only; no server processing timestamp is captured or
compared. Either field is omitted when Trusted Server evidence is absent or invalid,
or when the duration cannot be computed as non-negative.

Each consumed intent also carries a diagnostic-only, monotonically increasing
sequence number, shown in the overlay as `Request intent: <n>`. It orders
observations locally and carries no meaning beyond that.

For example, a direct request that consumed a fresh Trusted Server opportunity might
show:

```text
Request path: Trusted Server direct
Trusted Server auction: <opaque ID>
Opportunity → request 24 ms
```

A publisher-initiated refresh that consumed no Trusted Server evidence never shows an
auction ID or opportunity latency:

```text
Request path: Publisher refresh
```

## Trusted Server Evidence Ladder

The console keeps these observations independent and ordered:

1. `adInit` observed a direct opportunity and applied any candidate targeting.
2. A PUC `Prebid Request` passed the exact message-source, slot, and Trusted Server
   ad-ID ownership checks. This is selection evidence.
3. The bridge obtained inline or cached markup, or validated an APS renderer descriptor
   and endpoint, and `port.postMessage` returned without throwing. This confirms that a
   creative response was sent to the requesting PUC.
4. GPT later emitted `slotOnload` for the correlated slot.

Each step proves only itself. In particular, a successful response post does not prove
that the PUC consumed the response, and `slotOnload` is a GPT slot fact. Pixel-level
proof would require a controlled creative acknowledgement after the inner creative runs;
that acknowledgement is outside the zero-publisher-change design.

The derived `delivery` value uses these evidence-safe meanings:

| Delivery state                 | Panel wording                                                                                      |
| ------------------------------ | -------------------------------------------------------------------------------------------------- |
| `trusted_server_response_sent` | Trusted Server selected; creative response sent to PUC                                             |
| `trusted_server_selected`      | Trusted Server selected; no creative response confirmed                                            |
| `candidate_unconfirmed`        | Trusted Server candidate unconfirmed — another GAM result or a creative/bridge failure is possible |
| `no_candidate`                 | adInit observed no direct Trusted Server candidate for this request                                |
| `unknown`                      | Delivery status unknown — required GPT or direct-candidate evidence was not observed               |
| `pending`                      | Waiting for Trusted Server creative evidence                                                       |
| `not_applicable`               | No delivery conclusion is displayed before render or for an explicitly empty result.               |

For an explicit non-empty candidate, diagnostics wait five seconds from
`slotRenderEnded` for positive creative evidence. If no matched PUC request arrives,
the state becomes `candidate_unconfirmed`. Possible explanations include a different
GAM result, targeting overwrite, PUC configuration or ID mismatch, and bridge failure;
the missing request does not select among them. A late positive observation upgrades
the state.

### Creative-bridge failures

A matched creative attempt can report these safe, non-terminal categories:

- `missing_render_source`
- `cache_fetch_failed`
- `invalid_cache_payload`
- `response_post_failed`

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

## Rendered Replacement

When a request cycle receives a non-empty `slotRenderEnded`, diagnostics search the
same GPT slot's retained cycles for the most recent earlier cycle that also rendered
non-empty. If one is found, the current cycle records:

- `replacedRequestNumber`: the earlier cycle's request number.
- `previousRenderToRequestMs`: the interval from that earlier render to the current
  request, present only when the two timestamps form a valid non-negative duration.
- `previousCreativeId`: the earlier cycle's GAM creative ID, when GPT exposed one.
- `creativeChanged`: set only when both cycles expose a creative ID, comparing the
  previous and current values.

This is an **observed slot-render replacement relationship**, not proof that the
rendered pixels visibly changed inside the creative iframe. GPT's own
`slotContentChanged` observation remains an independent statement and is displayed
alongside the replacement facts rather than folded into them.

Creative transitions are reported only from GPT-provided identifiers. When either the
previous or current cycle lacks a creative ID, diagnostics omit the creative
comparison instead of guessing a changed or unchanged state. If the earlier rendered
cycle has already been evicted by the ten-cycles-per-slot retention limit, no
replacement relationship is invented for the current cycle — the fields are simply
absent.

Replacement detection runs independently of request-path classification: a
`trusted_server_direct`, `prebid_refresh`, `publisher_refresh`, `competing`, or
`unattributed` cycle can each report a replacement when the retained-history
conditions are met.

For example, a later request that replaced an earlier rendered cycle might show:

```text
Replaced rendered request 1 after 6048 ms
Creative changed 1000000001 → 1000000002
```

or, when both creative IDs match:

```text
Replaced rendered request 1 after 6048 ms
Creative unchanged 1000000001
```

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

Missing elements and duplicate DOM or GPT slot IDs remain visible in the panel as
Unbound or Ambiguous and receive no badge. If DOM uniqueness cannot be verified
because selector support is unavailable or throws, the export reports
`dom_uniqueness_unverifiable`. Framework replacement of an element with a new unique
element using the same exact ID is rebound automatically.

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

When active, the integration exposes a read-only operator API:

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
- Request path, opportunity, creative-progress timestamps, and safe failure enums.
- Source-neutral GAM identifiers and response classes.
- Non-negative derived durations only.
- The opaque Trusted Server auction-correlation ID and rendered-replacement facts
  (replaced request number, latency, and creative-ID transition), when present.
- Separate callback issues, attribution issues, coverage counters, and retention
  counters.

It does not contain raw targeting, per-bid identifiers, bid prices, bidder identity,
creative markup, cache URLs, cache payloads, cache or bridge error details, cookies,
user identifiers, query strings, or URL fragments. The Trusted Server auction ID
above is the one opaque correlation string forwarded from the bid response; it is
retained as-is and is not itself a bid price, bidder identity, or targeting value.

Captured records are memory-only. Diagnostics do not add an upload, diagnostics
network request, `localStorage`, `sessionStorage`, IndexedDB, or other persistence.
`export()` creates only the user-requested local JSON download; it sends nothing to a
server. The `__Host-ts-console` session cookie contains only the activation bit and is
inaccessible to JavaScript.

## Timing and Retention Bounds

- Direct, Prebid, and publisher request-intent sources: five seconds each,
  one-shot per intent, generation-safe, with independent per-source expiry.
- Retained request cycles bound rendered-replacement lookups: an evicted earlier
  render cannot be matched as a replacement source.
- Delivery observation after `slotRenderEnded`: five seconds.
- Creative-attempt mutation lifetime: 30 seconds from the first matched request.
- Retained GPT slot objects: 64.
- Retained request cycles per slot: 10.
- Retained callback issues: 128.
- Retained auction-slot-to-GPT-slot associations: 64.
- Retained creative attempts, including status tombstones: 128.
- Retained attribution issues: 128.

The least-recently-active slot is evicted when the slot bound is exceeded. An evicted
GPT slot can re-enter retention only after a future `slotRequested`; request numbers
remain monotonic. The oldest request cycle or issue is removed at its own bound.
Export metadata reports `evictedSlots`, `evictedRequestCycles`, `droppedCallbacks`,
and `droppedAttributionIssues`.

Timers schedule only diagnostics cleanup or presentation notification. They never
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

## Limits

The integration observes six documented PubAdsService events and the existing
Trusted Server `adInit`, Prebid refresh, publisher refresh, and creative-bridge
boundaries. It does not patch arbitrary publisher `display()` or `refresh()` calls,
inspect GPT network payloads, or identify demand ownership from GAM IDs. It cannot
prove inner-iframe execution or visual correctness without a controlled creative
acknowledgement, cannot see past a retained pre-installation function reference, and
never infers an auction winner from a correlated auction ID, targeting, or timing.

## Related

- [Google Publisher Tags Integration](./gpt.md)
- [Integrations Overview](/guide/integrations-overview)
- [Ad Serving](/guide/ad-serving)
