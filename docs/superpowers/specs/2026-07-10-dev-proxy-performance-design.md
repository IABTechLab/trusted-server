# Dev Proxy Performance Optimization Design

**Date:** 2026-07-10

**Status:** Proposed

## Summary

Optimize `ts dev proxy` for realistic browser page loads without changing its
routing, security, or failure-isolation semantics. The work begins with
repeatable measurements, then removes repeated upstream connection setup,
reduces request-path allocation and parsing costs, prevents duplicate
certificate work, and evaluates HTTP/2 and socket tuning against explicit
performance gates.

The principal known bottleneck is structural: the browser-facing connection
supports HTTP/1.1 keep-alive, but every forwarded request opens a new upstream
TCP connection, performs a new TLS and Hyper handshake, sends one request, and
drops the sender. A page with many assets therefore repeats network setup that
should be amortized across requests.

## Goals

1. Reuse upstream connections safely across forwarded requests.
2. Support upstream HTTP/2 multiplexing when the origin negotiates it, with
   transparent HTTP/1.1 fallback.
3. Establish objective before-and-after measurements for latency, connection
   count, handshake count, throughput, and CPU-sensitive proxy overhead.
4. Remove avoidable per-request cloning, parsing, encoding, and allocation.
5. Reduce cold-start work when a browser opens several concurrent tunnels.
6. Preserve every existing security and routing guarantee.
7. Keep the implementation maintainable and proportionate to a local developer
   tool rather than turning it into a general-purpose production proxy.

## Non-goals

- Production proxy deployment or load-balancer behavior.
- Response or asset caching.
- Request coalescing, speculative prefetching, or content modification.
- Browser-facing HTTP/2 in the initial implementation. It may be evaluated
  separately after upstream optimizations are measured.
- HTTP/3 or QUIC.
- WebSocket proxying, which remains out of scope.
- Persisting DNS or connection state across process restarts.
- Changing the semantics of `--map`, `--rewrite-host`, `--resolve`,
  `--upstream-plaintext`, `--insecure`, or Basic authentication.

## Current Architecture and Cost

For a mapped HTTPS request, the proxy currently performs:

1. Browser TCP accept.
2. Byte-at-a-time parsing of the initial CONNECT request head.
3. Browser-facing TLS termination using a cached per-host leaf configuration.
4. HTTP/1.1 request parsing on a reusable browser tunnel.
5. Per request, cloning of rules, Basic-auth configuration, and the resolution
   map.
6. Per request, hostname lookup or `--resolve` selection.
7. Per request, a new upstream TCP connection.
8. Per request, a new upstream TLS handshake for HTTPS.
9. Per request, a new Hyper HTTP/1.1 handshake and connection driver.
10. Streaming of one response, after which the upstream sender is dropped.

Steps 6–9 dominate remote-origin latency. Steps 2 and 5 are smaller but occur
often enough to matter after connection reuse removes the larger cost.

## Design Principles

- **Evidence before tuning.** Instrument a cost before attempting to optimize it.
- **Correctness before reuse.** Never send a request on a connection whose
  origin, TLS policy, or pinned destination differs from the request.
- **Bounded state.** Pools and caches must have capacity and lifetime bounds.
- **Streaming remains streaming.** Do not buffer request or response bodies to
  simplify pooling.
- **Failure remains local.** A dead pooled connection produces a controlled
  retry or one request-level `502`; it never terminates the accept loop or an
  unrelated browser tunnel.
- **No silent security downgrade.** HTTP/2 fallback, retry, DNS caching, and
  pooling must preserve certificate verification and SNI.
- **Incremental delivery.** Each optimization is independently tested and
  benchmarked so its effect is attributable and reversible.

## Architecture

### Shared Proxy State

Introduce a shared immutable/runtime state object, conceptually:

```rust
struct ProxyState {
    config: Arc<ResolvedConfig>,
    upstream: UpstreamClient,
    metrics: Arc<ProxyMetrics>,
}
```

The state is created once after configuration resolution and shared by accepted
browser connections. Hyper service closures clone an `Arc<ProxyState>` rather
than cloning the rule table, DNS pins, and credentials for every request.

`ResolvedConfig` remains immutable after listener binding updates the effective
listen address. Values needed in the hot path are validated and precomputed
during resolution.

### Upstream Origin Key

Every reusable connection is indexed by a key containing all properties that
affect transport identity or security:

```text
protocol: plaintext | TLS
logical host: normalized TO hostname
port
connect address: DNS-derived address or --resolve pin
TLS verification: secure | insecure
```

The rewritten HTTP `Host` value is not itself a transport key: HTTP/1.1 and
HTTP/2 may carry different valid Host/authority values over a connection to the
same logical upstream origin. The logical TO hostname and TLS policy are always
part of the key, so a connection authenticated for one SNI cannot be reused for
another.

