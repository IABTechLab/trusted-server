# Dev Proxy Review Backfill Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Correct dev-proxy outcome metrics and stale-readiness recovery, remove rejected HTTP/2 state, and backfill the lifecycle/security integration coverage identified in the final review.

**Architecture:** Keep response delivery outcome separate from connection poolability: terminal response EOS counts as completion even when the connection must close. Reused-sender readiness failure switches acquisition into an explicit fresh-only path. HTTP/1 remains the sole application protocol, so the origin key and TLS cache contain only fields that affect retained runtime behavior. Test-only raw upstreams exercise malformed, cancelled, and pipelined byte streams without changing production APIs.

**Tech Stack:** Rust 2024, Tokio, Hyper 1.x, rustls/tokio-rustls, error-stack, macOS host-target integration tests.

---

## File map

- `crates/trusted-server-cli/src/commands/dev/proxy/upstream/body.rs`: response outcome accounting versus poolability.
- `crates/trusted-server-cli/src/commands/dev/proxy/upstream/mod.rs`: reused-sender readiness recovery.
- `crates/trusted-server-cli/src/commands/dev/proxy/upstream/key.rs`: retained HTTP/1 origin identity.
- `crates/trusted-server-cli/src/commands/dev/proxy/upstream/connect.rs`: HTTP/1 TLS configuration tests.
- `crates/trusted-server-cli/src/commands/dev/proxy/rewrite.rs`: construct the simplified origin key.
- `crates/trusted-server-cli/src/commands/dev/proxy/config.rs`: update key expectations after HTTP/2-state removal.
- `crates/trusted-server-cli/src/commands/dev/proxy/server.rs`: unit coverage for warning/header/over-read helpers only if required by the integration harness.
- `crates/trusted-server-cli/tests/support/mod.rs`: deterministic raw upstream and client fixtures.
- `crates/trusted-server-cli/tests/proxy_e2e.rs`: lifecycle, security, and three-path over-read coverage.
- `docs/superpowers/specs/2026-07-10-dev-proxy-performance-design.md`: align the retained architecture with the rejected HTTP/2 experiment.
- `docs/superpowers/implementation-notes/2026-07-10-dev-proxy-performance.md`: correct the no-dormant-code claim and record backfilled gates.

### Task 1: Separate response outcome from connection reuse

**Files:**

- Modify: `crates/trusted-server-cli/src/commands/dev/proxy/upstream/body.rs`
- Test: `crates/trusted-server-cli/src/commands/dev/proxy/upstream/body.rs`

- [ ] Add failing unit cases proving terminal response EOS records `requests_completed = 1` and `requests_failed = 0` when `Connection: close`, a streaming/failed upload, or a finished driver prevents reuse, while body error/drop before EOS records failure and never completion.
- [ ] Run `cargo test --package trusted-server-cli --target aarch64-apple-darwin upstream::body::tests::complete_unpoolable_response_is_not_a_request_failure` and confirm the current code reports a failure.
- [ ] Change finalization so `response_complete` alone selects completed versus failed metrics; independently use `can_reuse` to return or abort the lease.
- [ ] Re-run the focused body tests and commit with `Correct dev proxy request outcome metrics`.

### Task 2: Force a fresh connection after readiness failure

**Files:**

- Modify: `crates/trusted-server-cli/src/commands/dev/proxy/upstream/mod.rs`
- Test: `crates/trusted-server-cli/src/commands/dev/proxy/upstream/manager.rs`
- Test: `crates/trusted-server-cli/tests/proxy_e2e.rs`
- Modify: `crates/trusted-server-cli/tests/support/mod.rs`

- [ ] Behavior-preserving refactor: route initial manager acquisition through a private `AcquisitionMode` boundary whose `Normal` and `FreshAfterReadinessFailure` variants both initially call `acquire`; run the existing manager and stale-retry tests green.
- [ ] Add a failing upstream-client policy test with two idle senders and a cap of two. Invoke the `FreshAfterReadinessFailure` boundary and assert it discards idle senders and remains pending until driver reconciliation permits an `Open` reservation. Confirm current behavior incorrectly returns `Reused`.
- [ ] Change only `FreshAfterReadinessFailure` to call `acquire_fresh`, and switch the reused readiness-error branch to that mode while preserving exactly one stale retry.
- [ ] Retain the post-dispatch stale E2E as the wire-observable fallback; document that driver-guard priority normally removes closed idle senders before a second request can deterministically observe `ready()` failure.
- [ ] Run focused manager/upstream tests and the existing post-dispatch stale E2E.
- [ ] Commit with `Use a fresh connection after stale readiness`.

### Task 3: Remove rejected HTTP/2 application state

**Files:**

