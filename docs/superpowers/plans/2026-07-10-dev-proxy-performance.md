# Dev Proxy Performance Optimization Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `ts dev proxy` materially faster by reusing upstream HTTP/1.1 connections, then retain optional parser, DNS, HTTP/2, and socket optimizations only when measurements clear the approved gates.

**Architecture:** Create a shared `ProxyState` and bounded upstream-manager actor. The actor owns exact origin-key isolation, FIFO admission, connection opening, sender leases, and idle return; body adapters preserve streaming and return a lease only when request upload and response download both completed successfully. Deterministic counters prove correctness before manual timing comparisons.

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

- [ ] Run `./scripts/test-cli.sh`. Expected: baseline PASS; stop if it does not.
- [ ] Write failing `metrics.rs` tests requiring separate TCP-attempt/established counters and a fixed-size timing histogram.

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

- [ ] Run `cargo test --package trusted-server-cli --target "$(rustc -vV | awk '/host:/ { print $2 }')" metrics::tests`. Expected: RED because metrics do not exist.
- [ ] Implement `ProxyMetrics` with `AtomicU64` fields and fixed atomic duration buckets, including methods for initial-head parse, pool acquisition, and queue wait. Expose `snapshot`, phase-recording methods, and a redacted debug summary; retain no per-request labels or samples. Task 1 uses fixture counters for the baseline; runtime phase wiring occurs when `ProxyState` reaches `server.rs` in Task 6.
- [ ] Extend the TLS upstream fixture with shared accepted-connection, request, handshake, and failure counters.
- [ ] Add two baseline `#[ignore = "manual performance workload"]` tests: 100 sequential TLS GETs and 100 delayed GETs at concurrency 20. Add the injectable remote-latency model in Task 10 after the connection factory exists.
- [ ] Run the ignored harness with `--ignored --nocapture --test-threads=1`. Expected baseline: approximately 100 established upstream connections and handshakes for 100 sequential requests.
- [ ] Record raw output, machine, OS, Rust version, and command in implementation notes.
- [ ] Run `./scripts/test-cli.sh` and commit as `test(cli): establish dev proxy performance baseline`.

---

### Task 2: Safe Precomputed Origin Identity

**Files:** Create `upstream/{mod.rs,key.rs}`; modify `config.rs`, `rewrite.rs`, and proxy `mod.rs`.

- [ ] Write failing key tests varying transport, normalized TO/SNI host, port, verify mode, application mode, DNS policy, and `--resolve` pin independently.
- [ ] Add a test proving two peer IPs selected for one DNS origin do not fragment the logical key, and two TO names sharing one IP never compare equal.
- [ ] Run focused tests. Expected: RED because `OriginKey` does not exist.
- [ ] Implement closed key types:

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

- [ ] Write failing tests requiring prevalidated `HeaderValue`s, `ServerName<'static>`, and a Basic-auth Debug output containing neither username, password, nor encoded token.
- [ ] Replace `BasicAuth { user, pass }` with a private reusable header and custom `Debug` rendering `BasicAuth([REDACTED])`. Update fixtures to construct it through a checked constructor.
- [ ] Precompute rule host/forwarding headers, SNI, transport, and stable origin-key fields during `config::resolve`; eliminate per-request Base64 and host-header formatting.
- [ ] Run config/rewrite/key tests and existing E2E tests. Expected: GREEN with unchanged behavior.
- [ ] Commit as `refactor(cli): precompute dev proxy upstream identity`.

---

### Task 3: Bounded Manager Actor

**Files:** Create `upstream/manager.rs`; modify `upstream/mod.rs` and CLI Cargo.toml.

