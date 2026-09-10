# GPT Diagnostics Label Dictionary

This dictionary defines the exact fixed labels and dynamic label prefixes in the GPT Runtime Diagnostics panel and badges. Text after a prefix such as `Server auction winner:` is the bounded observed value. The console reports observations; it does not identify the creative ultimately served unless a listed evidence source explicitly establishes that fact. Browser times use `performance.now()` and server times use the server request's `RequestTimings` clock. The clocks are never subtracted.

See [GPT Runtime Diagnostics](./gpt-diagnostics.md) for activation and operational details.

## Identity and controls

| Label                         | Badge        | Source                                               | Meaning and limits                                                                                                                     |
| ----------------------------- | ------------ | ---------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------- |
| `Ad #N`                       | `Ad #N`      | `runtimeSlotNumber`                                  | Stable number assigned to a GPT slot object in this page. It is not a creative ID.                                                     |
| `Request #M`                  | `Request #M` | `requestNumber`                                      | Monotonic request cycle for that slot, including refreshes. It does not imply fill.                                                    |
| `GPT-reported creative`       | —            | `adManager.creativeId` or `sourceAgnosticCreativeId` | GAM's callback identifier. It is separate from Ad/Request identity and does not identify a demand source.                              |
| `Filter`                      | —            | panel state                                          | Limits displayed rows. Activating a badge resets a hiding filter to `All`.                                                             |
| `Export JSON`                 | —            | `gptDiagnostics.export()`                            | Downloads the allowlisted V1 snapshot. It issues no ad request.                                                                        |
| `Label dictionary`            | —            | documentation link                                   | Opens this keyboard-accessible help page.                                                                                              |
| `How to read this evidence`   | —            | inline help                                          | Expands a concise explanation that winner, candidate, render, and timing observations have separate meanings and clocks.               |
| `Locate on page`              | —            | exact unique DOM binding                             | Scrolls only on activation and briefly draws a diagnostics-layer highlight. It never changes publisher attributes, classes, or styles. |
| `Collapse`, `Expand`, `Close` | —            | panel state                                          | Presentation controls only. `show()` can reopen a closed panel.                                                                        |
| `Request history`             | —            | retained cycles                                      | Earlier retained requests. At most ten cycles are retained per slot. An evicted selection is reported as no longer retained.           |
| `Technical details`           | —            | allowlisted snapshot fields                          | Expands correlation IDs, coverage, failures, and other non-summary evidence.                                                           |

The per-request panel groups are `Summary`, `Auction evidence`, `Delivery evidence`, `Timing`, and `Size and visibility`. Summary prefixes are `GPT result:` and `Observed auction path:`. Technical prefixes include `Ad unit`, `Request intent:`, `Trusted Server auction:`, and `Prebid auction:`. A fact appears in one detailed group; Technical details does not repeat those grouped facts.

Panel status is `GPT observed` or `Waiting for GPT`. Filter values are `All`, `Visible`, `Filled`, `Empty`, `Pending/Incomplete`, and `Unbound/Ambiguous`. Empty results are `No GPT slots observed yet.` or `No slots match.` The retained-cycle labels are `Initial request` and `Refresh N`; an evicted selection is announced as `Ad #N, Request #M is no longer retained.` The overview counts use the prefixes `slots`, `callback issues`, and `attribution issues`. Callback coverage uses the suffixes `observed`, `matched`, `unmatched`, and `ambiguous`.

## GPT lifecycle and delivery

