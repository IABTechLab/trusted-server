# Auction Telemetry Wiring (POST /auction) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make a completed `POST /auction` emit telemetry rows end-to-end: build the observation context at the call site, run it through the Plan 1/2 mapping/builder, and write NDJSON to the Fastly `ts_auction_events` real-time log endpoint.

**Architecture:** A new `auction::telemetry::context` builder turns the request/geo/consent into an `AuctionObservationContext`. `RuntimeServices` gains an `AuctionEventSink` (default no-op, so all existing call sites keep working) that `handle_auction` calls after `run_auction`. The Fastly adapter supplies a real sink that serializes each row with an injected `event_ts` and writes it to the named log endpoint. Emission is buffered/non-blocking and never on the response path.

**Tech Stack:** Rust 2024, `serde_json`, `uuid`, `chrono` (adapter), `fastly` 0.11 log endpoint.

## Global Constraints

Copied from the project conventions and the prior telemetry plans; every task implicitly includes these:

- Rust **2024 edition**. No `unwrap()` in non-test code (use `expect("should ...")`; `unwrap_or`/`unwrap_or_else`/`unwrap_or_default` are allowed). No `println!`/`eprintln!`; use `log` macros.
- Functions take at most 7 arguments. Comments on their own line above the code. No imports inside functions; no wildcard imports outside `#[cfg(test)]` (`use super::*;` allowed there).
- Tests: Arrange-Act-Assert, `expect()`/`expect_err()` with `"should ..."` messages, descriptive assertion messages, `serde_json::json!` over raw JSON strings. Only example/fictional domains (`example.com`, `test-publisher.com`).
- Each public item has a doc comment.
- Git commit messages: sentence case, imperative, no semantic prefixes (`feat:`/`fix:`), no bracketed tags, no `Co-Authored-By` trailer. Use the exact message in each task's commit step.
- The adapter crate targets `wasm32-wasip1`; verify adapter changes with `cargo check --package trusted-server-adapter-fastly --target wasm32-wasip1`.

**Scope boundary (deliberately NOT in this plan):** `handle_page_bids` wiring, the SSAT `dispatch_auction`/`collect_dispatched_auction` path and its abandoned/skipped/dispatch-failed/execution-failed outcomes, real device-signal population (`is_mobile`/`is_known_browser` are passed as `2` = unknown here), access logs, and the Tinybird/relay/Grafana side. Those are later plans. This plan covers only the completed `POST /auction` path.

**Verified facts this plan relies on (current code):**
- `handle_auction(settings, orchestrator, kv, registry, ec_context, services, req)` lives at `crates/trusted-server-core/src/auction/endpoints.rs`; after `run_auction` the `result: OrchestrationResult`, `auction_request: AuctionRequest`, and `services: &RuntimeServices` are all in scope (endpoints.rs:259-274). `geo` and `consent_context` locals are moved into `convert_tsjs_to_auction_request` earlier, so telemetry reads geo/consent back off `auction_request.device`/`auction_request.user.consent`.
- `AuctionRequest`: `publisher: PublisherInfo { domain: String, page_url: Option<String> }`, `slots: Vec<AdSlot>`, `device: Option<DeviceInfo { geo: Option<GeoInfo>, .. }>`, `user: UserInfo { consent: Option<crate::consent::ConsentContext>, .. }` (auction/types.rs).
- `GeoInfo { country: String, region: Option<String>, .. }` (`crate::platform::GeoInfo`). `ConsentContext { gdpr_applies: bool, .. }` with `fn is_empty(&self) -> bool` and `Default` (`crate::consent::ConsentContext`).
- `RuntimeServices` (crates/trusted-server-core/src/platform/types.rs) uses an `Option`-field builder that `expect()`s on `build()`, plus `with_kv_store(self, ..) -> Self`. Test factory `noop_services()` (platform::test_support).
- `fastly::log::Endpoint::from_name(name: &str) -> Self` implements `std::io::Write` (fastly 0.11.13). `chrono::Utc::now()` is already used in the adapter.
- Plan 1/2 already provide, under `crate::auction::telemetry`: `AuctionObservationContext`, `AuctionSource`, `EventKind`, `AuctionEventRow` (+ `AuctionEventRow::base`), `AuctionEventSink`, `NoopSink`, `InMemorySink`, and `build_completed_auction_events(ctx, slot_count, result)`.