- [ ] Add Tokio `test-util` only to the macOS dev-dependency feature set.
- [ ] With paused time and a test harness that observes `ConnectRequested` commands and manually replies `Connected`, write failing tests for: six live HTTP/1 connections per origin, 64 globally, two idle per origin, 32 idle globally, 32 queued per origin, 128 queued globally, FIFO wakeup, dropped-waiter removal, and 30-second timeout.
- [ ] Add a test proving origin/global admission is atomic and never reserves one capacity while waiting for the other.
- [ ] Run manager tests. Expected: RED because the actor is absent.
- [ ] Implement one bounded command actor:

```rust
enum Command {
    Acquire { key: OriginKey, reply: oneshot::Sender<Result<Lease, AcquireError>> },
    Connected { key: OriginKey, result: Result<IdleConnection, ConnectError> },
    Return(IdleConnection),
    DriverClosed(ConnectionId),
    Cancel(WaiterId),
    Expire(Instant),
}
```

- [ ] Keep all counts and FIFO queues actor-owned. Spawn connection work and report completion; never await network work inside the actor.
- [ ] Store production defaults in injectable `PoolLimits`; permit harness-only cap variants and pool-disabled baseline mode without adding CLI flags.
- [ ] Make capacity driver-owned: in actor tests, closing a lease emits an abort request but only a synthetic `DriverClosed(ConnectionId)` decrements live counts. Defer the real socket/upload assertion to Task 7 after the connector and body wrapper exist.
- [ ] Use one next-deadline timer rather than a task per idle connection. Expiry removes the idle entry and requests driver abort, but does not decrement or admit waiters until `DriverClosed`. Add a paused-time test proving no admission in that interval.
- [ ] Run manager tests. Expected: GREEN without wall-clock sleeps.
- [ ] Commit as `feat(cli): add bounded dev proxy upstream manager`.

---

### Task 4: Reusable HTTP/1 Connection Factory

**Files:** Create `upstream/connect.rs`; modify `manager.rs`, `metrics.rs`, and test support.

- [ ] Write failing connector tests for plaintext/TLS separation, exact SNI, secure/insecure validation, `--resolve`, actual peer diagnostics, driver-health transition, and total multi-address timeout.
- [ ] Fake DNS must consume part of the deadline; prove each remaining address receives at most `remaining / addresses_left` and no attempt extends the original deadline.
- [ ] Run focused tests. Expected: RED because `ConnectionFactory` is absent.
- [ ] Define `ProxyRequestBody = BoxBody<Bytes, ProxyBodyError>` in `upstream/mod.rs`, where `ProxyBodyError` wraps `hyper::Error` and represents local cancellation. Then implement a narrow injectable factory returning an opened HTTP/1 sender, driver health, explicit abort handle, driver drop guard, connection ID, and actual peer.
- [ ] Preserve IP-literal compatibility: DNS identities send SNI and validate DNS SAN; IP identities send no DNS SNI and validate IP SAN. Mark IP rules HTTP/1-required.
- [ ] Compute one deadline before resolution. Count TCP attempt before `connect`, established after success, TLS handshake after TLS success, and HTTP handshake after Hyper success.
- [ ] Spawn one Hyper driver per opened connection and report termination to the actor.
- [ ] Run connector tests and existing `--resolve` E2E coverage. Expected: GREEN.
- [ ] Commit as `feat(cli): open reusable dev proxy upstream connections`.

---

### Task 5: Streaming Lease State Machine

**Files:** Create `upstream/body.rs`; modify `upstream/{mod.rs,manager.rs}`.

- [ ] Write scripted-body tests proving request state becomes Complete only after terminal EOS including trailers, and Failed on body error or drop before EOS.
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
- [ ] Commit as `feat(cli): make pooled response leases streaming-safe`.

---

### Task 6: Integrate HTTP/1 Pooling

**Files:** Modify proxy `mod.rs`, `server.rs`, `upstream/mod.rs`, test support and existing E2E tests; create `proxy_pool_e2e.rs`.

