# Auction telemetry addendum: rollup correctness, breakdown dimensions, and dashboard specification

Date: 2026-07-30
Status: Draft for engineering review
Amends: `2026-06-22-auction-prebid-metrics-tinybird-grafana-design.md` (shipped in #818)

## Problem

The phase-1 pipeline shipped in #818 emits the right raw rows, but the layer
between `auction_events_raw` and Grafana loses information and the dashboard
does not answer the questions the design set out to answer.

A product review of the live dashboard (2026-07-30) found four classes of
problems:

1. **Cross-panel inconsistency.** The auction summary panel showed 3
   `auction_api` auctions total while provider health showed roughly 20,500
   provider calls per provider and seat yield showed 30,735 bids on the same
   source. Summary, provider, and bid rows for one auction are emitted in a
   single batch, so this ratio is structurally impossible in the raw data.
   Nothing on the dashboard surfaces the discrepancy; root cause is not yet
   confirmed (see Investigation below).
2. **Silent row dropping in the materialized views.** Three `IS NOT NULL`
   row filters in the MVs contradict the parent design's accounting rules and
   bias the aggregates:
   - `auction_provider_stats_mv` drops provider-call rows with null
     `provider_response_time_ms`. Launch errors and dispatch-abandoned calls
     legitimately have no latency, so provider error and abandonment counts
     undercount.
   - `auction_bid_stats_mv` drops bid rows with null `price_cpm`. The parent
     design states null-price bids still count as bids. A winning bid with a
     null raw price disappears entirely, so wins undercount.
   - `auction_overview_mv` drops summary rows with null `total_time_ms`. The
     builder currently always sets the field, but any future outcome that does
     not will vanish without trace.
3. **Missing breakdown dimensions.** `country`, `region`, and `is_mobile` are
   emitted on every raw row but are discarded by every rollup, so they are
   unqueryable from the dashboard and die with the 30-day raw TTL. Browser
   family is not emitted at all. A publisher customer has asked for breakdowns
   by device (mobile vs desktop), browser, country, and domain. Domain already
   exists in every rollup; the other three do not survive to the dashboard.
4. **The dashboard shows endpoint dumps, not answers.** No time series, no
   fill rate, no `terminal_reason` breakdown (85% of initial-navigation
   candidates were `skipped` with no visible reason), no sample counts, no
   freshness or quarantine panels (the quarantine pipe is a stub returning
   `unconfigured`), a `win_rate` label that reads as auction win rate but is
   bid win share, `total_time_ms` on initial navigation that reads as added
   latency but is time-to-collection, and CPM quantiles that ignore currency.
   The dashboard itself is hand-built and exists nowhere in the repo.

This addendum specifies the fixes, the new dimensional rollups, the small edge
change for browser signals, and a dashboard-as-code panel specification. The
directional, best-effort data contract from the parent design is unchanged.

## Investigation: the auction_api summary gap

Before any schema work, confirm where completed `auction_api` summary rows are
lost. Run against the workspace:

```sql
SELECT auction_source, event_kind, terminal_status, count() AS rows,
       uniqExact(auction_id) AS auctions
FROM auction_events_raw
WHERE event_ts >= now() - INTERVAL 1 DAY
GROUP BY auction_source, event_kind, terminal_status
ORDER BY auction_source, event_kind
```

Interpretation:

- **Raw has the summaries, rollup does not:** the overview MV was deployed or
  redeployed after ingestion started (Tinybird MVs process only rows inserted
  after the MV exists) or rows fail an MV filter. Fix by repopulating the
  rollup from raw.
- **Raw is missing the summaries:** emission or delivery. Check the
  `auction_events_raw` quarantine table for rejected rows, then the batch size
  guard (`tinybird.max_body_bytes` drops whole batches, but provider rows
  arriving without summaries rules out whole-batch loss, which points back to
  per-row quarantine).
- **Raw is fine and the rollup is fine:** Grafana panel filters differ, most
  likely a `publisher` parameter. Note #937 changed `publisher_domain`
  attribution for navigation paths after #818, so `auction_api` rows can carry
  a different domain than navigation rows for the same site. Neither current
  table panel displays `publisher_domain`.

The reconciliation endpoint in this addendum makes this class of failure
self-announcing regardless of cause.

## Decision summary

1. Fix the three MV null filters by moving null handling into conditional
   aggregate combinators. Rows are never dropped; nulls are excluded from
   quantiles only. Add explicit sample-count columns so panels can display n.
2. Keep the existing per-minute rollups for time series. Add a second family
   of **hourly dimensional rollups** carrying `country`, `device_class`,
   `browser_family`, and `os_family` for breakdown panels. Do not add these
   dimensions to the per-minute rollups; the cardinality product would slow
   every panel to serve a use case that does not need minute grain.
3. Emit two new coarse fields from the edge: `browser_family` and `os_family`.
   Both derive from signals `DeviceSignals` already computes or can compute
   from the UA without storing it. No raw UA, IP, or fingerprint value is
   emitted; the privacy posture of the parent design is unchanged.
4. Add `currency` to the bid rollups and endpoints. CPM quantiles are always
   computed and displayed per currency.
5. Replace the quarantine stub with the workspace quarantine source and add a
   raw-data reconciliation endpoint.
6. Ship the Grafana dashboard as provisioned JSON in the repo, with a panel
   specification (below) that every future change is reviewed against.
7. Rename or relabel misleading measures: `win_rate` becomes
   `bid_win_share`; `total_time_ms` for `initial_navigation` is labeled
   "time to collection" wherever displayed.

## A. Edge changes (Rust)

### New fields

Add to `AuctionObservationContext` and every emitted row:

| field            | type                   | values                                                                    |
| ---------------- | ---------------------- | ------------------------------------------------------------------------- |
| `browser_family` | LowCardinality(String) | `chrome` / `edge` / `safari` / `firefox` / `samsung` / `opera` / `other` / `unknown` |
| `os_family`      | LowCardinality(String) | `windows` / `mac` / `ios` / `android` / `linux` / `other` / `unknown`     |

- `os_family` is `DeviceSignals.platform_class` mapped to the bounded set,
  with `unknown` for `None`.
- `browser_family` is a new pure function in `ec/device.rs` beside
  `parse_platform_class`, an ordered allowlist match on the UA string. Order
  matters: `Edg` before `Chrome`, `SamsungBrowser` before `Chrome`, `OPR`
  before `Chrome`, `Chrome`/`CriOS` before `Safari`, `Firefox`/`FxiOS` before
  `Safari`. Anything matching none of the tokens is `other`; empty UA is
  `unknown`. The function returns an enum-backed `&'static str`, never a UA
  substring, so cardinality is bounded by construction.
- Both fields snapshot into the observation context at creation, same as
  `is_mobile`, so there is no re-parsing and no divergence between row kinds
  of one auction.

Privacy note: the parent design's guarantees hold. The new fields are coarse
classifications from an allowlist, not UA substrings, and are shared by very
large user populations. Existing tests asserting no raw UA serialization gain
cases for the new fields.

### Unchanged

Emission lifecycle, batching, sink, and the fire-and-forget transport are
untouched by this addendum.

## B. Tinybird: raw datasource migration

Add to `auction_events_raw` (additive, defaulted, so historical rows and
lagging edge versions remain valid):

```text
`browser_family` LowCardinality(String) DEFAULT 'unknown',
`os_family` LowCardinality(String) DEFAULT 'unknown'
```

Rows from edge versions that predate the field arrive without the key and take
the default. No sorting-key change.

## C. Tinybird: fixed per-minute materialized views

The three MVs are re-specified as follows. The pattern is identical in each:
the row-kind filter and dimension null guards remain in `WHERE`; measure null
guards move into combinators.

`auction_overview_mv` (replaces the `total_time_ms IS NOT NULL` filter):

```sql
SELECT
  toStartOfMinute(event_ts) AS minute,
  publisher_domain,
  auction_source,
  assumeNotNull(terminal_status) AS terminal_status,
  terminal_reason,
  countState() AS auctions,
  sumState(toUInt64(coalesce(slot_count, 0))) AS requested_slots,
  sumState(toUInt64(coalesce(winning_bid_count, 0))) AS winning_bids,
  countStateIf(total_time_ms IS NOT NULL) AS timed_auctions,
  quantilesStateIf(0.5, 0.95, 0.99)(coalesce(total_time_ms, 0), total_time_ms IS NOT NULL)
    AS total_time_quantiles
FROM auction_events_raw
WHERE event_kind = 'summary' AND terminal_status IS NOT NULL
GROUP BY minute, publisher_domain, auction_source, terminal_status, terminal_reason
```

`auction_provider_stats_mv` (replaces the `provider_response_time_ms IS NOT
NULL` filter; abandoned and launch-error calls now count):

```sql
  countState() AS requests,
  ...existing status sums unchanged...,
  countStateIf(provider_response_time_ms IS NOT NULL) AS latency_samples,
  quantilesStateIf(0.5, 0.95, 0.99)(coalesce(provider_response_time_ms, 0),
    provider_response_time_ms IS NOT NULL) AS latency_quantiles
WHERE event_kind = 'provider_call' AND provider IS NOT NULL
  AND provider_role IS NOT NULL AND status IS NOT NULL
```

`auction_bid_stats_mv` (replaces the `price_cpm IS NOT NULL` filter; adds
currency; null-price bids and wins now count):

```sql
SELECT
  toStartOfMinute(event_ts) AS minute,
  publisher_domain,
  auction_source,
  assumeNotNull(provider) AS provider,
  assumeNotNull(seat) AS seat,
  coalesce(currency, 'unknown') AS currency,
  countState() AS bids,
  countStateIf(price_cpm IS NOT NULL) AS priced_bids,
  sumState(toUInt64(coalesce(is_win, 0) = 1)) AS wins,
  quantilesStateIf(0.5, 0.95, 0.99)(coalesce(price_cpm, 0),
    coalesce(is_win, 0) = 1 AND price_cpm IS NOT NULL) AS winning_cpm_quantiles
FROM auction_events_raw
WHERE event_kind = 'bid' AND provider IS NOT NULL AND seat IS NOT NULL
GROUP BY minute, publisher_domain, auction_source, provider, seat, currency
```

Rollup target datasources gain the corresponding columns (`timed_auctions`,
`latency_samples`, `priced_bids`, `currency`). `currency` joins the bid
rollup sorting key.

**Migration note.** Changing an MV or its target requires a rebuild of the
rollup; Tinybird materializations do not retroactively process existing rows.
Plan a repopulate from `auction_events_raw` (30-day retention bounds the
backfill) in the same deployment, and treat rollup history older than 30 days
as carrying the pre-fix bias. Endpoints must read the new sample-count columns
with `countMerge`.

## D. Tinybird: hourly dimensional rollups

Three new materialized views and target datasources, mirroring the per-minute
family at hour grain with breakdown dimensions. `device_class` is derived once
in the MV: `is_mobile` 0 maps to `desktop`, 1 to `mobile`, 2 to `unknown`.

- `auction_overview_dims_rollup`: key `(event_date, hour, publisher_domain,
  auction_source, terminal_status, country, device_class, browser_family,
  os_family)`; measures `auctions`, `requested_slots`, `winning_bids`.
  `terminal_reason` is deliberately excluded here; reason analysis stays on
  the per-minute rollup to keep this table's cardinality down.
- `auction_provider_dims_rollup`: key `(event_date, hour, publisher_domain,
  auction_source, provider, country, device_class, browser_family)`; measures
  `requests`, `successes`, `nobids`, `errors`, `timeouts`, `abandonments`,
  `latency_samples`, `latency_quantiles`.
- `auction_bid_dims_rollup`: key `(event_date, hour, publisher_domain,
  auction_source, provider, seat, currency, country, device_class,
  browser_family)`; measures `bids`, `priced_bids`, `wins`,
  `winning_cpm_quantiles`.

Retention 13 months, same as the per-minute family. Real-world cardinality is
sparse (only observed combinations materialize), and hour grain keeps the
worst case bounded. If a deployment still grows too large, drop `os_family`
from the overview key first; it is the least requested dimension.

Three new published endpoints (`auction_dims`, `provider_dims`, `seat_dims`)
expose them with the standard time-range, `publisher`, `source`, and
per-dimension filters. Grafana does grouping; the endpoints only filter and
merge.

## E. Tinybird: health endpoints

- **Quarantine.** Replace the `quarantine_counts` stub with a query against
  the workspace quarantine companion (`auction_events_raw_quarantine`) or the
  Tinybird service data source for datasource operations, whichever the
  current Tinybird release documents, returning rows quarantined in the last
  24 hours plus the most recent quarantine error strings. The endpoint
  contract (datasource, quarantined_rows, checked_at, status) is unchanged, so
  the panel does not care which source backs it. Deployment smoke tests must
  assert `status != 'unconfigured'`.
- **Reconciliation.** New endpoint `event_reconciliation` over raw, last 24
  hours, per `auction_source`: `uniqExact(auction_id)` where
  `event_kind = 'summary'`, the same where `event_kind = 'provider_call'`, and
  their difference. Healthy is a difference of zero (allowing best-effort
  slack); the dashboard alerts visually when provider-call auctions exceed
  summary auctions by more than 2%. This turns the class of bug found in the
  2026-07-30 review into a red panel instead of a support escalation.

## F. Grafana: dashboard as code

Add `grafana/dashboards/auction-telemetry.json` (provisioning-ready) plus a
README documenting the Infinity datasource and read-token setup. Hand-edited
dashboards are prohibited going forward; the JSON in the repo is the artifact
of record. Panel specification, top to bottom:

1. **Health strip.** Ingestion freshness lag per source, quarantined rows
   (24h), reconciliation delta per source, raw rows per minute. Static text:
   "Directional, best-effort analytics. Not for billing or reconciliation."
2. **KPI row** (completed auctions unless noted): auctions per minute by
   source (time series); completion rate; fill rate (winning bids over
   requested slots, completed only); abandonment rate; skipped share of
   candidates. Every rate shows its denominator as n.
3. **Why auctions skip.** Time series and table of `terminal_reason` for
   `skipped` and `abandoned` summaries. This panel exists because 85% of
   initial-navigation candidates were skipping with no visible cause.
4. **Provider health.** Error rate, no-bid rate, timeout count per provider
   (time series, sample counts); latency p50/p95 per provider from
   `latency_samples`-labeled quantiles. Abandonments plotted separately, not
   inside error rate.
5. **Yield.** Winning CPM p50/p95 by seat, one panel per currency, labeled
   "priced bids only"; `bid_win_share` by seat with its definition in the
   panel description; wins by seat over time.
6. **Breakdowns** (hourly rollups): fill rate, provider error rate, and
   winning CPM cut by country, device_class, browser_family, and
   publisher_domain. Template variables for each dimension.

Labeling rules, enforced in review: every rate and quantile panel displays its
sample count; `total_time_ms` for `initial_navigation` is always titled "time
to collection (includes origin race)"; no panel title uses the bare phrase
"win rate".

## Testing

- Unit tests for `parse_browser_family` covering the ordered-token pitfalls
  (Edge, Samsung Internet, Opera, iOS Chrome/Firefox reporting as Safari
  lookalikes) and asserting the return set is closed.
- Fixture NDJSON gains rows with null `provider_response_time_ms` (launch
  error, abandoned), null `price_cpm` bids including a null-price win, and a
  non-USD currency. `tb test` cases assert these rows are counted, excluded
  from quantiles, and split by currency.
- A reconciliation fixture where provider-call auctions exceed summaries
  asserts the endpoint reports the delta.
- Privacy tests extend to the two new fields: values must be members of the
  closed sets, never UA substrings.
- Deployment smoke test asserts quarantine endpoint is configured and rollup
  repopulation ran (row counts in rebuilt rollups match a raw control query).

## Rollout order

1. Run the investigation query; root-cause and fix the auction_api summary
   gap. Do not build on the pipeline before trusting it.
2. Deploy raw datasource additive columns, then edge emission of the new
   fields (order matters; unknown keys would otherwise quarantine).
3. Deploy fixed per-minute MVs and rollup schemas with repopulate.
4. Deploy dimensional rollups and new endpoints; backfill from the 30-day raw
   window so breakdown panels are not empty on day one.
5. Deploy the provisioned dashboard JSON; retire the hand-built dashboard.

## Out of scope

Unchanged from the parent design: alerting rules, billing-grade delivery,
exactly-once semantics, per-creative analytics, currency conversion (CPMs are
reported per currency, not normalized), and ops access-log telemetry.
