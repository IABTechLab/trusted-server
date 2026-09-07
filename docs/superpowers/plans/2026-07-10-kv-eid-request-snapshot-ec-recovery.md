# KV EID Request Snapshot and EC Recovery Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Reuse one EC KV snapshot across publisher auction, finalize, and pull sync; overlap Fastly origin work with the lookup; and safely rotate orphaned EC cookies to a newly backed identity.

**Architecture:** A cloneable, EC-ID-bound snapshot carries persisted `KvEntry` state and an optional CAS generation through `EcContext` and Fastly response extensions. Publisher navigations preload it after a truly asynchronous origin start, finalize consumes and updates it, and pull sync uses the finalized state and request-wide bulk persistence. Missing rows rotate only during real-browser document navigation; withdrawal becomes existing-key-only.

**Tech Stack:** Rust 2024, `error-stack`, EdgeZero HTTP abstractions, Fastly KV generation/CAS, existing core and adapter test support.

---

## File Map

- Modify `crates/trusted-server-core/src/ec/mod.rs`: request-scoped snapshot type, EC-ID binding, navigation/recovery state, and normal-generation snapshot seeding.
- Modify `crates/trusted-server-core/src/ec/kv.rs`: snapshot-aware bulk mutation, existing-key-only tombstones, persisted-state outcomes, and CAS retry tests.
- Modify `crates/trusted-server-core/src/ec/prebid_eids.rs`: separate validated update collection from persistence so orphan creation can include IDs atomically.
- Modify `crates/trusted-server-core/src/ec/finalize.rs`: consume/return snapshot state, rotate authoritative misses, and preserve withdrawal ordering.
- Modify `crates/trusted-server-core/src/auction/endpoints.rs`: resolve server-side auction EIDs from a snapshot instead of performing KV I/O.
- Modify `crates/trusted-server-core/src/publisher.rs`: snapshot original request head, conditionally start origin first, preload snapshot, dispatch auction, and await origin.
- Modify `crates/trusted-server-core/src/ec/pull_sync.rs`: consume finalized snapshot and aggregate successful partner results across all HTTP batches before one bulk persistence operation.
- Modify `crates/trusted-server-adapter-fastly/src/app.rs`: carry navigation and snapshot state through `EcRequestState`/`EcFinalizeState` and publisher dispatch.
- Modify `crates/trusted-server-adapter-fastly/src/main.rs`: finalize mutable EC state, pass updated snapshot to post-send pull sync, and keep #880 concerns out.
- Modify adapter call sites/tests only as required by shared signature changes.

### Task 1: Define EC KV Snapshot Semantics

**Files:**

- Modify: `crates/trusted-server-core/src/ec/mod.rs`
- Modify: `crates/trusted-server-core/src/ec/kv.rs`
- Test: `crates/trusted-server-core/src/ec/mod.rs`
- Test: `crates/trusted-server-core/src/ec/kv.rs`

- [ ] **Step 1: Write failing snapshot-state tests**

Add tests proving that a snapshot distinguishes not-read, missing, failed, and present; a present snapshot is bound to one EC ID; a successful mutation can retain an entry without a usable generation; and a different active EC ID cannot consume stale state.

- [ ] **Step 2: Run the focused tests and verify RED**

Run: `cargo test -p trusted-server-core ec::tests::kv_snapshot`

Expected: compilation/test failure because the snapshot API does not exist.

- [ ] **Step 3: Implement the minimal snapshot type and accessors**

Use an enum whose present state contains `ec_id`, `KvEntry`, and `Option<u64>`. Keep the type cloneable and do not store `Report`. Add helpers for ID-safe entry/generation access and state replacement.

- [ ] **Step 4: Write failing authoritative-creation tests**

Cover create-if-absent Written, collision, and store error outcomes. Cover generation collision retry, bounded collision exhaustion, and rollback with no request-local candidate exposed as persisted.

- [ ] **Step 5: Run authoritative-creation tests and verify RED**

Run: `cargo test -p trusted-server-core ec::kv::tests::create_if_absent`

Run: `cargo test -p trusted-server-core ec::tests::generate_collision`

Expected: fail because the Add-only outcome and bounded retry do not exist.

- [ ] **Step 6: Implement Add-only creation and generation seeding**

Introduce a `KvIdentityGraph` create-if-absent outcome that preserves `Written` versus collision while propagating store errors. Replace generation-time `create_or_revive` use with that Add-only outcome. Seed the exact candidate entry only after `Written`; on collision generate a different fresh suffix and retry within a small bound. Never revive a tombstone or claim a colliding request-local candidate was persisted. Do not change cookie emission timing.

- [ ] **Step 7: Run focused tests and verify GREEN**

Run: `cargo test -p trusted-server-core ec::tests::kv_snapshot`

Run: `cargo test -p trusted-server-core ec::kv::tests::create_if_absent`