- Modify: `crates/trusted-server-cli/src/commands/dev/proxy/upstream/key.rs`
- Modify: `crates/trusted-server-cli/src/commands/dev/proxy/rewrite.rs`
- Modify: `crates/trusted-server-cli/src/commands/dev/proxy/config.rs`
- Modify: `crates/trusted-server-cli/src/commands/dev/proxy/upstream/manager.rs`
- Modify: `docs/superpowers/specs/2026-07-10-dev-proxy-performance-design.md`
- Modify: `docs/superpowers/implementation-notes/2026-07-10-dev-proxy-performance.md`

- [ ] Add a failing behavioral test that builds two retained HTTP/1 rules for the same logical TO—one preserving `Host: FROM`, one rewriting `Host: TO`—and asserts their origin keys are equal. Confirm `ApplicationMode` currently makes them unequal even though Host is request-local.
- [ ] Remove `ApplicationMode`, its `OriginKey` field/accessor/constructor argument, and rewrite/config branches that manufacture `Http2Eligible`.
- [ ] Keep TLS configuration keyed by `VerifyMode`; document that this is complete because retained runtime advertises only HTTP/1.1.
- [ ] Update the design spec wherever it still treats application mode as a retained key field or requires HTTP/2-mode separation; preserve the historical experiment section as rejected evidence.
- [ ] Run config, rewrite, key, and manager tests.
- [ ] Commit with `Remove rejected HTTP2 origin state`.

### Task 4: Add connector and TLS-cache unit coverage

**Files:**

- Modify: `crates/trusted-server-cli/src/commands/dev/proxy/upstream/connect.rs`

- [ ] Add characterization tests asserting secure configs are cached together, insecure configs are cached together, secure/insecure configs are distinct, and both advertise only `http/1.1`.
- [ ] Add a paused-time cancellation test with an IP origin and injected `connect_delay`: reserve one manager slot, spawn/register `PendingConnection`, drop it and the reservation before time advances, then assert the abort handle finishes and a replacement reservation is admitted exactly once without DNS/TCP.
- [ ] Add an unwind test that moves the reservation and delayed `PendingConnection` into a spawned task that panics. Assert task unwind drops both guards, manager shutdown/reacquisition completes, and capacity is not decremented twice.
- [ ] Run all `upstream::connect` and manager connector tests.
- [ ] Commit with `Cover dev proxy connector configuration`.

### Task 5: Prove credentials do not persist and insecure mode warns

**Files:**

- Modify: `crates/trusted-server-cli/tests/support/mod.rs`
- Modify: `crates/trusted-server-cli/tests/proxy_e2e.rs`

- [ ] Add the neutral two-request client fixture first, then a characterization test that sends `Authorization` only on the first request over one pooled upstream connection and asserts the gated upstream returns `200` then `401` on one TCP/TLS session.
- [ ] Add a CLI-process characterization test that reserves the configured listen port, launches `ts dev proxy --insecure` with a temporary CA directory, and asserts stderr contains the explicit verification-disabled warning before bind failure.
- [ ] Run both focused E2E tests.
- [ ] Commit with `Backfill dev proxy credential safety tests`.

### Task 6: Cover early response, cancellation, and response-body failure

**Files:**

- Modify: `crates/trusted-server-cli/tests/support/mod.rs`
- Modify: `crates/trusted-server-cli/tests/proxy_e2e.rs`

- [ ] Add a raw TLS upstream that sends a complete response immediately after the chunked request head without draining the upload. Assert the browser receives the response promptly, upload polling/socket activity stops, metrics count completion rather than failure, and the lease is not pooled.
- [ ] Add a slow chunked-response upstream. Drop the browser tunnel after one body chunk; assert `requests_failed` increments, the driver closes, and manager shutdown completes within one second.
- [ ] Add a truncated-response upstream that advertises a larger `Content-Length` than it sends; assert the browser sees truncation and metrics count one failure.
- [ ] Add a chunked response with DATA plus trailers followed by a second request on the same browser tunnel. Assert trailers are forwarded and the upstream connection is reused only after terminal EOS.
- [ ] Run each new E2E independently before proceeding to the next.
- [ ] Commit with `Cover dev proxy streaming failure lifecycles`.

### Task 7: Prove blind and plain forwarding bypass mapped pool accounting

**Files:**

- Modify: `crates/trusted-server-cli/tests/support/mod.rs`
- Modify: `crates/trusted-server-cli/tests/proxy_e2e.rs`

- [ ] Add a raw loopback echo upstream and hold more than 64 unmatched CONNECT tunnels open through one proxy.
- [ ] While those tunnels remain open, issue a mapped request and assert it succeeds with a normal upstream connection, proving blind tunnels do not consume the manager global-live bound.
- [ ] Repeat with more than 64 stray absolute-form plain-HTTP forwarding connections held open, then assert a mapped request still obtains upstream capacity.
- [ ] Close all tunnels and assert bounded proxy shutdown completes.
- [ ] Run the focused integration test.
- [ ] Commit with `Prove blind tunnels bypass pool limits`.