---

### Task 1: Observation-context builder

**Files:**
- Create: `crates/trusted-server-core/src/auction/telemetry/context.rs`
- Modify: `crates/trusted-server-core/src/auction/telemetry/mod.rs` (declare `context`, re-export `build_observation_context`)
- Test: inline `#[cfg(test)]` in `context.rs`

**Interfaces:**
- Consumes: `AuctionObservationContext`, `AuctionSource` (telemetry::types); `crate::platform::GeoInfo`; `crate::consent::ConsentContext`.
- Produces:
  - `pub fn build_observation_context(source: AuctionSource, publisher_domain: &str, page_url: Option<&str>, geo: Option<&GeoInfo>, consent: Option<&ConsentContext>, is_mobile: u8, is_known_browser: u8) -> AuctionObservationContext` — mints a fresh `Uuid::new_v4()`, normalizes `page_url` to a path, derives country/region from geo, and `gdpr_applies`/`consent_present` from consent (both `false` when `consent` is `None`).

- [ ] **Step 1: Write the failing test**

Create `crates/trusted-server-core/src/auction/telemetry/context.rs` with the test module first:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::auction::telemetry::types::AuctionSource;
    use crate::consent::ConsentContext;
    use crate::platform::GeoInfo;

    fn geo() -> GeoInfo {
        GeoInfo {
            city: "Springfield".to_string(),
            country: "US".to_string(),
            continent: "NA".to_string(),
            latitude: 0.0,
            longitude: 0.0,
            metro_code: 0,
            region: Some("CA".to_string()),
            asn: None,
        }
    }

    #[test]
    fn normalizes_full_url_to_path_without_query_or_fragment() {
        assert_eq!(
            normalize_page_path("https://www.example.com/news/article?utm=x#top"),
            "/news/article",
            "should keep only the path"
        );
        assert_eq!(
            normalize_page_path("/already/a/path?q=1"),
            "/already/a/path",
            "should strip the query from a bare path"
        );
        assert_eq!(
            normalize_page_path("https://example.com"),
            "/",
            "a URL with no path normalizes to /"
        );
        assert_eq!(normalize_page_path(""), "/", "empty input normalizes to /");
    }

    #[test]
    fn builds_context_from_geo_and_consent() {
        let consent = ConsentContext {
            gdpr_applies: true,
            ..ConsentContext::default()
        };
        let ctx = build_observation_context(
            AuctionSource::AuctionApi,
            "example.com",
            Some("https://example.com/p?x=1"),
            Some(&geo()),
            Some(&consent),
            1,
            1,
        );
        assert_eq!(ctx.source, AuctionSource::AuctionApi, "should carry the source");
        assert_eq!(ctx.publisher_domain, "example.com", "should carry the domain");
        assert_eq!(ctx.page_path, "/p", "should carry the normalized path");
        assert_eq!(ctx.country, "US", "should carry country from geo");
        assert_eq!(ctx.region.as_deref(), Some("CA"), "should carry region from geo");
        assert!(ctx.gdpr_applies, "should carry gdpr_applies from consent");
        assert!(!ctx.consent_present, "a default consent is empty so consent_present is false");
        assert!(!ctx.auction_id.is_nil(), "should mint a fresh telemetry id");
    }

    #[test]
    fn defaults_country_and_consent_when_absent() {
        let ctx = build_observation_context(
            AuctionSource::AuctionApi,
            "example.com",
            None,
            None,
            None,
            2,
            2,
        );
        assert_eq!(ctx.country, "", "no geo means empty country");
        assert!(ctx.region.is_none(), "no geo means no region");
        assert_eq!(ctx.page_path, "/", "no page url normalizes to /");
        assert!(!ctx.gdpr_applies, "no consent means gdpr_applies false");
        assert!(!ctx.consent_present, "no consent means consent_present false");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p trusted-server-core telemetry::context`
Expected: FAIL to compile (`context` module not declared; `build_observation_context`/`normalize_page_path` not found).

- [ ] **Step 3: Write minimal implementation**

Prepend to `context.rs` (above the test module):

```rust
//! Builds an `AuctionObservationContext` from request, geo, and consent inputs.
//!
//! This is the only telemetry code that mints the telemetry id and normalizes
//! the page path. It performs no I/O.

use uuid::Uuid;

use crate::auction::telemetry::types::{AuctionObservationContext, AuctionSource};
use crate::consent::ConsentContext;
use crate::platform::GeoInfo;

/// Build a PII-free observation context for one auction.
///
/// `is_mobile` and `is_known_browser` use `0`/`1`/`2` (`2` = unknown); a later
/// plan threads real device signals. `consent` is optional because a
/// non-regulated auction may carry no consent context.
#[must_use]
pub fn build_observation_context(
    source: AuctionSource,
    publisher_domain: &str,
    page_url: Option<&str>,
    geo: Option<&GeoInfo>,
    consent: Option<&ConsentContext>,
    is_mobile: u8,
    is_known_browser: u8,
) -> AuctionObservationContext {
    AuctionObservationContext {
        auction_id: Uuid::new_v4(),
        source,
        publisher_domain: publisher_domain.to_string(),
        page_path: page_url
            .map(normalize_page_path)
            .unwrap_or_else(|| "/".to_string()),
        country: geo.map(|info| info.country.clone()).unwrap_or_default(),
        region: geo.and_then(|info| info.region.clone()),
        is_mobile,
        is_known_browser,
        gdpr_applies: consent.is_some_and(|context| context.gdpr_applies),
        consent_present: consent.is_some_and(|context| !context.is_empty()),
    }
}

/// Reduce a page URL or path to a bounded path with no scheme, host, query, or
/// fragment. Empty or path-less inputs normalize to `/`.
fn normalize_page_path(page_url: &str) -> String {
    let without_fragment = page_url.split('#').next().unwrap_or("");
    let without_query = without_fragment.split('?').next().unwrap_or("");
    let path = match without_query.find("://") {
        Some(scheme_end) => {
            let after_scheme = &without_query[scheme_end + 3..];
            match after_scheme.find('/') {
                Some(slash) => &after_scheme[slash..],
                None => "/",
            }
        }
        None => without_query,
    };
    let path = if path.is_empty() { "/" } else { path };
    path.chars().take(512).collect()
}
```

In `mod.rs`, add `pub mod context;` (alphabetically, before `pub mod mapping;`) and add `pub use context::build_observation_context;` to the re-export block.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p trusted-server-core telemetry::context`
Expected: PASS (3 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-core/src/auction/telemetry/context.rs crates/trusted-server-core/src/auction/telemetry/mod.rs
git commit -m "Add auction observation context builder"
```

---

### Task 2: Auction event sink on RuntimeServices

**Files:**
- Modify: `crates/trusted-server-core/src/platform/types.rs` (struct field, builder, accessor, `with_` method, imports)
- Test: inline `#[cfg(test)]` in `platform/types.rs`

**Interfaces:**
- Consumes: `AuctionEventSink`, `NoopSink` (telemetry).
- Produces, on `RuntimeServices`:
  - `pub fn auction_event_sink(&self) -> &dyn AuctionEventSink`
  - `pub fn with_auction_event_sink(self, sink: Arc<dyn AuctionEventSink>) -> Self`
  - builder method `pub fn auction_event_sink(self, sink: Arc<dyn AuctionEventSink>) -> Self`
  - `build()` defaults the sink to `Arc::new(NoopSink)` when unset, so every existing `RuntimeServices` construction keeps compiling.

- [ ] **Step 1: Write the failing test**

Add a test module at the bottom of `platform/types.rs` (if the file already has a `#[cfg(test)] mod tests`, add these into it instead):

```rust
#[cfg(test)]
mod auction_sink_tests {
    use super::*;
    use crate::auction::telemetry::types::{AuctionObservationContext, AuctionSource, EventKind};
    use crate::auction::telemetry::{AuctionEventRow, InMemorySink};
    use crate::platform::test_support::noop_services;

    fn row() -> AuctionEventRow {
        let ctx = AuctionObservationContext {
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
        };
        AuctionEventRow::base(&ctx, EventKind::Summary)
    }

    #[test]
    fn default_sink_is_noop_and_does_not_panic() {
        let services = noop_services();
        services.auction_event_sink().emit(&[row()]);
    }

    #[test]
    fn injected_sink_captures_emitted_rows() {
        let sink = std::sync::Arc::new(InMemorySink::default());
        let services = noop_services().with_auction_event_sink(sink.clone());
        services.auction_event_sink().emit(&[row()]);
        assert_eq!(sink.rows().len(), 1, "should route emitted rows to the injected sink");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p trusted-server-core platform::types::auction_sink_tests`
Expected: FAIL to compile (`auction_event_sink`/`with_auction_event_sink` not found).

- [ ] **Step 3: Write minimal implementation**

In `platform/types.rs`:

Add the import near the top (with the other `use crate::...` lines):

```rust
use crate::auction::telemetry::{AuctionEventSink, NoopSink};
```

Add the field to the `RuntimeServices` struct (after `client_info`):

```rust
    /// Sink for auction telemetry rows. Defaults to a no-op; the Fastly adapter
    /// installs a real implementation.
    pub(crate) auction_event_sink: Arc<dyn AuctionEventSink>,
```

Add the accessor inside `impl RuntimeServices` (next to `client_info()`):

```rust
    /// Returns the auction telemetry sink.
    #[must_use]
    pub fn auction_event_sink(&self) -> &dyn AuctionEventSink {
        &*self.auction_event_sink
    }
```

Add the `with_` method inside `impl RuntimeServices` (next to `with_kv_store`):

```rust
    /// Return a clone of these services with a different auction event sink.
    #[must_use]
    pub fn with_auction_event_sink(self, sink: Arc<dyn AuctionEventSink>) -> Self {
        Self {
            auction_event_sink: sink,
            ..self
        }
    }
```

Add the builder field to `RuntimeServicesBuilder` (after `client_info: Option<ClientInfo>,`):

```rust
    auction_event_sink: Option<Arc<dyn AuctionEventSink>>,
```

Set it to `None` in `RuntimeServicesBuilder::new()` (after `client_info: None,`):

```rust
            auction_event_sink: None,
```

Add the builder method inside `impl RuntimeServicesBuilder` (next to `client_info`):

```rust
    /// Set the auction telemetry sink. Defaults to a no-op when unset.
    #[must_use]
    pub fn auction_event_sink(mut self, sink: Arc<dyn AuctionEventSink>) -> Self {
        self.auction_event_sink = Some(sink);
        self
    }
```

In `build()`, add the field to the returned `RuntimeServices` (after `client_info: ...`). Unlike the other fields, this one defaults instead of panicking:

```rust
            auction_event_sink: self
                .auction_event_sink
                .unwrap_or_else(|| Arc::new(NoopSink)),
```

- [ ] **Step 4: Run test + confirm existing constructions still compile**

Run: `cargo test -p trusted-server-core platform::types::auction_sink_tests`
Expected: PASS (2 tests).

Run: `cargo test -p trusted-server-core`
Expected: PASS (the whole core suite; this proves no existing `RuntimeServices::builder()` call broke, since the sink defaults).

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-core/src/platform/types.rs
git commit -m "Add auction event sink to runtime services with no-op default"
```

---

### Task 3: Emit telemetry from handle_auction

**Files:**
- Modify: `crates/trusted-server-core/src/auction/endpoints.rs` (add `use`, insert emission after `run_auction`, add test)
- Test: inline `#[cfg(test)]` in `endpoints.rs`

**Interfaces:**
- Consumes: `build_observation_context`, `build_completed_auction_events`, `AuctionSource` (telemetry); `RuntimeServices::auction_event_sink` (Task 2).
- Produces: no new public API; `POST /auction` now emits to `services.auction_event_sink()`.

- [ ] **Step 1: Write the failing test**

The orchestrator errors on both an empty provider list ("No providers configured") and an all-failed-to-launch auction ("All N configured provider(s) ... failed to launch"). To exercise the **completed** path the test registers a provider that launches successfully through a stubbed HTTP client and parses a no-bid success — the exact harness the orchestrator's own tests use.

This test needs imports the `tests` module does not already have. Add these to the `use` lines at the top of the `#[cfg(test)] mod tests` block:

```rust
    use crate::platform::test_support::{build_services_with_http_client, StubHttpClient};
    use crate::platform::PlatformHttpRequest;
    use error_stack::ResultExt as _;
```

(The module already imports `create_test_settings`, `make_ec_context`, `AuctionConfig`, `AuctionProvider`, `Jurisdiction`, `json`, `Arc`, `StatusCode`, `EdgeBody`, `Request`, `handle_auction`, `AuctionRequest`, `AuctionResponse`, `PlatformPendingRequest`, `PlatformResponse`, and `error_stack::Report` via the existing test setup.)

First add this provider struct inside the `tests` module (next to the existing `PanicOnBidProvider`). It mirrors `StubAuctionProvider` from the orchestrator tests:

```rust
    /// Provider that launches through the stub HTTP client and parses a no-bid
    /// success, so `run_auction` returns a completed `OrchestrationResult`. This
    /// is the path that must emit telemetry.
    struct StubLaunchProvider;

    #[async_trait::async_trait(?Send)]
    impl AuctionProvider for StubLaunchProvider {
        fn provider_name(&self) -> &'static str {
            "stub_provider"
        }

        async fn request_bids(
            &self,
            _request: &AuctionRequest,
            context: &AuctionContext<'_>,
        ) -> Result<PlatformPendingRequest, Report<TrustedServerError>> {
            let req = PlatformHttpRequest::new(
                Request::builder()
                    .method("POST")
                    .uri("https://example.com/bid")
                    .body(EdgeBody::empty())
                    .expect("should build stub bid request"),
                "stub-backend",
            );
            context
                .services
                .http_client()
                .send_async(req)
                .await
                .change_context(TrustedServerError::Auction {
                    message: "stub launch failed".to_string(),
                })
        }

        async fn parse_response(
            &self,
            _response: PlatformResponse,
            response_time_ms: u64,
        ) -> Result<AuctionResponse, Report<TrustedServerError>> {
            Ok(AuctionResponse::success("stub_provider", vec![], response_time_ms))
        }

        fn timeout_ms(&self) -> u32 {
            2000
        }

        fn backend_name(&self, _timeout_ms: u32) -> Option<String> {
            Some("stub-backend".to_string())
        }
    }
```

Then add the test itself:

```rust
    #[tokio::test]
    async fn auction_endpoint_emits_completed_telemetry() {
        // A non-regulated, ungated auction completes (one provider launches via
        // the stub HTTP client and parses a no-bid success), so it must emit one
        // summary row tagged auction_api to the injected sink.
        let settings = create_test_settings();
        let config = AuctionConfig {
            enabled: true,
            providers: vec!["stub_provider".to_string()],
            timeout_ms: 2000,
            mediator: None,
            ..Default::default()
        };
        let mut orchestrator = AuctionOrchestrator::new(config);
        orchestrator.register_provider(Arc::new(StubLaunchProvider));
        let http_client = Arc::new(StubHttpClient::new());
        http_client.push_response(200, b"{}".to_vec());
        let sink = Arc::new(crate::auction::telemetry::InMemorySink::default());
        let services =
            build_services_with_http_client(http_client).with_auction_event_sink(sink.clone());
        let ec_id = format!("{}.ABC123", "a".repeat(64));
        let ec_context = make_ec_context(Jurisdiction::NonRegulated, Some(&ec_id));

        let body = json!({
            "adUnits": [
                {
                    "code": "div-gpt-ad-1",
                    "mediaTypes": { "banner": { "sizes": [[300, 250]] } }
                }
            ]
        });
        let req = Request::builder()
            .method("POST")
            .uri("https://test-publisher.com/auction")
            .body(EdgeBody::from(
                serde_json::to_vec(&body).expect("should serialize body"),
            ))
            .expect("should build auction request");

        let response = handle_auction(
            &settings,
            &orchestrator,
            None,
            None,
            &ec_context,
            &services,
            req,
        )
        .await
        .expect("auction should return a valid response");

        assert_eq!(response.status(), StatusCode::OK, "should return 200");
        let rows = sink.rows();
        assert!(
            rows.iter().any(|r| r.event_kind
                == crate::auction::telemetry::EventKind::Summary
                && r.auction_source == crate::auction::telemetry::AuctionSource::AuctionApi),
            "should emit a summary row tagged auction_api, got {} rows",
            rows.len()
        );
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p trusted-server-core auction_endpoint_emits_completed_telemetry`
Expected: FAIL — the assertion fails because nothing emits yet (`sink.rows()` is empty). (It compiles, because `with_auction_event_sink`/`InMemorySink` exist from Tasks 1-2.)

- [ ] **Step 3: Write minimal implementation**

Add this `use` to the top imports of `endpoints.rs` (with the other `use crate::auction::...` lines):

```rust
use crate::auction::telemetry::{build_completed_auction_events, build_observation_context, AuctionSource};
```

Insert the emission block immediately after the `log::info!("Auction completed: ...")` statement and before the `convert_to_openrtb_response(...)` line (endpoints.rs ~line 272). Geo and consent are read back off `auction_request` because the original locals were moved into the request builder:

```rust
    // Emit completed-auction telemetry. The sink write is buffered and
    // non-blocking in production and a no-op by default in tests, so this never
    // affects the response. Device signals are unknown (`2`) until a later plan
    // threads them through.
    let observation = build_observation_context(
        AuctionSource::AuctionApi,
        &auction_request.publisher.domain,
        auction_request.publisher.page_url.as_deref(),
        auction_request
            .device
            .as_ref()
            .and_then(|device| device.geo.as_ref()),
        auction_request.user.consent.as_ref(),
        2,
        2,
    );
    let slot_count = u16::try_from(auction_request.slots.len()).unwrap_or(u16::MAX);
    let telemetry_rows = build_completed_auction_events(&observation, slot_count, &result);
    services.auction_event_sink().emit(&telemetry_rows);
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p trusted-server-core auction_endpoint_emits_completed_telemetry`
Expected: PASS.

Run: `cargo test -p trusted-server-core`
Expected: PASS (whole core suite; confirms the existing consent-gate test still passes, i.e. the gated path still emits nothing because it returns before this code).

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-core/src/auction/endpoints.rs
git commit -m "Emit completed-auction telemetry from the auction endpoint"
```

---

### Task 4: Fastly auction event sink

**Files:**
- Modify: `crates/trusted-server-core/src/auction/telemetry/types.rs` (add `to_json_line_with_event_ts` + test)
- Modify: `crates/trusted-server-core/src/auction/telemetry/mod.rs` (re-export it)
- Create: `crates/trusted-server-adapter-fastly/src/auction_sink.rs`
- Modify: `crates/trusted-server-adapter-fastly/src/main.rs` (declare `mod auction_sink;`)
- Modify: `crates/trusted-server-adapter-fastly/src/platform.rs` (install the sink in `build_runtime_services`)
- Test: inline `#[cfg(test)]` in `types.rs`

**Interfaces:**
- Consumes: `AuctionEventRow`, `AuctionEventSink` (telemetry).
- Produces:
  - core: `pub fn to_json_line_with_event_ts(row: &AuctionEventRow, event_ts: &str) -> Result<String, serde_json::Error>` — the row as a single JSON object with an injected `event_ts` field.
  - adapter: `pub struct FastlyAuctionEventSink;` implementing `AuctionEventSink`, installed on the runtime services.

- [ ] **Step 1: Write the failing test (core helper)**

Add to the existing `#[cfg(test)] mod tests` in `telemetry/types.rs` (it already has a `sample_context()` helper from the core plan):

```rust
    #[test]
    fn json_line_injects_event_ts_and_keeps_row_fields() {
        let row = AuctionEventRow::base(&sample_context(), EventKind::Summary);
        let line = to_json_line_with_event_ts(&row, "2026-06-22T00:00:00.000Z")
            .expect("should serialize the row");
        let value: serde_json::Value =
            serde_json::from_str(&line).expect("line should be valid JSON");
        assert_eq!(
            value.get("event_ts").and_then(serde_json::Value::as_str),
            Some("2026-06-22T00:00:00.000Z"),
            "should inject event_ts"
        );
        assert!(value.get("event_kind").is_some(), "should retain the row fields");
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p trusted-server-core telemetry::types::tests::json_line_injects_event_ts_and_keeps_row_fields`
Expected: FAIL to compile (`to_json_line_with_event_ts` not found).

- [ ] **Step 3: Write the core helper**

Add to `telemetry/types.rs` (after the `to_ndjson` function):

```rust
/// Serialize one row to a single JSON object string with an injected `event_ts`
/// field. Core stays clock-free; the caller supplies the timestamp.
///
/// # Errors
///
/// Returns the underlying `serde_json` error if the row cannot be serialized.
pub fn to_json_line_with_event_ts(
    row: &AuctionEventRow,
    event_ts: &str,
) -> Result<String, serde_json::Error> {
    let mut value = serde_json::to_value(row)?;
    if let serde_json::Value::Object(map) = &mut value {
        map.insert(
            "event_ts".to_string(),
            serde_json::Value::String(event_ts.to_string()),
        );
    }
    serde_json::to_string(&value)
}
```

In `mod.rs`, add `to_json_line_with_event_ts` to the `pub use types::{...}` re-export list.

- [ ] **Step 4: Run the core test**

Run: `cargo test -p trusted-server-core telemetry::types`
Expected: PASS (includes the new test).

- [ ] **Step 5: Write the Fastly sink**

Create `crates/trusted-server-adapter-fastly/src/auction_sink.rs`:

```rust
//! Fastly implementation of the auction telemetry sink.
//!
//! Writes one NDJSON line per telemetry row to the `ts_auction_events`
//! real-time log endpoint, stamping a shared `event_ts` per batch. The write is
//! buffered by the host and flushed asynchronously, so it never blocks the
//! response.

use std::io::Write as _;

use chrono::{SecondsFormat, Utc};
use fastly::log::Endpoint;
use trusted_server_core::auction::telemetry::{
    to_json_line_with_event_ts, AuctionEventRow, AuctionEventSink,
};

/// Name of the Fastly real-time log endpoint provisioned for auction telemetry.
const AUCTION_EVENTS_ENDPOINT: &str = "ts_auction_events";

/// Sink that serializes telemetry rows to NDJSON and writes them to the Fastly
/// auction-events log endpoint.
pub struct FastlyAuctionEventSink;

impl AuctionEventSink for FastlyAuctionEventSink {
    fn emit(&self, rows: &[AuctionEventRow]) {
        if rows.is_empty() {
            return;
        }
        let event_ts = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
        let mut endpoint = Endpoint::from_name(AUCTION_EVENTS_ENDPOINT);
        for row in rows {
            match to_json_line_with_event_ts(row, &event_ts) {
                Ok(line) => {
                    if let Err(error) = writeln!(endpoint, "{line}") {
                        log::warn!("auction telemetry log write failed: {error}");
                        break;
                    }
                }
                Err(error) => {
                    log::warn!("auction telemetry serialization failed: {error}");
                }
            }
        }
    }
}
```

- [ ] **Step 6: Install the sink and declare the module**

In `crates/trusted-server-adapter-fastly/src/main.rs`, add the module declaration with the other `mod` lines:

```rust
mod auction_sink;
```

In `crates/trusted-server-adapter-fastly/src/platform.rs`, add the sink to `build_runtime_services` (after the `.client_info(...)` call, before `.build()`):

```rust
        .auction_event_sink(std::sync::Arc::new(crate::auction_sink::FastlyAuctionEventSink))
```

- [ ] **Step 7: Verify the adapter builds for wasm + core tests pass**

Run: `cargo check --package trusted-server-adapter-fastly --target wasm32-wasip1`
Expected: builds with no errors.

Run: `cargo test -p trusted-server-core telemetry`
Expected: PASS.

Run: `cargo fmt --all -- --check`
Expected: no diff.

Run: `cargo clippy -p trusted-server-core --all-targets --all-features -- -D warnings`
Expected: no warnings.

- [ ] **Step 8: Commit**

```bash
git add crates/trusted-server-core/src/auction/telemetry/types.rs crates/trusted-server-core/src/auction/telemetry/mod.rs crates/trusted-server-adapter-fastly/src/auction_sink.rs crates/trusted-server-adapter-fastly/src/main.rs crates/trusted-server-adapter-fastly/src/platform.rs
git commit -m "Add Fastly auction telemetry sink and install it on runtime services"
```

---

## Self-Review

**Spec coverage (this plan's slice):** Observation context construction from request/geo/consent with a fresh telemetry id and normalized page path (Task 1). Sink seam on `RuntimeServices` with a no-op default so existing call sites keep working (Task 2). `POST /auction` emits completed-auction rows (Task 3). The Fastly sink writes per-row NDJSON with `event_ts` to `ts_auction_events` (Task 4). Emission is off the response path (buffered host write), satisfying the no-TTFB-hold constraint.

**Deferred (not gaps in this plan):** `handle_page_bids` wiring (same pattern, heavier harness), SSAT dispatch/collect + abandoned/skipped/dispatch-failed/execution-failed outcomes, real `is_mobile`/`is_known_browser` population (passed as `2`), access logs, and the deferred Minor from the mapping plan (share the orchestrator's `ERROR_TYPE_*` constants).

**Placeholder scan:** No `TBD`/`TODO`; every code step shows complete code.

**Type consistency:** `build_observation_context` (7 args, `consent: Option<&ConsentContext>`) is defined in Task 1 and called identically in Task 3. `auction_event_sink()`/`with_auction_event_sink()` are defined in Task 2 and used in Tasks 2-4. `to_json_line_with_event_ts(&AuctionEventRow, &str) -> Result<String, serde_json::Error>` is defined in Task 4 core and consumed by the Task 4 adapter sink. `build_completed_auction_events(ctx, slot_count, result)` matches the mapping plan's signature. `Endpoint::from_name(&str) -> Self` + `Write` matches fastly 0.11.13.
