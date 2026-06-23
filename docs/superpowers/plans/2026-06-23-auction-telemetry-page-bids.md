# Auction Telemetry Wiring (page-bids) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make a completed `GET /__ts/page-bids` (SPA navigation) auction emit telemetry, by extracting a shared emission helper and calling it from both `handle_auction` and `handle_page_bids`.

**Architecture:** A new `auction::telemetry::emit::emit_completed_auction_telemetry` builds the observation context from the `AuctionRequest` (reading geo/consent off the request, which both handlers populate), runs the Plan 1/2 builder, and emits via the runtime sink. `handle_auction` is refactored to call it (DRY), and `handle_page_bids` calls it in its `Ok(result)` branch. No orchestrator or response-path changes.

**Tech Stack:** Rust 2024, existing telemetry module.

## Global Constraints

- Rust **2024 edition**. No `unwrap()` in non-test code (`u16::try_from(..).unwrap_or(u16::MAX)`, `unwrap_or`, `expect("should ...")` allowed). No `println!`/`eprintln!`.
- Functions take at most 7 args. Comments on their own line above the code. No imports inside functions; no wildcard imports outside `#[cfg(test)]` (`use super::*;` allowed there).
- Tests: Arrange-Act-Assert, `expect()` with `"should ..."`, descriptive assertion messages, fictional domains only (`example.com`; the existing page-bids tests use `test-publisher.com` for the request URI, which is acceptable to mirror).
- Each public item has a doc comment.
- Commit messages: sentence case, imperative, no semantic prefixes, no bracketed tags, no `Co-Authored-By` trailer.
- Run `cargo fmt --all` before committing (a prior task forgot it). Commit only when the focused test, `cargo fmt --all -- --check`, and `cargo clippy -p trusted-server-core --all-targets --all-features -- -D warnings` are all green.

**Scope boundary (NOT in this plan):** the SSAT dispatch/collect path and its abandoned/skipped outcomes, real device signals (`is_mobile`/`is_known_browser` stay `2`), access logs.

**Verified facts (current code):**
- `handle_page_bids(settings, services: &RuntimeServices, kv, auction: AuctionDispatch<'_>, ec_context, req)` (publisher.rs:1733). Its `Ok(result)` branch is `Ok(result) => result.winning_bids` (publisher.rs:1878). `auction_request`, `services`, `geo`, `consent_context` are all alive there; `build_auction_request` sets `user.consent = Some(consent_context.clone())` and geo is set on `auction_request.device.geo`, so reading geo/consent off `auction_request` is correct.
- `handle_auction` (endpoints.rs) currently has an inline emission block added in the previous plan, using `build_observation_context` + `build_completed_auction_events` + `services.auction_event_sink().emit(..)`, and imports `use crate::auction::telemetry::{build_completed_auction_events, build_observation_context, AuctionSource};`.
- `AuctionDispatch<'a> { orchestrator, slots, registry }` (publisher.rs:1016). `AuctionOrchestrator` rejects empty providers and all-launch-failed auctions; a completing auction needs a provider that launches via `services.http_client().send_async` and parses a no-bid success (the `StubHttpClient` harness).
- The publisher test module already imports `build_services_with_http_client, noop_services, StubHttpClient`. Test helpers `settings_with_co()`, `article_slot()`, `make_page_bids_request(path)`, `consent_allowing_ec_context()` exist. `is_bot_user_agent` only flags UAs containing bot fragments, so a request with no UA is not a bot.
- Telemetry re-exports live under `crate::auction::telemetry` (`build_observation_context`, `build_completed_auction_events`, `AuctionSource`, `EventKind`, `InMemorySink`). `RuntimeServices::with_auction_event_sink` and `auction_event_sink()` exist.

---

### Task 1: Shared emission helper

**Files:**
- Create: `crates/trusted-server-core/src/auction/telemetry/emit.rs`
- Modify: `crates/trusted-server-core/src/auction/telemetry/mod.rs` (declare `emit`, re-export the helper)
- Test: inline `#[cfg(test)]` in `emit.rs`

