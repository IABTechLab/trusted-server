# Auction Telemetry Device Signals Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Populate `is_mobile` and `is_known_browser` on auction telemetry rows with real values derived from the request, replacing the hardcoded `2` (unknown).

**Architecture:** The shared `emit_completed_auction_telemetry` helper already has the request and `RuntimeServices`. It computes `DeviceSignals::derive(ua, ja4, h2)` from the request's user agent and the client's TLS JA4 / H2 fingerprint (already carried in `ClientInfo`), then maps to the `0`/`1`/`2` schema columns. Single-helper change; both auction handlers benefit at once.

**Tech Stack:** Rust 2024, existing `crate::ec::device::DeviceSignals`.

## Global Constraints

- Rust **2024 edition**. No `unwrap()` in non-test code (`unwrap_or`, `expect("should ...")` allowed). No `println!`/`eprintln!`.
- Comments on their own line above the code. No imports inside functions; no wildcard imports outside `#[cfg(test)]`.
- Tests: Arrange-Act-Assert, `expect()` with `"should ..."`, descriptive assertion messages, fictional domains only.
- Commit messages: sentence case, imperative, no semantic prefixes, no bracketed tags, no `Co-Authored-By` trailer.
- Run `cargo fmt --all` before committing. Commit only when the focused test, `cargo fmt --all -- --check`, and `cargo clippy -p trusted-server-core --all-targets --all-features -- -D warnings` are all green.

**Verified facts (current code):**

- `crate::ec::device::DeviceSignals::derive(ua: &str, ja4: Option<&str>, h2_fp: Option<&str>) -> DeviceSignals`; fields `is_mobile: u8` (0=desktop, 1=mobile, 2=unknown via `parse_is_mobile`: iPhone/iPad/Android→1, Macintosh/Windows/Linux→0, else→2) and `known_browser: Option<bool>` (`None` when JA4 or H2 is absent).
- `RuntimeServices::client_info() -> &ClientInfo`; `ClientInfo { tls_ja4: Option<String>, h2_fingerprint: Option<String>, .. }`.
- `emit_completed_auction_telemetry(services, source, request, result)` lives in `crates/trusted-server-core/src/auction/telemetry/emit.rs` and currently passes `2, 2` as the device-signal args to `build_observation_context`. The request's UA is at `request.device.as_ref().and_then(|d| d.user_agent.as_deref())`.
- `AuctionRequest.device: Option<DeviceInfo { user_agent: Option<String>, ip, geo }>` (auction::types).

---

### Task 1: Derive device signals in the emission helper

**Files:**

- Modify: `crates/trusted-server-core/src/auction/telemetry/emit.rs` (import, replace the `2, 2` args, add a test)
- Test: inline `#[cfg(test)]` in `emit.rs`

**Interfaces:**

- Consumes: `DeviceSignals::derive`, `ClientInfo` (via `services.client_info()`).
- Produces: no signature change to `emit_completed_auction_telemetry`; the emitted rows now carry derived `is_mobile`/`is_known_browser`.

- [ ] **Step 1: Write the failing test**

Add to the existing `#[cfg(test)] mod tests` in `emit.rs` (it already imports `EventKind`, `InMemorySink`, `PublisherInfo`, `UserInfo`, `noop_services`, `HashMap`, `Arc`, and defines `request()` and `empty_result()`). Add the `DeviceInfo` import to the test `use` lines:

```rust
    use crate::auction::types::DeviceInfo;
```

Then add the test:

```rust
    #[test]
    fn derives_is_mobile_from_user_agent() {
        let sink = Arc::new(InMemorySink::default());
        let services = noop_services().with_auction_event_sink(sink.clone());
        let mut req = request();
        req.device = Some(DeviceInfo {
            user_agent: Some(
                "Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) AppleWebKit/605.1.15"
                    .to_string(),
            ),
            ip: None,
            geo: None,
        });

        emit_completed_auction_telemetry(
            &services,
            AuctionSource::AuctionApi,
            &req,
            &empty_result(),
        );

        let rows = sink.rows();
        let summary = rows
            .iter()
            .find(|r| r.event_kind == EventKind::Summary)
            .expect("should emit a summary row");
        assert_eq!(summary.is_mobile, 1, "an iPhone user agent should classify as mobile");
        assert_eq!(
            summary.is_known_browser, 2,
            "with no JA4/H2 fingerprint the browser-legitimacy signal is unknown"
        );
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p trusted-server-core telemetry::emit`
Expected: FAIL — `derives_is_mobile_from_user_agent` fails because the helper still passes `2, 2`, so `summary.is_mobile` is `2`, not `1`. (The existing `emits_one_summary_tagged_with_the_given_source` test still passes; its request has `device: None`, so `is_mobile` stays `2`.)

- [ ] **Step 3: Write minimal implementation**

In `emit.rs`, add the import with the other top-of-file `use` lines:

```rust
use crate::ec::device::DeviceSignals;
```

Replace the body of `emit_completed_auction_telemetry` so it derives the signals and passes them to `build_observation_context` (replacing the `2, 2` args):

```rust
pub fn emit_completed_auction_telemetry(
    services: &RuntimeServices,
    source: AuctionSource,
    request: &AuctionRequest,
    result: &OrchestrationResult,
) {
    let user_agent = request
        .device
        .as_ref()
        .and_then(|device| device.user_agent.as_deref())
        .unwrap_or("");
    let client_info = services.client_info();
    let signals = DeviceSignals::derive(
        user_agent,
        client_info.tls_ja4.as_deref(),
        client_info.h2_fingerprint.as_deref(),
    );
    // Map the optional browser-legitimacy bit to the 0/1/2 schema column.
    let is_known_browser = match signals.known_browser {
        Some(true) => 1,
        Some(false) => 0,
        None => 2,
    };
    let observation = build_observation_context(
        source,
        &request.publisher.domain,
        request.publisher.page_url.as_deref(),
        request.device.as_ref().and_then(|device| device.geo.as_ref()),
        request.user.consent.as_ref(),
        signals.is_mobile,
        is_known_browser,
    );
    let slot_count = u16::try_from(request.slots.len()).unwrap_or(u16::MAX);
    let rows = build_completed_auction_events(&observation, slot_count, result);
    services.auction_event_sink().emit(&rows);
}
```

- [ ] **Step 4: Run test to verify it passes + gates**

Run: `cargo test -p trusted-server-core telemetry::emit`
Expected: PASS (2 tests).

Run: `cargo test -p trusted-server-core`
Expected: PASS.

Run: `cargo fmt --all -- --check` (after `cargo fmt --all`) and `cargo clippy -p trusted-server-core --all-targets --all-features -- -D warnings`
Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-core/src/auction/telemetry/emit.rs
git commit -m "Derive real device signals for auction telemetry rows"
```

---

## Self-Review

**Spec coverage:** `is_mobile`/`is_known_browser` now derive from the request UA and client JA4/H2 instead of hardcoded `2`. Both `handle_auction` and `handle_page_bids` benefit because they share the helper.

**Placeholder scan:** No `TBD`/`TODO`; complete code.

**Type consistency:** `DeviceSignals::derive(ua, ja4, h2)` and `signals.is_mobile`/`signals.known_browser` match the verified API; `ClientInfo.tls_ja4`/`h2_fingerprint` are `Option<String>` and passed as `Option<&str>` via `as_deref()`. The helper signature is unchanged, so the call sites from the prior plan are unaffected.
