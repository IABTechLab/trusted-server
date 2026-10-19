# Analytics modules

Analytics module crates live here, one per vendor, each supplying an
`AuctionTelemetrySink` from `trusted-server-core` under the name
`[analytics] module` selects it by:

- `crates/analytics/tinybird` (`trusted-server-analytics-tinybird`) sends each
  auction's telemetry rows to the Tinybird Events API as one request. It is
  selected by `[analytics] module = "tinybird"` and reads
  `[analytics.tinybird]`.
- Vendor analytics modules (for example `crates/analytics/<vendor>`) slot in
  alongside it, one per vendor.

Core builds the rows, being the auction's summary, each provider call and
each bid, and asks the selected sink to send them once the auction has
settled. With no `[analytics]` section nothing is sent, through the no-op
sink core supplies itself.
