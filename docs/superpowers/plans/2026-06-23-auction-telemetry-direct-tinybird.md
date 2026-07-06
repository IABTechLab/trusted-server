# Auction telemetry direct Tinybird implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans or an equivalent task-by-task execution workflow before coding. This plan is intentionally implementation-only guidance; do not treat the revised spec as optional.

**Goal:** Replace the prior Fastly real-time logging + relay direction with direct, best-effort Tinybird Events API ingestion from Fastly Compute. Auction telemetry is required for phase 1. Ops access telemetry is optional and must stay configuration-gated.

**Core invariant:** Telemetry failure must never fail, delay, or change a customer request. Backend creation, Secret Store lookup, JSON/NDJSON serialization, and `send_async` setup errors are logged at a bounded level and dropped.

**Delivery tradeoff:** Direct Tinybird ingestion has weaker delivery guarantees than Fastly real-time logging. It has no Fastly-managed batching, retry, response inspection, durable replay, or delivery confirmation because the application intentionally calls `send_async` and drops the pending Tinybird response.

**Reference spec:** `docs/superpowers/specs/2026-06-22-auction-prebid-metrics-tinybird-grafana-design.md`

---

## 1. Desired implementation summary

Implement a core-owned auction telemetry lifecycle and row builder, plus a Fastly adapter-owned Tinybird direct-ingest sink:

```text
auction candidate
  -> create AuctionObservationContext with fresh telemetry UUID
  -> run / skip / dispatch / collect / abandon auction
  -> build one bounded batch of summary/provider_call/bid rows
  -> serialize rows as NDJSON
  -> dynamic backend https://<tb-api-host> with SNI + Host = <tb-api-host>
  -> POST /v0/events?name=auction_events_raw
  -> Authorization: Bearer <append token from Fastly Secret Store>
  -> Request::send_async(...)
  -> drop PendingRequest without waiting for Tinybird response
```

The implementation must:

- cover `POST /auction`, `GET /__ts/page-bids`, and initial-navigation SSAT dispatch/collect;
- emit exactly one summary row per auction candidate per successful Compute execution path;
- batch all rows for one auction observation into one Tinybird Events API POST;
- omit EC ID, internal `AuctionRequest.id`, IP, raw user agent, full URL, query strings, and fragments;
- generate a fresh random telemetry UUID unrelated to EC or request IDs;
- emit provider-call rows for provider launch, parse, transport, timeout/no-response, no-bid, success, and abandoned outcomes where observable;
- avoid invented seat-level no-bids;
- keep the customer response path isolated from telemetry failures.

---

## 2. Current code areas inspected

### Auction core

- `crates/trusted-server-core/src/auction/mod.rs`
  - Exports auction types and orchestrator.
- `crates/trusted-server-core/src/auction/endpoints.rs`
  - `handle_auction` runs the explicit `POST /auction` path.
  - It short-circuits on consent denial with an empty OpenRTB response.
- `crates/trusted-server-core/src/auction/orchestrator.rs`
  - `run_auction` handles synchronous page-bids and explicit auction execution.
  - `dispatch_auction` launches split-phase SSAT provider requests and currently returns `Option<DispatchedAuction>`.
  - `collect_dispatched_auction` collects split-phase responses and returns best-effort `OrchestrationResult`.
  - Launch failures are represented as `AuctionResponse::error` in the synchronous path, but split dispatch currently logs launch failures without retaining them on the token.
  - Outstanding split requests that are never collected are currently dropped by caller branches.
- `crates/trusted-server-core/src/auction/types.rs`
  - `AuctionRequest.id` can be EC-derived on SSAT paths and must not be reused for telemetry.
  - `AuctionResponse`, `Bid`, and `BidStatus` contain most row-builder source data.

### Publisher auction paths

- `crates/trusted-server-core/src/publisher.rs`
  - `handle_publisher_request` creates SSAT auction requests, calls `dispatch_auction`, and later returns `PublisherResponse::Stream` carrying `OwnedProcessResponseParams.dispatched_auction`.
  - `stream_publisher_body_async`, `stream_html_with_auction_hold`, `body_close_hold_loop`, and `collect_stream_auction` collect SSAT auctions during streaming after headers/body chunks can already be committed.
  - `ResponseRoute::PassThrough` and `ResponseRoute::BufferedUnmodified` currently log that dispatched SSP requests will not be collected; those branches need explicit abandonment telemetry.
  - Origin proxy errors after dispatch currently return an error and drop the dispatch token; those also need abandonment telemetry.
  - `handle_page_bids` runs SPA auctions synchronously with `run_auction` and swallows auction errors into an empty bids response.

