# Dev Proxy Performance Optimization Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `ts dev proxy` materially faster against remote/staging upstreams by reusing HTTP/1.1 connections, while treating localhost connection-count reduction as success even when wall-clock change is small; retain optional parser, DNS, HTTP/2, and socket optimizations only when measurements clear the approved gates.

**Architecture:** Create a shared `ProxyState` and bounded upstream-manager actor. The actor owns exact origin-key isolation, FIFO admission, connection opening, sender leases, and idle return; body adapters preserve streaming and return a lease only when request upload and response download both completed successfully. Deterministic counters prove correctness before manual timing comparisons.

The single manager actor is a deliberate serialization point: at developer-proxy request rates its channel hop is negligible, and it makes driver-owned capacity, cancellation, and bounded teardown auditable. Pool-acquisition metrics verify that assumption.

**Tech Stack:** Rust 2024, Tokio, Hyper 1, hyper-util, Rustls 0.23, tokio-rustls, error-stack, macOS CLI integration tests.

**Design:** `docs/superpowers/specs/2026-07-10-dev-proxy-performance-design.md`

---

## File Structure

**Create:**

- `crates/trusted-server-cli/src/commands/dev/proxy/metrics.rs` — atomic counters, fixed timing buckets, snapshots.
- `crates/trusted-server-cli/src/commands/dev/proxy/upstream/mod.rs` — `UpstreamClient` facade.
- `crates/trusted-server-cli/src/commands/dev/proxy/upstream/key.rs` — transport/SNI/port/verification/application/address-policy identity.
- `crates/trusted-server-cli/src/commands/dev/proxy/upstream/manager.rs` — bounded actor, FIFO waiters, idle expiry and return.
- `crates/trusted-server-cli/src/commands/dev/proxy/upstream/connect.rs` — address selection, total deadline, TCP/TLS/Hyper handshakes.
- `crates/trusted-server-cli/src/commands/dev/proxy/upstream/body.rs` — request completion and pooled response body state machines.
- `crates/trusted-server-cli/tests/proxy_pool_e2e.rs` — pooling lifecycle tests.
- `crates/trusted-server-cli/tests/proxy_perf.rs` — ignored performance harness.
- `docs/superpowers/implementation-notes/2026-07-10-dev-proxy-performance.md` — evidence and gate decisions.

**Create only if the gate passes:**

- `crates/trusted-server-cli/src/commands/dev/proxy/upstream/dns.rs`
- `crates/trusted-server-cli/src/commands/dev/proxy/prefixed_io.rs`

**Modify:** `proxy/{mod.rs,config.rs,rewrite.rs,server.rs,ca.rs}`, `trusted-server-cli/Cargo.toml`, `tests/support/mod.rs`, `tests/proxy_e2e.rs`, and user docs only for observable changes.

## Global Rules

- Follow red-green-refactor for every behavior: write one test, observe the expected failure, implement minimally, rerun the focused test, then rerun the affected test binary.
- Use `./scripts/test-cli.sh`; never use a bare workspace test.
- Keep optional experiments out of v1 commits. If a gate fails, record `SKIP` or `REJECT` and remove experimental production code.
- Commit after each task only when its focused tests and the affected test binary pass.

Every performance run prints one raw record, and each ten-run comparison prints
separate across-run summaries:

```text
PERF_RUN workload=<name> variant=<name> run=<n> duration_us=<n> tcp_attempts=<n> tcp_established=<n> tls_handshakes=<n> failures=<n>
PERF_SUMMARY workload=<name> variant=<name> runs=10 median_duration_us=<n> p95_duration_us=<n> mad_duration_us=<n> failures=<n>
```

Each comparison test alternates variants internally after two warmups and prints median, p95, and MAD summary records. Harness-only preconnection opens the configured HTTP/1 connections and waits for manager idle state before starting the timer.

---

### Task 1: Baseline Metrics and Harness

**Files:** Create `metrics.rs`, `proxy_perf.rs`, and implementation notes; modify `mod.rs` and `tests/support/mod.rs`.

- [x] Run `./scripts/test-cli.sh`. Expected: baseline PASS; stop if it does not.
- [x] Write failing `metrics.rs` tests requiring separate TCP-attempt/established counters and a fixed-size timing histogram.

```rust
#[test]
fn snapshot_separates_attempts_from_established() {
    let metrics = ProxyMetrics::default();
    metrics.record_tcp_attempt();
    metrics.record_tcp_attempt();
    metrics.record_tcp_established(Duration::from_millis(7));
    let snapshot = metrics.snapshot();
    assert_eq!(snapshot.tcp_attempts, 2);
    assert_eq!(snapshot.tcp_established, 1);
    assert_eq!(snapshot.connect_latency.total(), 1);
}
```

