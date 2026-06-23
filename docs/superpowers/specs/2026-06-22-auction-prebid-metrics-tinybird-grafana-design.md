# Auction and Prebid metrics to Tinybird and Grafana

Date: 2026-06-22
Status: Design, revised for all-auction lifecycle coverage

## Problem

Trusted Server runs server-side auctions against multiple bid providers
(Prebid Server, APS, mediators) through three initiation paths: initial publisher
navigation using the split `dispatch_auction`/`collect_dispatched_auction` SSAT
flow, SPA navigation using `GET /__ts/page-bids`, and the explicit `POST
/auction` API. Auction internals (bids per seat, provider latency,
win/no-bid/error status, CPM) are computed in `OrchestrationResult`, but the
three paths currently expose them only through scattered plain-text logs such as
[auction/endpoints.rs:266](../../../crates/trusted-server-core/src/auction/endpoints.rs#L266)
and the publisher streaming collection logs.

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
- **The named "Fastly Prometheus exporter" (`fastly/fastly-exporter`) only
  surfaces Fastly's own service stats** (requests, status codes, edge/origin
  latency, bandwidth, cache hit ratio). It cannot see inside the auction. It is
  therefore not the mechanism for auction internals; it is at most an
  alternative ops-only source, which this design does not use.

## Decision

Emit best-effort structured lifecycle rows for every auction decision and
execution from the edge, stream them via Fastly real-time logging to Tinybird's
Events API, aggregate in Tinybird, and render in Grafana. Tinybird is the
always-on stateful aggregator that Compute cannot be. Ops and yield both land in
Tinybird so there is a single analytical store and a single Grafana datasource.

Phase 1 uses **hosted Tinybird Cloud**. The Events API and published pipe
endpoints use the API base URL for the selected Tinybird region; the host must
match the workspace and token region (see Deployment below).

```
edge (all auction paths)
  -> initial navigation: dispatch_auction -> origin race -> collect or abandon
  -> SPA navigation: GET /__ts/page-bids -> run_auction
  -> explicit API: POST /auction -> run_auction
  -> build one summary row plus provider-call and bid rows
  -> sink: buffered non-blocking writes, host flushes async
  -> configured Fastly real-time log endpoint (default "ts_auction_events", NDJSON, batched delivery)
  -> customer-controlled HTTPS relay "<log-relay-host>"
  -> hosted Tinybird Events API  POST https://<tb-api-host>/v0/events?name=auction_events_raw
  -> landing datasource (append-only, 30-day TTL)
  -> materialized views (per-minute rollups)
  -> published pipe endpoints
  -> Grafana Infinity datasource -> published endpoints on <tb-api-host>

edge (every request)
  -> Fastly real-time log endpoint "ts_access_logs" (one access line / request)
  -> customer-controlled HTTPS relay "<log-relay-host>"
  -> hosted Tinybird Events API  POST https://<tb-api-host>/v0/events?name=access_logs_raw
  -> rollups -> endpoints -> Grafana (ops panels)
```

## Resolved decisions

1. **No EC id is emitted.** `ec_id` is omitted from the schema entirely for
   privacy. Page URL is reduced to a bounded, normalized `page_path` with no
   query string, fragment, or dynamic identifier segment. No per-user identifier
   leaves the edge in this pipeline.
2. **Raw retention is 30 days** via TTL on `auction_events_raw` and
   `access_logs_raw`. Per-minute rollups in materialized views are retained 13
   months (adjustable), since they are small.
3. **Phase 1 ships ops and yield together** so the operations team and the
   revenue team both get visibility in the first release.
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

Those safeguards prevent predictable bias without introducing billing-grade
infrastructure. Phase 1 deliberately does **not** provide transactional writes,
exactly-once delivery, deduplication, durable replay, outage backfill, or
cross-system reconciliation. Fastly log delivery and Tinybird ingestion are
best effort, so rows can be missing during delivery or ingestion failures, and
a replayed delivery can create duplicates. The application is designed to emit
one summary per candidate auction, but the destination is not guaranteed to
contain exactly one.

In other words, the calculations should be correct for the rows that arrive,
but the dataset is not guaranteed to contain every row that occurred.