- [ ] Write a failing test that sends 100 sequential GETs over one MITM tunnel and expects 100 responses, one established upstream TCP connection, and one TLS handshake.
- [ ] Run `proxy_pool_e2e`. Expected: RED with about 100 upstream connections on the current path.
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
- [ ] Wire every mandatory metric at its boundary: CA hit/miss/mint; DNS lookup; TCP attempt/established/connect duration; TLS and Hyper handshake; pool hit/miss/stale/retry; request-to-header; request completion/failure. Add a snapshot integration test that drives one success and one failure and checks the expected deltas.
- [ ] On every clean shutdown path, emit the redacted debug summary after Safari restoration. Add a formatting test proving it contains counts/timings but no URL query, auth value, certificate data, or sensitive headers.
- [ ] Delegate mapped traffic to `UpstreamClient`. Keep blind tunnels, stray plain-HTTP forwarding, local PAC, Host-based per-request routing, and `421` behavior unchanged.
- [ ] Capture upstream close intent before response hop-by-hop sanitation, then wrap the response body with its lease.
- [ ] Run sequential reuse and all existing proxy E2E tests. Expected: one established connection/handshake and unchanged routing/security behavior.
- [ ] Commit as `feat(cli): reuse upstream HTTP/1 connections`.

---

### Task 7: Concurrency, Cancellation, and Bounds E2E

**Files:** Modify `manager.rs`, `body.rs`, `tests/support/mod.rs`, and `proxy_pool_e2e.rs`.

- [ ] Write a failing test for 100 GETs at concurrency 20 against a 25 ms upstream. Require observed concurrency between two and six, at most six manager-owned live connections, and at most two idle afterward.
- [ ] Write separate failing tests for upstream `Connection: close`, response body error, response trailers, slow/infinite response cancellation, truncated upload, and an origin responding before consuming the upload.
- [ ] Add manager-selection/header integration cases: TLS versus plaintext, secure versus insecure, pinned versus DNS, different TO identities, and different ports never share; two FROM rules sharing one TO do share when allowed; Host-preserved and Host-rewritten configurations send exact SNI/Host/forwarding headers; one request's Authorization never persists onto the next reused request.
- [ ] For every non-reusable case, require the next request to open a fresh connection.
- [ ] In the early-response case, require immediate browser response, driver abort, upload polling to stop, socket closure, and no live-capacity release until the driver drop guard reports termination.
- [ ] Run `proxy_pool_e2e`. Task 5 unit tests may make lifecycle cases GREEN immediately; treat that as acceptance evidence. If an integration case fails, verify the failure is the intended missing behavior before changing production code.
- [ ] Implement only the failing lifecycle transitions. Never background-drain; admit waiters only after driver termination decrements capacity.
- [ ] Rerun `proxy_pool_e2e`. Expected: GREEN without timing sleeps except controlled upstream delays.
- [ ] Commit as `test(cli): harden dev proxy pool lifecycle`.

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
- [ ] Commit as `feat(cli): retry stale pooled proxy connections safely`.

---

### Task 9: Normalize and Prewarm Certificates

**Files:** Modify `ca.rs`, proxy `mod.rs`, `server.rs`, metrics, and pool E2E tests.

- [ ] Write a failing test that prewarms lowercase `www.example.com`, CONNECTs as mixed-case `WWW.Example.COM`, and expects one mint total plus a runtime hit.
- [ ] Add failing tests for duplicate-rule deduplication and prewarm failure before browser launch.
- [ ] Run focused tests. Expected: RED because raw CONNECT case forms a distinct cache key.
- [ ] Normalize DNS CONNECT identities to lowercase before rule and CA lookup; preserve parsed IP identity without string-case logic.
- [ ] Prewarm every unique normalized FROM before listener/browser startup. Instrument hits, misses, and unexpected post-prewarm mints without logging key material.
- [ ] Run `./scripts/test-cli.sh`. Expected: GREEN.
- [ ] Commit as `perf(cli): prewarm normalized dev proxy certificates`.

---

