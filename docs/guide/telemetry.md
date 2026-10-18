# Auction Telemetry

Trusted Server can emit bounded auction observations to an analytics module.
`[analytics] module` selects the module, and core hands each settled auction's
rows to the sink that module supplies. Without the section nothing is sent,
through core's no-op sink. The one module a stock build ships is Tinybird,
`crates/analytics/tinybird`, which each adapter lists among the modules it
links. Only the Fastly adapter hands that sink to the auction. The Axum,
Cloudflare and Spin adapters link the module and check its settings, and use
core's no-op sink, so they send nothing, and log a warning at startup when
`[analytics]` selects a module.

## Adapter support

| Adapter      | Auction telemetry            | Access telemetry |
| ------------ | ---------------------------- | ---------------- |
| `axum`       | not sent, warning at startup | refused          |
| `cloudflare` | not sent, warning at startup | refused          |
| `fastly`     | sent                         | refused          |
| `spin`       | not sent, warning at startup | refused          |

## Data path

The request entry point derives coarse `DeviceSignals`. Auction execution
combines those signals with request and navigation facts in an
`AuctionObservationContext`. The terminal auction outcome is converted into
an `AuctionEventBatch` containing summary, provider-call, and bid rows.
The Tinybird module serializes the batch as NDJSON and starts one Events API
request to the configured auction datasource.

Emission is best effort. Failure to construct or start telemetry is logged and
must not change the customer response. The sink drops the in-flight handle
after dispatch; it does not wait for Tinybird's response. Each batch is limited
to 512 rows and `analytics.tinybird.max_body_bytes` bytes. The backend the
module registers uses two-second first-byte and between-byte timeouts.

## Privacy boundary

Telemetry records operational auction facts, not the EC identifier. The event
model includes coarse device classification, auction source and outcome,
provider status and timing, slot dimensions, media type, seat, price,
currency, win state, and selected creative identifiers. Tests reject the
request's derived EC ID from serialized NDJSON.

Every row also carries the reader's complete User-Agent, country, region and
page path, so rows are built only when the reader's permissions include
`analytics.ad_reporting.measure_ad_performance` (TCF Purpose 7). A reader
without it gets no row, whatever the outcome, a skip for consent included.

`browser_family` is a coarse User-Agent classification. Recognized values
are `chrome`, `safari`, `firefox`, `edge`, and `opera`; unrecognized
clients produce no value. Edge, Opera, and iOS-specific tokens are matched
before generic Chrome or Safari tokens so embedded UA tokens do not
misclassify them.

## Configuration

Select the module with `[analytics] module = "tinybird"`. In
`[analytics.tinybird]` provide the regional API host, select the auction
datasource, and set `auction_token_secret` to the key holding that
datasource's APPEND token in the default app-config secret store. The default
datasource is `auction_events_raw`; the default body limit is 1 MiB and the
minimum accepted limit is 1 KiB.

`analytics.tinybird.access_enabled = true` is rejected because no access-log
emitter is wired. `access_dataset` and `access_sample_rate` are reserved.
`access_token_secret` is deprecated input and is normalized away; it cannot
enable access telemetry.

The exact settings and secret-handling dispositions are in
[Configuration](/guide/configuration#analytics-configuration). The tracked
Tinybird project is described in [`tinybird/README.md`](https://github.com/IABTechLab/trusted-server/blob/main/tinybird/README.md).