If DNS returns multiple addresses, each live connection is associated with the
specific selected address. DNS refresh may create connections to a new address;
existing healthy connections may live until their idle deadline.

### Upstream Client and HTTP/1.1 Pool

Create a dedicated `upstream` module responsible for:

- resolving or pinning connection addresses;
- opening TCP sockets under the configured connect timeout;
- applying measured socket options;
- performing TLS negotiation and verification;
- selecting HTTP/1.1 or HTTP/2 from ALPN;
- leasing and returning reusable HTTP/1.1 connections;
- multiplexing requests over HTTP/2 connections;
- tracking connection lifecycle metrics.

The preferred implementation is Hyper's maintained pooled client machinery
through `hyper-util`, with a custom connector that honors `--resolve`, SNI,
plaintext, and `--insecure`. Before implementation, a focused spike/test must
prove that this connector can preserve origin-form requests and existing header
rewrites without losing streaming bodies.

If the maintained pool cannot meet those requirements cleanly, implement a
small bounded pool rather than bending request semantics around it. A custom
HTTP/1.1 pool must obey these rules:

- A connection is eligible for reuse only after the prior response body is
  completely consumed or explicitly drained.
- A connection reporting `Connection: close`, EOF, protocol failure, or driver
  termination is never returned to the pool.
- Concurrent requests may open multiple connections for an origin up to the
  configured internal limit; they must not serialize behind one long response.
- Idle connections expire after a fixed short duration, initially 60 seconds.
- Per-origin and global idle limits prevent unbounded state. Initial values are
  two idle HTTP/1.1 connections per origin and 32 globally, subject to
  benchmark adjustment.
- Pool configuration remains internal in the first release. No user-facing
  tuning flags are added without demonstrated need.

### Stale-connection Retry

Servers may close idle HTTP/1.1 connections without the proxy noticing until
the next write. The upstream client retries once when all of these are true:

- the request was attempted on a reused connection;
- failure occurred before any response headers were received;
- the request body is safely replayable.

Initially, replayable means an empty body or a fully available body type that
can be reconstructed without buffering. Streaming uploads are not retried. The
retry opens a fresh connection and preserves the original request headers and
security settings. There is never more than one automatic retry.

### Upstream HTTP/2

TLS client configuration advertises `h2` followed by `http/1.1`. ALPN selects
the protocol; absence of ALPN falls back to HTTP/1.1.

For HTTP/2:

- one healthy connection may carry multiple concurrent streams;
- the connection remains keyed by the complete upstream origin key;
- request `Host` semantics are translated correctly to HTTP/2 authority by the
  Hyper client without forwarding prohibited hop-by-hop headers;
- stream failure affects only that request where possible;
- GOAWAY stops new streams, permits eligible in-flight streams to complete, and
  causes future requests to establish a replacement connection;
- connection-level failure removes the connection from the pool;
- plaintext upstreams remain HTTP/1.1 unless explicit h2c support is designed
  later.

HTTP/2 is retained only if the benchmark suite shows a meaningful improvement
over pooled HTTP/1.1 for the representative concurrent workload and the added
complexity does not weaken correctness. The initial meaningful-improvement gate
is at least 10% lower median total duration or at least 20% fewer upstream
connections for the concurrent remote-style workload. Results are recorded in
the implementation notes.

### DNS Cache

Connection reuse naturally reduces DNS calls. A bounded in-process DNS cache
handles connection creation when no `--resolve` pin applies.

- Cache normalized hostname and port to all resolved socket addresses.
- Use a conservative 30-second TTL because Tokio's basic resolver API does not
  expose authoritative DNS TTLs.
- Bound the cache to 64 entries; evict expired entries before applying a simple
  least-recently-used or oldest-entry policy.
- Never cache lookup failures.
- On connection failure, try the remaining resolved addresses before failing.
- A `--resolve` entry bypasses DNS and the DNS cache entirely.
- Do not let cached DNS state alter TLS SNI or the HTTP Host header.

If benchmarks show no measurable resolver cost after pooling, the DNS cache may
be omitted and that decision documented. This avoids adding state without value.

### Buffered Initial Request Parsing

Replace one-byte reads with bounded chunk reads into `BytesMut` or an equivalent
buffer. Search incrementally for `\r\n\r\n` and enforce the existing 8 KiB head
limit.

The parser returns both:

- the exact complete HTTP request head; and
- any bytes read beyond the header terminator.

Over-read bytes must be replayed before subsequent socket bytes in every path:

- browser TLS after CONNECT;
- blind TLS tunnel;
- plain HTTP forwarding.

A prefixed-I/O wrapper or explicit replay buffer will preserve ordering. This is
required for correctness even though common browsers usually wait for the
CONNECT response before sending TLS data.

### Precomputed Rule Data