- [x] Run `cargo test --package trusted-server-cli --target "$(rustc -vV | awk '/host:/ { print $2 }')" metrics::tests`. Expected: RED because metrics do not exist.
- [x] Implement `ProxyMetrics` with `AtomicU64` fields and fixed atomic duration buckets, including methods for initial-head parse, pool acquisition, and queue wait. Expose `snapshot`, phase-recording methods, and a redacted debug summary; retain no per-request labels or samples. Task 1 uses fixture counters for the baseline; runtime phase wiring occurs when `ProxyState` reaches `server.rs` in Task 6.
- [x] Extend the TLS upstream fixture with shared accepted-connection, request, handshake, and failure counters.
- [x] Add baseline `#[ignore = "manual performance workload"]` tests for 100 sequential TLS GETs, 100 delayed GETs at matched concurrency six, and a separate concurrency-20 saturation workload. Add the injectable remote-latency model in Task 10 after the connection factory exists.
- [x] Run the ignored harness with `--ignored --nocapture --test-threads=1`. Expected baseline: approximately 100 established upstream connections and handshakes for 100 sequential requests.
- [x] Record raw output, machine, OS, Rust version, and command in implementation notes.
- [x] Run `./scripts/test-cli.sh` and commit as `Establish dev proxy performance baseline`.

---

### Task 2: Safe Precomputed Origin Identity

**Files:** Create `upstream/{mod.rs,key.rs}`; modify `config.rs`, `rewrite.rs`, and proxy `mod.rs`.

- [x] Write failing key tests varying transport, normalized TO/SNI host, port, verify mode, application mode, DNS policy, and `--resolve` pin independently.
- [x] Add a test proving two peer IPs selected for one DNS origin do not fragment the logical key, and two TO names sharing one IP never compare equal.
- [x] Run focused tests. Expected: RED because `OriginKey` does not exist.
- [x] Implement closed key types:

```rust
pub enum Transport { Plaintext, Tls }
pub enum VerifyMode { Secure, Insecure }
pub enum ApplicationMode { Http1Required, Http2Eligible }
pub enum AddressPolicy { Dns, Resolve(IpAddr) }
pub struct OriginKey {
    transport: Transport,
    reference: ReferenceIdentity,
    port: u16,
    verify: VerifyMode,
    application: ApplicationMode,
    address: AddressPolicy,
}

pub enum ReferenceIdentity { Dns(Arc<str>), Ip(IpAddr) }
```

- [x] Write failing tests requiring prevalidated `HeaderValue`s, `ServerName<'static>`, and a Basic-auth Debug output containing neither username, password, nor encoded token.
- [x] Replace `BasicAuth { user, pass }` with a private reusable header and custom `Debug` rendering `BasicAuth([REDACTED])`. Update fixtures to construct it through a checked constructor.
- [x] Precompute rule host/forwarding headers, SNI, transport, and stable origin-key fields during `config::resolve`; eliminate per-request Base64 and host-header formatting.
- [x] Run config/rewrite/key tests and existing E2E tests. Expected: GREEN with unchanged behavior.
- [x] Commit as `Precompute dev proxy upstream identity`.

---

### Task 3: Bounded Manager Actor

**Files:** Create `upstream/manager.rs`; modify `upstream/mod.rs` and CLI Cargo.toml.

- [ ] Add Tokio `test-util` only to the macOS dev-dependency feature set.
- [ ] Note in the code/implementation notes that Cargo feature unification makes `test-util` available in the macOS test build, while pause/advance behavior remains inert unless tests explicitly use it.
- [ ] With paused time and a test harness that observes `ConnectRequested` commands and manually replies `Connected`, write failing tests for: six live HTTP/1 connections per origin, 64 globally, two idle per origin, 32 idle globally, 32 queued per origin, 128 queued globally, FIFO wakeup within an origin, oldest-admissible wakeup across origins without head-of-line blocking, dropped-waiter removal, and 30-second timeout.
- [ ] Add a test proving origin/global admission is atomic and never reserves one capacity while waiting for the other.
- [ ] Run manager tests. Expected: RED because the actor is absent.
- [ ] Implement one bounded command actor:

```rust
enum Command {
    Acquire { key: OriginKey, ticket: Arc<AcquireTicket>, reply: oneshot::Sender<Result<Lease, AcquireError>> },
    Return(IdleConnection),
}

enum ControlEvent {
    Cancel(WaiterId),
    Shutdown { reply: oneshot::Sender<()> },
    ConnectFinished { id: ConnectionId, key: OriginKey, result: Result<IdleConnection, ConnectError> },
    DriverClosed(ConnectionId),
}
```