| Label                                                                | Badge                     | Raw value / source                          | Meaning and what it does not prove                                                                                                                                                        |
| -------------------------------------------------------------------- | ------------------------- | ------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `Waiting for request`                                                | —                         | no request cycle                            | GPT has not emitted `slotRequested` for the slot.                                                                                                                                         |
| `Requesting`                                                         | `Pending`                 | `slotRequested`                             | A GPT request callback was observed. Elapsed time alone does not mean failure.                                                                                                            |
| `Response received`                                                  | —                         | `slotResponseReceived`                      | GPT emitted its response callback. It does not prove fill.                                                                                                                                |
| `GPT result: Filled` / `Filled`                                      | `Filled`                  | `slotRenderEnded.isEmpty === false`         | GPT reported non-empty. It does not prove which bidder or pixels were served.                                                                                                             |
| `GPT result: Empty` / `Empty`                                        | `Empty`                   | `slotRenderEnded.isEmpty === true`          | GPT explicitly reported empty.                                                                                                                                                            |
| `GPT result: Rendered (fill unknown)` / `Rendered (fill unknown)`    | `Rendered (fill unknown)` | `slotRenderEnded` without `isEmpty`         | Render callback observed; fill remains unavailable.                                                                                                                                       |
| `Creative markup sent; execution not confirmed`                      | `TS response sent`        | `delivery=trusted_server_response_sent`     | The bridge posted markup to PUC. It does not prove execution, visibility, or final served source.                                                                                         |
| `Server bid selected by the creative bridge; response not confirmed` | `TS selected`             | `delivery=trusted_server_selected`          | A matched PUC request selected the server bid; no successful response post was observed.                                                                                                  |
| `Server bid available; selection not confirmed`                      | `TS unconfirmed`          | `delivery=candidate_unconfirmed`            | A server candidate existed but no matched selection appeared in the observation window.                                                                                                   |
| `Waiting for Trusted Server creative evidence`                       | `TS candidate (pending)`  | `delivery=pending`                          | The five-second positive-evidence window is open.                                                                                                                                         |
| `No direct Trusted Server candidate`                                 | `No TS candidate`         | `delivery=no_candidate`                     | `adInit` explicitly found no direct candidate for this request.                                                                                                                           |
| `Delivery status unknown — required evidence was not observed`       | `Delivery unknown`        | `delivery=unknown`                          | Evidence exists but cannot establish a more specific delivery state.                                                                                                                      |
| `Delivery evidence: Not applicable`                                  | —                         | `delivery=not_applicable`                   | No positive bridge evidence exists and a delivery conclusion does not apply before render or for an explicitly empty result.                                                              |
| `Delivery evidence: Not observed`                                    | —                         | missing delivery state                      | No delivery state was captured.                                                                                                                                                           |
| `Served bidder not confirmed`                                        | —                         | a non-empty GPT render without served proof | Server winner, targeting candidate, `bidWon`, and GPT render facts remain separate; none alone confirms the final served creative. This label is not shown for pending or empty requests. |
| `GPT slot onload observed`                                           | —                         | `slotOnload`                                | GPT emitted onload. It is not pixel-level or demand-source proof.                                                                                                                         |
| `GPT impressionViewable observed`                                    | —                         | `impressionViewable`                        | GPT emitted its viewability callback.                                                                                                                                                     |
| `Incomplete sequence`                                                | `Incomplete sequence`     | `incompleteSequence`                        | An observed callback proves a missing or invalid predecessor. Time alone never sets it.                                                                                                   |

## Auction evidence