Expected: snapshot tests and Written/collision/store-error creation tests pass.

- [ ] **Step 8: Run existing EC generation tests**

Run: `cargo test -p trusted-server-core ec::tests::generate`

Expected: existing generation and failure rollback tests pass.

### Task 2: Add Snapshot-Aware KV Mutations

**Files:**

- Modify: `crates/trusted-server-core/src/ec/kv.rs`
- Test: `crates/trusted-server-core/src/ec/kv.rs`

- [ ] **Step 1: Write failing bulk-mutation tests**

Cover: supplied generation avoids an initial read; unchanged updates preserve generation; successful writes return persisted entry with unavailable generation; unavailable generation refreshes exactly once; CAS conflict rereads and re-merges; tombstone rejects updates; missing never creates a root; store failure returns failed state rather than request-local data.

- [ ] **Step 2: Run focused tests and verify RED**

Run: `cargo test -p trusted-server-core ec::kv::tests::snapshot`

Expected: failure because snapshot-aware mutation APIs do not exist.

- [ ] **Step 3: Implement snapshot-aware bulk upsert**

Refactor the existing merge/CAS loop behind one API accepting an initial snapshot. Preserve `upsert_partner_id_if_exists` semantics for batch sync and keep compatibility wrappers only where necessary.

- [ ] **Step 4: Write failing conditional-tombstone tests**

Cover present row, missing row, CAS conflict, disappearance during retry, and store failure. Assert missing/failed cases perform no insert.

- [ ] **Step 5: Implement existing-key-only tombstones**

Use the carried generation when available, refresh when unavailable, and retry conflicts without unconditional overwrite. Retain the 24-hour TTL and empty tombstone entry.

- [ ] **Step 6: Run focused and module tests**

Run: `cargo test -p trusted-server-core ec::kv::tests`

Expected: all KV tests pass.

### Task 3: Separate EID Collection from Persistence

**Files:**

- Modify: `crates/trusted-server-core/src/ec/prebid_eids.rs`
- Test: `crates/trusted-server-core/src/ec/prebid_eids.rs`

- [ ] **Step 1: Write failing collection tests**

Prove one helper returns validated, registry-matched, deduplicated updates from `ts-eids` and `sharedId` without touching KV.

- [ ] **Step 2: Run focused tests and verify RED**

Run: `cargo test -p trusted-server-core ec::prebid_eids::tests::collect`

- [ ] **Step 3: Extract the collection API**

Reuse existing parsing and validation. Keep logging best-effort and keep raw identifiers out of logs.

- [ ] **Step 4: Route existing ingestion through the helper**

Preserve current public behavior while allowing finalize to apply updates to a new orphan-recovery entry before `Add`.

- [ ] **Step 5: Run module tests and verify GREEN**

Run: `cargo test -p trusted-server-core ec::prebid_eids::tests`

### Task 4: Implement Finalize Recovery and Persisted Outcomes

**Files:**

- Modify: `crates/trusted-server-core/src/ec/finalize.rs`
- Modify: `crates/trusted-server-core/src/ec/mod.rs`
- Test: `crates/trusted-server-core/src/ec/finalize.rs`

- [ ] **Step 1: Write failing persisted-mutation finalize tests**

Cover returning-user update from a supplied generation, unchanged update, CAS failure returning failed state, and successful update returning the persisted entry without generation. Cover NotRead returning EIDs performing exactly one lazy lookup/mutation, NotRead never rotating, and a failed lazy lookup degrading without retry.

- [ ] **Step 2: Run the persisted-mutation tests and verify RED**

Run: `cargo test -p trusted-server-core ec::finalize::tests::snapshot`

- [ ] **Step 3: Implement persisted finalize mutation and verify GREEN**

Use the snapshot-aware KV API and return only authoritative persisted state.

Run: `cargo test -p trusted-server-core ec::finalize::tests::snapshot`

- [ ] **Step 4: Write failing orphan-recovery tests**

Cover authoritative miss on real-browser navigation rotating to a fresh EC, applying request EIDs before one `Add`, emitting a cookie only after success, bounded ID collision retry, KV failure not rotating, tombstone not rotating, NotRead not rotating, and subresource/non-browser requests not rotating.

- [ ] **Step 5: Run orphan tests RED, implement rotation, and verify GREEN**

Run before and after implementation: `cargo test -p trusted-server-core ec::finalize::tests::orphan`

Make finalization update request EC state only after the replacement entry is durably added. Return the authoritative persisted snapshot for post-send consumers.

- [ ] **Step 6: Write failing withdrawal tests**

Assert browser cookie expiry remains immediate while present/missing/failed snapshot states follow the conditional tombstone contract. Cover cookie EC differing from active EC: use the carried snapshot only for its matching ID, independently look up/CAS the other, and never create either missing ID.