- [ ] Keep all counts and FIFO queues actor-owned. Spawn connection work and report completion; never await network work inside the actor.
- [ ] Use two lanes: a bounded ordinary `mpsc::channel` for Acquire/Return and an unbounded control/lifecycle channel for exactly-once waiter `Cancel`, the single owner-issued `Shutdown`, `ConnectFinished`, and `DriverClosed`. Use `tokio::select! { biased; ... }` to service control first; cancellation, shutdown delivery, spawned network completion, and synchronous drop never wait on or use `try_send` against the bounded lane.
- [ ] Give every acquire a unique waiter ID and shared `AcquireTicket` with atomic `Pending`, `Cancelled`, and `Resolved` states. Arm its drop guard only after the bounded ordinary send succeeds. Dropping races `Pending -> Cancelled` and emits one priority `Cancel` only on success; every terminal manager result races `Pending -> Resolved` before sending a lease or acquisition error. Check ticket state when dequeuing Acquire and before later admission/timeout, so Cancel may overtake Acquire without a tombstone.
- [ ] Prove the unbounded API lane is logically bounded: only successfully enqueued ordinary acquires and at most 128 admitted waiter IDs can emit one cancellation before resolution; no more than 64 manager-owned connection IDs exist; the driver start latch prevents one connection ID from having `ConnectFinished` and `DriverClosed` outstanding together; and only one `Shutdown` producer exists. Document this invariant beside the channel construction.
- [ ] Test cancellation overtaking its Acquire, cancellation while queued with the ordinary lane full, and simultaneous races against admission and timeout. Every case resolves the ticket and manager accounting once; a resolved ticket cannot emit a later stale cancellation.
- [ ] Store production defaults in injectable `PoolLimits`; permit harness-only cap variants and pool-disabled baseline mode without adding CLI flags.
- [ ] Make capacity driver-owned: in actor tests, closing a lease emits an abort request but only a synthetic `DriverClosed(ConnectionId)` decrements live counts. Defer the real socket/upload assertion to Task 7 after the connector and body wrapper exist.
- [ ] Use one actor-owned next-deadline timer branch for waiter acquisition deadlines and idle expiry rather than channel messages or a task per entry. Add paused-time assertions at 29/30 seconds for waiters and 59/60 seconds for idle connections. Idle expiry removes the entry and requests abort but does not decrement/admit until `DriverClosed`.
- [ ] Add actor-shutdown tests proving priority `Shutdown` is observed with the ordinary lane full, all waiters are failed, every pending connector and live driver is aborted, queues/idle maps are cleared, and counts reach zero only through synthetic `ConnectFinished`/`DriverClosed` events.
- [ ] Implement a Closing state: `Shutdown` rejects new Acquire commands, fails queued waiters, aborts every connector and driver, and continues processing unbounded lifecycle events. Reconcile every `ConnectFinished` by its reserved `ConnectionId`, never by key; failure/cancellation releases that exact connecting count, while success publishes nothing, aborts the new driver, and waits for matching `DriverClosed`. Acknowledge only when connecting and driver counts are both zero.
- [ ] Add shutdown-race tests for both successful and failed `ConnectFinished` after Closing begins; prove no connection reaches a waiter and capacity is reconciled exactly once.
- [ ] Add both message-order tests: `Return` then `DriverClosed` removes an idle connection once; priority `DriverClosed` then stale `Return` discards the sender without another decrement or invariant failure.
- [ ] Saturate the bounded ordinary channel in a paused-time actor test, enqueue priority `Shutdown`, mass-abort drivers, and prove shutdown is observed promptly, every lifecycle event is received, and zero-live acknowledgment succeeds.
- [ ] Run manager tests. Expected: GREEN without wall-clock sleeps.
- [ ] Commit as `Add bounded dev proxy upstream manager`.

---

### Task 4: Reusable HTTP/1 Connection Factory

**Files:** Create `upstream/connect.rs`; modify `manager.rs`, `metrics.rs`, and test support.

