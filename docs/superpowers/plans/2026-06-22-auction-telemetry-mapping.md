# Auction Telemetry Mapping Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a pure mapping layer in `trusted-server-core` that turns a real `OrchestrationResult` (a completed auction) into the Plan 1 telemetry inputs and the full row set, so a later wiring plan can call one function at the auction call sites.

**Architecture:** A new `auction::telemetry::mapping` module reads the per-provider outcomes the orchestrator already records (provider name, `BidStatus`, `response_time_ms`, and the `error_type` metadata for `launch_failed`/`parse_response`/`transport`) and produces `ProviderCallOutcome`s and a `Completed` `TerminalOutcome`, then delegates to the existing `build_auction_events`. Pure, no I/O, no edits to the orchestrator or any handler.

**Tech Stack:** Rust 2024, `serde_json` (only for reading `error_type` from metadata in tests/values).

## Global Constraints

Same as the core telemetry plan; every task implicitly includes these:

- Rust **2024 edition**. No `unwrap()` in non-test code (use `expect("should ...")`; `unwrap_or`/`unwrap_or_else` are allowed). No `println!`/`eprintln!`.
- Comments on their own line above the code, never inline. No imports inside functions; no wildcard imports outside `#[cfg(test)]` (`use super::*;` allowed there).
- Tests: Arrange-Act-Assert, `expect()` with `"should ..."` messages, descriptive assertion messages, `serde_json::json!` over raw JSON strings.
- Each public item has a doc comment.
- Git commit messages: sentence case, imperative, no semantic prefixes (`feat:`/`fix:`), no bracketed tags, no `Co-Authored-By` trailer. Use the exact message in each task's commit step.

**Scope boundary (what this plan deliberately does NOT do):** It does not call these functions from any handler, does not touch `run_auction`/`dispatch_auction`/`collect_dispatched_auction`, does not implement the Fastly sink, does not emit access logs, and does not handle SSAT abandonment. Those are later plans. This plan only adds pure, unit-tested core functions.

**Verified facts this plan relies on (from the current code):**

- `OrchestrationResult` (`crates/trusted-server-core/src/auction/orchestrator.rs`): `provider_responses: Vec<AuctionResponse>`, `mediator_response: Option<AuctionResponse>`, `winning_bids: HashMap<String, Bid>`, `total_time_ms: u64`, `metadata`.
- `AuctionResponse` (`auction/types.rs`): `provider: String`, `bids: Vec<Bid>`, `status: BidStatus`, `response_time_ms: u64`, `metadata: HashMap<String, serde_json::Value>`.
- On an `Error` response the orchestrator writes `metadata["error_type"]` to one of `"launch_failed"`, `"parse_response"`, `"transport"`.
- `BidStatus` variants: `Success`, `NoBid`, `Error`, `Pending`.

---

### Task 1: Map a completed result to provider-call outcomes

**Files:**

- Create: `crates/trusted-server-core/src/auction/telemetry/mapping.rs`
- Modify: `crates/trusted-server-core/src/auction/telemetry/mod.rs` (declare `mapping`, re-export `provider_calls_from_result`)
- Test: inline `#[cfg(test)]` in `mapping.rs`

**Interfaces:**

- Consumes: `OrchestrationResult` (orchestrator), `AuctionResponse`, `BidStatus`, `Bid` (auction/types), and `ProviderCallOutcome`, `ProviderCallStatus`, `ProviderRole` (telemetry::types).
- Produces: `pub fn provider_calls_from_result(result: &OrchestrationResult) -> Vec<ProviderCallOutcome>` — one outcome per `provider_responses` entry (role `Bidder`) plus one for `mediator_response` when present (role `Mediator`). Status mapping: `Success -> Success`, `NoBid -> NoBid`, `Pending -> Timeout`, `Error -> {launch_failed: LaunchError, parse_response: ParseError, transport: TransportError, else: TransportError}`.

- [ ] **Step 1: Write the failing test**

