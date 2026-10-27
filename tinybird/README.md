# Tinybird project

This directory is the tracked Tinybird resource project for Trusted Server
auction and access telemetry.

- `datasources/auction_events_raw.datasource` is the runtime ingest target.
- The auction overview, provider, and bid-stat rollups are materialized
  datasource outputs.
- `pipes/` contains summary, health, latency, freshness, quarantine, and seat
  yield endpoints or materialized views.
- `fixtures/auction_events_raw.ndjson` is test data only.
- `tests/` checks auction summary, provider health, and seat-yield results.
- `datasources/access_logs_v2.datasource` is the sampled access-telemetry ingest
  target, with 31 columns and a 30-day TTL. `fixtures/access_logs_v2.ndjson` is test data only.
- `datasources/access_logs_raw.datasource` preserves the reserved legacy schema.
  It is not the target for the new emitter and is not replaced or backfilled.

Authenticate the Tinybird CLI against the intended workspace, change to this
directory, run `tb test`, inspect `tb deploy --dry-run`, and deploy with
`tb deploy`. Do not commit workspace tokens or generated local credentials.

Create an APPEND token scoped to `auction_events_raw`, store its value in the
platform secret store, and put only its key name in
`tinybird.auction_token_secret`. The runtime sends directly to the regional
Events API host configured by `tinybird.api_host`.

For access telemetry, use the distinct `ts_access_v2_ingest` APPEND token scoped
only to `access_logs_v2`. Store its value in the default app-config secret store
under the key named by `tinybird.access_token_secret`. Set
`tinybird.access_dataset = "access_logs_v2"` explicitly when updating a config
that still carries the legacy dataset name. Keep access disabled until this
schema and token are ready. With access enabled, validation rejects exactly the
normalized legacy name `access_logs_raw`; custom dataset names remain allowed,
and disabled legacy configurations remain accepted.

For runtime behavior, sampling, privacy properties, and the Fastly-only emission
boundary, see [Telemetry](../docs/guide/telemetry.md). For exact configuration
fields, see [Configuration](../docs/guide/configuration.md).

## Deploy ordering

**Apply datasource changes to Tinybird before deploying the code that emits them.**
`AuctionEventBatch::to_ndjson` serializes with plain `serde_json` and no
`skip_serializing_if`, so every declared field is always on the wire, including as `null`.
Rows carrying a column the datasource does not declare go to quarantine rather than being
rejected loudly. Check quarantine in the Tinybird UI: `pipes/quarantine_counts.pipe`
is an unconfigured placeholder returning `NULL`, not a working monitor. Connect it to the
workspace quarantine source before relying on its counts. A code-first deploy can lose
rows silently until the schema catches up.

Adding an auction field means changing three things together: the struct in
`crates/trusted-server-core/src/auction/telemetry.rs`, the `SCHEMA` block in
`datasources/auction_events_raw.datasource`, and every row in
`fixtures/auction_events_raw.ndjson`. Access fields likewise must agree between
`crates/trusted-server-core/src/access_telemetry.rs`,
`datasources/access_logs_v2.datasource`, and `fixtures/access_logs_v2.ndjson`,
including the JSONPaths and nullable types.

The access v2 contract uses `auction_collected_ms` for collection return, not
provider transport completion. `ts_version` identifies the TS build like the
`x-ts-version` header, not a Fastly activation number. `delivery_result` reports
local transport handoff as `complete`, `partial`, or `error`, not client receipt.
Access ingestion is fire-and-forget; HTTP success and quarantined rows are not
observed by the emitter. The core producer/schema/fixture test checks field and
JSONPath agreement, but cannot establish live ingestion success.

These are pre-rollout changes to the new v2 datasource. If a preview v2 schema
has already been deployed, inspect and coordinate its migration before deploying
this producer; do not assume the new name and column appear automatically.

### Access datasource cutover

The new emitter uses a versioned datasource rather than evolving
`access_logs_raw`. The legacy datasource has nine columns, including raw
`path` and `cache_state`, and no phase columns. `access_logs_v2` has no
`FORWARD_QUERY` and does not copy historical raw paths into bounded route templates.

1. Inspect the deployed workspace resources and retention requirements. Preserve
   any existing `access_logs_raw` and its token; do not drop or recreate it.
2. Run the Tinybird deployment check against that workspace and confirm the diff
   creates `access_logs_v2` and `ts_access_v2_ingest` without changing the legacy
   datasource. Deploy the new resources before the emitting binary.
3. Provision the new APPEND token in the platform secret store. Deploy the new
   binary with access telemetry still disabled, then push a config with
   `enabled = true`, `access_enabled = true`, `access_dataset = "access_logs_v2"`,
   the new token key, and a positive sample rate. Use separate
   token keys for the access and auction sinks.
4. Verify accepted rows, their stored `sample_rate`, ingestion freshness, and
   workspace quarantine. Confirm the UUID join against `auction_events_raw` for
   active auction observations. Disabled auctions have null access UUIDs.
5. Switch dashboards and queries to `access_logs_v2`. Leave legacy resources in
   place for retention and rollback; any later removal is a separate operator
   decision.

Rollback disables access emission before restoring an older binary. Re-push a
compatible config with no non-default `[observability]` table; for access-only
telemetry also set `tinybird.enabled = false`, since older binaries interpret it
as auction enablement. Keep both datasets and tokens intact during rollback.
Do not redeploy an older `tinybird/` project that omits `access_logs_v2`;
its deployment may propose removing the new datasource. Inspect the dry-run
rather than treating a binary rollback as a Tinybird project rollback.

These steps are a rollout requirement, not evidence that a live workspace
migration has already been tested.

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
