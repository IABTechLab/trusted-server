//! Value types for auction telemetry rows.
//!
//! These types are pure data: no I/O, no clock, no Fastly dependency. They are
//! shared by the builder and serialized as NDJSON by the Fastly sink.

use serde::Serialize;

/// Auction initiation path that produced an observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuctionSource {
    /// Initial publisher navigation via split-phase SSAT.
    InitialNavigation,
    /// Single-page-app navigation via `GET /__ts/page-bids`.
    SpaNavigation,
    /// Explicit `POST /auction` API call.
    AuctionApi,
}

/// Terminal status of a candidate auction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalStatus {
    /// Produced an `OrchestrationResult`, including a valid zero-bid result.
    Completed,
    /// Synchronous orchestration failed.
    ExecutionFailed,
    /// No provider request could be launched.
    DispatchFailed,
    /// Split-phase SSAT launched providers but could not collect them.
    Abandoned,
    /// Matched slots existed but policy prevented initiation.
    Skipped,
}

/// Outcome of a single provider call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderCallStatus {
    /// Provider returned at least one bid.
    Success,
    /// Provider responded with no bid.
    #[serde(rename = "nobid")]
    NoBid,
    /// Provider request could not be launched.
    LaunchError,
    /// Provider response could not be parsed.
    ParseError,
    /// Provider request failed in transport.
    TransportError,
    /// Provider did not respond before the auction deadline.
    Timeout,
    /// Provider was dispatched but never collected.
    Abandoned,
}

/// Role a provider played in the auction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderRole {
    /// A bidder.
    Bidder,
    /// The mediation layer.
    Mediator,
}

/// Discriminator for the row grain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    /// One per candidate auction.
    Summary,
    /// One per provider call.
    ProviderCall,
    /// One per returned bid (or unmatched mediator winner).
    Bid,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enums_serialize_to_expected_wire_strings() {
        assert_eq!(
            serde_json::to_string(&AuctionSource::InitialNavigation)
                .expect("should serialize source"),
            "\"initial_navigation\"",
            "should use snake_case wire form"
        );
        assert_eq!(
            serde_json::to_string(&TerminalStatus::ExecutionFailed)
                .expect("should serialize status"),
            "\"execution_failed\"",
            "should use snake_case wire form"
        );
        assert_eq!(
            serde_json::to_string(&ProviderCallStatus::NoBid)
                .expect("should serialize provider status"),
            "\"nobid\"",
            "should render NoBid as the single token nobid"
        );
        assert_eq!(
            serde_json::to_string(&EventKind::ProviderCall)
                .expect("should serialize kind"),
            "\"provider_call\"",
            "should use snake_case wire form"
        );
    }
}