Create `crates/trusted-server-core/src/auction/telemetry/mapping.rs` with the test module first:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::auction::orchestrator::OrchestrationResult;
    use crate::auction::types::{AuctionResponse, Bid, BidStatus};
    use crate::auction::telemetry::types::{ProviderCallStatus, ProviderRole};
    use std::collections::HashMap;

    fn bid(slot: &str, bidder: &str) -> Bid {
        Bid {
            slot_id: slot.to_string(),
            price: Some(1.0),
            currency: "USD".to_string(),
            creative: None,
            adomain: None,
            bidder: bidder.to_string(),
            width: 300,
            height: 250,
            nurl: None,
            burl: None,
            ad_id: None,
            cache_id: None,
            cache_host: None,
            cache_path: None,
            metadata: HashMap::new(),
        }
    }

    fn response(
        provider: &str,
        status: BidStatus,
        time: u64,
        bids: Vec<Bid>,
        error_type: Option<&str>,
    ) -> AuctionResponse {
        let mut metadata = HashMap::new();
        if let Some(kind) = error_type {
            metadata.insert("error_type".to_string(), serde_json::json!(kind));
        }
        AuctionResponse {
            provider: provider.to_string(),
            bids,
            status,
            response_time_ms: time,
            metadata,
        }
    }

    fn result(
        provider_responses: Vec<AuctionResponse>,
        mediator_response: Option<AuctionResponse>,
    ) -> OrchestrationResult {
        OrchestrationResult {
            provider_responses,
            mediator_response,
            winning_bids: HashMap::new(),
            total_time_ms: 0,
            metadata: HashMap::new(),
        }
    }

    #[test]
    fn maps_each_status_to_the_expected_provider_call_status() {
        let res = result(
            vec![
                response("prebid", BidStatus::Success, 40, vec![bid("s1", "kargo")], None),
                response("rubicon", BidStatus::NoBid, 30, vec![], None),
                response("ix", BidStatus::Error, 10, vec![], Some("launch_failed")),
                response("appnexus", BidStatus::Error, 55, vec![], Some("parse_response")),
                response("openx", BidStatus::Error, 60, vec![], Some("transport")),
                response("smaato", BidStatus::Error, 5, vec![], None),
                response("teads", BidStatus::Pending, 70, vec![], None),
            ],
            None,
        );

        let calls = provider_calls_from_result(&res);

        assert_eq!(calls.len(), 7, "should emit one outcome per provider response");
        assert_eq!(calls[0].status, ProviderCallStatus::Success, "Success maps to Success");
        assert_eq!(calls[0].bid_count, Some(1), "should count returned bids");
        assert_eq!(calls[0].response_time_ms, Some(40), "should carry response time");
        assert_eq!(calls[0].role, ProviderRole::Bidder, "provider responses are bidders");
        assert_eq!(calls[1].status, ProviderCallStatus::NoBid, "NoBid maps to NoBid");
        assert_eq!(calls[2].status, ProviderCallStatus::LaunchError, "launch_failed maps to LaunchError");
        assert_eq!(calls[3].status, ProviderCallStatus::ParseError, "parse_response maps to ParseError");
        assert_eq!(calls[4].status, ProviderCallStatus::TransportError, "transport maps to TransportError");
        assert_eq!(
            calls[5].status,
            ProviderCallStatus::TransportError,
            "an Error with no recognized error_type falls back to TransportError"
        );
        assert_eq!(calls[6].status, ProviderCallStatus::Timeout, "Pending maps to Timeout");
    }

    #[test]
    fn appends_a_mediator_outcome_when_present() {
        let res = result(
            vec![response("prebid", BidStatus::Success, 40, vec![bid("s1", "kargo")], None)],
            Some(response("mediator", BidStatus::Success, 12, vec![], None)),
        );

        let calls = provider_calls_from_result(&res);

        assert_eq!(calls.len(), 2, "should append one outcome for the mediator");
        let mediator = calls.last().expect("should have a mediator outcome");
        assert_eq!(mediator.role, ProviderRole::Mediator, "mediator outcome uses the Mediator role");
        assert_eq!(mediator.provider, "mediator", "should carry the mediator provider name");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p trusted-server-core telemetry::mapping`
Expected: FAIL to compile (`mapping` module not declared; `provider_calls_from_result` not found).

- [ ] **Step 3: Write minimal implementation**

Prepend to `mapping.rs` (above the test module):

```rust
//! Maps a real `OrchestrationResult` into telemetry inputs.
//!
//! This is the adapter between the orchestrator's output types and the pure
//! telemetry builder. It performs no I/O and does not modify the auction.

use crate::auction::orchestrator::OrchestrationResult;
use crate::auction::telemetry::types::{
    ProviderCallOutcome, ProviderCallStatus, ProviderRole,
};
use crate::auction::types::{AuctionResponse, BidStatus};

/// Build one provider-call outcome per provider response, plus one for the
/// mediator when a mediator response is present.
#[must_use]
pub fn provider_calls_from_result(result: &OrchestrationResult) -> Vec<ProviderCallOutcome> {
    let mut calls: Vec<ProviderCallOutcome> = result
        .provider_responses
        .iter()
        .map(|response| provider_call_outcome(response, ProviderRole::Bidder))
        .collect();
    if let Some(mediator) = &result.mediator_response {
        calls.push(provider_call_outcome(mediator, ProviderRole::Mediator));
    }
    calls
}

/// Map one response to a provider-call outcome with the given role.
fn provider_call_outcome(response: &AuctionResponse, role: ProviderRole) -> ProviderCallOutcome {
    ProviderCallOutcome {
        provider: response.provider.clone(),
        role,
        status: provider_call_status(response),
        response_time_ms: Some(clamp_u32(response.response_time_ms)),
        bid_count: Some(clamp_u16(response.bids.len())),
    }
}

/// Classify a response into a provider-call status. For `Error`, read the
/// orchestrator's `error_type` metadata; an unrecognized or absent value falls
/// back to `TransportError` since the orchestrator only emits the three known
/// error types.
fn provider_call_status(response: &AuctionResponse) -> ProviderCallStatus {
    match response.status {
        BidStatus::Success => ProviderCallStatus::Success,
        BidStatus::NoBid => ProviderCallStatus::NoBid,
        BidStatus::Pending => ProviderCallStatus::Timeout,
        BidStatus::Error => match response
            .metadata
            .get("error_type")
            .and_then(|value| value.as_str())
        {
            Some("launch_failed") => ProviderCallStatus::LaunchError,
            Some("parse_response") => ProviderCallStatus::ParseError,
            Some("transport") => ProviderCallStatus::TransportError,
            _ => ProviderCallStatus::TransportError,
        },
    }
}

/// Clamp a `u64` millisecond count into the `u32` schema column without
/// panicking.
fn clamp_u32(value: u64) -> u32 {
    value.min(u64::from(u32::MAX)) as u32
}

/// Clamp a count into the `u16` schema column without panicking.
fn clamp_u16(value: usize) -> u16 {
    value.min(usize::from(u16::MAX)) as u16
}
```

Update `mod.rs`: add `pub mod mapping;` (alphabetically, before `pub mod sink;`) and add the re-export `pub use mapping::provider_calls_from_result;` near the other `pub use` lines. The full module file should read:

```rust
//! Pure auction telemetry: row types, builder, and sink abstraction.
//!
//! Wiring into the orchestrator, SSAT dispatch/collect, and the Fastly sink
//! lives in separate modules; this module performs no I/O.

pub mod builder;
pub mod mapping;
pub mod sink;
pub mod types;

pub use builder::build_auction_events;
pub use mapping::provider_calls_from_result;
pub use sink::{AuctionEventSink, InMemorySink, NoopSink};
pub use types::{
    to_ndjson, AuctionEventRow, AuctionObservationContext, AuctionSource, EventKind,
    ProviderCallOutcome, ProviderCallStatus, ProviderRole, TerminalOutcome, TerminalStatus,
};
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p trusted-server-core telemetry::mapping`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-core/src/auction/telemetry/mapping.rs crates/trusted-server-core/src/auction/telemetry/mod.rs
git commit -m "Map orchestration result to provider-call telemetry outcomes"
```

---

### Task 2: Build the full completed-auction row set from a result

**Files:**

- Modify: `crates/trusted-server-core/src/auction/telemetry/mapping.rs` (add two functions + tests)
- Modify: `crates/trusted-server-core/src/auction/telemetry/mod.rs` (re-export the two new functions)
- Test: inline `#[cfg(test)]` in `mapping.rs`

**Interfaces:**

- Consumes: `provider_calls_from_result` (Task 1), `build_auction_events`, `AuctionObservationContext`, `AuctionEventRow`, `TerminalOutcome`, `TerminalStatus` (telemetry), `OrchestrationResult`.
- Produces:
  - `pub fn completed_outcome(result: &OrchestrationResult, slot_count: u16) -> TerminalOutcome` — `status = Completed`, `reason = None`, `slot_count = Some(slot_count)`, `total_time_ms = Some(result.total_time_ms clamped)`, `winning_bid_count = Some(result.winning_bids.len() clamped)`.
  - `pub fn build_completed_auction_events(ctx: &AuctionObservationContext, slot_count: u16, result: &OrchestrationResult) -> Vec<AuctionEventRow>` — the single entry point a later wiring plan calls for a completed auction. Equivalent to `build_auction_events(ctx, &completed_outcome(result, slot_count), &provider_calls_from_result(result), Some(result))`.

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `mapping.rs` (the `bid`, `response`, `result` helpers from Task 1 are already present; add the new imports and tests):

```rust
use crate::auction::telemetry::types::{
    AuctionObservationContext, AuctionSource, EventKind, TerminalStatus,
};

fn ctx() -> AuctionObservationContext {
    AuctionObservationContext {
        auction_id: uuid::Uuid::nil(),
        source: AuctionSource::AuctionApi,
        publisher_domain: "example.com".to_string(),
        page_path: "/p".to_string(),
        country: "US".to_string(),
        region: None,
        is_mobile: 1,
        is_known_browser: 1,
        gdpr_applies: false,
        consent_present: true,
    }
}

#[test]
fn completed_outcome_carries_counts_from_the_result() {
    let mut res = result(
        vec![response("prebid", BidStatus::Success, 40, vec![bid("s1", "kargo")], None)],
        None,
    );
    res.total_time_ms = 88;
    res.winning_bids.insert("s1".to_string(), bid("s1", "kargo"));

    let outcome = completed_outcome(&res, 2);

    assert_eq!(outcome.status, TerminalStatus::Completed, "should be Completed");
    assert!(outcome.reason.is_none(), "completed auctions have no reason");
    assert_eq!(outcome.slot_count, Some(2), "should carry the requested slot count");
    assert_eq!(outcome.total_time_ms, Some(88), "should carry total time");
    assert_eq!(outcome.winning_bid_count, Some(1), "should count winning bids");
}

#[test]
fn build_completed_auction_events_emits_summary_provider_and_bid_rows() {
    let mut res = result(
        vec![
            response("prebid", BidStatus::Success, 40, vec![bid("s1", "kargo")], None),
            response("aps", BidStatus::NoBid, 30, vec![], None),
        ],
        None,
    );
    res.winning_bids.insert("s1".to_string(), bid("s1", "kargo"));

    let rows = build_completed_auction_events(&ctx(), 1, &res);

    assert_eq!(
        rows.iter().filter(|r| r.event_kind == EventKind::Summary).count(),
        1,
        "should emit exactly one summary"
    );
    assert_eq!(
        rows.iter().filter(|r| r.event_kind == EventKind::ProviderCall).count(),
        2,
        "should emit one provider-call row per provider"
    );
    assert_eq!(
        rows.iter().filter(|r| r.event_kind == EventKind::Bid).count(),
        1,
        "should emit a bid row for the returned bid"
    );
    let summary = rows
        .iter()
        .find(|r| r.event_kind == EventKind::Summary)
        .expect("should have a summary row");
    assert_eq!(summary.terminal_status, Some(TerminalStatus::Completed), "summary is Completed");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p trusted-server-core telemetry::mapping`
Expected: FAIL to compile (`completed_outcome` and `build_completed_auction_events` not found).

- [ ] **Step 3: Write minimal implementation**

Add to the import block at the top of `mapping.rs`:

```rust
use crate::auction::telemetry::builder::build_auction_events;
use crate::auction::telemetry::types::{
    AuctionEventRow, AuctionObservationContext, TerminalOutcome, TerminalStatus,
};
```

Add the two functions above the test module (after the existing functions):

```rust
/// Build the terminal outcome for a completed auction. `slot_count` is the
/// number of requested slots, which the result alone does not carry.
#[must_use]
pub fn completed_outcome(result: &OrchestrationResult, slot_count: u16) -> TerminalOutcome {
    TerminalOutcome {
        status: TerminalStatus::Completed,
        reason: None,
        slot_count: Some(slot_count),
        total_time_ms: Some(clamp_u32(result.total_time_ms)),
        winning_bid_count: Some(clamp_u16(result.winning_bids.len())),
    }
}

/// Build all telemetry rows for a completed auction. This is the single entry
/// point a wiring layer calls when `run_auction`/`collect_dispatched_auction`
/// returns an `OrchestrationResult`.
#[must_use]
pub fn build_completed_auction_events(
    ctx: &AuctionObservationContext,
    slot_count: u16,
    result: &OrchestrationResult,
) -> Vec<AuctionEventRow> {
    let outcome = completed_outcome(result, slot_count);
    let provider_calls = provider_calls_from_result(result);
    build_auction_events(ctx, &outcome, &provider_calls, Some(result))
}
```

Update `mod.rs` re-export line for `mapping` to include all three functions:

```rust
pub use mapping::{build_completed_auction_events, completed_outcome, provider_calls_from_result};
```

- [ ] **Step 4: Run test to verify it passes, then the gates**

Run: `cargo test -p trusted-server-core telemetry`
Expected: PASS (the full telemetry suite, including the 4 mapping tests).

Run: `cargo fmt --all -- --check`
Expected: no diff.

Run: `cargo clippy -p trusted-server-core --all-targets --all-features -- -D warnings`
Expected: no warnings.

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-core/src/auction/telemetry/mapping.rs crates/trusted-server-core/src/auction/telemetry/mod.rs
git commit -m "Build completed-auction telemetry rows from orchestration result"
```

---

## Self-Review

**Spec coverage (this plan's slice):** Provider-call status vocabulary mapping (`success`/`nobid`/`launch_error`/`parse_error`/`transport_error`/`timeout`) from the orchestrator's existing outputs: Task 1. Completed terminal outcome and the single `build_completed_auction_events` entry point: Task 2. Both are pure and reuse the Plan 1 builder.

**Deferred (not gaps in this plan):** Calling these from `handle_auction`/`handle_page_bids`/SSAT collect; abandoned/skipped/dispatch-failed/execution-failed outcomes; `media_type` and the observation-context construction from `EcContext`/geo/`DeviceSignals`; the Fastly sink and `event_ts`; access logs. Those are later plans.

**Placeholder scan:** No `TBD`/`TODO`; every code step is complete.

**Type consistency:** `provider_calls_from_result`, `completed_outcome`, `build_completed_auction_events`, and `build_auction_events` signatures match across tasks and match the verified `OrchestrationResult`/`AuctionResponse`/`BidStatus` field set. `clamp_u32`/`clamp_u16` are defined in Task 1 and reused in Task 2.
