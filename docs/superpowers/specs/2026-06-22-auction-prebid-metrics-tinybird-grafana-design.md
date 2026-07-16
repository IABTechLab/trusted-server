# Auction and Prebid metrics to Tinybird and Grafana

Date: 2026-06-22
Status: Design, revised for direct Tinybird ingestion from Fastly Compute

## Problem

Trusted Server runs server-side auctions against multiple bid providers
(Prebid Server, APS, mediators) through three initiation paths: initial publisher
navigation using the split `dispatch_auction`/`collect_dispatched_auction` SSAT
flow, SPA navigation using `GET /__ts/page-bids`, and the explicit `POST
/auction` API. Auction internals (bids per seat, provider latency,
win/no-bid/error status, CPM) are computed in `OrchestrationResult`, but the
three paths currently expose them only through scattered plain-text logs.

Operations teams have no structured QPS/error/latency view of Trusted Server,
and the revenue team has no broad, directional fill/win/CPM visibility across
all auction paths. We want both on one dashboard without treating `/auction` as
the only auction entry point.

## Constraints that shape the design

- **Fastly Compute is stateless and ephemeral.** Instances are per-request and
  short-lived. There is no shared process memory to accumulate counters in, so
  an in-process metrics registry plus a `/metrics` scrape endpoint cannot work.
  The aggregation state must live off the edge.
- **No TTFB holds on the hot path.** Nothing in the metrics path may add a
  synchronous network call before the auction response is returned.
- **Fastly real-time logging is no longer the phase-1 transport.** It avoids
  request-path delivery work, but it also requires Fastly logging endpoint setup
  and an RFC 8615 validation relay for hosted Tinybird. Phase 1 favors a simpler
  Fastly Compute `send_async` POST directly to Tinybird's Events API.
- **The named "Fastly Prometheus exporter" (`fastly/fastly-exporter`) only
  surfaces Fastly's own service stats** (requests, status codes, edge/origin
  latency, bandwidth, cache hit ratio). It cannot see inside the auction. It is
  therefore not the mechanism for auction internals; it is at most an
  alternative ops-only source, which this design does not use.

## Decision

Emit best-effort structured lifecycle rows for every auction decision and
execution from the edge, POST them directly from Fastly Compute to Tinybird's
Events API using asynchronous backend requests, aggregate in Tinybird, and render
in Grafana. Tinybird is the always-on stateful aggregator that Compute cannot
be. Auction yield telemetry is the phase-1 requirement. Ops access telemetry can
use the same Tinybird store and Grafana datasource when enabled, but it is
configuration-gated because direct per-request ingest has higher volume impact.

Phase 1 uses **hosted Tinybird Cloud**. The Events API and published pipe
endpoints use the API base URL for the selected Tinybird region; the host must
match the workspace and token region.

```text
edge (all auction paths)
  -> initial navigation: dispatch_auction -> origin race -> collect or abandon
  -> SPA navigation: GET /__ts/page-bids -> run_auction
  -> explicit API: POST /auction -> run_auction
  -> build one summary row plus provider-call and bid rows
  -> direct ingest sink: one bounded NDJSON body per auction observation
  -> Fastly dynamic backend for https://<tb-api-host>
  -> Request::send_async(...) and drop the pending response
  -> hosted Tinybird Events API POST https://<tb-api-host>/v0/events?name=auction_events_raw
  -> landing datasource (append-only, 30-day TTL)
  -> materialized views (per-minute rollups)
  -> published pipe endpoints
  -> Grafana Infinity datasource -> published endpoints on <tb-api-host>

edge (every request, if ops telemetry is enabled)
  -> one access-log row when the response is finalized
  -> direct ingest sink: one small NDJSON body per request, optionally sampled
  -> Fastly dynamic backend for https://<tb-api-host>
  -> Request::send_async(...) and drop the pending response
  -> hosted Tinybird Events API POST https://<tb-api-host>/v0/events?name=access_logs_raw
  -> rollups -> endpoints -> Grafana (ops panels)
```

This direct-ingest design removes the customer-controlled logging relay, Fastly
real-time log endpoints, and Fastly logging domain-control challenge for this
phase. The tradeoff is that delivery is less durable than Fastly real-time
logging: there is no Fastly-managed batching, retry, or response inspection
unless the application deliberately waits, which it must not do on the hot path.

## Resolved decisions