During configuration resolution, each rule precomputes validated immutable data:

- normalized FROM hostname;
- parsed logical TO hostname and port;
- TLS `ServerName` for TLS upstreams;
- upstream `Host` header value;
- `X-Forwarded-Host` and `X-Orig-Host` values;
- scheme and transport policy;
- stable portions of the upstream origin key.

Basic authentication is encoded and validated once into a reusable
`HeaderValue`. Request processing clones cheap reference-counted or header
values rather than allocating strings or Base64-encoding credentials.

Rule lookup remains linear initially because expected rule counts are small.
Replacing it with a map requires benchmark evidence; otherwise it adds duplicate
state and ordering complexity for no practical gain.

### Certificate Prewarming and Single Flight

Before launching browsers, generate the leaf server configuration for every
unique configured FROM hostname. Startup fails with the existing CA error
context if prewarming fails.

The cache also prevents duplicate work for hosts requested concurrently:

- different hostnames may mint concurrently;
- only one mint operation occurs for a given hostname;
- waiters receive the same cached `Arc<ServerConfig>` or the same failure;
- a failed mint does not permanently poison future attempts.

Prewarming removes normal browser cold-start contention; single-flight behavior
keeps the cache correct for future dynamic access and direct tests.

### Socket Options

Evaluate `TCP_NODELAY` independently for browser-facing and upstream sockets.
It is enabled only where repeated benchmark runs show a meaningful latency
improvement without an unacceptable increase in packet/CPU overhead. Socket
option failure is logged at debug level and does not abort the proxy.

No user-facing flag is planned. The accepted setting and evidence are recorded
in implementation notes.

## Metrics and Benchmarking

### Runtime Metrics

Use low-overhead atomics and scoped timers. Collect at least:

- accepted browser connections;
- CONNECT heads parsed and rejected;
- leaf cache hits, misses, waits, and mint duration;
- DNS cache hits, misses, and lookup duration;
- upstream TCP connections opened and connect duration;
- upstream TLS handshakes and handshake duration;
- negotiated HTTP/1.1 and HTTP/2 connection counts;
- HTTP/1 pool hits, misses, stale failures, and retries;
- HTTP/2 stream count and connection replacements;
- request time to upstream response headers;
- completed and failed requests.

Metrics are summarized on clean shutdown when verbose/debug diagnostics are
enabled. They must not print credentials, query strings, certificate material,
or sensitive headers. Normal output remains concise.

Timing uses monotonic `Instant`. Histograms may use bounded fixed buckets rather
than retaining one sample per request.

### Benchmark Harness

Add a deterministic local benchmark/integration harness, not a flaky CI
microbenchmark. It runs outside the normal blocking test suite unless explicitly
selected and supports:

1. One local plaintext request to measure the proxy overhead floor.
2. One hundred sequential requests over one browser-side keep-alive tunnel.
3. One hundred requests at browser-like concurrency.
4. Local TLS upstream requests with connection and handshake counters.
5. Artificially delayed upstream setup to model a remote service without
   requiring public network access.
6. Cold and warm certificate-cache runs.
7. DNS and `--resolve` variants.
8. HTTP/1.1-only and HTTP/2-capable upstreams.

The harness records median, p95, total duration, upstream TCP connection count,
TLS handshake count, and request failures. Timing assertions are not placed in
ordinary CI tests. Correctness tests assert deterministic connection/handshake
counts; performance comparisons are run deliberately and documented.

### Acceptance Criteria

For the sequential local-TLS workload after HTTP/1 pooling:

- at least 90% fewer upstream TCP connections after warm-up;
- at least 90% fewer upstream TLS handshakes after warm-up;
- no increase in request failures;
- streamed bodies remain unbuffered and byte-identical.

For the concurrent workload:

- total duration must not regress by more than 5% across repeated runs;
- the pool must permit concurrency rather than forcing all work through one
  HTTP/1.1 connection;
- no unbounded growth in live or idle connections.

For parser and allocation changes:

- deterministic tests prove over-read preservation;
- CPU or duration improves measurably in the local overhead workload, or the
  change is retained only when it materially simplifies the code without
  weakening correctness.

HTTP/2 and `TCP_NODELAY` use their specific retain/remove evidence gates. Raw
benchmark output and a short interpretation are saved with the implementation
notes rather than asserted as fragile wall-clock CI thresholds.

## Error Handling

All new fallible paths continue using `error-stack` with concrete contexts.

- Pool acquisition/open failures become request-level `502` responses.
- One eligible stale-connection retry is transparent; its final failure is
  logged once with origin and phase, without sensitive request data.
- DNS failures include the logical hostname and resolution phase.
- TLS failures distinguish negotiation, certificate verification, and protocol
  handshake phases where the underlying error permits it.
- Background connection-driver failures remove their connection and increment a
  metric; they do not panic.
