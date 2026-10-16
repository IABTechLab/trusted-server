# Tinybird project

This directory is the tracked Tinybird resource project for Trusted Server
auction telemetry.

- `datasources/auction_events_raw.datasource` is the runtime ingest target.
- The auction overview, provider, and bid-stat rollups are materialized
  datasource outputs.
- `pipes/` contains summary, health, latency, freshness, quarantine, and seat
  yield endpoints or materialized views.
- `fixtures/auction_events_raw.ndjson` is test data only.
- `tests/` checks auction summary, provider health, and seat-yield results.
- `datasources/access_logs_raw.datasource` is reserved; Trusted Server does
  not currently emit access logs.

Authenticate the Tinybird CLI against the intended workspace, change to this
directory, run `tb test`, inspect `tb deploy --dry-run`, and deploy with
`tb deploy`. Do not commit workspace tokens or generated local credentials.

Create an APPEND token scoped to `auction_events_raw`, store its value in the
platform secret store, and put only its key name in
`tinybird.auction_token_secret`. The runtime sends directly to the regional
Events API host configured by `tinybird.api_host`.

For runtime behavior, limits, privacy properties, and the Fastly-only emission
boundary, see [Auction Telemetry](../docs/guide/telemetry.md). For exact
configuration fields, see [Configuration](../docs/guide/configuration.md).

## Deploy ordering

**Apply datasource changes to Tinybird before deploying the code that emits them.**
`AuctionEventBatch::to_ndjson` serializes with plain `serde_json` and no
`skip_serializing_if`, so every declared field is always on the wire, including as `null`.
Rows carrying a column the datasource does not declare go to quarantine rather than being
rejected loudly. Check quarantine in the Tinybird UI: `pipes/quarantine_counts.pipe`
is an unconfigured placeholder returning `NULL`, not a working monitor. Connect it to the
workspace quarantine source before relying on its counts. A code-first deploy can lose
rows silently until the schema catches up.

Adding a field means changing three things together: the struct in
`crates/trusted-server-core/src/auction/telemetry.rs`, the `SCHEMA` block in
`datasources/auction_events_raw.datasource`, and every row in
`fixtures/auction_events_raw.ndjson`.

## Reading `origin_cache_shareable`

Reports whether a request's origin response **would be** eligible to share between readers.

**It is a predicate, not an outcome.** It records whether a request _would_ be eligible,
not whether anything was cached. A row with `1` still forced an origin fetch unless the
operator had set `creative_opportunities.origin_readthrough_enabled` (default `false`), and
even then the platform stores nothing if the origin's own `Cache-Control` refuses. So it
answers "how much traffic would the gate admit", and only in combination with that setting
does it bound "how much did it admit".

Three caveats, each of which silently produces wrong numbers if a query ignores it.

### The denominator is matching-slot candidates, not all requests

Publisher summary rows cover requests with matching renderable slots while creative
opportunities are enabled. They include completed auctions **and skipped candidates**:
bots, prefetches, consent-denied readers, and requests with auctions disabled can emit a
`Skipped` summary when slots match. A page with no matching slot, a non-GET request, or
traffic with creative opportunities disabled produces no publisher summary.

The query below therefore measures shareability among matching-slot candidates, including
skipped summaries. It is neither an ad-serving-pageview rate nor a site-wide rate. The
readthrough gate also governs origin fetches outside this telemetry population; measure
those separately before estimating whole-site impact.

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