### Platform and Fastly adapter

- `crates/trusted-server-core/src/platform/`
  - `RuntimeServices` already exposes `secret_store()`, `backend()`, `http_client()`, `geo()`, and `client_info()`.
  - `PlatformHttpClient::send_async` is platform-neutral and returns `PlatformPendingRequest`.
- `crates/trusted-server-adapter-fastly/src/platform.rs`
  - `FastlyPlatformSecretStore` reads Fastly Secret Store values.
  - `FastlyPlatformBackend` delegates dynamic backend creation to `BackendConfig`.
  - `FastlyPlatformHttpClient::send_async` converts edge HTTP requests to `fastly::Request`, calls Fastly `send_async`, and does not require awaiting a response.
- `crates/trusted-server-adapter-fastly/src/backend.rs`
  - Dynamic backend creation already sets `.enable_ssl().sni_hostname(self.host)` and `.override_host(&host_header)`.
  - For a Tinybird API host, use `scheme=https`, `host=<tb-api-host>`, no host override unless needed; SNI and Host will match the host.
- `crates/trusted-server-adapter-fastly/src/main.rs`
  - Legacy path routes all initial-navigation SSAT today.
  - EdgeZero path currently disables publisher fallback SSAT by passing empty slots; do not rely on EdgeZero initial-navigation telemetry until EdgeZero SSAT is wired, but keep new APIs compatible with that future path.
- `crates/trusted-server-adapter-fastly/src/app.rs`
  - EdgeZero `POST /auction` uses `handle_auction` and should inherit explicit auction telemetry when services/sink are wired there.
- `trusted-server.toml`
  - Needs a new `[tinybird]` section with disabled placeholders and no real credentials.
- `fastly.toml`
  - Dynamic backends do not need static declaration.
  - Local Secret Store fixtures can add placeholder Tinybird token keys if needed for tests/dev.

### Tinybird project

- No existing `tinybird/` directory was found. Plan to create one.

---

## 3. Proposed module/API shape

### 3.1 Core telemetry module

Create `crates/trusted-server-core/src/auction/telemetry.rs` and export it from `auction/mod.rs`.

Recommended core types:

- `AuctionSource`
  - `InitialNavigation`
  - `SpaNavigation`
  - `AuctionApi`
- `AuctionTerminalStatus`
  - `Completed`
  - `ExecutionFailed`
  - `DispatchFailed`
  - `Abandoned`
  - `Skipped`
- `AuctionSkipReason`
  - `ConsentDenied`
  - `AuctionDisabled`
  - `Bot`
  - `Prefetch`
  - `NoProviders`
  - bounded fallback reason
- `TriStateFlag`
  - numeric mapping for Tinybird: `0`, `1`, `2` for false/true/unknown according to the spec fields.
- `AuctionObservationContext`
  - `auction_id: uuid::Uuid` generated with `Uuid::new_v4()`.
  - `auction_source`.
  - `publisher_domain`.
  - normalized `page_path` with no query, no fragment, bounded length, and dynamic-looking segments redacted/bucketed.
  - `country`, `region` from existing geo context.
  - `is_mobile`, `is_known_browser` snapshotted from existing device signals; do not reparse UA.
  - `gdpr_applies` and `consent_present`.
  - `slot_count` and start instant/time metadata needed for elapsed time.
- `AuctionEventRow`
  - serde-serializable row matching `auction_events_raw` schema.
  - Use nullable fields (`Option<T>`) for row-kind-specific columns.
- `AuctionEventBatch`
  - owns `Vec<AuctionEventRow>` and exposes `row_count()` / `to_ndjson(max_body_bytes)`.
  - Serialization appends `\n` after every row.

Recommended pure builder API:

