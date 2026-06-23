# Core Auction Telemetry Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a pure, platform-agnostic telemetry layer in `trusted-server-core` that turns an auction observation into the summary / provider-call / bid rows the Tinybird pipeline ingests, plus the sink abstraction the Fastly adapter will implement.

**Architecture:** A new `auction::telemetry` module holds value types (no I/O, no clock, no Fastly), a pure `build_auction_events` builder over the existing `OrchestrationResult`/`Bid`/`AuctionResponse` types, and an `AuctionEventSink` trait with in-memory/no-op test implementations. Wiring into `run_auction`, the SSAT dispatch/collect path, and the Fastly sink are deliberately out of scope here (separate plans) so this plan stands alone and is fully unit-testable with `cargo test`.

**Tech Stack:** Rust 2024 edition, `serde` + `serde_json` for NDJSON, `uuid` for the telemetry id type.

## Global Constraints

Copied verbatim from the project conventions; every task implicitly includes these:

- Rust **2024 edition**.
- No `unwrap()` in non-test code; use `expect("should ...")` only where a panic is truly impossible. `Option::unwrap_or`/`unwrap_or_else` are allowed (they do not panic).
- No `println!`/`eprintln!`; use `log` macros if logging is needed.
- Comments on their own line **above** the code, never inline.
- `use super::*;` is allowed only in `#[cfg(test)]` modules. No other wildcard imports. No imports inside functions.
- Tests: Arrange-Act-Assert, `expect()`/`expect_err()` with `"should ..."` messages, descriptive assertion messages, `serde_json::json!` instead of raw JSON strings.
- Prefer `&[T]` over `&Vec<T>`. Functions take no more than 7 arguments.
- Each public item has a doc comment: one-line summary, blank line, details.
- Git commit messages: sentence case, imperative, no semantic prefixes (`feat:`/`fix:`), no bracketed tags, no `Co-Authored-By` trailer.
- `event_ts` is intentionally **not** produced here. Core is clock-free; the Fastly sink (separate plan) stamps `event_ts` at serialization, or Tinybird defaults it at ingestion. Do not add a clock dependency to core.
- `media_type` on bid rows is left `None` here; it requires the request slot definition, which this layer does not receive. A later wiring plan fills it. Do not guess it from the creative.

---

### Task 1: Module scaffold and serialized enums

**Files:**

- Create: `crates/trusted-server-core/src/auction/telemetry/mod.rs`
- Create: `crates/trusted-server-core/src/auction/telemetry/types.rs`
- Modify: `crates/trusted-server-core/src/auction/mod.rs` (add module declaration + re-exports)
- Test: inline `#[cfg(test)]` in `types.rs`

**Interfaces:**

- Produces: enums `AuctionSource`, `TerminalStatus`, `ProviderCallStatus`, `ProviderRole`, `EventKind`, each `#[derive(Serialize)]` with the exact wire strings asserted below.

- [ ] **Step 1: Write the failing test**

Create `crates/trusted-server-core/src/auction/telemetry/types.rs` with only the test module first:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enums_serialize_to_expected_wire_strings() {
        assert_eq!(
            serde_json::to_string(&AuctionSource::InitialNavigation)
                .expect("should serialize source"),
            "\"initial_navigation\"",
            "should use snake_case wire form"
        );
        assert_eq!(
            serde_json::to_string(&TerminalStatus::ExecutionFailed)
                .expect("should serialize status"),
            "\"execution_failed\"",
            "should use snake_case wire form"
        );
        assert_eq!(
            serde_json::to_string(&ProviderCallStatus::NoBid)
                .expect("should serialize provider status"),
            "\"nobid\"",
            "should render NoBid as the single token nobid"
        );
        assert_eq!(
            serde_json::to_string(&EventKind::ProviderCall)
                .expect("should serialize kind"),
            "\"provider_call\"",
            "should use snake_case wire form"
        );
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p trusted-server-core telemetry::types`
Expected: FAIL to compile (`AuctionSource` not found) — module not declared yet.

- [ ] **Step 3: Write minimal implementation**

Prepend to `types.rs` (above the test module):

```rust
//! Value types for auction telemetry rows.
//!
//! These types are pure data: no I/O, no clock, no Fastly dependency. They are
//! shared by the builder and serialized as NDJSON by the Fastly sink.

use serde::Serialize;

/// Auction initiation path that produced an observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuctionSource {
    /// Initial publisher navigation via split-phase SSAT.
    InitialNavigation,
    /// Single-page-app navigation via `GET /__ts/page-bids`.
    SpaNavigation,
    /// Explicit `POST /auction` API call.
    AuctionApi,
}

/// Terminal status of a candidate auction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalStatus {
    /// Produced an `OrchestrationResult`, including a valid zero-bid result.
    Completed,
    /// Synchronous orchestration failed.
    ExecutionFailed,
    /// No provider request could be launched.
    DispatchFailed,
    /// Split-phase SSAT launched providers but could not collect them.
    Abandoned,
    /// Matched slots existed but policy prevented initiation.
    Skipped,
}

/// Outcome of a single provider call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderCallStatus {
    /// Provider returned at least one bid.
    Success,
    /// Provider responded with no bid.
    #[serde(rename = "nobid")]
    NoBid,
    /// Provider request could not be launched.
    LaunchError,
    /// Provider response could not be parsed.
    ParseError,
    /// Provider request failed in transport.
    TransportError,
    /// Provider did not respond before the auction deadline.
    Timeout,
    /// Provider was dispatched but never collected.
    Abandoned,
}

