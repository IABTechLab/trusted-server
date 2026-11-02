# Absolute auction collection deadlines

## Contract

`PlatformHttpClient::select_until` limits additional network waiting to one
absolute deadline measured from auction dispatch. At expiry the adapter takes
one nonblocking snapshot of complete available responses, drops unfinished
operations, and permits the caller to drain only that completed snapshot.
This retains buffered bids when the publisher origin delayed collection. It
is not an upstream arrival-timestamp admission rule.

Every configured provider receives exactly one attributed outcome. Completed
bids remain eligible for local ranking. Unfinished responses become timeouts;
polling or transport errors remain transport failures. Mediation cannot launch
without budget and its wait uses the earlier of the auction deadline and its
own logical budget, independently of rounded transport timeouts.

## Adapter implementation

- Axum starts whole-response tasks at dispatch. Selection races their completion
  against the remaining deadline. The expired snapshot retains only finished
  tasks; dropping other handles aborts them.
- Cloudflare and Spin spawn cancelable full-response operations at dispatch and
  retain buffered results in channels. Selection probes the channel before
  waiting against the runtime timer. Pending ownership includes an abort handle;
  Cloudflare's fetch also owns an AbortController. Single-provider restrictions
  remain unchanged.
- Fastly explicitly bypasses caching for personalized auction requests, allowing
  conversion to the SDK's direct pending-request handle. Timed readiness waits
  contain real request/body handles. Header polling and body reads are
  nonblocking; ready bodies advance fairly. Expired collection caches one
  nonblocking snapshot and never waits for more bytes. The SDK cannot cancel
  pending backend requests, so expiration abandons them and closes acquired bodies.
- Fastly partial response payloads share a 10 MiB limit. Capacity pressure rejects
  the largest partial response with backend attribution. Completed vectors move
  directly into platform bodies. The limit does not include allocator overhead
  or runtime-owned host buffers.

## Validation

Regression coverage must exercise production split dispatch/collection, complete
buffered bids after a slow origin, provider count/order/elapsed time and timeout
attribution, exhausted-budget mediation, and the mediator's independent budget.
Adapters additionally cover delayed headers, trickling bodies, cancellation or
abandonment, bodyless response semantics, fair collection, and memory pressure.

The Fastly slow-origin harness runs automatically in CI. Worker runtime fixtures
use the actual HTTP adapters under local workerd and Spin to verify eager dispatch,
complete buffering, deadline waits, and cancellation. Native helper tests and
WASM compilation supplement these runtime cases.

Local parsing, ranking, HTML transformation, and delivery remain outside the
network-wait cutoff. Runtime scheduling and timer granularity can add a small
overshoot; already-triggered upstream side effects cannot be undone.