- [ ] Write failing connector tests for plaintext/TLS separation, exact SNI, secure/insecure validation, `--resolve`, actual peer diagnostics, driver-health transition, and total multi-address timeout.
- [ ] Fake DNS must consume part of the deadline; prove each remaining address receives at most `remaining / addresses_left` and no attempt extends the original deadline.
- [ ] Run focused tests. Expected: RED because `ConnectionFactory` is absent.
- [ ] Define `ProxyRequestBody = BoxBody<Bytes, ProxyBodyError>` in `upstream/mod.rs`, where `ProxyBodyError` wraps `hyper::Error` and represents local cancellation. Then implement a narrow injectable factory returning an opened HTTP/1 sender, driver health, explicit abort handle, driver drop guard, connection ID, and actual peer.
- [ ] Allocate `ConnectionId`, register a Connecting reservation, and retain a connector abort handle in the actor before spawning network work. Give every connector an exactly-once completion guard: normal completion sends its `ConnectFinished` result and disarms it; cancellation, actor-requested abort, or unwind synchronously sends cancelled completion on drop. Start every successful Hyper driver behind a one-shot latch: enqueue completion, let the actor transition the exact reservation to Live/Closing, then release the latch. With simultaneous connects for one key, prove success/failure affects only its ID; abort and unwind tests must reconcile Connecting once, and `DriverClosed` must never arrive before registration.
- [ ] Preserve IP-literal compatibility: DNS identities send SNI and validate DNS SAN; IP identities send no DNS SNI and validate IP SAN. Mark IP rules HTTP/1-required.
- [ ] Compute one deadline before resolution. Record DNS lookup duration, TCP attempt before `connect`, established/connect duration after success, TLS handshake duration after TLS success, and HTTP handshake duration after Hyper success.
- [ ] Spawn one Hyper driver per opened connection and report termination to the actor.
- [ ] Run connector tests and existing `--resolve` E2E coverage. Expected: GREEN.
- [ ] Commit as `Open reusable dev proxy upstream connections`.

---

### Task 5: Streaming Lease State Machine

**Files:** Create `upstream/body.rs`; modify `upstream/{mod.rs,manager.rs}`.

- [ ] Write scripted-body tests proving request state becomes Complete only after terminal EOS including trailers, and Failed on body error or drop before EOS.
- [ ] Add a backpressure test whose scripted request body counts polls and exposes one frame only when downstream demand arrives; assert the adapter neither pre-polls nor buffers later frames.
- [ ] Write failing response tests for DATA/trailer passthrough; release after terminal `None`; close intent; driver/body/upload failure; downstream drop; response EOS before upload completion; full/closed return channel; and idempotent finalization.
- [ ] Run body tests. Expected: RED because adapters are absent.
- [ ] Implement a streaming request body wrapper and one erased request type:

```rust
pub type ProxyRequestBody = BoxBody<Bytes, ProxyBodyError>;
enum UploadState { Streaming, Complete, Failed }
```

- [ ] Implement `PooledResponseBody: Body<Data = Bytes, Error = hyper::Error>`. It owns an optional lease/abort handle and finalizes once. Return only at response EOS when upload is already Complete, driver healthy, and no close intent.
- [ ] If response EOS arrives while upload remains Streaming, forward EOF immediately and close; never wait or drain in the background.
- [ ] Closing invokes driver abort and drops the sender, but does not decrement manager capacity; the driver drop guard reports termination and releases capacity.
- [ ] Bound the nonblocking return channel to 64; on full/closed, close rather than blocking `poll_frame`.
- [ ] Run body tests. Expected: GREEN.
- [ ] Commit as `Make pooled response leases streaming-safe`.

---

### Task 6: Integrate HTTP/1 Pooling

**Files:** Modify proxy `mod.rs`, `server.rs`, `upstream/mod.rs`, test support and existing E2E tests; create `proxy_pool_e2e.rs`.

- [ ] First write a failing smoke test sending two sequential GETs over one MITM tunnel and expecting one established upstream connection/handshake. Run it and observe RED with two connections.
- [ ] Introduce shared state:

```rust
pub struct ProxyState {
    config: Arc<ResolvedConfig>,
    upstream: UpstreamClient,
    metrics: Arc<ProxyMetrics>,
}
```

- [ ] Build `ProxyState` once in `proxy::run`; pass `Arc<ProxyState>` into `serve_on`. Update test spawning helpers. Hyper service closures clone this Arc, not rules/auth/resolve maps.
- [ ] Wire initial-head parse duration and manager acquisition/queue timing into the shared metrics at their actual boundaries.
- [ ] Wire every core metric at its boundary: accepted browser connections; parsed and rejected initial heads plus parse duration; CA hit/miss; DNS lookup duration; TCP attempt/established/connect duration; TLS and Hyper handshake durations; negotiated HTTP/1 count; pool hit/miss/stale/retry; pool acquisition/queue wait; request-to-header; request completion/failure. Add a snapshot integration test that drives one success, one rejected head, and one failure and checks every core category's expected delta. Task 9 adds mint metrics; Tasks 12/13 add retained-experiment metrics.
- [ ] On every clean shutdown path, emit the redacted debug summary after Safari restoration. Add a formatting test proving it contains counts/timings but no URL query, auth value, certificate data, or sensitive headers.
- [ ] Implement Ctrl-C ordering explicitly: restore Safari/system PAC first while the proxy is alive; abort/stop the accept-loop task; enqueue manager `Shutdown` without waiting on the priority control/lifecycle lane; place the entire acknowledgment wait under a fixed two-second Tokio timeout; emit current metrics on success or timeout; then return so runtime teardown reaps leftovers. Never await manager drain before Safari restoration.
- [ ] Delegate mapped traffic to `UpstreamClient`. Keep blind tunnels, stray plain-HTTP forwarding, local PAC, Host-based per-request routing, and `421` behavior unchanged.
- [ ] Capture upstream close intent before response hop-by-hop sanitation, then wrap the response body with its lease.
- [ ] Make the two-request smoke GREEN, then add the 100-request acceptance case and run all existing proxy E2E tests. Expected: one established connection/handshake and unchanged routing/security behavior.
- [ ] Add a test around proxy startup/output capture proving `--insecure` still emits its warning before serving. Do not weaken or relocate the warning behind debug logging.
- [ ] Commit as `Reuse upstream HTTP/1 connections`.

