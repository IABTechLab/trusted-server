# Prevent competing initial ad deliveries — Issue 1190

## Decision and evidence

Implement the demonstrated request-to-render ownership gap in the existing
per-slot first-impression mechanism. This specification replaces the earlier,
broader proposal on this branch. Specification, implementation, and regression
tests ship in one PR.

A controlled Chromium harness loaded built TSJS bundles, real Prebid 10.26.0,
and Prebid Universal Creative 1.17.2. It seeded an SSAT candidate and controlled
GPT events and auction responses using fictional endpoints. Registering the
publisher auction before `slotRequested` produced one call to the GPT fixture.
Registering after that event but before `slotRenderEnded` produced two calls.
The fixture delivered the recorded targeting through Universal Creative and
showed the server creative followed by the publisher-auction creative. Two
repeated comparisons passed timing and ownership-state assertions.

The fixture does not prove real GAM selection or the reported live flicker.
Seven subsequent staging loads yielded no sampled SSAT bids. The supplied
server diagnostic explains one such response: one provider returned no bid,
two returned HTTP 400, and the injected bid map was empty. These upstream errors
are a separate investigation, not part of this rendering change. No real domains,
credentials, customer configuration, or auction payloads belong in fixtures.

## Contract

Ownership is scoped to the existing physical DOM element and navigation
lifetime. It is asynchronous admission control, not a thread mutex.

1. Preserve publisher-first ownership and the existing publisher timeout/fallback.
2. Once TS claims initial delivery, publisher auctions started before initial
   `slotRenderEnded` are overlapping work. Register their suppression tokens
   before calling native Prebid, including after `slotRequested` and after five
   seconds. Consuming or abandoning another token must not close registration.
3. Filled and empty `slotRenderEnded` both settle the initial attempt. A missing
   render event does not unlock a submitted request on a timer. Setup failure
   before submission retains the existing claim release behavior.
4. Settlement is terminal for initial ownership. Later `slotRequested` events
   cannot reopen it. New auctions after settlement remain ordinary refreshes.
5. A registered losing token survives its lease, settlement, and consumption
   until its element/navigation lifetime ends. The same wrapped callback may
   execute again without gaining delivery permission. The existing pending Prebid code/bid
   index cleanup remains unchanged: indexes are consumed after delivery so they cannot indefinitely suppress
   unrelated, code-only refreshes.
6. Navigation or physical element replacement invalidates existing state under
   the existing identity rules. Do not add another generation or revision model.
7. Apply identical lifecycle transitions to the inline bootstrap and main bundle;
   bootstrap listeners can remain installed after main-bundle initialization.

SSAT candidate availability does not prove that GAM selected it. Ownership is
about the initial GPT delivery even if the bid map is empty or GAM chooses other
available demand.

## Targeting and compatibility boundary

This change prevents competing native GPT refreshes. It does not introduce a
transaction layer around arbitrary publisher targeting writes. Retain existing
initial-targeting restoration while initial delivery is pending. Once rendered,
suppress correlated losing refreshes without restoring the initial snapshot over
targeting from a later accepted delivery. Publisher targeting APIs may still
mutate targeting; preventing all such writes is separate work.

Retain the current callback-local and pending bid/code correlation behavior.
Re-entering the wrapped auction callback re-registers its original losing token.
After pending correlation has been consumed, neither repeated delivery using
its old bid ID nor an unattributable deferred refresh is guaranteed suppression.
Do not keep consumed code-only registrations indefinitely.

Preserve mixed-slot filtering, SRA batching, exclusion paths, refresh options,
internal TS refresh bypass, and one-shot GPT handoff consumption. The existing claim/token limits remain unchanged. At the 16-token per-claim
limit, overlapping auctions reuse a retained denial token from that exact TS
claim. No extra token is allocated and reaching capacity cannot permit delivery.
Render settlement still permits fresh auctions. Old tokens cannot suppress a
replacement DOM element or a new navigation. Publisher-owned capacity behavior
and the existing 256-claim limit remain unchanged.

Each newly acquired TS claim schedules one five-second diagnostic check, including
publisher-to-TS fallback. If that same claim and element/navigation lifetime are
still pending, record `pendingRenderDiagnostic` with phase and elapsed milliseconds.
The optional `tsjs.log.debug` message is “initial render remains pending”; enable
`tsjs.log.setLevel('debug')` to see it. The snapshot remains available on the claim
when logging is disabled or missing. This is evidence of delayed rendering, not
an error classification or timer-based unlock. Settled, released, replaced, and
previous-navigation claims do not emit it. Runtime and bootstrap use the same
rule; logger failures cannot affect delivery. There is no polling or new UI.

Broad targeting guards, pending-index redesign, diagnostic UI work, and
creative-renderer revisions remain deferred.

## Implementation map

- `crates/trusted-server-js/lib/src/core/first_impression.ts`: keep TS registration
  open through render; retain losing tokens; make render settlement terminal.
- `crates/trusted-server-core/src/integrations/gpt_bootstrap.js`: matching terminal
  lifecycle transition for persistent bootstrap listeners.
- `crates/trusted-server-js/lib/src/integrations/prebid/index.ts`: prevent restoring
  initial targeting after settlement; retain existing correlation cleanup.
- Extend Prebid and executable bootstrap tests. Update request-time refresh tests
  to settle initial rendering before asserting legitimate subsequent delivery.
- Commit a portable production-bundle browser regression with fictional traffic;
  real GPT/GAM and Rust auction execution remain outside that harness.

## Required verification

| Schedule or condition                                      | Expected behavior                                                          |
| ---------------------------------------------------------- | -------------------------------------------------------------------------- |
| Publisher starts first                                     | Publisher retains initial delivery                                         |
| Publisher starts after request, before render              | No competing native refresh                                                |
| Losing callback completes before or after render           | No competing native refresh                                                |
| Initial render pending beyond five seconds                 | New overlap still suppressed                                               |
| Losing callback consumed or throws, another auction starts | Protection remains open                                                    |
| Same wrapped losing callback repeats                       | Still suppressed                                                           |
| Fresh auction after filled or empty render                 | Subsequent refresh allowed                                                 |
| More than 16 overlapping auctions                          | Suppression remains bounded and effective; fresh post-render refresh works |
| Five-second pending diagnostic                             | One record; no unlock, no stale-lifetime report; logger failures harmless  |
| Later request event after initial render                   | Initial settlement stays terminal                                          |
| Losing callback after a legitimate refresh                 | No extra request or TS snapshot restoration                                |
| Setup failure before request                               | Existing safe claim release retained                                       |
| Navigation or element replacement                          | Old lifetime does not suppress new work                                    |
| Bootstrap then runtime                                     | Same request-to-render boundary                                            |
| Mixed slots, excluded slots, bare refresh, handoff         | Existing forwarding semantics preserved                                    |

CI runs the page-construction tests and both variants of the standalone browser
regression in fixed mode and uploads its evidence even on failure. Baseline mode
is a manual comparison tool, not a routine CI assertion.

Run focused tests red before implementation, then green, JS tests/lint/format/build,
the controlled browser regression, and repository CI gates before PR handoff.
Record any unavailable toolchain or service separately from code failures. Keep
the live flicker diagnosis explicitly unverified until a trace includes an SSAT
result and competing delivery. Deploy bootstrap and bundle changes together.
