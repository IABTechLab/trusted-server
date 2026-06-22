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

Tinybird runs **self-managed** (`tb infra`), not Tinybird Cloud, to control
cost (see Deployment below). The Events API and published pipe endpoints are
served from our own cluster host, so the URLs differ from `api.tinybird.co`.

```
edge (handle_auction)
  -> build event rows from OrchestrationResult (pure fn, cheap, inline)
  -> sink: buffered non-blocking write, host flushes async
  -> Fastly real-time log endpoint "ts_auction_events" (NDJSON, batched delivery)
  -> self-managed Tinybird Events API  POST https://<tb-host>/v0/events?name=auction_events_raw
  -> landing datasource (append-only, 30-day TTL)
  -> materialized views (per-minute rollups)
  -> published pipe endpoints
  -> Grafana (Tinybird datasource -> <tb-host>)

edge (every request)
  -> Fastly real-time log endpoint "ts_access_logs" (one access line / request)
  -> self-managed Tinybird Events API  POST https://<tb-host>/v0/events?name=access_logs_raw
  -> rollups -> endpoints -> Grafana (ops panels)
```

## Resolved decisions

1. **No EC id is emitted.** `ec_id` is omitted from the schema entirely for
   privacy. Page URL is reduced to `page_path` (no query string). No per-user
   identifier leaves the edge in this pipeline.
2. **Raw retention is 30 days** via TTL on `auction_events_raw` and
   `access_logs_raw`. Per-minute rollups in materialized views are retained 13
   months (adjustable), since they are small.
3. **Phase 1 ships ops and yield together** so the operations team and the
   revenue team both get visibility in the first release.
4. **Tinybird is self-managed (`tb infra`)**, not Tinybird Cloud, to start, for
   cost control. See Deployment.

## Deployment: self-managed Tinybird

