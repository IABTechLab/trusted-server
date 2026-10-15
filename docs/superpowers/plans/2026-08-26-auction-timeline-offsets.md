# Auction Timeline Offsets Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Record three T0-anchored auction milestones (dispatched, resolved, committed) plus the auction id on `RequestTimings`, and emit them as four additive columns on the `access_logs_raw` row.

**Architecture:** Follows spec section 18. All state lives in the existing `RequestTimings` inner (same `try_lock`/first-call-wins/saturating model as `mark_headers_ready`); the row builder reads the values from `TimingSnapshot`, so no new emission path and no adapter changes.

**Tech Stack:** Rust (core crate only), Tinybird datasource and fixture files.

**Spec:** `docs/superpowers/specs/2026-08-24-request-phase-timing-design.md` section 18.

## Global Constraints

- Marks are first-call-wins; `try_lock` only; a contended lock drops the sample, a poisoned lock is recovered (matching every other write on the collector).
- A null offset means "this milestone was not reached", not "no auction ran". `auction_id` is what separates the two: null id means no auction was attempted; a non-null id with a null dispatch offset means attempted but nothing sent; a non-null dispatch offset with a null resolve offset means dispatched and never collected.
- The id is stamped where the observation is built, not on dispatch, so skipped and dispatch-failed auctions stay joinable to the `auction_events_raw` rows they emit.
- Column names: `auction_dispatched_ms`, `auction_resolved_ms`, `auction_committed_ms`, `auction_id`; JSONPaths `json:$.<name>`; FORWARD_QUERY extended in the same order.
- `auction_id` is `Nullable(UUID)`, matching `auction_events_raw.auction_id` so the join needs no cast and no sentinel.
- All three `AuctionSource` variants are instrumented: `InitialNavigation`, `SpaNavigation` (`/_ts/page-bids`), and `AuctionApi` (`POST /auction`).
- No header emission and no config surface. Changes are confined to `trusted-server-core`, `tinybird/`, and the spec and plan documents.

---

### Task 1: RequestTimings marks and snapshot fields

**Files:**

- Modify: `crates/trusted-server-core/src/request_timing.rs`

**Interfaces:**

- Produces: `set_auction_id(&self, auction_id: Uuid)`, `mark_auction_dispatched(&self)`, `mark_auction_resolved(&self)`, `mark_auction_committed(&self)`; `TimingSnapshot { auction_dispatched_ms, auction_resolved_ms, auction_committed_ms: Option<u32>, auction_id: Option<Uuid>, .. }`

- [x] Add `auction_dispatched`, `auction_resolved`, `auction_committed: Option<Duration>` and `auction_id: Option<Uuid>` to `Inner`; initialize `None`.
- [x] Add `set_auction_id` plus the three no-arg mark methods, first-call-wins on their own field, storing `inner.t0.elapsed()`.
- [x] Recover a poisoned lock in all four, matching the other lock-taking methods on the collector.
- [x] Map all four into `TimingSnapshot` via `duration_ms`; `Uuid` is `Copy`, so the id needs no clone.
- [x] Tests: first-call-wins per mark, proved by sleeping between the two calls and asserting the value is unchanged; the id survives a request with no dispatch mark; unmarked snapshot yields all `None`.
- [x] `cargo test-fastly request_timing`, commit.

### Task 2: Auction call sites

**Files:**

- Modify: `crates/trusted-server-core/src/publisher.rs`
- Modify: `crates/trusted-server-core/src/auction/endpoints.rs`

**Interfaces:**

- Consumes: Task 1 methods; `observation.auction_id` (`AuctionObservationContext`), in scope wherever an observation is built.

- [x] Navigation path: `set_auction_id` at observation construction; `mark_auction_dispatched()` in the `DispatchAuctionOutcome::Dispatched` arm; `mark_auction_resolved()` after both `record_auction_wait` calls; `mark_auction_committed()` after both `write_bids_to_state` calls.
- [x] `/_ts/page-bids`: pull the collector off the request extensions; `set_auction_id` at observation construction; dispatch before `run_auction`, resolve on both its `Ok` and `Err` arms, commit after the bid map is built.
- [x] `POST /auction`: pull the collector off `parts.extensions` (the request is consumed by `into_parts` before the auction runs); same bracket around `run_auction`, commit once the OpenRTB response is converted.
- [x] Tests: extend the two existing collect-site tests to assert the resolve and commit offsets alongside `auction_wait_ms`.
- [x] `cargo test-fastly`, commit.

### Task 3: Row columns, datasource, fixture

**Files:**

- Modify: `crates/trusted-server-core/src/access_telemetry.rs`
- Modify: `tinybird/datasources/access_logs_raw.datasource`
- Modify: `tinybird/fixtures/access_logs_raw.ndjson`

- [x] `access_event_row`: add the three offset keys and `auction_id`, all nullable, after the existing phase keys.
- [x] Extend `row_serializes_nulls_for_missing_phases` (including `auction_id` in the nullable-column loop) and `row_serializes_recorded_phases_as_numbers` for the new keys.
- [x] Datasource: four schema columns with JSONPaths (`Nullable(UInt32)` ×3, `Nullable(UUID)`), appended at the end of SCHEMA and FORWARD_QUERY so existing column order stays stable.
- [x] Fixture: extend the existing row with a full timeline reusing the `auction_events_raw` fixture's UUID so the pair demonstrates the join, and add a second row covering the no-auction case.
- [x] Full gates: fmt, clippy (all six), test-fastly/axum/cloudflare/spin, parity. Commit.

### Task 4: Spec amendment

**Files:**

- Modify: `docs/superpowers/specs/2026-08-24-request-phase-timing-design.md`

- [x] Section 18: mark table including `set_auction_id`, the per-source instrumentation table, the null-semantics table, and the `Nullable(UUID)` rationale.
- [x] Section 18: correct the interpretation ladder so `time_elapsed_ms` is not shown last, since on a streamed body the headers commit before the seam collect.
- [x] Section 18: state the `R - D` versus `total_time_ms` reconciliation caveat and give the join query.
- [x] Section 9: add the four columns so the canonical column list the row builder's doc comment points at stays complete.
- [x] Docs format (`cd docs && npm run format`).
