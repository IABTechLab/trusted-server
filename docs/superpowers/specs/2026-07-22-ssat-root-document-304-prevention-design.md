# SSAT Root Document 304 Prevention Design

## Problem

An auction-eligible publisher navigation can currently return `304 Not Modified`
on reload. Trusted Server starts the server-side auction before fetching the
publisher document, but a 304 has no HTML body. The HTML processor therefore
cannot inject the current request's slot state or auction result, and the browser
reuses a previously synthesized document.

The behavior has two independent causes:

- Trusted Server forwards browser validators (`If-None-Match` and
  `If-Modified-Since`) to the publisher origin.
- The Fastly adapter sends publisher requests through its read-through cache.

Successful synthesized HTML is also returned with `private, max-age=0` while
retaining the publisher's `ETag` and `Last-Modified`. That explicitly permits
browser storage and revalidation even though those validators describe the
unmodified origin representation, not the personalized document returned by
Trusted Server.

## Scope

This change applies only when the existing `should_run_ad_stack` decision is
true. That decision already limits the path to GET document navigations that are
not prefetches or bots and that have matched ad slots, permitted consent, and an
enabled auction.

The change does not alter:

- HEAD requests;
- bots or prefetches;
- requests without matching slots or auction consent;
- publisher requests when the auction is disabled;
- static Trusted Server assets and their intentional conditional responses;
- the `/page-bids` client-side auction endpoint; or
- auction identifiers.

## Design

### Publisher request

Immediately before the publisher-origin fetch, an auction-eligible request will
remove `If-None-Match` and `If-Modified-Since`. This forces the publisher origin
to return a complete representation instead of validating a browser-cached
copy.

The corresponding `PlatformHttpRequest` will carry an explicit, default-false
cache-bypass option. The Fastly adapter will translate that option to
`fastly::Request::set_pass(true)` before both synchronous and asynchronous sends.
All other `PlatformHttpRequest` call sites retain their current behavior because
the option defaults to false. Adapters without an intermediary read-through
cache require no runtime change.

This bypass is deliberately scoped to the publisher fetch for an eligible SSAT
navigation. It must not be set on assets, image optimization, SSP fan-out, or
integration calls.

### Publisher response

When the eligible publisher response is HTML, Trusted Server will:

- set `Cache-Control: private, no-store`;
- remove `ETag` and `Last-Modified`;
- continue removing `Surrogate-Control` and
  `Fastly-Surrogate-Control`.

`no-store` is an intentional correctness choice. It prevents the browser from
retaining a synthesized document that could later be resurrected through
revalidation. This trades away some browser back/forward-cache eligibility and
increases publisher-origin traffic, but it guarantees that an eligible
navigation receives a fresh body for SSAT injection.

### Unexpected origin 304

An eligible publisher request will already be unconditional and will bypass the
Fastly cache. If the publisher nevertheless returns 304, Trusted Server will not
forward it to the browser. It will abandon the in-flight auction using a distinct
reason and return a synthetic `502 Bad Gateway` response with
`Cache-Control: private, no-store` and no validators or surrogate cache headers.

The implementation will not retry. The first request is already unconditional
and cache-bypassed, so repeating it is unlikely to produce a body and would add
origin traffic and latency. Returning an explicit non-cacheable error is safer
than allowing the browser to reuse stale personalized HTML.

## Data Flow

1. Trusted Server evaluates the existing SSAT eligibility gates.
2. If eligible, it dispatches the server-side auction as it does today.
3. Before the publisher fetch, it removes browser conditional headers and marks
   the platform request as cache-bypassed.
4. Fastly sends the request directly to the configured publisher backend.
5. A complete HTML response enters the existing buffering/HTML injection path.
6. Trusted Server injects current slot and auction data and removes all storage
   and validation metadata before responding.
7. If the origin unexpectedly returns 304, Trusted Server abandons the auction
   and returns the non-cacheable 502 instead.

## Error Handling and Observability

Existing publisher transport-error handling remains unchanged. The unexpected
304 case will reuse the existing abandoned-auction event mechanism with a
specific reason such as `unexpected_origin_304`, allowing it to be distinguished
from transport failures and ordinary bodiless responses.

Noneligible publisher requests retain their existing 304 behavior. This avoids a
global semantic change to the proxy and keeps normal conditional caching intact
outside personalized SSAT documents.

## Testing

Tests will prove the behavior at the relevant boundaries:

- `PlatformHttpRequest` defaults to ordinary cache behavior and its builder
  enables bypass explicitly.
- The Fastly adapter applies pass mode only when requested.
- An eligible publisher request removes both conditional headers and requests a
  platform cache bypass.
- A noneligible request preserves its conditional headers and ordinary cache
  behavior.
- Eligible HTML receives `private, no-store` and has origin and surrogate
  validators removed.
- An eligible origin 304, with or without `Content-Type`, is never returned as a
  client 304 and produces one abandoned-auction observation.
- Existing HEAD, prefetch, bot, noneligible 304, asset, and `/page-bids` tests
  remain unchanged.

Targeted tests will be written before implementation. Final verification will
use the repository's target-specific test, formatting, and lint commands rather
than a bare workspace build or test.

## Risks

- Every eligible SSAT navigation reaches the publisher origin and transfers a
  full document, increasing origin load and potentially TTFB.
- `no-store` can reduce back/forward-cache effectiveness, depending on browser
  behavior.
- A publisher that incorrectly emits 304 for an unconditional request will now
  expose a visible 502 instead of stale content. The distinct telemetry reason
  makes this condition diagnosable.

These costs are accepted because the requested invariant is that every eligible
SSAT navigation receives a complete document into which the current auction can
be injected.