Per the `tb infra` model
(<https://www.tinybird.co/blog/tb-infra>), `tb infra` generates Kubernetes
manifests for a containerized Tinybird that runs in our own AWS account. It
deploys the OLAP database (ClickHouse), the ingestion APIs (Events API), an API
gateway for published endpoints, and observability and backpressure components.
Management still goes through `cloud.tinybird.co` by selecting the self-managed
region, or by connecting the UI to the local image.

Implications for this design:

- **Hosts change.** Ingestion is `POST https://<tb-host>/v0/events?name=...` and
  pipe endpoints are served from `<tb-host>`, where `<tb-host>` is our cluster's
  gateway ingress, not `api.tinybird.co`. Auth is unchanged: Bearer tokens.
- **Fastly must reach `<tb-host>`.** The cluster gateway needs a
  publicly resolvable, TLS-terminated ingress so Fastly's HTTPS real-time log
  sink can POST to it. Lock it down to token auth and, if possible, restrict
  source ranges to Fastly.
- **Single-node to start.** The current `tb infra` offering is single-node with
  no HA, no S3-persistence optimization, and manual vertical scaling. That is
  acceptable here because this pipeline is a non-critical, fire-and-forget
  analytics sink: if Tinybird is down, Fastly real-time logging buffers and
  then drops, auctions are unaffected, and we lose analytics for the outage
  window only. It must never be on the auction request path.
- **Sizing.** Plan reference is 4+ CPU / 16GB+ RAM / 100GB+ SSD, roughly
  $150 to $600+ per month of infrastructure, traded against Tinybird Cloud's
  usage-based pricing. Confirm the node size against expected ingest volume
  (see Risks: log volume).
- **Migration path.** Datasources, pipes, and endpoints are defined as code
  (`.datasource` / `.pipe` files), so moving to multi-node self-managed or to
  Tinybird Cloud later is a redeploy against a different host, not a rewrite.

## Components

### A. Edge event emission (Rust, `trusted-server-core`)

Two pieces, split along the existing layering. The codebase keeps
`trusted-server-core` platform-agnostic (it logs through the `log` facade and
reaches Fastly only through a platform `services` abstraction); the Fastly
adapter owns `log_fastly` and the real endpoints. The metrics path follows the
same split:

- **Builder (core, pure).** `build_auction_events(result: &OrchestrationResult,
  ctx: &AuctionEventContext) -> Vec<AuctionEvent>` converts orchestration output
  into rows at the grain **one row per (auction x provider x seat-response)**. A
  provider that returns no bid emits one row with `status = nobid` and a null
  `price_cpm`; a provider error emits one row with `status = error`. Each row
  carries the auction-level fields denormalized. The builder must include
  `mediator_response` when a mediator is configured, since the winner can come
  from there, not only from `provider_responses`.
- **Sink (abstraction in core, implementation in adapter).** Core defines a
  small `AuctionEventSink` trait (or a method on the existing platform services
  object that already provides `services.geo()`). The Fastly adapter implements
  it with a dedicated endpoint via `fastly::log::Endpoint::from_name(
  "ts_auction_events")` and `writeln!`, emitting NDJSON (one JSON object per
  line) with no `timestamp LEVEL [module]` prefix, deliberately bypassing the
  fern formatter used for `tslog`. Tests use a no-op or in-memory sink, which
  also keeps the native (non-Fastly) build clean.

`is_win` is set by matching each provider/seat bid against `winning_bids`
(keyed by `slot_id`, value a full `Bid` clone) on `(slot_id, bidder, ad_id)`.
`ad_id` is `Option`, so when it is absent (e.g. APS/TAM) the match falls back to
`(slot_id, bidder)` and may be ambiguous if one seat returns multiple bids for a
slot; acceptable for analytics, noted as a known imprecision.

The sink write is a non-blocking, host-buffered append performed during request
handling; the Fastly host flushes to the log endpoint asynchronously, so it does
not add measurable response latency. There is no synchronous network call on the
request path.

Emission happens regardless of consent state (the rows contain no PII). Consent
state is recorded as booleans (`gdpr_applies` from `consent.gdpr_applies`,
`consent_present` from whether a consent context exists) for analysis, not used
to gate emission.

#### `auction_events_raw` row schema

Auction-level (denormalized onto every row):

| field                | type      | notes                                  |
| -------------------- | --------- | -------------------------------------- |
| `event_ts`           | DateTime  | auction completion time (UTC)          |
| `auction_id`         | String    | UUID, drill-down only                  |
| `publisher_domain`   | String    |                                        |
| `page_path`          | String    | path only, no query string             |
| `country`            | String    | from geo lookup                        |
| `region`             | String    | from geo lookup; nullable              |
| `is_mobile`          | UInt8     | 0=desktop, 1=mobile, 2=unknown          |
| `is_known_browser`   | UInt8     | 0=bot, 1=browser, 2=unknown (JA4/H2)    |
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

Device signals note: both `is_mobile` (0/1/2) and the bot-vs-browser bit come
from `derive_device_signals()`, already computed in the adapter before routing
at [main.rs:409](../../../crates/trusted-server-adapter-fastly/src/main.rs#L409)
(`DeviceSignals`, [ec/device.rs](../../../crates/trusted-server-core/src/ec/device.rs)).
Phase 1 threads that struct into `AuctionEventContext`; no UA re-parsing.
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

Either way, rows with null price still count toward bid/win/no-bid rates. Label
the CPM panel so a window with the legacy adapter is not read as total-market
CPM.

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

Tinybird Grafana datasource (or grafana-infinity against the published endpoint
URLs with a read token), pointed at `<tb-host>`, drives one dashboard with two
rows:

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

- **Log volume vs node size.** Grain is N rows per auction (N = providers x
  responding seats). At high QPS this multiplies ingest volume into Tinybird.
  For this testing phase the mitigation is vertical scaling of the single node;
  manual resize is acceptable and no autoscaling is expected. The 30-day raw TTL
  and rollup MVs bound storage. Sampling the ops access-log stream is deferred
  and revisited only if a larger or multi-node deployment is needed later.
- **Single-node availability.** Self-managed `tb infra` is single-node with no
  HA today. A Tinybird outage loses analytics for the window only (Fastly
  logging is fire-and-forget); it must never sit on the auction request path.
- **Fastly HTTPS delivery framing (highest integration unknown).** Fastly's
  HTTPS real-time log endpoint batches multiple log lines per delivery and can
  prepend framing depending on the endpoint's message-type/format settings.
  Tinybird's Events API wants a clean NDJSON body (one JSON object per line),
  the right `Content-Type`, and the Bearer token header. Before building
  dashboards, verify end to end that a batched Fastly delivery lands as valid
  rows: set the endpoint message type to a blank/raw format (no syslog prefix),
  confirm newline batching, and check Tinybird's quarantine table for rejects.
  This is the one piece that cannot be fully validated from code and must be
  tested against the real endpoints. **Fallback:** Tinybird Cloud's Events API
  is a documented, known-good target for Fastly HTTPS logging. If the
  self-managed HTTPS framing proves fiddly, point the same endpoints at hosted
  Tinybird to de-risk ingestion, trading the saved cost for usage-based pricing.
  The datasources/pipes are identical (as-code), so it is a host swap.
- **Schema drift.** The edge NDJSON shape and the Tinybird datasource schema
  must stay in sync. Keep the Rust struct and the `.datasource` definition
  reviewed together; a malformed row is dropped by Tinybird (quarantine), so add
  an ingest-error / quarantine check to the dashboard.
- **Token handling.** The Tinybird ingest token is configured on the Fastly log
  endpoint (Authorization header), provisioned as a Fastly service resource, not
  committed to the repo.
- **Ingress reachability.** Fastly's HTTPS log sink must reach `<tb-host>` over
  public TLS, so the self-managed cluster needs a resolvable, TLS-terminated
  gateway. Restrict it to token auth and, where feasible, Fastly source ranges.

## Prerequisites (provisioned outside the repo)

- Self-managed Tinybird cluster via `tb infra`, with a TLS-terminated gateway
  reachable from Fastly (`<tb-host>`).
- Two Tinybird ingest tokens (or one scoped to both datasources) and a read
  token for Grafana.
- Two Fastly real-time log endpoints on the service: `ts_auction_events` and
  `ts_access_logs`, each configured with the `<tb-host>` Events API URL, the
  Bearer token header, and a raw/blank message format.
- Grafana with the Tinybird datasource (or grafana-infinity) pointed at
  `<tb-host>`.

## Success criteria

- A live auction produces N rows in `auction_events_raw` (one per seat-response,
  including no-bid and error rows) with correct `is_win` flags and no PII.
- The yield dashboard shows fill rate, win rate by seat, no-bid rate, CPM
  quantiles (any seat with a decoded price; under the legacy APS adapter that
  excludes APS raw bids but still includes mediated winners), and per-seat
  latency for a chosen time range, filterable by publisher, provider, and
  bot-vs-browser.
- The ops dashboard shows QPS, status-class error rate, endpoint latency
  quantiles, and cache hit ratio from `access_logs_raw`.
- Tinybird quarantine stays empty under normal traffic; a malformed row is
  visible on the ingest-error panel.
- No measurable change in `/auction` response latency attributable to emission.

## Out of scope

- Alerting rules (Grafana alerts can be added once panels exist).
- Real-time analytics API / `fastly-exporter` integration.
- Per-creative or per-deal analytics beyond seat-level CPM.
