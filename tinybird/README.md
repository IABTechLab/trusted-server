# Tinybird datasources

Schemas and fixtures for Trusted Server's auction telemetry.

## Deploy ordering

**Apply datasource changes to Tinybird before deploying the code that emits them.**
`AuctionEventBatch::to_ndjson` serializes with plain `serde_json` and no
`skip_serializing_if`, so every declared field is always on the wire, including as `null`.
Rows carrying a column the datasource does not declare go to quarantine rather than being
rejected loudly — see `pipes/quarantine_counts.pipe`. A code-first deploy therefore loses
rows silently until the schema catches up.

Adding a field means changing three things together: the struct in
`crates/trusted-server-core/src/auction/telemetry.rs`, the `SCHEMA` block in
`datasources/auction_events_raw.datasource`, and every row in
`fixtures/auction_events_raw.ndjson`.

## Reading `origin_cache_shareable`

Reports whether the origin readthrough gate admitted a request — whether Trusted Server
allowed the platform cache to serve this page rather than forcing an origin fetch. Two
caveats, both of which silently produce wrong numbers if a query ignores them.

### The denominator is ad-serving pageviews, not all requests

A summary row is emitted only when an auction runs. A request that never reaches the ad
stack — a bot, a prefetch, a consent-denied reader, a page with no matched slot, or any
traffic while a kill switch is off — produces **no row at all**.

So a rate computed from this column is a rate over ad-serving pageviews. It is not a
site-wide figure and cannot be compared against one.

### `NULL` is "not measured", not "false"

`AuctionObservationContext` is shared with the `/auction` API source, which makes no cache
decision and leaves the column `NULL`. A query that reads `NULL` as "not shareable" will be
wrong for that whole source class. Filter on the source first:

```sql
SELECT countIf(origin_cache_shareable = 1) / count() AS shareable_rate
FROM auction_events_raw
WHERE event_kind = 'summary'
  AND auction_source = 'initial_navigation'
  AND origin_cache_shareable IS NOT NULL
```

### There is no template-cache hit/miss column

Deliberately, and it cannot be added without restructuring when telemetry is emitted. The
store outcome is not knowable when the row is sent: the auction is collected during body
streaming, which takes the observation and emits the batch, and the template is only stored
afterwards. `hit` was reachable and `miss-stored` was not, which would have made hit rate
compute as roughly 100%.

For debugging one request, the `x-ts-template-cache` response header still reports all nine
states.