### Task 10: Measure and Lock HTTP/1 v1

**Files:** Modify `proxy_perf.rs` and implementation notes.

- [ ] Run deterministic pool tests. Expected: exactly one established connection/handshake for sequential work and all bounds/lifecycle tests pass.
- [ ] Run two warmups and ten alternating baseline/pooled measurements; record raw runs, median, p95, and median absolute deviation.
- [ ] Add and run the injectable exact remote model: 100 GETs, concurrency 20, 30 ms connect delay, 30 ms TLS delay, 25 ms response delay. Use harness-only `PoolMode` and `PoolLimits`; expose no CLI flags.
- [ ] Run `cargo test --package trusted-server-cli --target "$(rustc -vV | awk '/host:/ { print $2 }')" --test proxy_perf perf_http1_comparison -- --ignored --nocapture --test-threads=1`.
- [ ] Record entry evidence for parser, DNS, and HTTP/2; label those stages `ENTER` or `SKIP`. `TCP_NODELAY` is always measured in Task 14 and is labeled only `RETAIN` or `REJECT`. For HTTP/2, calculate the spec's cold/preconnected and cap-6/cap-20 ratios from median durations.
- [ ] Run `perf_allocation_comparison` with harness-only `PoolMode::Disabled` for both baseline-compatible and precomputed variants so pooling gains cannot mask allocation effects. Record process CPU and total duration, or a written simplification justification if timing is neutral.
- [ ] Confirm HTTP/1 pooling does not regress median or p95 more than 5% and adds no failures.
- [ ] Commit notes as `docs: record dev proxy HTTP/1 performance results`.

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
- [ ] If retained, commit `perf(cli): buffer dev proxy initial request parsing`; otherwise remove experiment code and commit notes as `docs: reject buffered proxy parsing experiment`.

---

### Task 12: Conditional DNS Cache

**Entry gate:** Post-pooling DNS metrics are at least 5% of median request-to-upstream-header time, or a no-cache fixed-delay resolver versus no-cache zero-delay resolver changes the churn workload by at least 5%. Otherwise record `SKIP` and do not create `dns.rs`.

**Files if entered:** Create `upstream/dns.rs`; modify `connect.rs`, `key.rs`, metrics, tests, perf harness, and notes.

- [ ] Evaluate existing post-pooling DNS metrics first. If below 5%, define `perf_dns_lookup_contribution` as 100 requests that force fresh manager connections and compare no-cache fixed-delay resolution with no-cache zero-delay resolution.
- [ ] Run `cargo test --package trusted-server-cli --target "$(rustc -vV | awk '/host:/ { print $2 }')" --test proxy_perf perf_dns_lookup_contribution -- --ignored --nocapture --test-threads=1`. Only enter implementation if either non-circular entry arm reaches 5%.
- [ ] With paused time and a fake resolver, write failing tests for 30-second TTL, 64-entry bound, expired-first eviction, non-in-flight LRU eviction, all-in-flight bypass, concurrent miss coalescing, owned error fan-out, no failure caching, and `--resolve` bypass.
- [ ] Add failing identity tests proving multiple peer IPs share one DNS origin key, different TO identities never share, and healthy idle connections may outlive DNS TTL only until their 60-second idle deadline.
- [ ] Implement cache state without awaiting resolution under its lock. Store owned address lists; reconstruct equivalent I/O errors for waiters.
- [ ] Prove one total DNS/connect deadline and fair per-address remaining slices with fake time.
- [ ] After implementation, run `perf_dns_cache_retention` with cache-disabled/cache-enabled variants for ten alternating measurements. Retain only if the original 5% evidence remains and failures do not increase.
- [ ] Commit retained code, or remove it and commit notes-only rejection.

---

### Task 13: Conditional Upstream HTTP/2

