//! Fastly implementation of the auction telemetry sink.
//!
//! Writes one NDJSON line per telemetry row to the configured Fastly
//! real-time log endpoint, stamping a shared `event_ts` per batch. The write is
//! buffered by the host and flushed asynchronously, so it never blocks the
//! response.

use std::io::Write as _;

use chrono::{SecondsFormat, Utc};
use fastly::log::Endpoint;
use trusted_server_core::auction::telemetry::{
    to_json_line_with_event_ts, AuctionEventRow, AuctionEventSink,
};
use trusted_server_core::auction_config_types::DEFAULT_AUCTION_TELEMETRY_LOG_ENDPOINT;

/// Sink that serializes telemetry rows to NDJSON and writes them to the Fastly
/// auction-events log endpoint.
pub struct FastlyAuctionEventSink {
    endpoint_name: String,
}

impl FastlyAuctionEventSink {
    /// Create a sink that writes to `endpoint_name`.
    #[must_use]
    pub fn new(endpoint_name: impl Into<String>) -> Self {
        let endpoint_name = endpoint_name.into();
        let endpoint_name = endpoint_name.trim();
        let endpoint_name = if endpoint_name.is_empty() {
            DEFAULT_AUCTION_TELEMETRY_LOG_ENDPOINT.to_string()
        } else {
            endpoint_name.to_string()
        };
        Self { endpoint_name }
    }
}

impl AuctionEventSink for FastlyAuctionEventSink {
    fn emit(&self, rows: &[AuctionEventRow]) {
        if rows.is_empty() {
            return;
        }
        let event_ts = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
        let mut endpoint = Endpoint::from_name(&self.endpoint_name);
        for row in rows {
            match to_json_line_with_event_ts(row, &event_ts) {
                Ok(line) => {
                    if let Err(error) = writeln!(endpoint, "{line}") {
                        log::warn!("auction telemetry log write failed: {error}");
                        break;
                    }
                }
                Err(error) => {
                    log::warn!("auction telemetry serialization failed: {error}");
                }
            }
        }
    }
}
