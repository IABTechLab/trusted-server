# Request Phase Timing Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Every application response attributes its own server time by phase via a
Server-Timing header and a sampled Tinybird access-telemetry row.

**Architecture:** A core `RequestTimings` handle (Arc-shared, infallible recording)
collects phase spans always-on; the Fastly adapter freezes and emits at
`send_edgezero_response` immediately before `into_parts()`; a post-send emitter ships
one NDJSON row to the Tinybird Events API with a bounded, 2xx-validated await.

**Tech Stack:** Rust 2024, `edgezero` HTTP types, Fastly Compute (wasm32-wasip1,
Viceroy tests), Axum (native tests), Tinybird Events API.

**Spec:** `docs/superpowers/specs/2026-08-24-request-phase-timing-design.md`: the
plan argues from the spec; executors read both. Spec section numbers are cited per
task.

## Global Constraints

- Errors use `error-stack` (`Report<TrustedServerError>`); errors defined with
  `derive_more::Display`; never thiserror, never anyhow (except the Spin entry point).
- No `unwrap()` in production code; `expect("should ...")` only. Assertion messages
  `"should ..."`. Tests use Arrange-Act-Assert.
- No inline comments; comments on their own line above the code.
- Functions never exceed 7 arguments; use a struct instead (this bit
  `ec_finalize_response` in review; the timings handle travels inside existing state).
- No local imports inside functions; `use super::*` only in `#[cfg(test)]`.
- Only example/fictional data in tests and docs (`example.com` domains).
- Recording is infallible: saturating math, lock failure drops the sample, no panics
  (spec 5, 13).
- Vendor identity never appears in emitted surfaces: the filter span is `ts-filter`
  (spec 3).
- Test commands: `cargo test-axum` (native, fast inner loop), `cargo test-fastly`
  (Viceroy) for adapter tasks. Before PR handoff: the full CI gate list in
  `CLAUDE.md`.
- Commit style: sentence case, imperative, no prefixes, no trailers.

## File Structure

| File                                                              | Responsibility                                                                                     |
| ----------------------------------------------------------------- | -------------------------------------------------------------------------------------------------- |
| `crates/trusted-server-core/src/request_timing.rs` (new)          | `Phase`, `AuctionWaitPlacement`, `RequestTimings`, `PhaseSpan`, `TimingSnapshot`, header rendering |
| `crates/trusted-server-core/src/access_telemetry.rs` (new)        | `RouteClass`, `publisher_route_template`, `AccessTelemetrySnapshot`, `AccessEventRow` NDJSON       |
| `crates/trusted-server-core/src/geo.rs` (modify)                  | `GeoLookupState` response-extension type                                                           |
| `crates/trusted-server-core/src/settings.rs` (modify)             | `ObservabilitySettings`, tinybird flag decoupling, access validation                               |
| `crates/trusted-server-core/src/publisher.rs` (modify)            | `ts-origin`, `ts-template-cache`, auction-wait spans                                               |
| `crates/trusted-server-core/src/ec/kv.rs` (modify)                | `ts-kv` at the graph abstraction                                                                   |
| `crates/trusted-server-adapter-fastly/src/main.rs` (modify)       | T0, appbuild span, freeze point, `DeliveryOutcome`, post-send emission ordering                    |
| `crates/trusted-server-adapter-fastly/src/app.rs` (modify)        | filter span, geo span + `GeoLookupState` attach, route class assignment                            |
| `crates/trusted-server-adapter-fastly/src/middleware.rs` (modify) | finalize consumes `GeoLookupState`                                                                 |
| `crates/trusted-server-adapter-fastly/src/tinybird.rs` (modify)   | access sink with confirmed delivery                                                                |
| `crates/trusted-server-adapter-axum/src/` (modify)                | terminal freeze layer, header emission                                                             |
| `tinybird/datasources/access_logs_raw.datasource` (modify)        | phase-column schema, non-null sorting key                                                          |
| `trusted-server.example.toml` (modify)                            | `[observability]`, tinybird keys                                                                   |

Out of scope for this plan: the Grafana dashboard JSON (separate telemetry repo,
spec 11) and Cloudflare/Spin emission wiring (spec non-goal).

---

### Task 1: Core `RequestTimings`

**Files:**

- Create: `crates/trusted-server-core/src/request_timing.rs`
- Modify: `crates/trusted-server-core/src/lib.rs` (add `pub mod request_timing;`)
- Test: same file, `#[cfg(test)]`

**Interfaces:**