```rust
pub fn build_auction_events(input: AuctionTelemetryInput<'_>) -> AuctionEventBatch;

pub struct AuctionTelemetryInput<'a> {
    pub observation: AuctionObservationContext,
    pub terminal: AuctionTerminalOutcome<'a>,
}

pub enum AuctionTerminalOutcome<'a> {
    Completed {
        request: &'a AuctionRequest,
        result: &'a OrchestrationResult,
    },
    ExecutionFailed {
        request: Option<&'a AuctionRequest>,
        provider_responses: &'a [AuctionResponse],
        reason: &'a str,
        elapsed_ms: u64,
    },
    DispatchFailed {
        request: &'a AuctionRequest,
        provider_responses: &'a [AuctionResponse],
        reason: &'a str,
        elapsed_ms: u64,
    },
    Abandoned {
        request: &'a AuctionRequest,
        provider_responses: &'a [AuctionResponse],
        abandoned_providers: &'a [AbandonedProviderCall],
        reason: &'a str,
        elapsed_ms: u64,
    },
    Skipped {
        reason: &'a str,
        elapsed_ms: u64,
    },
}
```

Keep the builder pure and independently testable. It must not read settings, secrets, environment, backends, or clocks other than values supplied in the input.

### 3.2 Sink abstraction

Add a small core trait so core paths can emit without knowing Tinybird/Fastly details. The lowest-plumbing option is to add it to `RuntimeServices`:

```rust
#[async_trait::async_trait(?Send)]
pub trait AuctionTelemetrySink: Send + Sync {
    async fn emit_auction_events(
        &self,
        services: &RuntimeServices,
        rows: &[AuctionEventRow],
    ) -> Result<(), Report<TrustedServerError>>;
}
```

Add:

- `NoopAuctionTelemetrySink` in core for defaults/tests.
- `RuntimeServicesBuilder::auction_telemetry_sink(...)` with default/noop if omitted, or require it explicitly like existing services.
- `RuntimeServices::auction_telemetry_sink()` accessor.
- A helper such as `emit_auction_events_best_effort(services, batch).await` that catches and logs errors and returns `()`.

The helper is important: call sites should not accidentally propagate telemetry errors.

### 3.3 Fastly Tinybird sink

Create adapter-owned module:

- `crates/trusted-server-adapter-fastly/src/tinybird.rs`

Types/functions:

- `TinybirdConfig` parsed from `Settings.tinybird`.
- `FastlyTinybirdAuctionTelemetrySink` implementing `AuctionTelemetrySink`.
- `build_tinybird_backend_spec(api_host, timeout)` returning `PlatformBackendSpec` with:
  - `scheme = "https"`
  - `host = <tb-api-host>`
  - `host_header_override = None`
  - `certificate_check = true`
  - short `first_byte_timeout` for setup path; the response is not awaited, but backend config still needs sane values.
- `events_api_uri(api_host, dataset)` returning `https://<tb-api-host>/v0/events?name=<dataset>`.
- `authorization_header(token)` that never logs or exposes token values.

Behavior:

1. If `tinybird.enabled = false`, return `Ok(())` without doing work.
2. Convert rows to NDJSON, enforcing `tinybird.max_body_bytes` and a defensive row-count cap.
3. Read append token from Fastly Secret Store via configured store/key.
4. Ensure dynamic backend for `<tb-api-host>`.
5. Build `POST` request with:
   - `Authorization: Bearer <token>`
   - `Content-Type: application/x-ndjson` unless final Tinybird docs/tests prove a different header is required. Current Tinybird Events API docs consume NDJSON by default and examples omit a content type, so `application/x-ndjson` should be treated as an implementation choice to smoke-test, not as an assumption that Tinybird rejects other content types.
   - NDJSON body with trailing newline.
6. Call `services.http_client().send_async(...)`.
7. Drop the returned `PlatformPendingRequest` immediately.
8. Return only setup errors to the best-effort wrapper, which logs and suppresses them.

Do not inspect Tinybird HTTP responses in production emission.

### 3.4 Settings shape

Add a new settings struct in `crates/trusted-server-core/src/settings.rs`:

```toml
[tinybird]
enabled = false
api_host = ""
secret_store = "ts_secrets"
auction_dataset = "auction_events_raw"
auction_token_secret = "tinybird_auction_append_token"
access_enabled = false
access_dataset = "access_logs_raw"
access_token_secret = "tinybird_access_append_token"
access_sample_rate = 0.0
max_body_bytes = 1048576
```

Notes:

