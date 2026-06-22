# Auction and Prebid metrics to Tinybird and Grafana

Date: 2026-06-22
Status: Design, pending implementation plan

## Problem

Trusted Server runs server-side auctions against multiple bid providers
(Prebid Server, APS, mediators). All auction internals (bids per seat,
per-provider latency, win/no-bid/error status, CPM) are computed in the
`OrchestrationResult` but only ever rendered as plain-text log strings at
[auction/endpoints.rs:266](../../../crates/trusted-server-core/src/auction/endpoints.rs#L266).
Operations teams have no QPS/error/latency view of the `/auction` endpoint, and
the revenue team has no fill/win/CPM visibility. We want both on one dashboard.

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

Emit one structured log event per auction outcome from the edge, stream it via
Fastly real-time logging to Tinybird's Events API, aggregate in Tinybird, and
render in Grafana. Tinybird is the always-on stateful aggregator that Compute
cannot be. Ops and yield both land in Tinybird so there is a single store and a
single Grafana datasource.

```
edge (handle_auction)
  -> build event rows from OrchestrationResult (pure fn, off hot path)
  -> Fastly real-time log endpoint "ts_auction_events" (NDJSON, async batched)
  -> Tinybird Events API  POST /v0/events?name=auction_events_raw
  -> landing datasource (append-only, 30-day TTL)
  -> materialized views (per-minute rollups)
  -> published pipe endpoints
  -> Grafana (Tinybird datasource)

edge (every request)
  -> Fastly real-time log endpoint "ts_access_logs" (one access line / request)
  -> Tinybird Events API  POST /v0/events?name=access_logs_raw
  -> rollups -> endpoints -> Grafana (ops panels)
```

## Resolved decisions

1. **No EC id is emitted.** `ec_id` is omitted from the schema entirely for
   privacy. Page URL is reduced to `page_path` (no query string). No per-user
   identifier leaves the edge in this pipeline.
2. **Raw retention is 30 days** via TTL on `auction_events_raw` and
   `access_logs_raw`. Per-minute rollups in materialized views may be retained
   longer.
3. **Phase 1 ships ops and yield together** so the operations team and the
   revenue team both get visibility in the first release.

## Components

### A. Edge event emission (Rust, `trusted-server-core`)

A pure function `build_auction_events(result: &OrchestrationResult, ctx: ...) ->
Vec<AuctionEvent>` converts orchestration output into rows at the grain **one
row per (auction x provider x seat-response)**. A provider that returns no bid
emits exactly one row with `status = nobid` and a null `price_cpm`. A provider
error emits one row with `status = error`. Each row carries the auction-level
fields denormalized.

Serialization writes NDJSON (one JSON object per line) directly to a dedicated
Fastly log endpoint via `fastly::log::Endpoint::from_name("ts_auction_events")`
and `writeln!`. It deliberately bypasses the `log`/fern text formatter used for
`tslog` so the stream is clean JSON with no `timestamp LEVEL [module]` prefix.
Fastly real-time logging batches and POSTs asynchronously after the response is
sent, so there is no hot-path cost.

Emission happens regardless of consent state (the rows contain no PII). Consent
flags are recorded as booleans for analysis, not used to gate emission.

#### `auction_events_raw` row schema

Auction-level (denormalized onto every row):

| field                | type      | notes                                  |
| -------------------- | --------- | -------------------------------------- |
| `event_ts`           | DateTime  | auction completion time (UTC)          |
| `auction_id`         | String    | UUID, drill-down only                  |
| `publisher_domain`   | String    |                                        |
| `page_path`          | String    | path only, no query string             |
| `country`            | String    | from geo lookup                        |
| `region`             | String    | from geo lookup                        |
| `device_type`        | String    | derived from UA/signals                |
| `gdpr_applies`       | UInt8     | 0/1                                    |
| `consent_present`    | UInt8     | 0/1                                    |
| `slot_count`         | UInt16    |                                        |
| `total_time_ms`      | UInt32    | orchestration wall time                |
| `winning_bid_count`  | UInt16    |                                        |

Per seat-response:

| field                       | type      | notes                              |
| --------------------------- | --------- | ---------------------------------- |
| `slot_id`                   | String    |                                    |
| `slot_w`                    | UInt16    |                                    |
| `slot_h`                    | UInt16    |                                    |
| `media_type`                | String    | banner/video/native                |
| `provider`                  | String    | prebid/aps/mediator                |
| `seat`                      | String    | bidder/seat name                   |
| `status`                    | String    | bid / nobid / error                |
| `price_cpm`                 | Float64   | null for nobid/error               |
| `currency`                  | String    |                                    |
| `provider_response_time_ms` | UInt32    | per-provider latency               |
| `is_win`                    | UInt8     | 1 if this bid is a winning bid     |
| `ad_domain`                 | String    | advertiser domain, optional        |
| `ad_id`                     | String    | creative id, optional              |

Privacy note: no `ec_id`, no full URL, no IP, no user agent string. Geo is kept
at country/region granularity only.

### B. Tinybird (auction yield)

- **Landing datasource** `auction_events_raw`: `ENGINE MergeTree`, sorting key
  `(event_date, publisher_domain, provider, seat)` where `event_date =
  toDate(event_ts)`. `TTL event_date + INTERVAL 30 DAY`.
- **Materialized view** `auction_provider_stats_mv`: per
  `(minute, publisher_domain, provider, seat)` aggregate requests, bids, nobids,
  errors, wins, `quantilesState` of `provider_response_time_ms`, and
  `quantilesState` of `price_cpm` over winning bids. Longer retention than raw.
- **Materialized view** `auction_overview_mv`: per `(minute, publisher_domain)`
  aggregate auctions, slots, winning bids, and `quantilesState` of
  `total_time_ms`.
- **Published pipe endpoints** parametrized by time range, publisher, and
  provider: a yield-summary endpoint (fill rate, win rate by seat, no-bid rate,
  CPM quantiles) and a latency endpoint (per-provider/seat quantiles).

Fill rate, win rate, and no-bid rate are computed in pipes from the counts, not
stored, so definitions stay in one place.

### C. Tinybird (ops)

A second Fastly real-time log endpoint `ts_access_logs` emits one access line
per request to `access_logs_raw`: `event_ts`, `method`, `path` (normalized to a
route label so cardinality stays bounded), `status`, `time_elapsed_ms`,
`cache_state` (HIT/MISS/PASS from `fastly_info.state`), `country`. A per-minute
materialized view drives QPS, status-code class rates, endpoint latency
quantiles, and cache hit ratio. This replaces what `fastly-exporter` would give
for ops; the tradeoff (no POP-level analytics aggregates) was accepted.

### D. Grafana

Tinybird Grafana datasource (or grafana-infinity against the published endpoint
URLs with a read token) drives one dashboard with two rows:

- Ops: QPS, error-rate by status class, p50/p95/p99 endpoint latency, cache hit
  ratio.
- Yield: fill rate, win rate by seat, no-bid rate, CPM distribution, per-seat
  latency heatmap, filterable by publisher and provider.

## Testing

- `build_auction_events` is pure and unit-tested with Arrange-Act-Assert: given
  an `OrchestrationResult` with a winning provider, a no-bid provider, and an
  errored provider, assert one row per seat, a no-bid row present with null
  price, correct `is_win` flags, and that the auction-level fields are
  identical across all rows.
- NDJSON serialization test: each row serializes to a single line of valid JSON
  with the expected keys and no log-formatter prefix.
- Tinybird pipes: fixture NDJSON plus `tb test` cases asserting fill/win/no-bid
  math and quantile endpoints.

## Risks and notes

- **Log volume.** Grain is N rows per auction (N = providers x responding
  seats). At high QPS this multiplies ingest volume into Tinybird. The 30-day
  raw TTL and rollup MVs bound storage; monitor ingest cost after launch.
- **Schema drift.** The edge NDJSON shape and the Tinybird datasource schema
  must stay in sync. Keep the Rust struct and the `.datasource` definition
  reviewed together; a malformed row is dropped by Tinybird, so add an ingest
  error check to the dashboard.
- **Token handling.** The Tinybird ingest token is configured on the Fastly log
  endpoint (Authorization header), provisioned as a Fastly service resource, not
  committed to the repo.

## Out of scope

- Alerting rules (Grafana alerts can be added once panels exist).
- Real-time analytics API / `fastly-exporter` integration.
- Per-creative or per-deal analytics beyond seat-level CPM.