- Metrics failure or debug-summary formatting must never affect proxy behavior.
- Certificate prewarm failures abort startup before browsers launch.

## Security and Compatibility Invariants

The optimized implementation must preserve these invariants:

1. TLS SNI is always the TO hostname and never includes a port.
2. Secure mode validates the upstream certificate against the TO hostname.
3. `--insecure` remains explicit, global, and loudly warned.
4. `--resolve` changes only the connection address, never SNI or Host semantics.
5. Plaintext and TLS connections are never pooled together.
6. Connections authenticated for different TO hostnames are never shared.
7. Inbound spoofable forwarding headers remain stripped and authoritative
   headers remain stamped.
8. Basic auth remains restricted on non-loopback listeners and is never logged.
9. Unmatched CONNECT behavior remains unchanged.
10. Hop-by-hop headers remain stripped in both directions.
11. Streaming request and response bodies remain bounded by backpressure.
12. A retry never duplicates a non-replayable request body.
13. Pool and cache state is process-local, bounded, and discarded on exit.
14. Safari proxy restoration and CA trust behavior remain unchanged.

## Testing Strategy

All behavior changes follow test-driven development.

### Unit Tests

- Origin-key equality and separation across every security-relevant field.
- DNS cache hit, expiry, eviction, multi-address fallback, and `--resolve`
  bypass.
- Buffered head parsing at chunk boundaries, exact limit, oversized input,
  incomplete input, and over-read data.
- Precomputed header/SNI/auth values.
- Certificate single-flight success, failure, and retry.
- Retry eligibility for reused connections and replayable bodies.
- Metrics counters and bounded timing buckets.

### Integration Tests

- Multiple sequential requests reuse one upstream HTTP/1.1 connection.
- Concurrent requests open enough HTTP/1.1 connections to avoid serialization
  while respecting bounds.
- `Connection: close` prevents reuse.
- A server-closed idle connection is retried once on a fresh connection.
- Streaming upload is not retried.
- TLS, plaintext, secure, insecure, and `--resolve` pools remain isolated.
- HTTP/2 multiplexes concurrent requests and falls back to HTTP/1.1.
- GOAWAY and connection failure trigger safe replacement.
- CONNECT over-read bytes reach the TLS or blind-tunnel consumer intact.
- Existing routing, header sanitation, Basic auth, PAC, and non-loopback tests
  continue to pass.

### Quality Gates

Run the repository-prescribed CLI tests using `./scripts/test-cli.sh`, plus:

- `cargo fmt --all -- --check`
- the relevant host-target clippy invocation with warnings denied;
- the explicit performance harness for before-and-after results.

The full repository adapter test matrix is required only if shared core or
workspace dependency changes affect adapters; otherwise the CLI host-target
suite is the primary gate.

## Delivery Stages

1. **Measurement foundation:** metrics, deterministic connection counters, and
   benchmark harness.
2. **HTTP/1.1 connection reuse:** shared upstream state, connector, pooling,
   stale retry, and streaming correctness.
3. **Hot-path cleanup:** shared immutable configuration, precomputed rules/auth,
   and allocation removal.
4. **Buffered CONNECT parsing:** chunked parsing and over-read replay.
5. **Certificate cold-start:** prewarming and per-host single flight.
6. **DNS behavior:** measure after pooling; add the bounded cache only if useful.
7. **Upstream HTTP/2:** ALPN, multiplexing, fallback, GOAWAY handling, and
   benchmark retain/remove gate.
8. **Socket tuning:** independently measure `TCP_NODELAY` and retain only useful
   settings.
9. **Final verification and documentation:** record benchmark results, update
   developer documentation for diagnostics only where user-visible behavior
   changed, and run all quality gates.

Each stage must leave the proxy working and tested. No stage depends on accepting
an optimization that fails its evidence gate.

## Documentation

User-facing flags should remain unchanged. Update the dev-proxy guide only if
new diagnostics are exposed or behavior visible to users changes. Add a short
developer-facing performance note containing:

- benchmark command;
- workload definitions;
- before-and-after results;
- retained and rejected optimizations;
- pool/cache constants and the evidence supporting them.

## Open Implementation Decisions

The implementation plan must resolve these with small proof tests before broad
code changes:

1. Whether `hyper-util`'s pooled client can accept the existing streaming
   `Incoming` request body and custom connector without forcing URI or buffering
   changes.
2. The cleanest response-body wrapper or client API for returning HTTP/1.1
   connections only after body completion.
3. Whether HTTP/2 authority translation preserves the deliberately rewritten
   Host behavior for all rule forms.
4. Whether Tokio's resolver behavior makes the bounded DNS cache measurable
   after pooling.
5. Whether `TCP_NODELAY` helps the target workloads on macOS.

These are validation tasks, not reasons to weaken the invariants above.