- Consumes: nothing (leaf module; `std::time`, `std::sync`).
- Produces (later tasks rely on these exact names):
  - `pub enum Phase { AppBuild, Filter, Geo, EcKv, Origin, TemplateCacheLookup, AuctionWait, Stream }`
  - `pub enum AuctionWaitPlacement { PreHeader, InStream }`
  - `#[derive(Clone)] pub struct RequestTimings` with:
    - `pub fn new() -> Self`
    - `pub fn record(&self, phase: Phase, dur: Duration)` (saturating accumulate)
    - `pub fn record_auction_wait(&self, placement: AuctionWaitPlacement, dur: Duration)`
    - `pub fn span(&self, phase: Phase) -> PhaseSpan` (records on drop)
    - `pub fn mark_headers_ready(&self)` (first call wins)
    - `pub fn mark_request_elapsed(&self)` (first call wins)
    - `pub fn set_resp_bytes(&self, bytes: u64)`
    - `pub fn server_timing_value(&self) -> Option<String>`
    - `pub fn snapshot(&self) -> TimingSnapshot`
  - `pub struct TimingSnapshot { pub time_elapsed_ms: Option<u32>, pub request_elapsed_ms: Option<u32>, pub appbuild_ms: Option<u32>, pub filter_ms: Option<u32>, pub geo_ms: Option<u32>, pub kv_ms: Option<u32>, pub origin_ms: Option<u32>, pub template_cache_ms: Option<u32>, pub auction_wait_ms: Option<u32>, pub stream_ms: Option<u32>, pub auction_wait_placement: Option<AuctionWaitPlacement>, pub resp_bytes: Option<u64> }`

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_omits_unrecorded_phases_and_orders_total_first() {
        let timings = RequestTimings::new();
        timings.record(Phase::Filter, Duration::from_micros(9_100));
        timings.mark_headers_ready();
        let value = timings
            .server_timing_value()
            .expect("should render after mark_headers_ready");
        assert!(
            value.starts_with("ts-total;dur="),
            "should lead with ts-total: {value}"
        );
        assert!(value.contains("ts-filter;dur=9.1"), "should render one decimal: {value}");
        assert!(!value.contains("ts-geo"), "should omit unrecorded phases: {value}");
    }

    #[test]
    fn render_returns_none_before_headers_ready() {
        let timings = RequestTimings::new();
        timings.record(Phase::Geo, Duration::from_millis(1));
        assert!(timings.server_timing_value().is_none(), "should require the snapshot");
    }

    #[test]
    fn repeated_phases_accumulate_saturating() {
        let timings = RequestTimings::new();
        timings.record(Phase::Geo, Duration::from_millis(2));
        timings.record(Phase::Geo, Duration::from_millis(3));
        timings.mark_headers_ready();
        let snapshot = timings.snapshot();
        assert_eq!(snapshot.geo_ms, Some(5), "should accumulate repeats");
    }

    #[test]
    fn mark_headers_ready_is_first_call_wins() {
        let timings = RequestTimings::new();
        timings.mark_headers_ready();
        let first = timings.snapshot().time_elapsed_ms;
        std::thread::sleep(Duration::from_millis(5));
        timings.mark_headers_ready();
        assert_eq!(timings.snapshot().time_elapsed_ms, first, "should not restamp");
    }

    #[test]
    fn span_guard_records_on_drop() {
        let timings = RequestTimings::new();
        {
            let _span = timings.span(Phase::Origin);
            std::thread::sleep(Duration::from_millis(2));
        }
        timings.mark_headers_ready();
        assert!(
            timings.snapshot().origin_ms.expect("should record on drop") >= 1,
            "should measure elapsed span time"
        );
    }

    #[test]
    fn auction_wait_records_placement() {
        let timings = RequestTimings::new();
        timings.record_auction_wait(AuctionWaitPlacement::PreHeader, Duration::from_millis(40));
        let snapshot = timings.snapshot();
        assert_eq!(snapshot.auction_wait_ms, Some(40), "should record wait");
        assert_eq!(
            snapshot.auction_wait_placement,
            Some(AuctionWaitPlacement::PreHeader),
            "should record placement"
        );
    }

    #[test]
    fn rendered_names_never_include_vendor_terms() {
        let timings = RequestTimings::new();
        for phase in [Phase::AppBuild, Phase::Filter, Phase::Geo, Phase::EcKv, Phase::Origin, Phase::TemplateCacheLookup] {
            timings.record(phase, Duration::from_millis(1));
        }
        timings.mark_headers_ready();
        let value = timings.server_timing_value().expect("should render");
        assert!(!value.to_ascii_lowercase().contains("datadome"), "should mask vendors");
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test-axum -p trusted-server-core request_timing`
Expected: compile FAIL, module does not exist.

- [ ] **Step 3: Implement**

```rust
//! Per-request phase timing collection and Server-Timing rendering.
//!
//! Collection is always-on and infallible: saturating math, lock failure
//! drops the sample, no panics. See the design spec
//! `docs/superpowers/specs/2026-08-24-request-phase-timing-design.md`.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const PHASE_COUNT: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    AppBuild,
    Filter,
    Geo,
    EcKv,
    Origin,
    TemplateCacheLookup,
    AuctionWait,
    Stream,
}

impl Phase {
    fn index(self) -> usize { /* match self -> 0..=7 */ }