**Interfaces:**
- Consumes: `build_observation_context`, `build_completed_auction_events`, `AuctionSource` (telemetry); `AuctionRequest` (auction::types); `OrchestrationResult` (orchestrator); `RuntimeServices` (platform).
- Produces: `pub fn emit_completed_auction_telemetry(services: &RuntimeServices, source: AuctionSource, request: &AuctionRequest, result: &OrchestrationResult)` — builds rows for a completed auction and emits them; reads geo/consent off `request`; device signals unknown (`2`).

- [ ] **Step 1: Write the failing test**

Create `crates/trusted-server-core/src/auction/telemetry/emit.rs` with the test module first:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::auction::telemetry::{EventKind, InMemorySink};
    use crate::auction::types::{PublisherInfo, UserInfo};
    use crate::platform::test_support::noop_services;
    use std::collections::HashMap;
    use std::sync::Arc;

    fn request() -> AuctionRequest {
        AuctionRequest {
            id: "internal-id".to_string(),
            slots: vec![],
            publisher: PublisherInfo {
                domain: "example.com".to_string(),
                page_url: Some("https://example.com/news?x=1".to_string()),
            },
            user: UserInfo {
                id: None,
                consent: None,
                eids: None,
            },
            device: None,
            site: None,
            context: HashMap::new(),
        }
    }

    fn empty_result() -> OrchestrationResult {
        OrchestrationResult {
            provider_responses: vec![],
            mediator_response: None,
            winning_bids: HashMap::new(),
            total_time_ms: 0,
            metadata: HashMap::new(),
        }
    }

    #[test]
    fn emits_one_summary_tagged_with_the_given_source() {
        let sink = Arc::new(InMemorySink::default());
        let services = noop_services().with_auction_event_sink(sink.clone());

        emit_completed_auction_telemetry(
            &services,
            AuctionSource::SpaNavigation,
            &request(),
            &empty_result(),
        );

        let rows = sink.rows();
        let summary = rows
            .iter()
            .find(|r| r.event_kind == EventKind::Summary)
            .expect("should emit a summary row");
        assert_eq!(
            summary.auction_source,
            AuctionSource::SpaNavigation,
            "should tag the summary with the given source"
        );
        assert_eq!(
            summary.publisher_domain, "example.com",
            "should carry the publisher domain"
        );
        assert_eq!(
            summary.page_path, "/news",
            "should carry the normalized page path"
        );
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p trusted-server-core telemetry::emit`
Expected: FAIL to compile (`emit` module not declared; `emit_completed_auction_telemetry` not found).

- [ ] **Step 3: Write minimal implementation**

Prepend to `emit.rs` (above the test module):

```rust
//! Wiring helper that emits completed-auction telemetry from a handler.
//!
//! Reads geo and consent off the `AuctionRequest` (a handler's local copies may
//! have been moved). Device signals are unknown (`2`) until a later plan threads
//! them. The sink write is buffered/non-blocking in production.

use crate::auction::orchestrator::OrchestrationResult;
use crate::auction::telemetry::context::build_observation_context;
use crate::auction::telemetry::mapping::build_completed_auction_events;
use crate::auction::telemetry::types::AuctionSource;
use crate::auction::types::AuctionRequest;
use crate::platform::RuntimeServices;

/// Build and emit completed-auction telemetry for a finished auction.
pub fn emit_completed_auction_telemetry(
    services: &RuntimeServices,
    source: AuctionSource,
    request: &AuctionRequest,
    result: &OrchestrationResult,
) {
    let observation = build_observation_context(
        source,
        &request.publisher.domain,
        request.publisher.page_url.as_deref(),
        request.device.as_ref().and_then(|device| device.geo.as_ref()),
        request.user.consent.as_ref(),
        2,
        2,
    );
    let slot_count = u16::try_from(request.slots.len()).unwrap_or(u16::MAX);
    let rows = build_completed_auction_events(&observation, slot_count, result);
    services.auction_event_sink().emit(&rows);
}
```

In `mod.rs`: add `pub mod emit;` (alphabetically, before `pub mod mapping;`) and add `pub use emit::emit_completed_auction_telemetry;` to the re-export block.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p trusted-server-core telemetry::emit`
Expected: PASS (1 test).

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-core/src/auction/telemetry/emit.rs crates/trusted-server-core/src/auction/telemetry/mod.rs
git commit -m "Add shared completed-auction telemetry emission helper"
```

---

### Task 2: Refactor handle_auction onto the helper

**Files:**
- Modify: `crates/trusted-server-core/src/auction/endpoints.rs` (replace the inline emission block + its import)
- Test: the existing `auction_endpoint_emits_completed_telemetry` is the regression gate (no new test).

**Interfaces:**
- Consumes: `emit_completed_auction_telemetry`, `AuctionSource` (Task 1 / telemetry).
- Produces: no behavior change; `handle_auction` now emits via the shared helper.

- [ ] **Step 1: Replace the import**

In `endpoints.rs`, change the telemetry import line from:

```rust
use crate::auction::telemetry::{build_completed_auction_events, build_observation_context, AuctionSource};
```

to:

```rust
use crate::auction::telemetry::{emit_completed_auction_telemetry, AuctionSource};
```

- [ ] **Step 2: Replace the inline emission block**

Replace the inline emission block (the `let observation = build_observation_context(...)` through `services.auction_event_sink().emit(&telemetry_rows);`, immediately after the `log::info!("Auction completed: ...")` and before `convert_to_openrtb_response(...)`) with a single call:

```rust
    // Emit completed-auction telemetry off the response path via the shared
    // helper. Buffered/non-blocking in production, no-op by default in tests.
    emit_completed_auction_telemetry(
        services,
        AuctionSource::AuctionApi,
        &auction_request,
        &result,
    );
```

- [ ] **Step 3: Run the regression test + gates**

Run: `cargo test -p trusted-server-core auction_endpoint_emits_completed_telemetry`
Expected: PASS (unchanged behavior; the helper does exactly what the inline block did).

Run: `cargo test -p trusted-server-core`
Expected: PASS.

Run: `cargo fmt --all -- --check` (after `cargo fmt --all`) and `cargo clippy -p trusted-server-core --all-targets --all-features -- -D warnings`
Expected: clean (in particular, no unused-import warning for the removed `build_observation_context`/`build_completed_auction_events`).

- [ ] **Step 4: Commit**

```bash
git add crates/trusted-server-core/src/auction/endpoints.rs
git commit -m "Refactor auction endpoint emission onto the shared helper"
```

---

### Task 3: Emit telemetry from handle_page_bids

**Files:**
- Modify: `crates/trusted-server-core/src/publisher.rs` (import, emit in the `Ok` branch, add a test provider + test)
- Test: inline `#[cfg(test)]` in `publisher.rs`

**Interfaces:**
- Consumes: `emit_completed_auction_telemetry`, `AuctionSource` (telemetry).
- Produces: `GET /__ts/page-bids` emits a `spa_navigation` row set on a completed auction.

- [ ] **Step 1: Write the failing test**

The publisher test module already imports `build_services_with_http_client`, `noop_services`, `StubHttpClient`, `Arc`, `StatusCode`, `Method`, `Request`, `EdgeBody`, `AuctionOrchestrator`, `AuctionDispatch`, and the page-bids helpers. Add any of the following that are not already imported, to the `use` lines of the `#[cfg(test)] mod tests` block:

```rust
    use crate::auction::config::AuctionConfig;
    use crate::auction::provider::AuctionProvider;
    use crate::auction::types::{AuctionContext, AuctionRequest, AuctionResponse};
    use crate::platform::{PlatformHttpRequest, PlatformPendingRequest, PlatformResponse};
    use error_stack::{Report, ResultExt as _};
```

Add this provider struct inside the `tests` module (it launches via the stub HTTP client and parses a no-bid success, so the auction completes — the path that emits):

```rust
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

Add the test:

```rust
    #[tokio::test]
    async fn page_bids_emits_spa_navigation_telemetry() {
        // A consent-allowed page-bids auction that completes (one provider
        // launches via the stub HTTP client and parses a no-bid success) must
        // emit one summary row tagged spa_navigation to the injected sink.
        let settings = settings_with_co();
        let config = AuctionConfig {
            enabled: true,
            providers: vec!["stub_provider".to_string()],
            timeout_ms: 2000,
            mediator: None,
            ..Default::default()
        };
        let mut orchestrator = AuctionOrchestrator::new(config);
        orchestrator.register_provider(Arc::new(StubLaunchProvider));
        let slots = article_slot();
        let http_client = Arc::new(StubHttpClient::new());
        http_client.push_response(200, b"{}".to_vec());
        let sink = Arc::new(crate::auction::telemetry::InMemorySink::default());
        let services =
            build_services_with_http_client(http_client).with_auction_event_sink(sink.clone());
        let ec_context = consent_allowing_ec_context();
        let req = make_page_bids_request("/2024/01/my-article/");

        let response = handle_page_bids(
            &settings,
            &services,
            None,
            AuctionDispatch {
                orchestrator: &orchestrator,
                slots: &slots,
                registry: None,
            },
            &ec_context,
            req,
        )
        .await
        .expect("should return ok response");

        assert_eq!(response.status(), StatusCode::OK, "should return 200");
        let rows = sink.rows();
        assert!(
            rows.iter().any(|r| r.event_kind
                == crate::auction::telemetry::EventKind::Summary
                && r.auction_source == crate::auction::telemetry::AuctionSource::SpaNavigation),
            "should emit a summary row tagged spa_navigation, got {} rows",
            rows.len()
        );
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p trusted-server-core page_bids_emits_spa_navigation_telemetry`
Expected: FAIL — the assertion fails (`sink.rows()` empty) because page-bids does not emit yet. It must COMPILE.

- [ ] **Step 3: Write minimal implementation**

Add the import with the other `use crate::auction::...` lines at the top of `publisher.rs`:

```rust
use crate::auction::telemetry::{emit_completed_auction_telemetry, AuctionSource};
```

Change the `run_auction` `Ok` branch (publisher.rs ~line 1878) from:

```rust
            Ok(result) => result.winning_bids,
```

to:

```rust
            Ok(result) => {
                // Emit completed-auction telemetry off the response path.
                emit_completed_auction_telemetry(
                    services,
                    AuctionSource::SpaNavigation,
                    &auction_request,
                    &result,
                );
                result.winning_bids
            }
```

- [ ] **Step 4: Run test to verify it passes + gates**

Run: `cargo test -p trusted-server-core page_bids_emits_spa_navigation_telemetry`
Expected: PASS.

Run: `cargo test -p trusted-server-core`
Expected: PASS.

Run: `cargo fmt --all -- --check` (after `cargo fmt --all`) and `cargo clippy -p trusted-server-core --all-targets --all-features -- -D warnings`
Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-core/src/publisher.rs
git commit -m "Emit completed-auction telemetry from the page-bids handler"
```

---

## Self-Review

**Spec coverage (this plan's slice):** A shared, unit-tested emission helper (Task 1); `handle_auction` refactored onto it with no behavior change (Task 2); `GET /__ts/page-bids` emits a `spa_navigation` row set on a completed auction (Task 3). Both `run_auction` call sites now emit; emission is off the response path.

**Deferred (not gaps):** SSAT dispatch/collect + non-completed outcomes, real device signals, access logs.

**Placeholder scan:** No `TBD`/`TODO`; every code step is complete.

**Type consistency:** `emit_completed_auction_telemetry(services, source, request, result)` is defined in Task 1 and called identically in Tasks 2 and 3. The `StubLaunchProvider` mirrors the proven harness used in the auction-endpoint test. `build_completed_auction_events`/`build_observation_context`/`AuctionSource` signatures match the prior plans.