---

### Task 7: Concurrency, Cancellation, and Bounds E2E

**Files:** Modify `manager.rs`, `body.rs`, `tests/support/mod.rs`, and `proxy_pool_e2e.rs`.

- [ ] Write a failing test for 100 GETs at concurrency 20 against a 25 ms upstream. Require observed concurrency between two and six, at most six manager-owned live connections, and at most two idle afterward.
- [ ] Write separate failing tests for upstream `Connection: close`, response body error, response trailers, slow/infinite response cancellation, truncated upload, and an origin responding before consuming the upload.
- [ ] Add manager-selection/header integration cases: TLS versus plaintext, secure versus insecure, pinned versus DNS, different TO identities, and different ports never share; two FROM rules sharing one TO do share when allowed; Host-preserved and Host-rewritten configurations send exact SNI/Host/forwarding headers; one request's Authorization never persists onto the next reused request.
- [ ] Prove unmatched blind tunnels and stray plain-HTTP forwarding bypass the manager and do not consume/decrement its 64 mapped-connection count.
- [ ] For every non-reusable case, require the next request to open a fresh connection.
- [ ] In the early-response case, require immediate browser response, driver abort, upload polling to stop, socket closure, and no live-capacity release until the driver drop guard reports termination.
- [ ] Add an adversarial shutdown orchestration test with injected hooks: saturate ordinary manager commands and delay one lifecycle close beyond two seconds. Assert Safari restoration is invoked first, new accepts stop next, priority shutdown is observed despite saturation, shutdown returns at the deadline, and current metrics are emitted.
- [ ] Add bidirectional large streaming E2E cases (for example 2 MiB in small deterministic chunks): a successful chunked upload captured byte-for-byte at the upstream, and a chunked response read byte-for-byte at the browser. Assert incremental polling/backpressure on both paths rather than whole-body buffering.
- [ ] Add a real proxy-shutdown test proving active/idle mapped drivers are aborted, sockets close, bounded queues are discarded, and the manager task exits without retained state.
- [ ] Run `proxy_pool_e2e`. Task 5 unit tests may make lifecycle cases GREEN immediately; treat that as acceptance evidence. If an integration case fails, verify the failure is the intended missing behavior before changing production code.
- [ ] Implement only the failing lifecycle transitions. Never background-drain; admit waiters only after driver termination decrements capacity.
- [ ] Rerun `proxy_pool_e2e`. Expected: GREEN without timing sleeps except controlled upstream delays.
- [ ] Commit as `Harden dev proxy pool lifecycle`.

---

### Task 8: Conservative Stale Retry

**Files:** Modify `upstream/{mod.rs,body.rs}`, `metrics.rs`, and `proxy_pool_e2e.rs`.

- [ ] Write failing replayability tests. Capture eligibility from the original request before sanitation. Require GET/HEAD/OPTIONS, absent Content-Length and Transfer-Encoding, exact zero size, `is_end_stream()`, and an empty extension map.
- [ ] Explicitly reject unknown/lying hints, zero-data trailers, any extension, framed/streaming/truncated bodies, and other methods.
- [ ] Write failing E2E cases: sender readiness closes before body consumption; reused send fails before headers for eligible GET; ineligible requests do not retry; maximum one retry; final failure returns one `502` without closing the browser tunnel.
- [ ] Run focused tests. Expected: RED because no retry path exists.
- [ ] For eligible requests only, retain method, URI, version and headers and substitute a reusable empty body. Do not clone or buffer `Incoming`.
- [ ] Retry readiness failure with the unconsumed original request. Retry post-dispatch failure only from the retained template and only on a reused connection.
- [ ] Add stale/retry metrics, rerun focused tests and the full E2E binary. Expected: GREEN.
- [ ] Commit as `Retry stale pooled proxy connections safely`.