1. **No EC id is emitted.** `ec_id` is omitted from the schema entirely for
   privacy. Page URL is reduced to a bounded, normalized `page_path` with no
   query string, fragment, or dynamic identifier segment. No per-user identifier
   leaves the edge in this pipeline.
2. **Raw retention is 30 days** via TTL on `auction_events_raw` and
   `access_logs_raw`. Per-minute rollups in materialized views are retained 13
   months (adjustable), since they are small.
3. **Phase 1 ships auction yield telemetry.** Auction telemetry is the primary
   requirement. Ops access telemetry is a future phase: its config/schema can be
   reserved, but enabling it must fail closed until an emitter exists so an
   operator cannot turn on a no-op telemetry path by mistake.
4. **Phase 1 uses hosted Tinybird Cloud.** Tinybird manages the data plane,
   scaling, and service availability. See Deployment.
5. **All auction initiation paths are in scope.** Initial-navigation SSAT, SPA
   page-bids, and `POST /auction` share one telemetry model and are identified by
   `auction_source`.
6. **Telemetry auction IDs are independent random UUIDs.** The pipeline never
   copies `AuctionRequest.id`: the SSAT request builder may derive that internal
   ID from the EC value, so using it would violate the no-EC requirement.
7. **This is directional observability, not a system of record.** The data is
   intended to reveal SSP behavior and trends that help publishers make
   optimization decisions. It is not suitable for billing, payment,
   reconciliation, contractual reporting, or revenue-share calculations.
8. **Tinybird ingest credentials live in Fastly Secret Store.** The guest reads a
   dedicated Tinybird append token from Secret Store at runtime and sends it as
   `Authorization: Bearer <token>` on the direct Events API request. The token is
   resource-scoped to APPEND on the relevant datasource only. It is not the
   Grafana read token and is never an admin token. The implementation should keep
   Secret Store access cheap on the hot path by reusing handles or memoized
   configuration where Fastly's runtime permits; if lookup or token retrieval
   fails, telemetry emission is skipped and the customer response continues.

## Data quality and intended use

The dashboard provides best-effort operational and yield insight over meaningful
time windows. It should answer questions such as whether an SSP's no-bid rate is
rising, whether latency differs by provider, and whether fill or CPM trends
change after a publisher configuration adjustment. It is not an event ledger.

The application-side model still preserves low-cost correctness fundamentals:

- Use separate summary, provider-call, and bid grains so auction totals are not
  multiplied by the number of returned bids.
- Observe all three known auction initiation paths so comparisons are not
  accidentally limited to `POST /auction` traffic.
- Represent only facts present in the source data; do not manufacture seat-level
  no-bids from empty provider responses.
- Generate a telemetry UUID independent of EC and internal request identifiers.
- Batch all rows from one auction observation into a single Events API POST so a
  completed auction does not create one backend request per row.

Those safeguards prevent predictable bias without introducing billing-grade
infrastructure. Phase 1 deliberately does **not** provide transactional writes,
exactly-once delivery, deduplication, durable replay, outage backfill, delivery
confirmation, or cross-system reconciliation. The application is designed to
emit one summary per candidate auction, but the destination is not guaranteed to
contain exactly one.

In other words, the calculations should be correct for the rows that arrive, but
the dataset is not guaranteed to contain every row that occurred. Direct
fire-and-forget ingestion can lose rows when the async request cannot be started,
when Fastly terminates background delivery before completion, when Tinybird
rejects the request, or when the token/host/plan is misconfigured. It can also
create duplicates if application code emits a lifecycle twice.

Grafana panels must label the data as directional and show the underlying sample
volume alongside rates or quantiles. Comparisons should use sufficiently large
time windows and the same `auction_source` filter. The dashboard also exposes
ingestion freshness and quarantine counts so users can recognize incomplete or
degraded windows instead of interpreting them as real SSP behavior.

## Deployment: hosted Tinybird Cloud with direct Compute ingest

