# Auction Telemetry

Trusted Server can emit bounded auction observations directly to Tinybird.
Emission is currently wired only by the Fastly adapter. Other adapters use the
core no-op sink even if they can parse the shared settings.

## Data path

The request entry point derives coarse `DeviceSignals`. Auction execution
combines those signals with request and navigation facts in an
`AuctionObservationContext`. The terminal auction outcome is converted into
an `AuctionEventBatch` containing summary, provider-call, and bid rows.
Fastly serializes the batch as NDJSON and queues it for the request. After the
response is delivered, the entry point sends one Tinybird Events API request
per queued batch to the configured auction datasource. It sends nothing when
delivery did not commit the application's response body.

Emission is best effort. Failure to construct, queue, or send telemetry is
logged and does not change the customer response, which the client already
has. Each batch is limited to 512 rows and `tinybird.max_body_bytes` bytes.
Each request has a 2 s total timeout and a 1 s connect timeout, and bypasses
the cache.

## Privacy boundary

Telemetry records operational auction facts, not the EC identifier. The event
model includes coarse device classification, auction source and outcome,
provider status and timing, slot dimensions, media type, seat, price,
currency, win state, and selected creative identifiers. Tests reject the
request's derived EC ID from serialized NDJSON.

`browser_family` is a coarse User-Agent classification. Recognized values
are `chrome`, `safari`, `firefox`, `edge`, and `opera`; unrecognized
clients produce no value. Edge, Opera, and iOS-specific tokens are matched
before generic Chrome or Safari tokens so embedded UA tokens do not
misclassify them.

## Configuration

Set `tinybird.enabled = true`, provide the regional API host, select the
auction datasource, and set `auction_token_secret` to the key holding that
datasource's APPEND token in the default app-config secret store. The default
datasource is `auction_events_raw`; the default body limit is 1 MiB and the
minimum accepted limit is 1 KiB.

`tinybird.access_enabled = true` is rejected because no access-log emitter is
wired. `access_dataset` and `access_sample_rate` are reserved.
`access_token_secret` is deprecated input and is normalized away; it cannot
enable access telemetry.

The exact settings and secret-handling dispositions are in
[Configuration](/guide/configuration#tinybird). The tracked Tinybird project is
described in [`tinybird/README.md`](https://github.com/IABTechLab/trusted-server/blob/main/tinybird/README.md).
