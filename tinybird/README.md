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

## Reading the cache-outcome columns

Two columns report how the caches treated a request: `origin_cache_shareable` and
`template_cache_bypass_reason`. Both have caveats that will silently produce wrong numbers
if a query ignores them.

### The denominator is ad-serving pageviews, not all requests

A summary row is emitted only when an auction runs. A request that bypasses the template
cache *because* the ad stack did not run — a bot, a prefetch, a consent-denied reader, a
page with no matched slot, or any traffic while a kill switch is off — produces **no row at
all**.

So a rate computed from these columns is a rate over ad-serving pageviews. It is not a
site-wide cache hit rate, and it cannot be compared against one.

### `NULL` is "not measured", not "false"

`AuctionObservationContext` is shared with the `/auction` API source, where neither column
is populated because that path makes no cache decision. Those rows carry `NULL`.

A query that reads `NULL` as "not shareable" or as a cache miss will be wrong for that whole
source class. Filter on the source before computing anything:

```sql
SELECT countIf(origin_cache_shareable = 1) / count() AS shareable_rate
FROM auction_events_raw
WHERE event_kind = 'summary'
  AND auction_source = 'initial_navigation'
  AND origin_cache_shareable IS NOT NULL
```

### There is no template-cache hit/miss column

Deliberately. The store outcome is not knowable when the telemetry row is emitted: the
auction is collected during body streaming, which takes the observation and sends the batch,
and the template is only stored afterwards. `hit` was reachable and `miss-stored` was not,
which would have made hit rate compute as roughly 100%.

For per-response debugging the `x-ts-template-cache` response header still reports all nine
states. For the aggregate question, `template_cache_bypass_reason` tells you *why* the cache
was not used, which is the actionable half.