Grafana panels must label the data as directional and show the underlying sample
volume alongside rates or quantiles. Comparisons should use sufficiently large
time windows and the same `auction_source` filter. The dashboard also exposes
ingestion freshness and quarantine counts so users can recognize incomplete or
degraded windows instead of interpreting them as real SSP behavior.

## Deployment: hosted Tinybird Cloud

Phase 1 deploys the Tinybird project to a hosted Tinybird Cloud workspace. A
workspace belongs to one region, and that region has a specific API base URL
(<https://www.tinybird.co/docs/api-reference#regions-and-endpoints>). Use the
workspace's actual API host everywhere below; do not assume `api.tinybird.co`,
because other regions use different hosts.

Implications for this design:

- **Region-specific host.** Ingestion is `POST
https://<tb-api-host>/v0/events?name=...`; published pipe endpoints use the
  same regional API base. The token and host must belong to the same region.
- **Fastly delivery relay.** Fastly's generic HTTPS logging endpoint validates
  control of the destination hostname through
  `/.well-known/fastly/logging/challenge`. Because the hosted Tinybird API
  hostname is not controlled by us, Phase 1 uses a thin HTTPS relay at
  `<log-relay-host>` unless Fastly and Tinybird confirm a supported direct
  integration. The relay answers the validation challenge, normalizes Fastly's
  delivery framing if needed (strip any syslog prefix, guarantee
  newline-delimited JSON), and forwards the batched NDJSON body to the regional
  Tinybird Events API. It performs no analytical transformation and is
  asynchronous from the auction's perspective. It is a single point of failure
  for ingestion only, never on the auction request path.
- **Managed data plane.** Tinybird operates ClickHouse, ingestion, endpoint
  serving, persistence, scaling, and platform observability. There is no EKS
  cluster, gateway ingress, node sizing, storage class, or manual upgrade work
  for this phase.
- **Authentication.** Fastly authenticates to the relay with a dedicated secret.
  The relay holds resource-scoped Tinybird tokens with `DATASOURCE:APPEND` for
  the target datasources. Grafana Infinity uses a read token scoped to the
  published endpoints; neither relay nor ingest credentials are shared with
  Grafana.
- **Development and deployment.** Keep datasources, materializations, endpoints,
  fixtures, tests, and scoped tokens as code. Use Tinybird Cloud Branches for
  validation, then deploy the reviewed project to the main Cloud workspace. The
  branch structure is isolated from production data by default
  (<https://www.tinybird.co/docs/forward/core-concepts/branches>).
- **Plan and limits.** Choose a hosted plan after estimating auction and access
  log volume. Validate Events API request-size and request-rate limits for the
  organization, monitor `413`/`429` responses and ingestion health, and resize
  or upgrade the plan if sustained volume requires it. Fastly's batching keeps
  Events API request count lower than the emitted row count.
- **Failure isolation.** Tinybird remains off the request path. A hosted service
  or ingestion outage can lose analytics for that window, but auctions continue
  unaffected. This is acceptable under the directional data-quality contract.

## Components

### A. Edge event emission (Rust, `trusted-server-core`)

The implementation is split along the existing layering. `trusted-server-core`
owns the observation lifecycle, pure row builder, and sink abstraction. The
Fastly adapter owns `log_fastly` and the named real-time log endpoint.

#### Auction lifecycle coverage

Every candidate auction with matched slots gets an owned
`AuctionObservationContext` before it is executed or gated. It contains a fresh
random telemetry UUID, `auction_source`, normalized page context, coarse geo and
device signals, and consent booleans. It never contains an EC ID, raw user agent,
IP address, or the internal `AuctionRequest.id`.

| `auction_source`     | initiation path                            | terminal observation point                           |
| -------------------- | ------------------------------------------ | ---------------------------------------------------- |
| `initial_navigation` | publisher handler calls `dispatch_auction` | `collect_dispatched_auction` or explicit abandonment |
| `spa_navigation`     | `GET /__ts/page-bids` calls `run_auction`  | `run_auction` success or failure                     |
| `auction_api`        | `POST /auction` calls `run_auction`        | `run_auction` success or failure                     |

Initial-navigation SSAT is split-phase. `dispatch_auction` launches provider
requests with Fastly `send_async`, then the publisher origin request races them.
The returned `DispatchedAuction` must own the observation context alongside its
cloned `AuctionRequest`. On rewritable HTML, the adapter commits response headers
and streams the body; collection occurs at the held `</body>` tail or EOF, where
`collect_dispatched_auction` produces the final result. Emitting the completed
rows there cannot hold TTFB because headers and earlier body chunks have already
been handed to the client.

Not every dispatched SSAT auction reaches collection. If the origin request
fails, or the response is pass-through, non-processable, non-successful, or uses
an unsupported encoding, the provider calls have already consumed quota but no
`OrchestrationResult` is produced. Those branches must consume the
`DispatchedAuction` through an explicit abandonment path and emit one summary
row with `terminal_status = abandoned` plus provider-call rows with `status =
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
not auction candidates, and are represented only in access logs.

The split `dispatch_auction`/`collect_dispatched_auction` path must preserve the
same provider accounting as `run_auction`: launch failures, parse failures,
transport failures, timeouts, no-bids, and successful responses become explicit
provider-call outcomes. Today some split-path failures are only plain-text logs;
the implementation must retain them in the dispatched token or collection
result so telemetry does not silently undercount errors.

#### Builder and sink

- **Builder (core, pure).** `build_auction_events` consumes the owned observation
  context, terminal outcome, optional `AuctionRequest`, provider-call outcomes,
  and optional `OrchestrationResult`. It returns three row kinds: exactly one
  `summary`, zero or more `provider_call` rows, and zero or more `bid` rows.
- **Sink (core abstraction, Fastly implementation).** Core defines a small
  `AuctionEventSink` trait or a method on the existing runtime services object.
  The Fastly adapter writes each serialized row to
  `fastly::log::Endpoint::from_name(settings.auction.telemetry_log_endpoint)`
  with `writeln!`.
  Output is NDJSON with no fern `timestamp LEVEL [module]` prefix. Tests use a
  no-op or in-memory sink so native builds stay clean.

The sink append is a buffered host call. Fastly flushes to the remote endpoint
asynchronously, so there is no synchronous network call from Compute to
Tinybird. JSON serialization and host-call cost still exist and are covered by
the latency success criterion.

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
| `status`                    | LowCardinality(Nullable(String)) | success / nobid / launch_error / parse_error / transport_error / timeout / abandoned |
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
internal auction request ID, full URL, IP, or user-agent string is emitted. Page
paths use the same bounded route-normalization principle as access logs so a
dynamic path segment cannot become a per-user identifier. Geo remains at
country/region granularity.

Device signals note: both `is_mobile` (0/1/2) and the bot-vs-browser bit come
from `derive_device_signals()`, already computed in the adapter before routing
at [main.rs:409](../../../crates/trusted-server-adapter-fastly/src/main.rs#L409)
(`DeviceSignals`, [ec/device.rs](../../../crates/trusted-server-core/src/ec/device.rs)).
Phase 1 snapshots that struct into `AuctionObservationContext`; no UA re-parsing.
`is_known_browser` maps `DeviceSignals.known_browser: Option<bool>` to 1/0/2 and
lets the yield dashboard exclude bot traffic. Tablet is not distinguished.

`price_cpm` note: `Bid.price` is `Option<f64>`, so the design handles decoded
and undecoded prices uniformly. Whether APS per-seat CPM is present depends on
which APS adapter is wired, not on this design:

- The adapter currently in the repo uses the legacy targeting-key flow: it
  leaves `price = None` and stores the encoded `amznbid`/`amznp` in metadata for
  the mediation layer to decode
  ([integrations/aps.rs:407](../../../crates/trusted-server-core/src/integrations/aps.rs#L407),
  locked by `test_aps_bids_have_no_decoded_price`). Under this adapter, raw
  per-seat CPM excludes APS, though the mediated **winner** can still carry a
  decoded CPM via `mediator_response`/`winning_bids`.
- Amazon's newer OpenRTB Prebid Server adapter (`POST
https://web.ads.aps.amazon-adsystem.com/e/pb/bid`) returns a standard decoded
  `seatbid[].bid[].price`. When TS uses that path, APS `price` populates like any
  other bidder and `price_cpm` fills for APS seats with **no schema or pipe
  change**, because both already treat price as nullable.

Either way, null-price bid rows still count as bids. They do not contribute to
CPM quantiles. Provider no-bid rates come from provider-call status, not from
invented seat rows. Label the CPM panel so a window with the legacy adapter is
not read as total-market CPM.

### B. Tinybird (auction yield)

- **Landing datasource** `auction_events_raw`: `ENGINE MergeTree`, sorting key
  `(event_date, publisher_domain, event_kind, auction_source, auction_id)` where
  `event_date = toDate(event_ts)`. Nullable row-kind-specific fields are not used
  in the raw sorting key. `TTL event_date + INTERVAL 30 DAY`.
- **Materialized target datasources** use `AggregatingMergeTree`, retain 13
  months, store `AggregateFunction` columns, and include every dimension in
  their sorting keys. Materialized pipes use `*State` combinators; published
  endpoints read them with the corresponding `*Merge` combinators.
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
warnings, and specific conditions). So this is a new cross-cutting emission in
the adapter, added where the response is finalized in the top-level request flow
([adapter/.../main.rs](../../../crates/trusted-server-adapter-fastly/src/main.rs)),
since `status` and elapsed time are known only there. It writes one line per
request to a second endpoint `ts_access_logs` -> `access_logs_raw`: `event_ts`,
`method`, `path` (normalized to a bounded route label, not the raw path, so
cardinality stays bounded), `status`, `time_elapsed_ms`, `cache_state`
(HIT/MISS/PASS), `country`. Same buffered, non-blocking write as the auction
sink, so no added latency. A per-minute materialized view drives QPS,
status-code class rates, endpoint latency quantiles, and cache hit ratio. This
replaces what `fastly-exporter` would give for ops; the tradeoff (no POP-level
analytics aggregates) was accepted.

### D. Grafana

Grafana Infinity calls the published Tinybird endpoint URLs on
`<tb-api-host>` using a scoped read token stored in Grafana's secure datasource
configuration. This keeps Grafana on the published API contract rather than
granting workspace-wide query access. It drives one dashboard with two rows:

- Ops: QPS, error-rate by status class, p50/p95/p99 endpoint latency, cache hit
  ratio.
- Yield: fill rate, completion/abandonment rate, provider no-bid/error rate, win
  rate and CPM distribution by seat, and provider latency heatmap, filterable by
  publisher, auction source, and provider.

Every rate and quantile panel includes its sample count. The dashboard displays
an explicit "directional, best-effort analytics" notice plus ingestion freshness
and quarantine indicators. It must not present these values as billable totals
or reconciled revenue.

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
- Tinybird pipes: fixture NDJSON containing all three row kinds and auction
  sources, plus `tb test` cases asserting fill/win/no-bid/error/abandonment math
  and quantile endpoints. Fixtures include multiple bids for one auction to
  prove the summary MV counts that auction only once.

## Risks and notes

- **Log volume vs hosted plan.** A completed auction emits one summary row plus P
  provider-call rows and B returned-bid rows. Skipped or failed-before-dispatch
  auctions emit only a summary; abandoned SSAT auctions emit a summary plus
  provider-call rows. At high QPS this multiplies ingest volume into Tinybird.
  The 30-day raw TTL and rollup MVs bound storage. Monitor hosted-plan usage,
  ingestion latency, and `413`/`429` responses, then resize the plan if needed.
  Sampling the ops access-log stream is deferred until measured volume justifies
  it.
- **Lifecycle completeness.** A future auction call site could bypass the
  observation lifecycle. Keep `run_auction`, `dispatch_auction`, and
  `collect_dispatched_auction` instrumentation centralized, require an
  `auction_source` when constructing an observation, and add a test that
  enumerates the known entry paths. Explicit abandonment must replace every
  branch that currently drops a `DispatchedAuction`.
- **Directional accuracy.** Fastly real-time logging, the relay, and Tinybird
  ingestion are best effort. Missing or duplicate rows can distort a short
  interval, especially at low volume. Show sample sizes and ingestion health,
  compare like-for-like sources over longer windows, and do not use the dataset
  for financial reconciliation. Phase 1 accepts this limitation instead of
  adding a queue, replay store, or deduplication layer.
- **Hosted service availability.** A relay or Tinybird outage can lose analytics
  for the affected window. Both remain off the auction request path, so auctions
  continue unaffected.
- **Fastly HTTPS validation and delivery framing (highest integration
  unknown).** Fastly's HTTPS real-time log endpoint batches multiple log lines
  per delivery and can prepend framing depending on the endpoint's
  message-type/format settings.
  The relay must answer Fastly's domain-control challenge and forward a clean
  NDJSON body with the correct `Content-Type` and Tinybird Bearer token. Before
  building dashboards, verify end to end that a batched Fastly delivery lands
  as valid rows: use blank/raw message framing, confirm newline batching, and
  check Tinybird's quarantine table for rejects. A direct Fastly-to-Tinybird
  destination may replace the relay only after its validation and framing are
  proven against the hosted regional endpoint.
- **Schema drift.** The edge NDJSON shape and the Tinybird datasource schema
  must stay in sync. Keep the Rust struct and the `.datasource` definition
  reviewed together; a malformed row is dropped by Tinybird (quarantine), so add
  an ingest-error / quarantine check to the dashboard.
- **Token handling.** Tinybird append tokens are secrets held by the relay, not
  committed to the repo. The separate Fastly-to-relay secret is configured on
  the Fastly log endpoints. The relay must not log either credential or request
  authorization headers.
- **Regional configuration drift.** A token used against the wrong Tinybird API
  host will fail ingestion. Keep `<tb-api-host>` and scoped token configuration
  together and validate both in deployment smoke tests.

## Prerequisites (provisioned outside the repo)

- Hosted Tinybird Cloud workspace in the selected region, with its regional
  `<tb-api-host>` recorded in deployment configuration.
- Two Tinybird `DATASOURCE:APPEND` tokens (or one scoped to both datasources)
  held by the relay, plus a read token scoped to the published endpoints for
  Grafana.
- A customer-controlled, TLS-terminated `<log-relay-host>` that answers Fastly's
  logging challenge and forwards batched NDJSON to the regional Tinybird Events
  API. This prerequisite can be removed only after a direct hosted integration
  is validated.
- Two Fastly real-time log endpoints on the service: the configured auction
  telemetry endpoint (default `ts_auction_events`) and `ts_access_logs`, each
  configured with placement `none`, the appropriate relay
  URL, `Content-Type: application/json`, the relay authentication header,
  newline-delimited JSON, and blank/raw line framing.
- Grafana with the Infinity datasource configured for the published endpoint
  URLs on `<tb-api-host>` and the scoped read token.

## Success criteria

- The application emits one summary for each initial-navigation SSAT, SPA
  page-bids, and `POST /auction` candidate with the correct `auction_source`,
  plus provider-call and returned-bid rows where applicable. Under normal
  healthy delivery those rows appear in `auction_events_raw`; exactly-once
  storage is not required.
- An SSAT auction whose provider requests were dispatched but whose origin
  response cannot be rewritten produces an `abandoned` summary and provider
  rows; it is not silently omitted.
- Provider launch, parse, transport, timeout, no-bid, and success outcomes are
  represented consistently on synchronous and split-phase paths.
- Telemetry auction IDs are fresh random UUIDs unrelated to internal request IDs
  and EC values. No EC ID, full URL, IP, or raw user-agent string is emitted.
- The yield dashboard shows fill rate, completion/abandonment rate, provider
  no-bid/error rate, win rate by seat, CPM quantiles (any seat with a decoded
  price; under the legacy APS adapter that excludes APS raw bids but still
  includes mediated winners), and provider latency for a chosen time range,
  filterable by publisher, auction source, provider, and bot-vs-browser.
- Rate and quantile panels show sample counts and the dashboard clearly labels
  the data as directional and best effort, with ingestion freshness and
  quarantine visibility.
- The ops dashboard shows QPS, status-class error rate, endpoint latency
  quantiles, and cache hit ratio from `access_logs_raw`.
- Tinybird quarantine stays empty under normal traffic; a malformed row is
  visible on the ingest-error panel.
- No measurable change in `/auction`, page-bids, or initial-navigation TTFB
  attributable to emission. Initial-navigation completion emission occurs only
  during post-header auction collection.

## Out of scope

- Alerting rules (Grafana alerts can be added once panels exist).
- Real-time analytics API / `fastly-exporter` integration.
- Per-creative or per-deal analytics beyond seat-level CPM.
- Billing, invoicing, payment, revenue-share calculation, contractual reporting,
  or reconciliation against SSP/ad-server financial records.
- Exactly-once delivery, durable replay, outage backfill, and duplicate removal.