---

### Task 9: Normalize and Prewarm Certificates

**Files:** Modify `ca.rs`, proxy `mod.rs`, `server.rs`, metrics, and pool E2E tests.

- [ ] Write a failing test that prewarms lowercase `www.example.com`, CONNECTs as mixed-case `WWW.Example.COM`, and expects one mint total plus a runtime hit.
- [ ] Add failing tests for duplicate-rule deduplication and prewarm failure before browser launch.
- [ ] Run focused tests. Expected: RED because raw CONNECT case forms a distinct cache key.
- [ ] Normalize DNS CONNECT identities to lowercase before CA leaf lookup/cache insertion; `RuleTable::first_match` is already case-insensitive and needs no behavior change. Preserve parsed IP identity without string-case logic.
- [ ] Prewarm every unique normalized FROM before listener/browser startup. Instrument hits, misses, leaf mint duration, and unexpected post-prewarm mints without logging key material; assert snapshot deltas.
- [ ] Run `./scripts/test-cli.sh`. Expected: GREEN.
- [ ] Commit as `Prewarm normalized dev proxy certificates`.

---

### Task 10: Measure and Lock HTTP/1 v1

**Files:** Modify `proxy_perf.rs` and implementation notes.

- [ ] Run deterministic pool tests. Expected: exactly one established connection/handshake for sequential work and all bounds/lifecycle tests pass.
- [ ] Run two warmups and ten alternating baseline/pooled measurements for the sequential and matched-concurrency-six workloads; record raw runs, median, p95, and median absolute deviation. Run concurrency 20 as a bounds/queueing diagnostic only, because comparison with the unbounded baseline would measure the intentional six-connection cap rather than manager overhead.
- [ ] Add and run the injectable exact remote model: 100 zero-body GETs with no `Content-Length` or `Transfer-Encoding`, immediate request EOS, concurrency 20, 30 ms connect delay, 30 ms TLS delay, and 25 ms response delay. Upstream responses use keep-alive plus explicit `Content-Length`; never use close-delimited framing. Use harness-only `PoolMode` and `PoolLimits`; expose no CLI flags.
- [ ] Run `cargo test --package trusted-server-cli --target "$(rustc -vV | awk '/host:/ { print $2 }')" --test proxy_perf perf_http1_comparison -- --ignored --nocapture --test-threads=1`.
- [ ] Record entry evidence for parser, DNS, and HTTP/2; label those stages `ENTER` or `SKIP`. `TCP_NODELAY` is always measured in Task 14 and is labeled only `RETAIN` or `REJECT`. For HTTP/2, calculate the spec's cold/preconnected and cap-6/cap-20 ratios from median durations.
- [ ] Run `perf_allocation_comparison` with harness-only `PoolMode::Disabled` for both baseline-compatible and precomputed variants so pooling gains cannot mask allocation effects. Record process CPU and total duration, or a written simplification justification if timing is neutral.
- [ ] Confirm HTTP/1 pooling does not regress median or p95 more than 5% in the sequential and matched-concurrency-six comparisons and adds no failures in any workload.
- [ ] State explicitly in the notes: localhost success is primarily 100-to-1 connection/handshake reduction; material wall-clock gains are expected and judged on the remote/staging latency model.
- [ ] Commit notes as `Record dev proxy HTTP/1 performance results`.

---

### Task 11: Conditional Buffered Initial-Head Parsing

**Entry gate:** In a single-threaded workload of 1,000 new loopback connections that each send one local PAC request and read its response, aggregated parse duration is at least 5% of total client-observed duration. Otherwise record `SKIP` and create no parser code.

**Files if entered:** Create `prefixed_io.rs`; modify `server.rs`, metrics, pool E2E, perf harness, and notes.

- [ ] Measure and record the exact entry gate.
- [ ] Run `cargo test --package trusted-server-cli --target "$(rustc -vV | awk '/host:/ { print $2 }')" --test proxy_perf perf_parser_local -- --ignored --nocapture --test-threads=1` and calculate aggregate parse duration divided by total duration.
- [ ] If entered, write failing tests for delimiter splits, exactly 8 KiB head, oversized/incomplete head, a valid sub-8-KiB head whose over-read makes the combined buffer exceed 8 KiB, and exact-once prefix delivery.
- [ ] Write failing path tests proving over-read reaches browser TLS accept, blind tunnel, and plain-HTTP forwarding exactly once.
- [ ] Implement chunk reads and one `PrefixedIo<T>` owner. Apply the 8 KiB cap only through `\r\n\r\n`; every downstream path consumes the same adapter and no path separately replays bytes.
- [ ] Run correctness tests and ten alternating performance runs. Retain only if the 5% gate remains satisfied with no regression.
- [ ] If retained, commit `Buffer dev proxy initial request parsing`; otherwise remove experiment code and commit notes as `Reject buffered proxy parsing experiment`.