| Label                               | Badge            | Raw value / source                                                                | Meaning and limits                                                                                                                                                               |
| ----------------------------------- | ---------------- | --------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `SSAT: initial-page server auction` | `SSAT`           | `auctionType=ssat`                                                                | Explicit completed server-auction evidence from the initial page request.                                                                                                        |
| `TS auction: SPA server auction`    | `TS auction`     | `auctionType=trusted_server`                                                      | Explicit completed `/_ts/page-bids` server-auction evidence.                                                                                                                     |
| `Client-side Prebid auction`        | `Prebid auction` | `auctionType=client_side`, exact Prebid callback correlation                      | A completed Prebid attempt was correlated to this slot and next GPT request. A refresh wrapper call alone does not establish it.                                                 |
| `Multiple auction paths observed`   | `Multiple paths` | `auctionType=competing`                                                           | Completed server and Prebid auction evidence both exist for the request. This does not prove a race, overwrite, or winner.                                                       |
| `Auction not observed`              | no auction badge | missing or malformed explicit facts                                               | Diagnostics has no qualifying completed-auction evidence. It never defaults to SSAT.                                                                                             |
| `Server auction winner`             | —                | compatibility `auctionWinner`, server `hb_bidder`                                 | Winner selected by the server auction. It is not the final served bidder.                                                                                                        |
| `Server bid price bucket:`          | —                | server `hb_pb`                                                                    | Already-bucketed price; raw CPM is never exported. Currency is shown only when supplied.                                                                                         |
| `Prebid targeting candidate:`       | —                | `prebidAuction.targetingCandidate` after exact `setTargetingForGPTAsync` boundary | Candidate targeting observed on the exact GPT slot for the exact Prebid callback auction ID. It is not a final win.                                                              |
| `Prebid candidate price bucket:`    | —                | candidate `hb_pb`                                                                 | Bucketed targeting value for the candidate; it is not a winning-price claim.                                                                                                     |
| `Prebid bidWon observation:`        | —                | `prebidAuction.win`, documented `bidWon` payload                                  | A bounded event joined by exact auction ID, ad-unit code, slot object, navigation generation, and retention window. It remains distinct from GPT render and served-source proof. |
| `Prebid win price bucket:`          | —                | `bidWon.adserverTargeting.hb_pb`                                                  | Bucketed value observed on the correlated `bidWon` event; it is not proof of the creative GAM served.                                                                            |
| `(currency not supplied)`           | —                | absent validated ISO currency                                                     | No verified currency was supplied. The console never assumes USD and does not compare currencies.                                                                                |

Bidder names are limited to 128 UTF-8 bytes, numeric bucket strings to 64 bytes, currencies to three ASCII letters, and auction IDs to 256 UTF-8 bytes. Prebid supplies its own auction ID through `bidsBackHandler`; diagnostics does not override Prebid auction identity. Duplicate, ambiguous, expired, late, prior-navigation, and malformed observations are rejected. Only an active diagnostics recorder installs the `bidWon` listener or retains candidate/win state, which is bounded to 128 pending attempts and 30 seconds. No raw CPM, creative markup, targeting dump, or losing bids are retained.

## Request paths and opportunities

| Label                                              | Raw value                 | Meaning                                                                                                                                    |
| -------------------------------------------------- | ------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------ |
| `Request path: Trusted Server direct`              | `trusted_server_direct`   | The direct `adInit` route preceded the request. Route evidence alone is not auction evidence.                                              |
| `Request path: Prebid refresh`                     | `prebid_refresh`          | The installed wrapper delegated the GPT refresh. Timeout and throw fallbacks retain this route label but do not claim a completed auction. |
| `Request path: Publisher refresh`                  | `publisher_refresh`       | Publisher refresh boundary observed.                                                                                                       |
| `Request path: Multiple paths observed`            | `competing`               | More than one route marker preceded the request; competition is possible but unproven.                                                     |
| `Request path: Not observed`                       | `unattributed`            | No eligible route marker was consumed.                                                                                                     |
| `Server bid available; creative source present`    | `renderable_candidate`    | Bid targeting plus ad ID and inline markup or complete cache coordinates were present.                                                     |
| `Server bid available; creative source incomplete` | `unrenderable_candidate`  | Bid targeting existed but the bridge lacked a complete render source.                                                                      |
| `Direct opportunity: No candidate`                 | `no_candidate`            | `adInit` explicitly observed no direct bid targeting.                                                                                      |
| `Direct opportunity: Not observed`                 | missing opportunity       | No bounded direct-opportunity evidence was captured. It is not negative demand-source evidence.                                            |
| `Request intent:`                                  | `requestIntentId`         | Opaque local correlation sequence, not an auction or user ID.                                                                              |
| `Trusted Server auction:`                          | `trustedServerAuctionId`  | Opaque per-auction correlation token, not a GAM key or visitor identifier.                                                                 |
| `Prebid auction:`                                  | `prebidAuction.auctionId` | Opaque Prebid-supplied auction correlation token. Diagnostics does not create or replace it.                                               |

