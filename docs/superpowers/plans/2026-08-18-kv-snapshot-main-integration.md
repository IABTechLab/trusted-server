# KV Snapshot Main Integration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Resolve PR #885 against current `main`, preserve true-streaming SSAT responses, and complete request-scoped EC KV snapshot reuse and orphan recovery on the current architecture.

**Architecture:** Merge current `main` into the published feature branch, retaining newer SSAT/streaming behavior and threading `EcKvSnapshot` through it. Add an explicit platform capability for a single pending request whose response body remains streamed; Fastly implements that capability through a direct pending `wait`, while all other clients use the existing eager fallback. Keep #1013 out of this PR and verify it separately with a temporary merge.

**Tech Stack:** Rust 1.95, `edgezero_core` HTTP abstractions, Fastly Compute SDK 0.12, `error-stack`, Viceroy, Cargo workspace aliases.

---

## File Structure

- `crates/trusted-server-core/src/platform/http.rs`: pending-stream capability contract and documentation.
- `crates/trusted-server-core/src/platform/test_support.rs`: configurable test client support and request-order/stream assertions.
- `crates/trusted-server-adapter-fastly/src/platform.rs`: Fastly direct pending wait with streamed-response conversion and defensive select rejection.
- `crates/trusted-server-core/src/publisher.rs`: current-main SSAT scheduling plus request-scoped snapshot preload and reuse.
- `crates/trusted-server-core/src/http_util.rs`: retain #885's documentation of the internal `fastly-ssl` scheme signal used by origin rewriting.
- `crates/trusted-server-adapter-fastly/src/app.rs`: recovery authorization and snapshot-bearing finalize-state handoff on the streaming publisher path.
- `crates/trusted-server-adapter-fastly/src/main.rs`: mutable EC finalization and post-send pull-sync reuse.
- `crates/trusted-server-core/src/auction/endpoints.rs`: snapshot-based endpoint EID resolution without unused reads.
- `crates/trusted-server-core/src/ec/{mod.rs,kv.rs,finalize.rs,prebid_eids.rs,pull_sync.rs}`: existing #885 snapshot, recovery, bulk CAS, and tests reconciled with current main.

### Task 1: Merge Current Main and Preserve Both Sides' Behavior

**Files:**

- Modify: `crates/trusted-server-adapter-fastly/src/app.rs`
- Modify: `crates/trusted-server-core/src/publisher.rs`
- Auto-merge/reconcile as needed: `crates/trusted-server-adapter-fastly/src/main.rs`
- Auto-merge/reconcile as needed: `crates/trusted-server-core/src/auction/endpoints.rs`
- Auto-merge/reconcile as needed: `crates/trusted-server-core/src/ec/mod.rs`
- Auto-merge/reconcile as needed: `crates/trusted-server-core/src/ec/prebid_eids.rs`
- Auto-merge/reconcile as needed: `crates/trusted-server-core/src/ec/pull_sync.rs`
- Retain: `crates/trusted-server-core/src/http_util.rs`

- [ ] **Step 0: Verify a clean preflight**

Run: `git status --short --branch`

Expected: the approved spec and this reviewed plan are committed, and there are no other changes. Do not begin the merge otherwise.

- [ ] **Step 1: Merge `origin/main` without committing**

Run: `git merge --no-commit --no-ff origin/main`

Expected: conflicts only in the previously inventoried `app.rs` and `publisher.rs` regions; unrelated main changes are staged automatically.

- [ ] **Step 2: Resolve the adapter conflict structurally**

Keep `publisher_response_into_streaming_response(...)` from main. Immediately after `handle_publisher_request` succeeds and before conversion, authorize recovery only with the already-computed real-browser navigation predicate:

```rust
ec.ec_context
    .set_recovery_eligible(is_publisher_navigation);
publisher_response_into_streaming_response(/* current-main arguments */).await
```

- [ ] **Step 3: Resolve publisher request-building conflicts**

Keep current-main configured publisher-domain attribution and telemetry. Keep #885's request-head snapshot so the client request remains available after an origin request is consumed by a pending send. Preserve DataDome suppression, cache bypass, conditional/range stripping, streamed response selection, and current response routing.

