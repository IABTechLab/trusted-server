# Auction Telemetry Wiring (SSAT completed) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Emit `initial_navigation` telemetry when a server-side ad templates (SSAT) auction completes via `collect_dispatched_auction`, bringing the third auction source online for the completed path.

**Architecture:** The two publisher collect sites consume the `DispatchedAuction` (which carries the `AuctionRequest`). Each clones the request via a new getter before calling `collect_dispatched_auction`, then calls the shared `emit_completed_auction_telemetry` helper with `AuctionSource::InitialNavigation` after the result returns. No orchestrator behavior change beyond a read-only getter.

**Tech Stack:** Rust 2024, existing telemetry helper.

## Global Constraints

- Rust **2024 edition**. No `unwrap()` in non-test code (`unwrap_or`, `expect("should ...")` allowed). No `println!`/`eprintln!`.
- Comments on their own line above the code. No imports inside functions; no wildcard imports outside `#[cfg(test)]` (`use super::*;` allowed there).
- Tests: Arrange-Act-Assert, `expect()` with `"should ..."`, descriptive assertion messages, fictional domains only (existing SSAT test helpers use `test-publisher.com`, acceptable to mirror).
- Each public item has a doc comment.
- Commit messages: sentence case, imperative, no semantic prefixes, no bracketed tags, no `Co-Authored-By` trailer.
- Run `cargo fmt --all` before committing. Commit only when the focused test, `cargo fmt --all -- --check`, and `cargo clippy -p trusted-server-core --all-targets --all-features -- -D warnings` are all green.

**Scope boundary (NOT in this plan):** SSAT non-completed outcomes (abandoned dispatched tokens at the pass-through / buffered-unmodified branches, skipped, dispatch-failed), and access logs. Those are follow-ups.

**Verified facts (current code):**
- `DispatchedAuction` (orchestrator.rs:22) has a private `request: AuctionRequest` field and a `#[cfg(test)] impl` with `empty_for_test(request, timeout_ms)`. There is no non-test getter yet.
- `collect_dispatched_auction(&self, dispatched: DispatchedAuction, services, context) -> OrchestrationResult` (orchestrator.rs:854) consumes `dispatched` by value.
- Collect site A, `collect_stream_auction(dispatched: DispatchedAuction, price_granularity: PriceGranularity, ad_bids_state: &Arc<Mutex<Option<String>>>, orchestrator: &AuctionOrchestrator, services: &RuntimeServices, settings: &Settings)` (publisher.rs:954) — used by the HTML close-body hold loop. It calls `collect_dispatched_auction` then `write_bids_to_state`.
- Collect site B, the non-HTML branch in `stream_publisher_body_async` (publisher.rs:517-538) — calls `collect_dispatched_auction` then `write_bids_to_state` then returns.
- `publisher.rs` already imports `use crate::auction::telemetry::{emit_completed_auction_telemetry, AuctionSource};` (from a prior plan).
- Test helper `test_auction_request()` (publisher.rs:2072) returns an `AuctionRequest`. `DispatchedAuction::empty_for_test` and `PriceGranularity`, `Mutex`, `noop_services`, `AuctionOrchestrator` are available in the publisher test module (the existing `body_close_hold_loop_processes_close_tail...` test uses them). `collect_dispatched_auction` on an empty dispatched token returns an empty `OrchestrationResult` (no providers, no error).
- `emit_completed_auction_telemetry(services, source, request, result)` emits one summary row tagged with `source`.

---

### Task 1: Emit completed telemetry from the SSAT collect sites

**Files:**
- Modify: `crates/trusted-server-core/src/auction/orchestrator.rs` (add a `request()` getter to `DispatchedAuction`)
- Modify: `crates/trusted-server-core/src/publisher.rs` (emit at both collect sites; add a test)
- Test: inline `#[cfg(test)]` in `publisher.rs`

**Interfaces:**
- Consumes: `emit_completed_auction_telemetry`, `AuctionSource` (already imported in publisher.rs).
- Produces: `DispatchedAuction::request(&self) -> &AuctionRequest` (pub(crate)); both SSAT collect sites emit `initial_navigation` rows.

- [ ] **Step 1: Write the failing test**

Add to the publisher.rs `#[cfg(test)] mod tests` (it already imports `DispatchedAuction`, `AuctionOrchestrator`, `noop_services`, `Arc`, `Mutex`, `PriceGranularity`, and the `test_auction_request` helper; if any is missing, add it):