- [ ] **Step 7: Run withdrawal tests RED, implement, and verify GREEN**

Run before and after implementation: `cargo test -p trusted-server-core ec::finalize::tests::withdrawal`

- [ ] **Step 8: Run complete finalize and EC tests**

Run: `cargo test -p trusted-server-core ec::finalize::tests`

Run: `cargo test -p trusted-server-core ec::tests`

### Task 5: Thread Snapshot and Browser Eligibility Through Call Sites

**Files:**

- Modify: `crates/trusted-server-core/src/publisher.rs`
- Modify: `crates/trusted-server-adapter-fastly/src/app.rs`
- Modify: `crates/trusted-server-adapter-axum/src/app.rs`
- Modify: `crates/trusted-server-adapter-cloudflare/src/app.rs`
- Modify: `crates/trusted-server-adapter-spin/src/app.rs`

- [ ] **Step 1: Write failing handler-contract tests**

Require an explicit mutable snapshot and real-browser eligibility input at the publisher boundary. Prove non-Fastly adapters use NotRead/no-KV state and cannot authorize recovery merely from navigation headers.

- [ ] **Step 2: Run focused tests and verify RED**

Run: `cargo test -p trusted-server-core publisher::tests::ec_snapshot_contract`

- [ ] **Step 3: Thread the contract through every call site**

Update all publisher handler invocations together so the workspace remains compilable. Fastly supplies request state; other adapters pass explicit NotRead/no-KV and recovery-ineligible `false`. Do not change auction resolution or scheduling yet.

- [ ] **Step 4: Check all adapter compilation**

Run: `cargo check-fastly`

Run: `cargo check-axum`

Run: `cargo check-cloudflare`

Run: `cargo check-spin`

Expected: all call sites compile before auction behavior changes.

### Task 6: Resolve Auction EIDs and Schedule Origin from the Snapshot

**Files:**

- Modify: `crates/trusted-server-core/src/auction/endpoints.rs`
- Modify: `crates/trusted-server-core/src/publisher.rs`
- Test: `crates/trusted-server-core/src/auction/endpoints.rs`
- Test: `crates/trusted-server-core/src/publisher.rs`

- [ ] **Step 1: Write failing snapshot-resolution tests**

Cover present live entry, missing, failed, not-read, mismatched EC ID, tombstone, denied consent, and absent registry. Assert resolution performs no KV operation. Update the separate page-bids endpoint to perform its own single explicit snapshot load before resolution; do not retain a compatibility KV lookup inside `resolve_auction_eids`.

- [ ] **Step 2: Run focused tests and verify RED**

Run: `cargo test -p trusted-server-core auction::endpoints::tests::resolve_auction_eids`

- [ ] **Step 3: Replace every graph lookup call surface with snapshot resolution**

Keep client-EID merge and consent gating unchanged. Remove the discarded-generation lookup from auction resolution. Make page-bids snapshot ownership explicit and local because it is outside the publisher-navigation lifecycle.

- [ ] **Step 4: Run auction endpoint tests and verify GREEN**

Run: `cargo test -p trusted-server-core auction::endpoints::tests`

- [ ] **Step 5: Write failing request-head snapshot tests**

Assert bidders see the original method, URI, Host, scheme signal, and selected headers after the real request is rewritten and consumed by origin dispatch. Assert provider outbound sanitization tests remain unchanged.

- [ ] **Step 6: Write failing concurrent-order and preload-gate tests**

Record events proving `origin_start` precedes `kv_lookup` and `auction_dispatch`, then `origin_wait` follows dispatch for a truly concurrent client. Also prove real-browser document navigations preload when auctions are disabled, no slots match, or consent denies auction; non-document requests do not preload; and non-EC requests retain the ordinary origin path.

- [ ] **Step 7: Write failing eager-client ordering test**

Prove a client reporting no concurrent fan-out and needing a snapshot performs `kv_lookup -> auction_dispatch -> origin_execute`, never completing the eager origin before auction dispatch.

- [ ] **Step 8: Write failing error-path tests**

Cover origin-start failure causing no KV/auction work, auction dispatch failure still returning origin, and origin completion failure emitting one abandoned-auction terminal event.

- [ ] **Step 9: Run focused tests and verify RED**

Run: `cargo test -p trusted-server-core publisher::tests::origin_auction_order`

- [ ] **Step 10: Implement capability-aware scheduling**

Extract small helpers rather than duplicating auction construction. For concurrent clients, start origin first, preload/refresh while it is in flight, dispatch from the bodyless request head, then await origin. For eager clients, preload first, dispatch auction, then execute origin. Apply the explicit navigation/browser/active-EC/graph gates from the spec even when auction work is skipped.

- [ ] **Step 11: Run endpoint and publisher tests and verify GREEN**

Run: `cargo test -p trusted-server-core publisher::tests`