    /// Header entry name; row-only phases return None.
    fn header_name(self) -> Option<&'static str> {
        match self {
            Self::AppBuild => Some("ts-appbuild"),
            Self::Filter => Some("ts-filter"),
            Self::Geo => Some("ts-geo"),
            Self::EcKv => Some("ts-kv"),
            Self::Origin => Some("ts-origin"),
            Self::TemplateCacheLookup => Some("ts-template-cache"),
            Self::AuctionWait | Self::Stream => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuctionWaitPlacement {
    PreHeader,
    InStream,
}

struct Inner {
    t0: Instant,
    phases: [Option<Duration>; PHASE_COUNT],
    headers_ready_total: Option<Duration>,
    request_elapsed: Option<Duration>,
    auction_wait_placement: Option<AuctionWaitPlacement>,
    resp_bytes: Option<u64>,
}

#[derive(Clone)]
pub struct RequestTimings(Arc<Mutex<Inner>>);
```

Implementation notes (all bodies in this task, none deferred):

- Every method takes `if let Ok(mut inner) = self.0.lock()` and silently returns on
  poison, per the infallibility constraint.
- `record` accumulates with `saturating_add` semantics
  (`Some(existing.saturating_add(dur))`).
- `mark_headers_ready` and `mark_request_elapsed` write `t0.elapsed()` only when the
  slot is `None`.
- `server_timing_value` returns `None` unless `headers_ready_total` is set; renders
  `ts-total` first from the stored snapshot, then the six header phases in enum order
  with `{:.1}` millisecond formatting (`dur.as_secs_f64() * 1000.0`).
- `PhaseSpan { timings: RequestTimings, phase: Phase, started: Instant }`; `Drop`
  calls `record(self.phase, self.started.elapsed())`.
- `TimingSnapshot` converts each `Duration` with
  `u32::try_from(dur.as_millis()).unwrap_or(u32::MAX)`.
- `impl Default for RequestTimings` delegates to `new()`.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test-axum -p trusted-server-core request_timing`
Expected: all 7 tests PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-core/src/request_timing.rs crates/trusted-server-core/src/lib.rs
git commit -m "Add RequestTimings phase collection and Server-Timing rendering"
```

---

### Task 2: Settings: `[observability]`, tinybird decoupling, access validation

**Files:**

- Modify: `crates/trusted-server-core/src/settings.rs`
- Modify: `trusted-server.example.toml`

**Interfaces:**

- Produces:
  - `pub struct ObservabilitySettings { pub server_timing_enabled: bool }` as
    `settings.observability`, `#[serde(default)]` on the field and
    `#[serde(skip_serializing_if = "ObservabilitySettings::is_default")]`.
  - `TinybirdSettings.auction_enabled: bool` (`#[serde(default = "default_true")]`).
  - `prepare_runtime` validation: `access_enabled` requires `enabled`, non-empty
    `api_host`, `secret_store`, `access_dataset`, `access_token_secret`,
    `max_body_bytes > 0`, and `access_sample_rate > 0.0`.

- [ ] **Step 1: Write the failing tests** (in `settings.rs` tests module)

```rust
#[test]
fn observability_defaults_off_and_serializes_away() {
    let settings = create_test_settings();
    assert!(!settings.observability.server_timing_enabled, "should default off");
    let toml = toml::to_string(&settings).expect("should serialize settings");
    assert!(
        !toml.contains("[observability]"),
        "should omit the default table so a prior binary can parse the config"
    );
}

#[test]
fn access_enabled_requires_positive_sample_rate() {
    // access_enabled = true with access_sample_rate = 0 is armed-but-silent: an error.
    let err = settings_from_toml_with(
        "[tinybird]\nenabled = true\napi_host = \"api.example.com\"\naccess_enabled = true\naccess_sample_rate = 0.0\n",
    )
    .expect_err("should reject armed-but-silent access telemetry");
    assert!(format!("{err:?}").contains("access_sample_rate"), "should name the field");
}

#[test]
fn access_and_auction_emission_are_independent() {
    let settings = settings_from_toml_with(
        "[tinybird]\nenabled = true\napi_host = \"api.example.com\"\nauction_enabled = false\naccess_enabled = true\naccess_sample_rate = 1.0\n",
    )
    .expect("should accept access without auction");
    assert!(!settings.tinybird.auction_enabled, "should disable auction emission");
    assert!(settings.tinybird.access_enabled, "should enable access emission");
}

#[test]
fn auction_enabled_defaults_true_for_existing_configs() {
    let settings = settings_from_toml_with("[tinybird]\nenabled = true\napi_host = \"api.example.com\"\n")
        .expect("should parse a pre-decoupling config");
    assert!(settings.tinybird.auction_enabled, "should preserve current behavior");
}
```

Also REPLACE the existing rejection test
(`tinybird_access_enabled_is_rejected_until_emitter_is_wired`, `settings.rs:4123`)
with a wiring test asserting a fully-specified access config is accepted.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test-axum -p trusted-server-core observability access_enabled auction_enabled`
Expected: compile FAIL (`observability` field missing).

- [ ] **Step 3: Implement**

- Add `ObservabilitySettings` (derive `Debug, Clone, Default, PartialEq, Deserialize,
Serialize`, `#[serde(deny_unknown_fields)]`), with
  `fn is_default(&self) -> bool { *self == Self::default() }`.
- Add the `observability` field to `Settings` with the serde attributes above.
- Add `auction_enabled` to `TinybirdSettings` with `default_true()`; update
  `Default for TinybirdSettings`.
- Extend `TinybirdSettings::prepare_runtime` with the access validation matrix; error
  messages name the failing field (`"tinybird.access_sample_rate must be > 0 when
access_enabled"` and so on).
- `trusted-server.example.toml`: add a commented `[observability]` block with
  `server_timing_enabled = false` present-but-false and the env-override note (the
  overlay cannot create a missing leaf), plus `auction_enabled`/access keys in the
  tinybird section comments.
- Gate the auction sink: in `crates/trusted-server-adapter-fastly/src/app.rs`,
  `auction_sink_from_settings` condition becomes
  `settings.tinybird.enabled && settings.tinybird.auction_enabled`.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test-axum -p trusted-server-core` then `cargo test-fastly` (the sink gate
touches the Fastly adapter).
Expected: PASS, including the replaced wiring test.

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-core/src/settings.rs trusted-server.example.toml crates/trusted-server-adapter-fastly/src/tinybird.rs crates/trusted-server-adapter-fastly/src/app.rs
git commit -m "Add observability settings and decouple tinybird access and auction emission"
```

---

### Task 3: Fastly freeze point, header emission, `DeliveryOutcome`

**Files:**

- Modify: `crates/trusted-server-adapter-fastly/src/main.rs` (entry T0, appbuild span,
  `send_edgezero_response`)
- Test: `crates/trusted-server-adapter-fastly/src/app.rs` tests module (route-level
  tests run under Viceroy)

**Interfaces:**

- Consumes: `RequestTimings`, `Phase` (Task 1);
  `trusted_server_core::cache_policy::cache_control_headers_are_private_or_no_store`.
- Produces:
  - `RequestTimings` inserted into request extensions at dispatch
    (`core_req.extensions_mut().insert(timings.clone())`), alongside the existing
    `config_store`/`device_signals`/`client_info` inserts.
  - `send_edgezero_response(response, effects, timings) -> DeliveryOutcome` where
    `pub(crate) struct DeliveryOutcome { pub bytes: u64, pub result: DeliveryResult }`
    and `pub(crate) enum DeliveryResult { Complete, Error }` (streaming partial
    detection lands in Task 6).

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn server_timing_emitted_on_private_response_when_enabled() {
    // Arrange: settings with observability.server_timing_enabled = true; publisher
    // route fixture whose response is Cache-Control: private, no-store.
    // Act: dispatch through the full adapter path.
    // Assert:
    let header = response_header(&response, "server-timing").expect("should emit header");
    assert!(header.contains("ts-total;dur="), "should carry the stored total");
    assert_eq!(
        header.matches("ts-total").count(), 1,
        "should emit exactly one TS-owned metric set"
    );
}

#[test]
fn server_timing_absent_when_flag_off() { /* same fixture, flag false: no ts-total */ }

#[test]
fn server_timing_absent_on_cacheable_responses() {
    // tsjs route (public, max-age=31536000, immutable) and a bare max-age=60 response:
    // both must carry no ts-total even with the flag on.
}

#[test]
fn preexisting_server_timing_values_survive() {
    // Fixture response already carrying Server-Timing: upstream;dur=1 stays present
    // alongside the appended TS set.
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test-fastly server_timing`
Expected: FAIL, no header emitted.

- [ ] **Step 3: Implement**

In `edgezero_main` (`main.rs`):

```rust
let timings = RequestTimings::new();
{
    let _appbuild = timings.span(Phase::AppBuild);
    // existing: open_trusted_server_config_store() + build_app_with_state()
}
```

Move the config-store open inside the span scope. Insert `timings.clone()` into
request extensions before dispatch. Thread the handle into both send sites and the
error paths by value (it is a cheap clone).

In `send_edgezero_response`, immediately before `response.into_parts()`:

```rust
timings.mark_headers_ready();
let conclusively_private =
    cache_control_headers_are_private_or_no_store(response.headers());
if settings_enabled_server_timing && conclusively_private {
    if let Some(value) = timings.server_timing_value() {
        match HeaderValue::from_str(&value) {
            Ok(header_value) => {
                response.headers_mut().append(header::SERVER_TIMING, header_value);
            }
            Err(error) => log::warn!("skipping server-timing header: {error}"),
        }
    }
}
```

`settings_enabled_server_timing` arrives as a `bool` captured from the settings
snapshot at the call sites (the function already receives per-call context; extend its
parameters, staying at or under seven, or pass a small
`SendContext { timings: RequestTimings, server_timing_enabled: bool }`). Return
`DeliveryOutcome`; existing callers ignore it in this task (Task 8 consumes it).

- [ ] **Step 4: Run to verify pass**

Run: `cargo test-fastly` and `cargo clippy-fastly`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-adapter-fastly/src/main.rs crates/trusted-server-adapter-fastly/src/app.rs
git commit -m "Emit Server-Timing at the send freeze point on conclusively private responses"
```

---

### Task 4: Filter span and geo span with `GeoLookupState` dedupe

**Files:**

- Modify: `crates/trusted-server-core/src/geo.rs` (add `GeoLookupState`)
- Modify: `crates/trusted-server-adapter-fastly/src/app.rs`
  (`run_pre_route_filters` wrapper, `build_ec_request_state` geo span + state attach)
- Modify: `crates/trusted-server-adapter-fastly/src/middleware.rs` and `main.rs`
  (`resolve_geo_for_response` consumes carried state)

**Interfaces:**

- Consumes: `RequestTimings` from request extensions (Task 3).
- Produces:
  - `pub enum GeoLookupState { NotAttempted, Attempted, Resolved(GeoInfo) }` in
    `trusted_server_core::geo`, attached as a response extension on every exit path
    that attempted a lookup (including the asset fallback).
  - `resolve_geo_for_response` gains the carried state as input: live lookup only on
    `NotAttempted`; `Attempted` is never retried; fallback lookups are wrapped in
    `timings.span(Phase::Geo)`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn finalize_reuses_request_phase_geo_without_second_lookup() {
    // Counting geo stub: dispatch a publisher route; assert lookup count == 1 and
    // x-geo-country still set on the response.
}

#[test]
fn failed_lookup_is_not_retried() {
    // Stub returns None once; assert GeoLookupState::Attempted carried and the
    // finalize path performs zero further lookups.
}

#[test]
fn asset_fallback_carries_geo_state_without_ec_finalize_state() {
    // Asset route: response extension holds GeoLookupState, EcFinalizeState absent.
}

#[test]
fn filter_span_recorded_when_request_filter_runs() {
    // Registry fixture with a test request filter; assert snapshot().filter_ms is Some.
}

#[test]
fn geo_lookup_skipped_for_unauthorized_responses() {
    // Existing 401 rule preserved: no lookup, state NotAttempted.
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test-fastly geo_ filter_span`
Expected: FAIL (lookup count 2; no `GeoLookupState`).

- [ ] **Step 3: Implement**

- `GeoLookupState` derives `Debug, Clone`; store in response extensions from the
  dispatch layer right after `build_ec_request_state` resolves (or fails) its lookup.
- Wrap the `build_ec_request_state` lookup and any finalize fallback lookup in
  `timings.span(Phase::Geo)` (accumulating slot handles the repeat case).
- Wrap `run_pre_route_filters` (`app.rs:751`) in `timings.span(Phase::Filter)`,
  recording only when at least one filter is registered (skip the span when the
  registry has no request filters, so the header omits `ts-filter` on unconfigured
  deployments).
- `resolve_geo_for_response(response, carried: &GeoLookupState, client_ip, lookup)`
  keeps the 401 short-circuit first, then matches the carried state.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test-fastly` and `cargo test-axum`
Expected: PASS including untouched existing geo header tests.

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-core/src/geo.rs crates/trusted-server-adapter-fastly/src/app.rs crates/trusted-server-adapter-fastly/src/middleware.rs crates/trusted-server-adapter-fastly/src/main.rs
git commit -m "Record filter and geo spans and dedupe the per-request geo lookup"
```

---

### Task 5: Core spans: origin, template cache, KV abstraction

**Files:**

- Modify: `crates/trusted-server-core/src/publisher.rs` (origin send ~4496, template
  cache lookup ~4391 on current main)
- Modify: `crates/trusted-server-core/src/ec/kv.rs` (graph-level `ts-kv`)
- Test: `publisher.rs` and `ec/kv.rs` test modules

**Interfaces:**

- Consumes: `RequestTimings` read from request extensions inside
  `handle_publisher_request`; `KvIdentityGraph` gains
  `pub fn with_timings(self, timings: RequestTimings) -> Self` (builder-style,
  optional field), set where the graph is constructed in `main.rs`.
- Produces: `origin_ms`, `template_cache_ms`, `kv_ms` populated in snapshots.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn origin_span_covers_the_publisher_fetch() {
    // Stubbed origin with a small injected delay; assert snapshot().origin_ms is Some.
}

#[test]
fn template_cache_span_recorded_only_when_lookup_runs() {
    // Inline mode fixture: template_cache_ms None. Shared-mode eligible fixture:
    // template_cache_ms Some.
}

#[test]
fn kv_span_accumulates_across_graph_operations() {
    // Stub KV recording two operations through KvIdentityGraph; assert kv_ms Some and
    // covers both (accumulated, not last-write).
}

#[test]
fn ec_finalize_kv_lands_before_freeze() {
    // Adapter-level (test-fastly): EC-enabled fixture with eids cookies; assert the
    // emitted header contains ts-kv, proving the freeze point sits after finalize.
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test-axum -p trusted-server-core origin_span template_cache_span kv_span`
Expected: FAIL.

- [ ] **Step 3: Implement**

- `handle_publisher_request` reads the handle:
  `let timings = req.extensions().get::<RequestTimings>().cloned().unwrap_or_default();`
  (a defaulted handle records into nothing that ever renders, keeping non-adapter
  tests unchanged).
- Origin: `let origin_span = timings.span(Phase::Origin);` immediately before
  `services.http_client().send(platform_request).await`; `drop(origin_span)` when the
  response headers are available (directly after the `match` arm binds the response).
- Template cache: same guard pattern around
  `services.template_cache().lookup_or_reserve(key).await`.
- KV: `KvIdentityGraph` stores `timings: Option<RequestTimings>`; each public
  operation (`get`, `write_entry`, `create_or_revive`, `upsert_partner_ids`,
  `write_withdrawal_tombstone`, batch accessors) wraps its store call in
  `Phase::EcKv` spans when the handle is present. `ec_finalize_response` keeps seven
  arguments: the handle rides inside the graph, which it already receives.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test-axum` then `cargo test-fastly`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-core/src/publisher.rs crates/trusted-server-core/src/ec/kv.rs crates/trusted-server-adapter-fastly/src/main.rs
git commit -m "Record origin, template cache, and KV phase spans in core"
```

---

### Task 6: Body-phase capture: stream, auction wait placement, bytes

**Files:**

- Modify: `crates/trusted-server-core/src/publisher.rs` (seam wait + buffered wait)
- Modify: `crates/trusted-server-adapter-fastly/src/main.rs` (stream drive timing,
  `DeliveryOutcome.bytes`)
- Test: `publisher.rs` tests + adapter tests

**Interfaces:**

- Consumes: `record_auction_wait` (Task 1), `DeliveryOutcome` (Task 3).
- Produces: `stream_ms`, `auction_wait_ms` + placement, `resp_bytes`,
  `mark_request_elapsed()` called by the adapter immediately after the stream drive
  returns.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn streaming_seam_wait_records_in_stream_placement() {
    // Streaming fixture with a delayed auction: placement InStream, and
    // stream_ms >= auction_wait_ms.
}

#[test]
fn buffered_template_miss_records_pre_header_placement() {
    // Shared-template authorized miss (buffered finalizer): placement PreHeader; the
    // wait is recorded even though headers had not committed.
}

#[test]
fn delivery_outcome_reports_bytes_and_request_elapsed_set() {
    // Adapter: after send, snapshot has resp_bytes Some(body_len) and
    // request_elapsed_ms Some; request_elapsed excludes post-send emitter time by
    // construction (asserted by ordering test in Task 8).
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test-fastly seam_wait buffered_template delivery_outcome`
Expected: FAIL.

- [ ] **Step 3: Implement**

- Streaming path: around the `collect_stream_auction(...)` await inside the body
  stream, measure with `Instant::now()` and call
  `timings.record_auction_wait(AuctionWaitPlacement::InStream, waited)`. The handle
  reaches the stream closure through `OwnedProcessResponseParams`/assembly params (it
  is `Clone`; add a field).
- Buffered path (`buffer_publisher_response_async` and the shared-template miss
  finalizer): same measurement with `AuctionWaitPlacement::PreHeader`.
- Adapter stream drive: wrap the `block_on(stream_asset_body(...))` region; record
  `Phase::Stream` with the elapsed drive time, count bytes written into
  `DeliveryOutcome.bytes`, call `timings.set_resp_bytes(bytes)` and
  `timings.mark_request_elapsed()` immediately after the drive returns, before
  anything else post-send.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test-fastly` and `cargo test-axum`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-core/src/publisher.rs crates/trusted-server-adapter-fastly/src/main.rs
git commit -m "Capture stream duration, auction wait placement, and response bytes"
```

---

### Task 7: `AccessTelemetrySnapshot`, route class, route template

**Files:**

- Create: `crates/trusted-server-core/src/access_telemetry.rs`
- Modify: `crates/trusted-server-core/src/lib.rs`
- Modify: `crates/trusted-server-adapter-fastly/src/app.rs` (route class at handler
  selection), `main.rs` (snapshot build at freeze point)

**Interfaces:**

- Consumes: `TimingSnapshot` (Task 1), `GeoLookupState` (Task 4).
- Produces:
  - `pub enum RouteClass { PublisherHtml, Tsjs, IntegrationProxy, Ec, AuctionApi, Other }`
    with `pub fn as_str(&self) -> &'static str` (snake_case values from the spec).
  - `pub fn publisher_route_template(path: &str) -> String`: `/` plus first segment
    filtered to `[a-z0-9_-]`, truncated to 32 chars, plus `/*` when deeper; empty or
    disallowed first segments render `/other/*`.
  - `pub struct AccessTelemetrySnapshot { pub method: String, pub status: u16, pub route_class: RouteClass, pub route_template: String, pub publisher_domain: String, pub env: String, pub service_id: String, pub pop: String, pub ts_version: String, pub country: String, pub template_cache_state: String, pub body_mode: &'static str, pub sample_rate: f64 }`
  - `pub fn access_event_row(snapshot: &AccessTelemetrySnapshot, timings: &TimingSnapshot, event_ts_epoch_ms: u64) -> String` (one NDJSON line).

- [ ] **Step 1: Write the failing tests** (adversarial, per spec 9)

```rust
#[test]
fn admin_ec_route_template_never_contains_the_identifier() {
    // Named-route template comes from the route table: "/_ts/admin/ec/{id}".
    // Assert a row built for that route never contains a 64-hex EC id fixture.
}

#[test]
fn publisher_paths_normalize_to_coarse_templates() {
    assert_eq!(publisher_route_template("/news/some-article-slug"), "/news/*");
    assert_eq!(publisher_route_template("/"), "/");
    assert_eq!(
        publisher_route_template("/user@example.com/profile"),
        "/other/*",
        "should reject non-allowlisted characters"
    );
    assert_eq!(
        publisher_route_template(&format!("/{}", "a".repeat(500))),
        format!("/{}", "a".repeat(32)),
        "should bound segment length"
    );
    assert_eq!(publisher_route_template("/search terms here"), "/other/*");
}

#[test]
fn row_serializes_nulls_for_missing_phases() {
    // Sparse TimingSnapshot: absent phases serialize as JSON null, dimension fields
    // never null (unknown sentinel).
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test-axum -p trusted-server-core access_telemetry route_template`
Expected: compile FAIL.

- [ ] **Step 3: Implement**

Row serialization via `serde_json::json!` mapping spec section 9 column names exactly
(`time_elapsed_ms`, `appbuild_ms`, ..., `auction_wait_placement` as
`pre_header|in_stream|none`). `env`/`pop`/`service_id` read by the adapter from
settings state and Fastly env (`FASTLY_SERVICE_ID`, `FASTLY_POP`), defaulting
`"unknown"`. Route class assigned in the named-route table (add a `RouteClass` column
to `NAMED_ROUTES`) and `RouteClass::PublisherHtml`/`Tsjs` for the fallback and tsjs
handlers. The snapshot is built unconditionally in `send_edgezero_response` right
after `mark_headers_ready()` and returned to the caller inside `DeliveryOutcome` (add
field `pub snapshot: AccessTelemetrySnapshot`).

- [ ] **Step 4: Run to verify pass**

Run: `cargo test-axum` and `cargo test-fastly`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-core/src/access_telemetry.rs crates/trusted-server-core/src/lib.rs crates/trusted-server-adapter-fastly/src/app.rs crates/trusted-server-adapter-fastly/src/main.rs
git commit -m "Add access telemetry snapshot, route classes, and coarse route templates"
```

---

### Task 8: Access sink with confirmed delivery + post-send ordering

**Files:**

- Modify: `crates/trusted-server-adapter-fastly/src/tinybird.rs` (access sink)
- Modify: `crates/trusted-server-adapter-fastly/src/main.rs` (post-send ordering)

**Interfaces:**

- Consumes: `AccessTelemetrySnapshot` + `access_event_row` (Task 7), settings flags
  (Task 2), `DeliveryOutcome` (Tasks 3/6).
- Produces: `pub(crate) async fn emit_access_event(services: &RuntimeServices, target: &TinybirdEventsTarget, row: String) -> Result<(), Report<TrustedServerError>>` ,
  sends via `http_client().send(...)` (the blocking variant, post-delivery), checks
  `response.status().is_success()`, warns with status otherwise.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn access_emitter_posts_ndjson_and_validates_2xx() {
    // RecordingHttpClient returning 202: assert URI is /v0/events?name=access_logs_raw,
    // body is the row, Authorization bearer from the secret stub.
}

#[test]
fn access_emitter_warns_and_drops_on_non_2xx() {
    // RecordingHttpClient returning 422: emit returns Err naming the status; no retry
    // request recorded (exactly one request seen).
}

#[test]
fn sampled_out_requests_emit_nothing() {
    // access_sample_rate stub decision false: RecordingHttpClient sees zero requests.
}

#[test]
fn post_send_order_is_elapsed_then_pull_sync_then_telemetry() {
    // Instrumented stubs record call order; assert request_elapsed snapshot precedes
    // pull-sync dispatch which precedes the telemetry send.
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test-fastly access_emitter post_send_order`
Expected: FAIL.

- [ ] **Step 3: Implement**

- Reuse `TinybirdEventsTarget` with a second constructor
  `from_access_config(config: TinybirdSettings)` using `access_dataset` and
  `access_token_secret`.
- Sampling decision: `fn sampled_in(rate: f64, entropy: u64) -> bool` where entropy is
  derived from the event timestamp nanos XOR a per-request counter (no `rand`
  dependency; document that uniformity is approximate and sufficient).
- `main.rs` post-send, in order: `timings.mark_request_elapsed()` (already placed in
  Task 6), existing pull-sync dispatch unchanged, then when
  `settings.tinybird.enabled && settings.tinybird.access_enabled` and sampled in:
  build the row from `outcome.snapshot` + `timings.snapshot()`, call
  `emit_access_event`, log one warning on `Err`.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test-fastly` and `cargo clippy-fastly`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-adapter-fastly/src/tinybird.rs crates/trusted-server-adapter-fastly/src/main.rs
git commit -m "Emit confirmed access telemetry rows after pull-sync post-send"
```

---

### Task 9: Tinybird datasource schema

**Files:**

- Modify: `tinybird/datasources/access_logs_raw.datasource`

**Interfaces:**

- Consumes: column names exactly as serialized by `access_event_row` (Task 7).
- Produces: the deployed schema contract for the dashboard (separate repo).

- [ ] **Step 1: Rewrite the schema** per spec section 9: keep
      `event_ts DateTime64(3)`, `method`, `status UInt16`, `time_elapsed_ms UInt32`,
      `sample_rate Float64`, `event_date` + 30-day TTL; add the columns from spec 9 with
      dimension columns non-nullable `LowCardinality(String)` and phase columns
      `Nullable(UInt32)`; drop `path` and `cache_state`; set
      `ENGINE_SORTING_KEY "event_date, service_id, publisher_domain, env, route_class, pop, status"`.

- [ ] **Step 2: Validate** with the tinybird toolchain if available locally
      (`tb check` / project tests under `tinybird/tests`); otherwise assert the file
      parses by review and rely on rollout step 4's remote verification. Add a fixture row
      in `tinybird/fixtures` matching `access_event_row` output.

- [ ] **Step 3: Commit**

```bash
git add tinybird/datasources/access_logs_raw.datasource tinybird/fixtures
git commit -m "Extend access_logs_raw with phase columns and a non-null sorting key"
```

---

### Task 10: Axum adapter emission

**Files:**

- Modify: `crates/trusted-server-adapter-axum/src/` (terminal layer at the response
  serialization boundary; locate the equivalent of the Fastly send path)
- Test: axum adapter tests (`cargo test-axum`)

**Interfaces:**

- Consumes: `RequestTimings`, header emission helper. Extract the emission block from
  Task 3 into a shared core helper so both adapters call one function:
  `pub fn append_server_timing_if_private(response: &mut Response, timings: &RequestTimings, enabled: bool)`
  in `request_timing.rs` (move the Fastly inline logic here and re-point Task 3's call
  site).
- Produces: Axum responses carry the header under the same conservative predicate;
  `ts-appbuild` absent by construction (state built at startup); router-generated
  404/405 covered by the terminal layer; `/health` excluded by route match.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn axum_emits_header_on_private_response() { /* flag on, private response: ts-total present, ts-appbuild absent */ }

#[test]
fn axum_404_carries_header_when_private() { /* router-generated 404 passes through the terminal layer */ }

#[test]
fn axum_health_is_excluded() { /* /health: no ts-total */ }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test-axum axum_emits axum_404 axum_health`
Expected: FAIL.

- [ ] **Step 3: Implement** the terminal layer: create `RequestTimings::new()` per
      request at the outermost service layer, insert into request extensions, and at the
      layer's response side call `mark_headers_ready()` +
      `append_server_timing_if_private(...)`, skipping the `/health` path.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test-axum`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-adapter-axum/src crates/trusted-server-core/src/request_timing.rs crates/trusted-server-adapter-fastly/src/main.rs
git commit -m "Emit Server-Timing from the Axum terminal layer with adapter-specific semantics"
```

---

### Task 11: Full gate, docs, and PR

- [ ] **Step 1: Docs.** Add a short operator section to `docs/guide/configuration.md`:
      the `[observability]` flag, the tinybird access keys, the deploy/rollback ordering
      from spec section 12 (binary first, config second; config first on rollback), and
      the conservative emission rule. Run `cd docs && npm run format`.

- [ ] **Step 2: Full CI gate list** from `CLAUDE.md`:
      `cargo fmt --all -- --check`; all six clippy aliases; `test-fastly`, `test-axum`,
      `test-cloudflare`, `test-spin`; the integration-tests parity suite; JS build/test
      and formats. Cloudflare/Spin compile the new core modules (collection only), which
      is exactly what the non-goal requires.

- [ ] **Step 3: Commit docs, push the branch, open the implementation PR** referencing
      the spec PR #1069 and issue #1068, with the rollout section of the spec quoted as
      the deployment checklist (staging pass-through + MISS/HIT replay before production
      flag-on).

---

## Self-Review

- Spec coverage: sections 5 (Task 1), 12 (Task 2), 7 (Tasks 3, 10), 8/8a (Tasks 4,
  10), 6 (Tasks 4-6), 9 (Tasks 7, 9), 10 (Task 8), 13 (Tasks 1, 3, 8), 14 (test
  steps throughout), 15 steps 1-4 (Task 11 + deployment checklist). Section 11
  (dashboard) is explicitly out of scope for this repo's plan.
- Type consistency: `RequestTimings`/`TimingSnapshot`/`RouteClass`/
  `AccessTelemetrySnapshot`/`DeliveryOutcome` names and signatures match across
  Tasks 1, 3, 6, 7, 8, 10.
- Known intentional deferral: `DeliveryResult::Partial` detection is named in Task 3
  and wired when the stream drive reports bytes in Task 6; no other deferrals.
