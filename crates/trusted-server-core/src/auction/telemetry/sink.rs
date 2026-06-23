//! Sink abstraction for emitting telemetry rows.
//!
//! Core defines the trait and test implementations. The Fastly adapter provides
//! the real implementation that serializes rows to a named log endpoint.

use std::sync::Mutex;

use crate::auction::telemetry::types::AuctionEventRow;

/// Destination for telemetry rows.
///
/// Implementations must be cheap and non-blocking from the caller's view; the
/// Fastly implementation performs a buffered host write.
pub trait AuctionEventSink: Send + Sync {
    /// Emit a batch of rows for one auction observation.
    fn emit(&self, rows: &[AuctionEventRow]);
}

/// Sink that discards rows. Used where telemetry is disabled and in tests.
#[derive(Debug, Default)]
pub struct NoopSink;

impl AuctionEventSink for NoopSink {
    fn emit(&self, _rows: &[AuctionEventRow]) {}
}

/// Sink that accumulates rows in memory for assertions in tests.
#[derive(Debug, Default)]
pub struct InMemorySink {
    captured: Mutex<Vec<AuctionEventRow>>,
}

impl InMemorySink {
    /// Return a clone of all captured rows in emission order.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned (never happens in correct use).
    #[must_use]
    pub fn rows(&self) -> Vec<AuctionEventRow> {
        self.captured
            .lock()
            .expect("should lock captured rows")
            .clone()
    }
}

impl AuctionEventSink for InMemorySink {
    fn emit(&self, rows: &[AuctionEventRow]) {
        self.captured
            .lock()
            .expect("should lock captured rows")
            .extend_from_slice(rows);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auction::telemetry::types::{
        AuctionEventRow, AuctionObservationContext, AuctionSource, EventKind,
    };

    fn ctx() -> AuctionObservationContext {
        AuctionObservationContext {
            auction_id: uuid::Uuid::nil(),
            source: AuctionSource::AuctionApi,
            publisher_domain: "example.com".to_string(),
            page_path: "/p".to_string(),
            country: "US".to_string(),
            region: None,
            is_mobile: 2,
            is_known_browser: 2,
            gdpr_applies: false,
            consent_present: false,
        }
    }

    #[test]
    fn in_memory_sink_captures_emitted_rows() {
        let sink = InMemorySink::default();
        let rows = vec![AuctionEventRow::base(&ctx(), EventKind::Summary)];
        sink.emit(&rows);
        sink.emit(&rows);
        assert_eq!(
            sink.rows().len(),
            2,
            "should accumulate rows across emit calls"
        );
    }

    #[test]
    fn noop_sink_accepts_rows() {
        let sink = NoopSink;
        sink.emit(&[AuctionEventRow::base(&ctx(), EventKind::Summary)]);
    }
}