Phase 1 deploys the Tinybird project to a hosted Tinybird Cloud workspace. A
workspace belongs to one region, and that region has a specific API base URL
(<https://www.tinybird.co/docs/api-reference#regions-and-endpoints>). Use the
workspace's actual API host everywhere below; do not assume `api.tinybird.co`,
because other regions use different hosts.

Implications for this design:

- **Region-specific host.** Ingestion is `POST
https://<tb-api-host>/v0/events?name=...`; published pipe endpoints use the
  same regional API base. The token and host must belong to the same region.
- **Fastly dynamic backend.** The adapter creates or resolves a Fastly backend
  for `https://<tb-api-host>` and sends ordinary Compute backend requests to it.
  TLS SNI and the HTTP `Host` header must match `<tb-api-host>`. If backend
  construction or resolution fails, the adapter drops that telemetry batch and
  continues the customer response. This is not a Fastly real-time logging
  endpoint, so no `/.well-known/fastly/logging/challenge` is required.
- **Fire-and-forget send.** The adapter builds a `fastly::Request`, attaches the
  Tinybird bearer token and NDJSON body, calls `send_async`, and intentionally
  drops the `PendingRequest`. It may log immediate send setup failures, but it
  must not wait for the Tinybird response before returning or streaming the
  customer response.
- **Managed data plane.** Tinybird operates ClickHouse, ingestion, endpoint
  serving, persistence, scaling, and platform observability. There is no EKS
  cluster, gateway ingress, node sizing, storage class, or manual upgrade work
  for this phase.
- **Authentication.** Store Tinybird append tokens in Fastly Secret Store. Use
  resource-scoped tokens generated from the datasource definitions or the
  Tinybird UI: `TOKEN ts_ingest APPEND` on
  `tinybird/datasources/auction_events_raw.datasource` and, if ops telemetry is
  enabled, a separate `TOKEN ts_access_ingest APPEND` on
  `tinybird/datasources/access_logs_raw.datasource`. `tb token create static`
  creates workspace-level static scopes and is not the preferred way to create a
  per-datasource APPEND token. Grafana Infinity uses a separate read token scoped
  to the published endpoints. Deployment smoke tests must prove the configured
  regional host and Secret Store token can ingest fixture NDJSON and leave
  Tinybird quarantine empty.
- **Development and deployment.** Keep datasources, materializations, endpoints,
  fixtures, tests, and scoped token declarations as code. Use Tinybird Cloud
  Branches for validation, then deploy the reviewed project to the main Cloud
  workspace. The branch structure is isolated from production data by default
  (<https://www.tinybird.co/docs/forward/core-concepts/branches>).
- **Plan and limits.** Choose a hosted plan after estimating auction and access
  log volume. Validate Events API request-size and request-rate limits for the
  organization, monitor `413`/`429` responses and ingestion health, and resize
  or upgrade the plan if sustained volume requires it. Direct auction ingest
  batches rows per auction, but direct access-log ingest can add one Events API
  request per incoming request if enabled without sampling.
- **Failure isolation.** Tinybird remains off the request path. A hosted service,
  token, backend, or ingestion outage can lose analytics for that window, but
  auctions continue unaffected. This is acceptable under the directional
  data-quality contract.

## Components

### A. Edge event emission (Rust, `trusted-server-core` and Fastly adapter)

The implementation is split along the existing layering. `trusted-server-core`
owns the observation lifecycle, pure row builder, and sink abstraction. The
Fastly adapter owns Tinybird-specific direct ingest: Secret Store lookup, dynamic
backend construction, and `send_async`.

#### Auction lifecycle coverage

Every candidate auction with matched slots gets an owned
`AuctionObservationContext` before it is executed or gated. It contains a fresh
random telemetry UUID, `auction_source`, normalized page context, coarse geo and
device signals, and consent booleans. It never contains an EC ID, raw user
agent, IP address, or the internal `AuctionRequest.id`.

| `auction_source`     | initiation path                            | terminal observation point                           |
| -------------------- | ------------------------------------------ | ---------------------------------------------------- |
| `initial_navigation` | publisher handler calls `dispatch_auction` | `collect_dispatched_auction` or explicit abandonment |
| `spa_navigation`     | `GET /__ts/page-bids` calls `run_auction`  | `run_auction` success or failure                     |
| `auction_api`        | `POST /auction` calls `run_auction`        | `run_auction` success or failure                     |

Initial-navigation SSAT is split-phase. `dispatch_auction` launches provider
requests with Fastly `send_async`, then the publisher origin request races them.
The returned `DispatchedAuction` must own the observation context alongside its
cloned `AuctionRequest`. On rewritable HTML, the adapter commits response
headers and streams the body; collection occurs at the held `</body>` tail or
EOF, where `collect_dispatched_auction` produces the final result. Emitting the
completed rows there cannot hold TTFB because headers and earlier body chunks
have already been handed to the client. The Tinybird POST still must be
`send_async` and not awaited.

Not every dispatched SSAT auction reaches collection. If the origin request
fails, or the response is pass-through, non-processable, non-successful, or uses
an unsupported encoding, the provider calls have already consumed quota but no
`OrchestrationResult` is produced. Those branches must consume the
`DispatchedAuction` through an explicit abandonment path and emit one summary row
with `terminal_status = abandoned` plus provider-call rows with `status =
abandoned`. The token must not be silently dropped.

Within one successful Compute execution, the lifecycle is designed to emit
exactly one summary row per candidate auction. This is an application invariant,
not an exactly-once delivery guarantee:

- `completed` for an `OrchestrationResult`, including a valid zero-bid result.
- `execution_failed` when synchronous orchestration fails.
- `dispatch_failed` when no provider request could be launched.
- `abandoned` when split-phase SSAT launched providers but cannot collect them.
- `skipped` when matched slots exist but policy prevents initiation;
  `terminal_reason` distinguishes consent, bot, prefetch, and disabled cases.

Requests with no matching creative-opportunity slots are ordinary page traffic,
not auction candidates, and are represented only in access logs when ops
telemetry is enabled.

The split `dispatch_auction`/`collect_dispatched_auction` path must preserve the
same provider accounting as `run_auction`: launch failures, parse failures,
transport failures, timeouts, no-bids, and successful responses become explicit
provider-call outcomes. Today some split-path failures are only plain-text logs;
the implementation must retain them in the dispatched token or collection result
so telemetry does not silently undercount errors.

#### Builder and direct-ingest sink

- **Builder (core, pure).** `build_auction_events` consumes the owned observation
  context, terminal outcome, optional `AuctionRequest`, provider-call outcomes,
  and optional `OrchestrationResult`. It returns three row kinds: exactly one
  `summary`, zero or more `provider_call` rows, and zero or more `bid` rows.
- **Sink abstraction (core).** Core defines a small `AuctionEventSink` trait or a
  method on the existing runtime services object. Tests use a no-op or in-memory
  sink so native builds stay clean.
- **Fastly direct-ingest implementation (adapter).** The adapter serializes all
  rows from one auction observation as newline-delimited JSON, creates a POST to
  `/v0/events?name=auction_events_raw` on the configured Tinybird regional host,
  adds `Authorization: Bearer <secret-store-token>` and the Tinybird-documented
  NDJSON `Content-Type` (`application/x-ndjson` unless Tinybird's Events API
  requires otherwise), sends via `send_async`, and ignores the response.

Output is NDJSON with no fern `timestamp LEVEL [module]` prefix. JSON
serialization and host-call setup cost still exist and are covered by the
latency success criterion. The direct sink should bound payload size and row
count defensively; if a payload would exceed a configured maximum, drop the
telemetry batch and log a warning rather than holding the response path.

Bid rows are emitted for actual provider-returned bids. Provider-level no-bid
and error information belongs on `provider_call` rows because an empty
`AuctionResponse` has no seat or slot identity. When mediation is configured,
the mediator gets its own provider-call row, but its bids are not emitted again
when they can be matched to an original provider bid. Each `winning_bids` entry
is matched to at most one provider bid on `(slot_id, bidder, ad_id)`, falling
back to `(slot_id, bidder)` when `ad_id` is absent. A matched row receives
`is_win = 1` and, when its raw price is null, the mediator's decoded winning
price. If no original bid can be matched, emit one mediator-derived canonical
winner row. Never mark both an original and mediator copy as the same win.

Emission is not gated by consent. `gdpr_applies` comes from
`ConsentContext.gdpr_applies`; `consent_present` is `!ConsentContext::is_empty()`
because an `EcContext` always contains a consent context, even when no signal was
supplied.

#### `auction_events_raw` row schema

All row kinds share these columns:

| field              | type                   | notes                                             |
| ------------------ | ---------------------- | ------------------------------------------------- |
| `event_ts`         | DateTime64(3)          | terminal observation time, UTC                    |
| `event_kind`       | LowCardinality(String) | summary / provider_call / bid                     |
| `auction_id`       | UUID                   | fresh telemetry UUID; never `AuctionRequest.id`   |
| `auction_source`   | LowCardinality(String) | initial_navigation / spa_navigation / auction_api |
| `publisher_domain` | String                 |                                                   |
| `page_path`        | String                 | bounded normalized route; no query or fragment    |
| `country`          | LowCardinality(String) | coarse geo                                        |
| `region`           | Nullable(String)       | coarse geo                                        |
| `is_mobile`        | UInt8                  | 0=desktop, 1=mobile, 2=unknown                    |
| `is_known_browser` | UInt8                  | 0=bot, 1=browser, 2=unknown                       |
| `gdpr_applies`     | UInt8                  | 0/1                                               |
| `consent_present`  | UInt8                  | 0/1                                               |

Fields that do not apply to a row kind are null. Summary-only fields:

| field               | type                             | notes                                                                          |
| ------------------- | -------------------------------- | ------------------------------------------------------------------------------ |
| `terminal_status`   | LowCardinality(Nullable(String)) | `completed` / `execution_failed` / `dispatch_failed` / `abandoned` / `skipped` |
| `terminal_reason`   | LowCardinality(Nullable(String)) | bounded machine-readable reason                                                |
| `slot_count`        | Nullable(UInt16)                 | requested slots                                                                |
| `total_time_ms`     | Nullable(UInt32)                 | elapsed until completion or abandonment                                        |
| `winning_bid_count` | Nullable(UInt16)                 | zero for non-completed outcomes                                                |

Provider-call fields:

| field                       | type                             | notes                                                                                |
| --------------------------- | -------------------------------- | ------------------------------------------------------------------------------------ |
| `provider`                  | LowCardinality(Nullable(String)) | prebid / aps / mediator name                                                         |
| `provider_role`             | LowCardinality(Nullable(String)) | bidder / mediator                                                                    |
| `status`                    | LowCardinality(Nullable(String)) | success / nobid / launch_error / parse_error / transport_error / http_status_error / timeout / abandoned |
| `provider_response_time_ms` | Nullable(UInt32)                 | provider-call latency; null if unavailable                                           |
| `provider_bid_count`        | Nullable(UInt16)                 | number of parsed bids                                                                |

Bid fields:

| field        | type                             | notes                                                    |
| ------------ | -------------------------------- | -------------------------------------------------------- |
| `slot_id`    | Nullable(String)                 |                                                          |
| `slot_w`     | Nullable(UInt16)                 | returned creative width                                  |
| `slot_h`     | Nullable(UInt16)                 | returned creative height                                 |
| `media_type` | LowCardinality(Nullable(String)) | banner / video / native                                  |
| `provider`   | LowCardinality(Nullable(String)) | originating provider; mediator only for unmatched winner |
| `seat`       | LowCardinality(Nullable(String)) | bidder/seat name                                         |
| `price_cpm`  | Nullable(Float64)                | null when the provider has no decoded price              |
| `currency`   | LowCardinality(Nullable(String)) |                                                          |
| `is_win`     | Nullable(UInt8)                  | one canonical winning row per slot                       |
| `ad_domain`  | Nullable(String)                 | advertiser domain, optional                              |
| `ad_id`      | Nullable(String)                 | creative ID, optional                                    |

Privacy note: `auction_id` is generated independently for telemetry. No EC ID,
internal auction request ID, full URL, IP, or raw user-agent string is emitted.
Page paths use the same bounded route-normalization principle as access logs so
a dynamic path segment cannot become a per-user identifier. Geo remains at
country/region granularity.

Device signals note: both `is_mobile` (0/1/2) and the bot-vs-browser bit come
from the adapter's already-derived device signals. Phase 1 snapshots that struct
into `AuctionObservationContext`; no UA re-parsing. `is_known_browser` maps
`known_browser: Option<bool>` to 1/0/2 and lets the yield dashboard exclude bot
traffic. Tablet is not distinguished.

`price_cpm` note: `Bid.price` is `Option<f64>`, so the design handles decoded
and undecoded prices uniformly. Null-price bid rows still count as bids. They do
not contribute to CPM quantiles. Provider no-bid rates come from provider-call
status, not from invented seat rows. Label the CPM panel so a window with the
legacy APS adapter is not read as total-market CPM.

### B. Tinybird (auction yield)

- **Landing datasource** `auction_events_raw`: `ENGINE MergeTree`, sorting key
  `(event_date, publisher_domain, event_kind, auction_source, auction_id)` where
  `event_date = toDate(event_ts)`. Nullable row-kind-specific fields are not used
  in the raw sorting key. `TTL event_date + INTERVAL 30 DAY`. Declare the
  resource-scoped append token in this datasource as `TOKEN ts_ingest APPEND`.
- **Materialized target datasources** use `AggregatingMergeTree`, retain 13
  months, store `AggregateFunction` columns, and include every dimension in their
  sorting keys. Materialized pipes use `*State` combinators; published endpoints
  read them with the corresponding `*Merge` combinators.
- **Materialized view** `auction_overview_mv`: filters `summary` event rows. It
  aggregates per `(minute, publisher_domain, auction_source, terminal_status,
terminal_reason)`. Because there is exactly one summary row per auction,
  auctions, requested slots, winning bids, completion/abandonment rates, and
  `quantilesState` of `total_time_ms` are not multiplied by the number of
  provider or bid rows.
- **Materialized view** `auction_provider_stats_mv`: filters `provider_call`
  event rows. It aggregates per `(minute, publisher_domain, auction_source,
provider, provider_role)` requests, successes, nobids, errors, timeouts,
  abandonments, parsed bids, and `quantilesState` of
  `provider_response_time_ms`.
- **Materialized view** `auction_bid_stats_mv`: filters `bid` event rows. It
  aggregates per `(minute, publisher_domain, auction_source, provider, seat)`
  bids, wins, and `quantilesState` of decoded `price_cpm` over winning bids.
- **Published pipe endpoints** parametrized by time range, publisher, and
  optional source/provider filters: an auction-summary endpoint, a
  provider-health endpoint, a seat-yield endpoint, and a provider-latency
  endpoint.

Definitions stay in the published pipes:

- Fill rate = completed-auction winning slots / completed-auction requested
  slots, from summary rows.
- Provider no-bid rate = provider calls with `status = nobid` / all
  non-abandoned provider calls.
- Provider error rate = error and timeout provider calls / all provider calls.
- Seat win rate = canonical winning bid rows / returned bid rows for that seat.
- Abandonment rate = abandoned summary rows / summary rows that reached an
  execution attempt (`completed`, `execution_failed`, `dispatch_failed`, or
  `abandoned`).

Seat-level no-bid rate is deliberately not claimed: an empty provider response
does not identify which stored-request seats failed to bid. If future provider
adapters expose attempted-seat outcomes, add a fourth `opportunity` row kind
rather than manufacturing seat identities from an empty response.

### C. Tinybird (ops)

No per-request access logging exists today (the adapter logs only errors,
warnings, and specific conditions). If enabled, the adapter emits a new
cross-cutting access row where the response is finalized in the top-level request
flow, since `status` and elapsed time are known only there. It writes one line
per sampled request to `access_logs_raw`: `event_ts`, `method`, `path`
(normalized to a bounded route label, not the raw path, so cardinality stays
bounded), `status`, `time_elapsed_ms`, `cache_state` (HIT/MISS/PASS), and
`country`.

Unlike Fastly real-time logging, direct access telemetry creates an additional
asynchronous backend POST for each sampled request. This is acceptable only when
configured volume and Tinybird plan limits are understood. The implementation
therefore exposes configuration for enablement and sample rate. When enabled at
100%, the data drives QPS, status-code class rates, endpoint latency quantiles,
and cache hit ratio. When sampled, ops panels must account for the sample rate
or label counts as sampled estimates.

Declare a separate resource-scoped append token in
`access_logs_raw.datasource`, for example `TOKEN ts_access_ingest APPEND`, and
store its value in Fastly Secret Store separately from `ts_ingest`.

### D. Grafana

Grafana Infinity calls the published Tinybird endpoint URLs on `<tb-api-host>`
using a scoped read token stored in Grafana's secure datasource configuration.
This keeps Grafana on the published API contract rather than granting
workspace-wide query access. It drives one dashboard with two rows:

- Ops: QPS, error-rate by status class, p50/p95/p99 endpoint latency, cache hit
  ratio. If access telemetry is sampled, panels are labeled accordingly.
- Yield: fill rate, completion/abandonment rate, provider no-bid/error rate, win
  rate and CPM distribution by seat, and provider latency heatmap, filterable by
  publisher, auction source, and provider.

Every rate and quantile panel includes its sample count. The dashboard displays
an explicit "directional, best-effort analytics" notice plus ingestion freshness
and quarantine indicators. It must not present these values as billable totals or
reconciled revenue.

## Configuration

The Fastly adapter needs runtime configuration for direct Tinybird ingestion:

| setting                         | purpose                                                                             |
| ------------------------------- | ----------------------------------------------------------------------------------- |
| `tinybird.enabled`              | master enable/disable switch                                                        |
| `tinybird.api_host`             | regional Tinybird API host, without path                                            |
| `tinybird.auction_dataset`      | usually `auction_events_raw`                                                        |
| `tinybird.auction_token_secret` | Fastly Secret Store key containing the `ts_ingest` token                            |
| `tinybird.access_enabled`       | reserved for future direct access-log telemetry; rejected while no emitter is wired |
| `tinybird.access_dataset`       | future access-log datasource, usually `access_logs_raw`                             |
| `tinybird.access_token_secret`  | future Fastly Secret Store key for the access append token                          |
| `tinybird.access_sample_rate`   | future fraction of requests to emit for ops telemetry                               |
| `tinybird.max_body_bytes`       | defensive maximum body size for one direct ingest POST                              |

The adapter must not log token values or request `Authorization` headers. Local
and test environments may use disabled/no-op sinks or fixture secrets.

## Testing

- `build_auction_events` is pure and unit-tested with Arrange-Act-Assert. A
  completed result with successful, no-bid, and errored providers produces
  exactly one summary row, one provider-call row per attempted provider, and one
  bid row per returned bid. Assert that no-bid/error provider rows do not invent
  seat or slot values and that mediated wins produce exactly one `is_win = 1`
  bid row per winning slot.
- Lifecycle tests cover all three sources: `POST /auction`, SPA page-bids, and
  initial-navigation SSAT dispatch/collect. Each produces exactly one terminal
  summary with the correct `auction_source`.
- SSAT route tests cover origin failure, pass-through content,
  buffered-unmodified content, and unsupported encoding after successful
  dispatch. Each consumes the dispatch token and emits one `abandoned` summary
  plus abandoned provider-call rows instead of silently dropping the token.
- Failure tests cover provider launch, parse, transport, and timeout outcomes on
  both `run_auction` and split dispatch/collect paths.
- Privacy tests assert that the telemetry `auction_id` is a fresh UUID and never
  equals or contains the internal `AuctionRequest.id` or EC value. Page paths are
  normalized and bounded before serialization.
- NDJSON serialization test: each row serializes to a single line of valid JSON
  with the expected keys and no log-formatter prefix.
- Direct-ingest sink tests use a mock Fastly adapter seam where possible to prove
  one auction observation creates one POST to
  `/v0/events?name=auction_events_raw`, includes the Tinybird-documented NDJSON
  content type, includes a bearer token sourced through the secret interface, and
  does not wait for a response.
- Token handling tests assert the ingest sink disables emission safely when the
  configured secret is missing, without panicking or blocking the customer
  response path.
- Tinybird pipes: fixture NDJSON containing all three row kinds and auction
  sources, plus `tb test` cases asserting fill/win/no-bid/error/abandonment math
  and quantile endpoints. Fixtures include multiple bids for one auction to
  prove the summary MV counts that auction only once.

## Risks and notes

- **Direct ingest loses Fastly logging durability.** `send_async` can begin a
  backend request and let it continue after the guest exits, but the application
  does not receive Tinybird's response when it drops the pending request. Failed
  auth, `429`, `413`, `5xx`, or quarantine outcomes are visible only through
  Tinybird observability, freshness panels, and smoke tests. This is acceptable
  only because the dataset is directional.
- **Request volume vs hosted plan.** A completed auction emits one summary row
  plus P provider-call rows and B returned-bid rows, batched into one Tinybird
  POST. Access telemetry, if enabled, can add one Tinybird POST per sampled
  request. Monitor hosted-plan usage, ingestion latency, request-rate limits,
  request-size limits, and quarantine counts. Resize the plan or lower the
  access sample rate if sustained volume requires it.
- **Lifecycle completeness.** A future auction call site could bypass the
  observation lifecycle. Keep `run_auction`, `dispatch_auction`, and
  `collect_dispatched_auction` instrumentation centralized, require an
  `auction_source` when constructing an observation, and add a test that
  enumerates the known entry paths. Explicit abandonment must replace every
  branch that currently drops a `DispatchedAuction`.
- **Directional accuracy.** Direct fire-and-forget ingestion and Tinybird
  ingestion are best effort. Missing or duplicate rows can distort a short
  interval, especially at low volume. Show sample sizes and ingestion health,
  compare like-for-like sources over longer windows, and do not use the dataset
  for financial reconciliation. Phase 1 accepts this limitation instead of
  adding a queue, replay store, or deduplication layer.
- **Hosted service availability.** A Tinybird, backend, DNS, or secret
  configuration outage can lose analytics for the affected window. Tinybird
  remains off the auction request path, so auctions continue unaffected.
- **Schema drift.** The edge NDJSON shape and the Tinybird datasource schema must
  stay in sync. Keep the Rust struct and the `.datasource` definition reviewed
  together; a malformed row is dropped by Tinybird (quarantine), so add an
  ingest-error / quarantine check to the dashboard.
- **Token handling.** Tinybird append tokens are secrets held in Fastly Secret
  Store, not committed to the repo. The adapter must not log credentials or
  request authorization headers. The token should be limited to APPEND on a
  single datasource.
- **Regional configuration drift.** A token used against the wrong Tinybird API
  host will fail ingestion. Keep `<tb-api-host>` and scoped token configuration
  together and validate both in deployment smoke tests.
- **Content type drift.** The implementation should verify Tinybird's current
  Events API requirements before coding the final header. The intended payload is
  NDJSON; use `application/x-ndjson` unless the documented Events API requires a
  different content type for newline-delimited JSON.

## Prerequisites (provisioned outside the repo)

- Hosted Tinybird Cloud workspace in the selected region, with its regional
  `<tb-api-host>` recorded in deployment configuration.
- `TOKEN ts_ingest APPEND` declared on
  `tinybird/datasources/auction_events_raw.datasource`; after `tb deploy`, copy
  the generated token value from `tb --cloud token ls` or the Tinybird UI into
  Fastly Secret Store.
- If ops access telemetry is enabled, `TOKEN ts_access_ingest APPEND` declared on
  `tinybird/datasources/access_logs_raw.datasource`; copy that token value into
  Fastly Secret Store separately.
- A Fastly service configuration that allows a dynamic backend request to the
  Tinybird regional API host, with TLS SNI and `Host` matching that host.
- A deployment smoke test that sends fixture NDJSON through the configured
  Fastly/Tinybird settings and verifies rows arrive in the target datasource with
  no quarantine rejects.
- Grafana with the Infinity datasource configured for the published endpoint URLs
  on `<tb-api-host>` and the scoped read token.

The previous relay prerequisite is intentionally removed. A customer-controlled
`<log-relay-host>` and Fastly real-time logging endpoints are not required for
the direct-ingest architecture.

## Success criteria

- The application emits one summary for each initial-navigation SSAT, SPA
  page-bids, and `POST /auction` candidate with the correct `auction_source`,
  plus provider-call and returned-bid rows where applicable. Under normal
  healthy direct ingestion those rows appear in `auction_events_raw`;
  exactly-once storage is not required.
- An SSAT auction whose provider requests were dispatched but whose origin
  response cannot be rewritten produces an `abandoned` summary and provider rows;
  it is not silently omitted.
- Provider launch, parse, transport, timeout, no-bid, and success outcomes are
  represented consistently on synchronous and split-phase paths.
- Telemetry auction IDs are fresh random UUIDs unrelated to internal request IDs
  and EC values. No EC ID, full URL, IP, or raw user-agent string is emitted.
- The Fastly adapter sends auction rows to Tinybird through a direct asynchronous
  backend POST using a Secret Store token scoped to APPEND on
  `auction_events_raw`; it does not configure or require Fastly real-time
  logging, a relay host, or an RFC 8615 logging challenge.
- The adapter batches all rows for one auction observation into one NDJSON Events
  API request and does not wait for the Tinybird response before returning or
  streaming the customer response.
- If access telemetry is enabled, the ops dashboard shows QPS, status-class error
  rate, endpoint latency quantiles, and cache hit ratio from `access_logs_raw`,
  with sampling clearly labeled when the sample rate is below 100%.
- The yield dashboard shows fill rate, completion/abandonment rate, provider
  no-bid/error rate, win rate by seat, CPM quantiles, and provider latency for a
  chosen time range, filterable by publisher, auction source, provider, and
  bot-vs-browser.
- Rate and quantile panels show sample counts and the dashboard clearly labels
  the data as directional and best effort, with ingestion freshness and
  quarantine visibility.
- Tinybird quarantine stays empty under normal traffic; a malformed row is
  visible on the ingest-error panel.
- No measurable change in `/auction`, page-bids, or initial-navigation TTFB
  attributable to emission. Initial-navigation completion emission occurs only
  during post-header auction collection.

## Out of scope

- Alerting rules (Grafana alerts can be added once panels exist).
- Real-time analytics API / `fastly-exporter` integration.
- Fastly real-time logging and relay setup for auction telemetry.
- Per-creative or per-deal analytics beyond seat-level CPM.
- Billing, invoicing, payment, revenue-share calculation, contractual reporting,
  or reconciliation against SSP/ad-server financial records.
- Exactly-once delivery, durable replay, outage backfill, delivery confirmation,
  and duplicate removal.