### Task 8: Exercise CONNECT over-read through all three consumers

**Files:**

- Modify: `crates/trusted-server-cli/tests/support/mod.rs`
- Modify: `crates/trusted-server-cli/tests/proxy_e2e.rs`

- [ ] Add a blind-tunnel client that writes CONNECT head plus payload in one buffered write; assert the raw upstream receives the payload exactly once.
- [ ] Add an absolute-form plain-HTTP client that writes head plus body in one buffered write; assert the raw upstream receives head and body exactly once.
- [ ] Add a test-only browser IO wrapper that buffers the CONNECT head with rustls first-flight bytes and strips the proxy HTTP 200 response before exposing TLS records. Use the normal `TlsConnector` above that wrapper, then assert a mapped HTTPS request succeeds. This deterministically drives TLS ClientHello bytes through `PrefixedIo`.
- [ ] Run the three focused tests and the existing `prefixed_io` unit test.
- [ ] Commit with `Cover dev proxy overread consumers`.

### Task 9: Backfill adversarial shutdown at the integration boundary

**Files:**

- Modify: `crates/trusted-server-cli/src/commands/dev/proxy/mod.rs`
- Modify: `crates/trusted-server-cli/tests/proxy_e2e.rs`
- Modify: `crates/trusted-server-cli/tests/support/mod.rs` only if a delayed fixture is required.

- [ ] Keep the existing direct-channel unit test as the deterministic proof that priority shutdown overtakes all 64 occupied ordinary slots; do not attempt to reproduce channel fullness through the continuously draining public actor.
- [ ] Add an integration-boundary test with a live connector/driver whose lifecycle completion is deliberately delayed. Assert shutdown stays pending until reconciliation and an external one-second timeout bounds the caller.
- [ ] Behavior-preserving refactor the Ctrl-C cleanup into a private async helper that takes distinct restoration and accept-loop-stop closures plus the drain future. Add a test proving the exact order is restore Safari/system settings → stop/abort the accept loop → first poll manager drain, and that a delayed drain is capped at two seconds, without touching real system settings.
- [ ] Run the focused shutdown integration test.
- [ ] Commit with `Backfill adversarial proxy shutdown coverage`.

### Task 10: Cover feasible origin-key isolation at the integration boundary

**Files:**

- Modify: `crates/trusted-server-cli/tests/support/mod.rs`
- Modify: `crates/trusted-server-cli/tests/proxy_e2e.rs`
- Test: `crates/trusted-server-cli/src/commands/dev/proxy/upstream/key.rs`

- [ ] Add a multi-rule proxy fixture proving two distinct FROM hosts mapped to the same TO reuse one HTTP/1 connection while per-request Host and forwarding headers remain correct.
- [ ] Add distinct TO/port mappings and assert they never share connections.
- [ ] Retain unit-level independent variation for transport and verification mode because `--upstream-plaintext` and `--insecure` are process-global and cannot coexist in one proxy invocation; document this feasibility boundary in the test.
- [ ] Run the focused key and multi-rule E2E tests.
- [ ] Commit with `Cover dev proxy pool key isolation`.

### Task 11: Final verification and PR update

**Files:**

- Modify: `docs/superpowers/implementation-notes/2026-07-10-dev-proxy-performance.md`

- [ ] Update implementation notes with corrected metric semantics, HTTP/1-only keying, and the added lifecycle/security tests.
- [ ] Run `cargo fmt --all -- --check`.
- [ ] Run `cargo check --package trusted-server-cli --target aarch64-apple-darwin --tests`.
- [ ] Run `cargo clippy --package trusted-server-cli --target aarch64-apple-darwin --all-targets -- -D warnings`.
- [ ] Run `./scripts/test-cli.sh` and all six ignored `proxy_perf` workloads with one test thread.
- [ ] Run the complete repository CI matrix from `CLAUDE.md`: `cargo clippy-fastly`, `cargo clippy-axum`, `cargo clippy-cloudflare`, `cargo clippy-cloudflare-wasm`, `cargo clippy-spin-native`, `cargo clippy-spin-wasm`, `cargo test-fastly`, `cargo test-axum`, `cargo test-cloudflare`, `cargo test-spin`, and the cross-adapter parity test.
- [ ] Run JS build/tests/format (`node build-all.mjs`, `npx vitest run`, `npm run format`) and docs format (`cd docs && npm run format`).
- [ ] Run `git diff --check`.
- [ ] Perform a final self-review of the complete diff against the feedback list.
- [ ] Commit with `Complete dev proxy review backfill`, push `perf/dev-proxy-optimization`, and verify PR #896 is mergeable with CI restarted.
