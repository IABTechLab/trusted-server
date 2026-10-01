# Dev Proxy PR Feedback Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Bring PR #896 back into conformance with the approved dev-proxy performance design and address every review comment with regression coverage and individual review replies.

**Architecture:** Keep the approved two-lane manager actor, bounded pool, acquire-ticket state machine, and origin identity. Move connecting-capacity ownership into the connector task until cancellation has actually completed, make returned-connection delivery recoverable across cancelled waiters, and centralize pre-response failure accounting. Preserve HTTP trailer semantics explicitly at the request-leg boundary; keep the remaining fixes local to DNS notification, aggregate diagnostics, performance-fixture selection, and documentation.

**Tech Stack:** Rust 2024, Tokio, Hyper HTTP/1, rustls/aws-lc-rs, error-stack, GitHub CLI.

**Primary specification:** `docs/superpowers/specs/2026-07-10-dev-proxy-performance-design.md`

---

## File map

- `crates/trusted-server-cli/src/commands/dev/proxy/upstream/connect.rs`: connector-task ownership, cancellation reconciliation, rustls scheme caching.
- `crates/trusted-server-cli/src/commands/dev/proxy/upstream/manager.rs`: recoverable FIFO handoff and connection reservation registration.
- `crates/trusted-server-cli/src/commands/dev/proxy/upstream/mod.rs`: exactly-once send outcome accounting, acquisition timing, stale POST safety documentation.
- `crates/trusted-server-cli/src/commands/dev/proxy/upstream/dns.rs`: retained completion publication after receiver cancellation.
- `crates/trusted-server-cli/src/commands/dev/proxy/server.rs`: safe upstream request `Trailer` and `TE: trailers` regeneration.
- `crates/trusted-server-cli/src/commands/dev/proxy/metrics.rs`: complete aggregate latency summary.
- `crates/trusted-server-cli/tests/support/mod.rs`: raw upstream fixtures for request trailers and unreachable origins.
- `crates/trusted-server-cli/tests/proxy_e2e.rs`: end-to-end trailer and terminal setup-failure assertions.
- `crates/trusted-server-cli/tests/proxy_perf.rs`: self-validating remote workload.
- `docs/superpowers/implementation-notes/2026-07-10-dev-proxy-performance.md`: exact remote benchmark commands and review corrections.

### Task 1: Tie connector capacity to actual task termination

- [ ] Add paused-time tests in `upstream/connect.rs` that drop the current caller-owned `Reservation` together with `PendingConnection`, then use a connector-task barrier to prove replacement acquisition and shutdown remain pending until the aborted connector future executes its reservation-drop cleanup.
- [ ] Run the focused connector tests and confirm the new tests fail because dropping the caller-owned `Reservation` releases capacity immediately.
- [ ] Change `PendingConnection::spawn` to consume the `Reservation`; keep it inside the spawned connector future and return it only with a successful opened connection. Let cancellation, connector error, and unwind drop it inside the task; consume/disarm it only when registering the successful driver.
- [ ] Update both connection-open call sites in `upstream/mod.rs` and the connector tests to use the task-owned reservation.
- [ ] Run the connector and manager test modules and confirm all tests pass.

Focused command:

```bash
cargo test --package trusted-server-cli --target aarch64-apple-darwin commands::dev::proxy::upstream::connect -- --nocapture
```

### Task 2: Continue FIFO handoff after cancellation races

- [ ] Add a deterministic two-waiter actor test in `upstream/manager.rs` by constructing actor requests directly: drop the first request's oneshot receiver while deliberately leaving its ticket `Pending`, queue a live second request, return one matching connection, and assert the first `ticket.resolve()` succeeds but failed delivery is recovered and the second waiter receives that exact lease.
- [ ] Run the test and confirm it fails with the second waiter unresolved.
- [ ] Make `return_connection` loop over same-origin reusable waiters. If ticket resolution loses, continue. If oneshot delivery fails, recover `Acquired::Reused(Lease)` from the send error and continue with its connection payload.
- [ ] Run all manager tests and confirm FIFO, limits, cancellation, and shutdown remain green.

### Task 3: Preserve request trailers end to end