Path markers live for five seconds, are consumed once, and are keyed by GPT slot object identity.

## Timing

All values are milliseconds. Missing timing that should apply is `Unavailable`, never zero. Server timing is `Not applicable` when no completed server auction was observed. A displayed zero is a valid immediate observation.

| Label                                       | Origin and boundaries                                                            | Raw field                                  |
| ------------------------------------------- | -------------------------------------------------------------------------------- | ------------------------------------------ |
| `Server request start → auction dispatched` | Server request `RequestTimings` T0 to successful `dispatch_auction` outcome      | `serverAuctionTimings.auctionDispatchedMs` |
| `Server request start → auction collected`  | Same T0 to completion of `collect_dispatched_auction`                            | `auctionResolvedMs`                        |
| `Server request start → bids ready`         | Same T0 to winning-bid map commit                                                | `auctionCommittedMs`                       |
| `Auction collection wait`                   | Actual duration blocked in collect; placement is `pre-header` or `in stream`     | `auctionWaitMs`, `auctionWaitPlacement`    |
| `Opportunity → request`                     | Browser `performance.now()`: recorder observation to matched GPT `slotRequested` | `opportunityToRequestMs`                   |
| `GAM request → response`                    | Browser `slotRequested` to `slotResponseReceived`                                | `durations.requestToResponseMs`            |
| `GAM response → render`                     | Browser `slotResponseReceived` to `slotRenderEnded`                              | `responseToRenderMs`                       |
| `GAM request → render`                      | Browser `slotRequested` to `slotRenderEnded`                                     | `requestToRenderMs`                        |
| `Render → load`                             | Browser `slotRenderEnded` to `slotOnload`                                        | `renderToLoadMs`                           |
| `Render → viewable`                         | Browser `slotRenderEnded` to `impressionViewable`                                | `renderToViewableMs`                       |
| `Replaced rendered request`                 | Earlier browser render callback to later request callback                        | `previousRenderToRequestMs`                |

Server offsets are not browser timestamps. `auctionResolvedMs` means collection completed (including timeout handling), not that a network byte arrived at that exact instant.

## Size, visibility, binding, and GAM fields

| Label                                                                          | Raw field / source                    | Meaning and limits                                                                                                                          |
| ------------------------------------------------------------------------------ | ------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------- |
| `Requested sizes`                                                              | `requestedSlotSizes`                  | Configured sizes supplied to GPT; ordinary sizes such as 300×250 remain visible. Missing data is shown as `Requested sizes: Not observed`.  |
| `GPT-reported size`                                                            | `size`                                | Exact `slotRenderEnded.size`. A 1×1 placeholder is shown as `GPT-reported size: 1×1 placeholder hidden` and retained unchanged in V1 JSON.  |
| `Size filled` / `Measured outer slot size`                                     | `observedSlotSize`                    | CSS outer box of the exact uniquely bound slot after fill. Missing data is shown as `Size filled: Not observed · Measured outer slot size`. |
| `GPT visibility`                                                               | current/maximum visibility percentage | Values from GPT visibility callbacks; absence is `GPT visibility: Not observed`.                                                            |
| `Binding: Bound` / `Bound`                                                     | `binding.status=bound`                | Exactly one connected publisher element matched.                                                                                            |
| `Binding: unbound` / `Unbound`                                                 | `binding.status=unbound`              | No safe element binding; reason is shown.                                                                                                   |
| `Binding: ambiguous` / `Ambiguous binding`                                     | `binding.status=ambiguous`            | Duplicate ID or GPT-slot evidence prevented a safe choice. No badge or locate action is invented.                                           |
| `Outside viewport`                                                             | binding geometry                      | The exact element does not intersect the viewport; it remains in the panel.                                                                 |
| `Ad Manager response class: empty/backfill/reservation/unclassified non-empty` | `responseClass`                       | Source-neutral classification derived from GPT render facts.                                                                                |
| `Ad Manager reported …`                                                        | `adManager` identifiers               | GAM-reported line item, order, advertiser, creative, yield-group, and company IDs. They do not identify the demand source.                  |
| `Ad Manager fields: Not observed`                                              | missing `adManager`                   | No allowlisted GAM identifier was captured.                                                                                                 |
| `Ad Manager response class: Not observed`                                      | missing `responseClass`               | GPT did not provide enough render evidence to classify the response.                                                                        |
| `Backfill yes/no`                                                              | `isBackfill`                          | GPT's callback value.                                                                                                                       |
| `Slot content changed yes/no`                                                  | `slotContentChanged`                  | GPT's callback value; not proof pixels changed.                                                                                             |
| `Creative changed/unchanged`                                                   | retained GAM creative IDs             | Comparison only when both cycles supplied an ID.                                                                                            |

