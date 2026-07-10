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
address policy: logical DNS policy or concrete --resolve pin
TLS verification: secure | insecure
application mode: HTTP/1 required | HTTP/2 eligible
```

The rewritten HTTP `Host` value is not itself an HTTP/1.1 transport key because
sequential requests may carry different Host values over a connection to the
same logical TO origin. HTTP/2 is stricter: it is eligible only when
`--rewrite-host` makes the request authority equal the TO hostname authenticated
by TLS. A default rule that sends `Host: FROM` forces HTTP/1.1, preserving the
existing deliberate separation between SNI (`TO`) and Host (`FROM`) without
relying on cross-origin HTTP/2 behavior.

The stable pool key is `(transport, normalized SNI/TO host, port, verification
mode, application mode, address policy)`. For `--resolve`, address policy
contains the pinned IP. For DNS, address policy is the logical DNS policy, not a
fallback-varying peer IP. Each connection record stores its actual peer address
and DNS generation for diagnostics, but multi-address fallback does not create a
different logical pool. Existing healthy connections may remain reusable until
their 60-second idle deadline even when the 30-second DNS entry expires.

The custom manager performs exact-key lookup only. It never coalesces across TO
hostnames, even if two names resolve to one IP or a certificate contains both
names. Hyper and Rustls drive a connection but do not select a pool entry; only
the manager does. Thus every secure connection was authenticated for the exact
TO/SNI hostname in its key before carrying a request.

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

Use a small custom transport manager because the design requires explicit live
and waiter bounds, response-body-controlled lease return, exact SNI-host
isolation, address-policy identity, and serialized HTTP/2 protocol discovery.
Hyper-util's general client pool does not expose all of those policies as one
auditable state machine. The custom manager still uses Hyper's maintained
HTTP/1.1 and HTTP/2 connection drivers, but owns leasing, limits, and the
complete origin key itself.

The HTTP/1.1 pool obeys these rules:

- A connection is eligible for reuse only after the prior response body reaches
  successful end-of-stream, including trailers. The proxy does not drain a body
  after the downstream browser cancels or drops it; dropping the wrapper closes
  that upstream connection. A body error also closes it. This trades a new
  connection after cancellation for strict bounds and simple backpressure.
- A connection reporting `Connection: close`, EOF, protocol failure, or driver
  termination is never returned to the pool. The response wrapper records
  connection-close intent before the existing hop-by-hop sanitation removes the
  header from the browser-facing response.
- At most six live HTTP/1.1 connections exist per origin and 64 live upstream
  connections exist globally. “Live” includes connecting, leased, idle, and
  draining connections. Admission of both limits is atomic under the transport
  manager; it must not hold one capacity permit while waiting for the other.
  Requests at the limit wait in FIFO order. The wait queue is bounded to 32 per
  origin and 128 globally; excess requests receive `502`. Dropping the browser
  request cancels and removes its waiter. An internal 30-second acquisition
  timeout returns `502` so a saturated or wedged origin cannot wait forever.
- Idle connections expire after a fixed short duration, initially 60 seconds.
- Per-origin and global idle limits prevent inactive connections from consuming
  all permits. Initial values are two idle HTTP/1.1 connections per origin and
  32 globally, subject to benchmark adjustment.
- Pool configuration remains internal in the first release. No user-facing
  tuning flags are added without demonstrated need.

### HTTP/1.1 Lease State Machine

Connection return is part of the core design, not an implementation detail. A
leased HTTP/1.1 sender and its driver-health signal remain owned by a
`PooledResponseBody` until both request upload and response download finish.

The request-body adapter records one of `Streaming`, `Complete`, or `Failed`:

- it becomes `Complete` only after the browser body returns terminal
  end-of-stream, including any trailers;
- an input error or dropping the browser request before upload completion marks
  it `Failed` and makes the connection permanently non-reusable;
- backpressure is preserved; the adapter never pre-buffers a streaming upload.

The response-body adapter follows this state machine:

1. `Streaming`: forward DATA and trailer frames unchanged. A trailer frame is
   not terminal by itself; wait for the following successful end-of-stream.
2. `Reusable`: when response end-of-stream is polled, enter only if request
   upload is **already** `Complete`, the connection driver is healthy, and there
   is no upstream close intent. Send the lease back through the manager's
   non-blocking return channel. The channel is bounded to the
   64-live-connection global limit; if it is unexpectedly full or closed, close
   the connection instead of blocking a body poll.
3. `Closed`: enter on body error, driver failure, request-upload failure,
   downstream cancellation/drop before terminal end-of-stream, or upstream close
   intent. Also enter `Closed` when response end-of-stream arrives while request
   upload is still `Streaming`; forward response EOF immediately, without waiting
   for or draining the upload. Drop the sender and capacity lease; never drain in
   the background.

Upstream `Connection: close` and equivalent HTTP/1 connection intent are
captured before hop-by-hop response sanitation. The sanitized header is still
not forwarded to the reusable browser tunnel. Returning a lease is idempotent:
drop after `Reusable` cannot return it twice.

### Stale-connection Retry

Servers may close idle HTTP/1.1 connections without the proxy noticing until
the next write. Two HTTP/1 failure classes are distinct:

- If the leased sender reports closed during readiness, before `send_request`
  takes the request body, discard the connection and retry once on a fresh
  connection. The original request has not been consumed.
- If `send_request` fails before yielding response headers, retry once only when
  the request was attempted on a reused connection **and** the request is in the
  replayable class below. Hyper does not prove that the origin saw no bytes, so
  this class is deliberately limited to idempotent methods with a reconstructable
  empty body.

Initially, replayable means `GET`, `HEAD`, or `OPTIONS` with no
`Content-Length`, no `Transfer-Encoding`, and an `Incoming` body whose size hint
is exactly zero **and** `Body::is_end_stream()` is true before the first attempt.
Both conditions are required: a zero-byte size hint alone can still precede
trailers and must not cause the original body to be discarded. For eligible
requests, the proxy converts the body to Hyper's reusable empty body and retains
cloned method, URI, version, and headers for one reconstruction. Proxy request
extensions are not part of wire behavior and are not forwarded today; no
extension-dependent request is eligible for retry. Unknown or misleading size
hints, `is_end_stream() == false`, trailers, a body error, or any observed body
frame make a request non-replayable. All other methods and all streaming uploads
are attempted once. The retry opens a fresh connection and preserves the
original wire-visible request head and security settings. There is never more
than one automatic retry.

### Upstream HTTP/2

An HTTP/2-eligible TLS client configuration advertises `h2` followed by
`http/1.1`. ALPN selects the protocol; absence of ALPN falls back to HTTP/1.1.

HTTP/2 is attempted only for TLS rules whose rewritten authority is the TO
hostname (`--rewrite-host`). Rules that retain `Host: FROM`, all plaintext
rules, and any rule whose authority cannot be proven equal to the authenticated
TO origin use a TLS configuration advertising only `http/1.1`. Eligible rules
advertise `h2` followed by `http/1.1`; an origin selecting HTTP/1.1 enters the
normal HTTP/1.1 pool under the eligible-mode origin key. For eligible rules:

- one healthy HTTP/2 connection per origin may carry up to 100 concurrent
  streams. A draining GOAWAY connection and its replacement may temporarily
  coexist, but only the replacement accepts new streams. No more than 32
  non-draining HTTP/2 connections exist globally. Draining connections still
  count toward the 64-live-connection global transport limit. Stream waiters use
  the same bounded per-origin/global queue and cancellation rules as HTTP/1.1;
- the connection remains keyed by the complete upstream origin key;
- before dispatch, the client constructs an HTTP/2 request URI whose authority
  is the TO hostname plus any non-default port. Hyper therefore emits
  `:authority` equal to the origin authenticated by TLS; it does not depend on
  translating an origin-form HTTP/1.1 Host header. Tests inspect the received
  HTTP/2 authority and prove that `Host: FROM` rules never enter this path;
- stream failure affects only that request where possible;
- GOAWAY stops new streams, permits eligible in-flight streams to complete, and
  causes future requests to establish a replacement connection;
- when Hyper exposes a peer `REFUSED_STREAM`, or a GOAWAY proves that a stream ID
  is greater than the peer's last processed stream, the manager may retry once
  on a replacement connection only if the full request body is reconstructable.
  The peer guarantee permits non-idempotent methods, but the proxy still cannot
  replay a consumed streaming `Incoming` body. Unclassified HTTP/2 failures are
  not retried;
- connection-level failure removes the connection from the pool;
- plaintext upstreams remain HTTP/1.1 unless explicit h2c support is designed
  later.

Cold protocol discovery is serialized per origin. An HTTP/2-eligible origin
starts in `Vacant`; the first requester transitions it to `Discovering` and
opens one TCP/TLS connection. Concurrent cold requests join the bounded waiter
queue rather than opening duplicate handshakes. ALPN transitions the origin to
one of:

- `Http2`, publishing the single multiplexed connection to waiters; or
- `Http1`, publishing the first connection and allowing additional HTTP/1.1
  connections up to the six-connection origin limit.

Discovery failure removes the state and wakes one waiter to attempt the next
discovery. GOAWAY replacement uses the same single-creator rule, so concurrent
requests cannot create a replacement stampede.

HTTP/2 translation must preserve method, path, query, response DATA, trailers,
and final status across the HTTP/1 browser leg. Tests also cover informational
responses supported by Hyper. If required trailers or informational responses
cannot be preserved with the selected Hyper APIs, HTTP/2 is not shipped; the
proxy does not silently weaken semantics to retain a benchmark win.

TLS client configurations are cached on two independent axes: verification mode
(`secure` or `insecure`) and application mode (`HTTP/1 required` or `HTTP/2
eligible`). This yields up to four immutable configurations. HTTP/1-required
configurations advertise only `http/1.1`; eligible configurations advertise
`h2, http/1.1`. A cache keyed only by `--insecure` is prohibited because it could
advertise HTTP/2 on a `Host: FROM` rule.

HTTP/2 is not part of the initial HTTP/1 pooling delivery. Build the focused
protocol-discovery/translation proof only if post-pooling metrics show connection
setup or the six-connection HTTP/1 concurrency ceiling consumes at least 10% of
the named concurrent remote-latency workload. HTTP/2 is then retained only if
the benchmark suite shows a meaningful improvement
over pooled HTTP/1.1 for the representative concurrent workload and the added
complexity does not weaken correctness. After two unrecorded warmups, run each
variant ten times on the same machine with alternating variant order. HTTP/2
must improve median total duration or throughput by at least 10%, must not
regress p95 total duration by more than 5%, and must produce no additional
request failures. Connection reduction is recorded but is not sufficient by
itself to retain HTTP/2. Results and median absolute deviation are recorded in
the implementation notes.

### DNS Cache

Connection reuse naturally reduces DNS calls. A bounded in-process DNS cache
handles connection creation when no `--resolve` pin applies.

- Cache normalized hostname and port to all resolved socket addresses.
- Use a conservative 30-second TTL because Tokio's basic resolver API does not
  expose authoritative DNS TTLs.
- Bound the cache to 64 entries; evict expired entries before applying a simple
  least-recently-used policy. If all entries are unexpired, evict the least
  recently used entry that has no lookup in flight. If every entry has an active
  lookup, perform the new lookup without caching its result rather than exceeding
  the bound.
- Never cache lookup failures.
- Coalesce concurrent cache misses for the same hostname and port into one
  lookup. Waiters receive an owned address list or a newly constructed
  equivalent I/O error; resolver error objects are not shared by reference.
- On connection failure, try the remaining resolved addresses before failing.
  One monotonic connect deadline covers DNS resolution and every address attempt.
  After DNS completes, each address receives a fair slice of the remaining
  budget (`remaining / addresses_left`), and no attempt may extend the total
  deadline. TLS handshake time remains outside the existing `--connect-timeout`
  semantics.
- A `--resolve` entry bypasses DNS and the DNS cache entirely.
- Do not let cached DNS state alter TLS SNI or the HTTP Host header.

DNS caching is not part of the initial HTTP/1 pooling delivery. Proceed to its
implementation only if post-pooling metrics show DNS lookup accounts for at
least 5% of median request-to-upstream-header time or repeated connection churn
performs enough lookups to affect the named workload by at least 5%. Otherwise
omit it and document the result. This explicit prior avoids adding state without
value for a proxy that normally targets one origin.

### Buffered Initial Request Parsing

Replace one-byte reads with bounded chunk reads into `BytesMut` or an equivalent
buffer. Search incrementally for `\r\n\r\n` and enforce the existing 8 KiB head
limit.

The 8 KiB limit applies only to bytes through the `\r\n\r\n` terminator. Bytes
read beyond a valid terminator are protocol over-read and cannot turn a valid
small head into `400`, regardless of the chunk size.

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

The prefixed-I/O adapter is the single owner of over-read bytes and yields each
byte exactly once before delegating to the socket. The TLS acceptor, blind tunnel,
and plain-HTTP forwarder all consume that same adapter; no path separately
replays the bytes. Buffered parsing is deferred unless initial metrics attribute
at least 5% of the local proxy-overhead workload to CONNECT/PAC head parsing. If
it misses that gate, retain the exact byte reader and record the rejected
optimization rather than accepting new protocol risk for a marginal gain.

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
`HeaderValue` held by a credential wrapper whose `Debug` implementation is
redacted. Request processing clones cheap reference-counted or header values
rather than allocating strings or Base64-encoding credentials.

Rule lookup remains linear initially because expected rule counts are small.
Replacing it with a map requires benchmark evidence; otherwise it adds duplicate
state and ordering complexity for no practical gain.

### Certificate Prewarming

Before launching browsers, generate the leaf server configuration for every
unique configured FROM hostname. Startup fails with the existing CA error
context if prewarming fails.

Normalize CONNECT authority hosts to lowercase ASCII before rule lookup and
`CertAuthority::server_config`, and keep prewarm keys in the same normalized
form. Certificate cache identity is case-insensitive DNS identity, so mixed-case
CONNECT authorities cannot cause a second mint or a false post-prewarm miss.

Only configured FROM hosts enter the MITM path; unmatched hosts are blind
tunneled or rejected. Prewarming therefore removes the legitimate runtime cache
miss without introducing asynchronous single-flight error sharing. The existing
synchronous cache remains as a defensive fallback. Runtime metrics flag any
unexpected mint after prewarming so a future dynamic-rule feature cannot make
the assumption silently false.

### Socket Options

Evaluate `TCP_NODELAY` independently immediately after accepting browser-facing
TCP sockets and immediately after opening upstream TCP sockets, before TLS or
HTTP handshakes. Record successful and failed applications separately. An
unexpected failure is warned once per socket class and counted, but does not
abort an otherwise usable local proxy.

After two warmups, run each setting ten times in alternating order. Retain a
setting only if it improves median total duration by at least 3%, does not
regress p95 by more than 5%, and does not regress process CPU time reported by
`/usr/bin/time -p` by more than 5%. Median absolute deviation is recorded. Packet
count is not used as a gate because the repository has no portable deterministic
packet-count harness.

No user-facing flag is planned. The accepted setting and evidence are recorded
in implementation notes.

## Metrics and Benchmarking

### Runtime Metrics

Use low-overhead atomics and scoped timers. Collect at least:

- accepted browser connections;
- CONNECT heads parsed and rejected;
- leaf cache hits, misses, unexpected post-prewarm mints, and mint duration;
- DNS cache hits, misses, and lookup duration;
- upstream TCP connection attempts, established connections, and connect
  duration;
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
selected. Keep its mandatory scope to the workloads that gate decisions:

1. One hundred sequential requests to a local TLS keep-alive upstream, measuring
   reuse and handshake count.
2. One hundred requests at concurrency 20 to a delayed HTTP/1.1 upstream,
   measuring pool bounds and total duration.
3. A remote-latency model with injectable DNS/connect/TLS/response delays,
   measuring where remaining time is spent without public network access.

Cold/warm certificate, DNS/`--resolve`, HTTP/2, parser, and socket-option variants
are added only for the corresponding stage after it clears its entry gate. The
benchmark harness must remain smaller than the transport implementation it
evaluates.

The harness records median, p95, total duration, upstream TCP connection count,
TLS handshake count, and request failures. Timing assertions are not placed in
ordinary CI tests. Correctness tests assert deterministic connection/handshake
counts; performance comparisons are run deliberately and documented.

### Acceptance Criteria

The named sequential workload is 100 zero-body GET requests over one
browser-side keep-alive tunnel to a local TLS upstream that keeps connections
open. After HTTP/1 pooling:

- exactly one established upstream TCP connection and one upstream TLS handshake
  after warm-up, compared with 100 of each at baseline. Failed multi-address
  attempts are counted separately and do not change logical reuse;
- no increase in request failures;
- streamed bodies remain unbuffered and byte-identical.

The named concurrent workload is 100 zero-body GET requests with concurrency 20
against an HTTP/1.1 upstream that holds each response for 25 milliseconds:

- deterministic tests observe at least two and no more than six simultaneous
  upstream requests;
- no more than six live connections exist for the origin and no more than two
  remain idle afterward;
- a separate saturation test proves the 32-per-origin and 128-global waiter
  bounds, FIFO admission, cancellation removal, and 30-second timeout using
  paused Tokio time rather than wall-clock sleeps;
- after two warmups, ten alternating before/after benchmark runs must show no
  more than 5% regression in median total duration or p95 and no additional
  failures. Median absolute deviation is recorded.

For allocation changes:

- CPU or duration improves measurably in the local overhead workload, or the
  change is retained only when it materially simplifies the code without
  weakening correctness.

Buffered parsing is retained only after clearing its 5% entry gate and passing
all over-read and 8 KiB head-limit tests. Otherwise it is not implemented.

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
   Cross-name HTTP/2 coalescing is disabled even when IP addresses and
   certificate SANs overlap.
7. Forwarding-header sanitation is unchanged: inbound `Forwarded` and
   `Fastly-SSL` are removed; `X-Forwarded-Host`, `X-Orig-Host`, and
   `X-Forwarded-Proto` are overwritten authoritatively. Existing
   `X-Forwarded-For` pass-through behavior is unchanged by this performance
   work.
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
- TLS client configuration separation across secure/insecure and HTTP/1
  required/HTTP/2 eligible modes.
- DNS cache hit, expiry, eviction, multi-address fallback, and `--resolve`
  bypass, including concurrent miss coalescing and error fan-out.
- Buffered head parsing at chunk boundaries, exact limit, oversized input,
  incomplete input, and over-read data, including a valid head below 8 KiB with
  an over-read chunk that takes the combined buffer above 8 KiB.
- Precomputed header/SNI/auth values.
- Certificate prewarm success, startup failure, deduplication of configured
  FROM hosts, mixed-case CONNECT reuse, and the unexpected runtime-miss metric.
- Retry eligibility for reused connections and replayable bodies, including
  absent framing, misleading/unknown size hints, `exact == 0` with
  `is_end_stream() == false`, zero-data bodies with trailers, unexpected frames,
  and mid-body errors.
- Metrics counters and bounded timing buckets.

### Integration Tests

Core HTTP/1 tests always run. DNS, buffered-parser, HTTP/2, and socket-option
tests become required only when the corresponding experiment clears its entry
gate and its code is retained.

- Multiple sequential requests reuse one upstream HTTP/1.1 connection.
- Concurrent requests open enough HTTP/1.1 connections to avoid serialization
  while respecting bounds.
- `Connection: close` prevents reuse.
- Browser cancellation, slow or infinite response bodies, body errors, and
  trailers prove that an HTTP/1.1 lease is returned only after successful
  end-of-stream and otherwise closes without unbounded draining.
- An upstream that returns an early response without consuming the full
  streaming upload reaches the browser without delay and closes, rather than
  pooling, that upstream connection.
- A server-closed idle connection is retried once on a fresh connection.
- Sender-readiness failure preserves an unconsumed request; post-send failure
  retries only an eligible idempotent empty request. Streaming and truncated
  uploads are never retried or pooled.
- Multi-address connection attempts share one total deadline and record the
  actual selected peer without fragmenting logical pool identity.
- TLS, plaintext, secure, insecure, and `--resolve` pools remain isolated.
- Origin-key tests vary protocol, TO hostname, port, address policy,
  verification mode, and application mode independently, including combinations
  that cannot arise in one CLI invocation because `--insecure` and `--resolve`
  are global. Cross-rule integration tests use feasible same-process cases:
  shared TO with different FROM values, rewritten versus preserved Host in
  separate proxy configurations, and distinct TO/port mappings. They assert
  intended HTTP/1.1 reuse, prohibited cross-key reuse, correct SNI/Host and
  HTTP/2 authority, authoritative forwarding headers, and that per-request
  Authorization values do not persist onto later requests on a reused
  connection.
- HTTP/2 multiplexes concurrent requests and falls back to HTTP/1.1.
- Concurrent cold HTTP/2-eligible requests perform exactly one ALPN discovery
  connection; HTTP/1 fallback then expands only within the six-connection bound.
- `Host: FROM` rules advertise only HTTP/1.1; HTTP/2-eligible rules expose
  `:authority = TO[:non-default-port]` at the test upstream.
- GOAWAY and connection failure trigger safe replacement.
- CONNECT over-read bytes reach the TLS or blind-tunnel consumer intact.
- Browser-facing and upstream `TCP_NODELAY` application is observable through
  counters, while injected option failures remain non-fatal and warn once per
  socket class.
- Existing routing, header sanitation, Basic auth, PAC, and non-loopback tests
  continue to pass.

### Quality Gates

Run the repository-prescribed CLI tests using `./scripts/test-cli.sh`, plus:

- `cargo fmt --all -- --check`
- `cargo clippy --package trusted-server-cli --target "$(rustc -vV | awk
  '/host:/ { print $2 }')" --all-targets -- -D warnings`;
