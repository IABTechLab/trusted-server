# Auction Timeline Offsets Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Record three T0-anchored auction milestones (dispatched, resolved, committed) plus the auction id on `RequestTimings`, and emit them as four additive columns on the `access_logs_raw` row.

**Architecture:** Follows spec section 18 exactly. All state lives in the existing `RequestTimings` inner (same `try_lock`/first-call-wins/saturating model as `mark_headers_ready`); the row builder reads the values from `TimingSnapshot`, so no new emission path and no adapter changes.

**Tech Stack:** Rust (core crate only), Tinybird datasource file.

**Spec:** `docs/superpowers/specs/2026-08-24-request-phase-timing-design.md` section 18.

## Global Constraints

- Marks are first-call-wins; `try_lock` only; a contended lock drops the sample.
- Null offsets mean "no auction ran", never zero. `auction_id` sentinel is `none`.
- Column names: `auction_dispatched_ms`, `auction_resolved_ms`, `auction_committed_ms`, `auction_id`; JSONPaths `json:$.<name>`; FORWARD_QUERY extended in the same order.
- Dispatch mark records only on `DispatchAuctionOutcome::Dispatched`; a failed dispatch leaves all three offsets null (the auction dataset still records the failure).
- No header emission, no config surface, no changes outside `trusted-server-core` and `tinybird/`.

---

### Task 1: RequestTimings marks and snapshot fields

**Files:**
- Modify: `crates/trusted-server-core/src/request_timing.rs`

**Interfaces:**
- Produces: `mark_auction_dispatched(&self, auction_id: String)`, `mark_auction_resolved(&self)`, `mark_auction_committed(&self)`; `TimingSnapshot { auction_dispatched_ms, auction_resolved_ms, auction_committed_ms: Option<u32>, auction_id: Option<String>, .. }`

- [ ] Add `auction_dispatched`, `auction_resolved`, `auction_committed: Option<Duration>` and `auction_id: Option<String>` to `Inner`; initialize `None`.
- [ ] Add the three mark methods, first-call-wins on their own field, storing `inner.t0.elapsed()`; dispatched also stores the id first-call-wins.
- [ ] Map all four into `TimingSnapshot` via `duration_ms` / clone.
- [ ] Tests: first-call-wins per mark; snapshot maps offsets and id; unmarked snapshot yields all `None`.
- [ ] `cargo test-fastly request_timing`, commit.

### Task 2: Publisher call sites

**Files:**
- Modify: `crates/trusted-server-core/src/publisher.rs`

**Interfaces:**
- Consumes: Task 1 methods; `observation.auction_id` (`AuctionObservationContext`), in scope at the dispatch site.

- [ ] In the `DispatchAuctionOutcome::Dispatched` arm (~line 4341): `timings.mark_auction_dispatched(observation.auction_id.to_string());`
- [ ] After both `record_auction_wait` calls (collect sites ~3952 and ~4012): `mark_auction_resolved()`.
- [ ] After both `write_bids_to_state` calls (~3954 and ~4017): `mark_auction_committed()`.
- [ ] `cargo test-fastly`, commit.

### Task 3: Row columns and datasource

**Files:**
- Modify: `crates/trusted-server-core/src/access_telemetry.rs`
- Modify: `tinybird/datasources/access_logs_raw.datasource`

- [ ] `access_event_row`: add the three offset keys (nullable) and `auction_id` with `none` sentinel, after the existing phase keys.
- [ ] Extend `row_serializes_nulls_for_missing_phases` and `row_serializes_recorded_phases_as_numbers` for the new keys.
- [ ] Datasource: four schema columns with JSONPaths (`Nullable(UInt32)` ×3, `String`), appended at the end of SCHEMA and FORWARD_QUERY so existing column order stays stable.
- [ ] Full gates: fmt, clippy (all six), test-fastly/axum/cloudflare/spin, parity. Commit.