---

### Task 12: Conditional DNS Cache

**Entry gate:** Post-pooling DNS metrics are at least 5% of median request-to-upstream-header time, or a no-cache fixed-delay resolver versus no-cache zero-delay resolver changes the churn workload by at least 5%. Otherwise record `SKIP` and do not create `dns.rs`.

**Files if entered:** Create `upstream/dns.rs`; modify `connect.rs`, `key.rs`, metrics, tests, perf harness, and notes.

- [ ] Evaluate existing post-pooling DNS metrics first. If below 5%, define `perf_dns_lookup_contribution` as 100 requests that force fresh manager connections and compare no-cache fixed-delay resolution with no-cache zero-delay resolution.
- [ ] Run `cargo test --package trusted-server-cli --target "$(rustc -vV | awk '/host:/ { print $2 }')" --test proxy_perf perf_dns_lookup_contribution -- --ignored --nocapture --test-threads=1`. Only enter implementation if either non-circular entry arm reaches 5%.
- [ ] With paused time and a fake resolver, write failing tests for 30-second TTL, 64-entry bound, expired-first eviction, non-in-flight LRU eviction, all-in-flight bypass, concurrent miss coalescing, owned error fan-out, no failure caching, and `--resolve` bypass.
- [ ] Add failing identity tests proving multiple peer IPs share one DNS origin key, different TO identities never share, and healthy idle connections may outlive DNS TTL only until their 60-second idle deadline.
- [ ] Implement cache state without awaiting resolution under its lock. Store owned address lists; reconstruct equivalent I/O errors for waiters.
- [ ] Record DNS cache hits, misses, and lookup duration; add snapshot assertions for retained cache variants.
- [ ] Prove one total DNS/connect deadline and fair per-address remaining slices with fake time.
- [ ] After implementation, run `perf_dns_cache_retention` with cache-disabled/cache-enabled variants for ten alternating measurements. Retain only if the original 5% evidence remains and failures do not increase.
- [ ] Commit retained code as `Cache dev proxy DNS results`, or remove it and commit notes as `Reject dev proxy DNS cache experiment`.

---

### Task 13: Conditional Upstream HTTP/2

**Entry gate:** In the exact remote model, the median cold-versus-preconnected setup ratio or cap-6-versus-cap-20 concurrency ratio defined in the spec is at least 10% across ten alternating runs. Otherwise record `SKIP` and do not enable Hyper `http2`.

**Files if entered:** Modify CLI Cargo.toml, all upstream modules, test support, pool E2E, perf harness, and notes.

- [ ] Measure and record the entry gate.
- [ ] Run `cargo test --package trusted-server-cli --target "$(rustc -vV | awk '/host:/ { print $2 }')" --test proxy_perf perf_h2_entry -- --ignored --nocapture --test-threads=1`; this internally alternates cold/preconnected and cap-6/cap-20 variants.
- [ ] Temporarily enable Hyper `http2` and add a narrowly scoped `http2_feasibility` test target. This is prerequisite scaffolding, not a production behavior claim.
- [ ] Prove maintained public APIs can: abort an upload after response headers through `ProxyBodyError`; observe confirmed local stream termination; preserve required informational/trailer frames; classify GOAWAY/`REFUSED_STREAM`; and expose current/dynamically lowered peer stream capacity (or an equivalent protocol signal) so combined manager plus Hyper-internal waiters can be kept within 32 per origin and 128 globally. `SendRequest::ready()` is insufficient. If any proof is impossible, record `REJECT` immediately.
- [ ] On feasibility rejection, remove the Cargo feature, proof tests, fixtures, harness variants, and all HTTP/2 scaffolding before committing notes; skip the remaining steps.
- [ ] If feasible, write failing behavior tests for four TLS configurations, serialized cold discovery, exact authority/SNI/path/query, no cross-name reuse, stream-permit lifecycle, and protocol semantics.
- [ ] Write explicit failing global-bound tests: a 33rd non-draining h2 connection is not opened; draining connections continue consuming the 64 manager-owned live slots; replacement remains blocked at 64 until a driver-exit event releases capacity.
- [ ] Only if feasibility exposes adequate peer-capacity accounting, implement candidate active capacity `min(100, peer limit)` and keep all remaining requests in the bounded manager queues rather than a hidden Hyper queue. Hold active permits through response/reset termination and react to dynamic SETTINGS reductions.
- [ ] Test both layers: with peer max above 100, request 101 stays in the manager queue; with peer max 3, request 4 stays in the manager queue; dynamic lowering moves no extra work into an unbounded internal queue. Assert combined per-origin/global waiter bounds and cancellation.
- [ ] Test `REFUSED_STREAM` retries for a non-idempotent request only when its complete body is reconstructable; prove a consumed streaming body is never retried.
- [ ] With upload still streaming, test response EOS, downstream cancellation, response body error, and upload failure. Each must request reset, retain its permit until the termination guard fires, and keep a 101st request queued until confirmed termination.
- [ ] Implement `Vacant`, `Discovering`, `Http1`, `Http2`, and `Draining` manager states with one creator and exact bounds; implement only enough to turn each behavior test green.
- [ ] Record negotiated HTTP/2 connections, active/completed streams, and connection replacements; add snapshot assertions before retention measurement.
- [ ] After two warmups, run ten alternating HTTP/1/HTTP/2 variants. Retain only with at least 10% median duration/throughput improvement, p95 regression no worse than 5%, and no additional failures.
- [ ] Run the retained-protocol comparison with `cargo test --package trusted-server-cli --target "$(rustc -vV | awk '/host:/ { print $2 }')" --test proxy_perf perf_h2_retention -- --ignored --nocapture --test-threads=1`.
- [ ] On any retention rejection, remove HTTP/2 production code, Cargo features, proof/behavior tests, fixtures, harness variants, and scaffolding before committing notes. Do not leave dormant complexity.
- [ ] If retained, commit as `Add upstream HTTP/2 to dev proxy`; if rejected, commit notes as `Reject dev proxy HTTP/2 experiment`.