- [ ] **Step 4: Resolve test-module imports and retain both suites**

Keep #885's request-order/KV fixtures and current-main auction/streaming fixtures. Deduplicate imports only.

- [ ] **Step 5: Confirm no unresolved markers**

Run: `rg -n '^(<<<<<<<|=======|>>>>>>>)' crates docs`

Expected: no matches.

- [ ] **Step 6: Record the uncommitted merge state**

Run: `git status --short`

Expected: merged main files plus the deliberately resolved feature files, with no `UU` entries.

Keep `MERGE_HEAD` active through Tasks 2-5. Do not create intermediate commits: any `git commit` during this period would complete the entire merge with all staged main changes.

### Task 2: Specify the Pending Stream Contract with Failing Tests

**Files:**

- Modify: `crates/trusted-server-core/src/platform/http.rs`
- Modify: `crates/trusted-server-core/src/platform/test_support.rs`
- Test: `crates/trusted-server-adapter-fastly/src/platform.rs`

- [ ] **Step 1: Add a capability-matrix test**

Add a test client configuration proving concurrent fan-out and ordinary streamed `send` support do not imply pending streamed-response support. The new method defaults to `false`.

- [ ] **Step 2: Add Fastly pending-wait tests**

Add focused tests for these contracts:

```rust
assert!(client.supports_pending_streaming_responses());
```

- a stream-marked pending passed to `select` is rejected;
- a regular pending remains accepted by the buffered auction-select path;
- direct `wait` preserves the stream flag and request method in conversion;
- HEAD and `1xx`/`204`/`205`/`304` results remain bodiless.

- [ ] **Step 3: Run the tests and verify RED**

Run: `cargo test-fastly platform::tests -- --nocapture`

Expected: FAIL because the capability and direct wait contract are not implemented.

### Task 3: Implement Single-Pending Stream Preservation

**Files:**

- Modify: `crates/trusted-server-core/src/platform/http.rs`
- Modify: `crates/trusted-server-core/src/platform/test_support.rs`
- Modify: `crates/trusted-server-adapter-fastly/src/platform.rs`

- [ ] **Step 1: Add the explicit capability**

Add a default-false method to `PlatformHttpClient`:

```rust
fn supports_pending_streaming_responses(&self) -> bool {
    false
}
```

Document that support requires a direct single-request `wait`; stream-marked pendings must not enter `select`.

- [ ] **Step 2: Carry Fastly pending conversion metadata**

Wrap Fastly pending origin state with the pending request, `stream_response`, and original request method. Keep ordinary auction pendings on their existing representation or otherwise make the two paths unambiguous.

- [ ] **Step 3: Override Fastly `wait`**

Complete a stream-marked handle with `fastly::PendingRequest::wait()` and call the existing converter with the retained stream/method metadata. Preserve current error context and backend correlation.

- [ ] **Step 4: Reject stream-marked `select`**

Return `PlatformError::HttpClient` before calling Fastly `select` when any pending request requests streamed response conversion.

- [ ] **Step 5: Update the test client**

Allow tests to advertise the combined capability and resolve a stream-marked single pending through `wait`. Keep multi-request `select` buffered.

- [ ] **Step 6: Run the focused tests and verify GREEN**

Run: `cargo test-fastly platform::tests -- --nocapture`

Expected: PASS.

### Task 4: Reconcile SSAT Snapshot Scheduling with Current Streaming

**Files:**

- Modify: `crates/trusted-server-core/src/publisher.rs`
- Test: `crates/trusted-server-core/src/publisher.rs`

- [ ] **Step 1: Add/adjust failing concurrent-path tests**

Tests must assert the observable operation order:

```text
origin_start -> kv_lookup -> auction_dispatch -> origin_wait
```

They must also run with `supports_pending_streaming_responses=true` and assert
the final pending origin request:

- asks for cache bypass when the ad stack runs and streamed response conversion;
- rewrites URI and `Host` to the configured publisher origin while preserving
  the original client request snapshot for auction construction;
- strips the internal `fastly-ssl` header before forwarding;
- removes conditional and range headers for eligible synthesized documents and
  DataDome-suppressed HTML;