**Entry gate:** In the exact remote model, the median cold-versus-preconnected setup ratio or cap-6-versus-cap-20 concurrency ratio defined in the spec is at least 10% across ten alternating runs. Otherwise record `SKIP` and do not enable Hyper `http2`.

**Files if entered:** Modify CLI Cargo.toml, all upstream modules, test support, pool E2E, perf harness, and notes.

- [ ] Measure and record the entry gate.
- [ ] Run `cargo test --package trusted-server-cli --target "$(rustc -vV | awk '/host:/ { print $2 }')" --test proxy_perf perf_h2_entry -- --ignored --nocapture --test-threads=1`; this internally alternates cold/preconnected and cap-6/cap-20 variants.
- [ ] Temporarily enable Hyper `http2` and add a narrowly scoped `http2_feasibility` test target. This is prerequisite scaffolding, not a production behavior claim.
- [ ] Prove maintained public APIs can: abort an upload after response headers through `ProxyBodyError`, observe confirmed local stream termination, preserve required informational/trailer frames, and classify GOAWAY/`REFUSED_STREAM` sufficiently for the spec's retries. If any proof is impossible, record `REJECT` immediately.
- [ ] On feasibility rejection, remove the Cargo feature, proof tests, fixtures, harness variants, and all HTTP/2 scaffolding before committing notes; skip the remaining steps.
- [ ] If feasible, write failing behavior tests for four TLS configurations, serialized cold discovery, exact authority/SNI/path/query, no cross-name reuse, stream-permit lifecycle, and protocol semantics.
- [ ] With upload still streaming, test response EOS, downstream cancellation, response body error, and upload failure. Each must request reset, retain its permit until the termination guard fires, and keep a 101st request queued until confirmed termination.
- [ ] Implement `Vacant`, `Discovering`, `Http1`, `Http2`, and `Draining` manager states with one creator and exact bounds; implement only enough to turn each behavior test green.
- [ ] After two warmups, run ten alternating HTTP/1/HTTP/2 variants. Retain only with at least 10% median duration/throughput improvement, p95 regression no worse than 5%, and no additional failures.
- [ ] Run the retained-protocol comparison with `cargo test --package trusted-server-cli --target "$(rustc -vV | awk '/host:/ { print $2 }')" --test proxy_perf perf_h2_retention -- --ignored --nocapture --test-threads=1`.
- [ ] On any retention rejection, remove HTTP/2 production code, Cargo features, proof/behavior tests, fixtures, harness variants, and scaffolding before committing notes. Do not leave dormant complexity.

---

### Task 14: Conditional `TCP_NODELAY`

**Files if retained:** Modify `server.rs`, `connect.rs`, metrics, perf harness, tests, and notes.

- [ ] Run this task unconditionally; there is no entry-gate `SKIP` outcome.
- [ ] Write failing injected-option tests proving application immediately after accept/connect and before TLS/HTTP, success/failure counters, one warning per socket class, and nonfatal failure.
- [ ] Implement independently selectable `off_off`, `browser_on`, `upstream_on`, and `both_on` variants through `TS_PERF_VARIANT`, used only by the harness; add no CLI flag.
- [ ] After two warmups, run each setting as a separate process ten times in round-robin order. Example command: `/usr/bin/time -p env TS_PERF_VARIANT=browser_on cargo test --package trusted-server-cli --target "$(rustc -vV | awk '/host:/ { print $2 }')" --test proxy_perf perf_tcp_nodelay -- --ignored --nocapture --test-threads=1`. Repeat with each variant value so CPU evidence is per variant.
- [ ] Retain browser/upstream settings independently only with at least 3% median improvement, p95 regression no worse than 5%, and per-process CPU regression no worse than 5%.
- [ ] Remove option abstractions for rejected variants. Commit retained code or notes-only rejection.

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
- [ ] Commit final docs as `docs: document dev proxy performance results`.
- [ ] Use `superpowers:requesting-code-review` on the complete branch. Resolve every critical/important issue and rerun affected verification until approved.