- The revised spec lists token secret keys but not a Secret Store name. The implementation needs a store name because `PlatformSecretStore` reads by `StoreName`; use `tinybird.secret_store` with a safe default such as `ts_secrets` unless the user prefers separate per-token store settings.
- Validate only when `enabled` is true; `access_enabled = true` is rejected until Phase 9 wires an access-log emitter, so operators cannot enable a no-op path by mistake:
  - `api_host` must be non-empty, host-only, no scheme, no path, no control characters.
  - dataset names must be non-empty and bounded.
  - secret key names must be non-empty.
  - `access_sample_rate` in `[0.0, 1.0]`.
  - `max_body_bytes` above a small minimum and below a defensive cap.
- Do not commit real hosts or tokens.

---

## 4. Step-by-step implementation phases

### Phase 0: Pre-flight docs/API verification

- [ ] Re-read the revised spec.
- [ ] Check Tinybird Events API docs immediately before implementation:
  - endpoint is still `POST /v0/events?name=<datasource>`;
  - token scope is still `DATASOURCE:APPEND` / datasource APPEND token;
  - NDJSON remains the default event format;
  - request size/rate limits for the selected plan are known;
  - whether Tinybird documents or requires a specific `Content-Type` for NDJSON.
- [ ] Confirm no real customer names, real domains, or credentials are introduced in docs/tests/config.

### Phase 1: Add settings and no-op plumbing

- [ ] Add `TinybirdSettings` to `settings.rs` with defaults and validation.
- [ ] Add `[tinybird]` disabled placeholder block to `trusted-server.toml`.
- [ ] If helpful for local smoke tests, add placeholder local Secret Store entries to `fastly.toml`; keep values obviously fake.
- [ ] Add the core `AuctionTelemetrySink` trait and no-op implementation.
- [ ] Add the sink to `RuntimeServices` and update all builders/test support.
- [ ] Wire `NoopAuctionTelemetrySink` initially in both Fastly service builders so behavior is unchanged.
- [ ] Tests:
  - settings parse/defaults;
  - enabled Tinybird requires `api_host` and token secret key;
  - invalid `api_host` with scheme/path/control characters is rejected;
  - `access_sample_rate` bounds.

### Phase 2: Add pure auction event model and builder

- [ ] Create `auction/telemetry.rs` with row schema structs, enums, and `build_auction_events`.
- [ ] Implement route-safe page path normalization:
  - strip query and fragment;
  - force leading `/`;
  - cap length;
  - redact UUID-like, long numeric, base64/hash-like dynamic segments.
- [ ] Implement provider status mapping:
  - `BidStatus::Success` with bids => `success`;
  - `BidStatus::NoBid` or success with zero parsed bids where provider semantics indicate no-bid => `nobid`;
  - metadata `error_type = launch_failed` => `launch_error`;
  - metadata `error_type = parse_response` => `parse_error`;
  - metadata `error_type = transport` => `transport_error`;
  - explicit timeout/no-response outcomes => `timeout` when available;
  - split uncollected providers => `abandoned`.
- [ ] Implement bid row construction:
  - one row per actual provider-returned bid;
  - no invented seat rows for no-bids/errors;
  - canonical mediator win matching by `(slot_id, bidder, ad_id)`, fallback `(slot_id, bidder)`;
  - exactly one `is_win = 1` per winning slot;
  - mediator-derived winner only when no original provider bid matches.
- [ ] Tests:
  - completed auction with success/no-bid/error providers emits one summary, provider rows, and bid rows;
  - no-bid/error provider rows have no slot/seat values;
  - mediated wins do not double-count;
  - telemetry UUID is fresh and not equal to `AuctionRequest.id` or EC-like values;
  - page path normalization removes query/fragment and redacts dynamic segments;
  - NDJSON serialization emits one valid JSON object per line with no log prefix.

### Phase 3: Retain provider outcomes needed by telemetry

- [ ] Refactor `DispatchedAuction` to carry:
  - cloned `AuctionRequest` already present;
  - observation context once telemetry is wired;
  - launch-failure `AuctionResponse` rows from failed `request_bids` calls;
  - pending backend/provider names still in flight;
  - enough provider metadata to build abandoned provider-call rows without waiting.
- [ ] Replace or extend `dispatch_auction` return type so callers can distinguish:
  - not a candidate / no configured providers;
  - all providers skipped or launch failed (`dispatch_failed`);
  - at least one provider launched (`DispatchedAuction`).