/// Role a provider played in the auction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderRole {
    /// A bidder.
    Bidder,
    /// The mediation layer.
    Mediator,
}

/// Discriminator for the row grain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    /// One per candidate auction.
    Summary,
    /// One per provider call.
    ProviderCall,
    /// One per returned bid (or unmatched mediator winner).
    Bid,
}
```

Create `crates/trusted-server-core/src/auction/telemetry/mod.rs`:

```rust
//! Pure auction telemetry: row types, builder, and sink abstraction.
//!
//! Wiring into the orchestrator, SSAT dispatch/collect, and the Fastly sink
//! lives in separate modules; this module performs no I/O.

pub mod types;

pub use types::{
    AuctionSource, EventKind, ProviderCallStatus, ProviderRole, TerminalStatus,
};
```

In `crates/trusted-server-core/src/auction/mod.rs`, add the module declaration next to the other `pub mod` lines (after `pub mod orchestrator;`):

```rust
pub mod telemetry;
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p trusted-server-core telemetry::types`
Expected: PASS (1 test).

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-core/src/auction/telemetry/ crates/trusted-server-core/src/auction/mod.rs
git commit -m "Add auction telemetry module scaffold and serialized enums"
```

---

### Task 2: Observation context and outcome inputs

**Files:**

- Modify: `crates/trusted-server-core/src/auction/telemetry/types.rs`
- Modify: `crates/trusted-server-core/src/auction/telemetry/mod.rs` (re-export new types)
- Test: inline `#[cfg(test)]` in `types.rs`

**Interfaces:**

- Consumes: `AuctionSource` (Task 1).
- Produces: `AuctionObservationContext`, `TerminalOutcome`, `ProviderCallOutcome` structs with the public fields listed below. Later tasks construct these directly.

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `types.rs`:

```rust
#[test]
fn observation_context_holds_snapshotted_primitives() {
    let ctx = AuctionObservationContext {
        auction_id: uuid::Uuid::nil(),
        source: AuctionSource::AuctionApi,
        publisher_domain: "example.com".to_string(),
        page_path: "/news".to_string(),
        country: "US".to_string(),
        region: Some("CA".to_string()),
        is_mobile: 1,
        is_known_browser: 1,
        gdpr_applies: false,
        consent_present: true,
    };
    assert_eq!(ctx.source, AuctionSource::AuctionApi, "should retain source");
    assert_eq!(ctx.region.as_deref(), Some("CA"), "should retain region");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p trusted-server-core telemetry::types`
Expected: FAIL to compile (`AuctionObservationContext` not found).

- [ ] **Step 3: Write minimal implementation**

Add to `types.rs` (above the test module). Note the `use` for `uuid::Uuid`:

```rust
use uuid::Uuid;

/// Immutable, PII-free snapshot describing one candidate auction.
///
/// Built once per candidate auction by the wiring layer and carried to the
/// terminal observation point. Contains no EC id, raw user agent, IP, or
/// internal `AuctionRequest.id`.
#[derive(Debug, Clone)]
pub struct AuctionObservationContext {
    /// Telemetry-only identifier, minted independently of any request id.
    pub auction_id: Uuid,
    /// Initiation path.
    pub source: AuctionSource,
    /// Publisher domain.
    pub publisher_domain: String,
    /// Bounded, normalized route. No query string or fragment.
    pub page_path: String,
    /// Coarse country from geo lookup.
    pub country: String,
    /// Coarse region from geo lookup, when available.
    pub region: Option<String>,
    /// `0` = desktop, `1` = mobile, `2` = unknown.
    pub is_mobile: u8,
    /// `0` = bot, `1` = browser, `2` = unknown.
    pub is_known_browser: u8,
    /// Whether GDPR applies for this request.
    pub gdpr_applies: bool,
    /// Whether any consent signal was present.
    pub consent_present: bool,
}

/// Terminal outcome of a candidate auction, used for the summary row.
#[derive(Debug, Clone)]
pub struct TerminalOutcome {
    /// Terminal status.
    pub status: TerminalStatus,
    /// Bounded machine-readable reason, e.g. for `skipped` cases.
    pub reason: Option<String>,
    /// Requested slot count.
    pub slot_count: Option<u16>,
    /// Elapsed time until completion or abandonment.
    pub total_time_ms: Option<u32>,
    /// Winning bid count; zero for non-completed outcomes.
    pub winning_bid_count: Option<u16>,
}

/// Outcome of a single provider call, used for provider-call rows.
#[derive(Debug, Clone)]
pub struct ProviderCallOutcome {
    /// Provider name, e.g. `prebid`, `aps`, or a mediator name.
    pub provider: String,
    /// Role the provider played.
    pub role: ProviderRole,
    /// Provider call status.
    pub status: ProviderCallStatus,
    /// Provider call latency, when known.
    pub response_time_ms: Option<u32>,
    /// Number of parsed bids, when known.
    pub bid_count: Option<u16>,
}
```

Update `mod.rs` re-export to include the new types:

```rust
pub use types::{
    AuctionObservationContext, AuctionSource, EventKind, ProviderCallOutcome,
    ProviderCallStatus, ProviderRole, TerminalOutcome, TerminalStatus,
};
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p trusted-server-core telemetry::types`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-core/src/auction/telemetry/
git commit -m "Add observation context and outcome input types for telemetry"
```

---

### Task 3: Row struct and NDJSON serialization

**Files:**

- Modify: `crates/trusted-server-core/src/auction/telemetry/types.rs`
- Modify: `crates/trusted-server-core/src/auction/telemetry/mod.rs` (re-export `AuctionEventRow`, `to_ndjson`)
- Test: inline `#[cfg(test)]` in `types.rs`