### Task 7: Thread Finalize Outcome Through Fastly

**Files:**

- Modify: `crates/trusted-server-adapter-fastly/src/app.rs`
- Modify: `crates/trusted-server-adapter-fastly/src/main.rs`
- Test: `crates/trusted-server-adapter-fastly/src/app.rs`
- Test: `crates/trusted-server-adapter-fastly/src/main.rs`

- [ ] **Step 1: Write failing state-threading tests**

Assert publisher navigation state carries snapshot and navigation eligibility into `EcFinalizeState`; named/subresource routes cannot request orphan rotation; normal generated state is preserved; and cookie/active EC mismatch retains enough state for two-ID withdrawal handling.

- [ ] **Step 2: Run focused Fastly tests and verify RED**

Run: `cargo test-fastly ec_finalize_state`

- [ ] **Step 3: Implement state threading and mutable finalize outcome**

Pass the snapshot into `handle_publisher_request`, pop mutable finalize state in `main.rs`, apply the returned EC context/snapshot, send the response, and pass finalized state to pull sync.

- [ ] **Step 4: Run focused Fastly tests and verify GREEN**

Run: `cargo test-fastly ec_finalize_state`

### Task 8: Reuse Finalized State in Pull Sync

**Files:**

- Modify: `crates/trusted-server-core/src/ec/pull_sync.rs`
- Modify: `crates/trusted-server-adapter-fastly/src/main.rs`
- Test: `crates/trusted-server-core/src/ec/pull_sync.rs`

- [ ] **Step 1: Write failing eligibility tests**

Prove a present finalized snapshot selects only missing partners without an initial KV read; failed/missing/not-read/tombstone state dispatches no pull work in #851. Do not add #880 completeness-marker or partner-set precheck behavior.

- [ ] **Step 2: Write failing request-wide aggregation tests**

Provide successful partner responses across multiple concurrency batches and assert one final bulk update. Cover a usable generation, a finalize-written unavailable generation requiring exactly one refresh read, CAS conflict re-merge, tombstone on refresh, and persistence failure.

- [ ] **Step 3: Run focused tests and verify RED**

Run: `cargo test -p trusted-server-core ec::pull_sync::tests`

- [ ] **Step 4: Implement snapshot-aware pull sync**

Separate network-batch draining from KV persistence. Accumulate deduplicated updates for the whole request, persist once after all responses, and keep the current rate limits, allowlist, tokens, response bounds, and best-effort logging.

- [ ] **Step 5: Run pull-sync tests and verify GREEN**

Run: `cargo test -p trusted-server-core ec::pull_sync::tests`

### Task 9: Run Regression Suites

**Files:**

- Modify as required: `crates/trusted-server-adapter-axum/src/app.rs`
- Modify as required: `crates/trusted-server-adapter-cloudflare/src/app.rs`
- Modify as required: `crates/trusted-server-adapter-spin/src/app.rs`
- Modify as required: shared tests and test support

- [ ] **Step 1: Compile all adapters and fix only signature fallout**

Run: `cargo check-fastly`

Run: `cargo check-axum`

Run: `cargo check-cloudflare`

Run: `cargo check-spin`

- [ ] **Step 2: Run adapter test suites**

Run: `cargo test-fastly`

Run: `cargo test-axum`

Run: `cargo test-cloudflare`

Run: `cargo test-spin`

- [ ] **Step 3: Run cross-adapter parity tests**

Run: `cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test parity`

- [ ] **Step 4: Run formatting**

Run: `cargo fmt --all -- --check`

- [ ] **Step 5: Run target-matched clippy**

Run: `cargo clippy-fastly`

Run: `cargo clippy-axum`

Run: `cargo clippy-cloudflare`

Run: `cargo clippy-cloudflare-wasm`

Run: `cargo clippy-spin-native`

Run: `cargo clippy-spin-wasm`

- [ ] **Step 6: Inspect the final diff**

Run: `git diff --check`

Run: `git status --short`

Confirm no #880 completeness marker, partner fingerprint, or unrelated refactor entered the branch.

### Task 10: Final Code Review

**Files:**

- Review all modified production and test files

- [ ] **Step 1: Review against the approved spec**

Check every success criterion and privacy invariant against code and tests.

- [ ] **Step 2: Review concurrency and cost accounting**

Manually trace returning user, first generation, orphan miss, KV failure, withdrawal, finalize write followed by pull write, no auction, eager adapter, and concurrent adapter.

- [ ] **Step 3: Re-run any test affected by review fixes**

Use the smallest target-specific command first, then repeat the relevant adapter suite.

- [ ] **Step 4: Commit implementation in coherent units**

Use small commits aligned with snapshot/KV primitives, publisher scheduling, finalize recovery, and pull-sync reuse. Do not amend the reviewed design commit.