- [ ] Preserve minimal-change compatibility where possible, but do not keep an `Option<DispatchedAuction>` that loses all-failed launch details.
- [ ] Ensure `collect_dispatched_auction` includes launch-failure responses from dispatch token in the final `OrchestrationResult`.
- [ ] Add an `abandon_dispatched_auction(dispatched, reason)` helper that consumes a token without selecting/waiting and returns abandonment telemetry input:
  - one summary with `terminal_status = abandoned`;
  - prior launch-failure provider rows if any;
  - abandoned provider-call rows for every still-pending provider.
- [ ] Tests:
  - split dispatch launch failures are retained through collect;
  - all launch failures produce `dispatch_failed` telemetry data instead of disappearing;
  - abandonment helper emits one abandoned summary and one abandoned provider row per pending provider.

### Phase 4: Instrument explicit `POST /auction`

- [ ] Create `AuctionObservationContext` early after parsing the request body and before consent gating.
- [ ] If consent denies server-side auction, emit `skipped` with reason `consent_denied` and return the existing no-bid response.
- [ ] On `run_auction` success, emit `completed` after result creation and before response conversion.
- [ ] On `run_auction` error, emit `execution_failed` or `dispatch_failed` best-effort before returning the existing error.
- [ ] Do not let telemetry errors change `/auction` status/body.
- [ ] Tests:
  - consent-gated `/auction` emits skipped and does not contact providers;
  - successful `/auction` emits exactly one completed summary;
  - failed `/auction` emits a failure summary and still returns the existing error behavior.

### Phase 5: Instrument SPA `GET /__ts/page-bids`

- [ ] Create an observation only when matched slots exist; no matched slots are not auction candidates.
- [ ] Emit `skipped` for matched-slot requests prevented by:
  - `[auction].enabled = false`;
  - consent denial;
  - bot;
  - prefetch.
- [ ] On `run_auction` success, emit `completed` and use the existing winning-bids response.
- [ ] On `run_auction` error, emit `execution_failed` best-effort and preserve existing behavior of returning empty bids.
- [ ] Tests:
  - same-origin valid page-bids emits completed when auction succeeds;
  - disabled/consent/bot/prefetch matched-slot cases emit skipped;
  - no matched slot emits no auction telemetry;
  - auction failure emits failure telemetry but response remains valid JSON with empty bids.

### Phase 6: Instrument initial-navigation SSAT dispatch/collect/abandon

- [ ] In `handle_publisher_request`, create observation for matched-slot auction candidates before calling dispatch.
- [ ] Emit `skipped` when matched slots exist but policy prevents initiation:
  - auction disabled;
  - consent denied;
  - bot;
  - prefetch.
- [ ] Attach observation to the `DispatchedAuction` token for successful dispatch.
- [ ] If dispatch cannot launch any provider requests, emit `dispatch_failed` and continue origin proxying/injection behavior as today.
- [ ] On origin proxy error after dispatch, consume token with `abandoned` reason such as `origin_proxy_error`, emit, then return the existing proxy error.
- [ ] In `ResponseRoute::PassThrough`, consume token with `abandoned` reason `pass_through_response` before returning `PublisherResponse::PassThrough`.
- [ ] In `ResponseRoute::BufferedUnmodified`, consume token with `abandoned` reason `unmodified_response` or more specific bounded reason (`unsupported_encoding`, `non_success_status`, `empty_request_host`) before returning buffered response.
- [ ] In streaming collect helpers, emit `completed` after `collect_dispatched_auction` returns.
- [ ] In stream read/process error branches before collection, consume token with `abandoned` reason such as `stream_read_error` where possible.
- [ ] Avoid a `Drop`-based async cleanup design; Rust `Drop` cannot `await` or safely perform best-effort async Tinybird emission. Prefer explicit token consumption at every branch.
- [ ] Tests:
  - stream collection emits exactly one completed summary;
  - pass-through after dispatch emits abandoned instead of dropping token;
  - buffered-unmodified after dispatch emits abandoned;
  - origin proxy error after dispatch emits abandoned;
  - unsupported encoding abandoned provider rows are emitted.

### Phase 7: Implement Fastly Tinybird direct sink