**Interfaces:**

- Consumes: all enums + `AuctionObservationContext` (Tasks 1-2).
- Produces:
  - `AuctionEventRow` (all public fields, `#[derive(Serialize)]`).
  - `AuctionEventRow::base(ctx: &AuctionObservationContext, kind: EventKind) -> AuctionEventRow` — shared fields filled, all kind-specific fields `None`.
  - `pub fn to_ndjson(rows: &[AuctionEventRow]) -> Result<String, serde_json::Error>` — newline-delimited, one compact JSON object per line, no trailing newline.

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `types.rs`:

```rust
fn sample_context() -> AuctionObservationContext {
    AuctionObservationContext {
        auction_id: uuid::Uuid::nil(),
        source: AuctionSource::SpaNavigation,
        publisher_domain: "example.com".to_string(),
        page_path: "/p".to_string(),
        country: "US".to_string(),
        region: None,
        is_mobile: 0,
        is_known_browser: 1,
        gdpr_applies: true,
        consent_present: false,
    }
}

#[test]
fn base_row_fills_shared_fields_and_nulls_the_rest() {
    let row = AuctionEventRow::base(&sample_context(), EventKind::Summary);
    assert_eq!(row.event_kind, EventKind::Summary, "should set kind");
    assert_eq!(row.gdpr_applies, 1, "should map true to 1");
    assert_eq!(row.consent_present, 0, "should map false to 0");
    assert!(row.terminal_status.is_none(), "should null summary fields");
    assert!(row.provider.is_none(), "should null provider fields");
    assert!(row.slot_id.is_none(), "should null bid fields");
}

#[test]
fn to_ndjson_is_one_compact_object_per_line() {
    let rows = vec![
        AuctionEventRow::base(&sample_context(), EventKind::Summary),
        AuctionEventRow::base(&sample_context(), EventKind::Bid),
    ];
    let ndjson = to_ndjson(&rows).expect("should serialize rows");
    let lines: Vec<&str> = ndjson.split('\n').collect();
    assert_eq!(lines.len(), 2, "should emit one line per row with no trailing newline");
    for line in &lines {
        let value: serde_json::Value =
            serde_json::from_str(line).expect("each line should be valid JSON");
        assert!(value.get("event_kind").is_some(), "should always include event_kind");
        assert!(value.get("auction_id").is_some(), "should always include auction_id");
        assert!(value.get("region").is_some(), "should include region key even when null");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p trusted-server-core telemetry::types`
Expected: FAIL to compile (`AuctionEventRow` not found).

- [ ] **Step 3: Write minimal implementation**

Add to `types.rs` (above the test module):

```rust
/// One serialized telemetry row. A single flat shape covers all three grains;
/// fields that do not apply to a row kind are `None` and serialize to JSON
/// `null` so the NDJSON shape is stable across rows.
///
/// `event_ts` is intentionally absent: core is clock-free and the sink or
/// Tinybird supplies the timestamp.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AuctionEventRow {
    /// Row grain discriminator.
    pub event_kind: EventKind,
    /// Telemetry id, hyphenated UUID string.
    pub auction_id: String,
    /// Initiation path.
    pub auction_source: AuctionSource,
    /// Publisher domain.
    pub publisher_domain: String,
    /// Bounded normalized route.
    pub page_path: String,
    /// Coarse country.
    pub country: String,
    /// Coarse region.
    pub region: Option<String>,
    /// `0`/`1`/`2` device class.
    pub is_mobile: u8,
    /// `0`/`1`/`2` browser-legitimacy class.
    pub is_known_browser: u8,
    /// `0`/`1`.
    pub gdpr_applies: u8,
    /// `0`/`1`.
    pub consent_present: u8,
    /// Summary: terminal status.
    pub terminal_status: Option<TerminalStatus>,
    /// Summary: bounded reason.
    pub terminal_reason: Option<String>,
    /// Summary: requested slots.
    pub slot_count: Option<u16>,
    /// Summary: elapsed ms.
    pub total_time_ms: Option<u32>,
    /// Summary: winning bid count.
    pub winning_bid_count: Option<u16>,
    /// Provider-call and bid: provider name.
    pub provider: Option<String>,
    /// Provider-call: role.
    pub provider_role: Option<ProviderRole>,
    /// Provider-call: status.
    pub status: Option<ProviderCallStatus>,
    /// Provider-call: latency ms.
    pub provider_response_time_ms: Option<u32>,
    /// Provider-call: parsed bid count.
    pub provider_bid_count: Option<u16>,
    /// Bid: slot id.
    pub slot_id: Option<String>,
    /// Bid: returned creative width.
    pub slot_w: Option<u16>,
    /// Bid: returned creative height.
    pub slot_h: Option<u16>,
    /// Bid: media type, filled by a later wiring plan.
    pub media_type: Option<String>,
    /// Bid: seat/bidder name.
    pub seat: Option<String>,
    /// Bid: decoded CPM when available.
    pub price_cpm: Option<f64>,
    /// Bid: currency.
    pub currency: Option<String>,
    /// Bid: `1` for the one canonical winning row per slot, else `0`.
    pub is_win: Option<u8>,
    /// Bid: first advertiser domain.
    pub ad_domain: Option<String>,
    /// Bid: creative id.
    pub ad_id: Option<String>,
}

impl AuctionEventRow {
    /// Build a row with the shared columns filled from `ctx` and every
    /// kind-specific column set to `None`.
    #[must_use]
    pub fn base(ctx: &AuctionObservationContext, kind: EventKind) -> Self {
        Self {
            event_kind: kind,
            auction_id: ctx.auction_id.to_string(),
            auction_source: ctx.source,
            publisher_domain: ctx.publisher_domain.clone(),
            page_path: ctx.page_path.clone(),
            country: ctx.country.clone(),
            region: ctx.region.clone(),
            is_mobile: ctx.is_mobile,
            is_known_browser: ctx.is_known_browser,
            gdpr_applies: u8::from(ctx.gdpr_applies),
            consent_present: u8::from(ctx.consent_present),
            terminal_status: None,
            terminal_reason: None,
            slot_count: None,
            total_time_ms: None,
            winning_bid_count: None,
            provider: None,
            provider_role: None,
            status: None,
            provider_response_time_ms: None,
            provider_bid_count: None,
            slot_id: None,
            slot_w: None,
            slot_h: None,
            media_type: None,
            seat: None,
            price_cpm: None,
            currency: None,
            is_win: None,
            ad_domain: None,
            ad_id: None,
        }
    }
}

/// Serialize rows as newline-delimited JSON with no trailing newline.
///
/// # Errors
///
/// Returns the underlying `serde_json` error if a row cannot be serialized.
pub fn to_ndjson(rows: &[AuctionEventRow]) -> Result<String, serde_json::Error> {
    let mut out = String::new();
    for (index, row) in rows.iter().enumerate() {
        if index > 0 {
            out.push('\n');
        }
        out.push_str(&serde_json::to_string(row)?);
    }
    Ok(out)
}
```