- the explicit performance harness for before-and-after results.

The full repository adapter test matrix is required only if shared core or
workspace dependency changes affect adapters; otherwise the CLI host-target
suite is the primary gate.

## Delivery Stages

1. **Measurement foundation:** metrics, deterministic connection counters, and
   benchmark harness.
2. **HTTP/1.1 connection reuse (v1):** shared upstream state, bounded custom
   manager, lease state machine, connector, pooling, conservative stale retry,
   and streaming correctness.
3. **Hot-path cleanup:** shared immutable configuration, precomputed rules/auth,
   and allocation removal.
4. **Certificate cold-start:** normalized unique-host prewarming and defensive miss
   metrics.
5. **Buffered CONNECT parsing experiment:** implement chunked parsing and
   over-read replay only after its entry gate.
6. **DNS behavior experiment:** add the bounded cache only after its entry gate.
7. **Upstream HTTP/2 experiment:** after its entry gate, prove ALPN discovery,
   translation, multiplexing, fallback, GOAWAY handling, and
   benchmark retain/remove gate.
8. **Socket tuning experiment:** independently measure `TCP_NODELAY` and retain
   only useful settings.
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

## Implementation Validation Decisions

The connection-return state machine, origin identity, retry boundary, and
HTTP/2 eligibility are fixed design requirements above. The implementation plan
may resolve only these representation/evidence decisions with small proof tests:

1. The exact Hyper body erasure used to represent streaming `Incoming` bodies
   and reconstructable empty retry bodies without buffering.
2. Whether Tokio's resolver behavior makes the bounded DNS cache measurable
   after pooling.
3. Whether `TCP_NODELAY` helps the target workloads on macOS.

These are validation tasks, not reasons to weaken the invariants above.