- carries DataDome suppression through the existing response privacy and
  client-tag injection behavior;
- returns a publisher body that remains an `EdgeBody::Stream`.

Parameterize or extend the existing current-main request-rewrite, conditional/
range, and DataDome cases so they exercise the pending origin-first path rather
than passing only through fallback `send`.

- [ ] **Step 2: Add failure-path tests**

Add tests proving:

- pending start failure performs zero KV reads, zero auction dispatches, and zero abandonment events;
- pending wait failure after dispatch returns the existing proxy error and emits exactly one `origin_proxy_error` terminal event;
- configured publisher domain is used in both the auction request and observation event.

- [ ] **Step 3: Add fallback capability tests**

Cover clients with:

- concurrent + send-streaming + no pending-streaming;
- eager + buffered;
- eager + send-streaming.

Assert fallback order is snapshot preload, auction dispatch, then `send`; request `stream_response` exactly when ordinary streaming is supported.

- [ ] **Step 4: Run focused publisher tests and verify RED**

Run: `cargo test-fastly publisher::tests -- --nocapture`

Expected: new scheduling/capability tests fail against the unresolved behavior.

- [ ] **Step 5: Implement the minimal scheduling change**

Build the final origin `PlatformHttpRequest` before branching. Use pending origin-first scheduling only when snapshot preloading is useful and both concurrent fan-out and pending-stream response capabilities are true. Otherwise retain the eager current-main `send` path. In both paths, use the same snapshot for `apply_auction_eids_and_device`.

- [ ] **Step 6: Run focused publisher tests and verify GREEN**

Run: `cargo test-fastly publisher::tests -- --nocapture`

Expected: PASS.

### Task 5: Reconcile Finalization and Cross-Phase Snapshot Reuse

**Files:**

- Modify: `crates/trusted-server-adapter-fastly/src/app.rs`
- Modify: `crates/trusted-server-adapter-fastly/src/main.rs`
- Modify: `crates/trusted-server-core/src/auction/endpoints.rs`
- Modify: `crates/trusted-server-core/src/ec/mod.rs`
- Modify: `crates/trusted-server-core/src/ec/kv.rs`
- Modify: `crates/trusted-server-core/src/ec/finalize.rs`
- Modify: `crates/trusted-server-core/src/ec/prebid_eids.rs`
- Modify: `crates/trusted-server-core/src/ec/pull_sync.rs`

- [ ] **Step 1: Run existing #885 EC tests against the merged code**

Run: `cargo test-fastly ec:: -- --nocapture`

Expected: any failures identify current-main integration gaps rather than missing original coverage.

- [ ] **Step 2: Add a streaming-route recovery lifecycle regression test if absent**

Prove successful publisher navigation authorizes recovery before streaming response conversion, while named routes, filter short circuits, and origin-start failures remain ineligible.

- [ ] **Step 3: Run the new lifecycle test and verify RED**

Run the exact new test filter with `cargo test-fastly app::tests::<test_name> -- --nocapture`.

Expected: FAIL because the current-main streaming route has not yet authorized/reused the merged recovery state. If it already passes, record that result and do not make production changes for behavior the test proves is already correct.

- [ ] **Step 4: Reconcile mutable finalization**

Ensure `EcFinalizeState` carries the context snapshot, `ec_finalize_response` mutates it, and post-send pull sync reads the finalized snapshot. Preserve current streaming send and final Set-Cookie privacy enforcement.

- [ ] **Step 5: Reconcile endpoint reads**

Keep endpoint-local snapshot loads gated on a live auction and non-empty partner registry so unusable requests incur no billable KV read.

- [ ] **Step 6: Run EC and adapter tests and verify GREEN**

Run: `cargo test-fastly ec:: -- --nocapture`

Run: `cargo test-fastly app::tests -- --nocapture`

Expected: PASS.

### Task 6: Complete the Main Merge and Run Full Verification

**Files:**

- Verify all merged files

- [ ] **Step 1: Format**

Run: `cargo fmt --all -- --check`