- [ ] Add `tinybird.rs` in the Fastly adapter and wire module imports.
- [ ] Build sink from settings in `build_state_from_settings` or service construction.
- [ ] Put the sink into both legacy `build_runtime_services` and EdgeZero `build_per_request_services` paths.
- [ ] Implement Secret Store token lookup with `StoreName::from(settings.tinybird.secret_store.as_str())`.
- [ ] Implement dynamic backend ensure with TLS SNI and Host matching `tinybird.api_host`.
- [ ] Implement NDJSON POST request to `/v0/events?name=auction_events_raw` or configured dataset.
- [ ] Use `send_async` and drop the pending response.
- [ ] Never log token values or the Authorization header.
- [ ] Bound payload size and row count; drop oversize telemetry batch with `log::warn!`.
- [ ] Tests with mock platform services:
  - one auction observation creates one POST;
  - URI path/query is `/v0/events?name=auction_events_raw`;
  - backend spec host is `<tb-api-host>` and scheme is `https`;
  - `Authorization` uses bearer token from secret interface;
  - NDJSON content type is present;
  - response is not awaited (`send_async` called, no `select`/`wait`);
  - missing secret disables/drops emission without panic.

### Phase 8: Add Tinybird project files

Create a new `tinybird/` directory if none exists.

Recommended structure:

```text
tinybird/
  datasources/
    auction_events_raw.datasource
    access_logs_raw.datasource              # optional/config-gated ops phase
    auction_overview_rollup.datasource
    auction_provider_stats_rollup.datasource
    auction_bid_stats_rollup.datasource
  pipes/
    auction_overview_mv.pipe
    auction_provider_stats_mv.pipe
    auction_bid_stats_mv.pipe
    endpoints/
      auction_summary.pipe
      provider_health.pipe
      provider_latency.pipe
      seat_yield.pipe
      ingestion_freshness.pipe
      quarantine_counts.pipe
  fixtures/
    auction_events_raw.ndjson
  tests/
    auction_summary.test
    provider_health.test
    seat_yield.test
```

Datasource requirements:

- `auction_events_raw`
  - append-only landing datasource;
  - `ENGINE MergeTree`;
  - sorting key roughly `(event_date, publisher_domain, event_kind, auction_source, auction_id)`;
  - `event_date = toDate(event_ts)`;
  - `TTL event_date + INTERVAL 30 DAY`;
  - declare scoped token: `TOKEN ts_ingest APPEND`.
- Optional `access_logs_raw`
  - only if ops access telemetry is implemented in the same PR;
  - separate token: `TOKEN ts_access_ingest APPEND`.
- Rollups
  - `AggregatingMergeTree`;
  - retain approximately 13 months;
  - materialized pipes use `*State`, published endpoints use `*Merge`.

Published endpoint requirements:

- parameters for time range, publisher, optional auction source/provider filters;
- sample counts exposed alongside rates/quantiles;
- directional/best-effort dashboard note supported by endpoint data;
- ingestion freshness and quarantine visibility.

Tinybird fixture tests must prove:

- summary rows are not multiplied by provider/bid rows;
- fill rate uses completed summary rows;
- provider no-bid/error rates use provider-call rows;
- seat win rate uses canonical bid rows;
- abandonment rate includes abandoned summaries;
- multiple bids for one auction still count as one auction.

### Phase 9: Optional ops access telemetry

Do this only if requested for a future implementation PR; auction telemetry alone satisfies phase 1. Until this phase is implemented, `tinybird.access_enabled = true` fails validation rather than silently emitting nothing.

- [ ] Add `AccessTelemetrySink` or extend the Tinybird sink with `emit_access_log`.
- [ ] Emit one sampled row at top-level response finalization where status and elapsed time are known.
- [ ] Normalize path to bounded route label, not raw path.
- [ ] Use separate dataset and append token.
- [ ] Respect `tinybird.access_enabled` and `tinybird.access_sample_rate`.
- [ ] Label sampled counts as estimates in endpoints/dashboard docs.

---

## 5. Files likely to change

### Core Rust

- `crates/trusted-server-core/src/auction/mod.rs`
- `crates/trusted-server-core/src/auction/telemetry.rs` (new)
- `crates/trusted-server-core/src/auction/endpoints.rs`
- `crates/trusted-server-core/src/auction/orchestrator.rs`
- `crates/trusted-server-core/src/auction/types.rs` only if shared types need small additions
- `crates/trusted-server-core/src/publisher.rs`
- `crates/trusted-server-core/src/platform/mod.rs`
- `crates/trusted-server-core/src/platform/traits.rs` or new `platform/telemetry.rs`
- `crates/trusted-server-core/src/platform/types.rs`
- `crates/trusted-server-core/src/platform/test_support.rs`
- `crates/trusted-server-core/src/settings.rs`
- `crates/trusted-server-core/Cargo.toml` if new dependency features are needed; prefer existing `uuid`, `serde`, and `serde_json`.

