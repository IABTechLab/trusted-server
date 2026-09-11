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