Binding reasons are `missing_slot_element_id`, `missing_element`, `duplicate_dom_id`, `dom_uniqueness_unverifiable`, and `duplicate_gpt_slot_id`. Other exact technical fact prefixes are `Replaced rendered request`, `Creative changed`, `Creative unchanged`, `Trusted Server creative request observed at`, and `Trusted Server markup response sent at`.

## Failures, attribution, coverage, and retention

Creative bridge labels are `Creative bridge failure: missing render source`, `Creative bridge failure: cache fetch failed`, `Creative bridge failure: invalid cache payload`, and `Creative bridge failure: response post failed`. They map directly to `missing_render_source`, `cache_fetch_failed`, `invalid_cache_payload`, and `response_post_failed`; they describe only the observed bridge step and contain no URL, markup, payload, or stack trace.

Attribution issue labels map to `creative_request_without_slot`, `creative_request_without_cycle`, `creative_request_ambiguous_cycle`, `creative_request_on_empty_cycle`, `creative_attempt_capacity`, `creative_attempt_unknown`, `creative_attempt_expired`, and `creative_attempt_evicted`. Callback coverage separately counts `observed`, `matched`, `unmatched`, and `ambiguous` for `slotRequested`, `slotResponseReceived`, `slotRenderEnded`, `slotOnload`, `impressionViewable`, and `slotVisibilityChanged`.

The V1 export remains additive and compatible: optional Prebid evidence and optional currency fields are new; existing fields keep their meanings. Bounds are 64 slots, ten request cycles per slot, 128 callback issues, 128 attribution issues, 64 direct associations, 16 requested sizes, and 128 creative attempts. Metadata reports dropped callbacks/issues and evicted slots/cycles.

## Missing-value vocabulary

- **Not observed**: the relevant callback or explicit evidence was not captured.
- **Unavailable**: the value cannot be calculated or safely retained.
- **Not applicable**: the fact does not apply, for example delivery conclusions before render or for an explicitly empty result.
- **Unknown**: evidence exists but is insufficient to select a more specific state.

## Acronyms

- **TS**: Trusted Server.
- **SSAT**: server-side ad targeting on the initial page request.
- **SPA**: single-page application.
- **GPT**: Google Publisher Tag.
- **GAM**: Google Ad Manager.
- **Prebid**: the client-side header bidding library observed here.
- **PBS**: Prebid Server.
- **PUC**: Prebid Universal Creative.
- **CPM**: cost per thousand impressions; diagnostics retains only a bucketed targeting value, never raw CPM.
- **T0**: the start of one server request's `RequestTimings` clock.