```rust
    #[tokio::test]
    async fn collect_stream_auction_emits_initial_navigation_telemetry() {
        // A completed SSAT auction (collected at the body-close hold) must emit
        // one summary row tagged initial_navigation to the injected sink. An
        // empty dispatched token collects to an empty result, which still emits
        // a summary.
        let settings = create_test_settings();
        let sink = Arc::new(crate::auction::telemetry::InMemorySink::default());
        let services = noop_services().with_auction_event_sink(sink.clone());
        let orchestrator = AuctionOrchestrator::new(settings.auction.clone());
        let dispatched = DispatchedAuction::empty_for_test(test_auction_request(), 500);
        let ad_bids_state = Arc::new(Mutex::new(None));

        collect_stream_auction(
            dispatched,
            PriceGranularity::default(),
            &ad_bids_state,
            &orchestrator,
            &services,
            &settings,
        )
        .await;

        let rows = sink.rows();
        assert!(
            rows.iter().any(|r| r.event_kind
                == crate::auction::telemetry::EventKind::Summary
                && r.auction_source == crate::auction::telemetry::AuctionSource::InitialNavigation),
            "should emit an initial_navigation summary, got {} rows",
            rows.len()
        );
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p trusted-server-core collect_stream_auction_emits_initial_navigation_telemetry`
Expected: FAIL — the assertion fails (`sink.rows()` empty) because nothing emits yet. It must COMPILE.

- [ ] **Step 3: Add the getter**

In `orchestrator.rs`, add a non-test impl block for the getter immediately after the `DispatchedAuction` struct definition (before the `#[cfg(test)] impl DispatchedAuction`):

```rust
impl DispatchedAuction {
    /// The auction request carried by this dispatched auction.
    pub(crate) fn request(&self) -> &AuctionRequest {
        &self.request
    }
}
```

- [ ] **Step 4: Emit at both collect sites**

In `publisher.rs`, in `collect_stream_auction` (the function that calls `collect_dispatched_auction` then `write_bids_to_state`), clone the request before collect and emit after `write_bids_to_state`. The function currently looks like:

```rust
    let collect_ctx = make_collect_context(settings, services, &placeholder);
    let result = orchestrator
        .collect_dispatched_auction(dispatched, services, &collect_ctx)
        .await;
```

Change it to capture the request first, then add the emit after the existing `write_bids_to_state(...)` call in that function:

```rust
    let collect_ctx = make_collect_context(settings, services, &placeholder);
    let request = dispatched.request().clone();
    let result = orchestrator
        .collect_dispatched_auction(dispatched, services, &collect_ctx)
        .await;
```

and, immediately after the `write_bids_to_state(...)` call already present in `collect_stream_auction`:

```rust
    // Emit completed-auction telemetry off the response path.
    emit_completed_auction_telemetry(
        services,
        AuctionSource::InitialNavigation,
        &request,
        &result,
    );
```

In the non-HTML branch of `stream_publisher_body_async`, apply the same pattern. The branch currently is:

```rust
        let result = orchestrator
            .collect_dispatched_auction(
                dispatched,
                services,
                &make_collect_context(settings, services, &placeholder),
            )
            .await;
        write_bids_to_state(
            &result.winning_bids,
            params.price_granularity,
            &params.ad_bids_state,
            settings.debug.inject_adm_for_testing,
        );
        return stream_publisher_body(body, output, params, settings, integration_registry);
```

Change it to capture the request before collect and emit after `write_bids_to_state`:

```rust
        let request = dispatched.request().clone();
        let result = orchestrator
            .collect_dispatched_auction(
                dispatched,
                services,
                &make_collect_context(settings, services, &placeholder),
            )
            .await;
        write_bids_to_state(
            &result.winning_bids,
            params.price_granularity,
            &params.ad_bids_state,
            settings.debug.inject_adm_for_testing,
        );
        // Emit completed-auction telemetry off the response path.
        emit_completed_auction_telemetry(
            services,
            AuctionSource::InitialNavigation,
            &request,
            &result,
        );
        return stream_publisher_body(body, output, params, settings, integration_registry);
```

- [ ] **Step 5: Run test to verify it passes + gates**

Run: `cargo test -p trusted-server-core collect_stream_auction_emits_initial_navigation_telemetry`
Expected: PASS.

Run: `cargo test -p trusted-server-core`
Expected: PASS.

Run: `cargo fmt --all -- --check` (after `cargo fmt --all`) and `cargo clippy -p trusted-server-core --all-targets --all-features -- -D warnings`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add crates/trusted-server-core/src/auction/orchestrator.rs crates/trusted-server-core/src/publisher.rs
git commit -m "Emit completed-auction telemetry from the SSAT collect path"
```

---

## Self-Review

**Spec coverage:** Completed SSAT auctions (collected at the body-close hold and the non-HTML branch) emit `initial_navigation` rows via the shared helper. All three auction sources now emit on the completed path: `auction_api`, `spa_navigation`, `initial_navigation`.

**Deferred (not gaps):** SSAT non-completed outcomes (abandoned dispatched tokens, skipped, dispatch-failed) and access logs.

**Placeholder scan:** No `TBD`/`TODO`; complete code.

**Type consistency:** `DispatchedAuction::request()` returns `&AuctionRequest`; `.clone()` yields the owned `AuctionRequest` the helper takes by reference. `emit_completed_auction_telemetry(services, AuctionSource::InitialNavigation, &request, &result)` matches the helper signature used by the other two handlers. The test uses the existing `DispatchedAuction::empty_for_test` / `test_auction_request` harness.
