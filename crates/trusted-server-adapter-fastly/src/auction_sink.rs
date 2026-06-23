//! Fastly implementation of the auction telemetry sink.
//!
//! Writes one NDJSON line per telemetry row to the `ts_auction_events`
//! real-time log endpoint, stamping a shared `event_ts` per batch. The write is
//! buffered by the host and flushed asynchronously, so it never blocks the
//! response.

use std::io::Write as _;

use chrono::{SecondsFormat, Utc};
use fastly::log::Endpoint;
use trusted_server_core::auction::telemetry::{
    to_json_line_with_event_ts, AuctionEventRow, AuctionEventSink,
};

/// Name of the Fastly real-time log endpoint provisioned for auction telemetry.
const AUCTION_EVENTS_ENDPOINT: &str = "ts_auction_events";

/// Sink that serializes telemetry rows to NDJSON and writes them to the Fastly
/// auction-events log endpoint.
pub struct FastlyAuctionEventSink;

impl AuctionEventSink for FastlyAuctionEventSink {
    fn emit(&self, rows: &[AuctionEventRow]) {
        if rows.is_empty() {
            return;
        }
        let event_ts = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
        let mut endpoint = Endpoint::from_name(AUCTION_EVENTS_ENDPOINT);
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
