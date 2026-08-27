# PR 1079 Review Remediation Design

## Goal

Make the first-impression ownership and APS creative bridge safe under overlapping
publisher auctions, late callbacks, SPA navigation, mixed GPT refresh lists, and
nested 1x1 GAM shells. Preserve PR 1079's first-claimant policy: Trusted Server may
win an untouched physical slot, but must neither overwrite a publisher impression
nor let a stale response affect a later navigation.

## Ownership model

First-impression state remains keyed by navigation generation and exact physical
element identity. Each publisher auction gets an independent token whose
suppression decision is fixed when the auction is registered. When Trusted Server
commits its request, registration closes for new losing publisher auctions, while
already-registered losing tokens remain suppressible. Those tokens remain as
tombstones for the lifetime of the same navigation and exact physical element.
Unresolved suppressing tombstones are never evicted or removed by timeout or
auction failure; only navigation change or physical element replacement removes
them. The existing per-slot registration limit bounds the set before registration
closes, so an arbitrarily late correlated callback cannot become unrelated.

Prebid's pending bid/code correlation records carry the navigation generation and
physical element identity captured at registration. A record is usable only while
both still match, and consuming one exact ad-ID delivery removes only its auction's
registration. A code-only delivery consumes a record only when exactly one current
registration matches. Ambiguous ordinary code-only deliveries run an independent
auction rather than guessing; ambiguous TS-owned suppressing deliveries fail closed
without deleting their tombstones. Scoped `requestBids({ adUnitCodes })` calls
inspect, mutate, claim, and correlate only those requested global ad units.

## Refresh suppression

The Prebid delivery wrapper is the owner of first-impression delivery suppression.
When it suppresses a GPT slot, it also consumes any equivalent late-handoff
one-shot flag so the inner GPT wrapper cannot suppress the next legitimate
refresh. When it delegates a permitted GPT request, it consumes that flag at the
delegation boundary so the inner wrapper cannot silently drop the request. Mixed
refresh calls always forward the already-filtered slot list, including the path
where every remaining slot is excluded from a Prebid auction. That all-excluded
path performs the same ownership registration and consumption synchronously
before delegating. A bare refresh delayed by an auction becomes an explicit list
at callback time, preventing slots added after the snapshot from joining it.

A publisher-triggered GPT refresh that starts a synthetic Prebid auction registers
its own per-slot first-impression tokens before waiting for the asynchronous
callback. A publisher-first token reserves the slot so TS cannot claim it while
the auction is pending. A token registered against an earlier TS claim is consumed
at callback time, filtering that slot from the eventual GPT request. When TS emits
its first GPT request, registration closes for new losing publisher tokens so
ordinary later publisher refreshes continue normally. Mixed callbacks forward
only their unsuppressed slots and scope Prebid targeting to the same filtered set.
The callback also revalidates the captured navigation generation and exact
physical element, dropping stale work rather than refreshing a replacement slot.

## Creative bridge

Every asynchronous renderer/cache result is revalidated before posting a creative
response or recording successful response/billing evidence. A stale result may be
recorded as safe failure telemetry, but is never recorded as a response or win.
Validation covers navigation generation, winning bid identity, authenticated
source iframe identity, DOM connectivity, and containment in the authenticated
slot root. When a configured prefix matches several roots, the requesting frame
may disambiguate them only when exactly one candidate root owns that source.

After a valid response is posted, a collapsed 1x1 source iframe is expanded to the
winning creative size. The bridge walks all collapsed ancestors through the
authenticated slot root and expands each clipping shell. It refuses all resizing
for fixed/sticky, anchor, vignette, interstitial, detached, oversized, or
otherwise unauthenticated shells.

## Verification

Regression tests cover all seven review findings, including wrapper composition,
scoped ad-unit requests, mixed excluded refreshes, stale SPA callbacks,
overlapping auctions, stale cache responses with no successful response/billing
evidence, and two nested
collapsed ancestors. Existing JS unit/browser suites, formatting, lint, build,
and repository Rust verification remain the completion gates.