Expected: PASS. If formatting is needed, run `cargo fmt --all`, inspect the diff, then repeat the check.

- [ ] **Step 2: Run adapter test suites**

Run: `cargo test-fastly`

Run: `cargo test-axum`

Run: `cargo test-cloudflare`

Run: `cargo test-spin`

Expected: all PASS.

- [ ] **Step 3: Run target-matched Clippy**

Run: `cargo clippy-fastly`

Run: `cargo clippy-axum`

Run: `cargo clippy-cloudflare`

Run: `cargo clippy-cloudflare-wasm`

Run: `cargo clippy-spin-native`

Run: `cargo clippy-spin-wasm`

Expected: all PASS with warnings denied by the aliases.

- [ ] **Step 4: Run cross-adapter parity if touched by merge**

Run: `cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test parity`

Expected: PASS.

- [ ] **Step 5: Run JavaScript build, tests, and formatting**

Run: `cd crates/trusted-server-js/lib && node build-all.mjs && npx vitest run && npm run format`

Expected: PASS with no generated diff.

- [ ] **Step 6: Run docs formatting**

Run: `cd docs && npm run format`

Expected: PASS. Inspect and retain only formatting changes to the new spec/plan if produced.

- [ ] **Step 7: Re-run Rust format after all formatting tools**

Run: `cargo fmt --all -- --check`

Expected: PASS.

- [ ] **Step 8: Complete the single verified merge commit**

Run: `git status --short`

Expected: no conflicts or unstaged changes outside the intended merge. Stage resolved/new files as needed, then complete the merge with `git commit -m "Merge main and preserve EC snapshot streaming"`.

### Task 7: Verify #1013 Compatibility Without Polluting PR #885

**Files:**

- No committed changes expected

- [ ] **Step 1: Preview conflicts without changing the primary checkout**

Run `git merge-tree` with the completed #885 head and `origin/1009-esi-cacheable-root-spec` and record the conflicting files. This preview is diagnostic only and is not used for compilation.

- [ ] **Step 2: Create a detached temporary integration worktree**

Create a directory with `mktemp -d`, add a detached worktree at completed #885 `HEAD`, and inside it run:

```bash
git merge --no-commit --no-ff origin/1009-esi-cacheable-root-spec
```

Resolve conflicts only inside this temporary detached worktree. Do not commit or push the temporary merge. Record whether snapshot threading composes with C2/ESI and whether the known warm-hit KV-before-C2 ordering remains.

- [ ] **Step 3: Run focused combined tests when a clean temporary merge is feasible**

Run inside the temporary worktree:

```bash
cargo test-fastly publisher::tests -- --nocapture
cargo test-fastly esi_assembly::tests -- --nocapture
```

Expected: snapshot/recovery behavior remains compatible; warm ESI cache-hit TTFB remains a separate #1013 ordering concern unless explicitly changed there.

- [ ] **Step 4: Remove only the temporary integration workspace/ref**

Abort the temporary merge if still active, remove the detached worktree with `git worktree remove <explicit-temp-path>`, then remove the now-empty temporary directory. Do not alter or delete either published feature branch. Verify the primary checkout still has the completed #885 HEAD and a clean status.

### Task 8: Review and PR Handoff

**Files:**

- Review final diff only

- [ ] **Step 1: Review the branch diff against main**

Run: `git diff --check origin/main...HEAD`

Run: `git diff --name-status origin/main...HEAD`

Run: `git diff --stat origin/main...HEAD`

Expected: only #885 snapshot/recovery work, the retained `http_util.rs` documentation, narrow pending-stream support, tests, and design/plan documents. Investigate every unexpected path before handoff.

- [ ] **Step 2: Request code review**

Use the `superpowers:requesting-code-review` skill and address confirmed findings.

- [ ] **Step 3: Re-run affected verification after review changes**

Expected: all relevant checks remain green.

- [ ] **Step 4: Push and update PR #885**

Push `fix/kv-eid-request-snapshot-ec-ttl`, then comment with:

- confirmation that the issue remains relevant;
- the current-main merge and verification results;
- SSAT streaming preservation details;
- the separate warm ESI-hit ordering finding for #1013.