---

### Task 14: Conditional `TCP_NODELAY`

**Files if retained:** Modify `server.rs`, `connect.rs`, metrics, perf harness, tests, and notes.

- [ ] Run this task unconditionally; there is no entry-gate `SKIP` outcome.
- [ ] Write failing injected-option tests proving application immediately after accept/connect and before TLS/HTTP, success/failure counters, one warning per socket class, and nonfatal failure.
- [ ] Implement independently selectable `off_off`, `browser_on`, `upstream_on`, and `both_on` variants through `TS_PERF_VARIANT`, used only by the harness; add no CLI flag.
- [ ] Use the named sequential TLS workload. After two complete warmup rounds, run each setting as a separate process ten times in round-robin order. Example command: `/usr/bin/time -p env TS_PERF_VARIANT=browser_on cargo test --package trusted-server-cli --target "$(rustc -vV | awk '/host:/ { print $2 }')" --test proxy_perf perf_tcp_nodelay -- --ignored --nocapture --test-threads=1`. Repeat with each variant value so CPU evidence is per variant.
- [ ] Compare `browser_on` and `upstream_on` independently with `off_off`; each retained setting requires at least 3% median improvement, p95 regression no worse than 5%, and per-process CPU regression no worse than 5%. If both pass, require `both_on` to preserve at least 3% median improvement and both regression limits; otherwise retain only the passing one-sided variant with the larger median benefit.
- [ ] Remove option abstractions for rejected variants. Commit retained code or notes-only rejection.
- [ ] Use `Tune dev proxy TCP latency` for retained settings or `Reject dev proxy TCP_NODELAY experiment` for notes-only rejection.

---

### Task 15: Final Documentation and Verification

**Files:** Complete implementation notes; modify `docs/guide/ts-dev-proxy.md` only for observable behavior; review every changed CLI/test file.

- [ ] Document exact commands, machine/toolchain, raw results, median/p95/MAD, connection/handshake counts, constants, every entry/retention decision, and rejected-code cleanup.
- [ ] If existing debug logging exposes the metrics summary, add one concise troubleshooting note; do not document internal pool knobs or promise stable diagnostics.
- [ ] Run `cargo fmt --all -- --check`. Expected: PASS.
- [ ] Run `cd docs && npm ci && npm run format`. Expected: dependencies install from the lockfile and formatting passes without unrelated changes.
- [ ] Run `./scripts/test-cli.sh`. Expected: all CLI tests PASS.
- [ ] Run host clippy:

```bash
cargo clippy --package trusted-server-cli --target "$(rustc -vV | awk '/host:/ { print $2 }')" --all-targets -- -D warnings
```

- [ ] Run the retained ignored performance workloads single-threaded and compare to recorded variability.
- [ ] Audit all 14 security invariants against code and tests, including DNS/IP reference identity, no cross-SNI reuse, driver-owned capacity, no streaming-body replay, redacted auth, and bounded state.
- [ ] Commit final docs as `Document dev proxy performance results`.
- [ ] Use `superpowers:requesting-code-review` on the complete branch. Resolve every critical/important issue and rerun affected verification until approved.