### Fastly adapter

- `crates/trusted-server-adapter-fastly/src/main.rs`
- `crates/trusted-server-adapter-fastly/src/app.rs`
- `crates/trusted-server-adapter-fastly/src/platform.rs`
- `crates/trusted-server-adapter-fastly/src/tinybird.rs` (new)
- `crates/trusted-server-adapter-fastly/src/backend.rs` only if backend helper tests need adjustment; existing SNI/Host behavior likely suffices.
- `crates/trusted-server-adapter-fastly/src/route_tests.rs` if route-level telemetry assertions are placed there.

### Config/docs/Tinybird

- `trusted-server.toml`
- `fastly.toml`
- `tinybird/**` (new project files)
- `docs/superpowers/specs/2026-06-22-auction-prebid-metrics-tinybird-grafana-design.md` only if implementation discovers spec corrections; otherwise leave it.
- Potential Grafana dashboard docs under `docs/` if the implementation includes dashboard JSON/guidance.

---

## 6. Fastly adapter configuration and Secret Store handling

- Dynamic backend:
  - do not add static `fastly.toml` backends for Tinybird;
  - use runtime dynamic backend with `https` scheme and `api_host` as host;
  - TLS SNI must equal `api_host`;
  - outbound `Host` must equal `api_host`.
- Secret Store:
  - Tinybird append token lives in Fastly Secret Store, not config store and not repo files;
  - use a datasource-scoped APPEND token for `auction_events_raw`, declared as `TOKEN ts_ingest APPEND` in Tinybird datasource code or created through the Tinybird UI;
  - do not use Grafana read token or admin token for ingest;
  - recommended config separates `secret_store` from `auction_token_secret` key name;
  - missing store/key or invalid UTF-8 token must drop telemetry and continue.
- Local/dev:
  - keep `tinybird.enabled = false` by default;
  - any local fixture token must be fake and clearly non-production;
  - smoke tests can enable Tinybird through env/config override only.

---

## 7. Tests to add

### Pure builder/unit tests

- Completed auction emits:
  - exactly one summary row;
  - one provider-call row per attempted provider;
  - one bid row per provider-returned bid.
- No-bid/error providers do not invent slot, seat, or bid rows.
- Launch, parse, transport, timeout, no-bid, success, abandoned statuses map correctly.
- Mediated winners produce exactly one canonical winning bid row per slot.
- Telemetry UUID is fresh and independent of `AuctionRequest.id`/EC.
- Privacy fields are absent from serialized rows:
  - no EC ID;
  - no internal request ID;
  - no IP;
  - no raw UA;
  - no full URL/query/fragment.
- Page path normalization is bounded and redacts dynamic segments.
- NDJSON serialization emits valid single-line JSON rows and a trailing newline.

### Lifecycle tests

- `POST /auction`:
  - consent denied => skipped summary;
  - success => completed summary;
  - execution failure => failure summary and unchanged error behavior.
- `GET /__ts/page-bids`:
  - success => completed summary;
  - disabled/consent/bot/prefetch with matched slots => skipped summary;
  - no matched slots => no auction telemetry.
- Initial navigation SSAT:
  - dispatch + collect => completed summary;
  - pass-through response => abandoned summary/provider rows;
  - buffered-unmodified response => abandoned summary/provider rows;
  - origin proxy error after dispatch => abandoned summary/provider rows;
  - dispatch all providers failed => dispatch_failed summary.

### Adapter sink tests

- Enabled sink creates one POST per auction observation.
- URL is `https://<tb-api-host>/v0/events?name=auction_events_raw`.
- Dynamic backend spec uses TLS host and Host header matching `<tb-api-host>`.
- Authorization bearer token is read from the secret interface.
- Token values are not logged or exposed in errors.
- Missing token/store drops emission safely.
- Oversize payload drops emission before send.
- `send_async` is called and response is not awaited.

### Tinybird tests