Update `mod.rs` re-export:

```rust
pub use types::{
    to_ndjson, AuctionEventRow, AuctionObservationContext, AuctionSource, EventKind,
    ProviderCallOutcome, ProviderCallStatus, ProviderRole, TerminalOutcome, TerminalStatus,
};
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p trusted-server-core telemetry::types`
Expected: PASS (4 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-core/src/auction/telemetry/
git commit -m "Add flat telemetry row struct and NDJSON serialization"
```

---

### Task 4: Sink abstraction with test implementations

**Files:**

- Create: `crates/trusted-server-core/src/auction/telemetry/sink.rs`
- Modify: `crates/trusted-server-core/src/auction/telemetry/mod.rs` (declare `sink`, re-export)
- Test: inline `#[cfg(test)]` in `sink.rs`

**Interfaces:**

- Consumes: `AuctionEventRow` (Task 3).
- Produces:
  - `pub trait AuctionEventSink: Send + Sync { fn emit(&self, rows: &[AuctionEventRow]); }`
  - `NoopSink` (does nothing).
  - `InMemorySink` with `fn rows(&self) -> Vec<AuctionEventRow>` for tests.

- [ ] **Step 1: Write the failing test**

Create `crates/trusted-server-core/src/auction/telemetry/sink.rs` with the test module first:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::auction::telemetry::types::{AuctionObservationContext, AuctionSource, EventKind};

    fn ctx() -> AuctionObservationContext {
        AuctionObservationContext {
            auction_id: uuid::Uuid::nil(),
            source: AuctionSource::AuctionApi,
            publisher_domain: "example.com".to_string(),
            page_path: "/p".to_string(),
            country: "US".to_string(),
            region: None,
            is_mobile: 2,
            is_known_browser: 2,
            gdpr_applies: false,
            consent_present: false,
        }
    }

    #[test]
    fn in_memory_sink_captures_emitted_rows() {
        let sink = InMemorySink::default();
        let rows = vec![AuctionEventRow::base(&ctx(), EventKind::Summary)];
        sink.emit(&rows);
        sink.emit(&rows);
        assert_eq!(sink.rows().len(), 2, "should accumulate rows across emit calls");
    }

    #[test]
    fn noop_sink_accepts_rows() {
        let sink = NoopSink;
        sink.emit(&[AuctionEventRow::base(&ctx(), EventKind::Summary)]);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p trusted-server-core telemetry::sink`
Expected: FAIL to compile (`sink` module not declared; `AuctionEventSink` not found).

- [ ] **Step 3: Write minimal implementation**

Prepend to `sink.rs` (above the test module):

```rust
//! Sink abstraction for emitting telemetry rows.
//!
//! Core defines the trait and test implementations. The Fastly adapter provides
//! the real implementation that serializes rows to a named log endpoint.

use std::sync::Mutex;

use crate::auction::telemetry::types::AuctionEventRow;

/// Destination for telemetry rows.
///
/// Implementations must be cheap and non-blocking from the caller's view; the
/// Fastly implementation performs a buffered host write.
pub trait AuctionEventSink: Send + Sync {
    /// Emit a batch of rows for one auction observation.
    fn emit(&self, rows: &[AuctionEventRow]);
}

/// Sink that discards rows. Used where telemetry is disabled and in tests.
#[derive(Debug, Default)]
pub struct NoopSink;

impl AuctionEventSink for NoopSink {
    fn emit(&self, _rows: &[AuctionEventRow]) {}
}

/// Sink that accumulates rows in memory for assertions in tests.
#[derive(Debug, Default)]
pub struct InMemorySink {
    captured: Mutex<Vec<AuctionEventRow>>,
}

impl InMemorySink {
    /// Return a clone of all captured rows in emission order.
    #[must_use]
    pub fn rows(&self) -> Vec<AuctionEventRow> {
        self.captured
            .lock()
            .expect("should lock captured rows")
            .clone()
    }
}

impl AuctionEventSink for InMemorySink {
    fn emit(&self, rows: &[AuctionEventRow]) {
        self.captured
            .lock()
            .expect("should lock captured rows")
            .extend_from_slice(rows);
    }
}
```

Update `mod.rs`: add `pub mod sink;` after `pub mod types;`, and extend re-exports:

```rust
pub mod sink;
pub mod types;

pub use sink::{AuctionEventSink, InMemorySink, NoopSink};
pub use types::{
    to_ndjson, AuctionEventRow, AuctionObservationContext, AuctionSource, EventKind,
    ProviderCallOutcome, ProviderCallStatus, ProviderRole, TerminalOutcome, TerminalStatus,
};
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p trusted-server-core telemetry::sink`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-core/src/auction/telemetry/
git commit -m "Add auction event sink trait and test sinks"
```

---

### Task 5: Builder for summary and provider-call rows

**Files:**

- Create: `crates/trusted-server-core/src/auction/telemetry/builder.rs`
- Modify: `crates/trusted-server-core/src/auction/telemetry/mod.rs` (declare `builder`, re-export `build_auction_events`)
- Test: inline `#[cfg(test)]` in `builder.rs`

**Interfaces:**

- Consumes: `AuctionObservationContext`, `TerminalOutcome`, `ProviderCallOutcome`, `AuctionEventRow`, `EventKind` (Tasks 1-3).
- Produces: `pub fn build_auction_events(ctx: &AuctionObservationContext, outcome: &TerminalOutcome, provider_calls: &[ProviderCallOutcome], result: Option<&OrchestrationResult>) -> Vec<AuctionEventRow>`. This task implements the `result == None` behavior (summary + provider-call rows only); Task 6 adds bid rows when `result` is `Some`.

- [ ] **Step 1: Write the failing test**

Create `crates/trusted-server-core/src/auction/telemetry/builder.rs` with the test module first:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::auction::telemetry::types::{
        AuctionObservationContext, AuctionSource, EventKind, ProviderCallOutcome,
        ProviderCallStatus, ProviderRole, TerminalOutcome, TerminalStatus,
    };

    fn ctx(source: AuctionSource) -> AuctionObservationContext {
        AuctionObservationContext {
            auction_id: uuid::Uuid::nil(),
            source,
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
    fn abandoned_auction_emits_summary_plus_provider_calls_no_bids() {
        let outcome = TerminalOutcome {
            status: TerminalStatus::Abandoned,
            reason: Some("origin_unrewritable".to_string()),
            slot_count: Some(2),
            total_time_ms: Some(120),
            winning_bid_count: Some(0),
        };
        let calls = vec![
            ProviderCallOutcome {
                provider: "prebid".to_string(),
                role: ProviderRole::Bidder,
                status: ProviderCallStatus::Abandoned,
                response_time_ms: None,
                bid_count: None,
            },
            ProviderCallOutcome {
                provider: "aps".to_string(),
                role: ProviderRole::Bidder,
                status: ProviderCallStatus::Abandoned,
                response_time_ms: None,
                bid_count: None,
            },
        ];

        let rows = build_auction_events(&ctx(AuctionSource::InitialNavigation), &outcome, &calls, None);

        let summaries: Vec<_> = rows.iter().filter(|r| r.event_kind == EventKind::Summary).collect();
        assert_eq!(summaries.len(), 1, "should emit exactly one summary row");
        assert_eq!(
            summaries[0].terminal_status,
            Some(TerminalStatus::Abandoned),
            "should record the terminal status on the summary"
        );
        assert_eq!(
            rows.iter().filter(|r| r.event_kind == EventKind::ProviderCall).count(),
            2,
            "should emit one provider-call row per outcome"
        );
        assert_eq!(
            rows.iter().filter(|r| r.event_kind == EventKind::Bid).count(),
            0,
            "should emit no bid rows when there is no result"
        );
    }

    #[test]
    fn skipped_auction_emits_only_a_summary() {
        let outcome = TerminalOutcome {
            status: TerminalStatus::Skipped,
            reason: Some("consent".to_string()),
            slot_count: Some(3),
            total_time_ms: None,
            winning_bid_count: Some(0),
        };
        let rows = build_auction_events(&ctx(AuctionSource::AuctionApi), &outcome, &[], None);
        assert_eq!(rows.len(), 1, "should emit only the summary row");
        assert_eq!(rows[0].event_kind, EventKind::Summary, "should be a summary");
        assert_eq!(rows[0].terminal_reason.as_deref(), Some("consent"), "should carry the reason");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p trusted-server-core telemetry::builder`
Expected: FAIL to compile (`builder` module not declared; `build_auction_events` not found).

- [ ] **Step 3: Write minimal implementation**

Prepend to `builder.rs` (above the test module):

```rust
//! Pure builder that turns an auction observation into telemetry rows.

use crate::auction::orchestrator::OrchestrationResult;
use crate::auction::telemetry::types::{
    AuctionEventRow, AuctionObservationContext, EventKind, ProviderCallOutcome, TerminalOutcome,
};

/// Build all telemetry rows for one auction observation.
///
/// Always emits exactly one summary row, one provider-call row per entry in
/// `provider_calls`, and (when `result` is `Some`) one bid row per returned bid
/// plus one row for any winning slot not matched to a returned bid.
#[must_use]
pub fn build_auction_events(
    ctx: &AuctionObservationContext,
    outcome: &TerminalOutcome,
    provider_calls: &[ProviderCallOutcome],
    result: Option<&OrchestrationResult>,
) -> Vec<AuctionEventRow> {
    let mut rows = Vec::new();
    rows.push(summary_row(ctx, outcome));
    for call in provider_calls {
        rows.push(provider_call_row(ctx, call));
    }
    if let Some(result) = result {
        rows.extend(build_bid_rows(ctx, result));
    }
    rows
}

/// Build the single summary row.
fn summary_row(ctx: &AuctionObservationContext, outcome: &TerminalOutcome) -> AuctionEventRow {
    let mut row = AuctionEventRow::base(ctx, EventKind::Summary);
    row.terminal_status = Some(outcome.status);
    row.terminal_reason = outcome.reason.clone();
    row.slot_count = outcome.slot_count;
    row.total_time_ms = outcome.total_time_ms;
    row.winning_bid_count = outcome.winning_bid_count;
    row
}

/// Build one provider-call row.
fn provider_call_row(
    ctx: &AuctionObservationContext,
    call: &ProviderCallOutcome,
) -> AuctionEventRow {
    let mut row = AuctionEventRow::base(ctx, EventKind::ProviderCall);
    row.provider = Some(call.provider.clone());
    row.provider_role = Some(call.role);
    row.status = Some(call.status);
    row.provider_response_time_ms = call.response_time_ms;
    row.provider_bid_count = call.bid_count;
    row
}

/// Build bid rows from a completed orchestration result. Implemented in Task 6.
fn build_bid_rows(
    _ctx: &AuctionObservationContext,
    _result: &OrchestrationResult,
) -> Vec<AuctionEventRow> {
    Vec::new()
}
```

Update `mod.rs`: add `pub mod builder;` before `pub mod sink;`, and add the re-export:

```rust
pub mod builder;
pub mod sink;
pub mod types;

pub use builder::build_auction_events;
pub use sink::{AuctionEventSink, InMemorySink, NoopSink};
pub use types::{
    to_ndjson, AuctionEventRow, AuctionObservationContext, AuctionSource, EventKind,
    ProviderCallOutcome, ProviderCallStatus, ProviderRole, TerminalOutcome, TerminalStatus,
};
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p trusted-server-core telemetry::builder`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-core/src/auction/telemetry/
git commit -m "Add telemetry builder for summary and provider-call rows"
```

---

### Task 6: Bid rows with win matching and mediator dedup

**Files:**

- Modify: `crates/trusted-server-core/src/auction/telemetry/builder.rs` (replace the `build_bid_rows` stub, add helpers)
- Test: inline `#[cfg(test)]` in `builder.rs`

**Interfaces:**

- Consumes: `OrchestrationResult` (`provider_responses: Vec<AuctionResponse>`, `mediator_response: Option<AuctionResponse>`, `winning_bids: HashMap<String, Bid>`), `Bid`, `AuctionResponse` from `crate::auction::types`.
- Produces: a real `build_bid_rows` so that `build_auction_events(.., Some(result))` emits bid rows.

Matching rules (from the spec):

- One bid row per returned bid across `provider_responses`. Mediator bids are not re-emitted when matchable to an original provider bid.
- A bid is the winner for its slot when it matches `winning_bids[slot_id]` on `(slot_id, bidder, ad_id)`, falling back to `(slot_id, bidder)` when `ad_id` is absent. At most one winning row per slot (first match claims it).
- A matched winning row whose own `price` is `None` takes the winner's decoded `price`.
- A winning slot not matched to any returned bid emits one mediator-derived canonical winner row.

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `builder.rs`:

```rust
use crate::auction::types::{AuctionResponse, Bid, BidStatus};
use std::collections::HashMap;

fn bid(slot: &str, bidder: &str, price: Option<f64>, ad_id: Option<&str>) -> Bid {
    Bid {
        slot_id: slot.to_string(),
        price,
        currency: "USD".to_string(),
        creative: None,
        adomain: Some(vec!["advertiser.example".to_string()]),
        bidder: bidder.to_string(),
        width: 300,
        height: 250,
        nurl: None,
        burl: None,
        ad_id: ad_id.map(str::to_string),
        cache_id: None,
        cache_host: None,
        cache_path: None,
        metadata: HashMap::new(),
    }
}

fn response(provider: &str, bids: Vec<Bid>, status: BidStatus) -> AuctionResponse {
    AuctionResponse {
        provider: provider.to_string(),
        bids,
        status,
        response_time_ms: 42,
        metadata: HashMap::new(),
    }
}

fn completed_outcome() -> TerminalOutcome {
    TerminalOutcome {
        status: TerminalStatus::Completed,
        reason: None,
        slot_count: Some(1),
        total_time_ms: Some(50),
        winning_bid_count: Some(1),
    }
}

#[test]
fn emits_one_bid_row_per_returned_bid_with_single_winner() {
    let winner = bid("slot-1", "kargo", Some(2.5), Some("creative-1"));
    let mut winning_bids = HashMap::new();
    winning_bids.insert("slot-1".to_string(), winner.clone());
    let result = OrchestrationResult {
        provider_responses: vec![response(
            "prebid",
            vec![
                bid("slot-1", "kargo", Some(2.5), Some("creative-1")),
                bid("slot-1", "ix", Some(1.0), Some("creative-2")),
            ],
            BidStatus::Success,
        )],
        mediator_response: None,
        winning_bids,
        total_time_ms: 50,
        metadata: HashMap::new(),
    };

    let rows = build_auction_events(&ctx(AuctionSource::AuctionApi), &completed_outcome(), &[], Some(&result));
    let bid_rows: Vec<_> = rows.iter().filter(|r| r.event_kind == EventKind::Bid).collect();

    assert_eq!(bid_rows.len(), 2, "should emit one row per returned bid");
    assert_eq!(
        bid_rows.iter().filter(|r| r.is_win == Some(1)).count(),
        1,
        "should mark exactly one winning row per slot"
    );
    let winning = bid_rows.iter().find(|r| r.is_win == Some(1)).expect("should have a winner");
    assert_eq!(winning.seat.as_deref(), Some("kargo"), "should win for the matched seat");
}

#[test]
fn fills_decoded_price_on_null_priced_winner() {
    let winner = bid("slot-1", "aps", Some(3.1), Some("amzn-1"));
    let mut winning_bids = HashMap::new();
    winning_bids.insert("slot-1".to_string(), winner);
    let result = OrchestrationResult {
        // The original APS bid has no decoded price.
        provider_responses: vec![response(
            "aps",
            vec![bid("slot-1", "aps", None, Some("amzn-1"))],
            BidStatus::Success,
        )],
        mediator_response: Some(response("mediator", vec![], BidStatus::Success)),
        winning_bids,
        total_time_ms: 60,
        metadata: HashMap::new(),
    };

    let rows = build_auction_events(&ctx(AuctionSource::AuctionApi), &completed_outcome(), &[], Some(&result));
    let winning = rows
        .iter()
        .find(|r| r.event_kind == EventKind::Bid && r.is_win == Some(1))
        .expect("should have a winning bid row");
    assert_eq!(winning.price_cpm, Some(3.1), "should fill decoded winner price on a null-priced bid");
}

#[test]
fn unmatched_winner_emits_one_mediator_row() {
    let winner = bid("slot-9", "exclusive-seat", Some(5.0), Some("only-here"));
    let mut winning_bids = HashMap::new();
    winning_bids.insert("slot-9".to_string(), winner);
    let result = OrchestrationResult {
        // No provider response contains the winning bid.
        provider_responses: vec![response("prebid", vec![], BidStatus::NoBid)],
        mediator_response: Some(response("mediator", vec![], BidStatus::Success)),
        winning_bids,
        total_time_ms: 70,
        metadata: HashMap::new(),
    };

    let rows = build_auction_events(&ctx(AuctionSource::AuctionApi), &completed_outcome(), &[], Some(&result));
    let bid_rows: Vec<_> = rows.iter().filter(|r| r.event_kind == EventKind::Bid).collect();
    assert_eq!(bid_rows.len(), 1, "should synthesize one row for the unmatched winner");
    assert_eq!(bid_rows[0].is_win, Some(1), "should mark the synthesized row as the win");
    assert_eq!(bid_rows[0].provider.as_deref(), Some("mediator"), "should attribute it to the mediator");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p trusted-server-core telemetry::builder`
Expected: FAIL — the three new tests fail (`build_bid_rows` returns empty).

- [ ] **Step 3: Write minimal implementation**

In `builder.rs`, extend the imports at the top to bring in the bid types:

```rust
use crate::auction::types::Bid;
```

Replace the `build_bid_rows` stub with the real implementation plus helpers:

```rust
/// Build bid rows from a completed orchestration result.
fn build_bid_rows(
    ctx: &AuctionObservationContext,
    result: &OrchestrationResult,
) -> Vec<AuctionEventRow> {
    let mut rows = Vec::new();
    // Slots whose winning row has already been emitted, so each slot has at
    // most one `is_win = 1` row.
    let mut claimed_slots: Vec<String> = Vec::new();

    for response in &result.provider_responses {
        for bid in &response.bids {
            let winner = result.winning_bids.get(&bid.slot_id);
            let is_win = match winner {
                Some(winner) => {
                    matches_winner(bid, winner) && !claimed_slots.contains(&bid.slot_id)
                }
                None => false,
            };
            let price_override = if is_win && bid.price.is_none() {
                winner.and_then(|winner| winner.price)
            } else {
                None
            };
            if is_win {
                claimed_slots.push(bid.slot_id.clone());
            }
            rows.push(bid_row(ctx, &response.provider, bid, is_win, price_override));
        }
    }

    // Any winning slot not matched to a returned bid gets one canonical
    // mediator-derived winner row.
    let mediator_provider = result
        .mediator_response
        .as_ref()
        .map(|response| response.provider.clone())
        .unwrap_or_else(|| "mediator".to_string());
    for (slot_id, winner) in &result.winning_bids {
        if !claimed_slots.contains(slot_id) {
            rows.push(bid_row(ctx, &mediator_provider, winner, true, winner.price));
        }
    }

    rows
}

/// Whether a returned bid is the winner for its slot.
fn matches_winner(candidate: &Bid, winner: &Bid) -> bool {
    if candidate.slot_id != winner.slot_id || candidate.bidder != winner.bidder {
        return false;
    }
    match (&candidate.ad_id, &winner.ad_id) {
        (Some(left), Some(right)) => left == right,
        // Fall back to (slot, seat) identity when ad ids are absent.
        _ => true,
    }
}

/// Build one bid row. `price_override` carries a mediator-decoded price for a
/// winning bid whose own price is null.
fn bid_row(
    ctx: &AuctionObservationContext,
    provider: &str,
    bid: &Bid,
    is_win: bool,
    price_override: Option<f64>,
) -> AuctionEventRow {
    let mut row = AuctionEventRow::base(ctx, EventKind::Bid);
    row.provider = Some(provider.to_string());
    row.slot_id = Some(bid.slot_id.clone());
    row.slot_w = Some(clamp_dimension(bid.width));
    row.slot_h = Some(clamp_dimension(bid.height));
    row.seat = Some(bid.bidder.clone());
    row.price_cpm = price_override.or(bid.price);
    row.currency = Some(bid.currency.clone());
    row.is_win = Some(u8::from(is_win));
    row.ad_domain = bid.adomain.as_ref().and_then(|domains| domains.first().cloned());
    row.ad_id = bid.ad_id.clone();
    row
}

/// Clamp a `u32` creative dimension into the `u16` schema column without
/// panicking. Real creative sizes are always well within `u16`.
fn clamp_dimension(value: u32) -> u16 {
    value.min(u32::from(u16::MAX)) as u16
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p trusted-server-core telemetry::builder`
Expected: PASS (5 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-core/src/auction/telemetry/builder.rs
git commit -m "Build bid rows with win matching and mediator dedup"
```

---

### Task 7: End-to-end builder test over a mixed result

**Files:**

- Modify: `crates/trusted-server-core/src/auction/telemetry/builder.rs` (test only)
- Test: inline `#[cfg(test)]` in `builder.rs`

**Interfaces:**

- Consumes: everything from Tasks 1-6. No production code changes; this task locks the combined behavior with one realistic case and guards against regressions.

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `builder.rs`:

```rust
#[test]
fn completed_result_with_mixed_providers_produces_expected_grains() {
    // Arrange: one successful provider with two bids (one wins), one no-bid
    // provider, and an explicit provider-call list mirroring those outcomes.
    let winner = bid("slot-1", "kargo", Some(4.0), Some("c-1"));
    let mut winning_bids = HashMap::new();
    winning_bids.insert("slot-1".to_string(), winner);
    let result = OrchestrationResult {
        provider_responses: vec![
            response(
                "prebid",
                vec![
                    bid("slot-1", "kargo", Some(4.0), Some("c-1")),
                    bid("slot-1", "ix", Some(2.0), Some("c-2")),
                ],
                BidStatus::Success,
            ),
            response("aps", vec![], BidStatus::NoBid),
        ],
        mediator_response: None,
        winning_bids,
        total_time_ms: 88,
        metadata: HashMap::new(),
    };
    let calls = vec![
        ProviderCallOutcome {
            provider: "prebid".to_string(),
            role: ProviderRole::Bidder,
            status: ProviderCallStatus::Success,
            response_time_ms: Some(42),
            bid_count: Some(2),
        },
        ProviderCallOutcome {
            provider: "aps".to_string(),
            role: ProviderRole::Bidder,
            status: ProviderCallStatus::NoBid,
            response_time_ms: Some(40),
            bid_count: Some(0),
        },
    ];

    // Act
    let rows = build_auction_events(&ctx(AuctionSource::SpaNavigation), &completed_outcome(), &calls, Some(&result));

    // Assert: exactly one summary, two provider-call rows, two bid rows, and no
    // invented seats on the no-bid provider.
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
    let bid_rows: Vec<_> = rows.iter().filter(|r| r.event_kind == EventKind::Bid).collect();
    assert_eq!(bid_rows.len(), 2, "should emit a bid row only for returned bids");
    assert!(
        bid_rows.iter().all(|r| r.seat.is_some()),
        "should never emit a bid row without a seat"
    );
    assert_eq!(
        bid_rows.iter().filter(|r| r.is_win == Some(1)).count(),
        1,
        "should mark exactly one winning bid"
    );
}
```

- [ ] **Step 2: Run test to verify it fails or passes**

Run: `cargo test -p trusted-server-core telemetry::builder`
Expected: PASS immediately (behavior already implemented in Tasks 5-6). If it fails, fix the implementation, not the test.

- [ ] **Step 3: Run the full module suite**

Run: `cargo test -p trusted-server-core telemetry`
Expected: PASS (all telemetry tests across types/sink/builder).

- [ ] **Step 4: Run format and clippy gates**

Run: `cargo fmt --all -- --check`
Expected: no diff.

Run: `cargo clippy -p trusted-server-core --all-targets --all-features -- -D warnings`
Expected: no warnings.

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-core/src/auction/telemetry/builder.rs
git commit -m "Add end-to-end telemetry builder test over a mixed result"
```

---

## Self-Review

**Spec coverage (this plan's slice):**

- Three row grains (summary / provider_call / bid) with one summary per auction: Tasks 3, 5, 6, 7.
- Bid rows only for returned bids; no invented seats on no-bid/error: Tasks 5, 6, 7.
- Win matching on `(slot_id, bidder, ad_id)` with `(slot_id, bidder)` fallback, one win per slot, decoded-price fill, mediator dedup, unmatched-winner synthesis: Task 6.
- Stable NDJSON shape with nulls, no `event_ts` in core: Task 3.
- Sink abstraction in core with test implementations: Task 4.
- Schema column set and wire strings: Tasks 1, 3.

**Deferred to later plans (not gaps in this one):**

- Constructing `AuctionObservationContext` from `EcContext`/geo/`DeviceSignals`, telemetry-UUID independence from `AuctionRequest.id`, and page-path normalization: Plan 2/3 wiring (those inputs are not available to this pure layer).
- Mapping `BidStatus`/dispatch outcomes to `ProviderCallStatus` and populating `provider_calls`: Plan 2/3.
- `media_type` population from request slots: later wiring plan.
- Fastly sink implementation, `event_ts` stamping, access logs: Plan 4.

**Placeholder scan:** No `TBD`/`TODO`/"handle edge cases"; every code step shows complete code.

**Type consistency:** `build_auction_events`, `AuctionEventRow::base`, `to_ndjson`, `AuctionEventSink::emit`, and all field names are used identically across tasks. Field names match the verified `Bid`/`AuctionResponse`/`OrchestrationResult` definitions.