- [ ] Add a header-unit test in `server.rs` proving sanitation regenerates only Hyper-valid request trailer declarations and regenerates `TE: trailers` plus `Connection: TE` only when the browser advertised trailer acceptance.
- [ ] Add an E2E fixture that receives a chunked request with a declared request trailer, observes the trailer at the origin, and returns a response trailer only when upstream `TE: trailers` is present. Assert both trailers cross the proxy.
- [ ] Run the tests and confirm the request trailer/conditional response assertion fails because `rewrite_headers` removes `Trailer` and `TE`.
- [ ] Capture trailer declarations and trailer acceptance before hop-by-hop stripping, filter declarations with Hyper's valid-trailer field rules, and regenerate new upstream-leg metadata after authoritative header rewriting.
- [ ] Re-run the focused header and E2E tests and confirm they pass.

### Task 4: Record every terminal send failure exactly once

- [ ] Add an E2E test using a reserved but unreachable loopback port. Assert the browser receives `502`, `requests_completed == 0`, and `requests_failed == 1`.
- [ ] Add unit coverage showing acquisition duration is recorded on both `Ok` and `Err` manager outcomes.
- [ ] Run the tests and confirm connector/acquisition errors currently leave `requests_failed` and failed acquisition samples at zero.
- [ ] Add a scoped send-outcome guard at the `UpstreamClient::send` boundary. It records one failure unless ownership is handed to `PooledResponseBody`; remove branch-local failure increments that would double-count.
- [ ] Record pool-acquisition elapsed time immediately after each await and before propagating its result, including stale-replacement acquisition.
- [ ] Re-run focused upstream and E2E tests and assert exact counts.

### Task 5: Retain DNS completion without receivers

- [ ] Add a multi-threaded DNS test that starts one miss, cancels every receiver, holds the cache-state mutex to pause state replacement, completes the resolver, and subscribes late to the still-loading entry. Assert the published result is already retained.
- [ ] Run the test and confirm `watch::Sender::send` leaves the late subscriber without the completed value.
- [ ] Replace publication with `send_replace(Some(shared.clone()))`.
- [ ] Run all DNS tests and confirm success/failure fan-out, TTL, cancellation, and bounds remain green.

### Task 6: Complete diagnostics, benchmark validation, and cleanup feedback

- [ ] Extend the metrics summary test first to require totals and buckets for request-to-headers, DNS, TLS, HTTP handshake, and CA mint latency; confirm it fails.
- [ ] Emit sample count, total microseconds, and fixed buckets for every advertised latency phase without adding request labels or sensitive values.
- [ ] Add a remote-workload variant validator test and make `perf_http1_remote_model` reject any value except `remote_baseline` or `remote_pooled`.
- [ ] Document the exact alternating remote commands used for the recorded measurements.
- [ ] Cache `NoVerifier::supported_verify_schemes` in a `OnceLock<Vec<SignatureScheme>>` and add/extend configuration coverage.
- [ ] Document why stale post-dispatch replay excludes non-idempotent requests.
- [ ] Add missing documentation to public items touched by the PR where CLAUDE.md requires it.
- [ ] Run metrics, performance-fixture, connector, and upstream unit tests.

### Task 7: Full verification and GitHub review closure

- [ ] Run `cargo fmt --all -- --check`.
- [ ] Run `cargo check --tests --target aarch64-apple-darwin` from `crates/trusted-server-cli`.
- [ ] Run `cargo clippy --all-targets --target aarch64-apple-darwin -- -D warnings` from `crates/trusted-server-cli`.
- [ ] Run `./scripts/test-cli.sh` and record the exact test totals.
- [ ] Run `npm run format` from `docs` and confirm it makes no changes.
- [ ] Review `git diff --check`, the full diff, and the approved spec invariants. Confirm the pre-existing blind-tunnel logging commit remains untouched.
- [ ] Commit focused changes with imperative, non-semantic messages and push `perf/dev-proxy-optimization`.
- [ ] Reply individually to all ten inline comments: seven actionable findings, the signature-scheme cleanup, the deliberate non-idempotent replay trade-off, and the lifecycle praise thread. Include the implementing commit and regression-test name where applicable; resolve threads only after the pushed diff contains the fix or acknowledgement.
- [ ] Refresh PR checks and report any external CI failure without claiming completion until required checks are green.