- Fixture NDJSON covers all row kinds and all auction sources.
- Rollups do not multiply summary counts by provider/bid rows.
- Endpoint tests verify fill, no-bid, error, win, CPM, latency, and abandonment calculations.
- Quarantine/freshness endpoint queries compile.

---

## 8. Validation commands

Run at minimum after implementation:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
cd docs && npm run format
```

If JS/TS files are touched:

```bash
cd crates/trusted-server-js/lib && npx vitest run
cd crates/trusted-server-js/lib && npm run format
```

If Tinybird files are added and the CLI/project supports these commands:

```bash
cd tinybird && tb test
cd tinybird && tb deploy --dry-run
```

Deployment smoke test guidance:

```bash
# Pseudocode: use the real regional host and a scoped APPEND token from Secret Store.
curl \
  -H "Authorization: Bearer <append-token>" \
  -H "Content-Type: application/x-ndjson" \
  --data-binary @tinybird/fixtures/auction_events_raw.ndjson \
  "https://<tb-api-host>/v0/events?name=auction_events_raw"
```

Then verify rows arrive, ingestion freshness updates, and Tinybird quarantine stays empty. This smoke test is required because production fire-and-forget emission does not inspect Tinybird responses.

---

## 9. Risks, gotchas, and open questions

- **Weaker delivery guarantees:** Direct `send_async` ingestion can lose rows when setup fails, the guest exits before background delivery finishes, Tinybird rejects the request, or token/host/plan configuration is wrong. This is accepted only because the data is directional.
- **No customer request failure:** Every call site must use a best-effort helper that logs and suppresses telemetry errors.
- **Content type:** Current Tinybird Events API docs describe NDJSON ingestion and examples omit `Content-Type`; the spec expects `application/x-ndjson`. Use `application/x-ndjson` unless pre-implementation verification or smoke tests show Tinybird requires otherwise.
- **Secret Store name:** The spec names token keys but not the Secret Store name. Add `tinybird.secret_store` or confirm a project-wide default before coding.
- **Split-path abandonment:** Current pass-through/buffered-unmodified/origin-error branches drop dispatched auctions. Missing any branch will undercount abandoned SSAT provider work.
- **Async cleanup:** Do not rely on `Drop` to emit abandonment telemetry; it cannot await. Explicitly consume tokens.
- **Provider outcome fidelity:** Split dispatch currently loses launch failures. Retain these in `DispatchedAuction` or a richer dispatch result before building telemetry.
- **Timeout classification:** Some dropped pending requests are indistinguishable from abandoned unless the select loop observes a timeout/transport error. Do not label unobserved dropped split requests as timeout; use `abandoned`.
- **TTFB/latency:** JSON serialization, Secret Store lookup, backend ensure, and `send_async` setup still cost CPU/runtime time. Keep payload bounded and do not wait for Tinybird.
- **EdgeZero initial navigation:** EdgeZero publisher fallback currently passes no slots and does not run SSAT. Keep telemetry APIs reusable, but do not claim EdgeZero initial-navigation coverage until SSAT is wired there.
- **Tinybird host/token region drift:** A token from one region against another region’s API host returns ingestion errors that production emission will not inspect. Smoke tests must verify host+token together.
- **Schema drift:** Rust row structs and Tinybird datasource columns must be reviewed together. Malformed rows can go to quarantine.
- **Ops telemetry volume:** Access telemetry can create one additional async POST per sampled request. Keep it disabled unless volume and plan limits are known.
- **Financial misuse:** Dashboard/endpoints must label data as directional/best-effort, not billing, reconciliation, payment, or revenue-share data.

---

## 10. Acceptance checklist for later implementation

- [ ] Auction telemetry rows are built by pure core code and covered by unit tests.
- [ ] All three auction sources are represented with correct `auction_source` values.
- [ ] Matched-slot skip paths emit `skipped` summaries with bounded reasons.
- [ ] SSAT pass-through/unmodified/origin-error branches emit `abandoned` instead of dropping dispatch tokens.
- [ ] Direct Tinybird sink uses Fastly Secret Store append token and dynamic backend to regional API host.
- [ ] One auction observation creates at most one NDJSON Events API POST.
- [ ] Tinybird response is not awaited in production emission.
- [ ] Missing token/backend/send setup failures do not affect customer responses.
- [ ] Tinybird datasource/pipe/test files validate rollup math.
- [ ] Validation commands pass.
